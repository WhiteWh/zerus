#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use zerus_client_core::mobile::{
    self,
    computer_codes::{self, Target},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn connector(&self) -> PathBuf {
        self.0.join("connector")
    }
    fn config(&self) -> PathBuf {
        self.0.join("connector.json")
    }
    fn mode(&self, value: &str) {
        fs::write(self.0.join("mode"), value).unwrap();
    }
    fn effects(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("effects.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
fn fixture() -> (Scratch, Target) {
    let root = std::env::temp_dir().join(format!(
        "zerus-computer-codes-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_workspace_codes.py"),
        root.join("connector"),
    )
    .unwrap();
    fs::write(root.join("status.json"), json!({"state":"active","encrypted":true,"config_revision":"a".repeat(64),"server_url":"https://relay.example.invalid","node_id":"node-example","workspace_id":"ws-example","workspace_name":"Example team","computer_name":"workstation","running":true,"connector_state":"running","generation":"b".repeat(32)}).to_string()).unwrap();
    let scratch = Scratch(root);
    let workspace = mobile::read_status(&scratch.connector(), &scratch.config()).unwrap();
    let target = Target::capture(&workspace).unwrap();
    (scratch, target)
}
#[test]
fn issue_is_revision_bound_and_credential_never_appears_in_logged_argv_or_payload() {
    let (s, t) = fixture();
    let result = computer_codes::invite(&s.connector(), &s.config(), &t).unwrap();
    assert!(result.code.starts_with("ze1."));
    assert_eq!(s.effects().len(), 1);
    let calls = fs::read_to_string(s.0.join("calls.ndjson")).unwrap();
    assert!(!calls.contains(&result.code));
    let calls: Vec<Value> = calls
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        calls.last().unwrap()["payload"]["expected_revision"],
        t.config_revision
    );
}
#[test]
fn changed_snapshot_refuses_before_any_effect() {
    let (s, t) = fixture();
    let mut state: Value =
        serde_json::from_slice(&fs::read(s.0.join("status.json")).unwrap()).unwrap();
    state["config_revision"] = json!("c".repeat(64));
    fs::write(s.0.join("status.json"), state.to_string()).unwrap();
    assert!(computer_codes::invite(&s.connector(), &s.config(), &t).is_err());
    assert!(s.effects().is_empty());
}
#[test]
fn backend_cas_refuses_a_change_after_the_fresh_read() {
    let (s, t) = fixture();
    s.mode("raced");
    assert!(computer_codes::invite(&s.connector(), &s.config(), &t).is_err());
    assert!(s.effects().is_empty());
}
#[test]
fn another_relay_or_purpose_is_never_presented_as_a_computer_code() {
    for mode in ["wrong-relay", "wrong-purpose"] {
        let (s, t) = fixture();
        s.mode(mode);
        assert!(computer_codes::invite(&s.connector(), &s.config(), &t).is_err());
        assert_eq!(s.effects().len(), 1);
    }
}
#[test]
fn cancel_requires_the_confirmed_receipt_and_suppresses_raw_errors() {
    let (s, t) = fixture();
    s.mode("unknown");
    assert_eq!(
        computer_codes::cancel(&s.connector(), &s.config(), &t, "invitation-example").unwrap_err(),
        computer_codes::UNCONFIRMED
    );
    assert_eq!(s.effects().len(), 1);
    s.mode("bad-receipt");
    assert!(computer_codes::cancel(&s.connector(), &s.config(), &t, "invitation-example").is_err());
}
#[test]
fn pending_workspace_cannot_issue_a_code_and_check_uses_existing_status_shape() {
    let (s, t) = fixture();
    let mut workspace = computer_codes::check(&s.connector(), &s.config(), &t).unwrap();
    workspace.pending = Some("Saved connection".into());
    assert!(Target::capture(&workspace).is_err());
    computer_codes::cancel(&s.connector(), &s.config(), &t, "invitation-example").unwrap();
    assert_eq!(s.effects().len(), 2);
}
