//! Bounded display data for the Worktrees overview. Source decoding and review
//! meanings remain in swarm/worktree_review; no row or selection is permission
//! to remove a folder. The native caller must resolve and review it afresh.
use crate::{
    swarm::{self, Folder},
    worktree_review as review,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

pub const PAGE: usize = 256;
pub const SOURCES: usize = 512;
pub const ROWS: usize = 8192;
pub const SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const ROW_BYTES: usize = 16 * 1024 * 1024;
const ITEM_BYTES: usize = 64 * 1024;

/// Exact folder references retained on the native side, never deserialized as
/// executable source authority from the webview.
#[derive(Clone, Debug, Serialize)]
pub struct Reference {
    pub project: String,
    pub name: String,
    pub folder: Folder,
}
#[derive(Clone, Debug, Serialize)]
pub struct Source {
    pub computer: String,
    pub machine: String,
    pub path: String,
    pub references: Vec<Reference>,
}
#[derive(Default, Debug)]
pub struct Plan {
    pub sources: Vec<Source>,
    pub partial: bool,
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("overview byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn bytes(value: &impl Serialize, limit: usize) -> Option<usize> {
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value).ok()?;
    Some(count.bytes)
}
fn bounded(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// Qt's source grouping by exact connection and folder. The original decoder
/// owns legacy folder keys; large/unsupported plans remain explicitly partial.
pub fn plan(organization: &Value, local: &str, all: bool) -> Plan {
    let mut sources = BTreeMap::<(String, String), Source>::new();
    let mut sizes = BTreeMap::new();
    let mut used = 0usize;
    let mut partial = false;
    for project in swarm::projects(organization) {
        if project["accessible"] == false && !all {
            continue;
        }
        let id = swarm::project_id(project);
        let name = project["name"].as_str().unwrap_or(id);
        if !bounded(id, 320) || !bounded(name, 320) {
            partial = true;
            continue;
        }
        for folder in swarm::folders(project) {
            if !bounded(&folder.machine, 320) || !bounded(&folder.path, 8192) {
                partial = true;
                continue;
            }
            let computer = if folder.machine == local {
                String::new()
            } else {
                folder.machine.clone()
            };
            let key = (computer.clone(), folder.path.clone());
            if !sources.contains_key(&key) && sources.len() >= SOURCES {
                partial = true;
                continue;
            }
            let mut source = sources.get(&key).cloned().unwrap_or_else(|| Source {
                computer,
                machine: folder.machine.clone(),
                path: folder.path.clone(),
                references: Vec::new(),
            });
            if source
                .references
                .iter()
                .any(|r| r.project == id && r.folder == folder)
            {
                continue;
            }
            if source.references.len() >= swarm::MAX_PROJECTS {
                partial = true;
                continue;
            }
            source.references.push(Reference {
                project: id.into(),
                name: name.into(),
                folder,
            });
            let Some(size) = bytes(&source, ITEM_BYTES) else {
                partial = true;
                continue;
            };
            let previous = sizes.get(&key).copied().unwrap_or(0usize);
            let Some(total) = used
                .checked_sub(previous)
                .and_then(|n| n.checked_add(size))
                .filter(|n| *n <= SOURCE_BYTES)
            else {
                partial = true;
                continue;
            };
            sources.insert(key.clone(), source);
            sizes.insert(key, size);
            used = total;
        }
    }
    Plan {
        sources: sources.into_values().collect(),
        partial,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub source: usize,
    pub path: String,
    pub branch: String,
    pub kind: String,
    pub machine: String,
    pub computer: String,
    pub projects: Vec<String>,
    pub project_names: Vec<String>,
    pub category: u8,
    pub state: String,
    pub remains: String,
    pub size: String,
    pub bytes: Option<u64>,
    pub complete: bool,
    pub activity: Option<f64>,
    pub fresh: bool,
    pub sessions: Vec<String>,
    pub sampled_at: u64,
}
fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("")
}
/// Snapshot-only display projection, delegated to the existing review helpers.
/// `verified` means this display came from a checked read, never writer proof.
#[allow(clippy::too_many_arguments)]
pub fn project(
    scope: &str,
    source_index: usize,
    source: &Source,
    row: &Value,
    checked: Option<&Value>,
    verified: bool,
    sampled_at: u64,
) -> Option<Row> {
    if scope.len() > 512 || !["main", "linked", "bare", "folder"].contains(&text(row, "kind")) {
        return None;
    }
    bytes(row, ITEM_BYTES)?;
    if let Some(checked) = checked {
        bytes(checked, ITEM_BYTES)?;
    }
    let path = text(row, "path");
    if !bounded(path, 8192) {
        return None;
    }
    let detail = checked.unwrap_or(row);
    let valid = checked.is_some() && verified;
    let kind = text(row, "kind");
    let mut category = review::category(detail, valid);
    if kind == "folder" && !verified {
        category = 3;
    }
    let id = zerus_desktop_migration::sha256(
        &serde_json::to_vec(&(scope, &source.computer, path)).ok()?,
    );
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let projects = source
        .references
        .iter()
        .filter(|r| ids.insert(r.project.as_str()))
        .map(|r| r.project.clone())
        .collect();
    let project_names = source
        .references
        .iter()
        .filter(|r| names.insert(r.name.as_str()))
        .map(|r| r.name.clone())
        .collect();
    let state = if valid {
        match text(detail, "verdict") {
            "ready" => "Ready",
            "missing" => "Missing",
            "review" => "Review",
            "blocked" => "Blocked",
            _ => "Not checked",
        }
    } else {
        match kind {
            "main" => "Main checkout",
            "bare" => "Bare repository",
            _ if row["locked"] == true => "Locked",
            _ if row["available"] == false => "Missing",
            _ => "Not reviewed",
        }
    };
    let sessions = detail["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| bounded(s, 1024))
        .take(50)
        .map(str::to_owned)
        .collect();
    let result = Row {
        id,
        source: source_index,
        path: path.into(),
        branch: text(row, "branch").into(),
        kind: kind.into(),
        machine: source.machine.clone(),
        computer: source.computer.clone(),
        projects,
        project_names,
        category,
        state: state.into(),
        remains: if valid {
            review::summary_line(detail)
        } else {
            state.into()
        },
        size: review::size(detail),
        bytes: detail["disk_bytes"].as_u64(),
        complete: detail["disk_complete"] == true || detail["verdict"] == "missing",
        activity: detail["last_activity"].as_f64().filter(|n| n.is_finite()),
        fresh: verified,
        sessions,
        sampled_at,
    };
    bytes(&result, ITEM_BYTES).map(|_| result)
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Size,
    Activity,
    Name,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub computer: String,
    pub category: Option<u8>,
    #[serde(default)]
    pub show_main: bool,
    #[serde(default)]
    pub sort: Sort,
}
#[derive(Debug, Serialize)]
pub struct Group {
    pub category: u8,
    pub name: &'static str,
    pub total: usize,
    pub rows: Vec<Row>,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub index: usize,
    pub total: usize,
    pub groups: Vec<Group>,
    pub selected: usize,
    pub selected_ids: Vec<String>,
    pub candidates: usize,
    pub estimated_bytes: Option<u64>,
    pub partial: bool,
}
#[derive(Default)]
pub struct Model {
    rows: BTreeMap<String, Row>,
    sizes: BTreeMap<String, usize>,
    used: usize,
    pub partial: bool,
    selected: BTreeSet<String>,
}
impl Model {
    /// Byte/count refusal never evicts a retained row to admit another one.
    pub fn put(&mut self, row: Row) -> bool {
        if row.id.len() != 64
            || !row
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || row.activity.is_some_and(|n| !n.is_finite())
            || row.category > 4
            || row.source >= SOURCES
        {
            self.partial = true;
            return false;
        }
        if !self.rows.contains_key(&row.id) && self.rows.len() >= ROWS {
            self.partial = true;
            return false;
        }
        let Some(size) = bytes(&row, ITEM_BYTES) else {
            self.partial = true;
            return false;
        };
        let previous = self.sizes.get(&row.id).copied().unwrap_or(0);
        let Some(used) = self
            .used
            .checked_sub(previous)
            .and_then(|n| n.checked_add(size))
            .filter(|n| *n <= ROW_BYTES)
        else {
            self.partial = true;
            return false;
        };
        self.used = used;
        self.sizes.insert(row.id.clone(), size);
        self.rows.insert(row.id.clone(), row);
        true
    }
    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.get(id)
    }
    pub fn retained_bytes(&self) -> usize {
        self.used
    }
    /// Finish Refresh: departed identities leave selection, surviving IDs keep it.
    pub fn retain(&mut self, seen: &BTreeSet<String>) {
        self.rows.retain(|id, _| seen.contains(id));
        self.sizes.retain(|id, _| seen.contains(id));
        self.selected.retain(|id| self.rows.contains_key(id));
        self.used = self.sizes.values().sum();
    }
    pub fn select(&mut self, id: &str, value: bool) -> bool {
        if !self.rows.get(id).is_some_and(|r| r.category != 4) {
            return false;
        }
        if value {
            self.selected.insert(id.into());
        } else {
            self.selected.remove(id);
        }
        true
    }
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }
    pub fn selection(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }
    pub fn replace_selection(&mut self, ids: &[String]) -> bool {
        if ids.len() > ROWS
            || ids
                .iter()
                .any(|id| !self.rows.get(id).is_some_and(|r| r.category != 4))
        {
            return false;
        }
        self.selected = ids.iter().cloned().collect();
        true
    }
    fn matching<'a>(
        &'a self,
        filter: &Filter,
        now: u64,
    ) -> Result<Vec<(&'a Row, u8, bool, String)>, &'static str> {
        if filter.search.len() > 4096
            || filter.project.len() > 320
            || filter.computer.len() > 320
            || filter.category.is_some_and(|n| n > 4)
        {
            return Err("Invalid worktree filter.");
        }
        let query = filter.search.trim().to_lowercase();
        // Sort borrowed rows and one normalized path per row. Only the page's
        // bodies are cloned; a query never copies the whole retained catalog.
        let mut rows: Vec<(&Row, u8, bool, String)> = self
            .rows
            .values()
            .filter_map(|row| {
                let fresh =
                    row.fresh && now >= row.sampled_at && now.saturating_sub(row.sampled_at) < 300;
                let category = if !fresh && row.kind == "linked" {
                    3
                } else {
                    row.category
                };
                if (!filter.show_main && category == 4)
                    || filter.category.is_some_and(|n| n != category)
                    || (!filter.project.is_empty() && !row.projects.contains(&filter.project))
                    || (!filter.computer.is_empty() && row.machine != filter.computer)
                {
                    return None;
                }
                if !query.is_empty()
                    && !std::iter::once(row.path.as_str())
                        .chain(std::iter::once(row.branch.as_str()))
                        .chain(row.project_names.iter().map(String::as_str))
                        .chain(row.sessions.iter().map(String::as_str))
                        .any(|s| s.to_lowercase().contains(&query))
                {
                    return None;
                }
                Some((row, category, fresh, row.path.to_lowercase()))
            })
            .collect();
        rows.sort_by(|(a, ac, _, an), (b, bc, _, bn)| {
            ac.cmp(bc)
                .then_with(|| match filter.sort {
                    Sort::Size => b.bytes.cmp(&a.bytes),
                    Sort::Activity => a
                        .activity
                        .unwrap_or(f64::INFINITY)
                        .total_cmp(&b.activity.unwrap_or(f64::INFINITY)),
                    Sort::Name => an.cmp(bn),
                })
                .then_with(|| an.cmp(bn))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }
    /// Whole filtered candidate selection, never a prefix. Ordinary selection
    /// may exceed 64 for display, but a cleanup registration must refuse it.
    pub fn select_candidates(&mut self, filter: &Filter, now: u64) -> Result<(), &'static str> {
        let ids: Vec<_> = self
            .matching(filter, now)?
            .iter()
            .filter(|(_, category, fresh, _)| *category == 0 && *fresh)
            .map(|(row, _, _, _)| row.id.clone())
            .collect();
        if ids.len() > review::CLEANUP_TARGETS {
            return Err("More than 64 cleanup candidates. Filter by project or computer first.");
        }
        self.selected = ids.into_iter().collect();
        Ok(())
    }
    pub fn cleanup_selection(&self) -> Result<Vec<String>, &'static str> {
        if self.selected.len() > review::CLEANUP_TARGETS {
            return Err("Select at most 64 worktrees for one cleanup.");
        }
        Ok(self.selection())
    }
    pub fn page(&self, filter: &Filter, page: usize, now: u64) -> Result<Page, &'static str> {
        let rows = self.matching(filter, now)?;
        let candidates = rows.iter().filter(|(_, c, f, _)| *c == 0 && *f).count();
        let total = rows.len();
        let index = page.min(total.saturating_sub(1) / PAGE);
        let mut groups = Vec::new();
        for category in 0..=4 {
            let total = rows.iter().filter(|(_, c, _, _)| *c == category).count();
            let page_rows: Vec<_> = rows
                .iter()
                .skip(index * PAGE)
                .take(PAGE)
                .filter(|(_, c, _, _)| *c == category)
                .map(|(row, c, f, _)| {
                    let mut row = (*row).clone();
                    row.category = *c;
                    row.fresh = *f;
                    row
                })
                .collect();
            if !page_rows.is_empty() {
                groups.push(Group {
                    category,
                    name: review::category_name(category),
                    total,
                    rows: page_rows,
                });
            }
        }
        let selected_ids = rows
            .iter()
            .skip(index * PAGE)
            .take(PAGE)
            .filter(|(row, _, _, _)| self.selected.contains(&row.id))
            .map(|(row, _, _, _)| row.id.clone())
            .collect();
        Ok(Page {
            index,
            total,
            groups,
            selected: self.selected.len(),
            selected_ids,
            candidates,
            estimated_bytes: rows
                .iter()
                .filter(|(_, c, f, _)| *c == 0 && *f)
                .try_fold(0u64, |total, (row, _, _, _)| {
                    total.checked_add(row.bytes.unwrap_or(0))
                }),
            partial: self.partial,
        })
    }
}
