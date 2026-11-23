//! WhiteWh fork: durable team state. One private SQLite file holds every team
//! (live JSON with a revision and indexed task archives), the feed and the
//! idempotent operations, opened as the coordination waits are.
use super::super::*;
use super::model::{Team, Task, ArchivedTask, ArchivedPhase, Phase, PhaseState, Prerequisite};
use anyhow::{bail, ensure, Context};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{
    fs,
    time::{Duration, Instant},
};

/// Task 288: a team's tasks and attempts in their own rows.
mod rows;

/// Feed entries per page.
pub(crate) const PAGE: i64 = 50;
const STORE_PAGE_CAP: i64 = 262144;
const ARCHIVED_TASK_WINDOW: usize = 1000;
const ARCHIVED_PHASE_WINDOW: usize = 200;

pub(crate) fn path() -> PathBuf {
    root().join("teams/teams.sqlite3")
}

/// Task 8: the heads of the teams at `path` (see [`Store::heads`]); no store
/// yet is no team.
pub(crate) fn heads_at(path: &Path, limit: usize) -> anyhow::Result<(Vec<Value>, bool)> {
    if !path.exists() {
        return Ok((Vec::new(), false));
    }
    Store::open_at(path, false)?.heads(limit)
}

/// Task 8: the files whose stat changes when a team changes: the store, its
/// rollback journal and (task 105) its write-ahead log, where a WAL store's
/// writes land until a checkpoint copies them into the store.
pub(crate) fn files(path: &Path) -> Vec<PathBuf> {
    let beside = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    vec![path.to_path_buf(), beside("-journal"), beside("-wal")]
}

/// Task 105: how the store keeps its transactions. WAL lets every reader
/// (`hgs team list`, the GUI's polls, status) read the last committed state
/// while a writer works; the rollback journal (DELETE) made them wait behind
/// a long write, and a write behind a long read, up to "database is locked".
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Journal {
    Wal,
    /// The mode before task 105, kept for measurements and as the fallback
    /// where SQLite refuses WAL (a network folder).
    #[cfg(test)]
    Delete,
}

/// Task 105: how long a write open waits to switch the journal mode.
const SWITCH_WAIT: Duration = Duration::from_secs(2);

/// Task 105: a WAL store's log is truncated to this size after a checkpoint.
/// A team's record is megabytes of JSON, so a smaller log would be cut and
/// grown again on almost every write.
const WAL_KEEP_BYTES: i64 = 64 * 1024 * 1024;
/// Task 105: pages written before a commit checkpoints the log into the store
/// (SQLite's default is 1000, about 4 MB: one write of a large team). Measured
/// on a copy of team 33fbafeb's store, fewer checkpoints kept the writes'
/// p95 lower and the reads' steadier.
const WAL_CHECKPOINT_PAGES: i64 = 4000;

/// Task 105: the `-wal` and `-shm` files stay when the last connection closes.
/// A reader whose sandbox may not create files beside the store (a read-only
/// sandbox) can then still open it read-only: SQLite reads a WAL database
/// without write access only when those files exist.
fn persist_wal(db: &Connection) {
    let mut on: std::os::raw::c_int = 1;
    // SAFETY: the handle belongs to this open connection for the call; the
    // argument is the documented int pointer of SQLITE_FCNTL_PERSIST_WAL.
    unsafe {
        rusqlite::ffi::sqlite3_file_control(
            db.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut on as *mut std::os::raw::c_int).cast(),
        );
    }
}

/// One feed entry to append with a mutation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub kind: &'static str,
    pub payload: Value,
}

impl Event {
    pub(crate) fn new(kind: &'static str, payload: Value) -> Self {
        Self { kind, payload }
    }
}

pub(crate) struct Store {
    db: Connection,
    /// Task 287: the worker's changes to one team in this tick, written in one
    /// transaction at its end (`begin_batch`, `end_batch`).
    batch: Option<Batch>,
    /// Task 288: the teams this store read or wrote, as of their revision, so
    /// a change reads the team before the lock and, under it, only the rows
    /// written since.
    cache: std::cell::RefCell<std::collections::HashMap<String, rows::Snapshot>>,
    /// Task 288: the store has the `layout` column (a read-only open of a
    /// store no hgs of this version has written reads the whole records).
    /// Known at a write open; a read-only open asks at its first read, so the
    /// open itself never waits for a writer.
    has_layout: std::cell::Cell<Option<bool>>,
}

/// Task 288: teams a store keeps a copy of.
const CACHED_TEAMS: usize = 8;

/// Task 287: a team's working copy while the worker ticks it. Each change is
/// applied to the copy at once (its closure runs once, as before); the copy
/// is written in one transaction at the end of the team's steps. A record
/// another writer changed meanwhile is merged (`merge3`): their changes stay,
/// the worker's are applied over them.
struct Batch {
    team: String,
    base_revision: u64,
    base: Team,
    working: Team,
    events: Vec<(f64, Vec<Event>)>,
    changed: bool,
}

impl Batch {
    /// Task #6 of team c536f0e5 (P1.4, owner 2026-11-23): whether the batch
    /// has anything to write: an event, or a team that differs from the copy
    /// it started from in more than `updated`. A tick whose steps changed
    /// nothing writes nothing and keeps the revision and `updated`.
    fn dirty(&mut self) -> bool {
        if !self.changed {
            return false;
        }
        if self.events.iter().any(|(_, events)| !events.is_empty()) {
            return true;
        }
        let updated = std::mem::replace(&mut self.working.updated, self.base.updated);
        let same = self.working == self.base;
        self.working.updated = updated;
        !same
    }
}

/// Task 287: a write transaction whose lock hold is logged (`Hold`).
pub(crate) struct Write<'a> {
    transaction: rusqlite::Transaction<'a>,
    // Dropped after the transaction: a rollback is inside the measured hold.
    hold: Hold,
}

impl<'a> std::ops::Deref for Write<'a> {
    type Target = rusqlite::Transaction<'a>;
    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

/// Task 287: a transaction's write lock, from BEGIN IMMEDIATE to its COMMIT or
/// rollback, with the code that took it. Every hold is one line of
/// `<state>/logs/team-store-locks.log`; one of `SLOW_HOLD_MS` or more is a
/// warning, and also a line of the slow-calls log.
struct Hold {
    started: Instant,
    waited_ms: u64,
    caller: &'static std::panic::Location<'static>,
    team: String,
    outcome: &'static str,
}

/// Task 287: a lock held this long is a warning.
pub(crate) const SLOW_HOLD_MS: u64 = 200;

thread_local! {
    /// Task 287: the code holding this thread's write lock now.
    static HOLDER: std::cell::Cell<Option<&'static std::panic::Location<'static>>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Tests: the lock lines and the processes started under a held lock.
    static LOCK_LINES: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_lock_lines() -> Vec<Value> {
    LOCK_LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
}

impl Hold {
    fn start(caller: &'static std::panic::Location<'static>, team: &str, waited: Duration) -> Self {
        HOLDER.with(|holder| holder.set(Some(caller)));
        Self { started: Instant::now(), waited_ms: waited.as_millis() as u64, caller, team: team.to_owned(), outcome: "rollback" }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        HOLDER.with(|holder| holder.set(None));
        let held_ms = self.started.elapsed().as_millis() as u64;
        let level = if held_ms >= SLOW_HOLD_MS { "warn" } else { "info" };
        lock_line(json!({"at":now(),"pid":std::process::id(),"level":level,"caller":format!("{}:{}", self.caller.file(), self.caller.line()),
            "team":self.team,"held_ms":held_ms,"waited_ms":self.waited_ms,"outcome":self.outcome}));
    }
}

/// Task 287: the code holding this thread's write lock, if one is held.
pub(crate) fn lock_holder() -> Option<&'static std::panic::Location<'static>> {
    HOLDER.with(std::cell::Cell::get)
}

/// Task 287: a process about to start (`program` with `args`): one started
/// while this thread holds the write lock is a warning line naming both, so
/// a git call that slipped into a transaction is found.
pub(crate) fn note_process(program: &str, args: &[String]) {
    if let Some(holder) = lock_holder() {
        lock_line(json!({"at":now(),"pid":std::process::id(),"level":"warn","caller":format!("{}:{}", holder.file(), holder.line()),
            "process":program,"args":args.iter().take(8).collect::<Vec<_>>(),"note":"a process started while the team store's write lock was held"}));
    }
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn lock_log_path() -> PathBuf {
    root().join("logs").join("team-store-locks.log")
}

fn lock_line(line: Value) {
    #[cfg(test)]
    LOCK_LINES.with(|lines| lines.borrow_mut().push(line));
    #[cfg(not(test))]
    {
        let text = line.to_string();
        let _ = super::super::slow_calls::append(&lock_log_path(), &text);
        if line["level"] == "warn" {
            let _ = super::super::slow_calls::append(&super::super::slow_calls::log_path(), &text);
        }
    }
}

/// The stable identity of a request's content, for idempotent replays.
pub(crate) fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

pub(crate) fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

/// Task 25: how long one SQLite call waits for the lock another process holds
/// (a reader in rollback-journal mode holds it until its read ends).
const BUSY_WAIT: Duration = Duration::from_secs(30);
/// Task 25: how long a team write as a whole keeps waiting for the database:
/// SQLite answers some contention at once, without its busy wait.
const WRITE_WAIT: Duration = Duration::from_secs(60);

/// Task 25: the database is held by another connection right now.
fn busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(failure, _)
        if matches!(failure.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

/// Task 25: a write that still found the database busy after `WRITE_WAIT`
/// changed nothing; the same request id applies it once when run again.
fn busy_error(error: rusqlite::Error) -> anyhow::Error {
    if busy(&error) {
        anyhow::anyhow!(
            "The team database stayed busy for {} s (another Zerus process was writing or reading it); nothing was changed. \
             Run the same command again with the same request_id: it is applied once.",
            WRITE_WAIT.as_secs()
        )
    } else {
        error.into()
    }
}

/// Task 25: starts a write transaction, waiting while another connection
/// (the dispatcher, a member's command, a status read) holds the database.
/// Team 33fbafeb: `hgs team reassign` failed with "database is locked" after
/// the 2 s busy wait while the dispatcher wrote.
fn begin<'a>(db: &'a Connection, caller: &'static std::panic::Location<'static>, team: &str) -> anyhow::Result<Write<'a>> {
    let asked = Instant::now();
    let deadline = asked + write_wait();
    loop {
        match rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate) {
            Ok(transaction) => return Ok(Write { transaction, hold: Hold::start(caller, team, asked.elapsed()) }),
            Err(error) if busy(&error) && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(busy_error(error)),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: a shorter wait for a write than `WRITE_WAIT`.
    static TEST_WRITE_WAIT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_write_wait(wait: Option<Duration>) {
    TEST_WRITE_WAIT.with(|found| found.set(wait));
}

/// SQLite's own wait for one statement; a test with a shorter write wait
/// waits no longer.
fn busy_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_WRITE_WAIT.with(std::cell::Cell::get) {
        return wait;
    }
    BUSY_WAIT
}

fn write_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_WRITE_WAIT.with(std::cell::Cell::get) {
        return wait;
    }
    WRITE_WAIT
}

/// Task 25: commits, waiting for readers to finish. A COMMIT that fails with
/// SQLITE_BUSY leaves the transaction open, so it is tried again; any other
/// failure (or the deadline) rolls the whole change back.
fn commit(mut write: Write<'_>) -> anyhow::Result<()> {
    let deadline = Instant::now() + write_wait();
    loop {
        match write.transaction.execute_batch("COMMIT") {
            // The connection is back in autocommit: dropping the
            // transaction does nothing more.
            Ok(()) => {
                write.hold.outcome = "commit";
                // Task #393: a write that leaves a big log behind checkpoints
                // it into the store and truncates the file, so the next read
                // opens a small WAL instead of a 64 MB one.
                checkpoint_large_wal(&write.transaction);
                return Ok(());
            }
            Err(error) if busy(&error) && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(busy_error(error)),
        }
    }
}

/// Task #393 (owner, 2026-11-08): after a large write the WAL is checkpointed
/// and, when the file is big, truncated. A reader may hold the log: PASSIVE
/// copies what it can, and TRUNCATE only resets the file when no reader stands
/// in the way (a busy outcome leaves it for a later write).
fn checkpoint_large_wal(db: &Connection) {
    const LARGE: u64 = 8 * 1024 * 1024;
    let Ok(path) = db.query_row("SELECT file FROM pragma_database_list WHERE name='main'", [], |row| row.get::<_, String>(0)) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    let wal = std::path::PathBuf::from(format!("{path}-wal"));
    if !std::fs::metadata(&wal).map(|meta| meta.len() >= LARGE).unwrap_or(false) {
        return;
    }
    let _ = db.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    if std::fs::metadata(&wal).map(|meta| meta.len() >= LARGE).unwrap_or(false) {
        let _ = db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

/// U20: a member's write the sandbox blocks says what to do, not a bare SQLite
/// error. Everything else is passed through unchanged.
pub(crate) fn write_error(error: impl std::fmt::Display) -> anyhow::Error {
    let text = error.to_string();
    if text.to_ascii_lowercase().contains("database or disk is full") {
        return anyhow::anyhow!(
            "teams.sqlite3 is full (the store cap is 1 GiB / {STORE_PAGE_CAP} pages, or disk space \
             is unavailable); no changes were saved. The existing team record remains readable."
        );
    }
    if text.contains("readonly database") || text.contains("read-only database") {
        return anyhow::anyhow!(
            "the sandbox blocks the team store: your own hgs team report, ask, run, commit and your scratch team's calls (under the owner's standing rule) queue in your folder by themselves; anything else asks the lead"
        );
    }
    anyhow::anyhow!(text)
}

/// A member in a sandbox runs at Low integrity (DeepSeek's confined shell),
/// which may not write files of the usual Medium label: its `hgs team
/// report` could not record the report. The team folder and what it holds
/// take the Low label once (what is created later inherits it); the folder
/// keeps its owner and permissions. Failing to set it leaves reports from
/// sandboxes to the member's last reply.
#[cfg(windows)]
fn sandbox_writable(dir: &Path) {
    use std::os::windows::process::CommandExt;
    let marker = dir.join(".sandbox-writable");
    if marker.exists() {
        return;
    }
    let done = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/setintegritylevel", "(OI)(CI)L", "/T", "/C", "/Q"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .is_ok_and(|status| status.success());
    if done {
        let _ = fs::write(&marker, b"");
    }
}

/// Task 287: the member projection's trigger parsed the whole team record
/// (megabytes) with json_each on every write. The record's members are also
/// kept in their own column, written with `members_rev` (the revision that
/// wrote them); the trigger runs only when they changed. A write by an older
/// hgs, which sets neither, leaves `members_rev` behind the revision, and the
/// trigger then reads the members from the record as before.
const MEMBERS_CHANGED: &str =
    "NEW.members_rev IS NOT NEW.revision OR OLD.members_rev IS NOT OLD.revision OR NEW.members IS NOT OLD.members";
const MEMBERS_SOURCE: &str =
    "CASE WHEN NEW.members_rev IS NEW.revision THEN NEW.members ELSE json_extract(NEW.payload,'$.members') END";

/// Task 287: adds the members columns and replaces a projection trigger that
/// runs on every write, once, in one transaction.
fn members_column(db: &Connection) -> anyhow::Result<()> {
    let has_teams: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='teams')", [], |row| row.get(0))?;
    if !has_teams {
        return Ok(());
    }
    let columns: Vec<String> = db.prepare("SELECT name FROM pragma_table_info('teams')")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    let trigger: Option<String> = db.query_row("SELECT sql FROM sqlite_master WHERE name='team_rules_update'", [], |row| row.get(0)).optional()?;
    let old_trigger = trigger.as_deref().is_some_and(|sql| !sql.contains("members_rev"));
    let missing = !columns.iter().any(|name| name == "members") || !columns.iter().any(|name| name == "members_rev");
    if !missing && !old_trigger {
        return Ok(());
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    let done = (|| -> rusqlite::Result<()> {
        let columns: Vec<String> = db.prepare("SELECT name FROM pragma_table_info('teams')")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|name| name == "members") {
            db.execute_batch("ALTER TABLE teams ADD COLUMN members TEXT")?;
        }
        if !columns.iter().any(|name| name == "members_rev") {
            db.execute_batch("ALTER TABLE teams ADD COLUMN members_rev INTEGER")?;
        }
        // Re-created below by `open`, with its WHEN.
        let trigger: Option<String> = db.query_row("SELECT sql FROM sqlite_master WHERE name='team_rules_update'", [], |row| row.get(0)).optional()?;
        if trigger.as_deref().is_some_and(|sql| !sql.contains("members_rev")) {
            db.execute_batch("DROP TRIGGER team_rules_update")?;
        }
        Ok(())
    })();
    match done {
        Ok(()) => db.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK");
            return Err(error.into());
        }
    }
    Ok(())
}

impl Drop for Store {
    /// Task 287: a batch left open (a tick that returned early) is written.
    fn drop(&mut self) {
        if self.batch.is_some() {
            if let Err(error) = self.end_batch() {
                eprintln!("hgs team store: the worker's batch was not written: {error:#}");
            }
        }
    }
}

impl Store {

    pub(crate) fn open(write: bool) -> anyhow::Result<Self> {
        Self::open_at(&path(), write).map_err(write_error)
    }

    /// Task 117: a read on a hot path (every listing and inspection) waits
    /// at most `wait` for a writer instead of the usual 10 s.
    pub(crate) fn busy_timeout(&self, wait: Duration) -> anyhow::Result<()> {
        Ok(self.db.busy_timeout(wait)?)
    }

    pub(crate) fn open_at(path: &Path, write: bool) -> anyhow::Result<Self> {
        Self::open_journal(path, write, Journal::Wal)
    }

    /// The store's journal mode now: `wal` or `delete`.
    #[cfg(test)]
    pub(crate) fn journal_mode(&self) -> anyhow::Result<String> {
        Ok(self.db.query_row("PRAGMA journal_mode", [], |row| row.get(0))?)
    }

    /// Opens the store; a write open puts it in `journal` mode. Task 105: an
    /// existing rollback-journal store becomes WAL at its first write open
    /// (SQLite waits for the other connections to finish their transactions);
    /// where SQLite refuses (a busy store, a network folder) it stays as it
    /// was and the next write open tries again. An older hgs reads a WAL store
    /// as it is, and its write open turns it back to DELETE when no other
    /// connection has it open.
    pub(crate) fn open_journal(path: &Path, write: bool, journal: Journal) -> anyhow::Result<Self> {
        if let Ok(meta) = path.symlink_metadata() {
            ensure!(meta.is_file() && !meta.file_type().is_symlink(), "Nonregular team store");
        }
        if write {
            private_dir(path.parent().context("Missing team store parent")?).map_err(anyhow::Error::msg)?;
            if !path.exists() {
                crate::platform::private_open(fs::OpenOptions::new().write(true).create_new(true)).open(path)?;
            }
            #[cfg(windows)]
            sandbox_writable(path.parent().context("Missing team store parent")?);
        } else if !path.exists() {
            bail!("No teams yet");
        }
        let db = Connection::open_with_flags(
            path,
            if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY },
        )?;
        // Task 25: a write waits for the lock (it used to give up after 2 s);
        // a read waits less.
        db.busy_timeout(if write { busy_wait() } else { Duration::from_secs(10) })?;
        // Task #28: a sorted read (`team list`, `team current`) spilled to a
        // temporary file in %TEMP%, which a sandboxed member at Low integrity
        // may not create: "unable to open database file". Sorts stay in
        // memory; every such read is bounded by its LIMIT.
        db.pragma_update(None, "temp_store", "MEMORY")?;
        if write {
            db.pragma_update(None, "max_page_count", STORE_PAGE_CAP)?;
            let wanted = match journal {
                Journal::Wal => "WAL",
                #[cfg(test)]
                Journal::Delete => "DELETE",
            };
            // The pragma answers the mode it kept; a refusal keeps the old one.
            // Only a switch takes the store's lock, so it waits briefly: a busy
            // store is switched by a later open instead of delaying this one.
            let current: String = db.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap_or_default();
            let mode = if current.eq_ignore_ascii_case(wanted) {
                current
            } else {
                db.busy_timeout(SWITCH_WAIT)?;
                let mode = db.query_row(&format!("PRAGMA journal_mode={wanted}"), [], |row| row.get(0)).unwrap_or(current);
                db.busy_timeout(busy_wait())?;
                mode
            };
            if mode.eq_ignore_ascii_case("wal") {
                persist_wal(&db);
                db.pragma_update(None, "journal_size_limit", WAL_KEEP_BYTES)?;
                db.pragma_update(None, "wal_autocheckpoint", WAL_CHECKPOINT_PAGES)?;
            }
            db.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS teams(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,status TEXT NOT NULL,
                    updated REAL NOT NULL,payload TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS teams_updated ON teams(status,updated);
                CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,team TEXT NOT NULL,at REAL NOT NULL,
                    kind TEXT NOT NULL,payload TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS events_team ON events(team,seq);
                CREATE INDEX IF NOT EXISTS events_team_kind_at ON events(team,kind,at);
                CREATE TABLE IF NOT EXISTS team_task_archive(team TEXT NOT NULL,task INTEGER NOT NULL,
                    phase INTEGER NOT NULL,status TEXT NOT NULL,stub TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(team,task));
                CREATE INDEX IF NOT EXISTS team_task_archive_phase ON team_task_archive(team,phase,task);
                CREATE TABLE IF NOT EXISTS team_phase_archive(team TEXT NOT NULL,phase INTEGER NOT NULL,header TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(team,phase));
                CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY,digest TEXT NOT NULL,receipt TEXT NOT NULL,at REAL NOT NULL);
                -- Task #469: the store's one-time data backfills, named.
                CREATE TABLE IF NOT EXISTS store_migrations(name TEXT PRIMARY KEY,at REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS working_pulses(id TEXT PRIMARY KEY,team TEXT NOT NULL,member TEXT NOT NULL,
                    task INTEGER NOT NULL,done TEXT NOT NULL,next TEXT NOT NULL,blocked TEXT NOT NULL,at REAL NOT NULL,
                    source TEXT NOT NULL,expires REAL);
                CREATE INDEX IF NOT EXISTS working_pulses_member ON working_pulses(team,member,task,at);
                CREATE INDEX IF NOT EXISTS working_pulses_team_at ON working_pulses(team,at);
                -- Task #246: the owner's dismissed Waiting rows, with the state seen.
                CREATE TABLE IF NOT EXISTS dismissed(id TEXT PRIMARY KEY,team TEXT NOT NULL,state TEXT NOT NULL,
                    at REAL NOT NULL,by TEXT NOT NULL);",
            )?;
            // Task 134: the working tier's team messages (docs/lore.md 11.5).
            db.execute_batch(super::working::SCHEMA)?;
        }
        if write {
            members_column(&db)?;
            rows::ensure_schema(&db)?;
        }
        if write {
            // Build an indexed projection once; triggers keep it in the same
            // transaction as every team write, including dispatcher mutations.
            let mut exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='team_rule_members')",
                [],
                |row| row.get(0),
            )?;
            let old_projection = db.query_row("SELECT sql FROM sqlite_master WHERE name='team_rules_insert'", [], |row| row.get::<_,String>(0))
                .optional()?.is_some_and(|sql| !sql.contains("NULLIF"));
            let migration = !exists || old_projection;
            if migration { db.execute_batch("BEGIN IMMEDIATE")?; }
            if old_projection {
                db.execute_batch("DROP TRIGGER IF EXISTS team_rules_insert; DROP TRIGGER IF EXISTS team_rules_update; DROP TRIGGER IF EXISTS team_rules_delete; DELETE FROM team_rule_members;")?;
                exists = false;
            }
            db.execute_batch(&"CREATE TABLE IF NOT EXISTS team_rule_members(team TEXT NOT NULL,member TEXT NOT NULL,host TEXT NOT NULL,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,launch TEXT NOT NULL,PRIMARY KEY(team,member));
                CREATE INDEX IF NOT EXISTS team_rule_session ON team_rule_members(host,name,run,conversation);
                CREATE INDEX IF NOT EXISTS team_rule_launch ON team_rule_members(launch);
                CREATE TRIGGER IF NOT EXISTS team_rules_delete AFTER DELETE ON teams BEGIN DELETE FROM team_rule_members WHERE team=OLD.id; END;
                CREATE TRIGGER IF NOT EXISTS team_rules_update AFTER UPDATE OF payload ON teams WHEN {MEMBERS_CHANGED} BEGIN
                    DELETE FROM team_rule_members WHERE team=NEW.id;
                    INSERT INTO team_rule_members SELECT NEW.id,json_extract(m.value,'$.id'),COALESCE(NULLIF(json_extract(m.value,'$.session.host'),''),json_extract(m.value,'$.computer'),''),COALESCE(json_extract(m.value,'$.session.name'),''),COALESCE(json_extract(m.value,'$.session.run_id'),''),COALESCE(json_extract(m.value,'$.session.conversation_id'),''),COALESCE(json_extract(m.value,'$.launch_id'),'') FROM json_each({MEMBERS_SOURCE}) m; END;
                CREATE TRIGGER IF NOT EXISTS team_rules_insert AFTER INSERT ON teams BEGIN
                    INSERT INTO team_rule_members SELECT NEW.id,json_extract(m.value,'$.id'),COALESCE(NULLIF(json_extract(m.value,'$.session.host'),''),json_extract(m.value,'$.computer'),''),COALESCE(json_extract(m.value,'$.session.name'),''),COALESCE(json_extract(m.value,'$.session.run_id'),''),COALESCE(json_extract(m.value,'$.session.conversation_id'),''),COALESCE(json_extract(m.value,'$.launch_id'),'') FROM json_each(NEW.payload,'$.members') m; END;".replace("{MEMBERS_CHANGED}", MEMBERS_CHANGED).replace("{MEMBERS_SOURCE}", MEMBERS_SOURCE))?;
            if !exists {
                db.execute_batch("INSERT OR IGNORE INTO team_rule_members SELECT t.id,json_extract(m.value,'$.id'),COALESCE(NULLIF(json_extract(m.value,'$.session.host'),''),json_extract(m.value,'$.computer'),''),COALESCE(json_extract(m.value,'$.session.name'),''),COALESCE(json_extract(m.value,'$.session.run_id'),''),COALESCE(json_extract(m.value,'$.session.conversation_id'),''),COALESCE(json_extract(m.value,'$.launch_id'),'') FROM teams t,json_each(t.payload,'$.members') m;")?;
            }
            if migration { db.execute_batch("COMMIT")?; }
        }
        let has_layout = std::cell::Cell::new(write.then_some(true));
        if write {
            // Task 288: every team not yet in rows, one transaction each.
            rows::migrate(&db)?;
            // Task #469: archived stubs take their dependencies from the
            // archive's payload, once; later opens find the marker.
            backfill_archive_stubs(&db)?;
        }
        Ok(Self { db, batch: None, cache: std::cell::RefCell::new(std::collections::HashMap::new()), has_layout })
    }

