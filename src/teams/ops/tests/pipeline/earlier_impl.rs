//! zerus-63rru.20 round 2b: the earlier-implementer rule through the
//! product's own writers. A stage never goes to a member who implemented an
//! earlier round of its target: the record the reassignment and the
//! advance write, the stage's creation through `want_stage`, the real
//! `ReconcileUnstaffed`, the lead's inbox, and the record's bounded reads.
//!
//! The three-developer fixture here is a test fixture: the shared
//! `team-queue.json` keeps its two developers and one reviewer so the older
//! tests keep their shape.
use super::*;

/// A running team with three developers (m1, m2, m3) and one reviewer (m4).
/// Its default pipeline, `selfcheck`, gives its one stage the developer's
/// own role, so the earlier-implementer rule can exclude a whole role.
fn three_developers() -> Harness {
    let mut h = Harness::empty();
    let mut op = create();
    if let Operation::Create { bundle, .. } = &mut op {
        let catalog = zerus_team_plan::builtin();
        let mut pipelines = catalog.pipelines;
        let mut selfcheck = pipelines
            .iter()
            .find(|p| p.id == "reviewed")
            .unwrap()
            .clone();
        selfcheck.id = "selfcheck".into();
        selfcheck.name = "Self-check: a developer checks the round, lead".into();
        selfcheck.stages[0].role = "developer".into();
        pipelines.push(selfcheck);
        bundle.roles = catalog.roles;
        bundle.pipelines = pipelines;
        bundle.template.roster = vec![
            serde_json::from_value(serde_json::json!({"role":"developer","count":3})).unwrap(),
            serde_json::from_value(serde_json::json!({"role":"reviewer","count":1})).unwrap(),
        ];
        bundle.template.default_pipeline = "selfcheck".into();
    }
    h.run(&person(), op).unwrap();
    for id in ["lead", "m1", "m2", "m3", "m4"] {
        bind(&mut h, id);
    }
    h
}
/// Binds a member to a local session of this team, as the launcher's own
/// operation does.
fn bind(h: &mut Harness, id: &str) {
    h.run(
        &service(),
        Operation::SetMemberSession {
            member: id.into(),
            session: SessionKey {
                machine: fixture().team.home,
                session_id: format!("launch_id:{}", uuid::Uuid::new_v4()),
            },
            account: "account-example".into(),
            worktree: format!("/workspace/example-team-{id}"),
            ownership_nonce: format!("nonce-{id}"),
        },
    )
    .unwrap();
}
fn report_items(h: &Harness, node: u32) -> Vec<String> {
    h.store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|i| i.node == node && i.kind == "report" && i.state == "open")
        .map(|i| i.text)
        .collect()
}
fn unstaffed_items(h: &Harness) -> usize {
    h.store
        .inbox(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|i| i.kind.starts_with("unstaffed:"))
        .count()
}
fn run_state(h: &Harness, parent: u32) -> String {
    h.store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT state FROM pipeline_runs WHERE team=?1 AND node=?2",
            rusqlite::params![TEAM, parent],
            |r| r.get(0),
        )
        .unwrap()
}
/// The stage task bound, observed To do: the product's own two steps after a
/// wanted stage.
fn created_stage(h: &mut Harness, parent: u32) -> u32 {
    let stage = stages(h, parent)
        .into_iter()
        .find(|n| n.task.is_none())
        .unwrap()
        .id;
    let id = uuid::Uuid::new_v4().to_string();
    h.run(&service(), creation(stage, &id)).unwrap();
    h.run(&service(), created(&id, 1000 + stage)).unwrap();
    h.observe(stage, TaskStage::Todo);
    stage
}
/// A recorded member count for one target, as the store holds it.
fn record_count(h: &Harness, target: u32, member: &str) -> i64 {
    h.store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM target_implementers WHERE team=?1 AND target=?2 AND member=?3",
            rusqlite::params![TEAM, target, member],
            |r| r.get(0),
        )
        .unwrap()
}
/// The team's whole record table read as the old readiness did: each member
/// cell decoded as text. A fixture row that is not text fails this read.
fn whole_history_read(h: &Harness) -> rusqlite::Result<Vec<String>> {
    let db = h.store.db.as_ref().unwrap();
    let mut statement = db
        .prepare("SELECT member FROM target_implementers WHERE team=?1")
        .unwrap();
    statement
        .query_map([TEAM], |r| r.get::<_, String>(0))
        .unwrap()
        .collect()
}
/// A fixture record row the product's writers cannot write (a blob member),
/// under a target that is no longer live. It exists to tell a bounded read
/// of the live stages' targets from a read of the team's whole history.
fn poison_dead_target(h: &Harness, dead: u32) {
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "INSERT INTO target_implementers(team,target,member,round) VALUES(?1,?2,X'ff',1)",
            rusqlite::params![TEAM, dead],
        )
        .unwrap();
}
/// m1's round is reassigned and reopened to `next` through the product's
/// own operations; after it, `next` is the current implementer.
fn hand_over(h: &mut Harness, node: u32, next: &str) {
    h.run(
        &person(),
        Operation::Reassign {
            node,
            member: next.into(),
            reason: "Take the next round over".into(),
        },
    )
    .unwrap();
    let id = h.reopen(node, next);
    h.reopen_outcome(
        &id,
        HandoutOutcome::Submitted {
            receipt: "Reopen receipt".into(),
        },
    );
}

