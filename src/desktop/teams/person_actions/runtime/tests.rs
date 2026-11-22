//! Canonical rows come from the operation and swarm producers. Validated
//! fixtures model native originals; these tests do not attest a GUI gesture.
use super::*;
use crate::teams::ops::tests::{Harness, TEAM};
use zerus_client_core::upstream_teams::action::TeamAction;
mod controls;
mod placement;

fn sample() -> (Harness, Runtime, Arc<wire::Plan>, Arc<platform::FileLock>) {
    sample_for(Harness::new())
}

fn sample_for(h: Harness) -> (Harness, Runtime, Arc<wire::Plan>, Arc<platform::FileLock>) {
    let config = Config {
        home: h.temp.path().display().to_string(),
        dir: h.temp.path().join("config"),
        state: h.temp.path().to_owned(),
        selves: vec![],
        peers: vec![],
        base_peers: vec![],
        peers_from_environment: false,
        machines: Default::default(),
        tab: false,
        tab_colors: String::new(),
    };
    platform::private_dir(&config.dir).unwrap();
    crate::swarm::fixture_project(
        &config,
        "22222222-2222-4222-8222-222222222222",
        "example-project",
    );
    let scope = Scope {
        configuration: "a".repeat(64),
        service_epoch: uuid::Uuid::new_v4().to_string(),
    };
    let owner = Arc::new(platform::FileLock::exclusive(tempfile::tempfile().unwrap()).unwrap());
    let runtime = Runtime::new(
        &config,
        scope,
        owner.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let plan = Arc::new(wire::Plan {
        request_id: uuid::Uuid::new_v4().to_string(),
        team: TEAM.into(),
        expected_revision: h.store.team(TEAM).unwrap().unwrap().revision,
        action: wire::Action::Team {
            change: Box::new(TeamAction::RenameTeam {
                name: "Reviewed native worker name".into(),
            }),
        },
    });
    (h, runtime, plan, owner)
}
fn take(queue: &Queue) -> Value {
    serde_json::from_slice(&queue.next().expect("worker response")).unwrap()
}
fn stage(runtime: &mut Runtime, plan: &wire::Plan) {
    let body = serde_json::to_vec(plan).unwrap();
    let mut complete = false;
    for (ordinal, bytes) in body.chunks(wire::PIECE_BYTES).enumerate() {
        let piece = wire::Piece::from_bytes(
            uuid::Uuid::new_v4().to_string(),
            runtime.scope.clone(),
            plan.reference().unwrap(),
            ordinal as u16,
            bytes,
        )
        .unwrap();
        let result = runtime.stage(Validated::fixture(
            &json!({"op":"team_person_plan_piece","piece":piece}),
        ));
        assert_eq!(result["ok"], true, "{result}");
        complete = result["complete"] == true;
    }
    assert!(complete);
}

#[test]
fn native_v2_review_keeps_every_field_of_a_valid_dependency_heavy_draft() {
    use crate::teams::ops::tests::person;
    use zerus_team_plan::{
        draft::{DraftInput, DraftNode},
        model::{Dep, DepKind, NodeKind},
    };

    let (mut h, mut runtime, _, _owner) = sample();
    h.settings(|settings| settings.caps.max_phase_tasks = 200);
    let draft = DraftInput {
        title: "Reviewed graph boundary".into(),
        barrier: false,
        nodes: (1..=200)
            .map(|id| DraftNode {
                id,
                title: format!("Task {id}"),
                text: String::new(),
                criteria: vec!["Tests pass".into(); 32],
                role: "developer".into(),
                pipeline: "reviewed".into(),
                deps: (1..id.min(66))
                    .map(|node| Dep {
                        node,
                        kind: DepKind::Hard,
                    })
                    .collect(),
                assignee: None,
                writes: vec![],
                estimate_minutes: 1,
                kind: NodeKind::Task,
            })
            .collect(),
    };
    assert!(serde_json::to_vec(&draft).unwrap().len() < 250_000);
    assert!(serde_json::to_vec_pretty(&draft).unwrap().len() > 64 * 8192);
    // Prove that this is accepted data through the real operation writer,
    // using only the explicit native-admission fixture for the seed action.
    h.run(
        &person(),
        ops::Operation::SubmitDraftPhase {
            phase: 1,
            draft: draft.clone(),
        },
    )
    .unwrap();
    let plan = wire::Plan {
        request_id: uuid::Uuid::new_v4().to_string(),
        team: TEAM.into(),
        expected_revision: h.store.team(TEAM).unwrap().unwrap().revision,
        action: wire::Action::Team {
            change: Box::new(TeamAction::EditDraftPhase {
                phase: 1,
                generation: 1,
                draft: draft.clone(),
            }),
        },
    };
    assert!(plan.valid());
    stage(&mut runtime, &plan);
    let queue = Arc::new(Queue::default());
    runtime.query(&json!({"op":"team_person_action_prepare","scope":runtime.scope,"plan":plan.reference()}),queue.clone(),1).unwrap();
    let value = take(&queue)["value"].clone();
    assert_eq!(
        value["ok"], true,
        "valid draft must reach native review: {value}"
    );
    let review: wire::Review = serde_json::from_value(value["review"].clone()).unwrap();
    assert!(review.valid());
    let displayed = review
        .details
        .iter()
        .filter(|(key, _)| key.starts_with("draft ("))
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(
        serde_json::from_str::<DraftInput>(&displayed).unwrap(),
        draft
    );
    let queue = Arc::new(Queue::default());
    runtime
        .action(
            Validated::fixture(&json!({"op":"team_person_action","intent":review.intent})),
            queue.clone(),
            2,
        )
        .unwrap();
    let result = take(&queue)["value"].clone();
    assert_eq!(result["reply"]["ok"], true, "{result}");
    assert_eq!(h.store.phase_draft(TEAM, 1).unwrap().generation, Some(2));
}

#[test]
fn native_v2_worker_prepares_executes_and_only_reads_original_results() {
    let (h, mut runtime, plan, _owner) = sample();
    stage(&mut runtime, &plan);
    let queue = Arc::new(Queue::default());
    runtime.query(&json!({"op":"team_person_action_prepare","scope":runtime.scope,"plan":plan.reference()}),queue.clone(),1).unwrap();
    let value = take(&queue);
    let review: wire::Review = serde_json::from_value(value["value"]["review"].clone()).unwrap();
    assert_eq!(review.team_name, h.store.team(TEAM).unwrap().unwrap().name);
    assert_eq!(review.intent.plan, plan.reference().unwrap());
    let queue = Arc::new(Queue::default());
    runtime
        .action(
            Validated::fixture(&json!({"op":"team_person_action","intent":review.intent})),
            queue.clone(),
            2,
        )
        .unwrap();
    assert!(
        runtime.plans.get(&review.intent.plan).is_err(),
        "spent action data must not fill the bounded staging cache"
    );
    let original = take(&queue)["value"].clone();
    assert_eq!(original["reply"]["ok"], true, "{original}");
    assert_eq!(
        h.store.team(TEAM).unwrap().unwrap().name,
        "Reviewed native worker name"
    );
    // Read-only result lookup needs no staged plan and may run in a new epoch.
    runtime.plans = Cache::new(runtime.scope.clone());
    runtime.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    let queue = Arc::new(Queue::default());
    runtime
        .query(
            &json!({"op":"team_person_action_result","intent":review.intent}),
            queue.clone(),
            3,
        )
        .unwrap();
    assert_eq!(take(&queue)["value"], original);
    let mut foreign = review.intent;
    foreign.request = "e".repeat(64);
    let queue = Arc::new(Queue::default());
    runtime
        .query(
            &json!({"op":"team_person_action_result","intent":foreign}),
            queue.clone(),
            4,
        )
        .unwrap();
    assert_eq!(
        take(&queue)["value"]["code"],
        "team_person_action_evidence_unavailable"
    );
    assert_eq!(
        h.store.team(TEAM).unwrap().unwrap().revision,
        plan.expected_revision + 1
    );
}

#[test]
fn native_v2_workers_are_bounded_and_one_wait_does_not_block_the_actor() {
    let (_h, mut runtime, _plan, owner) = sample();
    assert_eq!(
        Arc::strong_count(&owner),
        1,
        "an idle bridge must not prevent service retirement"
    );
    let mut releases = Vec::new();
    for index in 0..WORKERS {
        let (send, receive) = std::sync::mpsc::channel();
        releases.push(send);
        runtime
            .start(Arc::new(Queue::default()), index as u64, move || {
                receive.recv().unwrap();
                json!({"fixture":index})
            })
            .unwrap();
    }
    assert_eq!(runtime.workers.len(), WORKERS);
    assert_eq!(
        Arc::strong_count(&owner),
        WORKERS + 1,
        "each physical worker retains the service owner"
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    assert_eq!(
        runtime
            .start(Arc::new(Queue::default()), 100, move || {
                observed.fetch_add(1, Ordering::SeqCst);
                Value::Null
            })
            .unwrap_err()
            .code,
        code::STORE_BUSY
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // No scheduler-time assertion: all original workers are still waiting
    // while the actor has already returned the bounded refusal above.
    for release in releases {
        release.send(()).unwrap();
    }
    drop(runtime);
    assert_eq!(Arc::strong_count(&owner), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn native_task_original_is_reserved_before_document_and_only_its_receipt_recovers() {
    let (h, mut runtime, _, _owner) = sample();
    let home = h.store.team(TEAM).unwrap().unwrap().home;
    crate::swarm::fixture_project(&runtime.config, &home, "example-project");
    {
        let tx = h
            .store
            .db
            .as_ref()
            .unwrap()
            .unchecked_transaction()
            .unwrap();
        store::person_actions::migrate(&tx).unwrap();
        tx.commit().unwrap();
    }
    let draft = wire::TaskDraft {
        request_id: uuid::Uuid::new_v4().to_string(),
        team: TEAM.into(),
        action: wire::Action::NewTask {
            project: "example-project".into(),
            title: "Native task original".into(),
            backlog: true,
        },
    };
    let queue = Arc::new(Queue::default());
    runtime
        .query(
            &json!({"op":"team_person_task_prepare","scope":runtime.scope,"draft":draft}),
            queue.clone(),
            1,
        )
        .unwrap();
    let value = take(&queue)["value"].clone();
    assert_eq!(value["ok"], true, "{value}");
    let plan: wire::Plan = serde_json::from_value(value["plan"].clone()).unwrap();
    let review: wire::Review = serde_json::from_value(value["review"].clone()).unwrap();
    stage(&mut runtime, &plan);
    let queue = Arc::new(Queue::default());
    runtime
        .action(
            Validated::fixture(&json!({"op":"team_person_action","intent":review.intent})),
            queue.clone(),
            2,
        )
        .unwrap();
    let original = take(&queue)["value"].clone();
    assert_eq!(original["reply"]["ok"], true, "{original}");
    assert_eq!(original["reply"]["task"]["uuid"], draft.request_id);
    assert_eq!(original["reply"]["team"]["queued"], true, "{original}");
    let db = h.store.db.as_ref().unwrap();
    let mut query = db.prepare("SELECT id,content FROM requests WHERE team=?1 AND json_extract(content,'$.op')='observe_task'").unwrap();
    let observations: Vec<(String, String)> = query
        .query_map([TEAM], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        observations.len(),
        1,
        "the admitted queue has one initial observation original"
    );
    let observation: ops::Request = serde_json::from_str(&observations[0].1).unwrap();
    assert_eq!(observations[0].0, observation.request_id);
    assert_ne!(
        observation.request_id, draft.request_id,
        "observation custody cannot spend the intake UUID"
    );
    let saved = store::person_actions::original(db, &review.intent).unwrap();
    assert_eq!(saved.request.request_id, draft.request_id);
    assert_eq!(saved.result, Some(original["reply"].clone()));
    let project =
        zerus_replica::ProjectStore::open(&h.temp.path().join("replica"), "example-project")
            .unwrap();
    let doc = project
        .admit_until(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .unwrap()
        .load_complete(&draft.request_id)
        .unwrap();
    assert_eq!(doc.team().unwrap().unwrap().team, TEAM);
    assert_eq!(doc.stage().unwrap(), zerus_replica::Stage::Backlog);
    drop(project);
    runtime.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    let queue = Arc::new(Queue::default());
    runtime
        .query(
            &json!({"op":"team_person_action_result","intent":review.intent}),
            queue.clone(),
            3,
        )
        .unwrap();
    assert_eq!(take(&queue)["value"], original);
    runtime.plans = Cache::new(runtime.scope.clone());
    stage(&mut runtime, &plan);
    let queue = Arc::new(Queue::default());
    runtime
        .action(
            Validated::fixture(&json!({"op":"team_person_action","intent":review.intent})),
            queue.clone(),
            4,
        )
        .unwrap_err();
    assert_eq!(h.store.queue(TEAM, 200, None).unwrap().rows.len(), 1);
}
