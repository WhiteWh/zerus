//! Question cards against the Qt client: `QuestionCard`, the selection and
//! guards of `SessionsWindow::renderQuestion` and `answerQuestion`, and
//! `QuestionReply`. Store keys are byte-exact against Qt 6.12's compact JSON.
use serde_json::{json, Value};
use zerus_client_core::drafts::{Loaded, StoredDraft};
use zerus_client_core::questions::{
    self as q, actionable, annotate_replies, answered_follow_up, answers, broader, card,
    cold_answer, consent_identity, custom_answer_problem, decision, pending, replies,
    replies_preview, select, skip_answers, still_answerable, unavailable, Actionable, AnswerDraft,
    AnswerFacts, AnswerItem, CardContext, Edit, Question, Recheck,
};

fn question(raw: Value) -> Question {
    Question::from_value(&raw)
}

fn ask(id: &str, optional: bool, created: f64) -> Value {
    json!({"question_id": id, "question_hash": format!("h-{id}"), "run_id": "run-1",
           "conversation_id": "conv-1", "source": "hook", "created_at": created,
           "optional": optional, "can_answer": true, "can_skip": optional,
           "questions": [{"id": "q_0", "question": "Which database?", "header": "Database",
                          "options": [{"id": "pg", "label": "PostgreSQL"}, {"id": "lite", "label": "SQLite"}],
                          "allow_other": true}]})
}

fn details(entries: Vec<Value>) -> Value {
    json!({"run_id": "run-1", "conversation_id": "conv-1", "runtime_state": "live",
           "process_state": "running", "pending_questions": entries})
}

fn facts() -> AnswerFacts {
    AnswerFacts {
        online: true,
        state: "running".into(),
        updating: false,
        inspect_error: false,
    }
}

fn ctx() -> CardContext {
    CardContext {
        available: true,
        ..CardContext::default()
    }
}

#[test]
fn a_required_card_wins_and_hides_the_editor() {
    let mut cannot = ask("first", false, 1.0);
    cannot["can_answer"] = json!(false);
    let list = pending(&details(vec![
        ask("opt", true, 5.0),
        cannot,
        ask("second", false, 2.0),
    ]));
    let selection = select(&list, &|_| false, None, 0);
    // A later required one that can be answered replaces one that cannot.
    assert_eq!(selection.question.as_ref().unwrap().id, "second");
    assert!(!selection.input_visible());
    assert_eq!(selection.pending, 3);
    assert_eq!(selection.optional, ["opt"]);
    let optional = select(
        &pending(&details(vec![ask("opt", true, 5.0)])),
        &|_| false,
        None,
        0,
    );
    assert!(optional.input_visible());
    assert!(select(&[], &|_| false, None, 0).input_visible());
}

#[test]
fn optional_cards_are_newest_first_and_keep_the_choice() {
    let list = pending(&details(vec![
        ask("old", true, 1.0),
        ask("new", true, 3.0),
        ask("mid", true, 2.0),
    ]));
    let first = select(&list, &|_| false, None, 0);
    assert_eq!(first.optional, ["new", "mid", "old"]);
    assert_eq!((first.index, first.chosen.as_deref()), (0, None));
    let next = select(&list, &|_| false, None, 1);
    assert_eq!((next.index, next.chosen.as_deref()), (1, Some("mid")));
    assert_eq!(next.question.unwrap().id, "mid");
    // Polling keeps the remembered one; navigation stops at the ends.
    let kept = select(&list, &|_| false, Some("old"), 0);
    assert_eq!((kept.index, kept.chosen.as_deref()), (2, Some("old")));
    let end = select(&list, &|_| false, Some("old"), 1);
    assert_eq!(end.index, 2);
    // A remembered one that left is forgotten.
    let gone = select(&list, &|_| false, Some("gone"), 0);
    assert_eq!((gone.index, gone.chosen), (0, None));
    // Submitted and repeated requests are left out.
    let fewer = select(&list, &|question| question.id == "new", None, 0);
    assert_eq!(fewer.optional, ["mid", "old"]);
}

#[test]
fn a_card_without_items_that_cannot_be_answered_still_shows() {
    let mut empty = ask("form", false, 1.0);
    empty["questions"] = json!([]);
    assert!(
        select(&pending(&details(vec![empty.clone()])), &|_| false, None, 0)
            .question
            .is_none()
    );
    empty["can_answer"] = json!(false);
    empty["answer_unavailable_reason"] = json!("  Answer this form in OpenCode  ");
    let selection = select(&pending(&details(vec![empty])), &|_| false, None, 0);
    let shown = selection.question.unwrap();
    assert_eq!(selection.pending, 1);
    assert_eq!(shown.unavailable_reason, "Answer this form in OpenCode");
    let view = card(&shown, &AnswerDraft::default(), &ctx());
    assert_eq!(view.status.english(), "Answer this form in OpenCode");
    assert!(!view.submit.visible);
}