    /// Task #246: the owner's dismissed Waiting rows: id, team, the state at
    /// dismissal and when. A missing table (an older store read before its next
    /// write open) is no dismissal.
    pub(crate) fn dismissed(&self) -> anyhow::Result<Vec<Value>> {
        let mut statement = self.db.prepare("SELECT id,team,state,at,by FROM dismissed ORDER BY at DESC")?;
        let rows = statement.query_map([], |row| {
            Ok(json!({"id":row.get::<_,String>(0)?,"team":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,
                "at":row.get::<_,f64>(3)?,"by":row.get::<_,String>(4)?}))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Task #246: dismiss a Waiting item; the owner's (the caller checks).
    pub(crate) fn dismiss(&self, id: &str, team: &str, state: &str, at: f64, by: &str) -> anyhow::Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO dismissed(id,team,state,at,by) VALUES(?1,?2,?3,?4,?5)",
            params![id, team, state, at, by],
        )?;
        Ok(())
    }

    /// Task #246: Show dismissed > Restore; false when nothing was dismissed.
    pub(crate) fn undismiss(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.db.execute("DELETE FROM dismissed WHERE id=?1", [id])? > 0)
    }

    /// Task 133: writes a member's pulse (docs/lore.md 11.2, 11.5). One by the
    /// same member and task within 5 minutes replaces it in place; the team
    /// keeps at most 2,000, the oldest that are not a member's latest go.
    pub(crate) fn put_pulse(&self, pulse: &super::pulses::Pulse) -> anyhow::Result<super::pulses::Pulse> {
        use super::pulses::{PER_TEAM, REPLACE_WITHIN};
        let mut pulse = pulse.clone();
        // Task 184: a pulse is working-tier text (docs/lore.md 11.9): a token in
        // a command line or a note is stored redacted, never refused, and the
        // replacement is a visible `[redacted: <pattern>]` marker.
        pulse.done = super::secrets::redact_working(&pulse.done);
        pulse.next = super::secrets::redact_working(&pulse.next);
        pulse.blocked = super::secrets::redact_working(&pulse.blocked);
        let previous: Option<String> = self
            .db
            .query_row(
                "SELECT id FROM working_pulses WHERE team=?1 AND member=?2 AND task=?3 AND at>=?4 ORDER BY at DESC LIMIT 1",
                params![pulse.team, pulse.member, pulse.task, pulse.at - REPLACE_WITHIN],
                |row| row.get(0),
            )
            .optional()?;
        match previous {
            Some(id) => {
                pulse.id = id;
                self.db.execute(
                    "UPDATE working_pulses SET done=?1,next=?2,blocked=?3,at=?4,source=?5 WHERE id=?6",
                    params![pulse.done, pulse.next, pulse.blocked, pulse.at, pulse.source, pulse.id],
                )?;
            }
            None => {
                if pulse.id.is_empty() {
                    pulse.id = format!("w-pulse-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                }
                self.db.execute(
                    "INSERT INTO working_pulses(id,team,member,task,done,next,blocked,at,source,expires) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL)",
                    params![pulse.id, pulse.team, pulse.member, pulse.task, pulse.done, pulse.next, pulse.blocked, pulse.at, pulse.source],
                )?;
            }
        }
        // The bound: the oldest rows beyond 2,000 that are no member's latest.
        self.db.execute(
            "DELETE FROM working_pulses WHERE id IN (SELECT id FROM working_pulses AS p WHERE team=?1
                AND at < (SELECT MAX(at) FROM working_pulses AS q WHERE q.team=p.team AND q.member=p.member AND q.task=p.task)
                ORDER BY at LIMIT MAX(0, (SELECT COUNT(*) FROM working_pulses WHERE team=?1) - ?2))",
            params![pulse.team, PER_TEAM],
        )?;
        Ok(pulse)
    }

    /// Task 133: the closed tasks' pulses expire 14 days after the close and
    /// a finished team's after its finish (11.3); at most 500 rows a call.
    /// Task 187: a task decided at a known time expires from that time; a
    /// task closed without one (cancelled before any hand-out) and an open
    /// task of a finished team expire from the first pass that sees them
    /// closed, a time that never moves again (never `team.updated`, which every
    /// change moves, so a finished team never expired); a reopened task's
    /// pulses lose their expiry, so they are never deleted while it is open.
    pub(crate) fn prune_pulses(&self, team: &Team, now: f64) -> anyhow::Result<usize> {
        use super::pulses::{closed_at, KEEP_SECONDS, PRUNE_ROWS};
        let finished = matches!(team.status, super::model::TeamStatus::Done | super::model::TeamStatus::Stopped);
        let mut changed = 0;
        for task in &team.tasks {
            match closed_at(task) {
                Some(closed) => {
                    changed += self.db.execute(
                        "UPDATE working_pulses SET expires=?1 WHERE team=?2 AND task=?3 AND (expires IS NULL OR expires<>?1)",
                        params![closed + KEEP_SECONDS, team.id, task.id],
                    )?;
                }
                None if finished || task.status.finished() => {
                    changed += self.db.execute(
                        "UPDATE working_pulses SET expires=?1 WHERE team=?2 AND task=?3 AND expires IS NULL",
                        params![now + KEEP_SECONDS, team.id, task.id],
                    )?;
                }
                None => {
                    changed += self.db.execute(
                        "UPDATE working_pulses SET expires=NULL WHERE team=?1 AND task=?2 AND expires IS NOT NULL",
                        params![team.id, task.id],
                    )?;
                }
            }
        }
        let removed = self.db.execute(
            "DELETE FROM working_pulses WHERE id IN (SELECT id FROM working_pulses WHERE team=?1 AND expires IS NOT NULL AND expires<=?2 LIMIT ?3)",
            params![team.id, now, PRUNE_ROWS],
        )?;
        let _ = changed; // expiries set; only removals count
        Ok(removed)
    }

    fn pulse_rows(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<super::pulses::Pulse> {
        let run = || -> rusqlite::Result<Vec<super::pulses::Pulse>> {
            let mut statement = self.db.prepare(sql)?;
            let rows = statement.query_map(args, |row| {
                Ok(super::pulses::Pulse {
                    id: row.get(0)?,
                    team: row.get(1)?,
                    member: row.get(2)?,
                    task: row.get(3)?,
                    done: row.get(4)?,
                    next: row.get(5)?,
                    blocked: row.get(6)?,
                    at: row.get(7)?,
                    source: row.get(8)?,
                })
            })?;
            rows.collect()
        };
        // A store written before task 133 has no table: no pulses yet.
        run().unwrap_or_default()
    }

    /// Task 133: the latest pulse of each member, newest first, unexpired.
    pub(crate) fn latest_pulses(&self, team: &str, now: f64) -> Vec<super::pulses::Pulse> {
        // Task #393 (owner, 2026-11-08): one ordered scan, newest first, then
        // the newest row of each member. The correlated subquery read the table
        // once per member and made a status wait seconds on a large store.
        let rows = self.pulse_rows(
            "SELECT id,team,member,task,done,next,blocked,at,source FROM working_pulses
                WHERE team=?1 AND (expires IS NULL OR expires>?2) ORDER BY at DESC LIMIT 2000",
            &[&team, &now],
        );
        let mut seen = std::collections::HashSet::new();
        rows.into_iter().filter(|pulse| seen.insert(pulse.member.clone())).collect()
    }

    /// Task 133: a team's pulses, newest first, of one member or all.
    pub(crate) fn pulses(&self, team: &str, member: Option<&str>, now: f64, limit: i64) -> Vec<super::pulses::Pulse> {
        self.pulse_rows(
            "SELECT id,team,member,task,done,next,blocked,at,source FROM working_pulses WHERE team=?1
                AND (?2='' OR member=?2) AND (expires IS NULL OR expires>?3) ORDER BY at DESC LIMIT ?4",
            &[&team, &member.unwrap_or_default(), &now, &limit.clamp(1, 500)],
        )
    }

    pub(crate) fn load(&self, id: &str) -> anyhow::Result<Option<Team>> {
        // Task 287: the worker's own changes of this tick are seen by its next step.
        if let Some(batch) = self.batch.as_ref().filter(|batch| batch.team == id) {
            return Ok(Some(batch.working.clone()));
        }
        self.resolved(self.current(id)?)
    }

    /// Task 288: the team as stored now, from this store's copy brought up to
    /// date (only the rows written since its revision are read), else read
    /// whole and kept.
    /// Whether the store has the `layout` column (see `has_layout`).
    fn layout(&self) -> anyhow::Result<bool> {
        if let Some(known) = self.has_layout.get() {
            return Ok(known);
        }
        let known: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('teams') WHERE name='layout')", [], |row| row.get(0))?;
        self.has_layout.set(Some(known));
        Ok(known)
    }

    fn current(&self, id: &str) -> anyhow::Result<Option<Team>> {
        if !self.layout()? {
            let payload: Option<String> =
                self.db.query_row("SELECT payload FROM teams WHERE id=?1", params![id], |row| row.get(0)).optional()?;
            return payload.map(|text| serde_json::from_str(&text).context("Invalid team record")).transpose();
        }
        let mut cache = self.cache.borrow_mut();
        if let Some(snapshot) = cache.get_mut(id) {
            if rows::refresh(&self.db, id, snapshot)? {
                return Ok(Some(snapshot.team.clone()));
            }
            cache.remove(id);
            return Ok(None);
        }
        let Some(snapshot) = rows::read(&self.db, id)? else { return Ok(None) };
        let team = snapshot.team.clone();
        if cache.len() >= CACHED_TEAMS {
            cache.clear();
        }
        cache.insert(id.to_owned(), snapshot);
        Ok(Some(team))
    }

    /// Task 288: keeps the copy of a team this store just wrote.
    fn remember(&self, id: &str, team: Team, saved: rows::Saved) {
        let mut cache = self.cache.borrow_mut();
        let base = cache.remove(id);
        // Task #5 (team ede724c4): a write that accepted a task records it in
        // the estimate history (after the commit, outside the lock); a status
        // read no longer does.
        if super::estimates::newly_accepted(base.as_ref().map(|base| &base.team), &team) {
            let _ = super::estimates::record_accepted(&team, now());
        }
        if cache.len() >= CACHED_TEAMS {
            cache.clear();
        }
        cache.insert(id.to_owned(), rows::after(base, team, saved));
    }

    /// Task 288: a team as a list query read its row: assembled from its rows.
    fn decoded(&self, rows: Vec<(String, i64, f64, Option<i64>, String)>) -> anyhow::Result<Vec<Team>> {
        rows.into_iter()
            .map(|(id, revision, updated, layout, payload)| {
                let mut team = rows::decode(&self.db, &id, revision, updated, layout, &payload)?;
                resolve_dependencies(&self.db, &mut team, &Value::Null)?;
                Ok(team)
            })
            .collect()
    }

    /// A team read from the store with the archived tasks its live tasks name
    /// (dependencies, prerequisites, origins) resolved from the archive.
    fn resolved(&self, team: Option<Team>) -> anyhow::Result<Option<Team>> {
        let Some(mut team) = team else { return Ok(None) };
        resolve_dependencies(&self.db, &mut team, &Value::Null)?;
        // Task #469: a stub archived before stubs kept their dependencies.
        restore_stub_deps(&self.db, &mut team)?;
        Ok(Some(team))
    }

    /// One live task as its rows (or the whole record) hold it, selected
    /// before decoding; None when the team has no such live task.
    fn live_task(&self, team: &str, task: u32) -> anyhow::Result<Option<Value>> {
        if self.layout()? {
            let layout: Option<i64> = self.db.query_row("SELECT layout FROM teams WHERE id=?1", params![team], |row| row.get(0)).optional()?.flatten();
            if rows::is_rows(layout) {
                let head: Option<String> = self.db.query_row(
                    "SELECT CASE WHEN length(CAST(payload AS BLOB))<=8388608 THEN payload ELSE NULL END FROM team_tasks WHERE team=?1 AND task=?2",
                    params![team, task], |row| row.get::<_, Option<String>>(0)).optional()?.flatten();
                let Some(head) = head else { return Ok(None) };
                let mut value: Value = serde_json::from_str(&head)?;
                // An archive stub is not the task: its full history is in the archive.
                if !value["archived"].is_null() { return Ok(None); }
                let bytes: i64 = self.db.query_row("SELECT COALESCE(sum(length(CAST(payload AS BLOB))),0) FROM team_attempts WHERE team=?1 AND task=?2", params![team, task], |row| row.get(0))?;
                anyhow::ensure!(bytes <= 8 * 1024 * 1024, "Unknown task or task exceeds the 8 MiB read limit");
                let mut query = self.db.prepare("SELECT payload FROM team_attempts WHERE team=?1 AND task=?2 ORDER BY ord")?;
                let attempts = query
                    .query_map(params![team, task], |row| row.get::<_, String>(0))?
                    .map(|row| Ok(serde_json::from_str::<Value>(&row?)?))
                    .collect::<anyhow::Result<Vec<Value>>>()?;
                value["attempts"] = Value::Array(attempts);
                return Ok(Some(value));
            }
        }
        let live: Option<String> = self.db.query_row(
            "SELECT CASE WHEN length(CAST(j.value AS BLOB))<=8388608 THEN j.value ELSE NULL END FROM teams t,json_each(t.payload,'$.tasks') j WHERE t.id=?1 AND json_extract(j.value,'$.id')=?2 AND json_extract(j.value,'$.archived') IS NULL LIMIT 1",
            params![team, task], |row| row.get::<_, Option<String>>(0)).optional()?.flatten();
        live.map(|payload| Ok(serde_json::from_str(&payload)?)).transpose()
    }

    /// The columns a list query reads for `decoded`.
    fn listed(&self) -> anyhow::Result<&'static str> {
        Ok(if self.layout()? {
            "id,revision,updated,layout,payload"
        } else {
            "id,revision,updated,NULL,payload"
        })
    }

