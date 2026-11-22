//! Verified compact archive references. JSON remains authoritative. Ordinary
//! catalog reconciliation and archive writers maintain this cache; cleanup
//! never rebuilds it or treats missing/stale coverage as an empty inventory.
use super::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    time::{Duration, Instant},
};
mod native_reader;
mod updates;

const FILES: usize = 5000;
const BYTES: usize = 32 * 1024 * 1024;
const RECORD: usize = super::storage::ID_RECORD_BYTES as usize;
const META: usize = 64 * 1024;
const INVALID: &str = "Archive reference coverage is unavailable; keep the folder for review";

fn fail(error: impl std::fmt::Display) -> String {
    format!("{INVALID}: {error}")
}
fn live(until: Instant) -> Result<()> {
    if Instant::now() >= until {
        Err(INVALID.into())
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    device: u64,
    inode: u64,
    modified: i64,
    nanos: i64,
    bytes: u64,
    changed: i64,
    changed_nanos: i64,
}
fn version(file: &fs::File) -> Result<Version> {
    let (id, meta) = crate::platform::FileIdentity::with_metadata(file).map_err(fail)?;
    #[cfg(unix)]
    let (changed, changed_nanos) = {
        use std::os::unix::fs::MetadataExt;
        (meta.ctime(), meta.ctime_nsec())
    };
    #[cfg(windows)]
    let (changed, changed_nanos) = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileBasicInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO,
        };
        let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                FileBasicInfo,
                &mut info as *mut _ as _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(fail(std::io::Error::last_os_error()));
        }
        (info.ChangeTime, 0)
    };
    Ok(Version {
        device: id.device,
        inode: id.inode,
        modified: id.modified,
        nanos: id.modified_nanos,
        bytes: meta.len(),
        changed,
        changed_nanos,
    })
}
fn open(path: &Path) -> Result<fs::File> {
    let file = crate::platform::no_follow(fs::OpenOptions::new().read(true))
        .open(path)
        .map_err(fail)?;
    let metadata = file.metadata().map_err(fail)?;
    if !metadata.is_file() {
        return Err(INVALID.into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(INVALID.into());
        }
    }
    Ok(file)
}
fn file_version(path: &Path) -> Result<Version> {
    version(&open(path)?)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u8,
    id: String,
    name: String,
    cwd: String,
    launch_dir: String,
    owner: String,
}
impl Metadata {
    fn from_record(id: &str, record: &Value) -> Result<Self> {
        if record["version"] != 1
            || string(record, "archive_id") != id
            || uuid::Uuid::parse_str(id).is_err()
            || string(record, "name").is_empty()
            || !AGENTS.contains(&string(record, "agent"))
        {
            return Err(INVALID.into());
        }
        Ok(Self {
            version: 1,
            id: id.into(),
            name: string(record, "name").into(),
            cwd: string(record, "cwd").into(),
            launch_dir: string(record, "launch_dir").into(),
            owner: string(&record["worktree_owner"], "id").into(),
        })
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = [&self.cwd, &self.launch_dir]
            .into_iter()
            .filter(|value| Path::new(value).is_absolute())
            .map(|value| {
                Path::new(value)
                    .components()
                    .collect::<PathBuf>()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
    fn value(&self) -> Value {
        json!({"version":1,"archive_id":self.id,"name":self.name,"cwd":self.cwd,
            "launch_dir":self.launch_dir,"worktree_owner":{"id":self.owner}})
    }
}
fn digest(stamp: &str, metadata: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("archive-references-v1\n{stamp}\n{metadata}"))
    )
}

/// Pure validation used while the catalog streams its compact cache in ID
/// order. It grants no authority; cleanup still verifies a fresh native roster.
pub(super) fn cached_projection(
    id: &str,
    stamp: &str,
    raw: &str,
    hash: &str,
    paths: &[String],
) -> bool {
    if raw.len() > META || stamp.len() > 1024 || hash.len() != 64 || digest(stamp, raw) != hash {
        return false;
    }
    let Ok(_version) = serde_json::from_str::<Version>(stamp) else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<Metadata>(raw) else {
        return false;
    };
    metadata.version == 1 && metadata.id == id && !metadata.name.is_empty() && metadata.paths() == paths
}

pub(super) fn stamped_bytes(stamp: &str) -> Result<u64> {
    serde_json::from_str::<Version>(stamp)
        .map(|version| version.bytes)
        .map_err(fail)
}

/// The search catalog is not cleanup coverage. These separate rows are verified
/// against the complete native file roster and each exact current file version.
pub(super) fn schema(db: &Connection) -> Result<bool> {
    let existed: bool = db.query_row("SELECT count(*)=2 FROM sqlite_master WHERE type='table' AND name IN ('archive_references','archive_reference_paths')",[],|r|r.get(0)).map_err(fail)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS archive_references(id TEXT PRIMARY KEY, stamp TEXT NOT NULL, metadata TEXT NOT NULL, digest TEXT NOT NULL, seen TEXT NOT NULL DEFAULT '');
        CREATE TABLE IF NOT EXISTS archive_reference_paths(folder TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(folder,id));
        CREATE INDEX IF NOT EXISTS archive_reference_by_id ON archive_reference_paths(id,folder);").map_err(fail)?;
    let initialized: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_meta WHERE key='archive_references_v1' AND value='1')",[],|r|r.get(0)).map_err(fail)?;
    Ok(existed && initialized)
}
fn entry(db: &Connection, id: &str) -> Result<Option<(Version, Metadata, usize)>> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT substr(stamp,1,1025),substr(metadata,1,65537),substr(digest,1,65) FROM archive_references WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(fail)?;
    let Some((stamp, raw, hash)) = row else {
        return Ok(None);
    };
    if raw.len() > META || stamp.len() > 1024 || hash.len() != 64 || digest(&stamp, &raw) != hash {
        return Err(INVALID.into());
    }
    let ver: Version = serde_json::from_str(&stamp).map_err(fail)?;
    let meta: Metadata = serde_json::from_str(&raw).map_err(fail)?;
    if meta.version != 1 || meta.id != id || meta.name.is_empty() {
        return Err(INVALID.into());
    }
    let mut query = db
        .prepare(
            "SELECT substr(folder,1,65537) FROM archive_reference_paths WHERE id=? ORDER BY folder",
        )
        .map_err(fail)?;
    let paths = query
        .query_map([id], |r| r.get::<_, String>(0))
        .map_err(fail)?
        .take(3)
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(fail)?;
    if paths != meta.paths() {
        return Err(INVALID.into());
    }
    let bytes = stamp.len() + raw.len() + hash.len() + paths.iter().map(String::len).sum::<usize>();
    Ok(Some((ver, meta, bytes)))
}
fn capture_body(path: &Path) -> Result<(Value, Version, String)> {
    // Bind the compact row to bytes from the SAME opened native file, not a
    // stamp taken after an earlier pathname read. Writers hold the state lock.
    let file = open(path)?;
    let expected = version(&file)?;
    let mut actual = read_value(&file, &expected)?;
    let raw_name = string(&actual, "name").to_owned();
    super::rename::normalize_archive(&mut actual)?;
    if file_version(path)? != expected {
        return Err(INVALID.into());
    }
    Ok((actual, expected, raw_name))
}

