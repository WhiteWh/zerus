//! zerus-63rru.55: a member that a ready task needs and that cannot be read
//! gets one card after the period, through the production Native, the real
//! tick, a fixed clock and fleet rows made by the producers.
use super::super::ports::{self, Availability, Fact};
use super::super::{fleet::Facts, Runtime};
use super::real_rows::Production;
use super::rows::{seed_row, KINDS};
use crate::teams::ops::{tests::*, Operation};
use crate::teams::store::{Card, InboxItem};
use serde_json::{json, Value};
use zerus_team_plan::model::{Member, Node, TaskRef, Team};

const MINUTE: i64 = 60_000;
const T0: i64 = 10_000_000;
const NULLS: [(&str, &str); 3] = [
    (
        "subagent_active_count",
        "its session has reported no subagent count; its hooks have sent no event yet",
    ),
    ("optional_questions", "its question index cannot be read"),
    (
        "process_work.count",
        "its background work cannot be verified from the process table",
    ),
];

struct World {
    h: Harness,
    a: Runtime,
    p: Production,
}

/// A team with `members` bound and `tasks` ready tasks of the developer
/// role; every member's fleet row is the producer's idle row.
fn world(members: &[&str], tasks: u32) -> World {
    let mut h = Harness::new();
    for m in members {
        h.bind(m);
    }
    for n in 1..=tasks {
        h.add(&person(), n);
    }
    let native = super::production::native(&h);
    let machine = h.store.team(TEAM).unwrap().unwrap().home;
    let a = Runtime::open(h.temp.path(), &machine).unwrap();
    World {
        h,
        a,
        p: Production {
            native,
            deliveries: vec![],
        },
    }
}

/// Commits `member`'s row: the producer's idle row of its agent, with its
/// identity and `changes`.
fn row(w: &World, member: &str, changes: &[(&str, Value)]) {
    let m = w.h.store.member(TEAM, member).unwrap().unwrap();
    let launch = m.session.as_ref().unwrap().session_id["launch_id:".len()..].to_owned();
    let run = format!("fixture-{member}");
    let mut all = vec![
        ("name", json!(format!("{}/fixture/{member}", m.agent))),
        ("run_id", json!(run)),
        ("launch_id", json!(launch)),
        ("conversation_id", json!(format!("conversation-{member}"))),
    ];
    all.extend(changes.iter().cloned());
    let row = seed_row(&m.agent, &all).to_string();
    rusqlite::Connection::open(w.p.native.config.state.join("desktop/state.sqlite3"))
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(1,?1,?2,?3)",
            rusqlite::params![format!("run:{run}"), row, row.len()],
        )
        .unwrap();
}
fn unknown(w: &World, member: &str, path: &str) {
    row(w, member, &[(path, Value::Null)]);
}
fn pass(w: &mut World, minute: i64) {
    let now = T0 + minute * MINUTE;
    w.a.tick(&mut w.h.store, &mut w.p, TEAM, now, false)
        .unwrap();
}
/// The cards of `member`'s row, oldest first.
fn cards(h: &Harness, member: &str) -> Vec<Card> {
    let mut rows: Vec<Card> = h
        .store
        .cards(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|c| {
            c.kind == "team_actor_blocked" && c.text.starts_with(&format!("Member {member} "))
        })
        .collect();
    rows.sort_by_key(|c| c.seq);
    rows
}
fn open(h: &Harness, member: &str) -> Vec<Card> {
    cards(h, member)
        .into_iter()
        .filter(|c| c.state == "open")
        .collect()
}
fn hold(w: &World, member: &str) -> Option<Value> {
    w.a.ledger
        .get(TEAM, &format!("gate:unreadable:{member}"))
        .unwrap()
        .map(|row| row.data)
}
fn decide(h: &mut Harness, card: u64, decision: &str) {
    h.run(
        &person(),
        Operation::DecideCard {
            card,
            decision: decision.into(),
        },
    )
    .unwrap();
}
fn needed(task: u32, fact: &str) -> String {
    format!("Member m1 (developer) is needed by ready task {task} but cannot be read: {fact}.")
}

