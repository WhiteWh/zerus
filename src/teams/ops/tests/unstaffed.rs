//! zerus-63rru.45: a ready node that no member can take is told to the lead
//! once, and the item goes when a member can take the node again.
use super::*;
use crate::teams::store::{InboxItem, JournalEntry};

fn reconcile(h: &mut Harness) -> Result<Reply> {
    h.run(&service(), Operation::ReconcileUnstaffed)
}
fn unchanged(h: &mut Harness) {
    let e = reconcile(h).unwrap_err();
    assert_eq!(e.code, code::UNSTAFFED_UNCHANGED, "{e:?}");
}
/// The open unstaffed items, `(node, kind, text)`.
fn open(h: &Harness) -> Vec<(u32, String, String)> {
    h.store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|i| i.state == "open" && i.kind.starts_with("unstaffed:"))
        .map(|i| (i.node, i.kind, i.text))
        .collect()
}
/// The items the reconcile ended, `(node, reply)`.
fn ended(h: &Harness) -> Vec<(u32, String)> {
    h.store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|i| i.kind.starts_with("unstaffed:") && i.reply.is_some())
        .map(|i| (i.node, i.reply.unwrap()))
        .collect()
}
/// Every unstaffed item, ended or not.
fn all(h: &Harness) -> usize {
    let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
    rows.iter()
        .filter(|i| i.kind.starts_with("unstaffed:"))
        .count()
}
/// The lead closing its open unstaffed items. No operation closes one for
/// the lead yet; every closer but the reconcile leaves the reply empty, as
/// this does.
fn closed_by_lead(h: &Harness) {
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE inbox SET state='closed' WHERE team=?1 AND state='open' AND kind LIKE 'unstaffed:%'",
            [TEAM],
        )
        .unwrap();
}
fn row(h: &Harness, node: u32) -> Option<String> {
    h.store
        .queue_rows(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|r| r.node.id == node)
        .unwrap()
        .unstaffed
}
fn revision(h: &Harness) -> u64 {
    h.store.team(TEAM).unwrap().unwrap().revision
}

#[test]
fn member_readability_refuses_wrong_callers_bindings_and_oversized_batches() {
    use crate::teams::store::member_readability::MAX_READINGS;

    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let reading = |h: &Harness, member: &str| MemberReading {
        member: member.into(),
        session: h
            .store
            .member(TEAM, member)
            .unwrap()
            .unwrap()
            .session
            .unwrap(),
        unreadable: true,
    };
    let m1 = reading(&h, "m1");
    let m2 = reading(&h, "m2");
    let mut stale = m2.clone();
    stale.session = m1.session.clone();
    let lead_reading = reading(&h, "lead");
    let start = revision(&h);
    for (who, readings, expected) in [
        (lead, vec![m1.clone()], code::NOT_ALLOWED),
        (person(), vec![m1.clone()], code::NOT_ALLOWED),
        (service(), vec![m1.clone(), stale], code::STALE_REVISION),
        (service(), vec![m1.clone(), m1.clone()], code::INVALID_STATE),
        (service(), vec![lead_reading], code::INVALID_STATE),
        (
            service(),
            vec![m1.clone(); MAX_READINGS + 1],
            code::OVER_CAP,
        ),
    ] {
        let rejected = h
            .run(&who, Operation::RecordMemberReadability { readings })
            .unwrap_err();
        assert_eq!(rejected.code, expected);
        assert_eq!(revision(&h), start);
        assert!(
            h.store.member_readability(TEAM).unwrap().is_empty(),
            "a refused batch rolls back"
        );
    }
    let op = Operation::RecordMemberReadability {
        readings: vec![m1, m2],
    };
    h.run(&service(), op.clone()).unwrap();
    assert_eq!(revision(&h), start + 1);
    assert_eq!(h.store.member_readability(TEAM).unwrap().len(), 2);
    assert_eq!(h.run(&service(), op).unwrap_err().code, code::INVALID_STATE);
    assert_eq!(revision(&h), start + 1);
}

