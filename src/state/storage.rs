use super::*;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;

// Read-only selected-slot source fences for saved Resume, Start fresh and terminal Fork.
pub(super) mod selected_source;
#[cfg(test)]
mod selected_source_tests;

// Dormant physical exclusion for a future legacy-to-ID storage move.
#[cfg(unix)]
#[allow(dead_code)]
pub(super) mod native_domain;

// Finite worker exclusion shared with a future owner-key transaction.
#[cfg(unix)]
pub(super) mod job_round;

// Dormant original-record/job transaction; no constructor calls this yet.
#[cfg(unix)]
#[allow(dead_code)]
pub(super) mod owner_transfer;

pub(super) fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.with(|root| root.borrow().clone()) {
        return root;
    }
    nonempty_env("HGS_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            nonempty_env("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state"))
                .join("hgs/sessions")
        })
}

#[cfg(test)]
std::thread_local! {
    /// A test's own state root on this thread (`with_test_state_root`).
    static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_root(root: Option<PathBuf>) {
    TEST_ROOT.with(|value| *value.borrow_mut() = root);
}

pub(super) fn private_dir(path: &Path) -> Result<()> {
    crate::platform::private_dir(path).map_err(|e| e.to_string())
}

pub(super) fn absolute_root() -> Result<PathBuf> {
    private_dir(&root())?;
    crate::platform::canonical(root()).map_err(|e| e.to_string())
}

/// Crash cuts for tests (#1437 review): a budget of durable writes, after
/// which every further write refuses before it starts, as if the writer had
/// crashed there. Off unless a test arms it.
#[cfg(test)]
pub(super) mod cut {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// The writes allowed, plus one; 0 is off.
    static BUDGET: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);
    /// Allows exactly `writes` more durable writes.
    #[cfg(unix)]
    pub(crate) fn arm(writes: usize) {
        DONE.store(0, Ordering::SeqCst);
        BUDGET.store(writes + 1, Ordering::SeqCst);
    }
    #[cfg(unix)]
    pub(crate) fn disarm() {
        BUDGET.store(0, Ordering::SeqCst);
    }
    /// Refuses a write once the budget is spent; else counts it.
    pub(crate) fn write() -> Result<(), String> {
        let budget = BUDGET.load(Ordering::SeqCst);
        if budget != 0 && DONE.load(Ordering::SeqCst) + 1 >= budget {
            return Err("crash cut".into());
        }
        DONE.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Counts a durable write for the crash cuts; nothing outside tests.
pub(super) fn cut_write() -> Result<()> {
    #[cfg(test)]
    cut::write()?;
    Ok(())
}

/// The fixed keeper already owns its original writer. Ordinary I/O here
/// would reacquire it or bypass the post-staging source/lifeline fence.
fn refuse_picker_io() -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if effort::picker_entered() {
        return effort::picker_refuse("Ordinary storage I/O is forbidden in the original picker keeper".into());
    }
    Ok(())
}

pub(super) fn atomic(path: &Path, contents: &str) -> Result<()> {
    refuse_picker_io()?;
    let parent = path.parent().ok_or("cannot write file without parent")?;
    cut_write()?;
    private_dir(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".hgs-")
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    temp.write_all(contents.as_bytes())
        .map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    crate::platform::persist(temp, path).map_err(|e| e.to_string())?;
    crate::platform::sync_dir(parent).map_err(|e| e.to_string())
}

pub(super) type Guard = crate::platform::FileLock;

pub(super) fn lock(path: Option<&Path>) -> Result<Guard> {
    refuse_picker_io()?;
    let default = root().join(".lock");
    let path = path.unwrap_or(&default);
    private_dir(path.parent().ok_or("invalid lock path")?)?;
    let file = lock_file(path)?;
    let guard = Guard::exclusive(file).map_err(|e| e.to_string())?;
    if path == default {
        rename::recover_locked()?;
        rename::sync_legacy_locked()?;
        worktree_lock::recover_locked();
    }
    Ok(guard)
}

/// An exclusive lock on an auxiliary file, or None when another process still
/// holds it at `deadline`. Use writer_until for the default state lock so that
/// its recovery protocol runs as well.
pub(super) fn lock_until(path: &Path, deadline: std::time::Instant) -> Result<Option<Guard>> {
    private_dir(path.parent().ok_or("invalid lock path")?)?;
    let file = lock_file(path)?;
    lock_open_until(file, deadline)
}

/// Read-only channel admission: the state writer already exists. Never
/// create it, follow a link, wait on a FIFO or invoke normal writer recovery.
pub(super) fn existing_lock_until(path: &Path, deadline: std::time::Instant) -> Result<Option<Guard>> {
    let parent = repositories::directory::Directory::open(path.parent().ok_or("invalid lock path")?)?;
    let name = path.file_name().and_then(|name| name.to_str()).ok_or("invalid lock name")?;
    lock_open_until(parent.existing_private_lock(name)?, deadline)
}

fn lock_open_until(file: fs::File, deadline: std::time::Instant) -> Result<Option<Guard>> {
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(Some(Guard::held(file))),
            Err(error) if !crate::platform::lock_contended(&error) => return Err(error.to_string()),
            Err(_) if std::time::Instant::now() >= deadline => return Ok(None),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

/// The normal writer-lock recovery, with a bounded admission wait for optional
/// lifecycle cleanup. Never turn a busy writer into an unbounded archive exit.
pub(super) fn writer_until(deadline: std::time::Instant) -> Result<Option<Guard>> {
    private_dir(&root())?;
    let Some(guard) = lock_until(&root().join(".lock"), deadline)? else { return Ok(None); };
    rename::recover_locked()?;
    rename::sync_legacy_locked()?;
    worktree_lock::recover_locked();
    Ok(Some(guard))
}

/// Inactive run-gate support: recover ONLY under the normal writer's original
/// pinned file. No installed caller uses this entry. Do not call the path-based
/// writer first: its recovery would run before a replaced lock was detected.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
pub(super) fn recover_run_writer(
    guard: &Guard,
    original: crate::platform::FileIdentity,
    until: std::time::Instant,
) -> Result<()> {
    let refused = "Original session state writer changed";
    if std::time::Instant::now() >= until {
        return Err(refused.into());
    }
    let file = crate::platform::no_follow(OpenOptions::new().read(true).write(true))
        .open(root().join(".lock"))
        .map_err(|_| refused)?;
    if !crate::platform::verify_private_file(&file).map_err(|_| refused)?
        || crate::platform::FileIdentity::of(&file).map_err(|_| refused)? != original
        || guard.identity().map_err(|_| refused)? != original
    {
        return Err(refused.into());
    }
    rename::recover_locked()?;
    rename::sync_legacy_locked()?;
    worktree_lock::recover_locked();
    if std::time::Instant::now() >= until {
        return Err(refused.into());
    }
    Ok(())
}

/// An owner-only lock file, never written. Windows locks a byte range only
/// through a handle with read or write access, which append-only lacks.
pub(super) fn lock_file(path: &Path) -> Result<fs::File> {
    crate::platform::private_open(OpenOptions::new().create(true).truncate(false).write(true))
        .open(path)
        .map_err(|e| e.to_string())
}

pub(super) fn legacy_record_path(name: &str) -> PathBuf {
    root().join(format!("{:x}.json", Sha256::digest(name.as_bytes())))
}

pub(super) fn route_path(name: &str) -> PathBuf {
    root()
        .join("name_routes")
        .join(legacy_record_path(name).file_name().unwrap())
}

/// The label's own path: `sha256(label).json`, or `bindings/` through
/// `name_routes`. Only the label's first holder lives there.
pub(super) fn label_record_path(name: &str) -> PathBuf {
    let legacy = legacy_record_path(name);
    if route_path(name).exists() {
        root().join("bindings").join(legacy.file_name().unwrap())
    } else {
        legacy
    }
}

/// Where a session that reuses a label lives for life (zerus-ucck P3, the ID
/// layout). Only a canonical session ID names such a path.
pub(super) fn id_record_path(id: &str) -> Option<PathBuf> {
    session_id::canonical(id).then(|| root().join("sessions").join(format!("{id}.json")))
}

/// Whether `record` says it lives in the ID layout.
pub(super) fn in_id_layout(record: &Value) -> bool {
    record["layout"] == "id"
}

/// Whether `record`, read from `sessions/<id>.json`, is that session's own
/// record: it says it lives in the ID layout and carries that canonical ID.
/// Every reader of the ID layout, native ones included, checks this first;
/// a present file that fails it refuses (#1437 review).
pub(crate) fn is_id_layout_record(record: &Value, id: &str) -> bool {
    session_id::canonical(id) && in_id_layout(record) && string(record, "session_id") == id
}

/// The path this command reads `name` from (P3a):
/// - the selected session's own record, when the command selected under
///   `name` a session in the ID layout. The read then verifies every selected
///   identity, so a selection never falls back to the label's holder;
/// - else the label's path, when its first holder is there;
/// - else the label's only session in the ID layout, so name-based readers
///   reach a session whose label has no first holder.
pub(super) fn record_path(name: &str) -> PathBuf {
    if let Some(path) = selected_id_path(name) {
        return path;
    }
    let label = label_record_path(name);
    if label.exists() {
        return label;
    }
    sole_id_holder(name).unwrap_or(label)
}

fn sole_id_holder(name: &str) -> Option<PathBuf> {
    let candidates = labels::candidates(name).ok()?;
    match (candidates.first, candidates.by_id.as_slice()) {
        (false, [only]) => id_record_path(string(only, "session_id")),
        _ => None,
    }
}

/// The key of the files a session owns, such as its recovery job and its
/// native-input lock, taken from its verified record (#1437 review): its
/// session ID in the ID layout (P3a), so two sessions that share a label never
/// share one and a rename in place moves none of them; else its label's
/// SHA-256, as before. Never from a label alone or the command's selection, so
/// a watch, a worker or a hook that holds a record keeps that session's files.
pub(super) fn owner_key(record: &Value) -> String {
    let id = string(record, "session_id");
    if in_id_layout(record) && session_id::canonical(id) {
        return id.to_owned();
    }
    label_key(string(record, "name"))
}

/// The SHA-256 of `name`, the key of a legacy session's files.
pub(super) fn label_key(name: &str) -> String {
    format!("{:x}", Sha256::digest(name.as_bytes()))
}

/// Whether a live or saved terminal session holds `name`, at its label path
/// or in the root ID layout. Native providers use this to exclude terminals,
/// not their own pending bindings. Unknown completeness still counts as taken.
pub(super) fn label_taken(name: &str) -> bool {
    labels::terminal_taken(name).unwrap_or(true)
}

/// The fixed path of the session the command selected in the ID layout under
/// `name`. It stays the route when that file vanishes or is replaced: the read
/// then refuses, and never falls back to the label's holder (#1437 review).
fn selected_id_path(name: &str) -> Option<PathBuf> {
    let selection = session_id::selected()
        .filter(|selection| selection.name == name && selection.id_layout)?;
    id_record_path(&selection.session_id)
}

pub(super) fn read(name: &str) -> Result<Value> {
    read_limit(name,None)
}
pub(super) fn read_bounded(name: &str, bytes: u64) -> Result<Value> {
    read_limit(name,Some(bytes))
}
/// The session in the ID layout with this ID, verified: the record in
/// `sessions/<id>.json` must carry that ID and say it is in the ID layout.
pub(super) fn read_id(id: &str) -> Result<Value> {
    let path = id_record_path(id).ok_or("invalid session ID")?;
    let bytes = record_bytes(&path, None).map_err(|e| not_found(id, e))?;
    let name = serde_json::from_slice::<Value>(&bytes)
        .map(|value| string(&value, "name").to_owned())
        .map_err(|e| format!("{id}: invalid saved binding: {e}"))?;
    verify_at(&path, &name, &bytes)
}

/// Shared byte bound for live ID records and their archived copies. Retained
/// job metadata can make a valid record larger than a megabyte.
pub(super) const ID_RECORD_BYTES: u64 = 16 << 20;

/// Bounded authoritative metadata only. No native telemetry, transcript cache,
/// SQLite view or process observation is refreshed before keeper admission.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn read_metadata(name: &str) -> Result<Value> {
    let path = if let Some(path) = selected_id_path(name) {
        path
    } else {
        let label = label_record_path(name);
        match fs::symlink_metadata(&label) {
            Ok(_) => label,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let candidates = labels::candidates_metadata(name)?;
                match (candidates.first, candidates.by_id.as_slice()) {
                    (false, [only]) => id_record_path(string(only, "session_id"))
                        .ok_or("Invalid metadata session ID")?,
                    (_, []) => label,
                    _ => {
                        return Err(format!(
                            "{name}: choose an exact session ID before reading metadata"
                        ))
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    read_metadata_at(&path, name)
}

/// One already-selected native slot, without telemetry repairs or observations.
/// The returned bytes add no launch/input authority.
pub(super) fn read_selected_metadata(name: &str) -> Result<Value> {
    read_metadata_at(&record_path(name), name)
}

fn read_metadata_at(path: &Path, name: &str) -> Result<Value> {
    let bytes = record_bytes(path, Some(ID_RECORD_BYTES)).map_err(|e| not_found(name, e))?;
    let record = verify_metadata_at(path, name, &bytes)?;
    session_id::verify(&record)?;
    Ok(record)
}

/// Structural proof for bytes retained by recovery's original read. Selection
/// is checked by public metadata readers; it never leaks into a different
/// historical transaction through ambient caller state.
pub(super) fn verify_metadata_at(path: &Path, name: &str, bytes: &[u8]) -> Result<Value> {
    let record: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("{name}: invalid saved binding: {e}"))?;
    if !record.is_object()
        || record["version"] != 1
        || string(&record, "name") != name
        || !AGENTS.contains(&string(&record, "agent"))
    {
        return Err(format!("{name}: invalid saved binding"));
    }
    let by_id = in_sessions(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if (by_id && !is_id_layout_record(&record, stem)) || (!by_id && in_id_layout(&record)) {
        return Err(format!(
            "{name}: the saved binding's layout and location disagree"
        ));
    }
    Ok(record)
}

/// Whether `path` is in the ID layout's own folder.
fn in_sessions(path: &Path) -> bool {
    path.parent().is_some_and(|parent| parent == root().join("sessions"))
}

/// A record file's bytes. In the ID layout, and whenever a caller bounds it,
/// the read never follows a final link, never blocks on a FIFO, takes only a
/// regular file and stops at its bound (#1437 review): a lock-free scan must
/// not hang on what it finds there.
fn record_bytes(path: &Path, limit: Option<u64>) -> io::Result<Vec<u8>> {
    let retiring = limit.is_some();
    let Some(limit) = limit.or(in_sessions(path).then_some(ID_RECORD_BYTES)) else {
        return fs::read(path);
    };
    bounded_bytes(
        path,
        limit,
        "Selected session is not a regular record",
        if retiring {
            "Selected session record exceeds retirement bound"
        } else {
            "Session record exceeds its bound"
        },
    )
}

/// A binding file's bytes for a scan that decides whether a folder is still
/// in use (zerus-lr1l2): never a final link, never waiting on a FIFO, a
/// regular file only, at most the ID layout's record bound. Anything else is
/// an error, so the caller keeps what it protects for review.
pub(super) fn binding_bytes(path: &Path) -> io::Result<Vec<u8>> {
    bounded_bytes(
        path,
        ID_RECORD_BYTES,
        "Session binding is not a regular file",
        "Session binding exceeds its bound",
    )
}

fn bounded_bytes(
    path: &Path,
    limit: u64,
    irregular: &'static str,
    too_large: &'static str,
) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = crate::platform::no_follow(OpenOptions::new().read(true)).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(irregular));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(too_large));
    }
    Ok(bytes)
}

fn not_found(name: &str, error: io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!("{name}: no saved binding; restart through hgs to enable tracking")
    } else {
        error.to_string()
    }
}

