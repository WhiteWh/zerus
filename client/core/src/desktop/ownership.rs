//! Closed ownership intents and read-only receipts. Status is never authority.
use super::{digest, ui_requests::Request};
use serde::{Deserialize, Serialize};
use zerus_desktop_migration::canonical_uuid;

pub const ABI: &str = "desktop-ownership-v1";
pub const HELP_MARKER: &str = "Desktop ownership ABI: desktop-ownership-v1";
pub const BYTES: usize = 16 * 1024;
pub const MAX_REVISION: u64 = (1 << 53) - 1;

/// Set once by the native startup path, before canonical instance registration.
/// This is carried by original-child evidence, never by the caller's intent.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Native,
    Service,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Grant,
    Rearm,
    NativeTakeover,
}
impl Action {
    pub fn mode(self) -> Mode {
        match self {
            Self::Grant | Self::Rearm => Mode::Service,
            Self::NativeTakeover => Mode::Native,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Passive,
    Preparing,
    Service,
    Draining,
    Revoked,
    Native,
    IdentityChanged,
    Unavailable,
}

/// A fixed-size configuration read, usable before a service subscription or
/// the native GUI's Bootstrap hold. It grants no permission to start writers.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub abi: String,
    pub configuration: String,
    pub journal: Option<String>,
    pub revision: u64,
    pub state: State,
    pub rearm_required: bool,
    pub request_id: Option<String>,
    pub owner_epoch: Option<String>,
    pub identity_current: bool,
}
impl Status {
    pub fn matches(&self, configuration: &str) -> bool {
        self.abi == ABI
            && digest(configuration)
            && self.configuration == configuration
            && self.journal.as_deref().is_none_or(canonical_uuid)
            && self.request_id.as_deref().is_none_or(canonical_uuid)
            && self.owner_epoch.as_deref().is_none_or(canonical_uuid)
            && self.revision <= MAX_REVISION
            && (self.journal.is_some() || self.revision == 0)
            && bounded(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Pending,
    /// Authenticated native takeover found no desktop state or service. This
    /// is a one-time observed no-op, not a durable grant or committed command.
    NativeAbsent,
    Granted {
        owner_epoch: String,
    },
    NativeReady {
        prior_owner_epoch: Option<String>,
        revoked: bool,
        checkpointed: bool,
        drained: bool,
    },
    Refused {
        code: String,
    },
}

/// The actor commits a prepared command before effects and publishes a final
/// receipt only for that command. Pending/unknown never permits GUI posting.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub abi: String,
    pub request_id: String,
    pub configuration: String,
    pub journal: Option<String>,
    pub fingerprint: String,
    pub action: Action,
    pub revision: u64,
    pub accepted: bool,
    pub durable: bool,
    pub outcome: Outcome,
    /// Fresh actor observation attached when returning/looking up the immutable
    /// command result. Historical NativeReady alone cannot settle a new GUI.
    pub current: Status,
}
impl Receipt {
    pub fn valid(&self) -> bool {
        self.abi == ABI
            && canonical_uuid(&self.request_id)
            && digest(&self.configuration)
            && self.journal.as_deref().is_none_or(canonical_uuid)
            && digest(&self.fingerprint)
            && self.revision <= MAX_REVISION
            && (!self.durable || self.accepted)
            && self.current.matches(&self.configuration)
            && bounded(self)
            && match &self.outcome {
                Outcome::Pending => self.accepted && self.durable && self.journal.is_some(),
                Outcome::NativeAbsent => {
                    self.action == Action::NativeTakeover
                        && self.accepted
                        && !self.durable
                        && self.journal.is_none()
                        && self.revision == 0
                        && self.current.journal.is_none()
                        && self.current.revision == 0
                        && self.current.state == State::Native
                        && !self.current.rearm_required
                        && self.current.owner_epoch.is_none()
                        && self.current.request_id.as_deref() == Some(self.request_id.as_str())
                        && self.current.identity_current
                }
                Outcome::Granted { owner_epoch } => {
                    self.action != Action::NativeTakeover
                        && self.accepted
                        && self.durable
                        && self.journal.is_some()
                        && canonical_uuid(owner_epoch)
                }
                Outcome::NativeReady {
                    prior_owner_epoch,
                    revoked,
                    checkpointed,
                    drained,
                } => {
                    self.action == Action::NativeTakeover
                        && self.accepted
                        && self.durable
                        && self.journal.is_some()
                        && *revoked
                        && *checkpointed
                        && *drained
                        && prior_owner_epoch.as_deref().is_none_or(canonical_uuid)
                }
                Outcome::Refused { code } => {
                    !code.is_empty() && code.len() <= 128 && !code.chars().any(char::is_control)
                }
            }
    }
    pub fn matches(&self, request: &Request, configuration: &str) -> bool {
        let Request::DesktopOwnership {
            request_id,
            action,
            journal,
            revision,
        } = request
        else {
            return false;
        };
        self.valid()
            && *revision <= MAX_REVISION
            && (journal.is_some() || *revision == 0)
            && self.configuration == configuration
            && self.request_id == *request_id
            && self.action == *action
            && self.fingerprint == request.fingerprint()
            && journal
                .as_ref()
                .is_none_or(|id| Some(id) == self.journal.as_ref())
            && (matches!(
                self.outcome,
                Outcome::Refused { .. } | Outcome::NativeAbsent
            ) || self.revision > *revision)
    }
    /// A historical grant receipt is readable but cannot stand in for the
    /// fresh current ownership observation attached by the actor.
    pub fn service_granted(&self) -> bool {
        self.valid()
            && matches!(&self.outcome, Outcome::Granted { owner_epoch }
                if self.current.owner_epoch.as_ref() == Some(owner_epoch))
            && self.current.state == State::Service
            && self.current.identity_current
            && !self.current.rearm_required
            && self.current.journal == self.journal
            && self.current.revision == self.revision
            && self.current.request_id.as_deref() == Some(self.request_id.as_str())
    }
    /// Call only AFTER matches() on the selected original relay's reply. A
    /// granted service epoch or a display status cannot release native writers.
    pub fn native_ready(&self) -> bool {
        self.valid()
            && self.action == Action::NativeTakeover
            && matches!(
                self.outcome,
                Outcome::NativeReady { .. } | Outcome::NativeAbsent
            )
            && self.current.state == State::Native
            && self.current.identity_current
            && self.current.journal == self.journal
            && self.current.revision == self.revision
            && self.current.request_id.as_deref() == Some(self.request_id.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    DesktopOwnershipResult {
        request_id: String,
        configuration: String,
        original_request_id: String,
        fingerprint: String,
        journal: Option<String>,
        revision: u64,
    },
}
impl Query {
    pub fn for_request(
        request_id: String,
        configuration: String,
        original: &Request,
    ) -> Option<Self> {
        let Request::DesktopOwnership {
            request_id: original_request_id,
            journal,
            revision,
            ..
        } = original
        else {
            return None;
        };
        let query = Self::DesktopOwnershipResult {
            request_id,
            configuration,
            original_request_id: original_request_id.clone(),
            fingerprint: original.fingerprint(),
            journal: journal.clone(),
            revision: *revision,
        };
        query.valid().then_some(query)
    }
    pub fn valid(&self) -> bool {
        let Self::DesktopOwnershipResult {
            request_id,
            configuration,
            original_request_id,
            fingerprint,
            journal,
            revision,
        } = self;
        canonical_uuid(request_id)
            && canonical_uuid(original_request_id)
            && digest(configuration)
            && digest(fingerprint)
            && journal.as_deref().is_none_or(canonical_uuid)
            && *revision <= MAX_REVISION
            && (journal.is_some() || *revision == 0)
            && bounded(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub abi: String,
    pub request_id: String,
    pub configuration: String,
    pub found: bool,
    /// A missing/expired receipt alone is not a definitive negative. The
    /// caller also needs its original child's verified exit before retrying a
    /// NEW intent; it never resends the old mutating request automatically.
    pub definitive: bool,
    pub receipt: Option<Receipt>,
}
impl Lookup {
    pub fn matches(&self, query: &Query) -> bool {
        let Query::DesktopOwnershipResult {
            request_id,
            configuration,
            original_request_id,
            fingerprint,
            journal,
            ..
        } = query;
        query.valid()
            && self.abi == ABI
            && self.request_id == *request_id
            && self.configuration == *configuration
            && bounded(self)
            && match &self.receipt {
                Some(receipt) => {
                    self.found
                        && !self.definitive
                        && receipt.valid()
                        && receipt.durable
                        && receipt.request_id == *original_request_id
                        && receipt.configuration == *configuration
                        && receipt.fingerprint == *fingerprint
                        && journal
                            .as_ref()
                            .is_none_or(|id| Some(id) == receipt.journal.as_ref())
                }
                None => !self.found,
            }
    }
}

fn bounded(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= BYTES)
}

#[cfg(test)]
mod tests;
