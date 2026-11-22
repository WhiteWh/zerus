use super::*;
#[test]
fn handout_reservation_survives_reopen_and_allows_only_one_member_slot() {
    let mut h = Harness::new();
    h.bind("m1");
    let first = h.add(&person(), 1);
    let second = h.add(&person(), 2);
    let id = h.intent(first, "m1");
    h.store = Store::open(h.temp.path()).unwrap();
    let op = Operation::HandoutIntent {
        node: second,
        member: "m1".into(),
        effect_request_id: uuid::Uuid::new_v4().to_string(),
    };
    refused(h.run(&service(), op.clone()), code::INVALID_STATE);
    h.outcome(
        &id,
        HandoutOutcome::Refused {
            reason: "Writer refused before delivery".into(),
        },
    );
    h.run(&service(), op).unwrap();
}
#[test]
fn refused_handout_leaves_no_assignee_or_implementer_and_another_member_can_take_it() {
    let mut h = Harness::new();
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    let id = h.intent(node, "m1");
    assert!(h.node(node).assignee.is_none() && h.node(node).implementer.is_none());
    h.outcome(
        &id,
        HandoutOutcome::Refused {
            reason: "Refused".into(),
        },
    );
    assert!(h.node(node).assignee.is_none() && h.node(node).implementer.is_none());
    let id = h.intent(node, "m2");
    h.outcome(
        &id,
        HandoutOutcome::Submitted {
            receipt: "receipt-example".into(),
        },
    );
    assert!(h.node(node).assignee.is_none());
    assert_eq!(h.node(node).implementer.as_deref(), Some("m2"));
}
#[test]
fn unknown_outcome_never_retries_the_same_effect_or_opens_a_second_slot() {
    for outcome in [
        HandoutOutcome::Uncertain {
            reason: "Lost receipt".into(),
        },
        HandoutOutcome::Uncertain {
            reason: "Cutting over; the receipt is unknown".into(),
        },
    ] {
        let mut h = Harness::new();
        h.bind("m1");
        let node = h.add(&person(), 1);
        let id = h.intent(node, "m1");
        h.outcome(&id, outcome);
        refused(
            h.run(
                &service(),
                Operation::HandoutIntent {
                    node,
                    member: "m1".into(),
                    effect_request_id: uuid::Uuid::new_v4().to_string(),
                },
            ),
            code::INVALID_STATE,
        );
        refused(
            h.run(
                &service(),
                Operation::HandoutOutcome {
                    effect_request_id: id,
                    outcome: HandoutOutcome::Submitted {
                        receipt: "late".into(),
                    },
                },
            ),
            code::INVALID_STATE,
        );
        assert_eq!(
            h.store.inbox(TEAM, 200, None).unwrap().rows[0].kind,
            "delivery_uncertain"
        );
    }
}
#[test]
fn a_superseded_reservation_accepts_its_late_outcome_without_restoring_the_slot() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    let old = h.intent(node, "m1");
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m2".into(),
            reason: "Use another member".into(),
        },
    )
    .unwrap();
    let new = h.reopen(node, "m2");
    h.outcome(
        &old,
        HandoutOutcome::Submitted {
            receipt: "Late original receipt".into(),
        },
    );
    let effects = h.store.effects(TEAM, 200, None).unwrap().rows;
    assert_eq!(effects[0].state, "superseded");
    assert!(effects[0].outcome.is_some());
    assert_eq!(effects[1].state, "reserved");
    assert!(h.node(node).implementer.is_none());
    h.reopen_outcome(
        &new,
        HandoutOutcome::Submitted {
            receipt: "Current receipt".into(),
        },
    );
    assert_eq!(h.node(node).implementer.as_deref(), Some("m2"));
}
#[test]
fn a_service_report_must_belong_to_the_effect_holder() {
    let mut h = Harness::new();
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    h.submit(node, "m1");
    h.observe(node, TaskStage::InReview);
    refused(
        h.run(
            &service(),
            Operation::RecordReport {
                node,
                generation: 1,
                member: "m2".into(),
                text: "Wrong holder".into(),
                commit: COMMIT.into(),
                notes: Vec::new(),
            },
        ),
        code::INVALID_STATE,
    );
    h.run(
        &service(),
        Operation::RecordReport {
            node,
            generation: 1,
            member: "m1".into(),
            text: "Correct holder".into(),
            commit: COMMIT.into(),
            notes: Vec::new(),
        },
    )
    .unwrap();
}
/// The report's service notes are validated, and a bad one writes nothing:
/// the member's text is untouched either way.
#[test]
fn report_validates_service_note_codes_and_bounds() {
    let mut h = Harness::new();
    h.bind("m1");
    let node = h.add(&person(), 1);
    h.submit(node, "m1");
    h.observe(node, TaskStage::InReview);
    let op = |notes| Operation::RecordReport {
        node,
        generation: 1,
        member: "m1".into(),
        text: "Report".into(),
        commit: COMMIT.into(),
        notes,
    };
    refused(
        h.run(
            &service(),
            op(vec![ReportNote {
                code: "Not A Token".into(),
                text: None,
                parameters: Vec::new(),
            }]),
        ),
        code::INVALID_STATE,
    );
    // A valid first character is not enough: every later character is checked
    // too, so a validation that only looks at the first one is refused.
    refused(
        h.run(
            &service(),
            op(vec![ReportNote {
                code: "text_Shortened".into(),
                text: None,
                parameters: Vec::new(),
            }]),
        ),
        code::INVALID_STATE,
    );
    refused(
        h.run(
            &service(),
            op(vec![ReportNote {
                code: "text_shortened".into(),
                text: Some("x".repeat(501)),
                parameters: Vec::new(),
            }]),
        ),
        code::OVER_CAP,
    );
    // Nine notes exceed the limit; the count comparison is not allowed to
    // accept one more, and nothing is written.
    refused(
        h.run(
            &service(),
            op((0..9)
                .map(|_| ReportNote {
                    code: "worktree_dirty".into(),
                    text: None,
                    parameters: Vec::new(),
                })
                .collect()),
        ),
        code::OVER_CAP,
    );
    assert!(h.store.report(TEAM, node, 1).unwrap().is_none());
    h.run(
        &service(),
        op(vec![ReportNote {
            code: "text_shortened".into(),
            text: Some("Why it was cut".into()),
            parameters: Vec::new(),
        }]),
    )
    .unwrap();
    let report = h.store.report(TEAM, node, 1).unwrap().unwrap();
    assert_eq!(report.text, "Report");
    assert_eq!(
        report.notes,
        vec![ReportNote {
            code: "text_shortened".into(),
            text: Some("Why it was cut".into()),
            parameters: Vec::new(),
        }]
    );
}

