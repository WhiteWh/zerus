//! Bounded, read-only data for the mockup's Project swarm Computers tab.
//! Reuses the existing observed `swarm get`; no read, poller or command here.
//! Conflict payloads stay in that native snapshot until explicitly reviewed.
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub const COMPUTERS: usize = 512;
/// Aggregate copied string bytes, plus fixed overhead for at most 512 rows.
pub const TEXT_BYTES: usize = 1 << 20;
pub const INVALID: &str = "Invalid project catalog response";
pub const TOO_LARGE: &str = "The project catalog is too large to review.";

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CatalogView {
    pub node_id: String,
    pub swarm_id: String,
    pub digest: String,
    pub initialized: bool,
    pub revision: u64,
    pub conflicts: usize,
    pub computers: Vec<Computer>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Computer {
    pub id: String,
    pub name: String,
    /// Qt's literal "This computer", "Not configured", or configured alias.
    pub access: String,
    pub sync: Sync,
    pub tasks_outdated: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Sync {
    Local,
    Shared,
    Waiting,
    Synced { at: u64 },
    Error { text: String },
}

fn copy(value: &Value, key: &str, maximum: usize, used: &mut usize) -> Result<String, String> {
    let text = match value.get(key) {
        None | Some(Value::Null) => "",
        Some(Value::String(text)) => text,
        _ => return Err(INVALID.into()),
    };
    if text.len() > maximum {
        return Err(TOO_LARGE.into());
    }
    *used = used.checked_add(text.len()).ok_or(TOO_LARGE)?;
    if *used > TEXT_BYTES {
        return Err(TOO_LARGE.into());
    }
    Ok(text.to_owned())
}

/// Refuse an oversized/malformed whole view, never silently show a prefix.
/// These IDs and statuses are descriptive; clicks must recheck current native
/// configuration and the controller reservation before invoking the CLI.
pub fn view(snapshot: &Value, revision: u64) -> Result<CatalogView, String> {
    if snapshot["schema"] != 1 || !snapshot["organization"].is_object() {
        return Err(INVALID.into());
    }
    let machines = snapshot["machines"].as_array().ok_or(INVALID)?;
    let peers = snapshot["peers"].as_object().ok_or(INVALID)?;
    let conflicts = snapshot["conflicts"].as_array().ok_or(INVALID)?.len();
    if machines.len() > COMPUTERS {
        return Err(TOO_LARGE.into());
    }
    let mut used = 0;
    let mut result = CatalogView {
        node_id: copy(snapshot, "node_id", 512, &mut used)?,
        swarm_id: copy(snapshot, "swarm_id", 512, &mut used)?,
        digest: copy(snapshot, "digest", 512, &mut used)?,
        initialized: snapshot["initialized"].as_bool().ok_or(INVALID)?,
        revision,
        conflicts,
        computers: Vec::with_capacity(machines.len()),
    };
    let mut ids = BTreeSet::new();
    for machine in machines {
        let id = copy(machine, "id", 512, &mut used)?;
        if id.is_empty() || !ids.insert(machine["id"].as_str().ok_or(INVALID)?) {
            return Err(INVALID.into());
        }
        let name = copy(machine, "name", 512, &mut used)?;
        let alias = copy(machine, "connection", 512, &mut used)?;
        let local = machine["local"].as_bool().unwrap_or(false);
        let peer = peers.get(&alias).filter(|value| !value.is_null());
        if peer.is_some_and(|value| !value.is_object()) {
            return Err(INVALID.into());
        }
        let error = peer
            .map(|p| copy(p, "error", 4000, &mut used))
            .transpose()?
            .unwrap_or_default();
        let sync = if !error.is_empty() {
            Sync::Error { text: error }
        } else if let Some(peer) = peer.filter(|p| p.as_object().is_some_and(|p| !p.is_empty())) {
            let at = match peer.get("last_sync") {
                None | Some(Value::Null) => 0,
                Some(_) => crate::views::whole(peer, "last_sync").ok_or(INVALID)?,
            };
            if at == 0 {
                Sync::Waiting
            } else {
                Sync::Synced { at }
            }
        } else if local {
            Sync::Local
        } else {
            Sync::Shared
        };
        let tasks_outdated = !matches!(sync, Sync::Error { .. })
            && peer.is_some_and(|p| p["tasks_outdated"] == true);
        result.computers.push(Computer {
            id,
            name,
            access: if local {
                "This computer".into()
            } else if alias.is_empty() {
                "Not configured".into()
            } else {
                alias
            },
            sync,
            tasks_outdated,
        });
    }
    Ok(result)
}
