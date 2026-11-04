//! Real Block_copy/call/destruction with original accounted continuations.
//! The fake Center never invokes UNUserNotificationCenter or notification APIs.
use super::*;
use crate::notifier::{Control, Hop, Notifier, Work};
use objc2::rc::autoreleasepool;
use std::{
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use zerus_client_core::attention::Notice;
use zerus_client_core::desktop::drain::{Closed, Gate, Observation, Role};

#[derive(Default)]
struct Calls {
    authorization: Option<Done<(bool, Option<String>)>>,
    add: Option<Done<Option<String>>>,
    authorizations: usize,
    adds: usize,
    removes: usize,
}
#[derive(Clone, Default)]
struct FakeCenter(Arc<Mutex<Calls>>);
impl Center for FakeCenter {
    fn authorize(&self, done: Done<(bool, Option<String>)>) {
        let mut calls = self.0.lock().unwrap();
        calls.authorizations += 1;
        calls.authorization = Some(done);
    }
    fn add(&self, _: &str, _: &str, _: &str, done: Done<Option<String>>) {
        let mut calls = self.0.lock().unwrap();
        calls.adds += 1;
        calls.add = Some(done);
    }
    fn remove(&self, _: &str) {
        self.0.lock().unwrap().removes += 1;
    }
}
impl FakeCenter {
    fn authorization(&self) -> Done<(bool, Option<String>)> {
        self.0.lock().unwrap().authorization.take().unwrap()
    }
    fn add(&self) -> Done<Option<String>> {
        self.0.lock().unwrap().add.take().unwrap()
    }
    fn counts(&self) -> (usize, usize, usize) {
        let calls = self.0.lock().unwrap();
        (calls.authorizations, calls.adds, calls.removes)
    }
}

/// Only one original callback/Hop may be outstanding in this finite fixture.
#[derive(Clone, Default)]
struct Queue(Arc<Mutex<Option<Work>>>, Arc<AtomicUsize>);
impl Queue {
    fn hop(&self) -> Hop {
        let queue = self.clone();
        Arc::new(move |work| {
            queue.1.fetch_add(1, Ordering::SeqCst);
            *queue.0.lock().unwrap() = Some(work);
        })
    }
    fn run(&self) {
        let work = self.0.lock().unwrap().take().unwrap();
        work();
    }
    fn discard(&self) {
        self.0.lock().unwrap().take();
    }
    fn count(&self) -> usize {
        self.1.load(Ordering::SeqCst)
    }
}
struct Fixture {
    center: FakeCenter,
    queue: Queue,
    notifier: Notifier,
    failed: Arc<Mutex<Option<(String, String)>>>,
    failures: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(hop: Option<Hop>) -> (Self, Gate, Control) {
        let center = FakeCenter::default();
        let queue = Queue::default();
        let failed = Arc::new(Mutex::new(None));
        let failures = Arc::new(AtomicUsize::new(0));
        let sink = failed.clone();
        let count = failures.clone();
        let gate = Gate::new(1).unwrap();
        let (notifier, control) = Notifier::new_accounted(
            Box::new(center.clone()),
            hop.unwrap_or_else(|| queue.hop()),
            Box::new(move |token, reason| {
                count.fetch_add(1, Ordering::SeqCst);
                *sink.lock().unwrap() = Some((token, reason));
            }),
            gate.admit(Role::NativePosting).unwrap(),
        )
        .unwrap();
        let fixture = Self {
            center,
            queue,
            notifier,
            failed,
            failures,
        };
        fixture.notifier.deliver(
            &[],
            &[Notice {
                token: "completion-fixture".into(),
                title: "Synthetic".into(),
                body: "Process-local callback custody".into(),
            }],
        );
        assert_eq!(fixture.center.counts(), (1, 0, 0));
        (fixture, gate, control)
    }
    fn grant(&self) {
        let reply = authorization_reply(self.center.authorization());
        reply.call((Bool::YES, ptr::null_mut()));
        self.queue.run();
        assert_eq!(self.center.counts(), (1, 1, 0));
    }
}
fn pending(closed: &Closed) {
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(closed.snapshot().unwrap().count(Role::NativePosting), 1);
}
fn idle(closed: &Closed) {
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(2)),
        Observation::Idle
    );
}
fn error() -> Retained<NSError> {
    // SAFETY: no userInfo; this process-local Foundation error invokes no SDK
    // notification, permission, GUI or service method.
    unsafe {
        NSError::errorWithDomain_code_userInfo(ns_string!("org.example.zerus.completion"), 7, None)
    }
}

#[test]
fn authorization_copies_consume_once_and_wait_for_the_delayed_original_hop() {
    autoreleasepool(|_| {
        let (f, gate, control) = Fixture::new(None);
        let reply = authorization_reply(f.center.authorization());
        let first = reply.copy();
        let copy = first.copy();
        drop(reply);
        drop(first);
        control.close().unwrap();
        let closed = gate.close();
        pending(&closed);
        copy.call((Bool::YES, ptr::null_mut()));
        copy.call((Bool::NO, ptr::null_mut()));
        assert_eq!(f.queue.count(), 1);
        pending(&closed);
        f.queue.run();
        idle(&closed); // Consumed copies contain no remaining continuation.
        copy.call((Bool::YES, ptr::null_mut()));
        assert_eq!(f.queue.count(), 1);
        assert_eq!(f.center.counts(), (1, 0, 0));
        assert_eq!(f.failures.load(Ordering::SeqCst), 0);
        assert!(!control.snapshot().unwrap().pinned);
    });
}