/// The record at the label's own path, never steered by a selection.
pub(super) fn read_label(name: &str) -> Result<Value> {
    read_at(&label_record_path(name), name, None)
}

/// [`read_label`] bounded: a regular file only, never a final link and never
/// waiting on a FIFO.
pub(super) fn read_label_bounded(name: &str, bytes: u64) -> Result<Value> {
    read_at(&label_record_path(name), name, Some(bytes))
}

fn read_limit(name: &str, limit: Option<u64>) -> Result<Value> {
    read_at(&record_path(name), name, limit)
}

fn read_at(path: &Path, name: &str, limit: Option<u64>) -> Result<Value> {
    let bytes = record_bytes(path, limit).map_err(|e| not_found(name, e))?;
    verify_at(path, name, &bytes)
}

/// The record in `bytes`, read from `path` as `name`, verified.
fn verify_at(path: &Path, name: &str, bytes: &[u8]) -> Result<Value> {
    let mut record: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("{name}: invalid saved binding: {e}"))?;
    if !record.is_object()
        || record["version"] != 1
        || string(&record, "name") != name
        || !AGENTS.contains(&string(&record, "agent"))
    {
        return Err(format!("{name}: invalid saved binding"));
    }
    // A record in the ID layout is verified against its own path: the same
    // session ID, and the layout it claims. Neither alone is authority.
    let by_id = in_sessions(path);
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    if (by_id && !is_id_layout_record(&record, stem)) || (!by_id && in_id_layout(&record)) {
        return Err(format!("{name}: the saved binding's layout and location disagree"));
    }
    session_id::verify(&record)?;
    journal::repair_kimi_main(&mut record);
    journal::repair_claude_internal(&mut record);
    claude_subagents::reconcile(&mut record);
    native_subagents::reconcile(&mut record);
    subagent_lifecycle::reconcile(&mut record);
    Ok(record)
}

