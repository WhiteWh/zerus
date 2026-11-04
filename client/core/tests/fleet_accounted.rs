//! Fleet retirement with actual private helpers, never an upstream source.
// The desktop starts the accounted Fleet only on Unix (src-tauri commands.rs);
// Windows keeps `Fleet::start`, whose stream EOF is covered in tests/fleet.rs.
#![cfg(unix)]
mod common;
use common::{catalog, eventually, hello, meta, status_of, FakeHgs, Seen};
use serde_json::json;
use std::{
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use zerus_client_core::{
    desktop::drain::{Gate, Observation, Role},
    ComputerId, ComputerStatus, Fleet, Timing,
};

fn timing() -> Timing {
    Timing {
        silence: Duration::from_secs(30),
        stop_grace: Duration::from_millis(100),
        ..Timing::scaled(0.01)
    }
}

#[test]
fn accounted_local_and_peer_keep_frames_controls_and_eof() {
    let fake = FakeHgs::new(
        "fleet-accounted-normal",
        json!({
            "":{"runs":[[{"lines":[hello("fixture"),catalog(&[]),meta(Some(&["peer"]))]},{"serve":true}]]},
            "peer":{"runs":[[{"lines":[hello("peer"),catalog(&[])]},{"serve":true}]]}
        }),
    );
    let gate = Gate::new(1).unwrap();
    let (tx, rx) = mpsc::channel();
    let fleet = Fleet::start_accounted(
        fake.path(),
        tx,
        timing(),
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    );
    let control = fleet.control();
    let mut seen = Seen::new(rx);
    for computer in ["", "peer"] {
        seen.until("live", |c| {
            status_of(c, &ComputerId::from(computer)) == Some(ComputerStatus::Live)
        });
    }
    assert_eq!(
        fleet.peers().unwrap().into_iter().collect::<Vec<_>>(),
        ["peer"]
    );
    assert!(fleet.accounting_available());
    control.request_stop();
    assert!(!fleet.reconnect(&ComputerId::from("missing")));
    fleet.stop();
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(3)),
        Observation::Idle
    );
    control.request_stop(); // Weak controls never keep the graph charged.
    for computer in ["", "peer"] {
        eventually("owned EOF", || fake.eofs(computer) == 1);
        assert_eq!(fake.runs(computer, &["watch", "--json"]).len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn a_removed_peer_keeps_the_fleet_charged_after_its_bounded_stop_returns() {
    use std::{fs, path::Path};
    let fake = FakeHgs::new("fleet-accounted-removed", json!({}));
    fs::remove_file(fake.path()).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_accounted.py"),
        fake.path(),
    )
    .unwrap();
    let marker = zerus_watch_protocol::HELP_MARKER;
    fs::write(fake.dir.join("accounted.json"),json!({"hosts":{
        "":{"phase":"idle","pipe":"stdout","marker":marker,"frames":[hello("fixture"),catalog(&[]),meta(Some(&["peer"]))],"remove_peers":meta(Some(&[]))},
        "peer":{"phase":"watch","pipe":"stderr","marker":marker,"frames":[hello("peer"),catalog(&[])]}
    }}).to_string()).unwrap();
    struct Release(std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("release"), []);
        }
    }
    let _release = Release(fake.dir.clone());
    let gate = Gate::new(1).unwrap();
    let (tx, rx) = mpsc::channel();
    let fleet = Fleet::start_accounted(
        fake.path(),
        tx,
        timing(),
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    );
    let mut seen = Seen::new(rx);
    seen.until("peer live", |c| {
        status_of(c, &ComputerId::from("peer")) == Some(ComputerStatus::Live)
    });
    eventually("descendant holds stderr", || {
        fake.dir.join("ready").exists()
    });
    fake.touch("remove-peers");
    eventually("peer removed from registry", || {
        fleet.peers().is_some_and(|p| p.is_empty())
    });
    seen.until("removed peer stop callback", |c| {
        status_of(c, &ComputerId::from("peer")) == Some(ComputerStatus::Ended)
    });
    let closed = gate.close();
    let began = Instant::now();
    fleet.stop();
    assert!(began.elapsed() < Duration::from_secs(3));
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    fake.touch("release");
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(5)),
        Observation::Idle
    );
}
