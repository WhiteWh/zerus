//! A phone projection and serialized source claims never grant native custody.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use zerus_replica::{
    phone::{answer, Exchange, Request},
    Actor, Document, Inventory, NewTask, ProjectStore, Stage, Target, TaskDoc, Update,
};

const NODE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn task(host: &ProjectStore) -> TaskDoc {
    let mut task = TaskDoc::create(NewTask {
        project: host.project_id(),
        id: "T-mobile",
        text: "The admitted original",
        stage: Stage::Todo,
        target: Target::Any,
        author: Actor::Person,
        rank: "V",
        now: 1,
    })
    .unwrap();
    host.save(&mut task).unwrap();
    assert_eq!(task.schema(), 8);
    task
}

fn unqualified(claim: bool) {
    let root = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let original = task(&host);
    let seal = host.admitted_cutover_identity(original.uuid()).unwrap();
    assert!(seal.is_some());
    // Candidate bytes can repeat a real seal and be causally well formed.
    // Neither property authenticates the source of a mobile mutation.
    let mut candidate = TaskDoc::from_bytes(&original.snapshot().unwrap()).unwrap();
    candidate
        .edit_text("An unqualified mobile write", &Actor::Person, 2)
        .unwrap();
    let mut request = json!({"schema":1,"project":"project-1",
        "inventory":{"project":{},"tasks":{}},
        "updates":[{"task":original.uuid(),"bytes":STANDARD.encode(candidate.snapshot().unwrap())}]});
    if claim {
        request["mobile_writer"] = json!({"device":"phone-1","store":NODE,
            "project":"project-1","task":original.uuid(),"seal":seal,"authenticated":true});
    }
    let response = answer(&host, serde_json::from_value(request).unwrap());
    drop(host);
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    assert_eq!(
        host.load(original.uuid()).unwrap().view().unwrap().text,
        "The admitted original",
        "canonical schema-8 task changed after an unqualified exchange"
    );
    let response = response.unwrap();
    assert_eq!(
        response.imported, 0,
        "unqualified phone bytes were imported"
    );
    assert!(response
        .refused
        .iter()
        .any(|refusal| refusal.uuid == original.uuid() && !refusal.error.is_empty()));
    assert_eq!(
        host.load(original.uuid()).unwrap().view().unwrap().text,
        "The admitted original"
    );
    assert_eq!(
        host.admitted_cutover_identity(original.uuid()).unwrap(),
        seal
    );
}

#[test]
fn an_unqualified_phone_exchange_cannot_mutate_an_admitted_sealed_task() {
    unqualified(false);
}

#[test]
fn serialized_mobile_source_claims_do_not_qualify_the_exchange() {
    unqualified(true);
}

#[cfg(feature = "test-support")]
#[test]
fn a_refused_migrated_recovery_preserves_every_task_file() {
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, fs};
    let root = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let mut legacy = TaskDoc::create(NewTask {
        project: "project-1",
        id: "T-legacy",
        text: "The migrated original",
        stage: Stage::Todo,
        target: Target::Any,
        author: Actor::Person,
        rank: "V",
        now: 1,
    })
    .unwrap();
    legacy.floor().unwrap();
    // A literal pre-upgrade writer is needed here: born-sealed tasks have
    // no readable .managed mark and never enter this recovery branch.
    host.save_legacy_for_test(&mut legacy).unwrap();
    let (own, frozen) = host.prepare_cutover(legacy.uuid(), NODE).unwrap();
    let mut proposal = TaskDoc::from_bytes(&frozen).unwrap();
    proposal
        .seal_cutover(
            BTreeMap::from([(NODE.into(), own.clone())]),
            BTreeMap::new(),
        )
        .unwrap();
    host.adopt_cutover(&proposal.snapshot().unwrap(), Some(&own))
        .unwrap();
    host.settle_cutover(legacy.uuid()).unwrap();
    let original = host.load(legacy.uuid()).unwrap();
    assert_eq!(original.schema(), 8);
    let folder = host.dir().join("tasks");
    assert!(folder
        .join(format!("{}.managed", original.uuid()))
        .is_file());
    let mut candidate = TaskDoc::from_bytes(&original.snapshot().unwrap()).unwrap();
    candidate
        .edit_text("An unqualified recovery", &Actor::Person, 2)
        .unwrap();
    let snapshot = folder.join(format!("{}.loro", original.uuid()));
    fs::write(&snapshot, b"synthetic corrupt snapshot").unwrap();
    assert!(host.load(original.uuid()).is_err());
    let files = || {
        let mut files: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name(),
                    format!("{:x}", Sha256::digest(fs::read(entry.path()).unwrap())),
                )
            })
            .collect();
        assert!(files.len() <= 16);
        files.sort();
        files
    };
    let before = files();
    for _ in 0..2 {
        let request = json!({"schema":1,"project":"project-1",
            "inventory":{"project":{},"tasks":{}},
            "updates":[{"task":original.uuid(),"bytes":STANDARD.encode(candidate.snapshot().unwrap())}]});
        let response = answer(&host, serde_json::from_value(request).unwrap()).unwrap();
        assert_eq!(response.imported, 0);
        assert_eq!(response.refused.len(), 1);
        assert!(response.refused[0]
            .error
            .contains("authenticated mobile edit receipt"));
        assert_eq!(files(), before, "a refused source changed task custody");
    }
    assert_eq!(fs::read(snapshot).unwrap(), b"synthetic corrupt snapshot");
}

