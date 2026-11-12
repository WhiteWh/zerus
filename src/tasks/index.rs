//! A disposable local index of one project's task headers (zerus-cg5h.2): what
//! paged task lists and per-task lookups read instead of decoding every task
//! document. The Loro files stay authoritative and every write still goes to
//! them. The index is never replicated; a missing, damaged or foreign one is
//! rebuilt silently from the documents, and a read never fails because of it:
//! callers read the documents instead.
//!
//! It is opened and written only while the project's store lock is held, its
//! connection never outlives that store, and callers never hold two at once,
//! so nothing else has it open and a damaged one can be removed and rebuilt;
//! WAL and a busy timeout stay as a second guard. It learns what changed from the store's change
//! journal, checks every task file's stamp (length, modification time, inode)
//! at least every 30 seconds for writers that predate the journal, and checks
//! the files of a page's rows before it serves them.
use super::*;
use crate::platform;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Duration;
use zerus_replica::{ids, Inventory, TaskFile};

/// v2 (ROWS 5): rows carry their owner session for session-filtered pages
/// (zerus-cg5h.26). A new file, not a new version of v1: an older hgs keeps
/// rebuilding any index not of its own schema, so sharing one file would make
/// an old and a new binary rebuild it in turn. Each keeps its own; this code
/// never opens, migrates or removes task-index-v1.sqlite3.
const FILE: &str = "task-index-v2.sqlite3";
/// 2: the histories of recently read tasks as well (slice B). 3: rows and
/// history headers carry the task's execution (zerus-cg5h.4), so rows cached
/// without it are built again rather than mixed with rows that have it.
/// 4: history entries naming an attempt carry its origin and phase (A4).
/// 5: rows carry their owner session and whether they are unassigned, and
/// each loaded task's inventory version and whether it is Done; the meta
/// holds the project document's version (zerus-cg5h.26, zerus-cg5h.21).
const ROWS: i64 = 5;
/// The index's version: its rows' shape and the replica's mark format, since
/// a row remembers whether its task loaded, which the mark decides. An index
/// made under another mark rule (before the seal, for one) is built again.
const SCHEMA: i64 = ROWS * 1000 + zerus_replica::MARK_FORMAT;
/// The rule rows are read under, kept in meta: "repaired-1" reads a task as
/// the store's inventory does, repairing a lowered execution schema first
/// (zerus-cg5h.21). Rows of an index without it are read again once; the
/// row shape is unchanged, so this needs no other index file.
const DERIVED: &str = "repaired-1";
/// The histories kept: the most recently read tasks.
const HISTORIES: i64 = 16;
/// The one-task history cache this index replaces.
const OLD_HISTORY: &str = "task-history-v1.sqlite3";
/// How often every task file's stamp is checked, for writers that do not
/// append to the change journal.
const FULL_CHECK_MS: i64 = 30_000;
/// Tasks decoded per transaction, so no transaction is long.
const CHUNK: usize = 256;
/// A row whose document was read longer ago than this is suspect.
const MAX_AGE_MS: i64 = 10 * 60_000;
/// A file modified more recently than this is not settled: a later write in
/// the same clock tick could keep its stamp, so the row is read again.
const SETTLE_MS: i64 = 2_000;
/// Suspect rows read again at each read of the index, oldest first.
const BATCH: i64 = 256;
/// The longest damage text kept for a task that does not load.
const ERROR_BYTES: usize = 512;
/// The longest prompt text a paged row carries; `get` returns all of it.
pub(super) const TEXT_BYTES: usize = 2048;
/// The largest page a list request may ask for.
pub(super) const MAX_LIMIT: usize = 200;
/// The damaged tasks a page names; `damaged_total` counts them all.
pub(super) const DAMAGED_SHOWN: usize = 20;
/// The most the index may hold, whatever its page size: a full or larger
/// one is disposed of and built again, never grown.
const BUDGET_BYTES: i64 = 256 * 1024 * 1024;

/// The pages `BUDGET_BYTES` allows at `page_size`, and whether `pages` of
/// them exceed it.
fn budget(page_size: i64, pages: i64) -> Option<i64> {
    let allowed = BUDGET_BYTES.checked_div(page_size).filter(|n| *n > 0)?;
    (pages <= allowed).then_some(allowed)
}

/// Why the index cannot answer: a damaged index is removed and rebuilt once;
/// anything else leaves the caller to read the documents.
#[derive(Debug)]
pub(super) enum Failure {
    Damaged,
    Other,
}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Other,
            // A cache: any other SQLite failure, including a missing table
            // or a full file, is answered by building it again.
            _ => Self::Damaged,
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Other
    }
}

impl From<zerus_replica::Error> for Failure {
    fn from(_: zerus_replica::Error) -> Self {
        Self::Other
    }
}

type Outcome<T> = std::result::Result<T, Failure>;

/// A task's stored files' stamps, row, error and what a phone holds of it.
type Stored = (String, Option<String>, Option<String>, Option<Held>);

