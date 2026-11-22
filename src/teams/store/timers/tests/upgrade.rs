//! The transition that adds this package to a store: one immediate
//! transaction, a marker written last, rerun whole when the marker is absent.
use super::*;
use crate::teams::store::bases::PINNED_EXTENSION;
use crate::teams::store::timers::schema::{AFTER_FENCE, BASE, BEFORE_MARK, EXTENSION, TRANSITIONS};
use std::sync::{Arc, Barrier, Mutex};

/// Every object timers.sql and answers.sql create, so that a store can be
/// returned to its layout from before this package.
fn objects() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let sql = [
        include_str!("../../timers.sql"),
        include_str!("../../answers.sql"),
    ];
    for line in sql.iter().flat_map(|sql| sql.lines()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let ["CREATE", kind, "IF", "NOT", "EXISTS", name, ..] = words.as_slice() {
            found.push((kind.to_string(), name.trim_end_matches('(').to_string()));
        }
    }
    found
}
fn strip(h: &Harness) {
    for (kind, name) in objects().into_iter().rev() {
        db(h)
            .execute_batch(&format!("DROP {kind} IF EXISTS {name};"))
            .unwrap();
    }
}
fn transitions() -> u32 {
    TRANSITIONS.with(|n| n.get())
}
fn names(h: &Harness) -> Vec<String> {
    column(
        db(h),
        "SELECT name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        [],
    )
    .unwrap()
}
fn marker(h: &Harness) -> Option<u32> {
    db(h)
        .query_row("SELECT version FROM timers_extension WHERE id=1", [], |r| {
            r.get(0)
        })
        .ok()
}
fn stopped() -> Harness {
    let mut h = Harness::new();
    h.bind("lead");
    h.bind("m1");
    let gone = h.add(&person(), 1);
    h.cancel_fact(gone);
    h.add(&person(), 2);
    h.run(&person(), Operation::Stop).unwrap();
    h
}

#[test]
fn the_strip_list_covers_every_object_of_this_package() {
    let h = Harness::new();
    let full = names(&h);
    strip(&h);
    let ours: Vec<String> = objects().into_iter().map(|(_, n)| n).collect();
    let left = names(&h);
    assert!(left.iter().all(|n| !ours.contains(n)), "{left:?}");
    assert!(full.len() > left.len());
}

#[test]
fn a_store_from_before_this_package_with_an_archived_node_opens() {
    let mut h = stopped();
    let full = names(&h);
    strip(&h);
    let before = transitions();
    reopen(&mut h);
    assert_eq!(transitions(), before + 1);
    assert_eq!(names(&h), full);
    assert_eq!(marker(&h), Some(EXTENSION));
    assert_eq!(h.store.keyed_teams("", 10).unwrap(), vec![TEAM.to_owned()]);
    assert_eq!(h.store.pending_cancels(TEAM, 200, 0).unwrap().len(), 1);
}

#[test]
fn a_stop_committed_while_the_transition_runs_waits_for_it_and_is_captured() {
    let mut h = Harness::new();
    h.add(&person(), 1);
    strip(&h);
    let state = h.temp.path().to_owned();
    let thread = Arc::new(Mutex::new(None));
    let gap = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (slot, seen) = (thread.clone(), gap.clone());
    BEFORE_MARK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // Inside the open transaction: another writer cannot commit.
            let raw = rusqlite::Connection::open(state.join("teams/teams-v1.sqlite3")).unwrap();
            raw.busy_timeout(std::time::Duration::from_millis(50))
                .unwrap();
            let wrote = raw.execute("UPDATE teams SET name=name", []).is_ok();
            seen.store(wrote, std::sync::atomic::Ordering::Release);
            *slot.lock().unwrap() = Some(std::thread::spawn(move || {
                let mut store = Store::open(&state).unwrap();
                let rev = store.team(TEAM).unwrap().unwrap().revision;
                let stop = request(rev, Operation::Stop);
                crate::teams::ops::tests::apply_fixture(&mut store, &person(), &stop, 1234)
                    .unwrap();
            }));
            Ok(())
        }));
    });
    reopen(&mut h);
    thread.lock().unwrap().take().unwrap().join().unwrap();
    assert!(
        !gap.load(std::sync::atomic::Ordering::Acquire),
        "no write commits inside the transition"
    );
    assert_eq!(
        h.store.keyed_teams("", 10).unwrap(),
        vec![TEAM.to_owned()],
        "the stop is captured"
    );
    assert_eq!(h.store.pending_cancels(TEAM, 200, 0).unwrap().len(), 1);
}

