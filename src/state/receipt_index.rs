//! Rebuildable, body-free receipt locators. Original JSON alone proves delivery.
//! Projection reads never enumerate source directories. Legacy discovery runs
//! in bounded background batches; publication registration compares bounded
//! directory-entry metadata. Neither deletes or repairs original receipts.
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
mod pin;
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
// 4: the rebound trigger also moves a row's count between sources, so the
// witness token stays exact across a source change (zerus-z5uax.1).
const ROWSET_PROTOCOL: &str = "4";
const VERIFY_SECONDS: f64 = 600.0;
// The row-set witness token (zerus-z5uax): one receipt_meta entry per source.
const WITNESS_PREFIX: &str = "rowset_witness:";
// A reader verifies at most this many rows appended after the last exact
// witness; beyond it the token is stale (unknown) until the worker re-witnesses.
const WITNESS_DELTA: i64 = 4096;
const WITNESS_MISMATCHES: &str = "rowset_witness_mismatches";

/// The exact count a worker last verified for one source, under the schema
/// it verified (its delete/rebind triggers) and up to the row sequence then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Witnessed {
    Agreed,
    Mismatch,
    Raced,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WitnessEntry {
    schema: i64,
    seq: i64,
    count: i64,
    at: f64,
}

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

// Rebuildable projection metadata; the original fingerprint and the pinned
// conversation still have to agree before any of these fields may be repaired.
struct CachedRow {
    fingerprint: String,
    stamp: String,
    name: String,
    run: String,
    conversation: String,
    agent: String,
    first: i64,
    at: f64,
    files: i64,
}
impl CachedRow {
    fn matches(&self, binding: &Binding) -> bool {
        self.conversation == binding.conversation && self.matches_except_conversation(binding)
    }
    fn matches_except_conversation(&self, binding: &Binding) -> bool {
        self.name == binding.name
            && self.run == binding.run
            && self.agent == binding.agent
            && self.first == i64::from(binding.first)
            && self.at == binding.at
            && self.files == i64::from(binding.files)
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
                BEGIN
                  UPDATE receipt_sources SET complete=0 WHERE source IN(OLD.source,NEW.source);
                  UPDATE receipt_sources SET indexed=MAX(0,indexed-1) WHERE source=OLD.source AND OLD.source IS NOT NEW.source;
                  UPDATE receipt_sources SET indexed=indexed+1 WHERE source=NEW.source AND OLD.source IS NOT NEW.source;
                  INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json',CASE WHEN OLD.conversation IS NOT NEW.conversation THEN 'binding_changed' ELSE 'index_row_changed' END)
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
                 CREATE INDEX IF NOT EXISTS receipt_change_initial ON receipt_rows(source,agent,run,change,request);
                 CREATE INDEX IF NOT EXISTS receipt_seq ON receipt_rows(source,seq);
                 CREATE INDEX IF NOT EXISTS receipt_gap ON receipt_rows(source) WHERE substr(stamp,1,4)='gap:';",
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
                // Legacy caches exempted every empty-to-nonempty conversation
                // change. Replace that SQL-only exemption atomically.
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
                     BEGIN
                       UPDATE receipt_sources SET complete=0 WHERE source IN(OLD.source,NEW.source);
                       UPDATE receipt_sources SET indexed=MAX(0,indexed-1) WHERE source=OLD.source AND OLD.source IS NOT NEW.source;
                       UPDATE receipt_sources SET indexed=indexed+1 WHERE source=NEW.source AND OLD.source IS NOT NEW.source;
                       INSERT INTO receipt_errors VALUES(OLD.source,OLD.request||'.json',CASE WHEN OLD.conversation IS NOT NEW.conversation THEN 'binding_changed' ELSE 'index_row_changed' END)
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
        let catalog = Self {
            db,
            state: state.to_owned(),
        };
        // A new, upgraded or schema-changed cache has no current token. Take
        // the exact witness once here; if that is busy or fails, readers stay
        // unknown and request the worker, which re-witnesses on every run.
        if create && !catalog.witness_present() {
            let _ = catalog.witness();
        }
        Ok(catalog)
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
    fn cached_row(&self, source: Source, request: &str) -> Result<Option<CachedRow>> {
        sql(self
            .db
            .query_row(
                "SELECT binding,stamp,name,run,conversation,agent,first_message,at,files
               FROM receipt_rows WHERE source=? AND request=?",
                params![source.key(), request],
                |row| {
                    Ok(CachedRow {
                        fingerprint: row.get(0)?,
                        stamp: row.get(1)?,
                        name: row.get(2)?,
                        run: row.get(3)?,
                        conversation: row.get(4)?,
                        agent: row.get(5)?,
                        first: row.get(6)?,
                        at: row.get(7)?,
                        files: row.get(8)?,
                    })
                },
            )
            .optional())
    }
    fn remember_row(&self, source: Source, path: &Path, value: &Value) -> Result<()> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid receipt name")?;
        let binding = Binding::parse(source, value, filename)?;
        let stamp = file_stamp(path)?;
        let mut proven_pin = false;
        if let Some(prior) = self.cached_row(source, &binding.id)? {
            if prior.fingerprint != binding.fingerprint
                || (!prior.conversation.is_empty() && prior.conversation != binding.conversation)
            {
                return self.error(source, filename, "binding_changed");
            }
            // An unchanged original can have corrupted cached projection fields.
            // Restore those too; preserve the immutable fingerprint and the
            // existing one-way first-prompt conversation pin.
            // An empty cached conversation with an equally empty original is
            // unchanged: rewriting it would publish a change on every walk.
            let resolved = prior.conversation.is_empty();
            if prior.stamp != stamp || !prior.matches(&binding) {
                let proof = if resolved && !binding.conversation.is_empty() {
                    let original = self.state.join(source.directory()).join(filename);
                    let Some(proof) = pin::ProvenPin::read(source, &original, &binding, &stamp)
                    else {
                        return self.pin_conflict(source, filename);
                    };
                    Some(proof)
                } else {
                    None
                };
                let change = self.bump()?;
                let write = || {
                    self.db.execute(
                    "UPDATE receipt_rows SET name=?,run=?,conversation=?,agent=?,first_message=?,at=?,files=?,stamp=?,change=? WHERE source=? AND request=?",
                    params![binding.name, binding.run, binding.conversation, binding.agent, binding.first, binding.at, binding.files, stamp, change, source.key(), binding.id],
                )
                };
                if let Some(proof) = proof.filter(|_| prior.matches_except_conversation(&binding)) {
                    proven_pin = true;
                    if proof.update(&self.db, write).is_err() {
                        return self.pin_conflict(source, filename);
                    }
                } else {
                    sql(write())?;
                }
            }
        } else {
            let change = self.bump()?;
            sql(self.db.execute("INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding,stamp,change) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![source.key(),binding.id,binding.name,binding.run,binding.conversation,binding.agent,binding.first,binding.at,binding.files,binding.fingerprint,stamp,change]))?;
        }
        // Clear a discrepancy only after verifying the actual stored row. A
        // failed write or another cache trigger must keep explicit unknownness.
        let verified = self.cached_row(source, &binding.id)?;
        if verified.is_none_or(|row| {
            row.fingerprint != binding.fingerprint || row.stamp != stamp || !row.matches(&binding)
        }) {
            return if proven_pin {
                self.pin_conflict(source, filename)
            } else {
                self.error(source, filename, "index_row_changed")
            };
        }
        sql(self.db.execute(
            "DELETE FROM receipt_errors WHERE source=? AND name=?",
            params![source.key(), filename],
        ))?;
        Ok(())
    }
    fn pin_conflict(&self, source: Source, filename: &str) -> Result<()> {
        sql(self.db.execute(
            "UPDATE receipt_sources SET complete=0 WHERE source=?",
            [source.key()],
        ))?;
        sql(self.db.execute(
            "UPDATE receipt_meta SET value=? WHERE key='epoch'",
            [uuid::Uuid::new_v4().to_string()],
        ))?;
        self.error(source, filename, "binding_changed")
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
        // Run the coverage check first in the same snapshot, bounded by the
        // deadline alone: an exhausted page budget must never read as
        // unverified coverage (zerus-hn36l). Its row-set witness token keeps
        // the check bounded whatever the history size (zerus-z5uax). The
        // page's own bounded queries keep their work cap below.
        self.db.progress_handler(
            1000,
            Some(move || deadline.is_some_and(|d| Instant::now() >= d)),
        );
        let verified = self.coverage_verified();
        let protocol = rowset_protocol(&self.db);
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
        if !self.witness_current() {
            return false;
        }
        // A row without a sequence, or a frontier behind MAX(change), means a
        // schema repair has not published its sequence yet. Never call that
        // Complete: the cursor must restart after the writer's repair.
        // Per-source seeks on receipt_change(source,change): a whole-table
        // scan here grew with the send history. Pages read only these
        // sources, and receipt_sources lists each source a row can name.
        let consistent: bool = self
            .db
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM receipt_sources s WHERE EXISTS
                     (SELECT 1 FROM receipt_rows r WHERE r.source=s.source AND r.change=0))
                   AND COALESCE((SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'),0)
                       >= COALESCE((SELECT MAX((SELECT MAX(change) FROM receipt_rows r WHERE r.source=s.source))
                                      FROM receipt_sources s),0)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !consistent {
            return false;
        }
        true
    }
    /// The row-set witness in bounded work, whatever the history size: every
    /// source's trigger counter equals the exact count a worker last verified
    /// plus the rows appended after it, and the schema (with its delete and
    /// rebind triggers) is unchanged since. A deletion fires those triggers,
    /// and a schema change voids the token. More than WITNESS_DELTA appended
    /// rows, a missing or foreign token, or any read failure is unknown: the
    /// caller requests the worker, which re-witnesses the exact rows.
    fn witness_current(&self) -> bool {
        let sources: rusqlite::Result<Vec<String>> = self
            .db
            .prepare("SELECT source FROM receipt_sources")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect()
            });
        let Ok(sources) = sources else {
            return false;
        };
        !sources.is_empty()
            && sources
                .iter()
                .all(|source| self.source_witness_current(source))
    }
    /// One source's row-set witness in bounded work (see witness_current).
    pub(super) fn source_witness_current(&self, source: &str) -> bool {
        let read = || -> rusqlite::Result<bool> {
            let schema: i64 = self
                .db
                .query_row("PRAGMA schema_version", [], |row| row.get(0))?;
            let (indexed, value): (i64, Option<String>) = self.db.query_row(
                "SELECT s.indexed,m.value FROM receipt_sources s
                   LEFT JOIN receipt_meta m ON m.key=?1||s.source WHERE s.source=?2",
                params![WITNESS_PREFIX, source],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(entry) =
                value.and_then(|value| serde_json::from_str::<WitnessEntry>(&value).ok())
            else {
                return Ok(false);
            };
            if entry.schema != schema {
                return Ok(false);
            }
            // The source's rows after its own token, on the (source,seq)
            // index and capped: a missing index is a read failure (unknown).
            let added: i64 = self.db.query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM receipt_rows INDEXED BY receipt_seq
                   WHERE source=?1 AND seq>?2 LIMIT ?3)",
                params![source, entry.seq, WITNESS_DELTA + 1],
                |row| row.get(0),
            )?;
            Ok(added <= WITNESS_DELTA && indexed == entry.count + added)
        };
        read().unwrap_or(false)
    }
    /// Whether every source has a token taken under the current schema.
    fn witness_present(&self) -> bool {
        let schema: rusqlite::Result<i64> = self
            .db
            .query_row("PRAGMA schema_version", [], |row| row.get(0));
        let Ok(schema) = schema else {
            return false;
        };
        Source::ALL.into_iter().all(|source| {
            self.db
                .query_row(
                    "SELECT value FROM receipt_meta WHERE key=?",
                    [format!("{WITNESS_PREFIX}{}", source.key())],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .and_then(|value| serde_json::from_str::<WitnessEntry>(&value).ok())
                .is_some_and(|entry| entry.schema == schema)
        })
    }
    /// Publish one source's row-set witness inside the caller's transaction:
    /// `counted` is an exact count of its rows taken under guards that rule
    /// out a concurrent change (the walk's bounded count, or a snapshot count
    /// plus the rows appended since). The trigger counter must equal it.
    /// Agreement refreshes the source's token; disagreement is a recorded
    /// mismatch. Constant SQL work: no row is counted here.
    pub(super) fn witness_publish(
        &self,
        source: Source,
        counter: i64,
        counted: i64,
    ) -> Result<bool> {
        if counter != counted {
            self.witness_mismatch(source, counter, counted)?;
            return Ok(false);
        }
        let schema: i64 = sql(self
            .db
            .query_row("PRAGMA schema_version", [], |row| row.get(0)))?;
        let seq: i64 =
            sql(self
                .db
                .query_row("SELECT COALESCE(MAX(seq),0) FROM receipt_rows", [], |row| {
                    row.get(0)
                }))?;
        let entry = WitnessEntry {
            schema,
            seq,
            count: counted,
            at: now(),
        };
        sql(self.db.execute(
            "INSERT INTO receipt_meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                format!("{WITNESS_PREFIX}{}", source.key()),
                serde_json::to_string(&entry).map_err(|_| "Receipt index is unavailable")?
            ],
        ))?;
        Ok(true)
    }
    /// A trigger counter that disagrees with its rows. Nothing is re-sealed
    /// here: the whole token is cleared, this source's coverage becomes
    /// unknown, cursors restart, and a visible error plus a durable diagnostic
    /// are recorded. The counter takes the exact count so that a later walk,
    /// which starts after this record and re-reads every original, can seal
    /// the source again; only such a walk clears the error.
    fn witness_mismatch(&self, source: Source, counter: i64, exact: i64) -> Result<()> {
        sql(self.db.execute(
            "DELETE FROM receipt_meta WHERE key GLOB ?1",
            [format!("{WITNESS_PREFIX}*")],
        ))?;
        sql(self.db.execute(
            "UPDATE receipt_sources SET complete=0,indexed=?1 WHERE source=?2",
            params![exact, source.key()],
        ))?;
        sql(self.db.execute(
            "UPDATE receipt_meta SET value=? WHERE key='epoch'",
            [uuid::Uuid::new_v4().to_string()],
        ))?;
        self.error(source, "@witness", "witness_mismatch")?;
        let prior: i64 = sql(self
            .db
            .query_row(
                "SELECT value FROM receipt_meta WHERE key=?",
                [WITNESS_MISMATCHES],
                |row| row.get::<_, String>(0),
            )
            .optional())?
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|value| value["count"].as_i64())
        .unwrap_or(0);
        sql(self.db.execute(
            "INSERT INTO receipt_meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                WITNESS_MISMATCHES,
                json!({"count":prior+1,"source":source.key(),"counter":counter,"rows":exact,"at":now()}).to_string()
            ],
        ))?;
        Ok(())
    }
    /// Re-witness every source exactly: each worker run, and a new, upgraded
    /// or schema-changed cache. The exact counts come from one read snapshot,
    /// without the write lock; a short IMMEDIATE transaction then adds the
    /// rows appended since that snapshot and publishes. A delete, rebind or
    /// schema change in between (each rotates the epoch or the schema) is a
    /// race: nothing changes and the next run retries. Any mismatch leaves no
    /// token at all, so readers stay unknown until a clean witness.
    pub(super) fn witness(&self) -> Result<Witnessed> {
        let (epoch, schema, watermark, counts) = {
            let read = sql(self.db.unchecked_transaction())?;
            let epoch: String = sql(self.db.query_row(
                "SELECT value FROM receipt_meta WHERE key='epoch'",
                [],
                |row| row.get(0),
            ))?;
            let schema: i64 = sql(self
                .db
                .query_row("PRAGMA schema_version", [], |row| row.get(0)))?;
            let watermark: i64 = sql(self.db.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM receipt_rows",
                [],
                |row| row.get(0),
            ))?;
            let mut counts = Vec::with_capacity(Source::ALL.len());
            for source in Source::ALL {
                counts.push(sql(self.db.query_row(
                    "SELECT COUNT(*) FROM receipt_rows WHERE source=?",
                    [source.key()],
                    |row| row.get::<_, i64>(0),
                ))?);
            }
            sql(read.commit())?;
            (epoch, schema, watermark, counts)
        };
        let write = sql(rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        ))?;
        let now_epoch: String = sql(self.db.query_row(
            "SELECT value FROM receipt_meta WHERE key='epoch'",
            [],
            |row| row.get(0),
        ))?;
        let now_schema: i64 = sql(self
            .db
            .query_row("PRAGMA schema_version", [], |row| row.get(0)))?;
        if now_epoch != epoch || now_schema != schema {
            sql(write.commit())?;
            return Ok(Witnessed::Raced);
        }
        let mut agreed = true;
        for (source, counted) in Source::ALL.into_iter().zip(counts) {
            let appended: i64 = sql(self.db.query_row(
                "SELECT COUNT(*) FROM receipt_rows INDEXED BY receipt_seq WHERE source=?2 AND seq>?1",
                params![watermark, source.key()],
                |row| row.get(0),
            ))?;
            let counter: i64 = sql(self.db.query_row(
                "SELECT indexed FROM receipt_sources WHERE source=?",
                [source.key()],
                |row| row.get(0),
            ))?;
            agreed &= self.witness_publish(source, counter, counted + appended)?;
        }
        if !agreed {
            sql(self.db.execute(
                "DELETE FROM receipt_meta WHERE key GLOB ?1",
                [format!("{WITNESS_PREFIX}*")],
            ))?;
        }
        sql(write.commit())?;
        Ok(if agreed {
            Witnessed::Agreed
        } else {
            Witnessed::Mismatch
        })
    }
    /// A stale-but-consistent locator stays complete: age only schedules the
    /// background re-verification walk, which flips coverage when it finds a
    /// discrepancy. No consumer waits on the 600 s timer by itself. The
    /// row-set token has the same cadence: a missing or old token is due.
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
        let witnessed_at = Source::ALL
            .into_iter()
            .map(|source| {
                self.db
                    .query_row(
                        "SELECT value FROM receipt_meta WHERE key=?",
                        [format!("{WITNESS_PREFIX}{}", source.key())],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|value| serde_json::from_str::<WitnessEntry>(&value).ok())
                    .map_or(0.0, |entry| entry.at)
            })
            .fold(f64::INFINITY, f64::min);
        now() - verified_at.min(witnessed_at) >= VERIFY_SECONDS
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

