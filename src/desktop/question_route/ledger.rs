//! The route ledger (zerus-sa9qn.8): one row per routed question identity,
//! written before the send child can exist, in the same transaction as the
//! deferred-send actor's original. Rows are retained; a settled row never
//! changes, so a question is never routed twice and an unknown result is
//! never replayed.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use zerus_client_core::desktop::{
    deferred_send::{Phase, Receipt},
    question_route::{Record, PER_PARENT_HOUR},
};

/// The retained rows; beyond this nothing more is routed.
pub(super) const ROWS: i64 = 65_536;
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
    let rows: i64 = tx.query_row("SELECT count(*) FROM desktop_question_routes", [], |r| {
        r.get(0)
    })?;
    ensure!(rows < ROWS, "question route ledger is full");
    tx.execute(
        "INSERT INTO desktop_question_routes(key,request_id,child_session_id,parent_session_id,
         question_id,created,state,code) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            record.identity.key(),
            record.request_id(),
            record.identity.session_id,
            record.parent.session_id,
            record.identity.question_id,
            at,
            state,
            code
        ],
    )?;
    Ok(())
}

/// The reservation, in the actor's admission transaction: `reserved` when
/// the parent is under its hourly bound (`true`), else the final refusal
/// (`false`). Any error writes nothing.
pub(super) fn reserve(tx: &Transaction<'_>, record: &Record, at: i64) -> Result<bool> {
    let routed: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_question_routes INDEXED BY desktop_question_routes_parent
         WHERE parent_session_id=?1 AND created>?2 AND state<>'refused'",
        params![record.parent.session_id, at - HOUR_MS],
        |r| r.get(0),
    )?;
    if routed >= PER_PARENT_HOUR {
        insert(tx, record, at, "refused", PARENT_BOUND)?;
        return Ok(false);
    }
    insert(tx, record, at, "reserved", "")?;
    Ok(true)
}

/// A final refusal before any admission: the route's fixed text or send is
/// not valid, so it never routes.
pub(super) fn refuse(db: &mut Connection, record: &Record, at: i64, code: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if !known(&tx, &record.identity.key())? {
        insert(&tx, record, at, "refused", code)?;
    }
    tx.commit()?;
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