/// A store without the mark is a store from before the extension, however
/// many of the extension's objects it already holds: the whole transition
/// runs, every step idempotent.
#[test]
fn a_store_without_the_mark_runs_the_whole_transition() {
    for sql in [
        "DROP TABLE timers_extension;",
        "DELETE FROM timers_extension;",
    ] {
        let mut h = stopped();
        let full = schema(&h);
        db(&h).execute_batch(sql).unwrap();
        let before = transitions();
        reopen(&mut h);
        assert_eq!(transitions(), before + 1, "{sql}");
        assert_eq!(schema(&h), full, "{sql}");
        assert_eq!(marker(&h), Some(EXTENSION));
        assert_eq!(h.store.keyed_teams("", 10).unwrap(), vec![TEAM.to_owned()]);
    }
}

#[test]
fn a_failed_transition_leaves_nothing_and_runs_again() {
    let mut h = stopped();
    let full = names(&h);
    strip(&h);
    let stripped = names(&h);
    BEFORE_MARK
        .with(|hook| *hook.borrow_mut() = Some(Box::new(|| Err(storage("injected failure")))));
    assert!(Store::open(h.temp.path()).is_err());
    assert_eq!(names(&h), stripped, "nothing of the transition remains");
    reopen(&mut h);
    assert_eq!(names(&h), full);
}

#[test]
fn two_processes_opening_at_once_both_open_and_one_extends() {
    let h = stopped();
    strip(&h);
    let barrier = Arc::new(Barrier::new(2));
    let openers: Vec<_> = (0..2)
        .map(|_| {
            let (state, barrier) = (h.temp.path().to_owned(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                let opened = Store::open(&state).map(|_| ());
                (opened, transitions())
            })
        })
        .collect();
    let results: Vec<_> = openers.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(
        results.iter().all(|(opened, _)| opened.is_ok()),
        "{results:?}"
    );
    assert_eq!(results.iter().map(|(_, n)| n).sum::<u32>(), 1);
    assert_eq!(marker(&h), Some(EXTENSION));
}

