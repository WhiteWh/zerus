use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::json;
use zerus_client_core::desktop::creation_worktree::{self as wire, Intent, Phase, Verified};

fn original(configuration: &str, epoch: &str) -> Verified {
    let creation = uuid::Uuid::new_v4().to_string();
    let request = uuid::Uuid::new_v4().to_string();
    let value = json!({"id":creation,"creation_schema":1,"created":1,"revision":4,
        "cmd":"codex","tag":"example","host":"","state":"worktree","auto":true,
        "parent":"/srv","target":"/srv/repo","branch":"zerus-example",
        "destination":"/srv/repo-zerus-example","worktree_common":"/srv/repo/.git",
        "worktree_id":request,"worktree_receipts":true,
        "expected_machine":"22222222-2222-4222-8222-222222222222"});
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        creation_id: creation,
        worktree_request_id: request,
        revision: 4,
        original: value.to_string(),
    }
    .verify()
    .unwrap()
}
/// The original CLI receipt shape the owned effect records.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn created(plan: &wire::Plan) -> serde_json::Value {
    json!({"schema":1,"request_id":plan.worktree_request_id,
        "fingerprint":plan.request_fingerprint().unwrap(),"status":"created",
        "path":plan.destination,"common_dir":plan.common_dir,"branch":plan.branch,
        "effects_started":true,"started_at":1,"finished_at":2})
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
fn exact_previous_store_upgrade_adds_only_the_independent_worktree_leaf() {
    assert_eq!(
        SCHEMA - 1,
        crate::desktop::delivery::SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let (_temp, location, store, _) = fixture();
    remove_fixture(&store.db);
    store
        .db
        .pragma_update(None, "user_version", SCHEMA - 1)
        .unwrap();
    let before: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let clone_sql: String = store
        .db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='desktop_creation_clones'",
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
        // Its own two tables, then the later launch, termination outcome, task
        // message, task message input, deferred send, deferred answer and
        // compact-to-send leaves, and the question route leaves'
        // (zerus-sa9qn.8, .32 and .50).
        before + 2 + 2 + 1 + 2 + 2 + 2 + 2 + 2 + 2 + 2 + 1 + 2
    );
    // The clone leaf's original table is byte-compatible and separate.
    assert_eq!(
        store
            .db
            .query_row::<String, _, _>(
                "SELECT sql FROM sqlite_schema WHERE name='desktop_creation_clones'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        clone_sql
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
}

#[test]
fn shared_resource_total_counts_task_cache_and_worktree_exactly_once() {
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
        .execute("UPDATE desktop_creation_worktrees SET source='{}'", [])
        .is_err());
    assert!(store
        .db
        .execute("DELETE FROM desktop_creation_worktrees", [])
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
        ledger::load(&store.db, &changed.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt
            .binding,
        row.receipt.binding
    );
}

fn retry(original: &Verified) -> Verified {
    let mut intent = original.intent.clone();
    intent.worktree_request_id = uuid::Uuid::new_v4().to_string();
    intent.revision += 1;
    let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
    value["worktree_id"] = json!(intent.worktree_request_id);
    value["revision"] = json!(intent.revision);
    value["worktree_previous"] = json!([original.plan.worktree_request_id]);
    intent.original = value.to_string();
    intent.verify().unwrap()
}
fn failed_fixture(
    db: &mut rusqlite::Connection,
    original: &Verified,
    effects: serde_json::Value,
) -> wire::Receipt {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut row = ledger::admit(&tx, original, &uuid::Uuid::new_v4().to_string(), 1).unwrap();
    // A storage fixture only: the actual-actor control below supplies the
    // independent positive for real custody retirement.
    row.receipt.phase = Phase::Failed;
    row.receipt.drained = true;
    let mut result = json!({"schema":1,"request_id":original.plan.worktree_request_id,
        "fingerprint":original.plan.request_fingerprint().unwrap(),"status":"failed",
        "detail":"fixture refused before checkout"});
    if !effects.is_null() {
        result["effects_started"] = effects;
    }
    row.receipt.worktree = Some(result);
    tx.execute("UPDATE desktop_creation_worktrees SET receipt=?1,phase='failed',drained=1 WHERE worktree_id=?2",
        rusqlite::params![serde_json::to_string(&row.receipt).unwrap(),original.plan.worktree_request_id]).unwrap();
    // This synthetic physical drain mirrors live reservation accounting; it
    // is not a second release when a later request retires the scheduling slot.
    tx.execute(
        "UPDATE desktop_creation_worktree_budget SET rows=rows-1,
         bytes=bytes-(SELECT length(CAST(source AS BLOB))+length(CAST(plan AS BLOB))+24576+8192+4096+1024
                      FROM desktop_creation_worktrees WHERE worktree_id=?1),active=active-1 WHERE id=1",
        [&original.plan.worktree_request_id],
    )
    .unwrap();
    tx.commit().unwrap();
    row.receipt
}
#[test]
fn retry_retains_the_original_and_reopen_cannot_resurrect_its_slot() {
    let (_temp, location, mut store, epoch) = fixture();
    let original = original(&location.configuration, &epoch);
    let receipt = failed_fixture(&mut store.db, &original, json!(false));
    let next = retry(&original);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::admit(&tx, &next, &uuid::Uuid::new_v4().to_string(), 2).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        ledger::load(&store.db, &original.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt,
        receipt
    );
    assert_eq!(
        store
            .db
            .query_row::<String, _, _>(
                "SELECT creation_slot FROM desktop_creation_worktrees WHERE worktree_id=?1",
                [&original.plan.worktree_request_id],
                |r| r.get(0)
            )
            .unwrap(),
        format!("retired:{}", original.plan.worktree_request_id)
    );
    assert!(store
        .db
        .execute(
            "UPDATE desktop_creation_worktrees SET creation_id='changed'",
            []
        )
        .is_err());
    assert!(store
        .db
        .execute("DELETE FROM desktop_creation_worktrees", [])
        .is_err());
    drop(store);
    let mut store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        ledger::load(&store.db, &original.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt,
        receipt
    );
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 3).is_err());
}
#[test]
fn retry_rolls_back_the_slot_and_all_budget_charges_after_a_late_insert_failure() {
    let (_temp, location, mut store, epoch) = fixture();
    let original = original(&location.configuration, &epoch);
    let receipt = failed_fixture(&mut store.db, &original, json!(false));
    let budget = |db: &rusqlite::Connection| {
        db.query_row::<(u64, u64, u64), _, _>(
            "SELECT rows,bytes,active FROM desktop_creation_worktree_budget",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    let before = budget(&store.db);
    store.db.execute_batch("CREATE TRIGGER fixture_insert_failure BEFORE INSERT ON desktop_creation_worktrees BEGIN SELECT RAISE(ABORT,'late fixture refusal'); END;").unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 2).is_err());
    drop(tx);
    assert_eq!(budget(&store.db), before);
    assert_eq!(
        ledger::load(&store.db, &original.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt,
        receipt
    );
    assert_eq!(
        store
            .db
            .query_row::<String, _, _>(
                "SELECT creation_slot FROM desktop_creation_worktrees",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        original.binding.creation_id
    );
}
#[test]
fn retry_requires_literal_no_effects_and_the_same_original_geometry() {
    for effects in [
        serde_json::Value::Null,
        json!("false"),
        json!(0),
        json!(true),
    ] {
        let (_temp, location, mut store, epoch) = fixture();
        let original = original(&location.configuration, &epoch);
        failed_fixture(&mut store.db, &original, effects);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 2).is_err()
        );
    }
    for key in [
        "expected_machine",
        "target",
        "worktree_common",
        "destination",
        "branch",
    ] {
        let (_temp, location, mut store, epoch) = fixture();
        let original = original(&location.configuration, &epoch);
        failed_fixture(&mut store.db, &original, json!(false));
        let mut next = retry(&original).intent;
        let mut value: serde_json::Value = serde_json::from_str(&next.original).unwrap();
        value[key] = json!(match key {
            "expected_machine" => "33333333-3333-4333-8333-333333333333",
            "branch" => "another-branch",
            _ => "/other/path",
        });
        next.original = value.to_string();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::admit(
            &tx,
            &next.verify().unwrap(),
            &uuid::Uuid::new_v4().to_string(),
            2
        )
        .is_err());
    }
    for phase in [Phase::Unknown, Phase::Created, Phase::Running] {
        let (_temp, location, mut store, epoch) = fixture();
        let original = original(&location.configuration, &epoch);
        let mut receipt = failed_fixture(&mut store.db, &original, json!(false));
        receipt.phase = phase;
        if phase == Phase::Running {
            receipt.drained = false;
        }
        // A deliberately inconsistent record must refuse too; no migration or
        // load is allowed to turn it into a safe predecessor.
        store
            .db
            .execute(
                "UPDATE desktop_creation_worktrees SET receipt=?1 WHERE worktree_id=?2",
                rusqlite::params![
                    serde_json::to_string(&receipt).unwrap(),
                    original.plan.worktree_request_id
                ],
            )
            .unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 2).is_err()
        );
    }
}
#[test]
fn malformed_failed_results_and_concurrent_attempts_never_share_a_slot() {
    for (field, value) in [
        ("schema", json!(2)),
        ("detail", json!(null)),
        ("resource_error", json!("unknown resource")),
        ("fingerprint", json!("0".repeat(64))),
    ] {
        let (_temp, location, mut store, epoch) = fixture();
        let original = original(&location.configuration, &epoch);
        let mut receipt = failed_fixture(&mut store.db, &original, json!(false));
        receipt.worktree.as_mut().unwrap()[field] = value;
        store
            .db
            .execute(
                "UPDATE desktop_creation_worktrees SET receipt=?1 WHERE worktree_id=?2",
                rusqlite::params![
                    serde_json::to_string(&receipt).unwrap(),
                    original.plan.worktree_request_id
                ],
            )
            .unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 2).is_err(),
            "{field}"
        );
    }
    let (_temp, location, mut store, epoch) = fixture();
    let original = original(&location.configuration, &epoch);
    let old = failed_fixture(&mut store.db, &original, json!(false));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let path = location.root.join("state.sqlite3");
        let intent = retry(&original).intent;
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let mut db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap();
            db.busy_timeout(std::time::Duration::from_secs(2)).unwrap();
            barrier.wait();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            if ledger::admit(
                &tx,
                &intent.verify().unwrap(),
                &uuid::Uuid::new_v4().to_string(),
                2,
            )
            .is_ok()
            {
                tx.commit().unwrap();
                true
            } else {
                false
            }
        }));
    }
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(
        ledger::load(&store.db, &original.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt,
        old
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
}

#[test]
fn slots_upgrade_preserves_the_exact_old_receipt_and_immutable_identity() {
    let (_temp, location, mut store, epoch) = fixture();
    let original = original(&location.configuration, &epoch);
    let receipt = failed_fixture(&mut store.db, &original, json!(false));
    rewind_slots(&store.db);
    store
        .db
        .pragma_update(None, "user_version", SLOTS_SCHEMA - 1)
        .unwrap();
    drop(store);
    let mut store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        ledger::load(&store.db, &original.plan.worktree_request_id)
            .unwrap()
            .unwrap()
            .receipt,
        receipt
    );
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::admit(&tx, &retry(&original), &uuid::Uuid::new_v4().to_string(), 2).unwrap();
    tx.commit().unwrap();
    assert!(store
        .db
        .execute("UPDATE desktop_creation_worktrees SET source='{}'", [])
        .is_err());
}

