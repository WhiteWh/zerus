//! A member bound to a launch resolves through its durable handle, its
//! session's own ID (zerus-63rru.53), and no longer through the launch's
//! evidence, which goes when the first run ends and after a week. Records are
//! the supervisor's own construction, saved, resumed, renamed, started fresh
//! and archived by the product's functions, and fleet rows are made by the
//! listing's own steps, in a child test process whose state root is private.
use super::super::availability::{current, Current, Unresolved};
use super::super::native::Native;
use super::production::fleet::session_of;
use super::real_rows::{isolated, Production};
use crate::state::listing_fixture as producer;
use crate::teams::ops::tests::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zerus_team_plan::model::{Member, Team};

const CHILD: &str = "desktop::teams::actor::tests::handles::";
const OTHER_LAUNCH: &str = "00000000-0000-4000-8000-0000000000e1";
const OTHER_SESSION: &str = "00000000-0000-4000-8000-0000000000e2";

struct World {
    h: Harness,
    native: Native,
    records: BTreeMap<String, Value>,
}

fn name(member: &str) -> String {
    format!("claude/fixture/{member}")
}
fn run(member: &str) -> String {
    format!("fixture-{member}")
}
fn launch_of(h: &Harness, member: &str) -> String {
    let m = h.store.member(TEAM, member).unwrap().unwrap();
    m.session.unwrap().session_id["launch_id:".len()..].to_owned()
}

/// A team whose `members` are bound to their launches. Each has the record
/// its supervisor writes, saved by the record writer at its label's path,
/// its launch's evidence as the launch writes it, and its idle fleet row.
fn world(members: &[&str]) -> World {
    let mut h = Harness::new();
    // The state root of the records is the team's own.
    std::env::set_var("HGS_STATE_DIR", h.temp.path());
    for m in members {
        h.bind(m);
    }
    let native = super::production::native(&h);
    let mut records = BTreeMap::new();
    for member in members {
        let m = h.store.member(TEAM, member).unwrap().unwrap();
        let record = adopt(&h, &m, &m.worktree);
        records.insert((*member).to_owned(), record);
    }
    World { h, native, records }
}

/// Replaces the fixture's seeded record and evidence of `m` by the ones the
/// product writes, its session working in `cwd`.
fn adopt(h: &Harness, m: &Member, cwd: &str) -> Value {
    let state = h.temp.path();
    let sid = session_of(&m.id);
    let launch = launch_of(h, &m.id);
    std::fs::remove_file(state.join("sessions").join(format!("{sid}.json"))).unwrap();
    std::fs::remove_file(state.join("launches").join(format!("{launch}.json"))).unwrap();
    let mut record = producer::launched_record(
        &m.agent,
        &name(&m.id),
        &launch,
        &sid,
        &run(&m.id),
        cwd,
        &m.account,
    );
    producer::confirmed(&mut record, &format!("conversation-{}", m.id));
    producer::save(&mut record).unwrap();
    producer::launch_started(&launch, &name(&m.id), &sid).unwrap();
    record
}

fn evidence(w: &World, launch: &str) -> std::path::PathBuf {
    w.h.temp
        .path()
        .join("launches")
        .join(format!("{launch}.json"))
}

/// Commits the idle row the listing makes for `record` under its run.
fn row_for(w: &World, record: &Value) {
    let row = producer::listed_live_row(record.clone()).unwrap();
    let key = format!("run:{}", record["run_id"].as_str().unwrap());
    let json = row.to_string();
    rusqlite::Connection::open(w.native.config.state.join("desktop/state.sqlite3"))
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(1,?1,?2,?3)",
            rusqlite::params![key, json, json.len()],
        )
        .unwrap();
}

