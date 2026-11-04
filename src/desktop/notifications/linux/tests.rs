use super::super::{Offered, Outbox};
use super::ingress::test_bus as bus;
use super::*;
use crate::desktop::store::{Location, Store};
use std::sync::atomic::{AtomicU32, AtomicU8};
use zerus_desktop_ledger::{self as ledger, Identity, Kind, Reserve, Target};

mod recovery;

fn session() -> Session {
    Session {
        configuration: "a".repeat(64),
        service_epoch: uuid::Uuid::new_v4().to_string(),
        writer_epoch: uuid::Uuid::new_v4().to_string(),
        adapter: uuid::Uuid::new_v4().to_string(),
        host_epoch: uuid::Uuid::new_v4().to_string(),
        lease: uuid::Uuid::new_v4().to_string(),
        platform: Platform::Linux,
    }
}
fn command(session: Session) -> Command {
    let token = zerus_client_core::attention::encode_token(
        json!({
            "host":"peer.example","run":"run-1","conversation":"conversation-1","name":"fixture",
        "notice":uuid::Uuid::new_v4().to_string(),
        })
        .as_object()
        .unwrap(),
    );
    Command {
        binding: wire::Binding {
            session,
            request_id: uuid::Uuid::new_v4().to_string(),
            intent_id: "b".repeat(64),
            revision: 1,
            seq: 1,
        },
        action: Action::Post {
            token,
            title: "Needs attention".into(),
            body: "<b>literal & text</b>".into(),
        },
    }
}
fn namespace() -> Namespace {
    Namespace {
        bus_id: "a".repeat(32),
        owner: ":1.42".into(),
    }
}

#[test]
fn mapping_restoration_is_read_only_exact_and_never_replaces_an_id() {
    let mut map = Mappings::new(namespace());
    let command = command(session());
    assert!(matches!(map.adopt(&command, 42), Outcome::Delivered { .. }));
    let value = map.rows[&42].clone();
    assert!(map.restore(vec![value.clone()]));
    let mut changed = value.clone();
    changed.request_id = uuid::Uuid::new_v4().to_string();
    assert!(!map.restore(vec![changed]));
    assert_eq!(map.rows[&42], value);
    let mut changed = value.clone();
    if let NativeId::Linux { bus_id, .. } = &mut changed.native_id {
        *bus_id = "b".repeat(32);
    }
    assert!(map.restore(vec![changed.clone()]));
    assert_eq!(map.rows[&42], value);
    if let NativeId::Linux { bus_id, owner, .. } = &mut changed.native_id {
        *bus_id = namespace().bus_id;
        *owner = ":1.99".into();
    }
    assert!(map.restore(vec![changed.clone()]));
    let mut context = changed.clone();
    context.platform_context = Some(PlatformContext::Wayland {
        activation_token: "not-persisted".into(),
    });
    assert!(!map.restore(vec![context]));
    let mut platform = changed.clone();
    platform.native_id = NativeId::Windows {
        tag: "fixture".into(),
        group: "fixture".into(),
    };
    assert!(!map.restore(vec![platform]));
    let mut zero = changed.clone();
    if let NativeId::Linux { id, .. } = &mut zero.native_id {
        *id = 0;
    }
    assert!(!map.restore(vec![zero]));
    changed.request_id = "malformed".into();
    assert!(!map.restore(vec![changed]));
    assert_eq!(map.rows[&42], value);
    assert!(matches!(map.adopt(&command, 42), Outcome::Unknown { .. }));
    assert!(matches!(map.adopt(&command, 0), Outcome::Unknown { .. }));
}

#[test]
fn callback_context_is_ephemeral_and_closed_removes_the_exact_mapping() {
    let s = session();
    let (outbox, endpoint) = super::super::Outbox::new(s.clone()).unwrap();
    let mut map = Mappings::new(namespace());
    let command = command(s);
    map.adopt(&command, 42);
    assert!(map.signal(
        &Signal::ActivationToken {
            id: 42,
            token: "wayland-activation".into()
        },
        &endpoint
    ));
    assert!(map.rows[&42].platform_context.is_none());
    assert!(map.signal(
        &Signal::ActionInvoked {
            id: 42,
            action: "default".into()
        },
        &endpoint
    ));
    let clicked = outbox.events.try_recv().unwrap();
    assert!(matches!(&clicked.event, Event::Clicked(v)
        if matches!(&v.platform_context,Some(PlatformContext::Wayland { activation_token }) if activation_token=="wayland-activation")));
    assert!(map.tokens.is_empty());
    assert!(map.signal(&Signal::NotificationClosed { id: 42, reason: 2 }, &endpoint));
    assert!(map.rows.is_empty());
    let closed = outbox.events.try_recv().unwrap();
    assert!(matches!(&closed.event, Event::Closed(v) if v.platform_context.is_none()));
    assert!(map.signal(
        &Signal::ActionInvoked {
            id: 42,
            action: "default".into()
        },
        &endpoint
    ));
}

