use super::*;
use crate::actions::{dispatch_with, dispatch_with_plan, Action, Batch, Outcome, Target};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use zerus_client_core::{readmarks::ReadMark, ComputerId, Exit, RowOut, ViewRow};

struct Fixture {
    root: PathBuf,
    actions: Actions,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!("read-barrier-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let actions = Actions::new(root.join("marks.json"));
        Self { root, actions }
    }
    fn path(&self) -> PathBuf {
        self.root.join("marks.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.root.join("release"), []);
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
fn mark() -> ReadMark {
    ReadMark {
        computer: "peer".into(),
        name: "claude/example/one".into(),
        run_id: "run-1".into(),
        conversation_id: "conv-1".into(),
        reply_id: Some("reply-1".into()),
        attention_signature: None,
    }
}
fn target() -> Target {
    let m = mark();
    Target {
        computer: m.computer,
        name: m.name,
        run_id: m.run_id,
        conversation_id: m.conversation_id,
    }
}
fn action() -> Action {
    Action::MarkRead {
        reply_id: mark().reply_id,
        attention_signature: None,
    }
}
fn settled() -> Batch {
    let m = mark();
    Batch {
        upserts: vec![RowOut {
            computer: ComputerId::from(m.computer.as_str()),
            key: "run:one".into(),
            row: ViewRow {
                name: m.name,
                run_id: Some(m.run_id),
                conversation_id: Some(m.conversation_id),
                reply_id: m.reply_id.clone(),
                read_reply_id: m.reply_id,
                ..ViewRow::default()
            },
        }],
        ..Batch::default()
    }
}
fn failed() -> Exit {
    Exit::Code {
        code: 1,
        stdout: vec![],
        stderr: String::new(),
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn freeze_refuses_every_store_entry_and_direct_effect_without_changing_checkpoint() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let frozen = f.actions.freeze_reads().unwrap();
    assert!(!f.path().exists(), "freeze itself does no file I/O");
    assert!(f.actions.queue_marks(vec![mark()]).is_err());
    f.actions.observe(&settled());
    f.actions
        .drop_orphans(|| panic!("closed admission must not inspect peers"));
    f.actions.flush_marks(); // Includes the updater/exit-flush entry point.
    assert!(!f.path().exists());
    assert!(tauri::async_runtime::block_on(dispatch_with(
        &f.actions,
        target(),
        action(),
        "same".into(),
        |_, _| panic!("closed read spawned")
    ))
    .is_err());
    let saved = frozen.checkpoint(deadline()).unwrap();
    assert_eq!(saved.marks, 1);
    let bytes = std::fs::read(f.path()).unwrap();
    assert_eq!(ReadMarks::load(f.path()).pending(), [mark()]);
    assert_eq!(sha256(&bytes), saved.sha256);
    f.actions.observe(&settled());
    f.actions.flush_marks();
    assert_eq!(std::fs::read(f.path()).unwrap(), bytes);
    assert!(matches!(f.actions.freeze_reads(), Err(Refused::Closed)));
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        0
    );
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn store_pressure_and_retained_admission_never_become_idle_on_a_timeout() {
    let f = Fixture::new();
    let held: Vec<_> = (0..64).map(|_| f.actions.admit_marks().unwrap()).collect();
    assert!(f.actions.queue_marks(vec![mark()]).is_err());
    let retained = held[0].clone();
    let frozen = f.actions.freeze_reads().unwrap();
    drop(held);
    assert_eq!(
        frozen.mutations.wait_until(Instant::now()),
        Observation::Pending
    );
    assert_eq!(frozen.checkpoint(Instant::now()), Err(Refused::Pending));
    assert!(!f.path().exists());
    drop(retained);
    assert!(
        f.actions.queue_marks(vec![mark()]).is_err(),
        "timeout never reopens"
    );
}

#[test]
fn an_admitted_orphan_callback_finishes_before_the_checkpoint() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let a = f.actions.clone();
    let writer = std::thread::spawn(move || {
        a.drop_orphans(|| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Some(Default::default())
        })
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let frozen = f.actions.freeze_reads().unwrap();
    assert_eq!(
        frozen.mutations.wait_until(Instant::now()),
        Observation::Pending
    );
    let done = std::thread::spawn(move || frozen.checkpoint(deadline()));
    release.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(done.join().unwrap().unwrap().marks, 0);
}

#[test]
fn queued_change_listener_is_included_in_the_mutation_lifetime() {
    let f = Fixture::new();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let released = Mutex::new(released);
    f.actions.listener.set(Box::new(move || {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
    }));
    let a = f.actions.clone();
    let writer = std::thread::spawn(move || a.queue_marks(vec![mark()]));
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let frozen = f.actions.freeze_reads().unwrap();
    assert_eq!(
        frozen.mutations.wait_until(Instant::now()),
        Observation::Pending
    );
    release.send(()).unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(frozen.checkpoint(deadline()).unwrap().marks, 1);
}

#[test]
fn direct_reads_use_the_same_four_slots_before_persisting_any_intent() {
    let f = Fixture::new();
    let calls: Vec<_> = (0..4)
        .map(|_| f.actions.read_delivery.admit().unwrap())
        .collect();
    assert!(tauri::async_runtime::block_on(dispatch_with(
        &f.actions,
        target(),
        action(),
        "same".into(),
        |_, _| panic!("fifth native read")
    ))
    .is_err());
    assert!(f.actions.pending_marks().is_empty());
    assert!(!f.path().exists());
    drop(calls);
    let out = tauri::async_runtime::block_on(dispatch_with(
        &f.actions,
        target(),
        action(),
        "same".into(),
        |_, inv| {
            assert_eq!(inv.args, ["@peer", "read", "claude/example/one", "--json"]);
            assert_eq!(inv.timeout, Duration::from_secs(35));
            let payload: serde_json::Value =
                serde_json::from_slice(inv.stdin.as_ref().unwrap()).unwrap();
            assert_eq!(
                payload,
                serde_json::json!({"run_id":"run-1","conversation_id":"conv-1","reply_id":"reply-1","attention_signature":null})
            );
            failed()
        },
    ));
    assert!(matches!(out, Ok(Outcome::Failed { .. })));
    assert_eq!(
        f.actions
            .freeze_reads()
            .unwrap()
            .checkpoint(deadline())
            .unwrap()
            .marks,
        1
    );
}

#[test]
fn direct_read_callback_keeps_the_native_charge_until_result_handling_ends() {
    let f = Fixture::new();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let a = f.actions.clone();
    let job = std::thread::spawn(move || {
        tauri::async_runtime::block_on(dispatch_with(
            &a,
            target(),
            action(),
            "same".into(),
            move |_, _| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                failed()
            },
        ))
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let frozen = f.actions.freeze_reads().unwrap();
    assert_eq!(
        frozen.native.wait_until(Instant::now()),
        Observation::Pending
    );
    release.send(()).unwrap();
    job.join().unwrap().unwrap();
    assert_eq!(frozen.checkpoint(deadline()).unwrap().marks, 1);
}

#[test]
fn poisoned_store_and_unreliable_initial_load_refuse_the_barrier() {
    let f = Fixture::new();
    let a = f.actions.clone();
    assert!(std::thread::spawn(move || {
        let _guard = a.marks.lock().unwrap();
        panic!("synthetic poison");
    })
    .join()
    .is_err());
    assert_eq!(
        f.actions.freeze_reads().unwrap().checkpoint(deadline()),
        Err(Refused::Unavailable)
    );
    let bad = f.root.join("bad.json");
    std::fs::write(&bad, b"{bad").unwrap();
    let actions = Actions::new(bad.clone());
    assert_eq!(
        actions.freeze_reads().unwrap().checkpoint(deadline()),
        Err(Refused::Checkpoint)
    );
    assert_eq!(std::fs::read(bad).unwrap(), b"{bad");
}

#[test]
fn original_frozen_capture_retains_its_charge_and_refuses_source_drift() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let captured = std::sync::Arc::new(
        Arc::new(f.actions.freeze_reads().unwrap())
            .capture(deadline())
            .unwrap(),
    );
    assert_eq!(captured.capture.source().rows, 1);
    assert!(captured.unchanged(deadline()).is_ok());
    let closed = f.actions.checkpoint_admission.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    // The original managed source remains intact until an explicit test mutation.
    std::fs::write(f.path(), b"[]").unwrap();
    assert_eq!(captured.unchanged(deadline()), Err(Refused::Checkpoint));
    drop(captured);
    assert_eq!(closed.wait_until(deadline()), Observation::Idle);
}

#[test]
fn an_invalid_last_managed_mark_never_exposes_an_import_capture() {
    let mut f = Fixture::new();
    let mut rows: Vec<_> = (0..193)
        .map(|n| {
            let mut m = mark();
            m.name = format!("agent/example/{n}");
            serde_json::json!(m)
        })
        .collect();
    rows[192]["conversation_id"] = serde_json::json!("");
    let bytes = serde_json::to_vec(&rows).unwrap();
    zerus_client_core::fsx::write_private_atomic(&f.path(), &bytes, true).unwrap();
    f.actions = Actions::new(f.path());
    assert!(matches!(
        Arc::new(f.actions.freeze_reads().unwrap()).capture(deadline()),
        Err(Refused::Checkpoint)
    ));
    assert_eq!(std::fs::read(f.path()).unwrap(), bytes);
}

#[test]
fn checkpoint_io_keeps_its_charge_and_a_late_success_is_refused() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let frozen = f.actions.freeze_reads().unwrap();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let end = Instant::now() + Duration::from_secs(1);
    let job = std::thread::spawn(move || {
        frozen.checkpoint_with(end, |marks| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            marks.checkpoint()
        })
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    assert!(f.actions.marks.try_lock().is_err());
    assert!(f.actions.queue_marks(vec![mark()]).is_err());
    std::thread::sleep(end.saturating_duration_since(Instant::now()) + Duration::from_millis(1));
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    release.send(()).unwrap();
    assert_eq!(job.join().unwrap(), Err(Refused::Pending));
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        0
    );
    assert!(
        f.path().exists(),
        "late completed I/O is not a successful barrier"
    );
}

#[test]
fn a_unix_error_number_is_never_retried_as_windows_sharing_contention() {
    let f = Fixture::new();
    let frozen = f.actions.freeze_reads().unwrap();
    let mut calls = 0;
    assert!(matches!(
        frozen.checkpoint_with(deadline(), |_| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(32))
        }),
        Err(Refused::Checkpoint)
    ));
    assert_eq!(calls, 1);
    assert!(!f.path().exists());
}

#[test]
fn direct_native_output_holders_pin_the_barrier_until_actual_eof() {
    let _process_tree = crate::PROCESS_TREE.read().unwrap();
    use std::os::unix::fs::PermissionsExt;
    for pipe in ["stdout", "stderr"] {
        let f = Fixture::new();
        let fake = f.root.join("hgs");
        let release = f.root.join("release");
        let body = format!("import os,time\nend=time.monotonic()+15\nwhile not os.path.exists({}) and time.monotonic()<end: time.sleep(.01)\n", serde_json::to_string(release.to_str().unwrap()).unwrap());
        let program = format!("#!/usr/bin/env python3\nimport subprocess,sys\nsubprocess.Popen([sys.executable,'-c',{}],stdin=subprocess.DEVNULL,stdout={},stderr={})\nprint('{{\"ok\":true,\"applied\":{{\"reply_id\":true,\"attention_signature\":false}}}}')\n", serde_json::to_string(&body).unwrap(), if pipe=="stdout" {"sys.stdout"} else {"subprocess.DEVNULL"}, if pipe=="stderr" {"sys.stderr"} else {"subprocess.DEVNULL"});
        std::fs::write(&fake, program).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let out = tauri::async_runtime::block_on(dispatch_with_plan(
            &f.actions,
            target(),
            action(),
            "original".into(),
            move |_, inv, read| {
                read.expect("shared accounted native runner")
                    .run(&fake, inv)
            },
            None,
        ));
        assert!(matches!(out, Ok(Outcome::Done { .. })), "{pipe}: {out:?}");
        let frozen = f.actions.freeze_reads().unwrap();
        assert_eq!(
            frozen.native.wait_until(Instant::now()),
            Observation::Pending,
            "{pipe}"
        );
        std::fs::write(release, []).unwrap();
        assert_eq!(frozen.checkpoint(deadline()).unwrap().marks, 1);
    }
}

#[test]
fn snapshot_store_contention_waits_without_losing_the_original_freeze() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    let a = f.actions.clone();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let _guard = a.marks.lock().unwrap();
        entered.send(()).unwrap();
        released.recv().unwrap();
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let original = frozen.clone();
    let (finished, result) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || finished.send(original.capture(deadline())).unwrap());
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(25)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    reader.join().unwrap();
    let captured = result
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert_eq!(captured.capture.source().rows, 1);
    assert!(captured.unchanged(deadline()).is_ok());
    assert!(
        matches!(f.actions.freeze_reads(), Err(Refused::Closed)),
        "the original barrier was not reopened"
    );
}

