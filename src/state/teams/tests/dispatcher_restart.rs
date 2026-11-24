use super::*;
use super::support::*;

#[test]
fn a_restart_launches_the_same_conversation_and_returns_the_attempt_unconfirmed() {
    use crate::state::teams::model::{Attempt, AttemptState, TaskStatus};
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.settings.restart_crashed = true;
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().seen = Some(MemberSeen { at: 1.0, live: false });
        let task = t.tasks.iter_mut().find(|task| task.assignee == "dev").unwrap();
        let task_id = task.id;
        task.attempts.push(Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Delivered, ..Default::default() });
        task.status = TaskStatus::InProgress;
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), t.clone(), vec![]).unwrap();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Vec<String>>::new()));
        let record = calls.clone();
        set_fake_hgs(Some(Box::new(move |_, args: &[&str], _: Option<&Value>| {
            record.borrow_mut().push(args.iter().map(|arg| arg.to_string()).collect());
            Ok(fake_output(0, "{}", ""))
        })));
        run_step(&mut store, &t, &Step::Restart { member: "dev".into() }, "").unwrap();
        run_step(&mut store, &t, &Step::CrashStopped { member: "dev".into() }, "").unwrap();
        set_fake_hgs(None);
        let after = store.load(&t.id).unwrap().unwrap();
        let dev = after.members.iter().find(|member| member.id == "dev").unwrap();
        assert_eq!(dev.restarts.len(), 1);
        assert!(dev.crash.is_some() && dev.crash_told);
        let attempt = after.task(task_id).unwrap().open_attempt().unwrap();
        assert_eq!(attempt.state, AttemptState::Unconfirmed);
        assert!(attempt.error.contains("crashed"), "{}", attempt.error);
        assert_eq!(
            calls.borrow().first().map(Vec::as_slice),
            Some(["resume".to_owned(), "codex/x/dev".to_owned(), "-d".to_owned()].as_slice())
        );
        let feed = store.events(&t.id, None, 50).unwrap().to_string();
        assert!(feed.contains("member_restarted") && feed.contains("member_crashed"), "{feed}");
    });
}

#[test]
fn a_crashed_member_is_restarted_only_when_the_team_allows_it() {
    let mut t = team();
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().seen = Some(MemberSeen { at: 1.0, live: true });
    let crashed = stopped("codex/x/dev", "r1", "stopped");
    // Off: only the liveness record changes.
    t.settings.restart_crashed = false;
    let found = steps(&t, std::slice::from_ref(&crashed), 20.0);
    assert!(found.contains(&Step::Liveness { member: "dev".into(), live: false }), "{found:?}");
    assert!(!found.iter().any(|step| matches!(step, Step::Restart { .. } | Step::CrashStopped { .. })), "{found:?}");
    // On: the same tick asks for the restart.
    t.settings.restart_crashed = true;
    let found = steps(&t, std::slice::from_ref(&crashed), 20.0);
    assert!(found.contains(&Step::Restart { member: "dev".into() }), "{found:?}");
    // Three restarts in the hour: the session stays stopped, once.
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().restarts = vec![10.0, 15.0, 18.0];
    let found = steps(&t, std::slice::from_ref(&crashed), 20.0);
    assert!(found.contains(&Step::CrashStopped { member: "dev".into() }), "{found:?}");
    assert!(!found.iter().any(|step| matches!(step, Step::Restart { .. })));
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().crash_told = true;
    assert!(!steps(&t, std::slice::from_ref(&crashed), 20.0).iter().any(|step| matches!(step, Step::CrashStopped { .. })));
    // An hour later the old restarts no longer count.
    let found = steps(&t, std::slice::from_ref(&crashed), 4000.0);
    assert!(found.contains(&Step::Restart { member: "dev".into() }), "{found:?}");
}

/// The owner, 2026-11-05: three Kimi agents exited while their terminals
/// stayed open, and the team treated them as live. An exited agent is a
/// stopped session; with restart_crashed on it is restarted, the lead too.
#[test]
fn an_exited_agent_with_an_open_terminal_is_stopped_and_restarted_even_the_lead() {
    let listing = json!([{"sessions":[{"name":"kimi/x/lead","state":"running","runtime_state":"live","process_state":"exited",
        "activity":"idle","phase":"idle"}]}]);
    let found = seen(&listing);
    assert_eq!((found[0].live, found[0].free, found[0].state.as_str()), (false, false, "stopped"));
    // A paused session stays paused.
    let paused = seen(&json!([{"sessions":[{"name":"kimi/x/lead","state":"paused","process_state":"exited"}]}]));
    assert_eq!(paused[0].state, "paused");
    let mut t = team();
    t.settings.restart_crashed = true;
    t.members.iter_mut().find(|member| member.id == "lead").unwrap().seen = Some(MemberSeen { at: 1.0, live: true });
    let found = steps(&t, &[stopped("claude/x/lead", "r0", "stopped")], 20.0);
    assert!(found.contains(&Step::Restart { member: "lead".into() }), "{found:?}");
}

