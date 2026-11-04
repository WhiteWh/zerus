//! Bounded review of an already observed `swarm get`, following
//! SwarmConflictReview.cpp. No reads, writes, controller or currentness proof.
//! A resolution retains the original JSON value and every reviewed causal
//! parent; the CLI preserves unseen concurrent heads rather than overwriting them.
mod text;

use crate::Invocation;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io, ops::Range};

pub const PAGE: usize = 64;
pub const VALUES_PAGE: usize = 32;
pub const CONFLICTS: usize = 50_000;
pub const VARIANTS: usize = 1024;
pub const SNAPSHOT_BYTES: usize = 16 << 20;
pub const PAGE_BYTES: usize = 1 << 20;
pub const INVALID: &str = "Invalid project catalog conflict";
pub const TOO_LARGE: &str = "The project conflicts are too large to review.";
pub const CHANGED: &str = "This conflict changed. Review the updated choices again.";
pub const CHOOSE: &str =
    "Select a row to see what will change. Closing this window leaves the conflict for later.";

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Field {
    Project { id: String, field: String },
    Folder { id: String },
    Session { machine: String, session: String },
    Machine { id: String, field: String },
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub offset: usize,
    pub total: usize,
    pub rows: Vec<T>,
}
#[derive(Debug, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Summary {
    /// Digest of the original field token; never a replacement for that token.
    pub id: String,
    pub setting: String,
    pub subject: String,
    pub project: Option<Project>,
    pub computer: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Origin {
    pub text: String,
    pub computer: Option<String>,
    pub local: bool,
}
#[derive(Debug, Serialize)]
pub struct Choice {
    /// Index in this immutable review, not an operation ID or user-supplied JSON.
    pub index: usize,
    pub text: String,
    pub project: Option<Project>,
    pub origins: Vec<Origin>,
    pub shown: bool,
    pub removal: bool,
}
#[derive(Debug, Serialize)]
pub struct Selection {
    pub outcome: String,
    pub action: String,
    pub confirmation: bool,
}

/// Borrows the native snapshot; does not retain another full catalog cache.
pub struct Catalog<'a> {
    snapshot: &'a Value,
    rows: &'a [Value],
    indexes: BTreeMap<String, usize>,
    projects: BTreeMap<&'a str, &'a Value>,
    machines: BTreeMap<&'a str, &'a Value>,
    folders: BTreeMap<&'a str, &'a str>,
    local_host: &'a str,
    session_title: Option<&'a SessionTitle>,
}
/// Optional lookup in the already bounded native session projection. No reads.
pub type SessionTitle = dyn Fn(&str, &str) -> Option<String>;
/// One immutable review. Original keys, versions and values stay native.
/// No Serialize/Clone and no permission or automatic execution path.
pub struct Review {
    identity: [String; 2],
    raw: Value,
    field: Field,
    groups: Vec<Vec<usize>>,
}
impl std::fmt::Debug for Review {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Review").finish_non_exhaustive()
    }
}

fn label(value: &Value, max: usize) -> Result<&str, String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or_else(|| INVALID.into())
}
fn uuid(s: &str) -> bool {
    crate::creation::uuid(s)
}
impl Field {
    fn removal(&self, value: &Value) -> bool {
        matches!(self, Self::Project { field, .. } if field == "alive" && value == false)
            || matches!(self, Self::Folder { .. }) && value.is_null()
            || matches!(self, Self::Machine { field, .. } if field == "redirect")
    }
    fn checked(raw: &Value) -> Result<Self, String> {
        let field: Self = serde_json::from_value(raw.clone()).map_err(|_| INVALID)?;
        let valid = match &field {
            Self::Project { id, field } => {
                label(&json!(id), 128).is_ok()
                    && matches!(field.as_str(), "name" | "color" | "alive")
            }
            Self::Folder { id } => label(&json!(id), 128).is_ok(),
            Self::Machine { id, field } => {
                uuid(id) && matches!(field.as_str(), "name" | "redirect")
            }
            Self::Session { machine, session } => {
                uuid(machine)
                    && !session.is_empty()
                    && session.len() <= 2048
                    && !session.contains('\0')
            }
        };
        valid.then_some(field).ok_or_else(|| INVALID.into())
    }
    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Session { .. } => value.is_null() || label(value, 128).is_ok(),
            Self::Folder { .. } => {
                value.is_null()
                    || value.as_object().is_some_and(|o| {
                        o.len() == 4
                            && label(&value["project"], 128).is_ok()
                            && value["machine"].as_str().is_some_and(uuid)
                            && value["path"].as_str().is_some_and(|p| {
                                p.starts_with('/')
                                    && p.len() <= 8192
                                    && !p.chars().any(char::is_control)
                            })
                            && value["name"]
                                .as_str()
                                .is_some_and(|s| s.len() <= 320 && !s.chars().any(char::is_control))
                    })
            }
            Self::Project { field, .. } => match field.as_str() {
                "alive" => value.is_boolean(),
                "color" => value.as_str().is_some_and(|s| {
                    s.len() == 7
                        && s.starts_with('#')
                        && s[1..].bytes().all(|c| c.is_ascii_hexdigit())
                }),
                _ => label(value, 320).is_ok(),
            },
            Self::Machine { id, field } if field == "redirect" => {
                value.as_str().is_some_and(|s| uuid(s) && s != id)
            }
            Self::Machine { .. } => label(value, 320).is_ok(),
        }
    }
}

