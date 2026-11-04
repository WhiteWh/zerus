//! One original off-actor selection worker. It performs no native post, process
//! launch, ledger mutation or authority grant. A timeout never releases its
//! actual service owner/handoff resources before the original worker joins.
use super::attachment::Selection;
use crate::{
    config::Config,
    desktop::{
        ownership::Grant,
        store::{HandoffLock, Location},
    },
    platform,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context;
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(in crate::desktop) static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Admission;
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

/// The real serve owner, never a caller-supplied liveness boolean or empty Arc.
pub(in crate::desktop) struct Owner {
    _service: Arc<platform::FileLock>,
    handoff: Arc<HandoffLock>,
    _admission: Admission,
}
impl Owner {
    fn acquire(service: Arc<platform::FileLock>, location: &Location) -> Result<Arc<Self>> {
        ensure!(
            ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "notification preparation still owned"
        );
        let admission = Admission;
        let handoff = Arc::new(location.handoff_lock()?);
        Ok(Arc::new(Self {
            _service: service,
            handoff,
            _admission: admission,
        }))
    }
    pub fn handoff(&self) -> Arc<HandoffLock> {
        self.handoff.clone()
    }
}

pub(in crate::desktop) struct Prepared {
    pub grant: Grant,
    pub selection: Selection,
}
pub(in crate::desktop) struct Preparation {
    worker: Option<JoinHandle<Result<Prepared>>>,
    stopped: Arc<AtomicBool>,
    owner: Option<Arc<Owner>>,
    until: Instant,
    failed: bool,
}
impl Preparation {
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::desktop) fn selected_for_test(
        service: Arc<platform::FileLock>,
        location: &Location,
        grant: Grant,
        selection: Selection,
    ) -> Result<Self> {
        Self::spawn(Owner::acquire(service, location)?, move |_| {
            Ok(Prepared { grant, selection })
        })
    }
    #[cfg(test)]
    pub(in crate::desktop) fn blocked_for_test(
        service: Arc<platform::FileLock>,
        location: &Location,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self::spawn(Owner::acquire(service, location).unwrap(), move |_| {
            release.recv().unwrap();
            anyhow::bail!("fixture selection refused after release")
        })
        .unwrap()
    }
    pub fn start(
        config: &Config,
        location: &Location,
        grant: Grant,
        service: Arc<platform::FileLock>,
    ) -> Result<Self> {
        let owner = Owner::acquire(service, location)?;
        #[cfg(windows)]
        let config = config.clone();
        #[cfg(not(windows))]
        let _ = config;
        Self::spawn(owner, move |stopped| {
            ensure!(
                !stopped.load(Ordering::Acquire),
                "notification selection stopped"
            );
            #[cfg(target_os = "linux")]
            let selection = {
                let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
                    .context("notification bus address unavailable")?;
                ensure!(
                    address.len() <= 4096 && address.starts_with("unix:"),
                    "notification bus requires an explicit bounded Unix address"
                );
                Selection::Linux { address }
            };
            #[cfg(target_os = "macos")]
            let selection = {
                let service = std::env::current_exe()?;
                let bundle = service
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                    .context("notification bundle unavailable")?;
                ensure!(
                    bundle.extension().is_some_and(|e| e == "app")
                        && bundle.join("Contents/MacOS/hgs") == service,
                    "running service is not the fixed paired bundle image"
                );
                Selection::Macos(super::host::SelectedHost::select(bundle)?)
            };
            #[cfg(windows)]
            let selection = Selection::Windows { config };
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            let selection = Selection::Unavailable;
            ensure!(
                !stopped.load(Ordering::Acquire),
                "notification selection stopped"
            );
            Ok(Prepared { grant, selection })
        })
    }
    fn spawn(
        owner: Arc<Owner>,
        select: impl FnOnce(&AtomicBool) -> Result<Prepared> + Send + 'static,
    ) -> Result<Self> {
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let owner = owner.clone();
            let stopped = stopped.clone();
            thread::Builder::new()
                .name("notification-selection".into())
                .spawn(move || {
                    let _owner = owner;
                    select(&stopped)
                })?
        };
        Ok(Self {
            worker: Some(worker),
            stopped,
            owner: Some(owner),
            until: Instant::now() + Duration::from_secs(10),
            failed: false,
        })
    }
    pub fn owner(&self) -> Option<Arc<Owner>> {
        self.owner.clone()
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    /// Return only after joining the same original worker. No replacement or
    /// timeout-based release of its pinned owner/handoff is possible.
    pub fn poll(&mut self) -> Option<Result<Prepared>> {
        if self.worker.as_ref().is_none_or(|w| !w.is_finished()) {
            return None;
        }
        let result = self
            .worker
            .take()
            .unwrap()
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("notification selection worker panicked")));
        if self.stopped.load(Ordering::Acquire) {
            self.owner = None;
            return Some(Err(anyhow::anyhow!("notification selection stopped")));
        }
        self.failed = result.is_err();
        Some(result)
    }
    pub fn drained(&self) -> bool {
        self.worker.is_none()
    }
    pub fn status(&self) -> Value {
        json!({"running":self.worker.is_some(),"drained":self.drained(),"stopping":self.stopped.load(Ordering::Acquire),"error":self.failed,"deadline_elapsed":Instant::now()>=self.until})
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        self.stop();
        if self.worker.is_some() {
            // No joined result means no proof of completion, including an
            // unwinding native verifier. Preserve this original admission.
            if let Some(owner) = self.owner.take() {
                std::mem::forget(owner);
            }
        }
    }
}

#[cfg(test)]
mod tests;
