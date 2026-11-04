#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use zerus_client_core::mobile::{
    self,
    connector_actions::{self, Kind},
    enrollment::Target,
};
static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new(running: bool) -> Self {
        let path = std::env::temp_dir().join(format!(
            "zerus-connector-actions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let f = Self(path);
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_enrollment.py"),
            f.program(),
        )
        .unwrap();
        f.save(json!({"state":"active","encrypted":true,"config_revision":"a".repeat(64),"server_url":"https://relay.example.invalid","node_id":"node-old","workspace_id":"ws-old","running":running,"connector_state":if running{"running"}else{"stopped"},"generation":if running{json!("e".repeat(32))}else{Value::Null}}));
        f
    }
    fn path(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
    fn program(&self) -> PathBuf {
        self.path("connector")
    }
    fn config(&self) -> PathBuf {
        self.path("connector.json")
    }
    fn save(&self, v: Value) {
        fs::write(self.path("status.json"), v.to_string()).unwrap();
    }
    fn value(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("status.json")).unwrap()).unwrap()
    }
    fn workspace(&self) -> mobile::Workspace {
        mobile::read_status(&self.program(), &self.config()).unwrap()
    }
    fn target(&self, kind: Kind) -> Target {
        connector_actions::admit(&self.workspace(), kind).unwrap()
    }
    fn apply(&self, t: &Target, k: Kind) -> Result<connector_actions::Answer, String> {
        connector_actions::apply(&self.program(), &self.config(), t, k)
    }
    fn effects(&self) -> Vec<String> {
        fs::read_to_string(self.path("effects.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["action"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .collect()
    }
}
#[test]
fn start_and_confirmed_stop_are_separate_exact_actions() {
    let f = Fixture::new(false);
    let started = f.apply(&f.target(Kind::Start), Kind::Start).unwrap();
    assert!(started.workspace.running);
    assert_eq!(started.notice, "");
    let stopped = f.apply(&f.target(Kind::Stop), Kind::Stop).unwrap();
    assert!(!stopped.workspace.running);
    assert_eq!(stopped.notice, "Connector stopped.");
    assert_eq!(f.effects(), ["start", "stop"]);
}
#[test]
fn old_connector_receives_no_stop() {
    let f = Fixture::new(true);
    let original = f.target(Kind::Stop);
    fs::write(f.path("old-connector"), "old").unwrap();
    assert!(connector_actions::admit(&f.workspace(), Kind::Stop).is_err());
    assert!(f.apply(&original, Kind::Stop).is_err());
    assert!(f.effects().is_empty());
}
#[test]
fn a_changed_generation_refuses_before_the_stop_call() {
    let f = Fixture::new(true);
    let t = f.target(Kind::Stop);
    let mut v = f.value();
    v["generation"] = json!("b".repeat(32));
    f.save(v);
    assert!(f.apply(&t, Kind::Stop).is_err());
    assert!(f.effects().is_empty());
}
#[test]
fn a_draining_reply_is_not_an_automatic_retry() {
    let f = Fixture::new(true);
    fs::write(f.path("mode"), "draining").unwrap();
    let a = f.apply(&f.target(Kind::Stop), Kind::Stop).unwrap();
    assert!(a.workspace.running);
    assert!(a.notice.contains("draining"));
    assert!(connector_actions::admit(&f.workspace(), Kind::Stop).is_err());
    assert_eq!(f.effects(), ["stop"]);
}
#[test]
fn unknown_stop_never_repeats_the_old_intent() {
    let f = Fixture::new(true);
    let t = f.target(Kind::Stop);
    fs::write(f.path("mode"), "unknown").unwrap();
    assert!(f.apply(&t, Kind::Stop).is_err());
    assert!(f.apply(&t, Kind::Stop).is_err());
    assert_eq!(f.effects(), ["stop"]);
}
#[test]
fn stop_can_drain_a_retired_generation_with_pending_enrollment() {
    let f = Fixture::new(true);
    let mut v = f.value();
    v["state"] = json!("retired");
    v["pending"] = json!({"state":"pending","node_id":"node-next","action":"switch-join"});
    f.save(v);
    let a = f.apply(&f.target(Kind::Stop), Kind::Stop).unwrap();
    assert!(!a.workspace.running);
    assert!(a.workspace.pending.is_some());
    assert!(connector_actions::admit(&a.workspace, Kind::Start).is_err());
    assert_eq!(f.effects(), ["stop"]);
}
