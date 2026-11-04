use super::*;
use crate::notifier::retiring::{Batch, Refused, Retiring};
#[cfg(unix)]
use crate::notifier::retiring::{BYTES, ITEMS, QUEUED};
fn token() -> String {
    zerus_client_core::attention::encode_token(
        serde_json::json!({"host":"","name":"codex/example/one","notice":"original"})
            .as_object()
            .unwrap(),
    )
}
fn batch() -> Batch {
    Batch {
        cleared: vec![],
        raised: vec![Notice {
            token: token(),
            title: "Zerus".into(),
            body: "private fixture".into(),
        }],
    }
}
type Fixture = (
    Retiring,
    crate::notifier::retiring::InputControl,
    Gate,
    FakeCenter,
    Queue,
    Arc<Mutex<Vec<(String, String)>>>,
);
fn fixture() -> Fixture {
    let gate = Gate::new(1).unwrap();
    let center = FakeCenter::default();
    let main = Queue::default();
    let failed = Arc::new(Mutex::new(vec![]));
    let sink = failed.clone();
    let (notifier, control) = Retiring::new_accounted(
        Box::new(center.clone()),
        main.hop(),
        Box::new(move |t, d| sink.lock().unwrap().push((t, d))),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    (notifier, control, gate, center, main, failed)
}
#[cfg(unix)]
#[test]
fn retirement_closes_new_inputs_but_finishes_admitted_authorize_add_and_failure_sink() {
    let (n, c, gate, center, main, failed) = fixture();
    let prepared = n.prepare(batch()).unwrap();
    let queued = c.close().unwrap();
    let native = gate.close();
    assert_eq!(queued.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    let refused = n.prepare(batch()).err().unwrap();
    assert_eq!(refused.reason, Refused::Closed);
    assert_eq!(refused.batch.raised[0].body, "private fixture");
    prepared.run();
    assert_eq!(queued.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(center.log(), ["authorize"]);
    center.answer_authorization(true, None);
    assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    main.run();
    assert_eq!(
        center.log(),
        [format!("add {}: Zerus: private fixture", token())]
    );
    center.answer_add(&token(), Some("fixture error"));
    assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    main.run();
    assert_eq!(*failed.lock().unwrap(), [(token(), "fixture error".into())]);
    assert_eq!(native.wait_until(Instant::now()), Observation::Idle);
    assert!(center.log().is_empty());
}
#[cfg(unix)]
#[test]
fn an_admitted_withdrawal_still_cancels_its_original_pending_post_after_retirement() {
    let (n, c, gate, center, main, failed) = fixture();
    n.prepare(batch()).unwrap().run();
    let withdrawal = n
        .prepare(Batch {
            cleared: vec![token()],
            raised: vec![],
        })
        .unwrap();
    c.close().unwrap();
    let native = gate.close();
    withdrawal.run();
    assert_eq!(
        center.log(),
        ["authorize".to_owned(), format!("remove {}", token())]
    );
    center.answer_authorization(true, None);
    main.run();
    assert!(center.log().is_empty());
    assert!(failed.lock().unwrap().is_empty());
    assert_eq!(native.wait_until(Instant::now()), Observation::Idle);
}
#[cfg(unix)]
#[test]
fn queue_pressure_and_size_or_owner_refusals_preserve_original_payload_before_effects() {
    let (n, c, gate, center, _, _) = fixture();
    let mut many = batch();
    many.raised[0].body = "x".repeat(BYTES + 1);
    let rejected = n.prepare(many).err().unwrap();
    assert_eq!(rejected.reason, Refused::TooLarge);
    assert_eq!(rejected.batch.raised[0].body.len(), BYTES + 1);
    let rejected = n
        .prepare(Batch {
            cleared: vec![token(); ITEMS + 1],
            raised: vec![],
        })
        .err()
        .unwrap();
    assert_eq!(rejected.reason, Refused::TooLarge);
    let mut wrong = batch();
    wrong.raised[0].token = "zerus-update:v2:dev:123".into();
    assert_eq!(n.prepare(wrong).err().unwrap().reason, Refused::TokenOwner);
    let mut prepared: Vec<_> = (0..QUEUED).map(|_| n.prepare(batch()).unwrap()).collect();
    let rejected = n.prepare(batch()).err().unwrap();
    assert_eq!(rejected.reason, Refused::Pressure);
    assert_eq!(rejected.batch.raised[0].body, "private fixture");
    // Dispatching one original batch releases only its queue reservation.
    // The other payloads stay charged and are never evicted.
    prepared.pop().unwrap().run();
    assert_eq!(center.log(), ["authorize"]);
    assert!(n.prepare(batch()).is_ok()); // Dropped unexecuted: unknown pins.
    c.close().unwrap();
    assert_eq!(
        gate.close().wait_until(Instant::now()),
        Observation::Pending
    );
    assert_eq!(prepared.len(), QUEUED - 1);
}
#[cfg(unix)]
#[test]
fn dropped_preparation_callback_or_hop_never_releases_unknown_native_work() {
    for stage in 0..3 {
        let (n, c, gate, center, main, _) = fixture();
        let prepared = n.prepare(batch()).unwrap();
        if stage == 0 {
            drop(prepared);
        } else {
            prepared.run();
        }
        c.close().unwrap();
        let native = gate.close();
        if stage == 1 {
            center.calls().authorizations.clear();
        }
        if stage == 2 {
            center.answer_authorization(true, None);
            main.0.lock().unwrap().clear();
        }
        assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    }
}
#[cfg(not(unix))]
#[test]
fn unsupported_retirement_changes_no_admissions() {
    let (n, c, gate, center, _, _) = fixture();
    assert!(matches!(c.close(), Err(Refused::Unsupported)));
    let prepared = n.prepare(batch()).unwrap();
    assert!(center.log().is_empty());
    assert_eq!(gate.snapshot().unwrap().count(Role::NativePosting), 1);
    drop(prepared);
}

#[cfg(unix)]
#[test]
fn retiring_preparation_holds_queue_and_native_scope_until_the_real_call_returns() {
    use std::{sync::mpsc, thread, time::Duration};
    struct Blocking {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Center for Blocking {
        fn authorize(&self, done: Done<(bool, Option<String>)>) {
            done((true, None));
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
        fn add(&self, _: &str, _: &str, _: &str, done: Done<Option<String>>) {
            done(None);
        }
        fn remove(&self, _: &str) {}
    }
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let gate = Gate::new(1).unwrap();
    let (n, c) = Retiring::new_accounted(
        Box::new(Blocking {
            entered,
            release: Mutex::new(released),
        }),
        Arc::new(|work| work()),
        Box::new(|_, _| panic!("failure")),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    let original = n.prepare(batch()).unwrap();
    let worker = thread::spawn(move || original.run());
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    let queued = c.close().unwrap();
    let native = gate.close();
    assert_eq!(queued.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(queued.wait_until(Instant::now()), Observation::Idle);
    assert_eq!(native.wait_until(Instant::now()), Observation::Idle);
}

#[cfg(unix)]
#[test]
fn staged_agent_retirement_keeps_shared_update_posting_and_outcomes_independent() {
    let center = FakeCenter::default();
    let main = Queue::default();
    let gate = Gate::new(1).unwrap();
    let failures = Arc::new(Mutex::new(vec![]));
    let sink = failures.clone();
    let (agent, updates, control) = crate::notifier::owners::Partitioned::new_retiring(
        Box::new(center.clone()),
        main.hop(),
        Box::new(|_, _| panic!("agent failure")),
        Box::new(move |t, d| sink.lock().unwrap().push((t, d))),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    agent.prepare(batch()).unwrap().run();
    let update = Notice {
        token: "zerus-update:v2:dev:123".into(),
        title: "Update".into(),
        body: "fixture".into(),
    };
    updates.deliver(&[], std::slice::from_ref(&update)).unwrap();
    assert_eq!(center.log(), ["authorize"]);
    control.close().unwrap();
    let native = gate.close();
    center.answer_authorization(true, None);
    main.run();
    assert_eq!(center.log().len(), 2);
    assert_eq!(native.wait_until(Instant::now()), Observation::Pending);
    center.answer_add(&token(), None);
    main.run();
    assert_eq!(
        native.wait_until(Instant::now()),
        Observation::Idle,
        "remaining update callback is GUI-owned"
    );
    center.answer_add(&update.token, Some("update error"));
    main.run();
    assert_eq!(
        *failures.lock().unwrap(),
        [(update.token.clone(), "update error".into())]
    );
    updates.deliver(&[update.token], &[]).unwrap();
    assert_eq!(center.log(), ["remove zerus-update:v2:dev:123"]);
    assert_eq!(
        agent.prepare(batch()).err().unwrap().reason,
        Refused::Closed
    );
}
