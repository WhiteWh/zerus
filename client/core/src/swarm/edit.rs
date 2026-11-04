//! The organisation edits of the Sessions list, as changes of `swarm get`'s
//! `organization` object: the desired side of `hgs swarm apply {base,
//! desired, versions}`. Each follows SessionOrganization's rule for the
//! shared fields (tray/src/SessionOrganization.cpp). `swarm apply` diffs base
//! and desired field by field and resolves concurrent heads
//! (src/swarm/model.rs:126-201), so an edit only changes the lists the user
//! asked for. The order of sessions inside a project is not shared
//! (SwarmController.cpp:148-150): it lives in [`super::layout`].
use super::{listed, members, project_id, projects, sessions, INBOX, MAX_PROJECTS};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The longest project name (SessionOrganization.cpp:29, 135, 150).
pub const NAME_CHARS: usize = 80;
/// A new project's colour (SessionOrganization.cpp:142).
pub const NEW_COLOR: &str = "#64b5f6";
pub const GONE: &str = "This project is no longer listed. Open the menu again.";
pub const NAME_NEEDED: &str = "Enter a project name.";
pub const TOO_MANY: &str = "A catalog holds at most 500 projects.";
pub const NOTHING: &str = "Choose the sessions to move.";

/// One edit. Members are `"<host>\n<session name>"` keys ([`super::member`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    /// Move to project, a drop and a selection's move: the members, in order,
    /// into `project` as explicit members (moveSession, 187-201). `before`
    /// places them in the local order only: `before` is not used for the
    /// shared side, because the order inside a project is device-local and
    /// the layout (`super::layout`) applies it, so a stale anchor still
    /// moves the members.
    Move {
        members: Vec<String>,
        project: String,
        before: Option<String>,
    },
    /// New project before the Inbox (createGroup, 133-145), then the members
    /// into it (SessionsWindow::createGroup, SessionsWindow.cpp:2558-2566).
    Create {
        id: String,
        name: String,
        members: Vec<String>,
    },
    /// A confirmed clone's new project and folder in one shared edit. Failure
    /// leaves the original catalog intact; the checkout itself is kept.
    CreateFolder {
        id: String,
        name: String,
        folder: super::folder_edit::Change,
    },
    /// A creation's source or cloned folder (SessionOrganization.cpp:258-269).
    AddFolder {
        id: String,
        project: String,
        machine: String,
        path: String,
        name: String,
        style: crate::paths::Style,
    },
    /// Project folder links, through the same shared writer. No file effects.
    Folder {
        project: String,
        change: super::folder_edit::Change,
    },
    /// Shared color only; vivid fill remains a device-local preference.
    Color { project: String, color: String },
    /// renameGroup (147-152).
    Rename { project: String, name: String },
    /// removeGroup (154-166): its members and the listed rows it showed go
    /// to `destination`.
    Delete {
        project: String,
        destination: String,
        members: Vec<String>,
    },
    /// moveGroup (174-185): before `before`, or last.
    Reorder {
        project: String,
        before: Option<String>,
    },
    /// Sort sessions by name: every listed member, already in order, as an
    /// explicit member (SessionsWindow.cpp:2601-2611).
    Sort {
        project: String,
        members: Vec<String>,
    },
    /// The same run under a new name (renameSession, 203-221). When another
    /// writer migrated it already, `from` is no member and nothing changes.
    Renamed { from: String, to: String },
}

/// The name SessionOrganization stores: trimmed, then at most 80 characters.
pub fn clean_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(NAME_CHARS).collect();
    (!name.is_empty()).then_some(name)
}

fn projects_mut(organization: &mut Value) -> Option<&mut Vec<Value>> {
    organization
        .get_mut("projects")
        .and_then(Value::as_array_mut)
}

fn position(organization: &Value, id: &str) -> Option<usize> {
    projects(organization)
        .iter()
        .position(|project| project_id(project) == id)
}