fn unstaffed_items(world: &World) -> Vec<InboxItem> {
    world
        .h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|item| item.kind.starts_with("unstaffed:"))
        .collect()
}

#[test]
fn every_eligible_member_unreadable_opens_a_lead_item_until_reads_recover() {
    let mut w = world(&["m1", "m2"], 1);
    unknown(&w, "m1", "subagent_active_count");
    unknown(&w, "m2", "optional_questions");
    for minute in 0..3 {
        pass(&mut w, minute);
        assert!(unstaffed_items(&w).is_empty(), "the wait has not elapsed");
    }
    pass(&mut w, 3);
    let inbox = unstaffed_items(&w);
    assert_eq!(inbox.len(), 1, "the lead must hear about both members");
    assert_eq!(
        inbox[0].text,
        "Task 1 is ready. The service cannot read any eligible member (m1, m2). It will check again."
    );
    let seq = inbox[0].seq;
    assert_eq!(open(&w.h, "m1").len(), 1, "the person's card still applies");
    restart(&mut w);
    row(&w, "m1", &[("activity", json!("busy"))]);
    pass(&mut w, 4);
    unknown(&w, "m1", "subagent_active_count");
    pass(&mut w, 5);
    assert_eq!(unstaffed_items(&w)[0].seq, seq);
    assert_eq!(
        unstaffed_items(&w)[0].state,
        "open",
        "one read cannot flap the notice"
    );

    row(&w, "m1", &[("activity", json!("busy"))]);
    for minute in 6..=9 {
        pass(&mut w, minute);
    }
    let inbox = unstaffed_items(&w);
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].seq, seq);
    assert_eq!(inbox[0].state, "closed");
    assert!(
        w.p.deliveries.is_empty(),
        "a readable busy member is not free"
    );
}

#[test]
fn a_readable_busy_candidate_prevents_the_all_unreadable_notice() {
    let mut w = world(&["m1", "m2"], 1);
    unknown(&w, "m1", "subagent_active_count");
    row(&w, "m2", &[("activity", json!("busy"))]);
    for minute in 0..=3 {
        pass(&mut w, minute);
        assert!(
            unstaffed_items(&w).is_empty(),
            "m2 can still take the task later"
        );
    }
    assert_eq!(
        w.h.store.unreadable_members(TEAM).unwrap(),
        ["m1".into()].into()
    );
    unknown(&w, "m2", "optional_questions");
    for minute in 4..7 {
        pass(&mut w, minute);
        assert!(
            unstaffed_items(&w).is_empty(),
            "m2's own wait has not elapsed"
        );
    }
    pass(&mut w, 7);
    assert_eq!(unstaffed_items(&w).len(), 1);
}

#[test]
#[ignore = "exports a private store and a client fixture for the two-engine check"]
fn export_teams81_readability_store() {
    use zerus_client_core::actions::Exit;
    use zerus_client_core::upstream_teams::drawers::{decode, DrawerKind, DrawerRead};

    let destination = std::path::PathBuf::from(std::env::var_os("TEAMS81_COMPAT_DIR").unwrap());
    let mut w = world(&["m1", "m2"], 1);
    unknown(&w, "m1", "subagent_active_count");
    unknown(&w, "m2", "optional_questions");
    for minute in 0..=3 {
        pass(&mut w, minute);
    }
    let page = w.h.store.inbox(TEAM, 200, None).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.next.is_none());
    let mut reply: Value = serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-inbox.json"
    )).unwrap();
    reply["rows"] = json!(page.rows);
    let projected = decode(
        &DrawerRead {
            team: TEAM.into(),
            kind: DrawerKind::Inbox,
            after: None,
            recorded_timings: false,
            timing_seqs: None,
            timing_next: None,
        },
        Exit::Code {
            code: 0,
            stdout: serde_json::to_vec(&reply).unwrap(),
            stderr: String::new(),
        },
    )
    .unwrap();
    let teams = destination.join("teams");
    crate::platform::private_dir(&teams).unwrap();
    let mut fixture = serde_json::to_string_pretty(&projected.rows[0]).unwrap();
    fixture.push('\n');
    std::fs::write(destination.join("unreadable-notice.json"), fixture).unwrap();
    drop(w.a);
    drop(w.h.store);
    for file in ["preview", "teams-v1.sqlite3"] {
        std::fs::copy(w.h.temp.path().join("teams").join(file), teams.join(file)).unwrap();
    }
    println!("The actor produced two durable marks, one inbox item and its client projection.");
}

