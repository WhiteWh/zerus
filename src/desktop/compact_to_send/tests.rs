use super::ledger::Child;
use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use zerus_client_core::desktop::compact_to_send::{Intent, Settled, Step, Verified};
use zerus_client_core::operation_recovery::deferred_input::{self as checkpoint, Checkpoint};

const SESSION: &str = "claude/private-compact";
const RUN: &str = "run-1";
const CONVERSATION: &str = "conversation-1";
const MACHINE: &str = "22222222-2222-4222-8222-222222222222";

/// A saved version 2 compact-to-send checkpoint with both exact stdins the
/// client builds for it.
fn original(configuration: &str, epoch: &str) -> Verified {
    let text = "after compaction";
    let checkpoint = Checkpoint::reserve_compact_to_send(checkpoint::Intent {
        kind: checkpoint::Kind::CompactToSend,
        draft_revision: 3,
        message_sha256: checkpoint::message_sha256(text, Vec::<(&str, &str, &str, &[u8])>::new()),
        run_id: RUN.into(),
        conversation_id: CONVERSATION.into(),
        question: None,
        start: None,
        retry_of: None,
        consent: true,
    })
    .unwrap();
    let compaction = json!({"request_id": checkpoint.request_id, "expected_run_id": RUN,
        "expected_conversation_id": CONVERSATION});
    let stdin = json!({"request_id": checkpoint.send_request_id, "text": text, "attachments": [],
        "expected_run_id": RUN, "expected_conversation_id": CONVERSATION,
        "expected_compaction_id": checkpoint.request_id});
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        session: SESSION.into(),
        request_id: checkpoint.request_id.clone(),
        send_request_id: checkpoint.send_request_id.clone().unwrap(),
        checkpoint: serde_json::to_string(&checkpoint).unwrap(),
        compaction: compaction.to_string(),
        original: stdin.to_string(),
    }
    .verify()
    .unwrap()
}
/// The CLI's own compaction record of `plan`'s request with `status`, as
/// `hgs compact-context` writes it.
fn cli_record(plan: &wire::Plan, status: &str) -> Value {
    json!({"request_id": plan.compaction_request_id, "name": plan.session, "run_id": plan.run_id,
        "conversation_id": plan.conversation_id, "status": status, "at": 7.5})
}
fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
/// Writes `body` atomically as the CLI's private compaction receipt of
/// `request` under the CLI state root `state`.
fn write_receipt(state: &Path, request: &str, body: &[u8]) -> PathBuf {
    let folder = state.join("compact_receipts");
    if !folder.exists() {
        std::fs::create_dir(&folder).unwrap();
    }
    mode(&folder, 0o700);
    let path = folder.join(format!("{request}.json"));
    let temp = folder.join(format!(".{request}.tmp"));
    std::fs::write(&temp, body).unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, &path).unwrap();
    path
}
/// The CLI's own session record of `name` under the state root `state`.
fn session_record(state: &Path, name: &str, agent: &str, run: &str, conversation: &str) {
    use sha2::Digest;
    let file = state.join(format!("{:x}.json", sha2::Sha256::digest(name.as_bytes())));
    let temp = state.join(".record.tmp");
    std::fs::write(
        &temp,
        json!({"name": name, "agent": agent, "run_id": run, "conversation_id": conversation})
            .to_string(),
    )
    .unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, file).unwrap();
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
fn tables(db: &rusqlite::Connection) -> i64 {
    db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_independent_compact_to_send_leaf() {
    assert_eq!(
        SCHEMA - 1,
        crate::desktop::deferred_answer::SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let (_temp, location, store, _) = fixture();
    remove_fixture(&store.db);
    store
        .db
        .pragma_update(None, "user_version", SCHEMA - 1)
        .unwrap();
    let before = tables(&store.db);
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    // Its own two tables, then the session resume leaf's two.
    assert_eq!(tables(&store.db), before + 2 + 2);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    assert_eq!(
        store.operation_status()["kinds"]["compact_to_send"]["undrained"],
        0
    );
}

fn admit(store: &mut Store, verified: &Verified, writer: &str) -> ledger::Row {
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::admit(&tx, verified, writer, 1).unwrap();
    tx.commit().unwrap();
    row
}

#[test]
fn admission_is_immutable_and_the_same_operation_returns_only_its_original_receipt() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    let row = admit(&mut store, &verified, &writer);
    assert_eq!(row.receipt.compaction.step, Step::Queued);
    assert_eq!(row.receipt.send.step, Step::Queued);
    assert!(row.compaction.is_none() && row.send.is_none());
    // The same operation again: its original receipt, nothing new.
    let again = admit(&mut store, &verified, &writer);
    assert_eq!(again.receipt, row.receipt);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    // Another send under the same compaction conflicts.
    let mut other = verified.intent.clone();
    let checkpoint: Checkpoint = serde_json::from_str(&other.checkpoint).unwrap();
    let send = uuid::Uuid::new_v4().to_string();
    let mut changed = checkpoint.clone();
    changed.send_request_id = Some(send.clone());
    other.send_request_id = send.clone();
    other.checkpoint = serde_json::to_string(&changed).unwrap();
    let mut stdin: Value = serde_json::from_str(&other.original).unwrap();
    stdin["request_id"] = json!(send);
    other.original = stdin.to_string();
    let conflicting = other.verify().unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &conflicting, &writer, 2).is_err());
    drop(tx);
    // The original columns never change.
    assert!(store
        .db
        .execute(
            "UPDATE desktop_compact_to_sends SET source='{}' WHERE request_id=?1",
            [&verified.plan.compaction_request_id]
        )
        .is_err());
    assert!(store
        .db
        .execute("DELETE FROM desktop_compact_to_sends", [])
        .is_err());
}

/// desktop-u R1: only this ledger's own proof that a child never crossed its
/// reservation records it as never started; a retired operation never runs.
#[test]
fn retirement_proves_only_unreserved_children_never_started() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let row = admit(&mut store, &verified, &uuid::Uuid::new_v4().to_string());
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::retire(&tx, &row).unwrap();
    tx.commit().unwrap();
    let retired = ledger::load(&store.db, row.id()).unwrap().unwrap();
    assert_eq!(retired.receipt.compaction.step, Step::NotStarted);
    assert_eq!(retired.receipt.send.step, Step::NotStarted);
    assert!(retired.receipt.drained);
    assert_eq!(retired.receipt.code, ledger::INTERRUPTED);
    assert_eq!(
        retired.receipt.result(&verified.plan),
        Some(Settled::NotStarted)
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    // Nothing follows a drained operation.
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::retire(&tx, &retired).is_err());
}

/// desktop-u R1: the ledger writes only a receipt that follows the stored
/// one by one revision; a settled child is never overwritten.
#[test]
fn the_ledger_writes_only_a_following_receipt() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let write = |store: &mut Store, row: &ledger::Row, next: &wire::Receipt| {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let result = ledger::state(&tx, row, next);
        if result.is_ok() {
            tx.commit().unwrap();
        }
        result.is_ok()
    };
    let mut same = row.receipt.clone();
    same.revision += 2;
    assert!(!write(&mut store, &row, &same), "a skipped revision");
    let mut back = row.receipt.clone();
    back.revision += 1;
    back.compaction.step = Step::Queued;
    assert!(!write(&mut store, &row, &back), "back to Queued");
    // Settle the compaction, then try to overwrite it.
    let mut recorded = row.receipt.clone();
    recorded.revision += 1;
    recorded.compaction = wire::Component {
        step: Step::Recorded,
        code: String::new(),
        outcome: Some(wire::Compacted {
            request_id: verified.plan.compaction_request_id.clone(),
            status: wire::SUBMITTED.into(),
        }),
    };
    assert!(write(&mut store, &row, &recorded));
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let mut downgraded = row.receipt.clone();
    downgraded.revision += 1;
    downgraded.compaction = wire::Component {
        step: Step::NotStarted,
        code: wire::NOT_STARTED.into(),
        outcome: None,
    };
    downgraded.send = downgraded.compaction.clone().map_outcome();
    assert!(
        !write(&mut store, &row, &downgraded),
        "a recorded compaction overwritten"
    );
}
trait MapOutcome {
    fn map_outcome(self) -> wire::Component<zerus_client_core::desktop::deferred_send::Outcome>;
}
impl MapOutcome for wire::Component<wire::Compacted> {
    fn map_outcome(self) -> wire::Component<zerus_client_core::desktop::deferred_send::Outcome> {
        wire::Component {
            step: self.step,
            code: self.code,
            outcome: None,
        }
    }
}

/// A Queued child was never reserved: its own valid reservation beside it
/// refuses the row.
#[test]
fn a_queued_child_with_a_reservation_never_loads() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    zerus_desktop_ledger::authorize(&mut store.db, &writer, &epoch).unwrap();
    let row = admit(&mut store, &verified, &writer);
    let ticket = zerus_desktop_ledger::operations::Ticket {
        operation_id: verified.binding.operation_id().unwrap(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        revision: row.receipt.revision + 1,
        admission_id: verified.plan.compaction_request_id.clone(),
        binding_sha256: verified.binding.fingerprint().unwrap(),
        writer_epoch: writer,
        service_epoch: epoch.clone(),
        lease: uuid::Uuid::new_v4().to_string(),
    };
    let (prepared, _held) = crate::desktop::custody::prepare(&location, &ticket).unwrap();
    let record = zerus_desktop_ledger::operations::custody::Record {
        prepared,
        helper: None,
    };
    store
        .db
        .execute(
            "UPDATE desktop_compact_to_sends SET compaction_ticket=?1,compaction_custody=?2",
            [
                serde_json::to_string(&ticket).unwrap(),
                serde_json::to_string(&record).unwrap(),
            ],
        )
        .unwrap();
    let error = ledger::load(&store.db, &verified.plan.compaction_request_id)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("reservation without its child"),
        "{error}"
    );
}