#[test]
fn mapping_capacity_never_evicts_and_a_restore_page_is_all_or_nothing() {
    let mut map = Mappings::new(namespace());
    let command = command(session());
    for id in 1..wire::MAPPINGS as u32 {
        assert!(matches!(map.adopt(&command, id), Outcome::Delivered { .. }));
    }
    let mut one = map.rows[&1].clone();
    let mut two = one.clone();
    if let NativeId::Linux { id, .. } = &mut one.native_id {
        *id = wire::MAPPINGS as u32;
    }
    if let NativeId::Linux { id, .. } = &mut two.native_id {
        *id = wire::MAPPINGS as u32 + 1;
    }
    assert!(!map.restore(vec![one.clone(), two]));
    assert_eq!(map.rows.len(), wire::MAPPINGS - 1);
    assert!(map.restore(vec![one]));
    assert!(matches!(
        map.adopt(&command, wire::MAPPINGS as u32 + 1),
        Outcome::Unknown { .. }
    ));
    assert!(map.rows.contains_key(&1));
    assert_eq!(map.rows.len(), wire::MAPPINGS);
}

#[test]
fn cleanup_keeps_the_first_deadline_without_a_mutex_or_renewal() {
    let scope = Shared {
        stop: AtomicBool::new(false),
        ready: AtomicBool::new(false),
        failed: AtomicBool::new(false),
        born: Instant::now(),
        cleanup: AtomicU64::new(0),
    };
    let first = scope.cleanup_deadline();
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(scope.cleanup_deadline(), first);
}

#[test]
fn native_reply_requires_the_pinned_sender_and_method_return_type() {
    let call = zbus::Message::method_call("/", "Example")
        .unwrap()
        .build(&())
        .unwrap();
    for owner in [":1.42", ":1.99"] {
        let reply = zbus::Message::method_return(&call.header())
            .unwrap()
            .sender(owner)
            .unwrap()
            .build(&42_u32)
            .unwrap();
        assert_eq!(worker::reply_from(&reply, ":1.42"), owner == ":1.42");
    }
    let signal = zbus::Message::signal("/", "org.example.Fixture", "Example")
        .unwrap()
        .sender(":1.42")
        .unwrap()
        .build(&42_u32)
        .unwrap();
    assert!(!worker::reply_from(&signal, ":1.42"));
}

#[test]
fn unsupported_action_consumes_its_ephemeral_token_without_a_click() {
    let s = session();
    let (outbox, endpoint) = Outbox::new(s.clone()).unwrap();
    let mut mappings = Mappings::new(namespace());
    mappings.adopt(&command(s), 42);
    assert!(mappings.signal(
        &Signal::ActivationToken {
            id: 42,
            token: "one-use".into()
        },
        &endpoint
    ));
    assert!(mappings.signal(
        &Signal::ActionInvoked {
            id: 42,
            action: "unsupported".into()
        },
        &endpoint
    ));
    assert!(mappings.tokens.is_empty());
    assert!(outbox.events.try_recv().is_err());
}