#[test]
fn report_requires_service_observation_and_the_current_generation() {
    let mut h = Harness::new();
    h.bind("m1");
    let node = h.add(&person(), 1);
    h.submit(node, "m1");
    let op = Operation::RecordReport {
        node,
        generation: 1,
        member: "m1".into(),
        text: "Report".into(),
        commit: COMMIT.into(),
        notes: Vec::new(),
};
    refused(h.run(&service(), op.clone()), code::INVALID_STATE);
    h.observe(node, TaskStage::InReview);
    h.run(&service(), op.clone()).unwrap();
    refused(h.run(&service(), op), code::INVALID_STATE);
}
#[test]
fn a_stage_keeps_the_checked_tasks_implementer_when_the_reviewer_is_submitted() {
    let mut h = Harness::new();
    let (parent, _) = h.reported();
    h.bind("m3");
    let node = h.add(&person(), 2);
    let mut stage = h.node(node);
    stage.kind = NodeKind::Stage;
    stage.role = "reviewer".into();
    stage.stage_of = Some(parent);
    stage.implementer = h.node(parent).implementer;
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![TEAM, node, rows::node_payload(&stage).unwrap()],
        )
        .unwrap();
    h.submit(node, "m3");
    assert_eq!(h.node(node).implementer.as_deref(), Some("m1"));
    assert!(h.node(node).assignee.is_none());
    let effect = h
        .store
        .effects(TEAM, 200, None)
        .unwrap()
        .rows
        .pop()
        .unwrap();
    assert_eq!(effect.member, "m3");
}

