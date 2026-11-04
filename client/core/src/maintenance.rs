//! Closed data contract for the desktop actor's maintenance lane. These values
//! carry preferences and display results, never native-frontend authority.
use crate::{recovery_settings, updates::Channel};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, path::Path};

pub const ABI: &str = "desktop-maintenance-v1";
pub const HELP_MARKER: &str = "Desktop maintenance ABI: desktop-maintenance-v1";
pub const CONFIG_LIMIT: usize = zerus_watch_protocol::CONTROL_LINE_BYTES - 4096;
pub const POLICY_LIMIT: usize = CONFIG_LIMIT / 2;

fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    decoder: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(decoder)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiIdentity {
    pub bundle_id: String,
    pub executable: String,
    pub app_version: String,
    pub dev_number: u64,
    pub source_commit: String,
}

impl GuiIdentity {
    pub fn valid(&self) -> bool {
        matches!(
            self.bundle_id.as_str(),
            "com.hgdev.hgs-tray" | "dev.zerus.next"
        ) && Path::new(&self.executable).is_absolute()
            && self.executable.len() <= 4096
            && !self.executable.chars().any(char::is_control)
            && !self.app_version.is_empty()
            && self.app_version.len() <= 180
            && !self.app_version.chars().any(char::is_control)
            && self.dev_number <= i64::MAX as u64
            && (self.source_commit.is_empty()
                || self.source_commit.len() == 40
                    && self.source_commit.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfiguration {
    pub gui: GuiIdentity,
    pub channel: Channel,
    pub automatic: bool,
}

/// The existing recovery form with a closed wire shape. Its policy semantics
/// and validation remain in recovery_settings::policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryForm {
    pub enabled: bool,
    pub rules: BTreeMap<String, bool>,
    pub session_limit_mode: String,
    pub delays: BTreeMap<String, String>,
}
impl From<RecoveryForm> for recovery_settings::Form {
    fn from(form: RecoveryForm) -> Self {
        Self {
            enabled: form.enabled,
            rules: form.rules,
            session_limit_mode: form.session_limit_mode,
            delays: form.delays,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfiguration {
    /// None starts policy synchronization without submitting a manual save.
    /// A supplied form is a one-shot edit; restart never replays it.
    #[serde(deserialize_with = "required")]
    pub policy: Option<RecoveryForm>,
    pub base: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    #[serde(deserialize_with = "required")]
    pub update: Option<UpdateConfiguration>,
    #[serde(deserialize_with = "required")]
    pub recovery: Option<RecoveryConfiguration>,
}
impl Configuration {
    pub fn valid(&self) -> bool {
        if self.update.as_ref().is_some_and(|u| !u.gui.valid()) {
            return false;
        }
        if let Some(recovery) = &self.recovery {
            if !recovery.base.is_empty()
                && (!recovery.base.get("policy").is_some_and(Value::is_object)
                    || !recovery
                        .base
                        .get("order")
                        .and_then(Value::as_array)
                        .is_some_and(|v| v.len() == 2))
            {
                return false;
            }
            if serde_json::to_vec(&recovery.base).map_or(true, |v| v.len() > POLICY_LIMIT) {
                return false;
            }
            if let Some(form) = &recovery.policy {
                if !matches!(form.session_limit_mode.as_str(), "reset" | "interval")
                    || form.rules.len() > recovery_settings::RULES.len()
                    || form.delays.len() > recovery_settings::RULES.len()
                    || form.rules.keys().chain(form.delays.keys()).any(|key| {
                        !recovery_settings::RULES
                            .iter()
                            .any(|(known, _)| key == known)
                    })
                    || form.delays.values().any(|value| value.len() > 128)
                    || recovery_settings::policy(&recovery.base, &form.clone().into()).is_err()
                {
                    return false;
                }
            }
        }
        serde_json::to_vec(self).is_ok_and(|v| v.len() <= CONFIG_LIMIT)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "maintenance_configure")]
    Configure {
        request_id: String,
        #[serde(deserialize_with = "required")]
        update: Option<UpdateConfiguration>,
        #[serde(deserialize_with = "required")]
        recovery: Option<RecoveryConfiguration>,
    },
    #[serde(rename = "maintenance_result")]
    Result {
        request_id: String,
        original_request_id: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjection {
    pub ready: bool,
    pub current_origin: bool,
    pub busy: bool,
    pub available: bool,
    pub channel: Option<Channel>,
    pub automatic: bool,
    pub feed: crate::updates::Feed,
    pub installation: crate::updates::Installation,
    pub comparison: i32,
    pub compared: bool,
    pub last_attempt: i64,
    pub last_success: i64,
    pub cached: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryProjection {
    pub ready: bool,
    pub current_origin: bool,
    pub status: String,
    pub error: String,
    pub busy: bool,
    pub can_save: bool,
    pub known: bool,
    pub has_machines: bool,
    pub has_policy: bool,
    pub saves: u64,
    pub envelope: Map<String, Value>,
}

/// A service watch projection, not a permission or configuration receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub abi: String,
    pub epoch: String,
    pub revision: Option<u64>,
    pub ready: bool,
    pub error: Option<String>,
    pub configuration: Option<Configuration>,
    pub updates: UpdateProjection,
    pub recovery: RecoveryProjection,
}
impl Projection {
    /// The transport independently validates its outer watch epoch/sequence.
    /// A missing/incompatible maintenance ABI never enables a UI poll fallback.
    pub fn valid_for(&self, epoch: &str) -> bool {
        self.abi == ABI
            && self.epoch == epoch
            && !epoch.is_empty()
            && epoch.len() <= 128
            && self.revision.is_none_or(|n| n <= i64::MAX as u64)
            && self.configuration.as_ref().is_none_or(Configuration::valid)
            && self.error.as_ref().is_none_or(|s| s.len() <= 2048)
            && serde_json::to_vec(self)
                .is_ok_and(|v| v.len() <= zerus_watch_protocol::MAX_FRAME_BYTES / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configure_requires_explicit_values_or_null_and_rejects_new_fields() {
        let base = json!({"op":"maintenance_configure", "request_id":"request", "update":null,"recovery":null});
        assert!(serde_json::from_value::<Request>(base.clone()).is_ok());
        for key in ["update", "recovery"] {
            let mut missing = base.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        let mut extra = base;
        extra["authority"] = true.into();
        assert!(serde_json::from_value::<Request>(extra).is_err());
    }

    #[test]
    fn installed_gui_identity_is_explicit_and_never_synthesized_from_cli() {
        let gui = GuiIdentity {
            bundle_id: "com.hgdev.hgs-tray".into(),
            executable: std::env::temp_dir()
                .join("example/hgs-tray")
                .to_string_lossy()
                .into(),
            app_version: "1.57.0-dev.42".into(),
            dev_number: 42,
            source_commit: "a".repeat(40),
        };
        assert!(gui.valid());
        assert!(!GuiIdentity {
            executable: "relative".into(),
            ..gui.clone()
        }
        .valid());
        assert!(!GuiIdentity {
            bundle_id: "unrelated".into(),
            ..gui.clone()
        }
        .valid());
        assert!(!GuiIdentity {
            source_commit: "unknown".into(),
            ..gui
        }
        .valid());
    }

    #[test]
    fn policy_base_and_whole_configuration_are_bounded_before_commit() {
        let empty = Configuration {
            update: None,
            recovery: None,
        };
        assert!(empty.valid());
        let mut base = Map::new();
        base.insert("policy".into(), "x".repeat(POLICY_LIMIT).into());
        let oversized = Configuration {
            update: None,
            recovery: Some(RecoveryConfiguration { policy: None, base }),
        };
        assert!(!oversized.valid());
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn projection_requires_maintenance_abi_and_the_transport_epoch() {
        let mut view = Projection {
            abi: ABI.into(),
            epoch: "epoch".into(),
            revision: None,
            ready: false,
            error: None,
            configuration: None,
            updates: UpdateProjection::default(),
            recovery: RecoveryProjection::default(),
        };
        assert!(view.valid_for("epoch"));
        assert!(!view.valid_for("other"));
        view.abi = "old-abi".into();
        assert!(!view.valid_for("epoch"));
        let mut json = serde_json::to_value(view).unwrap();
        json["permission"] = true.into();
        assert!(serde_json::from_value::<Projection>(json).is_err());
    }
}