#[track_caller]
fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT + Duration::from_secs(1);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "private notification fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
struct Fake {
    next: AtomicU32,
    called: mpsc::SyncSender<String>,
    closed: mpsc::SyncSender<u32>,
    mode: Arc<AtomicU8>,
}
impl Fake {
    async fn closed_range(&self, connection: &zbus::Connection, start: u32, end: u32) {
        for id in start..end {
            connection
                .emit_signal(
                    None::<&str>,
                    native_path(),
                    zerus_freedesktop_notify::SERVICE,
                    "NotificationClosed",
                    &(id, 2_u32),
                )
                .await
                .unwrap();
        }
    }
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Fake {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        _app: String,
        _replace: u32,
        _icon: String,
        _title: String,
        body: String,
        _actions: Vec<String>,
        _hints: BTreeMap<String, zbus::zvariant::OwnedValue>,
        _timeout: i32,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<u32> {
        self.called.try_send(body).unwrap();
        match self.mode.load(Ordering::Acquire) {
            1 => Err(zbus::fdo::Error::Failed("fixture error".into())),
            2 => Ok(0),
            5 => Ok(self.next.fetch_add(1, Ordering::AcqRel)),
            6 => {
                let id = self.next.fetch_add(1, Ordering::AcqRel);
                self.closed_range(connection, id.saturating_sub(64), id)
                    .await;
                Ok(id)
            }
            3 => {
                connection
                    .emit_signal(
                        None::<&str>,
                        native_path(),
                        zerus_freedesktop_notify::SERVICE,
                        "NotificationClosed",
                        &(42_u32, 2_u32),
                    )
                    .await
                    .unwrap();
                Ok(42)
            }
            _ => Ok(42),
        }
    }
    async fn close_notification(&self, id: u32, #[zbus(connection)] connection: &zbus::Connection) {
        self.closed.try_send(id).unwrap();
        if self.mode.load(Ordering::Acquire) == 7 {
            let end = self.next.load(Ordering::Acquire);
            self.closed_range(connection, end.saturating_sub(64), end)
                .await;
        }
        connection
            .emit_signal(
                None::<&str>,
                native_path(),
                zerus_freedesktop_notify::SERVICE,
                "NotificationClosed",
                &(id, 2_u32),
            )
            .await
            .unwrap();
    }
}
fn native_path() -> &'static str {
    zerus_freedesktop_notify::PATH
}
struct Server {
    connection: zbus::blocking::Connection,
    called: mpsc::Receiver<String>,
    closed: mpsc::Receiver<u32>,
    mode: Arc<AtomicU8>,
}
impl Server {
    fn new(bus: &bus::Bus) -> Self {
        Self::at(&bus.address)
    }
    fn at(address: &str) -> Self {
        let (called, calls) = mpsc::sync_channel(8);
        let (closed, closes) = mpsc::sync_channel(8);
        let mode = Arc::new(AtomicU8::new(0));
        let connection = zbus::blocking::connection::Builder::address(address)
            .unwrap()
            .name(zerus_freedesktop_notify::SERVICE)
            .unwrap()
            .serve_at(
                native_path(),
                Fake {
                    next: AtomicU32::new(1),
                    called,
                    closed,
                    mode: mode.clone(),
                },
            )
            .unwrap()
            .method_timeout(WAIT)
            .build()
            .unwrap();
        Self {
            connection,
            called: calls,
            closed: closes,
            mode,
        }
    }
    fn click(&self, id: u32) {
        self.connection
            .emit_signal(
                None::<&str>,
                native_path(),
                zerus_freedesktop_notify::SERVICE,
                "ActivationToken",
                &(id, "fixture-wayland"),
            )
            .unwrap();
        self.connection
            .emit_signal(
                None::<&str>,
                native_path(),
                zerus_freedesktop_notify::SERVICE,
                "ActionInvoked",
                &(id, "default"),
            )
            .unwrap();
    }
    fn close_burst(&self, count: u32) {
        for id in 1..=count {
            self.connection
                .emit_signal(
                    None::<&str>,
                    native_path(),
                    zerus_freedesktop_notify::SERVICE,
                    "NotificationClosed",
                    &(id, 2_u32),
                )
                .unwrap();
        }
    }
}
struct Harness {
    _root: tempfile::TempDir,
    store: Store,
    outbox: Outbox,
    endpoint: Option<Endpoint>,
}
impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let location = Location {
            root: root.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let session = session();
        let mut store = Store::open(&location, &session.service_epoch).unwrap();
        ledger::authorize(&mut store.db, &session.writer_epoch, &session.service_epoch).unwrap();
        let (outbox, endpoint) = Outbox::new(session).unwrap();
        Self {
            _root: root,
            store,
            outbox,
            endpoint: Some(endpoint),
        }
    }
    fn start(&mut self, address: &str) -> Runtime {
        Runtime::start(
            self.outbox.session.clone(),
            self.endpoint.take().unwrap(),
            address.to_owned(),
        )
        .unwrap()
    }
    fn activate(runtime: &Runtime) {
        let ack = runtime.finish_restoration().unwrap();
        until(|| ack.poll() == Some(true));
        assert_eq!(runtime.status()["ready"], true);
    }
    fn offer(&mut self, action: Action, kind: Kind) -> Reserve {
        let id = ledger::put_intent(
            &mut self.store.db,
            &Identity {
                kind,
                target: Target::Session {
                    computer: "peer.example".into(),
                    run: "run-1".into(),
                    conversation: "conversation-1".into(),
                },
                edge: uuid::Uuid::new_v4().to_string(),
            },
            1,
            &serde_json::to_value(action).unwrap(),
        )
        .unwrap();
        let s = &self.outbox.session;
        let request = Reserve {
            request_id: uuid::Uuid::new_v4().to_string(),
            intent_id: id,
            revision: 1,
            epoch: s.writer_epoch.clone(),
            service_epoch: s.service_epoch.clone(),
            adapter: s.adapter.clone(),
            lease: s.lease.clone(),
            retry_of: None,
        };
        assert!(matches!(
            self.outbox
                .offer(&mut self.store, request.clone(), Instant::now())
                .unwrap(),
            Offered::Queued { .. }
        ));
        request
    }
    fn post(&mut self) -> Reserve {
        self.offer(command(self.outbox.session.clone()).action, Kind::Post)
    }
    fn state(&self, request: &Reserve) -> String {
        self.store
            .db
            .query_row(
                "SELECT state FROM desktop_attempts WHERE request_id=?1",
                [&request.request_id],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn settle(&mut self, request: &Reserve, state: &str) {
        until(|| {
            self.outbox.poll(&mut self.store, Instant::now());
            self.state(request) == state
        });
    }
    fn mappings(&self) -> Vec<Activation> {
        let head = self.outbox.restore_head(&self.store).unwrap();
        assert!(head <= 256, "private fixture attempt bound");
        let mut after = 0;
        let mut values = Vec::new();
        for _ in 0..16 {
            let page = self.outbox.restore_page(&self.store, after, head).unwrap();
            values.extend(page.mappings);
            if page.complete {
                return values;
            }
            assert!(page.after > after);
            after = page.after;
        }
        panic!("private fixture restoration page bound");
    }
    fn stop(&mut self, runtime: &mut Runtime) {
        self.outbox.request_stop();
        runtime.request_stop();
        until(|| {
            self.outbox.poll(&mut self.store, Instant::now());
            runtime.drained() && self.outbox.operation_drained()
        });
    }
    fn shown(&mut self, server: &Server, count: u32) {
        // Exercise real original requests in the existing eight-command bound.
        for base in (0..count).step_by(wire::IN_FLIGHT) {
            let requests: Vec<_> = (0..(count - base).min(wire::IN_FLIGHT as u32))
                .map(|_| self.post())
                .collect();
            for _ in &requests {
                server.called.recv_timeout(WAIT).unwrap();
            }
            for request in requests {
                self.settle(&request, "delivered");
            }
        }
    }
    fn pause_batch(runtime: &Runtime) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (reached, observed) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let (reply, installed) = mpsc::sync_channel(1);
        runtime
            .control
            .try_send(Control::PauseBatch(
                BatchPause {
                    reached,
                    resume: paused,
                },
                reply,
            ))
            .ok()
            .unwrap();
        installed.recv_timeout(WAIT).unwrap();
        (observed, resume)
    }
    fn live(&self) -> usize {
        self.store
            .db
            .query_row(
                "SELECT COUNT(*) FROM desktop_intents
                 WHERE withdrawn=0 AND json_extract(identity,'$.kind')='post'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

#[test]
fn a_large_clear_all_burst_backpressures_the_actor_and_keeps_the_adapter_ready() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    server.mode.store(5, Ordering::Release);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    h.shown(&server, 128);
    server.close_burst(128);
    // Deliberately stop polling the actor until its fixed 32 permits fill.
    until(|| {
        h.outbox.event_count.load(Ordering::Acquire) == 32
            || runtime.shared.failed.load(Ordering::Acquire)
    });
    std::thread::sleep(WAIT + Duration::from_millis(200));
    assert!(!runtime.shared.failed.load(Ordering::Acquire));
    assert_eq!(runtime.status()["ready"], true);
    let deadline = Instant::now() + WAIT;
    while h.live() != 0 {
        h.outbox.poll(&mut h.store, Instant::now());
        assert!(
            Instant::now() < deadline,
            "burst settlement: live={}, actor={}, runtime={}",
            h.live(),
            h.outbox.status(),
            runtime.status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.outbox.status()["callbacks"], 0);
    let next = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    h.settle(&next, "delivered");
    assert_eq!(runtime.status()["ready"], true);
    h.stop(&mut runtime);
}

#[test]
fn stop_drains_more_than_one_raw_queue_of_closed_signals_before_clean_join() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    server.mode.store(5, Ordering::Release);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    h.shown(&server, 64);
    let (in_batch, resume) = Harness::pause_batch(&runtime);
    server.close_burst(64);
    in_batch.recv_timeout(WAIT).unwrap();
    // Stop after the batch captured its lookup, before its first drive. This
    // forces the cancellation path rather than relying on thread scheduling.
    h.outbox.request_stop();
    runtime.request_stop();
    resume.send(()).unwrap();
    h.stop(&mut runtime);
    assert_eq!(h.live(), 0);
}

#[test]
fn stop_during_the_post_command_batch_preserves_its_receipt_and_closed_drain() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    server.mode.store(3, Ordering::Release);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    let (in_batch, resume) = Harness::pause_batch(&runtime);
    let request = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    in_batch.recv_timeout(WAIT).unwrap();
    assert!(
        h.outbox.pending[0].shared.reply.lock().unwrap().is_none(),
        "pause must precede the original post receipt, in its post-command batch"
    );
    h.outbox.request_stop();
    runtime.request_stop();
    resume.send(()).unwrap();
    h.stop(&mut runtime);
    assert_eq!(h.state(&request), "delivered");
    assert_eq!(h.live(), 0);
}

#[test]
fn mapped_callback_bursts_preserve_the_original_notify_and_close_futures() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    server.mode.store(5, Ordering::Release);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    h.shown(&server, 64);
    // The original Notify emits 64 mapped callbacks BEFORE its one reply.
    server.mode.store(6, Ordering::Release);
    let post = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    h.settle(&post, "delivered");
    until(|| {
        h.outbox.poll(&mut h.store, Instant::now());
        h.live() == 1
    });
    let original = h.mappings().remove(0);
    assert_eq!(original.request_id, post.request_id);
    assert!(matches!(
        &original.native_id,
        NativeId::Linux { id: 65, .. }
    ));
    assert!(
        server.called.try_recv().is_err(),
        "Notify must not be replayed"
    );

    server.mode.store(5, Ordering::Release);
    h.shown(&server, 64);
    server.mode.store(7, Ordering::Release);
    ledger::withdraw(&mut h.store.db, &post.intent_id, post.revision).unwrap();
    let close = h.offer(
        Action::Withdraw {
            token: original.token,
            native_id: original.native_id,
        },
        Kind::Withdrawal,
    );
    assert_eq!(server.closed.recv_timeout(WAIT).unwrap(), 65);
    h.settle(&close, "delivered");
    until(|| {
        h.outbox.poll(&mut h.store, Instant::now());
        h.live() == 0
    });
    assert!(
        server.closed.try_recv().is_err(),
        "Close must not be replayed"
    );
    // With no current mappings, the same full burst cannot be correlated with
    // the new post before its reply. It remains Unknown without replay.
    server.mode.store(6, Ordering::Release);
    let unknown = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    h.settle(&unknown, "unknown");
    assert!(h.mappings().is_empty());
    assert!(matches!(
        h.outbox
            .offer(&mut h.store, unknown, Instant::now())
            .unwrap(),
        Offered::Existing(_)
    ));
    assert!(server.called.try_recv().is_err());
    assert_eq!(runtime.status()["ready"], true);
    h.stop(&mut runtime);
}