/// A Kimi request that waited for the Terminal and can be answered again goes
/// back to the lead, and is never the lead's and the owner's at once.
#[test]
fn a_terminal_request_that_can_be_answered_again_goes_back_to_the_lead() {
    use crate::state::teams::model::PendingQuestion;
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        {
            let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
            dev.asked.push("k1:hk".into());
            dev.terminal_question = Some(PendingQuestion { question_id: "k1".into(), question_hash: "hk".into(), approval: true, at: 1.0,
                card: json!({"can_answer":false}) });
        }
        store.create("11111111-1111-4111-8111-111111111112", &json!({}), t.clone(), vec![]).unwrap();
        let answerable = json!({"pending_questions":[{"question_id":"k1","question_hash":"hk","approval":true,"can_answer":true,
            "questions":[{"id":"approval","header":"Bash","question":"Run this command?","options":[]}]}]}).to_string();
        set_fake_hgs(Some(Box::new(move |_, _: &[&str], _: Option<&Value>| Ok(fake_output(0, &answerable, "")))));
        sync_permission_forget(&t.id, "dev");
        run_step(&mut store, &t, &Step::Permission { member: "dev".into() }, "").unwrap();
        set_fake_hgs(None);
        let after = store.load(&t.id).unwrap().unwrap();
        let dev = after.members.iter().find(|member| member.id == "dev").unwrap();
        assert_eq!(dev.pending.as_ref().map(|pending| pending.question_id.as_str()), Some("k1"));
        assert!(dev.terminal_question.is_none(), "{:?}", dev.terminal_question);
    });
}

#[test]
fn a_paused_archived_limited_or_unseen_session_is_never_restarted() {
    let mut t = team();
    t.settings.restart_crashed = true;
    for (state, limited) in [("paused", false), ("archived", false), ("stopped", true)] {
        {
            let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
            dev.seen = Some(MemberSeen { at: 1.0, live: true });
            dev.limited = None;
            dev.state = MemberState::Active;
            dev.restarts.clear();
            dev.crash_told = false;
        }
        let mut crashed = stopped("codex/x/dev", "r1", state);
        if limited {
            crashed.limited = Some(SeenLimit { at: 1.0, resets_at: Some(99.0), detail: "usage limit".into() });
        }
        let found = steps(&t, &[crashed], 20.0);
        assert!(!found.iter().any(|step| matches!(step, Step::Restart { .. } | Step::CrashStopped { .. })), "{state}: {found:?}");
    }
    // A member the team paused, or one a limit stopped, is not restarted.
    for (state, limited) in [(MemberState::Paused, None), (MemberState::Active, Some(MemberLimit { at: 1.0, resets_at: Some(99.0), detail: "limit".into() }))] {
        {
            let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
            dev.seen = Some(MemberSeen { at: 1.0, live: true });
            dev.state = state;
            dev.limited = limited;
        }
        let found = steps(&t, &[stopped("codex/x/dev", "r1", "stopped")], 20.0);
        assert!(!found.iter().any(|step| matches!(step, Step::Restart { .. })), "{state:?}: {found:?}");
    }
    // A session not seen live since the last restart does not loop.
    {
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.state = MemberState::Active;
        dev.limited = None;
        dev.seen = Some(MemberSeen { at: 1.0, live: false });
    }
    assert!(!steps(&t, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).iter().any(|step| matches!(step, Step::Restart { .. })));
}

