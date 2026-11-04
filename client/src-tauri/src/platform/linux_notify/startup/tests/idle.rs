use super::*;
use std::sync::atomic::{AtomicIsize, AtomicUsize};
use std::sync::OnceLock;

#[derive(Default)]
struct Trace {
    live_ticks: AtomicIsize,
    ticks: AtomicUsize,
    tick_drops: AtomicUsize,
    parks: AtomicUsize,
    returns: AtomicUsize,
    signals: AtomicUsize,
    bad_park: AtomicBool,
    clean_reader_entry: AtomicBool,
}
impl Trace {
    fn at(&self, at: Hook) {
        match at {
            Hook::TickCreated => {
                self.live_ticks.fetch_add(1, Ordering::AcqRel);
                self.ticks.fetch_add(1, Ordering::AcqRel);
            }
            Hook::TickDropped => {
                self.live_ticks.fetch_sub(1, Ordering::AcqRel);
                self.tick_drops.fetch_add(1, Ordering::AcqRel);
            }
            Hook::BeforePark => {
                self.parks.fetch_add(1, Ordering::AcqRel);
                if self.live_ticks.load(Ordering::Acquire) != 1 {
                    self.bad_park.store(true, Ordering::Release);
                }
            }
            Hook::AfterPark => {
                self.returns.fetch_add(1, Ordering::AcqRel);
            }
            Hook::ProbeSignal => {
                self.signals.fetch_add(1, Ordering::AcqRel);
            }
            Hook::DropReaders => self.clean_reader_entry.store(
                self.live_ticks.load(Ordering::Acquire) == 0,
                Ordering::Release,
            ),
            _ => (),
        }
    }
    fn ended(&self) {
        assert_eq!(self.live_ticks.load(Ordering::Acquire), 0);
        assert_eq!(
            self.ticks.load(Ordering::Acquire),
            self.tick_drops.load(Ordering::Acquire)
        );
        assert!(!self.bad_park.load(Ordering::Acquire));
    }
}