#[test]
fn a_capture_deadline_keeps_the_same_frozen_source_available_for_retry() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let frozen = Arc::new(f.actions.freeze_reads().unwrap());
    let guard = f.actions.marks.lock().unwrap();
    assert!(matches!(
        frozen.capture(Instant::now() + Duration::from_millis(25)),
        Err(Refused::Pending)
    ));
    assert!(
        !f.path().exists(),
        "the refused capture did not checkpoint any mutation"
    );
    assert_eq!(
        f.actions.checkpoint_admission.snapshot().unwrap().admitted,
        1
    );
    drop(guard);
    let captured = frozen.capture(deadline()).unwrap();
    assert_eq!(captured.capture.source().rows, 1);
    assert!(f.actions.queue_marks(vec![mark()]).is_err());
}

#[test]
fn source_verification_waits_for_readers_and_can_retry_its_original_capture() {
    let f = Fixture::new();
    f.actions.queue_marks(vec![mark()]).unwrap();
    let captured = Arc::new(
        Arc::new(f.actions.freeze_reads().unwrap())
            .capture(deadline())
            .unwrap(),
    );
    let guard = f.actions.marks.lock().unwrap();
    assert_eq!(
        captured.unchanged(Instant::now() + Duration::from_millis(25)),
        Err(Refused::Pending)
    );
    let original = captured.clone();
    let (finished, result) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || finished.send(original.unchanged(deadline())).unwrap());
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(25)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(guard);
    assert_eq!(result.recv_timeout(Duration::from_secs(3)).unwrap(), Ok(()));
    worker.join().unwrap();
    assert_eq!(ReadMarks::load(f.path()).pending(), [mark()]);
}
