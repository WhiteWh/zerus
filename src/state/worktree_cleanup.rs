//! Review and guarded removal of linked worktrees. Removal repeats every check
//! under the repository lock used by creation, compares the fingerprint the
//! person reviewed. Force is limited to explicitly confirmed, content-fingerprinted
//! uncommitted files. A short-lived manual process confirmation accepts incomplete
//! access to process paths; active/locked/nested and other unknowns stay protected.
use super::worktrees::{cache_file, canonical, error, git, git_holding, list, GitError};
use super::*;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::time::{Duration, Instant, UNIX_EPOCH};

const MAX_REVIEWED: usize = 64;
const RECENT: f64 = 24.0 * 3600.0;
const WALK_ENTRIES: usize = 200_000;
const WALK_TIME: Duration = Duration::from_secs(2);
const MAX_IGNORED: usize = 20;
const MAX_SAMPLES: usize = 5;
const PROCESS_CONSENT_SECONDS: u64 = 300;
/// Leftover rows in one reply, and their bytes: the rest stay recorded and
/// the reply is marked partial, so many records never blank a bounded reply.
const LEFTOVER_ROWS: usize = 32;
const LEFTOVER_BYTES: usize = 1024 * 1024;

const MACOS_LEFTOVER_REASON: &str = "Deleting leftover files is disabled on macOS until AppleDouble cleanup can be guarded. Open this folder in Finder to review and clean it up manually, or keep the folder.";

fn macos_leftover_deletion_blocked() -> bool {
    #[cfg(test)]
    if FORCE_MACOS_LEFTOVER_GUARD.with(std::cell::Cell::get) {
        return true;
    }
    cfg!(target_os = "macos")
}

fn leftover_deletion_guard() -> std::io::Result<()> {
    if macos_leftover_deletion_blocked() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            MACOS_LEFTOVER_REASON,
        ));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FORCE_MACOS_LEFTOVER_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn admit_leftover(used: &mut usize, row: &Value) -> bool {
    let size = serde_json::to_vec(row).map_or(usize::MAX, |bytes| bytes.len());
    match used.checked_add(size).filter(|n| *n <= LEFTOVER_BYTES) {
        Some(total) => {
            *used = total;
            true
        }
        None => false,
    }
}

fn process_consent(row: &Value, common: &Path, issued: u64) -> String {
    let proof = fingerprint(json!({"scope":"manual-process-uncertainty-v2",
        "worktree":row["fingerprint"],"common_dir":common,"issued":issued}));
    format!("v2:{issued}:{proof}")
}

fn valid_process_consent(proof: &str, row: &Value, common: &Path, at: u64) -> bool {
    let proof = if let Some(original) = proof.strip_prefix(zerus_client_core::worktree_review::PERMISSION_DENIALS_PREFIX) {
        if !zerus_client_core::worktree_review::permission_denials_complete(row) {
            return false;
        }
        original
    } else { proof };
    let Some(issued) = proof.strip_prefix("v2:").and_then(|s| s.split_once(':'))
        .and_then(|(issued, _)| issued.parse::<u64>().ok()) else { return false; };
    if issued > at || at - issued >= PROCESS_CONSENT_SECONDS
        || row["fingerprint"].as_str().is_none_or(str::is_empty) {
        return false;
    }
    // The person accepts unknown process paths for this checkout for five minutes,
    // including ordinary unrelated process churn. A fresh complete inventory is
    // still required. Any known use or other protection removes this authority.
    let reviewable = row["process_fingerprint"].as_str().is_some_and(|s| !s.is_empty());
    let clear = matches!(row["verdict"].as_str(), Some("ready" | "review"))
        && row["reasons"].as_array().is_some_and(Vec::is_empty);
    (reviewable || clear) && proof == process_consent(row, common, issued)
}

/// Folders a Zerus session record refers to.
pub(super) struct Reference {
    pub(super) name: String,
    pub(super) session_id: String,
    pub(super) folders: Vec<PathBuf>,
    /// The ID of the checkout ownership recorded at the session's launch.
    pub(super) owner: String,
    /// Actual enumerated metadata, only for a negative protection annotation.
    /// This is not an action record, native permission or caller-selected path.
    pub(super) binding: Option<BindingMetadata>,
}

pub(super) struct BindingMetadata {
    pub(super) directory: String,
    pub(super) file: String,
    pub(super) digest: String,
}

enum ProcessReview {
    Ordinary(std::result::Result<crate::platform::WorktreeProcesses, String>),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    NativeObjects {
        path: PathBuf,
        checkout: zerus_client_core::worktree_programs::DirectoryId,
        programs: Vec<zerus_client_core::worktree_programs::Program>,
        until: Instant,
    },
}
impl ProcessReview {
    fn has_permission_denials(&self) -> bool {
        matches!(self, Self::Ordinary(Ok(rows)) if !rows.unreadable.is_empty())
    }
}
struct Context {
    common: PathBuf,
    /// (display name, full ref) of the main checkout's branch and origin/HEAD.
    bases: Vec<(String, String)>,
    /// Other processes' current directories, or why they cannot be read.
    processes: ProcessReview,
    bindings: Vec<Reference>,
    bindings_complete: bool,
    archives: Vec<Reference>,
    archives_complete: bool,
}

fn reason(code: &str, message: impl Into<String>) -> Value {
    json!({"code":code,"message":message.into()})
}

fn plural(count: usize, word: &str) -> String {
    let suffix = match (count, word.ends_with('s')) {
        (1, _) => "",
        (_, true) => "es",
        _ => "s",
    };
    format!("{count} {word}{suffix}")
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes)
        .trim_end_matches('\n')
        .to_owned()
}

fn step(total: Instant, seconds: u64) -> Instant {
    total.min(Instant::now() + Duration::from_secs(seconds))
}

/// Missing folders keep their spelling; an existing parent is resolved so that
/// the same checkout compares equal through symlinked parents.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(path) = crate::platform::canonical(path) {
        return path;
    }
    match (
        path.parent()
            .and_then(|p| crate::platform::canonical(p).ok()),
        path.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

fn inside(folder: &Path, tree: &Path) -> bool {
    folder.starts_with(tree)
}

fn reference(record: &Value) -> Reference {
    let folders = ["cwd", "launch_dir"]
        .iter()
        .map(|key| string(record, key))
        .filter(|folder| Path::new(folder).is_absolute())
        .map(canonical)
        .collect();
    Reference {
        name: string(record, "name").to_owned(),
        session_id: string(record, "session_id").to_owned(),
        folders,
        owner: string(&record["worktree_owner"], "id").to_owned(),
        binding: None,
    }
}

/// Decode the entry actually enumerated, without the command's target
/// selection. A census must not replace another holder's reference with the
/// selected same-label session. The result is folder metadata, never an action
/// record or native permission.
fn binding_reference(directory: &str, path: &Path) -> Result<Option<Reference>> {
    let bytes = storage::binding_bytes(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let name = string(&value,"name");
    let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or("Invalid session binding path")?;
    if !value.is_object() || name.is_empty() { return Err("Invalid saved session identity".into()); }
    let by_id = matches!(directory,"sessions" | "dsh/sessions" | "opencode/sessions");
    if (by_id && !storage::is_id_layout_record(&value,stem)) || (!by_id && storage::in_id_layout(&value)) {
        return Err("The session binding layout and location disagree".into());
    }
    if !by_id && storage::label_key(name) != stem {
        return Err("The session binding name and location disagree".into());
    }
    let valid = match directory {
        "" | "bindings" | "sessions" => value["version"] == 1 && AGENTS.contains(&string(&value,"agent")),
        "dsh/bindings" | "dsh/sessions" => value["backend"] == "dsh" && name.starts_with("dsh/")
            && string(&value,"conversation_id").starts_with("session-") && string(&value,"conversation_id").len() <= 2048,
        "opencode/bindings" | "opencode/sessions" => value["backend"] == "opencode" && name.starts_with("opencode/")
            && opencode::native_id(string(&value,"conversation_id"),"ses_"),
        _ => false,
    };
    if !valid { return Err("Invalid saved session binding".into()); }
    for field in ["cwd","launch_dir"] {
        if value.get(field).is_some_and(|value| !value.is_null() && !value.is_string()) {
            return Err("A saved session folder is unreadable".into());
        }
    }
    if value["rename_shadow"] == true { return Ok(None); }
    let mut result=reference(&value);
    result.binding=Some(BindingMetadata {directory:directory.into(),
        file:path.file_name().and_then(|f|f.to_str()).ok_or("Invalid binding filename")?.into(),
        digest:format!("{:x}",Sha256::digest(&bytes))});
    Ok(Some(result))
}

pub(super) fn bindings() -> Result<Vec<Reference>> {
    // Ordinary listings may tolerate a broken entry. Folder/lock decisions
    // cannot treat a skipped, replaced or unreadable binding as no reference.
    let mut result = Vec::new();
    let mut count = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    for directory in storage::BINDING_DIRS {
        let folder = root().join(directory);
        let files = match fs::read_dir(&folder) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        for file in files {
            if Instant::now() >= deadline { return Err("Session references could not all be checked in time".into()); }
            let path = file.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "json") { continue; }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            if !storage::binding_stem(directory,stem) { continue; }
            count += 1;
            if count > 5000 { return Err("Too many session bindings to check".into()); }
            // Every physical holder (including routed legacy bindings) uses
            // main's bounded binding_bytes read. Do not re-resolve the label:
            // that would substitute the command-selected same-label twin.
            if let Some(reference) = binding_reference(directory,&path)? { result.push(reference); }
        }
    }
    if Instant::now() >= deadline { return Err("Session references could not all be checked in time".into()); }
    Ok(result)
}

fn context(common: &Path, trees: &[Value], deadline: Instant) -> Context {
    context_with_processes(common, trees, deadline, None)
}
fn context_with_processes(
    common: &Path,
    trees: &[Value],
    deadline: Instant,
    native: Option<ProcessReview>,
) -> Context {
    let mut bases = Vec::new();
    if let Some(main) = trees.first().filter(|tree| tree["kind"] == "main") {
        if let Some(branch) = main["branch"].as_str().filter(|b| !b.is_empty()) {
            bases.push((branch.to_owned(), format!("refs/heads/{branch}")));
        }
    }
    if let Ok(origin) = git(
        common,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        deadline,
    ) {
        let origin = text(origin);
        if origin.starts_with("refs/remotes/") {
            let name = origin.trim_start_matches("refs/remotes/").to_owned();
            bases.push((name, origin));
        }
    }
    let records = bindings();
    let bindings_complete = records.is_ok();
    let bindings = records.unwrap_or_default();
    let mut archives = Vec::new();
    let archives_complete = archive_references::visit_metadata(deadline, |record| {
        archives.push(reference(&record));
        Ok(())
    })
    .is_ok();
    Context {
        common: common.to_path_buf(),
        bases,
        processes: native.unwrap_or_else(|| {
            ProcessReview::Ordinary(crate::platform::worktree_processes(deadline))
        }),
        bindings,
        bindings_complete,
        archives,
        archives_complete,
    }
}

/// `Some(true)` when `commit` is reachable from `base`, `None` when Git failed.
fn ancestor(cwd: &Path, commit: &str, base: &str, deadline: Instant) -> Option<bool> {
    match git(
        cwd,
        &["merge-base", "--is-ancestor", commit, base],
        deadline,
    ) {
        Ok(_) => Some(true),
        Err(e) if e.state == "metadata_error" && e.detail.is_empty() => Some(false),
        Err(_) => None,
    }
}

#[derive(Default)]
struct Status {
    changes: usize,
    untracked: usize,
    ignored: Vec<String>,
    entries: Vec<String>,
}

fn status(tree: &Path, deadline: Instant) -> std::result::Result<Status, GitError> {
    let bytes = git(
        tree,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
        deadline,
    )?;
    let mut result = Status::default();
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            return Err(error("metadata_error", "Invalid Git status entry"));
        }
        let path = String::from_utf8_lossy(&field[3..]).into_owned();
        match &field[..2] {
            b"!!" => result.ignored.push(path.trim_end_matches('/').to_owned()),
            b"??" => result.untracked += 1,
            code => {
                result.changes += 1;
                // Renames and copies are followed by their original path.
                if matches!(code[0], b'R' | b'C') {
                    fields.next();
                }
            }
        }
        result
            .entries
            .push(String::from_utf8_lossy(field).into_owned());
    }
    Ok(result)
}

#[cfg(test)]
thread_local! {
    static AFTER_LEFTOVER_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static MEASURED_PATHS: std::cell::RefCell<Option<BTreeMap<PathBuf, usize>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn trace_measured(path: &Path) {
    MEASURED_PATHS.with(|paths| {
        if let Some(paths) = paths.borrow_mut().as_mut() {
            *paths.entry(path.to_path_buf()).or_default() += 1;
        }
    });
}

/// Apparent size of an ignored entry, without following symlinks.
fn measure(path: &Path, entries: &mut usize, deadline: Instant) -> (u64, bool, String) {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    let mut complete = true;
    let mut manifest = BTreeMap::new();
    while let Some(path) = pending.pop() {
        *entries += 1;
        if *entries > WALK_ENTRIES || Instant::now() >= deadline {
            complete = false;
            break;
        }
        #[cfg(test)]
        trace_measured(&path);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            complete = false;
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        manifest.insert(
            path.to_string_lossy().into_owned(),
            json!({
                "size":metadata.len(), "modified":modified.map(|t| t.as_nanos().to_string()),
                "directory":metadata.is_dir(), "link":fs::read_link(&path).ok()
            }),
        );
        if metadata.is_dir() {
            match fs::read_dir(&path) {
                Ok(children) => {
                    for child in children {
                        match child {
                            Ok(child) => pending.push(child.path()),
                            Err(_) => complete = false,
                        }
                    }
                }
                Err(_) => complete = false,
            }
        } else {
            total += metadata.len();
        }
    }
    (total, complete, fingerprint(json!(manifest)))
}

fn modified(path: &Path) -> f64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.)
}