/// Takes members out of every project (moveSession's removal, 193-195).
fn take(organization: &mut Value, gone: &BTreeSet<&str>) {
    for project in projects_mut(organization).into_iter().flatten() {
        if let Some(list) = project.get_mut("sessions").and_then(Value::as_array_mut) {
            list.retain(|member| !member.as_str().is_some_and(|member| gone.contains(member)));
        }
    }
}

/// The members, in order, into the project at `at` as explicit members.
fn move_into(organization: &mut Value, members: &[String], project: &str) -> Result<(), String> {
    let at = position(organization, project).ok_or(GONE)?;
    let mut seen = BTreeSet::new();
    let unique: Vec<&str> = members
        .iter()
        .map(String::as_str)
        .filter(|member| !member.is_empty() && seen.insert(*member))
        .collect();
    take(organization, &seen);
    let target = &mut projects_mut(organization).ok_or(GONE)?[at];
    if !target.get("sessions").is_some_and(Value::is_array) {
        target["sessions"] = json!([]);
    }
    let list = target["sessions"].as_array_mut().ok_or(GONE)?;
    list.extend(
        unique
            .into_iter()
            .map(|member| Value::String(member.to_owned())),
    );
    Ok(())
}

/// The organization after `intent`, or why it cannot be made.
pub fn apply(organization: &Value, intent: &Intent) -> Result<Value, String> {
    let mut next = organization.clone();
    match intent {
        Intent::CreateFolder { id, name, folder } => {
            if listed(organization, id) {
                return Err("This project identity is already listed.".into());
            }
            next = apply(
                organization,
                &Intent::Create {
                    id: id.clone(),
                    name: name.clone(),
                    members: Vec::new(),
                },
            )?;
            super::folder_edit::apply(&mut next, id, folder)?;
        }
        Intent::Move {
            members, project, ..
        } => {
            if members.is_empty() {
                return Err(NOTHING.into());
            }
            move_into(&mut next, members, project)?;
        }
        Intent::Create { id, name, members } => {
            let name = clean_name(name).ok_or(NAME_NEEDED)?;
            if !listed(&next, id) {
                if projects(&next).len() >= MAX_PROJECTS {
                    return Err(TOO_MANY.into());
                }
                let at = position(&next, INBOX).unwrap_or(projects(&next).len());
                let project = json!({"id": id, "name": name, "color": NEW_COLOR, "accessible": true,
                                     "folders": [], "sessions": []});
                match projects_mut(&mut next) {
                    Some(list) => list.insert(at, project),
                    None => next["projects"] = json!([project]),
                }
            }
            if !members.is_empty() {
                move_into(&mut next, members, id)?;
            }
        }
        Intent::AddFolder {
            id,
            project,
            machine,
            path,
            name,
            style,
        } => {
            if machine.trim().is_empty()
                || !crate::paths::absolute(path, *style)
                || path.contains('\n')
            {
                return Ok(next);
            }
            let Some(at) = position(&next, project) else {
                return Ok(next);
            };
            let clean = crate::paths::clean(path, *style);
            let project = &mut projects_mut(&mut next).ok_or(GONE)?[at];
            if !project["folders"].is_array() {
                project["folders"] = json!([]);
            }
            let folders = project["folders"].as_array_mut().ok_or(GONE)?;
            if !folders.iter().any(|folder| {
                folder["machine"].as_str() == Some(machine)
                    && crate::paths::same(
                        folder["path"].as_str().unwrap_or_default(),
                        &clean,
                        *style,
                    )
            }) {
                folders.push(json!({"id": id, "machine": machine,
                    "path": clean, "name": name.trim().chars().take(NAME_CHARS).collect::<String>()}));
            }
        }
        Intent::Folder { project, change } => {
            super::folder_edit::apply(&mut next, project, change)?;
        }
        Intent::Color { project, color } => {
            if color.len() != 7
                || !color.starts_with('#')
                || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            {
                return Err("Choose a color in #RRGGBB format.".into());
            }
            let at = position(&next, project).ok_or(GONE)?;
            projects_mut(&mut next).ok_or(GONE)?[at]["color"] = json!(color.to_ascii_lowercase());
        }
        Intent::Rename { project, name } => {
            let name = clean_name(name).ok_or(NAME_NEEDED)?;
            let at = position(&next, project).ok_or(GONE)?;
            projects_mut(&mut next).ok_or(GONE)?[at]["name"] = json!(name);
        }
        Intent::Delete {
            project,
            destination,
            members,
        } => {
            if project == destination || !listed(&next, destination) {
                return Err(GONE.into());
            }
            let at = position(&next, project).ok_or(GONE)?;
            let removed = projects_mut(&mut next).ok_or(GONE)?.remove(at);
            let mut displaced: Vec<String> = sessions(&removed).map(str::to_owned).collect();
            for member in members {
                if !displaced.contains(member) {
                    displaced.push(member.clone());
                }
            }
            if !displaced.is_empty() {
                move_into(&mut next, &displaced, destination)?;
            }
        }
        Intent::Reorder { project, before } => {
            let anchor = before.as_deref();
            if anchor == Some(project.as_str()) || anchor.is_some_and(|id| !listed(&next, id)) {
                return Ok(next);
            }
            let from = position(&next, project).ok_or(GONE)?;
            let list = projects_mut(&mut next).ok_or(GONE)?;
            let moving = list.remove(from);
            let to = anchor
                .and_then(|id| list.iter().position(|p| project_id(p) == id))
                .unwrap_or(list.len());
            list.insert(to, moving);
        }
        Intent::Sort { project, members } => move_into(&mut next, members, project)?,
        Intent::Renamed { from, to } => {
            if from == to || from.is_empty() || to.is_empty() || !members(&next).contains_key(from)
            {
                return Ok(next);
            }
            take(&mut next, &BTreeSet::from([to.as_str()]));
            for project in projects_mut(&mut next).into_iter().flatten() {
                if let Some(list) = project.get_mut("sessions").and_then(Value::as_array_mut) {
                    for member in list
                        .iter_mut()
                        .filter(|member| member.as_str() == Some(from.as_str()))
                    {
                        *member = json!(to);
                    }
                }
            }
        }
    }
    Ok(next)
}

