//! Rebuildable, body-free receipt locators. Original JSON alone proves delivery.
//! Reads never enumerate source directories. Legacy discovery runs separately in
//! bounded background batches and never deletes or repairs original receipts.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

mod migration;
mod point;
mod question;
pub(super) use point::{delivery_binding, delivery_receipt, DeliveryQuery};
// The independently developed recovery consumer stacks on this interface.
#[allow(unused_imports)]
pub(crate) use question::{question_page, Coverage, QuestionReceiptPage, QuestionReceiptRow};
#[cfg(test)]
mod tests;

pub(super) const PAGE_LIMIT: usize = 100;
const EXAMINE: usize = 256;
const ORIGINAL_BYTES: u64 = 256 * 1024;
const DATABASE: &str = "delivery-receipts-v1.sqlite3";
const ROWSET_PROTOCOL: &str = "2";
const VERIFY_SECONDS: f64 = 600.0;

// Coverage is sealed only after discovery. These guards invalidate that seal
// if a cached locator disappears or moves without a matching original walk.
// They never change an original receipt or establish delivery authority.
fn rowset_protocol(db: &Connection) -> bool {
    db.query_row(
        "SELECT (SELECT value FROM receipt_meta WHERE key='rowset_protocol')=?1 AND
         (SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND name IN
          ('receipt_row_removed','receipt_row_rebound'))=2",
        [ROWSET_PROTOCOL],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Source {
    Terminal,
    Dsh,
    OpenCode,
    Question,
}
impl Source {
    const ALL: [Self; 4] = [Self::Question, Self::Terminal, Self::Dsh, Self::OpenCode];
    pub(super) fn of(record: &Value) -> Self {
        if record["agent"] == "opencode" || record["backend"] == "opencode" {
            Self::OpenCode
        } else if record["agent"] == "dsh" || record["backend"] == "dsh" {
            Self::Dsh
        } else {
            Self::Terminal
        }
    }
    fn directory(self) -> &'static str {
        match self {
            Self::Terminal => "input_receipts",
            Self::Dsh => "dsh/receipts",
            Self::OpenCode => "opencode/receipts",
            Self::Question => "question_receipts",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Dsh => "dsh",
            Self::OpenCode => "opencode",
            Self::Question => "question",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Scope {
    source: Source,
    name: Option<String>,
    run: String,
    conversation: String,
    agent: String,
}
impl Scope {
    pub(super) fn new(record: &Value, agent: &str) -> Self {
        Self {
            source: Source::of(record),
            name: None,
            run: string(record, "run_id").into(),
            conversation: string(record, "conversation_id").into(),
            agent: agent.into(),
        }
    }
    fn hash(&self, files: bool) -> String {
        hash(
            &json!([
                self.source,
                self.name,
                self.run,
                self.conversation,
                self.agent,
                files
            ])
            .to_string(),
        )
    }
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    // SQLite messages may contain filesystem paths or values. Numeric codes
    // identify failures without exposing those details to callers.
    r.map_err(|error| match error {
        rusqlite::Error::SqliteFailure(code, _) => format!(
            "Receipt index is unavailable (SQLite {:?}, extended {})",
            code.code, code.extended_code
        ),
        _ => "Receipt index is unavailable (SQLite result conversion)".into(),
    })
}
fn bounded_identity(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}

/// An adapter's immutable original binding, not a delivery acknowledgement.
/// Other receipt families can reuse Catalog/pages with their own binding hash.
struct Binding {
    id: String,
    name: String,
    run: String,
    conversation: String,
    agent: String,
    first: bool,
    at: f64,
    files: bool,
    fingerprint: String,
}
impl Binding {
    fn parse(source: Source, value: &Value, filename: &str) -> Result<Self> {
        let id = string(value, "request_id");
        let name = string(value, "name");
        let run = string(value, "run_id");
        let original = string(value, "conversation_id");
        let resolved = string(value, "resolved_conversation_id");
        let agent = string(value, "agent_id");
        if id.is_empty()
            || id.len() > 128
            || filename != format!("{id}.json")
            || id.contains(['/', '\\'])
            || !bounded_identity(id, 128)
            || !bounded_identity(name, 512)
            || (source == Source::Question && (name.is_empty() || run.is_empty()))
            || !bounded_identity(run, 128)
            || !bounded_identity(original, 512)
            || !bounded_identity(resolved, 512)
            || !bounded_identity(agent, 512)
            || (run.is_empty() && original.is_empty())
            || (!original.is_empty() && !resolved.is_empty() && original != resolved)
        {
            return Err("Invalid receipt binding".into());
        }
        let at = value
            .get("submitted_at")
            .or_else(|| value.get("at"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0);
        Ok(Self {
            id: id.into(),
            name: name.into(),
            run: run.into(),
            conversation: if resolved.is_empty() {
                original
            } else {
                resolved
            }
            .into(),
            agent: agent.into(),
            first: value["first_message"] == true,
            at,
            files: value["attachments"]
                .as_array()
                .is_some_and(|v| !v.is_empty()),
            fingerprint: if source == Source::Question {
                hash(
                    &json!([
                        value["request_id"],
                        value["name"],
                        value["run_id"],
                        value["conversation_id"],
                        value["question_id"],
                        value["question_hash"],
                        value["digest"]
                    ])
                    .to_string(),
                )
            } else {
                hash(
                    &json!([
                        value["request_id"],
                        value["name"],
                        value["run_id"],
                        value["conversation_id"],
                        value["agent_id"],
                        value["digest"],
                        value["first_message"],
                        value["graph_target"]
                    ])
                    .to_string(),
                )
            },
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct Locator {
    source: Source,
    pub(super) id: String,
    sequence: i64,
    at: f64,
    fingerprint: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    epoch: String,
    scope: String,
    snapshot: i64,
    complete: bool,
    at: f64,
    id: String,
}

fn cursor_mac(secret: &str, bytes: &[u8]) -> String {
    let mut key = [0u8; 64];
    key[..secret.len()].copy_from_slice(secret.as_bytes());
    let mut inner = Sha256::new();
    inner.update(key.map(|b| b ^ 0x36));
    inner.update(bytes);
    let mut outer = Sha256::new();
    outer.update(key.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    format!("{:x}", outer.finalize())
}
fn encode_cursor(secret: &str, cursor: &Cursor) -> std::result::Result<String, PageFailure> {
    let bytes = serde_json::to_vec(cursor).map_err(|_| PageFailure::Cursor)?;
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&bytes),
        cursor_mac(secret, &bytes)
    ))
}
fn decode_cursor(secret: &str, token: &str) -> std::result::Result<Cursor, PageFailure> {
    let (body, signature) = token.split_once('.').ok_or(PageFailure::Cursor)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| PageFailure::Cursor)?;
    let expected = cursor_mac(secret, &bytes);
    if signature.len() != expected.len()
        || !signature
            .bytes()
            .zip(expected.bytes())
            .fold(true, |same, (a, b)| same & (a == b))
    {
        return Err(PageFailure::Cursor);
    }
    serde_json::from_slice(&bytes).map_err(|_| PageFailure::Cursor)
}

enum PageFailure {
    InvalidQuery,
    Cursor,
    Unavailable,
    Deadline,
}
impl From<String> for PageFailure {
    fn from(_: String) -> Self {
        Self::Unavailable
    }
}
impl PageFailure {
    fn message(&self) -> &'static str {
        match self {
            Self::InvalidQuery => "Invalid receipt page",
            Self::Cursor => "Receipt cursor belongs to another target or index",
            Self::Unavailable => "Receipt index is unavailable",
            Self::Deadline => "Receipt page deadline elapsed",
        }
    }
}

pub(super) struct Page {
    pub(super) locators: Vec<Locator>,
    pub(super) metadata: Value,
}

pub(super) struct Catalog {
    db: Connection,
    state: PathBuf,
}
impl Catalog {
    fn open(state: &Path, create: bool) -> Result<Self> {
        if create {
            private_dir(state)?;
        }
        // SQLite NOFOLLOW rejects ancestor aliases too (macOS /var and /tmp).
        // Resolve only ancestors: the state directory and database leaf remain
        // in the no-follow path, including if either is replaced concurrently.
        let state = match state.file_name() {
            Some(name) => {
                let parent = state
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                crate::platform::canonical(parent)
                    .map_err(|_| "Receipt index is unavailable")?
                    .join(name)
            }
            None => {
                crate::platform::canonical(state).map_err(|_| "Receipt index is unavailable")?
            }
        };
        if !fs::symlink_metadata(&state).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
            return Err("Receipt index is unavailable".into());
        }
        let path = state.join(DATABASE);
        if create {
            match crate::platform::private_open(OpenOptions::new().write(true).create_new(true))
                .open(&path)
            {
                Ok(file) => {
                    file.sync_all()
                        .map_err(|_| "Receipt index is unavailable")?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("Receipt index is unavailable".into()),
            }
        }
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
            return Err("Receipt index is unavailable".into());
        }
        let flags = if create {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let db = sql(Connection::open_with_flags(&path, flags))?;
        sql(db.busy_timeout(if create {
            Duration::from_millis(20)
        } else {
            Duration::ZERO
        }))?;
        let verified = rowset_protocol(&db);
        if !create && !verified {
            return Err("Receipt index coverage is unverified".into());
        }
        if create {
            sql(db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;"))?;
        }
        // Publish repaired guards and invalidated legacy coverage together.
        let mut upgrade = if create && !verified {
            Some(sql(db.unchecked_transaction())?)
        } else {
            None
        };
        if create {
            sql(db.execute_batch("CREATE TABLE IF NOT EXISTS receipt_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS receipt_sources(source TEXT PRIMARY KEY,stamp TEXT NOT NULL DEFAULT '',complete INTEGER NOT NULL DEFAULT 0,indexed INTEGER NOT NULL DEFAULT 0,errors INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS receipt_rows(seq INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,request TEXT NOT NULL,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,agent TEXT NOT NULL,first_message INTEGER NOT NULL,at REAL NOT NULL,files INTEGER NOT NULL,binding TEXT NOT NULL,stamp TEXT NOT NULL DEFAULT '',change INTEGER NOT NULL DEFAULT 0,UNIQUE(source,request));
                CREATE INDEX IF NOT EXISTS receipt_exact ON receipt_rows(source,name,run,conversation,at DESC,request DESC);
                CREATE INDEX IF NOT EXISTS receipt_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC);
                CREATE INDEX IF NOT EXISTS receipt_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC);
                CREATE INDEX IF NOT EXISTS receipt_file_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC) WHERE files=1;
                CREATE INDEX IF NOT EXISTS receipt_file_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC) WHERE files=1;
                CREATE TABLE IF NOT EXISTS receipt_errors(source TEXT NOT NULL,name TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(source,name));
                CREATE TRIGGER IF NOT EXISTS receipt_row_count AFTER INSERT ON receipt_rows BEGIN UPDATE receipt_sources SET indexed=indexed+1 WHERE source=NEW.source; END;
                CREATE TRIGGER IF NOT EXISTS receipt_error_count AFTER INSERT ON receipt_errors BEGIN UPDATE receipt_sources SET errors=errors+1 WHERE source=NEW.source; END;

                CREATE TRIGGER IF NOT EXISTS receipt_error_clear AFTER DELETE ON receipt_errors BEGIN UPDATE receipt_sources SET errors=MAX(0,errors-1) WHERE source=OLD.source; END;
                CREATE TRIGGER IF NOT EXISTS receipt_row_removed AFTER DELETE ON receipt_rows BEGIN
                  UPDATE receipt_sources SET complete=0,indexed=MAX(0,indexed-1) WHERE source=OLD.source;
                  INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json','index_row_missing')
                    ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason;
                  UPDATE receipt_meta SET value=lower(hex(randomblob(16))) WHERE key='epoch';
                END;
                CREATE TRIGGER IF NOT EXISTS receipt_row_rebound AFTER UPDATE ON receipt_rows
                WHEN (OLD.seq IS NOT NEW.seq OR OLD.source IS NOT NEW.source OR OLD.request IS NOT NEW.request
                  OR OLD.name IS NOT NEW.name OR OLD.run IS NOT NEW.run OR OLD.conversation IS NOT NEW.conversation
                  OR OLD.agent IS NOT NEW.agent OR OLD.first_message IS NOT NEW.first_message
                  OR OLD.at IS NOT NEW.at OR OLD.files IS NOT NEW.files OR OLD.binding IS NOT NEW.binding)
                  -- The one-way first-prompt pin is not a rebind: an empty
                  -- conversation resolving to its run's conversation keeps
                  -- every other identity column and the row's coverage.
                  AND NOT (OLD.conversation='' AND NEW.conversation<>''
                    AND OLD.seq IS NEW.seq AND OLD.source IS NEW.source AND OLD.request IS NEW.request
                    AND OLD.name IS NEW.name AND OLD.run IS NEW.run AND OLD.agent IS NEW.agent
                    AND OLD.first_message IS NEW.first_message AND OLD.at IS NEW.at
                    AND OLD.files IS NEW.files AND OLD.binding IS NEW.binding)
                BEGIN
                  UPDATE receipt_sources SET complete=0 WHERE source IN(OLD.source,NEW.source);
                  INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json','index_row_changed')
                    ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason;
                  UPDATE receipt_meta SET value=lower(hex(randomblob(16))) WHERE key='epoch';
                END;"))?;

            // #2055 shipped without change tracking. A v1 database has neither
            // column: add them (and seed their sequences) before any index
            // mentions them. A cache that already has the row-set guards can
            // still predate the change columns, and a persisted cut between
            // ADD COLUMN and its seed leaves rows at change=0 without a
            // published frontier. Repair both in one transaction, so a reader
            // never sees a Complete frontier that does not cover its rows.
            let columns: Vec<String> =
                sql(db.prepare("SELECT name FROM pragma_table_info('receipt_rows')"))?
                    .query_map([], |row| row.get(0))
                    .map_err(|_| "Receipt index is unavailable".to_owned())?
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| "Receipt index is unavailable".to_owned())?;
            let has_stamp = columns.iter().any(|column| column == "stamp");
            let has_change = columns.iter().any(|column| column == "change");
            let unseeded = if has_change {
                sql(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM receipt_rows WHERE change=0)",
                    [],
                    |row| row.get::<_, bool>(0),
                ))?
            } else {
                true
            };
            let frontier_behind = if has_change {
                let frontier: i64 = sql(db.query_row(
                    "SELECT COALESCE(CAST((SELECT value FROM receipt_meta WHERE key='change_seq') AS INTEGER),0)",
                    [],
                    |row| row.get(0),
                ))?;
                let maximum: i64 = sql(db.query_row(
                    "SELECT COALESCE(MAX(change),0) FROM receipt_rows",
                    [],
                    |row| row.get(0),
                ))?;
                frontier < maximum
            } else {
                false
            };
            let change_repair = !has_stamp || !has_change || unseeded || frontier_behind;
            let schema_transaction = if upgrade.is_none() && change_repair {
                Some(sql(db.unchecked_transaction())?)
            } else {
                None
            };
            if !has_stamp {
                sql(db.execute_batch(
                    "ALTER TABLE receipt_rows ADD COLUMN stamp TEXT NOT NULL DEFAULT '';",
                ))?;
            }
            if !has_change {
                sql(db.execute_batch(
                    "ALTER TABLE receipt_rows ADD COLUMN change INTEGER NOT NULL DEFAULT 0;",
                ))?;
            }
            if change_repair {
                // Seed every unseeded original once; later allocations
                // continue from the committed high-water.
                sql(db.execute_batch("UPDATE receipt_rows SET change=seq WHERE change=0;"))?;
            }
            sql(db.execute_batch(
                "CREATE INDEX IF NOT EXISTS receipt_change ON receipt_rows(source,change);
                 CREATE INDEX IF NOT EXISTS receipt_change_scope ON receipt_rows(source,agent,conversation,change,request);
                 CREATE INDEX IF NOT EXISTS receipt_change_initial ON receipt_rows(source,agent,run,change,request);",
            ))?;

            // The frontier must cover every seeded row, repairing a persisted
            // cut where the column exists but its sequence was never published.
            sql(db.execute(
                "INSERT INTO receipt_meta VALUES('change_seq',COALESCE((SELECT MAX(change) FROM receipt_rows),0)) ON CONFLICT(key) DO UPDATE SET value=CAST(MAX(CAST(value AS INTEGER),CAST(excluded.value AS INTEGER)) AS TEXT)",
                [],
            ))?;
            if change_repair {
                // A consumer must not resume a cursor from before a repair:
                // the sequence meaning changed, so old cursors restart.
                sql(db.execute(
                    "UPDATE receipt_meta SET value=? WHERE key='epoch'",
                    [uuid::Uuid::new_v4().to_string()],
                ))?;
            }
            if let Some(transaction) = schema_transaction {
                sql(transaction.commit())?;
            }
            if !verified {
                // Reissue the invalidation guards so a cache created with the
                // earlier trigger definition also exempts the first-prompt pin.
                sql(db.execute_batch(
                    "DROP TRIGGER IF EXISTS receipt_row_removed;
                     DROP TRIGGER IF EXISTS receipt_row_rebound;
                     CREATE TRIGGER receipt_row_removed AFTER DELETE ON receipt_rows BEGIN
                       UPDATE receipt_sources SET complete=0,indexed=MAX(0,indexed-1) WHERE source=OLD.source;
                       INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json','index_row_missing')
                         ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason;
                       UPDATE receipt_meta SET value=lower(hex(randomblob(16))) WHERE key='epoch';
                     END;
                     CREATE TRIGGER receipt_row_rebound AFTER UPDATE ON receipt_rows
                     WHEN (OLD.seq IS NOT NEW.seq OR OLD.source IS NOT NEW.source OR OLD.request IS NOT NEW.request
                       OR OLD.name IS NOT NEW.name OR OLD.run IS NOT NEW.run OR OLD.conversation IS NOT NEW.conversation
                       OR OLD.agent IS NOT NEW.agent OR OLD.first_message IS NOT NEW.first_message
                       OR OLD.at IS NOT NEW.at OR OLD.files IS NOT NEW.files OR OLD.binding IS NOT NEW.binding)
                       AND NOT (OLD.conversation='' AND NEW.conversation<>''
                         AND OLD.seq IS NEW.seq AND OLD.source IS NEW.source AND OLD.request IS NEW.request
                         AND OLD.name IS NEW.name AND OLD.run IS NEW.run AND OLD.agent IS NEW.agent
                         AND OLD.first_message IS NEW.first_message AND OLD.at IS NEW.at
                         AND OLD.files IS NEW.files AND OLD.binding IS NEW.binding)
                     BEGIN
                       UPDATE receipt_sources SET complete=0 WHERE source IN(OLD.source,NEW.source);
                       INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json','index_row_changed')
                         ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason;
                       UPDATE receipt_meta SET value=lower(hex(randomblob(16))) WHERE key='epoch';
                     END;",
                ))?;
            }
            sql(db.execute(
                "INSERT OR IGNORE INTO receipt_meta VALUES('epoch',?)",
                [uuid::Uuid::new_v4().to_string()],
            ))?;
            for source in Source::ALL {
                sql(db.execute(
                    "INSERT OR IGNORE INTO receipt_sources(source) VALUES(?)",
                    [source.key()],
                ))?;
            }
            sql(db.execute(
                "INSERT OR IGNORE INTO receipt_meta VALUES('verified_at',?)",
                [now().to_string()],
            ))?;
        }
        if create {
            sql(db.execute(
                "INSERT OR IGNORE INTO receipt_meta VALUES('cursor_secret',?)",
                [uuid::Uuid::new_v4().to_string()],
            ))?;
            if !verified {
                // A #2055 cache, or one with missing invalidation guards, has
                // no row-set proof. Do not inherit its old complete flags.
                sql(db.execute("UPDATE receipt_sources SET complete=0", []))?;
                sql(db.execute(
                    "UPDATE receipt_meta SET value=? WHERE key='epoch'",
                    [uuid::Uuid::new_v4().to_string()],
                ))?;
                sql(db.execute(
                    "INSERT INTO receipt_meta VALUES('rowset_protocol',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    [ROWSET_PROTOCOL],
                ))?;
            }
        }
        if let Some(transaction) = upgrade.take() {
            sql(transaction.commit())?;
        }
        drop(upgrade);
        Ok(Self {
            db,
            state: state.to_owned(),
        })
    }
    fn remember(&self, source: Source, path: &Path, value: &Value) -> Result<()> {
        // Sequence allocation and the row write must be visible together: a
        // reader that sees a higher change_seq must also see every lower row.
        if self.db.is_autocommit() {
            let transaction = sql(self.db.unchecked_transaction())?;
            self.remember_row(source, path, value)?;
            sql(transaction.commit())?;
            Ok(())
        } else {
            self.remember_row(source, path, value)
        }
    }
    fn remember_row(&self, source: Source, path: &Path, value: &Value) -> Result<()> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid receipt name")?;
        let binding = Binding::parse(source, value, filename)?;
        let stamp = file_stamp(path)?;
        let prior: Option<(String, String, String)> = sql(self
            .db
            .query_row(
                "SELECT binding,conversation,stamp FROM receipt_rows WHERE source=? AND request=?",
                params![source.key(), binding.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional())?;
        if let Some((original, conversation, prior_stamp)) = prior {
            if original != binding.fingerprint
                || (!conversation.is_empty() && conversation != binding.conversation)
            {
                return self.error(source, filename, "binding_changed");
            }
            // A content update keeps the immutable binding: the same original
            // identity, a new file stamp and one change for consumers. Only the
            // existing one-way first-prompt resolution may move scope.
            let resolved = conversation.is_empty();
            let conversation = if resolved {
                binding.conversation.clone()
            } else {
                conversation
            };
            if resolved || prior_stamp != stamp {
                let change = self.bump()?;
                sql(self.db.execute(
                    "UPDATE receipt_rows SET name=?,run=?,conversation=?,agent=?,first_message=?,at=?,files=?,stamp=?,change=? WHERE source=? AND request=?",
                    params![binding.name, binding.run, conversation, binding.agent, binding.first, binding.at, binding.files, stamp, change, source.key(), binding.id],
                ))?;
            }
        } else {
            let change = self.bump()?;
            sql(self.db.execute("INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding,stamp,change) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![source.key(),binding.id,binding.name,binding.run,binding.conversation,binding.agent,binding.first,binding.at,binding.files,binding.fingerprint,stamp,change]))?;
        }
        sql(self.db.execute(
            "DELETE FROM receipt_errors WHERE source=? AND name=?",
            params![source.key(), filename],
        ))?;
        Ok(())
    }
    fn bump(&self) -> Result<i64> {
        sql(self.db.query_row(
            "UPDATE receipt_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='change_seq' RETURNING CAST(value AS INTEGER)",
            [],
            |r| r.get(0),
        ))
    }
    fn error(&self, source: Source, name: &str, reason: &str) -> Result<()> {
        sql(self.db.execute("INSERT INTO receipt_errors VALUES(?,?,?) ON CONFLICT(source,name) DO UPDATE SET reason=excluded.reason",params![source.key(),name,reason]))?;
        Ok(())
    }
    fn page(&self, scope: &Scope, cursor: &str, limit: usize, files: bool) -> Result<Page> {
        self.page_until(scope, cursor, limit, files, None)
            .map_err(|error| error.message().to_owned())
    }
    fn page_until(
        &self,
        scope: &Scope,
        cursor: &str,
        limit: usize,
        files: bool,
        deadline: Option<Instant>,
    ) -> std::result::Result<Page, PageFailure> {
        if !(1..=PAGE_LIMIT).contains(&limit) || cursor.len() > 2048 {
            return Err(PageFailure::InvalidQuery);
        }
        if scope.run.is_empty() && scope.conversation.is_empty() {
            return Err(PageFailure::InvalidQuery);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(PageFailure::Deadline);
        }
        // Pin locator rows, namespace coverage and the high-water in one WAL
        // snapshot. Backfill finishing between separate queries must not turn
        // a prefix snapshot into complete legacy coverage.
        let transaction = sql(self.db.unchecked_transaction())?;
        let work = Arc::new(AtomicUsize::new(0));
        let counter = work.clone();
        self.db.progress_handler(
            1000,
            Some(move || {
                counter.fetch_add(1000, Ordering::Relaxed) >= 50_000
                    || deadline.is_some_and(|d| Instant::now() >= d)
            }),
        );
        let epoch: String = sql(self.db.query_row(
            "SELECT value FROM receipt_meta WHERE key='epoch'",
            [],
            |r| r.get(0),
        ))?;
        let secret: String = sql(self.db.query_row(
            "SELECT value FROM receipt_meta WHERE key='cursor_secret'",
            [],
            |r| r.get(0),
        ))?;
        if secret.len() != 36 || uuid::Uuid::parse_str(&secret).is_err() {
            return Err(PageFailure::Unavailable);
        }
        let (stamp, complete, indexed, errors): (String, bool, i64, i64) = sql(self.db.query_row(
            "SELECT stamp,complete,indexed,errors FROM receipt_sources WHERE source=?",
            [scope.source.key()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ))?;
        let current = migration::stamp(&self.state.join(scope.source.directory()));
        let verified = self.coverage_verified();
        let protocol = rowset_protocol(&self.db);
        let ready = verified && complete && current.as_ref().is_ok_and(|v| *v == stamp);
        let mut position = if cursor.is_empty() {
            Cursor {
                version: 1,
                epoch: epoch.clone(),
                scope: scope.hash(files),
                snapshot: sql(self.db.query_row(
                    "SELECT COALESCE(MAX(seq),0) FROM receipt_rows",
                    [],
                    |r| r.get(0),
                ))?,
                complete: ready && errors == 0,
                at: f64::MAX,
                id: String::new(),
            }
        } else {
            decode_cursor(&secret, cursor)?
        };
        if position.version != 1
            || position.epoch != epoch
            || position.scope != scope.hash(files)
            || position.snapshot < 0
            || !position.at.is_finite()
            || position.at < 0.0
            || position.id.len() > 128
        {
            return Err(PageFailure::Cursor);
        }
        let mut candidates = Vec::new();
        if let Some(name) = &scope.name {
            let mut statement=sql(self.db.prepare("SELECT request,seq,at,binding FROM receipt_rows INDEXED BY receipt_exact WHERE source=?1 AND name=?2 AND run=?3 AND conversation=?4 AND (at,request)<(?5,?6) ORDER BY at DESC,request DESC LIMIT ?7"))?;
            let rows = sql(statement.query_map(
                params![
                    scope.source.key(),
                    name,
                    scope.run,
                    scope.conversation,
                    position.at,
                    position.id,
                    EXAMINE + 1
                ],
                |r| {
                    Ok(Locator {
                        source: scope.source,
                        id: r.get(0)?,
                        sequence: r.get(1)?,
                        at: r.get(2)?,
                        fingerprint: r.get(3)?,
                    })
                },
            ))?;
            for row in rows {
                candidates.push(sql(row)?);
            }
        } else {
            for initial in [false, true] {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(PageFailure::Deadline);
                }
                if !initial && scope.conversation.is_empty() {
                    continue;
                }
                let index = match (files, initial) {
                    (true, true) => "receipt_file_initial",
                    (true, false) => "receipt_file_scope",
                    (false, true) => "receipt_initial",
                    (false, false) => "receipt_scope",
                };
                let extra = if initial {
                    "conversation='' AND first_message=1 AND run=?3"
                } else {
                    "conversation=?3"
                };
                let query=format!("SELECT request,seq,at,binding FROM receipt_rows INDEXED BY {index} WHERE source=?1 AND agent=?2 AND {extra} {} AND (at,request)<(?4,?5) ORDER BY at DESC,request DESC LIMIT ?6",if files {"AND files=1"} else {""});
                let mut statement = sql(self.db.prepare(&query))?;
                let rows = sql(statement.query_map(
                    params![
                        scope.source.key(),
                        scope.agent,
                        if initial {
                            &scope.run
                        } else {
                            &scope.conversation
                        },
                        position.at,
                        position.id,
                        EXAMINE + 1
                    ],
                    |r| {
                        Ok(Locator {
                            source: scope.source,
                            id: r.get(0)?,
                            sequence: r.get(1)?,
                            at: r.get(2)?,
                            fingerprint: r.get(3)?,
                        })
                    },
                ))?;
                for row in rows {
                    candidates.push(sql(row)?);
                }
            }
        }
        candidates.sort_by(|a, b| b.at.total_cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        let available = candidates.len();
        let mut scanned = 0;
        let mut locators = Vec::new();
        for locator in candidates.into_iter().take(EXAMINE) {
            if !locator.at.is_finite()
                || locator.at < 0.0
                || locator.id.is_empty()
                || !bounded_identity(&locator.id, 128)
                || locator.id.contains(['/', '\\'])
            {
                return Err(PageFailure::Unavailable);
            }
            position.at = locator.at;
            position.id = locator.id.clone();
            scanned += 1;
            if locator.sequence <= position.snapshot {
                locators.push(locator);
            }
            if locators.len() == limit {
                break;
            }
        }
        let more = scanned < available;
        let restart = ready && errors == 0 && !position.complete;
        let next = if more {
            encode_cursor(&secret, &position)?
        } else {
            String::new()
        };
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(PageFailure::Deadline);
        }
        sql(transaction.commit())?;
        Ok(Page {
            locators,
            metadata: json!({"schema":1,"state":if ready && errors==0 {"indexed"}else if errors>0 {"partial"}else{"building"},"coverage":"indexed_snapshot","absence_confirmed":false,"migration_complete":ready,"index_protocol_verified":protocol,"catalog_complete_at_snapshot":position.complete,"restart_required":restart,"indexed":indexed,"unattributed_errors":errors,"examined":scanned,"snapshot":position.snapshot,"has_more":more,"next_cursor":next}),
        })
    }
    pub(super) fn original(&self, locator: &Locator) -> std::result::Result<Value, &'static str> {
        if locator.id.is_empty()
            || !bounded_identity(&locator.id, 128)
            || locator.id.contains(['/', '\\'])
        {
            return Err("corrupt_index");
        }
        let filename = format!("{}.json", locator.id);
        let value = read_original(&self.state.join(locator.source.directory()).join(&filename))?;
        let binding = Binding::parse(locator.source, &value, &filename).map_err(|_| "corrupt")?;
        if binding.fingerprint != locator.fingerprint {
            return Err("binding_changed");
        }
        Ok(value)
    }
}

pub(super) const CURSOR_REJECTED: &str = "Receipt change cursor belongs to another target or index";

/// One original whose indexed content stamp changed since the caller's
/// frontier. The consumer validates [Catalog::original] before importing it.
#[derive(Debug)]
pub(super) struct ChangedRow {
    pub(super) locator: Locator,
    pub(super) stamp: String,
    pub(super) change: i64,
}

/// A bounded changed-since page with a frozen upper change sequence. Changes
/// committed while this chain is paged get a newer sequence and wait for the
/// caller's next chain, so a resumed frontier is stable.
#[derive(Debug)]
pub(super) struct ChangedPage {
    pub(super) rows: Vec<ChangedRow>,
    pub(super) cursor: String,
    pub(super) drained: bool,
    pub(super) coverage: Coverage,
    /// Discovery currently covers the directory ancestors of this page.
    pub(super) ready: bool,
    /// Changes committed beyond the frozen upper wait for the caller's next
    /// chain, so a paging consumer never claims completion over them.
    pub(super) pending: bool,
    pub(super) restart_required: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeCursor {
    version: u8,
    epoch: String,
    scope: String,
    upper: i64,
    after: i64,
}

impl Catalog {
    /// A cheap structural and freshness check for a pinned read. Presence of
    /// the invalidation guards, the trigger-maintained row witness and a
    /// bounded verification age keep silent row loss and coarse-timestamp
    /// originals from masquerading as complete coverage. It neither walks the
    /// directory nor writes: the caller requests the background worker.
    fn coverage_verified(&self) -> bool {
        if !rowset_protocol(&self.db) {
            return false;
        }
        let witness: bool = self
            .db
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM receipt_sources WHERE indexed != (SELECT COUNT(*) FROM receipt_rows WHERE source=receipt_sources.source))",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !witness {
            return false;
        }
        // A row without a sequence, or a frontier behind MAX(change), means a
        // schema repair has not published its sequence yet. Never call that
        // Complete: the cursor must restart after the writer's repair.
        let consistent: bool = self
            .db
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM receipt_rows WHERE change=0)
                   AND COALESCE((SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'),0)
                       >= COALESCE((SELECT MAX(change) FROM receipt_rows),0)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !consistent {
            return false;
        }
        true
    }
    /// A stale-but-consistent locator stays complete: age only schedules the
    /// background re-verification walk, which flips coverage when it finds a
    /// discrepancy. No consumer waits on the 600 s timer by itself.
    fn verification_due(&self) -> bool {
        let verified_at = self
            .db
            .query_row(
                "SELECT value FROM receipt_meta WHERE key='verified_at'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0);
        now() - verified_at >= VERIFY_SECONDS
    }
    fn change_epoch(&self) -> Result<(String, String, i64)> {
        let epoch: String = sql(self.db.query_row(
            "SELECT value FROM receipt_meta WHERE key='epoch'",
            [],
            |r| r.get(0),
        ))?;
        let secret: String = sql(self.db.query_row(
            "SELECT value FROM receipt_meta WHERE key='cursor_secret'",
            [],
            |r| r.get(0),
        ))?;
        let current: i64 = sql(self.db.query_row(
            "SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'",
            [],
            |r| r.get(0),
        ))?;
        Ok((epoch, secret, current))
    }
    fn changed_page(
        &self,
        scope: &Scope,
        cursor: &str,
        limit: usize,
        files: bool,
    ) -> Result<ChangedPage> {
        if !(1..=PAGE_LIMIT).contains(&limit) || cursor.len() > 2048 {
            return Err("Invalid receipt change page".into());
        }
        // The frozen upper and the rows it covers come from one snapshot, so
        // an upper can never pass a row that is not visible to this read.
        let transaction = sql(self.db.unchecked_transaction())?;
        let page = self.changed_page_snapshot(scope, cursor, limit, files)?;
        sql(transaction.commit())?;
        Ok(page)
    }
    fn changed_page_snapshot(
        &self,
        scope: &Scope,
        cursor: &str,
        limit: usize,
        files: bool,
    ) -> Result<ChangedPage> {
        if scope.run.is_empty() && scope.conversation.is_empty() {
            return Err("Invalid receipt change page".into());
        }
        if scope.run.is_empty() && scope.conversation.is_empty() {
            return Err("Invalid receipt change page".into());
        }
        let (epoch, secret, current) = self.change_epoch()?;
        let scope_hash = scope.hash(files);
        let prior: Option<ChangeCursor> = if cursor.is_empty() {
            None
        } else {
            let bytes = URL_SAFE_NO_PAD
                .decode(cursor.split_once('.').map_or(cursor, |(body, _)| body))
                .map_err(|_| CURSOR_REJECTED)?;
            let expected = cursor_mac(&secret, &bytes);
            let signature = cursor
                .split_once('.')
                .map(|(_, signature)| signature)
                .unwrap_or("");
            if signature.len() != expected.len()
                || !signature
                    .bytes()
                    .zip(expected.bytes())
                    .fold(true, |same, (a, b)| same & (a == b))
            {
                return Err(CURSOR_REJECTED.into());
            }
            Some(serde_json::from_slice(&bytes).map_err(|_| CURSOR_REJECTED)?)
        };
        let (after, upper) = match prior {
            Some(position)
                if position.version == 1
                    && position.epoch == epoch
                    && position.scope == scope_hash
                    && position.after >= 0
                    && position.upper >= position.after =>
            {
                // A drained frontier opens a new frozen chain at the current
                // sequence; a live one keeps its original upper bound.
                if position.after == position.upper {
                    (position.after, current)
                } else {
                    (position.after, position.upper)
                }
            }
            Some(_) => return Err(CURSOR_REJECTED.into()),
            None => (0, current),
        };
        let mut rows = Vec::new();
        for initial in [false, true] {
            if !initial && scope.conversation.is_empty() {
                continue;
            }
            let index = match (files, initial) {
                (true, true) => "receipt_change_initial",
                (true, false) => "receipt_change_scope",
                (false, true) => "receipt_change_initial",
                (false, false) => "receipt_change_scope",
            };
            let extra = if initial {
                "conversation='' AND first_message=1 AND run=?3"
            } else {
                "conversation=?3"
            };
            let query = format!(
                "SELECT request,at,binding,stamp,change FROM receipt_rows INDEXED BY {index} WHERE source=?1 AND agent=?2 AND {extra} AND change>?4 AND change<=?5 ORDER BY change LIMIT ?6"
            );
            let mut statement = sql(self.db.prepare(&query))?;
            let mapped = sql(statement.query_map(
                params![
                    scope.source.key(),
                    scope.agent,
                    if initial {
                        &scope.run
                    } else {
                        &scope.conversation
                    },
                    after,
                    upper,
                    limit
                ],
                |r| {
                    Ok(ChangedRow {
                        locator: Locator {
                            source: scope.source,
                            id: r.get(0)?,
                            sequence: r.get(4)?,
                            at: r.get(1)?,
                            fingerprint: r.get(2)?,
                        },
                        stamp: r.get(3)?,
                        change: r.get(4)?,
                    })
                },
            ))?;
            for row in mapped {
                rows.push(sql(row)?);
            }
        }
        rows.sort_by_key(|row| row.change);
        rows.truncate(limit);
        let (stamp, complete, indexed, errors): (String, bool, i64, i64) = sql(self.db.query_row(
            "SELECT stamp,complete,indexed,errors FROM receipt_sources WHERE source=?",
            [scope.source.key()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ))?;
        let current_directory = migration::stamp(&self.state.join(scope.source.directory()));
        // The pinned changed page retains the row-set protocol check through
        // coverage_verified(): a cache without its invalidation guards proves
        // no coverage.
        let verified = self.coverage_verified();
        let ready = verified
            && complete
            && current_directory
                .as_ref()
                .is_ok_and(|value| *value == stamp);
        let coverage = if ready && errors == 0 {
            Coverage::Complete
        } else if ready || (indexed > 0 && stamp == "absent") {
            // Ready with errors, or an indexed source whose directory is gone.
            Coverage::Gap
        } else {
            Coverage::Partial
        };
        let last = rows.last().map(|row| row.change).unwrap_or(after);
        let drained = rows.len() < limit || last >= upper;
        let after_next = if drained { upper } else { last };
        let next = ChangeCursor {
            version: 1,
            epoch,
            scope: scope_hash,
            upper,
            after: after_next,
        };
        let bytes = serde_json::to_vec(&next).map_err(|_| "Invalid receipt change page")?;
        let cursor = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&bytes),
            cursor_mac(&secret, &bytes)
        );
        Ok(ChangedPage {
            rows,
            cursor,
            drained,
            coverage,
            ready,
            pending: current > upper,
            restart_required: !ready && complete,
        })
    }
}

/// Some(record)'s bounded changed-since page. A missing or not-yet-migrated
/// index is explicitly unavailable; callers request discovery and project
/// indexing instead of absence.
pub(super) fn changed(
    record: &Value,
    agent: &str,
    cursor: &str,
    limit: usize,
    files: bool,
) -> Result<(Catalog, ChangedPage)> {
    let state = root();
    let scope = Scope::new(record, agent);
    let catalog = match Catalog::open(&state, false) {
        Ok(catalog) => catalog,
        Err(error) => {
            // A fresh or not-yet-migrated locator is seeded by the canonical
            // indexer in bounded slices; inspection paths keep their own rule.
            let _ = migration::advance(&state, Duration::from_millis(200));
            match Catalog::open(&state, false) {
                Ok(catalog) => catalog,
                Err(_) => return Err(error),
            }
        }
    };
    let page = catalog.changed_page(&scope, cursor, limit, files)?;
    if page.coverage != Coverage::Complete {
        let _ = migration::advance(&state, Duration::from_millis(200));
        migration::request(&state);
        let advanced = catalog.changed_page(&scope, cursor, limit, files)?;
        return Ok((catalog, advanced));
    }
    if catalog.verification_due() {
        migration::request(&state);
    }
    Ok((catalog, page))
}

fn read_original(path: &Path) -> std::result::Result<Value, &'static str> {
    let mut options = OpenOptions::new();
    options.read(true);
    crate::platform::refuse_link(&mut options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            "missing"
        } else {
            "unreadable"
        }
    })?;
    if !file.metadata().map_err(|_| "unreadable")?.is_file() {
        return Err("unreadable");
    }
    let mut bytes = Vec::new();
    file.take(ORIGINAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable")?;
    if bytes.len() as u64 > ORIGINAL_BYTES {
        return Err("oversized");
    }
    serde_json::from_slice(&bytes).map_err(|_| "corrupt")
}

/// A receipt original's content stamp. A missing file still yields a locator
/// change, and the consumer validates the original before importing it.
fn file_stamp(path: &Path) -> Result<String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let (seconds, nanos) = crate::platform::modified(&metadata);
            Ok(format!("{}:{seconds}:{nanos}", metadata.len()))
        }
        Ok(_) => Err("Receipt original is not a regular file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok("missing".into()),
        Err(_) => Err("Receipt original is unavailable".into()),
    }
}

