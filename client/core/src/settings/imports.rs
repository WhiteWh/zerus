//! Versioned first-G3 imports of previously unhandled UI families. The old
//! settings seed is never repeated. Values and their handled record commit
//! together in the existing settings writer, with Next's choices taking priority.
use super::{Contents, QtSnapshot, QtStore, QtValue, MAX_STORE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const VERSION: u32 = 1;
const MAX_KEYS: usize = 4096;
const FAMILIES: &[&str] = &[
    "machine-appearance",
    "project-view",
    "row-density",
    "task-preferences",
    "termination-default",
    "project-vivid",
    "window-geometry",
    "session-splitter",
    "project-folder-splitter",
];

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    families: BTreeMap<String, FamilyRecord>,
    quarantine: BTreeMap<String, Value>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyRecord {
    version: u32,
    family: String,
    schema: u32,
    source: String,
    source_path: String,
    at: u64,
    keys: Vec<String>,
    sha256: Option<String>,
    source_mtime_after_switch: Option<bool>,
}

/// The same bounded metadata for a family committed by another existing
/// client store. No source body or writer authority is included.
pub(crate) fn family_metadata(
    family: &str,
    store: &QtStore,
    snapshot: Option<&QtSnapshot>,
    keys: Vec<String>,
    seed_at: Option<u64>,
) -> Result<Value, &'static str> {
    let record = FamilyRecord {
        version: VERSION,
        family: family.into(),
        schema: 1,
        source: snapshot.map_or("none", |s| s.source).into(),
        source_path: store.path().to_string_lossy().into(),
        at: super::seconds(std::time::SystemTime::now()).unwrap_or(0),
        keys,
        sha256: snapshot.map(|s| s.sha256.clone()),
        source_mtime_after_switch: snapshot
            .and_then(|s| s.modified_at)
            .zip(seed_at)
            .map(|(mtime, seed)| mtime > seed),
    };
    if !valid_record(family, &record) {
        return Err("unsupported G3 family metadata");
    }
    Ok(serde_json::to_value(record).unwrap())
}

pub(crate) fn valid_family_metadata(family: &str, value: &Value) -> bool {
    serde_json::from_value::<FamilyRecord>(value.clone())
        .is_ok_and(|record| valid_record(family, &record))
}

const SHARED_ORGANIZATION: &str = "shared-organization";
const ORGANIZATION_PENDING: &str = "g3_shared_organization_pending";

/// Metadata only. It is not a request, a pending edit or writer authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrganizationImport {
    Unhandled,
    Pending {
        node_id: String,
        sha256: Option<String>,
    },
    Handled,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingOrganization {
    version: u32,
    node_id: String,
    record: FamilyRecord,
}

fn pending_organization(contents: &Contents) -> Result<Option<PendingOrganization>, &'static str> {
    let Some(value) = contents.extra.get(ORGANIZATION_PENDING) else {
        return Ok(None);
    };
    let pending: PendingOrganization =
        serde_json::from_value(value.clone()).map_err(|_| "unsupported pending project import")?;
    if pending.version != VERSION
        || !crate::creation::canonical_uuid(&pending.node_id)
        || !valid_record(SHARED_ORGANIZATION, &pending.record)
        || (pending.record.source != "none" && pending.record.keys != ["workspace/organization"])
    {
        return Err("unsupported pending project import");
    }
    Ok(Some(pending))
}

pub(super) fn organization_state(contents: &Contents) -> Result<OrganizationImport, &'static str> {
    let journal = journal(contents)?;
    let pending = pending_organization(contents)?;
    if journal.families.contains_key(SHARED_ORGANIZATION) {
        if pending.is_some() {
            return Err("conflicting project import records");
        }
        return Ok(OrganizationImport::Handled);
    }
    Ok(match pending {
        Some(pending) => OrganizationImport::Pending {
            node_id: pending.node_id,
            sha256: pending.record.sha256,
        },
        None => OrganizationImport::Unhandled,
    })
}

pub(super) fn prepare_organization(
    contents: &mut Contents,
    node_id: &str,
    store: &QtStore,
    snapshot: Option<&QtSnapshot>,
) -> Result<bool, &'static str> {
    match organization_state(contents)? {
        OrganizationImport::Handled => return Ok(false),
        OrganizationImport::Pending { node_id: old, .. } if old != node_id => {
            return Err("project import belongs to another catalog node")
        }
        _ => {}
    }
    if !crate::creation::canonical_uuid(node_id) {
        return Err("invalid project catalog node");
    }
    if journal(contents)?.families.len() >= 256 {
        return Err("project import record limit reached");
    }
    let seed_at = contents.seed.as_ref().and_then(|s| s["at"].as_u64());
    let pending = PendingOrganization {
        version: VERSION,
        node_id: node_id.into(),
        record: serde_json::from_value(family_metadata(
            SHARED_ORGANIZATION,
            store,
            snapshot,
            snapshot
                .filter(|s| s.values.contains_key("workspace/organization"))
                .map_or_else(Vec::new, |_| vec!["workspace/organization".into()]),
            seed_at,
        )?)
        .unwrap(),
    };
    contents.extra.insert(
        ORGANIZATION_PENDING.into(),
        serde_json::to_value(pending).unwrap(),
    );
    pending_organization(contents)?;
    Ok(true)
}

