//! The actor owns clone admission, fixed plans and actual original custody.
//! Creation records remain separate from task-start reconciliation bindings.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod lease;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod runtime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use runtime::Runtime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod recovery;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use recovery::Recovery;
#[cfg_attr(windows, allow(dead_code))]
// Windows keeps bounded receipts; this first executor is POSIX-only.
mod ledger;

// Merge-order allocation after the actual notification-controls migration.
// A later cache-leaf merge changes this one constant, never standalone DDL.
pub(super) const SCHEMA: i64 = 17;
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
    use zerus_client_core::desktop::creation_clone as wire;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Read {
        op: String,
        query: wire::Query,
    }
    let result = (|| -> anyhow::Result<wire::Lookup> {
        let read: Read = serde_json::from_value(value.clone())?;
        ensure!(
            read.op == "creation_clone_result" && read.query.valid(),
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
        let row = ledger::load(&store.db, &read.query.clone_request_id)?;
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
