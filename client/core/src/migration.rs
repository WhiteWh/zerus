//! Bounded G3 bundle assembly over the existing source decoders. This module
//! opens/writes no source files and delivers no effects. `transport` connects
//! bundles to G1's selected CLI publisher; G1 owns private publication and drain.
//!
//! Consume each yielded Read stream before asking for another item. Metadata
//! pages are body-free; only private snapshots hold source details. A record
//! arrives once, after every source succeeds, and is always explicitly Staged.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Cursor, Write},
};
use zerus_desktop_migration::{
    sha256, Evidence, ImportRecord, PageRef, SnapshotPage, SnapshotRef, SourceRecord,
    MAX_IMPORT_RECORD_BYTES, MAX_SNAPSHOT_BYTES, SOURCE, VERSION,
};

pub mod families;
pub mod journal;
pub mod preload;
pub mod sources;
pub mod transport;

/// Redacted failures; source payloads and filesystem paths never enter errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    NotProduction,
    InvalidMetadata,
    UnsupportedSource,
    SourceUnreadable,
    SourceChanged,
    TooLarge,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotProduction => "only the live production switch may assemble import evidence",
            Self::InvalidMetadata => "invalid import metadata",
            Self::UnsupportedSource => "unsupported source schema; original source retained",
            Self::SourceUnreadable => "source could not be read; original source retained",
            Self::SourceChanged => "source identity or bytes changed during the read",
            Self::TooLarge => "import exceeds its admitted byte or entry limit",
        })
    }
}
impl std::error::Error for SourceError {}

pub type SourceResult<T> = Result<T, SourceError>;

/// One existing decoder's paged values. The decoder must check its source
/// schema and exact identity/hash, and yield an error on unknown/partial data.
/// An individual value must already be bounded; do not load a whole journal.
pub struct SourceStream {
    identity: SourceRecord,
    chunks: Box<dyn Iterator<Item = SourceResult<Value>> + Send>,
    rechecked: bool,
}

impl SourceStream {
    pub fn new(
        identity: SourceRecord,
        chunks: impl Iterator<Item = SourceResult<Value>> + Send + 'static,
    ) -> Self {
        Self {
            identity,
            chunks: Box::new(chunks),
            rechecked: false,
        }
    }

    pub fn identity(&self) -> &SourceRecord {
        &self.identity
    }

    /// Internal consumers may project already-bounded snapshot values. This
    /// exposes data only, never a source drain or import-admission permit.
    pub(crate) fn into_chunks(self) -> Box<dyn Iterator<Item = SourceResult<Value>> + Send> {
        self.chunks
    }

    /// Recheck the exact source after its decoder finishes. The caller uses
    /// the existing source reader under G1's current drain, checking path/file
    /// identity there too. An error or changed hash prevents a final manifest.
    /// The explicitly versioned `preload` creation archive is historical: its
    /// recheck validates the durable original snapshot, not today's mutable
    /// journal, and provides no current-source completeness/drain claim.
    pub fn recheck(self, verify: impl FnOnce() -> SourceResult<String> + Send + 'static) -> Self {
        struct Checked<F> {
            chunks: Box<dyn Iterator<Item = SourceResult<Value>> + Send>,
            expected: String,
            verify: Option<F>,
            ended: bool,
        }
        impl<F: FnOnce() -> SourceResult<String>> Iterator for Checked<F> {
            type Item = SourceResult<Value>;
            fn next(&mut self) -> Option<Self::Item> {
                if self.ended {
                    return None;
                }
                if let Some(item) = self.chunks.next() {
                    return Some(item);
                }
                self.ended = true;
                let check = self.verify.take().unwrap()().and_then(|actual| {
                    (actual == self.expected)
                        .then_some(())
                        .ok_or(SourceError::SourceChanged)
                });
                check.err().map(Err)
            }
        }
        let chunks = Checked {
            chunks: self.chunks,
            expected: self.identity.sha256.clone(),
            verify: Some(verify),
            ended: false,
        };
        Self {
            identity: self.identity,
            chunks: Box::new(chunks),
            rechecked: true,
        }
    }
}

/// A private current body, a body-free metadata page, or the final manifest.
/// No Debug implementation: formatting a stream must not disclose its bytes.
pub enum BundleItem {
    Snapshot {
        reference: SnapshotRef,
        reader: Cursor<Vec<u8>>,
    },
    Page {
        reference: PageRef,
        reader: Cursor<Vec<u8>>,
    },
    Record(ImportRecord),
}

struct Page {
    reference: PageRef,
    bytes: Vec<u8>,
}

pub struct Bundle {
    record: ImportRecord,
    sources: std::vec::IntoIter<SourceStream>,
    current: Option<(SourceStream, u64)>,
    pending_page: Option<Page>,
    snapshots: BTreeMap<String, u64>,
    admitted_bytes: u64,
    emitted_bytes: u64,
    ended: bool,
    publication_checks: bool,
}

