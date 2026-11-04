//! G3 import evidence and G1's handoff marker, shared by hgs and the client.
//!
//! These types validate structure and identity only. No deserialized field
//! proves writer quiescence or grants writer authority. G1 alone transitions
//! the marker under the OS-backed handoff lock and fresh instance/delegation
//! proof; G3 assembles staged evidence and G1 owns immutable publication after
//! old writers drain.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
};

mod snapshot;
pub use snapshot::{SnapshotPage, SnapshotRef, MAX_SNAPSHOT_BYTES};

pub const VERSION: u32 = 1;
pub const SOURCE: &str = "com.hgdev.hgs-tray";
pub const HANDOFF_FILE: &str = "handoff-v1.json";
pub const HANDOFF_LOCK: &str = "handoff.lock";
pub const IMPORT_RECORD_FILE: &str = "record-v1.json";
pub const MAX_HANDOFF_BYTES: usize = 64 << 10;
pub const MAX_IMPORT_RECORD_BYTES: usize = 256 << 10;

/// A structural refusal. It never includes source paths or payload contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid(pub &'static str);
impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Invalid {}

type Result<T> = std::result::Result<T, Invalid>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Imported,
    Quiesced,
    Complete,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRecordRef {
    pub version: u32,
    pub sha256: String,
}

/// Exact marker shape. Phase is historical state, never current runtime proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub version: u32,
    pub configuration: String,
    pub source: String,
    pub import_id: String,
    pub writer_epoch: String,
    pub phase: Phase,
    pub import_record: ImportRecordRef,
    /// Migration carries no approval consent. Later opt-in is separate policy.
    pub automatic_approvals: bool,
}

impl Handoff {
    pub fn parse(bytes: &[u8], expected_configuration: &str) -> Result<Self> {
        if bytes.len() > MAX_HANDOFF_BYTES {
            return Err(Invalid("handoff exceeds its byte limit"));
        }
        let marker: Self = serde_json::from_slice(bytes).map_err(|_| Invalid("invalid handoff"))?;
        marker.validate(expected_configuration)?;
        Ok(marker)
    }

    pub fn validate(&self, expected_configuration: &str) -> Result<()> {
        if self.version != VERSION || self.import_record.version != VERSION {
            return Err(Invalid("unsupported handoff or import record version"));
        }
        if !lower_hex(&self.configuration, 64) || self.configuration != expected_configuration {
            return Err(Invalid("handoff configuration mismatch"));
        }
        if self.source != SOURCE {
            return Err(Invalid("handoff source is not the production switch"));
        }
        if !canonical_uuid(&self.import_id) || !canonical_uuid(&self.writer_epoch) {
            return Err(Invalid("invalid import or writer epoch identity"));
        }
        if !lower_hex(&self.import_record.sha256, 64) {
            return Err(Invalid("invalid import record digest"));
        }
        if self.automatic_approvals {
            return Err(Invalid("migration cannot grant automatic approval consent"));
        }
        Ok(())
    }

    /// Resolve only a fixed, validated name beneath desktop/. No JSON path
    /// can select another file. This does not open or follow any filesystem path.
    pub fn record_relative_path(&self) -> Result<PathBuf> {
        record_relative_path(&self.import_id)
    }

    /// Verify exact bytes, including the manifest's matching identities.
    /// The caller must first open a bounded, private, no-follow single-link
    /// regular file under its handoff lock. This method provides no OS proof.
    pub fn verify_import_record(&self, bytes: &[u8]) -> Result<ImportRecord> {
        self.validate(&self.configuration)?;
        if bytes.len() > MAX_IMPORT_RECORD_BYTES {
            return Err(Invalid("import record exceeds its byte limit"));
        }
        if sha256(bytes) != self.import_record.sha256 {
            return Err(Invalid("import record digest mismatch"));
        }
        let record = ImportRecord::parse(bytes)?;
        if record.configuration != self.configuration || record.import_id != self.import_id {
            return Err(Invalid("import record identity mismatch"));
        }
        Ok(record)
    }
}

/// Staging never qualifies as drained evidence. Even Drained is a historical
/// importer statement; G1 still requires fresh runtime exclusion/delegation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidence {
    Staged,
    Drained,
}

/// Body-free manifest. Family details and snapshots live in bounded digest
/// addressed pages; no message, question, token or session array belongs here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRecord {
    pub version: u32,
    pub configuration: String,
    pub source: String,
    pub import_id: String,
    pub evidence: Evidence,
    pub created_at: u64,
    pub automatic_approvals: bool,
    pub sources: Vec<SourceRecord>,
    pub pages: Vec<PageRef>,
}