    /// Stable keyset page of frozen task stubs, through the phase index. Task
    /// #469: a stub archived before stubs kept their dependencies takes them
    /// from the full task's payload, so the plan of a done phase keeps its
    /// layers and links.
    pub(crate) fn archived_tasks(&self, team: &str, phase: u32, after: u32, limit: i64) -> anyhow::Result<Value> {
        let limit = limit.clamp(1, PAGE);
        archive_page(&self.db,
            "SELECT CASE WHEN length(CAST(stub AS BLOB))<=65536 THEN
                CASE WHEN COALESCE(json_array_length(stub,'$.deps'),0)=0 AND json_array_length(payload,'$.deps')>0
                  THEN json_set(stub,'$.deps',json(json_extract(payload,'$.deps'))) ELSE stub END
             ELSE NULL END FROM team_task_archive WHERE team=?1 AND phase=?2 AND task>?3 ORDER BY task LIMIT ?4",
            params![team,phase,after,limit+1], limit, "id")
    }

    /// Archived phase headers, including those outside the 200-header window.
    pub(crate) fn archived_phases(&self, team: &str, after: u32, limit: i64) -> anyhow::Result<Value> {
        let limit = limit.clamp(1, PAGE);
        archive_page(&self.db,
            "SELECT CASE WHEN length(CAST(header AS BLOB))<=65536 THEN header ELSE NULL END FROM team_phase_archive WHERE team=?1 AND phase>?2 ORDER BY phase LIMIT ?3",
            params![team,after,limit+1], limit, "n")
    }

    /// One task selected before decoding; oversized history remains intact.
    pub(crate) fn task(&self, team: &str, task: u32) -> anyhow::Result<Value> {
        if let Some(live) = self.live_task(team, task)? {
            return Ok(live);
        }
        let payload = if has_archive(&self.db)? {
            self.db.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))<=8388608 THEN payload ELSE NULL END FROM team_task_archive WHERE team=?1 AND task=?2",
                params![team, task], |row| row.get::<_, Option<String>>(0)).optional()?.flatten()
        } else { None };
        Ok(serde_json::from_str(&payload.context("Unknown task or task exceeds the 8 MiB read limit")?)?)
    }

    /// Startup reads at most two matching records, never the whole roster.
    pub(crate) fn rules_member(&self, record: &Value) -> anyhow::Result<Option<(Team, String)>> {
        self.rules_session(
            "",
            super::super::string(record, "name"),
            super::super::string(record, "run_id"),
            super::super::string(record, "conversation_id"),
            super::super::string(record, "launch_id"),
        )
    }

    pub(crate) fn rules_session(
        &self,
        host: &str,
        name: &str,
        run: &str,
        conversation: &str,
        launch: &str,
    ) -> anyhow::Result<Option<(Team, String)>> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='team_rule_members')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let mut query = self.db.prepare("SELECT DISTINCT t.id FROM teams t JOIN
            (SELECT team FROM team_rule_members WHERE name=?1 AND run=?2 AND conversation=?3 AND name<>'' AND host=?5
             UNION SELECT team FROM team_rule_members WHERE launch=?4 AND launch<>'' AND host=?5) m ON t.id=m.team
            WHERE t.status IN ('draft','planning','running','paused') LIMIT 2")?;
        let found = query
            .query_map(params![name, run, conversation, launch, host], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(query);
        anyhow::ensure!(
            found.len() <= 1,
            "Session belongs to more than one active team"
        );
        let Some(id) = found.first() else {
            return Ok(None);
        };
        let Some(team) = self.resolved(self.current(id)?)? else { return Ok(None) };
        let member = team
            .members
            .iter()
            .find(|member| {
                member.session.as_ref().is_some_and(|session| {
                    (if session.host.is_empty() { member.computer.as_str() } else { session.host.as_str() }) == host
                        && session.name == name
                        && session.run_id == run
                        && session.conversation_id == conversation
                }) || (!launch.is_empty() && member.launch_id == launch && member.computer == host)
            })
            .map(|member| member.id.clone());
        Ok(member.map(|member| (team, member)))
    }

    /// Task #6 (team ede724c4): the active teams (planning, running, paused)
    /// with a member whose stored session has this name, run or conversation,
    /// found through the members projection without decoding any other team.
    /// They are a superset of the teams whose member the caller is: the caller
    /// still checks each one. None when this store has no projection.
    pub(crate) fn session_teams(&self, name: &str, run: &str, conversation: &str) -> anyhow::Result<Option<Vec<Team>>> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='team_rule_members')",
            [],
            |row| row.get(0),
        )?;
        if !exists || (name.is_empty() && run.is_empty() && conversation.is_empty()) {
            return Ok(None);
        }
        let mut query = self.db.prepare(
            "SELECT DISTINCT t.id FROM teams t JOIN team_rule_members m ON t.id=m.team
             WHERE t.status IN ('planning','running','paused')
               AND ((?1<>'' AND m.name=?1) OR (?2<>'' AND m.run=?2) OR (?3<>'' AND m.conversation=?3))
             ORDER BY t.updated DESC LIMIT 8",
        )?;
        let ids = query
            .query_map(params![name, run, conversation], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        let mut teams = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(team) = self.load(&id)? {
                teams.push(team);
            }
        }
        Ok(Some(teams))
    }

    /// Every team, most recently changed first, at most `limit`.
    pub(crate) fn teams(&self, include_archived: bool, limit: i64) -> anyhow::Result<Vec<Team>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {} FROM teams WHERE ?1 OR status!='archived' ORDER BY updated DESC LIMIT ?2",
            self.listed()?
        ))?;
        // Task 105: the rows are read first and parsed after the statement
        // ends, so the read never holds the store while megabytes of JSON parse.
        let found = statement
            .query_map(params![include_archived, limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        self.decoded(found)
    }

    /// Task 128: the teams whose memory the background writer looks at: the
    /// running ones and the ones finished since `since` (their last pass
    /// closes the plan), most recently changed first, at most `limit`.
    pub(crate) fn memory_teams(&self, since: f64, limit: i64) -> anyhow::Result<Vec<Team>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {} FROM teams WHERE status IN ('planning','running','paused') OR (status IN ('done','stopped') AND updated >= ?1) ORDER BY updated DESC LIMIT ?2",
            self.listed()?
        ))?;
        let found = statement
            .query_map(params![since, limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        self.decoded(found)
    }

    /// The teams that still run: Planning, Running or Paused, most recently
    /// changed first, at most `limit`. Uses the (status, updated) index, so a
    /// finished team's status never reads every team (C1).
    pub(crate) fn active_teams(&self, limit: i64) -> anyhow::Result<Vec<Team>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {} FROM teams WHERE status IN ('planning','running','paused') ORDER BY updated DESC LIMIT ?1",
            self.listed()?
        ))?;
        // Task 105: read first, parse after the statement ends (see teams).
        let found = statement
            .query_map(params![limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        self.decoded(found)
    }

    /// Task #17 of team c536f0e5 (P2.6a): the teams an ordinary worker tick
    /// looks at: the active ones and the drafts (a draft starts and briefs its
    /// lead), by the (status, updated) index. An ended team whose members
    /// still wait for a usage-limit reset is reached by the full tick.
    pub(crate) fn ticked_teams(&self, limit: i64) -> anyhow::Result<Vec<Team>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {} FROM teams WHERE status IN ('draft','planning','running','paused') ORDER BY updated DESC LIMIT ?1",
            self.listed()?
        ))?;
        let found = statement
            .query_map(params![limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        self.decoded(found)
    }

    /// The sum of the active teams' revisions, without reading their payloads:
    /// U43's read-time default keys its cache by it, so a member added and one
    /// removed between two equal-length writes still refreshes the set.
    pub(crate) fn active_revision(&self) -> anyhow::Result<i64> {
        Ok(self.db.query_row(
            "SELECT COALESCE(SUM(revision),0) FROM teams WHERE status IN ('planning','running','paused')",
            [],
            |row| row.get(0),
        )?)
    }

    /// The stored receipt of an identical request, read only: a retry returns
    /// it before the request does any outside work (L4).
    pub(crate) fn receipt(&self, id: &str, input: &Value) -> anyhow::Result<Option<Value>> {
        ensure!(canonical_uuid(id), "A canonical request_id is required");
        let stored: Option<(String, String)> = self
            .db
            .query_row("SELECT digest,receipt FROM operations WHERE id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        match stored {
            None => Ok(None),
            Some((digest_text, receipt)) => {
                ensure!(digest_text == digest(input), "This request_id was already used for another request");
                Ok(Some(serde_json::from_str(&receipt)?))
            }
        }
    }

    /// A page of one team's feed, newest first; `before` is the previous page's
    /// cursor. Entries appended meanwhile never shift an older page.
    /// Task 112: the newest event of these kinds, its time and detail.
    pub(crate) fn last_event(&self, team: &str, kinds: &[&str]) -> anyhow::Result<Option<(f64, Value)>> {
        let list = kinds.iter().map(|kind| format!("'{}'", kind.replace('\'', ""))).collect::<Vec<_>>().join(",");
        let found = self
            .db
            .query_row(
                &format!("SELECT at,payload FROM events WHERE team=?1 AND kind IN ({list}) ORDER BY seq DESC LIMIT 1"),
                params![team],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(found.map(|(at, payload)| (at, serde_json::from_str(&payload).unwrap_or_default())))
    }

    pub(crate) fn events(&self, team: &str, before: Option<i64>, limit: i64) -> anyhow::Result<Value> {
        let limit = limit.clamp(1, PAGE);
        let mut statement = self.db.prepare(
            "SELECT seq,at,kind,payload FROM events WHERE team=?1 AND seq<?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![team, before.unwrap_or(i64::MAX), limit + 1], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (seq, at, kind, payload) = row?;
            items.push(json!({"seq":seq,"at":at,"kind":kind,"detail":serde_json::from_str::<Value>(&payload)?}));
        }
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next = if more { items.last().map(|item| item["seq"].clone()) } else { None };
        Ok(json!({"items":items,"next_cursor":next}))
    }

    /// U38: one team's journal entries a timeline reads (spans::EVENT_KINDS)
    /// between `from` and `to`, oldest first, at most `limit`, through the
    /// (team, kind, at) index; true when more were there.
    pub(crate) fn span_events(&self, team: &str, from: f64, to: f64, limit: i64) -> anyhow::Result<(Vec<super::spans::Journal>, bool)> {
        let kinds = super::spans::EVENT_KINDS.iter().map(|kind| format!("'{kind}'")).collect::<Vec<_>>().join(",");
        let mut statement = self.db.prepare(&format!(
            "SELECT at,kind,payload FROM events WHERE team=?1 AND kind IN ({kinds}) AND at>=?2 AND at<=?3 ORDER BY at,seq LIMIT ?4"
        ))?;
        let rows = statement.query_map(params![team, from, to, limit.max(1) + 1], |row| {
            Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (at, kind, payload) = row?;
            items.push(super::spans::Journal { at, kind, detail: serde_json::from_str(&payload)? });
        }
        let cut = items.len() as i64 > limit.max(1);
        items.truncate(limit.max(1) as usize);
        Ok((items, cut))
    }

    /// Task 8: every team's head for the watch stream's `team` topic: its id,
    /// name, lead, status, revision and the time of its last change, the most
    /// recently changed first, at most `limit`, and whether more exist. One
    /// indexed query: the payload is read only for the name and the lead.
    pub(crate) fn heads(&self, limit: usize) -> anyhow::Result<(Vec<Value>, bool)> {
        let mut statement = self.db.prepare(
            "SELECT id,revision,status,updated,json_extract(payload,'$.name'),json_extract(payload,'$.lead'),
                    json_extract(payload,'$.plan_mode'),json_extract(payload,'$.phase_modes')
             FROM teams ORDER BY updated DESC, id LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.saturating_add(1) as i64], |row| {
            // Task #58: the team's plan mode and each phase's own one (a phase
            // not named plans in the team's), so a watcher sees a switch.
            let phase_modes = row
                .get::<_, Option<String>>(7)?
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "revision": row.get::<_, i64>(1)?,
                "status": row.get::<_, String>(2)?,
                "updated": row.get::<_, f64>(3)?,
                "name": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                "lead": row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                "plan_mode": row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "standard".into()),
                "phase_modes": phase_modes,
            }))
        })?;
        let mut heads = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let more = heads.len() > limit;
        heads.truncate(limit);
        Ok((heads, more))
    }

    /// Creates a team. A replay of the same request returns its first receipt.
    #[track_caller]
    pub(crate) fn create(&mut self, request_id: &str, input: &Value, mut team: Team, events: Vec<Event>) -> anyhow::Result<Value> {
        let transaction = begin(&self.db, std::panic::Location::caller(), &team.id)?;
        if let Some(receipt) = replay(&transaction, request_id, input)? {
            return Ok(receipt);
        }
        ensure!(
            transaction.query_row("SELECT 1 FROM teams WHERE id=?1", params![team.id], |_| Ok(())).optional()?.is_none(),
            "A team with this id already exists"
        );
        let at = now();
        team.revision = 1;
        team.created = at;
        team.updated = at;
        // Task 288: a new team is stored in rows; closed phases go to the archive first.
        archive_closed(&transaction, &mut team).map_err(write_error)?;
        let saved = rows::save(&transaction, None, &mut team).map_err(write_error)?;
        append(&transaction, &team.id, at, &events).map_err(write_error)?;
        let receipt = json!({"status":"created","request_id":request_id,"team":team.id,"revision":team.revision});
        record(&transaction, request_id, input, &receipt, at).map_err(write_error)?;
        commit(transaction).map_err(write_error)?;
        let id = team.id.clone();
        self.remember(&id, team, saved);
        Ok(receipt)
    }

    /// Applies one change to a team in one transaction: checks the expected
    /// revision, bumps it and appends the change's feed entries. A request id
    /// makes it idempotent; the dispatcher's own changes pass none.
    /// #754: the receipt stored under a request id, whatever its input.
    pub(crate) fn stored_receipt(&self, id: &str) -> anyhow::Result<Option<Value>> {
        let stored: Option<String> = self
            .db
            .query_row("SELECT receipt FROM operations WHERE id=?1", params![id], |row| row.get(0))
            .optional()?;
        Ok(match stored {
            Some(text) => Some(serde_json::from_str(&text)?),
            None => None,
        })
    }

    /// Task 134: the store's connection, for the working tier's reads.
    pub(crate) fn connection(&self) -> &Connection {
        &self.db
    }

    /// Task 134: a team change that also writes the working tier, in one
    /// transaction: `change` gets the transaction and the team, as `mutate`'s
    /// closure gets the team; the team record and its events are written
    /// with it. Answers what `change` answers.
    #[track_caller]
    pub(crate) fn mutate_working<T>(
        &mut self,
        team_id: &str,
        change: impl FnOnce(&rusqlite::Transaction, &mut Team, &mut Vec<Event>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let caller = std::panic::Location::caller();
        // Task 287: the worker's batch of this team is written first, and goes
        // on after this change.
        let resume = self.suspend_batch(team_id, caller)?;
        let answer = self.mutate_working_now(team_id, caller, change);
        self.resume_batch(resume)?;
        answer
    }

    fn mutate_working_now<T>(
        &mut self,
        team_id: &str,
        caller: &'static std::panic::Location<'static>,
        change: impl FnOnce(&rusqlite::Transaction, &mut Team, &mut Vec<Event>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        // Task 288: the team is read before the lock; under it only the rows
        // written since are read.
        let mut team = self.current(team_id)?.context("Unknown team")?;
        let transaction = begin(&self.db, caller, team_id)?;
        let team_now = self.locked(&transaction, team_id, team.revision)?;
        if let Some(fresh) = team_now {
            team = fresh;
        }
        let mut events = Vec::new();
        let answer = change(&transaction, &mut team, &mut events)?;
        let at = now();
        team.revision += 1;
        team.updated = at;
        let saved = rows::save(&transaction, self.cache.borrow().get(team_id), &mut team)?;
        append(&transaction, team_id, at, &events)?;
        commit(transaction)?;
        self.remember(team_id, team, saved);
        Ok(answer)
    }

    #[track_caller]
    pub(crate) fn mutate(
        &mut self,
        request: Option<(&str, &Value)>,
        team_id: &str,
        expected_revision: Option<u64>,
        change: impl FnOnce(&mut Team, &mut Vec<Event>) -> anyhow::Result<Value>,
    ) -> anyhow::Result<Value> {
        let caller = std::panic::Location::caller();
        if self.batch.as_ref().is_some_and(|batch| batch.team == team_id) {
            // Task 287: the worker's own change joins its tick's transaction.
            if request.is_none() && expected_revision.is_none() {
                return self.mutate_in_batch(change);
            }
            let resume = self.suspend_batch(team_id, caller)?;
            let answer = self.mutate_now(request, team_id, expected_revision, caller, change);
            self.resume_batch(resume)?;
            return answer;
        }
        self.mutate_now(request, team_id, expected_revision, caller, change)
    }

    fn mutate_now(
        &mut self,
        request: Option<(&str, &Value)>,
        team_id: &str,
        expected_revision: Option<u64>,
        caller: &'static std::panic::Location<'static>,
        change: impl FnOnce(&mut Team, &mut Vec<Event>) -> anyhow::Result<Value>,
    ) -> anyhow::Result<Value> {
        // Task 288: the team is read before the lock; under it only the rows
        // written since are read.
        let read = self.current(team_id)?;
        let transaction = begin(&self.db, caller, team_id)?;
        if let Some((id, input)) = request {
            if let Some(receipt) = replay(&transaction, id, input)? {
                return Ok(receipt);
            }
        }
        let mut team = read.context("Unknown team")?;
        if let Some(fresh) = self.locked(&transaction, team_id, team.revision)? {
            team = fresh;
        }
        if let Some(expected) = expected_revision {
            ensure!(team.revision == expected, "The team changed; refresh and try again");
        }
        archive_closed(&transaction, &mut team).map_err(write_error)?;
        let input = request.map_or(Value::Null, |(_, input)| input.clone());
        restore_phase(&transaction, &mut team, &input)?;
        resolve_dependencies(&transaction, &mut team, &input)?;
        load_lead_change(&transaction, &mut team, &input)?;
        // Task #469: the stubs a change writes keep the dependencies of the
        // tasks archived before stubs did, so a done phase's plan survives.
        restore_stub_deps(&transaction, &mut team)?;
        let mut events = Vec::new();
        let mut receipt = change(&mut team, &mut events)?;
        let at = now();
        after_change(&mut team, &mut events, at);
        team.revision += 1;
        team.updated = at;
        append(&transaction, team_id, at, &events).map_err(write_error)?;
        archive_closed(&transaction, &mut team).map_err(write_error)?;
        let saved = rows::save(&transaction, self.cache.borrow().get(team_id), &mut team).map_err(write_error)?;
        if receipt.is_object() {
            receipt["team"] = json!(team_id);
            receipt["revision"] = json!(team.revision);
        }
        if let Some((id, input)) = request {
            if receipt.is_object() {
                receipt["request_id"] = json!(id);
            }
            record(&transaction, id, input, &receipt, at).map_err(write_error)?;
        }
        commit(transaction).map_err(write_error)?;
        self.remember(team_id, team, saved);
        Ok(receipt)
    }
}

impl Store {
    /// Task 288: under the write lock, the team when another writer changed it
    /// since the copy read before the lock (`revision`): only the rows written
    /// since are read. None when the copy is current. The measurement's
    /// "before" (the whole-record layout) reads the whole record here, as
    /// every change did before task 288.
    fn locked(&self, transaction: &Connection, team_id: &str, revision: u64) -> anyhow::Result<Option<Team>> {
        if rows::legacy() {
            let payload: String = transaction
                .query_row("SELECT payload FROM teams WHERE id=?1", params![team_id], |row| row.get(0))
                .optional()?
                .context("Unknown team")?;
            return Ok(Some(serde_json::from_str(&payload).context("Invalid team record")?));
        }
        let stored: Option<i64> = transaction.query_row("SELECT revision FROM teams WHERE id=?1", params![team_id], |row| row.get(0)).optional()?;
        let stored = stored.context("Unknown team")?;
        if stored as u64 == revision {
            return Ok(None);
        }
        Ok(Some(self.current(team_id)?.context("Unknown team")?))
    }

    /// Task 287 (owner, 2026-11-08): `hgs team inbox` marking messages read
    /// rewrote the whole team record. A change of the working tier's rows that
    /// leaves the team record alone: its rows and its events, in one short
    /// transaction; the revision does not move.
    #[track_caller]
    pub(crate) fn write_rows<T>(
        &mut self,
        team_id: &str,
        change: impl FnOnce(&rusqlite::Transaction, &mut Vec<Event>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let transaction = begin(&self.db, std::panic::Location::caller(), team_id)?;
        let mut events = Vec::new();
        let answer = change(&transaction, &mut events)?;
        append(&transaction, team_id, now(), &events)?;
        commit(transaction)?;
        Ok(answer)
    }

    /// Task 287: the worker's changes to `team_id` from here on go to a working
    /// copy and are written once, by `end_batch` (a change with a request id,
    /// or one of the working tier, writes the batch first and goes on after).
    pub(crate) fn begin_batch(&mut self, team_id: &str) -> anyhow::Result<()> {
        self.end_batch()?;
        let Some(working) = self.current(team_id)? else { return Ok(()) };
        self.batch = Some(Batch { team: team_id.to_owned(), base_revision: working.revision, base: working.clone(), working, events: Vec::new(), changed: false });
        Ok(())
    }

    /// Task 287: writes the batch in one transaction; whether it wrote.
    #[track_caller]
    pub(crate) fn end_batch(&mut self) -> anyhow::Result<bool> {
        let caller = std::panic::Location::caller();
        let Some(mut batch) = self.batch.take() else { return Ok(false) };
        if !batch.dirty() {
            return Ok(false);
        }
        self.flush(batch, caller)?;
        Ok(true)
    }

    fn suspend_batch(&mut self, team_id: &str, caller: &'static std::panic::Location<'static>) -> anyhow::Result<Option<String>> {
        if !self.batch.as_ref().is_some_and(|batch| batch.team == team_id) {
            return Ok(None);
        }
        let mut batch = self.batch.take().expect("checked");
        if batch.dirty() {
            self.flush(batch, caller)?;
        }
        Ok(Some(team_id.to_owned()))
    }

    fn resume_batch(&mut self, team: Option<String>) -> anyhow::Result<()> {
        match team {
            Some(team) => self.begin_batch(&team),
            None => Ok(()),
        }
    }

    fn mutate_in_batch(&mut self, change: impl FnOnce(&mut Team, &mut Vec<Event>) -> anyhow::Result<Value>) -> anyhow::Result<Value> {
        let batch = self.batch.as_mut().expect("a batch");
        // A change that fails leaves the copy as it was, as a rollback does.
        let mut team = batch.working.clone();
        let mut events = Vec::new();
        let mut receipt = change(&mut team, &mut events)?;
        let at = now();
        after_change(&mut team, &mut events, at);
        // Task #6 (P1.4): a step that changed nothing and told nothing leaves
        // the copy, its `updated` and the batch's need to write alone.
        if !events.is_empty() || team != batch.working {
            team.updated = at;
            batch.working = team;
            if !events.is_empty() {
                batch.events.push((at, events));
            }
            batch.changed = true;
        }
        if receipt.is_object() {
            receipt["team"] = json!(batch.team);
            receipt["revision"] = json!(batch.base_revision + u64::from(batch.changed));
        }
        Ok(receipt)
    }

    /// Writes a batch: the copy itself when nobody else wrote the team since it
    /// was read, else the copy merged over the newer record. The record is
    /// prepared before the lock; under it only the revision is checked.
    fn flush(&mut self, batch: Batch, caller: &'static std::panic::Location<'static>) -> anyhow::Result<()> {
        let Batch { team: team_id, base_revision, base, mut working, events, .. } = batch;
        // The values of the merge, made only when another writer interleaved.
        let mut values: Option<(Value, Value)> = None;
        let mut expected = base_revision;
        for round in 0..4 {
            let transaction = begin(&self.db, caller, &team_id)?;
            let stored: Option<i64> =
                transaction.query_row("SELECT revision FROM teams WHERE id=?1", params![team_id], |row| row.get(0)).optional()?;
            let Some(stored) = stored else { return Ok(()) };
            if stored as u64 != expected {
                if round < 3 {
                    // Another writer changed the team: merge over its record, outside the lock.
                    drop(transaction);
                    if !self.merge_fresh(&team_id, &base, &mut values, &mut working, &mut expected)? {
                        return Ok(());
                    }
                    continue;
                }
                // The last round merges under the lock rather than lose the changes.
                if !self.merge_fresh(&team_id, &base, &mut values, &mut working, &mut expected)? {
                    return Ok(());
                }
            }
            working.revision = expected + 1;
            // Task 288: only the rows the tick changed, against the copy it started from.
            archive_closed(&transaction, &mut working).map_err(write_error)?;
            let saved = rows::save(&transaction, self.cache.borrow().get(&team_id), &mut working)?;
            for (at, events) in &events {
                append(&transaction, &team_id, *at, events)?;
            }
            commit(transaction)?;
            self.remember(&team_id, working, saved);
            return Ok(());
        }
        Ok(())
    }

    /// The batch's changes (from `base` to `working`) merged over the team
    /// as stored now; false when the team is gone.
    fn merge_fresh(&self, team_id: &str, base: &Team, values: &mut Option<(Value, Value)>, working: &mut Team, expected: &mut u64) -> anyhow::Result<bool> {
        let Some(fresh) = self.current(team_id)? else { return Ok(false) };
        if values.is_none() {
            *values = Some((serde_json::to_value(base)?, serde_json::to_value(&*working)?));
        }
        let (base_value, ours) = values.as_ref().expect("made");
        *working = merged(base_value, ours, &serde_json::to_value(&fresh)?)?;
        *expected = fresh.revision;
        Ok(true)
    }
}

fn has_archive(db: &Connection) -> anyhow::Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='team_task_archive')", [], |row| row.get(0))?)
}

