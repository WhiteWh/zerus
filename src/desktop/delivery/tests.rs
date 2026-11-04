use super::*;
use crate::desktop::{feed, store::Location};
use std::sync::{atomic::AtomicUsize, Mutex};
use zerus_watch_protocol::CatalogItem;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn committed_policy_receipt_remains_readable_without_a_runtime() {
    let mut f = Fixture::new(Arc::new(|_, _, _, _| panic!("no executor needed")));
    f.active();
    let receipt = f.enabled(true);
    let location = f.location;
    drop(f.runtime);
    drop(f.store);
    let store = Store::open(&location, &id()).unwrap();
    let response = result_request(
        &store,
        &json!({"op":"delivery_result",
        "request_id":id(),"original_request_id":receipt["request_id"]}),
    );
    assert_eq!(response["result"], receipt);
    assert_eq!(response["found"], true);
    assert_eq!(response["durable"], false);
    assert_eq!(
        result_request(
            &store,
            &json!({"op":"automatic_approvals",
        "request_id":id(),"enabled":false})
        )["accepted"],
        false
    );
    assert!(state::policy(&store.db).unwrap().enabled);
}
struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
    store: Store,
    runtime: Runtime,
    feed: feed::Broker,
    producer: Arc<feed::Producer>,
    generation: u64,
}
impl Fixture {
    fn passive_projection(&self) -> Value {
        crate::desktop::projection(
            &self.location,
            &self.store,
            &self.runtime.epoch,
            0,
            &Value::Null,
            &self.feed,
            &crate::desktop::catalog_read::Reader::default(),
        )
    }
    fn projection(&self) -> Value {
        let mut value = self.passive_projection();
        self.runtime.decorate(&mut value);
        value
    }
    fn new(run: Runner) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = id();
        let store = Store::open(&location, &epoch).unwrap();
        let mut feed = feed::Broker::new(&epoch);
        let producer = feed.claim_internal().unwrap();
        let runtime = Runtime::new(
            &epoch,
            temp.path().canonicalize().unwrap().join("approval-audit"),
            run,
        );
        Self {
            _temp: temp,
            location,
            store,
            runtime,
            feed,
            producer,
            generation: 0,
        }
    }
    fn active(&mut self) {
        zerus_desktop_ledger::authorize(&mut self.store.db, &id(), &self.runtime.epoch).unwrap();
        self.runtime.poll(&mut self.store);
    }
    fn enabled(&mut self, enabled: bool) -> Value {
        self.native_policy(&json!({"op":"automatic_approvals","request_id":id(),"enabled":enabled}))
    }
    fn native_policy(&mut self, value: &Value) -> Value {
        self.runtime.frontend_request(
            &mut self.store,
            crate::desktop::ui_authority::Validated::fixture(value),
        )
    }
    fn source(&mut self, row: Value, native: bool) {
        self.generation += 1;
        for packet in [
            feed::Packet::LegacyBegin {
                computer: String::new(),
                generation: self.generation,
                metadata: json!({"machine_id":"fixture"}),
                rows: 1,
                pages: 1,
            },
            feed::Packet::LegacyPage {
                computer: String::new(),
                generation: self.generation,
                index: 0,
                items: vec![CatalogItem {
                    key: "run:run".into(),
                    row,
                }],
            },
            feed::Packet::LegacyComplete {
                computer: String::new(),
                generation: self.generation,
            },
        ] {
            self.feed
                .apply(&mut self.store, &self.producer, &packet)
                .unwrap();
        }
        if native {
            self.runtime.sources.native_fixture(&self.store, "");
        }
    }
    fn tick_until(&mut self, predicate: impl Fn(&Runtime) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            self.runtime.poll(&mut self.store);
            if predicate(&self.runtime) {
                break;
            }
            assert!(
                Instant::now() < end,
                "delivery did not settle: {}",
                self.runtime.status()
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn scan(&mut self) {
        self.runtime.scan_at = Instant::now();
        self.runtime.poll(&mut self.store);
    }
    fn journal_status(&self) -> Option<String> {
        self.runtime
            .journal
            .as_ref()?
            .query_row("SELECT status FROM attempts LIMIT 1", [], |r| r.get(0))
            .optional()
            .unwrap()
    }
}
fn quiet() -> Runner {
    Arc::new(|_, _, _, _| panic!("unexpected native work"))
}
fn row() -> Value {
    json!({"name":"claude/example/a","cmd":"claude","created":123,"tracked":true,
        "state":"running","process_state":"running","phase":"approval","run_id":"run",
        "conversation_id":"conversation","reply_id":"reply","attention_signature":"attention"})
}
fn mark() -> reads::ReadMark {
    reads::ReadMark {
        computer: String::new(),
        name: "claude/example/a".into(),
        run_id: "run".into(),
        conversation_id: "conversation".into(),
        reply_id: Some("reply".into()),
        attention_signature: Some("attention".into()),
    }
}
fn question() -> Value {
    json!({"question_id":"q","question_hash":"a".repeat(64),"run_id":"run","conversation_id":"conversation",
        "source":"claude_tool_approval","approval":true,"can_answer":true,"approval_choices":true,
        "answer_transport":"claude_tui","questions":[{"id":"approval","options":[
            {"id":"yes","label":"Yes","scope":"once"},{"id":"always","label":"Always allow","scope":"always"}]}]})
}
fn inspected(q: Value) -> Exit {
    Exit::Code {
        code: 0,
        stderr: String::new(),
        stdout: serde_json::to_vec(&json!({"runtime_state":"live","process_state":"running",
        "run_id":"run","conversation_id":"conversation","pending_questions":[q]}))
        .unwrap(),
    }
}
fn reserve_read(f: &mut Fixture) -> reads::Delivery {
    reads::enqueue(&mut f.store.db, &id(), &mark(), now()).unwrap();
    let writer = f.runtime.writer_epoch(&f.store).unwrap().unwrap();
    reads::reserve(&mut f.store.db, &writer, &f.runtime.epoch, &id(), now())
        .unwrap()
        .unwrap()
}

#[test]
fn policy_defaults_off_and_replayed_receipts_never_reapply_old_settings() {
    let mut f = Fixture::new(quiet());
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    f.active();
    let first = json!({"op":"automatic_approvals","request_id":id(),"enabled":true});
    let receipt = f.native_policy(&first);
    assert_eq!(receipt["durable"], true);
    assert_eq!(f.enabled(false)["accepted"], true);
    assert_eq!(f.native_policy(&first), receipt);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    let result=f.runtime.request(&mut f.store,&json!({"op":"delivery_result","request_id":id(),"original_request_id":first["request_id"]}));
    assert_eq!(result["result"], receipt);
    let next = id();
    drop(f.store);
    let store = Store::open(&f.location, &next).unwrap();
    assert!(!state::policy(&store.db).unwrap().enabled);
    assert!(zerus_desktop_ledger::status(&store.db).unwrap()["revoked"]
        .as_bool()
        .unwrap());
}

#[test]
fn generic_runtime_policy_requests_cannot_bypass_the_native_permit_type() {
    let mut f = Fixture::new(quiet());
    for enabled in [true, false] {
        let result = f.runtime.request(
            &mut f.store,
            &json!({"op":"automatic_approvals",
            "request_id":id(),"enabled":enabled}),
        );
        assert_eq!(result["accepted"], false);
        assert_eq!(result["code"], "frontend_authentication_required");
    }
    assert!(!state::policy(&f.store.db).unwrap().enabled);
}

#[test]
fn request_types_cross_operation_ids_and_failed_commit_refuse() {
    let mut f = Fixture::new(quiet());
    for enabled in [Value::Null, json!(1), json!("true")] {
        assert_eq!(
            f.runtime.request(
                &mut f.store,
                &json!({"op":"automatic_approvals","request_id":id(),"enabled":enabled})
            )["accepted"],
            false
        );
    }
    let request = id();
    let read = json!({"op":"read_intent","request_id":request,"mark":mark()});
    let receipt = f.runtime.request(&mut f.store, &read);
    assert_eq!(receipt["durable"], true);
    assert_eq!(f.runtime.request(&mut f.store, &read), receipt);
    assert_eq!(
        f.runtime.request(
            &mut f.store,
            &json!({"op":"automatic_approvals","request_id":request,"enabled":true})
        )["accepted"],
        false
    );
    let request = id();
    assert!(state::set_policy(&mut f.store.db, &request, false).is_ok());
    assert!(state::enqueue(&mut f.store.db, &request, &mark(), now()).is_err());
    f.active();
    f.store.db.execute_batch("CREATE TRIGGER fail_policy BEFORE INSERT ON desktop_delivery_requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(f.enabled(true)["accepted"], false);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    f.store
        .db
        .execute_batch("DROP TRIGGER fail_policy;")
        .unwrap();
    assert_eq!(f.enabled(false)["accepted"], true);
    assert_eq!(f.enabled(true)["accepted"], true);
    assert!(f.runtime.approval_error.is_none());
}

#[test]
fn actual_root_projection_is_consumed_by_the_client_without_ready_json_fixtures() {
    use zerus_client_core::desktop::service::State;
    let mut f = Fixture::new(quiet());
    let mut client = State::new(f.location.configuration.clone()).unwrap();
    let mut output = Vec::new();
    let mut check = |case: &str, f: &Fixture, reads: bool, approvals: bool| {
        let value = f.projection();
        client.status(&value).unwrap();
        assert!(client.snapshot().connected);
        assert_eq!(client.snapshot().component_ready("reads"), reads, "{case}");
        assert_eq!(
            client.snapshot().component_ready("approvals"),
            approvals,
            "{case}"
        );
        output.push(json!({"case":case,"status":value}));
    };
    check("awaiting_handoff", &f, false, false);
    f.active();
    check("disabled_but_ready", &f, true, true);
    assert_eq!(
        f.projection()["lifecycle"]["background"]["approvals"]["code"],
        "disabled"
    );
    assert_eq!(
        f.projection()["lifecycle"]["background"]["approvals"]["enabled"],
        false
    );
    assert_eq!(f.enabled(true)["durable"], true);
    check("enabled", &f, true, true);
    f.runtime.approval_error = Some("fixture_approval_unavailable");
    check("approval_degraded", &f, true, false);
    assert_eq!(
        f.projection()["automatic_approvals"],
        true,
        "durable opt-in is not worker readiness"
    );
    assert_eq!(f.enabled(false)["durable"], true);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    check("disabled_while_degraded", &f, true, false);
    f.runtime.approval_error = None;
    f.runtime.read_error = Some("fixture_read_unavailable");
    check("read_degraded", &f, false, true);
    f.runtime.request_stop();
    check("drained", &f, false, false);
    assert_eq!(
        f.projection()["lifecycle"]["background"]["approvals"]["code"],
        "drained"
    );
    println!(
        "ROOT_DELIVERY_STATUS {}",
        serde_json::to_string(&output).unwrap()
    );
}

#[test]
fn enable_requires_current_writer_even_when_cached_projection_was_ready() {
    let mut f = Fixture::new(quiet());
    assert_eq!(f.enabled(true)["accepted"], false);
    f.active();
    assert_eq!(
        f.projection()["lifecycle"]["background"]["approvals"]["ready"],
        true
    );
    zerus_desktop_ledger::revoke(&f.store.db, "fixture").unwrap();
    // No intervening poll: the display cache must grant no authority.
    assert_eq!(f.enabled(true)["accepted"], false);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    assert_eq!(f.enabled(false)["durable"], true);
    f.runtime.request_stop();
    assert_eq!(f.enabled(false)["durable"], true);
}

#[test]
fn degraded_runtime_returns_original_receipt_without_reapplying_old_enable() {
    let mut f = Fixture::new(quiet());
    f.active();
    let original = json!({"op":"automatic_approvals","request_id":id(),"enabled":true});
    let receipt = f.native_policy(&original);
    assert_eq!(receipt["durable"], true);
    assert_eq!(f.enabled(false)["durable"], true);
    f.runtime.request_stop();
    assert_eq!(f.native_policy(&original), receipt);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    assert_eq!(f.enabled(true)["accepted"], false);
}

#[test]
fn passive_disable_requires_typed_authentication_and_an_actual_store_commit() {
    let mut f = Fixture::new(quiet());
    state::set_policy(&mut f.store.db, &id(), true).unwrap();
    let passive = f.passive_projection();
    assert_eq!(passive["automatic_approvals"], true);
    assert_eq!(
        passive["lifecycle"]["background"]["approvals"]["enabled"],
        true
    );
    assert_eq!(
        passive["lifecycle"]["background"]["approvals"]["ready"],
        false
    );
    let mut client =
        zerus_client_core::desktop::service::State::new(f.location.configuration.clone()).unwrap();
    client.status(&passive).unwrap();
    assert_eq!(client.snapshot().automatic_approvals, Some(true));
    assert!(!client.snapshot().component_ready("approvals"));
    let request = |enabled| json!({"op":"automatic_approvals","request_id":id(),"enabled":enabled});
    let apply = |store: &mut Store, request: &Value| {
        passive_frontend_request(
            store,
            crate::desktop::ui_authority::Validated::fixture(request),
        )
    };
    assert_eq!(apply(&mut f.store, &request(true))["accepted"], false);
    assert!(state::policy(&f.store.db).unwrap().enabled);
    f.store.db.execute_batch("CREATE TRIGGER deny_disable BEFORE UPDATE ON desktop_delivery_policy BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let original = request(false);
    assert_eq!(apply(&mut f.store, &original)["durable"], false);
    assert!(state::policy(&f.store.db).unwrap().enabled);
    f.store
        .db
        .execute_batch("DROP TRIGGER deny_disable;")
        .unwrap();
    let receipt = apply(&mut f.store, &original);
    assert_eq!(receipt["durable"], true);
    assert_eq!(apply(&mut f.store, &original), receipt);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    assert_eq!(
        f.runtime.request(&mut f.store, &request(false))["code"],
        "frontend_authentication_required"
    );
}

#[test]
fn display_feed_cannot_admit_delivery_or_retire_a_read() {
    let mut f = Fixture::new(quiet());
    f.active();
    reads::enqueue(&mut f.store.db, &id(), &mark(), now()).unwrap();
    let mut seen = row();
    seen["read"] = json!({"conversation_id":"conversation","reply_id":"reply","attention_signature":"attention"});
    f.source(seen, false);
    assert!(f.runtime.sources.eligible(&f.store).unwrap().is_empty());
    assert!(f.runtime.sources.page(&mut f.store).unwrap().is_empty());
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 1);
    f.runtime.sources.native_fixture(&f.store, "");
    f.runtime.sources.observe_reads(&mut f.store).unwrap();
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
}

#[test]
fn failed_read_worker_retains_the_original_ticket_until_finish_commits() {
    let mut f = Fixture::new(quiet());
    f.active();
    let delivery = reserve_read(&mut f);
    let lease = f.runtime.lease(Kind::Read).unwrap();
    f.runtime
        .jobs
        .push(Job::not_started(Phase::Read(delivery.clone()), lease));
    f.store.db.execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE ON desktop_read_attempts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    f.runtime.poll(&mut f.store);
    assert_eq!(f.runtime.jobs.len(), 1);
    assert_eq!(reads::status(&f.store.db).unwrap()["in_flight"], 1);
    f.store
        .db
        .execute_batch("DROP TRIGGER fail_finish;")
        .unwrap();
    f.runtime.poll(&mut f.store);
    assert!(f.runtime.jobs.is_empty());
    assert_eq!(reads::status(&f.store.db).unwrap()["in_flight"], 0);
    assert!(f.runtime.read_error.is_none());
    let result: String = f
        .store
        .db
        .query_row(
            "SELECT result FROM desktop_read_attempts WHERE id=?1",
            [delivery.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<reads::Outcome>(&result).unwrap(),
        reads::Outcome::NotSent
    );
}

#[test]
fn cancelled_reserved_answer_retries_only_its_journal_finish() {
    cancelled_answer_finish_with_due_scan(false);
}

#[test]
fn cancelled_reserved_answer_finish_survives_stop_without_new_inspection() {
    cancelled_answer_finish_with_due_scan(true);
}

fn cancelled_answer_finish_with_due_scan(stopping: bool) {
    let inspections = Arc::new(AtomicUsize::new(0));
    let answers = Arc::new(AtomicUsize::new(0));
    let run: Runner = {
        let inspections = inspections.clone();
        let answers = answers.clone();
        Arc::new(move |inv, _, _, _| {
            if inv.args.iter().any(|arg| arg == "inspect") {
                inspections.fetch_add(1, Ordering::SeqCst);
                inspected(question())
            } else {
                answers.fetch_add(1, Ordering::SeqCst);
                panic!("a cancelled Answer must never be replayed");
            }
        })
    };
    let mut f = Fixture::new(run);
    f.active();
    f.enabled(true);
    f.source(row(), true);
    let candidate = f.runtime.sources.page(&mut f.store).unwrap().remove(0);
    let mut journal = approval_log::open(&f.runtime.approval_root).unwrap();
    let r = approval_log::begin(
        &mut journal,
        &json!({"host":"local","session":mark().name,"provider":"claude","question":question()}),
    )
    .unwrap();
    let reserved = policy::reserved(&r, "claude", &question())
        .unwrap()
        .unwrap();
    let original_id = reserved.id;
    let original_request = reserved.request.clone();
    journal.execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE ON attempts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    f.runtime.journal = Some(journal);
    let lease = f.runtime.lease(Kind::Approval).unwrap();
    f.runtime.jobs.push(Job::not_started(
        Phase::Answer {
            candidate,
            audit: Audit {
                id: reserved.id,
                request: reserved.request,
            },
        },
        lease,
    ));
    if stopping {
        f.runtime.request_stop();
    }
    // Poll also discovers new work. Make that boundary deterministic instead
    // of depending on setup finishing within the one-second scan interval.
    f.runtime.scan_at = Instant::now();
    f.runtime.poll(&mut f.store);
    assert_eq!(f.journal_status().as_deref(), Some("prepared"));
    assert_eq!(f.runtime.jobs.len(), 1);
    assert!(
        matches!(&f.runtime.jobs[0].phase, Phase::Answer { audit, .. }
        if audit.id == original_id && audit.request == original_request)
    );
    assert_eq!(
        f.runtime.approval_error,
        Some("approval_receipt_uncommitted")
    );
    assert_eq!(inspections.load(Ordering::SeqCst), 0);
    assert_eq!(answers.load(Ordering::SeqCst), 0);
    f.runtime
        .journal
        .as_ref()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_finish;")
        .unwrap();
    f.runtime.scan_at = Instant::now();
    f.runtime.poll(&mut f.store);
    assert_eq!(f.journal_status().as_deref(), Some("cancelled"));
    assert!(f.runtime.approval_error.is_none());
    if stopping {
        assert!(f.runtime.drained());
    } else {
        // The original Answer is finished. Its removal and recovered journal
        // permit a FRESH Inspect in this same poll, never an Answer retry.
        assert_eq!(f.runtime.jobs.len(), 1);
        assert!(matches!(&f.runtime.jobs[0].phase, Phase::Inspect(_)));
        // Finish this worker with policy/writer admission still active. The
        // same question must find its cancelled reservation, not send Answer.
        // Keep later discovery outside tick_until's bounded five-second wait.
        f.runtime.scan_at = Instant::now() + Duration::from_secs(60);
        f.tick_until(|runtime| runtime.jobs.is_empty());
        f.runtime.request_stop();
        assert!(f.runtime.drained());
    }
    let journal = f.runtime.journal.as_ref().unwrap();
    let entry: (i64, String, String, String) = journal
        .query_row(
            "SELECT id,request_id,status,detail FROM attempts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        entry,
        (
            original_id,
            original_request,
            "cancelled".into(),
            policy::CANCELLED.into()
        )
    );
    assert_eq!(
        journal
            .query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(inspections.load(Ordering::SeqCst), usize::from(!stopping));
    assert_eq!(answers.load(Ordering::SeqCst), 0);
}

#[test]
fn approval_reservation_precedes_answer_and_unknown_never_replays_after_restart() {
    let answers = Arc::new(AtomicUsize::new(0));
    let count = answers.clone();
    let journal_path = Arc::new(Mutex::new(PathBuf::new()));
    let path = journal_path.clone();
    let run: Runner = Arc::new(move |inv, _, _, _| {
        if inv.args.iter().any(|a| a == "inspect") {
            return inspected(question());
        }
        let audit = Connection::open(path.lock().unwrap().as_path()).unwrap();
        let (status, request): (String, String) = audit
            .query_row("SELECT status,request_id FROM attempts LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "prepared");
        assert_eq!(inv.request_id.as_deref(), Some(request.as_str()));
        let input: Value = serde_json::from_slice(inv.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(input["expected_run_id"], "run");
        assert_eq!(input["expected_conversation_id"], "conversation");
        assert_eq!(input["expected_question_hash"], "a".repeat(64));
        count.fetch_add(1, Ordering::SeqCst);
        Exit::TimedOut
    });
    let mut f = Fixture::new(run.clone());
    *journal_path.lock().unwrap() = f.runtime.approval_root.join("journal.sqlite3");
    f.active();
    f.enabled(true);
    f.source(row(), true);
    f.scan();
    f.tick_until(|r| r.jobs.is_empty());
    assert_eq!(answers.load(Ordering::SeqCst), 1);
    assert_eq!(f.journal_status().as_deref(), Some("unknown"));
    let mut replacement = Runtime::new(&f.runtime.epoch, f.runtime.approval_root.clone(), run);
    replacement.sources.native_fixture(&f.store, "");
    f.runtime = replacement;
    f.scan();
    f.tick_until(|r| r.jobs.is_empty());
    assert_eq!(answers.load(Ordering::SeqCst), 1);
    assert_eq!(f.journal_status().as_deref(), Some("unknown"));
}

#[test]
fn cached_question_identity_and_broader_grants_never_reserve_or_answer() {
    for case in 0..5 {
        let run: Runner = Arc::new(move |inv, _, _, _| {
            assert!(inv.args.iter().any(|a| a == "inspect"));
            let mut q = question();
            match case {
                0 => q["run_id"] = json!("stale"),
                1 => q["conversation_id"] = json!("stale"),
                2 => q["trust_request"] = json!(true),
                3 => q["questions"][0]["options"][0]["scope"] = json!("always"),
                _ => q["approval"] = json!(false),
            }
            inspected(q)
        });
        let mut f = Fixture::new(run);
        f.active();
        f.enabled(true);
        f.source(row(), true);
        f.scan();
        f.tick_until(|r| r.jobs.is_empty());
        assert!(f.runtime.journal.is_none());
    }
}

#[test]
fn disabling_during_inspection_cancels_before_reservation() {
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    let run: Runner = Arc::new(move |_, _, _, _| {
        let end = Instant::now() + Duration::from_secs(3);
        while !gate.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        inspected(question())
    });
    let mut f = Fixture::new(run);
    f.active();
    f.enabled(true);
    f.source(row(), true);
    f.scan();
    assert_eq!(f.runtime.jobs.len(), 1);
    assert_eq!(f.enabled(false)["accepted"], true);
    release.store(true, Ordering::Release);
    f.tick_until(|r| r.jobs.is_empty());
    assert!(f.runtime.journal.is_none());
}

#[test]
fn retained_native_resources_hold_slots_and_prevent_a_false_drained_status() {
    let mut f = Fixture::new(quiet());
    let mut resources = Vec::new();
    for _ in 0..READS {
        resources.push(f.runtime.lease(Kind::Read).unwrap());
    }
    assert!(f.runtime.lease(Kind::Read).is_none());
    for _ in 0..APPROVALS {
        resources.push(f.runtime.lease(Kind::Approval).unwrap());
    }
    assert_eq!(f.runtime.status()["retained_slots"], SLOTS);
    assert!(f.runtime.lease(Kind::Approval).is_none());
    f.runtime.request_stop();
    assert!(!f.runtime.drained());
    drop(resources);
    assert!(f.runtime.drained());
}

#[test]
fn read_receipt_does_not_retire_intent_until_native_catalog_confirms_it() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let run: Runner = Arc::new(move |inv, _, _, _| {
        assert!(inv.args.iter().any(|a| a == "read"));
        let input: Value = serde_json::from_slice(inv.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(input["run_id"], "run");
        assert_eq!(input["conversation_id"], "conversation");
        count.fetch_add(1, Ordering::SeqCst);
        Exit::Code {
            code: 0,
            stderr: String::new(),
            stdout: serde_json::to_vec(
                &json!({"ok":true,"applied":{"reply_id":true,"attention_signature":true}}),
            )
            .unwrap(),
        }
    });
    let mut f = Fixture::new(run);
    f.active();
    f.source(row(), true);
    state::enqueue(&mut f.store.db, &id(), &mark(), now()).unwrap();
    f.runtime.poll(&mut f.store);
    f.tick_until(|r| r.jobs.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 1);
    let mut seen = row();
    seen["read"] = json!({"conversation_id":"conversation","reply_id":"reply","attention_signature":"attention"});
    f.source(seen, true);
    f.runtime.sources.observe_reads(&mut f.store).unwrap();
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
}

#[test]
fn imported_frozen_marks_use_the_original_identity_guarded_delivery_path_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let run: Runner = Arc::new(move |inv, _, _, _| {
        assert!(inv.args.iter().any(|a| a == "read"));
        assert!(inv.args.iter().any(|a| a == "claude/example/a"));
        let input: Value = serde_json::from_slice(inv.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(input["run_id"], "run");
        assert_eq!(input["conversation_id"], "conversation");
        assert_eq!(input["reply_id"], "reply");
        assert_eq!(input["attention_signature"], "attention");
        count.fetch_add(1, Ordering::SeqCst);
        Exit::Code {
            code: 0,
            stderr: String::new(),
            stdout: br#"{"ok":true,"applied":{"reply_id":true,"attention_signature":true}}"#
                .to_vec(),
        }
    });
    let mut f = Fixture::new(run);
    let capture = zerus_client_core::desktop::pending_reads::Capture::from_bytes(
        &serde_json::to_vec(&vec![mark()]).unwrap(),
    )
    .unwrap();
    let page = capture.page(0).unwrap();
    let request = || json!({"op":"import_pending_reads","request_id":id(),"page":page});
    assert_eq!(
        crate::desktop::pending_reads::frontend(
            &mut f.store,
            crate::desktop::ui_authority::Validated::fixture(&request())
        )["durable"],
        true
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!state::policy(&f.store.db).unwrap().enabled);
    f.source(row(), true);
    f.active();
    f.tick_until(|r| r.jobs.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 1);
    let mut seen = row();
    seen["read"] = json!({"conversation_id":"conversation","reply_id":"reply","attention_signature":"attention"});
    f.source(seen, true);
    f.runtime.sources.observe_reads(&mut f.store).unwrap();
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
    assert_eq!(
        crate::desktop::pending_reads::frontend(
            &mut f.store,
            crate::desktop::ui_authority::Validated::fixture(&request())
        )["durable"],
        true
    );
    f.scan();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn pending_observation_is_fair_bounded_and_indexed() {
    let mut f = Fixture::new(quiet());
    let m = mark();
    for n in 0..33 {
        let mut old = m.clone();
        old.name = format!("unlisted-{n}");
        reads::enqueue(&mut f.store.db, &id(), &old, now()).unwrap();
    }
    let mut old = m.clone();
    old.run_id = "superseded".into();
    reads::enqueue(&mut f.store.db, &id(), &old, now()).unwrap();
    f.source(row(), true);
    for _ in 0..2 {
        f.runtime.sources.observe_reads(&mut f.store).unwrap();
    }
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 34);
    f.runtime.sources.observe_reads(&mut f.store).unwrap();
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 33);
    let plan:String=f.store.db.query_row("EXPLAIN QUERY PLAN SELECT json FROM desktop_catalog_rows WHERE generation=1 AND CASE WHEN json_valid(json) THEN json_extract(json,'$.name') END='a' ORDER BY id LIMIT 2",[],|r|r.get(3)).unwrap();
    assert!(plan.contains("desktop_delivery_catalog_name"), "{plan}");
}

#[test]
fn a_valid_inspection_prefix_cannot_release_retained_native_resources() {
    let held = Arc::new(Mutex::new(None));
    let pin = held.clone();
    let run: Runner = Arc::new(move |_, _, _, lease| {
        *pin.lock().unwrap() = Some(lease);
        inspected(question())
    });
    let mut f = Fixture::new(run);
    f.active();
    f.enabled(true);
    f.source(row(), true);
    f.scan();
    f.tick_until(|r| r.jobs.is_empty());
    assert!(f.runtime.journal.is_none());
    assert_eq!(f.runtime.status()["retained_slots"], 1);
    f.runtime.request_stop();
    assert!(!f.runtime.drained());
    held.lock().unwrap().take();
    assert!(f.runtime.drained());
}

#[test]
fn replacing_the_writer_epoch_cancels_an_already_started_inspection() {
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    let run: Runner = Arc::new(move |_, _, _, _| {
        let end = Instant::now() + Duration::from_secs(3);
        while !gate.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        inspected(question())
    });
    let mut f = Fixture::new(run);
    f.active();
    f.enabled(true);
    f.source(row(), true);
    f.scan();
    assert_eq!(f.runtime.jobs.len(), 1);
    zerus_desktop_ledger::authorize(&mut f.store.db, &id(), &f.runtime.epoch).unwrap();
    f.runtime.poll(&mut f.store);
    release.store(true, Ordering::Release);
    f.tick_until(|r| r.jobs.is_empty());
    assert!(f.runtime.journal.is_none());
}

#[test]
fn persistence_retry_does_not_upgrade_an_original_incomplete_read_receipt() {
    let held = Arc::new(Mutex::new(None));
    let pin = held.clone();
    let run: Runner = Arc::new(move |_, _, _, lease| {
        *pin.lock().unwrap() = Some(lease);
        Exit::Code {
            code: 0,
            stderr: String::new(),
            stdout: serde_json::to_vec(&json!({"ok":true,
            "applied":{"reply_id":true,"attention_signature":true}}))
            .unwrap(),
        }
    });
    let mut f = Fixture::new(run);
    f.active();
    f.source(row(), true);
    reads::enqueue(&mut f.store.db, &id(), &mark(), now()).unwrap();
    f.store.db.execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE ON desktop_read_attempts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    f.runtime.poll(&mut f.store);
    f.tick_until(|r| r.jobs.first().is_some_and(|job| job.result.is_some()));
    held.lock().unwrap().take();
    f.store
        .db
        .execute_batch("DROP TRIGGER fail_finish;")
        .unwrap();
    f.runtime.poll(&mut f.store);
    assert!(f.runtime.jobs.is_empty());
    let result: String = f
        .store
        .db
        .query_row(
            "SELECT result FROM desktop_read_attempts LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<reads::Outcome>(&result).unwrap(),
        reads::Outcome::Failed
    );
}
