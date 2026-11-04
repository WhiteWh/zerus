//! TEST-ONLY G2/bridge integration. Admissions/attempts are SQLite fixture
//! setup before the fake driver. No actor, worker or runtime guard exists.
use super::*;

#[path = "../../../../client/core/tests/support/operation_port.rs"]
mod oracle;

fn corpus() -> oracle::Fixture {
    serde_json::from_str(include_str!(
        "../../../../client/fixtures/desktop-operation-port-v1.json"
    ))
    .unwrap()
}
struct FixtureAttempt {
    ticket: ledger::Ticket,
    fresh: bool,
    phase: String,
}
// No Debug/Serialize. The fake observer retains at most one private call.
struct Call {
    args: Vec<String>,
    input: Vec<u8>,
    binding: policy::Binding,
}
#[derive(Default)]
struct FakePort {
    call: Option<Call>,
    symbolic_lifetimes: Vec<String>,
}
impl FakePort {
    fn observe(&mut self, proposal: policy::Proposal) -> Result<(), Refused> {
        if self.call.is_some() {
            return Err(Refused::Identity);
        }
        self.call = Some(Call {
            args: proposal.args().to_vec(),
            input: proposal.input().to_vec(),
            binding: proposal.binding().clone(),
        });
        Ok(())
    }
    fn symbolic_lifetime(&mut self, event: &str) {
        assert!(self.symbolic_lifetimes.len() < 64);
        self.symbolic_lifetimes.push(event.into());
        // No SQL drain/release or native resource call.
    }
}
#[derive(Default)]
struct LookupModel {
    explicit: bool,
    cancelled: bool,
    pressure: bool,
    revoked: bool,
    unknown: bool,
}
struct FakeDriver {
    loaded: Loaded,
    row: FixtureAttempt,
    observed: bool,
    blocked: bool,
}
impl FakeDriver {
    fn new(
        db: &Connection,
        expected: &ledger::Admitted,
        row: FixtureAttempt,
    ) -> Result<Self, Refused> {
        let loaded = Loaded::load(db, expected)?;
        if row.ticket.operation_id != expected.operation_id
            || row.ticket.admission_id != expected.admission_id
            || row.ticket.binding_sha256 != expected.binding_sha256
            || row.ticket.revision != expected.revision
        {
            return Err(Refused::Identity);
        }
        Ok(Self {
            loaded,
            row,
            observed: false,
            blocked: false,
        })
    }
    fn lookup(&mut self, model: &LookupModel, port: &mut FakePort) -> Result<bool, Refused> {
        // These are symbolic observations, not a CurrentGuard. A spent or
        // negatively observed fake driver cannot become fresh by resetting flags.
        self.blocked |= model.cancelled || model.pressure || model.revoked || model.unknown;
        if self.observed
            || self.blocked
            || !self.row.fresh
            || self.row.phase != "reserved"
            || !model.explicit
        {
            return Ok(false);
        }
        let proposal = self.loaded.proposal(false)?;
        self.observed = true; // Lost/declined observation cannot reissue.
        port.observe(proposal)?;
        Ok(true)
    }
    fn reply(
        &self,
        db: &mut Connection,
        ticket: &ledger::Ticket,
        bytes: &[u8],
    ) -> Result<(), Refused> {
        // Existing results do not depend on a fresh lookup/current epoch.
        self.loaded.finish_submitted(db, ticket, bytes)
    }
}

fn seeded(
    coalesced: bool,
    unknown: Option<ledger::Unknown>,
) -> (Fixture, Connection, ledger::Admitted, FixtureAttempt) {
    let f = Fixture::new();
    let mut db = database();
    let admitted = f.seed(&mut db);
    let ticket = f.reserve(&mut db, &admitted); // Existing helper asserts fresh.
    let fresh = if coalesced {
        let repeated = ledger::reserve(
            &mut db,
            &ledger::Reserve {
                operation_id: ticket.operation_id.clone(),
                attempt_id: ticket.attempt_id.clone(),
                revision: ticket.revision,
                writer_epoch: ticket.writer_epoch.clone(),
                service_epoch: ticket.service_epoch.clone(),
                lease: ticket.lease.clone(),
            },
        )
        .unwrap();
        assert_eq!(repeated.attempt.ticket, ticket);
        assert!(!repeated.fresh);
        repeated.fresh
    } else {
        true
    };
    if let Some(reason) = unknown {
        ledger::finish(&mut db, &ticket, &ledger::Outcome::Unknown { reason }).unwrap();
    }
    let phase = attempt(&db).0;
    (
        f,
        db,
        admitted,
        FixtureAttempt {
            ticket,
            fresh,
            phase,
        },
    )
}
fn explicit() -> LookupModel {
    LookupModel {
        explicit: true,
        ..LookupModel::default()
    }
}

