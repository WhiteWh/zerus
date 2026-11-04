//! Blocking, bounded desktop-import-v1 client over the existing process runner.
//! Call off the GUI thread. The caller durably retains the import UUID before
//! publishing and supplies existing readers with fresh identity/hash rechecks.
//! This module reads no source file, chooses no destination and grants no writer
//! authority. Failed publication is never automatically repeated, even if a
//! subsequent fixed-ID status lookup reports no manifest.
use super::{bounded_json, Bundle, BundleItem, SourceError};
use crate::{
    actions::{Exit, Invocation},
    run::{run_streamed, run_streamed_retained},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fmt,
    io::Write,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, mpsc, Arc},
    time::Duration,
};
use zerus_desktop_migration::{
    canonical_uuid, sha256, Evidence, ImportRecordRef, MAX_IMPORT_RECORD_BYTES, SOURCE, VERSION,
};

const ABI: &str = "desktop-import-v1";
const HELP_MARKER: &str = "Desktop import ABI: desktop-import-v1";
const OUTPUT: usize = 16 << 10;
const HEADER: usize = 4096;
const MAX_BYTES: u64 = 64 << 20;
const CHILD_TIMEOUT: Duration = Duration::from_secs(130);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Unsupported,
    StatusUnavailable,
    ConfigurationChanged,
    InvalidImportId,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "selected CLI does not confirm desktop-import-v1",
            Self::StatusUnavailable => "desktop import status could not be verified",
            Self::ConfigurationChanged => "selected desktop configuration changed",
            Self::InvalidImportId => "invalid import UUID",
        })
    }
}
impl std::error::Error for AdmissionError {}

/// A selected executable/configuration, never a service lease or drain proof.
pub struct Publisher {
    hgs: PathBuf,
    environment: Vec<(String, String)>,
    configuration: String,
    retained: Option<(Arc<dyn Send + Sync>, Arc<AtomicBool>)>,
}

impl Publisher {
    /// Probe only ordinary --help before any desktop command; old CLIs may
    /// otherwise interpret an unknown command as a request to launch a session.
    /// All probes and publication use this same executable and environment.
    pub fn probe(hgs: &Path, environment: &[(&str, &str)]) -> Result<Self, AdmissionError> {
        let mut publisher = Self {
            hgs: hgs.to_path_buf(),
            environment: environment
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            configuration: String::new(),
            retained: None,
        };
        publisher.configuration = publisher.current_configuration()?;
        Ok(publisher)
    }