/// Git status omits special files (for example FIFOs), and ignored nested
/// repositories can be invisible there too. A bounded metadata inventory must
/// establish that neither is silently discarded. Symlinks are not followed.
fn inventory(root: &Path, known_nested: &[String], deadline: Instant) -> (u64, bool, Vec<Value>) {
    let mut pending = vec![root.to_path_buf()];
    let mut bytes = 0;
    let mut count = 0;
    let mut complete = true;
    let mut blockers = Vec::new();
    while let Some(path) = pending.pop() {
        count += 1;
        if count > WALK_ENTRIES || Instant::now() >= deadline {
            complete = false;
            break;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        if path.file_name().is_some_and(|name| {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        }) {
            if let Some(parent) = path.parent().filter(|parent| *parent != root) {
                if !known_nested
                    .iter()
                    .any(|nested| Path::new(nested) == parent)
                {
                    blockers.push(reason(
                        "nested_repository",
                        format!(
                            "Contains another Git repository: {}. Move it out first.",
                            parent.display()
                        ),
                    ));
                }
            }
            continue;
        }
        if metadata.is_dir() {
            match fs::read_dir(&path) {
                Ok(files) => {
                    for file in files {
                        match file {
                            Ok(file) => pending.push(file.path()),
                            Err(_) => complete = false,
                        }
                    }
                }
                Err(_) => complete = false,
            }
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            bytes += metadata.len();
        } else {
            blockers.push(reason(
                "special_files",
                format!(
                    "Contains a special local file: {}. Remove or move it explicitly first.",
                    path.display()
                ),
            ));
        }
        if blockers.len() >= MAX_SAMPLES {
            break;
        }
    }
    (bytes, complete, blockers)
}

fn fingerprint(material: Value) -> String {
    format!("{:x}", Sha256::digest(material.to_string().as_bytes()))
}

/// Bound the exact data a manual discard would lose, not merely status letters
/// or file sizes. Never follow symlinks, read special files, or bless a partial
/// scan. The cached index's raw blob IDs also protect staged-only changes.
fn discard_proof(tree: &Path, status: &Status, deadline: Instant) -> Option<String> {
    let staged = git(
        tree,
        &[
            "diff",
            "--cached",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "-z",
        ],
        deadline,
    )
    .ok()?;
    let mut pending = Vec::new();
    for entry in &status.entries {
        if entry.starts_with("!!") {
            continue;
        }
        let relative = Path::new(entry.get(3..)?);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        let mut parent = tree.to_path_buf();
        let parts: Vec<_> = relative.components().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            parent.push(part.as_os_str());
            if fs::symlink_metadata(&parent).ok()?.file_type().is_symlink() {
                return None;
            }
        }
        pending.push(tree.join(relative));
    }
    let mut manifest = BTreeMap::new();
    let mut remaining = 64 * 1024 * 1024_u64;
    let mut entries = 0;
    while let Some(path) = pending.pop() {
        entries += 1;
        if entries > WALK_ENTRIES || Instant::now() >= deadline || path.to_str().is_none() {
            return None;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                manifest.insert(path, json!({"missing":true}));
                continue;
            }
            Err(_) => return None,
        };
        let value = if metadata.file_type().is_symlink() {
            json!({"link": fs::read_link(&path).ok()?})
        } else if metadata.is_dir() {
            for child in fs::read_dir(&path).ok()? {
                pending.push(child.ok()?.path());
            }
            json!({"directory":true})
        } else if metadata.is_file() {
            if metadata.len() > remaining {
                return None;
            }
            let mut data = Vec::new();
            fs::File::open(&path)
                .ok()?
                .take(remaining + 1)
                .read_to_end(&mut data)
                .ok()?;
            if data.len() as u64 > remaining {
                return None;
            }
            remaining -= data.len() as u64;
            let after = fs::symlink_metadata(&path).ok()?;
            if metadata.len() != after.len()
                || metadata.modified().ok() != after.modified().ok()
                || !after.is_file()
            {
                return None;
            }
            json!({"content":format!("{:x}", Sha256::digest(&data))})
        } else {
            return None;
        };
        manifest.insert(path, value);
    }
    (Instant::now() < deadline).then(|| {
        fingerprint(json!({"files":manifest,"index":format!("{:x}",Sha256::digest(staged))}))
    })
}

fn finish(mut row: Value, reasons: Vec<Value>, notes: Vec<Value>) -> Value {
    row["verdict"] = json!(if !reasons.is_empty() {
        "blocked"
    } else if row["available"] == false {
        "missing"
    } else if !notes.is_empty() {
        "review"
    } else {
        "ready"
    });
    row["reasons"] = json!(reasons);
    row["notes"] = json!(notes);
    row
}

fn uses(references: &[Reference], path: &Path) -> Vec<String> {
    let mut names: Vec<String> = references
        .iter()
        .filter(|r| r.folders.iter().any(|folder| inside(folder, path)))
        .map(|r| r.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Running processes and saved sessions that use a folder. Shared by worktree
/// rows and partial-removal leftovers.
fn occupants(path: &Path, context: &Context, row: &mut Value, reasons: &mut Vec<Value>) {
    // Without the running processes' folders, nothing may count as unused.
    let mut samples: Vec<_> = match &context.processes {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        ProcessReview::NativeObjects {
            path: original,
            checkout,
            programs,
            until,
        } => {
            if path != original || Instant::now() >= *until {
                reasons.push(reason(
                    "processes_unavailable",
                    "The original native process review changed or expired.",
                ));
                Vec::new()
            } else {
                row["native_process_scope"] = json!(checkout);
                // Actual object-based observations, already bound to the held
                // original Scope/holder descriptor. No aggregate PID is erased.
                programs
                    .iter()
                    .map(|program| (program.pid, program.label.as_str()))
                    .collect()
            }
        }
        ProcessReview::Ordinary(Ok(processes)) => {
            row["process_coverage"] = json!({
                "abi": zerus_client_core::worktree_review::PERMISSION_DENIALS_ABI,
                "complete": processes.membership_complete,
                "open_files_checked": processes.membership_complete,
                "permission_denials": processes.unreadable.len(),
            });
            row["unreadable_processes"] = json!(processes.unreadable);
            if !processes.unreadable.is_empty() {
                reasons.push(reason("processes_unavailable", format!(
                    "Zerus cannot check whether some running programs use this folder ({} with hidden paths).",
                    plural(processes.unreadable.len(), "process")
                )));
            }
            processes
                .paths
                .iter()
                .filter(|(_, cwd, _)| inside(cwd, path))
                .map(|(pid, _, name)| (*pid, name.as_str()))
                .collect()
        }
        ProcessReview::Ordinary(Err(error)) => {
            reasons.push(reason(
                "processes_unavailable",
                format!("{error}, so a program may still use this folder."),
            ));
            Vec::new()
        }
    };
    samples.sort_by_key(|(pid, _)| *pid);
    samples.dedup_by_key(|(pid, _)| *pid);
    row["process_count"] = json!(samples.len());
    row["processes"] = json!(samples
        .iter()
        .take(MAX_SAMPLES)
        .map(|(pid, name)| json!({"pid":pid,"command":name}))
        .collect::<Vec<_>>());
    if !samples.is_empty() {
        let names: Vec<_> = samples
            .iter()
            .take(MAX_SAMPLES)
            .map(|(pid, name)| format!("{name} ({pid})"))
            .collect();
        reasons.push(reason(
            "processes",
            format!(
                "{} use this folder: {}. Close them first.",
                plural(samples.len(), "running process"),
                names.join(", ")
            ),
        ));
    }
    let sessions = uses(&context.bindings, path);
    if !sessions.is_empty() {
        reasons.push(reason(
            "sessions",
            format!(
                "Zerus sessions use this folder: {}. Archive them first.",
                sessions.join(", ")
            ),
        ));
    }
    row["sessions"] = json!(sessions);
}

fn assess(tree: &Value, trees: &[Value], context: &Context, total: Instant) -> Value {
    let path = PathBuf::from(tree["path"].as_str().unwrap_or(""));
    let head = string(tree, "head").to_owned();
    let branch = string(tree, "branch").to_owned();
    let mut row = tree.clone();
    let owner=super::worktree_lifecycle::owned(&context.common, &path);
    row["zerus_owned"] = json!(owner.is_some());
    row["ownership_id"] = owner.as_ref().map(|owner|owner["id"].clone()).unwrap_or(Value::Null);
    row["archive_restore_ready"] = json!(context.archives_complete && owner.as_ref().is_some_and(|owner|owner["branch"]==tree["branch"]));
    for key in ["locked", "prunable", "detached"] {
        row[key] = json!(tree[key] == true);
    }
    row["fingerprint"] = json!("");
    match string(tree, "kind") {
        "main" => {
            return finish(
                row,
                vec![reason(
                    "main_checkout",
                    "This is the main checkout of the repository.",
                )],
                vec![],
            )
        }
        "bare" => {
            return finish(
                row,
                vec![reason("bare", "A bare repository has no working folder.")],
                vec![],
            )
        }
        _ => {}
    }
    let mut reasons = Vec::new();
    let mut notes = Vec::new();
    if !context.bindings_complete {
        reasons.push(reason(
            "sessions_unavailable",
            "Saved sessions could not be checked. Review again before removal.",
        ));
    }
    if tree["locked"] == true {
        let why = string(tree, "locked_reason");
        reasons.push(reason(
            "locked",
            if why.is_empty() {
                "Locked by Git.".to_owned()
            } else {
                format!("Locked by Git: {why}")
            },
        ));
    }
    if Instant::now() >= total {
        reasons.push(reason(
            "status_unavailable",
            "The review ran out of time before this worktree.",
        ));
        return finish(row, reasons, notes);
    }

    occupants(&path, context, &mut row, &mut reasons);
    let archived = uses(&context.archives, &path).len();
    row["archived_sessions"] = json!(archived);
    if !context.archives_complete {
        notes.push(reason("archives_unavailable","Archive references could not be fully checked. Automatic cleanup keeps this worktree."));
    }

    if tree["available"] != true {
        row["fingerprint"] = json!(fingerprint(json!({"path":path,"head":head,"branch":branch,"owner":row["ownership_id"],
            "locked":row["locked"],"prunable":row["prunable"],"available":false,"sessions":row["sessions"]})));
        return finish(row, reasons, notes);
    }

    let mut nested: Vec<String> = trees
        .iter()
        .filter_map(|other| other["path"].as_str())
        .filter(|other| Path::new(other) != path && inside(Path::new(other), &path))
        .map(str::to_owned)
        .collect();
    nested.sort();
    if !nested.is_empty() {
        reasons.push(reason(
            "nested",
            format!("Contains a nested worktree: {}", nested.join(", ")),
        ));
    }
    row["nested"] = json!(nested);

    let status = match status(&path, step(total, 5)) {
        Ok(status) => status,
        Err(e) => {
            reasons.push(reason(
                "status_unavailable",
                format!("Git status is unavailable: {}", e.detail),
            ));
            return finish(row, reasons, notes);
        }
    };
    row["changes"] = json!(status.changes);
    row["untracked"] = json!(status.untracked);
    row["changed_files"] = json!(status
        .entries
        .iter()
        .filter(|s| !s.starts_with("!!"))
        .take(100)
        .collect::<Vec<_>>());
    if status.changes > 0 {
        reasons.push(reason(
            "changes",
            format!("{} not committed.", plural(status.changes, "changed file")),
        ));
    }
    if status.untracked > 0 {
        reasons.push(reason(
            "untracked",
            format!("{}.", plural(status.untracked, "untracked file")),
        ));
    }

    let metadata = git(
        &path,
        &["rev-parse", "--absolute-git-dir", "--git-path", "modules"],
        step(total, 5),
    )
    .map(text)
    .unwrap_or_default();
    let mut lines = metadata.lines().map(PathBuf::from);
    let gitdir = lines.next();
    let modules = lines
        .next()
        .map(|m| if m.is_absolute() { m } else { path.join(m) });
    if modules.is_some_and(|m| fs::read_dir(m).is_ok_and(|mut entries| entries.next().is_some())) {
        reasons.push(reason(
            "submodules",
            "Has initialized submodules; Git would require force.",
        ));
    }
    let activity = gitdir
        .map(|dir| {
            ["HEAD", "index", "logs/HEAD"]
                .iter()
                .map(|name| modified(&dir.join(name)))
                .fold(0., f64::max)
        })
        .unwrap_or(0.);
    row["last_activity"] = json!(activity);

    if tree["detached"] == true && !head.is_empty() {
        match git(
            &path,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "--contains",
                &head,
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ],
            step(total, 5),
        ) {
            Ok(found) if found.iter().all(u8::is_ascii_whitespace) => reasons.push(reason(
                "detached_unreachable",
                "The detached commit is not on any branch, tag or remote branch.",
            )),
            Ok(_) => {}
            Err(e) => reasons.push(reason(
                "status_unavailable",
                format!("Cannot check the detached commit: {}", e.detail),
            )),
        }
    }

    let mut merged = None;
    let mut integration = json!({"kind":"unknown"});
    for (name, base) in &context.bases {
        match ancestor(&path, &head, base, step(total, 5)) {
            Some(true) => {
                merged = Some(true);
                integration = json!({"kind":"ancestor","base":name,"ref":base});
                break;
            }
            Some(false) => merged = Some(false),
            None => {}
        }
    }
    row["merged"] = json!(merged);
    // Equivalent content is useful cleanup evidence, but never permission to
    // delete the original branch: `merged` remains ancestry-only.
    if merged != Some(true) {
        for (name, base) in &context.bases {
            if git(
                &path,
                &[
                    "diff",
                    "--quiet",
                    "--no-ext-diff",
                    "--no-textconv",
                    &head,
                    base,
                    "--",
                ],
                step(total, 3),
            )
            .is_ok()
            {
                integration = json!({"kind":"same_tree","base":name,"ref":base});
                break;
            }
            if let Ok(cherry) = git(&path, &["cherry", base, &head], step(total, 3)) {
                let cherry = text(cherry);
                if !cherry.is_empty() && cherry.lines().all(|line| line.starts_with("- ")) {
                    integration = json!({"kind":"equivalent_patches","base":name,"ref":base});
                    break;
                }
            }
        }
    }
    if integration["kind"] == "unknown" && merged == Some(false) {
        if let Some((name, base)) = context.bases.first() {
            integration = json!({"kind":"remaining","base":name,"ref":base});
        }
    }
    if let Some(base) = integration["ref"].as_str() {
        integration["base_head"] = git(&path, &["rev-parse", "--verify", base], step(total, 2))
            .ok()
            .map(text)
            .map_or(Value::Null, Value::String);
    }
    integration["remote_freshness"] = json!("local_refs_only");
    row["integration"] = integration.clone();
    row["ahead"] = context
        .bases
        .first()
        .and_then(|(_, base)| {
            git(
                &path,
                &["rev-list", "--count", &format!("{base}..{head}")],
                step(total, 5),
            )
            .ok()
        })
        .and_then(|count| text(count).parse::<u64>().ok())
        .map_or(Value::Null, |count| json!(count));
    let upstream = if branch.is_empty() {
        None
    } else {
        git(
            &path,
            &["rev-parse", "--symbolic-full-name", "@{upstream}"],
            step(total, 5),
        )
        .ok()
        .map(text)
        .filter(|u| !u.is_empty())
    };
    row["pushed"] =
        json!(upstream
            .as_ref()
            .and_then(|u| ancestor(&path, &head, u, step(total, 5))));
    row["upstream"] = json!(upstream.map(|u| u.trim_start_matches("refs/remotes/").to_owned()));

    let deadline = (Instant::now() + WALK_TIME).min(total);
    let mut entries = 0;
    let mut ignored: Vec<(String, u64, bool, String)> = status
        .ignored
        .iter()
        .map(|name| {
            let (bytes, complete, digest) = measure(&path.join(name), &mut entries, deadline);
            (name.clone(), bytes, complete, digest)
        })
        .collect();
    row["ignored_bytes"] = json!(ignored.iter().map(|(_, bytes, _, _)| bytes).sum::<u64>());
    row["ignored_complete"] = json!(ignored.iter().all(|(_, _, complete, _)| *complete));
    row["ignored_count"] = json!(ignored.len());
    row["generated_files_only"] = json!(super::worktree_caches::generated_only(&path, &status.ignored, total));
    row["ignored_fingerprint"] = json!(fingerprint(json!(ignored)));
    ignored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    row["ignored"] = json!(ignored
        .iter()
        .take(MAX_IGNORED)
        .map(|(name, bytes, complete, _)| json!({"path":name,"bytes":bytes,"complete":complete}))
        .collect::<Vec<_>>());
    if !ignored.is_empty() {
        notes.push(reason(
            "ignored_data",
            "Local files outside Git will be deleted. Review them before selecting this folder.",
        ));
        if row["ignored_complete"] != true {
            reasons.push(reason(
                "ignored_unavailable",
                "Local files could not be fully checked. Review again before removal.",
            ));
        }
    }
    let (bytes, complete, blockers) =
        inventory(&path, &nested, (Instant::now() + WALK_TIME).min(total));
    reasons.extend(blockers);
    if !complete {
        reasons.push(reason(
            "local_inventory_unavailable",
            "Local files could not be fully checked. Review again before removal.",
        ));
    }
    row["disk_bytes"] = json!(bytes);
    row["disk_complete"] = json!(complete);

    let base = context
        .bases
        .first()
        .map_or("the base branch", |(name, _)| name.as_str());
    match merged {
        Some(false) if integration["kind"] == "remaining" => notes.push(reason(
            "not_merged",
            match row["ahead"].as_u64() {
                Some(count) => format!(
                    "{} not in {base}. Removing the worktree keeps the branch.",
                    plural(count as usize, "commit")
                ),
                None => {
                    format!("Has commits not in {base}. Removing the worktree keeps the branch.")
                }
            },
        )),
        None if context.bases.is_empty() => notes.push(reason(
            "no_base",
            "No base branch was found, so the merge state is unknown.",
        )),
        None if integration["kind"] == "unknown" => reasons.push(reason(
            "integration_unavailable",
            "Integration could not be checked. Review again before removal.",
        )),
        _ => {}
    }
    if archived > 0 {
        notes.push(reason(
            "archived_sessions",
            format!(
                "{} refer to this folder and could not be restored there.",
                plural(archived, "archived session")
            ),
        ));
    }
    if now() - activity < RECENT {
        notes.push(reason("recent", "Used in the last 24 hours."));
    }
    if status.changes > 0 || status.untracked > 0 {
        // Lossy Git names cannot authorize deleting a different filesystem name.
        if !status.entries.iter().any(|s| s.contains('\u{fffd}')) {
            row["discard_fingerprint"] = json!(discard_proof(
                &path,
                &status,
                (Instant::now() + WALK_TIME).min(total)
            ));
        }
        row["discardable"] = json!(
            row["discard_fingerprint"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
                && reasons
                    .iter()
                    .all(|r| matches!(string(r, "code"), "changes" | "untracked"))
        );
    }
    row["fingerprint"] = json!(fingerprint(json!({"path":path,"head":head,"branch":branch,"owner":row["ownership_id"],
        "locked":row["locked"],"prunable":row["prunable"],"available":true,
        "status":status.entries,"nested":row["nested"],"sessions":row["sessions"],
        "ignored":row["ignored_fingerprint"],"discard":row["discard_fingerprint"]})));
    // A complete roster with only stable access-denied paths permits a narrow
    // manual exception. All other blockers (including any readable use) win,
    // except changes the person may also discard: then removal needs both
    // explicit consents (zerus-k2ga).
    let discard_offered = row["discard_fingerprint"].as_str().is_some_and(|s| !s.is_empty());
    let codes: Vec<&str> = reasons.iter().map(|r| string(r, "code")).collect();
    let changed = codes.iter().any(|code| matches!(*code, "changes" | "untracked"));
    if context.processes.has_permission_denials()
        && codes.contains(&"processes_unavailable")
        && codes.iter().all(|code| *code == "processes_unavailable"
            || (discard_offered && matches!(*code, "changes" | "untracked")))
    {
        row["process_fingerprint"] = json!(process_consent(&row, &context.common, now() as u64));
        if changed {
            // Older clients read `discardable` alone, so it stays false here.
            row["discardable_with_process_consent"] = json!(true);
        }
    }
    finish(row, reasons, notes)
}

/// Assessment of the checkouts of the repository containing `path`; `only`
/// limits it to one worktree, which may already be missing.
enum Settled {
    Removed,
    Registered,
    Partial,
    Unknown(String),
}

/// What a removal left, from a fresh worktree list and a no-follow check of
/// the folder. A missing registration alone never proves complete deletion.
fn settle(common: &Path, path: &Path) -> Settled {
    settle_until(common, path, Instant::now() + Duration::from_secs(5))
}
fn settle_until(common: &Path, path: &Path, until: Instant) -> Settled {
    let (trees, partial) = match list(common, common, until) {
        Ok(listed) => listed,
        Err(e) => return Settled::Unknown(e.detail),
    };
    if trees.iter().any(|tree| tree["path"] == json!(path)) {
        return Settled::Registered;
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settled::Removed,
        // A truncated list could omit a still-registered worktree.
        Ok(_) if partial => {
            Settled::Unknown("The worktree list was incomplete; review the folder again.".into())
        }
        Ok(_) => Settled::Partial,
        Err(e) => Settled::Unknown(e.to_string()),
    }
}

pub(super) fn repository_lock(common: &Path) -> Result<std::fs::File> {
    let cache = cache_file("repo", &common.to_string_lossy());
    private_dir(cache.parent().unwrap())?;
    crate::platform::private_open(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    )
    .open(cache.with_extension("lock"))
    .map_err(|e| e.to_string())
}

/// Folders a recorded removal left behind: Git no longer tracks them, yet they
/// exist. Each row is a fresh, bounded review of the remaining files, the
/// retained branch and current occupants. Records whose folder is gone are
/// resolved; a record whose worktree is still registered is resolved only when
/// no removal holds the repository lock. Nothing here deletes or retries.
/// At most `LEFTOVER_ROWS` rows within `LEFTOVER_BYTES`; `true` when more
/// records remain unreviewed. A targeted review (`only`, as given and
/// resolved) looks at that worktree's own record alone.
fn leftovers(
    common: &Path,
    trees: &[Value],
    listing_partial: bool,
    context: &Context,
    total: Instant,
    only: Option<&(PathBuf, PathBuf)>,
) -> Result<(Vec<Value>, bool)> {
    let mut rows = Vec::new();
    let mut used = 0;
    let mut cut = false;
    for record in super::worktree_maintenance::removals(common)? {
        let path = PathBuf::from(&record.path);
        if only.is_some_and(|(raw, resolved)| &path != raw && &path != resolved) {
            continue;
        }
        let registered = trees.iter().any(|tree| tree["path"] == json!(path));
        // A missing folder resolves only when it is gone for good; on an
        // unmounted or unidentifiable volume it stays as an uncertain row.
        let folder = record.volumes.as_ref().map(|v| v.folder.as_str());
        if !registered && super::worktree_maintenance::vanished(&record.path, folder) {
            super::worktree_maintenance::forget_removal(&record.path)?;
            continue;
        }
        if cut || rows.len() >= LEFTOVER_ROWS {
            cut = true;
            continue;
        }
        if registered {
            // The review's list can be seconds old. Only a fresh list, taken
            // while no removal holds the repository lock, may resolve the
            // record; a folder Git stopped tracking meanwhile is shown.
            let lock = repository_lock(common)?;
            if lock.try_lock_exclusive().is_err() {
                continue;
            }
            let _lock = crate::platform::FileLock::held(lock);
            #[cfg(all(test, unix))]
            crate::platform::lock_tests::reached("leftovers");
            let Ok((fresh, partial)) = list(common, common, step(total, 5)) else {
                continue;
            };
            if fresh.iter().any(|tree| tree["path"] == json!(path)) {
                super::worktree_maintenance::forget_removal(&record.path)?;
                continue;
            }
            if partial {
                continue;
            }
        }
        let row = leftover(&record, common, listing_partial, context, total);
        if admit_leftover(&mut used, &row) {
            rows.push(row);
        } else {
            cut = true;
        }
    }
    Ok((rows, cut))
}

fn leftover(
    record: &super::worktree_maintenance::Removal,
    common: &Path,
    listing_partial: bool,
    context: &Context,
    total: Instant,
) -> Value {
    let path = PathBuf::from(&record.path);
    let mut reasons = Vec::new();
    if macos_leftover_deletion_blocked() {
        reasons.push(reason("macos_appledouble_safety", MACOS_LEFTOVER_REASON));
    }
    let mut notes = vec![reason(
        "partially_removed",
        "Git no longer tracks this folder, but files remain. Review them; the removal is never retried.",
    )];
    let mut row = json!({"path":path,"kind":"leftover","available":true,"branch":record.branch,
        "head":record.head,"common_dir":common,"removal":record});
    let reference = format!("refs/heads/{}", record.branch);
    let branch_head = (!record.branch.is_empty())
        .then(|| {
            git(
                common,
                &["rev-parse", "--verify", "--end-of-options", &reference],
                step(total, 2),
            )
            .ok()
            .map(text)
        })
        .flatten();
    row["branch_head"] = json!(branch_head);
    // Unknown (no branch, Git failed or the budget ran out) is null, not moved.
    row["branch_retained"] = match &branch_head {
        Some(head) => json!(head == &record.head),
        None => Value::Null,
    };
    if listing_partial {
        notes.push(reason(
            "status_unavailable",
            "The worktree list was incomplete; Git may still track this folder.",
        ));
    }
    // An incomplete inventory is unknown use, never an empty set of sessions.
    // Keep the same completeness gate as ordinary worktree assessment.
    if !context.bindings_complete {
        reasons.push(reason(
            "sessions_unavailable",
            "Saved sessions could not be checked. Review again before removal.",
        ));
    }
    occupants(&path, context, &mut row, &mut reasons);
    let scan = scan_leftover(&path, step(total, WALK_TIME.as_secs()));
    #[cfg(test)]
    AFTER_LEFTOVER_SCAN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook()
        }
    });
    let complete = scan.complete;
    let manifest = scan.manifest;
    row["size"] = json!(scan.bytes);
    row["size_complete"] = json!(complete);
    if !complete {
        reasons.push(reason(
            "status_unavailable",
            "The remaining files could not be fully listed. Review again later.",
        ));
    }
    reasons.extend(scan.blockers);
    // Keep the identity captured before the walk; a later replacement must
    // not acquire the old manifest or confirmation.
    row["root_identity"] = scan.root.as_ref().map_or(Value::Null, RootIdentity::json);
    // Hidden processes elsewhere come and go; like worktree removal, their
    // count is not part of what the owner confirms.
    let stable: Vec<_> = reasons
        .iter()
        .filter(|reason| reason["code"] != "processes_unavailable")
        .collect();
    row["fingerprint"] = json!(fingerprint(json!({"record":record,"manifest":manifest,
        "complete":complete,"branch_head":row["branch_head"],"processes":row["processes"],
        "sessions":row["sessions"],"reasons":stable,"root":row["root_identity"]})));
    // The same narrow manual exception as worktree removal: a complete scan
    // whose only uncertainty is processes with unreadable folders.
    if context.processes.has_permission_denials()
        && reasons.len() == 1
        && reasons[0]["code"] == "processes_unavailable"
    {
        row["process_fingerprint"] = json!(process_consent(&row, common, now() as u64));
    }
    row["deletable"] = json!(reasons.is_empty() && !listing_partial);
    let mut row = finish(row, reasons, notes);
    row["verdict"] = json!("partially_removed");
    row
}