/// A session's durable identity (zerus-ucck P1): assigned when the session is
/// first created and kept by every write of this hgs, across resume, Clear,
/// rename, pause, archive and restore; a fork gets its own. An older hgs
/// rebuilds a record on resume without this field, and the next write here
/// then assigns a new one once. Consumers treat it as a hint and fall back to
/// the run and conversation identities.
pub(super) fn ensure_session_id(record: &mut Value) {
    if string(record, "session_id").is_empty() {
        record["session_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
}

/// Where `record` is written: back to its own `sessions/<id>.json` when it is
/// in the ID layout, else the label's path. A record keeps the ID layout only
/// when it says so and that file exists; when the two disagree the write is
/// refused, never guessed (P3a).
fn write_path(record: &Value) -> Result<(PathBuf, bool)> {
    let name = string(record, "name");
    let at_id = id_record_path(string(record, "session_id")).filter(|path| path.is_file());
    match (in_id_layout(record), at_id) {
        (true, Some(path)) => Ok((path, true)),
        (false, None) => Ok((label_record_path(name), false)),
        _ => Err(format!("{name}: the session's layout and its record disagree; refusing to write")),
    }
}

/// The current record of the session `record` names, read from where it
/// lives: its own `sessions/<id>.json` in the ID layout, else its label's path.
/// None when that file is gone.
pub(super) fn current_of(record: &Value) -> Result<Option<Value>> {
    if in_id_layout(record) {
        let id = string(record, "session_id");
        return match id_record_path(id) {
            Some(path) if path.exists() => read_id(id).map(Some),
            _ => Ok(None),
        };
    }
    let name = string(record, "name");
    if label_record_path(name).exists() {
        read_label(name).map(Some)
    } else {
        Ok(None)
    }
}

/// Removes the live or saved session `record` from where it lives, and then
/// everything that names it: the legacy rename shadow for a record at its
/// label's path, the ID index entry, and, for the ID layout, its label-index
/// entry, which goes only after the record (the index stays a superset).
pub(super) fn remove_record(record: &Value) -> Result<()> {
    let path = own_path(record)?;
    if in_id_layout(record) {
        if path.exists() {
            remove_record_file(&path, record)?;
        }
        session_index::retire(record);
        return labels::remove(string(record, "name"), string(record, "session_id"));
    }
    rename::remove_legacy(record)?;
    if path.exists() {
        remove_record_file(&path, record)?;
    }
    session_index::retire(record);
    Ok(())
}

fn remove_record_file(path: &Path, record: &Value) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        super::process_index_writer::remove_at(&root(), path, record, || {
            archive::remove_synced(path)
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = record;
        archive::remove_synced(path)
    }
}

/// The file `record` lives in: its own `sessions/<id>.json` in the ID layout,
/// else its label's path. Deletions use this, never the label alone, so a
/// session in the ID layout never removes its label's first holder.
pub(super) fn own_path(record: &Value) -> Result<PathBuf> {
    write_path(record).map(|(path, _)| path)
}

pub(super) fn write(record: &mut Value) -> Result<()> {
    refuse_picker_io()?;
    record["updated"] = json!(now());
    let (path, by_id) = write_path(record)?;
    atomic_record(&path, record)?;
    if by_id {
        // A session in the ID layout never writes at a label's path.
        session_index::note(record, session_index::Kind::IdRecord);
        return Ok(());
    }
    session_index::note(record, session_index::Kind::Record);
    rename::mirror_legacy(record)
}

#[cfg(all(test, unix))]
thread_local! {
    pub(super) static BEFORE_NEW_ID: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Dormant P3e primitive: publish a NEW, preallocated identity, never move or
/// recreate a prior one. Caller retains the actual Root writer. Terminal IDs
/// create and retain their Fresh input lock/kind before record publication.
/// No production writer calls this until the complete fleet floor is enabled.
/// An error is not a no-effect witness and must never re-mint a native request.
#[allow(dead_code)]
pub(super) fn publish_new_id(_writer: &Guard, record: &mut Value) -> Result<()> {
    use super::provider_binding::Provider;
    use super::repositories::directory::Directory;
    let id = string(record,"session_id");
    let name = string(record,"name");
    if !is_id_layout_record(record,id) || name.is_empty() || name.len() > 2048 {
        return Err("A new ID record needs its original canonical session identity".into());
    }
    let provider = Provider::named(name);
    let (namespace,kind,limit) = match provider {
        Some(Provider::DeepSeek) if record["backend"] == "dsh"
            && string(record,"conversation_id").starts_with("session-")
            && string(record,"conversation_id").len() <= 2048 =>
                (Some("dsh"),session_index::Kind::DeepSeekId,1<<20),
        Some(Provider::OpenCode) if record["backend"] == "opencode"
            && opencode::native_id(string(record,"conversation_id"),"ses_") =>
                (Some("opencode"),session_index::Kind::OpenCodeId,1<<20),
        None if record["version"] == 1 && AGENTS.contains(&string(record,"agent"))
            && name.starts_with(&format!("{}/",string(record,"agent"))) =>
                (None,session_index::Kind::IdRecord,ID_RECORD_BYTES),
        _ => return Err("The new session's native namespace and identity disagree".into()),
    };
    let references = worktree_cleanup::bindings()?;
    if references.iter().any(|r| r.session_id == id) {
        return Err("This session identity is already published; it cannot be created again".into());
    }
    let state = Directory::open(&absolute_root()?)?;
    let parent = namespace.map(|name| state.ensure_child(name)).transpose()?;
    let directory = parent.as_ref().unwrap_or(&state).ensure_child("sessions")?;
    let label_directory = state.ensure_child("labels")?;
    let mut published = record.clone();
    if !published["worktree_owner"].is_null() {
        published["worktree_protection"] = published["worktree_owner"]["id"].clone();
    }
    published["updated"] = json!(now());
    processes::compact(&mut published);
    let bytes = format!("{published}\n");
    if bytes.len() as u64 > limit {return Err("The new session record exceeds its bound".into());}
    // A protective Git veto is durable before either the superset or record.
    // Keep its actual repository/directory/lock resources through publication.
    let protection = worktree_lock::Protection::acquire(&published)?;
    #[cfg(unix)]
    let fresh_input = if namespace.is_none() {
        Some(native_domain::Fresh::create_under(_writer, &published, std::time::Instant::now() + std::time::Duration::from_secs(5))?)
    } else { None };
    #[cfg(not(unix))]
    if namespace.is_none() { return Err("Fresh ID native input is unverified on this computer".into()); }
    labels::add(name,id)?;
    if !labels::listed_in(&state.path,name).is_some_and(|ids| ids.iter().any(|i| i == id)) {
        return Err("The original label index did not retain the new identity".into());
    }
    #[cfg(all(test, unix))]
    if let Some(hook) = BEFORE_NEW_ID.with(|hook| hook.borrow_mut().take()) {hook();}
    state.verify()?; directory.verify()?; label_directory.verify()?;
    if crate::platform::canonical(root()).map_err(|e| e.to_string())? != state.path {
        return Err("The selected state root changed before ID publication".into());
    }
    if let Some(protection) = &protection {protection.verify()?;}
    #[cfg(unix)]
    if let Some(input) = &fresh_input {input.verify()?;}
    cut_write()?;
    directory.write_new(&format!("{id}.json"),bytes.as_bytes())?;
    crate::platform::sync_dir(&directory.path).map_err(|e| e.to_string())?;
    state.verify()?; directory.verify()?; label_directory.verify()?;
    if crate::platform::canonical(root()).map_err(|e| e.to_string())? != state.path {
        return Err("The selected state root changed after ID publication".into());
    }
    if let Some(protection) = &protection {protection.verify()?;}
    #[cfg(unix)]
    if let Some(input) = &fresh_input {input.verify()?;}
    session_index::note(&published,kind);
    *record = published;
    Ok(())
}

/// Saved live/archive writes compact command previews and settled job history.
pub(super) fn atomic_record(path: &Path, record: &mut Value) -> Result<()> {
    ensure_session_id(record);
    processes::compact(record);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    super::process_index_writer::write_at(&root(), path, record, |record| {
        atomic(path, &format!("{record}\n"))
    })?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    atomic(path, &format!("{record}\n"))?;
    super::archive_references::note(path, record);
    Ok(())
}

/// Names are mutable; provider hooks and already-running supervisors retain
/// their original environment. The run token is the immutable binding identity.
pub(super) fn read_run(name: &str, run_id: &str) -> Result<Option<Value>> {
    read_session_run(name, run_id, None)
}

/// `read_run` for the agent's own hooks, which also know the session they were
/// launched for (`HGS_SESSION_ID`, zerus-ucck P1). Only callers whose name and
/// run come from that same environment use it; a command an agent runs for
/// another session keeps `read_run`. It never depends on the ID surviving
/// an older hgs.
pub(super) fn read_own_run(name: &str, run_id: &str) -> Result<Option<Value>> {
    let session = nonempty_env("HGS_SESSION_ID");
    // The ID index first (P2a): one verified read, under whatever label the
    // session carries now, so a rename since launch costs no scan.
    if let (Some(id), false) = (session.as_deref(), run_id.is_empty()) {
        if let Some(found) = session_index::find(id) {
            if matches!(found.kind, session_index::Kind::Record | session_index::Kind::IdRecord)
                && string(&found.record, "run_id") == run_id
            {
                return Ok(Some(found.record));
            }
        }
    }
    // Hooks never repair the index: their poll loops must not take the writer
    // lock. The record's next write notes the entry again.
    read_session_run(name, run_id, session.as_deref())
}

/// `session` is a hint, never a requirement: an older hgs can rebuild the
/// record without its ID and the next write here assigns a new one, while the
/// agent keeps the ID it was launched with. The record found by name is taken
/// at once only when it carries `session`. Otherwise the run scan prefers the
/// records launched for `session` and, when none is, the run alone decides, as
/// before P1. Name-first is safe only while names are unique (P3 needs P2's ID
/// layout first).
fn read_session_run(name: &str, run_id: &str, session: Option<&str>) -> Result<Option<Value>> {
    if run_id.is_empty() {
        return Ok(None);
    }
    let launched_for =
        |record: &Value| session.is_none_or(|id| string(record, "session_id") == id);
    if record_path(name).exists() {
        let record = read(name)?;
        if string(&record, "run_id") == run_id && launched_for(&record) {
            return Ok(Some(record));
        }
    }
    session_index::scanned();
    let (own, others): (Vec<Value>, Vec<Value>) = records()?
        .into_iter()
        .filter(|record| string(record, "run_id") == run_id)
        .partition(|record| launched_for(record));
    let mut matching = if own.is_empty() { others } else { own }.into_iter();
    let record = matching.next();
    if matching.next().is_some() {
        return Err("ambiguous session run identity".into());
    }
    Ok(record)
}

/// Bindings are named by `legacy_record_path`; other JSON files in the state
/// root, such as desktop presence and read marks, are not session records.
fn is_record_path(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
        && stem.len() == 64
        && stem.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub(super) fn records() -> Result<Vec<Value>> {
    if !root().exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(root())
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_record_path(path))
        .collect();
    let bindings = root().join("bindings");
    if bindings.exists() {
        paths.extend(
            fs::read_dir(bindings)
                .map_err(|e| e.to_string())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| is_record_path(path)),
        );
    }
    paths.sort();
    // Sessions in the ID layout (P3a), read from their own path.
    let sessions = root().join("sessions");
    let mut by_id: Vec<PathBuf> = if sessions.is_dir() {
        fs::read_dir(&sessions)
            .map_err(|e| e.to_string())?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| is_id_record_path(path))
            .collect()
    } else {
        Vec::new()
    };
    by_id.sort();
    let mut result = Vec::new();
    for path in paths {
        let loaded = (|| {
            let value: Value =
                serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if value["rename_shadow"] == true {
                return Ok(None);
            }
            read_at(&label_record_path(string(&value, "name")), string(&value, "name"), None).map(Some)
        })();
        match loaded {
            Ok(Some(record)) => result.push(record),
            Ok(None) => {}
            Err(error) => eprintln!("hgs: cannot read {}: {error}", path.display()),
        }
    }
    for path in by_id {
        let id = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        match read_id(id) {
            Ok(record) => result.push(record),
            Err(error) => eprintln!("hgs: cannot read {}: {error}", path.display()),
        }
    }
    Ok(result)
}

/// Every directory that holds live or saved session bindings: the label
/// paths, the provider bindings and, since P3a, the ID layout. A scanner of
/// bindings reads all of them; `binding_dirs_cover_the_id_layout` checks the
/// source for one that does not.
pub(crate) const BINDING_DIRS: [&str; 7] = [
    "",
    "bindings",
    "dsh/bindings",
    "opencode/bindings",
    "sessions",
    "dsh/sessions",
    "opencode/sessions",
];

/// Whether a file named `stem` in the binding directory `directory` can be a
/// binding: a label's SHA-256 at the root and in `bindings/`, a canonical
/// session ID in the ID layout, and any name in a provider's bindings.
pub(crate) fn binding_stem(directory: &str, stem: &str) -> bool {
    match directory {
        "" | "bindings" => stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()),
        "sessions" | "dsh/sessions" | "opencode/sessions" => session_id::canonical(stem),
        _ => true,
    }
}

