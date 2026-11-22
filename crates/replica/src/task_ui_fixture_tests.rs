//! Task rows for the Team board's component tests, made by the product writer.
use super::*;
use std::{fs, path::Path};

#[test]
fn team_intake_component_fixture() {
    let mut task = TaskDoc::create_named(
        NewTask {
            project: "example-project",
            id: "T-aaaa01",
            text: "Check team intake",
            stage: Stage::Backlog,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1_790_000_000_000,
        },
        "Check team intake",
        Some(("11111111-1111-4111-8111-111111111111", "fixture")),
    )
    .unwrap();
    let mut rows = vec![task.view().unwrap()];
    task.set_stage(Stage::Todo, &Actor::Person, 1_790_000_001_000)
        .unwrap();
    rows.push(task.view().unwrap());
    let agent = Actor::Agent(SessionKey {
        machine: "00000000-0000-4000-8000-000000000001".into(),
        session: "codex/example/worker".into(),
        session_id: "launch_id:00000000-0000-4000-8000-000000000002".into(),
    });
    task.record_start(&agent, 1_790_000_002_000).unwrap();
    rows.push(task.view().unwrap());
    task.record_report("Ready for review", &agent, 1_790_000_003_000)
        .unwrap();
    rows.push(task.view().unwrap());
    task.set_stage(Stage::Done, &Actor::Person, 1_790_000_004_000)
        .unwrap();
    rows.push(task.view().unwrap());
    // A document from a newer writer is readable but not writable by this
    // build, exactly as the schema compatibility controls construct it.
    task.set_stage(Stage::Todo, &Actor::Person, 1_790_000_005_000)
        .unwrap();
    task.meta().insert("schema", TASK_SCHEMA + 1).unwrap();
    task.doc.commit();
    rows.push(task.view().unwrap());
    let output = serde_json::to_string_pretty(&rows).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../client/src/ui/teams/fixtures/task-intake.json");
    if std::env::var_os("ZERUS_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &output).unwrap();
    }
    assert_eq!(fs::read_to_string(path).unwrap(), output);
}
