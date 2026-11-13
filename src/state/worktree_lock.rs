//! zerus-ucck P3c: releasing a session's own Git worktree lock when the
//! session leaves, by archive or forget (the spec's worktree lock, rule 6).
//! Dormant P3e publication can prepare such a veto, but no production creator
//! calls it yet. Archive/forget retain the floor's release behavior, so a lock
//! does not linger after its last verified holder leaves.
//!
//! Git has no compare-and-unlock, so a release takes the lock. It renames
//! Git's `locked` file to a private name beside it, checks that the text is
//! exactly `zerus:<session ID>`, and only then removes it. Anything else is put
//! back, never over a lock that appeared meanwhile. An intent written before
//! the take lets the next holder of the writer lock put back a lock that a
//! crash left taken.
use super::*;
use std::fs;
use std::io::{self, Read};

mod protection;
pub(super) use protection::Protection;

/// The private name of a taken lock, beside Git's `locked`.
const TAKEN: &str = "locked.zerus-release-";
/// A lock file larger than this is not one Zerus wrote.
const LOCK_BYTES: u64 = 4096;
/// Intents settled per recovery, so an unexpected pile never stalls the
/// writer lock.
const INTENTS: usize = 64;

/// What a release did with the lock of a session that left.
#[derive(Debug, PartialEq)]
pub(super) enum Release {
    /// No lock has the session's exact text, so nothing was touched.
    None,
    /// The session's lock on this checkout is gone.
    Released(PathBuf),
    /// The lock on this checkout stays, for the reason given.
    Kept(PathBuf, String),
}

fn intents() -> PathBuf {
    root().join("worktree-locks")
}

/// A lock file's text: a regular file, read without following a final link
/// or blocking on a FIFO, and bounded. `None` when there is no lock.
fn lock_text(path: &Path) -> io::Result<Option<String>> {
    let file = match crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(LOCK_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LOCK_BYTES {
        return Err(io::Error::other("too large for a lock"));
    }
    String::from_utf8(bytes).map(Some).map_err(io::Error::other)
}

/// Whether `text` is exactly the lock `zerus:<id>`, as `git worktree lock
/// --reason` writes it (with its newline) or without the newline.
fn exact(text: &str, id: &str) -> bool {
    text.strip_suffix('\n').unwrap_or(text) == format!("zerus:{id}")
}

/// Whether `a` and `b` are the same file, never through a link.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
            (Ok(a), Ok(b)) => {
                a.is_file() && b.is_file() && a.dev() == b.dev() && a.ino() == b.ino()
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        fs::symlink_metadata(a).is_ok_and(|m| m.is_file())
            && fs::symlink_metadata(b).is_ok_and(|m| m.is_file())
            && crate::platform::file_identity(a)
                .is_some_and(|identity| Some(identity) == crate::platform::file_identity(b))
    }
}

/// Releases the lock of `record`, a session that has just left, by archive
/// or forget. The caller holds the state writer lock, so no session starts
/// using the checkout meanwhile. It never fails the caller. A lock that
/// cannot be released stays, and the result says why (L4).
pub(super) fn release_locked(record: &Value) -> Release {
    let id = string(record, "session_id");
    let owner = &record["worktree_owner"];
    let metadata = Path::new(string(owner, "metadata"));
    // L1: only the checkout Zerus created for this session at its launch, and
    // only a lock with the session's exact text. A person's lock, another
    // session's and a plain folder are never touched.
    if !session_id::canonical(id) || string(owner, "id").is_empty() || !metadata.is_absolute() {
        return Release::None;
    }
    let lock = metadata.join("locked");
    match lock_text(&lock) {
        Ok(Some(text)) if exact(&text, id) => {}
        _ => return Release::None,
    }
    let checkout = PathBuf::from(string(owner, "path"));
    let kept = |reason: String| Release::Kept(checkout.clone(), reason);
    // Rule 3: the ownership nonce and its common-dir and metadata binding.
    let verified = worktree_lifecycle::owned(Path::new(string(owner, "common_dir")), &checkout)
        .is_some_and(|ownership| {
            ownership["id"] == owner["id"] && ownership["metadata"] == owner["metadata"]
        });
    if !verified {
        return kept("its Zerus ownership cannot be verified".into());
    }
    let protection = match protection::existing(record) {
        Ok(protection) => protection,
        Err(error) => return kept(error),
    };
    // L3: another live or saved session that uses the checkout keeps it, and
    // so does evidence that cannot be read.
    match worktree_cleanup::bindings() {
        Err(error) => {
            return kept(format!(
                "the sessions that may use it cannot be checked: {error}"
            ))
        }
        Ok(references) => {
            let users: Vec<_> = references.iter().filter(|reference| {
                reference.owner == string(owner, "id")
                    || reference
                        .folders
                        .iter()
                        .any(|folder| folder.starts_with(&checkout))
            }).collect();
            if let Some(other) = users.first() {
                if let Some(protection) = protection {
                    // Never transfer to a guessed label or an unbound folder
                    // occupant. Unavailable identity keeps the original veto.
                    if users.iter().any(|r| r.owner != string(owner,"id")
                        || !session_id::canonical(&r.session_id) || r.session_id == id) {
                        return kept("a shared holder's exact identity needs review".into());
                    }
                    let next = users.iter().min_by_key(|r| &r.session_id).unwrap();
                    return match protection.transfer(next) {
                        Ok(()) => kept(format!("{} still uses it; its protective lock was transferred",other.name)),
                        Err(error) => kept(error),
                    };
                }
                return kept(format!("{} still uses it", other.name));
            }
        }
    }
    if let Some(protection) = &protection {
        if let Err(error) = protection.verify() {return kept(error);}
    }
    let original = match protection.as_ref().map(Protection::original_object).transpose() {
        Ok(original) => original,
        Err(error) => return kept(error),
    };
    match take_original(&lock, id, original.as_ref()) {
        Ok(true) => {
            if let Some(protection) = &protection {
                if let Err(error) = protection.released() {return kept(error);}
            }
            Release::Released(checkout)
        }
        Ok(false) => Release::None,
        Err(error) => kept(error),
    }
}

/// Releases the lock of `record`, which has just left, and reports what
/// happened on standard error. The caller holds the state writer lock.
pub(super) fn left(record: &Value) {
    match release_locked(record) {
        Release::None => {}
        Release::Released(checkout) => {
            eprintln!(
                "hgs: released the Git worktree lock on {}",
                checkout.display()
            );
        }
        Release::Kept(checkout, reason) => {
            eprintln!(
                "hgs: kept the Git worktree lock on {}: {reason}",
                checkout.display()
            );
        }
    }
}

/// L2: takes the lock by renaming it, removes it only when its text is
/// exactly `zerus:<id>`, and otherwise puts it back. `Ok(false)` when the
/// lock went before the take. Every failure after the take puts the lock
/// back, and one that cannot be put back yet stays in its intent for
/// `recover_locked`.
#[cfg(all(test, unix))]
fn take(lock: &Path, id: &str) -> Result<bool> {
    take_original(lock,id,None)
}

fn same_object(path: &Path, original: &Value) -> bool {
    let Ok(file) = crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path) else {return false;};
    file.metadata().is_ok_and(|m| m.is_file())
        && crate::platform::FileIdentity::of(&file).is_ok_and(|id| json!([id.device,id.inode]) == *original)
}

