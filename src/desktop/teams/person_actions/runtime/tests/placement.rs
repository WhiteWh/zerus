use super::*;
use crate::teams::ops::tests::{create, person};
use zerus_team_plan::model::{Dep, DepKind};

fn plan(h: &Harness, change: TeamAction) -> wire::Plan {
    wire::Plan {
        team: TEAM.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        expected_revision: h.store.team(TEAM).unwrap().unwrap().revision,
        action: wire::Action::Team {
            change: Box::new(change),
        },
    }
}

fn review(runtime: &mut Runtime, plan: &wire::Plan) -> wire::Review {
    stage(runtime, plan);
    let queue = Arc::new(Queue::default());
    runtime
        .query(
            &json!({"op":"team_person_action_prepare","scope":runtime.scope,"plan":plan.reference()}),
            queue.clone(),
            1,
        )
        .unwrap();
    let value = take(&queue);
    assert_eq!(value["value"]["ok"], true, "{value}");
    serde_json::from_value(value["value"]["review"].clone()).unwrap()
}

fn execute(runtime: &mut Runtime, intent: wire::Intent) -> Value {
    let queue = Arc::new(Queue::default());
    runtime
        .action(
            Validated::fixture(&json!({"op":"team_person_action","intent":intent})),
            queue.clone(),
            2,
        )
        .unwrap();
    take(&queue)["value"]["reply"].clone()
}

fn route(node: u32, role: &str, pipeline: &str) -> TeamAction {
    TeamAction::Route {
        node,
        role: role.into(),
        pipeline: pipeline.into(),
        keep_task_fields: true,
        deps: vec![],
        writes: vec![],
        assignee: None,
    }
}

#[test]
fn native_placement_route_keeps_dependencies_and_writes_and_only_clears_a_changed_role() {
    let mut h = Harness::empty();
    let mut create = create();
    let ops::Operation::Create { bundle, .. } = &mut create else {
        unreachable!()
    };
    let mut alternate = bundle.pipelines[0].clone();
    alternate.id = "alternate".into();
    bundle.pipelines.push(alternate.clone());
    alternate.id = "analysis".into();
    alternate.implementer = "reviewer".into();
    alternate.stages.clear();
    bundle.pipelines.push(alternate);
    h.run(&person(), create).unwrap();
    let (mut h, mut runtime, _, _owner) = sample_for(h);
    h.bind("m1");
    let prerequisite = h.add(&person(), 1);
    let node = h.add(&person(), 2);
    let dependency = Dep {
        node: prerequisite,
        kind: DepKind::Soft,
    };
    h.run(
        &person(),
        ops::Operation::Route {
            node,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            deps: vec![dependency],
            writes: vec!["src/example.rs".into(), "docs/examples".into()],
            assignee: Some("m1".into()),
        },
    )
    .unwrap();

    let same = plan(&h, route(node, "developer", "alternate"));
    let reviewed = review(&mut runtime, &same);
    assert!(reviewed
        .details
        .iter()
        .any(|(key, text)| key.starts_with("pipeline") && text.contains("alternate")));
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    let after = h.node(node);
    assert_eq!(after.pipeline, "alternate");
    assert_eq!(after.assignee.as_deref(), Some("m1"));
    assert_eq!(after.deps, [dependency]);
    assert_eq!(after.writes, ["src/example.rs", "docs/examples"]);

    let changed = plan(&h, route(node, "reviewer", "analysis"));
    let reviewed = review(&mut runtime, &changed);
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    let after = h.node(node);
    assert_eq!(after.role, "reviewer");
    assert_eq!(after.pipeline, "analysis");
    assert_eq!(after.assignee, None);
    assert_eq!(after.deps, [dependency]);
    assert_eq!(after.writes, ["src/example.rs", "docs/examples"]);
    let recorded: String = h
        .store
        .db
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT content FROM requests WHERE id=?1",
            [&changed.request_id],
            |row| row.get(0),
        )
        .unwrap();
    let recorded: ops::Request = serde_json::from_str(&recorded).unwrap();
    assert_eq!(
        recorded.operation,
        ops::Operation::Route {
            node,
            role: after.role,
            pipeline: after.pipeline,
            deps: after.deps,
            writes: after.writes,
            assignee: None,
        }
    );
}

#[test]
fn native_placement_reassign_refuses_a_stale_revision_and_records_the_reason_on_success() {
    let (mut h, mut runtime, _, _owner) = sample();
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    let change = TeamAction::Reassign {
        node,
        member: "m2".into(),
        reason: "The replacement has the needed context.".into(),
    };
    let first = plan(&h, change.clone());
    let reviewed = review(&mut runtime, &first);
    h.run(&person(), ops::Operation::Hold { node }).unwrap();
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["code"], code::STALE_REVISION, "{result}");
    assert!(h.node(node).assignee.is_none());

    let fresh = plan(&h, change);
    let reviewed = review(&mut runtime, &fresh);
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(h.node(node).assignee.as_deref(), Some("m2"));
    let decisions = h.store.decisions(TEAM, 1, None).unwrap();
    assert_eq!(decisions.rows[0].kind, "reassign");
    assert_eq!(
        decisions.rows[0].text,
        "The replacement has the needed context."
    );
}

