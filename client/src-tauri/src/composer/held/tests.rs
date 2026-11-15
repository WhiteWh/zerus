use super::*;
use crate::composer::send::tests::{eventually, opened, ready, send_hub, sent, state};
use crate::composer::send::SendReply;
use crate::composer::tests::{dir, until, KEY};
use serde_json::json;
use std::sync::Mutex;
use zerus_client_core::{Action, Outcome};

const HELD: &str = "11111111-2222-4333-8444-555555555555";
const OTHER: &str = "11111111-2222-4333-8444-666666666666";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn holding() -> Value {
    let mut details = ready();
    details["input_hold_supported"] = json!(true);
    details["held_inputs"] = json!([]);
    details["held_settled"] = json!([]);
    details
}

fn listing(ids: &[(&str, &str)]) -> Value {
    let mut details = holding();
    details["held_inputs"] = json!(ids
        .iter()
        .map(|(id, text)| json!({"request_id": id, "text": text, "queued_at": 1_793_000_000.0}))
        .collect::<Vec<_>>());
    details
}

/// The dispatch answers a send with hgs's "queued" receipt for its request.
fn queue_sends(fake: &crate::composer::send::tests::Fake) {
    *lock(&fake.outcome) = Box::new(|action, id| match action {
        Action::Send { text, .. } => Outcome::Done {
            receipt: json!({"status": "queued", "request_id": id, "name": "claude/sample-project/review",
                "run_id": "run-1", "conversation_id": "conv-1", "first_message": false, "text": text.trim(),
                "transport": "hgs_queue", "position": 1}),
            cleanup: None,
        },
        _ => Outcome::Failed {
            detail: "unexpected".into(),
        },
    });
}

/// The calls the fake runner saw, and what it answers each with.
type Calls = Arc<Mutex<Vec<Value>>>;
fn runner(answer: impl Fn(&Value) -> Exit + Send + Sync + 'static) -> (Arc<HgsRun>, Calls) {
    let calls: Calls = Default::default();
    let seen = Arc::clone(&calls);
    let run: Arc<HgsRun> = Arc::new(move |invocation: &Invocation| {
        let mut input: Value =
            serde_json::from_slice(invocation.stdin.as_deref().unwrap_or(&b"null"[..])).unwrap();
        input["args"] = json!(invocation.args);
        let exit = answer(&input);
        lock(&seen).push(input);
        exit
    });
    (run, calls)
}

fn ok(value: Value) -> Exit {
    Exit::Code {
        code: 0,
        stdout: value.to_string().into_bytes(),
        stderr: String::new(),
    }
}

