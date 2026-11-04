use super::*;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use zerus_client_core::desktop::drain::Observation;

fn scope() -> (Gate, Observers, Control) {
    let gate = Gate::new(1).unwrap();
    let (observers, control) = Observers::new(gate.admit(Role::Attention).unwrap()).unwrap();
    (gate, observers, control)
}

#[cfg(unix)]
#[test]
fn native_copies_keep_capture_charged_until_the_actual_last_destruction() {
    let (gate, observers, control) = scope();
    let calls = Arc::new(AtomicUsize::new(0));
    let action = calls.clone();
    let capture = Arc::new(
        observers
            .wrap(move || {
                action.fetch_add(1, Ordering::SeqCst);
            })
            .ok()
            .unwrap(),
    );
    let native_copy = capture.clone();
    capture.call().unwrap();
    let captures = control.close().unwrap();
    let host = gate.close();
    assert_eq!(native_copy.call(), Err(Refused::Closed));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(capture);
    drop(observers);
    assert_eq!(captures.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
    drop(native_copy);
    assert_eq!(captures.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(host.wait_until(Instant::now()), Observation::Idle);
    assert!(matches!(control.close(), Err(Refused::Unavailable)));
}

#[cfg(unix)]
#[test]
fn close_does_not_release_a_callback_that_has_not_actually_returned() {
    let (gate, observers, control) = scope();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let resume_rx = std::sync::Mutex::new(resume_rx);
    let capture = Arc::new(
        observers
            .wrap(move || {
                entered_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
            })
            .ok()
            .unwrap(),
    );
    let native_copy = capture.clone();
    let worker = std::thread::spawn(move || native_copy.call());
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let captures = control.close().unwrap();
    let host = gate.close();
    drop(capture);
    assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(captures.wait_until(Instant::now()), Observation::Pending);
    resume_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), Ok(()));
    assert_eq!(captures.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(host.wait_until(Instant::now()), Observation::Idle);
}

#[cfg(unix)]
struct DataDrop {
    host: Gate,
    captures: Gate,
    dropped: Arc<AtomicUsize>,
    panic: bool,
}
#[cfg(unix)]
impl Drop for DataDrop {
    fn drop(&mut self) {
        assert_eq!(self.host.snapshot().unwrap().count(Role::Attention), 1);
        assert_eq!(self.captures.snapshot().unwrap().admitted, 1);
        self.dropped.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic, "synthetic callback data drop failure");
    }
}
#[cfg(unix)]
fn data_capture(
    observers: &Observers,
    host: &Gate,
    dropped: Arc<AtomicUsize>,
    panic: bool,
) -> Observed<impl Fn() + Send + Sync> {
    let data = DataDrop {
        host: host.clone(),
        captures: observers.0.captures.clone(),
        dropped,
        panic,
    };
    observers
        .wrap(move || {
            std::hint::black_box(&data);
        })
        .ok()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn callback_data_is_destroyed_before_the_last_charge_and_slot() {
    let (gate, observers, control) = scope();
    let dropped = Arc::new(AtomicUsize::new(0));
    let capture = data_capture(&observers, &gate, dropped.clone(), false);
    let captures = control.close().unwrap();
    let host = gate.close();
    drop(capture);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(captures.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(host.wait_until(Instant::now()), Observation::Idle);
}

#[cfg(unix)]
#[test]
fn panicking_callback_data_pins_instead_of_certifying_destruction() {
    let (gate, observers, control) = scope();
    let dropped = Arc::new(AtomicUsize::new(0));
    let capture = data_capture(&observers, &gate, dropped.clone(), true);
    control.close().unwrap();
    let host = gate.close();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(capture))).is_err());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(observers.0.native.snapshot().unwrap().pinned);
    drop(observers);
    assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
}

#[cfg(unix)]
#[test]
fn callback_panic_pins_after_close_and_native_capture_destruction() {
    let (gate, observers, control) = scope();
    let capture = observers
        .wrap(|| panic!("synthetic observer callback failure"))
        .ok()
        .unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| capture.call())).is_err());
    control.close().unwrap();
    drop(capture);
    assert!(observers.0.native.snapshot().unwrap().pinned);
    drop(observers);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn abandonment_pins_one_fixed_scope_even_after_every_handle_drops() {
    let (gate, observers, control) = scope();
    for _ in 0..32 {
        drop(observers.wrap(|| {}).ok().unwrap());
    }
    assert!(observers.0.native.snapshot().unwrap().pinned);
    drop(observers);
    drop(control);
    assert_eq!(gate.snapshot().unwrap().count(Role::Attention), 1);
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[test]
fn two_fixed_captures_and_exact_role_admission() {
    for role in Role::ALL {
        let gate = Gate::new(1).unwrap();
        let result = Observers::new(gate.admit(role).unwrap());
        assert_eq!(result.is_ok(), role == Role::Attention);
        drop(result);
        assert_eq!(gate.snapshot().unwrap().admitted, 0);
    }
    let (_, observers, _) = scope();
    let first = observers.wrap(|| {}).ok().unwrap();
    let second = observers.wrap(|| {}).ok().unwrap();
    assert!(matches!(observers.wrap(|| {}), Err((Refused::Pressure, _))));
    drop((first, second));
}

#[cfg(unix)]
#[test]
fn repeated_close_is_sticky_and_an_idle_control_is_weak() {
    let (gate, observers, control) = scope();
    control.close().unwrap();
    control.close().unwrap();
    assert!(matches!(observers.wrap(|| {}), Err((Refused::Closed, _))));
    assert_eq!(gate.close().wait_until(Instant::now()), Observation::Idle);
    drop(observers);
    assert!(matches!(control.close(), Err(Refused::Unavailable)));
}

#[cfg(not(unix))]
#[test]
fn unsupported_close_refuses_before_mutating_either_admission() {
    let (_, observers, control) = scope();
    assert!(matches!(control.close(), Err(Refused::Unsupported)));
    let capture = observers.wrap(|| {}).ok().unwrap();
    assert_eq!(capture.call(), Ok(()));
    assert!(!observers.0.captures.snapshot().unwrap().closed);
    assert!(!observers.0.native.snapshot().unwrap().closed);
}
