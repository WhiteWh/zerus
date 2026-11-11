//! The durable request journal, `state_dir/journal.sqlite3`, opened in place
//! exactly as the Python `Journal` (`connector.py:104-249`) opens it: the same
//! file checks and lock, DDL text, legacy upgrade, restart recovery, pragmas,
//! retention and caps. While a rollback to Python is supported the schema is
//! table-compatible: `user_version` 0 and a rollback journal (never WAL).
//! Additive read indexes are mirrored by the Python connector.
//!
//! Each Python `with self.db:` block is one explicit DEFERRED transaction
//! here, and the clock is a parameter so tests can fix it.
use crate::error::{
    ConnectorError, CONFLICT, FOREIGN_BINDING, JOURNAL_UNAVAILABLE, RETAINED_LIMIT,
    STATE_UNAVAILABLE, UNSAFE_FILE,
};
use crate::fsguard;
use crate::request::{receipt, receipt_text};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::path::Path;
use std::time::Duration;
use zerus_relay::protocol::{OPERATIONS, READS};

#[path = "encrypted_journal.rs"]
mod encrypted;

pub const JOURNAL: &str = "journal.sqlite3";
pub const LOCK: &str = "connector.lock";
pub const SIDECARS: [&str; 3] = ["-journal", "-wal", "-shm"];

pub const METADATA_DDL: &str =
    "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)";
// The whitespace is part of the stored schema text, so it stays Python's.
pub const REQUESTS_DDL: &str = "CREATE TABLE IF NOT EXISTS requests (
            request_id TEXT PRIMARY KEY, operation TEXT NOT NULL,
            state TEXT NOT NULL, response TEXT, delivered INTEGER NOT NULL DEFAULT 0,
            request_hash TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL,
            delivered_at REAL, response_expired INTEGER NOT NULL DEFAULT 0
        )";
/// The columns a legacy five-column journal gains, in Python's order.
pub const ADDED_COLUMNS: [(&str, &str); 5] = [
    ("request_hash", "TEXT"),
    ("created_at", "REAL"),
    ("updated_at", "REAL"),
    ("delivered_at", "REAL"),
    ("response_expired", "INTEGER NOT NULL DEFAULT 0"),
];
pub const INDEXES: [(&str, &str); 10] = [
    ("delivered_history", "CREATE INDEX IF NOT EXISTS delivered_history ON requests(delivered,delivered_at)"),
    ("project_assignment_delivered_v1", "CREATE INDEX IF NOT EXISTS project_assignment_delivered_v1 ON requests(delivered_at) WHERE operation='project_assignment' AND delivered=1 AND state!='running'"),
    ("analytics_delivered_reads", "CREATE INDEX IF NOT EXISTS analytics_delivered_reads ON requests(delivered_at) WHERE operation='analytics_read' AND delivered=1 AND state!='running'"),
    ("terminal_delivered_reads", "CREATE INDEX IF NOT EXISTS terminal_delivered_reads ON requests(delivered_at) WHERE operation='terminal_snapshot' AND delivered=1 AND state!='running'"),
    ("archive_delivered_reads", "CREATE INDEX IF NOT EXISTS archive_delivered_reads ON requests(delivered_at) WHERE operation='archive_list' AND delivered=1 AND state!='running'"),
    ("file_delivered_reads", "CREATE INDEX IF NOT EXISTS file_delivered_reads ON requests(delivered_at) WHERE operation='file_read' AND delivered=1 AND state!='running'"),
    ("attachment_delivered_reads", "CREATE INDEX IF NOT EXISTS attachment_delivered_reads ON requests(delivered_at) WHERE operation='attachment_read' AND delivered=1 AND state!='running'"),
    ("journal_operation", "CREATE INDEX IF NOT EXISTS journal_operation ON requests(operation)"),
    ("delivered_reads", "CREATE INDEX IF NOT EXISTS delivered_reads ON requests(delivered_at) WHERE operation='inspect' AND delivered=1 AND state!='running'"),
    ("mutation_retention", "CREATE INDEX IF NOT EXISTS mutation_retention ON requests(delivered_at) WHERE operation!='inspect' AND delivered=1 AND state!='running' AND response IS NOT NULL AND response_expired=0"),
];

