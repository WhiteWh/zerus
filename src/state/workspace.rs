//! Passive, bounded workspace metadata. Never used to authorize lifecycle actions.
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn pane_cwds() -> BTreeMap<String, String> {
    cwds(None)
}

/// `pane_cwds` under the names that an earlier pane snapshot gave each session
/// ID (`$N`) in `names`. A rename keeps the ID, so a session renamed since that
/// snapshot keeps its folder under the name the snapshot listed, and one that
/// started since has none (zerus-18u1).
/// Lists the terminal `terminal` under its session's label in `names`, so its
/// pane folder goes to the row or inspection of that label (P3b).
pub(super) fn relabel(names: &mut BTreeMap<String, String>, terminal: &str, label: &str) {
    for name in names.values_mut().filter(|name| name.as_str() == terminal) {
        *name = label.to_owned();
    }
}

pub(super) fn snapshot_cwds(names: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    cwds(Some(names))
}

fn cwds(names: Option<&BTreeMap<String, String>>) -> BTreeMap<String, String> {
    let Ok(panes) = backend::current().pane_paths() else {
        return BTreeMap::new();
    };
    let mut rows = BTreeMap::<String, (u8, String)>::new();
    for pane in panes {
        if !valid_path(&pane.path) {
            continue;
        }
        // A pane without an ID goes by its name, as in the snapshot.
        let session = match names {
            Some(names) if !pane.session_id.is_empty() => {
                let Some(name) = names.get(&pane.session_id) else {
                    continue;
                };
                name.clone()
            }
            _ => pane.session,
        };
        let priority = u8::from(pane.active_window) * 2 + u8::from(pane.active_pane);
        if rows
            .get(&session)
            .is_none_or(|(score, _)| priority > *score)
        {
            rows.insert(session, (priority, pane.path));
        }
    }
    rows.into_iter()
        .map(|(name, (_, cwd))| (name, cwd))
        .collect()
}

fn valid_path(path: &str) -> bool {
    Path::new(path).is_absolute() && !path.chars().any(char::is_control)
}

fn empty(state: &str) -> Value {
    json!({"git_root":"","git_branch":"","git_worktree":false,
        "git_worktree_name":"","git_detached":false,"git_common_dir":"","git_metadata_state":state})
}

