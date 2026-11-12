//! Documents on disk, one directory per project:
//! `<root>/<sha256(project ID)>/project.loro` and `tasks/<uuid>.loro`, plus
//! `tasks/<uuid>.updates` with the updates appended since that snapshot. Each
//! appended block carries its length and a checksum; a block cut short or
//! damaged by a crash ends the list, and the next save rewrites the snapshot.
//! Updates are folded into a fresh snapshot when they grow, and once when a
//! task is accepted (its Done compaction). One process at a time holds a project.
use crate::project::ProjectDoc;
use crate::schema::Stage;
use crate::task::TaskDoc;
use crate::{ids, rank, Error, Result};
use fs2::FileExt;
use loro::VersionVector;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

mod copy_review;
pub use copy_review::{CopyReview, CopyReviewOutcome};

/// A rank longer than this renumbers its stage (ranks stay valid up to 64).
const RENUMBER_AT: usize = 32;
/// Appended updates beyond this size are folded into the snapshot.
const FOLD_BYTES: u64 = 256 * 1024;
/// A block: little-endian `u32` length, the first four bytes of the SHA-256
/// of the payload, then the payload (Loro updates).
const HEADER: usize = 8;
/// The change journal: a `gen <UUID> <BASE>` line, then one changed document
/// per line (a task UUID, or `project`). BASE is the place, in the journal's
/// whole history, of its first line here, so dropping its oldest lines keeps
/// every later place valid. A derived index reads it instead of every task
/// file. It is a hint only: never synced and never written in place. Each
/// change replaces it under its name, so a link there carries no write into
/// another file. A reader that finds it missing, oversized, not a regular
/// file, of another generation, past its place or with a line it does not
/// expect reads every task file instead.
const CHANGES: &str = "changes";
/// Past this the journal drops its oldest lines, keeping the newest half.
const CHANGES_LIMIT: usize = 64 * 1024;
/// The most of it ever read.
const CHANGES_READ: u64 = CHANGES_LIMIT as u64 + 4096;

/// The change journal as read: its generation, the place of its first line
/// and its whole lines.
struct Journal {
    generation: String,
    base: u64,
    body: Vec<u8>,
}

enum Hint {
    Missing,
    Unusable,
    Found(Journal),
}

/// Reads the change journal: bounded, never through a link and never
/// waiting on a special file.
fn read_journal(path: &Path) -> Hint {
    use std::io::Read;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Hint::Missing,
        Ok(meta) if meta.file_type().is_file() && meta.len() <= CHANGES_READ => (),
        _ => return Hint::Unusable,
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let Ok(file) = options.open(path) else {
        return Hint::Unusable;
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Hint::Unusable;
    }
    let mut bytes = Vec::new();
    if file.take(CHANGES_READ + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > CHANGES_READ
    {
        return Hint::Unusable;
    }
    // Whole lines only: a line cut short ends the journal.
    let whole = bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |end| end + 1);
    let Some(header) = bytes[..whole].split_inclusive(|&b| b == b'\n').next() else {
        return Hint::Unusable;
    };
    let parsed = std::str::from_utf8(header)
        .ok()
        .and_then(|line| line.strip_suffix('\n')?.strip_prefix("gen "))
        .and_then(|rest| rest.split_once(' '))
        .filter(|(generation, _)| ids::is_uuid(generation))
        .and_then(|(generation, base)| Some((generation.to_owned(), base.parse::<u64>().ok()?)))
        // A base with no room left to grow cannot be a real journal.
        .filter(|(_, base)| base.checked_add(CHANGES_READ * 2).is_some());
    match parsed {
        Some((generation, base)) => Hint::Found(Journal {
            generation,
            base,
            body: bytes[header.len()..whole].to_vec(),
        }),
        None => Hint::Unusable,
    }
}

/// Replaces the change journal under its name, never writing through it.
fn write_journal(path: &Path, generation: &str, base: u64, body: &[u8]) -> bool {
    let mut bytes = format!("gen {generation} {base}\n").into_bytes();
    bytes.extend_from_slice(body);
    let temp = path.with_extension(format!("tmp-{}", ids::new_uuid()));
    let written = private_options()
        .create_new(true)
        .write(true)
        .open(&temp)
        .and_then(|mut file| file.write_all(&bytes));
    if written.is_err() || fs::rename(&temp, path).is_err() {
        let _ = fs::remove_file(&temp);
        return false;
    }
    true
}

/// What changed since a reader's place in the change journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes {
    /// The journal's generation and the end of its last whole line; a
    /// reader passes both back next time.
    pub generation: String,
    pub offset: u64,
    /// The documents changed since that place, or `None` when the journal
    /// cannot tell (another generation, a damaged line): read every task.
    pub documents: Option<BTreeSet<String>>,
}

/// One of a task's files in the task folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskFile {
    /// `<uuid>.loro`
    Snapshot,
    /// `<uuid>.updates`, appended to the snapshot
    Updates,
    /// `<uuid>.managed`
    Mark,
}

/// The task an entry of the task folder named `name` belongs to, and which
/// of its files it is; None for any other entry. A mark alone is a task
/// too: kept for review, never invisible.
pub fn task_file(name: &std::ffi::OsStr) -> Option<(&str, TaskFile)> {
    let path = Path::new(name);
    let file = match path.extension().and_then(|e| e.to_str())? {
        "loro" => TaskFile::Snapshot,
        "updates" => TaskFile::Updates,
        "managed" => TaskFile::Mark,
        _ => return None,
    };
    let uuid = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| ids::is_uuid(s))?;
    Some((uuid, file))
}

/// A task file that failed to load, or a document an import refused. It
/// stays on disk and never blocks the others.
///
/// For a copy here that does not load, `kept` says the copy is kept for a
/// person's review whatever copy arrives, with `kept_reason` (`marked`, or
/// `unreadable_local_data`); otherwise `recovery` is
/// `depends_on_incoming_copy`: what happens is decided against each copy
/// that arrives, never shown as safe here. A refusal carries the decision
/// against that copy: `kept` with `marked`, `attempt_evidence`, `pending` or
/// `unreadable_local_data`. Information only; it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Damaged {
    pub uuid: String,
    pub error: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kept: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

/// Why a copy here that does not load is kept for a person's review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// This store cut it over, and nothing here proves another copy holds it.
    Marked,
    /// Its local data, merged with the incoming copy, shows attempts or a floor.
    AttemptEvidence,
    /// A local update block does not apply onto the incoming copy.
    Pending,
    /// Its local updates are torn or cannot be read.
    UnreadableLocalData,
}

impl Keep {
    pub fn as_str(self) -> &'static str {
        match self {
            Keep::Marked => "marked",
            Keep::AttemptEvidence => "attempt_evidence",
            Keep::Pending => "pending",
            Keep::UnreadableLocalData => "unreadable_local_data",
        }
    }
}

/// What a listing says when recovery is decided against each incoming copy.
pub const DEPENDS_ON_INCOMING_COPY: &str = "depends_on_incoming_copy";

impl Damaged {
    pub fn new(uuid: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            error: error.into(),
            kept: false,
            kept_reason: None,
            recovery: None,
        }
    }

    pub fn kept_for(mut self, reason: Keep) -> Self {
        self.kept = true;
        self.kept_reason = Some(reason.as_str().to_owned());
        self
    }
}

/// Where a task goes in the order of its stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Position {
    Top,
    End,
    /// Zero-based; beyond the end means the end.
    Index(usize),
    /// Before or after the task with this short ID or UUID.
    Before(String),
    After(String),
}

/// One task of a stage as a placement source has it (zerus-cg5h.24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub uuid: String,
    /// Its short ID.
    pub id: String,
    pub rank: String,
}

/// A stage's order and the short IDs in use, from a source that answers
/// without reading every task document (hgs's task index, zerus-cg5h.24).
/// An answer must be what the documents say: the loadable tasks only, in
/// (rank, UUID) order. `None` declines, and the whole write then reads the
/// documents as before.
pub trait Placement {
    /// The loadable tasks of `stage` in their order.
    fn order(&self, stage: Stage) -> Result<Option<Vec<Placed>>>;
    /// Whether a loadable task uses short ID `id`.
    fn short_id_used(&self, id: &str) -> Result<Option<bool>>;
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Task documents this thread loaded so far (tests of this and other crates).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn load_count_for_test() -> usize {
    LOADS.with(std::cell::Cell::get)
}

/// One load or save at a time through a store, in turn. An untimed entry
/// waits for its turn; a timed one waits until its deadline and is refused
/// then, before anything is read or written. A panic inside releases it.
#[derive(Default)]
struct Admission {
    held: std::sync::Mutex<bool>,
    freed: std::sync::Condvar,
}

struct Turn<'a>(&'a Admission);

impl Admission {
    fn enter(&self) -> Turn<'_> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *held {
            held = self
                .freed
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *held = true;
        Turn(self)
    }

    /// None once `deadline` has passed, whether or not another holds it.
    fn enter_until(&self, deadline: Instant) -> Option<Turn<'_>> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())?;
            if !*held {
                break;
            }
            held = self
                .freed
                .wait_timeout(held, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        *held = true;
        Some(Turn(self))
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        *self
            .0
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        // Every waiter checks again: one whose deadline passed as it woke
        // never keeps the turn from the others.
        self.0.freed.notify_all();
    }
}

/// A store's admission, held until dropped: its loads and saves come in
/// turn with every other load and save through the store, and nothing else
/// comes between them. A shared-store caller takes it with
/// `ProjectStore::admit_until`, so it never waits past its own deadline.
///
/// One call at a time: the guard is not `Sync`, and its load and save take
/// it by `&mut`, so two loads or saves through one admission never run at
/// once (review-n and desktop-u on #1446). While it is held, the store's
/// own `load` and `save` wait for it; use the guard's.
///
/// ```no_run
/// # fn check(store: &zerus_replica::ProjectStore, a: &mut zerus_replica::TaskDoc,
/// #          b: &mut zerus_replica::TaskDoc) -> zerus_replica::Result<()> {
/// let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
/// let mut admitted = store.admit_until(deadline).expect("the store is free");
/// admitted.save(a)?;
/// admitted.save(b)
/// # }
/// ```
///
/// It is never shared between threads:
///
/// ```compile_fail,E0277
/// # fn check(store: &zerus_replica::ProjectStore) {
/// fn shared<T: Sync>(_: &T) {}
/// let admitted = store.admit_until(std::time::Instant::now()).unwrap();
/// shared(&admitted);
/// # }
/// ```
///
/// nor used through a shared reference, even on one thread:
///
/// ```compile_fail,E0596
/// # fn check(store: &zerus_replica::ProjectStore) {
/// let admitted = store.admit_until(std::time::Instant::now()).unwrap();
/// let grant = &admitted;
/// let _ = grant.load("a");
/// # }
/// ```
///
/// so two threads never save through one admission (desktop-u's case;
/// either rule alone refuses it):
///
/// ```compile_fail,E0277
/// # fn check(store: &zerus_replica::ProjectStore, a: &mut zerus_replica::TaskDoc,
/// #          b: &mut zerus_replica::TaskDoc) {
/// let admitted = store.admit_until(std::time::Instant::now()).unwrap();
/// let grant = &admitted;
/// std::thread::scope(|scope| {
///     scope.spawn(move || grant.load("a"));
///     scope.spawn(move || grant.load("b"));
/// });
/// # }
/// ```
pub struct Admitted<'a> {
    store: &'a ProjectStore,
    _turn: Turn<'a>,
    /// Not `Sync`: a shared `&Admitted` never reaches another thread.
    _one_call: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl Admitted<'_> {
    /// `ProjectStore::load`, inside this admission.
    pub fn load(&mut self, uuid: &str) -> Result<TaskDoc> {
        self.store.load_admitted(uuid)
    }

    /// Read every retained byte without folding or repairing a readable
    /// prefix. Replacement proofs must refuse a cut, corrupt or unapplied
    /// block, as the existing agent-start counter and copy review do.
    pub fn load_complete(&mut self, uuid: &str) -> Result<TaskDoc> {
        let task = self.store.counted(uuid)?;
        if task.applied_version().is_none() {
            return Err(Error::failed("the local task has unapplied changes"));
        }
        Ok(task)
    }

    /// `ProjectStore::save`, inside this admission.
    pub fn save(&mut self, task: &mut TaskDoc) -> Result<()> {
        self.store.save_locked(task)
    }

    /// The project's directory, for a record kept beside its tasks under
    /// this admission and the project lock.
    pub fn dir(&self) -> &Path {
        self.store.dir()
    }

    /// The project document (its settings and hold), read inside this
    /// admission. Read only: it neither loads a task nor enters another
    /// admission.
    pub fn project(&self) -> Result<ProjectDoc> {
        self.store.project()
    }

    /// `ProjectStore::agent_starts_since`, counted inside this admission
    /// from the tasks as saved: every reservation and save serialized by it
    /// is counted, never a number read before it (#1519 review). It fails
    /// closed and never waits: any task that cannot be read whole as
    /// regular files (`counted`), and the deadline, are errors, never a
    /// count of zero.
    pub fn agent_starts_since(&mut self, since: i64, deadline: Instant) -> Result<usize> {
        let mut count = 0;
        for uuid in self.store.uuids()? {
            if Instant::now() >= deadline {
                return Err(Error::failed("the deadline passed while counting"));
            }
            count += self.store.counted(&uuid)?.agent_starts_since(since)?;
        }
        Ok(count)
    }
}