// The Python connector's frozen predicates (operations.json `sql`).
pub const READ_CLASS_SQL: &str = "('analytics_read','archive_list','attachment_read','catalog','dirs','file_read','history','history_projection','hooks_status','inspect','mark_read','process_output','project_assignment','speech_config','speech_config_status','speech_profiles','speech_stats','speech_status','terminal_snapshot','voice_profiles','voice_status','worktrees')";
pub const READ_SQL: &str = "('inspect','process_output','terminal_snapshot','catalog','dirs','worktrees','history','mark_read')";
pub const SPEECH_READ_SQL: &str = "('speech_profiles','speech_stats','speech_status')";
pub const SPEECH_CONFIG_READ_SQL: &str = "('speech_config','speech_config_status')";
pub const VOICE_READ_SQL: &str = "('voice_profiles','voice_status')";
/// Delivered exchanges older than the short-read window, found by their
/// operation: the age index would walk every delivered mutation tombstone.
pub const TASK_READ_PRUNE: &str = "DELETE FROM requests INDEXED BY journal_operation WHERE operation IN ('task_exchange','task_done') AND delivered=1 AND state!='running' AND delivered_at<?";

/// Additive Rust-only reads stay outside the frozen Python class predicate:
/// Git status, repository checks, and the phone's task exchange and Done list.
pub const GIT_STATUS_COUNT_SQL: &str =
    "SELECT count(*) FROM requests WHERE operation IN ('git_status','repository_check','repository_status','task_exchange','task_done','send_receipt')";
pub const GIT_STATUS_PRUNE_SQL: &str = "DELETE FROM requests WHERE request_id IN (SELECT request_id FROM requests INDEXED BY journal_operation WHERE operation IN ('git_status','repository_check','repository_status','send_receipt') AND delivered=1 AND state IN ('completed','failed','uncertain') AND delivered_at<? ORDER BY delivered_at,request_id LIMIT 5000)";

/// Delivered short-lived reads go after two minutes, other reads after an
/// hour; a delivered mutation becomes a tombstone after seven days.
pub const SHORT_READ_SECONDS: f64 = 120.0;
pub const READ_SECONDS: f64 = 3600.0;
pub const MUTATION_SECONDS: f64 = 7.0 * 86400.0;
pub const READ_CAP: i64 = 5000;
pub const MUTATION_CAP: i64 = 100_000;
pub const PENDING_BATCH: usize = 100;

pub const RESTART_ERROR: &str = "connector stopped before recording the native result";
pub const EXPIRED_ERROR: &str =
    "Native result history expired; this request will not execute again";
pub const IN_PROGRESS_ERROR: &str = "request is already in progress";

/// Why the journal could not be opened or used.
#[derive(Debug)]
pub enum JournalError {
    /// A refusal with Python's text (ConnectorError there).
    Refused(ConnectorError),
    /// The state directory could not be created (OSError there).
    Io(std::io::Error),
    /// SQLite failed (sqlite3.Error there, which stops the Python connector).
    Storage(rusqlite::Error),
}

