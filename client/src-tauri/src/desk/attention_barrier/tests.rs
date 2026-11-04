use super::*;
use crate::desk::ATTENTION_FILE;
#[cfg(unix)]
use crate::desk::{Context, Outcome, SWEEP};
#[cfg(unix)]
use std::thread;
use std::{path::PathBuf, time::Duration};
#[cfg(unix)]
use zerus_client_core::{Change, ComputerId, ComputerStatus, ViewRow};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}
#[cfg(unix)]
fn round(
    desk: &mut DeskLoop,
    inputs: impl IntoIterator<Item = DeskInput>,
    now: Instant,
) -> Outcome {
    desk.round(
        inputs,
        &mut Context {
            publish: &mut |_, _| {},
            now,
            unix: 2000,
            focused: false,
            optional_questions: &mut || false,
            marks: &mut || None,
            notice_id: &mut || "notice-example".into(),
        },
    )
}
#[cfg(unix)]
fn live(name: &str) -> Vec<DeskInput> {
    let mut row = ViewRow {
        name: name.into(),
        state: "running".into(),
        activity: "waiting".into(),
        phase: Some("approval".into()),
        run_id: Some(format!("run-{name}")),
        ..ViewRow::default()
    };
    row.notify.needs_action = true;
    row.notify.attention_id = "request-example".into();
    vec![
        DeskInput::Change(Box::new(Change::Replace {
            computer: ComputerId::local(),
            rows: [(name.into(), row)].into(),
        })),
        DeskInput::Change(Box::new(Change::Computer {
            computer: ComputerId::local(),
            status: ComputerStatus::Live,
        })),
    ]
}
#[cfg_attr(not(unix), allow(dead_code))]
struct Fixture {
    root: PathBuf,
    sender: InputSender,
    inputs: mpsc::Receiver<Message>,
    owner: Owner,
    desk: DeskLoop,
}
#[cfg_attr(not(unix), allow(dead_code))]
impl Fixture {
    fn new() -> Self {
        Self::with_source(None)
    }
    fn with_source(source: Option<&[u8]>) -> Self {
        let root = std::env::temp_dir().join(format!("desk-barrier-{}", uuid::Uuid::new_v4()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&root).unwrap();
        let (sender, inputs, owner) = channel(true);
        let path = root.join(ATTENTION_FILE);
        if let Some(bytes) = source {
            super::super::write_private(&path, bytes).unwrap();
        }
        let load = owner.track(Box::new(move || Attention::load(path, || None)));
        let desk = DeskLoop::new(
            super::super::OpenMode::Terminal,
            Some(load()),
            Instant::now(),
        );
        Self {
            root,
            sender,
            inputs,
            owner,
            desk,
        }
    }
    fn path(&self) -> PathBuf {
        self.root.join(ATTENTION_FILE)
    }
    #[cfg(unix)]
    fn command(&self) -> Command {
        match self.inputs.recv_timeout(Duration::from_secs(3)).unwrap() {
            Message::Barrier(command) => command,
            Message::Input(_) => panic!("expected ordered barrier"),
        }
    }
    #[cfg(unix)]
    fn stopped(&mut self) -> Stopped {
        let control = self.owner.control();
        let host = thread::spawn(move || control.stop(deadline()));
        self.command().run(&mut self.desk);
        host.join().unwrap().unwrap()
    }
    #[cfg(unix)]
    fn checkpoint(&mut self, stopped: Stopped) -> Result<Checkpoint, Refused> {
        let host = thread::spawn(move || stopped.checkpoint_after_native_drain(deadline()));
        self.command().run(&mut self.desk);
        host.join().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
#[test]
fn ordered_stop_keeps_prior_batch_and_ui_live_but_stops_timer_and_detector() {
    let mut f = Fixture::new();
    for input in live("example/first") {
        f.sender.send(input).unwrap();
    }
    let control = f.owner.control();
    let host = thread::spawn(move || control.stop(deadline()));
    // Wait for the stop to be queued without consuming the original inputs.
    let ready = deadline();
    while *f.owner.0.phase.lock().unwrap() == Phase::Running {
        assert!(Instant::now() < ready, "stop was not queued");
        thread::yield_now();
    }
    f.sender
        .send(DeskInput::Mode(super::super::OpenMode::Automatic))
        .unwrap();
    let first = f.inputs.recv().unwrap();
    let mut batch = Batch::new(Some(first), &f.inputs);
    let before = round(&mut f.desk, &mut batch, Instant::now());
    assert_eq!(before.changes.raised.len(), 1);
    assert_eq!(
        before.save_mode, None,
        "input after barrier is not in this batch"
    );
    let command = batch.barrier.take().unwrap();
    command.run(&mut f.desk); // Production does apply(before) before this.
    let stopped = host.join().unwrap().unwrap();
    let after = round(
        &mut f.desk,
        Batch::new(None, &f.inputs),
        Instant::now() + SWEEP * 2,
    );
    assert!(after.publish);
    assert_eq!(after.save_mode, Some(super::super::OpenMode::Automatic));
    assert!(after.changes.raised.is_empty());
    assert_eq!(
        f.desk.wait(Instant::now() + SWEEP * 2),
        SWEEP,
        "no expired sweep busy loop"
    );
    let source = std::fs::read(f.path()).unwrap();
    assert!(round(
        &mut f.desk,
        live("example/second"),
        Instant::now() + SWEEP * 3
    )
    .changes
    .raised
    .is_empty());
    assert_eq!(
        std::fs::read(f.path()).unwrap(),
        source,
        "UI changes cannot observe or save new edges"
    );
    assert_eq!(f.checkpoint(stopped).unwrap().sha256, sha256(&source));
    assert!(f.desk.attention.is_none());
    assert_eq!(f.owner.0.gate.snapshot().unwrap().admitted, 0);
}

#[cfg(unix)]
#[test]
fn native_failure_sent_before_finalizer_is_saved_before_exact_checkpoint() {
    let mut f = Fixture::new();
    let token = round(&mut f.desk, live("example/one"), Instant::now())
        .changes
        .raised
        .remove(0)
        .token;
    let stopped = f.stopped();
    let old = std::fs::read(f.path()).unwrap();
    f.sender
        .send(DeskInput::Failed {
            token: token.clone(),
            detail: "fixture failure".into(),
        })
        .unwrap();
    let host = thread::spawn(move || stopped.checkpoint_after_native_drain(deadline()));
    let first = f.inputs.recv().unwrap();
    let mut batch = Batch::new(Some(first), &f.inputs);
    // It is fine if the finalizer arrives after the ordinary batch ends.
    let result = round(&mut f.desk, &mut batch, Instant::now());
    assert_eq!(result.window_notices.len(), 1);
    assert_ne!(std::fs::read(f.path()).unwrap(), old);
    let command = batch.barrier.take().unwrap_or_else(|| f.command());
    command.run(&mut f.desk);
    let saved = host.join().unwrap().unwrap();
    let bytes = std::fs::read(f.path()).unwrap();
    assert_eq!(saved.sha256, sha256(&bytes));
    let source: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let saved_entry = source[""]
        .as_object()
        .unwrap()
        .values()
        .find(|entry| entry["token"] == token)
        .expect("original token retained");
    assert_eq!(saved_entry["retry_at"], 2060);
    let late = round(
        &mut f.desk,
        [DeskInput::Failed {
            token,
            detail: "late".into(),
        }],
        Instant::now(),
    );
    assert!(late.window_notices.is_empty());
    assert_eq!(std::fs::read(f.path()).unwrap(), bytes);
    let update_failure = round(
        &mut f.desk,
        [DeskInput::Failed {
            token: "zerus-update:v1:stable:123".into(),
            detail: "update fixture".into(),
        }],
        Instant::now(),
    );
    assert_eq!(
        update_failure.window_notices.len(),
        1,
        "GUI update notices are outside agent completion closure"
    );
    let click = round(
        &mut f.desk,
        [DeskInput::Clicked("late agent callback".into())],
        Instant::now(),
    );
    assert!(!click.show);
    let updates = round(
        &mut f.desk,
        [DeskInput::Updates {
            version: Some("1.2.3".into()),
            restart: false,
        }],
        Instant::now(),
    );
    assert!(
        updates.publish,
        "GUI updates stay live after agent Attention retirement"
    );
}

#[cfg(unix)]
#[test]
fn timed_out_checkpoint_keeps_actual_worker_and_frozen_value_charged() {
    let mut f = Fixture::new();
    let stopped = f.stopped();
    let host = thread::spawn(move || {
        stopped.checkpoint_after_native_drain(Instant::now() + Duration::from_millis(100))
    });
    let command = f.command();
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let mut desk = std::mem::replace(
        &mut f.desk,
        DeskLoop::new(super::super::OpenMode::Terminal, None, Instant::now()),
    );
    let worker = thread::spawn(move || {
        command.run_with(&mut desk, |attention| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            attention.checkpoint()
        });
        desk
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(host.join().unwrap(), Err(Refused::Pending));
    assert_eq!(
        f.owner.0.gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
    assert!(
        !f.path().exists(),
        "blocked original checkpoint has not written yet"
    );
    release.send(()).unwrap();
    f.desk = worker.join().unwrap();
    assert!(
        f.path().exists(),
        "actual I/O completed after the outer wait ended"
    );
    assert!(
        f.desk.attention.is_some(),
        "late sync cannot retire this owner"
    );
    assert!(*f.owner.0.phase.lock().unwrap() == Phase::Failed);
    assert_eq!(f.owner.0.gate.snapshot().unwrap().count(Role::Attention), 1);
    assert!(matches!(
        f.owner.control().stop(deadline()),
        Err(Refused::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn loading_and_an_abandoned_stop_never_release_or_reopen_the_original_owner() {
    let mut f = Fixture::new();
    // A fresh scope holds the charge before entering the original loader.
    let (_sender, inputs, owner) = channel(true);
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let path = f.path();
    let load = owner.track(Box::new(move || {
        entered.send(()).unwrap();
        released.recv().unwrap();
        Attention::load(path, || None)
    }));
    let worker = thread::spawn(load);
    waiting.recv().unwrap();
    assert_eq!(owner.0.gate.snapshot().unwrap().count(Role::Attention), 1);
    let control = owner.control();
    let host = thread::spawn(move || control.stop(Instant::now() + Duration::from_millis(20)));
    assert!(matches!(host.join().unwrap(), Err(Refused::Pending)));
    assert!(owner.0.gate.snapshot().unwrap().closed);
    release.send(()).unwrap();
    f.desk.attention = Some(worker.join().unwrap());
    let Message::Barrier(command) = inputs.recv().unwrap() else {
        panic!("stop")
    };
    command.run(&mut f.desk);
    assert!(!f.desk.observing_attention);
    assert!(matches!(
        owner.control().stop(deadline()),
        Err(Refused::Closed)
    ));
    assert_eq!(
        owner.0.gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[cfg(unix)]
#[test]
fn changed_or_unreliable_source_refuses_checkpoint_and_keeps_ui_live() {
    for malformed in [false, true] {
        let mut f = Fixture::with_source(malformed.then_some(b"not json"));
        let stopped = f.stopped();
        if !malformed {
            std::fs::write(f.path(), b"changed source").unwrap();
        }
        assert_eq!(f.checkpoint(stopped), Err(Refused::Checkpoint));
        assert!(f.desk.attention.is_some());
        let ui = round(
            &mut f.desk,
            [DeskInput::Updates {
                version: None,
                restart: true,
            }],
            Instant::now(),
        );
        assert!(ui.publish);
        assert_eq!(
            f.owner.0.gate.close().wait_until(Instant::now()),
            Observation::Pending
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dropped_checkpoint_reply_or_panicking_loader_cannot_report_idle() {
    let mut f = Fixture::new();
    let stopped = f.stopped();
    drop(stopped);
    let (reply, received) = mpsc::sync_channel(1);
    *f.owner.0.phase.lock().unwrap() = Phase::Finalizing;
    drop(received);
    Command::Checkpoint(f.owner.0.clone(), deadline(), reply).run(&mut f.desk);
    assert!(f.desk.attention.is_some());
    assert_eq!(
        f.owner.0.gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
    let (_, _, owner) = channel(true);
    let load = owner.track(Box::new(|| panic!("fixture loader panic")));
    assert!(thread::spawn(load).join().is_err());
    assert_eq!(
        owner.0.gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
}

#[cfg(unix)]
#[test]
fn absent_expired_duplicate_and_wrong_owner_requests_cannot_retire_attention() {
    let (_, _, absent) = channel(false);
    assert!(matches!(
        absent.control().stop(deadline()),
        Err(Refused::Unavailable)
    ));
    let mut f = Fixture::new();
    assert!(matches!(
        f.owner.control().stop(Instant::now()),
        Err(Refused::Pending)
    ));
    assert!(!f.owner.0.gate.snapshot().unwrap().closed);
    let other = Fixture::new();
    let control = other.owner.control();
    let host = thread::spawn(move || control.stop(deadline()));
    other.command().run(&mut f.desk);
    assert!(matches!(host.join().unwrap(), Err(Refused::Unavailable)));
    assert!(f.desk.observing_attention);
    let stopped = f.stopped();
    assert!(matches!(
        f.owner.control().stop(deadline()),
        Err(Refused::Closed)
    ));
    assert!(f.checkpoint(stopped).is_ok());
    assert!(matches!(
        f.owner.control().stop(deadline()),
        Err(Refused::Closed)
    ));
    let weak = f.owner.control();
    drop(f);
    assert!(matches!(weak.stop(deadline()), Err(Refused::Unavailable)));
}

#[test]
fn sender_returns_the_exact_original_input_on_disconnect() {
    let (sender, inputs, _owner) = channel(false);
    drop(inputs);
    let Err(mpsc::SendError(DeskInput::Clicked(token))) =
        sender.send(DeskInput::Clicked("opaque".into()))
    else {
        panic!("original input")
    };
    assert_eq!(token, "opaque");
}

#[cfg(not(unix))]
#[test]
fn unsupported_stop_does_not_close_queue_or_source() {
    let f = Fixture::new();
    assert!(matches!(
        f.owner.control().stop(deadline()),
        Err(Refused::Unsupported)
    ));
    assert!(!f.owner.0.gate.snapshot().unwrap().closed);
    assert!(f.desk.observing_attention);
    assert!(f.inputs.try_recv().is_err());
}

#[cfg(unix)]
fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn retaining_local_owner_and_control_does_not_keep_the_mailbox_connected() {
    let (sender, inputs, owner) = channel(false);
    let control = owner.control();
    drop(sender);
    assert!(matches!(
        inputs.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    drop(owner);
    assert!(control.0.upgrade().is_none());
}

#[cfg(unix)]
#[test]
fn admitted_native_failure_finishes_and_saves_before_the_ordered_desk_checkpoint() {
    use crate::notifier::{
        retiring::{Batch as NativeBatch, Retiring},
        Center, Done,
    };
    use zerus_client_core::desktop::drain::Gate as NativeGate;
    #[derive(Default)]
    struct Calls {
        authorize: Option<Done<(bool, Option<String>)>>,
        added: Option<Done<Option<String>>>,
    }
    #[derive(Clone, Default)]
    struct Native(Arc<Mutex<Calls>>);
    impl Center for Native {
        fn authorize(&self, done: Done<(bool, Option<String>)>) {
            self.0.lock().unwrap().authorize = Some(done);
        }
        fn add(&self, _: &str, _: &str, _: &str, done: Done<Option<String>>) {
            self.0.lock().unwrap().added = Some(done);
        }
        fn remove(&self, _: &str) {}
    }
    let mut f = Fixture::new();
    let original = round(&mut f.desk, live("example/one"), Instant::now())
        .changes
        .raised;
    let before = std::fs::read(f.path()).unwrap();
    let native = Native::default();
    let gate = NativeGate::new(1).unwrap();
    let sender = f.sender.clone();
    let (posting, input) = Retiring::new_accounted(
        Box::new(native.clone()),
        Arc::new(|work| work()),
        Box::new(move |token, detail| sender.send(DeskInput::Failed { token, detail }).unwrap()),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    posting
        .prepare(NativeBatch {
            cleared: vec![],
            raised: original,
        })
        .unwrap()
        .run();
    let stopped = f.stopped();
    let queued = input.close().unwrap();
    let native_closed = gate.close();
    assert_eq!(queued.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(
        native_closed.wait_until(Instant::now()),
        Observation::Pending
    );
    let authorized = native.0.lock().unwrap().authorize.take().unwrap();
    authorized((true, None));
    assert_eq!(
        native_closed.wait_until(Instant::now()),
        Observation::Pending,
        "add completion still held"
    );
    assert_eq!(std::fs::read(f.path()).unwrap(), before);
    let added = native.0.lock().unwrap().added.take().unwrap();
    added(Some("native fixture failure".into()));
    assert_eq!(native_closed.wait_until(Instant::now()), Observation::Idle);
    // The failure send has returned, so the final barrier is ordered after it.
    let host = thread::spawn(move || stopped.checkpoint_after_native_drain(deadline()));
    let first = f.inputs.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut batch = super::Batch::new(Some(first), &f.inputs);
    let outcome = round(&mut f.desk, &mut batch, Instant::now());
    assert_eq!(outcome.window_notices.len(), 1);
    let saved = std::fs::read(f.path()).unwrap();
    assert_ne!(saved, before);
    batch
        .barrier
        .take()
        .unwrap_or_else(|| f.command())
        .run(&mut f.desk);
    assert_eq!(host.join().unwrap().unwrap().sha256, sha256(&saved));
    assert!(f.desk.attention.is_none());
    assert_eq!(f.owner.0.gate.snapshot().unwrap().admitted, 0);
}
