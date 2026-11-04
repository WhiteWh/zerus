//! Durable guards for explicitly requested reconciliation, separate from native
//! notification attempts. The actor must verify the G2 Original/Proposal,
//! current delegation, drain, executor mapping and lease before admission or
//! reservation. These database APIs grant no authority from imported evidence.
//! No worker, process runner or positive frontend IPC lives here.
use crate::{hash, json, text, uuid, PAGE_ROWS, RECEIPT_BYTES};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub mod cache;
pub mod custody;

pub const PRIVATE_BYTES: usize = 1 << 20;
pub const METADATA_BYTES: usize = 16 << 10;
pub const UNSETTLED: i64 = 32;
pub const PAGE_BYTES: usize = 512 << 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    TaskStartReconcile,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub kind: Kind,
    pub schema: u32,
    pub configuration: String,
    pub project: String,
    pub task: String,
    pub request_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Executor {
    pub machine: String,
    pub session_id: String,
    pub run_id: String,
    pub conversation_id: String,
}
/// Mirrors G2's closed Binding data schema without a dependency from this
/// storage crate back into the client. The actor revalidates G2 policy first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub key: Key,
    pub marker_id: String,
    pub source_record_sha256: String,
    pub ui_snapshot_sha256: String,
    pub task_start_reconcile_input_v1_sha256: String,
    pub executor: Executor,
}
fn digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid operation digest"
    );
    Ok(())
}
impl Key {
    pub fn operation_id(&self) -> Result<String> {
        ensure!(self.schema == 1, "unsupported operation schema");
        digest(&self.configuration)?;
        text(&self.project, 512)?;
        uuid(&self.task)?;
        uuid(&self.request_id)?;
        let mut bytes = b"desktop-operation-key-v1\0".to_vec();
        for field in [
            "task_start_reconcile",
            "1",
            &self.configuration,
            &self.project,
            &self.task,
            &self.request_id,
        ] {
            bytes.extend_from_slice(&(u32::try_from(field.len())?).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        Ok(hash(&bytes))
    }
}
impl Binding {
    fn validate(&self) -> Result<String> {
        let id = self.key.operation_id()?;
        ensure!(
            self.marker_id == self.key.request_id,
            "operation marker changed"
        );
        for value in [
            &self.source_record_sha256,
            &self.ui_snapshot_sha256,
            &self.task_start_reconcile_input_v1_sha256,
        ] {
            digest(value)?;
        }
        for value in [
            &self.executor.machine,
            &self.executor.session_id,
            &self.executor.run_id,
            &self.executor.conversation_id,
        ] {
            text(value, 512)?;
        }
        Ok(id)
    }
}
pub fn consent_sha256(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"task-start-reconcile-consent-v1\0");
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

/// Private frozen bytes have neither Debug nor Serialize. Missing original
/// input refuses; this layer never serializes intent.plan to manufacture it.
pub struct Admission<'a> {
    pub admission_id: &'a str,
    pub binding: &'a Binding,
    pub raw_record: &'a [u8],
    pub retained_input: &'a [u8],
    pub provenance: &'a Value,
    pub writer_epoch: &'a str,
    pub service_epoch: &'a str,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admitted {
    pub operation_id: String,
    pub admission_id: String,
    pub binding_sha256: String,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reserve {
    pub operation_id: String,
    pub attempt_id: String,
    pub revision: u64,
    pub writer_epoch: String,
    pub service_epoch: String,
    pub lease: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub operation_id: String,
    pub attempt_id: String,
    pub revision: u64,
    pub admission_id: String,
    pub binding_sha256: String,
    pub writer_epoch: String,
    pub service_epoch: String,
    pub lease: String,
}
impl Ticket {
    pub fn fingerprint(&self) -> Result<String> {
        Ok(hash(self.encoded()?.as_bytes()))
    }

    fn encoded(&self) -> Result<String> {
        for id in [
            &self.attempt_id,
            &self.admission_id,
            &self.writer_epoch,
            &self.service_epoch,
            &self.lease,
        ] {
            uuid(id)?;
        }
        digest(&self.operation_id)?;
        digest(&self.binding_sha256)?;
        ensure!(
            (1..=i64::MAX as u64).contains(&self.revision),
            "invalid operation revision"
        );
        json(self, 4096)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u32,
    pub id: String,
    pub project: String,
    pub uuid: String,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    Policy,
    BeforeSpawnRefused,
    InvalidReply,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unknown {
    LostResult,
    TimedOut,
    Cancelled,
    ServiceRestart,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Submitted { receipt: Receipt },
    Review { reason: Review },
    Unknown { reason: Unknown },
}
impl Outcome {
    fn state(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "submitted",
            Self::Review { .. } => "review",
            Self::Unknown { .. } => "unknown",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub seq: i64,
    pub ticket: Ticket,
    pub state: String,
    pub outcome: Option<Outcome>,
    pub drained: bool,
}
pub struct Reservation {
    pub fresh: bool,
    pub attempt: Attempt,
}
/// Only the root actor's actual owned-resource accounting may supply this.
/// This is never deserialized from an adapter, frontend or import receipt.
pub struct VerifiedDrain {
    child: bool,
    input: bool,
    stdout: bool,
    stderr: bool,
}
impl VerifiedDrain {
    pub fn from_owned_resources(
        child: bool,
        input: bool,
        stdout: bool,
        stderr: bool,
    ) -> Result<Self> {
        ensure!(
            child && input && stdout && stderr,
            "operation resources are not drained"
        );
        Ok(Self {
            child,
            input,
            stdout,
            stderr,
        })
    }
}

pub fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE desktop_operations (
      seq INTEGER PRIMARY KEY AUTOINCREMENT, operation_id TEXT NOT NULL UNIQUE,
      admission_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL CHECK(kind='task_start_reconcile'),
      schema INTEGER NOT NULL CHECK(schema=1), configuration TEXT NOT NULL, project TEXT NOT NULL,
      task TEXT NOT NULL, original_request_id TEXT NOT NULL, key_json TEXT NOT NULL CHECK(length(CAST(key_json AS BLOB))<=4096),
      binding_json TEXT NOT NULL CHECK(length(CAST(binding_json AS BLOB))<=16384), binding_sha256 TEXT NOT NULL,
      raw_record BLOB NOT NULL CHECK(length(raw_record) BETWEEN 1 AND 1048576),
      retained_input BLOB NOT NULL CHECK(length(retained_input) BETWEEN 1 AND 1048576),
      source_record_sha256 TEXT NOT NULL, ui_snapshot_sha256 TEXT NOT NULL,
      task_start_reconcile_input_v1_sha256 TEXT NOT NULL, plan_sha256 TEXT NOT NULL, consent_sha256 TEXT NOT NULL,
      executor_machine TEXT NOT NULL, executor_session_id TEXT NOT NULL,
      executor_run_id TEXT NOT NULL, executor_conversation_id TEXT NOT NULL,
      provenance_json TEXT NOT NULL CHECK(length(CAST(provenance_json AS BLOB))<=16384),
      policy_state TEXT NOT NULL CHECK(policy_state IN('ready','review','submitted')),
      revision INTEGER NOT NULL CHECK(revision>0), outcome_json TEXT CHECK(length(CAST(outcome_json AS BLOB))<=16384),
      UNIQUE(configuration,kind,schema,project,task,original_request_id));
      CREATE TRIGGER desktop_operation_frozen BEFORE UPDATE OF
        operation_id,admission_id,kind,schema,configuration,project,task,original_request_id,key_json,
        binding_json,binding_sha256,raw_record,retained_input,source_record_sha256,ui_snapshot_sha256,
        task_start_reconcile_input_v1_sha256,plan_sha256,consent_sha256,executor_machine,
        executor_session_id,executor_run_id,executor_conversation_id,provenance_json,revision ON desktop_operations BEGIN
          SELECT RAISE(ABORT,'immutable operation'); END;
      CREATE TRIGGER desktop_operation_preserve BEFORE DELETE ON desktop_operations BEGIN
        SELECT RAISE(ABORT,'operation history requires explicit retention policy'); END;
      CREATE INDEX desktop_operations_policy ON desktop_operations(policy_state,seq);
      CREATE TABLE desktop_operation_attempts (
        seq INTEGER PRIMARY KEY AUTOINCREMENT, attempt_id TEXT NOT NULL UNIQUE,
        operation_id TEXT NOT NULL REFERENCES desktop_operations(operation_id), revision INTEGER NOT NULL,
        ticket TEXT NOT NULL CHECK(length(CAST(ticket AS BLOB))<=4096), fingerprint TEXT NOT NULL,
        writer_epoch TEXT NOT NULL, service_epoch TEXT NOT NULL, lease TEXT NOT NULL,
        admission_id TEXT NOT NULL, binding_sha256 TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN('reserved','submitted','review','unknown')),
        outcome_json TEXT CHECK(length(CAST(outcome_json AS BLOB))<=16384),
        drained INTEGER NOT NULL CHECK(drained IN(0,1)));
      CREATE TRIGGER desktop_operation_ticket BEFORE UPDATE OF attempt_id,operation_id,revision,
        ticket,fingerprint,writer_epoch,service_epoch,lease,admission_id,binding_sha256 ON desktop_operation_attempts BEGIN
          SELECT RAISE(ABORT,'immutable operation ticket'); END;
      CREATE TRIGGER desktop_operation_attempt_preserve BEFORE DELETE ON desktop_operation_attempts BEGIN
        SELECT RAISE(ABORT,'operation history requires explicit retention policy'); END;
      CREATE TRIGGER desktop_operation_terminal BEFORE UPDATE OF state,outcome_json ON desktop_operation_attempts
        WHEN OLD.state<>'reserved' AND (NEW.state<>OLD.state OR NEW.outcome_json IS NOT OLD.outcome_json) BEGIN
          SELECT RAISE(ABORT,'immutable operation outcome'); END;
      CREATE TRIGGER desktop_operation_drain_once BEFORE UPDATE OF drained ON desktop_operation_attempts
        WHEN (OLD.drained=1 AND NEW.drained=0) OR (NEW.drained=1 AND NEW.state='reserved') BEGIN
          SELECT RAISE(ABORT,'invalid operation drain transition'); END;
      CREATE INDEX desktop_operation_original ON desktop_operation_attempts(operation_id,seq DESC);
      CREATE INDEX desktop_operation_recovery ON desktop_operation_attempts(state,seq);
      CREATE UNIQUE INDEX desktop_operation_busy ON desktop_operation_attempts((1)) WHERE drained=0;
      CREATE TABLE desktop_operation_totals (
        id INTEGER PRIMARY KEY CHECK(id=1), unsettled INTEGER NOT NULL CHECK(unsettled BETWEEN 0 AND 32),
        undrained INTEGER NOT NULL CHECK(undrained BETWEEN 0 AND 1));
      INSERT INTO desktop_operation_totals VALUES(1,0,0);
      CREATE TRIGGER desktop_operation_insert AFTER INSERT ON desktop_operations BEGIN
        UPDATE desktop_operation_totals SET unsettled=unsettled+(NEW.policy_state<>'submitted') WHERE id=1; END;
      CREATE TRIGGER desktop_operation_state AFTER UPDATE OF policy_state ON desktop_operations BEGIN
        UPDATE desktop_operation_totals SET unsettled=unsettled+(NEW.policy_state<>'submitted')-(OLD.policy_state<>'submitted') WHERE id=1; END;
      CREATE TRIGGER desktop_operation_attempt_insert AFTER INSERT ON desktop_operation_attempts BEGIN
        UPDATE desktop_operation_totals SET undrained=undrained+(NEW.drained=0) WHERE id=1; END;
      CREATE TRIGGER desktop_operation_attempt_drain AFTER UPDATE OF drained ON desktop_operation_attempts BEGIN
        UPDATE desktop_operation_totals SET undrained=undrained+(NEW.drained=0)-(OLD.drained=0) WHERE id=1; END;")?;
    Ok(())
}
fn allowed(tx: &Connection, writer: &str, service: &str) -> Result<()> {
    uuid(writer)?;
    uuid(service)?;
    ensure!(
        tx.query_row(
            "SELECT revoked=0 AND epoch=?1 AND service_epoch=?2 FROM desktop_writer WHERE id=1",
            params![writer, service],
            |r| r.get::<_, bool>(0)
        )?,
        "operation writer is revoked or stale"
    );
    Ok(())
}
fn admitted(row: &rusqlite::Row<'_>) -> rusqlite::Result<Admitted> {
    Ok(Admitted {
        operation_id: row.get(0)?,
        admission_id: row.get(1)?,
        binding_sha256: row.get(2)?,
        revision: row.get(3)?,
    })
}
const ADMITTED: &str = "operation_id,admission_id,binding_sha256,revision";
fn attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let decode =
        |value: String| serde_json::from_str(&value).map_err(|_| rusqlite::Error::InvalidQuery);
    Ok(Attempt {
        seq: row.get(0)?,
        ticket: decode(row.get(1)?)?,
        state: row.get(2)?,
        outcome: row
            .get::<_, Option<String>>(3)?
            .map(|s| serde_json::from_str(&s).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?,
        drained: row.get(4)?,
    })
}
const ATTEMPT: &str = "seq,ticket,state,outcome_json,drained";

/// Requires already verified explicit G2 Original and current actor guard.
/// Exact duplicates coalesce without changing bodies or providing execution.
pub fn admit(db: &mut Connection, request: &Admission<'_>) -> Result<Admitted> {
    uuid(request.admission_id)?;
    let operation_id = request.binding.validate()?;
    ensure!(
        !request.raw_record.is_empty() && request.raw_record.len() <= PRIVATE_BYTES,
        "invalid frozen record size"
    );
    ensure!(
        !request.retained_input.is_empty() && request.retained_input.len() <= PRIVATE_BYTES,
        "frozen_input_required"
    );
    ensure!(
        hash(request.raw_record) == request.binding.source_record_sha256
            && hash(request.retained_input) == request.binding.task_start_reconcile_input_v1_sha256,
        "frozen operation bytes changed"
    );
    let binding = json(request.binding, METADATA_BYTES)?;
    let provenance = json(request.provenance, METADATA_BYTES)?;
    let binding_sha256 = hash(binding.as_bytes());
    let key = &request.binding.key;
    let executor = &request.binding.executor;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior_id: Option<String> = tx
        .query_row(
            "SELECT operation_id FROM desktop_operations WHERE admission_id=?1",
            [request.admission_id],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        prior_id.as_ref().is_none_or(|id| id == &operation_id),
        "operation admission UUID changed"
    );
    if let Some((old,old_binding,old_provenance))=tx.query_row(&format!("SELECT {ADMITTED},binding_json,provenance_json FROM desktop_operations WHERE operation_id=?1"),params![operation_id],|r|Ok((admitted(r)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional()? {
        ensure!(old.operation_id==operation_id && old.admission_id==request.admission_id && old_binding==binding && old_provenance==provenance,"operation admission identity changed");
        return Ok(old);
    }
    allowed(&tx, request.writer_epoch, request.service_epoch)?;
    let unsettled: i64 = tx.query_row(
        "SELECT unsettled FROM desktop_operation_totals WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(unsettled < UNSETTLED, "operation admission capacity");
    tx.execute("INSERT INTO desktop_operations(operation_id,admission_id,kind,schema,configuration,project,task,original_request_id,key_json,binding_json,binding_sha256,raw_record,retained_input,source_record_sha256,ui_snapshot_sha256,task_start_reconcile_input_v1_sha256,plan_sha256,consent_sha256,executor_machine,executor_session_id,executor_run_id,executor_conversation_id,provenance_json,policy_state,revision) VALUES(?1,?2,'task_start_reconcile',1,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14,?15,?16,?17,?18,?19,?20,'ready',1)",
      params![operation_id,request.admission_id,key.configuration,key.project,key.task,key.request_id,json(key,4096)?,binding,binding_sha256,request.raw_record,request.retained_input,request.binding.source_record_sha256,request.binding.ui_snapshot_sha256,request.binding.task_start_reconcile_input_v1_sha256,consent_sha256(request.retained_input),executor.machine,executor.session_id,executor.run_id,executor.conversation_id,provenance])?;
    tx.commit()?;
    Ok(Admitted {
        operation_id,
        admission_id: request.admission_id.into(),
        binding_sha256,
        revision: 1,
    })
}

/// Each fresh call must follow a NEW explicit lookup decision for the SAME
/// original, after actor guard validation. Never call this from a retry timer.
/// fresh=false carries the exact existing ticket and is status-only.
pub fn reserve(db: &mut Connection, request: &Reserve) -> Result<Reservation> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = reserve_in(&tx, request)?;
    tx.commit()?;
    Ok(result)
}

fn reserve_in(tx: &Transaction<'_>, request: &Reserve) -> Result<Reservation> {
    digest(&request.operation_id)?;
    uuid(&request.attempt_id)?;
    uuid(&request.lease)?;
    let original = tx.query_row(
        &format!("SELECT {ADMITTED} FROM desktop_operations WHERE operation_id=?1"),
        [&request.operation_id],
        admitted,
    )?;
    let ticket = Ticket {
        operation_id: original.operation_id,
        attempt_id: request.attempt_id.clone(),
        revision: request.revision,
        admission_id: original.admission_id,
        binding_sha256: original.binding_sha256,
        writer_epoch: request.writer_epoch.clone(),
        service_epoch: request.service_epoch.clone(),
        lease: request.lease.clone(),
    };
    let encoded = ticket.encoded()?;
    let fingerprint = hash(encoded.as_bytes());
    if let Some((old, attempt)) = tx
        .query_row(
            &format!(
                "SELECT fingerprint,{ATTEMPT} FROM desktop_operation_attempts WHERE attempt_id=?1"
            ),
            [&request.attempt_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Attempt {
                        seq: r.get(1)?,
                        ticket: serde_json::from_str(&r.get::<_, String>(2)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        state: r.get(3)?,
                        outcome: r
                            .get::<_, Option<String>>(4)?
                            .map(|s| {
                                serde_json::from_str(&s).map_err(|_| rusqlite::Error::InvalidQuery)
                            })
                            .transpose()?,
                        drained: r.get(5)?,
                    },
                ))
            },
        )
        .optional()?
    {
        ensure!(old == fingerprint, "operation reservation identity changed");
        return Ok(Reservation {
            fresh: false,
            attempt,
        });
    }
    allowed(tx, &request.writer_epoch, &request.service_epoch)?;
    ensure!(
        request.revision == original.revision,
        "stale operation revision"
    );
    let state: String = tx.query_row(
        "SELECT policy_state FROM desktop_operations WHERE operation_id=?1",
        [&request.operation_id],
        |r| r.get(0),
    )?;
    ensure!(state != "submitted", "operation already settled");
    if let Some(attempt) = tx
        .query_row(
            &format!("SELECT {ATTEMPT} FROM desktop_operation_attempts WHERE drained=0"),
            [],
            attempt,
        )
        .optional()?
    {
        ensure!(
            attempt.ticket.operation_id == request.operation_id
                && attempt.ticket.revision == request.revision,
            "operation execution capacity"
        );
        return Ok(Reservation {
            fresh: false,
            attempt,
        });
    }
    tx.execute("INSERT INTO desktop_operation_attempts(attempt_id,operation_id,revision,ticket,fingerprint,writer_epoch,service_epoch,lease,admission_id,binding_sha256,state,drained) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'reserved',0)",params![request.attempt_id,request.operation_id,request.revision,encoded,fingerprint,request.writer_epoch,request.service_epoch,request.lease,ticket.admission_id,ticket.binding_sha256])?;
    let attempt = Attempt {
        seq: tx.last_insert_rowid(),
        ticket,
        state: "reserved".into(),
        outcome: None,
        drained: false,
    };
    Ok(Reservation {
        fresh: true,
        attempt,
    })
}

/// Exact old-epoch receipts may finish their original still-reserved attempt
/// after revoke. Unknown outcomes never turn into a late success. No call here
/// starts work, frees the worker slot or replaces original bytes.
pub fn finish(db: &mut Connection, ticket: &Ticket, outcome: &Outcome) -> Result<()> {
    let fingerprint = hash(ticket.encoded()?.as_bytes());
    let encoded = json(outcome, RECEIPT_BYTES)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (old, state, previous): (String, String, Option<String>) = tx.query_row(
        "SELECT fingerprint,state,outcome_json FROM desktop_operation_attempts WHERE attempt_id=?1",
        [&ticket.attempt_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(old == fingerprint, "operation result ticket changed");
    if state != "reserved" {
        ensure!(
            state == outcome.state() && previous.as_deref() == Some(encoded.as_str()),
            "operation terminal outcome is immutable"
        );
        return Ok(());
    }
    let (key, revision): (String, u64) = tx.query_row(
        "SELECT key_json,revision FROM desktop_operations WHERE operation_id=?1",
        [&ticket.operation_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let key: Key = serde_json::from_str(&key)?;
    ensure!(
        revision == ticket.revision,
        "operation result revision changed"
    );
    if let Outcome::Submitted { receipt } = outcome {
        ensure!(
            receipt.schema == 1
                && receipt.id == key.request_id
                && receipt.project == key.project
                && receipt.uuid == key.task
                && receipt.state == "submitted",
            "operation receipt identity changed"
        );
    }
    tx.execute(
        "UPDATE desktop_operation_attempts SET state=?2,outcome_json=?3 WHERE attempt_id=?1",
        params![ticket.attempt_id, outcome.state(), encoded],
    )?;
    tx.execute(
        "UPDATE desktop_operations SET policy_state=?2,outcome_json=?3 WHERE operation_id=?1",
        params![
            ticket.operation_id,
            if matches!(outcome, Outcome::Submitted { .. }) {
                "submitted"
            } else {
                "review"
            },
            encoded
        ],
    )?;
    tx.commit()?;
    Ok(())
}
/// Separate from terminal outcome: only ACTUAL child, input writer and both
/// reader lifetime completions release this globally charged execution slot.
pub fn drained(db: &mut Connection, ticket: &Ticket, proof: VerifiedDrain) -> Result<()> {
    ensure!(
        proof.child && proof.input && proof.stdout && proof.stderr,
        "operation resources are not drained"
    );
    let fingerprint = hash(ticket.encoded()?.as_bytes());
    let changed=db.execute("UPDATE desktop_operation_attempts SET drained=1 WHERE attempt_id=?1 AND fingerprint=?2 AND state<>'reserved'",params![ticket.attempt_id,fingerprint])?;
    ensure!(
        changed == 1,
        "operation drain needs an exact terminal ticket"
    );
    Ok(())
}
/// Bounded restart recovery keeps resource uncertainty charged. A recovered
/// Unknown is not evidence that an orphaned child or retained pipe has ended.
pub fn recover_batch(db: &mut Connection, service_epoch: &str) -> Result<usize> {
    uuid(service_epoch)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut statement=tx.prepare(&format!("SELECT {ATTEMPT} FROM desktop_operation_attempts WHERE state='reserved' AND service_epoch<>?1 ORDER BY seq LIMIT 256"))?;
    let rows = statement
        .query_map([service_epoch], attempt)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let outcome = json(
        &Outcome::Unknown {
            reason: Unknown::ServiceRestart,
        },
        RECEIPT_BYTES,
    )?;
    for row in &rows {
        tx.execute("UPDATE desktop_operation_attempts SET state='unknown',outcome_json=?2 WHERE attempt_id=?1",params![row.ticket.attempt_id,outcome])?;
        tx.execute("UPDATE desktop_operations SET policy_state='review',outcome_json=?2 WHERE operation_id=?1",params![row.ticket.operation_id,outcome])?;
    }
    tx.commit()?;
    Ok(rows.len())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub unsettled: u64,
    pub undrained: u64,
}
pub fn totals(db: &Connection) -> Result<Totals> {
    Ok(db.query_row(
        "SELECT unsettled,undrained FROM desktop_operation_totals WHERE id=1",
        [],
        |r| {
            Ok(Totals {
                unsettled: r.get(0)?,
                undrained: r.get(1)?,
            })
        },
    )?)
}
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub seq: i64,
    pub operation_id: String,
    pub key: Key,
    pub revision: u64,
    pub policy_state: String,
    pub outcome: Option<Outcome>,
}
pub fn watermark(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT coalesce(max(seq),0) FROM desktop_operations",
        [],
        |r| r.get(0),
    )?)
}
pub fn page(db: &Connection, after: i64, through: i64, limit: usize) -> Result<Vec<Summary>> {
    ensure!(
        after >= 0 && through >= after && (1..=PAGE_ROWS).contains(&limit),
        "invalid operation page"
    );
    let mut statement=db.prepare("SELECT seq,operation_id,key_json,revision,policy_state,outcome_json FROM desktop_operations WHERE seq>?1 AND seq<=?2 ORDER BY seq LIMIT ?3")?;
    let mut rows = statement.query(params![after, through, limit])?;
    let mut result = Vec::new();
    let mut bytes = 0;
    while let Some(row) = rows.next()? {
        let key = row.get_ref(2)?.as_str()?;
        let outcome = match row.get_ref(5)? {
            rusqlite::types::ValueRef::Null => None,
            value => Some(value.as_str()?),
        };
        ensure!(
            key.len() <= 4096 && outcome.is_none_or(|s| s.len() <= RECEIPT_BYTES),
            "operation metadata exceeds its limit"
        );
        let id = row.get_ref(1)?.as_str()?;
        digest(id)?;
        let key: Key = serde_json::from_str(key)?;
        ensure!(
            key.operation_id()? == id,
            "operation summary identity changed"
        );
        let state = row.get_ref(4)?.as_str()?;
        ensure!(
            matches!(state, "ready" | "review" | "submitted"),
            "invalid operation policy state"
        );
        let summary = Summary {
            seq: row.get(0)?,
            operation_id: id.into(),
            key,
            revision: row.get(3)?,
            policy_state: state.into(),
            outcome: outcome.map(serde_json::from_str).transpose()?,
        };
        let charge = serde_json::to_vec(&summary)?.len() + 1;
        if bytes + charge > PAGE_BYTES {
            break;
        }
        bytes += charge;
        result.push(summary);
    }
    Ok(result)
}

/// One private original for off-UI-thread policy revalidation, not a dispatch.
/// Its bodies cannot escape through Debug or Serialize.
pub struct Frozen {
    pub admitted: Admitted,
    pub binding: Binding,
    pub raw_record: Vec<u8>,
    pub retained_input: Vec<u8>,
    pub provenance: Value,
}
pub fn load(db: &Connection, operation_id: &str) -> Result<Frozen> {
    digest(operation_id)?;
    let mut statement = db.prepare("SELECT operation_id,admission_id,binding_sha256,revision,binding_json,raw_record,retained_input,provenance_json FROM desktop_operations WHERE operation_id=?1")?;
    let mut rows = statement.query([operation_id])?;
    let row = rows
        .next()?
        .ok_or_else(|| anyhow::anyhow!("unknown original operation"))?;
    let binding = row.get_ref(4)?.as_str()?;
    let raw = row.get_ref(5)?.as_blob()?;
    let input = row.get_ref(6)?.as_blob()?;
    let provenance = row.get_ref(7)?.as_str()?;
    ensure!(
        binding.len() <= METADATA_BYTES
            && provenance.len() <= METADATA_BYTES
            && !raw.is_empty()
            && raw.len() <= PRIVATE_BYTES
            && !input.is_empty()
            && input.len() <= PRIVATE_BYTES,
        "frozen operation exceeds its limit"
    );
    let original = admitted(row)?;
    ensure!(
        hash(binding.as_bytes()) == original.binding_sha256,
        "frozen binding changed"
    );
    let binding: Binding = serde_json::from_str(binding)?;
    ensure!(
        binding.validate()? == operation_id
            && hash(raw) == binding.source_record_sha256
            && hash(input) == binding.task_start_reconcile_input_v1_sha256,
        "frozen operation changed"
    );
    Ok(Frozen {
        admitted: original,
        binding,
        raw_record: raw.to_vec(),
        retained_input: input.to_vec(),
        provenance: serde_json::from_str(provenance)?,
    })
}

#[cfg(test)]
mod tests;
