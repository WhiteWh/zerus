//! Actor-only deferred send storage (zerus-5wmh.66.4.4, slice 3). These
//! functions receive already admitted native plans; the runtime holds and
//! rechecks its real current guard before admission and reservation. Rows,
//! imported records and status grant no execution rights.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::receipt::{self, Observed};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use zerus_client_core::desktop::deferred_send::{
    self as wire, Intent, Phase, Plan, Receipt, Verified,
};
use zerus_desktop_ledger::operations::{custody::Record, Ticket};

const ROWS: i64 = 4096;
const BYTES: i64 = 16 << 20;
const ACTIVE: i64 = 32;
const SOURCE_BYTES: usize = 64 << 10;
const CHECKPOINT_BYTES: usize = 16 << 10;
const PLAN_BYTES: usize = 16 << 10;
const CUSTODY_BYTES: usize = 8 << 10;

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE desktop_deferred_send_budget(
        id INTEGER PRIMARY KEY CHECK(id=1),rows INTEGER NOT NULL CHECK(rows BETWEEN 0 AND 4096),
        bytes INTEGER NOT NULL CHECK(bytes BETWEEN 0 AND 16777216),active INTEGER NOT NULL CHECK(active BETWEEN 0 AND 32));
        INSERT INTO desktop_deferred_send_budget VALUES(1,0,0,0);
        CREATE TABLE desktop_deferred_sends(
        request_id TEXT PRIMARY KEY,session TEXT NOT NULL,fingerprint TEXT NOT NULL,
        source TEXT NOT NULL CHECK(length(CAST(source AS BLOB))<=65536),
        checkpoint TEXT NOT NULL CHECK(length(CAST(checkpoint AS BLOB))<=16384),
        plan TEXT NOT NULL CHECK(length(CAST(plan AS BLOB))<=16384),
        receipt TEXT NOT NULL CHECK(length(CAST(receipt AS BLOB))<=24576),
        writer_epoch TEXT NOT NULL,phase TEXT NOT NULL CHECK(phase IN('queued','running','recorded','failed','unknown')),
        drained INTEGER NOT NULL CHECK(drained IN(0,1)),
        ticket TEXT CHECK(length(CAST(ticket AS BLOB))<=4096),
        custody TEXT CHECK(length(CAST(custody AS BLOB))<=8192),
        created INTEGER NOT NULL,
        CHECK((ticket IS NULL)=(custody IS NULL)),
        CHECK(phase<>'running' OR ticket IS NOT NULL),
        CHECK(phase NOT IN('recorded','failed') OR drained=1));
        CREATE INDEX desktop_deferred_sends_pending ON desktop_deferred_sends(drained,phase,created,request_id);
        CREATE INDEX desktop_deferred_sends_custody ON desktop_deferred_sends(drained,request_id) WHERE custody IS NOT NULL;
        CREATE TRIGGER desktop_deferred_send_original BEFORE UPDATE OF request_id,session,fingerprint,source,checkpoint,plan,writer_epoch,created ON desktop_deferred_sends BEGIN
        SELECT RAISE(ABORT,'immutable deferred send original'); END;
        CREATE TRIGGER desktop_deferred_send_custody BEFORE UPDATE OF ticket,custody ON desktop_deferred_sends
        WHEN OLD.ticket IS NOT NULL AND (NEW.ticket IS NOT OLD.ticket OR NEW.custody IS NOT OLD.custody) BEGIN
        SELECT RAISE(ABORT,'immutable deferred send custody'); END;
        CREATE TRIGGER desktop_deferred_send_preserve BEFORE DELETE ON desktop_deferred_sends BEGIN
        SELECT RAISE(ABORT,'deferred send receipts are retained'); END;")?;
    Ok(())
}

