//! The `organization` topic: the shared project catalog, which groups the
//! Sessions list and drives Move to project.
//!
//! A follow is per computer, usually only this one: the catalog is shared.
//! A stream that offers the topic sends keyed rows (see the "Event stream"
//! section of `docs/reference.md`): the summary `organization`,
//! `project:<id>`, `members:<id>:<n>` and `version:<token>`. Snapshot pages
//! are staged until the last one, and a delta changes rows in place. The core
//! rebuilds `swarm get`'s object from the rows. The view gets
//! [`OrganizationUpdate::State`] with that object without `versions`, its
//! `digest` and the session membership indexed once per change, and only
//! when the object changed. Both are shared (`Arc`), so a catalog of tens of
//! thousands of sessions costs one index per change and no copies. The core keeps `versions`
//! with the digest they belong to; [`OrganizationSnapshot`] hands base and
//! versions to an edit (`swarm apply {base, desired, versions}`) consistently.
//! A CLI without the topic is polled with `hgs swarm get` instead, with the
//! same updates.
use crate::{Change, ComputerId};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use zerus_watch_protocol::{Delta, ErrorFrame, Snapshot};

/// How often a CLI without the topic is polled, as the Qt client does.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// One change of a followed organization.
#[derive(Clone, Debug, PartialEq)]
pub enum OrganizationUpdate {
    /// `swarm get`'s object without `versions` and `written`, its digest,
    /// and the project ID of every member, keyed as `organization.projects[]
    /// .sessions[]` names them (`<host>\n<session name>`).
    State {
        value: Arc<Value>,
        digest: String,
        membership: Arc<BTreeMap<String, String>>,
    },
    /// The catalog could not be read; the last state stays as known.
    Failed { code: String, message: String },
    /// The follow ended.
    Ended,
}

/// What an edit of the organization starts from: the organization the user
/// saw and the versions observed with it, for one digest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrganizationSnapshot {
    /// The same source's identities, rather than identities inferred from an
    /// alias or another cached listing. The CLI exposes node_id as machine_id.
    pub node_id: String,
    pub swarm_id: String,
    pub digest: String,
    /// `swarm get`'s `organization`.
    pub base: Value,
    /// `swarm get`'s `versions`.
    pub versions: Value,
}

/// One computer's followed organization, kept across its streams.
#[derive(Debug, Default)]
pub struct OrganizationState {
    followed: bool,
    rows: BTreeMap<String, Value>,
    staged: Option<BTreeMap<String, Value>>,
    /// The latest rebuilt object, with `versions`.
    full: Option<Value>,
}

