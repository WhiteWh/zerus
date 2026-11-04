//! Synthetic native ports exercise the real worker and existing actor ledger.
use super::super::{
    callbacks::Retained,
    tests::{activation, post, session, uuid},
    wire::{Command, NativeId, Refusal},
};
use super::*;
use crate::desktop::{
    notifications::Outbox,
    store::{Location, Store},
};
use std::{sync::atomic::AtomicUsize, time::Instant};
use zerus_desktop_ledger::{self as ledger, Identity, Kind, Reserve, Target};
static SERIAL: Mutex<()> = Mutex::new(());
fn until(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < end, "worker did not settle");
        thread::sleep(Duration::from_millis(1));
    }
}
struct Fake {
    posts: Arc<AtomicUsize>,
    close: Arc<AtomicBool>,
    slot: Slot,
    release: Option<mpsc::Receiver<()>>,
    entered: Option<mpsc::SyncSender<()>>,
    close_event: Option<String>,
    callback_allowed: bool,
}
impl Native for Fake {
    fn callbacks_current(&mut self) -> bool {
        self.callback_allowed
    }
    fn prepare(&mut self, _: &Command, _: &NativeId) -> Result<(), Refusal> {
        Ok(())
    }
    fn post(&mut self, _: &Command, id: &NativeId) -> Result<NativeId, ()> {
        self.posts.fetch_add(1, Ordering::AcqRel);
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
        }
        if let Some(release) = self.release.take() {
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Ok(id.clone())
    }
    fn withdraw(&mut self, _: &NativeId) -> Result<(), ()> {
        Ok(())
    }
    fn close(&mut self) -> bool {
        self.slot.request_stop();
        if let Some(request) = self.close_event.take() {
            assert!(self.slot.send(Candidate::Closed(request)));
        }
        self.close.load(Ordering::Acquire)
    }
}
fn queue(store: &mut Store, actor: &mut Outbox, scope: &Session) -> String {
    let command = post(scope, 1);
    let intent = ledger::put_intent(
        &mut store.db,
        &Identity {
            kind: Kind::Post,
            target: Target::Session {
                computer: "peer.example".into(),
                run: "fixture-run".into(),
                conversation: "fixture-conversation".into(),
            },
            edge: uuid(),
        },
        1,
        &serde_json::to_value(&command.action).unwrap(),
    )
    .unwrap();
    actor
        .offer(
            store,
            Reserve {
                request_id: command.binding.request_id.clone(),
                intent_id: intent,
                revision: 1,
                epoch: scope.writer_epoch.clone(),
                service_epoch: scope.service_epoch.clone(),
                adapter: scope.adapter.clone(),
                lease: scope.lease.clone(),
                retry_of: None,
            },
            Instant::now(),
        )
        .unwrap();
    command.binding.request_id
}
fn store(temp: &tempfile::TempDir, scope: &Session) -> Store {
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: scope.configuration.clone(),
    };
    location.prepare().unwrap();
    let mut store = Store::open(&location, &scope.service_epoch).unwrap();
    ledger::authorize(&mut store.db, &scope.writer_epoch, &scope.service_epoch).unwrap();
    store
}
#[test]
fn restore_precedes_effects_and_native_return_and_actual_join_bound_drain() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let scope = session();
    let mut store = store(&temp, &scope);
    let (mut actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let request = queue(&mut store, &mut actor, &scope);
    let posts = Arc::new(AtomicUsize::new(0));
    let close = Arc::new(AtomicBool::new(false));
    let (opened, slot_rx) = mpsc::sync_channel(1);
    let (entered, entered_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::channel();
    let mut runtime = Runtime::start_with(scope.clone(), endpoint, Arc::new(()), {
        let posts = posts.clone();
        let close = close.clone();
        move |_, slot| {
            opened.send(slot.clone()).unwrap();
            Ok(Fake {
                posts,
                close,
                slot,
                release: Some(release_rx),
                entered: Some(entered),
                close_event: None,
                callback_allowed: true,
            })
        }
    })
    .unwrap();
    let slot = slot_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let restored = activation(&post(&scope, 2));
    assert!(runtime.try_restore(restored));
    // The factory exists, but the worker cannot take an ordinary queued effect.
    assert_eq!(posts.load(Ordering::Acquire), 0);
    assert!(!slot.accepting_activations());
    assert!(runtime.finish_restoration());
    assert!(!runtime.finish_restoration());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(posts.load(Ordering::Acquire), 1);
    let callback = slot.hold().unwrap();
    runtime.request_stop();
    assert!(!runtime.drained());
    assert!(!runtime.try_join());
    // Stop does not detach or release an original synchronous native invocation.
    release.send(()).unwrap();
    until(|| !slot.accepting_activations());
    close.store(true, Ordering::Release);
    runtime.wake.unpark();
    assert!(!runtime.try_join());
    assert!(!runtime.drained());
    drop(callback);
    runtime.wake.unpark();
    until(|| runtime.try_join());
    assert!(runtime.drained());
    actor.poll(&mut store, Instant::now());
    let state: String = store
        .db
        .query_row(
            "SELECT state FROM desktop_attempts WHERE request_id=?1",
            [request],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
    assert_eq!(posts.load(Ordering::Acquire), 1);
}

#[test]
fn root_host_retains_restore_progress_and_waits_for_worker_and_callback_drain() {
    use crate::desktop::notifications::attachment::{Host, NativeHost};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let scope = session();
    let mut store = store(&temp, &scope);
    let (mut actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let owner = Arc::new(());
    let weak = Arc::downgrade(&owner);
    let posts = Arc::new(AtomicUsize::new(0));
    let (opened, slot_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let page: Vec<_> = (1..=16).map(|n| activation(&post(&scope, n))).collect();
    let closed = page[0].request_id.clone();
    let runtime = Runtime::start_with(scope.clone(), endpoint, owner.clone(), {
        let posts = posts.clone();
        move |_, slot| {
            opened.send(slot.clone()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(Fake {
                posts,
                close: Arc::new(AtomicBool::new(true)),
                slot,
                release: None,
                entered: None,
                close_event: Some(closed),
                callback_allowed: true,
            })
        }
    })
    .unwrap();
    drop(owner);
    let slot = slot_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut host = NativeHost::windows(runtime);
    assert!(!host.ready().unwrap());
    // The original eight-entry FIFO is full while native setup is held. A
    // retry must retain the first unaccepted index of this same bounded page.
    assert!(!host.restore(&page).unwrap());
    assert!(!host.restore(&page).unwrap());
    assert!(!slot.accepting_activations());
    release.send(()).unwrap();
    until(|| host.restore(&page).unwrap());
    until(|| host.finish().unwrap());
    assert!(host.ready().unwrap());
    assert!(slot.accepting_activations());
    assert_eq!(host.status()["mappings"], 16);
    assert_eq!(posts.load(Ordering::Acquire), 0);

    let delegate = slot.hold().unwrap();
    actor.request_stop();
    host.stop();
    until(|| !slot.accepting_activations());
    assert!(!host.drained(), "a retained native delegate prevents join");
    assert!(weak.upgrade().is_some());
    drop(delegate);
    until(|| host.drained());
    assert_eq!(actor.status()["callbacks"], 1);
    assert!(
        !actor.operation_drained(),
        "native join does not consume the original queued Closed callback"
    );
    actor.poll(&mut store, Instant::now());
    assert!(actor.operation_drained());
    assert!(weak.upgrade().is_some());
    drop(slot);
    drop(host);
    assert!(weak.upgrade().is_none());
}

#[test]
fn root_host_refuses_native_setup_fault_and_still_joins_the_original_worker() {
    use crate::desktop::notifications::attachment::{Host, NativeHost};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scope = session();
    let (_actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let runtime = Runtime::start_with(scope, endpoint, Arc::new(()), |_, _| {
        Err::<Fake, _>("fixture_native_refused")
    })
    .unwrap();
    let mut host = NativeHost::windows(runtime);
    until(|| host.status()["fault"] == "fixture_native_refused");
    assert!(host.ready().is_err());
    assert!(host.finish().is_err());
    host.stop();
    until(|| host.drained());
    assert_eq!(host.status()["joined"], true);
}
fn invalid_restore_with_order(finish_before_invalid: bool) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let scope = session();
    let mut store = store(&temp, &scope);
    let (mut actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    queue(&mut store, &mut actor, &scope);
    let posts = Arc::new(AtomicUsize::new(0));
    let close = Arc::new(AtomicBool::new(false));
    let (opened, slot_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::channel();
    let mut runtime = Runtime::start_with(scope.clone(), endpoint, Arc::new(()), {
        let posts = posts.clone();
        let close = close.clone();
        move |_, slot| {
            opened.send(slot.clone()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(Fake {
                posts,
                close,
                slot,
                release: None,
                entered: None,
                close_event: None,
                callback_allowed: true,
            })
        }
    })
    .unwrap();
    let slot = slot_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut invalid = activation(&post(&scope, 1));
    invalid.request_id = "invalid".into();
    assert!(runtime.try_restore(invalid));
    // Holding the constructor makes finish-before-consumption deterministic.
    if finish_before_invalid {
        assert!(runtime.finish_restoration());
    }
    release.send(()).unwrap();
    // Observing stop also covers the opposite ordering: the accepted restore
    // has been rejected before finish is attempted, so finish must now refuse.
    until(|| runtime.stop.load(Ordering::Acquire));
    let finished_after_invalid = runtime.finish_restoration();
    assert_eq!(runtime.status()["fault"], "notification_restore_refused");
    assert_eq!(posts.load(Ordering::Acquire), 0);
    assert!(!slot.accepting_activations());
    assert!(!runtime.try_restore(activation(&post(&scope, 2))));
    assert!(!runtime.try_join());
    assert!(!runtime.drained());
    close.store(true, Ordering::Release);
    runtime.wake.unpark();
    until(|| runtime.try_join());
    assert!(runtime.drained());
    assert!(runtime.worker.is_none(), "the original worker was joined");
    assert_eq!(posts.load(Ordering::Acquire), 0);
    assert!(!finished_after_invalid);
    drop(actor);
}
#[test]
fn invalid_restore_stops_before_native_work_and_closes_real_worker() {
    invalid_restore_with_order(true);
}
#[test]
fn invalid_restore_observed_before_finish_refuses_restoration_and_joins_worker() {
    invalid_restore_with_order(false);
}
#[test]
fn failed_native_setup_keeps_original_owner_until_last_native_reference_returns() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    struct Owner(Arc<AtomicBool>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release)
        }
    }
    let released = Arc::new(AtomicBool::new(false));
    let scope = session();
    let (actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let (held, held_rx) = mpsc::sync_channel::<Retained>(1);
    let mut runtime = Runtime::start_with::<Fake>(
        scope,
        endpoint,
        Arc::new(Owner(released.clone())),
        move |_, slot| {
            held.send(slot.hold().unwrap()).unwrap();
            Err("native_setup_refused")
        },
    )
    .unwrap();
    let callback = held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    until(|| runtime.status()["fault"] == "native_setup_refused");
    assert!(!runtime.try_join());
    assert!(!runtime.drained());
    assert!(!released.load(Ordering::Acquire));
    drop(runtime);
    assert!(!released.load(Ordering::Acquire));
    drop(callback);
    until(|| released.load(Ordering::Acquire));
    drop(actor);
    // The abandoned worker releases admission only after the actual retained object.
    until(|| !ACTIVE.load(Ordering::Acquire));
}

#[test]
fn close_generated_during_unregistration_is_forwarded_before_native_drain() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scope = session();
    let (actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let command = post(&scope, 1);
    let restored = activation(&command);
    let request = restored.request_id.clone();
    let mut runtime = Runtime::start_with(scope, endpoint, Arc::new(()), move |_, slot| {
        Ok(Fake {
            posts: Arc::new(AtomicUsize::new(0)),
            close: Arc::new(AtomicBool::new(true)),
            slot,
            release: None,
            entered: None,
            close_event: Some(request),
            callback_allowed: true,
        })
    })
    .unwrap();
    assert!(runtime.try_restore(restored));
    assert!(runtime.finish_restoration());
    until(|| runtime.status()["ready"] == true);
    runtime.request_stop();
    until(|| runtime.try_join());
    assert!(runtime.drained());
    assert_eq!(actor.status()["callbacks"], 1);
}

#[test]
fn actor_backpressure_keeps_the_original_close_until_its_queue_can_accept_it() {
    let temp = tempfile::tempdir().unwrap();
    let scope = session();
    let mut store = store(&temp, &scope);
    let (mut actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let original = activation(&post(&scope, 1));
    for _ in 0..super::super::super::EVENTS {
        assert!(endpoint.emit(Event::Closed(original.clone())));
    }
    let mut mappings = Mappings::new(scope).unwrap();
    assert!(mappings.restore(original.clone()));
    assert!(mappings.finish_restoration());
    let (slot, _) = Slot::new(thread::current(), Arc::new(()));
    let mut native = Fake {
        posts: Arc::new(AtomicUsize::new(0)),
        close: Arc::new(AtomicBool::new(true)),
        slot,
        release: None,
        entered: None,
        close_event: None,
        callback_allowed: true,
    };
    let (send, receive) = mpsc::sync_channel(1);
    send.send(Candidate::Closed(original.request_id.clone()))
        .unwrap();
    let mut pending = None;
    assert!(!drain_events(
        &receive,
        &mut pending,
        &mut mappings,
        &endpoint,
        &mut native
    ));
    assert!(matches!(&pending,Some(Event::Closed(value)) if value==&original));
    actor.poll(&mut store, Instant::now());
    assert_eq!(actor.status()["callbacks"], 0);
    assert!(drain_events(
        &receive,
        &mut pending,
        &mut mappings,
        &endpoint,
        &mut native
    ));
    assert!(pending.is_none());
    assert_eq!(actor.status()["callbacks"], 1);
    assert_eq!(native.posts.load(Ordering::Acquire), 0);
}

#[test]
fn a_restored_click_is_refused_after_the_native_user_or_registration_changes() {
    let scope = session();
    let (actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let original = activation(&post(&scope, 1));
    let mut mappings = Mappings::new(scope).unwrap();
    mappings.restore(original.clone());
    mappings.finish_restoration();
    let (slot, _) = Slot::new(thread::current(), Arc::new(()));
    let mut native = Fake {
        posts: Arc::new(AtomicUsize::new(0)),
        close: Arc::new(AtomicBool::new(true)),
        slot,
        release: None,
        entered: None,
        close_event: None,
        callback_allowed: false,
    };
    let (send, receive) = mpsc::sync_channel(1);
    send.send(Candidate::Clicked(original.request_id)).unwrap();
    let mut pending = None;
    assert!(drain_events(
        &receive,
        &mut pending,
        &mut mappings,
        &endpoint,
        &mut native
    ));
    assert_eq!(actor.status()["callbacks"], 0);
    assert_eq!(native.posts.load(Ordering::Acquire), 0);
}
