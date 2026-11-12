//! The live admission of one original, and the authority it was admitted
//! under. Neither a database row, a display readiness boolean nor a
//! notification grant constructs it; restart and import have no live value to
//! borrow.
//!
//! Two authorities exist and never stand in for each other:
//! - [`Authority::PersonConsent`]: a person's deferred send, admitted only by
//!   their native UI permit for a saved draft checkpoint
//!   ([`super::Runtime::frontend`]).
//! - [`Authority::OwnerApprovedRoute`]: the owner-approved, service-originated
//!   route of a supervised child's blocking question to its live parent
//!   (zerus-sa9qn.8, [`super::Runtime::route`]). It is the owner's standing
//!   approval for exactly that fixed send, never a person's consent, and it
//!   is admitted with no UI permit.
use super::ledger;
use crate::desktop::creation_clone::lease::Lease;
use anyhow::{ensure, Context, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use zerus_client_core::desktop::deferred_send::{Kind, Verified};
use zerus_desktop_ledger::operations::Ticket;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Authority {
    /// The person's own deferred send.
    PersonConsent,
    /// The owner-approved, service-originated question route.
    OwnerApprovedRoute,
}
impl Authority {
    /// The one original kind each authority admits.
    fn kind(self) -> Kind {
        match self {
            Self::PersonConsent => Kind::DeferredSend,
            Self::OwnerApprovedRoute => Kind::RouteQuestion,
        }
    }
}

pub(super) struct Admitted {
    pub original: Verified,
    pub lease: Arc<Lease>,
    pub cancel: AtomicBool,
    pub authority: Authority,
}
impl Admitted {
    /// The person's consent, from their native UI permit only.
    pub fn person_consent(original: Verified, lease: Arc<Lease>) -> Result<Arc<Self>> {
        Self::new(original, lease, Authority::PersonConsent)
    }
    /// The owner-approved route, from the service's own route admission only.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn owner_approved_route(original: Verified, lease: Arc<Lease>) -> Result<Arc<Self>> {
        Self::new(original, lease, Authority::OwnerApprovedRoute)
    }
    fn new(original: Verified, lease: Arc<Lease>, authority: Authority) -> Result<Arc<Self>> {
        // Each authority admits only its own kind.
        ensure!(
            original.binding.kind == authority.kind(),
            "deferred send authority does not admit this kind"
        );
        // The send names a local session; the lease's native source still
        // binds this service and computer.
        ensure!(
            original.intent.scope == lease.scope,
            "deferred send target changed"
        );
        Ok(Arc::new(Self {
            original,
            lease,
            cancel: AtomicBool::new(false),
            authority,
        }))
    }
    pub fn check(&self, row: &ledger::Row, ticket: Option<&Ticket>) -> Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire)
                && self.lease.alive()
                && row.verified.binding.kind == self.authority.kind()
                && self.original.binding == row.verified.binding
                && self.original.plan == row.verified.plan
                && self.original.intent.original == row.verified.intent.original
                && self.original.intent.checkpoint == row.verified.intent.checkpoint
                && self.lease.scope == row.verified.intent.scope
                && self.lease.writer_epoch == row.writer_epoch,
            "deferred send original/admission changed"
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
                            .context("invalid send binding")?,
                "deferred send reservation changed"
            );
        }
        Ok(())
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
