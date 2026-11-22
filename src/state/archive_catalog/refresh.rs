//! Disposable archive catalog maintenance. Native JSON is read only for new,
//! changed or damaged cache rows; requests never perform a bootstrap walk.
use super::*;
use fs2::FileExt;
use rusqlite::{OpenFlags, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::ReadDir;
use std::time::{Duration, Instant};

pub(super) const INCOMPLETE: &str =
    "Archive catalog coverage is incomplete; metadata is updating; retry the archive";
const AUX: &str = "archive-catalog-v2";
const DATABASE: &str = "archive-catalog-v1.sqlite3";
const FILES: usize = 100_000;
const CACHE_BYTES: usize = 32 << 20;
const TEXT: usize = storage::ID_RECORD_BYTES as usize;
const BATCH: usize = 64;
const SLICE: Duration = Duration::from_millis(25);

mod writer;
pub(super) use writer::{catalog as writer_catalog, published, removed};

fn fail(error: impl std::fmt::Display) -> String {
    format!("{INCOMPLETE}: {error}")
}
pub(super) fn directory() -> Result<String> {
    let path = root().join("archives");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok("absent".into()),
        Err(error) => Err(fail(error)),
        Ok(_) => archive_references::directory_stamp(&path),
    }
}
fn meta(db: &Connection, key: &str) -> Result<String> {
    sql(db.query_row(
        "SELECT COALESCE((SELECT substr(value,1,2049) FROM catalog_meta WHERE key=?),'')",
        [key],
        |row| row.get(0),
    ))
}
fn put_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
    sql(db.execute(
        "INSERT OR REPLACE INTO catalog_meta VALUES(?,?)",
        params![key, value],
    ))
    .map(drop)
}
fn schema(db: &Connection) -> Result<()> {
    sql(db.execute_batch("PRAGMA temp_store=MEMORY;
        CREATE TABLE IF NOT EXISTS catalog_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS archive_rows(id TEXT PRIMARY KEY,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,at REAL NOT NULL,search TEXT NOT NULL,stamp TEXT NOT NULL,seen TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS archive_by_date ON archive_rows(at DESC,id DESC);
        CREATE INDEX IF NOT EXISTS archive_identity ON archive_rows(name,run,conversation);
        CREATE TABLE IF NOT EXISTS archive_files(id TEXT PRIMARY KEY,stamp TEXT NOT NULL,row_digest TEXT NOT NULL,reference_digest TEXT NOT NULL,search_ok INTEGER NOT NULL,reference_ok INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS archive_identities(id TEXT PRIMARY KEY,stamp TEXT NOT NULL,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,digest TEXT NOT NULL,valid INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS archive_native_identity ON archive_identities(name,run,conversation);"))?;
    archive_references::schema(db)?;
    sql(db.execute(
        "INSERT OR IGNORE INTO catalog_meta VALUES('id',?)",
        [uuid::Uuid::new_v4().to_string()],
    ))?;
    Ok(())
}
fn open(writable: bool, until: Instant) -> Result<Connection> {
    let path = root().join(DATABASE);
    match fs::symlink_metadata(&path) {
        Err(error) if writable && error.kind() == io::ErrorKind::NotFound => {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).read(true).write(true);
            crate::platform::private_open(&mut options);
            crate::platform::no_follow(&mut options);
            match options.open(&path) {
                Ok(_file) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&path).map_err(fail)?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        return Err(INCOMPLETE.into());
                    }
                }
                Err(error) => return Err(fail(error)),
            }
        }
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => (),
        _ => return Err(INCOMPLETE.into()),
    }
    let access = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = sql(Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ))?;
    sql(db.busy_timeout(Duration::ZERO))?;
    db.progress_handler(1000, Some(move || Instant::now() >= until));
    if writable {
        schema(&db)?;
    }
    Ok(db)
}
pub(super) fn existing() -> Result<Connection> {
    open(false, Instant::now() + Duration::from_secs(2))
}
pub(super) fn ready(db: &Connection) -> Result<bool> {
    Ok(meta(db, "archive_scan_complete_v2")? == "1"
        && meta(db, "archive_directory_v2")? == directory()?)
}
pub(super) fn search_complete(db: &Connection) -> Result<bool> {
    Ok(ready(db)? && meta(db, "archive_search_errors_v2")? == "0")
}
fn empty(directory: &str) -> Result<Connection> {
    let db = sql(Connection::open_in_memory())?;
    schema(&db)?;
    put_meta(&db, "archive_scan_complete_v2", "1")?;
    put_meta(&db, "archive_directory_v2", directory)?;
    put_meta(&db, "archive_search_errors_v2", "0")?;
    put_meta(&db, "archive_reference_errors_v2", "0")?;
    put_meta(&db, "archive_identity_errors_v2", "0")?;
    Ok(db)
}
pub(super) fn catalog() -> Result<Connection> {
    read_catalog(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn catalog_without_request() -> Result<Connection> {
    read_catalog(false)
}

fn read_catalog(schedule: bool) -> Result<Connection> {
    let source = directory()?;
    if source == "absent" {
        return empty(&source);
    }
    let result = open(false, Instant::now() + Duration::from_secs(2)).and_then(|db| {
        if ready(&db)? {
            Ok(db)
        } else {
            Err(INCOMPLETE.into())
        }
    });
    if result.is_err() {
        let mut entries = fs::read_dir(root().join("archives")).map_err(fail)?;
        if entries.next().transpose().map_err(fail)?.is_none() && directory()? == source {
            return empty(&source);
        }
    }
    if schedule {
        request();
    }
    result
}

fn guard(name: &str) -> Result<Option<storage::Guard>> {
    // A late detached helper must not recreate a removed state root. Acquire
    // its auxiliary directory through the original held native parent.
    let state = crate::state::repositories::directory::Directory::open(
        &crate::platform::canonical(root()).map_err(fail)?,
    )?;
    let directory = state.ensure_child(AUX)?;
    let file = directory.open_private_lock(name)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(storage::Guard::held(file))),
        Err(error) if crate::platform::lock_contended(&error) => Ok(None),
        Err(error) => Err(fail(error)),
    }
}

