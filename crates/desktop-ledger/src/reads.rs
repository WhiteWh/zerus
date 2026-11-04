//! Actor-owned read intent. A host receipt schedules a retry; only a fresh
//! authoritative snapshot retires it. Reads alone may retry unknown delivery,
//! because the host applies exact run/conversation acknowledgements idempotently.
use crate::{hash, json, text, uuid, PAGE_ROWS};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
pub use zerus_desktop_read_policy::{ReadMark, Retry, MAX_IN_FLIGHT};

pub const MARK_BYTES: usize = 32 << 10;

pub fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE desktop_reads (
      seq INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE,
      computer TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
      mark TEXT NOT NULL, due_at INTEGER NOT NULL,
      failures INTEGER NOT NULL, unapplied INTEGER NOT NULL,
      attempt TEXT UNIQUE, settled INTEGER NOT NULL CHECK(settled IN(0,1)));
      CREATE INDEX desktop_reads_due ON desktop_reads(computer,due_at,seq)
        WHERE attempt IS NULL AND settled=0;
      CREATE INDEX desktop_reads_flying ON desktop_reads(computer,seq) WHERE attempt IS NOT NULL;
      CREATE TABLE desktop_read_hosts (
        computer TEXT PRIMARY KEY, in_flight INTEGER NOT NULL CHECK(in_flight BETWEEN 0 AND 4),
        next_due INTEGER);
      CREATE INDEX desktop_read_hosts_due ON desktop_read_hosts(in_flight,next_due,computer);
      CREATE TABLE desktop_read_requests (
        request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, receipt TEXT NOT NULL);
      CREATE TABLE desktop_read_attempts (
        id TEXT PRIMARY KEY, ticket TEXT NOT NULL, service_epoch TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN('reserved','settled','unknown')), result TEXT);
      CREATE TABLE desktop_read_totals (
        id INTEGER PRIMARY KEY CHECK(id=1), pending INTEGER NOT NULL CHECK(pending>=0),
        in_flight INTEGER NOT NULL CHECK(in_flight BETWEEN 0 AND 4));
      INSERT INTO desktop_read_totals VALUES(1,0,0);
      CREATE TRIGGER desktop_read_insert AFTER INSERT ON desktop_reads BEGIN
        UPDATE desktop_read_totals SET pending=pending+(NEW.settled=0),
          in_flight=in_flight+(NEW.attempt IS NOT NULL) WHERE id=1; END;
      CREATE TRIGGER desktop_read_delete AFTER DELETE ON desktop_reads BEGIN
        UPDATE desktop_read_totals SET pending=pending-(OLD.settled=0),
          in_flight=in_flight-(OLD.attempt IS NOT NULL) WHERE id=1; END;
      CREATE TRIGGER desktop_read_update AFTER UPDATE ON desktop_reads BEGIN
        UPDATE desktop_read_totals SET pending=pending+(NEW.settled=0)-(OLD.settled=0),
          in_flight=in_flight+(NEW.attempt IS NOT NULL)-(OLD.attempt IS NOT NULL) WHERE id=1; END;",
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub seq: i64,
    pub revision: i64,
    pub mark: ReadMark,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub request_id: String,
    pub key: String,
    pub seq: i64,
    pub revision: i64,
    /// A frozen mark that conflicted with an unsettled current intent was
    /// consumed without replacing that intent. Old receipts default to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub epoch: String,
    pub service_epoch: String,
    pub pending: Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Delivered { applied_all: bool },
    Failed,
    NotSent,
}

