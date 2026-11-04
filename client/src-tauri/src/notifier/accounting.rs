//! Optional lifetime accounting, not a native lease or full role barrier.
//! The host supplies one qualified role charge and retains the real platform
//! connection/delegate resources separately. No worker or OS call lives here.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, Weak,
};
use zerus_client_core::desktop::drain::{Charge, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    WrongRole,
    Closed,
    Unavailable,
}

/// Local diagnostics only, never a service drain or execution permit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub closed: bool,
    pub pinned: bool,
}

pub(crate) struct Resources {
    parent: Mutex<Option<Charge>>,
    pinned: AtomicBool,
    role: Role,
}

/// Weak, so retaining a finished control never retains the host charge.
#[derive(Clone)]
pub struct Control(Weak<Resources>);

impl Resources {
    pub fn new(charge: Charge) -> Result<(Arc<Self>, Control), Refused> {
        Self::new_for(charge, Role::NativePosting)
    }

    /// Reuse the same fixed completion scope for the qualified native and Attention observer roles.
    pub fn new_for(charge: Charge, expected: Role) -> Result<(Arc<Self>, Control), Refused> {
        if !matches!(
            expected,
            Role::NativePosting | Role::Badges | Role::Attention
        ) || charge.role() != expected
        {
            return Err(Refused::WrongRole);
        }
        let resources = Arc::new(Self {
            parent: Mutex::new(Some(charge)),
            pinned: AtomicBool::new(false),
            role: expected,
        });
        let control = Control(Arc::downgrade(&resources));
        Ok((resources, control))
    }

    /// Admission and close share one lock. Copies retain the same host group;
    /// they add no per-token collection or independent resource admission.
    pub fn start(self: &Arc<Self>) -> Result<Completion, Refused> {
        let parent = self.parent.lock().map_err(|_| Refused::Unavailable)?;
        let charge = parent.as_ref().ok_or(Refused::Closed)?.clone();
        Ok(Completion {
            resources: self.clone(),
            charge: Some(charge),
        })
    }

    fn pin(&self, charge: Charge) {
        if !self.pinned.swap(true, Ordering::AcqRel) {
            // A dropped completion/work item is not proof of completion.
            // Retain exactly one copy for this fixed scope until process exit,
            // even if its caller drops every control/notifier handle. Further
            // unknown drops add no retained allocation. Nothing is retried.
            std::mem::forget(charge);
            // This can run during unwinding. A broken diagnostic pipe must
            // not panic a second time from Drop.
            let diagnostic: &[u8] = match self.role {
                Role::Badges => b"zerus-client: role=badges slot=0 completion unverified; resources pinned\n",
                Role::Attention => b"zerus-client: role=attention slot=0 completion unverified; resources pinned\n",
                _ => b"zerus-client: role=native_posting slot=0 completion unverified; resources pinned\n",
            };
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), diagnostic);
        }
    }
}

#[allow(dead_code)] // No production host control caller in this prerequisite.
impl Control {
    /// Close-only and nonblocking apart from the short local lock. It stops
    /// future steps, not already admitted Center calls or completion blocks.
    /// The host must wait on its own closed Gate, off the GUI thread.
    pub fn close(&self) -> Result<Snapshot, Refused> {
        let resources = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let parent = resources
            .parent
            .lock()
            .map_err(|_| Refused::Unavailable)?
            .take();
        drop(parent);
        self.snapshot()
    }

    pub fn snapshot(&self) -> Result<Snapshot, Refused> {
        let resources = self.0.upgrade().ok_or(Refused::Unavailable)?;
        let closed = resources
            .parent
            .lock()
            .map_err(|_| Refused::Unavailable)?
            .is_none();
        Ok(Snapshot {
            closed,
            pinned: resources.pinned.load(Ordering::Acquire),
        })
    }
}

/// Armed until actual work returns normally, not merely until its callback is
/// dropped. The same obligation moves from a native completion into queued
/// Hop work. A panic or an unexecuted drop conservatively pins the fixed scope.
pub(crate) struct Completion {
    resources: Arc<Resources>,
    charge: Option<Charge>,
}
impl Completion {
    pub fn fork(&self) -> Self {
        Self {
            resources: self.resources.clone(),
            charge: self.charge.clone(),
        }
    }

    /// Plain retention through the scheduling callback's own return, even
    /// when Hop executes its work synchronously.
    pub fn hold(&self) -> Option<Charge> {
        self.charge.clone()
    }

    pub fn complete(mut self) {
        drop(self.charge.take());
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take() {
            self.resources.pin(charge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use zerus_client_core::desktop::drain::{Gate, Observation};

    #[test]
    fn only_matching_qualified_native_roles_are_admitted() {
        for expected in Role::ALL {
            for supplied in Role::ALL {
                let gate = Gate::new(1).unwrap();
                let result = Resources::new_for(gate.admit(supplied).unwrap(), expected);
                assert_eq!(
                    result.is_ok(),
                    expected == supplied
                        && matches!(
                            expected,
                            Role::NativePosting | Role::Badges | Role::Attention
                        )
                );
                drop(result);
                assert_eq!(gate.snapshot().unwrap().admitted, 0);
            }
        }
    }
    #[test]
    fn ordinary_native_constructor_still_refuses_attention() {
        let gate = Gate::new(1).unwrap();
        assert!(matches!(
            Resources::new(gate.admit(Role::Attention).unwrap()),
            Err(Refused::WrongRole)
        ));
        assert_eq!(gate.snapshot().unwrap().admitted, 0);
    }
    #[test]
    fn poisoned_attention_scope_cannot_certify_completion() {
        let gate = Gate::new(1).unwrap();
        let (resources, control) =
            Resources::new_for(gate.admit(Role::Attention).unwrap(), Role::Attention).unwrap();
        let capture = resources.start().unwrap();
        let broken = resources.clone();
        assert!(std::thread::spawn(move || {
            let _held = broken.parent.lock().unwrap();
            panic!("synthetic observer accounting failure");
        })
        .join()
        .is_err());
        assert!(matches!(resources.start(), Err(Refused::Unavailable)));
        assert_eq!(control.close(), Err(Refused::Unavailable));
        drop(capture);
        drop(resources);
        assert_eq!(
            gate.close().wait_until(Instant::now()),
            Observation::Pending
        );
    }
    #[test]
    fn badge_close_keeps_actual_work_but_not_an_idle_control_charged() {
        let gate = Gate::new(1).unwrap();
        let (resources, control) =
            Resources::new_for(gate.admit(Role::Badges).unwrap(), Role::Badges).unwrap();
        let work = resources.start().unwrap();
        control.close().unwrap();
        assert!(matches!(resources.start(), Err(Refused::Closed)));
        let closed = gate.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        work.complete();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        assert!(!control.snapshot().unwrap().pinned);
    }
    #[test]
    fn many_unknown_badge_drops_pin_one_fixed_group_after_every_handle_is_gone() {
        let gate = Gate::new(1).unwrap();
        let (resources, control) =
            Resources::new_for(gate.admit(Role::Badges).unwrap(), Role::Badges).unwrap();
        let work = resources.start().unwrap();
        for _ in 0..32 {
            drop(work.fork());
        }
        control.close().unwrap();
        work.complete();
        assert!(control.snapshot().unwrap().pinned);
        drop(control);
        drop(resources);
        assert_eq!(gate.snapshot().unwrap().count(Role::Badges), 1);
        assert_eq!(
            gate.close().wait_until(Instant::now()),
            Observation::Pending
        );
    }
}