/// The reader names a count only when it alone keeps the member from free;
/// anything known that keeps it busy makes it busy.
#[test]
fn the_reader_names_an_unknown_count_only_of_an_otherwise_free_member() {
    // The captured bridge reports complete, empty background work. A
    // legacy or incomplete summary still names the unknown background fact.
    let dsh = Facts::of(&seed_row("dsh", &[]));
    assert!(dsh.free());
    assert_eq!(dsh.unknown(), None);
    let unknown = Facts::of(&seed_row("dsh", &[("process_work.count", Value::Null)]));
    assert!(!unknown.free());
    assert_eq!(unknown.unknown(), Some(Fact::Background));
    // A reported count that cannot be settled is named apart from a count
    // never reported; one with a working child is busy, not unreadable.
    for kind in ["claude", "codex", "kimi"] {
        let unsettled = [
            ("subagent_source", json!("hook_profiles")),
            ("subagent_counts_complete", json!(false)),
            ("subagent_groups_complete", json!(false)),
        ];
        let facts = Facts::of(&seed_row(kind, &unsettled));
        assert!(!facts.free(), "{kind}");
        assert_eq!(facts.unknown(), Some(Fact::ChildrenUnsettled), "{kind}");
        let working = [
            unsettled[0].clone(),
            unsettled[1].clone(),
            ("subagent_active_count", json!(1)),
        ];
        assert_eq!(
            Facts::of(&seed_row(kind, &working)).unknown(),
            None,
            "{kind}"
        );
    }
    for kind in KINDS.iter().filter(|kind| **kind != "dsh") {
        let kind = *kind;
        let facts = Facts::of(&seed_row(kind, &[]));
        assert!(facts.free() && facts.unknown().is_none(), "{kind}");
        for (path, fact) in [
            ("process_work.count", Fact::Background),
            ("subagent_active_count", Fact::Children),
            ("optional_questions", Fact::Questions),
        ] {
            let alone = Facts::of(&seed_row(kind, &[(path, Value::Null)]));
            assert!(!alone.free(), "{kind} {path}");
            assert_eq!(alone.unknown(), Some(fact), "{kind} {path}");
            for busy in [
                ("activity", json!("busy")),
                ("phase", json!("tool")),
                ("process_state", json!("exited")),
                ("runtime_state", json!("stopped")),
                ("optional_questions.count", json!(1)),
                ("subagent_active_count", json!(1)),
                ("process_work.count", json!(1)),
            ] {
                if busy.0 == path || path == "optional_questions" && busy.0.starts_with(path) {
                    continue;
                }
                let row = seed_row(kind, &[busy.clone(), (path, Value::Null)]);
                assert_eq!(Facts::of(&row).unknown(), None, "{kind} {path} {busy:?}");
            }
        }
        // Several unknown: the first in the reader's order.
        let all = Facts::of(&seed_row(
            kind,
            &[
                ("optional_questions", Value::Null),
                ("subagent_active_count", Value::Null),
                ("process_work.count", Value::Null),
            ],
        ));
        assert_eq!(all.unknown(), Some(Fact::Background), "{kind}");
    }
}