#[cfg(test)]
thread_local! {
    /// How many git processes this thread asked for (P1.1 tests).
    static GIT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn git(cwd: &str, args: &[&str], deadline: Instant) -> Option<(bool, String)> {
    #[cfg(test)]
    GIT_CALLS.with(|calls| calls.set(calls.get() + 1));
    let until = (Instant::now() + Duration::from_millis(200)).min(deadline);
    if Instant::now() >= until {
        return None;
    }
    let mut command = Command::new("git");
    command
        .args([
            "--no-optional-locks",
            "-C",
            cwd,
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
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
    let mut child = crate::platform::spawn(&mut command).ok()?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut output = String::new();
    child
        .stdout
        .take()?
        .take(16 * 1024)
        .read_to_string(&mut output)
        .ok()?;
    Some((status.success(), output))
}

/// The folder's Git metadata and its own Git directory (where `HEAD` is),
/// when it is a repository.
fn inspect_git_dir(cwd: &str, deadline: Instant) -> (Value, Option<PathBuf>) {
    let Some((ok, output)) = git(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
        deadline,
    ) else {
        return (empty("unavailable"), None);
    };
    if !ok {
        return (empty("not_repo"), None);
    }
    let lines: Vec<_> = output.lines().collect();
    if lines.len() != 3 {
        return (empty("unavailable"), None);
    }
    let absolute = |path: &str| {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            Path::new(cwd).join(path)
        };
        crate::platform::canonical(&path).unwrap_or(path)
    };
    let root = absolute(lines[0]);
    let worktree = absolute(lines[1]) != absolute(lines[2]);
    let Some((named, branch)) = git(
        cwd,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        deadline,
    ) else {
        return (empty("unavailable"), None);
    };
    (json!({"git_root":root,"git_branch":if named {branch.trim()} else {""},
        "git_worktree":worktree,"git_worktree_name":if worktree {root.file_name().and_then(|s|s.to_str()).unwrap_or("")} else {""},
        "git_detached":!named,"git_common_dir":absolute(lines[2]),"git_metadata_state":"ok"}), Some(absolute(lines[1])))
}

/// Task 234: `HEAD`'s modification time and size: a branch switch, a new
/// detached commit or a removed worktree changes it. None when it is gone.
fn head_stamp(git_dir: &Path) -> Option<Value> {
    let meta = fs::metadata(git_dir.join("HEAD")).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    Some(json!([modified, meta.len()]))
}

/// How long a cached folder's metadata is trusted without a check (the
/// earlier rule for every entry).
const METADATA_FRESH_SECONDS: f64 = 15.0;
/// Task 234: a repository's metadata is reused while its `HEAD` is unchanged
/// (a branch switch, a detached commit or a removed worktree changes it), up
/// to this long; spawning git twice for each of a listing's folders spent its
/// whole 750 ms budget on every listing, and a short bound made the entries
/// written together expire together.
const METADATA_HEAD_SECONDS: f64 = 86_400.0;
/// A folder that is no repository is checked again after this long.
const METADATA_NOT_REPO_SECONDS: f64 = 120.0;

/// Whether a cache entry `{cwd, at, data, git_dir, head}` still holds for
/// `cwd` at `now`.
fn metadata_fresh(cache: &Value, cwd: &str, now: f64) -> bool {
    let age = now - cache["at"].as_f64().unwrap_or(0.0);
    if cache["cwd"] != cwd || !cache["data"].is_object() || age < 0.0 {
        return false;
    }
    if age < METADATA_FRESH_SECONDS {
        return true;
    }
    match string(&cache["data"], "git_metadata_state") {
        "ok" => {
            age < METADATA_HEAD_SECONDS
                && !cache["head"].is_null()
                && cache["git_dir"].as_str().and_then(|dir| head_stamp(Path::new(dir))).as_ref() == Some(&cache["head"])
        }
        "not_repo" => age < METADATA_NOT_REPO_SECONDS && Path::new(cwd).is_dir(),
        _ => false,
    }
}

/// Search publishes only the capped result window's folder scopes. Unlike
/// `enrich`, this never writes a metadata cache or reads live pane state.
pub(super) fn search_scopes(cwd: &str, deadline: Instant) -> Option<Value> {
    if !valid_path(cwd) || cwd.len() > 16 * 1024 || Instant::now() >= deadline {
        return None;
    }
    let canonical = crate::platform::canonical(Path::new(cwd)).ok();
    // P1.1: `git -C` in a folder that does not exist only fails.
    if !Path::new(cwd).is_dir() {
        return Some(json!({"cwd_canonical":canonical,"git_root":""}));
    }
    let (ok, root) = git(cwd, &["rev-parse", "--show-toplevel"], deadline)?;
    Some(json!({"cwd_canonical":canonical,"git_root":if ok { root.trim() } else { "" }}))
}

/// A kept entry written back after a check ran out of time: the same entry
/// with a new time, so the next listings trust it for its fresh window
/// instead of spawning git again on a slow machine.
fn kept_entry(cache: &Value, now: f64) -> Value {
    let mut entry = cache.clone();
    entry["at"] = json!(now);
    entry
}

fn cached(cwd: &str, deadline: Instant) -> Value {
    if !valid_path(cwd) {
        return empty("unavailable");
    }
    // P1.1: a folder that does not exist is no repository. `git -C` fails
    // there with the same row fields; deleted team and worktree folders made
    // every listing spawn it once their 15 s window passed. Nothing is
    // cached, so a folder that appears again is checked by the next listing.
    if !Path::new(cwd).is_dir() {
        return empty("not_repo");
    }
    let path = root()
        .join("metadata-cache")
        .join(format!("{:x}.json", Sha256::digest(cwd.as_bytes())));
    let cached = fs::metadata(&path)
        .ok()
        .filter(|m| m.len() < 32768)
        .and_then(|_| fs::read_to_string(&path).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(cache) = cached.as_ref().filter(|cache| metadata_fresh(cache, cwd, now())) {
        return cache["data"].clone();
    }
    // Task 234: a read that ran out of the listing's budget (git is slow on a
    // loaded machine) never replaces good metadata: the last good entry is
    // shown, and kept, until a read succeeds. It used to be overwritten with
    // "unavailable", which every next listing read again.
    let known = cached
        .as_ref()
        .filter(|cache| cache["cwd"] == cwd && string(&cache["data"], "git_metadata_state") != "unavailable")
        .map(|cache| cache["data"].clone());
    if Instant::now() >= deadline {
        return known.unwrap_or_else(|| empty("unavailable"));
    }
    let (data, git_dir) = inspect_git_dir(cwd, deadline);
    if string(&data, "git_metadata_state") == "unavailable" {
        if let (Some(known), Some(cache)) = (known, cached.as_ref()) {
            let _ = atomic(&path, &kept_entry(cache, now()).to_string());
            return known;
        }
    }
    let head = git_dir.as_deref().and_then(head_stamp);
    let _ = atomic(
        &path,
        &json!({"cwd":cwd,"at":now(),"data":data,"git_dir":git_dir,"head":head}).to_string(),
    );
    data
}

pub(super) fn enrich(rows: &mut [Value], live_cwds: &BTreeMap<String, String>) {
    let deadline = Instant::now() + Duration::from_millis(750);
    let mut values = BTreeMap::new();
    for row in rows {
        if string(row, "state") != "archived" {
            if let Some(cwd) = live_cwds.get(string(row, "name")) {
                row["cwd"] = json!(cwd);
                row["cwd_source"] = json!("tmux");
            }
        }
        let cwd = string(row, "cwd").to_owned();
        row["cwd_canonical"] = json!(crate::platform::canonical(&cwd).ok());
        let value = values
            .entry(cwd.clone())
            .or_insert_with(|| cached(&cwd, deadline));
        if let (Some(target), Some(data)) = (row.as_object_mut(), value.as_object()) {
            target.extend(data.clone());
        }
    }
}

/// Task 234: a folder's Git metadata is reused while its HEAD is unchanged,
/// instead of spawning git twice per folder every 15 seconds.
#[cfg(test)]
mod metadata_cache_tests {
    use super::*;

    #[test]
    fn a_repository_entry_holds_while_its_head_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let head = head_stamp(&git_dir).unwrap();
        let entry = json!({"cwd":cwd,"at":1000.0,"data":{"git_metadata_state":"ok","git_branch":"main"},
            "git_dir":git_dir.to_string_lossy(),"head":head});
        assert!(metadata_fresh(&entry, &cwd, 1010.0), "young");
        assert!(metadata_fresh(&entry, &cwd, 1000.0 + METADATA_HEAD_SECONDS - 1.0), "HEAD unchanged");
        assert!(!metadata_fresh(&entry, &cwd, 1000.0 + METADATA_HEAD_SECONDS), "bounded");
        assert!(!metadata_fresh(&entry, "elsewhere", 1010.0), "another folder");
        // A branch switch rewrites HEAD.
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature-branch\n").unwrap();
        assert!(!metadata_fresh(&entry, &cwd, 1100.0));
        // An entry of the earlier format (no HEAD stamp) holds 15 seconds.
        let old = json!({"cwd":cwd,"at":1000.0,"data":{"git_metadata_state":"ok"}});
        assert!(metadata_fresh(&old, &cwd, 1014.0) && !metadata_fresh(&old, &cwd, 1016.0));
    }

    #[test]
    fn a_folder_that_is_no_repository_is_checked_again_after_two_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let entry = json!({"cwd":cwd,"at":1000.0,"data":{"git_metadata_state":"not_repo"}});
        assert!(metadata_fresh(&entry, &cwd, 1000.0 + METADATA_NOT_REPO_SECONDS - 1.0));
        assert!(!metadata_fresh(&entry, &cwd, 1000.0 + METADATA_NOT_REPO_SECONDS));
        let unavailable = json!({"cwd":cwd,"at":1000.0,"data":{"git_metadata_state":"unavailable"}});
        assert!(!metadata_fresh(&unavailable, &cwd, 1020.0), "a failed read is retried");
    }

    #[test]
    fn a_missing_folder_never_calls_git() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("deleted-worktree").to_string_lossy().into_owned();
        GIT_CALLS.with(|calls| calls.set(0));
        let far = Instant::now() + Duration::from_secs(60);
        for _ in 0..3 {
            assert_eq!(cached(&missing, far), empty("not_repo"));
        }
        assert_eq!(GIT_CALLS.with(|calls| calls.get()), 0);
        // An entry an older build wrote for it does not hold past 15 s either.
        let entry = json!({"cwd":missing,"at":1000.0,"data":{"git_metadata_state":"not_repo"}});
        assert!(!metadata_fresh(&entry, &missing, 1000.0 + METADATA_FRESH_SECONDS));
    }

    #[test]
    fn an_entry_kept_after_a_timeout_is_written_back_with_a_new_time() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let stale = json!({"cwd":cwd,"at":1000.0,"data":{"git_metadata_state":"not_repo"}});
        let now = 1000.0 + METADATA_NOT_REPO_SECONDS + 5.0;
        assert!(!metadata_fresh(&stale, &cwd, now));
        let kept = kept_entry(&stale, now);
        assert_eq!(kept["data"], stale["data"]);
        assert!(metadata_fresh(&kept, &cwd, now + METADATA_NOT_REPO_SECONDS - 1.0));
        assert!(!metadata_fresh(&kept, &cwd, now + METADATA_NOT_REPO_SECONDS));
    }
}