/// zerus-3u5yu (1), adopted from the #2527 review probe: the native
/// confirmation of a window Route shows what the service binds. The kept
/// dependencies and write paths, and an assignment the role change clears,
/// are named in the details the person confirms.
#[test]
fn a_role_changing_route_shows_the_kept_fields_and_the_cleared_assignee() {
    let mut h = Harness::empty();
    let mut create = create();
    let ops::Operation::Create { bundle, .. } = &mut create else {
        unreachable!()
    };
    let mut alternate = bundle.pipelines[0].clone();
    alternate.id = "analysis".into();
    alternate.implementer = "reviewer".into();
    alternate.stages.clear();
    bundle.pipelines.push(alternate);
    h.run(&person(), create).unwrap();
    let (mut h, mut runtime, _, _owner) = sample_for(h);
    h.bind("m1");
    let prerequisite = h.add(&person(), 1);
    let node = h.add(&person(), 2);
    h.run(
        &person(),
        ops::Operation::Route {
            node,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            deps: vec![Dep {
                node: prerequisite,
                kind: DepKind::Soft,
            }],
            writes: vec!["src/example.rs".into()],
            assignee: Some("m1".into()),
        },
    )
    .unwrap();
    let changed = plan(&h, route(node, "reviewer", "analysis"));
    let reviewed = review(&mut runtime, &changed);
    let detail = |key: &str| {
        reviewed
            .details
            .iter()
            .filter(|(name, _)| name.starts_with(key))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
            .join("")
    };
    assert_eq!(detail("assignee"), "m1 → none", "{:?}", reviewed.details);
    assert!(
        detail("deps").contains(&prerequisite.to_string()),
        "{:?}",
        reviewed.details
    );
    assert!(
        detail("writes").contains("src/example.rs"),
        "{:?}",
        reviewed.details
    );
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(h.node(node).assignee, None);
}

/// zerus-3u5yu (2), adopted from the #2527 review probe: a same-role window
/// Route of a task whose assignee was removed is not refused for that
/// assignee. The removed member is not bound again; the dialog says the
/// assignment is cleared, and the Route succeeds with no assignee.
#[test]
fn a_same_role_route_after_the_assignee_was_removed_clears_it_and_says_so() {
    let (mut h, mut runtime, _, _owner) = sample();
    h.bind("m1");
    h.bind("m2");
    let node = h.add(&person(), 1);
    h.run(
        &person(),
        ops::Operation::Route {
            node,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            deps: vec![],
            writes: vec![],
            assignee: Some("m1".into()),
        },
    )
    .unwrap();
    h.run(
        &person(),
        ops::Operation::ArchiveMember {
            member: "m1".into(),
        },
    )
    .unwrap();
    assert_eq!(h.node(node).assignee.as_deref(), Some("m1"));
    let same = plan(&h, route(node, "developer", "reviewed"));
    let reviewed = review(&mut runtime, &same);
    let assignee: Vec<_> = reviewed
        .details
        .iter()
        .filter(|(name, _)| name.starts_with("assignee"))
        .map(|(_, value)| value.clone())
        .collect();
    assert_eq!(assignee, ["m1 → none"], "{:?}", reviewed.details);
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(h.node(node).assignee, None);
    assert_eq!(h.node(node).role, "developer");
}

/// A same-role Route keeps an assignee who can still take the task, and the
/// dialog names it as kept.
#[test]
fn a_same_role_route_keeps_an_active_assignee_and_names_it() {
    let (mut h, mut runtime, _, _owner) = sample();
    h.bind("m1");
    let node = h.add(&person(), 1);
    h.run(
        &person(),
        ops::Operation::Route {
            node,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            deps: vec![],
            writes: vec![],
            assignee: Some("m1".into()),
        },
    )
    .unwrap();
    let same = plan(&h, route(node, "developer", "reviewed"));
    let reviewed = review(&mut runtime, &same);
    let assignee: Vec<_> = reviewed
        .details
        .iter()
        .filter(|(name, _)| name.starts_with("assignee"))
        .map(|(_, value)| value.clone())
        .collect();
    assert_eq!(assignee, ["m1"], "{:?}", reviewed.details);
    let result = execute(&mut runtime, reviewed.intent);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(h.node(node).assignee.as_deref(), Some("m1"));
}
