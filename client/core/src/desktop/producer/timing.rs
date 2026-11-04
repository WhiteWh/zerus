//! Local monotonic bounds only. This is not a lease or authenticated context.
//! No caller, clock worker, wire message, role proof or admission is installed.
use std::time::{Duration, Instant};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
pub const IDLE_LIMIT: Duration = Duration::from_secs(15);
pub const DRAIN_WAIT: Duration = Duration::from_secs(120);
pub const ABSOLUTE_LIMIT: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    ConnectionLost,
    Revoked,
    OwnerChanged,
    ProofInvalidated,
    Unavailable,
    IdleExpired,
    AbsoluteExpired,
    DrainExpired,
    NonMonotonic,
}

/// WithinBounds describes these local timers, never live authority or drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Observation {
    WithinBounds,
    Invalid(Reason),
}

/// One preparation's non-renewable bounds. No clone, reset or serialization.
/// The host separately owns the exact authenticated connection and all binding,
/// current-owner, role, store, epoch and publication checks.
pub struct Timing {
    last: Instant,
    seen: Instant,
    absolute: Instant,
    drain: Instant,
    invalid: Option<Reason>,
}
impl Timing {
    /// Call only when the host starts a new, independently qualified preparation.
    /// This constructor itself qualifies nothing. A failed addition refuses.
    pub fn new(prepared: Instant) -> Option<Self> {
        Some(Self {
            last: prepared,
            seen: prepared,
            absolute: prepared.checked_add(ABSOLUTE_LIMIT)?,
            drain: prepared.checked_add(DRAIN_WAIT)?,
            invalid: None,
        })
    }

    pub fn heartbeat_due(&self) -> Instant {
        self.last
            .checked_add(HEARTBEAT_INTERVAL)
            .unwrap_or(self.absolute)
            .min(self.absolute)
    }
    /// The next idle/absolute boundary. It does not schedule a timer or I/O.
    pub fn deadline(&self) -> Instant {
        self.last
            .checked_add(IDLE_LIMIT)
            .unwrap_or(self.absolute)
            .min(self.absolute)
    }
    pub fn absolute_deadline(&self) -> Instant {
        self.absolute
    }
    pub fn drain_deadline(&self) -> Instant {
        self.drain
    }

    /// Use the host's observation time, never a timestamp claimed by a message.
    /// Equal deadlines expire. Missing or out-of-order observations fail closed.
    pub fn observe(&mut self, now: Instant) -> Observation {
        if let Some(reason) = self.invalid {
            return Observation::Invalid(reason);
        }
        if now < self.seen {
            return self.invalidate(Reason::NonMonotonic);
        }
        self.seen = now;
        if now >= self.absolute {
            self.invalidate(Reason::AbsoluteExpired)
        } else if now >= self.deadline() {
            self.invalidate(Reason::IdleExpired)
        } else {
            Observation::WithinBounds
        }
    }

    /// A current heartbeat extends only the idle boundary. It never renews the
    /// preparation's absolute lifetime or its drain wait, or revives failure.
    pub fn heartbeat(&mut self, now: Instant) -> Observation {
        let observation = self.observe(now);
        if observation == Observation::WithinBounds {
            self.last = now;
        }
        observation
    }

    /// The host uses this while still waiting for actual drain. After actual
    /// drain it still checks observe() before capture/publication/effects, plus
    /// all independent proofs. This method cannot establish that drain ended.
    pub fn observe_drain(&mut self, now: Instant) -> Observation {
        let observation = self.observe(now);
        if observation == Observation::WithinBounds && now >= self.drain {
            self.invalidate(Reason::DrainExpired)
        } else {
            observation
        }
    }

    /// Negative local state only. The first reason is sticky. The host must
    /// fence its actual permissions separately; this does not release resources,
    /// reopen frontend gates or restore a prior owner.
    pub fn invalidate(&mut self, reason: Reason) -> Observation {
        Observation::Invalid(*self.invalid.get_or_insert(reason))
    }
}

#[cfg(test)]
mod tests;