#[test]
fn malformed_or_excess_activation_context_is_optional_and_emit_retries_keep_identity() {
    let s = session();
    let (outbox, endpoint) = Outbox::new(s.clone()).unwrap();
    let mut mappings = Mappings::new(namespace());
    let command = command(s);
    for id in 1..=34 {
        mappings.adopt(&command, id);
    }
    for id in 1..=32 {
        assert!(mappings.signal(
            &Signal::ActivationToken {
                id,
                token: "valid".into()
            },
            &endpoint
        ));
    }
    assert!(mappings.signal(
        &Signal::ActivationToken {
            id: 33,
            token: "extra".into()
        },
        &endpoint
    ));
    assert!(!mappings.tokens.contains_key(&33));
    for token in [
        String::new(),
        "bad\ncontext".into(),
        "x".repeat(wire::ACTIVATION_TOKEN_BYTES + 1),
    ] {
        assert!(mappings.signal(&Signal::ActivationToken { id: 1, token }, &endpoint));
        assert!(!mappings.tokens.contains_key(&1));
    }
    // Fill the real actor queue with clicks, then refuse both a click and Closed.
    for _ in 0..32 {
        assert!(mappings.signal(
            &Signal::ActionInvoked {
                id: 2,
                action: "default".into()
            },
            &endpoint
        ));
    }
    let closed = Signal::NotificationClosed { id: 3, reason: 2 };
    let clicked = Signal::ActionInvoked {
        id: 4,
        action: "default".into(),
    };
    assert!(!mappings.signal(&closed, &endpoint));
    assert!(!mappings.signal(&clicked, &endpoint));
    assert!(mappings.rows.contains_key(&3));
    assert!(mappings.tokens.contains_key(&4));
    drop(outbox.events.try_recv().unwrap());
    assert!(mappings.signal(&closed, &endpoint));
    assert!(!mappings.rows.contains_key(&3));
    drop(outbox.events.try_recv().unwrap());
    assert!(mappings.signal(&clicked, &endpoint));
    assert!(!mappings.tokens.contains_key(&4));
}

