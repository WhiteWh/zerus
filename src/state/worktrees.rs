//! On-demand checkout catalog and explicit creation of a new branch/worktree.
use super::*;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::process::Command;
use std::time::{Duration, Instant};

mod creation_cli;

const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_TREES: usize = 512;
const TTL: f64 = 30.0;
static MOBILE_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn stop_mobile(_: libc::c_int) { MOBILE_STOP.store(true, std::sync::atomic::Ordering::Relaxed); }
#[cfg(unix)]
struct MobileSignals(Vec<(libc::c_int, libc::sighandler_t)>);
#[cfg(unix)]
impl MobileSignals {
    fn install() -> Self {
        MOBILE_STOP.store(false, std::sync::atomic::Ordering::Relaxed);
        Self([libc::SIGHUP, libc::SIGTERM].iter().map(|signal| (*signal, unsafe {
            libc::signal(*signal, stop_mobile as *const () as libc::sighandler_t)
        })).collect())
    }
}
#[cfg(unix)]
impl Drop for MobileSignals {
    fn drop(&mut self) { for (signal, old) in &self.0 { unsafe { libc::signal(*signal, *old); } } }
}
/// Windows is no mobile gateway in Phase 2 (the mobile connector is Unix-only),
/// so nothing cancels a mobile creation there; every Git call keeps its deadline.
#[cfg(not(unix))]
struct MobileSignals;
#[cfg(not(unix))]
impl MobileSignals {
    fn install() -> Self {
        MOBILE_STOP.store(false, std::sync::atomic::Ordering::Relaxed);
        Self
    }
}
fn mobile_stopped() -> bool { MOBILE_STOP.load(std::sync::atomic::Ordering::Relaxed) || super::worktree_requests::cancelled() }


pub(super) struct GitError {
    pub(super) state: &'static str,
    pub(super) detail: String,
    /// Git's exit code when it completed with a failure.
    pub(super) code: Option<i32>,
}
pub(super) fn error(state: &'static str, detail: impl Into<String>) -> GitError {
    GitError {
        state,
        detail: detail.into(),
        code: None,
    }
}

