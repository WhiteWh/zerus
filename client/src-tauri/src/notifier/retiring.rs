//! Local input retirement for admitted agent work. No SDK/controller caller.
//! Prepared owns the exact original batch; it is not a native lease or receipt.
#![allow(dead_code)]
use super::{AccountingRefused, Center, Completion, Control, Failed, Hop, Notifier, Resources};
use zerus_client_core::{
    attention::Notice,
    desktop::drain::{Charge, Closed, Gate, Role},
    native_notice::Owner,
};

pub(crate) const QUEUED: usize = 64;
pub(crate) const ITEMS: usize = 128;
pub(crate) const BYTES: usize = 64 << 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Unsupported,
    Closed,
    Pressure,
    TooLarge,
    TokenOwner,
    Unavailable,
}
/// Private source data moves once; no serialization or duplicate pending cache.
pub(crate) struct Batch {
    pub cleared: Vec<String>,
    pub raised: Vec<Notice>,
}
impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Batch { .. }")
    }
}
#[derive(Debug)]
pub(crate) struct Rejected {
    pub reason: Refused,
    pub batch: Batch,
}
impl Batch {
    fn checked(&self) -> Result<(), Refused> {
        if self.cleared.len().saturating_add(self.raised.len()) > ITEMS {
            return Err(Refused::TooLarge);
        }
        let mut bytes = 0usize;
        for part in self.cleared.iter().map(String::as_str).chain(
            self.raised
                .iter()
                .flat_map(|n| [&*n.token, &*n.title, &*n.body]),
        ) {
            bytes = bytes
                .checked_add(part.len())
                .filter(|n| *n <= BYTES)
                .ok_or(Refused::TooLarge)?;
        }
        if self
            .cleared
            .iter()
            .map(String::as_str)
            .chain(self.raised.iter().map(|n| n.token.as_str()))
            .any(|t| !Owner::AgentAttention.accepts_token(t))
        {
            return Err(Refused::TokenOwner);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Retiring {
    notifier: Notifier,
    queued: Gate,
}
#[derive(Clone)]
pub(crate) struct InputControl {
    native: Control,
    queued: Gate,
}
/// One use, nonserializing, and charged before entering the host's existing
/// main-thread queue. The payload drops before the queue reservation.
pub(crate) struct Prepared {
    notifier: Notifier,
    batch: Batch,
    work: Option<Completion>,
    _queued: Charge,
}
impl Retiring {
    pub fn new_accounted(
        center: Box<dyn Center>,
        hop: Hop,
        failed: Failed,
        charge: Charge,
    ) -> Result<(Self, InputControl), AccountingRefused> {
        let (resources, native) = Resources::new(charge)?;
        let notifier = Notifier::with_resources(
            center,
            hop,
            failed,
            Some(resources),
            super::CloseMode::FinishAdmitted,
        );
        let queued = Gate::new(QUEUED).expect("fixed preparation bound");
        Ok((
            Self {
                notifier,
                queued: queued.clone(),
            },
            InputControl { native, queued },
        ))
    }
    /// Refusal returns the unchanged original batch, before any native effect.
    /// Capacity counts preparations still queued/running, not historical notices.
    pub fn prepare(&self, batch: Batch) -> Result<Prepared, Rejected> {
        let admit = || {
            batch.checked()?;
            let queued = self
                .queued
                .admit(Role::NativePosting)
                .map_err(|error| match error {
                    zerus_client_core::desktop::drain::Refused::Closed => Refused::Closed,
                    zerus_client_core::desktop::drain::Refused::Capacity => Refused::Pressure,
                    _ => Refused::Unavailable,
                })?;
            let work = self
                .notifier
                .0
                .begin()
                .map_err(|error| match error {
                    AccountingRefused::Closed => Refused::Closed,
                    _ => Refused::Unavailable,
                })?
                .ok_or(Refused::Unavailable)?;
            Ok((queued, work))
        };
        match admit() {
            Ok((queued, work)) => Ok(Prepared {
                notifier: self.notifier.clone(),
                batch,
                work: Some(work),
                _queued: queued,
            }),
            Err(reason) => Err(Rejected { reason, batch }),
        }
    }
}
impl InputControl {
    /// Close new preparations before dropping the parent source charge. Already
    /// admitted continuations retain their original scope and may settle/save.
    /// The host must ALSO close/wait its supplied NativePosting Gate; this
    /// returned gate covers queued payloads only. Both waits use one deadline.
    pub fn close(&self) -> Result<Closed, Refused> {
        if !cfg!(unix) {
            return Err(Refused::Unsupported);
        }
        let queued = self.queued.close();
        self.native.close().map_err(|_| Refused::Unavailable)?;
        Ok(queued)
    }
}
impl Prepared {
    /// Run on the existing owner executor. There is no new worker/queue here.
    /// The original guard carries every derived native call, callback and Hop.
    pub fn run(mut self) {
        self.notifier
            .deliver_admitted(&self.batch.cleared, &self.batch.raised, self.work.take());
    }
}
