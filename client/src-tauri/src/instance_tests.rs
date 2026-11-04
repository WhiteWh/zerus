// These subprocesses exercise the real kernel lock and activation listeners,
// with a test-supplied namespace. No test opens the login user's GUI endpoint.
const FIXTURE: &str = "instance::tests::canonical_tests::launch_fixture";
const CONTROL: &str = "ZERUS_GUI_INSTANCE_TEST_CONTROL";

fn private_root() -> Scratch {
    // Darwin's socket path limit must not depend on the runner's TMPDIR.
    let directory = PathBuf::from("/tmp").join(format!("zgui-{}", uuid::Uuid::new_v4().simple()));
    DirBuilder::new().mode(0o700).create(&directory).unwrap();
    Scratch(directory)
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

fn wait_for(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture exited early"
        );
        assert!(Instant::now() < deadline, "fixture readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn launch(root: &Path, scope: &str) -> Child {
    let home = root.join(scope);
    fs::create_dir_all(home.join("tmp")).unwrap();
    fs::create_dir_all(home.join("runtime")).unwrap();
    let control = root.join(format!("{scope}.json"));
    fs::write(
        &control,
        serde_json::to_vec(&serde_json::json!({
            "root": root, "scope": scope
        }))
        .unwrap(),
    )
    .unwrap();
    Child(
        Command::new(std::env::current_exe().unwrap())
            // Never re-exec libtest without this exact filter.
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(CONTROL, control)
            .env("HOME", &home)
            .env("TMPDIR", home.join("tmp"))
            .env("XDG_RUNTIME_DIR", home.join("runtime"))
            .env("XDG_CONFIG_HOME", home.join("xdg"))
            .env("HGS_CONFIG_DIR", home.join("config"))
            .env("HGS_STATE_DIR", home.join("state"))
            // The incident used this GUI mode; it must not bypass exclusion.
            .env("ZERUS_CLIENT_DESKTOP_SERVICE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn launch_fixture() {
    let Some(control) = std::env::var_os(CONTROL) else {
        return;
    };
    let control: serde_json::Value = serde_json::from_slice(&fs::read(control).unwrap()).unwrap();
    let root = Path::new(control["root"].as_str().unwrap());
    let scope = control["scope"].as_str().unwrap();
    // The production startup path, with only the OS namespace supplied by
    // the fixture. All temporary/configuration discovery remains real.
    let claimed = match claim_in(
        "hgs-tray",
        None,
        Activation::Sessions,
        None,
        &root.join("canonical"),
        || {
            fs::write(root.join(format!("{scope}.effects")), b"renderer admitted").unwrap();
        },
    ) {
        Claim::Claimed(claimed) => claimed,
        Claim::Forwarded => {
            fs::write(root.join(format!("{scope}.forwarded")), b"clean exit").unwrap();
            return;
        }
        Claim::Failed(error) => panic!("fixture GUI owner: {error}"),
    };
    // This is the boundary before UI/desktop effects in the actual caller.
    fs::write(root.join(format!("{scope}.ready")), b"owner").unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while !root.join(format!("{scope}.stop")).exists() {
        assert!(Instant::now() < deadline, "fixture exit deadline");
        if let Ok(activation) = claimed.activations.recv_timeout(Duration::from_millis(20)) {
            assert_eq!(activation, Activation::Sessions);
            fs::write(root.join(format!("{scope}.activation")), activation.line()).unwrap();
        }
    }
    InstanceState(Mutex::new(Some(claimed.listening))).release();
    assert!(
        canonical::acquire(&root.join("canonical"))
            .unwrap()
            .is_none(),
        "final teardown must not release GUI exclusion before actual process exit"
    );
}

#[test]
fn two_tmpdirs_homes_and_configs_handoff_before_second_gui_effects() {
    let root = private_root();
    let mut owner = launch(&root, "first");
    wait_for(&root.join("first.ready"), &mut owner);
    let mut second = launch(&root, "second");
    wait_for(&root.join("second.forwarded"), &mut second);
    wait_for(&root.join("first.activation"), &mut owner);
    assert!(!root.join("second.ready").exists());
    assert!(!root.join("second.effects").exists());
    assert!(!root.join("second/tmp/hgs-tray").exists());
    assert!(!root.join("second/runtime/hgs-tray-start.lock").exists());
    assert_eq!(
        fs::read(root.join("first.activation")).unwrap(),
        b"sessions\n"
    );
    fs::write(root.join("first.stop"), b"").unwrap();
    assert!(owner.0.wait().unwrap().success());
    assert!(second.0.wait().unwrap().success());
}

#[test]
fn a_crash_releases_kernel_exclusion_and_replaces_only_the_stale_socket() {
    let root = private_root();
    let mut owner = launch(&root, "crashed");
    wait_for(&root.join("crashed.ready"), &mut owner);
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert!(root.join("canonical/gui.lock").exists());
    assert!(root.join("canonical/gui.sock").exists());
    let mut next = launch(&root, "replacement");
    wait_for(&root.join("replacement.ready"), &mut next);
    assert!(!root.join("replacement.forwarded").exists());
    fs::write(root.join("replacement.stop"), b"").unwrap();
    assert!(next.0.wait().unwrap().success());
    assert!(root.join("canonical/gui.lock").exists());
    assert!(!root.join("canonical/gui.sock").exists());
}

#[test]
fn canonical_exclusion_is_a_lifetime_lock_and_not_a_pid_or_age_heuristic() {
    let root = private_root();
    let directory = root.join("canonical");
    let first = canonical::acquire(&directory).unwrap().unwrap();
    fs::write(directory.join("gui.lock"), b"1\nexpired pid metadata\n").unwrap();
    assert!(canonical::acquire(&directory).unwrap().is_none());
    drop(first);
    assert!(canonical::acquire(&directory).unwrap().is_some());
    assert_eq!(
        canonical::directory(),
        PathBuf::from(format!("/tmp/com.hgdev.hgs-tray-{}", unsafe {
            libc::geteuid()
        }))
    );
}

#[test]
fn headless_dispatch_remains_available_with_the_gui_lock_held() {
    let root = private_root();
    let directory = root.join("canonical");
    let _gui = canonical::acquire(&directory).unwrap().unwrap();
    assert!(canonical::acquire(&directory).unwrap().is_none());
    let status = crate::entry::notification_host(["--desktop-notification-host"], || {
        // Production supplies notification_host::run, which authenticates the
        // inherited service channel before any SDK use. No GUI admission.
        23
    });
    assert_eq!(status, Some(23));
    assert!(canonical::acquire(&directory).unwrap().is_none());
}

#[test]
fn an_owner_not_yet_listening_still_excludes_a_second_gui() {
    let root = private_root();
    let directory = root.join("canonical");
    let _guard = canonical::acquire(&directory).unwrap().unwrap();
    assert!(canonical_claim(&directory, Activation::Sessions)
        .unwrap()
        .is_none());
    assert!(!directory.join("gui.sock").exists());
}

#[test]
fn unsafe_canonical_files_refuse_instead_of_starting_without_exclusion() {
    use std::os::unix::fs::symlink;
    let root = private_root();
    let directory = root.join("canonical");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, Permissions::from_mode(0o755)).unwrap();
    assert!(canonical::acquire(&directory).is_err());
    fs::set_permissions(&directory, Permissions::from_mode(0o700)).unwrap();
    let target = root.join("untouched");
    fs::write(&target, b"original").unwrap();
    symlink(&target, directory.join("gui.lock")).unwrap();
    assert!(canonical::acquire(&directory).is_err());
    fs::remove_file(directory.join("gui.lock")).unwrap();
    let mut guard = canonical::acquire(&directory).unwrap().unwrap();
    symlink(&target, directory.join("gui.sock")).unwrap();
    assert!(guard.bind().is_err());
    assert_eq!(fs::read(&target).unwrap(), b"original");
}
