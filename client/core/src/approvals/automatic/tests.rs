use super::*;
pub(super) fn q() -> Value {
    json!({"approval":true,"can_answer":true,"approval_choices":true,"source":"claude_tool_approval","answer_transport":"claude_tui","question_id":"approval-one","question_hash":"a".repeat(64),"run_id":"run-one","conversation_id":"conversation-one","questions":[{"id":"approval","question":"Synthetic permission","allow_other":false,"options":[{"id":"yes","label":"Yes","scope":"once"},{"id":"always","label":"Always allow","scope":"broader"}]}]})
}
fn row() -> ViewRow {
    let mut row = ViewRow {
        name: "claude/example/review".into(),
        run_id: Some("run-one".into()),
        conversation_id: Some("conversation-one".into()),
        tracked: true,
        state: "running".into(),
        phase: Some("approval".into()),
        created: 4,
        ..Default::default()
    };
    row.notify.cmd = "claude".into();
    row
}
#[test]
fn default_is_off_and_never_seeded_from_qt() {
    let key = crate::settings::find(ENABLED).unwrap();
    assert!(!key.seeded);
    assert_eq!(key.kind, crate::settings::Kind::Flag(false));
    assert!(!crate::settings::find(ERROR).unwrap().seeded);
}
#[test]
fn only_supported_live_candidates_with_both_identities_are_considered() {
    let mut r = row();
    assert!(candidate("peer", &r).is_some());
    r.notify.cmd = "codex".into();
    assert!(candidate("peer", &r).is_none());
    r.notify.cmd = "claude".into();
    r.conversation_id = None;
    assert!(candidate("peer", &r).is_none());
    r = row();
    r.tracked = false;
    assert!(candidate("peer", &r).is_none());
    r = row();
    r.process_state = "exited".into();
    assert!(candidate("peer", &r).is_none());
    r = row();
    r.phase = Some("input".into());
    assert!(candidate("peer", &r).is_none());
    r.notify.cmd = "opencode".into();
    assert!(candidate("peer", &r).is_some());
}
#[test]
fn ordinary_trust_permanent_ambiguous_and_unknown_permissions_remain_manual() {
    for change in [
        json!({"approval":false}),
        json!({"can_answer":false}),
        json!({"trust_request":true}),
        json!({"source":"claude_folder_trust"}),
        json!({"answer_transport":"unknown"}),
        json!({"approval_choices":false}),
    ] {
        let mut question = q();
        for (k, v) in change.as_object().unwrap() {
            question[k] = v.clone();
        }
        assert!(once("claude", &question).is_none());
    }
    let mut question = q();
    question["questions"][0]["options"][0]["scope"] = json!("broader");
    assert!(once("claude", &question).is_none());
    question = q();
    let yes = question["questions"][0]["options"][0].clone();
    question["questions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .push(yes);
    assert!(once("claude", &question).is_none());
    question = q();
    question["questions"][0]["allow_other"] = json!(true);
    assert!(once("claude", &question).is_none());
    question["answer_transport"] = json!("claude_hook");
    question["panel_verified"] = json!(true);
    assert!(once("claude", &question).is_some());
}
#[test]
fn native_opencode_and_deepseek_choose_only_the_reviewed_once_option() {
    let mut question = q();
    question["source"] = json!("opencode_native");
    question["answer_transport"] = json!("native_opencode");
    question["kind"] = json!("opencode_permission");
    question["questions"][0]["id"] = json!("permission");
    question["questions"][0]["options"] = json!([{"id":"once"},{"id":"always"}]);
    assert!(once("opencode", &question).is_some());
    question["questions"][0]["options"] = json!([{"id":"always"}]);
    assert!(once("opencode", &question).is_none());
    question["source"] = json!("dsh_native");
    question["answer_transport"] = json!("native_dsh");
    question["questions"][0]["id"] = json!("decision");
    question["questions"][0]["options"] =
        json!([{"id":"allow","label":"Allow once"},{"id":"deny","label":"Deny"}]);
    assert!(once("dsh", &question).is_some());
    question["questions"][0]["options"][0]["label"] = json!("Always allow");
    assert!(once("dsh", &question).is_none());
}
#[test]
fn fresh_details_bind_the_original_run_conversation_and_bounded_question() {
    let target = candidate("", &row()).unwrap();
    let mut details = json!({"runtime_state":"live","process_state":"running","run_id":"run-one","conversation_id":"conversation-one","pending_questions":[q()]});
    assert!(question(&target, &details).is_some());
    details["conversation_id"] = json!("other");
    assert!(question(&target, &details).is_none());
    details["conversation_id"] = json!("conversation-one");
    details["pending_questions"][0]["run_id"] = json!("other");
    assert!(question(&target, &details).is_none());
    details["pending_questions"][0] = q();
    details["pending_questions"][0]["extension"] = json!("x".repeat(MAX_QUESTION));
    assert!(question(&target, &details).is_none());
}
#[test]
fn only_a_fresh_exact_durable_reservation_admits_delivery() {
    let question = q();
    let mut reply = json!({"eligible":true,"fresh":true,"id":1,"request_id":"10000000-0000-4000-8000-000000000001","answers":once("claude",&question).unwrap()});
    assert!(reserved(&reply, "claude", &question).unwrap().is_some());
    reply["fresh"] = json!(false);
    assert!(reserved(&reply, "claude", &question).unwrap().is_none());
    reply["fresh"] = json!(true);
    reply["answers"][0]["selected_option_ids"] = json!(["always"]);
    assert!(reserved(&reply, "claude", &question).is_err());
    reply["answers"] = json!(once("claude", &question).unwrap());
    reply["request_id"] = json!("");
    assert!(reserved(&reply, "claude", &question).is_err());
}

#[test]
fn duplicate_native_option_ids_never_make_a_broader_grant_eligible() {
    let mut question = q();
    question["questions"][0]["options"][1]["id"] = json!("yes");
    assert!(once("claude", &question).is_none());
}