#[test]
fn actual_worker_waits_for_restoration_then_posts_clicks_and_withdraws_once() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    let request = h.post();
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        server.called.try_recv().is_err(),
        "restoration gate precedes Endpoint consumption"
    );
    Harness::activate(&runtime);
    assert_eq!(
        server.called.recv_timeout(WAIT).unwrap(),
        "&lt;b&gt;literal &amp; text&lt;/b&gt;"
    );
    h.settle(&request, "delivered");
    let original = h.mappings().remove(0);
    assert!(
        matches!(&original.native_id, NativeId::Linux { owner, id:42, .. }
        if owner==server.connection.unique_name().unwrap().as_str())
    );
    server.click(42);
    let mut clicked = Vec::new();
    until(|| {
        clicked.extend(h.outbox.poll(&mut h.store, Instant::now()));
        !clicked.is_empty()
    });
    assert_eq!(clicked.len(), 1);
    assert_eq!(clicked[0].request_id, request.request_id);
    assert!(
        matches!(&clicked[0].platform_context,Some(PlatformContext::Wayland { activation_token }) if activation_token=="fixture-wayland")
    );
    assert!(h.mappings()[0].platform_context.is_none());
    ledger::withdraw(&mut h.store.db, &request.intent_id, 1).unwrap();
    let withdrawal = h.offer(
        Action::Withdraw {
            token: original.token,
            native_id: original.native_id,
        },
        Kind::Withdrawal,
    );
    assert_eq!(server.closed.recv_timeout(WAIT).unwrap(), 42);
    h.settle(&withdrawal, "delivered");
    assert!(server.called.try_recv().is_err());
    h.stop(&mut runtime);
}

