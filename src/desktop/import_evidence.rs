//! Selected-local-child import transport. Staged evidence has no effect authority.
use super::{protocol, store::Location};
use crate::platform;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use zerus_desktop_migration::{
    canonical_uuid, sha256, Evidence, ImportRecord, PageRef, SnapshotPage, SnapshotRef,
    MAX_IMPORT_RECORD_BYTES, MAX_SNAPSHOT_BYTES, SOURCE,
};

const ABI: &str = "desktop-import-v1";
const HEADER: usize = 4096;
const MAX_BYTES: u64 = 64 << 20;
const MAX_ITEMS: usize = 2049;
const IDLE: Duration = Duration::from_secs(10);
pub(super) const TOTAL: Duration = Duration::from_secs(120);

pub(super) mod held;
mod stages;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    abi: String,
    configuration: String,
    source: String,
    replay: bool,
    import_id: String,
    max_bytes: u64,
}
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
enum Item {
    Snapshot { reference: SnapshotRef },
    Page { reference: PageRef },
    Record { bytes: u64 },
}

/// The one blocked stdin reader belongs only to this short-lived CLI child.
/// At most three 64 KiB chunks (current, queued, sender) exist. Returning on a
/// deadline exits that child, including its detached reader; no native job is
/// touched. Slow input never owns handoff.lock.
struct Input {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    current: io::Cursor<Vec<u8>>,
    deadline: Instant,
    idle: Duration,
    ended: bool,
}
impl Input {
    fn stdin() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("desktop-import-stdin".into())
            .spawn(move || {
                let mut input = io::stdin().lock();
                loop {
                    let mut bytes = vec![0; 64 << 10];
                    let result = input.read(&mut bytes).map(|count| {
                        bytes.truncate(count);
                        bytes
                    });
                    let ended = match &result {
                        Ok(bytes) => bytes.is_empty(),
                        Err(_) => true,
                    };
                    if sender.send(result).is_err() || ended {
                        break;
                    }
                }
            })?;
        Ok(Self {
            receiver,
            current: io::Cursor::new(Vec::new()),
            deadline: Instant::now() + TOTAL,
            idle: IDLE,
            ended: false,
        })
    }
}
impl Read for Input {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "import deadline exceeded",
            ));
        }
        if self.current.position() == self.current.get_ref().len() as u64 && !self.ended {
            let wait = self
                .idle
                .min(self.deadline.saturating_duration_since(Instant::now()));
            let bytes = self
                .receiver
                .recv_timeout(wait)
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "import input stalled"))??;
            self.ended = bytes.is_empty();
            self.current = io::Cursor::new(bytes);
        }
        self.current.read(output)
    }
}

