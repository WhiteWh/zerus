//! The read-only Activity lines of submitted question routes
//! (zerus-sa9qn.33): for one session, the newest routes the actor submitted
//! where it is the child or the parent, each with the child's name and
//! reference, the parent's name and the route's summary. The child's text
//! appears only as the route's own preview of at most
//! [`zerus_client_core::desktop::question_route::PREVIEW_CHARS`] characters;
//! the parent's line also carries the exact text it received, so its own
//! "You" message can fold into the line on an exact match.
//!
//! It reads the route ledger by its schema-38 indexes and each route's own
//! saved original. It never sends, settles, retries or grants anything, and
//! refused, reserved or unknown routes are not shown in this version.
use rusqlite::{params, Connection, Transaction};
use serde_json::{json, Value};

pub(super) const OP: &str = "question_route_activity";
/// The newest submitted routes one request returns.
pub(super) const LIMIT: usize = 20;

pub(super) fn migrate(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.execute_batch(
        "CREATE INDEX desktop_question_routes_child_submitted
        ON desktop_question_routes(child_session_id,created,request_id) WHERE state='submitted';
        CREATE INDEX desktop_question_routes_parent_submitted
        ON desktop_question_routes(parent_session_id,created,request_id) WHERE state='submitted';",
    )?;
    Ok(())
}
#[cfg(test)]
pub(super) fn remove_fixture(db: &Connection) {
    db.execute_batch(
        "DROP INDEX desktop_question_routes_child_submitted;
        DROP INDEX desktop_question_routes_parent_submitted;",
    )
    .unwrap();
}

/// The two exact reads, each at most [`LIMIT`] rows of its own index.
pub(super) const CHILD_SQL: &str = "SELECT request_id,child_session_id,parent_session_id,created
    FROM desktop_question_routes INDEXED BY desktop_question_routes_child_submitted
    WHERE child_session_id=?1 AND state='submitted' ORDER BY created DESC,request_id DESC LIMIT ?2";
pub(super) const PARENT_SQL: &str = "SELECT request_id,child_session_id,parent_session_id,created
    FROM desktop_question_routes INDEXED BY desktop_question_routes_parent_submitted
    WHERE parent_session_id=?1 AND state='submitted' ORDER BY created DESC,request_id DESC LIMIT ?2";

struct Route {
    request_id: String,
    child: String,
    parent: String,
    created: i64,
}

fn newest(db: &Connection, sql: &str, session: &str) -> rusqlite::Result<Vec<Route>> {
    db.prepare_cached(sql)?
        .query_map(params![session, LIMIT as i64], |r| {
            Ok(Route {
                request_id: r.get(0)?,
                child: r.get(1)?,
                parent: r.get(2)?,
                created: r.get(3)?,
            })
        })?
        .collect()
}

/// The newest [`LIMIT`] submitted routes of `session`, newest first. A
/// route whose original is missing or does not name exactly this ledger
/// row is left out.
fn routes(db: &Connection, session: &str) -> anyhow::Result<Vec<Value>> {
    let mut rows = newest(db, CHILD_SQL, session)?;
    rows.extend(newest(db, PARENT_SQL, session)?);
    rows.sort_by(|a, b| (b.created, &b.request_id).cmp(&(a.created, &a.request_id)));
    rows.truncate(LIMIT);
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Ok(Some(record)) = crate::desktop::deferred_send::route_record(db, &row.request_id)
        else {
            continue;
        };
        if !record.valid()
            || record.request_id() != row.request_id
            || record.identity.session_id != row.child
            || record.parent.session_id != row.parent
        {
            continue;
        }
        let child = row.child == session;
        let mut line = json!({
            "request_id": row.request_id,
            "at": row.created,
            "role": if child { "child" } else { "parent" },
            "child": {"name": record.child.name, "reference": record.child.reference,
                "session_id": row.child},
            "parent": {"name": record.parent.session, "session_id": row.parent},
            "question": record.question,
        });
        if !child {
            line["text"] = Value::String(record.text());
        }
        out.push(line);
    }
    Ok(out)
}

/// `{"op":"question_route_activity","request_id":UUID,"session_id":UUID}`:
/// exactly these keys and canonical UUIDs, else a closed refusal.
pub(in crate::desktop) fn request(db: &Connection, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let session = value["session_id"].as_str().unwrap_or("");
    let refused =
        |code: &str| json!({"request_id": id, "accepted": false, "durable": false, "code": code});
    let shape = value.as_object().is_some_and(|object| {
        object.len() == 3
            && value["op"] == OP
            && super::policy::canonical(id)
            && super::policy::canonical(session)
    });
    if !shape {
        return refused("invalid_question_route_activity");
    }
    match routes(db, session) {
        Ok(routes) => json!({"request_id": id, "accepted": true, "durable": false, "op": OP,
            "session_id": session, "routes": routes}),
        Err(_) => refused("question_route_store_refused"),
    }
}
