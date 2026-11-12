use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::json;
use zerus_client_core::desktop::creation_clone::{self as wire, Intent, Phase, Verified};

fn original(configuration: &str, epoch: &str) -> Verified {
    let creation = uuid::Uuid::new_v4().to_string();
    let clone_id = uuid::Uuid::new_v4().to_string();
    let value = json!({"id":creation,"creation_schema":1,"created":1,"revision":4,
        "cmd":"codex","tag":"example","host":"","state":"cloning","repository":true,
        "parent":"/srv","target":"/srv/repo","repository_name":"repo",
        "url":"https://example.invalid/repo.git","clone_id":clone_id,
        "expected_machine":"22222222-2222-4222-8222-222222222222"});
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        creation_id: creation,
        clone_request_id: clone_id,
        revision: 4,
        original: value.to_string(),
    }
    .verify()
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, Location, Store, String) {
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    let store = Store::open(&location, &epoch).unwrap();
    (temp, location, store, epoch)
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_independent_clone_leaf() {
    assert_eq!(
        SCHEMA - 1,
        16,
        "rebase this test onto the actual merge-order predecessor"
    );
    let (_temp, location, store, _) = fixture();
    crate::desktop::cache_maintenance::remove_from_downgraded_fixture(&store.db);
    store.db.execute_batch("DROP TABLE desktop_creation_clones; DROP TABLE desktop_creation_clone_budget; PRAGMA user_version=16;").unwrap();
    let before: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Existing task bindings remain byte-compatible and are not creation rows.
    let task_sql: String = store
        .db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='desktop_operations'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM sqlite_schema WHERE type='table'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        // creation17, cache18, attention20, worktree23, launch24, termination
        // outcome25, task message26, task message input27, deferred send28,
        // deferred answer29 and compact-to-send30 tables, and the question
        // route leaves' (zerus-sa9qn.8, .32 and .50).
        before + 2 + 4 + 2 + 2 + 2 + 1 + 2 + 2 + 2 + 2 + 2 + 2 + 2 + 1 + 2
    );
    assert_eq!(
        store
            .db
            .query_row::<String, _, _>(
                "SELECT sql FROM sqlite_schema WHERE name='desktop_operations'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        task_sql
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
}

#[test]
fn shared_resource_total_counts_task_cache_and_creation_exactly_once() {
    use zerus_desktop_ledger::operations::{self as task, cache};
    let (_temp, location, mut store, epoch) = fixture();
    let writer = uuid::Uuid::new_v4().to_string();
    zerus_desktop_ledger::authorize(&mut store.db, &writer, &epoch).unwrap();
    let source: serde_json::Value = serde_json::from_str(include_str!(
        "../../../client/fixtures/desktop-operation-port-v1.json"
    ))
    .unwrap();
    let binding: task::Binding = serde_json::from_value(source["binding"].clone()).unwrap();
    let admitted = task::admit(
        &mut store.db,
        &task::Admission {
            admission_id: &uuid::Uuid::new_v4().to_string(),
            binding: &binding,
            raw_record: source["raw_record"].as_str().unwrap().as_bytes(),
            retained_input: source["retained_input"].as_str().unwrap().as_bytes(),
            provenance: &json!({"fixture":"original-resource-accounting"}),
            writer_epoch: &writer,
            service_epoch: &epoch,
        },
    )
    .unwrap();
    task::reserve(
        &mut store.db,
        &task::Reserve {
            operation_id: admitted.operation_id,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: admitted.revision,
            writer_epoch: writer.clone(),
            service_epoch: epoch.clone(),
            lease: uuid::Uuid::new_v4().to_string(),
        },
    )
    .unwrap();
    assert_eq!(store.operation_undrained().unwrap(), 1);
    let tx = store.db.transaction().unwrap();
    cache::begin(
        &tx,
        &cache::Original {
            preparation_id: uuid::Uuid::new_v4().to_string(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            configuration: location.configuration.clone(),
            root: "/srv/example/cache".into(),
            limit: 1,
            defer_ms: 0,
            policy_change: true,
            revision: 0,
            service_epoch: epoch.clone(),
            writer_epoch: writer.clone(),
            lease: uuid::Uuid::new_v4().to_string(),
        },
        &"f".repeat(64),
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(store.operation_undrained().unwrap(), 2);
    let tx = store.db.transaction().unwrap();
    ledger::admit(&tx, &original(&location.configuration, &epoch), &writer, 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(task::totals(&store.db).unwrap().undrained, 1);
    assert_eq!(cache::undrained(&store.db).unwrap(), 1);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    assert_eq!(
        crate::desktop::store::operation_undrained(&store.db).unwrap(),
        3
    );
    assert_eq!(store.operation_undrained().unwrap(), 3);
    assert_eq!(store.operation_status()["undrained"], 3);
    assert_eq!(store.operation_status()["cache_undrained"], 1);
    // Revoked policy alone cannot hide any original per-kind resource.
    store
        .db
        .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        crate::desktop::store::writer_active(&location).unwrap(),
        Some(true)
    );
}

#[test]
fn admission_is_immutable_and_same_id_returns_only_its_original_receipt() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::admit(&tx, &verified, &writer, 1).unwrap();
    assert_eq!(row.receipt.phase, Phase::Queued);
    tx.commit().unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let again = ledger::admit(&tx, &verified, &writer, 2).unwrap();
    assert_eq!(again.receipt, row.receipt);
    tx.commit().unwrap();
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    assert_eq!(store.operation_undrained().unwrap(), 1);
    assert_eq!(store.operation_status()["undrained"], 1);
    assert_eq!(
        crate::desktop::store::writer_active(&location).unwrap(),
        Some(true)
    );
    assert!(store
        .db
        .execute("UPDATE desktop_creation_clones SET source='{}'", [])
        .is_err());
    assert!(store
        .db
        .execute("DELETE FROM desktop_creation_clones", [])
        .is_err());
    let mut intent = verified.intent;
    let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
    value["opaque_future"] = json!("changed original");
    intent.original = value.to_string();
    let changed = intent.verify().unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &changed, &writer, 3).is_err());
    drop(tx);
    assert_eq!(
        ledger::load(&store.db, &changed.plan.clone_request_id)
            .unwrap()
            .unwrap()
            .receipt
            .binding,
        row.receipt.binding
    );
}

#[test]
fn capacity_refusal_is_atomic_and_retirement_never_rearms_a_queued_original() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    store
        .db
        .execute(
            "UPDATE desktop_creation_clone_budget SET bytes=16777216",
            [],
        )
        .unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &verified, &writer, 1).is_err());
    drop(tx);
    assert!(ledger::load(&store.db, &verified.plan.clone_request_id)
        .unwrap()
        .is_none());
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    store
        .db
        .execute("UPDATE desktop_creation_clone_budget SET bytes=0", [])
        .unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::admit(&tx, &verified, &writer, 1).unwrap();
    tx.commit().unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::retire(&tx, &row).unwrap();
    tx.commit().unwrap();
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    assert!(ledger::pending(&store.db).unwrap().is_none());
    assert_eq!(store.operation_undrained().unwrap(), 0);
    assert_eq!(
        crate::desktop::store::writer_active(&location).unwrap(),
        Some(false)
    );
    let retired = ledger::load(&store.db, &verified.plan.clone_request_id)
        .unwrap()
        .unwrap();
    assert_eq!(retired.receipt.phase, Phase::Unknown);
    assert!(retired.receipt.drained);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let replay = ledger::admit(&tx, &verified, &writer, 2).unwrap();
    assert_eq!(replay.receipt, retired.receipt);
    assert_eq!(ledger::undrained(&tx).unwrap(), 0);
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn only_the_original_acquired_custody_lock_settles_a_clone_result() {
    use crate::desktop::custody;
    use zerus_desktop_ledger::operations::{custody::Record, Ticket};
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::admit(&tx, &verified, &writer, 1).unwrap();
    tx.commit().unwrap();
    let ticket = Ticket {
        operation_id: verified.binding.operation_id().unwrap(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        revision: 2,
        admission_id: verified.plan.clone_request_id.clone(),
        binding_sha256: verified.binding.fingerprint().unwrap(),
        writer_epoch: writer,
        service_epoch: epoch,
        lease: uuid::Uuid::new_v4().to_string(),
    };
    // This is a storage/kernel custody control, not native execution admission.
    let (prepared, held) = custody::prepare(&location, &ticket).unwrap();
    let record = Record {
        prepared,
        helper: None,
    };
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::reserve(&tx, &row, &ticket, &record).unwrap();
    tx.commit().unwrap();
    let row = ledger::pending(&store.db).unwrap().unwrap();
    assert_eq!(row.receipt.phase, Phase::Reserved);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::running(&tx, &row).unwrap();
    tx.commit().unwrap();
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let repository = json!({"status":"cloned","request_id":verified.plan.clone_request_id,
        "fingerprint":verified.plan.repository_fingerprint().unwrap(),"name":"repo","parent":"/srv","path":"/srv/repo"});
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::result(&tx, &row, Some(repository), "awaiting_resource_drain").unwrap();
    tx.commit().unwrap();
    let row = ledger::pending(&store.db).unwrap().unwrap();
    assert!(row.receipt.result(&verified.plan).is_none());
    let observation = custody::probe(&location, &record);
    assert!(matches!(observation, custody::Observation::Held));
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::drained(tx, row, observation).is_err());
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    drop(held);
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let observation = custody::probe(&location, &record);
    assert!(matches!(observation, custody::Observation::Released { .. }));
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let settled = ledger::drained(tx, row, observation).unwrap();
    assert_eq!(settled.phase, Phase::Cloned);
    assert_eq!(settled.result(&verified.plan).unwrap()["status"], "cloned");
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    assert!(ledger::pending(&store.db).unwrap().is_none());
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let old = ledger::admit(&tx, &verified, &uuid::Uuid::new_v4().to_string(), 2).unwrap();
    assert_eq!(old.receipt, settled);
    assert!(ledger::reserve(&tx, &old, &ticket, &record).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod actor {
    use super::*;
    use crate::{config::Config, desktop::ui_authority, state::repositories::Step};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    struct Fixture {
        _temp: tempfile::TempDir,
        location: Location,
        store: Store,
        runtime: runtime::Runtime,
        exclusion: Arc<crate::desktop::store::HandoffLock>,
        epoch: String,
        config: Config,
        owner: Arc<crate::platform::FileLock>,
    }
    impl Fixture {
        fn new(
            execute: impl Fn(
                    &wire::Plan,
                    &crate::state::repositories::Control<'_>,
                ) -> Result<serde_json::Value, String>
                + Send
                + Sync
                + 'static,
        ) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let config = Config {
                home: root.to_str().unwrap().into(),
                dir: root.join("config"),
                state: root.join("state"),
                selves: vec![],
                peers: vec![],
                base_peers: vec![],
                peers_from_environment: false,
                machines: Default::default(),
                tab: false,
                tab_colors: String::new(),
            };
            std::fs::create_dir(&config.dir).unwrap();
            let location = Location::new(&config).unwrap();
            location.prepare().unwrap();
            let epoch = uuid::Uuid::new_v4().to_string();
            let mut store = Store::open(&location, &epoch).unwrap();
            zerus_desktop_ledger::authorize(
                &mut store.db,
                &uuid::Uuid::new_v4().to_string(),
                &epoch,
            )
            .unwrap();
            let owner = Arc::new(location.lock().unwrap());
            let exclusion = Arc::new(location.handoff_lock().unwrap());
            let mut runtime = runtime::Runtime::new(&config, &epoch, owner.clone()).unwrap();
            // This synthetic source/Validated seam tests the actor, not native
            // authentication or an installed frontend authority positive.
            runtime.fixture_executor("22222222-2222-4222-8222-222222222222", Arc::new(execute));
            runtime.poll(&mut store, Some(exclusion.clone()));
            Self {
                _temp: temp,
                location,
                store,
                runtime,
                exclusion,
                epoch,
                config,
                owner,
            }
        }
        fn original(&self) -> Verified {
            let mut intent = super::original(&self.location.configuration, &self.epoch).intent;
            let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
            let parent = self._temp.path().canonicalize().unwrap();
            value["parent"] = json!(parent);
            value["target"] = json!(parent.join("repo"));
            intent.original = value.to_string();
            intent.verify().unwrap()
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            let request = zerus_client_core::desktop::ui_requests::Request::CreationClone {
                intent: Box::new(original.intent.clone()),
            };
            serde_json::from_value(self.runtime.frontend(
                &mut self.store,
                ui_authority::Validated::fixture(&json!(request)),
                Some(self.exclusion.clone()),
            ))
            .unwrap()
        }
        fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
            let until = Instant::now() + Duration::from_secs(8);
            while !done(self) {
                assert!(
                    Instant::now() < until,
                    "timed out: {what}, {}",
                    self.runtime.status()
                );
                self.runtime
                    .poll(&mut self.store, Some(self.exclusion.clone()));
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn row(&self, original: &Verified) -> ledger::Row {
            ledger::load(&self.store.db, &original.plan.clone_request_id)
                .unwrap()
                .unwrap()
        }
        fn revoke(&mut self) {
            self.store
                .db
                .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                .unwrap();
            self.runtime
                .poll(&mut self.store, Some(self.exclusion.clone()));
        }
    }
    fn cloned(plan: &wire::Plan) -> serde_json::Value {
        json!({"status":"cloned","request_id":plan.clone_request_id,"fingerprint":plan.repository_fingerprint().unwrap(),
            "name":plan.name,"parent":plan.parent,"path":std::path::Path::new(&plan.parent).join(&plan.name)})
    }
    #[test]
    fn fixed_admitted_plan_finishes_without_gui_and_waits_for_a_real_descendant_lock() {
        use std::process::{Command, Stdio};
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let original_children = Arc::new(std::sync::Mutex::new(Vec::new()));
        let children = original_children.clone();
        let mut f = Fixture::new(move |plan, control| {
            for step in [Step::Reserve, Step::Create, Step::Clone, Step::Verify] {
                (control.before)(step)?;
                count.fetch_add(1, Ordering::SeqCst);
            }
            // A real private helper and its descendant retain the exact
            // close-only lock across exec. Keep the original wait owner too;
            // no orphan fixture is delegated to the container PID 1.
            let mut command = Command::new("python3");
            command.args(["-c", "import os,sys,time\npid=os.fork()\nif pid:\n os.waitpid(pid,0)\n os._exit(0)\nopen(sys.argv[1], 'w').close()\nuntil=time.monotonic()+10\nwhile not os.path.exists(sys.argv[2]) and time.monotonic()<until: time.sleep(.005)\nos._exit(0)"])
                .arg(std::path::Path::new(&plan.parent).join("descendant-held"))
                .arg(std::path::Path::new(&plan.parent).join("descendant-release"))
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            let mut child = {
                let _spawn = crate::platform::spawn_turn();
                control
                    .custody
                    .inherit(&mut command)
                    .map_err(|e| e.to_string())?;
                command.spawn().map_err(|e| e.to_string())?
            };
            children.lock().unwrap().push(std::thread::spawn(move || {
                assert!(child.wait().unwrap().success())
            }));
            Ok(cloned(plan))
        });
        let original = f.original();
        assert!(f.admit(&original).matches(&original.intent));
        // There is deliberately no subsequent GUI proof/subscription.
        f.until("original result and descendant", |f| {
            f.row(&original).receipt.repository.is_some()
                && f._temp.path().join("descendant-held").exists()
        });
        assert_eq!(effects.load(Ordering::SeqCst), 4);
        assert_eq!(f.row(&original).next_step, 4);
        assert!(!f.row(&original).receipt.drained);
        assert!(f.row(&original).receipt.result(&original.plan).is_none());
        assert_eq!(f.store.operation_undrained().unwrap(), 1);
        f.runtime.request_stop();
        for _ in 0..8 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        assert!(!f.runtime.drained());
        std::fs::write(f._temp.path().join("descendant-release"), b"").unwrap();
        f.until("original kernel drain", |f| f.runtime.drained());
        assert_eq!(f.row(&original).receipt.phase, Phase::Cloned);
        assert!(f.row(&original).receipt.result(&original.plan).is_some());
        assert_eq!(f.store.operation_undrained().unwrap(), 0);
        for child in original_children.lock().unwrap().drain(..) {
            child.join().unwrap();
        }
    }
    #[test]
    fn revoke_between_steps_refuses_verify_and_new_admission_without_replaying_clone() {
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let mut f = Fixture::new(move |_, control| {
            for step in [Step::Reserve, Step::Create, Step::Clone] {
                (control.before)(step)?;
                count.fetch_add(1, Ordering::SeqCst);
            }
            let until = Instant::now() + Duration::from_secs(8);
            while !control.cancelled.load(Ordering::Acquire) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(control.cancelled.load(Ordering::Acquire));
            assert!((control.before)(Step::Verify).is_err());
            Err("original cancelled".into())
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("three effects", |_| effects.load(Ordering::SeqCst) == 3);
        f.revoke();
        assert!(!f.admit(&f.original()).accepted);
        f.until("cancelled original custody", |f| {
            f.row(&original).receipt.drained
        });
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
        assert_eq!(f.row(&original).next_step, 3);
        let query = wire::Query::for_plan(
            original.intent.scope.clone(),
            original.intent.creation_id.clone(),
            &original.plan,
        )
        .unwrap();
        let result = super::super::query(
            &f.store,
            &json!({"op":"creation_clone_result","query":query}),
        );
        let result: wire::Lookup = serde_json::from_value(result).unwrap();
        assert!(result.matches(&query));
        assert_eq!(result.receipt.unwrap().phase, Phase::Unknown);
        assert_eq!(effects.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn restart_retires_a_queued_plan_and_never_reconstructs_execution_consent() {
        let mut f = Fixture::new(|_, _| panic!("recovered plan must not execute"));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
        f.runtime.fixture_executor(
            "22222222-2222-4222-8222-222222222222",
            Arc::new(|_, _| panic!("restart must not execute")),
        );
        f.store = Store::open(&f.location, &f.epoch).unwrap();
        zerus_desktop_ledger::authorize(
            &mut f.store.db,
            &uuid::Uuid::new_v4().to_string(),
            &f.epoch,
        )
        .unwrap();
        for _ in 0..5 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
        let row = ledger::load(&f.store.db, &original.plan.clone_request_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.receipt.phase, Phase::Unknown);
        assert!(row.receipt.drained);
        // Even an exact new authenticated duplicate returns its old receipt;
        // it cannot create another live consent or reset the original phase.
        let before = row.receipt;
        let value = f.admit(&original);
        assert_eq!(value.receipt.unwrap(), before);
    }
    #[test]
    fn actor_step_checks_a_revoked_writer_without_worker_cancellation() {
        use std::sync::atomic::AtomicBool;
        let ready = Arc::new(AtomicBool::new(false));
        let resume = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicBool::new(false));
        let (started, release, stopped) = (ready.clone(), resume.clone(), refused.clone());
        let mut f = Fixture::new(move |_, control| {
            (control.before)(Step::Reserve)?;
            (control.before)(Step::Create)?;
            started.store(true, Ordering::Release);
            let until = Instant::now() + Duration::from_secs(8);
            while !release.load(Ordering::Acquire) {
                assert!(Instant::now() < until, "actor fixture was not released");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(!control.cancelled.load(Ordering::Acquire));
            let result = (control.before)(Step::Clone);
            assert!(!control.cancelled.load(Ordering::Acquire));
            stopped.store(result.is_err(), Ordering::Release);
            result?;
            Err("revoked actor incorrectly admitted Clone".into())
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("worker before Clone", |_| ready.load(Ordering::Acquire));
        assert_eq!(f.row(&original).next_step, 2);
        f.store
            .db
            .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
            .unwrap();
        resume.store(true, Ordering::Release);
        let until = Instant::now() + Duration::from_secs(8);
        while !refused.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "actor accepted a revoked step");
            // Deliberately omit refresh: its cancellation must not hide a
            // missing check in the actual BEGIN IMMEDIATE step transaction.
            f.runtime
                .fixture_poll_steps_without_refresh(&mut f.store)
                .unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(f.row(&original).next_step, 2);
        f.until("revoked step original drain", |f| {
            f.row(&original).receipt.drained
        });
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
    }
    #[test]
    fn effect_steps_are_monotone_and_cannot_skip_or_repeat_a_reservation() {
        let mut f = Fixture::new(|_, control| {
            assert!(
                (control.before)(Step::Clone).is_err(),
                "cannot skip reservation"
            );
            (control.before)(Step::Reserve)?;
            assert!(
                (control.before)(Step::Reserve).is_err(),
                "cannot repeat reservation"
            );
            Err("fixed plan stopped".into())
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("step protocol drain", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).next_step, 1);
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
    }
}
/// Reservation accounting controls use only synthetic originals and private Store fixtures.
/// They are ledger/Store tests, not native-agent qualification.
mod reservation_budget {
    use super::*;
    pub(super) fn budget(db: &rusqlite::Connection) -> (i64, i64, i64) {
        db.query_row(
            "SELECT rows,bytes,active FROM desktop_creation_clone_budget WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    fn key(v: &Verified) -> &str {
        &v.plan.clone_request_id
    }
    fn admitted(store: &mut Store, v: &Verified, writer: &str) -> ledger::Row {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let row = ledger::admit(&tx, v, writer, 1).unwrap();
        tx.commit().unwrap();
        row
    }
    fn retired(store: &mut Store, row: &ledger::Row) {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::retire(&tx, row).unwrap();
        tx.commit().unwrap();
    }
    fn original_charge(db: &rusqlite::Connection, id: &str) -> i64 {
        db.query_row("SELECT length(CAST(source AS BLOB))+length(CAST(plan AS BLOB))+37888 FROM desktop_creation_clones WHERE clone_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_creation_clones)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_creation_clones ORDER BY clone_id",
            columns
                .iter()
                .map(|column| format!("typeof({column}),hex(CAST({column} AS BLOB))"))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }
    /// Exact old cumulative charging formula, with complete valid retained originals.
    fn legacy(store: &mut Store, configuration: &str, epoch: &str) -> Vec<Verified> {
        let writer = uuid::Uuid::new_v4().to_string();
        let mut originals = Vec::new();
        let mut bytes = 0_i64;
        loop {
            let mut v = original(configuration, epoch);
            // Raw whitespace is retained and charged, not re-serialized away.
            v.intent.original = format!(" \n{}  ", v.intent.original);
            let v = v.intent.verify().unwrap();
            let charge =
                v.intent.original.len() + serde_json::to_string(&v.plan).unwrap().len() + 37888;
            if bytes + i64::try_from(charge).unwrap() > 16 << 20 {
                assert!((16 << 20) - bytes < i64::try_from(charge).unwrap());
                break;
            }
            let row = admitted(store, &v, &writer);
            bytes += original_charge(&store.db, key(&v));
            retired(store, &row);
            originals.push(v);
            assert!(originals.len() <= 4096);
        }
        assert!(!originals.is_empty());
        store
            .db
            .execute(
                "UPDATE desktop_creation_clone_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
                rusqlite::params![originals.len() as i64, bytes],
            )
            .unwrap();
        originals
    }
    #[test]
    fn exact_legacy_conversion_keeps_every_original_and_duplicate() {
        let (_temp, location, mut store, epoch) = fixture();
        let old = legacy(&mut store, &location.configuration, &epoch);
        let before = retained(&store.db);
        let mut v = original(&location.configuration, &epoch);
        v.intent.original = format!(" \n{}  ", v.intent.original);
        let v = v.intent.verify().unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::admit(&tx, &v, &uuid::Uuid::new_v4().to_string(), 1).is_err());
        drop(tx);
        ledger::startup(&mut store.db).unwrap();
        assert_eq!(budget(&store.db), (0, 0, 0));
        assert_eq!(retained(&store.db), before);
        for v in [&old[0], old.last().unwrap()] {
            let kept = ledger::load(&store.db, key(v)).unwrap().unwrap().receipt;
            assert_eq!(admitted(&mut store, v, &epoch).receipt, kept);
            assert_eq!(budget(&store.db), (0, 0, 0));
        }
        let row = admitted(&mut store, &v, &epoch);
        assert_eq!(
            budget(&store.db),
            (1, original_charge(&store.db, key(&v)), 1)
        );
        retired(&mut store, &row);
        ledger::startup(&mut store.db).unwrap();
        assert_eq!(budget(&store.db), (0, 0, 0));
    }
    #[test]
    fn over_4096_distinct_cycles_retain_history_without_live_charge_or_replay() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let (_temp, location, mut store, epoch) = fixture();
        let mut first = None;
        for _ in 0..4097 {
            let v = original(&location.configuration, &epoch);
            let row = admitted(&mut store, &v, &epoch);
            retired(&mut store, &row);
            assert_eq!(budget(&store.db), (0, 0, 0));
            if first.is_none() {
                first = Some((
                    v.intent.clone().verify().unwrap(),
                    ledger::load(&store.db, key(&v)).unwrap().unwrap().receipt,
                ));
            }
        }
        let (v, receipt) = first.unwrap();
        assert_eq!(admitted(&mut store, &v, &epoch).receipt, receipt);
        assert_eq!(budget(&store.db), (0, 0, 0));
        let steps = Arc::new(AtomicUsize::new(0));
        let counted = steps.clone();
        store.db.progress_handler(
            1,
            Some(move || counted.fetch_add(1, Ordering::Relaxed) > 2000),
        );
        ledger::startup(&mut store.db).unwrap();
        store.db.progress_handler(0, None::<fn() -> bool>);
        assert!(
            steps.load(Ordering::Relaxed) < 2000,
            "matching live accounting must not inspect history"
        );
        assert_eq!(
            ledger::load(&store.db, key(&v)).unwrap().unwrap().receipt,
            receipt
        );
        let status = store.operation_status();
        let leaf = &status["kinds"]["creation_clone"];
        assert_eq!(leaf["history_complete"], false);
        assert!(leaf["rows"].is_null() && leaf["unsettled"].is_null());
        assert!(status["unsettled"].is_null());
        assert_eq!(leaf["undrained"], 0);
        assert_eq!(leaf["original_custody"], 0);
    }
    #[test]
    fn thirty_two_real_reservations_refuse_the_next_without_forged_byte_capacity() {
        let (_temp, location, mut store, epoch) = fixture();
        let mut originals = Vec::new();
        for _ in 0..32 {
            let v = original(&location.configuration, &epoch);
            admitted(&mut store, &v, &epoch);
            originals.push(v);
        }
        let before = retained(&store.db);
        let charged = budget(&store.db);
        assert_eq!((charged.0, charged.2), (32, 32));
        assert!(charged.1 < 16 << 20);
        let v = original(&location.configuration, &epoch);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::admit(&tx, &v, &epoch, 2).is_err());
        drop(tx);
        assert_eq!(budget(&store.db), charged);
        assert_eq!(retained(&store.db), before);
        let row = ledger::load(&store.db, key(&originals[0]))
            .unwrap()
            .unwrap();
        let charge = original_charge(&store.db, key(&originals[0]));
        retired(&mut store, &row);
        assert_eq!(budget(&store.db), (31, charged.1 - charge, 31));
        admitted(&mut store, &v, &epoch);
        assert_eq!(budget(&store.db).2, 32);
    }
    #[test]
    fn legacy_corruption_or_write_and_commit_failures_never_repair_a_prefix() {
        for failure in ["counter", "receipt", "drained", "write", "commit"] {
            let (_temp, location, mut store, epoch) = fixture();
            legacy(&mut store, &location.configuration, &epoch);
            match failure {
                "counter"=>{store.db.execute("UPDATE desktop_creation_clone_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_creation_clones SET receipt='{}' WHERE clone_id=(SELECT clone_id FROM desktop_creation_clones LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_creation_clones SET drained=2 WHERE clone_id=(SELECT clone_id FROM desktop_creation_clones LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_creation_clone_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_creation_clone_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
                _=>unreachable!(),
            }
            let before = retained(&store.db);
            let old = budget(&store.db);
            assert!(ledger::startup(&mut store.db).is_err(), "{failure}");
            assert_eq!(budget(&store.db), old, "{failure}");
            assert_eq!(retained(&store.db), before, "{failure}");
        }
    }
    #[test]
    fn receipt_and_budget_credit_roll_back_together_and_never_credit_twice() {
        for failure in ["write", "commit"] {
            let (_temp, location, mut store, epoch) = fixture();
            let v = original(&location.configuration, &epoch);
            let row = admitted(&mut store, &v, &epoch);
            let before = retained(&store.db);
            let old = budget(&store.db);
            if failure == "write" {
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_creation_clone_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_creation_clone_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
            }
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let result = ledger::retire(&tx, &row);
            if result.is_ok() {
                assert!(tx.commit().is_err());
            } else {
                drop(tx);
            }
            assert_eq!(retained(&store.db), before);
            assert_eq!(budget(&store.db), old);
            store.db.execute_batch("DROP TRIGGER fail_budget;").unwrap();
            retired(&mut store, &row);
            assert_eq!(budget(&store.db), (0, 0, 0));
            let after = retained(&store.db);
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(ledger::retire(&tx, &row).is_err());
            drop(tx);
            assert_eq!(retained(&store.db), after);
            assert_eq!(budget(&store.db), (0, 0, 0));
        }
    }

    #[test]
    fn malformed_live_counter_is_unavailable_and_never_an_extra_reservation() {
        let (_temp, location, mut store, epoch) = fixture();
        store
            .db
            .execute(
                "UPDATE desktop_creation_clone_budget SET rows=1,bytes=1,active=1 WHERE id=1",
                [],
            )
            .unwrap();
        assert!(ledger::undrained(&store.db).is_err());
        assert!(ledger::startup(&mut store.db).is_err());
        let v = original(&location.configuration, &epoch);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::admit(&tx, &v, &epoch, 1).is_err());
        drop(tx);
        assert_eq!(budget(&store.db), (1, 1, 1));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn held_original_custody_keeps_the_entire_charge_until_its_exact_release() {
        let (_temp, location, mut store, epoch) = fixture();
        let v = original(&location.configuration, &epoch);
        let (record, held) = running(&mut store, &location, &v, &epoch);
        let before = budget(&store.db);
        assert_eq!((before.0, before.2), (1, 1));
        assert!(matches!(
            crate::desktop::custody::probe(&location, &record),
            crate::desktop::custody::Observation::Held
        ));
        let row = ledger::load(&store.db, key(&v)).unwrap().unwrap();
        retired(&mut store, &row);
        assert_eq!(
            budget(&store.db),
            before,
            "retiring an original is not its physical release"
        );
        let row = ledger::load(&store.db, key(&v)).unwrap().unwrap();
        assert!(!row.receipt.drained);
        drop(held);
        let observed = crate::desktop::custody::probe(&location, &record);
        assert!(matches!(
            observed,
            crate::desktop::custody::Observation::Released { .. }
        ));
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::drained(tx, row, observed).unwrap();
        assert_eq!(budget(&store.db), (0, 0, 0));
        let saved = retained(&store.db);
        ledger::startup(&mut store.db).unwrap();
        assert_eq!(retained(&store.db), saved);
        assert_eq!(budget(&store.db), (0, 0, 0));
    }
    #[test]
    fn a_thirty_third_valid_row_is_corruption_not_unbounded_roster_work() {
        let (_temp, location, mut store, epoch) = fixture();
        for _ in 0..32 {
            let v = original(&location.configuration, &epoch);
            admitted(&mut store, &v, &epoch);
        }
        let old = budget(&store.db);
        let v = original(&location.configuration, &epoch);
        let (_other_temp, _other_location, mut other, _other_epoch) = fixture();
        admitted(&mut other, &v, &epoch);
        let columns: usize = other
            .db
            .prepare("SELECT * FROM desktop_creation_clones")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_creation_clones", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_creation_clones VALUES({})",
            vec!["?"; columns].join(",")
        );
        store
            .db
            .execute(&sql, rusqlite::params_from_iter(values))
            .unwrap();
        assert!(ledger::charged_ids(&store.db).is_err());
        assert!(ledger::undrained(&store.db).is_err());
        assert!(ledger::startup(&mut store.db).is_err());
        assert_eq!(budget(&store.db), old);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn running(
        store: &mut Store,
        location: &Location,
        v: &Verified,
        epoch: &str,
    ) -> (
        zerus_desktop_ledger::operations::custody::Record,
        std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
    ) {
        use zerus_desktop_ledger::operations::{custody::Record, Ticket};
        let row = admitted(store, v, epoch);
        let ticket = Ticket {
            operation_id: v.binding.operation_id().unwrap(),
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: key(v).into(),
            binding_sha256: v.binding.fingerprint().unwrap(),
            writer_epoch: epoch.into(),
            service_epoch: epoch.into(),
            lease: uuid::Uuid::new_v4().to_string(),
        };
        let (prepared, held) = crate::desktop::custody::prepare(location, &ticket).unwrap();
        let record = Record {
            prepared,
            helper: None,
        };
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::reserve(&tx, &row, &ticket, &record).unwrap();
        tx.commit().unwrap();
        (record, held)
    }

    /// Corrupt stored bytes are copied into a fresh private database without
    /// bypassing/changing any production trigger or touching a live original.
    #[test]
    fn bounded_raw_columns_reject_nul_suffixes_types_and_unicode_overflow() {
        for (column, cap) in [
            ("source", 32768_usize),
            ("plan", 16384_usize),
            ("receipt", 24576_usize),
            ("fingerprint", 64_usize),
            ("writer_epoch", 36_usize),
            ("creation_id", 36_usize),
        ] {
            for mode in ["nul", "oversize", "unicode", "blob"] {
                let (_temp, location, mut store, epoch) = fixture();
                let v = original(&location.configuration, &epoch);
                let row = admitted(&mut store, &v, &epoch);
                retired(&mut store, &row);
                let charge = original_charge(&store.db, key(&v));
                let columns: Vec<String> = store
                    .db
                    .prepare("PRAGMA table_info(desktop_creation_clones)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_creation_clones", [], |r| {
                        (0..columns.len()).map(|i| r.get(i)).collect()
                    })
                    .unwrap();
                let index = columns.iter().position(|c| c == column).unwrap();
                let rusqlite::types::Value::Text(prefix) = values[index].clone() else {
                    panic!("fixture text")
                };
                values[index] = match mode {
                    "nul" => rusqlite::types::Value::Text(format!("{prefix}\0discarded suffix")),
                    "oversize" => {
                        rusqlite::types::Value::Text(format!("{prefix}{}", " ".repeat(cap + 1)))
                    }
                    "unicode" => rusqlite::types::Value::Text(format!(
                        "{prefix}{}",
                        "例".repeat(cap / 3 + 1)
                    )),
                    "blob" => rusqlite::types::Value::Blob(prefix.into_bytes()),
                    _ => unreachable!(),
                };
                let (_copy_temp, _copy_location, mut copy, _copy_epoch) = fixture();
                copy.db
                    .execute_batch("PRAGMA ignore_check_constraints=ON;")
                    .unwrap();
                let sql = format!(
                    "INSERT INTO desktop_creation_clones VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_creation_clone_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
                let before = retained(&copy.db);
                let before_budget = budget(&copy.db);
                assert!(ledger::load(&copy.db, key(&v)).is_err(), "{column}/{mode}");
                assert!(ledger::startup(&mut copy.db).is_err(), "{column}/{mode}");
                let fresh = original(&location.configuration, &epoch);
                let tx = copy
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                assert!(ledger::admit(&tx, &fresh, &epoch, 2).is_err());
                drop(tx);
                assert_eq!(retained(&copy.db), before, "{column}/{mode}");
                assert_eq!(budget(&copy.db), before_budget, "{column}/{mode}");
            }
        }
    }
}
