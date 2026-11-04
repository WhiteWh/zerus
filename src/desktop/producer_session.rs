//! Dormant off-actor identity handshake. No role, lease or effect admission.
use super::{producer_binding::Attempt, producer_binding::Matched, producer_io::Channel};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::BufReader,
    path::Path,
    sync::{Arc, Mutex},
};
use zerus_client_core::desktop::{producer::Challenge, Hello};

#[derive(Default)]
struct State {
    occupied: bool,
    closed: bool,
}

/// Construct once in the future sole configuration owner. No global runtime or
/// production caller installs it yet; a second Registry is not another permit.
pub(super) struct Registry {
    configuration: String,
    service_epoch: String,
    state: Arc<Mutex<State>>,
}

struct Reservation(Arc<Mutex<State>>);
impl Reservation {
    fn check(&self) -> Result<()> {
        let state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("producer registry unavailable"))?;
        ensure!(
            !state.closed && state.occupied,
            "producer registry is closed"
        );
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        // Poison remains unavailable; never recover or manufacture a free slot.
        if let Ok(mut state) = self.0.lock() {
            state.occupied = false;
        }
    }
}

/// Declaration order matters: actual channel/reader/FD destruction precedes
/// releasing the slot, including errors, panic and a dropped outer receiver.
pub(super) struct Pending {
    channel: Channel,
    reservation: Reservation,
    attempt: Option<Attempt>,
    expected: Hello,
}

/// Association at bind time only. Retains the original FD and buffered tail.
/// It has no currentness, drain, renewal, reconnect or effect API.
pub(super) struct Session {
    _channel: Channel,
    _reservation: Reservation,
    identity: Matched,
}
impl Session {
    pub fn identity(&self) -> &Matched {
        &self.identity
    }
}

#[derive(Serialize)]
struct Offer<'a> {
    abi: &'static str,
    version: u32,
    op: &'static str,
    sequence: u64,
    configuration: &'a str,
    expected: &'a Hello,
    challenge: &'a Challenge,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    abi: String,
    version: u32,
    op: String,
    sequence: u64,
    configuration: String,
    instance_epoch: String,
    challenge: Challenge,
}

impl Registry {
    pub fn new(configuration: String, service_epoch: String) -> Result<Self> {
        ensure!(
            zerus_client_core::desktop::digest(&configuration),
            "invalid producer configuration"
        );
        ensure!(
            uuid::Uuid::parse_str(&service_epoch)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == service_epoch),
            "invalid producer service epoch"
        );
        Ok(Self {
            configuration,
            service_epoch,
            state: Arc::default(),
        })
    }

    /// Reserve before handing Pending to an owned blocking worker. An outer wait
    /// must never own/drop the worker's Pending or Session on its behalf.
    pub fn begin(
        &self,
        reader: BufReader<super::transport::Stream>,
        expected: Hello,
    ) -> Result<Pending> {
        let reservation = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("producer registry unavailable"))?;
            ensure!(
                !state.closed && !state.occupied,
                "producer registry is closed or occupied"
            );
            state.occupied = true;
            Reservation(self.state.clone())
        };
        // These locals drop in reverse order on refusal: FD before reservation.
        let channel = Channel::new(reader)?;
        let attempt = Attempt::new(&self.configuration, &self.service_epoch, expected.clone())
            .map_err(|_| anyhow::anyhow!("invalid producer identity"))?;
        Ok(Pending {
            channel,
            reservation,
            attempt: Some(attempt),
            expected,
        })
    }

    /// Sticky admission fence only. It neither cancels a blocking worker nor
    /// proves its actual connection/resources ended. Existing holders stay held.
    pub fn close(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("producer registry unavailable"))?
            .closed = true;
        Ok(())
    }
}

impl Pending {
    /// The future host supplies its independently resolved canonical endpoint.
    /// No listener, resolver, thread, FD clone or production caller is added.
    pub fn bind(mut self, path: &Path) -> Result<Session> {
        self.reservation.check()?;
        let attempt = self.attempt.as_ref().context("producer attempt consumed")?;
        let deadline = attempt.deadline();
        let mut offer = serde_json::to_vec(&Offer {
            abi: zerus_client_core::desktop::producer::ABI,
            version: 1,
            op: "challenge",
            sequence: 1,
            configuration: self
                .expected
                .configuration
                .as_deref()
                .context("missing producer configuration")?,
            expected: &self.expected,
            challenge: attempt.challenge(),
        })?;
        offer.push(b'\n');
        self.channel.write_line(&offer, deadline)?;
        let bytes = self
            .channel
            .line(deadline)?
            .context("producer connection ended")?;
        let ready: Ready = serde_json::from_slice(&bytes).context("invalid producer readiness")?;
        ensure!(
            ready.abi == zerus_client_core::desktop::producer::ABI
                && ready.version == 1
                && ready.op == "ready"
                && ready.sequence == 1
                && Some(ready.configuration.as_str()) == self.expected.configuration.as_deref()
                && ready.instance_epoch == self.expected.instance_epoch
                && ready.challenge == *attempt.challenge(),
            "producer readiness mismatch"
        );
        self.reservation.check()?;
        // The original Attempt checks its fixed deadline before and after the
        // existing one-second canonical same-user probe. Ready alone binds nothing.
        let identity = self
            .attempt
            .take()
            .context("producer attempt consumed")?
            .probe_at(path)
            .map_err(|_| anyhow::anyhow!("canonical producer binding refused"))?;
        self.reservation.check()?;
        Ok(Session {
            _channel: self.channel,
            _reservation: self.reservation,
            identity,
        })
    }
}

macro_rules! redacted {
    ($($kind:ty),+) => { $(impl std::fmt::Debug for $kind {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($kind), "([producer association])"))
        }
    })+ };
}
redacted!(Registry, Pending, Session);

#[cfg(all(test, unix))]
mod tests;
