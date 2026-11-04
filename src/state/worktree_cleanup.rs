//! Review and guarded removal of linked worktrees. Removal repeats every check
//! under the repository lock used by creation, compares the fingerprint the
//! person reviewed. Force is limited to explicitly confirmed, content-fingerprinted
//! uncommitted files. A short-lived manual process confirmation accepts incomplete
//! access to process paths; active/locked/nested and other unknowns stay protected.
use super::worktrees::{cache_file, canonical, error, git, list, GitError};
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

fn process_consent(row: &Value, common: &Path, issued: u64) -> String {
    let proof = fingerprint(json!({"scope":"manual-process-uncertainty-v2",
        "worktree":row["fingerprint"],"common_dir":common,"issued":issued}));
    format!("v2:{issued}:{proof}")
}

fn valid_process_consent(proof: &str, row: &Value, common: &Path, at: u64) -> bool {
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
struct Reference {
    name: String,
    folders: Vec<PathBuf>,
}

struct Context {
    common: PathBuf,
    /// (display name, full ref) of the main checkout's branch and origin/HEAD.
    bases: Vec<(String, String)>,
    /// Other processes' current directories, or why they cannot be read.
    processes: std::result::Result<crate::platform::WorktreeProcesses, String>,
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
        folders,
    }
}

fn bindings() -> Result<Vec<Reference>> {
    // Ordinary catalog reads tolerate broken records. Deletion cannot interpret
    // a skipped or unreadable record as proof that no session uses the folder.
    let mut result = Vec::new();
    let mut count = 0;
    for directory in ["", "bindings", "dsh/bindings", "opencode/bindings"] {
        let folder = root().join(directory);
        let files = match fs::read_dir(&folder) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        for file in files {
            let file = file.map_err(|e| e.to_string())?;
            let path = file.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if !(directory.contains('/')
                || stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                continue;
            }
            count += 1;
            if count > 5000 {
                return Err("Too many session bindings to check".into());
            }
            let value: Value = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            if value["rename_shadow"] == true {
                continue;
            }
            if string(&value, "name").is_empty() {
                return Err("Invalid saved session identity".into());
            }
            let value = if directory.contains('/') {
                value
            } else {
                read(string(&value, "name"))?
            };
            result.push(reference(&value));
        }
    }
    Ok(result)
}