/// zerus-63rru.20: after a reassignment, a stage of the target never goes
/// to any member who implemented an earlier round, whatever the stage's
/// role; a member who never implemented it can take it; the record
/// survives a restart. Driven by the real reassignment and round
/// operations, then the kernel's hand-out over the stored rows.
#[test]
fn a_stage_after_a_reassignment_refuses_every_earlier_implementer() {
    let mut h = Harness::new();
    let (parent, _lead) = h.reported();
    h.bind("m2");
    // The person reassigns the reported task to m2; the store records m1 as
    // the earlier implementer in the reassignment's own transaction.
    h.run(
        &person(),
        Operation::Reassign {
            node: parent,
            member: "m2".into(),
            reason: "Take the next round over".into(),
        },
    )
    .unwrap();
    // m2 takes the round over through the real reopen and reports: the
    // report's freeze records m1 too and makes m2 the current implementer,
    // so the stage's refusal of m1 is the earlier record alone.
    let reopen = h.reopen(parent, "m2");
    h.reopen_outcome(
        &reopen,
        HandoutOutcome::Submitted {
            receipt: "Reopen receipt".into(),
        },
    );
    h.observe(parent, TaskStage::InProgress);
    h.report(parent, "m2");
    let recorded: i64 = h
        .store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM target_implementers WHERE team=?1 AND target=?2 AND member='m1'",
            rusqlite::params![TEAM, parent],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 1, "the reassignment recorded the first implementer");
    assert_eq!(h.node(parent).implementer.as_deref(), Some("m2"), "m2 is the current implementer");
    // A stage of the target whose role equals the implementers' role.
    let stage = h.add(&person(), 2);
    let mut s = h.node(stage);
    s.kind = NodeKind::Stage;
    s.role = h.node(parent).role.clone();
    s.stage_of = Some(parent);
    s.round = h.node(parent).round;
    s.implementer = h.node(parent).implementer.clone();
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![TEAM, stage, rows::node_payload(&s).unwrap()],
        )
        .unwrap();
    let intent = |h: &mut Harness, member: &str| {
        h.run(
            &service(),
            Operation::HandoutIntent {
                node: stage,
                member: member.into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
    };
    // The first implementer is refused though its role equals the stage's
    // and it is not the current implementer.
    refused(intent(&mut h, "m1"), code::INVALID_STATE);
    // The current implementer stays refused as before.
    refused(intent(&mut h, "m2"), code::INVALID_STATE);
    // The record survives a restart: m1 stays refused.
    h.store = Store::open(h.temp.path()).unwrap();
    refused(intent(&mut h, "m1"), code::INVALID_STATE);
}

/// zerus-63rru.20: the kernel's staffing names every earlier implementer
/// in the unstaffed reading (the lead's card).
#[test]
fn the_unstaffed_reading_names_every_earlier_implementer() {
    let mut h = Harness::new();
    let (parent, _lead) = h.reported();
    h.bind("m2");
    h.run(
        &person(),
        Operation::Reassign {
            node: parent,
            member: "m2".into(),
            reason: "Take the next round over".into(),
        },
    )
    .unwrap();
    let reopen = h.reopen(parent, "m2");
    h.reopen_outcome(
        &reopen,
        HandoutOutcome::Submitted {
            receipt: "Reopen receipt".into(),
        },
    );
    h.observe(parent, TaskStage::InProgress);
    h.report(parent, "m2");
    let stage = h.add(&person(), 2);
    let mut s = h.node(stage);
    s.kind = NodeKind::Stage;
    s.role = h.node(parent).role.clone();
    s.stage_of = Some(parent);
    s.round = h.node(parent).round;
    s.implementer = h.node(parent).implementer.clone();
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![TEAM, stage, rows::node_payload(&s).unwrap()],
        )
        .unwrap();
    // The kernel's staffing over the stored rows: the reason names both
    // implementers, the earlier m1 included.
    let readiness = crate::teams::ops::readiness::load(h.store.db.as_ref().unwrap(), TEAM).unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let members: Vec<zerus_team_plan::model::Member> = h
        .store
        .members(TEAM, 200, None)
        .unwrap()
        .rows;
    let member_views: Vec<zerus_team_plan::ready::MemberView> = members
        .iter()
        .map(|m| zerus_team_plan::ready::MemberView { id: m.id.clone(), role: m.role.clone(), state: m.state, free: false })
        .collect();
    let view = zerus_team_plan::ready::PlanView {
        phases: &readiness.phases,
        nodes: &readiness.nodes,
        members: &member_views,
        lead: &team.lead,
        integration: team.settings.integration,
    };
    let unstaffed = zerus_team_plan::ready::unstaffed(&view, &Default::default());
    let stage_entry = unstaffed.iter().find(|u| u.node == stage).unwrap_or_else(|| {
        panic!("the stage is unstaffed: {unstaffed:?}")
    });
    assert!(
        stage_entry.reason.contains("m1") && stage_entry.reason.contains("m2"),
        "{}",
        stage_entry.reason
    );
}