/// What `swarm apply` carries for an organization (sharedOrganization,
/// SwarmController.cpp:22-51): membership is explicit and the shared project
/// order is the listed one. This client never lists fallback rows as
/// members, and the catalog never holds this computer's Windows folders, so
/// nothing else is filtered.
pub fn shared(organization: &Value) -> Value {
    let mut out = organization.clone();
    if let Some(object) = out.as_object_mut() {
        // Fallback placement stays local: filter those sessions out of every
        // project, then drop the key (SwarmController.cpp:27-34).
        let unassigned: BTreeSet<String> = object
            .remove("unassigned_sessions")
            .and_then(|value| value.as_array().cloned())
            .into_iter()
            .flatten()
            .filter_map(|session| session.as_str().map(str::to_owned))
            .collect();
        if let Some(list) = object.get_mut("projects").and_then(Value::as_array_mut) {
            for project in list {
                let kept: Vec<Value> = sessions(project)
                    .filter(|member| !unassigned.contains(*member))
                    .map(|member| json!(member))
                    .collect();
                project["sessions"] = Value::Array(kept);
            }
        }
        object.insert("session_assignments_explicit".into(), json!(true));
        object.insert("project_order".into(), project_order(organization));
    }
    out
}

/// The shared project order of an organization: its projects' IDs in the
/// listed order (sharedOrganization, SwarmController.cpp:45-49).
pub(crate) fn project_order(organization: &Value) -> Value {
    Value::Array(
        projects(organization)
            .iter()
            .map(|p| json!(project_id(p)))
            .collect(),
    )
}

type Shape = (Vec<(String, Value, Value, Value)>, BTreeMap<String, String>);

fn shape(organization: &Value) -> Shape {
    let fields = projects(organization)
        .iter()
        .map(|p| {
            (
                project_id(p).to_owned(),
                p["name"].clone(),
                p["color"].clone(),
                p["folders"].clone(),
            )
        })
        .collect();
    (fields, members(organization))
}