#[test]
fn a_second_open_does_no_work_and_takes_no_lock() {
    let mut h = stopped();
    let raw = rusqlite::Connection::open(h.temp.path().join("teams/teams-v1.sqlite3")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let before = transitions();
    let started = std::time::Instant::now();
    reopen(&mut h);
    assert_eq!(transitions(), before);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    raw.execute_batch("ROLLBACK;").unwrap();
}

/// The one-time cost at two archive sizes, by SQLite's progress handler.
#[test]
fn the_transition_walks_each_archived_node_once() {
    let mut costs = Vec::new();
    for archived in [1_000u32, 10_000] {
        let mut h = stopped();
        let db = db(&h);
        db.execute("WITH RECURSIVE n(x) AS (VALUES(1000) UNION ALL SELECT x+1 FROM n WHERE x<?2+999) INSERT INTO node_identity SELECT ?1,x,'example-project',printf('00000000-0000-4000-8000-%012x',x) FROM n", rusqlite::params![TEAM, archived]).unwrap();
        let columns = crate::teams::store::rows::NODE_COLUMNS.replacen("team,id,", "", 1);
        db.execute(&format!("INSERT INTO node_archive(team,id,{columns}) SELECT i.team,i.id,{columns} FROM node_identity i, (SELECT {columns} FROM node_archive WHERE team=?1 LIMIT 1) WHERE i.team=?1 AND i.id>=1000"), [TEAM]).unwrap();
        strip(&h);
        let steps = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = steps.clone();
        let started = std::time::Instant::now();
        let raw = rusqlite::Connection::open(h.temp.path().join("teams/teams-v1.sqlite3")).unwrap();
        raw.progress_handler(
            1000,
            Some(move || {
                counter.fetch_add(1000, std::sync::atomic::Ordering::Relaxed);
                false
            }),
        );
        super::super::schema::extend(&raw).unwrap();
        let elapsed = started.elapsed();
        let steps = steps.load(std::sync::atomic::Ordering::Relaxed);
        println!(
            "archived nodes={archived}: transition VM steps~{steps}, {:?}, {} steps per node",
            elapsed,
            steps / u64::from(archived)
        );
        costs.push(steps / u64::from(archived));
        reopen(&mut h);
    }
    assert!(
        costs[1] <= costs[0] * 2,
        "the walk is linear in the archive: {costs:?}"
    );
}

#[test]
fn a_store_whose_older_read_index_is_replaced_is_extended_too() {
    let mut h = stopped();
    let full = names(&h);
    strip(&h);
    db(&h)
        .execute_batch(
            "DROP INDEX integration_card;
             CREATE UNIQUE INDEX integration_card ON cards(decision_seq);",
        )
        .unwrap();
    reopen(&mut h);
    assert_eq!(names(&h), full);
    assert_eq!(marker(&h), Some(EXTENSION));
}

/// A store marked by the layout before the lead's answers (version 8) runs
/// the transition again and gains their table, index and trigger.
#[test]
fn a_store_marked_before_the_answers_gains_them_on_the_next_open() {
    let mut h = stopped();
    let full = names(&h);
    db(&h)
        .execute_batch(
            "DROP TRIGGER answer_mark_drop_v1; DROP INDEX inbox_questions; DROP TABLE answer_marks;",
        )
        .unwrap();
    db(&h)
        .execute("UPDATE timers_extension SET version=8", [])
        .unwrap();
    let before = transitions();
    reopen(&mut h);
    assert_eq!(transitions(), before + 1);
    assert_eq!(names(&h), full);
    assert_eq!(marker(&h), Some(EXTENSION));
}

/// A store marked by the answers' layout (version 9) gains the cards'
/// decisions column in one step; its cards answer by their kind.
#[test]
fn a_store_marked_before_the_card_decisions_gains_their_column() {
    let mut h = stopped();
    let full = schema(&h);
    db(&h)
        .execute_batch(
            "ALTER TABLE cards DROP COLUMN decisions; UPDATE timers_extension SET version=9;",
        )
        .unwrap();
    let before = transitions();
    reopen(&mut h);
    assert_eq!(transitions(), before + 1);
    assert_eq!(schema(&h), full, "the column, as a new store has it");
    assert_eq!(marker(&h), Some(EXTENSION));
}

/// A store marked by the hold owners' layout (version 11) gains the index
/// that reads its key refusals newest first in one step.
#[test]
fn a_store_marked_before_the_key_refusal_order_gains_its_index() {
    let mut h = stopped();
    let full = schema(&h);
    db(&h)
        .execute_batch("DROP INDEX key_refusals_newest; UPDATE timers_extension SET version=11;")
        .unwrap();
    let before = transitions();
    reopen(&mut h);
    assert_eq!(transitions(), before + 1);
    assert_eq!(schema(&h), full, "the index, as a new store has it");
    assert_eq!(marker(&h), Some(EXTENSION));
}

#[test]
fn a_marker_newer_than_this_program_refuses_the_open_and_writes_nothing() {
    let h = stopped();
    db(&h)
        .execute(
            "UPDATE timers_extension SET version=?1",
            [PINNED_EXTENSION + 1],
        )
        .unwrap();
    let full = names(&h);
    let before = transitions();
    let refused = Store::open(h.temp.path()).unwrap_err();
    assert_eq!(refused.code, code::INVALID_STATE);
    assert!(
        refused
            .message
            .contains("store schema is newer than this version of Zerus"),
        "{}",
        refused.message
    );
    assert_eq!(transitions(), before, "no transition");
    assert_eq!(marker(&h), Some(PINNED_EXTENSION + 1), "no lowered mark");
    assert_eq!(names(&h), full);
}

type Schema = Vec<(String, String, Option<String>)>;
/// Every schema object's definition, to compare a store byte for byte.
fn schema(h: &Harness) -> Schema {
    schema_of(db(h))
}
fn schema_of(db: &Connection) -> Schema {
    let mut s = db
        .prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")
        .unwrap();
    let rows = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
}
fn older_card_index(h: &Harness) {
    db(h).execute_batch("DROP INDEX integration_card; CREATE UNIQUE INDEX integration_card ON cards(decision_seq);").unwrap();
}

#[test]
fn member_readability_binding_is_checked_as_is_and_repaired_by_ordinary_open() {
    let h = Harness::new();
    let installed = schema(&h);
    let timer_version = marker(&h);
    db(&h)
        .execute_batch("DROP TRIGGER team_member_readability_binding_v1;")
        .unwrap();
    let incomplete = schema(&h);
    let refusal = Store::person_evidence(h.temp.path()).unwrap_err();
    assert_eq!(refusal.code, code::INVALID_STATE);
    assert!(refusal
        .message
        .contains("missing trigger team_member_readability_binding_v1"));
    assert_eq!(schema(&h), incomplete);

    let reopened = Store::open(h.temp.path()).unwrap();
    assert_eq!(schema_of(reopened.db.as_ref().unwrap()), installed);
    assert_eq!(marker(&h), timer_version);
    Store::person_evidence(h.temp.path()).unwrap();
}

#[test]
fn a_store_marked_by_a_newer_program_is_refused_before_any_schema_write() {
    let h = stopped();
    db(&h)
        .execute(
            "UPDATE timers_extension SET version=?1",
            [PINNED_EXTENSION + 1],
        )
        .unwrap();
    older_card_index(&h);
    db(&h)
        .execute_batch("DROP TABLE verdicts; DROP INDEX teams_live;")
        .unwrap();
    let before = schema(&h);
    let refused = Store::open(h.temp.path()).unwrap_err();
    assert!(
        refused
            .message
            .contains("store schema is newer than this version of Zerus"),
        "{}",
        refused.message
    );
    assert_eq!(
        schema(&h),
        before,
        "the index, the schema batches and the mark are untouched"
    );
    assert_eq!(marker(&h), Some(PINNED_EXTENSION + 1));
}

#[test]
fn an_older_store_with_the_older_index_still_upgrades() {
    let mut h = stopped();
    let full = schema(&h);
    strip(&h);
    older_card_index(&h);
    reopen(&mut h);
    assert_eq!(schema(&h), full);
    assert_eq!(marker(&h), Some(EXTENSION));
}

/// Another program's commit right after this open's first, unlocked fence;
/// the schema it leaves is returned through the cell.
fn after_the_fence(h: &Harness, sql: String) -> std::rc::Rc<std::cell::RefCell<Schema>> {
    let file = h.temp.path().join("teams/teams-v1.sqlite3");
    let left = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = left.clone();
    AFTER_FENCE.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let other = Connection::open(file).unwrap();
            other.execute_batch(&sql).unwrap();
            *seen.borrow_mut() = schema_of(&other);
            Ok(())
        }))
    });
    left
}
fn refused_as_newer(h: &Harness) {
    let refused = Store::open(h.temp.path()).unwrap_err();
    assert!(
        refused
            .message
            .contains("store schema is newer than this version of Zerus"),
        "{}",
        refused.message
    );
}

