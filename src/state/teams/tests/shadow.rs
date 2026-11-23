use super::*;
use crate::state::teams::model::{Attempt, AttemptState, Park, ParkCondition, Report, TaskStatus};

/// dev holds #1 (open attempt a1); #2 is dev's but not handed; #3 is qa's.
fn team() -> Team {
    let mut team = Team { id: "t".into(), name: "Core".into(), lead: "lead".into(), status: TeamStatus::Running, ..Default::default() };
    for id in ["lead", "dev", "qa"] {
        team.members.push(Member { id: id.into(), role: id.into(), state: MemberState::Active, ..Default::default() });
    }
    team.tasks.push(Task { id: 1, title: "API".into(), assignee: "dev".into(), status: TaskStatus::InProgress,
        attempts: vec![Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Delivered, sent_at: 100.0, ..Default::default() }],
        ..Default::default() });
    team.tasks.push(Task { id: 2, title: "Docs".into(), assignee: "dev".into(), status: TaskStatus::Pending, ..Default::default() });
    team.tasks.push(Task { id: 145, title: "Merge".into(), assignee: "qa".into(), status: TaskStatus::Pending, ..Default::default() });
    team
}

#[test]
fn a_text_names_only_the_teams_tasks_in_their_usual_forms() {
    let team = team();
    assert_eq!(task_ids(&team, "Lead, for 145: build and measure it"), [145]);
    assert_eq!(task_ids(&team, "About #1 and Task #2, also task 145."), [1, 2, 145]);
    assert!(task_ids(&team, "wait 2 minutes, retry 145 times, version 1.2, #12a, #999, for 1 hour").is_empty());
    assert!(task_ids(&team, "Standup in 5").is_empty());
}

#[test]
fn a_lead_message_binds_to_the_handed_attempt_or_is_refused() {
    let team = team();
    let dev = team.member("dev").unwrap();
    let qa = team.member("qa").unwrap();
    let a1 = Binding { team: "t".into(), task: 1, attempt: "a1".into(), member: "dev".into() };
    assert_eq!(check_lead_message(&team, dev, "On #1: keep the old field", None).unwrap(), LeadCheck { bindings: vec![a1.clone()], references: vec![] });
    assert_eq!(check_lead_message(&team, qa, "Please check the logs and reply", None).unwrap(), LeadCheck::default(), "free-form: nothing to bind");
    // Task #30 (team ede724c4): a message whose "task" names a task the
    // member does not hold is refused, naming the hand-out.
    let error = check_lead_message(&team, qa, "Build, test and measure it", Some(145)).unwrap_err().to_string();
    assert!(error.starts_with("Task #145 is not handed to qa: it has no open attempt"), "{error}");
    assert!(error.contains("hgs team retry --json") && error.contains("hgs team reassign --json"), "{error}");
    let error = check_lead_message(&team, qa, "please review", Some(1)).unwrap_err().to_string();
    assert!(error.contains("it is handed to dev") && error.contains("hgs team reassign --json"), "{error}");
    assert!(check_lead_message(&team, qa, "x", Some(77)).unwrap_err().to_string().contains("Unknown task #77"));
    // Its own task in "task": bound, and the tasks the text mentions besides
    // are references.
    assert_eq!(check_lead_message(&team, dev, "Like #145 did, and #2 later", Some(1)).unwrap(),
        LeadCheck { bindings: vec![a1.clone()], references: vec![145, 2] });
    assert_eq!(reference_note(&[145, 2]).unwrap(), "#145, #2 are mentioned for reference only: the member does not hold them, so the message carries no work for them.");
    assert_eq!(reference_note(&[]), None);
}

