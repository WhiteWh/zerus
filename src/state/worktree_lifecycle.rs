//! Proven Zerus ownership and conservative cleanup at an explicit lifecycle exit.
//! A private receipt and a nonce in Git's per-checkout metadata must both match;
//! names and folder prefixes never establish ownership.
use super::worktrees::{cache_file, git};
use super::*;
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::time::{Duration, Instant};

const AFTER_ARCHIVE_GIT_TIME: Duration = Duration::from_secs(5);
pub(super) const OWNERSHIP_GIT_TIME: Duration = Duration::from_secs(3);
pub(super) const PREPARE_ARCHIVES_TIME: Duration = Duration::from_secs(2);
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const AFTER_ARCHIVE_FIXED_TIME: Duration = AFTER_ARCHIVE_GIT_TIME
    .saturating_add(super::worktree_cleanup::AUTOMATIC_REMOVE_FIXED_TIME);

pub(super) fn receipt_path(common: &Path, path: &Path) -> PathBuf {
    receipt_path_in(&root(), common, path)
}
pub(super) fn receipt_path_in(state: &Path, common: &Path, path: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = format!("{}\n{}", common.display(), path.display());
    state
        .join("worktree-cache")
        .join(format!("owned-{:x}.json", Sha256::digest(key.as_bytes())))
}

pub(super) fn created(common: &Path, path: &Path, branch: &str, head: &str) -> Result<()> {
    let metadata = git(
        path,
        &["rev-parse", "--absolute-git-dir"],
        Instant::now() + Duration::from_secs(3),
    )
    .map_err(|e| e.detail)?;
    created_at(common, path, branch, head, &metadata).map(drop)
}

/// `metadata` is the checkout's `rev-parse --absolute-git-dir` output. The
/// owned effect writes through its pinned entry instead.
fn created_at(
    common: &Path,
    path: &Path,
    branch: &str,
    head: &str,
    metadata: &[u8],
) -> Result<Value> {
    let metadata = crate::platform::canonical(String::from_utf8_lossy(metadata).trim())
        .map_err(|e| e.to_string())?;
    if !metadata.starts_with(common.join("worktrees")) {
        return Err("Cannot verify linked worktree metadata".into());
    }
    let value = owner_value(common, path, branch, head, &metadata);
    atomic(&metadata.join("zerus-owner.json"), &format!("{value}\n"))?;
    record_owner(common, path, value)
}

/// A new checkout's ownership record, for its metadata entry `metadata`.
pub(super) fn owner_value(
    common: &Path,
    path: &Path,
    branch: &str,
    head: &str,
    metadata: &Path,
) -> Value {
    json!({"version":1,"id":uuid::Uuid::new_v4().to_string(),"common_dir":common,"path":path,"branch":branch,"head":head,"metadata":metadata,"created_at":now()})
}

/// After the entry's owner file: the private receipt and maintenance record.
pub(super) fn record_owner(common: &Path, path: &Path, value: Value) -> Result<Value> {
    atomic(&receipt_path(common, path), &format!("{value}\n"))?;
    super::worktree_maintenance::register(&value)?;
    Ok(value)
}