fn encode(mark: &ReadMark) -> Result<String> {
    ensure!(mark.computer.len() <= 255, "invalid read computer");
    if !mark.computer.is_empty() {
        text(&mark.computer, 255)?;
        ensure!(!mark.computer.starts_with(['-', '@']), "invalid read alias");
        ensure!(
            !mark.computer.chars().any(char::is_whitespace),
            "invalid read alias"
        );
    }
    text(&mark.name, 1024)?;
    ensure!(!mark.name.starts_with(['-', '@']), "invalid read session");
    text(&mark.run_id, 1024)?;
    text(&mark.conversation_id, 1024)?;
    for part in [&mark.reply_id, &mark.attention_signature]
        .into_iter()
        .flatten()
    {
        if !part.is_empty() {
            text(part, 8192)?;
        }
    }
    ensure!(!mark.is_empty(), "empty read mark");
    json(mark, MARK_BYTES)
}
fn key(mark: &ReadMark) -> Result<String> {
    Ok(hash(
        serde_json::to_string(&[
            &mark.computer,
            &mark.name,
            &mark.run_id,
            &mark.conversation_id,
        ])?
        .as_bytes(),
    ))
}
fn refresh(tx: &Transaction<'_>, computer: &str) -> Result<()> {
    // The partial in-flight index contains at most four rows globally.
    let flying: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_reads WHERE computer=?1 AND attempt IS NOT NULL",
        [computer],
        |r| r.get(0),
    )?;
    let due: Option<i64> = tx.query_row(
        "SELECT due_at FROM desktop_reads WHERE computer=?1 AND attempt IS NULL AND settled=0 ORDER BY due_at,seq LIMIT 1",
        [computer], |r| r.get(0)).optional()?;
    if flying == 0 && due.is_none() {
        tx.execute(
            "DELETE FROM desktop_read_hosts WHERE computer=?1",
            [computer],
        )?;
    } else {
        tx.execute("INSERT INTO desktop_read_hosts VALUES(?1,?2,?3) ON CONFLICT(computer) DO UPDATE SET in_flight=excluded.in_flight,next_due=excluded.next_due",
            params![computer, flying, due])?;
    }
    Ok(())
}

/// Safe while passive: records intent, never dispatches it. The service must
/// commit this under FULL durability before reporting a frontend command queued.
/// Exact request replay returns its original receipt, even after retirement.
pub fn enqueue(db: &mut Connection, request_id: &str, mark: &ReadMark, now: i64) -> Result<Queued> {
    uuid(request_id)?;
    let encoded = encode(mark)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let receipt = enqueue_encoded(&tx, request_id, mark, encoded, now)?;
    tx.commit()?;
    Ok(receipt)
}

/// Actor-owned imports may commit a bounded page and its progress atomically.
/// The returned receipt is provisional until the caller commits this original
/// transaction. This records intent only; it grants no dispatch permission.
pub fn enqueue_in(
    tx: &Transaction<'_>,
    request_id: &str,
    mark: &ReadMark,
    now: i64,
) -> Result<Queued> {
    uuid(request_id)?;
    enqueue_encoded(tx, request_id, mark, encode(mark)?, now)
}

fn enqueue_encoded(
    tx: &Transaction<'_>,
    request_id: &str,
    mark: &ReadMark,
    encoded: String,
    now: i64,
) -> Result<Queued> {
    if let Some(receipt) = receipt_encoded(tx, request_id, mark, &encoded)? {
        return Ok(receipt);
    }
    enqueue_new(tx, request_id, mark, encoded, now, false)
}

/// Historical frozen intent never revives an identical settled row. A changed
/// settled mark is new intent; an unsettled conflicting mark stays current and
/// the frozen mark receives a superseded receipt in the original transaction.
pub fn adopt_in(
    tx: &Transaction<'_>,
    request_id: &str,
    mark: &ReadMark,
    now: i64,
) -> Result<Queued> {
    uuid(request_id)?;
    let encoded = encode(mark)?;
    if let Some(receipt) = receipt_encoded(tx, request_id, mark, &encoded)? {
        return Ok(receipt);
    }
    enqueue_new(tx, request_id, mark, encoded, now, true)
}

/// Indexed, read-only lookup bound to the exact normalized mark. Retired queue
/// rows do not erase their immutable request receipts.
pub fn receipt(db: &Connection, request_id: &str, mark: &ReadMark) -> Result<Option<Queued>> {
    uuid(request_id)?;
    receipt_encoded(db, request_id, mark, &encode(mark)?)
}

fn receipt_encoded(
    db: &Connection,
    request_id: &str,
    mark: &ReadMark,
    encoded: &str,
) -> Result<Option<Queued>> {
    let fingerprint = hash(encoded.as_bytes());
    let old: Option<(String, String)> = db
        .query_row(
            "SELECT substr(fingerprint,1,65),substr(receipt,1,4097) FROM desktop_read_requests WHERE request_id=?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((previous, receipt)) = old {
        ensure!(previous == fingerprint, "read request identity changed");
        ensure!(receipt.len() <= 4096, "invalid stored read receipt");
        let receipt: Queued = serde_json::from_str(&receipt)?;
        ensure!(
            receipt.request_id == request_id
                && receipt.key == key(mark)?
                && receipt.seq > 0
                && receipt.revision > 0,
            "invalid stored read receipt"
        );
        return Ok(Some(receipt));
    }
    Ok(None)
}