pub(super) struct Row {
    charge: i64,
    pub verified: Verified,
    pub receipt: Receipt,
    pub writer_epoch: String,
    pub ticket: Option<Ticket>,
    pub custody: Option<Record>,
}
fn word(phase: Phase) -> &'static str {
    match phase {
        Phase::Queued => "queued",
        Phase::Running => "running",
        Phase::Recorded => "recorded",
        Phase::Failed => "failed",
        Phase::Unknown => "unknown",
    }
}
fn encoded<T: serde::Serialize>(value: &T, limit: usize) -> Result<String> {
    let text = serde_json::to_string(value)?;
    ensure!(
        text.len() <= limit,
        "deferred send record exceeds its bound"
    );
    Ok(text)
}

pub(super) fn load(db: &Connection, id: &str) -> Result<Option<Row>> {
    type Saved = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        String,
    );
    let saved: Option<Saved> = db
        .query_row(
            "SELECT substr(source,1,65537),substr(checkpoint,1,16385),substr(plan,1,16385),
             substr(receipt,1,24577),substr(fingerprint,1,65),substr(writer_epoch,1,37),phase,
             drained,substr(ticket,1,4097),substr(custody,1,8193),substr(session,1,513)
             FROM desktop_deferred_sends WHERE request_id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        source,
        checkpoint,
        plan,
        receipt,
        fingerprint,
        writer_epoch,
        phase,
        drained,
        ticket,
        custody,
        session,
    )) = saved
    else {
        return Ok(None);
    };
    let charge = charge(&source, &checkpoint, &plan)?;
    ensure!(
        matches!(drained, 0 | 1)
            && receipt.len() <= wire::REPLY_BYTES
            && ticket.as_ref().is_none_or(|value| value.len() <= 4096)
            && custody
                .as_ref()
                .is_none_or(|value| value.len() <= CUSTODY_BYTES),
        "deferred send saved bounds changed"
    );
    let drained = drained == 1;
    let receipt: Receipt = serde_json::from_str(&receipt)?;
    ensure!(
        receipt.valid()
            && receipt.binding.request_id == id
            && receipt.binding.session == session
            && receipt.binding.fingerprint().as_deref() == Some(fingerprint.as_str())
            && word(receipt.phase) == phase
            && receipt.drained == drained,
        "deferred send receipt changed"
    );
    let intent = Intent {
        scope: wire::Scope {
            configuration: receipt.binding.configuration.clone(),
            service_epoch: receipt.admission_epoch.clone(),
        },
        session: receipt.binding.session.clone(),
        request_id: id.into(),
        checkpoint,
        original: source,
    };
    // Each original is re-derived by its own kind's rule: a person's
    // deferred send, or an owner-approved question route.
    let verified = intent
        .verify_kind(receipt.binding.kind)
        .map_err(anyhow::Error::msg)?;
    let plan: Plan = serde_json::from_str(&plan)?;
    ensure!(
        verified.plan == plan
            && verified.binding == receipt.binding
            && receipt
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome.matches(&plan)),
        "deferred send plan changed"
    );
    ensure!(
        uuid::Uuid::parse_str(&writer_epoch)?.to_string() == writer_epoch
            && !uuid::Uuid::parse_str(&writer_epoch)?.is_nil(),
        "invalid deferred send writer"
    );
    let ticket: Option<Ticket> = ticket.map(|v| serde_json::from_str(&v)).transpose()?;
    let custody: Option<Record> = custody.map(|v| serde_json::from_str(&v)).transpose()?;
    ensure!(
        ticket.is_some() == custody.is_some(),
        "incomplete deferred send custody"
    );
    if let (Some(ticket), Some(custody)) = (&ticket, &custody) {
        ticket.fingerprint()?;
        custody.prepared.validate()?;
        ensure!(
            custody.prepared.ticket == *ticket
                && ticket.operation_id
                    == verified
                        .binding
                        .operation_id()
                        .context("invalid send key")?
                && ticket.binding_sha256 == fingerprint
                && ticket.admission_id == id
                && ticket.writer_epoch == writer_epoch
                && ticket.service_epoch == receipt.admission_epoch,
            "deferred send reservation changed"
        );
    }
    Ok(Some(Row {
        charge,
        verified,
        receipt,
        writer_epoch,
        ticket,
        custody,
    }))
}