/// A file in `sessions/` named by a canonical session ID.
fn is_id_record_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(session_id::canonical)
}

pub(super) type Pane = (String, String, String);
pub(super) type Snapshot = BTreeMap<String, Vec<Pane>>;

/// Each live session's panes as (pane, dead, run marker). No server means none.
pub(super) fn live() -> Result<Snapshot> {
    live_sessions().map(|(snapshot, _)| snapshot)
}

/// `live`, and from the same answer each live session's name by its immutable
/// session ID (`$N`), which a rename keeps.
pub(super) fn live_sessions() -> Result<(Snapshot, BTreeMap<String, String>)> {
    let mut result: Snapshot = BTreeMap::new();
    let mut names = BTreeMap::new();
    for row in backend::current().panes()? {
        if !row.session_id.is_empty() {
            names.insert(row.session_id, row.session.clone());
        }
        result
            .entry(row.session)
            .or_default()
            .push((row.pane, row.dead, row.run_marker));
    }
    Ok((result, names))
}

/// The terminal session `record`'s agent runs in (zerus-ucck P3b): its
/// `terminal_name`, or for a record an older build wrote, its label, which
/// was then its terminal's name too. Every step from a record to its
/// terminal goes through this, never through the label.
pub(crate) fn terminal_name(record: &Value) -> &str {
    match string(record, "terminal_name") {
        "" => string(record, "name"),
        terminal => terminal,
    }
}

