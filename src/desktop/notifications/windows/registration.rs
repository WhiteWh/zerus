//! Explicit installer operation; the runtime never creates or repairs registry state.
use super::wire;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(windows)]
mod native;
#[cfg(test)]
mod tests;

pub(super) const CLASS_ID: &str = "{0F0F70A2-1D40-47D8-AC5E-D6A9B789605F}";
const APPLICATION_KEY: &str = r"Software\Classes\AppUserModelId\com.hgdev.hgs-tray";
const ABI_VALUE: &str = "HGSDesktopNotificationABI";
const CONFIGURATION_VALUE: &str = "HGSDesktopConfiguration";
pub(in crate::desktop) const MARKER: &str = "desktop-notification-registration-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Contents {
    values: BTreeMap<String, String>,
    children: BTreeSet<String>,
}
#[derive(Clone)]
struct Plan {
    configuration: String,
    sid: String,
    keys: Vec<(String, Contents)>,
}
fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}
impl Plan {
    fn new(configuration: &str, sid: &str, command: &str) -> Result<Self, &'static str> {
        if configuration.len() != 64
            || !configuration
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !sid.starts_with("S-1-")
            || sid.len() > 184
            || !sid
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'S' | b'-'))
            || matches!(sid, "S-1-5-18" | "S-1-5-19" | "S-1-5-20")
            || sid.starts_with("S-1-5-80-")
            || command.is_empty()
            || command.encode_utf16().count() >= 32767
            || command.chars().any(char::is_control)
        {
            return Err("notification_registration_plan_refused");
        }
        let class = format!(r"Software\Classes\CLSID\{CLASS_ID}");
        // Advertise the AUMID last; no worker or background process is started.
        Ok(Self {
            configuration: configuration.into(),
            sid: sid.into(),
            keys: vec![
                (
                    class.clone(),
                    Contents {
                        values: values(&[
                            (ABI_VALUE, wire::ABI),
                            (CONFIGURATION_VALUE, configuration),
                        ]),
                        children: ["LocalServer32".into()].into_iter().collect(),
                    },
                ),
                (
                    format!(r"{class}\LocalServer32"),
                    Contents {
                        values: values(&[("", command)]),
                        children: BTreeSet::new(),
                    },
                ),
                (
                    APPLICATION_KEY.into(),
                    Contents {
                        values: values(&[
                            (ABI_VALUE, wire::ABI),
                            (CONFIGURATION_VALUE, configuration),
                            ("CustomActivator", CLASS_ID),
                            ("DisplayName", "Zerus"),
                        ]),
                        children: BTreeSet::new(),
                    },
                ),
            ],
        })
    }
    fn inspect(&self, registry: &mut impl Registry) -> Result<bool, &'static str> {
        let mut present = 0;
        for (path, wanted) in &self.keys {
            if let Some(actual) = registry.read(path)? {
                if &actual != wanted {
                    return Err("foreign_or_incomplete_notification_registration_kept");
                }
                present += 1;
            }
        }
        if present != 0 && present != self.keys.len() {
            return Err("incomplete_notification_registration_kept");
        }
        Ok(present != 0)
    }
    fn apply(&self, registry: &mut impl Registry, operation: &str) -> Result<Reply, Failure> {
        if !matches!(operation, "status" | "install" | "remove") {
            return Err(Failure::before(
                "invalid_notification_registration_operation",
            ));
        }
        let registered = self.inspect(registry).map_err(Failure::before)?;
        if operation == "status" || (operation == "install") == registered {
            return Ok(Reply {
                registered,
                changed: false,
            });
        }
        // Native create is create-only. Each removal snapshots the opened key, then
        // deletes its fixed path. Failure never rolls back or deletes evidence.
        if operation == "install" {
            for (path, contents) in &self.keys {
                registry
                    .create_new(path, &contents.values)
                    .map_err(Failure::uncertain)?;
            }
        } else {
            for (path, contents) in self.keys.iter().rev() {
                let mut expected = contents.clone();
                expected.children.clear();
                registry
                    .remove_exact(path, &expected)
                    .map_err(Failure::uncertain)?;
            }
        }
        let after = self.inspect(registry).map_err(Failure::uncertain)?;
        if after != (operation == "install") {
            return Err(Failure::uncertain(
                "notification_registration_readback_refused",
            ));
        }
        Ok(Reply {
            registered: after,
            changed: true,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    registered: bool,
    changed: bool,
}
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    reason: &'static str,
    uncertain: bool,
}
impl Failure {
    fn before(reason: &'static str) -> Self {
        Self {
            reason,
            uncertain: false,
        }
    }
    fn uncertain(reason: &'static str) -> Self {
        Self {
            reason,
            uncertain: true,
        }
    }
}
trait Registry {
    fn read(&mut self, path: &str) -> Result<Option<Contents>, &'static str>;
    fn create_new(
        &mut self,
        path: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), &'static str>;
    fn remove_exact(&mut self, path: &str, contents: &Contents) -> Result<(), &'static str>;
}

#[cfg(windows)]
pub(in crate::desktop) fn command(
    config: &crate::config::Config,
    operation: &str,
    request_id: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    native::command(config, operation, request_id)
}

// Called only for the response read from the selected authenticated service;
// caller JSON is never an admission source. Keep the original status epoch.
fn settled_revoke(value: &serde_json::Value, id: &str, configuration: &str, epoch: &str) -> bool {
    zerus_desktop_migration::canonical_uuid(id)
        && zerus_desktop_migration::canonical_uuid(epoch)
        && value["request_id"].as_str() == Some(id)
        && value["configuration"].as_str() == Some(configuration)
        && value["writer_epoch"].as_str() == Some(epoch)
        && ["accepted", "durable", "revoked", "checkpointed"]
            .iter()
            .all(|field| value[*field] == true)
}
