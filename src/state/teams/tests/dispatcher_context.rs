use super::super::ops::{self, Actor};
use super::*;
use super::support::*;

#[test]
fn a_members_context_setting_overrides_its_role() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let t = team();
        let dev = t.member("dev").unwrap().clone();
        assert_eq!(context_before_task(&t, &dev), "", "no fields keeps today's none");
        let mut role = dev.clone();
        role.prompt_snapshot = Some(PromptSnapshot { context_before_task: "clear".into(), ..Default::default() });
        assert_eq!(context_before_task(&t, &role), "clear");
        // A member's own setting wins, and "none" overrides the role.
        role.context_before_task = "compact".into();
        assert_eq!(context_before_task(&t, &role), "compact");
        role.context_before_task = "none".into();
        assert_eq!(context_before_task(&t, &role), "");
    });
}

#[test]
fn a_team_without_a_context_action_hands_out_and_decides_as_today() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let t = team();
        // A team with none of U46's fields loads and serializes as today.
        let value = serde_json::to_value(&t).unwrap();
        assert!(value["members"][1].get("context_before_task").is_none(), "{value}");
        assert!(value["tasks"][0].get("waits").is_none(), "{value}");
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let record = calls.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], _| {
            record.borrow_mut().push(args.first().copied().unwrap_or_default().to_owned());
            Ok(fake_output(0, r#"{"status":"submitted"}"#, ""))
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        let saved = store.load(&t.id).unwrap().unwrap();
        let attempt = saved.task(1).unwrap().attempts.last().unwrap();
        assert!(attempt.prepare.is_empty(), "{attempt:?}");
        assert_eq!(attempt.state, AttemptState::Delivered);
        assert_eq!(*calls.borrow(), ["send"], "a team that sets none gets today's hand-out, with no context call");
        // A report and an accept keep today's events too.
        let attempt_id = attempt.id.clone();
        store
            .mutate(Some(("22222222-2222-4222-8222-222222222222", &json!({}))), &t.id, None, |team, events| {
                ops::apply(
                    team,
                    "report",
                    &json!({"task":1,"attempt":attempt_id,"text":"Done"}),
                    &Actor::Member("dev".into()),
                    &BTreeSet::new(),
                    events,
                )?;
                Ok(Value::Null)
            })
            .unwrap();
        store
            .mutate(Some(("33333333-3333-4333-8333-333333333333", &json!({}))), &t.id, None, |team, events| {
                ops::apply(team, "accept", &json!({"task":1}), &Actor::Human, &BTreeSet::new(), events)?;
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(event_kinds(&store, &t.id), ["handed_out", "delivered", "reported", "accepted"]);
        set_fake_hgs(None);
    });
}

#[test]
fn a_compact_waits_for_its_confirmation_before_the_hand_out() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "compact");
        // The member overrides the role snapshot, which asks for clear.
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().prompt_snapshot = Some(PromptSnapshot {
            context_before_task: "clear".into(),
            ..Default::default()
        });
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0));
        let (seen, seen_request, seen_inspects) = (calls.clone(), request.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let input = input.unwrap();
                    let id = input["request_id"].as_str().unwrap().to_owned();
                    assert_eq!(input["expected_run_id"], "r1");
                    assert_eq!(input["expected_conversation_id"], "c1");
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                // The first look shows it still compacting: the hand-out waits.
                "inspect" => {
                    seen_inspects.set(seen_inspects.get() + 1);
                    let status = if seen_inspects.get() == 1 { "compacting" } else { "completed" };
                    Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                        "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                            "conversation_id":"c1","status":status}}).to_string(), ""))
                }
                "send" => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        // The action is started and recorded; nothing is sent yet.
        assert_eq!(*calls.borrow(), ["compact-context"]);
        let saved = store.load(&t.id).unwrap().unwrap();
        let wait = saved.task(1).unwrap().waits.as_ref().expect("the wait is recorded");
        assert_eq!((wait.state.as_str(), wait.action.as_str(), wait.conversation.as_str()), ("waiting", "compact", "c1"));
        assert!(!wait.request_id.is_empty());
        assert!(saved.task(1).unwrap().attempts.is_empty());
        // A tick while it waits reads the record once and runs no action again.
        assert!(prepare_tick(&mut store, &t.id));
        assert_eq!(*calls.borrow(), ["compact-context", "inspect"]);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().waits.as_ref().unwrap().state, "waiting");
        assert!(saved.task(1).unwrap().attempts.is_empty());
        // The confirmation arrives: the next tick hands out.
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        let task = saved.task(1).unwrap();
        assert!(task.waits.is_none(), "the confirmed wait is gone");
        let attempt = task.attempts.last().unwrap();
        assert_eq!(
            (attempt.prepare[0].step.as_str(), attempt.prepare[0].to.as_str(), attempt.prepare[0].result.as_str()),
            ("context", "compact", "done")
        );
        assert_eq!(attempt.state, AttemptState::Delivered);
        assert_eq!(*calls.borrow(), ["compact-context", "inspect", "inspect", "send"], "the send waits for the confirmation");
        assert_eq!(inspects.get(), 2);
        assert_eq!(event_kinds(&store, &t.id), ["context_prepared", "handed_out", "delivered"]);
    });
}