#[test]
fn a_member_state_change_ends_the_previous_readability_fact() {
    let mut h = Harness::new();
    h.bind("m1");
    let session = h
        .store
        .member(TEAM, "m1")
        .unwrap()
        .unwrap()
        .session
        .unwrap();
    let record = |h: &mut Harness, now| {
        let read = request(
            revision(h),
            Operation::RecordMemberReadability {
                readings: vec![MemberReading {
                    member: "m1".into(),
                    session: session.clone(),
                    unreadable: true,
                }],
            },
        );
        apply_fixture(&mut h.store, &service(), &read, now).unwrap();
    };
    record(&mut h, 100);
    record(&mut h, 180_100);
    assert_eq!(
        h.store.unreadable_members(TEAM).unwrap(),
        ["m1".into()].into()
    );
    h.run(
        &person(),
        Operation::PauseMember {
            member: "m1".into(),
        },
    )
    .unwrap();
    assert!(h.store.member_readability(TEAM).unwrap().is_empty());
    h.run(
        &person(),
        Operation::ResumeMember {
            member: "m1".into(),
        },
    )
    .unwrap();
    record(&mut h, 181_100);
    assert!(h.store.unreadable_members(TEAM).unwrap().is_empty());
}

fn removed(h: &mut Harness, member: &str) {
    h.run(
        &person(),
        Operation::ArchiveMember {
            member: member.into(),
        },
    )
    .unwrap();
}
fn snapshot(h: &Harness) -> (u64, Vec<JournalEntry>, Vec<InboxItem>) {
    (
        revision(h),
        h.store.journal(TEAM, 200, None).unwrap().rows,
        h.store.inbox(TEAM, 200, None).unwrap().rows,
    )
}
/// A Reassign settled at once (the task was never handed out), then its
/// assignee removed: the first probe of the survey.
fn stranded_assignee(h: &mut Harness) -> (u32, Actor) {
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m2".into(),
            reason: "Give it to m2".into(),
        },
    )
    .unwrap();
    removed(h, "m2");
    (node, lead)
}

#[test]
fn a_settled_reassigns_removed_assignee_is_told_to_the_lead_once() {
    let mut h = Harness::new();
    let (node, _) = stranded_assignee(&mut h);
    assert!(open(&h).is_empty(), "nothing told before the reconcile");
    let before = revision(&h);
    reconcile(&mut h).unwrap();
    let items = open(&h);
    assert_eq!(items.len(), 1);
    let (n, kind, text) = &items[0];
    assert_eq!(*n, node);
    assert_eq!(kind, &format!("unstaffed:{}", before + 1));
    assert_eq!(
        text,
        &format!("Task {node} is ready, but no member can take it: The assignee m2 is not a member with the role developer. Route it to another role, reassign or cancel it.")
    );
    assert_eq!(row(&h, node).as_deref(), Some(text.as_str()));
    // Told once: a second reconcile finds nothing to change.
    let after = snapshot(&h);
    unchanged(&mut h);
    assert_eq!(snapshot(&h), after);
}

#[test]
fn a_role_whose_members_are_all_removed_is_told_to_the_lead() {
    let mut h = Harness::new();
    h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    removed(&mut h, "m1");
    removed(&mut h, "m2");
    reconcile(&mut h).unwrap();
    let items = open(&h);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, node);
    assert!(items[0]
        .2
        .contains("No member of the team has the role developer."));
}

/// One removal strands two nodes: one reconcile, one item each, under the
/// same kind; the inbox keys an item by team, kind, node and generation.
#[test]
fn two_nodes_stranded_by_one_removal_get_one_item_each() {
    let mut h = Harness::new();
    h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let a = h.add(&person(), 1);
    let b = h.add(&person(), 2);
    removed(&mut h, "m1");
    removed(&mut h, "m2");
    let before = revision(&h);
    reconcile(&mut h).unwrap();
    let items = open(&h);
    assert_eq!(items.iter().map(|i| i.0).collect::<Vec<_>>(), vec![a, b]);
    assert!(items
        .iter()
        .all(|i| i.1 == format!("unstaffed:{}", before + 1)));
    let seqs: std::collections::BTreeSet<u64> = h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|i| i.kind.starts_with("unstaffed:"))
        .map(|i| i.seq)
        .collect();
    assert_eq!(seqs.len(), 2);
}

