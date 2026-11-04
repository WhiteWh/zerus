//! Transactional guards for the service actor's native effects. No native I/O,
//! approval authority, clock or frontend lives here. Read intent is in `reads`. The actor
//! must verify current delegation and lease freshness before calling `reserve`.
//! A returned existing reservation never authorizes another delivery.
pub mod native;
pub mod operations;
pub mod reads;

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const PAYLOAD_BYTES: usize = 64 << 10;
pub const RECEIPT_BYTES: usize = 16 << 10;
pub const PAGE_ROWS: usize = 128;
pub const RECOVERY_ROWS: usize = 256;

/// Root hgs owns user_version and calls this inside its schema transaction.
pub fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE desktop_writer (
        id INTEGER PRIMARY KEY CHECK(id=1), epoch TEXT, service_epoch TEXT NOT NULL,
        revoked INTEGER NOT NULL CHECK(revoked IN(0,1)), reason TEXT NOT NULL);
        INSERT INTO desktop_writer VALUES(1,NULL,'',1,'handoff_required');
        CREATE TABLE desktop_intents (
        id TEXT PRIMARY KEY, identity TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
        payload TEXT NOT NULL, withdrawn INTEGER NOT NULL CHECK(withdrawn IN(0,1)),
        blocked INTEGER NOT NULL CHECK(blocked IN(0,1)));
        CREATE TABLE desktop_attempts (
        seq INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE, fingerprint TEXT NOT NULL,
        intent_id TEXT NOT NULL REFERENCES desktop_intents(id), revision INTEGER NOT NULL,
        epoch TEXT NOT NULL, service_epoch TEXT NOT NULL, adapter TEXT NOT NULL, lease TEXT NOT NULL,
        retry_of TEXT, state TEXT NOT NULL CHECK(state IN('reserved','delivered','refused','unknown')),
        receipt TEXT);
        CREATE INDEX desktop_attempt_identity ON desktop_attempts(intent_id,revision,seq DESC);
        CREATE INDEX desktop_attempt_edge ON desktop_attempts(intent_id,seq DESC);
        CREATE INDEX desktop_attempt_state ON desktop_attempts(state,seq);
        CREATE TABLE desktop_attempt_totals (state TEXT PRIMARY KEY, count INTEGER NOT NULL CHECK(count>=0));
        INSERT INTO desktop_attempt_totals VALUES('reserved',0),('delivered',0),('refused',0),('unknown',0);
        CREATE TRIGGER desktop_attempt_insert AFTER INSERT ON desktop_attempts BEGIN
          UPDATE desktop_attempt_totals SET count=count+1 WHERE state=NEW.state; END;
        CREATE TRIGGER desktop_attempt_update AFTER UPDATE OF state ON desktop_attempts BEGIN
          UPDATE desktop_attempt_totals SET count=count-1 WHERE state=OLD.state;
          UPDATE desktop_attempt_totals SET count=count+1 WHERE state=NEW.state; END;")?;
    Ok(())
}