/// One bounded view of the remaining files and everything that blocks
/// deletion. A skipped or unreadable subtree always leaves size incomplete.
struct LeftoverScan {
    bytes: u64,
    complete: bool,
    manifest: String,
    blockers: Vec<Value>,
    root: Option<RootIdentity>,
    #[cfg(all(test, unix))]
    queued: usize,
}

fn scan_leftover(root: &Path, deadline: Instant) -> LeftoverScan {
    scan_leftover_limit(root, deadline, WALK_ENTRIES)
}

fn scan_leftover_limit(root: &Path, deadline: Instant, limit: usize) -> LeftoverScan {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    let mut scan = LeftoverScan {
        bytes: 0,
        complete: true,
        manifest: String::new(),
        blockers: Vec::new(),
        root: None,
        #[cfg(all(test, unix))]
        queued: 0,
    };
    #[cfg(not(unix))]
    scan.blockers.push(reason(
        "unsupported_platform",
        "Deleting leftover files is currently available on Linux only. Keep the folder or delete it manually.",
    ));
    let mut manifest = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let (mut visited, mut queued) = (0, 1);
    #[cfg(unix)]
    let unidentified = |path: &Path| {
        reason(
            "mount_unavailable",
            format!(
                "The filesystem of {} cannot be identified, so deletion is refused.",
                path.display()
            ),
        )
    };
    while let Some(path) = pending.pop() {
        if visited >= limit || Instant::now() >= deadline || scan.blockers.len() >= MAX_SAMPLES {
            scan.complete = false;
            break;
        }
        visited += 1;
        #[cfg(test)]
        trace_measured(&path);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                scan.complete = false;
                continue;
            }
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        manifest.insert(
            path.to_string_lossy().into_owned(),
            json!({
                "size":metadata.len(), "modified":modified.map(|t| t.as_nanos().to_string()),
                "directory":metadata.is_dir(), "link":fs::read_link(&path).ok()
            }),
        );
        if path == root && !metadata.is_dir() {
            scan.blockers.push(reason(
                "not_a_folder",
                "The leftover is not a plain folder.",
            ));
        }
        let git = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".git"));
        if git && (path.parent() != Some(root) || metadata.is_dir()) {
            scan.blockers.push(reason(
                "nested_repository",
                format!(
                    "Contains a Git repository: {}. Move it out first.",
                    path.display()
                ),
            ));
            scan.complete = false;
            continue;
        }
        if metadata.is_dir() {
            #[cfg(unix)]
            {
                let id = match root_identity(&path) {
                    Ok(id) if id.dev == metadata.dev() && id.ino == metadata.ino() => id,
                    Ok(_) => {
                        scan.blockers.push(reason(
                            "folder_changed",
                            "The remaining folder changed while it was being listed. Review again.",
                        ));
                        scan.complete = false;
                        continue;
                    }
                    Err(_) => {
                        scan.blockers.push(unidentified(&path));
                        scan.complete = false;
                        continue;
                    }
                };
                if path == root {
                    scan.root = Some(id.clone());
                    match root.parent().map(root_identity) {
                        Some(Ok(parent)) if parent.dev == id.dev && parent.mount == id.mount => (),
                        Some(Ok(_)) => {
                            scan.blockers.push(reason(
                                "mount_point",
                                "The leftover folder is a mount point. Unmount it first.",
                            ));
                            scan.complete = false;
                            continue;
                        }
                        _ => {
                            scan.blockers.push(unidentified(root));
                            scan.complete = false;
                            continue;
                        }
                    }
                }
                let Some(scope) = &scan.root else {
                    scan.complete = false;
                    continue;
                };
                if id.dev != scope.dev || id.mount != scope.mount {
                    scan.blockers.push(reason(
                        "other_filesystem",
                        format!(
                            "{} is on another filesystem or mount. Move it out first.",
                            path.display()
                        ),
                    ));
                    scan.complete = false;
                    continue;
                }
                if metadata.uid() != uid {
                    scan.blockers.push(reason(
                        "foreign_owner",
                        format!("{} belongs to another user.", path.display()),
                    ));
                    scan.complete = false;
                    continue;
                }
            }
            match fs::read_dir(&path) {
                Ok(children) => {
                    for child in children {
                        // Bound enumeration as well as visited nodes: a very wide
                        // folder cannot fill an unbounded pending queue first.
                        if queued >= limit || Instant::now() >= deadline {
                            scan.complete = false;
                            break;
                        }
                        match child {
                            Ok(child) => {
                                queued += 1;
                                pending.push(child.path());
                            }
                            Err(_) => {
                                queued += 1;
                                scan.complete = false;
                            }
                        }
                    }
                }
                Err(_) => scan.complete = false,
            }
        } else {
            match scan.bytes.checked_add(metadata.len()) {
                Some(bytes) => scan.bytes = bytes,
                None => {
                    scan.complete = false;
                    break;
                }
            }
            if !metadata.is_file() && !metadata.file_type().is_symlink() {
                scan.blockers.push(reason(
                    "special_files",
                    format!(
                        "Contains a special file: {}. Remove or move it explicitly first.",
                        path.display()
                    ),
                ));
            }
        }
        if scan.blockers.len() >= MAX_SAMPLES {
            scan.complete = false;
            break;
        }
    }
    #[cfg(unix)]
    if scan
        .root
        .as_ref()
        .is_some_and(|expected| root_identity(root).as_ref().ok() != Some(expected))
    {
        scan.complete = false;
        if scan.blockers.len() < MAX_SAMPLES {
            scan.blockers.push(reason(
                "folder_changed",
                "The remaining folder changed while it was being listed. Review again.",
            ));
        }
    }
    #[cfg(all(test, unix))]
    {
        scan.queued = queued;
    }
    scan.manifest = fingerprint(json!(manifest));
    scan
}