/// P2-2. Every eligible developer implemented an earlier round, so the
/// stage creation hands the task to the lead: no stage node exists, and the
/// lead's item names the rule and the way forward. Under r6 (the kernel
/// drops the earlier list) m1 and m2 are eligible again and a stage is
/// created.
#[test]
fn a_stage_whose_every_eligible_member_implemented_goes_to_the_lead_at_creation() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    hand_over(&mut h, parent, "m2");
    hand_over(&mut h, parent, "m3");
    assert_eq!(h.node(parent).implementer.as_deref(), Some("m3"));
    for (target, member) in [(parent, "m1"), (parent, "m2")] {
        assert_eq!(record_count(&h, target, member), 1, "{member} recorded");
    }
    h.report(parent, "m3");
    assert!(
        stages(&h, parent).is_empty(),
        "every developer implemented, so no stage node is created"
    );
    assert_eq!(run_state(&h, parent), "lead");
    let items = report_items(&h, parent);
    assert_eq!(items.len(), 1, "the lead is told once: {items:?}");
    assert!(
        items[0].contains("m1 and m2 and m3 implemented every round of the task"),
        "{}",
        items[0]
    );
    assert!(
        items[0].contains("accept, return or cancel it there"),
        "{}",
        items[0]
    );
}

/// P2-3. The readiness read is bounded to the targets of the live stages:
/// a record row for a target that left the live plan, which no text read can
/// decode, is never read. Widening the read back to the team's whole
/// history makes this readiness read fail.
#[test]
fn readiness_reads_only_the_live_stages_targets() {
    let mut h = three_developers();
    let live = h.add(&person(), 1);
    h.submit(live, "m1");
    h.report(live, "m1");
    assert_eq!(stages(&h, live).len(), 1, "a live stage of the live target");
    let dead = h.add(&person(), 2);
    h.run(
        &person(),
        Operation::Cancel {
            node: dead,
            reason: "Not needed".into(),
        },
    )
    .unwrap();
    h.cancel_fact(dead);
    assert!(
        !h.store
            .queue(TEAM, 200, None)
            .unwrap()
            .rows
            .iter()
            .any(|n| n.id == dead),
        "the dead target left the live plan"
    );
    poison_dead_target(&h, dead);
    assert!(
        whole_history_read(&h).is_err(),
        "the fixture row is unreadable as text"
    );
    let db = h.store.db.as_ref().unwrap();
    crate::teams::ops::readiness::load(db, TEAM).unwrap();
}

/// P2-4, the reassignment's write alone. The reported target is reassigned
/// with no reopen, outcome or report after it: the reassignment is the only
/// writer, and the record it writes is what m1 drops.
#[test]
fn the_reassignments_record_names_the_earlier_implementer() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    assert_eq!(
        record_count(&h, parent, "m1"),
        0,
        "the report of m1's own round records nothing"
    );
    h.run(
        &person(),
        Operation::Reassign {
            node: parent,
            member: "m2".into(),
            reason: "Take the next round over".into(),
        },
    )
    .unwrap();
    assert_eq!(
        record_count(&h, parent, "m1"),
        1,
        "the reassignment records the earlier implementer"
    );
    assert_eq!(
        h.node(parent).implementer.as_deref(),
        Some("m1"),
        "no reopen cleared the implementer; the reassignment alone wrote the record"
    );
    // The stage the report made was retired by the reassignment's override;
    // the target's record is what a later stage's hand-out reads.
    assert!(stages(&h, parent).is_empty());
    assert_eq!(run_state(&h, parent), "overridden");
}

