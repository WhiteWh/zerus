use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use zerus_client_core::desktop::deferred_answer::{Intent, Phase, Verified};
use zerus_client_core::operation_recovery::deferred_input::{self as checkpoint, Checkpoint};

const SESSION: &str = "claude/private-answer";
const RUN: &str = "run-1";
const CONVERSATION: &str = "conversation-1";
const MACHINE: &str = "22222222-2222-4222-8222-222222222222";

const HASH: &str = "abababababababababababababababababababababababababababababababab";

/// The native question the person answered, as the session's inspection
/// carries it.
fn question() -> Value {
    json!({"question_id": "q-1", "question_hash": HASH, "run_id": RUN,
        "conversation_id": CONVERSATION, "source": "claude_folder_trust",
        "answer_transport": "claude_tui",
        "questions": [{"id": "q-1", "question": "Allow this folder?",
            "options": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}]}],
        "can_answer": true})
}
/// Its consent identity, the closed projection the client pins.
fn pinned() -> Value {
    zerus_client_core::questions::consent_identity(&question())
}
/// A saved answer checkpoint with its pinned consent and the exact stdin
/// the client builds for it.
fn original(configuration: &str, epoch: &str) -> Verified {
    original_of(&question(), "yes", configuration, epoch)
}
/// Codex's update question, as `hgs inspect` shows it.
fn update_question() -> Value {
    let mut asked = question();
    asked["question_id"] = json!(format!("codex-update:{HASH}"));
    asked["source"] = json!("codex_update");
    asked["answer_transport"] = json!("codex_tui");
    asked
}
/// A saved answer of `option` to `asked`, with its pinned consent and the
/// exact stdin the client builds for it.
fn original_of(asked: &Value, option: &str, configuration: &str, epoch: &str) -> Verified {
    let id = asked["question_id"].as_str().unwrap();
    let pinned = || zerus_client_core::questions::consent_identity(asked);
    let answers = json!([{"question_id": id, "selected_option_ids": [option], "text": ""}]);
    let checkpoint = Checkpoint::reserve(checkpoint::Intent {
        kind: checkpoint::Kind::Answer,
        draft_revision: 3,
        message_sha256: checkpoint::message_sha256(
            &answers.to_string(),
            Vec::<(&str, &str, &str, &[u8])>::new(),
        ),
        run_id: RUN.into(),
        conversation_id: CONVERSATION.into(),
        question: Some(checkpoint::Question {
            id: id.into(),
            identity_sha256: checkpoint::identity_sha256(&pinned()),
        }),
        start: None,
        retry_of: None,
        consent: false,
    });
    let stdin = json!({"request_id": checkpoint.request_id, "question_id": id,
        "expected_question_hash": HASH, "expected_run_id": RUN,
        "expected_conversation_id": CONVERSATION, "answers": answers});
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        session: SESSION.into(),
        request_id: checkpoint.request_id.clone(),
        checkpoint: serde_json::to_string(&checkpoint).unwrap(),
        pinned: pinned().to_string(),
        original: stdin.to_string(),
    }
    .verify()
    .unwrap()
}
/// The CLI's own record of `plan`'s request with `status`, as `hgs answer`
/// writes it.
fn cli_record(plan: &wire::Plan, status: &str) -> Value {
    json!({"request_id": plan.request_id, "name": plan.session, "run_id": plan.run_id,
        "conversation_id": plan.conversation_id, "question_id": plan.question_id,
        "question_hash": plan.question_hash, "digest": plan.input_sha256, "status": status,
        "submitted_at": 7.5, "answers": [], "transport": "claude_tui"})
}
fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
/// Writes `body` atomically as the CLI's private receipt of `request` under
/// the CLI state root `state`.
fn write_receipt(state: &Path, request: &str, body: &[u8]) -> PathBuf {
    let folder = state.join("question_receipts");
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
/// The CLI's own session record of `name` under the state root `state`, as
/// `hgs` keeps it (the legacy, unrouted path).
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
fn exact_previous_store_upgrade_adds_only_the_independent_answer_leaf() {
    assert_eq!(
        SCHEMA - 1,
        crate::desktop::deferred_send::SCHEMA,
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
    // Its own two tables, then the compact-to-send and session resume
    // leaves' two each.
    // ... and the question route leaves' tables (zerus-sa9qn.8, .32, .50 and .36).
    assert_eq!(tables(&store.db), before + 2 + 2 + 2 + 1 + 2 + 2 + 1);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    assert_eq!(
        store.operation_status()["kinds"]["deferred_answer"]["undrained"],
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
fn admission_is_immutable_and_the_same_id_returns_only_its_original_receipt() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    let row = admit(&mut store, &verified, &writer);
    assert_eq!(row.receipt.phase, Phase::Queued);
    assert_eq!(
        admit(&mut store, &verified, &writer).receipt,
        row.receipt,
        "the same original again"
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    assert_eq!(store.operation_undrained().unwrap(), 1);
    let status = store.operation_status();
    assert_eq!(status["kinds"]["deferred_answer"]["undrained"], 1);
    assert_eq!(status["kinds"]["deferred_answer"]["authority"], false);
    for statement in [
        "UPDATE desktop_deferred_answers SET source='{}'",
        "UPDATE desktop_deferred_answers SET session='claude/other'",
        "UPDATE desktop_deferred_answers SET checkpoint='{}'",
        "UPDATE desktop_deferred_answers SET pinned='{}'",
        "DELETE FROM desktop_deferred_answers",
    ] {
        assert!(store.db.execute(statement, []).is_err(), "{statement}");
    }
    // Another stdin under the same request ID never replaces it.
    let mut intent = verified.intent.clone();
    intent.original = format!(" {}", intent.original);
    let changed = intent.verify().unwrap();
    assert_ne!(changed.binding, verified.binding);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &changed, &writer, 3).is_err());
    drop(tx);
    let kept = ledger::load(&store.db, &verified.plan.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(kept.receipt.binding, row.receipt.binding);
    assert_eq!(kept.verified.intent.original, verified.intent.original);
}

#[test]
fn capacity_refusal_is_atomic_and_retirement_never_rearms_a_queued_original() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let writer = uuid::Uuid::new_v4().to_string();
    for full in [
        "UPDATE desktop_deferred_answer_budget SET bytes=16777216",
        "UPDATE desktop_deferred_answer_budget SET rows=4096",
        "UPDATE desktop_deferred_answer_budget SET active=32",
    ] {
        let saved: (i64, i64, i64) = store
            .db
            .query_row(
                "SELECT rows,bytes,active FROM desktop_deferred_answer_budget",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        store.db.execute(full, []).unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::admit(&tx, &verified, &writer, 1).is_err(), "{full}");
        drop(tx);
        assert!(ledger::load(&store.db, &verified.plan.request_id)
            .unwrap()
            .is_none());
        store
            .db
            .execute(
                "UPDATE desktop_deferred_answer_budget SET rows=?1,bytes=?2,active=?3",
                [saved.0, saved.1, saved.2],
            )
            .unwrap();
    }
    let row = admit(&mut store, &verified, &writer);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::retire(&tx, &row).unwrap();
    tx.commit().unwrap();
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    let retired = ledger::load(&store.db, &verified.plan.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (retired.receipt.phase, retired.receipt.code.as_str()),
        (Phase::Unknown, "deferred_answer_interrupted")
    );
    assert!(retired.receipt.drained);
    assert!(retired.receipt.result(&verified.plan).is_none());
    let replay = admit(&mut store, &verified, &writer);
    assert_eq!(replay.receipt, retired.receipt);
}

/// A reserved, running original with its exact custody, as the actor leaves
/// it before the child.
fn running(
    store: &mut Store,
    location: &Location,
    verified: &Verified,
    epoch: &str,
) -> (
    zerus_desktop_ledger::operations::custody::Record,
    std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
) {
    use zerus_desktop_ledger::operations::{custody::Record, Ticket};
    let writer = uuid::Uuid::new_v4().to_string();
    let row = admit(store, verified, &writer);
    let ticket = Ticket {
        operation_id: verified.binding.operation_id().unwrap(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        revision: 2,
        admission_id: verified.plan.request_id.clone(),
        binding_sha256: verified.binding.fingerprint().unwrap(),
        writer_epoch: writer,
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
    assert_eq!(
        ledger::load(&store.db, &verified.plan.request_id)
            .unwrap()
            .unwrap()
            .receipt
            .phase,
        Phase::Running
    );
    (record, held)
}

/// Observes `record` until its release, within a bound: a process another
/// test forks keeps a copy of every descriptor until its exec closes it.
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
fn only_the_released_custody_settles_an_answer_and_only_its_exact_cli_record_decides() {
    use super::receipt::{MISSING, UNAVAILABLE};
    use crate::desktop::custody::{self, Observation};
    use wire::Read as Cli;
    #[derive(Clone, Copy, Debug)]
    enum Ending {
        Exited,
        NotStarted,
        Unknown,
    }
    let recorded = |status: &'static str| {
        move |plan: &wire::Plan| {
            wire::Outcome::read(plan, cli_record(plan, status).to_string().as_bytes())
        }
    };
    let exact = recorded(wire::ANSWERED);
    let rejected = recorded("rejected");
    let other = |plan: &wire::Plan| {
        let Cli::Recorded(mut outcome) = wire::Outcome::read(
            plan,
            cli_record(plan, wire::SUBMITTED).to_string().as_bytes(),
        ) else {
            panic!("an exact record");
        };
        outcome.input_sha256 = "f".repeat(64);
        Cli::Recorded(outcome)
    };
    let unknown = |code: &'static str| move |_: &wire::Plan| Cli::Unknown(code);
    type Read = Box<dyn Fn(&wire::Plan) -> Cli>;
    let cases: Vec<(Ending, Read, Phase, &str)> = vec![
        // The exit proves nothing; only the CLI's own record decides.
        (Ending::Exited, Box::new(exact), Phase::Recorded, ""),
        (
            Ending::Exited,
            Box::new(recorded(wire::SUBMITTED)),
            Phase::Recorded,
            "",
        ),
        // The CLI's exact durable refusal before any delivery, whatever
        // the child's end.
        (
            Ending::Exited,
            Box::new(rejected),
            Phase::Failed,
            wire::REJECTED,
        ),
        (
            Ending::NotStarted,
            Box::new(rejected),
            Phase::Failed,
            wire::REJECTED,
        ),
        (
            Ending::Unknown,
            Box::new(rejected),
            Phase::Failed,
            wire::REJECTED,
        ),
        (
            Ending::Exited,
            Box::new(unknown(MISSING)),
            Phase::Unknown,
            MISSING,
        ),
        (
            Ending::Exited,
            Box::new(unknown(wire::RECEIPT_IN_PROGRESS)),
            Phase::Unknown,
            wire::RECEIPT_IN_PROGRESS,
        ),
        (
            Ending::Exited,
            Box::new(other),
            Phase::Unknown,
            wire::RECEIPT_MISMATCH,
        ),
        (
            Ending::Exited,
            Box::new(unknown(UNAVAILABLE)),
            Phase::Unknown,
            UNAVAILABLE,
        ),
        // The closed pre-child result fails only when no record exists.
        (
            Ending::NotStarted,
            Box::new(unknown(MISSING)),
            Phase::Failed,
            wire::NOT_STARTED,
        ),
        (Ending::NotStarted, Box::new(exact), Phase::Recorded, ""),
        (
            Ending::NotStarted,
            Box::new(unknown(wire::RECEIPT_IN_PROGRESS)),
            Phase::Unknown,
            wire::RECEIPT_IN_PROGRESS,
        ),
        (
            Ending::NotStarted,
            Box::new(unknown(UNAVAILABLE)),
            Phase::Unknown,
            UNAVAILABLE,
        ),
        (
            Ending::NotStarted,
            Box::new(other),
            Phase::Unknown,
            wire::RECEIPT_MISMATCH,
        ),
        // The child's own reason stays unless its exact record exists.
        (
            Ending::Unknown,
            Box::new(unknown(MISSING)),
            Phase::Unknown,
            "deferred_answer_timed_out",
        ),
        (Ending::Unknown, Box::new(exact), Phase::Recorded, ""),
    ];
    for (index, (ending, read, phase, code)) in cases.into_iter().enumerate() {
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
            Ending::Unknown => ledger::ended(&tx, &row, "deferred_answer_timed_out").unwrap(),
        }
        tx.commit().unwrap();
        // Nothing settles before the drain, whatever the receipt says.
        let row = ledger::pending(&store.db).unwrap().unwrap();
        assert_eq!(row.receipt.phase, Phase::Unknown, "{index}");
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
            Default::default()
        )
        .is_err());
        drop(held);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let observation = released(&location, &record);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let settled = ledger::drained(
            tx,
            row,
            observation,
            &read(&verified.plan),
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            (settled.phase, settled.code.as_str()),
            (phase, code),
            "{index}: {ending:?}"
        );
        assert!(settled.drained && settled.valid());
        match phase {
            Phase::Recorded => {
                let outcome = settled.outcome.clone().unwrap();
                assert_eq!(settled.result(&verified.plan), Some(Ok(outcome.clone())));
                assert_eq!(outcome.input_sha256, verified.plan.input_sha256);
                assert!(!settled.not_delivered(&verified.plan));
            }
            Phase::Failed => {
                assert!(settled.not_delivered(&verified.plan));
                assert_eq!(settled.result(&verified.plan), Some(Err(code.into())));
            }
            _ => {
                assert!(settled.result(&verified.plan).is_none());
                assert!(!settled.not_delivered(&verified.plan));
            }
        }
        // The settled receipt is the stored one, and it is final.
        assert_eq!(
            ledger::load(&store.db, &verified.plan.request_id)
                .unwrap()
                .unwrap()
                .receipt,
            settled
        );
        assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    }
}