/// Silent for the period, then one card naming the member, the task and the
/// fact; no hand-out while unknown, and no second card while it lasts.
#[test]
fn an_unknown_count_of_a_needed_member_shows_one_card_after_the_period() {
    for (path, fact) in NULLS {
        let mut w = world(&["m1"], 1);
        unknown(&w, "m1", path);
        for minute in 0..3 {
            pass(&mut w, minute);
            assert!(cards(&w.h, "m1").is_empty(), "{path} minute {minute}");
            assert_eq!(
                hold(&w, "m1").unwrap()["hold"]["start"],
                json!(T0),
                "{path}"
            );
        }
        pass(&mut w, 3);
        let shown = open(&w.h, "m1");
        assert_eq!(shown.len(), 1, "{path}");
        assert!(
            shown[0].text.starts_with(&needed(1, fact)),
            "{}",
            shown[0].text
        );
        assert_eq!(shown[0].node, None);
        for minute in 4..12 {
            pass(&mut w, minute);
        }
        assert_eq!(cards(&w.h, "m1").len(), 1, "{path}: one card in all");
        assert!(w.p.deliveries.is_empty(), "{path}: {:?}", w.p.deliveries);
    }
}

/// Readable again, busy: the card closes and the row ends; nothing is
/// handed out to a busy member.
#[test]
fn readable_again_the_card_closes_and_the_row_ends() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "subagent_active_count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    let card = open(&w.h, "m1").remove(0);
    row(&w, "m1", &[("activity", json!("busy"))]);
    pass(&mut w, 4);
    assert!(open(&w.h, "m1").is_empty());
    assert_eq!(cards(&w.h, "m1")[0].seq, card.seq);
    assert!(hold(&w, "m1").is_none());
    assert!(w.p.deliveries.is_empty());
}

/// A needed member that becomes free takes its task through the kernel's
/// first selection, and its row ends in that same pass.
#[test]
fn a_needed_member_becoming_free_takes_its_task_and_its_row_ends_in_that_pass() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "process_work.count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    assert_eq!(open(&w.h, "m1").len(), 1);
    row(&w, "m1", &[]);
    pass(&mut w, 4);
    assert_eq!(w.p.deliveries.len(), 1, "{:?}", w.p.deliveries);
    assert!(w.p.deliveries[0].starts_with("m1 -> Outcome(Submitted"));
    assert!(open(&w.h, "m1").is_empty());
    assert!(hold(&w, "m1").is_none());
}

/// A member nobody needs raises nothing: with no ready task, when another
/// free member takes the task, or when no ready task is of its role.
#[test]
fn a_member_nobody_needs_raises_nothing() {
    let cases: [(&[&str], u32, &str); 3] = [
        (&["m1"], 0, "m1"),
        (&["m1", "m2"], 1, "m1"),
        (&["m3"], 1, "m3"),
    ];
    for (members, tasks, member) in cases {
        let mut w = world(members, tasks);
        unknown(&w, member, "subagent_active_count");
        for minute in 0..100 {
            pass(&mut w, minute);
            assert!(hold(&w, member).is_none(), "{members:?} minute {minute}");
        }
        assert!(cards(&w.h, member).is_empty(), "{members:?}");
        let expected = usize::from(members.len() == 2);
        assert_eq!(w.p.deliveries.len(), expected, "{:?}", w.p.deliveries);
    }
}