#[test]
fn service_handout_requires_kernel_ready_and_reads_archived_dependencies() {
    let mut h = Harness::new();
    h.bind("m1");
    let lead = h.bind("lead");
    let dep = h.add(&person(), 1);
    let node = h.add(&person(), 2);
    let mut op = route(node);
    if let Operation::Route { deps, .. } = &mut op {
        deps.push(Dep {
            node: dep,
            kind: DepKind::Hard,
        });
    }
    h.run(&lead, op).unwrap();
    let handout = Operation::HandoutIntent {
        node,
        member: "m1".into(),
        effect_request_id: uuid::Uuid::new_v4().to_string(),
    };
    refused(h.run(&service(), handout.clone()), code::INVALID_STATE);
    h.submit(dep, "m1");
    h.report(dep, "m1");
    h.run(&lead, Operation::Accept { node: dep }).unwrap();
    h.observe(dep, TaskStage::Done);
    assert_eq!(h.node(dep).state, NodeState::Reported);
    refused(h.run(&service(), handout.clone()), code::INVALID_STATE);
    h.run(
        &service(),
        Operation::RecordIntegration {
            decision: h.accept_decision(dep),
            node: dep,
            commit: COMMIT.into(),
        },
    )
    .unwrap();
    assert_eq!(h.node(dep).state, NodeState::Accepted);
    assert_eq!(h.store.queue(TEAM, 200, None).unwrap().rows.len(), 1);
    h.run(&service(), handout).unwrap();
}

#[test]
fn returning_a_stage_advances_its_attempt_but_keeps_the_checked_round() {
    let mut h = Harness::new();
    let (parent, lead) = h.reported();
    h.bind("m3");
    let node = h.add(&person(), 2);
    let mut stage = h.node(node);
    stage.kind = NodeKind::Stage;
    stage.role = "reviewer".into();
    stage.stage_of = Some(parent);
    stage.implementer = h.node(parent).implementer;
    h.store
        .db
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE nodes SET data=?3 WHERE team=?1 AND id=?2",
            params![TEAM, node, rows::node_payload(&stage).unwrap()],
        )
        .unwrap();
    h.submit(node, "m3");
    h.report(node, "m3");
    h.run(
        &lead,
        Operation::Return {
            node,
            reason: "Check again".into(),
        },
    )
    .unwrap();
    h.reopen(node, "m3");
    assert_eq!(h.store.generation(TEAM, node).unwrap(), Some(2));
    assert_eq!(h.node(node).round, h.node(parent).round);
    assert_eq!(h.node(node).implementer.as_deref(), Some("m1"));
}

