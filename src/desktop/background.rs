//! Service-owned upstream lifetime. The actor applies every original event.
use super::{
    attention, delivery, details,
    fanout::Queue,
    feed, fleet,
    notifications::{activation, service as notification_service},
    presence_worker,
    store::{maintenance, Location, Store},
};
use crate::{config::Config, platform};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const POLL_EVENTS: usize = 16;

pub(super) struct Runtime {
    fleet: fleet::Runtime,
    producer: Arc<feed::Producer>,
    presence: Option<presence_worker::Runtime>,
    delivery: delivery::Runtime,
    details: details::Adapter,
    maintenance: maintenance::Runtime,
    cache: super::cache_maintenance::Runtime,
    attention: attention::Runtime,
    activations: activation::Router,
    drained: bool,
    stopping: bool,
    notifications: notification_service::Runtime,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    creation: super::creation_clone::Runtime,
}

/// The actor can replace only an already admitted background runtime after its
/// actual original drain. A display-ready slot or client retry grants nothing.
pub(super) fn replacement_permitted(
    background_mode: bool,
    drained: bool,
    location: &Location,
    store: &Store,
) -> Result<bool> {
    Ok(background_mode && drained && super::revoke::background_permitted(location, store)?)
}

#[cfg(test)]
mod tests;

impl Runtime {
    pub fn start(
        config: &Config,
        epoch: &str,
        broker: &mut feed::Broker,
        owner: Arc<platform::FileLock>,
    ) -> Result<Self> {
        let location = Location::new(config)?;
        let delivery = delivery::Runtime::start(config, epoch)?;
        let maintenance = maintenance::Runtime::start(config, epoch)?;
        let producer = broker.claim_internal()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let creation = super::creation_clone::Runtime::new(config, epoch, owner.clone())?;
        match fleet::Runtime::start(config, epoch) {
            Ok(fleet) => Ok(Self {
                fleet,
                producer,
                presence: None,
                delivery,
                details: details::Adapter::new(epoch),
                maintenance,
                cache: super::cache_maintenance::Runtime::new(&location, epoch, owner.clone()),
                attention: attention::Runtime::new(epoch),
                activations: activation::Router::new(&location.configuration, epoch),
                drained: false,
                stopping: false,
                notifications: notification_service::Runtime::new(config, &location, epoch, owner),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                creation,
            }),
            Err(error) => {
                producer.close();
                Err(error)
            }
        }
    }

    pub fn start_presence(&mut self, location: &Location) -> Result<()> {
        self.presence = Some(presence_worker::Runtime::start(location)?);
        Ok(())
    }

