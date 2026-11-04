//! One native worker, with nonblocking admission and observed resource drain.
use super::{
    callbacks::{Candidate, Slot},
    wire, Mappings, Native,
};
use crate::desktop::notifications::{Endpoint, Event};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle, Thread},
    time::Duration,
};
use wire::{Activation, Platform, Session};

const IDLE: Duration = Duration::from_millis(250);
static ACTIVE: AtomicBool = AtomicBool::new(false);
struct Admission;
impl Admission {
    fn acquire() -> Result<Self, &'static str> {
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "notification_runtime_still_owned")
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

// One deferred original callback when the actor's bounded queue is full.
// Returning false never drops a Closed edge or claims callback drain.
fn drain_events(
    events: &mpsc::Receiver<Candidate>,
    pending: &mut Option<Event>,
    mappings: &mut Mappings,
    endpoint: &Endpoint,
    native: &mut impl Native,
) -> bool {
    for _ in 0..super::super::EVENTS {
        if mappings.closing && matches!(pending, Some(Event::Clicked(_))) {
            *pending = None;
        }
        if pending.is_none() {
            let value = match events.try_recv() {
                Ok(Candidate::Clicked(request)) => mappings.clicked(&request).map(Event::Clicked),
                Ok(Candidate::Closed(request)) => mappings.closed(&request).map(|value| {
                    native.retire(&request);
                    Event::Closed(value)
                }),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return true,
            };
            *pending = value;
        }
        if matches!(pending, Some(Event::Clicked(_))) && !native.callbacks_current() {
            *pending = None;
            continue;
        }
        if let Some(event) = pending.as_ref() {
            let event = match event {
                Event::Clicked(v) => Event::Clicked(v.clone()),
                Event::Closed(v) => Event::Closed(v.clone()),
            };
            if !endpoint.emit(event) {
                return false;
            }
            *pending = None;
        }
    }
    false
}