fn capture(path: &Path) -> Result<(Metadata, Version)> {
    let id = path.file_stem().and_then(|v| v.to_str()).ok_or(INVALID)?;
    let (record, stamp, _) = capture_body(path)?;
    Ok((Metadata::from_record(id, &record)?, stamp))
}

/// Search and reference maintenance share one bounded, no-follow native read.
/// The compact references and returned search input bind the same opened file.
pub(super) struct Captured {
    pub record: Value,
    pub raw_name: String,
    pub stamp: String,
    pub bytes: u64,
    pub reference_digest: Result<String>,
}

pub(super) fn current_stamp(path: &Path) -> Result<String> {
    serde_json::to_string(&file_version(path)?).map_err(fail)
}

pub(super) fn reconcile_with_record(db: &Connection, path: &Path) -> Result<Captured> {
    let id = path.file_stem().and_then(|v| v.to_str()).ok_or(INVALID)?;
    let (record, expected, raw_name) = capture_body(path)?;
    let stamp = serde_json::to_string(&expected).map_err(fail)?;
    // Reference projection has its own bound and omission policy. A failure
    // must not discard a body the independent search reader can still use.
    let reference_digest = (|| {
        let metadata = Metadata::from_record(id, &record)?;
        store(db, &metadata, &expected)?;
        let _ = updates::forget(&metadata.id);
        let raw = serde_json::to_string(&metadata).map_err(fail)?;
        Ok(digest(&stamp, &raw))
    })();
    Ok(Captured {
        record,
        raw_name,
        reference_digest,
        stamp,
        bytes: expected.bytes,
    })
}
/// Independent of search/label selection: old archives of the selected label
/// remain restore references even when the scoped archive reader omits them.
fn store(db: &Connection, metadata: &Metadata, expected: &Version) -> Result<()> {
    let id = &metadata.id;
    let raw = serde_json::to_string(metadata).map_err(fail)?;
    if raw.len() > META {
        return Err(INVALID.into());
    }
    let stamp = serde_json::to_string(expected).map_err(fail)?;
    db.execute(
        "INSERT INTO archive_references(id,stamp,metadata,digest) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET stamp=excluded.stamp,metadata=excluded.metadata,digest=excluded.digest",
        params![id, stamp, raw, digest(&stamp, &raw)],
    )
    .map_err(fail)?;
    db.execute("DELETE FROM archive_reference_paths WHERE id=?", [id])
        .map_err(fail)?;
    for folder in metadata.paths() {
        db.execute(
            "INSERT INTO archive_reference_paths VALUES(?,?)",
            params![folder, id],
        )
        .map_err(fail)?;
    }
    Ok(())
}
/// Cache maintenance cannot change the archive writer's outcome. Failure leaves
/// coverage stale, so a later cleanup refuses until ordinary reconciliation.
pub(super) fn note(path: &Path, record: &Value) {
    if path.parent() != Some(root().join("archives").as_path()) {
        return;
    }
    let run = || -> Result<()> {
        let id = path.file_stem().and_then(|v| v.to_str()).ok_or(INVALID)?;
        let mut normalized = record.clone();
        super::rename::normalize_archive(&mut normalized)?;
        let supplied = Metadata::from_record(id, &normalized)?;
        let (metadata, stamp) = capture(path)?;
        if supplied != metadata {
            return Err(INVALID.into());
        }
        updates::write(&metadata, &stamp)
    };
    let _ = run();
}
pub(super) fn forget(path: &Path) {
    if path.parent() != Some(root().join("archives").as_path()) {
        return;
    }
    if let Some(id) = path.file_stem().and_then(|v| v.to_str()) {
        let _ = updates::forget(id);
    }
}

