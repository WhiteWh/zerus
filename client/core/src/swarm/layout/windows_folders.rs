//! Device-local drive/UNC metadata, as Qt's SwarmController localOnly/presentation.
//! No source reader, I/O, migration writer, filesystem or launch authority.
//! Callers must prove current Pump hello.machine_id == organization.node_id.
use super::{Layout, MAX_BYTES};
use crate::{
    creation::canonical_uuid,
    paths::{self, Style},
    swarm::{self, folder_edit},
};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
pub const KEY: &str = "local_windows_folders";
pub const INVALID: &str =
    "The local Windows project folders are unsupported or malformed. The original is kept.";
pub const OWNER: &str =
    "These local project folders belong to a different or unconfirmed computer.";
pub const LIMIT: &str =
    "The local project folders exceed their supported limits. Nothing was changed.";
pub const AMBIGUOUS: &str =
    "The local project folders repeat a folder identity or Windows path. Nothing was changed.";
#[cfg(test)]
mod tests;

struct Family<'a> {
    node: &'a str,
    projects: &'a Map<String, Value>,
}
fn label(s: &str, cap: usize) -> bool {
    !s.is_empty() && s.len() <= cap && !s.chars().any(char::is_control)
}
fn bounded(value: &Value) -> bool {
    let mut output = crate::fsx::checkpoint::Bounded {
        bytes: Vec::new(),
        limit: MAX_BYTES as usize,
    };
    serde_json::to_writer(&mut output, value).is_ok()
}
fn family(value: &Value) -> Result<Family<'_>, &'static str> {
    if !bounded(value) {
        return Err(LIMIT);
    }
    if value["version"].as_u64() != Some(1) {
        return Err(INVALID);
    }
    let node = value["node_id"]
        .as_str()
        .filter(|s| canonical_uuid(s))
        .ok_or(INVALID)?;
    let projects = value["projects"].as_object().ok_or(INVALID)?;
    if projects.len() > swarm::MAX_PROJECTS {
        return Err(LIMIT);
    }
    let mut ids = BTreeSet::new();
    for (project, entries) in projects {
        if !label(project, 128) {
            return Err(INVALID);
        }
        let entries = entries.as_array().ok_or(INVALID)?;
        if entries.len() > swarm::FOLDERS {
            return Err(LIMIT);
        }
        let mut paths_seen = BTreeSet::new();
        for entry in entries {
            let folder = swarm::folder_value(entry).ok_or(INVALID)?;
            if !label(&folder.id, 128)
                || !label(&folder.machine, 512)
                || !label(&folder.path, 32768)
                || !paths::absolute(&folder.path, Style::Windows)
                || folder.machine_id != node
                || entry.get("name").is_some_and(|v| {
                    !v.as_str()
                        .is_some_and(|s| s.len() <= 1280 && !s.chars().any(char::is_control))
                })
                || entry.get("machine_name").is_some_and(|v| {
                    !v.as_str()
                        .is_some_and(|s| s.len() <= 512 && !s.chars().any(char::is_control))
                })
            {
                return Err(INVALID);
            }
            if !ids.insert(folder.id)
                || !paths_seen.insert(paths::comparison_key(&folder.path, Style::Windows))
            {
                return Err(AMBIGUOUS);
            }
        }
    }
    Ok(Family { node, projects })
}
/// Validates presence, including null/future/malformed values; none is absence.
pub(super) fn validate(value: &Value) -> Result<(), &'static str> {
    family(value).map(|_| ())
}
fn live(organization: &Value) -> Result<BTreeSet<&str>, &'static str> {
    let projects = organization["projects"].as_array().ok_or(INVALID)?;
    if projects.len() > swarm::MAX_PROJECTS {
        return Err(LIMIT);
    }
    let mut ids = BTreeSet::new();
    for project in projects {
        let id = project["id"]
            .as_str()
            .filter(|id| label(id, 128))
            .ok_or(INVALID)?;
        if !ids.insert(id) {
            return Err(INVALID);
        }
    }
    Ok(ids)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Initialized {
    Inserted,
    AlreadyPresent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Absent,
    Active,
    Dormant,
}
fn overlay_family(
    value: Option<&Value>,
    organization: &Value,
    current_node: &str,
) -> Result<Value, &'static str> {
    live(organization)?;
    let mut shown = organization.clone();
    let Some(value) = value else {
        return Ok(shown);
    };
    let local = family(value)?;
    if !canonical_uuid(current_node) || local.node != current_node {
        return Ok(shown);
    }
    let mut all_ids: BTreeSet<String> = swarm::projects(organization)
        .iter()
        .flat_map(|p| p["folders"].as_array().into_iter().flatten())
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect();
    for project in shown["projects"].as_array_mut().ok_or(INVALID)? {
        let Some(entries) = local
            .projects
            .get(swarm::project_id(project))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let shared = project
            .as_object_mut()
            .ok_or(INVALID)?
            .entry("folders")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or(INVALID)?;
        if shared.len() + entries.len() > swarm::FOLDERS {
            return Err(LIMIT);
        }
        let mut ids: BTreeSet<String> = shared
            .iter()
            .map(|f| f["id"].as_str().unwrap_or("").to_owned())
            .collect();
        for folder in entries {
            if !all_ids.insert(folder["id"].as_str().ok_or(INVALID)?.to_owned())
                || !ids.insert(folder["id"].as_str().ok_or(INVALID)?.to_owned())
            {
                return Err(AMBIGUOUS);
            }
            shared.push(folder.clone());
        }
    }
    Ok(shown)
}

