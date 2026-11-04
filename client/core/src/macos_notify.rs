//! Bounded macOS notification data policy, with no native or persistence calls.
//!
//! An envelope is untrusted correlation evidence, never a signed peer, current
//! lease, delivered mapping or activation permit. G1 owns those proofs and all
//! effects. The current GUI notifier does not use this proposed service format.
use serde::{Deserialize, Serialize};

use crate::{attention, creation::canonical_uuid};

#[cfg(all(target_os = "macos", feature = "macos-native-peer"))]
pub mod peer;

pub const USER_INFO_BYTES: usize = 16 * 1024;
pub const TOKEN_BYTES: usize = 8192;
pub const TOKEN_KEY: &str = "hgsAttentionToken";
pub const CORRELATION_KEY: &str = "hgsAttentionCorrelationV1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    Malformed,
    Token,
    Correlation,
    RequestId,
}

/// Original token and per-post correlation. Debug deliberately omits both.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope {
    token: String,
    correlation: String,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Envelope { .. }")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserInfo {
    #[serde(rename = "hgsAttentionToken")]
    token: String,
    // The key versions this format; absent is historical, explicit null invalid.
    #[serde(
        rename = "hgsAttentionCorrelationV1",
        default,
        deserialize_with = "present_correlation",
        skip_serializing_if = "Option::is_none"
    )]
    correlation: Option<String>,
}

fn present_correlation<'de, D: serde::Deserializer<'de>>(
    value: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(value).map(Some)
}

impl Envelope {
    pub fn new(token: &str, correlation: &str) -> Result<Self, Invalid> {
        valid_token(token)?;
        if !canonical_uuid(correlation) {
            return Err(Invalid::Correlation);
        }
        Ok(Self {
            token: token.into(),
            correlation: correlation.into(),
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn correlation(&self) -> &str {
        &self.correlation
    }

    /// Preserve Qt's exact identifier, without decoding/re-encoding its token.
    pub fn request_id(&self) -> String {
        attention::request_id(&self.token)
    }

    /// Bounded property-list string dictionary, represented as JSON for the
    /// future host's conversion. It is not an admitted IPC/control message.
    pub fn user_info(&self) -> Vec<u8> {
        // Both fields are bounded ASCII: a base64url token and canonical UUID.
        serde_json::to_vec(&UserInfo {
            token: self.token.clone(),
            correlation: Some(self.correlation.clone()),
        })
        .expect("two strings serialize")
    }
}

fn valid_token(token: &str) -> Result<(), Invalid> {
    if token.len() > TOKEN_BYTES || attention::target(token).is_none() {
        Err(Invalid::Token)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignore {
    NotDefaultAction,
    LegacyUnbound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ignored(Ignore),
    /// Merely well-formed data. G1 must authenticate ingress, look up the exact
    /// Delivered original mapping and recheck tombstone/target before claiming.
    Candidate(Envelope),
}

/// The host compares the action against the framework's default-action constant
/// before calling. No click target is resolved or accepted here. Unknown keys,
/// duplicate keys, invalid UTF-8 and future versions refuse instead of guessing.
pub fn response(
    default_action: bool,
    request_id: &str,
    user_info: &[u8],
) -> Result<Response, Invalid> {
    if !default_action {
        return Ok(Response::Ignored(Ignore::NotDefaultAction));
    }
    if user_info.len() > USER_INFO_BYTES
        || request_id.len() > "zerus.attention.".len() + TOKEN_BYTES
    {
        return Err(Invalid::TooLarge);
    }
    let info: UserInfo = serde_json::from_slice(user_info).map_err(|_| Invalid::Malformed)?;
    valid_token(&info.token)?;
    if request_id != attention::request_id(&info.token) {
        return Err(Invalid::RequestId);
    }
    let Some(correlation) = info.correlation else {
        return Ok(Response::Ignored(Ignore::LegacyUnbound));
    };
    Ok(Response::Candidate(Envelope::new(
        &info.token,
        &correlation,
    )?))
}

/// Observations supplied by the future native host, not execution authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostObservation {
    VerifiedNotIssued,
    MayHaveIssued,
    Completion { accepted: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// G1 must independently verify the before-call refusal before any retry.
    RefusedBeforeCall,
    /// Accepted for scheduling, never proof of visual display.
    AcceptedByFramework,
    Unknown,
}

pub fn post_outcome(observation: PostObservation) -> Outcome {
    match observation {
        PostObservation::VerifiedNotIssued => Outcome::RefusedBeforeCall,
        PostObservation::Completion { accepted: true } => Outcome::AcceptedByFramework,
        PostObservation::MayHaveIssued | PostObservation::Completion { accepted: false } => {
            Outcome::Unknown
        }
    }
}

/// The two asynchronous void removals have no native completion receipt.
/// Issued metadata does not confirm removal, even when both methods returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WithdrawalObservation {
    pub pending_issued: bool,
    pub delivered_issued: bool,
}

impl WithdrawalObservation {
    pub fn outcome(&self) -> Outcome {
        Outcome::Unknown
    }
}

#[cfg(test)]
mod tests;
