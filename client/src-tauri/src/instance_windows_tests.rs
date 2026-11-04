use std::{
    fs,
    path::PathBuf,
    process::{Child as Process, Command, Stdio},
};

struct Child(Process);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zerus-gui-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn launch(&self, scope: &str, name: &str) -> Child {
        let home = self.0.join(scope);
        fs::create_dir_all(&home).unwrap();
        let control = self.0.join(format!("{scope}.json"));
        fs::write(
            &control,
            serde_json::to_vec(&serde_json::json!({
                "root": self.0, "scope": scope, "canonical": name
            }))
            .unwrap(),
        )
        .unwrap();
        Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "instance::tests::canonical_tests::launch_fixture",
                    "--nocapture",
                ])
                .env("ZERUS_GUI_INSTANCE_TEST_CONTROL", control)
                .env("USERPROFILE", &home)
                .env("LOCALAPPDATA", &home)
                .env("TEMP", &home)
                .env("TMP", &home)
                .env("TMPDIR", &home)
                .env("HGS_CONFIG_DIR", home.join("config"))
                .env("HGS_STATE_DIR", home.join("state"))
                .env("ZERUS_CLIENT_DESKTOP_SERVICE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
    fn wait_for(&self, suffix: &str, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.0.join(suffix).exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "fixture exited early"
            );
            assert!(Instant::now() < deadline, "fixture readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn launch_fixture() {
    let Some(control) = std::env::var_os("ZERUS_GUI_INSTANCE_TEST_CONTROL") else {
        return;
    };
    let control: serde_json::Value = serde_json::from_slice(&fs::read(control).unwrap()).unwrap();
    let root = Path::new(control["root"].as_str().unwrap());
    let scope = control["scope"].as_str().unwrap();
    let name = control["canonical"].as_str().unwrap();
    let claimed = match claim_in(
        "hgs-tray",
        Activation::Sessions,
        None,
        Some(name.into()),
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
    fs::write(root.join(format!("{scope}.ready")), b"owner").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !root.join(format!("{scope}.stop")).exists() {
        assert!(Instant::now() < deadline, "fixture exit deadline");
        if let Ok(activation) = claimed.activations.recv_timeout(Duration::from_millis(20)) {
            assert_eq!(activation, Activation::Sessions);
            fs::write(root.join(format!("{scope}.activation")), b"sessions\n").unwrap();
        }
    }
    InstanceState(Mutex::new(Some(claimed.listening))).release();
    assert!(
        Canonical::acquire(name.into()).unwrap().is_none(),
        "final teardown must not release GUI exclusion before actual process exit"
    );
}

#[test]
fn distinct_temp_profile_and_config_scopes_handoff_before_gui_effects() {
    let fixture = Fixture::new();
    let name = format!("zerus-canonical-test-{}", uuid::Uuid::new_v4());
    let mut owner = fixture.launch("first", &name);
    fixture.wait_for("first.ready", &mut owner);
    let mut second = fixture.launch("second", &name);
    fixture.wait_for("second.forwarded", &mut second);
    fixture.wait_for("first.activation", &mut owner);
    assert!(!fixture.0.join("second.ready").exists());
    assert!(!fixture.0.join("second.effects").exists());
    assert_eq!(fs::read_dir(fixture.0.join("second")).unwrap().count(), 0);
    fs::write(fixture.0.join("first.stop"), b"").unwrap();
    assert!(owner.0.wait().unwrap().success());
    assert!(second.0.wait().unwrap().success());
}

#[test]
fn a_crash_releases_canonical_kernel_exclusion_without_pid_cleanup() {
    let fixture = Fixture::new();
    let name = format!("zerus-canonical-crash-{}", uuid::Uuid::new_v4());
    let mut owner = fixture.launch("crashed", &name);
    fixture.wait_for("crashed.ready", &mut owner);
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    let mut next = fixture.launch("next", &name);
    fixture.wait_for("next.ready", &mut next);
    fs::write(fixture.0.join("next.stop"), b"").unwrap();
    assert!(next.0.wait().unwrap().success());
}

#[test]
fn canonical_name_uses_token_sid_and_lifetime_not_profile_configuration() {
    assert_eq!(
        Canonical::name("S-1-5-21-123"),
        "com.hgdev.hgs-tray.gui-S-1-5-21-123"
    );
    let name = format!("zerus-canonical-unit-{}", uuid::Uuid::new_v4());
    let guard = Canonical::acquire(name.clone()).unwrap().unwrap();
    assert!(Canonical::acquire(name.clone()).unwrap().is_none());
    assert_eq!(
        crate::entry::notification_host(["--desktop-notification-host"], || 23),
        Some(23)
    );
    drop(guard);
    assert!(Canonical::acquire(name).unwrap().is_some());
}