/// zerus-5wmh.66.4.7.2: Codex's update phase is copied only from the CLI's
/// exact receipt of Update now, kept through the drained settlement and the
/// ledger; a malformed or missing one stays Unknown, never a refusal, and
/// the original is never offered again.
#[test]
fn a_codex_update_phase_survives_settlement_and_a_malformed_one_stays_unknown() {
    use wire::UpdatePhase;
    /// The option, the receipt's phase value and Terminal flag, then the
    /// settled phase, its code and the outcome's phase.
    type Case = (
        &'static str,
        Option<Value>,
        bool,
        Phase,
        &'static str,
        Option<UpdatePhase>,
    );
    let cases: Vec<Case> = vec![
        (
            "update_now",
            Some(json!("installing")),
            true,
            Phase::Recorded,
            "",
            Some(UpdatePhase::Installing),
        ),
        (
            "update_now",
            Some(json!("exited")),
            true,
            Phase::Recorded,
            "",
            Some(UpdatePhase::Exited),
        ),
        ("not_now", None, false, Phase::Recorded, "", None),
        ("skip_version", None, false, Phase::Recorded, "", None),
        (
            "update_now",
            Some(json!("none")),
            true,
            Phase::Unknown,
            wire::RECEIPT_PHASE,
            None,
        ),
        (
            "update_now",
            Some(Value::Null),
            true,
            Phase::Unknown,
            wire::RECEIPT_PHASE,
            None,
        ),
        (
            "update_now",
            None,
            true,
            Phase::Unknown,
            wire::RECEIPT_PHASE,
            None,
        ),
        (
            "update_now",
            Some(json!("installing")),
            false,
            Phase::Unknown,
            wire::RECEIPT_PHASE,
            None,
        ),
    ];
    for (index, (option, value, terminal, phase, code, expected)) in cases.into_iter().enumerate() {
        let (_temp, location, mut store, epoch) = fixture();
        let verified = original_of(&update_question(), option, &location.configuration, &epoch);
        let (record, held) = running(&mut store, &location, &verified, &epoch);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ledger::ended(&tx, &row, ledger::AWAITING_DRAIN).unwrap();
        tx.commit().unwrap();
        drop(held);
        let mut cli = cli_record(&verified.plan, wire::ANSWERED);
        cli["open_terminal"] = json!(terminal);
        if let Some(value) = value {
            cli["codex_update"] = value;
        }
        let read = wire::Outcome::read(&verified.plan, cli.to_string().as_bytes());
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let observation = released(&location, &record);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let settled = ledger::drained(tx, row, observation, &read, Default::default()).unwrap();
        assert_eq!(
            (settled.phase, settled.code.as_str()),
            (phase, code),
            "{index}"
        );
        assert!(settled.drained && settled.valid(), "{index}");
        // The stored row is the settled one, with the same phase.
        let stored = ledger::load(&store.db, &verified.plan.request_id)
            .unwrap()
            .unwrap()
            .receipt;
        assert_eq!(stored, settled, "{index}");
        if phase == Phase::Recorded {
            let outcome = stored.outcome.clone().unwrap();
            assert_eq!(outcome.codex_update, expected, "{index}");
            assert_eq!(outcome.open_terminal, terminal, "{index}");
            assert_eq!(stored.result(&verified.plan), Some(Ok(outcome)), "{index}");
        } else {
            // Unknown for good: never settled, never a refusal before
            // delivery, so nothing may answer it again.
            assert!(stored.outcome.is_none(), "{index}");
            assert_eq!(stored.result(&verified.plan), None, "{index}");
            assert!(!stored.not_delivered(&verified.plan), "{index}");
        }
        assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    }
}

/// An original no live runtime holds (an earlier run's) has no original
/// store observation: Store recovery drains it Unknown and never reads a
/// receipt, however exact.
#[test]
fn an_unheld_original_drains_unknown_without_reading_any_receipt() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (record, held) = running(&mut store, &location, &verified, &epoch);
    let state = location.root.parent().unwrap().to_path_buf();
    write_receipt(
        &state,
        &verified.plan.request_id,
        cli_record(&verified.plan, "rejected")
            .to_string()
            .as_bytes(),
    );
    drop(held);
    let _ = released(&location, &record);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        store.recover_operation_custody(&location);
        let row = ledger::load(&store.db, &verified.plan.request_id)
            .unwrap()
            .unwrap();
        if row.receipt.drained {
            assert_eq!(
                (row.receipt.phase, row.receipt.code.as_str()),
                (Phase::Unknown, super::receipt::UNHELD)
            );
            break;
        }
        assert!(std::time::Instant::now() < until, "never drained");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(store.creation_recovery_ok());
}

#[test]
fn no_unknown_code_poses_as_the_closed_not_started_result() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    let row = ledger::pending(&store.db).unwrap().unwrap();
    for code in [wire::NOT_STARTED, "awaiting_not_started_drain"] {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::ended(&tx, &row, code).is_err(), "{code}");
    }
}

#[test]
fn the_lookup_reads_only_its_exact_original() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    let query = wire::Query::for_plan(verified.intent.scope.clone(), &verified.plan).unwrap();
    let read = |store: &Store, query: &wire::Query| {
        super::query(
            store,
            &json!({"op": "deferred_answer_result", "query": query}),
        )
    };
    let lookup: wire::Lookup = serde_json::from_value(read(&store, &query)).unwrap();
    assert!(lookup.matches(&query) && lookup.read_only);
    assert_eq!(lookup.receipt.unwrap().phase, Phase::Running);
    let mut changed = query.clone();
    changed.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        read(&store, &changed)["code"],
        "deferred_answer_result_unavailable"
    );
    // Another plan under the same request ID is not this original's.
    let mut other = query.clone();
    other.plan_sha256 = "f".repeat(64);
    assert_eq!(
        read(&store, &other)["code"],
        "deferred_answer_result_unavailable"
    );
    assert_eq!(
        super::query(
            &store,
            &json!({"op": "task_message_result", "query": query})
        )["code"],
        "deferred_answer_result_unavailable"
    );
    let mut missing = query;
    missing.request_id = uuid::Uuid::new_v4().to_string();
    let lookup: wire::Lookup = serde_json::from_value(read(&store, &missing)).unwrap();
    assert!(!lookup.found && lookup.receipt.is_none());
}

/// A private CLI state root under `parent`, as the CLI keeps it.
fn state_root(parent: &Path) -> PathBuf {
    let state = parent.join("state");
    std::fs::create_dir(&state).unwrap();
    mode(&state, 0o700);
    state
}
fn observe(state: &Path, plan: &wire::Plan) -> wire::Read {
    let roots = super::receipt::Roots::open(state, super::receipt::FOLDER).unwrap();
    super::receipt::read(&roots, plan, &|| {}).0
}