#[test]
fn archived_dependency_history_is_a_leaf_for_dependent_and_unrelated_handouts() {
    let mut h = Harness::new();
    h.bind("m1");
    h.bind("m2");
    let dependent = h.add(&person(), 1);
    let unrelated = h.add(&person(), 2);
    let mut archived = h.node(dependent);
    archived.task = None;
    archived.state = NodeState::Accepted;
    let tx = h.store.db.as_mut().unwrap().transaction().unwrap();
    for id in 3..=2102 {
        archived.id = id;
        archived.task = Some(task(id));
        tx.execute("INSERT INTO node_identity(team,id,task_project,task_uuid) VALUES(?1,?2,'example-project',?3)",params![TEAM,id,task(id).uuid]).unwrap();
        tx.execute("INSERT INTO node_archive(team,id,data,rank,admitted,routed,task_revision,task_stage,task_cancelled,integrated) VALUES(?1,?2,?3,'',1,1,'archived','done',0,1)",params![TEAM,id,rows::node_payload(&archived).unwrap()]).unwrap();
        if id > 3 {
            tx.execute(
                "INSERT INTO dependencies(team,node,dependency,kind) VALUES(?1,?2,?3,?4)",
                params![TEAM, id, id - 1, encode(&DepKind::Hard).unwrap()],
            )
            .unwrap();
        }
    }
    tx.execute(
        "INSERT INTO dependencies(team,node,dependency,kind) VALUES(?1,?2,2102,?3)",
        params![TEAM, dependent, encode(&DepKind::Hard).unwrap()],
    )
    .unwrap();
    tx.commit().unwrap();
    h.intent(dependent, "m1");
    h.intent(unrelated, "m2");
}
#[test]
fn reassigning_a_never_handed_node_cannot_bypass_readiness_with_a_reopen() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let dep = h.add(&person(), 1);
    let node = h.add(&person(), 2);
    let mut op = route(node);
    if let Operation::Route { deps, .. } = &mut op {
        deps.push(Dep {
            node: dep,
            kind: DepKind::Hard,
        });
    }
    h.run(&lead, op).unwrap();
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m1".into(),
            reason: "Plan assignment".into(),
        },
    )
    .unwrap();
    refused(
        h.run(
            &service(),
            Operation::ReopenIntent {
                node,
                member: "m1".into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        ),
        code::INVALID_STATE,
    );
    refused(
        h.run(
            &service(),
            Operation::HandoutIntent {
                node,
                member: "m1".into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        ),
        code::INVALID_STATE,
    );
    assert!(!h.node(node).hold);
    assert_eq!(
        h.store
            .decisions(TEAM, 200, None)
            .unwrap()
            .rows
            .last()
            .unwrap()
            .state,
        DecisionState::Settled
    );
    h.cancel_fact(dep);
    h.intent(node, "m1");
}

#[test]
fn reopen_requires_an_older_effect_even_for_a_retained_pending_reassignment() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let node = h.add(&person(), 1);
    let mut planned = h.node(node);
    planned.assignee = Some("m1".into());
    let db = h.store.db.as_ref().unwrap();
    db.execute(
        "UPDATE nodes SET data=?3,requested='reassign',hold=1 WHERE team=?1 AND id=?2",
        params![TEAM, node, rows::node_payload(&planned).unwrap()],
    )
    .unwrap();
    db.execute("INSERT INTO decisions(team,node,generation,actor,kind,text,state) VALUES(?1,?2,1,?3,'reassign','Retained pre-fix decision',?4)",params![TEAM,node,encode(&lead).unwrap(),DecisionState::Pending]).unwrap();
    refused(
        h.run(
            &service(),
            Operation::ReopenIntent {
                node,
                member: "m1".into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        ),
        code::INVALID_STATE,
    );
    assert!(h.store.effects(TEAM, 200, None).unwrap().rows.is_empty());
    assert_eq!(h.store.generation(TEAM, node).unwrap(), Some(1));
}

#[test]
fn readiness_view_contains_only_live_nodes_and_their_archived_direct_leaves() {
    let mut h = Harness::new();
    h.settings(|s| s.integration = Integration::None);
    let mut previous = None;
    for id in 1..=3 {
        let node = h.add(&person(), id);
        let mut op = route(node);
        if let (Some(parent), Operation::Route { deps, .. }) = (previous, &mut op) {
            deps.push(Dep {
                node: parent,
                kind: DepKind::Hard,
            });
        }
        h.run(&person(), op).unwrap();
        h.observe(node, TaskStage::Done);
        previous = Some(node);
    }
    let live = h.add(&person(), 4);
    let mut op = route(live);
    if let Operation::Route { deps, .. } = &mut op {
        deps.push(Dep {
            node: previous.unwrap(),
            kind: DepKind::Hard,
        });
    }
    h.run(&person(), op.clone()).unwrap();
    let team = h.store.team(TEAM).unwrap().unwrap();
    let actor = service();
    let ctx = Context {
        db: h.store.db.as_ref().unwrap(),
        state: h.temp.path(),
        actor: &actor,
        team: &team,
        op: &op,
        request_id: TEAM,
        provider_stop: None,
        closed_confirmation_card: None,
        task_custody: None,
        now: 1234,
        reply: Reply {
            ok: true,
            revision: 0,
            member: None,
            node: None,
            item: None,
        },
        event: None,
        land_skipped_for: None,
        superseded: None,
        timing: None,
    };
    let nodes = super::super::handout::kernel_nodes(&ctx).unwrap();
    assert_eq!(nodes.iter().map(|n| n.id).collect::<Vec<_>>(), vec![3, 4]);
    assert!(nodes[0].deps.is_empty());
    assert_eq!(
        nodes[1].deps,
        vec![Dep {
            node: 3,
            kind: DepKind::Hard
        }]
    );
}

#[test]
fn a_refused_handout_cannot_be_reopened_to_bypass_a_new_dependency() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    h.bind("m1");
    let dependency = h.add(&person(), 1);
    let node = h.add(&person(), 2);
    let effect = h.intent(node, "m1");
    h.outcome(
        &effect,
        HandoutOutcome::Refused {
            reason: "Writer refused delivery".into(),
        },
    );
    let mut op = route(node);
    if let Operation::Route { deps, .. } = &mut op {
        deps.push(Dep {
            node: dependency,
            kind: DepKind::Hard,
        });
    }
    h.run(&lead, op).unwrap();
    refused(
        h.run(
            &service(),
            Operation::HandoutIntent {
                node,
                member: "m1".into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        ),
        code::INVALID_STATE,
    );
    h.run(
        &lead,
        Operation::Reassign {
            node,
            member: "m1".into(),
            reason: "Plan assignment after refusal".into(),
        },
    )
    .unwrap();
    refused(
        h.run(
            &service(),
            Operation::ReopenIntent {
                node,
                member: "m1".into(),
                effect_request_id: uuid::Uuid::new_v4().to_string(),
            },
        ),
        code::INVALID_STATE,
    );
    assert_eq!(h.store.generation(TEAM, node).unwrap(), Some(1));
    assert_eq!(h.store.effects(TEAM, 200, None).unwrap().rows.len(), 1);
    assert!(!h.node(node).hold);
    h.cancel_fact(dependency);
    h.intent(node, "m1");
}

/// The journal's hand-out outcome row says what the outcome was and, for a
/// refusal, its reason, cleaned (zerus-63rru.79): an intent that typed
/// nothing does not read like a hand-out that happened.
#[test]
fn the_journal_says_each_hand_out_outcome_and_a_refusals_reason_cleaned() {
    let mut h = Harness::new();
    h.bind("m1");
    h.bind("m2");
    let mut texts = Vec::new();
    // An uncertain outcome keeps its member's slot: m2 takes it.
    for (n, (member, outcome)) in [
        (
            "m1",
            HandoutOutcome::Refused {
                reason: "terminal has a draft,\n\tdialog\u{1b}[31m, or\u{202e} prompt".into(),
            },
        ),
        (
            "m2",
            HandoutOutcome::Uncertain {
                reason: "The task input was not confirmed.".into(),
            },
        ),
        (
            "m1",
            HandoutOutcome::Submitted {
                receipt: "receipt".into(),
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let node = h.add(&person(), n as u32 + 1);
        let id = h.intent(node, member);
        h.outcome(&id, outcome);
        let row = h
            .store
            .journal(TEAM, 200, None)
            .unwrap()
            .rows
            .into_iter()
            .rev()
            .find(|j| j.kind == "handout_outcome")
            .unwrap();
        texts.push(row.text.split_once(": ").unwrap().1.to_owned());
    }
    assert_eq!(
        texts,
        [
            "refused: terminal has a draft, dialog[31m, or prompt",
            "uncertain: The task input was not confirmed.",
            "submitted",
        ]
    );
}
