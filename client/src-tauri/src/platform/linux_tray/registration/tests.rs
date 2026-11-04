use super::super::*;
use std::sync::mpsc;
use zerus_client_core::desktop::drain::{Gate, Observation, Role};

#[derive(Default)]
struct Hold {
    released: AtomicBool,
    wake: Mutex<Option<Waker>>,
}
impl Hold {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        if let Some(wake) = lock(&self.wake).take() {
            wake.wake();
        }
    }
    async fn wait(&self) {
        std::future::poll_fn(|context| {
            let mut wake = lock(&self.wake);
            if self.released.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                *wake = Some(context.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
struct Watcher {
    started: mpsc::Sender<String>,
    hold: Option<Arc<Hold>>,
}
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(connection)] connection: &zbus::Connection,
    ) {
        // Every message goes to the item's actual connection before this
        // method replies. The old blocking registration stalled after queue2.
        for index in 0..3_i32 {
            connection
                .emit_signal(
                    Some(service.as_str()),
                    "/Fixture",
                    "org.example.Fixture",
                    "Traffic",
                    &(index,),
                )
                .await
                .unwrap();
        }
        let _ = self.started.send(service);
        if let Some(hold) = &self.hold {
            hold.wait().await;
        }
    }
}
fn watcher(
    bus: &crate::platform::linux_test_bus::Bus,
    hold: Option<Arc<Hold>>,
) -> (Connection, mpsc::Receiver<String>) {
    let (started, incoming) = mpsc::channel();
    let connection = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(WATCHER)
        .unwrap()
        .method_timeout(Duration::from_secs(5))
        .serve_at("/StatusNotifierWatcher", Watcher { started, hold })
        .unwrap()
        .build()
        .unwrap();
    (connection, incoming)
}
fn activate(
    bus: &crate::platform::linux_test_bus::Bus,
    native: &Native,
    incoming: &mpsc::Receiver<MenuAction>,
) {
    let client = bus.connect();
    let item = Proxy::new(
        &client,
        native.connection.unique_name().unwrap().as_str(),
        PATH,
        SNI,
    )
    .unwrap();
    let before = Instant::now();
    item.call::<_, _, ()>("Activate", &(0_i32, 0_i32)).unwrap();
    assert_eq!(
        incoming.recv_timeout(Duration::from_secs(1)).unwrap(),
        MenuAction::Open
    );
    assert!(
        before.elapsed() < Duration::from_secs(2),
        "activation stalled behind registration timeout"
    );
}

#[test]
fn three_item_messages_before_the_watcher_reply_do_not_stall_registration_or_activation() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let (_watcher, registered) = watcher(&bus, None);
    let gate = Gate::new(1).unwrap();
    let (scope, control, startup) = badge::Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
    let (sent, incoming) = mpsc::channel();
    let native = start_with_badge(
        Rgb::new(255, 255, 255),
        Arc::new(move |action, _| {
            let _ = sent.send(action);
        }),
        Some(&bus.address),
        Some(scope.clone()),
    )
    .unwrap();
    scope.ready();
    startup.complete();
    assert_eq!(
        registered.recv_timeout(Duration::from_secs(5)).unwrap(),
        format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
    );
    activate(&bus, &native, &incoming);
    let client = bus.connect();
    let query = Proxy::new(
        &client,
        native.connection.unique_name().unwrap().as_str(),
        BADGE_PATH,
        BADGE,
    )
    .unwrap();
    let (_, props): (String, Props) = query.call("Query", &()).unwrap();
    assert_eq!(i64::try_from(&props["count"]).unwrap(), 0);
    control.close().unwrap();
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(5)),
        Observation::Idle
    );
    activate(&bus, &native, &incoming);
}

#[test]
fn pending_ui_registration_survives_badge_close_and_cannot_erase_a_new_watcher() {
    let bus = crate::platform::linux_test_bus::Bus::new();
    let hold = Arc::new(Hold::default());
    let (old, started) = watcher(&bus, Some(hold.clone()));
    let gate = Gate::new(1).unwrap();
    let (scope, control, startup) = badge::Scope::new(gate.admit(Role::Badges).unwrap()).unwrap();
    let (sent, incoming) = mpsc::channel();
    let native = start_with_badge(
        Rgb::new(255, 255, 255),
        Arc::new(move |a, _| {
            let _ = sent.send(a);
        }),
        Some(&bus.address),
        Some(scope.clone()),
    )
    .unwrap();
    scope.ready();
    startup.complete();
    let name = started.recv_timeout(Duration::from_secs(5)).unwrap();
    activate(&bus, &native, &incoming);
    control.close().unwrap();
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(5)),
        Observation::Idle,
        "pending registration is UI-only and captures no badge scope"
    );
    assert!(!control.snapshot().unwrap().pinned);
    old.release_name(WATCHER).unwrap();
    let (_replacement, replaced) = watcher(&bus, None);
    // The replacement owner is now selected by the bus before the old
    // in-flight completion arrives. It must still receive this existing item.
    hold.release();
    assert_eq!(replaced.recv_timeout(Duration::from_secs(5)).unwrap(), name);
    activate(&bus, &native, &incoming);
}
