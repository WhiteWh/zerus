//! On-demand, bounded public-history ingestion. No worker, service, discovery
//! scan or provider RPC is started here. Exact targets come from the caller's
//! current catalog; every target is re-read from its authoritative private file.
use super::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use zerus_content_search::{Index, Message, Role, Target, DEFAULT_FILE_BYTES, DOCUMENT_BYTES};
mod catalog;
mod query;
mod wire;
pub(crate) use catalog::{DiscoveryBudget, DiscoveryProgress};
pub(super) use query::read as indexed;

const ABI: &str = "content-search-index-v1";
const INPUT_BYTES: usize = 64 << 10;
const RECORD_BYTES: usize = 1 << 20;
const READ_BYTES: usize = 2 << 20;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    name: String,
    run_id: String,
    conversation_id: String,
    #[serde(default)]
    archive_id: String,
}
impl Identity {
    fn target(&self) -> Target {
        Target {
            name: self.name.clone(),
            run_id: self.run_id.clone(),
            conversation_id: self.conversation_id.clone(),
            archive_id: self.archive_id.clone(),
            agent: "unknown".into(),
            cwd: String::new(),
            archived_at: 0.0,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    abi: String,
    targets: Vec<Identity>,
}

pub(crate) struct Budget {
    pub files: usize,
    pub directory_entries: usize,
    pub records: usize,
    pub bytes: usize,
    pub deadline: Instant,
}
impl Budget {
    pub fn interactive() -> Self {
        Self {
            files: 32,
            directory_entries: 2000,
            records: 128,
            bytes: 4 << 20,
            deadline: Instant::now() + Duration::from_millis(250),
        }
    }
}
#[derive(Default, Serialize)]
pub(crate) struct Progress {
    files: usize,
    directory_entries: usize,
    locator_probes: usize,
    records: usize,
    bytes: usize,
    targets: usize,
    stopped: bool,
    errors: Vec<String>,
}
impl Progress {
    fn can_read(&self, budget: &Budget, cancel: &AtomicBool) -> bool {
        !cancel.load(Ordering::Relaxed)
            && Instant::now() < budget.deadline
            && self.records < budget.records
            && self.bytes < budget.bytes
    }
    fn available(&self, budget: &Budget, cancel: &AtomicBool) -> bool {
        self.can_read(budget, cancel) && self.files < budget.files
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Cursor {
    stamp: Option<Stamp>,
    wire: Option<wire::Locator>,
    offset: u64,
    seq: i64,
    skipping: bool,
    incomplete: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    path: PathBuf,
    device: u64,
    inode: u64,
    head_bytes: usize,
    head: String,
    tail: String,
    len: u64,
    modified: i64,
    nanos: i64,
}

pub(crate) struct Store {
    directory: PathBuf,
    index: Index,
    wire_roots: std::collections::BTreeMap<PathBuf, std::sync::Arc<wire::Snapshot>>,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        private_dir(directory)?;
        if !crate::platform::verify_private(directory, crate::platform::Private::Directory)
            .map_err(|e| e.to_string())?
        {
            return Err("Search index directory must be private and owned by this user".into());
        }
        // SQLite NOFOLLOW rejects symlinks in every path component, including
        // macOS's ordinary /var -> /private/var parent. Resolve the verified
        // directory only; keep refusing a symlink at the database itself.
        let path = crate::platform::canonical(directory)
            .map_err(|e| e.to_string())?
            .join("index.sqlite3");
        // A copied/restored database or auxiliary file must not become a new
        // public copy of message text, even if its parent is currently private.
        for name in [
            "index.sqlite3",
            "index.sqlite3-journal",
            "index.sqlite3-wal",
            "index.sqlite3-shm",
        ] {
            match crate::platform::verify_private(
                &directory.join(name),
                crate::platform::Private::File,
            ) {
                Ok(true) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                _ => {
                    return Err(
                        "Search index files must be private single-link regular files".into(),
                    )
                }
            }
        }
        let file = crate::platform::no_follow(crate::platform::private_open(
            fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true),
        ))
        .open(&path)
        .map_err(|e| e.to_string())?;
        if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
            return Err("Search index file is not private".into());
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let db = Connection::open_with_flags(&path, flags).map_err(|e| e.to_string())?;
        let index = Index::new(db, DEFAULT_FILE_BYTES).map_err(|e| e.to_string())?;
        let parent = crate::platform::canonical(directory.parent().ok_or("Invalid index path")?)
            .map_err(|e| e.to_string())?;
        let configuration = format!("{:x}", Sha256::digest(parent.to_string_lossy().as_bytes()));
        index
            .connection()
            .execute(
                "INSERT OR IGNORE INTO meta VALUES('configuration',?)",
                [&configuration],
            )
            .map_err(|e| e.to_string())?;
        let saved: String = index
            .connection()
            .query_row(
                "SELECT value FROM meta WHERE key='configuration'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if saved != configuration {
            return Err(
                "Search index belongs to another state directory; dispose and rebuild it".into(),
            );
        }
        index.connection().execute_batch("CREATE TABLE IF NOT EXISTS input_cursors(source TEXT PRIMARY KEY,cursor TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS purge_queue(source TEXT PRIMARY KEY)").map_err(|e|e.to_string())?;
        Ok(Self {
            directory: directory.to_owned(),
            index,
            wire_roots: Default::default(),
        })
    }

    /// Pure bounded-work entry point for a later G1 owner. No work is scheduled
    /// after return; cancelled jobs leave their committed input cursors intact.
    pub fn step(
        &mut self,
        state: &Path,
        targets: &[Identity],
        budget: &Budget,
        cancel: &AtomicBool,
    ) -> Result<Progress> {
        if targets.len() > 16
            || budget.files > 256
            || budget.directory_entries > 2000
            || budget.records > 512
            || budget.bytes > 8 << 20
            || budget.deadline > Instant::now() + Duration::from_secs(2)
        {
            return Err("Invalid search-index step budget".into());
        }
        if crate::platform::canonical(state).map_err(|e| e.to_string())?
            != crate::platform::canonical(self.directory.parent().ok_or("Invalid index path")?)
                .map_err(|e| e.to_string())?
        {
            return Err("Search-index step configuration changed".into());
        }
        self.wire_roots.clear();
        let mut progress = Progress::default();
        if !progress.available(budget, cancel) {
            progress.stopped = true;
            return Ok(progress);
        }
        self.cleanup()?;
        self.sweep(state, budget, &mut progress, cancel)?;
        for identity in targets {
            let target = identity.target();
            let key = target.key().map_err(|e| e.to_string())?;
            if !progress.available(budget, cancel)
                || budget.bytes - progress.bytes < RECORD_BYTES + 1
            {
                progress.stopped = true;
                break;
            }
            let record = match binding(state, identity, &mut progress, budget) {
                Ok(record) => record,
                Err(_) => {
                    self.retire(&key)?;
                    progress
                        .errors
                        .push("Source missing, replaced or unreadable; index entry retired".into());
                    continue;
                }
            };
            let target = record_target(&record);
            if let Err(error) = self.index.register(&target) {
                progress.errors.push(error.to_string());
                continue;
            }
            let encoded: Option<String> = self
                .index
                .connection()
                .query_row(
                    "SELECT cursor FROM input_cursors WHERE source=?",
                    [&key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let mut cursor: Cursor = match encoded {
                Some(encoded) => serde_json::from_str(&encoded)
                    .map_err(|_| "Invalid source cursor; dispose and rebuild the search index")?,
                None => Cursor::default(),
            };
            let before = cursor.clone();
            let mut unavailable = false;
            let result = self.transcript(&record, &key, &mut cursor, budget, &mut progress, cancel);
            let complete = match result {
                Ok(complete) => complete,
                Err(error) => {
                    cursor = before;
                    unavailable = true;
                    progress.errors.push(error.chars().take(160).collect());
                    false
                }
            };
            let active: bool = self
                .index
                .connection()
                .query_row("SELECT active FROM sources WHERE id=?", [&key], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?;
            if !active {
                progress.targets += 1;
                continue;
            }
            // A missing or unsupported provider never hides its recorded journal.
            let journal_complete =
                match self.journal(&record, &key, &mut cursor, budget, &mut progress, cancel) {
                    Ok(complete) => complete,
                    Err(_) => {
                        unavailable = true;
                        false
                    }
                };
            let mut snapshots_complete = true;
            for (field, kind, role, time) in [
                ("prompt", "UserPromptSubmit", "user", "turn_started"),
                ("last_message", "AgentMessage", "assistant", "last_event_at"),
            ] {
                if !progress.can_read(budget, cancel) {
                    snapshots_complete = false;
                    break;
                }
                let text = if field == "prompt" {
                    journal::user_prompt(&record).as_str().unwrap_or("")
                } else {
                    string(&record, field)
                };
                if !text.is_empty() {
                    let event =
                        json!({"type":kind,"source":"snapshot","at":record[time],"detail":text});
                    if self
                        .put(&key, event, role, &mut cursor, &mut progress)
                        .is_err()
                    {
                        unavailable = true;
                        snapshots_complete = false;
                    }
                }
            }
            self.index
                .connection()
                .execute(
                    "INSERT OR REPLACE INTO input_cursors VALUES(?,?)",
                    params![
                        key,
                        serde_json::to_string(&cursor).map_err(|e| e.to_string())?
                    ],
                )
                .map_err(|e| e.to_string())?;
            let _ = self.index.source_progress(
                &key,
                complete && journal_complete && snapshots_complete && !cursor.incomplete,
                unavailable || cursor.incomplete,
            );
            progress.targets += 1;
        }
        progress.stopped |= !progress.available(budget, cancel);
        Ok(progress)
    }

    fn put(
        &mut self,
        key: &str,
        mut event: Value,
        role: &str,
        cursor: &mut Cursor,
        progress: &mut Progress,
    ) -> Result<()> {
        progress.records += 1;
        let text = string(&event, "detail");
        if text.is_empty() {
            return Ok(());
        }
        if text.len() > DOCUMENT_BYTES {
            cursor.incomplete = true;
            return Ok(());
        }
        let id = if string(&event, "message_id").is_empty() {
            format!("indexed-{:x}", Sha256::digest(format!("{key}:{event}")))
        } else {
            string(&event, "message_id").to_owned()
        };
        // Only these typed public fields reach the persistent copy.
        event["message_id"] = json!(id);
        let message = Message {
            id,
            at: event["at"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .unwrap_or(0.0),
            role: match role {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::Event,
            },
            kind: string(&event, "type").to_owned(),
            source: string(&event, "source").to_owned(),
            seq: event["seq"].as_u64().unwrap_or(0),
            text: string(&event, "detail").to_owned(),
        };
        if let Err(error) = self.index.ingest(key, &[message]) {
            // SQLITE_FULL must not advance this source's input cursor. Eviction
            // is separately bounded and visible; a later step retries the input.
            if matches!(error.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(code,_)) if code.code==rusqlite::ErrorCode::DiskFull)
            {
                let _ = self.index.evict();
            }
            return Err(error.to_string());
        }
        Ok(())
    }

    fn transcript(
        &mut self,
        record: &Value,
        key: &str,
        cursor: &mut Cursor,
        budget: &Budget,
        progress: &mut Progress,
        cancel: &AtomicBool,
    ) -> Result<bool> {
        if !progress.available(budget, cancel) {
            return Ok(false);
        }
        if record["backend"] == "dsh" {
            return Err(
                "Native history needs legacy search until its indexed adapter is available".into(),
            );
        }
        let path = if record["agent"] == "kimi" {
            let Some((path, locator)) = self.locate_wire(record, budget, progress, cancel)? else {
                return Ok(false);
            };
            cursor.wire = Some(locator);
            path
        } else {
            PathBuf::from(string(record, "transcript"))
        };
        if !path.is_absolute()
            || (record["agent"] != "kimi"
                && !path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .contains(string(record, "conversation_id"))
                }))
        {
            return Err("Exact conversation transcript unavailable".into());
        }
        progress.files += 1;
        let mut file = crate::platform::no_follow(fs::OpenOptions::new().read(true))
            .open(&path)
            .map_err(|_| "Transcript unavailable")?;
        let (identity, metadata) =
            crate::platform::FileIdentity::with_metadata(&file).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Transcript is not a regular file".into());
        }
        let head_bytes = cursor
            .stamp
            .as_ref()
            .map(|s| s.head_bytes)
            .unwrap_or(metadata.len().min(256) as usize);
        let head = sample(&mut file, 0, head_bytes, progress, budget)?;
        let tail = sample(
            &mut file,
            cursor.offset.saturating_sub(256),
            cursor.offset.min(256) as usize,
            progress,
            budget,
        )?;
        if cursor.stamp.as_ref().is_some_and(|s| {
            s.path != path
                || s.device != identity.device
                || s.inode != identity.inode
                || cursor.offset > metadata.len()
                || s.head != head
                || s.tail != tail
                || (s.len == metadata.len()
                    && (s.modified != identity.modified || s.nanos != identity.modified_nanos))
        }) {
            self.retire(key)?;
            return Err(
                "Transcript changed identity; index source retired, use legacy search".into(),
            );
        }
        file.seek(SeekFrom::Start(cursor.offset))
            .map_err(|e| e.to_string())?;
        // Reserve the final fingerprint read before committing an input cursor.
        let count = READ_BYTES.min(budget.bytes.saturating_sub(progress.bytes + 256));
        if count == 0 {
            return Ok(false);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(count as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        progress.bytes += bytes.len();
        for line in bytes.split_inclusive(|c| *c == b'\n') {
            if cancel.load(Ordering::Relaxed)
                || Instant::now() >= budget.deadline
                || progress.records >= budget.records
            {
                break;
            }
            if !line.ends_with(b"\n") {
                if line.len() == READ_BYTES {
                    cursor.skipping = true;
                    cursor.incomplete = true;
                    cursor.offset += line.len() as u64;
                }
                break;
            }
            if cursor.skipping {
                cursor.skipping = false;
                cursor.offset += line.len() as u64;
                continue;
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                progress.records += 1;
                cursor.offset += line.len() as u64;
                continue;
            }
            match serde_json::from_slice::<Value>(line) {
                Ok(value) => {
                    if (record["agent"] == "claude"
                        && value["sessionId"].is_string()
                        && value["sessionId"] != record["conversation_id"])
                        || (record["agent"] == "codex"
                            && value["type"] == "session_meta"
                            && value["payload"]["id"] != record["conversation_id"])
                    {
                        self.retire(key)?;
                        return Err("Transcript conversation changed; source retired".into());
                    }
                    if let Some((event, role)) =
                        search::provider_event(string(record, "agent"), &value)
                    {
                        self.put(key, event, role, cursor, progress)?;
                    } else {
                        progress.records += 1;
                    }
                }
                Err(_) => {
                    cursor.incomplete = true;
                    progress.records += 1;
                }
            }
            cursor.offset += line.len() as u64;
        }
        let tail = sample(
            &mut file,
            cursor.offset.saturating_sub(256),
            cursor.offset.min(256) as usize,
            progress,
            budget,
        )?;
        cursor.stamp = Some(Stamp {
            path,
            device: identity.device,
            inode: identity.inode,
            head_bytes,
            head,
            tail,
            len: metadata.len(),
            modified: identity.modified,
            nanos: identity.modified_nanos,
        });
        Ok(cursor.offset == metadata.len())
    }

    fn journal(
        &mut self,
        record: &Value,
        key: &str,
        cursor: &mut Cursor,
        budget: &Budget,
        progress: &mut Progress,
        cancel: &AtomicBool,
    ) -> Result<bool> {
        // NOFOLLOW below: resolve the state directory, never the database name.
        let state = self
            .directory
            .parent()
            .ok_or("Invalid search state directory")?;
        let path = match crate::platform::canonical(state) {
            Ok(state) => state.join("events.sqlite3"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(e.to_string()),
        };
        if !path.exists() {
            return Ok(true);
        }
        if !progress.available(budget, cancel) {
            return Ok(false);
        }
        progress.files += 1;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| e.to_string())?;
        db.busy_timeout(Duration::from_millis(25))
            .map_err(|e| e.to_string())?;
        let deadline = budget.deadline;
        db.progress_handler(1000, Some(move || Instant::now() >= deadline));
        let name = if string(record, "journal_name").is_empty() {
            string(record, "name")
        } else {
            string(record, "journal_name")
        };
        let mut statement=db.prepare(&format!("SELECT seq,substr(CAST(payload AS BLOB),1,?6),length(CAST(payload AS BLOB)) FROM events WHERE {} AND seq>?4 ORDER BY seq LIMIT ?5",journal::TIMELINE)).map_err(|e|e.to_string())?;
        let mut rows = statement
            .query(params![
                name,
                record["conversation_id"].as_str(),
                clear_context::earlier_conversations(record).to_string(),
                cursor.seq,
                budget.records - progress.records + 1,
                RECORD_BYTES.min(budget.bytes - progress.bytes)
            ])
            .map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if !progress.can_read(budget, cancel) {
                return Ok(false);
            }
            let bytes: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            let size: usize = row.get(2).map_err(|e| e.to_string())?;
            if bytes.len() > budget.bytes - progress.bytes {
                return Ok(false);
            }
            progress.bytes += bytes.len();
            let seq: i64 = row.get(0).map_err(|e| e.to_string())?;
            if size > bytes.len() {
                if size <= RECORD_BYTES {
                    return Ok(false);
                }
                cursor.incomplete = true;
                progress.records += 1;
                cursor.seq = seq;
                continue;
            }
            let Ok(mut event) = serde_json::from_slice::<Value>(&bytes) else {
                cursor.incomplete = true;
                cursor.seq = seq;
                progress.records += 1;
                continue;
            };
            if event.is_object() {
                journal::mark_system_prompt(record, &mut event, "");
                let role = search::journal_role(&event);
                event["seq"] = json!(seq);
                event["source"] = json!("journal");
                self.put(key, event, role, cursor, progress)?;
            } else {
                cursor.incomplete = true;
                progress.records += 1;
            }
            cursor.seq = seq;
        }
        Ok(true)
    }

    pub fn status(&self) -> Result<Value> {
        let coverage = self.index.status().map_err(|e| e.to_string())?;
        Ok(
            json!({"abi":ABI,"coverage":coverage,"disposable":true,"worker":"on_demand","catalog":self.catalog_status()?}),
        )
    }

    fn retire(&self, key: &str) -> Result<()> {
        self.index.retire(key).map_err(|e| e.to_string())?;
        self.mark_retired_references(key)?;
        self.index
            .connection()
            .execute("DELETE FROM input_cursors WHERE source=?", [key])
            .map_err(|e| e.to_string())?;
        self.index
            .connection()
            .execute("INSERT OR IGNORE INTO purge_queue VALUES(?)", [key])
            .map_err(|e| e.to_string())?;
        self.cleanup()
    }

    fn cleanup(&self) -> Result<()> {
        let key: Option<String> = self
            .index
            .connection()
            .query_row(
                "SELECT source FROM purge_queue ORDER BY source LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(key) = key {
            // A missing source has no payload; otherwise the index enforces a
            // <=32-document secure purge. Keep the queue until its last row.
            let active: Option<bool> = self
                .index
                .connection()
                .query_row("SELECT active FROM sources WHERE id=?", [&key], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| e.to_string())?;
            if active == Some(false) {
                self.index.purge(&key).map_err(|e| e.to_string())?;
            }
            let remains: bool = self
                .index
                .connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE source=? LIMIT 1)",
                    [&key],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !remains {
                self.index
                    .connection()
                    .execute("DELETE FROM purge_queue WHERE source=?", [&key])
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn sweep(
        &self,
        state: &Path,
        budget: &Budget,
        progress: &mut Progress,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let last: String = self
            .index
            .connection()
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='source_sweep'),'')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let entries: Vec<(String, String)> = self
            .index
            .connection()
            .prepare("SELECT id,target FROM sources WHERE active=1 AND id>? ORDER BY id LIMIT 4")
            .map_err(|e| e.to_string())?
            .query_map([&last], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())?;
        let mut next = if entries.is_empty() {
            String::new()
        } else {
            last
        };
        for (key, target) in entries {
            if !progress.available(budget, cancel)
                || budget.bytes - progress.bytes < RECORD_BYTES + 1
            {
                break;
            }
            let target: Target = serde_json::from_str(&target)
                .map_err(|_| "Invalid indexed target; dispose and rebuild the index")?;
            let identity = Identity {
                name: target.name,
                run_id: target.run_id,
                conversation_id: target.conversation_id,
                archive_id: target.archive_id,
            };
            if binding(state, &identity, progress, budget).is_err() {
                self.retire(&key)?;
            }
            next = key;
        }
        self.index
            .connection()
            .execute(
                "INSERT OR REPLACE INTO meta VALUES('source_sweep',?)",
                [next],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn record_target(record: &Value) -> Target {
    Target {
        name: string(record, "name").into(),
        run_id: string(record, "run_id").into(),
        conversation_id: string(record, "conversation_id").into(),
        archive_id: string(record, "archive_id").into(),
        agent: string(record, "agent").into(),
        cwd: string(record, "cwd").into(),
        archived_at: record["archived_at"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .unwrap_or(0.0),
    }
}
fn binding(
    state: &Path,
    identity: &Identity,
    progress: &mut Progress,
    budget: &Budget,
) -> Result<Value> {
    let path = if identity.archive_id.is_empty() {
        let basename = format!("{:x}.json", Sha256::digest(identity.name.as_bytes()));
        if state.join("name_routes").join(&basename).exists() {
            state.join("bindings").join(basename)
        } else {
            state.join(basename)
        }
    } else {
        uuid::Uuid::parse_str(&identity.archive_id).map_err(|_| "Invalid archive identity")?;
        state
            .join("archives")
            .join(format!("{}.json", identity.archive_id))
    };
    progress.files += 1;
    let file = crate::platform::no_follow(fs::OpenOptions::new().read(true))
        .open(path)
        .map_err(|_| "Binding unavailable")?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Binding is not a regular file".into());
    }
    let cap = (RECORD_BYTES + 1).min(budget.bytes - progress.bytes);
    let mut bytes = Vec::new();
    file.take(cap as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    progress.bytes += bytes.len();
    if bytes.len() > RECORD_BYTES {
        return Err("Binding exceeds the index metadata budget".into());
    }
    let mut record: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Binding is incomplete or invalid")?;
    if !identity.archive_id.is_empty() {
        rename::normalize_archive(&mut record)?;
    }
    if record["version"] != 1
        || record["rename_shadow"] == true
        || record["name"] != identity.name
        || record["run_id"] != identity.run_id
        || record["conversation_id"] != identity.conversation_id
        || string(&record, "archive_id") != identity.archive_id
    {
        return Err("Binding identity changed".into());
    }
    Ok(record)
}
fn sample(
    file: &mut fs::File,
    start: u64,
    len: usize,
    progress: &mut Progress,
    budget: &Budget,
) -> Result<String> {
    if len > budget.bytes.saturating_sub(progress.bytes) {
        return Err("Search-index byte budget reached".into());
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    progress.bytes += len;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn dispatch(args: &[String]) -> Option<Result<i32>> {
    let mode = match args {
        [mode]
            if [
                "--index-status",
                "--index-step",
                "--index-discover",
                "--index-catchup",
            ]
            .contains(&mode.as_str()) =>
        {
            mode
        }
        _ => return None,
    };
    Some((|| {
        let directory = root().join("content-search-v1");
        if mode == "--index-status" && !directory.exists() {
            println!(
                "{}",
                json!({"abi":ABI,"coverage":{"state":"cold","file_bytes":0},"worker":"on_demand","disposable":true})
            );
            return Ok(0);
        }
        let mut store = Store::open(&directory)?;
        if mode == "--index-discover" || mode == "--index-catchup" {
            lower_own_priority();
            let progress: DiscoveryProgress = store.discover(
                &root(),
                &DiscoveryBudget::interactive(),
                &AtomicBool::new(false),
            )?;
            let ingestion = if mode == "--index-catchup" {
                Some(store.consume(&root(), &Budget::interactive(), &AtomicBool::new(false))?)
            } else {
                None
            };
            println!(
                "{}",
                json!({"abi":ABI,"discovery":progress,"catchup":ingestion,"status":store.status()?})
            );
            return Ok(0);
        }
        if mode == "--index-step" {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((INPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > INPUT_BYTES {
                return Err("Search-index request exceeds its input budget".into());
            }
            let request: Request =
                serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(|_| "Invalid search-index request")?;
            if request.abi != ABI {
                return Err("Unsupported search-index ABI".into());
            }
            // Only this short CLI process is reprioritized. An eventual G1
            // owner sets priority on its owned worker, not the service actor.
            lower_own_priority();
            let progress = store.step(
                &root(),
                &request.targets,
                &Budget::interactive(),
                &AtomicBool::new(false),
            )?;
            println!(
                "{}",
                json!({"abi":ABI,"progress":progress,"status":store.status()?})
            );
        } else {
            println!("{}", store.status()?);
        }
        Ok(0)
    })())
}

fn lower_own_priority() {
    #[cfg(unix)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn indexed_search_opens_and_discovers_a_windows_fixture() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::write(state.join(format!("{:064x}.json", 1)), "{}").unwrap();
        let store = Store::open(&state.join("content-search-v1")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut progress = DiscoveryProgress::default();
        for _ in 0..128 {
            let budget = DiscoveryBudget::interactive();
            progress = store
                .discover(&state, &budget, &AtomicBool::new(false))
                .unwrap();
            assert!(Instant::now() < deadline, "fixture discovery timed out");
            assert!(progress.entries <= budget.entries);
            if progress.complete {
                break;
            }
        }
        assert!(progress.complete);
        assert_eq!(progress.staged, 1);
        assert_eq!(store.status().unwrap()["catalog"]["queued_paths"], 1);
    }
}
