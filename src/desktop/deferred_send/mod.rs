//! The actor owns deferred send admission, the fixed plan, the original
//! `hgs send` child and its actual custody (zerus-5wmh.66.4.4, slice 3, its
//! first kind). Its records stay separate from every other leaf.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod admitted;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod child;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod receipt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod recovery;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod runtime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use child::adopt_custody;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(in crate::desktop) use child::Ended;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use recovery::{Owned, Recovery};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod target;
#[cfg(target_os = "linux")]
pub(super) use runtime::Routed;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use runtime::Runtime;
#[cfg_attr(windows, allow(dead_code))]
// Windows keeps bounded receipts; this executor is POSIX-only, like clone.
mod ledger;

use anyhow::{ensure, Context};
use zerus_client_core::desktop::deferred_send as wire;

// Merge-order allocation: the next free Store version when this lands. A
// later schema merge renumbers this one constant, never standalone DDL.
pub(super) const SCHEMA: i64 = 28;
/// Test rewind of this leaf for older-version upgrade fixtures, the later
/// deferred answer leaf first; dropping the tables drops their triggers and
/// indexes.
#[cfg(test)]
pub(super) fn remove_fixture(db: &rusqlite::Connection) {
    super::deferred_answer::remove_fixture(db);
    db.execute_batch("DROP TABLE desktop_deferred_sends; DROP TABLE desktop_deferred_send_budget;")
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The saved receipt of one original, read-only: the question route settles
/// its own ledger from it (zerus-sa9qn.8). It grants nothing.
pub(super) fn receipt(
    db: &rusqlite::Connection,
    request_id: &str,
) -> anyhow::Result<Option<wire::Receipt>> {
    Ok(ledger::load(db, request_id)?.map(|row| row.receipt))
}

/// The scoped read-only lookup of one original's receipt
/// (`deferred_send_result`). It never sends, admits or retries anything.
pub(super) fn query(store: &super::store::Store, value: &serde_json::Value) -> serde_json::Value {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Read {
        op: String,
        query: wire::Query,
    }
    let result = (|| -> anyhow::Result<wire::Lookup> {
        let read: Read = serde_json::from_value(value.clone())?;
        ensure!(
            read.op == "deferred_send_result" && read.query.valid(),
            "invalid deferred send query"
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
            "deferred send query scope changed"
        );
        let row = ledger::load(&store.db, &read.query.request_id)?;
        let reply = wire::Lookup {
            abi: wire::ABI.into(),
            request_id: read.query.request_id.clone(),
            scope: read.query.scope.clone(),
            read_only: true,
            found: row.is_some(),
            receipt: row.map(|r| r.receipt),
        };
        ensure!(reply.matches(&read.query), "deferred send original changed");
        ensure!(
            serde_json::to_vec(&reply)
                .context("deferred send reply")?
                .len()
                <= wire::REPLY_BYTES,
            "deferred send reply exceeds bound"
        );
        Ok(reply)
    })();
    result.map(|v| serde_json::json!(v)).unwrap_or_else(|_| {
        serde_json::json!({"accepted":false,"read_only":true,"code":"deferred_send_result_unavailable"})
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