    /// Native startup's original source/freeze/helper resources, not a writer
    /// permit. Every probe, publication and lookup retains them through actual
    /// child/pipe completion; uncertain publication still keeps the same UUID.
    pub fn probe_retained(
        hgs: &Path,
        environment: &[(&str, &str)],
        resources: Arc<dyn Send + Sync>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, AdmissionError> {
        let mut publisher = Self {
            hgs: hgs.into(),
            environment: environment
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            configuration: String::new(),
            retained: Some((resources, cancel)),
        };
        publisher.configuration = publisher.current_configuration()?;
        Ok(publisher)
    }

    fn exchange(
        &self,
        inv: &Invocation,
        cap: usize,
        input: impl FnOnce(std::process::ChildStdin) -> bool + Send + 'static,
    ) -> Result<Exit, crate::run::TooLarge> {
        if let Some((resources, cancel)) = &self.retained {
            run_streamed_retained(
                &self.hgs,
                inv,
                &self.environment(),
                cap,
                cancel,
                resources.clone(),
                input,
            )
        } else {
            run_streamed(&self.hgs, inv, &self.environment(), cap, input)
        }
    }

    pub fn configuration(&self) -> &str {
        &self.configuration
    }

    fn environment(&self) -> Vec<(&str, &str)> {
        self.environment
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    fn read(&self, args: &[&str], cap: usize) -> Option<Vec<u8>> {
        match self.exchange(&invocation(args, Duration::from_secs(5)), cap, |_| true) {
            Ok(Exit::Code {
                code: 0, stdout, ..
            }) => Some(stdout),
            _ => None,
        }
    }

    fn current_configuration(&self) -> Result<String, AdmissionError> {
        let help = self
            .read(&["--help"], 256 << 10)
            .ok_or(AdmissionError::Unsupported)?;
        let help = std::str::from_utf8(&help).map_err(|_| AdmissionError::Unsupported)?;
        if !help.lines().any(|line| line.trim() == HELP_MARKER) {
            return Err(AdmissionError::Unsupported);
        }
        #[derive(Deserialize)]
        struct Status {
            abi: String,
            configuration: String,
        }
        let bytes = self
            .read(&["desktop", "status", "--json"], OUTPUT)
            .ok_or(AdmissionError::StatusUnavailable)?;
        let status: Status =
            serde_json::from_slice(&bytes).map_err(|_| AdmissionError::StatusUnavailable)?;
        if status.abi != "desktop-v1" || !digest(&status.configuration) {
            return Err(AdmissionError::StatusUnavailable);
        }
        Ok(status.configuration)
    }

    fn revalidate(&self) -> Result<(), AdmissionError> {
        if self.current_configuration()? != self.configuration {
            return Err(AdmissionError::ConfigurationChanged);
        }
        Ok(())
    }

    /// Streams an untouched Bundle with a recheck on every source. Persist its
    /// UUID first. Success confirms only immutable Staged publication. Any
    /// unconfirmed result retains that UUID for explicit read-only inspection;
    /// this method never retries, overwrites or generates another identity.
    pub fn publish(&self, bundle: Bundle) -> Result<StagedReceipt, PublishFailure> {
        self.publish_with_receipt(bundle, |_| Ok(()))
    }

    /// Persist the exact expected receipt after all source rechecks, BEFORE
    /// writing the final record that can commit. Native recovery can therefore
    /// bind an uncertain outcome by original UUID and manifest hash, without
    /// recapturing sources, guessing counts or replaying publication.
    pub fn publish_with_receipt(
        &self,
        bundle: Bundle,
        before_record: impl FnOnce(&StagedReceipt) -> Result<(), SourceError> + Send + 'static,
    ) -> Result<StagedReceipt, PublishFailure> {
        let import_id = bundle.record.import_id.clone();
        let failure = |reason| PublishFailure {
            import_id: import_id.clone(),
            reason,
        };
        if bundle.record.configuration != self.configuration
            || bundle.ended
            || bundle.emitted_bytes != 0
            || !bundle.publication_checks
            || bundle.admitted_bytes > MAX_BYTES
        {
            return Err(failure(PublishError::InvalidBundle));
        }
        self.revalidate()
            .map_err(|e| failure(PublishError::Admission(e)))?;
        let (done, result) = mpsc::sync_channel(1);
        let exit = self.exchange(
            &invocation(&["desktop", "import-evidence", "--json"], CHILD_TIMEOUT),
            OUTPUT,
            move |mut pipe| {
                let result = encode(bundle, &mut pipe, before_record);
                let ok = result.is_ok();
                // Store the expected receipt before EOF can let the child
                // commit/exit. This bounded, initially empty channel cannot
                // block; failures also close the pipe without a final record.
                let _ = done.send(result);
                drop(pipe);
                ok
            },
        );
        let expected = match result.try_recv() {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(error)) => return Err(failure(error)),
            Err(_) => return Err(failure(PublishError::Unconfirmed)),
        };
        publication_reply(exit, &expected, self.retained.is_some()).map_err(failure)
    }

    /// Read one retained UUID. Absent includes incomplete publication and never
    /// authorizes reuse or another import. Present is historical evidence only.
    pub fn status(&self, import_id: &str) -> Result<ImportStatus, AdmissionError> {
        if !canonical_uuid(import_id) {
            return Err(AdmissionError::InvalidImportId);
        }
        self.revalidate()?;
        let bytes = self
            .read(
                &[
                    "desktop",
                    "import-status",
                    "--json",
                    "--import-id",
                    import_id,
                ],
                OUTPUT,
            )
            .ok_or(AdmissionError::StatusUnavailable)?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Status {
            abi: String,
            configuration: String,
            import_id: String,
            record_present: bool,
            evidence: Option<Evidence>,
            pages: Option<u64>,
            import_record: Option<ImportRecordRef>,
        }
        let status: Status =
            serde_json::from_slice(&bytes).map_err(|_| AdmissionError::StatusUnavailable)?;
        if status.abi != ABI
            || status.configuration != self.configuration
            || status.import_id != import_id
        {
            return Err(AdmissionError::StatusUnavailable);
        }
        match (
            status.record_present,
            status.evidence,
            status.pages,
            status.import_record,
        ) {
            (false, None, None, None) => Ok(ImportStatus::Absent),
            (true, Some(evidence), Some(pages), Some(record))
                if pages <= 1024 && record.version == VERSION && digest(&record.sha256) =>
            {
                Ok(ImportStatus::Present {
                    evidence,
                    pages,
                    import_record: record,
                })
            }
            _ => Err(AdmissionError::StatusUnavailable),
        }
    }
}

/// Body-free result validated against the exact bytes sent to the publisher.
/// It is never a marker, service lease, drain proof or automatic-approval grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedReceipt {
    pub abi: String,
    pub published: bool,
    pub evidence: Evidence,
    pub configuration: String,
    pub import_id: String,
    pub import_record: ImportRecordRef,
    pub bytes: u64,
    pub snapshots: u64,
    pub pages: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportStatus {
    Absent,
    Present {
        evidence: Evidence,
        pages: u64,
        import_record: ImportRecordRef,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishError {
    InvalidBundle,
    Admission(AdmissionError),
    Source(SourceError),
    /// Only this exact attempt is known not to have reserved/committed. Native
    /// explicit retry still needs the SAME UUID/record and fresh source checks.
    HandoffBusy,
    Unconfirmed,
}

fn publication_reply(
    exit: Result<Exit, crate::run::TooLarge>,
    expected: &StagedReceipt,
    original_complete: bool,
) -> Result<StagedReceipt, PublishError> {
    let stdout = match exit {
        Ok(Exit::Code {
            code: 0, stdout, ..
        }) => stdout,
        _ => return Err(PublishError::Unconfirmed),
    };
    if serde_json::from_slice::<StagedReceipt>(&stdout).is_ok_and(|actual| actual == *expected) {
        return Ok(expected.clone());
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NotCommitted {
        abi: String,
        published: bool,
        configuration: String,
        import_id: String,
        import_record: ImportRecordRef,
        code: String,
        this_attempt_not_committed: bool,
    }
    if original_complete
        && serde_json::from_slice::<NotCommitted>(&stdout).is_ok_and(|reply| {
            reply.abi == expected.abi
                && !reply.published
                && reply.configuration == expected.configuration
                && reply.import_id == expected.import_id
                && reply.import_record == expected.import_record
                && reply.code == "handoff_busy"
                && reply.this_attempt_not_committed
        })
    {
        return Err(PublishError::HandoffBusy);
    }
    Err(PublishError::Unconfirmed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishFailure {
    pub import_id: String,
    pub reason: PublishError,
}
impl fmt::Display for PublishFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("import publication not confirmed; retain this import UUID and original sources for review")
    }
}
impl std::error::Error for PublishFailure {}

fn invocation(args: &[&str], timeout: Duration) -> Invocation {
    Invocation {
        args: args.iter().map(|s| s.to_string()).collect(),
        stdin: None,
        timeout,
        request_id: None,
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn line(output: &mut impl Write, value: &impl Serialize) -> Result<(), PublishError> {
    let bytes = bounded_json(value, HEADER - 1).map_err(PublishError::Source)?;
    output
        .write_all(&bytes)
        .and_then(|_| output.write_all(b"\n"))
        .map_err(|_| PublishError::Unconfirmed)
}

fn body(
    output: &mut impl Write,
    header: &impl Serialize,
    bytes: &[u8],
) -> Result<(), PublishError> {
    line(output, header)?;
    output
        .write_all(bytes)
        .and_then(|_| output.write_all(b"\n"))
        .map_err(|_| PublishError::Unconfirmed)
}

fn encode(
    mut bundle: Bundle,
    output: &mut impl Write,
    before_record: impl FnOnce(&StagedReceipt) -> Result<(), SourceError>,
) -> Result<StagedReceipt, PublishError> {
    line(
        output,
        &json!({"abi":ABI,"configuration":bundle.record.configuration,
        "source":SOURCE,"replay":false,"import_id":bundle.record.import_id,
        "max_bytes":bundle.admitted_bytes}),
    )?;
    while let Some(item) = bundle.next() {
        match item.map_err(PublishError::Source)? {
            BundleItem::Snapshot { reference, reader } => body(
                output,
                &json!({"t":"snapshot","reference":reference}),
                reader.get_ref(),
            )?,
            BundleItem::Page { reference, reader } => body(
                output,
                &json!({"t":"page","reference":reference}),
                reader.get_ref(),
            )?,
            BundleItem::Record(record) => {
                let bytes =
                    bounded_json(&record, MAX_IMPORT_RECORD_BYTES).map_err(PublishError::Source)?;
                let receipt = StagedReceipt {
                    abi: ABI.into(),
                    published: true,
                    evidence: Evidence::Staged,
                    configuration: record.configuration,
                    import_id: record.import_id,
                    import_record: ImportRecordRef {
                        version: VERSION,
                        sha256: sha256(&bytes),
                    },
                    bytes: bundle.emitted_bytes,
                    snapshots: bundle.snapshots.len() as u64,
                    pages: record.pages.len() as u64,
                };
                before_record(&receipt).map_err(PublishError::Source)?;
                body(output, &json!({"t":"record","bytes":bytes.len()}), &bytes)?;
                output.flush().map_err(|_| PublishError::Unconfirmed)?;
                return Ok(receipt);
            }
        }
    }
    Err(PublishError::Unconfirmed)
}

#[cfg(test)]
mod tests;