#[cfg(test)]
pub(super) fn request() {}
#[cfg(not(test))]
pub(super) fn request() {
    request_worker(false);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn request_for_team() {
    request_worker(true);
}

#[cfg(any(not(test), target_os = "linux", target_os = "macos"))]
fn request_worker(team: bool) {
    let launch = (|| -> Result<()> {
        let Some(_launch) = guard("launch.lock")? else {
            return Ok(());
        };
        let Some(probe) = guard("worker.lock")? else {
            return Ok(());
        };
        let (leaf, interval) = if team {
            ("teams-attempt.json", 300.0)
        } else {
            ("attempt.json", 5.0)
        };
        let path = root().join(AUX).join(leaf);
        let current = now();
        let raw = match labels::read_file(&path, 4096) {
            Ok(raw) => raw,
            // Recover an oversized regular reservation without reading it.
            Err(_) if team && fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.is_file() && meta.len() > 4096) => None,
            Err(error) => return Err(fail(error)),
        };
        let at = raw.as_deref()
            .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
            .and_then(|row| row["at"].as_f64());
        // A malformed or future-dated reservation admits one replacement
        // under both locks. Reserve it before spawning, including on failure.
        if at.is_some_and(|at| (0.0..interval).contains(&(current - at))) {
            return Ok(());
        }
        atomic(&path, &json!({"at":current}).to_string())?;
        // The child acquires the same worker exclusion as the ordinary
        // catalog refresh. Release the probe before it can try to acquire it.
        drop(probe);
        launch_worker(team)?;
        Ok(())
    })();
    if team {
        if let Err(error) = launch {
            eprintln!("hgs: could not request Teams archive catalog repair: {error}");
        }
    }
}

#[cfg(any(not(test), target_os = "linux", target_os = "macos"))]
fn worker_args(team: bool) -> Vec<&'static str> {
    let mut args = vec!["__state", "archive-catalog-worker"];
    if team {
        args.push("--teams-repair");
    }
    args
}