/// Task #30 (team ede724c4, 2026-11-10): the lead's message to dev3 about its
/// task #3 was refused because it mentioned #1 for context. A mention of an
/// accepted task, another member's or a not handed one is a reference: the
/// message goes, bound to the member's own task when the text names it.
#[test]
fn a_lead_message_about_its_own_task_may_mention_any_task() {
    let mut team = team();
    team.tasks.push(Task { id: 9, title: "Done before".into(), assignee: "qa".into(), status: TaskStatus::Accepted, ..Default::default() });
    let dev = team.member("dev").unwrap();
    let qa = team.member("qa").unwrap();
    let a1 = Binding { team: "t".into(), task: 1, attempt: "a1".into(), member: "dev".into() };
    // Its own #1 with #9 (accepted), #145 (qa's) and #2 (dev's, not handed) for context.
    assert_eq!(check_lead_message(&team, dev, "On #1: reuse what #9 did; #145 and #2 will follow", None).unwrap(),
        LeadCheck { bindings: vec![a1], references: vec![9, 145, 2] });
    // A mention alone never refuses: it binds nothing and names the references.
    assert_eq!(check_lead_message(&team, qa, "Lead, for 145: see #1 first", None).unwrap(), LeadCheck { bindings: vec![], references: vec![145, 1] });
    // The binding is kept on the attempt, bounded, with an event.
    let mut stored = team.clone();
    let mut events = Vec::new();
    for n in 0..25 {
        attach(&mut stored, &[Binding { team: "t".into(), task: 1, attempt: "a1".into(), member: "dev".into() }], &format!("note {n}"), "hgs send", 1.0, &mut events);
    }
    let kept = &stored.task(1).unwrap().attempts[0].lead_messages;
    assert_eq!(kept.len(), 20);
    assert_eq!(kept.last().unwrap()["text"], "note 24");
    assert_eq!(events[0].kind, "lead_message");
}

#[test]
fn a_member_job_needs_its_handed_work() {
    let team = team();
    let job = job_of(&team, "dev", "").unwrap();
    assert_eq!((job.task, job.attempt.as_str()), (1, "a1"));
    assert_eq!(job_of(&team, "dev", "a1").unwrap().task, 1);
    let error = job_of(&team, "dev", "other").unwrap_err().to_string();
    assert!(error.contains("Attempt other is not handed to you; your work is #1 (attempt a1)"), "{error}");
    let error = job_of(&team, "qa", "").unwrap_err().to_string();
    assert!(error.starts_with("No task is handed to you in team Core: ask the lead"), "{error}");
}

fn seen(free: bool, building: bool, turn_started: Option<f64>) -> super::super::dispatcher::Seen {
    super::super::dispatcher::Seen { name: "codex/x/qa".into(), free, live: true, building, build_label: if building { "cargo test".into() } else { String::new() },
        turn_started, activity: if free { "idle".into() } else { "busy".into() }, ..Default::default() }
}

#[test]
fn work_with_nothing_handed_is_unassigned_after_the_grace_and_a_build_at_once() {
    let mut team = team();
    let qa = team.member("qa").unwrap().clone();
    assert_eq!(unassigned(&team, &qa, &seen(false, true, None), 1_000.0, Vec::new).as_deref(), Some("builds cargo test"));
    assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(950.0)), 1_000.0, Vec::new), None, "within the grace");
    assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(800.0)), 1_000.0, Vec::new).as_deref(), Some("works (busy) since 3 min"));
    assert_eq!(unassigned(&team, &qa, &seen(true, false, None), 1_000.0, Vec::new), None, "an idle member is no work");
    // The rest of the turn its report ended is not unassigned.
    team.task_mut(145).unwrap().attempts.push(Attempt { n: 1, id: "q1".into(), member: "qa".into(), state: AttemptState::Reported,
        sent_at: 500.0, report: Some(Report { text: "done".into(), at: 900.0, ..Default::default() }), ..Default::default() });
    assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(800.0)), 1_000.0, Vec::new), None);
    // Handed work, or the lead, is never flagged.
    let dev = team.member("dev").unwrap().clone();
    assert_eq!(unassigned(&team, &dev, &seen(false, true, Some(0.0)), 1_000.0, Vec::new), None);
    let lead = team.member("lead").unwrap().clone();
    assert_eq!(unassigned(&team, &lead, &seen(false, true, Some(0.0)), 1_000.0, Vec::new), None);
}

