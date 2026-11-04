use super::*;
use zerus_client_core::{ComputerId, ComputerStatus, MachineMeta, ViewRow};
fn q() -> Value {
    json!({"approval":true,"can_answer":true,"approval_choices":true,"source":"claude_tool_approval","answer_transport":"claude_tui","question_id":"approval-one","question_hash":"a".repeat(64),"run_id":"run-one","conversation_id":"conversation-one","questions":[{"id":"approval","options":[{"id":"yes","label":"Yes","scope":"once"},{"id":"always","label":"Always allow","scope":"broader"}]}]})
}
fn row() -> ViewRow {
    let mut row = ViewRow {
        name: "claude/example/permission".into(),
        run_id: Some("run-one".into()),
        conversation_id: Some("conversation-one".into()),
        tracked: true,
        state: "running".into(),
        phase: Some("approval".into()),
        created: 1,
        ..Default::default()
    };
    row.notify.cmd = "claude".into();
    row
}
fn target() -> policy::Candidate {
    policy::candidate("peer", &row()).unwrap()
}
struct Fixture {
    calls: Calls,
    log: Arc<Mutex<Vec<String>>>,
    current: Arc<AtomicBool>,
    change_after_begin: Arc<AtomicBool>,
    malformed: Arc<AtomicBool>,
    outcome: Arc<Mutex<Outcome>>,
    fail_finish: Arc<AtomicBool>,
}
fn fixture() -> Fixture {
    let log = Arc::new(Mutex::new(Vec::new()));
    let current = Arc::new(AtomicBool::new(true));
    let fresh = Arc::new(AtomicBool::new(true));
    let change = Arc::new(AtomicBool::new(false));
    let malformed = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(Mutex::new(Outcome::Done {
        receipt: json!({}),
        cleanup: None,
    }));
    let fail_finish = Arc::new(AtomicBool::new(false));
    let calls = Calls {
        current: {
            let current = current.clone();
            Arc::new(move |_| current.load(Ordering::Acquire))
        },
        inspect: {
            let log = log.clone();
            Arc::new(move |_| {
                log.lock().unwrap().push("inspect".into());
                Ok(
                    json!({"runtime_state":"live","process_state":"running","run_id":"run-one","conversation_id":"conversation-one","pending_questions":[q()]}),
                )
            })
        },
        journal: {
            let log = log.clone();
            let current = current.clone();
            let change = change.clone();
            let malformed = malformed.clone();
            let fail_finish = fail_finish.clone();
            Arc::new(move |operation, value| match operation {
                Operation::Begin => {
                    log.lock().unwrap().push("durable begin".into());
                    if change.load(Ordering::Acquire) {
                        current.store(false, Ordering::Release);
                    }
                    Ok(
                        json!({"eligible":true,"fresh":fresh.swap(false,Ordering::AcqRel),"id":1,"request_id":"10000000-0000-4000-8000-000000000001","answers":if malformed.load(Ordering::Acquire){json!([])}else{json!(policy::once("claude",&q()).unwrap())}}),
                    )
                }
                Operation::Finish => {
                    log.lock()
                        .unwrap()
                        .push(format!("finish {}", value["status"].as_str().unwrap()));
                    if fail_finish.load(Ordering::Acquire) {
                        Err("journal unavailable".into())
                    } else {
                        Ok(json!({"id":value["id"],"status":value["status"]}))
                    }
                }
                _ => panic!("not a journal write"),
            })
        },
        dispatch: {
            let log = log.clone();
            let outcome = outcome.clone();
            Arc::new(move |_, action, id| {
                assert!(matches!(action, Action::Answer { .. }));
                assert_eq!(id, "10000000-0000-4000-8000-000000000001");
                assert!(log.lock().unwrap().contains(&"durable begin".into()));
                log.lock().unwrap().push("shared Answer lane".into());
                Ok(outcome.lock().unwrap().clone())
            })
        },
    };
    Fixture {
        calls,
        log,
        current,
        change_after_begin: change,
        malformed,
        outcome,
        fail_finish,
    }
}
#[test]
fn a_durable_reservation_precedes_the_shared_answer_lane() {
    let f = fixture();
    attempt(&f.calls, &target()).unwrap();
    assert_eq!(
        *f.log.lock().unwrap(),
        [
            "inspect",
            "durable begin",
            "shared Answer lane",
            "finish approved"
        ]
    );
}
#[test]
fn disabling_after_reservation_cancels_without_native_input() {
    let f = fixture();
    f.change_after_begin.store(true, Ordering::Release);
    attempt(&f.calls, &target()).unwrap();
    assert_eq!(
        *f.log.lock().unwrap(),
        ["inspect", "durable begin", "finish cancelled"]
    );
}
#[test]
fn disabled_and_malformed_reservations_never_dispatch() {
    let f = fixture();
    f.current.store(false, Ordering::Release);
    attempt(&f.calls, &target()).unwrap();
    assert!(f.log.lock().unwrap().is_empty());
    f.current.store(true, Ordering::Release);
    f.malformed.store(true, Ordering::Release);
    assert!(attempt(&f.calls, &target()).is_err());
    assert!(!f
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|s| s == "shared Answer lane"));
}
#[test]
fn unknown_delivery_and_a_failed_finish_are_never_replayed() {
    let f = fixture();
    *f.outcome.lock().unwrap() = Outcome::Uncertain {
        detail: "delivery uncertain".into(),
    };
    f.fail_finish.store(true, Ordering::Release);
    assert!(attempt(&f.calls, &target()).is_err());
    attempt(&f.calls, &target()).unwrap();
    assert_eq!(
        f.log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.as_str() == "shared Answer lane")
            .count(),
        1
    );
    assert!(f.log.lock().unwrap().contains(&"finish unknown".into()));
}
#[test]
fn discovery_is_bounded_and_round_robin_and_never_uses_an_offline_replacement() {
    let pump = Pump::default();
    let computer = ComputerId::local();
    let node = "10000000-0000-4000-8000-000000000001";
    pump.push(zerus_client_core::Change::Machine {
        computer: computer.clone(),
        machine: MachineMeta {
            machine_id: Some(node.into()),
            ok: Some(true),
            ..Default::default()
        },
    });
    pump.push(zerus_client_core::Change::Computer {
        computer: computer.clone(),
        status: ComputerStatus::Live,
    });
    for i in 0..1000 {
        let mut r = row();
        r.name = format!("claude/example/{i}");
        if i != 900 {
            r.phase = Some("idle".into());
        }
        pump.push(zerus_client_core::Change::Upsert {
            computer: computer.clone(),
            key: format!("row{i}"),
            row: r,
        });
    }
    let mut cursor = ApprovalCursor::default();
    assert!(pump
        .approval_candidates(&mut cursor, 2, |_| true)
        .is_empty());
    let targets = pump.approval_candidates(&mut cursor, 1, |_| true);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].catalog_key, "row900");
    assert!(pump.approval_current(&targets[0]));
    assert!(pump
        .approval_candidates(&mut cursor, 0, |_| true)
        .is_empty());
    pump.push(zerus_client_core::Change::Computer {
        computer,
        status: ComputerStatus::Reconnecting { attempt: 1 },
    });
    assert!(!pump.approval_current(&targets[0]));
}

