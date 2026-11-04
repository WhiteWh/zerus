//! Append-only extension of the existing cache leaf's recovery evidence.
use anyhow::Result;

pub(super) const SCHEMA: i64 = 19;

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    zerus_desktop_ledger::operations::cache::migrate_effects(tx)
}