#[test]
fn capacity_refusal_is_atomic() {
    let (_temp, location, mut store, epoch) = fixture();
    store
        .db
        .execute(
            "UPDATE desktop_compact_to_send_budget SET active=32 WHERE id=1",
            [],
        )
        .unwrap();
    let verified = original(&location.configuration, &epoch);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &verified, &uuid::Uuid::new_v4().to_string(), 1).is_err());
    drop(tx);
    let rows: i64 = store
        .db
        .query_row("SELECT count(*) FROM desktop_compact_to_sends", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

/// Reserves the compaction under a real custody prepared for the row and
/// returns the exact record and its held lock.
fn running(
    store: &mut Store,
    location: &Location,
    verified: &Verified,
    epoch: &str,
) -> (
    zerus_desktop_ledger::operations::custody::Record,
    std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
) {
    let writer = uuid::Uuid::new_v4().to_string();
    zerus_desktop_ledger::authorize(&mut store.db, &writer, epoch).unwrap();
    let row = admit(store, verified, &writer);
    let ticket = zerus_desktop_ledger::operations::Ticket {
        operation_id: verified.binding.operation_id().unwrap(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        revision: row.receipt.revision + 1,
        admission_id: verified.plan.compaction_request_id.clone(),
        binding_sha256: verified.binding.fingerprint().unwrap(),
        writer_epoch: writer,
        service_epoch: epoch.into(),
        lease: uuid::Uuid::new_v4().to_string(),
    };
    let (prepared, held) = crate::desktop::custody::prepare(location, &ticket).unwrap();
    let record = zerus_desktop_ledger::operations::custody::Record {
        prepared,
        helper: None,
    };
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::reserve(&tx, &row, Child::Compaction, &ticket, &record).unwrap();
    tx.commit().unwrap();
    let row = ledger::load(&store.db, &verified.plan.compaction_request_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.receipt.compaction.step, Step::Running);
    assert_eq!(row.draining(), Some(Child::Compaction));
    // The send can never be reserved while the compaction runs.
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::reserve(&tx, &row, Child::Send, &ticket, &record).is_err());
    drop(tx);
    (record, held)
}

/// Observes `record` until its release, within a bound.
fn released(
    location: &Location,
    record: &zerus_desktop_ledger::operations::custody::Record,
) -> crate::desktop::custody::Observation {
    use crate::desktop::custody::{probe, Observation};
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let observation = probe(location, record);
        if matches!(observation, Observation::Released { .. }) || std::time::Instant::now() >= until
        {
            return observation;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn only_the_released_custody_settles_the_compaction_and_only_its_exact_cli_record_decides() {
    use super::receipt::{Observed, MISSING, UNAVAILABLE};
    use crate::desktop::custody::{self, Observation};
    #[derive(Clone, Copy, Debug)]
    enum Ending {
        Exited,
        NotStarted,
        Unknown,
    }
    type Read = Box<dyn Fn(&wire::Plan) -> Observed>;
    let exact: fn(&wire::Plan) -> Observed = |plan| {
        Observed::Compacted(
            wire::Compacted::from_receipt(
                plan,
                cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
            )
            .unwrap(),
        )
    };
    let other: fn(&wire::Plan) -> Observed = |plan| {
        Observed::Compacted(wire::Compacted {
            request_id: plan.send_request_id.clone(),
            status: wire::SUBMITTED.into(),
        })
    };
    let unknown = |code: &'static str| -> Read { Box::new(move |_| Observed::Unknown(code)) };
    // An absence exists only as a read of a held store that has no receipt.
    let missing = || -> Read {
        Box::new(|plan| {
            let temp = tempfile::tempdir().unwrap();
            let state = state_root(&temp.path().canonicalize().unwrap());
            let folder = state.join(super::receipt::COMPACTION_FOLDER);
            std::fs::create_dir(&folder).unwrap();
            mode(&folder, 0o700);
            let observed = observe(&state, plan);
            assert!(matches!(observed, Observed::Missing(_)), "{observed:?}");
            observed
        })
    };
    use Step::*;
    let cases: Vec<(Ending, Read, Step, &str, Settled)> = vec![
        // The exit proves nothing; only the CLI's own record decides.
        (
            Ending::Exited,
            Box::new(exact),
            Recorded,
            "",
            Settled::NotSent { compacted: true },
        ),
        (
            Ending::Unknown,
            Box::new(exact),
            Recorded,
            "",
            Settled::NotSent { compacted: true },
        ),
        (
            Ending::NotStarted,
            Box::new(exact),
            Recorded,
            "",
            Settled::NotSent { compacted: true },
        ),
        (
            Ending::Exited,
            missing(),
            Unknown,
            MISSING,
            Settled::NotSent { compacted: false },
        ),
        (
            Ending::Exited,
            unknown(wire::RECEIPT_IN_PROGRESS),
            Unknown,
            wire::RECEIPT_IN_PROGRESS,
            Settled::NotSent { compacted: false },
        ),
        (
            Ending::Exited,
            unknown(wire::RECEIPT_UNCERTAIN),
            Unknown,
            wire::RECEIPT_UNCERTAIN,
            Settled::NotSent { compacted: false },
        ),
        (
            Ending::Exited,
            Box::new(other),
            Unknown,
            wire::RECEIPT_MISMATCH,
            Settled::NotSent { compacted: false },
        ),
        (
            Ending::Exited,
            unknown(UNAVAILABLE),
            Unknown,
            UNAVAILABLE,
            Settled::NotSent { compacted: false },
        ),
        // The closed pre-child result: never started only when no record
        // exists; then the send never follows either.
        (
            Ending::NotStarted,
            missing(),
            NotStarted,
            wire::NOT_STARTED,
            Settled::NotStarted,
        ),
        // A missing receipt's code that no held read produced proves
        // nothing.
        (
            Ending::NotStarted,
            unknown(MISSING),
            Unknown,
            MISSING,
            Settled::NotSent { compacted: false },
        ),
        (
            Ending::NotStarted,
            unknown(UNAVAILABLE),
            Unknown,
            UNAVAILABLE,
            Settled::NotSent { compacted: false },
        ),
        // The child's own reason stays unless its exact record exists.
        (
            Ending::Unknown,
            missing(),
            Unknown,
            "compact_to_send_timed_out",
            Settled::NotSent { compacted: false },
        ),
    ];
    for (index, (ending, read, step, code, settled)) in cases.into_iter().enumerate() {
        let (_temp, location, mut store, epoch) = fixture();
        let verified = original(&location.configuration, &epoch);
        let (record, held) = running(&mut store, &location, &verified, &epoch);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        match ending {
            Ending::Exited => ledger::ended(&tx, &row, ledger::AWAITING_DRAIN).unwrap(),
            Ending::NotStarted => ledger::not_started(&tx, &row).unwrap(),
            Ending::Unknown => ledger::ended(&tx, &row, "compact_to_send_timed_out").unwrap(),
        }
        tx.commit().unwrap();
        // Nothing settles before the drain, whatever the receipt says.
        let row = ledger::pending(&store.db).unwrap().unwrap();
        assert_eq!(row.receipt.compaction.step, Running, "{index}");
        assert!(row.receipt.result(&verified.plan).is_none());
        let observation = custody::probe(&location, &record);
        assert!(matches!(observation, Observation::Held));
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::drained(
            tx,
            row,
            observation,
            &read(&verified.plan),
            Default::default(),
        )
        .is_err());
        drop(held);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let observation = released(&location, &record);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let receipt = ledger::drained(
            tx,
            row,
            observation,
            &read(&verified.plan),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            (receipt.compaction.step, receipt.compaction.code.as_str()),
            (step, code),
            "{index}: {ending:?}"
        );
        assert!(receipt.valid(), "{index}");
        if step == NotStarted {
            // A send never follows a compaction that never started.
            assert_eq!(receipt.send.step, NotStarted, "{index}");
            assert!(receipt.drained, "{index}");
        } else {
            // The same live operation now waits: its send is still Queued
            // and nothing settles.
            assert_eq!(receipt.send.step, Queued, "{index}");
            assert!(!receipt.drained, "{index}");
            assert_eq!(receipt.result(&verified.plan), None, "{index}");
            // A wait that ends without the send: never started, from the
            // ledger, for its closed reason.
            let row = ledger::pending(&store.db).unwrap().unwrap();
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            ledger::wait_ended(&tx, &row, ledger::WAIT_EXPIRED).unwrap();
            tx.commit().unwrap();
        }
        let stored = ledger::load(&store.db, &verified.plan.compaction_request_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.receipt.result(&verified.plan),
            Some(settled),
            "{index}"
        );
        assert!(stored.send.is_none(), "the send was never reserved");
        assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    }
}

/// A restart retires an operation whose compaction may run: the send is
/// proven never started from the ledger, the compaction keeps its
/// reservation for the read-only drain, and an unheld one drains Unknown
/// without reading any receipt.
#[test]
fn restart_keeps_a_reserved_compaction_for_its_drain_and_never_reads_an_unheld_receipt() {
    use super::receipt::UNHELD;
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (record, held) = running(&mut store, &location, &verified, &epoch);
    let state = location.root.parent().unwrap().to_owned();
    // Even an exact receipt is never read for an unheld original.
    write_receipt(
        &state,
        &verified.plan.compaction_request_id,
        cli_record(&verified.plan, wire::SUBMITTED)
            .to_string()
            .as_bytes(),
    );
    ledger::startup(&mut store.db).unwrap();
    let row = ledger::pending(&store.db).unwrap().unwrap();
    assert_eq!(row.receipt.compaction.step, Step::Running);
    assert_eq!(row.receipt.send.step, Step::NotStarted);
    assert!(!row.receipt.drained);
    // A second restart changes nothing about either child.
    ledger::startup(&mut store.db).unwrap();
    let again = ledger::pending(&store.db).unwrap().unwrap();
    assert_eq!(
        (again.receipt.compaction.clone(), again.receipt.send.clone()),
        (row.receipt.compaction.clone(), row.receipt.send.clone())
    );
    drop(held);
    let observation = released(&location, &record);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let receipt = ledger::unheld(tx, again, observation).unwrap();
    assert_eq!(
        (receipt.compaction.step, receipt.compaction.code.as_str()),
        (Step::Unknown, UNHELD)
    );
    assert_eq!(
        receipt.result(&verified.plan),
        Some(Settled::NotSent { compacted: false })
    );
}

/// Store recovery settles an original no live runtime holds as Unknown
/// (UNHELD), never as never started, even when its spawn turn recorded the
/// pre-child result: it holds no receipt store to prove that no other
/// dispatch ran the request.
#[test]
fn store_recovery_never_proves_an_unheld_pre_child_result() {
    use super::receipt::UNHELD;
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, held) = running(&mut store, &location, &verified, &epoch);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::load(&tx, &verified.plan.compaction_request_id)
        .unwrap()
        .unwrap();
    ledger::not_started(&tx, &row).unwrap();
    tx.commit().unwrap();
    drop(held);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let receipt = loop {
        store.recover_operation_custody(&location);
        let row = ledger::load(&store.db, &verified.plan.compaction_request_id)
            .unwrap()
            .unwrap();
        if row.receipt.drained {
            break row.receipt;
        }
        assert!(
            std::time::Instant::now() < until,
            "recovery never settled it"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(
        (receipt.compaction.step, receipt.compaction.code.as_str()),
        (Step::Unknown, UNHELD)
    );
    // No runtime is left to continue it: the queued send never starts.
    assert_eq!(
        (receipt.send.step, receipt.code.as_str()),
        (Step::NotStarted, ledger::INTERRUPTED)
    );
    // No runtime is left to continue it: the queued send never starts.
    assert_eq!(
        (receipt.send.step, receipt.code.as_str()),
        (Step::NotStarted, ledger::INTERRUPTED)
    );
    assert_eq!(
        receipt.result(&verified.plan),
        Some(Settled::NotSent { compacted: false })
    );
}

/// desktop-u R1 negatives: an unreadable reservation is no proof that a
/// child never started; it stops recovery instead.
#[test]
fn an_unreadable_reservation_never_proves_a_child_never_started() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    store
        .db
        .execute_batch("DROP TRIGGER desktop_compact_to_send_compaction_custody;")
        .unwrap();
    for broken in ["{", "null"] {
        store
            .db
            .execute(
                "UPDATE desktop_compact_to_sends SET compaction_custody=?1",
                [broken],
            )
            .unwrap();
        assert!(ledger::load(&store.db, &verified.plan.compaction_request_id).is_err());
        assert!(ledger::startup(&mut store.db).is_err());
        let steps: (String, String) = store
            .db
            .query_row(
                "SELECT compaction_step,send_step FROM desktop_compact_to_sends",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            steps,
            ("running".to_owned(), "queued".to_owned()),
            "{broken}"
        );
    }
}

#[test]
fn the_lookup_reads_only_its_exact_operation() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    admit(&mut store, &verified, &uuid::Uuid::new_v4().to_string());
    let scope = wire::Scope {
        configuration: location.configuration.clone(),
        service_epoch: epoch.clone(),
    };
    let query = wire::Query::for_plan(scope.clone(), &verified.plan).unwrap();
    let read = |value: Value| query_value(&store, value);
    let lookup: wire::Lookup =
        serde_json::from_value(read(json!({"op":"compact_to_send_result","query":query}))).unwrap();
    assert!(lookup.matches(&query) && lookup.found);
    assert_eq!(
        lookup.receipt.unwrap().binding,
        verified.binding,
        "its own operation"
    );
    // Another kind's op, another scope or an unknown operation never reads.
    for value in [
        json!({"op":"deferred_send_result","query":query}),
        json!({"op":"compact_to_send_result","query":query,"extra":1}),
    ] {
        assert_eq!(read(value)["code"], "compact_to_send_result_unavailable");
    }
    let mut other = query.clone();
    other.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        read(json!({"op":"compact_to_send_result","query":other}))["code"],
        "compact_to_send_result_unavailable"
    );
    let mut missing = query.clone();
    missing.request_id = verified.plan.send_request_id.clone();
    let none: wire::Lookup =
        serde_json::from_value(read(json!({"op":"compact_to_send_result","query":missing})))
            .unwrap();
    assert!(!none.found && none.receipt.is_none());
}
fn query_value(store: &Store, value: Value) -> Value {
    super::query(store, &value)
}

