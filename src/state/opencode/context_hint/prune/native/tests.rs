use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::{symlink, PermissionsExt},
    process::{Child, Command, Stdio},
};

fn asset(state: &Path) -> PathBuf {
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
    fs::write(
        path.join("owner.json"),
        json!({"revision":2,"launch":"prepared","nonce":nonce,
        "sha256":format!("{:x}",Sha256::digest(include_bytes!("../../../context_plugin.js")))})
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(path.join("owner.json"), fs::Permissions::from_mode(0o600)).unwrap();
    path
}
fn owner(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path.join("owner.json")).unwrap()).unwrap()
}
fn write_owner(path: &Path, value: &Value) {
    fs::write(path.join("owner.json"), value.to_string()).unwrap();
}
fn bind(path: &Path, pid: u32, birth: &str) {
    let mut value = owner(path);
    value["launch"] = json!("bound");
    value["service"] = json!({"pid":pid,"birth":birth,"id":"private-fixture"});
    write_owner(path, &value);
}
fn until() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

// Our own unsigned Rust image has a readable environment on macOS. Platform
// binaries such as /bin/sleep do not provide that contract. The pipe handshake
// keeps the original child alive without a timing assumption or host scan.
struct OwnChild {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    reaped: bool,
}
impl OwnChild {
    fn new(content: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::opencode::context_hint::prune::native::tests::environment_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("ZERUS_HINT_ENV_CHILD", "1")
            .env("OPENCODE_CONFIG_CONTENT", content)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut value = Self {
            child,
            output,
            reaped: false,
        };
        value.wait_for("HINT_CHILD_READY");
        value
    }
    fn wait_for(&mut self, marker: &str) {
        for _ in 0..16 {
            let mut line = String::new();
            assert!(self.output.read_line(&mut line).unwrap() > 0);
            if line.contains(marker) {
                return;
            }
        }
        panic!("private child did not acknowledge");
    }
    #[cfg(target_os = "linux")]
    fn protect(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"D").unwrap();
        self.wait_for("HINT_CHILD_PROTECTED");
    }
    fn stop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            self.child.wait().unwrap();
            self.reaped = true;
        }
    }
}
impl Drop for OwnChild {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
#[ignore = "private subprocess handshake fixture"]
fn environment_child() {
    if std::env::var("ZERUS_HINT_ENV_CHILD").as_deref() != Ok("1") {
        return;
    }
    println!("HINT_CHILD_READY");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    while std::io::stdin().read_exact(&mut byte).is_ok() {
        #[cfg(target_os = "linux")]
        if byte[0] == b'D' {
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) }, 0);
            println!("HINT_CHILD_PROTECTED");
            std::io::stdout().flush().unwrap();
        }
    }
}