/// The content stamp of `file_stamp` and the modification time, from one
/// metadata read. None for anything but a regular file.
fn file_state(path: &Path) -> Option<(String, f64)> {
    let metadata = fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())?;
    let (seconds, nanos) = crate::platform::modified(&metadata);
    Some((
        format!("{}:{seconds}:{nanos}", metadata.len()),
        seconds as f64 + nanos as f64 / 1e9,
    ))
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

fn receipt_source(path: &Path) -> Option<Source> {
    Source::ALL.into_iter().find(|source| {
        path.parent()
            .is_some_and(|p| p.ends_with(source.directory()))
    })
}

fn receipt_location(path: &Path, source: Source) -> Result<(PathBuf, &str)> {
    let parent = path.parent().ok_or("Invalid receipt path")?;
    let owner = if matches!(source, Source::Terminal | Source::Question) {
        parent.parent()
    } else {
        parent.parent().and_then(Path::parent)
    }
    .ok_or("Invalid receipt owner")?;
    let state = crate::platform::canonical(owner).map_err(|_| "Receipt owner is unavailable")?;
    if crate::platform::canonical(parent).map_err(|_| "Receipt directory is unavailable")?
        != state.join(source.directory())
    {
        return Err("Foreign receipt directory".into());
    }
    let filename = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("Invalid receipt name")?;
    Ok((state, filename))
}

