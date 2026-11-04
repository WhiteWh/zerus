//! Storage mechanisms only. The production actor must supply native authority,
//! original frozen workers, actual owner exclusion and physical join evidence.
use super::*;
use rusqlite::{Connection, TransactionBehavior};
const EPOCH: &str = "11111111-1111-4111-8111-111111111111";
fn id(n: usize) -> String {
    format!("22222222-2222-4222-8222-{n:012}")
}
fn database(db: &mut Connection) {
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE service(id INTEGER PRIMARY KEY,configuration TEXT,epoch TEXT);
        CREATE TABLE desktop_commands(state TEXT);").unwrap();
    db.execute(
        "INSERT INTO service VALUES(1,?1,?2)",
        params!["a".repeat(64), EPOCH],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    super::super::migrate(&tx).unwrap();
    migrate(&tx).unwrap();
    migrate_effects(&tx).unwrap();
    tx.commit().unwrap();
}
fn original(n: usize) -> Original {
    Original {
        preparation_id: id(n * 2),
        operation_id: id(n * 2 + 1),
        configuration: "a".repeat(64),
        root: "/srv/example/cache".into(),
        limit: 1,
        defer_ms: 0,
        policy_change: true,
        revision: 0,
        service_epoch: EPOCH.into(),
        writer_epoch: id(100),
        lease: id(n + 200),
    }
}
fn binding(original: &Original) -> Binding {
    let directory = "b".repeat(64);
    let name = "report.txt".to_owned();
    let sha256 = "c".repeat(64);
    Binding {
        root: original.root.clone(),
        directory: directory.clone(),
        index_sha256: "d".repeat(64),
        index_identity: "e".repeat(64),
        limit: 1,
        candidates: vec![Candidate {
            content_id: content_id(&"f".repeat(64), &sha256),
            object: "f".repeat(64),
            name,
            sha256,
            bytes: 5,
            identity: "e".repeat(64),
        }],
    }
}
fn kept(binding: &Binding) -> Receipt {
    Receipt {
        index_sha256: binding.index_sha256.clone(),
        files: vec![FileState::Kept; binding.candidates.len()],
        bytes: binding.candidates.iter().map(|c| c.bytes).sum(),
        count: binding.candidates.len(),
    }
}
fn start(db: &mut Connection, original: &Original, hash: &str) {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    start_effects(&tx, original, hash).unwrap();
    tx.commit().unwrap();
}
fn prepare(db: &mut Connection, original: &Original, binding: &Binding) -> String {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(begin(&tx, original, &"f".repeat(64)).unwrap());
    let hash = prepared(&tx, original, binding, &kept(binding)).unwrap();
    tx.commit().unwrap();
    hash
}
fn admit(db: &mut Connection, original: &Original, hash: &str) -> Vec<bool> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let flags = reserve(&tx, original, hash).unwrap();
    tx.commit().unwrap();
    flags
}
fn join(db: &mut Connection, original: &Original, receipt: Option<&Receipt>) {
    let tx = db.transaction().unwrap();
    finish_joined(&tx, original, receipt).unwrap();
    tx.commit().unwrap();
}

#[test]
fn preparation_has_no_execution_and_changed_originals_and_scopes_refuse() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let original = original(1);
    let binding = binding(&original);
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(begin(&tx, &original, &"f".repeat(64)).unwrap());
    assert!(reserve(&tx, &original, &"a".repeat(64)).is_err());
    assert!(!begin(&tx, &original, &"f".repeat(64)).unwrap());
    assert!(begin(&tx, &original, &"b".repeat(64)).is_err());
    let hash = prepared(&tx, &original, &binding, &kept(&binding)).unwrap();
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    let mut changed = original.clone();
    changed.lease = id(999);
    assert!(reserve(&tx, &changed, &hash).is_err());
    changed = original.clone();
    changed.writer_epoch = id(998);
    assert!(reserve(&tx, &changed, &hash).is_err());
    assert!(reserve(&tx, &original, &"0".repeat(64)).is_err());
    tx.rollback().unwrap();
    db.execute("UPDATE service SET epoch=?1", [id(997)])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert!(reserve(&tx, &original, &hash).is_err());
}