#[test]
fn a_clear_waits_for_the_new_conversation_before_the_hand_out() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.task_mut(1).unwrap().description = "Build the cards API".into();
        t.task_mut(1).unwrap().criteria = vec!["Paged list".into()];
        // The role snapshot asks for a clear; the member adds no override.
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.session = Some(SessionRef { name: "codex/x/dev".into(), run_id: "r1".into(), conversation_id: "c1".into(), ..Default::default() });
        dev.state = MemberState::Active;
        dev.prompt_snapshot = Some(PromptSnapshot { context_before_task: "clear".into(), ..Default::default() });
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0));
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Value::Null));
        let (seen, seen_inspects, seen_sent) = (calls.clone(), inspects.clone(), sent.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "clear-context" => Ok(fake_output(0, &json!({"request_id":input.unwrap()["request_id"],
                    "name":"codex/x/dev","run_id":"r1","conversation_id":"c1","status":"submitted"}).to_string(), "")),
                "inspect" => {
                    seen_inspects.set(seen_inspects.get() + 1);
                    let conversation = if seen_inspects.get() == 1 { "c1" } else { "c2" };
                    Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":conversation,"activity":"idle","phase":"idle"}).to_string(), ""))
                }
                "send" => {
                    *seen_sent.borrow_mut() = input.cloned().unwrap_or_default();
                    Ok(fake_output(0, r#"{"status":"submitted"}"#, ""))
                }
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        assert_eq!(*calls.borrow(), ["clear-context"]);
        let saved = store.load(&t.id).unwrap().unwrap();
        let wait = saved.task(1).unwrap().waits.as_ref().expect("the wait is recorded");
        assert_eq!((wait.state.as_str(), wait.action.as_str(), wait.conversation.as_str()), ("waiting", "clear", "c1"));
        assert!(saved.task(1).unwrap().attempts.is_empty());
        // The old conversation is still there: the hand-out waits.
        assert!(prepare_tick(&mut store, &t.id));
        assert_eq!(*calls.borrow(), ["clear-context", "inspect"]);
        // The new conversation appears: the next tick hands out naming it.
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.task(1).unwrap().waits.is_none());
        let attempt = saved.task(1).unwrap().attempts.last().unwrap();
        assert_eq!((attempt.prepare[0].to.as_str(), attempt.prepare[0].result.as_str()), ("clear", "done"));
        assert_eq!(attempt.state, AttemptState::Delivered);
        assert_eq!(*calls.borrow(), ["clear-context", "inspect", "inspect", "send"]);
        // The member's session follows the new conversation, and the send names it.
        assert_eq!(saved.member("dev").unwrap().session.as_ref().unwrap().conversation_id, "c2");
        assert_eq!(sent.borrow()["expected_conversation_id"], "c2");
        // A cleared context gets the task's own input, never "as before".
        let text = crate::state::teams::doorbell::posted_text(&sent.borrow());
        assert!(text.contains("Build the cards API"), "{text}");
        assert!(text.contains("Paged list"), "{text}");
        assert!(!text.to_lowercase().contains("as before"), "{text}");
    });
}

#[test]
fn a_tick_while_the_context_waits_does_not_run_the_action_again() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "compact");
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let (seen, seen_request) = (calls.clone(), request.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                "inspect" => Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                    "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                        "conversation_id":"c1","status":"compacting"}}).to_string(), "")),
                "send" => panic!("a waiting context never hands out"),
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        let recorded = store.load(&t.id).unwrap().unwrap().task(1).unwrap().waits.clone().unwrap();
        assert!(prepare_tick(&mut store, &t.id));
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        assert_eq!(*calls.borrow(), ["compact-context", "inspect", "inspect"], "only looks, never the action again");
        let saved = store.load(&t.id).unwrap().unwrap();
        let wait = saved.task(1).unwrap().waits.as_ref().unwrap();
        assert_eq!((wait.state.as_str(), wait.request_id.as_str()), ("waiting", recorded.request_id.as_str()));
        assert!(saved.task(1).unwrap().attempts.is_empty());
    });
}

#[test]
fn a_restart_resumes_the_context_request_recorded_on_the_task() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let path = dir.path().join("teams/teams.sqlite3");
        let mut store = Store::open_at(&path, true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "compact");
        stored(&mut store, &t);
        let started = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let calls = started.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            calls.borrow_mut().push(args[0].to_owned());
            match args[0] {
                "compact-context" => Ok(fake_output(0, &json!({"request_id":input.unwrap()["request_id"],
                    "status":"submitted"}).to_string(), "")),
                other => panic!("the start runs the action only: {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        set_fake_hgs(None);
        let recorded = store.load(&t.id).unwrap().unwrap().task(1).unwrap().waits.clone().unwrap();
        assert_eq!((recorded.state.as_str(), recorded.request_id.is_empty()), ("waiting", false));
        assert_eq!(*started.borrow(), ["compact-context"]);
        // A restarted worker opens the same store and resumes from the record.
        drop(store);
        let mut store = Store::open_at(&path, true).unwrap();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let resumed = calls.clone();
        let request_id = recorded.request_id.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], _| {
            resumed.borrow_mut().push(args[0].to_owned());
            match args[0] {
                "inspect" => Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                    "compact_context_request":{"request_id":request_id,"run_id":"r1","conversation_id":"c1","status":"completed"}}).to_string(), "")),
                "send" => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
                other => panic!("the restart runs no action again: {other}"),
            }
        })));
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        assert_eq!(*calls.borrow(), ["inspect", "send"], "no compact again after the restart");
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.task(1).unwrap().waits.is_none());
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
    });
}

