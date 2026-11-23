use super::*;
use crate::state::teams::model::{Attempt, AttemptState, Task};
use std::cell::RefCell;

/// The outside world a test decides, with a count of its reads.
#[derive(Default)]
struct Fake {
    ci: RefCell<Vec<(String, String)>>,
    installed: RefCell<Installed>,
    reads: RefCell<u32>,
}

impl Probes for Fake {
    fn ci(&self, _repo: &str, _run: u64) -> Result<(String, String), String> {
        *self.reads.borrow_mut() += 1;
        let mut answers = self.ci.borrow_mut();
        if answers.is_empty() { Err("unreachable".into()) } else { Ok(answers.remove(0)) }
    }
    fn build(&self, _program: &str) -> Result<Installed, String> {
        *self.reads.borrow_mut() += 1;
        Ok(self.installed.borrow().clone())
    }
}

fn parked(condition: ParkCondition) -> Park {
    Park { attempt: "a1".into(), member: "dev".into(), by: "dev".into(), at: 1_000.0, condition, eta: 2_800.0, state: WAITING.into(), ..Park::default() }
}

fn team() -> Team {
    let mut team = Team { id: "t".into(), status: TeamStatus::Running, ..Default::default() };
    team.tasks.push(Task { id: 1, title: "Install check".into(), assignee: "dev".into(), status: TaskStatus::Pending, ..Default::default() });
    team.tasks.push(Task { id: 2, title: "Release".into(), assignee: "qa".into(), status: TaskStatus::InProgress, ..Default::default() });
    team.proposals.push(json!({"id":"p1","kind":"question","state":"open"}));
    team
}

#[test]
fn conditions_read_and_say_what_they_wait_for() {
    let team = team();
    let read = |value: Value| condition_of(&team, 1, &value).map(|condition| label(&condition)).map_err(|error| error.to_string());
    assert_eq!(read(json!({"kind":"ci","repo":"WhiteWh/zerus","run":42})).unwrap(), "CI run 42 of WhiteWh/zerus finishing");
    assert_eq!(read(json!({"kind":"build","version":"1.58.0","build":12})).unwrap(), "hgs version 1.58.0, build 12 installed");
    assert_eq!(read(json!({"kind":"task","task":2})).unwrap(), "task #2 accepted");
    assert_eq!(read(json!({"kind":"card","proposal":"p1"})).unwrap(), "the owner's answer on card p1");
    for (value, said) in [
        (json!({"kind":"ci","repo":"a/b/c","run":1}), "OWNER/REPO"),
        (json!({"kind":"ci","repo":"a/b","run":0}), "run's number"),
        (json!({"kind":"build"}), "names a version"),
        (json!({"kind":"build","commit":"xyz"}), "hexadecimal"),
        (json!({"kind":"build","version":"1.0","program":"relative.exe"}), "absolute path"),
        (json!({"kind":"task","task":9}), "Unknown task #9"),
        (json!({"kind":"card","proposal":"p9"}), "Unknown card p9"),
        (json!({"kind":"time"}), "A park condition is one of"),
    ] {
        let error = read(value.clone()).unwrap_err();
        assert!(error.contains(said), "{value}: {error}");
    }
    let park = parked(ParkCondition::Task { task: 2 });
    assert!(summary(&park, 1_000.0).ends_with("(in 30 min)"), "{}", summary(&park, 1_000.0));
    assert!(summary(&park, 4_600.0).ends_with("(30 min late)"), "{}", summary(&park, 4_600.0));
}

#[test]
fn task_card_and_time_are_read_from_the_team_on_every_pass() {
    let mut team = team();
    let fake = Fake::default();
    let task = parked(ParkCondition::Task { task: 2 });
    assert_eq!(evaluate(&team, &task, 2_000.0, 0.0, &fake), None);
    team.task_mut(2).unwrap().status = TaskStatus::Accepted;
    assert_eq!(evaluate(&team, &task, 2_000.0, 0.0, &fake), Some(Outcome::Met("task #2 was accepted".into())));
    team.task_mut(2).unwrap().status = TaskStatus::Cancelled;
    assert_eq!(evaluate(&team, &task, 2_000.0, 0.0, &fake), Some(Outcome::Failed("task #2 was cancelled".into())));
    let card = parked(ParkCondition::Card { proposal: "p1".into() });
    assert_eq!(evaluate(&team, &card, 2_000.0, 0.0, &fake), None);
    team.proposals[0]["state"] = json!("rejected");
    team.proposals[0]["note"] = json!("not this week");
    assert_eq!(evaluate(&team, &card, 2_000.0, 0.0, &fake), Some(Outcome::Failed("the owner denied card p1: not this week".into())));
    team.proposals[0]["state"] = json!("accepted");
    assert!(matches!(evaluate(&team, &card, 2_000.0, 0.0, &fake), Some(Outcome::Met(_))));
    let time = parked(ParkCondition::Time { at: 3_000.0 });
    assert_eq!(evaluate(&team, &time, 2_999.0, 0.0, &fake), None);
    assert!(matches!(evaluate(&team, &time, 3_000.0, 0.0, &fake), Some(Outcome::Met(_))));
    assert_eq!(*fake.reads.borrow(), 0, "none of these reads the outside world");
}

