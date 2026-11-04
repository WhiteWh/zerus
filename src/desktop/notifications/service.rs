//! The sole actor binds a durable native grant to the actual serve owner and
//! one original native host. Readiness/rosters and caller JSON never construct it.
use super::{activation, attachment, preparation, wire};
use crate::{
    config::Config,
    desktop::{
        attention, ownership,
        store::{HandoffLock, Location, Store},
    },
    platform,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];

pub(in crate::desktop) struct Runtime {
    config: Config,
    location: Location,
    epoch: String,
    service: Arc<platform::FileLock>,
    grant: Option<ownership::Grant>,
    owner: Option<Arc<preparation::Owner>>,
    preparing: Option<preparation::Preparation>,
    native: Option<attachment::Runtime>,
    attempted: bool,
    stopping: bool,
    drained: bool,
    error: Option<String>,
    recovering: bool,
    retry_at: Option<Instant>,
    failures: usize,
    preferences: zerus_client_core::desktop::notification_control::Preferences,
    #[cfg(all(test, target_os = "linux"))]
    test_bus: Option<String>,
    #[cfg(target_os = "linux")]
    closed: super::closed::Runtime,
}
impl Runtime {
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::desktop) fn retry_now_for_test(&mut self) {
        assert!(
            self.retry_at.is_some(),
            "original resources must drain before a retry exists"
        );
        self.retry_at = Some(Instant::now());
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::desktop) fn hold_owner_for_test(&self) -> Arc<dyn Send + Sync> {
        self.owner.as_ref().unwrap().clone()
    }
    pub fn new(
        config: &Config,
        location: &Location,
        epoch: &str,
        service: Arc<platform::FileLock>,
    ) -> Self {
        Self {
            config: config.clone(),
            location: Location {
                root: location.root.clone(),
                configuration: location.configuration.clone(),
            },
            epoch: epoch.into(),
            service,
            grant: None,
            owner: None,
            preparing: None,
            native: None,
            attempted: false,
            stopping: false,
            drained: false,
            error: None,
            recovering: false,
            retry_at: None,
            failures: 0,
            preferences: Default::default(),
            #[cfg(all(test, target_os = "linux"))]
            test_bus: None,
            #[cfg(target_os = "linux")]
            closed: super::closed::Runtime::new(config, location),
        }
    }
    pub fn handoff(&self) -> Option<Arc<HandoffLock>> {
        self.owner.as_ref().map(|o| o.handoff())
    }
    pub fn set_preferences(
        &mut self,
        preferences: zerus_client_core::desktop::notification_control::Preferences,
    ) {
        self.preferences = preferences;
    }
    /// Actor-only loan of this original owner, not a serialized role/lease or
    /// a new writer grant. Its strong resource count also fences actual drain.
    pub fn publication_owner(&self, store: &Store) -> Result<Option<Arc<preparation::Owner>>> {
        if self.stopping || self.drained || self.recovering {
            return Ok(None);
        }
        let Some(grant) = &self.grant else {
            return Ok(None);
        };
        if !grant.current(store)? {
            return Ok(None);
        }
        Ok(self.owner.clone())
    }
    pub fn writer_epoch(&self) -> Option<&str> {
        self.grant.as_ref().map(ownership::Grant::writer_epoch)
    }
    fn begin(&mut self, store: &mut Store) -> Result<()> {
        #[cfg(all(test, target_os = "linux"))]
        if let Some(address) = self.test_bus.clone() {
            return self.begin_with(store, |_, location, grant, service| {
                preparation::Preparation::selected_for_test(
                    service,
                    location,
                    grant,
                    attachment::Selection::Linux { address },
                )
            });
        }
        self.begin_with(store, preparation::Preparation::start)
    }
    fn begin_with(
        &mut self,
        store: &mut Store,
        select: impl FnOnce(
            &Config,
            &Location,
            ownership::Grant,
            Arc<platform::FileLock>,
        ) -> Result<preparation::Preparation>,
    ) -> Result<()> {
        let Some(grant) = ownership::grant(store, &self.location.configuration)? else {
            return Ok(());
        };
        ensure!(grant.current(store)?, "notification ownership changed");
        self.attempted = true;
        let preparation = select(
            &self.config,
            &self.location,
            grant.clone(),
            self.service.clone(),
        )?;
        self.owner = preparation.owner();
        self.preparing = Some(preparation);
        self.grant = Some(grant);
        // Both original OS exclusions are now held. The notification SDK is
        // independent of read/approval readiness; failure to select a native
        // host cannot revoke their separately configured service work.
        ensure!(
            store.operation_undrained()? == 0,
            "original operation resources remain owned"
        );
        ensure!(
            self.grant.as_ref().unwrap().current(store)?,
            "notification grant changed"
        );
        zerus_desktop_ledger::authorize(
            &mut store.db,
            self.grant.as_ref().unwrap().writer_epoch(),
            &self.epoch,
        )?;
        Ok(())
    }
    fn attach(&mut self, store: &Store, prepared: preparation::Prepared) -> Result<()> {
        ensure!(prepared.grant.current(store)?, "prepared ownership changed");
        let session = wire::Session {
            configuration: self.location.configuration.clone(),
            service_epoch: self.epoch.clone(),
            writer_epoch: prepared.grant.writer_epoch().into(),
            adapter: id(),
            host_epoch: id(),
            lease: id(),
            platform: if cfg!(target_os = "linux") {
                wire::Platform::Linux
            } else if cfg!(target_os = "macos") {
                wire::Platform::Macos
            } else {
                wire::Platform::Windows
            },
        };
        let owner = self
            .owner
            .as_ref()
            .context("original notification owner lost")?
            .clone();
        self.native = Some(attachment::Runtime::start(
            &self.location,
            store,
            session,
            prepared.selection,
            owner,
        )?);
        Ok(())
    }
    pub fn poll(
        &mut self,
        store: &mut Store,
        attention: &mut attention::Runtime,
        activations: &mut activation::Router,
        seq: &mut u64,
    ) {
        if !self.stopping
            && self
                .grant
                .as_ref()
                .is_some_and(|grant| !grant.current(store).unwrap_or(false))
        {
            self.request_stop();
        }
        if !self.stopping && !self.recovering && !self.attempted {
            if let Err(error) = self.begin(store) {
                self.fail(error);
            }
        }
        let selected = self
            .preparing
            .as_mut()
            .and_then(preparation::Preparation::poll);
        if let Some(selected) = selected {
            self.preparing = None; // Original worker was joined by poll().
            if !self.stopping && !self.recovering {
                if let Err(error) = selected.and_then(|prepared| self.attach(store, prepared)) {
                    self.fail(error);
                }
            }
        }
        let mut fault = None;
        if let Some(native) = &mut self.native {
            // Native readiness can briefly be unknown (for example a busy
            // status mutex). That pauses dispatch, not the ownership policy.
            // A transport outage pauses discovery effects; it is not a user
            // policy change and cannot retire all previously shown mappings.
            if self.stopping || native.admitting() && !self.recovering {
                attention.set_policy(attention::Policy {
                    enabled: !self.stopping,
                    optional_questions: !self.stopping && self.preferences.optional_questions,
                });
            }
            let callbacks = native.poll(store, attention);
            for value in callbacks {
                if !self.stopping && !self.recovering && native.fault().is_none() {
                    if let Err(error) = activations.offer(store, native.outbox(), value) {
                        self.error = Some(bounded(error));
                    }
                }
            }
            activations.poll(
                store,
                (!self.recovering && native.fault().is_none()).then_some(native.outbox()),
                seq,
            );
            #[cfg(target_os = "linux")]
            if !self.stopping && !self.recovering && native.ready() {
                if let (Some(grant), Some(owner)) = (&self.grant, &self.owner) {
                    self.closed
                        .poll(store, grant, owner, native.outbox(), activations);
                }
            }
            fault = native.fault().map(str::to_owned);
            if native.ready() && !self.recovering {
                self.error = None;
                self.failures = 0;
            }
        } else {
            activations.poll(store, None, seq);
        }
        if !self.stopping && !self.recovering {
            if let Some(fault) = fault {
                self.fail(anyhow::anyhow!(fault));
            }
        }
        if self.recovering && !self.stopping {
            self.poll_recovery(store);
        }
        if self.stopping {
            attention.set_policy(attention::Policy {
                enabled: false,
                optional_questions: false,
            });
            let selected_done = self
                .preparing
                .as_ref()
                .is_none_or(preparation::Preparation::drained);
            let native_done = self
                .native
                .as_ref()
                .is_none_or(attachment::Runtime::drained);
            #[cfg(target_os = "linux")]
            let closed_done = self.closed.drained();
            #[cfg(not(target_os = "linux"))]
            let closed_done = true;
            if selected_done && native_done && closed_done {
                self.native = None;
                self.preparing = None;
                // An uncertain help child may retain this same owner after
                // its launch worker returned. Its real resource charge wins.
                if self
                    .owner
                    .as_ref()
                    .is_none_or(|owner| Arc::strong_count(owner) == 1)
                {
                    self.owner = None;
                    self.drained = true;
                }
            }
        }
    }
    fn fail(&mut self, error: anyhow::Error) {
        self.error = Some(bounded(error));
        self.recovering = true;
        self.retry_at = None;
        #[cfg(target_os = "linux")]
        self.closed.stop();
        if let Some(preparing) = &self.preparing {
            preparing.stop();
        }
        // A failed attachment has already stopped its original host. It must
        // keep polling the same handles; no retry is admitted by this error.
    }
    fn poll_recovery(&mut self, store: &mut Store) {
        if self.retry_at.is_none() {
            let selected_done = self
                .preparing
                .as_ref()
                .is_none_or(preparation::Preparation::drained);
            let native_done = self
                .native
                .as_ref()
                .is_none_or(attachment::Runtime::drained);
            #[cfg(target_os = "linux")]
            let closed_done = self.closed.drained();
            #[cfg(not(target_os = "linux"))]
            let closed_done = true;
            if !selected_done || !native_done || !closed_done {
                return;
            }
            self.native = None;
            self.preparing = None;
            if self
                .owner
                .as_ref()
                .is_some_and(|owner| Arc::strong_count(owner) != 1)
            {
                return;
            }
            self.owner = None;
            let seconds = BACKOFF[self.failures.min(BACKOFF.len() - 1)];
            self.failures = self.failures.saturating_add(1);
            self.retry_at = Some(Instant::now() + Duration::from_secs(seconds));
        }
        if self.retry_at.is_some_and(|at| Instant::now() >= at) {
            // Re-read the grant and both exclusions. A timer never re-arms a
            // revoked/native-owned configuration or replays a ledger effect.
            self.retry_at = None;
            self.recovering = false;
            self.attempted = false;
            #[cfg(target_os = "linux")]
            {
                self.closed = super::closed::Runtime::new(&self.config, &self.location);
            }
            if let Err(error) = self.begin(store) {
                self.fail(error);
            }
        }
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.recovering = false;
        self.error = None;
        self.retry_at = None;
        #[cfg(target_os = "linux")]
        self.closed.stop();
        if let Some(preparing) = &self.preparing {
            preparing.stop();
        }
        if let Some(native) = &mut self.native {
            native.request_stop();
        }
    }
    pub fn drained(&self) -> bool {
        self.drained
    }
    pub fn ready(&self) -> bool {
        !self.stopping
            && !self.recovering
            && self.error.is_none()
            && self.native.as_ref().is_some_and(attachment::Runtime::ready)
    }
    pub fn status(&self) -> Value {
        let value = json!({"ready":self.ready(),"stopping":self.stopping,"drained":self.drained,"attempted":self.attempted,"error":self.error,
        "recovering":self.recovering,"retry_in_ms":self.retry_at.map(|at| at.saturating_duration_since(Instant::now()).as_millis() as u64),
        "failed_attempts":self.failures,
        "selection":self.preparing.as_ref().map(preparation::Preparation::status),"attachment":self.native.as_ref().map(attachment::Runtime::status)});
        #[cfg(target_os = "linux")]
        let value = {
            let mut value = value;
            value["closed_frontend"] = self.closed.status();
            value
        };
        value
    }
}