#[test]
fn an_unconfirmed_context_leaves_the_task_waiting_and_never_retries_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "compact");
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let seen = calls.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            seen.borrow_mut().push(args[0].to_owned());
            match args[0] {
                "compact-context" => Ok(fake_output(0, &json!({"request_id":input.unwrap()["request_id"],
                    "status":"submitted"}).to_string(), "")),
                other => panic!("a timed-out wait only looks at the clock: {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        // The action is still running; a later tick finds it past the bound.
        store
            .mutate(None, &t.id, None, |team, _| {
                team.task_mut(1).unwrap().waits.as_mut().unwrap().started_at = crate::state::now() - 61.0;
                Ok(Value::Null)
            })
            .unwrap();
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        assert_eq!(*calls.borrow(), ["compact-context"], "the bound is checked before any read");
        let saved = store.load(&t.id).unwrap().unwrap();
        let task = saved.task(1).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        let attempt = task.attempts.last().unwrap();
        assert_eq!((attempt.prepare[0].result.as_str(), attempt.state), ("uncertain", AttemptState::Refused));
        let wait = task.waits.as_ref().expect("the task waits for Zerus");
        assert_eq!(wait.state, "uncertain");
        assert!(wait.reason.contains("not confirmed within 60 s"), "{wait:?}");
        // It is not ready again, and status names Zerus, not the member.
        assert!(!plan::ready(&saved).contains(&1));
        assert_eq!(super::inbox::waits(&saved, task, false).unwrap()["for"], "zerus");
        let items = super::inbox::items(&saved);
        assert!(items.iter().any(|item| item.kind == "task_waits" && item.task == Some(1)), "{items:?}");
        // A retry (the lead or the owner) starts the preparation again.
        let mut cleared = saved.clone();
        let mut events = Vec::new();
        ops::apply(&mut cleared, "retry", &json!({"task":1}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
        assert!(cleared.task(1).unwrap().waits.is_none());
    });
}

#[test]
fn a_refused_context_leaves_the_task_waiting_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "clear");
        t.task_mut(1).unwrap().fresh_retry = true;
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("clear-context") => Ok(fake_output(1, "", "hgs: Wait for the agent to become idle before clearing context")),
            Some("send") => panic!("a refused clear never hands out"),
            other => panic!("unexpected hgs {other:?}"),
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        let task = saved.task(1).unwrap();
        let attempt = task.attempts.last().unwrap();
        assert_eq!((attempt.prepare[0].result.as_str(), attempt.state), ("refused", AttemptState::Refused));
        assert!(!task.fresh_retry, "an automatic refusal drops the fresh exemption");
        let wait = task.waits.as_ref().expect("the task waits for Zerus");
        assert!(wait.reason.contains("Wait for the agent to become idle"), "{wait:?}");
        assert_eq!(event_kinds(&store, &t.id), ["prepare_failed"]);
    });
}

#[test]
fn an_agent_without_context_actions_gets_none_with_one_feed_note() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        // OpenCode has no native clear: the task continues the conversation.
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().agent = "opencode".into();
        with_context(&mut t, "dev", "clear");
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("send") => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
            other => panic!("a skipped context calls no native action: {other:?}"),
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        hand_out(&mut store, &t.id, 2, "dev", None, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        for id in [1, 2] {
            let attempt = saved.task(id).unwrap().attempts.last().unwrap();
            assert_eq!(attempt.prepare[0].result, "skipped", "{attempt:?}");
            assert_eq!(attempt.prepare[0].to, "none");
            assert_eq!(attempt.prepare[0].reason, "OpenCode has no clear; the task continues the conversation");
        }
        assert!(saved.member("dev").unwrap().context_noted);
        let noted = event_kinds(&store, &t.id).into_iter().filter(|kind| kind == "context_skipped").count();
        assert_eq!(noted, 1, "one feed note per member");
    });
}

/// Task 209: a member whose collected reading reached the team's threshold is
/// compacted before the hand-out, and the task goes once the compaction is
/// confirmed, as any U46 action does.
#[test]
fn the_context_threshold_compacts_before_a_hand_out() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        let at = crate::state::now();
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().context = Some(MemberContext::of(960_000, 1_000_000, at));
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0));
        let (seen, seen_request, seen_inspects) = (calls.clone(), request.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                "inspect" => {
                    seen_inspects.set(seen_inspects.get() + 1);
                    let status = if seen_inspects.get() == 1 { "compacting" } else { "completed" };
                    Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                        "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                            "conversation_id":"c1","status":status}}).to_string(), ""))
                }
                "send" => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        // The reading chose the compact; nothing is sent yet.
        assert_eq!(*calls.borrow(), ["compact-context"]);
        let saved = store.load(&t.id).unwrap().unwrap();
        let wait = saved.task(1).unwrap().waits.as_ref().expect("the wait is recorded");
        assert_eq!((wait.state.as_str(), wait.action.as_str()), ("waiting", "compact"));
        assert!(saved.task(1).unwrap().attempts.is_empty());
        assert!(prepare_tick(&mut store, &t.id));
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        let attempt = saved.task(1).unwrap().attempts.last().unwrap();
        assert_eq!(attempt.state, AttemptState::Delivered);
        assert_eq!(
            (attempt.prepare[0].step.as_str(), attempt.prepare[0].to.as_str(), attempt.prepare[0].result.as_str()),
            ("context", "compact", "done")
        );
        assert_eq!(*calls.borrow(), ["compact-context", "inspect", "inspect", "send"]);
        let kinds = event_kinds(&store, &t.id);
        assert!(kinds.contains(&"context_prepared".to_owned()), "{kinds:?}");
    });
}