/// Projection is advisory. Missing index/original data is never proof of absence
/// and this module is deliberately absent from every send/retry admission check.
pub(super) fn page(
    record: &Value,
    agent: &str,
    cursor: &str,
    limit: usize,
) -> Result<(Catalog, Page)> {
    let state = root();
    let catalog = Catalog::open(&state, false)?;
    let page = catalog.page(&Scope::new(record, agent), cursor, limit, true)?;
    if page.metadata["migration_complete"] != true || catalog.verification_due() {
        migration::request(&state);
    }
    Ok((catalog, page))
}
pub(super) fn request_migration() {
    migration::request(&root());
}

/// Preserve the authoritative atomic write and its error exactly. Index failure
/// never turns a possibly delivered send into a refusal or allows its replay.
pub(super) fn persist(path: &Path, value: &Value) -> Result<()> {
    let source = Source::ALL.into_iter().find(|source| {
        path.parent()
            .is_some_and(|p| p.ends_with(source.directory()))
    });
    let before = source
        .and_then(|_| path.parent())
        .and_then(|p| migration::stamp(p).ok());
    atomic(path, &value.to_string())?;
    if let Some(source) = source {
        let remember = (|| -> Result<()> {
            let parent = path.parent().ok_or("Invalid receipt path")?;
            let owner = if matches!(source, Source::Terminal | Source::Question) {
                parent.parent()
            } else {
                parent.parent().and_then(Path::parent)
            }
            .ok_or("Invalid receipt owner")?;
            let state =
                crate::platform::canonical(owner).map_err(|_| "Receipt owner is unavailable")?;
            if crate::platform::canonical(parent).map_err(|_| "Receipt directory is unavailable")?
                != state.join(source.directory())
            {
                return Err("Foreign receipt directory".into());
            }
            let catalog = Catalog::open(&state, true)?;
            catalog.remember(source, path, value)?;
            // Only advance an already reconciled snapshot. A missed/legacy
            // writer leaves a mismatch, so inspection requests background work.
            if let (Some(before), Ok(after)) = (before, migration::stamp(parent)) {
                sql(catalog.db.execute(
                    "UPDATE receipt_sources SET stamp=? WHERE source=? AND complete=1 AND stamp=?",
                    params![after, source.key(), before],
                ))?;
            }
            Ok(())
        })();
        let _ = remember;
    }
    Ok(())
}

pub(super) fn worker() -> Result<i32> {
    migration::worker(&root())
}
