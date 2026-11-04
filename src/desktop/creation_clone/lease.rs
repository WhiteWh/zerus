//! Live guard for one narrow effect profile. Neither a database row, a display
//! readiness boolean nor a notification grant constructs execution consent.
use crate::{
    desktop::{
        feed, fleet,
        store::{HandoffLock, Location},
    },
    platform,
};
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use zerus_client_core::desktop::creation_clone::{Scope, Verified};
use zerus_desktop_ledger::operations::Ticket;
use zerus_watch_protocol::Body;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Machine {
    generation: u64,
    stream: String,
    pub id: String,
}
impl Machine {
    #[cfg(test)]
    pub(super) fn fixture(id: &str) -> Self {
        Self {
            generation: 1,
            stream: "actor-fixture".into(),
            id: id.into(),
        }
    }
    /// The caller supplies only an original FleetEvent after owned Broker apply.
    /// A generic source packet, caption, count or cached GUI row never calls it.
    pub fn observe(current: &mut Option<Self>, event: &fleet::FleetEvent) {
        let Some((computer, generation)) = event.packet.scope() else {
            return;
        };
        if !computer.is_empty() || current.as_ref().is_some_and(|v| generation < v.generation) {
            return;
        }
        match &event.packet {
            feed::Packet::Watch { frame, .. } => match &frame.body {
                Body::Hello(hello) => {
                    *current = hello
                        .machine_id
                        .as_ref()
                        .filter(|id| {
                            uuid::Uuid::parse_str(id)
                                .is_ok_and(|v| !v.is_nil() && v.to_string() == **id)
                        })
                        .filter(|_| {
                            !hello.stream.is_empty()
                                && hello.stream.len() <= 128
                                && !hello.stream.chars().any(char::is_control)
                        })
                        .map(|id| Self {
                            generation,
                            stream: hello.stream.clone(),
                            id: id.clone(),
                        });
                }
                _ if current.as_ref().is_some_and(|v| v.generation != generation) => {
                    *current = None
                }
                _ => (),
            },
            feed::Packet::Offline { .. }
            | feed::Packet::Remove { .. }
            | feed::Packet::LegacyBegin { .. } => *current = None,
            _ => (),
        }
    }
    pub fn reject(current: &mut Option<Self>, event: &fleet::FleetEvent) {
        if event.packet.scope().is_some_and(|(computer, generation)| {
            computer.is_empty() && current.as_ref().is_none_or(|v| generation >= v.generation)
        }) {
            *current = None;
        }
    }
}

pub(super) struct Lease {
    pub id: String,
    pub writer_epoch: String,
    pub scope: Scope,
    machine: Machine,
    alive: AtomicBool,
    // Real original exclusions, retained through work and every original pipe.
    _owner: Arc<platform::FileLock>,
    exclusion: Arc<HandoffLock>,
}
fn writer(db: &Connection, scope: &Scope) -> Result<Option<String>> {
    let service: (String, String) = db.query_row(
        "SELECT configuration,epoch FROM service WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        service == (scope.configuration.clone(), scope.service_epoch.clone()),
        "creation service scope changed"
    );
    let row: Option<(Option<String>, String, bool)> = db
        .query_row(
            "SELECT epoch,service_epoch,revoked FROM desktop_writer WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.and_then(|(epoch, service, revoked)| {
        epoch.filter(|epoch| {
            !revoked
                && service == scope.service_epoch
                && uuid::Uuid::parse_str(epoch)
                    .is_ok_and(|v| !v.is_nil() && v.to_string() == *epoch)
        })
    }))
}
impl Lease {
    pub fn new(
        db: &Connection,
        scope: Scope,
        location: &Location,
        machine: Machine,
        owner: Arc<platform::FileLock>,
        exclusion: Arc<HandoffLock>,
    ) -> Result<Arc<Self>> {
        ensure!(
            scope.valid() && scope.configuration == location.configuration,
            "invalid creation scope"
        );
        exclusion.verify(location)?;
        let writer_epoch = writer(db, &scope)?.context("creation writer unavailable")?;
        Ok(Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            writer_epoch,
            scope,
            machine,
            alive: AtomicBool::new(true),
            _owner: owner,
            exclusion,
        }))
    }
    pub fn current(
        &self,
        db: &Connection,
        location: &Location,
        machine: Option<&Machine>,
        exclusion: Option<&Arc<HandoffLock>>,
    ) -> Result<()> {
        ensure!(
            self.alive.load(Ordering::Acquire)
                && machine == Some(&self.machine)
                && exclusion.is_some_and(|v| Arc::ptr_eq(v, &self.exclusion)),
            "creation exclusion/source changed"
        );
        self.exclusion.verify(location)?;
        ensure!(
            writer(db, &self.scope)?.as_deref() == Some(self.writer_epoch.as_str()),
            "creation writer revoked"
        );
        Ok(())
    }
    pub fn invalidate(&self) {
        self.alive.store(false, Ordering::Release);
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.invalidate();
    }
}

/// Minted only after opaque native admission commits in this running actor.
/// Restart/import has no live value to borrow and cannot reconstruct one from
/// serialized plans or consent hashes. GUI liveness is not checked afterwards.
pub(super) struct Consent {
    pub original: Verified,
    pub lease: Arc<Lease>,
    pub cancel: AtomicBool,
}
impl Consent {
    pub fn admitted(original: Verified, lease: Arc<Lease>) -> Result<Arc<Self>> {
        ensure!(
            original.intent.scope == lease.scope
                && original.plan.expected_machine == lease.machine.id,
            "creation target changed"
        );
        Ok(Arc::new(Self {
            original,
            lease,
            cancel: AtomicBool::new(false),
        }))
    }
    pub fn check(&self, row: &super::ledger::Row, ticket: Option<&Ticket>) -> Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire)
                && self.lease.alive.load(Ordering::Acquire)
                && self.original.binding == row.verified.binding
                && self.original.plan == row.verified.plan
                && self.original.intent.original == row.verified.intent.original
                && self.lease.scope == row.verified.intent.scope
                && self.lease.writer_epoch == row.writer_epoch,
            "creation original/consent changed"
        );
        if let Some(ticket) = ticket {
            ticket.fingerprint()?;
            ensure!(
                row.ticket.as_ref() == Some(ticket)
                    && ticket.lease == self.lease.id
                    && ticket.binding_sha256
                        == self
                            .original
                            .binding
                            .fingerprint()
                            .context("invalid clone binding")?,
                "creation reservation changed"
            );
        }
        Ok(())
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
