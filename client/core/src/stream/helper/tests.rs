use super::*;
use crate::{
    desktop::drain::{Gate, Observation, Role},
    stream::{probe_until, resources::Scope, Probe},
    ComputerId,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new(phase: &str, exit_first: bool) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "stream-group-{}-{stamp}-{next}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake_hgs_group.py"),
            root.join("hgs"),
        )
        .unwrap();
        fs::write(
            root.join("group.json"),
            json!({"phase":phase,"exit_first":exit_first,"frames":[
                {"t":"hello","host":"fixture","hgs_version":"1.56.0","stream":"group-test","heartbeat_ms":10000,"max_frame_bytes":1048576,"topics":["catalog"],"limits":{"activity_subscriptions":16,"page_bytes":524288,"queue_bytes":4194304,"activity_tail":100,"activity_read":200,"control_line_bytes":65536}},
                {"t":"snapshot","topic":"catalog","items":[],"page":0,"last":true,"cursor":1}
            ]}).to_string(),
        )
        .unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("hgs")
    }
    fn group_gone(&self) {
        let p: serde_json::Value =
            serde_json::from_slice(&fs::read(self.0.join("pids.json")).unwrap()).unwrap();
        let group = p["group"].as_i64().unwrap() as libc::pid_t;
        assert_eq!(p["leader"].as_i64().unwrap(), i64::from(group));
        assert_ne!(
            group,
            unsafe { libc::getpgrp() },
            "never signal the test process group"
        );
        let end = Instant::now() + Duration::from_secs(6);
        loop {
            if unsafe { libc::killpg(group, 0) } != 0
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            assert!(Instant::now() < end, "owned helper group still exists");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("release"), []);
    }
}

#[test]
fn timed_out_peer_probe_kills_its_pipe_holder_and_reuses_primary_within_one_poll() {
    let f = Fixture::new("probe", false);
    let gate = Gate::new(1).unwrap();
    let scope = Scope::new(Arc::new(gate.admit(Role::Fleet).unwrap()));
    let stop = AtomicBool::new(false);
    assert!(matches!(
        probe_until(
            &f.path(),
            &ComputerId::from("peer"),
            Duration::from_secs(2),
            &stop,
            None,
            Some(&scope)
        ),
        Probe::Failed(_)
    ));
    f.group_gone();
    // One supervisor poll gives actual EOF readers their scheduling turn.
    thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    assert_eq!(
        probe_until(
            &f.path(),
            &ComputerId::from("peer"),
            Duration::from_secs(2),
            &stop,
            None,
            Some(&scope)
        ),
        Probe::Supported
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    drop(scope);
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(1)),
        Observation::Idle
    );
}

#[test]
fn exited_leader_is_unreaped_until_its_pipe_holding_group_is_stopped() {
    let f = Fixture::new("probe", true);
    assert_eq!(
        crate::stream::probe(&f.path(), &ComputerId::from("peer"), Duration::from_secs(2)),
        Probe::Supported
    );
    f.group_gone();
}

#[test]
fn watch_stop_kills_only_its_group_and_next_probe_needs_no_orphan_timeout() {
    use crate::{Change, ComputerStatus, Timing};
    let f = Fixture::new("watch", false);
    let gate = Gate::new(1).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = crate::stream::spawn_accounted(
        &f.path(),
        &ComputerId::from("peer"),
        Timing {
            silence: Duration::from_secs(2),
            stop_grace: Duration::from_millis(25),
            restart: [Duration::from_secs(30); 4],
            ..Timing::default()
        },
        tx,
        Arc::new(gate.admit(Role::Fleet).unwrap()),
    )
    .unwrap();
    let status = |wanted| {
        let end = Instant::now() + Duration::from_secs(6);
        loop {
            let event = rx
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap();
            if matches!(event,crate::stream::StreamEvent::Change(Change::Computer{status,..}) if status==wanted)
            {
                break;
            }
        }
    };
    status(ComputerStatus::Live);
    // Silence ends the real watch, and close() stops its still-live group.
    status(ComputerStatus::Reconnecting { attempt: 1 });
    f.group_gone();
    thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    assert!(handle.reconnect());
    status(ComputerStatus::Live);
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "Retry now must not wait for an orphan timeout"
    );
    let calls = fs::read_to_string(f.0.join("calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(
                |line| serde_json::from_str::<serde_json::Value>(line).unwrap()["args"]
                    == json!(["-h"])
            )
            .count(),
        2
    );
    handle.stop();
    assert_eq!(
        gate.close()
            .wait_until(Instant::now() + Duration::from_secs(2)),
        Observation::Idle
    );
}
