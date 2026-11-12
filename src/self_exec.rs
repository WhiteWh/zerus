//! Production helper entry points require the CLI's real `main` dispatcher.
//! libtest replaces it and interprets helper argv as test filters instead.
use std::{io, path::PathBuf};

pub(crate) const TEST_IMAGE: &str = "A unit-test image cannot launch a production hgs helper";

/// Check before manager activation or any other work leading to self-exec.
/// Explicit test subprocesses still use their exact test filters directly.
pub(crate) fn ensure() -> io::Result<()> {
    if cfg!(test) {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, TEST_IMAGE))
    } else {
        Ok(())
    }
}

pub(crate) fn current() -> io::Result<PathBuf> {
    ensure()?;
    std::env::current_exe()
}

/// Names the one ignored probe child its parent started.
#[cfg(all(test, target_os = "linux"))]
const PROBE: &str = "HGS_SELF_EXEC_PROBE";

/// The first statement of every ignored probe child, before any I/O. A child
/// reached without its parent's exact marker fails here instead of probing;
/// probe children never contain a spawning path of their own.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn probe_child(name: &str) {
    assert_eq!(
        std::env::var(PROBE).ok().as_deref(),
        Some(name),
        "run only by its private_probe parent"
    );
}

#[cfg(all(test, target_os = "linux"))]
fn write_manager(path: &std::path::Path) {
    crate::test_executable::write_executable(
        path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/manager-called\"\nexit 0\n",
    );
}

/// Run the distinct ignored test `child` alone in an exact-filter subprocess
/// with private stores, synthetic service units, a fake service manager and an
/// independent descendant supervisor, so a regressed guard stays bounded. The
/// parent never runs probe code itself. The default configuration directory is
/// below the private home, where service-backed paths treat it as default.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn private_probe(child: &str) {
    private_probe_with(child, 10, 16);
}

/// The same isolation with a larger bound, for children that run real Git
/// (its helpers count toward the descendant budget).
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn private_probe_with(child: &str, seconds: u32, processes: u32) {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for name in ["tmp", "runtime"] {
        fs::create_dir(root.join(name)).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let called = root.join("manager-called");
    // Recording-only shims: none of them runs what it is handed.
    for manager in ["systemctl", "launchctl", "tmux"] {
        let path = bin.join(manager);
        write_manager(&path);
    }
    let units = root.join(".config/systemd/user");
    fs::create_dir_all(&units).unwrap();
    for unit in [
        "hgs-voice.service",
        "hgs-recovery.service",
        "hgs-swarm.service",
    ] {
        fs::write(units.join(unit), "synthetic fixture\n").unwrap();
    }
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bounded_process.py"
        ))
        .args([
            "--seconds",
            &seconds.to_string(),
            "--max-processes",
            &processes.to_string(),
            "--",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([child, "--exact", "--ignored", "--test-threads=1"])
        .env_clear()
        .env("LANG", "C.UTF-8")
        .env(PROBE, child)
        .env("HOME", &root)
        .env("TMPDIR", root.join("tmp"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("HGS_STATE_DIR", root.join("state/hgs"))
        .env(
            "PATH",
            std::env::join_paths([bin, "/usr/bin".into(), "/bin".into()]).unwrap(),
        )
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated self-exec probe failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // An exact filter that matched nothing would also succeed.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the probe child did not run exactly once:\n{stdout}"
    );
    assert!(
        !called.exists(),
        "test image activated a service manager: {:?}",
        fs::read_to_string(&called)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn recording_managers_execute_while_parent_writers_are_retained() {
        let root = tempfile::tempdir().unwrap();
        let held = crate::test_executable::HeldWriters::arm();
        let paths: Vec<_> = ["systemctl", "launchctl", "tmux"]
            .into_iter()
            .map(|name| {
                let path = root.path().join(name);
                write_manager(&path);
                path
            })
            .collect();
        for path in &paths {
            let status = std::process::Command::new(path)
                .arg(path.file_name().unwrap())
                .env("HOME", root.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join("manager-called"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        held.assert_separate(&paths);
    }

    #[test]
    fn libtest_never_returns_a_production_helper_image() {
        assert_eq!(
            ensure().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(current().unwrap_err().to_string(), TEST_IMAGE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_probe_child_without_its_exact_marker_fails_first() {
        // This parent carries no marker, so a lost marker can never reach probe
        // code; a different child's marker does not admit this one either.
        let other = std::panic::catch_unwind(|| probe_child("self_exec::tests::missing"));
        assert!(other.is_err());
    }
}
