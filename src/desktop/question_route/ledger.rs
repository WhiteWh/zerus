//! The route ledger (zerus-sa9qn.8): one row per routed question identity,
//! written before the send child can exist, in the same transaction as the
//! deferred-send actor's original. A settled row never changes, so a
//! question is never routed twice and an unknown result is never replayed.
//! Retention is rolling (zerus-sa9qn.36): a full ledger prunes a bounded
//! batch of its oldest settled rows whose question can never be read again,
//! and keeps every reserved or unknown row.
use crate::desktop::deferred_send::Reservation;
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::path::Path;
use zerus_client_core::desktop::{
    deferred_send::{Phase, Receipt},
    question_route::{Record, PER_PARENT_HOUR},
};

/// The rows kept at most; a full ledger first runs one retention pass.
pub(super) const ROWS: i64 = 65_536;
/// The settled rows one retention pass deletes at most.
pub(super) const PRUNE: i64 = 256;
/// The settled rows one retention pass reads at most.
pub(super) const SCAN: i64 = 1_024;
/// A settled row is kept at least this long: the per-parent hourly bound and
/// Activity read recent rows.
pub(super) const RETAIN_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
/// The final refusal of a question beyond the per-parent bound.
pub(super) const PARENT_BOUND: &str = "question_route_parent_bound";
/// The final refusal of a question whose fixed text or send is not valid.
pub(super) const INVALID: &str = "question_route_invalid";
/// The actor has no original of a reserved route: never sent by this rule.
const ORIGINAL_MISSING: &str = "question_route_original_missing";

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE desktop_question_routes(
        key TEXT PRIMARY KEY CHECK(length(key)=64),
        request_id TEXT NOT NULL UNIQUE CHECK(length(request_id)=36),
        child_session_id TEXT NOT NULL CHECK(length(child_session_id)=36),
        parent_session_id TEXT NOT NULL CHECK(length(parent_session_id)=36),
        question_id TEXT NOT NULL CHECK(length(CAST(question_id AS BLOB)) BETWEEN 1 AND 256),
        created INTEGER NOT NULL,
        state TEXT NOT NULL CHECK(state IN('reserved','submitted','refused','unknown')),
        code TEXT NOT NULL CHECK(length(code)<=80));
        CREATE INDEX desktop_question_routes_parent ON desktop_question_routes(parent_session_id,created);
        CREATE INDEX desktop_question_routes_reserved ON desktop_question_routes(created,key) WHERE state='reserved';
        CREATE TRIGGER desktop_question_route_identity BEFORE UPDATE OF key,request_id,child_session_id,parent_session_id,question_id,created ON desktop_question_routes BEGIN
        SELECT RAISE(ABORT,'immutable question route'); END;
        CREATE TRIGGER desktop_question_route_settled BEFORE UPDATE OF state,code ON desktop_question_routes
        WHEN OLD.state<>'reserved' OR NEW.state='reserved' BEGIN
        SELECT RAISE(ABORT,'settled question route'); END;
        CREATE TRIGGER desktop_question_route_retained BEFORE DELETE ON desktop_question_routes BEGIN
        SELECT RAISE(ABORT,'question routes are retained'); END;
        CREATE INDEX desktop_question_route_sessions ON desktop_catalog_rows(generation,
        CASE WHEN json_valid(json) THEN json_extract(json,'$.session_id') END);",
    )?;
    Ok(())
}

