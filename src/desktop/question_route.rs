//! The owner-approved, service-originated route of a supervised child's
//! blocking question to its live parent session (zerus-sa9qn.8, slice a).
//!
//! Authority: the owner's standing approval of 2026-11-12 lets this service,
//! on the child's own computer, originate one guarded `hgs send` of a
//! supervised child's blocking question to that child's live parent, with no
//! per-message confirmation by the person. It is not a person's consent and
//! grants nothing else: no UI permit is built, nothing is answered, selected
//! or approved, and who is alerted for a human signal never changes. The
//! person's deferred sends are unchanged.
//!
//! The router observes the local catalog independently of the notification
//! setting. A supervised, quiet Claude child waiting for an answer is an edge
//! ([`zerus_client_core::desktop::question_route::edge`]); after a short
//! settle it is read once with a bounded `hgs inspect`, and read again
//! immediately before the reservation. Only Claude's own AskUserQuestion of
//! the main agent routes. The deferred-send actor runs the one fixed send
//! ([`super::deferred_send::Runtime::route`]); this leaf's ledger reserves
//! each question identity in the same transaction, and settles it from the
//! actor's receipt only: `submitted`, `refused` (verified before any child
//! ran, or the per-parent bound) or `unknown`. Nothing is retried or
//! replayed. v1 routes on Linux only.
// The router runs on Linux only in v1; elsewhere only the schema exists.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod ledger;
/// The person's global switch (zerus-sa9qn.32).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod policy;
pub(super) use policy::{decorate, frontend, result_request};
/// The read-only Activity lines of submitted routes (zerus-sa9qn.33).
mod activity;
pub(super) use activity::request as activity_request;
/// The person's per-child latch (zerus-sa9qn.50).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod latch;
pub(super) use latch::{
    frontend as latch_frontend, result_request as latch_result, status_request as latch_status,
};
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
pub(super) use runtime::Runtime;

// Merge-order allocation: the next free Store version when this lands. A
// later schema merge renumbers this one constant, never standalone DDL.
pub(super) const SCHEMA: i64 = 36;
/// The global switch's leaf (zerus-sa9qn.32); merge-order allocation as above.
pub(super) const POLICY_SCHEMA: i64 = 37;
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    ledger::migrate(tx)
}
pub(super) fn migrate_policy(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    policy::migrate(tx)
}
/// The Activity leaf's indexes (zerus-sa9qn.33); merge-order allocation as above.
pub(super) const ACTIVITY_SCHEMA: i64 = 38;
pub(super) fn migrate_activity(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    activity::migrate(tx)
}
/// The person's per-child latch leaf (zerus-sa9qn.50); merge-order
/// allocation as above.
pub(super) const LATCH_SCHEMA: i64 = 39;
pub(super) fn migrate_latch(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    latch::migrate(tx)
}
/// Test rewind of the schema-39 latch leaf.
#[cfg(test)]
pub(super) fn remove_latch_fixture(db: &rusqlite::Connection) {
    latch::remove_fixture(db);
}
/// Test rewind of the schema-38 Activity leaf, the later latch leaf first.
#[cfg(test)]
pub(super) fn remove_activity_fixture(db: &rusqlite::Connection) {
    remove_latch_fixture(db);
    activity::remove_fixture(db);
}
/// The one visible local row with this session ID, from the indexed session
/// expression; none when absent or ambiguous.
pub(super) fn session_row(
    db: &rusqlite::Connection,
    session_id: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let mut query = db.prepare_cached(
        "SELECT r.json FROM desktop_catalog_sources s JOIN desktop_catalog_rows r
         ON r.generation=s.visible
         AND (CASE WHEN json_valid(r.json) THEN json_extract(r.json,'$.session_id') END)=?2
         WHERE s.computer=?1 AND length(CAST(r.json AS BLOB))<=?3 LIMIT 2",
    )?;
    let rows = query
        .query_map(
            rusqlite::params!["", session_id, zerus_desktop_catalog::PAGE_BYTES as i64],
            |r| r.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match rows.as_slice() {
        [raw] => serde_json::from_str(raw).ok(),
        _ => None,
    })
}
/// Test rewind of the schema-37 switch leaf, the later Activity leaf first.
#[cfg(test)]
pub(super) fn remove_policy_fixture(db: &rusqlite::Connection) {
    remove_activity_fixture(db);
    db.execute_batch(
        "DROP TABLE desktop_question_route_policy; DROP TABLE desktop_question_route_policy_requests;",
    )
    .unwrap();
}
/// Test rewind of this leaf for older-version upgrade fixtures; dropping the
/// table drops its triggers and indexes.
#[cfg(test)]
pub(super) fn remove_fixture(db: &rusqlite::Connection) {
    // Rewind the later schema-37 switch leaf too.
    remove_policy_fixture(db);
    db.execute_batch(
        "DROP TABLE desktop_question_routes; DROP INDEX desktop_question_route_sessions;",
    )
    .unwrap();
}

#[cfg(test)]
mod tests;
