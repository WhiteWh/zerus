//! Authenticated activation replies and separate, non-authoritative UI navigation.
//! Decoding a frame or a token does not grant activation or writer authority.
use super::{digest, notifications::PlatformContext};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zerus_desktop_migration::canonical_uuid;

pub const ABI: &str = "desktop-activation-v1";
pub const HELP_MARKER: &str = "Desktop activation ABI: desktop-activation-v1";
/// GUI navigation only: this capability/argument never grants callback or
/// writer authority. Launch custody remains the service's opaque selection.
pub const NAVIGATION_ABI: &str = "desktop-target-v1";
pub const NAVIGATION_HELP_MARKER: &str = "Desktop target navigation ABI: desktop-target-v1";
pub const NAVIGATION_ARG: &str = "--desktop-target";
/// Service launch input is consumed before any GUI worker can inherit it.
pub const NAVIGATION_ENV: &str = "ZERUS_CLIENT_DESKTOP_TARGET";
pub const NAVIGATION_ENV_HELP_MARKER: &str =
    "Desktop target environment ABI: desktop-target-env-v1";
pub const NAVIGATION_BYTES: usize = 8192;

pub const WAIT: Duration = Duration::from_secs(15);
pub const PEER_WAIT: Duration = Duration::from_secs(20);
pub const CLIENT_WAIT: Duration = Duration::from_secs(25);
pub const QUEUE: usize = 32;
pub const MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub computer: String,
    pub run: String,
    pub conversation: String,
}
impl Target {
    pub fn valid(&self) -> bool {
        self.computer.len() <= 255
            && !self.computer.chars().any(char::is_control)
            && [&self.run, &self.conversation]
                .iter()
                .all(|v| !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control))
    }
}
pub fn encode_navigation(target: &Target) -> Option<String> {
    if !target.valid() {
        return None;
    }
    serde_json::to_string(target)
        .ok()
        .filter(|value| value.len() <= NAVIGATION_BYTES)
}
pub fn decode_navigation(value: &str) -> Option<Target> {
    if value.len() > NAVIGATION_BYTES {
        return None;
    }
    serde_json::from_str::<Target>(value)
        .ok()
        .filter(Target::valid)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Open {
    /// Ephemeral click identity, distinct from the original native post UUID.
    pub activation_id: String,
    pub post_request_id: String,
    pub target: Target,
    pub token: String,
    pub platform_context: Option<PlatformContext>,
}
impl Open {
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.activation_id)
            && canonical_uuid(&self.post_request_id)
            && self.target.valid()
            && self.token.len() <= super::notifications::TOKEN_BYTES
            && crate::attention::target(&self.token).is_some_and(|t| {
                t.get("host").and_then(serde_json::Value::as_str)
                    == Some(self.target.computer.as_str())
                    && t.get("run").and_then(serde_json::Value::as_str)
                        == Some(self.target.run.as_str())
                    && t.get("conversation")
                        .is_none_or(|v| v.as_str() == Some(self.target.conversation.as_str()))
            })
            && self
                .platform_context
                .as_ref()
                .is_none_or(|context| match context {
                    PlatformContext::Wayland { activation_token } => {
                        !activation_token.is_empty()
                            && activation_token.len()
                                <= super::notifications::ACTIVATION_TOKEN_BYTES
                            && !activation_token.chars().any(char::is_control)
                    }
                })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub abi: String,
    pub request_id: String,
    pub configuration: String,
    pub service_epoch: String,
    /// None is an ordinary idle timeout, not a source/ownership assertion.
    pub activation: Option<Open>,
}
impl Reply {
    pub fn matches(&self, request_id: &str, configuration: &str, service_epoch: &str) -> bool {
        self.abi == ABI
            && canonical_uuid(request_id)
            && self.request_id == request_id
            && digest(configuration)
            && self.configuration == configuration
            && canonical_uuid(service_epoch)
            && self.service_epoch == service_epoch
            && self.activation.as_ref().is_none_or(Open::valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_is_bounded_closed_target_data_without_callback_context() {
        let target = Target {
            computer: "peer.example".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        };
        let encoded = encode_navigation(&target).unwrap();
        assert_eq!(decode_navigation(&encoded), Some(target));
        for input in [
            "{}".to_owned(),
            "x".repeat(NAVIGATION_BYTES + 1),
            r#"{"computer":"peer","run":"r","conversation":"c","token":"injected"}"#.into(),
            r#"{"computer":"peer","run":"r\n","conversation":"c"}"#.into(),
            r#"{"computer":"peer","run":"","conversation":"c"}"#.into(),
        ] {
            assert!(decode_navigation(&input).is_none());
        }
    }

    #[test]
    fn exact_target_epoch_and_closed_request_shape() {
        let id = "11111111-1111-4111-8111-111111111111";
        let token=crate::attention::encode_token(serde_json::json!({"host":"peer.example","run":"run","conversation":"conversation","name":"fixture","notice":"reply"}).as_object().unwrap());
        let mut reply = Reply {
            abi: ABI.into(),
            request_id: id.into(),
            configuration: "a".repeat(64),
            service_epoch: id.into(),
            activation: Some(Open {
                activation_id: id.into(),
                post_request_id: id.into(),
                target: Target {
                    computer: "peer.example".into(),
                    run: "run".into(),
                    conversation: "conversation".into(),
                },
                token,
                platform_context: Some(PlatformContext::Wayland {
                    activation_token: "ephemeral".into(),
                }),
            }),
        };
        assert!(reply.matches(id, &"a".repeat(64), id));
        assert!(!reply.matches(id, &"b".repeat(64), id));
        reply.activation.as_mut().unwrap().target.conversation = "other".into();
        assert!(!reply.matches(id, &"a".repeat(64), id));
        let request = serde_json::json!({"op":"activation_wait","request_id":id});
        assert!(
            serde_json::from_value::<super::super::ui_requests::Request>(request.clone()).is_ok()
        );
        for field in ["token", "target", "confirmed", "role"] {
            let mut bad = request.clone();
            bad[field] = true.into();
            assert!(serde_json::from_value::<super::super::ui_requests::Request>(bad).is_err());
        }
    }
}