#[test]
fn an_unchanged_visible_projection_does_not_hide_a_sealed_history_write() {
    let root = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let original = task(&host);
    let before = original.applied_version();
    let mut candidate = TaskDoc::from_bytes(&original.snapshot().unwrap()).unwrap();
    candidate
        .edit_text("A transient operation", &Actor::Person, 2)
        .unwrap();
    candidate
        .edit_text("The admitted original", &Actor::Person, 3)
        .unwrap();
    let request = json!({"schema":1,"project":"project-1",
        "inventory":{"project":{},"tasks":{}},
        "updates":[{"task":original.uuid(),"bytes":STANDARD.encode(candidate.snapshot().unwrap())}]});
    let response = answer(&host, serde_json::from_value(request).unwrap()).unwrap();
    drop(host);
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let current = host.load(original.uuid()).unwrap();
    assert_eq!(current.applied_version(), before);
    assert_eq!(current.view().unwrap().text, "The admitted original");
    assert_eq!(response.imported, 0);
    assert_eq!(response.refused.len(), 1);
    assert_eq!(response.refused[0].uuid, original.uuid());
}

#[test]
fn repeating_the_same_sealed_state_is_still_a_read_only_no_op() {
    let root = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let original = task(&host);
    let before = original.applied_version();
    let request = json!({"schema":1,"project":"project-1",
        "inventory":{"project":{},"tasks":{}},
        "updates":[{"task":original.uuid(),"bytes":STANDARD.encode(original.snapshot().unwrap())}]});
    let response = answer(&host, serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(response.imported, 0);
    assert!(response.refused.is_empty());
    assert_eq!(
        host.load(original.uuid()).unwrap().applied_version(),
        before
    );
}

#[test]
fn the_projection_receiver_keeps_receiving_sealed_state_read_only() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(a.path(), "project-1").unwrap();
    let original = task(&host);
    let phone = ProjectStore::open_phone(b.path(), "project-1").unwrap();
    let request = json!({"schema":1,"project":"project-1",
        "inventory":{"project":{},"tasks":{}},
        "updates":[{"task":original.uuid(),"bytes":STANDARD.encode(original.snapshot().unwrap())}]});
    let response = answer(&phone, serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(response.imported, 1);
    assert!(response.refused.is_empty());
    let received = phone.load(original.uuid()).unwrap();
    assert!(!received.view().unwrap().writable);
    assert_eq!(received.view().unwrap().text, "The admitted original");
    assert!(phone
        .admitted_cutover_identity(original.uuid())
        .unwrap()
        .is_none());
}

#[test]
fn the_phone_boundary_does_not_replace_native_import_admission() {
    let root = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(root.path(), "project-1").unwrap();
    let original = task(&host);
    let mut candidate = TaskDoc::from_bytes(&original.snapshot().unwrap()).unwrap();
    candidate
        .edit_text("A native admitted update", &Actor::Person, 2)
        .unwrap();
    let imported = host
        .import_updates(vec![Update {
            document: Document::Task(original.uuid().into()),
            bytes: candidate.snapshot().unwrap(),
        }])
        .unwrap();
    assert!(imported.refused.is_empty());
    assert_eq!(imported.changed, 1);
    assert_eq!(
        host.load(original.uuid()).unwrap().view().unwrap().text,
        "A native admitted update"
    );
}

#[test]
fn an_unqualified_sealed_task_does_not_stop_an_unrelated_legacy_document() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(a.path(), "project-1").unwrap();
    let original = task(&host);
    let mut candidate = TaskDoc::from_bytes(&original.snapshot().unwrap()).unwrap();
    candidate
        .edit_text("An unqualified mobile write", &Actor::Person, 2)
        .unwrap();
    let phone = ProjectStore::open_phone(b.path(), "project-1").unwrap();
    let mut legacy = TaskDoc::create(NewTask {
        project: "project-1",
        id: "T-legacy",
        text: "Unrelated legacy task",
        stage: Stage::Todo,
        target: Target::Any,
        author: Actor::Person,
        rank: "V",
        now: 1,
    })
    .unwrap();
    phone.save(&mut legacy).unwrap();
    assert!(legacy.schema() <= 2);
    let request = json!({"schema":1,"project":"project-1",
        "inventory":{"project":{},"tasks":{}},
        "updates":[
            {"task":original.uuid(),"bytes":STANDARD.encode(candidate.snapshot().unwrap())},
            {"task":legacy.uuid(),"bytes":STANDARD.encode(legacy.snapshot().unwrap())}]});
    let answer = answer(&host, serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(answer.imported, 1);
    assert_eq!(answer.refused.len(), 1);
    assert_eq!(answer.refused[0].uuid, original.uuid());
    assert_eq!(
        host.load(legacy.uuid()).unwrap().view().unwrap().text,
        "Unrelated legacy task"
    );
}

fn empty_request(project: &str) -> Request {
    Request {
        schema: 1,
        project: project.into(),
        inventory: Inventory::default(),
        range: None,
        updates: vec![],
        resume: None,
    }
}

#[test]
fn native_mobile_ingress_cannot_receive_unadopted_sealed_state() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(a.path(), "project-1").unwrap();
    let task = task(&host);
    let native = ProjectStore::open(b.path(), "project-1").unwrap();
    let request = empty_request("project-1");
    let offered = answer(&host, request.clone()).unwrap();
    assert_eq!(offered.updates.len(), 1);
    let refused = answer(
        &native,
        Request {
            updates: offered.updates,
            ..request
        },
    )
    .unwrap();
    assert_eq!(refused.imported, 0);
    assert_eq!(refused.refused.len(), 1);
    assert_eq!(refused.refused[0].uuid, task.uuid());
    assert!(refused.refused[0]
        .error
        .contains("has not adopted the admitted attempts cutover"));
    assert_eq!(native.live_inventory().unwrap().0, Inventory::default());
    assert_eq!(refused.digest, Inventory::default().digest());
    assert_ne!(refused.digest, offered.digest);
    let (updates, _) = host
        .updates_for(
            &host.live_inventory().unwrap().0,
            &Inventory::default(),
            usize::MAX,
        )
        .unwrap();
    let native_import = native.import_updates(updates).unwrap();
    assert_eq!(native_import.changed, 0);
    assert_eq!(native_import.refused.len(), 1);
    assert_eq!(native_import.refused[0].uuid, task.uuid());
    assert_eq!(native.live_inventory().unwrap().0, Inventory::default());
}

