//! First-G3 local Windows folder preparation. No discovery, native admission,
//! shared overlay, filesystem action or second writer is introduced here.
use super::{windows_folders, Layout, MAX_BYTES};
use crate::creation::canonical_uuid;
use crate::settings::{family_metadata, qt::QtValue, valid_family_metadata, QtSnapshot, QtStore};
use crate::{paths, swarm};
use serde_json::{json, Map, Value};
use std::{collections::BTreeSet, path::Path};

const FAMILY: &str = "local-windows-folders";
const RECORD: &str = "g3_local_windows_folders";
const KEY: &str = swarm::qt_import::KEY;
const REFUSED: &str = "The saved Qt Windows project folders could not be imported. The original is kept; import will retry at the next start.";

/// Independent caller-supplied observations, never inferred from a Qt alias,
/// path, URI host or the candidate rows. The production caller must prove the
/// source's owning catalog node, the current local watch machine_id and the
/// fresh local `swarm get` catalog. This data grants no writer permission.
pub struct WindowsFoldersContext<'a> {
    pub source_node: &'a str,
    pub current_node: &'a str,
    pub catalog: &'a Value,
}

impl WindowsFoldersContext<'_> {
    fn organization(&self) -> Result<&Value, &'static str> {
        if !canonical_uuid(self.source_node)
            || self.source_node != self.current_node
            || self.catalog["node_id"].as_str() != Some(self.current_node)
            || !self.catalog["organization"].is_object()
        {
            return Err(windows_folders::OWNER);
        }
        Ok(&self.catalog["organization"])
    }
}

/// Bounded original row copies and metadata for the existing Layout writer.
/// No Debug/Serialize exposes source paths or lets a caller construct a seed.
pub struct WindowsFoldersSeed {
    candidate: Value,
    record: Value,
}

fn label(text: &str, cap: usize) -> bool {
    !text.is_empty() && text.len() <= cap && !text.chars().any(char::is_control)
}

fn decode(
    store: &QtStore,
    snapshot: Option<&QtSnapshot>,
    context: &WindowsFoldersContext<'_>,
    seed_at: Option<u64>,
) -> Result<WindowsFoldersSeed, String> {
    context.organization()?;
    // Absence is not evidence of an empty family. Discovery/admission must
    // settle the source before this family can become handled.
    let snapshot = snapshot.ok_or(REFUSED)?;
    let bytes = match snapshot.values.get(KEY) {
        Some(QtValue::Bytes(bytes)) => bytes.as_slice(),
        Some(QtValue::Text(text)) => text.as_bytes(),
        _ => return Err(REFUSED.into()),
    };
    let root = swarm::qt_import::decode(bytes).map_err(|_| REFUSED)?;
    let projects = root["projects"].as_array().ok_or(REFUSED)?;
    if projects.len() > swarm::MAX_PROJECTS {
        return Err(REFUSED.into());
    }
    let mut ids = BTreeSet::new();
    let mut local = Map::new();
    for project in projects {
        let id = project["id"]
            .as_str()
            .filter(|id| label(id, 128) && *id != "pinned")
            .ok_or(REFUSED)?;
        if !ids.insert(id) {
            return Err(REFUSED.into());
        }
        let folders = project["folders"].as_array().ok_or(REFUSED)?;
        if folders.len() > swarm::FOLDERS {
            return Err(REFUSED.into());
        }
        let mut selected = Vec::new();
        for folder in folders {
            let path = folder["path"]
                .as_str()
                .filter(|path| label(path, 32768))
                .ok_or(REFUSED)?;
            if !paths::windows_shaped(path) {
                if !paths::absolute(path, paths::Style::Posix) {
                    return Err(REFUSED.into());
                }
                continue;
            }
            let node = folder["machine_id"]
                .as_str()
                .filter(|node| canonical_uuid(node))
                .ok_or(windows_folders::OWNER)?;
            if node == context.source_node {
                // Keep the explicit original machine_id, path, ID and unknown
                // row fields. Never synthesize ownership or normalize evidence.
                selected.push(folder.clone());
            }
        }
        if !selected.is_empty() {
            local.insert(id.into(), Value::Array(selected));
        }
    }
    let candidate = json!({"version":1,"node_id":context.source_node,"projects":local});
    // Reuse #745's complete candidate/live-project/collision checks before
    // returning a preparation; commit repeats them against the fresh catalog.
    Layout::default().initialize_local_windows_folders(
        candidate.clone(),
        context.current_node,
        context.organization()?,
    )?;
    let record = family_metadata(FAMILY, store, Some(snapshot), vec![KEY.into()], seed_at)?;
    Ok(WindowsFoldersSeed { candidate, record })
}

