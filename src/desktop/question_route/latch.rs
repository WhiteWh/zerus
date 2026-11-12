//! The person's latch on one child's question route (zerus-sa9qn.50,
//! design B). While a child's `person_off` is set, its questions never
//! route, whatever its host file says: each reservation reads the latch in
//! its own transaction beside the global switch and the host file. Only the
//! person's native UI permit (`Request::QuestionRouteChild`) or a phone
//! request the service verifies sets or clears it, by compare-and-swap.
//!
//! Boundary: the latch is person-only against an agent using commands (the
//! CLI, `hgs desktop request`, the connector). Like the global switch, it is
//! kept in the service Store, which the same user can edit directly; that is
//! outside this protection.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use zerus_client_core::desktop::ui_requests::Request;

/// Request receipts kept for an uncertain result's lookup.
const RECEIPTS: i64 = 4096;
/// Latched children kept; beyond this a new latch is refused, never dropped.
pub(super) const CHILDREN: i64 = 65_536;
pub(super) const OP: &str = "question_route_child";
pub(super) const RESULT_OP: &str = "question_route_child_result";
pub(super) const STATUS_OP: &str = "question_route_child_status";

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE desktop_question_route_children(
        session_id TEXT PRIMARY KEY CHECK(length(session_id)=36),
        person_off INTEGER NOT NULL CHECK(person_off IN(0,1)),
        revision INTEGER NOT NULL CHECK(revision>=1));
        CREATE TRIGGER desktop_question_route_child_kept BEFORE DELETE ON desktop_question_route_children BEGIN
        SELECT RAISE(ABORT,'a question route latch is kept'); END;
        CREATE TABLE desktop_question_route_child_requests(
        request_id TEXT PRIMARY KEY CHECK(length(request_id)=36),
        session_id TEXT NOT NULL CHECK(length(session_id)=36),
        person_off INTEGER NOT NULL CHECK(person_off IN(0,1)),
        revision INTEGER NOT NULL CHECK(revision>=0),
        applied INTEGER NOT NULL CHECK(applied IN(0,1)),
        created INTEGER NOT NULL);
        CREATE INDEX desktop_question_route_child_requests_age
        ON desktop_question_route_child_requests(created,request_id);
        CREATE TRIGGER desktop_question_route_child_receipt BEFORE UPDATE ON desktop_question_route_child_requests BEGIN
        SELECT RAISE(ABORT,'a question route latch receipt is immutable'); END;",
    )?;
    Ok(())
}
#[cfg(test)]
pub(super) fn remove_fixture(db: &Connection) {
    db.execute_batch(
        "DROP TABLE desktop_question_route_children; DROP TABLE desktop_question_route_child_requests;",
    )
    .unwrap();
}

/// The child's latch and its revision; an absent row is `(false, 0)`. A
/// corrupt row is an error, never "not latched".
pub(super) fn state(db: &Connection, session_id: &str) -> Result<(bool, i64)> {
    let row: Option<(i64, i64)> = db
        .query_row(
            "SELECT person_off,revision FROM desktop_question_route_children WHERE session_id=?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok((false, 0)),
        Some((off, revision)) => {
            ensure!(
                matches!(off, 0 | 1) && revision >= 1,
                "question route latch changed"
            );
            Ok((off == 1, revision))
        }
    }
}

/// Whether the person latched this child off, read in the caller's
/// transaction.
pub(super) fn latched(db: &Connection, session_id: &str) -> Result<bool> {
    Ok(state(db, session_id)?.0)
}

fn reply(id: &str, session_id: &str, person_off: bool, revision: i64, applied: bool) -> Value {
    json!({"request_id": id, "op": OP, "accepted": true, "durable": true, "session_id": session_id,
        "person_off": person_off, "revision": revision, "applied": applied, "code": ""})
}
fn refusal(id: &str, code: &str) -> Value {
    json!({"request_id": id, "op": OP, "accepted": false, "durable": false, "code": code})
}

fn receipt(db: &Connection, id: &str) -> Result<Option<Value>> {
    Ok(db
        .query_row(
            "SELECT session_id,person_off,revision,applied FROM desktop_question_route_child_requests
             WHERE request_id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(session, off, revision, applied)| reply(id, &session, off == 1, revision, applied == 1)))
}

