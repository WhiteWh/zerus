use super::*;
use zerus_client_core::{ComputerStatus, MachineMeta};
const NODE: &str = "10000000-0000-4000-8000-000000000001";
const OTHER: &str = "20000000-0000-4000-8000-000000000002";

fn catalog() -> Value {
    let mut value = full("windows-source");
    value["node_id"] = json!(NODE);
    value["swarm_id"] = json!("catalog-one");
    value["organization"]["projects"][0]["folders"] = json!([
        {"id":"peer","machine":"peer","machine_id":OTHER,"path":"//server/share","name":"Peer POSIX"}
    ]);
    value
}
fn layout() -> Value {
    json!({"version":1,"local_windows_folders":{"version":1,"node_id":NODE,"projects":{
        "alpha":[{"id":"drive","machine":"old-local-name","machine_id":NODE,"path":"C:\\Source","name":"Drive"},
            {"id":"unc","machine":"old-local-name","machine_id":NODE,"path":"\\\\server\\share\\Source","name":"UNC"}],
        "temporarily-missing":[{"id":"kept","machine":"old-local-name","machine_id":NODE,"path":"D:\\Other"}]
    }}})
}
fn hello(pump: &Pump, node: &str, platform: Option<&str>, status: ComputerStatus) {
    pump.push(Change::Machine {
        computer: ComputerId::local(),
        machine: MachineMeta {
            machine_id: Some(node.into()),
            host: Some("workstation".into()),
            platform: platform.map(str::to_owned),
            ok: Some(true),
            ..Default::default()
        },
    });
    pump.push(Change::Computer {
        computer: ComputerId::local(),
        status,
    });
}
fn shown(org: &Organization) -> Value {
    let latest = Arc::new(Mutex::new(None));
    let copy = latest.clone();
    org.set_presented(Arc::new(move |value, _, _| {
        *copy.lock().unwrap() = value;
    }));
    let value = latest.lock().unwrap().take().unwrap();
    (*value).clone()
}
fn folder_count(org: &Organization) -> usize {
    shown(org)["organization"]["projects"][0]["folders"]
        .as_array()
        .unwrap()
        .len()
}
fn setup() -> (Fake, Pump, Organization, Value) {
    let fake = Fake::landing();
    std::fs::write(fake.layout(), serde_json::to_vec(&layout()).unwrap()).unwrap();
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let org = fake.organization(&pump, true);
    let value = catalog();
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    (fake, pump, org, value)
}

#[test]
fn projects_overlay_never_reaches_shared_source_launch_or_layout_edits() {
    let (fake, pump, org, value) = setup();
    // The ordinary grouping pass may seed its order; it must retain the
    // whole metadata family, including the unlisted project.
    if let Some(bytes) = pump.take_layout() {
        let saved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            saved["local_windows_folders"],
            layout()["local_windows_folders"]
        );
    }
    assert_eq!(folder_count(&org), 3);
    assert_eq!(
        org.swarm_snapshot().unwrap()["organization"],
        value["organization"]
    );
    assert_eq!(
        org.project_folders("alpha").len(),
        1,
        "launch reads only shared folders"
    );
    assert_eq!(org.creation_projects("workstation")[0].folders.len(), 1);
    assert_eq!(
        shown(&org)["organization"]["projects"][0]["folders"][0]["path"],
        "//server/share"
    );
    assert_eq!(
        pump.take_layout(),
        None,
        "a read never dirties or prunes Layout"
    );
    assert!(
        folders::machine(
            &pump,
            &folders::Place {
                computer: String::new(),
                host: "workstation".into(),
                machine_id: Some(NODE.into())
            }
        )
        .is_err(),
        "the Windows native adapter stays fenced"
    );
    assert!(fake.applies().is_empty());
    fake.finish(org);
}

#[test]
fn repeated_source_start_keeps_the_existing_live_observation() {
    let (fake, _pump, org, value) = setup();
    let generation = org.lock().source_generation;
    org.set_initial_source(snapshot_of(&value));
    assert_eq!(org.lock().source_generation, generation);
    assert_eq!(folder_count(&org), 3);
    fake.finish(org);
}

#[test]
fn only_current_live_windows_hello_at_the_exact_node_qualifies() {
    let (fake, pump, org, value) = setup();
    let mut metrics = pump.machine(&ComputerId::local()).unwrap();
    metrics.metrics.cpu_percent = Some(15.0);
    let update = Change::Machine {
        computer: ComputerId::local(),
        machine: metrics,
    };
    org.observe(&update);
    pump.push(update);
    assert_eq!(
        folder_count(&org),
        3,
        "ordinary metrics do not replace the owner proof"
    );
    for (node, platform, status) in [
        (OTHER, Some("windows"), ComputerStatus::Live),
        (NODE, None, ComputerStatus::Live),
        (NODE, Some("linux"), ComputerStatus::Live),
        (NODE, Some("windows"), ComputerStatus::Polled),
        (
            NODE,
            Some("windows"),
            ComputerStatus::Reconnecting { attempt: 1 },
        ),
        (NODE, Some("windows"), ComputerStatus::Ended),
    ] {
        hello(&pump, node, platform, status);
        assert_eq!(folder_count(&org), 1);
    }
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let disconnected = Change::Computer {
        computer: ComputerId::local(),
        status: ComputerStatus::Reconnecting { attempt: 1 },
    };
    org.observe(&disconnected);
    pump.push(disconnected);
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    assert_eq!(
        folder_count(&org),
        1,
        "a cached state does not requalify on reconnect"
    );
    org.observe(&state_of(&value));
    assert_eq!(folder_count(&org), 3);
    fake.finish(org);
}

