//! The native queue against the Qt client: the panel (`ActivityView`), Send
//! now (`sendQueuedNow`, `queueSendFinished`), the queue matcher with the
//! cases of `tray/tests/test_sessionswindow.cpp` (the push tests), the push
//! (`armPush` through `endPush`) and OpenCode's inbox removal.
use serde_json::{json, Value};
use zerus_client_core::actions::Outcome;
use zerus_client_core::input_queue::{
    can_send_now, inbox_removable, message_start, queue_holds, queue_view, send_now_notice,
    InboxFacts, MessageStart, Notice, PushFacts, PushStep, Pusher, NOTHING_QUEUED, PUSH_SECONDS,
};

fn queue(id: &str, text: &str, can_send_now: bool) -> Value {
    json!({"run_id": "run-1", "conversation_id": "conv-1",
           "input_queue": {"id": id, "text": text, "can_send_now": can_send_now, "hint": "ctrl+x ctrl+s to send now"}})
}

#[test]
fn the_panel_follows_tracking_online_and_sending() {
    let details = queue("q-1", "\u{276F} later", true);
    let view = queue_view(&details, true, true, false, false).unwrap();
    assert_eq!(
        (view.id.as_str(), view.text.as_str(), view.hint.as_str()),
        ("q-1", "\u{276F} later", "ctrl+x ctrl+s to send now")
    );
    assert_eq!(
        (view.run_id.as_str(), view.conversation_id.as_str()),
        ("run-1", "conv-1")
    );
    assert!(view.can_send_now && view.enabled);
    assert_eq!(view.label, "Send now");
    let sending = queue_view(&details, true, true, false, true).unwrap();
    assert!(sending.can_send_now && !sending.enabled);
    assert_eq!(sending.label, "Sending\u{2026}");
    // Offline or after a failed inspection the panel shows, but cannot send.
    for (online, inspect_error) in [(false, false), (true, true)] {
        let view = queue_view(&details, true, online, inspect_error, false).unwrap();
        assert!(!view.can_send_now && !view.enabled);
    }
    assert_eq!(queue_view(&details, false, true, false, false), None);
    assert_eq!(
        queue_view(&queue("", "x", true), true, true, false, false),
        None
    );
    assert_eq!(queue_view(&json!({}), true, true, false, false), None);
    let blocked = queue_view(&queue("q-1", "x", false), true, true, false, false).unwrap();
    assert!(!blocked.enabled);
}

#[test]
fn send_now_needs_the_same_queue() {
    let details = queue("q-1", "x", true);
    assert!(can_send_now(&details, true, "q-1"));
    // An empty id means the current queue (the chord).
    assert!(can_send_now(&details, true, ""));
    assert!(!can_send_now(&details, true, "q-0"));
    assert!(!can_send_now(&details, false, "q-1"));
    assert!(!can_send_now(&queue("q-1", "x", false), true, "q-1"));
    assert!(!can_send_now(&json!({}), true, ""));
    assert_eq!(NOTHING_QUEUED, "Nothing queued can be sent now");
}

#[test]
fn send_now_notices() {
    let done = Outcome::Done {
        receipt: json!({}),
        cleanup: None,
    };
    assert_eq!(
        send_now_notice(&done),
        Notice {
            text: "Send now requested. Waiting for the agent to accept the queue.".into(),
            error: false,
            transient: false
        }
    );
    let stale = "The native queue is no longer visible; nothing was sent. Activity is being refreshed; check Terminal or the current queue before retrying.";
    for detail in [
        "hgs: The native queue is no longer visible; nothing was sent",
        "  The native queue is no longer visible; nothing was sent\n",
    ] {
        let notice = send_now_notice(&Outcome::Failed {
            detail: detail.into(),
        });
        assert_eq!(
            (notice.text.as_str(), notice.error, notice.transient),
            (stale, true, true)
        );
    }
    let other = send_now_notice(&Outcome::Uncertain {
        detail: "Queue action timed out. Check Terminal before retrying.".into(),
    });
    assert_eq!(
        (other.text.as_str(), other.error, other.transient),
        (
            "Queue action timed out. Check Terminal before retrying.",
            true,
            false
        )
    );
}

