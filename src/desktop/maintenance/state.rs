//! One bounded actor-owned row. Recent receipts expire; unknown edits never replay.
use anyhow::{ensure, Result};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use zerus_client_core::maintenance::{Configuration, UpdateProjection};

pub(super) const LIMIT: usize = 2 * 1024 * 1024;
pub(super) const REQUESTS: usize = 4096;
pub(super) const UNKNOWN_REQUESTS: usize = 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub fingerprint: String,
    pub revision: u64,
    pub recovery_save: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stored {
    pub revision: u64,
    pub configuration: Option<Configuration>,
    pub receipts: BTreeMap<String, Receipt>,
    pub updates: BTreeMap<String, UpdateProjection>,
    pub policy: Map<String, Value>,
}
pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE desktop_maintenance(id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL CHECK(length(CAST(value AS BLOB))<=2097152));")?;
    db.execute(
        "INSERT INTO desktop_maintenance VALUES(1,?1)",
        [serde_json::to_string(&Stored::default())?],
    )?;
    Ok(())
}
pub(super) fn load(db: &Connection) -> Result<Stored> {
    // Check the borrowed SQLite value before making an owned copy.
    let value = db.query_row(
        "SELECT value FROM desktop_maintenance WHERE id=1",
        [],
        |row| {
            let text = row
                .get_ref(0)?
                .as_str()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if text.len() > LIMIT {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(text.to_owned())
        },
    )?;
    let stored: Stored = serde_json::from_str(&value)?;
    ensure!(
        stored.receipts.len() <= REQUESTS + UNKNOWN_REQUESTS
            && stored
                .receipts
                .values()
                .filter(|r| r.recovery_save == "unknown")
                .count()
                <= UNKNOWN_REQUESTS
            && stored
                .receipts
                .values()
                .filter(|r| r.recovery_save != "unknown")
                .count()
                <= REQUESTS
            && stored.updates.len() <= 3,
        "invalid maintenance row"
    );
    ensure!(
        stored
            .configuration
            .as_ref()
            .is_none_or(Configuration::valid),
        "invalid maintenance configuration"
    );
    ensure!(
        stored
            .configuration
            .as_ref()
            .and_then(|c| c.recovery.as_ref())
            .is_none_or(|c| c.policy.is_none()),
        "unconsumed recovery edit"
    );
    Ok(stored)
}
pub(super) fn save(db: &mut Connection, stored: &mut Stored) -> Result<()> {
    // Ordinary metadata/preferences have a recency window, not a lifetime cap.
    // Unknown edits keep their original reservation separately: their absence
    // must never invite replay, and they cannot freeze ordinary GUI upgrades.
    let mut completed: Vec<_> = stored
        .receipts
        .iter()
        .filter(|(_, r)| r.recovery_save != "unknown")
        .map(|(id, r)| (r.revision, id.clone()))
        .collect();
    completed.sort_unstable();
    for (_, id) in completed
        .iter()
        .take(completed.len().saturating_sub(REQUESTS))
    {
        stored.receipts.remove(id);
    }
    ensure!(
        stored.receipts.len() <= REQUESTS + UNKNOWN_REQUESTS
            && stored
                .receipts
                .values()
                .filter(|r| r.recovery_save == "unknown")
                .count()
                <= UNKNOWN_REQUESTS
            && stored
                .receipts
                .values()
                .filter(|r| r.recovery_save != "unknown")
                .count()
                <= REQUESTS
            && stored.updates.len() <= 3,
        "maintenance history full"
    );
    let value = serde_json::to_string(stored)?;
    ensure!(value.len() <= LIMIT, "maintenance row full");
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure!(
        tx.execute(
            "UPDATE desktop_maintenance SET value=?1 WHERE id=1",
            [value]
        )? == 1,
        "maintenance row missing"
    );
    tx.commit()?;
    Ok(())
}