/// Task #334 (owner, 2026-11-08): installing a package restarted the agents
/// while their members were paused; after the resume no tick had seen those
/// sessions live, so nothing launched them and the lead sent relaunch cards.
/// A member that holds or was handed a task is launched again all the same.
#[test]
fn an_exited_member_that_holds_a_task_is_launched_again_without_a_live_sighting() {
    let mut t = team();
    t.settings.restart_crashed = true;
    delivered(&mut t);
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().seen = Some(MemberSeen { at: 1.0, live: false });
    let found = steps(&t, &[stopped("codex/x/dev", "r1", "stopped")], 20.0);
    assert!(found.contains(&Step::Restart { member: "dev".into() }), "{found:?}");
    // A returned stage keeps the member working just as a hand-out does.
    let mut staged = t.clone();
    staged.tasks.iter_mut().for_each(|task| task.attempts.clear());
    let task = staged.task_mut(2).unwrap();
    task.status = TaskStatus::InReview;
    task.pipeline_run = Some(PipelineRun {
        id: "code".into(),
        name: "Code: review, test, lead".into(),
        max_rounds: 3,
        round: 1,
        folder: "hold".into(),
        accept: "lead".into(),
        runs: vec![StageRun { id: "stage-run-1".into(), stage: 0, kind: "review".into(), round: 1, attempt: 1, member: "dev".into(), state: StageState::Delivered, ..Default::default() }],
        ..Default::default()
    });
    assert!(steps(&staged, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).contains(&Step::Restart { member: "dev".into() }));
    // With nothing in its hands the member still needs a live sighting, so a
    // launch that never came up does not loop.
    let mut idle = t.clone();
    idle.tasks.iter_mut().for_each(|task| task.attempts.clear());
    assert!(!steps(&idle, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).iter().any(|step| matches!(step, Step::Restart { .. })));
    // The setting off holds the exit for the lead, as before.
    let mut off = t.clone();
    off.settings.restart_crashed = false;
    assert!(!steps(&off, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).iter().any(|step| matches!(step, Step::Restart { .. })));
}

/// Task #334: the launch of a member that lost work marks its attempt
/// unconfirmed for the crash recovery and says in the feed why it happened.
#[test]
fn the_restart_of_a_member_that_lost_work_says_why_in_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.settings.restart_crashed = true;
        delivered(&mut t);
        t.members.iter_mut().find(|member| member.id == "dev").unwrap().seen = Some(MemberSeen { at: 1.0, live: false });
        store.create("11111111-1111-4111-8111-111111111113", &json!({}), t.clone(), vec![]).unwrap();
        set_fake_hgs(Some(Box::new(|_: &str, _: &[&str], _: Option<&Value>| Ok(fake_output(0, "{}", "")))));
        run_step(&mut store, &t, &Step::Restart { member: "dev".into() }, "").unwrap();
        set_fake_hgs(None);
        let feed = store.events(&t.id, None, 50).unwrap().to_string();
        assert!(feed.contains("member_restarted") && feed.contains("exited with a task"), "{feed}");
        let after = store.load(&t.id).unwrap().unwrap();
        let dev = after.members.iter().find(|member| member.id == "dev").unwrap();
        assert_eq!(dev.restarts.len(), 1);
        assert!(!dev.relaunch_exited);
        let attempt = after.task(1).unwrap().open_attempt().unwrap();
        assert_eq!(attempt.state, AttemptState::Unconfirmed);
        assert!(attempt.error.contains("crashed"), "{}", attempt.error);
        // The gave-back attempt is not work for a second launch: a relaunch
        // that never comes up does not loop (the next live sighting ends it).
        assert!(!steps(&after, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).iter().any(|step| matches!(step, Step::Restart { .. })));
    });
}