/// Metadata only, never a permission to write an original. A scan uses at most
/// 2 MiB of entry digests and retains only their sorted SHA-256 fingerprint.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Clone)]
pub(in crate::state) struct BeforeCommit {
    path: PathBuf,
    entries: ReceiptEntries,
}
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Clone)]
struct ReceiptEntries {
    object: (u64, u64),
    stamp: String,
    others: usize,
    fingerprint: [u8; 32],
    receipts: usize,
    target: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn receipt_entries(path: &Path) -> Option<ReceiptEntries> {
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
    #[cfg(unix)]
    {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{DirEntryExt, MetadataExt},
        };
        let started = Instant::now();
        let parent = path.parent()?;
        let target = path.file_name()?;
        let metadata = fs::symlink_metadata(parent).ok()?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return None;
        }
        let object = (metadata.dev(), metadata.ino());
        let unchanged = |other: &fs::Metadata| {
            other.is_dir()
                && !other.file_type().is_symlink()
                && (
                    other.dev(),
                    other.ino(),
                    other.len(),
                    other.mtime(),
                    other.mtime_nsec(),
                    other.ctime(),
                    other.ctime_nsec(),
                ) == (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
        };
        let mut result = ReceiptEntries {
            object,
            stamp: migration::stamp(parent).ok()?,
            others: 0,
            fingerprint: [0; 32],
            receipts: 0,
            target: false,
        };
        let mut entries: Vec<[u8; 32]> = Vec::new();
        for (number, entry) in fs::read_dir(parent).ok()?.enumerate() {
            if number >= 65_536 || started.elapsed() >= Duration::from_millis(20) {
                return None;
            }
            let entry = entry.ok()?;
            let name = entry.file_name();
            if name.as_bytes().ends_with(b".json") {
                result.receipts += 1;
            }
            if name == target {
                result.target = true;
                continue;
            }
            if entry.ino() == 0 {
                return None;
            }
            let mut hash = Sha256::new();
            hash.update(entry.ino().to_be_bytes());
            hash.update(name.as_bytes());
            entries.push(hash.finalize().into());
            result.others += 1;
        }
        entries.sort_unstable();
        let mut hash = Sha256::new();
        for entry in entries {
            hash.update(entry);
        }
        result.fingerprint = hash.finalize().into();
        if started.elapsed() >= Duration::from_millis(20)
            || !unchanged(&fs::symlink_metadata(parent).ok()?)
            || migration::stamp(parent).ok()? != result.stamp
        {
            return None;
        }
        Some(result)
    }
}