#[test]
fn a_lost_destructive_result_fences_the_same_content_under_new_ids_and_index_generation() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let frozen = binding(&first);
    let hash = prepare(&mut db, &first, &frozen);
    assert_eq!(admit(&mut db, &first, &hash), [true]);
    start(&mut db, &first, &hash);
    assert_eq!(undrained(&db).unwrap(), 1);
    join(&mut db, &first, None);
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Unknown
    );
    assert_eq!(undrained(&db).unwrap(), 0);
    let mut second = original(2);
    second.revision = 1;
    let mut new_generation = binding(&second);
    new_generation.index_sha256 = "1".repeat(64);
    let hash = prepare(&mut db, &second, &new_generation);
    assert_eq!(admit(&mut db, &second, &hash), [false]);
    let receipt = Receipt {
        index_sha256: new_generation.index_sha256,
        files: vec![FileState::Kept],
        bytes: 5,
        count: 1,
    };
    join(&mut db, &second, Some(&receipt));
    assert!(db.execute("DELETE FROM desktop_cache_unknown", []).is_err());
    assert!(db
        .execute(
            "UPDATE desktop_cache_unknown SET content_id=?1",
            ["0".repeat(64)]
        )
        .is_err());
    let row = load(&db, &first.preparation_id).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Unknown);
}

#[test]
fn root_policy_changes_and_global_revoke_block_new_effects_without_releasing_workers() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let hash = prepare(&mut db, &first, &binding(&first));
    let tx = db.transaction().unwrap();
    revoke(&tx).unwrap();
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    assert!(reserve(&tx, &first, &hash).is_err());
    tx.rollback().unwrap();
    assert_eq!(undrained(&db).unwrap(), 1);
    let mut second = original(2);
    second.revision = 1;
    let tx = db.transaction().unwrap();
    assert!(begin(&tx, &second, &"f".repeat(64)).is_err());
    tx.rollback().unwrap();
    join(&mut db, &first, None);
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Refused
    );
    let hash = prepare(&mut db, &second, &binding(&second));
    db.execute("INSERT INTO desktop_commands VALUES('prepared')", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert!(reserve(&tx, &second, &hash).is_err());
}

#[test]
fn committed_original_result_can_finish_after_revoke_but_cannot_finish_twice() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let original = original(1);
    let binding = binding(&original);
    let hash = prepare(&mut db, &original, &binding);
    admit(&mut db, &original, &hash);
    start(&mut db, &original, &hash);
    let tx = db.transaction().unwrap();
    revoke(&tx).unwrap();
    tx.commit().unwrap();
    let receipt = Receipt {
        index_sha256: binding.index_sha256,
        files: vec![FileState::Removed],
        bytes: 0,
        count: 0,
    };
    join(&mut db, &original, Some(&receipt));
    let row = load(&db, &original.operation_id).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Settled);
    assert!(!row.undrained);
    assert_eq!(row.receipt, Some(receipt.clone()));
    let tx = db.transaction().unwrap();
    assert!(finish_joined(&tx, &original, Some(&receipt)).is_err());
}

#[test]
fn actual_database_reopen_recovers_reserved_work_as_unknown_without_replay() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("state.sqlite3");
    let first = original(1);
    {
        let mut db = Connection::open(&path).unwrap();
        database(&mut db);
        let hash = prepare(&mut db, &first, &binding(&first));
        admit(&mut db, &first, &hash);
        start(&mut db, &first, &hash);
    }
    let mut db = Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    recover(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Unknown
    );
    assert_eq!(undrained(&db).unwrap(), 0);
    let mut second = original(2);
    second.revision = 1;
    let hash = prepare(&mut db, &second, &binding(&second));
    assert_eq!(admit(&mut db, &second, &hash), [false]);
}