#[test]
fn add_error_copies_keep_the_original_hop_and_strong_close_refuses_late_effects() {
    autoreleasepool(|_| {
        let (f, gate, control) = Fixture::new(None);
        f.grant();
        let reply = error_reply(f.center.add());
        let copy = reply.copy();
        drop(reply);
        control.close().unwrap();
        let closed = gate.close();
        pending(&closed);
        let error = error();
        copy.call((Retained::as_ptr(&error).cast_mut(),));
        copy.call((ptr::null_mut(),));
        assert_eq!(f.queue.count(), 2); // One authorization, one add result.
        pending(&closed);
        f.queue.run();
        idle(&closed);
        assert_eq!(f.center.counts(), (1, 1, 0));
        assert_eq!(f.failures.load(Ordering::SeqCst), 0);
        assert!(!control.snapshot().unwrap().pinned);
    });
}

#[test]
fn real_error_description_and_denied_boolean_reach_the_original_sink_only_once() {
    autoreleasepool(|_| {
        for add in [false, true] {
            let (f, gate, control) = Fixture::new(None);
            let error = error();
            let expected = error.localizedDescription().to_string();
            if add {
                f.grant();
                let reply = error_reply(f.center.add());
                reply.copy().call((Retained::as_ptr(&error).cast_mut(),));
                reply.call((ptr::null_mut(),));
            } else {
                let reply = authorization_reply(f.center.authorization());
                reply
                    .copy()
                    .call((Bool::NO, Retained::as_ptr(&error).cast_mut()));
                reply.call((Bool::YES, ptr::null_mut()));
            }
            f.queue.run();
            assert_eq!(
                *f.failed.lock().unwrap(),
                Some(("completion-fixture".into(), expected))
            );
            assert_eq!(f.failures.load(Ordering::SeqCst), 1);
            assert_eq!(f.center.counts(), (1, usize::from(add), 0));
            control.close().unwrap();
            idle(&gate.close());
        }
    });
}

#[test]
fn final_uncalled_authorization_block_destruction_pins_the_same_original_group() {
    autoreleasepool(|_| {
        let (f, gate, control) = Fixture::new(None);
        let reply = authorization_reply(f.center.authorization());
        let copy = reply.copy();
        control.close().unwrap();
        let closed = gate.close();
        drop(reply);
        assert!(!control.snapshot().unwrap().pinned);
        pending(&closed);
        drop(copy);
        assert!(control.snapshot().unwrap().pinned);
        drop(f);
        drop(control);
        pending(&closed);
    });
}

#[test]
fn final_uncalled_add_block_destruction_pins_the_same_original_group() {
    autoreleasepool(|_| {
        let (f, gate, control) = Fixture::new(None);
        f.grant();
        let reply = error_reply(f.center.add());
        let copy = reply.copy();
        control.close().unwrap();
        let closed = gate.close();
        drop(reply);
        assert!(!control.snapshot().unwrap().pinned);
        pending(&closed);
        drop(copy);
        assert!(control.snapshot().unwrap().pinned);
        drop(f);
        drop(control);
        pending(&closed);
    });
}

#[test]
fn consumed_block_cannot_recover_a_discarded_original_hop() {
    autoreleasepool(|_| {
        let (f, gate, control) = Fixture::new(None);
        let reply = authorization_reply(f.center.authorization());
        reply.call((Bool::NO, ptr::null_mut()));
        control.close().unwrap();
        let closed = gate.close();
        f.queue.discard();
        assert!(control.snapshot().unwrap().pinned);
        reply.copy().call((Bool::YES, ptr::null_mut()));
        assert_eq!(f.queue.count(), 1);
        drop(reply);
        drop(f);
        drop(control);
        pending(&closed);
    });
}

#[test]
fn actual_block_call_stays_charged_after_synchronous_hop_until_callback_return() {
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let resume = release.clone();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout = timed_out.clone();
    let hop = Arc::new(move |work: Work| {
        work();
        let _ = entered.send(());
        let (lock, ready) = &*resume;
        let result = ready
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                !*released
            })
            .unwrap();
        // Never unwind across a native block boundary on a deadline.
        timeout.store(result.1.timed_out(), Ordering::SeqCst);
    });
    let (f, gate, control) = Fixture::new(Some(hop));
    let done = f.center.authorization(); // Done is Send; RcBlock is not moved.
    let worker = std::thread::spawn(move || {
        autoreleasepool(|_| {
            let reply = authorization_reply(done);
            reply.copy().call((Bool::NO, ptr::null_mut()));
            reply.call((Bool::YES, ptr::null_mut()));
        })
    });
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(f.failures.load(Ordering::SeqCst), 1);
    control.close().unwrap();
    let closed = gate.close();
    pending(&closed);
    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    worker.join().unwrap();
    assert!(!timed_out.load(Ordering::SeqCst));
    assert_eq!(f.failures.load(Ordering::SeqCst), 1);
    assert_eq!(f.center.counts(), (1, 0, 0));
    assert!(!control.snapshot().unwrap().pinned);
    idle(&closed);
}
