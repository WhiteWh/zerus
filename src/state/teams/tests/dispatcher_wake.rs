//! Task #17 of team c536f0e5 (P2.6a, owner 2026-11-23): the team worker
//! waits on changes, at most 3 s between wakes. A commit to the team store by
//! another connection wakes it within 100 ms; its own commit does not.
use super::super::wake::{self, Counts, Event, Waker};
use super::*;

#[test]
fn a_delivery_or_a_queued_notice_keeps_the_worker_ticking() {
    let mut team = Team { id: "t".into(), status: TeamStatus::Running, ..Default::default() };
    team.tasks.push(Task { id: 1, assignee: "dev".into(), ..Default::default() });
    assert_eq!(unsettled(&team), None);
    team.tasks[0].attempts.push(Attempt { n: 1, state: AttemptState::Delivered, ..Default::default() });
    assert_eq!(unsettled(&team), None, "a delivered hand-out waits for the member, not the clock");
    team.tasks[0].attempts[0].state = AttemptState::Delivering;
    assert_eq!(unsettled(&team), Some("delivering"));
    team.tasks[0].attempts[0].state = AttemptState::Delivered;
    team.tasks[0].attempts[0].notice = Notice::Pending;
    assert_eq!(unsettled(&team), Some("notice"));
    team.tasks[0].attempts[0].notice = Notice::Sent;
    team.brief = Notice::Sending;
    assert_eq!(unsettled(&team), Some("notice"));
}

#[test]
fn a_skipped_phase_asks_for_the_next_wake() {
    let _ = take_again();
    again("deferred");
    again("other");
    assert_eq!(take_again(), Some("deferred"), "the first reason stays");
    assert_eq!(take_again(), None);
}

/// A team store in `root` as the worker's connection holds it.
fn store_at(root: &Path) -> (PathBuf, rusqlite::Connection) {
    let dir = root.join("teams");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("teams.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS t(v INTEGER);").unwrap();
    (path, db)
}

/// The acceptance of task #17, measured: twelve commits by another
/// connection (every third after a checkpoint, so the WAL is reused at the
/// same size) each wake the worker within 100 ms; its own commit leaves it
/// waiting to the deadline. The latencies are written beside the test
/// binary (`wake-latency.json`).
#[cfg(windows)]
#[test]
fn a_commit_by_another_connection_wakes_the_worker_within_100_ms_and_its_own_does_not() {
    let root = tempfile::tempdir().unwrap();
    let (path, own) = store_at(root.path());
    let mut other = rusqlite::Connection::open(&path).unwrap();
    let mut waker = Waker::start(root.path(), None);
    assert!(waker.listening(), "{:?}", waker.fault);
    let mut counts = Counts::new(0.0);

    // Its own commit: notified, explained by no other connection, waited past.
    let version = wake::data_version(&own);
    own.execute("INSERT INTO t VALUES(1)", []).unwrap();
    let started = Instant::now();
    let events = wait_for_change(&mut waker, &own, &mut counts, version, false);
    let own_wait = started.elapsed();
    assert!(events.is_empty(), "{events:?}");
    assert!(own_wait >= wake::WAKE - Duration::from_millis(20), "{own_wait:?}");
    let own_counts = counts.value(true);
    assert_eq!(own_counts["wakes"]["timer"], 1, "{own_counts}");

    let mut latencies = Vec::new();
    for sample in 0..12u32 {
        if sample % 3 == 2 {
            other.execute_batch("PRAGMA wal_checkpoint(PASSIVE);").unwrap();
        }
        let version = wake::data_version(&own);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            other.execute("INSERT INTO t VALUES(?1)", [sample]).unwrap();
            let committed = Instant::now();
            (other, committed)
        });
        let events = wait_for_change(&mut waker, &own, &mut counts, version, false);
        let woke = Instant::now();
        let (back, committed) = writer.join().unwrap();
        other = back;
        assert_eq!(events, BTreeSet::from([Event::Store]), "sample {sample}");
        latencies.push(woke.saturating_duration_since(committed));
    }
    let ms: Vec<f64> = latencies.iter().map(|took| took.as_secs_f64() * 1000.0).collect();
    let mut sorted = ms.clone();
    sorted.sort_by(f64::total_cmp);
    let measured = json!({"own_commit_wait_ms":own_wait.as_millis(),"own_commit_counts":own_counts,
        "other_commit_ms":ms,"median_ms":(sorted[5] + sorted[6]) / 2.0,"max_ms":sorted[11],"counts":counts.value(true)});
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        let _ = fs::write(dir.join("wake-latency.json"), measured.to_string());
    }
    assert!(sorted[11] < 100.0, "{measured}");
}

/// Without notifications (or with the bus switched off) the waker reports
/// it, so the worker ticks on every wake as before.
#[test]
fn a_waker_without_a_folder_ticks_every_wake() {
    let missing = tempfile::tempdir().unwrap().path().join("gone");
    let waker = Waker::start(&missing, None);
    assert!(!waker.listening());
    assert!(waker.fault.is_some());
}
