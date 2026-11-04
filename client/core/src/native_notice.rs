//! Explicit notification routing classes, not native ownership or authority.
//! The caller supplies the expected class. A valid token cannot classify legacy
//! evidence, prove an original post, or authorize withdrawal or activation.
use crate::{attention, updates};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    AgentAttention,
    SoftwareUpdate,
}
impl Owner {
    /// Validate against an already selected class; never infer a native owner
    /// from this result. Both existing token formats and their bounds are reused.
    pub fn accepts_token(self, token: &str) -> bool {
        match self {
            Self::AgentAttention => attention::target(token).is_some(),
            Self::SoftwareUpdate => updates::is_update_token(token),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent() -> String {
        attention::encode_token(
            serde_json::json!({"host":"","name":"codex/example/one","notice":"example"})
                .as_object()
                .unwrap(),
        )
    }
    #[test]
    fn expected_owner_domains_are_disjoint_and_never_classify_legacy_ids() {
        let agent = agent();
        let update = "zerus-update:v2:dev:123";
        assert!(Owner::AgentAttention.accepts_token(&agent));
        assert!(!Owner::SoftwareUpdate.accepts_token(&agent));
        assert!(Owner::SoftwareUpdate.accepts_token(update));
        assert!(!Owner::AgentAttention.accepts_token(update));
        for token in [
            "legacy-unbound",
            "zerus.attention.other",
            "e30",
            "zerus-update:v2:dev:0123",
            "",
        ] {
            assert!(!Owner::AgentAttention.accepts_token(token));
            assert!(!Owner::SoftwareUpdate.accepts_token(token));
        }
    }
    #[test]
    fn qt_padding_and_token_bounds_follow_existing_decoders() {
        assert!(Owner::AgentAttention.accepts_token(&(agent() + "=")));
        assert!(!Owner::AgentAttention.accepts_token(&"e".repeat(8193)));
        assert!(!Owner::SoftwareUpdate.accepts_token("zerus-update:v2:dev:12345678901234567"));
    }
}
