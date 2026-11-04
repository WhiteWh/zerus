use super::*;
use crate::desktop::{
    attention,
    notifications::{activation, attachment, preparation, service},
    ownership,
};

struct Root {
    h: Harness,
    location: Location,
    runtime: service::Runtime,
    attention: attention::Runtime,
    router: activation::Router,
    seq: u64,
}
impl Root {
    fn new(address: &str) -> Self {
        let mut h = Harness::new();
        let location = Location {
            root: h._root.path().canonicalize().unwrap().join("state/desktop"),
            configuration: h.outbox.session.configuration.clone(),
        };
        let config = crate::config::Config {
            home: h._root.path().to_str().unwrap().into(),
            dir: h._root.path().join("config"),
            state: h._root.path().join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        let image = h._root.path().join("hgs-tray");
        std::fs::write(&image, b"native grant fixture").unwrap();
        let origin = ownership::Origin::capture(
            &image,
            zerus_client_core::desktop::Build {
                version: "1.2.3".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
        )
        .unwrap();
        h.store
            .db
            .execute(
                "UPDATE desktop_ownership SET state='service',owner_epoch=?1,origin=?2 WHERE id=1",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    serde_json::to_string(&origin).unwrap()
                ],
            )
            .unwrap();
        let epoch = h.outbox.session.service_epoch.clone();
        let mut runtime = service::Runtime::new(
            &config,
            &location,
            &epoch,
            Arc::new(location.lock().unwrap()),
        );
        runtime.begin_on_test_bus(&mut h.store, address.to_owned());
        Self {
            h,
            location: Location {
                root: location.root.clone(),
                configuration: location.configuration.clone(),
            },
            runtime,
            attention: attention::Runtime::new(&epoch),
            router: activation::Router::new(&location.configuration, &epoch),
            seq: 0,
        }
    }
    fn poll(&mut self) {
        self.runtime.poll(
            &mut self.h.store,
            &mut self.attention,
            &mut self.router,
            &mut self.seq,
        );
    }
    fn ready(&mut self) {
        until(|| {
            self.poll();
            self.runtime.ready()
        });
    }
    fn waiting(&mut self) -> Value {
        until(|| {
            self.poll();
            !self.runtime.status()["retry_in_ms"].is_null()
        });
        self.runtime.status()
    }
    fn stop(&mut self) {
        self.runtime.request_stop();
        until(|| {
            self.poll();
            self.runtime.drained()
        });
    }
    fn post(&mut self) -> String {
        let intent = ledger::put_intent(
            &mut self.h.store.db,
            &Identity {
                kind: Kind::Post,
                target: Target::Session {
                    computer: "peer.example".into(),
                    run: "run-1".into(),
                    conversation: "conversation-1".into(),
                },
                edge: uuid::Uuid::new_v4().to_string(),
            },
            1,
            &serde_json::to_value(command(session()).action).unwrap(),
        )
        .unwrap();
        self.h.store.db.execute("INSERT INTO desktop_attention_edges(logical,session,intent,identity,payload,blocked,bytes)
            SELECT id,'fixture',id,identity,payload,0,0 FROM desktop_intents WHERE id=?1",[&intent]).unwrap();
        self.runtime.offer_for_test(&mut self.h.store, &intent);
        intent
    }
}

#[test]
fn missing_owner_activates_only_the_fixed_dbus_service_without_posting() {
    let bus = bus::Bus::new();
    let address = zbus::Address::try_from(bus.address.as_str()).unwrap();
    let zbus::address::Transport::Unix(unix) = address.transport() else {
        panic!("fixture Unix bus")
    };
    let zbus::address::transport::UnixSocket::File(path) = unix.path() else {
        panic!("fixture file socket")
    };
    let directory = path.parent().unwrap().to_owned();
    let services = directory.join("data/dbus-1/services");
    std::fs::create_dir_all(&services).unwrap();
    let executable = std::env::current_exe().unwrap();
    assert!(!executable
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(u8::is_ascii_whitespace));
    let script = directory.join("activate.sh");
    std::fs::write(&script, format!("#!/bin/sh\nexec '{}' --ignored --exact desktop::notifications::linux::tests::recovery::activation_daemon_process --nocapture >'{}/activation-child.log' 2>&1\n",executable.display(),directory.display())).unwrap();
    std::fs::write(
        services.join("org.freedesktop.Notifications.service"),
        format!(
            "[D-BUS Service]\nName=org.freedesktop.Notifications\nExec=/bin/sh {}\n",
            script.display()
        ),
    )
    .unwrap();
    bus.connect()
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ReloadConfig",
            &(),
        )
        .unwrap();
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    let ack = runtime.finish_restoration().unwrap();
    until(|| ack.poll().is_some());
    assert_eq!(runtime.status()["ready"], true, "{}", runtime.status());
    assert!(
        directory.join("activation-started").exists(),
        "the bus activated its maintained service definition"
    );
    assert!(
        !directory.join("activation-called").exists(),
        "activation/restoration do not post"
    );
    let request = h.post();
    h.settle(&request, "delivered");
    until(|| directory.join("activation-called").exists());
    h.stop(&mut runtime);
    std::fs::write(directory.join("activation-stop"), b"stop").unwrap();
    until(|| directory.join("activation-ended").exists());
    for file in [
        "activation-started",
        "activation-called",
        "activation-stop",
        "activation-ended",
    ] {
        std::fs::remove_file(directory.join(file)).unwrap();
    }
    std::fs::remove_file(services.join("org.freedesktop.Notifications.service")).unwrap();
    std::fs::remove_dir(&services).unwrap();
    std::fs::remove_dir(services.parent().unwrap()).unwrap();
    std::fs::remove_file(script).unwrap();
    std::fs::remove_file(directory.join("activation-child.log")).unwrap();
}

#[test]
#[ignore = "private D-Bus activation subprocess fixture, launched by the real daemon"]
fn activation_daemon_process() {
    let address = std::env::var("DBUS_STARTER_ADDRESS").unwrap();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(home
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("g4-bus-"));
    let server = Server::at(&address);
    std::fs::write(home.join("activation-started"), b"ready").unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    while !home.join("activation-stop").exists()
        && !server.connection.inner().is_closed()
        && Instant::now() < until
    {
        if server.called.try_recv().is_ok() {
            std::fs::write(home.join("activation-called"), b"called").unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    server.connection.close().unwrap();
    std::fs::write(home.join("activation-ended"), b"closed").unwrap();
}

#[test]
fn failed_setup_retries_with_capped_backoff_and_revocation_cancels_the_timer() {
    let _prepare = preparation::TEST_SERIAL.lock().unwrap();
    let _attach = attachment::TEST_SERIAL.lock().unwrap();
    let bus = bus::Bus::new();
    let mut root = Root::new(&bus.address); // No daemon and no activatable service.
    for seconds in [1, 2, 5, 10, 30, 30] {
        let status = root.waiting();
        let remaining = status["retry_in_ms"].as_u64().unwrap();
        assert!(
            remaining <= seconds * 1000 && remaining > seconds * 1000 - 500,
            "{status}"
        );
        assert!(
            status["error"].as_str().is_some(),
            "attachment fault must reach service status"
        );
        assert!(!root.runtime.ready());
        let attempts = status["failed_attempts"].as_u64().unwrap();
        for _ in 0..100 {
            root.poll();
        }
        assert_eq!(
            root.runtime.status()["failed_attempts"],
            attempts,
            "actor turns do not retry before the deadline"
        );
        root.runtime.retry_now_for_test();
        root.poll();
    }
    root.waiting();
    let server = Server::new(&bus);
    root.runtime.retry_now_for_test();
    root.ready();
    assert!(root.runtime.status()["error"].is_null());
    assert_eq!(root.runtime.status()["failed_attempts"], 0);
    assert!(server.called.try_recv().is_err());
    server
        .connection
        .release_name(zerus_freedesktop_notify::SERVICE)
        .unwrap();
    root.waiting();
    ownership::invalidate(&root.h.store.db).unwrap();
    ledger::revoke(&root.h.store.db, "manual_revoke").unwrap();
    root.poll();
    assert!(root.runtime.status()["retry_in_ms"].is_null());
    assert_eq!(root.runtime.status()["recovering"], false);
    assert!(root.runtime.status()["error"].is_null());
    root.stop();
    assert!(root.location.handoff_lock().is_ok());
}

#[test]
fn current_grant_recovers_after_owner_loss_only_when_original_owner_resources_drain() {
    let _prepare = preparation::TEST_SERIAL.lock().unwrap();
    let _attach = attachment::TEST_SERIAL.lock().unwrap();
    let bus = bus::Bus::new();
    let first = Server::new(&bus);
    let mut root = Root::new(&bus.address);
    root.ready();
    let intent = root.post();
    first.called.recv_timeout(WAIT).unwrap();
    until(|| {
        root.poll();
        root.h
            .store
            .db
            .query_row(
                "SELECT state FROM desktop_attempts WHERE intent_id=?1",
                [&intent],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            == "delivered"
    });
    let held = root.runtime.hold_owner_for_test();
    first
        .connection
        .release_name(zerus_freedesktop_notify::SERVICE)
        .unwrap();
    let second = Server::new(&bus);
    until(|| {
        root.poll();
        root.runtime.status()["recovering"] == true
    });
    for _ in 0..20 {
        root.poll();
    }
    assert!(
        root.runtime.status()["retry_in_ms"].is_null(),
        "copied original lifetime still owns the exclusion"
    );
    assert!(root.location.handoff_lock().is_err());
    assert!(
        root.attention.enabled(),
        "host loss is not a policy-off retirement"
    );
    assert!(second.called.try_recv().is_err());
    assert!(second.closed.try_recv().is_err());
    drop(held);
    root.waiting();
    root.runtime.retry_now_for_test();
    root.ready();
    assert!(root.runtime.status()["error"].is_null());
    assert!(
        second.called.try_recv().is_err(),
        "restoration never reposts a delivered notification"
    );
    assert!(
        second.closed.try_recv().is_err(),
        "foreign numeric IDs never redirect"
    );
    assert_eq!(
        root.h
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM desktop_attempts WHERE intent_id=?1",
                [intent],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    root.post();
    second.called.recv_timeout(WAIT).unwrap();
    root.stop();
    assert!(root.location.handoff_lock().is_ok());
}
