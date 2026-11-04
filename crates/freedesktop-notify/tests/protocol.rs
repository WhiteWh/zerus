#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::names::UniqueName;
use zbus::zvariant::OwnedValue;
use zerus_freedesktop_notify::{close_notification, decode_signal, notify, Signal, PATH, SERVICE};

const WAIT: Duration = Duration::from_secs(5);

#[path = "../src/ingress/test_bus.rs"]
mod bus;
use bus::Bus;

#[derive(Debug)]
struct Call {
    app: String,
    replaces: u32,
    icon: String,
    title: String,
    body: String,
    actions: Vec<String>,
    hints: BTreeMap<String, OwnedValue>,
    timeout: i32,
}
struct Fake {
    calls: mpsc::SyncSender<Call>,
    closes: mpsc::SyncSender<u32>,
    outcome: Arc<AtomicU8>,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Fake {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app: String,
        replaces: u32,
        icon: String,
        title: String,
        body: String,
        actions: Vec<String>,
        hints: BTreeMap<String, OwnedValue>,
        timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        self.calls
            .try_send(Call {
                app,
                replaces,
                icon,
                title,
                body,
                actions,
                hints,
                timeout,
            })
            .map_err(|_| zbus::fdo::Error::Failed("fixture queue full".into()))?;
        match self.outcome.load(Ordering::Relaxed) {
            1 => Err(zbus::fdo::Error::Failed("fixture refusal".into())),
            2 => Err(zbus::fdo::Error::NoReply("fixture unknown outcome".into())),
            3 => Ok(0),
            _ => Ok(42),
        }
    }
    fn close_notification(&self, id: u32) {
        self.closes.try_send(id).unwrap();
    }
}
struct Server {
    connection: Connection,
    calls: mpsc::Receiver<Call>,
    closes: mpsc::Receiver<u32>,
    outcome: Arc<AtomicU8>,
}
impl Server {
    fn new(bus: &Bus) -> Self {
        let (calls, called) = mpsc::sync_channel(8);
        let (closes, closed) = mpsc::sync_channel(8);
        let outcome = Arc::new(AtomicU8::new(0));
        let connection = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(
                PATH,
                Fake {
                    calls,
                    closes,
                    outcome: outcome.clone(),
                },
            )
            .unwrap()
            .build()
            .unwrap();
        Self {
            connection,
            calls: called,
            closes: closed,
            outcome,
        }
    }
    fn owner(&self) -> UniqueName<'_> {
        self.connection
            .unique_name()
            .unwrap()
            .as_str()
            .try_into()
            .unwrap()
    }
}

#[test]
fn notify_keeps_the_existing_app_identity_actions_and_wire_values() {
    let bus = Bus::new();
    let server = Server::new(&bus);
    let client = bus.connect();
    assert_eq!(
        notify(
            &client,
            server.owner(),
            "Session",
            "&lt;b&gt;literal &amp; body&lt;/b&gt;"
        )
        .unwrap(),
        42
    );
    let call = server.calls.recv_timeout(WAIT).unwrap();
    assert_eq!(call.app, "Zerus");
    assert_eq!(call.replaces, 0);
    assert_eq!(call.icon, "hgs-zerus-swarm-symbolic");
    assert_eq!(call.title, "Session");
    assert_eq!(call.body, "&lt;b&gt;literal &amp; body&lt;/b&gt;");
    assert_eq!(
        call.actions,
        ["default", "Open session", "open-session", "Open session"]
    );
    assert_eq!(
        <&str>::try_from(&call.hints["desktop-entry"]).unwrap(),
        "hgs-tray"
    );
    assert_eq!(u8::try_from(&call.hints["urgency"]).unwrap(), 1);
    assert_eq!(
        <&str>::try_from(&call.hints["category"]).unwrap(),
        "im.received"
    );
    assert_eq!(call.timeout, -1);
    assert!(server.calls.try_recv().is_err());
}

#[test]
fn async_methods_keep_the_same_packet_and_never_retry_zero_or_errors() {
    use zerus_freedesktop_notify::{close_notification_async, notify_async};
    let bus = Bus::new();
    let server = Server::new(&bus);
    let client = bus.connect();
    assert_eq!(
        futures_lite::future::block_on(notify_async(
            client.inner(),
            server.owner(),
            "Session",
            "&lt;body&gt;"
        ))
        .unwrap()
        .body()
        .deserialize::<u32>()
        .unwrap(),
        42
    );
    let call = server.calls.recv_timeout(WAIT).unwrap();
    assert_eq!(call.app, "Zerus");
    assert_eq!(call.replaces, 0);
    assert_eq!(call.body, "&lt;body&gt;");
    assert_eq!(
        call.actions,
        ["default", "Open session", "open-session", "Open session"]
    );
    assert_eq!(
        <&str>::try_from(&call.hints["desktop-entry"]).unwrap(),
        "hgs-tray"
    );
    assert_eq!(u8::try_from(&call.hints["urgency"]).unwrap(), 1);
    assert_eq!(
        <&str>::try_from(&call.hints["category"]).unwrap(),
        "im.received"
    );
    assert_eq!(call.timeout, -1);
    futures_lite::future::block_on(close_notification_async(client.inner(), server.owner(), 42))
        .unwrap()
        .body()
        .deserialize::<()>()
        .unwrap();
    assert_eq!(server.closes.recv_timeout(WAIT).unwrap(), 42);
    for outcome in [1, 2, 3] {
        server.outcome.store(outcome, Ordering::Relaxed);
        let result = futures_lite::future::block_on(notify_async(
            client.inner(),
            server.owner(),
            "Session",
            "body",
        ));
        if outcome == 3 {
            assert_eq!(result.unwrap().body().deserialize::<u32>().unwrap(), 0);
        } else {
            assert!(result.is_err());
        }
        server.calls.recv_timeout(WAIT).unwrap();
        assert!(server.calls.try_recv().is_err());
    }
}

