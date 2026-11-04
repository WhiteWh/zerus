//! Dormant startup resources only: no stores, payloads, mappings or app sink.
//! The once-only async-io reactor is shared process infrastructure. Every
//! per-startup socket/task/waker/capture must still end before CleanExit/join.
//! Raw16 bounds item count; ingress separately caps serialized wire/text.
//! Decoded SDK metadata/allocator memory remains an open production gate.
//! This supplies no role permission.
#![allow(dead_code)] // No production constructor caller until G1 qualification.
use crate::notifier::accounting::{Completion, Control as AccountingControl, Resources};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex, Weak,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zerus_client_core::desktop::drain::{Charge, Role};

#[path = "../../../../../crates/freedesktop-notify/src/ingress.rs"]
mod ingress;
#[cfg(test)]
mod tests;
mod worker;
const BUDGET: Duration = Duration::from_secs(5);
const RAW: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupRefused {
    Input,
    Role,
    Context,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupProgress {
    Pending,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartupSnapshot {
    pub closed: bool,
    pub ready: bool,
    pub pinned: bool,
}

struct State {
    closed: bool,
    ready: bool,
    cleanup: Option<Instant>,
    #[cfg(test)]
    peer: Option<String>,
}
struct Scope {
    state: Mutex<State>,
    cancel: AtomicBool,
    wake: mpsc::SyncSender<()>,
    accounting: AccountingControl,
    _resources: Arc<Resources>,
    budget: Duration,
}
impl Scope {
    fn close(&self) -> Result<StartupSnapshot, StartupRefused> {
        let original = Instant::now() + self.budget;
        let change = self.state.lock().map(|mut s| {
            s.closed = true;
            s.ready = false;
            s.cleanup.get_or_insert(original);
        });
        // Poison never recovers readiness; wake the original worker to refuse.
        self.cancel.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        change.map_err(|_| StartupRefused::Unavailable)?;
        self.accounting
            .close()
            .map_err(|_| StartupRefused::Unavailable)?;
        self.snapshot()
    }
    fn snapshot(&self) -> Result<StartupSnapshot, StartupRefused> {
        let s = self.state.lock().map_err(|_| StartupRefused::Unavailable)?;
        let accounting = self
            .accounting
            .snapshot()
            .map_err(|_| StartupRefused::Unavailable)?;
        Ok(StartupSnapshot {
            closed: s.closed,
            ready: s.ready && !s.closed,
            pinned: accounting.pinned,
        })
    }
    fn ready(&self, deadline: Instant) -> Result<(), ()> {
        let mut s = self.state.lock().map_err(|_| ())?;
        if s.closed || Instant::now() >= deadline {
            return Err(());
        }
        s.ready = true;
        Ok(())
    }
    fn cleanup(&self) -> Result<Instant, ()> {
        self.close().map_err(|_| ())?;
        self.state.lock().map_err(|_| ())?.cleanup.ok_or(())
    }
}

pub(crate) struct OwnedControl(Weak<Scope>);
impl OwnedControl {
    pub(crate) fn close(&self) -> Result<StartupSnapshot, StartupRefused> {
        self.0.upgrade().ok_or(StartupRefused::Unavailable)?.close()
    }
    pub(crate) fn snapshot(&self) -> Result<StartupSnapshot, StartupRefused> {
        self.0
            .upgrade()
            .ok_or(StartupRefused::Unavailable)?
            .snapshot()
    }
}
pub(crate) struct OwnedStartup {
    scope: Arc<Scope>,
    worker: Option<JoinHandle<worker::Exit>>,
    guard: Option<Completion>,
    finished: bool,
}
impl OwnedStartup {
    /// Off GUI only. Neither is_finished nor a stopped flag releases accounting.
    pub(crate) fn try_finish(&mut self) -> Result<StartupProgress, StartupRefused> {
        if gtk::is_initialized_main_thread() {
            return Err(StartupRefused::Context);
        }
        if self.finished {
            return Ok(StartupProgress::Finished);
        }
        if self.guard.is_none() {
            return Err(StartupRefused::Unavailable);
        }
        if !matches!(self.scope.snapshot(), Ok(snapshot) if !snapshot.pinned) {
            let _ = self.scope.close();
            drop(self.guard.take());
            return Err(StartupRefused::Unavailable);
        }
        let handle = self.worker.as_ref().ok_or(StartupRefused::Unavailable)?;
        if !handle.is_finished() {
            return Ok(StartupProgress::Pending);
        }
        let result = self.worker.take().unwrap().join();
        if matches!(result, Ok(worker::Exit::Clean)) {
            self.guard.take().unwrap().complete();
            self.finished = true;
            Ok(StartupProgress::Finished)
        } else {
            // An unexecuted/unknown guard pins exactly its existing fixed group.
            let _ = self.scope.close();
            drop(self.guard.take());
            Err(StartupRefused::Unavailable)
        }
    }
}
impl Drop for OwnedStartup {
    fn drop(&mut self) {
        let _ = self.scope.close();
    }
}

pub(crate) fn spawn_owned_startup(
    address: String,
    host: Charge,
) -> Result<(OwnedStartup, OwnedControl), StartupRefused> {
    spawn(address, host, Hooks::default())
}
fn spawn(
    address: String,
    host: Charge,
    hooks: Hooks,
) -> Result<(OwnedStartup, OwnedControl), StartupRefused> {
    if host.role() != Role::NativePosting {
        return Err(StartupRefused::Role);
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(StartupRefused::Context);
    }
    // This local Linux prerequisite never interprets an autolaunch/fallback list.
    if address.is_empty()
        || address.len() > 16384
        || address.capacity() > ingress::AUTH
        || !address.starts_with("unix:")
        || address.contains(';')
        || address.chars().any(char::is_control)
        || zbus::Address::try_from(address.as_str()).is_err()
    {
        return Err(StartupRefused::Input);
    }
    let budget = hooks.budget();
    if budget.is_zero() || budget > BUDGET {
        return Err(StartupRefused::Input);
    }
    // Original deadline, before handoff. This existing fixed NativePosting
    // group reserves ingress::RESERVATION. Unknown/lost handles keep that
    // SAME group's pin; no byte object's Drop refunds it. Only this original
    // Completion after CleanExit/join releases it.
    let setup = Instant::now() + budget;
    let (resources, accounting) = Resources::new(host).map_err(|_| StartupRefused::Unavailable)?;
    let guard = resources.start().map_err(|_| StartupRefused::Unavailable)?;
    let (wake, receiver) = mpsc::sync_channel(1);
    let scope = Arc::new(Scope {
        state: Mutex::new(State {
            closed: false,
            ready: false,
            cleanup: None,
            #[cfg(test)]
            peer: None,
        }),
        cancel: AtomicBool::new(false),
        wake,
        accounting,
        _resources: resources,
        budget,
    });
    let worker_scope = scope.clone();
    let worker = std::thread::Builder::new()
        .name("zerus-linux-notifications".into())
        .spawn(move || {
            // No native work until the original handle and guard are owned by
            // OwnedStartup. This first token shares the bounded wake channel.
            if receiver
                .recv_timeout(setup.saturating_duration_since(Instant::now()))
                .is_err()
            {
                return worker::Exit::Unknown;
            }
            worker::run(address, worker_scope, receiver, setup, hooks)
        })
        .map_err(|_| StartupRefused::Unavailable)?;
    let control = OwnedControl(Arc::downgrade(&scope));
    let startup = OwnedStartup {
        scope,
        worker: Some(worker),
        guard: Some(guard),
        finished: false,
    };
    let _ = startup.scope.wake.try_send(());
    Ok((startup, control))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hook {
    BeforeNative,
    Connected,
    Matched,
    Raw,
    Message,
    DropReaders,
    DropConnection,
    #[cfg(test)]
    TickCreated,
    #[cfg(test)]
    TickDropped,
    #[cfg(test)]
    BeforePark,
    #[cfg(test)]
    AfterPark,
    #[cfg(test)]
    ProbeSignal,
}
#[derive(Default)]
struct Hooks {
    #[cfg(test)]
    event: Option<Arc<dyn Fn(Hook) + Send + Sync>>,
    #[cfg(test)]
    budget: Option<Duration>,
    #[cfg(test)]
    runnable: Option<Arc<worker::RunnableProbe>>,
}
impl Hooks {
    fn budget(&self) -> Duration {
        #[cfg(test)]
        if let Some(budget) = self.budget {
            return budget;
        }
        BUDGET
    }
    fn at(&self, point: Hook) {
        #[cfg(test)]
        if let Some(f) = &self.event {
            f(point);
        }
        #[cfg(not(test))]
        let _ = point;
    }
}
macro_rules! redacted {
    ($($t:ty),+) => { $(impl std::fmt::Debug for $t {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($t),"([startup resources])"))
        }
    })+ };
}
redacted!(OwnedStartup, OwnedControl);
