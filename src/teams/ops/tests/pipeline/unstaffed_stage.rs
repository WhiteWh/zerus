//! zerus-63rru.45: a stage that loses every member who could take it goes the
//! way a stage nobody could take goes at its creation.
use super::*;
use std::collections::BTreeSet;

fn reconcile(h: &mut Harness) -> Result<Reply> {
    h.run(&service(), Operation::ReconcileUnstaffed)
}
/// A created stage task, observed To do and not handed out.
fn created_stage(h: &mut Harness, parent: u32) -> u32 {
    let stage = stages(h, parent)[0].id;
    let id = uuid::Uuid::new_v4().to_string();
    h.run(&service(), creation(stage, &id)).unwrap();
    h.run(&service(), created(&id, 1000 + stage)).unwrap();
    h.observe(stage, TaskStage::Todo);
    stage
}
fn remove(h: &mut Harness, member: &str) {
    h.run(
        &person(),
        Operation::ArchiveMember {
            member: member.into(),
        },
    )
    .unwrap();
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
/// A node's hold columns as stored: the explicit hold, its owner, and the
/// hand-back's hold.
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
/// The stage's unsettled cancels, as the cancel step reads them.
fn pending_cancels(h: &Harness, stage: u32) -> usize {
    h.store
        .pending_cancels(TEAM, 200, 0)
        .unwrap()
        .into_iter()
        .filter(|r| r.node == stage)
        .count()
}
/// A retired stage whose task is bound waits for its cancel; the task's
/// observation, cancelled, ends it (zerus-63rru.38).
fn end_retired(h: &mut Harness, parent: u32, stage: u32) {
    assert_eq!(
        pending_cancels(h, stage),
        1,
        "the stage waits for one cancel"
    );
    assert_eq!(
        stages(h, parent).iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![stage],
        "the retired stage waits for its cancel"
    );
    h.cancel_fact(stage);
    assert!(stages(h, parent).is_empty(), "the retired stage ended");
}

/// The kernel's set before the open-effect rule, as the store reads it.
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

fn unreadable_stage() -> (Harness, u32, u32) {
    let (mut h, parent, _) = wanted();
    let stage = created_stage(&mut h, parent);
    let session = h
        .store
        .member(TEAM, "m3")
        .unwrap()
        .unwrap()
        .session
        .unwrap();
    for now in [100, 180_100] {
        let revision = h.store.team(TEAM).unwrap().unwrap().revision;
        let command = request(
            revision,
            Operation::RecordMemberReadability {
                readings: vec![MemberReading {
                    member: "m3".into(),
                    session: session.clone(),
                    unreadable: true,
                }],
            },
        );
        apply_fixture(&mut h.store, &service(), &command, now).unwrap();
    }
    reconcile(&mut h).unwrap();
    (h, parent, stage)
}

#[test]
fn an_unreadable_reviewer_is_told_to_the_lead_without_retiring_the_stage() {
    let (mut h, parent, stage) = unreadable_stage();
    let page = h.store.inbox(TEAM, 200, None).unwrap().rows;
    let item = page
        .iter()
        .find(|item| item.node == stage && item.kind.starts_with("unstaffed:"))
        .unwrap();
    assert_eq!(item.state, "open");
    assert_eq!(
        item.text,
        format!("Pipeline stage {stage} is ready. The service cannot read any eligible member (m3). It will check again.")
    );
    assert_eq!(stages(&h, parent)[0].id, stage);
    assert_eq!(run_state(&h, parent), "active");
    assert_eq!(pending_cancels(&h, stage), 0);
    let effect = h.intent(stage, "m3");
    assert_eq!(
        h.store.open_effects(TEAM, 200, None).unwrap().rows[0].request_id,
        effect
    );
}

#[test]
#[ignore = "exports the stored stage notice through the client drawer decoder"]
fn export_unreadable_stage_notice() {
    use serde_json::Value;
    use zerus_client_core::actions::Exit;
    use zerus_client_core::upstream_teams::drawers::{decode, DrawerKind, DrawerRead};

    let (h, _, stage) = unreadable_stage();
    let page = h.store.inbox(TEAM, 200, None).unwrap();
    let mut reply: Value = serde_json::from_str(include_str!(
        "../../../../../crates/team-plan/fixtures/cli-inbox.json"
    ))
    .unwrap();
    reply["rows"] = serde_json::json!(page.rows);
    let drawer = decode(
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
    let item = drawer
        .rows
        .iter()
        .find(|item| item.node == Some(u64::from(stage)) && item.kind.starts_with("unstaffed:"))
        .unwrap();
    let destination = std::env::var_os("TEAMS81_COMPAT_DIR").unwrap();
    let mut fixture = serde_json::to_string_pretty(item).unwrap();
    fixture.push('\n');
    std::fs::write(
        std::path::Path::new(&destination).join("unreadable-stage-notice.json"),
        fixture,
    )
    .unwrap();
}

/// The stage's reviewer is removed after its creation: the reconcile
/// retires the stage and hands the parent to the lead with the kernel's
/// reason, exactly as a reviewer removed before the creation does: the same
/// run state, the same report item and text, no unstaffed item. The stage's
/// task is bound, so the retirement leaves it one pending cancel
/// (zerus-63rru.38); its end hands nothing more to the lead.
#[test]
fn an_unstaffed_stage_is_retired_and_its_parent_handed_to_the_lead_as_at_creation() {
    // At creation: the reviewer is removed before the report.
    let mut at_creation = pipeline_harness("reviewed");
    at_creation.bind("m1");
    at_creation.bind("m3");
    remove(&mut at_creation, "m3");
    let first = at_creation.add(&person(), 1);
    at_creation.submit(first, "m1");
    at_creation.report(first, "m1");
    assert!(stages(&at_creation, first).is_empty());
    let expected = report_items(&at_creation, first);
    assert_eq!(expected.len(), 1);
    // Later: the stage exists, then its reviewer is removed.
    let (mut h, parent, _) = wanted();
    let stage = created_stage(&mut h, parent);
    assert_eq!(run_state(&h, parent), "active");
    remove(&mut h, "m3");
    assert_eq!(kernel_set(&h), vec![stage]);
    reconcile(&mut h).unwrap();
    for when in ["retired", "ended"] {
        if when == "ended" {
            end_retired(&mut h, parent, stage);
        }
        assert_eq!(run_state(&h, parent), "lead", "{when}");
        assert!(!h.node(parent).hold, "{when}");
        assert_eq!(report_items(&h, parent), expected, "{when}");
        assert_eq!(
            unstaffed_items(&h),
            0,
            "{when}: no unstaffed item for a stage"
        );
        let e = reconcile(&mut h).unwrap_err();
        assert_eq!(e.code, code::UNSTAFFED_UNCHANGED, "{when}");
    }
    assert!(expected[0].contains("No member of the team has the role reviewer."));
}

/// A stage that is pending a cancel, held, still has a member who could take
/// it (paused, or Active and busy), or has an open effect is never in the
/// set, so the reconcile never retires it. A stage whose hand-out belonged to
/// a removed member is handed back to the lead like any task (zerus-63rru.77):
/// held by the hand-back, so not ready, and the lead decides.
#[test]
fn a_stage_that_is_not_ready_or_still_staffed_is_never_retired() {
    for case in [
        "pending_cancel",
        "held",
        "paused",
        "busy",
        "open_effect",
        "removed",
    ] {
        let (mut h, parent, _) = wanted();
        let stage = created_stage(&mut h, parent);
        match case {
            "pending_cancel" => {
                h.run(
                    &person(),
                    Operation::Cancel {
                        node: stage,
                        reason: "Stop this review".into(),
                    },
                )
                .unwrap();
                remove(&mut h, "m3");
            }
            "held" => {
                h.run(&person(), Operation::Hold { node: stage }).unwrap();
                remove(&mut h, "m3");
            }
            "paused" => {
                h.run(
                    &person(),
                    Operation::PauseMember {
                        member: "m3".into(),
                    },
                )
                .unwrap();
            }
            "busy" => {}
            "open_effect" => {
                h.run(
                    &service(),
                    Operation::HandoutIntent {
                        node: stage,
                        member: "m3".into(),
                        effect_request_id: uuid::Uuid::new_v4().to_string(),
                    },
                )
                .unwrap();
                // zerus-63rru.84 refuses Route of a stage, so the premise
                // the route used to build is written by SQL as a store
                // fixture: everything up to the last step is the product's
                // own operations (the stage and its open hand-out), and the
                // last step is the role rewrite Route used to write. No
                // member may take it while m3's hand-out stays open.
                let db = h.store.db.as_ref().unwrap();
                let data: String = db
                    .query_row(
                        "SELECT data FROM nodes WHERE team=?1 AND id=?2",
                        rusqlite::params![TEAM, stage],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut node: serde_json::Value = serde_json::from_str(&data).unwrap();
                node["role"] = serde_json::json!("developer");
                node["pipeline"] = serde_json::json!("reviewed");
                node["assignee"] = serde_json::json!("m1");
                db.execute(
                    "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
                    rusqlite::params![TEAM, stage, node.to_string()],
                )
                .unwrap();
                // The kernel alone would name it; the open effect keeps it out.
                assert_eq!(kernel_set(&h), vec![stage], "{case}");
            }
            _ => {
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
                let parent_before = (hold_columns(&h, parent), h.node(parent).state);
                let parent_items = h
                    .store
                    .inbox(TEAM, 200, None)
                    .unwrap()
                    .rows
                    .into_iter()
                    .filter(|i| i.node == parent)
                    .count();
                remove(&mut h, "m3");
                let effect = h
                    .store
                    .effects(TEAM, 200, None)
                    .unwrap()
                    .rows
                    .into_iter()
                    .find(|e| e.request_id == id)
                    .unwrap();
                assert_eq!(effect.state, "superseded", "{case}");
                // Held by the hand-back to the lead, and by nobody's explicit hold.
                assert_eq!(hold_columns(&h, stage), (false, None, true), "{case}");
                let told: Vec<_> = h
                    .store
                    .inbox(TEAM, 200, None)
                    .unwrap()
                    .rows
                    .into_iter()
                    .filter(|i| i.node == stage)
                    .map(|i| (i.kind, i.state))
                    .collect();
                assert_eq!(
                    told,
                    [(format!("member_removed:{id}"), "open".to_owned())],
                    "{case}"
                );
                assert!(kernel_set(&h).is_empty(), "{case}: held, so not ready");
                assert_eq!(
                    (hold_columns(&h, parent), h.node(parent).state),
                    parent_before,
                    "{case}"
                );
                assert_eq!(
                    h.store
                        .inbox(TEAM, 200, None)
                        .unwrap()
                        .rows
                        .into_iter()
                        .filter(|i| i.node == parent)
                        .count(),
                    parent_items,
                    "{case}"
                );
            }
        }
        let e = reconcile(&mut h).unwrap_err();
        assert_eq!(e.code, code::UNSTAFFED_UNCHANGED, "{case}");
        assert_eq!(
            stages(&h, parent).iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![stage],
            "{case}: the stage stays"
        );
        assert_eq!(run_state(&h, parent), "active", "{case}");
        assert_eq!(unstaffed_items(&h), 0, "{case}");
    }
}

/// A hold has an owner (the hold rule of teams/wp10). While its stage runs
/// the parent is held by the pipeline's own hand-back (`want_stage`);
/// retiring the stage and handing the run to the lead ends that hand-back,
/// as at the stage's creation. A person's Hold set during the run is the
/// person's own: it is accepted, and it stays through the retirement and
/// the stage's end.
#[test]
fn the_pipelines_hold_on_the_parent_ends_with_its_retired_stage() {
    for persons in [false, true] {
        let (mut h, parent, _) = wanted();
        let stage = created_stage(&mut h, parent);
        assert!(h.node(parent).hold, "the pipeline holds the parent");
        assert_eq!(hold_columns(&h, parent), (false, None, true), "{persons}");
        if persons {
            h.run(&person(), Operation::Hold { node: parent }).unwrap();
            assert_eq!(
                hold_columns(&h, parent),
                (true, Some("person".into()), true)
            );
        }
        remove(&mut h, "m3");
        reconcile(&mut h).unwrap();
        let expected = if persons {
            (true, Some("person".into()), false)
        } else {
            (false, None, false)
        };
        for when in ["retired", "ended"] {
            if when == "ended" {
                end_retired(&mut h, parent, stage);
            }
            assert_eq!(run_state(&h, parent), "lead", "{persons} {when}");
            assert_eq!(
                hold_columns(&h, parent),
                expected,
                "{persons} {when}: the pipeline's hand-back ended with its run, a person's Hold stays"
            );
            assert_eq!(h.node(parent).hold, persons, "{persons} {when}");
            assert_eq!(report_items(&h, parent).len(), 1, "{persons} {when}");
        }
    }
}

/// A stage's parent, handed to the lead when its stage was retired: the lead
/// closing that item changes nothing, and a hundred reconciles write nothing.
#[test]
fn the_lead_closing_a_retired_stages_parent_item_is_not_told_again() {
    let (mut h, parent, _) = wanted();
    created_stage(&mut h, parent);
    remove(&mut h, "m3");
    reconcile(&mut h).unwrap();
    assert_eq!(report_items(&h, parent).len(), 1);
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE inbox SET state='closed' WHERE team=?1 AND node=?2 AND state='open'",
            rusqlite::params![TEAM, parent],
        )
        .unwrap();
    let revision = h.store.team(TEAM).unwrap().unwrap().revision;
    let inbox = h.store.inbox(TEAM, 200, None).unwrap().rows;
    for _ in 0..100 {
        let e = reconcile(&mut h).unwrap_err();
        assert_eq!(e.code, code::UNSTAFFED_UNCHANGED);
    }
    assert_eq!(h.store.team(TEAM).unwrap().unwrap().revision, revision);
    assert_eq!(h.store.inbox(TEAM, 200, None).unwrap().rows, inbox);
    assert_eq!(unstaffed_items(&h), 0);
}

/// Review 45, P1 (the reviewer's reproducer): a person's Hold set before the
/// report survives the reconcile that retires the stage. Under the rule "A
/// hold has an owner" the stage path ends only the pipeline's own hold, the
/// hand-back (`pipeline::to_lead`).
#[test]
fn a_persons_hold_from_before_the_report_survives_the_stage_reconcile() {
    let mut h = pipeline_harness("reviewed");
    h.bind("m1");
    h.bind("m3");
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.run(&person(), Operation::Hold { node: parent }).unwrap();
    assert!(
        h.node(parent).hold,
        "the person held the parent before the report"
    );
    h.report(parent, "m1");
    let stage = created_stage(&mut h, parent);
    remove(&mut h, "m3");
    reconcile(&mut h).unwrap();
    end_retired(&mut h, parent, stage);
    assert_ne!(stage, parent);
    assert_eq!(run_state(&h, parent), "lead");
    assert!(
        h.node(parent).hold,
        "the stage reconcile released the person's pre-report hold"
    );
    assert_eq!(
        hold_columns(&h, parent),
        (true, Some("person".into()), false)
    );
}
