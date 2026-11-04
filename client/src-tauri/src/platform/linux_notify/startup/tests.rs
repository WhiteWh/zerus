use super::*;
mod byte;
mod idle;
use crate::platform::linux_test_bus::Bus;
use std::pin::Pin;
use std::sync::Condvar;
use std::task::{Context, Poll, Waker};
use zbus::export::futures_core::Stream;
use zerus_client_core::desktop::drain::{Gate, Observation};

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < end, "startup condition did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn finish(startup: &mut OwnedStartup) {
    wait(|| startup.try_finish().unwrap() == StartupProgress::Finished);
}
#[derive(Default)]
struct Hold {
    entered: AtomicBool,
    released: Mutex<bool>,
    wake: Condvar,
}
impl Hold {
    fn enter(&self) {
        self.entered.store(true, Ordering::Release);
        let mut released = self.released.lock().unwrap();
        while !*released {
            let (next, timeout) = self
                .wake
                .wait_timeout(released, Duration::from_secs(4))
                .unwrap();
            assert!(!timeout.timed_out(), "private hold was not released");
            released = next;
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

#[test]
fn explicit_role_address_and_context_refuse_before_any_worker() {
    for role in Role::ALL {
        if role == Role::NativePosting {
            continue;
        }
        let gate = Gate::new(1).unwrap();
        assert_eq!(
            spawn_owned_startup("unix:path=/not-contacted".into(), gate.admit(role).unwrap())
                .unwrap_err(),
            StartupRefused::Role
        );
        assert_eq!(gate.snapshot().unwrap().admitted, 0);
    }
    for address in [
        "",
        "autolaunch:",
        "unix:path=/private;autolaunch:",
        "unix:path=/private\n",
    ] {
        let gate = Gate::new(1).unwrap();
        assert_eq!(
            spawn_owned_startup(address.into(), gate.admit(Role::NativePosting).unwrap())
                .unwrap_err(),
            StartupRefused::Input
        );
        assert_eq!(gate.snapshot().unwrap().admitted, 0);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let gate = Gate::new(1).unwrap();
    assert_eq!(
        spawn_owned_startup(
            "unix:path=/not-contacted".into(),
            gate.admit(Role::NativePosting).unwrap()
        )
        .unwrap_err(),
        StartupRefused::Context
    );
    assert_eq!(gate.snapshot().unwrap().admitted, 0);
}

#[test]
fn actual_startup_readers_and_exact_join_finish_before_weak_control_disappears() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let (mut startup, control) = spawn_owned_startup(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    wait(|| control.snapshot().unwrap().ready);
    assert_eq!(startup.try_finish().unwrap(), StartupProgress::Pending);
    control.close().unwrap();
    assert!(!control.snapshot().unwrap().ready);
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    finish(&mut startup);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(startup.try_finish().unwrap(), StartupProgress::Finished);
    assert!(!control.close().unwrap().pinned);
    drop(startup);
    assert_eq!(control.snapshot().unwrap_err(), StartupRefused::Unavailable);
}

#[test]
fn held_real_receiver_destruction_and_repeated_close_keep_original_deadline_and_charge() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let hooks = Hooks {
        budget: None,
        runnable: None,
        event: Some(Arc::new(move |at| {
            if matches!(at, Hook::DropReaders) {
                held.enter();
            }
        })),
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| control.snapshot().unwrap().ready);
    control.close().unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    let deadline = startup.scope.state.lock().unwrap().cleanup.unwrap();
    for _ in 0..10 {
        control.close().unwrap();
    }
    assert_eq!(startup.scope.state.lock().unwrap().cleanup, Some(deadline));
    let closed = gate.close();
    assert_eq!(startup.try_finish().unwrap(), StartupProgress::Pending);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    hold.release();
    finish(&mut startup);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
}

#[test]
fn all_public_handle_loss_during_setup_pins_instead_of_inventing_completion() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let hooks = Hooks {
        budget: None,
        runnable: None,
        event: Some(Arc::new(move |at| {
            if matches!(at, Hook::BeforeNative) {
                held.enter();
            }
        })),
    };
    let (startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    drop(control);
    drop(startup);
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    hold.release();
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_millis(50)),
        Observation::Pending
    );
}

#[test]
fn both_match_acknowledgements_precede_shared16_raw_activation_under_addressed_backpressure() {
    let bus = Bus::new();
    let peer = bus.connect();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let order = Arc::new(Mutex::new(Vec::new()));
    let trace = order.clone();
    let hooks = Hooks {
        budget: None,
        runnable: None,
        event: Some(Arc::new(move |at| {
            let mut trace = trace.lock().unwrap();
            trace.push(at);
            let first_match =
                at == Hook::Matched && trace.iter().filter(|x| **x == Hook::Matched).count() == 1;
            drop(trace);
            if first_match {
                held.enter();
            }
        })),
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    let destination = startup.scope.state.lock().unwrap().peer.clone().unwrap();
    for n in 0..64u32 {
        peer.emit_signal(
            Some(destination.as_str()),
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(n, "default"),
        )
        .unwrap();
    }
    // A sender-side bus barrier, not a timing guess about addressed messages.
    zbus::blocking::fdo::DBusProxy::new(&peer)
        .unwrap()
        .get_id()
        .unwrap();
    hold.release();
    wait(|| control.snapshot().unwrap().ready);
    let trace = order.lock().unwrap();
    let raw = trace.iter().position(|x| *x == Hook::Raw).unwrap();
    assert_eq!(
        trace[..raw].iter().filter(|x| **x == Hook::Matched).count(),
        2
    );
    drop(trace);
    control.close().unwrap();
    finish(&mut startup);
}

fn socket_fds() -> std::collections::BTreeSet<String> {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|e| {
            let value = std::fs::read_link(e.ok()?.path())
                .ok()?
                .to_string_lossy()
                .into_owned();
            value.starts_with("socket:[").then_some(value)
        })
        .collect()
}
fn reactor_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("comm")).ok())
        .filter(|name| name.trim() == "async-io")
        .count()
}