pub struct ProjectStore {
    dir: PathBuf,
    project: String,
    _lock: File,
    /// Serializes this instance's read-modify-write of the change journal:
    /// a store is shared between threads, and `save` takes `&self`.
    journal: std::sync::Mutex<()>,
    /// One save at a time through this store, so a save's check of what is
    /// durable and its write are one step: the project lock keeps other
    /// processes out, this keeps this one's threads in turn.
    admission: Admission,
    /// The version of each task this store last read from or wrote to its
    /// files. A document whose base is older is merged into the files'
    /// current state before it is saved, never written over it.
    durable: std::sync::Mutex<std::collections::HashMap<String, VersionVector>>,
}

fn private_options() -> OpenOptions {
    #[cfg(unix)]
    let mut options = OpenOptions::new();
    #[cfg(not(unix))]
    let options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", ids::new_uuid()));
    let mut file = private_options().create_new(true).write(true).open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    // The rename itself survives a power loss only once the directory is synced.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn checksum(payload: &[u8]) -> [u8; 4] {
    Sha256::digest(payload)[..4].try_into().expect("four bytes")
}

/// A block as this store writes it, for fixtures of other writers.
#[cfg(test)]
pub(crate) fn block_for_test(payload: &[u8]) -> Vec<u8> {
    block(payload)
}

fn block(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&checksum(payload));
    out.extend_from_slice(payload);
    out
}

/// The managed mark: what this store last saved of a task cut over to the
/// execution schema, written before each save makes it durable. `saved` is
/// the version vector the task's files then hold, by decimal peer; `opening`
/// is the SHA-256 of the first block of the updates file that follows the
/// snapshot, header and payload. Every fold of such a task opens that file
/// with the operations at the document's frontiers: a block every copy
/// already holds, so it imports as nothing in this build and older ones,
/// and one that differs whenever the version does. A writer that folds or
/// rewrites the files without this mark (an older build), or a mark restored
/// from before a later fold, leaves the opening apart from the files; files
/// cut short leave them holding less than `saved`. On disk it carries a
/// `seal`, the SHA-256 of its whole canonical content, so a change to any
/// field (a flipped bit included) makes it unreadable and keeps the copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mark {
    pub(crate) saved: Option<BTreeMap<String, i32>>,
    #[serde(default)]
    pub(crate) opening: Option<String>,
}

/// The format of the mark and its seal. Every cache that remembers whether
/// a task loads (the inventory cache, the task index) names it, so a change
/// to what makes a mark readable makes them read every task again.
pub const MARK_FORMAT: i64 = 1;

/// The mark as written: its fields and their seal.
#[derive(Serialize, Deserialize)]
struct SealedMark {
    saved: Option<BTreeMap<String, i32>>,
    #[serde(default)]
    opening: Option<String>,
    #[serde(default)]
    seal: Option<String>,
}

impl Mark {
    /// The digest over the mark's canonical content (its fields in order,
    /// the peers sorted).
    fn seal(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(format!("zerus-task-mark-{MARK_FORMAT}\n").as_bytes());
        hasher.update(serde_json::to_vec(self)?);
        Ok(format!("{:x}", hasher.finalize()))
    }