#[cfg(test)]
thread_local! {
    /// Task documents decoded and task files stamped, for the bounded-work tests.
    pub(super) static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static STAMPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Stored rows read to compare with a task read again.
    pub(super) static STORED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Task histories read from their documents into the index.
    pub(super) static MATERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Reads rows as the build before the repair rule did, without it.
    pub(super) static LEGACY_DERIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Reads rows as the #1736 and #1748 builds did: with the repair and
    /// their own "derived_*" marks, but without the repair epoch.
    pub(super) static PROOF_WRITER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The time the index reads, held by a test that must not depend on how
    /// long this host takes to build an index (zerus-iezgx).
    pub(super) static CLOCK: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// The wall clock in milliseconds, or the time a test holds.
fn now() -> i64 {
    #[cfg(test)]
    if let Some(at) = CLOCK.with(std::cell::Cell::get) {
        return at;
    }
    super::now()
}

/// A file's length, modification time and inode, or `-` when it is missing,
/// and whether it is settled: modified at least `SETTLE_MS` before `now`.
fn stamp_at(path: &Path, now: i64) -> (String, bool) {
    entry_stamp(fs::metadata(path), now)
}

/// A stamp of the entry itself, not what it links to: a dangling mark
/// changes it too.
fn link_stamp_at(path: &Path, now: i64) -> (String, bool) {
    entry_stamp(fs::symlink_metadata(path), now)
}

/// A file's stamp when it is not there.
const MISSING: &str = "-";

fn entry_stamp(meta: std::io::Result<fs::Metadata>, now: i64) -> (String, bool) {
    #[cfg(test)]
    STAMPED.with(|n| n.set(n.get() + 1));
    let Ok(meta) = meta else {
        return (MISSING.into(), true);
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    let millis = (modified / 1_000_000) as i64;
    (format!("{}:{modified}:{inode}", meta.len()), millis <= now - SETTLE_MS)
}

fn stamp(path: &Path) -> String {
    stamp_at(path, 0).0
}

/// The stamps of a task's snapshot, appended updates and mark, and whether
/// all are settled: a change to the mark alone is a change of the task.
fn stamped(dir: &Path, uuid: &str, now: i64) -> (String, bool) {
    let tasks = dir.join("tasks");
    let (snapshot, a) = stamp_at(&tasks.join(format!("{uuid}.loro")), now);
    let (updates, b) = stamp_at(&tasks.join(format!("{uuid}.updates")), now);
    let (mark, c) = link_stamp_at(&tasks.join(format!("{uuid}.managed")), now);
    (format!("{snapshot}|{updates}|{mark}"), a && b && c)
}

fn files(dir: &Path, uuid: &str) -> String {
    stamped(dir, uuid, 0).0
}

/// `stamped`'s stamps of every task in the task folder, as one listing of
/// the folder found its files (zerus-cg5h.30): a file it did not list is
/// missing without a lookup. A file written after the listing is found by a
/// later check.
fn listed(dir: &Path) -> Outcome<BTreeMap<String, String>> {
    let mut parts: BTreeMap<String, [String; 3]> = BTreeMap::new();
    for entry in fs::read_dir(dir.join("tasks"))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some((uuid, file)) = zerus_replica::task_file(&name) else {
            continue;
        };
        let (stamp, _) = entry_stamp(listed_metadata(&entry, file), 0);
        let slot = match file {
            TaskFile::Snapshot => 0,
            TaskFile::Updates => 1,
            TaskFile::Mark => 2,
        };
        parts.entry(uuid.to_owned()).or_insert_with(|| [MISSING.into(), MISSING.into(), MISSING.into()])[slot] = stamp;
    }
    Ok(parts.into_iter().map(|(uuid, [snapshot, updates, mark])| (uuid, format!("{snapshot}|{updates}|{mark}"))).collect())
}

/// A listed task file's metadata as `stamped` reads it: the mark's own
/// entry, and what the snapshot and updates link to. On Unix a file that is
/// no link is read through the folder's handle, without a path lookup;
/// elsewhere the listing's own metadata may lag a write, so it is not used.
fn listed_metadata(entry: &fs::DirEntry, file: TaskFile) -> std::io::Result<fs::Metadata> {
    #[cfg(unix)]
    if file == TaskFile::Mark || entry.file_type().is_ok_and(|t| !t.is_symlink()) {
        return entry.metadata();
    }
    match file {
        TaskFile::Mark => fs::symlink_metadata(entry.path()),
        _ => fs::metadata(entry.path()),
    }
}

/// What a full check compares each task with: the stamps its stored row
/// holds and those the folder listing found.
struct Known {
    stamps: HashMap<String, String>,
    listed: BTreeMap<String, String>,
}

/// Whether stamps from `stamped` say every file of the task is gone.
fn gone(stamps: &str) -> bool {
    stamps.split('|').all(|part| part == MISSING)
}

/// `text` cut to at most `bytes` at a character boundary.
fn cut(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A damaged task's error text as the index keeps it.
pub(super) fn capped_error(error: &str) -> String {
    cut(error, ERROR_BYTES).to_owned()
}

/// A damaged task as a page names it: its error capped, and what is known
/// here of its recovery (`kept` with `kept_reason`, or `recovery`).
pub(super) fn damaged_row(damaged: &zerus_replica::Damaged) -> Value {
    let mut row = serde_json::to_value(damaged).unwrap_or_else(|_| json!({"uuid": damaged.uuid}));
    row["error"] = json!(capped_error(&damaged.error));
    row
}

/// A row's prompt text capped for a page, marked when it was cut.
pub(super) fn capped(mut row: Value) -> Value {
    if let Some(text) = row["text"].as_str() {
        if text.len() > TEXT_BYTES {
            row["text"] = json!(cut(text, TEXT_BYTES));
            row["text_truncated"] = json!(true);
        }
    }
    row
}

/// A cached row, only if it is the object the index wrote for this task:
/// its uuid, stage, short ID and rank equal the indexed ones the page was
/// selected and ordered by. Anything else is a damaged index, never served.
fn cached_row(raw: &str, listed: &Listed, stage: Stage) -> Outcome<Value> {
    let row: Value = serde_json::from_str(raw).map_err(|_| Failure::Damaged)?;
    let fits = row.is_object()
        && row["uuid"].as_str() == Some(listed.0.as_str())
        && row["stage"].as_str() == Some(stage.as_str())
        && row["id"].as_str() == Some(listed.6.as_str())
        && row["rank"].as_str() == Some(listed.1.as_str());
    if fits {
        Ok(row)
    } else {
        Err(Failure::Damaged)
    }
}

/// What the index keeps of one task.
enum Derived {
    Row {
        stage: &'static str,
        rank: String,
        id: String,
        row: String,
        sends: Vec<i64>,
        held: Option<Held>,
    },
    /// A task that does not load, or whose header cannot be read; one that
    /// loads still holds its inventory version.
    Damaged(String, Option<Held>),
}

/// What the phone's inventory holds of a loaded task (zerus-cg5h.21): its
/// version (canonical JSON) and whether it is Done. None when its stage
/// cannot be read, so it is in no inventory read from the index.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    version: String,
    done: bool,
}

fn held(task: &TaskDoc) -> Option<Held> {
    let stage = task.stage().ok()?;
    Some(Held {
        version: serde_json::to_string(&task.inventory_version()).ok()?,
        done: stage == Stage::Done,
    })
}

/// Whether this read emulates the build before the repair rule (tests).
fn legacy() -> bool {
    #[cfg(test)]
    return LEGACY_DERIVE.with(std::cell::Cell::get);
    #[cfg(not(test))]
    false
}

/// Whether this build keeps the repair epoch (zerus-cg5h.21.2): builds
/// before it (emulated in tests) do not.
fn epochs() -> bool {
    #[cfg(test)]
    if PROOF_WRITER.with(std::cell::Cell::get) {
        return false;
    }
    !legacy()
}

/// A task's row and the stamps of its files, read as the store's inventory
/// reads it (zerus-cg5h.21): a task whose execution schema an older
/// replica's merged upgrade lowered is repaired and saved first, and the
/// version read and the stamps taken are one observation.
fn derive(store: &ProjectStore, uuid: &str, stamp: impl FnOnce() -> (String, bool)) -> (Derived, (String, bool)) {
    if legacy() {
        return (derive_from(store.load(uuid)), stamp());
    }
    let (loaded, stamps) = store.observe(uuid, stamp);
    (derive_from(loaded), stamps)
}

fn derive_from(loaded: zerus_replica::Result<TaskDoc>) -> Derived {
    #[cfg(test)]
    DECODED.with(|n| n.set(n.get() + 1));
    let damaged = |error: String, held| Derived::Damaged(cut(&error, ERROR_BYTES).to_owned(), held);
    let task = match loaded {
        Ok(task) => task,
        Err(error) => return damaged(error.to_string(), None),
    };
    let held = held(&task);
    let view = match task.view() {
        Ok(view) => view,
        Err(error) => return damaged(error.to_string(), held),
    };
    let (stage, rank, id) = (view.stage.as_str(), view.rank.clone(), view.id.clone());
    // What `ProjectStore::agent_starts_since` counts.
    let sends = view
        .log
        .iter()
        .filter(|e| e.kind == "sent" && matches!(e.actor, Actor::Agent(_)))
        .map(|e| e.at)
        .collect();
    match row(view, task.revision()) {
        Ok(row) => Derived::Row {
            stage,
            rank,
            id,
            row: capped(row).to_string(),
            sends,
            held,
        },
        Err(error) => damaged(error.message, held),
    }
}

/// The task index as the place a write puts a task and the short IDs it
/// avoids (zerus-cg5h.24): it answers only while it may place tasks
/// (`Index::placeable`), and declines on any failure, so the whole write
/// reads the documents instead. The replica checks the chosen neighbours
/// against their documents.
pub(super) struct IndexPlacement<'i, 'a> {
    index: &'i Index<'a>,
}

impl<'i, 'a> IndexPlacement<'i, 'a> {
    pub(super) fn new(index: &'i Index<'a>) -> Self {
        Self { index }
    }
}

impl zerus_replica::Placement for IndexPlacement<'_, '_> {
    fn order(&self, stage: Stage) -> zerus_replica::Result<Option<Vec<zerus_replica::Placed>>> {
        Ok(match self.index.placeable() {
            Ok(true) => self.index.stage_order(stage).ok(),
            _ => None,
        })
    }

    fn short_id_used(&self, id: &str) -> zerus_replica::Result<Option<bool>> {
        Ok(match self.index.placeable() {
            Ok(true) => self.index.short_id_used(id).ok(),
            _ => None,
        })
    }
}

/// Where a page is: the first rows, after or before a row's key, or the last rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Anchor {
    Start,
    After(String, String),
    Before(String, String),
    /// The last page.
    End,
}

/// One page of a stage, in the stage's order.
pub(super) struct Page {
    pub rows: Vec<Value>,
    /// The zero-based place of the first row in the stage.
    pub position: usize,
    pub next: Option<String>,
    pub previous: Option<String>,
}

/// The session a filtered page belongs to, by identity, never by name: its
/// computer and session ID (`launch_id:...` or `run_id:...`).
#[derive(Clone, Debug)]
pub(super) struct Scope {
    owner: String,
}

impl Scope {
    pub(super) fn new(machine: &str, session_id: &str) -> Self {
        Self { owner: owner_key(machine, session_id) }
    }

    /// Whether `row` (a listed task) belongs to this session's view.
    pub(super) fn includes(&self, row: &Value) -> bool {
        let (owner, unassigned) = owner_of_value(row);
        unassigned || owner.as_deref() == Some(self.owner.as_str())
    }
}

fn owner_key(machine: &str, session_id: &str) -> String {
    format!("{machine}\n{session_id}")
}

