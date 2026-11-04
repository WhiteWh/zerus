//! Closed notification settings and native foreground observations.
//!
//! Ownership, native provenance and store admission remain Root checks. A
//! client checks the actual selected CLI marker before using these types.
use super::digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zerus_desktop_migration::canonical_uuid;

pub mod native;

pub const ABI: &str = "desktop-notification-controls-v1";
pub const HELP_MARKER: &str = "Notification controls ABI: desktop-notification-controls-v1";
pub const BYTES: usize = 8192;
pub const MAX_REVISION: u64 = (1 << 53) - 1;
pub const FOCUS_TTL_MS: u64 = 5000;
/// Repeated Foreground renewals only. Background/Unknown transitions bypass
/// this delay, while still retaining one outstanding original child at a time.
pub const FOCUS_REFRESH_MS: u64 = 2000;

/// Exactly the existing optional-question preference. No writer or approval
/// consent, global notification switch, mode preference or focus is stored here.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub optional_questions: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Configure {
        request_id: String,
        journal: Option<String>,
        revision: u64,
        preferences: Preferences,
    },
    /// There is deliberately no caller-supplied focused value. The canonical
    /// native Responder supplies its own observation in original-child evidence.
    Focus { request_id: String },
}
impl Command {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Configure { request_id, .. } | Self::Focus { request_id } => request_id,
        }
    }
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Configure { .. } => "notification_preferences",
            Self::Focus { .. } => "notification_focus",
        }
    }
    pub fn valid(&self) -> bool {
        canonical_uuid(self.request_id())
            && bounded(self)
            && match self {
                Self::Configure {
                    journal, revision, ..
                } => {
                    journal.as_deref().is_none_or(canonical_uuid)
                        && *revision < MAX_REVISION
                        && (journal.is_some() || *revision == 0)
                }
                Self::Focus { .. } => true,
            }
    }
    pub fn fingerprint(&self, configuration: &str) -> Option<String> {
        if !self.valid() || !digest(configuration) {
            return None;
        }
        Some(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(configuration, self)).ok()?)
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub configuration: String,
    pub service_epoch: String,
}
impl Scope {
    pub fn valid(&self) -> bool {
        digest(&self.configuration) && canonical_uuid(&self.service_epoch)
    }
}

/// Display only. On a first connection the native GUI may initialize an
/// unconfigured record from its existing preference. Reconnect never overwrites
/// a configured value; later edits use the exact journal and revision.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub abi: String,
    pub scope: Scope,
    pub journal: Option<String>,
    pub revision: u64,
    pub configured: bool,
    pub preferences: Preferences,
}
impl Status {
    pub fn valid(&self) -> bool {
        self.abi == ABI
            && self.scope.valid()
            && bounded(self)
            && self.journal.as_deref().is_none_or(canonical_uuid)
            && self.revision <= MAX_REVISION
            && if self.configured {
                self.journal.is_some() && self.revision > 0
            } else {
                self.revision == 0 && self.preferences == Preferences::default()
            }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Saved,
    Refused { code: String },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub fingerprint: String,
    pub journal: Option<String>,
    pub revision: u64,
    pub preferences: Preferences,
    pub durable: bool,
    pub outcome: Outcome,
}
impl Receipt {
    /// A historical save acknowledges this exact edit. It does not prove that
    /// the saved value is still current, nor permit notification posting.
    pub fn matches(&self, original: &Command, configuration: &str) -> bool {
        let Command::Configure {
            journal,
            revision,
            preferences,
            ..
        } = original
        else {
            return false;
        };
        self.abi == ABI
            && self.scope.valid()
            && self.scope.configuration == configuration
            && self.request_id == original.request_id()
            && original.fingerprint(configuration).as_ref() == Some(&self.fingerprint)
            && self.journal.as_deref().is_none_or(canonical_uuid)
            && self.revision <= MAX_REVISION
            && (!self.durable || self.journal.is_some())
            && self.preferences == *preferences
            && bounded(self)
            && match &self.outcome {
                Outcome::Saved => {
                    self.durable
                        && self.journal.is_some()
                        && (journal.is_none() || journal == &self.journal)
                        && self.revision == revision + 1
                }
                Outcome::Refused { code } => !code.is_empty() && code.len() <= 128,
            }
    }
}

/// Read-only recovery of a preference edit; focus has no journal or lookup.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub request_id: String,
    pub scope: Scope,
    pub original: Command,
}
impl Query {
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.request_id)
            && self.scope.valid()
            && self.original.valid()
            && matches!(self.original, Command::Configure { .. })
            && bounded(self)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub original_request_id: String,
    pub fingerprint: String,
    pub receipt: Option<Receipt>,
    /// Valid only for a missing command in the SAME original nonempty journal
    /// at the unchanged revision. The client also requires original-child exit
    /// confirmation before starting a different logical edit.
    pub definitive_not_applied: bool,
}
impl Lookup {
    pub fn matches(&self, query: &Query) -> bool {
        query.valid()
            && self.abi == ABI
            && self.scope == query.scope
            && self.request_id == query.request_id
            && self.original_request_id == query.original.request_id()
            && query
                .original
                .fingerprint(&query.scope.configuration)
                .as_ref()
                == Some(&self.fingerprint)
            && bounded(self)
            && match &self.receipt {
                Some(receipt) => {
                    !self.definitive_not_applied
                        && receipt.durable
                        && receipt.matches(&query.original, &query.scope.configuration)
                }
                None => {
                    !self.definitive_not_applied
                        || matches!(
                            &query.original,
                            Command::Configure {
                                journal: Some(_),
                                ..
                            }
                        )
                }
            }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FocusState {
    Unknown,
    Background,
    Foreground,
}

/// Native-only cached evidence: a connection incarnation set by the retained
/// service connection, and state/revision updated by native window focus events.
/// Never accepted as caller payload, a setting, a timestamp or a permission.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeFocus {
    pub connection_id: String,
    pub revision: u64,
    pub state: FocusState,
}
/// Original-child evidence from the canonical native Responder, never a
/// Configure/Focus request field. The service also verifies its kernel origin.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeControl {
    pub scope: Scope,
    pub mode: super::ownership::Mode,
    pub focus: Option<NativeFocus>,
}
impl NativeFocus {
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.connection_id)
            && self.revision <= MAX_REVISION
            && (self.state == FocusState::Unknown || self.revision > 0)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusReply {
    pub abi: String,
    pub request_id: String,
    pub scope: Scope,
    pub observation: NativeFocus,
    /// Remaining monotonic lease time, never an absolute wall-clock timestamp.
    pub ttl_ms: u64,
}
impl FocusReply {
    pub fn matches(&self, original: &Command, scope: &Scope) -> bool {
        matches!(original, Command::Focus { .. })
            && original.valid()
            && self.abi == ABI
            && self.scope.valid()
            && &self.scope == scope
            && self.request_id == original.request_id()
            && self.observation.valid()
            && self.ttl_ms <= FOCUS_TTL_MS
            && bounded(self)
            && (self.observation.state == FocusState::Foreground || self.ttl_ms == 0)
    }
}
fn bounded(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= BYTES)
}

#[cfg(test)]
mod tests;