/// Whether `reference` (`refs/heads/<branch>`) is taken: the ref itself, a
/// ref under it (`<branch>/x`) or a ref named by one of its leading parts
/// (`a` for `a/b`). Each check reads at most one ref, so a repository that
/// keeps thousands of branches never overflows the bounded Git output, as a
/// full branch listing did. Ref patterns match whole path parts, and refs sort
/// by name, so a part sorts before every ref under it.
fn branch_taken(read: &dyn Fn(&[&str]) -> Result<String>, reference: &str) -> Result<bool> {
    let first = |pattern: &str| -> Result<String> {
        Ok(
            read(&["for-each-ref", "--count=1", "--format=%(refname)", pattern])?
                .trim_end()
                .to_owned(),
        )
    };
    if !first(reference)?.is_empty() {
        return Ok(true);
    }
    let parts: Vec<&str> = reference.split('/').collect();
    for end in 3..parts.len() {
        let parent = parts[..end].join("/");
        if first(&parent)? == parent {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Task #32 (team ede724c4): how long one Git read may take: `seconds` here; in
/// tests at least a minute, which a machine busy with parallel builds and test
/// threads fits, so a test never depends on how fast the machine is. A read
/// that still runs out says so (`GitError::state` "timeout").
pub(crate) fn read_window(seconds: u64) -> Duration {
    #[cfg(test)]
    let seconds = seconds.max(60);
    Duration::from_secs(seconds)
}

/// Task #32: the window of a desktop worktree creation's reads before the
/// clone (a phone keeps its short one inside its 25 s operation).
const CREATE_READ_SECONDS: u64 = 60;

pub(super) fn git(cwd: &Path, args: &[&str], deadline: Instant) -> std::result::Result<Vec<u8>, GitError> {
    git_holding(cwd, args, deadline, None)
}

/// Like [`git`], and on Unix the Git process also holds `lock`: it inherits
/// that one descriptor (an flock belongs to the open file description), so
/// the lock stays taken until Git and anything it starts have exited, even if
/// this process is killed first. Only this child inherits it; every other
/// descriptor, and every other child, keeps close-on-exec.
pub(super) fn git_holding(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    lock: Option<&fs::File>,
) -> std::result::Result<Vec<u8>, GitError> {
    git_holding_cancellable(cwd, args, deadline, lock, &|| false)
}
/// The private native continuation can add cancellation, never waive an
/// existing guard or replace the original Git/deletion deadline.
pub(super) fn git_holding_cancellable(
    cwd: &Path, args: &[&str], deadline: Instant, lock: Option<&fs::File>, cancelled: &impl Fn() -> bool,
) -> std::result::Result<Vec<u8>, GitError> {
    let mut command = args;
    while command.len() >= 2 && matches!(command[0], "-c" | "-C") {
        command = &command[2..];
    }
    let subcommand = command.first().copied().unwrap_or("command");
    let timeout = || error("timeout", format!("git {subcommand} timed out or was cancelled"));
    if mobile_stopped() || cancelled() || Instant::now() >= deadline { return Err(timeout()); }
    let mut command = Command::new("git");
    #[cfg(unix)]
    if let Some(lock) = lock {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let fd = lock.as_raw_fd();
        // SAFETY: runs in the forked child just before exec and calls only
        // fcntl, which is async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = lock;
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C"])
        .arg(cwd)
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0");
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(key);
    }
    if args.contains(&"ls-remote") {
        // Read-only publication checks must never open a credential prompt.
        crate::platform::git_noninteractive(&mut command);
    }
    // Both pipes are drained while Git runs, including large catalogs. A
    // checkout hook or filter can outlive Git: on timeout the whole operation
    // stops instead of writing on after an error was reported.
    let (status, out, err) =
        crate::platform::run_bounded(&mut command, [MAX_OUTPUT, 8192], deadline, &|| mobile_stopped() || cancelled())
            .map_err(|failure| match failure {
                crate::platform::RunError::Spawn(e) => error(
                    if e.kind() == io::ErrorKind::NotFound {
                        "git_unavailable"
                    } else {
                        "metadata_error"
                    },
                    e.to_string(),
                ),
                crate::platform::RunError::TooLarge => {
                    error("too_large", "Git operation exceeded the output limit")
                }
                crate::platform::RunError::Io(e) => error("metadata_error", e.to_string()),
                _ => timeout(),
            })?;
    if status.success() {
        Ok(out)
    } else {
        let detail = String::from_utf8_lossy(&err).trim().to_owned();
        let mut failure = error(
            if detail.contains("not a git repository") {
                "not_repo"
            } else {
                "metadata_error"
            },
            detail,
        );
        failure.code = status.code();
        Err(failure)
    }
}

pub(super) fn cache_file(kind: &str, identity: &str) -> PathBuf {
    root().join("worktree-cache").join(format!(
        "{}-{:x}.json",
        kind,
        Sha256::digest(identity.as_bytes())
    ))
}
fn load(path: &Path) -> Option<Value> {
    fs::metadata(path)
        .ok()
        .filter(|m| m.len() <= 2 * MAX_OUTPUT as u64)?;
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
/// Existing recent exact-path evidence only. Directory browsing never runs Git
/// merely to suggest a project; absent/stale snapshots do not relate worktrees.
pub(super) fn cached_catalog(path: &Path) -> Option<Value> {
    let path = crate::platform::canonical(path).ok()?;
    let value = load(&cache_file("path", &path.to_string_lossy()))?;
    let age = now() - value["sampled_at"].as_f64()?;
    (value["path"].as_str() == path.to_str() && value["state"] == "ok"
        && value["stale"] == false && value["partial"] == false && (0.0..TTL).contains(&age)).then_some(value)
}
pub(super) fn canonical(path: &str) -> PathBuf {
    crate::platform::canonical(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn parse(bytes: &[u8]) -> std::result::Result<(Vec<Value>, bool), GitError> {
    let mut trees = Vec::new();
    let mut row = serde_json::Map::new();
    let mut partial = false;
    for field in bytes.split(|b| *b == 0) {
        if field.is_empty() {
            if row.is_empty() {
                continue;
            }
            if !row.contains_key("path") {
                return Err(error("metadata_error", "Invalid worktree record"));
            }
            if trees.len() == MAX_TREES {
                partial = true;
                row.clear();
                break;
            }
            let path = row["path"].as_str().unwrap();
            let root = canonical(path);
            row.insert("available".into(), json!(root.is_dir()));
            row.insert("path".into(), json!(root));
            row.insert(
                "kind".into(),
                json!(if row.get("bare") == Some(&json!(true)) {
                    "bare"
                } else if trees.is_empty() {
                    "main"
                } else {
                    "linked"
                }),
            );
            trees.push(Value::Object(std::mem::take(&mut row)));
            continue;
        }
        let value = std::str::from_utf8(field)
            .map_err(|_| error("metadata_error", "Git returned a path that is not UTF-8"))?;
        let (key, value) = value.split_once(' ').unwrap_or((value, ""));
        match key {
            "worktree" => {
                if !Path::new(value).is_absolute() {
                    return Err(error(
                        "metadata_error",
                        "Git returned a relative worktree path",
                    ));
                }
                row.insert("path".into(), json!(value));
            }
            "HEAD" => {
                row.insert("head".into(), json!(value));
            }
            "branch" => {
                row.insert(
                    "branch".into(),
                    json!(value.strip_prefix("refs/heads/").unwrap_or(value)),
                );
            }
            "bare" | "detached" => {
                row.insert(key.into(), json!(true));
            }
            "locked" | "prunable" => {
                row.insert(key.into(), json!(true));
                row.insert(format!("{key}_reason"), json!(value));
            }
            _ => {}
        }
    }
    if !row.is_empty() {
        return Err(error("metadata_error", "Incomplete worktree record"));
    }
    if trees.is_empty() {
        return Err(error(
            "metadata_error",
            "Git returned an empty worktree catalog",
        ));
    }
    Ok((trees, partial))
}

/// Current checkouts of the repository at `path`, whose common directory is `common`.
pub(super) fn list(
    path: &Path,
    common: &Path,
    deadline: Instant,
) -> std::result::Result<(Vec<Value>, bool), GitError> {
    let bytes = git(path, &["worktree", "list", "--porcelain", "-z"], deadline)?;
    let (mut trees, partial) = parse(&bytes)?;
    // Git's porcelain reports the gitdir for a submodule's main
    // checkout. Resolve core.worktree through Git; never offer the
    // metadata directory itself as a working folder.
    if trees[0]["kind"] == "main" && trees[0]["path"] == json!(common) {
        let working = git(common, &["rev-parse", "--show-toplevel"], deadline)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| canonical(s.trim_end_matches('\n')));
        if let Some(working) = working.filter(|p| p.is_absolute() && p != common) {
            trees[0]["available"] = json!(working.is_dir());
            trees[0]["path"] = json!(working);
        } else {
            trees[0]["available"] = json!(false);
        }
    }
    Ok((trees, partial))
}

pub(super) fn catalog(path: &Path, refresh: bool) -> Value {
    catalog_until(path, refresh, Instant::now() + Duration::from_secs(3))
}
// Only a publication newer than this forced request can replace its own
// refresh. This is repository-list evidence, never the selected path's identity.
fn refreshed_while_waiting(
    value: &Value,
    previous: Option<&Value>,
    common: &Path,
    started: f64,
) -> bool {
    let Some(sample) = value["sampled_at"]
        .as_f64()
        .filter(|sample| sample.is_finite())
    else {
        return false;
    };
    let age = now() - sample;
    value["state"] == "ok"
        && value["partial"] == false
        && value["common_dir"] == json!(common)
        && started.is_finite()
        && sample > started
        && (0.0..TTL).contains(&age)
        && previous.is_none_or(|old| {
            old["sampled_at"]
                .as_f64()
                .is_some_and(|old| old.is_finite() && sample > old)
        })
        && value["worktrees"].as_array().is_some_and(|rows| {
            !rows.is_empty()
                && rows.len() <= MAX_TREES
                && rows.iter().all(|row| {
                    row["path"]
                        .as_str()
                        .is_some_and(|path| Path::new(path).is_absolute())
                        && row["available"].is_boolean()
                        && matches!(row["kind"].as_str(), Some("main" | "linked" | "bare"))
                        && ["head", "branch", "locked_reason", "prunable_reason"]
                            .iter()
                            .all(|key| row.get(*key).is_none_or(Value::is_string))
                        && ["bare", "detached", "locked", "prunable"]
                            .iter()
                            .all(|key| row.get(*key).is_none_or(Value::is_boolean))
                })
        })
}
pub(super) fn catalog_until(path: &Path, refresh: bool, deadline: Instant) -> Value {
    let started = now();
    let requested = path.to_string_lossy();
    let path_cache = cache_file("path", &requested);
    let previous = load(&path_cache);
    let path = match crate::platform::canonical(path) {
        Ok(p) if p.is_dir() => p,
        _ => {
            return json!({"path":requested,"state":"folder_unavailable","detail":"Folder is missing or inaccessible","worktrees":[],"sampled_at":now()})
        }
    };
    let result = (|| {
        let common = git(
            &path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            deadline,
        )?;
        let common = std::str::from_utf8(&common)
            .map_err(|_| error("metadata_error", "Invalid repository path"))?
            .trim_end_matches('\n');
        let common =
            crate::platform::canonical(common).map_err(|e| error("metadata_error", e.to_string()))?;
        let identity = common.to_string_lossy();
        let cache = cache_file("repo", &identity);
        let mut snapshot = load(&cache);
        let fresh = |v: &Value| {
            let age = now() - v["sampled_at"].as_f64().unwrap_or(0.);
            (0.0..TTL).contains(&age) && v["state"] == "ok"
        };
        if refresh || !snapshot.as_ref().is_some_and(fresh) {
            let _ = private_dir(cache.parent().unwrap());
            let guard = lock_file(&cache.with_extension("lock")).ok();
            let mut coalesced = false;
            let _lock = match guard {
                Some(guard) => {
                    let mut waited = false;
                    loop {
                        match guard.try_lock_exclusive() {
                            Ok(()) => break,
                            Err(e) if crate::platform::lock_contended(&e) => {
                                waited = true;
                                if Instant::now() >= deadline {
                                    return Err(error("timeout", "Catalog is being refreshed"));
                                }
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(e) => return Err(error("metadata_error", e.to_string())),
                        }
                    }
                    let guard = crate::platform::FileLock::held(guard);
                    #[cfg(all(test, unix))]
                    crate::platform::residual_lock_tests::reached("catalog-acquired");
                    if !refresh {
                        snapshot = load(&cache);
                    } else if waited {
                        if let Some(current) = load(&cache).filter(|value| {
                            refreshed_while_waiting(value, snapshot.as_ref(), &common, started)
                        }) {
                            snapshot = Some(current);
                            coalesced = true;
                        }
                    }
                    Some(guard)
                }
                None => None,
            };
            if Instant::now() >= deadline {
                return Err(error("timeout", "Catalog verification timed out"));
            }
            if (refresh && !coalesced) || !snapshot.as_ref().is_some_and(fresh) {
                let (trees, partial) = list(&path, &common, deadline)?;
                let value = json!({"state":"ok","common_dir":common,"worktrees":trees,"partial":partial,"sampled_at":now()});
                #[cfg(all(test, unix))]
                crate::platform::residual_lock_tests::reached("catalog-publish");
                let _ = atomic(&cache, &value.to_string());
                snapshot = Some(value);
            }
            if coalesced {
                // The selected checkout may have changed while its repository
                // lock was held elsewhere. Never borrow another path's proof.
                let current = git(&path, &["rev-parse", "--path-format=absolute", "--git-common-dir"], deadline)?;
                let current = std::str::from_utf8(&current)
                    .map_err(|_| error("metadata_error", "Invalid repository path"))?;
                let current = crate::platform::canonical(current.trim_end_matches('\n'))
                    .map_err(|e| error("metadata_error", e.to_string()))?;
                if current != common {
                    return Err(error("metadata_error", "Repository changed during catalog refresh"));
                }
            }
        }
        let mut snapshot =
            snapshot.ok_or_else(|| error("metadata_error", "No worktree snapshot"))?;
        if Instant::now() >= deadline {
            return Err(error("timeout", "Catalog verification timed out"));
        }
        // Discovery is on the agent's machine and identifies nested cwd and symlinks.
        let selected_root = match git(&path, &["rev-parse", "--show-toplevel"], deadline) {
            Err(e) if e.state == "timeout" => return Err(e),
            other => other,
        };
        snapshot["selected_root"] = selected_root
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|p| json!(canonical(p.trim_end_matches('\n'))))
            .unwrap_or(Value::Null);
        let selected_branch = match git(&path, &["symbolic-ref", "--quiet", "--short", "HEAD"], deadline) {
            Err(e) if e.state == "timeout" => return Err(e),
            other => other,
        };
        snapshot["selected_branch"] = selected_branch
            .ok().and_then(|bytes|String::from_utf8(bytes).ok())
            .map(|branch|json!(branch.trim_end_matches('\n'))).unwrap_or(Value::Null);
        snapshot["path"] = json!(path);
        snapshot["stale"] = json!(false);
        // Partial-removal records of this repository, read fresh and never
        // cached with the listing: a reader reviews the repository for them.
        if let Ok(count) = super::worktree_maintenance::removal_count(&common) {
            snapshot["leftovers"] = json!(count);
        }
        if Instant::now() >= deadline {
            return Err(error("timeout", "Catalog verification timed out"));
        }
        Ok::<_, GitError>(snapshot)
    })();
    match result {
        Ok(mut snapshot) => {
            snapshot["creation_receipts_v1"] = json!(true);
            let _ = atomic(&path_cache, &snapshot.to_string());
            snapshot
        }
        Err(e) => {
            let mut value = if e.state != "not_repo" {
                previous
                    .filter(|v| v["state"] == "ok")
                    .unwrap_or(json!({"worktrees":[],"sampled_at":now()}))
            } else {
                json!({"worktrees":[],"sampled_at":now()})
            };
            value["path"] = json!(path);
            value["state"] = json!(e.state);
            value["detail"] = json!(e.detail);
            value["stale"] = json!(!value["worktrees"].as_array().is_none_or(|a| a.is_empty()));
            value
        }
    }
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let action = args.first().map(String::as_str);
    if action == Some("program-cleanup-status") {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let value = super::worktree_programs::cleanup_status(&args[1..])?;
            println!("{}", serde_json::to_string(&value).map_err(|e| e.to_string())?);
            return Ok(0);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("Native program cleanup is unavailable on this computer".into());
    }
    if action == Some("program-cleanup") {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return super::worktree_programs::cleanup(&args[1..]);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("Native program cleanup is unavailable on this computer".into());
    }
    if action == Some("program-stop-status") {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let value = super::worktree_programs::stop_status(&args[1..])?;
            println!("{}", serde_json::to_string(&value).map_err(|e| e.to_string())?);
            return Ok(0);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("Native program stop is unavailable on this computer".into());
    }
    if action == Some("program-stop") {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return super::worktree_programs::stop(&args[1..]);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("Native program stop is unavailable on this computer".into());
    }
    if action == Some("program-holder") {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return super::worktree_programs::hold(&args[1..]);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("Native worktree custody is unavailable on this computer".into());
    }
    if let Some(
        action @ ("create" | "status" | "cancel" | "review" | "inspect" | "remove" | "keep-folder"
        | "leftovers" | "delete-leftover" | "policy" | "sweep" | "programs"),
    ) = action
    {
        let value = match action {
            "create" if args.iter().any(|arg| arg == "--receipt") => super::worktree_requests::create(&args[1..])?,
            "create" if super::worktree_requests::has_receipt_id(&args[1..]) => creation_cli::outcome(super::worktree_requests::create_direct(&args[1..])?)?,
            "create" => create(&args[1..])?,
            "status" | "cancel" => super::worktree_requests::control(action, &args[1..])?,
            "review" => super::worktree_cleanup::review_command(&args[1..])?,
            "programs" => {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                { super::worktree_programs::discover(&args[1..])? }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                { return Err("Native worktree program discovery is unavailable on this computer".into()); }
            }
            "keep-folder" => {
                let (result, attempted) = super::worktree_cleanup::keep_folder_command(&args[1..]);
                if let Err(detail) = &result {
                    if args.iter().any(|s| s == "--json") {
                        let option =
                            |key| args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str());
                        println!(
                            "{}",
                            json!({"operation":"worktree_leftover_keep","outcome_version":1,
                            "status":if attempted {"unconfirmed"} else {"kept"},
                            "path":option("--path"),"common_dir":option("--common-dir"),
                            "request_id":option("--request-id"),"detail":detail})
                        );
                    }
                }
                result?
            }
            "leftovers" => super::worktree_cleanup::leftovers_command(&args[1..])?,
            "delete-leftover" => {
                let (result, attempted) =
                    super::worktree_cleanup::delete_leftover_command(&args[1..]);
                if let Err(detail) = &result {
                    if args.iter().any(|s| s == "--json") {
                        let option =
                            |key| args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str());
                        println!(
                            "{}",
                            json!({"operation":"worktree_leftover_delete","outcome_version":1,
                            "status":if attempted {"unconfirmed"} else {"kept"},
                            "path":option("--path"),"common_dir":option("--common-dir"),
                            "request_id":option("--request-id"),"detail":detail})
                        );
                    }
                }
                result?
            }
            "inspect" => super::worktree_inspection::command(&args[1..])?,
            "policy" => super::worktree_maintenance::command(&args[1..])?,
            "sweep" if args[1..].iter().all(|a| a=="--json") => super::worktree_maintenance::Sweep::default().tick()?,
            "sweep" => return Err("usage: hgs worktrees sweep --json (one bounded automatic cleanup step)".into()),
            _ => {
                let (result, attempted) = super::worktree_cleanup::remove_with_outcome(&args[1..]);
                if let Err(detail) = &result {
                    if args.iter().any(|s| s == "--json") {
                        let option = |key| args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str());
                        println!("{}", json!({"operation":"worktree_remove","outcome_version":1,
                            "status":if attempted {"unconfirmed"} else {"kept"},
                            "path":option("--path"),"common_dir":option("--common-dir"),
                            "request_id":option("--request-id"),"detail":detail}));
                    }
                }
                result?
            },
        };
        println!("{value}");
        return Ok(0);
    }
    let mut path = None;
    let mut refresh = false;
    let mut creation=false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--path" => {
                i += 1;
                path = Some(PathBuf::from(args.get(i).ok_or("--path needs a folder")?));
            }
            "--refresh" => refresh = true,
            "--creation" => creation=true,
            "--json" => {}
            _ => return Err("usage: hgs worktrees --path PATH [--refresh] [--json]".into()),
        }
        i += 1;
    }
    let path = path.ok_or("--path is required")?;
    if !path.is_absolute() {
        return Err("worktree catalog requires an absolute folder path".into());
    }
    let mut value = catalog(&path, refresh);
    if creation { value["worktrees"]=json!([]); }
    value["machine"] = json!(crate::config::Config::load()
        .map_err(|e| e.to_string())?
        .host());
    println!("{value}");
    Ok(0)
}