/// Same repository -> state lock order as guarded removal. Hold these across
/// the final membership/proof checks and one signal, so a new saved/live
/// binding or managed worktree mutation cannot race that original decision.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) struct ProgramStopGuards {
    _repository: crate::platform::FileLock,
    _state: storage::Guard,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn program_stop_guards(
    capture: &zerus_client_core::termination_cleanup::Capture,
    until: Instant,
) -> Result<ProgramStopGuards> {
    let repository = repository_lock(Path::new(&capture.common_dir))?;
    loop {
        match repository.try_lock_exclusive() {
            Ok(()) => break,
            Err(e) if !crate::platform::lock_contended(&e) => return Err(e.to_string()),
            Err(_) if Instant::now() >= until || crate::platform::shutdown_signal() != 0 => {
                return Err("The working folder is busy".into())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let repository = crate::platform::FileLock::held(repository);
    #[cfg(all(test, unix))]
    crate::platform::lock_tests::reached("program-stop");
    let state = storage::writer_until(until)?.ok_or("Session state is busy")?;
    Ok(ProgramStopGuards {
        _repository: repository,
        _state: state,
    })
}

/// Existing session/Git/launch protections for the explicit program-stop
/// phase. This grants no deletion permission and does not relax assessment,
/// process review, ignored-file preservation or branch/archive restoration.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn program_stop_check(
    capture: &zerus_client_core::termination_cleanup::Capture,
    until: Instant,
) -> Result<()> {
    let fail =
        || "The working folder is protected or changed; review the original action".to_owned();
    if Instant::now() >= until || crate::platform::shutdown_signal() != 0 {
        return Err(fail());
    }
    let path = Path::new(&capture.root);
    let common = Path::new(&capture.common_dir);
    let owned = super::worktree_lifecycle::owned(common, path).ok_or_else(fail)?;
    if owned["id"] != capture.owner_id
        || owned["path"] != capture.root
        || owned["common_dir"] != capture.common_dir
    {
        return Err(fail());
    }
    let (trees, partial) = list(path, common, until).map_err(|_| fail())?;
    let selected = trees
        .iter()
        .find(|row| row["path"] == capture.root)
        .ok_or_else(fail)?;
    if partial
        || selected["kind"] != "linked"
        || selected["available"] != true
        || selected["locked"] == true
        || selected["prunable"] == true
        || selected["branch"] != owned["branch"]
        || trees.iter().any(|row| {
            row["path"]
                .as_str()
                .is_some_and(|other| Path::new(other) != path && Path::new(other).starts_with(path))
        })
    {
        return Err(fail());
    }
    // An archived original is not a saved/live occupant. A new same-name
    // session or any other retained binding still protects its actual folder.
    if bindings()?
        .iter()
        .any(|record| record.folders.iter().any(|folder| folder.starts_with(path)))
    {
        return Err(fail());
    }
    if Instant::now() >= until || crate::platform::shutdown_signal() != 0 {
        return Err(fail());
    }
    Ok(())
}

pub(super) fn review(path: &Path, only: Option<&Path>, total: Instant) -> Value {
    review_with_processes(path, only, total, None)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)] // Read-only prerequisite; the private executing adapter follows.
pub(super) fn program_cleanup_review(
    intent: &zerus_client_core::worktree_programs::cleanup::Intent,
    loan: &crate::platform::worktree_membership::objects::loan::borrower::Borrowed,
) -> Result<Value> {
    loan.current_cleanup(intent)
        .map_err(|error| error.to_string())?;
    let programs = loan
        .programs_cleanup(intent)
        .map_err(|error| error.to_string())?;
    let scope = ProcessReview::NativeObjects {
        path: PathBuf::from(&intent.input.stop.manifest.capture.root),
        checkout: loan.scope().identity().clone(),
        programs,
        until: loan.deadline(),
    };
    let result = review_with_processes(
        Path::new(&intent.input.stop.manifest.capture.common_dir),
        Some(Path::new(&intent.input.stop.manifest.capture.root)),
        loan.deadline(),
        Some(scope),
    );
    loan.current_cleanup(intent)
        .map_err(|error| error.to_string())?;
    Ok(result)
}

fn review_with_processes(
    path: &Path,
    only: Option<&Path>,
    total: Instant,
    native: Option<ProcessReview>,
) -> Value {
    let machine = crate::config::Config::load()
        .map(|c| c.host().to_owned())
        .unwrap_or_default();
    let Ok(path) = crate::platform::canonical(path)
        .map_err(|_| ())
        .and_then(|p| if p.is_dir() { Ok(p) } else { Err(()) })
    else {
        return json!({"path":path,"state":"folder_unavailable","detail":"Folder is missing or inaccessible",
            "worktrees":[],"machine":machine,"sampled_at":now()});
    };
    let result = (|| {
        let common = git(
            &path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            step(total, 3),
        )?;
        let common = crate::platform::canonical(text(common))
            .map_err(|e| error("metadata_error", e.to_string()))?;
        let (trees, partial) = list(&path, &common, step(total, 3))?;
        let context = context_with_processes(&common, &trees, step(total, 3), native);
        let wanted = only.map(|only| (only.to_path_buf(), resolved(only)));
        let mut reviewed = 0;
        let mut cut = false;
        let mut rows = Vec::new();
        for tree in &trees {
            let tree_path = Path::new(tree["path"].as_str().unwrap_or(""));
            if let Some((raw, resolved)) = &wanted {
                if tree_path != raw && tree_path != resolved {
                    continue;
                }
            }
            if tree["kind"] == "linked" {
                reviewed += 1;
                if reviewed > MAX_REVIEWED {
                    cut = true;
                    let mut row = tree.clone();
                    row["fingerprint"] = json!("");
                    rows.push(finish(
                        row,
                        vec![reason(
                            "status_unavailable",
                            "Not reviewed: too many worktrees.",
                        )],
                        vec![],
                    ));
                    continue;
                }
            }
            rows.push(assess(tree, &trees, &context, total));
        }
        let base: Vec<_> = context.bases.iter().map(|(name, _)| name.clone()).collect();
        let mut value = json!({"state":"ok","path":path,"common_dir":common,"base":base,
            "partial":partial || cut,"worktrees":rows,"machine":machine,"sampled_at":now()});
        // A targeted review reports only that worktree's leftover, so a
        // removal that stopped partway is visible where it was asked about.
        match leftovers(&common, &trees, partial, &context, total, wanted.as_ref()) {
            Ok((rows, cut)) => {
                value["leftovers"] = json!(rows);
                value["leftovers_partial"] = json!(cut);
            }
            Err(detail) => {
                value["leftovers"] = json!([]);
                value["leftovers_unavailable"] = json!(detail);
            }
        }
        Ok::<_, GitError>(value)
    })();
    result.unwrap_or_else(|e| {
        json!({"path":path,"state":e.state,"detail":e.detail,"worktrees":[],"machine":machine,"sampled_at":now()})
    })
}

pub(super) fn review_command(args: &[String]) -> Result<Value> {
    let mut path = None;
    let mut only = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            key @ ("--path" | "--worktree") => {
                i += 1;
                let value =
                    PathBuf::from(args.get(i).ok_or_else(|| format!("{key} needs a folder"))?);
                if !value.is_absolute() {
                    return Err(format!("{key} must be an absolute folder path"));
                }
                if key == "--path" {
                    path = Some(value);
                } else {
                    only = Some(value);
                }
            }
            _ => {
                return Err(
                    "usage: hgs worktrees review --path PATH [--worktree WORKTREE] [--json]".into(),
                )
            }
        }
        i += 1;
    }
    let path = path.ok_or("--path is required")?;
    Ok(review(
        &path,
        only.as_deref(),
        Instant::now() + Duration::from_secs(20),
    ))
}

/// A leftover whose repository no longer exists: review cannot list it, but it
/// still counts toward the bound, so it is listed here for keep-folder.
fn orphan_row(
    record: &super::worktree_maintenance::Removal,
    context: &Context,
    total: Instant,
) -> Value {
    let mut row = leftover(record, Path::new(&record.common_dir), false, context, total);
    row["repository_available"] = json!(false);
    row
}

/// `hgs worktrees leftovers`: records whose repository is gone. Those whose
/// folder is gone too are resolved first. Bounded like a review's leftovers;
/// `partial` when more records remain.
pub(super) fn leftovers_command(args: &[String]) -> Result<Value> {
    if !args.iter().all(|arg| arg == "--json") {
        return Err("usage: hgs worktrees leftovers [--json]".into());
    }
    let total = Instant::now() + Duration::from_secs(20);
    let records = super::worktree_maintenance::orphan_removals()?;
    let context = context(Path::new("/"), &[], step(total, 3));
    let mut rows = Vec::new();
    let mut used = 0;
    let mut partial = false;
    for record in &records {
        if rows.len() >= LEFTOVER_ROWS {
            partial = true;
            break;
        }
        let row = orphan_row(record, &context, total);
        if !admit_leftover(&mut used, &row) {
            partial = true;
            break;
        }
        rows.push(row);
    }
    let machine = crate::config::Config::load()
        .map(|c| c.host().to_owned())
        .unwrap_or_default();
    Ok(json!({"schema":1,"leftovers":rows,"partial":partial,"sampled_at":now(),"machine":machine}))
}

/// Explicit, guarded deletion of a leftover's remaining files. Requires the
/// owner's confirmation and the fingerprint of the fresh review they saw, and
/// re-checks everything under the repository and session locks. The branch is
/// never touched. A failure after deletion starts keeps the record for review
/// and is never retried.
pub(super) fn delete_leftover_command(args: &[String]) -> (Result<Value>, bool) {
    let mut attempted = false;
    // Refuse before even reserving an attempt: a known platform refusal is
    // the existing kept/v1 outcome, with no partial filesystem effect.
    let result = leftover_deletion_guard()
        .map_err(|error| error.to_string())
        .and_then(|()| delete_leftover(args, &mut attempted));
    (result, attempted)
}

fn delete_leftover(args: &[String], attempted: &mut bool) -> Result<Value> {
    const USAGE: &str = "usage: hgs worktrees delete-leftover --path FOLDER --common-dir DIR --fingerprint HASH --confirm-delete [--process-fingerprint HASH] [--request-id ID] [--json]";
    let mut options = BTreeMap::new();
    let mut confirmed = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            "--confirm-delete" => confirmed = true,
            key @ ("--path"
            | "--common-dir"
            | "--fingerprint"
            | "--process-fingerprint"
            | "--request-id") => {
                i += 1;
                let value = args.get(i).ok_or_else(|| format!("{key} needs a value"))?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err(format!(
                        "{key} cannot be empty or contain control characters"
                    ));
                }
                if options.insert(key, value.as_str()).is_some() {
                    return Err(format!("duplicate {key}"));
                }
            }
            _ => return Err(USAGE.into()),
        }
        i += 1;
    }
    if !confirmed {
        return Err("Deleting leftover files requires --confirm-delete.".into());
    }
    let required = |key| {
        options
            .get(key)
            .copied()
            .ok_or_else(|| format!("{key} is required"))
    };
    let target = Path::new(required("--path")?);
    let common = Path::new(required("--common-dir")?);
    let expected = required("--fingerprint")?;
    if !target.is_absolute() || !common.is_absolute() {
        return Err("Folder and repository must be absolute paths".into());
    }
    let common = crate::platform::canonical(common).map_err(|e| format!("Repository: {e}"))?;
    let actual = git(
        &common,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        Instant::now() + Duration::from_secs(5),
    )
    .map_err(|e| e.detail)?;
    if crate::platform::canonical(text(actual)).ok().as_ref() != Some(&common) {
        return Err("Repository changed. Review the worktrees again.".into());
    }
    let guard = repository_lock(&common)?;
    let waited = Instant::now() + Duration::from_secs(10);
    while guard.try_lock_exclusive().is_err() {
        if Instant::now() >= waited {
            return Err(
                "Another worktree operation is running. Try again after it finishes.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _lock = crate::platform::FileLock::held(guard);
    // The same lock order as removal: a new session binding cannot race it.
    let _state_lock = storage::writer_until(Instant::now() + Duration::from_secs(10))?
        .ok_or("Session state is busy. Try again after the current operation finishes.")?;
    let resolved = crate::platform::canonical(target).ok();
    let record = super::worktree_maintenance::removals(&common)?
        .into_iter()
        .find(|r| Path::new(&r.path) == target || resolved.as_deref() == Some(Path::new(&r.path)))
        .ok_or("No partial-removal record exists for this folder. Review the worktrees again.")?;
    let total = Instant::now() + Duration::from_secs(20);
    let (trees, partial) = list(&common, &common, step(total, 5)).map_err(|e| e.detail)?;
    let path = PathBuf::from(&record.path);
    if trees.iter().any(|tree| tree["path"] == json!(path)) {
        return Err("Git tracks this folder again. Review it as a worktree.".into());
    }
    if partial {
        return Err("The worktree list was incomplete. Review the folder again.".into());
    }
    let context = context(&common, &trees, step(total, 3));
    let row = leftover(&record, &common, partial, &context, total);
    if row["fingerprint"] != expected {
        return Err("The leftover folder changed since the review. Review it again.".into());
    }
    let consent = options.get("--process-fingerprint").copied();
    if row["deletable"] != true
        && !consent.is_some_and(|proof| valid_process_consent(proof, &row, &common, now() as u64))
    {
        let reasons: Vec<_> = row["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| string(r, "message").to_owned())
            .collect();
        return Err(format!(
            "Cannot delete {}: {}",
            path.display(),
            reasons.join(" ")
        ));
    }
    let expected = RootIdentity::from_json(&row["root_identity"])
        .ok_or("The leftover folder cannot be identified. Review it again.")?;
    // The confirmation is single-use even if deletion changes no files and
    // recording its stopped outcome fails. Keep this in the existing record
    // so older readers also see a changed fingerprint. Failure to reserve it
    // refuses before any filesystem change.
    let attempt = uuid::Uuid::new_v4();
    let pending = format!(
        "Deleting remaining files started: attempt {attempt}. Review the folder again if the outcome is unknown."
    );
    super::worktree_maintenance::settle_removal(&record, Some(&pending))?;
    *attempted = true;
    // Completion is confirmed through the parent's descriptor, never by path.
    match delete_tree(&path, &expected, Instant::now() + Duration::from_secs(300)) {
        Ok(()) => {
            super::worktree_maintenance::forget_removal(&record.path)?;
            Ok(json!({"status":"deleted","path":path,"common_dir":common,
                "bytes":row["size"],"branch":record.branch,"request_id":options.get("--request-id"),
                "machine":crate::config::Config::load().map(|c| c.host().to_owned()).unwrap_or_default()}))
        }
        Err(e) => {
            let detail = format!("Deleting the leftover stopped: {e}");
            // Keep the unique reference before any possibly long error text:
            // the bounded detail must never restore a previous confirmation.
            let stored = format!("Deleting the leftover stopped: attempt {attempt}; {e}");
            let _ = super::worktree_maintenance::settle_removal(&record, Some(&stored));
            Err(format!(
                "{detail}\nSome files may remain; review the folder again. The deletion is never retried."
            ))
        }
    }
}

/// The identity of a reviewed leftover root: device, inode and the mount that
/// holds it (Linux mount ID, macOS filesystem ID and mount point).
#[derive(Clone, Debug, PartialEq, Eq)]
struct RootIdentity {
    dev: u64,
    ino: u64,
    mount: String,
}

impl RootIdentity {
    fn json(&self) -> Value {
        json!({"dev":self.dev.to_string(),"ino":self.ino.to_string(),"mount":self.mount})
    }
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            dev: value["dev"].as_str()?.parse().ok()?,
            ino: value["ino"].as_str()?.parse().ok()?,
            mount: value["mount"].as_str()?.to_owned(),
        })
    }
}

