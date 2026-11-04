//! One-use canonical probe attempt. Matched data is never current authority.
//! No actor, listener, controller or positive producer command calls this yet.
use std::{path::Path, time::Instant};
use zerus_client_core::desktop::{
    producer::{Challenge, Observation, CHALLENGE_WAIT},
    Hello,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refused {
    InvalidIdentity,
    Unavailable,
    Expired,
    Absent,
    Unknown,
    LiveLegacyOrUnknown,
    Mismatch,
    Unsupported,
}

/// The original expectation and deadline cannot be replaced after construction.
/// Consumed on any probe outcome. This owns no producer connection or role.
pub(super) struct Attempt {
    configuration: String,
    expected: Hello,
    challenge: Challenge,
    started: Instant,
    deadline: Instant,
}

/// Point-in-time identity evidence only, with no Clone or serialization. Even
/// its exact fields cannot prove a role, current lease or installed code trust.
pub(super) struct Matched {
    configuration: String,
    hello: Hello,
    challenge: Challenge,
}
impl std::fmt::Debug for Matched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatchedProducerIdentity")
            .finish_non_exhaustive()
    }
}
impl Matched {
    pub fn configuration(&self) -> &str {
        &self.configuration
    }
    pub fn hello(&self) -> &Hello {
        &self.hello
    }
    pub fn challenge(&self) -> &Challenge {
        &self.challenge
    }
}

fn non_nil_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

impl Attempt {
    pub fn new(configuration: &str, service_epoch: &str, expected: Hello) -> Result<Self, Refused> {
        Self::at(configuration, service_epoch, expected, Instant::now())
    }

    fn at(
        configuration: &str,
        service_epoch: &str,
        expected: Hello,
        started: Instant,
    ) -> Result<Self, Refused> {
        if !non_nil_uuid(service_epoch)
            || !non_nil_uuid(&expected.instance_epoch)
            || !non_nil_uuid(&expected.nonce)
            || !expected.identity_matches(&expected.nonce, configuration)
            || expected.writer_epoch.is_some()
        {
            return Err(Refused::InvalidIdentity);
        }
        Ok(Self {
            configuration: configuration.into(),
            expected,
            challenge: Challenge {
                nonce: uuid::Uuid::new_v4().to_string(),
                service_epoch: service_epoch.into(),
                producer_epoch: uuid::Uuid::new_v4().to_string(),
            },
            started,
            deadline: started
                .checked_add(CHALLENGE_WAIT)
                .ok_or(Refused::Unavailable)?,
        })
    }

    /// Data for the future authenticated control connection to install in the
    /// exact frontend's existing cached slot. This getter installs nothing.
    pub fn challenge(&self) -> &Challenge {
        &self.challenge
    }

    /// The original challenge end, shared by control I/O and the canonical probe.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Future host calls off-actor, with its pinned canonical endpoint path.
    /// Reuses the same-user, one-second, 4096-byte exchange; no extra transport,
    /// filesystem resolver, child, thread, retry or socket replacement.
    pub fn probe_at(self, path: &Path) -> Result<Matched, Refused> {
        #[cfg(unix)]
        {
            if Instant::now() >= self.deadline {
                return Err(Refused::Expired);
            }
            let observed = zerus_client_core::desktop::probe_producer_at(
                path,
                &self.challenge.nonce,
                &self.configuration,
            );
            self.complete(observed, Instant::now())
        }
        #[cfg(not(unix))]
        {
            let _ = (self, path);
            Err(Refused::Unsupported)
        }
    }

    fn complete(self, observed: Observation, now: Instant) -> Result<Matched, Refused> {
        if now < self.started {
            return Err(Refused::Unavailable);
        }
        if now >= self.deadline {
            return Err(Refused::Expired);
        }
        match observed {
            Observation::Absent => Err(Refused::Absent),
            Observation::Unknown => Err(Refused::Unknown),
            Observation::LiveLegacyOrUnknown => Err(Refused::LiveLegacyOrUnknown),
            Observation::LiveNext { reply }
                if reply.binds(&self.challenge, &self.expected, self.deadline) =>
            {
                Ok(Matched {
                    configuration: self.configuration,
                    hello: reply.hello,
                    challenge: self.challenge,
                })
            }
            Observation::LiveNext { .. } => Err(Refused::Mismatch),
        }
    }
}

#[cfg(test)]
mod tests;