/// A need that comes and goes faster than the period shows nothing, as
/// long as no task waits a whole period; one that then lasts the period
/// shows its card exactly a period after it began.
#[test]
fn a_need_shorter_than_the_period_shows_nothing() {
    let mut w = world(&["m1"], 1);
    let mut minute = 0;
    for _ in 0..8 {
        // Needed and unreadable for two passes, then readable and busy.
        unknown(&w, "m1", "subagent_active_count");
        for _ in 0..3 {
            pass(&mut w, minute);
            minute += 1;
        }
        row(&w, "m1", &[("activity", json!("busy"))]);
        pass(&mut w, minute);
        minute += 1;
        assert!(hold(&w, "m1").is_none());
    }
    // The need itself goes: a paused team hands nothing out.
    for _ in 0..4 {
        unknown(&w, "m1", "subagent_active_count");
        for _ in 0..2 {
            pass(&mut w, minute);
            minute += 1;
        }
        w.h.run(&person(), Operation::Pause).unwrap();
        pass(&mut w, minute);
        minute += 1;
        assert!(hold(&w, "m1").is_none(), "a paused team needs nobody");
        w.h.run(&person(), Operation::Resume).unwrap();
    }
    assert!(cards(&w.h, "m1").is_empty());
    let start = minute;
    for _ in 0..3 {
        pass(&mut w, minute);
        minute += 1;
    }
    assert!(cards(&w.h, "m1").is_empty());
    pass(&mut w, start + 3);
    assert_eq!(open(&w.h, "m1").len(), 1);
    assert!(w.p.deliveries.is_empty());
}

/// A working member whose count is still unknown is busy however long that
/// lasts; the pass it turns idle with the count still unknown, it is needed
/// and unreadable, and its age starts there.
#[test]
fn a_working_member_with_an_unknown_count_raises_nothing_until_it_is_idle() {
    let mut w = world(&["m1"], 1);
    row(
        &w,
        "m1",
        &[
            ("activity", json!("busy")),
            ("subagent_active_count", Value::Null),
        ],
    );
    for minute in 0..30 {
        pass(&mut w, minute);
        assert!(hold(&w, "m1").is_none(), "minute {minute}");
    }
    assert!(cards(&w.h, "m1").is_empty());
    unknown(&w, "m1", "subagent_active_count");
    pass(&mut w, 30);
    let start = T0 + 30 * MINUTE;
    assert_eq!(hold(&w, "m1").unwrap()["hold"]["start"], json!(start));
    pass(&mut w, 32);
    assert!(cards(&w.h, "m1").is_empty());
    pass(&mut w, 33);
    assert_eq!(open(&w.h, "m1").len(), 1);
    assert!(w.p.deliveries.is_empty());
}

/// A changed fact neither restarts the age nor closes the card; it replaces
/// the card once it has stood for the period. A flapping fact keeps the
/// first card.
#[test]
fn a_changed_fact_replaces_the_card_only_after_it_stood_for_the_period() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "subagent_active_count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    let first = open(&w.h, "m1").remove(0);
    // Flapping: never the same fact for a whole period.
    for minute in 4..20 {
        let path = if minute % 2 == 0 {
            "optional_questions"
        } else {
            "subagent_active_count"
        };
        unknown(&w, "m1", path);
        pass(&mut w, minute);
        let shown = open(&w.h, "m1");
        assert_eq!(shown.len(), 1, "minute {minute}");
        assert_eq!(shown[0].seq, first.seq, "minute {minute}");
    }
    unknown(&w, "m1", "optional_questions");
    for minute in 20..23 {
        pass(&mut w, minute);
        assert_eq!(open(&w.h, "m1")[0].seq, first.seq, "minute {minute}");
    }
    pass(&mut w, 23);
    let shown = open(&w.h, "m1");
    assert_eq!(shown.len(), 1);
    assert_ne!(shown[0].seq, first.seq);
    assert!(shown[0]
        .text
        .starts_with(&needed(1, "its question index cannot be read")));
    assert_eq!(cards(&w.h, "m1").len(), 2);
    assert_eq!(hold(&w, "m1").unwrap()["hold"]["start"], json!(T0));
}

/// A dismissal lasts while the sentence is the same; a changed one that has
/// stood for the period opens a new card.
#[test]
fn a_dismissal_lasts_while_the_fact_is_the_same() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "subagent_active_count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    let first = open(&w.h, "m1").remove(0);
    decide(&mut w.h, first.seq, "acknowledged");
    for minute in 4..30 {
        pass(&mut w, minute);
    }
    assert!(open(&w.h, "m1").is_empty());
    assert_eq!(cards(&w.h, "m1").len(), 1);
    unknown(&w, "m1", "process_work.count");
    for minute in 30..33 {
        pass(&mut w, minute);
        assert!(open(&w.h, "m1").is_empty(), "minute {minute}");
    }
    pass(&mut w, 33);
    assert_eq!(open(&w.h, "m1").len(), 1);
}

