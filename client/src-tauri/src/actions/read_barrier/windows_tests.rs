//! Native Windows fixtures: original handles, private managed files only.
use super::*;
use std::{
    fs::OpenOptions, os::windows::fs::OpenOptionsExt, path::PathBuf, sync::mpsc, time::Duration,
};
use zerus_client_core::readmarks::ReadMark;
struct Fixture {
    root: PathBuf,
    actions: Actions,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("read-barrier-win-{}", uuid::Uuid::new_v4()));
        let actions = Actions::new(root.join("marks.json"));
        Self { root, actions }
    }
    fn path(&self) -> PathBuf {
        self.root.join("marks.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn mark(n: usize) -> ReadMark {
    ReadMark {
        computer: String::new(),
        name: format!("agent/example/{n}"),
        run_id: format!("run-{n}"),
        conversation_id: format!("conversation-{n}"),
        reply_id: Some(format!("reply-{n}")),
        attention_signature: None,
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn native_windows_freeze_closes_admissions_before_io_and_captures_every_mark() {
    let f = Fixture::new();
    f.actions.queue_marks((0..193).map(mark).collect()).unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    assert!(!f.path().exists());
    assert!(f.actions.queue_marks(vec![mark(500)]).is_err());
    let captured = frozen.capture(deadline()).unwrap();
    assert_eq!(captured.capture.source().rows, 193);
    let mut count = 0;
    let mut after = 0;
    while after < 193 {
        let page = captured.capture.page(after).unwrap();
        count += page.marks.len();
        after = page.through();
    }
    assert_eq!(count, 193);
    assert!(captured.unchanged(deadline()).is_ok());
    assert!(
        OpenOptions::new().write(true).open(f.path()).is_err(),
        "capture keeps the original exclusive leaf"
    );
    assert!(matches!(f.actions.freeze_reads(), Err(Refused::Closed)));
    let hash = captured.capture.source().sha256.clone();
    drop(captured);
    drop(frozen);
    let data = std::fs::read(f.path()).unwrap();
    assert_eq!(
        zerus_client_core::desktop::pending_reads::Capture::from_bytes(&data)
            .unwrap()
            .source()
            .sha256,
        hash
    );
}

#[test]
fn native_windows_busy_file_can_retry_the_same_freeze_without_reopening_writers() {
    let mut f = Fixture::new();
    let mut initial = ReadMarks::load(f.path());
    initial.add_all([mark(0)]);
    drop(initial.checkpoint().unwrap());
    f.actions = Actions::new(f.path());
    let original = std::fs::read(f.path()).unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(7)
        .open(f.path())
        .unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    assert!(matches!(
        frozen.capture(deadline()),
        Err(Refused::Checkpoint)
    ));
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    assert!(f.actions.queue_marks(vec![mark(1)]).is_err());
    drop(reader);
    let captured = frozen.capture(deadline()).unwrap();
    assert_eq!(captured.capture.source().rows, 1);
    drop(captured);
    drop(frozen);
    assert_eq!(std::fs::read(f.path()).unwrap(), original);
}

#[test]
fn native_windows_read_only_store_borrows_wait_under_the_original_deadline() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark(0)]).unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    let guard = f.actions.marks.lock().unwrap();
    assert!(matches!(
        frozen.capture(Instant::now() + Duration::from_millis(25)),
        Err(Refused::Pending)
    ));
    assert!(!f.path().exists());
    let same = frozen.clone();
    let (sent, received) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || sent.send(same.capture(deadline())).unwrap());
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(25)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(guard);
    let captured = received
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert!(captured.unchanged(deadline()).is_ok());
}

#[test]
fn native_windows_invalid_last_managed_mark_never_exposes_a_capture_or_clears_source() {
    let mut f = Fixture::new();
    let mut initial = ReadMarks::load(f.path());
    drop(initial.checkpoint().unwrap());
    let mut rows: Vec<_> = (0..193).map(|n| serde_json::json!(mark(n))).collect();
    rows[192]["conversation_id"] = serde_json::json!("");
    let data = serde_json::to_vec(&rows).unwrap();
    std::fs::write(f.path(), &data).unwrap();
    f.actions = Actions::new(f.path());
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    assert!(matches!(
        frozen.capture(deadline()),
        Err(Refused::Checkpoint)
    ));
    assert_eq!(std::fs::read(f.path()).unwrap(), data);
    assert!(f.actions.queue_marks(vec![mark(500)]).is_err());
}

#[test]
fn native_windows_capture_pins_survive_until_the_last_original_resource_owner_drops() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark(0)]).unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    let captured = Arc::new(frozen.capture(deadline()).unwrap());
    let resources: Arc<dyn Send + Sync> = Arc::new(captured.clone());
    drop(frozen);
    drop(captured);
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    assert!(OpenOptions::new().read(true).open(f.path()).is_err());
    drop(resources);
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        0
    );
    assert!(!std::fs::read(f.path()).unwrap().is_empty());
}

#[test]
fn native_windows_brief_sharing_contention_retries_the_same_frozen_source() {
    let mut f = Fixture::new();
    let mut initial = ReadMarks::load(f.path());
    initial.add_all([mark(0)]);
    drop(initial.checkpoint().unwrap());
    f.actions = Actions::new(f.path());
    let original = std::fs::read(f.path()).unwrap();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(f.path())
        .unwrap();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        drop(reader);
    });
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    let captured = frozen.capture(deadline());
    worker.join().unwrap();
    let captured = captured.unwrap();
    assert_eq!(captured.capture.source().rows, 1);
    assert!(f.actions.queue_marks(vec![mark(1)]).is_err());
    drop(captured);
    drop(frozen);
    assert_eq!(std::fs::read(f.path()).unwrap(), original);
}

#[test]
fn native_windows_sharing_wait_obeys_the_original_deadline_and_retains_retry() {
    let mut f = Fixture::new();
    let mut initial = ReadMarks::load(f.path());
    drop(initial.checkpoint().unwrap());
    f.actions = Actions::new(f.path());
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(f.path())
        .unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    assert!(matches!(
        frozen.capture(Instant::now() + Duration::from_millis(25)),
        Err(Refused::Pending)
    ));
    assert!(f.actions.queue_marks(vec![mark(0)]).is_err());
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    drop(reader);
    assert_eq!(frozen.capture(deadline()).unwrap().capture.source().rows, 0);
}

#[test]
fn sharing_retry_count_is_finite_and_other_errors_are_not_retried() {
    for (code, expected) in [(32, 21), (5, 1)] {
        let f = Fixture::new();
        let frozen = f.actions.freeze_reads().unwrap();
        let mut calls = 0;
        assert!(matches!(
            frozen.checkpoint_inner(deadline(), |_| {
                calls += 1;
                Err(std::io::Error::from_raw_os_error(code))
            }),
            Err(Refused::Checkpoint)
        ));
        assert_eq!(calls, expected);
        assert!(!f.path().exists());
    }
}