/// The remote upstream of the local branch a new worktree starts from, as Git's
/// `--track=inherit` would copy it, so the new branch's publication can be
/// verified. Commits, tags, detached HEAD and local upstreams have none. Under
/// push.default=upstream or tracking a bare `git push` from the new branch
/// would update the base branch, so it keeps no upstream there either.
fn inherited_upstream(read: &dyn Fn(&[&str]) -> Result<String>, base: &str) -> Option<(String, String)> {
    let full = read(&["rev-parse", "--verify", "--symbolic-full-name", "--end-of-options", base]).ok()?;
    let name = full.strip_prefix("refs/heads/")?;
    let remote = read(&["config", "--get", &format!("branch.{name}.remote")]).ok()?;
    let merge = read(&["config", "--get", &format!("branch.{name}.merge")]).ok()?;
    let push = read(&["config", "--get", "push.default"]).unwrap_or_default();
    if remote.is_empty() || remote == "." || !merge.starts_with("refs/heads/") || ["upstream", "tracking"].contains(&push.as_str()) {
        return None;
    }
    Some((remote, merge))
}

/// U34: a DeepSeek member's sandbox needs full control of the folder it
/// confines: on Windows its write grant needs WRITE_DAC and WRITE_OWNER, which
/// a folder on a second drive does not inherit. Only the folder this call
/// created is changed, and a failure is a member error, never a silent skip.
/// N9: the grant is made only when the caller asks for it (a dsh member); every
/// other worktree, including the desktop's for Claude or Codex, is left as it
/// is, so a share or a file system without ACL support cannot turn a created
/// worktree into a failed create. Other systems have nothing to grant.
#[cfg(windows)]
fn grant_for_sandbox(folder: &Path) -> Result<()> {
    crate::platform::grant_full_control(folder).map_err(|error| {
        format!(
            "The folder was created, but Zerus could not give this user full control of it, which the DeepSeek sandbox needs: {error}"
        )
    })
}
#[cfg(not(windows))]
fn grant_for_sandbox(_: &Path) -> Result<()> {
    Ok(())
}