#[cfg(not(test))]
fn launch_worker(team: bool) -> Result<()> {
    spawn_worker(&worker_args(team))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
thread_local! {
    pub(super) static TEST_WORKER: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn launch_worker(team: bool) -> Result<()> {
    if let Some(test) = TEST_WORKER.with(std::cell::Cell::get) {
        spawn_worker(&worker_args(team), test)?;
    }
    Ok(())
}

#[cfg(any(not(test), target_os = "linux", target_os = "macos"))]
fn spawn_worker(args: &[&str], #[cfg(test)] test: &str) -> Result<()> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe().map_err(fail)?);
    for (key, _) in
        std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("HGS_"))
    {
        command.env_remove(key);
    }
    #[cfg(test)]
    command
        .args(["--exact", test, "--ignored", "--test-threads=1"])
        .env("TEST_ARCHIVE_WORKER_ARGV", json!(args).to_string());
    #[cfg(not(test))]
    command.args(args);
    command
        .env(
            "HGS_STATE_DIR",
            crate::platform::canonical(root()).map_err(fail)?,
        )
        .current_dir(root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = crate::platform::spawn_detached(&mut command).map_err(fail)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[derive(Clone, Serialize)]
struct Row {
    id: String,
    name: String,
    run: String,
    conversation: String,
    at: f64,
    search: String,
    stamp: String,
}
impl Row {
    fn captured(id: &str, record: Value, stamp: String, raw_name: &str) -> Result<Self> {
        let record = archive::validated_record(raw_name, id, record)?;
        let name = string(&record, "name").to_owned();
        let at = record["archived_at"]
            .as_f64()
            .filter(|at| at.is_finite() && *at >= 0.0 && *at < MAX_DATE as f64)
            .unwrap_or(0.0);
        let at = if at == 0.0 { 0.0 } else { at };
        let search = super::search_text(&record);
        Ok(Self {
            id: id.into(),
            name,
            run: string(&record, "run_id").into(),
            conversation: string(&record, "conversation_id").into(),
            at,
            search,
            stamp,
        })
    }
    fn digest(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(fail)?)
        ))
    }
    fn store(&self, db: &Connection) -> Result<()> {
        sql(db.execute(
            "INSERT OR REPLACE INTO archive_rows VALUES(?,?,?,?,?,?,?,?)",
            params![
                self.id,
                self.name,
                self.run,
                self.conversation,
                self.at,
                self.search,
                self.stamp,
                "v2"
            ],
        ))?;
        Ok(())
    }
}
/// Independent of search omission and folder-projection bounds. Complete
/// native identity coverage is required before an indexed miss means absence.
#[derive(Serialize)]
struct NativeIdentity {
    name: String,
    run: String,
    conversation: String,
}
impl NativeIdentity {
    fn captured(id: &str, record: &Value) -> Result<Self> {
        let name = string(record, "name");
        archive::validate_record(name, id, record)?;
        let value = Self {
            name: name.into(),
            run: string(record, "run_id").into(),
            conversation: string(record, "conversation_id").into(),
        };
        value.check()?;
        Ok(value)
    }
    fn check(&self) -> Result<()> {
        if [&self.name, &self.run, &self.conversation]
            .iter()
            .any(|value| value.len() > 4096)
            || serde_json::to_vec(self).map_err(fail)?.len() > 8192
        {
            return Err(INCOMPLETE.into());
        }
        Ok(())
    }
    fn digest(&self, id: &str, stamp: &str) -> Result<String> {
        self.check()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(id, stamp, self)).map_err(fail)?)
        ))
    }
    fn store(value: Result<Self>, db: &Connection, id: &str, stamp: &str) -> Result<bool> {
        let (value, hash, valid) = match value {
            Ok(value) => {
                let hash = value.digest(id, stamp)?;
                (value, hash, true)
            }
            Err(_) => (
                Self {
                    name: String::new(),
                    run: String::new(),
                    conversation: String::new(),
                },
                String::new(),
                false,
            ),
        };
        sql(db.execute(
            "INSERT OR REPLACE INTO archive_identities VALUES(?,?,?,?,?,?,?)",
            params![
                id,
                stamp,
                value.name,
                value.run,
                value.conversation,
                hash,
                valid
            ],
        ))?;
        Ok(valid)
    }
}
fn native_identity(db: &Connection, id: &str) -> Result<(String, bool, bool)> {
    let mut query=sql(db.prepare_cached("SELECT substr(stamp,1,1025),substr(name,1,4097),substr(run,1,4097),substr(conversation,1,4097),substr(digest,1,65),valid FROM archive_identities WHERE id=?"))?;
    let value = sql(query
        .query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                NativeIdentity {
                    name: r.get(1)?,
                    run: r.get(2)?,
                    conversation: r.get(3)?,
                },
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })
        .optional())?;
    let Some((stamp, value, hash, valid)) = value else {
        return Ok((String::new(), false, false));
    };
    let sound = if valid {
        value.digest(id, &stamp).ok().as_deref() == Some(hash.as_str())
    } else {
        value.name.is_empty()
            && value.run.is_empty()
            && value.conversation.is_empty()
            && hash.is_empty()
    };
    Ok((stamp, sound, valid))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn has_identity(db: &Connection, id: &str, stamp: &str) -> bool {
    native_identity(db, id)
        .is_ok_and(|(indexed, sound, valid)| indexed == stamp && sound && valid)
}