#[test]
fn ci_and_builds_are_read_outside_at_bounded_intervals() {
    let team = team();
    let fake = Fake::default();
    let ci = parked(ParkCondition::Ci { repo: "WhiteWh/zerus".into(), run: 42 });
    fake.ci.borrow_mut().extend([("in_progress".to_owned(), String::new()), ("completed".to_owned(), "failure".to_owned())]);
    assert_eq!(evaluate(&team, &ci, 2_000.0, 0.0, &fake), None);
    // Within two minutes of the last read nothing is read.
    assert_eq!(evaluate(&team, &ci, 2_100.0, 2_000.0, &fake), None);
    assert_eq!(*fake.reads.borrow(), 1);
    assert_eq!(evaluate(&team, &ci, 2_120.0, 2_000.0, &fake), Some(Outcome::Failed("CI run 42 of WhiteWh/zerus finished: failure".into())));
    fake.ci.borrow_mut().push(("completed".into(), "success".into()));
    assert_eq!(evaluate(&team, &ci, 2_300.0, 2_120.0, &fake), Some(Outcome::Met("CI run 42 succeeded".into())));
    // An unreachable API is no outcome; the next read tries again.
    assert_eq!(evaluate(&team, &ci, 2_500.0, 2_300.0, &fake), None);
    let build = parked(ParkCondition::Build { version: "1.58.0".into(), build: 7, commit: String::new(), program: String::new() });
    *fake.installed.borrow_mut() = Installed { version: "1.57.2".into(), build: 9, commit: String::new() };
    assert_eq!(evaluate(&team, &build, 3_000.0, 0.0, &fake), None);
    assert_eq!(evaluate(&team, &build, 3_030.0, 3_000.0, &fake), None, "a minute between build reads");
    *fake.installed.borrow_mut() = Installed { version: "1.58.0-dev.7".into(), build: 7, commit: String::new() };
    assert_eq!(evaluate(&team, &build, 3_060.0, 3_000.0, &fake), Some(Outcome::Met("version 1.58.0-dev.7, build 7 is installed".into())));
    let commit = parked(ParkCondition::Build { version: String::new(), build: 0, commit: "abc1234".into(), program: "D:/apps/Zerus.exe".into() });
    *fake.installed.borrow_mut() = Installed { version: "0.49.0".into(), build: 0, commit: "abc1234def".into() };
    assert!(matches!(evaluate(&team, &commit, 4_000.0, 0.0, &fake), Some(Outcome::Met(_))));
}