    fn sealed(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&SealedMark {
            saved: self.saved.clone(),
            opening: self.opening.clone(),
            seal: Some(self.seal()?),
        })?)
    }

    /// A mark read back; none unless its seal matches its content.
    fn unsealed(bytes: &[u8]) -> Option<Mark> {
        let sealed: SealedMark = serde_json::from_slice(bytes).ok()?;
        let mark = Mark {
            saved: sealed.saved,
            opening: sealed.opening,
        };
        (sealed.seal? == mark.seal().ok()?).then_some(mark)
    }

    /// The mark for files that hold `saved`, whose updates file opens with
    /// `first` (header and payload).
    pub(crate) fn new(saved: &VersionVector, first: &[u8]) -> Self {
        Self {
            saved: Some(
                saved
                    .iter()
                    .map(|(peer, counter)| (peer.to_string(), *counter))
                    .collect(),
            ),
            opening: Some(digest(first)),
        }
    }

    /// The saved version it records, when it records a possible one: the
    /// files hold the task's creation, so an empty version or a peer counted
    /// at zero or below is unreadable.
    pub(crate) fn version(&self) -> Option<VersionVector> {
        let saved = self.saved.as_ref().filter(|s| !s.is_empty())?;
        saved
            .iter()
            .map(|(peer, counter)| Some((peer.parse().ok()?, (*counter > 0).then_some(*counter)?)))
            .collect()
    }

    /// Whether it names `first` as the first block of the updates file.
    fn opens_with(&self, first: &[u8]) -> bool {
        self.opening.as_deref() == Some(digest(first).as_str())
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The first block of an updates file, header and payload; none when the
/// file is missing or its first block is cut short.
fn first_block(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = match open_regular(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata()?.len();
    let mut block = vec![0u8; HEADER];
    match std::io::Read::read_exact(&mut file, &mut block) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_le_bytes(block[..4].try_into().expect("four bytes")) as u64;
    // A length past the file is a cut block, never an allocation.
    if length == 0 || HEADER as u64 + length > size {
        return Ok(None);
    }
    block.resize(HEADER + length as usize, 0);
    std::io::Read::read_exact(&mut file, &mut block[HEADER..])?;
    Ok(Some(block))
}

/// What a load knows of a task's mark.
enum Marked {
    No,
    Unreadable,
    Saved(VersionVector),
}

#[cfg(test)]
thread_local! {
    /// Runs while a first store identity is being created, under its lock.
    pub(crate) static STORE_ID_CREATING: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Runs inside an observation, between its read and its stamps.
    pub(crate) static OBSERVING: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// A store file opened for reading without waiting (#1519 review,
/// zerus-r75ru): only a regular file, refused before it is opened otherwise;
/// opened without following a final link or waiting for a FIFO's writer; the
/// handle a regular file. A missing file is `NotFound`.
fn open_regular(path: &Path) -> std::io::Result<File> {
    let irregular = || std::io::Error::other("not a regular file");
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(irregular());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: a link is opened itself.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(irregular());
    }
    Ok(file)
}

/// One task or project file, read whole without waiting (`open_regular`).
/// Every read of a store's documents goes through it, so a FIFO, device or
/// link in a task's place never holds a command that reads every task: that
/// task does not load, and is listed as damaged.
///
/// No size is refused (zerus-ern6o): no writer bounds these files. A
/// project's or a task's Loro history grows with every edit, and only a
/// task's visible text is held to `MAX_DOCUMENT`, so any cap here would make
/// a file this store wrote unreadable. A read takes the memory a load of
/// that file always took; only regular files are read.
pub(crate) fn read_regular(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The most a `store-id` may hold: its writers write a canonical UUID and a
/// newline (37 bytes), and a read trims surrounding whitespace.
const STORE_ID_BYTES: u64 = 1024;
/// The most a `project-id` may hold: the project ID itself, which `opening`
/// refuses past 128 bytes, written exactly.
pub(crate) const PROJECT_ID_BYTES: u64 = 128;

/// A small identity file read whole without waiting (`open_regular`), at
/// most `limit` bytes (zerus-iih7g): `None` only when nothing is at its path.
/// Anything else that is not a regular file within the limit (a FIFO, a
/// device, a link, even a dangling one, or a larger file) is an error, so an
/// identity that cannot be read is never taken for a missing one.
pub(crate) fn read_identity(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let file = match open_regular(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other("larger than an identity"));
    }
    Ok(Some(bytes))
}

/// The intact blocks with their end offsets, and whether the whole input was
/// intact. A short, empty or mismatched block ends the list.
pub(crate) fn blocks(bytes: &[u8]) -> (Vec<(&[u8], usize)>, bool) {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.len() < HEADER {
            return (out, false);
        }
        let length = u32::from_le_bytes(rest[..4].try_into().expect("four bytes")) as usize;
        if length == 0 || rest.len() - HEADER < length {
            return (out, false);
        }
        let payload = &rest[HEADER..HEADER + length];
        if checksum(payload) != rest[4..8] {
            return (out, false);
        }
        at += HEADER + length;
        out.push((payload, at));
    }
    (out, true)
}

impl ProjectStore {
    /// The random identity of the replica root below `root`, in `store-id`,
    /// created on first use. A root restored from a backup or copied to
    /// another computer has its `store-id` deleted, so a new one is created
    /// and other computers see a changed store.
    pub fn store_id(root: &Path) -> Result<String> {
        Ok(Self::store_id_with(root, None)?.expect("an untimed store identity waits for its lock"))
    }

    /// `store_id`, waiting for the creation lock no later than `deadline`:
    /// none when the deadline has passed or another creator still holds it.
    pub fn store_id_until(root: &Path, deadline: Instant) -> Result<Option<String>> {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        Self::store_id_with(root, Some(deadline))
    }

    fn store_id_with(root: &Path, deadline: Option<Instant>) -> Result<Option<String>> {
        private_dir(root)?;
        let path = root.join("store-id");
        // The identity, or none when there is none yet: nothing at its path,
        // or an empty file, which the first writer (create, then one write)
        // left when it stopped between the two, before any identity was
        // written. Anything else that is not a canonical UUID within the
        // bound, readable without waiting, is an identity that cannot be
        // known (zerus-iih7g): refused and kept, never replaced by another.
        let read = |path: &Path| -> Result<Option<String>> {
            let unknown = |why: &str| {
                Error::failed(format!(
                    "this computer's replica identity ({}) {why}; it is kept, and a new one is \
                     made only after a person removes it",
                    path.display()
                ))
            };
            let bytes = match read_identity(path, STORE_ID_BYTES) {
                Ok(None) => return Ok(None),
                Ok(Some(bytes)) if bytes.is_empty() => return Ok(None),
                Ok(Some(bytes)) => bytes,
                Err(error) => return Err(unknown(&format!("cannot be read: {error}"))),
            };
            let text = String::from_utf8(bytes).map_err(|_| unknown("is not text"))?;
            let id = text.trim();
            if !ids::is_uuid(id) {
                return Err(unknown("is not a canonical identity"));
            }
            Ok(Some(id.to_owned()))
        };
        if let Some(id) = read(&path)? {
            return Ok(Some(id));
        }
        // Creating it is one step across processes: under this lock the file
        // is read again, and only then written whole (an atomic replace), so
        // a creator never replaces another's identity and nothing reads a
        // partial one.
        let lock = private_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("store-id.lock"))?;
        match deadline {
            None => lock.lock_exclusive()?,
            Some(deadline) => {
                while lock.try_lock_exclusive().is_err() {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        if let Some(id) = read(&path)? {
            return Ok(Some(id));
        }
        let id = ids::new_uuid();
        #[cfg(test)]
        STORE_ID_CREATING.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
        write_atomic(&path, format!("{id}\n").as_bytes())?;
        Ok(Some(id))
    }

    /// Opens the project's directory below `root` and waits for its lock.
    pub fn open(root: &Path, project: &str) -> Result<Self> {
        Ok(Self::opening(root, project, None)?.expect("open waits for the lock"))
    }

    /// Opens the project like `open`, but gives up after `wait` while another
    /// process holds its lock.
    pub fn open_within(root: &Path, project: &str, wait: Duration) -> Result<Option<Self>> {
        Self::opening(root, project, Some(wait))
    }

    fn opening(root: &Path, project: &str, wait: Option<Duration>) -> Result<Option<Self>> {
        if project.is_empty() || project.len() > 128 || project.chars().any(char::is_control) {
            return Err(Error::new("invalid project ID"));
        }
        private_dir(root)?;
        let dir = root.join(format!("{:x}", Sha256::digest(project.as_bytes())));
        private_dir(&dir)?;
        private_dir(&dir.join("tasks"))?;
        let lock = private_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))?;
        match wait {
            None => lock.lock_exclusive()?,
            Some(wait) => {
                let deadline = Instant::now() + wait;
                while lock.try_lock_exclusive().is_err() {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        // A crash during an atomic write leaves its temporary file behind. No
        // one else writes while this lock is held, so any left now is garbage;
        // removing it is best effort.
        for folder in [dir.clone(), dir.join("tasks")] {
            for entry in fs::read_dir(&folder)? {
                let path = entry?.path();
                let temporary = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.starts_with("tmp-"));
                if temporary {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        let name = dir.join("project-id");
        if !name.exists() {
            write_atomic(&name, project.as_bytes())?;
        }
        Ok(Some(Self {
            dir,
            project: project.into(),
            _lock: lock,
            journal: std::sync::Mutex::new(()),
            admission: Admission::default(),
            durable: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
    }

    pub fn project_id(&self) -> &str {
        &self.project
    }

    /// The project's folder: derived local caches (such as hgs's task index)
    /// live beside its documents and are never replicated.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn project(&self) -> Result<ProjectDoc> {
        match read_regular(&self.dir.join("project.loro")) {
            Ok(bytes) => ProjectDoc::from_bytes(&bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(ProjectDoc::new()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save_project(&self, doc: &ProjectDoc) -> Result<()> {
        write_atomic(&self.dir.join("project.loro"), &doc.snapshot()?)?;
        self.note_change("project");
        Ok(())
    }

    /// Notes `document` in the change journal by replacing it, never writing
    /// through its name; an unusable one starts a new generation. Best
    /// effort: a lost line is found by a reader's periodic check of every
    /// task file.
    pub(crate) fn note_change(&self, document: &str) {
        let _serial = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.dir.join(CHANGES);
        let (generation, mut base, mut body) = match read_journal(&path) {
            Hint::Found(journal) => (journal.generation, journal.base, journal.body),
            Hint::Missing | Hint::Unusable => (ids::new_uuid(), 0, Vec::new()),
        };
        body.extend_from_slice(document.as_bytes());
        body.push(b'\n');
        if body.len() > CHANGES_LIMIT {
            let from = body.len() - CHANGES_LIMIT / 2;
            let cut = body[from..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(body.len(), |at| from + at + 1);
            match base.checked_add(cut as u64) {
                Some(moved) => {
                    base = moved;
                    body.drain(..cut);
                }
                // No room left: a new generation, with only this change.
                None => {
                    let fresh = ids::new_uuid();
                    let line = format!("{document}\n");
                    write_journal(&path, &fresh, 0, line.as_bytes());
                    return;
                }
            }
        }
        write_journal(&path, &generation, base, &body);
    }

    /// The documents changed since place `offset` of journal `generation`
    /// (both as returned last time; an empty generation the first time). A
    /// missing or unusable journal cannot say: it is started again, empty, so
    /// later reads can, and this one reports every document as unknown.
    pub fn changes_since(&self, generation: &str, offset: u64) -> Result<Changes> {
        let _serial = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.dir.join(CHANGES);
        let journal = match read_journal(&path) {
            Hint::Found(journal) => journal,
            Hint::Missing | Hint::Unusable => {
                let fresh = ids::new_uuid();
                let started = write_journal(&path, &fresh, 0, &[]);
                return Ok(Changes {
                    generation: if started { fresh } else { String::new() },
                    offset: 0,
                    documents: None,
                });
            }
        };
        let Some(end) = journal.base.checked_add(journal.body.len() as u64) else {
            return Ok(Changes {
                generation: String::new(),
                offset: 0,
                documents: None,
            });
        };
        let mut changes = Changes {
            generation: journal.generation,
            offset: end,
            documents: None,
        };
        if changes.generation != generation || offset < journal.base || offset > end {
            return Ok(changes);
        }
        let mut documents = BTreeSet::new();
        let unread = &journal.body[(offset - journal.base) as usize..];
        for line in unread.split_inclusive(|&b| b == b'\n') {
            match std::str::from_utf8(line)
                .ok()
                .and_then(|l| l.strip_suffix('\n'))
            {
                Some(document) if document == "project" || ids::is_uuid(document) => {
                    documents.insert(document.to_owned());
                }
                _ => return Ok(changes),
            }
        }
        changes.documents = Some(documents);
        Ok(changes)
    }

    pub(crate) fn snapshot_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("tasks").join(format!("{uuid}.loro"))
    }

    pub(crate) fn updates_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("tasks").join(format!("{uuid}.updates"))
    }

    /// Beside a task's files: present once this store saved it cut over to the
    /// execution schema (attempts or a floor), recording the version its
    /// snapshot holds. It outlives a damaged copy, so that copy is never
    /// replaced by another replica's whole document that lacks part of it.
    pub(crate) fn managed_path(&self, uuid: &str) -> PathBuf {
        self.dir.join("tasks").join(format!("{uuid}.managed"))
    }

    /// Whether this store ever saved the task cut over to the execution
    /// schema. Any entry at the mark's path counts, even one that cannot be
    /// read or a dangling link: only its verified absence means unmarked.
    pub fn was_managed(&self, uuid: &str) -> bool {
        !matches!(
            fs::symlink_metadata(self.managed_path(uuid)),
            Err(error) if error.kind() == ErrorKind::NotFound
        )
    }

    /// The task's mark; none when it is missing or cannot be read.
    pub(crate) fn mark(&self, uuid: &str) -> Option<Mark> {
        Mark::unsealed(&read_regular(&self.managed_path(uuid)).ok()?)
    }

    /// Whether the task is marked, and the version its mark says this store
    /// saved: a single read of the mark, then a check of its path only
    /// when there is nothing to read.
    fn marked(&self, uuid: &str) -> Marked {
        let path = self.managed_path(uuid);
        match read_regular(&path) {
            Ok(bytes) => Mark::unsealed(&bytes)
                .and_then(|mark| mark.version())
                .map_or(Marked::Unreadable, Marked::Saved),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == ErrorKind::NotFound => Marked::No,
                    _ => Marked::Unreadable,
                }
            }
            Err(_) => Marked::Unreadable,
        }
    }

    /// Writes the mark. Every listing stamps it with the task's files, so
    /// a save that fails after its mark still reaches them.
    pub(crate) fn write_mark(&self, uuid: &str, mark: &Mark) -> Result<()> {
        write_atomic(&self.managed_path(uuid), &mark.sealed()?)
    }

    /// What is known here, read-only, of a copy that does not load: kept
    /// for every incoming copy (it is marked, or its local updates are torn
    /// or unreadable), or none when recovery is decided against each copy
    /// that arrives. Recovery (`import_task`) keeps a copy in each case
    /// this returns.
    pub fn locally_kept(&self, uuid: &str) -> Option<Keep> {
        if self.was_managed(uuid) {
            return Some(Keep::Marked);
        }
        match read_regular(&self.updates_path(uuid)) {
            Ok(bytes) => (!blocks(&bytes).1).then_some(Keep::UnreadableLocalData),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => Some(Keep::UnreadableLocalData),
        }
    }

    /// A copy here that does not load, as a listing names it.
    pub fn damaged(&self, uuid: &str, error: impl Into<String>) -> Damaged {
        let listed = Damaged::new(uuid, error);
        match self.locally_kept(uuid) {
            Some(reason) => listed.kept_for(reason),
            None => Damaged {
                recovery: Some(DEPENDS_ON_INCOMING_COPY.to_owned()),
                ..listed
            },
        }
    }

    /// The version this store last saved, as the mark records it, only while
    /// the updates file still opens with the exact block the mark names: no
    /// other writer folded or rewrote the files, and no later fold of this
    /// store replaced them, since the mark was written.
    pub(crate) fn fresh_version(&self, uuid: &str) -> Option<VersionVector> {
        let mark = self.mark(uuid)?;
        let first = first_block(&self.updates_path(uuid)).ok()??;
        mark.opens_with(&first).then(|| mark.version()).flatten()
    }

    /// How many tasks this store cut over to the execution schema (each has
    /// a mark), and when the latest of their saves wrote its mark. Names and
    /// stamps only, no task is read, within `entries` directory entries and
    /// `time`: none when either runs out first, never a partial count.
    pub fn cut_over_within(
        &self,
        entries: usize,
        time: Duration,
    ) -> Result<Option<(usize, Option<SystemTime>)>> {
        let started = Instant::now();
        let mut count = 0;
        let mut latest: Option<SystemTime> = None;
        for (seen, entry) in fs::read_dir(self.dir.join("tasks"))?.enumerate() {
            if seen >= entries || started.elapsed() > time {
                return Ok(None);
            }
            let path = entry?.path();
            let marked = path.extension().and_then(|e| e.to_str()) == Some("managed")
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(ids::is_uuid);
            if marked {
                count += 1;
                if let Ok(modified) = fs::symlink_metadata(&path).and_then(|m| m.modified()) {
                    latest = Some(latest.map_or(modified, |l| l.max(modified)));
                }
            }
        }
        // The last read and stat count against the time too.
        if started.elapsed() > time {
            return Ok(None);
        }
        Ok(Some((count, latest)))
    }

    pub fn uuids(&self) -> Result<Vec<String>> {
        let mut uuids = BTreeSet::new();
        for entry in fs::read_dir(self.dir.join("tasks"))? {
            let name = entry?.file_name();
            if let Some((uuid, _)) = task_file(&name) {
                uuids.insert(uuid.to_owned());
            }
        }
        Ok(uuids.into_iter().collect())
    }

    /// Reads the task, in turn with this store's saves.
    pub fn load(&self, uuid: &str) -> Result<TaskDoc> {
        let _turn = self.admission.enter();
        self.load_admitted(uuid)
    }

    /// This store's admission, waiting for it no later than `deadline`.
    /// None when another load or save still holds it then, or the deadline
    /// has already passed: nothing was read or written (zerus-cg5h.4.4).
    pub fn admit_until(&self, deadline: Instant) -> Option<Admitted<'_>> {
        let turn = self.admission.enter_until(deadline)?;
        Some(Admitted {
            store: self,
            _turn: turn,
            _one_call: std::marker::PhantomData,
        })
    }

    /// A task read only for the agent-start count, inside an admission:
    /// every file of it read whole (`read_regular`), and a copy that holds
    /// less than its mark is damaged. It writes nothing (no mark is raised).
    fn counted(&self, uuid: &str) -> Result<TaskDoc> {
        self.counted_with(uuid, read_regular)
    }

    fn counted_with(
        &self,
        uuid: &str,
        read: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
    ) -> Result<TaskDoc> {
        let mut task = TaskDoc::from_bytes(&read(&self.snapshot_path(uuid))?)?;
        if task.uuid() != uuid {
            return Err(Error::failed("the task file does not match its name"));
        }
        // Every update block must frame and import whole: a broken or cut
        // record is unknown evidence, never fewer starts (#1519 R3).
        match read(&self.updates_path(uuid)) {
            Ok(bytes) => {
                let (found, intact) = blocks(&bytes);
                if !intact {
                    return Err(Error::failed("the task's updates are not whole"));
                }
                for (payload, _) in found {
                    if !task.import_block(payload) {
                        return Err(Error::failed("an update of the task cannot be read"));
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match read(&self.managed_path(uuid)) {
            Ok(bytes) => {
                let saved = Mark::unsealed(&bytes)
                    .and_then(|mark| mark.version())
                    .ok_or_else(|| Error::failed("the copy's mark cannot be read"))?;
                if !task.current_vv().includes_vv(&saved) {
                    return Err(Error::failed(
                        "the copy holds less than this computer saved",
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        task.view()?;
        Ok(task)
    }

    fn load_admitted(&self, uuid: &str) -> Result<TaskDoc> {
        #[cfg(any(test, feature = "test-support"))]
        LOADS.with(|n| n.set(n.get() + 1));
        let mut task = TaskDoc::from_bytes(&read_regular(&self.snapshot_path(uuid))?)?;
        if task.uuid() != uuid {
            return Err(Error::failed("the task file does not match its name"));
        }
        let mut first = None;
        match read_regular(&self.updates_path(uuid)) {
            Ok(bytes) => {
                let (found, intact) = blocks(&bytes);
                task.needs_fold = !intact;
                first = found.first().map(|(_, end)| bytes[..*end].to_vec());
                for (payload, end) in found {
                    if !task.import_block(payload) {
                        task.needs_fold = true;
                        break;
                    }
                    task.saved_len = end as u64;
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // A copy this store cut over holds at least what its mark says was
        // saved (one small read); less is files cut short, a save that never
        // finished, or an older build that wrote them so, and the copy is
        // damaged rather than read without those changes. A mark that cannot
        // be read proves nothing, so it keeps the copy too.
        match self.marked(uuid) {
            Marked::No => {}
            Marked::Unreadable => {
                return Err(Error::failed(
                    "the copy here was cut over to execution attempts and its mark cannot be read",
                ))
            }
            Marked::Saved(saved) => {
                if !task.current_vv().includes_vv(&saved) {
                    return Err(Error::failed(
                        "the copy here holds less than this computer saved: its files were cut \
                         short or a save did not finish",
                    ));
                }
                // The files hold more than the mark, or open with another
                // block (an older build folded or appended): the mark is
                // raised to what they hold before anything is read from
                // them, so nothing is acknowledged or sent beyond it. It
                // only rises, and names no opening when the files have none.
                let raised = match &first {
                    Some(first) => Mark::new(&task.current_vv(), first),
                    None => Mark {
                        opening: None,
                        ..Mark::new(&task.current_vv(), &[])
                    },
                };
                if self.mark(uuid).as_ref() != Some(&raised) {
                    self.write_mark(uuid, &raised)?;
                }
            }
        }
        // A task that cannot be shown is damaged: listed apart, never blocking the rest.
        task.view()?;
        task.saved = Some(task.current_vv());
        task.saved_stage = Some(task.stage()?);
        self.durable(uuid, task.current_vv());
        Ok(task)
    }

    fn durable(&self, uuid: &str, version: VersionVector) {
        self.durable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(uuid.to_owned(), version);
    }

    /// Saves the task. A document read from this store before a later save
    /// of the same task is first merged into the files' current state: its
    /// own new changes are applied there, and every change saved since is
    /// kept. The mark of a cut-over task never moves back.
    pub fn save(&self, task: &mut TaskDoc) -> Result<()> {
        let _turn = self.admission.enter();
        self.save_locked(task)
    }

    /// Reads a task, saves it when its execution schema needs the repair, and
    /// stamps its files with `stamp`, all in one admission: the version read
    /// and the stamps taken are one observation, which no save of this store
    /// comes between. The inventory reads every task so, and so does any
    /// cache of its versions (hgs's task index).
    pub fn observe<S>(&self, uuid: &str, stamp: impl FnOnce() -> S) -> (Result<TaskDoc>, S) {
        let _turn = self.admission.enter();
        let loaded = self.load_admitted(uuid).and_then(|mut task| {
            if task.repair_schema()? {
                self.save_locked(&mut task)?;
            }
            Ok(task)
        });
        #[cfg(test)]
        OBSERVING.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
        (loaded, stamp())
    }

    fn save_locked(&self, task: &mut TaskDoc) -> Result<()> {
        let durable = self
            .durable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task.uuid())
            .cloned();
        if let (Some(base), Some(durable)) = (task.saved.clone(), durable) {
            if base != durable {
                let changes = task.export_since_vv(&base)?;
                let mut current = self.load_admitted(task.uuid())?;
                if !current.import_block(&changes) {
                    return Err(Error::new(
                        "this task changed here meanwhile and these changes do not apply to it; \
                         read it again",
                    ));
                }
                current.repair_schema()?;
                *task = current;
            }
        }
        // Classified under this admission, whatever the document's base: a
        // mark that is present but cannot be read keeps the copy for a
        // person's review, and nothing here rewrites it.
        match self.marked(task.uuid()) {
            Marked::No => {}
            Marked::Unreadable => {
                return Err(Error::new(
                    "this task was cut over to execution attempts here and its mark cannot be \
                     read; it is kept for a person's review and nothing is written for it",
                ))
            }
            Marked::Saved(saved) => {
                if !task.current_vv().includes_vv(&saved) {
                    return Err(Error::new(
                        "this copy holds less than this computer already saved of the task; \
                         read it again",
                    ));
                }
            }
        }
        self.save_admitted(task)?;
        self.durable(task.uuid(), task.current_vv());
        Ok(())
    }

    fn save_admitted(&self, task: &mut TaskDoc) -> Result<()> {
        let snapshot = self.snapshot_path(task.uuid());
        let updates = self.updates_path(task.uuid());
        let current = task.current_vv();
        let stage = task.stage()?;
        let on_disk = match fs::metadata(&updates) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let fold = task.saved.is_none()
            || !snapshot.exists()
            || task.needs_fold
            // Bytes past the valid blocks: a cut append, or another writer.
            || on_disk != task.saved_len
            || on_disk >= FOLD_BYTES
            || (stage == Stage::Done && task.saved_stage != Some(Stage::Done));
        if !fold && task.saved.as_ref() == Some(&current) {
            return Ok(());
        }
        let managed = task.execution_managed();
        // A fold of a cut-over task opens its updates file with the
        // operations at the frontiers; any other save appends the changes.
        let next = if fold {
            managed
                .then(|| task.export_frontier().map(|bytes| block(&bytes)))
                .transpose()?
        } else {
            let saved = task.saved.as_ref().expect("checked above");
            Some(block(&task.export_since_vv(saved)?))
        };
        // The mark precedes the files it describes, on every save of a
        // cut-over task (one small durable write each): after a crash between
        // them it names a version or a block they do not hold, which only
        // keeps a later recovery from going ahead.
        if managed {
            let ours = next.as_deref().expect("a cut-over save writes a block");
            let existing = if fold { None } else { first_block(&updates)? };
            let mark = Mark::new(&current, existing.as_deref().unwrap_or(ours));
            if self.mark(task.uuid()).as_ref() != Some(&mark) {
                self.write_mark(task.uuid(), &mark)?;
            }
        }
        if fold {
            // The snapshot first: a crash before the removal leaves updates
            // the snapshot already contains, and importing them again is harmless.
            write_atomic(&snapshot, &task.snapshot()?)?;
            match &next {
                Some(first) => write_atomic(&updates, first)?,
                None => match fs::remove_file(&updates) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                },
            }
            task.saved_len = next.as_ref().map_or(0, |first| first.len() as u64);
            task.needs_fold = false;
        } else {
            let bytes = next.expect("an append writes a block");
            let mut file = private_options().create(true).append(true).open(&updates)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            task.saved_len += bytes.len() as u64;
        }
        task.saved = Some(current);
        task.saved_stage = Some(stage);
        self.note_change(task.uuid());
        Ok(())
    }

    /// Every task that loads, and the ones that do not.
    pub fn tasks(&self) -> Result<(Vec<TaskDoc>, Vec<Damaged>)> {
        let mut tasks = Vec::new();
        let mut damaged = Vec::new();
        for uuid in self.uuids()? {
            match self.load(&uuid) {
                Ok(task) => tasks.push(task),
                Err(error) => damaged.push(self.damaged(&uuid, error.to_string())),
            }
        }
        Ok((tasks, damaged))
    }

    /// A task by its UUID or its short ID.
    pub fn find(&self, reference: &str) -> Result<TaskDoc> {
        if ids::is_uuid(reference) {
            if !self.snapshot_path(reference).exists()
                && !self.updates_path(reference).exists()
                && !self.was_managed(reference)
            {
                return Err(Error::new(format!("no task {reference} in this project")));
            }
            return self
                .load(reference)
                .map_err(|e| Error::failed(format!("task {reference} is damaged: {e}")));
        }
        if !ids::is_short_id(reference) {
            return Err(Error::new(format!("{reference:?} is not a task ID")));
        }
        let (tasks, damaged) = self.tasks()?;
        let mut found: Vec<_> = tasks.into_iter().filter(|t| t.id() == reference).collect();
        match found.len() {
            // A damaged file's short ID cannot be read: it may be the one.
            0 if !damaged.is_empty() => Err(Error::new(format!(
                "no task {reference} in this project; {} could not be read",
                match damaged.len() {
                    1 => "1 task file".to_owned(),
                    n => format!("{n} task files"),
                }
            ))),
            0 => Err(Error::new(format!("no task {reference} in this project"))),
            1 => Ok(found.pop().expect("one")),
            _ => Err(Error::new(format!(
                "{reference} names more than one task; use its UUID"
            ))),
        }
    }

    /// A short ID no local task uses yet.
    pub fn unused_short_id(&self) -> Result<String> {
        let (tasks, _) = self.tasks()?;
        let used: BTreeSet<String> = tasks.iter().map(TaskDoc::id).collect();
        loop {
            let id = ids::short_id();
            if !used.contains(&id) {
                return Ok(id);
            }
        }
    }

    /// The tasks of `stage` in their order: rank, then UUID for equal ranks.
    pub fn ordered(&self, stage: Stage) -> Result<Vec<TaskDoc>> {
        let (tasks, _) = self.listing()?;
        Ok(tasks
            .into_iter()
            .filter(|t| t.stage().ok() == Some(stage))
            .collect())
    }

    /// Every task, loaded once and sorted by rank then UUID (so each stage is
    /// in its order), and the ones that do not load.
    pub fn listing(&self) -> Result<(Vec<TaskDoc>, Vec<Damaged>)> {
        let (mut tasks, damaged) = self.tasks()?;
        tasks.sort_by(|a, b| (a.rank(), a.uuid()).cmp(&(b.rank(), b.uuid())));
        Ok((tasks, damaged))
    }

    /// The rank for `position` in `stage` (leaving out `moving`) and, with
    /// `need_id`, an unused short ID: from `placement` when its answer holds,
    /// else from the documents as `rank_for` and `unused_short_id` give them,
    /// for the whole write (zerus-cg5h.24).
    pub(crate) fn place(
        &self,
        placement: Option<&dyn Placement>,
        stage: Stage,
        position: &Position,
        moving: Option<&str>,
        need_id: bool,
    ) -> Result<(String, Option<String>)> {
        if let Some(placement) = placement {
            if let Some(placed) = self.place_from(placement, stage, position, moving, need_id)? {
                return Ok(placed);
            }
        }
        let rank = self.rank_for(stage, position, moving)?;
        let id = need_id.then(|| self.unused_short_id()).transpose()?;
        Ok((rank, id))
    }

    /// `place` from `placement`, or None when it declines or its answer may
    /// not hold: a reference it does not know, a rank that needs the stage
    /// renumbered (read from the documents, with its schema rule), or a
    /// chosen neighbour whose document is not in `stage` with the rank given.
    /// Only those neighbours are read.
    fn place_from(
        &self,
        placement: &dyn Placement,
        stage: Stage,
        position: &Position,
        moving: Option<&str>,
        need_id: bool,
    ) -> Result<Option<(String, Option<String>)>> {
        let Some(order) = placement.order(stage)? else {
            return Ok(None);
        };
        let list: Vec<&Placed> = order
            .iter()
            .filter(|t| Some(t.uuid.as_str()) != moving)
            .collect();
        let at = |reference: &str| {
            list.iter()
                .position(|t| t.id == reference || t.uuid == reference)
        };
        let index = match position {
            Position::Top => 0,
            Position::End => list.len(),
            Position::Index(index) => (*index).min(list.len()),
            Position::Before(reference) => match at(reference) {
                Some(index) => index,
                None => return Ok(None),
            },
            Position::After(reference) => match at(reference) {
                Some(index) => index + 1,
                None => return Ok(None),
            },
        };
        let low = index.checked_sub(1).map(|i| list[i]);
        // As `rank_for`: aim below the first rank higher than the one before.
        let high = list[index..]
            .iter()
            .copied()
            .find(|t| low.is_none_or(|l| t.rank > l.rank));
        let Ok(rank) = rank::between(low.map(|t| t.rank.as_str()), high.map(|t| t.rank.as_str()))
        else {
            return Ok(None);
        };
        if rank.len() > RENUMBER_AT {
            return Ok(None);
        }
        for neighbour in [low, high].into_iter().flatten() {
            let holds = self.load(&neighbour.uuid).is_ok_and(|task| {
                task.stage().ok() == Some(stage) && task.rank() == neighbour.rank
            });
            if !holds {
                return Ok(None);
            }
        }
        let id = if need_id {
            loop {
                let id = ids::short_id();
                match placement.short_id_used(&id)? {
                    None => return Ok(None),
                    Some(false) => break Some(id),
                    Some(true) => {}
                }
            }
        } else {
            None
        };
        Ok(Some((rank, id)))
    }

    /// The rank that puts a task at `position` in `stage`, leaving out the
    /// task being moved (its UUID).
    pub fn rank_for(
        &self,
        stage: Stage,
        position: &Position,
        moving: Option<&str>,
    ) -> Result<String> {
        let list: Vec<TaskDoc> = self
            .ordered(stage)?
            .into_iter()
            .filter(|t| Some(t.uuid()) != moving)
            .collect();
        let at = |reference: &str| {
            list.iter()
                .position(|t| t.id() == reference || t.uuid() == reference)
                .ok_or_else(|| Error::new(format!("{reference} is not in {}", stage.label())))
        };
        let index = match position {
            Position::Top => 0,
            Position::End => list.len(),
            Position::Index(index) => (*index).min(list.len()),
            Position::Before(reference) => at(reference)?,
            Position::After(reference) => at(reference)? + 1,
        };
        let low = index.checked_sub(1).map(|i| list[i].rank());
        // Equal ranks can meet after concurrent moves: aim below the first higher one.
        let high = list[index..]
            .iter()
            .map(TaskDoc::rank)
            .find(|r| low.as_ref().is_none_or(|l| r > l));
        let placed = rank::between(low.as_deref(), high.as_deref());
        if placed.as_ref().is_ok_and(|r| r.len() <= RENUMBER_AT)
            || list.iter().any(|t| t.schema() > crate::TASK_SCHEMA)
        {
            return placed;
        }
        // Inserting at one place again and again lengthens ranks; renumber the
        // stage evenly, in its current order, and place the task among them.
        let fresh = rank::spread(list.len());
        for (mut task, rank) in list.into_iter().zip(&fresh) {
            if task.rank() != *rank {
                task.set_rank(rank)?;
                self.save(&mut task)?;
            }
        }
        let low = index.checked_sub(1).map(|i| fresh[i].as_str());
        rank::between(low, fresh.get(index).map(String::as_str))
    }

    /// How many tasks agents started since `since` (Unix milliseconds).
    pub fn agent_starts_since(&self, since: i64) -> Result<usize> {
        let (tasks, _) = self.tasks()?;
        let mut count = 0;
        for task in tasks {
            count += task.agent_starts_since(since)?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Actor, SessionKey, Target};
    use crate::task::NewTask;
    use crate::ErrorKind;

    fn new_task(store: &ProjectStore, rank: &str) -> TaskDoc {
        TaskDoc::create(NewTask {
            project: store.project_id(),
            id: &store.unused_short_id().unwrap(),
            text: "Add rate limiting",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank,
            now: 1,
        })
        .unwrap()
    }

    fn api() -> SessionKey {
        SessionKey {
            machine: "m".into(),
            session: "codex/api".into(),
            ..Default::default()
        }
    }

    fn comments(task: &TaskDoc) -> Vec<String> {
        task.view()
            .unwrap()
            .log
            .into_iter()
            .filter(|e| e.kind == "comment")
            .map(|e| e.text)
            .collect()
    }

    fn append(store: &ProjectStore, task: &TaskDoc, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(store.updates_path(task.uuid()))
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    /// #1519 R2 (review-win-c): the agent-start count inside an admission
    /// fails closed and never waits. It counts what it reads whole; a task
    /// file it cannot read as a bounded regular file (a FIFO, a link, one
    /// it may not read) or the deadline is an error, never a count of zero.
    /// zerus-ern6o (review-win-c on #1521): a project whose settings history
    /// outgrew the earlier 64 MiB read cap, written by `save_project` as any
    /// long-lived project's would be, is read again. No writer bounds a
    /// document's history, so the reader refuses no size.
    #[test]
    fn a_project_whose_history_outgrew_any_cap_is_read_again() {
        use crate::project::Settings;
        fn text(seed: &mut u64) -> String {
            const LETTERS: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut text = String::from("[Zerus task]\n");
            while text.len() < 16 * 1024 {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                text.push(LETTERS[(*seed as usize) % LETTERS.len()] as char);
            }
            text
        }
        let earlier_cap = 16 * crate::schema::MAX_DOCUMENT;
        let mut doc = ProjectDoc::new();
        let mut seed = 0x67c2_1ab7_d393_u64;
        for _ in 0..2250 {
            let settings = Settings {
                task_template: text(&mut seed),
                cancel_template: text(&mut seed),
                agents_other: true,
                agent_limit: 0,
            };
            doc.set_settings(&settings).unwrap();
        }
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "growth").unwrap();
        store.save_project(&doc).unwrap();
        let size = fs::metadata(store.dir().join("project.loro"))
            .unwrap()
            .len();
        assert!(size > earlier_cap as u64, "{size}");
        let read = store.project().unwrap();
        assert_eq!(read.settings(), doc.settings());
        // A regular file of any size is read whole, by every reader.
        let large = root.path().join("large.loro");
        let file = File::create(&large).unwrap();
        file.set_len(earlier_cap as u64 * 2 + 1).unwrap();
        assert_eq!(
            read_regular(&large).unwrap().len() as u64,
            earlier_cap as u64 * 2 + 1
        );
    }

    /// zerus-r75ru: a FIFO in place of any task file, or of the project
    /// file, never holds a read: that task does not load and is listed as
    /// damaged (kept for review), the others load, and a short ID is found.
    /// Each read runs under a watchdog, so a regression fails, not hangs.
    #[cfg(unix)]
    #[test]
    fn an_irregular_task_file_never_holds_a_read_of_every_task() {
        fn fifo(path: &Path) {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }
        fn soon<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> T {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(read());
            });
            rx.recv_timeout(Duration::from_secs(20))
                .expect("a read waited on an irregular file")
        }
        let root = tempfile::tempdir().unwrap();
        let open = {
            let root = root.path().to_owned();
            move || ProjectStore::open(&root, "example").unwrap()
        };
        let store = open();
        let mut good = new_task(&store, "U");
        store.save(&mut good).unwrap();
        let mut updated = new_task(&store, "V");
        store.save(&mut updated).unwrap();
        updated.comment("an update", &Actor::Person, 2).unwrap();
        store.save(&mut updated).unwrap();
        let mut marked = new_task(&store, "W");
        store.save(&mut marked).unwrap();
        let lone = "11111111-1111-4111-8111-111111111111";
        fifo(&store.snapshot_path(lone));
        let updates = store.updates_path(updated.uuid());
        fs::remove_file(&updates).unwrap();
        fifo(&updates);
        fifo(&store.managed_path(marked.uuid()));
        let (short, uuids) = (
            good.id().to_owned(),
            [lone, updated.uuid(), marked.uuid()].map(str::to_owned),
        );
        let project = store.dir().join("project.loro");
        // Each read opens its own store, as a command does.
        drop(store);
        let (loaded, damaged) = soon({
            let open = open.clone();
            move || {
                let (tasks, damaged) = open().tasks().unwrap();
                let ids: Vec<_> = tasks.iter().map(|t| t.uuid().to_owned()).collect();
                let damaged: BTreeSet<_> = damaged.into_iter().map(|d| d.uuid).collect();
                (ids, damaged)
            }
        });
        assert_eq!(loaded, [good.uuid().to_owned()]);
        assert_eq!(damaged, BTreeSet::from(uuids.clone()));
        // Each is kept for review: an irregular updates log or mark is
        // local data that cannot be read.
        let kept = soon({
            let (open, uuids) = (open.clone(), uuids.clone());
            move || uuids.map(|uuid| open().locally_kept(&uuid))
        });
        assert_eq!(kept[1], Some(Keep::UnreadableLocalData));
        assert_eq!(kept[2], Some(Keep::Marked));
        // The mark itself is not read through either.
        let mark = uuids[2].clone();
        assert!(soon({
            let open = open.clone();
            move || open().mark(&mark).is_none()
        }));
        // A short ID is found by reading every task.
        let found = soon({
            let open = open.clone();
            move || open().find(&short).map(|t| t.uuid().to_owned())
        });
        assert_eq!(found.unwrap(), good.uuid());
        // A FIFO as the project file is refused, not waited on.
        fs::remove_file(&project).ok();
        fifo(&project);
        assert!(soon(move || open().project().is_err()));
    }

    #[test]
    fn the_admitted_agent_count_fails_closed_and_never_waits() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let agent = Actor::Agent(SessionKey {
            machine: "m1".into(),
            session: "codex/api".into(),
            session_id: "launch_id:api".into(),
        });
        let mut task = new_task(&store, "V");
        task.record_send(
            &SessionKey {
                machine: "m1".into(),
                session: "codex/api".into(),
                session_id: "launch_id:api".into(),
            },
            "legacy",
            &agent,
            5,
        )
        .unwrap();
        store.save(&mut task).unwrap();
        let soon = Instant::now() + Duration::from_secs(5);
        let count = |store: &ProjectStore, deadline: Instant| {
            store
                .admit_until(Instant::now() + Duration::from_secs(5))
                .unwrap()
                .agent_starts_since(0, deadline)
        };
        assert_eq!(count(&store, soon).unwrap(), 1);
        assert!(count(&store, Instant::now()).is_err(), "past the deadline");
        // A FIFO named as another task's snapshot is refused, not waited on.
        let fifo = store.snapshot_path("11111111-1111-4111-8111-111111111111");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        #[cfg(unix)]
        {
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            let began = Instant::now();
            assert!(count(&store, soon).is_err());
            assert!(began.elapsed() < Duration::from_secs(2));
            fs::remove_file(&fifo).unwrap();
            // A link in a task's place is not read through.
            let snapshot = store.snapshot_path(task.uuid());
            let elsewhere = root.path().join("elsewhere.loro");
            fs::rename(&snapshot, &elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &snapshot).unwrap();
            assert!(count(&store, soon).is_err());
            fs::remove_file(&snapshot).unwrap();
            fs::rename(&elsewhere, &snapshot).unwrap();
        }
        let _ = path;
        assert_eq!(count(&store, soon).unwrap(), 1);
        // Updates whose framing is broken, or cut short, are not counted as
        // fewer starts: the count refuses (#1519 R3).
        task.comment("an update", &Actor::Person, 6).unwrap();
        store.save(&mut task).unwrap();
        let updates = store.updates_path(task.uuid());
        let whole = fs::read(&updates).unwrap();
        assert_eq!(count(&store, soon).unwrap(), 1);
        let mut broken = b"garbage".to_vec();
        broken.extend_from_slice(&whole);
        fs::write(&updates, &broken).unwrap();
        assert!(count(&store, soon).is_err());
        fs::write(&updates, &whole[..whole.len() - 1]).unwrap();
        assert!(count(&store, soon).is_err());
        fs::write(&updates, &whole).unwrap();
        assert_eq!(count(&store, soon).unwrap(), 1);
    }

    /// desktop-u's fifth control on #1519 R3: an agent's start recorded
    /// in a legacy task's updates, with one checksum bit flipped, is unknown
    /// evidence: the count refuses, never reads it as no start.
    #[test]
    fn a_torn_legacy_charge_is_unknown_not_zero() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let to = SessionKey {
            machine: "m1".into(),
            session: "codex/api".into(),
            session_id: "launch_id:api".into(),
        };
        let mut legacy = new_task(&store, "W");
        store.save(&mut legacy).unwrap();
        legacy
            .record_send(&to, "legacy", &Actor::Agent(to.clone()), 5)
            .unwrap();
        store.save(&mut legacy).unwrap();
        assert!(!store.managed_path(legacy.uuid()).exists());
        let soon = Instant::now() + Duration::from_secs(5);
        let count = |store: &ProjectStore| {
            store
                .admit_until(Instant::now() + Duration::from_secs(5))
                .unwrap()
                .agent_starts_since(0, soon)
        };
        assert_eq!(count(&store).unwrap(), 1);
        let updates = store.updates_path(legacy.uuid());
        let mut bytes = fs::read(&updates).unwrap();
        assert!(bytes.len() > 8);
        bytes[4] ^= 1;
        fs::write(&updates, bytes).unwrap();
        assert!(count(&store).is_err());
    }

    #[test]
    fn saves_append_updates_and_reload() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        task.comment("first", &Actor::Person, 2).unwrap();
        store.save(&mut task).unwrap();
        task.comment("second", &Actor::Person, 3).unwrap();
        store.save(&mut task).unwrap();
        let updates = fs::read(store.updates_path(task.uuid())).unwrap();
        let (found, intact) = blocks(&updates);
        assert_eq!((found.len(), intact), (2, true));
        let loaded = store.find(&task.id()).unwrap();
        assert_eq!(loaded.view().unwrap(), task.view().unwrap());
        assert_eq!(
            store.find(task.uuid()).unwrap().view().unwrap(),
            task.view().unwrap()
        );
    }

    #[test]
    fn a_cut_tail_is_repaired_and_later_saves_survive() {
        for junk in [&[200, 0, 0, 0, 1, 2][..], &[0; 64][..]] {
            let root = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(root.path(), "p1").unwrap();
            let mut task = new_task(&store, "V");
            store.save(&mut task).unwrap();
            task.comment("first", &Actor::Person, 2).unwrap();
            store.save(&mut task).unwrap();
            append(&store, &task, junk);
            // A later process: the cut tail is skipped, and the next save rewrites the snapshot.
            let mut again = store.load(task.uuid()).unwrap();
            assert!(again.needs_fold);
            again.comment("after the crash", &Actor::Person, 3).unwrap();
            store.save(&mut again).unwrap();
            assert!(!store.updates_path(task.uuid()).exists());
            again.comment("then appended", &Actor::Person, 4).unwrap();
            store.save(&mut again).unwrap();
            let last = store.load(task.uuid()).unwrap();
            assert_eq!(
                comments(&last),
                ["first", "after the crash", "then appended"]
            );
            assert!(!last.needs_fold);
        }
    }

    #[test]
    fn a_damaged_block_ends_the_updates() {
        // A damaged payload byte, and a damaged checksum over an intact payload.
        for damaged in [|len: usize| len - 1, |_| 4] {
            let root = tempfile::tempdir().unwrap();
            let store = ProjectStore::open(root.path(), "p1").unwrap();
            let mut task = new_task(&store, "V");
            store.save(&mut task).unwrap();
            task.comment("first", &Actor::Person, 2).unwrap();
            store.save(&mut task).unwrap();
            let path = store.updates_path(task.uuid());
            let mut bytes = fs::read(&path).unwrap();
            let at = damaged(bytes.len());
            bytes[at] ^= 0xff;
            fs::write(&path, bytes).unwrap();
            let loaded = store.load(task.uuid()).unwrap();
            assert!(loaded.needs_fold);
            assert!(comments(&loaded).is_empty());
        }
    }

    #[test]
    fn done_compacts_once() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        task.record_send(&api(), "r", &Actor::Person, 2).unwrap();
        store.save(&mut task).unwrap();
        task.record_report("Done.", &Actor::Person, 3).unwrap();
        task.set_stage(Stage::Done, &Actor::Person, 4).unwrap();
        store.save(&mut task).unwrap();
        assert!(!store.updates_path(task.uuid()).exists());
        // Comments on a Done task are appended again.
        task.comment("thanks", &Actor::Person, 5).unwrap();
        store.save(&mut task).unwrap();
        assert!(store.updates_path(task.uuid()).exists());
        assert_eq!(
            store.load(task.uuid()).unwrap().view().unwrap(),
            task.view().unwrap()
        );
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_projects_are_separate() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        let bad = ids::new_uuid();
        fs::write(store.snapshot_path(&bad), b"not loro").unwrap();
        let (tasks, damaged) = store.tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(damaged.len(), 1);
        assert_eq!(damaged[0].uuid, bad);
        assert!(store.snapshot_path(&bad).exists());
        let other = ProjectStore::open(root.path(), "p2").unwrap();
        assert!(other.tasks().unwrap().0.is_empty());
        assert!(store.find("T-zzzzzz").is_err());
        assert!(store.find("nonsense").is_err());
    }

    #[test]
    fn a_damaged_task_is_named_as_damaged() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        let bad = ids::new_uuid();
        fs::write(store.snapshot_path(&bad), b"not loro").unwrap();
        let by_uuid = store.find(&bad).err().unwrap();
        assert!(by_uuid.to_string().contains("is damaged"), "{by_uuid}");
        assert_eq!(by_uuid.kind(), ErrorKind::Failed);
        let by_id = store.find("T-zzzzzz").err().unwrap().to_string();
        assert!(by_id.contains("1 task file could not be read"), "{by_id}");
        let missing = store.find(&ids::new_uuid()).err().unwrap().to_string();
        assert!(missing.starts_with("no task"), "{missing}");
        let refused = task.set_stage(Stage::Done, &Actor::Person, 2).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Refused);
    }

    #[test]
    fn leftover_temporary_files_are_swept_on_open() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        let strays = [
            store
                .dir
                .join("tasks")
                .join(format!("{}.tmp-{}", task.uuid(), ids::new_uuid())),
            store.dir.join(format!("project.tmp-{}", ids::new_uuid())),
        ];
        for stray in &strays {
            fs::write(stray, b"half written").unwrap();
        }
        drop(store);
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        assert!(strays.iter().all(|s| !s.exists()));
        assert!(store.load(task.uuid()).is_ok());
    }

    #[test]
    fn a_task_that_cannot_be_shown_is_set_aside() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut good = new_task(&store, "V");
        store.save(&mut good).unwrap();
        let mut bad = new_task(&store, "W");
        bad.set_raw_field("target", "{\"kind\":\"team\"}");
        store.save(&mut bad).unwrap();
        let (tasks, damaged) = store.tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(damaged.len(), 1);
        assert_eq!(damaged[0].uuid, bad.uuid());
        // Listing a stage and counting agent starts still work.
        assert_eq!(store.ordered(Stage::Todo).unwrap().len(), 1);
        assert_eq!(store.agent_starts_since(0).unwrap(), 0);
    }

    #[test]
    fn ranks_place_tasks_even_between_equal_ranks() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        assert_eq!(
            store.rank_for(Stage::Todo, &Position::End, None).unwrap(),
            "V"
        );
        for rank in ["V", "V", "X"] {
            let mut task = new_task(&store, rank);
            store.save(&mut task).unwrap();
        }
        let list = store.ordered(Stage::Todo).unwrap();
        let last = list[2].id();
        let between = store
            .rank_for(Stage::Todo, &Position::Index(1), None)
            .unwrap();
        assert!(
            "V" < between.as_str() && between.as_str() < "X",
            "{between}"
        );
        let before = store
            .rank_for(Stage::Todo, &Position::Before(last.clone()), None)
            .unwrap();
        assert!("V" < before.as_str() && before.as_str() < "X", "{before}");
        assert!(
            store
                .rank_for(Stage::Todo, &Position::Top, None)
                .unwrap()
                .as_str()
                < "V"
        );
        assert!(
            store
                .rank_for(Stage::Todo, &Position::End, None)
                .unwrap()
                .as_str()
                > "X"
        );
        let after_last = store
            .rank_for(
                Stage::Todo,
                &Position::After(last.clone()),
                Some(list[2].uuid()),
            )
            .is_err();
        assert!(after_last, "the moving task is not its own anchor");
        assert!(store
            .rank_for(Stage::Backlog, &Position::Before(last), None)
            .is_err());
    }

    #[test]
    fn a_listing_loads_every_task_once_in_rank_order() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut ids = Vec::new();
        for rank in ["X", "V", "W"] {
            let mut task = new_task(&store, rank);
            store.save(&mut task).unwrap();
            ids.push(task.id());
        }
        fs::write(store.snapshot_path(&ids::new_uuid()), b"not loro").unwrap();
        let (tasks, damaged) = store.listing().unwrap();
        let order: Vec<String> = tasks.iter().map(TaskDoc::id).collect();
        assert_eq!(order, [ids[1].clone(), ids[2].clone(), ids[0].clone()]);
        assert_eq!(damaged.len(), 1);
    }

    #[test]
    fn long_ranks_are_renumbered_in_order() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        // Ranks that many moves to the top leave behind.
        let long = |last: char| format!("{}{last}", "0".repeat(40));
        for rank in [long('1'), long('2'), "V".to_owned()] {
            let mut task = new_task(&store, &rank);
            store.save(&mut task).unwrap();
        }
        let before: Vec<String> = store
            .ordered(Stage::Todo)
            .unwrap()
            .iter()
            .map(TaskDoc::id)
            .collect();
        let top = store.rank_for(Stage::Todo, &Position::Top, None).unwrap();
        let after = store.ordered(Stage::Todo).unwrap();
        assert_eq!(after.iter().map(TaskDoc::id).collect::<Vec<_>>(), before);
        assert!(after.iter().all(|t| t.rank().len() <= 5), "renumbered");
        assert!(top.len() <= 5 && top < after[0].rank(), "{top}");
    }

    #[test]
    fn agent_starts_are_counted() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut by_agent = new_task(&store, "V");
        by_agent
            .record_send(&api(), "r1", &Actor::Agent(api()), 1_000)
            .unwrap();
        store.save(&mut by_agent).unwrap();
        let mut by_person = new_task(&store, "W");
        by_person
            .record_send(&api(), "r2", &Actor::Person, 1_000)
            .unwrap();
        store.save(&mut by_person).unwrap();
        assert_eq!(store.agent_starts_since(500).unwrap(), 1);
        assert_eq!(store.agent_starts_since(2_000).unwrap(), 0);
    }

    /// The lock is released when the store drops, at once. No test of this
    /// binary starts a child process: a child forked between this store's
    /// open and its drop holds a copy of the lock's descriptor until it
    /// executes, and the lock with it (zerus-kxpjt).
    #[test]
    fn the_project_lock_is_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let other = File::open(store.dir.join(".lock")).unwrap();
        assert!(other.try_lock_exclusive().is_err());
        drop(store);
        assert!(other.try_lock_exclusive().is_ok());
    }

    /// The task's snapshot, updates and mark, byte for byte (None when absent).
    fn files_of(store: &ProjectStore, uuid: &str) -> Vec<Option<Vec<u8>>> {
        [
            store.snapshot_path(uuid),
            store.updates_path(uuid),
            store.managed_path(uuid),
        ]
        .iter()
        .map(|path| fs::read(path).ok())
        .collect()
    }

    /// zerus-cg5h.4.4: a shared-store caller waits for the admission no
    /// later than its deadline; refused, it has read and written nothing,
    /// and the holder's work and later callers are unaffected.
    #[test]
    fn a_timed_admission_is_refused_at_its_deadline_and_writes_nothing() {
        use std::sync::mpsc::channel;
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        let before = files_of(&store, &uuid);
        let (store, uuid) = (&store, uuid.as_str());
        let (held, holding) = channel();
        let (release, released) = channel::<()>();
        let (refusal, waited) = std::thread::scope(|scope| {
            let holder = scope.spawn(move || {
                let mut admitted = store
                    .admit_until(Instant::now() + Duration::from_secs(30))
                    .unwrap();
                held.send(()).unwrap();
                released.recv().unwrap();
                // The holder's own save inside its admission still lands.
                let mut mine = admitted.load(uuid).unwrap();
                mine.comment("by the holder", &Actor::Person, 3).unwrap();
                admitted.save(&mut mine).unwrap();
            });
            holding.recv().unwrap();
            let started = Instant::now();
            let refusal = store
                .admit_until(started + Duration::from_millis(150))
                .map(|_| ());
            let waited = started.elapsed();
            let unchanged = files_of(store, uuid) == before;
            release.send(()).unwrap();
            holder.join().unwrap();
            assert!(unchanged, "a refused admission changed the task's files");
            (refusal, waited)
        });
        // An exhausted deadline is refused even when nothing holds the store.
        assert!(
            store.admit_until(Instant::now()).is_none(),
            "an exhausted deadline entered"
        );
        assert!(
            refusal.is_none(),
            "the admission entered while another held it"
        );
        assert!(
            waited >= Duration::from_millis(150),
            "refused early: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(5),
            "waited past its deadline: {waited:?}"
        );
        assert_eq!(
            comments(&store.load(uuid).unwrap()),
            vec!["by the holder".to_owned()]
        );
        // Free, it enters within its deadline, and its load and save are one turn.
        let mut admitted = store
            .admit_until(Instant::now() + Duration::from_secs(5))
            .unwrap();
        let mut task = admitted.load(uuid).unwrap();
        task.comment("timed", &Actor::Person, 4).unwrap();
        admitted.save(&mut task).unwrap();
        drop(admitted);
        assert_eq!(
            comments(&store.load(uuid).unwrap()),
            vec!["by the holder".to_owned(), "timed".to_owned()]
        );
    }

    /// A timed caller that waits until its turn enters as soon as the holder
    /// leaves, and an untimed load or save waiting beside it is never left
    /// waiting after a timed caller gave up. A panic inside releases it.
    #[test]
    fn the_admission_passes_on_in_turn_and_survives_a_panic() {
        use std::sync::mpsc::channel;
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        let (store, uuid) = (&store, uuid.as_str());
        let (held, holding) = channel();
        let (release, released) = channel::<()>();
        let (done, finished) = channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let _admitted = store
                    .admit_until(Instant::now() + Duration::from_secs(30))
                    .unwrap();
                held.send(()).unwrap();
                released.recv().unwrap();
            });
            holding.recv().unwrap();
            // An untimed load waits behind the holder ...
            scope.spawn(move || {
                let loaded = store.load(uuid).is_ok();
                done.send(loaded).unwrap();
            });
            // ... while a timed caller gives up first,
            let gave_up = store
                .admit_until(Instant::now() + Duration::from_millis(50))
                .is_none();
            // and a timed caller with time left enters once the holder leaves.
            let waiting = scope.spawn(move || {
                store
                    .admit_until(Instant::now() + Duration::from_secs(10))
                    .map(|mut admitted| admitted.load(uuid).is_ok())
            });
            std::thread::sleep(Duration::from_millis(50));
            // Released before anything is asserted, so a failure never
            // leaves the holder waiting.
            release.send(()).unwrap();
            let entered = waiting.join().unwrap();
            let loaded = finished.recv_timeout(Duration::from_secs(10));
            assert!(
                gave_up,
                "a timed caller entered while another held the store"
            );
            assert_eq!(entered, Some(true));
            assert_eq!(loaded, Ok(true));
        });
        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _admitted = store
                        .admit_until(Instant::now() + Duration::from_secs(5))
                        .unwrap();
                    panic!("inside the admission");
                })
                .join()
        });
        assert!(panicked.is_err());
        assert!(store
            .admit_until(Instant::now() + Duration::from_secs(1))
            .is_some());
        let mut task = store.load(uuid).unwrap();
        task.comment("after the panic", &Actor::Person, 5).unwrap();
        store.save(&mut task).unwrap();
    }

    /// desktop-u on #1446: one admission holds several reads, a recheck and
    /// more than one save of a cut-over task in turn, and every save lands.
    #[test]
    fn one_admission_reads_rechecks_and_saves_in_turn() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(root.path(), "p1").unwrap();
        let mut task = new_task(&store, "V");
        assert!(task.floor().unwrap());
        store.save(&mut task).unwrap();
        let uuid = task.uuid().to_owned();
        let mut admitted = store
            .admit_until(Instant::now() + Duration::from_secs(5))
            .unwrap();
        let first = admitted.load(&uuid).unwrap();
        let recheck = admitted.load(&uuid).unwrap();
        assert!(first.floor_record().is_some());
        assert_eq!(first.version(), recheck.version());
        let mut a = TaskDoc::from_bytes(&first.snapshot().unwrap()).unwrap();
        a.comment("first in the turn", &Actor::Person, 2).unwrap();
        admitted.save(&mut a).unwrap();
        // A copy forked before that save is refused, never written over it.
        let mut stale = TaskDoc::from_bytes(&recheck.snapshot().unwrap()).unwrap();
        stale
            .comment("from a stale copy", &Actor::Person, 3)
            .unwrap();
        assert_eq!(
            admitted.save(&mut stale).unwrap_err().kind(),
            ErrorKind::Refused
        );
        // Read again inside the same turn, it saves.
        let mut b = admitted.load(&uuid).unwrap();
        b.comment("second in the turn", &Actor::Person, 4).unwrap();
        admitted.save(&mut b).unwrap();
        let after = admitted.load(&uuid).unwrap();
        drop(admitted);
        let expected = vec![
            "first in the turn".to_owned(),
            "second in the turn".to_owned(),
        ];
        assert_eq!(comments(&after), expected);
        let reloaded = store.load(&uuid).unwrap();
        assert_eq!(comments(&reloaded), expected);
        assert!(reloaded.floor_record().is_some());
        assert!(
            store.managed_path(&uuid).exists(),
            "the cut-over task keeps its mark"
        );
    }

    #[test]
    fn the_change_journal_names_saved_tasks_and_otherwise_says_read_all() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        // No journal: it is started, and this read cannot say.
        let started = store.changes_since("", 0).unwrap();
        assert!(started.documents.is_none() && !started.generation.is_empty());
        let place = |c: &Changes| {
            store
                .changes_since(&c.generation, c.offset)
                .unwrap()
                .documents
        };
        assert_eq!(place(&started), Some(BTreeSet::new()));
        let mut a = new_task(&store, "U");
        store.save(&mut a).unwrap();
        // A reader that does not know this generation cannot tell.
        let first = store.changes_since("", 0).unwrap();
        assert!(first.documents.is_none() && !first.generation.is_empty());
        let place = |c: &Changes| {
            store
                .changes_since(&c.generation, c.offset)
                .unwrap()
                .documents
        };
        assert_eq!(place(&first), Some(BTreeSet::new()));
        let mut b = new_task(&store, "V");
        store.save(&mut b).unwrap();
        a.comment("note", &Actor::Person, 2).unwrap();
        store.save(&mut a).unwrap();
        let next = store
            .changes_since(&first.generation, first.offset)
            .unwrap();
        let both = BTreeSet::from([a.uuid().to_owned(), b.uuid().to_owned()]);
        assert_eq!(next.documents, Some(both));
        // A save that writes nothing notes nothing.
        store.save(&mut a).unwrap();
        assert_eq!(place(&next), Some(BTreeSet::new()));
        // Another generation or a place past the end means read all.
        let other = Changes {
            generation: ids::new_uuid(),
            ..next.clone()
        };
        assert!(place(&other).is_none());
        let beyond = Changes {
            offset: next.offset + 4096,
            ..next.clone()
        };
        assert!(place(&beyond).is_none());
        // An unexpected line means read all, once: from the place returned
        // then, later notes read normally.
        let path = store.dir().join(CHANGES);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("not-a-document\n");
        fs::write(&path, &text).unwrap();
        let damaged = store.changes_since(&next.generation, next.offset).unwrap();
        assert!(damaged.documents.is_none());
        store.save_project(&ProjectDoc::new()).unwrap();
        let fresh = place(&damaged);
        assert_eq!(fresh, Some(BTreeSet::from(["project".to_owned()])));
        // A removed journal means read all.
        fs::remove_file(&path).unwrap();
        assert!(place(&damaged).is_none());
    }

    /// A growing journal keeps its generation and drops its oldest lines: a
    /// reader that keeps up never misses a change; one left behind reads all.
    #[test]
    fn a_full_journal_drops_its_oldest_lines_under_the_same_generation() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        store.save_project(&ProjectDoc::new()).unwrap();
        let start = store.changes_since("", 0).unwrap();
        let mut behind = start.clone();
        let mut keeping_up = start.clone();
        let mut task = new_task(&store, "U");
        store.save(&mut task).unwrap();
        for n in 0..3000 {
            task.comment("note", &Actor::Person, 10 + n).unwrap();
            store.save(&mut task).unwrap();
            if n % 500 == 0 {
                let read = store
                    .changes_since(&keeping_up.generation, keeping_up.offset)
                    .unwrap();
                assert_eq!(
                    read.documents,
                    Some(BTreeSet::from([task.uuid().to_owned()])),
                    "{n}"
                );
                keeping_up = read;
            }
        }
        let read = store
            .changes_since(&keeping_up.generation, keeping_up.offset)
            .unwrap();
        assert_eq!(read.generation, start.generation, "the generation is kept");
        assert!(read.documents.is_some());
        assert!(fs::metadata(store.dir().join(CHANGES)).unwrap().len() <= CHANGES_READ);
        behind = store
            .changes_since(&behind.generation, behind.offset)
            .unwrap();
        assert!(behind.documents.is_none(), "its place was dropped");
    }

    /// review-n on #1350: the hint never writes through a link, never reads
    /// more than its budget and never waits on a special file.
    #[test]
    fn an_unsafe_or_oversized_hint_is_unknown_and_never_written_through() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        store.save_project(&ProjectDoc::new()).unwrap();
        let path = store.dir().join(CHANGES);
        let project = store.dir().join("project.loro");
        let before = fs::read(&project).unwrap();
        // A hard link to the project document: a save replaces the name.
        fs::remove_file(&path).unwrap();
        fs::hard_link(&project, &path).unwrap();
        assert!(store.changes_since("", 0).unwrap().documents.is_none());
        let mut task = new_task(&store, "U");
        store.save(&mut task).unwrap();
        assert_eq!(
            fs::read(&project).unwrap(),
            before,
            "the project is untouched"
        );
        store.project().unwrap();
        let known = store.changes_since("", 0).unwrap();
        assert!(!known.generation.is_empty());
        // A symbolic link: likewise, and what it names is never touched.
        #[cfg(unix)]
        {
            let outside = temp.path().join("outside");
            fs::write(&outside, b"kept").unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            assert!(store
                .changes_since(&known.generation, known.offset)
                .unwrap()
                .documents
                .is_none());
            task.comment("note", &Actor::Person, 2).unwrap();
            store.save(&mut task).unwrap();
            assert_eq!(fs::read(&outside).unwrap(), b"kept");
            assert!(!fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        // Oversized: unknown, not read and trusted.
        let line = format!("{}\n", task.uuid());
        fs::write(
            &path,
            format!("gen {} 0\n{}", known.generation, line.repeat(30_000)),
        )
        .unwrap();
        let read = store.changes_since(&known.generation, 0).unwrap();
        assert!(read.documents.is_none() && read.generation != known.generation);
        // ... and it was started again, small.
        assert!(fs::metadata(&path).unwrap().len() < 128);
        assert_eq!(
            store
                .changes_since(&read.generation, read.offset)
                .unwrap()
                .documents,
            Some(BTreeSet::new())
        );
        // A special file is never opened to wait on. It is made in this
        // process, never by a child: a child forked beside another test holds
        // that test's open descriptors until it executes, and so its project
        // lock (the_project_lock_is_exclusive, zerus-kxpjt).
        #[cfg(unix)]
        {
            fs::remove_file(&path).unwrap();
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(store
                .changes_since(&known.generation, known.offset)
                .unwrap()
                .documents
                .is_none());
            task.comment("again", &Actor::Person, 3).unwrap();
            store.save(&mut task).unwrap();
            assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());
        }
    }

    /// review-n on #1350 R2: a damaged base never panics a read or a save.
    #[test]
    fn a_journal_base_without_room_is_unknown_and_never_panics() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        let path = store.dir().join(CHANGES);
        let generation = ids::new_uuid();
        // At the very end: reading it must not overflow.
        fs::write(&path, format!("gen {generation} {}\nproject\n", u64::MAX)).unwrap();
        let read = store.changes_since(&generation, u64::MAX).unwrap();
        assert!(read.documents.is_none());
        // Near the end with a full body: a save stores the task and notes it.
        let near = u64::MAX - 10;
        let full = "project\n".repeat(CHANGES_LIMIT / 8);
        fs::write(&path, format!("gen {generation} {near}\n{full}")).unwrap();
        let mut task = new_task(&store, "U");
        store.save(&mut task).unwrap();
        store.load(task.uuid()).unwrap();
        let known = store.changes_since("", 0).unwrap();
        assert!(!known.generation.is_empty());
        task.comment("note", &Actor::Person, 2).unwrap();
        store.save(&mut task).unwrap();
        assert_eq!(
            store
                .changes_since(&known.generation, known.offset)
                .unwrap()
                .documents,
            Some(BTreeSet::from([task.uuid().to_owned()]))
        );
    }

    /// One store shared by threads: no note is lost to a concurrent
    /// read-modify-write of the journal.
    #[test]
    fn notes_from_threads_sharing_a_store_are_all_kept() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(temp.path(), "example").unwrap();
        let start = store.changes_since("", 0).unwrap();
        let mut tasks: Vec<TaskDoc> = (0..64).map(|_| new_task(&store, "U")).collect();
        let uuids: BTreeSet<String> = tasks.iter().map(|t| t.uuid().to_owned()).collect();
        std::thread::scope(|scope| {
            for chunk in tasks.chunks_mut(8) {
                let store = &store;
                scope.spawn(move || {
                    for task in chunk {
                        store.save(task).unwrap();
                    }
                });
            }
        });
        let read = store
            .changes_since(&start.generation, start.offset)
            .unwrap();
        assert_eq!(read.documents, Some(uuids));
    }

    /// review-n on #1432: two first hellos at once agree on one store
    /// identity, and a creator under the lock is never replaced by another.
    /// zerus-iih7g: an identity that cannot be known is refused and kept,
    /// never replaced; only an empty file (the first writer's create before
    /// its one write) is created as if missing.
    #[test]
    fn concurrent_first_hellos_agree_on_one_store_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("replica");
        let (ready, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (one, two) = std::thread::scope(|scope| {
            let root_one = root.clone();
            let one = scope.spawn(move || {
                STORE_ID_CREATING.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        ready.send(()).unwrap();
                        released.recv().unwrap();
                    }))
                });
                ProjectStore::store_id(&root_one).unwrap()
            });
            waiting.recv().unwrap();
            // The first creator holds the lock: the second waits, then reads it.
            let root_two = root.clone();
            let two = scope.spawn(move || ProjectStore::store_id(&root_two).unwrap());
            std::thread::sleep(Duration::from_millis(100));
            let waited = !two.is_finished();
            release.send(()).unwrap();
            let ids = (one.join().unwrap(), two.join().unwrap());
            assert!(waited, "the second creator did not wait");
            ids
        });
        assert_eq!(one, two);
        assert_eq!(ProjectStore::store_id(&root).unwrap(), one);
        assert!(ids::is_uuid(&one));
        // An empty identity file is created once, and then kept.
        let path = root.join("store-id");
        fs::write(&path, b"").unwrap();
        let created = ProjectStore::store_id(&root).unwrap();
        assert!(ids::is_uuid(&created) && created != one);
        assert_eq!(ProjectStore::store_id(&root).unwrap(), created);
        // Any other identity that is not canonical is refused, untouched,
        // by the untimed and the timed read alike.
        let soon = Instant::now() + Duration::from_secs(5);
        let refused = |bytes: &[u8], what: &str| {
            fs::write(&path, bytes).unwrap();
            let error = ProjectStore::store_id(&root).unwrap_err().to_string();
            assert!(error.contains("is kept"), "{what}: {error}");
            assert!(ProjectStore::store_id_until(&root, soon).is_err(), "{what}");
            assert_eq!(fs::read(&path).unwrap(), bytes, "{what}");
        };
        refused(b"not an identity", "malformed");
        refused(&created.as_bytes()[..20], "cut short");
        refused(&[0xff, 0xfe], "not text");
        refused(
            &format!("{created}{}", " ".repeat(2000)).into_bytes(),
            "past the bound",
        );
        fs::write(&path, format!("  {created}\r\n")).unwrap();
        assert_eq!(ProjectStore::store_id(&root).unwrap(), created);
    }

    /// zerus-iih7g: a FIFO or a link, even a dangling one, at the identity's
    /// place is refused without waiting and is never replaced.
    #[cfg(unix)]
    #[test]
    fn an_irregular_store_identity_is_refused_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("replica");
        let path = root.join("store-id");
        let id = ProjectStore::store_id(&root).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::write(&elsewhere, format!("{id}\n")).unwrap();
        let check = |what: &str| {
            let root = root.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(ProjectStore::store_id(&root).map_err(|e| e.to_string()));
            });
            let read = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("waited on an irregular identity");
            assert!(read.unwrap_err().contains("is kept"), "{what}");
            assert!(
                !fs::symlink_metadata(&path).unwrap().is_file(),
                "{what} was replaced"
            );
        };
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        check("a link to a valid identity");
        fs::remove_file(&elsewhere).unwrap();
        check("a dangling link");
        fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        check("a FIFO");
    }

    /// The cutover status counts marks within a strict budget, and past it
    /// says nothing rather than a partial count.
    #[test]
    fn the_cut_over_count_stays_within_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(dir.path(), "p").unwrap();
        for n in 0..3 {
            let mut task = TaskDoc::create(NewTask {
                project: "p",
                id: &format!("T-00000{n}"),
                text: "Floored",
                stage: Stage::Todo,
                target: Target::Any,
                author: Actor::Person,
                rank: "V",
                now: 1,
            })
            .unwrap();
            assert!(task.floor().unwrap());
            store.save(&mut task).unwrap();
        }
        let long = Duration::from_secs(60);
        let (count, latest) = store.cut_over_within(1_000, long).unwrap().unwrap();
        assert_eq!(count, 3);
        assert!(latest.is_some());
        assert_eq!(store.cut_over_within(2, long).unwrap(), None);
        assert_eq!(store.cut_over_within(1_000, Duration::ZERO).unwrap(), None);
        // An empty folder still answers within its time, or not at all.
        let empty = tempfile::tempdir().unwrap();
        let none = ProjectStore::open(empty.path(), "q").unwrap();
        assert_eq!(none.cut_over_within(1_000, long).unwrap(), Some((0, None)));
        assert_eq!(none.cut_over_within(1_000, Duration::ZERO).unwrap(), None);
    }
}