#[test]
fn availability_texts_follow_qts_order() {
    let d = details(vec![ask("a", false, 1.0)]);
    let shown = &pending(&d)[0];
    assert_eq!(unavailable(&facts(), shown, &d), None);
    let mut f = facts();
    f.online = false;
    f.state = "stopped".into();
    assert_eq!(
        unavailable(&f, shown, &d),
        Some("Machine unavailable. Your answers stay here.")
    );
    f.online = true;
    assert_eq!(
        unavailable(&f, shown, &d),
        Some("This session is not running.")
    );
    f.state = "running".into();
    f.updating = true;
    f.inspect_error = true;
    assert_eq!(
        unavailable(&f, shown, &d),
        Some("Wait for the session update to finish.")
    );
    f.updating = false;
    assert_eq!(
        unavailable(&f, shown, &d),
        Some("Refresh the session before answering.")
    );
    let mut moved = d.clone();
    moved["run_id"] = json!("run-2");
    assert_eq!(
        unavailable(&facts(), shown, &moved),
        Some("Refresh the pending question before answering.")
    );
    let mut ended = d.clone();
    ended["process_state"] = json!("exited");
    assert_eq!(
        unavailable(&facts(), shown, &ended),
        Some("The agent is no longer running.")
    );
}

#[test]
fn store_keys_and_labels_are_qts() {
    // QJsonDocument(QJsonArray{key, "q-1", "h"}).toJson(Compact), recorded
    // from Qt 6.12.
    let recorded = [
        ("studio\nclaude/p/a", r#"["studio\nclaude/p/a","q-1","h"]"#),
        (
            "claude/\u{43f}\u{440}\u{43e}\u{435}\u{43a}\u{442}/\"x\"",
            "[\"claude/\u{43f}\u{440}\u{43e}\u{435}\u{43a}\u{442}/\\\"x\\\"\",\"q-1\",\"h\"]",
        ),
        ("tab\there\\back", r#"["tab\there\\back","q-1","h"]"#),
        (
            "ctl\u{1}\u{1f}\u{7f} end",
            "[\"ctl\\u0001\\u001f\u{7f} end\",\"q-1\",\"h\"]",
        ),
    ];
    for (session, identity) in recorded {
        let shown = question(
            json!({"question_id": "q-1", "question_hash": "h", "run_id": "r", "conversation_id": "c"}),
        );
        assert_eq!(
            q::store_key(session, &shown).unwrap(),
            format!("question\n{identity}\nr\nc"),
            "{session:?}"
        );
    }
    assert_eq!(q::store_key("s", &question(json!({}))), None);
    assert_eq!(
        q::label("studio\nclaude/p/a"),
        "studio / claude/p/a (question answer)"
    );
}

fn loaded(form_state: Value, uncertain: bool) -> Loaded {
    Loaded {
        draft: StoredDraft {
            form_state: form_state.as_object().cloned().unwrap_or_default(),
            uncertain,
            ..StoredDraft::default()
        },
        storage_error: None,
        notice: None,
    }
}

#[test]
fn drafts_round_trip_through_form_state() {
    let shown = question(ask("a", false, 1.0));
    let mut draft = AnswerDraft::default();
    assert!(draft.apply(
        &shown,
        Edit::Option {
            item: "q_0".into(),
            option: "pg".into(),
            checked: true
        }
    ));
    draft.page = 0;
    let stored = draft.stored();
    assert_eq!(
        Value::Object(stored.form_state.clone()),
        json!({"answers": [{"id": "q_0", "text": "", "other": false, "options": ["pg"]}],
               "page": 0, "answered": false, "submitted": false})
    );
    let back = AnswerDraft::from_loaded(
        &Loaded {
            draft: stored,
            storage_error: None,
            notice: None,
        },
        "label",
    );
    assert_eq!(back.answers, draft.answers);
    // Seeded defaults are not a draft.
    let mut seeded = AnswerDraft::default();
    seeded.answers.insert("q_9".into(), Default::default());
    seeded.seeded.insert("q_9".into());
    assert_eq!(seeded.stored().form_state["answers"], json!([]));
    // An answered draft keeps neither answers nor text.
    let mut done = draft.clone();
    done.set_answered(false);
    let stored = done.stored();
    assert_eq!(stored.form_state["answers"], json!([]));
    assert_eq!(stored.draft.text, "");
    let restored = AnswerDraft::from_loaded(&loaded(json!({"answered": true}), false), "l");
    assert_eq!(restored.notice, "Response recorded.");
    let submitted =
        AnswerDraft::from_loaded(&loaded(json!({"submitted": true, "page": 2}), false), "l");
    assert_eq!(
        (submitted.notice.as_str(), submitted.page),
        (
            "Answer submitted. Waiting for the agent to record it. Check Activity or Terminal.",
            2
        )
    );
}

#[test]
fn a_sending_draft_loads_uncertain() {
    let mut saved = loaded(json!({}), true);
    saved.notice =
        Some("Delivery was interrupted. Check Activity or Terminal before sending again.".into());
    let draft = AnswerDraft::from_loaded(&saved, "l");
    assert!(draft.uncertain && draft.error);
    let view = card(&question(ask("a", false, 1.0)), &draft, &ctx());
    assert!(view.retry && !view.submit.enabled);
    assert_eq!(
        view.status.english(),
        "Delivery was interrupted. Check Activity or Terminal before sending again."
    );
}

#[test]
fn single_select_edits_are_exclusive_and_multi_toggles() {
    let shown = question(ask("a", false, 1.0));
    let mut draft = AnswerDraft::default();
    let option = |id: &str, checked: bool| Edit::Option {
        item: "q_0".into(),
        option: id.into(),
        checked,
    };
    assert!(draft.apply(&shown, option("pg", true)));
    assert!(draft.apply(&shown, option("lite", true)));
    assert_eq!(
        draft.answers["q_0"].options.iter().collect::<Vec<_>>(),
        ["lite"]
    );
    // Unchecking a radio does nothing; an unknown option or item neither.
    assert!(!draft.apply(&shown, option("lite", false)));
    assert!(!draft.apply(&shown, option("mysql", true)));
    assert!(!draft.apply(
        &shown,
        Edit::Text {
            item: "q_9".into(),
            text: "x".into()
        }
    ));
    assert!(draft.apply(
        &shown,
        Edit::Other {
            item: "q_0".into(),
            checked: true
        }
    ));
    assert!(draft.answers["q_0"].options.is_empty() && draft.answers["q_0"].other);
    assert!(draft.apply(&shown, option("pg", true)));
    assert!(!draft.answers["q_0"].other);
    let mut multi = ask("m", false, 1.0);
    multi["questions"][0]["multi_select"] = json!(true);
    let multi = question(multi);
    let mut draft = AnswerDraft::default();
    draft.apply(&multi, option("pg", true));
    draft.apply(&multi, option("lite", true));
    draft.apply(
        &multi,
        Edit::Other {
            item: "q_0".into(),
            checked: true,
        },
    );
    assert_eq!(draft.answers["q_0"].options.len(), 2);
    assert!(draft.answers["q_0"].other);
    draft.apply(&multi, option("pg", false));
    assert_eq!(
        draft.answers["q_0"].options.iter().collect::<Vec<_>>(),
        ["lite"]
    );
    // A locked draft takes no edits.
    draft.begin_sending();
    assert!(!draft.apply(&multi, option("pg", true)));
}

fn opencode() -> Question {
    question(
        json!({"question_id": "oc", "question_hash": "h", "run_id": "run-1",
        "conversation_id": "conv-1", "source": "opencode_native", "can_answer": true,
        "questions": [
            {"id": "pick", "question": "Pick", "multi_select": true, "min_items": 1, "max_items": 2,
             "options": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}, {"id": "c", "label": "C"}],
             "default_option_ids": ["a", "zz", "b"]},
            {"id": "name", "question": "Name", "allow_other": true, "max_length": 5,
             "default_text": "abcdefgh"}]}),
    )
}