#[test]
fn actual_native_errors_and_zero_ids_are_unknown_without_replay() {
    for mode in [1, 2] {
        let bus = bus::Bus::new();
        let server = Server::new(&bus);
        server.mode.store(mode, Ordering::Release);
        let mut h = Harness::new();
        let mut runtime = h.start(&bus.address);
        Harness::activate(&runtime);
        let request = h.post();
        server.called.recv_timeout(WAIT).unwrap();
        h.settle(&request, "unknown");
        assert!(h.mappings().is_empty());
        assert!(matches!(
            h.outbox
                .offer(&mut h.store, request, Instant::now())
                .unwrap(),
            Offered::Existing(_)
        ));
        assert!(server.called.try_recv().is_err());
        h.stop(&mut runtime);
    }
}

#[test]
fn stopping_during_restoration_refuses_queued_work_and_joins_the_original_host() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    let ack = runtime.restore(Vec::new()).unwrap();
    until(|| ack.poll() == Some(true));
    let request = h.post();
    h.stop(&mut runtime);
    assert_eq!(h.state(&request), "refused");
    assert!(server.called.try_recv().is_err());
}

#[test]
fn actual_early_close_keeps_original_dispatch_charge_and_never_resurrects() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    server.mode.store(3, Ordering::Release);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    let request = h.post();
    let held = h.outbox.pending[0].shared.clone();
    server.called.recv_timeout(WAIT).unwrap();
    until(|| h.outbox.event_count.load(Ordering::Acquire) > 0);
    h.settle(&request, "delivered");
    assert!(
        !h.outbox.operations_idle(),
        "original resources remain charged"
    );
    assert!(h.mappings().is_empty());
    drop(held);
    h.outbox.poll(&mut h.store, Instant::now());
    assert!(h.outbox.operations_idle());
    assert!(h.mappings().is_empty());
    h.stop(&mut runtime);
}

#[test]
fn actual_restore_rebuilds_callbacks_without_notify_and_rejects_other_daemon() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    let request = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    h.settle(&request, "delivered");
    let values = h.mappings();
    h.stop(&mut runtime);
    let mut session = h.outbox.session.clone();
    session.adapter = uuid::Uuid::new_v4().to_string();
    session.host_epoch = uuid::Uuid::new_v4().to_string();
    session.lease = uuid::Uuid::new_v4().to_string();
    let (outbox, endpoint) = Outbox::new(session).unwrap();
    h.outbox = outbox;
    h.endpoint = Some(endpoint);
    let mut runtime = h.start(&bus.address);
    let ack = runtime.restore(values).unwrap();
    until(|| ack.poll() == Some(true));
    Harness::activate(&runtime);
    assert!(server.called.try_recv().is_err());
    server.click(42);
    let mut clicked = Vec::new();
    until(|| {
        clicked.extend(h.outbox.poll(&mut h.store, Instant::now()));
        !clicked.is_empty()
    });
    assert_eq!(clicked[0].request_id, request.request_id);
    server
        .connection
        .release_name(zerus_freedesktop_notify::SERVICE)
        .unwrap();
    let replacement = Server::new(&bus);
    until(|| runtime.shared.failed.load(Ordering::Acquire));
    until(|| runtime.thread.as_ref().is_some_and(JoinHandle::is_finished));
    assert!(
        runtime.drained(),
        "the original socket/executor actually closed before join"
    );
    assert_eq!(
        runtime.status()["clean"],
        false,
        "namespace loss is never a clean callback fence"
    );
    assert_eq!(runtime.status()["error"], true);
    assert!(replacement.called.try_recv().is_err());
    assert!(replacement.closed.try_recv().is_err());
}