/// The observation of the CLI's own receipt: only an exact, private,
/// bounded record in the original, unchanged store is an outcome or the
/// CLI's refusal.
#[test]
fn the_receipt_observation_reads_only_the_exact_private_record() {
    use super::receipt::{MISSING, UNAVAILABLE};
    use wire::Read as Observed;
    let temp = tempfile::tempdir().unwrap();
    let state = state_root(&temp.path().canonicalize().unwrap());
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let plan = &verified.plan;
    let id = &plan.request_id;
    let folder = state.join("question_receipts");
    let path = folder.join(format!("{id}.json"));
    let clear = || {
        let _ = std::fs::remove_file(&path);
    };
    // No folder, then no record.
    assert_eq!(observe(&state, plan), Observed::Unknown(MISSING));
    std::fs::create_dir(&folder).unwrap();
    mode(&folder, 0o700);
    assert_eq!(observe(&state, plan), Observed::Unknown(MISSING));
    // The exact recorded answers, and the exact refusal.
    for status in [wire::ANSWERED, wire::SUBMITTED] {
        write_receipt(&state, id, cli_record(plan, status).to_string().as_bytes());
        match observe(&state, plan) {
            Observed::Recorded(outcome) => {
                assert!(outcome.matches(plan));
                assert_eq!(outcome.status, status);
                assert_eq!(outcome.input_sha256, plan.input_sha256);
            }
            other => panic!("{other:?}"),
        }
    }
    write_receipt(
        &state,
        id,
        cli_record(plan, "rejected").to_string().as_bytes(),
    );
    assert_eq!(observe(&state, plan), Observed::Rejected);
    let with = |key: &str, value: Value| {
        let mut record = cli_record(plan, wire::ANSWERED);
        record[key] = value;
        record.to_string()
    };
    for (body, code) in [
        (
            cli_record(plan, "in_progress").to_string(),
            wire::RECEIPT_IN_PROGRESS,
        ),
        // The CLI keeps an earlier record when another stdin reuses the ID.
        (
            with("digest", json!("f".repeat(64))),
            wire::RECEIPT_MISMATCH,
        ),
        (with("name", json!("claude/other")), wire::RECEIPT_MISMATCH),
        (with("run_id", json!("run-2")), wire::RECEIPT_MISMATCH),
        (
            with("conversation_id", json!("conversation-2")),
            wire::RECEIPT_MISMATCH,
        ),
        (with("question_id", json!("q-2")), wire::RECEIPT_MISMATCH),
        (
            with("question_hash", json!("c".repeat(64))),
            wire::RECEIPT_MISMATCH,
        ),
        (with("status", json!("failed")), wire::RECEIPT_MISMATCH),
        ("{".into(), wire::RECEIPT_INVALID),
    ] {
        write_receipt(&state, id, body.as_bytes());
        assert_eq!(observe(&state, plan), Observed::Unknown(code), "{body}");
    }
    // Over the bound, though otherwise exact.
    let mut large = cli_record(plan, wire::ANSWERED);
    large["answers"] = json!("x".repeat(wire::RECEIPT_BYTES));
    write_receipt(&state, id, large.to_string().as_bytes());
    assert_eq!(observe(&state, plan), Observed::Unknown(UNAVAILABLE));
    // Unsafe records and folders are never read.
    let exact = cli_record(plan, wire::ANSWERED).to_string();
    write_receipt(&state, id, exact.as_bytes());
    mode(&path, 0o644);
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(UNAVAILABLE),
        "shared record"
    );
    write_receipt(&state, id, exact.as_bytes());
    mode(&folder, 0o755);
    assert!(
        super::receipt::Roots::open(&state, super::receipt::FOLDER).is_err(),
        "a shared folder is never held"
    );
    mode(&folder, 0o700);
    let elsewhere = temp.path().join("elsewhere.json");
    std::fs::write(&elsewhere, exact.as_bytes()).unwrap();
    mode(&elsewhere, 0o600);
    clear();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(UNAVAILABLE),
        "linked record"
    );
    clear();
    std::fs::hard_link(&elsewhere, &path).unwrap();
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(UNAVAILABLE),
        "second name"
    );
    clear();
    let fifo = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path; mkfifo only creates the node.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(
        observe(&state, plan),
        Observed::Unknown(UNAVAILABLE),
        "a FIFO is never opened for a blocking read"
    );
    clear();
    // A linked or shared state root is never held.
    mode(&state, 0o755);
    assert!(
        super::receipt::Roots::open(&state, super::receipt::FOLDER).is_err(),
        "shared root"
    );
    mode(&state, 0o700);
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(&state, &linked).unwrap();
    assert!(
        super::receipt::Roots::open(&linked, super::receipt::FOLDER).is_err(),
        "linked root"
    );
}

/// review: the original store is the one opened before the child; a moved,
/// replaced or changing root, folder or record never proves a submission,
/// however exact its bytes, and is never a safe absence.
/// Only the original, unchanged store proves this original's record: an
/// exact `answered` record, or an exact `rejected` one, is never an outcome
/// or the CLI's refusal once the root, folder or record changed.
#[test]
fn only_the_original_unchanged_store_proves_an_answer() {
    only_the_original_unchanged_store_proves(wire::ANSWERED);
}
#[test]
fn only_the_original_unchanged_store_proves_a_refusal() {
    only_the_original_unchanged_store_proves("rejected");
}
fn only_the_original_unchanged_store_proves(status: &str) {
    use super::receipt::{read, Roots, CHANGED, MISSING};
    use wire::Read as Observed;
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let plan = &verified.plan;
    let id = plan.request_id.clone();
    let exact = cli_record(plan, status).to_string();
    let proven = |read: &Observed| match read {
        Observed::Recorded(outcome) => status != "rejected" && outcome.matches(plan),
        Observed::Rejected => status == "rejected",
        Observed::Unknown(_) => false,
    };
    // A fresh store under `parent`/state holding the exact record, and the
    // store's roots opened before anything changes.
    let fresh = |parent: &Path, folder: bool| -> (PathBuf, Roots) {
        std::fs::create_dir_all(parent).unwrap();
        let state = state_root(parent);
        if folder {
            write_receipt(&state, &id, exact.as_bytes());
        }
        let roots = Roots::open(&state, super::receipt::FOLDER).unwrap();
        (state, roots)
    };
    let copy = |state: &Path, to: &Path| {
        std::fs::create_dir(to).unwrap();
        mode(to, 0o700);
        write_receipt(to, &id, exact.as_bytes());
        let _ = state;
    };
    // Each case changes the store after the roots were opened.
    type Change<'a> = Box<dyn Fn(&Path, &Path) + 'a>;
    let cases: Vec<(&str, bool, Change, &str)> = vec![
        // desktop-u's negative: the state moved away, and a link at its old
        // name points to another private store with matching JSON.
        (
            "linked root",
            true,
            Box::new(|parent, state| {
                std::fs::rename(state, parent.join("moved")).unwrap();
                let other = parent.join("other");
                copy(state, &other);
                std::os::unix::fs::symlink(&other, state).unwrap();
            }),
            CHANGED,
        ),
        (
            "copied root",
            true,
            Box::new(|parent, state| {
                std::fs::rename(state, parent.join("moved")).unwrap();
                copy(state, state);
            }),
            CHANGED,
        ),
        (
            "replaced ancestor",
            true,
            Box::new(|parent, state| {
                let up = parent.parent().unwrap().join(format!(
                    "{}-moved",
                    parent.file_name().unwrap().to_str().unwrap()
                ));
                std::fs::rename(parent, &up).unwrap();
                std::fs::create_dir(parent).unwrap();
                copy(state, state);
            }),
            CHANGED,
        ),
        (
            "replaced folder",
            true,
            Box::new(|parent, state| {
                let folder = state.join("question_receipts");
                std::fs::rename(&folder, parent.join("old-folder")).unwrap();
                write_receipt(state, &id, exact.as_bytes());
            }),
            CHANGED,
        ),
        // A safe absence only in the original store.
        (
            "absent, root replaced",
            false,
            Box::new(|parent, state| {
                std::fs::rename(state, parent.join("moved")).unwrap();
                state_root(parent);
            }),
            CHANGED,
        ),
        (
            "absent from the held folder, root replaced",
            true,
            Box::new(|parent, state| {
                std::fs::remove_file(state.join("question_receipts").join(format!("{id}.json")))
                    .unwrap();
                std::fs::rename(state, parent.join("moved")).unwrap();
                copy(state, state);
            }),
            CHANGED,
        ),
        (
            "absent from the held folder, folder replaced",
            true,
            Box::new(|parent, state| {
                let folder = state.join("question_receipts");
                std::fs::remove_file(folder.join(format!("{id}.json"))).unwrap();
                std::fs::rename(&folder, parent.join("old-folder")).unwrap();
                write_receipt(state, &id, exact.as_bytes());
            }),
            CHANGED,
        ),
    ];
    for (index, (name, folder, change, code)) in cases.into_iter().enumerate() {
        let parent = base.join(format!("case-{index}"));
        let (state, roots) = fresh(&parent, folder);
        change(&parent, &state);
        assert_eq!(
            read(&roots, plan, &|| {}).0,
            Observed::Unknown(code),
            "{name}"
        );
    }
    // The same changes during the read: after the bytes, before the checks.
    let during: Vec<(&str, Change)> = vec![
        (
            "in place, same bytes",
            Box::new(|_, state| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let path = state.join("question_receipts").join(format!("{id}.json"));
                let bytes = std::fs::read(&path).unwrap();
                std::fs::write(&path, bytes).unwrap();
            }),
        ),
        (
            "in place, appended",
            Box::new(|_, state| {
                use std::io::Write;
                let path = state.join("question_receipts").join(format!("{id}.json"));
                let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
                file.write_all(b" ").unwrap();
            }),
        ),
        (
            "final name moved",
            Box::new(|_, state| {
                let folder = state.join("question_receipts");
                std::fs::rename(
                    folder.join(format!("{id}.json")),
                    folder.join("elsewhere.json"),
                )
                .unwrap();
            }),
        ),
        (
            "final name replaced with the same bytes",
            Box::new(|parent, state| {
                let other = parent.join("same.json");
                std::fs::write(&other, exact.as_bytes()).unwrap();
                mode(&other, 0o600);
                std::fs::rename(
                    other,
                    state.join("question_receipts").join(format!("{id}.json")),
                )
                .unwrap();
            }),
        ),
        (
            "folder replaced",
            Box::new(|parent, state| {
                let folder = state.join("question_receipts");
                std::fs::rename(&folder, parent.join("old-folder")).unwrap();
                write_receipt(state, &id, exact.as_bytes());
            }),
        ),
        (
            "root linked",
            Box::new(|parent, state| {
                std::fs::rename(state, parent.join("moved")).unwrap();
                let other = parent.join("other");
                copy(state, &other);
                std::os::unix::fs::symlink(&other, state).unwrap();
            }),
        ),
    ];
    for (index, (name, change)) in during.into_iter().enumerate() {
        let parent = base.join(format!("during-{index}"));
        let (state, roots) = fresh(&parent, true);
        let between = || change(&parent, &state);
        assert_eq!(
            read(&roots, plan, &between).0,
            Observed::Unknown(CHANGED),
            "{name}"
        );
    }
    // Unchanged, the original store proves it; a folder created after the
    // roots were opened is read from the held root.
    let (_, roots) = fresh(&base.join("unchanged"), true);
    assert!(proven(&read(&roots, plan, &|| {}).0));
    // A record renamed into the original folder before the read is how the
    // CLI itself writes every receipt (in_progress, then its result): it is
    // the original folder's record. Only a change during the read refuses.
    let (state, roots) = fresh(&base.join("rewritten"), true);
    write_receipt(&state, &id, exact.as_bytes());
    assert!(proven(&read(&roots, plan, &|| {}).0));
    let (state, roots) = fresh(&base.join("later-folder"), false);
    assert_eq!(read(&roots, plan, &|| {}).0, Observed::Unknown(MISSING));
    write_receipt(&state, &id, exact.as_bytes());
    assert!(proven(&read(&roots, plan, &|| {}).0));
}

