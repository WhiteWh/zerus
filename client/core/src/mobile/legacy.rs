//! RelayConfiguration.h: private, explicit legacy configuration edits.
//! Snapshots and edits contain credentials and deliberately have no Debug or Serialize.
use serde::Deserialize;
#[cfg(unix)]
use serde_json::json;
use serde_json::Value;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

pub mod service;
#[cfg(all(test, unix))]
mod tests;
#[cfg(unix)]
mod unix;
pub mod windows;
pub const OFFICIAL: &str = "https://relay.zerus.dev";
#[cfg(unix)]
const PREVIOUS: &str = "https://zerus.dev.guthub.dev";
pub const CHANGED: &str = "Configuration changed outside this editor. Reload before saving.";
pub const UNKNOWN: &str =
    "Configuration was not confirmed saved. Reload to check; no connector restart was requested.";
pub const PRIVATE: &str = "Configuration directories must be owned, private and free of symlinks.";
pub const PRIVATE_FILE: &str =
    "Connector configuration must be an owned regular private file (0600), at most 64 KiB.";
pub const NOT_OBJECT: &str = "Connector configuration is not a JSON object.";
pub const UNREADABLE: &str = "Cannot read private connector configuration; symlinks are refused.";
pub const ENCRYPTED: &str = "Unavailable while this computer uses an encrypted workspace";
pub const SAVED: &str = "Configuration saved privately. Changes apply when the connector restarts. Service status does not verify relay connectivity or that this configuration is active.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub relay: String,
    pub replacement: String,
    pub directory: PathBuf,
}
/// Only a validation Refused admits correcting the original snapshot.
#[derive(Debug, PartialEq)]
pub enum Failure {
    Refused(String),
    Locked(String),
}
impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Self::Refused(s) | Self::Locked(s) => s,
        }
    }
}
impl From<String> for Failure {
    fn from(s: String) -> Self {
        Self::Locked(s)
    }
}
impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Self::Locked(s.into())
    }
}
#[derive(Default)]
pub struct Snapshot {
    pub(crate) object: Value,
    bytes: Option<Vec<u8>>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl Snapshot {
    pub fn exists(&self) -> bool {
        self.bytes.is_some()
    }
    pub fn relay(&self) -> &str {
        self.object["server_url"]
            .as_str()
            .filter(|s| super::valid_relay(s))
            .unwrap_or("")
    }
    pub fn directory(&self, home: &Path) -> PathBuf {
        self.object["state_dir"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| default_directory(home))
    }
    pub fn encrypted(&self) -> bool {
        self.object.get("encrypted_workspace").is_some()
    }
}
pub fn default_directory(home: &Path) -> PathBuf {
    home.join(".local/state/hgs/mobile-connector")
}
pub fn new_directory(home: &Path, nonce: &str) -> PathBuf {
    home.join(format!(".local/state/hgs/mobile-connector-{nonce}"))
}
#[cfg(unix)]
fn clean(path: &Path) -> bool {
    let normalized: PathBuf = path.components().collect();
    path.as_os_str().as_encoded_bytes().len() <= 4096
        && normalized.as_os_str() == path.as_os_str()
        && path.is_absolute()
        && path.components().all(|c| {
            matches!(
                c,
                Component::RootDir | Component::Normal(_) | Component::Prefix(_)
            )
        })
        && !path.as_os_str().to_string_lossy().contains("/./")
}
fn token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 8192 && s.bytes().all(|b| (33..=126).contains(&b))
}
#[cfg(unix)]
fn managed(s: &str) -> Option<&'static str> {
    for origin in [OFFICIAL, PREVIOUS] {
        if s == origin || s == format!("{origin}:443") {
            return Some(origin);
        }
    }
    None
}
#[cfg(unix)]
struct Prepared {
    object: Value,
    new_directory: bool,
}
#[cfg(unix)]
fn prepare(old: &Snapshot, edit: Edit, home: &Path) -> Result<Prepared, String> {
    if old.encrypted() {
        return Err(ENCRYPTED.into());
    }
    let relay = edit.relay.trim_end_matches('/');
    if !super::valid_relay(relay) {
        return Err("Enter an HTTPS relay URL without credentials, query or fragment.".into());
    }
    if managed(relay) == Some(PREVIOUS) {
        return Err("The previous official relay is retiring. Use https://relay.zerus.dev.".into());
    }
    let previous = old.object["server_url"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('/');
    let old_token = old.object["node_token"].as_str().unwrap_or("");
    let replacement = if edit.replacement.is_empty() {
        old_token
    } else {
        &edit.replacement
    };
    if !token(replacement) {
        return Err("Supply the computer credential issued by this relay.".into());
    }
    let identity = match old.object.get("identity_url") {
        None|Some(Value::Null) => previous,
        Some(Value::String(s)) => s.trim_end_matches('/'),
        _ => return Err("The existing receipt identity must be a URL string. Review the connector configuration before saving.".into()),
    };
    if old.exists() && (!super::valid_relay(previous) || !token(old_token)) {
        return Err("The existing configuration is invalid. Correct it using the connector setup documentation.".into());
    }
    if old.exists()
        && identity != previous
        && !(managed(previous) == Some(OFFICIAL) && managed(identity) == Some(PREVIOUS))
    {
        return Err("The existing receipt identity is invalid. Review the connector configuration before saving.".into());
    }
    let equivalent = old.exists()
        && (relay == previous || (managed(relay).is_some() && managed(previous).is_some()));
    let new_directory = !old.exists() || !equivalent || replacement != old_token;
    if !new_directory
        && identity != relay
        && !(managed(relay) == Some(OFFICIAL) && managed(identity) == Some(PREVIOUS))
    {
        return Err("Keep this relay URL's existing spelling to preserve its receipt journal. A new binding requires a replacement credential and a new receipt directory.".into());
    }
    if old.exists() && !equivalent && (edit.replacement.is_empty() || replacement == old_token) {
        return Err("A different relay requires its own new computer credential. Existing credentials cannot be reused.".into());
    }
    if !clean(&edit.directory) {
        return Err(
            "Choose an absolute private receipt directory without relative path segments.".into(),
        );
    }
    let prior = old.directory(home);
    if new_directory && (edit.directory == prior || edit.directory.symlink_metadata().is_ok()) {
        return Err(
            "Use a new receipt directory. The previous journal must remain untouched.".into(),
        );
    }
    if !new_directory && edit.directory != prior {
        return Err("Keep the receipt directory unchanged for this relay and credential.".into());
    }
    let mut object = if old.exists() {
        old.object.clone()
    } else {
        json!({"hgs_path":"hgs","poll_interval":5})
    };
    object["server_url"] = json!(relay);
    object["node_token"] = json!(replacement);
    object["state_dir"] = json!(edit.directory);
    if !new_directory && managed(relay) == Some(OFFICIAL) && managed(identity) == Some(PREVIOUS) {
        object["identity_url"] = json!(identity);
    } else {
        object
            .as_object_mut()
            .ok_or(PRIVATE)?
            .remove("identity_url");
    }
    Ok(Prepared {
        object,
        new_directory,
    })
}
pub fn load(path: &Path) -> Result<Snapshot, String> {
    #[cfg(unix)]
    {
        unix::load(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PRIVATE.into())
    }
}
pub fn save(path: &Path, old: &Snapshot, edit: Edit, home: &Path) -> Result<Snapshot, Failure> {
    #[cfg(unix)]
    {
        if old.encrypted() {
            return Err(Failure::Locked(ENCRYPTED.into()));
        }
        let prepared = prepare(old, edit, home).map_err(Failure::Refused)?;
        unix::save(path, old, prepared).map_err(Failure::Locked)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, old, edit, home);
        Err(PRIVATE.into())
    }
}