/// The retention leaf (zerus-sa9qn.36). Each new row keeps its run, so a
/// settled row is pruned only once its question can never be read again.
/// Only a settled row can be deleted; reserved and unknown rows stay. A
/// counter keeps the row count, and a cursor keeps the rolling position.
pub(super) fn migrate_retention(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE desktop_question_routes ADD COLUMN run_id TEXT
        CHECK(run_id IS NULL OR length(CAST(run_id AS BLOB)) BETWEEN 1 AND 128);
        DROP TRIGGER desktop_question_route_identity;
        CREATE TRIGGER desktop_question_route_identity BEFORE UPDATE OF key,request_id,child_session_id,parent_session_id,question_id,created,run_id ON desktop_question_routes BEGIN
        SELECT RAISE(ABORT,'immutable question route'); END;
        DROP TRIGGER desktop_question_route_retained;
        CREATE TRIGGER desktop_question_route_retained BEFORE DELETE ON desktop_question_routes
        WHEN OLD.state NOT IN('submitted','refused') BEGIN
        SELECT RAISE(ABORT,'unsettled question routes are retained'); END;
        CREATE INDEX desktop_question_routes_settled ON desktop_question_routes(created,key)
        WHERE state IN('submitted','refused');
        CREATE TABLE desktop_question_route_retention(id INTEGER PRIMARY KEY CHECK(id=1),
        rows INTEGER NOT NULL CHECK(rows>=0), cursor_created INTEGER, cursor_key TEXT,
        CHECK((cursor_created IS NULL)=(cursor_key IS NULL)));
        INSERT INTO desktop_question_route_retention(id,rows)
        SELECT 1,count(*) FROM desktop_question_routes;
        CREATE TRIGGER desktop_question_route_counted AFTER INSERT ON desktop_question_routes BEGIN
        UPDATE desktop_question_route_retention SET rows=rows+1 WHERE id=1; END;
        CREATE TRIGGER desktop_question_route_uncounted AFTER DELETE ON desktop_question_routes BEGIN
        UPDATE desktop_question_route_retention SET rows=rows-1 WHERE id=1; END;",
    )?;
    Ok(())
}
/// Test rewind of the retention leaf to the schema-39 ledger.
#[cfg(test)]
pub(super) fn remove_retention_fixture(db: &Connection) {
    db.execute_batch(
        "DROP TRIGGER desktop_question_route_counted;
        DROP TRIGGER desktop_question_route_uncounted;
        DROP TABLE desktop_question_route_retention;
        DROP INDEX desktop_question_routes_settled;
        DROP TRIGGER desktop_question_route_retained;
        CREATE TRIGGER desktop_question_route_retained BEFORE DELETE ON desktop_question_routes BEGIN
        SELECT RAISE(ABORT,'question routes are retained'); END;
        DROP TRIGGER desktop_question_route_identity;
        CREATE TRIGGER desktop_question_route_identity BEFORE UPDATE OF key,request_id,child_session_id,parent_session_id,question_id,created ON desktop_question_routes BEGIN
        SELECT RAISE(ABORT,'immutable question route'); END;
        ALTER TABLE desktop_question_routes DROP COLUMN run_id;",
    )
    .unwrap();
}

/// The ledger's row count, from its counter.
pub(super) fn count(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT rows FROM desktop_question_route_retention WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}