/// Whether two organizations differ in what `swarm apply` writes: the
/// projects, their names, colours and folders, their order and the
/// membership. The order of sessions inside a project is local.
pub fn changes_shared(base: &Value, desired: &Value) -> bool {
    shape(base) != shape(desired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{project_id, projects, sessions};
    use serde_json::json;

    const R: &str = "workstation\nclaude/sample-project/review";
    const O: &str = "studio\ncodex/aurora/orders-v3";
    /// A fallback row: no project lists it.
    const N: &str = "workstation\nkimi/docs-site/landing";

    fn org() -> Value {
        json!({"version": 2, "default_project": "ungrouped", "projects": [
            {"id": "alpha", "name": "Alpha", "color": "#3fb950", "accessible": true,
             "folders": [{"id": "f1", "machine": "workstation", "machine_id": "m-1", "path": "/home/example/src/sample-project", "name": ""}],
             "sessions": [R]},
            {"id": "beta", "name": "Beta", "color": "#64b5f6", "accessible": true, "folders": [], "sessions": [O]},
            {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa", "accessible": true, "folders": [], "sessions": []}]})
    }

    fn lists(org: &Value) -> Vec<(String, Vec<String>)> {
        projects(org)
            .iter()
            .map(|p| {
                (
                    project_id(p).to_owned(),
                    sessions(p).map(str::to_owned).collect(),
                )
            })
            .collect()
    }

    fn ids(org: &Value) -> Vec<String> {
        projects(org)
            .iter()
            .map(|p| project_id(p).to_owned())
            .collect()
    }

    fn moving(members: &[&str], project: &str) -> Intent {
        Intent::Move {
            members: members.iter().map(|m| (*m).to_owned()).collect(),
            project: project.into(),
            before: None,
        }
    }

    #[test]
    fn project_color_changes_only_that_shared_field_and_rejects_unsafe_values() {
        let original = org();
        let changed = apply(
            &original,
            &Intent::Color {
                project: "alpha".into(),
                color: "#ABCDEF".into(),
            },
        )
        .unwrap();
        let mut expected = original.clone();
        expected["projects"][0]["color"] = json!("#abcdef");
        assert_eq!(changed, expected);
        assert!(
            changed["projects"][0].get("vivid").is_none(),
            "vivid never enters the shared intent"
        );
        assert!(apply(
            &original,
            &Intent::Color {
                project: "gone".into(),
                color: "#abcdef".into()
            }
        )
        .is_err());
        for color in ["red", "#abc", "#ffffff; background:red", "#ZZZZZZ", ""] {
            assert!(apply(
                &original,
                &Intent::Color {
                    project: "alpha".into(),
                    color: color.into()
                }
            )
            .is_err());
        }
        assert_eq!(
            apply(
                &changed,
                &Intent::Color {
                    project: "alpha".into(),
                    color: "#abcdef".into()
                }
            )
            .unwrap(),
            changed,
            "same color has no shared difference"
        );
    }

    #[test]
    fn a_move_takes_every_member_out_of_its_project_and_appends_them_in_order() {
        let next = apply(&org(), &moving(&[O, N, R, O], "alpha")).unwrap();
        assert_eq!(
            lists(&next),
            [
                (
                    "alpha".to_owned(),
                    vec![O.to_owned(), N.to_owned(), R.to_owned()]
                ),
                ("beta".to_owned(), vec![]),
                ("ungrouped".to_owned(), vec![]),
            ]
        );
        assert!(changes_shared(&org(), &next));
        let same = apply(&org(), &moving(&[R], "alpha")).unwrap();
        assert!(
            !changes_shared(&org(), &same),
            "a move inside its own project changes only the local order"
        );
        assert_eq!(apply(&org(), &moving(&[R], "gone")), Err(GONE.to_owned()));
        assert_eq!(
            apply(&org(), &moving(&[], "alpha")),
            Err(NOTHING.to_owned())
        );
    }

    #[test]
    fn a_new_project_goes_before_the_inbox_and_takes_its_members() {
        let id = "0f0e0d0c-0b0a-4908-8706-050403020100";
        let next = apply(
            &org(),
            &Intent::Create {
                id: id.into(),
                name: "  Release work  ".into(),
                members: vec![N.into()],
            },
        )
        .unwrap();
        assert_eq!(ids(&next), ["alpha", "beta", id, "ungrouped"]);
        let created = &projects(&next)[2];
        assert_eq!(created["name"], "Release work");
        assert_eq!(created["color"], NEW_COLOR);
        assert_eq!(created["sessions"], json!([N]));
        let long = apply(
            &org(),
            &Intent::Create {
                id: "id-2".into(),
                name: "x".repeat(90),
                members: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            projects(&long)[2]["name"].as_str().unwrap().chars().count(),
            NAME_CHARS
        );
        assert_eq!(
            apply(
                &org(),
                &Intent::Create {
                    id: "id-3".into(),
                    name: "   ".into(),
                    members: vec![]
                }
            ),
            Err(NAME_NEEDED.to_owned())
        );
        let no_inbox = json!({"projects": [{"id": "alpha", "sessions": []}]});
        let last = apply(
            &no_inbox,
            &Intent::Create {
                id: "id-4".into(),
                name: "Last".into(),
                members: vec![],
            },
        )
        .unwrap();
        assert_eq!(ids(&last), ["alpha", "id-4"]);
        assert!(changes_shared(&org(), &next));
        assert_eq!(
            shared(&next)["project_order"],
            json!(["alpha", "beta", id, "ungrouped"])
        );
    }

    #[test]
    fn rename_trims_and_keeps_eighty_characters() {
        let next = apply(
            &org(),
            &Intent::Rename {
                project: "beta".into(),
                name: " Orders ".into(),
            },
        )
        .unwrap();
        assert_eq!(projects(&next)[1]["name"], "Orders");
        let long = apply(
            &org(),
            &Intent::Rename {
                project: "beta".into(),
                name: "y".repeat(120),
            },
        )
        .unwrap();
        assert_eq!(projects(&long)[1]["name"], "y".repeat(NAME_CHARS).as_str());
        assert_eq!(
            apply(
                &org(),
                &Intent::Rename {
                    project: "beta".into(),
                    name: " ".into()
                }
            ),
            Err(NAME_NEEDED.to_owned())
        );
        assert_eq!(
            apply(
                &org(),
                &Intent::Rename {
                    project: "gone".into(),
                    name: "x".into()
                }
            ),
            Err(GONE.to_owned())
        );
    }

    #[test]
    fn delete_moves_its_members_and_the_listed_rows_into_the_destination() {
        let next = apply(
            &org(),
            &Intent::Delete {
                project: "beta".into(),
                destination: "alpha".into(),
                members: vec![O.into(), N.into()],
            },
        )
        .unwrap();
        assert_eq!(
            lists(&next),
            [
                (
                    "alpha".to_owned(),
                    vec![R.to_owned(), O.to_owned(), N.to_owned()]
                ),
                ("ungrouped".to_owned(), vec![]),
            ]
        );
        assert_eq!(
            projects(&next)[0]["folders"],
            org()["projects"][0]["folders"],
            "the receiver keeps its folders"
        );
        for (project, destination) in [("beta", "beta"), ("beta", "gone"), ("gone", "alpha")] {
            let intent = Intent::Delete {
                project: project.into(),
                destination: destination.into(),
                members: vec![],
            };
            assert_eq!(
                apply(&org(), &intent),
                Err(GONE.to_owned()),
                "{project} into {destination}"
            );
        }
    }

    #[test]
    fn reorder_follows_move_group() {
        let up = apply(
            &org(),
            &Intent::Reorder {
                project: "beta".into(),
                before: Some("alpha".into()),
            },
        )
        .unwrap();
        assert_eq!(ids(&up), ["beta", "alpha", "ungrouped"]);
        let last = apply(
            &org(),
            &Intent::Reorder {
                project: "alpha".into(),
                before: None,
            },
        )
        .unwrap();
        assert_eq!(ids(&last), ["beta", "ungrouped", "alpha"]);
        let unknown = apply(
            &org(),
            &Intent::Reorder {
                project: "alpha".into(),
                before: Some("gone".into()),
            },
        )
        .unwrap();
        assert_eq!(
            ids(&unknown),
            ["alpha", "beta", "ungrouped"],
            "moveGroup ignores an unknown anchor"
        );
        assert!(changes_shared(&org(), &up));
        assert_eq!(
            shared(&up)["project_order"],
            json!(["beta", "alpha", "ungrouped"])
        );
    }

    #[test]
    fn sort_makes_every_listed_member_explicit() {
        let inbox = apply(
            &org(),
            &Intent::Sort {
                project: "ungrouped".into(),
                members: vec![N.into()],
            },
        )
        .unwrap();
        assert_eq!(
            lists(&inbox)[2],
            ("ungrouped".to_owned(), vec![N.to_owned()])
        );
        assert!(changes_shared(&org(), &inbox));
    }

    #[test]
    fn a_rename_keeps_an_explicit_members_project() {
        const R2: &str = "workstation\nclaude/sample-project/review-2";
        let next = apply(
            &org(),
            &Intent::Renamed {
                from: R.into(),
                to: R2.into(),
            },
        )
        .unwrap();
        assert_eq!(lists(&next)[0], ("alpha".to_owned(), vec![R2.to_owned()]));
        let implicit = apply(
            &org(),
            &Intent::Renamed {
                from: N.into(),
                to: R2.into(),
            },
        )
        .unwrap();
        assert!(
            !changes_shared(&org(), &implicit),
            "a fallback row's rename changes nothing shared"
        );
        // Qt migrated it first: the old name is no member, so the second migration changes nothing.
        let again = apply(
            &next,
            &Intent::Renamed {
                from: R.into(),
                to: R2.into(),
            },
        )
        .unwrap();
        assert!(
            !changes_shared(&next, &again),
            "a migration another writer made is a no-op"
        );
        // The old explicit name wins over the new name's own project (SessionOrganization.cpp:208-215).
        let both = apply(
            &org(),
            &Intent::Renamed {
                from: R.into(),
                to: O.into(),
            },
        )
        .unwrap();
        assert_eq!(
            lists(&both),
            [
                ("alpha".to_owned(), vec![O.to_owned()]),
                ("beta".to_owned(), vec![]),
                ("ungrouped".to_owned(), vec![]),
            ]
        );
    }

    #[test]
    fn the_request_marks_membership_explicit_and_carries_the_shared_order() {
        let request = shared(&org());
        assert_eq!(request["session_assignments_explicit"], true);
        assert_eq!(
            request["project_order"],
            json!(["alpha", "beta", "ungrouped"])
        );
        assert_eq!(request["projects"], org()["projects"]);
        let mut cached = org();
        cached["unassigned_sessions"] = json!([N]);
        assert!(shared(&cached).get("unassigned_sessions").is_none());
    }

    #[test]
    fn unassigned_sessions_never_leave_as_members() {
        let mut cached = org();
        cached["unassigned_sessions"] = json!([R]);
        let request = shared(&cached);
        assert!(request.get("unassigned_sessions").is_none());
        assert_eq!(lists(&request)[0], ("alpha".to_owned(), vec![]));
        assert_eq!(lists(&request)[1], ("beta".to_owned(), vec![O.to_owned()]));
    }

    #[test]
    fn a_catalog_holds_at_most_max_projects() {
        let many: Vec<Value> = (0..MAX_PROJECTS)
            .map(|i| json!({"id": format!("p{i}"), "sessions": []}))
            .collect();
        let full = json!({"projects": many});
        let intent = Intent::Create {
            id: "extra".into(),
            name: "Extra".into(),
            members: vec![],
        };
        assert_eq!(apply(&full, &intent), Err(TOO_MANY.to_owned()));
        let one_less = json!({"projects": full["projects"].as_array().unwrap()[1..].to_vec()});
        assert!(apply(&one_less, &intent).is_ok());
    }
}