#[test]
fn message_starts_are_qts() {
    assert_eq!(
        message_start("Run the release checks before tagging v2"),
        MessageStart {
            references: vec![],
            text: "Run the release checks befor".into(),
            whole: false
        }
    );
    assert_eq!(
        message_start("  test \n suite  "),
        MessageStart {
            references: vec![],
            text: "test suite".into(),
            whole: true
        }
    );
    assert_eq!(
        message_start("[File #1] [Image #12] Check this"),
        MessageStart {
            references: vec!["[File #1]".into(), "[Image #12]".into()],
            text: "Check this".into(),
            whole: true
        }
    );
    // Attachments only: nothing tells the message apart.
    assert_eq!(
        message_start("[File #1] [Image #2]"),
        MessageStart::default()
    );
    assert_eq!(message_start("   "), MessageStart::default());
    // Not references: no digits, another word.
    assert_eq!(message_start("[File #] x").text, "[File #] x");
    assert_eq!(message_start("[Doc #1] x").text, "[Doc #1] x");
}

#[test]
fn queue_holds_matches_qts_cases() {
    // pushSendsNowWhenTheMessageIsQueued: the terminal wraps the text.
    let message = "Run the release checks before tagging v2";
    assert!(queue_holds(
        "\u{276F} Run the release checks\n  before tagging v2",
        message
    ));
    // pushIgnoresQueuesWithoutTheMessage, with spaces collapsed.
    let table = "Summarize the failing tests in one table";
    assert!(!queue_holds("\u{276F} Rename the release branch", table));
    assert!(queue_holds(
        "\u{276F} Rename the release branch\n\u{276F} Summarize the   failing tests in one table",
        table
    ));
    // pushIgnoresTheMessageInsideAnotherWord.
    let short = "test suite on CI";
    assert!(!queue_holds("\u{276F} Rerun the latest suite on CI", short));
    assert!(!queue_holds(
        "\u{276F} test suite on CI-only runners",
        short
    ));
    assert!(queue_holds(
        "\u{276F} Rerun the latest suite on CI\n\u{276F} test suite on CI",
        short
    ));
    // pushIgnoresTheCodexQueueHeader.
    let header = "\u{2022} Messages to be submitted after next tool call (press esc to interrupt and send immediately)";
    assert!(!queue_holds(
        &format!("{header}\n  \u{21B3} Check the build first"),
        "interrupt"
    ));
    assert!(!queue_holds(
        "\u{2022} Messages to be submitted after next tool call (press esc to\n  interrupt and send immediately)\n  \u{21B3} Check the build first",
        "interrupt"
    ));
    assert!(!queue_holds(
        "\u{2022} Queued follow-up inputs\n  \u{21B3} Check the build first\n    interrupt",
        "interrupt"
    ));
    assert!(queue_holds(
        &format!("{header}\n  \u{21B3} Check the build first\n  \u{21B3} interrupt"),
        "interrupt"
    ));
    // A bullet row ends an item; an unindented row too.
    assert!(!queue_holds(
        "\u{276F} Check\n  \u{2022} the build",
        "Check the build"
    ));
    assert!(!queue_holds("\u{276F} Check\nthe build", "Check the build"));
    // Codex's prompt marker.
    assert!(queue_holds("\u{203A} hello", "hello"));
    // Attachments only never match.
    assert!(!queue_holds("\u{276F} [File #1]", "[File #1]"));
}

