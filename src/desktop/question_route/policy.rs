//! The global switch of the owner-approved question route (zerus-sa9qn.32),
//! kept for the desktop service: one row, on by default, changed only by the
//! person's native UI permit (`Request::QuestionRoutes`). The route ledger
//! reads it inside each reservation transaction, so off stops new
//! reservations at once; it never cancels or replays a reserved, sent or
//! unknown route.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use zerus_client_core::desktop::ui_requests::Request;

/// Request receipts kept for an uncertain result's lookup.
const RECEIPTS: i64 = 4096;
const OP: &str = "question_routes";
const RESULT_OP: &str = "question_routes_result";

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE desktop_question_route_policy(
        id INTEGER PRIMARY KEY CHECK(id=1),
        enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),
        revision INTEGER NOT NULL CHECK(revision>=0));
        INSERT INTO desktop_question_route_policy VALUES(1,1,0);
        CREATE TRIGGER desktop_question_route_policy_kept BEFORE DELETE ON desktop_question_route_policy BEGIN
        SELECT RAISE(ABORT,'the question route policy is kept'); END;
        CREATE TABLE desktop_question_route_policy_requests(
        request_id TEXT PRIMARY KEY CHECK(length(request_id)=36),
        enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),
        revision INTEGER NOT NULL CHECK(revision>=0),
        applied INTEGER NOT NULL CHECK(applied IN(0,1)),
        created INTEGER NOT NULL);
        CREATE INDEX desktop_question_route_policy_requests_age
        ON desktop_question_route_policy_requests(created,request_id);
        CREATE TRIGGER desktop_question_route_policy_receipt BEFORE UPDATE ON desktop_question_route_policy_requests BEGIN
        SELECT RAISE(ABORT,'a question route policy receipt is immutable'); END;",
    )?;
    Ok(())
}

/// Whether the person's global switch is on, read in the caller's
/// transaction. A missing or corrupt row is an error, never "on".
pub(super) fn enabled(db: &Connection) -> Result<bool> {
    let (enabled, revision): (i64, i64) = db.query_row(
        "SELECT enabled,revision FROM desktop_question_route_policy WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        matches!(enabled, 0 | 1) && revision >= 0,
        "question route policy changed"
    );
    Ok(enabled == 1)
}

fn current(db: &Connection) -> Result<(bool, i64)> {
    let revision: i64 = db.query_row(
        "SELECT revision FROM desktop_question_route_policy WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    Ok((enabled(db)?, revision))
}

fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| !v.is_nil() && v.hyphenated().to_string() == id)
}

fn reply(id: &str, enabled: bool, revision: i64, applied: bool) -> Value {
    json!({"request_id": id, "op": OP, "accepted": true, "durable": true,
        "enabled": enabled, "revision": revision, "applied": applied, "code": ""})
}

fn receipt(db: &Connection, id: &str) -> Result<Option<Value>> {
    Ok(db
        .query_row(
            "SELECT enabled,revision,applied FROM desktop_question_route_policy_requests WHERE request_id=?1",
            [id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .optional()?
        .map(|(enabled, revision, applied)| reply(id, enabled == 1, revision, applied == 1)))
}

/// Sets the switch to `enabled` for request `id`, once: the same request
/// again returns its receipt; another value under a used request is refused.
pub(super) fn set(db: &mut Connection, id: &str, enabled: bool, at: i64) -> Result<Value> {
    ensure!(canonical(id), "invalid question route request");
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(old) = receipt(&tx, id)? {
        ensure!(old["enabled"] == enabled, "question route request reused");
        return Ok(old);
    }
    let (now, revision) = current(&tx)?;
    let applied = now != enabled;
    let revision = if applied {
        let next = revision.checked_add(1).filter(|r| *r < i64::MAX);
        let next =
            next.ok_or_else(|| anyhow::anyhow!("question route policy revisions exhausted"))?;
        ensure!(
            tx.execute(
                "UPDATE desktop_question_route_policy SET enabled=?1,revision=?2 WHERE id=1 AND revision=?3",
                params![enabled, next, revision],
            )? == 1,
            "question route policy conflict"
        );
        next
    } else {
        revision
    };
    let kept: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_question_route_policy_requests",
        [],
        |r| r.get(0),
    )?;
    if kept >= RECEIPTS {
        tx.execute(
            "DELETE FROM desktop_question_route_policy_requests WHERE request_id IN
             (SELECT request_id FROM desktop_question_route_policy_requests
              INDEXED BY desktop_question_route_policy_requests_age ORDER BY created,request_id LIMIT ?1)",
            [kept - RECEIPTS + 1],
        )?;
    }
    tx.execute(
        "INSERT INTO desktop_question_route_policy_requests(request_id,enabled,revision,applied,created)
         VALUES(?1,?2,?3,?4,?5)",
        params![id, enabled, revision, applied, at],
    )?;
    tx.commit()?;
    Ok(reply(id, enabled, revision, applied))
}

/// The person's native request (`question_routes`), from either the passive
/// or the background frontend lane: it needs no background worker.
pub(in crate::desktop) fn frontend(
    db: &mut Connection,
    authority: crate::desktop::ui_authority::Validated,
) -> Value {
    let id = authority.id().to_owned();
    let refused = |code: &str| json!({"request_id": id, "op": OP, "accepted": false, "durable": false, "code": code});
    let Ok(Request::QuestionRoutes { enabled, .. }) = authority.into_request() else {
        return refused("frontend_identity_expired");
    };
    if !canonical(&id) {
        return refused("invalid_request_id");
    }
    set(db, &id, enabled, chrono::Utc::now().timestamp_millis())
        .unwrap_or_else(|_| refused("question_routes_store_refused"))
}

/// The read-only lookup of one request's receipt (`question_routes_result`).
pub(in crate::desktop) fn result_request(db: &Connection, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let refused =
        |code: &str| json!({"request_id": id, "accepted": false, "durable": false, "code": code});
    let original = value["original_request_id"].as_str().unwrap_or("");
    let shape = value.as_object().is_some_and(|object| {
        object.len() == 3 && value["op"] == RESULT_OP && canonical(id) && canonical(original)
    });
    if !shape {
        return refused("invalid_question_routes_request");
    }
    match receipt(db, original) {
        Ok(result) => json!({"request_id": id, "accepted": true, "durable": false, "op": RESULT_OP,
            "original_request_id": original, "found": result.is_some(), "result": result}),
        Err(_) => refused("question_routes_store_refused"),
    }
}

/// The status projection: the switch on Linux, where the router runs; null
/// elsewhere, so the setting is not shown where nothing routes.
pub(in crate::desktop) fn decorate(db: &Connection, value: &mut Value) {
    value["question_routes"] = if cfg!(target_os = "linux") {
        json!(enabled(db).ok())
    } else {
        Value::Null
    };
}
