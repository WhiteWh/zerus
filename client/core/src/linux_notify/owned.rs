//! Passive owned-notification data, never a native ticket, current namespace
//! proof or release permission. No caller, source reader or effect lives here.
//!
//! The future optional writer uses a sibling file; schema1/default authority
//! remains unchanged. Original bytes and opaque fields belong to G3 evidence,
//! never this projection. There are no reservation/result/withdrawal transitions
//! yet; those need exact instance, owner/token and namespace-generation binding.
use super::{Namespace, Saved, FILE_LIMIT, LIMIT};
use crate::native_notice::Owner;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

pub mod data;
mod decode;
pub mod inspection;
pub use data::transitions;
mod selected;
#[cfg(test)]
mod tests;

pub const FILE: &str = "linux-notifications-owned.json";
/// Refuse the optional constructor above this cap BEFORE any ownership change.
/// Never fall back implicitly after service handoff, or evict accepted evidence.
pub const LEGACY_LIMIT: usize = LIMIT / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    TooLarge,
    Malformed,
    UnsupportedVersion,
    InvalidIdentity,
    InvalidNamespace,
    DuplicateIdentity,
    LegacyCapacity,
    UnknownFields,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tag {
    AgentAttention,
    SoftwareUpdate,
}
impl Tag {
    fn from_owner(owner: Owner) -> Self {
        match owner {
            Owner::AgentAttention => Self::AgentAttention,
            Owner::SoftwareUpdate => Self::SoftwareUpdate,
        }
    }
    fn owner(self) -> Owner {
        match self {
            Self::AgentAttention => Owner::AgentAttention,
            Self::SoftwareUpdate => Owner::SoftwareUpdate,
        }
    }
    fn text(self) -> &'static str {
        match self {
            Self::AgentAttention => "agent_attention",
            Self::SoftwareUpdate => "software_update",
        }
    }
}

/// The expected owner is supplied explicitly, never inferred from a token.
/// It validates retained data only; it does not bind an OS ID or authorize work.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedToken {
    tag: Tag,
    token: String,
}
impl ScopedToken {
    pub fn new(owner: Owner, token: String) -> Result<Self, Refused> {
        if !owner.accepts_token(&token) {
            return Err(Refused::InvalidIdentity);
        }
        Ok(Self {
            tag: Tag::from_owner(owner),
            token,
        })
    }
    pub fn owner(&self) -> Owner {
        self.tag.owner()
    }
    pub fn token(&self) -> &str {
        &self.token
    }
}
impl std::fmt::Debug for ScopedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScopedToken([retained data])")
    }
}

/// Not serde-Serialize: opaque same-version extensions must never be dropped by
/// an accidental projection write. Only encode() may emit this retained format.
pub struct OwnedSaved {
    namespace: Option<Namespace>,
    bindings: BTreeMap<u32, ScopedToken>,
    uncertain: BTreeSet<ScopedToken>,
    legacy: BTreeSet<String>,
    has_unknown: bool,
    raw_entries: usize,
    selected: Option<(Owner, data::Rows)>,
}
impl std::fmt::Debug for OwnedSaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedSaved([retained data])")
    }
}
impl OwnedSaved {
    /// Validated typed-data convenience ONLY. Saved has coalesced raw members;
    /// use quarantine_legacy(bytes) for any raw schema1 quarantine. Neither is
    /// original-source evidence. Every ownerless ID loses numeric authority.
    pub fn from_legacy(saved: Saved) -> Result<Self, Refused> {
        if !saved.valid() {
            return Err(Refused::InvalidIdentity);
        }
        let raw_entries = saved.tokens.len() + saved.uncertain.len();
        if raw_entries > LEGACY_LIMIT {
            return Err(Refused::LegacyCapacity);
        }
        let result = Self {
            namespace: None,
            bindings: BTreeMap::new(),
            uncertain: BTreeSet::new(),
            legacy: saved.tokens.into_values().chain(saved.uncertain).collect(),
            has_unknown: false,
            raw_entries,
            selected: None,
        };
        // Typed callers have no raw byte length: still bound the possible output.
        result.encode()?;
        Ok(result)
    }
    pub fn namespace(&self) -> Option<&Namespace> {
        self.namespace.as_ref()
    }
    pub fn bindings(&self) -> impl ExactSizeIterator<Item = (u32, &ScopedToken)> {
        self.bindings.iter().map(|(id, token)| (*id, token))
    }
    pub fn uncertain(&self) -> impl ExactSizeIterator<Item = &ScopedToken> {
        self.uncertain.iter()
    }
    pub fn legacy_uncertain(&self) -> impl ExactSizeIterator<Item = &str> {
        self.legacy.iter().map(String::as_str)
    }
    pub fn has_unknown(&self) -> bool {
        self.has_unknown
    }
    /// Parse/typed-input count before coalescing, not original-source proof.
    pub fn raw_entries(&self) -> usize {
        self.raw_entries
    }
    /// Explicitly selected row data only, never a current ownership proof.
    pub fn rows(&self) -> Option<&data::Rows> {
        self.selected.as_ref().map(|(_, rows)| rows)
    }

