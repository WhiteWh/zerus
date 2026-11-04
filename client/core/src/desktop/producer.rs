//! Cached connection challenge and read-only health. Neither grants a role.
use super::{Hello, Responder, REPLY_BYTES};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use zerus_desktop_migration::canonical_uuid;

pub mod timing;

pub const ABI: &str = "desktop-producer-v1";
pub const CHALLENGE_WAIT: Duration = Duration::from_secs(10);

pub fn query(nonce: &str) -> Option<Vec<u8>> {
    canonical_uuid(nonce).then(|| format!("{ABI} {nonce}\n").into_bytes())
}
pub fn query_nonce(line: &[u8]) -> Option<&str> {
    let nonce = std::str::from_utf8(line)
        .ok()?
        .strip_prefix(ABI)?
        .strip_prefix(' ')?;
    canonical_uuid(nonce).then_some(nonce)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub nonce: String,
    pub service_epoch: String,
    pub producer_epoch: String,
}
impl Challenge {
    pub fn valid(&self) -> bool {
        [&self.nonce, &self.service_epoch, &self.producer_epoch]
            .into_iter()
            .all(|s| canonical_uuid(s))
    }
}

/// Fixed-size diagnostic of GUI delivery resources, never a drain receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadDelivery {
    pub revision: u64,
    pub capacity: u8,
    pub in_flight: u8,
    pub pinned: u8,
    pub stopped: bool,
}
impl ReadDelivery {
    pub fn valid(&self) -> bool {
        self.revision > 0
            && self.capacity == 4
            && self.pinned <= self.in_flight
            && self.in_flight <= self.capacity
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub abi: String,
    pub version: u32,
    pub hello: Hello,
    pub challenge: Option<Challenge>,
    pub read_delivery: Option<ReadDelivery>,
}
impl Reply {
    pub fn identity_matches(&self, nonce: &str, configuration: &str) -> bool {
        self.abi == ABI
            && self.version == 1
            && self.hello.identity_matches(nonce, configuration)
            && self
                .challenge
                .as_ref()
                .is_none_or(|c| c.valid() && c.nonce == nonce)
            && self.read_delivery.is_none_or(|h| h.valid())
    }
    /// Exact pending connection plus the previously observed frontend identity.
    /// An unsolicited diagnostic reply can never satisfy this predicate.
    pub fn binds(&self, pending: &Challenge, expected: &Hello, deadline: Instant) -> bool {
        let Some(configuration) = expected.configuration.as_deref() else {
            return false;
        };
        let mut hello = expected.clone();
        hello.nonce.clone_from(&pending.nonce);
        Instant::now() < deadline
            && pending.valid()
            && self.identity_matches(&pending.nonce, configuration)
            && self.challenge.as_ref() == Some(pending)
            && self.hello == hello
    }
}

#[derive(Default)]
pub(super) struct Cached {
    offer: Option<(Challenge, Instant, bool)>,
    health: Option<ReadDelivery>,
}
impl Responder {
    /// Future authenticated controller only. There is no production caller yet.
    /// One offer, monotonic expiry, no replacement of an unexpired offer.
    pub fn install_challenge(&self, challenge: Challenge, now: Instant) -> bool {
        if !challenge.valid()
            || !self.hello(&challenge.nonce).identity_matches(
                &challenge.nonce,
                self.configuration.get().map(String::as_str).unwrap_or(""),
            )
        {
            return false;
        }
        let Ok(mut cached) = self.producer.lock() else {
            return false;
        };
        if cached
            .offer
            .as_ref()
            .is_some_and(|(old, until, _)| now < *until || old == &challenge)
        {
            return false;
        }
        cached.offer = Some((challenge, now + CHALLENGE_WAIT, false));
        true
    }
    /// Updates only bounded diagnostics. Older callbacks cannot overwrite newer ones.
    pub fn read_delivery(&self, health: ReadDelivery) {
        if !health.valid() {
            return;
        }
        if let Ok(mut cached) = self.producer.lock() {
            if cached
                .health
                .is_none_or(|old| health.revision > old.revision)
            {
                cached.health = Some(health);
            }
        }
    }
    pub(super) fn producer_reply(&self, nonce: &str, now: Instant) -> Option<Vec<u8>> {
        let mut cached = self.producer.lock().ok()?;
        let challenge = cached.offer.as_mut().and_then(|(offer, until, used)| {
            if !*used && now < *until && offer.nonce == nonce {
                *used = true;
                Some(offer.clone())
            } else {
                None
            }
        });
        let mut bytes = serde_json::to_vec(&Reply {
            abi: ABI.into(),
            version: 1,
            hello: self.hello(nonce),
            challenge,
            read_delivery: cached.health,
        })
        .ok()?;
        bytes.push(b'\n');
        (bytes.len() <= REPLY_BYTES).then_some(bytes)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    Absent,
    Unknown,
    LiveLegacyOrUnknown,
    LiveNext { reply: Box<Reply> },
}

#[cfg(test)]
mod tests {
    use super::*;
    const NONCE: &str = "11111111-1111-4111-8111-111111111111";
    const EPOCH: &str = "22222222-2222-4222-8222-222222222222";
    fn responder() -> Responder {
        Responder::new(
            "com.hgdev.hgs-tray".into(),
            super::super::Build {
                version: "1.2.3".into(),
                source_commit: "b".repeat(40),
                source_dirty: Some(false),
            },
            EPOCH.into(),
            true,
            false,
        )
    }
    fn hello() -> Hello {
        let responder = responder();
        responder.configure_once("a".repeat(64));
        responder.hello(NONCE)
    }
    fn challenge() -> Challenge {
        Challenge {
            nonce: NONCE.into(),
            service_epoch: EPOCH.into(),
            producer_epoch: "33333333-3333-4333-8333-333333333333".into(),
        }
    }
    fn decode(responder: &Responder, nonce: &str, now: Instant) -> Reply {
        serde_json::from_slice(&responder.producer_reply(nonce, now).unwrap()).unwrap()
    }
    #[test]
    fn diagnostic_reply_does_not_bind_and_old_delegation_stays_identical() {
        let responder = responder();
        assert!(responder.configure_once("a".repeat(64)));
        let old = super::super::query(NONCE).unwrap();
        let before = responder.reply(old.strip_suffix(b"\n").unwrap()).unwrap();
        let line = query(NONCE).unwrap();
        assert!(line.len() <= 64);
        assert_eq!(query_nonce(line.strip_suffix(b"\n").unwrap()), Some(NONCE));
        assert!(query_nonce(b"desktop-producer-v1 invalid").is_none());
        let reply: Reply =
            serde_json::from_slice(&responder.reply(line.strip_suffix(b"\n").unwrap()).unwrap())
                .unwrap();
        assert!(reply.identity_matches(NONCE, &"a".repeat(64)));
        assert!(!reply.binds(&challenge(), &hello(), Instant::now() + CHALLENGE_WAIT));
        assert_eq!(
            before,
            responder.reply(old.strip_suffix(b"\n").unwrap()).unwrap()
        );
    }
    #[test]
    fn offer_is_one_use_and_expires_without_replacement_or_wrong_nonce_consumption() {
        let responder = responder();
        let now = Instant::now();
        assert!(
            !responder.install_challenge(challenge(), now),
            "unconfigured"
        );
        responder.configure_once("a".repeat(64));
        assert!(responder.install_challenge(challenge(), now));
        assert!(!responder.install_challenge(challenge(), now));
        assert!(decode(&responder, EPOCH, now).challenge.is_none());
        let reply = decode(&responder, NONCE, now);
        assert!(reply.binds(&challenge(), &hello(), Instant::now() + CHALLENGE_WAIT));
        assert!(!reply.binds(&challenge(), &hello(), Instant::now()));
        assert!(decode(&responder, NONCE, now).challenge.is_none());
        let later = now + CHALLENGE_WAIT;
        assert!(!responder.install_challenge(challenge(), later));
        let mut fresh = challenge();
        fresh.nonce = EPOCH.into();
        assert!(responder.install_challenge(fresh, later));
        assert!(decode(&responder, NONCE, later + CHALLENGE_WAIT)
            .challenge
            .is_none());
        assert!(decode(&responder, EPOCH, later + CHALLENGE_WAIT)
            .challenge
            .is_none());
    }
    #[test]
    fn exact_connection_identity_and_closed_bounded_health_are_required() {
        let responder = responder();
        responder.configure_once("a".repeat(64));
        let now = Instant::now();
        responder.install_challenge(challenge(), now);
        let reply = decode(&responder, NONCE, now);
        let mut other = challenge();
        other.service_epoch = other.producer_epoch.clone();
        assert!(!reply.binds(&other, &hello(), Instant::now() + CHALLENGE_WAIT));
        other = challenge();
        other.producer_epoch = EPOCH.into();
        assert!(!reply.binds(&other, &hello(), Instant::now() + CHALLENGE_WAIT));
        let mut expected = hello();
        expected.instance_epoch = challenge().producer_epoch;
        assert!(!reply.binds(&challenge(), &expected, Instant::now() + CHALLENGE_WAIT));
        let mut changed = reply.clone();
        changed.hello.build.source_commit = "c".repeat(40);
        assert!(!changed.binds(&challenge(), &hello(), Instant::now() + CHALLENGE_WAIT));
        let health = ReadDelivery {
            revision: 2,
            capacity: 4,
            in_flight: 4,
            pinned: 4,
            stopped: false,
        };
        responder.read_delivery(health);
        responder.read_delivery(ReadDelivery {
            revision: 1,
            pinned: 0,
            ..health
        });
        responder.read_delivery(ReadDelivery {
            revision: 3,
            pinned: 5,
            ..health
        });
        assert_eq!(decode(&responder, NONCE, now).read_delivery, Some(health));
        let mut raw = serde_json::to_value(reply).unwrap();
        raw["authority"] = true.into();
        assert!(serde_json::from_value::<Reply>(raw).is_err());
    }
    #[test]
    fn dev_replay_and_poisoned_cache_never_install_an_offer() {
        let mut responder = responder();
        responder.replay = true;
        responder.configure_once("a".repeat(64));
        assert!(!responder.install_challenge(challenge(), Instant::now()));
        responder.replay = false;
        responder.application = "dev.zerus.next".into();
        assert!(!responder.install_challenge(challenge(), Instant::now()));
        let responder = super::tests::responder();
        responder.configure_once("a".repeat(64));
        let _ = std::panic::catch_unwind(|| {
            let _held = responder.producer.lock().unwrap();
            panic!("fixture");
        });
        assert!(!responder.install_challenge(challenge(), Instant::now()));
        assert!(responder.producer_reply(NONCE, Instant::now()).is_none());
    }
}