impl Layout {
    /// Presence is independent of emptiness or support. G3 must never turn an
    /// unsupported target into absence or map source rows by an alias/path.
    pub fn has_local_windows_folders(&self) -> bool {
        self.extra.contains_key(KEY)
    }
    pub fn local_windows_folders_state(&self, current_node: &str) -> Result<State, &'static str> {
        let Some(value) = self.extra.get(KEY) else {
            return Ok(State::Absent);
        };
        Ok(
            if family(value)?.node == current_node && canonical_uuid(current_node) {
                State::Active
            } else {
                State::Dormant
            },
        )
    }
    fn install_windows_folders(&mut self, value: Value) -> Result<(), &'static str> {
        validate(&value)?;
        let mut next = self.clone();
        next.extra.insert(KEY.into(), value);
        if next.to_json().len() as u64 > MAX_BYTES {
            return Err(LIMIT);
        }
        next.dirty = true;
        *self = next;
        Ok(())
    }
    /// Pure whole-ABSENT initializer. G3 owns source proof, exact current-node
    /// admission, durable family checkpoint and handled/crash-retry evidence.
    /// Presence wins before candidate/source decoding. These RAM outcomes
    /// grant neither validity, durability nor a handled-record admission.
    pub fn initialize_local_windows_folders(
        &mut self,
        candidate: Value,
        current_node: &str,
        organization: &Value,
    ) -> Result<Initialized, &'static str> {
        if self.has_local_windows_folders() {
            return Ok(Initialized::AlreadyPresent);
        }
        let candidate_view = family(&candidate)?;
        if !canonical_uuid(current_node) || candidate_view.node != current_node {
            return Err(OWNER);
        }
        let projects = live(organization)?;
        if candidate_view
            .projects
            .keys()
            .any(|id| !projects.contains(id.as_str()))
        {
            return Err(swarm::edit::GONE);
        }

        overlay_family(Some(&candidate), organization, current_node)?;
        self.install_windows_folders(candidate)?;
        Ok(Initialized::Inserted)
    }
    /// A copy for presentation only. Never feed the overlay to shared desired,
    /// apply or source evidence. Missing observations never erase stored rows.
    pub fn overlay_local_windows_folders(
        &self,
        organization: &Value,
        current_node: &str,
    ) -> Result<Value, &'static str> {
        overlay_family(self.extra.get(KEY), organization, current_node)
    }
    /// Metadata only, routed separately from folder_edit::apply (shared POSIX).
    /// Native callers must bind current local node/platform before entering.
    pub fn edit_local_windows_folder(
        &mut self,
        organization: &Value,
        current_node: &str,
        project: &str,
        place_machine_id: Option<&str>,
        change: &folder_edit::Change,
    ) -> Result<(), String> {
        if !canonical_uuid(current_node) {
            return Err(OWNER.into());
        }
        let live = live(organization)?;
        if !live.contains(project)
            || swarm::projects(organization)
                .iter()
                .find(|p| swarm::project_id(p) == project)
                .is_none_or(|p| p["accessible"] == false)
        {
            return Err(swarm::edit::GONE.into());
        }
        let mut value = self
            .extra
            .get(KEY)
            .cloned()
            .unwrap_or_else(|| json!({"version":1,"node_id":current_node,"projects":{}}));
        let parsed = family(&value)?;
        if parsed.node != current_node {
            return Err(OWNER.into());
        }
        let mut names: BTreeSet<String> = live.iter().map(|s| s.to_string()).collect();
        names.extend(parsed.projects.keys().cloned());
        let mut temporary = json!({"projects":names.iter().map(|id|json!({"id":id,"accessible":live.contains(id.as_str())&&swarm::projects(organization).iter().find(|p|swarm::project_id(p)==id).is_some_and(|p|p["accessible"]!=false),"folders":parsed.projects.get(id).cloned().unwrap_or_else(||json!([]))})).collect::<Vec<_>>()});
        if let folder_edit::Change::Set { original, .. } = change {
            if place_machine_id != Some(current_node)
                || original
                    .as_ref()
                    .is_some_and(|f| f.machine_id != current_node)
            {
                return Err(OWNER.into());
            }
        }
        folder_edit::apply_windows(&mut temporary, project, change)?;
        if let folder_edit::Change::Set { original, id, .. } = change {
            let id = original.as_ref().map(|f| f.id.as_str()).unwrap_or(id);
            for p in temporary["projects"].as_array_mut().ok_or(INVALID)? {
                for folder in p["folders"].as_array_mut().ok_or(INVALID)? {
                    if folder["id"] == id {
                        folder["machine_id"] = json!(place_machine_id.ok_or(OWNER)?);
                    }
                }
            }
        }
        let projects = value["projects"].as_object_mut().ok_or(INVALID)?;
        for p in temporary["projects"].as_array().ok_or(INVALID)? {
            let id = swarm::project_id(p);
            if projects.contains_key(id)
                || !p["folders"].as_array().ok_or(INVALID)?.is_empty()
                || id == project
            {
                projects.insert(id.into(), p["folders"].clone());
            }
        }
        overlay_family(Some(&value), organization, current_node)?;
        self.install_windows_folders(value)?;
        Ok(())
    }
    /// Explicit project deletion metadata only. A missing catalog observation
    /// never calls this; native deletion ordering/admission remains its owner's.
    pub fn remove_local_windows_project(
        &mut self,
        current_node: &str,
        project: &str,
    ) -> Result<(), &'static str> {
        let Some(mut value) = self.extra.get(KEY).cloned() else {
            return Ok(());
        };
        if family(&value)?.node != current_node || !canonical_uuid(current_node) {
            return Err(OWNER);
        }
        value["projects"]
            .as_object_mut()
            .ok_or(INVALID)?
            .remove(project);
        self.install_windows_folders(value)
    }
}