    /// Each original permit is held through the transaction and observation
    /// point. At most sixteen packets are processed per actor turn.
    pub fn poll(
        &mut self,
        store: &mut Store,
        broker: &mut feed::Broker,
        seq: &mut u64,
    ) -> (Vec<Value>, bool) {
        if self.stopping {
            let _ = self.poll_drain(store);
            return (Vec::new(), false);
        }
        let fleet = &self.fleet;
        self.details.tick(
            &mut |computer, generation, controls| fleet.controls(computer, generation, controls),
            seq,
        );
        let mut changes = Vec::new();
        let mut consumed = 0;
        for _ in 0..POLL_EVENTS {
            let Some(event) = self.fleet.poll() else {
                break;
            };
            consumed += 1;
            match broker.apply(store, &self.producer, &event.packet) {
                Ok(change) => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    self.creation.observe(&event);
                    self.delivery.observe(store, &event);
                    self.maintenance.observe(store, &event);
                    if self.attention.enabled() {
                        self.attention.observe(store, &event);
                    }
                    let fleet = &self.fleet;
                    self.details.observe(
                        &event,
                        &mut |computer, generation, controls| {
                            fleet.controls(computer, generation, controls)
                        },
                        seq,
                    );
                    if let Some(change) = change {
                        changes.push(change);
                    }
                }
                Err(_) => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    self.creation.reject(&event);
                    self.maintenance.reject(&event);
                    self.attention.reject_event(&event);
                    self.details.reject(&event, seq);
                    broker.invalidate(&self.producer, &event.packet);
                    self.fleet.reject(&event);
                }
            }
        }
        self.delivery.poll(store);
        self.maintenance.poll(store);
        self.cache.poll(store);
        self.notifications
            .poll(store, &mut self.attention, &mut self.activations, seq);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.creation.poll(store, self.notifications.handoff());
        // Disabled discovery must not accumulate permanent edge/baseline rows.
        // Enabling resets the baseline before later observations may post.
        if !self.notifications.ready() && self.attention.retiring() {
            self.attention.poll(store, None);
        }
        (
            changes,
            consumed == POLL_EVENTS || self.notifications.ready() && self.attention.work_pending(),
        )
    }

    pub fn delivery_request(&mut self, store: &mut Store, value: &Value) -> Value {
        self.delivery.request(store, value)
    }

    pub fn activation_wait(
        &mut self,
        authority: super::ui_authority::Validated,
        queue: Arc<Queue>,
    ) -> Result<()> {
        self.activations.wait(authority, queue)
    }

    pub fn frontend_request(
        &mut self,
        store: &mut Store,
        authority: super::ui_authority::Validated,
    ) -> Value {
        if authority.operation() == "creation_clone" {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            return self
                .creation
                .frontend(store, authority, self.notifications.handoff());
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            return json!({"request_id":authority.id(),"accepted":false,"durable":false,"code":"creation_clone_unsupported"});
        }
        if authority.operation() == "automatic_approvals" {
            return self.delivery.frontend_request(store, authority);
        }
        if authority.operation() == "cache_maintenance" {
            return self.cache.frontend_request(store, authority);
        }
        self.maintenance.frontend_request(store, authority)
    }

    pub fn details_open(&mut self, queue: &Arc<Queue>) -> Option<Value> {
        self.details.open(queue)
    }
    pub fn details_request(&mut self, store: &Store, request: &Value, seq: &mut u64) -> Value {
        let fleet = &self.fleet;
        self.details.request(
            store,
            request,
            &mut |computer, generation, controls| fleet.controls(computer, generation, controls),
            seq,
        )
    }

    pub fn decorate(&self, value: &mut Value) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            value["creation_clone_runtime"] = self.creation.status();
            value["lifecycle"]["background"]["operations"] =
                value["creation_clone_runtime"].clone();
        }
        value["detail_subscriptions"] = self.details.status();
        value["attention_discovery"] = self.attention.status();
        value["activation_transport"] = self.activations.status();
        value["notification_runtime"] = self.notifications.status();
        value["lifecycle"]["background"]["attention"] = json!({
            "ready":self.notifications.ready(),"drained":self.notifications.drained(),
            "code":if self.notifications.drained() {"drained"} else if self.notifications.ready() {"running"} else {"awaiting_native_handoff"},
            "status":value["notification_runtime"]});
        value["maintenance"] = self.maintenance.status();
        value["cache_maintenance"] = self.cache.status();
        value["lifecycle"]["background"]["cache_maintenance"] = self.cache.status();
        value["components"]["maintenance"] = if value["maintenance"]["ready"] == true {
            "running"
        } else {
            "unavailable"
        }
        .into();
        value["lifecycle"]["background"]["maintenance"] = value["maintenance"].clone();
        let status = self.fleet.status();
        let ready = status["transport_ready"] == true;
        let code = match status["state"].as_str() {
            Some("drained") => "drained",
            Some("stopping") => "draining",
            _ if ready => "running",
            _ => "starting_or_degraded",
        };
        let presence = self.presence.as_ref().map(presence_worker::Runtime::status);
        let delivery = self.delivery.status();
        self.delivery.decorate(value);
        value["components"]["reads"] = value["lifecycle"]["background"]["reads"]["code"].clone();
        value["components"]["approvals"] =
            value["lifecycle"]["background"]["approvals"]["code"].clone();
        value["lifecycle"]["background"]["delivery"] = delivery.clone();
        value["delivery_runtime"] = delivery.clone();
        value["ownership"] = if code == "drained"
            && delivery["drained"] == true
            && self.maintenance.drained()
            && self.cache.drained()
            && self.attention.drained()
            && presence.as_ref().is_none_or(|p| p["drained"] == true)
        {
            "revoked"
        } else if presence.is_some() {
            "background_service"
        } else {
            "background_readonly"
        }
        .into();
        value["upstreams"] = status["computers"].clone();
        value["background_runtime"] = status.clone();
        value["components"]["fleet"] = code.into();
        value["lifecycle"]["background"]["fleet"] = json!({"ready":ready,
            "code":code,"status":status});
        if let Some(presence) = presence {
            value["writers_enabled"] = presence["writer_active"].clone();
            value["components"]["presence"] = presence["code"].clone();
            value["lifecycle"]["background"]["presence"] = presence;
        }
    }

    pub fn handoff(&self) -> Option<Arc<super::store::HandoffLock>> {
        self.notifications.handoff()
    }
    pub fn notification_controls(
        &mut self,
        preferences: zerus_client_core::desktop::notification_control::Preferences,
        foreground_until: Option<Instant>,
    ) {
        self.notifications.set_preferences(preferences);
        self.attention.set_foreground_until(foreground_until);
    }
    pub fn publication_owner(
        &self,
        store: &Store,
    ) -> Result<Option<Arc<super::notifications::preparation::Owner>>> {
        if self.stopping || self.drained {
            return Ok(None);
        }
        self.notifications.publication_owner(store)
    }
    pub fn revocation_epoch(&self, fallback: &str) -> String {
        self.notifications
            .writer_epoch()
            .unwrap_or(fallback)
            .to_owned()
    }

    pub fn drained(&self) -> bool {
        self.drained
    }

    pub fn drain(&mut self, store: &mut Store, timeout: Duration) -> Result<()> {
        self.stopping = true;
        self.activations.stop();
        self.details.stop();
        self.producer.close();
        self.delivery.request_stop();
        self.maintenance.request_stop();
        // Retain the error until every component has received its stop. An
        // unavailable cache database must not leave other owners admitting work.
        let cache_stop = self.cache.request_stop(store);
        self.attention.set_policy(attention::Policy::default());
        self.notifications.request_stop();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.creation.request_stop();
        if let Some(presence) = &self.presence {
            presence.request_stop();
        }
        cache_stop?;
        let end = Instant::now() + timeout;
        loop {
            if self.poll_drain(store)? {
                return Ok(());
            }
            ensure!(
                Instant::now() < end,
                "desktop background shutdown is incomplete; owned workers have not drained"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    /// Used both by the bounded takeover wait and subsequent actor turns. A
    /// slow replacement continues draining without another frontend request.
    fn poll_drain(&mut self, store: &mut Store) -> Result<bool> {
        let fleet_done = self.fleet.stop();
        self.delivery.poll(store);
        self.maintenance.poll(store);
        self.cache.poll(store);
        let mut seq = 0;
        self.notifications
            .poll(store, &mut self.attention, &mut self.activations, &mut seq);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.creation.poll(store, self.notifications.handoff());
        if self.notifications.drained() {
            self.attention.request_stop();
        }
        let presence_done = match self.presence.as_mut().map(presence_worker::Runtime::stop) {
            None | Some(Ok(true)) => true,
            Some(Ok(false)) => false,
            Some(Err(error)) => return Err(error),
        };
        self.drained = fleet_done
            && presence_done
            && self.delivery.drained()
            && self.maintenance.drained()
            && self.cache.drained()
            && self.attention.drained()
            && self.notifications.drained();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.drained = self.drained && self.creation.drained();
        }
        Ok(self.drained)
    }
}