// N9: records the folders a test asked a grant for, so the scope is testable.
#[cfg(test)]
thread_local! {
    static SANDBOX_GRANTS: std::cell::RefCell<Vec<std::path::PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(super) fn granted_worktrees() -> Vec<std::path::PathBuf> {
    SANDBOX_GRANTS.with(|grants| std::mem::take(&mut *grants.borrow_mut()))
}

/// N9: the grant a `--sandbox` caller asks for, and nothing for any other
/// worktree.
fn grant_if_requested(sandbox: bool, folder: &Path) -> Result<()> {
    if !sandbox {
        return Ok(());
    }
    #[cfg(test)]
    SANDBOX_GRANTS.with(|grants| grants.borrow_mut().push(folder.to_path_buf()));
    grant_for_sandbox(folder)
}

/// A full clone of a large repository on Windows can take many minutes; its
/// own deadline is not shared with the steps after it.
const CLONE_SECONDS: u64 = 25 * 60;
/// The member's branch is checked out after the clone, with its own deadline.
const CHECKOUT_SECONDS: u64 = 10 * 60;

// A stand-in for a slow clone: runs after `git clone` returns.
#[cfg(test)]
thread_local! {
    static AFTER_CLONE: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
    static BEFORE_CREATE_READ: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

/// Why `destination` is not a clone of `source` that a create may finish:
/// one `--clone` made (its own `.git`, its origin `source`), without the
/// member's branch yet and without changes. Such a clone is left when an
/// earlier create ran out of time after `git clone`; a relaunch finishes it
/// instead of failing the member again.
fn unfinished_clone(destination: &Path, source: &Path, reference: &str, deadline: Instant) -> std::result::Result<(), String> {
    let read = |args: &[&str]| {
        git(destination, args, deadline)
            .map(|bytes| String::from_utf8_lossy(&bytes).trim_end_matches('\n').to_owned())
            .map_err(|e| e.detail)
    };
    let same = |left: &str, right: &Path| {
        let left = left.strip_prefix("file://").unwrap_or(left);
        crate::platform::canonical(left).is_ok_and(|left| crate::platform::canonical(right).is_ok_and(|right| left == right))
    };
    if !fs::symlink_metadata(destination).is_ok_and(|meta| meta.is_dir()) {
        return Err("it is not a folder".into());
    }
    let top = read(&["rev-parse", "--show-toplevel"]).map_err(|_| "it is not a Git checkout".to_owned())?;
    if !same(&top, destination) {
        return Err("it is inside another checkout".into());
    }
    let common = read(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if !same(&common, &destination.join(".git")) {
        return Err("it is a worktree, not a clone".into());
    }
    let origin = read(&["config", "--get", "remote.origin.url"]).map_err(|_| "it has no origin".to_owned())?;
    if !same(&origin, source) {
        return Err("its origin is another repository".into());
    }
    if read(&["rev-parse", "--verify", "--quiet", reference]).is_ok() {
        return Err("its branch exists already".into());
    }
    if !read(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("it has changes".into());
    }
    Ok(())
}

/// New external steps of an owned (service) worktree creation, each checked
/// by the admitted desktop actor. The direct CLI path has no such checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))] // The owned executor is POSIX-only.
pub(crate) enum Step {
    Reserve,
    Destination,
    Checkout,
    Verify,
    Track,
    Own,
}
impl Step {
    #[cfg_attr(not(all(test, target_os = "linux")), allow(dead_code))] // Read by the actor's ledger.
    pub(crate) fn ordinal(self) -> usize {
        match self {
            Self::Reserve => 0,
            Self::Destination => 1,
            Self::Checkout => 2,
            Self::Verify => 3,
            Self::Track => 4,
            Self::Own => 5,
        }
    }
}

/// The service's per-step actor check and original custody. It is never
/// constructed from CLI arguments or imported recovery records.
#[cfg_attr(not(all(test, target_os = "linux")), allow(dead_code))] // Built by the worktree actor.
pub(crate) struct Control<'a> {
    pub before: &'a dyn Fn(Step) -> Result<()>,
    #[cfg(unix)]
    pub cancelled: &'a std::sync::atomic::AtomicBool,
    #[cfg(unix)]
    pub custody: &'a std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
}

/// Pinned directories and checks of an owned creation; Git runs in the
/// deadline-owning worker under the effect's custody.
#[cfg(unix)]
struct Owned<'a> {
    control: &'a Control<'a>,
    source: super::repositories::directory::Directory,
    parent: super::repositories::directory::Directory,
}
#[cfg(unix)]
impl Owned<'_> {
    fn before(&self, step: Step) -> Result<()> {
        if self
            .control
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("Worktree creation stopped".into());
        }
        (self.control.before)(step)
    }
    fn git(
        &self,
        directory: &super::repositories::directory::Directory,
        args: &[&str],
        seconds: u64,
    ) -> Result<Vec<u8>> {
        self.git_sharing(directory, &[], Default::default(), args, seconds)
    }
    /// Git in `directory` that also holds each `share` as its own descriptor,
    /// with `bind` naming them as its `GIT_DIR` and `GIT_COMMON_DIR`.
    fn git_sharing(
        &self,
        directory: &super::repositories::directory::Directory,
        share: &[&super::repositories::directory::Directory],
        bind: super::repositories::Bind,
        args: &[&str],
        seconds: u64,
    ) -> Result<Vec<u8>> {
        self.source.verify()?;
        self.parent.verify()?;
        super::repositories::owned_git(
            directory,
            share,
            bind,
            args,
            seconds,
            self.control.cancelled,
            self.control.custody,
        )
    }
}

#[cfg(all(test, unix))]
type Hook = std::cell::RefCell<Option<Box<dyn FnOnce()>>>;
#[cfg(all(test, unix))]
thread_local! {
    /// Runs after the checkout's final identity checks, just before Git
    /// starts: the window where only a descriptor-bound Git is safe.
    pub(super) static BEFORE_OWNED_CHECKOUT: Hook = const { std::cell::RefCell::new(None) };
    /// Runs after the tree checkout's final identity checks, just before Git
    /// starts: the second window where only descriptor-bound Git is safe.
    pub(super) static BEFORE_OWNED_TREE: Hook = const { std::cell::RefCell::new(None) };
}
#[cfg(all(test, unix))]
fn hook(slot: &'static std::thread::LocalKey<Hook>) {
    if let Some(hook) = slot.with(|hook| hook.borrow_mut().take()) {
        hook()
    }
}