#[test]
fn actual_stop_drains_ordered_closed_callbacks_but_never_opens_a_late_click() {
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
    let mut h = Harness::new();
    let mut runtime = h.start(&bus.address);
    Harness::activate(&runtime);
    let request = h.post();
    server.called.recv_timeout(WAIT).unwrap();
    h.settle(&request, "delivered");
    let original = h.mappings().remove(0);
    h.stop(&mut runtime);
    let mut next = h.outbox.session.clone();
    next.adapter = uuid::Uuid::new_v4().to_string();
    next.host_epoch = uuid::Uuid::new_v4().to_string();
    next.lease = uuid::Uuid::new_v4().to_string();
    let (outbox, endpoint) = Outbox::new(next).unwrap();
    h.outbox = outbox;
    assert!(worker::test_close_with_queued_callbacks(
        bus.address.clone(),
        endpoint,
        original.clone(),
        || {
            h.outbox.request_stop();
            server.click(42);
            server
                .connection
                .emit_signal(
                    None::<&str>,
                    native_path(),
                    zerus_freedesktop_notify::SERVICE,
                    "NotificationClosed",
                    &(42_u32, 2_u32),
                )
                .unwrap();
            // emit_signal only writes this producer's socket. Fence it on the
            // SAME connection before the consumer sends RemoveMatch: otherwise
            // the bus can remove the match before forwarding these callbacks.
            let reply = server
                .connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetId",
                    &(),
                )
                .unwrap();
            assert_eq!(
                reply.header().sender().map(|s| s.as_str()),
                Some("org.freedesktop.DBus")
            );
            let bus_id: String = reply.body().deserialize().unwrap();
            assert!(
                matches!(&original.native_id, NativeId::Linux { bus_id: expected, .. } if *expected == bus_id)
            );
        }
    ));
    assert_eq!(h.outbox.status()["callbacks"], 1);
    assert!(h.outbox.poll(&mut h.store, Instant::now()).is_empty());
    assert!(h
        .outbox
        .current_activation(&h.store, &original)
        .unwrap()
        .is_none());
    assert!(h.outbox.operation_drained());
}