/// Task #469: the name of the archived-stub backfill in `store_migrations`.
const STUB_DEPS: &str = "archived-stub-deps-469";

/// Task #469: one bounded pass over the store's archived stubs, which take the
/// dependencies they lost from the archive's payload (a done phase drew in one
/// layer without links). Both copies are patched where they live, never by
/// loading a team; the marker stops the pass after the first one.
fn backfill_archive_stubs(db: &Connection) -> anyhow::Result<()> {
    if !has_archive(db)? {
        return Ok(());
    }
    let done: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM store_migrations WHERE name=?1)", [STUB_DEPS], |row| row.get(0))?;
    if done {
        return Ok(());
    }
    // The task rows of the plan (layout 2) and the archive's own stub copy
    // (what `archived_tasks` pages and dependency resolution read).
    let pass = (|| -> anyhow::Result<()> {
        db.execute_batch("BEGIN IMMEDIATE")?;
        db.execute_batch(
            "UPDATE team_tasks SET payload=json_set(team_tasks.payload,'$.deps',json(json_extract(a.payload,'$.deps')))
               FROM team_task_archive a
               WHERE a.team=team_tasks.team AND a.task=team_tasks.task
                 AND json_type(team_tasks.payload,'$.archived') IS NOT NULL
                 AND COALESCE(json_array_length(team_tasks.payload,'$.deps'),0)=0
                 AND json_array_length(a.payload,'$.deps')>0;
             UPDATE team_task_archive SET stub=json_set(stub,'$.deps',json(json_extract(payload,'$.deps')))
               WHERE COALESCE(json_array_length(stub,'$.deps'),0)=0 AND json_array_length(payload,'$.deps')>0;",
        )?;
        Ok(())
    })();
    match pass {
        Ok(()) => db.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    db.execute("INSERT INTO store_migrations(name,at) VALUES(?1,?2)", params![STUB_DEPS, now()])?;
    Ok(())
}

fn archive_page(db: &Connection, sql: &str, args: impl rusqlite::Params, limit: i64, key: &str) -> anyhow::Result<Value> {
    if !has_archive(db)? { return Ok(json!({"items":[],"next_cursor":null})); }
    let mut query = db.prepare(sql)?;
    let rows = query.query_map(args, |row| row.get::<_,Option<String>>(0))?;
    let mut items: Vec<Value> = rows.map(|row| Ok(serde_json::from_str(&row?.context("Archived metadata exceeds the 64 KiB read limit")?)?)).collect::<anyhow::Result<_>>()?;
    let more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    let next = if more { items.last().map(|item| item[key].clone()) } else { None };
    Ok(json!({"items":items,"next_cursor":next}))
}

/// Explicit edits of archived work restore only its phase, at most 500 tasks.
fn archived_phase_to_restore(db: &Connection, team: &Team, input: &Value) -> anyhow::Result<Option<u32>> {
    let task = input["task"].as_u64().and_then(|id| u32::try_from(id).ok()).or_else(|| {
        (input["action"] == "reopen")
            .then(|| {
                team.findings
                    .iter()
                    .find(|finding| finding.id == input["finding"].as_str().unwrap_or_default())
                    .and_then(|finding| finding.task)
            })
            .flatten()
    });
    if !has_archive(db)? {
        return Ok(None);
    }
    let requested = input["phase"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| task.and_then(|id| team.task(id).filter(|task| task.archived.is_some()).map(|task| task.phase)));
    let phase = match requested {
        Some(n) => match team.phases.iter().find(|phase| phase.n == n) {
            Some(phase) => phase.archived.as_ref().map(|_| n),
            None => db
                .query_row(
                    "SELECT phase FROM team_phase_archive WHERE team=?1 AND phase=?2",
                    params![team.id, n],
                    |row| row.get(0),
                )
                .optional()?,
        },
        None => match task.filter(|id| team.task(*id).is_none()) {
            Some(id) => db
                .query_row(
                    "SELECT phase FROM team_task_archive WHERE team=?1 AND task=?2",
                    params![team.id, id],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        },
    };
    Ok(phase)
}

fn read_archived_phase_tasks(db: &Connection, team: &Team, n: u32) -> anyhow::Result<Vec<Task>> {
    let bytes: i64 = db.query_row(
        "SELECT COALESCE(sum(bytes),0) FROM (SELECT length(CAST(payload AS BLOB)) AS
         bytes FROM team_task_archive WHERE team=?1 AND phase=?2 LIMIT 501)",
        params![team.id, n],
        |row| row.get(0),
    )?;
    ensure!(bytes <= 8 * 1024 * 1024, "Archived phase exceeds the 8 MiB edit limit; read individual tasks instead");
    let mut query = db.prepare(
        "SELECT CASE WHEN length(CAST(payload AS BLOB))<=8388608 THEN payload ELSE NULL
         END FROM team_task_archive WHERE team=?1 AND phase=?2 ORDER BY task LIMIT 501",
    )?;
    let tasks: Vec<Task> = query
        .query_map(params![team.id, n], |row| row.get::<_, Option<String>>(0))?
        .map(|row| Ok(serde_json::from_str(&row?.context("Archived task exceeds the 8 MiB edit limit")?)?))
        .collect::<anyhow::Result<_>>()?;
    ensure!(
        tasks.len() + team.tasks.iter().filter(|task| task.phase != n && task.archived.is_none()).count() <= 500,
        "Restoring this archived phase would exceed 500 live tasks"
    );
    Ok(tasks)
}

fn restore_phase(db: &Connection, team: &mut Team, input: &Value) -> anyhow::Result<()> {
    let phase = archived_phase_to_restore(db, team, input)?;
    let Some(n) = phase else { return Ok(()) };
    let tasks = read_archived_phase_tasks(db, team, n)?;
    if tasks.is_empty() {
        return Ok(());
    }
    let payload: Option<String> = db.query_row(
        "SELECT CASE WHEN length(CAST(payload AS BLOB))<=8388608 THEN payload ELSE NULL
         END FROM team_phase_archive WHERE team=?1 AND phase=?2",
        params![team.id, n],
        |row| row.get(0),
    )?;
    let payload = payload.context("Archived phase header exceeds the 8 MiB edit limit")?;
    let header: String =
        db.query_row("SELECT header FROM team_phase_archive WHERE team=?1 AND phase=?2", params![team.id, n], |row| {
            row.get(0)
        })?;
    let header: Phase = serde_json::from_str(&header)?;
    let counts = header.archived.as_ref().context("Missing archived phase counts")?;
    let totals = &team.archive_totals;
    ensure!(
        totals.phases > 0
            && totals.tasks >= counts.tasks
            && totals.accepted >= counts.accepted
            && totals.rejected >= counts.rejected
            && totals.cancelled >= counts.cancelled,
        "Invalid archived phase totals"
    );
    team.archive_totals.remove(counts);
    team.phases.retain(|phase| phase.n != n);
    team.phases.push(serde_json::from_str(&payload)?);
    team.phases.sort_by_key(|phase| phase.n);
    team.tasks.retain(|task| task.phase != n);
    team.tasks.extend(tasks);
    team.tasks.sort_by_key(|task| task.id);
    db.execute("DELETE FROM team_task_archive WHERE team=?1 AND phase=?2", params![team.id, n])?;
    db.execute("DELETE FROM team_phase_archive WHERE team=?1 AND phase=?2", params![team.id, n])?;
    Ok(())
}

/// Resolve referenced missing dependencies, prerequisites and origins by archive primary key.
/// This one helper serves loads and mutations, including proposed input.
/// Task #4 (team ede724c4): the snapshots of the lead change a request names
/// (`revert-lead-change`), read from `team_lead_changes` into the team before
/// the change runs; the write puts them back into their row's keeping.
fn load_lead_change(db: &Connection, team: &mut Team, input: &Value) -> anyhow::Result<()> {
    let Some(id) = input["change"].as_str().map(str::trim) else { return Ok(()) };
    let Some(change) = team.lead_changes.iter_mut().find(|change| change.id == id && change.history_missing()) else { return Ok(()) };
    if let Some(history) = rows::lead_change_history(db, &team.id, id)? {
        change.tasks_before = history.tasks_before;
        change.phases_before = history.phases_before;
    }
    Ok(())
}

fn resolve_dependencies(db: &Connection, team: &mut Team, input: &Value) -> anyhow::Result<()> {
    let mut ids: std::collections::BTreeSet<u32> = team
        .tasks
        .iter()
        .filter(|task| task.archived.is_none())
        .flat_map(|task| task.deps.iter().map(|dep| dep.task).chain(task.requires.iter().filter_map(|required| {
            if let Prerequisite::Task { task } = required { Some(*task) } else { None }
        })).chain(task.origin))
        .collect();
    fn proposed(value: &Value, ids: &mut std::collections::BTreeSet<u32>) {
        if let Some(id) = value.get("origin").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) {
            ids.insert(id);
        }
        for field in ["deps", "requires"] {
            if let Some(deps) = value.get(field).and_then(Value::as_array) {
                for dep in deps {
                    if let Some(id) = dep.as_u64().or_else(|| dep["task"].as_u64()).and_then(|id| u32::try_from(id).ok()) {
                        ids.insert(id);
                    }
                }
            }
        }
        match value {
            Value::Object(values) => {
                for value in values.values() {
                    proposed(value, ids);
                }
            }
            Value::Array(values) => {
                for value in values {
                    proposed(value, ids);
                }
            }
            _ => {}
        }
    }
    proposed(input, &mut ids);
    if ids.iter().all(|id| team.task(*id).is_some()) {
        return Ok(());
    }
    if !has_archive(db)? {
        return Ok(());
    }
    let mut query = db.prepare(
        "SELECT CASE WHEN COALESCE(json_array_length(stub,'$.deps'),0)=0 AND json_array_length(payload,'$.deps')>0
         THEN json_set(stub,'$.deps',json(json_extract(payload,'$.deps'))) ELSE stub END
         FROM team_task_archive WHERE team=?1 AND task=?2",
    )?;
    for id in ids {
        if team.task(id).is_some() {
            continue;
        }
        if let Some(stub) = query.query_row(params![team.id, id], |row| row.get::<_, String>(0)).optional()? {
            team.archive_deps.insert(id, serde_json::from_str(&stub)?);
        }
    }
    Ok(())
}

/// Task #469: a stub written before stubs kept their dependencies drew its done
/// phase in one layer, without links. The full task stays in the archive's
/// payload, so the stub takes the dependencies the plan lays out from there,
/// for its layers and its links. Bounded by the plan's stub window and asked
/// for only when a stub has none; the write path saves what this fills.
fn restore_stub_deps(db: &Connection, team: &mut Team) -> anyhow::Result<()> {
    let missing: Vec<u32> = team
        .tasks
        .iter()
        .filter(|task| task.archived.is_some() && task.deps.is_empty())
        .map(|task| task.id)
        .take(ARCHIVED_TASK_WINDOW)
        .collect();
    if missing.is_empty() || !has_archive(db)? {
        return Ok(());
    }
    let mut query = db.prepare(
        "SELECT CAST(j.value AS INTEGER),json_extract(a.payload,'$.deps') FROM json_each(?2) j
         JOIN team_task_archive a ON a.team=?1 AND a.task=CAST(j.value AS INTEGER)",
    )?;
    let rows: Vec<(u32, Option<String>)> = query
        .query_map(params![team.id, serde_json::to_string(&missing)?], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut found: std::collections::HashMap<u32, Vec<super::model::Dependency>> = std::collections::HashMap::new();
    for (id, text) in rows {
        let Some(text) = text.filter(|text| text != "[]") else { continue };
        let deps: Vec<super::model::Dependency> = serde_json::from_str(&text)?;
        if !deps.is_empty() {
            found.insert(id, deps);
        }
    }
    for task in &mut team.tasks {
        if let Some(deps) = found.remove(&task.id) {
            task.deps = deps;
        }
    }
    Ok(())
}

/// Archive histories and replace the live metadata in the caller's transaction.
fn archive_closed(transaction: &rusqlite::Transaction, team: &mut Team) -> anyhow::Result<()> {
    let closed = closable_phases(team);
    archive_tasks(transaction, team, &closed)?;
    archive_headers(transaction, team, &closed)?;
    apply_archive_windows(team);
    Ok(())
}

fn closable_phases(team: &Team) -> std::collections::BTreeSet<u32> {
    let mut phase_closed = std::collections::BTreeMap::new();
    for task in &team.tasks {
        let state = phase_closed.entry(task.phase).or_insert((true, false));
        state.0 &= task.status.finished()
            && task.untested_merge.is_none()
            && !task.merge.as_ref().is_some_and(|merge| matches!(merge.state.as_str(), "queued" | "testing"));
        state.1 |= task.archived.is_none();
    }
    phase_closed
        .into_iter()
        .filter_map(|(n, (closed, fresh))| {
            let phase = team.phases.iter().find(|phase| phase.n == n);
            let accepted = phase.map_or(n == 1 && team.status != super::model::TeamStatus::Planning, |phase| {
                phase.state == PhaseState::Done || phase.state == PhaseState::Approved && phase.acceptance.is_lead()
            });
            (closed
                && fresh
                && accepted
                && !(team.plan_mode == super::model::PlanMode::Free && super::ops::open_phase(team) == n))
                .then_some(n)
        })
        .collect()
}

fn archive_tasks(
    transaction: &rusqlite::Transaction,
    team: &mut Team,
    closed: &std::collections::BTreeSet<u32>,
) -> anyhow::Result<()> {
    let summaries: Vec<(usize, ArchivedTask)> = team
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.archived.is_none() && closed.contains(&task.phase))
        .map(|(index, task)| {
            (
                index,
                ArchivedTask {
                    accepted_at: task.attempts.last().and_then(|attempt| attempt.decided_at),
                    started_at: task.attempts.first().map(|attempt| attempt.sent_at),
                    merge_commit: task.merge.as_ref().map(|merge| merge.commit.clone()).unwrap_or_default(),
                    timing: {
                        let mut timing = super::timing::timing(team, task, now());
                        timing.as_object_mut().unwrap().remove("attempts");
                        timing
                    },
                },
            )
        })
        .collect();
    for (index, summary) in summaries {
        let task = &mut team.tasks[index];
        let payload = serde_json::to_string(task)?;
        let stub = Task {
            id: task.id,
            phase: task.phase,
            title: task.title.clone(),
            assignee: task.assignee.clone(),
            status: task.status,
            // Task #469: the plan draws a done phase's layers and links from
            // its dependencies, so the stub keeps them and the origin.
            deps: task.deps.clone(),
            origin: task.origin,
            archived: Some(summary),
            ..Default::default()
        };
        transaction.execute(
            "INSERT INTO team_task_archive(team,task,phase,status,stub,payload)
             VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(team,task) DO UPDATE SET
             phase=excluded.phase,status=excluded.status,stub=excluded.stub,payload=excluded.payload",
            params![
                team.id,
                task.id,
                task.phase,
                serde_json::to_value(task.status)?.as_str(),
                super::model::task_stub(&stub).to_string(),
                payload
            ],
        )?;
        *task = stub;
    }
    Ok(())
}