/// A listed task's owner session and whether it is unassigned, as the desktop
/// inspector shows them (`belongsTo` and `boardRows` in tasks-ui.ts): queued
/// tasks belong to their target (a new session's or subagent's parent), others
/// to their executor; a queued task for any session belongs to every session.
/// A reference without a session ID belongs to none: names never match.
fn owner_of_value(row: &Value) -> (Option<String>, bool) {
    let queued = matches!(row["stage"].as_str(), Some("todo" | "backlog"));
    let target = &row["target"];
    if queued && target["kind"] == "any" {
        return (None, true);
    }
    let reference = if queued {
        if target["parent"].is_object() { &target["parent"] } else { target }
    } else {
        &row["session"]
    };
    let (machine, id) = (reference["machine"].as_str().unwrap_or(""), reference["session_id"].as_str().unwrap_or(""));
    ((!machine.is_empty() && !id.is_empty()).then(|| owner_key(machine, id)), false)
}

fn owner_of(row: &str) -> (Option<String>, bool) {
    serde_json::from_str::<Value>(row).map(|row| owner_of_value(&row)).unwrap_or((None, false))
}

/// Which rows one keyset range read covers.
enum Part<'a> {
    All,
    Owner(&'a str),
    Unassigned,
}

impl Part<'_> {
    fn predicate(&self) -> &'static str {
        match self {
            Part::All => "",
            Part::Owner(_) => "owner=?5 AND ",
            Part::Unassigned => "unassigned=1 AND ",
        }
    }
}

fn scope(project: &str, stage: Stage) -> String {
    let hash = Sha256::digest(format!("task-list\n{project}\n{}", stage.as_str()));
    format!("{hash:x}")[..16].to_owned()
}

/// An opaque list cursor: bound to the project and stage, naming a row's
/// rank and UUID, so rows added or moved elsewhere never shift it.
pub(super) fn cursor(project: &str, stage: Stage, rank: &str, uuid: &str) -> String {
    let rank: String = rank.bytes().map(|b| format!("{b:02x}")).collect();
    format!("t1.{}.{rank}.{uuid}", scope(project, stage))
}

pub(super) fn parse_cursor(project: &str, stage: Stage, cursor: &str) -> Result<(String, String)> {
    let refused = || fail("task list cursor does not match this list");
    let parts: Vec<&str> = cursor.split('.').collect();
    let [version, found, rank, uuid] = parts[..] else {
        return Err(refused());
    };
    if version != "t1"
        || found != scope(project, stage)
        || rank.len() > 256
        || rank.len() % 2 != 0
        || !rank.bytes().all(|b| b.is_ascii_hexdigit())
        || !ids::is_uuid(uuid)
    {
        return Err(refused());
    }
    let bytes = (0..rank.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&rank[i..i + 2], 16))
        .collect::<std::result::Result<Vec<u8>, _>>()
        .map_err(|_| refused())?;
    let rank = String::from_utf8(bytes).map_err(|_| refused())?;
    Ok((rank, uuid.to_owned()))
}

/// A listed row: UUID, rank, file stamps, row, when it was read, whether its
/// files were settled then, and its short ID.
type Listed = (String, String, String, String, i64, bool, String);

pub(super) struct Index<'a> {
    store: &'a ProjectStore,
    db: Connection,
}

/// Removes the index and its SQLite files. Only while the project lock is
/// held and this process has no connection to it open.
fn dispose(dir: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(dir.join(format!("{FILE}{suffix}")));
    }
}

fn connect(dir: &Path) -> Outcome<Connection> {
    // The project folder is private; check its resolved form too, so an
    // ancestor changed between the two never selects another folder.
    let canonical = platform::canonical(dir)?;
    for folder in [dir, canonical.as_path()] {
        if !platform::verify_private(folder, platform::Private::Directory)? {
            return Err(Failure::Other);
        }
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        match platform::verify_private(&dir.join(format!("{FILE}{suffix}")), platform::Private::File) {
            Ok(true) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(Failure::Damaged),
        }
    }
    let path = canonical.join(FILE);
    // Never close a raw descriptor of a live database (zerus-iiee): create
    // it owner-only, check it by path and bind the connection to that file.
    #[cfg(unix)]
    let identity = {
        crate::sqlite_file::create(&path)?;
        crate::sqlite_file::private(&path).map_err(|_| Failure::Damaged)?
    };
    // Windows locks belong to the handle that took them.
    #[cfg(not(unix))]
    {
        let file = platform::no_follow(platform::private_open(
            fs::OpenOptions::new().create(true).truncate(false).read(true).write(true),
        ))
        .open(&path)?;
        if !platform::verify_private_file(&file)? {
            return Err(Failure::Damaged);
        }
    }
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    #[cfg(unix)]
    crate::sqlite_file::bind_private(&db, &path, identity).map_err(|_| Failure::Damaged)?;
    db.busy_timeout(Duration::from_secs(2))?;
    let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Failure::Damaged);
    }
    db.execute_batch("PRAGMA synchronous=NORMAL;")?;
    let page_size: i64 = db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let allowed = budget(page_size, pages).ok_or(Failure::Damaged)?;
    db.execute_batch(&format!("PRAGMA max_page_count={allowed};"))?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name IN
            ('meta', 'tasks', 'sends', 'tasks_order', 'tasks_short', 'tasks_checked', 'tasks_untrusted',
             'sends_at', 'sends_uuid', 'histories', 'entries', 'entries_place', 'entries_id',
             'tasks_owner', 'tasks_unassigned', 'tasks_held')",
        [],
        |r| r.get(0),
    )?;
    if version != SCHEMA || objects != 16 {
        // The one-task history cache before this index; nothing has it open
        // while the project lock is held.
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let _ = fs::remove_file(dir.join(format!("{OLD_HISTORY}{suffix}")));
        }
        // New, or not this schema: start again from the documents.
        db.execute_batch(&format!(
            "BEGIN IMMEDIATE;
            DROP TABLE IF EXISTS meta; DROP TABLE IF EXISTS tasks; DROP TABLE IF EXISTS sends;
            DROP TABLE IF EXISTS histories; DROP TABLE IF EXISTS entries;
            CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE tasks(uuid TEXT NOT NULL UNIQUE, files TEXT NOT NULL, stage TEXT, rank TEXT,
                short_id TEXT, row TEXT, error TEXT, checked INTEGER NOT NULL, trusted INTEGER NOT NULL,
                owner TEXT, unassigned INTEGER NOT NULL DEFAULT 0, version TEXT, done INTEGER, repaired INTEGER);
            CREATE INDEX tasks_order ON tasks(stage, rank, uuid);
            CREATE INDEX tasks_owner ON tasks(owner, stage, rank, uuid);
            CREATE INDEX tasks_unassigned ON tasks(stage, rank, uuid) WHERE unassigned = 1;
            CREATE INDEX tasks_held ON tasks(done, uuid) WHERE version IS NOT NULL;
            CREATE INDEX tasks_short ON tasks(short_id);
            CREATE INDEX tasks_checked ON tasks(checked);
            CREATE INDEX tasks_untrusted ON tasks(checked) WHERE trusted = 0;
            CREATE TABLE sends(uuid TEXT NOT NULL, at INTEGER NOT NULL);
            CREATE INDEX sends_at ON sends(at);
            CREATE INDEX sends_uuid ON sends(uuid);
            CREATE TABLE histories(uuid TEXT PRIMARY KEY, files TEXT NOT NULL, header TEXT NOT NULL,
                summary TEXT NOT NULL, total INTEGER NOT NULL, used INTEGER NOT NULL);
            CREATE TABLE entries(uuid TEXT NOT NULL, id TEXT NOT NULL, at INTEGER NOT NULL,
                ordinal INTEGER NOT NULL, payload TEXT NOT NULL, summary TEXT NOT NULL);
            CREATE UNIQUE INDEX entries_place ON entries(uuid, at, ordinal);
            CREATE UNIQUE INDEX entries_id ON entries(uuid, id);
            PRAGMA user_version={SCHEMA};
            COMMIT;"
        ))?;
    }
    // The repair epoch each row was read under (zerus-cg5h.21.2), which
    // builds before it never write. Added in place: the user version and the
    // objects stay, so those builds keep the file; until every row was read
    // here, the index does not answer for the phone.
    let present: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'repaired')", [], |r| r.get(0))?;
    if !present {
        db.execute_batch(
            "BEGIN IMMEDIATE;
            ALTER TABLE tasks ADD COLUMN repaired INTEGER;
            INSERT OR REPLACE INTO meta VALUES('uncertified', '1');
            DELETE FROM meta WHERE key = 'repair_floor';
            COMMIT;",
        )?;
    }
    Ok(db)
}