#[test]
fn keeps_each_live_service_and_latest_marker_but_reclaims_known_dead_neighbours() {
    let temp = tempfile::tempdir().unwrap();
    let child = OwnChild::new("{}");
    let live = asset(temp.path());
    bind(&live, child.child.id(), &birth(child.child.id()).unwrap());
    let current = asset(temp.path());
    fs::write(
        temp.path().join("opencode/context-hint/service.json"),
        json!({"nonce":current.file_name().unwrap().to_str().unwrap()}).to_string(),
    )
    .unwrap();
    let mut gone = OwnChild::new("{}");
    let pid = gone.child.id();
    let before = birth(pid).unwrap();
    gone.stop();
    let dead = asset(temp.path());
    bind(&dead, pid, &before);
    let unused = asset(temp.path());
    fs::set_permissions(
        temp.path().join("opencode"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(live.join("index.js").is_file());
    assert!(current.join("index.js").is_file());
    assert!(!dead.exists());
    assert!(!unused.exists());
}

#[test]
fn unknown_starts_legacy_and_malformed_pins_are_kept() {
    let temp = tempfile::tempdir().unwrap();
    let mut kept = Vec::new();
    for update in [
        json!({"revision":1}),
        json!({"launch":"starting"}),
        json!({"launch":"bound"}),
        json!({"launch":"prepared","service":null}),
        json!({"launch":"bound","service":{"pid":0,"birth":"1","id":"x"}}),
        json!({"launch":"bound","service":{"pid":2147483647,"birth":"bad","id":"x"}}),
    ] {
        let path = asset(temp.path());
        let mut value = owner(&path);
        for (key, v) in update.as_object().unwrap() {
            value[key] = v.clone();
        }
        write_owner(&path, &value);
        kept.push(path);
    }
    let legacy = asset(temp.path());
    fs::remove_file(legacy.join("owner.json")).unwrap();
    kept.push(legacy);
    assert!(sweep(temp.path(), until()).unwrap());
    for path in kept {
        assert!(path.join("index.js").is_file());
    }
    assert!(sweep(temp.path(), Instant::now()).is_err());
}

#[test]
fn preserves_unknown_files_bad_ownership_and_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let extra = asset(temp.path());
    fs::write(extra.join("person.txt"), "keep").unwrap();
    let changed = asset(temp.path());
    fs::write(changed.join("index.js"), "unknown").unwrap();
    let owner = tempfile::tempdir().unwrap();
    fs::write(owner.path().join("index.js"), "PERSON_CONFIG").unwrap();
    let link = extra
        .parent()
        .unwrap()
        .join(uuid::Uuid::new_v4().to_string());
    symlink(owner.path(), &link).unwrap();
    sweep(temp.path(), until()).unwrap();
    assert!(extra.join("person.txt").is_file());
    assert_eq!(
        fs::read_to_string(changed.join("index.js")).unwrap(),
        "unknown"
    );
    assert_eq!(
        fs::read_to_string(link.join("index.js")).unwrap(),
        "PERSON_CONFIG"
    );
    let other = tempfile::tempdir().unwrap();
    fs::create_dir_all(other.path().join("opencode/context-hint")).unwrap();
    symlink(
        owner.path(),
        other.path().join("opencode/context-hint/plugins"),
    )
    .unwrap();
    assert!(sweep(other.path(), until()).is_err());
}

#[test]
fn work_and_allocation_remain_bounded() {
    let temp = tempfile::tempdir().unwrap();
    for _ in 0..FOLDERS + 5 {
        asset(temp.path());
    }
    assert!(!sweep(temp.path(), until()).unwrap());
    assert_eq!(
        fs::read_dir(temp.path().join("opencode/context-hint/plugins"))
            .unwrap()
            .count(),
        FOLDERS + 5 - REMOVALS
    );
    assert!(sweep(temp.path(), until()).unwrap());
    let retained = tempfile::tempdir().unwrap();
    for _ in 0..FOLDERS {
        let path = asset(retained.path());
        let mut v = owner(&path);
        v["launch"] = json!("starting");
        write_owner(&path, &v);
    }
    assert!(!sweep(retained.path(), until()).unwrap());
}

#[test]
fn reads_real_process_environment_and_resolves_exact_overlay_paths() {
    let temp = tempfile::tempdir().unwrap();
    let path = asset(temp.path());
    let content = json!({"plugins":[{"package":path}]}).to_string();
    let mut child = OwnChild::new(&content);
    let before = birth(child.child.id()).unwrap();
    let pin = service_pin(child.child.id(), &content).unwrap();
    assert_eq!(pin["pid"], child.child.id());
    assert_eq!(pin["birth"], before);
    assert!(service_pin(child.child.id(), "{}").is_none());
    let overlay = Overlay {
        nonce: path.file_name().unwrap().to_str().unwrap().into(),
        folder: path.clone(),
        content,
    };
    assert!(starting(&overlay));
    assert!(!starting(&overlay));
    let reg = service::Registration {
        id: "private-service".into(),
        version: "2.2.7".into(),
        port: 1,
        pid: child.child.id(),
        password: None,
    };
    pin_service(&reg, &overlay);
    assert_eq!(owner(&path)["service"]["birth"], before);
    assert_eq!(owner(&path)["launch"], "bound");
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(path.exists());
    child.stop();
    assert!(known_gone(reg.pid, &before, until()));
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(!path.exists());
}

#[test]
fn process_exit_during_environment_inspection_is_unknown() {
    let mut child = OwnChild::new("{}");
    let before = birth(child.child.id()).unwrap();
    let result = checked_environment(child.child.id(), &before, |pid| {
        let bytes = crate::state::task_reconcile::environment(pid).map_err(|_| uncertain())?;
        child.stop();
        Ok(bytes)
    });
    assert!(result.is_err());
}

#[test]
fn failed_exec_and_partial_preparation_can_be_reclaimed_without_guessing() {
    let temp = tempfile::tempdir().unwrap();
    let path = asset(temp.path());
    let overlay = Overlay {
        nonce: path.file_name().unwrap().to_str().unwrap().into(),
        folder: path.clone(),
        content: "{}".into(),
    };
    assert!(starting(&overlay));
    assert!(crate::platform::spawn_detached(&mut Command::new(
        temp.path().join("no-such-executable")
    ))
    .is_err());
    spawn_failed(&overlay);
    assert_eq!(owner(&path)["launch"], "prepared");
    fs::remove_file(path.join("index.js")).unwrap();
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(!path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn an_exited_leader_with_a_live_thread_is_not_retired() {
    let mut child = Command::new("python3").args(["-c",
        "import ctypes,threading,time;threading.Thread(target=lambda:time.sleep(20)).start();print('ready',flush=True);time.sleep(.05);ctypes.CDLL(None).pthread_exit(None)"])
        .env_clear().stdout(Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while birth(child.id()).is_some() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    let leader_gone = birth(child.id()).is_none();
    let exited = known_gone(child.id(), "1", end);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(ready.trim(), "ready");
    assert!(leader_gone);
    assert!(!exited);
}

#[cfg(target_os = "linux")]
#[test]
fn nondumpable_same_user_service_is_kept_without_a_host_census() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = OwnChild::new("{}");
    let pid = child.child.id();
    let before = birth(pid).unwrap();
    let path = asset(temp.path());
    bind(&path, pid, &before);
    child.protect();
    assert!(!known_gone(pid, &before, until()));
    let unused = asset(temp.path());
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(path.exists());
    assert!(!unused.exists());
    child.stop();
    assert!(known_gone(pid, &before, until()));
    assert!(sweep(temp.path(), until()).unwrap());
    assert!(!path.exists());
}

#[test]
fn pid_reuse_retires_only_the_original_identity() {
    let child = OwnChild::new("{}");
    let current = birth(child.child.id()).unwrap();
    #[cfg(target_os = "linux")]
    let old = (current.parse::<u64>().unwrap() - 1).to_string();
    #[cfg(target_os = "macos")]
    let old = format!(
        "{}:0",
        current.split_once(':').unwrap().0.parse::<u64>().unwrap() - 1
    );
    assert!(valid_birth(&old));
    assert!(known_gone(child.child.id(), &old, until()));
    assert!(!known_gone(child.child.id(), &current, until()));
}

#[test]
fn more_than_256_retired_service_folders_do_not_exhaust_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = OwnChild::new("{}");
    let pid = child.child.id();
    let before = birth(pid).unwrap();
    child.stop();
    // Use the cold-start sweep in the ordinary host PID namespace. Unrelated
    // unreadable environments/churn cannot veto it. A fixture deadline keeps
    // this storage/capacity control independent of test-runner scheduling.
    for _ in 0..300 {
        let path = asset(temp.path());
        bind(&path, pid, &before);
        assert!(sweep(temp.path(), until()).unwrap());
        assert!(!path.exists());
    }
}