fn state_root(parent: &Path) -> PathBuf {
    let state = parent.join("state");
    std::fs::create_dir(&state).unwrap();
    mode(&state, 0o700);
    state
}
fn observe(state: &Path, plan: &wire::Plan) -> super::receipt::Observed {
    let roots = super::receipt::Roots::open(state, super::receipt::COMPACTION_FOLDER).unwrap();
    super::receipt::compaction(&roots, plan, &|| ()).0
}

#[test]
fn the_receipt_observation_reads_only_the_exact_private_submitted_record() {
    use super::receipt::{Observed, CHANGED};
    let temp = tempfile::tempdir().unwrap();
    let state = state_root(&temp.path().canonicalize().unwrap());
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let plan = &verified.plan;
    let id = &plan.compaction_request_id;
    write_receipt(&state, id, b"{}");
    std::fs::remove_file(state.join("compact_receipts").join(format!("{id}.json"))).unwrap();
    assert!(matches!(observe(&state, plan), Observed::Missing(_)));
    write_receipt(
        &state,
        id,
        cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
    );
    assert!(matches!(observe(&state, plan), Observed::Compacted(o) if o.matches(plan)));
    // The CLI's placeholder before any input, and its own uncertain end.
    write_receipt(&state, id, br#"{"status":"in_progress"}"#);
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(wire::RECEIPT_IN_PROGRESS)
    );
    write_receipt(
        &state,
        id,
        cli_record(plan, "uncertain").to_string().as_bytes(),
    );
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(wire::RECEIPT_UNCERTAIN)
    );
    let mut other = cli_record(plan, wire::SUBMITTED);
    other["conversation_id"] = json!("other");
    write_receipt(&state, id, other.to_string().as_bytes());
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(wire::RECEIPT_MISMATCH)
    );
    // A receipt that is not private is never read.
    let path = write_receipt(
        &state,
        id,
        cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
    );
    mode(&path, 0o644);
    assert!(matches!(observe(&state, plan), Observed::Unknown(_)));
    mode(&path, 0o600);
    // A receipt that changes while it is read is Unknown.
    let roots = super::receipt::Roots::open(&state, super::receipt::COMPACTION_FOLDER).unwrap();
    let changed = super::receipt::compaction(&roots, plan, &|| {
        write_receipt(
            &state,
            id,
            cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
        );
    })
    .0;
    assert_eq!(changed, Observed::Unknown(CHANGED));
}

/// The compaction slot the CLI's hooks keep for `plan` with `status`.
fn slot_of(plan: &wire::Plan, status: &str) -> Value {
    json!({"request_id": plan.compaction_request_id, "run_id": plan.run_id,
        "conversation_id": plan.conversation_id, "status": status})
}
/// The CLI's own session record of `plan` with `slot`, idle or busy.
fn slot_record(state: &Path, plan: &wire::Plan, slot: Value, idle: bool) {
    use sha2::Digest;
    let file = state.join(format!(
        "{:x}.json",
        sha2::Sha256::digest(plan.session.as_bytes())
    ));
    let temp = state.join(".record.tmp");
    let (activity, phase) = if idle {
        ("idle", "idle")
    } else {
        ("busy", "working")
    };
    std::fs::write(
        &temp,
        json!({"name": plan.session, "agent": "claude", "run_id": plan.run_id,
            "conversation_id": plan.conversation_id, "activity": activity, "phase": phase,
            "compact_context_request": slot})
        .to_string(),
    )
    .unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, file).unwrap();
}
/// The CLI's own exact input receipt of the plan's send.
fn sent_record(state: &Path, plan: &wire::Plan) {
    let folder = state.join("input_receipts");
    if !folder.exists() {
        std::fs::create_dir(&folder).unwrap();
    }
    mode(&folder, 0o700);
    let id = &plan.send_request_id;
    let temp = folder.join(format!(".{id}.tmp"));
    std::fs::write(
        &temp,
        json!({"request_id": id, "name": plan.session, "run_id": plan.run_id,
            "conversation_id": plan.conversation_id, "digest": plan.input_sha256,
            "status": "submitted", "transport": "terminal"})
        .to_string(),
    )
    .unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, folder.join(format!("{id}.json"))).unwrap();
}

/// The slot rule: only this compaction completed, not superseded, on an
/// idle session lets the send start; pending states wait; anything else
/// ends the wait.
#[test]
fn only_this_completed_compaction_on_an_idle_session_lets_the_send_start() {
    use super::slot::{classify, Slot, SLOT_ENDED};
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let plan = &verified.plan;
    let record = |slot: Value, activity: &str, phase: &str| json!({"activity": activity, "phase": phase, "compact_context_request": slot});
    assert_eq!(
        classify(&record(slot_of(plan, "completed"), "idle", "idle"), plan),
        Slot::Completed
    );
    for (slot, activity, phase) in [
        (slot_of(plan, "submitted"), "idle", "idle"),
        (slot_of(plan, "compacting"), "busy", "working"),
        (slot_of(plan, "completed"), "busy", "idle"),
        (slot_of(plan, "completed"), "idle", "working"),
    ] {
        assert_eq!(
            classify(&record(slot.clone(), activity, phase), plan),
            Slot::Waiting,
            "{slot}"
        );
    }
    let mut superseded = slot_of(plan, "completed");
    superseded["superseded_at"] = json!(7.5);
    let mut other = slot_of(plan, "completed");
    other["request_id"] = json!(plan.send_request_id);
    let mut run = slot_of(plan, "completed");
    run["run_id"] = json!("run-2");
    let mut conversation = slot_of(plan, "completed");
    conversation["conversation_id"] = json!("other");
    for slot in [
        slot_of(plan, "cancelled"),
        slot_of(plan, "uncertain"),
        slot_of(plan, ""),
        superseded,
        other,
        run,
        conversation,
        Value::Null,
    ] {
        assert_eq!(
            classify(&record(slot.clone(), "idle", "idle"), plan),
            Slot::Ended(SLOT_ENDED),
            "{slot}"
        );
    }
}

/// Only an operation whose compaction settled and whose send was never
/// reserved ends its wait, once, for a closed reason.
#[test]
fn only_a_waiting_operation_ends_its_wait() {
    use super::receipt::Observed;
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (record, held) = running(&mut store, &location, &verified, &epoch);
    let end = |store: &mut Store, code: &str| {
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let result = ledger::wait_ended(&tx, &row, code);
        if result.is_ok() {
            tx.commit().unwrap();
        }
        result.is_ok()
    };
    assert!(
        !end(&mut store, ledger::WAIT_EXPIRED),
        "the compaction still runs"
    );
    drop(held);
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let observation = released(&location, &record);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::drained(
        tx,
        row,
        observation,
        &Observed::Compacted(wire::Compacted {
            request_id: verified.plan.compaction_request_id.clone(),
            status: wire::SUBMITTED.into(),
        }),
        Default::default(),
    )
    .unwrap();
    assert!(!end(&mut store, ""), "a closed reason");
    assert!(end(&mut store, ledger::VETOED));
    let receipt = ledger::load(&store.db, &verified.plan.compaction_request_id)
        .unwrap()
        .unwrap()
        .receipt;
    assert_eq!(
        (receipt.send.step, receipt.code.as_str(), receipt.drained),
        (Step::NotStarted, ledger::VETOED, true)
    );
    assert_eq!(
        receipt.result(&verified.plan),
        Some(Settled::NotSent { compacted: true })
    );
    assert!(ledger::pending(&store.db).unwrap().is_none());
}

/// A later child's store is the held original root itself, never a
/// replacement the state path names since.
#[test]
fn a_sibling_store_is_the_held_root_never_a_replacement() {
    use super::receipt::{Roots, COMPACTION_FOLDER, INPUT_FOLDER};
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let state = state_root(&parent);
    let roots = Roots::open(&state, COMPACTION_FOLDER).unwrap();
    assert!(roots.sibling(INPUT_FOLDER).unwrap().root_current());
    std::fs::rename(&state, parent.join("moved")).unwrap();
    std::fs::create_dir(&state).unwrap();
    mode(&state, 0o700);
    assert!(!roots.root_current());
    assert!(roots.sibling(INPUT_FOLDER).is_err());
}