    /// Pure bounded encoding only. A later writer must read/check the existing
    /// sibling record, preserve schema1 bytes and refuse an unknown extension.
    pub fn encode(&self) -> Result<Vec<u8>, Refused> {
        if self.has_unknown {
            return Err(Refused::UnknownFields);
        }
        if let Some((owner, rows)) = &self.selected {
            return selected::encode(self, *owner, rows);
        }
        let namespace = self.namespace.as_ref();
        let wire = Wire {
            version: 2,
            bus_id: namespace.map_or("", |ns| ns.bus_id.as_str()),
            owner: namespace.map_or("", |ns| ns.owner.as_str()),
            tokens: Bindings(&self.bindings),
            uncertain: Uncertain(&self.uncertain),
            legacy_uncertain: &self.legacy,
        };
        let mut output = Capped(Vec::new());
        serde_json::to_writer(&mut output, &wire).map_err(|_| Refused::TooLarge)?;
        Ok(output.0)
    }
}

/// Decode schema2 data only, not a current daemon/lease/receipt. The caller
/// enforces secure reads and retains original bytes/hash/order for evidence.
pub fn decode_owned(bytes: &[u8]) -> Result<OwnedSaved, Refused> {
    decode::read(bytes, true)
}
/// Same bounded raw-member parser with explicit owner selection and retained
/// provenance. Data only: no current-source preparation or native permission.
pub fn decode_owned_for(bytes: &[u8], expected: Owner) -> Result<OwnedSaved, Refused> {
    decode::read_for(bytes, expected)
}
/// Raw-bounded schema1 quarantine. Duplicate raw ID members are intentionally
/// retained until EVERY distinct token has entered unbound suppression.
pub fn quarantine_legacy(bytes: &[u8]) -> Result<OwnedSaved, Refused> {
    decode::read(bytes, false)
}
fn legacy_valid(token: &str) -> bool {
    !token.is_empty() && token.len() <= 16384
}
fn namespace(bus_id: String, owner: String) -> Result<Option<Namespace>, Refused> {
    if bus_id.is_empty() && owner.is_empty() {
        return Ok(None);
    }
    let mut parts = owner.strip_prefix(':').unwrap_or("").split('.');
    let component = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    let unique = owner.len() <= 255 && parts.clone().count() >= 2 && parts.all(component);
    if bus_id.len() != 32 || !bus_id.bytes().all(|b| b.is_ascii_hexdigit()) || !unique {
        return Err(Refused::InvalidNamespace);
    }
    Ok(Some(Namespace { bus_id, owner }))
}

#[derive(Serialize)]
struct Wire<'a> {
    version: u32,
    bus_id: &'a str,
    owner: &'a str,
    tokens: Bindings<'a>,
    uncertain: Uncertain<'a>,
    legacy_uncertain: &'a BTreeSet<String>,
}
#[derive(Serialize)]
struct Entry<'a> {
    owner: &'static str,
    token: &'a str,
}
impl<'a> From<&'a ScopedToken> for Entry<'a> {
    fn from(token: &'a ScopedToken) -> Self {
        Self {
            owner: token.tag.text(),
            token: &token.token,
        }
    }
}
struct Bindings<'a>(&'a BTreeMap<u32, ScopedToken>);
impl Serialize for Bindings<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (id, token) in self.0 {
            map.serialize_entry(id, &Entry::from(token))?;
        }
        map.end()
    }
}
struct Uncertain<'a>(&'a BTreeSet<ScopedToken>);
impl Serialize for Uncertain<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for token in self.0 {
            seq.serialize_element(&Entry::from(token))?;
        }
        seq.end()
    }
}
struct Capped(Vec<u8>);
impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) as u64 > FILE_LIMIT {
            return Err(std::io::Error::other("Owned notification byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