pub(super) fn owned(common: &Path, path: &Path) -> Option<Value> {
    owned_receipt(&receipt_path(common, path), common, path)
        .ok()
        .flatten()
}
/// An ownership check could not finish. This never establishes lost ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipUnavailable;
type Verification<T> = std::result::Result<T, OwnershipUnavailable>;
/// Same ownership rules, with a captured root and a distinct failed-read outcome.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn owned_in(state: &Path, common: &Path, path: &Path) -> Verification<Option<Value>> {
    owned_receipt(&receipt_path_in(state, common, path), common, path)
}
fn owned_receipt(receipt: &Path, common: &Path, path: &Path) -> Verification<Option<Value>> {
    owned_receipt_with(receipt, common, path, &|path| {
        git(
            path,
            &["rev-parse", "--absolute-git-dir"],
            Instant::now() + OWNERSHIP_GIT_TIME,
        )
        .map_err(|_| OwnershipUnavailable)
    })
}
fn owner_json(path: &Path) -> Verification<Option<Value>> {
    labels::read_file(path, 16384)
        .map_err(|_| OwnershipUnavailable)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| OwnershipUnavailable))
        .transpose()
}
fn owner_canonical(path: &Path) -> Verification<Option<PathBuf>> {
    match crate::platform::canonical(path) {
        Ok(path) => Ok(Some(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(OwnershipUnavailable),
    }
}
fn owned_receipt_with(
    receipt: &Path,
    common: &Path,
    path: &Path,
    read_git: &impl Fn(&Path) -> Verification<Vec<u8>>,
) -> Verification<Option<Value>> {
    let Some(value) = owner_json(receipt)? else {
        return Ok(None);
    };
    if value["version"] != 1
        || value["common_dir"] != json!(common)
        || value["path"] != json!(path)
        || !value["id"]
            .as_str()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
    {
        return Ok(None);
    }
    let Some(metadata) = value["metadata"].as_str().map(Path::new) else {
        return Ok(None);
    };
    if !metadata.starts_with(common.join("worktrees"))
        || owner_canonical(metadata)?.as_deref() != Some(metadata)
    {
        return Ok(None);
    }
    if owner_json(&metadata.join("zerus-owner.json"))?.as_ref() != Some(&value) {
        return Ok(None);
    }
    if let Err(e) = fs::symlink_metadata(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(OwnershipUnavailable);
        }
        let Some(backlink) =
            labels::read_file(&metadata.join("gitdir"), 16384).map_err(|_| OwnershipUnavailable)?
        else {
            return Ok(None);
        };
        let backlink = String::from_utf8(backlink).map_err(|_| OwnershipUnavailable)?;
        return Ok(
            (Path::new(backlink.trim_end_matches('\n')) == path.join(".git")).then_some(value),
        );
    }
    let actual = read_git(path)?;
    let actual = String::from_utf8_lossy(&actual);
    Ok((owner_canonical(Path::new(actual.trim()))?.as_deref() == Some(metadata)).then_some(value))
}

/// Bind a session to the checkout it actually launches in. A later checkout
/// at the same path has a different nonce and is never that session's cleanup.
pub(super) fn for_folder(folder: &Path) -> Option<Value> {
    let info = git(folder,
        &["rev-parse", "--show-toplevel", "--path-format=absolute", "--git-common-dir"],
        Instant::now() + Duration::from_secs(1)).ok()?;
    let info = String::from_utf8(info).ok()?;
    let mut lines = info.lines();
    let path = crate::platform::canonical(lines.next()?).ok()?;
    let common = crate::platform::canonical(lines.next()?).ok()?;
    owned(&common, &path)
}

/// Called while the repository and state locks are held, before deletion. The
/// durable intent lets explicit Restore recreate the exact retained branch even
/// if the process exits between Git removal and its success receipt.
pub(super) fn prepare_removal(common: &Path, row: &Value) -> Result<()> {
    let path = Path::new(string(row, "path"));
    let ownership = owned(common, path).ok_or("Not a verified Zerus-created worktree")?;
    if ownership["branch"] != row["branch"] {
        return Err("The managed worktree branch changed; review it manually".into());
    }
    super::archive_references::visit_related(Instant::now()+PREPARE_ARCHIVES_TIME, path, string(&ownership,"id"), |mut record| {
        let refers = ["cwd", "launch_dir"]
            .iter()
            .any(|key| Path::new(string(&record, key)).starts_with(path));
        if !refers {
            return Ok(());
        }
        if !record["worktree_owner"].is_null() && record["worktree_owner"]["id"]!=ownership["id"] {
            return Ok(());
        }
        let cleanup = json!({"owner":ownership,"head":row["head"],"branch":row["branch"],"status":"removal_pending"});
        if record["backend"] == "dsh" {
            super::dsh::native_archives::mark_worktree_removal(&record, &cleanup)?;
        } else {
            record["worktree_cleanup"] = cleanup;
            atomic_record(
                &archive::path(string(&record, "archive_id"))?,
                &mut record,
            )?;
        }
        Ok(())
    })
}

fn archive_folder(record: &Value) -> &Path {
    let launch = string(record, "launch_dir");
    let folder = if launch.is_empty() && record["backend"] == "dsh" {
        string(record, "cwd")
    } else {
        launch
    };
    Path::new(folder)
}

pub(super) fn restorable(record: &Value) -> bool {
    let cleanup = &record["worktree_cleanup"];
    let owner = &cleanup["owner"];
    cleanup["status"] == "removal_pending"
        && owner["version"] == 1
        && Path::new(string(owner, "path")).is_absolute()
        && Path::new(string(owner, "common_dir")).is_dir()
        && !string(cleanup, "branch").is_empty()
        && !string(cleanup, "head").is_empty()
        && archive_folder(record).starts_with(Path::new(string(owner, "path")))
}

/// Restore is an explicit launch operation, never triggered by browsing an
/// archive. Refuse branch changes, existing destinations and all uncertain cases.
pub(super) fn restore(record: &Value) -> Result<()> {
    let folder = archive_folder(record);
    if folder.is_dir() {
        return Ok(());
    }
    if !restorable(record) {
        return Err(format!(
            "Working folder is unavailable: {}",
            folder.display()
        ));
    }
    let cleanup = &record["worktree_cleanup"];
    let owner = &cleanup["owner"];
    let common = Path::new(string(owner, "common_dir"));
    let path = Path::new(string(owner, "path"));
    let cache = cache_file("repo", &common.to_string_lossy());
    let guard = crate::platform::private_open(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true),
    )
    .open(cache.with_extension("lock"))
    .map_err(|e| e.to_string())?;
    guard
        .try_lock_exclusive()
        .map_err(|_| "Another worktree operation is running; retry Restore after it finishes")?;
    let _repo_lock = crate::platform::FileLock::held(guard);
    let _state_lock = lock(None)?;
    let stored: Value =
        serde_json::from_slice(&fs::read(receipt_path(common, path)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if stored != *owner || fs::symlink_metadata(path).is_ok() {
        return Err(
            "Restore destination or ownership changed; inspect the worktree manually".into(),
        );
    }
    if crate::platform::canonical(common).ok().as_deref() != Some(common)
        || path
            .parent()
            .and_then(|parent| crate::platform::canonical(parent).ok())
            .as_deref()
            != path.parent()
    {
        return Err(
            "Restore repository or destination parent changed; inspect its location manually"
                .into(),
        );
    }
    let branch = string(cleanup, "branch");
    let reference = format!("refs/heads/{branch}");
    let head = git(
        common,
        &["rev-parse", "--verify", "--end-of-options", &reference],
        Instant::now() + Duration::from_secs(3),
    )
    .map_err(|e| e.detail)?;
    if String::from_utf8_lossy(&head).trim() != string(cleanup, "head") {
        return Err(
            "Retained worktree branch changed; select its folder explicitly before restoring"
                .into(),
        );
    }
    git(
        common,
        &[
            "worktree",
            "add",
            "--quiet",
            "--",
            &path.to_string_lossy(),
            branch,
        ],
        Instant::now() + Duration::from_secs(30),
    )
    .map_err(|e| e.detail)?;
    let expected = format!("{}\nrefs/heads/{branch}", string(cleanup, "head"));
    let actual = git(
        path,
        &["rev-parse", "HEAD", "--symbolic-full-name", "HEAD"],
        Instant::now() + Duration::from_secs(3),
    )
    .map_err(|e| e.detail)?;
    if String::from_utf8_lossy(&actual).trim() != expected {
        return Err(
            "Restored worktree changed during checkout; inspect it before retrying Restore".into(),
        );
    }
    // Re-establish ownership for the new Git metadata instance. Keep branch/head.
    created(common, path, branch, string(cleanup, "head"))?;
    let _ = fs::remove_file(cache);
    if !folder.is_dir() {
        return Err("Original session subfolder is unavailable in the restored worktree".into());
    }
    Ok(())
}

/// Archive success must never become a failed termination because cleanup was
/// skipped or could not be confirmed. Emit a separate, machine-readable outcome.
pub(super) fn after_archive(record: &Value, archive_id: &str) {
    if terminating::pending(record) {
        return;
    }
    if !super::worktree_maintenance::policy().is_ok_and(|p| p.enabled && p.after_session) {
        return;
    }
    let folder = Path::new(string(record, "launch_dir"));
    if !folder.is_absolute() || !folder.is_dir() {
        return;
    }
    let mut observed = None;
    let mut attempted = false;
    let mut launch_owned = false;
    let result = (|| -> Result<Value> {
        let deadline = Instant::now() + AFTER_ARCHIVE_GIT_TIME;
        let info = git(
            folder,
            &[
                "rev-parse",
                "--show-toplevel",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            deadline,
        )
        .map_err(|e| e.detail)?;
        let info = String::from_utf8(info).map_err(|e| e.to_string())?;
        let mut lines = info.lines();
        let path = crate::platform::canonical(lines.next().ok_or("Missing worktree path")?)
            .map_err(|e| e.to_string())?;
        let common = crate::platform::canonical(lines.next().ok_or("Missing repository")?)
            .map_err(|e| e.to_string())?;
        let outcome = (|| -> Result<Value> {
            if path.join(".git").is_dir() {
                return Ok(Value::Null);
            }
            let original=&record["worktree_owner"];
            let expected=string(original,"id");
            if expected.is_empty() || original["path"]!=json!(path) || original["common_dir"]!=json!(common) {
                return Err("Not a verified Zerus-created checkout at session launch; review the folder manually".into());
            }
            launch_owned = true;
            let args = vec![
                "--path".into(),
                path.to_string_lossy().into_owned(),
                "--common-dir".into(),
                common.to_string_lossy().into_owned(),
                "--fingerprint".into(),
                "automatic".into(),
                "--automatic".into(),
                "--owner-id".into(),expected.into(),
            ];
            let (result, issued, review) = super::worktree_cleanup::remove_with_review(&args);
            attempted = issued;
            observed = review;
            result
        })();
        Ok(match outcome {
            Ok(value) if value.is_null() => Value::Null,
            Ok(value) => value,
            Err(error) => {
                json!({"status":if attempted {"unconfirmed"} else {"kept"},"path":path,"common_dir":common,"reason":error})
            }
        })
    })();
    if let Ok(mut value) = result {
        if !value.is_null() {
            value["archive"] = json!({"version":1,"archive_id":archive_id,
                "name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],
                "owner_id":record["worktree_owner"]["id"].as_str().unwrap_or(""),
                "launch_owned":launch_owned});
            retain_bounded_review(&mut value, observed);
            eprintln!("ZERUS_WORKTREE_CLEANUP {value}");
        }
    }
}

fn retain_bounded_review(value: &mut Value, observed: Option<Value>) {
    const LIMIT: usize = 60 * 1024;
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    // Keep the outcome and exact identity readable even when the old error
    // contains a wall of private paths. Never pretend an omitted detail is full.
    if size(value) > LIMIT {
        value.as_object_mut().unwrap().remove("reason");
        value["details_omitted"] = json!(true);
    }
    // The existing runner caps stderr at 64 KiB. Leave framing space; an
    // oversized row is absent, never partially trusted or truncated.
    let remaining = LIMIT.saturating_sub(size(value));
    if let Some(row) = observed.filter(|row| size(row).saturating_add(16) <= remaining) {
        value["review"] = row;
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn oversized_details_do_not_hide_the_archive_outcome_or_truncate_a_row() {
        let mut value = json!({"status":"kept","path":"/srv/example/work","common_dir":"/srv/example/main/.git",
            "archive":{"archive_id":"11111111-1111-4111-8111-111111111111"},"reason":"x".repeat(65536)});
        let row = json!({"path":"/srv/example/work","kind":"linked","changes":1});
        retain_bounded_review(&mut value, Some(row.clone()));
        assert_eq!(value["review"], row);
        assert_eq!(value["details_omitted"], true);
        assert_eq!(value["status"], "kept");
        assert!(value.get("archive").is_some());
        assert!(value.get("reason").is_none());
        let mut small = json!({"status":"kept"});
        retain_bounded_review(&mut small, Some(json!({"private":"x".repeat(65536)})));
        assert!(small.get("review").is_none());
        assert!(serde_json::to_vec(&value).unwrap().len() < 60 * 1024);
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod team_reader_tests {
    use super::*;
    #[test]
    fn explicit_state_ownership_uses_the_existing_pair_and_backlink_rules() {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let state = temp.path().join("state");
        let common = temp.path().join("repository.git");
        let metadata = common.join("worktrees/member");
        let path = temp.path().join("missing-checkout");
        fs::create_dir_all(&metadata).unwrap();
        fs::create_dir_all(state.join("worktree-cache")).unwrap();
        let owner = owner_value(&common, &path, "team-member", "fixture-head", &metadata);
        let receipt = receipt_path_in(&state, &common, &path);
        fs::write(&receipt, owner.to_string()).unwrap();
        fs::write(metadata.join("zerus-owner.json"), owner.to_string()).unwrap();
        fs::write(metadata.join("gitdir"), path.join(".git").to_str().unwrap()).unwrap();
        assert_eq!(owned_in(&state, &common, &path), Ok(Some(owner.clone())));
        assert_eq!(
            crate::state::team_worktree_ownership(&state, &common, &path),
            Ok(Some(owner.clone()))
        );
        let mut replaced = owner.clone();
        replaced["id"] = json!(uuid::Uuid::new_v4().to_string());
        fs::write(metadata.join("zerus-owner.json"), replaced.to_string()).unwrap();
        assert!(owned_in(&state, &common, &path).unwrap().is_none());
        fs::write(metadata.join("zerus-owner.json"), owner.to_string()).unwrap();
        fs::write(metadata.join("gitdir"), "/workspace/other/.git").unwrap();
        assert!(owned_in(&state, &common, &path).unwrap().is_none());
    }
    #[test]
    fn an_expired_git_read_is_unavailable_and_never_lost_ownership() {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let common = temp.path().join("repository.git");
        let metadata = common.join("worktrees/member");
        let path = temp.path().join("checkout");
        fs::create_dir_all(&metadata).unwrap();
        fs::create_dir(&path).unwrap();
        let state = temp.path().join("state");
        let receipt = receipt_path_in(&state, &common, &path);
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        let owner = owner_value(&common, &path, "member", "fixture", &metadata);
        fs::write(&receipt, owner.to_string()).unwrap();
        fs::write(metadata.join("zerus-owner.json"), owner.to_string()).unwrap();
        let expired = owned_receipt_with(&receipt, &common, &path, &|path| {
            git(path, &["rev-parse", "--absolute-git-dir"], Instant::now())
                .map_err(|_| OwnershipUnavailable)
        });
        assert_eq!(expired, Err(OwnershipUnavailable));
        assert_eq!(
            owned_receipt_with(&receipt, &common, &path, &|_| Ok(metadata
                .to_str()
                .unwrap()
                .as_bytes()
                .to_vec()))
            .unwrap(),
            Some(owner)
        );
        assert_eq!(
            receipt_path(&common, &path),
            receipt_path_in(&root(), &common, &path)
        );
    }
}