/// Task 209: an idle member mid-task whose meter reached the threshold is
/// compacted and told to continue the same attempt; the attempt is not ended.
#[test]
fn an_idle_member_at_the_threshold_is_compacted_and_continues_its_attempt() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        delivered(&mut t);
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().context =
            Some(MemberContext::of(970_000, 1_000_000, crate::state::now()));
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Value::Null));
        let (seen, seen_request, seen_sent) = (calls.clone(), request.clone(), sent.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                "inspect" => Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                    "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                        "conversation_id":"c1","status":"completed"}}).to_string(), "")),
                "send" => {
                    *seen_sent.borrow_mut() = input.cloned().unwrap_or_default();
                    Ok(fake_output(0, r#"{"status":"submitted"}"#, ""))
                }
                other => panic!("unexpected hgs {other}"),
            }
        })));
        let step = steps(&t, &free_sessions(), crate::state::now())
            .into_iter()
            .find(|step| matches!(step, Step::CompactIdle { member } if member == "dev"))
            .expect("the threshold asks for a compaction");
        run_step(&mut store, &t, &step, "").unwrap();
        let pending = store.load(&t.id).unwrap().unwrap().member("dev").unwrap().context_compact.clone().expect("the compaction is recorded");
        assert_eq!((pending.action.as_str(), pending.task, pending.reason.as_str()), ("compact", 1, "threshold"));
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered, "the attempt stays open");
        // The confirmation arrives: the member is told to go on, the same attempt.
        run_step(&mut store, &saved, &Step::CheckCompact { member: "dev".into() }, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.member("dev").unwrap().context_compact.is_none());
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
        let text = crate::state::teams::doorbell::posted_text(&sent.borrow());
        assert!(text.contains("context was compacted") || text.contains("handoff brief"), "{text}");
        assert!(text.contains("Continue task #1"), "{text}");
        let kinds = event_kinds(&store, &t.id);
        assert!(kinds.contains(&"context_compact_started".to_owned()), "{kinds:?}");
        assert!(kinds.contains(&"context_compacted".to_owned()), "{kinds:?}");
        assert_eq!(*calls.borrow(), ["compact-context", "inspect", "send"]);
    });
}

/// Task 209: a report that claims there is no context left while the meter is
/// below the threshold is not a report: the member hears the figure and keeps
/// the attempt.
#[test]
fn a_false_context_claim_is_answered_with_the_meter_and_keeps_the_attempt() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        delivered(&mut t);
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("inspect") => Ok(fake_output(0, r#"{"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                "session_usage":{"context":{"used":206000,"limit":1000000}}}"#, "")),
            other => panic!("unexpected hgs {other:?}"),
        })));
        let attempt = t.task(1).unwrap().attempts.last().unwrap().id.clone();
        let answer = report_context_answer(
            &mut store,
            &t.id,
            &json!({"task":1,"attempt":attempt,"text":"I have no context budget left, stopping here."}),
        )
        .unwrap()
        .expect("the claim is answered");
        set_fake_hgs(None);
        assert!(answer.contains("20%") && answer.contains("206000 of 1000000"), "{answer}");
        assert!(answer.contains("below the team's 95% threshold"), "{answer}");
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().status, TaskStatus::InProgress, "no report was taken");
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
        assert_eq!(saved.member("dev").unwrap().context.unwrap().percent, 20);
        let kinds = event_kinds(&store, &t.id);
        assert!(kinds.contains(&"context_claim_refused".to_owned()), "{kinds:?}");
        // An ordinary report is a report.
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| panic!("no inspect for a plain report: {args:?}"))));
        assert!(report_context_answer(&mut store, &t.id, &json!({"task":1,"attempt":attempt,"text":"Done: the API is in place."}))
            .unwrap()
            .is_none());
        set_fake_hgs(None);
    });
}

/// Task 209: a claim at or above the threshold starts the compaction instead;
/// the report is still not taken.
#[test]
fn a_context_claim_at_the_threshold_compacts_instead_of_reporting() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        delivered(&mut t);
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("inspect") => Ok(fake_output(0, r#"{"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                "session_usage":{"context":{"used":970000,"limit":1000000}}}"#, "")),
            other => panic!("unexpected hgs {other:?}"),
        })));
        let attempt = t.task(1).unwrap().attempts.last().unwrap().id.clone();
        let answer = report_context_answer(&mut store, &t.id, &json!({"task":1,"attempt":attempt,"text":"Out of context."}))
            .unwrap()
            .expect("the claim is answered");
        set_fake_hgs(None);
        assert!(answer.contains("97%") && answer.contains("compacting"), "{answer}");
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().status, TaskStatus::InProgress);
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
        let kinds = event_kinds(&store, &t.id);
        assert!(kinds.contains(&"context_compact_requested".to_owned()), "{kinds:?}");
    });
}