/// One bounded retention pass (zerus-sa9qn.36). From the rolling cursor it
/// reads at most [`SCAN`] settled rows created before `at - RETAIN_MS`,
/// oldest first, and deletes at most [`PRUNE`] of them whose question can
/// never be read again: this computer's complete visible catalog lists no
/// row of the child's session at the row's run. A run ID is never reused,
/// and a question routes only from its listed run, so a pruned question
/// never routes twice. Kept: every reserved or unknown row, a row of a run
/// still listed, a row without a run (older ledgers) while its session is
/// listed, and every row while the catalog is incomplete. The cursor moves
/// past what was read and wraps at the end, so kept rows never stall later
/// passes. Returns the rows deleted.
pub(super) fn prune(tx: &Connection, at: i64) -> Result<i64> {
    let generation: Option<i64> = tx
        .query_row(
            "SELECT visible FROM desktop_catalog_sources
             WHERE computer='' AND complete=1 AND active=1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(generation) = generation else {
        return Ok(0);
    };
    let (after_created, after_key): (Option<i64>, Option<String>) = tx.query_row(
        "SELECT cursor_created,cursor_key FROM desktop_question_route_retention WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut read = tx.prepare_cached(
        "SELECT key,created,child_session_id,run_id FROM desktop_question_routes
         INDEXED BY desktop_question_routes_settled
         WHERE state IN('submitted','refused') AND created<?1 AND (created,key)>(?2,?3)
         ORDER BY created,key LIMIT ?4",
    )?;
    let candidates = read
        .query_map(
            params![
                at.saturating_sub(RETAIN_MS),
                after_created.unwrap_or(i64::MIN),
                after_key.unwrap_or_default(),
                SCAN
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut listed = tx.prepare_cached(
        "SELECT 1 FROM desktop_catalog_rows WHERE generation=?1
         AND (CASE WHEN json_valid(json) THEN json_extract(json,'$.session_id') END)=?2
         AND (?3 IS NULL OR json_extract(json,'$.run_id') IS NULL
              OR json_extract(json,'$.run_id')=?3) LIMIT 1",
    )?;
    let (mut pruned, mut read_rows, mut last) = (0, 0, None);
    for (key, created, child, run) in &candidates {
        read_rows += 1;
        last = Some((*created, key.clone()));
        if listed.exists(params![generation, child, run])? {
            continue;
        }
        ensure!(
            tx.execute(
                "DELETE FROM desktop_question_routes
                 WHERE key=?1 AND state IN('submitted','refused')",
                [key],
            )? == 1,
            "question route retention conflict"
        );
        pruned += 1;
        if pruned == PRUNE {
            break;
        }
    }
    // Past the last row read; back to the start once the end was reached.
    let end = read_rows == candidates.len() && (candidates.len() as i64) < SCAN;
    let (created, key) = if end { (None, None) } else { last.unzip() };
    tx.execute(
        "UPDATE desktop_question_route_retention SET cursor_created=?1,cursor_key=?2 WHERE id=1",
        params![created, key],
    )?;
    Ok(pruned)
}

/// Room for one more row: a full ledger first runs one retention pass.
fn room(tx: &Connection, at: i64) -> Result<bool> {
    if count(tx)? < ROWS {
        return Ok(true);
    }
    prune(tx, at)?;
    Ok(count(tx)? < ROWS)
}

/// Whether this question identity was ever reserved or refused.
pub(super) fn known(db: &Connection, key: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM desktop_question_routes WHERE key=?1",
            [key],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn insert(tx: &Connection, record: &Record, at: i64, state: &str, code: &str) -> Result<()> {
    ensure!(count(tx)? < ROWS, "question route ledger is full");
    tx.execute(
        "INSERT INTO desktop_question_routes(key,request_id,child_session_id,parent_session_id,
         question_id,created,state,code,run_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            record.identity.key(),
            record.request_id(),
            record.identity.session_id,
            record.parent.session_id,
            record.identity.question_id,
            at,
            state,
            code,
            record.identity.run_id
        ],
    )?;
    Ok(())
}

/// The reservation, in the actor's admission transaction. The person's
/// switches come first, read in this transaction: the global one and the
/// person's latch on this child in the Store (zerus-sa9qn.50), and the
/// child's own on its host (`state`, the CLI state root). Off writes
/// nothing. A full ledger runs one retention pass, and when it is still
/// full the route is not reserved (`Full`, only that pass is kept). Then
/// `reserved` when the parent is under its hourly bound, else the final
/// refusal. Any error writes nothing.
pub(super) fn reserve(
    tx: &Transaction<'_>,
    record: &Record,
    at: i64,
    state: &Path,
) -> Result<Reservation> {
    if !super::policy::enabled(tx)?
        || super::latch::latched(tx, &record.identity.session_id)?
        || crate::state::question_routing::send_to_parent_at(state, &record.identity.session_id)
            != Some(true)
    {
        return Ok(Reservation::Off);
    }
    if !room(tx, at)? {
        return Ok(Reservation::Full);
    }
    let routed: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_question_routes INDEXED BY desktop_question_routes_parent
         WHERE parent_session_id=?1 AND created>?2 AND state<>'refused'",
        params![record.parent.session_id, at - HOUR_MS],
        |r| r.get(0),
    )?;
    if routed >= PER_PARENT_HOUR {
        insert(tx, record, at, "refused", PARENT_BOUND)?;
        return Ok(Reservation::Refused);
    }
    insert(tx, record, at, "reserved", "")?;
    Ok(Reservation::Reserved)
}

/// A final refusal before any admission: the route's fixed text or send is
/// not valid, so it never routes.
pub(super) fn refuse(db: &mut Connection, record: &Record, at: i64, code: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut recorded = true;
    if !known(&tx, &record.identity.key())? {
        recorded = room(&tx, at)?;
        if recorded {
            insert(&tx, record, at, "refused", code)?;
        }
    }
    // A full ledger keeps only its retention pass.
    tx.commit()?;
    ensure!(recorded, "question route ledger is full");
    Ok(())
}

/// What the actor's receipt says about a reserved route; `None` while it is
/// not drained.
pub(super) fn settled(receipt: Option<&Receipt>) -> Option<(&'static str, String)> {
    let Some(receipt) = receipt else {
        return Some(("unknown", ORIGINAL_MISSING.into()));
    };
    if !receipt.drained {
        return None;
    }
    Some(match receipt.phase {
        // The CLI's own exact submission receipt.
        Phase::Recorded => ("submitted", String::new()),
        // The closed no-spawn boundary: verified before any child ran.
        Phase::Failed => ("refused", receipt.code.clone()),
        // Everything else, an `hgs send` refusal without a receipt included,
        // is unknown and never replayed.
        _ => ("unknown", receipt.code.clone()),
    })
}

/// The oldest reserved routes, at most `limit`, from the partial index.
pub(super) fn reserved(db: &Connection, limit: usize) -> Result<Vec<(String, String)>> {
    let mut query = db.prepare_cached(
        "SELECT key,request_id FROM desktop_question_routes INDEXED BY desktop_question_routes_reserved
         WHERE state='reserved' ORDER BY created,key LIMIT ?1",
    )?;
    let rows = query
        .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Settles one reserved route from its actor receipt, once.
pub(super) fn settle(db: &mut Connection, key: &str, state: &str, code: &str) -> Result<()> {
    ensure!(
        ["submitted", "refused", "unknown"].contains(&state) && code.len() <= 80,
        "invalid question route settlement"
    );
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    ensure!(
        tx.execute(
            "UPDATE desktop_question_routes SET state=?1,code=?2 WHERE key=?3 AND state='reserved'",
            params![state, code, key],
        )? == 1,
        "question route settlement conflict"
    );
    tx.commit()?;
    Ok(())
}