/// An acknowledgement does not outlive the row: a new need after the period
/// shows the card again.
#[test]
fn an_acknowledgement_does_not_outlive_the_row() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "subagent_active_count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    let first = open(&w.h, "m1").remove(0);
    decide(&mut w.h, first.seq, "acknowledged");
    row(&w, "m1", &[("activity", json!("busy"))]);
    pass(&mut w, 4);
    assert!(hold(&w, "m1").is_none());
    unknown(&w, "m1", "subagent_active_count");
    for minute in 5..8 {
        pass(&mut w, minute);
        assert!(open(&w.h, "m1").is_empty(), "minute {minute}");
    }
    pass(&mut w, 8);
    assert_eq!(open(&w.h, "m1").len(), 1);
    assert_eq!(cards(&w.h, "m1").len(), 2);
}

/// Retry reads the member again on the next pass: still unreadable, its
/// card is shown again at once.
#[test]
fn retry_reads_again_on_the_next_pass() {
    let mut w = world(&["m1"], 1);
    unknown(&w, "m1", "subagent_active_count");
    for minute in 0..4 {
        pass(&mut w, minute);
    }
    let first = open(&w.h, "m1").remove(0);
    decide(&mut w.h, first.seq, "retry");
    assert!(open(&w.h, "m1").is_empty());
    pass(&mut w, 4);
    let shown = open(&w.h, "m1");
    assert_eq!(shown.len(), 1);
    assert_ne!(shown[0].seq, first.seq);
    // Free by then: Retry's pass hands the task out and shows nothing.
    decide(&mut w.h, shown[0].seq, "retry");
    row(&w, "m1", &[]);
    pass(&mut w, 5);
    assert!(open(&w.h, "m1").is_empty());
    assert_eq!(w.p.deliveries.len(), 1);
    assert!(hold(&w, "m1").is_none());
}

fn crash(point: &'static str, action: impl FnOnce()) {
    super::super::retry::CRASH.with(|s| s.set(Some(point)));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    let remaining = super::super::retry::CRASH.with(|s| s.replace(None));
    assert!(result.is_err() && remaining.is_none(), "unvisited {point}");
}
fn restart(w: &mut World) {
    let machine = w.h.store.team(TEAM).unwrap().unwrap().home;
    w.a = Runtime::open(w.h.temp.path(), &machine).unwrap();
    w.h.store.refresh().unwrap();
}

/// A restart at each durable step of opening and of closing the card: one
/// card in all, opened once and closed once.
#[test]
fn a_restart_at_each_card_step_leaves_one_card() {
    for point in ["notice_reserved", "notice_applied", "notice_saved"] {
        let mut w = world(&["m1"], 1);
        unknown(&w, "m1", "subagent_active_count");
        for minute in 0..3 {
            pass(&mut w, minute);
        }
        crash(point, || pass(&mut w, 3));
        restart(&mut w);
        for minute in 4..8 {
            pass(&mut w, minute);
        }
        assert_eq!(cards(&w.h, "m1").len(), 1, "{point}");
        assert_eq!(open(&w.h, "m1").len(), 1, "{point}");
    }
    for point in [
        "recovery_before_close",
        "recovery_closed",
        "recovery_removed",
    ] {
        let mut w = world(&["m1"], 1);
        unknown(&w, "m1", "subagent_active_count");
        for minute in 0..4 {
            pass(&mut w, minute);
        }
        row(&w, "m1", &[("activity", json!("busy"))]);
        crash(point, || pass(&mut w, 4));
        restart(&mut w);
        for minute in 5..9 {
            pass(&mut w, minute);
        }
        let all = cards(&w.h, "m1");
        assert_eq!(all.len(), 1, "{point}");
        assert_ne!(all[0].state, "open", "{point}");
        assert!(hold(&w, "m1").is_none(), "{point}");
    }
}