/// Task 209: a No report reply that says there is no context left is answered
/// with the meter and the attempt stays open.
#[test]
fn a_context_claim_reply_is_answered_instead_of_no_report() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        delivered(&mut t);
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.context = Some(MemberContext::of(206_000, 1_000_000, crate::state::now()));
        t.task_mut(1).unwrap().attempts.last_mut().unwrap().last_reply = "I ran out of context and cannot continue.".into();
        stored(&mut store, &t);
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Value::Null));
        let seen = sent.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| match args.first().copied() {
            Some("send") => {
                *seen.borrow_mut() = input.cloned().unwrap_or_default();
                Ok(fake_output(0, r#"{"status":"submitted"}"#, ""))
            }
            other => panic!("unexpected hgs {other:?}"),
        })));
        run_step(&mut store, &t, &Step::AnswerContextClaim { task: 1 }, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        let attempt = saved.task(1).unwrap().attempts.last().unwrap();
        assert!(attempt.context_answered_at.is_some());
        assert!(!attempt.no_report, "not a No report");
        assert_eq!(attempt.state, AttemptState::Delivered);
        let text = crate::state::teams::doorbell::posted_text(&sent.borrow());
        assert!(text.contains("20%") && text.contains("Continue task #1"), "{text}");
        assert!(event_kinds(&store, &t.id).contains(&"context_claim_refused".to_owned()));
    });
}

/// Task 135/209: a fresh session with an open attempt gets the handoff brief
/// and continues the attempt.
#[test]
fn a_fresh_sessions_handoff_brief_continues_the_open_attempt() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        delivered(&mut t);
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.handoff = Some(crate::state::teams::model::HandoffNeed {
            why: "relaunch".into(),
            at: crate::state::now() - 60.0,
            from: "codex/x/dev".into(),
            changes: Vec::new(),
        });
        stored(&mut store, &t);
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Value::Null));
        let seen = sent.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| match args.first().copied() {
            Some("send") => {
                *seen.borrow_mut() = input.cloned().unwrap_or_default();
                Ok(fake_output(0, r#"{"status":"submitted"}"#, ""))
            }
            other => panic!("unexpected hgs {other:?}"),
        })));
        run_step(&mut store, &t, &Step::SendHandoff { member: "dev".into() }, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.member("dev").unwrap().handoff.is_none());
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().n, 1, "the same attempt");
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
        let text = crate::state::teams::doorbell::posted_text(&sent.borrow());
        assert!(text.contains("handoff brief") && text.contains("relaunched in a fresh conversation"), "{text}");
        assert!(text.contains("Continue task #1"), "{text}");
        assert!(event_kinds(&store, &t.id).contains(&"handoff_sent".to_owned()));
    });
}

/// Task #7: a member without an After-a-task policy keeps its context, even
/// with a full one; today's path (U46's own setting only) is the default.
#[test]
fn a_member_without_an_after_task_policy_keeps_its_context() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "none");
        with_usage("codex/x/dev", 950_000, 1_000_000);
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("send") => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
            other => panic!("no policy runs no context action: {other:?}"),
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        let attempt = saved.task(1).unwrap().attempts.last().unwrap();
        assert!(attempt.prepare.is_empty(), "{attempt:?}");
        assert_eq!(attempt.state, AttemptState::Delivered);
    });
}

/// Task #7: below its threshold the policy keeps the context and hands out.
#[test]
fn an_after_task_policy_keeps_the_context_below_its_threshold() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "none");
        with_after_task(&mut t, "dev", "compact", "percent", 80);
        with_usage("codex/x/dev", 700_000, 1_000_000);
        stored(&mut store, &t);
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| match args.first().copied() {
            Some("send") => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
            other => panic!("below the threshold no context action runs: {other:?}"),
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.task(1).unwrap().attempts.last().unwrap().prepare.is_empty());
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
    });
}

/// Task #7: at its percent threshold the policy compacts before the next task,
/// and the message waits for the confirmation.
#[test]
fn an_after_task_policy_compacts_before_the_next_task_at_its_threshold() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "none");
        with_after_task(&mut t, "dev", "compact", "percent", 80);
        with_usage("codex/x/dev", 850_000, 1_000_000);
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0));
        let (seen, seen_request, seen_inspects) = (calls.clone(), request.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                "inspect" => {
                    seen_inspects.set(seen_inspects.get() + 1);
                    let status = if seen_inspects.get() == 1 { "compacting" } else { "completed" };
                    Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                        "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                            "conversation_id":"c1","status":status}}).to_string(), ""))
                }
                "send" => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        assert_eq!(*calls.borrow(), ["compact-context"], "the action runs before the message");
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().waits.as_ref().unwrap().action, "compact");
        assert!(saved.task(1).unwrap().attempts.is_empty());
        assert!(prepare_tick(&mut store, &t.id));
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert!(saved.task(1).unwrap().waits.is_none());
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
        assert_eq!(*calls.borrow(), ["compact-context", "inspect", "inspect", "send"]);
    });
}

