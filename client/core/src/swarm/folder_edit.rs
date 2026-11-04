//! Qt's folder-link edits on the existing organization value. No discovery,
//! filesystem, CLI or synchronization stack lives here.
use super::{folder_value, Folder, FOLDERS};
use crate::paths::{self, Style};
use serde::Serialize;
use serde_json::{json, Value};

pub const CHANGED: &str = "This project folder changed. Select it again.";
pub const DUPLICATE: &str = "This folder is already in this project.";
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Set {
        original: Option<Folder>,
        id: String,
        machine: String,
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
        id: String,
    },
}
fn project(org: &Value, id: &str) -> Result<usize, String> {
    let list = super::projects(org);
    let matches: Vec<_> = list
        .iter()
        .enumerate()
        .filter(|(_, p)| super::project_id(p) == id)
        .collect();
    match matches.as_slice() {
        [(at, p)] if p["accessible"] != false => Ok(*at),
        _ => Err(super::edit::GONE.into()),
    }
}
fn folders(org: &Value, at: usize) -> Result<&[Value], String> {
    match org["projects"][at].get("folders") {
        None => Ok(&[]),
        Some(Value::Array(values)) if values.len() <= FOLDERS => Ok(values),
        _ => Err("The saved folder list could not be read. It was kept unchanged.".into()),
    }
}
fn source(values: &[Value], expected: &Folder) -> Result<usize, String> {
    let matches: Vec<_> = values
        .iter()
        .enumerate()
        .filter_map(|(at, value)| {
            folder_value(value)
                .filter(|f| f.key == expected.key)
                .map(|f| (at, f))
        })
        .collect();
    match matches.as_slice() {
        [(at, current)] if current == expected => Ok(*at),
        _ => Err(CHANGED.into()),
    }
}
fn duplicate(
    values: &[Value],
    candidate: &Folder,
    except: Option<usize>,
    physical: bool,
    style: Style,
) -> Option<usize> {
    values.iter().enumerate().find_map(|(at, value)| {
        let f = folder_value(value)?;
        let same_machine =
            if physical && !candidate.machine_id.is_empty() && !f.machine_id.is_empty() {
                candidate.machine_id == f.machine_id
            } else {
                candidate.machine == f.machine
            };
        (Some(at) != except && same_machine && paths::same(&candidate.path, &f.path, style))
            .then_some(at)
    })
}
fn set_folders(org: &mut Value, at: usize, values: Vec<Value>) {
    org["projects"][at]["folders"] = Value::Array(values);
}
fn supported(folder: &Folder, style: Style) -> Result<(), String> {
    if !paths::absolute(&folder.path, style)
        || (style == Style::Posix && paths::windows_shaped(&folder.path))
    {
        return Err("Folder management is unavailable for this computer.".into());
    }
    Ok(())
}
pub fn original(org: &Value, project_id: &str, expected: &Folder) -> Result<Value, String> {
    let at = project(org, project_id)?;
    let list = folders(org, at)?;
    Ok(list[source(list, expected)?].clone())
}
pub fn apply(org: &mut Value, project_id: &str, change: &Change) -> Result<(), String> {
    apply_style(org, project_id, change, Style::Posix)
}
/// Only the device-local Windows family uses this; the shared writer stays POSIX.
pub(super) fn apply_windows(
    org: &mut Value,
    project_id: &str,
    change: &Change,
) -> Result<(), String> {
    apply_style(org, project_id, change, Style::Windows)
}
fn apply_style(
    org: &mut Value,
    project_id: &str,
    change: &Change,
    style: Style,
) -> Result<(), String> {
    let at = project(org, project_id)?;
    let mut list = folders(org, at)?.to_vec();
    match change {
        Change::Set {
            original,
            id,
            machine,
            path,
            name,
        } => {
            if machine.is_empty()
                || machine.len() > 512
                || machine.chars().any(char::is_control)
                || path.len() > 32768
                || path.chars().any(char::is_control)
                || !paths::absolute(path, style)
                || (style == Style::Posix && paths::windows_shaped(path))
                || name.chars().any(char::is_control)
            {
                return Err("Choose an absolute folder path on this computer.".into());
            }
            let old = original.as_ref().map(|f| source(&list, f)).transpose()?;
            if let Some(f) = original {
                supported(f, style)?;
            }
            let mut value = old
                .map(|i| list[i].clone())
                .unwrap_or_else(|| json!({"id":id}));
            if value["machine"].as_str() != Some(machine) {
                value.as_object_mut().ok_or(CHANGED)?.remove("machine_id");
                value.as_object_mut().ok_or(CHANGED)?.remove("machine_name");
            }
            value["machine"] = json!(machine);
            value["path"] = json!(paths::clean(path, style));
            value["name"] = json!(name
                .trim()
                .chars()
                .take(super::edit::NAME_CHARS)
                .collect::<String>());
            let candidate = folder_value(&value).ok_or(CHANGED)?;
            if duplicate(&list, &candidate, old, false, style).is_some() {
                return Err(DUPLICATE.into());
            }
            if let Some(i) = old {
                list[i] = value;
            } else {
                if !crate::creation::canonical_uuid(id) || list.len() >= FOLDERS {
                    return Err("This project cannot hold another folder.".into());
                }
                list.push(value);
            }
            set_folders(org, at, list);
        }
        Change::Remove { original } => {
            supported(original, style)?;
            let index = source(&list, original)?;
            list.remove(index);
            set_folders(org, at, list);
        }
        Change::Transfer {
            original,
            destination,
            copy,
            id,
        } => {
            supported(original, style)?;
            if destination == project_id {
                return Err(super::edit::GONE.into());
            }
            let index = source(&list, original)?;
            let to = project(org, destination)?;
            let mut target = folders(org, to)?.to_vec();
            if duplicate(&target, original, None, true, style).is_none() {
                if target.len() >= FOLDERS {
                    return Err("This project cannot hold another folder.".into());
                }
                let mut value = list[index].clone();
                if *copy {
                    if !crate::creation::canonical_uuid(id) {
                        return Err(CHANGED.into());
                    }
                    value["id"] = json!(id);
                }
                target.push(value);
            }
            if !copy {
                list.remove(index);
                set_folders(org, at, list);
            }
            set_folders(org, to, target);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> String {
        "10000000-0000-4000-8000-000000000001".into()
    }
    fn org() -> Value {
        json!({"projects":[{"id":"a","folders":[{"id":"original","machine":"host","path":"/srv/source","name":"","machine_id":"physical-1","machine_name":"Host","private_extension":{"retain":true}}]}, {"id":"b","folders":[]}]})
    }
    fn captured(value: &Value) -> Folder {
        folder_value(&value["projects"][0]["folders"][0]).unwrap()
    }
    #[test]
    fn edit_preserves_id_unknown_fields_and_same_computer_lineage() {
        let mut value = org();
        let original = captured(&value);
        apply(
            &mut value,
            "a",
            &Change::Set {
                original: Some(original),
                id: id(),
                machine: "host".into(),
                path: "/srv/a/../changed/".into(),
                name: " Label ".into(),
            },
        )
        .unwrap();
        let actual = &value["projects"][0]["folders"][0];
        assert_eq!(actual["id"], "original");
        assert_eq!(actual["path"], "/srv/changed");
        assert_eq!(actual["name"], "Label");
        assert_eq!(actual["machine_id"], "physical-1");
        assert_eq!(actual["private_extension"], json!({"retain":true}));
    }
    #[test]
    fn editing_the_computer_clears_old_machine_lineage_only() {
        let mut value = org();
        let original = captured(&value);
        apply(
            &mut value,
            "a",
            &Change::Set {
                original: Some(original),
                id: id(),
                machine: "other".into(),
                path: "/srv/source".into(),
                name: "".into(),
            },
        )
        .unwrap();
        let actual = &value["projects"][0]["folders"][0];
        assert!(actual.get("machine_id").is_none());
        assert!(actual.get("machine_name").is_none());
        assert!(actual["private_extension"].is_object());
    }
    #[test]
    fn a_stale_or_ambiguous_original_never_changes_the_catalog() {
        for field in [
            "id",
            "machine",
            "path",
            "name",
            "machine_id",
            "machine_name",
        ] {
            let mut value = org();
            let old = captured(&value);
            value["projects"][0]["folders"][0][field] = json!("changed");
            let before = value.clone();
            assert!(apply(&mut value, "a", &Change::Remove { original: old }).is_err());
            assert_eq!(value, before);
        }
        let mut value = org();
        let old = captured(&value);
        let copy = value["projects"][0]["folders"][0].clone();
        value["projects"][0]["folders"]
            .as_array_mut()
            .unwrap()
            .push(copy);
        assert!(apply(&mut value, "a", &Change::Remove { original: old }).is_err());
    }
    #[test]
    fn copy_preserves_payload_but_has_a_new_id_and_move_deduplicates_the_target() {
        let mut value = org();
        let old = captured(&value);
        apply(
            &mut value,
            "a",
            &Change::Transfer {
                original: old.clone(),
                destination: "b".into(),
                copy: true,
                id: id(),
            },
        )
        .unwrap();
        assert_eq!(value["projects"][0]["folders"].as_array().unwrap().len(), 1);
        assert_eq!(value["projects"][1]["folders"][0]["id"], id());
        assert_eq!(
            value["projects"][1]["folders"][0]["private_extension"],
            json!({"retain":true})
        );
        apply(
            &mut value,
            "a",
            &Change::Transfer {
                original: old,
                destination: "b".into(),
                copy: false,
                id: id(),
            },
        )
        .unwrap();
        assert!(value["projects"][0]["folders"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(value["projects"][1]["folders"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn add_uses_qt_clean_duplicate_and_capacity_rules() {
        let mut value = org();
        let set = |path: &str| Change::Set {
            original: None,
            id: id(),
            machine: "host".into(),
            path: path.into(),
            name: "".into(),
        };
        assert_eq!(
            apply(&mut value, "a", &set("/srv/./source/")),
            Err(DUPLICATE.into())
        );
        for path in ["relative", "C:\\source", "//server/share", "/srv/a\nother"] {
            assert!(apply(&mut value, "a", &set(path)).is_err());
        }
        apply(&mut value, "b", &set("/srv/new")).unwrap();
        assert_eq!(value["projects"][1]["folders"][0]["id"], id());
        let f = value["projects"][1]["folders"][0].clone();
        value["projects"][1]["folders"] = Value::Array(vec![f; FOLDERS]);
        assert!(apply(&mut value, "b", &set("/srv/another")).is_err());
    }
}
