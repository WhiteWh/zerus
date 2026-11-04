use super::*;
use crate::notifier::owners::{Owner, Partitioned, Refused};

fn agent_token() -> String {
    let target =
        serde_json::json!({"host":"","name":"codex/example/one","notice":"notice-example"});
    zerus_client_core::attention::encode_token(target.as_object().unwrap())
}
const UPDATE: &str = "zerus-update:v2:dev:123";
fn notice(token: &str) -> Notice {
    Notice {
        token: token.into(),
        title: "Zerus".into(),
        body: "fixture".into(),
    }
}
struct Split {
    center: FakeCenter,
    main: Queue,
    pair: Partitioned,
    control: Control,
    gate: Gate,
    agent_failed: Arc<Mutex<Vec<(String, String)>>>,
    update_failed: Arc<Mutex<Vec<(String, String)>>>,
}
impl Split {
    fn new() -> Self {
        let center = FakeCenter::default();
        let main = Queue::default();
        let gate = Gate::new(1).unwrap();
        let agent_failed = Arc::new(Mutex::new(Vec::new()));
        let update_failed = Arc::new(Mutex::new(Vec::new()));
        let agent_sink = agent_failed.clone();
        let update_sink = update_failed.clone();
        let (pair, control) = Partitioned::new_accounted(
            Box::new(center.clone()),
            main.hop(),
            Box::new(move |t, d| agent_sink.lock().unwrap().push((t, d))),
            Box::new(move |t, d| update_sink.lock().unwrap().push((t, d))),
            gate.admit(Role::NativePosting).unwrap(),
        )
        .unwrap();
        Self {
            center,
            main,
            pair,
            control,
            gate,
            agent_failed,
            update_failed,
        }
    }
    fn post_both(&self, update_first: bool) {
        let agent = || {
            self.pair
                .agent
                .deliver(&[], &[notice(&agent_token())])
                .unwrap()
        };
        let update = || self.pair.updates.deliver(&[], &[notice(UPDATE)]).unwrap();
        if update_first {
            update();
            agent();
        } else {
            agent();
            update();
        }
    }
    fn close(&self) -> zerus_client_core::desktop::drain::Closed {
        self.control.close().unwrap();
        self.gate.close()
    }
}
#[test]
fn exact_domains_and_request_ids_are_disjoint_without_reencoding() {
    let token = agent_token();
    assert!(Owner::AgentAttention.accepts_token(&token));
    assert!(!Owner::SoftwareUpdate.accepts_token(&token));
    assert!(Owner::SoftwareUpdate.accepts_token(UPDATE));
    assert!(!Owner::AgentAttention.accepts_token(UPDATE));
    for token in [
        "legacy-unbound",
        "",
        "zerus-update:v2:dev:0123",
        "e30",
        "zerus.attention.other",
    ] {
        assert!(!Owner::AgentAttention.accepts_token(token));
        assert!(!Owner::SoftwareUpdate.accepts_token(token));
    }
    let original = token + "="; // Qt accepts this padding; never normalize its native ID.
    assert!(Owner::AgentAttention.accepts_token(&original));
    assert_eq!(
        zerus_client_core::attention::request_id(&original),
        format!("zerus.attention.{original}")
    );
    let f = Split::new();
    f.pair.agent.deliver(&[], &[notice(&original)]).unwrap();
    f.center.log();
    f.center.answer_authorization(true, None);
    f.main.run();
    assert_eq!(f.center.log(), [format!("add {original}: Zerus: fixture")]);
    f.center.answer_add(&original, None);
    f.main.run();
    assert_eq!(f.close().wait_until(Instant::now()), Observation::Idle);
}
#[test]
fn wrong_owner_refuses_the_entire_batch_before_any_effect_in_both_directions() {
    let f = Split::new();
    assert_eq!(
        f.pair
            .agent
            .deliver(&[], &[notice(&agent_token()), notice(UPDATE)]),
        Err(Refused::TokenOwner)
    );
    assert_eq!(
        f.pair
            .updates
            .deliver(&[], &[notice(UPDATE), notice(&agent_token())]),
        Err(Refused::TokenOwner)
    );
    assert_eq!(
        f.pair
            .agent
            .deliver(&[UPDATE.into()], &[notice(&agent_token())]),
        Err(Refused::TokenOwner)
    );
    assert_eq!(
        f.pair.updates.deliver(&[agent_token()], &[notice(UPDATE)]),
        Err(Refused::TokenOwner)
    );
    assert!(f.center.log().is_empty());
    assert_eq!(f.close().wait_until(Instant::now()), Observation::Idle);
}
#[test]
fn authorization_fans_out_once_to_two_separate_delivery_states_and_failure_sinks() {
    for update_first in [false, true] {
        let f = Split::new();
        f.post_both(update_first);
        assert_eq!(f.center.log(), ["authorize"]);
        f.center.answer_authorization(false, Some("fixture denial"));
        f.main.run();
        assert!(f.center.log().is_empty());
        assert_eq!(
            *f.agent_failed.lock().unwrap(),
            [(agent_token(), "fixture denial".into())]
        );
        assert_eq!(
            *f.update_failed.lock().unwrap(),
            [(UPDATE.into(), "fixture denial".into())]
        );
        assert_eq!(f.close().wait_until(Instant::now()), Observation::Idle);
    }
}
#[test]
fn withdraw_during_shared_authorization_never_removes_or_posts_the_other_owner() {
    for withdrawing_agent in [false, true] {
        let f = Split::new();
        f.post_both(false);
        f.center.log();
        let (withdrawn, retained, token) = if withdrawing_agent {
            (&f.pair.agent, UPDATE.to_owned(), agent_token())
        } else {
            (&f.pair.updates, agent_token(), UPDATE.to_owned())
        };
        withdrawn
            .deliver(std::slice::from_ref(&token), &[])
            .unwrap();
        assert_eq!(f.center.log(), [format!("remove {token}")]);
        f.center.answer_authorization(true, None);
        f.main.run();
        let log = f.center.log();
        assert_eq!(log, [format!("add {retained}: Zerus: fixture")]);
        f.center.answer_add(&retained, None);
        f.main.run();
        assert!(f.agent_failed.lock().unwrap().is_empty());
        assert!(f.update_failed.lock().unwrap().is_empty());
        assert_eq!(f.close().wait_until(Instant::now()), Observation::Idle);
    }
}
#[test]
fn closing_agent_fences_its_late_callback_but_update_posts_and_withdrawals_continue() {
    for update_first in [false, true] {
        let f = Split::new();
        f.post_both(update_first);
        assert_eq!(f.center.log(), ["authorize"]);
        let closed = f.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        assert_eq!(
            f.pair.agent.deliver(&[], &[notice(&agent_token())]),
            Err(Refused::Accounting(AccountingRefused::Closed))
        );
        f.center.answer_authorization(true, None);
        assert_eq!(
            closed.wait_until(Instant::now()),
            Observation::Pending,
            "agent Hop still queued"
        );
        f.main.run();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
        assert_eq!(f.center.log(), [format!("add {UPDATE}: Zerus: fixture")]);
        f.center.answer_add(UPDATE, None);
        f.main.run();
        f.pair.updates.deliver(&[UPDATE.into()], &[]).unwrap();
        assert_eq!(f.center.log(), [format!("remove {UPDATE}")]);
        f.pair.updates.deliver(&[], &[notice(UPDATE)]).unwrap();
        assert_eq!(f.center.log(), ["authorize"]);
        f.center.answer_authorization(false, Some("update-only"));
        f.main.run();
        assert!(f.agent_failed.lock().unwrap().is_empty());
        assert_eq!(
            *f.update_failed.lock().unwrap(),
            [(UPDATE.into(), "update-only".into())]
        );
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    }
}
#[test]
fn pending_agent_add_and_its_result_remain_charged_after_close_while_update_result_is_independent()
{
    let f = Split::new();
    f.post_both(false);
    f.center.answer_authorization(true, None);
    f.main.run();
    f.center.log();
    let closed = f.close();
    f.center.answer_add(UPDATE, Some("update failure"));
    f.main.run();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    assert_eq!(
        *f.update_failed.lock().unwrap(),
        [(UPDATE.into(), "update failure".into())]
    );
    f.center
        .answer_add(&agent_token(), Some("late agent failure"));
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    f.main.run();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    assert!(
        f.agent_failed.lock().unwrap().is_empty(),
        "closed agent cannot send a late failure into frozen Desk"
    );
    assert!(f.center.log().is_empty());
}
#[test]
fn abandoned_native_authorization_or_agent_hop_cannot_look_drained() {
    for drop_hop in [false, true] {
        let f = Split::new();
        f.post_both(false);
        f.center.log();
        let closed = f.close();
        if drop_hop {
            f.center.answer_authorization(true, None);
            f.main.0.lock().unwrap().clear();
        } else {
            f.center.calls().authorizations.clear();
        }
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        // An unknown shared check is not permission to start another native
        // request. The original per-owner Delivery remains in checking state.
        f.pair
            .updates
            .deliver(&[], &[notice("zerus-update:v2:dev:124")])
            .unwrap();
        assert!(f.center.log().is_empty());
    }
}

#[test]
fn an_immediate_permission_reply_cannot_release_its_still_running_native_call() {
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
    let (pair, control) = Partitioned::new_accounted(
        Box::new(Blocking {
            entered,
            release: Mutex::new(released),
        }),
        Arc::new(|work| work()),
        Box::new(|_, _| panic!("agent failure")),
        Box::new(|_, _| panic!("update failure")),
        gate.admit(Role::NativePosting).unwrap(),
    )
    .unwrap();
    let worker = thread::spawn(move || pair.agent.deliver(&[], &[notice(&agent_token())]));
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    control.close().unwrap();
    let closed = gate.close();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
}