/// Task #23 of team c536f0e5 (owner, 2026-11-23): sync was flagged about ten
/// times while it only waited for its own queued cargo-test of a parked task.
/// A run bound to the member's open or parked attempt, queued or running, is
/// that task's work, and so is the rest of the turn in which it parked an
/// attempt; work outside both is still flagged.
#[test]
fn its_own_run_of_a_parked_attempt_and_the_parking_turn_are_owned_work() {
    let mut team = team();
    team.task_mut(145).unwrap().attempts.push(Attempt { n: 1, id: "q1".into(), member: "qa".into(), state: AttemptState::Parked, sent_at: 100.0, ..Default::default() });
    team.task_mut(145).unwrap().park = Some(Park { attempt: "q1".into(), member: "qa".into(), by: "qa".into(), at: 300.0,
        condition: ParkCondition::Run { request: "r1".into() }, state: "waiting".into(), ..Default::default() });
    let qa = team.member("qa").unwrap().clone();
    let run = |attempt: &str, running: bool| OwnRun { request: "r1".into(), attempt: attempt.into(), running, label: "cargo-test".into() };
    // Its queued, or running, cargo-test of the parked attempt: not flagged.
    for running in [false, true] {
        assert_eq!(unassigned(&team, &qa, &seen(false, true, Some(500.0)), 1_000.0, || vec![run("q1", running)]), None, "running: {running}");
        assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(500.0)), 1_000.0, || vec![run("q1", running)]), None);
    }
    assert_eq!(run_task(&team, "qa", &run("q1", false)), Some(145));
    // The turn in which it parked: owned, building or working past the grace.
    assert_eq!(unassigned(&team, &qa, &seen(false, true, Some(250.0)), 1_000.0, Vec::new), None);
    assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(250.0)), 1_000.0, Vec::new), None);
    // Genuine unassigned work is still flagged: a later turn with no run of its
    // own, a run bound to another member's attempt or to no attempt.
    assert_eq!(unassigned(&team, &qa, &seen(false, true, Some(500.0)), 1_000.0, Vec::new).as_deref(), Some("builds cargo test"));
    assert_eq!(unassigned(&team, &qa, &seen(false, true, Some(500.0)), 1_000.0, || vec![run("a1", true)]).as_deref(), Some("builds cargo test"));
    assert_eq!(unassigned(&team, &qa, &seen(false, false, Some(500.0)), 1_000.0, || vec![run("", false)]).as_deref(), Some("works (busy) since 8 min"));
    // A run of an attempt that ended is no longer owned.
    team.task_mut(145).unwrap().attempts[0].state = AttemptState::Revoked;
    assert_eq!(run_task(&team, "qa", &run("q1", false)), None);
}

/// Task #23: the runs a member queued and runs, read from its outbox; another
/// member's in the same folder are not its own.
#[test]
fn own_runs_are_read_from_the_members_outbox() {
    let dir = tempfile::tempdir().unwrap();
    let queued = "11111111-1111-4111-8111-111111111111";
    let claimed = "22222222-2222-4222-8222-222222222222";
    let other = "33333333-3333-4333-8333-333333333333";
    super::super::outbox::push(dir.path(), &json!({"op":"run","request_id":queued,"member":"qa","attempt":"q1","template":"cargo-test"})).unwrap();
    super::super::outbox::push(dir.path(), &json!({"op":"run","request_id":other,"member":"dev","attempt":"a1","template":"cargo-build"})).unwrap();
    super::super::outbox::write_running(dir.path(), claimed, &json!({"request_id":claimed,"member":"qa","attempt":"q1","label":"cargo-clippy"})).unwrap();
    let mut runs = own_runs(dir.path(), "qa");
    runs.sort_by(|a, b| a.request.cmp(&b.request));
    assert_eq!(runs, vec![
        OwnRun { request: queued.into(), attempt: "q1".into(), running: false, label: "cargo-test".into() },
        OwnRun { request: claimed.into(), attempt: "q1".into(), running: true, label: "cargo-clippy".into() },
    ]);
    assert!(own_runs(&dir.path().join("missing"), "qa").is_empty());
}