/// A port that answers with one named fact for m1, so that every sentence
/// is read from the card the real tick opens.
struct Named(Fact, bool);
impl ports::Ports for Named {
    fn changes(&mut self, _: &str, _: &str, _: u64) -> ports::Result<zerus_replica::Changes> {
        Ok(zerus_replica::Changes {
            generation: "journal".into(),
            offset: 0,
            documents: Some(Default::default()),
        })
    }
    fn observe(&mut self, _: &mut crate::teams::store::Store, _: &TaskRef) -> ports::Result<()> {
        Ok(())
    }
    fn free(&mut self, _: &Team, _: &Member) -> ports::Result<bool> {
        Ok(false)
    }
    fn availability(&mut self, _: &Team, m: &Member) -> ports::Result<Availability> {
        if self.1 {
            return Err(crate::desktop::teams::actor::problem(
                "fixture read failure",
            ));
        }
        Ok(if m.id == "m1" {
            Availability::Unreadable(self.0.clone())
        } else {
            Availability::Busy
        })
    }
    fn deliver(&mut self, _: &Team, _: &Node, _: &Member, _: &str) -> ports::DeliveryResult {
        panic!("nothing is handed out to an unreadable member")
    }
}

/// Each fact's sentence on the card: what cannot be read and what the person
/// can do about it with the operations this build has.
#[test]
fn each_fact_reads_its_sentence_on_the_card() {
    let close = "This card closes when the member can be read or no ready task needs it.";
    let last = "This seat cannot be bound again; archive the member.";
    let cases = [
        (Fact::Children, "its session has reported no subagent count; its hooks have sent no event yet. Wait for its first hook event. If its hooks never report, stop or archive the member.".to_owned()),
        (Fact::ChildrenUnsettled, "the state of some of its subagents cannot be settled. If this lasts, stop or archive the member.".to_owned()),
        (Fact::Questions, "its question index cannot be read. If this lasts, stop or archive the member.".to_owned()),
        (Fact::Background, "its background work cannot be verified from the process table. If this lasts, stop or archive the member.".to_owned()),
        (Fact::NoHandle, format!("it was bound before session handles, and nothing names its session now. {last}")),
        (Fact::Gone, "its session no longer exists here. If it was archived, restore it and the member is read again; otherwise archive the member.".to_owned()),
        (Fact::Changed, format!("its session now belongs to another launch, agent or folder. {last}")),
        (Fact::StartedFresh, format!("its session was started fresh, and a new conversation does not take over its tasks. {last}")),
        (Fact::Unavailable, "its session record did not answer in time. Retry reads it again on the next pass.".to_owned()),
    ];
    // The terminal fact's own sentence names the dialog its gate knew
    // (zerus-63rru.83), in the card's own shape.
    {
        let mut h = Harness::new();
        h.bind("m1");
        h.add(&person(), 1);
        let machine = h.store.team(TEAM).unwrap().unwrap().home;
        let mut a = Runtime::open(h.temp.path(), &machine).unwrap();
        let mut p = Named(
            Fact::TerminalWaiting {
                dialog: Some("claude_usage_limit".into()),
            },
            false,
        );
        for minute in 0..4 {
            a.tick(&mut h.store, &mut p, TEAM, T0 + minute * MINUTE, false)
                .unwrap();
        }
        let shown = open(&h, "m1");
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(
            shown[0].text,
            "Member m1 (developer) is needed by ready task 1, but its terminal is waiting for input (a usage-limit choice). Answer it or clear it in the member's terminal. This card closes when the member can take work again or no ready task needs it."
        );
    }
    for (fact, words) in cases {
        let mut h = Harness::new();
        h.bind("m1");
        h.add(&person(), 1);
        let machine = h.store.team(TEAM).unwrap().unwrap().home;
        let mut a = Runtime::open(h.temp.path(), &machine).unwrap();
        let mut p = Named(fact.clone(), false);
        for minute in 0..4 {
            a.tick(&mut h.store, &mut p, TEAM, T0 + minute * MINUTE, false)
                .unwrap();
        }
        let shown = open(&h, "m1");
        assert_eq!(shown.len(), 1, "{fact:?}");
        assert_eq!(
            shown[0].text,
            format!(
                "Member m1 (developer) is needed by ready task 1 but cannot be read: {words} {close}"
            ),
            "{fact:?}"
        );
        assert_eq!(shown[0].decisions, ["retry", "acknowledged"]);
        assert!(!shown[0].text.contains("lead"), "{fact:?}");
    }
}