/// The live panes of `record`'s own terminal, matched or not.
pub(super) fn panes_of<'a>(snapshot: &'a Snapshot, record: &Value) -> Option<&'a Vec<Pane>> {
    snapshot.get(terminal_name(record))
}

/// The original pane/run proves liveness first. A still-starting launch
/// instead identifies its durable session through the terminal's environment.
/// An unreadable environment is never evidence that the launch is absent.
pub(super) fn terminal_live(snapshot: &Snapshot, record: &Value) -> Result<bool> {
    if matches(record, panes_of(snapshot, record)) {
        return Ok(true);
    }
    launched_as(snapshot, terminal_name(record), record)
}

pub(super) fn launched_as(snapshot: &Snapshot, terminal: &str, record: &Value) -> Result<bool> {
    if !snapshot.contains_key(terminal) {
        return Ok(false);
    }
    launch_identity(record, |key| backend::current().session_variable(terminal, key))
        .map_err(|error| format!("{error}; it may be this session's own launch, so nothing was changed"))
}

fn launch_identity(record: &Value, mut variable: impl FnMut(&str) -> Result<Option<String>>) -> Result<bool> {
    // New launches/resumes carry their own ID. The rollback floor already
    // carries HGS_RESUME_SESSION_ID when it resumes an ID-layout session.
    for key in ["HGS_SESSION_ID", "HGS_RESUME_SESSION_ID"] {
        // Older launchers explicitly clear the unused marker. Only a
        // nonempty value declares identity; malformed declarations still refuse.
        if let Some(id) = variable(key)?.filter(|id| !id.is_empty()) {
            if !session_id::canonical(&id) || uuid::Uuid::parse_str(&id).is_ok_and(|id| id.is_nil()) {
                return Err("the terminal's launch session ID is invalid".into());
            }
            return Ok(id == string(record, "session_id"));
        }
    }
    // Only the legacy first holder can have been launched without either
    // marker. A label-only launch cannot establish an ID twin's liveness.
    Ok(!in_id_layout(record) && variable("HGS_SESSION")?.as_deref() == Some(string(record, "name")))
}

