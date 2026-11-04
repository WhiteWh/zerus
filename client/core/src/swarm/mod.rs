//! The shared project organization in the Sessions list (`hgs swarm`), on
//! top of the core's `organization` topic (`crate::organization`):
//!
//! - [`member`], [`members`] and [`default_project`]: Qt's membership rules
//!   (tray/src/SessionOrganization.cpp);
//! - [`folders`]: the one parser of a project's folders, with Qt's rules
//!   (SessionOrganization.cpp:36-44), for Projects and New session;
//! - [`edit`]: the menus' edits, as the desired side of `swarm apply`;
//! - [`layout`]: this device's collapsed projects, session order and default;
//! - [`grouping`]: each row's project and rank and the headers, sent to the
//!   webview only when they change;
//! - [`sync`]: the write controller, a port of tray/src/SwarmController.cpp;
//! - [`draft`]: its pending edits on disk, so a restart sends them again.
//!
//! Pure Rust: the Tauri hub (client/src-tauri/src/organization.rs) runs
//! `hgs swarm apply` and passes the clock in. Placement itself (concurrent
//! Inbox and default markers against a concrete project, causal heads) is
//! resolved by `hgs swarm apply` and the replica (src/swarm/model.rs:126-201,
//! src/swarm/catalog.rs:369-394), never here.
pub mod conflicts;
pub mod connection;
pub mod dialog;
pub mod draft;
pub mod edit;
pub mod folder_edit;
pub mod grouping;
pub mod layout;
pub mod qt_import;
pub mod runs;
pub mod sync;

use crate::paths::windows_shaped;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The Inbox: the project of sessions no project lists, unless this device
/// or the catalog names another (SessionOrganization.cpp:52-58).
pub const INBOX: &str = "ungrouped";
/// The Inbox's name and colour when the catalog lists no project at all
/// (SessionOrganization.cpp:53).
pub const INBOX_NAME: &str = "General";
pub const INBOX_COLOR: &str = "#8d9baa";
/// The most projects the list shows (phase 3a's bound).
pub const MAX_PROJECTS: usize = 500;
/// The most folders of one project that Projects and New session read.
pub const FOLDERS: usize = 200;
/// Shown while project sync has not read the catalog yet.
pub const STARTING: &str = "Project sync is starting.";

/// A session as `organization.projects[].sessions[]` names it: the machine
/// (this computer's host name or the peer's alias, as `ViewRow::machine`)
/// and the session name (SessionOrganization.cpp:225).
pub fn member(machine: &str, name: &str) -> String {
    format!("{machine}\n{name}")
}

/// The archived session's organization identity (SessionOrganization.cpp:225).
pub fn archive_member(machine: &str, archive_id: &str) -> String {
    format!("{machine}\narchive\n{archive_id}")
}

/// The listed projects, in the catalog's order.
pub fn projects(organization: &Value) -> &[Value] {
    organization
        .get("projects")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn project_id(project: &Value) -> &str {
    project.get("id").and_then(Value::as_str).unwrap_or("")
}

/// A project folder as SessionOrganization keeps it
/// (SessionOrganization.cpp:36-44).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Folder {
    /// The catalog's `id`, empty when it has none. Never made up here.
    pub id: String,
    /// The folder's identity on this side only: `id` when it has one, else
    /// `machine` + `\n` + `path`. It stands where Qt makes up a random id
    /// (SessionOrganization.cpp:42), and it is never sent to `hgs` as a
    /// folder id: edits build their desired side from the catalog's own
    /// folders ([`edit::apply`]).
    pub key: String,
    pub machine: String,
    pub path: String,
    pub name: String,
    pub machine_id: String,
    pub machine_name: String,
}

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// A project's folders, as Qt reads them (SessionOrganization.cpp:36-44):
/// a folder without a computer, with a path neither absolute ("/") nor
/// Windows-shaped (this computer's drive or UNC folders), or with the
/// computer and path of an earlier one is left out. At most [`FOLDERS`].
pub fn folders(project: &Value) -> Vec<Folder> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for folder in project
        .get("folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if out.len() == FOLDERS {
            break;
        }
        let Some(folder) = folder_value(folder) else {
            continue;
        };
        if seen.insert(format!("{}\n{}", folder.machine, folder.path)) {
            out.push(folder);
        }
    }
    out
}

/// The same decoder for one original folder record; edits preserve the rest
/// of that record instead of reconstructing it from this presentation.
pub fn folder_value(value: &Value) -> Option<Folder> {
    let machine = field(value, "machine");
    let path = field(value, "path");
    if machine.is_empty() || (!path.starts_with('/') && !windows_shaped(&path)) {
        return None;
    }
    let id = field(value, "id");
    Some(Folder {
        key: if id.is_empty() {
            format!("{machine}\n{path}")
        } else {
            id.clone()
        },
        id,
        machine,
        path,
        name: field(value, "name"),
        machine_id: field(value, "machine_id"),
        machine_name: field(value, "machine_name"),
    })
}