#[test]
fn nondestructive_preparation_recovery_is_refused_and_never_promoted() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    prepare(&mut db, &first, &binding(&first));
    let tx = db.transaction().unwrap();
    recover(&tx).unwrap();
    tx.commit().unwrap();
    let row = load(&db, &first.operation_id).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Refused);
    assert_eq!(
        db.query_row("SELECT count(*) FROM desktop_cache_unknown", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let tx = db.transaction().unwrap();
    assert!(reserve(&tx, &first, row.binding_sha256.as_ref().unwrap()).is_err());
}

#[test]
fn explicit_policy_commit_is_separate_from_effects_and_refresh_cannot_change_it() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let mut automatic = original(1);
    automatic.policy_change = false;
    let tx = db.transaction().unwrap();
    assert!(begin(&tx, &automatic, &"f".repeat(64)).is_err());
    tx.rollback().unwrap();
    let explicit = original(2);
    let tx = db.transaction().unwrap();
    assert!(begin(&tx, &explicit, &"f".repeat(64)).unwrap());
    tx.commit().unwrap();
    // A missing/invalid cache can retain an explicit preference. No file
    // reservation exists and a refresh cannot manufacture a different cap.
    join(&mut db, &explicit, None);
    let mut refresh = original(3);
    refresh.policy_change = false;
    refresh.revision = 1;
    let mut changed = refresh.clone();
    changed.limit = 2;
    let tx = db.transaction().unwrap();
    assert!(begin(&tx, &changed, &"e".repeat(64)).is_err());
    tx.rollback().unwrap();
    let hash = prepare(&mut db, &refresh, &binding(&refresh));
    assert_eq!(admit(&mut db, &refresh, &hash), [true]);
    assert_eq!(
        db.query_row("SELECT revision FROM desktop_cache_roots", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn a_never_started_reservation_keeps_its_original_receipt_after_expiry_and_join() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let frozen = binding(&first);
    let mut saved = kept(&frozen);
    saved.bytes = 73;
    saved.count = 4;
    let tx = db.transaction().unwrap();
    begin(&tx, &first, &"f".repeat(64)).unwrap();
    let hash = prepared(&tx, &first, &frozen, &saved).unwrap();
    tx.commit().unwrap();
    admit(&mut db, &first, &hash);
    let tx = db.transaction().unwrap();
    expire(&tx, &first).unwrap();
    tx.commit().unwrap();
    let pending = load(&db, &first.operation_id).unwrap().unwrap();
    assert_eq!(pending.phase, Phase::Reserved);
    assert!(!pending.effect_started);
    assert!(pending.undrained);
    let tx = db.transaction().unwrap();
    assert!(start_effects(&tx, &first, &hash).is_err());
    drop(tx);
    join(&mut db, &first, None);
    let row = load(&db, &first.operation_id).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Settled);
    assert_eq!(row.receipt, Some(saved));
    assert!(!row.undrained);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM desktop_cache_unknown", [], |r| r
            .get(0))
            .unwrap(),
        0
    );
    let mut next = original(2);
    next.revision = 1;
    let next_hash = prepare(&mut db, &next, &binding(&next));
    assert_eq!(admit(&mut db, &next, &next_hash), [true]);
}