pub(super) fn complete_organization(
    contents: &mut Contents,
    node_id: &str,
) -> Result<(), &'static str> {
    if organization_state(contents)? == OrganizationImport::Handled {
        return Ok(());
    }
    let pending = pending_organization(contents)?.ok_or("missing project import record")?;
    if pending.node_id != node_id {
        return Err("project import belongs to another catalog node");
    }
    let mut journal = journal(contents)?;
    if journal.families.len() >= 256 {
        return Err("project import record limit reached");
    }
    journal
        .families
        .insert(SHARED_ORGANIZATION.into(), pending.record);
    contents
        .extra
        .insert("g3".into(), serde_json::to_value(journal).unwrap());
    contents.extra.remove(ORGANIZATION_PENDING);
    Ok(())
}

fn journal(contents: &Contents) -> Result<Journal, &'static str> {
    let Some(saved) = contents.extra.get("g3") else {
        return Ok(Journal {
            version: VERSION,
            ..Journal::default()
        });
    };
    let journal: Journal =
        serde_json::from_value(saved.clone()).map_err(|_| "invalid G3 family record")?;
    if journal.version != VERSION || journal.families.len() > 256 || journal.quarantine.len() > 256
    {
        return Err("unsupported G3 family record");
    }
    // An unknown family is retained, never interpreted or imported again.
    for (family, record) in &journal.families {
        if !valid_record(family, record) {
            return Err("invalid handled G3 family");
        }
    }
    Ok(journal)
}

fn valid_record(family: &str, record: &FamilyRecord) -> bool {
    let digest = match &record.sha256 {
        Some(hash) => {
            record.source != "none"
                && hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        None => record.source == "none" && record.keys.is_empty(),
    };
    record.family == family
        && record.version == VERSION
        && record.schema == 1
        && digest
        && record.keys.len() <= MAX_KEYS
        && !record.source_path.is_empty()
        && ["none", "qt-ini", "qt-plist", "qt-defaults"].contains(&record.source.as_str())
}

pub(super) fn pending(contents: &Contents) -> Result<bool, &'static str> {
    let journal = journal(contents)?;
    Ok(FAMILIES
        .iter()
        .any(|family| !journal.families.contains_key(*family)))
}

fn matches(family: &str, key: &str) -> bool {
    match family {
        "machine-appearance" => {
            key.starts_with("machines/colors/") || key.starts_with("machines/vivid/")
        }
        "project-view" => key == "workspace/allSwarmProjects",
        "project-vivid" => key == crate::swarm::qt_import::KEY,
        "row-density" => key == crate::row_preferences::KEY,
        "termination-default" => key == "workspace/removeWorktreeAfterTermination",
        "task-preferences" => key == "tasks/onlySession",
        "window-geometry" => key == "sessions/geometry",
        "session-splitter" => key == "workspace/splitter",
        "project-folder-splitter" => key == "workspace/projectFolderSplit",
        _ => false,
    }
}

fn target(key: &str) -> &str {
    match key {
        "sessions/geometry" => super::geometry::KEY,
        "workspace/splitter" => "workspace/sessionsWidth",
        _ => key,
    }
}

fn converted(key: &str, value: &QtValue) -> Result<Value, &'static str> {
    if key == crate::row_preferences::KEY {
        return crate::row_preferences::from_qt(value).ok_or("invalid packed row-density keys");
    }
    if key == "sessions/geometry" {
        return super::qt_layout::geometry(value)
            .map(|geometry| Value::String(serde_json::to_string(&geometry).unwrap()))
            .ok_or("invalid Qt window geometry");
    }
    if matches!(key, "workspace/splitter" | "workspace/projectFolderSplit") {
        let orientation = if key == "workspace/splitter" { 1 } else { 2 };
        let size = super::qt_layout::splitter(value, orientation).ok_or("invalid Qt splitter")?;
        return Ok(super::effective(
            super::find(target(key)).unwrap().kind,
            Some(&json!(size)),
        ));
    }
    let kind = super::find(key).ok_or("invalid preference identity")?.kind;
    if key.starts_with("machines/colors/") {
        let text = value.to_text();
        if !matches!(value, QtValue::Text(_) | QtValue::Bytes(_))
            || crate::trayicon::parse_color(&text).is_none()
        {
            return Err("invalid machine color");
        }
    } else if super::checked_qt_flag(value).is_none() {
        return Err("invalid preference flag");
    }
    Ok(super::from_qt(kind, value))
}