#[test]
fn discovery_never_occupies_both_slots_with_the_same_session() {
    let pump = Pump::default();
    let computer = ComputerId::local();
    pump.push(zerus_client_core::Change::Machine {
        computer: computer.clone(),
        machine: MachineMeta {
            machine_id: Some("10000000-0000-4000-8000-000000000001".into()),
            ok: Some(true),
            ..Default::default()
        },
    });
    pump.push(zerus_client_core::Change::Computer {
        computer: computer.clone(),
        status: ComputerStatus::Live,
    });
    pump.push(zerus_client_core::Change::Upsert {
        computer,
        key: "only".into(),
        row: row(),
    });
    assert_eq!(
        pump.approval_candidates(&mut ApprovalCursor::default(), 2, |_| true)
            .len(),
        1
    );
}

#[test]
fn live_rows_without_a_verified_machine_identity_never_admit_an_approval() {
    let pump = Pump::default();
    let computer = ComputerId::local();
    pump.push(zerus_client_core::Change::Machine {
        computer: computer.clone(),
        machine: MachineMeta {
            ok: Some(true),
            ..Default::default()
        },
    });
    pump.push(zerus_client_core::Change::Computer {
        computer: computer.clone(),
        status: ComputerStatus::Live,
    });
    pump.push(zerus_client_core::Change::Upsert {
        computer,
        key: "unidentified".into(),
        row: row(),
    });
    assert!(pump
        .approval_candidates(&mut ApprovalCursor::default(), 2, |_| true)
        .is_empty());
}