/// Each way a node leaves the set closes its item, and the row clears: a
/// Route, a Reassign, a hold, and the task leaving the team.
#[test]
fn a_node_fixed_by_route_reassign_hold_or_leaving_closes_its_item() {
    for fix in ["route", "reassign", "hold", "leave"] {
        let mut h = Harness::new();
        let (node, lead) = stranded_assignee(&mut h);
        reconcile(&mut h).unwrap();
        assert_eq!(open(&h).len(), 1, "{fix}");
        match fix {
            "route" => {
                h.run(&lead, route(node)).unwrap();
            }
            "reassign" => {
                h.run(
                    &lead,
                    Operation::Reassign {
                        node,
                        member: "m1".into(),
                        reason: "m1 takes it".into(),
                    },
                )
                .unwrap();
            }
            "hold" => {
                h.run(&lead, Operation::Hold { node }).unwrap();
            }
            _ => {
                let mut o = h.observation(node, TaskStage::Todo);
                o.key = KeyState::Absent;
                h.run(
                    &service(),
                    Operation::ObserveTask {
                        node,
                        observation: o,
                    },
                )
                .unwrap();
            }
        }
        if fix == "leave" {
            // The node's end already closed its item; only the reconcile
            // ends the episode, once.
            assert!(open(&h).is_empty(), "{fix}");
            reconcile(&mut h).unwrap();
            assert_eq!(ended(&h).len(), 1, "{fix}");
            unchanged(&mut h);
        } else {
            reconcile(&mut h).unwrap();
            assert!(open(&h).is_empty(), "{fix}");
            assert_eq!(row(&h, node), None, "{fix}");
            unchanged(&mut h);
        }
    }
}

/// A node that is fixed and then stranded again is told again, once.
#[test]
fn a_node_stranded_again_is_told_again_once() {
    let mut h = Harness::new();
    let (node, lead) = stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    let first = open(&h)[0].1.clone();
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m1".into(),
            reason: "m1 takes it".into(),
        },
    )
    .unwrap();
    reconcile(&mut h).unwrap();
    assert!(open(&h).is_empty());
    removed(&mut h, "m1");
    reconcile(&mut h).unwrap();
    let again = open(&h);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].0, node);
    assert_ne!(again[0].1, first);
    // m2 went first, so no developer is left: the kernel names the role.
    assert!(again[0]
        .2
        .contains("No member of the team has the role developer."));
    unchanged(&mut h);
}

/// A hundred passes that find nothing to change write nothing: the revision,
/// the journal and the inbox stay as they are.
#[test]
fn a_hundred_reconciles_with_nothing_changed_write_nothing() {
    let mut h = Harness::new();
    stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    let before = snapshot(&h);
    for _ in 0..100 {
        unchanged(&mut h);
    }
    assert_eq!(snapshot(&h), before);
}

/// A reconcile saved and replayed by its request ID answers its saved reply
/// and changes nothing.
#[test]
fn a_replayed_reconcile_answers_its_saved_reply_and_changes_nothing() {
    let mut h = Harness::new();
    stranded_assignee(&mut h);
    let r = request(revision(&h), Operation::ReconcileUnstaffed);
    let first = super::super::apply(&mut h.store, &service(), &r, 1234).unwrap();
    let after = snapshot(&h);
    let again = super::super::apply(&mut h.store, &service(), &r, 1234).unwrap();
    assert_eq!(again, first);
    assert_eq!(snapshot(&h), after);
}

/// Only the service reconciles, and only in a running team.
#[test]
fn only_the_service_reconciles_a_running_team() {
    let mut h = Harness::new();
    let (_, lead) = stranded_assignee(&mut h);
    refused(
        h.run(&lead, Operation::ReconcileUnstaffed),
        code::NOT_ALLOWED,
    );
    refused(
        h.run(&person(), Operation::ReconcileUnstaffed),
        code::NOT_ALLOWED,
    );
    h.run(&person(), Operation::Pause).unwrap();
    refused(reconcile(&mut h), code::INVALID_STATE);
    assert!(open(&h).is_empty());
}

