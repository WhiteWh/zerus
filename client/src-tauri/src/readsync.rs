//! The read-mark delivery worker: Qt's `ReadMarkSync` (`tray/src/ReadMarkSync.cpp`)
//! around the core's [`ReadSync`]. One thread wakes every 5 s and whenever
//! marks are queued, hands the pending marks to `ReadSync` and runs each due
//! `hgs [@host] read` on its own short-lived thread (at most four at once), so
//! one slow SSH peer never holds up the loop.
//!
//! Each call claims its session's read lane; a session whose read lane is busy
//! is left for the next wake. Writes and interrupts keep their own lanes: a
//! read never blocks a write, and Stop never waits for one. Individual deliveries
//! show no notice; saturation of all four retained slots warns once. Each wake first
//! writes the marks `queue_read_marks` added, so a mark is saved before any
//! call while the command itself does no file I/O. Marks are settled by
//! `Actions::observe`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use zerus_client_core::actions::{classify, invocation};
use zerus_client_core::desktop::drain::{Charge, Closed, Gate, Role};
use zerus_client_core::readmarks::ReadMark;
use zerus_client_core::readsync::{applied_all, ReadSync};
use zerus_client_core::{default_hgs, Action, Exit, Invocation, Outcome, Target};

use crate::actions::{Actions, Claim, Lane};
mod slots;

/// Qt's `m_timer` interval.
pub const INTERVAL: Duration = Duration::from_secs(5);

/// Wakes the worker before its interval is up; clones share it.
#[derive(Clone, Default)]
pub struct Wake {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Wake {
    pub fn notify(&self) {
        let (woken, signal) = &*self.inner;
        *lock(woken) = true;
        signal.notify_one();
    }

    /// Waits until notified or `timeout`; true when notified. Clears the flag.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (woken, signal) = &*self.inner;
        let guard = lock(woken);
        let (mut guard, _) = signal
            .wait_timeout_while(guard, timeout, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *guard)
    }
}

/// Runs one invocation (in the app: `run` with `default_hgs()`).
type Runner = Arc<dyn Fn(&Invocation, Arc<slots::Resource>, &AtomicBool) -> Exit + Send + Sync>;
/// Milliseconds on a monotonic clock.
type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
struct Worker {
    actions: Actions,
    sync: Arc<Mutex<ReadSync>>,
    run: Runner,
    clock: Clock,
    /// False in a replay without `ZERUS_CLIENT_HGS`: marks are saved, never sent.
    deliver: bool,
    gate: Gate,
    cancel: Arc<AtomicBool>,
    slots: slots::Pool,
}

/// Shared by Actions' direct MarkRead path and the one background worker.
/// Stop covers native calls only; Actions owns the ordered mark-store barrier.
#[derive(Clone)]
pub struct ReadDelivery {
    gate: Gate,
    cancel: Arc<AtomicBool>,
    wake: Wake,
    slots: slots::Pool,
    started: Arc<AtomicBool>,
}