fn team(w: &World) -> Team {
    w.h.store.team(TEAM).unwrap().unwrap()
}
fn member(w: &World, id: &str) -> Member {
    w.h.store.member(TEAM, id).unwrap().unwrap()
}
/// The production free check (it records a missing handle once verified).
fn free(w: &mut World, id: &str) -> bool {
    let (t, m) = (team(w), member(w, id));
    super::super::ports::Ports::free(&mut w.native, &t, &m).unwrap()
}
fn now(w: &World, id: &str) -> Current {
    current(&w.native.config, TEAM, &member(w, id), false).unwrap()
}

/// One actor tick over one new task: what the production ports delivered.
fn hand_out(w: World) -> Vec<String> {
    let mut h = w.h;
    h.add(&person(), 1);
    let machine = h.store.team(TEAM).unwrap().unwrap().home;
    let mut a = super::super::Runtime::open(h.temp.path(), &machine).unwrap();
    let mut p = Production {
        native: w.native,
        deliveries: vec![],
    };
    a.tick(&mut h.store, &mut p, TEAM, 100, true).unwrap();
    p.deliveries
}
fn handed_out(w: World) {
    let deliveries = hand_out(w);
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert!(
        deliveries[0].starts_with("m1 -> Outcome(Submitted"),
        "{deliveries:?}"
    );
}

/// The first free read records the handle while the evidence still exists.
fn bound(w: &mut World) {
    assert!(free(w, "m1"));
    let handle = w.h.store.member_handle(TEAM, "m1").unwrap().unwrap();
    assert_eq!(handle.session_id, session_of("m1"));
    assert_eq!(handle.label_hint, name("m1"));
}