/// The reservation includes the maximum future receipt, ticket and custody.
/// Stored original strings, not a re-serialization, determine its exact size.
fn charge(source: &str, checkpoint: &str, plan: &str) -> Result<i64> {
    ensure!(
        source.len() <= SOURCE_BYTES
            && checkpoint.len() <= CHECKPOINT_BYTES
            && plan.len() <= PLAN_BYTES,
        "deferred send original exceeds its bound"
    );
    [
        source.len(),
        checkpoint.len(),
        plan.len(),
        wire::REPLY_BYTES,
        CUSTODY_BYTES,
        4096,
        1024,
    ]
    .into_iter()
    .try_fold(0_i64, |sum, size| {
        sum.checked_add(i64::try_from(size).ok()?)
            .filter(|sum| *sum <= BYTES)
    })
    .context("deferred send reservation exceeds its bound")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Budget {
    rows: i64,
    bytes: i64,
    active: i64,
}
impl Budget {
    fn bounded(self) -> bool {
        (0..=ROWS).contains(&self.rows)
            && (0..=BYTES).contains(&self.bytes)
            && (0..=ACTIVE).contains(&self.active)
    }
    fn read(db: &Connection) -> Result<Self> {
        let mut query = db.prepare_cached(
            "SELECT id,rows,bytes,active FROM desktop_deferred_send_budget LIMIT 2",
        )?;
        let entries = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    Self {
                        rows: r.get(1)?,
                        bytes: r.get(2)?,
                        active: r.get(3)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            entries.len() == 1 && entries[0].0 == 1 && entries[0].1.bounded(),
            "deferred send budget changed"
        );
        Ok(entries[0].1)
    }
    fn reserve(self, charge: i64) -> Option<Self> {
        let next = Self {
            rows: self.rows.checked_add(1)?,
            bytes: self.bytes.checked_add(charge)?,
            active: self.active.checked_add(1)?,
        };
        next.bounded().then_some(next)
    }
    fn release(self, charge: i64) -> Result<Self> {
        let next = Self {
            rows: self
                .rows
                .checked_sub(1)
                .context("deferred send row budget underflow")?,
            bytes: self
                .bytes
                .checked_sub(charge)
                .context("deferred send byte budget underflow")?,
            active: self
                .active
                .checked_sub(1)
                .context("deferred send active budget underflow")?,
        };
        ensure!(next.bounded(), "deferred send budget underflow");
        Ok(next)
    }
    fn store(self, tx: &Transaction<'_>, old: Self) -> Result<()> {
        ensure!(
            self.bounded()
                && tx.execute(
                    "UPDATE desktop_deferred_send_budget SET rows=?1,bytes=?2,active=?3
             WHERE id=1 AND rows=?4 AND bytes=?5 AND active=?6",
                    params![
                        self.rows,
                        self.bytes,
                        self.active,
                        old.rows,
                        old.bytes,
                        old.active
                    ]
                )? == 1,
            "deferred send budget conflict"
        );
        Ok(())
    }
}

/// The pending index supplies at most 33 originals, without sorting or
/// scanning historical IDs. Every outstanding charge is checked from its row.
fn outstanding(db: &Connection) -> Result<(Budget, u64)> {
    let mut budget = Budget::default();
    let mut custody = 0;
    for id in charged_ids(db)? {
        let row = load(db, &id)?.context("deferred send outstanding original disappeared")?;
        ensure!(
            !row.receipt.drained,
            "deferred send outstanding state changed"
        );
        budget = budget
            .reserve(row.charge)
            .context("deferred send outstanding budget exceeds bound")?;
        custody += u64::from(row.custody.is_some());
    }
    Ok((budget, custody))
}
fn live_budget(db: &Connection) -> Result<Budget> {
    let saved = Budget::read(db)?;
    let (actual, _) = outstanding(db)?;
    ensure!(saved == actual, "deferred send reservation budget changed");
    Ok(saved)
}

/// Only owned Store startup calls this, in its original IMMEDIATE transaction.
/// Matching live accounting never touches historical rows. A legacy conversion
/// needs complete, bounded, validated coverage and the exact old formula; no
/// receipt or original is changed, deleted or made executable by the conversion.
fn reconcile(tx: &Transaction<'_>) -> Result<()> {
    let saved = Budget::read(tx)?;
    let (actual, _) = outstanding(tx)?;
    if saved == actual {
        return Ok(());
    }
    ensure!(
        saved.active == actual.active && saved.rows >= actual.rows && saved.bytes >= actual.bytes,
        "deferred send legacy budget changed"
    );
    let mut legacy = Budget::default();
    let mut after: Option<String> = None;
    loop {
        const PAGE: usize = 64;
        let ids: Vec<String> = if let Some(after) = &after {
            tx.prepare_cached("SELECT substr(request_id,1,37) FROM desktop_deferred_sends WHERE request_id>?1 ORDER BY request_id LIMIT 64")?
                .query_map([after], |r| r.get(0))?.collect::<std::result::Result<_,_>>()?
        } else {
            // Include a malformed NULL/empty first ID: never skip it as a cursor.
            tx.prepare_cached("SELECT substr(request_id,1,37) FROM desktop_deferred_sends ORDER BY request_id LIMIT 64")?
                .query_map([], |r| r.get(0))?.collect::<std::result::Result<_,_>>()?
        };
        for id in &ids {
            let row = load(tx, id)?.context("deferred send legacy original disappeared")?;
            legacy.rows = legacy
                .rows
                .checked_add(1)
                .context("deferred send history row overflow")?;
            legacy.bytes = legacy
                .bytes
                .checked_add(row.charge)
                .context("deferred send history byte overflow")?;
            legacy.active = legacy
                .active
                .checked_add(i64::from(!row.receipt.drained))
                .context("deferred send history active overflow")?;
            ensure!(
                legacy.bounded(),
                "deferred send legacy coverage exceeds bound"
            );
        }
        if ids.len() < PAGE {
            break;
        }
        after = ids.last().cloned();
    }
    ensure!(legacy == saved, "deferred send legacy accounting mismatch");
    actual.store(tx, saved)
}

/// A capacity-only decision before admission, in the same writer transaction
/// and native lease. Errors are not a capacity proof and grant no negative.
pub(super) fn capacity(tx: &Transaction<'_>, verified: &Verified) -> Result<bool> {
    let budget = live_budget(tx)?;
    let plan = encoded(&verified.plan, PLAN_BYTES)?;
    Ok(budget
        .reserve(charge(
            &verified.intent.original,
            &verified.intent.checkpoint,
            &plan,
        )?)
        .is_some())
}

/// Called in the same IMMEDIATE transaction as the final opaque native permit
/// and actual runtime guard checks. No source or recovery route calls this.
pub(super) fn admit(
    tx: &Transaction<'_>,
    verified: &Verified,
    writer_epoch: &str,
    at: i64,
) -> Result<Row> {
    let id = &verified.plan.request_id;
    if let Some(old) = load(tx, id)? {
        ensure!(
            old.verified.binding == verified.binding
                && old.verified.intent.original == verified.intent.original
                && old.verified.intent.checkpoint == verified.intent.checkpoint,
            "deferred send admission conflict"
        );
        return Ok(old);
    }
    ensure!(
        verified.intent.original.len() <= SOURCE_BYTES
            && verified.intent.checkpoint.len() <= CHECKPOINT_BYTES,
        "deferred send original exceeds its bound"
    );
    let plan = encoded(&verified.plan, PLAN_BYTES)?;
    let receipt = Receipt {
        binding: verified.binding.clone(),
        admission_epoch: verified.intent.scope.service_epoch.clone(),
        revision: 1,
        phase: Phase::Queued,
        drained: false,
        code: String::new(),
        outcome: None,
    };
    ensure!(receipt.valid(), "invalid original deferred send");
    let encoded_receipt = encoded(&receipt, wire::REPLY_BYTES)?;
    let charge = charge(
        &verified.intent.original,
        &verified.intent.checkpoint,
        &plan,
    )?;
    let budget = live_budget(tx)?;
    budget
        .reserve(charge)
        .context("deferred send capacity unavailable")?
        .store(tx, budget)?;
    tx.execute(
        "INSERT INTO desktop_deferred_sends(request_id,session,fingerprint,source,checkpoint,plan,
         receipt,writer_epoch,phase,drained,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'queued',0,?9)",
        params![
            id,
            verified.binding.session,
            verified
                .binding
                .fingerprint()
                .context("invalid send binding")?,
            verified.intent.original,
            verified.intent.checkpoint,
            plan,
            encoded_receipt,
            writer_epoch,
            at
        ],
    )?;
    load(tx, id)?.context("deferred send admission missing")
}

