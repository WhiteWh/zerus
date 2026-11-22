use super::*;
fn lead_ops() -> Vec<Operation> {
    vec![
        Operation::LeadAddMember {
            role: "reviewer".into(),
            agent: "codex".into(),
            model: String::new(),
            effort: String::new(),
            account: Default::default(),
        },
        Operation::Pause,
        Operation::Resume,
        Operation::PauseMember {
            member: "m1".into(),
        },
        Operation::ResumeMember {
            member: "m1".into(),
        },
        Operation::Admit { node: 1 },
        route(1),
        Operation::Hold { node: 1 },
        Operation::Release { node: 1 },
        Operation::Cancel {
            node: 1,
            reason: "Example".into(),
        },
        Operation::Accept { node: 1 },
        Operation::Return {
            node: 1,
            reason: "Example".into(),
        },
        Operation::Reassign {
            node: 1,
            member: "m1".into(),
            reason: "Example".into(),
        },
        Operation::Reply {
            item: 1,
            text: "Example".into(),
        },
        Operation::OpenCard {
            kind: "proposal".into(),
            text: "Example".into(),
            node: None,
            decisions: None,
                parameters: None,
        },
    ]
}
#[test]
fn every_lead_only_operation_refuses_a_bound_member_through_apply() {
    let mut h = Harness::new();
    h.bind("lead");
    let member = h.bind("m1");
    for op in lead_ops() {
        refused(h.run(&member, op), code::NOT_ALLOWED);
    }
}
#[test]
fn every_person_only_operation_refuses_the_bound_lead_through_apply() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    let ops = vec![
        create(),
        Operation::Stop,
        Operation::Archive,
        Operation::StopMember {
            member: "m1".into(),
        },
        Operation::ArchiveMember {
            member: "m1".into(),
        },
        Operation::ChangeSettings {
            settings: Settings::default(),
        },
        Operation::DecideCard {
            card: 1,
            decision: "approve".into(),
        },
    ];
    for op in ops {
        refused(h.run(&lead, op), code::NOT_ALLOWED);
    }
}
fn facts(o: TaskObservation) -> Vec<Operation> {
    vec![
        Operation::ObserveTask {
            node: 1,
            observation: o,
        },
        Operation::HandoutIntent {
            node: 1,
            member: "m1".into(),
            effect_request_id: uuid::Uuid::new_v4().to_string(),
        },
        Operation::HandoutOutcome {
            effect_request_id: uuid::Uuid::new_v4().to_string(),
            outcome: HandoutOutcome::Submitted {
                receipt: "Example".into(),
            },
        },
        Operation::ReopenIntent {
            node: 1,
            member: "m1".into(),
            effect_request_id: uuid::Uuid::new_v4().to_string(),
        },
        Operation::ReopenOutcome {
            effect_request_id: uuid::Uuid::new_v4().to_string(),
            outcome: HandoutOutcome::Submitted {
                receipt: "Example".into(),
            },
        },
        Operation::SetMemberSession {
            member: "m2".into(),
            session: fixture().members[2].session.clone().unwrap(),
            account: "example".into(),
            worktree: "/workspace/example".into(),
            ownership_nonce: "example".into(),
        },
        Operation::SetMemberState {
            member: "m1".into(),
            state: MemberState::Paused,
        },
        Operation::RecordReport {
            node: 1,
            generation: 1,
            member: "m1".into(),
            text: "Example".into(),
            commit: COMMIT.into(),
            notes: Vec::new(),
        },
        Operation::RecordIntegration {
            decision: 1,
            node: 1,
            commit: COMMIT.into(),
        },
        Operation::RecordIntegrationConflict {
            decision: 1,
            node: 1,
            conflicts: vec!["example.rs".into()],
        },
    ]
}
#[test]
fn all_fact_records_refuse_person_lead_member_and_other_through_apply() {
    let mut h = Harness::new();
    let lead = h.bind("lead");
    let member = h.bind("m1");
    let node = h.add(&person(), 1);
    let o = h.observation(node, TaskStage::Todo);
    for actor in [person(), lead, member, other()] {
        for op in facts(o.clone()) {
            refused(h.run(&actor, op), code::NOT_ALLOWED);
        }
    }
}
#[test]
fn exact_binding_and_person_are_checked_before_mutation() {
    let mut h = Harness::new();
    let mut lead = h.bind("lead");
    if let Actor::Session { session, .. } = &mut lead {
        session.session_id = "run_id:different".into();
    }
    refused(h.run(&lead, Operation::Pause), code::NOT_ALLOWED);
    let wrong = Actor::Person {
        person: "someone-else".into(),
        origin: PersonOrigin::DesktopUi,
    };
    refused(h.run(&wrong, Operation::Pause), code::NOT_ALLOWED);
}