#[test]
fn effect_start_checks_the_current_original_policy_hash_lease_and_revoke_and_is_monotone() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let hash = prepare(&mut db, &first, &binding(&first));
    let tx = db.transaction().unwrap();
    assert!(start_effects(&tx, &first, &hash).is_err());
    drop(tx);
    admit(&mut db, &first, &hash);
    for changed in 0..6 {
        let tx = db.transaction().unwrap();
        let mut selected = first.clone();
        let mut selected_hash = hash.clone();
        match changed {
            0 => selected.lease = id(777),
            1 => selected.writer_epoch = id(778),
            2 => selected_hash = "0".repeat(64),
            3 => {
                tx.execute("UPDATE desktop_cache_roots SET revision=revision+1", [])
                    .unwrap();
            }
            4 => {
                revoke(&tx).unwrap();
            }
            _ => {
                tx.execute("INSERT INTO desktop_commands VALUES('prepared')", [])
                    .unwrap();
            }
        }
        assert!(start_effects(&tx, &selected, &selected_hash).is_err());
        tx.rollback().unwrap();
        assert!(
            !load(&db, &first.operation_id)
                .unwrap()
                .unwrap()
                .effect_started
        );
    }
    start(&mut db, &first, &hash);
    assert!(
        load(&db, &first.operation_id)
            .unwrap()
            .unwrap()
            .effect_started
    );
    let tx = db.transaction().unwrap();
    assert!(start_effects(&tx, &first, &hash).is_err());
    drop(tx);
    assert!(db
        .execute("UPDATE desktop_cache_operations SET effect_started=0", [])
        .is_err());
    assert!(db
        .execute("UPDATE desktop_cache_operations SET kept_receipt=NULL", [])
        .is_err());
    join(&mut db, &first, None);
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Unknown
    );
}

#[test]
fn a_rolled_back_start_cannot_turn_the_original_reservation_into_an_effect() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let hash = prepare(&mut db, &first, &binding(&first));
    admit(&mut db, &first, &hash);
    let tx = db.transaction().unwrap();
    start_effects(&tx, &first, &hash).unwrap();
    tx.rollback().unwrap();
    assert!(
        !load(&db, &first.operation_id)
            .unwrap()
            .unwrap()
            .effect_started
    );
    let contradicts = Receipt {
        files: vec![FileState::Removed],
        ..kept(&binding(&first))
    };
    let tx = db.transaction().unwrap();
    assert!(finish_joined(&tx, &first, Some(&contradicts)).is_err());
    tx.rollback().unwrap();
    assert_eq!(undrained(&db).unwrap(), 1);
    join(&mut db, &first, None);
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Settled
    );
}

#[test]
fn an_actual_legacy_eighteen_reservation_migrates_as_started_and_recovers_unknown() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let hash = prepare(&mut db, &first, &binding(&first));
    admit(&mut db, &first, &hash);
    db.execute_batch(
        "DROP TRIGGER desktop_cache_effect_monotone; DROP TRIGGER desktop_cache_kept_original;
        ALTER TABLE desktop_cache_operations DROP COLUMN effect_started;
        ALTER TABLE desktop_cache_operations DROP COLUMN kept_receipt;",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    migrate_effects(&tx).unwrap();
    tx.commit().unwrap();
    let legacy = load(&db, &first.operation_id).unwrap().unwrap();
    assert!(legacy.effect_started);
    assert!(legacy.kept_receipt.is_none());
    assert_eq!(legacy.original, first);
    let tx = db.transaction().unwrap();
    recover(&tx).unwrap();
    tx.commit().unwrap();
    let row = load(&db, &first.operation_id).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Unknown);
    assert!(!row.undrained);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM desktop_cache_unknown", [], |r| r
            .get(0))
            .unwrap(),
        1
    );
}

#[test]
fn missing_original_kept_evidence_refuses_recovery_without_inventing_totals() {
    let mut db = Connection::open_in_memory().unwrap();
    database(&mut db);
    let first = original(1);
    let hash = prepare(&mut db, &first, &binding(&first));
    admit(&mut db, &first, &hash);
    // Deliberately model damaged evidence; ordinary writers cannot erase it.
    db.execute_batch("DROP TRIGGER desktop_cache_kept_original; UPDATE desktop_cache_operations SET kept_receipt=NULL;").unwrap();
    let tx = db.transaction().unwrap();
    assert!(finish_joined(&tx, &first, None).is_err());
    tx.rollback().unwrap();
    assert_eq!(undrained(&db).unwrap(), 1);
    assert_eq!(
        load(&db, &first.operation_id).unwrap().unwrap().phase,
        Phase::Reserved
    );
}