/// The observation's brackets, on a held store of their own: the state
/// root must be the held one before the observation runs and again after
/// its reply, and the session must be current on both sides.
#[test]
fn the_observation_is_bracketed_by_the_held_root_and_the_session() {
    use super::receipt::{Roots, FOLDER};
    use super::runtime::observed;
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    let reply =
        || Some(format!("{}\n", json!({"found": true, "consent_identity": pinned()})).into_bytes());
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let check = |roots: &Roots, change: &dyn Fn(), calls: &Cell<u32>| {
        observed(
            &verified,
            roots,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
            |args, input, _| {
                calls.set(calls.get() + 1);
                assert_eq!(args, ["answer", SESSION, "--observe", "--json"]);
                assert!(!input.is_empty());
                change();
                reply()
            },
        )
    };
    let replace = |state: &Path| {
        let moved = state.with_file_name("moved");
        std::fs::rename(state, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, state).unwrap();
    };
    for (name, before, during, current) in [
        ("unchanged", false, false, true),
        ("replaced before", true, false, false),
        ("replaced during", false, true, false),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let state = state_root(&temp.path().canonicalize().unwrap());
        session_record(&state, SESSION, "claude", RUN, CONVERSATION);
        let roots = Roots::open(&state, FOLDER).unwrap();
        if before {
            replace(&state);
        }
        let calls = Cell::new(0);
        let change = || {
            if during {
                replace(&state);
            }
        };
        assert_eq!(check(&roots, &change, &calls), current, "{name}");
        // A replaced root before it is never observed at all.
        assert_eq!(calls.get(), u32::from(!before), "{name}");
    }
    // The session before the observation, as after it.
    let temp = tempfile::tempdir().unwrap();
    let state = state_root(&temp.path().canonicalize().unwrap());
    session_record(&state, SESSION, "claude", "run-2", CONVERSATION);
    let roots = Roots::open(&state, FOLDER).unwrap();
    let calls = Cell::new(0);
    assert!(!check(&roots, &|| (), &calls));
    assert_eq!(calls.get(), 0, "another run is never observed");
}

mod actor {
    use super::super::child::{self, Ended};
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