/// Metadata for one read-only source. Unknown source schemas are retained by
/// the importer in private quarantine; this manifest never embeds their bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub family: String,
    pub schema: u32,
    pub format: String,
    pub sha256: String,
    pub build: Option<String>,
}

impl SourceRecord {
    pub fn validate(&self) -> Result<()> {
        if !name(&self.family)
            || !name(&self.format)
            || self.schema == 0
            || !lower_hex(&self.sha256, 64)
            || self
                .build
                .as_ref()
                .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(Invalid("invalid import source metadata"));
        }
        Ok(())
    }
}

/// A page resolves to imports/<import_id>/pages/<sha256>.json only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRef {
    pub sha256: String,
    pub bytes: u64,
}

impl ImportRecord {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_IMPORT_RECORD_BYTES {
            return Err(Invalid("import record exceeds its byte limit"));
        }
        let record: Self =
            serde_json::from_slice(bytes).map_err(|_| Invalid("invalid import record"))?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Invalid("unsupported import record version"));
        }
        if !lower_hex(&self.configuration, 64)
            || self.source != SOURCE
            || !canonical_uuid(&self.import_id)
        {
            return Err(Invalid("invalid import record identity"));
        }
        if self.automatic_approvals {
            return Err(Invalid("migration cannot grant automatic approval consent"));
        }
        if self.sources.len() > 256 || self.pages.len() > 1024 {
            return Err(Invalid("import record exceeds its entry limits"));
        }
        let mut families = std::collections::BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !families.insert(&source.family) {
                return Err(Invalid("duplicate import source family"));
            }
        }
        let mut pages = std::collections::BTreeSet::new();
        for page in &self.pages {
            if !lower_hex(&page.sha256, 64)
                || !pages.insert(&page.sha256)
                || page.bytes == 0
                || page.bytes > (1 << 20)
            {
                return Err(Invalid("invalid import page reference"));
            }
        }
        Ok(())
    }

    pub fn page_relative_path(&self, page: &PageRef) -> Result<PathBuf> {
        self.validate()?;
        if !self.pages.contains(page) {
            return Err(Invalid("page is not in this import record"));
        }
        Ok(import_directory(&self.import_id)?
            .join("pages")
            .join(format!("{}.json", page.sha256)))
    }

    pub fn verify_page(&self, page: &PageRef, bytes: &[u8]) -> Result<()> {
        self.page_relative_path(page)?;
        if bytes.len() as u64 != page.bytes || sha256(bytes) != page.sha256 {
            return Err(Invalid("import page length or digest mismatch"));
        }
        Ok(())
    }

    pub fn snapshot_page(&self, reference: &PageRef, bytes: &[u8]) -> Result<SnapshotPage> {
        self.verify_page(reference, bytes)?;
        let page = SnapshotPage::parse(bytes)?;
        let source = self
            .sources
            .iter()
            .find(|s| s.family == page.family)
            .ok_or(Invalid("snapshot page names an unknown source family"))?;
        if source.schema != page.schema || source.sha256 != page.source_sha256 {
            return Err(Invalid("snapshot page source identity mismatch"));
        }
        Ok(page)
    }
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_./".contains(&b))
        && !value.contains("..")
        && !value.starts_with('/')
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn import_directory(import_id: &str) -> Result<PathBuf> {
    if !canonical_uuid(import_id) {
        return Err(Invalid("invalid import identity"));
    }
    Ok(Path::new("imports").join(import_id))
}

pub fn record_relative_path(import_id: &str) -> Result<PathBuf> {
    Ok(import_directory(import_id)?.join(IMPORT_RECORD_FILE))
}

/// Existing desktop Location identity, extracted without changing its byte
/// algorithm: canonical state then config, each length-prefixed native bytes.
pub fn configuration(state: &Path, dir: &Path) -> io::Result<String> {
    let state = canonical(state)?;
    let dir = canonical(dir)?;
    let mut hash = Sha256::new();
    for path in [&state, &dir] {
        let path = path.as_os_str().as_encoded_bytes();
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Resolve existing ancestors, even on a first read-only status. Aliases through
/// symlinks must not acquire separate service ownership.
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir => {
                resolved.pop();
            }
            component => {
                resolved.push(component);
            }
        }
        // Windows separates a path's prefix from RootDir. A rooted prefix
        // alone (notably \\?\C:) is not a complete path to open. Resolve it
        // only once RootDir has been appended; drive-relative inputs retain
        // their existing behavior.
        if path.has_root() && matches!(component, Component::Prefix(_)) {
            continue;
        }
        match std::fs::canonicalize(&resolved) {
            Ok(path) => resolved = path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests;