#[test]
fn versions_and_build_info_are_read_as_installed() {
    assert!(at_least("1.58.0", "1.58.0") && at_least("1.58.1", "1.58") && at_least("2.0", "1.99.9") && at_least("1.58.0-dev.3", "1.58.0"));
    assert!(!at_least("1.57.9", "1.58.0") && !at_least("", "0.1"));
    assert_eq!(parse_installed("hgs 1.57.2\n"), Installed { version: "1.57.2".into(), build: 0, commit: String::new() });
    assert_eq!(parse_installed("hgs 1.58.0-dev.41"), Installed { version: "1.58.0-dev.41".into(), build: 41, commit: String::new() });
    assert_eq!(
        parse_installed(r#"{"version":"0.49.0","versionName":"0.49.0-dev.12","devBuildNumber":12,"sourceCommit":"abc1234"}"#),
        Installed { version: "0.49.0".into(), build: 12, commit: "abc1234".into() }
    );
}

#[test]
fn the_watcher_records_the_outcome_once_and_only_then_writes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_at(&dir.path().join("teams.sqlite3"), true).unwrap();
    let mut team = team();
    {
        let task = team.task_mut(1).unwrap();
        task.attempts.push(Attempt { n: 1, id: "a1".into(), member: "dev".into(), state: AttemptState::Parked, ..Default::default() });
        task.park = Some(parked(ParkCondition::Task { task: 2 }));
    }
    store.create("11111111-1111-4111-8111-111111111111", &json!({}), team.clone(), vec![]).unwrap();
    let fake = Fake::default();
    let mut reads = HashMap::new();
    let revision = store.load("t").unwrap().unwrap().revision;
    assert_eq!(watch(&mut store, std::slice::from_ref(&team), 2_000.0, &fake, &mut reads).unwrap(), 0);
    assert_eq!(store.load("t").unwrap().unwrap().revision, revision, "nothing held: nothing written");
    team.task_mut(2).unwrap().status = TaskStatus::Accepted;
    store.mutate(None, "t", None, |stored, _| {
        stored.task_mut(2).unwrap().status = TaskStatus::Accepted;
        Ok(Value::Null)
    }).unwrap();
    assert_eq!(watch(&mut store, std::slice::from_ref(&team), 2_030.0, &fake, &mut reads).unwrap(), 1);
    let saved = store.load("t").unwrap().unwrap();
    let park = saved.task(1).unwrap().park.clone().unwrap();
    assert_eq!((park.state.as_str(), park.detail.as_str()), (MET, "task #2 was accepted"));
    let feed = store.events("t", None, 20).unwrap().to_string();
    assert!(feed.contains("park_met"), "{feed}");
    // A met park is not checked again.
    assert_eq!(watch(&mut store, std::slice::from_ref(&saved), 2_060.0, &fake, &mut reads).unwrap(), 0);
}

/// Task #23 of team c536f0e5 (owner, 2026-11-23): a park on the member's own
/// run. It waits while the run is queued or claimed, is met when its result is
/// in the member's outbox (with the exit code; a failed exit is still met, so
/// the member reads it) and fails when the request is gone without a result.
#[test]
fn a_park_on_its_own_run_comes_back_with_the_exit_code() {
    use crate::state::teams::model::Member;
    let dir = tempfile::tempdir().unwrap();
    let mut team = team();
    team.members.push(Member { id: "dev".into(), directory: dir.path().to_string_lossy().into_owned(), ..Default::default() });
    let request = "11111111-1111-4111-8111-111111111111";
    let read = |value: Value| condition_of(&team, 1, &value).map(|condition| label(&condition)).map_err(|error| error.to_string());
    assert_eq!(read(json!({"kind":"run","request":request})).unwrap(), format!("its run {request} finishing"));
    assert!(read(json!({"kind":"run","request":"cargo-test"})).unwrap_err().contains("request_id of your hgs team run"));
    assert!(read(json!({"kind":"later"})).unwrap_err().contains("{\"kind\":\"run\",\"request\":\"RUN_REQUEST_ID\"}"), "the error lists it");
    let fake = Fake::default();
    let park = parked(ParkCondition::Run { request: request.into() });
    let folder = dir.path();
    // Queued, then claimed by its runner: waiting.
    super::super::outbox::push(folder, &json!({"op":"run","request_id":request,"member":"dev","attempt":"a1","template":"cargo-test"})).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), None);
    super::super::outbox::write_running(folder, request, &json!({"request_id":request,"member":"dev","attempt":"a1"})).unwrap();
    std::fs::remove_file(super::super::outbox::request_path(folder, request)).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), None);
    // Finished: met with its exit code, a failed one too.
    super::super::outbox::settle_running(folder, request, &json!({"status":"ran","request_id":request,"exit":101,"timed_out":false})).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), Some(Outcome::Met(format!("run {request} finished with exit code 101"))));
    super::super::outbox::settle_running(folder, request, &json!({"status":"ran","request_id":request,"exit":0,"timed_out":true})).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), Some(Outcome::Met(format!("run {request} finished with exit code 0 (timed out)"))));
    super::super::outbox::settle_running(folder, request, &json!({"status":"refused","request_id":request,"error":"the template is not allowed"})).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), Some(Outcome::Met(format!("run {request} ended without running: the template is not allowed"))));
    // Gone without a result: failed, for the lead.
    std::fs::remove_file(super::super::outbox::result_path(folder, request)).unwrap();
    assert_eq!(evaluate(&team, &park, 2_000.0, 0.0, &fake), Some(Outcome::Failed(format!("run {request} is gone from the member's outbox without a result"))));
    assert_eq!(*fake.reads.borrow(), 0, "no outside probe");
    // The resume message carries the exit code.
    let mut met = park.clone();
    met.detail = format!("run {request} finished with exit code 101");
    assert!(resume_text(1, "Install check", &met).contains("exit code 101"));
}