#[test]
fn opencode_defaults_seed_but_are_not_saved() {
    let form = opencode();
    assert_eq!((form.items[0].min_items, form.items[0].max_items), (1, 2));
    assert_eq!(form.items[1].max_length, 5);
    let mut draft = AnswerDraft::default();
    draft.seed(&form);
    assert_eq!(
        draft.answers["pick"].options.iter().collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(draft.answers["name"].text, "abcde");
    assert_eq!(draft.seeded.len(), 2);
    assert_eq!(draft.stored().form_state["answers"], json!([]));
    // Seeds are sent as shown, and become the user's on submit.
    let (sent, complete) = answers(&form, &draft);
    assert!(complete);
    assert_eq!(
        serde_json::to_value(&sent).unwrap(),
        json!([{"question_id": "pick", "selected_option_ids": ["a", "b"], "text": ""},
               {"question_id": "name", "selected_option_ids": [], "text": "abcde"}])
    );
    assert!(!draft.confirm_step(&form));
    assert!(draft.seeded.is_empty());
    // An item emptied by the user is never refilled.
    let mut edited = AnswerDraft::default();
    edited.seed(&form);
    edited.apply(
        &form,
        Edit::Text {
            item: "name".into(),
            text: String::new(),
        },
    );
    edited.seed(&form);
    assert_eq!(edited.answers["name"].text, "");
    // OpenCode's limit in characters.
    assert!(!edited.apply(
        &form,
        Edit::Text {
            item: "name".into(),
            text: "abcdef".into()
        }
    ));
    assert!(edited.apply(
        &form,
        Edit::Text {
            item: "name".into(),
            text: "\u{1F600}bcde".into()
        }
    ));
}

#[test]
fn a_submitted_codex_answer_is_restored_and_hidden() {
    let mut raw = ask("cx", true, 1.0);
    raw["source"] = json!("codex_async");
    raw["answer_delivery"] = json!({"status": "submitted", "question_id": "cx", "question_hash": "h-cx",
        "run_id": "run-1", "conversation_id": "conv-1",
        "answers": [{"question_id": "q_0", "selected_option_ids": [], "text": "MySQL"}]});
    let shown = question(raw.clone());
    let mut draft = AnswerDraft::default();
    assert!(draft.restore_submitted(&shown));
    assert!(draft.submitted && !draft.sending);
    assert!(draft.answers["q_0"].other);
    assert_eq!(
        draft.notice,
        "Answer submitted. Waiting for Codex to record it. You can check the queue above or open Terminal."
    );
    assert!(!draft.restore_submitted(&shown));
    // Another run's delivery is not this question's.
    raw["answer_delivery"]["run_id"] = json!("run-0");
    assert!(!AnswerDraft::default().restore_submitted(&question(raw)));
    let view = card(&shown, &draft, &ctx());
    assert_eq!(
        (
            view.submit.label.english().as_str(),
            view.progress
                .as_ref()
                .map(|t| t.english())
                .unwrap_or_default()
                .as_str()
        ),
        ("Submitted", "Awaiting agent")
    );
}

#[test]
fn answers_follow_source_order_and_bounds() {
    let form = opencode();
    let mut draft = AnswerDraft::default();
    for id in ["c", "a"] {
        draft.apply(
            &form,
            Edit::Option {
                item: "pick".into(),
                option: id.into(),
                checked: true,
            },
        );
    }
    draft.apply(
        &form,
        Edit::Text {
            item: "name".into(),
            text: " Ann ".into(),
        },
    );
    let (sent, complete) = answers(&form, &draft);
    assert!(complete);
    assert_eq!(
        sent[0],
        AnswerItem::Choice {
            question_id: "pick".into(),
            selected_option_ids: vec!["a".into(), "c".into()],
            text: String::new()
        }
    );
    assert_eq!(
        sent[1],
        AnswerItem::Choice {
            question_id: "name".into(),
            selected_option_ids: vec![],
            text: "Ann".into()
        }
    );
    draft.apply(
        &form,
        Edit::Option {
            item: "pick".into(),
            option: "b".into(),
            checked: true,
        },
    );
    assert!(!answers(&form, &draft).1, "three over a maximum of two");
    // Other chosen without text is incomplete.
    let shown = question(ask("a", false, 1.0));
    let mut other = AnswerDraft::default();
    other.apply(
        &shown,
        Edit::Other {
            item: "q_0".into(),
            checked: true,
        },
    );
    assert!(!answers(&shown, &other).1);
    other.apply(
        &shown,
        Edit::Text {
            item: "q_0".into(),
            text: "MySQL".into(),
        },
    );
    assert!(answers(&shown, &other).1);
    assert!(!answers(&question(json!({"questions": []})), &AnswerDraft::default()).1);
    assert_eq!(
        serde_json::to_value(skip_answers(&shown)).unwrap(),
        json!([{"question_id": "q_0", "skip": true}])
    );
}

#[test]
fn custom_answer_problems() {
    assert_eq!(
        custom_answer_problem(&"x".repeat(4097), false),
        Some("Keep your custom answer under 4 KiB.")
    );
    assert_eq!(
        custom_answer_problem(&"\u{43f}".repeat(2049), false),
        Some("Keep your custom answer under 4 KiB.")
    );
    assert_eq!(
        custom_answer_problem("a\nb", false),
        Some("Use one line for a custom answer. Multiline input is available in Terminal.")
    );
    assert_eq!(
        custom_answer_problem("a\u{7}b", false),
        Some("Remove control characters from your custom answer.")
    );
    assert_eq!(
        custom_answer_problem("a\u{85}b", false),
        Some("Remove control characters from your custom answer.")
    );
    assert_eq!(
        custom_answer_problem(" /compact", false),
        Some("Use Terminal for answers starting with / or !.")
    );
    assert_eq!(custom_answer_problem("!ls", true), None);
    assert_eq!(custom_answer_problem("fine", false), None);
}

fn approval_choices() -> Question {
    question(
        json!({"question_id": "ap", "question_hash": "h", "run_id": "run-1",
        "conversation_id": "conv-1", "source": "claude_tool_approval", "can_answer": true,
        "approval": true, "approval_choices": true,
        "questions": [{"id": "q_0", "question": "Run rm -rf scratch?", "body": "rm -rf scratch",
            "options": [{"id": "yes", "label": "Yes"},
                        {"id": "always", "label": "Yes, and don't ask again", "scope": "broader"},
                        {"id": "no", "label": "No", "description": "Tell Claude what to do"}]}]}),
    )
}

#[test]
fn a_broader_option_needs_a_second_confirm() {
    let form = approval_choices();
    let mut draft = AnswerDraft::default();
    draft.apply(
        &form,
        Edit::Option {
            item: "q_0".into(),
            option: "always".into(),
            checked: true,
        },
    );
    assert_eq!(
        broader(&form, &draft).as_deref(),
        Some("Yes, and don't ask again")
    );
    assert!(draft.confirm_step(&form), "the first Submit only asks");
    let view = card(&form, &draft, &ctx());
    assert_eq!(view.submit.label.english(), "Confirm");
    assert_eq!(
        view.submit
            .review_label
            .as_ref()
            .map(|t| t.english())
            .as_deref(),
        Some("Submit answer")
    );
    assert_eq!(
        view.status.english(),
        "\u{201C}Yes, and don't ask again\u{201D} changes the agent's permissions beyond this request. Select Confirm to send it."
    );
    assert!(!draft.confirm_step(&form), "the second sends");
    let option = &view.forms[0].options[1];
    assert_eq!(
        option.description.english(),
        "Applies beyond this request. Asks for confirmation."
    );
    assert!(!option.markdown);
    assert_eq!(
        view.forms[0].options[2].description.english(),
        "Tell Claude what to do"
    );
    assert!(view.review && view.forms[0].monospace_body);
}

#[test]
fn an_edit_disarms_confirm() {
    let form = approval_choices();
    let mut draft = AnswerDraft::default();
    draft.apply(
        &form,
        Edit::Option {
            item: "q_0".into(),
            option: "always".into(),
            checked: true,
        },
    );
    assert!(draft.confirm_step(&form));
    draft.apply(
        &form,
        Edit::Option {
            item: "q_0".into(),
            option: "yes".into(),
            checked: true,
        },
    );
    assert!(draft.confirm.is_empty());
    draft.apply(
        &form,
        Edit::Option {
            item: "q_0".into(),
            option: "always".into(),
            checked: true,
        },
    );
    assert!(draft.confirm_step(&form), "asks again");
}

#[test]
fn card_labels_and_status_priority() {
    let shown = question(ask("a", false, 1.0));
    let mut draft = AnswerDraft::default();
    let view = card(&shown, &draft, &ctx());
    assert_eq!(view.heading.english(), "Agent needs your answer");
    assert_eq!(view.status.english(), "Answer each question to continue.");
    assert_eq!(
        (
            view.progress
                .as_ref()
                .map(|t| t.english())
                .unwrap_or_default()
                .as_str(),
            view.pending
                .as_ref()
                .map(|t| t.english())
                .unwrap_or_default()
                .as_str()
        ),
        ("0 of 1 answered", "1 pending")
    );
    assert_eq!(view.submit.label.english(), "Submit answer");
    assert!(view.submit.visible && !view.submit.enabled);
    assert_eq!(view.forms[0].tab.english(), "1: Database");
    assert_eq!(view.forms[0].mode.english(), "Select one");
    let other = view.forms[0].other.as_ref().unwrap();
    assert_eq!(
        (
            other.label.english().as_str(),
            other.description.english().as_str()
        ),
        ("Other", "Write your own answer")
    );
    let field = view.forms[0].text.as_ref().unwrap();
    assert!(!field.visible);
    assert_eq!(
        field.accessible_name.english(),
        "Your answer to Which database?"
    );
    assert_eq!(field.placeholder.english(), "Write your answer\u{2026}");
    draft.apply(
        &shown,
        Edit::Option {
            item: "q_0".into(),
            option: "pg".into(),
            checked: true,
        },
    );
    let ready = card(&shown, &draft, &ctx());
    assert_eq!(ready.status.english(), "Your choices are ready to send.");
    assert_eq!(ready.forms[0].tab.english(), "\u{2713} 1: Database");
    assert_eq!(
        ready
            .progress
            .as_ref()
            .map(|t| t.english())
            .unwrap_or_default(),
        "1 of 1 answered"
    );
    assert!(ready.submit.enabled);
    // A custom answer's problem comes before the generic texts.
    draft.apply(
        &shown,
        Edit::Other {
            item: "q_0".into(),
            checked: true,
        },
    );
    draft.apply(
        &shown,
        Edit::Text {
            item: "q_0".into(),
            text: "/x".into(),
        },
    );
    assert_eq!(
        card(&shown, &draft, &ctx()).status.english(),
        "Use Terminal for answers starting with / or !."
    );
    // Unavailable, then loading.
    let unavailable = CardContext {
        available: false,
        unavailable_reason: "This session is not running.".into(),
        ..CardContext::default()
    };
    assert_eq!(
        card(&shown, &AnswerDraft::default(), &unavailable)
            .status
            .english(),
        "This session is not running."
    );
    let loading = CardContext {
        loading: true,
        ..ctx()
    };
    let view = card(&shown, &AnswerDraft::default(), &loading);
    assert_eq!(view.status.english(), "Loading your saved answer\u{2026}");
    assert!(view.locked);
    // The draft's notice, then a storage error over it.
    let mut failing = AnswerDraft::default();
    failing.set_error("Answer transfer failed (exit 1).", false);
    assert_eq!(
        card(&shown, &failing, &ctx()).status.english(),
        "Answer transfer failed (exit 1)."
    );
    failing.storage_error = "Your draft could not be saved.".into();
    let view = card(&shown, &failing, &ctx());
    assert_eq!(view.status.english(), "Your draft could not be saved.");
    assert!(view.status_error);
    // An uncertain answer locks Submit until Allow retry.
    let mut uncertain = AnswerDraft::default();
    uncertain.apply(
        &shown,
        Edit::Option {
            item: "q_0".into(),
            option: "pg".into(),
            checked: true,
        },
    );
    uncertain.begin_sending();
    assert_eq!(
        card(&shown, &uncertain, &ctx()).submit.label.english(),
        "Submitting\u{2026}"
    );
    uncertain.set_error(
        "Answer delivery timed out. Check Terminal before retrying.",
        true,
    );
    let view = card(&shown, &uncertain, &ctx());
    assert_eq!(
        view.status.english(),
        "Delivery is not confirmed. Check Terminal before retrying. Answer delivery timed out. Check Terminal before retrying."
    );
    assert!(view.retry && !view.submit.enabled);
    uncertain.allow_retry();
    let view = card(&shown, &uncertain, &ctx());
    assert!(!view.retry && view.submit.enabled);
    // Optional and approval headings.
    let optional = question(ask("o", true, 1.0));
    let view = card(&optional, &AnswerDraft::default(), &ctx());
    assert_eq!(view.heading.english(), "Optional question");
    assert_eq!(
        view.status.english(),
        "The agent can continue. Answer when ready or skip this question."
    );
    assert!(view.skip.visible && view.skip.enabled);
    let queued = CardContext {
        queue_index: 1,
        queue_count: 3,
        ..ctx()
    };
    let view = card(&optional, &AnswerDraft::default(), &queued);
    assert_eq!(view.heading.english(), "Optional question #2/3");
    let nav = view.nav.unwrap();
    assert!(nav.previous && nav.next);
    assert_eq!(
        nav.next_tip.english(),
        "Next queued question (older)\nRequest 2 of 3, newest first. Draft answers are kept when switching."
    );
    // The status is cut to 600 units.
    let mut long = AnswerDraft::default();
    long.set_error(&"x".repeat(700), false);
    assert_eq!(card(&shown, &long, &ctx()).status.english().len(), 600);
}

#[test]
fn approvals_without_choices_use_approve_and_deny() {
    let form = question(
        json!({"question_id": "ap", "question_hash": "h", "run_id": "run-1",
        "conversation_id": "conv-1", "source": "hook", "approval": true, "can_answer": true,
        "questions": [{"id": "q_0", "question": "Allow?", "options": [{"id": "allow", "label": "Allow"}, {"id": "deny", "label": "Deny"}]}]}),
    );
    let view = card(&form, &AnswerDraft::default(), &ctx());
    assert_eq!(view.heading.english(), "Agent needs your approval");
    assert!(!view.submit.visible);
    assert!(view.approve.visible && view.approve.enabled && view.deny.enabled);
    assert_eq!(
        view.approve
            .review_label
            .as_ref()
            .map(|t| t.english())
            .as_deref(),
        Some("Approve once")
    );
    assert_eq!(view.deny.review_label, None);
    assert!(
        !view.forms[0].options[0].visible,
        "the pair has its own buttons"
    );
    assert_eq!(
        view.status.english(),
        "Approval applies only to this command. Review it before continuing."
    );
    assert_eq!(
        view.progress
            .as_ref()
            .map(|t| t.english())
            .unwrap_or_default(),
        ""
    );
    assert_eq!(
        serde_json::to_value(decision(&form, true).unwrap()).unwrap(),
        json!([{"question_id": "q_0", "selected_option_ids": ["allow"], "text": ""}])
    );
    assert_eq!(decision(&question(ask("a", false, 1.0)), true), None);
}

#[test]
fn read_only_cards_disable_their_options() {
    let mut raw = approval_choices().raw;
    raw["can_answer"] = json!(false);
    let form = question(raw);
    let view = card(&form, &AnswerDraft::default(), &ctx());
    assert!(view.forms[0].options.iter().all(|option| !option.enabled));
    assert!(!view.submit.visible);
    assert_eq!(view.status.english(), "Answer this request in Terminal.");
    assert!(!AnswerDraft::default().apply(
        &form,
        Edit::Option {
            item: "q_0".into(),
            option: "yes".into(),
            checked: true
        }
    ));
}

#[test]
fn changed_codex_rate_limit_card_opens_terminal_without_answer_options() {
    let form = question(
        json!({"question_id":"codex-rate-limit:fixture","question_hash":"fixture",
        "source":"codex_rate_limit","can_answer":false,
        "answer_unavailable_reason":"This Codex chooser changed or is incomplete. Open Terminal to answer it.",
        "questions":[{"id":"codex_rate_limit","question":"Approaching rate limits",
            "body":"Future native chooser text","options":[]}]}),
    );
    let view = card(&form, &AnswerDraft::default(), &ctx());
    assert!(!view.submit.visible);
    assert!(view.status.english().contains("Open Terminal"));
    assert!(view.forms.iter().all(|form| form.options.is_empty()));
}

#[test]
fn a_body_refresh_keeps_the_form() {
    let old = approval_choices();
    let mut raw = old.raw.clone();
    raw["questions"][0]["body"] = json!("rm -rf scratch (2s)");
    assert!(q::same_form(&old, &question(raw.clone())));
    raw["questions"][0]["options"][0]["label"] = json!("Sure");
    assert!(!q::same_form(&old, &question(raw)));
    let mut other_hash = old.raw.clone();
    other_hash["question_hash"] = json!("h2");
    assert!(!q::same_form(&old, &question(other_hash)));
    let mut hook = old.raw.clone();
    hook["source"] = json!("hook");
    assert!(!q::same_form(&question(hook.clone()), &question(hook)));
    // A body that was not shown cannot refresh in place.
    let mut bodiless = old.raw.clone();
    bodiless["questions"][0]["body"] = json!("");
    let mut refreshed = bodiless.clone();
    refreshed["questions"][0]["body"] = json!("now");
    assert!(!q::same_form(&question(bodiless), &question(refreshed)));
}

#[test]
fn actionable_is_silent_for_a_session_that_is_not_ready() {
    let d = details(vec![ask("a", false, 1.0)]);
    let mut f = facts();
    f.online = false;
    assert_eq!(actionable(&d, &f, "a", false), Actionable::Silent);
    let mut stopped = d.clone();
    stopped["runtime_state"] = json!("starting");
    assert_eq!(
        actionable(&stopped, &facts(), "a", false),
        Actionable::Silent
    );
    assert!(matches!(
        actionable(&d, &facts(), "a", false),
        Actionable::Ready(_)
    ));
}

#[test]
fn actionable_reports_a_changed_card() {
    let d = details(vec![ask("a", false, 1.0)]);
    assert_eq!(actionable(&d, &facts(), "gone", false), Actionable::Changed);
    assert_eq!(
        actionable(&d, &facts(), "a", true),
        Actionable::Changed,
        "not skippable"
    );
    let mut moved = d.clone();
    moved["pending_questions"][0]["run_id"] = json!("run-0");
    assert_eq!(
        actionable(&moved, &facts(), "a", false),
        Actionable::Changed
    );
    assert_eq!(
        q::CHANGED,
        "This question changed. Refresh before answering."
    );
}

#[test]
fn still_answerable_refuses_every_change() {
    let d = details(vec![ask("a", false, 1.0)]);
    let shown = pending(&d).remove(0);
    let pinned = consent_identity(&shown.raw);
    let f = facts();
    let base = Recheck {
        pinned: &pinned,
        question: &shown,
        evidence: &d,
        captured: &d,
        row_same: true,
        row_identity: true,
        facts: &f,
        skipping: false,
        consent: false,
        submitted: false,
        now: 100.0,
    };
    assert!(still_answerable(&base));
    assert!(!still_answerable(&Recheck {
        row_same: false,
        ..base
    }));
    assert!(!still_answerable(&Recheck {
        row_identity: false,
        ..base
    }));
    assert!(!still_answerable(&Recheck {
        submitted: true,
        ..base
    }));
    assert!(!still_answerable(&Recheck {
        skipping: true,
        ..base
    }));
    let offline = AnswerFacts {
        online: false,
        ..facts()
    };
    assert!(!still_answerable(&Recheck {
        facts: &offline,
        ..base
    }));
    let mut changed = d.clone();
    changed["pending_questions"][0]["questions"][0]["question"] = json!("Which cache?");
    assert!(!still_answerable(&Recheck {
        evidence: &changed,
        ..base
    }));
    let mut moved = d.clone();
    moved["conversation_id"] = json!("conv-2");
    assert!(!still_answerable(&Recheck {
        evidence: &moved,
        ..base
    }));
    // Consent covers the listed fields present: not created_at or can_*.
    assert_eq!(consent_identity(&shown.raw).as_object().unwrap().len(), 7);
    let mut unrelated = d.clone();
    unrelated["pending_questions"][0]["created_at"] = json!(9.0);
    assert!(still_answerable(&Recheck {
        evidence: &unrelated,
        ..base
    }));
}

fn cold(details: &mut Value) {
    details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
}

#[test]
fn a_cold_cache_asks_except_kimis_own_panel() {
    let mut d = details(vec![ask("a", false, 1.0)]);
    let shown = pending(&d).remove(0);
    assert_eq!(cold_answer(&d, &shown, 100.0), None);
    cold(&mut d);
    {
        let asked = cold_answer(&d, &shown, 10_000.0).unwrap();
        assert_eq!(asked.title, "Submit with a cold cache?");
        assert_eq!(asked.proceed, "Submit with full context");
        assert_eq!(
            asked.text,
            "The agent reports a cold cache (120,000 tokens). Continuing will rebuild the cached context.\n\nSubmitting this answer may resume the agent with the entire conversation, increasing token usage, cost or subscription usage. Compaction or clearing can invalidate the pending question. Cancel keeps your answers."
        );
        let mut kimi = shown.raw.clone();
        kimi["source"] = json!("kimi_cache_hint");
        kimi["answer_transport"] = json!("kimi_tui");
        assert_eq!(cold_answer(&d, &question(kimi), 10_000.0), None);
        // Without consent the recheck refuses a cold cache.
        let pinned = consent_identity(&shown.raw);
        let f = facts();
        let check = Recheck {
            pinned: &pinned,
            question: &shown,
            evidence: &d,
            captured: &d,
            row_same: true,
            row_identity: true,
            facts: &f,
            skipping: false,
            consent: false,
            submitted: false,
            now: 10_000.0,
        };
        assert!(!still_answerable(&check));
        assert!(still_answerable(&Recheck {
            consent: true,
            ..check
        }));
    }
}

#[test]
fn codex_update_follow_ups() {
    let hash = "a".repeat(64);
    let update =
        question(json!({"question_id": format!("codex-update:{hash}"), "question_hash": hash}));
    let receipt = json!({"question_hash": "a".repeat(64), "run_id": "run-1", "open_terminal": true, "codex_update": "installing"});
    let follow = answered_follow_up(&update, &receipt, Some("run-1"), "codex/p (studio)");
    assert!(follow.open_terminal);
    assert_eq!(
        follow.notice.as_deref(),
        Some("Codex in codex/p (studio) is installing its update in Terminal. Resume the session after Codex exits.")
    );
    let mut exited = receipt.clone();
    exited["codex_update"] = json!("exited");
    assert_eq!(
        answered_follow_up(&update, &exited, None, "codex/p (studio)").notice.as_deref(),
        Some("Codex in codex/p (studio) exited after its update. Check the result in Terminal, then resume the session.")
    );
    assert!(
        !answered_follow_up(&update, &receipt, None, "x").open_terminal,
        "another session selected"
    );
    assert!(!answered_follow_up(&update, &receipt, Some("run-2"), "x").open_terminal);
    let plain = question(ask("a", false, 1.0));
    assert_eq!(
        answered_follow_up(&plain, &receipt, Some("run-1"), "x"),
        Default::default()
    );
}

#[test]
fn replies_parse_and_preview() {
    let one = r#"<send_user_message_question_reply>{"questionItemId":"q_0","question":"Which?","answer":"PostgreSQL"}</send_user_message_question_reply>"#;
    let parsed = replies(one);
    assert_eq!(
        (parsed[0].question.as_str(), parsed[0].answer.as_str()),
        ("Which?", "PostgreSQL")
    );
    assert_eq!(replies_preview(&parsed), "Your answer: PostgreSQL");
    let two = r#"  <send_user_message_question_reply>[{"questionItemId":"a","question":"A?","answer":"x"},{"questionItemId":"b","question":"B?","answer":""}]</send_user_message_question_reply> "#;
    assert_eq!(
        replies_preview(&replies(two)),
        "Your answers: x / (empty answer)"
    );
    let ide =
        format!("# Context from my IDE setup:\n## Open tabs\n\n## My request for Codex:\n{one}\n");
    assert_eq!(replies(&ide).len(), 1);
    assert!(replies("# Context from my IDE setup:\nno request").is_empty());
    for bad in [
        "plain text",
        r#"<send_user_message_question_reply>{"questionItemId":"","question":"Q","answer":"a"}</send_user_message_question_reply>"#,
        r#"<send_user_message_question_reply>{"questionItemId":"a","question":"  ","answer":"a"}</send_user_message_question_reply>"#,
        r#"<send_user_message_question_reply>{"questionItemId":"a","question":"Q","answer":1}</send_user_message_question_reply>"#,
        r#"<send_user_message_question_reply>[1]</send_user_message_question_reply>"#,
        r#"<send_user_message_question_reply>not json</send_user_message_question_reply>"#,
    ] {
        assert!(replies(bad).is_empty(), "{bad}");
    }
    assert_eq!(replies_preview(&[]), "");
}

#[test]
fn annotate_replies_marks_user_events_only() {
    let reply = r#"<send_user_message_question_reply>{"questionItemId":"q","question":"Q?","answer":"yes"}</send_user_message_question_reply>"#;
    let mut events = json!({"events": [
        {"type": "UserPromptSubmit", "detail": reply},
        {"type": "AssistantMessage", "detail": reply},
        {"type": "TurnStarted", "detail": "plain"}], "cursor": 3});
    annotate_replies(&mut events);
    assert_eq!(
        events["events"][0]["question_replies"],
        json!([{"question": "Q?", "answer": "yes"}])
    );
    assert!(events["events"][1].get("question_replies").is_none());
    assert!(events["events"][2].get("question_replies").is_none());
    let mut event =
        json!({"id": "e", "rev": 1, "event": {"type": "QuestionAnswered", "detail": reply}});
    annotate_replies(&mut event);
    assert!(event["event"]["question_replies"].is_array());
    let mut state = json!({"value": {"prompt": reply}, "rev": 2});
    annotate_replies(&mut state);
    assert_eq!(state["value"]["prompt_replies"][0]["answer"], "yes");
}

#[test]
fn card_previews_read_answers() {
    let reply = r#"<send_user_message_question_reply>{"questionItemId":"q","question":"Q?","answer":"yes"}</send_user_message_question_reply>"#;
    assert_eq!(
        q::prompt_preview(&json!({"prompt": reply})),
        "Your answer: yes"
    );
    assert_eq!(q::prompt_preview(&json!({"prompt": "hello"})), "hello");
    // The detail's own answers, or the prompt's when it is its clipped excerpt.
    assert_eq!(
        q::activity_detail_preview(&json!({"activity_detail": reply})),
        "Your answer: yes"
    );
    let excerpt: String = reply.chars().take(40).collect::<String>() + "\u{2026}";
    assert_eq!(
        q::activity_detail_preview(&json!({"activity_detail": excerpt, "prompt": reply})),
        "Your answer: yes"
    );
    // An older answer never replaces the current tool or approval text.
    assert_eq!(
        q::activity_detail_preview(&json!({"activity_detail": "rm -rf scratch", "prompt": reply})),
        "rm -rf scratch"
    );
}