fn take_original(lock: &Path, id: &str, original: Option<&Value>) -> Result<bool> {
    let dir = lock.parent().ok_or("invalid lock path")?;
    let nonce = uuid::Uuid::new_v4();
    let taken = dir.join(format!("{TAKEN}{nonce}"));
    let intent = intents().join(format!("{nonce}.json"));
    let value = json!({"version":1,"lock":lock,"taken":taken,"session_id":id,"original_object":original});
    atomic(&intent, &format!("{value}\n"))?;
    let taking =
        storage::cut_write().and_then(|()| fs::rename(lock, &taken).map_err(|e| e.to_string()));
    if let Err(error) = taking {
        // Nothing was taken: the lock is where it was, or already gone.
        let absent = |path: &Path| {
            fs::symlink_metadata(path).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        };
        let gone = absent(lock) && absent(&taken);
        let _ = archive::remove_synced(&intent);
        return if gone {
            Ok(false)
        } else {
            Err(format!("the lock could not be taken: {error}"))
        };
    }
    let checked = (|| -> Result<()> {
        crate::platform::sync_dir(dir).map_err(|e| e.to_string())?;
        match lock_text(&taken) {
            Ok(Some(text)) if exact(&text, id) => {}
            _ => return Err("the lock changed before its release; it was put back".into()),
        }
        if original.is_some_and(|original| !same_object(&taken,original)) {
            return Err("the original protective lock was replaced before release; it was put back".into());
        }
        storage::cut_write()?;
        fs::remove_file(&taken).map_err(|e| e.to_string())
    })();
    if let Err(error) = checked {
        return Err(match restore_original(lock, &taken, id, original) {
            Ok(()) => {
                let _ = archive::remove_synced(&intent);
                error
            }
            Err(failed) => format!("{error}; it could not be put back yet: {failed}"),
        });
    }
    let _ = crate::platform::sync_dir(dir);
    // A removal that fails leaves an intent whose taken lock is gone, which
    // recovery simply discards.
    let _ = archive::remove_synced(&intent);
    Ok(true)
}

/// Puts a taken lock back as Git's `locked`, never over a lock there. When
/// one appeared meanwhile, a taken lock with the session's exact text may go,
/// since the checkout stays locked. Any other stays beside it for review and
/// is never deleted (L2).
#[cfg(all(test, unix))]
fn restore(lock: &Path, taken: &Path, id: &str) -> Result<()> {
    restore_original(lock,taken,id,None)
}

