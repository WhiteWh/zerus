//! Optional synchronous observer captures, not SDK removal or role proof.
//! An optional adapter must put the whole Observed inside the actual copied native
//! block. Removing its observer/token is not proof that those copies are gone.
#![allow(dead_code)] // The optional SDK adapter has no production caller.
use crate::notifier::accounting::{self, Completion, Resources};
use std::sync::{Arc, Weak};
use zerus_client_core::desktop::drain::{Charge, Closed, Gate, Role};

pub(crate) const CAPTURES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Unsupported,
    WrongRole,
    Closed,
    Pressure,
    Unavailable,
}
impl From<accounting::Refused> for Refused {
    fn from(value: accounting::Refused) -> Self {
        match value {
            accounting::Refused::WrongRole => Self::WrongRole,
            accounting::Refused::Closed => Self::Closed,
            accounting::Refused::Unavailable => Self::Unavailable,
        }
    }
}

struct Scope {
    captures: Gate,
    resources: Arc<Resources>,
    native: accounting::Control,
}
pub(crate) struct Observers(Arc<Scope>);
/// Keeping this control never retains an otherwise finished owner/capture.
#[derive(Clone)]
pub(crate) struct Control(Weak<Scope>);

/// No Clone/serialization or OS identity. Native block copies must retain this
/// very capture, not a new uncharged closure around its action.
pub(crate) struct Observed<F> {
    // Field order is part of the proof: action data drops before its guard.
    action: F,
    guard: Capture,
}
struct Capture {
    resources: Arc<Resources>,
    native: accounting::Control,
    lifetime: Option<Completion>,
    // Drop the slot last, after callback data and the lifetime decision.
    _slot: Charge,
}

impl Observers {
    /// Take one host Attention charge before installing any native observers.
    /// The host separately retains all real SDK tokens/queues/worker resources.
    pub fn new(charge: Charge) -> Result<(Self, Control), Refused> {
        let (resources, native) = Resources::new_for(charge, Role::Attention)?;
        let scope = Arc::new(Scope {
            captures: Gate::new(CAPTURES).expect("fixed capture bound"),
            resources,
            native,
        });
        let control = Control(Arc::downgrade(&scope));
        Ok((Self(scope), control))
    }

    /// One adapter owner obligation in this same fixed resource group. An
    /// explicit close may complete it after removal; abandonment drops it
    /// uncompleted and pins even after callback admissions have been fenced.
    pub fn retain_owner(&self) -> Result<Completion, Refused> {
        self.0.resources.start().map_err(Refused::from)
    }

    /// Wrap before native registration. A refusal leaves the action with the
    /// caller; no callback was admitted. No thread, queue or SDK call is made.
    pub fn wrap<F>(&self, action: F) -> Result<Observed<F>, (Refused, F)>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let admit = || {
            let slot = self.0.captures.admit(Role::Attention).map_err(|e| {
                use zerus_client_core::desktop::drain::Refused as E;
                match e {
                    E::Closed => Refused::Closed,
                    E::Capacity => Refused::Pressure,
                    _ => Refused::Unavailable,
                }
            })?;
            let lifetime = self.0.resources.start().map_err(Refused::from)?;
            Ok(Capture {
                resources: self.0.resources.clone(),
                native: self.0.native.clone(),
                lifetime: Some(lifetime),
                _slot: slot,
            })
        };
        match admit() {
            Ok(guard) => Ok(Observed { action, guard }),
            Err(error) => Err((error, action)),
        }
    }
}
impl Control {
    /// Fence future capture/callback admissions; does not remove any observer.
    /// Close/wait the host's Attention gate as well, under the same deadline.
    /// A retained native capture keeps both gates Pending until real destruction.
    pub fn close(&self) -> Result<Closed, Refused> {
        if !cfg!(unix) {
            return Err(Refused::Unsupported);
        }
        let scope = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let captures = scope.captures.close();
        scope.native.close()?;
        Ok(captures)
    }
}
impl<F: Fn()> Observed<F> {
    /// Synchronous only. The action may not launch/queue unaccounted work.
    /// Close and admission share the qualified resource lock. An already
    /// admitted callback remains charged through its actual return or panic.
    pub fn call(&self) -> Result<(), Refused> {
        let work = self.guard.resources.start()?;
        (self.action)();
        work.complete();
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        // An abandoned open capture, poisoned scope or unwinding destructor
        // cannot certify removal/completion. Armed Completion pins one group.
        if !std::thread::panicking() && self.native.snapshot().is_ok_and(|state| state.closed) {
            if let Some(lifetime) = self.lifetime.take() {
                lifetime.complete();
            }
        }
    }
}

#[cfg(test)]
mod tests;
