//! Immutable Staged finalization by the service's actual lock owner. Filesystem
//! work stays on the existing bounded client thread, never the actor.
use super::*;
// Production uses the original std::fs::rename. Tests model an uncertain
// result after a real rename, without a production fault switch or route.
use crate::{
    desktop::{notifications::preparation::Owner, transport::Stream},
    platform::FileIdentity,
};
#[cfg(not(test))]
use std::fs::rename as claim_move;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[cfg(test)]
use tests::claim_move;

pub(in crate::desktop) const OP: &str = "import_publish";
static ACTIVE: AtomicBool = AtomicBool::new(false);
struct Admission;
impl Admission {
    fn take() -> Result<Self> {
        ensure!(
            ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "import finalizer is busy"
        );
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

pub(in crate::desktop) struct Loan {
    pub owner: Option<Arc<Owner>>,
    pub epoch: String,
    pub seq: u64,
}
struct Session {
    stream: Stream,
    loan: Option<Loan>,
    pending: Option<mpsc::Receiver<Loan>>,
    _admission: Option<Admission>,
}
impl Session {
    fn respond(&mut self, value: Value) -> Result<()> {
        let (epoch, seq) = self
            .loan
            .as_ref()
            .map(|loan| (loan.epoch.as_str(), loan.seq))
            .unwrap_or(("", 0));
        let response = super::super::frame(epoch, seq, "response", value);
        self.stream.write_all(&protocol::encode(&response)?)?;
        self.stream.flush()?;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Also runs on errors/panic. Neither original charge is released before
        // the response connection has actually been shut down.
        super::super::transport::shutdown(&self.stream);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    version: u32,
    stage: String,
    import_id: String,
    record_sha256: String,
    max_bytes: u64,
    directory: [u64; 2],
}
impl Reference {
    fn parse(value: Value) -> Result<Self> {
        let value: Self = serde_json::from_value(value)?;
        ensure!(
            value.version == 1
                && canonical_uuid(&value.import_id)
                && value.stage.starts_with(".stage-")
                && (8..=70).contains(&value.stage.len())
                && value.stage[7..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                && value.record_sha256.len() == 64
                && value
                    .record_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && (1..=MAX_BYTES).contains(&value.max_bytes),
            "invalid managed import reference"
        );
        Ok(value)
    }
    fn not_committed(&self, location: &Location) -> Value {
        json!({"abi":ABI,"published":false,"configuration":location.configuration,"import_id":self.import_id,
            "import_record":{"version":1,"sha256":self.record_sha256},
            "code":"handoff_busy","this_attempt_not_committed":true})
    }
}

pub(super) fn directory_handle(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    platform::no_follow(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    let file = options.open(path)?;
    ensure!(
        platform::verify_private_directory(&file)?,
        "unsafe managed import directory"
    );
    Ok(file)
}
pub(super) fn key(file: &fs::File) -> Result<[u64; 2]> {
    let id = FileIdentity::of(file)?;
    Ok([id.device, id.inode])
}
fn cleanup_original(path: &Path, pin: &fs::File) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()), // server owns the renamed directory
        Err(error) => Err(error.into()),
        Ok(_) => {
            ensure!(
                key(&directory_handle(path)?)? == key(pin)?,
                "managed import directory replaced"
            );
            super::stages::discard_input(path, key(pin)?)?;
            Ok(())
        }
    }
}

/// Called only after this CLI fully validates and syncs the original stream and
/// the actual local try-lock is contended. Unknown RPC outcomes never retry.
pub(super) fn route(location: &Location, mut staged: Staged) -> Result<Value> {
    let pin = directory_handle(staged.stage.path())?;
    let reference = json!({"version":1,"stage":staged.stage.path().file_name().and_then(|s|s.to_str()).context("invalid staging leaf")?,
        "import_id":staged.start.import_id,"record_sha256":sha256(&staged.manifest),
        "max_bytes":staged.start.max_bytes,"directory":key(&pin)?});
    let negative = staged.not_committed();
    let expected = staged.receipt();
    // Transfer custody BEFORE the RPC can become uncertain. This process's
    // TempDir Drop or death cannot remove an admitted finalizer's input.
    staged.ownership.retain();
    let path = staged.stage.keep();
    let result = super::super::exchange(location, OP, Some(reference));
    let value = match result {
        Ok(Some(value)) if value == expected || value == negative => value,
        Ok(None) => negative, // no connection/request was admitted
        _ => anyhow::bail!("owned import finalization is unconfirmed"), // keep original managed stage
    };
    if cleanup_original(&path, &pin).is_ok() {
        staged.ownership.discard();
    }
    Ok(value)
}

pub(in crate::desktop) fn client(
    stream: Stream,
    hello: protocol::Hello,
    send: mpsc::SyncSender<super::super::Event>,
    stopped: Arc<AtomicBool>,
    location: &Location,
) -> Result<()> {
    client_with(stream, hello, send, stopped, location, |_| Ok(()))
}
fn client_with(
    stream: Stream,
    hello: protocol::Hello,
    send: mpsc::SyncSender<super::super::Event>,
    stopped: Arc<AtomicBool>,
    location: &Location,
    after_claim: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    ensure!(
        hello.abi == protocol::ABI && hello.configuration == location.configuration,
        "invalid import finalization scope"
    );
    let reference = Reference::parse(hello.request.context("missing managed import reference")?)?;
    let mut session = Session {
        stream,
        loan: None,
        pending: None,
        _admission: None,
    };
    let negative = reference.not_committed(location);
    if stopped.load(Ordering::Acquire) {
        return session.respond(negative);
    }
    let Ok(admission) = Admission::take() else {
        return session.respond(negative);
    };
    session._admission = Some(admission);
    let (reply, receive) = mpsc::sync_channel(1);
    session.pending = Some(receive);
    if send
        .try_send(super::super::Event::ImportOwner(
            location.configuration.clone(),
            reply,
        ))
        .is_err()
    {
        return session.respond(negative);
    }
    let Ok(loan) = session
        .pending
        .as_ref()
        .expect("original admission receiver")
        .recv_timeout(super::super::transport::TIMEOUT)
    else {
        return session.respond(negative);
    };
    session.pending = None;
    session.loan = Some(loan);
    let loan = session
        .loan
        .as_ref()
        .context("missing original import owner response")?;
    let value = if let Some(owner) = &loan.owner {
        // The actor supplied this same original Owner, including its serve
        // lock and preparation charge, rather than a reconstructed role.
        match finalize(
            location,
            &reference,
            &owner.handoff(),
            &stopped,
            after_claim,
        ) {
            Ok(value) => value,
            Err(error) if !error.rename_entered => negative,
            Err(error) => return Err(error.error),
        }
    } else {
        reference.not_committed(location)
    };
    session.respond(value)
}

#[derive(Debug)]
struct FinalizeError {
    rename_entered: bool,
    error: anyhow::Error,
}

struct Claimed {
    ownership: super::stages::Lease,
    directory: Option<tempfile::TempDir>,
    keep: bool,
}
impl Drop for Claimed {
    fn drop(&mut self) {
        if self.keep {
            if let Some(directory) = self.directory.take() {
                let _ = directory.keep();
            }
        }
    }
}
fn finalize(
    location: &Location,
    reference: &Reference,
    lock: &super::super::store::HandoffLock,
    stopped: &AtomicBool,
    after_claim: impl FnOnce(&Path) -> Result<()>,
) -> std::result::Result<Value, FinalizeError> {
    let mut rename_entered = false;
    let result = (|| -> Result<Value> {
        let until = Instant::now() + TOTAL;
        lock.verify(location)?;
        location.check_existing()?;
        let imports = location.root.join("imports");
        ensure!(
            platform::verify_private(&imports, platform::Private::Directory)?,
            "unsafe import root"
        );
        let original = imports.join(&reference.stage);
        let pin = directory_handle(&original)?;
        ensure!(
            key(&pin)? == reference.directory,
            "managed import directory replaced"
        );
        ensure!(!stopped.load(Ordering::Acquire), "service stopping");
        let directory = tempfile::Builder::new()
            .prefix(".held-import-")
            .tempdir_in(&imports)?;
        platform::restrict_dir(directory.path())?;
        let mut ownership = super::stages::Lease::claim(
            location,
            directory.path(),
            &reference.import_id,
            &reference.stage,
            reference.directory,
        )?;
        ownership.retain();
        let owned = directory.path().join("input");
        let mut claimed = Claimed {
            ownership,
            directory: Some(directory),
            keep: true,
        };
        // Set before the syscall: even a failed/uncertain rename must not grant a
        // pre-claim negative, let alone allow reuse after UUID reservation.
        rename_entered = true;
        claim_move(&original, &owned)?;
        ensure!(
            key(&directory_handle(&owned)?)? == reference.directory,
            "managed import directory replaced during transfer"
        );
        platform::sync_dir(&imports)?;
        after_claim(&owned)?;
        let input = Replay::new(&owned, reference, location, stopped, until)?;
        let prepared = stage(location, &mut BufReader::new(input))?;
        ensure!(
            sha256(&prepared.manifest) == reference.record_sha256
                && !stopped.load(Ordering::Acquire)
                && Instant::now() < until,
            "import finalization changed or stopped"
        );
        let value = prepared.commit(location, lock, |_| Ok(()))?;
        claimed.keep = false; // only exact validated, bounded managed files are removed
        claimed.ownership.discard();
        Ok(value)
    })();
    result.map_err(|error| FinalizeError {
        rename_entered,
        error,
    })
}

struct Body {
    directory: Arc<PinnedDirectory>,
    leaf: String,
    reference: SnapshotRef,
    identity: FileIdentity,
}
struct PinnedDirectory {
    path: std::path::PathBuf,
    file: fs::File,
    parent: Option<Arc<Self>>,
}
impl PinnedDirectory {
    fn open(path: &Path, parent: Option<Arc<Self>>) -> Result<Arc<Self>> {
        if let Some(parent) = &parent {
            parent.check()?;
        }
        let file = directory_handle(path)?;
        Ok(Arc::new(Self {
            path: path.into(),
            file,
            parent,
        }))
    }
    fn check(&self) -> Result<()> {
        if let Some(parent) = &self.parent {
            parent.check()?;
        }
        ensure!(
            key(&directory_handle(&self.path)?)? == key(&self.file)?,
            "import directory replaced"
        );
        Ok(())
    }
}
pub(super) fn read_body(path: &Path, limit: usize) -> Result<(Vec<u8>, FileIdentity)> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    platform::no_follow(&mut options);
    #[cfg(windows)]
    platform::refuse_link(&mut options);
    let mut file = options.open(path)?;
    ensure!(platform::verify_private_file(&file)?, "unsafe import body");
    let (identity, metadata) = FileIdentity::with_metadata(&file)?;
    ensure!(metadata.len() <= limit as u64, "import body exceeds limit");
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit
            && bytes.len() as u64 == metadata.len()
            && FileIdentity::of(&file)? == identity
            && platform::verify_private_file(&file)?,
        "import body changed"
    );
    Ok((bytes, identity))
}
fn inventory(directory: &PinnedDirectory, expected: &BTreeSet<String>) -> Result<()> {
    directory.check()?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&directory.path)?.take(expected.len() + 1) {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid import entry"))?;
        ensure!(
            expected.contains(&name) && names.insert(name),
            "unexpected import entry"
        );
    }
    ensure!(names == *expected, "import entries changed");
    directory.check()
}
struct Replay<'a> {
    bodies: VecDeque<(Value, Body)>,
    record: Vec<u8>,
    current: io::Cursor<Vec<u8>>,
    stopped: &'a AtomicBool,
    until: Instant,
    ended: bool,
    root: Arc<PinnedDirectory>,
    snapshots: (Arc<PinnedDirectory>, BTreeSet<String>),
    pages: (Arc<PinnedDirectory>, BTreeSet<String>),
}
impl<'a> Replay<'a> {
    fn new(
        root: &Path,
        reference: &Reference,
        location: &Location,
        stopped: &'a AtomicBool,
        until: Instant,
    ) -> Result<Self> {
        let root = PinnedDirectory::open(root, None)?;
        ensure!(
            key(&root.file)? == reference.directory,
            "claimed import directory replaced"
        );
        let snapshot_directory =
            PinnedDirectory::open(&root.path.join("snapshots"), Some(root.clone()))?;
        let page_directory = PinnedDirectory::open(&root.path.join("pages"), Some(root.clone()))?;
        inventory(
            &root,
            &["snapshots".into(), "pages".into(), "record-v1.json".into()]
                .into_iter()
                .collect(),
        )?;
        let (record, _) = read_body(&root.path.join("record-v1.json"), MAX_IMPORT_RECORD_BYTES)?;
        ensure!(
            sha256(&record) == reference.record_sha256,
            "import record hash mismatch"
        );
        let parsed = ImportRecord::parse(&record)?;
        ensure!(
            parsed.import_id == reference.import_id
                && parsed.configuration == location.configuration
                && parsed.evidence == Evidence::Staged,
            "import record scope mismatch"
        );
        let mut budget = reference
            .max_bytes
            .checked_sub(record.len() as u64)
            .context("import byte limit")?;
        let mut bodies = VecDeque::new();
        let mut snapshots = BTreeSet::new();
        let mut pages = BTreeSet::new();
        for page in &parsed.pages {
            ensure!(
                !stopped.load(Ordering::Acquire) && Instant::now() < until,
                "service stopping or import deadline exceeded"
            );
            budget = budget
                .checked_sub(page.bytes)
                .context("import byte limit")?;
            page_directory.check()?;
            let leaf = format!("{}.json", page.sha256);
            let path = page_directory.path.join(&leaf);
            let (bytes, identity) = read_body(&path, MAX_SNAPSHOT_BYTES as usize)?;
            ensure!(
                bytes.len() as u64 == page.bytes && sha256(&bytes) == page.sha256,
                "import page changed"
            );
            let metadata = SnapshotPage::parse(&bytes)?;
            let snapshot = metadata.snapshot;
            let name = format!("{}.json", snapshot.sha256);
            if snapshots.insert(name.clone()) {
                budget = budget
                    .checked_sub(snapshot.bytes)
                    .context("import byte limit")?;
                snapshot_directory.check()?;
                let path = snapshot_directory.path.join(&name);
                let (bytes, identity) = read_body(&path, MAX_SNAPSHOT_BYTES as usize)?;
                snapshot.verify(&bytes)?;
                bodies.push_back((
                    json!({"t":"snapshot","reference":snapshot}),
                    Body {
                        directory: snapshot_directory.clone(),
                        leaf: name,
                        reference: snapshot,
                        identity,
                    },
                ));
            }
            ensure!(
                pages.insert(format!("{}.json", page.sha256)),
                "duplicate import page"
            );
            bodies.push_back((
                json!({"t":"page","reference":page}),
                Body {
                    directory: page_directory.clone(),
                    leaf,
                    reference: SnapshotRef {
                        sha256: page.sha256.clone(),
                        bytes: page.bytes,
                    },
                    identity,
                },
            ));
        }
        inventory(&snapshot_directory, &snapshots)?;
        inventory(&page_directory, &pages)?;
        let start = json!({"abi":ABI,"configuration":location.configuration,"source":SOURCE,"replay":false,"import_id":reference.import_id,"max_bytes":reference.max_bytes});
        let mut current = serde_json::to_vec(&start)?;
        current.push(b'\n');
        Ok(Self {
            bodies,
            record,
            current: io::Cursor::new(current),
            stopped,
            until,
            ended: false,
            root,
            snapshots: (snapshot_directory, snapshots),
            pages: (page_directory, pages),
        })
    }
}
impl Read for Replay<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "service stopping or import deadline exceeded",
            ));
        }
        loop {
            let n = self.current.read(output)?;
            if n > 0 || output.is_empty() {
                return Ok(n);
            }
            if self.ended {
                return Ok(0);
            }
            let mut next = Vec::new();
            if let Some((header, body)) = self.bodies.pop_front() {
                body.directory.check().map_err(io::Error::other)?;
                let (bytes, identity) = read_body(
                    &body.directory.path.join(&body.leaf),
                    MAX_SNAPSHOT_BYTES as usize,
                )
                .map_err(io::Error::other)?;
                if identity != body.identity {
                    return Err(io::Error::other("import body replaced"));
                }
                body.reference.verify(&bytes).map_err(io::Error::other)?;
                next.extend(serde_json::to_vec(&header)?);
                next.push(b'\n');
                next.extend(bytes);
                next.push(b'\n');
            } else {
                inventory(
                    &self.root,
                    &["snapshots".into(), "pages".into(), "record-v1.json".into()]
                        .into_iter()
                        .collect(),
                )
                .map_err(io::Error::other)?;
                inventory(&self.snapshots.0, &self.snapshots.1).map_err(io::Error::other)?;
                inventory(&self.pages.0, &self.pages.1).map_err(io::Error::other)?;
                next.extend(serde_json::to_vec(
                    &json!({"t":"record","bytes":self.record.len()}),
                )?);
                next.push(b'\n');
                next.extend(&self.record);
                next.push(b'\n');
                self.ended = true;
            }
            self.current = io::Cursor::new(next);
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(all(test, target_os = "linux"))]
pub(in crate::desktop) use tests::pending as pending_for_test;