/// One removal strands 500 ready nodes: one reconcile, 500 items. A 501st
/// told item makes the next reconcile fail closed with a named refusal, not
/// a silent cut, and the service's read fails the same way.
#[test]
fn a_removal_that_strands_500_nodes_is_one_reconcile_and_the_bound_fails_closed() {
    let mut h = Harness::new();
    h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let nodes: Vec<u32> = (1..=500).map(|n| h.add(&person(), n)).collect();
    removed(&mut h, "m1");
    removed(&mut h, "m2");
    reconcile(&mut h).unwrap();
    let told: Vec<u32> = h
        .store
        .told_unstaffed(TEAM)
        .unwrap()
        .into_iter()
        .map(|t| t.node)
        .collect();
    assert_eq!(told, nodes);
    let db = h.store.db.as_ref().unwrap();
    db.execute(
        "INSERT INTO inbox(team,kind,node,generation,actor,text,created_ms) VALUES(?1,'unstaffed:999999',9999,1,'{}','A forged 501st item',0)",
        params![TEAM],
    )
    .unwrap();
    let before = snapshot(&h);
    let e = reconcile(&mut h).unwrap_err();
    assert_eq!(e.code, code::OVER_CAP);
    assert!(
        e.message.contains("More than 500 told unstaffed items"),
        "{e:?}"
    );
    assert_eq!(snapshot(&h), before);
    let e = h.store.told_unstaffed(TEAM).unwrap_err();
    assert_eq!(e.code, code::OVER_CAP);
}

/// The plans and costs of the new reads at 1,000 and 10,000 settled items:
/// the told items and the queue page's `unstaffed` read the partial index
/// `inbox_unstaffed_told`; they scan no table, sort nothing, and execute the
/// same number of virtual machine steps whatever the ended history. `--nocapture` prints them.
#[test]
fn the_unstaffed_reads_cost_the_same_at_1000_and_10000_settled_items() {
    use rusqlite::StatementStatus;
    let mut h = Harness::new();
    h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let a = h.add(&person(), 1);
    h.add(&person(), 2);
    removed(&mut h, "m1");
    removed(&mut h, "m2");
    reconcile(&mut h).unwrap();
    let db = h.store.db.as_ref().unwrap();
    let page = crate::teams::store::pages::node_page_sql("nodes", false);
    let run = |sql: &str, page: bool| -> (usize, i32, i32) {
        let mut s = db.prepare(sql).unwrap();
        let rows = if page {
            s.query_map(params![TEAM, 0, 1_000_000, 200, ""], |_| Ok(()))
                .unwrap()
                .count()
        } else {
            s.query_map(params![TEAM], |_| Ok(())).unwrap().count()
        };
        (
            rows,
            s.get_status(StatementStatus::VmStep),
            s.get_status(StatementStatus::FullscanStep),
        )
    };
    let plan = |sql: &str| -> Vec<String> {
        let mut s = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![rusqlite::types::Null; s.parameter_count()];
        s.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let settle = |from: usize, to: usize| {
        for i in from..to {
            db.execute(
                "INSERT INTO inbox(team,kind,node,generation,actor,text,state,reply,reply_actor,created_ms) VALUES(?1,?2,?3,1,'{}','A settled episode','closed','Ended by the reconcile','{}',0),(?1,?4,?3,1,'{}','A settled report','closed',NULL,NULL,0)",
                params![TEAM, format!("unstaffed:settled-{i}"), a, format!("report:settled-{i}")],
            )
            .unwrap();
        }
    };
    let told_items = crate::teams::store::unstaffed::TOLD_ITEMS;
    let mut costs = Vec::new();
    for (from, to) in [(0, 1_000), (1_000, 10_000)] {
        settle(from, to);
        let items = run(told_items, false);
        let rows = run(&page, true);
        println!(
            "UNSTAFFED-COST settled={to}: told items rows={} vm_steps={} fullscan_steps={}; queue page rows={} vm_steps={} fullscan_steps={}",
            items.0, items.1, items.2, rows.0, rows.1, rows.2
        );
        assert_eq!(items.0, 2);
        assert_eq!(rows.0, 2);
        assert_eq!((items.2, rows.2), (0, 0), "no table scanned");
        costs.push((items.1, rows.1));
    }
    assert_eq!(costs[0], costs[1], "the cost does not grow with history");
    for (name, sql, index) in [
        ("TOLD_ITEMS", told_items, "inbox_unstaffed_told"),
        ("QUEUE_PAGE", page.as_str(), "inbox_unstaffed_told"),
    ] {
        let p = plan(sql);
        println!("UNSTAFFED-PLAN {name}:\n  {}", p.join("\n  "));
        assert!(
            p.iter().any(|l| l.contains(index)),
            "{name} reads the partial index {index}: {p:?}"
        );
        assert!(
            !p.iter().any(|l| l.contains("TEMP B-TREE")),
            "{name} sorts nothing: {p:?}"
        );
        assert!(
            !p.iter()
                .any(|l| l.starts_with("SCAN i") || l == "SCAN inbox"),
            "{name} scans no inbox: {p:?}"
        );
    }
}

/// Review 45, P2-2 (the reviewer's store reproducer): the gap changes while
/// the node stays unstaffed. The reconcile replaces the item once: it ends
/// the old one and tells the current reason.
#[test]
fn an_open_item_is_replaced_once_when_the_kernels_reason_changes() {
    let mut h = Harness::new();
    let (node, _) = stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    assert!(open(&h)[0].2.contains("The assignee m2"));
    removed(&mut h, "m1");
    reconcile(&mut h).unwrap();
    let current = open(&h);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].0, node);
    assert!(
        current[0]
            .2
            .contains("No member of the team has the role developer."),
        "the item kept the earlier reason: {}",
        current[0].2
    );
    assert_eq!(ended(&h).len(), 1);
    assert!(ended(&h)[0].1.contains("reason or attempt changed"));
    assert_eq!(all(&h), 2);
    unchanged(&mut h);
}

