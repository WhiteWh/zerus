use super::ledger::Child;
use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use zerus_client_core::desktop::session_resume::{Intent, Settled, Step, Verified};
use zerus_client_core::operation_recovery::deferred_input::{self as checkpoint, Checkpoint};

const SESSION: &str = "claude/private-resume";
/// A Codex session, whose readiness only the send's own observation shows
/// (zerus-5wmh.66.4.10).
const CODEX: &str = "codex/private-resume";
/// The observation replies `hgs send --observe` prints.
const OBSERVED_READY: &[u8] = br#"{"ready":true,"reason":""}"#;
const OBSERVED_NOT_READY: &[u8] =
    br#"{"ready":false,"reason":"terminal has a draft, dialog, or unrecognized prompt"}"#;
const RUN: &str = "run-1";
const RESUMED: &str = "00000000-0000-4000-8000-0000000000b1";
const CONVERSATION: &str = "conversation-1";
const MACHINE: &str = "22222222-2222-4222-8222-222222222222";

/// A saved version 3 resume-then-send checkpoint with the exact resume stdin
/// and send template the client builds for it.
fn original(configuration: &str, epoch: &str) -> Verified {
    original_of(configuration, epoch, SESSION)
}
/// [`original`] for the session `session`.
fn original_of(configuration: &str, epoch: &str, session: &str) -> Verified {
    let text = "after resume";
    let checkpoint = Checkpoint::reserve_session_resume(checkpoint::Intent {
        kind: checkpoint::Kind::ResumeToSend,
        draft_revision: 3,
        message_sha256: checkpoint::message_sha256(text, Vec::<(&str, &str, &str, &[u8])>::new()),
        run_id: RUN.into(),
        conversation_id: CONVERSATION.into(),
        question: None,
        start: Some(checkpoint::Start::Resume),
        retry_of: None,
        consent: true,
    })
    .unwrap();
    let resume = json!({"request_id": checkpoint.request_id, "action": "resume",
        "expected_run_id": RUN, "expected_conversation_id": CONVERSATION});
    let stdin = json!({"request_id": checkpoint.send_request_id, "text": text, "attachments": [],
        "expected_run_id": RUN, "expected_conversation_id": CONVERSATION});
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        session: session.into(),
        request_id: checkpoint.request_id.clone(),
        send_request_id: checkpoint.send_request_id.clone().unwrap(),
        checkpoint: serde_json::to_string(&checkpoint).unwrap(),
        resume: resume.to_string(),
        original: stdin.to_string(),
    }
    .verify()
    .unwrap()
}
/// The resume's own exact outcome for `plan`.
fn resumed(plan: &wire::Plan) -> wire::Resumed {
    wire::Resumed {
        request_id: plan.resume_request_id.clone(),
        status: wire::COMPLETED.into(),
        result_target: wire::ResultTarget {
            name: plan.session.clone(),
            run_id: RESUMED.into(),
            conversation_id: plan.conversation_id.clone(),
        },
    }
}
fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
/// Writes `body` atomically as the CLI's private receipt `<request>.json` in
/// `folder` under the CLI state root `state`.
fn write_receipt(state: &Path, folder: &str, request: &str, body: &[u8]) -> PathBuf {
    let folder = state.join(folder);
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
/// The exact action receipt `hgs session-action` writes for the resume
/// stdin `stdin` of `plan` with `status` (completed: resumed to
/// [`RESUMED`]).
fn action_receipt(state: &Path, plan: &wire::Plan, stdin: &[u8], status: &str) -> PathBuf {
    let value = crate::state::lifecycle_fixture_receipt(
        &plan.session,
        stdin,
        status,
        (status == "completed").then_some(RESUMED),
    );
    write_receipt(
        state,
        receipt::RESUME_FOLDER,
        &plan.resume_request_id,
        value.to_string().as_bytes(),
    )
}
/// The CLI's own exact input receipt of `plan`'s send with the stdin
/// `stdin`, bound to [`RESUMED`].
fn sent_receipt(state: &Path, plan: &wire::Plan, stdin: &[u8]) {
    let digest = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(stdin));
    write_receipt(
        state,
        receipt::INPUT_FOLDER,
        &plan.send_request_id,
        json!({"request_id": plan.send_request_id, "name": plan.session, "run_id": RESUMED,
            "conversation_id": plan.conversation_id, "digest": digest,
            "status": "submitted", "transport": "terminal"})
        .to_string()
        .as_bytes(),
    );
}
/// The process start identity the CLI records for `pid`.
fn started(pid: u32) -> String {
    crate::platform::process_start_time(pid)
        .or_else(|| crate::platform::process_start_fallback(pid))
        .unwrap_or_default()
}
/// The CLI's own session record of `name` under the state root `state`,
/// with the extra `fields`.
fn session_record(state: &Path, name: &str, agent: &str, run: &str, fields: Value) {
    use sha2::Digest;
    let file = state.join(format!("{:x}.json", sha2::Sha256::digest(name.as_bytes())));
    let temp = state.join(".record.tmp");
    let mut record = json!({"name": name, "agent": agent, "run_id": run,
        "conversation_id": CONVERSATION});
    for (key, value) in fields.as_object().unwrap() {
        record[key] = value.clone();
    }
    std::fs::write(&temp, record.to_string()).unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, file).unwrap();
}
/// The resumed run's record of a confirmed, idle, live Claude: ready.
fn resumed_record(state: &Path, ready: bool) {
    resumed_record_of(state, SESSION, ready)
}
/// The resumed run's record of a resumed Codex pending its first message:
/// it never shows by itself that the agent takes one.
fn codex_pending_record(state: &Path, session: &str) {
    let pid = std::process::id();
    session_record(
        state,
        session,
        "codex",
        RESUMED,
        json!({"run_identity_version": 1, "startup_kind": "resume", "supervisor": {},
            "expected_id": CONVERSATION, "activity": "unknown", "phase": "unknown", "error": "",
            "last_event_at": 0, "pid": pid, "process_start": started(pid)}),
    );
}
/// [`resumed_record`] for the session `session`.
fn resumed_record_of(state: &Path, session: &str, ready: bool) {
    let pid = std::process::id();
    let start = started(pid);
    let (activity, phase) = if ready {
        ("idle", "idle")
    } else {
        ("busy", "working")
    };
    session_record(
        state,
        session,
        "claude",
        RESUMED,
        json!({"expected_id": "", "activity": activity, "phase": phase, "error": "",
            "pid": pid, "process_start": start}),
    );
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
fn exact_previous_store_upgrade_adds_only_the_independent_session_resume_leaf() {
    assert_eq!(
        SCHEMA - 1,
        crate::desktop::delivery::CATEGORIES_SCHEMA,
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
    // ... and the question route leaves' tables (zerus-sa9qn.8 and .32).
    assert_eq!(tables(&store.db), before + 2 + 1 + 2);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    assert_eq!(
        store.operation_status()["kinds"]["session_resume"]["undrained"],
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
    assert_eq!(row.receipt.resume.step, Step::Queued);
    assert_eq!(row.receipt.send.step, Step::Queued);
    assert!(row.resume.is_none() && row.send.is_none() && row.bound().is_none());
    // The same operation again: its original receipt, nothing new.
    let again = admit(&mut store, &verified, &uuid::Uuid::new_v4().to_string());
    assert_eq!(again.receipt, row.receipt);
    assert_eq!(again.writer_epoch, writer);
    // Another send under the same resume conflicts.
    let mut other = verified.intent.clone();
    let mut stdin: Value = serde_json::from_str(&other.original).unwrap();
    stdin["text"] = json!("something else");
    other.original = stdin.to_string();
    assert!(other.verify().is_err());
    // The original columns are immutable and its receipts are retained.
    for sql in [
        "UPDATE desktop_session_resumes SET source='{}'",
        "UPDATE desktop_session_resumes SET resume='{}'",
        "UPDATE desktop_session_resumes SET checkpoint='{}'",
        "UPDATE desktop_session_resumes SET plan='{}'",
        "UPDATE desktop_session_resumes SET writer_epoch='x'",
        "UPDATE desktop_session_resumes SET send_request_id='x'",
        "DELETE FROM desktop_session_resumes",
    ] {
        assert!(store.db.execute(sql, []).is_err(), "{sql}");
    }
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
}

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
    let row = ledger::load(&store.db, row.id()).unwrap().unwrap();
    assert!(row.receipt.drained);
    assert_eq!(row.receipt.code, ledger::INTERRUPTED);
    assert_eq!(
        (row.receipt.resume.step, row.receipt.send.step),
        (Step::NotStarted, Step::NotStarted)
    );
    assert_eq!(
        row.receipt.result(&verified.plan),
        Some(Settled::NotStarted)
    );
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    // Nothing follows the drain.
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::retire(&tx, &row).is_err());
}

fn running(
    store: &mut Store,
    location: &Location,
    verified: &Verified,
    epoch: &str,
) -> (
    zerus_desktop_ledger::operations::custody::Record,
    std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
    zerus_desktop_ledger::operations::Ticket,
) {
    let writer = uuid::Uuid::new_v4().to_string();
    zerus_desktop_ledger::authorize(&mut store.db, &writer, epoch).unwrap();
    let row = admit(store, verified, &writer);
    let ticket = zerus_desktop_ledger::operations::Ticket {
        operation_id: verified.binding.operation_id().unwrap(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        revision: row.receipt.revision + 1,
        admission_id: verified.plan.resume_request_id.clone(),
        binding_sha256: verified.binding.fingerprint().unwrap(),
        writer_epoch: writer,
        service_epoch: epoch.into(),
        lease: uuid::Uuid::new_v4().to_string(),
    };
    // The send can never be reserved before the resume, nor while it runs.
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (prepared, held) = crate::desktop::custody::prepare(location, &ticket).unwrap();
    let record = zerus_desktop_ledger::operations::custody::Record {
        prepared,
        helper: None,
    };
    assert!(ledger::reserve(&tx, &row, Child::Send, &ticket, &record).is_err());
    ledger::reserve(&tx, &row, Child::Resume, &ticket, &record).unwrap();
    tx.commit().unwrap();
    let row = ledger::load(&store.db, &verified.plan.resume_request_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.receipt.resume.step, Step::Running);
    assert_eq!(row.draining(), Some(Child::Resume));
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::reserve(&tx, &row, Child::Send, &ticket, &record).is_err());
    drop(tx);
    // A reservation is immutable once made.
    assert!(store
        .db
        .execute("UPDATE desktop_session_resumes SET resume_custody='{}'", [])
        .is_err());
    (record, held, ticket)
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

fn state_root(parent: &Path) -> PathBuf {
    let state = parent.join("state");
    std::fs::create_dir(&state).unwrap();
    mode(&state, 0o700);
    state
}
/// An absence: a read of a held store that has no receipt of the resume.
fn missing(plan: &wire::Plan, original: &str) -> receipt::Observed {
    let temp = tempfile::tempdir().unwrap();
    let state = state_root(&temp.path().canonicalize().unwrap());
    let folder = state.join(receipt::RESUME_FOLDER);
    std::fs::create_dir(&folder).unwrap();
    mode(&folder, 0o700);
    let roots = receipt::Roots::open(&state, receipt::RESUME_FOLDER).unwrap();
    let observed = receipt::resume(&roots, plan, original.as_bytes(), &|| ()).0;
    assert!(
        matches!(observed, receipt::Observed::Missing(_)),
        "{observed:?}"
    );
    observed
}

/// Only the drain settles the resume, from its own exact receipt: a
/// recorded resume fixes the bound send and waits for it; an exact refusal
/// or anything that is not the resume's own record ends the send unstarted;
/// an absence proves nothing ran only with the runner's no-spawn result.
#[test]
fn only_the_released_custody_settles_the_resume_and_only_its_exact_cli_record_decides() {
    use receipt::{Observed, MISSING};
    #[derive(Clone, Copy, Debug)]
    enum Ending {
        Exited,
        NotStarted,
    }
    let refused = |_: &wire::Plan, _: &str| Observed::Refused;
    let exact = |plan: &wire::Plan, _: &str| Observed::Resumed(resumed(plan));
    let other = |plan: &wire::Plan, _: &str| {
        let mut outcome = resumed(plan);
        outcome.result_target.conversation_id = "other".into();
        Observed::Resumed(outcome)
    };
    let uncertain = |_: &wire::Plan, _: &str| Observed::Unknown(wire::RECEIPT_UNCERTAIN);
    type Read = fn(&wire::Plan, &str) -> Observed;
    use Step::*;
    let cases: [(Ending, Read, Step, &str, Option<Settled>); 8] = [
        (Ending::Exited, exact, Recorded, "", None),
        // An exact record wins over the runner's own no-spawn result.
        (Ending::NotStarted, exact, Recorded, "", None),
        (
            Ending::Exited,
            refused,
            Failed,
            wire::REFUSED,
            Some(Settled::NotSent { resume: Failed }),
        ),
        (
            Ending::NotStarted,
            refused,
            Failed,
            wire::REFUSED,
            Some(Settled::NotSent { resume: Failed }),
        ),
        (
            Ending::NotStarted,
            missing,
            NotStarted,
            wire::NOT_STARTED,
            Some(Settled::NotStarted),
        ),
        // After a possible spawn, an absence proves nothing.
        (
            Ending::Exited,
            missing,
            Unknown,
            MISSING,
            Some(Settled::NotSent { resume: Unknown }),
        ),
        (
            Ending::Exited,
            uncertain,
            Unknown,
            wire::RECEIPT_UNCERTAIN,
            Some(Settled::NotSent { resume: Unknown }),
        ),
        (
            Ending::Exited,
            other,
            Unknown,
            wire::RECEIPT_MISMATCH,
            Some(Settled::NotSent { resume: Unknown }),
        ),
    ];
    for (ending, read, step, code, settled) in cases {
        let (_temp, location, mut store, epoch) = fixture();
        let verified = original(&location.configuration, &epoch);
        let (record, held, _) = running(&mut store, &location, &verified, &epoch);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let row = ledger::load(&tx, &verified.plan.resume_request_id)
            .unwrap()
            .unwrap();
        match ending {
            Ending::Exited => ledger::ended(&tx, &row, ledger::AWAITING_DRAIN).unwrap(),
            Ending::NotStarted => ledger::not_started(&tx, &row).unwrap(),
        }
        tx.commit().unwrap();
        // Held custody never settles.
        assert!(matches!(
            crate::desktop::custody::probe(&location, &record),
            crate::desktop::custody::Observation::Held
        ));
        drop(held);
        let observation = released(&location, &record);
        let observed = read(&verified.plan, &verified.intent.resume);
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let row = ledger::load(&tx, &verified.plan.resume_request_id)
            .unwrap()
            .unwrap();
        let next = ledger::drained(
            tx,
            row,
            observation,
            &observed,
            receipt::Evidence::default(),
        )
        .unwrap();
        let label = format!("{ending:?} {step:?}");
        assert_eq!(next.resume.step, step, "{label}");
        assert_eq!(next.resume.code, code, "{label}");
        let row = ledger::load(&store.db, &verified.plan.resume_request_id)
            .unwrap()
            .unwrap();
        match settled {
            // A recorded resume fixes the bound send and leaves the send
            // queued for the same live operation's wait.
            None => {
                assert!(!next.drained, "{label}");
                assert_eq!(next.send.step, Queued, "{label}");
                let (_, stdin, digest) = row.bound().unwrap();
                assert_eq!(next.bound_sha256.as_deref(), Some(digest.as_str()));
                let stdin: Value = serde_json::from_str(&stdin).unwrap();
                assert_eq!(stdin["expected_run_id"], RESUMED);
                assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
            }
            // Anything else ends the send, which was never reserved.
            Some(settled) => {
                assert!(next.drained, "{label}");
                assert_eq!(next.send.step, NotStarted, "{label}");
                assert!(row.send.is_none(), "{label}");
                assert_eq!(next.result(&verified.plan), Some(settled), "{label}");
                assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
            }
        }
    }
}

/// A recorded resume's bound send is fixed: a stored receipt whose digest
/// is not the template bound to that resume never loads.
#[test]
fn a_changed_bound_send_never_loads() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (record, held, _) = running(&mut store, &location, &verified, &epoch);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::load(&tx, &verified.plan.resume_request_id)
        .unwrap()
        .unwrap();
    ledger::ended(&tx, &row, ledger::AWAITING_DRAIN).unwrap();
    tx.commit().unwrap();
    drop(held);
    let observation = released(&location, &record);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::load(&tx, &verified.plan.resume_request_id)
        .unwrap()
        .unwrap();
    let observed = receipt::Observed::Resumed(resumed(&verified.plan));
    ledger::drained(
        tx,
        row,
        observation,
        &observed,
        receipt::Evidence::default(),
    )
    .unwrap();
    let stored: String = store
        .db
        .query_row("SELECT receipt FROM desktop_session_resumes", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut forged: Value = serde_json::from_str(&stored).unwrap();
    forged["bound_sha256"] = json!("f".repeat(64));
    store
        .db
        .execute(
            "UPDATE desktop_session_resumes SET receipt=?1",
            [forged.to_string()],
        )
        .unwrap();
    assert!(ledger::load(&store.db, &verified.plan.resume_request_id).is_err());
}

/// Restart retires a reserved resume for its read-only drain; store
/// recovery never reads an unheld receipt, and nothing follows it.
#[test]
fn store_recovery_never_reads_an_unheld_receipt_and_never_continues() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (record, held, _) = running(&mut store, &location, &verified, &epoch);
    // A restart: the queued send can never start; the running resume waits
    // for its custody drain.
    ledger::startup(&mut store.db).unwrap();
    let row = ledger::load(&store.db, &verified.plan.resume_request_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.receipt.resume.step, Step::Running);
    assert_eq!(row.receipt.send.step, Step::NotStarted);
    assert!(!row.receipt.drained);
    drop(held);
    let observation = released(&location, &record);
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let row = ledger::load(&tx, &verified.plan.resume_request_id)
        .unwrap()
        .unwrap();
    let next = ledger::unheld(tx, row, observation).unwrap();
    assert!(next.drained);
    assert_eq!(
        (next.resume.step, next.resume.code.as_str()),
        (Step::Unknown, receipt::UNHELD)
    );
    assert_eq!(
        next.result(&verified.plan),
        Some(Settled::NotSent {
            resume: Step::Unknown
        })
    );
}

fn observe(state: &Path, verified: &Verified) -> receipt::Observed {
    let roots = receipt::Roots::open(state, receipt::RESUME_FOLDER).unwrap();
    receipt::resume(
        &roots,
        &verified.plan,
        verified.intent.resume.as_bytes(),
        &|| (),
    )
    .0
}

/// The resume's receipt is read from the held store by the CLI's own
/// observation rule: only the exact `completed` record resumes, only the
/// exact `failed` one refuses, a `claimed` one is uncertain, and another
/// request's record under this name is never this original's.
#[test]
fn the_resume_observation_reads_only_the_exact_private_action_receipt() {
    use receipt::Observed;
    let temp = tempfile::tempdir().unwrap();
    let state = state_root(&temp.path().canonicalize().unwrap());
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    // No folder yet, then an empty one: a read of the held store finds none.
    assert!(matches!(observe(&state, &verified), Observed::Missing(_)));
    action_receipt(
        &state,
        &verified.plan,
        verified.intent.resume.as_bytes(),
        "completed",
    );
    assert_eq!(
        observe(&state, &verified),
        Observed::Resumed(resumed(&verified.plan))
    );
    action_receipt(
        &state,
        &verified.plan,
        verified.intent.resume.as_bytes(),
        "failed",
    );
    assert_eq!(observe(&state, &verified), Observed::Refused);
    for status in ["claimed", "uncertain"] {
        action_receipt(
            &state,
            &verified.plan,
            verified.intent.resume.as_bytes(),
            status,
        );
        assert_eq!(
            observe(&state, &verified),
            Observed::Unknown(wire::RECEIPT_UNCERTAIN),
            "{status}"
        );
    }
    // Another request's record written under this request's name, by
    // another session's action: never this original's.
    let other = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let foreign = crate::state::lifecycle_fixture_receipt(
        SESSION,
        other.intent.resume.as_bytes(),
        "failed",
        None,
    );
    let path = state
        .join(receipt::RESUME_FOLDER)
        .join(format!("{}.json", verified.plan.resume_request_id));
    std::fs::write(&path, foreign.to_string()).unwrap();
    assert_eq!(
        observe(&state, &verified),
        Observed::Unknown(wire::RECEIPT_INVALID)
    );
    // A group-readable receipt is not the CLI's private record.
    action_receipt(
        &state,
        &verified.plan,
        verified.intent.resume.as_bytes(),
        "completed",
    );
    mode(&path, 0o644);
    assert_eq!(
        observe(&state, &verified),
        Observed::Unknown(receipt::UNAVAILABLE)
    );
}

/// The readiness rule: only the resumed run's own record of a confirmed,
/// idle, live agent lets the send start; until the supervisor writes it,
/// and while the agent starts or works, the wait reads again; a failed or
/// changed session ends it.
#[test]
fn only_the_resumed_runs_ready_record_lets_the_send_start() {
    use ready::{Readiness, READY_ENDED, ROOT_CHANGED};
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let state = state_root(&parent);
    let verified = original(&"a".repeat(64), &uuid::Uuid::new_v4().to_string());
    let plan = &verified.plan;
    let later = || std::time::Instant::now() + std::time::Duration::from_secs(5);
    let never = |_: &[String], _: &[u8], _: std::time::Instant| -> Option<Vec<u8>> {
        panic!("a Claude session is never observed")
    };
    let read = |state: &Path| {
        let roots = receipt::Roots::open(state, receipt::RESUME_FOLDER).unwrap();
        ready::read(&roots, plan, RESUMED, later(), never)
    };
    // The original run's record still: not written yet.
    session_record(&state, SESSION, "claude", RUN, json!({}));
    assert_eq!(read(&state), Readiness::Waiting);
    resumed_record(&state, false);
    assert_eq!(read(&state), Readiness::Waiting);
    resumed_record(&state, true);
    assert_eq!(read(&state), Readiness::Ready);
    // Not yet confirmed by SessionStart.
    let pid = std::process::id();
    let start = started(pid);
    session_record(
        &state,
        SESSION,
        "claude",
        RESUMED,
        json!({"expected_id": CONVERSATION, "activity": "idle", "phase": "idle", "error": "",
            "pid": pid, "process_start": start}),
    );
    assert_eq!(read(&state), Readiness::Waiting);
    // The supervisor failed.
    session_record(
        &state,
        SESSION,
        "claude",
        RESUMED,
        json!({"expected_id": "", "activity": "idle", "phase": "idle",
            "error": "saved binding changed before agent launch", "pid": pid,
            "process_start": start}),
    );
    assert_eq!(read(&state), Readiness::Ended(READY_ENDED));
    // A replaced root ends the wait.
    resumed_record(&state, true);
    let roots = receipt::Roots::open(&state, receipt::RESUME_FOLDER).unwrap();
    std::fs::rename(&state, parent.join("moved")).unwrap();
    std::fs::create_dir(&state).unwrap();
    mode(&state, 0o700);
    assert_eq!(
        ready::read(&roots, plan, RESUMED, later(), never),
        Readiness::Ended(ROOT_CHANGED)
    );
}

/// zerus-5wmh.66.4.10: a resumed Codex pending its first message is ready
/// only when the send's own observation, with exactly its arguments and
/// stdin, says so in time; its record only says whether it ended, and an
/// ended or unwritten record is never observed. Any other reply, none, or
/// a late one keeps waiting; a session that ends during the observation
/// ends the wait whatever the reply.
#[test]
fn a_resumed_codex_is_ready_only_by_the_sends_own_observation() {
    use ready::{Readiness, READY_ENDED};
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let state = state_root(&parent);
    let verified = original_of(&"a".repeat(64), &uuid::Uuid::new_v4().to_string(), CODEX);
    let plan = &verified.plan;
    assert!(plan.observed());
    let roots = receipt::Roots::open(&state, receipt::RESUME_FOLDER).unwrap();
    let later = || Instant::now() + Duration::from_secs(5);
    let asked: RefCell<Vec<(Vec<String>, Value)>> = RefCell::new(Vec::new());
    let read = |reply: Option<&[u8]>, until: Instant| {
        ready::read(&roots, plan, RESUMED, until, |args, input, _| {
            asked
                .borrow_mut()
                .push((args.to_vec(), serde_json::from_slice(input).unwrap()));
            reply.map(<[u8]>::to_vec)
        })
    };
    // Its resumed record is not written yet: nothing is observed.
    session_record(&state, CODEX, "codex", RUN, json!({}));
    assert_eq!(read(Some(OBSERVED_READY), later()), Readiness::Waiting);
    assert!(asked.borrow().is_empty());
    // Pending its first message, the record alone never says ready.
    codex_pending_record(&state, CODEX);
    assert!(ready::current(&roots, plan, RESUMED));
    assert_eq!(read(Some(OBSERVED_READY), later()), Readiness::Ready);
    assert_eq!(
        asked.borrow().as_slice(),
        [(
            vec![
                "send".to_owned(),
                CODEX.to_owned(),
                "--observe".to_owned(),
                "--json".to_owned()
            ],
            json!({"expected_run_id": RESUMED, "expected_conversation_id": CONVERSATION})
        )]
    );
    let mut oversized = OBSERVED_READY.to_vec();
    oversized.resize(ready::OBSERVATION_BYTES + 1, b' ');
    for reply in [
        Some(OBSERVED_NOT_READY),
        None,
        Some(&b"not json"[..]),
        Some(&br#"{"ready":true}"#[..]),
        Some(&br#"{"ready":true,"reason":"","more":1}"#[..]),
        Some(&oversized[..]),
    ] {
        assert_eq!(read(reply, later()), Readiness::Waiting, "{reply:?}");
    }
    // A reply after its deadline never counts.
    let past = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .unwrap();
    assert_eq!(read(Some(OBSERVED_READY), past), Readiness::Waiting);
    // A session that ends during the observation ends the wait.
    let ended = ready::read(&roots, plan, RESUMED, later(), |_, _, _| {
        let pid = std::process::id();
        session_record(
            &state,
            CODEX,
            "codex",
            RESUMED,
            json!({"error": "saved binding changed before agent launch", "pid": pid,
                "process_start": started(pid)}),
        );
        Some(OBSERVED_READY.to_vec())
    });
    assert_eq!(ended, Readiness::Ended(READY_ENDED));
    // An ended record is never observed, and never current.
    asked.borrow_mut().clear();
    assert_eq!(
        read(Some(OBSERVED_READY), later()),
        Readiness::Ended(READY_ENDED)
    );
    assert!(asked.borrow().is_empty());
    assert!(!ready::current(&roots, plan, RESUMED));
}

#[test]
fn the_lookup_reads_only_its_exact_operation() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let row = admit(&mut store, &verified, &uuid::Uuid::new_v4().to_string());
    let scope = verified.intent.scope.clone();
    let asked = wire::Query::for_plan(scope.clone(), &verified.plan).unwrap();
    let value = query(
        &store,
        &json!({"op": "session_resume_result", "query": asked}),
    );
    let lookup: wire::Lookup = serde_json::from_value(value).unwrap();
    assert!(lookup.matches(&asked) && lookup.found);
    assert_eq!(lookup.receipt.unwrap(), row.receipt);
    // Another plan under this request, another epoch or another op answers
    // nothing.
    let unavailable =
        json!({"accepted":false,"read_only":true,"code":"session_resume_result_unavailable"});
    let mut other = asked.clone();
    other.plan_sha256 = "f".repeat(64);
    assert_eq!(
        query(
            &store,
            &json!({"op": "session_resume_result", "query": other})
        ),
        unavailable
    );
    let mut epoch_changed = asked.clone();
    epoch_changed.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        query(
            &store,
            &json!({"op": "session_resume_result", "query": epoch_changed})
        ),
        unavailable
    );
    assert_eq!(
        query(
            &store,
            &json!({"op": "compact_to_send_result", "query": asked})
        ),
        unavailable
    );
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

    struct Fixture {
        _temp: tempfile::TempDir,
        store: Store,
        runtime: runtime::Runtime,
        exclusion: Arc<crate::desktop::store::HandoffLock>,
        location: Location,
        epoch: String,
        state: PathBuf,
        config: Config,
        owner: Arc<crate::platform::FileLock>,
        /// The epoch's sealed negatives: every runtime of this epoch shares them.
        fences: Arc<crate::desktop::not_admitted::Fences>,
    }
    impl Fixture {
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
            // The stopped session at the original run, before its resume.
            session_record(&config.state, SESSION, "claude", RUN, json!({}));
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
                state: config.state.clone(),
                config,
                owner,
                fences,
            }
        }
        fn install(runtime: &mut runtime::Runtime, state: &Path, execute: Option<Arc<Execute>>) {
            let state = state.to_path_buf();
            match execute {
                Some(execute) => runtime.fixture_executor(
                    MACHINE,
                    Arc::new(move |plan, args, input, lock, resources| {
                        execute(&state, plan, args, input, lock, resources)
                    }),
                ),
                None => runtime.fixture_machine(MACHINE),
            }
            runtime.fixture_mac_fence(false);
        }
        /// Destroys the runtime and builds it again under the same epoch,
        /// as the service does when it restarts its background runtime.
        fn rebuild(&mut self, execute: Arc<Execute>) {
            let torn = std::mem::replace(
                &mut self.runtime,
                runtime::Runtime::new(
                    &self.config,
                    &self.epoch,
                    self.owner.clone(),
                    self.fences.clone(),
                )
                .unwrap(),
            );
            drop(torn);
            Self::install(&mut self.runtime, &self.state, Some(execute));
            self.runtime
                .poll(&mut self.store, Some(self.exclusion.clone()));
        }
        fn original(&self) -> Verified {
            super::original(&self.location.configuration, &self.epoch)
        }
        /// A Codex original, its session stopped at the original run.
        fn codex(&self) -> Verified {
            session_record(&self.state, CODEX, "codex", RUN, json!({}));
            super::original_of(&self.location.configuration, &self.epoch, CODEX)
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            let request = json!(
                zerus_client_core::desktop::ui_requests::Request::SessionResume {
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
            ledger::load(&self.store.db, &original.plan.resume_request_id)
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

    /// Counts each child the executor ran, by its command.
    #[derive(Default)]
    struct Ran {
        resumes: AtomicUsize,
        sends: AtomicUsize,
    }
    impl Ran {
        fn counts(&self) -> (usize, usize) {
            (
                self.resumes.load(Ordering::SeqCst),
                self.sends.load(Ordering::SeqCst),
            )
        }
    }
    /// How the executor's resume ends.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Resume {
        /// `completed`; the resumed record is ready (or busy, `false`).
        Completed {
            ready: bool,
        },
        Failed,
        Uncertain,
        /// No receipt at all.
        Silent,
    }
    /// An executor whose resume writes the CLI's own action receipt and the
    /// resumed record as `resume` says, and whose send writes its exact
    /// input receipt.
    fn executor(ran: Arc<Ran>, resume: Resume) -> Arc<Execute> {
        Arc::new(move |state, plan, args, input, _, _| {
            let digest = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(input));
            match args[0].as_str() {
                "session-action" => {
                    ran.resumes.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(args, ["session-action", &plan.session, "--json"]);
                    assert_eq!(digest, plan.resume_sha256);
                    match resume {
                        Resume::Completed { ready } => {
                            action_receipt(state, plan, input, "completed");
                            resumed_record_of(state, &plan.session, ready);
                        }
                        Resume::Failed => {
                            action_receipt(state, plan, input, "failed");
                        }
                        Resume::Uncertain => {
                            action_receipt(state, plan, input, "uncertain");
                        }
                        Resume::Silent => {}
                    }
                }
                "send" => {
                    ran.sends.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(args, ["send", &plan.session, "--json"]);
                    let stdin: Value = serde_json::from_slice(input).unwrap();
                    assert_eq!(stdin["expected_run_id"], RESUMED);
                    assert_eq!(stdin["expected_conversation_id"], CONVERSATION);
                    assert_eq!(stdin["request_id"], plan.send_request_id.as_str());
                    assert!(stdin.get("expected_compaction_id").is_none());
                    sent_receipt(state, plan, input);
                }
                other => panic!("unexpected child {other}"),
            }
            Ended::Exited { code: 0 }
        })
    }

    #[test]
    fn a_recorded_resume_sends_once_bound_to_the_resumed_run_and_settles_from_both_receipts() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        let original = f.original();
        let admission = f.admit(&original);
        assert!(admission.accepted && admission.durable && admission.matches(&original.intent));
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(row.receipt.resume.step, Step::Recorded);
        assert_eq!(row.receipt.send.step, Step::Recorded);
        assert!(row.resume.is_some() && row.send.is_some());
        // The send's outcome is exactly the template bound to the resume.
        let (_, _, digest) = row.bound().unwrap();
        let sent = row.receipt.send.outcome.clone().unwrap();
        assert_eq!(sent.input_sha256, digest);
        assert_eq!(
            row.receipt.result(&original.plan),
            Some(Settled::Sent(sent))
        );
        assert!(row.receipt.matches(&original.intent));
        // The same operation admitted again returns its settled receipt.
        assert_eq!(f.admit(&original).receipt.unwrap(), row.receipt);
        f.runtime.request_stop();
        f.until("stopped", |f| f.runtime.drained());
        assert_eq!(ran.counts(), (1, 1), "each child once");
    }

    /// [`executor`] for a Codex: its resume leaves the resumed agent pending
    /// its first message, which its record never shows ready.
    fn codex_executor(ran: Arc<Ran>) -> Arc<Execute> {
        let inner = executor(ran, Resume::Completed { ready: false });
        Arc::new(move |state, plan, args, input, lock, resources| {
            let ended = inner(state, plan, args, input, lock, resources);
            if args[0] == "session-action" {
                codex_pending_record(state, &plan.session);
            }
            ended
        })
    }
    type Observe = Arc<dyn Fn(&[String], &[u8]) -> Option<Vec<u8>> + Send + Sync>;

    /// zerus-5wmh.66.4.10: a resumed Codex's send starts only once the
    /// send's own observation, with exactly its arguments and stdin, says the
    /// agent takes it: the wait reads again until then, and the observation
    /// runs once more right before the send. A Claude session is never
    /// observed.
    #[test]
    fn a_resumed_codex_sends_only_once_its_observation_says_ready() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(codex_executor(ran.clone()));
        let observed = Arc::new(AtomicUsize::new(0));
        let count = observed.clone();
        let observer: Observe = Arc::new(move |args, input| {
            assert_eq!(args, ["send", CODEX, "--observe", "--json"]);
            assert_eq!(
                serde_json::from_slice::<Value>(input).unwrap(),
                json!({"expected_run_id": RESUMED, "expected_conversation_id": CONVERSATION})
            );
            let n = count.fetch_add(1, Ordering::SeqCst);
            Some(
                if n < 3 {
                    OBSERVED_NOT_READY
                } else {
                    OBSERVED_READY
                }
                .to_vec(),
            )
        });
        f.runtime
            .fixture_observer(Some(observer), Duration::from_secs(5));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.codex();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(
            (row.receipt.resume.step, row.receipt.send.step),
            (Step::Recorded, Step::Recorded)
        );
        assert_eq!(ran.counts(), (1, 1));
        // Three not ready, the ready one in the wait, then the one right
        // before the send.
        assert_eq!(observed.load(Ordering::SeqCst), 5);

        // A Claude session's readiness is its record alone.
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        let observed = Arc::new(AtomicUsize::new(0));
        let count = observed.clone();
        f.runtime.fixture_observer(
            Some(Arc::new(move |_: &[String], _: &[u8]| {
                count.fetch_add(1, Ordering::SeqCst);
                Some(OBSERVED_READY.to_vec())
            })),
            Duration::from_secs(5),
        );
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.send.step, Step::Recorded);
        assert_eq!(ran.counts(), (1, 1));
        assert_eq!(observed.load(Ordering::SeqCst), 0, "never observed");
    }

    /// A resumed Codex whose observation never says it takes the message,
    /// or that is never observed at all, never sends: the wait ends at its
    /// budget and the send was never reserved. One whose observation turns
    /// to not ready right before the send starts no send, which then never
    /// started.
    #[test]
    fn a_resumed_codex_without_a_ready_observation_never_sends() {
        for case in ["never", "unobserved", "before_send"] {
            let ran = Arc::new(Ran::default());
            let mut f = Fixture::with(codex_executor(ran.clone()));
            let observed = Arc::new(AtomicUsize::new(0));
            let count = observed.clone();
            let observer: Option<Observe> = (case != "unobserved").then(|| {
                let observer: Observe = Arc::new(move |_, _| {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    let ready = case == "before_send" && n == 0;
                    Some(
                        if ready {
                            OBSERVED_READY
                        } else {
                            OBSERVED_NOT_READY
                        }
                        .to_vec(),
                    )
                });
                observer
            });
            f.runtime.fixture_observer(observer, Duration::from_secs(5));
            f.runtime.fixture_wait(
                Duration::from_millis(80),
                Duration::from_millis(5),
                runtime::CHAIN,
            );
            let original = f.codex();
            f.admit(&original);
            f.until(case, |f| f.row(&original).receipt.drained);
            let row = f.row(&original);
            assert_eq!(row.receipt.resume.step, Step::Recorded, "{case}");
            if case == "before_send" {
                assert!(row.send.is_some(), "{case}: reserved before its turn");
                assert_eq!(
                    (row.receipt.send.step, row.receipt.send.code.as_str()),
                    (Step::NotStarted, wire::NOT_STARTED),
                    "{case}"
                );
                assert_eq!(observed.load(Ordering::SeqCst), 2, "{case}");
            } else {
                assert!(row.send.is_none(), "{case}: never reserved");
                assert_eq!(
                    (row.receipt.send.step, row.receipt.code.as_str()),
                    (Step::NotStarted, ledger::WAIT_EXPIRED),
                    "{case}"
                );
            }
            assert_eq!(
                row.receipt.result(&original.plan),
                Some(Settled::NotSent {
                    resume: Step::Recorded
                }),
                "{case}"
            );
            assert_eq!(ran.counts(), (1, 0), "{case}");
        }
    }

    /// A refused, uncertain or silent resume never sends: without the
    /// resume's own exact receipt there is no resumed run to bind.
    #[test]
    fn a_resume_that_is_not_recorded_never_sends() {
        for (resume, step, code) in [
            (Resume::Failed, Step::Failed, wire::REFUSED),
            (Resume::Uncertain, Step::Unknown, wire::RECEIPT_UNCERTAIN),
            (Resume::Silent, Step::Unknown, receipt::MISSING),
        ] {
            let ran = Arc::new(Ran::default());
            let mut f = Fixture::with(executor(ran.clone(), resume));
            let original = f.original();
            f.admit(&original);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let row = f.row(&original);
            assert_eq!(
                (row.receipt.resume.step, row.receipt.resume.code.as_str()),
                (step, code),
                "{resume:?}"
            );
            assert_eq!(row.receipt.send.step, Step::NotStarted, "{resume:?}");
            assert!(row.send.is_none(), "{resume:?}: never reserved");
            assert_eq!(
                row.receipt.result(&original.plan),
                Some(Settled::NotSent { resume: step }),
                "{resume:?}"
            );
            assert_eq!(ran.counts(), (1, 0), "{resume:?}");
        }
    }

    /// The wait ends without a send when the resumed agent never becomes
    /// ready within the budget, when its session ends, when the operation
    /// is revoked, or at the operation's deadline. The send was never
    /// reserved, so the ledger proves it never started.
    #[test]
    fn the_wait_ends_without_a_send_for_its_closed_reason() {
        type Case = (&'static str, bool, Duration, &'static str);
        let cases: [Case; 4] = [
            ("busy", false, Duration::from_secs(10), ledger::WAIT_EXPIRED),
            ("failed", true, Duration::from_secs(10), ready::READY_ENDED),
            ("revoked", false, Duration::from_secs(10), ledger::VETOED),
            (
                "chain",
                false,
                Duration::from_millis(150),
                ledger::CHAIN_EXPIRED,
            ),
        ];
        for (name, ready_record, chain, code) in cases {
            let ran = Arc::new(Ran::default());
            let inner = executor(
                ran.clone(),
                Resume::Completed {
                    ready: ready_record,
                },
            );
            let mut f = Fixture::with(Arc::new(
                move |state, plan, args, input, lock, resources| {
                    let ended = inner(state, plan, args, input, lock, resources);
                    if name == "failed" {
                        let pid = std::process::id();
                        session_record(
                            state,
                            SESSION,
                            "claude",
                            RESUMED,
                            json!({"expected_id": "", "activity": "idle", "phase": "idle",
                            "error": "saved binding changed before agent launch",
                            "pid": pid, "process_start": started(pid)}),
                        );
                    }
                    ended
                },
            ));
            let wait = if name == "busy" {
                Duration::from_millis(80)
            } else {
                Duration::from_secs(5)
            };
            f.runtime
                .fixture_wait(wait, Duration::from_millis(5), chain);
            let original = f.original();
            f.admit(&original);
            f.until(name, |f| {
                let row = f.row(&original);
                row.receipt.resume.step == Step::Recorded || row.receipt.drained
            });
            if name == "revoked" {
                f.revoke();
            }
            f.until(name, |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(receipt.resume.step, Step::Recorded, "{name}");
            assert_eq!(
                (receipt.send.step, receipt.code.as_str()),
                (Step::NotStarted, code),
                "{name}"
            );
            assert_eq!(
                receipt.result(&original.plan),
                Some(Settled::NotSent {
                    resume: Step::Recorded
                }),
                "{name}"
            );
            assert!(f.row(&original).send.is_none(), "{name}: never reserved");
            assert_eq!(ran.counts(), (1, 0), "{name}");
        }
    }

    /// The send starts only once the resumed run's record is ready: the
    /// supervisor's record appearing later, or the agent becoming idle
    /// later, still sends within the same live operation.
    #[test]
    fn the_send_waits_for_the_resumed_agent_to_be_ready() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: false }));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.original();
        f.admit(&original);
        f.until("resumed", |f| {
            f.row(&original).receipt.resume.step == Step::Recorded
        });
        for _ in 0..30 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(ran.counts(), (1, 0), "no send while the agent is busy");
        assert_eq!(f.row(&original).receipt.send.step, Step::Queued);
        resumed_record(&f.state, true);
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.send.step, Step::Recorded);
        assert_eq!(ran.counts(), (1, 1));
    }

    /// A restart never continues a wait or starts the send: the operation
    /// it finds is retired, its send recorded as never started.
    #[test]
    fn a_restart_during_the_wait_never_sends() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: false }));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.original();
        f.admit(&original);
        f.until("resumed", |f| {
            f.row(&original).receipt.resume.step == Step::Recorded
        });
        // The service restarts: a new runtime, the store reopened.
        f.runtime.request_stop();
        f.until("stopped", |f| {
            f.runtime.drained() || f.row(&original).receipt.drained
        });
        ledger::startup(&mut f.store.db).unwrap();
        let receipt = f.row(&original).receipt;
        assert!(receipt.drained);
        assert_eq!(receipt.resume.step, Step::Recorded);
        assert_eq!(receipt.send.step, Step::NotStarted);
        assert!(f.row(&original).send.is_none());
        resumed_record(&f.state, true);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(ran.counts(), (1, 0));
    }

    /// macOS refuses every original before any row or child.
    #[test]
    fn the_mac_fence_refuses_before_any_row() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        f.runtime.fixture_mac_fence(true);
        let original = f.original();
        let admission = f.admit(&original);
        assert!(!admission.accepted && !admission.durable && admission.receipt.is_none());
        assert_eq!(admission.code, runtime::MAC_UNAVAILABLE);
        assert!(ledger::load(&f.store.db, &original.plan.resume_request_id)
            .unwrap()
            .is_none());
        assert_eq!(ran.counts(), (0, 0));
    }

    /// zerus-qw7wp's clock, shared through `operation_clock`: a second
    /// operation queued behind another's wait keeps its whole bound, counted
    /// from its own first turn.
    #[test]
    fn a_second_operation_queued_behind_a_wait_still_runs() {
        let ran = Arc::new(Ran::default());
        let first_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = first_done.clone();
        let inner = executor(ran.clone(), Resume::Completed { ready: true });
        let mut f = Fixture::with(Arc::new(
            move |state, plan, args, input, lock, resources| {
                // The first operation's resumed agent never becomes ready.
                if args[0] == "session-action" && plan.session == SESSION {
                    gate.store(true, Ordering::SeqCst);
                    ran_busy(state, plan, input);
                    return Ended::Exited { code: 0 };
                }
                inner(state, plan, args, input, lock, resources)
            },
        ));
        // Keep the real operation bound: only the first's deliberately
        // unfinished wait is short. The second's resume and send are real
        // child I/O that a loaded runner can stretch past a tight bound, so
        // the clocks are observed directly instead (zerus-c4q4p, as
        // zerus-mupq4 for compact-to-send).
        f.runtime.fixture_wait(
            Duration::from_millis(300),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        // Two stopped sessions: one resume each. The queue runs by admission
        // millisecond, then request ID: with the first's ID lower, a tie
        // between the two admissions still runs the first first
        // (review-win-e on #1750, as zerus-mupq4 for compact-to-send).
        let other = "claude/private-second";
        session_record(&f.state, other, "claude", RUN, json!({}));
        let (first, second) = loop {
            let first = f.original();
            let second = original_of(&f.location.configuration, &f.epoch, other);
            if first.plan.resume_request_id < second.plan.resume_request_id {
                break (first, second);
            }
        };
        f.admit(&first);
        let admitted = Instant::now();
        f.admit(&second);
        let started = |f: &Fixture, original: &Verified| {
            f.runtime
                .fixture_chain_started(&original.plan.resume_request_id)
        };
        assert_eq!(
            started(&f, &first),
            Some(None),
            "admission must not start the first operation's clock"
        );
        assert_eq!(
            started(&f, &second),
            Some(None),
            "admission must not start the queued operation's clock"
        );
        f.until("first holds the waiting work slot", |f| {
            f.row(&first).receipt.resume.step == Step::Recorded && !f.row(&first).receipt.drained
        });
        assert!(
            started(&f, &first).flatten().is_some(),
            "the first turn must start the first operation's clock"
        );
        assert_eq!(
            started(&f, &second),
            Some(None),
            "another operation's wait must not start the queued clock"
        );
        assert_eq!(f.row(&second).receipt.resume.step, Step::Queued);
        f.until("both settled", |f| {
            f.row(&first).receipt.drained && f.row(&second).receipt.drained
        });
        assert!(Instant::now() > admitted + Duration::from_millis(300));
        // The first ends unsent at its own wait budget; the second, queued
        // behind it, still resumes and sends within its own bound.
        let first_receipt = f.row(&first).receipt;
        assert_eq!(first_receipt.code, ledger::WAIT_EXPIRED);
        assert_eq!(
            first_receipt.result(&first.plan),
            Some(Settled::NotSent {
                resume: Step::Recorded
            })
        );
        assert_eq!(f.row(&second).receipt.send.step, Step::Recorded);
        assert!(first_done.load(Ordering::SeqCst));
    }
    /// The first operation's resume: recorded, its agent busy.
    fn ran_busy(state: &Path, plan: &wire::Plan, input: &[u8]) {
        action_receipt(state, plan, input, "completed");
        resumed_record_of(state, &plan.session, false);
    }

    /// A pinned stand-in image for `hgs`: its resume copies the prepared
    /// exact action receipt and writes the resumed run's ready record, after
    /// marking its custody descriptor close-on-exec as `hgs` does first and
    /// starting a daemon the way tmux starts its server; its send writes its
    /// exact input receipt, or only the `sent` marker.
    #[cfg(target_os = "linux")]
    fn image(path: &Path, state: &Path, dir: &Path, receipt: bool) {
        let pid = std::process::id();
        std::fs::write(
            path,
            format!(
                r#"#!/usr/bin/env python3
import fcntl,hashlib,json,os,subprocess,sys,time
data=sys.stdin.buffer.read()
request=json.loads(data)
state={state:?}
dir={dir:?}
custody=os.environ.get("HGS_SESSION_RESUME_CUSTODY_FD")
with open(os.path.join(dir,"seen"),"a") as log:
    log.write(json.dumps({{"argv":sys.argv[1:],"stdin":data.decode(),"custody":custody}})+"\n")
# As hgs does first: its custody lock lives only as long as this child.
fd=int(custody)
fcntl.fcntl(fd,fcntl.F_SETFD,fcntl.fcntl(fd,fcntl.F_GETFD)|fcntl.FD_CLOEXEC)
def write(path,value):
    fd=os.open(path+".tmp",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    os.write(fd,value)
    os.close(fd)
    os.rename(path+".tmp",path)
name=sys.argv[2]
if sys.argv[1]=="session-action":
    # A server that outlives this child, as tmux's does: it inherits this
    # child's output, then detaches and closes it.
    daemon="import os,sys,time\nif os.fork():\n    os._exit(0)\nos.setsid()\nnull=os.open('/dev/null',os.O_RDWR)\nfor n in (0,1,2):\n    os.dup2(null,n)\nopen(sys.argv[1],'w').write(' '.join(os.listdir('/proc/self/fd')))\ntime.sleep(3)\n"
    subprocess.run([sys.executable,"-c",daemon,os.path.join(dir,"daemon-fds"),custody])
    write(os.path.join(state,"action_receipts",request["request_id"]+".json"),open(os.path.join(dir,"receipt"),"rb").read())
    record={{"name":name,"agent":"claude","run_id":"{resumed}","conversation_id":request["expected_conversation_id"],"expected_id":"","activity":"idle","phase":"idle","error":"","pid":{pid},"process_start":open(os.path.join(dir,"start")).read()}}
    write(os.path.join(state,hashlib.sha256(name.encode()).hexdigest()+".json"),json.dumps(record).encode())
elif {receipt}:
    write(os.path.join(state,"input_receipts",request["request_id"]+".json"),json.dumps({{"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"digest":hashlib.sha256(data).hexdigest(),"status":"submitted","transport":"terminal"}}).encode())
else:
    open(os.path.join(dir,"sent"),"w").close()
"#,
                state = state.to_str().unwrap(),
                dir = dir.to_str().unwrap(),
                pid = pid,
                resumed = RESUMED,
                receipt = if receipt { "True" } else { "False" },
            ),
        )
        .unwrap();
        mode(path, 0o700);
        std::fs::write(dir.join("start"), started(pid)).unwrap();
    }
    /// Prepares the pinned image's exact action receipt for `original`.
    #[cfg(target_os = "linux")]
    fn prepared(f: &Fixture, dir: &Path, original: &Verified) {
        prepared_of(f, dir, original, SESSION)
    }
    /// [`prepared`] for the session `session`.
    #[cfg(target_os = "linux")]
    fn prepared_of(f: &Fixture, dir: &Path, original: &Verified, session: &str) {
        for folder in [receipt::RESUME_FOLDER, receipt::INPUT_FOLDER] {
            std::fs::create_dir(f.state.join(folder)).unwrap();
            mode(&f.state.join(folder), 0o700);
        }
        let value = crate::state::lifecycle_fixture_receipt(
            session,
            original.intent.resume.as_bytes(),
            "completed",
            Some(RESUMED),
        );
        std::fs::write(dir.join("receipt"), value.to_string()).unwrap();
    }

    /// A pinned stand-in for `hgs` and a resumed Codex: its resume copies the
    /// prepared action receipt and leaves the resumed agent pending its first
    /// message; its observation (`send <session> --observe --json`) answers
    /// in turn from `replies` (the last repeating) without custody or any
    /// state write; its send writes its exact input receipt. Each call logs
    /// its arguments, stdin, custody variable and whether it leads its own
    /// process group.
    #[cfg(target_os = "linux")]
    fn codex_image(path: &Path, state: &Path, dir: &Path, replies: &[&[u8]]) {
        let pid = std::process::id();
        let replies: Vec<Value> = replies
            .iter()
            .map(|reply| serde_json::from_slice(reply).unwrap())
            .collect();
        std::fs::write(dir.join("replies"), json!(replies).to_string()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"#!/usr/bin/env python3
import fcntl,hashlib,json,os,sys
data=sys.stdin.buffer.read()
request=json.loads(data)
state={state:?}
dir={dir:?}
argv=sys.argv[1:]
custody=os.environ.get("HGS_SESSION_RESUME_CUSTODY_FD")
with open(os.path.join(dir,"seen"),"a") as log:
    log.write(json.dumps({{"argv":argv,"stdin":data.decode(),"custody":custody,"leader":os.getpgid(0)==os.getpid()}})+"\n")
name=argv[1]
if argv[2:]==["--observe","--json"]:
    replies=json.load(open(os.path.join(dir,"replies")))
    count=os.path.join(dir,"observed")
    n=int(open(count).read()) if os.path.exists(count) else 0
    open(count,"w").write(str(n+1))
    print(json.dumps(replies[min(n,len(replies)-1)]))
    sys.exit(0)
fd=int(custody)
fcntl.fcntl(fd,fcntl.F_SETFD,fcntl.fcntl(fd,fcntl.F_GETFD)|fcntl.FD_CLOEXEC)
def write(path,value):
    fd=os.open(path+".tmp",os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    os.write(fd,value)
    os.close(fd)
    os.rename(path+".tmp",path)
if argv[0]=="session-action":
    write(os.path.join(state,"action_receipts",request["request_id"]+".json"),open(os.path.join(dir,"receipt"),"rb").read())
    conversation=request["expected_conversation_id"]
    record={{"name":name,"agent":"codex","run_id":"{resumed}","conversation_id":conversation,"run_identity_version":1,"startup_kind":"resume","supervisor":{{}},"expected_id":conversation,"activity":"unknown","phase":"unknown","error":"","last_event_at":0,"pid":{pid},"process_start":open(os.path.join(dir,"start")).read()}}
    write(os.path.join(state,hashlib.sha256(name.encode()).hexdigest()+".json"),json.dumps(record).encode())
else:
    write(os.path.join(state,"input_receipts",request["request_id"]+".json"),json.dumps({{"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"digest":hashlib.sha256(data).hexdigest(),"status":"submitted","transport":"terminal"}}).encode())
"#,
                state = state.to_str().unwrap(),
                dir = dir.to_str().unwrap(),
                pid = pid,
                resumed = RESUMED,
            ),
        )
        .unwrap();
        mode(path, 0o700);
        std::fs::write(dir.join("start"), started(pid)).unwrap();
    }

    /// zerus-5wmh.66.4.10: the pinned image observes a resumed Codex as its
    /// own child, leading its own process group, with exactly the
    /// observation's stdin and no custody, and the send follows only a ready
    /// reply right before it; one that turns to not ready there starts no
    /// send, which then never started.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_image_observes_a_resumed_codex_without_custody_before_its_send() {
        for (replies, sends) in [
            (vec![OBSERVED_NOT_READY, OBSERVED_READY], true),
            (vec![OBSERVED_READY, OBSERVED_NOT_READY], false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let mut f = Fixture::unexecuted();
            codex_image(&path, &f.state, dir.path(), &replies);
            f.runtime.fixture_image(&path);
            f.runtime.fixture_wait(
                Duration::from_secs(5),
                Duration::from_millis(5),
                runtime::CHAIN,
            );
            let original = f.codex();
            prepared_of(&f, dir.path(), &original, CODEX);
            assert!(f.admit(&original).accepted);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let row = f.row(&original);
            let seen: Vec<Value> = std::fs::read_to_string(dir.path().join("seen"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let (observations, originals): (Vec<&Value>, Vec<&Value>) =
                seen.iter().partition(|call| call["argv"][2] == "--observe");
            for call in &observations {
                assert_eq!(call["argv"], json!(["send", CODEX, "--observe", "--json"]));
                assert_eq!(
                    serde_json::from_str::<Value>(call["stdin"].as_str().unwrap()).unwrap(),
                    json!({"expected_run_id": RESUMED, "expected_conversation_id": CONVERSATION})
                );
                assert_eq!(
                    call["custody"],
                    Value::Null,
                    "an observation holds no custody"
                );
                assert_eq!(call["leader"], true, "its own process group");
            }
            assert!(originals.iter().all(|call| call["custody"].is_string()));
            assert_eq!(row.receipt.resume.step, Step::Recorded);
            if sends {
                // Not ready, ready in the wait, ready right before the send.
                assert_eq!(observations.len(), 3);
                assert_eq!(row.receipt.send.step, Step::Recorded);
                assert_eq!(originals.len(), 2);
            } else {
                assert_eq!(observations.len(), 2);
                assert_eq!(
                    (row.receipt.send.step, row.receipt.send.code.as_str()),
                    (Step::NotStarted, wire::NOT_STARTED)
                );
                assert_eq!(originals.len(), 1, "only the resume ran");
            }
        }
    }

    /// The pinned image runs the resume with its exact stdin, then the send
    /// with exactly the template bound to the resumed run, each holding its
    /// own custody. The resume's drain settles while the server it started
    /// still runs, and that server never holds the custody lock.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_image_runs_both_originals_and_a_detached_server_holds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hgs");
        let mut f = Fixture::unexecuted();
        image(&path, &f.state, dir.path(), true);
        f.runtime.fixture_image(&path);
        let original = f.original();
        prepared(&f, dir.path(), &original);
        assert!(f.admit(&original).accepted);
        let started_at = Instant::now();
        f.until("resumed", |f| {
            f.row(&original).receipt.resume.step == Step::Recorded
        });
        // The detached server sleeps 3 s: the resume drained without it.
        assert!(started_at.elapsed() < Duration::from_secs(3));
        f.until("settled", |f| f.row(&original).receipt.drained);
        let row = f.row(&original);
        assert_eq!(row.receipt.send.step, Step::Recorded);
        let (_, bound, _) = row.bound().unwrap();
        let seen: Vec<Value> = std::fs::read_to_string(dir.path().join("seen"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0]["argv"],
            json!(["session-action", SESSION, "--json"])
        );
        assert_eq!(seen[0]["stdin"], original.intent.resume.as_str());
        assert_eq!(seen[1]["argv"], json!(["send", SESSION, "--json"]));
        assert_eq!(seen[1]["stdin"], bound.as_str());
        for child in &seen {
            assert!(child["custody"]
                .as_str()
                .is_some_and(|fd| fd.parse::<u32>().is_ok()));
        }
        let custody = seen[0]["custody"].as_str().unwrap();
        let fds = std::fs::read_to_string(dir.path().join("daemon-fds")).unwrap();
        assert!(
            !fds.split_whitespace().any(|fd| fd == custody),
            "the server holds the custody descriptor {custody}: {fds}"
        );
    }

    /// The send's final spawn turn re-proves the resumed run's record and
    /// its readiness from the retained roots, after every earlier read. A
    /// record that changed only there starts no send, which then never
    /// started; the resume stays recorded.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_send_reproves_its_target_and_readiness_in_the_final_turn() {
        for change in ["target", "busy"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hgs");
            let mut f = Fixture::unexecuted();
            image(&path, &f.state, dir.path(), false);
            f.runtime.fixture_image(&path);
            let original = f.original();
            prepared(&f, dir.path(), &original);
            let turns = Arc::new(AtomicUsize::new(0));
            let count = turns.clone();
            let state = f.state.clone();
            f.runtime.fixture_spawn_hook(Arc::new(move || {
                // The second spawn turn is the send's.
                if count.fetch_add(1, Ordering::SeqCst) == 1 {
                    match change {
                        "target" => session_record(&state, SESSION, "claude", "run-3", json!({})),
                        _ => resumed_record(&state, false),
                    }
                }
            }));
            assert!(f.admit(&original).accepted);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let row = f.row(&original);
            assert!(row.send.is_some(), "{change}: reserved before its turn");
            assert_eq!(turns.load(Ordering::SeqCst), 2, "{change}");
            assert_eq!(row.receipt.resume.step, Step::Recorded, "{change}");
            assert_eq!(
                (row.receipt.send.step, row.receipt.send.code.as_str()),
                (Step::NotStarted, wire::NOT_STARTED),
                "{change}"
            );
            assert_eq!(
                row.receipt.result(&original.plan),
                Some(Settled::NotSent {
                    resume: Step::Recorded
                }),
                "{change}"
            );
            assert!(!dir.path().join("sent").exists(), "{change}: never sent");
        }
    }

    /// A stop, a revoke, a changed source or a changed original record after
    /// the resume's reservation committed starts nothing: the target and
    /// consent vetoes run last in the spawn turn, and with no record of the
    /// request both children never started.
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
            assert_eq!(
                f.row(&original).receipt.resume.step,
                Step::Running,
                "{action}"
            );
            match action {
                "stop" => f.runtime.request_stop(),
                "revoke" => f.revoke(),
                "source" => f
                    .runtime
                    .fixture_machine("33333333-3333-4333-8333-333333333333"),
                _ => session_record(&f.state, SESSION, "claude", "run-2", json!({})),
            }
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            release.send(()).unwrap();
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.resume.step, receipt.send.step),
                (Step::NotStarted, Step::NotStarted),
                "{action}"
            );
            assert!(!ran.exists(), "{action}: no child ever existed");
        }
    }

    /// A receipt that changes between its read and its recheck is Unknown,
    /// never the record it was: the resume may have launched, so nothing is
    /// sent.
    #[test]
    fn a_receipt_changed_around_its_read_is_unknown() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        let state = f.state.clone();
        let original = f.original();
        let plan = original.plan.clone();
        let stdin = original.intent.resume.clone();
        f.runtime.fixture_read(
            Some(Arc::new(move || {
                action_receipt(&state, &plan, stdin.as_bytes(), "failed");
            })),
            runtime::READ_WAIT,
        );
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.resume.step, receipt.resume.code.as_str()),
            (Step::Unknown, receipt::CHANGED)
        );
        assert_eq!(receipt.send.step, Step::NotStarted);
        assert_eq!(ran.counts(), (1, 0));
    }

    /// A stop that arrives while a readiness read runs ends the wait, vetoed,
    /// once that bounded read finishes: the send never starts, however ready
    /// the agent became meanwhile.
    #[test]
    fn a_stop_during_a_readiness_read_ends_the_wait_without_a_send() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: false }));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let entered_tx = std::sync::Mutex::new(entered_tx);
        let released = std::sync::Mutex::new(released);
        let state = f.state.clone();
        f.runtime.fixture_ready_hook(Arc::new(move || {
            resumed_record(&state, true);
            let _ = entered_tx.lock().unwrap().send(());
            let _ = released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10));
        }));
        let original = f.original();
        f.admit(&original);
        let until = Instant::now() + Duration::from_secs(8);
        while entered.try_recv().is_err() {
            assert!(Instant::now() < until, "never read");
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        f.runtime.request_stop();
        release.send(()).unwrap();
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.resume.step, Step::Recorded);
        assert_eq!(
            (receipt.send.step, receipt.code.as_str()),
            (Step::NotStarted, ledger::VETOED)
        );
        assert_eq!(ran.counts(), (1, 0));
    }

    /// review-win-e on #1750: a readiness read that ends after the wait's
    /// own budget, however it began, starts nothing: the send never starts
    /// past the budget, even when the agent became ready meanwhile.
    #[test]
    fn a_ready_read_that_ends_after_the_budget_never_sends() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: false }));
        f.runtime.fixture_wait(
            Duration::from_millis(500),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let state = f.state.clone();
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        f.runtime.fixture_ready_hook(Arc::new(move || {
            // The first read outlives the budget, and only then finds the
            // agent ready.
            if first.swap(false, Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(700));
                resumed_record(&state, true);
            }
        }));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.resume.step, Step::Recorded);
        assert_eq!(
            (receipt.send.step, receipt.code.as_str()),
            (Step::NotStarted, ledger::WAIT_EXPIRED)
        );
        assert_eq!(ran.counts(), (1, 0));
    }

    /// A preparation that is lost retires the operation: nothing started.
    #[test]
    fn a_lost_preparation_retires_without_any_child() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        f.runtime
            .fixture_prepare_hook(Arc::new(|| panic!("preparation lost")));
        let original = f.original();
        f.admit(&original);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (
                receipt.resume.step,
                receipt.send.step,
                receipt.code.as_str()
            ),
            (Step::NotStarted, Step::NotStarted, ledger::INTERRUPTED)
        );
        assert_eq!(ran.counts(), (0, 0));
    }

    /// `original`'s request with a resume that no longer verifies: refused
    /// in the writer transaction after its empty read.
    fn broken(original: &Verified) -> Intent {
        let mut bad = original.intent.clone();
        bad.resume = "{}".into();
        bad
    }
    fn offer(f: &mut Fixture, intent: Intent) -> wire::Admission {
        let request = json!(
            zerus_client_core::desktop::ui_requests::Request::SessionResume {
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
    fn ready() -> (Arc<Ran>, Fixture) {
        let ran = Arc::new(Ran::default());
        let f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
        (ran, f)
    }

    /// zerus-tlb9q, desktop-u's pin: a resume refused before its admission
    /// with no row is answered not admitted and sealed for the epoch; an
    /// otherwise admissible copy of the same request that arrives after it
    /// gets the same answer and never runs. The lookup never answers it.
    #[test]
    fn a_resume_refused_before_admission_is_sealed_and_its_copy_never_runs() {
        let (ran, mut f) = ready();
        let original = f.original();
        let id = original.plan.resume_request_id.clone();
        let refused = offer(&mut f, broken(&original));
        assert_eq!(
            (refused.accepted, refused.durable, refused.code.as_str()),
            (false, false, wire::NOT_ADMITTED)
        );
        assert!(refused.not_admitted(&original.intent), "{}", refused.code);
        assert!(no_row(&f, &id) && f.fences.session_resume.sealed(&id));
        let copy = f.admit(&original);
        assert!(copy.not_admitted(&original.intent), "{}", copy.code);
        idle(&mut f);
        assert_eq!(ran.counts(), (0, 0), "the copy never ran");
        assert!(no_row(&f, &id));
        let asked = wire::Query::for_plan(original.intent.scope.clone(), &original.plan).unwrap();
        let lookup = super::super::query(
            &f.store,
            &json!({"op": "session_resume_result", "query": asked}),
        );
        assert_eq!(
            (lookup["found"].as_bool(), lookup.get("code")),
            (Some(false), None)
        );
    }

    /// zerus-5llxc (review-win-d on #1824): another connection's request is
    /// never answered not admitted nor sealed, even when its permit expires
    /// after the empty read or when this epoch sealed its ID.
    #[test]
    fn another_connections_request_is_never_not_admitted() {
        let (_, mut f) = ready();
        let foreign = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let request = json!(
            zerus_client_core::desktop::ui_requests::Request::SessionResume {
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
        assert_eq!(reply["code"], "session_resume_unavailable", "{reply}");
        assert!(!f
            .fences
            .session_resume
            .sealed(&foreign.plan.resume_request_id));
        // This epoch's seal is no answer for another connection's copy.
        let original = f.original();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        let mut intent = original.intent.clone();
        intent.scope.service_epoch = uuid::Uuid::new_v4().to_string();
        let copy = intent.verify().unwrap();
        let reply = f.admit(&copy);
        assert_eq!(reply.code, "session_resume_unavailable");
        assert!(!reply.not_admitted(&copy.intent));
        assert!(no_row(&f, &original.plan.resume_request_id));
    }

    /// The seal belongs to the epoch: the background runtime destroyed and
    /// built again under the same epoch still refuses the sealed request,
    /// and a request of another epoch's connection is never sealed.
    #[test]
    fn a_resume_runtime_rebuilt_in_the_same_epoch_keeps_the_seal() {
        let (ran, mut f) = ready();
        let original = f.original();
        let id = original.plan.resume_request_id.clone();
        assert!(offer(&mut f, broken(&original)).not_admitted(&original.intent));
        f.rebuild(executor(ran.clone(), Resume::Completed { ready: true }));
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
            (false, "session_resume_unavailable")
        );
        assert!(!reply.not_admitted(&stale.intent));
        assert!(!f
            .fences
            .session_resume
            .sealed(&stale.plan.resume_request_id));
        assert!(no_row(&f, &stale.plan.resume_request_id));
    }

    /// Before the writer transaction the refusal is proven by a fresh empty
    /// read; a request whose row exists, a failure from the admission on and
    /// a full fence all stay unavailable and unsealed.
    #[test]
    fn only_a_proven_absent_resume_is_not_admitted() {
        // Refused before the transaction (the owner stops): proven absent.
        let (_, mut f) = ready();
        let original = f.original();
        f.runtime.request_stop();
        let stopped = f.admit(&original);
        assert!(stopped.not_admitted(&original.intent), "{}", stopped.code);
        assert!(no_row(&f, &original.plan.resume_request_id));
        // Another connection's request refused there is never sealed.
        let stale = super::original(&f.location.configuration, &uuid::Uuid::new_v4().to_string());
        let reply = f.admit(&stale);
        assert_eq!(reply.code, "session_resume_unavailable");
        assert!(!f
            .fences
            .session_resume
            .sealed(&stale.plan.resume_request_id));

        // Refused before the transaction while another writer holds the
        // store: nothing is proven.
        let (_, mut f) = ready();
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
            (false, "session_resume_unavailable")
        );
        assert!(!f
            .fences
            .session_resume
            .sealed(&original.plan.resume_request_id));

        // A row exists: the same refusal stays unavailable.
        let (_, mut f) = ready();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let existing = offer(&mut f, broken(&original));
        assert_eq!(
            (existing.accepted, existing.code.as_str()),
            (false, "session_resume_unavailable")
        );
        f.runtime.request_stop();
        let stopped = f.admit(&original);
        assert_eq!(stopped.code, "session_resume_unavailable");
        assert!(!f
            .fences
            .session_resume
            .sealed(&original.plan.resume_request_id));

        // The admission itself fails: unavailable although no row is left,
        // and nothing is sealed.
        let (ran, mut f) = ready();
        let original = f.original();
        f.store
            .db
            .execute_batch(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON desktop_session_resumes \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let failed = f.admit(&original);
        assert_eq!(
            (failed.accepted, failed.code.as_str()),
            (false, "session_resume_unavailable")
        );
        assert!(no_row(&f, &original.plan.resume_request_id));
        assert!(!f
            .fences
            .session_resume
            .sealed(&original.plan.resume_request_id));
        f.store.db.execute_batch("DROP TRIGGER refuse;").unwrap();
        assert!(
            f.admit(&original).accepted,
            "an unavailable refusal seals nothing"
        );
        f.until("settled", |f| f.row(&original).receipt.drained);
        assert_eq!(ran.counts(), (1, 1));

        // A full fence mints no new negative; its sealed IDs stay sealed.
        let (_, mut f) = ready();
        for n in 0..crate::desktop::not_admitted::BOUND {
            assert!(f.fences.session_resume.seal(&format!("sealed-{n}")));
        }
        let original = f.original();
        let full = offer(&mut f, broken(&original));
        assert_eq!(
            (full.accepted, full.code.as_str()),
            (false, "session_resume_unavailable")
        );
        assert!(!f
            .fences
            .session_resume
            .sealed(&original.plan.resume_request_id));
        assert!(f.fences.session_resume.sealed("sealed-0"));
    }

    #[test]
    fn budget_corruption_or_ambiguous_commit_is_unavailable_not_not_admitted() {
        for failure in ["counter", "row", "commit"] {
            let sent = Arc::new(Ran::default());
            let mut f = Fixture::with(executor(sent.clone(), Resume::Completed { ready: true }));
            let original = f.original();
            if failure == "counter" {
                f.store
                    .db
                    .execute("UPDATE desktop_session_resume_budget SET bytes=1", [])
                    .unwrap();
            } else if failure == "row" {
                let victim = f.original();
                assert!(f.admit(&victim).accepted);
                let receipt = serde_json::to_string(&f.row(&victim).receipt).unwrap();
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_session_resumes SET receipt=?1 WHERE request_id=?2",
                        rusqlite::params![
                            format!("{receipt}\0suffix"),
                            victim.plan.resume_request_id
                        ],
                    )
                    .unwrap();
            } else {
                f.store.db.execute_batch("PRAGMA foreign_keys=ON;
                            CREATE TABLE send_parent(id INTEGER PRIMARY KEY);
                            CREATE TABLE send_child(id INTEGER REFERENCES send_parent(id) DEFERRABLE INITIALLY DEFERRED);
                            CREATE TEMP TRIGGER fail_send_commit AFTER INSERT ON desktop_session_resumes BEGIN INSERT INTO send_child VALUES(1); END;").unwrap();
            }
            let result = f.admit(&original);
            assert_eq!(result.code, "session_resume_unavailable", "{failure}");
            assert!(!result.not_admitted(&original.intent));
            assert!(!f
                .fences
                .session_resume
                .sealed(&original.plan.resume_request_id));
            assert!(no_row(&f, &original.plan.resume_request_id));
            assert_eq!(sent.counts(), (0, 0));
        }
    }

    #[test]
    fn budget_capacity_is_proven_not_admitted_before_any_child_or_write() {
        let sent = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(sent.clone(), Resume::Completed { ready: true }));
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
        assert!(no_row(&f, &refused.plan.resume_request_id));
        assert_eq!(super::reservation_budget::retained(&f.store.db), protected);
        assert_eq!(sent.counts(), (0, 0));
        assert_eq!(super::reservation_budget::budget(&f.store.db).2, 32);
        f.runtime.request_stop();
        f.until("queued originals retire without input", |f| {
            f.runtime.drained()
        });
        assert_eq!(super::reservation_budget::budget(&f.store.db), (0, 0, 0));
        assert!(f.admit(&refused).not_admitted(&refused.intent));
        assert_eq!(sent.counts(), (0, 0));
    }

    #[test]
    fn budget_recorded_original_returns_its_charge_and_duplicate_executes_nothing() {
        let ran = Arc::new(Ran::default());
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: true }));
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
        let mut f = Fixture::with(executor(ran.clone(), Resume::Completed { ready: false }));
        f.runtime.fixture_wait(
            Duration::from_secs(5),
            Duration::from_millis(5),
            runtime::CHAIN,
        );
        let original = f.original();
        assert!(f.admit(&original).accepted);
        let charged = super::reservation_budget::budget(&f.store.db);
        f.until("first child physically drained", |f| {
            f.row(&original).receipt.resume.step == Step::Recorded
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
            "SELECT rows,bytes,active FROM desktop_session_resume_budget WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    fn key(v: &Verified) -> &str {
        &v.plan.resume_request_id
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
        db.query_row("SELECT length(CAST(resume AS BLOB))+length(CAST(source AS BLOB))+length(CAST(checkpoint AS BLOB))+length(CAST(plan AS BLOB))+50176 FROM desktop_session_resumes WHERE request_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_session_resumes)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_session_resumes ORDER BY request_id",
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
                + v.intent.resume.len()
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
                "UPDATE desktop_session_resume_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
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
        let leaf = &status["kinds"]["session_resume"];
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
                "counter"=>{store.db.execute("UPDATE desktop_session_resume_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_session_resumes SET receipt='{}' WHERE request_id=(SELECT request_id FROM desktop_session_resumes LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_session_resumes SET drained=2 WHERE request_id=(SELECT request_id FROM desktop_session_resumes LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_session_resume_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_session_resume_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
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
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_session_resume_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_session_resume_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
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
                "UPDATE desktop_session_resume_budget SET rows=1,bytes=1,active=1 WHERE id=1",
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
        let (record, held, _ticket) = super::running(&mut store, &location, &v, &epoch);
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
            .prepare("SELECT * FROM desktop_session_resumes")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_session_resumes", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_session_resumes VALUES({})",
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
            ("resume", 8192_usize),
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
                    .prepare("PRAGMA table_info(desktop_session_resumes)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_session_resumes", [], |r| {
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
                    "INSERT INTO desktop_session_resumes VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_session_resume_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
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
