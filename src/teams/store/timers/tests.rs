use super::*;
mod legacy_holds;
mod upgrade;
mod work;
use crate::teams::ops::{tests::*, Actor, Operation};

fn reopen(h: &mut Harness) {
    h.store = Store::open(h.temp.path()).unwrap();
}
fn db(h: &Harness) -> &Connection {
    h.store.db.as_ref().unwrap()
}

#[test]
fn the_terminal_index_is_filled_by_the_transition_and_never_by_a_marked_open() {
    let mut h = Harness::new();
    h.run(&person(), Operation::Stop).unwrap();
    db(&h)
        .execute_batch("DROP TRIGGER key_clear_terminal_v1; DROP TABLE key_clear_teams; DROP TABLE timers_extension;")
        .unwrap();
    reopen(&mut h);
    assert_eq!(h.store.keyed_teams("", 10).unwrap(), vec![TEAM.to_owned()]);
    db(&h).execute("DELETE FROM key_clear_teams", []).unwrap();
    reopen(&mut h);
    assert!(
        h.store.keyed_teams("", 10).unwrap().is_empty(),
        "later opens never fill it again"
    );
}

#[test]
fn a_reopened_store_keeps_its_timer_triggers_under_their_versioned_names() {
    let mut h = Harness::new();
    reopen(&mut h);
    let names: Vec<String> = column(
        db(&h),
        "SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name",
        [],
    )
    .unwrap();
    assert_eq!(
        names,
        [
            "answer_mark_drop_v1",
            "key_clear_cancel_settled_v1",
            "key_clear_stage_archived_v2",
            "key_clear_stage_settled_v2",
            "key_clear_terminal_v1",
            "member_handles_removed_v1",
            "member_handles_terminal_v1",
            "person_action_original_immutable",
            "person_action_original_kept",
            "person_gesture_store_immutable",
            "person_gesture_store_kept",
            "person_gestures_immutable",
            "phase_pause_identity",
            "phase_rework_identity",
            "team_member_readability_binding_v1",
            "timer_cards_decided_v1",
            "timer_cards_item_v1"
        ],
        "a changed body is a new name; version 1 of the stage triggers is dropped"
    );
}

#[test]
fn the_inbox_view_counts_open_items_up_to_its_bound() {
    let h = Harness::new();
    assert_eq!(h.store.inbox_view(TEAM).unwrap(), None);
    for n in 0..250 {
        let state = if n % 50 == 0 { "closed" } else { "open" };
        db(&h).execute("INSERT INTO inbox(team,kind,node,generation,actor,text,state,created_ms) VALUES(?1,'report',?2,1,'{}','Example',?3,?2)",params![TEAM,n,state]).unwrap();
    }
    let view = h.store.inbox_view(TEAM).unwrap().unwrap();
    assert_eq!((view.open, view.newest), (201, 250));
    assert_eq!(view.oldest.unwrap().node, 1);
}

#[test]
fn the_doorbell_admission_reads_the_team_the_binding_and_a_new_item() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    let Actor::Session { session, .. } = lead.clone() else {
        unreachable!()
    };
    let bell = |after| Bell {
        team: TEAM.into(),
        member: "lead".into(),
        session: session.clone(),
        after,
    };
    assert!(admit_bell(db(&h), &bell(0)).is_err(), "nothing waits");
    h.bind("m1");
    let node = h.add(&person(), 1);
    h.submit(node, "m1");
    h.report(node, "m1");
    admit_bell(db(&h), &bell(0)).unwrap();
    assert!(
        admit_bell(db(&h), &bell(1)).is_err(),
        "nothing beyond the mark"
    );
    let mut other = bell(0);
    other.session.session_id = "run_id:another-run".into();
    assert!(admit_bell(db(&h), &other).is_err(), "another binding");
    h.run(&lead, Operation::Pause).unwrap();
    assert!(admit_bell(db(&h), &bell(0)).is_err(), "paused");
}

/// Every timer read is a search through an index: no full scan of a table
/// and no temporary sort. `--nocapture` prints the plans for the report.
#[test]
fn every_timer_read_searches_an_index() {
    let h = Harness::new();
    let queue = crate::teams::store::pages::node_page_sql("nodes", false);
    let live = crate::teams::store::pages::node_page_sql("nodes", true);
    let archived = crate::teams::store::pages::node_page_sql("node_archive", true);
    let effect_nodes = crate::teams::store::unstaffed::open_effect_nodes_sql();
    for (name, sql) in [
        ("QUEUE_PAGE", queue.as_str()),
        ("NODES_BY_STATE_PAGE", live.as_str()),
        ("ARCHIVE_BY_STATE_PAGE", archived.as_str()),
        (
            "UNSTAFFED_TOLD_ITEMS",
            crate::teams::store::unstaffed::TOLD_ITEMS,
        ),
        ("OPEN_EFFECT_NODES", effect_nodes.as_str()),
        ("MEMBER_PAGE", crate::teams::store::pages::MEMBER_PAGE_SQL),
        ("NEWEST", sql::NEWEST),
        ("OPEN", sql::OPEN),
        ("OLDEST", sql::OLDEST),
        ("UNREPORTED", sql::UNREPORTED),
        ("UNSTARTED", sql::UNSTARTED),
        ("UNDECIDED", sql::UNDECIDED),
        ("FIRED", sql::FIRED),
        ("KEYED_TEAMS", sql::KEYED_TEAMS),
        ("KEYED_PAGE", sql::KEYED_PAGE),
        ("SETTLE", sql::SETTLE),
        ("LATE", sql::LATE),
        ("REFUSALS", sql::REFUSALS),
        ("KEY_REFUSALS_NEWEST", sql::REFUSALS_NEWEST),
        ("KEY_REFUSALS_COUNT", sql::REFUSALS_COUNT),
        ("SAVED_REFUSAL", sql::SAVED_REFUSAL),
        ("PENDING_CANCELS", super::cancels::sql::PENDING),
        ("CANCEL_REFUSALS_REST", super::cancels::sql::REFUSED_REST),
        ("CANCEL_REFUSALS_AFTER", super::cancels::sql::REFUSED_AFTER),
        ("CANCEL_REFUSALS_COUNT", super::cancels::sql::REFUSED_COUNT),
        ("RELEASED_STAGES", sql::RELEASED_STAGES),
        ("QUESTIONS", super::answers::sql::QUESTIONS),
        ("ANSWER_MARKS", super::answers::sql::MARKS),
        ("ANSWER_MARK", super::answers::sql::MARK),
        ("ANSWER_HOLDER", super::answers::sql::HOLDER),
        ("WAITING", sql::WAITING),
        ("REVIEWED", super::reviewed::sql::REVIEWED),
        ("ACCEPTED", super::reviewed::sql::ACCEPTED),
        ("OWN", super::reviewed::sql::OWN),
    ] {
        let plan = explain(db(&h), sql);
        println!("{name}:\n  {}", plan.join("\n  "));
        for line in &plan {
            let table = !line.starts_with("SCAN (subquery-") && line != "SCAN CONSTANT ROW";
            let access = table && (line.starts_with("SCAN") || line.starts_with("SEARCH"));
            assert!(!access || line.contains("USING"), "{name}: {line}");
            assert!(!line.contains("TEMP B-TREE"), "{name}: {line}");
        }
    }
}
fn explain(db: &Connection, sql: &str) -> Vec<String> {
    let mut s = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let nulls = vec![rusqlite::types::Null; s.parameter_count()];
    let rows = s.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3));
    rows.unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
}
