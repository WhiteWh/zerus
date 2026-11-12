//! The actor owns worktree admission, fixed plans and actual original custody.
//! Its records stay separate from clone and task-start reconciliation.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod consent;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod runtime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use runtime::Runtime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod recovery;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use recovery::Recovery;
#[cfg_attr(windows, allow(dead_code))]
// Windows keeps bounded receipts; this executor is POSIX-only, like clone.
mod ledger;

// Merge-order allocation: the next free Store version when this lands. A
// later schema merge renumbers this one constant, never standalone DDL.
pub(super) const SCHEMA: i64 = 23;
pub(super) const SLOTS_SCHEMA: i64 = 35;
pub(super) fn migrate_slots(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    ledger::migrate_slots(tx)
}
#[cfg(test)]
pub(super) fn rewind_slots(db: &rusqlite::Connection) {
    // Rewind the later schema-36 question route leaf too.
    super::question_route::remove_fixture(db);
    ledger::rewind_slots(db);
}
/// Test rewind of this leaf for older-version upgrade fixtures; dropping the
/// tables drops their triggers and indexes.
#[cfg(test)]
pub(super) fn remove_fixture(db: &rusqlite::Connection) {
    // Rewind the later schema-24 creation launch leaf too, never leaving it
    // labelled older.
    super::creation_launch::remove_fixture(db);
    db.execute_batch(
        "DROP TABLE desktop_creation_worktrees; DROP TABLE desktop_creation_worktree_budget;",
    )
    .unwrap();
}
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    ledger::migrate(tx)
}
pub(super) fn undrained(db: &rusqlite::Connection) -> anyhow::Result<u64> {
    ledger::undrained(db)
}
pub(super) fn summary(db: &rusqlite::Connection) -> anyhow::Result<serde_json::Value> {
    ledger::summary(db)
}
pub(super) fn startup(db: &mut rusqlite::Connection) -> anyhow::Result<()> {
    ledger::startup(db)
}

/// Bounded indexed original-result read. Fresh query scope identifies the
/// service; a historical receipt remains data and grants no new effect.
pub(super) fn query(store: &super::store::Store, value: &serde_json::Value) -> serde_json::Value {
    use anyhow::{ensure, Context};
    use zerus_client_core::desktop::creation_worktree as wire;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Read {
        op: String,
        query: wire::Query,
    }
    let result = (|| -> anyhow::Result<wire::Lookup> {
        let read: Read = serde_json::from_value(value.clone())?;
        ensure!(
            read.op == "creation_worktree_result" && read.query.valid(),
            "invalid creation query"
        );
        let scope: (String, String) = store.db.query_row(
            "SELECT configuration,epoch FROM service WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            scope
                == (
                    read.query.scope.configuration.clone(),
                    read.query.scope.service_epoch.clone()
                ),
            "creation query scope changed"
        );
        let row = ledger::load(&store.db, &read.query.worktree_request_id)?;
        let reply = wire::Lookup {
            abi: wire::ABI.into(),
            request_id: read.query.request_id.clone(),
            scope: read.query.scope.clone(),
            read_only: true,
            found: row.is_some(),
            receipt: row.map(|r| r.receipt),
        };
        ensure!(reply.matches(&read.query), "creation original changed");
        ensure!(
            serde_json::to_vec(&reply).context("creation reply")?.len() <= wire::REPLY_BYTES,
            "creation reply exceeds bound"
        );
        Ok(reply)
    })();
    result.map(|v|serde_json::json!(v)).unwrap_or_else(|_|serde_json::json!({"accepted":false,"read_only":true,"code":"creation_result_unavailable"}))
}

#[cfg(test)]
mod tests;