// Count encoded bytes without building an unbounded serialized copy.
struct Budget(usize);
impl io::Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other(TOO_LARGE))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn bounded(value: &impl Serialize, max: usize) -> Result<(), String> {
    serde_json::to_writer(&mut Budget(max), value).map_err(|_| TOO_LARGE.into())
}
fn range(page: usize, total: usize, size: usize) -> Range<usize> {
    let first = page.min(total.saturating_sub(1) / size) * size;
    first..(first + size).min(total)
}
fn token_id(raw: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(raw["key"].as_str().unwrap_or_default().as_bytes())
    )
}
fn variants(raw: &Value) -> &[Value] {
    raw["variants"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn checked_conflict(raw: &Value) -> Result<Field, String> {
    let key = label(&raw["key"], 4096)?;
    let parsed: Value = serde_json::from_str(key).map_err(|_| INVALID)?;
    if parsed != raw["field"] {
        return Err(INVALID.into());
    }
    let field = Field::checked(&parsed)?;
    let ops = raw["variants"].as_array().ok_or(INVALID)?;
    if ops.len() > VARIANTS {
        return Err(TOO_LARGE.into());
    }
    if ops.len() < 2 {
        return Err(INVALID.into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut different = false;
    for op in ops {
        if !ids.insert(label(&op["id"], 512)?)
            || label(&op["actor"], 512).is_err()
            || op.get("value").is_none()
            || !field.accepts(&op["value"])
        {
            return Err(INVALID.into());
        }
        different |= op["value"] != ops[0]["value"];
    }
    if !different
        || !raw["selected"].is_null() && !raw["selected"].as_str().is_some_and(|s| ids.contains(s))
    {
        return Err(INVALID.into());
    }
    Ok(field)
}

impl<'a> Catalog<'a> {
    /// Whole-view qualification before publishing any page. Malformed or excess
    /// rows refuse the view, never a seemingly complete prefix.
    pub fn checked(snapshot: &'a Value) -> Result<Self, String> {
        bounded(snapshot, SNAPSHOT_BYTES)?;
        if snapshot["schema"] != 1 || !snapshot["initialized"].is_boolean() {
            return Err(INVALID.into());
        }
        label(&snapshot["node_id"], 512)?;
        label(&snapshot["swarm_id"], 512)?;
        let rows = snapshot["conflicts"].as_array().ok_or(INVALID)?;
        let projects = snapshot["organization"]["projects"]
            .as_array()
            .ok_or(INVALID)?;
        let machines = snapshot["machines"].as_array().ok_or(INVALID)?;
        if rows.len() > CONFLICTS
            || projects.len() > super::MAX_PROJECTS
            || machines.len() > super::dialog::COMPUTERS
        {
            return Err(TOO_LARGE.into());
        }
        let mut result = Self {
            snapshot,
            rows,
            indexes: BTreeMap::new(),
            projects: BTreeMap::new(),
            machines: BTreeMap::new(),
            folders: BTreeMap::new(),
            local_host: "",
            session_title: None,
        };
        for project in projects {
            if result
                .projects
                .insert(label(&project["id"], 128)?, project)
                .is_some()
            {
                return Err(INVALID.into());
            }
            text::optional(project, "name", 320)?;
            text::optional(project, "color", 32)?;
            let folders = project["folders"].as_array().ok_or(INVALID)?;
            if folders.len() > super::FOLDERS {
                return Err(TOO_LARGE.into());
            }
            for folder in folders {
                let id = label(&folder["id"], 128)?;
                let path = label(&folder["path"], 8192)?;
                if result.folders.insert(id, path).is_some() {
                    return Err(INVALID.into());
                }
            }
        }
        for machine in machines {
            if result
                .machines
                .insert(label(&machine["id"], 512)?, machine)
                .is_some()
            {
                return Err(INVALID.into());
            }
            text::optional(machine, "name", 512)?;
            text::optional(machine, "connection", 512)?;
        }
        for (index, raw) in rows.iter().enumerate() {
            checked_conflict(raw)?;
            if result.indexes.insert(token_id(raw), index).is_some() {
                return Err(INVALID.into());
            }
        }
        Ok(result)
    }
    /// Use existing Fleet/Pump labels only; no history or metadata discovery.
    /// A missing title keeps Qt's raw-name/archive-ID fallback.
    pub fn with_session_titles(
        mut self,
        local_host: &'a str,
        title: &'a SessionTitle,
    ) -> Result<Self, String> {
        if local_host.len() > 512 || local_host.chars().any(char::is_control) {
            return Err(INVALID.into());
        }
        self.local_host = local_host;
        self.session_title = Some(title);
        Ok(self)
    }
    pub fn page(&self, page: usize) -> Result<Page<Summary>, String> {
        let range = range(page, self.rows.len(), PAGE);
        let result = Page {
            offset: range.start,
            total: self.rows.len(),
            rows: range
                .map(|i| self.summary(&self.rows[i]))
                .collect::<Result<_, _>>()?,
        };
        bounded(&result, PAGE_BYTES)?;
        Ok(result)
    }
    pub fn summary_for(&self, id: &str) -> Result<Summary, String> {
        self.summary(&self.rows[*self.indexes.get(id).ok_or(CHANGED)?])
    }
    pub fn review(&self, id: &str) -> Result<Review, String> {
        let raw = self
            .rows
            .get(*self.indexes.get(id).ok_or(CHANGED)?)
            .ok_or(CHANGED)?;
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut lookup = BTreeMap::new();
        for (index, op) in variants(raw).iter().enumerate() {
            let token = op["value"].to_string();
            let next = groups.len();
            let group = *lookup.entry(token).or_insert(next);
            if group == next {
                groups.push(Vec::new());
            }
            groups[group].push(index);
        }
        Ok(Review {
            identity: [
                self.snapshot["node_id"].as_str().unwrap().into(),
                self.snapshot["swarm_id"].as_str().unwrap().into(),
            ],
            raw: raw.clone(),
            field: checked_conflict(raw)?,
            groups,
        })
    }
}
impl Review {
    /// Unrelated catalog edits need not discard a review. These exact original
    /// variants and the catalog's node/swarm identity must still be present.
    pub fn matches(&self, current: &Catalog<'_>) -> bool {
        self.identity[0] == current.snapshot["node_id"]
            && self.identity[1] == current.snapshot["swarm_id"]
            && current.indexes.get(&token_id(&self.raw)).is_some_and(|i| {
                let raw = &current.rows[*i];
                raw["key"] == self.raw["key"]
                    && raw["field"] == self.raw["field"]
                    && raw["variants"] == self.raw["variants"]
            })
    }
    pub fn explanation(&self, changed: bool) -> String {
        let text = match self.field {
            Field::Session { .. } => "Connected computers have different project assignments for this session. Choose the project it should belong to.",
            Field::Folder { .. } => "Different folder locations or a removal were saved before the changes were synchronized. Choose the folder link to keep.",
            _ => "Different changes to this setting were saved before they were synchronized. Choose the value to keep.",
        };
        format!(
            "{}{text}\n“Shown now” is a temporary display choice, not a resolved conflict.",
            if changed {
                format!("{CHANGED}\n")
            } else {
                String::new()
            }
        )
    }
    pub fn choices(&self, current: &Catalog<'_>, page: usize) -> Result<Page<Choice>, String> {
        if !self.matches(current) {
            return Err(CHANGED.into());
        }
        let range = range(page, self.groups.len(), VALUES_PAGE);
        let rows = range
            .clone()
            .map(|i| self.choice(current, i))
            .collect::<Result<_, _>>()?;
        let result = Page {
            offset: range.start,
            total: self.groups.len(),
            rows,
        };
        bounded(&result, PAGE_BYTES)?;
        Ok(result)
    }
    fn value(&self, index: usize) -> Result<&Value, String> {
        self.groups
            .get(index)
            .map(|group| &variants(&self.raw)[group[0]]["value"])
            .ok_or_else(|| INVALID.into())
    }
    pub fn selection(&self, current: &Catalog<'_>, index: usize) -> Result<Selection, String> {
        if !self.matches(current) {
            return Err(CHANGED.into());
        }
        let value = self.value(index)?;
        Ok(Selection {
            outcome: self.consequence(current, value),
            action: match self.field {
                Field::Session { .. } => "Keep this project",
                Field::Folder { .. } => "Keep this folder link",
                Field::Project { ref field, .. } if field == "alive" => "Apply project choice",
                _ => "Keep this value",
            }
            .into(),
            confirmation: self.field.removal(value),
        })
    }
    /// Build the existing CLI payload only; native must reserve its single writer,
    /// freshly read/recheck, collect actual resources and settle the source.
    /// Unknown delivery must never be retried from this captured review.
    pub fn resolve(
        &self,
        current: &Catalog<'_>,
        index: usize,
        confirmed: bool,
    ) -> Result<Invocation, String> {
        let selection = self.selection(current, index)?;
        if current.snapshot["initialized"] != true {
            return Err(super::sync::NOT_SET_UP.into());
        }
        if selection.confirmation && !confirmed {
            return Err("Confirm catalog change".into());
        }
        let payload = json!({"key":self.raw["key"],"versions":variants(&self.raw).iter().map(|op| &op["id"]).collect::<Vec<_>>(),"value":self.value(index)?});
        bounded(&payload, SNAPSHOT_BYTES)?;
        Ok(Invocation {
            args: vec!["swarm".into(), "resolve".into()],
            stdin: Some(serde_json::to_vec(&payload).map_err(|_| INVALID)?),
            timeout: super::connection::NETWORK_TIMEOUT,
            request_id: None,
        })
    }
}