/// The reason changes twice: two replacements, one open item each time.
/// The assignee m2 is removed; then the last developer; then the node's
/// role becomes one nobody has (written directly: every built-in pipeline is
/// implemented by developers, so no Route reaches another role).
#[test]
fn a_reason_that_changes_twice_replaces_the_item_twice() {
    let mut h = Harness::new();
    let (node, _) = stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    assert!(open(&h)[0].2.contains("The assignee m2 is not a member"));
    removed(&mut h, "m1");
    reconcile(&mut h).unwrap();
    let first = open(&h);
    assert_eq!(first.len(), 1);
    assert!(first[0]
        .2
        .contains("No member of the team has the role developer."));
    unchanged(&mut h);
    let mut n = h.node(node);
    n.role = "tester".into();
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![TEAM, node, rows::node_payload(&n).unwrap()],
        )
        .unwrap();
    reconcile(&mut h).unwrap();
    let second = open(&h);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].0, node);
    assert!(
        second[0]
            .2
            .contains("No member of the team has the role tester."),
        "{}",
        second[0].2
    );
    assert_eq!(all(&h), 3);
    assert_eq!(ended(&h).len(), 2);
    let after = snapshot(&h);
    for _ in 0..100 {
        unchanged(&mut h);
    }
    assert_eq!(snapshot(&h), after);
}

/// Review 45, P2-1 at the store: an item the lead closed still stands for
/// its episode. A hundred reconciles tell nothing and move no revision.
#[test]
fn an_item_the_lead_closed_is_not_told_again_while_its_episode_lasts() {
    let mut h = Harness::new();
    let (node, _) = stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    for _ in 0..100 {
        closed_by_lead(&h);
        let before = snapshot(&h);
        unchanged(&mut h);
        assert_eq!(snapshot(&h), before);
    }
    assert!(open(&h).is_empty());
    assert_eq!(all(&h), 1);
    assert!(ended(&h).is_empty());
    let told = h.store.told_unstaffed(TEAM).unwrap();
    assert_eq!(told.iter().map(|t| t.node).collect::<Vec<_>>(), [node]);
}

/// A node that leaves the set ends its episode, whether its item was open
/// or the lead had closed it; its return is a new episode, told once.
#[test]
fn a_node_that_leaves_and_returns_is_a_new_episode_told_once() {
    for lead_closed in [false, true] {
        let mut h = Harness::new();
        let (node, lead) = stranded_assignee(&mut h);
        reconcile(&mut h).unwrap();
        if lead_closed {
            closed_by_lead(&h);
        }
        h.run(&lead, Operation::Hold { node }).unwrap();
        reconcile(&mut h).unwrap();
        assert!(open(&h).is_empty(), "lead_closed={lead_closed}");
        assert_eq!(ended(&h).len(), 1, "lead_closed={lead_closed}");
        assert!(ended(&h)[0].1.contains("a member can take the task again"));
        unchanged(&mut h);
        h.run(&lead, Operation::Release { node }).unwrap();
        reconcile(&mut h).unwrap();
        let again = open(&h);
        assert_eq!(again.len(), 1, "lead_closed={lead_closed}");
        assert_eq!(again[0].0, node);
        assert!(again[0].2.contains("The assignee m2"));
        assert_eq!(all(&h), 2, "lead_closed={lead_closed}");
        unchanged(&mut h);
    }
}