/// Opens `path`'s final component without following a link.
#[cfg(unix)]
fn root_identity(path: &Path) -> std::io::Result<RootIdentity> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    deletion::identify(&deletion::open(libc::AT_FDCWD, &name)?)
}

/// Deletes the reviewed tree without following links or leaving its mount.
/// The parent is opened component by component without following links, and
/// the root must be the exact reviewed directory (device, inode, mount). Every
/// folder is opened with O_NOFOLLOW relative to its parent's descriptor and
/// checked through that descriptor (owner, device, mount) before anything in
/// it changes; only then is owner access granted, which is what readonly
/// folders denied to Git. Completion is confirmed through the parent's
/// descriptor.
#[cfg(unix)]
fn delete_tree(root: &Path, expected: &RootIdentity, deadline: Instant) -> std::io::Result<()> {
    leftover_deletion_guard()?;
    use std::ffi::CString;
    use std::io::Error;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    struct Frame {
        fd: std::os::fd::OwnedFd,
        names: Vec<CString>,
        name: Option<CString>,
    }
    let uid = unsafe { libc::geteuid() };
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return Err(Error::other("the leftover has no parent folder"));
    };
    let parent = deletion::open_path(&fs::canonicalize(parent)?)?;
    let name = CString::new(name.as_bytes())?;
    let fd = deletion::open(parent.as_raw_fd(), &name)?;
    if deletion::identify(&fd)? != *expected {
        return Err(Error::other(
            "the leftover folder is not the reviewed folder; deletion stopped",
        ));
    }
    let outer = deletion::identify(&parent)?;
    if outer.dev != expected.dev || outer.mount != expected.mount {
        return Err(Error::other(
            "the leftover folder is a mount point; deletion stopped",
        ));
    }
    deletion::checked(&fd, uid, expected)?;
    let pinned_root = fd.try_clone()?;
    let names = deletion::entries(&fd)?;
    let mut stack = vec![Frame {
        fd,
        names,
        name: None,
    }];
    let mut visited = 0;
    loop {
        visited += 1;
        if Instant::now() >= deadline || visited > 2 * WALK_ENTRIES {
            return Err(Error::new(
                std::io::ErrorKind::TimedOut,
                "the deletion ran out of time",
            ));
        }
        let frame = stack
            .last_mut()
            .expect("the root frame stays until the end");
        let Some(entry) = frame.names.pop() else {
            let done = stack.pop().expect("a frame was just inspected");
            drop(done.fd);
            match (done.name, stack.last()) {
                (Some(entry), Some(above)) => {
                    deletion::unlink(above.fd.as_raw_fd(), &entry, libc::AT_REMOVEDIR)?
                }
                _ => break,
            }
            continue;
        };
        if deletion::is_directory(frame.fd.as_raw_fd(), &entry)? {
            let child = deletion::open(frame.fd.as_raw_fd(), &entry)?;
            deletion::checked(&child, uid, expected).map_err(|e| {
                Error::other(format!(
                    "{}: {e}; deletion stopped",
                    entry.to_string_lossy()
                ))
            })?;
            let names = deletion::entries(&child)?;
            stack.push(Frame {
                fd: child,
                names,
                name: Some(entry),
            });
        } else {
            deletion::unlink(frame.fd.as_raw_fd(), &entry, 0)?;
        }
    }
    #[cfg(test)]
    BEFORE_ROOT_UNLINK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook()
        }
    });
    // unlinkat is by name, so reject an observable replacement immediately
    // before it. Keep the reviewed inode open to prevent inode-number reuse.
    // This cannot make the check and unlink atomic; see the documented limit.
    if deletion::identify(&deletion::open(parent.as_raw_fd(), &name)?)?
        != deletion::identify(&pinned_root)?
    {
        return Err(Error::other(
            "the leftover folder changed before its final removal",
        ));
    }
    deletion::unlink(parent.as_raw_fd(), &name, libc::AT_REMOVEDIR)?;
    #[cfg(test)]
    AFTER_ROOT_UNLINK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook()
        }
    });
    match deletion::is_directory(parent.as_raw_fd(), &name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(Error::other(
            "the deletion could not be confirmed: something else now has that name",
        )),
        Err(e) => Err(e),
    }
}

#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_ROOT_UNLINK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Runs right after the reviewed root is unlinked, before its absence is
    /// confirmed through the parent.
    static AFTER_ROOT_UNLINK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
mod deletion {
    use super::RootIdentity;
    use std::ffi::{CStr, CString};
    use std::io::{Error, Result};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    /// Ancestors are only looked up in, identified and unlinked from, which
    /// needs search permission alone: Linux opens them as paths and macOS for
    /// search, so an execute-only ancestor does not stop the deletion.
    #[cfg(target_os = "linux")]
    const ANCESTOR: libc::c_int = libc::O_PATH;
    #[cfg(target_os = "macos")]
    const ANCESTOR: libc::c_int = libc::O_SEARCH;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const ANCESTOR: libc::c_int = libc::O_RDONLY;

    fn open_as(at: RawFd, name: &CStr, access: libc::c_int) -> Result<OwnedFd> {
        // O_DIRECTORY also refuses the link itself that O_PATH|O_NOFOLLOW
        // would otherwise return.
        let flags = access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(at, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A folder the deletion reads and changes.
    pub(super) fn open(at: RawFd, name: &CStr) -> Result<OwnedFd> {
        open_as(at, name, libc::O_RDONLY)
    }

    /// Opens an absolute, canonical path one component at a time without
    /// following any link, so no component can redirect the result.
    pub(super) fn open_path(path: &Path) -> Result<OwnedFd> {
        let mut fd = open_as(libc::AT_FDCWD, c"/", ANCESTOR)?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    fd = open_as(fd.as_raw_fd(), &CString::new(name.as_bytes())?, ANCESTOR)?;
                }
                _ => return Err(Error::other("unexpected path component")),
            }
        }
        Ok(fd)
    }

    fn stat(fd: &OwnedFd) -> Result<libc::stat> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(stat)
    }

    /// The mount holding an open directory. Unreadable means unknown, which
    /// every caller refuses.
    #[cfg(target_os = "linux")]
    fn mount(fd: &OwnedFd) -> Result<String> {
        let mut stat: libc::statx = unsafe { std::mem::zeroed() };
        let flags = libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW;
        let mask = libc::STATX_MNT_ID | libc::STATX_MNT_ID_UNIQUE;
        if unsafe { libc::statx(fd.as_raw_fd(), c"".as_ptr(), flags, mask, &mut stat) } != 0 {
            return Err(Error::last_os_error());
        }
        if stat.stx_mask & libc::STATX_MNT_ID_UNIQUE != 0 {
            Ok(format!("unique:{}", stat.stx_mnt_id))
        } else if stat.stx_mask & libc::STATX_MNT_ID != 0 {
            Ok(format!("id:{}", stat.stx_mnt_id))
        } else {
            Err(Error::other("the mount identity is unavailable"))
        }
    }

    #[cfg(target_os = "macos")]
    fn mount(fd: &OwnedFd) -> Result<String> {
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(fd.as_raw_fd(), &mut stat) } != 0 {
            return Err(Error::last_os_error());
        }
        // fsid_t keeps its value private; compare its bytes.
        let fsid = unsafe {
            std::slice::from_raw_parts(
                (&stat.f_fsid as *const libc::fsid_t).cast::<u8>(),
                std::mem::size_of::<libc::fsid_t>(),
            )
        };
        let on = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) };
        let fsid: String = fsid.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("{fsid}:{}", on.to_string_lossy()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn mount(_fd: &OwnedFd) -> Result<String> {
        Err(Error::other("the mount identity is unavailable here"))
    }

    pub(super) fn identify(fd: &OwnedFd) -> Result<RootIdentity> {
        let stat = stat(fd)?;
        #[allow(clippy::unnecessary_cast)]
        Ok(RootIdentity {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
            mount: mount(fd)?,
        })
    }

    /// Proves an opened folder is the owner's directory on the reviewed device
    /// and mount, and only then grants the owner access to it.
    pub(super) fn checked(fd: &OwnedFd, uid: libc::uid_t, scope: &RootIdentity) -> Result<()> {
        let stat = stat(fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR || stat.st_uid != uid {
            return Err(Error::other("the leftover folder changed during deletion"));
        }
        #[allow(clippy::unnecessary_cast)]
        if stat.st_dev as u64 != scope.dev || mount(fd)? != scope.mount {
            return Err(Error::other("it is on another filesystem or mount"));
        }
        let owner = 0o700 as libc::mode_t;
        if stat.st_mode & owner != owner
            && unsafe { libc::fchmod(fd.as_raw_fd(), (stat.st_mode & 0o7777) | owner) } != 0
        {
            return Err(Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn is_directory(at: RawFd, name: &CStr) -> Result<bool> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let flags = libc::AT_SYMLINK_NOFOLLOW;
        if unsafe { libc::fstatat(at, name.as_ptr(), &mut stat, flags) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(stat.st_mode & libc::S_IFMT == libc::S_IFDIR)
    }

    pub(super) fn unlink(at: RawFd, name: &CStr, flags: libc::c_int) -> Result<()> {
        // XNU may remove ordinary ._* files while retrying a directory
        // removal. Both nested and root callers must fail before that syscall.
        if flags & libc::AT_REMOVEDIR != 0 {
            super::leftover_deletion_guard()?;
        }
        if unsafe { libc::unlinkat(at, name.as_ptr(), flags) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }

    /// A read error ends the listing early; removing the folder then fails
    /// as not empty, so nothing is skipped silently.
    pub(super) fn entries(fd: &OwnedFd) -> Result<Vec<CString>> {
        let copy = unsafe { libc::dup(fd.as_raw_fd()) };
        if copy < 0 {
            return Err(Error::last_os_error());
        }
        let folder = unsafe { libc::fdopendir(copy) };
        if folder.is_null() {
            let error = Error::last_os_error();
            unsafe { libc::close(copy) };
            return Err(error);
        }
        let mut names = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(folder) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                names.push(name.to_owned());
            }
        }
        unsafe { libc::closedir(folder) };
        Ok(names)
    }
}

#[cfg(not(unix))]
fn delete_tree(_path: &Path, _expected: &RootIdentity, _deadline: Instant) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "deleting leftover files is not supported here",
    ))
}

/// "Keep folder, forget record": the leftover folder and its files stay as
/// they are; only the private removal record goes. Bound under the repository
/// lock to the fresh review the owner confirmed. `true` once forgetting the
/// record may have started: a later failure is not a refusal.
pub(super) fn keep_folder_command(args: &[String]) -> (Result<Value>, bool) {
    let mut attempted = false;
    let result = keep_folder(args, &mut attempted);
    (result, attempted)
}