#[test]
fn a_message_that_starts_with_an_attachment_matches_its_pasted_location() {
    for (reference, instruction, path) in [
        (
            "[File #1]",
            "Read the attached file at",
            "\"/fixture/hgs/attachments/request-one/notes.txt\"",
        ),
        (
            "[Image #1]",
            "Inspect the attached image at",
            r#""C:\\fixture\\hgs\\attachments\\shot.png""#,
        ),
    ] {
        let message = format!("{reference} Check this");
        let pasted = format!("{reference} ({instruction} {path}) Check this");
        // Not a header row, not the middle of another item, not another
        // message's attachment.
        for queue in [
            format!("\u{2022} {pasted}\n  \u{21B3} Rename the release branch"),
            format!("\u{276F} Compare the release notes {pasted}"),
            format!("\u{276F} Compare the release notes\n  {pasted}"),
            format!("\u{276F} {}", pasted.replace("#1]", "#2]")),
        ] {
            assert!(!queue_holds(&queue, &message), "{queue}");
        }
        // The terminal wraps the location onto the next row.
        let wrapped = format!(
            "\u{276F} Rename the release branch\n\u{276F} {reference} ({instruction}\n  {path}) Check this"
        );
        assert!(queue_holds(&wrapped, &message), "{wrapped}");
        assert!(queue_holds(&format!("\u{276F} {pasted}"), &message));
    }
}

const KEY: &str = "studio\nclaude/sample-project/review";

fn facts() -> PushFacts {
    PushFacts::default()
}

fn sending() -> PushFacts {
    PushFacts {
        sending: true,
        ..PushFacts::default()
    }
}

/// A push armed at 0 and confirmed for run-1 and conv-1.
fn confirmed(text: &str) -> Pusher {
    let mut pusher = Pusher::default();
    assert_eq!(pusher.arm(KEY, text, 0.0), None);
    pusher.dispatched(KEY, text, "req-1", "run-1", "conv-1");
    assert_eq!(pusher.send_finished("req-1", true, false), None);
    pusher
}

#[test]
fn a_push_sends_only_the_snapshot_that_holds_the_message() {
    let text = "Run the release checks before tagging v2";
    let mut pusher = confirmed(text);
    assert!(pusher.active().unwrap().confirmed);
    // Another session's inspection is not this push's.
    assert_eq!(
        pusher.inspected(
            "other",
            &queue("q-x", "\u{276F} Run the release checks", true),
            1.0,
            facts(),
            false
        ),
        PushStep::Wait
    );
    // A queue without the message, or one that cannot be sent now, waits.
    let other = queue("q-other", "\u{276F} Rename the release branch", true);
    assert_eq!(
        pusher.inspected(KEY, &other, 1.0, facts(), false),
        PushStep::Wait
    );
    let blocked = queue(
        "q-blocked",
        "\u{276F} Run the release checks before tagging v2",
        false,
    );
    assert_eq!(
        pusher.inspected(KEY, &blocked, 1.0, facts(), false),
        PushStep::Wait
    );
    // Another Send now is running: a later snapshot decides.
    let holding = queue(
        "q-push",
        "\u{276F} Run the release checks\n  before tagging v2",
        true,
    );
    assert_eq!(
        pusher.inspected(KEY, &holding, 2.0, facts(), true),
        PushStep::Wait
    );
    assert_eq!(
        pusher.inspected(KEY, &holding, 2.0, facts(), false),
        PushStep::SendNow("q-push".into())
    );
    // The push is spent: it never sends again.
    assert_eq!(pusher.active(), None);
    assert_eq!(
        pusher.inspected(KEY, &holding, 3.0, facts(), false),
        PushStep::Wait
    );
}

#[test]
fn an_unconfirmed_push_never_sends() {
    let text = "Run the release checks";
    let mut pusher = Pusher::default();
    pusher.arm(KEY, text, 0.0);
    pusher.dispatched(KEY, text, "req-1", "run-1", "conv-1");
    let holding = queue("q-push", "\u{276F} Run the release checks", true);
    assert_eq!(
        pusher.inspected(KEY, &holding, 1.0, facts(), false),
        PushStep::Wait
    );
    assert!(pusher.active().is_some());
}