fn archive_headers(
    transaction: &rusqlite::Transaction,
    team: &mut Team,
    closed: &std::collections::BTreeSet<u32>,
) -> anyhow::Result<()> {
    for &n in closed {
        if !team.phases.iter().any(|phase| phase.n == n) {
            team.phases.push(Phase {
                n,
                title: format!("Phase {n}"),
                state: PhaseState::Approved,
                ..Default::default()
            });
        }
        let phase = team.phases.iter_mut().find(|phase| phase.n == n).unwrap();
        if phase.archived.is_none() {
            let original = serde_json::to_string(&phase)?;
            let tasks: Vec<_> = team.tasks.iter().filter(|task| task.phase == n).collect();
            phase.archived = Some(ArchivedPhase {
                tasks: tasks.len(),
                accepted: tasks.iter().filter(|task| task.status == super::model::TaskStatus::Accepted).count(),
                rejected: tasks.iter().filter(|task| task.status == super::model::TaskStatus::Rejected).count(),
                cancelled: tasks.iter().filter(|task| task.status == super::model::TaskStatus::Cancelled).count(),
                closed_at: if phase.acceptance.is_lead() {
                    tasks.iter().filter_map(|task| task.archived.as_ref()?.accepted_at).max_by(f64::total_cmp)
                } else {
                    transaction
                        .query_row(
                            "SELECT at FROM events WHERE team=?1 AND json_extract(payload,'$.phase')=?2 AND
                             kind='phase_owner_accepted' ORDER BY at DESC LIMIT 1",
                            params![team.id, n],
                            |row| row.get(0),
                        )
                        .optional()?
                },
            });
            team.archive_totals.add(phase.archived.as_ref().unwrap());
            transaction.execute(
                "INSERT INTO team_phase_archive(team,phase,header,payload) VALUES(?1,?2,?3,?4) ON
                 CONFLICT(team,phase) DO UPDATE SET
                 header=excluded.header,payload=excluded.payload",
                params![team.id, n, super::model::phase_stub(phase).to_string(), original],
            )?;
        }
        phase.tasks.clear();
        phase.changes.clear();
    }
    Ok(())
}

fn newest_ids(ids: impl Iterator<Item = u32>, limit: usize) -> std::collections::BTreeSet<u32> {
    let mut ids: Vec<u32> = ids.collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.into_iter().take(limit).collect()
}

fn apply_archive_windows(team: &mut Team) {
    let highest = team.tasks.iter().filter(|task| task.archived.is_some()).map(|task| task.id).max().unwrap_or(0);
    team.last_archived_task = team.last_archived_task.max(highest);
    let phases = newest_ids(
        team.phases.iter().filter(|phase| phase.archived.is_some()).map(|phase| phase.n),
        ARCHIVED_PHASE_WINDOW,
    );
    let evicted: std::collections::BTreeSet<u32> = team.phases.iter()
        .filter(|phase| phase.archived.is_some() && !phases.contains(&phase.n))
        .map(|phase| phase.n).collect();
    team.phase_modes.retain(|phase, _| !evicted.contains(phase));
    team.phases.retain(|phase| phase.archived.is_none() || phases.contains(&phase.n));
    let tasks = newest_ids(
        team.tasks.iter().filter(|task| task.archived.is_some() && phases.contains(&task.phase)).map(|task| task.id),
        ARCHIVED_TASK_WINDOW,
    );
    team.tasks.retain(|task| task.archived.is_none() || tasks.contains(&task.id));
}

/// The rules every change of a team keeps, in a transaction or a batch.
fn after_change(team: &mut Team, events: &mut Vec<Event>, at: f64) {
    // Task 111: every change re-checks the open proposals; one that can no
    // longer apply is withdrawn in the same write.
    super::ops::withdraw_inapplicable(team, events);
    // Task 112: only a paused team keeps its pause record.
    if team.status != super::model::TeamStatus::Paused {
        team.paused = None;
    }
    // Task 81: a task of the approved plan gets its baseline schedule in the
    // change that approves it or adds it.
    super::baseline::stamp(team, at);
}

/// Task 287: the worker's copy merged over a newer record, as a team.
fn merged(base: &Value, ours: &Value, theirs: &Value) -> anyhow::Result<Team> {
    serde_json::from_value(merge3(base, ours, theirs)).context("The worker's changes did not merge into the team record")
}

/// Task 287: the changes from `base` to `ours` applied over `theirs`, another
/// writer's newer record. Objects merge key by key; lists of records with an
/// `id` (tasks, attempts, members) or an `n` (phases) merge record by record;
/// any other value is ours when only we changed it, and theirs when they did
/// (a member's or the owner's action wins over the worker's, which the next
/// tick recomputes).
pub(crate) fn merge3(base: &Value, ours: &Value, theirs: &Value) -> Value {
    if ours == base || ours == theirs {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }
    match (base, ours, theirs) {
        (Value::Object(base), Value::Object(ours), Value::Object(theirs)) => {
            let mut out = serde_json::Map::new();
            for (key, their) in theirs {
                match (base.get(key), ours.get(key)) {
                    (Some(was), Some(our)) => {
                        out.insert(key.clone(), merge3(was, our, their));
                    }
                    // We removed it: gone unless they changed it.
                    (Some(was), None) => {
                        if their != was {
                            out.insert(key.clone(), their.clone());
                        }
                    }
                    (None, Some(our)) => {
                        out.insert(key.clone(), merge3(&Value::Null, our, their));
                    }
                    (None, None) => {
                        out.insert(key.clone(), their.clone());
                    }
                }
            }
            for (key, our) in ours {
                if theirs.contains_key(key) {
                    continue;
                }
                // They removed it: gone unless we changed it.
                match base.get(key) {
                    Some(was) if was == our => {}
                    _ => {
                        out.insert(key.clone(), our.clone());
                    }
                }
            }
            Value::Object(out)
        }
        (Value::Array(base), Value::Array(ours), Value::Array(theirs)) => {
            let (base, ours, theirs) = (base.as_slice(), ours.as_slice(), theirs.as_slice());
            match (keyed(base), keyed(ours), keyed(theirs)) {
            (Some(base_keys), Some(our_keys), Some(their_keys)) => {
                let find = |list: &[Value], keys: &[String], key: &str| keys.iter().position(|found| found == key).map(|index| list[index].clone());
                let mut out = Vec::new();
                for (index, key) in their_keys.iter().enumerate() {
                    let their = &theirs[index];
                    match (find(base, base_keys.as_slice(), key), find(ours, our_keys.as_slice(), key)) {
                        (Some(was), Some(our)) => out.push(merge3(&was, &our, their)),
                        (Some(was), None) => {
                            if *their != was {
                                out.push(their.clone());
                            }
                        }
                        (None, Some(our)) => out.push(merge3(&Value::Null, &our, their)),
                        (None, None) => out.push(their.clone()),
                    }
                }
                for (index, key) in our_keys.iter().enumerate() {
                    if their_keys.contains(key) {
                        continue;
                    }
                    match find(base, base_keys.as_slice(), key) {
                        Some(was) if was == ours[index] => {}
                        _ => out.push(ours[index].clone()),
                    }
                }
                Value::Array(out)
            }
            // Both changed a plain list: theirs, as for any other value.
            _ => Value::Array(theirs.to_vec()),
            }
        }
        _ => theirs.clone(),
    }
}

