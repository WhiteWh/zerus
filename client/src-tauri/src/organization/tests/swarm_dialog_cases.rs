use super::*;
use crate::organization::swarm_dialog::{Action, View};
use zerus_client_core::swarm::sync::EXTERNAL_CHANGED;

fn local() -> Value {
    let mut value = full("d1");
    value["swarm_id"] = json!("local-swarm");
    value["machines"] = json!([{"id":"node-1","name":"Workstation","local":true}]);
    value
}
fn joined() -> Value {
    let mut value = local();
    value["digest"] = json!("d2");
    value["swarm_id"] = json!("peer-swarm");
    value["peers"] = json!({"fixture-peer":{"node":"peer-node","last_sync":1}});
    value
}
fn machines(revision: char) -> Value {
    json!({"local":"workstation","revision":revision.to_string().repeat(64),"machines":[{"alias":"fixture-peer","enabled":false}]})
}
fn remote() -> Value {
    json!({"node_id":"peer-node","swarm_id":"peer-swarm","same_swarm":false,"initialized":true,"projects":[{"id":"remote-alpha","name":"Remote Alpha"}]})
}
fn config() -> Value {
    json!({"machines":[{"stdout":machines('a')}],"preview":[{"stdout":remote()}],"get":[{"stdout":local()},{"stdout":joined()}],"join":[{"stdout":{"joined":true,"snapshot":joined()}}],"sync":[{"stdout":{"schema":1}}],"apply":[]})
}
fn fixture(config: Value, allowed: bool) -> (Fake, Organization, Arc<Mutex<Value>>) {
    let fake = Fake::new(json!([]));
    std::fs::write(
        fake.dir.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let org = fake.organization(&Pump::default(), allowed);
    let cache = Arc::new(Mutex::new(local()));
    let source = cache.clone();
    org.set_source(Arc::new(move || {
        let value = source.lock().unwrap();
        Some(OrganizationSnapshot {
            node_id: value["node_id"].as_str().unwrap().into(),
            swarm_id: value["swarm_id"].as_str().unwrap().into(),
            digest: value["digest"].as_str().unwrap().into(),
            base: value["organization"].clone(),
            versions: value["versions"].clone(),
        })
    }));
    org.observe(&state_of(&local()));
    (fake, org, cache)
}
fn json(view: View) -> Value {
    serde_json::to_value(view).unwrap()
}
fn view(org: &Organization, id: &str) -> Value {
    json(org.catalog_request(Some(id), None, false).unwrap())
}
fn idle(org: &Organization, id: &str) -> Value {
    wait_until("the catalog job", || view(org, id)["busy"] == false);
    view(org, id)
}
fn open(org: &Organization) -> String {
    let value = json(org.catalog_request(None, None, false).unwrap());
    let id = value["id"].as_str().unwrap().to_owned();
    idle(org, &id);
    id
}
fn preview(org: &Organization, id: &str) {
    org.catalog_request(
        Some(id),
        Some(Action::Preview("fixture-peer".into())),
        false,
    )
    .unwrap();
    wait_until("the catalog preview", || {
        view(org, id)["preview"].is_object()
    });
}
fn join(org: &Organization, id: &str) {
    org.catalog_request(
        Some(id),
        Some(Action::Join {
            mappings: vec![("alpha".into(), "remote-alpha".into())],
            keep: true,
        }),
        false,
    )
    .unwrap();
}
fn calls(fake: &Fake, verb: &str) -> Vec<Value> {
    std::fs::read_to_string(fake.dir.join("log.ndjson"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| {
            v["argv"]
                .as_array()
                .is_some_and(|a| a.iter().any(|s| s == verb))
        })
        .collect()
}
fn advance(org: &Organization, cache: &Mutex<Value>, value: Value) {
    *cache.lock().unwrap() = value.clone();
    org.observe(&state_of(&value));
}

#[test]
fn open_and_preview_are_read_only_and_join_waits_for_the_matching_live_source() {
    let (fake, org, cache) = fixture(config(), true);
    let id = open(&org);
    assert_eq!(view(&org, &id)["aliases"], json!(["fixture-peer"]));
    assert!(calls(&fake, "join").is_empty());
    assert!(calls(&fake, "get").is_empty());
    preview(&org, &id);
    assert_eq!(
        view(&org, &id)["preview"]["projects"][0]["id"],
        "remote-alpha"
    );
    join(&org, &id);
    wait_until("the post-call source barrier", || {
        calls(&fake, "get").len() == 2 && view(&org, &id)["busy"] == false
    });
    assert!(org.lock().sync.busy());
    assert!(!org.lock().sync.ready());
    assert_eq!(org.swarm_id(), "local-swarm");
    let mut wrong = joined();
    wrong["node_id"] = json!("different-node");
    advance(&org, &cache, wrong);
    assert!(
        org.lock().sync.busy(),
        "equal digest cannot qualify a different node"
    );
    advance(&org, &cache, joined());
    wait_until("the matched source", || org.lock().sync.ready());
    let done = view(&org, &id);
    assert_eq!(done["status"], "Connected to fixture-peer.");
    assert_eq!(done["error"], false);
    assert!(done["preview"].is_null());
    assert_eq!(calls(&fake, "join").len(), 1);
    let payload: Value =
        serde_json::from_str(calls(&fake, "join")[0]["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(payload["project_map"], json!({"alpha":"remote-alpha"}));
    assert_eq!(payload["node_id"], "peer-node");
    assert!(org
        .catalog_request(
            Some(&id),
            Some(Action::Join {
                mappings: vec![],
                keep: false
            }),
            false
        )
        .is_err());
    assert_eq!(calls(&fake, "join").len(), 1);
    fake.finish(org);
}

#[test]
fn changed_ssh_configuration_refuses_before_join_and_releases_only_after_a_fresh_read() {
    let mut settings = config();
    settings["machines"] =
        json!([{"stdout":machines('a')},{"stdout":machines('a')},{"stdout":machines('b')}]);
    settings["get"] = json!([{"stdout":local()}]);
    let (fake, org, _) = fixture(settings, true);
    let id = open(&org);
    preview(&org, &id);
    join(&org, &id);
    wait_until("the guarded refusal", || {
        !org.lock().sync.busy() && view(&org, &id)["error"] == true
    });
    assert!(view(&org, &id)["status"]
        .as_str()
        .unwrap()
        .contains("Machine settings changed"));
    assert!(calls(&fake, "join").is_empty());
    assert_eq!(calls(&fake, "get").len(), 1);
    fake.finish(org);
}

#[test]
fn local_catalog_drift_at_the_real_read_refuses_before_join() {
    let mut settings = config();
    let mut changed = local();
    changed["digest"] = json!("changed");
    settings["get"] = json!([{"stdout":changed},{"stdout":local()}]);
    let (fake, org, _) = fixture(settings, true);
    let id = open(&org);
    preview(&org, &id);
    join(&org, &id);
    wait_until("the local identity refusal", || {
        !org.lock().sync.busy() && view(&org, &id)["error"] == true
    });
    assert!(calls(&fake, "join").is_empty());
    assert_eq!(view(&org, &id)["status"], EXTERNAL_CHANGED);
    fake.finish(org);
}

#[test]
fn lost_join_reply_is_not_replayed_when_the_source_shows_enrollment() {
    let mut settings = config();
    settings["join"] = json!([{"exit":1,"stderr":"fixture reply lost"}]);
    let (fake, org, cache) = fixture(settings, true);
    let id = open(&org);
    preview(&org, &id);
    join(&org, &id);
    wait_until("the unknown source barrier", || {
        calls(&fake, "get").len() == 2 && view(&org, &id)["busy"] == false
    });
    advance(&org, &cache, joined());
    wait_until("the settled current state", || org.lock().sync.ready());
    assert_eq!(view(&org, &id)["error"], true);
    assert!(view(&org, &id)["status"]
        .as_str()
        .unwrap()
        .contains("fixture reply lost"));
    assert!(view(&org, &id)["preview"].is_null());
    org.catalog_request(Some(&id), None, true).unwrap();
    let _ = open(&org);
    assert_eq!(calls(&fake, "join").len(), 1);
    fake.finish(org);
}

#[test]
fn inherited_output_keeps_the_reservation_charged_after_the_runner_returns() {
    let mut settings = config();
    settings["get"] = json!([{"stdout":local()}]);
    settings["sync"] = json!([{"stdout":{"schema":1},"hold_output":"pipe-release"}]);
    let (fake, org, _) = fixture(settings, true);
    let id = open(&org);
    org.catalog_request(Some(&id), Some(Action::Sync), false)
        .unwrap();
    wait_until("the fake sync", || calls(&fake, "sync").len() == 1);
    std::thread::sleep(Duration::from_millis(2300));
    assert!(org.lock().sync.busy());
    assert_eq!(
        calls(&fake, "get").len(),
        1,
        "no post-call read until actual pipe completion"
    );
    assert!(org
        .catalog_request(Some(&id), Some(Action::Sync), false)
        .is_err());
    std::fs::write(fake.dir.join("pipe-release"), b"release").unwrap();
    wait_until("actual inherited-pipe completion", || {
        org.lock().sync.ready()
    });
    assert_eq!(calls(&fake, "sync").len(), 1);
    assert_eq!(calls(&fake, "get").len(), 2);
    fake.finish(org);
}

#[test]
fn closing_a_read_cannot_admit_late_actions_and_replay_without_fake_cli_dispatches_nothing() {
    let mut settings = config();
    settings["machines"] = json!([{"stdout":machines('a'),"gate":"config-release"}]);
    let (fake, org, _) = fixture(settings, true);
    let first = json(org.catalog_request(None, None, false).unwrap());
    let id = first["id"].as_str().unwrap().to_owned();
    wait_until("the held config read", || calls(&fake, "ls").len() == 1);
    org.catalog_request(Some(&id), None, true).unwrap();
    assert!(org
        .catalog_request(
            Some(&id),
            Some(Action::Preview("fixture-peer".into())),
            false
        )
        .is_err());
    std::fs::write(fake.dir.join("config-release"), b"release").unwrap();
    idle(&org, &id);
    assert!(calls(&fake, "preview").is_empty());
    assert!(calls(&fake, "join").is_empty());
    fake.finish(org);
    let (fake, org, _) = fixture(config(), false);
    let id = open(&org);
    let v = view(&org, &id);
    assert_eq!(v["can_sync"], false);
    assert_eq!(v["can_join"], false);
    assert!(org
        .catalog_request(Some(&id), Some(Action::Sync), false)
        .is_err());
    assert!(calls(&fake, "ls").is_empty());
    fake.finish(org);
}

#[test]
fn a_held_source_cannot_schedule_qt_recovery_during_a_join() {
    let mut settings = config();
    settings["join"] =
        json!([{"gate":"join-release","stdout":{"joined":true,"snapshot":joined()}}]);
    let (fake, org, cache) = fixture(settings, true);
    let id = open(&org);
    preview(&org, &id);
    join(&org, &id);
    wait_until("the held Join", || calls(&fake, "join").len() == 1);
    {
        let mut hub = org.lock();
        hub.qt_recovery = Some(QtRecovery {
            source: zerus_client_core::settings::QtStore::Ini(fake.dir.join("unread-qt.conf")),
            settings: zerus_client_core::settings::Settings::open(
                fake.dir.join("settings.json"),
                None,
            ),
        });
        hub.recovery_seen = Some(("node-1".into(), true));
    }
    let mut stale = local();
    stale["initialized"] = json!(false);
    advance(&org, &cache, stale);
    {
        let mut hub = org.lock();
        assert!(hub.sync.busy());
        assert!(
            hub.recovery_due.is_none(),
            "held data must not authorize recovery"
        );
        hub.qt_recovery = None;
    }
    assert!(calls(&fake, "initialize").is_empty());
    std::fs::write(fake.dir.join("join-release"), b"release").unwrap();
    wait_until("post-Join source wait", || {
        calls(&fake, "get").len() == 2 && !view(&org, &id)["busy"].as_bool().unwrap()
    });
    advance(&org, &cache, joined());
    wait_until("Join settlement", || org.lock().sync.ready());
    fake.finish(org);
}

#[test]
fn repeated_views_are_read_only_and_do_not_announce_themselves() {
    let (fake, org, _) = fixture(config(), true);
    org.set_presented(Arc::new(|_, _, _| {}));
    let id = open(&org);
    let before = org.lock().announcements;
    let reads = calls(&fake, "ls").len();
    for _ in 0..100 {
        assert_eq!(view(&org, &id)["id"], id);
    }
    assert_eq!(org.lock().announcements, before);
    assert_eq!(calls(&fake, "ls").len(), reads);
    fake.finish(org);
}
