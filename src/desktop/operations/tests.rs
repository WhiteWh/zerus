//! G2 + real SQLite mechanism evidence only. Fixture admission/reservation is
//! not a current producer, worker, child/pipe/custody or runtime drain proof.
use super::*;
use serde_json::json;

mod integration;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod owned;

const PRIVATE: &str = "PRIVATE TASK AND SQL DIAGNOSTIC";
const OTHER: &str = "ffffffff-1111-4111-8111-111111111111";

struct Fixture {
    value: Value,
    binding: policy::Binding,
    raw: Vec<u8>,
    input: Vec<u8>,
    provenance: Value,
}
impl Fixture {
    fn new() -> Self {
        let value: Value = serde_json::from_str(include_str!(
            "../../../client/fixtures/desktop-operation-port-v1.json"
        ))
        .unwrap();
        Self {
            binding: serde_json::from_value(value["binding"].clone()).unwrap(),
            raw: value["raw_record"].as_str().unwrap().as_bytes().to_vec(),
            input: value["retained_input"]
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
            provenance: json!({"fixture":"staged-only","private":PRIVATE}),
            value,
        }
    }
    fn verified(&self) -> Result<VerifiedOriginal, Refused> {
        VerifiedOriginal::verify(
            self.binding.clone(),
            &self.raw,
            &self.input,
            &self.provenance,
        )
    }
    fn ticket(&self) -> ledger::Ticket {
        serde_json::from_value(self.value["tickets"]["first"].clone()).unwrap()
    }
    fn seed(&self, db: &mut Connection) -> ledger::Admitted {
        let original = self.verified().unwrap();
        let t = self.ticket();
        ledger::admit(
            db,
            &original
                .admission(&t.admission_id, &t.writer_epoch, &t.service_epoch)
                .unwrap(),
        )
        .unwrap()
    }
    fn reserve(&self, db: &mut Connection, admitted: &ledger::Admitted) -> ledger::Ticket {
        let t = self.ticket();
        let result = ledger::reserve(
            db,
            &ledger::Reserve {
                operation_id: admitted.operation_id.clone(),
                attempt_id: t.attempt_id,
                revision: admitted.revision,
                writer_epoch: t.writer_epoch,
                service_epoch: t.service_epoch,
                lease: t.lease,
            },
        )
        .unwrap();
        assert!(result.fresh);
        result.attempt.ticket
    }
    fn reply(&self) -> Vec<u8> {
        let key = &self.binding.key;
        serde_json::to_vec(
            &json!({"schema":1,"id":key.request_id,"project":key.project,
            "uuid":key.task,"state":"submitted","error":PRIVATE,
            "task":{"uuid":key.task,"project":key.project,"body":PRIVATE}}),
        )
        .unwrap()
    }
}
fn database() -> Connection {
    initialize(Connection::open_in_memory().unwrap())
}
fn initialize(mut db: Connection) -> Connection {
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let tx = db.transaction().unwrap();
    zerus_desktop_ledger::migrate(&tx).unwrap();
    ledger::migrate(&tx).unwrap();
    tx.commit().unwrap();
    let t = Fixture::new().ticket();
    zerus_desktop_ledger::begin_service(&mut db, &t.service_epoch).unwrap();
    zerus_desktop_ledger::authorize(&mut db, &t.writer_epoch, &t.service_epoch).unwrap();
    db
}
fn attempt(db: &Connection) -> (String, bool, Option<String>) {
    db.query_row(
        "SELECT state,drained,outcome_json FROM desktop_operation_attempts",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}
fn held(db: &Connection) {
    assert_eq!(ledger::totals(db).unwrap().undrained, 1);
    let count: i64 = db
        .query_row("SELECT count(*) FROM desktop_operation_attempts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(!attempt(db).1);
}

#[test]
fn shared_utf8_original_is_data_only_and_roundtrips_every_field_and_exact_bytes() {
    let f = Fixture::new();
    let original = f.verified().unwrap();
    assert_eq!(
        serde_json::to_value(&original.binding).unwrap(),
        f.value["binding"]
    );
    assert_eq!(to_policy(&original.binding), f.binding);
    assert_eq!(
        original.binding.key.operation_id().unwrap(),
        f.value["expected_operation_id"]
    );
    assert_eq!(original.input_sha256(), f.value["expected_plan_sha256"]);
    assert_eq!(
        original.consent_sha256(),
        f.value["expected_consent_sha256"]
    );
    assert_ne!(
        original.binding.source_record_sha256,
        original.binding.ui_snapshot_sha256
    );
    let t = f.ticket();
    let data = original
        .admission(&t.admission_id, &t.writer_epoch, &t.service_epoch)
        .unwrap();
    assert_eq!(data.raw_record, f.raw);
    assert_eq!(data.retained_input, f.input);
    let mut db = database();
    assert_eq!(ledger::totals(&db).unwrap().unsettled, 0);
    assert_eq!(ledger::totals(&db).unwrap().undrained, 0);
    let admitted = ledger::admit(&mut db, &data).unwrap(); // fixture only
    let loaded = Loaded::load(&db, &admitted).unwrap();
    assert_eq!(loaded.verified.raw, f.raw);
    assert_eq!(loaded.verified.input, f.input);
    assert_eq!(loaded.verified.provenance, f.provenance);
    let proposal = loaded.proposal(false).unwrap();
    assert_eq!(proposal.binding(), &f.binding);
    assert_eq!(proposal.input(), f.input);
    assert_eq!(
        proposal.args(),
        [
            "task",
            "start-deliver",
            &f.binding.key.task,
            "--project",
            &f.binding.key.project,
            "--json",
            "--recover-only"
        ]
    );
    let stored: (String, String) = db
        .query_row(
            "SELECT plan_sha256,consent_sha256 FROM desktop_operations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (original.input_sha256(), original.consent_sha256()));
    assert_eq!(
        ledger::totals(&db).unwrap().undrained,
        0,
        "a pure proposal never reserves"
    );
}

#[test]
fn missing_oversized_or_changed_private_originals_and_metadata_refuse_without_body_errors() {
    let mut f = Fixture::new();
    f.input.clear();
    assert_eq!(
        f.verified().err(),
        Some(Refused::Policy(policy::Review::MissingRetainedInput))
    );
    for input in [false, true] {
        let mut f = Fixture::new();
        if input {
            f.input = vec![b' '; policy::PRIVATE_BYTES + 1];
        } else {
            f.raw = vec![b' '; policy::PRIVATE_BYTES + 1];
        }
        assert_eq!(
            f.verified().err(),
            Some(Refused::Policy(policy::Review::TooLarge))
        );
    }
    let mut f = Fixture::new();
    f.provenance = json!({"body":PRIVATE.repeat(ledger::METADATA_BYTES)});
    assert_eq!(f.verified().err(), Some(Refused::Metadata));
    for part in 0..4 {
        let mut f = Fixture::new();
        match part {
            0 => f.raw.push(b' '),
            1 => f.binding.ui_snapshot_sha256 = "0".repeat(64),
            2 => f.input.push(b' '),
            _ => {
                let mut plan: Value = serde_json::from_slice(&f.input).unwrap();
                plan["request"]["text"] = json!(PRIVATE);
                f.input = serde_json::to_vec(&plan).unwrap();
                f.binding.task_start_reconcile_input_v1_sha256 = hash(&f.input);
            }
        }
        let error = f.verified().err().expect("changed original refused");
        assert!(!format!("{error:?}").contains(PRIVATE));
    }
}

#[test]
fn changed_key_marker_and_each_executor_identity_are_revalidated_by_g2() {
    for part in 0..8 {
        let mut f = Fixture::new();
        match part {
            0 => f.binding.key.project = "another-project".into(),
            1 => f.binding.key.task = OTHER.into(),
            2 => f.binding.key.request_id = OTHER.into(),
            3 => f.binding.marker_id = OTHER.into(),
            4 => f.binding.executor.machine = "another-machine".into(),
            5 => f.binding.executor.session_id = "another-session".into(),
            6 => f.binding.executor.run_id = "another-run".into(),
            _ => f.binding.executor.conversation_id = "another-conversation".into(),
        }
        assert!(f.verified().is_err(), "identity case {part}");
    }
    let mut f = Fixture::new();
    f.binding.key.schema = 2;
    assert!(f.verified().is_err());
}

#[test]
fn reload_matches_all_admitted_fields_and_refuses_ledger_only_invalid_g2_records() {
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    for part in 0..4 {
        let mut changed = admitted.clone();
        match part {
            0 => changed.operation_id = "0".repeat(64),
            1 => changed.admission_id = OTHER.into(),
            2 => changed.binding_sha256 = "0".repeat(64),
            _ => changed.revision += 1,
        }
        assert!(Loaded::load(&db, &changed).is_err());
    }
    let mut db = database();
    let mut binding = to_ledger(&f.binding);
    binding.executor.run_id = "forged-run".into();
    let t = f.ticket();
    let stored = ledger::admit(
        &mut db,
        &ledger::Admission {
            admission_id: &t.admission_id,
            binding: &binding,
            raw_record: &f.raw,
            retained_input: &f.input,
            provenance: &f.provenance,
            writer_epoch: &t.writer_epoch,
            service_epoch: &t.service_epoch,
        },
    )
    .unwrap();
    assert!(
        Loaded::load(&db, &stored).is_err(),
        "ledger byte hashes alone never replace G2 verification"
    );
    assert_eq!(ledger::totals(&db).unwrap().undrained, 0);
}

#[test]
fn historical_terminal_cancelled_and_invalid_or_private_reply_never_settle_an_attempt() {
    let mut f = Fixture::new();
    let mut raw: Value = serde_json::from_slice(&f.raw).unwrap();
    raw["phase"] = json!("started");
    f.raw = serde_json::to_vec(&raw).unwrap();
    f.binding.source_record_sha256 = hash(&f.raw);
    f.binding.ui_snapshot_sha256 = zerus_client_core::tasks_write::outbox::snapshot(&raw);
    let mut db = database();
    let admitted = f.seed(&mut db);
    let loaded = Loaded::load(&db, &admitted).unwrap();
    assert!(matches!(
        loaded.proposal(false),
        Err(Refused::Policy(policy::Review::HistoricalTerminal))
    ));
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    let t = f.reserve(&mut db, &admitted);
    let loaded = Loaded::load(&db, &admitted).unwrap();
    assert!(matches!(
        loaded.proposal(true),
        Err(Refused::Policy(policy::Review::Cancelled))
    ));
    for bytes in [
        PRIVATE.as_bytes().to_vec(),
        vec![b' '; policy::REPLY_BYTES + 1],
        b"{}".to_vec(),
    ] {
        let error = loaded.finish_submitted(&mut db, &t, &bytes).unwrap_err();
        assert!(!format!("{error:?}").contains(PRIVATE));
        assert_eq!(attempt(&db), ("reserved".into(), false, None));
        held(&db);
    }
}

#[test]
fn complete_ticket_forgery_cannot_finish_or_release_the_valid_reserved_attempt() {
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    let ticket = f.reserve(&mut db, &admitted);
    let loaded = Loaded::load(&db, &admitted).unwrap();
    for part in 0..8 {
        let mut t = ticket.clone();
        match part {
            0 => t.operation_id = "0".repeat(64),
            1 => t.admission_id = OTHER.into(),
            2 => t.binding_sha256 = "0".repeat(64),
            3 => t.revision += 1,
            4 => t.attempt_id = OTHER.into(),
            5 => t.service_epoch = OTHER.into(),
            6 => t.writer_epoch = OTHER.into(),
            _ => t.lease = OTHER.into(),
        }
        assert!(
            loaded.finish_submitted(&mut db, &t, &f.reply()).is_err(),
            "ticket case {part}"
        );
        assert_eq!(attempt(&db), ("reserved".into(), false, None));
        held(&db);
    }
    loaded
        .finish_submitted(&mut db, &ticket, &f.reply())
        .unwrap();
    held(&db);
    let saved = attempt(&db).2.unwrap();
    assert!(!saved.contains(PRIVATE));
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap()["receipt"],
        json!({"schema":1,
        "id":f.binding.key.request_id,"project":f.binding.key.project,"uuid":f.binding.key.task,"state":"submitted"})
    );
}

#[test]
fn exact_old_epoch_result_can_finish_after_revoke_without_current_epoch_or_drain() {
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    let ticket = f.reserve(&mut db, &admitted);
    zerus_desktop_ledger::revoke(&db, "fixture").unwrap();
    zerus_desktop_ledger::begin_service(&mut db, OTHER).unwrap();
    let loaded = Loaded::load(&db, &admitted).unwrap();
    loaded
        .finish_submitted(&mut db, &ticket, &f.reply())
        .unwrap();
    loaded
        .finish_submitted(&mut db, &ticket, &f.reply())
        .unwrap();
    assert_eq!(attempt(&db).0, "submitted");
    held(&db);
}

#[test]
fn terminal_unknown_and_restart_unknown_cannot_be_replaced_by_late_success() {
    for restart in [false, true] {
        let f = Fixture::new();
        let mut db = database();
        let admitted = f.seed(&mut db);
        let ticket = f.reserve(&mut db, &admitted);
        if restart {
            assert_eq!(ledger::recover_batch(&mut db, OTHER).unwrap(), 1);
        } else {
            ledger::finish(
                &mut db,
                &ticket,
                &ledger::Outcome::Unknown {
                    reason: ledger::Unknown::LostResult,
                },
            )
            .unwrap();
        }
        let before = attempt(&db);
        let loaded = Loaded::load(&db, &admitted).unwrap();
        assert!(loaded
            .finish_submitted(&mut db, &ticket, &f.reply())
            .is_err());
        assert_eq!(attempt(&db), before);
        held(&db);
    }
}

#[test]
fn finish_transaction_failure_is_body_free_and_atomic_without_another_reservation() {
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    let ticket = f.reserve(&mut db, &admitted);
    let loaded = Loaded::load(&db, &admitted).unwrap();
    db.execute_batch("CREATE TEMP TRIGGER fixture_abort BEFORE UPDATE ON desktop_operations BEGIN SELECT RAISE(ABORT,'PRIVATE TASK AND SQL DIAGNOSTIC'); END;").unwrap();
    assert_eq!(
        loaded.finish_submitted(&mut db, &ticket, &f.reply()),
        Err(Refused::Store)
    );
    assert_eq!(attempt(&db), ("reserved".into(), false, None));
    held(&db);
    let state: String = db
        .query_row("SELECT policy_state FROM desktop_operations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "ready");
    db.execute_batch("DROP TRIGGER fixture_abort").unwrap();
    loaded
        .finish_submitted(&mut db, &ticket, &f.reply())
        .unwrap();
    held(&db);
}

#[test]
fn file_backed_reopen_preserves_exact_original_and_unknown_resource_charge() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.sqlite3");
    let f = Fixture::new();
    let mut db = initialize(Connection::open(&path).unwrap());
    let admitted = f.seed(&mut db);
    let ticket = f.reserve(&mut db, &admitted);
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    assert_eq!(ledger::recover_batch(&mut db, OTHER).unwrap(), 1);
    let loaded = Loaded::load(&db, &admitted).unwrap();
    assert_eq!(loaded.verified.raw, f.raw);
    assert_eq!(loaded.verified.input, f.input);
    assert!(loaded
        .finish_submitted(&mut db, &ticket, &f.reply())
        .is_err());
    held(&db);
}