/// P2-4, the advance's write alone, and P3-1. A restore (Done, then To do)
/// clears the implementer through `advance`, which records m1; no
/// reassignment happens. The next round's stage then refuses m1 at its
/// hand-out and at a person's Reassign, admits the unrecorded m3, and the
/// stage's Reassign writes no record of its own.
#[test]
fn the_restores_record_refuses_the_earlier_implementer_of_a_stage() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    // The first stage is checked by m2, passes, and ends.
    let first = created_stage(&mut h, parent);
    h.submit(first, "m2");
    let m2 = bound(&h, "m2");
    h.run(&m2, verdict(first, 1, true)).unwrap();
    h.observe(first, TaskStage::Done);
    // The lead accepts and integrates the round; the writer then reports
    // Done and the person restores it, which advances the generation and
    // records m1 as the earlier implementer.
    let lead = bound(&h, "lead");
    h.run(&lead, Operation::Accept { node: parent }).unwrap();
    let decision = h.accept_decision(parent);
    h.run(
        &service(),
        Operation::RecordIntegration {
            node: parent,
            decision,
            commit: COMMIT.into(),
        },
    )
    .unwrap();
    h.observe(parent, TaskStage::Done);
    h.observe(parent, TaskStage::Todo);
    assert_eq!(h.node(parent).implementer, None, "the restore cleared it");
    assert_eq!(
        record_count(&h, parent, "m1"),
        1,
        "the restore's advance recorded the earlier implementer"
    );
    // The next round: m2 implements, and the stage creation offers the
    // stage to m3, the one developer who has not implemented the target.
    h.submit(parent, "m2");
    h.report(parent, "m2");
    let stage = created_stage(&mut h, parent);
    let attempt = |h: &mut Harness, member: &str| {
        h.run(
            &service(),
            Operation::HandoutIntent {
                node: stage,
                member: member.into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
    };
    refused(attempt(&mut h, "m1"), code::INVALID_STATE);
    // P3-1: a person's Reassign of the stage follows the hand-out's rule.
    let wrong = h
        .run(
            &person(),
            Operation::Reassign {
                node: stage,
                member: "m1".into(),
                reason: "Let the first implementer check it".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        wrong.message,
        "Replacement member cannot take this task: a stage is never handed to a member who implemented any round of the task."
    );
    h.run(
        &person(),
        Operation::Reassign {
            node: stage,
            member: "m3".into(),
            reason: "The third developer checks it".into(),
        },
    )
    .unwrap();
    assert_eq!(
        record_count(&h, stage, "m2"),
        0,
        "a stage's reassignment writes no record under the stage"
    );
    attempt(&mut h, "m3").unwrap();
}

/// P3-5. A stage made by `want_stage` through the real pipeline, then
/// unstaffed by the product's own member archive: the real reconcile retires
/// it and hands its parent to the lead with the kernel's reason and the way
/// forward. No stage node or record row is written by hand.
#[test]
fn the_real_reconcile_retires_a_wanted_unstaffed_stage_and_tells_the_lead() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = created_stage(&mut h, parent);
    assert_eq!(run_state(&h, parent), "active");
    assert_eq!(
        report_items(&h, parent).len(),
        0,
        "the stage was staffed at creation; the lead was not told"
    );
    for member in ["m2", "m3"] {
        h.run(
            &person(),
            Operation::ArchiveMember {
                member: member.into(),
            },
        )
        .unwrap();
    }
    let team = h.store.team(TEAM).unwrap().unwrap();
    let readiness = crate::teams::ops::readiness(&h.store, TEAM).unwrap();
    let members = h.store.members(TEAM, 200, None).unwrap().rows;
    let unstaffed = crate::teams::ops::unstaffed::unstaffed_of(
        &team,
        &readiness,
        &members,
        &std::collections::BTreeSet::new(),
        &std::collections::BTreeSet::new(),
    );
    assert_eq!(
        unstaffed.iter().map(|u| u.node).collect::<Vec<_>>(),
        vec![stage],
        "only the earlier implementer is left with the role"
    );
    h.run(&service(), Operation::ReconcileUnstaffed).unwrap();
    assert_eq!(run_state(&h, parent), "lead");
    let items = report_items(&h, parent);
    assert_eq!(items.len(), 1, "the lead has the parent: {items:?}");
    assert!(
        items[0].contains("m1 implemented the task and is the only member with the role developer"),
        "{}",
        items[0]
    );
    assert!(
        items[0]
            .contains("a stage is never handed to a member who implemented any round of the task"),
        "{}",
        items[0]
    );
    assert!(
        items[0].contains("accept, return or cancel it there"),
        "{}",
        items[0]
    );
    assert_eq!(unstaffed_items(&h), 0, "a stage is retired, never told");
    assert_eq!(
        stages(&h, parent).iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![stage],
        "the retired stage waits for its cancel"
    );
}

/// P3-5, the positive case. The stage made by `want_stage` refuses the
/// earlier implementer and admits the third developer, who has not
/// implemented the target.
#[test]
fn the_unrecorded_third_developer_takes_the_stage() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = created_stage(&mut h, parent);
    let attempt = |h: &mut Harness, member: &str| {
        h.run(
            &service(),
            Operation::HandoutIntent {
                node: stage,
                member: member.into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
    };
    refused(attempt(&mut h, "m1"), code::INVALID_STATE);
    let id = uuid::Uuid::new_v4().to_string();
    h.run(
        &service(),
        Operation::HandoutIntent {
            node: stage,
            member: "m3".into(),
            effect_request_id: id.clone(),
        },
    )
    .unwrap();
    h.run(
        &service(),
        Operation::HandoutOutcome {
            effect_request_id: id,
            outcome: HandoutOutcome::Submitted {
                receipt: "Third developer received it".into(),
            },
        },
    )
    .unwrap();
    let holder = crate::teams::store::holder_of(h.store.db.as_ref().unwrap(), TEAM, stage)
        .unwrap()
        .unwrap();
    assert_eq!(holder.member, "m3");
    // The stage keeps the checked round's implementer, not its checker.
    assert_eq!(h.node(stage).implementer.as_deref(), Some("m1"));
    assert_eq!(report_items(&h, parent).len(), 0);
}

/// P3-3. A record read that fails refuses the hand-out and the person's
/// Reassign of a stage (fail closed), instead of admitting the member. The
/// fixture row is a blob `member` under the target the operations read.
#[test]
fn a_failed_record_read_refuses_rather_than_admits() {
    // A person's Reassign of a stage reads the target's record.
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = stages(&h, parent)[0].id;
    poison_dead_target(&h, parent);
    let e = h
        .run(
            &person(),
            Operation::Reassign {
                node: stage,
                member: "m2".into(),
                reason: "Next checker".into(),
            },
        )
        .unwrap_err();
    assert_eq!(e.code, code::STORAGE_FAILED);

    // A reopen's hand-out eligibility reads the target's record.
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = created_stage(&mut h, parent);
    h.submit(stage, "m2");
    h.run(
        &person(),
        Operation::Reassign {
            node: stage,
            member: "m3".into(),
            reason: "Next checker".into(),
        },
    )
    .unwrap();
    poison_dead_target(&h, parent);
    let id = uuid::Uuid::new_v4().to_string();
    let e = h
        .run(
            &service(),
            Operation::ReopenIntent {
                node: stage,
                member: "m3".into(),
                effect_request_id: id,
            },
        )
        .unwrap_err();
    assert_eq!(e.code, code::STORAGE_FAILED);
}

/// zerus-3u5yu (5): the queue row of a stage names its target's earlier
/// implementers, whom a stage's Reassign refuses, so the window can leave
/// them out of the stage's replacement choices. Another row names nobody.
#[test]
fn a_stage_row_names_its_targets_earlier_implementers() {
    let mut h = three_developers();
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let first = created_stage(&mut h, parent);
    h.submit(first, "m2");
    let m2 = bound(&h, "m2");
    h.run(&m2, verdict(first, 1, true)).unwrap();
    h.observe(first, TaskStage::Done);
    let lead = bound(&h, "lead");
    h.run(&lead, Operation::Accept { node: parent }).unwrap();
    let decision = h.accept_decision(parent);
    h.run(
        &service(),
        Operation::RecordIntegration {
            node: parent,
            decision,
            commit: COMMIT.into(),
        },
    )
    .unwrap();
    h.observe(parent, TaskStage::Done);
    h.observe(parent, TaskStage::Todo);
    h.submit(parent, "m2");
    h.report(parent, "m2");
    let stage = created_stage(&mut h, parent);
    let rows = h.store.queue_rows(TEAM, 200, None).unwrap().rows;
    let row = |id: u32| rows.iter().find(|row| row.node.id == id).unwrap();
    assert_eq!(row(stage).node.implementer.as_deref(), Some("m2"));
    assert_eq!(row(stage).earlier_implementers, ["m1"]);
    assert!(row(parent).earlier_implementers.is_empty());
}