/// Task #7: a token threshold counts the tokens used, not the window's share.
#[test]
fn an_after_task_policy_counts_tokens_when_it_says_so() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        with_context(&mut t, "dev", "none");
        with_after_task(&mut t, "dev", "compact", "tokens", 200_000);
        // A quarter of the window, but past the token threshold.
        with_usage("codex/x/dev", 250_000, 1_000_000);
        stored(&mut store, &t);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0));
        let (seen, seen_request, seen_inspects) = (calls.clone(), request.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                "inspect" => {
                    seen_inspects.set(seen_inspects.get() + 1);
                    let status = if seen_inspects.get() == 1 { "compacting" } else { "completed" };
                    Ok(fake_output(0, &json!({"run_id":"r1","conversation_id":"c1","activity":"idle","phase":"idle",
                        "compact_context_request":{"request_id":seen_request.borrow().clone(),"run_id":"r1",
                            "conversation_id":"c1","status":status}}).to_string(), ""))
                }
                "send" => Ok(fake_output(0, r#"{"status":"submitted"}"#, "")),
                other => panic!("unexpected hgs {other}"),
            }
        })));
        hand_out(&mut store, &t.id, 1, "dev", None, "").unwrap();
        assert_eq!(*calls.borrow(), ["compact-context"], "the token criterion fires on the tokens used");
        assert!(prepare_tick(&mut store, &t.id));
        assert!(prepare_tick(&mut store, &t.id));
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(1).unwrap().attempts.last().unwrap().state, AttemptState::Delivered);
    });
}

/// Task #7: the lead's policy runs only while the lead is free; a busy lead
/// still gets its own task, without a message and without a context action.
#[test]
fn the_leads_after_task_policy_runs_only_while_the_lead_is_free() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.tasks.push(Task { id: 4, title: "Plan review".into(), assignee: "lead".into(), status: TaskStatus::Pending, ..Default::default() });
        t.tasks.push(Task { id: 5, title: "Plan review 2".into(), assignee: "lead".into(), status: TaskStatus::Pending, ..Default::default() });
        with_after_task(&mut t, "lead", "compact", "percent", 80);
        stored(&mut store, &t);
        // Busy: C1 hands the task out with no message and no context action.
        busy_record("claude/x/lead");
        set_fake_hgs(Some(Box::new(|_, args: &[&str], _| panic!("a busy lead runs no action: {args:?}"))));
        lead_hand_out(&mut store, &t.id, 4).unwrap();
        set_fake_hgs(None);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(4).unwrap().attempts.last().unwrap().member, "lead");
        // Free: the policy runs before the lead's next task.
        free_record("claude/x/lead", 900_000, 1_000_000);
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let request = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let (seen, seen_request) = (calls.clone(), request.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], input: Option<&Value>| {
            let call = args.first().copied().unwrap_or_default().to_owned();
            seen.borrow_mut().push(call.clone());
            match call.as_str() {
                "compact-context" => {
                    let id = input.unwrap()["request_id"].as_str().unwrap().to_owned();
                    *seen_request.borrow_mut() = id.clone();
                    Ok(fake_output(0, &json!({"request_id":id,"status":"submitted"}).to_string(), ""))
                }
                other => panic!("unexpected hgs {other}"),
            }
        })));
        lead_hand_out(&mut store, &t.id, 5).unwrap();
        set_fake_hgs(None);
        assert_eq!(*calls.borrow(), ["compact-context"]);
        let saved = store.load(&t.id).unwrap().unwrap();
        assert_eq!(saved.task(5).unwrap().waits.as_ref().unwrap().action, "compact");
    });
}

/// Task #2 (team ede724c4): a reading that only carries a new time is not
/// worth a write; one that moved the shown figure, crossed the threshold,
/// names another window or has grown old is.
#[test]
fn only_a_moved_context_reading_is_worth_a_write() {
    let stored = MemberContext::of(400_000, 1_000_000, 1000.0);
    let at = |used: u64, limit: u64, when: f64| MemberContext::of(used, limit, when);
    assert!(super::context_moved(None, &stored, 95), "the first reading is written");
    assert!(!super::context_moved(Some(&stored), &at(400_000, 1_000_000, 1030.0), 95), "same figure, new time");
    assert!(!super::context_moved(Some(&stored), &at(419_000, 1_000_000, 1030.0), 95), "one point is not a move");
    assert!(super::context_moved(Some(&stored), &at(420_000, 1_000_000, 1030.0), 95), "two points are");
    assert!(super::context_moved(Some(&stored), &at(380_000, 1_000_000, 1030.0), 95), "down two points too");
    assert!(super::context_moved(Some(&stored), &at(400_000, 2_000_000, 1030.0), 95), "another window");
    let below = MemberContext::of(940_000, 1_000_000, 1000.0);
    assert!(super::context_moved(Some(&below), &at(950_000, 1_000_000, 1030.0), 95), "crossing the threshold by one point");
    assert!(!super::context_moved(Some(&stored), &at(400_000, 1_000_000, 1000.0 + super::CONTEXT_REFRESH - 1.0), 95));
    assert!(super::context_moved(Some(&stored), &at(400_000, 1_000_000, 1000.0 + super::CONTEXT_REFRESH), 95), "an old reading is refreshed");
    // The refresh keeps the stored reading fresh for a claim between two reads.
    const { assert!(super::CONTEXT_REFRESH + super::CONTEXT_IDLE_EVERY < super::super::context::CLAIM_FRESH) };
}

