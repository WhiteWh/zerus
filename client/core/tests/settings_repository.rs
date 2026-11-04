use serde_json::{json, Value};
use std::{fs, time::Duration};
use zerus_client_core::{
    repository::{Pending, Store},
    settings::{repository::Checkpoint, Settings},
};
const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn pending() -> Pending {
    Pending {
        request_id: "00000000-0000-4000-8000-000000000001".into(),
        url: "https://example.org/team/repo.git".into(),
        parent: "/srv/example".into(),
        name: "repo".into(),
        path: "/srv/example/repo".into(),
    }
}
#[test]
fn clone_checkpoint_is_private_durable_and_exactly_replaced() {
    let dir =
        std::env::temp_dir().join(format!("zerus-repository-settings-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    let settings = Settings::open(path.clone(), None);
    let initial = settings.repository_checkpoint(KEY).unwrap();
    let mut next = initial.clone();
    next.apply(&Store::Pending(pending()));
    settings
        .replace_repository_checkpoint(KEY, &initial, next.clone())
        .unwrap();
    assert!(!settings.all().contains_key("repository_clones"));
    assert!(settings.set("repository_clones", Value::Null).is_err());
    let bytes = fs::read(&path).unwrap();
    let raw: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        raw["repository_clones"]["entries"][KEY]["pending"]["request_id"],
        pending().request_id
    );
    assert!(settings
        .replace_repository_checkpoint(KEY, &initial, Checkpoint::default())
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(settings);
    let settings = Settings::open(path.clone(), None);
    assert_eq!(settings.repository_checkpoint(KEY).unwrap(), next);
    let mut retired = next.clone();
    retired.apply(&Store::Retired);
    settings
        .replace_repository_checkpoint(KEY, &next, retired.clone())
        .unwrap();
    assert_eq!(retired.previous, Some(pending()));
    assert!(retired.pending.is_none());
    assert!(settings.flush_within(Duration::from_secs(2)));
    drop(settings);
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn future_clone_checkpoint_is_refused_without_rewriting_it() {
    let dir = std::env::temp_dir().join(format!("zerus-repository-future-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    let bytes =
        json!({"version":1,"values":{},"repository_clones":{"version":2,"entries":{}}}).to_string();
    fs::write(&path, &bytes).unwrap();
    let s = Settings::open(path.clone(), None);
    assert!(s.repository_checkpoint(KEY).is_err());
    assert!(s
        .replace_repository_checkpoint(KEY, &Checkpoint::default(), Checkpoint::default())
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    drop(s);
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn creating_project_and_link_is_atomic_on_folder_refusal() {
    use zerus_client_core::swarm::{
        edit::{self, Intent},
        folder_edit::Change,
    };
    let original = json!({"version":2,"projects":[{"id":"ungrouped","name":"Inbox","folders":[],"sessions":[]}]});
    let id = "00000000-0000-4000-8000-000000000001";
    let make = |path: &str| Intent::CreateFolder {
        id: id.into(),
        name: "Example".into(),
        folder: Change::Set {
            original: None,
            id: "00000000-0000-4000-8000-000000000002".into(),
            machine: "workstation".into(),
            path: path.into(),
            name: String::new(),
        },
    };
    assert!(edit::apply(&original, &make("relative")).is_err());
    assert_eq!(original["projects"].as_array().unwrap().len(), 1);
    let next = edit::apply(&original, &make("/srv/example/repo")).unwrap();
    assert_eq!(next["projects"][0]["id"], id);
    assert_eq!(
        next["projects"][0]["folders"][0]["path"],
        "/srv/example/repo"
    );
    assert!(edit::apply(&next, &make("/srv/example/repo")).is_err());
}

#[test]
fn unknown_pending_fields_are_retained_and_refuse_execution_checkpoint_updates() {
    let dir = std::env::temp_dir().join(format!("zerus-repository-unknown-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    let mut record = serde_json::to_value(pending()).unwrap();
    record["future_guard"] = json!(true);
    let bytes=json!({"version":1,"values":{},"repository_clones":{"version":1,"entries":{KEY:{"parent":null,"pending":record,"previous":null}}}}).to_string();
    fs::write(&path, &bytes).unwrap();
    let settings = Settings::open(path.clone(), None);
    assert!(settings.repository_checkpoint(KEY).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), bytes);
    drop(settings);
    fs::remove_dir_all(dir).unwrap();
}