mod actor {
    use super::super::child::Ended;
    use super::*;
    use crate::{config::Config, desktop::ui_authority};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use zerus_client_core::run::custody::InheritedLock;

    type Execute = dyn Fn(
            &Path,
            &wire::Plan,
            &[String],
            &[u8],
            &Arc<InheritedLock>,
            &Arc<dyn Send + Sync>,
        ) -> Ended
        + Send
        + Sync;

    /// An executor closure behind [`Execute`], its signature inferred.
    fn erased(
        execute: impl Fn(
                &Path,
                &wire::Plan,
                &[String],
                &[u8],
                &Arc<InheritedLock>,
                &Arc<dyn Send + Sync>,
            ) -> Ended
            + Send
            + Sync
            + 'static,
    ) -> Arc<Execute> {
        Arc::new(execute)
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        store: Store,
        runtime: runtime::Runtime,
        exclusion: Arc<crate::desktop::store::HandoffLock>,
        location: Location,
        epoch: String,
        config: Config,
        owner: Arc<crate::platform::FileLock>,
        /// The epoch's sealed negatives: every runtime of this epoch shares them.
        fences: Arc<crate::desktop::not_admitted::Fences>,
    }
    impl Fixture {
        fn new(
            execute: impl Fn(
                    &Path,
                    &wire::Plan,
                    &[String],
                    &[u8],
                    &Arc<InheritedLock>,
                    &Arc<dyn Send + Sync>,
                ) -> Ended
                + Send
                + Sync
                + 'static,
        ) -> Self {
            Self::build(Some(erased(execute)))
        }
        fn with(execute: Arc<Execute>) -> Self {
            Self::build(Some(execute))
        }
        /// No executor: the original path runs through the pinned image.
        #[cfg(target_os = "linux")]
        fn unexecuted() -> Self {
            Self::build(None)
        }
        fn build(execute: Option<Arc<Execute>>) -> Self {
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
            assert_eq!(location.root.parent().unwrap(), config.state);
            session_record(&config.state, SESSION, "claude", RUN, CONVERSATION);
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
            let fences = Arc::new(crate::desktop::not_admitted::Fences::default());
            let mut runtime =
                runtime::Runtime::new(&config, &epoch, owner.clone(), fences.clone()).unwrap();
            Self::install(&mut runtime, &config.state, execute);
            runtime.poll(&mut store, Some(exclusion.clone()));
            Self {
                _temp: temp,
                store,
                runtime,
                exclusion,
                location,
                epoch,
                config,
                owner,
                fences,
            }
        }
        /// The fixture's executor and macOS fence on `runtime`.
        fn install(runtime: &mut runtime::Runtime, state: &Path, execute: Option<Arc<Execute>>) {
            match execute {
                Some(execute) => {
                    let state = state.to_path_buf();
                    runtime.fixture_executor(
                        MACHINE,
                        Arc::new(move |plan, args, input, lock, resources| {
                            execute(&state, plan, args, input, lock, resources)
                        }),
                    )
                }
                None => runtime.fixture_machine(MACHINE),
            }
            runtime.fixture_mac_fence(false);
        }
        /// Destroys the runtime and builds it again under the same epoch,
        /// as the service does when it restarts its background runtime.
        fn rebuild(&mut self, execute: Arc<Execute>) {
            let torn = std::mem::replace(
                &mut self.runtime,
                runtime::Runtime::new(&self.config, &self.epoch, self.owner.clone(), self.fences.clone())
                    .unwrap(),
            );
            drop(torn);
            Self::install(&mut self.runtime, &self.config.state, Some(execute));
            self.runtime
                .poll(&mut self.store, Some(self.exclusion.clone()));
        }
        fn original(&self) -> Verified {
            super::original(&self.location.configuration, &self.epoch)
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            let request = json!(
                zerus_client_core::desktop::ui_requests::Request::CompactToSend {
                    intent: Box::new(original.intent.clone()),
                }
            );
            serde_json::from_value(self.runtime.frontend(
                &mut self.store,
                ui_authority::Validated::fixture(&request),
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
            ledger::load(&self.store.db, &original.plan.compaction_request_id)
                .unwrap()
                .unwrap()
        }
        fn revoke(&mut self) {
            self.store
                .db
                .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                .unwrap();
        }
    }
    fn compacted(state: &Path, plan: &wire::Plan) {
        write_receipt(
            state,
            &plan.compaction_request_id,
            cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
        );
    }

    /// Counts each child the executor ran, by its command.
    #[derive(Default)]
    struct Ran {
        compactions: AtomicUsize,
        sends: AtomicUsize,
    }
    impl Ran {
        fn counts(&self) -> (usize, usize) {
            (
                self.compactions.load(Ordering::SeqCst),
                self.sends.load(Ordering::SeqCst),
            )
        }
    }
    /// An executor whose compaction writes its receipt (when `receipt`) and
    /// leaves the slot `slot` on an idle session, and whose send writes its
    /// receipt (when `sent`).
    fn executor(ran: Arc<Ran>, receipt: bool, slot: &'static str, sent: bool) -> Arc<Execute> {
        erased(move |state, plan, args, input, _, _| {
            match args[0].as_str() {
                "compact-context" => {
                    ran.compactions.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(args, ["compact-context", SESSION, "--json"]);
                    assert_eq!(
                        format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(input)),
                        plan.compaction_sha256
                    );
                    if receipt {
                        compacted(state, plan);
                    }
                    slot_record(state, plan, slot_of(plan, slot), true);
                }
                "send" => {
                    ran.sends.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(args, ["send", SESSION, "--json"]);
                    let stdin: Value = serde_json::from_slice(input).unwrap();
                    assert_eq!(stdin["request_id"], plan.send_request_id.as_str());
                    assert_eq!(
                        stdin["expected_compaction_id"],
                        plan.compaction_request_id.as_str()
                    );
                    assert_eq!(
                        format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(input)),
                        plan.input_sha256
                    );
                    if sent {
                        sent_record(state, plan);
                    }
                }
                other => panic!("unexpected child {other}"),
            }
            Ended::Exited { code: 0 }
        })
    }

