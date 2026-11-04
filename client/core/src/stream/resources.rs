//! Optional lifetime accounting around the existing supervisor. No output
//! adapter, process policy or authority: the host must bound source admission.
use crate::desktop::drain::{Charge, Gate, Role};
use std::sync::{Arc, Weak};

#[derive(Clone)]
pub(super) struct Scope(Arc<Inner>);
#[derive(Clone)]
pub(super) struct Control(Weak<Inner>);
impl Control {
    pub fn close(&self) {
        if let Some(inner) = self.0.upgrade() {
            Scope(inner).close();
        }
    }
}
struct Inner {
    _host: Arc<dyn Send + Sync>,
    primary: Gate,
    rpc: Gate,
}
struct Group {
    _scope: Scope,
    _charge: Charge,
}
impl Scope {
    pub fn new(host: Arc<dyn Send + Sync>) -> Self {
        Self(Arc::new(Inner {
            _host: host,
            primary: Gate::new(1).unwrap(),
            // One listing, each already-bounded Activity/process follow, and
            // the existing organization poller and worker-refresh read.
            // Retained pipes keep capacity.
            rpc: Gate::new(
                3 + zerus_watch_protocol::ACTIVITY_SUBSCRIPTIONS
                    + zerus_watch_protocol::PROCESSES_SUBSCRIPTIONS,
            )
            .unwrap(),
        }))
    }
    pub fn control(&self) -> Control {
        Control(Arc::downgrade(&self.0))
    }
    pub fn primary(&self) -> Option<Arc<dyn Send + Sync>> {
        self.admit(&self.0.primary)
    }
    pub fn rpc(&self) -> Option<Arc<dyn Send + Sync>> {
        self.admit(&self.0.rpc)
    }
    fn admit(&self, gate: &Gate) -> Option<Arc<dyn Send + Sync>> {
        let charge = gate.admit(Role::Fleet).ok()?;
        Some(Arc::new(Group {
            _scope: self.clone(),
            _charge: charge,
        }))
    }
    pub fn close(&self) {
        self.0.primary.close();
        self.0.rpc.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::drain::Observation;
    use std::time::Instant;
    #[test]
    fn primary_and_rpc_limits_retain_copies_until_actual_drop_and_never_reopen() {
        let gate = Gate::new(1).unwrap();
        let scope = Scope::new(Arc::new(gate.admit(Role::Fleet).unwrap()));
        let primary = scope.primary().unwrap();
        let retained = primary.clone();
        assert!(scope.primary().is_none());
        drop(primary);
        assert!(scope.primary().is_none());
        let limit = 35; // Fixed admitted callback budget, independent of held copies.
        let jobs: Vec<_> = (0..limit).map(|_| scope.rpc().unwrap()).collect();
        assert!(scope.rpc().is_none());
        scope.close();
        drop(jobs);
        drop(retained);
        assert!(scope.primary().is_none());
        assert!(scope.rpc().is_none());
        let closed = gate.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        drop(scope);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    }
    #[test]
    fn an_unverified_child_pins_its_primary_slot_and_the_host_group() {
        let gate = Gate::new(1).unwrap();
        let scope = Scope::new(Arc::new(gate.admit(Role::Fleet).unwrap()));
        let child = crate::run::ChildCharge::new(scope.primary().unwrap());
        drop(child); // Models unavailable exit; actual external-reap case is separate.
        assert!(scope.primary().is_none());
        scope.close();
        drop(scope);
        assert_eq!(
            gate.close().wait_until(Instant::now()),
            Observation::Pending
        );
    }
}
