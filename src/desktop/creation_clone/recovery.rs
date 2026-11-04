//! One original read-only custody probe at a time, including in a passive
//! actor. SQLite stays on the actor; unknown effects are never started again.
use super::ledger;
use crate::desktop::{custody, store::Location};
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use std::{
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_desktop_ledger::operations::custody::Record;

struct Observed {
    record: Record,
    original: custody::Observation,
    repository: Option<serde_json::Value>,
}
struct Pending {
    id: String,
    record: Record,
    worker: JoinHandle<()>,
    result: mpsc::Receiver<Observed>,
}
pub(in crate::desktop) struct Recovery {
    pending: Option<Pending>,
    after: String,
    next: Instant,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            pending: None,
            after: String::new(),
            next: Instant::now(),
        }
    }
}
impl Recovery {
    pub fn tick(&mut self, location: &Location, db: &mut Connection) -> Result<()> {
        if self
            .pending
            .as_ref()
            .is_some_and(|v| !v.worker.is_finished())
        {
            return Ok(());
        }
        if let Some(pending) = self.pending.take() {
            pending
                .worker
                .join()
                .map_err(|_| anyhow::anyhow!("creation custody observer panicked"))?;
            let observed = pending
                .result
                .try_recv()
                .context("creation custody observation missing")?;
            self.after = pending.id.clone();
            self.next = Instant::now() + Duration::from_millis(250);
            let row = ledger::load(db, &pending.id)?.context("creation original missing")?;
            ensure!(
                row.custody.as_ref() == Some(&pending.record) && pending.record == observed.record,
                "creation custody scope changed"
            );
            match observed.original {
                original @ custody::Observation::Released { .. } => {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = ledger::load(&tx, &pending.id)?.context("creation disappeared")?;
                    if row.receipt.drained {
                        tx.commit()?;
                        return Ok(());
                    }
                    // Reading the original per-request acquisition receipt can
                    // resolve a lost result, never authorize another clone.
                    if let Some(value) = observed.repository {
                        ledger::result(&tx, &row, Some(value), "awaiting_resource_drain")?;
                    }
                    let row = ledger::load(&tx, &pending.id)?.context("creation disappeared")?;
                    ledger::drained(tx, row, original)?;
                }
                custody::Observation::Held => (),
                custody::Observation::Unavailable => {
                    anyhow::bail!("creation original custody unavailable")
                }
            }
        }
        if Instant::now() < self.next {
            return Ok(());
        }
        let row = match ledger::custody_pending(db, &self.after)? {
            Some(row) => Some(row),
            None => {
                self.after.clear();
                ledger::custody_pending(db, "")?
            }
        };
        let Some(row) = row else { return Ok(()) };
        let id = row.verified.plan.clone_request_id.clone();
        let record = row.custody.context("creation has no original custody")?;
        let held = record.clone();
        let location = Location {
            root: location.root.clone(),
            configuration: location.configuration.clone(),
        };
        let state = location
            .root
            .parent()
            .context("missing state root")?
            .to_path_buf();
        let selected = id.clone();
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("creation-custody".into())
            .spawn(move || {
                let original = custody::probe(&location, &held);
                let repository = if matches!(original, custody::Observation::Released { .. }) {
                    crate::state::repositories::read_receipt(&state, &selected).ok()
                } else {
                    None
                };
                let _ = send.send(Observed {
                    record: held,
                    original,
                    repository,
                });
            })?;
        self.pending = Some(Pending {
            id,
            record,
            worker,
            result,
        });
        Ok(())
    }
}