fn zbus_drivers() -> std::collections::BTreeSet<String> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            if !name.starts_with("zbus::Connect") {
                return None;
            }
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let birth = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
            Some(format!("{}:{birth}", entry.file_name().to_string_lossy()))
        })
        .collect()
}

// Whole-process /proc baselines belong in an exact child, isolated from every
// other test in the default parallel harness. Each child also owns its bus and
// filesystem environment. These resource-only cases need no display; clearing
// the environment prevents either child from opening an inherited display.
fn private_process_case(case: &str) -> bool {
    if std::env::var("ZERUS_STARTUP_PROCESS_CASE").as_deref() == Ok(case) {
        return true;
    }
    use std::os::unix::fs::DirBuilderExt;
    let root = std::env::temp_dir().join(format!("zsc-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let mut child = std::process::Command::new("dbus-run-session");
    child
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([case, "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("GDK_BACKEND", "x11")
        .env("WAYLAND_DISPLAY", "startup-none")
        .env("NO_AT_BRIDGE", "1")
        .env("ZERUS_STARTUP_PROCESS_CASE", case)
        .stdin(std::process::Stdio::null());
    for (key, part) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("TMPDIR", "tmp"),
        ("TMUX_TMPDIR", "tmux"),
    ] {
        let path = root.join(part);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        child.env(key, path);
    }
    let output = child.output().unwrap();
    print!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
    assert!(output.status.success(), "private process case {case}");
    false
}

#[test]
fn repeated_exact_connections_and_captures_end_while_one_shared_reactor_remains() {
    const CASE: &str = "platform::linux_notify::startup::tests::repeated_exact_connections_and_captures_end_while_one_shared_reactor_remains";
    if !private_process_case(CASE) {
        return;
    }
    let bus = Bus::new();
    let observer = bus.connect(); // Warm the one shared reactor before baselining.
    let proxy = zbus::blocking::fdo::DBusProxy::new(&observer).unwrap();
    proxy.get_id().unwrap(); // The fixture's existing driver has actually run.
    let sockets = socket_fds();
    let drivers = zbus_drivers();
    assert_eq!(reactor_count(), 1);
    for _ in 0..16 {
        let gate = Gate::new(1).unwrap();
        let capture = Arc::new(());
        let weak = Arc::downgrade(&capture);
        let hooks = Hooks {
            budget: None,
            runnable: None,
            event: Some(Arc::new(move |_| {
                let _keep = &capture;
            })),
        };
        let (mut startup, control) = spawn(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
            hooks,
        )
        .unwrap();
        wait(|| control.snapshot().unwrap().ready);
        assert!(zbus_drivers().is_subset(&drivers));
        let name = startup.scope.state.lock().unwrap().peer.clone().unwrap();
        assert!(proxy
            .name_has_owner(name.as_str().try_into().unwrap())
            .unwrap());
        let closed = gate.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        control.close().unwrap();
        finish(&mut startup);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        assert!(weak.upgrade().is_none());
        assert!(!proxy
            .name_has_owner(name.as_str().try_into().unwrap())
            .unwrap());
        wait(|| socket_fds() == sockets);
        assert_eq!(reactor_count(), 1);
    }
    println!("WARM STARTUP: sixteen exact startup joins, zero retained startup sockets/captures, one shared reactor and no extra zbus driver");
}

#[test]
fn no_notification_method_effects_or_body_routing_are_installed() {
    let bus = Bus::new();
    let server = bus.connect();
    server
        .request_name("org.freedesktop.Notifications")
        .unwrap();
    let mut observed = zbus::MessageStream::from(server.inner().clone());
    let gate = Gate::new(1).unwrap();
    let (mut startup, control) = spawn_owned_startup(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    wait(|| control.snapshot().unwrap().ready);
    let name = startup.scope.state.lock().unwrap().peer.clone().unwrap();
    for n in 0..64u32 {
        server
            .emit_signal(
                Some(name.as_str()),
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(n, "default"),
            )
            .unwrap();
    }
    control.close().unwrap();
    finish(&mut startup);
    while let Poll::Ready(Some(message)) =
        Pin::new(&mut observed).poll_next(&mut Context::from_waker(Waker::noop()))
    {
        let message = message.unwrap();
        assert!(!matches!(
            message.header().member().map(|m| m.as_str()),
            Some("Notify" | "CloseNotification")
        ));
    }
}

#[test]
fn close_before_native_work_waits_for_the_original_worker_join() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let hooks = Hooks {
        budget: None,
        runnable: None,
        event: Some(Arc::new(move |at| {
            if at == Hook::BeforeNative {
                held.enter();
            }
        })),
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    control.close().unwrap();
    let closed = gate.close();
    assert_eq!(startup.try_finish().unwrap(), StartupProgress::Pending);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    hold.release();
    finish(&mut startup);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    assert!(startup.scope.state.lock().unwrap().peer.is_none());
}

#[test]
fn panic_or_poison_never_releases_or_accepts_a_later_finish() {
    for poison in [false, true] {
        let bus = Bus::new();
        let gate = Gate::new(1).unwrap();
        let hooks = Hooks {
            budget: None,
            runnable: None,
            event: Some(Arc::new(move |at| {
                if !poison && at == Hook::DropReaders {
                    panic!("private reader teardown panic");
                }
            })),
        };
        let (mut startup, control) = spawn(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
            hooks,
        )
        .unwrap();
        wait(|| control.snapshot().unwrap().ready);
        if poison {
            let scope = startup.scope.clone();
            assert!(std::thread::spawn(move || {
                let _held = scope.state.lock().unwrap();
                panic!("private startup state poison");
            })
            .join()
            .is_err());
        }
        let _ = control.close();
        let closed = gate.close();
        wait(|| startup.worker.as_ref().unwrap().is_finished());
        assert_eq!(
            startup.try_finish().unwrap_err(),
            StartupRefused::Unavailable
        );
        assert_eq!(
            startup.try_finish().unwrap_err(),
            StartupRefused::Unavailable
        );
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    }
}

#[test]
fn actual_borrowed_message_and_connection_drop_hold_accounting_until_their_return() {
    for phase in [Hook::Message, Hook::DropConnection] {
        let bus = Bus::new();
        let sender = bus.connect();
        let gate = Gate::new(1).unwrap();
        let hold = Arc::new(Hold::default());
        let held = hold.clone();
        let hooks = Hooks {
            budget: None,
            runnable: None,
            event: Some(Arc::new(move |at| {
                if at == phase {
                    held.enter();
                }
            })),
        };
        let (mut startup, control) = spawn(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
            hooks,
        )
        .unwrap();
        wait(|| control.snapshot().unwrap().ready);
        if phase == Hook::Message {
            let name = startup.scope.state.lock().unwrap().peer.clone().unwrap();
            sender
                .emit_signal(
                    Some(name.as_str()),
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                    "ActionInvoked",
                    &(42u32, "default"),
                )
                .unwrap();
            wait(|| hold.entered.load(Ordering::Acquire));
        }
        control.close().unwrap();
        wait(|| hold.entered.load(Ordering::Acquire));
        let closed = gate.close();
        assert!(!control.snapshot().unwrap().ready);
        assert_eq!(startup.try_finish().unwrap(), StartupProgress::Pending);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        hold.release();
        finish(&mut startup);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    }
}

#[test]
fn original_setup_and_cleanup_deadlines_expire_without_late_success_or_renewal() {
    for phase in [Hook::BeforeNative, Hook::DropConnection] {
        let bus = Bus::new();
        let gate = Gate::new(1).unwrap();
        let hold = Arc::new(Hold::default());
        let held = hold.clone();
        let hooks = Hooks {
            budget: Some(Duration::from_millis(500)),
            runnable: None,
            event: Some(Arc::new(move |at| {
                if at == phase {
                    held.enter();
                }
            })),
        };
        let (mut startup, control) = spawn(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
            hooks,
        )
        .unwrap();
        if phase == Hook::DropConnection {
            wait(|| control.snapshot().unwrap().ready);
        }
        control.close().unwrap();
        wait(|| hold.entered.load(Ordering::Acquire));
        let deadline = startup.scope.state.lock().unwrap().cleanup.unwrap();
        std::thread::sleep(Duration::from_millis(600));
        control.close().unwrap();
        assert_eq!(startup.scope.state.lock().unwrap().cleanup, Some(deadline));
        hold.release();
        let closed = gate.close();
        wait(|| startup.worker.as_ref().unwrap().is_finished());
        assert_eq!(
            startup.try_finish().unwrap_err(),
            StartupRefused::Unavailable
        );
        assert_eq!(
            startup.try_finish().unwrap_err(),
            StartupRefused::Unavailable
        );
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        assert!(control.snapshot().unwrap().pinned);
    }
}

#[test]
fn actual_authentication_timeout_never_becomes_no_work_or_a_retry() {
    use std::os::unix::net::UnixListener;
    let root = std::env::temp_dir().join(format!("zsa-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("bus");
    let listener = UnixListener::bind(&path).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let peer = std::thread::spawn(move || {
        let (_connection, _) = listener.accept().unwrap();
        held.enter(); // Real socket accepted, no authentication reply.
    });
    let gate = Gate::new(1).unwrap();
    let hooks = Hooks {
        budget: Some(Duration::from_millis(500)),
        runnable: None,
        event: None,
    };
    let (mut startup, control) = spawn(
        format!("unix:path={}", path.display()),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    wait(|| startup.worker.as_ref().unwrap().is_finished());
    let closed = gate.close();
    assert_eq!(
        startup.try_finish().unwrap_err(),
        StartupRefused::Unavailable
    );
    assert_eq!(
        startup.try_finish().unwrap_err(),
        StartupRefused::Unavailable
    );
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    assert!(control.snapshot().unwrap().pinned);
    hold.release();
    peer.join().unwrap();
    drop(startup);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn actual_unanswered_match_removal_pins_and_close_never_renews_it() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let hooks = Hooks {
        budget: Some(Duration::from_millis(500)),
        runnable: None,
        event: None,
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| control.snapshot().unwrap().ready);
    let paused = bus.suspend(); // Only this test helper's exact child daemon.
    control.close().unwrap();
    let deadline = startup.scope.state.lock().unwrap().cleanup.unwrap();
    let closed = gate.close();
    wait(|| startup.worker.as_ref().unwrap().is_finished());
    assert_eq!(
        startup.try_finish().unwrap_err(),
        StartupRefused::Unavailable
    );
    for _ in 0..8 {
        control.close().unwrap();
    }
    assert_eq!(startup.scope.state.lock().unwrap().cleanup, Some(deadline));
    assert_eq!(
        startup.try_finish().unwrap_err(),
        StartupRefused::Unavailable
    );
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    assert!(control.snapshot().unwrap().pinned);
    drop(paused);
}

#[test]
fn cold_private_process_initializes_one_global_reactor_and_reuses_it() {
    const CASE: &str = "platform::linux_notify::startup::tests::cold_private_process_initializes_one_global_reactor_and_reuses_it";
    if !private_process_case(CASE) {
        return;
    }
    assert_eq!(reactor_count(), 0);
    let bus = Bus::new(); // Child daemon only; no parent zbus connection warms it.
    let before = socket_fds();
    for _ in 0..4 {
        let gate = Gate::new(1).unwrap();
        let (mut startup, control) = spawn_owned_startup(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
        )
        .unwrap();
        wait(|| control.snapshot().unwrap().ready);
        assert_eq!(reactor_count(), 1);
        control.close().unwrap();
        finish(&mut startup);
        assert_eq!(reactor_count(), 1);
        wait(|| socket_fds() == before);
    }
    println!("COLD STARTUP: reactor0 -> one shared reactor, four exact startup joins, zero retained startup sockets");
}