/// Task #2 (team ede724c4): the tick's context sampling. An unchanged reading
/// writes nothing (no commit, the same revision) and is not read again before
/// the interval; a moved one is written once.
#[test]
fn an_unchanged_context_reading_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        // A unique team id: the worker's read times are kept per team and member.
        t.id = uuid::Uuid::new_v4().to_string();
        with_context(&mut t, "dev", "");
        stored(&mut store, &t);
        let used = std::rc::Rc::new(std::cell::Cell::new(400_000u64));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let (reading, count) = (used.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], _| {
            assert_eq!(args.first().copied(), Some("inspect"));
            count.set(count.get() + 1);
            let body = json!({"session_usage":{"context":{"used":reading.get(),"limit":1_000_000}}}).to_string();
            Ok(fake_output(0, &body, ""))
        })));
        let listed = vec![session("codex/x/dev", "r1", true)];
        let sample = |store: &mut Store, at: f64| {
            let teams = vec![store.load(&t.id).unwrap().unwrap()];
            super::super::store::take_lock_lines();
            super::sample_contexts(store, &teams, &listed, at);
            let commits = super::super::store::take_lock_lines().iter().filter(|line| line["outcome"] == "commit").count();
            (commits, store.load(&t.id).unwrap().unwrap())
        };
        let start = crate::state::now();
        let (commits, first) = sample(&mut store, start);
        assert_eq!((commits, inspects.get()), (1, 1), "the first reading is written");
        assert_eq!(first.member("dev").unwrap().context.unwrap().percent, 40);
        // The idle interval later the same figure is read and not written.
        let (commits, second) = sample(&mut store, start + super::CONTEXT_IDLE_EVERY + 1.0);
        assert_eq!((commits, inspects.get()), (0, 2), "an unchanged reading writes nothing");
        assert_eq!(second.revision, first.revision, "and leaves the revision alone");
        // The skipped write does not make the next tick read again.
        let (commits, _) = sample(&mut store, start + super::CONTEXT_IDLE_EVERY + 10.0);
        assert_eq!((commits, inspects.get()), (0, 2), "no read before the interval");
        // A moved figure is written once.
        used.set(450_000);
        let (commits, third) = sample(&mut store, start + 2.0 * super::CONTEXT_IDLE_EVERY + 2.0);
        assert_eq!((commits, inspects.get()), (1, 3));
        assert_eq!(third.member("dev").unwrap().context.unwrap().percent, 45);
        assert_eq!(third.revision, first.revision + 1);
        set_fake_hgs(None);
    });
}

/// Task #2 (team ede724c4): a busy member's last event time alone is
/// written at most every WATCH_EVENT_EVERY; a phase change at once.
#[test]
fn a_busy_members_event_time_alone_is_written_rarely() {
    let mut t = team();
    let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
    dev.state = MemberState::Active;
    dev.seen = Some(MemberSeen { at: 1000.0, live: true });
    dev.seen_phase = "working".into();
    dev.seen_activity = "busy".into();
    dev.seen_event_at = Some(1000.0);
    let busy = |event: f64, phase: &str| Seen { activity: "busy".into(), phase: phase.into(), last_event_at: Some(event), ..session("codex/x/dev", "r1", false) };
    let watched = |seen: Seen, now: f64| steps(&t, &[seen], now).iter().any(|step| matches!(step, Step::Watch { member, .. } if member == "dev"));
    assert!(!watched(busy(1090.0, "working"), 1090.0), "a minute and a half of events is not written");
    assert!(!watched(busy(1000.0 + super::WATCH_EVENT_EVERY - 1.0, "working"), 1300.0));
    assert!(watched(busy(1000.0 + super::WATCH_EVENT_EVERY, "working"), 1300.0), "the bound is");
    assert!(watched(busy(1010.0, "question"), 1010.0), "a phase change is written at once");
    // Task #6 of team c536f0e5 (P1.4): working and tool are one busy turn;
    // a flip between them waits for the event time's bound.
    assert!(!watched(busy(1010.0, "tool"), 1010.0), "a working/tool flip is not written");
    assert!(watched(busy(1000.0 + super::WATCH_EVENT_EVERY, "tool"), 1300.0), "it rides with the event time");
    assert!(watched(Seen { activity: "idle".into(), ..busy(1010.0, "idle") }, 1010.0), "a turn's end is written at once");
}