#[test]
fn the_existing_worker_rechecks_a_live_status_arriving_after_the_source() {
    let fake = Fake::landing();
    std::fs::write(fake.layout(), serde_json::to_vec(&layout()).unwrap()).unwrap();
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Probing);
    let org = fake.organization(&pump, true);
    let value = catalog();
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    let latest = Arc::new(Mutex::new(None));
    let copy = latest.clone();
    org.set_presented(Arc::new(move |value, _, _| *copy.lock().unwrap() = value));
    let live = Change::Computer {
        computer: ComputerId::local(),
        status: ComputerStatus::Live,
    };
    org.observe(&live); // observer runs before the mirror update
    pump.push(live);
    org.0.wake.notify_all();
    wait_until("the existing idle pass to project after Live", || {
        latest.lock().unwrap().as_deref().is_some_and(|v| {
            v["organization"]["projects"][0]["folders"]
                .as_array()
                .unwrap()
                .len()
                == 3
        })
    });
    assert!(fake.applies().is_empty());
    fake.finish(org);
}

#[test]
fn failed_ended_held_or_mismatched_sources_require_a_new_accepted_observation() {
    let (fake, _pump, org, value) = setup();
    for update in [
        OrganizationUpdate::Failed {
            code: "read".into(),
            message: "unavailable".into(),
        },
        OrganizationUpdate::Ended,
    ] {
        org.observe(&Change::Organization {
            computer: ComputerId::local(),
            update,
        });
        assert_eq!(folder_count(&org), 1);
        org.observe(&state_of(&value));
        assert_eq!(folder_count(&org), 3);
    }
    let mut wrong = value.clone();
    wrong["organization"]["projects"][0]["name"] = json!("different source");
    org.set_source(snapshot_of(&wrong));
    org.observe(&state_of(&value));
    assert_eq!(folder_count(&org), 1);
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    assert_eq!(folder_count(&org), 3);
    {
        let _writer = org.writer();
        let mut hub = org.lock();
        let revision = hub.sync.revision();
        hub.sync.begin_external(revision).unwrap();
        org.publish(&mut hub);
        org.release(hub);
        assert_eq!(folder_count(&org), 1);
        org.observe(&state_of(&value)); // held by the external reservation
        assert!(org.lock().windows_read.is_none());
        org.lock().sync = Controller::default();
    }
    assert!(fake.applies().is_empty());
    fake.finish(org);
}

#[test]
fn pending_layout_recovery_and_unreadable_originals_do_not_qualify() {
    let (fake, _pump, org, _value) = setup();
    {
        let _writer = org.writer();
        let mut hub = org.lock();
        hub.recovery_due = Some(NODE.into());
        assert!(org.windows_read_node(&hub).is_none());
        hub.recovery_due = None;
        hub.sync
            .edit(
                &Intent::Rename {
                    project: "alpha".into(),
                    name: "Renamed".into(),
                },
                org.now(),
            )
            .unwrap();
        assert!(org.windows_read_node(&hub).is_none());
        hub.sync = Controller::default();
    }
    fake.finish(org);
    let fake = Fake::landing();
    let bytes = br#"{"version":1,"local_windows_folders":null}"#;
    std::fs::write(fake.layout(), bytes).unwrap();
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let org = fake.organization(&pump, true);
    let value = catalog();
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    assert_eq!(folder_count(&org), 1);
    org.flush();
    assert_eq!(std::fs::read(fake.layout()).unwrap(), bytes);
    fake.finish(org);
}