#[derive(Clone)]
struct Cached {
    stamp: String,
    valid: bool,
    identity_ok: bool,
    search_ok: bool,
    reference_ok: bool,
}
fn cached(db: &Connection, id: &str) -> Result<Cached> {
    // The cache is streamed in ID order, so these indexed joins share nearby
    // pages rather than reopening reference projections in ReadDir order.
    let mut query = sql(db.prepare_cached("SELECT substr(f.stamp,1,1025),substr(f.row_digest,1,65),substr(f.reference_digest,1,65),f.search_ok,f.reference_ok,
        substr(r.name,1,?2),substr(r.run,1,?2),substr(r.conversation,1,?2),r.at,substr(r.search,1,?3),substr(r.stamp,1,1025),
        substr(a.stamp,1,1025),substr(a.metadata,1,65537),substr(a.digest,1,65),
        (SELECT substr(folder,1,65537) FROM archive_reference_paths WHERE id=?1 ORDER BY folder LIMIT 1),
        (SELECT substr(folder,1,65537) FROM archive_reference_paths WHERE id=?1 ORDER BY folder LIMIT 1 OFFSET 1),
        (SELECT count(*) FROM (SELECT 1 FROM archive_reference_paths WHERE id=?1 LIMIT 3))
        FROM archive_files f LEFT JOIN archive_rows r ON r.id=f.id LEFT JOIN archive_references a ON a.id=f.id WHERE f.id=?1"))?;
    let value = sql(query
        .query_row(params![id, TEXT + 1, 4 * TEXT + 1], |r| {
            let stamp: String = r.get(0)?;
            let row_hash: String = r.get(1)?;
            let reference_hash: String = r.get(2)?;
            let search_ok: bool = r.get(3)?;
            let reference_ok: bool = r.get(4)?;
            let row = match (
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ) {
                (
                    Some(name),
                    Some(run),
                    Some(conversation),
                    Some(at),
                    Some(search),
                    Some(stamp),
                ) => Some(Row {
                    id: id.into(),
                    name,
                    run,
                    conversation,
                    at,
                    search,
                    stamp,
                }),
                _ => None,
            };
            let refs = (
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
            );
            let paths = [
                r.get::<_, Option<String>>(14)?,
                r.get::<_, Option<String>>(15)?,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let paths_count: usize = r.get(16)?;
            Ok((
                stamp,
                row_hash,
                reference_hash,
                search_ok,
                reference_ok,
                row,
                refs,
                paths,
                paths_count,
            ))
        })
        .optional())?;
    let Some((
        stamp,
        row_hash,
        reference_hash,
        search_ok,
        reference_ok,
        row,
        refs,
        paths,
        paths_count,
    )) = value
    else {
        return Ok(Cached {
            stamp: String::new(),
            valid: false,
            identity_ok: false,
            search_ok: false,
            reference_ok: false,
        });
    };
    let search_valid = if search_ok {
        row.as_ref().is_some_and(|row| {
            row.stamp == stamp && row.digest().ok().as_deref() == Some(row_hash.as_str())
        })
    } else {
        row.is_none()
    };
    let reference_valid = if reference_ok {
        match refs {
            (Some(actual), Some(raw), Some(hash)) => {
                actual == stamp
                    && hash == reference_hash
                    && paths_count == paths.len()
                    && archive_references::cached_projection(id, &actual, &raw, &hash, &paths)
            }
            _ => false,
        }
    } else {
        true
    };
    let (native_stamp, native_sound, identity_ok) = native_identity(db, id)?;
    Ok(Cached {
        valid: search_valid && reference_valid && native_sound && native_stamp == stamp,
        stamp,
        identity_ok,
        search_ok,
        reference_ok,
    })
}

fn keys(db: &Connection, after: &str) -> Result<Vec<String>> {
    // Bound each index separately, independently of SQLite's compound-query
    // plan. Loading the cache must make progress within the helper budget.
    let mut keys = BTreeSet::new();
    for table in [
        "archive_files",
        "archive_rows",
        "archive_references",
        "archive_identities",
    ] {
        let mut statement = sql(db.prepare_cached(&format!(
            "SELECT substr(id,1,65) FROM {table} WHERE id>? ORDER BY id LIMIT ?"
        )))?;
        let rows = sql(statement.query_map(params![after, BATCH], |row| row.get::<_, String>(0)))?;
        for key in rows {
            let key = sql(key)?;
            if key.len() > 64 {
                return Err(INCOMPLETE.into());
            }
            keys.insert(key);
        }
    }
    Ok(keys.into_iter().take(BATCH).collect())
}

fn invalidate(db: &Connection, invalidated: &mut bool) -> Result<()> {
    if !*invalidated {
        put_meta(db, "archive_scan_complete_v2", "0")?;
        *invalidated = true;
    }
    Ok(())
}

#[derive(PartialEq)]
enum Phase {
    Load,
    Walk,
    Prune,
    Seal,
    Complete,
}
pub(super) enum Step {
    Progress,
    Complete,
    Restart,
}
/// An owned directory iterator survives every slice. Restart retains all
/// committed per-file entries; it never publishes a partially visited roster.
pub(super) struct Builder {
    db: Connection,
    phase: Phase,
    after: String,
    cache: BTreeMap<String, Cached>,
    cache_bytes: usize,
    entries: Option<ReadDir>,
    pending: Option<PathBuf>,
    seen: BTreeSet<String>,
    remove: VecDeque<String>,
    started: String,
    invalidated: bool,
    examined: usize,
    search_errors: usize,
    reference_errors: usize,
    identity_errors: usize,
    search_rows: usize,
    read_bytes: u64,
    data_version: i64,
}
impl Builder {
    pub(super) fn new(until: Instant) -> Result<Self> {
        let metadata = fs::symlink_metadata(root()).map_err(fail)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(INCOMPLETE.into());
        }
        let db = open(true, until)?;
        let data_version = sql(db.query_row("PRAGMA data_version", [], |row| row.get(0)))?;
        Ok(Self {
            db,
            phase: Phase::Load,
            after: String::new(),
            cache: BTreeMap::new(),
            cache_bytes: 0,
            entries: None,
            pending: None,
            seen: BTreeSet::new(),
            remove: VecDeque::new(),
            started: String::new(),
            invalidated: false,
            examined: 0,
            search_errors: 0,
            reference_errors: 0,
            identity_errors: 0,
            search_rows: 0,
            read_bytes: 0,
            data_version,
        })
    }
    pub(super) fn bytes(&self) -> u64 {
        self.read_bytes
    }
    pub(super) fn step(&mut self) -> Result<Step> {
        let external: i64 = sql(self
            .db
            .query_row("PRAGMA data_version", [], |row| row.get(0)))?;
        if external != self.data_version {
            return Ok(Step::Restart);
        }
        let until = Instant::now() + SLICE;
        if self.phase == Phase::Load {
            let batch = keys(&self.db, &self.after)?;
            let mut processed = 0;
            for id in &batch {
                if processed > 0 && Instant::now() >= until {
                    break;
                }
                let entry = cached(&self.db, id)?;
                self.cache_bytes += id.len() + entry.stamp.len() + 128;
                self.cache.insert(id.clone(), entry);
                self.after = id.clone();
                processed += 1;
                if self.cache.len() > FILES || self.cache_bytes > CACHE_BYTES {
                    return Err(INCOMPLETE.into());
                }
            }
            if batch.len() < BATCH && processed == batch.len() {
                self.started = directory()?;
                self.entries = if self.started == "absent" {
                    None
                } else {
                    Some(fs::read_dir(root().join("archives")).map_err(fail)?)
                };
                self.phase = Phase::Walk;
            }
            return Ok(Step::Progress);
        }
        if self.phase == Phase::Walk {
            let tx = sql(self.db.unchecked_transaction())?;
            let mut processed = 0;
            let mut bytes = 0;
            loop {
                if processed >= BATCH
                    || bytes >= 32 << 20
                    || (processed > 0 && Instant::now() >= until)
                {
                    break;
                }
                let path = if let Some(path) = self.pending.take() {
                    Some(path)
                } else {
                    self.entries
                        .as_mut()
                        .and_then(Iterator::next)
                        .transpose()
                        .map_err(fail)?
                        .map(|entry| entry.path())
                };
                let Some(path) = path else {
                    if directory()? != self.started {
                        return Ok(Step::Restart);
                    }
                    self.remove = self
                        .cache
                        .keys()
                        .filter(|id| !self.seen.contains(*id))
                        .cloned()
                        .collect();
                    self.phase = Phase::Prune;
                    break;
                };
                self.examined += 1;
                if self.examined > FILES {
                    return Err(INCOMPLETE.into());
                }
                processed += 1;
                if path.extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                let Some(id) = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                    .map(str::to_owned)
                else {
                    self.search_errors += 1;
                    self.reference_errors += 1;
                    invalidate(&tx, &mut self.invalidated)?;
                    continue;
                };
                let actual = archive_references::current_stamp(&path);
                let prior = self.cache.get(&id);
                if actual.is_err()
                    && prior.is_some_and(|row| {
                        row.valid && row.stamp.is_empty() && !row.search_ok && !row.reference_ok
                    })
                {
                    self.seen.insert(id);
                    self.search_errors += 1;
                    self.reference_errors += 1;
                    self.identity_errors += 1;
                    continue;
                }
                if let (Ok(stamp), Some(prior)) = (&actual, prior) {
                    if prior.valid && prior.stamp == *stamp {
                        self.seen.insert(id);
                        self.search_rows += usize::from(prior.search_ok);
                        self.search_errors += usize::from(!prior.search_ok);
                        self.reference_errors += usize::from(!prior.reference_ok);
                        self.identity_errors += usize::from(!prior.identity_ok);
                        continue;
                    }
                }
                if bytes > 0 && bytes + storage::ID_RECORD_BYTES + 1 > 32 << 20 {
                    self.pending = Some(path);
                    self.examined -= 1;
                    break;
                }
                invalidate(&tx, &mut self.invalidated)?;
                #[cfg(all(test, unix))]
                BEFORE_METADATA_READ.with(|slot| {
                    if let Some(hook) = slot.take() {
                        hook(&path);
                    }
                });
                let can_read = actual
                    .as_deref()
                    .ok()
                    .and_then(|stamp| archive_references::stamped_bytes(stamp).ok())
                    .is_some_and(|bytes| bytes <= storage::ID_RECORD_BYTES);
                let capture = if can_read {
                    archive_references::reconcile_with_record(&tx, &path)
                } else {
                    Err(INCOMPLETE.into())
                };
                let (stamp, row, reference_digest, identity) = match capture {
                    Ok(captured) => {
                        bytes += captured.bytes;
                        self.read_bytes += captured.bytes;
                        let identity = NativeIdentity::captured(&id, &captured.record);
                        let row = Row::captured(
                            &id,
                            captured.record,
                            captured.stamp.clone(),
                            &captured.raw_name,
                        );
                        (captured.stamp, row, captured.reference_digest, identity)
                    }
                    Err(error) => {
                        // An unsuccessful read may have consumed the full
                        // existing record bound. Charge it conservatively.
                        if can_read {
                            bytes += storage::ID_RECORD_BYTES + 1;
                            self.read_bytes += storage::ID_RECORD_BYTES + 1;
                        }
                        (
                            actual.unwrap_or_default(),
                            Err(error.clone()),
                            Err(error.clone()),
                            Err(error),
                        )
                    }
                };
                let (row_hash, search_ok) = match row {
                    Ok(row) => {
                        let hash = row.digest()?;
                        row.store(&tx)?;
                        self.search_rows += 1;
                        (hash, true)
                    }
                    Err(error) => {
                        eprintln!("hgs: archive {id} omitted from catalog: {error}");
                        sql(tx.execute("DELETE FROM archive_rows WHERE id=?", [&id]))?;
                        self.search_errors += 1;
                        (String::new(), false)
                    }
                };
                let (reference_hash, reference_ok) = match reference_digest {
                    Ok(hash) => (hash, true),
                    Err(_) => {
                        self.reference_errors += 1;
                        (String::new(), false)
                    }
                };
                sql(tx.execute(
                    "INSERT OR REPLACE INTO archive_files VALUES(?,?,?,?,?,?)",
                    params![id, stamp, row_hash, reference_hash, search_ok, reference_ok],
                ))?;
                if !NativeIdentity::store(identity, &tx, &id, &stamp)? {
                    self.identity_errors += 1;
                }
                self.seen.insert(id);
            }
            sql(tx.commit())?;
            return Ok(Step::Progress);
        }
        if self.phase == Phase::Prune {
            if directory()? != self.started {
                return Ok(Step::Restart);
            }
            let tx = sql(self.db.unchecked_transaction())?;
            let mut processed = 0;
            while processed < BATCH && (processed == 0 || Instant::now() < until) {
                let Some(id) = self.remove.pop_front() else {
                    self.phase = Phase::Seal;
                    break;
                };
                invalidate(&tx, &mut self.invalidated)?;
                for table in [
                    "archive_rows",
                    "archive_reference_paths",
                    "archive_references",
                    "archive_files",
                    "archive_identities",
                ] {
                    sql(tx.execute(&format!("DELETE FROM {table} WHERE id=?"), [&id]))?;
                }
                processed += 1;
            }
            sql(tx.commit())?;
            return Ok(Step::Progress);
        }
        if self.phase == Phase::Seal {
            if directory()? != self.started {
                return Ok(Step::Restart);
            }
            let files: u64 =
                sql(self
                    .db
                    .query_row("SELECT count(*) FROM archive_files", [], |row| row.get(0)))?;
            let rows: u64 =
                sql(self
                    .db
                    .query_row("SELECT count(*) FROM archive_rows", [], |row| row.get(0)))?;
            if files != self.seen.len() as u64 || rows != self.search_rows as u64 {
                return Ok(Step::Restart);
            }
            let legacy = if self.started == "absent" {
                "absent".into()
            } else {
                super::stamp(&root().join("archives"))?
            };
            if !self.invalidated
                && ready(&self.db)?
                && meta(&self.db, "archive_search_errors_v2")? == self.search_errors.to_string()
                && meta(&self.db, "archive_reference_errors_v2")?
                    == self.reference_errors.to_string()
                && meta(&self.db, "archive_files_v2")? == self.seen.len().to_string()
                && meta(&self.db, "archive_identity_errors_v2")? == self.identity_errors.to_string()
                && meta(&self.db, "archive_references_v1")? == "1"
                && meta(&self.db, "archive_references_stamp")? == legacy
                && meta(&self.db, "stamp")? == legacy
            {
                self.phase = Phase::Complete;
                return Ok(Step::Complete);
            }
            let tx = sql(self.db.unchecked_transaction())?;
            put_meta(&tx, "archive_directory_v2", &self.started)?;
            put_meta(
                &tx,
                "archive_search_errors_v2",
                &self.search_errors.to_string(),
            )?;
            put_meta(
                &tx,
                "archive_reference_errors_v2",
                &self.reference_errors.to_string(),
            )?;
            put_meta(&tx, "archive_files_v2", &self.seen.len().to_string())?;
            put_meta(
                &tx,
                "archive_identity_errors_v2",
                &self.identity_errors.to_string(),
            )?;
            put_meta(&tx, "archive_scan_complete_v2", "1")?;
            // Retain the old reference migration marker and stamps. A compact
            // cleanup proof still verifies every native original separately.
            put_meta(&tx, "archive_references_v1", "1")?;
            put_meta(&tx, "stamp", &legacy)?;
            put_meta(&tx, "archive_references_stamp", &legacy)?;
            if directory()? != self.started {
                return Ok(Step::Restart);
            }
            sql(tx.commit())?;
            self.phase = Phase::Complete;
            return Ok(Step::Complete);
        }
        Ok(Step::Complete)
    }
}

pub(super) fn worker() -> Result<i32> {
    worker_inner(false, Duration::from_secs(60))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn team_worker() -> Result<i32> {
    worker_inner(true, Duration::from_secs(60))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn team_worker_with_budget(budget: Duration) -> Result<i32> {
    worker_inner(true, budget)
}

fn worker_inner(team: bool, budget: Duration) -> Result<i32> {
    let state = crate::state::repositories::directory::Directory::open(
        &crate::platform::canonical(root()).map_err(fail)?,
    )?;
    let Some(_guard) = guard("worker.lock")? else {
        return Ok(0);
    };
    let until = Instant::now() + budget;
    // Teams repair shares the native writer's exclusion, taking it for one
    // builder step at a time. No session I/O or inline rebuild on the reader.
    let writer = || -> Result<Option<storage::Guard>> {
        if team {
            let file = state.open_private_lock(".lock")?;
            let lock = storage::lock_open_until(file, until)?.ok_or(INCOMPLETE)?;
            return Ok(Some(lock));
        }
        Ok(None)
    };
    let mut builder = {
        let _writer = writer()?;
        Builder::new(until)?
    };
    let mut consumed = 0;
    let mut restarts = 0;
    for _ in 0..8192 {
        if Instant::now() >= until || consumed + builder.bytes() >= 16 << 30 {
            return Ok(1);
        }
        let _writer = writer()?;
        state.verify()?;
        match builder.step()? {
            Step::Complete => return Ok(0),
            Step::Progress => {}
            Step::Restart => {
                consumed += builder.bytes();
                restarts += 1;
                if restarts >= 8 {
                    return Ok(1);
                }
                builder = Builder::new(until)?;
            }
        }
    }
    Ok(1)
}

/// One fresh compact proof for an authority caller. Its read transaction stays
/// pinned in Catalog while that caller holds the existing state writer lock.
pub(super) fn proof(db: &Connection) -> Result<String> {
    if !ready(db)? || meta(db, "archive_identity_errors_v2")? != "0" {
        return Err(INCOMPLETE.into());
    }
    let started = directory()?;
    if started == "absent" {
        return Ok(started);
    }
    let until = Instant::now() + Duration::from_secs(2);
    let mut after = String::new();
    let mut cache = BTreeMap::new();
    let mut bytes = 0;
    loop {
        let mut statement = sql(db.prepare_cached(
            "SELECT substr(id,1,65) FROM archive_identities WHERE id>? ORDER BY id LIMIT ?",
        ))?;
        let rows = sql(statement.query_map(params![after, BATCH], |r| r.get::<_, String>(0)))?;
        let batch = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
        for id in &batch {
            if id.len() > 64 || Instant::now() >= until || cache.len() >= FILES {
                return Err(INCOMPLETE.into());
            }
            let (stamp, sound, valid) = native_identity(db, id)?;
            if !sound || !valid {
                return Err(INCOMPLETE.into());
            }
            bytes += id.len() + stamp.len() + 128;
            if bytes > CACHE_BYTES {
                return Err(INCOMPLETE.into());
            }
            after = id.clone();
            cache.insert(id.clone(), stamp);
        }
        if batch.len() < BATCH {
            break;
        }
    }
    let mut seen = BTreeSet::new();
    for (count, entry) in fs::read_dir(root().join("archives"))
        .map_err(fail)?
        .enumerate()
    {
        if count >= FILES || Instant::now() >= until {
            return Err(INCOMPLETE.into());
        }
        let path = entry.map_err(fail)?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(INCOMPLETE)?;
        if uuid::Uuid::parse_str(id).is_err()
            || !cache.get(id).is_some_and(|stamp| {
                archive_references::current_stamp(&path).ok().as_deref() == Some(stamp.as_str())
            })
        {
            return Err(INCOMPLETE.into());
        }
        seen.insert(id.to_owned());
    }
    if directory()? != started || !seen.iter().eq(cache.keys()) {
        return Err(INCOMPLETE.into());
    }
    Ok(started)
}