fn restore_original(lock: &Path, taken: &Path, id: &str, original: Option<&Value>) -> Result<()> {
    let dir = lock.parent().ok_or("invalid lock path")?;
    storage::cut_write()?;
    match fs::hard_link(taken, lock) {
        Ok(()) => {}
        // Nothing to put back: the taken lock, or its whole entry, is gone.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !same_file(taken, lock)
                && (!matches!(lock_text(taken), Ok(Some(text)) if exact(&text, id))
                    || original.is_some_and(|original| !same_object(taken,original)))
            {
                return Err(format!(
                    "another lock appeared; the taken one stays at {} for review",
                    taken.display()
                ));
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    crate::platform::sync_dir(dir).map_err(|e| e.to_string())?;
    storage::cut_write()?;
    match fs::remove_file(taken) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    crate::platform::sync_dir(dir).map_err(|e| e.to_string())
}

/// A release intent: Git's `locked`, its taken name beside it, and the
/// session. An intent that is not exactly one of these is never acted on.
struct Intent {
    lock: PathBuf,
    taken: PathBuf,
    id: String,
    original: Option<Value>,
}

fn load(path: &Path) -> Option<Intent> {
    let mut bytes = Vec::new();
    crate::platform::no_follow(fs::OpenOptions::new().read(true))
        .open(path)
        .ok()?
        .take(LOCK_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    intent_value(serde_json::from_slice(&bytes).ok()?)
}

fn intent_value(value: Value) -> Option<Intent> {
    let lock = PathBuf::from(value["lock"].as_str()?);
    let taken = PathBuf::from(value["taken"].as_str()?);
    let id = value["session_id"].as_str()?.to_owned();
    let original = value.get("original_object").filter(|v| !v.is_null()).cloned();
    if original.as_ref().is_some_and(|v| !v.as_array().is_some_and(|a| a.len() == 2 && a.iter().all(|v| v.as_u64().is_some()))) {
        return None;
    }
    (value["version"] == 1
        && lock.is_absolute()
        && lock.file_name()? == "locked"
        && taken.parent() == lock.parent()
        && taken.file_name()?.to_str()?.starts_with(TAKEN)
        && session_id::canonical(&id))
    .then_some(Intent { lock, taken, id, original })
}

/// Called right after the writer lock is taken: puts back every lock a
/// release took and did not finish, after a crash included (L2). It never
/// fails the caller. An intent it cannot settle stays for the next holder.
pub(super) fn recover_locked() {
    let Ok(entries) = fs::read_dir(intents()) else {
        return;
    };
    for entry in entries.flatten().take(INTENTS) {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(intent) = load(&path) else {
            continue;
        };
        if restore_original(&intent.lock, &intent.taken, &intent.id, intent.original.as_ref()).is_ok() {
            let _ = archive::remove_synced(&path);
        }
    }
}

/// Same bounded best-effort inventory under an already-held original Root.
/// Invalid intents remain; selected control/primitive refusal never falls
/// through to the ordinary restore or removal lane.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn recover_guarded(effects: &dyn guarded_recovery::Effects) -> Result<()> {
    let scan = guarded_recovery::View::new(effects);
    scan.check()?;
    let entries = match fs::read_dir(intents()) {
        Ok(entries) => entries,
        Err(_) => return scan.check(),
    };
    for entry in entries.take(INTENTS) {
        scan.check()?;
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.extension().is_none_or(|v| v != "json") {
            continue;
        }
        let mut view = guarded_recovery::View::new(effects);
        let Some(bytes) = view.read(&path, LOCK_BYTES)? else {
            continue;
        };
        let Some(intent) = serde_json::from_slice(&bytes).ok().and_then(intent_value) else {
            continue;
        };
        match view.restore(
            &intent.lock,
            &intent.taken,
            &intent.id,
            intent.original.as_ref(),
        ) {
            Ok(()) => view.remove(&path)?,
            Err(error) => {
                // A retained invalid/changed lock may stay just as before,
                // but a latched EOF/deadline/source refusal must abort.
                view.check()?;
                let _ = error;
            }
        }
    }
    scan.check()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    /// Runs one case of the ignored fixture in a child with a private state
    /// root, a private Git repository and no tmux server.
    fn control(case: &str) {
        let state = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::worktree_lock::tests::fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path().join("sessions-root"))
            .env("TMUX_TMPDIR", state.path().join("tmux"))
            .env("ZERUS_WORKTREE_LOCK_CONTROL", case)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", state.path().join("gitconfig"))
            .env_remove("TMUX")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{case}: {stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// `control`, but a child that is still running after `within` is
    /// killed and fails the test: a blocked scan must fail, never hang.
    fn control_within(case: &str, within: std::time::Duration) {
        use std::io::Read;
        let state = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::worktree_lock::tests::fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path().join("sessions-root"))
            .env("TMUX_TMPDIR", state.path().join("tmux"))
            .env("ZERUS_WORKTREE_LOCK_CONTROL", case)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", state.path().join("gitconfig"))
            .env_remove("TMUX")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + within;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{case}: the control was still blocked after {within:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            status.success() && stdout.contains("test result: ok. 1 passed"),
            "{case}: {stdout}{stderr}"
        );
    }

    #[test]
    fn its_own_lock_is_released_and_no_other_is_touched() {
        control("own");
    }
    /// zerus-lr1l2: the initial binding scan reads each file bounded: a
    /// canonical FIFO, a link or an oversized binding keeps the lock for
    /// review, promptly, and never blocks the release. zerus-zy12o: so does
    /// a label that routes to a FIFO.
    #[test]
    fn a_fifo_link_or_oversized_binding_keeps_the_lock_without_blocking() {
        control_within("unreadable", std::time::Duration::from_secs(60));
    }
    #[test]
    fn a_shared_checkout_unverified_ownership_or_unreadable_evidence_keeps_the_lock() {
        control("kept");
    }
    #[test]
    fn a_lock_replaced_before_the_take_or_beside_a_restore_is_never_deleted() {
        control("replaced");
    }
    #[test]
    fn a_crash_at_every_write_puts_back_a_lock_it_did_not_release() {
        control("crash");
    }
    #[test]
    fn an_archive_stands_when_its_lock_is_kept_and_forget_releases_it() {
        control("archive");
    }

    #[test]
    fn ownership_files_are_regular_bounded_and_never_followed() {
        control("owner_files");
    }

    #[test]
    fn new_id_publication_is_create_only_and_retains_its_git_veto() {
        control("publication");
    }

    #[test]
    fn shared_id_holders_transfer_one_original_lock_without_an_unlocked_gap() {
        control("publication_shared");
    }

    #[test]
    fn a_foreign_lock_or_manual_unlock_never_becomes_new_publication_protection() {
        control("publication_choices");
    }

    #[test]
    fn replaced_checkout_and_same_text_lock_objects_refuse_publication_and_release() {
        control("publication_objects");
    }

    #[test]
    fn every_publication_cut_keeps_the_record_unexposed_or_durably_protected() {
        control_within("publication_cuts",std::time::Duration::from_secs(60));
    }

    #[test]
    fn a_destination_appearing_after_the_census_is_never_replaced() {
        control("publication_destination");
    }

    #[test]
    fn publication_refuses_descendant_aliases_without_creating_foreign_entries() {
        control("publication_namespace");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires the checksum-verified v0.47.0 Arch release via HGS_TEST_OLD_CLI"]
    fn literal_v047_maintenance_keeps_an_id_only_session_worktree() {
        control_within("publication_old",std::time::Duration::from_secs(60));
    }

    #[test]
    fn review_receipt_directory_alias_never_writes_foreign_entries() {
        control("review_receipt_alias");
    }
    #[test]
    fn review_missing_receipt_never_downgrades_original_lock_proof() {
        control("review_missing_receipt");
    }

    #[test]
    fn a_receipt_directory_swapped_after_acquisition_never_receives_a_write() {
        control("review_receipt_swap");
    }
    #[test]
    fn a_protected_legacy_shared_holder_never_loses_its_original_lock_fence() {
        control("review_shared_root");
    }
    #[test]
    fn a_shared_dsh_holder_is_marked_under_its_own_nonblocking_exclusion() {
        control("review_shared_dsh");
    }
    #[test]
    fn a_shared_opencode_holder_is_marked_under_its_own_nonblocking_exclusion() {
        control("review_shared_opencode");
    }

    #[test]
    fn independent_provider_lock_alias_never_creates_foreign_entries() {
        control("independent_provider_alias");
    }
    #[test]
    fn a_provider_lock_fifo_refuses_promptly_without_changing_the_binding() {
        control_within("provider_fifo",std::time::Duration::from_secs(5));
    }

    #[test]
    fn legacy_move_preparation_retains_the_original_existing_git_veto() {
        control_within("move_existing_veto", std::time::Duration::from_secs(15));
    }

    #[test]
    fn the_record_job_transaction_retains_the_actual_legacy_git_veto() {
        control_within("move_record_transaction", std::time::Duration::from_secs(15));
    }

    #[test]
    fn legacy_move_preparation_never_relocks_a_manual_unlock_or_foreign_veto() {
        control_within("move_no_relock", std::time::Duration::from_secs(15));
    }

    #[test]
    fn interrupted_legacy_retention_keeps_its_negative_and_original_object() {
        control_within("move_retention_cut", std::time::Duration::from_secs(15));
    }

    #[test]
    fn legacy_move_refuses_writable_special_hardlinked_and_symlinked_git_files() {
        control_within("move_invalid_files", std::time::Duration::from_secs(15));
    }

    #[test]
    fn legacy_move_never_rebuilds_a_lost_marker_or_proof() {
        control_within("move_lost_metadata", std::time::Duration::from_secs(15));
    }

    #[test]
    fn a_manual_unlock_or_relock_between_capture_and_retention_refuses() {
        for case in ["move_between_unlock", "move_between_relock", "move_between_replace"] {
            control_within(case, std::time::Duration::from_secs(15));
        }
    }
    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn taken_left(lock: &Path) -> Vec<String> {
        fs::read_dir(lock.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(TAKEN))
            .collect()
    }

    fn intents_left() -> usize {
        fs::read_dir(intents()).map_or(0, |entries| entries.count())
    }

    fn saved(name: &str, run: &str, folder: &Path) -> Value {
        let mut record = json!({"version":1,"agent":"claude","name":name,"run_id":run,
            "conversation_id":format!("conversation-{run}"),"created":100,
            "session_id":uuid::Uuid::new_v4().to_string(),"cwd":folder,"launch_dir":folder});
        storage::write(&mut record).unwrap();
        read(name).unwrap()
    }

    #[test]
    #[ignore = "run by the control tests"]
    fn fixture() {
        let Ok(case) = std::env::var("ZERUS_WORKTREE_LOCK_CONTROL") else {
            return;
        };
        storage::private_dir(&root()).unwrap();
        let base = root().parent().unwrap().join("git");
        let main = base.join("main");
        fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(
            &main,
            &[
                "-c",
                "user.name=Zerus",
                "-c",
                "user.email=zerus@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(&main, &["worktree", "add", "-q", "../work", "-b", "work"]);
        let common = crate::platform::canonical(main.join(".git")).unwrap();
        let checkout = crate::platform::canonical(base.join("work")).unwrap();
        let head = git(&checkout, &["rev-parse", "HEAD"]);
        worktree_lifecycle::created(&common, &checkout, "work", head.trim()).unwrap();
        let ownership = worktree_lifecycle::owned(&common, &checkout).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let record = json!({"version":1,"agent":"claude","name":"claude/example/locked","session_id":id,
            "run_id":"run-locked","conversation_id":"conversation-locked","created":100,
            "worktree_owner":ownership,"cwd":checkout,"launch_dir":checkout});
        let lock = PathBuf::from(ownership["metadata"].as_str().unwrap()).join("locked");
        let own = format!("zerus:{id}");
        let place = |reason: &str| {
            git(
                &main,
                &[
                    "worktree",
                    "lock",
                    "--reason",
                    reason,
                    checkout.to_str().unwrap(),
                ],
            );
        };
        let unlock = || {
            git(&main, &["worktree", "unlock", checkout.to_str().unwrap()]);
        };
        let clean = || {
            assert!(taken_left(&lock).is_empty(), "{:?}", taken_left(&lock));
            assert_eq!(intents_left(), 0);
        };
        match case.as_str() {
            case if case.starts_with("scoped_") => {
                scoped_tests::run(case, &main, &checkout, &common, &record, &lock);
            }
            "move_existing_veto" | "move_no_relock" | "move_retention_cut" |
            "move_invalid_files" | "move_lost_metadata" | "move_between_unlock" | "move_record_transaction" |
            "move_between_relock" | "move_between_replace" => {
                use std::os::unix::fs::PermissionsExt;
                // This is the private native publication fixture; legacy
                // cleanup's separate readable-directory rules are unchanged.
                fs::set_permissions(root(), fs::Permissions::from_mode(0o700)).unwrap();
                let _writer = storage::lock(None).unwrap();
                let mut legacy = record.clone();
                let binding = storage::legacy_record_path(string(&legacy, "name"));
                atomic(&binding, &legacy.to_string()).unwrap();
                if case == "move_no_relock" {
                    let before = fs::read(&binding).unwrap();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert!(!lock.exists(), "no Git lock may be minted for an old holder");
                    assert_eq!(fs::read(&binding).unwrap(), before);
                    place("the-persons-lock");
                    let original = crate::platform::file_identity(&lock).unwrap();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    let after = crate::platform::file_identity(&lock).unwrap();
                    assert_eq!((original.device,original.inode),(after.device,after.inode));
                    assert_eq!(fs::read_to_string(&lock).unwrap(), "the-persons-lock\n");
                    assert_eq!(fs::read(&binding).unwrap(), before);
                    return;
                }
                // This exact case runs alone in the wrapper's private child.
                // Let Git itself create the genuine 0644 veto; never chmod a
                // positive fixture into satisfying the new proof.
                // SAFETY: only this owned, single-case child changes its mask.
                let previous_mask = unsafe { libc::umask(0o022) };
                place(&own);
                // SAFETY: restore that same private child's original mask.
                unsafe { libc::umask(previous_mask) };
                use std::os::unix::fs::MetadataExt;
                assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o7777, 0o644);
                if case == "move_record_transaction" {
                    let original=crate::platform::file_identity(&lock).unwrap();
                    let moved=storage::owner_transfer::begin(&_writer,&legacy,
                        std::time::Instant::now()+std::time::Duration::from_secs(10)).unwrap();
                    assert!(storage::is_id_layout_record(&moved,&id));
                    assert!(!binding.exists());
                    assert_eq!(storage::read_id(&id).unwrap()["session_id"],moved["session_id"]);
                    assert!(!storage::owner_transfer::blocked(&moved));
                    let retained=Protection::retained_for_move(&moved).unwrap().unwrap();
                    retained.verify().unwrap();drop(retained);
                    let after=crate::platform::file_identity(&lock).unwrap();
                    assert_eq!((original.device,original.inode),(after.device,after.inode));
                    assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
                    assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o7777,0o644);
                    return;
                }
                if case == "move_invalid_files" {
                    let before = fs::read(&binding).unwrap();
                    for mode in [0o664,0o646,0o4644,0o2644,0o1644] {
                        fs::set_permissions(&lock, fs::Permissions::from_mode(mode)).unwrap();
                        assert!(Protection::for_legacy_move(&mut legacy).is_err(), "mode{mode:o}");
                        assert_eq!(fs::read(&binding).unwrap(), before);
                    }
                    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
                    let link = lock.with_extension("extra-link");
                    fs::hard_link(&lock,&link).unwrap();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert_eq!(fs::read(&binding).unwrap(), before);
                    fs::remove_file(&link).unwrap();
                    let retained = lock.with_extension("original-file");
                    fs::rename(&lock,&retained).unwrap();
                    std::os::unix::fs::symlink(&retained,&lock).unwrap();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert_eq!(fs::read_to_string(&retained).unwrap(),format!("{own}\n"));
                    assert_eq!(fs::read(&binding).unwrap(),before);
                    return;
                }
                if case.starts_with("move_between_") {
                    let before = fs::read(&binding).unwrap();
                    let repo = main.clone(); let checkout = checkout.clone();
                    let hook_lock = lock.clone(); let reason = own.clone(); let selected = case.clone();
                    protection::BEFORE_LEGACY_RETENTION.with(|slot|*slot.borrow_mut()=Some(Box::new(move || {
                        if selected == "move_between_replace" {
                            fs::rename(&hook_lock,hook_lock.with_extension("retained-original")).unwrap();
                            fs::write(&hook_lock,format!("{reason}\n")).unwrap();
                        } else {
                            git(&repo,&["worktree","unlock",checkout.to_str().unwrap()]);
                            if selected == "move_between_relock" {
                                git(&repo,&["worktree","lock","--reason",&reason,checkout.to_str().unwrap()]);
                            }
                        }
                    })));
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert_eq!(fs::read(&binding).unwrap(),before);
                    if case == "move_between_unlock" {assert!(!lock.exists());}
                    else {assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));}
                    return;
                }
                let original = crate::platform::file_identity(&lock).unwrap();
                let receipt = root().join("worktree-protections").join(format!("{}.json", storage::label_key(string(&ownership,"id"))));
                if case == "move_lost_metadata" {
                    let held=Protection::for_legacy_move(&mut legacy).unwrap().unwrap();
                    held.verify().unwrap();drop(held);
                    let proof=fs::read(&receipt).unwrap();
                    let mut missing=legacy.clone();missing.as_object_mut().unwrap().remove("worktree_protection");
                    atomic(&binding,&missing.to_string()).unwrap();
                    assert!(Protection::for_legacy_move(&mut missing).is_err());
                    assert!(serde_json::from_slice::<Value>(&fs::read(&binding).unwrap()).unwrap().get("worktree_protection").is_none());
                    assert_eq!(fs::read(&receipt).unwrap(),proof);
                    atomic(&binding,&legacy.to_string()).unwrap();fs::remove_file(&receipt).unwrap();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert!(matches!(release_locked(&legacy),Release::Kept(..)));
                } else if case == "move_retention_cut" {
                    // Negative record write succeeds; receipt publication is
                    // cut. There is still no ID-layout record or new authority.
                    storage::cut::arm(1);
                    let result = Protection::for_legacy_move(&mut legacy);
                    storage::cut::disarm();
                    assert!(result.is_err());
                    assert!(!receipt.exists());
                    let mut current:Value = serde_json::from_slice(&fs::read(&binding).unwrap()).unwrap();
                    assert_eq!(current["worktree_protection"], ownership["id"]);
                    assert!(Protection::for_legacy_move(&mut current).is_err(), "missing proof is never rebuilt");
                    fs::remove_file(&binding).unwrap();
                    assert!(matches!(release_locked(&current), Release::Kept(..)));
                } else {
                    let held = Protection::for_legacy_move(&mut legacy).unwrap().unwrap();
                    held.verify().unwrap();
                    let mut expected = record.clone();
                    expected["worktree_protection"] = ownership["id"].clone();
                    assert_eq!(legacy, expected, "all delivery/history/parent fields remain original");
                    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&binding).unwrap()).unwrap(), expected);
                    let proof = fs::read(&receipt).unwrap();
                    drop(held);
                    let again = Protection::for_legacy_move(&mut legacy).unwrap().unwrap();
                    again.verify().unwrap();
                    drop(again);
                    assert_eq!(fs::read(&receipt).unwrap(), proof);
                    assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o7777, 0o644);
                    unlock();
                    assert!(Protection::for_legacy_move(&mut legacy).is_err());
                    assert!(!lock.exists(), "a retained proof never authorizes relocking");
                    assert_eq!(fs::read(&receipt).unwrap(), proof);
                    return;
                }
                let after = crate::platform::file_identity(&lock).unwrap();
                assert_eq!((original.device,original.inode),(after.device,after.inode));
                assert_eq!(fs::read_to_string(&lock).unwrap(), format!("{own}\n"));
                assert!(!storage::id_record_path(&id).unwrap().exists());
                assert!(taken_left(&lock).is_empty());
            }
            "independent_provider_alias" | "provider_fifo" => {
                let _writer=storage::lock(None).unwrap();
                let mut legacy=record.clone();
                let legacy_id=uuid::Uuid::new_v4().to_string();
                legacy["session_id"]=json!(legacy_id);
                legacy["run_id"]=json!("legacy-run");
                legacy["name"]=json!("dsh/example/legacy");
                legacy["backend"]=json!("dsh");
                legacy["conversation_id"]=json!("session-native");
                let binding=root().join("dsh/bindings").join(format!("{}.json",storage::label_key(string(&legacy,"name"))));
                atomic(&binding,&legacy.to_string()).unwrap();
                let mut candidate=record.clone();candidate["layout"]=json!("id");
                storage::publish_new_id(&_writer, &mut candidate).unwrap();
                storage::remove_record(&candidate).unwrap();
                let foreign=base.join("foreign-provider-locks");fs::create_dir(&foreign).unwrap();
                let aux=root().join("dsh/bindings.lock");
                assert!(!aux.exists());
                if case == "provider_fifo" {
                    let name=std::ffi::CString::new(aux.to_str().unwrap()).unwrap();
                    // SAFETY: this fixture owns this absent NUL-free entry.
                    assert_eq!(unsafe{libc::mkfifo(name.as_ptr(),0o600)},0);
                } else {
                    std::os::unix::fs::symlink(foreign.join("created.lock"),&aux).unwrap();
                }
                let result=release_locked(&candidate);
                let entries=fs::read_dir(&foreign).unwrap().count();
                assert_eq!(entries,0,"provider exclusion followed a lock alias: {result:?}");
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
                assert_eq!(serde_json::from_slice::<Value>(&fs::read(&binding).unwrap()).unwrap(),legacy);
            }

            "review_receipt_alias" => {
                let _writer = storage::lock(None).unwrap();
                let foreign = base.join("foreign-protection");
                fs::create_dir(&foreign).unwrap();
                std::os::unix::fs::symlink(&foreign,root().join("worktree-protections")).unwrap();
                let mut candidate=record.clone(); candidate["layout"]=json!("id");
                let result=storage::publish_new_id(&_writer, &mut candidate);
                let entries=fs::read_dir(&foreign).unwrap().count();
                assert!(result.is_err() && entries==0,
                    "receipt alias was followed: result={result:?}, foreign entries={entries}");
            }
            "review_missing_receipt" => {
                let _writer=storage::lock(None).unwrap();
                let mut candidate=record.clone();candidate["layout"]=json!("id");
                storage::publish_new_id(&_writer, &mut candidate).unwrap();
                let retained=lock.with_extension("retained-original");
                fs::rename(&lock,&retained).unwrap();
                atomic(&lock,&format!("{own}\n")).unwrap();
                let a=crate::platform::file_identity(&retained).unwrap();
                let b=crate::platform::file_identity(&lock).unwrap();
                assert_ne!((a.device,a.inode),(b.device,b.inode));
                let receipt=root().join("worktree-protections")
                    .join(format!("{}.json",storage::label_key(string(&ownership,"id"))));
                fs::remove_file(receipt).unwrap();
                storage::remove_record(&candidate).unwrap();
                let result=release_locked(&candidate);
                assert!(matches!(result,Release::Kept(..)) && lock.exists(),
                    "missing proof allowed replacement lock release: {result:?}; exists={}",lock.exists());
            }

            "publication" | "publication_old" => {
                let mut writer = Some(storage::lock(None).unwrap());
                let mut candidate = record.clone(); candidate["layout"] = json!("id");
                let file = storage::id_record_path(&id).unwrap();
                storage::publish_new_id(writer.as_ref().unwrap(), &mut candidate).unwrap();
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
                assert_eq!(serde_json::from_slice::<Value>(&fs::read(&file).unwrap()).unwrap(),candidate);
                assert_eq!(storage::read_id(&id).unwrap()["session_id"],id);
                assert!(labels::listed_in(&root(),string(&candidate,"name")).unwrap().contains(&id));
                if case == "publication_old" {
                    use sha2::{Digest,Sha256};
                    let old=PathBuf::from(std::env::var_os("HGS_TEST_OLD_CLI").expect("provide the verified v0.47.0 CLI"));
                    let bytes=labels::read_file(&old,64<<20).unwrap().unwrap();
                    assert_eq!(format!("{:x}",Sha256::digest(bytes)),
                        "146731d47b4aa7c9787096bb12cf3d9054e0b043194fca245fa5a27f2988bbf5");
                    drop(writer.take());
                    let reviewed=Command::new(&old).args(["worktrees","review","--path"])
                        .arg(&main).arg("--worktree").arg(&checkout).arg("--json").output().unwrap();
                    assert!(reviewed.status.success(),"{}",String::from_utf8_lossy(&reviewed.stderr));
                    let value:Value=serde_json::from_slice(&reviewed.stdout).unwrap();
                    let row=value["worktrees"].as_array().unwrap().iter().find(|r|r["path"]==json!(checkout)).unwrap();
                    assert_eq!(row["sessions"],json!([]),"old maintenance must not know the ID holder");
                    assert_eq!(row["verdict"],"blocked");
                    assert!(row["reasons"].as_array().unwrap().iter().any(|r|r["code"]=="locked"));
                    let lock_before=fs::read(&lock).unwrap();
                    let removed=Command::new(&old).args(["worktrees","remove","--path"])
                        .arg(&checkout).arg("--common-dir").arg(&common)
                        .arg("--fingerprint").arg(row["fingerprint"].as_str().unwrap())
                        .args(["--automatic","--json"]).output().unwrap();
                    assert!(!removed.status.success());
                    assert!(String::from_utf8_lossy(&removed.stderr).contains("Locked by Git"),
                        "{}",String::from_utf8_lossy(&removed.stderr));
                    assert_eq!(fs::read(&lock).unwrap(),lock_before);assert!(checkout.is_dir());
                    writer=Some(storage::lock(None).unwrap());
                }
                let before = fs::read(&file).unwrap();
                let mut changed = candidate.clone(); changed["run_id"] = json!("another-run");
                assert!(storage::publish_new_id(writer.as_ref().unwrap(), &mut changed).is_err());
                assert_eq!(fs::read(&file).unwrap(),before);
                let removed = Command::new("git").current_dir(&main)
                    .args(["worktree","remove"]).arg(&checkout).output().unwrap();
                assert!(!removed.status.success()); assert!(checkout.is_dir());
                storage::remove_record(&candidate).unwrap();
                assert_eq!(release_locked(&candidate),Release::Released(checkout.clone()));
                assert!(!lock.exists());
                assert_eq!(git(&main,&["rev-parse","refs/heads/work"]).trim(),head.trim());
                // A later explicit publication after our confirmed release is
                // different from relocking after the person's manual unlock.
                let mut later=record.clone(); later["layout"]=json!("id");
                later["session_id"]=json!(uuid::Uuid::new_v4().to_string());
                storage::publish_new_id(writer.as_ref().unwrap(), &mut later).unwrap();
                fs::rename(&checkout,base.join("removed-by-person")).unwrap();
                storage::remove_record(&later).unwrap();
                assert_eq!(release_locked(&later),Release::Released(checkout.clone()));
                assert!(!lock.exists());
                drop(writer);
            }
            "review_receipt_swap" => {
                let _writer=storage::lock(None).unwrap();
                let mut candidate=record.clone(); candidate["layout"]=json!("id");
                let receipts=root().join("worktree-protections");
                let original=base.join("original-receipts");
                let foreign=base.join("foreign-receipts");fs::create_dir(&foreign).unwrap();
                let destination=receipts.clone();let outside=foreign.clone();
                storage::BEFORE_NEW_ID.with(|hook| *hook.borrow_mut()=Some(Box::new(move || {
                    fs::rename(&destination,&original).unwrap();
                    std::os::unix::fs::symlink(&outside,&destination).unwrap();
                })));
                assert!(storage::publish_new_id(&_writer, &mut candidate).is_err());
                assert_eq!(fs::read_dir(&foreign).unwrap().count(),0);
                assert!(!storage::id_record_path(&id).unwrap().exists());
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
            }
            "review_shared_root" | "review_shared_dsh" | "review_shared_opencode" => {
                let _writer=storage::lock(None).unwrap();
                let provider=case.strip_prefix("review_shared_").unwrap();
                let mut legacy=record.clone();
                let legacy_id=uuid::Uuid::new_v4().to_string();
                legacy["session_id"]=json!(legacy_id); legacy["run_id"]=json!("legacy-run");
                let directory=match provider {
                    "root" => {legacy["name"]=json!("claude/example/legacy");root()},
                    "dsh" => {legacy["name"]=json!("dsh/example/legacy");legacy["backend"]=json!("dsh");
                        legacy["conversation_id"]=json!("session-native");root().join("dsh/bindings")},
                    "opencode" => {legacy["name"]=json!("opencode/example/legacy");legacy["backend"]=json!("opencode");
                        legacy["conversation_id"]=json!("ses_native");root().join("opencode/bindings")},
                    _ => unreachable!(),
                };
                let binding=directory.join(format!("{}.json",storage::label_key(string(&legacy,"name"))));
                atomic(&binding,&legacy.to_string()).unwrap();
                let mut candidate=record.clone();candidate["layout"]=json!("id");
                storage::publish_new_id(&_writer, &mut candidate).unwrap();
                let object=crate::platform::file_identity(&lock).unwrap();
                storage::remove_record(&candidate).unwrap();
                if provider != "root" {
                    let aux=root().join(provider).join("bindings.lock");
                    let held=storage::lock(Some(&aux)).unwrap();
                    assert!(matches!(release_locked(&candidate),Release::Kept(_,reason) if reason.contains("busy")));
                    assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
                    assert_eq!(serde_json::from_slice::<Value>(&fs::read(&binding).unwrap()).unwrap(),legacy);
                    drop(held);
                }
                assert!(matches!(release_locked(&candidate),Release::Kept(..)));
                let after=crate::platform::file_identity(&lock).unwrap();
                assert_eq!((object.device,object.inode),(after.device,after.inode));
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("zerus:{legacy_id}\n"));
                let marked:Value=serde_json::from_slice(&fs::read(&binding).unwrap()).unwrap();
                let mut expected=legacy.clone();expected["worktree_protection"]=ownership["id"].clone();
                assert_eq!(marked,expected,"only negative retention metadata may change");
                let receipt=root().join("worktree-protections").join(format!("{}.json",storage::label_key(string(&ownership,"id"))));
                let proof=fs::read(&receipt).unwrap();
                let original=lock.with_extension("retained-original");fs::rename(&lock,&original).unwrap();
                let reason=format!("zerus:{legacy_id}\n");atomic(&lock,&reason).unwrap();
                fs::remove_file(&receipt).unwrap();fs::remove_file(&binding).unwrap();
                assert!(matches!(release_locked(&marked),Release::Kept(..)));
                assert_eq!(fs::read_to_string(&lock).unwrap(),reason);
                // The same valid proof and original object still permit the
                // final release; a negative marker never grants one by itself.
                fs::remove_file(&lock).unwrap();fs::rename(&original,&lock).unwrap();
                atomic(&receipt,std::str::from_utf8(&proof).unwrap()).unwrap();
                assert_eq!(release_locked(&marked),Release::Released(checkout.clone()));
                assert!(!lock.exists());
            }
            "publication_destination" => {
                let _writer=storage::lock(None).unwrap();
                let mut candidate=record.clone();candidate["layout"]=json!("id");
                let path=storage::id_record_path(&id).unwrap();
                let mut foreign=candidate.clone();foreign["run_id"]=json!("foreign-run");
                let bytes=foreign.to_string();let expected=bytes.clone();let destination=path.clone();
                storage::BEFORE_NEW_ID.with(|hook| *hook.borrow_mut()=Some(Box::new(move || {
                    fs::write(destination,bytes).unwrap();
                })));
                assert!(storage::publish_new_id(&_writer, &mut candidate).is_err());
                assert_eq!(fs::read_to_string(path).unwrap(),expected);
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
            }
            "publication_namespace" => {
                let _writer=storage::lock(None).unwrap();
                let foreign=base.join("foreign-namespace");fs::create_dir(&foreign).unwrap();
                for entry in ["sessions","labels","dsh"] {
                    let alias=root().join(entry);
                    std::os::unix::fs::symlink(&foreign,&alias).unwrap();
                    let mut candidate=record.clone();candidate["layout"]=json!("id");
                    if entry=="dsh" {
                        candidate["name"]=json!("dsh/example/shared");candidate["backend"]=json!("dsh");
                        candidate["conversation_id"]=json!("session-native");
                    }
                    assert!(storage::publish_new_id(&_writer, &mut candidate).is_err());
                    assert_eq!(fs::read_dir(&foreign).unwrap().count(),0);
                    assert!(!lock.exists());
                    fs::remove_file(alias).unwrap();
                }
            }
            "publication_shared" => {
                let _writer = storage::lock(None).unwrap();
                let mut one = record.clone(); one["layout"] = json!("id");
                storage::publish_new_id(&_writer, &mut one).unwrap();
                let object = crate::platform::file_identity(&lock).unwrap();
                let mut two = record.clone(); two["layout"] = json!("id");
                let two_id = uuid::Uuid::new_v4().to_string(); two["session_id"] = json!(two_id);
                two["run_id"] = json!("second-run"); two["conversation_id"] = json!("second-conversation");
                storage::publish_new_id(&_writer, &mut two).unwrap();
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("{own}\n"));
                storage::remove_record(&one).unwrap();
                assert!(matches!(release_locked(&one),Release::Kept(..)));
                let after = crate::platform::file_identity(&lock).unwrap();
                assert_eq!((object.device,object.inode),(after.device,after.inode));
                assert_eq!(fs::read_to_string(&lock).unwrap(),format!("zerus:{two_id}\n"));
                assert_eq!(release_locked(&one),Release::None);
                assert!(storage::read_id(&two_id).is_ok());
                storage::remove_record(&two).unwrap();
                assert_eq!(release_locked(&two),Release::Released(checkout.clone()));
                assert!(!lock.exists());
            }
            "publication_choices" => {
                let _writer = storage::lock(None).unwrap();
                let mut candidate = record.clone(); candidate["layout"] = json!("id");
                place("A person's lock");
                let before = fs::read(&lock).unwrap();
                assert!(storage::publish_new_id(&_writer, &mut candidate).is_err());
                assert_eq!(fs::read(&lock).unwrap(),before);
                assert!(!storage::id_record_path(&id).unwrap().exists());
                unlock();
                storage::publish_new_id(&_writer, &mut candidate).unwrap();
                unlock();
                let mut another = candidate.clone(); another["session_id"] = json!(uuid::Uuid::new_v4().to_string());
                assert!(storage::publish_new_id(&_writer, &mut another).is_err());
                assert!(!lock.exists());
                assert!(!storage::id_record_path(string(&another,"session_id")).unwrap().exists());
                assert!(storage::read_id(&id).is_ok());
            }
            "publication_objects" => {
                let _writer = storage::lock(None).unwrap();
                let held = Protection::acquire(&record).unwrap().unwrap();
                let original_object = held.original_object().unwrap();
                let git_file=fs::read(checkout.join(".git")).unwrap();
                let aside = base.join("checkout-aside");
                fs::rename(&checkout,&aside).unwrap();fs::create_dir(&checkout).unwrap();
                fs::write(checkout.join(".git"),git_file).unwrap();
                // Matching Git metadata/nonce is insufficient: this is still
                // a different native checkout object from the held original.
                assert_eq!(worktree_lifecycle::owned(&common,&checkout).unwrap(),ownership);
                assert!(held.verify().is_err());
                fs::remove_file(checkout.join(".git")).unwrap();
                fs::remove_dir(&checkout).unwrap();fs::rename(&aside,&checkout).unwrap();
                held.verify().unwrap();
                let original = lock.with_extension("original");
                fs::rename(&lock,&original).unwrap();
                atomic(&lock,&format!("{own}\n")).unwrap();
                let replaced = fs::read(&lock).unwrap();
                assert!(held.verify().is_err()); drop(held);
                assert!(take_original(&lock,&id,Some(&original_object)).is_err());
                assert!(matches!(release_locked(&record),Release::Kept(..)));
                assert_eq!(fs::read(&lock).unwrap(),replaced);
                assert!(original.is_file());
                assert!(!storage::id_record_path(&id).unwrap().exists());
            }
            "publication_cuts" => {
                let _writer = storage::lock(None).unwrap();
                for budget in 0..14 {
                    let mut candidate = record.clone(); candidate["layout"] = json!("id");
                    let id = uuid::Uuid::new_v4().to_string();candidate["session_id"] = json!(id);
                    storage::cut::arm(budget);
                    let result = storage::publish_new_id(&_writer, &mut candidate);
                    storage::cut::disarm();
                    let file = storage::id_record_path(&id).unwrap();
                    if file.exists() {
                        assert_eq!(fs::read_to_string(&lock).unwrap(),format!("zerus:{id}\n"));
                        assert!(labels::listed_in(&root(),string(&candidate,"name")).unwrap().contains(&id));
                        assert_eq!(storage::read_id(&id).unwrap()["session_id"],id);
                        storage::remove_record(&candidate).unwrap();
                    } else {
                        assert!(result.is_err());
                        if lock.exists() {
                            // An interrupted veto is retained, never adopted by
                            // a fresh identity or cleaned up automatically.
                            let before=fs::read(&lock).unwrap();
                            let mut retry=candidate.clone();retry["session_id"]=json!(uuid::Uuid::new_v4().to_string());
                            assert!(storage::publish_new_id(&_writer, &mut retry).is_err());
                            assert_eq!(fs::read(&lock).unwrap(),before);
                        }
                    }
                    // Explicit teardown of this test's private synthetic veto.
                    if lock.exists() {fs::remove_file(&lock).unwrap();}
                    let receipt=root().join("worktree-protections")
                        .join(format!("{}.json",storage::label_key(string(&ownership,"id"))));
                    if receipt.exists() {fs::remove_file(receipt).unwrap();}
                }
            }
            "owner_files" => {
                let receipt = worktree_lifecycle::receipt_path(&common,&checkout);
                let marker = PathBuf::from(ownership["metadata"].as_str().unwrap()).join("zerus-owner.json");
                for path in [&receipt,&marker] {
                    let original = fs::read(path).unwrap();
                    let foreign = base.join("foreign-owner.json");
                    fs::write(&foreign,&original).unwrap();
                    fs::remove_file(path).unwrap();
                    let name=std::ffi::CString::new(path.to_str().unwrap()).unwrap();
                    // SAFETY: the fixture owns this absent NUL-free path.
                    assert_eq!(unsafe {libc::mkfifo(name.as_ptr(),0o600)},0);
                    assert!(worktree_lifecycle::owned(&common,&checkout).is_none());
                    fs::remove_file(path).unwrap();
                    std::os::unix::fs::symlink(&foreign,path).unwrap();
                    assert!(worktree_lifecycle::owned(&common,&checkout).is_none());
                    assert_eq!(fs::read(&foreign).unwrap(),original);
                    fs::remove_file(path).unwrap();
                    fs::write(path,vec![b'x';16385]).unwrap();
                    assert!(worktree_lifecycle::owned(&common,&checkout).is_none());
                    fs::write(path,&original).unwrap();
                    assert_eq!(worktree_lifecycle::owned(&common,&checkout).unwrap(),ownership);
                }
                let backlink=PathBuf::from(ownership["metadata"].as_str().unwrap()).join("gitdir");
                let original=fs::read(&backlink).unwrap();
                let aside=base.join("aside");fs::rename(&checkout,&aside).unwrap();
                assert_eq!(worktree_lifecycle::owned(&common,&checkout).unwrap(),ownership);
                fs::remove_file(&backlink).unwrap();
                let name=std::ffi::CString::new(backlink.to_str().unwrap()).unwrap();
                // SAFETY: this fixture owns the removed metadata entry.
                assert_eq!(unsafe {libc::mkfifo(name.as_ptr(),0o600)},0);
                assert!(worktree_lifecycle::owned(&common,&checkout).is_none());
                fs::remove_file(&backlink).unwrap();fs::write(&backlink,original).unwrap();
                fs::rename(&aside,&checkout).unwrap();
                assert_eq!(worktree_lifecycle::owned(&common,&checkout).unwrap(),ownership);
            }
            "own" => {
                place(&own);
                assert_eq!(release_locked(&record), Release::Released(checkout.clone()));
                assert!(!lock.exists());
                let listed = git(&main, &["worktree", "list", "--porcelain"]);
                assert!(
                    !listed.lines().any(|line| line.starts_with("locked")),
                    "{listed}"
                );
                clean();
                // Exactly its own text without Git's newline is its own too.
                fs::write(&lock, &own).unwrap();
                assert_eq!(release_locked(&record), Release::Released(checkout.clone()));
                // A person's lock, another session's and near misses are
                // never touched.
                let other = format!("zerus:{}", uuid::Uuid::new_v4());
                let spaced = format!("{own} ");
                let capital = format!("Zerus:{id}");
                for reason in [
                    "my own reason",
                    other.as_str(),
                    spaced.as_str(),
                    capital.as_str(),
                ] {
                    place(reason);
                    let before = fs::read(&lock).unwrap();
                    assert_eq!(release_locked(&record), Release::None, "{reason}");
                    assert_eq!(fs::read(&lock).unwrap(), before, "{reason}");
                    unlock();
                }
                // A link where Git's lock is, never followed.
                let elsewhere = base.join("elsewhere");
                fs::write(&elsewhere, format!("{own}\n")).unwrap();
                std::os::unix::fs::symlink(&elsewhere, &lock).unwrap();
                assert_eq!(release_locked(&record), Release::None);
                assert!(fs::symlink_metadata(&lock)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                fs::remove_file(&lock).unwrap();
                // Without the checkout's ownership at launch, or without a
                // session ID, there is nothing to release.
                place(&own);
                for (key, value) in [("worktree_owner", Value::Null), ("session_id", json!(""))] {
                    let mut plain = record.clone();
                    plain[key] = value;
                    assert_eq!(release_locked(&plain), Release::None, "{key}");
                }
                assert_eq!(fs::read_to_string(&lock).unwrap(), format!("{own}\n"));
                clean();
            }
            "kept" => {
                place(&own);
                let before = fs::read(&lock).unwrap();
                let kept = |record: &Value, why: &str| match release_locked(record) {
                    Release::Kept(path, reason) => {
                        assert_eq!(path, checkout);
                        assert!(reason.contains(why), "{reason}");
                        assert_eq!(fs::read(&lock).unwrap(), before);
                    }
                    other => panic!("{why}: {other:?}"),
                };
                // Rule 3: another ownership nonce does not verify.
                let mut forged = record.clone();
                forged["worktree_owner"]["id"] = json!(uuid::Uuid::new_v4().to_string());
                kept(&forged, "ownership cannot be verified");
                // L3: a saved session in the checkout, or one launched with
                // its ownership from another folder.
                let inside = saved("claude/example/fork", "run-fork", &checkout.join("sub"));
                kept(&record, "claude/example/fork still uses it");
                storage::remove_record(&inside).unwrap();
                let mut owner = saved("claude/example/owner", "run-owner", &base);
                owner["worktree_owner"] = ownership.clone();
                storage::write(&mut owner).unwrap();
                kept(&record, "claude/example/owner still uses it");
                storage::remove_record(&owner).unwrap();
                // Evidence that cannot be read keeps it.
                let broken = root().join(format!("{}.json", "0".repeat(64)));
                fs::write(&broken, "{").unwrap();
                kept(&record, "cannot be checked");
                fs::remove_file(&broken).unwrap();
                clean();
                assert_eq!(release_locked(&record), Release::Released(checkout.clone()));
            }
            "unreadable" => {
                place(&own);
                let before = fs::read(&lock).unwrap();
                let kept = |why: &str| match release_locked(&record) {
                    Release::Kept(path, reason) => {
                        assert_eq!(path, checkout);
                        assert!(reason.contains("cannot be checked"), "{why}: {reason}");
                        assert_eq!(fs::read(&lock).unwrap(), before, "{why}");
                    }
                    other => panic!("{why}: {other:?}"),
                };
                // A FIFO at a canonical ID-layout binding path: no writer
                // ever opens it, so a blocking read would wait forever.
                let sessions = root().join("sessions");
                fs::create_dir_all(&sessions).unwrap();
                let fifo = sessions.join(format!("{}.json", uuid::Uuid::new_v4()));
                let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                kept("a FIFO binding");
                fs::remove_file(&fifo).unwrap();
                // A link at a legacy binding path, to a valid record elsewhere.
                let target = base.join("record.json");
                fs::write(&target, serde_json::to_vec(&record).unwrap()).unwrap();
                let linked = root().join(format!("{}.json", "1".repeat(64)));
                std::os::unix::fs::symlink(&target, &linked).unwrap();
                kept("a linked binding");
                fs::remove_file(&linked).unwrap();
                // A binding past the record bound (sparse: no disk used).
                let huge = root().join(format!("{}.json", "2".repeat(64)));
                fs::File::create(&huge)
                    .unwrap()
                    .set_len(storage::ID_RECORD_BYTES + 1)
                    .unwrap();
                kept("an oversized binding");
                fs::remove_file(&huge).unwrap();
                // zerus-zy12o: a regular legacy binding whose label routes
                // through `name_routes` to a FIFO in `bindings/`. The scan
                // reads the routed record before it reaches that folder.
                let routed = "claude/example/routed";
                let mut legacy = record.clone();
                legacy["name"] = json!(routed);
                let legacy_path = storage::legacy_record_path(routed);
                fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
                let route = storage::route_path(routed);
                fs::create_dir_all(route.parent().unwrap()).unwrap();
                fs::write(&route, json!({"version": 1, "name": routed}).to_string()).unwrap();
                let bindings = root().join("bindings");
                fs::create_dir_all(&bindings).unwrap();
                let fifo = bindings.join(legacy_path.file_name().unwrap());
                let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                kept("a label routed to a FIFO");
                for path in [&fifo, &route, &legacy_path] {
                    fs::remove_file(path).unwrap();
                }
                clean();
                // With only readable bindings left, the lock is released.
                assert_eq!(release_locked(&record), Release::Released(checkout.clone()));
            }
            "replaced" => {
                // Replaced between the read and the take: put back as it was.
                place("my own reason");
                let before = fs::read(&lock).unwrap();
                assert!(take(&lock, &id)
                    .unwrap_err()
                    .contains("changed before its release"));
                assert_eq!(fs::read(&lock).unwrap(), before);
                clean();
                unlock();
                // A lock that appears while a taken one is put back is left
                // alone. A taken lock that is not exactly this session's stays
                // beside it for review; one that is goes.
                let taken = lock.parent().unwrap().join(format!("{TAKEN}manual"));
                fs::write(&lock, "appeared\n").unwrap();
                fs::write(&taken, "my own reason\n").unwrap();
                assert!(restore(&lock, &taken, &id)
                    .unwrap_err()
                    .contains("for review"));
                assert_eq!(fs::read_to_string(&lock).unwrap(), "appeared\n");
                assert_eq!(fs::read_to_string(&taken).unwrap(), "my own reason\n");
                fs::write(&taken, format!("{own}\n")).unwrap();
                restore(&lock, &taken, &id).unwrap();
                assert_eq!(fs::read_to_string(&lock).unwrap(), "appeared\n");
                assert!(!taken.exists());
                fs::remove_file(&lock).unwrap();
                // A lock that went before the take is no release at all.
                assert!(!take(&lock, &id).unwrap());
                clean();
            }
            "crash" => {
                // Each durable write in turn is the last before a crash; the
                // next holder of the writer lock settles what is left. A lock
                // goes only when its exact text was checked and it was
                // removed; any other comes back byte for byte.
                for (text, releases) in [
                    (format!("{own}\n"), true),
                    ("my own reason\n".to_owned(), false),
                ] {
                    let mut finished = false;
                    for budget in 0..8 {
                        fs::write(&lock, &text).unwrap();
                        storage::cut::arm(budget);
                        let result = if releases {
                            release_locked(&record) == Release::Released(checkout.clone())
                        } else {
                            take(&lock, &id).is_ok()
                        };
                        storage::cut::disarm();
                        assert!(
                            !result || releases,
                            "budget {budget}: a foreign lock was released"
                        );
                        drop(storage::lock(None).unwrap());
                        let gone = !lock.exists();
                        assert_eq!(gone, releases && budget >= 3, "budget {budget}: {text}");
                        if !gone {
                            assert_eq!(fs::read_to_string(&lock).unwrap(), text, "budget {budget}");
                        }
                        clean();
                        finished |= budget >= 5;
                    }
                    assert!(finished);
                }
            }
            "archive" => {
                let name = "claude/example/locked";
                let write_run = |run: &str| {
                    let mut session = record.clone();
                    session["run_id"] = json!(run);
                    session["conversation_id"] = json!(format!("conversation-{run}"));
                    storage::write(&mut session).unwrap();
                    read(name).unwrap()
                };
                // Every way a session leaves releases its own lock: an archive,
                // terminate --forget of a saved session, and terminate --forget
                // of one already archived.
                place(&own);
                let session = write_run("run-archive");
                let guard = storage::lock(None).unwrap();
                archive::save(&session).unwrap();
                drop(guard);
                assert!(!lock.exists());
                place(&own);
                write_run("run-forget");
                terminate::dispatch_scoped(&[name.to_owned()], false, None, true).unwrap();
                assert!(read(name).is_err());
                assert!(!lock.exists());
                place(&own);
                let session = write_run("run-archived");
                let fork = saved("claude/example/fork", "run-fork", &checkout);
                let guard = storage::lock(None).unwrap();
                archive::save(&session).unwrap();
                drop(guard);
                storage::remove_record(&fork).unwrap();
                assert!(lock.exists());
                write_run("run-archived");
                terminate::dispatch_scoped(&[name.to_owned()], false, None, true).unwrap();
                assert!(read(name).is_err());
                assert!(!lock.exists());
                clean();
                // L4: the archive stands and the lock stays while another
                // session uses the checkout; forgetting the archive once none
                // does releases it.
                place(&own);
                let mut session = record.clone();
                storage::write(&mut session).unwrap();
                let session = read("claude/example/locked").unwrap();
                let fork = saved("claude/example/fork", "run-fork", &checkout);
                let guard = storage::lock(None).unwrap();
                let archive_id = archive::save(&session).unwrap();
                drop(guard);
                assert!(read("claude/example/locked").is_err());
                assert_eq!(fs::read_to_string(&lock).unwrap(), format!("{own}\n"));
                storage::remove_record(&fork).unwrap();
                archive::forget("claude/example/locked", &archive_id, false).unwrap();
                assert!(!lock.exists());
                clean();
                // A take that fails keeps the lock and never fails the archive.
                if unsafe { libc::geteuid() } != 0 {
                    use std::os::unix::fs::PermissionsExt;
                    place(&own);
                    let mut session = record.clone();
                    session["run_id"] = json!("run-again");
                    storage::write(&mut session).unwrap();
                    let session = read("claude/example/locked").unwrap();
                    let entry = lock.parent().unwrap().to_path_buf();
                    fs::set_permissions(&entry, fs::Permissions::from_mode(0o500)).unwrap();
                    let guard = storage::lock(None).unwrap();
                    let archived = archive::save(&session);
                    // What that archive's release met, once its record left.
                    let outcome = release_locked(&session);
                    drop(guard);
                    fs::set_permissions(&entry, fs::Permissions::from_mode(0o700)).unwrap();
                    archived.unwrap();
                    assert!(read("claude/example/locked").is_err());
                    assert!(
                        matches!(&outcome, Release::Kept(_, reason) if reason.contains("could not be taken")),
                        "{outcome:?}"
                    );
                    assert_eq!(fs::read_to_string(&lock).unwrap(), format!("{own}\n"));
                    clean();
                }
            }
            other => panic!("unknown control {other}"),
        }
    }
}

#[cfg(all(test, unix))]
mod scoped_tests;