#[test]
fn a_push_ends_when_the_session_changes() {
    let text = "Run the release checks";
    let changed = |details: Value| {
        let mut pusher = confirmed(text);
        pusher.inspected(KEY, &details, 1.0, facts(), false)
    };
    let error = PushStep::End(Some(Notice {
        text: "The message was not pushed: the session changed".into(),
        error: true,
        transient: false,
    }));
    assert_eq!(
        changed(json!({"run_id": "run-2", "conversation_id": "conv-1"})),
        error
    );
    assert_eq!(
        changed(json!({"run_id": "run-1", "conversation_id": "conv-2"})),
        error
    );
    // A run's first message adopts its conversation.
    let mut pusher = Pusher::default();
    pusher.arm(KEY, text, 0.0);
    pusher.dispatched(KEY, text, "req-1", "run-1", "");
    pusher.send_finished("req-1", true, false);
    assert_eq!(
        pusher.inspected(
            KEY,
            &json!({"run_id": "run-1", "conversation_id": "conv-9"}),
            1.0,
            facts(),
            false
        ),
        PushStep::Wait
    );
    assert_eq!(pusher.active().unwrap().conversation, "conv-9");
    // Before its request the run may still change (starting, compacting).
    let mut early = Pusher::default();
    early.arm(KEY, text, 0.0);
    assert_eq!(
        early.inspected(KEY, &json!({"run_id": "run-7"}), 1.0, sending(), false),
        PushStep::Wait
    );
    assert!(early.active().is_some());
}

#[test]
fn a_push_ends_on_failure_uncertainty_or_no_text() {
    let ended = |ok: bool, uncertain: bool, text: &str| {
        let mut pusher = Pusher::default();
        pusher.arm(KEY, text, 0.0);
        pusher.dispatched(KEY, text, "req-1", "run-1", "conv-1");
        // Another request's outcome is not this push's.
        assert_eq!(pusher.send_finished("req-2", false, true), None);
        let notice = pusher.send_finished("req-1", ok, uncertain);
        assert_eq!(pusher.active(), None);
        notice.unwrap()
    };
    assert_eq!(
        ended(false, true, "hello"),
        Notice {
            text: "The message was not pushed: delivery is not confirmed".into(),
            error: true,
            transient: false
        }
    );
    assert_eq!(
        ended(false, false, "hello").text,
        "The message was not pushed: sending did not complete"
    );
    let attachments = ended(true, false, "[File #1]");
    assert_eq!(
        (attachments.text.as_str(), attachments.error),
        (
            "Sent without push: a message without text cannot be found in the agent's queue",
            false
        )
    );
    // A send that ended without a request (refused, dialog cancelled).
    let mut refused = Pusher::default();
    refused.arm(KEY, "hello", 0.0);
    assert_eq!(
        refused.check(1.0, facts()).unwrap(),
        Notice {
            text: "The message was not pushed: sending did not complete".into(),
            error: true,
            transient: false
        }
    );
}

#[test]
fn the_twenty_second_endings() {
    let info = |text: &str| Notice {
        text: text.into(),
        error: false,
        transient: false,
    };
    // Confirmed, but the agent never offered its queue.
    let mut offered = confirmed("hello");
    assert_eq!(offered.check(PUSH_SECONDS - 0.5, facts()), None);
    assert_eq!(
        offered.check(PUSH_SECONDS, facts()),
        Some(info("The message was sent without push: the agent did not offer to send it now within 20 seconds"))
    );
    // A snapshot after the deadline never sends the queue.
    let mut late = confirmed("hello");
    assert_eq!(
        late.inspected(KEY, &queue("q-1", "\u{276F} hello", true), PUSH_SECONDS + 1.0, facts(), false),
        PushStep::End(Some(info("The message was sent without push: the agent did not offer to send it now within 20 seconds")))
    );
    // Still in transport, or waiting for the session to start or compact.
    let mut transport = Pusher::default();
    transport.arm(KEY, "hello", 0.0);
    transport.dispatched(KEY, "hello", "req-1", "run-1", "conv-1");
    assert_eq!(
        transport.check(PUSH_SECONDS, facts()),
        Some(info(
            "The message will be sent without push: it was not delivered within 20 seconds"
        ))
    );
    let mut waiting = Pusher::default();
    waiting.arm(KEY, "hello", 0.0);
    let start = PushFacts {
        waiting: true,
        ..PushFacts::default()
    };
    assert_eq!(waiting.check(5.0, start), None);
    assert_eq!(
        waiting.check(PUSH_SECONDS, start),
        Some(info(
            "The message will be sent without push: it was not delivered within 20 seconds"
        ))
    );
    // Still behind a dialog: nothing was sent yet.
    let mut dialog = Pusher::default();
    dialog.arm(KEY, "hello", 0.0);
    let open = PushFacts {
        dialog: true,
        ..PushFacts::default()
    };
    assert_eq!(dialog.check(5.0, open), None);
    assert_eq!(
        dialog.check(PUSH_SECONDS, open),
        Some(info(
            "The message was not pushed: it was not sent within 20 seconds"
        ))
    );
}

