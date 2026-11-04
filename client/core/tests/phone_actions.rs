#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use zerus_client_core::mobile::{
    self,
    phone_actions::{self, Target},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const PHONE: &str = "11111111-1111-4111-8111-111111111111";
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (Scratch, Target) {
    let root = std::env::temp_dir().join(format!(
        "zerus-phones-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_phones.py"),
        root.join("hgs"),
    )
    .unwrap();
    fs::write(root.join("phones.json"),json!({"configured":true,"workspace_id":"ws-example","workspace_name":"Example","server_url":"https://relay.example.invalid","encrypted":true,"devices":[{"id":PHONE,"name":"Example phone","platform":"ios"}]}).to_string()).unwrap();
    let list = mobile::phones(&root.join("hgs"), "studio")
        .unwrap()
        .unwrap();
    let target = Target::capture(&list).unwrap();
    (Scratch(root), target)
}
fn writes(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| {
            v.as_array()
                .unwrap()
                .iter()
                .any(|v| v == "invite" || v == "revoke-phone")
        })
        .collect()
}
#[test]
fn pins_the_peer_workspace_and_relay_and_returns_a_transient_invitation() {
    let (s, target) = fixture();
    let invite = phone_actions::invite(&s.0.join("hgs"), &target).unwrap();
    assert!(!invite.pair_code.is_empty());
    assert!(invite.pair_url.starts_with("zerus://pair?"));
    let calls = writes(&s.0);
    assert_eq!(calls.len(), 1);
    let args = calls[0].as_array().unwrap();
    assert_eq!(args[0], "@studio");
    assert!(args.iter().any(|v| v == "--workspace"));
    assert!(args.iter().any(|v| v == "--server"));
    assert!(!fs::read_to_string(s.0.join("calls.jsonl"))
        .unwrap()
        .contains(&invite.pair_code));
}
#[test]
fn changed_workspace_refuses_before_any_effect() {
    let (s, target) = fixture();
    let mut data: Value =
        serde_json::from_slice(&fs::read(s.0.join("phones.json")).unwrap()).unwrap();
    data["workspace_id"] = json!("ws-other");
    fs::write(s.0.join("phones.json"), data.to_string()).unwrap();
    assert!(phone_actions::invite(&s.0.join("hgs"), &target).is_err());
    assert!(writes(&s.0).is_empty());
}
#[test]
fn rejects_a_returned_invitation_for_another_workspace() {
    let (s, target) = fixture();
    fs::write(s.0.join("mode"), "wrong").unwrap();
    let error = phone_actions::invite(&s.0.join("hgs"), &target)
        .err()
        .unwrap();
    assert!(error.starts_with("This pairing code belongs to another workspace"));
    assert_eq!(writes(&s.0).len(), 1);
}
#[test]
fn an_uncertain_revoke_never_replays_and_a_fresh_read_finds_it_gone() {
    let (s, target) = fixture();
    fs::write(s.0.join("mode"), "unknown").unwrap();
    assert!(phone_actions::revoke(&s.0.join("hgs"), &target, PHONE).is_err());
    assert!(phone_actions::revoke(&s.0.join("hgs"), &target, PHONE).is_err());
    assert_eq!(writes(&s.0).len(), 1);
    let list = mobile::phones(&s.0.join("hgs"), "studio").unwrap().unwrap();
    assert!(list.phones.is_empty());
}
#[test]
fn revoke_checks_the_exact_receipt() {
    let (s, target) = fixture();
    phone_actions::revoke(&s.0.join("hgs"), &target, PHONE).unwrap();
    assert_eq!(writes(&s.0).len(), 1);
}

#[test]
fn malformed_device_identifiers_refuse_before_the_command() {
    let (s, target) = fixture();
    for phone in ["--json", "not-a-device"] {
        assert!(phone_actions::revoke(&s.0.join("hgs"), &target, phone).is_err());
    }
    assert!(writes(&s.0).is_empty());
}