fn keep_folder(args: &[String], attempted: &mut bool) -> Result<Value> {
    const USAGE: &str = "usage: hgs worktrees keep-folder --path FOLDER --common-dir DIR --fingerprint HASH [--request-id ID] [--json]";
    let mut options = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            key @ ("--path" | "--common-dir" | "--fingerprint" | "--request-id") => {
                i += 1;
                let value = args.get(i).ok_or_else(|| format!("{key} needs a value"))?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err(format!(
                        "{key} cannot be empty or contain control characters"
                    ));
                }
                if options.insert(key, value.as_str()).is_some() {
                    return Err(format!("duplicate {key}"));
                }
            }
            _ => return Err(USAGE.into()),
        }
        i += 1;
    }
    let required = |key| {
        options
            .get(key)
            .copied()
            .ok_or_else(|| format!("{key} is required"))
    };
    let target = Path::new(required("--path")?);
    let common = Path::new(required("--common-dir")?);
    let expected = required("--fingerprint")?;
    if !target.is_absolute() || !common.is_absolute() {
        return Err("Folder and repository must be absolute paths".into());
    }
    // A record whose repository is gone is reached by its stored path.
    let orphan =
        fs::symlink_metadata(common).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    let common = if orphan {
        common.to_path_buf()
    } else {
        let common = crate::platform::canonical(common).map_err(|e| format!("Repository: {e}"))?;
        let actual = git(
            &common,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            Instant::now() + Duration::from_secs(5),
        )
        .map_err(|e| e.detail)?;
        if crate::platform::canonical(text(actual)).ok().as_ref() != Some(&common) {
            return Err("Repository changed. Review the worktrees again.".into());
        }
        common
    };
    let guard = repository_lock(&common)?;
    let waited = Instant::now() + Duration::from_secs(10);
    while guard.try_lock_exclusive().is_err() {
        if Instant::now() >= waited {
            return Err(
                "Another worktree operation is running. Try again after it finishes.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _lock = crate::platform::FileLock::held(guard);
    let resolved = crate::platform::canonical(target).ok();
    let record = super::worktree_maintenance::removals(&common)?
        .into_iter()
        .find(|r| Path::new(&r.path) == target || resolved.as_deref() == Some(Path::new(&r.path)))
        .ok_or("No partial-removal record exists for this folder. Review the worktrees again.")?;
    let total = Instant::now() + Duration::from_secs(20);
    let path = PathBuf::from(&record.path);
    let row = if orphan {
        let context = context(&common, &[], step(total, 3));
        orphan_row(&record, &context, total)
    } else {
        let (trees, partial) = list(&common, &common, step(total, 5)).map_err(|e| e.detail)?;
        if trees.iter().any(|tree| tree["path"] == json!(path)) {
            return Err("Git tracks this folder again. Review it as a worktree.".into());
        }
        let context = context(&common, &trees, step(total, 3));
        leftover(&record, &common, partial, &context, total)
    };
    if row["fingerprint"] != expected {
        return Err("The leftover folder changed since the review. Review it again.".into());
    }
    *attempted = true;
    super::worktree_maintenance::forget_removal(&record.path)?;
    Ok(json!({"status":"kept","path":path,"common_dir":common,
        "request_id":options.get("--request-id"),"machine":crate::config::Config::load()
            .map(|c| c.host().to_owned()).unwrap_or_default()}))
}

/// Only the CLI exposes the structured outcome. Internal lifecycle callers keep
/// the existing Result contract. Once Git may have started, errors are uncertain.
pub(super) fn remove_with_outcome(args: &[String]) -> (Result<Value>, bool) {
    let (result, attempted, _) = remove_with_review(args);
    (result, attempted)
}

/// The same operation's observed review, for the archive cleanup notice. This
/// never performs a second check or retries an uncertain removal.
pub(super) fn remove_with_review(args: &[String]) -> (Result<Value>, bool, Option<Value>) {
    let mut attempted = false;
    let mut observed = None;
    let result = remove(args, &mut attempted, &mut observed);
    (result, attempted, observed)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct ProgramCleanupAdapter<'a, 'claim> {
    never: std::convert::Infallible,
    marker: std::marker::PhantomData<(&'a (), &'claim ())>,
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl ProgramCleanupAdapter<'_, '_> {
    fn request_id(&self) -> &str {
        match self.never {}
    }
    fn deadline(&self) -> Instant {
        match self.never {}
    }
    fn current(&self) -> Result<()> {
        match self.never {}
    }
    fn review(&self) -> Result<Value> {
        match self.never {}
    }
    fn capture(&self) -> &zerus_client_core::termination_cleanup::Capture {
        match self.never {}
    }
    fn final_check(&self, _: &Value) -> Result<()> {
        match self.never {}
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ProgramCleanupAdapter<'a, 'claim> {
    config: &'a crate::config::Config,
    intent: &'a zerus_client_core::worktree_programs::cleanup::Intent,
    loan: &'a crate::platform::worktree_membership::objects::loan::borrower::Borrowed,
    permit: &'a crate::desktop::program_cleanup::ProgramCleanupPermit<'claim>,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ProgramCleanupAdapter<'_, '_> {
    fn request_id(&self) -> &str {
        self.intent.request_id()
    }
    fn capture(&self) -> &zerus_client_core::termination_cleanup::Capture {
        &self.intent.input.stop.manifest.capture
    }
    fn deadline(&self) -> Instant {
        self.loan.deadline()
    }
    fn current(&self) -> Result<()> {
        if self.permit.intent() != self.intent {
            return Err("The original removal scope changed".into());
        }
        self.permit.current().map_err(|error| error.to_string())?;
        self.loan
            .current_cleanup(self.intent)
            .map_err(|error| error.to_string())?;
        super::worktree_programs::cleanup_original_current(
            &self.config.state,
            &self.intent.input.stop,
            self.deadline(),
        )?;
        program_stop_check(self.capture(), self.deadline())
    }
    fn review(&self) -> Result<Value> {
        self.current()?;
        let value = program_cleanup_review(self.intent, self.loan)?;
        self.current()?;
        Ok(value)
    }
    fn final_check(&self, expected: &Value) -> Result<()> {
        self.current()?;
        // The fresh census itself can be slow. Repeat the guarded file/owner/
        // session assessment AFTER it, rather than letting that I/O create a
        // window in which changed ignored data inherits an older fingerprint.
        let data = program_cleanup_review(self.intent, self.loan)?;
        let row = data["worktrees"]
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or("The working folder changed")?;
        if data["state"] != "ok"
            || row["fingerprint"] != expected["fingerprint"]
            || !zerus_client_core::worktree_review::exit_cleanup_ready(row)
        {
            return Err("The working folder changed or is still in use; review it again".into());
        }
        self.loan
            .current_cleanup(self.intent)
            .map_err(|error| error.to_string())?;
        self.permit.current().map_err(|error| error.to_string())?;
        if Instant::now() >= self.deadline() || crate::platform::shutdown_signal() != 0 {
            return Err("The original removal was cancelled or expired".into());
        }
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn remove_program_cleanup(
    config: &crate::config::Config,
    intent: &zerus_client_core::worktree_programs::cleanup::Intent,
    loan: &crate::platform::worktree_membership::objects::loan::borrower::Borrowed,
    permit: &crate::desktop::program_cleanup::ProgramCleanupPermit<'_>,
) -> (Result<Value>, bool, Option<Value>) {
    use zerus_client_core::termination_cleanup::{Cleanup, Entry, Step};
    let mut attempted = false;
    let mut observed = None;
    let result = (|| {
        let adapter = ProgramCleanupAdapter {
            config,
            intent,
            loan,
            permit,
        };
        let capture = adapter.capture();
        let data = adapter.review()?;
        let entry = Entry::terminated(
            "",
            &capture.machine,
            &capture.root,
            &capture.common_dir,
            &capture.owner_id,
            None,
        )
        .ok_or("The original removal scope changed")?;
        let mut plan = Cleanup::after_termination(vec![entry]);
        if !matches!(plan.step(), Step::Review { .. }) {
            return Err("The original removal is unavailable".into());
        }
        let Step::Remove { removal, .. } = plan.reviewed("", &capture.common_dir, Ok(&data)) else {
            observed = data["worktrees"]
                .as_array()
                .and_then(|rows| rows.first())
                .cloned();
            return Err("The working folder needs a fresh review".into());
        };
        if removal.delete_branch
            || !removal.discard_fingerprint.is_empty()
            || !removal.process_fingerprint.is_empty()
            || !removal.preserve_archive_restore
        {
            return Err("The original removal scope changed".into());
        }
        let invocation = zerus_client_core::worktree_review::remove_invocation(
            &zerus_client_core::ComputerId(String::new()),
            &removal,
            &intent.input.request_id,
        );
        if invocation.args.get(..2) != Some(&["worktrees".into(), "remove".into()]) {
            return Err("The original removal scope changed".into());
        }
        let mut args = invocation.args[2..].to_vec();
        args.extend(["--owner-id".into(), capture.owner_id.clone()]);
        remove_inner(&args, &mut attempted, &mut observed, Some(&adapter))
    })();
    (result, attempted, observed)
}

fn remove(args: &[String], attempted: &mut bool, observed: &mut Option<Value>) -> Result<Value> {
    remove_inner(args, attempted, observed, None)
}
fn remove_inner(
    args: &[String], attempted: &mut bool, observed: &mut Option<Value>,
    native: Option<&ProgramCleanupAdapter<'_, '_>>,
) -> Result<Value> {
    let deadline = |seconds| {
        let ordinary = Instant::now() + Duration::from_secs(seconds);
        native.map_or(ordinary, |adapter| ordinary.min(adapter.deadline()))
    };

    const USAGE: &str = "usage: hgs worktrees remove --path WORKTREE --common-dir DIR --fingerprint HASH [--ignored-fingerprint HASH] [--process-fingerprint HASH] [--delete-branch] [--request-id ID] [--json]";
    let mut options = BTreeMap::new();
    let mut delete_branch = false;
    let mut automatic = false;
    let mut preserve_restore = false;
    let mut periodic = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            "--delete-branch" => delete_branch = true,
            "--automatic" => automatic = true,
            "--preserve-archive-restore" => preserve_restore = true,
            "--periodic" => periodic = true,
            "--dry-run" => return Err("worktree removal does not support --dry-run".into()),
            key @ ("--path"
            | "--common-dir"
            | "--fingerprint"
            | "--ignored-fingerprint"
            | "--discard-fingerprint"
            | "--process-fingerprint"
            | "--owner-id"
            | "--request-id") => {
                i += 1;
                let value = args.get(i).ok_or_else(|| format!("{key} needs a value"))?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err(format!(
                        "{key} cannot be empty or contain control characters"
                    ));
                }
                if options.insert(key, value.as_str()).is_some() {
                    return Err(format!("duplicate {key}"));
                }
            }
            _ => return Err(USAGE.into()),
        }
        i += 1;
    }
    let required = |key| {
        options
            .get(key)
            .copied()
            .ok_or_else(|| format!("{key} is required"))
    };
    let target = Path::new(required("--path")?);
    if let Some(adapter) = native {
        adapter.current()?;
        if automatic || periodic || delete_branch || !preserve_restore
            || options.contains_key("--discard-fingerprint") || options.contains_key("--process-fingerprint")
            || target != Path::new(&adapter.capture().root)
            || options.get("--common-dir").copied() != Some(adapter.capture().common_dir.as_str())
            || options.get("--owner-id").copied() != Some(adapter.capture().owner_id.as_str())
            || options.get("--request-id").copied() != Some(adapter.request_id())
        { return Err("The original removal scope changed".into()); }
    }
    if preserve_restore && delete_branch { return Err("Exit cleanup preserves the retained worktree branch".into()); }
    if periodic && !automatic { return Err("Periodic cleanup requires automatic safety checks".into()); }
    let common = Path::new(required("--common-dir")?);
    let expected = required("--fingerprint")?;
    if !target.is_absolute() || !common.is_absolute() {
        return Err("Worktree and repository must be absolute paths".into());
    }
    let common = crate::platform::canonical(common).map_err(|e| format!("Repository: {e}"))?;
    let changed = "Repository changed or this folder is not one of its worktrees. Review the worktrees again.";
    let actual = git(
        &common,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        deadline(5),
    )
    .map_err(|e| e.detail)?;
    if crate::platform::canonical(text(actual)).ok().as_ref() != Some(&common) {
        return Err(changed.into());
    }
    // An unreadable folder is not a missing one: its ownership is checked or
    // the removal refused.
    if target
        .try_exists()
        .map_err(|e| format!("Worktree folder: {e}"))?
    {
        let own = git(
            target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            deadline(5),
        )
        .map(text)
        .ok()
        .and_then(|own| crate::platform::canonical(own).ok());
        if own.as_ref() != Some(&common) {
            return Err(changed.into());
        }
    }

    let cache = cache_file("repo", &common.to_string_lossy());
    private_dir(cache.parent().unwrap())?;
    let guard = crate::platform::private_open(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    )
    .open(cache.with_extension("lock"))
    .map_err(|e| e.to_string())?;
    let waited = deadline(if automatic { 1 } else { 10 });
    while guard.try_lock_exclusive().is_err() {
        if Instant::now() >= waited {
            return Err(
                "Another worktree operation is running. Try again after it finishes.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The removal's Git inherits this lock (`git_holding`), so it is released
    // by closing, never by an explicit unlock: an unlock would free it for a
    // Git that outlived this process, and a review could then resolve the
    // record before Git deregisters the worktree.
    let lock = guard;
    // One lock order: repository, then session writer. No lifecycle caller holds
    // the writer lock while entering cleanup. A new binding cannot race removal.
    let _state_lock = storage::writer_until(
        deadline(if automatic { 1 } else { 10 }),
    )?
    .ok_or("Session state is busy. Review cleanup after the current operation finishes.")?;

    let data = match native {
        Some(adapter) => adapter.review()?,
        None => review(&common, Some(target), deadline(if automatic { 5 } else { 20 })),
    };
    if data["state"] != "ok" {
        return Err(format!(
            "Cannot review the repository: {}",
            string(&data, "detail")
        ));
    }
    let row = data["worktrees"]
        .as_array()
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or(changed)?;
    *observed = Some(row.clone());
    let path = PathBuf::from(string(&row, "path"));
    if options.get("--owner-id").is_some_and(|expected|row["ownership_id"].as_str()!=Some(*expected)) {
        return Err("Worktree ownership changed; the replacement checkout was kept for review".into());
    }
    if row["kind"] != "linked" {
        return Err("Only linked worktrees can be removed; the main checkout stays.".into());
    }
    let discard = options.get("--discard-fingerprint").copied();
    let discardable = row["discardable"] == true || row["discardable_with_process_consent"] == true;
    if discard.is_some() && (!discardable || discard != row["discard_fingerprint"].as_str()) {
        return Err(
            "Discard confirmation is stale or this worktree is still protected. Review it again."
                .into(),
        );
    }
    let process_consent = options.get("--process-fingerprint").copied();
    if process_consent.is_some()
        && (automatic || !valid_process_consent(process_consent.unwrap(), &row, &common, now() as u64))
    {
        return Err("Process confirmation is stale or this worktree is still protected. Review it again.".into());
    }
    // Every blocking reason needs its own explicit consent: a discard for
    // changes, the bounded acceptance for unreadable processes (zerus-k2ga).
    if row["verdict"] == "blocked" {
        let reasons: Vec<_> = row["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| match string(r, "code") {
                "changes" | "untracked" => discard.is_none(),
                "processes_unavailable" => process_consent.is_none(),
                _ => true,
            })
            .map(|r| string(r, "message").to_owned())
            .collect();
        if !reasons.is_empty() {
            return Err(format!(
                "Cannot remove {}: {}",
                path.display(),
                reasons.join(" ")
            ));
        }
    }
    if automatic {
        let policy = super::worktree_maintenance::policy()?;
        if !policy.enabled || (!periodic && !policy.after_session) {
            return Err("Automatic worktree cleanup is disabled".into());
        }
        if delete_branch
            || options.contains_key("--discard-fingerprint")
            || options.contains_key("--ignored-fingerprint")
        {
            return Err("Automatic cleanup cannot discard local data or delete branches".into());
        }
        if super::worktree_lifecycle::owned(&common, &path).is_none() {
            return Err("Not a verified Zerus-created worktree; review it manually".into());
        }
        let missing = row["available"] == false;
        if missing {
            if !periodic || !policy.missing || fs::symlink_metadata(&path).is_ok()
                || fs::symlink_metadata(&path).is_err_and(|e| e.kind()!=std::io::ErrorKind::NotFound) {
                return Err("Missing worktree cleanup is disabled or the path cannot be verified absent".into());
            }
            let reference=format!("refs/heads/{}",string(&row,"branch"));
            let retained=git(&common,&["rev-parse","--verify","--end-of-options",&reference],Instant::now()+Duration::from_secs(2)).ok().map(text);
            if retained.as_deref()!=Some(string(&row,"head")) {
                return Err("The missing checkout head is not retained by its branch; review manually".into());
            }
        } else {
        if periodic && !policy.unused_integrated { return Err("Unused worktree cleanup is disabled".into()); }
        if periodic && (row["last_activity"].as_f64().is_none_or(|at| now()-at < f64::from(policy.grace_hours)*3600.)) {
            return Err("Worktree is within the configured inactivity grace period".into());
        }
        if row["changes"].as_u64().unwrap_or(1) != 0
            || row["untracked"].as_u64().unwrap_or(1) != 0
            || row["ignored_count"].as_u64().unwrap_or(1) != 0
        {
            return Err("Uncommitted changes or local files remain; review them manually".into());
        }
        if !matches!(
            string(&row["integration"], "kind"),
            "ancestor" | "same_tree" | "equivalent_patches"
        ) {
            return Err("Work is not proven integrated; its folder and branch were kept".into());
        }
        }
    }
    if !automatic && string(&row, "fingerprint") != expected {
        return Err("Worktree changed since the review. Review it again.".into());
    }
    if row["ignored_count"].as_u64().unwrap_or(0) > 0
        && options.get("--ignored-fingerprint").copied() != row["ignored_fingerprint"].as_str()
    {
        return Err("Review the local files outside Git and explicitly confirm their deletion before removing this worktree.".into());
    }
    let missing = row["verdict"] == "missing";
    // Reserve the private removal record before anything else can change, and
    // before Git: its deregistration can succeed while file deletion fails.
    let removal = super::worktree_maintenance::Removal {
        path: path.to_string_lossy().into_owned(),
        common_dir: common.to_string_lossy().into_owned(),
        branch: string(&row, "branch").to_owned(),
        head: string(&row, "head").to_owned(),
        fingerprint: string(&row, "fingerprint").to_owned(),
        request_id: options
            .get("--request-id")
            .copied()
            .unwrap_or("")
            .to_owned(),
        reserved_at: now(),
        status: "pending".into(),
        detail: String::new(),
        volumes: None,
    };
    super::worktree_maintenance::reserve_removal(&removal)?;
    if automatic || preserve_restore {
        if let Err(e) = super::worktree_lifecycle::prepare_removal(&common, &row)
            .and_then(|_| super::worktree_maintenance::reserve(&common, &path))
        {
            let _ = super::worktree_maintenance::settle_removal(&removal, None);
            return Err(e);
        }
    }
    let spelling = path.to_string_lossy();
    let mut command = vec!["worktree", "remove"];
    if discard.is_some() {
        command.push("--force");
    }
    command.extend(["--", &spelling]);
    if let Some(adapter) = native {
        if let Err(error) = adapter.final_check(&row) {
            // This exact phase has not invoked deletion. Keep the automatic
            // queue fenced, but do not leave a false possible-deletion record.
            let _ = super::worktree_maintenance::settle_removal(&removal, None);
            return Err(error);
        }
    }
    *attempted = true;
    let removed = if native.is_some() {
        super::worktrees::git_holding_cancellable(&common, &command, deadline(300), Some(&lock), &|| crate::platform::shutdown_signal() != 0)
    } else {
        git_holding(&common, &command, deadline(if automatic { 3 } else { 300 }), Some(&lock))
    };
    let _ = fs::remove_file(&cache);
    let settled = match native {
        Some(_) => settle_until(&common, &path, deadline(5)),
        None => settle(&common, &path),
    };
    let _ = match &settled {
        Settled::Removed | Settled::Registered => {
            super::worktree_maintenance::settle_removal(&removal, None)
        }
        Settled::Partial => super::worktree_maintenance::settle_removal(
            &removal,
            Some(match &removed {
                Err(e) => e.detail.as_str(),
                Ok(_) => "Git reported success, but the folder remains.",
            }),
        ),
        // Interrupted or unreadable: the pending record stays for review.
        Settled::Unknown(_) => Ok(()),
    };
    if let Err(e) = removed {
        return Err(match settled {
            Settled::Partial => format!(
                "Git stopped tracking this worktree, but files remain in {}: {}\nReview the leftover folder; the removal is never retried.",
                path.display(),
                e.detail
            ),
            _ => format!(
                "Removal could not be confirmed: {}\nReview the worktree again before retrying.",
                e.detail
            ),
        });
    }
    match settled {
        Settled::Removed => {}
        Settled::Unknown(detail) => {
            return Err(format!("Removal could not be confirmed: {detail}"))
        }
        Settled::Partial => {
            return Err(format!(
                "Git stopped tracking this worktree, but its folder remains: {}. Review the leftover folder.",
                path.display()
            ))
        }
        Settled::Registered => {
            return Err("Removal could not be confirmed. Review the worktree again.".into())
        }
    }

    let branch = string(&row, "branch").to_owned();
    let mut deleted = false;
    let mut branch_error = Value::Null;
    if delete_branch && !branch.is_empty() {
        if row["merged"] != true {
            let base = data["base"][0].as_str().unwrap_or("the base branch");
            branch_error = json!(format!(
                "Branch '{branch}' is not merged into {base}; it was kept."
            ));
        } else {
            match git(
                &common,
                &["branch", "-d", "--", &branch],
                deadline(10),
            ) {
                Ok(_) => deleted = true,
                Err(e) => branch_error = json!(e.detail),
            }
        }
    }
    let result = json!({"status":if missing {"forgotten"} else {"removed"},"path":path,"branch":branch,
        "branch_deleted":deleted,"branch_error":branch_error,"common_dir":common,
        "request_id":options.get("--request-id"),"machine":data["machine"]});
    if automatic || preserve_restore {
        super::worktree_maintenance::removed(&path,&result)
            .map_err(|e|format!("Removal could not be confirmed: result could not be saved: {e}"))?;
    }
    drop(lock);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_object_review_never_erases_known_use_or_treats_wrong_scope_as_clear() {
        // Projection controls use synthetic DATA. Native holder/FD/cwd and
        // process identity are exercised by the real membership/loan controls.
        let root = Path::new("/fixture/generated");
        for (observed, expired, programs) in [
            (root, false, vec![zerus_client_core::worktree_programs::Program {
                pid: 41, birth: "original-birth".into(), label: "holder with another open file".into(),
            }]),
            (Path::new("/fixture/another"), false, vec![]),
            (root, true, vec![]),
        ] {
            let context = Context {
                common: "/fixture/repository/.git".into(), bases: vec![],
                processes: ProcessReview::NativeObjects {
                    path: observed.into(),
                    checkout: zerus_client_core::worktree_programs::DirectoryId { device: 1, inode: 2, mount: "native-mount".into() },
                    programs, until: if expired { Instant::now() } else { Instant::now() + Duration::from_secs(5) },
                },
                bindings: vec![], bindings_complete: true, archives: vec![], archives_complete: true,
            };
            let mut row = json!({});
            let mut reasons = vec![];
            occupants(root, &context, &mut row, &mut reasons);
            let projected = finish(row, reasons, vec![]);
            assert_eq!(projected["verdict"], "blocked");
            if observed == root && !expired {
                assert_eq!(projected["process_count"], 1);
                assert_eq!(projected["processes"][0]["pid"], 41);
                assert_eq!(projected["reasons"][0]["code"], "processes");
            } else {
                assert_eq!(projected["reasons"][0]["code"], "processes_unavailable");
            }
            assert!(projected["process_fingerprint"].is_null());
        }
    }

    #[test]
    fn explicit_denial_proof_revalidates_current_native_coverage_before_its_v2_binding() {
        use zerus_client_core::worktree_review::{PERMISSION_DENIALS_ABI, PERMISSION_DENIALS_PREFIX};
        let common = Path::new("/fixture/repository/.git");
        let mut row = json!({"fingerprint":"original-checkout", "process_fingerprint":"available",
            "verdict":"blocked", "reasons":[{"code":"processes_unavailable"}],
            "unreadable_processes":[{"pid":7,"identity":"original-birth","cwd":true,"executable":false,"files":true}],
            "process_coverage":{"abi":PERMISSION_DENIALS_ABI,"complete":true,"open_files_checked":true,"permission_denials":1}});
        let proof = format!("{PERMISSION_DENIALS_PREFIX}{}", process_consent(&row, common, 1000));
        assert!(valid_process_consent(&proof, &row, common, 1000));
        assert!(!valid_process_consent(&proof, &row, common, 1300));
        let original = row.clone();
        for key in ["complete", "open_files_checked"] {
            row = original.clone(); row["process_coverage"][key] = json!(false);
            assert!(!valid_process_consent(&proof, &row, common, 1001), "{key}");
        }
        row = original.clone(); row["process_coverage"] = Value::Null;
        assert!(!valid_process_consent(&proof, &row, common, 1001));
        row = original.clone(); row["reasons"] = json!([{"code":"processes"}]); row["process_fingerprint"] = Value::Null;
        assert!(!valid_process_consent(&proof, &row, common, 1001), "new known use wins");
        row = original.clone(); row["fingerprint"] = json!("replacement-checkout");
        assert!(!valid_process_consent(&proof, &row, common, 1001));
        row = original; row["verdict"] = json!("ready"); row["reasons"] = json!([]);
        row["unreadable_processes"] = json!([]); row["process_fingerprint"] = Value::Null;
        row["process_coverage"]["permission_denials"] = json!(0);
        assert!(valid_process_consent(&proof, &row, common, 1001), "fresh complete no-denial roster remains clear");
    }

    #[test]
    fn readable_membership_is_not_erased_by_a_denied_half_of_the_same_sample() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("open-original");
        fs::write(&file, "fixture").unwrap();
        let context = Context { common: dir.path().into(), bases: vec![],
            processes: ProcessReview::Ordinary(Ok(crate::platform::WorktreeProcesses {
                paths: vec![(7, file, "known-holder".into())],
                unreadable: vec![crate::platform::UnreadableWorktreeProcess { pid:7, identity:"original-birth".into(), command:"known-holder".into(),
                    cwd:true, executable:false, files:false, invocation_ancestor:false }], membership_complete:true })),
            bindings:vec![], bindings_complete:true, archives:vec![], archives_complete:true };
        let mut row = json!({}); let mut reasons = vec![];
        occupants(dir.path(), &context, &mut row, &mut reasons);
        assert_eq!(row["process_count"], 1);
        assert_eq!(row["processes"][0]["pid"], 7);
        assert!(reasons.iter().any(|r| r["code"] == "processes"));
        assert!(reasons.iter().any(|r| r["code"] == "processes_unavailable"));
        assert!(zerus_client_core::worktree_review::permission_denials_complete(&row));
        assert!(row.get("process_fingerprint").is_none(), "known use is not process-uncertainty consent");
    }

    #[test]
    fn manual_process_consent_expires_and_binds_the_checkout() {
        let common = Path::new("/fixture/repository/.git");
        let mut row = json!({"fingerprint":"original-checkout", "process_fingerprint":"available",
            "verdict":"blocked", "reasons":[{"code":"processes_unavailable"}]});
        let proof = process_consent(&row, common, 1000);
        assert!(valid_process_consent(&proof, &row, common, 1000));
        assert!(valid_process_consent(&proof, &row, common, 1299));
        assert!(!valid_process_consent(&proof, &row, common, 1300));
        assert!(!valid_process_consent(&proof, &row, common, 999));
        assert!(!valid_process_consent(&proof, &row, Path::new("/fixture/other/.git"), 1000));
        assert!(!valid_process_consent(&proof.replace("1000", "1001"), &row, common, 1001));
        row["fingerprint"] = json!("replacement-checkout");
        assert!(!valid_process_consent(&proof, &row, common, 1000));
    }

    #[test]
    fn manual_process_consent_does_not_admit_another_blocker() {
        let common = Path::new("/fixture/repository/.git");
        let mut row = json!({"fingerprint":"checkout", "process_fingerprint":"available", "verdict":"blocked"});
        let proof = process_consent(&row, common, 1000);
        row.as_object_mut().unwrap().remove("process_fingerprint");
        for code in ["processes", "sessions", "sessions_unavailable", "processes_unavailable", "status_unavailable", "locked"] {
            row["reasons"] = json!([{"code":code}]);
            assert!(!valid_process_consent(&proof, &row, common, 1000));
        }
        row["verdict"] = json!("ready");row["reasons"] = json!([]);
        assert!(valid_process_consent(&proof, &row, common, 1000), "Complete readable coverage also satisfies the accepted risk");
    }
    #[cfg(unix)]
    #[test]
    fn leftover_review_measures_each_entry_once_and_keeps_its_navigation() {
        use std::os::unix::fs::symlink;
        let base = tempfile::tempdir().unwrap();
        let common = base.path().join("repository.git");
        let initialized = std::process::Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&common)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let root = base.path().join("leftover");
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("cache/bytes"), "fixture data").unwrap();
        let outside = base.path().join("outside-ignored");
        fs::write(&outside, "keep this data").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let record = super::super::worktree_maintenance::Removal {
            path: root.to_string_lossy().into_owned(),
            common_dir: common.to_string_lossy().into_owned(),
            branch: String::new(),
            head: String::new(),
            fingerprint: "original".into(),
            request_id: "fixture".into(),
            reserved_at: 1.0,
            status: "partial".into(),
            detail: String::new(),
            volumes: None,
        };
        let context = Context {
            common: common.clone(),
            bases: Vec::new(),
            processes: ProcessReview::Ordinary(Ok(Default::default())),
            bindings: Vec::new(),
            bindings_complete: true,
            archives: Vec::new(),
            archives_complete: true,
        };
        MEASURED_PATHS.with(|paths| *paths.borrow_mut() = Some(BTreeMap::new()));
        let row = leftover(
            &record,
            &common,
            false,
            &context,
            Instant::now() + Duration::from_secs(10),
        );
        let visits = MEASURED_PATHS.with(|paths| paths.borrow_mut().take().unwrap());
        assert_eq!(visits.len(), 4);
        assert!(
            visits.values().all(|count| *count == 1),
            "a leftover gets one metadata walk: {visits:?}"
        );
        assert!(!visits.contains_key(&outside));
        assert_eq!(row["path"], json!(root));
        assert_eq!(row["removal"]["fingerprint"], "original");
        assert_eq!(row["size_complete"], true);
        assert_eq!(
            row["size"],
            json!(
                "fixture data".len() as u64
                    + fs::symlink_metadata(root.join("link")).unwrap().len()
            )
        );
        assert_eq!(row["deletable"], !cfg!(target_os = "macos"));
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep this data");
        assert_eq!(
            leftover(
                &record,
                &common,
                false,
                &context,
                Instant::now() + Duration::from_secs(10)
            )["fingerprint"],
            row["fingerprint"]
        );
        {
            struct Restore(bool);
            impl Drop for Restore {
                fn drop(&mut self) {
                    FORCE_MACOS_LEFTOVER_GUARD.with(|guard| guard.set(self.0));
                }
            }
            let _restore = Restore(FORCE_MACOS_LEFTOVER_GUARD.with(|guard| guard.replace(true)));
            use std::os::unix::fs::PermissionsExt;
            let original_mode = fs::metadata(&root).unwrap().permissions();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
            let blocked = leftover(
                &record,
                &common,
                false,
                &context,
                Instant::now() + Duration::from_secs(10),
            );
            assert_eq!(blocked["reasons"][0]["code"], "macos_appledouble_safety");
            assert_eq!(blocked["deletable"], false);
            assert!(blocked["process_fingerprint"].is_null());
            let mode = fs::metadata(&root).unwrap().permissions();
            let (refused, attempted) = delete_leftover_command(&[
                "--path".into(),
                root.to_string_lossy().into_owned(),
                "--common-dir".into(),
                common.join("never-opened").to_string_lossy().into_owned(),
                "--fingerprint".into(),
                string(&blocked, "fingerprint").into(),
                "--confirm-delete".into(),
            ]);
            assert!(
                !attempted,
                "a platform refusal cannot consume a confirmation"
            );
            assert_eq!(refused.unwrap_err(), MACOS_LEFTOVER_REASON);
            assert_eq!(
                delete_tree(
                    &root,
                    &RootIdentity::from_json(&row["root_identity"]).unwrap(),
                    Instant::now() + Duration::from_secs(10)
                )
                .unwrap_err()
                .kind(),
                std::io::ErrorKind::Unsupported
            );
            assert_eq!(fs::metadata(&root).unwrap().permissions(), mode);
            assert_eq!(
                fs::read_to_string(root.join("cache/bytes")).unwrap(),
                "fixture data"
            );
            use std::os::fd::AsRawFd;
            let parent = deletion::open_path(&fs::canonicalize(base.path()).unwrap()).unwrap();
            for name in [c"empty-root", c"empty-nested"] {
                fs::create_dir(base.path().join(name.to_str().unwrap())).unwrap();
                assert_eq!(
                    deletion::unlink(parent.as_raw_fd(), name, libc::AT_REMOVEDIR)
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::Unsupported
                );
                assert!(base.path().join(name.to_str().unwrap()).is_dir());
            }
            fs::set_permissions(&root, original_mode).unwrap();
        }
        // A replacement after the walk cannot inherit its manifest. Keep
        // the old root identity in the row so the effect refuses it.
        let original = row["root_identity"].clone();
        let moved = base.path().join("reviewed-leftover");
        let (swap_root, swap_moved) = (root.clone(), moved.clone());
        AFTER_LEFTOVER_SCAN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&swap_root, &swap_moved).unwrap();
                fs::create_dir(&swap_root).unwrap();
            }))
        });
        let replaced = leftover(
            &record,
            &common,
            false,
            &context,
            Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(
            replaced["root_identity"], original,
            "a new root cannot acquire an old scan"
        );
        assert!(delete_tree(
            &root,
            &RootIdentity::from_json(&replaced["root_identity"]).unwrap(),
            Instant::now() + Duration::from_secs(10)
        )
        .is_err());
        assert!(root.is_dir());
        assert_eq!(
            fs::read_to_string(moved.join("cache/bytes")).unwrap(),
            "fixture data"
        );
    }
    #[cfg(unix)]
    #[test]
    fn a_wide_leftover_bounds_enumeration_and_reports_incomplete_size() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("wide");
        fs::create_dir(&root).unwrap();
        for number in 0..64 {
            fs::write(root.join(number.to_string()), "x").unwrap();
        }
        MEASURED_PATHS.with(|paths| *paths.borrow_mut() = Some(BTreeMap::new()));
        let scan = scan_leftover_limit(&root, Instant::now() + Duration::from_secs(10), 4);
        let visits = MEASURED_PATHS.with(|paths| paths.borrow_mut().take().unwrap());
        assert!(!scan.complete);
        assert!(scan.blockers.is_empty());
        assert_eq!(
            scan.queued, 4,
            "bound queued entries before filling the pending walk"
        );
        assert_eq!(visits.len(), 4);
        assert_eq!(scan.bytes, 3);
        assert!(visits.values().all(|count| *count == 1));
        MEASURED_PATHS.with(|paths| *paths.borrow_mut() = Some(BTreeMap::new()));
        let expired = scan_leftover_limit(&root, Instant::now(), 4);
        let visits = MEASURED_PATHS.with(|paths| paths.borrow_mut().take().unwrap());
        assert!(visits.is_empty());
        assert!(!expired.complete && expired.root.is_none());
        assert_eq!(fs::read_dir(root).unwrap().count(), 64);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an explicitly authorized native macOS filesystem window"]
    fn macos_late_swap_preserves_appledouble_payload_at_root_and_nested_names() {
        use std::os::fd::AsRawFd;
        for nested in [false, true] {
            let base = tempfile::tempdir().unwrap();
            let parent = if nested {
                base.path().join("parent")
            } else {
                base.path().to_path_buf()
            };
            fs::create_dir_all(&parent).unwrap();
            let root = parent.join("reviewed");
            fs::create_dir(&root).unwrap();
            let parent_fd = deletion::open_path(&fs::canonicalize(&parent).unwrap()).unwrap();
            let pinned = deletion::open(parent_fd.as_raw_fd(), c"reviewed").unwrap();
            let expected = deletion::identify(&pinned).unwrap();
            assert_eq!(
                deletion::identify(&deletion::open(parent_fd.as_raw_fd(), c"reviewed").unwrap())
                    .unwrap(),
                expected
            );
            // The swap is AFTER the final identity check, not before it.
            let moved = parent.join("original");
            fs::rename(&root, &moved).unwrap();
            fs::create_dir(&root).unwrap();
            fs::write(root.join("._payload"), b"unreviewed user bytes").unwrap();
            assert!(
                deletion::unlink(parent_fd.as_raw_fd(), c"reviewed", libc::AT_REMOVEDIR).is_err()
            );
            assert_eq!(
                fs::read(root.join("._payload")).unwrap(),
                b"unreviewed user bytes"
            );
            assert_eq!(deletion::identify(&pinned).unwrap(), expected);
            assert!(moved.is_dir());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn an_empty_replacement_before_the_final_unlink_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("leftover");
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("cache/file"), "reviewed").unwrap();
        let identity = root_identity(&root).unwrap();
        let moved = base.path().join("reviewed-root");
        let (swap_root, swap_moved) = (root.clone(), moved.clone());
        BEFORE_ROOT_UNLINK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&swap_root, &swap_moved).unwrap();
                fs::create_dir(&swap_root).unwrap();
            }))
        });
        let result = delete_tree(&root, &identity, Instant::now() + Duration::from_secs(10));
        assert!(
            result.is_err(),
            "an unreviewed empty replacement must survive"
        );
        assert!(root.is_dir() && moved.is_dir());
        assert!(fs::read_dir(root).unwrap().next().is_none());
    }

    /// A filesystem or same-device bind mount inside a leftover after its
    /// review: the scan reports it and the deletion stops before changing it.
    /// Runs the probe in a private user and mount namespace when available.
    #[cfg(target_os = "linux")]
    #[test]
    fn deletion_stops_at_another_filesystem_inside_the_leftover() {
        let base = tempfile::tempdir().unwrap();
        let Ok(output) = std::process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::worktree_cleanup::tests::leftover_mount_probe",
                "--nocapture",
            ])
            .env("HGS_TEST_LEFTOVER_MOUNT", base.path())
            .env_remove("HGS_SESSION")
            .env_remove("HGS_RUN_ID")
            .output()
        else {
            eprintln!("skipped: unshare is unavailable");
            return;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.contains("leftover mount probe ran") {
            eprintln!("skipped: user and mount namespaces are unavailable: {stderr}");
            return;
        }
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("leftover mount probe passed"),
            "{stdout}\n{stderr}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn leftover_mount_probe() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Some(base) = std::env::var_os("HGS_TEST_LEFTOVER_MOUNT") else {
            return;
        };
        println!("leftover mount probe ran");
        let mount = |source: &Path, path: &Path, kind: Option<&std::ffi::CStr>, data: &str| {
            let source = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
            let target = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            let data = std::ffi::CString::new(data).unwrap();
            let flags = if kind.is_none() { libc::MS_BIND } else { 0 };
            let rc = unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    kind.map_or(std::ptr::null(), |k| k.as_ptr()),
                    flags,
                    data.as_ptr().cast(),
                )
            };
            assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        };
        let base = PathBuf::from(base);
        let deadline = || Instant::now() + Duration::from_secs(10);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        // 1. A readonly tmpfs inside: refused before its mode changes.
        let root = base.join("leftover");
        fs::create_dir_all(root.join("build/out")).unwrap();
        fs::write(root.join("build/out/file"), "build").unwrap();
        let mounted = root.join("mounted");
        fs::create_dir(&mounted).unwrap();
        mount(Path::new("none"), &mounted, Some(c"tmpfs"), "mode=0700");
        fs::write(mounted.join("keep"), "other filesystem").unwrap();
        fs::set_permissions(&mounted, fs::Permissions::from_mode(0o500)).unwrap();
        let identity = root_identity(&root).unwrap();
        assert!(scan_leftover(&root, deadline())
            .blockers
            .iter()
            .any(|r| r["code"] == "other_filesystem"));
        let error = delete_tree(&root, &identity, deadline()).unwrap_err();
        assert!(error.to_string().contains("another filesystem"), "{error}");
        assert_eq!(mode(&mounted), 0o500, "the mode changed before the check");
        assert_eq!(
            fs::read_to_string(mounted.join("keep")).unwrap(),
            "other filesystem"
        );
        // 2. A same-device bind mount of a folder outside the leftover.
        let outside = base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("precious"), "outside").unwrap();
        let tree = base.join("bound-leftover");
        let bound = tree.join("bound");
        fs::create_dir_all(&bound).unwrap();
        mount(&outside, &bound, None, "");
        let identity = root_identity(&tree).unwrap();
        assert_eq!(
            fs::metadata(&bound).unwrap().dev(),
            fs::metadata(&tree).unwrap().dev(),
            "the bind mount shares the device"
        );
        assert!(scan_leftover(&tree, deadline())
            .blockers
            .iter()
            .any(|r| r["code"] == "other_filesystem"));
        let error = delete_tree(&tree, &identity, deadline()).unwrap_err();
        assert!(error.to_string().contains("another filesystem"), "{error}");
        assert_eq!(
            fs::read_to_string(outside.join("precious")).unwrap(),
            "outside"
        );
        // 3. A leftover root that is itself a mount point.
        let top = base.join("mounted-root");
        fs::create_dir(&top).unwrap();
        mount(Path::new("none"), &top, Some(c"tmpfs"), "");
        fs::write(top.join("keep"), "mounted root").unwrap();
        let identity = root_identity(&top).unwrap();
        assert!(scan_leftover(&top, deadline())
            .blockers
            .iter()
            .any(|r| r["code"] == "mount_point"));
        let error = delete_tree(&top, &identity, deadline()).unwrap_err();
        assert!(error.to_string().contains("mount point"), "{error}");
        assert_eq!(
            fs::read_to_string(top.join("keep")).unwrap(),
            "mounted root"
        );
        println!("leftover mount probe passed");
    }

    /// The reviewed root is bound by device and inode. Swapping its parent for
    /// a link, or renaming another folder into place, stops the deletion.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_swapped_leftover_root_is_refused_and_the_decoy_survives() {
        let base = tempfile::tempdir().unwrap();
        let parent = base.path().join("parent");
        let root = parent.join("leftover");
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/file"), "reviewed").unwrap();
        let identity = root_identity(&root).unwrap();
        let deadline = || Instant::now() + Duration::from_secs(10);
        let real = base.path().join("real-parent");
        fs::rename(&parent, &real).unwrap();
        let decoy = base.path().join("decoy-parent");
        fs::create_dir_all(decoy.join("leftover")).unwrap();
        fs::write(decoy.join("leftover/precious"), "decoy").unwrap();
        std::os::unix::fs::symlink(&decoy, &parent).unwrap();
        let error = delete_tree(&root, &identity, deadline()).unwrap_err();
        assert!(
            error.to_string().contains("not the reviewed folder"),
            "{error}"
        );
        fs::remove_file(&parent).unwrap();
        fs::rename(&decoy, &parent).unwrap();
        let error = delete_tree(&root, &identity, deadline()).unwrap_err();
        assert!(
            error.to_string().contains("not the reviewed folder"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(parent.join("leftover/precious")).unwrap(),
            "decoy"
        );
        assert_eq!(
            fs::read_to_string(real.join("leftover/build/file")).unwrap(),
            "reviewed"
        );
        // The reviewed folder itself is still deleted and confirmed.
        delete_tree(&real.join("leftover"), &identity, deadline()).unwrap();
        assert!(!real.join("leftover").exists());
    }

    /// zerus-ty94 #4: an ancestor the owner may only search (execute without
    /// read) does not stop the deletion, and a link in the ancestor chain is
    /// still never followed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_search_only_ancestor_does_not_stop_the_deletion() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let base = tempfile::tempdir().unwrap();
        let base_path = fs::canonicalize(base.path()).unwrap();
        let gate = base_path.join("search-only");
        let root = gate.join("parent/leftover");
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/file"), "reviewed").unwrap();
        std::os::unix::fs::symlink(&gate, base_path.join("link")).unwrap();
        let identity = root_identity(&root).unwrap();
        fs::set_permissions(&gate, fs::Permissions::from_mode(0o100)).unwrap();
        let linked = deletion::open_path(&base_path.join("link/parent"));
        let result = delete_tree(&root, &identity, Instant::now() + Duration::from_secs(10));
        fs::set_permissions(&gate, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(linked.is_err(), "a link in the ancestor chain was followed");
        result.unwrap();
        assert!(!root.exists() && gate.join("parent").is_dir());
    }

    /// Mutation R5: the deletion is confirmed only when the name is absent
    /// through the parent, never because the unlink returned.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_reappears_after_the_root_is_unlinked_is_not_confirmed() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("leftover");
        fs::create_dir_all(root.join("build")).unwrap();
        fs::write(root.join("build/file"), "reviewed").unwrap();
        let identity = root_identity(&root).unwrap();
        let again = root.clone();
        AFTER_ROOT_UNLINK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || fs::create_dir(&again).unwrap()))
        });
        let error =
            delete_tree(&root, &identity, Instant::now() + Duration::from_secs(10)).unwrap_err();
        assert!(
            error.to_string().contains("could not be confirmed"),
            "{error}"
        );
        assert!(root.is_dir() && !root.join("build").exists());
    }

    #[cfg(unix)]
    #[test]
    fn deletion_opens_folders_without_following_links() {
        use std::os::unix::ffi::OsStrExt;
        let base = tempfile::tempdir().unwrap();
        let folder = base.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let name = std::ffi::CString::new(link.as_os_str().as_bytes()).unwrap();
        assert!(deletion::open(libc::AT_FDCWD, &name).is_err());
        let name = std::ffi::CString::new(folder.as_os_str().as_bytes()).unwrap();
        assert!(deletion::open(libc::AT_FDCWD, &name).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn deletion_checks_the_owner_before_granting_access() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let folder = base.path().join("readonly");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o500)).unwrap();
        let identity = root_identity(&folder).unwrap();
        let name = std::ffi::CString::new(folder.as_os_str().as_bytes()).unwrap();
        let fd = deletion::open(libc::AT_FDCWD, &name).unwrap();
        let other = unsafe { libc::geteuid() }.wrapping_add(1);
        assert!(deletion::checked(&fd, other, &identity).is_err());
        let mode = || fs::metadata(&folder).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(), 0o500);
        deletion::checked(&fd, unsafe { libc::geteuid() }, &identity).unwrap();
        assert_eq!(mode(), 0o700);
    }
}