fn said(events: &[ComposerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ComposerEvent::Said { text, .. } => Some(text.english()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_just_queued_message_shows_until_an_inspection_lists_it_or_two_did_not() {
    let mut holds = Holds::default();
    holds.queued(KEY, HELD, "first", "first");
    holds.queued(KEY, OTHER, "second", "second");
    let rows = |holds: &Holds, details: &Value| -> Vec<String> {
        holds
            .rows(KEY, Some(details))
            .into_iter()
            .map(|row| row.request_id)
            .collect()
    };
    // Shown at once, oldest first, before any inspection lists them.
    assert_eq!(rows(&holds, &holding()), [HELD, OTHER]);
    // The first read lists one: it goes from the just-queued set, and the
    // listed row comes from the inspection.
    let first = listing(&[(HELD, "first")]);
    assert!(holds.observed(KEY, &first).is_empty());
    assert_eq!(rows(&holds, &first), [HELD, OTHER]);
    // The second read lists neither: the unlisted one has had its two reads.
    holds.observed(KEY, &holding());
    assert!(rows(&holds, &holding()).is_empty());
    assert!(holds.just.is_empty());
}

#[test]
fn a_held_message_hgs_settled_comes_back_once_with_why() {
    let mut holds = Holds::default();
    holds.queued(KEY, HELD, "trimmed by hgs", "as written\n");
    holds.queued(KEY, OTHER, "other", "other");
    let mut details = holding();
    details["held_settled"] = json!([
        {"request_id": HELD, "text": "trimmed by hgs", "status": "failed", "error": "no longer running", "settled_at": 1.0},
        {"request_id": "not-ours", "text": "someone else's", "status": "failed", "error": "x", "settled_at": 1.0},
    ]);
    let lost = holds.observed(KEY, &details);
    assert_eq!(
        lost,
        [Lost {
            text: "as written\n".into(),
            error: "no longer running".into(),
            uncertain: false
        }]
    );
    // Each comes back once: the next read of the same list brings nothing.
    assert!(holds.observed(KEY, &details).is_empty());
    assert_eq!(
        notice(&lost[0]).english(),
        "Not delivered: no longer running. The text is back in the composer."
    );
    let uncertain = Lost {
        text: "t".into(),
        error: "delivery uncertain: check Terminal".into(),
        uncertain: true,
    };
    assert_eq!(
        notice(&uncertain).english(),
        "Delivery unconfirmed: check the conversation before sending again. delivery uncertain: check Terminal"
    );
    // An empty editor takes the text; a draft keeps it and gets it below.
    assert_eq!(restored("  ", "lost"), "lost");
    assert_eq!(restored("draft", "lost"), "draft\n\nlost");
    assert_eq!(restored("draft\n\nlost", "lost"), "draft\n\nlost");
}

#[test]
fn a_cancel_confirmed_leaves_the_bar_and_a_failed_one_enables_it_again() {
    let mut holds = Holds::default();
    holds.queued(KEY, HELD, "first", "first");
    assert!(holds.begin_cancel(KEY, HELD));
    assert!(!holds.begin_cancel(KEY, HELD), "one cancel at a time");
    assert!(holds.rows(KEY, None)[0].cancelling);
    holds.end_cancel(KEY, HELD, false);
    assert!(!holds.rows(KEY, None)[0].cancelling);
    assert!(holds.begin_cancel(KEY, HELD));
    holds.end_cancel(KEY, HELD, true);
    // An inspection that still lists it does not bring it back.
    let still = listing(&[(HELD, "first")]);
    assert!(holds.rows(KEY, Some(&still)).is_empty());
    holds.observed(KEY, &holding());
    assert!(holds.cancelled.is_empty(), "forgotten once hgs stops listing it");
}

#[test]
fn a_send_asks_hgs_to_hold_it_only_when_the_inspection_supports_it() {
    let root = dir("held-opt-in");
    let (c, fake) = send_hub(&root);
    let view = opened(&c, ready());
    c.edit(view.handle, 1, view.text_epoch, "plain".into(), 5, 5);
    assert_eq!(
        c.send_message(view.handle, 2, view.text_epoch, "plain".into(), 5, 5),
        SendReply::Sending
    );
    let view = until(&c, |v| !v.sending && v.text.is_empty());
    c.observe(&state(holding()));
    c.edit(view.handle, 3, view.text_epoch, "held".into(), 4, 4);
    assert_eq!(
        c.send_message(view.handle, 4, view.text_epoch, "held".into(), 4, 4),
        SendReply::Sending
    );
    until(&c, |v| !v.sending && v.text.is_empty());
    let flags: Vec<bool> = sent(&fake)
        .into_iter()
        .map(|(_, action, _)| match action {
            Action::Send {
                queue_when_unconfirmed,
                ..
            } => queue_when_unconfirmed,
            _ => panic!("not a send"),
        })
        .collect();
    assert_eq!(flags, [false, true]);
}

#[test]
fn a_queued_receipt_shows_a_held_row_instead_of_a_sent_card() {
    let root = dir("held-queued");
    let (c, fake) = send_hub(&root);
    queue_sends(&fake);
    let view = opened(&c, holding());
    c.edit(view.handle, 1, view.text_epoch, "wait for me".into(), 11, 11);
    assert_eq!(
        c.send_message(view.handle, 2, view.text_epoch, "wait for me".into(), 11, 11),
        SendReply::Sending
    );
    let view = until(&c, |v| !v.sending && v.held_inputs.len() == 1);
    let id = sent(&fake)[0].2.clone();
    assert_eq!(view.held_inputs[0].request_id, id);
    assert_eq!(view.held_inputs[0].text, "wait for me");
    assert!(view.local.is_empty(), "no sent card");
    assert!(view.text.is_empty(), "the draft left the composer");
    // Listed by the next inspection, it stays; once hgs sent it, it goes.
    c.observe(&state(listing(&[(id.as_str(), "wait for me")])));
    assert_eq!(c.current().unwrap().held_inputs.len(), 1);
    c.observe(&state(holding()));
    assert!(c.current().unwrap().held_inputs.is_empty());
}

#[test]
fn a_held_message_hgs_could_not_deliver_returns_to_the_composer() {
    let root = dir("held-settled");
    let (c, fake) = send_hub(&root);
    queue_sends(&fake);
    let view = opened(&c, holding());
    c.edit(view.handle, 1, view.text_epoch, "come back".into(), 9, 9);
    c.send_message(view.handle, 2, view.text_epoch, "come back".into(), 9, 9);
    until(&c, |v| !v.sending && v.held_inputs.len() == 1);
    let id = sent(&fake)[0].2.clone();
    c.take_events();
    let mut details = holding();
    details["held_settled"] = json!([{"request_id": id, "text": "come back", "status": "failed",
        "error": "session identity changed; refresh before sending", "settled_at": 1.0}]);
    c.observe(&state(details.clone()));
    let view = until(&c, |v| v.text == "come back");
    assert_eq!(view.position, 9);
    assert_eq!(
        said(&c.take_events()),
        ["Not delivered: session identity changed; refresh before sending. The text is back in the composer."]
    );
    // Once only.
    c.observe(&state(details));
    assert!(said(&c.take_events()).is_empty());
    assert_eq!(c.current().unwrap().text, "come back");
}

#[test]
fn cancel_puts_the_text_back_into_an_empty_editor_once_hgs_confirms() {
    let root = dir("held-cancel");
    let (c, _) = send_hub(&root);
    let (run, calls) = runner(|input| ok(json!({"request_id": input["request_id"], "status": "cancelled"})));
    c.set_hgs(run);
    let view = opened(&c, listing(&[(HELD, "take it back")]));
    assert_eq!(view.held_inputs.len(), 1);
    assert!(!c.held_cancel(view.handle + 1, HELD), "another composer's handle");
    assert!(!c.held_cancel(view.handle, OTHER), "not a shown row");
    assert!(c.held_cancel(view.handle, HELD));
    let view = until(&c, |v| v.text == "take it back");
    assert!(view.held_inputs.is_empty(), "a confirmed cancel leaves the bar");
    // The idle agent's held row also reads the Terminal draft; the cancel is its own call.
    let input = lock(&calls)
        .iter()
        .find(|call| call["args"][1] == "send-cancel")
        .cloned()
        .expect("a cancel");
    assert_eq!(input["request_id"], HELD);
    assert_eq!(
        input["args"],
        json!(["@studio", "send-cancel", "claude/sample-project/review", "--json"])
    );
}

#[test]
fn a_failed_cancel_keeps_the_row_and_the_draft_and_says_why() {
    let root = dir("held-cancel-failed");
    let (c, _) = send_hub(&root);
    let (run, _) = runner(|_| Exit::Code {
        code: 1,
        stdout: Vec::new(),
        stderr: "the message was already delivered; it cannot be cancelled".into(),
    });
    c.set_hgs(run);
    let view = opened(&c, listing(&[(HELD, "already gone")]));
    c.edit(view.handle, 1, view.text_epoch, "my draft".into(), 8, 8);
    c.take_events();
    assert!(c.held_cancel(view.handle, HELD));
    eventually(|| !said(&c.lock().events.iter().cloned().collect::<Vec<_>>()).is_empty());
    assert_eq!(
        said(&c.take_events()),
        ["The message could not be cancelled. the message was already delivered; it cannot be cancelled"]
    );
    let view = c.current().unwrap();
    assert_eq!(view.text, "my draft");
    assert!(view.held_inputs.len() == 1 && !view.held_inputs[0].cancelling);
}

#[test]
fn a_send_refused_for_a_terminal_draft_reads_it_and_send_it_presses_enter() {
    let root = dir("held-draft");
    let (c, fake) = send_hub(&root);
    *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
        detail: "terminal has a draft, dialog, or unrecognized prompt; open Terminal and return to an empty agent prompt".into(),
    });
    let submitted = Arc::new(Mutex::new(false));
    let pressed = Arc::clone(&submitted);
    let (run, calls) = runner(move |input| match input["action"].as_str() {
        Some("draft") if !*lock(&pressed) => ok(json!({"request_id": input["request_id"], "status": "draft",
            "draft": "half a thought", "draft_hash": HASH, "draft_supported": true})),
        Some("draft") => ok(json!({"request_id": input["request_id"], "status": "draft", "draft": null, "draft_hash": HASH})),
        Some("submit_draft") => {
            *lock(&pressed) = true;
            ok(json!({"request_id": input["request_id"], "status": "submitted"}))
        }
        _ => Exit::StartFailed("unexpected".into()),
    });
    c.set_hgs(run);
    let view = opened(&c, ready());
    assert!(view.terminal_draft.is_none(), "hidden by default");
    c.edit(view.handle, 1, view.text_epoch, "my message".into(), 10, 10);
    c.send_message(view.handle, 2, view.text_epoch, "my message".into(), 10, 10);
    let view = until(&c, |v| v.terminal_draft.is_some());
    let bar = view.terminal_draft.clone().unwrap();
    assert_eq!(bar.draft, "half a thought");
    assert!(bar.can_send && !bar.submitting && bar.error.is_none());
    assert_eq!(view.text, "my message", "the owner's message stays");
    let read = lock(&calls)[0].clone();
    assert_eq!(read["action"], "draft");
    assert_eq!(read["expected_run_id"], "run-1");
    assert_eq!(read["expected_conversation_id"], "conv-1");
    assert!(c.draft_submit(view.handle));
    assert!(!c.draft_submit(view.handle), "one Enter at a time");
    until(&c, |v| v.terminal_draft.is_none());
    let submit = lock(&calls)
        .iter()
        .find(|call| call["action"] == "submit_draft")
        .cloned()
        .unwrap();
    assert_eq!(submit["draft_hash"], HASH);
    assert_ne!(submit["request_id"], read["request_id"], "a fresh request id");
}

#[test]
fn a_refused_enter_shows_why_and_reads_the_draft_again() {
    let root = dir("held-draft-refused");
    let (c, fake) = send_hub(&root);
    *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
        detail: "terminal has a draft, dialog, or unrecognized prompt".into(),
    });
    let (run, calls) = runner(|input| match input["action"].as_str() {
        Some("draft") => ok(json!({"request_id": input["request_id"], "status": "draft",
            "draft": "changed text", "draft_hash": HASH})),
        _ => Exit::Code {
            code: 1,
            stdout: Vec::new(),
            stderr: "The Terminal draft changed; nothing was sent".into(),
        },
    });
    c.set_hgs(run);
    let view = opened(&c, ready());
    c.edit(view.handle, 1, view.text_epoch, "x".into(), 1, 1);
    c.send_message(view.handle, 2, view.text_epoch, "x".into(), 1, 1);
    let view = until(&c, |v| v.terminal_draft.as_ref().is_some_and(|bar| bar.can_send));
    c.take_events();
    assert!(c.draft_submit(view.handle));
    eventually(|| lock(&calls).iter().filter(|call| call["action"] == "draft").count() >= 2);
    let view = until(&c, |v| v.terminal_draft.as_ref().is_some_and(|bar| bar.can_send));
    assert_eq!(view.terminal_draft.unwrap().draft, "changed text");
    assert_eq!(
        said(&c.take_events()),
        ["The Terminal draft changed; nothing was sent"]
    );
}