/// The told episodes are one comparison for the store and the service: a
/// told item that is not its node's newest, or whose generation moved, is
/// ended, and its node is told anew.
#[test]
fn the_comparison_ends_an_older_item_and_a_moved_generation() {
    use crate::teams::store::unstaffed::Told;
    use zerus_team_plan::ready::Unstaffed;
    let now = [Unstaffed {
        node: 1,
        reason: "No member of the team has the role developer.".into(),
        unreadable: false,
    }];
    let text = "Task 1 is ready, but no member can take it: No member of the team has the role developer. Route it to another role, reassign or cancel it.";
    let told = |seq, generation, current| Told {
        seq,
        node: 1,
        generation,
        current,
        text: text.into(),
    };
    let none = std::collections::BTreeSet::new();
    let standing = crate::teams::ops::unstaffed_changes(&now, &none, &[told(7, 1, Some(1))]);
    assert!(standing.is_empty());
    let older = crate::teams::ops::unstaffed_changes(
        &now,
        &none,
        &[told(5, 1, Some(1)), told(7, 1, Some(1))],
    );
    assert_eq!(older.end.iter().map(|e| e.0).collect::<Vec<_>>(), [5]);
    assert!(older.tell.is_empty());
    let moved = crate::teams::ops::unstaffed_changes(&now, &none, &[told(7, 1, Some(2))]);
    assert_eq!(moved.end.iter().map(|e| e.0).collect::<Vec<_>>(), [7]);
    assert_eq!(moved.tell.len(), 1);
    let stage: std::collections::BTreeSet<u32> = [1].into();
    let retired = crate::teams::ops::unstaffed_changes(&now, &stage, &[]);
    assert!(retired.tell.is_empty());
    assert_eq!(retired.stages.len(), 1);
}

/// The row's `unstaffed` is the node's current episode: shown while the item
/// is open, still shown after the lead closed it, the new sentence after a
/// replacement, and blank once the reconcile ended the episode.
#[test]
fn the_row_shows_the_current_episode_whether_or_not_the_lead_closed_its_item() {
    let mut h = Harness::new();
    let (node, lead) = stranded_assignee(&mut h);
    assert_eq!(row(&h, node), None, "not told yet");
    reconcile(&mut h).unwrap();
    let first = open(&h)[0].2.clone();
    assert_eq!(row(&h, node).as_deref(), Some(first.as_str()), "item open");
    closed_by_lead(&h);
    assert!(open(&h).is_empty());
    assert_eq!(
        row(&h, node).as_deref(),
        Some(first.as_str()),
        "item closed by the lead"
    );
    removed(&mut h, "m1");
    reconcile(&mut h).unwrap();
    let second = open(&h)[0].2.clone();
    assert_ne!(second, first);
    assert_eq!(row(&h, node).as_deref(), Some(second.as_str()), "replaced");
    h.run(&lead, Operation::Hold { node }).unwrap();
    assert_eq!(
        row(&h, node).as_deref(),
        Some(second.as_str()),
        "until the reconcile"
    );
    reconcile(&mut h).unwrap();
    assert_eq!(row(&h, node), None, "the episode ended");
}

/// A Reply cannot forge the reconcile's mark: Reply answers questions only,
/// so a lead's or a person's Reply to an unstaffed item, open or closed by
/// the lead, is refused and writes nothing, and the episode stands.
#[test]
fn a_reply_to_an_unstaffed_item_is_refused_and_leaves_the_episode() {
    let mut h = Harness::new();
    let (_, lead) = stranded_assignee(&mut h);
    reconcile(&mut h).unwrap();
    let item = h
        .store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .find(|i| i.kind.starts_with("unstaffed:"))
        .unwrap()
        .seq;
    for lead_closed in [false, true] {
        if lead_closed {
            closed_by_lead(&h);
        }
        for actor in [lead.clone(), person()] {
            let before = snapshot(&h);
            let e = h
                .run(
                    &actor,
                    Operation::Reply {
                        item,
                        text: "Done, this needs nothing.".into(),
                    },
                )
                .unwrap_err();
            assert_eq!(e.code, code::INVALID_STATE, "{e:?}");
            assert_eq!(snapshot(&h), before, "lead_closed={lead_closed}");
        }
        assert!(ended(&h).is_empty());
        unchanged(&mut h);
    }
}

