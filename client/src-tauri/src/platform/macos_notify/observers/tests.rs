use super::*;
use objc2::rc::autoreleasepool;
use objc2_foundation::ns_string;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use zerus_client_core::desktop::drain::{Gate, Observation};

fn names() -> [&'static NSString; 2] {
    [
        ns_string!("org.example.zerus.capture.active"),
        ns_string!("org.example.zerus.capture.inactive"),
    ]
}
fn post(center: &NSNotificationCenter, index: usize) {
    // SAFETY: a process-local test name, no object or userInfo, synchronous.
    unsafe { center.postNotificationName_object(names()[index], None) };
}
fn invoke(block: &Callback) {
    // SAFETY: no object filter; this real retained notification stays alive
    // through the native block invocation. The adapter does not inspect it.
    let note = unsafe { NSNotification::notificationWithName_object(names()[0], None) };
    block.call((NonNull::from(&*note),));
}
fn adapter(
    center: Retained<NSNotificationCenter>,
    gate: &Gate,
    calls: Arc<AtomicUsize>,
) -> ActivityObservers {
    let inactive = calls.clone();
    ActivityObservers::at(
        center,
        names(),
        gate.admit(Role::Attention).unwrap(),
        [
            Box::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(move || {
                inactive.fetch_add(1, Ordering::SeqCst);
            }),
        ],
    )
    .unwrap()
}
fn idle(closed: &Closed) {
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(2)),
        Observation::Idle
    );
}

#[test]
fn actual_registration_post_and_removal_end_only_after_native_capture_destruction() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (host, captures) = autoreleasepool(|_| {
        let center = NSNotificationCenter::new();
        let gate = Gate::new(1).unwrap();
        let mut owned = adapter(center.clone(), &gate, calls.clone());
        post(&center, 0);
        post(&center, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let host = gate.close();
        let captures = owned.close().unwrap();
        // Repeated close neither registers nor removes a replacement observer.
        let repeated = owned.close().unwrap();
        post(&center, 0);
        post(&center, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        drop(owned);
        drop(repeated);
        (host, captures)
    });
    idle(&captures);
    idle(&host);
}

#[test]
fn actual_block_copies_outlive_removal_and_factory_and_refuse_late_actions() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (host, captures, copy) = autoreleasepool(|_| {
        let center = NSNotificationCenter::new();
        let gate = Gate::new(1).unwrap();
        let mut owned = adapter(center, &gate, calls.clone());
        let first = owned.entries[0].as_ref().unwrap().block.copy();
        let copy = first.copy(); // Both invoke the real Block_copy path.
        drop(first);
        invoke(&copy);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let captures = owned.close().unwrap();
        let host = gate.close();
        drop(owned); // Includes the exact original factory and both tokens.
        (host, captures, copy)
    });
    assert_eq!(captures.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
    autoreleasepool(|_| invoke(&copy));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(copy);
    idle(&captures);
    idle(&host);
}

#[test]
fn dropping_an_open_adapter_fences_late_calls_and_pins_the_original_group() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (host, copy) = autoreleasepool(|_| {
        let center = NSNotificationCenter::new();
        let gate = Gate::new(1).unwrap();
        let owned = adapter(center, &gate, calls.clone());
        let copy = owned.entries[0].as_ref().unwrap().block.copy();
        let host = gate.close();
        drop(owned); // No explicit close: owner Completion MUST stay pinned.
        (host, copy)
    });
    autoreleasepool(|_| invoke(&copy));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(copy);
    // Even final verified block destruction cannot complete an abandoned owner.
    assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
}

#[test]
fn a_real_inflight_callback_remains_charged_through_its_actual_return() {
    let (host, captures) = autoreleasepool(|_| {
        let center = NSNotificationCenter::new();
        let gate = Gate::new(1).unwrap();
        let (entered, observed) = std::sync::mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let resume = release.clone();
        let timed_out = Arc::new(AtomicBool::new(false));
        let timeout = timed_out.clone();
        let mut owned = ActivityObservers::at(
            center.clone(),
            names(),
            gate.admit(Role::Attention).unwrap(),
            [
                Box::new(move || {
                    let _ = entered.send(());
                    let (lock, ready) = &*resume;
                    let result = ready
                        .wait_timeout_while(
                            lock.lock().unwrap(),
                            Duration::from_secs(5),
                            |released| !*released,
                        )
                        .unwrap();
                    // Never panic across the native block boundary on timeout.
                    timeout.store(result.1.timed_out(), Ordering::SeqCst);
                }),
                Box::new(|| {}),
            ],
        )
        .unwrap();
        let worker_center = center.clone();
        let worker = std::thread::spawn(move || autoreleasepool(|_| post(&worker_center, 0)));
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let host = gate.close();
        let captures = owned.close().unwrap();
        drop(owned);
        assert_eq!(captures.wait_until(Instant::now()), Observation::Pending);
        assert_eq!(host.wait_until(Instant::now()), Observation::Pending);
        let (lock, ready) = &*release;
        *lock.lock().unwrap() = true;
        ready.notify_all();
        worker.join().unwrap();
        assert!(!timed_out.load(Ordering::SeqCst));
        (host, captures)
    });
    idle(&captures);
    idle(&host);
}