#[test]
fn an_uncertain_enter_is_never_pressed_again_by_itself() {
    let error = submit_error(DraftError {
        detail: "tmux lost".into(),
        uncertain: true,
    });
    assert_eq!(
        error.english(),
        "Enter may have reached Terminal; check it before sending again. tmux lost"
    );
    assert_eq!(error.source, held_input::MAYBE_SENT);
    assert_eq!(
        submit_error(DraftError {
            detail: held_input::NOT_SENT.into(),
            uncertain: false
        }),
        Text::new(held_input::NOT_SENT)
    );
}

#[test]
fn messages_held_for_an_idle_agent_read_the_draft_at_most_every_four_seconds() {
    let root = dir("held-draft-poll");
    let (c, fake) = send_hub(&root);
    let (run, calls) = runner(|input| ok(json!({"request_id": input["request_id"], "status": "draft", "draft": null, "draft_hash": HASH})));
    c.set_hgs(run);
    let details = listing(&[(HELD, "waiting")]);
    opened(&c, details.clone());
    eventually(|| lock(&calls).len() == 1);
    c.observe(&state(details.clone()));
    *lock(&fake.now) += 1.0;
    c.observe(&state(details.clone()));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(lock(&calls).len(), 1, "not again within 4 s");
    *lock(&fake.now) += 4.0;
    c.observe(&state(details.clone()));
    eventually(|| lock(&calls).len() == 2);
    // A busy agent is working on it: no read.
    let mut busy = details;
    busy["activity"] = json!("busy");
    *lock(&fake.now) += 10.0;
    c.observe(&state(busy));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(lock(&calls).len(), 2);
}