/// Task #334: the owner or the lead resuming a member whose agent exited
/// launches the same conversation again on the next tick - without a card -
/// and a pause withdraws that request.
#[test]
fn resuming_a_member_whose_agent_exited_launches_it_again() {
    use super::super::ops::{self, Actor};
    let mut t = team();
    t.settings.restart_crashed = true;
    {
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.state = MemberState::Paused;
        dev.seen = Some(MemberSeen { at: 1.0, live: false });
    }
    let mut events = Vec::new();
    let answer = ops::apply(&mut t, "resume-member", &json!({"member":"dev"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    assert_eq!(answer["relaunch"], json!(true), "{answer}");
    assert_eq!(t.member("dev").unwrap().state, MemberState::Active);
    assert!(t.member("dev").unwrap().relaunch_exited);
    let found = steps(&t, &[stopped("codex/x/dev", "r1", "stopped")], 20.0);
    assert!(found.contains(&Step::Restart { member: "dev".into() }), "{found:?}");
    // A pause takes the request back.
    let paused = ops::apply(&mut t, "pause-member", &json!({"member":"dev"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    assert_eq!(paused["relaunch"], json!(false));
    assert!(!t.member("dev").unwrap().relaunch_exited);
    // A member whose session was live at the last look is left to the ordinary
    // crash path: resuming it asks for nothing extra.
    {
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.state = MemberState::Paused;
        dev.seen = Some(MemberSeen { at: 1.0, live: true });
    }
    let running = ops::apply(&mut t, "resume-member", &json!({"member":"dev"}), &Actor::Human, &BTreeSet::new(), &mut events).unwrap();
    assert_eq!(running["relaunch"], json!(false), "{running}");
    assert!(!t.member("dev").unwrap().relaunch_exited);
}

/// Task #334: the hourly restart bound holds for a resumed member too; the
/// lead hears once and the request is spent instead of looping.
#[test]
fn a_resumed_member_at_the_hourly_bound_stays_stopped_and_the_lead_hears_once() {
    use super::super::ops::{self, Actor};
    let dir = tempfile::tempdir().unwrap();
    crate::state::with_test_state_root(dir.path(), || {
        let mut store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let mut t = team();
        t.settings.restart_crashed = true;
        {
            let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
            dev.state = MemberState::Paused;
            dev.seen = Some(MemberSeen { at: 1.0, live: false });
            dev.restarts = vec![10.0, 15.0, 18.0];
        }
        store.create("11111111-1111-4111-8111-111111111114", &json!({}), t.clone(), vec![]).unwrap();
        // The resume itself is the stored change, as the CLI applies it.
        store
            .mutate(None, &t.id, None, |team, events| {
                ops::apply(team, "resume-member", &json!({"member":"dev"}), &Actor::Human, &BTreeSet::new(), events)
            })
            .unwrap();
        let t = store.load(&t.id).unwrap().unwrap();
        assert!(t.member("dev").unwrap().relaunch_exited);
        let found = steps(&t, &[stopped("codex/x/dev", "r1", "stopped")], 20.0);
        assert!(!found.iter().any(|step| matches!(step, Step::Restart { .. })), "{found:?}");
        assert!(found.contains(&Step::CrashStopped { member: "dev".into() }), "{found:?}");
        run_step(&mut store, &t, &Step::CrashStopped { member: "dev".into() }, "").unwrap();
        let after = store.load(&t.id).unwrap().unwrap();
        let dev = after.members.iter().find(|member| member.id == "dev").unwrap();
        assert!(dev.crash_told && !dev.relaunch_exited);
        // The request is spent: no second card item from the same resume.
        assert!(!steps(&after, &[stopped("codex/x/dev", "r1", "stopped")], 20.0).iter().any(|step| matches!(step, Step::Restart { .. } | Step::CrashStopped { .. })));
    });
}

#[test]
fn a_member_prompt_snapshot_wins_over_the_live_catalog() {
    let mut t = team();
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().prompt = "Backend prompt".into();
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().prompt_snapshot = Some(crate::state::teams::model::PromptSnapshot {
        prompt_order: vec!["protocol".into()],
        protocol_extra: "SNAPSHOT EXTRA".into(),
        ..Default::default()
    });
    let task = t.task(1).unwrap().clone();
    let attempt = Attempt { n: 1, id: "a1".into(), member: "dev".into(), ..Default::default() };
    let text = hand_off_text(&t, &task, &attempt, "", "");
    assert!(text.contains("SNAPSHOT EXTRA"), "{text}");
    // An old member without a snapshot keeps today's live catalog text.
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().prompt_snapshot = None;
    let text = hand_off_text(&t, &task, &attempt, "", "");
    assert!(!text.contains("SNAPSHOT EXTRA"), "{text}");
}

#[test]
fn a_stage_hand_off_names_the_change_the_folder_and_the_verdict() {
    // U36/#536: a review or test hand-off says what to read, where, what a
    // verdict contains and that a stage never edits the task's files.
    let mut t = team();
    {
        let dev = t.members.iter_mut().find(|member| member.id == "dev").unwrap();
        dev.directory = "/work/checkout-dev".into();
        dev.folder = MemberFolder::Worktree;
    }
    {
        let task = t.tasks.iter_mut().find(|task| task.id == 1).unwrap();
        task.description = "Build the cards API".into();
        task.criteria = vec!["Paged list".into(), "No N+1".into()];
        let mut attempt = Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Reported, ..Default::default() };
        attempt.commits.push(CommitRef { sha: "abc1234".into(), files: 2, ..Default::default() });
        attempt.report = Some(Report { text: "Changed src/cards.rs and src/db.rs".into(), at: 1.0, ..Default::default() });
        task.attempts.push(attempt);
        task.pipeline_run = Some(PipelineRun {
            id: "code".into(),
            name: "Code: review, test, lead".into(),
            max_rounds: 3,
            round: 1,
            folder: "hold".into(),
            accept: "lead".into(),
            ..Default::default()
        });
    }
    let task = t.task(1).unwrap().clone();
    let review = StageRun {
        id: "stage-run-1".into(),
        stage: 0,
        kind: "review".into(),
        round: 1,
        attempt: 1,
        member: "qa".into(),
        state: StageState::Delivered,
        ..Default::default()
    };
    let text = stage_hand_off_text(&t, &task, &review);
    for part in [
        "from: Lead",
        "task: #1",
        "stage: review",
        "round 1 of 3",
        "Your role: Tester",
        "Build the cards API",
        "Acceptance criteria:",
        "Paged list",
        "No N+1",
        "Changed src/cards.rs and src/db.rs",
        "/work/checkout-dev",
        "abc1234",
        "git show",
        "review stage",
        "Change nothing in the task's files",
        "\"verdict\":\"pass|return\"",
        "\"attempt\":\"stage-run-1\"",
        "return sends your findings to the implementer",
        "hgs team report --json",
        "hgs team ask --json",
        "hgs team current --json",
    ] {
        assert!(text.contains(part), "{part} missing in\n{text}");
    }
    assert!(!text.contains("shared folder"), "{text}");
    // A test stage says what to run and that the folder is held.
    let test = StageRun { kind: "test".into(), ..review.clone() };
    let text = stage_hand_off_text(&t, &task, &test);
    for part in [
        "stage: test",
        "only the suites of the files",
        "take the criteria's shots",
        "folder is held",
        "Judge each acceptance criterion on its own line",
        "deletes, skips or narrows a test",
    ] {
        assert!(text.contains(part), "{part} missing in\n{text}");
    }
    // A shared folder says which files are the task's, and no commits names
    // the report's changed-file list instead.
    t.members.iter_mut().find(|member| member.id == "dev").unwrap().folder = MemberFolder::Shared;
    let text = stage_hand_off_text(&t, &t.task(1).unwrap().clone(), &review);
    assert!(text.contains("shared folder") && text.contains("only the files the report names"), "{text}");
    let mut without = t.clone();
    without.tasks.iter_mut().find(|task| task.id == 1).unwrap().attempts[0].commits.clear();
    let text = stage_hand_off_text(&without, &without.task(1).unwrap().clone(), &review);
    assert!(text.contains("No commits were recorded"), "{text}");
}

/// #728 (live, 2026-10-31): frontend waited in one long Bash call polling
/// its own test run and was marked stuck. A member inside a running tool
/// call is working until the much longer tool bound, counted from the
/// call's start; a tool phase with no running call has the usual bound.
#[test]
fn a_member_waiting_in_its_own_running_tool_call_is_not_stuck() {
    let t = team();
    let at = 100_000.0;
    let lead = session("claude/x/lead", "r0", true);
    let mut dev = session("codex/x/dev", "r1", false);
    dev.activity = "busy".into();
    dev.phase = "tool".into();
    dev.turn_started = Some(at - 50.0 * 60.0);
    dev.tool = "Bash".into();
    dev.tool_started = Some(at - 40.0 * 60.0);
    dev.last_event_at = Some(at - 40.0 * 60.0);
    let marked = |seen: &Seen| steps(&t, &[lead.clone(), seen.clone()], at).into_iter().find_map(|step| match step {
        Step::MarkStuck { since, .. } => Some(since),
        _ => None,
    });
    assert_eq!(marked(&dev), None, "40 min in one running Bash call is work");
    assert_eq!(tool_stuck_minutes(&t), 60.0);
    // Past the tool bound it is stuck, since the call's start.
    let mut long = dev.clone();
    long.tool_started = Some(at - 61.0 * 60.0);
    long.last_event_at = Some(at - 61.0 * 60.0);
    assert_eq!(marked(&long), Some(at - 61.0 * 60.0));
    // A tool phase with no running call: the usual 10-minute bound.
    let mut idle_tool = dev.clone();
    idle_tool.tool.clear();
    idle_tool.tool_started = None;
    assert_eq!(marked(&idle_tool), Some(at - 40.0 * 60.0));
    // A tool started before this turn is not this turn's running call.
    let mut old = dev.clone();
    old.tool_started = Some(at - 55.0 * 60.0);
    assert!(marked(&old).is_some());
}