#[test]
fn a_new_push_replaces_the_old_one() {
    let mut pusher = Pusher::default();
    assert_eq!(pusher.arm(KEY, "first", 0.0), None);
    assert_eq!(
        pusher.arm(KEY, "second", 1.0),
        Some(Notice {
            text: "The earlier push was replaced by a new one".into(),
            error: false,
            transient: false
        })
    );
    let push = pusher.active().unwrap();
    assert_eq!(
        (push.text.as_str(), push.deadline),
        ("second", 1.0 + PUSH_SECONDS)
    );
    // Only the armed text takes the request, and only once.
    pusher.dispatched(KEY, "first", "req-1", "run-1", "conv-1");
    assert_eq!(pusher.active().unwrap().request, None);
    pusher.dispatched(KEY, "second", "req-2", "run-1", "conv-1");
    pusher.dispatched(KEY, "second", "req-3", "run-2", "conv-2");
    let push = pusher.active().unwrap();
    assert_eq!(
        (push.request.as_deref(), push.run.as_str()),
        (Some("req-2"), "run-1")
    );
}

#[test]
fn selecting_another_session_ends_the_push() {
    let mut same = confirmed("hello");
    assert_eq!(same.selection_changed(Some(KEY), "review"), None);
    assert!(same.active().is_some());
    let mut other = confirmed("hello");
    assert_eq!(
        other
            .selection_changed(Some("studio\nother"), "review")
            .unwrap()
            .text,
        "The message to review was not pushed: another session was selected"
    );
    assert_eq!(other.active(), None);
    let mut none = confirmed("hello");
    assert_eq!(
        none.selection_changed(None, "review").unwrap().text,
        "The message to review was not pushed: its session is no longer selected"
    );
    let mut empty = confirmed("hello");
    assert_eq!(
        empty.selection_changed(Some(""), "review").unwrap().text,
        "The message to review was not pushed: its session is no longer selected"
    );
    assert_eq!(Pusher::default().selection_changed(None, "review"), None);
}

#[test]
fn inbox_removal_needs_a_not_delivered_message() {
    let details = json!({"run_id": "run-1"});
    let events = [
        json!({"type": "UserPromptSubmit", "message_id": "msg_a", "delivery_state": "not_delivered"}),
        json!({"type": "UserPromptSubmit", "message_id": "msg_b", "delivery_state": "queued"}),
    ];
    let facts = InboxFacts {
        cmd: "opencode",
        online: true,
        inspect_error: false,
        busy: false,
        allowed: true,
        details: &details,
        events: &events,
    };
    assert!(inbox_removable(&facts, "msg_a"));
    assert!(!inbox_removable(&facts, "msg_b"));
    assert!(!inbox_removable(&facts, "msg_c"));
    assert!(!inbox_removable(&facts, ""));
    let unrun = json!({"run_id": ""});
    for blocked in [
        InboxFacts {
            cmd: "claude",
            ..facts
        },
        InboxFacts {
            online: false,
            ..facts
        },
        InboxFacts {
            inspect_error: true,
            ..facts
        },
        InboxFacts {
            busy: true,
            ..facts
        },
        InboxFacts {
            allowed: false,
            ..facts
        },
        InboxFacts {
            details: &unrun,
            ..facts
        },
    ] {
        assert!(!inbox_removable(&blocked, "msg_a"));
    }
}