fn qt_recovery(fake: &Fake, org: &Organization, text: &str) -> PathBuf {
    let qt = fake.dir.join("qt.ini");
    std::fs::write(&qt, text).unwrap();
    let settings =
        zerus_client_core::settings::Settings::open(fake.dir.join("settings.json"), None);
    // The already-completed shared/presentation families are independent of
    // this local Windows family. Exercise the ordinary worker pass below.
    let mut hub = org.lock();
    hub.qt_recovery = Some(QtRecovery {
        source: zerus_client_core::settings::QtStore::Ini(qt.clone()),
        settings,
    });
    hub.recovery_seen = Some((NODE.into(), true));
    qt
}
fn qt_folders() -> String {
    let org = json!({"version":2,"projects":[{"id":"alpha","name":"Alpha","sessions":[],"folders":[
        {"id":"local","machine":"old-local-alias","machine_id":NODE,"path":"C:\\Source","name":"Local"},
        {"id":"foreign","machine":"workstation","machine_id":OTHER,"path":"D:\\Peer","name":"Foreign"}
    ]}]});
    format!(
        "[workspace]\norganization={}\n",
        serde_json::to_string(&org.to_string()).unwrap()
    )
}
fn saved_windows(fake: &Fake) -> Option<Value> {
    std::fs::read(fake.layout())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|value| value.get("g3_local_windows_folders").is_some())
}
#[test]
fn worker_imports_local_windows_folders_once_through_existing_layout_writer() {
    let fake = Fake::landing();
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let org = fake.organization(&pump, true);
    let original = qt_folders();
    let qt = qt_recovery(&fake, &org, &original);
    let value = catalog();
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    wait_until("the Windows family checkpoint", || {
        saved_windows(&fake).is_some()
    });
    let saved = saved_windows(&fake).unwrap();
    assert_eq!(
        saved["local_windows_folders"]["projects"]["alpha"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        saved["local_windows_folders"]["projects"]["alpha"][0]["id"],
        "local"
    );
    assert_eq!(
        saved["g3_local_windows_folders"]["keys"],
        json!(["workspace/organization"])
    );
    assert_eq!(std::fs::read_to_string(&qt).unwrap(), original);
    assert_eq!(
        org.swarm_snapshot().unwrap()["organization"],
        value["organization"]
    );
    assert!(
        !fake.dir.join("log.ndjson").exists(),
        "the local import runs no hgs command"
    );
    let runs = org.runs_writer();
    let shared = Arc::downgrade(&org.0);
    drop(org);
    wait_until("the first worker to end", || shared.strong_count() == 0);
    runs.stop();
    // Rollback can change Qt; a handled family is never re-read/reseeded.
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let org = fake.organization(&pump, true);
    qt_recovery(&fake, &org, "[workspace]\norganization=invalid\n");
    org.set_source(snapshot_of(&value));
    org.observe(&state_of(&value));
    wait_until("the already-handled startup", || {
        org.lock().windows_import_attempted
    });
    assert_eq!(
        saved_windows(&fake).unwrap()["local_windows_folders"],
        saved["local_windows_folders"]
    );
    fake.finish(org);
}
#[test]
fn missing_qt_organization_and_offline_or_mismatched_local_watch_do_not_mark_handled() {
    for (text, node, status) in [
        (
            "[workspace]\ntheme=dark\n".to_owned(),
            NODE,
            ComputerStatus::Live,
        ),
        (qt_folders(), OTHER, ComputerStatus::Live),
        (
            qt_folders(),
            NODE,
            ComputerStatus::Reconnecting { attempt: 1 },
        ),
    ] {
        let fake = Fake::landing();
        let pump = Pump::default();
        hello(&pump, node, Some("windows"), status);
        let org = fake.organization(&pump, true);
        qt_recovery(&fake, &org, &text);
        let value = catalog();
        org.set_source(snapshot_of(&value));
        org.observe(&state_of(&value));
        org.recover_windows_folders().unwrap();
        assert!(saved_windows(&fake).is_none());
        assert!(pump.windows_folders_import_layout().unwrap().is_some());
        fake.finish(org);
    }
}
#[test]
fn existing_empty_or_unsupported_family_wins_over_a_broken_qt_source() {
    for family in [
        json!({"version":1,"node_id":NODE,"projects":{}}),
        Value::Null,
    ] {
        let fake = Fake::landing();
        let original = json!({"version":1,"local_windows_folders":family}).to_string();
        std::fs::write(fake.layout(), &original).unwrap();
        let pump = Pump::default();
        hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
        let org = fake.organization(&pump, true);
        qt_recovery(&fake, &org, "[workspace]\norganization=broken\n");
        let value = catalog();
        org.set_source(snapshot_of(&value));
        org.observe(&state_of(&value));
        org.recover_windows_folders().unwrap();
        if family.is_null() {
            assert!(
                !org.0.layout_writable,
                "unsupported original disables all layout saves"
            );
            assert_eq!(std::fs::read_to_string(fake.layout()).unwrap(), original);
        } else {
            assert!(pump.windows_folders_import_layout().unwrap().is_none());
        }
        assert!(saved_windows(&fake).is_none());
        fake.finish(org);
    }
}

#[test]
fn catalog_source_lost_during_qt_preparation_leaves_the_family_unhandled() {
    let fake = Fake::landing();
    let pump = Pump::default();
    hello(&pump, NODE, Some("windows"), ComputerStatus::Live);
    let org = fake.organization(&pump, true);
    qt_recovery(&fake, &org, &qt_folders());
    let value = catalog();
    let stable = snapshot_of(&value);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    org.set_source(Arc::new(move || {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            stable()
        } else {
            None
        }
    }));
    org.observe(&state_of(&value));
    wait_until("the refused Windows source recheck", || {
        calls.load(Ordering::SeqCst) >= 2
    });
    assert!(saved_windows(&fake).is_none());
    assert!(pump.windows_folders_import_layout().unwrap().is_some());
    fake.finish(org);
}