#[test]
fn a_shown_draft_the_person_cleared_in_terminal_leaves_the_bar() {
    // Owner, 2026-11-15: the bar stayed although Terminal was empty.
    let root = dir("held-draft-cleared");
    let (c, fake) = send_hub(&root);
    *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
        detail: "terminal has a draft, dialog, or unrecognized prompt; open Terminal and return to an empty agent prompt".into(),
    });
    let cleared = Arc::new(Mutex::new(false));
    let gone = Arc::clone(&cleared);
    let (run, _calls) = runner(move |input| match input["action"].as_str() {
        Some("draft") if !*lock(&gone) => ok(json!({"request_id": input["request_id"], "status": "draft",
            "draft": "не починил", "draft_hash": HASH, "draft_supported": true})),
        Some("draft") => ok(json!({"request_id": input["request_id"], "status": "draft", "draft": null, "draft_hash": HASH})),
        _ => Exit::StartFailed("unexpected".into()),
    });
    c.set_hgs(run);
    let view = opened(&c, ready());
    c.edit(view.handle, 1, view.text_epoch, "my message".into(), 10, 10);
    c.send_message(view.handle, 2, view.text_epoch, "my message".into(), 10, 10);
    until(&c, |v| v.terminal_draft.is_some());
    // The person sends the draft in Terminal; no message is held.
    *lock(&cleared) = true;
    *lock(&fake.now) += 5.0;
    c.observe(&state(ready()));
    until(&c, |v| v.terminal_draft.is_none());
}

#[test]
fn a_send_that_does_nothing_while_the_last_one_is_unconfirmed_says_why() {
    let root = dir("held-busy-notice");
    let (c, _) = send_hub(&root);
    let view = opened(&c, ready());
    c.edit(view.handle, 1, view.text_epoch, "again".into(), 5, 5);
    c.lock().composers.get_mut(KEY).unwrap().uncertain = true;
    c.take_events();
    let view = c.current().unwrap();
    assert_eq!(
        c.send_message(view.handle, 2, view.text_epoch, "again".into(), 5, 5),
        SendReply::Busy
    );
    assert_eq!(said(&c.take_events()), [UNSETTLED]);
    // A stale epoch is no such case: nothing to say.
    c.lock().composers.get_mut(KEY).unwrap().uncertain = false;
    let view = c.current().unwrap();
    assert_eq!(
        c.send_message(view.handle, 3, view.text_epoch + 7, "again".into(), 5, 5),
        SendReply::Busy
    );
    assert!(said(&c.take_events()).is_empty());
}