/// The keys of a list of records (`id`, else `n`), when every record has a
/// distinct one.
fn keyed(list: &[Value]) -> Option<Vec<String>> {
    let keys: Vec<String> = list
        .iter()
        .map(|item| {
            let key = item.get("id").filter(|key| !key.is_null()).or_else(|| item.get("n").filter(|key| !key.is_null()))?;
            Some(match key {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
        })
        .collect::<Option<_>>()?;
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    (unique.len() == keys.len()).then_some(keys)
}



fn append(transaction: &rusqlite::Transaction, team: &str, at: f64, events: &[Event]) -> anyhow::Result<()> {
    for event in events {
        transaction.execute(
            "INSERT INTO events(team,at,kind,payload) VALUES(?1,?2,?3,?4)",
            params![team, at, event.kind, event.payload.to_string()],
        )?;
    }
    Ok(())
}

/// The stored receipt of an earlier identical request; another request that
/// reuses the id is refused.
fn replay(transaction: &rusqlite::Transaction, id: &str, input: &Value) -> anyhow::Result<Option<Value>> {
    ensure!(canonical_uuid(id), "A canonical request_id is required");
    let stored: Option<(String, String)> = transaction
        .query_row("SELECT digest,receipt FROM operations WHERE id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    match stored {
        None => Ok(None),
        Some((digest_text, receipt)) => {
            ensure!(digest_text == digest(input), "This request_id was already used for another request");
            Ok(Some(serde_json::from_str(&receipt)?))
        }
    }
}

fn record(transaction: &rusqlite::Transaction, id: &str, input: &Value, receipt: &Value, at: f64) -> anyhow::Result<()> {
    transaction.execute(
        "INSERT INTO operations(id,digest,receipt,at) VALUES(?1,?2,?3,?4)",
        params![id, digest(input), receipt.to_string(), at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::model::{TeamStatus};
    use super::*;

    /// Task #28: `hgs team current` and `hgs team list` from a sandboxed
    /// DeepSeek member failed with "unable to open database file": the sorted
    /// read of the active teams spilled to a temporary file, and Low integrity
    /// may not create one in %TEMP%. Here the temporary folder is gone before
    /// the read, and the sort is larger than the cache.
    #[test]
    fn a_sorted_read_needs_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams.sqlite3");
        let store = Store::open_at(&path, true).unwrap();
        for index in 0..40 {
            let team = Team { id: format!("t{index}"), name: format!("t{index}"), goal: "g".repeat(200_000),
                status: TeamStatus::Running, ..Default::default() };
            store.db.execute("INSERT INTO teams(id,revision,status,updated,payload) VALUES(?1,1,'running',?2,?3)",
                params![team.id, index as f64, serde_json::to_string(&team).unwrap()]).unwrap();
        }
        drop(store);
        let store = Store::open_at(&path, false).unwrap();
        store.db.pragma_update(None, "cache_size", -1).unwrap();
        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        store.db.execute_batch(&format!("PRAGMA temp_store_directory='{}'", gone.display())).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let teams = store.active_teams(1000);
        store.db.execute_batch("PRAGMA temp_store_directory=''").unwrap();
        let teams = teams.unwrap();
        assert_eq!(teams.len(), 40);
        assert_eq!(teams[0].id, "t39");
    }

    /// A store with `count` teams, each a record of about `bytes` bytes.
    fn filled(path: &Path, journal: Journal, count: usize, bytes: usize) -> Store {
        let mut store = Store::open_journal(path, true, journal).unwrap();
        for index in 0..count {
            let team = Team { id: format!("t{index}"), name: format!("Team {index}"), lead: "lead".into(), goal: "g".repeat(bytes),
                status: TeamStatus::Running, ..Default::default() };
            store.create(&format!("11111111-1111-4111-8111-{index:012}"), &json!({"n":index}), team, vec![]).unwrap();
        }
        store
    }

    /// Task #393 (owner, 2026-11-08): a large write checkpoints the log into
    /// the store and truncates it, so the next read opens a small WAL.
    #[test]
    fn a_large_write_checkpoints_and_truncates_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams.sqlite3");
        let mut store = Store::open_journal(&path, true, Journal::Wal).unwrap();
        let team = Team {
            id: "big".into(),
            name: "Big".into(),
            lead: "lead".into(),
            goal: "g".repeat(12 * 1024 * 1024),
            status: TeamStatus::Running,
            ..Default::default()
        };
        store.create("11111111-1111-4111-8111-111111111111", &json!({}), team, Vec::new()).unwrap();
        let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
        let size = std::fs::metadata(&wal).map(|meta| meta.len()).unwrap_or(0);
        assert!(size < 1024 * 1024, "the log is {size} bytes after a large write");
    }

    /// Task 105 (ui1, 2026-11-06): `hgs team list` waited behind a writer and
    /// failed with "database is locked" after ~15 s. A writer that holds the
    /// store exclusively (as a rollback-journal writer does while it commits or
    /// spills a large record) no longer stops a reader: it reads the last
    /// committed state at once. The rollback journal it replaces is shown
    /// failing the same read.
    #[test]
    fn a_reader_answers_while_a_writer_holds_the_store() {
        for journal in [Journal::Wal, Journal::Delete] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("teams.sqlite3");
            let writer = filled(&path, journal, 3, 50_000);
            writer.db.execute_batch("BEGIN EXCLUSIVE; UPDATE teams SET payload=json_set(payload,'$.name','Renamed') WHERE id='t0';").unwrap();
            let reader = Store::open_journal(&path, false, journal).unwrap();
            reader.db.busy_timeout(Duration::from_millis(300)).unwrap();
            let started = Instant::now();
            let read = reader.teams(false, 100).and_then(|teams| Ok((teams, reader.heads(10)?.0)));
            let took = started.elapsed();
            match journal {
                Journal::Wal => {
                    let (teams, heads) = read.unwrap();
                    assert!(took < Duration::from_secs(1), "{took:?}");
                    assert_eq!(teams.len(), 3);
                    // The uncommitted change is not seen.
                    assert!(heads.iter().all(|head| head["name"] != "Renamed"), "{heads:?}");
                }
                Journal::Delete => assert!(read.unwrap_err().to_string().contains("locked"), "the old mode waited and failed"),
            }
            writer.db.execute_batch("COMMIT").unwrap();
            drop(reader);
            let after = Store::open_journal(&path, false, journal).unwrap();
            assert!(after.heads(10).unwrap().0.iter().any(|head| head["name"] == "Renamed"));
        }
    }

    /// Task 117 (owner, 2026-11-06): Activity stuck on "Could not refresh
    /// activity: timed out". Every inspection reads the team reasons; a
    /// rollback-journal writer that holds the store kept that read waiting its
    /// whole busy timeout (10 s, measured here with 2 s), past the desktop's
    /// 5 s limit. The read now waits at most 300 ms and answers with the last
    /// good reasons (at most 2 minutes old); in WAL it reads at once.
    #[test]
    fn the_listing_reads_team_reasons_within_its_bound_or_from_the_last_good_ones() {
        use crate::state::session_state::team_reasons_at;
        use crate::state::teams::model::{Member, MemberState, SessionRef};
        for journal in [Journal::Delete, Journal::Wal] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("teams.sqlite3");
            let cache = dir.path().join("waiting-cache.json");
            let mut writer = Store::open_journal(&path, true, journal).unwrap();
            let lead = Member { id: "lead".into(), role: "Lead".into(), agent: "claude".into(), state: MemberState::Active,
                session: Some(SessionRef { name: "claude/x/lead".into(), run_id: "r0".into(), ..Default::default() }), ..Default::default() };
            let team = Team { id: "t0".into(), name: "Team".into(), lead: "lead".into(), status: TeamStatus::Running,
                members: vec![lead], ..Default::default() };
            writer.create("11111111-1111-4111-8111-000000000117", &json!({}), team, vec![]).unwrap();
            let fresh = team_reasons_at(&path, &cache, Duration::from_millis(300), 1000.0);
            assert_eq!(fresh.len(), 1, "{journal:?}");
            assert!(cache.is_file(), "the last good reasons are kept");
            writer.db.execute_batch("BEGIN EXCLUSIVE; UPDATE teams SET payload=json_set(payload,'$.name','Renamed');").unwrap();
            if journal == Journal::Delete {
                // Before task 117: the reader waits out its whole busy timeout.
                let old = Store::open_journal(&path, false, journal).unwrap();
                old.db.busy_timeout(Duration::from_secs(2)).unwrap();
                let started = Instant::now();
                assert!(old.active_teams(32).is_err());
                let waited = started.elapsed();
                assert!(waited >= Duration::from_millis(1900), "{waited:?}");
                eprintln!("task 117 {journal:?}: the unbounded reader waited {waited:?}");
            }
            let started = Instant::now();
            let bounded = team_reasons_at(&path, &cache, Duration::from_millis(300), 1000.0);
            let took = started.elapsed();
            eprintln!("task 117 {journal:?}: the bounded reader answered in {took:?}");
            assert!(took < Duration::from_millis(1500), "{journal:?} {took:?}");
            assert_eq!(bounded, fresh, "{journal:?}: the read or the last good reasons");
            if journal == Journal::Delete {
                // Older than two minutes, the kept reasons are not used.
                assert!(team_reasons_at(&path, &cache, Duration::from_millis(50), 1121.0).is_empty());
            }
            writer.db.execute_batch("COMMIT").unwrap();
        }
    }

    /// Task 105: an existing rollback-journal store becomes WAL at its first
    /// write open with every team kept; an older hgs (whose write open asks for
    /// DELETE) reads it and turns it back; a read-only open works after the last
    /// writer closed, because the log files stay.
    #[test]
    fn an_old_store_migrates_and_an_older_hgs_still_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams.sqlite3");
        drop(filled(&path, Journal::Delete, 2, 1000));
        let old = Store::open_journal(&path, false, Journal::Delete).unwrap();
        assert_eq!(old.journal_mode().unwrap(), "delete");
        drop(old);
        let migrated = Store::open_at(&path, true).unwrap();
        assert_eq!(migrated.journal_mode().unwrap(), "wal");
        assert_eq!(migrated.teams(true, 10).unwrap().len(), 2);
        drop(migrated);
        // The log files stay for a reader that may not create them.
        let wal = files(&path)[2].clone();
        assert!(wal.exists(), "{wal:?}");
        let reader = Store::open_at(&path, false).unwrap();
        assert_eq!(reader.heads(10).unwrap().0.len(), 2);
        drop(reader);
        // An older hgs: its write open asks for DELETE and reads everything.
        let older = Store::open_journal(&path, true, Journal::Delete).unwrap();
        assert_eq!(older.journal_mode().unwrap(), "delete");
        assert_eq!(older.teams(true, 10).unwrap().len(), 2);
        drop(older);
        // This hgs turns it into WAL again.
        assert_eq!(Store::open_at(&path, true).unwrap().journal_mode().unwrap(), "wal");
    }

    /// The read `hgs team list` made before task 105: each record parsed while
    /// the statement still held the store.
    fn old_teams(store: &Store) -> anyhow::Result<Vec<Team>> {
        let mut statement = store.db.prepare("SELECT payload FROM teams WHERE status!='archived' ORDER BY updated DESC LIMIT 1000")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Task 105's measurement: `ZERUS_STORE_COPY` names a copy of a live
    /// teams.sqlite3. Two writers rewrite its largest active team back to back
    /// (the dispatcher and the owner's decisions) for `ZERUS_STORE_SECONDS`
    /// (default 8) while three readers run `team list`'s read (open, teams,
    /// heads) and a status read of that team without pause. Prints each case's
    /// read and write times: before (rollback journal, the old read), the
    /// rollback journal with the new read, and WAL with the new read.
    #[test]
    #[ignore]
    fn store_contention_on_a_copy() {
        let Some(source) = std::env::var_os("ZERUS_STORE_COPY") else { return };
        let seconds: u64 = std::env::var("ZERUS_STORE_SECONDS").ok().and_then(|text| text.parse().ok()).unwrap_or(8);
        let keep: Option<i64> = std::env::var("ZERUS_WAL_KEEP").ok().and_then(|text| text.parse().ok());
        let checkpoint: Option<i64> = std::env::var("ZERUS_WAL_CHECKPOINT").ok().and_then(|text| text.parse().ok());
        let only = std::env::var("ZERUS_STORE_CASES").unwrap_or_default();
        // ZERUS_STORE_LIST: one reader runs exactly `hgs team list`'s work (the
        // store read and the list items) every 250 ms, as the desktop polls.
        let list = std::env::var_os("ZERUS_STORE_LIST").is_some();
        for (case, journal, old_read) in [("before", Journal::Delete, true), ("delete_new_read", Journal::Delete, false), ("after", Journal::Wal, false)] {
            if !only.is_empty() && !only.split(',').any(|wanted| wanted == case) {
                continue;
            }
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("teams.sqlite3");
            std::fs::copy(&source, &path).unwrap();
            let target: String = {
                let store = Store::open_journal(&path, true, journal).unwrap();
                assert_eq!(store.journal_mode().unwrap(), if journal == Journal::Wal { "wal" } else { "delete" });
                store.db.query_row("SELECT id FROM teams WHERE status IN ('running','paused','planning') ORDER BY length(payload) DESC LIMIT 1", [], |row| row.get(0)).unwrap()
            };
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let writers: Vec<_> = (0..2)
                .map(|n| {
                    let (path, target, stop) = (path.clone(), target.clone(), stop.clone());
                    std::thread::spawn(move || {
                        let mut store = Store::open_journal(&path, true, journal).unwrap();
                        if let Some(keep) = keep {
                            store.db.pragma_update(None, "journal_size_limit", keep).unwrap();
                        }
                        if let Some(pages) = checkpoint {
                            store.db.pragma_update(None, "wal_autocheckpoint", pages).unwrap();
                        }
                        let mut times = Vec::new();
                        let mut failed = 0;
                        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                            let started = Instant::now();
                            let result = store.mutate(None, &target, None, |team, events| {
                                team.stop_reason = format!("writer {n} at {:?}", Instant::now());
                                events.push(Event::new("measured", json!({"writer":n})));
                                Ok(Value::Null)
                            });
                            if result.is_err() { failed += 1; }
                            times.push(started.elapsed().as_secs_f64());
                        }
                        (times, failed)
                    })
                })
                .collect();
            let readers: Vec<_> = (0..if list { 1 } else { 3 })
                .map(|_| {
                    let (path, target, stop) = (path.clone(), target.clone(), stop.clone());
                    std::thread::spawn(move || {
                        let (mut reads, mut errors) = (Vec::new(), Vec::new());
                        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                            let started = Instant::now();
                            let read = Store::open_at(&path, false).and_then(|store| {
                                let teams = if old_read { old_teams(&store)? } else { store.teams(false, 200)? };
                                if list {
                                    super::super::commands::list_items(&teams, false);
                                } else {
                                    store.heads(200)?;
                                    store.load(&target)?;
                                }
                                Ok(())
                            });
                            reads.push(started.elapsed().as_secs_f64());
                            if let Err(error) = read {
                                errors.push(error.to_string());
                            }
                            if list {
                                std::thread::sleep(Duration::from_millis(250));
                            }
                        }
                        (reads, errors)
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_secs(seconds));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let (mut writes, mut write_failures) = (Vec::new(), 0);
            for writer in writers {
                let (times, failures) = writer.join().unwrap();
                writes.extend(times);
                write_failures += failures;
            }
            let (mut reads, mut errors) = (Vec::new(), Vec::new());
            for reader in readers {
                let (times, failed) = reader.join().unwrap();
                reads.extend(times);
                errors.extend(failed);
            }
            let over = reads.iter().filter(|read| **read > 1.0).count();
            let stats = |values: &mut Vec<f64>| {
                values.sort_by(f64::total_cmp);
                let at = |q: f64| values.get(((values.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
                json!({"n":values.len(),"p50":at(0.5),"p95":at(0.95),"max":at(1.0)})
            };
            let failures = errors.len();
            errors.dedup();
            println!("{}", json!({"list":list,"case":case,"journal":format!("{journal:?}"),"team":target,"reads":stats(&mut reads),"reads_over_1s":over,
                "read_failures":failures,"read_errors":errors.iter().take(2).collect::<Vec<_>>(),"writes":stats(&mut writes),
                "write_failures":write_failures,"wal_keep":keep,"wal_checkpoint":checkpoint}));
        }
    }

    /// Task 105's write breakdown on `ZERUS_STORE_COPY`: where one write of the
    /// largest active team spends its time (median of 10): reading and parsing
    /// its record, serializing it, the UPDATE with the member projection's
    /// trigger, the same UPDATE without the trigger, and the commit.
    #[test]
    #[ignore]
    fn store_write_breakdown_on_a_copy() {
        let Some(source) = std::env::var_os("ZERUS_STORE_COPY") else { return };
        for journal in [Journal::Delete, Journal::Wal] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("teams.sqlite3");
            std::fs::copy(&source, &path).unwrap();
            let store = Store::open_journal(&path, true, journal).unwrap();
            let target: String = store.db.query_row("SELECT id FROM teams WHERE status IN ('running','paused','planning') ORDER BY length(payload) DESC LIMIT 1", [], |row| row.get(0)).unwrap();
            let median = |mut values: Vec<f64>| { values.sort_by(f64::total_cmp); values[values.len() / 2] };
            let (mut parse, mut serialize, mut update, mut bare, mut commit_ms) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for round in 0..10 {
                let started = Instant::now();
                let payload: String = store.db.query_row("SELECT payload FROM teams WHERE id=?1", params![target], |row| row.get(0)).unwrap();
                let mut team: Team = serde_json::from_str(&payload).unwrap();
                parse.push(started.elapsed().as_secs_f64() * 1000.0);
                team.stop_reason = format!("round {round}");
                let started = Instant::now();
                let text = serde_json::to_string(&team).unwrap();
                serialize.push(started.elapsed().as_secs_f64() * 1000.0);
                store.db.execute_batch("BEGIN IMMEDIATE").unwrap();
                let started = Instant::now();
                store.db.execute("UPDATE teams SET payload=?2 WHERE id=?1", params![target, text]).unwrap();
                update.push(started.elapsed().as_secs_f64() * 1000.0);
                let started = Instant::now();
                store.db.execute_batch("COMMIT").unwrap();
                commit_ms.push(started.elapsed().as_secs_f64() * 1000.0);
                store.db.execute_batch("BEGIN IMMEDIATE; DROP TRIGGER team_rules_update;").unwrap();
                let started = Instant::now();
                store.db.execute("UPDATE teams SET payload=?2 WHERE id=?1", params![target, text]).unwrap();
                bare.push(started.elapsed().as_secs_f64() * 1000.0);
                store.db.execute_batch("ROLLBACK").unwrap();
            }
            println!("{}", json!({"journal":format!("{journal:?}"),"team":target,"payload_bytes":store.load(&target).unwrap().map(|team| serde_json::to_string(&team).unwrap().len()),
                "parse_ms":median(parse),"serialize_ms":median(serialize),"update_with_trigger_ms":median(update),
                "update_without_trigger_ms":median(bare),"commit_ms":median(commit_ms)}));
        }
    }

    /// Task 287: every write's lock hold is a line with the code that took it
    /// (not the store's own), its hold and its wait; a hold of 200 ms or more
    /// is a warning, and a change that fails is a rollback.
    #[test]
    fn every_write_logs_its_lock_hold_with_its_caller() {
        let (_dir, mut store) = store();
        take_lock_lines();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                std::thread::sleep(Duration::from_millis(250));
                team.name.push('!');
                Ok(Value::Null)
            })
            .unwrap();
        assert!(store.mutate(None, "t1", None, |_, _| anyhow::bail!("refused")).is_err());
        let lines = take_lock_lines();
        assert_eq!(lines.len(), 3, "{lines:?}");
        let here = format!("{}:", file!());
        assert!(lines.iter().all(|line| line["caller"].as_str().unwrap().starts_with(&here)), "the caller is this test: {lines:?}");
        assert!(lines.iter().all(|line| line["team"] == "t1" && line["waited_ms"].is_u64() && line["pid"].is_u64()), "{lines:?}");
        assert_eq!(lines[0]["outcome"], "commit");
        assert!(lines[1]["held_ms"].as_u64().unwrap() >= 250, "{}", lines[1]);
        assert_eq!((lines[1]["level"].as_str(), lines[1]["outcome"].as_str()), (Some("warn"), Some("commit")));
        assert_eq!(lines[2]["outcome"], "rollback");
        assert!(lock_holder().is_none(), "no lock is left held");
    }

    /// Task 287: a process started while the write lock is held (a git that
    /// slipped into a transaction) is a warning naming the holder.
    #[test]
    fn a_process_under_the_write_lock_is_a_warning() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        take_lock_lines();
        note_process("git", &["outside".into()]);
        assert!(take_lock_lines().is_empty(), "no lock held: nothing to say");
        store
            .mutate(None, "t1", None, |_, _| {
                assert!(lock_holder().is_some());
                note_process("git", &["inside".into()]);
                Ok(Value::Null)
            })
            .unwrap();
        let lines = take_lock_lines();
        let started: Vec<&Value> = lines.iter().filter(|line| line["process"] == "git").collect();
        assert_eq!(started.len(), 1, "{lines:?}");
        assert_eq!((started[0]["level"].as_str(), started[0]["args"].clone()), (Some("warn"), json!(["inside"])));
    }

    /// Task 287: the worker's changes to a team in one tick are one
    /// transaction; each step sees the earlier ones, a failed change leaves the
    /// copy as it was, and a change with a request id writes the batch first.
    #[test]
    fn a_worker_tick_is_one_transaction() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        take_lock_lines();
        store.begin_batch("t1").unwrap();
        for step in ["a", "b", "c"] {
            store
                .mutate(None, "t1", None, |team, events| {
                    team.name.push_str(step);
                    events.push(Event::new("ran", json!({"step":step})));
                    Ok(Value::Null)
                })
                .unwrap();
        }
        assert_eq!(store.load("t1").unwrap().unwrap().name, "Checkoutabc", "the next step sees the earlier ones");
        let written: i64 = store.db.query_row("SELECT revision FROM teams WHERE id='t1'", [], |row| row.get(0)).unwrap();
        assert_eq!(written, 1, "nothing is written during the steps");
        assert!(take_lock_lines().is_empty(), "no lock is taken during the steps");
        assert!(store.end_batch().unwrap());
        assert_eq!(take_lock_lines().len(), 1, "one transaction for the tick");
        let saved = store.load("t1").unwrap().unwrap();
        assert_eq!((saved.name.as_str(), saved.revision), ("Checkoutabc", 2));
        assert_eq!(store.events("t1", None, 10).unwrap()["items"].as_array().unwrap().len(), 3, "every step's events");
        // A failed change leaves the copy as it was; nothing changed, nothing written.
        store.begin_batch("t1").unwrap();
        assert!(store
            .mutate(None, "t1", None, |team, _| {
                team.name.clear();
                anyhow::bail!("refused")
            })
            .is_err());
        assert_eq!(store.load("t1").unwrap().unwrap().name, "Checkoutabc");
        assert!(!store.end_batch().unwrap());
        // A change with a request id writes the batch first and the batch goes on.
        store.begin_batch("t1").unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                team.goal = "worker".into();
                Ok(Value::Null)
            })
            .unwrap();
        reassign(&mut store, "22222222-2222-4222-8222-222222222222").unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                team.stop_reason = "later".into();
                Ok(Value::Null)
            })
            .unwrap();
        store.end_batch().unwrap();
        let saved = store.load("t1").unwrap().unwrap();
        assert_eq!((saved.goal.as_str(), saved.name.as_str(), saved.stop_reason.as_str()), ("worker", "Checkoutabc+", "later"));
    }

    /// Task #6 of team c536f0e5 (P1.4, owner 2026-11-23): a tick whose steps
    /// change nothing and tell nothing writes nothing: the revision and
    /// `updated` stay. A change put back within the tick is nothing too; an
    /// event alone is written.
    #[test]
    fn a_no_op_tick_keeps_the_revision() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        let before = store.load("t1").unwrap().unwrap();
        take_lock_lines();
        store.begin_batch("t1").unwrap();
        for _ in 0..3 {
            let receipt = store.mutate(None, "t1", None, |team, _| {
                team.name = "Checkout".into();
                Ok(json!({}))
            });
            assert_eq!(receipt.unwrap()["revision"], before.revision, "the receipt names the kept revision");
        }
        assert!(!store.end_batch().unwrap(), "nothing to write");
        assert!(take_lock_lines().is_empty(), "no transaction");
        let after = store.load("t1").unwrap().unwrap();
        assert_eq!((after.revision, after.updated), (before.revision, before.updated));
        let written: (i64, f64) = store.db.query_row("SELECT revision,updated FROM teams WHERE id='t1'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(written, (before.revision as i64, before.updated));
        // Changed and put back in the same tick: nothing either.
        store.begin_batch("t1").unwrap();
        store.mutate(None, "t1", None, |team, _| { team.goal = "later".into(); Ok(Value::Null) }).unwrap();
        store.mutate(None, "t1", None, |team, _| { team.goal = String::new(); Ok(Value::Null) }).unwrap();
        assert!(!store.end_batch().unwrap());
        assert_eq!(store.load("t1").unwrap().unwrap().revision, before.revision);
        // A step's event is written, with a new revision.
        store.begin_batch("t1").unwrap();
        store.mutate(None, "t1", None, |_, events| { events.push(Event::new("ran", json!({}))); Ok(Value::Null) }).unwrap();
        assert!(store.end_batch().unwrap());
        assert_eq!(store.load("t1").unwrap().unwrap().revision, before.revision + 1);
        // A request suspends the batch: a batch with nothing to write writes nothing first.
        store.begin_batch("t1").unwrap();
        store.mutate(None, "t1", None, |_, _| Ok(Value::Null)).unwrap();
        take_lock_lines();
        reassign(&mut store, "22222222-2222-4222-8222-222222222222").unwrap();
        assert_eq!(take_lock_lines().len(), 1, "only the request's own transaction");
        assert!(!store.end_batch().unwrap());
    }

    /// Task 287: another writer that changed the team during the worker's tick
    /// keeps its change; the worker's are applied over it, and a value both
    /// changed is theirs.
    #[test]
    fn a_batch_merges_over_another_writers_change() {
        use super::super::model::Member;
        let (dir, mut store) = store();
        let mut first = team("t1");
        first.members.push(Member { id: "dev".into(), ..Default::default() });
        store.create(REQUEST, &json!({}), first, vec![]).unwrap();
        let mut other = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        store.begin_batch("t1").unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                team.goal = "worker".into();
                team.name = "Worker".into();
                team.members[0].role = "Backend".into();
                Ok(Value::Null)
            })
            .unwrap();
        other
            .mutate(Some(("22222222-2222-4222-8222-222222222222", &json!({}))), "t1", None, |team, _| {
                team.stop_reason = "owner".into();
                team.name = "Owner".into();
                team.members.push(Member { id: "qa".into(), ..Default::default() });
                Ok(json!({}))
            })
            .unwrap();
        store.end_batch().unwrap();
        let saved = store.load("t1").unwrap().unwrap();
        assert_eq!((saved.goal.as_str(), saved.stop_reason.as_str()), ("worker", "owner"), "both changes are kept");
        assert_eq!(saved.name, "Owner", "a value both changed is theirs");
        let members: Vec<(&str, &str)> = saved.members.iter().map(|member| (member.id.as_str(), member.role.as_str())).collect();
        assert_eq!(members, [("dev", "Backend"), ("qa", "")]);
        assert_eq!(saved.revision, 3);
    }

    #[test]
    fn the_merge_goes_record_by_record() {
        let base = json!({"tasks":[{"id":1,"status":"todo","n":0},{"id":2,"status":"todo"}],"list":[1,2],"name":"a"});
        let ours = json!({"tasks":[{"id":1,"status":"in_progress","n":0},{"id":2,"status":"todo"},{"id":3,"status":"todo"}],"list":[1,2,3],"name":"a"});
        let theirs = json!({"tasks":[{"id":1,"status":"todo","n":1},{"id":2,"status":"cancelled"}],"list":[2],"name":"b"});
        let merged = merge3(&base, &ours, &theirs);
        assert_eq!(merged["tasks"], json!([{"id":1,"status":"in_progress","n":1},{"id":2,"status":"cancelled"},{"id":3,"status":"todo"}]));
        assert_eq!(merged["list"], json!([2]), "a plain list both changed: theirs");
        assert_eq!(merged["name"], "b");
        assert_eq!(merge3(&json!({"a":1,"b":2}), &json!({"a":1}), &json!({"a":1,"b":2,"c":3})), json!({"a":1,"c":3}), "removed by us, untouched by them");
        assert_eq!(merge3(&json!({"a":1}), &json!({"a":1,"b":2}), &json!({})), json!({"b":2}), "added by us, a removal by them kept");
    }

    /// Task #6 (team ede724c4): a member's own call finds its team through the
    /// members projection: only active teams whose member list names the
    /// session's name, run or conversation, and nothing when nothing does.
    #[test]
    fn a_session_finds_its_active_teams_through_the_member_projection() {
        use super::super::model::{Member, SessionRef, TeamStatus};
        let (_dir, mut store) = store();
        let member = |id: &str, name: &str, run: &str, conversation: &str| Member {
            id: id.into(),
            session: Some(SessionRef { name: name.into(), run_id: run.into(), conversation_id: conversation.into(), ..Default::default() }),
            ..Default::default()
        };
        let mut mine = team("mine");
        mine.status = TeamStatus::Running;
        mine.members.push(member("dev", "claude/x/dev", "run-1", "conv-1"));
        let mut other = team("other");
        other.status = TeamStatus::Running;
        other.members.push(member("qa", "claude/x/qa", "run-2", "conv-2"));
        let mut finished = team("finished");
        finished.status = TeamStatus::Done;
        finished.members.push(member("dev", "claude/x/dev", "run-1", "conv-1"));
        for (n, team) in [mine, other, finished].into_iter().enumerate() {
            store.create(&format!("11111111-1111-4111-8111-11111111111{n}"), &json!({}), team, vec![]).unwrap();
        }
        let ids = |found: Option<Vec<Team>>| found.unwrap().into_iter().map(|team| team.id).collect::<Vec<_>>();
        assert_eq!(ids(store.session_teams("claude/x/dev", "", "").unwrap()), ["mine"], "a finished team is not read");
        assert_eq!(ids(store.session_teams("renamed", "run-1", "").unwrap()), ["mine"], "by its run after a rename");
        assert_eq!(ids(store.session_teams("", "", "conv-2").unwrap()), ["other"], "a DeepSeek member by its conversation");
        assert!(ids(store.session_teams("nobody", "none", "").unwrap()).is_empty());
        assert!(store.session_teams("", "", "").unwrap().is_none(), "no identity: the caller reads the teams as before");
    }

    /// Task 287: the member projection's trigger parsed the whole record on
    /// every write; it runs only when the members change, and for a write of
    /// an older hgs (which leaves `members_rev` behind).
    #[test]
    fn the_member_projection_is_rebuilt_only_when_members_change() {
        use super::super::model::Member;
        let (dir, mut store) = store();
        let mut first = team("t1");
        first.members.push(Member { id: "dev".into(), ..Default::default() });
        store.create(REQUEST, &json!({}), first, vec![]).unwrap();
        let rows = |store: &Store| -> Vec<(String, String)> {
            store
                .db
                .prepare("SELECT member,name FROM team_rule_members WHERE team='t1' ORDER BY member")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let fresh = |ids: &[&str]| ids.iter().map(|id| ((*id).to_owned(), String::new())).collect::<Vec<_>>();
        assert_eq!(rows(&store), fresh(&["dev"]));
        store.db.execute("UPDATE team_rule_members SET name='stale'", []).unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                team.goal = "g".into();
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(rows(&store), [("dev".to_owned(), "stale".to_owned())], "the members did not change: not rebuilt");
        store
            .mutate(None, "t1", None, |team, _| {
                team.members.push(Member { id: "qa".into(), ..Default::default() });
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(rows(&store), fresh(&["dev", "qa"]), "a members change rebuilds it");
        // An older hgs writes the record alone: rebuilt from the record.
        store.db.execute("UPDATE team_rule_members SET name='stale'", []).unwrap();
        store.db.execute("UPDATE teams SET revision=revision+1,payload=json_set(payload,'$.goal','old') WHERE id='t1'", []).unwrap();
        assert_eq!(rows(&store), fresh(&["dev", "qa"]));
        // A store with the trigger of before is given the new one on open.
        store
            .db
            .execute_batch("DROP TRIGGER team_rules_update; CREATE TRIGGER team_rules_update AFTER UPDATE OF payload ON teams BEGIN DELETE FROM team_rule_members WHERE team=NEW.id; END;")
            .unwrap();
        drop(store);
        let store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        let sql: String = store.db.query_row("SELECT sql FROM sqlite_master WHERE name='team_rules_update'", [], |row| row.get(0)).unwrap();
        assert!(sql.contains("members_rev"), "{sql}");
    }

    /// Task 288: a team of `tasks` tasks with `attempts` attempts each, about
    /// a kilobyte an attempt, as a long-running team's record grows.
    fn big_team(id: &str, tasks: u32, attempts: u32) -> Team {
        use super::super::model::{Attempt, AttemptState, Task};
        let mut team = team(id);
        for task in 1..=tasks {
            team.tasks.push(Task {
                id: task,
                title: format!("Task {task}"),
                assignee: "dev".into(),
                attempts: (1..=attempts)
                    .map(|n| Attempt {
                        id: format!("{task:08}-0000-4000-8000-{n:012}"),
                        n,
                        member: "dev".into(),
                        state: AttemptState::Reported,
                        error: format!("attempt {n} of task {task}: ").repeat(30),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            });
        }
        team
    }

    fn row_counts(store: &Store, team: &str) -> (i64, i64) {
        store
            .db
            .query_row(
                "SELECT (SELECT COUNT(*) FROM team_tasks WHERE team=?1),(SELECT COUNT(*) FROM team_attempts WHERE team=?1)",
                params![team],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    /// Task 288: a large team stored whole (as every store before task 288
    /// holds it) moves to rows at the next write open, in one transaction; the
    /// assembled view equals the old JSON; a second open changes nothing; an
    /// older hgs refuses the migrated head instead of writing it back without
    /// its tasks.
    #[test]
    fn a_large_store_migrates_to_rows_once_and_reads_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams/teams.sqlite3");
        rows::set_legacy(true);
        {
            let mut store = Store::open_at(&path, true).unwrap();
            store.create(REQUEST, &json!({}), big_team("t1", 400, 3), vec![]).unwrap();
            store.create("22222222-2222-4222-8222-222222222222", &json!({}), big_team("t2", 20, 1), vec![]).unwrap();
        }
        rows::set_legacy(false);
        let db = Connection::open(&path).unwrap();
        let (layout, payload): (Option<i64>, String) =
            db.query_row("SELECT layout,payload FROM teams WHERE id='t1'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        drop(db);
        assert!(layout.is_none(), "stored whole, not migrated yet");
        let old: Team = serde_json::from_str(&payload).unwrap();
        take_lock_lines();
        let store = Store::open_at(&path, true).unwrap();
        assert_eq!(take_lock_lines().len(), 2, "one transaction a team");
        let loaded = store.load("t1").unwrap().unwrap();
        assert_eq!(serde_json::to_value(&loaded).unwrap(), serde_json::to_value(&old).unwrap(), "the assembled view equals the old JSON");
        assert_eq!(row_counts(&store, "t1"), (400, 1200));
        let head: String = store.db.query_row("SELECT payload FROM teams WHERE id='t1'", [], |row| row.get(0)).unwrap();
        assert!(head.len() * 50 < payload.len(), "the head keeps the small shared fields: {} of {} bytes", head.len(), payload.len());
        assert!(serde_json::from_str::<Team>(&head).is_err(), "an older hgs refuses the head");
        // The list reads assemble the same teams.
        let listed = store.teams(true, 10).unwrap();
        assert_eq!(serde_json::to_value(listed.iter().find(|team| team.id == "t1").unwrap()).unwrap(), serde_json::to_value(&old).unwrap());
        drop(store);
        // Idempotent: the next open finds nothing to migrate.
        let again = Store::open_at(&path, true).unwrap();
        assert!(take_lock_lines().is_empty(), "a second open migrates nothing");
        assert_eq!(serde_json::to_value(again.load("t1").unwrap().unwrap()).unwrap(), serde_json::to_value(&old).unwrap());
        // A team whose task ids repeat keeps the whole record.
        let mut twice = team("t3");
        twice.tasks = big_team("x", 2, 1).tasks;
        twice.tasks[1].id = 1;
        let mut again = again;
        again.create("33333333-3333-4333-8333-333333333333", &json!({}), twice.clone(), vec![]).unwrap();
        assert_eq!(row_counts(&again, "t3"), (0, 0));
        assert_eq!(again.load("t3").unwrap().unwrap().tasks.len(), 2);
    }

    /// Task 288: a change writes only the rows it touches: one attempt's row
    /// for an attempt's change, the task's own row for its own fields, the
    /// head only when a shared field changed; another store brings its copy up
    /// to date from the rows written since and reads the same team.
    #[test]
    fn a_change_writes_only_its_rows_and_another_store_reads_them() {
        use super::super::model::Task;
        let (dir, mut store) = store();
        let path = dir.path().join("teams/teams.sqlite3");
        store.create(REQUEST, &json!({}), big_team("t1", 200, 3), vec![]).unwrap();
        let mut other = Store::open_at(&path, true).unwrap();
        assert_eq!(other.load("t1").unwrap().unwrap().tasks.len(), 200, "the other store keeps a copy");
        let written = |store: &Store, revision: i64| -> (i64, i64, i64) {
            store
                .db
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM team_tasks WHERE team='t1' AND rev=?1),(SELECT COUNT(*) FROM team_attempts WHERE team='t1' AND rev=?1),
                        (SELECT head_rev FROM teams WHERE id='t1')",
                    params![revision],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        store
            .mutate(None, "t1", None, |team, _| {
                team.task_mut(7).unwrap().attempts[1].error = "changed".into();
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(written(&store, 2), (0, 1, 1), "one attempt row; no task row; the head as it was");
        store
            .mutate(None, "t1", None, |team, _| {
                team.task_mut(3).unwrap().title = "Renamed task".into();
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(written(&store, 3), (1, 0, 1), "the task's own row only");
        store
            .mutate(None, "t1", None, |team, _| {
                team.goal = "A new goal".into();
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(written(&store, 4).2, 4, "a shared field writes the head");
        // Removed, added, moved, attempts cut and added.
        store
            .mutate(None, "t1", None, |team, _| {
                team.tasks.retain(|task| task.id != 5);
                let last = team.tasks.pop().unwrap();
                team.tasks.insert(0, last);
                team.tasks.push(Task { id: 999, title: "New".into(), ..Default::default() });
                team.task_mut(9).unwrap().attempts.truncate(1);
                let more = team.task_mut(10).unwrap().attempts[0].clone();
                team.task_mut(10).unwrap().attempts.push(more);
                Ok(Value::Null)
            })
            .unwrap();
        // The other store's copy follows the rows written since, and equals a fresh read.
        let followed = other.load("t1").unwrap().unwrap();
        let fresh = Store::open_at(&path, false).unwrap().load("t1").unwrap().unwrap();
        assert_eq!(serde_json::to_value(&followed).unwrap(), serde_json::to_value(&fresh).unwrap());
        assert_eq!(serde_json::to_value(&followed).unwrap(), serde_json::to_value(store.load("t1").unwrap().unwrap()).unwrap());
        assert_eq!(followed.tasks[0].id, 200);
        assert!(followed.task(5).is_none() && followed.task(999).is_some());
        assert_eq!((followed.task(9).unwrap().attempts.len(), followed.task(10).unwrap().attempts.len()), (1, 4));
        assert_eq!(followed.task(3).unwrap().title, "Renamed task");
        assert_eq!(followed.goal, "A new goal");
        // A change through the other store starts from its followed copy.
        other
            .mutate(None, "t1", None, |team, _| {
                team.task_mut(999).unwrap().title = "From the other store".into();
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(store.load("t1").unwrap().unwrap().task(999).unwrap().title, "From the other store");
        assert_eq!(row_counts(&store, "t1"), (200, 596));
    }

    /// Task 288: members' calls while the worker ticks a large team never wait
    /// a second for the write lock: the team is read before the lock and a
    /// change writes only its rows.
    #[test]
    fn member_calls_never_wait_a_second_behind_worker_ticks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let (dir, mut store) = store();
        let path = dir.path().join("teams/teams.sqlite3");
        store.create(REQUEST, &json!({}), big_team("t1", 600, 3), vec![]).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let worker = {
            let (path, done) = (path.clone(), done.clone());
            std::thread::spawn(move || {
                let mut store = Store::open_at(&path, true).unwrap();
                let mut ticks = 0u32;
                while !done.load(Ordering::SeqCst) && ticks < 400 {
                    store.begin_batch("t1").unwrap();
                    for step in 0..5u32 {
                        store
                            .mutate(None, "t1", None, |team, _| {
                                let task = 1 + (ticks * 5 + step) % 600;
                                team.task_mut(task).unwrap().attempts[0].error = format!("tick {ticks}");
                                Ok(Value::Null)
                            })
                            .unwrap();
                    }
                    store.end_batch().unwrap();
                    ticks += 1;
                    // The worker pauses between teams (task 287's TEAM_YIELD)
                    // and ticks seconds apart; SQLite queues nobody.
                    std::thread::sleep(Duration::from_millis(100));
                }
                let held = take_lock_lines().iter().filter_map(|line| line["held_ms"].as_u64()).max().unwrap_or(0);
                (ticks, held)
            })
        };
        let members: Vec<_> = (0..3u32)
            .map(|member| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open_at(&path, true).unwrap();
                    for call in 0..12u32 {
                        let input = json!({"member":member,"call":call});
                        store
                            .mutate(Some((&uuid::Uuid::new_v4().to_string(), &input)), "t1", None, |team, _| {
                                team.task_mut(100 + member * 100 + call).unwrap().title = format!("member {member} call {call}");
                                Ok(json!({}))
                            })
                            .unwrap();
                    }
                    let lines = take_lock_lines();
                    let waited = lines.iter().map(|line| line["waited_ms"].as_u64().unwrap_or(0)).max().unwrap_or(0);
                    let held = lines.iter().filter_map(|line| line["held_ms"].as_u64()).max().unwrap_or(0);
                    (waited, held)
                })
            })
            .collect();
        let calls: Vec<(u64, u64)> = members.into_iter().map(|member| member.join().unwrap()).collect();
        done.store(true, Ordering::SeqCst);
        let (ticks, worker_held) = worker.join().unwrap();
        assert!(ticks > 0);
        // Task #44 (team ede724c4): the bound is the holds this run measured,
        // so a loaded machine (waits of 2 to 7.5 s while every hold grew
        // alike) passes. A waiting call polls on SQLite's busy handler (sleeps
        // of up to 100 ms) and the worker pauses 100 ms between batches, so a
        // descheduled member may miss a window or two: at most three worker
        // batches, the other two members' calls and the polling (seen: a
        // 744 ms wait beside 277 ms batches). A queue of batches or a lock
        // held across ticks still fails, and the holds themselves are bounded
        // by the machine's speed below (task 288: rows, not whole records).
        // A shared CI runner deschedules a member for longer still (seen: a
        // 1139 ms wait beside 106 ms batches and 137 ms calls on GitHub's
        // Windows runner), so the slack is a fourth batch and a second: a lock
        // held across the worker's ticks keeps a member waiting for many
        // batches, far beyond it.
        let member_held = calls.iter().map(|(_, held)| *held).max().unwrap_or(0);
        let bound = 4 * worker_held + 2 * member_held + 1000;
        let waits: Vec<u64> = calls.iter().map(|(waited, _)| *waited).collect();
        assert!(waits.iter().all(|wait| *wait <= bound), "the members' longest waits {waits:?} ms exceed one worker batch ({worker_held} ms) and two member calls ({member_held} ms each)");
        let reference = crate::state::reference_work().as_millis() as u64;
        assert!(worker_held <= 1000 + 50 * reference && member_held <= 1000 + 50 * reference,
            "a hold grew beyond what the machine's speed explains: worker {worker_held} ms, member {member_held} ms, reference work {reference} ms");
        let saved = store.load("t1").unwrap().unwrap();
        assert_eq!(saved.task(100).unwrap().title, "member 0 call 0");
        assert_eq!(saved.task(311).unwrap().title, "member 2 call 11");
    }

    /// Task 288's measurement: lock holds on a copy of a live store
    /// (ZERUS_STORE_COPY, read only), its largest active team, before (the
    /// whole-record layout of task 287) and after (rows): 20 member changes
    /// with a request id and 20 worker ticks each way. The live store is never
    /// written: it is copied with VACUUM INTO.
    #[test]
    #[ignore]
    fn lock_times_on_a_copy_before_and_after_rows() {
        let Some(source) = std::env::var_os("ZERUS_STORE_COPY") else { return };
        let live = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (target, bytes): (String, i64) = live
            .query_row("SELECT id,length(payload) FROM teams WHERE status IN ('running','paused','planning') ORDER BY length(payload) DESC LIMIT 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        let stats = |mut values: Vec<u64>| {
            values.sort_unstable();
            let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
            json!({"median_ms":at(0.5),"p95_ms":at(0.95),"max_ms":values[values.len() - 1]})
        };
        let mut measured = serde_json::Map::new();
        for (name, legacy) in [("before", true), ("after", false)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("teams.sqlite3");
            live.execute("VACUUM INTO ?1", params![path.display().to_string()]).unwrap();
            rows::set_legacy(legacy);
            take_lock_lines();
            let started = Instant::now();
            let mut store = Store::open_at(&path, true).unwrap();
            let opened_ms = started.elapsed().as_millis() as u64;
            let migration: Vec<u64> = take_lock_lines().iter().filter_map(|line| line["held_ms"].as_u64()).collect();
            for round in 0..20 {
                let input = json!({"round":round});
                store
                    .mutate(Some((&uuid::Uuid::new_v4().to_string(), &input)), &target, None, |team, _| {
                        if let Some(task) = team.tasks.last_mut() {
                            task.reopen_note = format!("measured {round}");
                        }
                        Ok(json!({}))
                    })
                    .unwrap();
            }
            let member: Vec<u64> = take_lock_lines().iter().filter_map(|line| line["held_ms"].as_u64()).collect();
            for round in 0..20 {
                store.begin_batch(&target).unwrap();
                store
                    .mutate(None, &target, None, |team, _| {
                        team.stop_reason = format!("tick {round}");
                        if let Some(attempt) = team.tasks.iter_mut().rev().find_map(|task| task.attempts.last_mut()) {
                            attempt.error = format!("tick {round}");
                        }
                        Ok(Value::Null)
                    })
                    .unwrap();
                store.end_batch().unwrap();
            }
            let worker: Vec<u64> = take_lock_lines().iter().filter_map(|line| line["held_ms"].as_u64()).collect();
            rows::set_legacy(false);
            measured.insert(
                name.into(),
                json!({"member_change":stats(member),"worker_tick":stats(worker),"open_ms":opened_ms,
                    "migration_held_ms":migration.iter().max().copied().unwrap_or(0)}),
            );
        }
        println!("{}", json!({"task":288,"team":target,"record_bytes":bytes,"lock_held":measured}));
    }

    /// Task #4 (team ede724c4): a team with lead changes whose snapshots are
    /// in memory, as an ops change records them.
    fn team_with_lead_changes(id: &str) -> Team {
        use super::super::model::LeadChange;
        let mut team = big_team(id, 5, 1);
        team.phases.push(Phase { n: 1, tasks: vec![1, 2, 3, 4, 5], ..Default::default() });
        for n in 1..=3u32 {
            let mut before = team.tasks[n as usize].clone();
            before.title = format!("Before change {n}");
            team.lead_changes.push(LeadChange {
                id: format!("lc{n}"),
                op: "edit-draft".into(),
                summary: format!("edited #{} Task {}", before.id, before.id),
                by: "lead".into(),
                at: f64::from(n),
                tasks_before: vec![before],
                phases_before: vec![Phase { n: 1, tasks: vec![1, 2, 3], ..Default::default() }],
                ..Default::default()
            });
        }
        team.lead_changes.push(LeadChange { id: "lc4".into(), op: "use-last-reply".into(), summary: "a report".into(), by: "lead".into(), at: 4.0, ..Default::default() });
        team
    }

    /// Rewrites a stored team as task 288's layout 2 holds it: the whole head
    /// with the lead changes' snapshots, no `team_lead_changes` rows.
    fn as_task_288(store: &Store, team: &Team, payload: Option<&str>) {
        let mut head = serde_json::to_value(team).unwrap();
        head["tasks"] = json!("stored in team_tasks (Zerus team store layout 2): this hgs is too old to read this team");
        head["revision"] = json!(0);
        head["updated"] = json!(0.0);
        let head = payload.map_or_else(|| head.to_string(), str::to_owned);
        store.db.execute("UPDATE teams SET payload=?2,layout=2 WHERE id=?1", params![team.id, head]).unwrap();
        store.db.execute("DELETE FROM team_lead_changes WHERE team=?1", params![team.id]).unwrap();
    }

    fn history_rows(store: &Store, team: &str) -> i64 {
        store.db.query_row("SELECT COUNT(*) FROM team_lead_changes WHERE team=?1", params![team], |row| row.get(0)).unwrap()
    }

    /// Task #4 (team ede724c4): a team of task 288's layout 2 moves its lead
    /// changes' snapshots to `team_lead_changes` at the next write open, one
    /// transaction a team; the task rows stay; the view keeps every change's
    /// summary fields and the ids it changed, and the snapshots read back on
    /// demand. A team that cannot migrate never fails the open and migrates
    /// at a later open; a second open migrates nothing; an hgs of task 288
    /// refuses the migrated head.
    #[test]
    fn a_layout_2_team_moves_its_lead_change_history_to_rows_once() {
        let (dir, mut store) = store();
        let path = dir.path().join("teams/teams.sqlite3");
        let original = team_with_lead_changes("t1");
        store.create(REQUEST, &json!({}), original.clone(), vec![]).unwrap();
        store.create("22222222-2222-4222-8222-222222222222", &json!({}), team_with_lead_changes("t2"), vec![]).unwrap();
        assert_eq!(history_rows(&store, "t1"), 3, "a new team keeps its snapshots in rows");
        // As this hgs keeps it in memory: the ids instead of the snapshots.
        let expected = store.load("t1").unwrap().unwrap();
        assert!(expected.lead_changes.iter().all(|change| change.tasks_before.is_empty() && change.phases_before.is_empty()));
        assert_eq!((expected.lead_changes[0].changed.clone(), expected.lead_changes[0].phases_changed.clone()), (vec![2], vec![1]));
        let with_history = |team: &Team| {
            let mut old = team.clone();
            old.lead_changes = original.lead_changes.clone();
            old
        };
        let t2 = store.load("t2").unwrap().unwrap();
        let tasks_written: i64 = store.db.query_row("SELECT MAX(rev) FROM team_tasks WHERE team='t1'", [], |row| row.get(0)).unwrap();
        as_task_288(&store, &with_history(&expected), None);
        // t2's head is unreadable: it stays as it is and never fails the open.
        as_task_288(&store, &t2, Some("{not json"));
        let old_head: String = store.db.query_row("SELECT payload FROM teams WHERE id='t1'", [], |row| row.get(0)).unwrap();
        assert!(old_head.contains("tasks_before"));
        drop(store);
        take_lock_lines();
        let store = Store::open_at(&path, true).unwrap();
        assert_eq!(take_lock_lines().len(), 2, "one transaction a team");
        let (layout, head): (i64, String) = store.db.query_row("SELECT layout,payload FROM teams WHERE id='t1'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(layout, rows::ROWS);
        assert!(!head.contains("tasks_before") && !head.contains("phases_before"), "{head}");
        assert!(head.contains(r#""changed":[2]"#) && head.contains(r#""phases_changed":[1]"#), "{head}");
        assert!(head.len() * 2 < old_head.len(), "{} of {} bytes", head.len(), old_head.len());
        assert_eq!(history_rows(&store, "t1"), 3);
        let rewritten: i64 = store.db.query_row("SELECT MAX(rev) FROM team_tasks WHERE team='t1'", [], |row| row.get(0)).unwrap();
        assert_eq!(rewritten, tasks_written, "the task rows stay as they were");
        let loaded = store.load("t1").unwrap().unwrap();
        assert_eq!(serde_json::to_value(&loaded).unwrap(), serde_json::to_value(&expected).unwrap());
        let history = rows::lead_change_history(&store.db, "t1", "lc2").unwrap().unwrap();
        assert_eq!((history.tasks_before, history.phases_before), (original.lead_changes[1].tasks_before.clone(), original.lead_changes[1].phases_before.clone()));
        // An hgs of task 288 reads a layout other than 2 as a whole record: refused.
        assert!(serde_json::from_str::<Team>(&head).is_err(), "an older hgs refuses the head");
        let left: Option<i64> = store.db.query_row("SELECT layout FROM teams WHERE id='t2'", [], |row| row.get(0)).unwrap();
        assert_eq!(left, Some(rows::TASK_ROWS), "the unreadable team stays for a later open");
        // Resumes: once t2 reads, the next open migrates t2 alone.
        as_task_288(&store, &with_history(&t2), None);
        drop(store);
        let store = Store::open_at(&path, true).unwrap();
        assert_eq!(take_lock_lines().len(), 1, "only the team left");
        assert_eq!(history_rows(&store, "t2"), 3);
        drop(store);
        let mut store = Store::open_at(&path, true).unwrap();
        assert!(take_lock_lines().is_empty(), "a second open migrates nothing");
        // A read-only store reads the same team.
        let reader = Store::open_at(&path, false).unwrap();
        assert_eq!(serde_json::to_value(reader.load("t1").unwrap().unwrap()).unwrap(), serde_json::to_value(&loaded).unwrap());
        // A head change leaves the history rows alone; a trimmed change loses its row.
        store
            .mutate(None, "t1", None, |team, _| {
                team.goal = "A new goal".into();
                Ok(Value::Null)
            })
            .unwrap();
        let revs: i64 = store.db.query_row("SELECT MAX(rev) FROM team_lead_changes WHERE team='t1'", [], |row| row.get(0)).unwrap();
        assert!(revs < store.load("t1").unwrap().unwrap().revision as i64, "no history row rewritten");
        store
            .mutate(None, "t1", None, |team, _| {
                team.lead_changes.remove(0);
                Ok(Value::Null)
            })
            .unwrap();
        assert_eq!(history_rows(&store, "t1"), 2);
        // The whole-record layout (task ids that repeat) carries the snapshots
        // again, and no row is left behind.
        store
            .mutate(None, "t1", None, |team, _| {
                team.tasks[1].id = team.tasks[0].id;
                Ok(Value::Null)
            })
            .unwrap();
        let whole: String = store.db.query_row("SELECT payload FROM teams WHERE id='t1'", [], |row| row.get(0)).unwrap();
        let whole: Team = serde_json::from_str(&whole).unwrap();
        assert_eq!(whole.lead_changes[0].tasks_before, original.lead_changes[1].tasks_before);
        assert_eq!(history_rows(&store, "t1"), 0);
    }

    /// Task #4's measurement on `ZERUS_STORE_COPY` (read only; copied with
    /// VACUUM INTO): the largest team's head before and after the migration,
    /// the migration's lock hold, and that its lead changes, status view and a
    /// revert answer the same. `ZERUS_STORE_OUT` keeps both copies (before.sqlite3,
    /// after.sqlite3) for timing `hgs team status` on them.
    #[test]
    #[ignore]
    fn lead_change_history_on_a_copy() {
        let Some(source) = std::env::var_os("ZERUS_STORE_COPY") else { return };
        let live = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let target: String = std::env::var("ZERUS_STORE_TEAM").unwrap_or_else(|_| {
            live.query_row("SELECT id FROM teams ORDER BY length(payload) DESC LIMIT 1", [], |row| row.get(0)).unwrap()
        });
        let dir = tempfile::tempdir().unwrap();
        let out = std::env::var_os("ZERUS_STORE_OUT").map(std::path::PathBuf::from).unwrap_or_else(|| dir.path().to_path_buf());
        let (before, after) = (out.join("before/teams/teams.sqlite3"), out.join("after/teams/teams.sqlite3"));
        for path in [&before, &after] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(path);
            live.execute("VACUUM INTO ?1", params![path.display().to_string()]).unwrap();
        }
        let head_bytes = |path: &Path| -> (i64, Option<i64>) {
            Connection::open(path).unwrap().query_row("SELECT length(payload),layout FROM teams WHERE id=?1", params![target], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        };
        let old = Store::open_at(&before, false).unwrap().load(&target).unwrap().unwrap();
        take_lock_lines();
        let started = Instant::now();
        let store = Store::open_at(&after, true).unwrap();
        let open_ms = started.elapsed().as_millis();
        let held: Vec<u64> = take_lock_lines().iter().filter_map(|line| line["held_ms"].as_u64()).collect();
        let new = store.load(&target).unwrap().unwrap();
        let status = |team: &Team| {
            let mut value = super::super::commands::view_with(team, &[], None);
            super::super::commands::bound_status(&mut value, team);
            value
        };
        assert_eq!(super::super::lead_plan::list(&new, 300), super::super::lead_plan::list(&old, 300), "lead-changes answers the same");
        // The team is the same but for the snapshots, now ids; the status's
        // lead changes are the same (the rest of a running team's status
        // counts waits from the clock).
        let mut light = old.clone();
        for change in &mut light.lead_changes {
            if !change.tasks_before.is_empty() || !change.phases_before.is_empty() {
                change.changed = change.tasks_before.drain(..).map(|task| task.id).collect();
                change.phases_changed = change.phases_before.drain(..).map(|phase| phase.n).collect();
            }
        }
        assert_eq!(serde_json::to_value(&new).unwrap(), serde_json::to_value(&light).unwrap(), "the team reads the same");
        let (before_status, after_status) = (status(&old), status(&new));
        assert_eq!(after_status["lead_changes"], before_status["lead_changes"], "status shows the same lead changes");
        assert_eq!(after_status["lead_changes_total"], before_status["lead_changes_total"]);
        // `team status`'s own work on each copy, min of 5: a read-only open,
        // the team's load and its bounded status view.
        let timed = |path: &Path| {
            (0..5)
                .map(|_| {
                    let started = Instant::now();
                    let team = Store::open_at(path, false).unwrap().load(&target).unwrap().unwrap();
                    let _ = status(&team);
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .fold(f64::MAX, f64::min)
        };
        let status_ms = json!({"before":timed(&before),"after":timed(&after)});
        // The newest revertible change, reverted on both: the same team (the
        // before copy writes the whole record, as before task 288).
        let change = new.lead_changes.iter().rev().find(|change| change.reverted_at.is_none() && change.op != "use-last-reply").map(|change| change.id.clone());
        // The newest revertible change, reverted on both: before on the team
        // as layout 2 holds it (the snapshots in memory), after through the
        // migrated store (read on demand).
        let mut reverted = Vec::new();
        if let Some(change) = &change {
            let input = json!({"change":change,"by":"human"});
            let answer = |answer: anyhow::Result<Value>| {
                answer
                    .map(|mut value| {
                        for key in ["request_id", "team", "revision"] {
                            value.as_object_mut().map(|object| object.remove(key));
                        }
                        value
                    })
                    .map_err(|error| error.to_string())
            };
            let mut team = old.clone();
            let done = super::super::lead_plan::revert(&mut team, &input, &super::super::ops::Actor::Human, &mut Vec::new());
            reverted.push((answer(done), serde_json::to_value(&team.tasks).unwrap(), serde_json::to_value(&team.phases).unwrap()));
            let mut store = Store::open_at(&after, true).unwrap();
            let done = store.mutate(Some((&uuid::Uuid::new_v4().to_string(), &input)), &target, None, |team, events| {
                super::super::lead_plan::revert(team, &input, &super::super::ops::Actor::Human, events)
            });
            let team = store.load(&target).unwrap().unwrap();
            reverted.push((answer(done), serde_json::to_value(&team.tasks).unwrap(), serde_json::to_value(&team.phases).unwrap()));
            assert_eq!(reverted[0].0, reverted[1].0, "the revert answers the same");
            if reverted[0].0.is_ok() {
                assert_eq!(reverted[0], reverted[1], "the revert restores the same");
            }
        }
        drop(store);
        let (old_bytes, old_layout) = head_bytes(&before);
        let (new_bytes, new_layout) = head_bytes(&after);
        let rows: i64 = Connection::open(&after).unwrap().query_row("SELECT COUNT(*) FROM team_lead_changes WHERE team=?1", params![target], |row| row.get(0)).unwrap();
        println!("{}", json!({"task":4,"team":target,"lead_changes":new.lead_changes.len(),"history_rows":rows,
            "head_bytes":{"before":old_bytes,"after":new_bytes},"layout":{"before":old_layout,"after":new_layout},
            "open_ms":open_ms,"migration_held_ms":held.iter().max(),"status_min_of_5_ms":status_ms,"reverted":change,"revert":reverted.first().map(|r| r.0.clone())}));
    }

    #[test]
    fn a_sandbox_blocked_write_says_what_to_do() {
        let error = write_error("attempt to write a readonly database").to_string();
        assert!(error.contains("the sandbox blocks the team store"), "{error}");
        assert!(error.contains("hgs team report") && error.contains("scratch team"), "{error}");
        assert_eq!(write_error("disk I/O error").to_string(), "disk I/O error");
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(&dir.path().join("teams/teams.sqlite3"), true).unwrap();
        (dir, store)
    }
    fn team(id: &str) -> Team {
        Team { id: id.into(), name: "Checkout".into(), lead: "lead".into(), ..Default::default() }
    }
    const REQUEST: &str = "11111111-1111-4111-8111-111111111111";

    /// Task 25: another connection holds the database for `hold`: a writer
    /// (`BEGIN IMMEDIATE`, as the dispatcher's change) or a reader (a read
    /// transaction, as a status). Returns once it holds it.
    fn hold(path: &Path, write: bool, hold: Duration) -> std::thread::JoinHandle<()> {
        let path = path.to_path_buf();
        let (held, ready) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let db = Connection::open(&path).unwrap();
            if write {
                db.execute_batch("BEGIN IMMEDIATE; UPDATE teams SET updated=updated;").unwrap();
            } else {
                db.execute_batch("BEGIN").unwrap();
                let _: i64 = db.query_row("SELECT COUNT(*) FROM teams", [], |row| row.get(0)).unwrap();
            }
            held.send(()).unwrap();
            std::thread::sleep(hold);
            db.execute_batch("COMMIT").unwrap();
        });
        ready.recv().unwrap();
        holder
    }

    /// The lead's `hgs team reassign` as the store applies it.
    fn reassign(store: &mut Store, request: &str) -> anyhow::Result<Value> {
        let input = json!({"task":4,"member":"ui2"});
        store.mutate(Some((request, &input)), "t1", None, |team, events| {
            team.name.push('+');
            events.push(Event::new("reassigned", json!({"task":4})));
            Ok(json!({"status":"reassigned","task":4}))
        })
    }

    #[test]
    fn a_team_write_waits_for_the_dispatchers_write_instead_of_failing() {
        // Task 25, team 33fbafeb: `hgs team reassign` failed with "hgs:
        // database is locked" while the dispatcher wrote, after a 2 s wait.
        let (dir, mut store) = store();
        let path = dir.path().join("teams/teams.sqlite3");
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        let holder = hold(&path, true, Duration::from_millis(3500));
        let started = Instant::now();
        let mut lead = Store::open_at(&path, true).unwrap();
        let receipt = reassign(&mut lead, "22222222-2222-4222-8222-222222222222");
        holder.join().unwrap();
        let receipt = receipt.expect("the write waits for the other writer");
        assert!(started.elapsed() >= Duration::from_secs(3), "it waited");
        assert_eq!((receipt["status"].as_str(), receipt["revision"].as_u64()), (Some("reassigned"), Some(2)));
        // A reader holding the database at the commit is waited for too.
        let holder = hold(&path, false, Duration::from_millis(3500));
        let receipt = reassign(&mut lead, "33333333-3333-4333-8333-333333333333");
        holder.join().unwrap();
        assert_eq!(receipt.unwrap()["revision"].as_u64(), Some(3));
        assert_eq!(store.load("t1").unwrap().unwrap().name, "Checkout++");
    }

    #[test]
    fn concurrent_writes_wait_and_one_request_applies_once() {
        // Task 25: the dispatcher's write and the lead's, and the lead's
        // command run twice by hand, all while the database is held: every
        // write succeeds, the repeated request takes effect once.
        let (dir, mut store) = store();
        let path = dir.path().join("teams/teams.sqlite3");
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        let holder = hold(&path, true, Duration::from_millis(2500));
        let writers: Vec<_> = (0..4)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open_at(&path, true).unwrap();
                    if n == 0 {
                        // The dispatcher's own change: no request id.
                        store.mutate(None, "t1", None, |team, _| {
                            team.lead.push('!');
                            Ok(Value::Null)
                        })
                    } else {
                        reassign(&mut store, "44444444-4444-4444-8444-444444444444")
                    }
                })
            })
            .collect();
        holder.join().unwrap();
        let results: Vec<Value> = writers.into_iter().map(|writer| writer.join().unwrap().unwrap()).collect();
        let receipts: Vec<&Value> = results[1..].iter().collect();
        assert!(receipts.iter().all(|receipt| *receipt == receipts[0]), "{receipts:?}");
        let saved = store.load("t1").unwrap().unwrap();
        assert_eq!((saved.name.as_str(), saved.lead.as_str(), saved.revision), ("Checkout+", "lead!", 3));
        let kinds: Vec<String> = store.events("t1", None, 50).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["kind"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(kinds.iter().filter(|kind| *kind == "reassigned").count(), 1, "{kinds:?}");
    }

    #[test]
    fn a_database_still_busy_says_nothing_changed_and_how_to_retry() {
        let error = busy_error(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        ))
        .to_string();
        assert!(error.contains("nothing was changed") && error.contains("same request_id"), "{error}");
    }

    #[test]
    fn create_is_idempotent_and_a_reused_id_is_refused() {
        let (_dir, mut store) = store();
        let input = json!({"name":"Checkout"});
        let first = store.create(REQUEST, &input, team("t1"), vec![Event::new("created", json!({}))]).unwrap();
        assert_eq!(store.create(REQUEST, &input, team("t1"), vec![]).unwrap(), first);
        assert!(store.create(REQUEST, &json!({"name":"Other"}), team("t2"), vec![]).is_err());
        assert!(store.create("not-a-uuid", &input, team("t3"), vec![]).is_err());
        assert_eq!(store.load("t1").unwrap().unwrap().revision, 1);
    }

    #[test]
    fn the_active_revision_changes_with_a_team_change() {
        // #595 L2: the U43 default's cache key carries this revision, so a
        // change between two equal-length writes still refreshes the set.
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        let first = store.active_revision().unwrap();
        store
            .mutate(None, "t1", None, |team, _| {
                team.name = "Renamed".into();
                Ok(json!({}))
            })
            .unwrap();
        assert!(store.active_revision().unwrap() > first);
    }

    #[test]
    fn the_active_teams_query_is_bounded() {
        let (_dir, mut store) = store();
        for n in 0..12u32 {
            let mut t = team(&format!("t{n}"));
            t.status = if n % 4 == 0 { TeamStatus::Running } else { TeamStatus::Done };
            let request = format!("00000000-0000-4000-8000-{n:012}");
            store.create(&request, &json!({}), t, vec![]).unwrap();
        }
        // Only the three running teams, never the nine done ones, and at most
        // the requested limit (C1).
        let active = store.active_teams(1000).unwrap();
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|team| team.status == TeamStatus::Running));
        assert_eq!(store.active_teams(2).unwrap().len(), 2);
    }

    #[test]
    fn a_receipt_is_read_before_the_request_runs() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({"name":"Checkout"}), team("t1"), vec![]).unwrap();
        assert_eq!(store.receipt(REQUEST, &json!({"name":"Checkout"})).unwrap().unwrap()["status"], "created");
        assert!(store.receipt("22222222-2222-4222-8222-222222222222", &json!({})).unwrap().is_none());
        // The same id with another request is refused, as a mutation would.
        assert!(store.receipt(REQUEST, &json!({"name":"Other"})).is_err());
    }

    #[test]
    fn an_extend_replay_returns_the_first_receipt() {
        use super::super::model::{Member, TeamStatus};
        use super::super::ops::{self, Actor};
        use std::collections::BTreeSet;
        let (_dir, mut store) = store();
        let mut team = team("t1");
        team.status = TeamStatus::Running;
        team.members = vec![
            Member { id: "lead".into(), agent: "claude".into(), ..Default::default() },
            Member { id: "dev".into(), role: "Backend".into(), agent: "codex".into(), ..Default::default() },
        ];
        store.create(REQUEST, &json!({}), team, vec![]).unwrap();
        const EXTEND: &str = "22222222-2222-4222-8222-222222222222";
        let input = json!({"tasks":[{"id":1,"title":"Fix","assignee":"dev"}]});
        let extend = |store: &mut Store, input: &Value| {
            store.mutate(Some((EXTEND, input)), "t1", None, |team, events| {
                ops::apply(team, "extend", input, &Actor::Member("lead".into()), &BTreeSet::new(), events)
            })
        };
        let first = extend(&mut store, &input).unwrap();
        assert_eq!(first["status"], "added");
        let again = extend(&mut store, &input).unwrap();
        assert_eq!(again, first);
        assert_eq!(again["revision"], json!(2));
        assert_eq!(store.load("t1").unwrap().unwrap().phases.len(), 1);
        assert!(extend(&mut store, &json!({"tasks":[{"id":1,"title":"Other","assignee":"dev"}]})).is_err());
    }

    #[test]
    fn the_new_attempt_fields_survive_the_store_and_a_replay() {
        use super::super::model::{
            Attempt, AttemptState, Hold, Member, ReplacedReport, Report, Settings, Task, TaskStatus, TeamStatus,
        };
        use super::super::ops::{self, Actor};
        use std::collections::BTreeSet;
        let (_dir, mut store) = store();
        let mut stored = team("t1");
        stored.status = TeamStatus::Running;
        stored.settings = Settings { start_timeout_seconds: 300, ..Default::default() };
        stored.members = vec![
            Member { id: "lead".into(), agent: "claude".into(), ..Default::default() },
            Member { id: "dev".into(), role: "Backend".into(), agent: "codex".into(), ..Default::default() },
        ];
        stored.tasks = vec![
            Task {
                id: 1,
                title: "API".into(),
                assignee: "dev".into(),
                status: TaskStatus::InProgress,
                attempts: vec![
                    Attempt {
                        n: 1,
                        id: "00000000-0000-4000-8000-000000000001".into(),
                        member: "dev".into(),
                        state: AttemptState::Reported,
                        sent_at: 1.0,
                        delivered_at: Some(2.0),
                        started_at: Some(3.0),
                        start_warned: true,
                        turn_ended_at: Some(4.0),
                        turn_reply: "reply-7".into(),
                        report: Some(Report {
                            text: "Done".into(),
                            at: 6.0,
                            captured: true,
                            replaced: vec![ReplacedReport { text: "Building".into(), at: 5.5 }],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Attempt {
                        n: 2,
                        id: "00000000-0000-4000-8000-000000000002".into(),
                        member: "dev".into(),
                        state: AttemptState::Delivered,
                        sent_at: 8.0,
                        delivered_at: Some(9.0),
                        started_at: Some(10.0),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Task {
                id: 2,
                title: "Tests".into(),
                assignee: "dev".into(),
                status: TaskStatus::Pending,
                hold: Some(Hold { by: "Lead".into(), at: 7.0, reason: "Busy with #1".into() }),
                ..Default::default()
            },
        ];
        store.create(REQUEST, &json!({}), stored.clone(), vec![]).unwrap();
        let loaded = store.load("t1").unwrap().unwrap();
        assert_eq!(loaded.tasks, stored.tasks);
        assert_eq!(loaded.settings.start_timeout_seconds, 300);
        assert_eq!(loaded.tasks[1].hold.as_ref().map(|hold| hold.reason.as_str()), Some("Busy with #1"));

        // A member's question, replayed with the same request: one receipt.
        const ASK: &str = "33333333-3333-4333-8333-333333333333";
        let input = json!({"text":"Must I keep the old JSON field?"});
        let ask = |store: &mut Store, input: &Value| {
            store.mutate(Some((ASK, input)), "t1", None, |team, events| {
                ops::apply(team, "ask", input, &Actor::Member("dev".into()), &BTreeSet::new(), events)
            })
        };
        let first = ask(&mut store, &input).unwrap();
        assert_eq!((first["status"].as_str(), first["question"].as_str()), (Some("asked"), Some("q1")));
        assert_eq!((first["task"].as_u64(), first["attempt"].as_u64()), (Some(1), Some(2)));
        assert_eq!(ask(&mut store, &input).unwrap(), first);
        let loaded = store.load("t1").unwrap().unwrap();
        let questions = &loaded.tasks[0].attempts[1].questions;
        assert_eq!(questions.len(), 1);
        assert_eq!(
            (questions[0].id.as_str(), questions[0].text.as_str(), questions[0].answered_at),
            ("q1", "Must I keep the old JSON field?", None)
        );
        // The feed has the question once, not once per replay.
        let feed = store.events("t1", None, PAGE).unwrap();
        let asked: Vec<&Value> =
            feed["items"].as_array().unwrap().iter().filter(|item| item["kind"] == "question_asked").collect();
        assert_eq!(asked.len(), 1);
        assert!(asked[0]["seq"].as_i64().is_some());
        assert_eq!(asked[0]["detail"]["question"], json!("q1"));
        assert_eq!(loaded.revision, 2);
    }

    #[test]
    fn a_mutation_checks_the_revision_and_appends_its_events() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        let changed = store
            .mutate(None, "t1", Some(1), |team, events| {
                team.goal = "Ship".into();
                events.push(Event::new("planned", json!({"tasks":0})));
                Ok(json!({"status":"planned"}))
            })
            .unwrap();
        assert_eq!(changed["revision"], 2);
        assert!(store.mutate(None, "t1", Some(1), |_, _| Ok(json!({}))).is_err());
        // A failed change leaves the team untouched.
        assert!(store.mutate(None, "t1", None, |team, _| { team.goal = "Lost".into(); bail!("refused") }).is_err());
        assert_eq!(store.load("t1").unwrap().unwrap().goal, "Ship");
    }

    #[test]
    fn feed_pages_stay_stable_while_entries_arrive() {
        let (_dir, mut store) = store();
        store.create(REQUEST, &json!({}), team("t1"), vec![]).unwrap();
        for n in 0..60 {
            store.mutate(None, "t1", None, |_, events| { events.push(Event::new("tick", json!({"n":n}))); Ok(json!({})) }).unwrap();
        }
        let first = store.events("t1", None, PAGE).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 50);
        assert_eq!(first["items"][0]["detail"]["n"], 59);
        let cursor = first["next_cursor"].as_i64().unwrap();
        store.mutate(None, "t1", None, |_, events| { events.push(Event::new("tick", json!({"n":60}))); Ok(json!({})) }).unwrap();
        let second = store.events("t1", Some(cursor), PAGE).unwrap();
        assert_eq!(second["items"].as_array().unwrap().len(), 10);
        assert_eq!(second["items"][0]["detail"]["n"], 9);
        assert!(second["next_cursor"].is_null());
    }
}

#[cfg(test)]
#[path = "tests/store_archive.rs"]
mod archive_tests;