/// The metadata entry Git just made for the pinned checkout, pinned before any
/// later check or write: named by the gitfile read through the pinned
/// destination, opened through the pinned common directory, and accepted only
/// when its own backlink, branch and common link are that checkout's.
#[cfg(unix)]
fn owned_metadata(
    destination: &super::repositories::directory::Directory,
    common: &super::repositories::directory::Directory,
    branch: &str,
) -> Result<super::repositories::directory::Directory> {
    let invalid = || "Cannot verify linked worktree metadata".to_string();
    let gitfile = destination.read_file(".git", 4096)?;
    let gitfile = std::str::from_utf8(&gitfile).map_err(|_| invalid())?;
    let named = gitfile
        .strip_prefix("gitdir: ")
        .and_then(|value| value.strip_suffix('\n'))
        .map(Path::new)
        .ok_or_else(invalid)?;
    if named.parent() != Some(common.path.join("worktrees").as_path()) {
        return Err(invalid());
    }
    let name = named
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let entry = common.child("worktrees")?.child(name)?;
    check_metadata(&entry, destination, branch)?;
    Ok(entry)
}

/// Whether `git version` output names a Git whose `worktree add
/// --no-checkout` starts no child through an absolute Git directory: from
/// 2.44 it creates the new worktree's HEAD in-process (2.36-2.43 run `git
/// symbolic-ref` with the gitfile's real path). It also has `git hook run`.
#[cfg(unix)]
fn owns_checkouts(version: &str) -> bool {
    let mut parts = version
        .strip_prefix("git version ")
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_digit())
        .map(|part| part.parse::<u32>().ok());
    matches!((parts.next(), parts.next()), (Some(Some(major)), Some(Some(minor))) if (major, minor) >= (2, 44))
}
#[cfg(all(test, unix))]
#[test]
fn only_git_that_adds_worktrees_in_process_owns_a_checkout() {
    for (version, runs) in [
        ("git version 2.56.0", true),
        ("git version 2.44.0", true),
        ("git version 2.43.7", false),
        ("git version 2.36.0", false),
        ("git version 3.0.1 (Apple Git-200)", true),
        ("git version 2.35.8", false),
        ("git version 2.9.5", false),
        ("git version unknown", false),
        ("", false),
    ] {
        assert_eq!(owns_checkouts(version), runs, "{version}");
    }
}