/// Capture before staging adds its temporary directory entry. Failure only
/// disables the optimization; it must never refuse an otherwise valid send.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub(in crate::state) fn before_commit(path: &Path) -> Option<BeforeCommit> {
    Some(BeforeCommit {
        path: path.to_owned(),
        entries: receipt_entries(path)?,
    })
}

/// Register an original which its custodian has already committed and verified.
/// Keep the batch's atomic change allocation and witness/epoch rules. Advancing
/// coverage additionally requires a stable exact directory delta: every other
/// filename/inode is unchanged, and the prior receipt count agrees with the
/// verified ledger. Unknown deltas leave coverage for the background worker.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub(in crate::state) fn remember_committed(
    path: &Path,
    value: &Value,
    before: Option<&BeforeCommit>,
) -> bool {
    let Some(source) = receipt_source(path) else {
        return false;
    };
    let remember = (|| -> Result<()> {
        let (state, filename) = receipt_location(path, source)?;
        let catalog = Catalog::open(&state, true)?;
        let registered = (|| -> Result<()> {
            let transaction = sql(catalog.db.unchecked_transaction())?;
            let prior: (String, bool, i64, i64) = sql(catalog.db.query_row(
                "SELECT stamp,complete,indexed,errors FROM receipt_sources WHERE source=?",
                [source.key()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            ))?;
            let covered = before
                .filter(|b| {
                    b.path == path
                        && prior.1
                        && prior.3 == 0
                        && prior.0 == b.entries.stamp
                        && prior.2 == b.entries.receipts as i64
                })
                .filter(|_| catalog.coverage_verified());
            catalog.remember(source, path, value)?;
            let failed: bool = sql(catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM receipt_errors WHERE source=? AND name=?)",
                params![source.key(), filename],
                |r| r.get(0),
            ))?;
            if failed {
                return Err("Receipt registration is incomplete".into());
            }
            let mut advanced = false;
            if let (Some(before), Some(after)) =
                (covered, covered.and_then(|_| receipt_entries(path)))
            {
                if after.target
                    && after.object == before.entries.object
                    && after.others == before.entries.others
                    && after.fingerprint == before.entries.fingerprint
                    && after.receipts
                        == before.entries.receipts + usize::from(!before.entries.target)
                    && catalog.coverage_verified()
                {
                    // A legitimate mutable projection update may rotate the
                    // epoch. Preserve that rotation while restoring coverage.
                    sql(catalog.db.execute(
                        "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source=? AND errors=0",
                        params![after.stamp, source.key()],
                    ))?;
                    advanced = true;
                }
            }
            if !advanced {
                // A coarse clock may leave the same stamp despite a foreign
                // entry change. Unknown proof must clear the flag as well.
                sql(catalog.db.execute(
                    "UPDATE receipt_sources SET complete=0 WHERE source=?",
                    [source.key()],
                ))?;
            }
            sql(transaction.commit())?;
            Ok(())
        })();
        if registered.is_err() {
            // Outside the rolled-back transaction: never resurrect its old
            // complete witness after an allocation or row-write failure.
            let _ = catalog.db.execute(
                "UPDATE receipt_sources SET complete=0 WHERE source=?",
                [source.key()],
            );
        }
        registered
    })();
    // Report this advisory failure to the frontend as well: the invalidation
    // itself may have failed. Neither result can fail or replay the send.
    remember.is_ok()
}