#[test]
fn all_27_shared_vectors_remain_an_unchanged_independent_model_oracle() {
    let f = corpus();
    assert_eq!(f.abi, "desktop-operation-contract-v1");
    assert!(f.model_only);
    assert_eq!(f.cases.len(), 27);
    assert_eq!(
        to_ledger(&f.binding).key.operation_id().unwrap(),
        f.expected_operation_id
    );
    assert_eq!(hash(f.retained_input.as_bytes()), f.expected_plan_sha256);
    assert_eq!(
        ledger::consent_sha256(f.retained_input.as_bytes()),
        f.expected_consent_sha256
    );
    for case in &f.cases {
        let result = oracle::check_trace(&f, case);
        println!("MODEL CONFORMANCE {}: {:?}", case.name, result);
        assert_eq!(
            result.err(),
            case.expected_error.as_deref(),
            "{}",
            case.name
        );
        if !case.input_available {
            assert!(matches!(
                f.original(false),
                Err(policy::Review::MissingRetainedInput)
            ));
        }
    }
    println!("MODEL CONFORMANCE: 27 vectors, not 27 executing-ledger traces or Rust entries");
}

#[test]
fn fresh_explicit_fixture_observes_one_exact_proposal_and_closed_existing_receipt() {
    let (f, mut db, admitted, row) = seeded(false, None);
    let ticket = row.ticket.clone();
    let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
    let mut port = FakePort::default();
    assert!(driver.lookup(&explicit(), &mut port).unwrap());
    assert!(!driver.lookup(&explicit(), &mut port).unwrap());
    let call = port.call.as_ref().unwrap();
    assert_eq!(call.input, f.input);
    assert_eq!(call.binding, f.binding);
    assert_eq!(
        call.args,
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
    held(&db);
    driver.reply(&mut db, &ticket, &f.reply()).unwrap();
    driver.reply(&mut db, &ticket, &f.reply()).unwrap(); // Same result, no new call.
    assert!(!attempt(&db).2.unwrap().contains(PRIVATE));
    let frozen = ledger::load(&db, &admitted.operation_id).unwrap();
    assert_eq!(frozen.raw_record, f.raw);
    assert_eq!(frozen.retained_input, f.input);
    assert!(!driver.lookup(&explicit(), &mut port).unwrap());
    held(&db);
}

#[test]
fn no_lookup_cancel_pressure_revocation_unknown_or_coalesced_status_observes_no_call() {
    for case in 0..7 {
        let (_, db, admitted, row) = seeded(case == 6, None);
        let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
        let mut model = explicit();
        match case {
            0 => model.explicit = false,
            1 => model.cancelled = true,
            2 => model.pressure = true,
            3 => model.revoked = true,
            4 => model.unknown = true,
            5 => {
                model.explicit = false;
                model.unknown = true;
            }
            _ => (),
        }
        let mut port = FakePort::default();
        assert!(!driver.lookup(&model, &mut port).unwrap(), "case {case}");
        if case != 0 {
            assert!(!driver.lookup(&explicit(), &mut port).unwrap());
        }
        assert!(port.call.is_none());
        assert_eq!(attempt(&db), ("reserved".into(), false, None));
        held(&db);
    }
}

#[test]
fn old_epoch_result_after_revoke_settles_without_a_second_proposal_or_release() {
    let (f, mut db, admitted, row) = seeded(false, None);
    let ticket = row.ticket.clone();
    let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
    let mut port = FakePort::default();
    assert!(driver.lookup(&explicit(), &mut port).unwrap());
    zerus_desktop_ledger::revoke(&db, "fixture").unwrap();
    zerus_desktop_ledger::begin_service(&mut db, OTHER).unwrap();
    driver.reply(&mut db, &ticket, &f.reply()).unwrap();
    assert_eq!(attempt(&db).0, "submitted");
    assert!(!driver.lookup(&explicit(), &mut port).unwrap());
    held(&db);
}

#[test]
fn terminal_and_restart_unknown_refuse_a_new_observation_and_late_success() {
    for reason in [ledger::Unknown::LostResult, ledger::Unknown::ServiceRestart] {
        let (f, mut db, admitted, row) = seeded(false, Some(reason));
        let ticket = row.ticket.clone();
        let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
        let mut port = FakePort::default();
        let before = attempt(&db);
        assert!(!driver.lookup(&explicit(), &mut port).unwrap());
        assert!(port.call.is_none());
        assert!(driver.reply(&mut db, &ticket, &f.reply()).is_err());
        assert_eq!(attempt(&db), before);
        held(&db);
    }
}

#[test]
fn changed_admitted_or_complete_result_ticket_cannot_cross_the_fake_port() {
    for field in 0..4 {
        let (_, db, mut admitted, row) = seeded(false, None);
        match field {
            0 => admitted.operation_id = "f".repeat(64),
            1 => admitted.admission_id = OTHER.into(),
            2 => admitted.binding_sha256 = "f".repeat(64),
            _ => admitted.revision += 1,
        }
        assert!(FakeDriver::new(&db, &admitted, row).is_err());
        held(&db);
    }
    let (f, mut db, admitted, row) = seeded(false, None);
    let ticket = row.ticket.clone();
    let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
    let mut port = FakePort::default();
    assert!(driver.lookup(&explicit(), &mut port).unwrap());
    for field in 0..8 {
        let mut wrong = ticket.clone();
        match field {
            0 => wrong.operation_id = "f".repeat(64),
            1 => wrong.admission_id = OTHER.into(),
            2 => wrong.binding_sha256 = "f".repeat(64),
            3 => wrong.revision += 1,
            4 => wrong.attempt_id = OTHER.into(),
            5 => wrong.service_epoch = OTHER.into(),
            6 => wrong.writer_epoch = OTHER.into(),
            _ => wrong.lease = OTHER.into(),
        }
        assert!(driver.reply(&mut db, &wrong, &f.reply()).is_err());
        assert_eq!(attempt(&db), ("reserved".into(), false, None));
        held(&db);
    }
    assert!(driver.reply(&mut db, &ticket, b"{}").is_err());
    assert!(!driver.lookup(&explicit(), &mut port).unwrap());
    assert_eq!(attempt(&db), ("reserved".into(), false, None));
    held(&db);
}

#[test]
fn symbolic_output_stop_drain_and_release_never_free_sql_or_prove_held_input() {
    let f = corpus();
    let case = f
        .cases
        .iter()
        .find(|c| c.trace.iter().any(|e| e.t == "release") && c.expected_error.is_none())
        .unwrap();
    assert!(oracle::check_trace(&f, case).is_ok());
    assert!(case.trace.iter().all(|e| e.t != "stdin_done")); // Legacy oracle omits stdin.
    let (sql, mut db, admitted, row) = seeded(false, None);
    let ticket = row.ticket.clone();
    let mut driver = FakeDriver::new(&db, &admitted, row).unwrap();
    let mut port = FakePort::default();
    assert!(driver.lookup(&explicit(), &mut port).unwrap());
    driver.reply(&mut db, &ticket, &sql.reply()).unwrap();
    for event in &case.trace {
        if matches!(
            event.t.as_str(),
            "child_done" | "stdout_done" | "stderr_done" | "stop_returned" | "drained" | "release"
        ) {
            port.symbolic_lifetime(&event.t);
            held(&db); // No physical input worker was exercised or proved ended.
        }
    }
    assert!(port.symbolic_lifetimes.iter().any(|e| e == "release"));
    assert_eq!(attempt(&db).0, "submitted");
    held(&db);
}

#[test]
fn missing_input_stays_review_only_and_an_independent_fixture_is_not_a_retry() {
    let mut f = Fixture::new();
    f.input.clear(); // Complete plan remains in raw: never regenerate it.
    assert_eq!(
        f.verified().err(),
        Some(Refused::Policy(policy::Review::MissingRetainedInput))
    );
    let (first, first_db, first_admitted, first_row) =
        seeded(false, Some(ledger::Unknown::LostResult));
    let mut first_driver = FakeDriver::new(&first_db, &first_admitted, first_row).unwrap();
    let mut first_port = FakePort::default();
    assert!(!first_driver.lookup(&explicit(), &mut first_port).unwrap());
    // Separate SQLite fixture, NOT the first configuration's next attempt.
    let (second, mut second_db, second_admitted, second_row) = seeded(false, None);
    let second_ticket = second_row.ticket.clone();
    let mut second_driver = FakeDriver::new(&second_db, &second_admitted, second_row).unwrap();
    let mut second_port = FakePort::default();
    assert!(second_driver.lookup(&explicit(), &mut second_port).unwrap());
    second_driver
        .reply(&mut second_db, &second_ticket, &second.reply())
        .unwrap();
    assert_eq!(first.raw, second.raw);
    assert_eq!(attempt(&first_db).0, "unknown");
    held(&first_db);
    held(&second_db);
}