/// The pinned entry's own backlink, branch and common link, read through it.
#[cfg(unix)]
fn check_metadata(
    entry: &super::repositories::directory::Directory,
    destination: &super::repositories::directory::Directory,
    branch: &str,
) -> Result<()> {
    let read = |file: &str| {
        entry
            .read_file(file, 4096)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    if read("gitdir") != Some(format!("{}\n", destination.path.join(".git").display()))
        || read("HEAD") != Some(format!("ref: refs/heads/{branch}\n"))
        || read("commondir").as_deref() != Some("../..\n")
    {
        return Err("Cannot verify linked worktree metadata".into());
    }
    Ok(())
}

pub(super) fn create(args: &[String]) -> Result<Value> {
    create_with(args, None)
}

pub(super) fn create_with(args: &[String], control: Option<&Control<'_>>) -> Result<Value> {
    if super::worktree_requests::has_receipt_id(args) { super::worktree_requests::original_current(args)?; }
    let mobile = args.iter().any(|value| value == "--mobile");
    if mobile && control.is_some() {
        return Err("An owned worktree creation has no mobile form".into());
    }
    // Git binds the pinned common directory through its own descriptor
    // (/proc/self/fd), which only Linux offers: elsewhere the owned effect is
    // refused before anything starts.
    if control.is_some() && !cfg!(target_os = "linux") {
        return Err("creation_worktree_unsupported".into());
    }
    let _signals = mobile.then(MobileSignals::install);
    let total_deadline = Instant::now() + Duration::from_secs(if mobile { 25 } else { 315 });
    // A step's own deadline: a desktop clone's steps each get theirs; a phone
    // request keeps its one short total.
    let step = |desktop: u64, phone: u64| {
        if mobile { total_deadline.min(Instant::now() + Duration::from_secs(phone)) } else { Instant::now() + Duration::from_secs(desktop) }
    };
    let mut options = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" | "--mobile" | "--clone" | "--sandbox" => {
                if args[i] == "--clone" {
                    options.insert("--clone", "true");
                }
                if args[i] == "--sandbox" {
                    options.insert("--sandbox", "true");
                }
            }
            key @ ("--path" | "--destination" | "--branch" | "--base" | "--common-dir" | "--request-id") => {
                i += 1;
                let value = args.get(i).ok_or_else(|| format!("{key} needs a value"))?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err(format!("{key} cannot be empty or contain control characters"));
                }
                if options.insert(key, value.as_str()).is_some() { return Err(format!("duplicate {key}")); }
            }
            _ => return Err("usage: hgs worktrees create --path REPO --destination FOLDER --branch NEW_BRANCH [--base HEAD] [--clone] [--sandbox] [--json]".into()),
        }
        i += 1;
    }
    let required = |key| {
        options
            .get(key)
            .copied()
            .ok_or_else(|| format!("{key} is required"))
    };
    let source = Path::new(required("--path")?);
    let destination = Path::new(required("--destination")?);
    if !source.is_absolute() || !destination.is_absolute() {
        return Err("Repository and destination must be absolute folder paths".into());
    }
    let source = crate::platform::canonical(source).map_err(|e| format!("Repository: {e}"))?;
    let parent =
        crate::platform::canonical(destination.parent().ok_or("Choose a destination folder")?)
            .map_err(|e| format!("Destination parent must already exist: {e}"))?;
    let name = destination
        .file_name()
        .ok_or("Choose a destination folder")?
        .to_owned();
    let destination = parent.join(&name);
    let existing = fs::symlink_metadata(&destination).is_ok();
    if existing && !options.contains_key("--clone") {
        return Err("Destination already exists. Choose a new folder.".into());
    }
    // The owned path pins the repository and the destination's parent now and
    // rechecks both around every Git command and step.
    #[cfg(unix)]
    let owned = match control {
        Some(control) => Some(Owned {
            control,
            source: super::repositories::directory::Directory::open(&source)?,
            parent: super::repositories::directory::Directory::open(&parent)?,
        }),
        None => None,
    };
    let read = |args: &[&str]| -> Result<String> {
        // Each read keeps its own window within the operation deadline. Task
        // #32: a desktop read gets a minute (its 5 s ran out under load: "git
        // for-each-ref timed out or was cancelled"); a phone keeps 5 s.
        let window = if mobile { Duration::from_secs(5) } else { read_window(CREATE_READ_SECONDS) };
        let deadline = total_deadline.min(Instant::now() + window);
        #[cfg(test)]
        if let Some(delay) = BEFORE_CREATE_READ.with(std::cell::Cell::get) { delay(); }
        #[cfg(unix)]
        let bytes = match &owned {
            Some(owned) => owned.git(&owned.source, args, window.as_secs())?,
            None => git(&source, args, deadline).map_err(|e| e.detail)?,
        };
        #[cfg(not(unix))]
        let bytes = git(&source, args, deadline).map_err(|e| e.detail)?;
        String::from_utf8(bytes)
            .map(|s| s.trim_end_matches('\n').to_owned())
            .map_err(|e| e.to_string())
    };
    let common = crate::platform::canonical(read(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?)
    .map_err(|e| e.to_string())?;
    if let Some(expected) = options.get("--common-dir") {
        if crate::platform::canonical(expected).ok().as_ref() != Some(&common) {
            return Err("Repository changed. Refresh its worktrees before creating one.".into());
        }
    }
    if destination.starts_with(&common) {
        return Err("Destination cannot be inside Git metadata".into());
    }
    // The owned path also pins the repository's common Git directory. Every
    // later Git write runs inside it or is verified against it.
    #[cfg(unix)]
    let common_pin = match &owned {
        Some(_) => Some(super::repositories::directory::Directory::open(&common)?),
        None => None,
    };
    // Before Git 2.44, `worktree add --no-checkout` still starts a child
    // through the gitfile's absolute real path; older Git is refused here,
    // before any effect.
    #[cfg(unix)]
    if owned.is_some() && !owns_checkouts(&read(&["version"])?) {
        return Err("Owned worktree creation needs Git 2.44 or later.".into());
    }
    let branch = required("--branch")?;
    if branch.starts_with('-') {
        return Err("Branch name cannot start with '-'".into());
    }
    let reference = format!("refs/heads/{branch}");
    read(&["check-ref-format", &reference]).map_err(|_| "Invalid new branch name".to_string())?;
    read(&["check-ref-format", "--branch", branch])
        .map_err(|_| "Invalid new branch name".to_string())?;
    if branch_taken(&read, &reference)? {
        return Err(format!(
            "Branch '{branch}' already exists or conflicts with an existing branch. Choose a new branch name."
        ));
    }
    let base = options.get("--base").copied().unwrap_or("HEAD");
    let head = read(&[
        "rev-parse",
        "--verify",
        "--end-of-options",
        &format!("{base}^{{commit}}"),
    ])
    .map_err(|e| format!("Cannot resolve starting revision '{base}': {e}"))?;
    let upstream = inherited_upstream(&read, base);
    // An unfinished clone of this repository is finished; anything else
    // there stays untouched.
    if existing {
        unfinished_clone(&destination, &source, &reference, step(CHECKOUT_SECONDS, 5)).map_err(|why| {
            format!("Destination already exists and is not an unfinished clone of this repository ({why}). Choose a new folder.")
        })?;
    }
    let cache = cache_file("repo", &common.to_string_lossy());
    crate::platform::private_dir(cache.parent().unwrap()).map_err(|e| e.to_string())?;
    let guard = crate::platform::private_open(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    )
    .open(cache.with_extension("lock"))
    .map_err(|e| e.to_string())?;
    let deadline = total_deadline.min(Instant::now() + Duration::from_secs(5));
    while guard.try_lock_exclusive().is_err() {
        if Instant::now() >= deadline {
            return Err(
                "Another worktree operation is running. Try again after it finishes.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _lock = crate::platform::FileLock::held(guard);
    // Atomic reservation rejects simultaneous creators and existing/symlinked
    // destinations. Git accepts this empty directory but never overwrites files.
    if mobile_stopped() || Instant::now() >= total_deadline {
        return Err("Worktree creation cancelled before checkout".into());
    }
    #[cfg(unix)]
    if let Some(owned) = &owned {
        owned.before(Step::Destination)?;
    }
    super::worktree_requests::reserve_checkout()?;
    // N9: only a folder made for a sandboxed member is granted to it.
    let sandbox = options.contains_key("--sandbox");
    if options.contains_key("--clone") {
        if !existing {
            fs::create_dir(&destination).map_err(|e| format!("Cannot reserve destination: {e}"))?;
        }
        // B4/F10: a full clone with its own .git, so commit, merge and fetch
        // work inside a sandboxed member's folder. Never a worktree.
        // `--no-hardlinks`: a sandboxed member's folder is granted full
        // control, and a hard link shares its file's ACL with the source.
        let retry = |detail: String| {
            format!("{detail}\nThe clone in {} is kept: start the member again to finish it on branch '{branch}'.", destination.display())
        };
        if !existing {
            let outcome = git(
                &source,
                &["clone", "--local", "--no-hardlinks", "--quiet", &source.to_string_lossy(), &destination.to_string_lossy()],
                step(CLONE_SECONDS, 15),
            );
            outcome.map_err(|e| retry(e.detail))?;
            #[cfg(test)]
            if let Some(slow) = AFTER_CLONE.with(std::cell::Cell::get) {
                slow();
            }
        }
        // Every step after the clone has its own deadline. They used to share
        // the 5 s one read before the clone, so a clone longer than that
        // always failed here with "Git operation cancelled or timed out".
        let checkout = step(CHECKOUT_SECONDS, 5);
        // The member's branch starts at the base commit, not at the clone's
        // default branch; an older unfinished clone fetches it first.
        let commit = format!("{head}^{{commit}}");
        if git(&destination, &["cat-file", "-e", &commit], checkout).is_err() {
            git(&destination, &["fetch", "--quiet", "origin"], checkout).map_err(|e| retry(e.detail))?;
        }
        let switch = git(&destination, &["switch", "--quiet", "-c", branch, &head], checkout);
        switch.map_err(|e| retry(e.detail))?;
        let deadline = step(30, 5);
        let actual_branch = git(&destination, &["symbolic-ref", "--quiet", "HEAD"], deadline).map_err(|e| e.detail)?;
        if String::from_utf8_lossy(&actual_branch).trim_end_matches('\n') != reference {
            return Err("Clone creation could not be confirmed. Inspect the destination before retrying.".into());
        }
        grant_if_requested(sandbox, &destination)?;
        let upstream = upstream.map(|(remote, merge)| format!("{remote}/{}", merge.strip_prefix("refs/heads/").unwrap_or(&merge)));
        return Ok(json!({"status":"created", "path":destination, "branch":branch, "base":base, "head":head,
            "upstream":upstream, "clone":true, "resumed":existing, "request_id":options.get("--request-id"),
            "machine":crate::config::Config::load().map_err(|e|e.to_string())?.host()}));
    }
    #[cfg(unix)]
    let pinned = match &owned {
        // A private staging folder is published with no-replace rename; a
        // failed publication keeps it beside the destination for review.
        Some(owned) => Some(
            owned
                .parent
                .create(
                    &name.to_string_lossy(),
                    &format!(".zerus-worktree-{}", uuid::Uuid::new_v4()),
                )
                .map_err(|e| format!("Cannot reserve destination: {e}"))?,
        ),
        None => {
            fs::create_dir(&destination).map_err(|e| format!("Cannot reserve destination: {e}"))?;
            None
        }
    };
    #[cfg(not(unix))]
    fs::create_dir(&destination).map_err(|e| format!("Cannot reserve destination: {e}"))?;
    let destination_text = destination.to_string_lossy().into_owned();
    let add = [
        "worktree",
        "add",
        "--quiet",
        "-b",
        branch,
        "--",
        destination_text.as_str(),
        head.as_str(),
    ];
    #[cfg(unix)]
    let outcome = match &owned {
        Some(owned) => owned.before(Step::Checkout).and_then(|()| {
            let (Some(pinned), Some(common_pin)) = (&pinned, &common_pin) else {
                return Err("Owned worktree pins are missing".into());
            };
            // Git is bound to every object it writes. The admitted destination
            // is its working directory (path "."), and the pinned common
            // directory is its own inherited descriptor; neither is a pathname
            // a later swap can redirect. `worktree add` would start its own
            // checkout child with an absolute GIT_DIR, so it only creates the
            // branch, entry and gitfile here (--no-checkout). Absolute links
            // keep the backlinks that Own checks exact.
            let left = || {
                total_deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1)
            };
            let git_dir = format!("--git-dir=/proc/self/fd/{}", common_pin.descriptor());
            common_pin.verify()?;
            pinned.verify()?;
            #[cfg(test)]
            hook(&BEFORE_OWNED_CHECKOUT);
            owned.git_sharing(
                pinned,
                &[common_pin],
                Default::default(),
                &[
                    "-c",
                    "worktree.useRelativePaths=false",
                    git_dir.as_str(),
                    "worktree",
                    "add",
                    "--no-checkout",
                    "--quiet",
                    "-b",
                    branch,
                    "--",
                    ".",
                    head.as_str(),
                ],
                left(),
            )?;
            common_pin.verify()?;
            pinned.verify()?;
            // Pin the metadata entry `worktree add` just made, by descriptor;
            // the checkout and Own write only through this pinned object.
            let entry = owned_metadata(pinned, common_pin, branch)?;
            // The checkout `worktree add` itself would run, with GIT_DIR and
            // GIT_COMMON_DIR bound to the pinned entry and common directory.
            // Git's files ref backend ignores that binding for a linked
            // worktree (it re-reads the entry's `commondir` and takes its real
            // path), so the tree is read with `read-tree`, which updates no
            // ref: only the index (through the entry) and the work tree. The
            // new branch already names `head`, which HEAD follows.
            let bind = super::repositories::Bind {
                git_dir: Some(entry.descriptor()),
                common_dir: Some(common_pin.descriptor()),
            };
            let zero = "0".repeat(head.len());
            for args in [
                &[
                    "--work-tree=.",
                    "read-tree",
                    "--reset",
                    "-u",
                    "--no-recurse-submodules",
                    "--quiet",
                    head.as_str(),
                ][..],
                &[
                    "--work-tree=.",
                    "hook",
                    "run",
                    "--ignore-missing",
                    "post-checkout",
                    "--",
                    zero.as_str(),
                    head.as_str(),
                    "1",
                ][..],
            ] {
                entry.verify()?;
                common_pin.verify()?;
                pinned.verify()?;
                #[cfg(test)]
                if args.contains(&"read-tree") {
                    hook(&BEFORE_OWNED_TREE);
                }
                owned.git_sharing(pinned, &[common_pin, &entry], bind, args, left())?;
            }
            entry.verify()?;
            common_pin.verify()?;
            pinned.verify()?;
            Ok(Some(entry))
        }),
        None => git(
            &source,
            &add,
            total_deadline.min(Instant::now() + Duration::from_secs(if mobile { 15 } else { 300 })),
        )
        .map(|_| None)
        .map_err(|e| e.detail),
    };
    #[cfg(not(unix))]
    let outcome = git(
        &source,
        &add,
        total_deadline.min(Instant::now() + Duration::from_secs(if mobile { 15 } else { 300 })),
    )
    .map(drop)
    .map_err(|e| e.detail);
    let _ = fs::remove_file(cache);
    let failed = |e: String| format!("{e}\nCheck {} and branch '{branch}' before retrying. Any created files and branch have been kept.", destination.display());
    #[cfg(unix)]
    let entry = outcome.map_err(failed)?;
    #[cfg(not(unix))]
    outcome.map_err(failed)?;
    // Confirm the resulting checkout, not merely the exit code (hooks may fail
    // after creation, or another process may have changed the destination).
    let deadline = total_deadline.min(Instant::now() + Duration::from_secs(5));
    let in_checkout = |args: &[&str]| -> Result<Vec<u8>> {
        #[cfg(unix)]
        if let (Some(owned), Some(pinned), Some(common_pin)) = (&owned, &pinned, &common_pin) {
            common_pin.verify()?;
            pinned.verify()?;
            let bytes = owned.git(pinned, args, 5)?;
            pinned.verify()?;
            common_pin.verify()?;
            return Ok(bytes);
        }
        git(&destination, args, deadline).map_err(|e| e.detail)
    };
    #[cfg(unix)]
    if let Some(owned) = &owned {
        owned.before(Step::Verify)?;
    }
    let actual = in_checkout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let actual =
        crate::platform::canonical(String::from_utf8_lossy(&actual).trim_end_matches('\n'))
            .map_err(|e| e.to_string())?;
    let actual_branch = in_checkout(&["symbolic-ref", "--quiet", "HEAD"])?;
    if actual != common
        || String::from_utf8_lossy(&actual_branch).trim_end_matches('\n') != reference
    {
        return Err(
            "Worktree creation could not be confirmed. Inspect the destination before retrying."
                .into(),
        );
    }
    grant_if_requested(sandbox, &destination)?;
    // Best effort: the checkout exists either way, and a missing upstream only
    // leaves publication unverified. Never keep half of the tracking pair.
    let direct_track = |upstream: Option<(String, String)>| {
        upstream.filter(|(remote, merge)| {
            let set = |key: &str, value: &str| {
                in_checkout(&["config", &format!("branch.{branch}.{key}"), value]).is_ok()
            };
            let complete = set("remote", remote) && set("merge", merge);
            if !complete {
                let _ = in_checkout(&["config", "--unset", &format!("branch.{branch}.remote")]);
            }
            complete
        })
    };
    // An owned creation checks this write as its own step and runs it inside
    // the pinned common directory, with no checkout or path indirection; any
    // failure leaves it unconfirmed instead of a silent best effort.
    #[cfg(unix)]
    let upstream = match (&owned, &common_pin, upstream) {
        (Some(owned), Some(common_pin), Some((remote, merge))) => {
            owned.before(Step::Track)?;
            for (key, value) in [("remote", &remote), ("merge", &merge)] {
                let name = format!("branch.{branch}.{key}");
                common_pin.verify()?;
                owned.git(common_pin, &["--git-dir=.", "config", name.as_str(), value.as_str()], 5)?;
                common_pin.verify()?;
            }
            Some((remote, merge))
        }
        // Track is always a checked step, so the actor's step sequence is the
        // same for every repository: without a safe inherited upstream it
        // writes nothing.
        (Some(owned), _, None) => {
            owned.before(Step::Track)?;
            None
        }
        (None, _, upstream) => direct_track(upstream),
        (Some(_), None, Some(_)) => return Err("Owned worktree pins are missing".into()),
    };
    #[cfg(not(unix))]
    let upstream = direct_track(upstream);
    let upstream = upstream.map(|(remote, merge)| {
        format!(
            "{remote}/{}",
            merge.strip_prefix("refs/heads/").unwrap_or(&merge)
        )
    });
    #[cfg(unix)]
    let managed = match &owned {
        Some(owned) => {
            owned.before(Step::Own)?;
            let (Some(pinned), Some(common_pin), Some(entry)) = (&pinned, &common_pin, &entry)
            else {
                return Err("Owned worktree pins are missing".into());
            };
            // The entry pinned right after the checkout: its links are checked
            // again through that descriptor, Git's own view of the checkout must
            // name the same object, and the owner file is written only through
            // it. Any pin, read or write failure propagates: never Created.
            check_metadata(entry, pinned, branch)?;
            let metadata = in_checkout(&["rev-parse", "--absolute-git-dir"])?;
            let path = crate::platform::canonical(String::from_utf8_lossy(&metadata).trim())
                .map_err(|e| e.to_string())?;
            if path != entry.path {
                return Err("Cannot verify linked worktree metadata".into());
            }
            entry.verify()?;
            let value = super::worktree_lifecycle::owner_value(
                &common,
                &destination,
                branch,
                &head,
                &entry.path,
            );
            // No replacement: an existing owner file is never overwritten.
            entry.write_new("zerus-owner.json", format!("{value}\n").as_bytes())?;
            entry.verify()?;
            common_pin.verify()?;
            pinned.verify()?;
            super::worktree_lifecycle::record_owner(&common, &destination, value)
        }
        None => super::worktree_lifecycle::created(&common, &destination, branch, &head)
            .map(|()| Value::Null),
    };
    #[cfg(not(unix))]
    let managed = super::worktree_lifecycle::created(&common, &destination, branch, &head)
        .map(|()| Value::Null);
    // An owned failure above has already returned: the owned path never
    // reports Created without its ownership evidence.
    let owned_evidence = control.is_some();
    let mut created = json!({"status":"created", "path":destination, "branch":branch, "base":base, "head":head,
        "upstream":upstream, "common_dir":common, "request_id":options.get("--request-id"),
        "managed_cleanup":managed.is_ok(), "cleanup_note":managed.as_ref().err(),
        "machine":crate::config::Config::load().map_err(|e|e.to_string())?.host()});
    if owned_evidence {
        created["creation_owner"] = managed.unwrap_or(Value::Null);
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_git_identifies_the_subcommand() {
        let dir = tempfile::tempdir().unwrap();
        let cases: &[&[&str]] = &[&["rev-parse", "HEAD"], &["-c", "core.hooksPath=", "rev-parse", "HEAD"]];
        for args in cases {
            let failure = git(dir.path(), args, Instant::now()).unwrap_err();
            assert_eq!(failure.state, "timeout");
            assert!(failure.detail.contains("git rev-parse timed out"), "{}", failure.detail);
        }
    }

    /// N9: the sandbox grant is made only when the caller asks for it; every
    /// other worktree is created without touching its ACLs.
    #[test]
    fn only_a_sandboxed_worktree_asks_for_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("member");
        std::fs::create_dir(&folder).unwrap();
        grant_if_requested(false, &folder).unwrap();
        assert!(granted_worktrees().is_empty(), "a Codex or Claude worktree is never granted");
        grant_if_requested(true, &folder).unwrap();
        assert_eq!(granted_worktrees(), vec![folder.clone()], "a dsh member's folder is");
    }

    /// One git command in `folder`, which must succeed; its trimmed output.
    fn run(folder: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(folder)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A repository on main with a `base` branch one commit ahead of it, the
    /// way a team's base branch differs from the folder's default branch.
    fn repository(root: &Path) -> (PathBuf, String, String) {
        let source = root.join("project");
        fs::create_dir(&source).unwrap();
        run(&source, &["init", "-q"]);
        run(&source, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        fs::write(source.join("a.txt"), "a").unwrap();
        run(&source, &["add", "-A"]);
        run(&source, &["commit", "-qm", "main"]);
        let main = run(&source, &["rev-parse", "HEAD"]);
        run(&source, &["switch", "-q", "-c", "base"]);
        fs::write(source.join("b.txt"), "b").unwrap();
        run(&source, &["add", "-A"]);
        run(&source, &["commit", "-qm", "base"]);
        let base = run(&source, &["rev-parse", "HEAD"]);
        run(&source, &["switch", "-q", "main"]);
        (source, main, base)
    }

    /// `hgs worktrees create --clone` of `source` into `destination`, on
    /// `team/x/dev` from the `base` branch, with its own state folder.
    fn clone_member(source: &Path, destination: &Path) -> Result<Value> {
        let state = destination.parent().unwrap().join("state");
        std::env::set_var("HGS_STATE_DIR", &state);
        let args: Vec<String> = ["worktrees", "create", "--clone", "--path", &source.display().to_string(), "--branch", "team/x/dev",
            "--destination", &destination.display().to_string(), "--base", "base", "--json"]
            .iter()
            .skip(2)
            .map(|arg| arg.to_string())
            .collect();
        create(&args)
    }

    #[test]
    fn pre_clone_reads_each_have_their_own_deadline() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("HGS_STATE_DIR").ok();
        let dir = tempfile::tempdir().unwrap();
        let (source, _, base) = repository(dir.path());
        let destination = dir.path().join("member");
        // At least five reads spend 5.5 seconds in delays, each within its window.
        BEFORE_CREATE_READ.with(|delay| delay.set(Some(|| std::thread::sleep(Duration::from_millis(1100)))));
        let started = Instant::now();
        let created = clone_member(&source, &destination);
        BEFORE_CREATE_READ.with(|delay| delay.set(None));
        match old {
            Some(old) => std::env::set_var("HGS_STATE_DIR", old),
            None => std::env::remove_var("HGS_STATE_DIR"),
        }
        let created = created.unwrap();
        assert!(started.elapsed() > Duration::from_secs(5), "the reads exceed one shared window");
        assert_eq!(created["head"], base);
        assert_eq!(run(&destination, &["symbolic-ref", "--short", "HEAD"]), "team/x/dev");
    }

    /// Task 20: a clone that takes longer than the old 5 s window still gets
    /// its branch, from the base branch rather than the clone's default one.
    #[test]
    fn a_slow_clone_finishes_on_the_base_branch() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("HGS_STATE_DIR").ok();
        let dir = tempfile::tempdir().unwrap();
        let (source, main, base) = repository(dir.path());
        let destination = dir.path().join("member");
        AFTER_CLONE.with(|slow| slow.set(Some(|| std::thread::sleep(Duration::from_secs(6)))));
        let created = clone_member(&source, &destination);
        AFTER_CLONE.with(|slow| slow.set(None));
        let created = created.unwrap();
        assert_eq!((created["clone"].as_bool(), created["resumed"].as_bool()), (Some(true), Some(false)));
        assert_eq!(run(&destination, &["symbolic-ref", "--short", "HEAD"]), "team/x/dev");
        assert_eq!(run(&destination, &["rev-parse", "HEAD"]), base);
        assert_ne!(base, main);
        assert!(destination.join("b.txt").exists(), "the base branch's files are checked out");
        match old {
            Some(old) => std::env::set_var("HGS_STATE_DIR", old),
            None => std::env::remove_var("HGS_STATE_DIR"),
        }
    }

    /// Task 20: a clone left without its branch by an earlier create that ran
    /// out of time is finished on relaunch; a clone with changes, or another
    /// folder, is never touched.
    #[test]
    fn an_unfinished_clone_is_finished_on_relaunch() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("HGS_STATE_DIR").ok();
        let dir = tempfile::tempdir().unwrap();
        let (source, _, base) = repository(dir.path());
        let destination = dir.path().join("member");
        // What the failed create left: the clone, on its default branch.
        run(dir.path(), &["clone", "--local", "--no-hardlinks", "--quiet", &source.display().to_string(), &destination.display().to_string()]);
        // The base branch moved on after that clone: it is fetched.
        run(&source, &["switch", "-q", "base"]);
        fs::write(source.join("c.txt"), "c").unwrap();
        run(&source, &["add", "-A"]);
        run(&source, &["commit", "-qm", "later"]);
        let base_now = run(&source, &["rev-parse", "HEAD"]);
        run(&source, &["switch", "-q", "main"]);
        assert_ne!(base, base_now);
        // Changes in it: refused, nothing changed.
        fs::write(destination.join("a.txt"), "edited").unwrap();
        let error = clone_member(&source, &destination).unwrap_err();
        assert!(error.contains("not an unfinished clone of this repository (it has changes)"), "{error}");
        assert_eq!(run(&destination, &["symbolic-ref", "--short", "HEAD"]), "main");
        run(&destination, &["checkout", "-q", "--", "a.txt"]);
        let created = clone_member(&source, &destination).unwrap();
        assert_eq!(created["resumed"].as_bool(), Some(true));
        assert_eq!(run(&destination, &["symbolic-ref", "--short", "HEAD"]), "team/x/dev");
        assert_eq!(run(&destination, &["rev-parse", "HEAD"]), base_now);
        // Its branch exists now: a second create refuses instead of moving it.
        let error = clone_member(&source, &destination).unwrap_err();
        assert!(error.contains("its branch exists already"), "{error}");
        // Another repository at the destination is refused.
        let other = dir.path().join("other");
        fs::create_dir(&other).unwrap();
        run(&other, &["init", "-q"]);
        let error = clone_member(&source, &other).unwrap_err();
        assert!(error.contains("not an unfinished clone"), "{error}");
        match old {
            Some(old) => std::env::set_var("HGS_STATE_DIR", old),
            None => std::env::remove_var("HGS_STATE_DIR"),
        }
    }
}

#[cfg(test)]
mod branch_taken_tests {
    use super::branch_taken;
    use std::cell::Cell;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn a_repository_with_thousands_of_branches_keeps_every_check_to_one_ref() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        let head = git(repo, &["rev-parse", "HEAD"]).trim().to_owned();
        // 3000 long branch names: a full refs/heads listing is far beyond the 64 KiB Git output bound.
        let mut batch = String::new();
        for i in 0..3000 {
            batch.push_str(&format!(
                "create refs/heads/work-{i:04}-{} {head}\n",
                "x".repeat(40)
            ));
        }
        for name in ["feat", "a/b", "deep/one/two"] {
            batch.push_str(&format!("create refs/heads/{name} {head}\n"));
        }
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["update-ref", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), batch.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]).len() > 65_536);

        let largest = Cell::new(0usize);
        let read = |args: &[&str]| -> crate::state::Result<String> {
            let out = git(repo, args);
            largest.set(largest.get().max(out.len()));
            Ok(out)
        };
        let taken = |branch: &str| branch_taken(&read, &format!("refs/heads/{branch}")).unwrap();
        assert!(taken("main"), "the ref itself");
        assert!(
            taken("work-0042-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            "an existing branch among thousands"
        );
        assert!(taken("feat/x"), "a leading part is a branch");
        assert!(
            taken("deep/one/two/three"),
            "a deeper leading part is a branch"
        );
        assert!(taken("a"), "a branch lives under it");
        assert!(taken("deep"), "a branch lives further under it");
        assert!(!taken("a/c"), "a sibling under the same parent is free");
        assert!(
            !taken("work-0042"),
            "a name that only shares a prefix is free"
        );
        assert!(!taken("work-aaca42cc"), "a new name is free");
        assert!(
            largest.get() < 256,
            "each check reads at most one ref, got {} bytes",
            largest.get()
        );
    }
}

#[cfg(all(test, unix))]
mod residual_lock_tests;