#[test]
fn attachment_after_daemon_replacement_skips_old_live_and_cleanup_mappings() {
    use super::super::attachment;
    let _serial = attachment::TEST_SERIAL.lock().unwrap();
    for retired in [false, true] {
        let bus = bus::Bus::new();
        let old = Server::new(&bus);
        let mut h = Harness::new();
        let mut runtime = h.start(&bus.address);
        Harness::activate(&runtime);
        let request = h.post();
        old.called.recv_timeout(WAIT).unwrap();
        h.settle(&request, "delivered");
        h.store.db.execute("INSERT INTO desktop_attention_edges(logical,session,intent,identity,payload,blocked,bytes)
            SELECT 'fixture','fixture',id,identity,payload,0,0 FROM desktop_intents WHERE id=?1",[&request.intent_id]).unwrap();
        h.stop(&mut runtime);
        if retired {
            ledger::withdraw(&mut h.store.db, &request.intent_id, 1).unwrap();
            h.store
                .db
                .execute(
                    "UPDATE desktop_attention_edges SET live=0,blocked=1,dirty=1",
                    [],
                )
                .unwrap();
        }
        old.connection
            .release_name(zerus_freedesktop_notify::SERVICE)
            .unwrap();
        let replacement = Server::new(&bus);
        let location = Location {
            root: h._root.path().canonicalize().unwrap().join("state/desktop"),
            configuration: h.outbox.session.configuration.clone(),
        };
        let mut session = h.outbox.session.clone();
        session.adapter = uuid::Uuid::new_v4().to_string();
        session.host_epoch = uuid::Uuid::new_v4().to_string();
        session.lease = uuid::Uuid::new_v4().to_string();
        let mut attached = attachment::Runtime::start(
            &location,
            &h.store,
            session,
            attachment::Selection::Linux {
                address: bus.address.clone(),
            },
            Arc::new(()),
        )
        .unwrap();
        let mut attention =
            crate::desktop::attention::Runtime::new(&h.outbox.session.service_epoch);
        until(|| {
            attached.poll(&mut h.store, &mut attention);
            attached.status()["restored"] == true && attached.status()["host"]["ready"] == true
        });
        assert!(
            replacement.called.try_recv().is_err(),
            "no replay on the new daemon"
        );
        assert!(
            replacement.closed.try_recv().is_err(),
            "no old ID redirected to the new daemon"
        );
        attached.request_stop();
        until(|| {
            attached.poll(&mut h.store, &mut attention);
            attached.drained()
        });
        assert!(replacement.closed.try_recv().is_err());
    }
}

#[test]
fn root_service_grant_attaches_original_host_posts_and_routes_click_then_releases_on_revoke() {
    use crate::desktop::{
        attention,
        fanout::Queue,
        notifications::{activation, attachment, preparation, service},
        ownership, ui_authority,
    };
    let _prepare = preparation::TEST_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _attach = attachment::TEST_SERIAL.lock().unwrap();
    let bus = bus::Bus::new();
    let server = Server::new(&bus);
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
    let owner = Arc::new(location.lock().unwrap());
    let mut runtime = service::Runtime::new(
        &config,
        &location,
        &h.outbox.session.service_epoch,
        owner.clone(),
    );
    let mut attention = attention::Runtime::new(&h.outbox.session.service_epoch);
    let mut router =
        activation::Router::new(&location.configuration, &h.outbox.session.service_epoch);
    let mut seq = 0;
    runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
    assert_eq!(
        runtime.status()["attempted"],
        false,
        "no factory without native grant"
    );
    assert!(server.called.try_recv().is_err());
    assert!(location.handoff_lock().is_ok());
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
    runtime.begin_on_test_bus(&mut h.store, bus.address.clone());
    until(|| {
        runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
        runtime.ready()
    });
    assert!(location.handoff_lock().is_err());
    let report = crate::desktop::import_evidence::publish_fixture_for_test(&location).unwrap();
    assert_eq!(report["code"], "handoff_busy");
    assert_eq!(report["this_attempt_not_committed"], true);
    assert!(!location
        .root
        .join("imports")
        .join("11111111-1111-4111-8111-111111111111")
        .exists());
    runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
    assert!(
        runtime.ready(),
        "a deferred Staged report leaves the live host ready"
    );
    let publication = crate::desktop::import_evidence::held::pending_for_test(
        &location,
        runtime
            .publication_owner(&h.store)
            .unwrap()
            .expect("original current granted owner"),
    );
    assert!(publication.owned.join("record-v1.json").is_file());
    let report = publication.finish_connected();
    assert_eq!(report["published"], true);
    h.store.db.execute_batch("SAVEPOINT unavailable_owner; ALTER TABLE desktop_ownership RENAME TO unavailable_ownership;").unwrap();
    assert!(
        runtime.publication_owner(&h.store).is_err(),
        "a failed original-grant query remains unknown, never a known ownerless refusal"
    );
    h.store
        .db
        .execute_batch("ROLLBACK TO unavailable_owner; RELEASE unavailable_owner;")
        .unwrap();
    runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
    assert!(
        runtime.ready(),
        "admitted report does not stop the granted runtime"
    );
    let action = command(session()).action;
    let intent = ledger::put_intent(
        &mut h.store.db,
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
        &serde_json::to_value(action).unwrap(),
    )
    .unwrap();
    let request = runtime.offer_for_test(&mut h.store, &intent);
    h.store.db.execute("INSERT INTO desktop_attention_edges(logical,session,intent,identity,payload,blocked,bytes)
        SELECT 'root_fixture','fixture',id,identity,payload,0,0 FROM desktop_intents WHERE id=?1",[&intent]).unwrap();
    server.called.recv_timeout(WAIT).unwrap();
    until(|| {
        runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
        h.store
            .db
            .query_row(
                "SELECT state='delivered' FROM desktop_attempts WHERE request_id=?1",
                [&request],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    });
    let wait = uuid::Uuid::new_v4().to_string();
    let queue = Arc::new(Queue::default());
    router
        .wait(
            ui_authority::Validated::fixture(&json!({"op":"activation_wait","request_id":wait})),
            queue.clone(),
        )
        .unwrap();
    server.click(42);
    let mut frame = None;
    until(|| {
        runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
        frame = queue.take();
        frame.is_some()
    });
    let frame: serde_json::Value = serde_json::from_slice(&frame.unwrap()).unwrap();
    let reply: zerus_client_core::desktop::activation::Reply =
        serde_json::from_value(frame["value"].clone()).unwrap();
    assert!(reply.matches(
        &wait,
        &location.configuration,
        &h.outbox.session.service_epoch
    ));
    let click = reply.activation.unwrap();
    assert_eq!(click.post_request_id, request);
    assert_eq!(click.target.conversation, "conversation-1");
    assert!(click.platform_context.is_some());
    let mut publication = crate::desktop::import_evidence::held::pending_for_test(
        &location,
        runtime
            .publication_owner(&h.store)
            .unwrap()
            .expect("same still-current granted owner"),
    );
    let published_id = publication.import_id.clone();
    ownership::invalidate(&h.store.db).unwrap();
    ledger::revoke(&h.store.db, "fixture_revoke").unwrap();
    publication.disconnect();
    runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
    assert!(
        runtime.publication_owner(&h.store).unwrap().is_none(),
        "revoke refuses new publication jobs"
    );
    assert!(
        !runtime.drained(),
        "the SAME original owner is still charged by the admitted worker"
    );
    assert!(location.handoff_lock().is_err());
    assert!(
        publication.owned.join("record-v1.json").is_file(),
        "caller disconnect never deletes the admitted input"
    );
    publication.finish();
    assert_eq!(crate::desktop::import_evidence::status(&location, &published_id).unwrap()["record_present"], true,
        "the original admitted finalization can finish while revoke waits; its lost reply is lookup-only");
    until(|| {
        runtime.poll(&mut h.store, &mut attention, &mut router, &mut seq);
        runtime.drained()
    });
    assert!(!runtime.ready());
    assert_eq!(
        server.closed.recv_timeout(WAIT).unwrap(),
        42,
        "actual native close precedes original host join"
    );
    assert!(location.handoff_lock().is_ok());
    assert!(
        server.called.try_recv().is_err(),
        "no post replay on revoke/drain"
    );
    drop(runtime);
    drop(owner);
    assert!(location.lock().is_ok());
}