#[test]
fn capacity_refusal_is_atomic_and_retirement_never_rearms_a_queued_original() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    store
        .db
        .execute(
            "UPDATE desktop_creation_worktree_budget SET bytes=16777216",
            [],
        )
        .unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &verified, &writer, 1).is_err());
    drop(tx);
    assert!(ledger::load(&store.db, &verified.plan.worktree_request_id)
        .unwrap()
        .is_none());
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    store
        .db
        .execute("UPDATE desktop_creation_worktree_budget SET bytes=0", [])
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
    let retired = ledger::load(&store.db, &verified.plan.worktree_request_id)
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
fn only_the_original_acquired_custody_lock_settles_a_worktree_result() {
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
        admission_id: verified.plan.worktree_request_id.clone(),
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
    let worktree = created(&verified.plan);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::result(&tx, &row, Some(worktree), "awaiting_resource_drain").unwrap();
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
    assert_eq!(settled.phase, Phase::Created);
    assert_eq!(
        settled.result(&verified.plan).unwrap().unwrap()["status"],
        "created"
    );
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

/// Recovery stores the original receipt it reads before recording drain.
/// An original over the wire bound is stored as its bounded projection, or as
/// Unknown when even that does not fit; storage never fails on size, and the
/// released charge always drains.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn an_oversized_original_is_stored_bounded_and_its_released_charge_drains() {
    use crate::desktop::custody;
    use zerus_desktop_ledger::operations::{custody::Record, Ticket};
    for unrepresentable in [false, true] {
        let (_temp, location, mut store, epoch) = fixture();
        let mut intent = original(&location.configuration, &epoch).intent;
        let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
        let long = |base: &str| {
            let mut path = String::from(base);
            while path.len() < 3400 {
                path.push_str("/segment-of-a-long-but-valid-worktree-path");
            }
            path
        };
        let target = long("/srv/repo");
        value["target"] = json!(target);
        value["worktree_common"] = json!(format!("{target}/.git"));
        value["destination"] = json!(long("/srv/repo-zerus-example"));
        intent.original = value.to_string();
        let verified = intent.verify().unwrap();
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
            admission_id: verified.plan.worktree_request_id.clone(),
            binding_sha256: verified.binding.fingerprint().unwrap(),
            writer_epoch: writer,
            service_epoch: epoch,
            lease: uuid::Uuid::new_v4().to_string(),
        };
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
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::running(&tx, &row).unwrap();
        tx.commit().unwrap();
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let plan = &verified.plan;
        let mut worktree = created(plan);
        worktree["creation_owner"] = json!({"version":1,
            "id":"44444444-4444-4444-8444-444444444444","common_dir":plan.common_dir,
            "path":plan.destination,"branch":plan.branch,"head":"a".repeat(40),
            "metadata":format!("{}/worktrees/-",plan.common_dir),"created_at":1});
        if unrepresentable {
            worktree["path"] = json!("\"".repeat(9000));
        }
        assert!(serde_json::to_vec(&worktree).unwrap().len() > wire::RESULT_BYTES);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::result(&tx, &row, Some(worktree), "awaiting_resource_drain").unwrap();
        tx.commit().unwrap();
        drop(held);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let observation = custody::probe(&location, &record);
        assert!(matches!(observation, custody::Observation::Released { .. }));
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let settled = ledger::drained(tx, row, observation).unwrap();
        if unrepresentable {
            assert_eq!(settled.phase, Phase::Unknown);
            assert_eq!(settled.code, ledger::UNREPRESENTABLE);
        } else {
            assert_eq!(settled.phase, Phase::Created);
            assert!(settled.result(plan).unwrap().is_ok());
        }
        assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod actor {
    use super::*;
    use crate::{config::Config, desktop::ui_authority, state::WorktreeStep as Step};
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
                    &crate::state::WorktreeControl<'_>,
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
            value["worktree_common"] = json!(parent.join("repo/.git"));
            value["destination"] = json!(parent.join("repo-zerus-example"));
            intent.original = value.to_string();
            intent.verify().unwrap()
        }
        /// The same original at other (synthetic) paths.
        fn original_at(&self, target: &str, destination: &str) -> Verified {
            let mut intent = super::original(&self.location.configuration, &self.epoch).intent;
            let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
            value["parent"] = json!(std::path::Path::new(destination).parent().unwrap());
            value["target"] = json!(target);
            value["worktree_common"] = json!(format!("{target}/.git"));
            value["destination"] = json!(destination);
            intent.original = value.to_string();
            intent.verify().unwrap()
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            let request = zerus_client_core::desktop::ui_requests::Request::CreationWorktree {
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
            ledger::load(&self.store.db, &original.plan.worktree_request_id)
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
    #[test]
    fn a_typed_slot_refusal_keeps_the_original_native_charge_and_reason() {
        let mut f = Fixture::new(|plan, control| {
            (control.before)(Step::Reserve)?;
            Ok(json!({"schema":1,"request_id":plan.worktree_request_id,
                "fingerprint":plan.request_fingerprint().unwrap(),"status":"failed",
                "effects_started":false,"detail":"fixture pre-effect failure"}))
        });
        let first = f.original();
        assert!(f.admit(&first).accepted);
        let next = retry(&first);
        let refused = f.admit(&next);
        assert!(!refused.accepted);
        assert_eq!(refused.code, "creation_worktree_predecessor_unsettled");
        assert!(refused
            .refusal(&next.intent)
            .unwrap()
            .contains("previous attempt"));
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 1);
        assert!(ledger::load(&f.store.db, &next.plan.worktree_request_id)
            .unwrap()
            .is_none());
        f.until("original physical drain", |f| f.row(&first).receipt.drained);
        assert!(f.admit(&next).accepted);
        f.until("new original physical drain", |f| {
            f.row(&next).receipt.drained
        });
    }

    #[test]
    fn a_legacy_unknown_card_stops_through_the_real_reducer_rpc_and_receipt_lane() {
        use zerus_client_core::creation::{
            self, Answer, Call, Creations, Effect, Journal, Placement,
        };
        struct Places;
        impl Placement for Places {
            fn resolve(
                &mut self,
                _: &mut serde_json::Value,
                _: &serde_json::Value,
            ) -> Result<(), String> {
                Ok(())
            }
        }
        // Use the maintained reducer and its actual ordered durable-write
        // barriers, not an extracted transition or a synthesized stopped row.
        fn flush(
            queue: &mut Creations,
            file: &std::path::Path,
            effects: Vec<Effect>,
        ) -> Vec<(u64, Call)> {
            let mut pending: std::collections::VecDeque<_> = effects.into();
            let mut calls = Vec::new();
            while let Some(effect) = pending.pop_front() {
                match effect {
                    Effect::Write { write, bytes, .. } => {
                        let ok =
                            zerus_client_core::fsx::write_private_durable(file, &bytes).is_ok();
                        assert!(ok);
                        pending.extend(queue.written(write, ok));
                    }
                    Effect::Call { flight, call } => calls.push((flight, call)),
                    Effect::Launch { .. } => panic!("Stop must not launch"),
                    _ => (),
                }
            }
            calls
        }
        let mut f = Fixture::new(|plan, control| {
            (control.before)(Step::Reserve)?;
            Ok(json!({"schema":1,"request_id":plan.worktree_request_id,
                "fingerprint":plan.request_fingerprint().unwrap(),"status":"failed",
                "effects_started":false,"detail":"fixture pre-effect failure"}))
        });
        let first = f.original();
        assert!(f.admit(&first).accepted);
        f.until("first original physical drain", |f| {
            f.row(&first).receipt.drained
        });
        let old_receipt = f.row(&first).receipt;
        let requests = f.config.state.join("worktree-requests");
        crate::platform::private_dir(&requests).unwrap();
        let old_path = requests.join(format!("{}.json", first.plan.worktree_request_id));
        let old_bytes = serde_json::to_vec(old_receipt.worktree.as_ref().unwrap()).unwrap();
        zerus_client_core::fsx::write_private_durable(&old_path, &old_bytes).unwrap();
        let current = retry(&first);
        assert!(ledger::load(&f.store.db, &current.plan.worktree_request_id)
            .unwrap()
            .is_none());
        let id = current.binding.creation_id.clone();
        let mut operation: serde_json::Value =
            serde_json::from_str(&current.intent.original).unwrap();
        operation["state"] = json!("unknown");
        operation["stop_requested"] = json!(true);
        let original = json!({"schema":1,"operations":{id.clone():operation}});
        let journal = f.config.state.join("creation-fixture.json");
        let mut queue = Creations::new(|| 1000, || uuid::Uuid::new_v4().to_string());
        let effects = queue.load(Journal::Bytes(serde_json::to_vec(&original).unwrap()));
        assert!(flush(&mut queue, &journal, effects).is_empty());
        let effects = queue.stop(&id);
        let calls = flush(&mut queue, &journal, effects);
        assert_eq!(calls.len(), 1);
        let (flight, call) = &calls[0];
        let Call::WorktreeReceipt {
            action,
            request_id,
            fingerprint,
            ..
        } = call
        else {
            panic!("expected exact original Stop")
        };
        assert_eq!(action, "cancel");
        assert_eq!(request_id, &current.plan.worktree_request_id);
        assert_eq!(
            Some(fingerprint.clone()),
            current.plan.request_fingerprint()
        );
        let before_reply = std::fs::read(&journal).unwrap();
        let request = wire::ControlRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            scope: current.intent.scope.clone(),
            creation_id: id.clone(),
            plan: current.plan.clone(),
            action: wire::ControlAction::Cancel,
        };
        let result = f.runtime.receipt_control(
            &f.store,
            &json!({"op":"creation_worktree_receipt","request":request}),
            Some(f.exclusion.clone()),
        );
        let reply: wire::ControlReply = serde_json::from_value(result).unwrap();
        assert!(reply.matches(&request));
        let fence = reply.receipt.unwrap();
        assert!(wire::fenced(&fence, &current.plan));
        let effects = queue.answered(
            *flight,
            Answer {
                ok: true,
                data: fence.clone(),
                error: String::new(),
            },
            &mut Places,
        );
        assert!(flush(&mut queue, &journal, effects).is_empty());
        let stopped = queue.operation(&id).unwrap();
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(stopped["worktree_id"], current.plan.worktree_request_id);
        assert_eq!(
            stopped["worktree_future_fence"],
            current.plan.worktree_request_id
        );
        assert!(stopped["error"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown"));
        assert!(!creation::can_resume(stopped));
        assert!(queue.can_edit(&id));
        assert!(queue.resume(&id).is_empty());
        assert_eq!(f.row(&first).receipt, old_receipt);
        assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
        // Losing the Stop reply cannot lose the fence or cause a second write:
        // reloading the pre-reply journal recovers by STATUS only.
        let saved = before_reply;
        let fence_path = requests.join(format!("{}.json", current.plan.worktree_request_id));
        let bytes = std::fs::read(&fence_path).unwrap();
        let mut recovered = Creations::new(|| 1000, || uuid::Uuid::new_v4().to_string());
        let effects = recovered.load(Journal::Bytes(saved));
        assert!(flush(&mut recovered, &journal, effects).is_empty());
        let effects = recovered.check(&id);
        let calls = flush(&mut recovered, &journal, effects);
        assert_eq!(calls.len(), 1);
        let (recovery_flight, recovery_call) = &calls[0];
        assert!(
            matches!(recovery_call, Call::WorktreeReceipt { action, .. } if action == "status")
        );
        let mut read = request;
        read.request_id = uuid::Uuid::new_v4().to_string();
        read.action = wire::ControlAction::Status;
        let result = f.runtime.receipt_control(
            &f.store,
            &json!({"op":"creation_worktree_receipt","request":read}),
            Some(f.exclusion.clone()),
        );
        let reply: wire::ControlReply = serde_json::from_value(result).unwrap();
        assert!(reply.matches(&read));
        assert_eq!(reply.receipt, Some(fence.clone()));
        let effects = recovered.answered(
            *recovery_flight,
            Answer {
                ok: true,
                data: fence,
                error: String::new(),
            },
            &mut Places,
        );
        assert!(flush(&mut recovered, &journal, effects).is_empty());
        assert_eq!(recovered.operation(&id).unwrap()["state"], "stopped");
        assert_eq!(std::fs::read(&fence_path).unwrap(), bytes);
        assert!(!creation::can_resume(recovered.operation(&id).unwrap()));
        // A separate explicit creation is allowed; its admission still needs
        // its own native proof. The old unknown row and original receipt stay.
        let recipe = json!({"cmd":"codex","tag":"recreated-fixture","host":"","auto":true,
            "target":current.plan.path,"destination":current.plan.destination,"branch":current.plan.branch});
        let (new_id, effects) = recovered.submit(recipe);
        let new_id = new_id.unwrap();
        assert_ne!(new_id, id);
        let calls = flush(&mut recovered, &journal, effects);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].1, Call::CreationDirectory { .. }));
        let effects = recovered.answered(calls[0].0, Answer { ok:true, data:json!({"path":current.plan.path,"creation_identity_v1":true,"computer_id":current.plan.expected_machine}), error:String::new() }, &mut Places);
        let calls = flush(&mut recovered, &journal, effects);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].1, Call::Catalog { .. }));
        let effects = recovered.answered(calls[0].0, Answer { ok:true, data:json!({"state":"ok","common_dir":current.plan.common_dir,"selected_root":current.plan.path,"creation_receipts_v1":true}), error:String::new() }, &mut Places);
        let calls = flush(&mut recovered, &journal, effects);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].1, Call::CreateWorktree { .. }));
        let value = recovered.operation(&new_id).unwrap();
        let fresh = wire::Intent {
            scope: current.intent.scope.clone(),
            creation_id: new_id.clone(),
            worktree_request_id: value["worktree_id"].as_str().unwrap().into(),
            revision: value["revision"].as_u64().unwrap(),
            original: creation::effect_checkpoint(value).unwrap(),
        }
        .verify()
        .unwrap();
        assert_ne!(
            fresh.plan.worktree_request_id,
            current.plan.worktree_request_id
        );
        assert!(f.admit(&fresh).accepted);
        f.until("new original physical drain", |f| {
            f.row(&fresh).receipt.drained
        });
        assert_eq!(
            recovered.operation(&id).unwrap()["worktree_id"],
            current.plan.worktree_request_id
        );
        assert_eq!(f.row(&first).receipt, old_receipt);
    }

    #[test]
    fn receipt_control_refuses_changed_scope_machine_and_the_original_native_root() {
        let mut f = Fixture::new(|_, _| Err("not executed".into()));
        let original = f.original();
        crate::platform::private_dir(&f.config.state.join("worktree-requests")).unwrap();
        let request = wire::ControlRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            scope: original.intent.scope.clone(),
            creation_id: original.binding.creation_id,
            plan: original.plan,
            action: wire::ControlAction::Cancel,
        };
        for changed in ["scope", "machine", "root"] {
            let mut value = json!({"op":"creation_worktree_receipt","request":request});
            match changed {
                "scope" => {
                    value["request"]["scope"]["service_epoch"] =
                        json!(uuid::Uuid::new_v4().to_string())
                }
                "machine" => {
                    value["request"]["plan"]["expected_machine"] =
                        json!(uuid::Uuid::new_v4().to_string())
                }
                _ => {
                    std::fs::rename(
                        &f.config.state,
                        f._temp.path().join("original-state-retained"),
                    )
                    .unwrap();
                    crate::platform::private_dir(&f.config.state).unwrap();
                    crate::platform::private_dir(&f.config.state.join("worktree-requests"))
                        .unwrap();
                }
            }
            let result = f
                .runtime
                .receipt_control(&f.store, &value, Some(f.exclusion.clone()));
            assert_eq!(result["accepted"], false, "{changed}");
        }
        assert!(!f
            .config
            .state
            .join("worktree-requests.identity.json")
            .exists());
    }

    #[test]
    fn a_fresh_attempt_after_a_drained_effect_free_failure_keeps_the_original_receipt() {
        let mut f = Fixture::new(|plan, control| {
            (control.before)(Step::Reserve)?;
            Ok(json!({"schema":1,"request_id":plan.worktree_request_id,
                "fingerprint":plan.request_fingerprint().unwrap(),"status":"failed",
                "effects_started":false,"detail":"synthetic refusal before checkout"}))
        });
        let first = f.original();
        assert!(f.admit(&first).accepted);
        f.until("first failure's actual resource drain", |f| {
            f.row(&first).receipt.drained
        });
        let retained = f.row(&first).receipt;
        assert_eq!(retained.phase, Phase::Failed);
        assert_eq!(
            retained.worktree.as_ref().unwrap()["effects_started"],
            false
        );
        let mut intent = first.intent.clone();
        let request = uuid::Uuid::new_v4().to_string();
        let mut value: serde_json::Value = serde_json::from_str(&intent.original).unwrap();
        value["worktree_previous"] = json!([first.plan.worktree_request_id]);
        value["worktree_id"] = json!(request);
        value["revision"] = json!(intent.revision + 1);
        intent.worktree_request_id = request;
        intent.revision += 1;
        intent.original = value.to_string();
        let next = intent.verify().unwrap();
        let admission = f.admit(&next);
        assert!(
            admission.accepted,
            "fresh attempt refused: {}",
            admission.code
        );
        assert_eq!(f.row(&first).receipt, retained);
        f.until("new attempt's actual resource drain", |f| {
            f.row(&next).receipt.drained
        });
        assert_eq!(f.row(&first).receipt, retained);
        let query = wire::Query::for_plan(
            first.intent.scope.clone(),
            first.binding.creation_id.clone(),
            &first.plan,
        )
        .unwrap();
        let reply: wire::Lookup = serde_json::from_value(super::query(
            &f.store,
            &json!({"op":"creation_worktree_result","query":query}),
        ))
        .unwrap();
        assert!(reply.matches(&query));
        assert_eq!(reply.receipt.unwrap(), retained);
    }
    #[test]
    fn fixed_admitted_plan_finishes_without_gui_and_waits_for_a_real_descendant_lock() {
        use std::process::{Command, Stdio};
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let original_children = Arc::new(std::sync::Mutex::new(Vec::new()));
        let children = original_children.clone();
        let mut f = Fixture::new(move |plan, control| {
            for step in [
                Step::Reserve,
                Step::Destination,
                Step::Checkout,
                Step::Verify,
                Step::Track,
                Step::Own,
            ] {
                (control.before)(step)?;
                count.fetch_add(1, Ordering::SeqCst);
            }
            // A real private helper and its descendant retain the exact
            // close-only lock across exec. Keep the original wait owner too;
            // no orphan fixture is delegated to the container PID 1.
            let mut command = Command::new("python3");
            command.args(["-c", "import os,sys,time\npid=os.fork()\nif pid:\n os.waitpid(pid,0)\n os._exit(0)\nopen(sys.argv[1], 'w').close()\nuntil=time.monotonic()+10\nwhile not os.path.exists(sys.argv[2]) and time.monotonic()<until: time.sleep(.005)\nos._exit(0)"])
                .arg(std::path::Path::new(&plan.destination).with_file_name("descendant-held"))
                .arg(std::path::Path::new(&plan.destination).with_file_name("descendant-release"))
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
            Ok(super::created(plan))
        });
        let original = f.original();
        assert!(f.admit(&original).matches(&original.intent));
        // There is deliberately no subsequent GUI proof/subscription.
        f.until("original result and descendant", |f| {
            f.row(&original).receipt.worktree.is_some()
                && f._temp.path().join("descendant-held").exists()
        });
        assert_eq!(effects.load(Ordering::SeqCst), 6);
        assert_eq!(f.row(&original).next_step, 6);
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
        assert_eq!(f.row(&original).receipt.phase, Phase::Created);
        assert!(f.row(&original).receipt.result(&original.plan).is_some());
        assert_eq!(f.store.operation_undrained().unwrap(), 0);
        for child in original_children.lock().unwrap().drain(..) {
            child.join().unwrap();
        }
    }
    /// Valid paths of about 3.4 KiB: the full original receipt repeats them in
    /// its ownership record and exceeds the wire's result bound.
    #[test]
    fn a_long_path_original_settles_through_its_bounded_projection_and_drains() {
        for unrepresentable in [false, true] {
            let full = Arc::new(AtomicUsize::new(0));
            let size = full.clone();
            let mut f = Fixture::new(move |plan, control| {
                for step in [
                    Step::Reserve,
                    Step::Destination,
                    Step::Checkout,
                    Step::Verify,
                    Step::Track,
                    Step::Own,
                ] {
                    (control.before)(step)?;
                }
                let mut receipt = super::created(plan);
                receipt["creation_owner"] = json!({"version":1,
                    "id":"44444444-4444-4444-8444-444444444444","common_dir":plan.common_dir,
                    "path":plan.destination,"branch":plan.branch,"head":"a".repeat(40),
                    "metadata":format!("{}/worktrees/-",plan.common_dir),"created_at":1});
                if unrepresentable {
                    // Not this plan's checkout, and too large for any projection.
                    receipt["path"] = json!("\"".repeat(9000));
                }
                size.store(
                    serde_json::to_vec(&receipt).unwrap().len(),
                    Ordering::SeqCst,
                );
                Ok(receipt)
            });
            let root = f._temp.path().canonicalize().unwrap();
            let long = |name: &str| {
                let mut path = root.join(name).to_string_lossy().into_owned();
                while path.len() < 3400 {
                    path.push_str("/segment-of-a-long-but-valid-worktree-path");
                }
                path
            };
            let original = f.original_at(&long("repo"), &long("repo-zerus-example"));
            assert!(f.admit(&original).matches(&original.intent));
            f.until("original drain", |f| f.row(&original).receipt.drained);
            assert!(full.load(Ordering::SeqCst) > wire::RESULT_BYTES);
            let receipt = f.row(&original).receipt;
            if unrepresentable {
                assert_eq!(receipt.phase, Phase::Unknown);
                assert_eq!(receipt.code, ledger::UNREPRESENTABLE);
                assert!(receipt.worktree.is_none());
            } else {
                assert_eq!(receipt.phase, Phase::Created);
                let created = receipt.result(&original.plan).unwrap().unwrap();
                assert_eq!(created["path"], json!(original.plan.destination));
                assert!(receipt.worktree.unwrap().get("creation_owner").is_none());
            }
            f.runtime.request_stop();
            f.until("stopped runtime drain", |f| f.runtime.drained());
            assert_eq!(f.store.operation_undrained().unwrap(), 0);
        }
    }

    /// The real owned executor, through the actor's own step checks, for a
    /// repository without an inherited upstream.
    #[cfg(target_os = "linux")]
    #[test]
    fn real_owned_executor_creates_a_no_upstream_checkout_through_every_actor_step() {
        crate::self_exec::private_probe_with(
            "desktop::creation_worktree::tests::actor::real_git_no_upstream_probe",
            120,
            96,
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run by real_owned_executor_creates_a_no_upstream_checkout_through_every_actor_step"]
    fn real_git_no_upstream_probe() {
        crate::self_exec::probe_child(
            "desktop::creation_worktree::tests::actor::real_git_no_upstream_probe",
        );
        let mut f = Fixture::new(|plan, control| {
            crate::state::repositories::with_direct_owned_git(|| {
                crate::state::create_worktree_owned(&plan.arguments(), control)
            })
        });
        let root = f._temp.path().canonicalize().unwrap();
        for args in [
            &["init", "-q", "-b", "main", "repo"][..],
            &[
                "-C",
                "repo",
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ][..],
        ] {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        }
        let original = f.original();
        assert!(f.admit(&original).matches(&original.intent));
        f.until("original drain", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(row.receipt.phase, Phase::Created, "{:?}", row.receipt);
        assert_eq!(row.next_step, 6, "every step, Track included, was checked");
        assert!(row.receipt.result(&original.plan).unwrap().is_ok());
        assert!(root.join("repo-zerus-example/.git").is_file());
        f.runtime.request_stop();
        f.until("stopped runtime drain", |f| f.runtime.drained());
        assert_eq!(f.store.operation_undrained().unwrap(), 0);
    }

    #[test]
    fn revoke_between_steps_refuses_verify_and_new_admission_without_replaying_the_worktree() {
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        let mut f = Fixture::new(move |_, control| {
            for step in [Step::Reserve, Step::Destination, Step::Checkout] {
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
            &json!({"op":"creation_worktree_result","query":query}),
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
        let row = ledger::load(&f.store.db, &original.plan.worktree_request_id)
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
            (control.before)(Step::Destination)?;
            started.store(true, Ordering::Release);
            let until = Instant::now() + Duration::from_secs(8);
            while !release.load(Ordering::Acquire) {
                assert!(Instant::now() < until, "actor fixture was not released");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(!control.cancelled.load(Ordering::Acquire));
            let result = (control.before)(Step::Checkout);
            assert!(!control.cancelled.load(Ordering::Acquire));
            stopped.store(result.is_err(), Ordering::Release);
            result?;
            Err("revoked actor incorrectly admitted Checkout".into())
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("worker before Checkout", |_| ready.load(Ordering::Acquire));
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
                (control.before)(Step::Checkout).is_err(),
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
            "SELECT rows,bytes,active FROM desktop_creation_worktree_budget WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    fn key(v: &Verified) -> &str {
        &v.plan.worktree_request_id
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
        db.query_row("SELECT length(CAST(source AS BLOB))+length(CAST(plan AS BLOB))+37888 FROM desktop_creation_worktrees WHERE worktree_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_creation_worktrees)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_creation_worktrees ORDER BY worktree_id",
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
                "UPDATE desktop_creation_worktree_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
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
        let leaf = &status["kinds"]["creation_worktree"];
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
                "counter"=>{store.db.execute("UPDATE desktop_creation_worktree_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_creation_worktrees SET receipt='{}' WHERE worktree_id=(SELECT worktree_id FROM desktop_creation_worktrees LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_creation_worktrees SET drained=2 WHERE worktree_id=(SELECT worktree_id FROM desktop_creation_worktrees LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_creation_worktree_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_creation_worktree_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
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
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_creation_worktree_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_creation_worktree_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
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
                "UPDATE desktop_creation_worktree_budget SET rows=1,bytes=1,active=1 WHERE id=1",
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
            .prepare("SELECT * FROM desktop_creation_worktrees")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_creation_worktrees", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_creation_worktrees VALUES({})",
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
                    .prepare("PRAGMA table_info(desktop_creation_worktrees)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_creation_worktrees", [], |r| {
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
                    "INSERT INTO desktop_creation_worktrees VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_creation_worktree_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
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
