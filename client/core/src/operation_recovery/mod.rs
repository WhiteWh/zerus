//! Historical operation evidence, never an execution or recovery authority.
//!
//! G1 supplies already admitted private reads. This projection owns no files,
//! store, receipts, worker, startup hook or effect calls. One immutable manifest
//! bounds each traversal; private bodies are discarded after each observation.
use serde::{Deserialize, Serialize};
use zerus_desktop_migration::{
    sha256, Evidence, ImportRecord, ImportRecordRef, PageRef, SnapshotRef, SourceRecord, VERSION,
};

mod observation;
pub use observation::{Kind, Operation, RecordedState, Scope};
pub mod creation_clone;
pub mod creation_worktree;
pub mod task_message;
pub mod task_start;

pub const MAX_PAGES: usize = 64;
pub const MAX_BYTES: u64 = 8 << 20;
const MIN_BYTES: u64 = 2 << 20;

/// Errors contain no source path, body, command output or native diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidEvidence,
    InvalidCursor,
    InvalidLimits,
    Unavailable,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidEvidence => "Operation recovery evidence could not be verified.",
            Self::InvalidCursor => "Operation recovery evidence changed. Start a new page.",
            Self::InvalidLimits => "Operation recovery read limits are invalid.",
            Self::Unavailable => "Operation recovery evidence is unavailable.",
        })
    }
}
impl std::error::Error for Error {}

/// Implemented by the existing G1 reader, not a new filesystem/security stack.
/// Reads must enforce the reference byte bound before allocation and use G1's
/// fixed digest paths, private no-follow regular-file and identity checks.
pub trait EvidenceReader {
    fn page(&mut self, reference: &PageRef) -> Result<Vec<u8>, Error>;
    fn snapshot(&mut self, reference: &SnapshotRef) -> Result<Vec<u8>, Error>;
}

#[derive(Clone, Copy)]
pub struct Limits {
    pages: usize,
    bytes: u64,
}
impl Limits {
    pub fn new(pages: usize, bytes: u64) -> Result<Self, Error> {
        if pages == 0 || pages > MAX_PAGES || !(MIN_BYTES..=MAX_BYTES).contains(&bytes) {
            return Err(Error::InvalidLimits);
        }
        Ok(Self { pages, bytes })
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            pages: 16,
            bytes: 4 << 20,
        }
    }
}

