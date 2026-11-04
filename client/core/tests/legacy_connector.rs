#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
};
use zerus_client_core::mobile::legacy::{self, windows};
struct Fixture {
    root: PathBuf,
    config: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        // Parallel tests can read the same clock value on macOS; the counter keeps
        // each fixture root distinct.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "legacy-manager-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let config = root.join("config.json");
        fs::write(&config,serde_json::json!({"configured":true,"running":false,"revision":"a".repeat(64),"generation":null,"server_url":"https://relay.example.invalid","state_dir":"example-receipts","startup_at_login":false}).to_string()).unwrap();
        Self { root, config }
    }
    fn exe(&self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_legacy_manager.py")
    }
    fn calls(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn validation_is_rearmable_only_after_the_fresh_windows_status_matches() {
    let f = Fixture::new();
    let old = windows::status(&f.exe(), &f.config).unwrap();
    let edit = || {
        Some(legacy::Edit {
            relay: "http://relay.example.invalid".into(),
            replacement: String::new(),
            directory: PathBuf::new(),
        })
    };
    assert!(matches!(
        windows::apply(&f.exe(), &f.config, &old, "save", edit()),
        Err(legacy::Failure::Refused(_))
    ));
    assert!(f.calls().iter().all(|v| v["action"] == "status"));
    fs::write(f.root.join("mode"), "bad-status").unwrap();
    assert!(matches!(
        windows::apply(&f.exe(), &f.config, &old, "save", edit()),
        Err(legacy::Failure::Locked(_))
    ));
}
#[test]
fn windows_save_uses_stdin_after_exact_revision_and_never_echoes_the_token() {
    let f = Fixture::new();
    let old = windows::status(&f.exe(), &f.config).unwrap();
    let (new, _) = windows::apply(
        &f.exe(),
        &f.config,
        &old,
        "save",
        Some(legacy::Edit {
            relay: "https://other.example.invalid".into(),
            replacement: "transient-fixture".into(),
            directory: PathBuf::new(),
        }),
    )
    .unwrap();
    assert_eq!(new.server_url, "https://other.example.invalid");
    let calls = f.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1]["action"], "status");
    assert_eq!(
        calls[2]["keys"],
        serde_json::json!(["node_token", "revision", "server_url"])
    );
    assert!(!fs::read_to_string(f.root.join("calls"))
        .unwrap()
        .contains("transient-fixture"));
}
#[test]
fn windows_start_and_generation_bound_stop_use_existing_manager_once() {
    let f = Fixture::new();
    let old = windows::status(&f.exe(), &f.config).unwrap();
    let (running, _) = windows::apply(&f.exe(), &f.config, &old, "start", None).unwrap();
    assert!(running.running);
    let (stopped, _) = windows::apply(&f.exe(), &f.config, &running, "stop", None).unwrap();
    assert!(!stopped.running);
    assert_eq!(
        f.calls().iter().filter(|v| v["action"] == "stop").count(),
        1
    );
}
#[test]
fn a_stale_snapshot_refuses_before_any_effect() {
    let f = Fixture::new();
    let old = windows::status(&f.exe(), &f.config).unwrap();
    let mut next: serde_json::Value =
        serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    next["revision"] = serde_json::json!("b".repeat(64));
    fs::write(&f.config, next.to_string()).unwrap();
    assert_eq!(
        windows::apply(&f.exe(), &f.config, &old, "start", None)
            .err()
            .unwrap()
            .message(),
        legacy::CHANGED
    );
    assert!(f.calls().iter().all(|v| v["action"] == "status"));
}
#[test]
fn unknown_effect_and_untrusted_status_never_surface_output_or_replay() {
    let f = Fixture::new();
    let old = windows::status(&f.exe(), &f.config).unwrap();
    fs::write(f.root.join("mode"), "unknown").unwrap();
    assert_eq!(
        windows::apply(&f.exe(), &f.config, &old, "start", None)
            .err()
            .unwrap()
            .message(),
        windows::UNCONFIRMED
    );
    assert_eq!(
        f.calls().iter().filter(|v| v["action"] == "start").count(),
        1
    );
    fs::write(f.root.join("mode"), "bad-status").unwrap();
    assert_eq!(
        windows::status(&f.exe(), &f.config).err().unwrap(),
        windows::UNCONFIRMED
    );
}