/// Presence and handled state win before any source I/O or context decoding.
/// Call off main and outside the layout/writer lock, then recheck current
/// admission before committing. G1 owns production discovery and admission.
pub fn qt_windows_folders(
    layout: &Layout,
    store: &QtStore,
    target: &Path,
    context: &WindowsFoldersContext<'_>,
    seed_at: Option<u64>,
) -> Result<Option<WindowsFoldersSeed>, String> {
    read_checked(layout, store, target, context, seed_at, || {
        store.snapshot().map_err(|_| REFUSED.into())
    })
}

fn read_checked(
    layout: &Layout,
    store: &QtStore,
    target: &Path,
    context: &WindowsFoldersContext<'_>,
    seed_at: Option<u64>,
    mut read: impl FnMut() -> Result<Option<QtSnapshot>, String>,
) -> Result<Option<WindowsFoldersSeed>, String> {
    if !layout.windows_folders_import_pending()? {
        return Ok(None);
    }
    context.organization()?;
    let canonical = zerus_desktop_migration::canonical(store.path()).map_err(|_| REFUSED)?;
    if zerus_desktop_migration::canonical(target).map_err(|_| REFUSED)? == canonical {
        return Err(REFUSED.into());
    }
    let snapshot = read()?;
    let seed = decode(store, snapshot.as_ref(), context, seed_at)?;
    let fresh = read()?;
    if zerus_desktop_migration::canonical(store.path()).map_err(|_| REFUSED)? != canonical
        || zerus_desktop_migration::canonical(target).map_err(|_| REFUSED)? == canonical
        || fresh.as_ref().map(|s| (s.source, s.sha256.as_str()))
            != snapshot.as_ref().map(|s| (s.source, s.sha256.as_str()))
    {
        return Err(REFUSED.into());
    }
    Ok(Some(seed))
}

impl Layout {
    pub fn windows_folders_import_pending(&self) -> Result<bool, &'static str> {
        if self.has_local_windows_folders() {
            return Ok(false);
        }
        match self.extra.get(RECORD) {
            None => Ok(true),
            Some(value) if valid_family_metadata(FAMILY, value) => Ok(false),
            Some(_) => Err("the saved Windows folder import record is unsupported"),
        }
    }

    /// Whole-ABSENT only. Recheck fresh source/current-node admission and live
    /// projects before changing a clone. Values and the handled record become
    /// one existing Layout checkpoint; a failed write leaves disk unhandled.
    /// Presence acquired during preparation also wins without decoding inputs.
    pub fn seed_windows_folders(
        &mut self,
        seed: WindowsFoldersSeed,
        context: &WindowsFoldersContext<'_>,
    ) -> Result<bool, &'static str> {
        if !self.windows_folders_import_pending()? {
            return Ok(false);
        }
        let mut next = self.clone();
        next.initialize_local_windows_folders(
            seed.candidate,
            context.current_node,
            context.organization()?,
        )?;
        next.extra.insert(RECORD.into(), seed.record);
        if next.to_json().len() as u64 > MAX_BYTES {
            return Err(windows_folders::LIMIT);
        }
        next.dirty = true;
        *self = next;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