pub(super) fn matches(record: &Value, panes: Option<&Vec<Pane>>) -> bool {
    panes
        .map(|panes| {
            panes.len() == 1
                && panes[0].0 == string(record, "pane")
                && panes[0].1 == "0"
                && panes[0].2 == string(record, "run_id")
                && !panes[0].2.is_empty()
        })
        .unwrap_or(false)
}

pub(super) fn process_start(pid: u32) -> String {
    if let Some(start) = crate::platform::process_start_time(pid) { return start; }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if effort::picker_entered() {
        let args=["-p".into(),pid.to_string(),"-o".into(),"lstart=".into()];
        return effort::picker_helper_read(effort::PickerRead::ProcessStart,&args)
            .and_then(std::result::Result::ok).filter(|output|output.status.success())
            .map(|output|String::from_utf8_lossy(&output.stdout).trim().to_owned()).unwrap_or_default();
    }
    crate::platform::process_start_fallback(pid).unwrap_or_default()
}

pub(super) fn process_alive(record: &Value) -> bool {
    let start = string(record, "process_start");
    let pid = record["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .unwrap_or(0);
    !start.is_empty() && pid > 0 && process_start(pid) == start
}

pub(super) fn pause_active(record: &Value) -> bool {
    let owner = &record["pausing"];
    let pid = owner["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .unwrap_or(0);
    !string(owner, "start").is_empty() && process_start(pid) == string(owner, "start")
}

pub(super) fn join_command(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_words::quote(arg).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The hgs that a session's supervisor, hooks and recipes run: on Windows only
/// the stable CLI, whichever hgs.exe runs now (spec D8).
#[cfg(windows)]
pub(super) fn runner_executable() -> Result<String> {
    crate::platform::stable_executable().map(|path| path.display().to_string())
}

#[cfg(unix)]
pub(super) fn runner_executable() -> Result<String> {
    if let Some(executable) = nonempty_env("HGS_EXECUTABLE") {
        let path = Path::new(&executable);
        if path.is_absolute() && crate::platform::executable(path) {
            return Ok(executable);
        }
    }
    Ok(std::env::current_exe()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .into_owned())
}

/// The pane command of a tracked session started from its saved `record`
/// (resume, restore, fresh start, fork): `executable --run 0 ARGV`, through the
/// record's login shell with the account's variables on Unix, directly on
/// Windows, where the session's own variables carry the account
/// (`account_variables`, spec D8).
pub(super) fn supervisor_command(
    record: &Value,
    executable: &str,
    argv: Vec<String>,
    hint_environment: &[String],
) -> Vec<String> {
    supervisor_command_with_count(record, executable, argv, hint_environment, "0")
}

pub(super) fn supervisor_command_with_count(
    record: &Value,
    executable: &str,
    argv: Vec<String>,
    hint_environment: &[String],
    count: &str,
) -> Vec<String> {
    #[cfg(unix)]
    let mut launch = {
        let shell = if string(record, "shell").is_empty() {
            "/bin/bash"
        } else {
            string(record, "shell")
        };
        let (shell, flags) = crate::accounts::supervisor_shell(shell);
        vec![
            shell.into(),
            flags.into(),
            crate::accounts::run_script(
                shell,
                string(record, "account_id"),
                string(record, "agent"),
                string(record, "agent_home"),
                hint_environment,
            ),
            executable.into(),
            count.into(),
        ]
    };
    #[cfg(windows)]
    let mut launch = {
        let _ = (record, hint_environment);
        vec![executable.into(), "--run".into(), count.into()]
    };
    launch.extend(argv);
    launch
}

/// The account variables of a Windows session started from `record`, after its
/// other variables so that they win; none on Unix, where `supervisor_command`
/// applies them after the login shell.
pub(super) fn account_variables(record: &Value) -> Vec<String> {
    if cfg!(windows) {
        crate::accounts::run_environment(
            string(record, "account_id"),
            string(record, "agent"),
            string(record, "agent_home"),
        )
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod agent_hint_tests {
    use super::*;

    #[test]
    fn agent_hint_transport_keeps_project_launchers_user_flags_and_native_argv() {
        let hint = vec!["HGS_AGENT_HINT=1".into(), "HGS_AGENT_HINT_OFF=".into()];
        for (agent, argv) in [
            (
                "claude",
                vec![
                    "project-claude",
                    "--resume",
                    "conversation",
                    "--append-system-prompt",
                    "user append",
                    "--system-prompt",
                    "user system",
                    "--settings",
                    "user.json",
                ],
            ),
            (
                "codex",
                vec![
                    "project-codex",
                    "resume",
                    "conversation",
                    "-c",
                    "developer_instructions=\"User instructions\"",
                    "--profile",
                    "work",
                ],
            ),
        ] {
            let record = json!({"agent":agent,"account_id":"work-account","agent_home":"/private/profile","shell":"/bin/bash"});
            let native = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let command = supervisor_command(&record, "/stable/hgs", native.clone(), &hint);
            assert_eq!(&command[command.len() - native.len()..], native.as_slice());
            #[cfg(unix)]
            {
                assert_eq!(&command[..2], &["/bin/bash", "-lic"]);
                let script = shell_words::split(&command[2]).unwrap();
                assert!(script.contains(&"HGS_AGENT_HINT=1".into()));
                assert!(script.contains(&"HGS_AGENT_HINT_OFF=".into()));
                assert!(!script
                    .iter()
                    .any(|word| word.starts_with("HGS_CONFIG_DIR=")));
                assert_eq!(&command[3..5], &["/stable/hgs", "0"]);
            }
            #[cfg(windows)]
            assert_eq!(&command[..3], &["/stable/hgs", "--run", "0"]);
        }
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::*;

    #[test]
    fn a_launch_identity_never_borrows_another_twin_s_label() {
        let one = "11111111-1111-4111-8111-111111111111";
        let two = "22222222-2222-4222-8222-222222222222";
        let record = json!({"name":"codex/example/shared","session_id":one,"layout":"id"});
        let env = |id: Option<&str>, resume: Option<&str>, label: Option<&str>| {
            let fields = [("HGS_SESSION_ID",id),("HGS_RESUME_SESSION_ID",resume),("HGS_SESSION",label)];
            launch_identity(&record, |key| Ok(fields.iter().find(|(k,_)|*k==key).unwrap().1.map(str::to_owned)))
        };
        assert_eq!(env(Some(one),None,Some("codex/example/old-label")),Ok(true));
        assert_eq!(env(Some(two),None,Some("codex/example/shared")),Ok(false));
        assert_eq!(env(None,Some(one),Some("codex/example/shared")),Ok(true));
        assert_eq!(env(None,Some(two),Some("codex/example/shared")),Ok(false));
        assert_eq!(env(None,None,Some("codex/example/shared")),Ok(false));
        assert!(env(Some("damaged"),Some(one),Some("codex/example/shared")).is_err());
        assert!(env(Some("00000000-0000-0000-0000-000000000000"),None,None).is_err());
        let mut legacy = record.clone(); legacy.as_object_mut().unwrap().remove("layout");
        assert_eq!(launch_identity(&legacy, |key|Ok((key=="HGS_SESSION").then(||"codex/example/shared".into()))),Ok(true));
        assert!(launch_identity(&record, |_|Err("unreadable".into())).is_err());
    }

    #[test]
    fn empty_launch_markers_keep_only_the_legacy_liveness_fallback() {
        let id = "11111111-1111-4111-8111-111111111111";
        let legacy = json!({"name":"codex/example/shared","session_id":id});
        let mut by_id = legacy.clone(); by_id["layout"] = json!("id");
        let mut old = legacy.clone(); old.as_object_mut().unwrap().remove("session_id");
        let check = |record: &Value, primary: Option<&str>, resume: Option<&str>| {
            launch_identity(record, |key| Ok(match key {
                "HGS_SESSION_ID" => primary.map(str::to_owned),
                "HGS_RESUME_SESSION_ID" => resume.map(str::to_owned),
                "HGS_SESSION" => Some("codex/example/shared".into()),
                _ => unreachable!(),
            }))
        };
        for record in [&legacy, &old] {
            assert_eq!(check(record,None,Some("")),Ok(true));
            assert_eq!(check(record,Some(""),Some("")),Ok(true));
        }
        assert_eq!(check(&by_id,Some(""),Some("")),Ok(false));
        assert_eq!(check(&by_id,Some(""),Some(id)),Ok(true));
        assert!(check(&legacy,Some(" "),Some(id)).is_err());
        assert!(check(&legacy,Some(""),Some("damaged")).is_err());
        assert!(launch_identity(&legacy, |_|Err("unavailable".into())).is_err());
    }

    /// zerus-ucck P1, #1317 R5: every write of this hgs keeps a session's ID,
    /// an older writer's strip leads to one new ID, and no two records ever
    /// share one. The fixture runs in a child process with a private state
    /// directory.
    #[test]
    fn a_session_keeps_one_id_through_every_writer() {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::storage::session_id_tests::session_id_writers_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path())
            .env("ZERUS_SESSION_ID_WRITERS_FIXTURE", "1")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    #[ignore = "run by a_session_keeps_one_id_through_every_writer"]
    fn session_id_writers_fixture() {
        if std::env::var_os("ZERUS_SESSION_ID_WRITERS_FIXTURE").is_none() {
            return;
        }
        std::fs::create_dir_all(root()).unwrap();
        let on_disk = |path: &Path| -> Value { serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap() };
        let id = |record: &Value| string(record, "session_id").to_owned();
        let claude = |name: &str, conversation: &str, run: &str| {
            json!({"version":1,"agent":"claude","name":name,"conversation_id":conversation,"run_id":run,"created":100})
        };
        // Every write path of this hgs keeps the ID: records, archives and
        // both provider saves, which save the caller's record twice.
        let mut live = claude("claude/example/live", "conversation-1", "run-1");
        write(&mut live).unwrap();
        let session = id(&live);
        assert!(uuid::Uuid::parse_str(&session).is_ok());
        live["run_id"] = json!("run-2");
        write(&mut live).unwrap();
        assert_eq!(id(&live), session);
        assert_eq!(string(&on_disk(&record_path("claude/example/live")), "session_id"), session);
        let archive = root().join("archives").join("archive-1.json");
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let mut archived = live.clone();
        archived["archive_id"] = json!("archive-1");
        atomic_record(&archive, &mut archived).unwrap();
        assert_eq!(string(&on_disk(&archive), "session_id"), session, "an archive keeps it");
        let mut older_archive = claude("claude/example/old", "conversation-old", "run-3");
        older_archive["archive_id"] = json!("archive-2");
        atomic_record(&root().join("archives").join("archive-2.json"), &mut older_archive).unwrap();
        assert!(!id(&older_archive).is_empty(), "an archive of an older record gains one");
        let mut deepseek = json!({"backend":"dsh","agent":"dsh","name":"dsh/example/one","conversation_id":"session-1","run_id":"run-4"});
        super::super::dsh::save(&mut deepseek).unwrap();
        let first = id(&deepseek);
        let binding = root().join("dsh/bindings").join(format!("{:x}.json", Sha256::digest(b"dsh/example/one")));
        assert_eq!(string(&on_disk(&binding), "session_id"), first, "kept on the caller's record");
        deepseek["paused"] = json!(false);
        super::super::dsh::save(&mut deepseek).unwrap();
        assert_eq!(id(&deepseek), first);
        let mut opencode = json!({"backend":"opencode","agent":"opencode","name":"opencode/example/one","conversation_id":"ses_1","run_id":"run-5"});
        super::super::opencode::save(&mut opencode).unwrap();
        let first_opencode = id(&opencode);
        let binding = root().join("opencode/bindings").join(format!("{:x}.json", Sha256::digest(b"opencode/example/one")));
        assert_eq!(string(&on_disk(&binding), "session_id"), first_opencode, "kept on the caller's record");
        super::super::opencode::save(&mut opencode).unwrap();
        assert_eq!(id(&opencode), first_opencode);
        // Expected: an older hgs rebuilds the record without the field, and
        // the next write here assigns a new ID once, which then stays.
        let mut stripped = live.clone();
        stripped.as_object_mut().unwrap().remove("session_id");
        write(&mut stripped).unwrap();
        let reminted = id(&stripped);
        assert!(!reminted.is_empty() && reminted != session, "one new ID after an older writer");
        write(&mut stripped).unwrap();
        assert_eq!(id(&stripped), reminted, "then stable");
        // A fork is another session with its own ID.
        let mut fork = claude("claude/example/fork", "conversation-2", "run-6");
        fork["fork_parent_id"] = json!("conversation-1");
        write(&mut fork).unwrap();
        assert!(!id(&fork).is_empty() && id(&fork) != reminted);
        // No two records ever share an ID: equal names, conversations and
        // creation seconds, a rename and a reopen in the same second, and
        // concurrent first writes.
        let mut seen = std::collections::HashSet::new();
        for record in [&live, &older_archive, &deepseek, &opencode, &stripped, &fork] {
            assert!(seen.insert(id(record)), "distinct");
        }
        let mut renamed = claude("claude/example/renamed", "conversation-3", "run-7");
        renamed["journal_name"] = json!("claude/example/same");
        write(&mut renamed).unwrap();
        let mut reopened = claude("claude/example/same", "conversation-3", "run-8");
        write(&mut reopened).unwrap();
        let mut twin = claude("claude/example/same", "conversation-3", "run-8");
        twin["archive_id"] = json!("archive-3");
        atomic_record(&root().join("archives").join("archive-3.json"), &mut twin).unwrap();
        for record in [&renamed, &reopened, &twin] {
            assert!(seen.insert(id(record)), "same-second rename, reopen and copy stay distinct");
        }
        let written: Vec<String> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..32)
                .map(|n| {
                    scope.spawn(move || {
                        let mut record = json!({"version":1,"agent":"claude","name":format!("claude/example/concurrent-{n}"),
                            "conversation_id":"conversation-shared","run_id":format!("run-{n}"),"created":100});
                        write(&mut record).unwrap();
                        string(&record, "session_id").to_owned()
                    })
                })
                .collect();
            workers.into_iter().map(|worker| worker.join().unwrap()).collect()
        });
        for written in written {
            assert!(seen.insert(written), "concurrent first writes stay distinct");
        }
    }
}