fn context(common: &Path, trees: &[Value], deadline: Instant) -> Context {
    let mut bases = Vec::new();
    if trees[0]["kind"] == "main" {
        if let Some(branch) = trees[0]["branch"].as_str().filter(|b| !b.is_empty()) {
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
    let mut archives=Vec::new();
    let archives_complete=archive::visit_bounded(deadline,|record|{archives.push(reference(&record));Ok(())}).is_ok();
    Context {
        common: common.to_path_buf(),
        bases,
        processes: crate::platform::worktree_processes(deadline),
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

    // Without the running processes' folders, nothing may count as unused.
    let mut samples: Vec<_> = match &context.processes {
        Ok(processes) => {
            if !processes.unreadable.is_empty() {
                reasons.push(reason("processes_unavailable", format!(
                    "Zerus cannot check whether some running programs use this folder ({} with hidden paths).",
                    plural(processes.unreadable.len(), "process")
                )));
                row["unreadable_processes"] = json!(processes.unreadable);
            }
            processes.paths.iter()
                .filter(|(_, cwd, _)| inside(cwd, &path))
                .collect()
        }
        Err(error) => {
            reasons.push(reason(
                "processes_unavailable",
                format!("{error}, so a program may still use this folder."),
            ));
            Vec::new()
        }
    };
    samples.sort_by_key(|(pid, _, _)| *pid);
    samples.dedup_by_key(|(pid, _, _)| *pid);
    row["process_count"] = json!(samples.len());
    row["processes"] = json!(samples
        .iter()
        .take(MAX_SAMPLES)
        .map(|(pid, _, name)| json!({"pid":pid,"command":name}))
        .collect::<Vec<_>>());
    if !samples.is_empty() {
        let names: Vec<_> = samples
            .iter()
            .take(MAX_SAMPLES)
            .map(|(pid, _, name)| format!("{name} ({pid})"))
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
    let uses = |references: &[Reference]| -> Vec<String> {
        let mut names: Vec<String> = references
            .iter()
            .filter(|r| r.folders.iter().any(|folder| inside(folder, &path)))
            .map(|r| r.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    let sessions = uses(&context.bindings);
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
    let archived = uses(&context.archives).len();
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
    // manual exception. All other blockers (including any readable use) win.
    if context.processes.as_ref().is_ok_and(|p| !p.unreadable.is_empty())
        && reasons.len() == 1 && reasons[0]["code"] == "processes_unavailable"
    {
        row["process_fingerprint"] = json!(process_consent(&row, &context.common, now() as u64));
    }
    finish(row, reasons, notes)
}

/// Assessment of the checkouts of the repository containing `path`; `only`
/// limits it to one worktree, which may already be missing.
pub(super) fn review(path: &Path, only: Option<&Path>, total: Instant) -> Value {
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
        let context = context(&common, &trees, step(total, 3));
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
        Ok::<_, GitError>(
            json!({"state":"ok","path":path,"common_dir":common,"base":base,
            "partial":partial || cut,"worktrees":rows,"machine":machine,"sampled_at":now()}),
        )
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

pub(super) fn remove_command(args: &[String]) -> Result<Value> {
    remove_with_outcome(args).0
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

fn remove(args: &[String], attempted: &mut bool, observed: &mut Option<Value>) -> Result<Value> {
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
        Instant::now() + Duration::from_secs(5),
    )
    .map_err(|e| e.detail)?;
    if crate::platform::canonical(text(actual)).ok().as_ref() != Some(&common) {
        return Err(changed.into());
    }
    if target.exists() {
        let own = git(
            target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            Instant::now() + Duration::from_secs(5),
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
    let waited = Instant::now() + Duration::from_secs(if automatic { 1 } else { 10 });
    while guard.try_lock_exclusive().is_err() {
        if Instant::now() >= waited {
            return Err(
                "Another worktree operation is running. Try again after it finishes.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let lock = crate::platform::FileLock::held(guard);
    // One lock order: repository, then session writer. No lifecycle caller holds
    // the writer lock while entering cleanup. A new binding cannot race removal.
    let _state_lock = storage::writer_until(
        Instant::now() + Duration::from_secs(if automatic { 1 } else { 10 }),
    )?
    .ok_or("Session state is busy. Review cleanup after the current operation finishes.")?;

    let data = review(
        &common,
        Some(target),
        Instant::now() + Duration::from_secs(if automatic { 5 } else { 20 }),
    );
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
    if discard.is_some()
        && (row["discardable"] != true || discard != row["discard_fingerprint"].as_str())
    {
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
    if row["verdict"] == "blocked" && discard.is_none() && process_consent.is_none() {
        let reasons: Vec<_> = row["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| string(r, "message").to_owned())
            .collect();
        return Err(format!(
            "Cannot remove {}: {}",
            path.display(),
            reasons.join(" ")
        ));
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
    if automatic || preserve_restore {
        super::worktree_lifecycle::prepare_removal(&common, &row)?;
        super::worktree_maintenance::reserve(&common, &path)?;
    }
    let spelling = path.to_string_lossy();
    let mut command = vec!["worktree", "remove"];
    if discard.is_some() {
        command.push("--force");
    }
    command.extend(["--", &spelling]);
    *attempted = true;
    git(
        &common,
        &command,
        Instant::now() + Duration::from_secs(if automatic { 3 } else { 300 }),
    )
    .map_err(|e| {
        format!(
            "Removal could not be confirmed: {}\nReview the worktree again before retrying.",
            e.detail
        )
    })?;
    let _ = fs::remove_file(&cache);
    let (trees, _) = list(&common, &common, Instant::now() + Duration::from_secs(5))
        .map_err(|e| format!("Removal could not be confirmed: {}", e.detail))?;
    if trees.iter().any(|tree| tree["path"] == json!(path)) || (!missing && path.exists()) {
        return Err("Removal could not be confirmed. Review the worktree again.".into());
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
                Instant::now() + Duration::from_secs(10),
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
        for code in ["processes", "sessions", "processes_unavailable", "status_unavailable", "locked"] {
            row["reasons"] = json!([{"code":code}]);
            assert!(!valid_process_consent(&proof, &row, common, 1000));
        }
        row["verdict"] = json!("ready");row["reasons"] = json!([]);
        assert!(valid_process_consent(&proof, &row, common, 1000), "Complete readable coverage also satisfies the accepted risk");
    }
}