/// A project's explicit members.
pub fn sessions(project: &Value) -> impl Iterator<Item = &str> {
    project
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

pub fn listed(organization: &Value, id: &str) -> bool {
    projects(organization)
        .iter()
        .any(|project| project_id(project) == id)
}

/// The project of sessions no project lists (SessionOrganization.cpp:55-58):
/// this device's choice when listed, then the catalog's `default_project`
/// when listed, then the first project, then the Inbox.
pub fn default_project(organization: &Value, local: Option<&str>) -> String {
    if let Some(id) = local.filter(|id| listed(organization, id)) {
        return id.to_owned();
    }
    let catalog = organization
        .get("default_project")
        .and_then(Value::as_str)
        .unwrap_or(INBOX);
    if listed(organization, catalog) {
        return catalog.to_owned();
    }
    projects(organization)
        .iter()
        .map(project_id)
        .find(|id| !id.is_empty())
        .unwrap_or(INBOX)
        .to_owned()
}

/// Every explicit member's project. A member listed by two projects stays in
/// the first (indexSessions, SessionOrganization.cpp:98-104); `swarm apply`
/// refuses such a catalog (src/swarm/model.rs:107-110), so a valid snapshot
/// lists each member once.
pub fn members(organization: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for project in projects(organization) {
        for member in sessions(project) {
            out.entry(member.to_owned())
                .or_insert_with(|| project_id(project).to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn organization() -> Value {
        json!({"default_project": "ungrouped", "projects": [
            {"id": "alpha", "name": "Alpha", "sessions": ["workstation\nclaude/sample-project/review", "studio\ncodex/aurora/orders-v3"]},
            {"id": "beta", "name": "Beta", "sessions": ["workstation\nclaude/sample-project/review"]},
            {"id": "ungrouped", "name": "Inbox", "sessions": []}]})
    }

    #[test]
    fn a_member_is_the_machine_and_the_session_name() {
        assert_eq!(
            member("workstation", "claude/sample-project/review"),
            "workstation\nclaude/sample-project/review"
        );
    }

    #[test]
    fn the_first_project_that_lists_a_member_keeps_it() {
        let members = members(&organization());
        assert_eq!(
            members["workstation\nclaude/sample-project/review"],
            "alpha"
        );
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn the_default_project_follows_qt() {
        let org = organization();
        assert_eq!(default_project(&org, None), "ungrouped");
        assert_eq!(default_project(&org, Some("beta")), "beta");
        assert_eq!(default_project(&org, Some("gone")), "ungrouped");
        let without =
            json!({"default_project": "ungrouped", "projects": [{"id": "alpha"}, {"id": "beta"}]});
        assert_eq!(default_project(&without, None), "alpha");
        assert_eq!(default_project(&json!({"projects": []}), None), INBOX);
        assert_eq!(default_project(&json!(null), None), INBOX);
    }

    fn folder(id: Option<&str>, machine: &str, path: &str) -> Value {
        let mut folder = json!({"machine": machine, "path": path, "name": "", "machine_id": "m-1", "machine_name": "Workstation"});
        if let Some(id) = id {
            folder["id"] = json!(id);
        }
        folder
    }

    fn keys(project: &Value) -> Vec<String> {
        folders(project).into_iter().map(|f| f.key).collect()
    }

    // SessionOrganization.cpp:36-44: no computer, a path neither absolute nor
    // Windows-shaped, and a repeated computer and path are left out.
    #[test]
    fn a_folder_without_a_computer_or_a_folder_path_is_left_out() {
        let project = json!({"folders": [
            folder(Some("no-machine"), "", "/srv/a"),
            folder(Some("relative"), "workstation", "srv/a"),
            folder(Some("empty-path"), "workstation", ""),
            folder(Some("backslash"), "workstation", "\\work"),
            folder(Some("drive-relative"), "workstation", "C:work"),
            folder(Some("posix"), "workstation", "/srv/a"),
            folder(Some("drive"), "win-lab", "C:\\Users\\example"),
            folder(Some("share"), "win-lab", "\\\\server\\share\\docs"),
            json!("not a folder"),
            json!({"machine": 7, "path": "/srv/b", "id": "not-text"})]});
        assert_eq!(keys(&project), ["posix", "drive", "share"]);
        assert!(folders(&json!({"id": "alpha"})).is_empty());
        assert!(folders(&json!({"folders": {"machine": "workstation"}})).is_empty());
    }

    #[test]
    fn a_repeated_computer_and_path_keeps_the_first() {
        let project = json!({"folders": [
            folder(Some("first"), "workstation", "/srv/a"),
            folder(Some("again"), "workstation", "/srv/a"),
            folder(None, "workstation", "/srv/a"),
            folder(Some("other-computer"), "studio", "/srv/a"),
            folder(Some("other-path"), "workstation", "/srv/a/")]});
        assert_eq!(keys(&project), ["first", "other-computer", "other-path"]);
    }

    #[test]
    fn a_folder_without_an_id_keeps_it_empty_and_is_keyed_by_its_place() {
        let got = folders(&json!({"folders": [folder(None, "workstation", "/srv/a")]}));
        assert_eq!(
            got,
            [Folder {
                id: String::new(),
                key: "workstation\n/srv/a".into(),
                machine: "workstation".into(),
                path: "/srv/a".into(),
                name: String::new(),
                machine_id: "m-1".into(),
                machine_name: "Workstation".into(),
            }]
        );
        let empty = folders(&json!({"folders": [folder(Some(""), "workstation", "/srv/b")]}));
        assert_eq!(
            (empty[0].id.as_str(), empty[0].key.as_str()),
            ("", "workstation\n/srv/b")
        );
        let named = folders(&json!({"folders": [folder(Some("f1"), "workstation", "/srv/c")]}));
        assert_eq!((named[0].id.as_str(), named[0].key.as_str()), ("f1", "f1"));
    }

    #[test]
    fn at_most_folders_are_read() {
        let list: Vec<Value> = (0..FOLDERS + 20)
            .map(|n| folder(Some(&format!("f{n}")), "workstation", &format!("/srv/{n}")))
            .collect();
        let mut with_bad = vec![folder(Some("bad"), "", "/srv/x")];
        with_bad.extend(list);
        let got = folders(&json!({ "folders": with_bad }));
        assert_eq!(got.len(), FOLDERS);
        assert_eq!(got[0].key, "f0", "a left-out folder takes no place");
        assert_eq!(got[FOLDERS - 1].key, format!("f{}", FOLDERS - 1));
    }
}
