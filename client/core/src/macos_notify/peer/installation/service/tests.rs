//! Native negative/owned-child controls only; no genuine signed pair positive.
use super::*;
use std::{
    fs,
    process::{Command, Stdio},
};

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().expect("owned child wait").is_none() {
            self.0
                .kill()
                .expect("terminate only the original fixture Child");
        }
        self.0.wait().expect("reap original fixture Child");
    }
}
fn sleeper(root: &Path) -> Owned {
    let mut command = Command::new("/bin/sleep");
    command
        .arg("30")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, name) in [
        ("HOME", "home"),
        ("TMPDIR", "tmp"),
        ("TMUX_TMPDIR", "tmux"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("HGS_CONFIG_DIR", "hgs-config"),
        ("HGS_STATE_DIR", "hgs-state"),
    ] {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        command.env(key, path);
    }
    Owned(command.spawn().unwrap())
}

#[test]
fn external_bare_service_cannot_select_a_synthetic_unpaired_or_missing_bundle() {
    let tree = super::super::tests::Tree::new();
    // Checks the actual running test executable. No owner path/identity is read.
    assert!(select_for_service(&tree.0).is_err());
    assert!(select_for_service(&tree.0.join("missing.app")).is_err());
    assert!(select_for_service(Path::new("relative.app")).is_err());
    assert!(select_own().is_err());
}

#[test]
fn original_child_birth_is_native_and_a_reaped_handle_never_reenters_validation() {
    let tree = super::super::tests::Tree::new();
    let mut child = sleeper(tree.0.parent().unwrap());
    let before = birth(&mut child.0).unwrap();
    assert_eq!(before.pid, child.0.id());
    assert_eq!(before.parent, std::process::id());
    assert_eq!(before.uid, unsafe { libc::geteuid() });
    assert_eq!(before.real_uid, unsafe { libc::getuid() });
    assert!(birth(&mut child.0).unwrap() == before);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    // try_wait sees Child's cached terminal status, before any PID lookup.
    assert!(matches!(birth(&mut child.0), Err(Refusal::Child)));
    assert!(matches!(birth(&mut child.0), Err(Refusal::Child)));
}

#[test]
fn original_owned_process_is_not_the_selected_app_just_because_it_is_a_child() {
    let tree = super::super::tests::Tree::new();
    let mut child = sleeper(tree.0.parent().unwrap());
    assert!(birth(&mut child.0).is_ok());
    let mut expected = super::super::tests::fake_signature(&tree.0, true);
    expected.requirement = "anchor apple generic".into();
    // A real native guest lookup for our original Child; path/hash/identifier
    // still cannot match the synthetic selected app. This is not a signer test.
    autoreleasepool(|_| assert!(verify_child_code(&child.0, &expected).is_err()));
}