fn enqueue_new(
    tx: &Transaction<'_>,
    request_id: &str,
    mark: &ReadMark,
    encoded: String,
    now: i64,
    historical: bool,
) -> Result<Queued> {
    let fingerprint = hash(encoded.as_bytes());
    let key = key(mark)?;
    let old: Option<(String, i64, bool)> = tx
        .query_row(
            "SELECT mark,revision,settled FROM desktop_reads WHERE key=?1",
            [&key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let mut superseded = false;
    let revision = if let Some((old, revision, settled)) = old {
        let old: ReadMark = serde_json::from_str(&old)?;
        ensure!(
            old.computer == mark.computer
                && old.name == mark.name
                && old.run_id == mark.run_id
                && old.conversation_id == mark.conversation_id,
            "stored read identity changed"
        );
        let conflict = historical
            && !settled
            && [
                (&old.reply_id, &mark.reply_id),
                (&old.attention_signature, &mark.attention_signature),
            ]
            .into_iter()
            .any(|(previous, incoming)| {
                let previous = previous.as_deref().filter(|s| !s.is_empty());
                let incoming = incoming.as_deref().filter(|s| !s.is_empty());
                previous.is_some() && incoming.is_some() && previous != incoming
            });
        // Consume the whole conflicting frozen mark; none of its parts may
        // replace or delay the current unsettled/in-flight intent.
        superseded = conflict;
        let merged = if conflict {
            old.clone()
        } else {
            ReadMark::merged(&old, mark.clone())
        };
        if old != merged || (settled && !historical) {
            let revision = revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("read revision exhausted"))?;
            tx.execute("UPDATE desktop_reads SET mark=?2,revision=?3,due_at=?4,failures=0,unapplied=0,settled=0 WHERE key=?1",
                params![key, encode(&merged)?, revision, now])?;
            revision
        } else {
            revision
        }
    } else {
        tx.execute("INSERT INTO desktop_reads(key,computer,revision,mark,due_at,failures,unapplied,attempt,settled) VALUES(?1,?2,1,?3,?4,0,0,NULL,0)",
            params![key, mark.computer, encoded, now])?;
        1
    };
    let seq = tx.query_row("SELECT seq FROM desktop_reads WHERE key=?1", [&key], |r| {
        r.get(0)
    })?;
    let receipt = Queued {
        request_id: request_id.into(),
        key,
        seq,
        revision,
        superseded,
    };
    tx.execute(
        "INSERT INTO desktop_read_requests VALUES(?1,?2,?3)",
        params![request_id, fingerprint, json(&receipt, 4096)?],
    )?;
    refresh(tx, &mark.computer)?;
    Ok(receipt)
}

fn pending(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pending> {
    let value: String = row.get(2)?;
    let mark = serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Pending {
        seq: row.get(0)?,
        revision: row.get(1)?,
        mark,
    })
}

/// A durable dispatch reservation. The caller still supplies fresh delegation,
/// source identity and cancellation proof; an active ledger row is not consent.
/// Never reuses an attempt ID, including a completed or recovered one.
pub fn reserve(
    db: &mut Connection,
    epoch: &str,
    service_epoch: &str,
    id: &str,
    now: i64,
) -> Result<Option<Delivery>> {
    reserve_sources(db, epoch, service_epoch, id, now, None)
}

/// Limit fair admission to the caller's current service-fleet sources. This is
/// only a filter; it neither creates writer authority nor authenticates a host.
pub fn reserve_for(
    db: &mut Connection,
    epoch: &str,
    service_epoch: &str,
    id: &str,
    now: i64,
    computers: &[String],
) -> Result<Option<Delivery>> {
    ensure!(computers.len() <= 64, "read source limit");
    let mut seen = std::collections::BTreeSet::new();
    for computer in computers {
        if !computer.is_empty() {
            text(computer, 255)?;
        }
        ensure!(seen.insert(computer), "duplicate read source");
    }
    let sources = serde_json::to_string(computers)?;
    reserve_sources(db, epoch, service_epoch, id, now, Some(&sources))
}

fn reserve_sources(
    db: &mut Connection,
    epoch: &str,
    service_epoch: &str,
    id: &str,
    now: i64,
    sources: Option<&str>,
) -> Result<Option<Delivery>> {
    uuid(epoch)?;
    uuid(service_epoch)?;
    uuid(id)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let allowed: bool = tx.query_row(
        "SELECT revoked=0 AND epoch=?1 AND service_epoch=?2 FROM desktop_writer WHERE id=1",
        params![epoch, service_epoch],
        |r| r.get(0),
    )?;
    ensure!(allowed, "desktop read writer is revoked or stale");
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM desktop_read_attempts WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?;
    ensure!(!exists, "read attempt ID already used");
    let flying: i64 = tx.query_row(
        "SELECT count(*) FROM (SELECT seq FROM desktop_reads WHERE attempt IS NOT NULL LIMIT 5)",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        flying <= MAX_IN_FLIGHT as i64,
        "invalid read admission count"
    );
    if flying == MAX_IN_FLIGHT as i64 {
        return Ok(None);
    }
    let mut computer = None;
    for busy in 0..MAX_IN_FLIGHT as i64 {
        computer = match sources {
            Some(sources) => tx.query_row("SELECT h.computer FROM json_each(?3) j
                JOIN desktop_read_hosts h ON h.computer=j.value
                WHERE h.in_flight=?1 AND h.next_due<=?2 ORDER BY h.next_due,h.computer LIMIT 1",
                params![busy, now, sources], |r|r.get::<_,String>(0)).optional()?,
            None => tx.query_row("SELECT computer FROM desktop_read_hosts WHERE in_flight=?1 AND next_due<=?2 ORDER BY next_due,computer LIMIT 1",
                params![busy, now], |r| r.get::<_, String>(0)).optional()?,
        };
        if computer.is_some() {
            break;
        }
    }
    let Some(computer) = computer else {
        return Ok(None);
    };
    let pending = tx.query_row("SELECT seq,revision,mark FROM desktop_reads WHERE computer=?1 AND attempt IS NULL AND settled=0 AND due_at<=?2 ORDER BY due_at,seq LIMIT 1",
        params![computer, now], pending)?;
    let delivery = Delivery {
        id: id.into(),
        epoch: epoch.into(),
        service_epoch: service_epoch.into(),
        pending,
    };
    tx.execute(
        "INSERT INTO desktop_read_attempts VALUES(?1,?2,?3,'reserved',NULL)",
        params![id, json(&delivery, MARK_BYTES + 4096)?, service_epoch],
    )?;
    tx.execute(
        "UPDATE desktop_reads SET attempt=?2 WHERE seq=?1",
        params![delivery.pending.seq, id],
    )?;
    refresh(&tx, &computer)?;
    tx.commit()?;
    Ok(Some(delivery))
}

fn release(tx: &Transaction<'_>, delivery: &Delivery, outcome: Outcome, now: i64) -> Result<()> {
    let row: Option<(i64, u32, u32, bool)> = tx.query_row(
        "SELECT revision,failures,unapplied,settled FROM desktop_reads WHERE seq=?1 AND attempt=?2",
        params![delivery.pending.seq, delivery.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((revision, failures, unapplied, settled)) = row else {
        anyhow::bail!("read reservation lost");
    };
    if settled {
        tx.execute(
            "DELETE FROM desktop_reads WHERE seq=?1",
            [delivery.pending.seq],
        )?;
    } else if revision != delivery.pending.revision {
        // A callback for the older revision cannot delay the changed mark.
        tx.execute(
            "UPDATE desktop_reads SET attempt=NULL WHERE seq=?1",
            [delivery.pending.seq],
        )?;
    } else {
        let mut retry = Retry {
            failures,
            unapplied,
        };
        let due = match outcome {
            Outcome::Delivered { applied_all } => retry.finished(true, applied_all, now),
            Outcome::Failed => retry.finished(false, false, now),
            Outcome::NotSent => now,
        };
        tx.execute(
            "UPDATE desktop_reads SET attempt=NULL,due_at=?2,failures=?3,unapplied=?4 WHERE seq=?1",
            params![delivery.pending.seq, due, retry.failures, retry.unapplied],
        )?;
    }
    refresh(tx, &delivery.pending.mark.computer)
}

/// Settles only this exact reserved dispatch, even if revocation occurred while
/// it was in flight. A stale service or altered ticket cannot release a new job.
pub fn finish(
    db: &mut Connection,
    delivery: &Delivery,
    outcome: Outcome,
    now: i64,
) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (ticket, state, previous): (String, String, Option<String>) = tx.query_row(
        "SELECT ticket,state,result FROM desktop_read_attempts WHERE id=?1",
        [&delivery.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        serde_json::from_str::<Delivery>(&ticket)? == *delivery,
        "read completion identity changed"
    );
    let result = serde_json::to_string(&outcome)?;
    if state == "settled" {
        ensure!(previous.as_deref() == Some(&result), "read outcome changed");
        return Ok(false);
    }
    ensure!(state == "reserved", "unknown read attempt cannot settle");
    let same: bool = tx.query_row(
        "SELECT service_epoch=?1 FROM desktop_writer WHERE id=1",
        [&delivery.service_epoch],
        |r| r.get(0),
    )?;
    ensure!(same, "read service epoch changed");
    release(&tx, delivery, outcome, now)?;
    tx.execute(
        "UPDATE desktop_read_attempts SET state='settled',result=?2 WHERE id=?1",
        params![delivery.id, result],
    )?;
    tx.commit()?;
    Ok(true)
}

/// A fresh catalog row in source order, supplied by the actor, never a receipt.
pub struct Snapshot<'a> {
    pub computer: &'a str,
    pub name: &'a str,
    pub run: Option<&'a str>,
    pub conversation: Option<&'a str>,
    pub reply: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub read_reply: Option<&'a str>,
    pub read_signature: Option<&'a str>,
}

pub fn observe(db: &mut Connection, expected: &Pending, snapshot: &Snapshot<'_>) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = tx.query_row("SELECT seq,revision,mark FROM desktop_reads WHERE seq=?1 AND revision=?2 AND settled=0",
        params![expected.seq,expected.revision], pending).optional()?;
    let Some(stored) = stored else {
        return Ok(false);
    };
    ensure!(stored == *expected, "read observation identity changed");
    let mark = &stored.mark;
    ensure!(
        snapshot.computer == mark.computer && snapshot.name == mark.name,
        "read snapshot belongs to another computer or session"
    );
    if !mark.settled(
        snapshot.run,
        snapshot.conversation,
        snapshot.reply,
        snapshot.signature,
        snapshot.read_reply,
        snapshot.read_signature,
    ) {
        return Ok(false);
    }
    tx.execute(
        "UPDATE desktop_reads SET settled=1 WHERE seq=?1",
        [stored.seq],
    )?;
    tx.execute(
        "DELETE FROM desktop_reads WHERE seq=?1 AND attempt IS NULL",
        [stored.seq],
    )?;
    refresh(&tx, &mark.computer)?;
    tx.commit()?;
    Ok(true)
}

/// At most four dispatches can survive a crash. Their exact attempt remains
/// unknown forever; only the idempotent acknowledgement gets a new attempt.
pub fn recover(db: &mut Connection, service_epoch: &str, now: i64) -> Result<usize> {
    uuid(service_epoch)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let same: bool = tx.query_row(
        "SELECT service_epoch=?1 FROM desktop_writer WHERE id=1",
        [service_epoch],
        |r| r.get(0),
    )?;
    ensure!(same, "read recovery service epoch changed");
    let ids: Vec<String> = {
        let mut stmt =
            tx.prepare("SELECT attempt FROM desktop_reads WHERE attempt IS NOT NULL LIMIT 5")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    ensure!(ids.len() <= MAX_IN_FLIGHT, "invalid read recovery count");
    let mut recovered = 0;
    for id in ids {
        let (ticket, state): (String, String) = tx.query_row(
            "SELECT ticket,state FROM desktop_read_attempts WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let delivery: Delivery = serde_json::from_str(&ticket)?;
        ensure!(
            state == "reserved" && delivery.id == id,
            "invalid read recovery ticket"
        );
        if delivery.service_epoch == service_epoch {
            continue;
        }
        release(&tx, &delivery, Outcome::Failed, now)?;
        tx.execute(
            "UPDATE desktop_read_attempts SET state='unknown' WHERE id=?1",
            [&id],
        )?;
        recovered += 1;
    }
    tx.commit()?;
    Ok(recovered)
}

/// Stable sequence watermark; a new enqueue never extends an existing page walk.
pub fn watermark(db: &Connection) -> Result<i64> {
    Ok(
        db.query_row("SELECT coalesce(max(seq),0) FROM desktop_reads", [], |r| {
            r.get(0)
        })?,
    )
}
pub fn page(db: &Connection, after: i64, through: i64, limit: usize) -> Result<Vec<Pending>> {
    ensure!(
        after >= 0 && through >= after && (1..=PAGE_ROWS).contains(&limit),
        "invalid read page"
    );
    let mut stmt = db.prepare("SELECT seq,revision,mark FROM desktop_reads WHERE seq>?1 AND seq<=?2 AND settled=0 ORDER BY seq LIMIT ?3")?;
    let rows = stmt
        .query_map(params![after, through, limit as i64], pending)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Constant-size status even when pending marks and receipt history are large.
pub fn status(db: &Connection) -> Result<serde_json::Value> {
    Ok(db.query_row(
        "SELECT pending,in_flight FROM desktop_read_totals WHERE id=1",
        [],
        |r| Ok(serde_json::json!({"pending":r.get::<_,i64>(0)?,"in_flight":r.get::<_,i64>(1)?})),
    )?)
}

#[cfg(test)]
mod tests;
