//! Qt's settled/suspend/acceptExternal boundary (SwarmDialog.cpp:155-191),
//! inside the maintained organization controller. Unused until the native
//! dispatcher integrates it: no CLI call, timer, persistence or second writer.
use super::{Controller, Observation, GUARD_MS};
use serde_json::Value;
use std::sync::Arc;

pub const EXTERNAL_BUSY: &str = "Wait for project changes to finish syncing.";
pub const EXTERNAL_CHANGED: &str = "The project catalog changed. Preview it again.";

/// An in-process reservation, bound by pointer identity to one controller and
/// one immutable observed snapshot. Never serialized or accepted from the UI.
/// It is not a service lease or permission to execute any operation.
pub struct ExternalTicket {
    owner: Arc<()>,
    snapshot: Arc<Value>,
}

impl std::fmt::Debug for ExternalTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalTicket").finish_non_exhaustive()
    }
}

impl ExternalTicket {
    /// Historical input for the existing CLI's exact preview/resolve guards.
    pub fn snapshot(&self) -> &Value {
        &self.snapshot
    }
}

impl Controller {
    /// Reserve only a current, settled catalog. A held state must be adopted
    /// first; never discard pending edits or checkpoint data to admit a join.
    pub fn begin_external(&mut self, revision: u64) -> Result<ExternalTicket, String> {
        if revision != self.revision {
            return Err(EXTERNAL_CHANGED.into());
        }
        if !self.ready() || self.busy() || self.pending() || self.held.is_some() {
            return Err(EXTERNAL_BUSY.into());
        }
        let snapshot = self.value.as_ref().ok_or(EXTERNAL_BUSY)?;
        if snapshot["initialized"] != true {
            return Err(EXTERNAL_CHANGED.into());
        }
        for key in ["node_id", "swarm_id", "digest"] {
            if snapshot[key]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 512 || s.chars().any(char::is_control))
            {
                return Err(EXTERNAL_CHANGED.into());
            }
        }
        let owner = Arc::new(());
        let ticket = ExternalTicket {
            owner: Arc::clone(&owner),
            snapshot: Arc::clone(snapshot),
        };
        self.external = Some(owner);
        self.revision += 1;
        Ok(ticket)
    }

    /// Complete with a bounded, validated snapshot read AFTER the external
    /// command has finished, on the same native operation lane. A pre-command
    /// cached/topic state is insufficient. Even a failed/unknown command needs
    /// that fresh read: failure alone never releases the fence or retries it.
    /// If late completion is possible, an unknown result must first be settled
    /// by the operation's existing guards; a single read cannot prove that.
    ///
    /// Discard observations held during the call; they can predate its write.
    /// The existing ten-second causal-head guard protects its versions. The
    /// source must also echo the completed catalog identity before ordinary
    /// observations resume; time alone cannot qualify an old catalog.
    /// The native caller must invalidate pre-command source reads before this
    /// completion. This pure reservation does not supply that runtime proof.
    pub fn finish_external(
        &mut self,
        ticket: &ExternalTicket,
        observation: Observation,
        now: u64,
    ) -> Result<(), String> {
        if !self
            .external
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &ticket.owner))
        {
            return Err(EXTERNAL_CHANGED.into());
        }
        let node = observation.value["node_id"].as_str().unwrap_or("");
        let swarm = observation.value["swarm_id"].as_str().unwrap_or("");
        let valid = |s: &str| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control);
        if node != ticket.snapshot["node_id"].as_str().unwrap_or("")
            || !valid(node)
            || !valid(swarm)
            || !valid(&observation.digest)
            || observation.value["initialized"] != true
            || !observation.versions.is_object()
        {
            return Err(EXTERNAL_CHANGED.into());
        }
        let identity = (node.to_owned(), swarm.to_owned());
        let guard = observation
            .versions
            .as_object()
            .cloned()
            .unwrap_or_default();
        self.external = None;
        self.held = None;
        self.guard.clear();
        self.flush_at = None;
        self.refresh_at = None;
        self.tick_pulled = false;
        self.ended = false;
        self.adopt(observation);
        self.guard = guard.into_iter().collect();
        self.external_guard = Some(identity);
        self.guard_until = now.saturating_add(GUARD_MS);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