impl ReadDelivery {
    pub(crate) fn new(wake: Wake) -> Self {
        Self {
            gate: Gate::new(5).expect("one loop plus four native read calls"),
            cancel: Arc::new(AtomicBool::new(false)),
            wake,
            slots: slots::Pool::new(),
            started: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn stop(&self) -> Closed {
        let closed = self.gate.close();
        self.cancel.store(true, Ordering::Release);
        self.slots.stop();
        self.wake.notify();
        closed
    }
    pub(crate) fn admit(&self) -> Option<Call> {
        let charge = self.gate.admit(Role::ReadDelivery).ok()?;
        let resource = self.slots.take(charge)?;
        Some(Call {
            resource: Some(resource),
            control: self.clone(),
            wake_on_drop: false,
        })
    }
}

/// The callback's copy lives through result handling. Every actual pipe and
/// the owned child retain the same resource through run_accounted.
pub(crate) struct Call {
    resource: Option<Arc<slots::Resource>>,
    control: ReadDelivery,
    wake_on_drop: bool,
}
impl Call {
    fn resource(&self) -> Arc<slots::Resource> {
        self.resource.as_ref().unwrap().clone()
    }
    pub(crate) fn native(&mut self) -> NativeRead {
        self.wake_on_drop = true;
        NativeRead {
            resource: self.resource(),
            cancel: self.control.cancel.clone(),
        }
    }
}
pub(crate) struct NativeRead {
    resource: Arc<slots::Resource>,
    cancel: Arc<AtomicBool>,
}
impl NativeRead {
    pub(crate) fn run(&self, hgs: &std::path::Path, inv: &Invocation) -> Exit {
        run_accounted(hgs, inv, self.resource.clone(), &self.cancel)
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        let resource = self.resource.take().unwrap();
        if self.control.slots.finished(&resource) {
            eprintln!(
                "zerus-client: resource pinned role=read_delivery slot={}",
                resource.slot
            );
        }
        drop(resource);
        if self.wake_on_drop {
            self.control.wake.notify();
        }
    }
}

fn run_accounted(
    hgs: &std::path::Path,
    inv: &Invocation,
    resource: Arc<slots::Resource>,
    cancel: &AtomicBool,
) -> Exit {
    match zerus_client_core::run::run_accounted(hgs, inv, &[], 1 << 20, cancel, resource) {
        Ok(exit) => exit,
        Err(_) => Exit::Abnormal {
            stderr: String::new(),
        },
    }
}

impl Worker {
    fn new(actions: Actions, run: Runner, clock: Clock, deliver: bool) -> Self {
        let control = actions.read_delivery.clone();
        Self {
            actions,
            sync: Arc::new(Mutex::new(ReadSync::new())),
            run,
            clock,
            deliver,
            gate: control.gate,
            cancel: control.cancel,
            slots: control.slots,
        }
    }

    fn control(&self) -> ReadDelivery {
        self.actions.read_delivery.clone()
    }

    /// One wake (`ReadMarkSync::setPending` and `pump`): each due mark whose
    /// session's read lane is free goes to its own thread; one whose lane is
    /// busy is left for the next wake. Returns the delivery threads.
    fn pump(&self) -> Vec<JoinHandle<()>> {
        if self.cancel.load(Ordering::Acquire) {
            return Vec::new();
        }
        // Saved before any call (`ReadMarkStore::add` before `setPending`).
        self.actions.flush_marks();
        if !self.deliver {
            return Vec::new();
        }
        let pending = self.actions.pending_marks();
        let due = {
            let mut sync = lock(&self.sync);
            sync.set_pending(pending);
            sync.due((self.clock)())
        };
        let mut threads = Vec::new();
        for mark in due {
            let Some(claim) = self
                .actions
                .claim_lane(&mark.computer, &mark.name, Lane::Read)
            else {
                lock(&self.sync).not_sent(&mark);
                continue;
            };
            let Some(call) = self.control().admit() else {
                lock(&self.sync).not_sent(&mark);
                continue;
            };
            let worker = self.clone();
            let sent = mark.clone();
            let spawned = std::thread::Builder::new()
                .name("zerus-read-mark".into())
                .spawn(move || worker.deliver(&sent, claim, call));
            match spawned {
                Ok(thread) => threads.push(thread),
                // The claim went with the closure.
                Err(_) => lock(&self.sync).not_sent(&mark),
            }
        }
        threads
    }

    /// Runs `hgs [@host] read` for the mark and reports how it ended. An
    /// unset part is sent as null, which hosts never apply (`ReadMark::payload`).
    fn deliver(&self, mark: &ReadMark, claim: Claim, mut call: Call) {
        call.wake_on_drop = true;
        let target = Target {
            computer: mark.computer.clone(),
            name: mark.name.clone(),
            run_id: mark.run_id.clone(),
            conversation_id: mark.conversation_id.clone(),
        };
        let part = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
        let action = Action::MarkRead {
            reply_id: part(&mark.reply_id),
            attention_signature: part(&mark.attention_signature),
        };
        // A panic still frees the slot, as a failed call.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let request_id = uuid::Uuid::new_v4().to_string();
            invocation(&target, &action, &request_id).ok().map(|inv| {
                classify(
                    &target,
                    &action,
                    &inv,
                    (self.run)(&inv, call.resource(), &self.cancel),
                )
            })
        }));
        drop(claim);
        let (delivered, all) = match outcome {
            Ok(Some(Outcome::Done { receipt, .. })) => (true, applied_all(mark, &receipt)),
            _ => (false, false),
        };
        lock(&self.sync).finished(mark, delivered, all, (self.clock)());
        drop(call);
    }

    fn start(self) -> ReadDelivery {
        let control = self.control();
        if control.started.swap(true, Ordering::AcqRel) {
            return control;
        }
        let wake = self.actions.wake.clone();
        let charged = self.gate.admit(Role::ReadDelivery);
        let spawned = charged.ok().and_then(|charge| {
            std::thread::Builder::new()
                .name("zerus-read-marks".into())
                .spawn(move || {
                    let _loop = charge;
                    loop {
                        // The first wake keeps the existing Qt interval.
                        wake.wait(INTERVAL);
                        if self.cancel.load(Ordering::Acquire) {
                            break;
                        }
                        // Jobs may outlive this turn; their guards remain in
                        // the actual callback and every owned pipe worker.
                        drop(self.pump());
                    }
                })
                .ok()
        });
        if spawned.is_none() {
            control.stop();
            eprintln!("zerus-client: the read-mark worker did not start");
        }
        control
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the worker for the app's marks. In replay mode without an explicit
/// `ZERUS_CLIENT_HGS` it only saves queued marks and sends nothing, so a
/// replay never drives the real CLI.
pub fn start(actions: Actions, observe: slots::Observe) -> ReadDelivery {
    let deliver = !crate::actions::blocked();
    let started = Instant::now();
    let run: Runner =
        Arc::new(|inv, charge, cancel| run_accounted(&default_hgs(), inv, charge, cancel));
    let clock: Clock =
        Arc::new(move || i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));
    let worker = Worker::new(actions, run, clock, deliver);
    worker.slots.observe(observe);
    worker.start()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::Lane;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicI64, Ordering};

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new(test: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "zerus-readsync-worker-{}-{test}.json",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            Temp(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn mark(computer: &str, name: &str, reply: Option<&str>, signature: Option<&str>) -> ReadMark {
        ReadMark {
            computer: computer.into(),
            name: name.into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
            reply_id: reply.map(Into::into),
            attention_signature: signature.map(Into::into),
        }
    }

    fn exit(code: i32, stdout: Value) -> Exit {
        Exit::Code {
            code,
            stdout: serde_json::to_vec(&stdout).unwrap(),
            stderr: String::new(),
        }
    }

    /// A worker whose calls answer `answer`, recording each invocation, on a
    /// clock the test moves.
    struct Fixture {
        _temp: Temp,
        actions: Actions,
        worker: Worker,
        calls: Arc<Mutex<Vec<Invocation>>>,
        now: Arc<AtomicI64>,
    }

    impl Fixture {
        fn new(test: &str, answer: impl Fn(&Invocation) -> Exit + Send + Sync + 'static) -> Self {
            Self::with(test, true, answer)
        }

        /// `deliver` false: a replay without `ZERUS_CLIENT_HGS`.
        fn with(
            test: &str,
            deliver: bool,
            answer: impl Fn(&Invocation) -> Exit + Send + Sync + 'static,
        ) -> Self {
            let temp = Temp::new(test);
            let actions = Actions::new(temp.0.clone());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let now = Arc::new(AtomicI64::new(0));
            let recorded = Arc::clone(&calls);
            let run: Runner = Arc::new(move |inv: &Invocation, _charge, _cancel| {
                recorded.lock().unwrap().push(inv.clone());
                answer(inv)
            });
            let clock_now = Arc::clone(&now);
            let clock: Clock = Arc::new(move || clock_now.load(Ordering::SeqCst));
            let worker = Worker::new(actions.clone(), run, clock, deliver);
            Self {
                _temp: temp,
                actions,
                worker,
                calls,
                now,
            }
        }

        /// One wake, waiting for its calls; returns how many it started.
        fn pump(&self) -> usize {
            let threads = self.worker.pump();
            let started = threads.len();
            for thread in threads {
                thread.join().unwrap();
            }
            started
        }

        fn calls(&self) -> Vec<Invocation> {
            self.calls.lock().unwrap().clone()
        }

        fn at(&self, ms: i64) {
            self.now.store(ms, Ordering::SeqCst);
        }
    }

    fn applied(reply: bool, signature: bool) -> Value {
        json!({"ok": true, "applied": {"reply_id": reply, "attention_signature": signature},
               "read": {"conversation_id": "conv-1"}})
    }

    #[test]
    fn a_queued_mark_is_sent_with_its_parts_and_resent_after_120_s() {
        let f = Fixture::new("send", |_| exit(0, applied(true, false)));
        f.actions
            .queue_marks(vec![mark(
                "mac",
                "claude/sample-project/review",
                Some("r1"),
                None,
            )])
            .unwrap();
        assert_eq!(f.pump(), 1);
        let calls = f.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].args,
            ["@mac", "read", "claude/sample-project/review", "--json"]
        );
        let input: Value = serde_json::from_slice(calls[0].stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"run_id": "run-1", "conversation_id": "conv-1",
                   "reply_id": "r1", "attention_signature": null})
        );
        // Delivered and applied: nothing until 120 s have passed.
        f.at(119_999);
        assert_eq!(f.pump(), 0);
        f.at(120_000);
        assert_eq!(f.pump(), 1);
        // The mark stays pending until a snapshot settles it.
        assert_eq!(f.actions.pending_marks().len(), 1);
    }

    /// The JSON the frontend's `markAllRead` passes to `queue_read_marks` (the
    /// payload asserted in `client/src/store/marks.test.ts`) is queued as
    /// `queue_read_marks` does and delivered through `read NAME --json`.
    #[test]
    fn the_frontends_queue_payload_is_delivered_through_read() {
        let f = Fixture::new("frontend-payload", |_| exit(0, applied(true, false)));
        let marks: Vec<ReadMark> = serde_json::from_value(json!([{
            "computer": "mac", "name": "claude/sample-project/a", "run_id": "run-1",
            "conversation_id": "conv-1", "reply_id": "r1", "attention_signature": null
        }]))
        .unwrap();
        assert_eq!(f.actions.queue_marks(marks).unwrap().len(), 1);
        assert_eq!(f.pump(), 1);
        let calls = f.calls();
        assert_eq!(
            calls[0].args,
            ["@mac", "read", "claude/sample-project/a", "--json"]
        );
        let input: Value = serde_json::from_slice(calls[0].stdin.as_ref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"run_id": "run-1", "conversation_id": "conv-1",
                   "reply_id": "r1", "attention_signature": null})
        );
    }

    #[test]
    fn an_empty_part_is_sent_as_null() {
        let f = Fixture::new("null", |_| exit(0, applied(false, true)));
        // A mark saved by run_action may carry an empty string.
        f.actions.add_mark_for_test(mark(
            "",
            "claude/sample-project/review",
            Some(""),
            Some("s1"),
        ));
        assert_eq!(f.pump(), 1);
        let calls = f.calls();
        assert_eq!(
            calls[0].args,
            ["read", "claude/sample-project/review", "--json"]
        );
        let input: Value = serde_json::from_slice(calls[0].stdin.as_ref().unwrap()).unwrap();
        assert_eq!(input["reply_id"], Value::Null);
        assert_eq!(input["attention_signature"], "s1");
        // Applied in full (the reply was not set): the 120 s resend.
        f.at(119_999);
        assert_eq!(f.pump(), 0);
    }

    #[test]
    fn a_part_the_host_did_not_apply_waits_longer() {
        let f = Fixture::new("partial", |_| exit(0, applied(true, false)));
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), Some("s1"))])
            .unwrap();
        assert_eq!(f.pump(), 1);
        f.at(120_000);
        assert_eq!(f.pump(), 1);
        // Second partial answer: 240 s.
        f.at(120_000 + 239_999);
        assert_eq!(f.pump(), 0);
        f.at(120_000 + 240_000);
        assert_eq!(f.pump(), 1);
    }

    #[test]
    fn a_failed_call_backs_off() {
        let f = Fixture::new("failed", |_| Exit::TimedOut);
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), None)])
            .unwrap();
        assert_eq!(f.pump(), 1);
        f.at(9_999);
        assert_eq!(f.pump(), 0);
        f.at(10_000);
        assert_eq!(f.pump(), 1);
        f.at(10_000 + 19_999);
        assert_eq!(f.pump(), 0);
        f.at(10_000 + 20_000);
        assert_eq!(f.pump(), 1);
    }

    #[test]
    fn a_session_that_is_gone_counts_as_delivered() {
        let f = Fixture::new("gone", |_| {
            exit(1, json!({"ok": false, "error": "not_found"}))
        });
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), None)])
            .unwrap();
        assert_eq!(f.pump(), 1);
        // Delivered, nothing applied: 120 s, not a 10 s failure.
        f.at(119_999);
        assert_eq!(f.pump(), 0);
        f.at(120_000);
        assert_eq!(f.pump(), 1);
    }

    #[test]
    fn a_busy_read_lane_leaves_the_mark_for_the_next_wake() {
        let f = Fixture::new("lane", |_| exit(0, applied(true, true)));
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), None)])
            .unwrap();
        let read = f.actions.claim_lane("mac", "s", Lane::Read).unwrap();
        assert_eq!(f.pump(), 0);
        assert!(f.calls().is_empty());
        drop(read);
        // Not counted as a failure: due again at once.
        assert_eq!(f.pump(), 1);
        // The lane is free again once the call ended.
        assert!(f.actions.claim_lane("mac", "s", Lane::Read).is_some());
    }

    #[test]
    fn a_running_write_or_stop_does_not_hold_up_a_read() {
        let f = Fixture::new("lanes", |_| exit(0, applied(true, true)));
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), None)])
            .unwrap();
        let _write = f.actions.claim_lane("mac", "s", Lane::Write).unwrap();
        let _stop = f.actions.claim_lane("mac", "s", Lane::Interrupt).unwrap();
        assert_eq!(f.pump(), 1);
    }

    #[test]
    fn at_most_four_calls_run_at_once() {
        // Each call waits inside the runner until the test opens the gate, so
        // four calls in there at the same time prove they run concurrently.
        let gate = Arc::new((Mutex::new((0_usize, false)), Condvar::new()));
        let inside = Arc::clone(&gate);
        let f = Fixture::new("four", move |_| {
            let (state, signal) = &*inside;
            let mut state = state.lock().unwrap();
            state.0 += 1;
            signal.notify_all();
            let (state, _) = signal
                .wait_timeout_while(state, Duration::from_secs(10), |state| !state.1)
                .unwrap();
            assert!(state.1, "the test opened the gate");
            exit(0, applied(true, true))
        });
        f.actions
            .queue_marks(
                (0..6)
                    .map(|i| mark(&format!("host-{i}"), "s", Some("r1"), None))
                    .collect(),
            )
            .unwrap();
        let first = f.worker.pump();
        assert_eq!(first.len(), 4);
        let (state, signal) = &*gate;
        let arrived = signal
            .wait_timeout_while(state.lock().unwrap(), Duration::from_secs(10), |state| {
                state.0 < 4
            })
            .unwrap()
            .0
             .0;
        assert_eq!(arrived, 4, "four calls inside the runner at once");
        // The loop did not wait for them, and no fifth call starts meanwhile.
        assert!(f.worker.pump().is_empty());
        assert_eq!(f.calls().len(), 4);
        state.lock().unwrap().1 = true;
        signal.notify_all();
        for thread in first {
            thread.join().unwrap();
        }
        assert_eq!(f.pump(), 2);
        assert_eq!(f.calls().len(), 6);
    }

    #[test]
    fn queued_marks_are_on_disk_before_the_first_call() {
        let path = Arc::new(Mutex::new(None::<std::path::PathBuf>));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (reader, record) = (Arc::clone(&path), Arc::clone(&seen));
        let f = Fixture::new("saved-first", move |_| {
            let file = reader.lock().unwrap().clone().unwrap();
            record
                .lock()
                .unwrap()
                .push(zerus_client_core::readmarks::ReadMarks::load(file).pending());
            exit(0, applied(true, true))
        });
        *path.lock().unwrap() = Some(f._temp.0.clone());
        let m = mark("mac", "s", Some("r1"), None);
        f.actions.queue_marks(vec![m.clone()]).unwrap();
        // Queueing does no file I/O.
        assert!(!f._temp.0.exists());
        assert_eq!(f.pump(), 1);
        assert_eq!(*seen.lock().unwrap(), vec![vec![m]]);
    }

    #[test]
    fn a_replay_without_hgs_saves_marks_but_sends_none() {
        let f = Fixture::with("replay", false, |_| exit(0, applied(true, true)));
        let m = mark("mac", "s", Some("r1"), None);
        f.actions.queue_marks(vec![m.clone()]).unwrap();
        assert_eq!(f.pump(), 0);
        f.at(600_000);
        assert_eq!(f.pump(), 0);
        assert!(f.calls().is_empty());
        assert_eq!(
            zerus_client_core::readmarks::ReadMarks::load(f._temp.0.clone()).pending(),
            vec![m]
        );
    }

    #[test]
    fn a_mark_a_snapshot_settled_is_not_sent() {
        use zerus_client_core::{Batch, ComputerId, RowOut, ViewRow};
        let f = Fixture::new("settled", |_| exit(0, applied(true, true)));
        f.actions
            .queue_marks(vec![mark("mac", "s", Some("r1"), None)])
            .unwrap();
        f.actions.observe(&Batch {
            upserts: vec![RowOut {
                computer: ComputerId::from("mac"),
                key: "run:s".into(),
                row: ViewRow {
                    name: "s".into(),
                    run_id: Some("run-1".into()),
                    conversation_id: Some("conv-1".into()),
                    reply_id: Some("r1".into()),
                    read_reply_id: Some("r1".into()),
                    ..ViewRow::default()
                },
            }],
            ..Batch::default()
        });
        assert_eq!(f.pump(), 0);
        assert!(f.calls().is_empty());
    }

    #[test]
    fn a_wake_is_kept_until_the_worker_waits() {
        let wake = Wake::default();
        assert!(!wake.wait(Duration::ZERO));
        wake.notify();
        wake.notify();
        assert!(wake.wait(Duration::from_secs(5)));
        assert!(!wake.wait(Duration::from_millis(10)));
        let other = wake.clone();
        let waiter = std::thread::spawn(move || other.wait(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        wake.notify();
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn shared_worker_start_is_one_use_and_native_stop_preserves_the_store_queue() {
        use zerus_client_core::desktop::drain::Observation;
        let f = Fixture::with("single-worker", false, |_| panic!("replay delivered"));
        let first = f.worker.clone().start();
        let second = f.worker.clone().start();
        assert_eq!(
            first.gate.snapshot().unwrap().admitted,
            1,
            "one existing loop"
        );
        let closed = second.stop();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(2)),
            Observation::Idle
        );
        f.actions
            .queue_marks(vec![mark("", "waiting", Some("reply"), None)])
            .unwrap();
        f.actions.flush_marks();
        assert_eq!(
            f.actions.pending_marks().len(),
            1,
            "native stop alone never discards queued intent"
        );
        f.worker.clone().start();
        assert!(first.gate.snapshot().unwrap().closed);
        assert_eq!(first.gate.snapshot().unwrap().admitted, 0);
    }

    #[test]
    fn explicit_stop_keeps_pending_marks_and_never_starts_another_delivery() {
        use zerus_client_core::desktop::drain::Observation;
        let f = Fixture::new("drain-before-pump", |_| exit(0, serde_json::json!({})));
        f.actions
            .queue_marks(vec![mark("", "waiting", Some("reply"), None)])
            .unwrap();
        let closed = f.worker.control().stop();
        assert_eq!(f.pump(), 0);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        assert_eq!(f.actions.pending_marks().len(), 1);
        assert!(f.calls().is_empty());
        f.at(i64::MAX);
        f.actions.wake.notify();
        assert_eq!(f.pump(), 0);
    }

    #[test]
    fn stop_waits_for_the_real_delivery_callback_and_wakes_the_loop() {
        use zerus_client_core::desktop::drain::Observation;
        let (started, begun) = std::sync::mpsc::sync_channel(1);
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let ready = released.clone();
        let f = Fixture::new("drain-callback", move |_| {
            started.send(()).unwrap();
            let _done = ready.1.wait_while(lock(&ready.0), |done| !*done).unwrap();
            exit(1, serde_json::json!({}))
        });
        f.actions
            .queue_marks(vec![mark("", "waiting", Some("reply"), None)])
            .unwrap();
        let control = f.worker.clone().start();
        begun.recv_timeout(Duration::from_secs(2)).unwrap();
        let closed = control.stop();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        *lock(&released.0) = true;
        released.1.notify_all();
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(2)),
            Observation::Idle
        );
        assert_eq!(f.actions.pending_marks().len(), 1);
        f.at(i64::MAX);
        f.actions.wake.notify();
        assert_eq!(f.pump(), 0);
        assert_eq!(f.calls().len(), 1);
    }
}
