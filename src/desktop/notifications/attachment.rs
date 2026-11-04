//! Actor attachment to an original platform handle. Selection/admission belongs
//! to the lifecycle; no generic request or ambient path search starts a host.
#![allow(dead_code)] // Activated by the explicit service handoff/policy integration.
use super::{Activation, Endpoint, Outbox, RestorePage, Session};
use crate::desktop::{
    attention,
    store::{Location, Store},
};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

static ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(all(test, target_os = "linux"))]
pub(in crate::desktop) static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Admission;
impl Admission {
    fn acquire() -> Result<Self> {
        ensure!(
            ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "notification attachment still owned"
        );
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}
struct Lifetime {
    _owner: Arc<dyn Send + Sync>,
    _admission: Admission,
}

/// Constructed only from maintained selection. Mac native selection is done
/// off actor before passing its opaque result here; an address is data only.
pub(in crate::desktop) enum Selection {
    #[cfg(target_os = "linux")]
    Linux { address: String },
    #[cfg(target_os = "macos")]
    Macos(super::host::SelectedHost),
    #[cfg(windows)]
    Windows { config: crate::config::Config },
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    Unavailable,
}

pub(in crate::desktop) trait Host {
    /// Retry the SAME page until true. It never invokes a posting API.
    fn restore(&mut self, page: &[Activation]) -> Result<bool>;
    fn finish(&mut self) -> Result<bool>;
    fn ready(&self) -> Result<bool>;
    fn stop(&self);
    fn drained(&mut self) -> bool;
    fn status(&self) -> Value;
}

pub(in crate::desktop) struct NativeHost(NativeKind);
enum NativeKind {
    #[cfg(target_os = "linux")]
    Linux {
        runtime: super::linux::Runtime,
        acknowledgement: Option<super::linux::Acknowledgement>,
    },
    #[cfg(target_os = "macos")]
    Macos {
        runtime: super::host::OwnedHost,
        offset: usize,
        finished: bool,
    },
    #[cfg(any(windows, test))]
    Windows {
        runtime: super::windows::runtime::Runtime,
        offset: usize,
        finished: bool,
    },
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    Unavailable,
}
impl NativeHost {
    #[cfg(any(windows, test))]
    pub(in crate::desktop) fn windows(runtime: super::windows::runtime::Runtime) -> Self {
        Self(NativeKind::Windows {
            runtime,
            offset: 0,
            finished: false,
        })
    }
}
impl Host for NativeHost {
    fn restore(&mut self, page: &[Activation]) -> Result<bool> {
        match &mut self.0 {
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            NativeKind::Unavailable => {
                let _ = page;
                anyhow::bail!("notification platform unavailable")
            }
            #[cfg(target_os = "linux")]
            NativeKind::Linux {
                runtime,
                acknowledgement,
            } => {
                if let Some(ack) = acknowledgement {
                    return match ack.poll() {
                        None => Ok(false),
                        Some(ok) => {
                            *acknowledgement = None;
                            ensure!(ok, "notification restore refused");
                            Ok(true)
                        }
                    };
                }
                if page.is_empty() {
                    return Ok(true);
                }
                *acknowledgement = runtime.restore(page.to_vec());
                Ok(false)
            }
            #[cfg(target_os = "macos")]
            NativeKind::Macos {
                runtime, offset, ..
            } => {
                while *offset < page.len() {
                    if !runtime.restore(page[*offset].clone()) {
                        return Ok(false);
                    }
                    *offset += 1;
                }
                *offset = 0;
                Ok(true)
            }
            #[cfg(any(windows, test))]
            NativeKind::Windows {
                runtime, offset, ..
            } => {
                while *offset < page.len() {
                    if !runtime.try_restore(page[*offset].clone()) {
                        return Ok(false);
                    }
                    *offset += 1;
                }
                *offset = 0;
                Ok(true)
            }
        }
    }
    fn finish(&mut self) -> Result<bool> {
        match &mut self.0 {
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            NativeKind::Unavailable => anyhow::bail!("notification platform unavailable"),
            #[cfg(target_os = "linux")]
            NativeKind::Linux {
                runtime,
                acknowledgement,
            } => {
                if let Some(ack) = acknowledgement {
                    return match ack.poll() {
                        None => Ok(false),
                        Some(ok) => {
                            *acknowledgement = None;
                            ensure!(ok, "notification finish refused");
                            Ok(true)
                        }
                    };
                }
                *acknowledgement = runtime.finish_restoration();
                Ok(false)
            }
            #[cfg(target_os = "macos")]
            NativeKind::Macos {
                runtime, finished, ..
            } => {
                if !*finished {
                    runtime.finish_restoration();
                    *finished = true;
                }
                // The original FIFO writes every Restore before new commands.
                Ok(runtime.status()["ready"] == true)
            }
            #[cfg(any(windows, test))]
            NativeKind::Windows {
                runtime, finished, ..
            } => {
                if !*finished {
                    ensure!(runtime.finish_restoration(), "notification finish refused");
                    *finished = true;
                }
                let status = runtime.status();
                ensure!(status["fault"].is_null(), "notification host failed");
                // The worker acknowledges only after its original restore FIFO
                // is empty and native activation admission has been opened.
                Ok(status["restored"] == true && status["ready"] == true)
            }
        }
    }
    fn ready(&self) -> Result<bool> {
        let status = self.status();
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        ensure!(
            status["supported"] == true,
            "notification platform unavailable"
        );
        #[cfg(target_os = "linux")]
        ensure!(
            status["error"] != true,
            "{}",
            status["code"]
                .as_str()
                .unwrap_or("notification_linux_host_failed")
        );
        #[cfg(target_os = "macos")]
        if let Some(code) = status["fault"].as_str() {
            ensure!(code == "notification_status_busy", "{code}");
        }
        #[cfg(any(windows, test))]
        if matches!(&self.0, NativeKind::Windows { .. }) {
            ensure!(
                status["fault"].is_null(),
                "{}",
                status["fault"]
                    .as_str()
                    .unwrap_or("notification_windows_host_failed")
            );
        }
        Ok(status["ready"] == true)
    }
    fn stop(&self) {
        match &self.0 {
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            NativeKind::Unavailable => (),
            #[cfg(target_os = "linux")]
            NativeKind::Linux { runtime, .. } => runtime.request_stop(),
            #[cfg(target_os = "macos")]
            NativeKind::Macos { runtime, .. } => runtime.request_stop(),
            #[cfg(any(windows, test))]
            NativeKind::Windows { runtime, .. } => runtime.request_stop(),
        }
    }
    fn drained(&mut self) -> bool {
        match &mut self.0 {
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            NativeKind::Unavailable => true,
            #[cfg(target_os = "linux")]
            NativeKind::Linux { runtime, .. } => runtime.drained(),
            #[cfg(target_os = "macos")]
            NativeKind::Macos { runtime, .. } => runtime.poll_drained(),
            #[cfg(any(windows, test))]
            NativeKind::Windows { runtime, .. } => {
                runtime.try_join();
                runtime.drained()
            }
        }
    }
    fn status(&self) -> Value {
        match &self.0 {
            #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
            NativeKind::Unavailable => json!({"supported":false,"ready":false}),
            #[cfg(target_os = "linux")]
            NativeKind::Linux { runtime, .. } => runtime.status(),
            #[cfg(target_os = "macos")]
            NativeKind::Macos { runtime, .. } => runtime.status(),
            #[cfg(any(windows, test))]
            NativeKind::Windows { runtime, .. } => runtime.status(),
        }
    }
}

pub(in crate::desktop) struct Runtime<H: Host = NativeHost> {
    outbox: Outbox,
    host: H,
    owner: Option<Arc<dyn Send + Sync>>,
    after: i64,
    through: i64,
    cleanup_through: i64,
    cleanup: bool,
    page: Option<RestorePage>,
    restored: bool,
    finishing: bool,
    stopping: bool,
    quiescing: bool,
    retirement_started: bool,
    retirement_until: Option<Instant>,
    drained: bool,
    error: Option<String>,
    #[cfg(test)]
    retirement_clock: Option<Instant>,
}
impl Runtime<NativeHost> {
    pub fn start(
        location: &Location,
        store: &Store,
        session: Session,
        selection: Selection,
        owner: Arc<dyn Send + Sync>,
    ) -> Result<Self> {
        let lifetime: Arc<dyn Send + Sync> = Arc::new(Lifetime {
            _owner: owner,
            _admission: Admission::acquire()?,
        });
        #[cfg(windows)]
        let windows_owner = lifetime.clone();
        Self::with_factory(
            store,
            session,
            lifetime,
            |session, endpoint| match selection {
                #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                Selection::Unavailable => {
                    let _ = (location, session, endpoint);
                    anyhow::bail!("notification platform unavailable")
                }
                #[cfg(target_os = "linux")]
                Selection::Linux { address } => {
                    let _ = location;
                    Ok(NativeHost(NativeKind::Linux {
                        runtime: super::linux::Runtime::start(session, endpoint, address)?,
                        acknowledgement: None,
                    }))
                }
                #[cfg(target_os = "macos")]
                Selection::Macos(selected) => Ok(NativeHost(NativeKind::Macos {
                    runtime: selected.launch(location, session, endpoint)?,
                    offset: 0,
                    finished: false,
                })),
                #[cfg(windows)]
                Selection::Windows { config } => {
                    let selected = Location::new(&config)?;
                    ensure!(
                        selected.configuration == location.configuration
                            && selected.root == location.root,
                        "notification configuration changed"
                    );
                    Ok(NativeHost::windows(
                        super::windows::runtime::Runtime::start(
                            config,
                            session,
                            endpoint,
                            windows_owner,
                        )
                        .map_err(anyhow::Error::msg)?,
                    ))
                }
            },
        )
    }
}
impl<H: Host> Runtime<H> {
    fn with_factory(
        store: &Store,
        session: Session,
        owner: Arc<dyn Send + Sync>,
        factory: impl FnOnce(Session, Endpoint) -> Result<H>,
    ) -> Result<Self> {
        let (outbox, endpoint) = Outbox::new_owned(session.clone(), owner.clone())?;
        // Fallible store checks precede a possible returned platform worker.
        let through = outbox.restore_live_head(store)?;
        let cleanup_through = outbox.restore_cleanup_head(store)?;
        let host = factory(session, endpoint)?;
        Ok(Self {
            outbox,
            host,
            owner: Some(owner),
            after: 0,
            through,
            cleanup_through,
            cleanup: false,
            page: None,
            restored: false,
            finishing: false,
            stopping: false,
            quiescing: false,
            retirement_started: false,
            retirement_until: None,
            drained: false,
            error: None,
            #[cfg(test)]
            retirement_clock: None,
        })
    }
    fn retirement_now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.retirement_clock {
            return now;
        }
        Instant::now()
    }
    pub fn poll(
        &mut self,
        store: &mut Store,
        attention: &mut attention::Runtime,
    ) -> Vec<Activation> {
        if self.quiescing && !self.retirement_started {
            self.outbox.retire();
            attention.set_policy(attention::Policy::default());
            attention.request_retirement();
            self.retirement_started = true;
        }
        let activations = self.outbox.poll(store, Instant::now());
        self.bound_retirement(attention);
        if self.stopping {
            self.drained = self.host.drained() && self.outbox.operation_drained();
            return activations;
        }
        if let Err(error) = self.turn(store, attention) {
            self.error = Some(format!("{error:#}").chars().take(256).collect());
            self.stop_host();
        }
        self.bound_retirement(attention);
        if self.quiescing && self.restored && !attention.retiring() && self.outbox.operations_idle()
        {
            attention.request_stop();
            self.stop_host();
        }
        if self.stopping {
            self.drained = self.host.drained() && self.outbox.operation_drained();
        }
        activations
    }
    fn bound_retirement(&mut self, attention: &mut attention::Runtime) {
        if self.quiescing && !self.stopping {
            let failed = attention.failed() || self.outbox.error.is_some();
            let expired = self
                .retirement_until
                .is_some_and(|until| self.retirement_now() >= until);
            if failed || expired {
                self.error = Some(
                    if failed {
                        "notification_cleanup_failed"
                    } else {
                        "notification_cleanup_deadline"
                    }
                    .into(),
                );
                attention.request_stop();
                self.stop_host();
            }
        }
    }
    fn turn(&mut self, store: &mut Store, attention: &mut attention::Runtime) -> Result<()> {
        let ready = self.host.ready()?;
        if !self.restored {
            if self.finishing {
                self.restored = self.host.finish()?;
                return Ok(());
            }
            if self.page.is_none() {
                self.page = Some(if self.cleanup {
                    self.outbox
                        .restore_cleanup_page(store, self.after, self.cleanup_through)?
                } else {
                    self.outbox
                        .restore_live_page(store, self.after, self.through)?
                });
            }
            let page = self.page.as_ref().unwrap();
            if !self.host.restore(&page.mappings)? {
                return Ok(());
            }
            self.after = page.after;
            let complete = page.complete;
            self.page = None;
            if complete {
                if self.cleanup {
                    self.finishing = true;
                    self.restored = self.host.finish()?;
                } else {
                    self.cleanup = true;
                    self.after = 0;
                }
            }
            return Ok(());
        }
        if ready || attention.retiring() {
            attention.poll(store, Some(&mut self.outbox));
        }
        Ok(())
    }
    pub fn request_stop(&mut self) {
        self.quiescing = true;
        let now = self.retirement_now();
        self.retirement_until
            .get_or_insert_with(|| now + super::COMMAND_WAIT);
        self.outbox.retire();
    }
    fn stop_host(&mut self) {
        self.stopping = true;
        self.outbox.request_stop();
        self.host.stop();
    }
    pub fn drained(&self) -> bool {
        self.drained
    }
    pub fn fault(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::desktop) fn outbox_mut_for_test(&mut self) -> &mut Outbox {
        &mut self.outbox
    }
    pub fn ready(&self) -> bool {
        self.admitting() && self.host.ready().unwrap_or(false)
    }
    /// Stable restore/lifecycle admission. A temporarily busy status lock can
    /// pause an SDK turn but cannot turn the user's policy off and retire rows.
    pub fn admitting(&self) -> bool {
        self.restored && !self.quiescing && !self.stopping && self.error.is_none()
    }
    pub fn status(&self) -> Value {
        json!({"restored":self.restored,"restore_after":self.after,"restore_through":self.through,"cleanup":self.cleanup,"cleanup_through":self.cleanup_through,
            "stopping":self.quiescing||self.stopping,"withdrawing":self.quiescing&&!self.stopping,
            "native_stop_sent":self.stopping,"drained":self.drained,"error":self.error,
            "outbox":self.outbox.status(),"host":self.host.status()})
    }
}
impl<H: Host> Drop for Runtime<H> {
    fn drop(&mut self) {
        self.stop_host(); // Abandonment is not a completed cleanup/drain.
        self.drained = self.drained || (self.host.drained() && self.outbox.operation_drained());
        if !self.drained {
            // Abandonment proves no drain. Preserve the original owner and
            // admission for process lifetime rather than permit replacement.
            if let Some(owner) = self.owner.take() {
                std::mem::forget(owner);
            }
        }
    }
}

#[cfg(test)]
mod tests;