    #[test]
    fn a_completed_compaction_sends_once_and_settles_from_both_receipts() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "completed", true));
        let original = f.original();
        let admission = f.admit(&original);
        assert!(admission.accepted && admission.durable && admission.matches(&original.intent));
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(row.receipt.compaction.step, Step::Recorded);
        assert_eq!(row.receipt.send.step, Step::Recorded);
        assert!(row.compaction.is_some() && row.send.is_some());
        let sent = row.receipt.send.outcome.clone().unwrap();
        assert_eq!(
            row.receipt.result(&original.plan),
            Some(Settled::Sent(sent))
        );
        // The same operation admitted again returns its settled receipt.
        assert_eq!(f.admit(&original).receipt.unwrap(), row.receipt);
        f.runtime.request_stop();
        f.until("stopped", |f| f.runtime.drained());
        assert_eq!(ran.counts(), (1, 1), "each child once");
    }

    /// No send while the compaction's custody is held, even with its slot
    /// already completed: the send follows only its physical drain.
    #[test]
    fn the_send_waits_for_the_compactions_physical_drain() {
        let ran = Arc::new(Ran::default());
        let inner = executor(ran.clone(), true, "completed", true);
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = std::sync::Mutex::new(released);
        let mut f = Fixture::new(move |state, plan, args, input, lock, resources| {
            let ended = inner(state, plan, args, input, lock, resources);
            if args[0] == "compact-context" {
                entered_tx.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            ended
        });
        let original = f.original();
        f.admit(&original);
        f.until("compacting", |f| {
            f.row(&original).receipt.compaction.step == Step::Running
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..50 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(ran.counts(), (1, 0), "no send before the drain");
        assert_eq!(f.row(&original).receipt.send.step, Step::Queued);
        release.send(()).unwrap();
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(ran.counts(), (1, 1));
    }

    /// The wait ends without a send when the slot ends, the session changes,
    /// the budget expires or the operation's deadline passes.
    /// A slot read that ends after the wait's own budget, however it
    /// began, starts nothing: the send never starts past the budget, even
    /// when the slot completed meanwhile (the zerus-5wmh.66.4.9.1 class).
    #[test]
    fn a_completed_slot_read_that_ends_after_the_budget_never_sends() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "submitted", true));
        f.runtime.fixture_wait(
            Duration::from_millis(500),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.original();
        let plan = original.plan.clone();
        let state = f.config.state.clone();
        let first = Arc::new(AtomicUsize::new(0));
        f.runtime.fixture_slot_hook(Arc::new(move || {
            // The first read outlives the budget, and only then finds the
            // compaction completed on an idle session.
            if first.fetch_add(1, Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(700));
                slot_record(&state, &plan, slot_of(&plan, "completed"), true);
            }
        }));
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.compaction.step, Step::Recorded);
        assert_eq!(
            (receipt.send.step, receipt.code.as_str()),
            (Step::NotStarted, ledger::WAIT_EXPIRED)
        );
        assert_eq!(ran.counts(), (1, 0));
    }

    #[test]
    fn the_wait_ends_without_a_send_for_its_closed_reason() {
        use super::super::slot::{SESSION_CHANGED, SLOT_ENDED};
        let cases: [(&str, &'static str, bool, &str); 5] = [
            ("cancelled", "cancelled", true, SLOT_ENDED),
            ("uncertain", "uncertain", true, SLOT_ENDED),
            ("pending", "submitted", true, ledger::WAIT_EXPIRED),
            ("busy", "completed", false, ledger::WAIT_EXPIRED),
            ("session", "completed", true, SESSION_CHANGED),
        ];
        for (name, slot, idle, code) in cases {
            let ran = Arc::new(Ran::default());
            let inner = executor(ran.clone(), true, slot, true);
            let mut f = Fixture::new(move |state, plan, args, input, lock, resources| {
                let ended = inner(state, plan, args, input, lock, resources);
                if !idle {
                    slot_record(state, plan, slot_of(plan, slot), false);
                }
                if name == "session" {
                    session_record(state, SESSION, "claude", "run-2", CONVERSATION);
                }
                ended
            });
            f.runtime.fixture_wait(
                Duration::from_millis(80),
                Duration::from_millis(5),
                runtime::CHAIN,
            );
            let original = f.original();
            f.admit(&original);
            f.until(name, |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(receipt.compaction.step, Step::Recorded, "{name}");
            assert_eq!(
                (receipt.send.step, receipt.code.as_str()),
                (Step::NotStarted, code),
                "{name}"
            );
            assert_eq!(
                receipt.result(&original.plan),
                Some(Settled::NotSent { compacted: true }),
                "{name}"
            );
            assert!(f.row(&original).send.is_none(), "{name}: never reserved");
            assert_eq!(ran.counts(), (1, 0), "{name}");
        }
        // The operation's own deadline can expire before the compaction starts
        // or cap the wait after its drain; each closed outcome stays exact.
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "submitted", true));
        f.runtime.fixture_wait(
            runtime::WAIT,
            Duration::from_millis(5),
            Duration::from_millis(300),
        );
        let original = f.original();
        f.admit(&original);
        f.until("deadline", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        match (receipt.compaction.step, receipt.send.step) {
            (Step::NotStarted, Step::NotStarted) => {
                assert_eq!(receipt.compaction.code, wire::NOT_STARTED);
                assert_eq!(receipt.send.code, wire::NOT_STARTED);
                assert_eq!(receipt.code, "");
                assert_eq!(receipt.result(&original.plan), Some(Settled::NotStarted));
                assert_eq!(ran.counts(), (0, 0));
            }
            (Step::Recorded, Step::NotStarted) => {
                assert_eq!(receipt.compaction.code, "");
                assert_eq!(receipt.send.code, wire::NOT_STARTED);
                assert_eq!(receipt.code, ledger::CHAIN_EXPIRED);
                assert_eq!(
                    receipt.result(&original.plan),
                    Some(Settled::NotSent { compacted: true })
                );
                assert_eq!(ran.counts(), (1, 0));
            }
            (compaction, send) => {
                panic!("unexpected deadline outcome: {compaction:?}/{send:?}")
            }
        }
        assert!(f.row(&original).send.is_none(), "never reserved");
    }

    /// A stop or a revoke ends the wait at once, never after its budget.
    #[test]
    fn a_stop_or_revoke_during_the_wait_ends_it_at_once() {
        for action in ["stop", "revoke"] {
            let ran = Arc::new(Ran::default());
            let mut f = Fixture::with(executor(ran.clone(), true, "submitted", true));
            let original = f.original();
            f.admit(&original);
            f.until("waiting", |f| {
                let receipt = f.row(&original).receipt;
                receipt.compaction.step == Step::Recorded && !receipt.drained
            });
            let started = Instant::now();
            match action {
                "stop" => f.runtime.request_stop(),
                _ => f.revoke(),
            }
            f.until(action, |f| f.row(&original).receipt.drained);
            assert!(started.elapsed() < Duration::from_secs(5), "{action}");
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.send.step, receipt.code.as_str()),
                (Step::NotStarted, ledger::VETOED),
                "{action}"
            );
            assert_eq!(ran.counts(), (1, 0), "{action}");
        }
    }

    /// desktop-u Q1: a compaction whose own receipt is missing stays
    /// Unknown, yet a completed slot (here, as a manual `/compact` in
    /// Terminal would leave it) lets the send start; the slot proves nothing
    /// about the original compaction.
    #[test]
    fn a_completed_slot_sends_after_an_unknown_compaction_which_stays_unknown() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), false, "completed", true));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.compaction.step, receipt.compaction.code.as_str()),
            (Step::Unknown, super::super::receipt::MISSING)
        );
        assert_eq!(receipt.send.step, Step::Recorded);
        assert!(matches!(
            receipt.result(&original.plan),
            Some(Settled::Sent(_))
        ));
        assert_eq!(ran.counts(), (1, 1));
    }

    /// desktop-u (2): a send the CLI refused after it may have started (its
    /// check printed, no receipt written) is Unknown, never a refusal.
    #[test]
    fn a_send_refused_after_spawn_without_its_receipt_stays_unknown() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "completed", false));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.send.step, receipt.send.code.as_str()),
            (Step::Unknown, super::super::receipt::MISSING)
        );
        assert_eq!(receipt.result(&original.plan), Some(Settled::Unknown));
        assert_eq!(ran.counts(), (1, 1));
    }

    /// The spawn turn reads the slot again: one that ended since the wait
    /// starts no send, which then never started; an exact earlier receipt
    /// of the send still wins over that no-spawn result.
    #[test]
    fn the_spawn_turn_rechecks_the_slot_and_an_earlier_receipt_wins() {
        for earlier in [false, true] {
            let ran = Arc::new(Ran::default());
            let mut f = Fixture::with(executor(ran.clone(), true, "completed", true));
            let original = f.original();
            let plan = original.plan.clone();
            let state = f.config.state.clone();
            let preparations = Arc::new(AtomicUsize::new(0));
            let count = preparations.clone();
            f.runtime.fixture_prepare_hook(Arc::new(move || {
                // The second preparation is the send's.
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    slot_record(&state, &plan, slot_of(&plan, "cancelled"), true);
                    if earlier {
                        sent_record(&state, &plan);
                    }
                }
            }));
            f.admit(&original);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert!(f.row(&original).send.is_some(), "{earlier}: reserved");
            if earlier {
                assert_eq!(receipt.send.step, Step::Recorded);
            } else {
                assert_eq!(
                    (receipt.send.step, receipt.send.code.as_str()),
                    (Step::NotStarted, wire::NOT_STARTED)
                );
                assert_eq!(
                    receipt.result(&original.plan),
                    Some(Settled::NotSent { compacted: true })
                );
            }
            assert_eq!(ran.counts(), (1, 0), "{earlier}: no send child");
        }
    }

    /// desktop-u R1: a restart during the wait never sends; the send was
    /// never reserved, so this ledger proves it never started.
    #[test]
    fn a_restart_during_the_wait_never_sends() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "submitted", true));
        let original = f.original();
        f.admit(&original);
        f.until("waiting", |f| {
            let receipt = f.row(&original).receipt;
            receipt.compaction.step == Step::Recorded && !receipt.drained
        });
        f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone(), f.fences.clone()).unwrap();
        f.runtime.fixture_executor(
            MACHINE,
            Arc::new(|_, _, _, _, _| panic!("restart must not run a child")),
        );
        f.runtime.fixture_mac_fence(false);
        f.store = Store::open(&f.location, &f.epoch).unwrap();
        zerus_desktop_ledger::authorize(
            &mut f.store.db,
            &uuid::Uuid::new_v4().to_string(),
            &f.epoch,
        )
        .unwrap();
        // The slot completes after the restart: nothing resumes the wait.
        slot_record(
            &f.config.state,
            &original.plan,
            slot_of(&original.plan, "completed"),
            true,
        );
        f.until("retired", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(
            (row.receipt.send.step, row.receipt.code.as_str()),
            (Step::NotStarted, ledger::INTERRUPTED)
        );
        assert!(row.send.is_none());
        assert_eq!(ran.counts(), (1, 0));
    }

    /// A slot read that finds the compaction completed only after the
    /// operation's deadline reserves no send.
    #[test]
    fn a_slot_completed_after_the_deadline_never_sends() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "completed", true));
        f.runtime.fixture_wait(
            runtime::WAIT,
            Duration::from_millis(5),
            Duration::from_millis(800),
        );
        f.runtime
            .fixture_slot_hook(Arc::new(|| std::thread::sleep(Duration::from_millis(1000))));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(
            (row.receipt.send.step, row.receipt.code.as_str()),
            (Step::NotStarted, ledger::CHAIN_EXPIRED)
        );
        assert!(row.send.is_none(), "never reserved");
        assert_eq!(ran.counts(), (1, 0));
    }

    /// The deadline is checked last in the send's own spawn turn: one that
    /// passed during the slow checks before it starts nothing.
    #[test]
    fn a_deadline_spent_before_the_sends_spawn_starts_nothing() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "completed", true));
        f.runtime.fixture_wait(
            runtime::WAIT,
            Duration::from_millis(5),
            Duration::from_millis(800),
        );
        let turns = Arc::new(AtomicUsize::new(0));
        let count = turns.clone();
        f.runtime.fixture_spawn_hook(Arc::new(move || {
            // The second spawn turn is the send's.
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                std::thread::sleep(Duration::from_millis(1000));
            }
        }));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert!(row.send.is_some(), "reserved before its spawn turn");
        assert_eq!(
            (row.receipt.send.step, row.receipt.send.code.as_str()),
            (Step::NotStarted, wire::NOT_STARTED)
        );
        assert_eq!(ran.counts(), (1, 0), "no send child");
    }

    /// zerus-qw7wp: one work slot, so an operation queued behind another's
    /// wait keeps its clock unstarted until its own turn. Observe that
    /// ownership directly: scaling the whole chain to milliseconds would
    /// instead benchmark unrelated disk and thread scheduling on the host.
    #[test]
    fn a_second_operation_queued_behind_a_long_wait_still_runs() {
        let first_id = Arc::new(std::sync::Mutex::new(String::new()));
        let (ran_first, ran_second) = (Arc::new(Ran::default()), Arc::new(Ran::default()));
        // The first's compaction never completes; the second's does.
        let pending = executor(ran_first.clone(), true, "submitted", true);
        let completes = executor(ran_second.clone(), true, "completed", true);
        let which = first_id.clone();
        let mut f = Fixture::new(move |state, plan, args, input, lock, resources| {
            if plan.compaction_request_id == *which.lock().unwrap() {
                return pending(state, plan, args, input, lock, resources);
            }
            completes(state, plan, args, input, lock, resources)
        });
        // Keep the real operation bound. Only the first's deliberately
        // unfinished slot wait is short; the child/receipt path is real I/O.
        f.runtime.fixture_wait(
            Duration::from_millis(300),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        // The queue runs by admission millisecond, then request ID: with the
        // first's ID lower, a tie between the two admissions still runs the
        // first first (zerus-mupq4).
        let (first, second) = match (f.original(), f.original()) {
            (a, b) if a.plan.compaction_request_id < b.plan.compaction_request_id => (a, b),
            (a, b) => (b, a),
        };
        *first_id.lock().unwrap() = first.plan.compaction_request_id.clone();
        assert!(f.admit(&first).accepted);
        assert!(f.admit(&second).accepted);
        assert_eq!(
            f.runtime
                .fixture_chain_started(&first.plan.compaction_request_id),
            Some(None),
            "admission must not start the first operation's clock"
        );
        assert_eq!(
            f.runtime
                .fixture_chain_started(&second.plan.compaction_request_id),
            Some(None),
            "admission must not start the queued operation's clock"
        );
        f.until("first holds the waiting work slot", |f| {
            f.row(&first).receipt.compaction.step == Step::Recorded
                && !f.row(&first).receipt.drained
        });
        assert!(
            f.runtime
                .fixture_chain_started(&first.plan.compaction_request_id)
                .flatten()
                .is_some(),
            "the first turn must start the first operation's clock"
        );
        assert_eq!(
            f.runtime
                .fixture_chain_started(&second.plan.compaction_request_id),
            Some(None),
            "another operation's wait must not start the queued clock"
        );
        assert_eq!(f.row(&second).receipt.compaction.step, Step::Queued);
        assert_eq!(ran_second.counts(), (0, 0));
        f.until("both settled", |f| {
            f.row(&first).receipt.drained && f.row(&second).receipt.drained
        });
        // The first's wait expired: compacted, never sent.
        let row = f.row(&first);
        assert_eq!(
            row.receipt.result(&first.plan),
            Some(Settled::NotSent { compacted: true })
        );
        assert!(row.send.is_none());
        assert_eq!(ran_first.counts(), (1, 0));
        // After the other operation's wait, this same queued original gets
        // its own turn and runs both children exactly once.
        let row = f.row(&second);
        assert_eq!(row.receipt.compaction.step, Step::Recorded);
        assert_eq!(row.receipt.send.step, Step::Recorded);
        assert_eq!(
            row.receipt.result(&second.plan),
            Some(Settled::Sent(row.receipt.send.outcome.clone().unwrap()))
        );
        assert_eq!(ran_second.counts(), (1, 1), "each child once");
    }

    /// An operation past its compaction that no live work holds lost its
    /// wait: it is retired, never resumed.
    #[test]
    fn a_lost_wait_is_retired_and_never_resumed() {
        use super::super::receipt::Observed;
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("no child"));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        // The compaction settled outside any work of this runtime.
        let row = f.row(&original);
        let ticket = zerus_desktop_ledger::operations::Ticket {
            operation_id: original.binding.operation_id().unwrap(),
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: original.plan.compaction_request_id.clone(),
            binding_sha256: original.binding.fingerprint().unwrap(),
            writer_epoch: row.writer_epoch.clone(),
            service_epoch: f.epoch.clone(),
            lease: uuid::Uuid::new_v4().to_string(),
        };
        let (prepared, held) = crate::desktop::custody::prepare(&f.location, &ticket).unwrap();
        let record = zerus_desktop_ledger::operations::custody::Record {
            prepared,
            helper: None,
        };
        let tx = f
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::reserve(&tx, &row, Child::Compaction, &ticket, &record).unwrap();
        tx.commit().unwrap();
        drop(held);
        let row = f.row(&original);
        let observation = super::released(&f.location, &record);
        let tx = f
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::drained(
            tx,
            row,
            observation,
            &Observed::Compacted(wire::Compacted {
                request_id: original.plan.compaction_request_id.clone(),
                status: wire::SUBMITTED.into(),
            }),
            Default::default(),
        )
        .unwrap();
        slot_record(
            &f.config.state,
            &original.plan,
            slot_of(&original.plan, "completed"),
            true,
        );
        f.until("retired", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(
            (row.receipt.send.step, row.receipt.code.as_str()),
            (Step::NotStarted, ledger::INTERRUPTED)
        );
        assert!(row.send.is_none());
    }

    #[test]
    fn an_exit_alone_never_settles() {
        let mut f = Fixture::new(|_, _, _, _, _, _| Ended::Exited { code: 0 });
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.compaction.step, receipt.compaction.code.as_str()),
            (Step::Unknown, super::super::receipt::MISSING)
        );
        // No slot names it: the wait ends, and no send ever starts.
        assert_eq!(
            (receipt.send.step, receipt.code.as_str()),
            (Step::NotStarted, super::super::slot::SLOT_ENDED)
        );
    }

    /// The selected session's actual record is checked in the spawn turn: a
    /// session that changed starts no child, and with no record of the
    /// request the compaction never started and neither does the send.
    #[test]
    fn a_changed_session_starts_nothing_and_the_operation_never_started() {
        let ran = Arc::new(AtomicUsize::new(0));
        let count = ran.clone();
        let mut f = Fixture::new(move |_, _, _, _, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ended::Exited { code: 0 }
        });
        session_record(&f.config.state, SESSION, "claude", "run-2", CONVERSATION);
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.compaction.step, receipt.send.step),
            (Step::NotStarted, Step::NotStarted)
        );
        assert_eq!(receipt.result(&original.plan), Some(Settled::NotStarted));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_revoked_writer_or_stop_after_admission_starts_nothing_and_retires_the_operation() {
        let ran = Arc::new(AtomicUsize::new(0));
        let count = ran.clone();
        let mut f = Fixture::new(move |_, _, _, _, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ended::Exited { code: 0 }
        });
        let original = f.original();
        // Admit without polling the work, then revoke before the child.
        let admission = f.admit(&original);
        assert!(admission.accepted);
        f.store
            .db
            .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
            .unwrap();
        f.until("retired", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.compaction.step, Step::NotStarted);
        assert_eq!(receipt.send.step, Step::NotStarted);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn restart_retires_a_queued_operation_and_never_reconstructs_consent() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("recovered operation must not run"));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        // A restart before any child: a new runtime and Store, no consent.
        f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone(), f.fences.clone()).unwrap();
        f.runtime.fixture_executor(
            MACHINE,
            Arc::new(|_, _, _, _, _| panic!("restart must not run a child")),
        );
        f.runtime.fixture_mac_fence(false);
        f.store = Store::open(&f.location, &f.epoch).unwrap();
        zerus_desktop_ledger::authorize(
            &mut f.store.db,
            &uuid::Uuid::new_v4().to_string(),
            &f.epoch,
        )
        .unwrap();
        f.until("retired", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (
                receipt.compaction.step,
                receipt.send.step,
                receipt.code.as_str()
            ),
            (Step::NotStarted, Step::NotStarted, ledger::INTERRUPTED)
        );
        // The same operation again gets only that receipt.
        let again = f.admit(&original);
        assert!(again.accepted);
        assert_eq!(again.receipt.unwrap(), receipt);
    }

    #[test]
    fn another_scope_or_an_invalid_original_admits_nothing() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("no child"));
        let other = super::original(&"b".repeat(64), &f.epoch);
        let admission = f.admit(&other);
        assert!(!admission.accepted && !admission.durable);
        assert_eq!(admission.code, "compact_to_send_unavailable");
        let mut invalid = f.original();
        invalid.intent.compaction = "{}".into();
        assert!(!f.admit(&invalid).accepted);
        let rows: i64 = f
            .store
            .db
            .query_row("SELECT count(*) FROM desktop_compact_to_sends", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn the_macos_fence_refuses_before_any_row_or_child() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("no child"));
        f.runtime.fixture_mac_fence(true);
        let original = f.original();
        let admission = f.admit(&original);
        assert!(!admission.accepted && !admission.durable);
        assert_eq!(admission.code, runtime::MAC_UNAVAILABLE);
        assert!(
            ledger::load(&f.store.db, &original.plan.compaction_request_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_receipt_read_past_its_deadline_is_unknown_however_exact() {
        let mut f = Fixture::new(|state, plan, _, _, _, _| {
            compacted(state, plan);
            Ended::Exited { code: 0 }
        });
        f.runtime.fixture_read(
            Some(Arc::new(|| std::thread::sleep(Duration::from_millis(60)))),
            Duration::from_millis(10),
        );
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(
            f.row(&original).receipt.compaction.code,
            super::super::receipt::TIMED_OUT
        );
    }

    #[test]
    fn store_recovery_leaves_the_runtimes_own_original_to_it() {
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = std::sync::Mutex::new(released);
        let mut f = Fixture::new(move |state, plan, _, _, _, _| {
            entered_tx.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            compacted(state, plan);
            Ended::Exited { code: 0 }
        });
        let original = f.original();
        f.admit(&original);
        f.until("running", |f| {
            f.row(&original).receipt.compaction.step == Step::Running
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        // Store recovery runs while the child is held, and again after the
        // child ended and released its custody but before the runtime's own
        // drain: it never settles the runtime's own original.
        for _ in 0..5 {
            f.store.recover_operation_custody(&f.location);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!f.row(&original).receipt.drained);
        release.send(()).unwrap();
        f.until("ended", |f| {
            f.row(&original).receipt.code == ledger::AWAITING_DRAIN
        });
        // Past the recovery's own 250 ms probe interval.
        for _ in 0..120 {
            f.store.recover_operation_custody(&f.location);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!f.row(&original).receipt.drained, "recovery settled it");
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.compaction.step, Step::Recorded);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_image_runs_both_original_children_and_their_receipts_settle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hgs");
        let seen = dir.path().join("seen");
        let mut f = Fixture::unexecuted();
        let state = f.config.state.clone();
        for folder in ["compact_receipts", "input_receipts"] {
            std::fs::create_dir(state.join(folder)).unwrap();
            mode(&state.join(folder), 0o700);
        }
        std::fs::write(
            &path,
            format!(
                r#"#!/usr/bin/env python3
import hashlib,json,os,sys
data=sys.stdin.buffer.read()
request=json.loads(data)
state={state:?}
with open({seen:?},"a") as log:
    log.write(json.dumps({{"argv":sys.argv[1:],"stdin":data.decode(),"custody":os.environ.get("HGS_COMPACT_TO_SEND_CUSTODY_FD")}})+"\n")
def write(path,value):
    fd=os.open(path+".tmp",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    os.write(fd,json.dumps(value).encode())
    os.close(fd)
    os.rename(path+".tmp",path)
name=sys.argv[2]
if sys.argv[1]=="compact-context":
    write(os.path.join(state,"compact_receipts",request["request_id"]+".json"),{{"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"status":"submitted","at":1.0}})
    slot={{"request_id":request["request_id"],"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"status":"completed"}}
    write(os.path.join(state,hashlib.sha256(name.encode()).hexdigest()+".json"),{{"name":name,"agent":"claude","run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"activity":"idle","phase":"idle","compact_context_request":slot}})
else:
    write(os.path.join(state,"input_receipts",request["request_id"]+".json"),{{"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"digest":hashlib.sha256(data).hexdigest(),"status":"submitted","transport":"terminal"}})
"#,
                seen = seen.to_str().unwrap(),
                state = state.to_str().unwrap(),
            ),
        )
        .unwrap();
        mode(&path, 0o700);
        f.runtime.fixture_image(&path);
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.compaction.step, Step::Recorded);
        assert_eq!(receipt.send.step, Step::Recorded);
        let seen: Vec<Value> = std::fs::read_to_string(&seen)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0]["argv"],
            json!(["compact-context", SESSION, "--json"])
        );
        assert_eq!(seen[0]["stdin"], original.intent.compaction.as_str());
        assert_eq!(seen[1]["argv"], json!(["send", SESSION, "--json"]));
        assert_eq!(seen[1]["stdin"], original.intent.original.as_str());
        for child in &seen {
            assert!(child["custody"]
                .as_str()
                .is_some_and(|fd| fd.parse::<u32>().is_ok()));
        }
    }

    /// A pinned image whose compaction writes its exact receipt and completes
    /// the slot, and whose send only leaves the `sent` marker.
    #[cfg(target_os = "linux")]
    fn marker_image(path: &Path, state: &Path, sent: &Path) {
        std::fs::write(
            path,
            format!(
                r#"#!/usr/bin/env python3
import hashlib,json,os,sys
data=sys.stdin.buffer.read()
request=json.loads(data)
state={state:?}
def write(path,value):
    fd=os.open(path+".tmp",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    os.write(fd,json.dumps(value).encode())
    os.close(fd)
    os.rename(path+".tmp",path)
name=sys.argv[2]
if sys.argv[1]=="compact-context":
    write(os.path.join(state,"compact_receipts",request["request_id"]+".json"),{{"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"status":"submitted","at":1.0}})
    slot={{"request_id":request["request_id"],"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"status":"completed"}}
    write(os.path.join(state,hashlib.sha256(name.encode()).hexdigest()+".json"),{{"name":name,"agent":"claude","run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"activity":"idle","phase":"idle","compact_context_request":slot}})
else:
    open({sent:?},"w").close()
"#,
                state = state.to_str().unwrap(),
                sent = sent.to_str().unwrap(),
            ),
        )
        .unwrap();
        mode(path, 0o700);
    }

    /// The pinned image's final spawn turn checks the deadline last: a send
    /// whose deadline passed during the checks before it never runs.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_send_checks_its_deadline_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hgs");
        let sent = dir.path().join("sent");
        let mut f = Fixture::unexecuted();
        let state = f.config.state.clone();
        std::fs::create_dir(state.join("compact_receipts")).unwrap();
        mode(&state.join("compact_receipts"), 0o700);
        marker_image(&path, &state, &sent);
        f.runtime.fixture_image(&path);
        f.runtime.fixture_wait(
            runtime::WAIT,
            Duration::from_millis(5),
            Duration::from_millis(1500),
        );
        let turns = Arc::new(AtomicUsize::new(0));
        let count = turns.clone();
        f.runtime.fixture_spawn_hook(Arc::new(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                std::thread::sleep(Duration::from_millis(1700));
            }
        }));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.compaction.step, Step::Recorded);
        assert_eq!(receipt.send.step, Step::NotStarted);
        assert!(!sent.exists(), "the send never ran");
    }

    /// desktop-u G1 (PR 1455), for the send: its final spawn turn re-proves
    /// the selected session's record and the completed slot from the
    /// retained roots, after every earlier read. A run or a slot that
    /// changed only there starts no send, which then never started.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_send_reproves_its_target_and_slot_in_the_final_turn() {
        for change in ["target", "slot"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let sent = dir.path().join("sent");
            let mut f = Fixture::unexecuted();
            let state = f.config.state.clone();
            for folder in ["compact_receipts", "input_receipts"] {
                std::fs::create_dir(state.join(folder)).unwrap();
                mode(&state.join(folder), 0o700);
            }
            marker_image(&path, &state, &sent);
            f.runtime.fixture_image(&path);
            let original = f.original();
            let plan = original.plan.clone();
            let turns = Arc::new(AtomicUsize::new(0));
            let count = turns.clone();
            let hook_state = state.clone();
            f.runtime.fixture_spawn_hook(Arc::new(move || {
                // The second spawn turn is the send's.
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    match change {
                        "target" => {
                            session_record(&hook_state, SESSION, "claude", "run-2", CONVERSATION)
                        }
                        _ => slot_record(&hook_state, &plan, slot_of(&plan, "cancelled"), true),
                    }
                }
            }));
            assert!(f.admit(&original).accepted);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let row = f.row(&original);
            assert!(row.send.is_some(), "{change}: reserved before its turn");
            assert_eq!(turns.load(Ordering::SeqCst), 2, "{change}");
            assert_eq!(row.receipt.compaction.step, Step::Recorded, "{change}");
            assert_eq!(
                (row.receipt.send.step, row.receipt.send.code.as_str()),
                (Step::NotStarted, wire::NOT_STARTED),
                "{change}"
            );
            assert_eq!(
                row.receipt.result(&original.plan),
                Some(Settled::NotSent { compacted: true }),
                "{change}"
            );
            assert!(!sent.exists(), "{change}: the send never ran");
        }
    }

    /// A stop, a revoke, a changed source or a changed target record after
    /// the reservation committed starts nothing: the target and consent
    /// vetoes run last in the spawn turn, and with no record of the request
    /// both children never started.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stop_revoke_source_or_target_change_after_reservation_starts_nothing() {
        for action in ["stop", "revoke", "source", "target"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let ran = dir.path().join("ran");
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\ntouch {:?}\ncat >/dev/null\n",
                    ran.to_str().unwrap()
                ),
            )
            .unwrap();
            mode(&path, 0o700);
            let mut f = Fixture::unexecuted();
            f.runtime.fixture_image(&path);
            let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let entered_tx = std::sync::Mutex::new(entered_tx);
            let released = std::sync::Mutex::new(released);
            f.runtime.fixture_spawn_hook(Arc::new(move || {
                let _ = entered_tx.lock().unwrap().send(());
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            }));
            let original = f.original();
            assert!(f.admit(&original).accepted);
            let until = Instant::now() + Duration::from_secs(8);
            while entered.try_recv().is_err() {
                assert!(
                    Instant::now() < until,
                    "{action}: never reached the spawn turn"
                );
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                std::thread::sleep(Duration::from_millis(2));
            }
            // The reservation committed before the worker existed.
            assert_eq!(
                f.row(&original).receipt.compaction.step,
                Step::Running,
                "{action}"
            );
            match action {
                "stop" => f.runtime.request_stop(),
                "revoke" => f.revoke(),
                "source" => f
                    .runtime
                    .fixture_machine("33333333-3333-4333-8333-333333333333"),
                // desktop-u G1: only the selected session's run changes, in
                // the final turn after every earlier read.
                _ => session_record(&f.config.state, SESSION, "claude", "run-2", CONVERSATION),
            }
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            release.send(()).unwrap();
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.compaction.step, receipt.send.step),
                (Step::NotStarted, Step::NotStarted),
                "{action}"
            );
            assert!(!ran.exists(), "{action}: no child ever existed");
        }
    }

    /// `original`'s request with content that no longer verifies: refused
    /// in the writer transaction after its empty read.
    fn broken(original: &Verified) -> Intent {
        let mut bad = original.intent.clone();
        bad.original = "{}".into();
        bad
    }
    fn offer(f: &mut Fixture, intent: Intent) -> wire::Admission {
        let request = json!(
            zerus_client_core::desktop::ui_requests::Request::CompactToSend {
                intent: Box::new(intent),
            }
        );
        serde_json::from_value(f.runtime.frontend(
            &mut f.store,
            ui_authority::Validated::fixture(&request),
            Some(f.exclusion.clone()),
        ))
        .unwrap()
    }
    fn no_row(f: &Fixture, request_id: &str) -> bool {
        ledger::load(&f.store.db, request_id).unwrap().is_none()
    }
    /// Polls a while: nothing sealed may start in that time.
    fn idle(f: &mut Fixture) {
        for _ in 0..20 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    /// A fixture whose compaction and send complete and count themselves.
    fn counting() -> (Arc<Ran>, Fixture) {
        let ran = Arc::new(Ran::default());
        let f = Fixture::with(executor(ran.clone(), true, "completed", true));
        (ran, f)
    }

    /// zerus-tlb9q, desktop-u's pin: a request refused before its admission
    /// with no row is answered not admitted and sealed for the epoch; an
    /// otherwise admissible copy of the same request that arrives after it
    /// gets the same answer and never runs. The lookup never answers it.
    #[test]
    fn a_request_refused_before_admission_is_sealed_and_its_copy_never_runs() {
        let (ran, mut f) = counting();
        let original = f.original();
        let id = original.plan.compaction_request_id.clone();
        let refused = offer(&mut f, broken(&original));
        assert_eq!(
            (refused.accepted, refused.durable, refused.code.as_str()),
            (false, false, wire::NOT_ADMITTED)
        );
        assert!(refused.not_admitted(&original.intent), "{}", refused.code);
        assert!(no_row(&f, &id) && f.fences.compact_to_send.sealed(&id));
        let copy = f.admit(&original);
        assert!(copy.not_admitted(&original.intent), "{}", copy.code);
        idle(&mut f);
        assert_eq!(ran.counts(), (0, 0), "the copy never ran");
        assert!(no_row(&f, &id));
        let query = wire::Query::for_plan(original.intent.scope.clone(), &original.plan).unwrap();
        let lookup = super::super::query(&f.store, &json!({"op": "compact_to_send_result", "query": query}));
        assert_eq!(
            (lookup["found"].as_bool(), lookup.get("code")),
            (Some(false), None)
        );
    }

    /// zerus-5llxc: another connection's request is never answered not
    /// admitted nor sealed, even when its permit expires after the empty
    /// read or when this epoch sealed its ID.
    #[test]
    fn another_connections_request_is_never_not_admitted() {
        let (_, mut f) = counting();
        let foreign = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let request = json!(
            zerus_client_core::desktop::ui_requests::Request::CompactToSend {
                intent: Box::new(foreign.intent.clone()),
            }
        );
        let expired = ui_authority::Validated::fixture_expiring(
            &request,
            Instant::now() - Duration::from_secs(1),
        );
        let reply = f
            .runtime
            .frontend(&mut f.store, expired, Some(f.exclusion.clone()));
        assert_eq!(reply["code"], "compact_to_send_unavailable", "{reply}");
        assert!(!f.fences.compact_to_send.sealed(&foreign.plan.compaction_request_id));
        // This epoch's seal is no answer for another connection's copy.
        let original = f.original();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        let mut intent = original.intent.clone();
        intent.scope.service_epoch = uuid::Uuid::new_v4().to_string();
        let copy = intent.verify().unwrap();
        let reply = f.admit(&copy);
        assert_eq!(reply.code, "compact_to_send_unavailable");
        assert!(!reply.not_admitted(&copy.intent));
        assert!(no_row(&f, &original.plan.compaction_request_id));
    }

    /// The seal belongs to the epoch: the background runtime destroyed and
    /// built again under the same epoch still refuses the sealed request,
    /// and a request of another epoch's connection is never sealed.
    #[test]
    fn a_runtime_rebuilt_in_the_same_epoch_keeps_the_seal() {
        let (ran, mut f) = counting();
        let original = f.original();
        let id = original.plan.compaction_request_id.clone();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        f.rebuild(executor(ran.clone(), true, "completed", true));
        let copy = f.admit(&original);
        assert!(copy.not_admitted(&original.intent), "{}", copy.code);
        idle(&mut f);
        assert_eq!(ran.counts(), (0, 0));
        assert!(no_row(&f, &id));
        // The rebuilt runtime still admits a new request.
        let other = f.original();
        assert!(f.admit(&other).accepted);
        f.until("settled", |f| f.row(&other).receipt.drained);
        assert_eq!(ran.counts(), (1, 1));
        // A request of another epoch's connection: unavailable, not sealed.
        let stale = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let reply = f.admit(&stale);
        assert_eq!(
            (reply.accepted, reply.code.as_str()),
            (false, "compact_to_send_unavailable")
        );
        assert!(!reply.not_admitted(&stale.intent));
        assert!(!f.fences.compact_to_send.sealed(&stale.plan.compaction_request_id));
        assert!(no_row(&f, &stale.plan.compaction_request_id));
    }

    /// Before the writer transaction the refusal is proven by a fresh empty
    /// read; a request whose row exists, a failure from the admission on and
    /// a full fence all stay unavailable and unsealed.
    #[test]
    fn only_a_proven_absence_is_not_admitted() {
        // Refused before the transaction (the owner stops): proven absent.
        let (_, mut f) = counting();
        let original = f.original();
        f.runtime.request_stop();
        let stopped = f.admit(&original);
        assert!(stopped.not_admitted(&original.intent), "{}", stopped.code);
        assert!(no_row(&f, &original.plan.compaction_request_id));
        // Another connection's request refused there is never sealed.
        let stale = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let reply = f.admit(&stale);
        assert_eq!(reply.code, "compact_to_send_unavailable");
        assert!(!f.fences.compact_to_send.sealed(&stale.plan.compaction_request_id));

        // Refused before the transaction while another writer holds the
        // store: nothing is proven.
        let (_, mut f) = counting();
        let original = f.original();
        f.runtime.request_stop();
        let mut other = rusqlite::Connection::open(f.location.root.join("state.sqlite3")).unwrap();
        let held = other
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        f.store.db.busy_timeout(Duration::from_millis(1)).unwrap();
        let busy = f.admit(&original);
        drop(held);
        assert_eq!(
            (busy.accepted, busy.code.as_str()),
            (false, "compact_to_send_unavailable")
        );
        assert!(!f.fences.compact_to_send.sealed(&original.plan.compaction_request_id));

        // A row exists: the same refusal stays unavailable.
        let (_, mut f) = counting();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let existing = offer(&mut f, broken(&original));
        assert_eq!(
            (existing.accepted, existing.code.as_str()),
            (false, "compact_to_send_unavailable")
        );
        f.runtime.request_stop();
        let stopped = f.admit(&original);
        assert_eq!(stopped.code, "compact_to_send_unavailable");
        assert!(!f.fences.compact_to_send.sealed(&original.plan.compaction_request_id));

        // The admission itself fails: unavailable although no row is left,
        // and nothing is sealed.
        let (ran, mut f) = counting();
        let original = f.original();
        f.store
            .db
            .execute_batch(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON desktop_compact_to_sends \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let failed = f.admit(&original);
        assert_eq!(
            (failed.accepted, failed.code.as_str()),
            (false, "compact_to_send_unavailable")
        );
        assert!(no_row(&f, &original.plan.compaction_request_id));
        assert!(!f.fences.compact_to_send.sealed(&original.plan.compaction_request_id));
        f.store.db.execute_batch("DROP TRIGGER refuse;").unwrap();
        assert!(f.admit(&original).accepted, "an unavailable refusal seals nothing");
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(ran.counts(), (1, 1));

        // A full fence mints no new negative; its sealed IDs stay sealed.
        let (_, mut f) = counting();
        for n in 0..crate::desktop::not_admitted::BOUND {
            assert!(f.fences.compact_to_send.seal(&format!("sealed-{n}")));
        }
        let original = f.original();
        let full = offer(&mut f, broken(&original));
        assert_eq!(
            (full.accepted, full.code.as_str()),
            (false, "compact_to_send_unavailable")
        );
        assert!(!f.fences.compact_to_send.sealed(&original.plan.compaction_request_id));
        assert!(f.fences.compact_to_send.sealed("sealed-0"));
    }

    #[test]
    fn budget_corruption_or_ambiguous_commit_is_unavailable_not_not_admitted() {
        for failure in ["counter", "row", "commit"] {
            let (sent, mut f) = counting();
            let original = f.original();
            if failure == "counter" {
                f.store
                    .db
                    .execute("UPDATE desktop_compact_to_send_budget SET bytes=1", [])
                    .unwrap();
            } else if failure == "row" {
                let victim = f.original();
                assert!(f.admit(&victim).accepted);
                let receipt = serde_json::to_string(&f.row(&victim).receipt).unwrap();
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_compact_to_sends SET receipt=?1 WHERE request_id=?2",
                        rusqlite::params![
                            format!("{receipt}\0suffix"),
                            victim.plan.compaction_request_id
                        ],
                    )
                    .unwrap();
            } else {
                f.store.db.execute_batch("PRAGMA foreign_keys=ON;
                            CREATE TABLE send_parent(id INTEGER PRIMARY KEY);
                            CREATE TABLE send_child(id INTEGER REFERENCES send_parent(id) DEFERRABLE INITIALLY DEFERRED);
                            CREATE TEMP TRIGGER fail_send_commit AFTER INSERT ON desktop_compact_to_sends BEGIN INSERT INTO send_child VALUES(1); END;").unwrap();
            }
            let result = f.admit(&original);
            assert_eq!(result.code, "compact_to_send_unavailable", "{failure}");
            assert!(!result.not_admitted(&original.intent));
            assert!(!f
                .fences
                .compact_to_send
                .sealed(&original.plan.compaction_request_id));
            assert!(no_row(&f, &original.plan.compaction_request_id));
            assert_eq!(sent.counts(), (0, 0));
        }
    }

    #[test]
    fn budget_capacity_is_proven_not_admitted_before_any_child_or_write() {
        let (sent, mut f) = counting();
        for _ in 0..32 {
            let v = f.original();
            assert!(f.admit(&v).accepted);
        }
        let protected = super::reservation_budget::retained(&f.store.db);
        let refused = f.original();
        let reply = f.admit(&refused);
        assert!(
            reply.not_admitted(&refused.intent),
            "capacity must be a proven pre-admission refusal: {}",
            reply.code
        );
        assert!(no_row(&f, &refused.plan.compaction_request_id));
        assert_eq!(super::reservation_budget::retained(&f.store.db), protected);
        assert_eq!(sent.counts(), (0, 0));
        assert_eq!(super::reservation_budget::budget(&f.store.db).2, 32);
        f.runtime.request_stop();
        // Each poll retires one queued original however long this host takes
        // for it, so the drain is bounded in polls, not in seconds: in a
        // loaded suite the fixture's eight seconds ran out with 6 to 18 of
        // the 32 still queued (zerus-dha0w).
        for _ in 0..32 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        assert!(
            f.runtime.drained(),
            "queued originals retire without input, one per poll: {}",
            f.runtime.status()
        );
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
        assert!(f.admit(&refused).not_admitted(&refused.intent));
        assert_eq!(sent.counts(), (0, 0));
    }

    #[test]
    fn budget_recorded_original_returns_its_charge_and_duplicate_executes_nothing() {
        let (ran, mut f) = counting();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        assert_eq!(super::reservation_budget::budget(&f.store.db).2, 1);
        f.until("original physically drains", |f| {
            f.row(&original).receipt.drained
        });
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
        let receipt = f.row(&original).receipt;
        assert_eq!(f.admit(&original).receipt, Some(receipt));
        assert_eq!(ran.counts(), (1, 1));
        f.runtime.request_stop();
        f.until("all original resources settle", |f| f.runtime.drained());
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
    }

    #[test]
    fn budget_keeps_the_full_original_charge_between_children_and_wait_retirement() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), true, "submitted", true));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.original();
        assert!(f.admit(&original).accepted);
        let charged = super::reservation_budget::budget(&f.store.db);
        f.until("first child physically drained", |f| {
            f.row(&original).receipt.compaction.step == Step::Recorded
        });
        assert!(!f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.send.step, Step::Queued);
        assert_eq!(super::reservation_budget::budget(&f.store.db), charged);
        assert_eq!(ran.counts(), (1, 0));
        f.runtime.request_stop();
        f.until("unused send retired without a child", |f| {
            f.runtime.drained()
        });
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
        assert_eq!(ran.counts(), (1, 0));
    }
}
/// Reservation accounting controls use only synthetic originals and private Store fixtures.
/// They are ledger/Store tests, not native-agent qualification.
mod reservation_budget {
    use super::*;
    pub(super) fn budget(db: &rusqlite::Connection) -> (i64, i64, i64) {
        db.query_row(
            "SELECT rows,bytes,active FROM desktop_compact_to_send_budget WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    fn key(v: &Verified) -> &str {
        &v.plan.compaction_request_id
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
        db.query_row("SELECT length(CAST(compaction AS BLOB))+length(CAST(source AS BLOB))+length(CAST(checkpoint AS BLOB))+length(CAST(plan AS BLOB))+50176 FROM desktop_compact_to_sends WHERE request_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_compact_to_sends)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_compact_to_sends ORDER BY request_id",
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
            let charge = v.intent.original.len()
                + v.intent.compaction.len()
                + v.intent.checkpoint.len()
                + serde_json::to_string(&v.plan).unwrap().len()
                + 50176;
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
                "UPDATE desktop_compact_to_send_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
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
        let leaf = &status["kinds"]["compact_to_send"];
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
                "counter"=>{store.db.execute("UPDATE desktop_compact_to_send_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_compact_to_sends SET receipt='{}' WHERE request_id=(SELECT request_id FROM desktop_compact_to_sends LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_compact_to_sends SET drained=2 WHERE request_id=(SELECT request_id FROM desktop_compact_to_sends LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_compact_to_send_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_compact_to_send_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
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
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_compact_to_send_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_compact_to_send_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
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
                "UPDATE desktop_compact_to_send_budget SET rows=1,bytes=1,active=1 WHERE id=1",
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
        let (record, held) = super::running(&mut store, &location, &v, &epoch);
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
        ledger::unheld(tx, row, observed).unwrap();
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
            .prepare("SELECT * FROM desktop_compact_to_sends")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_compact_to_sends", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_compact_to_sends VALUES({})",
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

    /// Corrupt stored bytes are copied into a fresh private database without
    /// bypassing/changing any production trigger or touching a live original.
    #[test]
    fn bounded_raw_columns_reject_nul_suffixes_types_and_unicode_overflow() {
        for (column, cap) in [
            ("compaction", 8192_usize),
            ("source", 65536_usize),
            ("checkpoint", 16384_usize),
            ("plan", 16384_usize),
            ("receipt", 24576_usize),
            ("fingerprint", 64_usize),
            ("writer_epoch", 36_usize),
            ("session", 512_usize),
        ] {
            for mode in ["nul", "oversize", "unicode", "blob"] {
                let (_temp, location, mut store, epoch) = fixture();
                let v = original(&location.configuration, &epoch);
                let row = admitted(&mut store, &v, &epoch);
                retired(&mut store, &row);
                let charge = original_charge(&store.db, key(&v));
                let columns: Vec<String> = store
                    .db
                    .prepare("PRAGMA table_info(desktop_compact_to_sends)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_compact_to_sends", [], |r| {
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
                    "INSERT INTO desktop_compact_to_sends VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_compact_to_send_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
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