/// A read that fails waits for its backoff; the passes in between read
/// nothing about the member, and its row and card stay as they are.
#[test]
fn a_read_that_is_not_due_leaves_the_row_and_its_card() {
    let mut h = Harness::new();
    h.bind("m1");
    h.add(&person(), 1);
    let machine = h.store.team(TEAM).unwrap().unwrap().home;
    let mut a = Runtime::open(h.temp.path(), &machine).unwrap();
    let mut p = Named(Fact::Children, false);
    let at = |second: i64| T0 + second * 1_000;
    for minute in 0..4 {
        a.tick(&mut h.store, &mut p, TEAM, at(minute * 60), false)
            .unwrap();
    }
    let card = open(&h, "m1").remove(0);
    p.1 = true;
    for second in [240, 250, 270, 290] {
        a.tick(&mut h.store, &mut p, TEAM, at(second), false)
            .unwrap();
        let shown = open(&h, "m1");
        assert_eq!(shown.len(), 1, "second {second}");
        assert_eq!(shown[0].seq, card.seq, "second {second}");
        assert!(a.ledger.get(TEAM, "gate:unreadable:m1").unwrap().is_some());
    }
    p.1 = false;
    a.tick(&mut h.store, &mut p, TEAM, at(360), false).unwrap();
    assert_eq!(open(&h, "m1")[0].seq, card.seq);
    assert_eq!(cards(&h, "m1").len(), 1);
}

/// Native's answer keeps the session's task index: an idle, readable member
/// whose session holds an active task elsewhere is busy, not free, in both
/// `availability` and the `free` derived from it.
#[test]
fn an_occupied_member_is_busy_in_natives_answer() {
    use super::super::native::TEST_INVENTORY;
    use ports::Ports;
    let w = world(&["m1"], 0);
    let mut native = w.p.native;
    let team = w.h.store.team(TEAM).unwrap().unwrap();
    let member = w.h.store.member(TEAM, "m1").unwrap().unwrap();
    TEST_INVENTORY.with(|s| {
        *s.borrow_mut() =
            Some(json!({"node_id":native.machine,"organization":{"projects":[{"id":"project-1"}]}}))
    });
    for (tasks, expected) in [
        (json!([{"id":"busy"}]), Availability::Busy),
        (json!([]), Availability::Free),
    ] {
        let reply = json!({"damaged_total":0,"tasks":tasks}).to_string();
        crate::desktop::teams::child::FIXTURE_EXIT
            .with(|s| *s.borrow_mut() = Some(reply.clone().into_bytes()));
        assert_eq!(native.availability(&team, &member).unwrap(), expected);
        crate::desktop::teams::child::FIXTURE_EXIT
            .with(|s| *s.borrow_mut() = Some(reply.into_bytes()));
        assert_eq!(
            native.free(&team, &member).unwrap(),
            expected == Availability::Free
        );
    }
    crate::desktop::teams::child::FIXTURE_EXIT.with(|s| s.borrow_mut().take());
    TEST_INVENTORY.with(|s| *s.borrow_mut() = None);
}
