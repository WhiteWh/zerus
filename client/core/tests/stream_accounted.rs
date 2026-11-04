//! Actual owned helper/pipe resources. Consumer callbacks belong to the next
//! Fleet barrier; a completed resource group does not drain an external sink.
mod common;
use common::{catalog, change_of, eventually, hello, status_of, FakeHgs, Seen};
use serde_json::json;
use std::{
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use zerus_client_core::{
    desktop::drain::{Gate, Observation, Role},
    stream, ComputerId, ComputerStatus, Timing,
};
use zerus_watch_protocol::{Control, Ping};
fn timing() -> Timing {
    Timing {
        silence: Duration::from_secs(30),
        reprobe: Duration::from_secs(30),
        stop_grace: Duration::from_millis(100),
        probe_timeout: Duration::from_secs(3),
        peer_probe_timeout: Duration::from_secs(3),
        ..Timing::scaled(0.01)
    }
}

#[test]
fn ordinary_frames_controls_and_owned_exit_keep_the_existing_behavior() {
    let fake = FakeHgs::new(
        "accounted-ordinary",
        json!({"":{"runs":[[{"lines":[hello("fixture"),catalog(&[])]},{"serve":true}]]}}),
    );
    let gate = Gate::new(1).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut seen = Seen::new(rx);
    let handle = stream::spawn_accounted(
        &fake.path(),
        &ComputerId::local(),
        timing(),
        tx,
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    )
    .unwrap();
    seen.until("live", |e| {
        change_of(e).and_then(|c| status_of(c, &ComputerId::local())) == Some(ComputerStatus::Live)
    });
    assert!(handle.send(Control::Ping(Ping { id: Some(json!(7)) })));
    eventually("unchanged control", || {
        fake.stdin("")
            .iter()
            .any(|c| *c == json!({"op":"ping","id":7}))
    });
    let closed = gate.close();
    handle.stop();
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(3)),
        Observation::Idle
    );
    eventually("owned stdin EOF", || fake.eofs("") == 1);
    assert_eq!(fake.runs("", &["watch", "--json"]).len(), 1);
}
#[test]
fn ordinary_legacy_results_remain_polled_and_release_after_owned_exit() {
    let fake = FakeHgs::new("accounted-legacy-normal", json!({"peer":{"marker":false}}));
    std::fs::write(fake.dir.join("ls-peer.json"), b"{\"sessions\":[]}").unwrap();
    let gate = Gate::new(1).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut seen = Seen::new(rx);
    let handle = stream::spawn_accounted(
        &fake.path(),
        &ComputerId::from("peer"),
        timing(),
        tx,
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    )
    .unwrap();
    seen.until("polled", |e| {
        change_of(e).and_then(|c| status_of(c, &ComputerId::from("peer")))
            == Some(ComputerStatus::Polled)
    });
    let closed = gate.close();
    handle.stop();
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(3)),
        Observation::Idle
    );
    assert_eq!(fake.runs("peer", &["ls", "--json"]).len(), 1);
}
#[test]
fn invalid_alias_refuses_before_a_helper_and_releases_admission() {
    let gate = Gate::new(1).unwrap();
    let (tx, _) = mpsc::channel();
    assert!(stream::spawn_accounted(
        std::path::Path::new("unavailable-fixture"),
        &ComputerId::from("-bad"),
        timing(),
        tx,
        Arc::new(gate.admit(Role::Fleet).unwrap())
    )
    .is_err());
    assert_eq!(gate.close().wait_until(Instant::now()), Observation::Idle);
}

#[test]
fn a_missing_helper_releases_the_group_without_inventing_child_exit() {
    let gate = Gate::new(1).unwrap();
    let (tx, _) = mpsc::channel();
    let handle = stream::spawn_accounted(
        std::path::Path::new("unavailable-fleet-accounting-fixture"),
        &ComputerId::local(),
        timing(),
        tx,
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    )
    .unwrap();
    handle.stop();
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(3)),
        Observation::Idle
    );
}