#[derive(Default)]
struct Status {
    ready: bool,
    restored: bool,
    native_drained: bool,
    resources: usize,
    mappings: usize,
    fault: Option<&'static str>,
}
pub(in crate::desktop) struct Runtime {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    wake: Thread,
    restore: mpsc::SyncSender<Activation>,
    restore_closed: Mutex<bool>,
    restore_done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    joined: bool,
    // Independent of command resources, including after a worker panic.
    _resources: Arc<dyn Send + Sync>,
}
impl Runtime {
    #[cfg(windows)]
    pub fn start(
        config: crate::config::Config,
        session: Session,
        endpoint: Endpoint,
        resources: Arc<dyn Send + Sync>,
    ) -> Result<Self, &'static str> {
        Self::start_with(session, endpoint, resources, move |session, slot| {
            super::native::Backend::open(&config, session, slot)
        })
    }
    fn start_with<N: Native + 'static>(
        session: Session,
        endpoint: Endpoint,
        resources: Arc<dyn Send + Sync>,
        factory: impl FnOnce(&Session, Slot) -> Result<N, &'static str> + Send + 'static,
    ) -> Result<Self, &'static str> {
        if !session.valid()
            || session.platform != Platform::Windows
            || endpoint.platform != Platform::Windows
        {
            return Err("notification_scope_changed");
        }
        let admission = Admission::acquire()?;
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let (restore, restores) = mpsc::sync_channel(wire::IN_FLIGHT);
        let restore_done = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            let status = status.clone();
            let restore_done = restore_done.clone();
            let resources = resources.clone();
            thread::Builder::new()
                .name("windows-notifications".into())
                .spawn(move || {
                    let _admission = admission;
                    let (slot, events) = Slot::new(thread::current(), resources);
                    let opened = factory(&session, slot.clone());
                    let mut native = match opened {
                        Ok(value) => value,
                        Err(reason) => {
                            slot.request_stop();
                            stop.store(true, Ordering::Release);
                            status.lock().unwrap_or_else(|e| e.into_inner()).fault = Some(reason);
                            // A failed constructor never proves that native references
                            // vanished. Retain original owner resources while any live.
                            loop {
                                let count = slot.retained();
                                status.lock().unwrap_or_else(|e| e.into_inner()).resources = count;
                                if count == 0 {
                                    break;
                                }
                                thread::park_timeout(IDLE);
                            }
                            status
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .native_drained = true;
                            return;
                        }
                    };
                    let mut mappings = Mappings::new(session).expect("checked original session");
                    let mut pending = None;
                    loop {
                        if native.unavailable() {
                            stop.store(true, Ordering::Release);
                            status
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .fault
                                .get_or_insert("notification_native_scope_changed");
                        }
                        let stopping = stop.load(Ordering::Acquire) || endpoint.stopping();
                        if stopping {
                            mappings.request_stop();
                            slot.request_stop();
                        }
                        let mut progress = false;
                        if !mappings.restored && !stopping {
                            for _ in 0..wire::IN_FLIGHT {
                                match restores.try_recv() {
                                    Ok(value) => {
                                        progress = true;
                                        if !mappings.restore(value) {
                                            status
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .fault = Some("notification_restore_refused");
                                            stop.store(true, Ordering::Release);
                                            break;
                                        }
                                    }
                                    Err(mpsc::TryRecvError::Empty) => {
                                        // Finish publishes only after all successful
                                        // enqueues. This empty check follows its load.
                                        if restore_done.load(Ordering::Acquire) {
                                            match restores.try_recv() {
                                                Ok(value) => {
                                                    progress = true;
                                                    if !mappings.restore(value) {
                                                        status
                                                            .lock()
                                                            .unwrap_or_else(|e| e.into_inner())
                                                            .fault =
                                                            Some("notification_restore_refused");
                                                        stop.store(true, Ordering::Release);
                                                    }
                                                }
                                                Err(mpsc::TryRecvError::Empty) => {
                                                    mappings.finish_restoration();
                                                    slot.finish_restoration();
                                                }
                                                Err(mpsc::TryRecvError::Disconnected) => {
                                                    stop.store(true, Ordering::Release);
                                                }
                                            }
                                        }
                                        break;
                                    }
                                    Err(mpsc::TryRecvError::Disconnected) => {
                                        stop.store(true, Ordering::Release);
                                        break;
                                    }
                                }
                            }
                        }
                        drain_events(&events, &mut pending, &mut mappings, &endpoint, &mut native);
                        if slot.overflow() {
                            status.lock().unwrap_or_else(|e| e.into_inner()).fault =
                                Some("notification_callback_overflow");
                            stop.store(true, Ordering::Release);
                        }
                        if native.unavailable() {
                            stop.store(true, Ordering::Release);
                            status
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .fault
                                .get_or_insert("notification_native_scope_changed");
                        }
                        let stopping = stop.load(Ordering::Acquire) || endpoint.stopping();
                        if stopping {
                            mappings.request_stop();
                            slot.request_stop();
                            if native.close()
                                && slot.retained() == 0
                                && drain_events(
                                    &events,
                                    &mut pending,
                                    &mut mappings,
                                    &endpoint,
                                    &mut native,
                                )
                            {
                                let mut value = status.lock().unwrap_or_else(|e| e.into_inner());
                                value.ready = false;
                                value.native_drained = true;
                                value.resources = 0;
                                break;
                            }
                        } else if mappings.restored {
                            if let Ok(work) = endpoint.try_next() {
                                progress = true;
                                if let Some(dispatch) = work.begin() {
                                    mappings.dispatch(dispatch, &mut native, &stop);
                                }
                            }
                        }
                        {
                            let mut value = status.lock().unwrap_or_else(|e| e.into_inner());
                            value.ready = mappings.restored && !stopping;
                            value.restored = mappings.restored;
                            value.resources = slot.retained();
                            value.mappings = mappings.delivered.len();
                        }
                        if !progress {
                            thread::park_timeout(IDLE);
                        }
                    }
                })
                .map_err(|_| "notification_worker_unavailable")?
        };
        Ok(Self {
            stop,
            status,
            wake: worker.thread().clone(),
            restore,
            restore_closed: Mutex::new(false),
            restore_done,
            worker: Some(worker),
            joined: false,
            _resources: resources,
        })
    }
    pub fn try_restore(&self, value: Activation) -> bool {
        let closed = self
            .restore_closed
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *closed || self.stop.load(Ordering::Acquire) {
            return false;
        }
        let sent = self.restore.try_send(value).is_ok();
        if sent {
            self.wake.unpark();
        }
        sent
    }
    pub fn finish_restoration(&self) -> bool {
        let mut closed = self
            .restore_closed
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *closed || self.stop.load(Ordering::Acquire) {
            return false;
        }
        *closed = true;
        self.restore_done.store(true, Ordering::Release);
        self.wake.unpark();
        true
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.unpark();
    }
    pub fn try_join(&mut self) -> bool {
        if self.joined {
            return true;
        }
        if !self
            .worker
            .as_ref()
            .is_some_and(|value| value.is_finished())
        {
            return false;
        }
        let value = self.worker.take().expect("finished worker").join();
        if value.is_err() {
            self.status.lock().unwrap_or_else(|e| e.into_inner()).fault =
                Some("notification_worker_panicked");
            return false;
        }
        self.joined = true;
        true
    }
    pub fn drained(&self) -> bool {
        self.joined
            && self
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .native_drained
    }
    pub fn status(&self) -> serde_json::Value {
        let value = self.status.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::json!({"ready":value.ready&&!self.stop.load(Ordering::Acquire),"restored":value.restored,"native_resources":value.resources,"mappings":value.mappings,"joined":self.joined,"drained":self.joined&&value.native_drained,"fault":value.fault,"cold_start_available":false})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[cfg(test)]
mod tests;
