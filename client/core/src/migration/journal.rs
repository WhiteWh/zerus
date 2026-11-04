//! One native report's private identity/receipt record, not a source store,
//! delivery queue, handoff marker or worker permission. Unknown never resets it.
use super::{transport::StagedReceipt, SourceError, SourceResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zerus_desktop_migration::{canonical_uuid, sha256, Evidence};

pub const SCOPE: &str = "qt-retained-v1";
pub const MAX_ATTEMPTS: u32 = 16;
const BYTES: usize = 64 << 10;
const FILE: &str = "qt-retained-import-v1.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Reserved,
    Attempted,
    NotCommitted,
    Published,
    Unknown,
    Quarantined,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub scope: String,
    pub configuration: String,
    pub import_id: String,
    pub created_at: u64,
    pub qt_sha256: String,
    pub settings_sha256: String,
    pub attempt: u32,
    pub phase: Phase,
    pub expected: Option<StagedReceipt>,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Record {
    pub fn valid(&self) -> bool {
        self.version == 1
            && self.scope == SCOPE
            && digest(&self.configuration)
            && canonical_uuid(&self.import_id)
            && self.created_at > 0
            && digest(&self.qt_sha256)
            && digest(&self.settings_sha256)
            && self.attempt <= MAX_ATTEMPTS
            && (matches!(self.phase, Phase::Reserved | Phase::Quarantined) || self.attempt > 0)
            && (self.phase != Phase::Reserved || self.attempt == 0 && self.expected.is_none())
            && (!matches!(self.phase, Phase::NotCommitted | Phase::Published)
                || self.expected.is_some())
            && self.expected.as_ref().is_none_or(|r| {
                r.abi == "desktop-import-v1"
                    && r.published
                    && r.evidence == Evidence::Staged
                    && r.configuration == self.configuration
                    && r.import_id == self.import_id
                    && r.import_record.version == 1
                    && digest(&r.import_record.sha256)
                    && r.bytes <= 64 << 20
                    && r.snapshots <= 1024
                    && r.pages <= 1024
            })
    }
    fn follows(&self, old: &Self) -> bool {
        if self.version != old.version
            || self.scope != old.scope
            || self.configuration != old.configuration
            || self.import_id != old.import_id
            || self.created_at != old.created_at
            || self.qt_sha256 != old.qt_sha256
            || self.settings_sha256 != old.settings_sha256
            || old
                .expected
                .as_ref()
                .is_some_and(|r| self.expected.as_ref() != Some(r))
        {
            return false;
        }
        match (old.phase, self.phase) {
            (Phase::Reserved, Phase::Attempted) => self.attempt == 1,
            (Phase::NotCommitted, Phase::Attempted) => self.attempt == old.attempt + 1,
            (
                Phase::Attempted,
                Phase::Attempted
                | Phase::NotCommitted
                | Phase::Published
                | Phase::Unknown
                | Phase::Quarantined,
            )
            | (Phase::Unknown, Phase::Unknown | Phase::Published)
            | (Phase::Reserved, Phase::Quarantined)
            | (Phase::Reserved, Phase::Reserved)
            | (Phase::Published, Phase::Published)
            | (Phase::Quarantined, Phase::Quarantined)
            | (Phase::NotCommitted, Phase::Quarantined)
            | (Phase::NotCommitted, Phase::NotCommitted) => self.attempt == old.attempt,
            _ => false,
        }
    }
}
pub struct Stored {
    pub record: Record,
    pub sha256: String,
}
pub struct Journal {
    path: PathBuf,
    configuration: String,
}
impl Journal {
    /// Native startup supplies its original identity's managed data directory.
    /// No serialized source, response or webview chooses this filename.
    pub fn new(data: &Path, configuration: &str) -> SourceResult<Self> {
        if !data.is_absolute() || !digest(configuration) {
            return Err(SourceError::InvalidMetadata);
        }
        Ok(Self {
            path: data.join(FILE),
            configuration: configuration.into(),
        })
    }
    pub fn load(&self) -> SourceResult<Option<Stored>> {
        let Some(bytes) = crate::fsx::read_private_bounded(&self.path, BYTES)
            .map_err(|_| SourceError::SourceUnreadable)?
        else {
            return Ok(None);
        };
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| SourceError::UnsupportedSource)?;
        if !record.valid() || record.configuration != self.configuration {
            return Err(SourceError::InvalidMetadata);
        }
        Ok(Some(Stored {
            record,
            sha256: sha256(&bytes),
        }))
    }
    /// Compare the original private metadata record before replacement. A
    /// future, damaged, mismatched or changed record is kept, never reset.
    pub fn save(&self, record: Record, previous: Option<&str>) -> SourceResult<Stored> {
        if !record.valid() || record.configuration != self.configuration {
            return Err(SourceError::InvalidMetadata);
        }
        let current = self.load()?;
        if current.is_none() && record.phase != Phase::Reserved {
            return Err(SourceError::InvalidMetadata);
        }
        if current.as_ref().map(|s| s.sha256.as_str()) != previous
            || current.as_ref().is_some_and(|s| !record.follows(&s.record))
        {
            return Err(SourceError::SourceChanged);
        }
        let bytes = serde_json::to_vec_pretty(&record).map_err(|_| SourceError::InvalidMetadata)?;
        if bytes.len() > BYTES {
            return Err(SourceError::TooLarge);
        }
        crate::fsx::write_private_durable(&self.path, &bytes)
            .map_err(|_| SourceError::SourceUnreadable)?;
        let actual = self.load()?.ok_or(SourceError::SourceUnreadable)?;
        if actual.record != record || actual.sha256 != sha256(&bytes) {
            return Err(SourceError::SourceChanged);
        }
        Ok(actual)
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "zerus-import-journal-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            #[cfg(unix)]
            fs::create_dir(&root).unwrap();
            #[cfg(windows)]
            drop(crate::fsx::windows::Parents::open(&root, true).unwrap());
            Self(root.canonicalize().unwrap())
        }
        fn journal(&self) -> Journal {
            Journal::new(&self.0, &"a".repeat(64)).unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn record() -> Record {
        Record {
            version: 1,
            scope: SCOPE.into(),
            configuration: "a".repeat(64),
            import_id: "11111111-1111-4111-8111-111111111111".into(),
            created_at: 1,
            qt_sha256: "b".repeat(64),
            settings_sha256: "c".repeat(64),
            attempt: 0,
            phase: Phase::Reserved,
            expected: None,
        }
    }
    fn receipt(record: &Record) -> StagedReceipt {
        StagedReceipt {
            abi: "desktop-import-v1".into(),
            published: true,
            evidence: Evidence::Staged,
            configuration: record.configuration.clone(),
            import_id: record.import_id.clone(),
            import_record: zerus_desktop_migration::ImportRecordRef {
                version: 1,
                sha256: "d".repeat(64),
            },
            bytes: 1,
            pages: 1,
            snapshots: 1,
        }
    }
    fn update(journal: &Journal, old: Stored, f: impl FnOnce(&mut Record)) -> Stored {
        let mut record = old.record;
        f(&mut record);
        journal.save(record, Some(&old.sha256)).unwrap()
    }
    #[test]
    fn exact_negative_allows_only_a_new_attempt_of_the_same_original_record() {
        let scratch = Scratch::new();
        let journal = scratch.journal();
        let saved = journal.save(record(), None).unwrap();
        let saved = update(&journal, saved, |r| {
            r.attempt = 1;
            r.phase = Phase::Attempted;
        });
        let saved = update(&journal, saved, |r| {
            r.expected = Some(receipt(r));
        });
        let saved = update(&journal, saved, |r| {
            r.phase = Phase::NotCommitted;
        });
        let original = saved.record.clone();
        let saved = update(&journal, saved, |r| {
            r.attempt = 2;
            r.phase = Phase::Attempted;
        });
        assert_eq!(saved.record.import_id, original.import_id);
        assert_eq!(saved.record.expected, original.expected);
        let saved = update(&journal, saved, |r| {
            r.phase = Phase::Published;
        });
        let mut repeated = saved.record.clone();
        repeated.attempt += 1;
        repeated.phase = Phase::Attempted;
        assert!(journal.save(repeated, Some(&saved.sha256)).is_err());
    }
    #[test]
    fn unknown_never_becomes_absent_negative_or_a_new_id() {
        let scratch = Scratch::new();
        let journal = scratch.journal();
        let saved = journal.save(record(), None).unwrap();
        let saved = update(&journal, saved, |r| {
            r.attempt = 1;
            r.phase = Phase::Attempted;
            r.expected = Some(receipt(r));
        });
        let saved = update(&journal, saved, |r| {
            r.phase = Phase::Unknown;
        });
        for phase in [Phase::Reserved, Phase::NotCommitted, Phase::Attempted] {
            let mut changed = saved.record.clone();
            changed.phase = phase;
            assert!(journal.save(changed, Some(&saved.sha256)).is_err());
        }
        let mut changed = saved.record.clone();
        changed.import_id = "22222222-2222-4222-8222-222222222222".into();
        changed.expected = None;
        assert!(journal.save(changed, Some(&saved.sha256)).is_err());
        assert_eq!(journal.load().unwrap().unwrap().record, saved.record);
    }
    #[test]
    fn mismatch_or_replacement_keeps_the_original_receipt_and_bytes() {
        let scratch = Scratch::new();
        let journal = scratch.journal();
        let saved = journal.save(record(), None).unwrap();
        let before = fs::read(&journal.path).unwrap();
        let mut changed = saved.record.clone();
        changed.qt_sha256 = "e".repeat(64);
        assert!(journal.save(changed, Some(&saved.sha256)).is_err());
        let mut next = saved.record.clone();
        next.attempt = 1;
        next.phase = Phase::Attempted;
        assert!(journal.save(next, Some(&"f".repeat(64))).is_err());
        assert_eq!(fs::read(&journal.path).unwrap(), before);
    }
    #[test]
    fn future_malformed_or_foreign_metadata_is_never_reset() {
        let scratch = Scratch::new();
        let journal = scratch.journal();
        for bytes in [
            b"future".to_vec(),
            serde_json::to_vec(&serde_json::json!({"version":2})).unwrap(),
        ] {
            crate::fsx::write_private_durable(&journal.path, &bytes).unwrap();
            assert!(journal.load().is_err());
            assert!(journal.save(record(), None).is_err());
            assert_eq!(fs::read(&journal.path).unwrap(), bytes);
        }
        fs::remove_file(&journal.path).unwrap();
        journal.save(record(), None).unwrap();
        assert!(Journal::new(&scratch.0, &"f".repeat(64))
            .unwrap()
            .load()
            .is_err());
    }
}