#[cfg(unix)]
mod retained {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    struct Fixture {
        _fake: FakeHgs,
        root: PathBuf,
    }
    impl Fixture {
        fn new(phase: &str, pipe: &str) -> Self {
            let fake = FakeHgs::new(&format!("accounted-{phase}-{pipe}"), json!({}));
            fs::remove_file(fake.path()).unwrap();
            std::os::unix::fs::symlink(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_accounted.py"),
                fake.path(),
            )
            .unwrap();
            let mut greeting = hello("fixture");
            if phase == "worker" {
                greeting["topics"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("organization"));
            }
            let organization = json!({"schema":1,"node_id":"node-1","swarm_id":"swarm-1","digest":"outdated","initialized":true,"conflicts":[],"preferences":{},"machines":[],"peers":{},"worker":{"running":true,"current":false},"organization":{"version":2,"default_project":"ungrouped","projects":[]}});
            fs::write(fake.dir.join("accounted.json"), json!({"phase":phase,"pipe":pipe,"marker":zerus_watch_protocol::HELP_MARKER,"frames":[greeting,catalog(&[])],"organization_frames":[{"t":"snapshot","topic":"organization","items":[{"key":"organization","row":organization}],"page":0,"last":true,"cursor":1}]}).to_string()).unwrap();
            Self {
                root: fake.dir.clone(),
                _fake: fake,
            }
        }
        fn path(&self) -> PathBuf {
            self.root.join("hgs")
        }
        fn ready(&self) {
            eventually("private helper reached ready", || {
                self.root.join("ready").exists()
            });
        }
        fn release(&self) {
            fs::write(self.root.join("release"), []).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::write(self.root.join("release"), []);
        }
    }
    fn check(phase: &str, pipe: &str) {
        let f = Fixture::new(phase, pipe);
        let gate = Gate::new(1).unwrap();
        let (tx, rx) = mpsc::channel();
        let handle = stream::spawn_accounted(
            &f.path(),
            &ComputerId::local(),
            timing(),
            tx,
            Arc::new(gate.admit(Role::Fleet).unwrap()),
        )
        .unwrap();
        if phase == "organization" {
            handle.follow_organization().unwrap();
        }
        let mut seen = Seen::new(rx);
        if phase == "worker" {
            seen.until("live organization-capable stream", |e| {
                change_of(e).and_then(|c| status_of(c, &ComputerId::local()))
                    == Some(ComputerStatus::Live)
            });
            handle.follow_organization().unwrap();
        }
        f.ready();
        if pipe == "stdin" {
            seen.until("live before the blocked write", |e| {
                change_of(e).and_then(|c| status_of(c, &ComputerId::local()))
                    == Some(ComputerStatus::Live)
            });
            assert!(handle.send(Control::Ping(Ping {
                id: Some(json!("x".repeat(2 << 20)))
            })));
            eventually("the control writer entered the fixture pipe", || {
                f.root.join("stdin-buffered").exists()
            });
        }
        let closed = gate.close();
        let began = Instant::now();
        handle.stop();
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "bounded stop; resources remain charged"
        );
        assert_eq!(
            closed.wait_until(Instant::now()),
            Observation::Pending,
            "{phase}/{pipe}"
        );
        f.release();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(5)),
            Observation::Idle,
            "{phase}/{pipe}"
        );
        let calls = fs::read_to_string(f.root.join("calls.jsonl")).unwrap();
        if phase == "probe" {
            assert_eq!(
                calls.lines().filter(|l| *l == "[\"-h\"]").count(),
                1,
                "retained primary prevents stacked probes"
            );
        }
    }
    #[test]
    fn help_stdout_is_retained_after_the_owned_probe_exits() {
        check("probe", "stdout");
    }
    #[test]
    fn watch_stdout_is_retained_after_bounded_stop() {
        check("watch", "stdout");
    }
    #[test]
    fn watch_stderr_is_retained_after_bounded_stop() {
        check("watch", "stderr");
    }
    #[test]
    fn blocked_control_stdin_keeps_the_supervisor_and_helper_charged() {
        check("watch", "stdin");
    }
    #[test]
    fn legacy_output_and_result_callback_remain_charged() {
        check("legacy", "stdout");
    }
    #[test]
    fn detached_worker_refresh_keeps_its_pipe_and_callback_charge() {
        check("worker", "stderr");
    }
    #[test]
    fn organization_worker_and_stderr_remain_charged() {
        check("organization", "stderr");
    }
}