/// Expand only the local fill preference from the existing organization
/// decoder. Evidence continues to name/hash the original Qt store and key,
/// never synthetic flattened Qt keys or a rewritten organization checkpoint.
fn project_vivid(
    value: &QtValue,
    contents: &Contents,
    handled_keys: &BTreeSet<String>,
) -> Result<Map<String, Value>, &'static str> {
    let bytes = match value {
        QtValue::Bytes(bytes) => bytes.as_slice(),
        QtValue::Text(text) => text.as_bytes(),
        _ => return Err("invalid project fill source"),
    };
    let organization = crate::swarm::qt_import::decode(bytes)?;
    let projects = organization["projects"]
        .as_array()
        .ok_or("invalid project fill source")?;
    let mut ids = BTreeSet::new();
    let mut values = Map::new();
    for project in projects {
        let id = project["id"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && *s != "pinned"
                    && !s.chars().any(char::is_control)
            })
            .ok_or("invalid project fill identity")?;
        if !ids.insert(id)
            || !project["name"].as_str().is_some_and(|s| {
                !s.trim().is_empty() && s.len() <= 320 && !s.chars().any(char::is_control)
            })
        {
            return Err("ambiguous project fill identity");
        }
        let key = format!("projects/vivid/{id}");
        // Presence is decisive: explicit Next false is never an empty default.
        if contents.values.contains_key(&key) || handled_keys.contains(&key) {
            continue;
        }
        if let Some(vivid) = project.get("vivid") {
            values.insert(
                key,
                Value::Bool(vivid.as_bool().ok_or("invalid project fill flag")?),
            );
        }
    }
    Ok(values)
}

pub(super) fn apply(
    contents: &mut Contents,
    store: &QtStore,
    snapshot: Option<&QtSnapshot>,
) -> bool {
    let Ok(mut journal) = journal(contents) else {
        return false;
    };
    let at = super::seconds(std::time::SystemTime::now()).unwrap_or(0);
    let old_seed_at = contents.seed.as_ref().and_then(|s| s["at"].as_u64());
    let changed = snapshot
        .and_then(|s| s.modified_at)
        .zip(old_seed_at)
        .map(|(mtime, seed)| mtime > seed);
    let handled_keys: BTreeSet<String> = contents
        .seed
        .as_ref()
        .and_then(|s| s["keys"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut dirty = false;
    for family in FAMILIES {
        if journal.families.contains_key(*family) {
            continue;
        }
        let entries: Vec<_> = snapshot
            .into_iter()
            .flat_map(|s| s.values.iter())
            .filter(|(key, _)| matches(family, key))
            .take(MAX_KEYS + 1)
            .collect();
        let mut candidate = contents.clone();
        let converted: Result<Map<String, Value>, _> = if entries.len() > MAX_KEYS {
            Err("family exceeds its key limit")
        } else if *family == "project-vivid" {
            match entries.first() {
                Some((key, value)) if !handled_keys.contains(*key) => {
                    project_vivid(value, contents, &handled_keys)
                }
                _ => Ok(Map::new()),
            }
        } else {
            entries
                .iter()
                // Existing Next values and keys handled by the old seed are
                // not candidates. Malformed unused Qt values cannot turn a
                // deliberate Next choice into an import/retry requirement.
                .filter(|(key, _)| {
                    !contents.values.contains_key(target(key))
                        && !handled_keys.contains(*key)
                        && !handled_keys.contains(target(key))
                })
                .map(|(key, value)| converted(key, value).map(|v| (target(key).to_owned(), v)))
                .collect()
        };
        let mut record = json!({
            "version": VERSION, "family": family, "schema": 1,
            "source": snapshot.map_or("none", |s| s.source),
            "source_path": store.path().to_string_lossy(), "at": at,
            "keys": entries.iter().take(MAX_KEYS).map(|(k, _)| *k).collect::<Vec<_>>(),
            "sha256": snapshot.map(|s| &s.sha256), "source_mtime_after_switch": changed,
        });
        let values = match converted {
            Ok(values) => values,
            Err(reason) => {
                // Private metadata points to the unchanged source. Source
                // values (which might contain secrets) never enter a report.
                record["reason"] = json!(reason);
                record["preserved_in"] = json!("source");
                journal.quarantine.insert((*family).into(), record);
                candidate
                    .extra
                    .insert("g3".into(), serde_json::to_value(&journal).unwrap());
                if candidate.to_bytes().len() as u64 <= MAX_STORE {
                    *contents = candidate;
                    dirty = true;
                }
                continue;
            }
        };
        for (key, value) in values {
            candidate.values.entry(key).or_insert(value);
        }
        journal
            .families
            .insert((*family).into(), serde_json::from_value(record).unwrap());
        candidate
            .extra
            .insert("g3".into(), serde_json::to_value(&journal).unwrap());
        if candidate.to_bytes().len() as u64 <= MAX_STORE {
            *contents = candidate;
            dirty = true;
        } else {
            journal.families.remove(*family);
        }
    }
    dirty
}