#[cfg(all(test, unix))]
mod binding_census_tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_binding_census_keeps_every_twin_and_refuses_irregular_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "state::worktree_cleanup::binding_census_tests::binding_census_fixture", "--ignored", "--test-threads=1"])
            .env("HGS_STATE_DIR",fixture.path().join("state"))
            .env("HGS_CONFIG_DIR",fixture.path().join("config"))
            .env("ZERUS_BINDING_CENSUS_FIXTURE","1")
            .env_remove("TMUX").env_remove("HGS_SESSION_ID").env_remove("HGS_SESSION").env_remove("HGS_RUN_ID")
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let until = Instant::now()+Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if Instant::now() >= until { child.kill().unwrap(); break child.wait().unwrap(); }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut out = Vec::new(); let mut err = Vec::new();
        child.stdout.take().unwrap().take(65536).read_to_end(&mut out).unwrap();
        child.stderr.take().unwrap().take(65536).read_to_end(&mut err).unwrap();
        assert!(status.success() && String::from_utf8_lossy(&out).contains("test result: ok. 1 passed"),
            "{}{}",String::from_utf8_lossy(&out),String::from_utf8_lossy(&err));
    }

    #[test]
    #[ignore = "process-isolated binding census fixture"]
    fn binding_census_fixture() {
        if std::env::var("ZERUS_BINDING_CENSUS_FIXTURE").as_deref()!=Ok("1") {return;}
        let name = "codex/example/shared";
        let folders: Vec<_> = (0..4).map(|n|root().parent().unwrap().join(format!("folder-{n}"))).collect();
        for folder in &folders { fs::create_dir_all(folder).unwrap(); }
        let first = json!({"version":1,"agent":"codex","name":name,"session_id":uuid::Uuid::new_v4().to_string(),
            "run_id":"first","conversation_id":"first-conversation","cwd":folders[0]});
        let second = json!({"version":1,"agent":"codex","name":name,"session_id":uuid::Uuid::new_v4().to_string(),
            "layout":"id","run_id":"second","conversation_id":"second-conversation","cwd":folders[1]});
        atomic(&storage::label_record_path(name),&first.to_string()).unwrap();
        let second_path = storage::id_record_path(string(&second,"session_id")).unwrap();
        atomic(&second_path,&second.to_string()).unwrap();
        for (provider,cid,folder) in [("dsh","session-third",&folders[2]),("opencode","ses_fourth",&folders[3])] {
            let id=uuid::Uuid::new_v4().to_string();
            let record=json!({"backend":provider,"agent":provider,"name":format!("{provider}/example/shared"),
                "layout":"id","session_id":id,"run_id":provider,"conversation_id":cid,"cwd":folder});
            atomic(&root().join(provider).join("sessions").join(format!("{id}.json")),&record.to_string()).unwrap();
        }
        session_id::select(name,string(&second,"session_id"));
        let refs=bindings().unwrap();
        assert_eq!(refs.len(),4);
        for folder in &folders {
            let expected = crate::platform::canonical(folder).unwrap();
            assert!(refs.iter().any(|r|r.folders.contains(&expected)),"missing {}",folder.display());
        }
        assert_eq!(refs.iter().filter(|r|r.name==name).count(),2);
        let original=fs::read(&second_path).unwrap();
        fs::remove_file(&second_path).unwrap();
        let path=std::ffi::CString::new(second_path.to_str().unwrap()).unwrap();
        // SAFETY: the fixture owns this NUL-free absent path.
        assert_eq!(unsafe {libc::mkfifo(path.as_ptr(),0o600)},0);
        assert!(bindings().is_err());
        fs::remove_file(&second_path).unwrap();
        std::os::unix::fs::symlink(storage::label_record_path(name),&second_path).unwrap();
        assert!(bindings().is_err());
        fs::remove_file(&second_path).unwrap();
        atomic(&second_path,"{}").unwrap(); assert!(bindings().is_err());
        fs::write(&second_path,&original).unwrap();
        assert_eq!(bindings().unwrap().len(),4);
        session_id::clear();
    }
}

#[cfg(all(test, unix))]
pub(super) mod scoped_tests;
