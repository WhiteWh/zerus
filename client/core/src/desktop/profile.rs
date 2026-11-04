//! Canonical profile definition data, never current ownership or permission.
//! No producer, journal, role gate, source-family allowlist or caller is added.
use super::drain::Role;
use crate::native_notice::Owner;
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"hgs-desktop-profile-v1\0";
const ID: &str = "g1-background-v1";
const ROLES: [Role; 6] = [
    Role::Fleet,
    Role::Attention,
    Role::NativePosting,
    Role::Badges,
    Role::ReadDelivery,
    Role::Presence,
];

// Literal concatenation makes these bytes independent of checkout line endings,
// host paths, runtime JSON formatting or dependency feature unification.
const CANONICAL: &str = concat!(
    r#"{"id":"g1-background-v1","native_notice_owner":"agent_attention","roles":["#,
    r#"{"permissions":["fleet_source"],"role":"fleet"},"#,
    r#"{"permissions":["attention_intent"],"role":"attention"},"#,
    r#"{"permissions":["agent_post","agent_withdrawal","agent_activation"],"role":"native_posting"},"#,
    r#"{"permissions":["attention_badge"],"role":"badges"},"#,
    r#"{"permissions":["read_delivery"],"role":"read_delivery"},"#,
    r#"{"permissions":["presence_publish"],"role":"presence"}],"version":1}"#,
);

/// Names the service capabilities described by this definition. Values do not
/// enable a capability or establish any current role/permission assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    FleetSource,
    AttentionIntent,
    AgentPost,
    AgentWithdrawal,
    AgentActivation,
    AttentionBadge,
    ReadDelivery,
    PresencePublish,
}
impl Permission {
    pub fn name(self) -> &'static str {
        match self {
            Self::FleetSource => "fleet_source",
            Self::AttentionIntent => "attention_intent",
            Self::AgentPost => "agent_post",
            Self::AgentWithdrawal => "agent_withdrawal",
            Self::AgentActivation => "agent_activation",
            Self::AttentionBadge => "attention_badge",
            Self::ReadDelivery => "read_delivery",
            Self::PresencePublish => "presence_publish",
        }
    }
}

/// Immutable, copyable definition only. Unlike a future current runtime proof,
/// it has no caller identity, owner, epoch, lease, fresh drain or consent.
#[derive(Clone, Copy, Debug)]
pub struct Definition(());
pub const BACKGROUND_V1: Definition = Definition(());

impl Definition {
    pub fn id(self) -> &'static str {
        ID
    }
    pub fn version(self) -> u32 {
        1
    }
    pub fn roles(self) -> &'static [Role] {
        &ROLES
    }
    pub fn native_notice_owner(self) -> Owner {
        Owner::AgentAttention
    }
    pub fn includes(self, role: Role) -> bool {
        ROLES.contains(&role)
    }
    pub fn permissions(self, role: Role) -> &'static [Permission] {
        use Permission::*;
        match role {
            Role::Fleet => &[FleetSource],
            Role::Attention => &[AttentionIntent],
            Role::NativePosting => &[AgentPost, AgentWithdrawal, AgentActivation],
            Role::Badges => &[AttentionBadge],
            Role::ReadDelivery => &[ReadDelivery],
            Role::Presence => &[PresencePublish],
            _ => &[],
        }
    }
    pub fn role_name(self, role: Role) -> Option<&'static str> {
        Some(match role {
            Role::Fleet => "fleet",
            Role::Attention => "attention",
            Role::NativePosting => "native_posting",
            Role::Badges => "badges",
            Role::ReadDelivery => "read_delivery",
            Role::Presence => "presence",
            _ => return None,
        })
    }
    /// Exact set comparison, not proof that any of these roles is instrumented,
    /// owned, idle or flushed. Missing, extra and duplicate declarations refuse.
    pub fn matches_roles(self, supplied: &[Role]) -> bool {
        supplied.len() == ROLES.len() && ROLES.iter().all(|role| supplied.contains(role))
    }
    pub fn canonical_bytes(self) -> &'static [u8] {
        CANONICAL.as_bytes()
    }
    /// Lowercase SHA256(domain || canonical bytes). No clock or platform input.
    pub fn hash(self) -> String {
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(self.canonical_bytes());
        format!("{:x}", hash.finalize())
    }
    /// Definition equality only; a matching declaration remains unauthorised.
    /// Existing hello/challenge handlers do not call or advertise this helper.
    pub fn matches(self, version: u32, id: &str, hash: &str) -> bool {
        version == self.version() && id == ID && super::digest(hash) && hash == self.hash()
    }
}

#[cfg(test)]
mod tests;