    struct Fixture {
        _temp: tempfile::TempDir,
        location: Location,
        store: Store,
        runtime: runtime::Runtime,
        exclusion: Arc<crate::desktop::store::HandoffLock>,
        epoch: String,
        config: Config,
        owner: Arc<crate::platform::FileLock>,
        /// The epoch's sealed negatives: every runtime of this epoch shares them.
        fences: Arc<crate::desktop::not_admitted::Fences>,
    }
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
    impl Fixture {
        /// `execute` stands in for the one child; it also receives the CLI
        /// state root, where the real `hgs send` writes its receipt.
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
            Self::build(Some(Arc::new(execute)))
        }
        /// No executor: the original path runs, and this test image's own
        /// self-exec refusal stops it before any child.
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
            // The CLI's state root is the service root's parent, where the
            // selected session's own record is.
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
                location,
                store,
                runtime,
                exclusion,
                epoch,
                config,
                owner,
                fences,
            }
        }
        /// The fixture's executor, observer and macOS fence on `runtime`.
        fn install(runtime: &mut runtime::Runtime, state: &Path, execute: Option<Arc<Execute>>) {
            // This synthetic source/Validated seam tests the actor, not native
            // authentication or an installed frontend authority positive.
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
            // The observation child finds the question the person answered,
            // asked exactly for it.
            runtime.fixture_observer(Arc::new(|args, input| {
                assert_eq!(args, ["answer", SESSION, "--observe", "--json"]);
                let asked: Value = serde_json::from_slice(input).unwrap();
                assert_eq!(
                    asked,
                    json!({"question_id": "q-1", "expected_run_id": RUN,
                        "expected_conversation_id": CONVERSATION})
                );
                Some(observation(true, Some(pinned())))
            }));
            // The shared actor logic is tested on every platform; the macOS
            // fence has its own test.
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
        fn request(original: &Verified) -> Value {
            json!(
                zerus_client_core::desktop::ui_requests::Request::DeferredAnswer {
                    intent: Box::new(original.intent.clone()),
                }
            )
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            serde_json::from_value(self.runtime.frontend(
                &mut self.store,
                ui_authority::Validated::fixture(&Self::request(original)),
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
            ledger::load(&self.store.db, &original.plan.request_id)
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
    fn answered(state: &Path, plan: &wire::Plan) {
        write_receipt(
            state,
            &plan.request_id,
            cli_record(plan, wire::ANSWERED).to_string().as_bytes(),
        );
    }
    /// The observation child's reply, as `hgs answer --observe` prints it.
    fn observation(found: bool, identity: Option<Value>) -> Vec<u8> {
        format!(
            "{}\n",
            json!({"found": found, "consent_identity": identity})
        )
        .into_bytes()
    }

    #[test]
    fn an_admitted_answer_runs_once_and_settles_only_from_its_cli_receipt() {
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let mut f = Fixture::new(move |state, plan, args, input, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            // The one `hgs answer <session> --json`, with the exact stdin.
            assert_eq!(args, ["answer", SESSION, "--json"]);
            let stdin: Value = serde_json::from_slice(input).unwrap();
            assert_eq!(stdin["request_id"], plan.request_id.as_str());
            assert_eq!(
                (
                    stdin["question_id"].as_str(),
                    stdin["expected_question_hash"].as_str()
                ),
                (Some("q-1"), Some(HASH))
            );
            assert_eq!(
                (
                    stdin["expected_run_id"].as_str(),
                    stdin["expected_conversation_id"].as_str()
                ),
                (Some(RUN), Some(CONVERSATION))
            );
            assert_eq!(
                format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(input)),
                plan.input_sha256
            );
            answered(state, plan);
            Ended::Exited { code: 0 }
        });
        let original = f.original();
        let admission = f.admit(&original);
        assert!(admission.accepted && admission.durable && admission.matches(&original.intent));
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Recorded, "")
        );
        let outcome = receipt.outcome.clone().unwrap();
        assert_eq!(outcome.status, wire::ANSWERED);
        assert_eq!(receipt.result(&original.plan), Some(Ok(outcome)));
        // The same original admitted again returns its settled receipt.
        assert_eq!(f.admit(&original).receipt.unwrap(), receipt);
        f.runtime.request_stop();
        f.until("stopped", |f| f.runtime.drained());
        assert_eq!(sent.load(Ordering::SeqCst), 1, "never sent again");
    }

    #[test]
    fn an_exit_alone_never_settles_and_only_the_exact_receipt_records() {
        enum Record {
            None,
            Exact,
            Rejected,
            InProgress,
            OtherStdin,
        }
        for (ended, record, phase, code) in [
            (
                Ended::Exited { code: 0 },
                Record::None,
                Phase::Unknown,
                super::super::receipt::MISSING,
            ),
            (
                Ended::Exited { code: 0 },
                Record::InProgress,
                Phase::Unknown,
                wire::RECEIPT_IN_PROGRESS,
            ),
            (
                Ended::Exited { code: 1 },
                Record::OtherStdin,
                Phase::Unknown,
                wire::RECEIPT_MISMATCH,
            ),
            // The CLI's exact record, whatever the exit said.
            (
                Ended::Exited { code: 2 },
                Record::Exact,
                Phase::Recorded,
                "",
            ),
            (
                Ended::Unknown("deferred_answer_output_retained"),
                Record::Exact,
                Phase::Recorded,
                "",
            ),
            (
                Ended::Unknown("deferred_answer_timed_out"),
                Record::None,
                Phase::Unknown,
                "deferred_answer_timed_out",
            ),
            // The CLI's exact durable refusal: nothing reached the pane.
            (
                Ended::Exited { code: 1 },
                Record::Rejected,
                Phase::Failed,
                wire::REJECTED,
            ),
            (
                Ended::Unknown("deferred_answer_signalled"),
                Record::Rejected,
                Phase::Failed,
                wire::REJECTED,
            ),
        ] {
            let result = std::sync::Mutex::new(Some((ended, record)));
            let mut f = Fixture::new(move |state, plan, _, _, _, _| {
                let (ended, record) = result.lock().unwrap().take().unwrap();
                let body = match record {
                    Record::None => None,
                    Record::Exact => Some(cli_record(plan, wire::ANSWERED)),
                    Record::Rejected => Some(cli_record(plan, "rejected")),
                    Record::InProgress => Some(cli_record(plan, "in_progress")),
                    Record::OtherStdin => {
                        let mut other = cli_record(plan, "rejected");
                        other["digest"] = json!("e".repeat(64));
                        Some(other)
                    }
                };
                if let Some(body) = body {
                    write_receipt(state, &plan.request_id, body.to_string().as_bytes());
                }
                ended
            });
            let original = f.original();
            assert!(f.admit(&original).accepted);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!((receipt.phase, receipt.code.as_str()), (phase, code));
            f.runtime.request_stop();
            f.until("stopped", |f| f.runtime.drained());
        }
        // The real original path: this test image refuses its own image, and
        // no record of the request exists.
        let mut f = Fixture::unexecuted();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Failed, wire::NOT_STARTED)
        );
        assert!(receipt.not_delivered(&original.plan));
    }

    /// The real original path, without an executor: the runtime's pinned
    /// image (a script here) is run first as the observation, without any
    /// custody, then, only for a current question, as the original call
    /// with its frozen stdin and custody descriptor, writing the CLI's
    /// receipt as `hgs answer` does. Any other observation starts nothing.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_image_observes_first_and_answers_only_a_current_question() {
        let mut changed = pinned();
        changed["approval"] = json!("allow_always");
        let cases: [(&str, String, u64, bool); 6] = [
            (
                "current",
                String::from_utf8(observation(true, Some(pinned()))).unwrap(),
                0,
                true,
            ),
            (
                "not found",
                String::from_utf8(observation(false, None)).unwrap(),
                0,
                false,
            ),
            (
                "changed consent",
                String::from_utf8(observation(true, Some(changed))).unwrap(),
                0,
                false,
            ),
            (
                "an unknown field",
                json!({"found": true, "consent_identity": pinned(), "source": "x"}).to_string(),
                0,
                false,
            ),
            (
                "a failed observation",
                String::from_utf8(observation(true, Some(pinned()))).unwrap(),
                1,
                false,
            ),
            (
                "a late observation",
                String::from_utf8(observation(true, Some(pinned()))).unwrap(),
                2,
                false,
            ),
        ];
        for (name, reply, behaviour, answers) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let calls = dir.path().join("calls");
            let mut f = Fixture::unexecuted();
            f.runtime
                .fixture_observation_wait(Duration::from_millis(300));
            let receipts = f.config.state.join("question_receipts");
            std::fs::create_dir(&receipts).unwrap();
            mode(&receipts, 0o700);
            std::fs::write(
                &path,
                format!(
                    r#"#!/usr/bin/env python3
import hashlib,json,os,sys,time
data=sys.stdin.buffer.read()
with open({calls:?},"a") as log:
    log.write(json.dumps({{"argv":sys.argv[1:],"stdin":data.decode(),"custody":os.environ.get("HGS_DEFERRED_ANSWER_CUSTODY_FD")}})+"\n")
if "--observe" in sys.argv:
    if {behaviour}==1:
        sys.stdout.write({reply:?})
        sys.exit(1)
    if {behaviour}==2: time.sleep(2)
    sys.stdout.write({reply:?})
    sys.exit(0)
request=json.loads(data)
record={{"request_id":request["request_id"],"name":sys.argv[2],"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"question_id":request["question_id"],"question_hash":request["expected_question_hash"],"digest":hashlib.sha256(data).hexdigest(),"status":"answered","transport":"claude_tui"}}
target=os.path.join({receipts:?},request["request_id"]+".json")
fd=os.open(target+".tmp",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
os.write(fd,json.dumps(record).encode())
os.close(fd)
os.rename(target+".tmp",target)
print(json.dumps(record))
"#,
                    calls = calls.to_str().unwrap(),
                    receipts = receipts.to_str().unwrap(),
                ),
            )
            .unwrap();
            mode(&path, 0o700);
            f.runtime.fixture_image(&path);
            let original = f.original();
            assert!(f.admit(&original).accepted, "{name}");
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            let calls: Vec<Value> = std::fs::read_to_string(&calls)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            // The observation, exactly asked and holding no custody.
            assert_eq!(
                calls[0]["argv"],
                json!(["answer", SESSION, "--observe", "--json"]),
                "{name}"
            );
            assert_eq!(
                serde_json::from_str::<Value>(calls[0]["stdin"].as_str().unwrap()).unwrap(),
                json!({"question_id": "q-1", "expected_run_id": RUN,
                    "expected_conversation_id": CONVERSATION}),
                "{name}"
            );
            assert!(calls[0]["custody"].is_null(), "{name}");
            if answers {
                assert_eq!(
                    (receipt.phase, receipt.code.as_str()),
                    (Phase::Recorded, ""),
                    "{name}"
                );
                assert_eq!(calls.len(), 2, "{name}");
                assert_eq!(calls[1]["argv"], json!(["answer", SESSION, "--json"]));
                assert_eq!(calls[1]["stdin"], original.intent.original.as_str());
                assert!(calls[1]["custody"]
                    .as_str()
                    .is_some_and(|fd| fd.parse::<u32>().is_ok()));
            } else {
                assert_eq!(
                    (receipt.phase, receipt.code.as_str()),
                    (Phase::Failed, wire::NOT_STARTED),
                    "{name}"
                );
                assert!(receipt.not_delivered(&original.plan), "{name}");
                assert_eq!(calls.len(), 1, "{name}: never answered");
            }
        }
    }

    /// The original child writes a reply, starts a descendant that keeps its
    /// stdout open and exits. Like `hgs` (adopt_custody), it never lets the
    /// descendant inherit the custody lock. Only an EOF on the test's accepted
    /// socket ends the descendant, which writes the CLI's record of the
    /// request last, as a late submission would.
    const RETAINING: &str = r#"import os,subprocess,sys
sys.stdin.buffer.read()
sys.stdout.buffer.write(b'{}')
sys.stdout.flush()
subprocess.Popen([sys.executable,'-c',"import os,socket,sys\nc=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\nc.connect(sys.argv[1])\nassert c.recv(1)==b''\nt=sys.argv[2]+'.tmp'\nf=os.open(t,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)\nos.write(f,sys.argv[3].encode())\nos.close(f)\nos.rename(t,sys.argv[2])",sys.argv[1],sys.argv[2],sys.argv[3]],stdin=subprocess.DEVNULL,close_fds=True)
os._exit(0)"#;

    /// The one connection of the descendant, accepted within a bound.
    fn accept(
        listener: &std::os::unix::net::UnixListener,
        mut turn: impl FnMut(),
    ) -> std::os::unix::net::UnixStream {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < until, "the descendant never connected");
                    turn();
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn a_descendant_pipe_keeps_custody_and_the_receipt_is_read_only_after_its_eof() {
        use crate::desktop::custody::{self, Observation};
        let control = tempfile::tempdir().unwrap();
        let socket = control.path().join("c");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let path = socket.to_str().unwrap().to_owned();
        // A real original child; its descendant keeps the original stdout
        // open past the deadline and holds no custody lock of its own.
        let mut f = Fixture::new(move |state, plan, _, input, lock, resources| {
            let folder = state.join("question_receipts");
            std::fs::create_dir(&folder).unwrap();
            mode(&folder, 0o700);
            child::run(
                Path::new("/usr/bin/env"),
                &[
                    "python3".into(),
                    "-c".into(),
                    RETAINING.into(),
                    path.clone(),
                    folder
                        .join(format!("{}.json", plan.request_id))
                        .to_str()
                        .unwrap()
                        .into(),
                    cli_record(plan, wire::SUBMITTED).to_string(),
                ],
                input,
                lock,
                resources,
                Instant::now() + Duration::from_millis(1500),
                |_| Ok(()),
            )
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        // Keep the SAME accepted endpoint through the logical end.
        let release = accept(&listener, || {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        });
        f.until("logical end", |f| {
            f.row(&original).receipt.code == "deferred_answer_output_retained"
        });
        let row = f.row(&original);
        assert_eq!(row.receipt.phase, Phase::Unknown);
        assert!(!row.receipt.drained);
        let record = row.custody.clone().unwrap();
        assert!(matches!(
            custody::probe(&f.location, &record),
            Observation::Held
        ));
        // Neither revoke nor a stop request releases the original resources,
        // and no receipt is read while they are held.
        f.revoke();
        f.runtime.request_stop();
        for _ in 0..40 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!f.runtime.drained(), "takeover stays blocked");
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 1, "charged");
        assert!(!f.row(&original).receipt.drained);
        assert!(matches!(
            custody::probe(&f.location, &record),
            Observation::Held
        ));
        // EOF is the one explicit release of the descendant, which writes the
        // CLI's record last, just before its end releases the custody.
        drop(release);
        f.until("drained", |f| {
            f.runtime.drained()
                && matches!(
                    custody::probe(&f.location, &record),
                    Observation::Released { .. }
                )
        });
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
        let receipt = f.row(&original).receipt;
        assert!(receipt.drained);
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Recorded, ""),
            "read only after the physical drain"
        );
    }

    #[test]
    fn a_send_past_its_deadline_is_signalled_and_drains_unknown() {
        assert_eq!(runtime::DEADLINE, Duration::from_secs(30));
        let mut f = Fixture::new(|_, _, _, input, lock, resources| {
            child::run(
                Path::new("/bin/sh"),
                &["-c".into(), "cat >/dev/null; exec sleep 30".into()],
                input,
                lock,
                resources,
                Instant::now() + Duration::from_millis(1500),
                |_| Ok(()),
            )
        });
        let started = Instant::now();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert!(started.elapsed() < Duration::from_secs(8));
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Unknown, "deferred_answer_timed_out")
        );
    }

    #[test]
    fn the_macos_fence_refuses_before_any_row_lease_or_child() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("no fenced child"));
        f.runtime.fixture_mac_fence(true);
        let original = f.original();
        let admission = f.admit(&original);
        assert!(!admission.accepted && !admission.durable);
        assert_eq!(admission.code, runtime::MAC_UNAVAILABLE);
        assert!(admission.receipt.is_none());
        assert!(ledger::load(&f.store.db, &original.plan.request_id)
            .unwrap()
            .is_none());
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
        for _ in 0..20 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        assert!(!f.runtime.status()["working"].as_bool().unwrap());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_native_permit_that_expires_while_begin_waits_admits_nothing() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("expired permit must not execute"));
        let original = f.original();
        let authority = ui_authority::Validated::fixture_expiring(
            &Fixture::request(&original),
            Instant::now() + Duration::from_millis(150),
        );
        let database = f.location.root.join("state.sqlite3");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut db = rusqlite::Connection::open(database).unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            tx.commit().unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let answer = f
            .runtime
            .frontend(&mut f.store, authority, Some(f.exclusion.clone()));
        writer.join().unwrap();
        assert_eq!(answer["accepted"], false, "consumed after it expired");
        // Refused after the empty read and before the admission: it never
        // started, so it is answered not admitted and sealed (zerus-tlb9q).
        assert_eq!(answer["code"], wire::NOT_ADMITTED);
        assert!(f.fences.deferred_answer.sealed(&original.plan.request_id));
        assert!(ledger::load(&f.store.db, &original.plan.request_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn another_scope_or_an_invalid_original_admits_nothing() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("refused original must not execute"));
        let mut original = f.original();
        original.intent.scope.service_epoch = uuid::Uuid::new_v4().to_string();
        let admission = f.admit(&original);
        assert!(!admission.accepted);
        assert_eq!(admission.code, "deferred_answer_unavailable");
        assert!(ledger::load(&f.store.db, &original.plan.request_id)
            .unwrap()
            .is_none());
        let mut original = f.original();
        original.intent.scope.configuration = "b".repeat(64);
        assert!(!f.admit(&original).accepted);
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
    }

    #[test]
    fn a_revoke_after_admission_starts_nothing_and_retires_the_original() {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let mut f = Fixture::new(move |_, _, _, _, _, _| {
            called.fetch_add(1, Ordering::SeqCst);
            Ended::Unknown("lost")
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.revoke();
        f.until("retired", |f| f.row(&original).receipt.drained);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Unknown, "deferred_answer_interrupted")
        );
        assert!(receipt.result(&original.plan).is_none());
    }

    #[test]
    fn duplicates_while_running_and_after_a_lost_result_never_send_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        let mut f = Fixture::new(move |_, _, _, _, _, _| {
            called.fetch_add(1, Ordering::SeqCst);
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ended::Unknown("deferred_answer_unobserved")
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("running child", |_| calls.load(Ordering::SeqCst) == 1);
        for _ in 0..3 {
            assert!(f.admit(&original).accepted);
        }
        release_tx.send(()).unwrap();
        f.until("Unknown drained", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        // The lost result with no CLI record stays Unknown, never unsent.
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Unknown, "deferred_answer_unobserved")
        );
        for _ in 0..3 {
            assert_eq!(f.admit(&original).receipt.unwrap(), receipt);
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        f.runtime.request_stop();
        f.until("drained", |f| f.runtime.drained());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the original send");
    }

    #[test]
    fn a_revoked_writer_refuses_admission_and_no_child_starts() {
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let mut f = Fixture::new(move |_, _, _, _, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ended::Exited { code: 0 }
        });
        let original = f.original();
        f.revoke();
        let admission = f.admit(&original);
        assert!(!admission.accepted);
        for _ in 0..50 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(sent.load(Ordering::SeqCst), 0, "no child after revoke");
        assert!(ledger::load(&f.store.db, &original.plan.request_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn restart_retires_a_queued_send_and_never_reconstructs_execution_consent() {
        let mut f = Fixture::new(|_, _, _, _, _, _| panic!("recovered send must not execute"));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone(), f.fences.clone()).unwrap();
        // A new runtime fences macOS again; the fixture's runs without it.
        f.runtime.fixture_mac_fence(false);
        f.runtime.fixture_executor(
            MACHINE,
            Arc::new(|_, _, _, _, _| panic!("restart must not execute")),
        );
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
            (receipt.phase, receipt.code.as_str()),
            (Phase::Unknown, "deferred_answer_interrupted")
        );
        // The same original again gets only that receipt.
        let again = f.admit(&original);
        assert!(again.accepted);
        assert_eq!(again.receipt.unwrap(), receipt);
    }
    /// The name's hash, as the CLI names a session's files.
    fn hashed(name: &str) -> String {
        use sha2::Digest;
        format!("{:x}.json", sha2::Sha256::digest(name.as_bytes()))
    }
    fn private_file(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        mode(path, 0o600);
    }

    /// desktop-u (#1393 R4): the name's agent prefix routes a send here but
    /// never authorizes it. The actual record, backend and run are checked
    /// last before the child; a refusal there never started anything.
    #[test]
    fn the_selected_sessions_actual_record_backend_and_run_are_checked_before_the_child() {
        type Change = fn(&Path);
        let cases: [(&str, Change, bool); 8] = [
            ("the selected record", |_| (), true),
            (
                "a routed record",
                |state| {
                    let file = hashed(SESSION);
                    std::fs::rename(state.join(&file), state.join("bindings").join(&file))
                        .or_else(|_| {
                            std::fs::create_dir(state.join("bindings"))?;
                            mode(&state.join("bindings"), 0o700);
                            std::fs::rename(state.join(&file), state.join("bindings").join(&file))
                        })
                        .unwrap();
                    private_file(&state.join("name_routes").join(&file), "{}");
                },
                true,
            ),
            (
                "a DeepSeek binding",
                |state| private_file(&state.join("dsh/bindings").join(hashed(SESSION)), "{}"),
                false,
            ),
            (
                "an OpenCode binding",
                |state| private_file(&state.join("opencode/bindings").join(hashed(SESSION)), "{}"),
                false,
            ),
            (
                "another run",
                |state| session_record(state, SESSION, "claude", "run-2", CONVERSATION),
                false,
            ),
            (
                "another conversation",
                |state| session_record(state, SESSION, "claude", RUN, "conversation-2"),
                false,
            ),
            (
                "another agent",
                |state| session_record(state, SESSION, "codex", RUN, CONVERSATION),
                false,
            ),
            (
                "no record",
                |state| std::fs::remove_file(state.join(hashed(SESSION))).unwrap(),
                false,
            ),
        ];
        for (name, change, runs) in cases {
            let sent = Arc::new(AtomicUsize::new(0));
            let count = sent.clone();
            let mut f = Fixture::new(move |state, plan, _, _, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
                answered(state, plan);
                Ended::Exited { code: 0 }
            });
            change(&f.config.state);
            let original = f.original();
            assert!(f.admit(&original).accepted, "{name}");
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            if runs {
                assert_eq!(receipt.phase, Phase::Recorded, "{name}");
                assert_eq!(sent.load(Ordering::SeqCst), 1, "{name}");
            } else {
                assert_eq!(
                    (receipt.phase, receipt.code.as_str()),
                    (Phase::Failed, wire::NOT_STARTED),
                    "{name}"
                );
                assert_eq!(sent.load(Ordering::SeqCst), 0, "{name}: no child");
            }
        }
    }

    /// The answer child starts only after the observation, read without any
    /// effect, finds exactly the pinned consent identity of a v1 source.
    #[test]
    fn only_an_exact_current_observation_starts_the_answer() {
        let mut changed = pinned();
        changed["approval"] = json!("allow_always");
        let mut gained = pinned();
        gained["optional"] = json!(true);
        let mut lost = pinned();
        lost.as_object_mut().unwrap().remove("answer_transport");
        let mut direct = pinned();
        direct["source"] = json!("hook");
        let mut large = observation(true, Some(pinned()));
        large.resize(super::super::child::OBSERVATION_BYTES + 1, b' ');
        let cases: Vec<(&str, Option<Vec<u8>>)> = vec![
            ("no reply", None),
            ("not found", Some(observation(false, None))),
            ("found without an identity", Some(observation(true, None))),
            (
                "an identity not found",
                Some(observation(false, Some(pinned()))),
            ),
            ("changed consent", Some(observation(true, Some(changed)))),
            ("a gained field", Some(observation(true, Some(gained)))),
            ("a lost field", Some(observation(true, Some(lost)))),
            ("a direct source", Some(observation(true, Some(direct)))),
            (
                "an unknown field",
                Some(
                    json!({"found": true, "consent_identity": pinned(), "source": "x"})
                        .to_string()
                        .into_bytes(),
                ),
            ),
            ("not JSON", Some(b"{".to_vec())),
            ("over the bound", Some(large)),
            ("late", Some(Vec::new())),
        ];
        for (name, reply) in cases {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let mut f = Fixture::new(move |state, plan, _, _, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
                answered(state, plan);
                Ended::Exited { code: 0 }
            });
            // An empty reply stands for the exact one, arriving past the
            // observation's own deadline.
            let late = reply.as_ref().is_some_and(Vec::is_empty);
            if late {
                f.runtime
                    .fixture_observation_wait(Duration::from_millis(100));
            }
            let reply = std::sync::Mutex::new(reply);
            f.runtime.fixture_observer(Arc::new(move |_, _| {
                if late {
                    std::thread::sleep(Duration::from_millis(300));
                    return Some(observation(true, Some(pinned())));
                }
                reply.lock().unwrap().clone()
            }));
            let original = f.original();
            assert!(f.admit(&original).accepted, "{name}");
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.phase, receipt.code.as_str()),
                (Phase::Failed, wire::NOT_STARTED),
                "{name}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{name}: never answered");
        }
    }

    /// The selected session's backend and run are checked again after the
    /// observation: a change while it ran starts nothing.
    #[test]
    fn the_session_is_checked_again_after_the_observation() {
        type Change = fn(&Path);
        let cases: [(&str, Change); 3] = [
            ("another run", |state| {
                session_record(state, SESSION, "claude", "run-2", CONVERSATION)
            }),
            ("another conversation", |state| {
                session_record(state, SESSION, "claude", RUN, "conversation-2")
            }),
            ("a DeepSeek binding", |state| {
                private_file(&state.join("dsh/bindings").join(hashed(SESSION)), "{}")
            }),
        ];
        for (name, change) in cases {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let mut f = Fixture::new(move |state, plan, _, _, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
                answered(state, plan);
                Ended::Exited { code: 0 }
            });
            let state = f.config.state.clone();
            f.runtime.fixture_observer(Arc::new(move |_, _| {
                change(&state);
                Some(observation(true, Some(pinned())))
            }));
            let original = f.original();
            assert!(f.admit(&original).accepted, "{name}");
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.phase, receipt.code.as_str()),
                (Phase::Failed, wire::NOT_STARTED),
                "{name}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{name}: never answered");
        }
    }

    /// review-w D6: a consent stopped after the reservation committed and
    /// before the spawn starts nothing. The hook runs in the actual final
    /// spawn turn, after the worker's own checks, so only the runner's
    /// veto there can refuse.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stop_revoke_or_source_change_after_reservation_starts_nothing() {
        for action in ["stop", "revoke", "source"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let ran = dir.path().join("ran");
            // It answers the observation with the pinned question; only the
            // original call touches `ran`.
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\ncat >/dev/null\nif [ \"$3\" = --observe ]; then printf '%s\\n' '{}'; exit 0; fi\ntouch {:?}\n",
                    json!({"found": true, "consent_identity": pinned()}),
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
            assert_eq!(f.row(&original).receipt.phase, Phase::Running, "{action}");
            match action {
                "stop" => f.runtime.request_stop(),
                "revoke" => f.revoke(),
                _ => f
                    .runtime
                    .fixture_machine("33333333-3333-4333-8333-333333333333"),
            }
            // The actor sees the change and stops the live consent.
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            release.send(()).unwrap();
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.phase, receipt.code.as_str()),
                (Phase::Failed, wire::NOT_STARTED),
                "{action}"
            );
            assert!(!ran.exists(), "{action}: no child ever existed");
        }
    }

    /// desktop-u P2: a drain worker blocked in its receipt read keeps the
    /// serve-owner loan and the charge until it physically ends, through a
    /// stop and the runtime's teardown, and its late result never records.
    #[test]
    fn a_blocked_drain_read_keeps_its_loans_and_never_records_late() {
        let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let entered_tx = std::sync::Mutex::new(entered_tx);
        let released = std::sync::Mutex::new(released);
        let mut f = Fixture::new(|state, plan, _, _, _, _| {
            answered(state, plan);
            Ended::Exited { code: 0 }
        });
        f.runtime.fixture_read(
            Some(Arc::new(move || {
                let _ = entered_tx.lock().unwrap().send(());
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            })),
            Duration::from_millis(200),
        );
        let original = f.original();
        assert!(f.admit(&original).accepted);
        let until = Instant::now() + Duration::from_secs(8);
        while entered.try_recv().is_err() {
            assert!(Instant::now() < until, "the read never started");
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        // Blocked: a stop drains nothing and the charge stays.
        f.runtime.request_stop();
        for _ in 0..20 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!f.runtime.drained());
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 1, "charged");
        // Teardown: the worker's own loan outlives the runtime.
        let torn = std::mem::replace(
            &mut f.runtime,
            runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone(), f.fences.clone()).unwrap(),
        );
        drop(torn);
        // The fixture's and the new runtime's copies, plus the worker's.
        assert!(Arc::strong_count(&f.owner) > 2, "the worker keeps its loan");
        for _ in 0..20 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 1, "still charged");
        assert!(!f.row(&original).receipt.drained);
        // Released past its deadline: the worker ends, its loan with it, and
        // nothing records the exact receipt it read late.
        std::thread::sleep(Duration::from_millis(250));
        release.send(()).unwrap();
        f.until("the worker's loan ended", |f| {
            Arc::strong_count(&f.owner) == 2
        });
        f.until("drained", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.phase, Phase::Unknown);
        assert_eq!(receipt.code, super::super::receipt::UNHELD);
        assert!(receipt.result(&original.plan).is_none());
    }

    #[test]
    fn a_receipt_read_past_its_deadline_is_unknown_however_exact() {
        let mut f = Fixture::new(|state, plan, _, _, _, _| {
            answered(state, plan);
            Ended::Exited { code: 0 }
        });
        f.runtime.fixture_read(
            Some(Arc::new(|| std::thread::sleep(Duration::from_millis(300)))),
            Duration::from_millis(100),
        );
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Unknown, super::super::receipt::TIMED_OUT)
        );
    }
    /// Store recovery never drains an original its live runtime holds: only
    /// the runtime reads its receipt from the original store.
    #[test]
    fn store_recovery_leaves_the_runtimes_own_original_to_it() {
        let mut f = Fixture::new(|state, plan, _, _, _, _| {
            answered(state, plan);
            Ended::Exited { code: 0 }
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("ended", |f| {
            f.row(&original).receipt.code == ledger::AWAITING_DRAIN
        });
        // Released, owned, not yet probed by the runtime: Store recovery
        // alone never settles it.
        for _ in 0..60 {
            f.store.recover_operation_custody(&f.location);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!f.row(&original).receipt.drained, "left to its runtime");
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.phase, Phase::Recorded);
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
            zerus_client_core::desktop::ui_requests::Request::DeferredAnswer {
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
    /// A fixture whose one child answers and counts itself, and the same
    /// executor for a rebuilt runtime.
    fn counting() -> (Arc<AtomicUsize>, Fixture) {
        let ran = Arc::new(AtomicUsize::new(0));
        let f = Fixture::build(Some(answering(ran.clone())));
        (ran, f)
    }
    fn answering(ran: Arc<AtomicUsize>) -> Arc<Execute> {
        erased(move |state, plan, _, _, _, _| {
            ran.fetch_add(1, Ordering::SeqCst);
            answered(state, plan);
            Ended::Exited { code: 0 }
        })
    }

    /// zerus-tlb9q, desktop-u's pin: a request refused before its admission
    /// with no row is answered not admitted and sealed for the epoch; an
    /// otherwise admissible copy of the same request that arrives after it
    /// gets the same answer and never runs. The lookup never answers it.
    #[test]
    fn a_request_refused_before_admission_is_sealed_and_its_copy_never_runs() {
        let (ran, mut f) = counting();
        let original = f.original();
        let id = original.plan.request_id.clone();
        let refused = offer(&mut f, broken(&original));
        assert_eq!(
            (refused.accepted, refused.durable, refused.code.as_str()),
            (false, false, wire::NOT_ADMITTED)
        );
        assert!(refused.not_admitted(&original.intent), "{}", refused.code);
        assert!(no_row(&f, &id) && f.fences.deferred_answer.sealed(&id));
        let copy = f.admit(&original);
        assert!(copy.not_admitted(&original.intent), "{}", copy.code);
        idle(&mut f);
        assert_eq!(ran.load(Ordering::SeqCst), 0, "the copy never ran");
        assert!(no_row(&f, &id));
        let query = wire::Query::for_plan(original.intent.scope.clone(), &original.plan).unwrap();
        let lookup = super::super::query(&f.store, &json!({"op": "deferred_answer_result", "query": query}));
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
            zerus_client_core::desktop::ui_requests::Request::DeferredAnswer {
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
        assert_eq!(reply["code"], "deferred_answer_unavailable", "{reply}");
        assert!(!f.fences.deferred_answer.sealed(&foreign.plan.request_id));
        // This epoch's seal is no answer for another connection's copy.
        let original = f.original();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        let mut intent = original.intent.clone();
        intent.scope.service_epoch = uuid::Uuid::new_v4().to_string();
        let copy = intent.verify().unwrap();
        let reply = f.admit(&copy);
        assert_eq!(reply.code, "deferred_answer_unavailable");
        assert!(!reply.not_admitted(&copy.intent));
        assert!(no_row(&f, &original.plan.request_id));
    }

    /// The seal belongs to the epoch: the background runtime destroyed and
    /// built again under the same epoch still refuses the sealed request,
    /// and a request of another epoch's connection is never sealed.
    #[test]
    fn a_runtime_rebuilt_in_the_same_epoch_keeps_the_seal() {
        let (ran, mut f) = counting();
        let original = f.original();
        let id = original.plan.request_id.clone();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        f.rebuild(answering(ran.clone()));
        let copy = f.admit(&original);
        assert!(copy.not_admitted(&original.intent), "{}", copy.code);
        idle(&mut f);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(no_row(&f, &id));
        // The rebuilt runtime still admits a new request.
        let other = f.original();
        assert!(f.admit(&other).accepted);
        f.until("settled", |f| f.row(&other).receipt.drained);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        // A request of another epoch's connection: unavailable, not sealed.
        let stale = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let reply = f.admit(&stale);
        assert_eq!(
            (reply.accepted, reply.code.as_str()),
            (false, "deferred_answer_unavailable")
        );
        assert!(!reply.not_admitted(&stale.intent));
        assert!(!f.fences.deferred_answer.sealed(&stale.plan.request_id));
        assert!(no_row(&f, &stale.plan.request_id));
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
        assert!(no_row(&f, &original.plan.request_id));
        // Another connection's request refused there is never sealed.
        let stale = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let reply = f.admit(&stale);
        assert_eq!(reply.code, "deferred_answer_unavailable");
        assert!(!f.fences.deferred_answer.sealed(&stale.plan.request_id));

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
            (false, "deferred_answer_unavailable")
        );
        assert!(!f.fences.deferred_answer.sealed(&original.plan.request_id));

        // A row exists: the same refusal stays unavailable.
        let (_, mut f) = counting();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let existing = offer(&mut f, broken(&original));
        assert_eq!(
            (existing.accepted, existing.code.as_str()),
            (false, "deferred_answer_unavailable")
        );
        f.runtime.request_stop();
        let stopped = f.admit(&original);
        assert_eq!(stopped.code, "deferred_answer_unavailable");
        assert!(!f.fences.deferred_answer.sealed(&original.plan.request_id));

        // The admission itself fails: unavailable although no row is left,
        // and nothing is sealed.
        let (ran, mut f) = counting();
        let original = f.original();
        f.store
            .db
            .execute_batch(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON desktop_deferred_answers \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let failed = f.admit(&original);
        assert_eq!(
            (failed.accepted, failed.code.as_str()),
            (false, "deferred_answer_unavailable")
        );
        assert!(no_row(&f, &original.plan.request_id));
        assert!(!f.fences.deferred_answer.sealed(&original.plan.request_id));
        f.store.db.execute_batch("DROP TRIGGER refuse;").unwrap();
        assert!(f.admit(&original).accepted, "an unavailable refusal seals nothing");
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        // A full fence mints no new negative; its sealed IDs stay sealed.
        let (_, mut f) = counting();
        for n in 0..crate::desktop::not_admitted::BOUND {
            assert!(f.fences.deferred_answer.seal(&format!("sealed-{n}")));
        }
        let original = f.original();
        let full = offer(&mut f, broken(&original));
        assert_eq!(
            (full.accepted, full.code.as_str()),
            (false, "deferred_answer_unavailable")
        );
        assert!(!f.fences.deferred_answer.sealed(&original.plan.request_id));
        assert!(f.fences.deferred_answer.sealed("sealed-0"));
    }

    #[test]
    fn budget_corruption_or_ambiguous_commit_is_unavailable_not_not_admitted() {
        for failure in ["counter", "row", "commit"] {
            let (sent, mut f) = counting();
            let original = f.original();
            if failure == "counter" {
                f.store
                    .db
                    .execute("UPDATE desktop_deferred_answer_budget SET bytes=1", [])
                    .unwrap();
            } else if failure == "row" {
                let victim = f.original();
                assert!(f.admit(&victim).accepted);
                let receipt = serde_json::to_string(&f.row(&victim).receipt).unwrap();
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_deferred_answers SET receipt=?1 WHERE request_id=?2",
                        rusqlite::params![format!("{receipt}\0suffix"), victim.plan.request_id],
                    )
                    .unwrap();
            } else {
                f.store.db.execute_batch("PRAGMA foreign_keys=ON;
                            CREATE TABLE send_parent(id INTEGER PRIMARY KEY);
                            CREATE TABLE send_child(id INTEGER REFERENCES send_parent(id) DEFERRABLE INITIALLY DEFERRED);
                            CREATE TEMP TRIGGER fail_send_commit AFTER INSERT ON desktop_deferred_answers BEGIN INSERT INTO send_child VALUES(1); END;").unwrap();
            }
            let result = f.admit(&original);
            assert_eq!(result.code, "deferred_answer_unavailable", "{failure}");
            assert!(!result.not_admitted(&original.intent));
            assert!(!f.fences.deferred_answer.sealed(&original.plan.request_id));
            assert!(no_row(&f, &original.plan.request_id));
            assert_eq!(sent.load(Ordering::SeqCst), 0);
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
        assert!(no_row(&f, &refused.plan.request_id));
        assert_eq!(super::reservation_budget::retained(&f.store.db), protected);
        assert_eq!(sent.load(Ordering::SeqCst), 0);
        assert_eq!(super::reservation_budget::budget(&f.store.db).2, 32);
        f.runtime.request_stop();
        f.until("queued originals retire without input", |f| {
            f.runtime.drained()
        });
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
        assert!(f.admit(&refused).not_admitted(&refused.intent));
        assert_eq!(sent.load(Ordering::SeqCst), 0);
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
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        f.runtime.request_stop();
        f.until("all original resources settle", |f| f.runtime.drained());
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
    }
}

/// desktop-u's G1 on #1433: this kind's child runner checks the
/// never-renewed deadline last before the spawn and before any input.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod spent_deadline {
    use crate::desktop::original_child::{crossed, OFFERED, SPAWNED};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use zerus_client_core::run::custody::InheritedLock;

    pub(super) fn custody() -> Arc<InheritedLock> {
        InheritedLock::try_lock(tempfile::tempfile().unwrap()).unwrap()
    }
    pub(super) fn resources() -> Arc<dyn Send + Sync> {
        Arc::new(())
    }
    pub(super) fn cat() -> &'static Path {
        Path::new("/bin/cat")
    }
    pub(super) fn clear() {
        SPAWNED.with(|spawned| spawned.set(None));
        OFFERED.with(|offered| offered.set(None));
    }
    pub(super) fn spawned() -> Option<u32> {
        SPAWNED.with(|spawned| spawned.take())
    }
    pub(super) fn expired() -> Instant {
        Instant::now() - Duration::from_millis(1)
    }
    pub(super) fn soon() -> Instant {
        Instant::now() + Duration::from_secs(20)
    }

    #[test]
    fn an_answer_child_never_starts_once_its_deadline_is_spent() {
        use super::super::child::{run, Ended};
        let input = b"private fixture answer";
        clear();
        let ended = run(
            cat(),
            &[],
            input,
            &custody(),
            &resources(),
            expired(),
            |_| Ok(()),
        );
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(spawned(), None, "no child after its deadline");
        let ended = crossed(|deadline, check| {
            clear();
            run(cat(), &[], input, &custody(), &resources(), deadline, check)
        });
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(spawned(), None, "no child once the last check spent it");
        // Within its budget, the same child runs with all of its input.
        clear();
        let ended = run(cat(), &[], input, &custody(), &resources(), soon(), |_| {
            Ok(())
        });
        assert_eq!(ended, Ended::Exited { code: 0 });
        assert!(spawned().is_some());
        assert_eq!(OFFERED.with(|offered| offered.take()), Some(input.len()));
    }

    #[test]
    fn an_observation_child_never_starts_once_its_deadline_is_spent() {
        use super::super::child::observe;
        clear();
        assert_eq!(observe(cat(), &[], b"{}", expired(), |_| Ok(())), None);
        assert_eq!(spawned(), None);
        let observed = crossed(|deadline, check| {
            clear();
            observe(cat(), &[], b"{}", deadline, check)
        });
        assert_eq!(observed, None);
        assert_eq!(spawned(), None);
        clear();
        assert_eq!(
            observe(cat(), &[], b"{}", soon(), |_| Ok(())),
            Some(b"{}".to_vec())
        );
    }
}

/// zerus-ucck P3a (#1437 review): the deferred answer's target check is the
/// deferred send's, `original_store::session_current`. These are the send's
/// target tests for the answer: a session in the ID layout is found through
/// its proven label index; the label's own record counts with its twins; an
/// unreadable record or candidate, an unproven index and a malformed listed
/// index all refuse.
mod id_layout_target {
    use super::*;
    use sha2::Digest;

    fn private(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        mode(path.parent().unwrap(), 0o700);
        std::fs::write(path, bytes).unwrap();
        mode(path, 0o600);
    }
    fn current(state: &Path, plan: &wire::Plan) -> bool {
        let roots = super::super::receipt::Roots::open(state, super::super::receipt::FOLDER).unwrap();
        super::super::runtime::session_current(&roots, plan)
    }
    fn key() -> String {
        format!("{:x}", sha2::Sha256::digest(SESSION.as_bytes()))
    }
    /// Writes the label's index and a manifest entry that proves it.
    fn prove(state: &Path, index: &Value) {
        let bytes = index.to_string();
        private(&state.join("labels").join(format!("{}.json", key())), bytes.as_bytes());
        let digest = format!("{:x}", sha2::Sha256::digest(bytes.as_bytes()));
        private(
            &state.join("labels").join(".complete"),
            json!({"version": 1, "labels": {key(): digest}}).to_string().as_bytes(),
        );
    }
    fn twin(state: &Path, id: &str, run: &str) {
        let record = json!({"name": SESSION, "agent": "claude", "run_id": run,
            "conversation_id": CONVERSATION, "session_id": id, "layout": "id"});
        private(&state.join("sessions").join(format!("{id}.json")), record.to_string().as_bytes());
    }

    #[test]
    fn a_session_in_the_id_layout_is_found_through_its_proven_index() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_root(temp.path());
        let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
        let plan = &verified.plan;
        session_record(&state, SESSION, "claude", "run-other", "conversation-other");
        assert!(!current(&state, plan), "the label's holder runs another run");
        let id = uuid::Uuid::new_v4().to_string();
        twin(&state, &id, RUN);
        assert!(!current(&state, plan), "no label index: refused");
        let index = state.join("labels").join(format!("{}.json", key()));
        private(&index, json!({"version": 1, "label": SESSION, "ids": [id]}).to_string().as_bytes());
        assert!(!current(&state, plan), "an index no manifest proves: refused");
        prove(&state, &json!({"version": 1, "label": SESSION, "ids": [id]}));
        assert!(current(&state, plan));
        let stale = uuid::Uuid::new_v4().to_string();
        private(&index, json!({"version": 1, "label": SESSION, "ids": [id, stale]}).to_string().as_bytes());
        assert!(!current(&state, plan), "a changed index no longer matches its manifest entry");
        prove(&state, &json!({"version": 1, "label": SESSION, "ids": [id, stale]}));
        assert!(current(&state, plan), "a listed session whose file is gone is stale");
        private(&state.join("sessions").join(format!("{stale}.json")), b"{");
        assert!(!current(&state, plan), "an unreadable listed candidate refuses");
    }

    #[test]
    fn the_label_record_counts_with_its_twins() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_root(temp.path());
        let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
        let plan = &verified.plan;
        session_record(&state, SESSION, "claude", RUN, CONVERSATION);
        assert!(current(&state, plan), "no ID layout: the label's record decides");
        let sessions = state.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        mode(&sessions, 0o700);
        assert!(!current(&state, plan), "the ID layout exists and nothing proves the label");
        private(&state.join("labels").join(".complete"), json!({"version": 1, "labels": {}}).to_string().as_bytes());
        assert!(current(&state, plan), "the manifest proves the label has no twin");
        let id = uuid::Uuid::new_v4().to_string();
        prove(&state, &json!({"version": 1, "label": SESSION, "ids": [id]}));
        private(&sessions.join(format!("{id}.json")), b"{");
        assert!(!current(&state, plan), "a listed twin that cannot be read");
        twin(&state, &id, "run-other");
        assert!(current(&state, plan), "a twin on another run: the label's record is the one");
        twin(&state, &id, RUN);
        assert!(!current(&state, plan), "the label's record and a twin both carry the plan");
    }

    #[test]
    fn a_listed_candidate_without_its_identity_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_root(temp.path());
        let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
        let plan = &verified.plan;
        session_record(&state, SESSION, "claude", RUN, CONVERSATION);
        let id = uuid::Uuid::new_v4().to_string();
        twin(&state, &id, "run-other");
        prove(&state, &json!({"version": 1, "label": SESSION, "ids": [id]}));
        assert!(current(&state, plan), "a valid twin on another run is a non-match");
        let path = state.join("sessions").join(format!("{id}.json"));
        for record in [
            Value::Null,
            json!({}),
            json!({"name": SESSION, "agent": "claude", "run_id": "run-other", "session_id": id, "layout": "legacy"}),
            json!({"name": SESSION, "agent": "claude", "run_id": "run-other", "session_id": uuid::Uuid::new_v4().to_string(), "layout": "id"}),
        ] {
            private(&path, record.to_string().as_bytes());
            assert!(!current(&state, plan), "a listed candidate {record}");
        }
        // review-q2: while the label's record matches, a twin that cannot even
        // be opened, a link or a file that is not private, refuses too.
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(state.join("missing"), &path).unwrap();
        assert!(!current(&state, plan), "a listed twin that is a link");
        std::fs::remove_file(&path).unwrap();
        twin(&state, &id, "run-other");
        mode(&path, 0o644);
        assert!(!current(&state, plan), "a listed twin that is not private");
        mode(&path, 0o600);
        assert!(current(&state, plan));
    }

    #[test]
    fn every_malformed_listed_index_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_root(temp.path());
        let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
        let plan = &verified.plan;
        session_record(&state, SESSION, "claude", RUN, CONVERSATION);
        let id = uuid::Uuid::new_v4().to_string();
        twin(&state, &id, "run-other");
        prove(&state, &json!({"version": 1, "label": SESSION, "ids": [id]}));
        assert!(current(&state, plan), "a well-formed index whose twin runs another run");
        for ids in [
            None,
            Some(Value::Null),
            Some(json!({})),
            Some(json!([])),
            Some(json!([1])),
            Some(json!(["not-an-id"])),
            Some(json!([id, "not-an-id"])),
            Some(json!([id, 1])),
        ] {
            let mut index = json!({"version": 1, "label": SESSION});
            if let Some(ids) = ids.clone() {
                index["ids"] = ids;
            }
            prove(&state, &index);
            assert!(!current(&state, plan), "a listed index with ids {ids:?}");
        }
    }

}
/// Reservation accounting controls use only synthetic originals and private Store fixtures.
/// They are ledger/Store tests, not native-agent qualification.
mod reservation_budget {
    use super::*;
    pub(super) fn budget(db: &rusqlite::Connection) -> (i64, i64, i64) {
        db.query_row(
            "SELECT rows,bytes,active FROM desktop_deferred_answer_budget WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    fn key(v: &Verified) -> &str {
        &v.plan.request_id
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
        db.query_row("SELECT length(CAST(source AS BLOB))+length(CAST(pinned AS BLOB))+length(CAST(checkpoint AS BLOB))+length(CAST(plan AS BLOB))+37888 FROM desktop_deferred_answers WHERE request_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_deferred_answers)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_deferred_answers ORDER BY request_id",
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
                + v.intent.pinned.len()
                + v.intent.checkpoint.len()
                + serde_json::to_string(&v.plan).unwrap().len()
                + 37888;
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
                "UPDATE desktop_deferred_answer_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
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
        let leaf = &status["kinds"]["deferred_answer"];
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
                "counter"=>{store.db.execute("UPDATE desktop_deferred_answer_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_deferred_answers SET receipt='{}' WHERE request_id=(SELECT request_id FROM desktop_deferred_answers LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_deferred_answers SET drained=2 WHERE request_id=(SELECT request_id FROM desktop_deferred_answers LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_deferred_answer_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_deferred_answer_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
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
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_deferred_answer_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_deferred_answer_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
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
                "UPDATE desktop_deferred_answer_budget SET rows=1,bytes=1,active=1 WHERE id=1",
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
        ledger::drained(
            tx,
            row,
            observed,
            &wire::Read::Unknown(crate::desktop::deferred_answer::receipt::UNHELD),
            crate::desktop::deferred_answer::receipt::Evidence::default(),
        )
        .unwrap();
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
            .prepare("SELECT * FROM desktop_deferred_answers")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_deferred_answers", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_deferred_answers VALUES({})",
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
            ("source", 65536_usize),
            ("pinned", 65536_usize),
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
                    .prepare("PRAGMA table_info(desktop_deferred_answers)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_deferred_answers", [], |r| {
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
                    "INSERT INTO desktop_deferred_answers VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_deferred_answer_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
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
