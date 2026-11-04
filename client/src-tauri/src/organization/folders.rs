//! Project folder links reuse the organization writer, source decoder and
//! native directory opener. Reading/editing a link creates no folder or checkout.
use super::*;
use serde::Deserialize;
use zerus_client_core::swarm::{folder_edit, Folder};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub(super) computer: String,
    pub(super) host: String,
    pub(super) machine_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Add {
        place: Place,
        path: String,
        name: String,
    },
    Edit {
        original: Folder,
        place: Place,
        path: String,
        name: String,
    },
    Remove {
        original: Folder,
    },
    Transfer {
        original: Folder,
        destination: String,
        copy: bool,
    },
}
pub(super) fn machine(pump: &Pump, place: &Place) -> Result<String, String> {
    let current = pump
        .machine(&zerus_client_core::ComputerId(place.computer.clone()))
        .ok_or("Waiting for this computer's identity.")?;
    if current.host.as_deref() != Some(&place.host)
        || current.machine_id != place.machine_id
        || place.host.is_empty()
    {
        return Err("The selected computer changed. Select it again.".into());
    }
    if place.computer.is_empty()
        && (current.local_sessions == Some(false)
            || zerus_client_core::paths::of(current.platform.as_deref(), true)
                != zerus_client_core::paths::Style::Posix)
    {
        return Err("Folder management is unavailable for this computer.".into());
    }
    Ok(if place.computer.is_empty() {
        place.host.clone()
    } else {
        place.computer.clone()
    })
}
fn intent(pump: &Pump, project: String, change: Change, id: String) -> Result<Intent, String> {
    let change = match change {
        Change::Add { place, path, name } => folder_edit::Change::Set {
            original: None,
            id,
            machine: machine(pump, &place)?,
            path,
            name,
        },
        Change::Edit {
            original,
            place,
            path,
            name,
        } => folder_edit::Change::Set {
            original: Some(original),
            id,
            machine: machine(pump, &place)?,
            path,
            name,
        },
        Change::Remove { original } => folder_edit::Change::Remove { original },
        Change::Transfer {
            original,
            destination,
            copy,
        } => folder_edit::Change::Transfer {
            original,
            destination,
            copy,
            id,
        },
    };
    Ok(Intent::Folder { project, change })
}
#[derive(serde::Serialize)]
pub struct Changed {
    #[serde(flatten)]
    outcome: EditOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
}
#[tauri::command]
pub async fn project_folder_change(
    state: State<'_, Organization>,
    project: String,
    change: Change,
) -> Result<Changed, String> {
    let org = state.inner().clone();
    let id = uuid::Uuid::new_v4().to_string();
    let folder = match &change {
        Change::Add { .. } => Some(id.clone()),
        Change::Edit {
            original,
            place,
            path,
            ..
        } => Some(if !original.id.is_empty() {
            original.id.clone()
        } else {
            format!(
                "{}\n{}",
                if place.computer.is_empty() {
                    &place.host
                } else {
                    &place.computer
                },
                zerus_client_core::paths::clean(path, zerus_client_core::paths::Style::Posix)
            )
        }),
        _ => None,
    };
    tauri::async_runtime::spawn_blocking(move || Changed {
        outcome: org.submit_checked(move |pump| intent(pump, project, change, id)),
        folder,
    })
    .await
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn project_folder_open(
    state: State<'_, Organization>,
    folders: State<'_, crate::session_folders::SessionFolders>,
    project: String,
    original: Folder,
) -> Result<(), String> {
    if !state.0.allowed {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let org = state.inner().clone();
    let opener = folders.opener();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let hub = org.lock();
            let (current, _) = hub.sync.presented().ok_or(folder_edit::CHANGED)?;
            folder_edit::original(current, &project, &original)?;
            let local = org
                .0
                .pump
                .machine(&zerus_client_core::ComputerId::local())
                .ok_or("Waiting for this computer's identity.")?;
            if zerus_client_core::paths::of(local.platform.as_deref(), true)
                != zerus_client_core::paths::Style::Posix
            {
                return Err("Folder management is unavailable for this computer.".into());
            }
            if local.host.as_deref() != Some(&original.machine)
                || (!original.machine_id.is_empty()
                    && local.machine_id.as_deref() != Some(&original.machine_id))
            {
                return Err(format!(
                    "Open this folder from Zerus on {}.",
                    original.machine
                ));
            }
        }
        let path = std::path::Path::new(&original.path);
        if !path.is_absolute() || !path.is_dir() {
            return Err("This folder is unavailable on this computer.".into());
        }
        if !(opener.open)(path) {
            return Err("Could not open the folder in the file manager.".into());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