/// Called only after the keeper exits. A complete successful registration
/// needs no forced history walk; incomplete or due verification remains work.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn request_committed_reconciliation() {
    let state = root();
    let ready = (|| -> Result<bool> {
        let catalog = Catalog::open(&state, false)?;
        let (stamp, complete, errors): (String, bool, i64) = sql(catalog.db.query_row(
            "SELECT stamp,complete,errors FROM receipt_sources WHERE source='terminal'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ))?;
        Ok(complete
            && errors == 0
            && catalog.coverage_verified()
            && !catalog.verification_due()
            && migration::stamp(&state.join(Source::Terminal.directory()))
                .is_ok_and(|now| now == stamp))
    })();
    if ready != Ok(true) {
        migration::request(&state);
    }
}

/// Preserve the authoritative atomic write and its error exactly. Index failure
/// never turns a possibly delivered send into a refusal or allows its replay.
pub(super) fn persist(path: &Path, value: &Value) -> Result<()> {
    let source = receipt_source(path);
    let before = source
        .and_then(|_| path.parent())
        .and_then(|p| migration::stamp(p).ok());
    atomic(path, &value.to_string())?;
    if let Some(source) = source {
        let remember = (|| -> Result<()> {
            let parent = path.parent().ok_or("Invalid receipt path")?;
            let (state, _) = receipt_location(path, source)?;
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