impl JournalError {
    /// A sanitized text: no path or driver diagnostic.
    pub fn message(&self) -> &str {
        match self {
            JournalError::Refused(error) => error.message(),
            JournalError::Io(_) => STATE_UNAVAILABLE,
            JournalError::Storage(_) => JOURNAL_UNAVAILABLE,
        }
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for JournalError {}

impl From<ConnectorError> for JournalError {
    fn from(error: ConnectorError) -> Self {
        JournalError::Refused(error)
    }
}

impl From<rusqlite::Error> for JournalError {
    fn from(error: rusqlite::Error) -> Self {
        JournalError::Storage(error)
    }
}

pub type Result<T> = std::result::Result<T, JournalError>;

fn refused(text: &str) -> JournalError {
    JournalError::Refused(ConnectorError::new(text))
}

/// `operation in MUTATIONS`: a known operation that is not a read. Unknown
/// operations recover as failed reads do, as in Python. The relay's set
/// includes the operations only this connector executes (`remove_queued`),
/// so an in-flight one recovers as uncertain too.
pub fn is_mutation(operation: &str) -> bool {
    OPERATIONS.contains(&operation) && !READS.contains(&operation)
}

pub struct Journal {
    // Declared first, so the connection closes before the lock is released.
    db: Connection,
    _lock: File,
    #[cfg(windows)]
    _directory: zerus_windows_private::DirectoryGuard,
}

impl Journal {
    /// Opens `directory/journal.sqlite3` for `binding` at clock `now`,
    /// creating the private directory and files when missing.
    pub fn open(directory: &Path, binding: &str, now: f64) -> Result<Self> {
        fsguard::create_directory(directory).map_err(JournalError::Io)?;
        fsguard::check_private_directory(directory)?;
        #[cfg(windows)]
        let directory_guard =
            zerus_windows_private::directory(directory, false).map_err(JournalError::Io)?;
        let lock = fsguard::private_file(&directory.join(LOCK), true)?
            .ok_or_else(|| refused(UNSAFE_FILE))?;
        fsguard::lock_exclusive(&lock)?;
        let path = directory.join(JOURNAL);
        drop(fsguard::private_file(&path, true)?);
        for suffix in SIDECARS {
            // A sidecar that vanished meanwhile is simply absent.
            drop(fsguard::private_file(
                &directory.join(format!("{JOURNAL}{suffix}")),
                false,
            )?);
        }
        // No SQLITE_OPEN_NOFOLLOW: it refuses a symlink anywhere in the path,
        // such as a linked ~/.local, which Python accepts. The file itself was
        // just opened without following a symlink, as Python checks it.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let db = Connection::open_with_flags(&path, flags)?;
        // Python's sqlite3 waits 5 s by default; the bundled build enables
        // foreign keys, which Python's connection does not.
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA foreign_keys=OFF")?;
        db.execute_batch(METADATA_DDL)?;
        {
            let tx = db.unchecked_transaction()?;
            let stored: Option<String> = tx
                .query_row(
                    "SELECT value FROM metadata WHERE key='relay_binding'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if stored.is_some_and(|stored| stored != binding) {
                return Err(refused(FOREIGN_BINDING));
            }
            tx.execute(
                "INSERT OR IGNORE INTO metadata VALUES('relay_binding',?)",
                [binding],
            )?;
            tx.commit()?;
        }
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA secure_delete=ON")?;
        db.execute_batch(REQUESTS_DDL)?;
        let columns = columns(&db)?;
        {
            let tx = db.unchecked_transaction()?;
            // Legacy records have no reliable age or request fingerprint. Give
            // them the current time rather than pruning old unknown deliveries.
            for (name, kind) in ADDED_COLUMNS {
                if !columns.contains(name) {
                    tx.execute_batch(&format!("ALTER TABLE requests ADD COLUMN {name} {kind}"))?;
                }
            }
            tx.execute(
                "UPDATE requests SET created_at=? WHERE created_at IS NULL",
                [now],
            )?;
            tx.execute(
                "UPDATE requests SET updated_at=? WHERE updated_at IS NULL",
                [now],
            )?;
            tx.execute(
                "UPDATE requests SET delivered_at=? WHERE delivered=1 AND delivered_at IS NULL",
                [now],
            )?;
            let running: Vec<(String, String)> = tx
                .prepare("SELECT request_id, operation FROM requests WHERE state='running'")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (request_id, operation) in running {
                let state = if is_mutation(&operation) {
                    "uncertain"
                } else {
                    "failed"
                };
                let text = stored_text(&receipt(state, Value::Null, Some(RESTART_ERROR)))?;
                tx.execute(
                    "UPDATE requests SET state=?,response=?,updated_at=? WHERE request_id=?",
                    params![state, text, now, request_id],
                )?;
            }
            tx.commit()?;
        }
        for (_, ddl) in INDEXES {
            db.execute_batch(ddl)?;
        }
        let journal = Journal {
            db,
            _lock: lock,
            #[cfg(windows)]
            _directory: directory_guard,
        };
        journal.prune(now)?;
        Ok(journal)
    }

    /// Retention (`connector.py:188-204`); undelivered rows never expire.
    pub fn prune(&self, now: f64) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let delivered = "delivered=1 AND state!='running' AND delivered_at<?";
        let short = now - SHORT_READ_SECONDS;
        tx.execute(
            &format!("DELETE FROM requests WHERE operation='terminal_snapshot' AND {delivered}"),
            [short],
        )?;
        for set in [
            "('analytics_read')",
            SPEECH_CONFIG_READ_SQL,
            SPEECH_READ_SQL,
            VOICE_READ_SQL,
            "('hooks_status')",
            "('project_assignment')",
        ] {
            tx.execute(
                &format!("DELETE FROM requests WHERE operation IN {set} AND {delivered}"),
                [short],
            )?;
        }
        // Delivered attachment chunks are never kept beyond the same short window.
        tx.execute(
            &format!("DELETE FROM requests WHERE operation='file_read' AND {delivered}"),
            [short],
        )?;
        tx.execute(
            &format!("DELETE FROM requests WHERE operation='attachment_read' AND {delivered}"),
            [short],
        )?;
        tx.execute(TASK_READ_PRUNE, [short])?;
        tx.execute(
            &format!("DELETE FROM requests WHERE operation='archive_list' AND {delivered}"),
            [now - 3600.0],
        )?;
        tx.execute(&format!("DELETE FROM requests WHERE operation IN {READ_SQL} AND operation!='terminal_snapshot' AND {delivered}"),
                   [now - READ_SECONDS])?;
        // Projection reads share the read cap and expire through the operation index.
        tx.execute("DELETE FROM requests WHERE request_id IN (SELECT request_id FROM requests INDEXED BY journal_operation WHERE operation='history_projection' AND delivered=1 AND state IN ('completed','failed','uncertain') AND delivered_at<? ORDER BY delivered_at,request_id LIMIT 5000)", [now - READ_SECONDS])?;
        // This class shares the read cap, but must expire before the unchanged
        // legacy NOT-IN mutation tombstone selector can see it.
        tx.execute(GIT_STATUS_PRUNE_SQL, [now - READ_SECONDS])?;
        let expired: Vec<(String, String)> = tx
            .prepare(&format!("SELECT request_id,state FROM requests WHERE operation NOT IN {READ_CLASS_SQL} AND {delivered} AND response IS NOT NULL AND response_expired=0"))?
            .query_map([now - MUTATION_SECONDS], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (request_id, state) in expired {
            // Keep a bounded explicit receipt, never the private native result.
            let text = stored_text(&receipt(
                &state,
                json!({"status": "history_expired"}),
                Some(EXPIRED_ERROR),
            ))?;
            tx.execute(
                "UPDATE requests SET response=?,response_expired=1 WHERE request_id=?",
                params![text, request_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records a new request as running. `Ok(false)` means the identity is
    /// already journaled with the same operation and fingerprint, so its
    /// stored outcome is replayed instead of executing again.
    pub fn claim(
        &self,
        request_id: &str,
        operation: &str,
        request_hash: Option<&str>,
        now: f64,
    ) -> Result<bool> {
        self.prune(now)?;
        let tx = self.db.unchecked_transaction()?;
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT operation,request_hash FROM requests WHERE request_id=?",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((stored, hash)) = existing {
            // A legacy row has no fingerprint, so no fingerprinted claim matches it.
            if stored != operation
                || request_hash.is_some_and(|wanted| hash.as_deref() != Some(wanted))
            {
                return Err(refused(CONFLICT));
            }
            return Ok(false);
        }
        let reads = READS.contains(&operation);
        let count: i64 = tx.query_row(
            &format!("SELECT count(*) FROM requests WHERE (operation IN {READ_CLASS_SQL})=?"),
            [reads as i64],
            |row| row.get(0),
        )?;
        // The legacy NOT-IN count includes new Rust-only reads. Correct only
        // this additive class, in the same transaction as admission.
        let git_reads: i64 = tx.query_row(GIT_STATUS_COUNT_SQL, [], |row| row.get(0))?;
        let count = if reads {
            count + git_reads
        } else {
            count - git_reads
        };
        if count >= if reads { READ_CAP } else { MUTATION_CAP } {
            return Err(refused(RETAINED_LIMIT));
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO requests(request_id,operation,state,request_hash,created_at,updated_at) VALUES(?,?,'running',?,?,?)",
            params![request_id, operation, request_hash, now, now])?;
        tx.commit()?;
        Ok(inserted == 1)
    }

    /// Stores a receipt (`{"state", "result", "error"}`) for upload.
    pub fn finish(&self, request_id: &str, response: &Value, now: f64) -> Result<()> {
        let state = response["state"]
            .as_str()
            .ok_or_else(|| refused(JOURNAL_UNAVAILABLE))?;
        let text = stored_text(response)?;
        self.db.execute(
            "UPDATE requests SET state=?,response=?,delivered=0,delivered_at=NULL,response_expired=0,updated_at=? WHERE request_id=?",
            params![state, text, now, request_id])?;
        Ok(())
    }

    /// The stored receipt text, queued for upload again, or an in-progress
    /// receipt while the request still runs.
    pub fn replay(&self, request_id: &str) -> Result<String> {
        let stored: Option<Option<String>> = self
            .db
            .query_row(
                "SELECT response FROM requests WHERE request_id=?",
                [request_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(Some(text)) = stored else {
            return stored_text(&receipt("uncertain", Value::Null, Some(IN_PROGRESS_ERROR)));
        };
        self.db.execute(
            "UPDATE requests SET delivered=0,delivered_at=NULL WHERE request_id=?",
            [request_id],
        )?;
        Ok(text)
    }

    /// Up to 100 undelivered receipts as stored text, in no defined order.
    pub fn pending(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT request_id,response FROM requests WHERE delivered=0 AND response IS NOT NULL LIMIT {PENDING_BATCH}"))?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// [`Self::pending`] with each row's operation, which decides the state
    /// of a substitute upload (divergence g).
    pub fn pending_rows(&self) -> Result<Vec<(String, String, String)>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT request_id,operation,response FROM requests WHERE delivered=0 AND response IS NOT NULL LIMIT {PENDING_BATCH}"))?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delivered(&self, request_id: &str, now: f64) -> Result<()> {
        self.db.execute(
            "UPDATE requests SET delivered=1,delivered_at=?,updated_at=? WHERE request_id=?",
            params![now, now, request_id],
        )?;
        Ok(())
    }

    /// One integer pragma of this connection, for schema checks.
    pub fn connection_pragma(&self, name: &str) -> Result<i64> {
        Ok(self
            .db
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))?)
    }

    pub fn close(self) {
        drop(self);
    }
}

fn stored_text(receipt: &Value) -> Result<String> {
    receipt_text(receipt).map_err(|_| refused(JOURNAL_UNAVAILABLE))
}

pub fn columns(db: &Connection) -> rusqlite::Result<HashSet<String>> {
    db.prepare("PRAGMA table_info(requests)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}