/// The first run ends with its conversation confirmed: its supervisor
/// removes the launch's evidence.
fn first_run_ends(w: &World) {
    let launch = launch_of(&w.h, "m1");
    producer::launch_finished(&launch, &name("m1"), &run("m1"));
    assert!(!evidence(w, &launch).exists(), "the evidence stayed");
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn resumed_after_its_first_run_child() {
    let mut w = world(&["m1"]);
    bound(&mut w);
    first_run_ends(&w);
    // A person resumes it: another run of the same session.
    let mut resumed = producer::resumed_record(&w.records["m1"], "run-resumed");
    producer::confirmed(&mut resumed, "conversation-m1");
    producer::save(&mut resumed).unwrap();
    row_for(&w, &resumed);
    assert_eq!(now(&w, "m1"), Current::Run("run-resumed".into()));
    handed_out(w);
}
#[test]
fn a_member_resumed_after_its_first_run_ended_is_free_again() {
    isolated(&format!("{CHILD}resumed_after_its_first_run_child"));
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn eight_days_after_its_launch_child() {
    let mut w = world(&["m1"]);
    bound(&mut w);
    let launch = launch_of(&w.h, "m1");
    producer::age_launch(&launch, 8);
    // A later launch prunes evidence older than a week.
    producer::launch_started(OTHER_LAUNCH, "claude/fixture/other", OTHER_SESSION).unwrap();
    assert!(!evidence(&w, &launch).exists(), "the old evidence stayed");
    assert_eq!(now(&w, "m1"), Current::Run(run("m1")));
    handed_out(w);
}
#[test]
fn a_member_eight_days_after_its_launch_is_free() {
    isolated(&format!("{CHILD}eight_days_after_its_launch_child"));
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn renamed_child() {
    let mut w = world(&["m1"]);
    bound(&mut w);
    first_run_ends(&w);
    producer::rename(&name("m1"), "claude/fixture/m1-renamed").unwrap();
    let mut renamed = w.records["m1"].clone();
    renamed["name"] = json!("claude/fixture/m1-renamed");
    row_for(&w, &renamed);
    assert_eq!(now(&w, "m1"), Current::Run(run("m1")));
    handed_out(w);
}
#[test]
fn a_renamed_member_is_free_under_its_new_name() {
    isolated(&format!("{CHILD}renamed_child"));
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn archived_child() {
    let mut w = world(&["m1"]);
    bound(&mut w);
    producer::archive(&w.records["m1"]).unwrap();
    assert_eq!(now(&w, "m1"), Current::Unresolved(Unresolved::Gone));
    assert!(!free(&mut w, "m1"));
    assert_eq!(hand_out(w), Vec::<String>::new());
}
#[test]
fn an_archived_member_is_not_free_and_is_named_gone() {
    isolated(&format!("{CHILD}archived_child"));
}

#[test]
fn only_the_member_with_a_native_archive_gets_a_catalog_card_that_closes_on_repair() {
    isolated(&format!("{CHILD}archive_catalog_card_child"));
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn archive_catalog_card_child() {
    use super::super::ports::{Availability, Fact, Ports};
    let mut w = world(&["m1", "m2"]);
    for id in ["m1", "m2"] {
        assert!(free(&mut w, id));
    }
    let forgotten = producer::archive(&w.records["m2"]).unwrap();
    let id = producer::archive_with_catalog_gap(&w.records["m1"]).unwrap();
    let state = w.h.temp.path().to_owned();
    let db = state.join("archive-catalog-v1.sqlite3");
    std::fs::remove_file(state.join(format!("archives/{forgotten}.json"))).unwrap();
    let indexed = || {
        rusqlite::Connection::open(&db).unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM archive_identities WHERE id=?)",
            [&id],
            |row| row.get::<_, bool>(0),
        ).unwrap()
    };
    assert!(!indexed(), "the native writer rolled back its catalog update");
    let admitted = state.join("archive-catalog-v2/teams-attempt.json");
    let t = team(&w);
    let (m1, m2) = (member(&w, "m1"), member(&w, "m2"));
    assert!(!free(&mut w, "m1"));
    assert!(admitted.is_file(), "the member reader requested bounded repair");
    assert!(state.join(format!("archives/{id}.json")).is_file());
    let first = std::fs::read(&admitted).unwrap();
    let child = |test: &str| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{CHILD}{test}"),
                "--ignored",
                "--test-threads=1",
            ])
            .spawn()
            .unwrap()
    };
    assert!(child("confirmation_repair_child").wait().unwrap().success());
    assert_eq!(
        std::fs::read(&admitted).unwrap(),
        first,
        "another reader process shares the reservation for a different member"
    );
    let gone = Availability::Unreadable(Fact::Gone);
    assert_eq!(w.native.availability(&t, &m2).unwrap(), gone);
    let other_files: Vec<_> = (0..65)
        .map(|n| state.join(format!("archives/unrelated-{n}")))
        .collect();
    for path in &other_files {
        std::fs::write(path, "").unwrap();
    }
    assert_eq!(
        w.native.availability(&t, &m2).unwrap(),
        Availability::Unreadable(Fact::Unavailable),
        "exhausting the disk inspection budget cannot decide Gone"
    );
    for path in other_files {
        std::fs::remove_file(path).unwrap();
    }

    w.h.add(&person(), 1);
    let mut actor = super::super::Runtime::open(&state, &t.home).unwrap();
    let mut p = Production {
        native: w.native,
        deliveries: vec![],
    };
    let t0 = 10_000_000;
    for minute in 0..=3 {
        actor
            .tick(&mut w.h.store, &mut p, TEAM, t0 + minute * 60_000, false)
            .unwrap();
    }
    let cards = |h: &Harness| h.store.cards(TEAM, 200, None).unwrap().rows;
    let card = cards(&w.h)
        .into_iter()
        .find(|card| {
            card.state == "open"
                && card.text.starts_with("Member m1 ")
                && card.text.contains("an archive from its session is missing from the archive catalog")
        })
        .expect("the durable archive has a catalog card after three minutes");
    assert!(cards(&w.h).iter().all(|row| !row.text.starts_with("Member m2 ")
        || !row.text.contains("missing from the archive catalog")));

    let writer = std::fs::File::options()
        .read(true)
        .write(true)
        .open(state.join(".lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&writer).unwrap();
    let error = crate::state::archive_catalog::team_worker_with_budget(
        std::time::Duration::from_millis(25),
    )
    .unwrap_err();
    assert!(
        error.starts_with(crate::state::archive_catalog::INCOMPLETE),
        "{error}"
    );
    assert!(!indexed(), "the worker must wait for the state writer");
    drop(writer);
    let status = crate::state::dispatch_test_archive_catalog_worker().unwrap();
    assert_eq!(status, 0);
    assert_eq!(p.native.availability(&t, &m1).unwrap(), gone);
    actor
        .tick(&mut w.h.store, &mut p, TEAM, t0 + 4 * 60_000, false)
        .unwrap();
    assert_ne!(
        w.h.store.card(TEAM, card.seq).unwrap().unwrap().state,
        "open"
    );
    assert!(p.deliveries.is_empty());
}

#[test]
#[ignore = "run by the archive-card fixture in its original state root"]
fn confirmation_repair_child() {
    let state = std::path::PathBuf::from(std::env::var_os("HGS_STATE_DIR").unwrap());
    let missing =
        crate::state::archive_catalog::find_run(&state, &name("m2"), &run("m2")).unwrap_err();
    assert!(
        missing.starts_with(crate::state::archive_catalog::INCOMPLETE),
        "{missing}"
    );
}

#[test]
#[ignore = "run by its isolated wrapper"]
fn started_fresh_child() {
    let mut w = world(&["m1"]);
    bound(&mut w);
    first_run_ends(&w);
    let mut fresh = producer::started_fresh_record(&w.records["m1"], "run-fresh");
    producer::save(&mut fresh).unwrap();
    row_for(&w, &fresh);
    assert_eq!(now(&w, "m1"), Current::Unresolved(Unresolved::StartedFresh));
    // And once its new conversation started.
    producer::confirmed(&mut fresh, "conversation-fresh");
    fresh.as_object_mut().unwrap().remove("fresh_from");
    fresh["session_clear"] = json!({"source":"fresh_start","run_id":"run-fresh",
        "conversation_id":"conversation-fresh","previous_conversation_id":"conversation-m1"});
    producer::save(&mut fresh).unwrap();
    assert_eq!(now(&w, "m1"), Current::Unresolved(Unresolved::StartedFresh));
    assert!(!free(&mut w, "m1"));
    assert_eq!(hand_out(w), Vec::<String>::new());
}
#[test]
fn a_member_started_fresh_is_not_free_and_is_named() {
    isolated(&format!("{CHILD}started_fresh_child"));
}

/// A member bound before handles: none recorded, the evidence gone. The
/// launch ledger's observation at binding gives the handle; a read never
/// writes it, the free check does, once.
#[test]
#[ignore = "run by its isolated wrapper"]
fn backfilled_from_the_ledger_child() {
    use crate::desktop::teams::ledger::{Ledger, Row, State};
    use crate::desktop::teams::ports::{ObservedSession, Worktree};
    let mut w = world(&["m1"]);
    first_run_ends(&w);
    assert_eq!(now(&w, "m1"), Current::Unresolved(Unresolved::NoHandle));
    assert!(!free(&mut w, "m1"));
    let t = team(&w);
    let m = member(&w, "m1");
    let launch = launch_of(&w.h, "m1");
    let observed = |session: &str| ObservedSession {
        machine: t.home.clone(),
        launch_id: launch.clone(),
        agent: m.agent.clone(),
        account: m.account.clone(),
        worktree: Worktree {
            path: m.worktree.clone(),
            ownership_nonce: m.ownership_nonce.clone(),
        },
        parent: None,
        name: name("m1"),
        session_id: session.into(),
        run_id: run("m1"),
        conversation_id: "conversation-m1".into(),
    };
    // A ledger value no session carries is never recorded.
    let mut row = Row::new(&t, "m1", "epoch", State::Done);
    row.observed = Some(observed(OTHER_SESSION));
    let mut ledger = Ledger::open(w.h.temp.path()).unwrap();
    assert!(ledger.insert(&row, 1).unwrap());
    assert_eq!(now(&w, "m1"), Current::Unresolved(Unresolved::Gone));
    assert!(!free(&mut w, "m1"));
    assert_eq!(w.h.store.member_handle(TEAM, "m1").unwrap(), None);
    // The value the launcher saw at binding.
    let mut row = ledger.get(TEAM, "m1").unwrap().unwrap();
    row.observed = Some(observed(&session_of("m1")));
    ledger.save(&mut row, 2).unwrap();
    drop(ledger);
    assert_eq!(now(&w, "m1"), Current::Run(run("m1")));
    assert_eq!(w.h.store.member_handle(TEAM, "m1").unwrap(), None);
    assert!(free(&mut w, "m1"));
    let handle = w.h.store.member_handle(TEAM, "m1").unwrap().unwrap();
    assert_eq!(handle.session_id, session_of("m1"));
    handed_out(w);
}
#[test]
fn a_member_bound_before_handles_takes_its_handle_from_the_launch_ledger() {
    isolated(&format!("{CHILD}backfilled_from_the_ledger_child"));
}

/// An owned linked worktree for `member` on its own branch, with the
/// ownership receipts the launcher's check reads.
fn owned(
    w: &World,
    root: &std::path::Path,
    empty: &std::path::Path,
    member: &str,
) -> (String, String) {
    use super::production::git;
    let base = root.parent().unwrap();
    let wt = base.join(format!("worktree-{member}"));
    let branch = format!("zerus/team/{member}");
    git(
        root,
        empty,
        &["worktree", "add", "-q", "-b", &branch, wt.to_str().unwrap()],
    );
    let metadata = std::path::PathBuf::from(git(&wt, empty, &["rev-parse", "--absolute-git-dir"]));
    let common = root.join(".git");
    let nonce = uuid::Uuid::new_v4().to_string();
    let owner = json!({"version":1,"id":nonce,"common_dir":common,"path":wt,"metadata":metadata,"branch":branch,"head":"fixture"});
    let receipt = crate::state::team_worktree_receipt_path(w.h.temp.path(), &common, &wt);
    std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    std::fs::write(&receipt, owner.to_string()).unwrap();
    std::fs::write(metadata.join("zerus-owner.json"), owner.to_string()).unwrap();
    (wt.to_str().unwrap().to_owned(), branch)
}

/// The parent link reads the member and the lead through their handles: a
/// lead whose first run ended has no evidence left and still links.
#[test]
#[ignore = "run by its isolated wrapper"]
fn parent_after_the_leads_first_run_child() {
    use super::production::git;
    use crate::desktop::teams::ledger::{Ledger, Row, State};
    use crate::desktop::teams::ports::{Placement, Plan};
    let w = world(&["lead", "m1"]);
    let base = crate::platform::canonical(w.h.temp.path()).unwrap();
    let root = base.join("repo");
    std::fs::create_dir(&root).unwrap();
    let empty = base.join("empty-config");
    std::fs::write(&empty, "").unwrap();
    git(&root, &empty, &["init", "-q", "--initial-branch", "main"]);
    git(&root, &empty, &["config", "user.name", "Teams Fixture"]);
    git(
        &root,
        &empty,
        &["config", "user.email", "fixture@example.test"],
    );
    git(&root, &empty, &["config", "commit.gpgsign", "false"]);
    git(&root, &empty, &["commit", "--allow-empty", "-qm", "base"]);
    let t = team(&w);
    let mut plans = BTreeMap::new();
    for id in ["lead", "m1"] {
        let m = member(&w, id);
        let (wt, branch) = owned(&w, &root, &empty, id);
        // The session works in its owned worktree.
        let sid = session_of(id);
        let launch = launch_of(&w.h, id);
        std::fs::remove_file(
            w.h.temp
                .path()
                .join("launches")
                .join(format!("{launch}.json")),
        )
        .unwrap();
        let mut record = producer::launched_record(
            &m.agent,
            &name(id),
            &launch,
            &sid,
            &run(id),
            &wt,
            &m.account,
        );
        producer::confirmed(&mut record, &format!("conversation-{id}"));
        producer::save(&mut record).unwrap();
        producer::launch_started(&launch, &name(id), &sid).unwrap();
        // The launcher records the handle at binding.
        let mut h_store = crate::teams::store::Store::open(w.h.temp.path()).unwrap();
        let rev = h_store.team(TEAM).unwrap().unwrap().revision;
        crate::teams::ops::apply(
            &mut h_store,
            &service(),
            &crate::teams::ops::Request {
                request_id: uuid::Uuid::new_v4().to_string(),
                team: TEAM.into(),
                expected_revision: rev,
                operation: crate::teams::ops::Operation::SetMemberHandle {
                    member: id.into(),
                    session_id: sid.clone(),
                    label_hint: name(id),
                },
            },
            1,
        )
        .unwrap();
        let plan = Plan {
            id: uuid::Uuid::new_v4().to_string(),
            team: TEAM.into(),
            person: t.owner.clone(),
            machine: t.home.clone(),
            project: t.project.clone(),
            member: id.into(),
            role: m.role.clone(),
            agent: m.agent.clone(),
            account: m.account.clone(),
            model: String::new(),
            effort: String::new(),
            base_commit: t.base_commit.clone(),
            placement: Placement {
                source: root.to_str().unwrap().into(),
                git_dir: root.join(".git").to_str().unwrap().into(),
                branch,
                destination: wt,
            },
            parent: (id == "m1").then(|| member(&w, "lead").session.unwrap()),
            launch_id: launch,
            session_name: String::new(),
            member_generation: None,
        };
        let mut row = Row::new(&t, id, "epoch", State::Done);
        row.plan = Some(plan.clone());
        assert!(Ledger::open(w.h.temp.path())
            .unwrap()
            .insert(&row, 1)
            .unwrap());
        plans.insert(id, plan);
    }
    // The lead's first run ends: its evidence goes.
    let lead_launch = launch_of(&w.h, "lead");
    producer::launch_finished(&lead_launch, &name("lead"), &run("lead"));
    assert!(!evidence(&w, &lead_launch).exists());
    let parent = |w: &World| {
        use crate::desktop::teams::ports::{Effects, ObservedSession, Worktree};
        let mut launcher = crate::desktop::teams::native::Native {
            config: w.native.config.clone(),
            machine: w.native.machine.clone(),
            stop: w.native.stop.clone(),
        };
        // `parent` reads both sessions itself; the observation it is given
        // at binding is not used.
        let unused = ObservedSession {
            machine: String::new(),
            launch_id: String::new(),
            agent: String::new(),
            account: String::new(),
            worktree: Worktree {
                path: String::new(),
                ownership_nonce: String::new(),
            },
            parent: None,
            name: String::new(),
            session_id: String::new(),
            run_id: String::new(),
            conversation_id: String::new(),
        };
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| {
            *s.borrow_mut() = Some(
                json!({"parent":{"session_id":"existing"}})
                    .to_string()
                    .into_bytes(),
            )
        });
        let result = launcher.parent(&plans["m1"], &unused, "original");
        crate::desktop::teams::child::FIXTURE_EXIT.with(|s| s.borrow_mut().take());
        result
    };
    assert_eq!(parent(&w), "existing_link_kept");
    // Without the lead's handle only the evidence remains, and it is gone.
    rusqlite::Connection::open(w.h.temp.path().join("teams/teams-v1.sqlite3"))
        .unwrap()
        .execute("DELETE FROM member_handles WHERE member='lead'", [])
        .unwrap();
    assert_eq!(parent(&w), "identity_pending");
}
#[test]
fn the_parent_link_reaches_a_lead_whose_first_run_ended() {
    isolated(&format!("{CHILD}parent_after_the_leads_first_run_child"));
}
