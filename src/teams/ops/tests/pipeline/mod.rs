use super::*;
mod creation;
mod delegation;
mod earlier_impl;
mod landing;
mod overrides;
mod removed_holder;
mod removed_stage;
mod stage_route;
mod review;
mod review2;
mod unstaffed_stage;
mod verdicts;

fn stages(h: &Harness, parent: u32) -> Vec<Node> {
    h.store
        .queue(TEAM, 200, None)
        .unwrap()
        .rows
        .into_iter()
        .filter(|n| n.kind == NodeKind::Stage && n.stage_of == Some(parent))
        .collect()
}

fn op(value: serde_json::Value) -> Operation {
    serde_json::from_value(value).unwrap()
}
fn creation(node: u32, id: &str) -> Operation {
    op(serde_json::json!({"op":"stage_task_intent","node":node,"effect_request_id":id}))
}
fn created(id: &str, n: u32) -> Operation {
    op(
        serde_json::json!({"op":"stage_task_outcome","effect_request_id":id,"outcome":{"status":"created","task":task(n)}}),
    )
}
fn wanted() -> (Harness, u32, u32) {
    let mut h = pipeline_harness("reviewed");
    h.bind("m1");
    h.bind("m3");
    let parent = h.add(&person(), 1);
    h.submit(parent, "m1");
    h.report(parent, "m1");
    let stage = stages(&h, parent)[0].id;
    (h, parent, stage)
}

fn pipeline_harness(id: &str) -> Harness {
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
        bundle.template.default_pipeline = id.into();
    }
    h.run(&person(), op).unwrap();
    h
}
fn verdict(node: u32, generation: u32, pass: bool) -> Operation {
    op(
        serde_json::json!({"op":"verdict","node":node,"generation":generation,"pass":pass,"text":"Checked the criteria; findings supplied"}),
    )
}
fn stage_attempt(h: &mut Harness, parent: u32, member: &str) -> u32 {
    let stage = stages(h, parent)
        .into_iter()
        .find(|n| n.task.is_none())
        .unwrap()
        .id;
    let id = uuid::Uuid::new_v4().to_string();
    h.run(&service(), creation(stage, &id)).unwrap();
    h.run(&service(), created(&id, 1000 + stage)).unwrap();
    h.observe(stage, TaskStage::Todo);
    h.submit(stage, member);
    stage
}
fn bound(h: &Harness, id: &str) -> Actor {
    Actor::Session {
        person: "local".into(),
        team: TEAM.into(),
        member: id.into(),
        session: h.store.member(TEAM, id).unwrap().unwrap().session.unwrap(),
    }
}
fn bind_tester(h: &mut Harness) {
    h.run(
        &service(),
        Operation::SetMemberSession {
            member: "m4".into(),
            session: SessionKey {
                machine: fixture().team.home,
                session_id: "launch_id:00000000-0000-4000-8000-000000000044".into(),
            },
            account: "account-example".into(),
            worktree: "/workspace/example-tester".into(),
            ownership_nonce: "nonce-tester".into(),
        },
    )
    .unwrap();
}
