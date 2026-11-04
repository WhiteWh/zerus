//! The existing `hgs swarm preview/join/sync` boundary (SwarmDialog.cpp).
//! Pure bounded decoding and arguments; native owns configuration rechecks,
//! the controller reservation, actual child lifetime and result settlement.
use super::{sync::EXTERNAL_CHANGED, MAX_PROJECTS};
use crate::{fleet::valid_alias, Invocation};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(180);
pub const INVALID: &str = "Invalid project catalog response";
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct Preview {
    alias: String,
    local: [String; 3],
    node: String,
    swarm: String,
    initialized: bool,
    same: bool,
    projects: Vec<Project>,
}
fn label(value: &Value, key: &str, limit: usize) -> Result<String, String> {
    let text = value[key].as_str().ok_or(INVALID)?;
    if text.is_empty() || text.len() > limit || text.chars().any(char::is_control) {
        return Err(INVALID.into());
    }
    Ok(text.into())
}
fn identity(local: &Value) -> Result<[String; 3], String> {
    if local["schema"] != 1
        || !local["initialized"].is_boolean()
        || !local["organization"].is_object()
    {
        return Err(EXTERNAL_CHANGED.into());
    }
    Ok([
        label(local, "node_id", 512)?,
        label(local, "swarm_id", 512)?,
        label(local, "digest", 512)?,
    ])
}
fn projects(value: &Value) -> Result<Vec<Project>, String> {
    let rows = value.as_array().ok_or(INVALID)?;
    if rows.len() > MAX_PROJECTS {
        return Err(super::edit::TOO_MANY.into());
    }
    let mut ids = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let id = label(row, "id", 128)?;
            if !ids.insert(id.clone()) {
                return Err(INVALID.into());
            }
            let name = match row.get("name") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) if s.len() <= 320 && !s.chars().any(char::is_control) => {
                    s.clone()
                }
                _ => return Err(INVALID.into()),
            };
            Ok(Project { id, name })
        })
        .collect()
}
/// Exact project IDs and names; no folders or session memberships cross this boundary.
pub fn local_projects(local: &Value) -> Result<Vec<Project>, String> {
    projects(&local["organization"]["projects"])
}
pub fn preview_invocation(alias: &str) -> Result<Invocation, String> {
    if !valid_alias(alias) {
        return Err("Choose a configured computer.".into());
    }
    Ok(Invocation {
        args: vec!["swarm".into(), "preview".into(), alias.into()],
        stdin: None,
        timeout: NETWORK_TIMEOUT,
        request_id: None,
    })
}
pub fn sync_invocation() -> Invocation {
    Invocation {
        args: vec!["swarm".into(), "sync".into()],
        stdin: None,
        timeout: NETWORK_TIMEOUT,
        request_id: None,
    }
}

/// Saved choices from the maintained `Read::Machines` response. No SSH data
/// or paths are copied into the Swarm dialog and no prefix is selected.
pub fn configured_aliases(list: &Value) -> Result<Vec<String>, String> {
    let rows = list["machines"].as_array().ok_or(INVALID)?;
    if rows.len() > super::dialog::COMPUTERS {
        return Err(super::dialog::TOO_LARGE.into());
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        let alias = row["alias"].as_str().ok_or(INVALID)?;
        if !valid_alias(alias) || !seen.insert(alias) {
            return Err(INVALID.into());
        }
        if list["local"].as_str() != Some(alias) {
            result.push(alias.to_owned());
        }
    }
    Ok(result)
}

/// Bind a Preview to the exact SSH configuration, and repeat this before Join.
/// Peer node/swarm checks remain the existing root CLI's final guard.
pub fn configuration_revision(list: &Value, alias: &str) -> Result<String, String> {
    let revision = list["revision"].as_str().ok_or(INVALID)?;
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || !configured_aliases(list)?
            .iter()
            .any(|listed| listed == alias)
    {
        return Err(crate::machine_connections::CHANGED.into());
    }
    Ok(revision.to_owned())
}
impl Preview {
    pub fn checked(alias: &str, local: &Value, reply: &Value) -> Result<Self, String> {
        preview_invocation(alias)?;
        let local = identity(local)?;
        let node = label(reply, "node_id", 512)?;
        let swarm = label(reply, "swarm_id", 512)?;
        let same = reply["same_swarm"].as_bool().ok_or(INVALID)?;
        if same != (local[1] == swarm) {
            return Err(INVALID.into());
        }
        Ok(Self {
            alias: alias.into(),
            local,
            node,
            swarm,
            same,
            initialized: reply["initialized"].as_bool().ok_or(INVALID)?,
            projects: projects(&reply["projects"])?,
        })
    }
    pub fn alias(&self) -> &str {
        &self.alias
    }
    pub fn initialized(&self) -> bool {
        self.initialized
    }
    pub fn same_swarm(&self) -> bool {
        self.same
    }
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }
    pub fn matches_local(&self, local: &Value) -> bool {
        identity(local).is_ok_and(|identity| identity == self.local)
    }
    /// Read-only settlement evidence after the owned call/resources finish.
    /// Does not authorize retry or establish quiescence on its own.
    pub fn joined_in(&self, current: &Value) -> bool {
        current["initialized"] == true
            && current["node_id"].as_str() == Some(self.local[0].as_str())
            && current["swarm_id"].as_str() == Some(self.swarm.as_str())
            && current["peers"][&self.alias]["node"].as_str() == Some(self.node.as_str())
    }
    /// Root join repeats exact peer node/swarm and live project-map checks.
    /// A new preview is required if the local catalog changed meanwhile.
    pub fn join(
        &self,
        local: &Value,
        mappings: &[(String, String)],
        keep: bool,
    ) -> Result<Invocation, String> {
        if local["initialized"] != true {
            return Err(super::sync::NOT_SET_UP.into());
        }
        if identity(local)? != self.local {
            return Err(EXTERNAL_CHANGED.into());
        }
        if !self.initialized {
            return Err(format!(
                "Open Projects in Zerus on {} first to import its catalog.",
                self.alias
            ));
        }
        if self.node == self.local[0] {
            return Err("cannot join this machine to itself".into());
        }
        if mappings.len() > MAX_PROJECTS || self.same && !mappings.is_empty() {
            return Err(INVALID.into());
        }
        let local = local_projects(local)?;
        let mut map = BTreeMap::new();
        for (from, to) in mappings {
            if !local.iter().any(|p| &p.id == from)
                || !self.projects.iter().any(|p| &p.id == to)
                || map.insert(from, to).is_some()
            {
                return Err(EXTERNAL_CHANGED.into());
            }
        }
        Ok(Invocation {args:vec!["swarm".into(),"join".into(),self.alias.clone()],stdin:Some(serde_json::to_vec(&json!({"node_id":self.node,"swarm_id":self.swarm,"project_map":map,"prefer_peer_memberships":keep})).map_err(|_|INVALID)?),timeout:NETWORK_TIMEOUT,request_id:None})
    }
}