fn uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value),
        "invalid ledger UUID"
    );
    Ok(())
}
fn text(value: &str, limit: usize) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control),
        "invalid ledger identity"
    );
    Ok(())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn json(value: &impl Serialize, limit: usize) -> Result<String> {
    let encoded = serde_json::to_string(value)?;
    ensure!(
        encoded.len() <= limit,
        "ledger value exceeds its byte limit"
    );
    Ok(encoded)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Post,
    Withdrawal,
    Badge,
    Presence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Session {
        computer: String,
        run: String,
        conversation: String,
    },
    Device {
        configuration: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub kind: Kind,
    pub target: Target,
    pub edge: String,
}
impl Identity {
    fn encoded(&self) -> Result<String> {
        text(&self.edge, 1024)?;
        match &self.target {
            Target::Session {
                computer,
                run,
                conversation,
            } => {
                // The desktop/watch protocol uses the empty alias for this
                // computer. Remote aliases retain the original validation.
                if !computer.is_empty() {
                    text(computer, 255)?;
                }
                text(run, 1024)?;
                text(conversation, 1024)?;
                ensure!(
                    matches!(self.kind, Kind::Post | Kind::Withdrawal),
                    "invalid session effect kind"
                );
            }
            Target::Device { configuration } => {
                ensure!(
                    configuration.len() == 64
                        && configuration
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                    "invalid device configuration"
                );
                ensure!(
                    matches!(self.kind, Kind::Badge | Kind::Presence),
                    "invalid device effect kind"
                );
            }
        }
        json(self, 4096)
    }
}

/// Canonical lookup key without creating or changing a durable intent.
pub fn intent_id(identity: &Identity) -> Result<String> {
    Ok(hash(identity.encoded()?.as_bytes()))
}

/// Baselines/imports may be stored while passive. This records intent only and
/// cannot deliver it. A withdrawn edge is terminal; a new edge needs a new ID.
pub fn put_intent(
    db: &mut Connection,
    identity: &Identity,
    revision: u64,
    payload: &Value,
) -> Result<String> {
    put(db, identity, revision, payload, false)
}
/// Historic/imported/focus-suppressed edges never become a fresh banner later.
/// Their native tokens may still be checked against current withdrawal state.
pub fn put_baseline(
    db: &mut Connection,
    identity: &Identity,
    revision: u64,
    payload: &Value,
) -> Result<String> {
    put(db, identity, revision, payload, true)
}
fn put(
    db: &mut Connection,
    identity: &Identity,
    revision: u64,
    payload: &Value,
    blocked: bool,
) -> Result<String> {
    ensure!(
        revision > 0 && revision <= i64::MAX as u64,
        "invalid effect revision"
    );
    let identity = identity.encoded()?;
    let id = hash(identity.as_bytes());
    let payload = json(payload, PAYLOAD_BYTES)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, i64, String, bool)> = tx
        .query_row(
            "SELECT identity,revision,payload,withdrawn FROM desktop_intents WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((old_identity, old_revision, old_payload, withdrawn)) = existing {
        ensure!(
            old_identity == identity && !withdrawn,
            "effect identity is withdrawn or conflicting"
        );
        ensure!(revision as i64 >= old_revision, "stale effect revision");
        if revision as i64 == old_revision {
            ensure!(payload == old_payload, "effect revision payload changed");
            if blocked {
                tx.execute("UPDATE desktop_intents SET blocked=1 WHERE id=?1", [&id])?;
            }
            tx.commit()?;
            return Ok(id);
        }
        tx.execute(
            "UPDATE desktop_intents SET revision=?2,payload=?3,blocked=blocked OR ?4 WHERE id=?1",
            params![id, revision as i64, payload, blocked],
        )?;
    } else {
        tx.execute(
            "INSERT INTO desktop_intents VALUES(?1,?2,?3,?4,0,?5)",
            params![id, identity, revision as i64, payload, blocked],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn withdraw(db: &mut Connection, intent: &str, revision: u64) -> Result<()> {
    ensure!(
        revision > 0 && revision <= i64::MAX as u64,
        "invalid withdrawal revision"
    );
    let changed = db.execute(
        "UPDATE desktop_intents SET withdrawn=1,revision=?2 WHERE id=?1 AND revision<=?2",
        params![intent, revision as i64],
    )?;
    ensure!(changed == 1, "unknown or stale withdrawal");
    Ok(())
}
pub fn current(db: &Connection, intent: &str, revision: u64) -> Result<bool> {
    ensure!(revision <= i64::MAX as u64, "invalid effect revision");
    Ok(db
        .query_row(
            "SELECT 1 FROM desktop_intents WHERE id=?1 AND revision=?2 AND withdrawn=0",
            params![intent, revision as i64],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

/// Fence every process restart before any worker starts. Manual revocation
/// reasons survive restart. Recovery of old reservations is separately paged.
pub fn begin_service(db: &mut Connection, service_epoch: &str) -> Result<()> {
    uuid(service_epoch)?;
    db.execute("UPDATE desktop_writer SET service_epoch=?1,reason=CASE WHEN revoked=0 THEN 'service_restart' ELSE reason END,revoked=1 WHERE id=1", [service_epoch])?;
    Ok(())
}
/// Internal actor operation, never a frontend setting: the caller must already
/// hold both OS locks and supply fresh runtime/import/effect-guard proof.
pub fn authorize(db: &mut Connection, epoch: &str, service_epoch: &str) -> Result<()> {
    uuid(epoch)?;
    uuid(service_epoch)?;
    let changed = db.execute(
        "UPDATE desktop_writer SET epoch=?1,revoked=0,reason='' WHERE id=1 AND service_epoch=?2",
        params![epoch, service_epoch],
    )?;
    ensure!(changed == 1, "service epoch changed");
    Ok(())
}
pub fn revoke(db: &Connection, reason: &str) -> Result<()> {
    ensure!(
        !reason.is_empty()
            && reason.len() <= 64
            && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
        "invalid revocation reason"
    );
    db.execute(
        "UPDATE desktop_writer SET revoked=1,reason=?1 WHERE id=1",
        [reason],
    )?;
    Ok(())
}
pub fn recover_batch(db: &mut Connection, service_epoch: &str) -> Result<usize> {
    uuid(service_epoch)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let same: bool = tx.query_row(
        "SELECT service_epoch=?1 FROM desktop_writer WHERE id=1",
        [service_epoch],
        |r| r.get(0),
    )?;
    ensure!(same, "service epoch changed during recovery");
    let changed=tx.execute("UPDATE desktop_attempts SET state='unknown' WHERE seq IN (SELECT seq FROM desktop_attempts WHERE state='reserved' AND service_epoch<>?1 ORDER BY seq LIMIT ?2)",params![service_epoch,RECOVERY_ROWS as i64])?;
    tx.commit()?;
    Ok(changed)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reserve {
    pub request_id: String,
    pub intent_id: String,
    pub revision: u64,
    pub epoch: String,
    pub service_epoch: String,
    pub adapter: String,
    pub lease: String,
    pub retry_of: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Reserved,
    Delivered,
    Refused,
    Unknown,
}
impl State {
    fn name(&self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Delivered => "delivered",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
    fn parse(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "reserved" => Ok(Self::Reserved),
            "delivered" => Ok(Self::Delivered),
            "refused" => Ok(Self::Refused),
            "unknown" => Ok(Self::Unknown),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub seq: i64,
    pub request_id: String,
    pub intent_id: String,
    pub revision: u64,
    pub state: State,
}
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        seq: row.get(0)?,
        request_id: row.get(1)?,
        intent_id: row.get(2)?,
        revision: row.get(3)?,
        state: State::parse(row.get(4)?)?,
    })
}
const COLUMNS: &str = "seq,request_id,intent_id,revision,state";
pub struct Reservation {
    pub fresh: bool,
    pub attempt: Attempt,
    // Only the original successful commit creates this process-local origin.
    // Public response data and later indexed reads cannot reconstruct it.
    origin: Option<ReservationOrigin>,
}
struct ReservationOrigin {
    request: Reserve,
    seq: i64,
    withdrawal: bool,
}

/// Commit before native input. An exact duplicate, a crash, a cancelled worker
/// or an unknown outcome never returns fresh. Only an explicit retry of the
/// latest confirmed pre-delivery refusal can create another attempt.
pub fn reserve(db: &mut Connection, request: &Reserve) -> Result<Reservation> {
    reserve_scoped(db, request, false)
}
/// Cleanup only: the still-owned actor may remove one exact confirmed native
/// delivery after revocation. It cannot post, change writer epochs or replay an
/// unknown withdrawal. Original native owner/lease custody stays a caller duty.
pub fn reserve_withdrawal(db: &mut Connection, request: &Reserve) -> Result<Reservation> {
    ensure!(
        request.retry_of.is_none(),
        "cleanup never retries a native effect"
    );
    reserve_scoped(db, request, true)
}
fn reserve_scoped(db: &mut Connection, request: &Reserve, withdrawal: bool) -> Result<Reservation> {
    for id in [
        &request.request_id,
        &request.epoch,
        &request.service_epoch,
        &request.adapter,
        &request.lease,
    ] {
        uuid(id)?;
    }
    if let Some(id) = &request.retry_of {
        uuid(id)?;
    }
    ensure!(
        request.intent_id.len() == 64
            && request
                .intent_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid intent ID"
    );
    ensure!(
        request.revision > 0 && request.revision <= i64::MAX as u64,
        "invalid effect revision"
    );
    let fingerprint = hash(json(request, 4096)?.as_bytes());
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let previous: Option<(String, Attempt)> = tx
        .query_row(
            &format!("SELECT fingerprint,{COLUMNS} FROM desktop_attempts WHERE request_id=?1"),
            [&request.request_id],
            |r| {
                Ok((
                    r.get(0)?,
                    Attempt {
                        seq: r.get(1)?,
                        request_id: r.get(2)?,
                        intent_id: r.get(3)?,
                        revision: r.get(4)?,
                        state: State::parse(r.get(5)?)?,
                    },
                ))
            },
        )
        .optional()?;
    if let Some((old, attempt)) = previous {
        ensure!(old == fingerprint, "reservation request identity changed");
        return Ok(Reservation {
            fresh: false,
            attempt,
            origin: None,
        });
    }
    let allowed: bool = tx.query_row(
        "SELECT (revoked=0 OR ?3) AND epoch=?1 AND service_epoch=?2 FROM desktop_writer WHERE id=1",
        params![request.epoch, request.service_epoch, withdrawal],
        |r| r.get(0),
    )?;
    ensure!(allowed, "desktop writer is revoked or stale");
    ensure!(
        current(&tx, &request.intent_id, request.revision)?,
        "effect intent is stale or withdrawn"
    );
    let (encoded, blocked): (String, bool) = tx.query_row(
        "SELECT identity,blocked FROM desktop_intents WHERE id=?1",
        [&request.intent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(!blocked, "historic or suppressed effect cannot be posted");
    let identity: Identity = serde_json::from_str(&encoded)?;
    if withdrawal {
        let payload:String=tx.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB))<=?2 THEN payload END FROM desktop_intents WHERE id=?1",
            params![request.intent_id,PAYLOAD_BYTES as i64],|r|r.get(0))?;
        withdrawal_source(&tx, &identity, &payload)?;
    }
    let latest = if matches!(identity.kind, Kind::Post | Kind::Withdrawal) {
        tx.query_row(&format!("SELECT {COLUMNS} FROM desktop_attempts WHERE intent_id=?1 ORDER BY seq DESC LIMIT 1"),[&request.intent_id],row).optional()?
    } else {
        tx.query_row(&format!("SELECT {COLUMNS} FROM desktop_attempts WHERE intent_id=?1 AND revision=?2 ORDER BY seq DESC LIMIT 1"),params![request.intent_id,request.revision as i64],row).optional()?
    };
    if let Some(attempt) = latest {
        if request.retry_of.as_deref() != Some(attempt.request_id.as_str()) {
            ensure!(
                request.retry_of.is_none(),
                "retry does not name latest attempt"
            );
            return Ok(Reservation {
                fresh: false,
                attempt,
                origin: None,
            });
        }
        ensure!(
            attempt.state == State::Refused,
            "only confirmed pre-delivery refusal permits retry"
        );
    } else {
        ensure!(request.retry_of.is_none(), "retry has no prior attempt");
    }
    tx.execute("INSERT INTO desktop_attempts(request_id,fingerprint,intent_id,revision,epoch,service_epoch,adapter,lease,retry_of,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'reserved')",params![request.request_id,fingerprint,request.intent_id,request.revision as i64,request.epoch,request.service_epoch,request.adapter,request.lease,request.retry_of])?;
    let attempt = Attempt {
        seq: tx.last_insert_rowid(),
        request_id: request.request_id.clone(),
        intent_id: request.intent_id.clone(),
        revision: request.revision,
        state: State::Reserved,
    };
    tx.commit()?;
    Ok(Reservation {
        fresh: true,
        origin: Some(ReservationOrigin {
            request: request.clone(),
            seq: attempt.seq,
            withdrawal,
        }),
        attempt,
    })
}

/// Verify cleanup against the original indexed delivery, in the caller's SAME
/// transaction. Snapshot inspection repeats this proof after the fresh commit.
fn withdrawal_source(db: &Connection, identity: &Identity, payload: &str) -> Result<()> {
    ensure!(
        identity.kind == Kind::Withdrawal,
        "cleanup cannot reserve a post or device effect"
    );
    let mut post = identity.clone();
    post.kind = Kind::Post;
    let post_id = intent_id(&post)?;
    type Delivery = (
        String,
        i64,
        u64,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let (request, seq, revision, epoch, service, adapter, lease, payload_post, receipt): Delivery =
        db.query_row(
            "SELECT a.request_id,a.seq,a.revision,a.epoch,a.service_epoch,a.adapter,a.lease,
         CASE WHEN length(CAST(i.payload AS BLOB))<=?2 THEN i.payload END,
         CASE WHEN length(CAST(a.receipt AS BLOB))<=?3 THEN a.receipt END
         FROM desktop_intents i JOIN desktop_attempts a ON a.intent_id=i.id
         WHERE i.id=?1 AND a.revision=i.revision AND a.state='delivered' AND i.blocked=0
         ORDER BY a.seq DESC LIMIT 1",
            params![post_id, PAYLOAD_BYTES as i64, RECEIPT_BYTES as i64],
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
                ))
            },
        )?;
    let (cleanup, post, receipt): (Value, Value, Value) = (
        serde_json::from_str(payload)?,
        serde_json::from_str(&payload_post)?,
        serde_json::from_str(&receipt)?,
    );
    ensure!(
        cleanup.as_object().is_some_and(|v| v.len() == 3)
            && cleanup["op"] == "withdraw"
            && post["op"] == "post"
            && cleanup["token"].as_str().is_some_and(|s| !s.is_empty())
            && cleanup["token"] == post["token"]
            && receipt["outcome"]["state"] == "delivered"
            && !cleanup["native_id"].is_null()
            && cleanup["native_id"] == receipt["outcome"]["native_id"]
            && receipt["binding"]["intent_id"] == post_id,
        "withdrawal does not match the original delivered mapping"
    );
    let binding = &receipt["binding"];
    let session = &binding["session"];
    ensure!(
        binding["request_id"] == request
            && binding["seq"] == seq
            && binding["revision"] == revision
            && session["writer_epoch"] == epoch
            && session["service_epoch"] == service
            && session["adapter"] == adapter
            && session["lease"] == lease,
        "withdrawal receipt is not the original post binding"
    );
    Ok(())
}

/// Receipts settle already-reserved work, including after revocation. The actor
/// authenticates the adapter and verifies a Refused receipt means no native
/// delivery was attempted. A tombstone still invalidates subsequent activation.
pub fn finish(
    db: &mut Connection,
    request: &Reserve,
    outcome: State,
    receipt: &Value,
) -> Result<()> {
    ensure!(outcome != State::Reserved, "invalid terminal receipt state");
    let fingerprint = hash(json(request, 4096)?.as_bytes());
    let receipt = json(receipt, RECEIPT_BYTES)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (old, state, old_receipt): (String, String, Option<String>) = tx.query_row(
        "SELECT fingerprint,state,receipt FROM desktop_attempts WHERE request_id=?1",
        [&request.request_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        old == fingerprint,
        "receipt epoch, lease or intent identity changed"
    );
    if state != "reserved" {
        ensure!(
            state == outcome.name() && old_receipt.as_deref() == Some(receipt.as_str()),
            "conflicting or unknown receipt is immutable"
        );
        return Ok(());
    }
    tx.execute(
        "UPDATE desktop_attempts SET state=?2,receipt=?3 WHERE request_id=?1",
        params![request.request_id, outcome.name(), receipt],
    )?;
    tx.commit()?;
    Ok(())
}

/// Counts use four transactionally maintained rows, not a journal scan.
pub fn status(db: &Connection) -> Result<Value> {
    let (revoked, reason): (bool, String) = db.query_row(
        "SELECT revoked,reason FROM desktop_writer WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut counts = serde_json::Map::new();
    let mut query =
        db.prepare("SELECT state,count FROM desktop_attempt_totals ORDER BY state LIMIT 5")?;
    for entry in query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (state, count) = entry?;
        ensure!(
            matches!(
                state.as_str(),
                "reserved" | "delivered" | "refused" | "unknown"
            ),
            "invalid ledger counter"
        );
        counts.insert(state, Value::from(count));
    }
    ensure!(counts.len() == 4, "incomplete ledger counters");
    Ok(serde_json::json!({"revoked":revoked,"reason":reason,"attempts":counts}))
}
pub fn page(db: &Connection, before: i64, limit: usize) -> Result<Vec<Attempt>> {
    ensure!(
        before > 0 && (1..=PAGE_ROWS).contains(&limit),
        "invalid ledger page"
    );
    let mut query = db.prepare(&format!(
        "SELECT {COLUMNS} FROM desktop_attempts WHERE seq<?1 ORDER BY seq DESC LIMIT ?2"
    ))?;
    let rows = query
        .query_map(params![before, limit as i64], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests;