/// The reviewer's interleaving: a newer program commits its mark and its own
/// unique index between this open's first fence and the index conversion.
#[test]
fn a_mark_committed_after_the_first_fence_stops_the_index_conversion() {
    let h = stopped();
    let left = after_the_fence(
        &h,
        format!(
            "BEGIN IMMEDIATE; UPDATE timers_extension SET version={}; DROP INDEX integration_card; CREATE UNIQUE INDEX integration_card ON cards(decision_seq); COMMIT;",
            PINNED_EXTENSION + 1
        ),
    );
    refused_as_newer(&h);
    assert!(!left.borrow().is_empty(), "the other program committed");
    assert_eq!(schema(&h), *left.borrow(), "byte for byte what it left");
}

/// The same race before the common batches: a newer program that removed
/// objects they would create again.
#[test]
fn a_mark_committed_after_the_first_fence_stops_the_common_batches() {
    let h = stopped();
    let left = after_the_fence(
        &h,
        format!(
            "BEGIN IMMEDIATE; UPDATE timers_extension SET version={}; DROP TABLE verdicts; DROP INDEX teams_live; COMMIT;",
            PINNED_EXTENSION + 1
        ),
    );
    refused_as_newer(&h);
    assert!(!left.borrow().is_empty(), "the other program committed");
    assert_eq!(schema(&h), *left.borrow(), "byte for byte what it left");
}

