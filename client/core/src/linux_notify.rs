//! Pure Linux notification compatibility policy, shared with the native adapter.
//!
//! These are retained data, not authenticated endpoints or execution authority.
//! The caller owns secure bounded reads, current bus/daemon authentication,
//! durable writes and all OS effects. This module opens no files or connections.
//! Importers must retain the original bytes (including unknown fields), rather
//! than serialize this projection as proof of the original source.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Combined retained IDs and unknown-delivery tokens, as in the GUI adapter.
pub const LIMIT: usize = 8192;
/// Maximum serialized saved-state bytes; readers must enforce this before parse.
pub const FILE_LIMIT: u64 = 16 * 1024 * 1024;

/// Existing Next schema-1 saved IDs. Qt seeds have an empty `bus_id` and retain
/// suppression only. Public fields support the existing writer; constructing or
/// decoding this value does not establish current bus lineage or post authority.
#[derive(Default, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    /// Unique daemon names are scoped to one bus generation. Older Next/Qt
    /// records omit this and retain suppression only, never numeric-ID authority.
    #[serde(default)]
    pub bus_id: String,
    pub owner: String,
    pub tokens: BTreeMap<u32, String>,
    /// A reservation survives an unknown Notify result. It is never reposted
    /// automatically: a late native banner may already exist without its ID.
    #[serde(default)]
    pub uncertain: BTreeSet<String>,
}
impl Saved {
    /// Existing schema/size/identity checks, also used after decoding Qt seeds.
    pub fn valid(&self) -> bool {
        self.version == 1
            && (self.bus_id.is_empty()
                || (self.bus_id.len() == 32 && self.bus_id.bytes().all(|b| b.is_ascii_hexdigit())))
            && self.tokens.len() + self.uncertain.len() <= LIMIT
            && self
                .tokens
                .iter()
                .all(|(id, token)| *id > 0 && !token.is_empty() && token.len() <= 16384)
            && self
                .uncertain
                .iter()
                .all(|token| !token.is_empty() && token.len() <= 16384)
    }

    /// Quarantine IDs outside the caller's current authenticated namespace.
    /// Unknown tokens survive every transition until their attention edge is
    /// withdrawn. The caller must clear transient activation tokens and persist
    /// this change before using the new namespace. Returns whether it changed.
    /// This pure transition cannot authenticate the supplied namespace.
    pub fn adopt_namespace(&mut self, current: &Namespace) -> bool {
        if Namespace::saved(self) == *current
            && (!current.owner.is_empty() || self.tokens.is_empty())
        {
            return false;
        }
        self.uncertain
            .extend(std::mem::take(&mut self.tokens).into_values());
        self.bus_id.clone_from(&current.bus_id);
        self.owner.clone_from(&current.owner);
        true
    }
}

/// Bounded projection of Next's saved-state bytes, with the existing validation
/// and diagnostic texts. Does not normalize, repair, seed, persist or execute.
pub fn decode_saved(bytes: &[u8]) -> Result<Saved, String> {
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("Notification identities exceed the storage limit".into());
    }
    let saved: Saved = serde_json::from_slice(bytes)
        .map_err(|e| format!("Could not read notification identities: {e}"))?;
    if !saved.valid() {
        return Err(
            "Unsupported or invalid notification identities; the original file was preserved"
                .into(),
        );
    }
    Ok(saved)
}

/// Bus-generation and unique-daemon-name data. The type is intentionally not a
/// verified connection/lease/ticket; only a live authenticated caller can supply
/// that proof. Numeric IDs are meaningful only within this exact pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Namespace {
    pub bus_id: String,
    pub owner: String,
}
impl Namespace {
    pub fn saved(saved: &Saved) -> Self {
        Self {
            bus_id: saved.bus_id.clone(),
            owner: saved.owner.clone(),
        }
    }
}

/// Qt-compatible Freedesktop body markup escaping. Session text remains literal
/// even when a daemon supports markup; this does not escape summaries or links.
pub fn escape_body(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub mod owned;

#[cfg(all(target_os = "linux", feature = "linux-native-notify"))]
pub mod transport;

#[cfg(test)]
mod tests;
