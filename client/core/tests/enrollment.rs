#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use zerus_client_core::mobile::{
    self,
    enrollment::{self, Mode, Request, Target},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn program(&self) -> PathBuf {
        self.0.join("connector")
    }
    fn config(&self) -> PathBuf {
        self.0.join("connector.json")
    }
    fn mode(&self, s: &str) {
        fs::write(self.0.join("mode"), s).unwrap();
    }
    fn effects(&self) -> Vec<String> {
        fs::read_to_string(self.0.join("effects.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|s| {
                serde_json::from_str::<Value>(s).unwrap()["action"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
    fn target(&self) -> Target {
        Target::capture(&mobile::read_status(&self.program(), &self.config()).unwrap()).unwrap()
    }
    fn state(&self, value: Value) {
        fs::write(self.0.join("status.json"), value.to_string()).unwrap();
    }
}
fn fixture(active: bool) -> (Scratch, Target) {
    let root = std::env::temp_dir().join(format!(
        "zerus-enroll-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_enrollment.py"),
        root.join("connector"),
    )
    .unwrap();
    let s = Scratch(root);
    s.state(if active{json!({"state":"active","encrypted":true,"config_revision":"a".repeat(64),"server_url":"https://old-relay.example.invalid","node_id":"node-old","workspace_id":"ws-old","running":true,"connector_state":"running","generation":"e".repeat(32)})}else{json!({"state":"unconfigured","encrypted":false,"config_revision":""})});
    let target = s.target();
    (s, target)
}
fn request(mode: Mode) -> Request {
    Request {
        mode,
        relay: "https://new-relay.example.invalid/".into(),
        code: if mode == Mode::Join {
            "ze1.synthetic".into()
        } else {
            String::new()
        },
    }
}
#[test]
fn create_uses_only_stdin_for_the_original_configuration_and_no_stop() {
    let (s, t) = fixture(false);
    let answer = enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Create),
        "workstation",
    )
    .unwrap();
    assert!(answer.connected);
    assert_eq!(answer.workspace.node_id, "node-new");
    assert_eq!(s.effects(), ["create"]);
}
#[test]
fn confirmed_change_stops_the_exact_generation_before_sending_join() {
    let (s, t) = fixture(true);
    enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation",
    )
    .unwrap();
    assert_eq!(s.effects(), ["stop", "switch-join"]);
    assert!(!fs::read_to_string(s.0.join("calls.ndjson"))
        .unwrap()
        .contains("ze1.synthetic"));
}
#[test]
fn draining_stop_never_dispatches_the_change() {
    let (s, t) = fixture(true);
    s.mode("draining");
    let answer = enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation",
    )
    .unwrap();
    assert_eq!(answer.notice, enrollment::DRAINING);
    assert!(answer.continue_node.is_none());
    assert_eq!(s.effects(), ["stop"]);
}
#[test]
fn a_replacement_generation_after_stop_refuses_before_join() {
    let (s, t) = fixture(true);
    s.mode("replacement");
    assert!(enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation"
    )
    .is_err());
    assert_eq!(s.effects(), ["stop"]);
}
#[test]
fn pending_enrollment_is_recovered_by_check_without_another_create() {
    let (s, t) = fixture(false);
    s.mode("pending");
    let answer = enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation",
    )
    .unwrap();
    assert!(!answer.connected);
    let pending = s.target();
    assert!(!pending.can_enroll());
    s.mode("");
    assert!(
        enrollment::check(&s.program(), &s.config(), &pending, false)
            .unwrap()
            .connected
    );
    assert_eq!(s.effects(), ["join", "check"]);
}
#[test]
fn legacy_local_disconnect_is_a_separate_explicit_check() {
    let (s, _) = fixture(false);
    s.state(json!({"state":"legacy","encrypted":false,"config_revision":"a".repeat(64),"pending":{"state":"legacy-retirement-unconfirmed","node_id":"node-new","action":"switch-join"}}));
    let target = s.target();
    let waiting = enrollment::check(&s.program(), &s.config(), &target, false).unwrap();
    assert!(!waiting.connected);
    assert!(
        enrollment::check(&s.program(), &s.config(), &target, true)
            .unwrap()
            .connected
    );
    assert_eq!(s.effects(), ["check", "check"]);
}
#[test]
fn automatic_continuation_requires_the_exact_draining_reply_and_saved_node() {
    for mode in ["migration-draining", "migration-blocked"] {
        let (s, t) = fixture(false);
        s.mode(mode);
        let answer = enrollment::enroll(
            &s.program(),
            &s.config(),
            &t,
            &request(Mode::Create),
            "workstation",
        )
        .unwrap();
        assert_eq!(answer.continue_node.is_some(), mode == "migration-draining");
    }
}
#[test]
fn invalid_or_unknown_results_never_start_a_second_enrollment() {
    let (s, t) = fixture(false);
    s.mode("unknown");
    assert!(enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation"
    )
    .is_err());
    assert_eq!(s.effects(), ["join"]);
    let (s, t) = fixture(false);
    s.mode("wrong-relay");
    assert!(enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Create),
        "workstation"
    )
    .is_err());
}

#[test]
fn old_connector_never_receives_stop_or_running_change() {
    let (s, _) = fixture(true);
    fs::write(s.0.join("old-connector"), "old").unwrap();
    let t = s.target();
    assert!(!t.stop_supported);
    assert!(!t.can_enroll());
    assert!(enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation"
    )
    .is_err());
    assert!(s.effects().is_empty());
    let mut stopped =
        serde_json::from_str::<Value>(&fs::read_to_string(s.0.join("status.json")).unwrap())
            .unwrap();
    stopped["running"] = json!(false);
    stopped["connector_state"] = json!("stopped");
    stopped["generation"] = Value::Null;
    s.state(stopped);
    let t = s.target();
    enrollment::enroll(
        &s.program(),
        &s.config(),
        &t,
        &request(Mode::Join),
        "workstation",
    )
    .unwrap();
    assert_eq!(s.effects(), ["switch-join"]);
}