#[test]
fn a_common_object_a_store_lacks_is_created_under_the_lock() {
    let mut h = stopped();
    let full = schema(&h);
    db(&h)
        .execute_batch("DROP TABLE verdicts; DROP INDEX teams_live;")
        .unwrap();
    reopen(&mut h);
    assert_eq!(schema(&h), full);
}

/// The open looks only for the objects of `CREATE ... IF NOT EXISTS`
/// statements: every statement of the common batches must be one.
#[test]
fn every_statement_of_the_common_batches_is_seen_by_the_presence_check() {
    for sql in crate::teams::store::COMMON_SCHEMA {
        let statements = sql.matches(';').count();
        assert_eq!(crate::teams::store::created(sql).len(), statements, "{sql}");
    }
}

/// Every table's rows, to compare a store's content byte for byte.
fn content(h: &Harness) -> Vec<String> {
    let db = db(h);
    let tables: Vec<String> = column(
        db,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        [],
    )
    .unwrap();
    let mut rows = Vec::new();
    for table in tables {
        let mut s = db
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .unwrap();
        let n = s.column_count();
        let mut q = s.query([]).unwrap();
        while let Some(r) = q.next().unwrap() {
            let values: Vec<String> = (0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect();
            rows.push(format!("{table}: {}", values.join(", ")));
        }
    }
    rows
}

/// Every lower mark was written only by an unmerged development build of
/// this package: the open is refused with a sentence that says so, and the
/// store is left as it was.
#[test]
fn every_development_mark_is_refused_and_writes_nothing() {
    for mark in 0..BASE {
        let h = stopped();
        db(&h)
            .execute("UPDATE timers_extension SET version=?1", [mark])
            .unwrap();
        let (before, rows) = (schema(&h), content(&h));
        let refused = Store::open(h.temp.path()).unwrap_err();
        assert_eq!(refused.code, code::INVALID_STATE, "{mark}");
        assert!(
            refused.message.contains("unmerged development build"),
            "{mark}: {}",
            refused.message
        );
        assert_eq!(schema(&h), before, "{mark}");
        assert_eq!(content(&h), rows, "{mark}");
    }
}

/// A store at the current layout opens without a write: no transition, and
/// its schema and rows as they were, while another connection holds the
/// write lock.
#[test]
fn the_current_mark_opens_without_a_write() {
    let mut h = stopped();
    let (before, rows) = (schema(&h), content(&h));
    let raw = rusqlite::Connection::open(h.temp.path().join("teams/teams-v1.sqlite3")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let transitions_before = transitions();
    reopen(&mut h);
    raw.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(transitions(), transitions_before);
    assert_eq!((schema(&h), content(&h)), (before, rows));
}

/// A Return as an earlier layout admitted it while the hand-out was only
/// reserved, when the holder was the newest effect whatever its state. It is
/// decided through the operation with the effect read as delivered, then the
/// effect is written back to reserved; with `stranded` false it stays
/// delivered and the narrowed holder still reads its member.
fn old_return(h: &mut Harness, stranded: bool) -> (u32, u64, Actor) {
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    let id = h.intent(node, "m1");
    h.observe(node, crate::teams::ops::TaskStage::InReview);
    let effect = |h: &Harness, state: &str| {
        db(h)
            .execute(
                "UPDATE effects SET state=?2 WHERE request_id=?1",
                params![id, state],
            )
            .unwrap();
    };
    effect(h, "submitted");
    h.run(
        &lead,
        Operation::Return {
            node,
            reason: "Fix the edge case".into(),
        },
    )
    .unwrap();
    if stranded {
        effect(h, "reserved");
    }
    let seq = h
        .store
        .decisions(TEAM, 200, None)
        .unwrap()
        .rows
        .last()
        .unwrap()
        .seq;
    (node, seq, lead)
}
fn to_layout_9(h: &Harness) {
    db(h)
        .execute_batch(
            "ALTER TABLE cards DROP COLUMN decisions; UPDATE timers_extension SET version=9;",
        )
        .unwrap();
}
fn handed_back(h: &Harness) -> Vec<String> {
    h.store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .map(|i| i.kind)
        .filter(|k| k.starts_with("return_without_holder:"))
        .collect()
}
fn revision(h: &Harness) -> u64 {
    h.store.team(TEAM).unwrap().unwrap().revision
}

/// The step to layout 10 hands back a Return that the narrowed holder
/// leaves with nobody to go to: settled, its node held, one item for the
/// lead, the team's revision moved, nothing left for the reopen page, and
/// the lead may decide again. Run again (the mark set back to 9), the step
/// finds nothing and writes nothing.
#[test]
fn the_step_to_layout_10_hands_a_return_without_holder_back_to_the_lead_once() {
    let mut h = Harness::new();
    let (node, seq, lead) = old_return(&mut h, true);
    assert!(h.store.attempt_holder(TEAM, node).unwrap().is_none());
    to_layout_9(&h);
    let before = (transitions(), revision(&h));
    reopen(&mut h);
    assert_eq!(transitions(), before.0 + 1);
    assert_eq!(marker(&h), Some(EXTENSION));
    let decision = h.store.decisions(TEAM, 200, None).unwrap().rows;
    assert_eq!(decision.last().unwrap().seq, seq);
    assert_eq!(decision.last().unwrap().state, DecisionState::Settled);
    assert!(h.node(node).hold, "held, as a refused reopen holds it");
    assert_eq!(handed_back(&h), [format!("return_without_holder:{seq}")]);
    assert_eq!(revision(&h), before.1 + 1);
    assert!(h
        .store
        .pending_reopens(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    // The step run again finds nothing and writes nothing.
    db(&h)
        .execute_batch("UPDATE timers_extension SET version=9;")
        .unwrap();
    let again = (transitions(), revision(&h));
    reopen(&mut h);
    assert_eq!(transitions(), again.0 + 1);
    assert_eq!(handed_back(&h).len(), 1, "one item, once");
    assert_eq!(revision(&h), again.1);
    // The lead decides again.
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m2".into(),
            reason: "Give it to the second member".into(),
        },
    )
    .unwrap();
}

/// A Return whose holder the narrowed rule still reads, and a Return whose
/// reopen is already reserved (it has started; its outcome settles it), are
/// left as they are by the step.
#[test]
fn the_step_to_layout_10_leaves_a_held_or_started_return_alone() {
    for case in ["held", "started"] {
        let mut h = Harness::new();
        let (node, seq, _) = old_return(&mut h, false);
        if case == "started" {
            h.reopen(node, "m1");
            assert!(h.store.attempt_holder(TEAM, node).unwrap().is_none());
        }
        let state = |h: &Harness| {
            h.store
                .decisions(TEAM, 200, None)
                .unwrap()
                .rows
                .into_iter()
                .find(|d| d.seq == seq)
                .unwrap()
                .state
        };
        let before = (state(&h), revision(&h), h.node(node).hold);
        to_layout_9(&h);
        reopen(&mut h);
        assert_eq!(marker(&h), Some(EXTENSION));
        assert_eq!(
            (state(&h), revision(&h), h.node(node).hold),
            before,
            "{case}"
        );
        assert_ne!(state(&h), DecisionState::Settled, "{case}");
        assert!(handed_back(&h).is_empty(), "{case}");
    }
}

/// The step to layout 11 gives each stored hold an owner: a held node with
/// an unsettled decision held for that decision, which now holds it by being
/// pending; any other held node, live or archived, the person's.
#[test]
fn the_step_to_layout_11_gives_each_stored_hold_its_owner() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let decided = h.add(&person(), 1);
    h.run(
        &lead,
        Operation::Cancel {
            node: decided,
            reason: "Not needed".into(),
        },
    )
    .unwrap();
    let held = h.add(&person(), 2);
    let archived = h.add(&person(), 3);
    h.cancel_fact(archived);
    let stored = |h: &Harness, table: &str, node: u32| -> (bool, Option<String>, bool) {
        db(h)
            .query_row(
                &format!("SELECT hold,hold_owner,hand_back FROM {table} WHERE team=?1 AND id=?2"),
                params![TEAM, node],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    let in_archive: bool = db(&h)
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM node_archive WHERE team=?1 AND id=?2)",
            params![TEAM, archived],
            |r| r.get(0),
        )
        .unwrap();
    assert!(in_archive);
    db(&h)
        .execute_batch(&format!(
            "UPDATE nodes SET hold=1 WHERE team='{TEAM}' AND id IN ({decided},{held});
             UPDATE node_archive SET hold=1 WHERE team='{TEAM}' AND id={archived};
             ALTER TABLE nodes DROP COLUMN hand_back; ALTER TABLE node_archive DROP COLUMN hand_back;
             ALTER TABLE nodes DROP COLUMN hold_owner; ALTER TABLE node_archive DROP COLUMN hold_owner;
             UPDATE timers_extension SET version=10;"
        ))
        .unwrap();
    let before = transitions();
    reopen(&mut h);
    assert_eq!(transitions(), before + 1);
    assert_eq!(marker(&h), Some(EXTENSION));
    assert_eq!(stored(&h, "nodes", decided), (false, None, false));
    assert!(h.node(decided).hold, "held by its pending Cancel");
    assert_eq!(
        stored(&h, "nodes", held),
        (true, Some("person".into()), false)
    );
    assert_eq!(
        stored(&h, "node_archive", archived),
        (true, Some("person".into()), false)
    );
    refused_release(&mut h, &lead, held);
}
fn refused_release(h: &mut Harness, lead: &Actor, node: u32) {
    let error = h.run(lead, Operation::Release { node }).unwrap_err();
    assert_eq!(error.code, zerus_team_plan::problem::code::NOT_ALLOWED);
    h.run(&person(), Operation::Release { node }).unwrap();
    assert!(!h.node(node).hold);
}

/// A trace alone proves nothing: a hand-back's item, effect or pipeline step
/// that no journal record places (here none of them has the operation's row
/// that made it) leaves the hold the person's, whatever the trace's kind or
/// generation, and the lead's Release says why it is refused. The stores the
/// old commits wrote are in `legacy_holds`.
#[test]
fn a_hand_back_trace_without_its_record_leaves_the_hold_the_persons() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    for member in ["m1", "m2", "m3"] {
        h.bind(member);
    }
    let mut nodes = Vec::new();
    for n in 1..=15 {
        nodes.push(h.add(&person(), n));
    }
    let kinds = [
        "return_without_holder:91",
        "member_removed:92",
        "conflict_holder_removed:93",
        "verdict_holder_removed:94",
        "cancel_refused:95",
    ];
    let actor = r#"{"kind":"service","person":"local","team":"t"}"#;
    for (node, kind) in nodes.iter().zip(kinds) {
        db(&h)
            .execute(
                "INSERT INTO inbox(team,kind,node,generation,actor,text,created_ms) VALUES(?1,?2,?3,1,?4,'Handed back',1)",
                params![TEAM, kind, node, actor],
            )
            .unwrap();
    }
    // A reopen that was not delivered; the delivered one (node 12) is no trace.
    for (node, member, state) in [
        (nodes[5], "m1", "refused"),
        (nodes[6], "m1", "uncertain"),
        (nodes[7], "m2", "cutting_over"),
        (nodes[12], "m3", "submitted"),
    ] {
        db(&h)
            .execute(
                "INSERT INTO effects(team,node,member,generation,request_id,actor,kind,created_ms,state) VALUES(?1,?2,?3,1,?4,?5,'reopen',1,?6)",
                params![TEAM, node, member, uuid::Uuid::new_v4().to_string(), actor, state],
            )
            .unwrap();
    }
    // A pipeline waiting on the person or on a stage; one active without a
    // stage (node 13) is no trace.
    for (node, state, stage) in [
        (nodes[8], "person", None),
        (nodes[9], "active", Some(nodes[14])),
        (nodes[13], "active", None),
    ] {
        db(&h)
            .execute(
                "INSERT INTO pipeline_runs(team,node,generation,state,stage) VALUES(?1,?2,1,?3,?4)",
                params![TEAM, node, state, stage],
            )
            .unwrap();
    }
    // An earlier generation's item is no trace of the hold it has now, and
    // an uncertain delivery's item is filed by ordinary hand-outs too.
    db(&h)
        .execute_batch(&format!(
            "UPDATE nodes SET generation=2 WHERE team='{TEAM}' AND id={};
             INSERT INTO inbox(team,kind,node,generation,actor,text,created_ms) VALUES('{TEAM}','cancel_refused:96',{},1,'{actor}','Old',1);
             INSERT INTO inbox(team,kind,node,generation,actor,text,created_ms) VALUES('{TEAM}','delivery_uncertain',{},1,'{actor}','Unknown',1);",
            nodes[11], nodes[11], nodes[14]
        ))
        .unwrap();
    db(&h)
        .execute_batch(&format!(
            "UPDATE nodes SET hold=1 WHERE team='{TEAM}';
             ALTER TABLE nodes DROP COLUMN hand_back; ALTER TABLE node_archive DROP COLUMN hand_back;
             ALTER TABLE nodes DROP COLUMN hold_owner; ALTER TABLE node_archive DROP COLUMN hold_owner;
             UPDATE timers_extension SET version=10;"
        ))
        .unwrap();
    reopen(&mut h);
    assert_eq!(marker(&h), Some(EXTENSION));
    let stored = |h: &Harness, node: u32| -> (bool, Option<String>, bool) {
        db(h)
            .query_row(
                "SELECT hold,hold_owner,hand_back FROM nodes WHERE team=?1 AND id=?2",
                params![TEAM, node],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    let persons = (true, Some("person".to_owned()), false);
    for (i, node) in nodes.iter().enumerate() {
        assert_eq!(stored(&h, *node), persons, "node {i}");
        assert!(h.node(*node).hold, "node {i}");
    }
    let error = h
        .run(&lead, Operation::Release { node: nodes[0] })
        .unwrap_err();
    assert!(
        error.message.contains("before holds had owners"),
        "{}",
        error.message
    );
}

/// A store that is really at layout 9 (no hold owner, no hand-back column)
/// with a Return its holder no longer reads: the step to 10 hands it back in
/// that layout's own form, which no journal row records, so the step to 11
/// leaves it the person's: the lead's Release is refused, the person's ends
/// it.
#[test]
fn a_layout_9_store_with_a_stranded_return_opens_and_its_hold_is_the_persons() {
    let mut h = Harness::new();
    let (node, seq, lead) = old_return(&mut h, true);
    db(&h)
        .execute_batch(
            "ALTER TABLE cards DROP COLUMN decisions;
             ALTER TABLE nodes DROP COLUMN hand_back; ALTER TABLE node_archive DROP COLUMN hand_back;
             ALTER TABLE nodes DROP COLUMN hold_owner; ALTER TABLE node_archive DROP COLUMN hold_owner;
             UPDATE timers_extension SET version=9;",
        )
        .unwrap();
    reopen(&mut h);
    assert_eq!(marker(&h), Some(EXTENSION));
    assert_eq!(handed_back(&h), [format!("return_without_holder:{seq}")]);
    let stored: (bool, Option<String>, bool) = db(&h)
        .query_row(
            "SELECT hold,hold_owner,hand_back FROM nodes WHERE team=?1 AND id=?2",
            params![TEAM, node],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (true, Some("person".to_owned()), false),
        "the person's"
    );
    let error = h.run(&lead, Operation::Release { node }).unwrap_err();
    assert_eq!(error.code, code::NOT_ALLOWED);
    h.run(&person(), Operation::Release { node }).unwrap();
    assert!(!h.node(node).hold);
}