/// The unended unstaffed items of the team, counted in the store itself:
/// the inbox page shows only its first 200 rows.
fn unended(h: &Harness) -> usize {
    h.store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM inbox WHERE team=?1 AND kind LIKE 'unstaffed:%' AND reply IS NULL",
            [TEAM],
            |r| r.get(0),
        )
        .unwrap()
}
fn open_count(h: &Harness) -> usize {
    h.store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM inbox WHERE team=?1 AND kind LIKE 'unstaffed:%' AND state='open'",
            [TEAM],
            |r| r.get(0),
        )
        .unwrap()
}
/// The delta review's legacy store: `episodes` Hold, reconcile, Release
/// rounds of one stranded node through this head's own operations, then
/// every item's reply removed by SQL. That is the layout the earlier head
/// dd1176fe1 left (each item closed, reply-less); this head cannot leave it
/// itself, because its reconcile ends every episode with a reply.
pub(super) fn legacy(h: &mut Harness, episodes: usize) -> u32 {
    let (node, _) = stranded_assignee(h);
    for episode in 0..episodes {
        if episode != 0 {
            h.run(&person(), Operation::Release { node }).unwrap();
        }
        reconcile(h).unwrap();
        h.run(&person(), Operation::Hold { node }).unwrap();
        reconcile(h).unwrap();
    }
    let stripped = h
        .store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE inbox SET state='closed',reply=NULL,reply_actor=NULL WHERE team=?1 AND kind LIKE 'unstaffed:%'",
            [TEAM],
        )
        .unwrap();
    assert_eq!(stripped, episodes);
    node
}

/// More than 500 stale items never wedge the reconcile: the cap counts the
/// current episodes, one per node; the stale items are ended a page at a
/// time in their own operation; the node's current episode is then the
/// reconcile's, and a return of the node is told once.
#[test]
fn stale_items_are_ended_a_page_at_a_time_and_the_lead_is_told_once() {
    let mut h = Harness::new();
    let node = legacy(&mut h, 501);
    assert_eq!(unended(&h), 501);
    // 500 stale items and one current episode: the cap judges one.
    let told = h.store.told_unstaffed(TEAM).unwrap();
    assert_eq!(told.len(), 1);
    assert!(h.store.stale_unstaffed(TEAM).unwrap());
    refused(
        h.run(&person(), Operation::EndStaleUnstaffed),
        code::NOT_ALLOWED,
    );
    for page in [200, 200, 100] {
        let before = (unended(&h), revision(&h));
        h.run(&service(), Operation::EndStaleUnstaffed).unwrap();
        assert_eq!(before.0 - unended(&h), page);
        assert_eq!(revision(&h), before.1 + 1);
    }
    assert!(!h.store.stale_unstaffed(TEAM).unwrap());
    let e = h.run(&service(), Operation::EndStaleUnstaffed).unwrap_err();
    assert_eq!(e.code, code::UNSTAFFED_UNCHANGED, "{e:?}");
    assert_eq!(h.store.told_unstaffed(TEAM).unwrap(), told);
    // The node is held: the reconcile ends its current episode.
    reconcile(&mut h).unwrap();
    assert_eq!(unended(&h), 0);
    unchanged(&mut h);
    // Its return is a new episode, told once.
    h.run(&person(), Operation::Release { node }).unwrap();
    reconcile(&mut h).unwrap();
    assert_eq!((unended(&h), open_count(&h)), (1, 1));
    unchanged(&mut h);
    // A paused team ends nothing.
    h.run(&person(), Operation::Pause).unwrap();
    refused(
        h.run(&service(), Operation::EndStaleUnstaffed),
        code::INVALID_STATE,
    );
}

