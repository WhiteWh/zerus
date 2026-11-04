use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn eventually(label: &str, test: impl Fn() -> bool) {
    let until = deadline();
    while !test() {
        assert!(Instant::now() < until, "{label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn empty_freeze_is_one_use_and_every_control_remains_closed() {
    let live = Arc::new(Live::default());
    live.freeze().unwrap().drain_until(deadline()).unwrap();
    assert!(matches!(live.freeze(), Err(Refused::Closed)));
    assert!(live
        .start_from(|| PathBuf::from("unavailable-fixture"), None)
        .is_err());
    assert!(!live.reconnect("peer"));
    assert!(live.follow_tasks(None).is_err());
    assert!(live.admit().is_err());
    assert_eq!(live.resources.snapshot().unwrap().admitted, 0);
}

#[test]
fn admitted_control_finishes_before_drain_and_timeout_never_reopens() {
    let live = Arc::new(Live::default());
    let held = live.admit().unwrap();
    let frozen = live.freeze().unwrap();
    assert_eq!(frozen.drain_until(Instant::now()), Err(Refused::Pending));
    drop(held);
    assert!(live.admit().is_err());
    assert!(matches!(live.freeze(), Err(Refused::Closed)));
}

#[test]
fn admitted_start_waiting_for_source_cannot_create_a_late_graph() {
    let live = Arc::new(Live::default());
    let source = live.source.lock().unwrap();
    let worker = live.clone();
    let started = std::thread::spawn(move || {
        worker.start_from(|| PathBuf::from("unavailable-fixture"), None)
    });
    eventually("start admission", || {
        live.controls.snapshot().unwrap().admitted == 1
    });
    let frozen = live.freeze().unwrap();
    drop(source);
    assert!(started.join().unwrap().is_err());
    frozen.drain_until(deadline()).unwrap();
    assert!(live.source.lock().unwrap().is_none());
}

#[test]
fn poisoned_source_never_becomes_a_successful_empty_drain() {
    let live = Arc::new(Live::default());
    let broken = live.clone();
    assert!(std::thread::spawn(move || {
        let _held = broken.source.lock().unwrap();
        panic!("synthetic source failure");
    })
    .join()
    .is_err());
    assert_eq!(
        live.freeze().unwrap().drain_until(deadline()),
        Err(Refused::Unavailable)
    );
}

#[test]
fn busy_source_is_refused_without_waiting_or_reopening() {
    let live = Arc::new(Live::default());
    let frozen = live.freeze().unwrap();
    let _held = live.source.lock().unwrap();
    assert_eq!(frozen.drain_until(deadline()), Err(Refused::Unavailable));
    assert!(live.admit().is_err());
}

#[test]
fn callback_completion_outlives_an_abandoned_controller() {
    let live = Arc::new(Live::default());
    let (tx, rx) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    live.pump.drain_accounted(
        rx,
        move |_| {
            entered.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
            false
        },
        || {},
        Arc::new(live.resources.admit(Role::Fleet).unwrap()),
    );
    tx.send(zerus_client_core::Change::Computer {
        computer: zerus_client_core::ComputerId::local(),
        status: zerus_client_core::ComputerStatus::Live,
    })
    .unwrap();
    drop(tx);
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    let frozen = live.freeze().unwrap();
    let resources = frozen.resources.clone();
    assert_eq!(
        frozen.drain_until(Instant::now() + Duration::from_millis(25)),
        Err(Refused::Pending)
    );
    assert_eq!(resources.wait_until(Instant::now()), Observation::Pending);
    release.send(()).unwrap();
    assert_eq!(resources.wait_until(deadline()), Observation::Idle);
    assert!(live.admit().is_err());
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!("fleet-barrier-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../core/tests/support/fake_hgs_accounted.py"),
            root.join("hgs"),
        )
        .unwrap();
        let hello = serde_json::json!({"t":"hello","host":"fixture","hgs_version":"1.56.0","stream":"fixture","heartbeat_ms":10000,"max_frame_bytes":1048576,"topics":["catalog","catalog.meta","activity"],"limits":{"activity_subscriptions":16,"page_bytes":524288,"queue_bytes":4194304,"activity_tail":100,"activity_read":200,"control_line_bytes":65536}});
        fs::write(root.join("accounted.json"),serde_json::json!({"phase":"watch","pipe":"stderr","marker":zerus_watch_protocol::HELP_MARKER,"frames":[hello,{"t":"snapshot","topic":"catalog","items":[],"page":0,"last":true,"cursor":1}]}).to_string()).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("release"), []);
    }
}

#[test]
fn live_gui_source_requires_actual_helper_pipe_and_pump_completion() {
    let f = Fixture::new();
    let live = Arc::new(Live::default());
    assert_eq!(live.start_from(|| f.0.join("hgs"), None).unwrap(), "live");
    eventually("watch descendant owns pipe", || f.0.join("ready").exists());
    let frozen = live.freeze().unwrap();
    let resources = frozen.resources.clone();
    assert_eq!(
        frozen.drain_until(Instant::now() + Duration::from_millis(50)),
        Err(Refused::Pending)
    );
    assert_eq!(resources.wait_until(Instant::now()), Observation::Pending);
    fs::write(f.0.join("release"), []).unwrap();
    assert_eq!(resources.wait_until(deadline()), Observation::Idle);
    assert!(live.start_from(|| f.0.join("hgs"), None).is_err());
}

#[test]
fn replay_never_supplies_live_fleet_proof() {
    let f = Fixture::new();
    fs::write(
        f.0.join("replay.ndjson"),
        include_bytes!("../../../../../core/tests/fixtures/basic.ndjson"),
    )
    .unwrap();
    let live = Arc::new(Live::default());
    assert_eq!(
        live.start_from(|| f.0.join("hgs"), Some(f.0.join("replay.ndjson")))
            .unwrap(),
        "replay"
    );
    assert_eq!(
        live.freeze().unwrap().drain_until(deadline()),
        Err(Refused::Replay)
    );
}