#[test]
fn pending_tick_registration_wakes_the_original_executor_and_ends_before_readers() {
    const CASE: &str = "platform::linux_notify::startup::tests::idle::pending_tick_registration_wakes_the_original_executor_and_ends_before_readers";
    if !private_process_case(CASE) {
        return;
    }
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let trace = Arc::new(Trace::default());
    let events = trace.clone();
    let runnable = Arc::new(worker::RunnableProbe::default());
    let capture = Arc::new(());
    let weak = Arc::downgrade(&capture);
    let thread = Arc::new(OnceLock::new());
    let recorded = thread.clone();
    let hooks = Hooks {
        runnable: Some(runnable.clone()),
        event: Some(Arc::new(move |at| {
            let _owned_capture = &capture;
            events.at(at);
            if at == Hook::BeforePark {
                recorded.get_or_init(|| {
                    // Record the original worker from INSIDE its hook, in this
                    // exact private child. A reused TID cannot pass the birth check.
                    let relative = std::fs::read_link("/proc/thread-self").unwrap();
                    assert!(relative.starts_with(std::process::id().to_string()));
                    let path = std::path::Path::new("/proc").join(relative);
                    let birth = thread_birth(&path).unwrap();
                    (path, birth)
                });
            }
        })),
        ..Hooks::default()
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    // Positive kernel wait observation supplements the actual recv hooks. No
    // elapsed sleep, CPU percentage or missing hook alone proves an idle wait.
    // There are no holds/locks in this callback after the first identity read.
    wait(|| {
        let Some((path, birth)) = thread.get() else {
            return false;
        };
        !startup.worker.as_ref().unwrap().is_finished()
            && thread_birth(path).as_ref() == Some(birth)
            && trace.parks.load(Ordering::Acquire) > trace.returns.load(Ordering::Acquire)
            && std::fs::read_to_string(path.join("wchan"))
                .is_ok_and(|name| name.trim().contains("futex"))
    });
    assert!(runnable.pending.load(Ordering::Acquire));
    assert!(!runnable.finished.load(Ordering::Acquire));
    assert_eq!(trace.live_ticks.load(Ordering::Acquire), 1);
    let returns = trace.returns.load(Ordering::Acquire);
    // This wakes an actual task on THIS connection's executor after its tick
    // registered Pending. No bus traffic or second wake rescues a lost sleeper.
    runnable.wake();
    wait(|| runnable.finished.load(Ordering::Acquire));
    wait(|| runnable.dropped.load(Ordering::Acquire));
    assert!(trace.returns.load(Ordering::Acquire) > returns);
    control.close().unwrap();
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    finish(&mut startup);
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    trace.ended();
    assert!(trace.clean_reader_entry.load(Ordering::Acquire));
    assert!(weak.upgrade().is_none());
    println!("IDLE EXECUTOR: recorded original worker wait, actual runnable wake, exact join and capture destruction");
}

fn thread_birth(path: &std::path::Path) -> Option<String> {
    let stat = std::fs::read_to_string(path.join("stat")).ok()?;
    Some(stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.into())
}

#[test]
fn one_addressed_burst_above_raw16_drains_both_readers_without_a_second_burst() {
    let bus = Bus::new();
    let peer = bus.connect();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let trace = Arc::new(Trace::default());
    let events = trace.clone();
    let hooks = Hooks {
        event: Some(Arc::new(move |at| {
            events.at(at);
            if at == Hook::Raw {
                held.enter();
            }
        })),
        ..Hooks::default()
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    let destination = startup.scope.state.lock().unwrap().peer.clone().unwrap();
    for _ in 0..64 {
        peer.emit_signal(
            Some(destination.as_str()),
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "IdleWakeProbe",
            &(),
        )
        .unwrap();
    }
    zbus::blocking::fdo::DBusProxy::new(&peer)
        .unwrap()
        .get_id()
        .unwrap();
    let _ = startup.scope.wake.try_send(());
    assert!(matches!(
        startup.scope.wake.try_send(()),
        Err(mpsc::TrySendError::Full(()))
    ));
    hold.release();
    // Header-only test instrumentation counts the exact synthetic member on
    // each reader. Bodies remain unread; no original, ID or mapping is made.
    wait(|| trace.signals.load(Ordering::Acquire) == 128);
    control.close().unwrap();
    finish(&mut startup);
    assert_eq!(trace.signals.load(Ordering::Acquire), 128);
    trace.ended();
    assert!(trace.clean_reader_entry.load(Ordering::Acquire));
}

#[test]
fn close_wakes_idle_receive_and_a_full_poll_to_park_gap_without_renewal() {
    for held_gap in [false, true] {
        let bus = Bus::new();
        let gate = Gate::new(1).unwrap();
        let hold = Arc::new(Hold::default());
        let held = hold.clone();
        let trace = Arc::new(Trace::default());
        let events = trace.clone();
        let (entered, ready) = mpsc::sync_channel(1);
        let once = AtomicBool::new(true);
        let hooks = Hooks {
            event: Some(Arc::new(move |at| {
                events.at(at);
                if at == Hook::BeforePark && once.swap(false, Ordering::AcqRel) {
                    entered.try_send(()).unwrap();
                    if held_gap {
                        held.enter();
                    }
                }
            })),
            ..Hooks::default()
        };
        let (mut startup, control) = spawn(
            bus.address.clone(),
            gate.admit(Role::NativePosting).unwrap(),
            hooks,
        )
        .unwrap();
        ready.recv_timeout(Duration::from_secs(8)).unwrap();
        if held_gap {
            wait(|| hold.entered.load(Ordering::Acquire));
            let _ = startup.scope.wake.try_send(());
            assert!(matches!(
                startup.scope.wake.try_send(()),
                Err(mpsc::TrySendError::Full(()))
            ));
        }
        control.close().unwrap();
        assert!(!control.snapshot().unwrap().ready);
        let deadline = startup.scope.state.lock().unwrap().cleanup.unwrap();
        for _ in 0..32 {
            control.close().unwrap();
        }
        assert_eq!(startup.scope.state.lock().unwrap().cleanup, Some(deadline));
        let closed = gate.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        if held_gap {
            assert_eq!(startup.try_finish().unwrap(), StartupProgress::Pending);
            hold.release();
        }
        finish(&mut startup);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        assert!(trace.returns.load(Ordering::Acquire) > 0);
        trace.ended();
        assert!(trace.clean_reader_entry.load(Ordering::Acquire));
    }
}

#[test]
fn idle_hook_panic_destroys_actual_tick_storage_and_pins_original_completion() {
    let bus = Bus::new();
    let gate = Gate::new(1).unwrap();
    let trace = Arc::new(Trace::default());
    let events = trace.clone();
    let hooks = Hooks {
        event: Some(Arc::new(move |at| {
            events.at(at);
            if at == Hook::BeforePark {
                panic!("private idle park panic");
            }
        })),
        ..Hooks::default()
    };
    let (mut startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
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
    trace.ended();
    assert!(!trace.clean_reader_entry.load(Ordering::Acquire));
}

#[test]
fn losing_all_public_handles_at_idle_wakes_teardown_but_never_invents_a_join() {
    let bus = Bus::new();
    let observer = bus.connect();
    let proxy = zbus::blocking::fdo::DBusProxy::new(&observer).unwrap();
    let gate = Gate::new(1).unwrap();
    let hold = Arc::new(Hold::default());
    let held = hold.clone();
    let capture = Arc::new(());
    let weak = Arc::downgrade(&capture);
    let trace = Arc::new(Trace::default());
    let events = trace.clone();
    let once = AtomicBool::new(true);
    let hooks = Hooks {
        event: Some(Arc::new(move |at| {
            let _owned = &capture;
            events.at(at);
            if at == Hook::BeforePark && once.swap(false, Ordering::AcqRel) {
                held.enter();
            }
        })),
        ..Hooks::default()
    };
    let (startup, control) = spawn(
        bus.address.clone(),
        gate.admit(Role::NativePosting).unwrap(),
        hooks,
    )
    .unwrap();
    wait(|| hold.entered.load(Ordering::Acquire));
    let name = startup.scope.state.lock().unwrap().peer.clone().unwrap();
    drop(control);
    drop(startup);
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    hold.release();
    wait(|| weak.upgrade().is_none());
    wait(|| {
        !proxy
            .name_has_owner(name.as_str().try_into().unwrap())
            .unwrap()
    });
    trace.ended();
    assert!(trace.clean_reader_entry.load(Ordering::Acquire));
    // Peer and callback disappearance never substitute for the lost exact join.
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
}