struct Snapshot {
    rows: BTreeMap<String, (Version, Metadata)>,
    selected: Vec<String>,
    bytes: usize,
    namespace: Option<String>,
}
#[cfg(all(test, unix))]
std::thread_local! {
    static AFTER_DIRECTORY_TYPE: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
}
pub(super) fn directory_stamp(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path).map_err(fail)?;
    if !meta.is_dir() {
        return Err(INVALID.into());
    }
    #[cfg(all(test, unix))]
    AFTER_DIRECTORY_TYPE.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(path);
        }
    });
    let mut options = fs::OpenOptions::new();
    crate::platform::refuse_link(options.read(true));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The name may change after the type check. Never wait on a FIFO or
        // follow a replacement link, and require the opened object to be a directory.
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // custom_flags replaces rather than merges the earlier reparse flag.
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(fail)?;
    if !file.metadata().map_err(fail)?.is_dir() {
        return Err(INVALID.into());
    }
    serde_json::to_string(&version(&file)?).map_err(fail)
}
fn snapshot(until: Instant, target: Option<&Path>) -> Result<Snapshot> {
    live(until)?;
    let directory = root().join("archives");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                rows: BTreeMap::new(),
                selected: vec![],
                bytes: 0,
                namespace: None,
            })
        }
        Err(error) => return Err(fail(error)),
        Ok(_) => (),
    }
    let prior = directory_stamp(&directory)?;
    let entries = fs::read_dir(&directory).map_err(fail)?;
    let mut roster = Vec::new();
    for (count, item) in entries.enumerate() {
        live(until)?;
        if count >= FILES {
            return Err(INVALID.into());
        }
        let path = item.map_err(fail)?.path();
        if path.extension().is_none_or(|v| v != "json") {
            continue;
        }
        let id = path.file_stem().and_then(|v| v.to_str()).ok_or(INVALID)?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(INVALID.into());
        }
        roster.push((id.to_owned(), file_version(&path)?));
    }
    if directory_stamp(&directory)? != prior {
        return Err(INVALID.into());
    }
    if roster.is_empty() {
        return Ok(Snapshot {
            rows: BTreeMap::new(),
            selected: vec![],
            bytes: 0,
            namespace: Some(prior),
        });
    }
    let db = native_reader::Reader::open(until)?;
    db.execute_batch("BEGIN DEFERRED").map_err(fail)?;
    let mut rows = BTreeMap::new();
    let mut changed = std::collections::BTreeSet::new();
    let mut bytes = 0;
    for (id, actual) in roster {
        live(until)?;
        let base = entry(&db, &id)?;
        let (indexed, metadata, size) = match base {
            Some((version, metadata, size)) if version == actual => (version, metadata, size),
            prior => {
                let (version, metadata, size) = updates::read(&id)?.ok_or(INVALID)?;
                if version != actual {
                    return Err(INVALID.into());
                }
                changed.insert(id.clone());
                (
                    version,
                    metadata,
                    size + prior.map_or(0, |(_, _, size)| size),
                )
            }
        };
        bytes += size;
        if bytes > BYTES {
            return Err(INVALID.into());
        }
        rows.insert(id, (indexed, metadata));
    }
    let selected = if let Some(target) = target {
        let target = target.components().collect::<PathBuf>();
        let target = target.to_str().ok_or(INVALID)?;
        let base = target.trim_end_matches(std::path::MAIN_SEPARATOR);
        let prefix = format!("{base}{}", std::path::MAIN_SEPARATOR);
        let upper = format!("{base}{}", char::from(std::path::MAIN_SEPARATOR as u8 + 1));
        let mut query=db.prepare("SELECT DISTINCT substr(id,1,65) AS selected_id FROM archive_reference_paths WHERE folder=? OR (folder>=? AND folder<?) ORDER BY selected_id LIMIT 5001").map_err(fail)?;
        let selected = query
            .query_map(params![target, prefix, upper], |r| r.get::<_, String>(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(fail)?;
        if selected.len() > FILES {
            return Err(INVALID.into());
        }
        let selected = selected
            .into_iter()
            .filter(|id| rows.contains_key(id) && !changed.contains(id))
            .collect::<std::collections::BTreeSet<_>>();
        let matches = rows
            .iter()
            .filter(|(_, (_, metadata))| {
                metadata
                    .paths()
                    .iter()
                    .any(|folder| Path::new(folder).starts_with(Path::new(target)))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let unchanged = matches
            .iter()
            .filter(|id| !changed.contains(*id))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if unchanged != selected {
            return Err(INVALID.into());
        }
        matches
    } else {
        vec![]
    };
    for (id, (expected, _)) in &rows {
        live(until)?;
        if file_version(&super::archive::path(id)?)? != *expected {
            return Err(INVALID.into());
        }
    }
    if directory_stamp(&directory)? != prior {
        return Err(INVALID.into());
    }
    db.check()?;
    bytes = bytes.checked_add(db.bytes()).ok_or(INVALID)?;
    if bytes > BYTES {
        return Err(INVALID.into());
    }
    live(until)?;
    Ok(Snapshot {
        rows,
        selected,
        bytes,
        namespace: Some(prior),
    })
}
#[cfg(all(test, unix))]
std::thread_local! {
    static AFTER_METADATA_READ: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    pub(super) static METADATA_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Shared physical reader for archive JSON, independent of the reference cache.
/// Preserve the raw label: callers retain their own normalization/selection rules.
pub(super) fn read_metadata(path: &Path) -> Result<Value> {
    let file = open(path)?;
    let expected = version(&file)?;
    let value = read_value(&file, &expected)?;
    #[cfg(all(test, unix))]
    AFTER_METADATA_READ.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(path);
        }
    });
    if file_version(path)? != expected {
        return Err(INVALID.into());
    }
    Ok(value)
}

fn read_value(file: &fs::File, expected: &Version) -> Result<Value> {
    if version(file)? != *expected || expected.bytes > RECORD as u64 {
        return Err(INVALID.into());
    }
    let mut data = Vec::new();
    file.take(RECORD as u64 + 1)
        .read_to_end(&mut data)
        .map_err(fail)?;
    #[cfg(all(test, unix))]
    METADATA_WORK.with(|work| {
        let (reads, bytes) = work.get();
        work.set((reads + 1, bytes + data.len()));
    });
    if data.len() > RECORD || version(file)? != *expected {
        return Err(INVALID.into());
    }
    serde_json::from_slice(&data).map_err(fail)
}

fn read_record(file: &fs::File, id: &str, expected: &Version) -> Result<Value> {
    let mut record = read_value(file, expected)?;
    super::rename::normalize_archive(&mut record)?;
    Metadata::from_record(id, &record)?;
    Ok(record)
}
/// Compact terminal references and bounded DeepSeek binding metadata, with
/// complete archive coverage. No conversation journal is opened by review.
pub(super) fn visit_metadata(
    until: Instant,
    mut visit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let snapshot = snapshot(until, None)?;
    let remaining = FILES - snapshot.rows.len();
    for (_, (_, metadata)) in snapshot.rows {
        live(until)?;
        visit(metadata.value())?;
    }
    super::dsh::native_archives::visit_references(until, remaining, BYTES - snapshot.bytes, visit)
}
/// Select through the folder index, then verify the authoritative relevant
/// bodies before the first callback. Caller retains repository -> state locks
/// through durable restore preparation. The per-record bound matches live
/// records; the count, aggregate byte and deadline bounds remain unchanged.
pub(super) fn visit_related(
    until: Instant,
    target: &Path,
    owner: &str,
    mut visit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let snapshot = snapshot(until, Some(target))?;
    let mut bytes = snapshot.bytes;
    let mut records = Vec::new();
    for id in &snapshot.selected {
        live(until)?;
        let (expected, metadata) = snapshot.rows.get(id).ok_or(INVALID)?;
        if !metadata.owner.is_empty() && metadata.owner != owner {
            continue;
        }
        let path = super::archive::path(id)?;
        let file = open(&path)?;
        let record = read_record(&file, id, expected)?;
        bytes += expected.bytes as usize;
        if bytes > BYTES || file_version(&path)? != *expected {
            return Err(INVALID.into());
        }
        if Metadata::from_record(id, &record)? != *metadata {
            return Err(INVALID.into());
        }
        records.push((id.clone(), expected.clone(), record));
    }
    let mut native = Vec::new();
    super::dsh::native_archives::visit_references(
        until,
        FILES - snapshot.rows.len(),
        BYTES - bytes,
        |record| {
            let original = string(&record["worktree_owner"], "id");
            if (original.is_empty() || original == owner)
                && ["cwd", "launch_dir"].iter().any(|key| {
                    let folder = Path::new(string(&record, key));
                    folder.is_absolute() && folder.starts_with(target)
                })
            {
                native.push(record);
            }
            Ok(())
        },
    )?;
    live(until)?;
    if snapshot
        .namespace
        .as_ref()
        .is_some_and(|prior| directory_stamp(&root().join("archives")).as_ref() != Ok(prior))
    {
        return Err(INVALID.into());
    }
    for (id, expected, record) in records {
        live(until)?;
        if file_version(&super::archive::path(&id)?)? != expected {
            return Err(INVALID.into());
        }
        visit(record)?;
    }
    for record in native {
        live(until)?;
        visit(record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