impl Bundle {
    /// `admitted_bytes` is G1's explicit staging budget, including bodies,
    /// pages and final manifest. This is structural assembly, never consent
    /// or a claim that the current process owns/drained any source writer.
    pub fn new(
        identifier: &str,
        replay: bool,
        configuration: String,
        import_id: String,
        created_at: u64,
        sources: Vec<SourceStream>,
        admitted_bytes: u64,
    ) -> SourceResult<Self> {
        if identifier != SOURCE || replay {
            return Err(SourceError::NotProduction);
        }
        if sources.len() > 256 {
            return Err(SourceError::TooLarge);
        }
        for source in &sources {
            source
                .identity
                .validate()
                .map_err(|_| SourceError::InvalidMetadata)?;
        }
        let record = ImportRecord {
            version: VERSION,
            configuration,
            source: SOURCE.into(),
            import_id,
            evidence: Evidence::Staged,
            created_at,
            automatic_approvals: false,
            sources: sources.iter().map(|s| s.identity.clone()).collect(),
            pages: Vec::new(),
        };
        record
            .validate()
            .map_err(|_| SourceError::InvalidMetadata)?;
        if admitted_bytes == 0 {
            return Err(SourceError::TooLarge);
        }
        let publication_checks = sources.iter().all(|source| source.rechecked);
        Ok(Self {
            record,
            sources: sources.into_iter(),
            current: None,
            pending_page: None,
            snapshots: BTreeMap::new(),
            admitted_bytes,
            emitted_bytes: 0,
            ended: false,
            publication_checks,
        })
    }

    fn charge(&mut self, bytes: usize) -> SourceResult<()> {
        let next = self
            .emitted_bytes
            .checked_add(bytes as u64)
            .ok_or(SourceError::TooLarge)?;
        if next > self.admitted_bytes {
            return Err(SourceError::TooLarge);
        }
        self.emitted_bytes = next;
        Ok(())
    }

    fn chunk(
        &mut self,
        source: &SourceRecord,
        index: u64,
        value: Value,
    ) -> SourceResult<BundleItem> {
        if self.record.pages.len() >= 1024 {
            return Err(SourceError::TooLarge);
        }
        let snapshot_bytes = bounded_json(&value, MAX_SNAPSHOT_BYTES as usize)?;
        let snapshot = SnapshotRef {
            sha256: sha256(&snapshot_bytes),
            bytes: snapshot_bytes.len() as u64,
        };
        let metadata = SnapshotPage {
            version: VERSION,
            family: source.family.clone(),
            schema: source.schema,
            source_sha256: source.sha256.clone(),
            index,
            snapshot: snapshot.clone(),
        };
        metadata
            .validate()
            .map_err(|_| SourceError::InvalidMetadata)?;
        let page_bytes = bounded_json(&metadata, MAX_SNAPSHOT_BYTES as usize)?;
        let page_ref = PageRef {
            sha256: sha256(&page_bytes),
            bytes: page_bytes.len() as u64,
        };
        let duplicate = self.snapshots.contains_key(&snapshot.sha256);
        self.charge(page_bytes.len() + if duplicate { 0 } else { snapshot_bytes.len() })?;
        self.record.pages.push(page_ref.clone());
        if duplicate {
            Ok(BundleItem::Page {
                reference: page_ref,
                reader: Cursor::new(page_bytes),
            })
        } else {
            self.snapshots
                .insert(snapshot.sha256.clone(), snapshot.bytes);
            self.pending_page = Some(Page {
                reference: page_ref,
                bytes: page_bytes,
            });
            Ok(BundleItem::Snapshot {
                reference: snapshot,
                reader: Cursor::new(snapshot_bytes),
            })
        }
    }

    fn advance(&mut self) -> SourceResult<BundleItem> {
        if let Some(page) = self.pending_page.take() {
            return Ok(BundleItem::Page {
                reference: page.reference,
                reader: Cursor::new(page.bytes),
            });
        }
        loop {
            if let Some((mut source, index)) = self.current.take() {
                if let Some(chunk) = source.chunks.next() {
                    let item = self.chunk(&source.identity, index, chunk?)?;
                    self.current =
                        Some((source, index.checked_add(1).ok_or(SourceError::TooLarge)?));
                    return Ok(item);
                }
            }
            if let Some(source) = self.sources.next() {
                self.current = Some((source, 0));
            } else {
                self.record
                    .validate()
                    .map_err(|_| SourceError::InvalidMetadata)?;
                let bytes = bounded_json(&self.record, MAX_IMPORT_RECORD_BYTES)?;
                self.charge(bytes.len())?;
                self.ended = true;
                return Ok(BundleItem::Record(self.record.clone()));
            }
        }
    }
}

impl Iterator for Bundle {
    type Item = SourceResult<BundleItem>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.advance() {
            Ok(item) => Some(Ok(item)),
            Err(error) => {
                self.ended = true;
                Some(Err(error))
            }
        }
    }
}

/// A hard allocation bound while serializing one already-bounded source item.
fn bounded_json(value: &impl Serialize, limit: usize) -> SourceResult<Vec<u8>> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("bounded import item exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| SourceError::TooLarge)?;
    Ok(output.bytes)
}

#[cfg(test)]
mod tests;