#[test]
fn the_phone_refuses_updates_for_another_task_or_project() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(a.path(), "project-1").unwrap();
    let task = task(&host);
    for (project, wrong_id, expected_refusal) in [
        ("project-1", Some(NODE), "the update is for another task"),
        ("project-2", None, "the task belongs to another project"),
    ] {
        let phone = ProjectStore::open_phone(b.path(), project).unwrap();
        let mut exchange = Exchange::new();
        assert!(exchange.next_request(&phone).unwrap().is_some());
        let mut offered = answer(&host, empty_request("project-1")).unwrap();
        if let Some(wrong_id) = wrong_id {
            offered.updates[0].task = Some(wrong_id.into());
        }
        let progress = exchange.accept(&phone, offered).unwrap();
        assert_eq!(progress.changed, 0);
        assert_eq!(progress.refused_here.len(), 1);
        assert_eq!(
            progress.refused_here[0].uuid,
            wrong_id.unwrap_or(task.uuid())
        );
        assert_eq!(progress.refused_here[0].error, expected_refusal);
        assert_eq!(phone.live_inventory().unwrap().0, Inventory::default());
    }
}

#[test]
fn a_phone_receives_the_admitted_task_without_obtaining_native_authority() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host = ProjectStore::open(a.path(), "project-1").unwrap();
    let task = task(&host);
    let phone = ProjectStore::open_phone(b.path(), "project-1").unwrap();
    let mut exchange = Exchange::new();
    let mut changed = 0;
    while let Some(request) = exchange.next_request(&phone).unwrap() {
        let progress = exchange
            .accept(&phone, answer(&host, request).unwrap())
            .unwrap();
        changed += progress.changed;
        assert_eq!(changed, 1, "the first phone page must adopt the one task");
        assert!(progress.refused_here.is_empty());
        assert!(progress.refused_there.is_empty());
    }
    let host_inventory = host.live_inventory().unwrap().0;
    assert!(host_inventory.project.is_empty());
    assert_eq!(host_inventory.tasks.len(), 1);
    drop(phone);
    let phone = ProjectStore::open_phone(b.path(), "project-1").unwrap();
    assert_eq!(phone.live_inventory().unwrap().0, host_inventory);
    let mut received = phone.load(task.uuid()).unwrap();
    assert_eq!(received.view().unwrap().text, "The admitted original");
    assert!(!received.view().unwrap().writable);
    assert!(received.edit_text("No permit", &Actor::Person, 2).is_err());
    assert!(phone.save(&mut received).is_err());
    assert!(phone.prepare_cutover(task.uuid(), NODE).is_err());
    assert!(phone.export_cutover(task.uuid()).is_err());
    assert!(phone
        .admitted_cutover_identity(task.uuid())
        .unwrap()
        .is_none());
    drop(phone);
    let native = ProjectStore::open(b.path(), "project-1").unwrap();
    assert!(native
        .admitted_cutover_identity(task.uuid())
        .unwrap()
        .is_none());
    assert!(native.export_cutover(task.uuid()).is_err());
}
