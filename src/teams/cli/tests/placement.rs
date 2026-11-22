//! The inspector's choices and revision come from operation-written views.
use super::harness::*;
use crate::teams::{cli::actor::Caller, ops::Operation};
use serde_json::{json, Value};
use zerus_client_core::upstream_teams::{read, view};

fn fetch(h: &Harness, command: &read::TeamRead) -> Value {
    let args = command.args().unwrap();
    let (reply, exit) = h.cli(&Caller::Person, &args[1..args.len() - 1].join(" "), "");
    assert_eq!(exit, 0, "{reply}");
    reply
}

fn payload() -> Value {
    let h = Harness::new();
    let mut team = fixture().team;
    team.bundle.pipelines[0].stages.clear();
    let mut another = team.bundle.pipelines[0].clone();
    another.id = "analysis".into();
    another.name = "Analysis".into();
    another.implementer = "reviewer".into();
    team.bundle.pipelines.push(another);
    h.apply(
        &person(),
        Operation::Create {
            expected_template: None,
            delegations: None,
            project: team.project,
            name: team.name,
            goal: team.goal,
            home: team.home,
            bundle: Box::new(team.bundle),
            integration_branch: team.integration_branch,
            base_commit: team.base_commit,
        },
    );
    h.bind("lead", LEAD_SESSION);
    h.bind("m1", M1_SESSION);
    h.bind("m2", "launch_id:00000000-0000-4000-8000-000000000032");
    h.bind("m3", super::verdict::REVIEWER_SESSION);
    let reported = h.reported(1);
    let pending = h.add(2);
    h.apply(
        &person(),
        Operation::Route {
            node: pending,
            role: "developer".into(),
            pipeline: "reviewed".into(),
            deps: vec![],
            writes: vec!["src/example.rs".into()],
            assignee: Some("m2".into()),
        },
    );
    let status_read = read::TeamRead::Status { team: TEAM.into() };
    let queue_read = read::TeamRead::Queue {
        team: TEAM.into(),
        after: None,
    };
    let snapshot = || view::team(&fetch(&h, &status_read), &fetch(&h, &queue_read));
    let before = snapshot();
    h.apply(&person(), Operation::Hold { node: pending });
    let after = snapshot();
    json!({ "before": before, "after": after, "reported": reported, "pending": pending })
}

fn path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("client/src/ui/teams/fixtures/task-placement-view.json")
}

#[test]
fn placement_views_use_the_pinned_bundle_and_writer_revisions() {
    let actual = payload();
    assert_eq!(
        actual["before"]["routing_pipelines"],
        json!([
            {"id":"reviewed", "name":"Reviewed: review, lead", "role":"developer"},
            {"id":"analysis", "name":"Analysis", "role":"reviewer"}
        ])
    );
    assert_eq!(
        actual["after"]["revision"].as_u64().unwrap(),
        actual["before"]["revision"].as_u64().unwrap() + 1
    );
    let expected: Value = serde_json::from_slice(&std::fs::read(path()).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
#[ignore = "regenerates the inspector fixture through the actual queue and status reads"]
fn regenerate_task_placement_fixture() {
    std::fs::write(
        path(),
        format!("{}\n", serde_json::to_string_pretty(&payload()).unwrap()),
    )
    .unwrap();
}
