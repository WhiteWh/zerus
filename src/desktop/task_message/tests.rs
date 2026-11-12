use super::*;
use crate::desktop::store::{Location, Store};
use rusqlite::TransactionBehavior;
use serde_json::json;
use zerus_client_core::desktop::task_message::{self as wire, Intent, Phase, Verified};
use zerus_client_core::tasks_write::{input_digest, message_input, message_intent};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MACHINE: &str = "22222222-2222-4222-8222-222222222222";
const TASK: &str = "11111111-1111-4111-8111-111111111111";

/// An outbox record as `reserve_message` saves a canonical frozen stdin.
fn record(request: &str) -> serde_json::Value {
    let intent = message_intent("hello", &[], "queue", "");
    json!({"id": request, "intent": intent, "input_sha256": input_digest(&message_input(&intent))})
}
fn original(configuration: &str, epoch: &str) -> Verified {
    let request = uuid::Uuid::new_v4().to_string();
    Intent {
        scope: wire::Scope {
            configuration: configuration.into(),
            service_epoch: epoch.into(),
        },
        project: "example".into(),
        task_id: TASK.into(),
        request_id: request.clone(),
        original: record(&request).to_string(),
        form: Default::default(),
    }
    .verify()
    .unwrap()
}
/// The exact reply of `verified`'s original `hgs task message` call.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reply(verified: &Verified) -> Vec<u8> {
    serde_json::to_vec(&json!({"task": {"uuid": TASK, "project": "example"},
        "message": {"kind": "message", "request_id": verified.plan.request_id, "session": null,
            "delivery": {"mode": "queue", "state": "queued", "stage": "todo", "stage_at": 7}}}))
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
fn tables(db: &rusqlite::Connection) -> i64 {
    db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_independent_message_leaf() {
    assert_eq!(
        SCHEMA - 1,
        crate::desktop::termination_outcome::SCHEMA,
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
    // Its own two tables, the task message input leaf's two, then the
    // deferred send, deferred answer and compact-to-send leaves' two each.
    // ... and the question route leaves' tables (zerus-sa9qn.8 and .32).
    assert_eq!(tables(&store.db), before + 2 + 2 + 2 + 2 + 2 + 2 + 1 + 2);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
}

#[test]
fn the_input_upgrade_adds_only_its_two_tables_with_no_charge() {
    assert_eq!(INPUTS_SCHEMA - 1, SCHEMA);
    assert_eq!(INPUTS_SCHEMA + 1, crate::desktop::deferred_send::SCHEMA);
    let (_temp, location, store, _) = fixture();
    remove_inputs_fixture(&store.db);
    store
        .db
        .pragma_update(None, "user_version", SCHEMA)
        .unwrap();
    let before = tables(&store.db);
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    // Its own two tables, then the deferred send, deferred answer and
    // compact-to-send leaves' two each.
    // ... and the question route leaves' tables (zerus-sa9qn.8 and .32).
    assert_eq!(tables(&store.db), before + 2 + 2 + 2 + 2 + 2 + 1 + 2);
    assert_eq!(inputs::totals(&store.db).unwrap(), (0, 0));
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
    let again = admit(&mut store, &verified, &writer);
    assert_eq!(again.receipt, row.receipt);
    assert_eq!(ledger::undrained(&store.db).unwrap(), 1);
    assert_eq!(store.operation_undrained().unwrap(), 1);
    assert_eq!(
        store.operation_status()["kinds"]["task_message"]["undrained"],
        1
    );
    for statement in [
        "UPDATE desktop_task_messages SET source='{}'",
        "DELETE FROM desktop_task_messages",
    ] {
        assert!(store.db.execute(statement, []).is_err(), "{statement}");
    }
    // Another original under the same request ID never replaces it.
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
        ledger::load(&store.db, &changed.plan.request_id)
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
        .execute("UPDATE desktop_task_message_budget SET bytes=16777216", [])
        .unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(ledger::admit(&tx, &verified, &writer, 1).is_err());
    drop(tx);
    assert!(ledger::load(&store.db, &verified.plan.request_id)
        .unwrap()
        .is_none());
    store
        .db
        .execute("UPDATE desktop_task_message_budget SET bytes=0", [])
        .unwrap();
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
    assert_eq!(retired.receipt.phase, Phase::Unknown);
    assert!(retired.receipt.drained);
    let replay = admit(&mut store, &verified, &writer);
    assert_eq!(replay.receipt, retired.receipt);
}

/// A reserved, running original with its exact custody, as the actor leaves it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let tx = store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    ledger::running(&tx, &row).unwrap();
    tx.commit().unwrap();
    (record, held)
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn only_the_released_custody_settles_a_message_and_its_reply_never_changes() {
    use crate::desktop::custody;
    enum Ending {
        Exit(ledger::Exit),
        NotStarted,
        Unknown,
    }
    for (index, expected) in [
        Phase::Recorded,
        Phase::Failed,
        Phase::Failed,
        Phase::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        let (_temp, location, mut store, epoch) = fixture();
        let verified = original(&location.configuration, &epoch);
        let (record, held) = running(&mut store, &location, &verified, &epoch);
        let ending = match index {
            0 => Ending::Exit(ledger::Exit {
                code: 0,
                outcome: wire::Outcome::from_reply(&verified.plan, &reply(&verified)),
            }),
            1 => Ending::Exit(ledger::Exit {
                code: 2,
                outcome: None,
            }),
            2 => Ending::NotStarted,
            _ => Ending::Unknown,
        };
        let row = ledger::pending(&store.db).unwrap().unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        match &ending {
            Ending::Exit(exit) => {
                ledger::result(&tx, &row, Some(exit), "awaiting_resource_drain").unwrap()
            }
            Ending::NotStarted => ledger::not_started(&tx, &row).unwrap(),
            Ending::Unknown => ledger::result(&tx, &row, None, "task_message_timed_out").unwrap(),
        }
        tx.commit().unwrap();
        // The recorded reply is immutable, and nothing settles before drain.
        if matches!(ending, Ending::Exit(_)) {
            assert!(store
                .db
                .execute("UPDATE desktop_task_messages SET reply=NULL", [])
                .is_err());
        }
        let row = ledger::pending(&store.db).unwrap().unwrap();
        assert!(row.receipt.result(&verified.plan).is_none());
        let observation = custody::probe(&location, &record);
        assert!(matches!(observation, custody::Observation::Held));
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::drained(tx, row, observation).is_err());
        drop(held);
        let row = ledger::pending(&store.db).unwrap().unwrap();
        // A process another test forks keeps a copy of every descriptor until
        // its exec closes it, the custody lock's open file description
        // included: the release is observed once that window passed, as the
        // drain itself keeps observing.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let observation = loop {
            let observation = custody::probe(&location, &record);
            if matches!(observation, custody::Observation::Released { .. })
                || std::time::Instant::now() >= until
            {
                break observation;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let settled = ledger::drained(tx, row, observation).unwrap();
        assert_eq!(settled.phase, expected, "{index}");
        match index {
            0 => {
                assert_eq!(settled.result(&verified.plan), Some(Ok(())));
                assert_eq!(settled.outcome.as_ref().unwrap().state, "queued");
            }
            1 => assert_eq!(
                settled.result(&verified.plan),
                Some(Err(wire::EXITED.into()))
            ),
            2 => assert_eq!(
                settled.result(&verified.plan),
                Some(Err(wire::NOT_STARTED.into()))
            ),
            _ => {
                assert!(settled.result(&verified.plan).is_none());
                assert_eq!(settled.code, "task_message_timed_out");
            }
        }
        assert_eq!(ledger::undrained(&store.db).unwrap(), 0);
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn only_a_proven_outcome_or_a_definite_exit_is_ever_recorded() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    let row = ledger::pending(&store.db).unwrap().unwrap();
    let mut wrong = wire::Outcome::from_reply(&verified.plan, &reply(&verified)).unwrap();
    wrong.state = "note".into();
    for exit in [
        // Exit 0 without the outcome it proved, or with another one.
        ledger::Exit {
            code: 0,
            outcome: None,
        },
        ledger::Exit {
            code: 0,
            outcome: Some(wrong),
        },
        // A failed exit never carries an outcome.
        ledger::Exit {
            code: 1,
            outcome: wire::Outcome::from_reply(&verified.plan, &reply(&verified)),
        },
    ] {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::result(&tx, &row, Some(&exit), "awaiting_resource_drain").is_err());
    }
    // No Unknown code can pose as the closed not-started result.
    for code in [wire::NOT_STARTED, "awaiting_not_started_drain"] {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(ledger::result(&tx, &row, None, code).is_err(), "{code}");
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn the_lookup_reads_only_its_exact_original() {
    let (_temp, location, mut store, epoch) = fixture();
    let verified = original(&location.configuration, &epoch);
    let (_record, _held) = running(&mut store, &location, &verified, &epoch);
    let query = wire::Query::for_plan(verified.intent.scope.clone(), &verified.plan).unwrap();
    let read = |store: &Store, query: &wire::Query| {
        super::query(store, &json!({"op": "task_message_result", "query": query}))
    };
    let lookup: wire::Lookup = serde_json::from_value(read(&store, &query)).unwrap();
    assert!(lookup.matches(&query));
    assert_eq!(lookup.receipt.unwrap().phase, Phase::Running);
    let mut changed = query.clone();
    changed.scope.service_epoch = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        read(&store, &changed)["code"],
        "task_message_result_unavailable"
    );
    let mut missing = query;
    missing.request_id = uuid::Uuid::new_v4().to_string();
    let lookup: wire::Lookup = serde_json::from_value(read(&store, &missing)).unwrap();
    assert!(!lookup.found && lookup.receipt.is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod message_child {
    use super::super::child::{self, Ended};
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn lock() -> (
        tempfile::TempDir,
        Location,
        zerus_desktop_ledger::operations::custody::Record,
        std::sync::Arc<zerus_client_core::run::custody::InheritedLock>,
    ) {
        let (temp, location, mut store, epoch) = fixture();
        let verified = original(&location.configuration, &epoch);
        let (record, held) = running(&mut store, &location, &verified, &epoch);
        (temp, location, record, held)
    }
    fn python(script: &str) -> Vec<String> {
        vec!["python3".into(), "-c".into(), script.into()]
    }
    fn guard() -> Arc<dyn Send + Sync> {
        Arc::new(())
    }
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(20)
    }

    /// Reports whether the descriptor named by `argv[1]` is the file `argv[2]`
    /// (`dev:ino`), on stdout unless `argv[3]` is `stderr`.
    const CUSTODY_SCRIPT: &str = r#"import os, sys
var = sys.argv[1]
want = sys.argv[2]
out = sys.stderr if sys.argv[3:] == ["stderr"] else sys.stdout
named = os.environ.get(var, "")
if not named.isdigit():
    print("custody: no variable", end="", file=out)
    sys.exit(3)
try:
    seen = os.fstat(int(named))
except OSError:
    print("custody: not inherited", end="", file=out)
    sys.exit(3)
print("custody: inherited" if f"{seen.st_dev}:{seen.st_ino}" == want else "custody: another file", end="", file=out)
"#;

    /// The runner's child holds the actor's exact custody descriptor, named
    /// by this kind's variable.
    #[test]
    fn the_runner_child_inherits_exactly_the_named_custody() {
        use std::os::fd::AsRawFd;
        let (_temp, _location, _record, held) = lock();
        // SAFETY: fstat into a zeroed struct on a descriptor `held` owns.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(held.as_raw_fd(), &mut stat) }, 0);
        let mut args = python(CUSTODY_SCRIPT);
        args.push(child::CUSTODY_FD.into());
        args.push(format!("{}:{}", stat.st_dev, stat.st_ino));
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &args,
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Exited {
                code: 0,
                stdout: b"custody: inherited".to_vec()
            }
        );
    }

    #[test]
    fn the_child_reads_exactly_its_frozen_input_and_returns_its_whole_reply() {
        let (_temp, _location, _record, held) = lock();
        let input = b"frozen \xff stdin".to_vec();
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &python("import sys\nsys.stdout.buffer.write(sys.stdin.buffer.read()[::-1])"),
                &input,
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Exited {
                code: 0,
                stdout: input.iter().rev().copied().collect()
            }
        );
        // A non-zero exit is reported as such, with its reply.
        assert_eq!(
            child::run(
                Path::new("/bin/sh"),
                &["-c".into(), "cat >/dev/null; printf no; exit 3".into()],
                b"x",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Exited {
                code: 3,
                stdout: b"no".to_vec()
            }
        );
    }

    #[test]
    fn incomplete_input_or_reply_and_the_deadline_are_unknown() {
        let (_temp, _location, _record, held) = lock();
        // A child that exits without reading more than a pipe's worth.
        assert_eq!(
            child::run(
                Path::new("/bin/sh"),
                &["-c".into(), "exit 0".into()],
                &vec![b'x'; 1 << 20],
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Unknown("task_message_input_unwritten")
        );
        // A reply of exactly the retained bound is whole; one byte more is
        // Unknown.
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &python(&format!(
                    "import sys\nsys.stdout.buffer.write(b'x' * {})",
                    child::STDOUT_BYTES
                )),
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Exited {
                code: 0,
                stdout: vec![b'x'; child::STDOUT_BYTES]
            }
        );
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &python(&format!(
                    "import sys\nsys.stdout.buffer.write(b'x' * {})",
                    child::STDOUT_BYTES + 1
                )),
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Unknown("task_message_reply_too_large")
        );
        // The deadline kills only the original child.
        let started = Instant::now();
        assert_eq!(
            child::run(
                Path::new("/bin/sleep"),
                &["30".into()],
                b"",
                &held,
                &guard(),
                Instant::now() + Duration::from_millis(1500),
                |_| Ok(())
            ),
            Ended::Unknown("task_message_timed_out")
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // A signal after the child started is never "not started".
        assert_eq!(
            child::run(
                Path::new("/bin/sh"),
                &["-c".into(), "kill -9 $$".into()],
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::Unknown("task_message_signalled")
        );
    }

    #[test]
    fn only_a_child_that_never_existed_is_not_started() {
        let (_temp, _location, _record, held) = lock();
        assert_eq!(
            child::run(
                Path::new("/nonexistent/zerus-hgs"),
                &[],
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::NotStarted
        );
        child::REFUSE_PRE_EXEC.with(|refuse| refuse.set(true));
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &["true".into()],
                b"",
                &held,
                &guard(),
                soon(),
                |_| Ok(())
            ),
            Ended::NotStarted
        );
    }

    #[test]
    fn an_unreadable_output_or_unobserved_exit_is_unknown_and_the_child_is_reaped() {
        let (_temp, _location, _record, held) = lock();
        for (read, expected) in [
            (true, "task_message_output_unreadable"),
            (false, "task_message_unobserved"),
        ] {
            if read {
                child::READ_ERROR.with(|fault| fault.set(true));
            } else {
                child::WAIT_ERROR.with(|fault| fault.set(true));
            }
            // The wait-fault helper exits on its own in two seconds, even
            // under mutation; nothing is signalled by name.
            let resources = Arc::new(());
            let weak = Arc::downgrade(&resources);
            let guard: Arc<dyn Send + Sync> = resources;
            let ended = child::run(
                Path::new("/usr/bin/env"),
                &python(if read {
                    "pass"
                } else {
                    "import time; time.sleep(2)"
                }),
                b"",
                &held,
                &guard,
                soon(),
                |_| Ok(()),
            );
            drop(guard);
            assert_eq!(ended, Ended::Unknown(expected));
            assert!(
                child::READ_ERROR.with(|v| !v.get()) && child::WAIT_ERROR.with(|v| !v.get()),
                "the fault was observed"
            );
            let pid = child::SPAWNED.with(|spawned| spawned.take()).unwrap() as i32;
            let mut status = 0;
            // SAFETY: exactly the private child PID reported by Child::id;
            // waitpid cannot observe an unrelated non-child.
            let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            let reaped = result == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
            if result == 0 {
                // Mutation cleanup only: the same helper's own bounded exit.
                unsafe {
                    libc::waitpid(pid, &mut status, 0);
                }
            }
            assert!(reaped, "the original child was waited for");
            if read {
                // That pipe's end can never be observed: its guard stays.
                std::thread::sleep(Duration::from_millis(50));
                assert!(weak.strong_count() > 0, "an unreadable pipe pins the guard");
            } else {
                // A confirmed exit and real pipe ends release every copy.
                let until = Instant::now() + Duration::from_secs(10);
                while weak.strong_count() > 0 {
                    assert!(Instant::now() < until, "the guard outlived the pipes");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// review-n's probe at f5791071: after a timeout or an unobserved exit, the
    /// recovery drained the row while a descendant still held the original
    /// stdout. Now the pipe's guard keeps the SAME lock until that pipe ends.
    #[test]
    fn a_timed_out_or_unobserved_original_keeps_custody_until_its_descendant_pipe_ends() {
        for wait_error in [false, true] {
            let (_temp, location, mut store, epoch) = fixture();
            let original = original(&location.configuration, &epoch);
            let (record, held) = running(&mut store, &location, &original, &epoch);
            let finish = location.root.join("descendant-finished");
            let release = location.root.join("descendant-release");
            let spawned = location.root.join("descendant-spawned");
            // The descendant keeps the original stdout until the test releases
            // it, then records its end; it never inherits the custody lock.
            // Nothing here races the clock: the deadline only needs to cover
            // interpreter startup under load, and only the release (or, if
            // the test failed, 30 seconds) ends it.
            let script = format!(
                "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',\"import pathlib,sys,time\\nr=pathlib.Path(sys.argv[2])\\nend=time.time()+30\\nwhile not r.exists() and time.time()<end: time.sleep(0.02)\\npathlib.Path(sys.argv[1]).write_text('finished')\",{},{}],stdout=sys.stdout,stderr=subprocess.DEVNULL)\nopen({},'w').write('spawned')\nsys.stdout.write('descendant-ready')\nsys.stdout.flush()\ntime.sleep(60)",
                serde_json::to_string(&finish).unwrap(),
                serde_json::to_string(&release).unwrap(),
                serde_json::to_string(&spawned).unwrap()
            );
            child::WAIT_ERROR_AFTER_OUTPUT.with(|armed| armed.set(wait_error));
            let guard: Arc<dyn Send + Sync> = Arc::new(held.clone());
            let ended = child::run(
                Path::new("/usr/bin/env"),
                &python(&script),
                b"frozen",
                &held,
                &guard,
                Instant::now() + Duration::from_secs(5),
                |_| Ok(()),
            );
            // A child too slow to reach its descendant within the deadline
            // reports itself here, never as a failure of the code under test.
            assert!(
                spawned.exists(),
                "the original child never started its descendant within 5 s: a slow start, not this test's subject"
            );
            let expected = if wait_error {
                "task_message_unobserved"
            } else {
                "task_message_timed_out"
            };
            assert_eq!(ended, Ended::Unknown(expected));
            let row = ledger::load(&store.db, &original.plan.request_id)
                .unwrap()
                .unwrap();
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            ledger::result(&tx, &row, None, expected).unwrap();
            tx.commit().unwrap();
            drop((guard, held));
            let drained = |store: &mut Store| {
                store.recover_operation_custody(&location);
                ledger::load(&store.db, &original.plan.request_id)
                    .unwrap()
                    .unwrap()
                    .receipt
                    .drained
            };
            let until = Instant::now() + Duration::from_millis(800);
            while Instant::now() < until {
                assert!(
                    !drained(&mut store) || finish.exists(),
                    "drained before the descendant's pipe ended ({expected})"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(matches!(
                crate::desktop::custody::probe(&location, &record),
                crate::desktop::custody::Observation::Held
            ));
            std::fs::write(&release, b"release").unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            while !drained(&mut store) {
                assert!(Instant::now() < until, "never drained after the real end");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(finish.exists());
        }
    }

    /// The original child prints a reply, starts a descendant that keeps its
    /// stdout open and exits. Like `hgs` (adopt_custody), it never lets the
    /// descendant inherit the custody lock. Only an EOF on the test's accepted
    /// socket ends the descendant; a panic or crash closes it too.
    pub(super) const RETAINING: &str = r#"import os,subprocess,sys
sys.stdin.buffer.read()
sys.stdout.buffer.write(b'{}')
sys.stdout.flush()
subprocess.Popen([sys.executable,'-c',"import socket,sys\nc=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\nc.connect(sys.argv[1])\nassert c.recv(1)==b''",sys.argv[1]],stdin=subprocess.DEVNULL,close_fds=True)
os._exit(0)"#;

    /// The one connection of the descendant, accepted within a bound.
    pub(super) fn accept(
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
    fn an_original_pipe_held_by_a_descendant_keeps_the_guard_until_its_real_end() {
        let (_temp, _location, _record, held) = lock();
        let control = tempfile::tempdir().unwrap();
        let socket = control.path().join("c");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let resources = Arc::new(());
        let weak = Arc::downgrade(&resources);
        let guard: Arc<dyn Send + Sync> = resources;
        let mut args = python(RETAINING);
        args.push(socket.to_str().unwrap().into());
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &args,
                b"",
                &held,
                &guard,
                Instant::now() + Duration::from_millis(1500),
                |_| Ok(())
            ),
            Ended::Unknown("task_message_output_retained")
        );
        let release = accept(&listener, || ());
        drop(guard);
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            weak.strong_count() > 0,
            "the still-open original pipe keeps its own guard copy"
        );
        drop(release); // EOF is the one explicit release of the descendant.
        let until = Instant::now() + Duration::from_secs(10);
        while weak.strong_count() > 0 {
            assert!(Instant::now() < until, "the guard outlived the pipe's end");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A script image: the original reads its input and answers `original`;
    /// a replacement records that it ran and echoes the private input.
    #[cfg(target_os = "linux")]
    fn image(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("hgs");
        std::fs::write(
            &path,
            "#!/usr/bin/env python3\nimport sys\nsys.stdin.buffer.read()\nsys.stdout.write('original')\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        (path, dir.join("replacement-ran"))
    }
    #[cfg(target_os = "linux")]
    fn replace(path: &Path, ran: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let next = path.with_extension("next");
        std::fs::write(
            &next,
            format!(
                "#!/usr/bin/env python3\nimport pathlib,sys\npathlib.Path({:?}).write_text('ran')\nsys.stdout.buffer.write(b'replacement received: '+sys.stdin.buffer.read())\n",
                ran.to_str().unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(&next, path).unwrap();
    }

    /// The same inode rewritten in place, as another writer of the selected
    /// image could.
    #[cfg(target_os = "linux")]
    fn rewrite(path: &Path, ran: &Path) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        write!(
            file,
            "#!/usr/bin/env python3\nimport pathlib,sys\npathlib.Path({:?}).write_text('ran')\nsys.stdout.buffer.write(b'rewrite received: '+sys.stdin.buffer.read())\n",
            ran.to_str().unwrap()
        )
        .unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_image_rewritten_in_place_before_the_final_check_starts_no_child() {
        let (_temp, _location, _record, held) = lock();
        let dir = tempfile::tempdir().unwrap();
        let (path, ran) = image(dir.path());
        let pin = zerus_client_core::image::linux::Pin::open(&path).unwrap();
        let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());
        rewrite(&path, &ran);
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap()),
            inode,
            "the same inode"
        );
        assert_eq!(
            super::super::runtime::pinned(
                &pin,
                &[],
                b"private frozen input",
                &held,
                &guard(),
                soon()
            ),
            Ended::NotStarted
        );
        assert!(!ran.exists(), "the rewrite never ran");
    }

    /// review-win2-c's same-inode probe on #1307: after the final check, an
    /// in-place rewrite must never run. The child executes the sealed snapshot.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_image_rewritten_in_place_after_the_final_check_never_runs_or_receives_the_input() {
        let (_temp, _location, _record, held) = lock();
        let dir = tempfile::tempdir().unwrap();
        let (path, ran) = image(dir.path());
        let pin = zerus_client_core::image::linux::Pin::open(&path).unwrap();
        let (late, marker) = (path.clone(), ran.clone());
        child::AFTER_FINAL_CHECK
            .with(|hook| *hook.borrow_mut() = Some(Box::new(move || rewrite(&late, &marker))));
        assert_eq!(
            super::super::runtime::pinned(
                &pin,
                &[],
                b"private frozen input",
                &held,
                &guard(),
                soon()
            ),
            Ended::Exited {
                code: 0,
                stdout: b"original".to_vec()
            }
        );
        assert!(child::AFTER_FINAL_CHECK.with(|hook| hook.borrow().is_none()));
        assert!(!ran.exists(), "the rewrite never ran");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_image_replaced_before_the_final_check_starts_no_child() {
        let (_temp, _location, _record, held) = lock();
        let dir = tempfile::tempdir().unwrap();
        let (path, ran) = image(dir.path());
        let pin = zerus_client_core::image::linux::Pin::open(&path).unwrap();
        replace(&path, &ran);
        assert_eq!(
            super::super::runtime::pinned(
                &pin,
                &[],
                b"private frozen input",
                &held,
                &guard(),
                soon()
            ),
            Ended::NotStarted
        );
        assert!(!ran.exists(), "the replacement never ran");
    }

    /// review-n's late-swap probe at f5791071: the replacement ran and echoed
    /// the private frozen input. Now the child executes the pinned descriptor.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_image_replaced_after_the_final_check_never_runs_or_receives_the_input() {
        let (_temp, _location, _record, held) = lock();
        let dir = tempfile::tempdir().unwrap();
        let (path, ran) = image(dir.path());
        let pin = zerus_client_core::image::linux::Pin::open(&path).unwrap();
        let (late, marker) = (path.clone(), ran.clone());
        child::AFTER_FINAL_CHECK
            .with(|hook| *hook.borrow_mut() = Some(Box::new(move || replace(&late, &marker))));
        assert_eq!(
            super::super::runtime::pinned(
                &pin,
                &[],
                b"private frozen input",
                &held,
                &guard(),
                soon()
            ),
            Ended::Exited {
                code: 0,
                stdout: b"original".to_vec()
            }
        );
        assert!(child::AFTER_FINAL_CHECK.with(|hook| hook.borrow().is_none()));
        assert!(!ran.exists(), "the replacement never ran");
    }

    #[test]
    fn the_child_leads_its_own_process_group() {
        let (_temp, _location, _record, held) = lock();
        assert_eq!(
            child::run(
                Path::new("/usr/bin/env"),
                &python("import os,sys\nsys.stdout.write('own' if os.getpgid(0)==os.getpid() else 'shared')"),
                b"",
                &held,
                &guard(),
                soon(), |_| Ok(())),
            Ended::Exited {
                code: 0,
                stdout: b"own".to_vec()
            }
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod actor {
    use super::super::child::Ended;
    use super::*;
    use crate::{config::Config, desktop::ui_authority};
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
                    &[String],
                    &[u8],
                    &Arc<zerus_client_core::run::custody::InheritedLock>,
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
        #[allow(clippy::type_complexity)]
        fn build(
            execute: Option<
                Arc<
                    dyn Fn(
                            &wire::Plan,
                            &[String],
                            &[u8],
                            &Arc<zerus_client_core::run::custody::InheritedLock>,
                            &Arc<dyn Send + Sync>,
                        ) -> Ended
                        + Send
                        + Sync,
                >,
            >,
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
            match execute {
                Some(execute) => runtime.fixture_executor(MACHINE, execute),
                None => runtime.fixture_machine(MACHINE),
            }
            // The shared actor logic is tested on every platform; the macOS
            // fence has its own test.
            runtime.fixture_mac_fence(false);
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
            super::original(&self.location.configuration, &self.epoch)
        }
        fn admit(&mut self, original: &Verified) -> wire::Admission {
            let request = zerus_client_core::desktop::ui_requests::Request::TaskMessage {
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
            ledger::load(&self.store.db, &original.plan.request_id)
                .unwrap()
                .unwrap()
        }
    }

    #[test]
    fn an_admitted_message_runs_once_and_settles_only_from_its_exact_reply() {
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let mut f = Fixture::new(move |plan, args, input, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            // The one `hgs task message` call, with the record's frozen stdin.
            assert_eq!(
                args,
                [
                    "task",
                    "message",
                    TASK,
                    "--request-id",
                    &plan.request_id,
                    "--mode",
                    "queue",
                    "--expect-dispatch",
                    "",
                    "--content-json",
                    "--project",
                    "example",
                ]
            );
            let frozen = message_input(&message_intent("hello", &[], "queue", ""));
            assert_eq!(input, frozen.as_bytes());
            assert_eq!(input_digest(&frozen), plan.input_sha256);
            Ended::Exited {
                code: 0,
                stdout: reply_for(plan),
            }
        });
        let original = f.original();
        assert!(f.admit(&original).matches(&original.intent));
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(receipt.phase, Phase::Recorded);
        assert_eq!(receipt.result(&original.plan), Some(Ok(())));
        assert_eq!(receipt.outcome.as_ref().unwrap().state, "queued");
        // The same original admitted again returns its settled receipt.
        assert_eq!(f.admit(&original).receipt.unwrap(), receipt);
        f.runtime.request_stop();
        f.until("stopped", |f| f.runtime.drained());
        assert_eq!(sent.load(Ordering::SeqCst), 1, "never sent again");
    }

    fn reply_for(plan: &wire::Plan) -> Vec<u8> {
        serde_json::to_vec(&json!({"task": {"uuid": plan.task_id, "project": plan.project},
            "message": {"kind": "message", "request_id": plan.request_id, "session": null,
                "delivery": {"mode": plan.mode, "state": "queued", "stage": "todo", "stage_at": 7}}}))
        .unwrap()
    }

    #[test]
    fn an_unproven_reply_a_failed_exit_and_the_own_image_refusal_settle_as_such() {
        for (ended, phase, code) in [
            (
                Ended::Exited {
                    code: 0,
                    stdout: b"{}".to_vec(),
                },
                Phase::Unknown,
                "task_message_reply_unconfirmed",
            ),
            (
                Ended::Exited {
                    code: 2,
                    stdout: b"hgs: refused".to_vec(),
                },
                Phase::Failed,
                wire::EXITED,
            ),
            (
                Ended::Unknown("task_message_timed_out"),
                Phase::Unknown,
                "task_message_timed_out",
            ),
        ] {
            let result = std::sync::Mutex::new(Some(ended));
            let mut f = Fixture::new(move |_, _, _, _, _| result.lock().unwrap().take().unwrap());
            let original = f.original();
            assert!(f.admit(&original).accepted);
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!((receipt.phase, receipt.code.as_str()), (phase, code));
            assert!(receipt.outcome.is_none());
            f.runtime.request_stop();
            f.until("stopped", |f| f.runtime.drained());
        }
        // The real original path: this test image refuses its own image.
        let mut f = Fixture::unexecuted();
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Failed, wire::NOT_STARTED)
        );
    }

    #[test]
    fn a_descendant_pipe_keeps_custody_charge_and_takeover_until_its_eof() {
        use super::message_child::{accept, RETAINING};
        use crate::desktop::custody::{self, Observation};
        let control = tempfile::tempdir().unwrap();
        let socket = control.path().join("c");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let path = socket.to_str().unwrap().to_owned();
        // A real original child; its descendant keeps the original stdout
        // open past the deadline and holds no custody lock of its own.
        let mut f = Fixture::new(move |_, _, input, lock, resources| {
            child::run(
                std::path::Path::new("/usr/bin/env"),
                &[
                    "python3".into(),
                    "-c".into(),
                    RETAINING.into(),
                    path.clone(),
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
        // Keep the SAME accepted endpoint through the logical outcome.
        let release = accept(&listener, || {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        });
        f.until("logical Unknown", |f| {
            f.row(&original).receipt.code == "task_message_output_retained"
        });
        let row = f.row(&original);
        assert_eq!(row.receipt.phase, Phase::Unknown);
        assert!(!row.receipt.drained);
        let record = row.custody.clone().unwrap();
        assert!(matches!(
            custody::probe(&f.location, &record),
            Observation::Held
        ));
        // Neither revoke nor a stop request releases the original resources.
        f.store
            .db
            .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
            .unwrap();
        f.runtime.request_stop();
        for _ in 0..40 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!f.runtime.drained(), "takeover stays blocked");
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 1, "charged");
        assert!(matches!(
            custody::probe(&f.location, &record),
            Observation::Held
        ));
        drop(release); // EOF is the one explicit release of the descendant.
        f.until("drained", |f| {
            f.runtime.drained()
                && matches!(
                    custody::probe(&f.location, &record),
                    Observation::Released { .. }
                )
        });
        assert_eq!(ledger::undrained(&f.store.db).unwrap(), 0);
        let row = f.row(&original);
        assert!(row.receipt.drained);
        assert_eq!(
            (row.receipt.phase, row.receipt.code.as_str()),
            (Phase::Unknown, "task_message_output_retained")
        );
    }

    #[test]
    fn the_macos_fence_refuses_before_any_row_lease_or_child() {
        let mut f = Fixture::new(|_, _, _, _, _| panic!("no fenced child"));
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

    /// review-n's probes at f5791071, kept as regressions.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_native_permit_that_expires_while_begin_waits_admits_nothing() {
        let mut f = Fixture::new(|_, _, _, _, _| panic!("expired permit must not execute"));
        let original = f.original();
        let request = zerus_client_core::desktop::ui_requests::Request::TaskMessage {
            intent: Box::new(original.intent.clone()),
        };
        let authority = ui_authority::Validated::fixture_expiring(
            &json!(request),
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
        assert!(ledger::load(&f.store.db, &original.plan.request_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_revoke_after_reservation_prevents_the_message_step() {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let mut f = Fixture::new(move |_, _, _, _, _| {
            called.fetch_add(1, Ordering::SeqCst);
            Ended::Unknown("lost")
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("reserved before the step", |f| {
            f.row(&original).receipt.phase == Phase::Running
        });
        f.store
            .db
            .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
            .unwrap();
        f.until("refused and drained", |f| f.row(&original).receipt.drained);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
    }

    #[test]
    fn duplicates_while_running_and_after_a_lost_result_never_dispatch_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        let mut f = Fixture::new(move |_, _, _, _, _| {
            called.fetch_add(1, Ordering::SeqCst);
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ended::Unknown("task_message_reply_lost")
        });
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("running child", |_| calls.load(Ordering::SeqCst) == 1);
        for _ in 0..3 {
            assert!(f.admit(&original).accepted);
        }
        release_tx.send(()).unwrap();
        f.until("Unknown drained", |f| f.row(&original).receipt.drained);
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
        for _ in 0..3 {
            assert!(f.admit(&original).accepted);
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
        }
        f.runtime.request_stop();
        f.until("drained", |f| f.runtime.drained());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the original dispatch"
        );
    }

    /// The real original path, without an executor: the runtime's pinned
    /// image (a script here) receives the exact call and frozen stdin.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pinned_image_runs_the_original_call_and_its_exact_reply_settles() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hgs");
        let seen = dir.path().join("stdin");
        std::fs::write(
            &path,
            format!(
                "#!/usr/bin/env python3\nimport json,pathlib,sys\npathlib.Path({:?}).write_bytes(sys.stdin.buffer.read())\na=sys.argv\nprint(json.dumps({{'task':{{'uuid':a[a.index('message')+1],'project':a[a.index('--project')+1]}},'message':{{'kind':'message','request_id':a[a.index('--request-id')+1],'session':None,'delivery':{{'mode':a[a.index('--mode')+1],'state':'queued','stage':'todo','stage_at':7}}}}}}))\n",
                seen.to_str().unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut f = Fixture::unexecuted();
        f.runtime.fixture_image(&path);
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.until("settled", |f| f.row(&original).receipt.drained);
        let receipt = f.row(&original).receipt;
        assert_eq!(
            (receipt.phase, receipt.code.as_str()),
            (Phase::Recorded, "")
        );
        assert_eq!(
            std::fs::read_to_string(&seen).unwrap(),
            message_input(&message_intent("hello", &[], "queue", ""))
        );
    }

    #[test]
    fn a_reference_form_original_is_refused_before_any_row_until_it_can_be_prepared() {
        let mut f = Fixture::new(|_, _, _, _, _| panic!("no reference child"));
        let intent = zerus_client_core::tasks_write::message_intent(
            "see this",
            &[zerus_client_core::attachments::Attachment {
                name: "screen.png".into(),
                mime: "image/png".into(),
                bytes: vec![7; 3000].into(),
                reference: "[screen.png]".into(),
            }],
            "queue",
            "",
        );
        let request = uuid::Uuid::new_v4().to_string();
        let record = json!({"id": request, "intent": intent,
            "input_sha256": input_digest(&message_input(&intent))});
        let (projection, _) = zerus_client_core::tasks_write::references::project(&record).unwrap();
        let referenced = Intent {
            scope: wire::Scope {
                configuration: f.location.configuration.clone(),
                service_epoch: f.epoch.clone(),
            },
            project: "example".into(),
            task_id: TASK.into(),
            request_id: request.clone(),
            original: projection.to_string(),
            form: wire::Form::ReferenceV1,
        }
        .verify()
        .unwrap();
        let admission = f.admit(&referenced);
        assert!(!admission.accepted);
        assert_eq!(admission.code, "task_message_unavailable");
        assert!(ledger::load(&f.store.db, &request).unwrap().is_none());
    }

    #[test]
    fn a_revoked_writer_refuses_the_step_and_no_child_starts() {
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let mut f = Fixture::new(move |_, _, _, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ended::Exited {
                code: 0,
                stdout: Vec::new(),
            }
        });
        let original = f.original();
        f.store
            .db
            .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
            .unwrap();
        let _ = f.admit(&original);
        for _ in 0..50 {
            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(sent.load(Ordering::SeqCst), 0, "no child after revoke");
        assert!(!f.admit(&f.original()).accepted);
    }

    #[test]
    fn restart_retires_a_queued_message_and_never_reconstructs_execution_consent() {
        let mut f = Fixture::new(|_, _, _, _, _| panic!("recovered message must not execute"));
        let original = f.original();
        assert!(f.admit(&original).accepted);
        f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
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
        assert_eq!(f.row(&original).receipt.phase, Phase::Unknown);
        assert_eq!(f.row(&original).receipt.code, "task_message_interrupted");
    }

    // The reference form (zerus-5wmh.66.4.4.2): preparation off the actor,
    // a short admission and the custody input's lifetime.
    mod reference {
        use super::super::super::{inputs, staging};
        use super::*;
        use crate::desktop::fanout::Queue;
        use serde_json::Value;
        use sha2::Digest;
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use zerus_client_core::attachments::Attachment;
        use zerus_client_core::tasks_write::references;

        fn shot(seed: u8, size: usize) -> Vec<u8> {
            (0..size).map(|n| (n as u8).wrapping_mul(seed)).collect()
        }
        impl Fixture {
            /// A verified reference original of this service, its blobs and
            /// its exact stdin.
            fn referenced(&self, files: &[Vec<u8>]) -> (Verified, Vec<Vec<u8>>, String) {
                let attachments: Vec<Attachment> = files
                    .iter()
                    .enumerate()
                    .map(|(n, bytes)| Attachment {
                        name: format!("shot-{n}.png"),
                        mime: "image/png".into(),
                        bytes: bytes.clone().into(),
                        reference: format!("[shot-{n}.png]"),
                    })
                    .collect();
                let intent = message_intent("see these", &attachments, "queue", "");
                let request = uuid::Uuid::new_v4().to_string();
                let record = json!({"id": request, "intent": intent,
                    "input_sha256": input_digest(&message_input(&intent))});
                let (projection, blobs) = references::project(&record).unwrap();
                let verified = Intent {
                    scope: wire::Scope {
                        configuration: self.location.configuration.clone(),
                        service_epoch: self.epoch.clone(),
                    },
                    project: "example".into(),
                    task_id: TASK.into(),
                    request_id: request,
                    original: projection.to_string(),
                    form: wire::Form::ReferenceV1,
                }
                .verify()
                .unwrap();
                (verified, blobs, message_input(&record["intent"]))
            }
            /// Stages each blob as the client does.
            fn stage(&self, verified: &Verified, blobs: &[Vec<u8>]) -> PathBuf {
                let dir = self
                    .location
                    .root
                    .join(staging::STAGING)
                    .join(&verified.plan.request_id);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
                for blob in blobs {
                    let file = dir.join(format!("{:x}", sha2::Sha256::digest(blob)));
                    std::fs::write(&file, blob).unwrap();
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                dir
            }
            fn send_reference(
                &mut self,
                original: &Verified,
                authority: ui_authority::Validated,
            ) -> Arc<Queue> {
                let _ = original;
                let queue = Arc::new(Queue::default());
                self.runtime.frontend_reference(
                    &mut self.store,
                    authority,
                    Some(self.exclusion.clone()),
                    queue.clone(),
                    1,
                );
                queue
            }
            fn request(original: &Verified) -> Value {
                json!(
                    zerus_client_core::desktop::ui_requests::Request::TaskMessage {
                        intent: Box::new(original.intent.clone()),
                    }
                )
            }
            fn answered(&mut self, queue: &Queue) -> Value {
                let until = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(frame) = queue.take() {
                        let frame: Value = serde_json::from_slice(&frame).unwrap();
                        return frame["value"].clone();
                    }
                    assert!(Instant::now() < until, "no answer");
                    self.runtime
                        .poll(&mut self.store, Some(self.exclusion.clone()));
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            fn input(&self, original: &Verified) -> Option<inputs::Record> {
                inputs::load(&self.store.db, &original.plan.request_id).unwrap()
            }
            fn input_path(&self, original: &Verified) -> PathBuf {
                self.location
                    .root
                    .join(staging::INPUTS)
                    .join(&original.plan.request_id)
            }
            fn state(&self, original: &Verified) -> String {
                self.input(original).map(|r| r.state).unwrap_or_default()
            }
        }
        /// A script image that records its stdin and answers with the exact
        /// reply of the call it was given.
        #[cfg(target_os = "linux")]
        fn replying_image(dir: &std::path::Path) -> (PathBuf, PathBuf) {
            let path = dir.join("hgs");
            let seen = dir.join("stdin");
            std::fs::write(
                &path,
                format!(
                    "#!/usr/bin/env python3\nimport json,os,pathlib,stat,sys\npathlib.Path({:?}).with_suffix('.fifo').write_text(str(stat.S_ISFIFO(os.fstat(0).st_mode)))\npathlib.Path({:?}).write_bytes(sys.stdin.buffer.read())\na=sys.argv\nprint(json.dumps({{'task':{{'uuid':a[a.index('message')+1],'project':a[a.index('--project')+1]}},'message':{{'kind':'message','request_id':a[a.index('--request-id')+1],'session':None,'delivery':{{'mode':a[a.index('--mode')+1],'state':'queued','stage':'todo','stage_at':7}}}}}}))\n",
                    seen.to_str().unwrap(),
                    seen.to_str().unwrap()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            (path, seen)
        }

        #[test]
        #[cfg(target_os = "linux")]
        fn a_reference_original_runs_from_its_exact_input_which_goes_only_after_its_drain() {
            let dir = tempfile::tempdir().unwrap();
            let (image, seen) = replying_image(dir.path());
            let mut f = Fixture::unexecuted();
            f.runtime.fixture_image(&image);
            // Over the inline limit: only the reference form can carry it.
            let (original, blobs, stdin) = f.referenced(&[shot(3, 100 << 10), shot(5, 4000)]);
            let staged = f.stage(&original, &blobs);
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            let answer = f.answered(&queue);
            assert_eq!(answer["accepted"], true, "{answer}");
            assert_eq!(f.state(&original), "held");
            f.until("settled", |f| f.row(&original).receipt.drained);
            let receipt = f.row(&original).receipt;
            assert_eq!(
                (receipt.phase, receipt.code.as_str()),
                (Phase::Recorded, "")
            );
            assert_eq!(std::fs::read_to_string(&seen).unwrap(), stdin);
            // The child read a pipe, never the custody input's descriptor.
            assert_eq!(
                std::fs::read_to_string(seen.with_extension("fifo")).unwrap(),
                "True"
            );
            f.until("input removed", |f| f.state(&original) == "removed");
            assert!(!f.input_path(&original).exists());
            assert!(!staged.exists(), "its staged copies went at admission");
            assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            // A legitimately removed input never stops the bounded lookup.
            let query =
                wire::Query::for_plan(original.intent.scope.clone(), &original.plan).unwrap();
            let lookup: wire::Lookup = serde_json::from_value(super::super::super::query(
                &f.store,
                &json!({"op": "task_message_result", "query": query}),
            ))
            .unwrap();
            assert!(lookup.matches(&query));
            assert_eq!(lookup.receipt.unwrap().phase, Phase::Recorded);
            // The same original again only reads its receipt: never prepared
            // again, its input never recreated.
            let again = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            let answer = f.answered(&again);
            assert_eq!(answer["accepted"], true);
            assert_eq!(answer["receipt"]["phase"], "recorded");
            assert_eq!(f.state(&original), "removed");
            assert!(!f.input_path(&original).exists());
        }

        /// The sweep's removal worker keeps the service's own exclusion until
        /// it finished, even when its runtime is gone, and never unlinks
        /// after its deadline.
        #[test]
        #[cfg(target_os = "linux")]
        fn a_removal_worker_keeps_the_owner_until_it_finished_and_never_unlinks_late() {
            let dir = tempfile::tempdir().unwrap();
            let (image, _seen) = replying_image(dir.path());
            let mut f = Fixture::unexecuted();
            f.runtime.fixture_image(&image);
            let (original, blobs, _) = f.referenced(&[shot(29, 3000)]);
            f.stage(&original, &blobs);
            let id = original.plan.request_id.clone();
            staging::REMOVAL_TIMES
                .lock()
                .unwrap()
                .push((id.clone(), Duration::from_millis(300)));
            let (entered, reading) = std::sync::mpsc::channel::<()>();
            let (resume, held) = std::sync::mpsc::channel::<()>();
            staging::REMOVAL_HOOKS.lock().unwrap().push((
                id.clone(),
                staging::RemovalPoint::Read,
                Box::new(move || {
                    let _ = entered.send(());
                    let _ = held.recv_timeout(Duration::from_secs(10));
                }),
            ));
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            assert_eq!(f.answered(&queue)["accepted"], true);
            let end = Instant::now() + Duration::from_secs(10);
            while reading.try_recv().is_err() {
                assert!(Instant::now() < end, "the removal never started");
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                std::thread::sleep(Duration::from_millis(2));
            }
            // The runtime goes while its worker still reads.
            let old = std::mem::replace(
                &mut f.runtime,
                runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap(),
            );
            drop(old);
            let during = Arc::strong_count(&f.owner);
            std::thread::sleep(Duration::from_millis(400));
            resume.send(()).unwrap();
            let end = Instant::now() + Duration::from_secs(10);
            while Arc::strong_count(&f.owner) >= during {
                assert!(Instant::now() < end, "the worker never let go");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(
                Arc::strong_count(&f.owner),
                during - 1,
                "the worker held one loan"
            );
            assert!(f.input_path(&original).exists(), "never unlinked late");
            assert_eq!(f.state(&original), "held", "still charged");
        }

        /// A permit that expires, or a writer revoked, while preparation runs:
        /// nothing is admitted, and the exact published input is removed
        /// before its reservation is released.
        #[test]
        fn a_permit_expired_or_a_writer_revoked_during_preparation_admits_nothing() {
            for revoke in [false, true] {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing was admitted"));
                let (original, blobs, _) = f.referenced(&[shot(7, 3000)]);
                let staged = f.stage(&original, &blobs);
                staging::DELAYS
                    .lock()
                    .unwrap()
                    .push((original.plan.request_id.clone(), Duration::from_millis(400)));
                let authority = if revoke {
                    ui_authority::Validated::fixture(&Fixture::request(&original))
                } else {
                    ui_authority::Validated::fixture_expiring(
                        &Fixture::request(&original),
                        Instant::now() + Duration::from_millis(150),
                    )
                };
                let queue = f.send_reference(&original, authority);
                assert_eq!(f.state(&original), "reserved", "reserved before preparing");
                if revoke {
                    f.store
                        .db
                        .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                        .unwrap();
                }
                let answer = f.answered(&queue);
                assert_eq!(answer["accepted"], false, "{revoke}: {answer}");
                assert!(ledger::load(&f.store.db, &original.plan.request_id)
                    .unwrap()
                    .is_none());
                f.until("released", |f| f.state(&original) == "released");
                assert!(!f.input_path(&original).exists(), "the exact input went");
                assert!(staged.exists(), "the client's copies stay for its review");
                assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            }
        }


        /// Actual ReferenceV1 preparation reaches operation admission with a full
        /// valid roster. Its separate input is removed before that input charge
        /// returns; no new reference original/negative ABI or retry is created.
        #[test]
        fn operation_capacity_after_reference_preparation_keeps_input_and_original_charges_distinct(
        ) {
            let target = Arc::new(std::sync::Mutex::new(None::<String>));
            let selected = target.clone();
            let ran = Arc::new(AtomicUsize::new(0));
            let counted = ran.clone();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = std::sync::Mutex::new(released);
            let mut f = Fixture::new(move |plan, _, _, _, _| {
                if let Some(reference) = selected.lock().unwrap().as_ref() {
                    assert_ne!(
                        &plan.request_id, reference,
                        "refused reference never executes"
                    );
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
                Ended::Unknown("original_fixture_held")
            });
            for _ in 0..32 {
                let v = f.original();
                assert!(f.admit(&v).accepted);
            }
            let protected = super::super::reservation_budget::budget(&f.store.db);
            assert_eq!((protected.0, protected.2), (32, 32));
            let (original, blobs, _) = f.referenced(&[shot(31, 3000)]);
            *target.lock().unwrap() = Some(original.plan.request_id.clone());
            let staged = f.stage(&original, &blobs);
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            assert_eq!(f.state(&original), "reserved");
            assert_eq!(
                inputs::totals(&f.store.db).unwrap(),
                (1, original.input_len().unwrap())
            );
            let answer = f.answered(&queue);
            assert_eq!(answer["accepted"], false);
            assert_eq!(answer["code"], "task_message_unavailable");
            assert!(ledger::load(&f.store.db, &original.plan.request_id)
                .unwrap()
                .is_none());
            assert_eq!(
                super::super::reservation_budget::budget(&f.store.db),
                protected,
                "old original custody retains operation charges"
            );
            f.until("exact refused input released", |f| {
                f.state(&original) == "released"
            });
            assert!(!f.input_path(&original).exists());
            assert!(staged.exists(), "staged client copies remain for review");
            assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            assert_eq!(
                super::super::reservation_budget::budget(&f.store.db),
                protected
            );
            f.runtime.request_stop();
            let _ = release.send(());
            f.until("original resources settle", |f| f.runtime.drained());
            assert_eq!(
                super::super::reservation_budget::budget(&f.store.db),
                (0, 0, 0)
            );
            assert!(ran.load(Ordering::SeqCst) <= 1);
            assert!(ledger::load(&f.store.db, &original.plan.request_id)
                .unwrap()
                .is_none());
        }

        #[test]
        fn the_input_budget_and_a_missing_staging_refuse_before_any_input() {
            let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing was admitted"));
            let (original, blobs, _) = f.referenced(&[shot(9, 3000)]);
            let staged = f.stage(&original, &blobs);
            f.store
                .db
                .execute(
                    "UPDATE desktop_task_message_input_budget SET bytes=?1",
                    [inputs::BYTES],
                )
                .unwrap();
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            assert_eq!(f.answered(&queue)["accepted"], false);
            assert!(f.input(&original).is_none(), "no reservation at all");
            assert!(staged.exists());
            f.store
                .db
                .execute("UPDATE desktop_task_message_input_budget SET bytes=0", [])
                .unwrap();
            // Nothing staged: refused after its reservation, released.
            let (other, _, _) = f.referenced(&[shot(11, 3000)]);
            let queue = f.send_reference(
                &other,
                ui_authority::Validated::fixture(&Fixture::request(&other)),
            );
            let answer = f.answered(&queue);
            assert_eq!(answer["accepted"], false);
            assert_eq!(answer["code"], staging::UNAVAILABLE);
            // Released only once the sweep saw that no input exists.
            f.until("released", |f| f.state(&other) == "released");
            assert!(!f.input_path(&other).exists());
            assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            // The same request again: an existing record, a distinct and
            // non-definite refusal, never a second reservation.
            let queue = f.send_reference(
                &other,
                ui_authority::Validated::fixture(&Fixture::request(&other)),
            );
            let answer = f.answered(&queue);
            assert_eq!(
                (answer["accepted"].clone(), answer["code"].clone()),
                (json!(false), json!(staging::EXISTS))
            );
            assert_eq!(f.state(&other), "released");
        }

        /// A preparation that fails after its input was published: the
        /// identity unread, the removal failed or the worker panicked. The
        /// input is never left untracked: it becomes an exact orphan, still
        /// charged, and nothing is admitted.
        #[test]
        fn a_preparation_failing_after_publication_keeps_its_input_as_an_exact_orphan() {
            for fault in [
                staging::Fault::Identity,
                staging::Fault::Removal,
                staging::Fault::Panic,
            ] {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing was admitted"));
                let (original, blobs, _) = f.referenced(&[shot(19, 3000)]);
                f.stage(&original, &blobs);
                staging::FAULTS
                    .lock()
                    .unwrap()
                    .push((original.plan.request_id.clone(), fault));
                let queue = f.send_reference(
                    &original,
                    ui_authority::Validated::fixture(&Fixture::request(&original)),
                );
                let answer = f.answered(&queue);
                assert_eq!(answer["accepted"], false, "{fault:?}: {answer}");
                assert!(f.input_path(&original).exists(), "{fault:?}");
                f.until("orphaned", |f| f.state(&original) == "orphaned");
                assert!(f.input_path(&original).exists(), "{fault:?}: kept exactly");
                assert_eq!(
                    inputs::totals(&f.store.db).unwrap(),
                    (1, original.input_len().unwrap()),
                    "{fault:?}: still charged"
                );
                assert!(ledger::load(&f.store.db, &original.plan.request_id)
                    .unwrap()
                    .is_none());
            }
        }

        /// A second request for an original still being prepared gets its own
        /// refusal and never a second reservation; the first goes on.
        #[test]
        fn a_second_request_during_preparation_is_refused_as_preparing() {
            let mut f = Fixture::new(|_, _, _, _, _| Ended::NotStarted);
            let (original, blobs, _) = f.referenced(&[shot(23, 3000)]);
            f.stage(&original, &blobs);
            staging::DELAYS
                .lock()
                .unwrap()
                .push((original.plan.request_id.clone(), Duration::from_millis(300)));
            let first = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            let second = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            let answer = f.answered(&second);
            assert_eq!(
                (answer["accepted"].clone(), answer["code"].clone()),
                (json!(false), json!("task_message_preparing"))
            );
            assert_eq!(f.state(&original), "reserved");
            assert_eq!(f.answered(&first)["accepted"], true);
            assert_eq!(f.state(&original), "held");
        }

        #[test]
        fn a_restart_keeps_a_published_unadmitted_input_as_an_exact_orphan() {
            let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
            let (published, blobs, _) = f.referenced(&[shot(13, 3000)]);
            let (unpublished, _, _) = f.referenced(&[shot(17, 3000)]);
            // The cut: reserved, then the input published, then the process
            // ended before admission; and one that ended before publishing.
            for original in [&published, &unpublished] {
                let tx = f
                    .store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                inputs::reserve(
                    &tx,
                    &original.plan.request_id,
                    original.input_len().unwrap(),
                    &original.plan.input_sha256,
                    1,
                )
                .unwrap();
                tx.commit().unwrap();
            }
            f.stage(&published, &blobs);
            let roots = staging::Roots::open(&f.location.root).unwrap();
            let prepared =
                staging::prepare(&roots, &published, Instant::now() + Duration::from_secs(10))
                    .unwrap();
            let charged = inputs::totals(&f.store.db).unwrap();
            f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
            f.runtime.fixture_executor(
                MACHINE,
                Arc::new(|_, _, _, _, _| panic!("restart never runs it")),
            );
            f.runtime.fixture_mac_fence(false);
            f.until("recovered", |f| {
                f.state(&published) == "orphaned" && f.state(&unpublished) == "released"
            });
            assert!(
                f.input_path(&published).exists(),
                "the exact original stays"
            );
            let mut kept = Vec::new();
            std::io::Read::read_to_end(
                &mut std::fs::File::open(f.input_path(&published)).unwrap(),
                &mut kept,
            )
            .unwrap();
            assert_eq!(kept.len() as u64, prepared.input.bytes);
            assert_eq!(
                inputs::totals(&f.store.db).unwrap(),
                (1, published.input_len().unwrap()),
                "the orphan stays charged"
            );
            assert!(charged.0 == 2);
        }

        /// A retired original is not a drained one: its input and both
        /// charges stay until every original pipe ended.
        #[test]
        fn a_retired_but_undrained_original_keeps_its_input_and_both_charges_until_eof() {
            use super::super::message_child::{accept, RETAINING};
            use crate::desktop::custody::{self, Observation};
            let control = tempfile::tempdir().unwrap();
            let socket = control.path().join("c");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let path = socket.to_str().unwrap().to_owned();
            let mut f = Fixture::new(move |_, _, input, lock, resources| {
                super::super::child::run(
                    std::path::Path::new("/usr/bin/env"),
                    &[
                        "python3".into(),
                        "-c".into(),
                        RETAINING.into(),
                        path.clone(),
                    ],
                    input,
                    lock,
                    resources,
                    Instant::now() + Duration::from_millis(1500),
                    |_| Ok(()),
                )
            });
            let (original, blobs, _) = f.referenced(&[shot(19, 200 << 10)]);
            f.stage(&original, &blobs);
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            assert_eq!(f.answered(&queue)["accepted"], true);
            let release = accept(&listener, || {
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
            });
            f.until("logical Unknown", |f| {
                f.row(&original).receipt.code == "task_message_output_retained"
            });
            let record = f.row(&original).custody.unwrap();
            f.store
                .db
                .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                .unwrap();
            f.runtime.request_stop();
            for _ in 0..40 {
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(!f.runtime.drained());
            assert_eq!(
                ledger::undrained(&f.store.db).unwrap(),
                1,
                "operation charge"
            );
            assert_eq!(f.state(&original), "held", "storage charge");
            assert!(f.input_path(&original).exists());
            assert!(matches!(
                custody::probe(&f.location, &record),
                Observation::Held
            ));
            // A restart holds no consent, preparation or worker of the
            // original: only its physical drain may remove the input.
            f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
            f.runtime.fixture_executor(
                MACHINE,
                Arc::new(|_, _, _, _, _| panic!("a restart never runs it")),
            );
            f.runtime.fixture_mac_fence(false);
            for _ in 0..40 {
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(f.state(&original), "held", "kept across the restart");
            assert!(f.input_path(&original).exists());
            drop(release);
            // Drained after the restart, but its original object is gone with
            // the old process: kept, failed and charged for explicit review.
            f.until("drained and kept for review", |f| {
                !f.row(&original).custody.is_none()
                    && f.row(&original).receipt.drained
                    && f.state(&original) == "failed"
            });
            assert!(f.input_path(&original).exists());
            assert_eq!(inputs::totals(&f.store.db).unwrap().0, 1);
        }

        #[test]
        fn the_macos_fence_refuses_a_reference_before_any_reservation() {
            let mut f = Fixture::new(|_, _, _, _, _| panic!("no fenced child"));
            f.runtime.fixture_mac_fence(true);
            let (original, blobs, _) = f.referenced(&[shot(23, 3000)]);
            f.stage(&original, &blobs);
            let queue = f.send_reference(
                &original,
                ui_authority::Validated::fixture(&Fixture::request(&original)),
            );
            let answer = f.answered(&queue);
            assert_eq!(answer["code"], runtime::MAC_UNAVAILABLE);
            assert!(f.input(&original).is_none());
        }

        /// The explicit review and release of charged custody inputs
        /// (zerus-pi2k9), under G1's conditions.
        mod review {
            use super::*;
            use zerus_client_core::desktop::task_message::review::{
                Cursor, List, Observation, Outcome, Page, Release, Released, Version, LIST_OP,
            };

            impl Fixture {
                /// A published but never admitted input: an exact orphan.
                fn orphan(&mut self, seed: u8, created: i64) -> Verified {
                    let (original, blobs, _) = self.referenced(&[shot(seed, 3000)]);
                    let id = original.plan.request_id.clone();
                    let tx = self
                        .store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .unwrap();
                    inputs::reserve(
                        &tx,
                        &id,
                        original.input_len().unwrap(),
                        &original.plan.input_sha256,
                        created,
                    )
                    .unwrap();
                    tx.commit().unwrap();
                    self.stage(&original, &blobs);
                    let roots = staging::Roots::open(&self.location.root).unwrap();
                    drop(
                        staging::prepare(
                            &roots,
                            &original,
                            Instant::now() + Duration::from_secs(10),
                        )
                        .unwrap(),
                    );
                    let tx = self
                        .store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .unwrap();
                    inputs::recover_reserved(&tx, &id, true).unwrap();
                    tx.commit().unwrap();
                    assert_eq!(self.state(&original), "orphaned");
                    original
                }
                /// One page. The process-wide listing bound is shared by
                /// parallel tests, so a busy refusal is read again.
                fn list(&mut self, after: Option<Cursor>) -> Page {
                    let list = List {
                        op: LIST_OP.into(),
                        scope: wire::Scope {
                            configuration: self.location.configuration.clone(),
                            service_epoch: self.epoch.clone(),
                        },
                        after,
                    };
                    let end = Instant::now() + Duration::from_secs(10);
                    loop {
                        let queue = Arc::new(Queue::default());
                        self.runtime.task_message_inputs(
                            &self.store,
                            &json!(list),
                            queue.clone(),
                            1,
                        );
                        let page: Page = serde_json::from_value(self.answered(&queue)).unwrap();
                        assert!(page.matches(&list), "{page:?}");
                        if page.code != "task_message_input_review_busy" {
                            return page;
                        }
                        assert!(Instant::now() < end, "listings stayed busy");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                fn observed(&mut self, original: &Verified) -> Observation {
                    let page = self.list(None);
                    page.entries
                        .into_iter()
                        .find(|e| e.request_id == original.plan.request_id)
                        .expect("listed")
                        .observed
                }
                fn release_of(
                    &self,
                    original: &Verified,
                    state: &str,
                    observed: Option<Version>,
                ) -> Release {
                    Release {
                        scope: wire::Scope {
                            configuration: self.location.configuration.clone(),
                            service_epoch: self.epoch.clone(),
                        },
                        release_id: uuid::Uuid::new_v4().to_string(),
                        request_id: original.plan.request_id.clone(),
                        state: state.into(),
                        bytes: original.input_len().unwrap(),
                        input_sha256: original.plan.input_sha256.clone(),
                        observed,
                    }
                }
                fn send_release(
                    &mut self,
                    release: &Release,
                    expires: Option<Instant>,
                ) -> Arc<Queue> {
                    let request = json!(
                        zerus_client_core::desktop::ui_requests::Request::TaskMessageInputRelease {
                            release: Box::new(release.clone()),
                        }
                    );
                    let authority = match expires {
                        Some(at) => ui_authority::Validated::fixture_expiring(&request, at),
                        None => ui_authority::Validated::fixture(&request),
                    };
                    let queue = Arc::new(Queue::default());
                    self.runtime.task_message_input_release(
                        &mut self.store,
                        authority,
                        Some(self.exclusion.clone()),
                        queue.clone(),
                        1,
                    );
                    queue
                }
                fn release(&mut self, release: &Release) -> Released {
                    let queue = self.send_release(release, None);
                    let value = self.answered(&queue);
                    let released: Released = serde_json::from_value(value).unwrap();
                    assert!(released.matches(release), "{released:?}");
                    released
                }
            }
            fn present(observation: Observation) -> Version {
                match observation {
                    Observation::Present { version } => version,
                    other => panic!("not present: {other:?}"),
                }
            }
            /// Runs `hook` on the review's worker at `point`, for `original`.
            fn at(
                original: &Verified,
                point: staging::ReviewPoint,
                hook: impl FnOnce() + Send + 'static,
            ) {
                staging::REVIEW_HOOKS.lock().unwrap().push((
                    original.plan.request_id.clone(),
                    point,
                    Box::new(hook),
                ));
            }

            #[test]
            fn an_orphan_is_removed_only_as_the_exact_reviewed_object() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(29, 1);
                let path = f.input_path(&original);
                let version = present(f.observed(&original));
                // Same bytes under the same name after the review: another
                // object, never deleted from this review.
                let bytes = std::fs::read(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
                std::fs::write(&path, &bytes).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
                let stale = f.release_of(&original, "orphaned", Some(version));
                let answer = f.release(&stale);
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, staging::REVIEW_CHANGED)
                );
                assert!(path.exists());
                assert_eq!(f.state(&original), "orphaned");
                // A fresh review of the current object removes exactly it.
                let version = present(f.observed(&original));
                let answer = f.release(&f.release_of(&original, "orphaned", Some(version)));
                assert_eq!(answer.outcome, Outcome::Removed, "{answer:?}");
                assert!(!path.exists());
                let record = f.input(&original).unwrap();
                assert_eq!(record.state, "removed");
                assert_eq!(record.identity, Some((version.device, version.inode)));
                assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
                assert!(f.list(None).entries.is_empty());
                // Never released twice, never admitted again by that ID.
                let again = f.release(&f.release_of(&original, "orphaned", Some(version)));
                assert_eq!(again.outcome, Outcome::Kept);
                let queue = f.send_reference(
                    &original,
                    ui_authority::Validated::fixture(&Fixture::request(&original)),
                );
                assert_eq!(f.answered(&queue)["code"], staging::EXISTS);
                assert!(ledger::load(&f.store.db, &original.plan.request_id)
                    .unwrap()
                    .is_none());
            }

            #[test]
            fn status_counts_the_reviewable_copies_and_a_full_budget() {
                let inputs_status = |f: &Fixture| {
                    crate::desktop::task_message::summary(&f.store.db).unwrap()["inputs"].clone()
                };
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                assert_eq!(
                    inputs_status(&f),
                    json!({"charged": 0, "bytes": 0, "review": 0, "full": false})
                );
                let original = f.orphan(35, 1);
                let other = f.orphan(37, 2);
                let bytes = original.input_len().unwrap() + other.input_len().unwrap();
                assert_eq!(
                    inputs_status(&f),
                    json!({"charged": 2, "bytes": bytes, "review": 2, "full": false})
                );
                // A held record is charged but not for review.
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_task_message_inputs SET state='held',device=1,inode=1
                         WHERE request_id=?1",
                        [&other.plan.request_id],
                    )
                    .unwrap();
                assert_eq!(inputs_status(&f)["review"], 1);
                // Either cap reached pauses every further input.
                for (rows, bytes) in [(inputs::ROWS, 1), (2, inputs::BYTES)] {
                    f.store
                        .db
                        .execute(
                            "UPDATE desktop_task_message_input_budget SET rows=?1,bytes=?2",
                            [rows, bytes],
                        )
                        .unwrap();
                    assert_eq!(inputs_status(&f)["full"], true, "{rows} {bytes}");
                }
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_task_message_input_budget SET rows=?1,bytes=?2",
                        [inputs::ROWS - 1, inputs::BYTES - 1],
                    )
                    .unwrap();
                assert_eq!(inputs_status(&f)["full"], false);
            }

            #[test]
            fn a_change_while_hashing_or_a_stop_before_the_unlink_removes_nothing() {
                for case in ["in place", "stop", "revoke", "swapped"] {
                    let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                    let original = f.orphan(31, 1);
                    let other = f.orphan(33, 2);
                    let path = f.input_path(&original);
                    let version = present(f.observed(&original));
                    let other_version = present(f.observed(&other));
                    let (held, release_hook) = std::sync::mpsc::channel::<()>();
                    let (entered, wait) = std::sync::mpsc::channel::<()>();
                    let mutate = (case == "in place").then(|| path.clone());
                    if case == "swapped" {
                        // Right before the unlink, another object takes the
                        // name: it is never removed.
                        let name = path.clone();
                        at(&original, staging::ReviewPoint::BeforeName, move || {
                            let other = name.with_extension("other");
                            std::fs::write(&other, b"other").unwrap();
                            std::fs::set_permissions(
                                &other,
                                std::fs::Permissions::from_mode(0o600),
                            )
                            .unwrap();
                            std::fs::rename(&other, &name).unwrap();
                        });
                    }
                    at(&original, staging::ReviewPoint::Hashed, move || {
                        if let Some(path) = mutate {
                            // Same length, other bytes, written in place.
                            use std::io::{Seek, Write};
                            let mut file =
                                std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                            file.seek(std::io::SeekFrom::Start(0)).unwrap();
                            file.write_all(b"X").unwrap();
                        }
                        let _ = entered.send(());
                        let _ = release_hook.recv_timeout(Duration::from_secs(10));
                    });
                    let queue =
                        f.send_release(&f.release_of(&original, "orphaned", Some(version)), None);
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    // The worker holds the claim: still charged, not drained,
                    // the slot excludes a second review.
                    assert_eq!(f.runtime.review_finished(), Some(false));
                    assert_eq!(inputs::totals(&f.store.db).unwrap().0, 2);
                    // The one slot excludes a second review, of this record or
                    // of any other.
                    let second = f.release(&f.release_of(&original, "orphaned", Some(version)));
                    assert_eq!(
                        (second.outcome, second.code.as_str()),
                        (Outcome::Kept, "task_message_input_review_busy")
                    );
                    let third = f.release(&f.release_of(&other, "orphaned", Some(other_version)));
                    assert_eq!(
                        (third.outcome, third.code.as_str()),
                        (Outcome::Kept, "task_message_input_review_busy")
                    );
                    assert_eq!(
                        f.runtime.review_finished(),
                        Some(false),
                        "the first claim stays"
                    );
                    match case {
                        "stop" => f.runtime.request_stop(),
                        "revoke" => {
                            f.store
                                .db
                                .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                                .unwrap();
                            f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                        }
                        _ => {}
                    }
                    assert!(!f.runtime.drained(), "{case}: the claim joins the drain");
                    held.send(()).unwrap();
                    let answer: Released = serde_json::from_value(f.answered(&queue)).unwrap();
                    let code = if matches!(case, "in place" | "swapped") {
                        staging::REVIEW_CHANGED
                    } else {
                        staging::REVIEW_STOPPED
                    };
                    assert_eq!(
                        (answer.outcome, answer.code.as_str()),
                        (Outcome::Kept, code),
                        "{case}"
                    );
                    assert!(path.exists(), "{case}: nothing removed");
                    assert_eq!(f.state(&original), "orphaned");
                    assert_eq!(inputs::totals(&f.store.db).unwrap().0, 2);
                }
            }

            #[test]
            fn an_absent_slot_is_released_only_after_a_clean_bounded_scan() {
                use std::os::unix::fs::PermissionsExt;
                let cases = ["clean", "elsewhere", "unsafe", "bound", "churn"];
                for case in cases {
                    let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                    let original = f.orphan(37, 1);
                    let path = f.input_path(&original);
                    let folder = path.parent().unwrap().to_path_buf();
                    match case {
                        "elsewhere" => std::fs::rename(&path, folder.join("moved")).unwrap(),
                        _ => std::fs::remove_file(&path).unwrap(),
                    }
                    match case {
                        "unsafe" => {
                            let fifo = std::ffi::CString::new(
                                folder.join("fifo").as_os_str().as_encoded_bytes(),
                            )
                            .unwrap();
                            // SAFETY: a NUL-terminated path in the private test folder.
                            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                        }
                        "bound" => {
                            for n in 0..=staging::SCAN_ENTRIES {
                                let file = folder.join(format!("filler-{n}"));
                                std::fs::write(&file, b"x").unwrap();
                                std::fs::set_permissions(
                                    &file,
                                    std::fs::Permissions::from_mode(0o600),
                                )
                                .unwrap();
                            }
                        }
                        "churn" => {
                            let back = path.clone();
                            at(&original, staging::ReviewPoint::Scanned, move || {
                                std::fs::write(&back, b"back").unwrap();
                            });
                        }
                        _ => {}
                    }
                    assert_eq!(f.observed(&original), Observation::Absent, "{case}");
                    let answer = f.release(&f.release_of(&original, "orphaned", None));
                    let expected = match case {
                        "clean" => (Outcome::Released, ""),
                        "elsewhere" => (Outcome::Kept, staging::REVIEW_ELSEWHERE),
                        "unsafe" => (Outcome::Kept, staging::REVIEW_UNSAFE),
                        "bound" => (Outcome::Kept, staging::REVIEW_BOUND),
                        _ => (Outcome::Kept, staging::REVIEW_CHANGED),
                    };
                    assert_eq!((answer.outcome, answer.code.as_str()), expected, "{case}");
                    if case == "clean" {
                        assert_eq!(f.state(&original), "released");
                        assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
                        // The released ID never reserves or admits again.
                        let queue = f.send_reference(
                            &original,
                            ui_authority::Validated::fixture(&Fixture::request(&original)),
                        );
                        assert_eq!(f.answered(&queue)["code"], staging::EXISTS);
                    } else {
                        assert_eq!(f.state(&original), "orphaned", "{case}");
                        assert_eq!(inputs::totals(&f.store.db).unwrap().0, 1, "{case}");
                    }
                    if case == "elsewhere" {
                        assert!(
                            folder.join("moved").exists(),
                            "a copy elsewhere is never touched"
                        );
                    }
                }
            }

            #[test]
            fn a_permit_expired_before_the_claim_claims_nothing() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(41, 1);
                let version = present(f.observed(&original));
                // Already expired: refused before anything is read.
                let queue = f.send_release(
                    &f.release_of(&original, "orphaned", Some(version)),
                    Some(Instant::now() - Duration::from_millis(1)),
                );
                assert_eq!(
                    f.answered(&queue)["code"],
                    "task_message_input_review_refused"
                );
                assert_eq!(f.runtime.review_finished(), None, "never claimed");
                // Expiring while BEGIN IMMEDIATE waits for the writer lock:
                // the permit is consumed after BEGIN, so nothing is claimed.
                f.store.db.busy_timeout(Duration::from_secs(3)).unwrap();
                let expires = Instant::now() + Duration::from_millis(400);
                let path = f.location.root.join("state.sqlite3");
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                let blocker = std::thread::spawn(move || {
                    let mut db = rusqlite::Connection::open(path).unwrap();
                    let tx = db
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .unwrap();
                    send.send(()).unwrap();
                    std::thread::sleep(
                        (expires + Duration::from_millis(20))
                            .saturating_duration_since(Instant::now()),
                    );
                    tx.commit().unwrap();
                });
                receive.recv_timeout(Duration::from_secs(1)).unwrap();
                let release = f.release_of(&original, "orphaned", Some(version));
                let queue = f.send_release(&release, Some(expires));
                blocker.join().unwrap();
                let answer: Released = serde_json::from_value(f.answered(&queue)).unwrap();
                assert!(answer.matches(&release));
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, "task_message_input_review_refused")
                );
                assert_eq!(f.runtime.review_finished(), None, "never claimed");
                assert!(f.input_path(&original).exists());
                assert_eq!(f.state(&original), "orphaned");
            }

            #[test]
            fn a_restart_after_the_unlink_leaves_only_a_fresh_absent_review() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(43, 1);
                let version = present(f.observed(&original));
                let _queue =
                    f.send_release(&f.release_of(&original, "orphaned", Some(version)), None);
                let end = Instant::now() + Duration::from_secs(10);
                while f.runtime.review_finished() != Some(true) {
                    assert!(Instant::now() < end, "the worker never finished");
                    std::thread::sleep(Duration::from_millis(2));
                }
                // The process ends before Phase C recorded the effect.
                assert!(!f.input_path(&original).exists());
                f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
                f.runtime
                    .fixture_executor(MACHINE, Arc::new(|_, _, _, _, _| panic!("nothing runs")));
                f.runtime.fixture_mac_fence(false);
                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                assert_eq!(
                    f.state(&original),
                    "orphaned",
                    "never recorded without a review"
                );
                assert_eq!(f.observed(&original), Observation::Absent);
                let answer = f.release(&f.release_of(&original, "orphaned", None));
                assert_eq!(answer.outcome, Outcome::Released);
                assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            }

            /// A failed record's original must have drained exactly, here
            /// one admitted but neither run nor drained, held by no consent
            /// of this process (as after a restart).
            #[test]
            fn a_failed_record_is_refused_until_its_original_drained() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.original();
                assert!(f.admit(&original).accepted);
                f.runtime = runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap();
                f.runtime
                    .fixture_executor(MACHINE, Arc::new(|_, _, _, _, _| panic!("nothing runs")));
                f.runtime.fixture_mac_fence(false);
                let id = original.plan.request_id.clone();
                let tx = f
                    .store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                inputs::reserve(&tx, &id, 10, &"e".repeat(64), 1).unwrap();
                inputs::hold(&tx, &id, (1, 2)).unwrap();
                tx.execute(
                    "UPDATE desktop_task_message_inputs SET state='failed' WHERE request_id=?1",
                    [&id],
                )
                .unwrap();
                tx.commit().unwrap();
                let mut release = Release {
                    scope: wire::Scope {
                        configuration: f.location.configuration.clone(),
                        service_epoch: f.epoch.clone(),
                    },
                    release_id: uuid::Uuid::new_v4().to_string(),
                    request_id: id.clone(),
                    state: "failed".into(),
                    bytes: 10,
                    input_sha256: "e".repeat(64),
                    observed: None,
                };
                let answer = f.release(&release);
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, "task_message_input_review_refused")
                );
                assert_eq!(f.input(&original).unwrap().state, "failed");
                assert_eq!(inputs::totals(&f.store.db).unwrap(), (1, 10));
                // Nor is an orphan's release taken for a failed record's.
                release.release_id = uuid::Uuid::new_v4().to_string();
                release.state = "orphaned".into();
                assert_eq!(f.release(&release).outcome, Outcome::Kept);
                // And an orphan must have no execution row at all.
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_task_message_inputs SET state='orphaned' WHERE request_id=?1",
                        [&id],
                    )
                    .unwrap();
                release.release_id = uuid::Uuid::new_v4().to_string();
                let answer = f.release(&release);
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, "task_message_input_review_refused")
                );
                assert_eq!(f.input(&original).unwrap().state, "orphaned");
            }

            /// A failed record whose original drained: only its recorded
            /// object, unchanged, is removed.
            #[test]
            #[cfg(target_os = "linux")]
            fn a_failed_record_removes_only_its_recorded_object_after_the_drain() {
                let dir = tempfile::tempdir().unwrap();
                let (image, _seen) = replying_image(dir.path());
                let mut f = Fixture::unexecuted();
                f.runtime.fixture_image(&image);
                let (original, blobs, _) = f.referenced(&[shot(53, 3000)]);
                f.stage(&original, &blobs);
                let queue = f.send_reference(
                    &original,
                    ui_authority::Validated::fixture(&Fixture::request(&original)),
                );
                assert_eq!(f.answered(&queue)["accepted"], true);
                // Its removal failed: the sweep never takes a failed record.
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_task_message_inputs SET state='failed' WHERE request_id=?1",
                        [&original.plan.request_id],
                    )
                    .unwrap();
                f.until("drained", |f| f.row(&original).receipt.drained);
                let recorded = f.input(&original).unwrap().identity.unwrap();
                let path = f.input_path(&original);
                assert!(path.exists());
                let entry = f
                    .list(None)
                    .entries
                    .into_iter()
                    .find(|e| e.request_id == original.plan.request_id)
                    .unwrap();
                assert_eq!(entry.recorded, Some(recorded));
                let version = present(entry.observed);
                // The same bytes as another object are not the recorded one.
                let bytes = std::fs::read(&path).unwrap();
                let aside = path.with_extension("aside");
                std::fs::rename(&path, &aside).unwrap();
                std::fs::write(&path, &bytes).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
                let other = present(f.observed(&original));
                let answer = f.release(&f.release_of(&original, "failed", Some(other)));
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, "task_message_input_review_refused")
                );
                std::fs::remove_file(&path).unwrap();
                std::fs::rename(&aside, &path).unwrap();
                // Renaming changed its status time: the earlier review is
                // stale, so a fresh one names the recorded object again.
                assert_eq!(
                    f.release(&f.release_of(&original, "failed", Some(version)))
                        .outcome,
                    Outcome::Kept
                );
                let version = present(f.observed(&original));
                assert_eq!((version.device, version.inode), recorded);
                let answer = f.release(&f.release_of(&original, "failed", Some(version)));
                assert_eq!(answer.outcome, Outcome::Removed, "{answer:?}");
                assert!(!path.exists());
                let record = f.input(&original).unwrap();
                assert_eq!(
                    (record.state.as_str(), record.identity),
                    ("removed", Some(recorded))
                );
                assert_eq!(inputs::totals(&f.store.db).unwrap(), (0, 0));
            }

            /// Any input record, even a released one, keeps its request ID
            /// from a new inline admission as well.
            #[test]
            fn an_input_record_keeps_its_request_id_from_inline_admission() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.original();
                let tx = f
                    .store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                inputs::reserve(&tx, &original.plan.request_id, 10, &"e".repeat(64), 1).unwrap();
                inputs::recover_reserved(&tx, &original.plan.request_id, false).unwrap();
                tx.commit().unwrap();
                assert_eq!(f.input(&original).unwrap().state, "released");
                assert!(!f.admit(&original).accepted);
                assert!(ledger::load(&f.store.db, &original.plan.request_id)
                    .unwrap()
                    .is_none());
            }

            #[test]
            fn a_listing_is_one_bounded_page_with_a_stable_cursor() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let originals: Vec<Verified> = (0..18)
                    .map(|n| f.orphan(47 + n as u8, 10 + n as i64))
                    .collect();
                let first = f.list(None);
                assert_eq!(first.entries.len(), 16);
                let cursor = first.next.clone().expect("a second page");
                // A record that arrives later is listed after, never twice.
                let late = f.orphan(99, 100);
                let second = f.list(Some(cursor));
                let ids: Vec<&str> = first
                    .entries
                    .iter()
                    .chain(second.entries.iter())
                    .map(|e| e.request_id.as_str())
                    .collect();
                let mut expected: Vec<&str> = originals
                    .iter()
                    .map(|o| o.plan.request_id.as_str())
                    .collect();
                expected.push(&late.plan.request_id);
                assert_eq!(ids, expected);
                assert!(second.next.is_none());
            }

            /// A reviewed object whose version matches but whose bytes are not
            /// the record's is never removed (review-q2's R1).
            #[test]
            fn a_reviewed_object_with_other_bytes_is_kept() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(71, 1);
                let path = f.input_path(&original);
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[0] ^= 1;
                {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .write_all(&bytes)
                        .unwrap();
                }
                // The review observes the rewritten object as it now is.
                let version = present(f.observed(&original));
                let answer = f.release(&f.release_of(&original, "orphaned", Some(version)));
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, staging::REVIEW_CHANGED)
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes, "never removed");
                assert_eq!(f.state(&original), "orphaned");
                assert_eq!(inputs::totals(&f.store.db).unwrap().0, 1);
            }

            /// An in-place change between the post-read recheck and the final
            /// name observation keeps the object (review-q2's R6).
            #[test]
            fn an_in_place_change_before_the_name_check_is_kept() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(73, 1);
                let path = f.input_path(&original);
                let version = present(f.observed(&original));
                let target = path.clone();
                at(&original, staging::ReviewPoint::BeforeName, move || {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&target)
                        .unwrap()
                        .write_all(b"X")
                        .unwrap();
                });
                let answer = f.release(&f.release_of(&original, "orphaned", Some(version)));
                assert_eq!(
                    (answer.outcome, answer.code.as_str()),
                    (Outcome::Kept, staging::REVIEW_CHANGED)
                );
                assert!(path.exists(), "never removed");
                assert_eq!(f.state(&original), "orphaned");
            }

            /// A listing whose worker cannot start still answers, and the
            /// listing bound is restored.
            #[test]
            fn a_listing_whose_worker_never_started_answers_a_refusal() {
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let original = f.orphan(75, 1);
                runtime::LISTING_SPAWN_FAILS.with(|fail| fail.set(true));
                let refused = f.list(None);
                assert_eq!(refused.code, "task_message_inputs_unavailable");
                assert!(refused.entries.is_empty() && refused.next.is_none());
                let listed = f.list(None);
                assert_eq!(listed.code, "");
                assert_eq!(listed.entries[0].request_id, original.plan.request_id);
            }

            /// The open-record queries search the open-state partial index
            /// on the first and every cursor page: a retained terminal history
            /// adds no work.
            #[test]
            fn open_record_queries_search_the_open_index_and_never_the_history() {
                use rusqlite::ToSql;
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                let a = f.orphan(51, 2000);
                let b = f.orphan(53, 2001);
                let cursor = (2000i64, a.plan.request_id.clone());
                let first: [&dyn ToSql; 3] = [&None::<i64>, &"", &17];
                let next: [&dyn ToSql; 3] = [&cursor.0, &cursor.1, &17];
                let queries: [(&str, &str, &[&dyn ToSql]); 5] = [
                    ("first page", inputs::REVIEWABLE, &first),
                    ("cursor page", inputs::REVIEWABLE, &next),
                    ("in state", inputs::IN_STATE, &[&"reserved", &4]),
                    ("held drained", inputs::HELD_DRAINED, &[&4]),
                    ("review count", inputs::REVIEW_COUNT, &[]),
                ];
                for (name, sql, params) in queries {
                    let mut plan = f
                        .store
                        .db
                        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                        .unwrap();
                    let steps: Vec<String> = plan
                        .query_map(params, |r| r.get::<_, String>(3))
                        .unwrap()
                        .map(Result::unwrap)
                        .collect();
                    assert!(
                        steps
                            .iter()
                            .any(|s| s.contains("INDEX desktop_task_message_inputs_open (state=?)")),
                        "{name}: {steps:?}"
                    );
                    assert!(
                        !steps
                            .iter()
                            .any(|s| s.starts_with("SCAN desktop_task_message_inputs")
                                || s.starts_with("SCAN i")),
                        "{name}: {steps:?}"
                    );
                }
                // The work each read does, in SQLite VM steps, before and
                // after 4096 retained terminal records.
                let work = |db: &rusqlite::Connection, sql: &str, params: &[&dyn ToSql]| {
                    let count = Arc::new(AtomicUsize::new(0));
                    let counter = count.clone();
                    db.progress_handler(
                        1,
                        Some(move || {
                            counter.fetch_add(1, Ordering::Relaxed);
                            false
                        }),
                    );
                    let mut query = db.prepare(sql).unwrap();
                    let rows = query
                        .query_map(params, |r| r.get::<_, String>(0))
                        .unwrap()
                        .count();
                    db.progress_handler(0, None::<fn() -> bool>);
                    (rows, count.load(Ordering::Relaxed))
                };
                // The same listing without the index predicate: the probe
                // sees a scan.
                let scanning =
                    inputs::REVIEWABLE.replace("state NOT IN('removed','released') AND ", "");
                let mut before = Vec::new();
                for sql in [inputs::REVIEWABLE, scanning.as_str()] {
                    before.push(work(&f.store.db, sql, &first));
                    before.push(work(&f.store.db, sql, &next));
                }
                let tx = f
                    .store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                for n in 0..4096i64 {
                    tx.execute(
                        "INSERT INTO desktop_task_message_inputs
                         (request_id,bytes,input_sha256,device,inode,state,created)
                         VALUES(?1,1,?2,1,?3,?4,?5)",
                        rusqlite::params![
                            format!("{n:08x}-0000-4000-8000-000000000000"),
                            "a".repeat(64),
                            n,
                            if n % 2 == 0 { "removed" } else { "released" },
                            n
                        ],
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
                let mut after = Vec::new();
                for sql in [inputs::REVIEWABLE, scanning.as_str()] {
                    after.push(work(&f.store.db, sql, &first));
                    after.push(work(&f.store.db, sql, &next));
                }
                assert_eq!((before[0].0, before[1].0), (2, 1));
                assert_eq!((after[0].0, after[1].0), (2, 1));
                for page in 0..2 {
                    assert!(
                        after[page].1 <= before[page].1 + 8,
                        "page {page}: {} steps after, {} before",
                        after[page].1,
                        before[page].1
                    );
                    assert!(
                        after[page + 2].1 > before[page + 2].1 + 4096,
                        "the probe sees a scan: {:?} {:?}",
                        after[page + 2],
                        before[page + 2]
                    );
                }
                let listed: Vec<String> = f
                    .list(None)
                    .entries
                    .into_iter()
                    .map(|e| e.request_id)
                    .collect();
                assert_eq!(listed, [a.plan.request_id, b.plan.request_id]);
            }

            /// Every final filesystem observation comes before the last
            /// fence and deadline: a stop, a revoke or a deadline that passes
            /// after them still removes and releases nothing.
            #[test]
            fn a_stop_revoke_or_deadline_after_the_final_observations_changes_nothing() {
                for absent in [false, true] {
                    for case in ["stop", "revoke", "deadline", "late scan", "late empty scan"] {
                        if !absent && case.starts_with("late") {
                            continue;
                        }
                        let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                        let original = f.orphan(57, 1);
                        let path = f.input_path(&original);
                        let id = original.plan.request_id.clone();
                        if absent && case != "late empty scan" {
                            // A same-size, other-bytes entry: the scan's last
                            // and only entry.
                            let mut bytes = std::fs::read(&path).unwrap();
                            bytes[0] ^= 1;
                            let other = path.with_file_name("other");
                            std::fs::write(&other, bytes).unwrap();
                            std::fs::set_permissions(
                                &other,
                                std::fs::Permissions::from_mode(0o600),
                            )
                            .unwrap();
                        }
                        let observed = if absent {
                            std::fs::remove_file(&path).unwrap();
                            assert_eq!(f.observed(&original), Observation::Absent);
                            None
                        } else {
                            Some(present(f.observed(&original)))
                        };
                        let limit = Duration::from_millis(400);
                        if matches!(case, "deadline" | "late scan" | "late empty scan") {
                            staging::REVIEW_TIMES
                                .lock()
                                .unwrap()
                                .push((id.clone(), limit));
                        }
                        let (entered, wait) = std::sync::mpsc::channel::<()>();
                        let (resume, held) = std::sync::mpsc::channel::<()>();
                        let point = if case.starts_with("late") {
                            staging::ReviewPoint::Scanned
                        } else {
                            staging::ReviewPoint::BeforeUnlink
                        };
                        at(&original, point, move || {
                            let _ = entered.send(());
                            let _ = held.recv_timeout(Duration::from_secs(10));
                        });
                        let started = Instant::now();
                        let queue =
                            f.send_release(&f.release_of(&original, "orphaned", observed), None);
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                        match case {
                            "stop" => f.runtime.request_stop(),
                            "revoke" => {
                                f.store
                                    .db
                                    .execute("UPDATE desktop_writer SET revoked=1 WHERE id=1", [])
                                    .unwrap();
                                f.runtime.poll(&mut f.store, Some(f.exclusion.clone()));
                            }
                            _ => std::thread::sleep(
                                (started + limit + Duration::from_millis(100))
                                    .saturating_duration_since(Instant::now()),
                            ),
                        }
                        resume.send(()).unwrap();
                        let answer: Released = serde_json::from_value(f.answered(&queue)).unwrap();
                        let code = if matches!(case, "stop" | "revoke") {
                            staging::REVIEW_STOPPED
                        } else {
                            staging::REVIEW_BOUND
                        };
                        assert_eq!(
                            (answer.outcome, answer.code.as_str()),
                            (Outcome::Kept, code),
                            "{absent} {case}"
                        );
                        assert_eq!(path.exists(), !absent, "{absent} {case}: nothing removed");
                        assert_eq!(f.state(&original), "orphaned", "{absent} {case}");
                        assert_eq!(inputs::totals(&f.store.db).unwrap().0, 1);
                    }
                }
            }

            /// The absent-slot scan holds the namespace it saw: a copy that
            /// appears, or a scanned object that changes or goes, after the
            /// scan keeps the record, and nothing is touched.
            #[test]
            fn an_absent_slot_scan_refuses_any_change_to_the_scanned_folder() {
                for case in [
                    "copy added",
                    "scanned rewritten",
                    "scanned replaced",
                    "scanned removed",
                ] {
                    let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                    let original = f.orphan(59, 1);
                    let path = f.input_path(&original);
                    let bytes = std::fs::read(&path).unwrap();
                    let mut other_bytes = bytes.clone();
                    other_bytes[0] ^= 1;
                    let other = path.with_file_name("other");
                    std::fs::write(&other, &other_bytes).unwrap();
                    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    std::fs::remove_file(&path).unwrap();
                    assert_eq!(f.observed(&original), Observation::Absent);
                    let copy = path.with_file_name("copy");
                    let (target, staged, content) = (other.clone(), copy.clone(), bytes.clone());
                    at(&original, staging::ReviewPoint::Scanned, move || {
                        let private = |file: &std::path::Path| {
                            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
                                .unwrap()
                        };
                        match case {
                            // The same bytes under a new name.
                            "copy added" => {
                                std::fs::write(&staged, &content).unwrap();
                                private(&staged);
                            }
                            // An already-scanned object takes the bytes in
                            // place, same size.
                            "scanned rewritten" => {
                                use std::io::Write;
                                let mut file = std::fs::OpenOptions::new()
                                    .write(true)
                                    .open(&target)
                                    .unwrap();
                                file.write_all(&content).unwrap();
                            }
                            // Another object with the bytes takes its name.
                            "scanned replaced" => {
                                std::fs::write(&staged, &content).unwrap();
                                private(&staged);
                                std::fs::rename(&staged, &target).unwrap();
                            }
                            _ => std::fs::remove_file(&target).unwrap(),
                        }
                    });
                    let answer = f.release(&f.release_of(&original, "orphaned", None));
                    assert_eq!(
                        (answer.outcome, answer.code.as_str()),
                        (Outcome::Kept, staging::REVIEW_CHANGED),
                        "{case}"
                    );
                    assert_eq!(f.state(&original), "orphaned", "{case}");
                    assert_eq!(inputs::totals(&f.store.db).unwrap().0, 1, "{case}");
                    match case {
                        "copy added" => assert_eq!(std::fs::read(&copy).unwrap(), bytes),
                        "scanned removed" => assert!(!other.exists()),
                        _ => assert_eq!(std::fs::read(&other).unwrap(), bytes, "{case}"),
                    }
                }
            }

            /// A preparation, the only writer of new inputs, and a review's
            /// claim exclude each other, for any request IDs.
            #[test]
            fn a_review_and_a_preparation_never_overlap() {
                // A preparation running: no review is claimed.
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing was admitted"));
                let orphan = f.orphan(61, 1);
                let version = present(f.observed(&orphan));
                let (preparing, blobs, _) = f.referenced(&[shot(63, 3000)]);
                f.stage(&preparing, &blobs);
                staging::DELAYS.lock().unwrap().push((
                    preparing.plan.request_id.clone(),
                    Duration::from_millis(600),
                ));
                let queue = f.send_reference(
                    &preparing,
                    ui_authority::Validated::fixture_expiring(
                        &Fixture::request(&preparing),
                        Instant::now() + Duration::from_millis(200),
                    ),
                );
                assert_eq!(f.state(&preparing), "reserved", "preparing");
                let busy = f.release(&f.release_of(&orphan, "orphaned", Some(version)));
                assert_eq!(
                    (busy.outcome, busy.code.as_str()),
                    (Outcome::Kept, "task_message_input_review_busy")
                );
                assert_eq!(f.runtime.review_finished(), None, "never claimed");
                assert_eq!(f.answered(&queue)["accepted"], false);
                f.until("released", |f| f.state(&preparing) == "released");
                let answer = f.release(&f.release_of(&orphan, "orphaned", Some(version)));
                assert_eq!(answer.outcome, Outcome::Removed, "{answer:?}");

                // A review claimed: no preparation starts until it was
                // recorded.
                let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing was admitted"));
                let orphan = f.orphan(65, 1);
                let version = present(f.observed(&orphan));
                let (entered, wait) = std::sync::mpsc::channel::<()>();
                let (resume, held) = std::sync::mpsc::channel::<()>();
                at(&orphan, staging::ReviewPoint::Hashed, move || {
                    let _ = entered.send(());
                    let _ = held.recv_timeout(Duration::from_secs(10));
                });
                let review =
                    f.send_release(&f.release_of(&orphan, "orphaned", Some(version)), None);
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                let (next, blobs, _) = f.referenced(&[shot(67, 3000)]);
                f.stage(&next, &blobs);
                let queue = f.send_reference(
                    &next,
                    ui_authority::Validated::fixture(&Fixture::request(&next)),
                );
                let refused = f.answered(&queue);
                assert_eq!(
                    (&refused["accepted"], &refused["code"]),
                    (&json!(false), &json!("task_message_unavailable"))
                );
                assert!(f.input(&next).is_none(), "nothing reserved");
                assert!(!f.input_path(&next).exists(), "nothing published");
                resume.send(()).unwrap();
                let answer: Released = serde_json::from_value(f.answered(&review)).unwrap();
                assert_eq!(answer.outcome, Outcome::Removed, "{answer:?}");
            }

            /// A review worker blocked past its runtime and connection keeps
            /// the serve-owner loan until it ends, then lets go of exactly it,
            /// and never unlinks late.
            #[test]
            fn a_blocked_review_worker_keeps_the_owner_after_teardown() {
                for absent in [false, true] {
                    let mut f = Fixture::new(|_, _, _, _, _| panic!("nothing runs"));
                    let original = f.orphan(69, 1);
                    let path = f.input_path(&original);
                    let observed = if absent {
                        std::fs::remove_file(&path).unwrap();
                        None
                    } else {
                        Some(present(f.observed(&original)))
                    };
                    let (entered, wait) = std::sync::mpsc::channel::<()>();
                    let (resume, held) = std::sync::mpsc::channel::<()>();
                    let point = if absent {
                        staging::ReviewPoint::Scanned
                    } else {
                        staging::ReviewPoint::Hashed
                    };
                    at(&original, point, move || {
                        let _ = entered.send(());
                        let _ = held.recv_timeout(Duration::from_secs(10));
                    });
                    let before = Arc::strong_count(&f.owner);
                    let queue =
                        f.send_release(&f.release_of(&original, "orphaned", observed), None);
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    // The connection and the runtime go while the worker runs.
                    drop(queue);
                    let old = std::mem::replace(
                        &mut f.runtime,
                        runtime::Runtime::new(&f.config, &f.epoch, f.owner.clone()).unwrap(),
                    );
                    drop(old);
                    let during = Arc::strong_count(&f.owner);
                    assert_eq!(during, before + 1, "{absent}: the worker's one loan");
                    std::thread::sleep(Duration::from_millis(300));
                    assert_eq!(Arc::strong_count(&f.owner), during, "{absent}: still held");
                    resume.send(()).unwrap();
                    let end = Instant::now() + Duration::from_secs(10);
                    while Arc::strong_count(&f.owner) >= during {
                        assert!(Instant::now() < end, "{absent}: the worker never let go");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    // Its loans (its own and its lease's) are gone: the
                    // fixture's and the new runtime's remain.
                    assert_eq!(Arc::strong_count(&f.owner), 2, "{absent}");
                    assert_eq!(path.exists(), !absent, "{absent}: never unlinked late");
                    assert_eq!(f.state(&original), "orphaned", "{absent}");
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod staging_tests {
    use super::super::staging::{self, Roots};
    use super::*;
    use sha2::Digest;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use zerus_client_core::attachments::Attachment;
    use zerus_client_core::tasks_write::references;

    fn private_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn fresh() -> (tempfile::TempDir, PathBuf, Roots) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("desktop");
        private_dir(&root);
        let roots = Roots::open(&root).unwrap();
        (temp, root, roots)
    }
    fn shot(seed: u8, size: usize) -> Vec<u8> {
        (0..size).map(|n| (n as u8).wrapping_mul(seed)).collect()
    }
    /// A verified reference original whose attachments are `files`, its
    /// blobs and its exact stdin.
    fn original(files: &[Vec<u8>]) -> (Verified, Vec<Vec<u8>>, String) {
        let (verified, blobs, stdin, _) = original_with_record(files);
        (verified, blobs, stdin)
    }
    fn original_with_record(
        files: &[Vec<u8>],
    ) -> (Verified, Vec<Vec<u8>>, String, serde_json::Value) {
        let attachments: Vec<Attachment> = files
            .iter()
            .enumerate()
            .map(|(n, bytes)| Attachment {
                name: format!("shot-{n}.png"),
                mime: "image/png".into(),
                bytes: bytes.clone().into(),
                reference: format!("[shot-{n}.png]"),
            })
            .collect();
        let intent = message_intent("see these", &attachments, "queue", "");
        let request = uuid::Uuid::new_v4().to_string();
        let record = json!({"id": request, "intent": intent,
            "input_sha256": input_digest(&message_input(&intent))});
        let (projection, blobs) = references::project(&record).unwrap();
        let verified = Intent {
            scope: wire::Scope {
                configuration: "a".repeat(64),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            project: "example".into(),
            task_id: TASK.into(),
            request_id: request,
            original: projection.to_string(),
            form: wire::Form::ReferenceV1,
        }
        .verify()
        .unwrap();
        let stdin = message_input(&record["intent"]);
        (verified, blobs, stdin, record)
    }
    /// Stages each blob as `<staging>/<request>/<sha256>`, as the client does.
    fn stage(root: &Path, request: &str, blobs: &[Vec<u8>]) -> PathBuf {
        let dir = root.join(staging::STAGING).join(request);
        private_dir(&dir);
        for blob in blobs {
            let file = dir.join(format!("{:x}", sha2::Sha256::digest(blob)));
            std::fs::write(&file, blob).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        dir
    }
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(20)
    }
    fn input(root: &Path, request: &str) -> PathBuf {
        root.join(staging::INPUTS).join(request)
    }
    /// The exact custody input a removal expects.
    fn expected(prepared: &staging::Prepared) -> staging::Expected {
        staging::Expected {
            identity: prepared.input.identity,
            bytes: prepared.input.bytes,
            sha256: prepared.input.sha256.clone(),
        }
    }

    #[test]
    fn a_staged_original_prepares_its_exact_input_once_and_never_overwrites_it() {
        let (_temp, root, roots) = fresh();
        assert_eq!(roots.staging.path(), root.join(staging::STAGING));
        let (verified, blobs, stdin, record) =
            original_with_record(&[shot(3, 3000), shot(5, 70 << 10)]);
        let request = verified.plan.request_id.clone();
        let dir = stage(&root, &request, &blobs);
        let prepared = staging::prepare(&roots, &verified, soon()).unwrap();
        // Exactly the call of its original record (too large to go inline),
        // and through the kept handle exactly its stdin.
        let call =
            zerus_client_core::tasks_write::message_call("example", TASK, &record, false).unwrap();
        assert_eq!(call.input, stdin);
        assert_eq!(prepared.input.args, call.invocation().args);
        assert_eq!(prepared.input.timeout, call.invocation().timeout);
        let mut kept = vec![0; stdin.len()];
        std::os::unix::fs::FileExt::read_exact_at(&prepared.input.file, &mut kept, 0).unwrap();
        assert_eq!(kept, stdin.as_bytes());
        assert_eq!(prepared.input.bytes, stdin.len() as u64);
        assert_eq!(prepared.input.sha256, verified.plan.input_sha256);
        assert_eq!(
            std::fs::read_to_string(input(&root, &request)).unwrap(),
            stdin
        );
        let metadata = std::fs::metadata(input(&root, &request)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(prepared.staged.len(), 2);
        // The same request never replaces its custody input.
        assert_eq!(
            staging::prepare(&roots, &verified, soon()).err(),
            Some(staging::EXISTS)
        );
        assert_eq!(
            std::fs::read_to_string(input(&root, &request)).unwrap(),
            stdin
        );
        staging::remove_staged(&roots, &request, &prepared.staged).unwrap();
        assert!(!dir.exists(), "exactly its copies, then the empty folder");
        staging::remove_input(
            &roots,
            &request,
            &expected(&prepared),
            &prepared.input.file,
            soon(),
        )
        .unwrap();
        assert!(!input(&root, &request).exists());
        // Its original is no longer linked: a second removal refuses.
        assert!(staging::remove_input(
            &roots,
            &request,
            &expected(&prepared),
            &prepared.input.file,
            soon()
        )
        .is_err());
    }

    #[test]
    fn the_same_screenshot_sent_to_two_tasks_stays_two_independent_copies() {
        let (_temp, root, roots) = fresh();
        let same = shot(7, 9000);
        // Within one message too: the same bytes twice share one staged copy.
        let (first, blobs, _) = original(&[same.clone(), same.clone()]);
        let (second, other, _) = original(std::slice::from_ref(&same));
        stage(&root, &first.plan.request_id, &blobs);
        let kept = stage(&root, &second.plan.request_id, &other);
        let one = staging::prepare(&roots, &first, soon()).unwrap();
        assert_eq!(one.staged.len(), 1);
        staging::remove_staged(&roots, &first.plan.request_id, &one.staged).unwrap();
        assert!(kept.exists(), "another request's copy is never touched");
        let two = staging::prepare(&roots, &second, soon()).unwrap();
        assert_ne!(one.input.identity, two.input.identity);
    }

    #[test]
    fn unsafe_or_wrong_staging_is_refused_and_leaves_no_input() {
        type Edit = fn(&Path, &Path);
        let cases: [(&str, Edit, &str); 9] = [
            (
                "missing folder",
                |dir, _| std::fs::remove_dir_all(dir).unwrap(),
                staging::UNAVAILABLE,
            ),
            (
                "folder symlink",
                |dir, _| {
                    let real = dir.with_extension("real");
                    std::fs::rename(dir, &real).unwrap();
                    std::os::unix::fs::symlink(&real, dir).unwrap();
                },
                staging::UNAVAILABLE,
            ),
            (
                "open folder",
                |dir, _| {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap()
                },
                staging::UNAVAILABLE,
            ),
            (
                "missing copy",
                |_, file| std::fs::remove_file(file).unwrap(),
                staging::UNAVAILABLE,
            ),
            (
                "copy symlink",
                |_, file| {
                    let real = file.with_extension("real");
                    std::fs::rename(file, &real).unwrap();
                    std::os::unix::fs::symlink(&real, file).unwrap();
                },
                staging::UNAVAILABLE,
            ),
            (
                "readable copy",
                |_, file| {
                    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap()
                },
                staging::UNAVAILABLE,
            ),
            (
                "hard link",
                |dir, file| std::fs::hard_link(file, dir.join("another-name")).unwrap(),
                staging::UNAVAILABLE,
            ),
            (
                "special file",
                |_, file| {
                    std::fs::remove_file(file).unwrap();
                    let name = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
                    // SAFETY: a NUL-terminated path in a private test folder.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                },
                staging::UNAVAILABLE,
            ),
            (
                "other bytes",
                |_, file| {
                    let mut bytes = std::fs::read(file).unwrap();
                    bytes[0] ^= 1;
                    std::fs::write(file, bytes).unwrap();
                },
                staging::MISMATCH,
            ),
        ];
        for (name, edit, code) in cases {
            let (_temp, root, roots) = fresh();
            let (verified, blobs, _) = original(&[shot(9, 4096)]);
            let request = verified.plan.request_id.clone();
            let dir = stage(&root, &request, &blobs);
            let file = dir.join(&verified.references[0].sha256);
            edit(&dir, &file);
            // Bounded: an open that blocks (a FIFO without O_NONBLOCK) fails
            // here instead of hanging the suite.
            let (send, done) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = send.send(staging::prepare(&roots, &verified, soon()).err());
            });
            let refused = done
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{name}: preparation blocked"));
            assert_eq!(refused, Some(code), "{name}");
            assert!(!input(&root, &request).exists(), "{name}: no input");
        }
        // A copy of another size is refused as not the original's.
        let (_temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(9, 4096)]);
        let dir = stage(&root, &verified.plan.request_id, &blobs);
        std::fs::write(dir.join(&verified.references[0].sha256), b"short").unwrap();
        assert_eq!(
            staging::prepare(&roots, &verified, soon()).err(),
            Some(staging::MISMATCH)
        );
    }

    #[test]
    fn a_replaced_root_or_a_passed_deadline_prepares_nothing() {
        let (_temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(11, 2048)]);
        let request = verified.plan.request_id.clone();
        stage(&root, &request, &blobs);
        assert_eq!(
            staging::prepare(&roots, &verified, Instant::now()).err(),
            Some(staging::EXPIRED)
        );
        assert!(!input(&root, &request).exists());
        // The staging folder replaced under the same name: refused before
        // any copy is read, even one the held folder still has (here one
        // that would not match).
        let staging_dir = root.join(staging::STAGING);
        std::fs::rename(&staging_dir, root.join("moved")).unwrap();
        let held_copy = root
            .join("moved")
            .join(&request)
            .join(&verified.references[0].sha256);
        std::fs::write(&held_copy, b"not the original").unwrap();
        private_dir(&staging_dir);
        stage(&root, &request, &blobs);
        assert!(!roots.current());
        assert_eq!(
            staging::prepare(&roots, &verified, soon()).err(),
            Some(staging::UNAVAILABLE)
        );
        assert!(!input(&root, &request).exists());
        // An existing directory that is not private is never adopted.
        let (_temp, root, _) = fresh();
        std::fs::set_permissions(
            root.join(staging::INPUTS),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(Roots::open(&root).is_err());
    }

    #[test]
    fn current_refuses_a_replaced_or_linked_root_and_an_unsafe_inputs_folder() {
        let (temp, root, roots) = fresh();
        assert!(roots.current());
        let inputs = root.join(staging::INPUTS);
        std::fs::set_permissions(&inputs, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!roots.current(), "an inputs folder open to others");
        std::fs::set_permissions(&inputs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(roots.current());
        // The desktop root behind a link to itself, then replaced.
        let moved = temp.path().canonicalize().unwrap().join("moved");
        std::fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        assert!(!roots.current(), "a linked root");
        std::fs::remove_file(&root).unwrap();
        std::fs::rename(&moved, &root).unwrap();
        assert!(roots.current());
        std::fs::rename(&root, &moved).unwrap();
        private_dir(&root.join(staging::STAGING));
        private_dir(&root.join(staging::INPUTS));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!roots.current(), "a replaced root");
        let (verified, blobs, _) = original(&[shot(29, 1024)]);
        stage(&moved, &verified.plan.request_id, &blobs);
        assert_eq!(
            staging::prepare(&roots, &verified, soon()).err(),
            Some(staging::UNAVAILABLE)
        );
        assert!(!input(&moved, &verified.plan.request_id).exists());
    }

    #[test]
    fn a_root_replaced_before_the_create_or_expiry_after_the_write_leaves_no_input() {
        // Replaced after the copies were read, right before the create.
        let (temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(31, 2048)]);
        let request = verified.plan.request_id.clone();
        stage(&root, &request, &blobs);
        let (held, swapped) = (root.join(staging::INPUTS), temp.path().join("held-inputs"));
        let (from, to) = (held.clone(), swapped.clone());
        staging::HOOKS.with(|hooks| {
            hooks.borrow_mut().push((
                staging::Point::BeforeCreate,
                Box::new(move || {
                    std::fs::rename(&from, &to).unwrap();
                    private_dir(&from);
                }),
            ))
        });
        assert_eq!(
            staging::prepare(&roots, &verified, soon()).err(),
            Some(staging::UNAVAILABLE)
        );
        assert!(!held.join(&request).exists() && !swapped.join(&request).exists());
        // Expiry after the custody write: exactly that new file goes.
        let (_temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(37, 2048)]);
        let request = verified.plan.request_id.clone();
        stage(&root, &request, &blobs);
        let deadline = Instant::now() + Duration::from_millis(500);
        let written = input(&root, &request);
        let seen = std::rc::Rc::new(std::cell::Cell::new(false));
        let (path, saw) = (written.clone(), seen.clone());
        staging::HOOKS.with(|hooks| {
            hooks.borrow_mut().push((
                staging::Point::AfterWrite,
                Box::new(move || {
                    saw.set(path.exists());
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    std::thread::sleep(Duration::from_millis(20));
                }),
            ))
        });
        assert_eq!(
            staging::prepare(&roots, &verified, deadline).err(),
            Some(staging::EXPIRED)
        );
        assert!(
            seen.get(),
            "the input was written before the deadline passed"
        );
        assert!(!written.exists(), "and removed exactly");
    }

    #[test]
    fn a_custody_input_needs_its_recorded_identity_and_a_private_file() {
        for case in ["hard link", "readable by others", "not the original"] {
            let (_temp, root, roots) = fresh();
            let (verified, blobs, _) = original(&[shot(23, 2048)]);
            let request = verified.plan.request_id.clone();
            stage(&root, &request, &blobs);
            let prepared = staging::prepare(&roots, &verified, soon()).unwrap();
            let wanted = expected(&prepared);
            let path = input(&root, &request);
            let mut held = prepared.input.file.try_clone().unwrap();
            match case {
                "hard link" => std::fs::hard_link(&path, path.with_extension("link")).unwrap(),
                "readable by others" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
                }
                _ => {
                    // The same bytes as another object, held as if it were the
                    // original and named like it.
                    let other = path.with_extension("other");
                    std::fs::write(&other, std::fs::read(&path).unwrap()).unwrap();
                    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    std::fs::rename(&other, &path).unwrap();
                    held = std::fs::File::open(&path).unwrap();
                }
            }
            assert!(
                staging::remove_input(&roots, &request, &wanted, &held, soon()).is_err(),
                "{case}"
            );
            assert!(path.exists(), "{case}: kept for explicit review");
        }
    }

    #[test]
    fn a_removal_checks_the_name_again_after_its_read_within_its_deadline() {
        use staging::RemovalPoint::{BeforeName, BeforeUnlink, Read};
        let cases = [
            ("swapped before the name", BeforeName),
            ("renamed during the read", Read),
            ("rewritten during the read", Read),
            ("late", Read),
            ("late right before the unlink", BeforeUnlink),
            ("exact", Read),
        ];
        for (case, point) in cases {
            let (_temp, root, roots) = fresh();
            let (verified, blobs, _) = original(&[shot(19, 4096)]);
            let request = verified.plan.request_id.clone();
            stage(&root, &request, &blobs);
            let prepared = staging::prepare(&roots, &verified, soon()).unwrap();
            let wanted = expected(&prepared);
            let path = input(&root, &request);
            let bytes = std::fs::read(&path).unwrap();
            let name = path.clone();
            let hook: Box<dyn FnOnce() + Send> = match case {
                "swapped before the name" | "renamed during the read" => Box::new(move || {
                    // The held original moves aside; another object takes
                    // its name.
                    std::fs::rename(&name, name.with_extension("aside")).unwrap();
                    std::fs::write(&name, b"replacement").unwrap();
                    std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }),
                "rewritten during the read" => Box::new(move || {
                    // The same bytes again, in place: another version.
                    use std::io::Write;
                    std::thread::sleep(Duration::from_millis(20));
                    let mut file = std::fs::OpenOptions::new().write(true).open(&name).unwrap();
                    file.write_all(&bytes).unwrap();
                    file.sync_all().unwrap();
                }),
                "late" | "late right before the unlink" => {
                    Box::new(|| std::thread::sleep(Duration::from_millis(400)))
                }
                _ => Box::new(|| {}),
            };
            staging::REMOVAL_HOOKS
                .lock()
                .unwrap()
                .push((request.clone(), point, hook));
            let deadline = Instant::now()
                + if case.starts_with("late") {
                    Duration::from_millis(300)
                } else {
                    Duration::from_secs(20)
                };
            let result =
                staging::remove_input(&roots, &request, &wanted, &prepared.input.file, deadline);
            match case {
                "exact" => {
                    result.unwrap();
                    assert!(!path.exists());
                }
                "swapped before the name" | "renamed during the read" => {
                    assert!(result.is_err(), "{case}");
                    assert_eq!(std::fs::read(&path).unwrap(), b"replacement", "{case}");
                    assert!(path.with_extension("aside").exists(), "{case}");
                }
                _ => {
                    assert!(result.is_err(), "{case}");
                    assert!(path.exists(), "{case}");
                }
            }
        }
    }

    /// An absent-slot scan hashes at most 64 MiB in all: exactly that much
    /// completes, one byte more keeps the record.
    #[test]
    fn an_absent_slot_scan_hashes_at_most_its_aggregate_bound() {
        use std::os::unix::fs::OpenOptionsExt;
        for (size, expected) in [
            (staging::SCAN_BYTES / 2, staging::Effect::Gone),
            (
                staging::SCAN_BYTES / 2 + 1,
                staging::Effect::Kept(staging::REVIEW_BOUND),
            ),
        ] {
            let (_temp, root, roots) = fresh();
            let request = uuid::Uuid::new_v4().to_string();
            for name in ["a", "b"] {
                // Sparse zeros of the record's exact size: both are hashed.
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.join(staging::INPUTS).join(name))
                    .unwrap()
                    .set_len(size)
                    .unwrap();
            }
            let started = Instant::now();
            let effect = staging::release_reviewed(
                &roots,
                &request,
                size,
                &"0".repeat(64),
                &|| true,
                Instant::now() + Duration::from_secs(300),
            );
            assert_eq!(effect, expected, "{size} in {:?}", started.elapsed());
        }
    }

    #[test]
    fn removal_touches_only_the_exact_objects_it_read_or_created() {
        let (_temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(13, 1024)]);
        let request = verified.plan.request_id.clone();
        let dir = stage(&root, &request, &blobs);
        let prepared = staging::prepare(&roots, &verified, soon()).unwrap();
        let wanted = expected(&prepared);
        let staging::Prepared {
            input: custody,
            staged,
        } = prepared;
        // Rewritten in place, the original holds other bytes: never removed.
        let path = input(&root, &request);
        let mut other = std::fs::read(&path).unwrap();
        other[0] ^= 1;
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all(&other).unwrap();
        }
        assert!(staging::remove_input(&roots, &request, &wanted, &custody.file, soon()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), other);
        // And another object of other bytes under the custody name stays.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"another").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(staging::remove_input(&roots, &request, &wanted, &custody.file, soon()).is_err());
        assert!(path.exists());
        // A staged copy is still held, so a file recreated under its name
        // is another object: it stays, and so does the folder.
        let copy = dir.join(&verified.references[0].sha256);
        std::fs::remove_file(&copy).unwrap();
        std::fs::write(&copy, b"another").unwrap();
        assert!(staging::remove_staged(&roots, &request, &staged).is_err());
        assert!(copy.exists());
        // Anything else in the folder keeps the folder.
        let (_temp, root, roots) = fresh();
        let (verified, blobs, _) = original(&[shot(17, 1024)]);
        let request = verified.plan.request_id.clone();
        let dir = stage(&root, &request, &blobs);
        let prepared = staging::prepare(&roots, &verified, soon()).unwrap();
        std::fs::write(dir.join("unrelated"), b"kept").unwrap();
        staging::remove_staged(&roots, &request, &prepared.staged).unwrap();
        assert!(dir.join("unrelated").exists() && dir.exists());
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
    fn a_task_message_child_never_starts_once_its_deadline_is_spent() {
        use super::super::child::{run, Ended};
        clear();
        let ended = run(
            cat(),
            &[],
            b"task",
            &custody(),
            &resources(),
            expired(),
            |_| Ok(()),
        );
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(spawned(), None);
        let ended = crossed(|deadline, check| {
            clear();
            run(
                cat(),
                &[],
                b"task",
                &custody(),
                &resources(),
                deadline,
                check,
            )
        });
        assert_eq!(ended, Ended::NotStarted);
        assert_eq!(spawned(), None, "no child once the last check spent it");
        clear();
        let ended = run(
            cat(),
            &[],
            b"task",
            &custody(),
            &resources(),
            soon(),
            |_| Ok(()),
        );
        assert_eq!(
            ended,
            Ended::Exited {
                code: 0,
                stdout: b"task".to_vec()
            }
        );
    }
}
/// Reservation accounting controls use only synthetic originals and private Store fixtures.
/// They are ledger/Store tests, not native-agent qualification.
mod reservation_budget {
    use super::*;
    pub(super) fn budget(db: &rusqlite::Connection) -> (i64, i64, i64) {
        db.query_row(
            "SELECT rows,bytes,active FROM desktop_task_message_budget WHERE id=1",
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
        db.query_row("SELECT length(CAST(source AS BLOB))+length(CAST(plan AS BLOB))+37888 FROM desktop_task_messages WHERE request_id=?1",[id],|r|r.get(0)).unwrap()
    }
    pub(super) fn retained(db: &rusqlite::Connection) -> Vec<String> {
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(desktop_task_messages)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        db.prepare(&format!(
            "SELECT json_array({}) FROM desktop_task_messages ORDER BY request_id",
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
                "UPDATE desktop_task_message_budget SET rows=?1,bytes=?2,active=0 WHERE id=1",
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
        let leaf = &status["kinds"]["task_message"];
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
                "counter"=>{store.db.execute("UPDATE desktop_task_message_budget SET bytes=bytes-1",[]).unwrap();},
                "receipt"=>{store.db.execute("UPDATE desktop_task_messages SET receipt='{}' WHERE request_id=(SELECT request_id FROM desktop_task_messages LIMIT 1)",[]).unwrap();},
                "drained"=>{store.db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE desktop_task_messages SET drained=2 WHERE request_id=(SELECT request_id FROM desktop_task_messages LIMIT 1);").unwrap();},
                "write"=>store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_task_message_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap(),
                "commit"=>store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget_commit AFTER UPDATE ON desktop_task_message_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap(),
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
                store.db.execute_batch("CREATE TEMP TRIGGER fail_budget BEFORE UPDATE ON desktop_task_message_budget BEGIN SELECT RAISE(ABORT,'refused'); END;").unwrap();
            } else {
                store.db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE budget_parent(id INTEGER PRIMARY KEY);CREATE TABLE budget_child(id INTEGER REFERENCES budget_parent(id) DEFERRABLE INITIALLY DEFERRED);CREATE TEMP TRIGGER fail_budget AFTER UPDATE ON desktop_task_message_budget BEGIN INSERT INTO budget_child VALUES(1); END;").unwrap();
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
                "UPDATE desktop_task_message_budget SET rows=1,bytes=1,active=1 WHERE id=1",
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
            .prepare("SELECT * FROM desktop_task_messages")
            .unwrap()
            .column_count();
        let values: Vec<rusqlite::types::Value> = other
            .db
            .query_row("SELECT * FROM desktop_task_messages", [], |r| {
                (0..columns).map(|i| r.get(i)).collect()
            })
            .unwrap();
        let sql = format!(
            "INSERT INTO desktop_task_messages VALUES({})",
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
            ("plan", 16384_usize),
            ("receipt", 24576_usize),
            ("fingerprint", 64_usize),
            ("writer_epoch", 36_usize),
            ("task_id", 36_usize),
        ] {
            for mode in ["nul", "oversize", "unicode", "blob"] {
                let (_temp, location, mut store, epoch) = fixture();
                let v = original(&location.configuration, &epoch);
                let row = admitted(&mut store, &v, &epoch);
                retired(&mut store, &row);
                let charge = original_charge(&store.db, key(&v));
                let columns: Vec<String> = store
                    .db
                    .prepare("PRAGMA table_info(desktop_task_messages)")
                    .unwrap()
                    .query_map([], |r| r.get(1))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut values: Vec<rusqlite::types::Value> = store
                    .db
                    .query_row("SELECT * FROM desktop_task_messages", [], |r| {
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
                    "INSERT INTO desktop_task_messages VALUES({})",
                    vec!["?"; columns.len()].join(",")
                );
                copy.db
                    .execute(&sql, rusqlite::params_from_iter(values))
                    .unwrap();
                // Prefix-priced cumulative counters must never be normalized into
                // live zero by accepting a truncated original. This is corruption.
                copy.db.execute("UPDATE desktop_task_message_budget SET rows=1,bytes=?1,active=0 WHERE id=1",[charge]).unwrap();
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