/// Sets the child's latch to `person_off` for request `id`, once, at
/// `expected_revision` (`None`: whatever it is, a phone's statement). The
/// same request again returns its receipt; another value or child under a
/// used request is refused. A stale revision is the closed `conflict`
/// refusal with the current value; it writes nothing.
pub(super) fn set(
    db: &mut Connection,
    id: &str,
    session_id: &str,
    person_off: bool,
    expected_revision: Option<i64>,
    at: i64,
) -> Result<Value> {
    ensure!(
        super::policy::canonical(id) && super::policy::canonical(session_id),
        "invalid question route latch request"
    );
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(old) = receipt(&tx, id)? {
        ensure!(
            old["session_id"] == session_id && old["person_off"] == person_off,
            "question route latch request reused"
        );
        return Ok(old);
    }
    let (now, revision) = state(&tx, session_id)?;
    if expected_revision.is_some_and(|expected| expected != revision) {
        let mut conflict = refusal(id, "conflict");
        conflict["person_off"] = json!(now);
        conflict["revision"] = json!(revision);
        return Ok(conflict);
    }
    let applied = now != person_off;
    let revision = if applied {
        let next = revision
            .checked_add(1)
            .filter(|r| *r < i64::MAX)
            .ok_or_else(|| anyhow::anyhow!("question route latch revisions exhausted"))?;
        if revision == 0 {
            let kept: i64 = tx.query_row(
                "SELECT count(*) FROM desktop_question_route_children",
                [],
                |r| r.get(0),
            )?;
            if kept >= CHILDREN {
                return Ok(refusal(id, "capacity"));
            }
            tx.execute(
                "INSERT INTO desktop_question_route_children(session_id,person_off,revision)
                 VALUES(?1,?2,?3)",
                params![session_id, person_off, next],
            )?;
        } else {
            ensure!(
                tx.execute(
                    "UPDATE desktop_question_route_children SET person_off=?1,revision=?2
                     WHERE session_id=?3 AND revision=?4",
                    params![person_off, next, session_id, revision],
                )? == 1,
                "question route latch conflict"
            );
        }
        next
    } else {
        revision
    };
    let kept: i64 = tx.query_row(
        "SELECT count(*) FROM desktop_question_route_child_requests",
        [],
        |r| r.get(0),
    )?;
    if kept >= RECEIPTS {
        tx.execute(
            "DELETE FROM desktop_question_route_child_requests WHERE request_id IN
             (SELECT request_id FROM desktop_question_route_child_requests
              INDEXED BY desktop_question_route_child_requests_age ORDER BY created,request_id LIMIT ?1)",
            [kept - RECEIPTS + 1],
        )?;
    }
    tx.execute(
        "INSERT INTO desktop_question_route_child_requests(request_id,session_id,person_off,revision,
         applied,created) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, session_id, person_off, revision, applied, at],
    )?;
    tx.commit()?;
    Ok(reply(id, session_id, person_off, revision, applied))
}