#[test]
fn refusal_unknown_and_zero_identity_are_returned_without_retry() {
    let bus = Bus::new();
    let server = Server::new(&bus);
    let client = bus.connect();
    for (outcome, error) in [
        (1, "org.freedesktop.DBus.Error.Failed"),
        (2, "org.freedesktop.DBus.Error.NoReply"),
    ] {
        server.outcome.store(outcome, Ordering::Relaxed);
        match notify(&client, server.owner(), "Session", "body") {
            Err(zbus::Error::MethodError(name, _, _)) => assert_eq!(name.as_str(), error),
            result => panic!("expected original method error: {result:?}"),
        }
        server.calls.recv_timeout(WAIT).unwrap();
        assert!(server.calls.try_recv().is_err());
    }
    server.outcome.store(3, Ordering::Relaxed);
    assert_eq!(
        notify(&client, server.owner(), "Session", "body").unwrap(),
        0
    );
    server.calls.recv_timeout(WAIT).unwrap();
    assert!(server.calls.try_recv().is_err());
}

#[test]
fn a_daemon_replacement_cannot_receive_the_old_daemons_close() {
    let bus = Bus::new();
    let original = Server::new(&bus);
    let client = bus.connect();
    original.connection.release_name(SERVICE).unwrap();
    let replacement = Server::new(&bus);
    close_notification(&client, original.owner(), 42).unwrap();
    assert_eq!(original.closes.recv_timeout(WAIT).unwrap(), 42);
    assert!(replacement.closes.try_recv().is_err());
    // Once the old connection is gone, its unique name fails; it never falls
    // back to the well-known service or closes a reused ID on its replacement.
    let old_name = original.owner().to_string();
    original.connection.close().unwrap();
    assert!(close_notification(&client, old_name.as_str().try_into().unwrap(), 42).is_err());
    assert!(replacement.closes.try_recv().is_err());
}

fn message(
    member: &str,
    sender: &str,
    path: &str,
    interface: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Message {
    zbus::Message::signal(path, interface, member)
        .unwrap()
        .sender(sender)
        .unwrap()
        .build(body)
        .unwrap()
}

#[test]
fn signal_payloads_are_decoded_without_interpreting_ids_as_permission() {
    let owner: UniqueName<'_> = ":1.42".try_into().unwrap();
    let activation = message(
        "ActivationToken",
        owner.as_str(),
        PATH,
        SERVICE,
        &(42_u32, "wayland-token"),
    );
    assert_eq!(
        decode_signal(&activation, &owner),
        Some(Signal::ActivationToken {
            id: 42,
            token: "wayland-token".into()
        })
    );
    let action = message(
        "ActionInvoked",
        owner.as_str(),
        PATH,
        SERVICE,
        &(42_u32, "open-session"),
    );
    assert_eq!(
        decode_signal(&action, &owner),
        Some(Signal::ActionInvoked {
            id: 42,
            action: "open-session".into()
        })
    );
    let expired = message(
        "NotificationClosed",
        owner.as_str(),
        PATH,
        SERVICE,
        &(42_u32, 1_u32),
    );
    assert_eq!(
        decode_signal(&expired, &owner),
        Some(Signal::NotificationClosed { id: 42, reason: 1 })
    );
}

#[test]
fn foreign_malformed_and_non_protocol_messages_are_ignored() {
    let owner: UniqueName<'_> = ":1.42".try_into().unwrap();
    for (sender, path, interface, member) in [
        (":1.43", PATH, SERVICE, "ActionInvoked"),
        (":1.42", "/other", SERVICE, "ActionInvoked"),
        (":1.42", PATH, "org.example.Other", "ActionInvoked"),
        (":1.42", PATH, SERVICE, "Other"),
    ] {
        assert_eq!(
            decode_signal(
                &message(member, sender, path, interface, &(42_u32, "default")),
                &owner
            ),
            None
        );
    }
    let malformed = message(
        "ActionInvoked",
        owner.as_str(),
        PATH,
        SERVICE,
        &("wrong", 42_u32),
    );
    assert_eq!(decode_signal(&malformed, &owner), None);
    let call = zbus::Message::method_call(PATH, "ActionInvoked")
        .unwrap()
        .sender(owner.as_str())
        .unwrap()
        .interface(SERVICE)
        .unwrap()
        .build(&(42_u32, "default"))
        .unwrap();
    assert_eq!(decode_signal(&call, &owner), None);
    assert!(UniqueName::try_from(SERVICE).is_err());
}