/// The review's u2 pin: a stale item ends with the exact sentence and the
/// service as its reply actor. The sentence is spelled here once; the
/// product's constant is private to the operation.
#[test]
fn the_stale_ending_writes_the_exact_reply_and_its_actor() {
    let mut h = Harness::new();
    let node = legacy(&mut h, 2);
    assert_eq!(unended(&h), 2);
    h.run(&service(), Operation::EndStaleUnstaffed).unwrap();
    let db = h.store.db.as_ref().unwrap();
    let ended: (String, String) = db
        .query_row(
            "SELECT reply,reply_actor FROM inbox WHERE team=?1 AND kind LIKE 'unstaffed:%' AND reply IS NOT NULL",
            [TEAM],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        ended,
        (
            "The service ended this item: a newer item of the same task stands for it.".to_owned(),
            crate::teams::store::encode(&service()).unwrap(),
        ),
        "the reply and its actor, exactly"
    );
    assert_eq!(unended(&h), 1, "the node's newest item stays");
    let told = h.store.told_unstaffed(TEAM).unwrap();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].node, node);
}

/// The review's u6 pin: the newest of each node is the one kept, by sequence
/// and never by the clock — three nodes whose items interleave by sequence
/// and whose clocks are inverted against it.
#[test]
fn each_nodes_newest_is_kept_and_told_across_interleaved_nodes() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let nodes = [
        h.add(&person(), 1),
        h.add(&person(), 2),
        h.add(&person(), 3),
    ];
    for n in nodes {
        h.run(
            &lead,
            Operation::Reassign {
                node: n,
                member: "m2".into(),
                reason: "Give it to m2".into(),
            },
        )
        .unwrap();
    }
    removed(&mut h, "m2");
    // Rounds of the product's own episodes, round-robin so the items of the
    // three nodes interleave by sequence: the first reconcile tells all
    // three stranded nodes, then each node is held and released in turn.
    reconcile(&mut h).unwrap();
    for round in 0..2 {
        for n in nodes {
            if round != 0 {
                h.run(&person(), Operation::Release { node: n }).unwrap();
                reconcile(&mut h).unwrap();
            }
            h.run(&person(), Operation::Hold { node: n }).unwrap();
            reconcile(&mut h).unwrap();
        }
    }
    // The earlier head's layout: every reply removed by SQL, and the clocks
    // deliberately inverted against the sequence, so the newest item of each
    // node carries the oldest clock.
    let stripped: usize = {
        let db = h.store.db.as_ref().unwrap();
        let stripped = db
            .execute(
                "UPDATE inbox SET state='closed',reply=NULL,reply_actor=NULL WHERE team=?1 AND kind LIKE 'unstaffed:%'",
                [TEAM],
            )
            .unwrap();
        db.execute(
            "UPDATE inbox SET created_ms = 1000000 - seq * 10 WHERE team=?1 AND kind LIKE 'unstaffed:%'",
            [TEAM],
        )
        .unwrap();
        stripped
    };
    assert!(stripped >= 6, "{stripped}");
    h.run(&service(), Operation::EndStaleUnstaffed).unwrap();
    let db = h.store.db.as_ref().unwrap();
    // Exactly each node's newest unended item (the maximum sequence) is kept,
    // and it is the one told.
    let kept: Vec<(u32, u64)> = {
        let mut s = db
            .prepare(
                "SELECT node,seq FROM inbox WHERE team=?1 AND kind LIKE 'unstaffed:%' AND reply IS NULL ORDER BY node",
            )
            .unwrap();
        let rows = s
            .query_map([TEAM], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(kept.len(), 3, "one kept item per node: {kept:?}");
    for (n, seq) in &kept {
        let max: u64 = db
            .query_row(
                "SELECT max(seq) FROM inbox WHERE team=?1 AND node=?2 AND kind LIKE 'unstaffed:%'",
                rusqlite::params![TEAM, n],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(*seq, max, "node {n} keeps its newest by sequence");
        let kept_clock: i64 = db
            .query_row(
                "SELECT created_ms FROM inbox WHERE team=?1 AND node=?2 AND seq=?3",
                rusqlite::params![TEAM, n, seq],
                |r| r.get(0),
            )
            .unwrap();
        let stale_clock: i64 = db
            .query_row(
                "SELECT min(created_ms) FROM inbox WHERE team=?1 AND node=?2 AND seq<?3",
                rusqlite::params![TEAM, n, seq],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            kept_clock < stale_clock,
            "node {n}: the newest by sequence carries the oldest clock ({kept_clock} < {stale_clock})"
        );
    }
    let told = h.store.told_unstaffed(TEAM).unwrap();
    assert_eq!(told.len(), 3, "each node told once: {told:?}");
    for (n, _) in &kept {
        assert!(told.iter().any(|t| t.node == *n), "node {n} is told");
    }
}