/// Rebuilds `swarm get`'s object (without `written`) from the topic's rows;
/// None without a summary row or with a missing project or member row.
pub fn rebuild(rows: &BTreeMap<String, Value>) -> Option<Value> {
    let mut snapshot = rows.get("organization")?.clone();
    let ids = snapshot["organization"]["projects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut projects = Vec::with_capacity(ids.len());
    for id in ids.iter().filter_map(Value::as_str) {
        let mut project = rows.get(&format!("project:{id}"))?.clone();
        let object = project.as_object_mut()?;
        let chunks = object
            .remove("session_chunks")
            .and_then(|count| count.as_u64())
            .unwrap_or(0);
        let mut sessions = Vec::new();
        for index in 0..chunks {
            let chunk = rows.get(&format!("members:{id}:{index}"))?;
            sessions.extend(chunk["sessions"].as_array().cloned().unwrap_or_default());
        }
        object.insert("sessions".into(), Value::Array(sessions));
        projects.push(project);
    }
    if snapshot["organization"].is_object() {
        snapshot["organization"]["projects"] = Value::Array(projects);
    }
    let versions: Map<String, Value> = rows
        .iter()
        .filter_map(|(key, row)| Some((key.strip_prefix("version:")?.to_owned(), row.clone())))
        .collect();
    snapshot["versions"] = Value::Object(versions);
    Some(snapshot)
}

/// The project ID of every member of `swarm get`'s object.
pub fn membership(value: &Value) -> BTreeMap<String, String> {
    crate::swarm::members(&value["organization"])
}

fn change(computer: &ComputerId, update: OrganizationUpdate) -> Change {
    Change::Organization {
        computer: computer.clone(),
        update,
    }
}

impl OrganizationState {
    /// Accepts the topic's frames from now on; a followed organization keeps
    /// what it holds.
    pub fn follow(&mut self) {
        self.followed = true;
    }

    /// Stops following and drops the rows and the snapshot.
    pub fn unfollow(&mut self) {
        *self = Self::default();
    }

    pub fn is_followed(&self) -> bool {
        self.followed
    }

    /// The base and versions of the latest state, for an edit.
    pub fn snapshot(&self) -> Option<OrganizationSnapshot> {
        let full = self.full.as_ref()?;
        Some(OrganizationSnapshot {
            node_id: full["node_id"].as_str().unwrap_or_default().to_owned(),
            swarm_id: full["swarm_id"].as_str().unwrap_or_default().to_owned(),
            digest: full["digest"].as_str().unwrap_or_default().to_owned(),
            base: full["organization"].clone(),
            versions: full["versions"].clone(),
        })
    }

    /// Whether the latest state reports an outdated catalog worker.
    pub fn worker_outdated(&self) -> bool {
        self.full.as_ref().is_some_and(|full| {
            full["worker"]["running"] == true && full["worker"]["current"] == false
        })
    }

    /// A new stream attempt: pages in flight are dropped.
    pub(crate) fn begin(&mut self) {
        self.staged = None;
    }

    /// Takes a rebuilt object (from rows or a poll) and reports it when it
    /// changed.
    fn take(&mut self, computer: &ComputerId, mut full: Value) -> Option<Change> {
        if let Some(object) = full.as_object_mut() {
            object.remove("written");
        }
        if self.full.as_ref() == Some(&full) {
            return None;
        }
        let digest = full["digest"].as_str().unwrap_or_default().to_owned();
        let mut value = full.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("versions");
        }
        let membership = membership(&value);
        self.full = Some(full);
        Some(change(
            computer,
            OrganizationUpdate::State {
                value: Arc::new(value),
                digest,
                membership: Arc::new(membership),
            },
        ))
    }

    /// A `snapshot` page of the topic.
    pub(crate) fn snapshot_page(
        &mut self,
        computer: &ComputerId,
        page: &Snapshot,
    ) -> Option<Change> {
        if !self.followed {
            return None;
        }
        if page.page == 0 {
            self.staged = Some(BTreeMap::new());
        }
        let staged = self.staged.get_or_insert_with(BTreeMap::new);
        for item in page.items.iter().flatten() {
            if let (Some(key), Some(row)) =
                (item.get("key").and_then(Value::as_str), item.get("row"))
            {
                staged.insert(key.to_owned(), row.clone());
            }
        }
        if !page.last {
            return None;
        }
        self.rows = self.staged.take().unwrap_or_default();
        let full = rebuild(&self.rows)?;
        self.take(computer, full)
    }

    /// A `delta` of the topic.
    pub(crate) fn delta(&mut self, computer: &ComputerId, delta: &Delta) -> Option<Change> {
        if !self.followed || self.rows.is_empty() {
            return None;
        }
        for removal in &delta.remove {
            self.rows.remove(&removal.key);
        }
        for item in &delta.upsert {
            self.rows.insert(item.key.clone(), item.row.clone());
        }
        let full = rebuild(&self.rows)?;
        self.take(computer, full)
    }

    /// A `swarm get` reply of the poll fallback.
    pub(crate) fn polled(&mut self, computer: &ComputerId, reply: Value) -> Option<Change> {
        if !self.followed || !reply.is_object() {
            return None;
        }
        self.rows.clear();
        self.take(computer, reply)
    }

    /// A non-fatal `error` of the topic, or a failed poll.
    pub(crate) fn failed(
        &mut self,
        computer: &ComputerId,
        code: &str,
        message: &str,
    ) -> Option<Change> {
        self.followed.then(|| {
            change(
                computer,
                OrganizationUpdate::Failed {
                    code: code.to_owned(),
                    message: message.to_owned(),
                },
            )
        })
    }

    pub(crate) fn error(&mut self, computer: &ComputerId, error: &ErrorFrame) -> Option<Change> {
        if error.code == "unsupported_topic" {
            return None;
        }
        self.failed(computer, &error.code, &error.message)
    }

    /// The follow ends without an unfollow (its stream stopped for good).
    pub(crate) fn end(&mut self, computer: &ComputerId) -> Option<Change> {
        let followed = self.followed;
        self.unfollow();
        followed.then(|| change(computer, OrganizationUpdate::Ended))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerus_watch_protocol::{CatalogItem, Removal};

    #[test]
    fn a_session_listed_by_two_projects_maps_to_the_first() {
        // Qt's indexSessions keeps the first (tray/src/SessionOrganization.cpp:46-48).
        let value = json!({"organization": {"projects": [
            {"id": "alpha", "sessions": ["h\ns"]}, {"id": "beta", "sessions": ["h\ns"]}]}});
        assert_eq!(membership(&value)["h\ns"], "alpha");
    }

    fn full(sessions: &[&str], digest: &str) -> Value {
        json!({
            "schema": 1, "node_id": "node-1", "swarm_id": "swarm-1", "digest": digest, "initialized": true,
            "conflicts": [], "preferences": {}, "machines": [], "peers": {},
            "worker": {"running": true, "current": true},
            "organization": {"version": 2, "default_project": "ungrouped", "project_order": ["alpha"],
                "projects": [{"id": "alpha", "name": "Alpha", "color": "#336699", "accessible": true,
                              "folders": [], "sessions": sessions}]},
            "versions": {"project:alpha:name": ["op-1"]}
        })
    }

    /// The rows the server sends for `full` (see src/watch/organization.rs).
    fn rows(full: &Value) -> Vec<(String, Value)> {
        let mut summary = full.clone();
        summary.as_object_mut().unwrap().remove("versions");
        summary["organization"]["projects"] = json!(["alpha"]);
        let sessions = full["organization"]["projects"][0]["sessions"].clone();
        let mut project = full["organization"]["projects"][0].clone();
        project.as_object_mut().unwrap().remove("sessions");
        project["session_chunks"] = json!(1);
        vec![
            ("organization".into(), summary),
            ("project:alpha".into(), project),
            ("members:alpha:0".into(), json!({"sessions": sessions})),
            ("version:project:alpha:name".into(), json!(["op-1"])),
        ]
    }

    fn page(rows: Vec<(String, Value)>, page: u32, last: bool) -> Snapshot {
        Snapshot {
            topic: "organization".into(),
            key: None,
            items: Some(
                rows.into_iter()
                    .map(|(key, row)| json!({"key": key, "row": row}))
                    .collect(),
            ),
            value: None,
            page,
            last,
            cursor: 1,
        }
    }

    fn state(change: Option<Change>) -> (Value, String) {
        match change {
            Some(Change::Organization {
                update:
                    OrganizationUpdate::State {
                        value,
                        digest,
                        membership,
                    },
                ..
            }) => {
                assert_eq!(*membership, super::membership(&value));
                (Value::clone(&value), digest)
            }
            other => panic!("not a state: {other:?}"),
        }
    }

    #[test]
    fn pages_and_deltas_rebuild_swarm_get_and_the_view_value_leaves_out_versions() {
        let local = ComputerId::local();
        let mut organization = OrganizationState::default();
        assert_eq!(
            organization.snapshot_page(&local, &page(rows(&full(&["a"], "d1")), 0, true)),
            None,
            "not followed"
        );
        organization.follow();
        let all = rows(&full(&["a"], "d1"));
        assert_eq!(
            organization.snapshot_page(&local, &page(all[..2].to_vec(), 0, false)),
            None
        );
        let (value, digest) =
            state(organization.snapshot_page(&local, &page(all[2..].to_vec(), 1, true)));
        assert_eq!(digest, "d1");
        assert!(value.get("versions").is_none());
        assert_eq!(
            value["organization"]["projects"][0]["sessions"],
            json!(["a"])
        );
        let snapshot = organization.snapshot().unwrap();
        assert_eq!(snapshot.node_id, "node-1");
        assert_eq!(snapshot.swarm_id, "swarm-1");
        assert_eq!(snapshot.digest, "d1");
        assert_eq!(snapshot.versions, json!({"project:alpha:name": ["op-1"]}));
        assert_eq!(snapshot.base["projects"][0]["id"], "alpha");
        // The same rows again (a reconnect) report nothing; a delta does.
        assert_eq!(
            organization.snapshot_page(&local, &page(rows(&full(&["a"], "d1")), 0, true)),
            None
        );
        let changed = rows(&full(&["a", "b"], "d2"));
        let delta = Delta {
            topic: "organization".into(),
            upsert: [&changed[0], &changed[2]]
                .into_iter()
                .map(|(key, row)| CatalogItem {
                    key: key.clone(),
                    row: row.clone(),
                })
                .collect(),
            remove: vec![],
            cursor: 2,
        };
        let update = organization.delta(&local, &delta);
        let Some(Change::Organization {
            update: OrganizationUpdate::State { membership, .. },
            ..
        }) = &update
        else {
            panic!("a state")
        };
        assert_eq!(membership.get("b").map(String::as_str), Some("alpha"));
        let (value, digest) = state(update);
        assert_eq!(digest, "d2");
        assert_eq!(
            value["organization"]["projects"][0]["sessions"],
            json!(["a", "b"])
        );
        assert_eq!(organization.snapshot().unwrap().digest, "d2");
        // A delta that leaves a project without its row cannot be rebuilt and
        // keeps the last state.
        let broken = Delta {
            topic: "organization".into(),
            upsert: vec![],
            remove: vec![Removal {
                key: "project:alpha".into(),
                reason: None,
            }],
            cursor: 3,
        };
        assert_eq!(organization.delta(&local, &broken), None);
        assert_eq!(organization.snapshot().unwrap().digest, "d2");
    }

    #[test]
    fn polls_errors_worker_state_and_endings() {
        let local = ComputerId::local();
        let mut organization = OrganizationState::default();
        organization.follow();
        let mut reply = full(&["a"], "d1");
        reply["written"] = json!({});
        let (value, _) = state(organization.polled(&local, reply.clone()));
        assert!(value.get("written").is_none() && value.get("versions").is_none());
        assert_eq!(
            organization.polled(&local, reply.clone()),
            None,
            "an unchanged poll"
        );
        assert!(!organization.worker_outdated());
        reply["worker"] = json!({"running": true, "current": false});
        state(organization.polled(&local, reply));
        assert!(organization.worker_outdated());
        let error = ErrorFrame {
            code: "organization_failed".into(),
            message: "damaged".into(),
            topic: Some("organization".into()),
            key: None,
            fatal: false,
        };
        assert!(matches!(
            organization.error(&local, &error),
            Some(Change::Organization {
                update: OrganizationUpdate::Failed { .. },
                ..
            })
        ));
        assert!(
            organization.snapshot().is_some(),
            "a failure keeps the last state"
        );
        assert!(matches!(
            organization.end(&local),
            Some(Change::Organization {
                update: OrganizationUpdate::Ended,
                ..
            })
        ));
        assert!(!organization.is_followed() && organization.snapshot().is_none());
        assert_eq!(organization.end(&local), None);
    }
}