pub(super) fn pending(db: &Connection) -> Result<Option<Row>> {
    let id: Option<String> = db
        .query_row(
            "SELECT substr(request_id,1,37) FROM desktop_deferred_sends INDEXED BY desktop_deferred_sends_pending WHERE drained=0
             ORDER BY phase,created,request_id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| load(db, &id)?.context("deferred send disappeared"))
        .transpose()
}
pub(super) fn undrained(db: &Connection) -> Result<u64> {
    let saved = Budget::read(db)?;
    let (actual, _) = outstanding(db)?;
    // Read-only startup probes can still see verified legacy cumulative rows
    // and bytes; only the owned startup transaction may reconcile those.
    ensure!(
        saved.active == actual.active,
        "deferred send resource budget changed"
    );
    Ok(u64::try_from(actual.active)?)
}
pub(super) fn summary(db: &Connection) -> Result<serde_json::Value> {
    let saved = Budget::read(db)?;
    let (actual, custody) = outstanding(db)?;
    ensure!(
        saved.active == actual.active,
        "deferred send resource budget changed"
    );
    // Indexed metadata only, with one overflow sentinel. Historical IDs are
    // retained without becoming an unbounded foreground scan or queue.
    let mut query = db.prepare_cached("SELECT substr(phase,1,10),drained FROM desktop_deferred_sends ORDER BY request_id LIMIT 4097")?;
    let mut rows = query.query([])?;
    let mut count = 0_u64;
    let mut unsettled = 0_u64;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > ROWS as u64 {
            break;
        }
        let phase: String = row.get(0)?;
        let drained: i64 = row.get(1)?;
        ensure!(
            matches!(
                phase.as_str(),
                "queued" | "running" | "recorded" | "failed" | "unknown"
            ) && matches!(drained, 0 | 1),
            "deferred send history metadata changed"
        );
        unsettled += u64::from(!matches!(phase.as_str(), "recorded" | "failed"));
    }
    let complete = count <= ROWS as u64;
    Ok(
        serde_json::json!({"profile":wire::PROFILE,"rows":complete.then_some(count),
        "unsettled":complete.then_some(unsettled),"history_complete":complete,
        "undrained":actual.active,"original_custody":custody,"display_only":true,"authority":false}),
    )
}
pub(super) fn charged_ids(db: &Connection) -> Result<Vec<String>> {
    let mut query = db.prepare_cached(
        "SELECT substr(request_id,1,37) FROM desktop_deferred_sends INDEXED BY desktop_deferred_sends_pending
         WHERE drained=0 ORDER BY phase,created,request_id LIMIT 33",
    )?;
    let ids = query
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        ids.len() <= ACTIVE as usize,
        "deferred send active bound changed"
    );
    Ok(ids)
}
pub(super) fn custody_pending(db: &Connection, after: &str) -> Result<Option<Row>> {
    let id: Option<String> = db
        .query_row(
            "SELECT substr(request_id,1,37) FROM desktop_deferred_sends INDEXED BY desktop_deferred_sends_custody WHERE drained=0 AND custody IS NOT NULL
             AND request_id>?1 ORDER BY request_id LIMIT 1",
            [after],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| load(db, &id)?.context("deferred send disappeared"))
        .transpose()
}
pub(super) fn startup(db: &mut Connection) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    reconcile(&tx)?;
    let ids = charged_ids(&tx)?;
    for id in ids {
        let row = load(&tx, &id)?.context("deferred send disappeared")?;
        if row.receipt.phase != Phase::Unknown {
            retire(&tx, &row)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn state(tx: &Transaction<'_>, old: &Row, next: &Receipt) -> Result<()> {
    ensure!(
        !old.receipt.drained
            && next.valid()
            && next.binding == old.receipt.binding
            && next.admission_epoch == old.receipt.admission_epoch
            && next.revision
                == old
                    .receipt
                    .revision
                    .checked_add(1)
                    .context("deferred send revision exhausted")?,
        "deferred send state changed"
    );
    let budget = live_budget(tx)?;
    let released = next
        .drained
        .then(|| budget.release(old.charge))
        .transpose()?;
    ensure!(
        tx.execute(
            "UPDATE desktop_deferred_sends SET receipt=?1,phase=?2,drained=?3
             WHERE request_id=?4 AND fingerprint=?5 AND receipt=?6",
            params![
                encoded(next, wire::REPLY_BYTES)?,
                word(next.phase),
                next.drained,
                next.binding.request_id,
                next.binding.fingerprint().context("invalid send binding")?,
                encoded(&old.receipt, wire::REPLY_BYTES)?
            ]
        )? == 1,
        "deferred send state conflict"
    );
    if let Some(released) = released {
        released.store(tx, budget)?;
    }

    Ok(())
}

/// Sets the exact custody and Running together, before an original child
/// exists.
pub(super) fn reserve(
    tx: &Transaction<'_>,
    row: &Row,
    ticket: &Ticket,
    record: &Record,
) -> Result<()> {
    ensure!(
        row.receipt.phase == Phase::Queued && !row.receipt.drained && row.ticket.is_none(),
        "deferred send is not reservable"
    );
    ticket.fingerprint()?;
    record.prepared.validate()?;
    ensure!(
        record.prepared.ticket == *ticket
            && record.prepared.configuration == row.verified.binding.configuration
            && ticket.operation_id
                == row
                    .verified
                    .binding
                    .operation_id()
                    .context("invalid send key")?
            && ticket.binding_sha256
                == row
                    .verified
                    .binding
                    .fingerprint()
                    .context("invalid send binding")?
            && ticket.admission_id == row.verified.plan.request_id
            && ticket.revision == row.receipt.revision + 1
            && ticket.writer_epoch == row.writer_epoch
            && ticket.service_epoch == row.receipt.admission_epoch,
        "deferred send custody changed"
    );
    ensure!(
        tx.execute(
            "UPDATE desktop_deferred_sends SET ticket=?1,custody=?2 WHERE request_id=?3 AND ticket IS NULL",
            params![
                encoded(ticket, 4096)?,
                encoded(record, CUSTODY_BYTES)?,
                ticket.admission_id
            ]
        )? == 1,
        "deferred send reservation conflict"
    );
    let mut next = row.receipt.clone();
    next.phase = Phase::Running;
    next.revision += 1;
    state(tx, row, &next)
}

/// Recorded only by [`not_started`], before the drain; only the drain turns it
/// into a Failed receipt with [`wire::NOT_STARTED`], and only when it finds no
/// CLI record of the request. No Unknown code can be either.
const NOT_STARTED_PENDING: &str = "awaiting_not_started_drain";
/// The child ended; its result is read from the CLI's own receipt at drain.
pub(super) const AWAITING_DRAIN: &str = "awaiting_resource_drain";

/// Records how the original child ended (never its result) before the
/// resource drain settles the receipt.
pub(super) fn ended(tx: &Transaction<'_>, row: &Row, code: &str) -> Result<()> {
    ensure!(
        code != NOT_STARTED_PENDING && code != wire::NOT_STARTED,
        "only the pre-child result records a send that never started"
    );
    record(tx, row, code)
}

/// Records the closed pre-child result: the original child never started.
/// The receipt stays Unknown until the custody drain settles it.
pub(super) fn not_started(tx: &Transaction<'_>, row: &Row) -> Result<()> {
    record(tx, row, NOT_STARTED_PENDING)
}

fn record(tx: &Transaction<'_>, row: &Row, code: &str) -> Result<()> {
    ensure!(!row.receipt.drained, "deferred send is already drained");
    let mut next = row.receipt.clone();
    next.phase = Phase::Unknown;
    next.revision += 1;
    next.code = code.into();
    state(tx, row, &next)
}

/// Settles the receipt at the original's physical drain, from the CLI receipt
/// read after it. Takes the transaction, the exact acquired lock and the
/// objects the read used, so none is released before commit.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn drained(
    tx: Transaction<'_>,
    row: Row,
    observed: super::super::custody::Observation,
    cli: &Observed,
    evidence: receipt::Evidence,
) -> Result<Receipt> {
    let super::super::custody::Observation::Released { ref record, .. } = observed else {
        anyhow::bail!("deferred send custody is retained");
    };
    ensure!(
        row.custody.as_ref() == Some(record.as_ref()),
        "deferred send custody changed"
    );
    let mut next = row.receipt.clone();
    next.drained = true;
    next.revision += 1;
    let not_started = row.receipt.code == NOT_STARTED_PENDING;
    match cli {
        // The CLI's own exact submission record: the message is in the
        // terminal, whichever dispatch put it there, so it is never reported
        // as unsent.
        Observed::Submitted(outcome) if outcome.matches(&row.verified.plan) => {
            next.phase = Phase::Recorded;
            next.code.clear();
            next.outcome = Some(outcome.clone());
        }
        // The closed pre-child result, confirmed by the absence of any
        // record of the request: nothing ran and nothing else submitted it.
        Observed::Unknown(receipt::MISSING) if not_started => {
            next.phase = Phase::Failed;
            next.code = wire::NOT_STARTED.into();
        }
        Observed::Submitted(_) => {
            next.phase = Phase::Unknown;
            next.code = wire::RECEIPT_MISMATCH.into();
        }
        Observed::Unknown(code) => {
            next.phase = Phase::Unknown;
            // The receipt's reason, unless the child already named a more
            // specific one. An unconfirmed pre-child result is the
            // receipt's: another dispatch may hold the request.
            if next.code.is_empty() || next.code == AWAITING_DRAIN || not_started {
                next.code = (*code).into();
            }
        }
    }
    state(&tx, &row, &next)?;
    tx.commit()?;
    drop(evidence);
    drop(observed);
    Ok(next)
}

/// Restart or revoke never makes a queued or imported record executable. An
/// original reservation keeps its exact custody for read-only reconciliation.
pub(super) fn retire(tx: &Transaction<'_>, row: &Row) -> Result<()> {
    if row.receipt.drained {
        return Ok(());
    }
    let mut next = row.receipt.clone();
    next.phase = Phase::Unknown;
    next.revision += 1;
    next.drained = row.custody.is_none();
    next.code = "deferred_send_interrupted".into();
    state(tx, row, &next)
}
