use super::*;
use std::{fs, path::PathBuf, sync::atomic::AtomicU64};
const NONCE: &str = "11111111-1111-4111-8111-111111111111";
struct Scratch(PathBuf);
impl Scratch {
    fn new(mode: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("mode"), mode).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_startup.py");
        std::os::unix::fs::symlink(fixture, path.join("hgs")).unwrap();
        Self(path)
    }
    fn begin(&self) -> Bootstrap {
        Bootstrap::begin(
            &self.0.join("hgs"),
            NONCE,
            &[("TEST_STARTUP_ROOT", self.0.to_str().unwrap())],
        )
        .unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn finish(held: &mut Bootstrap) -> Result<String, Refused> {
    loop {
        match held.try_finish()? {
            Some(configuration) => return Ok(configuration),
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
}
#[test]
fn startup_duplex_keeps_input_until_release_and_joins_before_returning_configuration() {
    let root = Scratch::new("good");
    let mut held = root.begin();
    let weak = Arc::downgrade(&held.resources);
    held.wait_ready().unwrap();
    assert_eq!(held.try_finish(), Err(Refused::Invalid));
    assert!(!held.worker.as_ref().unwrap().is_finished());
    held.release().unwrap();
    assert_eq!(held.release(), Err(Refused::Invalid));
    assert_eq!(finish(&mut held).unwrap(), "a".repeat(64));
    assert!(held.worker.is_none());
    assert_eq!(Arc::strong_count(&held.resources), 1);
    drop(held);
    assert!(weak.upgrade().is_none());
}
#[test]
fn startup_help_marker_is_exact_and_old_cli_never_receives_an_unknown_command() {
    let root = Scratch::new("old");
    let mut held = root.begin();
    assert_eq!(held.wait_ready(), Err(Refused::Unsupported));
    assert!(held.worker.is_none());
    assert_eq!(Arc::strong_count(&held.resources), 1);
    assert_eq!(
        fs::read_to_string(root.0.join("calls")).unwrap(),
        "[\"--help\"]\n"
    );
}
#[test]
fn startup_foreign_extra_oversized_and_partial_frames_never_become_ready() {
    for mode in ["nonce", "extra", "oversize", "partial"] {
        let root = Scratch::new(mode);
        let mut held = root.begin();
        assert!(held.wait_ready().is_err(), "{mode}");
        assert!(held.configuration.is_none());
    }
}
#[test]
fn startup_duplicate_reply_tail_and_child_failure_never_publish_configuration() {
    for mode in ["duplicate", "tail", "failure"] {
        let root = Scratch::new(mode);
        let mut held = root.begin();
        held.wait_ready().unwrap();
        held.release().unwrap();
        assert!(finish(&mut held).is_err(), "{mode}");
    }
}
#[test]
fn startup_waits_for_actual_inherited_reader_eof_after_the_helper_exits() {
    let root = Scratch::new("held");
    let mut held = root.begin();
    held.wait_ready().unwrap();
    held.release().unwrap();
    let start = Instant::now();
    assert_eq!(finish(&mut held).unwrap(), "a".repeat(64));
    assert!(start.elapsed() >= Duration::from_millis(200));
    assert_eq!(
        fs::read_to_string(root.0.join("reader-ended")).unwrap(),
        "ended"
    );
}
#[test]
fn startup_unresolved_descendant_stays_unknown_and_pinned_after_outer_drop() {
    let root = Scratch::new("unknown");
    let mut held = root.begin();
    held.wait_ready().unwrap();
    held.release().unwrap();
    let weak = Arc::downgrade(&held.resources);
    assert_eq!(finish(&mut held), Err(Refused::Unknown));
    assert!(!root.0.join("reader-ended").exists());
    drop(held);
    assert!(weak.upgrade().is_some());
    // Wait for this fixture's own finite descendant before removing its files.
    let end = Instant::now() + Duration::from_secs(2);
    while !root.0.join("reader-ended").exists() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        weak.upgrade().is_some(),
        "unknown completion must remain pinned"
    );
}
#[test]
fn startup_dropped_owner_cannot_publish_or_recreate_the_original_worker() {
    let root = Scratch::new("good");
    let mut held = root.begin();
    held.wait_ready().unwrap();
    let weak = Arc::downgrade(&held.resources);
    drop(held);
    assert!(weak.upgrade().is_some());
}
#[test]
fn startup_invalid_nonce_refuses_before_any_child_and_frames_bind_the_exact_configuration() {
    let root = Scratch::new("good");
    assert!(matches!(
        Bootstrap::begin(&root.0.join("hgs"), "invalid", &[]),
        Err(Refused::Invalid)
    ));
    assert!(!root.0.join("calls").exists());
    for op in ["ready", "release", "released"] {
        let bytes = Frame::encode(op, NONCE, Some(&"a".repeat(64))).unwrap();
        let frame: Frame = serde_json::from_slice(&bytes).unwrap();
        assert!(frame.checked(op, NONCE).is_some());
        assert!(frame.checked("hold", NONCE).is_none());
        assert!(Frame::encode(op, NONCE, Some("unknown")).is_none());
    }
}

#[test]
fn startup_stall_does_not_renew_the_original_deadline() {
    let root = Scratch::new("stall");
    let start = Instant::now();
    let mut held = Bootstrap::begin_until(
        &root.0.join("hgs"),
        NONCE,
        &[("TEST_STARTUP_ROOT", root.0.to_str().unwrap())],
        start + Duration::from_millis(300),
    )
    .unwrap();
    assert!(held.wait_ready().is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(held.try_finish(), Err(Refused::TimedOut));
}

#[test]
fn startup_non_utf8_successful_help_is_never_old_cli_compatibility() {
    let root = Scratch::new("bad-utf8");
    let mut held = root.begin();
    assert_eq!(held.wait_ready(), Err(Refused::Unavailable));
    assert!(
        !root.0.join("pid").exists(),
        "no startup subcommand may run"
    );
    assert_eq!(
        fs::read_to_string(root.0.join("calls")).unwrap(),
        "[\"--help\"]\n"
    );
    assert!(held.configuration.is_none());
}
