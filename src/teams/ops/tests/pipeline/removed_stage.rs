//! zerus-63rru.77 meets zerus-63rru.45: a stage whose hand-out belonged to a
//! removed member is handed back to the lead like any task. Its hold is the
//! hand-back's, so it is not ready and the unstaffed reconcile leaves it; the
//! lead decides, and each decision is measured here through the store's
//! operations and the real reconcile.
use super::*;
use std::collections::BTreeSet;

fn reconcile(h: &mut Harness) -> Result<Reply> {
    h.run(&service(), Operation::ReconcileUnstaffed)
}
fn created_stage(h: &mut Harness, parent: u32) -> u32 {
    let stage = stages(h, parent)[0].id;
    let id = uuid::Uuid::new_v4().to_string();
    h.run(&service(), creation(stage, &id)).unwrap();
    h.run(&service(), created(&id, 1000 + stage)).unwrap();
    h.observe(stage, TaskStage::Todo);
    stage
}
fn kernel_set(h: &Harness) -> Vec<u32> {
    let team = h.store.team(TEAM).unwrap().unwrap();
    let r = crate::teams::ops::readiness(&h.store, TEAM).unwrap();
    let members = h.store.members(TEAM, 200, None).unwrap().rows;
    crate::teams::ops::unstaffed::unstaffed_of(
        &team,
        &r,
        &members,
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .into_iter()
    .map(|u| u.node)
    .collect()
}
/// The explicit hold, its owner, and the hand-back's hold.
fn hold_columns(h: &Harness, node: u32) -> (bool, Option<String>, bool) {
    h.store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT hold,hold_owner,hand_back FROM nodes WHERE team=?1 AND id=?2",
            rusqlite::params![TEAM, node],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
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
/// A node's inbox items: kind, state and text.
fn items(h: &Harness, node: u32) -> Vec<(String, String, String)> {
    let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
    rows.into_iter()
        .filter(|i| i.node == node)
        .map(|i| (i.kind, i.state, i.text))
        .collect()
}
fn unstaffed_items(h: &Harness) -> usize {
    let rows = h.store.inbox(TEAM, 200, None).unwrap().rows;
    rows.into_iter()
        .filter(|i| i.kind.starts_with("unstaffed:"))
        .count()
}
fn effect_state(h: &Harness, id: &str) -> String {
    let rows = h.store.effects(TEAM, 200, None).unwrap().rows;
    rows.into_iter().find(|e| e.request_id == id).unwrap().state
}

/// The team of `wanted`, its lead bound, and with `second`, a second
/// reviewer m5 bound in a session of its own.
fn team(second: bool) -> (Harness, u32, Actor) {
    let mut h = Harness::empty();
    let mut op = create();
    if let Operation::Create { bundle, .. } = &mut op {
        let catalog = zerus_team_plan::builtin();
        bundle.roles = catalog.roles;
        bundle.pipelines = catalog.pipelines;
        bundle
            .template
            .roster
            .push(serde_json::from_value(serde_json::json!({"role":"tester","count":1})).unwrap());
        if second {
            bundle.template.roster.push(
                serde_json::from_value(serde_json::json!({"role":"reviewer","count":1})).unwrap(),
            );
        }
        bundle.template.default_pipeline = "reviewed".into();
    }
    h.run(&person(), op).unwrap();
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m3");
    if second {
        let m3 = fixture()
            .members
            .into_iter()
            .find(|m| m.id == "m3")
            .unwrap();
        let mut session = m3.session.unwrap();
        session.session_id = format!("launch_id:{}", uuid::Uuid::new_v4());
        h.run(
            &service(),
            Operation::SetMemberSession {
                member: "m5".into(),
                session,
                account: m3.account,
                worktree: format!("{}-m5", m3.worktree),
                ownership_nonce: uuid::Uuid::new_v4().to_string(),
            },
        )
        .unwrap();
        let m5 = h.store.member(TEAM, "m5").unwrap().unwrap();
        assert_eq!(
            (m5.role.as_str(), m5.state),
            ("reviewer", MemberState::Active)
        );
    }
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    (h, parent, lead)
}
/// The stage created and its hand-out reserved for m3, then m3 removed: the
/// removal's state, asserted whole.
fn removed_reviewer(h: &mut Harness, parent: u32) -> (u32, String) {
    let stage = created_stage(h, parent);
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
        &person(),
        Operation::ArchiveMember {
            member: "m3".into(),
        },
    )
    .unwrap();
    assert_eq!(effect_state(h, &id), "superseded");
    assert_eq!(hold_columns(h, stage), (false, None, true));
    let told = items(h, stage);
    assert_eq!(told.len(), 1);
    assert_eq!(
        (told[0].0.as_str(), told[0].1.as_str()),
        (format!("member_removed:{id}").as_str(), "open")
    );
    assert!(kernel_set(h).is_empty());
    (stage, id)
}

/// The lead releases the stage while another reviewer could take it: the
/// stage is ready and goes to that member; nothing is retired or told.
#[test]
fn the_leads_release_with_another_reviewer_hands_the_stage_to_it() {
    let (mut h, parent, lead) = team(true);
    let (stage, _) = removed_reviewer(&mut h, parent);
    h.run(&lead, Operation::Release { node: stage }).unwrap();
    assert_eq!(hold_columns(&h, stage), (false, None, false));
    assert!(kernel_set(&h).is_empty(), "m5 may take it");
    h.run(
        &service(),
        Operation::HandoutIntent {
            node: stage,
            member: "m5".into(),
            effect_request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .unwrap();
    let e = reconcile(&mut h).unwrap_err();
    assert_eq!(e.code, code::UNSTAFFED_UNCHANGED);
    assert_eq!(
        stages(&h, parent).iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![stage]
    );
    assert_eq!(run_state(&h, parent), "active");
    assert_eq!(unstaffed_items(&h), 0);
}

/// The lead releases the stage while nobody could take it: the reconcile
/// retires it and hands the parent to the lead with the kernel's reason, one
/// report item for the parent; the stage's member_removed item is closed
/// with the stage.
#[test]
fn the_leads_release_with_nobody_able_retires_the_stage_and_hands_back_the_parent() {
    let (mut h, parent, lead) = team(false);
    let (stage, id) = removed_reviewer(&mut h, parent);
    let parent_before: Vec<_> = items(&h, parent)
        .into_iter()
        .filter(|(kind, _, _)| kind == "report")
        .collect();
    h.run(&lead, Operation::Release { node: stage }).unwrap();
    assert_eq!(kernel_set(&h), vec![stage]);
    reconcile(&mut h).unwrap();
    assert_eq!(run_state(&h, parent), "lead");
    let reports: Vec<_> = items(&h, parent)
        .into_iter()
        .filter(|(kind, state, _)| kind == "report" && state == "open")
        .collect();
    assert_eq!(reports.len(), 1, "{parent_before:?} {reports:?}");
    assert!(reports[0]
        .2
        .contains("No member of the team has the role reviewer."));
    assert_eq!(unstaffed_items(&h), 0);
    let stage_items = items(&h, stage);
    assert_eq!(stage_items.len(), 1);
    assert_eq!(
        (stage_items[0].0.as_str(), stage_items[0].1.as_str()),
        (format!("member_removed:{id}").as_str(), "closed")
    );
    let e = reconcile(&mut h).unwrap_err();
    assert_eq!(e.code, code::UNSTAFFED_UNCHANGED);
}

/// The lead's other decisions on the held stage, measured: a Reassign to
/// another reviewer is admitted and settles at once, and the stage goes to
/// that member by the ordinary hand-out; one to a member of another role is
/// refused; the lead's Cancel of a stage is refused (only the person
/// overrides a stage), and the person's is admitted.
#[test]
fn the_leads_reassign_and_cancel_on_the_held_stage() {
    let (mut h, parent, lead) = team(true);
    let (stage, _) = removed_reviewer(&mut h, parent);
    let wrong = h
        .run(
            &lead,
            Operation::Reassign {
                node: stage,
                member: "m1".into(),
                reason: "Another member takes it".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        wrong.message,
        "Replacement member cannot take this task: a stage is never handed to a member who implemented any round of the task."
    );
    let cancel = h
        .run(
            &lead,
            Operation::Cancel {
                node: stage,
                reason: "Not needed".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        cancel.message,
        "The stage needs its holder's verdict; only the person may override it."
    );
    h.run(
        &lead,
        Operation::Reassign {
            node: stage,
            member: "m5".into(),
            reason: "Another reviewer takes it".into(),
        },
    )
    .unwrap();
    // In nobody's hands (never started, its effect settled), the Reassign
    // settles at once and the ordinary hand-out follows the new assignee;
    // the hand-back's hold ends with it.
    let d = h
        .store
        .decisions(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .rev()
        .find(|d| d.node == stage)
        .unwrap();
    assert_eq!(
        (d.kind.as_str(), d.state),
        ("reassign", DecisionState::Settled)
    );
    assert!(h
        .store
        .pending_reopens(TEAM, 200, None)
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(h.node(stage).assignee.as_deref(), Some("m5"));
    assert_eq!(hold_columns(&h, stage), (false, None, false));
    h.run(
        &service(),
        Operation::HandoutIntent {
            node: stage,
            member: "m5".into(),
            effect_request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .unwrap();

    // The person's Cancel of the held stage (in a team of its own).
    let (mut h, parent, _) = team(false);
    let (stage, _) = removed_reviewer(&mut h, parent);
    h.run(
        &person(),
        Operation::Cancel {
            node: stage,
            reason: "Not needed".into(),
        },
    )
    .unwrap();
    let d = h
        .store
        .decisions(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .rev()
        .find(|d| d.node == stage)
        .unwrap();
    assert_eq!(
        (d.kind.as_str(), d.state),
        ("cancel", DecisionState::Pending)
    );
}

/// The person holds the handed-back stage before the lead decides: the
/// lead's Release is refused by the hold rule, as for a task, and the
/// person's own Release ends both holds.
#[test]
fn the_persons_hold_on_the_handed_back_stage_survives_the_leads_release() {
    let (mut h, parent, lead) = team(false);
    let (stage, _) = removed_reviewer(&mut h, parent);
    h.run(&person(), Operation::Hold { node: stage }).unwrap();
    assert_eq!(hold_columns(&h, stage), (true, Some("person".into()), true));
    let e = h
        .run(&lead, Operation::Release { node: stage })
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    assert!(e
        .message
        .starts_with("The person holds this task, and only the person releases that hold;"));
    assert_eq!(hold_columns(&h, stage), (true, Some("person".into()), true));
    h.run(&person(), Operation::Release { node: stage })
        .unwrap();
    assert_eq!(hold_columns(&h, stage), (false, None, false));
}

/// A stage its reviewer had started is taken back by the started-task rule,
/// unchanged: the effect superseded, the stage held, one item that says the
/// session was not told and where its work is.
#[test]
fn a_started_stage_of_a_removed_reviewer_follows_the_started_task_rule() {
    let (mut h, parent, lead) = team(true);
    let stage = created_stage(&mut h, parent);
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
    h.outcome(
        &id,
        HandoutOutcome::Submitted {
            receipt: "receipt-example".into(),
        },
    );
    h.observe(stage, TaskStage::InProgress);
    let path = h.store.member(TEAM, "m3").unwrap().unwrap().worktree;
    h.run(
        &person(),
        Operation::ArchiveMember {
            member: "m3".into(),
        },
    )
    .unwrap();
    assert_eq!(effect_state(&h, &id), "superseded");
    assert_eq!(hold_columns(&h, stage), (false, None, true));
    let told = items(&h, stage);
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].0, format!("member_removed:{id}"));
    assert_eq!(
        told[0].2,
        format!("Member m3 was removed while task {stage} was in progress; the task is with the lead again. Its session was not told and may still be working on it, in the worktree at {path}. Reassign gives the task to another member, and a later report from that session will not move it. The lead cannot cancel a stage; the person's Cancel stops that session and ends the task, and its parent comes back to the lead.")
    );
    assert!(kernel_set(&h).is_empty());
    // The decisions that item names, by the lead, on a started stage: the
    // Reassign is admitted and waits for its hand-over (the stage is in
    // someone's hands); the Cancel of a stage is the person's alone.
    let cancel = h
        .run(
            &lead,
            Operation::Cancel {
                node: stage,
                reason: "Not needed".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        cancel.message,
        "The stage needs its holder's verdict; only the person may override it."
    );
    h.run(
        &lead,
        Operation::Reassign {
            node: stage,
            member: "m5".into(),
            reason: "Another reviewer takes it".into(),
        },
    )
    .unwrap();
    let pending = h.store.pending_reopens(TEAM, 200, None).unwrap().rows;
    assert_eq!(
        pending
            .iter()
            .map(|d| (d.node, d.kind.as_str()))
            .collect::<Vec<_>>(),
        [(stage, "reassign")]
    );
    // And the person's Cancel, the other decision the item names, is
    // admitted (it replaces the lead's Reassign, which has not started).
    h.run(
        &person(),
        Operation::Cancel {
            node: stage,
            reason: "Not needed".into(),
        },
    )
    .unwrap();
    let d = h
        .store
        .decisions(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .rev()
        .find(|d| d.node == stage)
        .unwrap();
    assert_eq!(
        (d.kind.as_str(), d.state),
        ("cancel", DecisionState::Pending)
    );
}