#[cfg(test)]
impl Runtime {
    #[cfg(target_os = "linux")]
    pub(in crate::desktop) fn begin_on_test_bus(&mut self, store: &mut Store, address: String) {
        self.test_bus = Some(address.clone());
        self.begin_with(store, |_, location, grant, service| {
            preparation::Preparation::selected_for_test(
                service,
                location,
                grant,
                super::attachment::Selection::Linux { address },
            )
        })
        .unwrap();
    }
    #[cfg(target_os = "linux")]
    pub(in crate::desktop) fn offer_for_test(&mut self, store: &mut Store, intent: &str) -> String {
        let super::Offered::Queued { request_id } = self
            .native
            .as_mut()
            .unwrap()
            .outbox_mut_for_test()
            .offer_intent(store, intent, 1, std::time::Instant::now())
            .unwrap()
        else {
            panic!("fixture post was already consumed")
        };
        request_id
    }
    pub(in crate::desktop) fn block_selection_for_test(
        &mut self,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        let preparing = preparation::Preparation::blocked_for_test(
            self.service.clone(),
            &self.location,
            release,
        );
        self.owner = preparing.owner();
        self.preparing = Some(preparing);
        self.attempted = true;
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
        if !self.drained && (self.native.is_some() || self.preparing.is_some()) {
            if let Some(owner) = self.owner.take() {
                std::mem::forget(owner);
            }
        }
    }
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn bounded(error: anyhow::Error) -> String {
    format!("{error:#}").chars().take(256).collect()
}