fn header<T: serde::de::DeserializeOwned>(input: &mut impl BufRead) -> Result<T> {
    let bytes = protocol::line(input, HEADER)?.context("missing import header")?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid import header"))
}
fn body(input: &mut impl Read, size: u64, limit: u64, budget: &mut u64) -> Result<Vec<u8>> {
    ensure!(
        size > 0 && size <= limit && size <= *budget,
        "import byte limit exceeded"
    );
    *budget -= size;
    let mut bytes = vec![0; size as usize];
    input.read_exact(&mut bytes)?;
    let mut newline = [0];
    input.read_exact(&mut newline)?;
    ensure!(newline == *b"\n", "missing import body delimiter");
    Ok(bytes)
}
fn directory(path: &Path) -> Result<()> {
    platform::private_dir(path)?;
    ensure!(
        platform::verify_private(path, platform::Private::Directory)?,
        "unsafe import directory"
    );
    Ok(())
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    platform::private_open(options.write(true).create_new(true));
    #[cfg(unix)]
    platform::no_follow(&mut options);
    #[cfg(windows)]
    platform::refuse_link(&mut options);
    let mut file = options.open(path)?;
    ensure!(platform::verify_private_file(&file)?, "unsafe import file");
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn cli(location: &Location) -> Result<Value> {
    publish(location,&mut BufReader::new(Input::stdin()?))
        .map_err(|_|anyhow::anyhow!("import evidence not published; original sources retained; inspect the same import UUID before retry"))
}

/// No source-store reads, owner.lock, marker writes, worker or consent changes.
fn publish(location: &Location, input: &mut impl BufRead) -> Result<Value> {
    publish_with(location, input, |_| Ok(()))
}
struct Staged {
    start: Start,
    ownership: stages::Lease,
    stage: tempfile::TempDir,
    manifest: Vec<u8>,
    bytes: u64,
    snapshots: usize,
    pages: usize,
}

fn publish_with(
    location: &Location,
    input: &mut impl BufRead,
    after_reservation: impl FnOnce(&Path) -> Result<()>,
) -> Result<Value> {
    let staged = stage(location, input)?;
    match location.try_handoff_lock()? {
        Some(lock) => staged.commit(location, &lock, after_reservation),
        None => held::route(location, staged),
    }
}

fn stage(location: &Location, input: &mut impl BufRead) -> Result<Staged> {
    let start: Start = header(input)?;
    ensure!(
        start.abi == ABI
            && start.configuration == location.configuration
            && start.source == SOURCE
            && !start.replay
            && canonical_uuid(&start.import_id)
            && (1..=MAX_BYTES).contains(&start.max_bytes),
        "unsupported import identity"
    );
    location.prepare()?;
    let imports = location.root.join("imports");
    directory(&imports)?;
    let target = imports.join(&start.import_id);
    ensure!(
        fs::symlink_metadata(&target).is_err_and(|e| e.kind() == io::ErrorKind::NotFound),
        "import UUID already exists"
    );
    let stage = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(&imports)?;
    // This newly created temporary belongs to us; its verified private parent
    // protects the creation -> restriction interval on platforms using umask.
    platform::restrict_dir(stage.path())?;
    directory(stage.path())?;
    let ownership = stages::Lease::input(location, stage.path(), &start.import_id)?;
    for kind in ["snapshots", "pages"] {
        directory(&stage.path().join(kind))?;
    }
    let mut budget = start.max_bytes;
    let mut snapshots = BTreeMap::new();
    let mut pages = Vec::new();
    let mut metadata = Vec::new();
    let mut used = BTreeSet::new();
    let mut page_ids = BTreeSet::new();
    let mut final_record = None;
    for _ in 0..MAX_ITEMS {
        match header::<Item>(input)? {
            Item::Snapshot { reference } => {
                reference.validate()?;
                ensure!(
                    snapshots.len() < 1024 && !snapshots.contains_key(&reference.sha256),
                    "duplicate or excess snapshot"
                );
                let bytes = body(input, reference.bytes, MAX_SNAPSHOT_BYTES, &mut budget)?;
                reference.verify(&bytes)?;
                serde_json::from_slice::<serde::de::IgnoredAny>(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid snapshot JSON"))?;
                private_write(
                    &stage
                        .path()
                        .join("snapshots")
                        .join(format!("{}.json", reference.sha256)),
                    &bytes,
                )?;
                snapshots.insert(reference.sha256, reference.bytes);
            }
            Item::Page { reference } => {
                SnapshotRef {
                    sha256: reference.sha256.clone(),
                    bytes: reference.bytes,
                }
                .validate()?;
                ensure!(
                    pages.len() < 1024 && page_ids.insert(reference.sha256.clone()),
                    "duplicate or excess page"
                );
                let bytes = body(input, reference.bytes, MAX_SNAPSHOT_BYTES, &mut budget)?;
                ensure!(
                    sha256(&bytes) == reference.sha256,
                    "import page digest mismatch"
                );
                let page = SnapshotPage::parse(&bytes)?;
                ensure!(
                    snapshots.get(&page.snapshot.sha256) == Some(&page.snapshot.bytes),
                    "snapshot was not supplied"
                );
                used.insert(page.snapshot.sha256.clone());
                private_write(
                    &stage
                        .path()
                        .join("pages")
                        .join(format!("{}.json", reference.sha256)),
                    &bytes,
                )?;
                pages.push(reference);
                metadata.push(page);
            }
            Item::Record { bytes } => {
                let bytes = body(input, bytes, MAX_IMPORT_RECORD_BYTES as u64, &mut budget)?;
                let record = ImportRecord::parse(&bytes)?;
                ensure!(
                    record.configuration == start.configuration
                        && record.import_id == start.import_id
                        && record.evidence == Evidence::Staged
                        && record.pages == pages
                        && used.len() == snapshots.len(),
                    "import record does not match stream"
                );
                let sources: BTreeMap<_, _> = record
                    .sources
                    .iter()
                    .map(|s| (s.family.as_str(), s))
                    .collect();
                let mut indices = BTreeMap::new();
                for page in metadata {
                    let source = sources
                        .get(page.family.as_str())
                        .context("unknown import family")?;
                    ensure!(
                        source.schema == page.schema && source.sha256 == page.source_sha256,
                        "import source mismatch"
                    );
                    let index = indices.entry(page.family).or_insert(0);
                    ensure!(page.index == *index, "non-contiguous import page");
                    *index += 1;
                }
                let mut trailing = [0];
                ensure!(input.read(&mut trailing)? == 0, "trailing import data");
                private_write(&stage.path().join("record-v1.json"), &bytes)?;
                final_record = Some(bytes);
                break;
            }
        }
    }
    let bytes = final_record.context("import record missing")?;
    for kind in ["snapshots", "pages"] {
        platform::sync_dir(&stage.path().join(kind))?;
    }
    platform::sync_dir(stage.path())?;
    Ok(Staged {
        ownership,
        bytes: start.max_bytes - budget,
        snapshots: snapshots.len(),
        pages: pages.len(),
        start,
        stage,
        manifest: bytes,
    })
}

impl Staged {
    fn receipt(&self) -> Value {
        json!({"abi":ABI,"published":true,"evidence":"staged","configuration":self.start.configuration,
            "import_id":self.start.import_id,"import_record":{"version":1,"sha256":sha256(&self.manifest)},
            "bytes":self.bytes,"snapshots":self.snapshots,"pages":self.pages})
    }
    fn not_committed(&self) -> Value {
        // This invocation has validated all input but attempted no UUID mkdir.
        // This never settles an earlier unknown attempt or global absence.
        json!({"abi":ABI,"published":false,
            "configuration":self.start.configuration,"import_id":self.start.import_id,
            "import_record":{"version":1,"sha256":sha256(&self.manifest)},
            "code":"handoff_busy","this_attempt_not_committed":true})
    }
    fn commit(
        &self,
        location: &Location,
        lock: &super::store::HandoffLock,
        after_reservation: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Value> {
        lock.verify(location)?;
        // Best-effort bounded generated-stage cleanup is never publication or
        // replay authority. Live input/claim leases are skipped physically.
        let _ = stages::sweep(location, lock);
        let start = &self.start;
        let imports = location.root.join("imports");
        let target = imports.join(&start.import_id);
        let stage = &self.stage;
        location.check_existing()?;
        directory(&imports)?;
        ensure!(
            platform::verify_private(stage.path(), platform::Private::Directory)?,
            "unsafe import staging"
        );
        // Exclusive mkdir reserves the UUID forever, including a crash/partial
        // commit. The parent is private during mkdir -> restrict_dir. Only the
        // synced final manifest makes these staged bytes a committed evidence set.
        fs::create_dir(&target)?;
        platform::restrict_dir(&target)?;
        directory(&target)?;
        after_reservation(&target)?;
        for kind in ["snapshots", "pages"] {
            fs::rename(stage.path().join(kind), target.join(kind))?;
        }
        fs::rename(
            stage.path().join("record-v1.json"),
            target.join("record-v1.json"),
        )?;
        platform::sync_dir(&target)?;
        platform::sync_dir(&imports)?;
        Ok(self.receipt())
    }
}

/// Fixed-path manifest lookup only, never a page/body scan or authority grant.
pub fn status(location: &Location, id: &str) -> Result<Value> {
    ensure!(canonical_uuid(id), "invalid import UUID");
    location.check_existing()?;
    for directory in [
        location.root.join("imports"),
        location.root.join("imports").join(id),
    ] {
        match platform::verify_private(&directory, platform::Private::Directory) {
            Ok(true) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(
                    json!({"abi":ABI,"record_present":false,"configuration":location.configuration,"import_id":id}),
                )
            }
            _ => anyhow::bail!("unsafe import directory"),
        }
    }
    let path = location
        .root
        .join(zerus_desktop_migration::record_relative_path(id)?);
    let bytes = match super::handoff::read_private(&path, MAX_IMPORT_RECORD_BYTES) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(
                json!({"abi":ABI,"record_present":false,"configuration":location.configuration,"import_id":id}),
            )
        }
        Err(_) => anyhow::bail!("import record unavailable"),
    };
    let record = ImportRecord::parse(&bytes)?;
    ensure!(
        record.configuration == location.configuration && record.import_id == id,
        "import configuration mismatch"
    );
    Ok(
        json!({"abi":ABI,"record_present":true,"configuration":location.configuration,"import_id":id,
        "evidence":record.evidence,"pages":record.pages.len(),"import_record":{"version":1,"sha256":sha256(&bytes)}}),
    )
}

#[cfg(all(test, target_os = "linux"))]
pub(in crate::desktop) fn publish_fixture_for_test(location: &Location) -> Result<Value> {
    publish(location, &mut &tests::bundle("")[..])
}

#[cfg(test)]
mod tests;