/// Exact immutable generation; Staged and Drained are historical labels only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub configuration: String,
    pub import_id: String,
    pub record_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    generation: Generation,
    next_page: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Recognized metadata, not a claim that its bodies have been verified.
    Decodable,
    Unsupported,
    OtherResponsibility,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceCoverage {
    pub source: SourceRecord,
    pub support: Support,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Creations,
    TaskMessages,
    TaskStarts,
    PendingLaunches,
    PendingExecutorLaunches,
    DeferredInput,
    DraftTranscription,
    ProviderOperations,
    DownloadCache,
    TerminationCleanup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Coverage {
    pub sources: Vec<SourceCoverage>,
    /// No decodable source metadata for these domains. Never infer no pending
    /// operations, successful migration, discovery or drain from this list.
    pub unavailable: Vec<Domain>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// End of this manifest only, never complete source discovery or cutover.
    ManifestEnd,
    PageLimit,
    ByteLimit,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Page {
    pub generation: Generation,
    pub evidence: Evidence,
    pub operations: Vec<Operation>,
    pub unsupported_pages: Vec<usize>,
    pub next: Option<Cursor>,
    pub stop: Stop,
    pub scanned_pages: usize,
    pub read_bytes: u64,
}

pub struct Projection {
    generation: Generation,
    record: ImportRecord,
}

impl Projection {
    /// `reference` and expected identities come from G1's selected immutable
    /// import, not a body-selected manifest. No handoff phase grants authority.
    pub fn new(
        reference: &ImportRecordRef,
        bytes: &[u8],
        configuration: &str,
        import_id: &str,
    ) -> Result<Self, Error> {
        let record = ImportRecord::parse(bytes).map_err(|_| Error::InvalidEvidence)?;
        if reference.version != VERSION
            || sha256(bytes) != reference.sha256
            || record.configuration != configuration
            || record.import_id != import_id
        {
            return Err(Error::InvalidEvidence);
        }
        Ok(Self {
            generation: Generation {
                configuration: record.configuration.clone(),
                import_id: record.import_id.clone(),
                record_sha256: reference.sha256.clone(),
            },
            record,
        })
    }

    /// Metadata-only coverage. An absent or unsupported family is unavailable,
    /// never an empty operation set. Even Decodable is not verified content.
    pub fn coverage(&self) -> Coverage {
        let sources = self
            .record
            .sources
            .iter()
            .map(|source| SourceCoverage {
                source: source.clone(),
                support: observation::support(source),
            })
            .collect();
        let mut unavailable = Vec::new();
        for (kind, domain) in [
            (Kind::Creation, Domain::Creations),
            (Kind::TaskMessage, Domain::TaskMessages),
            (Kind::TaskStart, Domain::TaskStarts),
        ] {
            if !self
                .record
                .sources
                .iter()
                .any(|source| observation::kind(source) == Some(kind))
            {
                unavailable.push(domain);
            }
        }
        unavailable.extend([
            Domain::PendingLaunches,
            Domain::PendingExecutorLaunches,
            Domain::DeferredInput,
            Domain::DraftTranscription,
            Domain::ProviderOperations,
            Domain::DownloadCache,
            Domain::TerminationCleanup,
        ]);
        Coverage {
            sources,
            unavailable,
        }
    }

    /// At most 64 metadata pages and 8 MiB of reads per call, no catalog scan
    /// or retained per-operation cache. A cancellation leaves the next exact
    /// page available; no private body is returned or retained by the cursor.
    pub fn page(
        &self,
        cursor: Option<&Cursor>,
        limits: Limits,
        reader: &mut impl EvidenceReader,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Page, Error> {
        let mut at = match cursor {
            Some(cursor)
                if cursor.generation == self.generation
                    && cursor.next_page < self.record.pages.len() =>
            {
                cursor.next_page
            }
            Some(_) => return Err(Error::InvalidCursor),
            None => 0,
        };
        let mut out = Page {
            generation: self.generation.clone(),
            evidence: self.record.evidence,
            operations: Vec::new(),
            unsupported_pages: Vec::new(),
            next: None,
            stop: Stop::ManifestEnd,
            scanned_pages: 0,
            read_bytes: 0,
        };
        while let Some(reference) = self.record.pages.get(at) {
            if cancelled() {
                out.stop = Stop::Cancelled;
                break;
            }
            if out.scanned_pages >= limits.pages {
                out.stop = Stop::PageLimit;
                break;
            }
            if out.read_bytes + reference.bytes > limits.bytes {
                out.stop = Stop::ByteLimit;
                break;
            }
            let bytes = reader.page(reference)?;
            let page = self
                .record
                .snapshot_page(reference, &bytes)
                .map_err(|_| Error::InvalidEvidence)?;
            out.read_bytes += reference.bytes;
            let source = self
                .record
                .sources
                .iter()
                .find(|source| source.family == page.family)
                .ok_or(Error::InvalidEvidence)?;
            match observation::support(source) {
                Support::Unsupported => out.unsupported_pages.push(at),
                Support::OtherResponsibility => (),
                Support::Decodable => {
                    if cancelled() {
                        out.stop = Stop::Cancelled;
                        break;
                    }
                    if out.read_bytes + page.snapshot.bytes > limits.bytes {
                        out.stop = Stop::ByteLimit;
                        break;
                    }
                    let body = reader.snapshot(&page.snapshot)?;
                    page.snapshot
                        .verify(&body)
                        .map_err(|_| Error::InvalidEvidence)?;
                    out.read_bytes += page.snapshot.bytes;
                    if cancelled() {
                        out.stop = Stop::Cancelled;
                        break;
                    }
                    if let Some(operation) = observation::decode(source, &page, at, &body)? {
                        out.operations.push(operation);
                    }
                }
            }
            out.scanned_pages += 1;
            at += 1;
        }
        if at < self.record.pages.len() {
            out.next = Some(Cursor {
                generation: self.generation.clone(),
                next_page: at,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