impl<'a> Index<'a> {
    /// The project's index, brought up to date with its documents; `None`
    /// when it cannot be used now, and the caller reads the documents.
    pub(super) fn open(store: &'a ProjectStore) -> Option<Self> {
        let dir = store.dir();
        for attempt in 0..2 {
            let opened = connect(dir).and_then(|db| {
                let mut index = Self { store, db };
                index.sync()?;
                Ok(index)
            });
            match opened {
                Ok(index) => return Some(index),
                // The connection is closed by now; nothing else has it open.
                Err(Failure::Damaged) if attempt == 0 => dispose(dir),
                Err(_) => return None,
            }
        }
        None
    }

    /// Closes the index; a damaged one is removed, to be built again.
    pub(super) fn discard(self, failure: &Failure) {
        let dir = self.store.dir().to_owned();
        drop(self.db);
        if matches!(failure, Failure::Damaged) {
            dispose(&dir);
        }
    }

    /// The current repair epoch (zerus-cg5h.21.2).
    fn epoch(&self) -> Outcome<i64> {
        Ok(self.meta("repair_epoch")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    fn meta(&self, key: &str) -> Outcome<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// The index's place in the journal, the task folder's stamp and the
    /// time of the last full check.
    fn sync(&mut self) -> Outcome<()> {
        let dir = self.store.dir().to_owned();
        let generation = self.meta("generation")?.unwrap_or_default();
        let offset = self.meta("offset")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        let checked: i64 = self.meta("checked")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        let built = self.meta("revision")?.is_some();
        let folder = stamp(&dir.join("tasks"));
        let changes = self.store.changes_since(&generation, offset)?;
        let now = now();
        let quiet = changes.documents.as_ref().is_some_and(BTreeSet::is_empty);
        let periodic = now < checked || now - checked >= FULL_CHECK_MS;
        // An index whose rows a build without the repair rule read (#1724)
        // reads every row again once. After that, a build without the rule
        // that shares the file can still store rows: any row change this
        // build did not make leaves "derived_revision" behind "revision", and
        // the rows stored since this build's last write ("derived_at", by the
        // checked index) are read again. A quiet read compares two values.
        let ours = !legacy();
        let mut rederive = ours && self.meta("derived")?.as_deref() != Some(DERIVED);
        let mut foreign: BTreeSet<String> = BTreeSet::new();
        if ours && !rederive && built && self.meta("revision")? != self.meta("derived_revision")? {
            let since: i64 = self.meta("derived_at")?.and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut query = self.db.prepare_cached("SELECT uuid FROM tasks WHERE checked >= ?1")?;
            for uuid in query.query_map([since], |r| r.get::<_, String>(0))? {
                foreign.insert(uuid?);
            }
            // A clock set back can hide those rows: then every row is read again.
            rederive = foreign.is_empty();
        }
        // Clocks may also straddle that time, a later read of the same row by
        // such a build moves its stamps and read time without a new revision,
        // and #1736/#1748 builds mark "derived_revision" current themselves
        // (zerus-cg5h.21.2): any row change by a build without the repair
        // epoch leaves "epoch_revision", which only such builds as this one
        // write, behind "revision". Then a new repair epoch makes every row
        // pending until read here again, and the phone's inventory is not read
        // from the index until then.
        if epochs() && built && self.meta("revision")? != self.meta("epoch_revision")? {
            let epoch = self.epoch()? + 1;
            self.db.execute("INSERT OR REPLACE INTO meta VALUES('repair_epoch', ?1)", [epoch.to_string()])?;
            self.db.execute("INSERT OR REPLACE INTO meta VALUES('uncertified', '1')", [])?;
        }
        let full = !built
            || rederive
            || changes.documents.is_none()
            // The folder changed while the journal says nothing did: a
            // writer that does not keep the journal.
            || (quiet && self.meta("folder")?.as_deref() != Some(folder.as_str()))
            || periodic;
        // A clock set back makes earlier reads look recent: any read dated
        // after now is suspect, and so is everything before a suspect mark
        // dated after now.
        let past = now - MAX_AGE_MS - 1;
        self.db.execute("UPDATE tasks SET checked=?1 WHERE checked > ?2", params![past, now])?;
        if self.meta("suspect")?.and_then(|v| v.parse::<i64>().ok()).is_some_and(|mark| mark > now) {
            self.db.execute("INSERT OR REPLACE INTO meta VALUES('suspect', ?1)", [now.to_string()])?;
        }
        let moved = changes.generation != generation || changes.offset != offset;
        if built && changes.documents.is_none() && moved {
            // The journal cannot say what changed (another generation, left
            // behind, damaged or missing; it is started again): every row
            // read before now is suspect, whatever its stamps.
            self.db.execute("INSERT OR REPLACE INTO meta VALUES('suspect', ?1)", [now.to_string()])?;
        }
        if full {
            let (known, uuids) = self.known()?;
            let every: BTreeSet<String>;
            let noted = if rederive {
                every = uuids.iter().cloned().collect();
                Some(&every)
            } else {
                changes.documents.as_ref()
            };
            self.refresh(&uuids, Some(&known), noted)?;
        } else if let Some(documents) = &changes.documents {
            let uuids: Vec<String> = documents.iter().filter(|d| ids::is_uuid(d)).cloned().collect();
            self.refresh(&uuids, None, Some(documents))?;
        }
        // Suspect rows are read again, a bounded batch at a time, those read
        // or tried longest ago first: a row read again goes to the back, so
        // every suspect row is reached within ceil(suspect / BATCH) reads.
        let floor = self.floor(now)?;
        let suspect: BTreeSet<String> = {
            let mut oldest: Vec<(i64, String)> = Vec::new();
            for sql in [
                "SELECT checked, uuid FROM tasks WHERE checked < ?1 ORDER BY checked LIMIT ?2",
                "SELECT checked, uuid FROM tasks WHERE trusted = 0 AND ?1 = ?1 ORDER BY checked LIMIT ?2",
            ] {
                let mut query = self.db.prepare_cached(sql)?;
                let rows = query.query_map(params![floor, BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?;
                for row in rows {
                    oldest.push(row?);
                }
            }
            oldest.sort();
            oldest.dedup_by(|a, b| a.1 == b.1);
            oldest.into_iter().take(BATCH as usize).map(|(_, uuid)| uuid).collect()
        };
        if !suspect.is_empty() {
            let list: Vec<String> = suspect.iter().cloned().collect();
            self.refresh(&list, None, Some(&suspect))?;
        }
        if !rederive && !foreign.is_empty() {
            let list: Vec<String> = foreign.iter().cloned().collect();
            self.refresh(&list, None, Some(&foreign))?;
        }
        // Rows not yet read under the current repair epoch, a bounded batch
        // at a time; the index answers for the phone again once none remain.
        if epochs() && self.meta("uncertified")?.is_some() {
            let epoch = self.epoch()?;
            let pending: BTreeSet<String> = {
                let mut query =
                    self.db.prepare_cached("SELECT uuid FROM tasks WHERE repaired IS NULL OR repaired < ?1 LIMIT ?2")?;
                let rows = query.query_map(params![epoch, BATCH], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            if !pending.is_empty() {
                let list: Vec<String> = pending.iter().cloned().collect();
                self.refresh(&list, None, Some(&pending))?;
            }
            let left: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE repaired IS NULL OR repaired < ?1)",
                [epoch],
                |r| r.get(0),
            )?;
            if !left {
                self.db.execute("DELETE FROM meta WHERE key = 'uncertified'", [])?;
            }
        }
        // The project document's version, as the phone's inventory holds it:
        // read again when the journal names it, and at a full check when its
        // file changed; kept apart from the rows and their revision.
        let noted = changes.documents.as_ref().is_some_and(|d| d.contains("project"));
        let project = if full || noted || self.meta("project_version")?.is_none() {
            self.project_version(now, noted)?
        } else {
            None
        };
        let tx = self.db.transaction()?;
        {
            let mut set = tx.prepare("INSERT OR REPLACE INTO meta VALUES(?1, ?2)")?;
            set.execute(params!["generation", changes.generation])?;
            set.execute(params!["offset", changes.offset.to_string()])?;
            set.execute(params!["folder", folder])?;
            if full {
                set.execute(params!["checked", now.to_string()])?;
            }
            if full && ours {
                set.execute(params!["derived", DERIVED])?;
            }
            match project {
                Some((files, Some(version))) => {
                    set.execute(params!["project_files", files])?;
                    set.execute(params!["project_version", version])?;
                }
                // Unreadable now: read again at the next sync.
                Some((_, None)) => {
                    tx.execute("DELETE FROM meta WHERE key IN ('project_files', 'project_version')", [])?;
                }
                None => (),
            }
            tx.execute("INSERT OR IGNORE INTO meta VALUES('revision', ?1)", [ids::new_uuid()])?;
            if ours {
                // Every row is now as this build reads it.
                tx.execute("INSERT OR REPLACE INTO meta SELECT 'derived_revision', value FROM meta WHERE key = 'revision'", [])?;
                tx.execute("INSERT OR REPLACE INTO meta VALUES('derived_at', ?1)", [self::now().to_string()])?;
            }
            if epochs() {
                // Rows another build changed are pending under the epoch.
                tx.execute("INSERT OR REPLACE INTO meta SELECT 'epoch_revision', value FROM meta WHERE key = 'revision'", [])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The project document's stamp and version (canonical JSON; None when
    /// it cannot be read) to keep, or None when the kept ones still hold:
    /// its file's stamp is unchanged and the journal (`noted`) names no save.
    /// An unsettled file's stamp is not kept, so it is read again next time.
    fn project_version(&self, now: i64, noted: bool) -> Outcome<Option<(String, Option<String>)>> {
        let (files, settled) = stamp_at(&self.store.dir().join("project.loro"), now);
        let kept = self.meta("project_version")?.is_some() && self.meta("project_files")?.as_deref() == Some(files.as_str());
        if kept && !noted {
            return Ok(None);
        }
        let version = self.store.project_version().ok().and_then(|v| serde_json::to_string(&v).ok());
        Ok(Some((if settled { files } else { String::new() }, version)))
    }

    /// Rows checked before this are suspect: read before the journal last
    /// became unknown, or longer than `MAX_AGE_MS` ago. So are rows read from
    /// unsettled files (`trusted = 0`) and rows dated after now.
    fn floor(&self, now: i64) -> Outcome<i64> {
        let suspect = self.meta("suspect")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        Ok(suspect.max(now - MAX_AGE_MS))
    }

    /// Indexes again each of `uuids` whose files changed (`known` holds the
    /// stamps already read and the folder listing, for a full check), and
    /// every one the journal names (`noted`) whatever its stamps: Windows has
    /// no inode and coarse file times, so a rewrite of the same length can
    /// keep its stamp. Short transactions; any change gives the index a new
    /// revision.
    fn refresh(&mut self, uuids: &[String], known: Option<&Known>, noted: Option<&BTreeSet<String>>) -> Outcome<()> {
        let dir = self.store.dir().to_owned();
        let read_at = now();
        // The repair epoch rows read here are marked with; an emulated older
        // build writes none (zerus-cg5h.21.2).
        let epoch = epochs().then(|| self.epoch()).transpose()?;
        for chunk in uuids.chunks(CHUNK) {
            let mut work = Vec::new();
            for uuid in chunk {
                // When it was read orders the background queue; whether its
                // files were settled decides if a page may trust it.
                let checked = read_at;
                // A task the journal names is read whatever its stamps, and
                // stamped only with that read.
                let forced = noted.is_some_and(|noted| noted.contains(uuid));
                if !forced {
                    // A full check compares with the listing and reads the
                    // stored row only of a task whose stamps changed.
                    let (stamps, now) = match known {
                        Some(known) => (
                            known.stamps.get(uuid).cloned(),
                            known.listed.get(uuid).cloned().unwrap_or_else(|| format!("{MISSING}|{MISSING}|{MISSING}")),
                        ),
                        None => (self.stored(uuid)?.map(|s| s.0), stamped(&dir, uuid, read_at).0),
                    };
                    let unchanged = stamps.as_deref() == Some(now.as_str()) || (stamps.is_none() && gone(&now));
                    if unchanged {
                        continue;
                    }
                }
                let stored = self.stored(uuid)?;
                // A repair saves the task: its row keeps the stamps taken
                // with that read, and a task whose files are gone has none.
                let (derived, (now, trusted)) = derive(self.store, uuid, || stamped(&dir, uuid, read_at));
                let derived = (!gone(&now)).then_some(derived);
                // Read again to the same row: only its stamps and time change.
                let same = match (&derived, &stored) {
                    (Some(Derived::Row { row, held, .. }), Some((_, Some(old), None, was))) => row == old && held == was,
                    (Some(Derived::Damaged(error, held)), Some((_, None, Some(old), was))) => error == old && held == was,
                    _ => false,
                };
                work.push((uuid, now, derived, same, checked, trusted));
            }
            if work.is_empty() {
                continue;
            }
            let tx = self.db.transaction()?;
            let mut changed = false;
            for (uuid, files, derived, same, checked, trusted) in work {
                if same {
                    match epoch {
                        Some(epoch) => tx.execute(
                            "UPDATE tasks SET files=?1, checked=?2, trusted=?3, repaired=?5 WHERE uuid=?4",
                            params![files, checked, trusted, uuid, epoch],
                        )?,
                        None => tx.execute(
                            "UPDATE tasks SET files=?1, checked=?2, trusted=?3 WHERE uuid=?4",
                            params![files, checked, trusted, uuid],
                        )?,
                    };
                    continue;
                }
                changed = true;
                tx.execute("DELETE FROM tasks WHERE uuid=?1", [uuid])?;
                tx.execute("DELETE FROM sends WHERE uuid=?1", [uuid])?;
                // A changed task's history is read again when next asked for.
                tx.execute("DELETE FROM histories WHERE uuid=?1", [uuid])?;
                tx.execute("DELETE FROM entries WHERE uuid=?1", [uuid])?;
                match derived {
                    None => (),
                    Some(Derived::Damaged(error, held)) => {
                        let (version, done) = held.map(|h| (h.version, h.done)).unzip();
                        tx.execute(
                            "INSERT INTO tasks(uuid, files, error, checked, trusted, version, done, repaired)
                             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![uuid, files, error, checked, trusted, version, done, epoch],
                        )?;
                    }
                    Some(Derived::Row { stage, rank, id, row, sends, held }) => {
                        let (owner, unassigned) = owner_of(&row);
                        let (version, done) = held.map(|h| (h.version, h.done)).unzip();
                        tx.execute(
                            "INSERT INTO tasks(uuid, files, stage, rank, short_id, row, checked, trusted, owner, unassigned,
                                version, done, repaired)
                             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                            params![uuid, files, stage, rank, id, row, checked, trusted, owner, unassigned, version, done, epoch],
                        )?;
                        for at in sends {
                            tx.execute("INSERT INTO sends VALUES(?1, ?2)", params![uuid, at])?;
                        }
                    }
                }
            }
            if changed {
                // This build's own change keeps the repair evidence current,
                // unless another build changed rows since (see `sync`).
                let clean: bool = !legacy()
                    && tx.query_row(
                        "SELECT (SELECT value FROM meta WHERE key = 'revision') IS (SELECT value FROM meta WHERE key = 'derived_revision')",
                        [],
                        |r| r.get(0),
                    )?;
                let clean_epoch: bool = epochs()
                    && tx.query_row(
                        "SELECT (SELECT value FROM meta WHERE key = 'revision') IS (SELECT value FROM meta WHERE key = 'epoch_revision')",
                        [],
                        |r| r.get(0),
                    )?;
                let revision = ids::new_uuid();
                tx.execute("INSERT OR REPLACE INTO meta VALUES('revision', ?1)", [&revision])?;
                if clean {
                    tx.execute("INSERT OR REPLACE INTO meta VALUES('derived_revision', ?1)", [&revision])?;
                }
                if clean_epoch {
                    tx.execute("INSERT OR REPLACE INTO meta VALUES('epoch_revision', ?1)", [&revision])?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// What a full check compares with, and every task it checks: each one
    /// the index holds or the task folder lists.
    fn known(&self) -> Outcome<(Known, Vec<String>)> {
        let mut stamps: HashMap<String, String> = HashMap::new();
        {
            let mut query = self.db.prepare_cached("SELECT uuid, files FROM tasks")?;
            let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (uuid, files) = row?;
                stamps.insert(uuid, files);
            }
        }
        let listed = listed(self.store.dir())?;
        let mut uuids: BTreeSet<&String> = listed.keys().collect();
        uuids.extend(stamps.keys());
        let uuids: Vec<String> = uuids.into_iter().cloned().collect();
        Ok((Known { stamps, listed }, uuids))
    }

    /// A task's stored row: its stamps, row or error, and what a phone holds.
    fn stored(&self, uuid: &str) -> Outcome<Option<Stored>> {
        #[cfg(test)]
        STORED.with(|n| n.set(n.get() + 1));
        let mut query = self.db.prepare_cached("SELECT files, row, error, version, done FROM tasks WHERE uuid=?1")?;
        Ok(query
            .query_row([uuid], |r| {
                let held = match (r.get::<_, Option<String>>(3)?, r.get::<_, Option<bool>>(4)?) {
                    (Some(version), Some(done)) => Some(Held { version, done }),
                    _ => None,
                };
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, held))
            })
            .optional()?)
    }

    /// Whether the index may place tasks (zerus-cg5h.24): certified current
    /// under the repair epoch (no row change by another build pending) and
    /// holding no damaged task, whose document may still name a short ID or
    /// a place the index does not have.
    fn placeable(&self) -> Outcome<bool> {
        if self.meta("uncertified")?.is_some() {
            return Ok(false);
        }
        let damaged: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE error IS NOT NULL)", [], |r| r.get(0))?;
        Ok(!damaged)
    }

    /// Checks every task file's stamp before the index places a task
    /// (zerus-cg5h.24), as the periodic full check does: a writer without the
    /// journal may have added or moved a task while the journal named other
    /// saves, which no stamp of the task folder tells apart from them. New,
    /// changed and vanished task files are read again; no other task is
    /// decoded.
    pub(super) fn check_files(&mut self) -> Outcome<()> {
        let (known, uuids) = self.known()?;
        // The periodic full check keeps its own clock: it also reads the
        // project document again, which this check does not (zerus-k2388).
        self.refresh(&uuids, Some(&known), None)?;
        Ok(())
    }

    /// The Done tasks the phone's Done page is chosen from (zerus-cg5h.23),
    /// from their rows: each one's UUID, when it entered Done and the session
    /// it was sent to. None when the index may not answer (`placeable`, after
    /// checking every task file's stamp) or a row lacks them; the store then
    /// reads every task as before.
    pub(super) fn done_entries(&mut self) -> Outcome<Option<Vec<zerus_replica::DoneEntry>>> {
        self.check_files()?;
        if !self.placeable()? {
            return Ok(None);
        }
        let mut query = self.db.prepare_cached("SELECT uuid, row FROM tasks WHERE done = 1 AND version IS NOT NULL")?;
        let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
        let mut entries = Vec::new();
        for row in rows {
            let (uuid, row) = row?;
            let Some(row) = row.and_then(|row| serde_json::from_str::<Value>(&row).ok()) else {
                return Ok(None);
            };
            let Some(at) = row["stage_at"].as_i64() else {
                return Ok(None);
            };
            // The row's session property is required: an explicit null is a
            // task sent to no session, a missing one is a row that cannot say
            // (zerus-7w9zj), so the store answers.
            let sent = match row.get("session") {
                None => return Ok(None),
                Some(Value::Null) => None,
                Some(session) => match serde_json::from_value::<SessionKey>(session.clone()) {
                    Ok(session) => Some(session),
                    Err(_) => return Ok(None),
                },
            };
            entries.push(zerus_replica::DoneEntry { at, uuid, sent });
        }
        Ok(Some(entries))
    }

    /// The loadable tasks of `stage` in (rank, UUID) order.
    fn stage_order(&self, stage: Stage) -> Outcome<Vec<zerus_replica::Placed>> {
        let mut query = self
            .db
            .prepare_cached("SELECT uuid, short_id, rank FROM tasks WHERE stage = ?1 AND row IS NOT NULL ORDER BY rank, uuid")?;
        let rows = query.query_map([stage.as_str()], |r| {
            Ok(zerus_replica::Placed { uuid: r.get(0)?, id: r.get(1)?, rank: r.get(2)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn short_id_used(&self, id: &str) -> Outcome<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE short_id = ?1)", [id], |r| r.get(0))?)
    }

    /// What a phone holds of this project (zerus-cg5h.21), as
    /// `ProjectStore::live_inventory` reads it: the project document and
    /// every loaded task that is not Done, read again only where the journal
    /// or the periodic check finds a change.
    pub(super) fn live_inventory(&mut self) -> Outcome<Inventory> {
        self.sync()?;
        // After another build changed rows, every row must have been read
        // here again under the current repair epoch (zerus-cg5h.21.2); until
        // then the caller reads the store.
        if self.meta("uncertified")?.is_some() {
            return Err(Failure::Other);
        }
        // Missing while the project document cannot be read: the caller
        // reads the store, which says why.
        let project = self.meta("project_version")?.ok_or(Failure::Other)?;
        let mut inventory = Inventory {
            project: serde_json::from_str(&project).map_err(|_| Failure::Damaged)?,
            tasks: BTreeMap::new(),
        };
        let mut query = self.db.prepare_cached("SELECT uuid, version FROM tasks WHERE done = 0 AND version IS NOT NULL")?;
        let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (uuid, version) = row?;
            inventory.tasks.insert(uuid, serde_json::from_str(&version).map_err(|_| Failure::Damaged)?);
        }
        Ok(inventory)
    }

    /// What a phone holding `theirs` is answered from, as
    /// `ProjectStore::phone_inventory` reads it: the live view and every
    /// loaded task it already holds, Done ones included.
    pub(super) fn phone_inventory(&mut self, theirs: &Inventory) -> Outcome<Inventory> {
        let mut inventory = self.live_inventory()?;
        let mut query = self.db.prepare_cached("SELECT version FROM tasks WHERE uuid = ?1 AND done = 1 AND version IS NOT NULL")?;
        let mut held = Vec::new();
        for uuid in theirs.tasks.keys().filter(|uuid| !inventory.tasks.contains_key(*uuid)) {
            if let Some(version) = query.query_row([uuid], |r| r.get::<_, String>(0)).optional()? {
                held.push((uuid.clone(), serde_json::from_str(&version).map_err(|_| Failure::Damaged)?));
            }
        }
        inventory.tasks.extend(held);
        Ok(inventory)
    }

    /// Changes whenever any indexed row changes, including after a rebuild.
    pub(super) fn revision(&self) -> Outcome<String> {
        self.meta("revision")?.ok_or(Failure::Damaged)
    }

    /// One page of `stage`. A page past either end is the nearest whole
    /// page, so every row stays reachable both ways; only an empty stage has
    /// none. Each row's files are checked first; a changed task is indexed
    /// again and the page read again.
    #[cfg(test)]
    pub(super) fn page(&mut self, stage: Stage, limit: usize, from: &Anchor) -> Outcome<Page> {
        self.page_in(stage, limit, from, None)
    }

    /// `page`, of only the rows of one session's view when `filter` is set:
    /// two bounded keyset reads (its own rows, unassigned rows) merged in the
    /// stage's order, with the same cursors.
    pub(super) fn page_in(&mut self, stage: Stage, limit: usize, from: &Anchor, filter: Option<&Scope>) -> Outcome<Page> {
        let mut from = from.clone();
        // Rows read from their documents in this call are current even when
        // their files are not yet settled.
        let mut read = BTreeSet::new();
        for _ in 0..4 {
            let rows = self.rows_in(stage, limit, &from, filter)?;
            if rows.is_empty() {
                match from {
                    Anchor::After(..) => {
                        from = Anchor::End;
                        continue;
                    }
                    Anchor::Before(..) => {
                        from = Anchor::Start;
                        continue;
                    }
                    Anchor::Start | Anchor::End => (),
                }
            }
            // A row is served only when its files are as read and it is not
            // suspect; any other is read again from its document first.
            let dir = self.store.dir().to_owned();
            let at = now();
            let floor = self.floor(at)?;
            let stale: BTreeSet<String> = rows
                .iter()
                .filter(|(uuid, _, stored, _, checked, trusted, _)| {
                    ((!*trusted || *checked < floor || *checked > at) && !read.contains(uuid))
                        || files(&dir, uuid) != *stored
                })
                .map(|(uuid, ..)| uuid.clone())
                .collect();
            if !stale.is_empty() {
                let list: Vec<String> = stale.iter().cloned().collect();
                self.refresh(&list, None, Some(&stale))?;
                read.extend(stale);
                continue;
            }
            return self.finish(stage, rows, filter);
        }
        Err(Failure::Other)
    }

    fn parts(filter: Option<&Scope>) -> Vec<Part<'_>> {
        match filter {
            None => vec![Part::All],
            Some(scope) => vec![Part::Owner(&scope.owner), Part::Unassigned],
        }
    }

    fn rows_in(&self, stage: Stage, limit: usize, from: &Anchor, filter: Option<&Scope>) -> Outcome<Vec<Listed>> {
        let Some(_) = filter else { return self.rows(stage, limit, from) };
        let mut rows = Vec::new();
        for part in Self::parts(filter) {
            rows.extend(self.rows_of(stage, limit, from, &part)?);
        }
        rows.sort_by(|a, b| (a.1.as_str(), a.0.as_str()).cmp(&(b.1.as_str(), b.0.as_str())));
        rows.dedup_by(|a, b| a.0 == b.0);
        // The page's side of the anchor: the first rows going forward, the
        // last going back.
        if matches!(from, Anchor::Before(..) | Anchor::End) {
            let extra = rows.len().saturating_sub(limit);
            rows.drain(..extra);
        } else {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    fn rows_of(&self, stage: Stage, limit: usize, from: &Anchor, part: &Part) -> Outcome<Vec<Listed>> {
        let predicate = part.predicate();
        let columns = "SELECT uuid, rank, files, row, checked, trusted, short_id FROM tasks";
        let (sql, rank, uuid) = match from {
            Anchor::Start => (format!("{columns} WHERE {predicate}stage=?1 AND (rank, uuid) > (?2, ?3) ORDER BY rank, uuid LIMIT ?4"), "", ""),
            Anchor::After(rank, uuid) => (format!("{columns} WHERE {predicate}stage=?1 AND (rank, uuid) > (?2, ?3) ORDER BY rank, uuid LIMIT ?4"), rank.as_str(), uuid.as_str()),
            Anchor::Before(rank, uuid) => (format!("{columns} WHERE {predicate}stage=?1 AND (rank, uuid) < (?2, ?3) ORDER BY rank DESC, uuid DESC LIMIT ?4"), rank.as_str(), uuid.as_str()),
            Anchor::End => (format!("{columns} WHERE {predicate}stage=?1 AND ?2 = ?3 ORDER BY rank DESC, uuid DESC LIMIT ?4"), "", ""),
        };
        let mut query = self.db.prepare_cached(&sql)?;
        let map = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?));
        let mut rows = match part {
            Part::Owner(owner) => query.query_map(params![stage.as_str(), rank, uuid, limit as i64, owner], map)?,
            _ => query.query_map(params![stage.as_str(), rank, uuid, limit as i64], map)?,
        }
        .collect::<rusqlite::Result<Vec<Listed>>>()?;
        if matches!(from, Anchor::Before(..) | Anchor::End) {
            rows.reverse();
        }
        Ok(rows)
    }

    fn rows(&self, stage: Stage, limit: usize, from: &Anchor) -> Outcome<Vec<Listed>> {
        const ASCENDING: &str = "SELECT uuid, rank, files, row, checked, trusted, short_id FROM tasks WHERE stage=?1 AND (rank, uuid) > (?2, ?3) ORDER BY rank, uuid LIMIT ?4";
        const DESCENDING: &str = "SELECT uuid, rank, files, row, checked, trusted, short_id FROM tasks WHERE stage=?1 AND (rank, uuid) < (?2, ?3) ORDER BY rank DESC, uuid DESC LIMIT ?4";
        const LAST: &str = "SELECT uuid, rank, files, row, checked, trusted, short_id FROM tasks WHERE stage=?1 AND ?2 = ?3 ORDER BY rank DESC, uuid DESC LIMIT ?4";
        let (sql, rank, uuid) = match from {
            Anchor::Start => (ASCENDING, "", ""),
            Anchor::After(rank, uuid) => (ASCENDING, rank.as_str(), uuid.as_str()),
            Anchor::Before(rank, uuid) => (DESCENDING, rank.as_str(), uuid.as_str()),
            Anchor::End => (LAST, "", ""),
        };
        let mut query = self.db.prepare_cached(sql)?;
        let mut rows = query
            .query_map(params![stage.as_str(), rank, uuid, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            })?
            .collect::<rusqlite::Result<Vec<Listed>>>()?;
        if matches!(from, Anchor::Before(..) | Anchor::End) {
            rows.reverse();
        }
        Ok(rows)
    }

    fn exists_in(&self, stage: Stage, rank: &str, uuid: &str, after: bool, filter: Option<&Scope>) -> Outcome<bool> {
        if filter.is_none() {
            return self.exists(stage, rank, uuid, after);
        }
        let side = if after { ">" } else { "<" };
        for part in Self::parts(filter) {
            let sql = format!("SELECT EXISTS(SELECT 1 FROM tasks WHERE {}stage=?1 AND (rank, uuid) {side} (?2, ?3))", part.predicate());
            let found: bool = match part {
                Part::Owner(owner) => self.db.query_row(&sql, params![stage.as_str(), rank, uuid, 0, owner], |r| r.get(0))?,
                _ => self.db.query_row(&sql, params![stage.as_str(), rank, uuid], |r| r.get(0))?,
            };
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn before_in(&self, stage: Stage, rank: &str, uuid: &str, filter: Option<&Scope>) -> Outcome<usize> {
        if filter.is_none() {
            return self.before(stage, rank, uuid);
        }
        let mut total = 0;
        for part in Self::parts(filter) {
            let sql = format!("SELECT count(*) FROM tasks WHERE {}stage=?1 AND (rank, uuid) < (?2, ?3)", part.predicate());
            let count: i64 = match part {
                Part::Owner(owner) => self.db.query_row(&sql, params![stage.as_str(), rank, uuid, 0, owner], |r| r.get(0))?,
                _ => self.db.query_row(&sql, params![stage.as_str(), rank, uuid], |r| r.get(0))?,
            };
            total += count as usize;
        }
        Ok(total)
    }

    fn exists(&self, stage: Stage, rank: &str, uuid: &str, after: bool) -> Outcome<bool> {
        let sql = if after {
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE stage=?1 AND (rank, uuid) > (?2, ?3))"
        } else {
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE stage=?1 AND (rank, uuid) < (?2, ?3))"
        };
        Ok(self.db.query_row(sql, params![stage.as_str(), rank, uuid], |r| r.get(0))?)
    }

    fn before(&self, stage: Stage, rank: &str, uuid: &str) -> Outcome<usize> {
        let count: i64 = self.db.query_row(
            "SELECT count(*) FROM tasks WHERE stage=?1 AND (rank, uuid) < (?2, ?3)",
            params![stage.as_str(), rank, uuid],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    fn finish(&self, stage: Stage, rows: Vec<Listed>, filter: Option<&Scope>) -> Outcome<Page> {
        let project = self.store.project_id();
        let (mut next, mut previous, mut position) = (None, None, 0);
        if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
            if self.exists_in(stage, &last.1, &last.0, true, filter)? {
                next = Some(cursor(project, stage, &last.1, &last.0));
            }
            if self.exists_in(stage, &first.1, &first.0, false, filter)? {
                previous = Some(cursor(project, stage, &first.1, &first.0));
            }
            position = self.before_in(stage, &first.1, &first.0, filter)?;
        }
        let rows = rows
            .into_iter()
            .map(|listed| cached_row(&listed.3, &listed, stage))
            .collect::<Outcome<Vec<Value>>>()?;
        Ok(Page { rows, position, next, previous })
    }

    /// `counts`, of one session's view when `filter` is set.
    pub(super) fn counts_in(&self, filter: Option<&Scope>) -> Outcome<BTreeMap<String, usize>> {
        let Some(scope) = filter else { return self.counts() };
        let mut counts = BTreeMap::new();
        for (sql, owner) in [
            ("SELECT stage, count(*) FROM tasks WHERE owner=?1 AND stage IS NOT NULL GROUP BY stage", Some(scope.owner.as_str())),
            ("SELECT stage, count(*) FROM tasks WHERE unassigned=1 AND stage IS NOT NULL GROUP BY stage", None),
        ] {
            let mut query = self.db.prepare_cached(sql)?;
            let map = |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?));
            let rows = match owner {
                Some(owner) => query.query_map([owner], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
                None => query.query_map([], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
            };
            for (stage, count) in rows {
                *counts.entry(stage).or_insert(0) += count as usize;
            }
        }
        Ok(counts)
    }

    /// The number of loadable tasks in each stage, by stage name.
    pub(super) fn counts(&self) -> Outcome<BTreeMap<String, usize>> {
        let mut query = self
            .db
            .prepare_cached("SELECT stage, count(*) FROM tasks WHERE stage IS NOT NULL GROUP BY stage")?;
        let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut counts = BTreeMap::new();
        for row in rows {
            let (stage, count) = row?;
            counts.insert(stage, count as usize);
        }
        Ok(counts)
    }

    /// The first damaged tasks by UUID, and how many there are. What is known
    /// of a shown one's recovery is read from its files now.
    pub(super) fn damaged(&self) -> Outcome<(Vec<Value>, usize)> {
        let mut query = self
            .db
            .prepare_cached("SELECT uuid, error FROM tasks WHERE error IS NOT NULL ORDER BY uuid LIMIT ?1")?;
        let shown = query
            .query_map([DAMAGED_SHOWN as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(uuid, error)| damaged_row(&self.store.damaged(&uuid, error)))
            .collect();
        let total: i64 = self
            .db
            .query_row("SELECT count(*) FROM tasks WHERE error IS NOT NULL", [], |r| r.get(0))?;
        Ok((shown, total as usize))
    }

    /// The UUIDs of the loadable tasks with short ID `id`, and whether any
    /// task does not load (its short ID cannot be read).
    pub(super) fn short_id(&self, id: &str) -> Outcome<(Vec<String>, bool)> {
        let mut query = self.db.prepare_cached("SELECT uuid FROM tasks WHERE short_id=?1")?;
        let uuids = query
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let damaged: bool = self
            .db
            .query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE error IS NOT NULL)", [], |r| r.get(0))?;
        Ok((uuids, damaged))
    }

    /// The place of task `uuid` in its stage, counting from 1.
    pub(super) fn place(&self, uuid: &str) -> Outcome<Option<usize>> {
        let found: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT stage, rank FROM tasks WHERE uuid=?1 AND stage IS NOT NULL",
                [uuid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((stage, rank)) = found else {
            return Ok(None);
        };
        let count: i64 = self.db.query_row(
            "SELECT count(*) FROM tasks WHERE stage=?1 AND (rank, uuid) < (?2, ?3)",
            params![stage, rank, uuid],
            |r| r.get(0),
        )?;
        Ok(Some(count as usize + 1))
    }

    /// One chronological page of task `uuid`'s history, `None` when the
    /// entry `before` names is gone. The histories of the most recently read
    /// tasks are kept; another is read from its document once and replaces
    /// the least recently read. `summary` returns attachment metadata
    /// instead of the files and names each entry.
    pub(super) fn history(
        &mut self,
        uuid: &str,
        limit: usize,
        before: Option<&str>,
        summary: bool,
        prefix: &str,
    ) -> Outcome<Option<Value>> {
        // The task's row is held to the rules a page holds its rows to: one
        // that is untrusted, read before the floor or dated after now is read
        // again first. A row read again with other content drops the cached
        // history, so the stamps check below never trusts a stale history.
        let at = now();
        let floor = self.floor(at)?;
        let row: Option<(i64, bool)> = self
            .db
            .query_row("SELECT checked, trusted FROM tasks WHERE uuid=?1", [uuid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if !row.is_some_and(|(checked, trusted)| trusted && checked >= floor && checked <= at) {
            let only = BTreeSet::from([uuid.to_owned()]);
            self.refresh(&[uuid.to_owned()], None, Some(&only))?;
        }
        let dir = self.store.dir().to_owned();
        let now = files(&dir, uuid);
        let cached: Option<String> = self
            .db
            .query_row("SELECT files FROM histories WHERE uuid=?1", [uuid], |r| r.get(0))
            .optional()?;
        let used: i64 = self
            .db
            .query_row("SELECT coalesce(max(used), 0) + 1 FROM histories", [], |r| r.get(0))?;
        if cached.as_deref() == Some(now.as_str()) {
            self.db.execute("UPDATE histories SET used=?1 WHERE uuid=?2", params![used, uuid])?;
        } else {
            #[cfg(test)]
            MATERIALIZED.with(|n| n.set(n.get() + 1));
            // A task that cannot be read is answered from its document,
            // which gives the reason.
            let task = self.store.load(uuid).map_err(|_| Failure::Other)?;
            let read = super::history::materialize(&task).map_err(|_| Failure::Other)?;
            let tx = self.db.transaction()?;
            tx.execute("DELETE FROM histories WHERE uuid=?1", [uuid])?;
            tx.execute("DELETE FROM entries WHERE uuid=?1", [uuid])?;
            tx.execute(
                "INSERT INTO histories VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![uuid, now, read.header.to_string(), read.summary.to_string(), read.entries.len() as i64, used],
            )?;
            {
                let mut insert = tx.prepare("INSERT INTO entries VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
                for (ordinal, entry) in read.entries.iter().enumerate() {
                    insert.execute(params![
                        uuid,
                        entry.id,
                        entry.at,
                        ordinal as i64,
                        entry.payload.to_string(),
                        entry.summary.to_string()
                    ])?;
                }
            }
            tx.execute(
                "DELETE FROM entries WHERE uuid IN (SELECT uuid FROM histories ORDER BY used DESC LIMIT -1 OFFSET ?1)",
                [HISTORIES],
            )?;
            tx.execute(
                "DELETE FROM histories WHERE uuid IN (SELECT uuid FROM histories ORDER BY used DESC LIMIT -1 OFFSET ?1)",
                [HISTORIES],
            )?;
            tx.commit()?;
        }
        let column = if summary { "summary" } else { "header" };
        let (header, total): (String, i64) = self.db.query_row(
            &format!("SELECT {column}, total FROM histories WHERE uuid=?1"),
            [uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let boundary = match before {
            Some(id) => match self
                .db
                .query_row(
                    "SELECT at, ordinal FROM entries WHERE uuid=?1 AND id=?2",
                    params![uuid, id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()?
            {
                Some(boundary) => boundary,
                None => return Ok(None),
            },
            None => (i64::MAX, i64::MAX),
        };
        let column = if summary { "summary" } else { "payload" };
        let mut query = self.db.prepare_cached(&format!(
            "SELECT id, {column}, at FROM entries WHERE uuid=?1 AND (at, ordinal) < (?2, ?3)
             ORDER BY at DESC, ordinal DESC LIMIT ?4"
        ))?;
        let mut rows = query
            .query_map(params![uuid, boundary.0, boundary.1, (limit + 1) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        rows.reverse();
        // Cached JSON is served only as the index wrote it: objects, the
        // header for this task (its uuid and indexed short ID) and each entry
        // at its indexed time. Anything else is a damaged index.
        let object = |raw: &str| {
            serde_json::from_str::<Value>(raw)
                .ok()
                .filter(Value::is_object)
                .ok_or(Failure::Damaged)
        };
        let mut entries = Vec::with_capacity(rows.len());
        for (id, raw, at) in &rows {
            let mut entry = object(raw)?;
            if entry["at"].as_i64() != Some(*at) {
                return Err(Failure::Damaged);
            }
            if summary {
                entry["id"] = json!(format!("{prefix}{id}"));
            }
            entries.push(entry);
        }
        let header = object(&header)?;
        let short_id: Option<String> = self
            .db
            .query_row("SELECT short_id FROM tasks WHERE uuid=?1", [uuid], |r| r.get(0))
            .optional()?
            .flatten();
        if header["uuid"].as_str() != Some(uuid) || short_id.is_none() || header["id"].as_str() != short_id.as_deref() {
            return Err(Failure::Damaged);
        }
        let next = more.then(|| rows.first().map(|(id, ..)| format!("{prefix}{id}"))).flatten();
        Ok(Some(super::history::finish(header, entries, total as usize, next)))
    }
}

#[cfg(test)]
mod tests;