/// The person's native request (`question_route_child`), from either the
/// passive or the background frontend lane. The child must be the one
/// visible local row with this session ID, at the request's run and
/// conversation.
pub(in crate::desktop) fn frontend(
    db: &mut Connection,
    authority: crate::desktop::ui_authority::Validated,
) -> Value {
    let id = authority.id().to_owned();
    let Ok(Request::QuestionRouteChild {
        session_id,
        run_id,
        conversation_id,
        person_off,
        expected_revision,
        ..
    }) = authority.into_request()
    else {
        return refusal(&id, "frontend_identity_expired");
    };
    if !super::policy::canonical(&id) {
        return refusal(&id, "invalid_request_id");
    }
    match super::session_row(db, &session_id) {
        Ok(Some(row)) if row["run_id"] == run_id && row["conversation_id"] == conversation_id => {}
        Ok(Some(_)) => return refusal(&id, "stale_identity"),
        Ok(None) => return refusal(&id, "not_found"),
        Err(_) => return refusal(&id, "question_route_store_refused"),
    }
    let Ok(expected) = i64::try_from(expected_revision) else {
        return refusal(&id, "invalid_request");
    };
    set(
        db,
        &id,
        &session_id,
        person_off,
        Some(expected),
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap_or_else(|_| refusal(&id, "question_route_store_refused"))
}

/// The read-only lookup of one request's receipt
/// (`question_route_child_result`).
pub(in crate::desktop) fn result_request(db: &Connection, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let refused =
        |code: &str| json!({"request_id": id, "accepted": false, "durable": false, "code": code});
    let original = value["original_request_id"].as_str().unwrap_or("");
    let shape = value.as_object().is_some_and(|object| {
        object.len() == 3
            && value["op"] == RESULT_OP
            && super::policy::canonical(id)
            && super::policy::canonical(original)
    });
    if !shape {
        return refused("invalid_question_route_child_request");
    }
    match receipt(db, original) {
        Ok(result) => json!({"request_id": id, "accepted": true, "durable": false, "op": RESULT_OP,
            "original_request_id": original, "found": result.is_some(), "result": result}),
        Err(_) => refused("question_route_store_refused"),
    }
}

/// The read-only latch of one child (`question_route_child_status`), for
/// the person's next compare-and-swap.
pub(in crate::desktop) fn status_request(db: &Connection, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let session = value["session_id"].as_str().unwrap_or("");
    let refused =
        |code: &str| json!({"request_id": id, "accepted": false, "durable": false, "code": code});
    let shape = value.as_object().is_some_and(|object| {
        object.len() == 3
            && value["op"] == STATUS_OP
            && super::policy::canonical(id)
            && super::policy::canonical(session)
    });
    if !shape {
        return refused("invalid_question_route_child_request");
    }
    match state(db, session) {
        Ok((person_off, revision)) => json!({"request_id": id, "accepted": true, "durable": false,
            "op": STATUS_OP, "session_id": session, "person_off": person_off, "revision": revision}),
        Err(_) => refused("question_route_store_refused"),
    }
}

pub(super) const PERSON_OP: &str = "question_route_person";

/// A phone's latch statement (zerus-sa9qn.50), as the connector forwards
/// it. Its fields are the shared crypto crate's `RoutePersonStatement`.
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Statement {
    workspace_id: String,
    device_id: String,
    gateway_id: String,
    session_id: String,
    run_id: String,
    conversation_id: String,
    person_off: bool,
    request_id: String,
    issued_at: u64,
    expires_at: u64,
}

/// `{"op":"question_route_person","statement":{...},"signature":"..."}`:
/// a phone's request through the connector. It applies only what this
/// service's own run of the connector's verifier (`verify`) confirms: a
/// current phone of this workspace signed exactly this statement for this
/// computer, unexpired. The latch is set to the statement's value once per
/// `request_id`, then the child's host file follows (`state`, the CLI state
/// root), so the phone needs one request. The forwarder grants nothing.
pub(in crate::desktop) fn person_request(
    db: &mut Connection,
    state: &std::path::Path,
    value: &Value,
    verify: impl FnOnce(&Value) -> Result<Value>,
) -> Value {
    let id = value["statement"]["request_id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let shape = value.as_object().is_some_and(|o| {
        o.len() == 3 && value["op"] == PERSON_OP && value["signature"].is_string()
    });
    let Some(statement) = shape
        .then(|| serde_json::from_value::<Statement>(value["statement"].clone()).ok())
        .flatten()
        .filter(|s| {
            super::policy::canonical(&s.request_id) && super::policy::canonical(&s.session_id)
        })
    else {
        return refusal(&id, "invalid_request");
    };
    // A request already applied returns its receipt.
    match receipt(db, &id) {
        Ok(Some(old)) => {
            return if old["session_id"] == statement.session_id
                && old["person_off"] == statement.person_off
            {
                old
            } else {
                refusal(&id, "invalid_request")
            }
        }
        Ok(None) => {}
        Err(_) => return refusal(&id, "question_route_store_refused"),
    }
    let input = json!({"statement": statement, "signature": value["signature"]});
    match verify(&input) {
        Ok(reply) if reply["ok"] == true && reply["verified"] == input["statement"] => {}
        _ => return refusal(&id, "not_verified"),
    }
    match super::session_row(db, &statement.session_id) {
        Ok(Some(row))
            if row["run_id"] == statement.run_id
                && row["conversation_id"] == statement.conversation_id => {}
        Ok(Some(_)) => return refusal(&id, "stale_identity"),
        Ok(None) => return refusal(&id, "not_found"),
        Err(_) => return refusal(&id, "question_route_store_refused"),
    }
    let mut reply = match set(
        db,
        &id,
        &statement.session_id,
        statement.person_off,
        None,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(reply) => reply,
        Err(_) => return refusal(&id, "question_route_store_refused"),
    };
    if reply["accepted"] == true {
        // Off already holds through the latch; on needs the host as well.
        reply["host"] = json!(crate::state::question_routing::set_for_person(
            state,
            &statement.session_id,
            !statement.person_off,
        )
        .is_ok());
    }
    reply
}