/// Task #2 (team ede724c4): the measurement for the report. A scripted hour of
/// 5-second ticks over three members with sessions: one working (an open
/// attempt; its window grows half a point a minute) and two idle (unchanged).
/// Before, every read was a write; now only a moved or old reading is.
#[test]
fn context_writes_per_minute_over_a_scripted_hour() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.id = uuid::Uuid::new_v4().to_string();
        with_context(&mut t, "dev", "");
        delivered(&mut t);
        let qa = t.members.iter_mut().find(|member| member.id == "qa").unwrap();
        qa.session = Some(SessionRef { name: "kimi/x/qa".into(), run_id: "r3".into(), conversation_id: "c3".into(), ..Default::default() });
        qa.state = MemberState::Active;
        stored(&mut store, &t);
        let start = 1_000_000.0;
        let clock = std::rc::Rc::new(std::cell::Cell::new(start));
        let inspects = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let (now, count) = (clock.clone(), inspects.clone());
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], _| {
            count.set(count.get() + 1);
            let minutes = (now.get() - start) / 60.0;
            // dev grows from 30% by half a point a minute; the others stay.
            let used = if args.get(1).copied() == Some("codex/x/dev") { 300_000 + (minutes * 5_000.0) as u64 } else { 200_000 };
            Ok(fake_output(0, &json!({"session_usage":{"context":{"used":used,"limit":1_000_000}}}).to_string(), ""))
        })));
        let listed = vec![session("claude/x/lead", "r0", true), session("codex/x/dev", "r1", false), session("kimi/x/qa", "r3", true)];
        super::super::store::take_lock_lines();
        let mut commits = 0;
        let ticks = 12 * 60;
        for tick in 0..ticks {
            let at = start + 5.0 * tick as f64;
            clock.set(at);
            let teams = vec![store.load(&t.id).unwrap().unwrap()];
            super::sample_contexts(&mut store, &teams, &listed, at);
            commits += super::super::store::take_lock_lines().iter().filter(|line| line["outcome"] == "commit").count();
        }
        set_fake_hgs(None);
        let (before, after) = (inspects.get() as f64 / 60.0, commits as f64 / 60.0);
        eprintln!("context writes per minute over a scripted hour: before {before:.2} (every read), after {after:.2}");
        assert!(after * 3.0 < before, "before {before:.2}, after {after:.2}");
        // The shown value stays current: never older than the refresh plus a read.
        let saved = store.load(&t.id).unwrap().unwrap();
        let end = start + 5.0 * (ticks - 1) as f64;
        for member in saved.members.iter().filter(|member| member.session.is_some()) {
            let context = member.context.unwrap_or_else(|| panic!("{} has a reading", member.id));
            assert!(end - context.at <= super::CONTEXT_REFRESH + super::CONTEXT_IDLE_EVERY, "{}: {}", member.id, end - context.at);
        }
        assert!(saved.member("dev").unwrap().context.unwrap().percent >= 58, "dev's figure follows its growth");
    });
}

/// Task #3 (team ede724c4): four members due for a context read. Each read is
/// an `hgs inspect` of up to 10 s; the tick starts them side by side on
/// threads of their own and returns at once, and the next tick's sampling
/// records what they read. Before, the tick ran them one after another.
#[test]
fn four_context_reads_run_side_by_side_and_the_tick_does_not_wait() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.id = uuid::Uuid::new_v4().to_string();
        with_context(&mut t, "dev", "");
        with_context(&mut t, "qa", "");
        let mut doc = t.member("dev").unwrap().clone();
        doc.id = "doc".into();
        doc.session = Some(SessionRef { name: "codex/x/doc".into(), run_id: "r1".into(), conversation_id: "c1".into(), ..Default::default() });
        t.members.push(doc);
        stored(&mut store, &t);
        let (running, most, reads) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (now_running, peak, count) = (running.clone(), most.clone(), reads.clone());
        super::set_context_reader(&t.id, Some(Arc::new(move |_member: &Member, at: f64| {
            let inside = now_running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(inside, Ordering::SeqCst);
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(400));
            now_running.fetch_sub(1, Ordering::SeqCst);
            Some(MemberContext::of(300_000, 1_000_000, at))
        })));
        let listed = vec![
            session("claude/x/lead", "r0", true),
            session("codex/x/dev", "r1", true),
            session("codex/x/qa", "r1", true),
            session("codex/x/doc", "r1", true),
        ];
        let teams = vec![store.load(&t.id).unwrap().unwrap()];
        let at = crate::state::now();
        let started = Instant::now();
        super::sample_contexts(&mut store, &teams, &listed, at);
        let took = started.elapsed();
        assert!(took < Duration::from_millis(400), "the tick waited {took:?} for the reads");
        // A sampling while they run starts no second read of the same member.
        super::sample_contexts(&mut store, &teams, &listed, at + 1.0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while ["lead", "dev", "qa", "doc"].iter().any(|member| super::context_in_flight(&t.id, member)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(reads.load(Ordering::SeqCst), 4, "one read per member");
        assert_eq!(most.load(Ordering::SeqCst), 4, "the four reads ran side by side, not in sequence");
        // Nothing was recorded by the tick that started them ...
        let before = store.load(&t.id).unwrap().unwrap();
        assert!(before.members.iter().all(|member| member.context.is_none()), "recorded only by a later sampling");
        // ... the next sampling records every reading and starts none again.
        super::sample_contexts(&mut store, &teams, &listed, at + 3.0);
        let after = store.load(&t.id).unwrap().unwrap();
        for id in ["lead", "dev", "qa", "doc"] {
            assert_eq!(after.member(id).unwrap().context.map(|context| context.percent), Some(30), "{id}");
        }
        assert_eq!(reads.load(Ordering::SeqCst), 4);
        super::set_context_reader(&t.id, None);
    });
}
