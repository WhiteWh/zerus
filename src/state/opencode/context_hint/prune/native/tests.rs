use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

fn asset(state: &Path, owned: bool) -> PathBuf {
    let nonce = uuid::Uuid::new_v4().to_string();
    let path = state.join("opencode/context-hint/plugins").join(&nonce);
    fs::create_dir_all(&path).unwrap();
    for parent in [
        state.to_path_buf(),
        state.join("opencode"),
        state.join("opencode/context-hint"),
        path.parent().unwrap().to_path_buf(),
        path.clone(),
    ] {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (name, text) in [
        ("index.js", include_str!("../../../context_plugin.js")),
        ("package.json", PACKAGE),
    ] {
        fs::write(path.join(name), text).unwrap();
        fs::set_permissions(path.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    if owned {
        fs::write(
            path.join("owner.json"),
            json!({"revision":1,"nonce":nonce,
            "sha256":format!("{:x}",Sha256::digest(include_bytes!("../../../context_plugin.js")))})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(path.join("owner.json"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}
fn until() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn retains_all_live_overlay_paths_and_current_marker_not_only_latest_service() {
    let temp = tempfile::tempdir().unwrap();
    let old_live = asset(temp.path(), false);
    let new_live = asset(temp.path(), true);
    let current = asset(temp.path(), true);
    let failed = asset(temp.path(), true);
    let legacy_dead = asset(temp.path(), false);
    fs::set_permissions(
        temp.path().join("opencode"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(
        temp.path().join("opencode/context-hint/service.json"),
        json!({"nonce":current.file_name().unwrap().to_str().unwrap()}).to_string(),
    )
    .unwrap();
    assert!(sweep(temp.path(), until(), |_| Ok(BTreeSet::from([
        old_live.clone(),
        new_live.clone()
    ])))
    .unwrap());
    for path in [old_live, new_live, current] {
        assert!(path.join("index.js").is_file());
    }
    assert!(!failed.exists());
    assert!(!legacy_dead.exists());
}

#[test]
fn incomplete_process_census_and_expired_deadline_never_delete() {
    let temp = tempfile::tempdir().unwrap();
    let path = asset(temp.path(), true);
    assert!(sweep(temp.path(), until(), |_| Err(uncertain())).unwrap());
    assert!(path.exists());
    assert!(sweep(temp.path(), Instant::now(), |_| panic!(
        "expired before census"
    ))
    .is_err());
    assert!(path.join("index.js").is_file());
}

#[test]
fn preserves_unknown_data_bad_ownership_and_symlinks_without_following_them() {
    let temp = tempfile::tempdir().unwrap();
    let extra = asset(temp.path(), true);
    fs::write(extra.join("person.txt"), "keep").unwrap();
    let changed = asset(temp.path(), true);
    fs::write(changed.join("index.js"), "unknown").unwrap();
    let owner = tempfile::tempdir().unwrap();
    fs::write(owner.path().join("index.js"), "PERSON_CONFIG").unwrap();
    let link = extra
        .parent()
        .unwrap()
        .join(uuid::Uuid::new_v4().to_string());
    symlink(owner.path(), &link).unwrap();
    sweep(temp.path(), until(), |_| Ok(BTreeSet::new())).unwrap();
    assert!(extra.join("person.txt").is_file());
    assert_eq!(
        fs::read_to_string(changed.join("index.js")).unwrap(),
        "unknown"
    );
    assert_eq!(
        fs::read_to_string(link.join("index.js")).unwrap(),
        "PERSON_CONFIG"
    );
    // Replacing the entire plugins directory by a symlink also refuses.
    let other = tempfile::tempdir().unwrap();
    fs::create_dir_all(other.path().join("opencode/context-hint")).unwrap();
    symlink(
        owner.path(),
        other.path().join("opencode/context-hint/plugins"),
    )
    .unwrap();
    assert!(sweep(other.path(), until(), |_| panic!("linked ancestor")).is_err());
}

#[test]
fn work_and_new_allocation_are_bounded_even_when_every_asset_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    for _ in 0..FOLDERS + 5 {
        asset(temp.path(), true);
    }
    assert!(!sweep(temp.path(), until(), |_| Err(uncertain())).unwrap());
    assert!(!sweep(temp.path(), until(), |_| Ok(BTreeSet::new())).unwrap());
    assert_eq!(
        fs::read_dir(temp.path().join("opencode/context-hint/plugins"))
            .unwrap()
            .count(),
        FOLDERS + 5 - REMOVALS
    );
    assert!(sweep(temp.path(), until(), |_| Ok(BTreeSet::new())).unwrap());
}

#[test]
fn reads_real_process_environment_and_resolves_exact_overlay_paths() {
    let temp = tempfile::tempdir().unwrap();
    let path = asset(temp.path(), true);
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("20")
        .env_clear()
        .env(
            "OPENCODE_CONFIG_CONTENT",
            json!({"plugins":[{"package":path}]}).to_string(),
        )
        .spawn()
        .unwrap();
    let before = birth(child.id()).unwrap();
    let environment = crate::state::task_reconcile::environment(child.id()).unwrap();
    assert_eq!(birth(child.id()), Some(before));
    assert_eq!(
        paths(&environment, until()).unwrap(),
        BTreeSet::from([path])
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(birth(child.id()).is_none());
    assert!(paths(b"OPENCODE_CONFIG_CONTENT={\0", until()).is_err());
    assert!(paths(
        b"OPENCODE_CONFIG_CONTENT={}\0OPENCODE_CONFIG_CONTENT={}\0",
        until()
    )
    .is_err());
}

#[test]
fn process_exit_during_environment_inspection_is_unknown() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("20")
        .env_clear()
        .spawn()
        .unwrap();
    let before = birth(child.id()).unwrap();
    let result = checked_environment(child.id(), &before, |pid| {
        let bytes = crate::state::task_reconcile::environment(pid).map_err(|_| uncertain())?;
        child.kill().unwrap();
        child.wait().unwrap();
        Ok(bytes)
    });
    assert!(result.is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn an_exited_leader_with_a_live_thread_is_not_retired() {
    use std::io::BufRead;
    let mut child = std::process::Command::new("python3").args(["-c",
        "import ctypes,threading,time;threading.Thread(target=lambda:time.sleep(20)).start();print('ready',flush=True);time.sleep(.05);ctypes.CDLL(None).pthread_exit(None)"])
        .env_clear().stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while birth(child.id()).is_some() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    let leader_gone = birth(child.id()).is_none();
    let retired = retired(child.id(), end);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(ready.trim(), "ready");
    assert!(leader_gone);
    assert!(!retired);
}

#[cfg(target_os = "linux")]
#[test]
fn nondumpable_same_user_process_is_never_filtered_by_proc_directory_owner() {
    use std::io::BufRead;
    let mut child = std::process::Command::new("python3").args(["-c",
        "import ctypes,time;assert ctypes.CDLL(None).prctl(4,0)==0;print('ready',flush=True);time.sleep(20)"])
        .env_clear().stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let rows = pids(until());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(ready.trim(), "ready");
    assert!(rows.unwrap().contains(&child.id()));
}
