//! Explicit Worktrees review. The existing core/CLI checks own removal;
//! this bounded dialog retains immutable review rows and one-use confirmations.
pub mod manual;
pub mod reminder;

use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use tauri::{AppHandle, Emitter, Manager, State};
use zerus_client_core::{
    default_hgs, run_limited, worktree_review as review, worktrees, ComputerId, Exit, Invocation,
};

const LIMIT: usize = review::CLEANUP_TARGETS;
const CHANGED: &str = "Worktree review changed. Review it again before continuing.";
#[derive(Default, Clone)]
pub struct WorktreeCleanup(Arc<Mutex<Model>>);
#[derive(Default)]
struct Model {
    dialog: Option<Dialog>,
    manual: Option<manual::Dialog>,
    reading: bool,
    reminders: reminder::Cache,
    // Never evicted to admit another write. A full retained set refuses new
    // removals; reopening/refreshing a dialog never replays an unknown result.
    unknown: BTreeMap<(String, String, String), String>,
}
struct Dialog {
    id: String,
    revision: u64,
    computer: String,
    machine: String,
    label: String,
    repository: String,
    common: String,
    focus: String,
    rows: Vec<Item>,
    busy: bool,
    partial: bool,
    plan: Option<Plan>,
}
struct Item {
    raw: Value,
    outcome: Option<review::Removed>,
}
struct Plan {
    id: String,
    calls: Vec<(usize, review::Removal)>,
    consent: bool,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn identity(s: &str, cap: usize) -> bool {
    !s.is_empty() && s.len() <= cap && !s.chars().any(char::is_control)
}
fn eligible(row: &Value) -> bool {
    review::removable(row) || review::process_reviewable(row)
}
fn probe(inv: &Invocation) -> Result<(), String> {
    let hgs = default_hgs();
    let probe = worktrees::guard_probe(inv);
    worktrees::guard_supported(
        run_limited(&hgs, &probe, &[], 64 * 1024).map_err(|_| CHANGED.to_owned())?,
    )
}
fn run(inv: &Invocation) -> Result<Exit, String> {
    run_limited(&default_hgs(), inv, &[], review::REVIEW_LIMIT)
        .map_err(|_| "The worktree reply is too large. Review the folder before continuing.".into())
}
pub(crate) fn read(
    computer: &str,
    machine: &str,
    common: &str,
    focus: &str,
) -> Result<Value, String> {
    let mut inv = review::review_invocation(&ComputerId(computer.into()), common, focus);
    worktrees::guard(&mut inv.args, machine);
    probe(&inv)?;
    let value = review::review(run(&inv)?, computer, false)?;
    if value["state"] != "ok" || value["common_dir"] != common {
        return Err(CHANGED.into());
    }
    Ok(value)
}
/// Shared bounded read-only inspector; callers retain their own opaque scope.
pub(crate) fn inspect(
    computer: &str,
    machine: &str,
    path: &str,
    common: &str,
    file: &str,
    offset: usize,
) -> Result<Value, String> {
    if !zerus_client_core::creation::canonical_uuid(machine)
        || file.len() > 4096
        || file.chars().any(char::is_control)
        || offset > 1_000_000
    {
        return Err(CHANGED.into());
    }
    let mut inv =
        review::inspect_invocation(&ComputerId(computer.into()), path, common, file, offset);
    worktrees::guard(&mut inv.args, machine);
    let exit = worktrees::run_guarded(&default_hgs(), &inv, |_, inv| {
        run(inv).unwrap_or_else(Exit::StartFailed)
    })?;
    review::inspected(exit, path, common, file, offset)
}
/// The maintained exact request-bound removal path, shared by manual surfaces.
fn remove(computer: &str, expected: &str, removal: &review::Removal) -> review::Removed {
    let request = uuid::Uuid::new_v4().to_string();
    let mut inv = review::remove_invocation(&ComputerId(computer.into()), removal, &request);
    worktrees::guard(&mut inv.args, expected);
    match probe(&inv) {
        Err(detail) => review::Removed::Refused { reply:serde_json::json!({"status":"kept"}), detail },
        Ok(()) => match run(&inv) {
            Ok(exit) => match review::removed(exit, computer, &request, removal) {
                review::Removed::Done { reply } if reply["common_dir"] != removal.common_dir => review::Removed::Unknown { detail: "Removal could not be confirmed for this repository. An unknown result will not be retried.".into() },
                result => result,
            },
            Err(detail) => review::Removed::Unknown { detail },
        }
    }
}

impl WorktreeCleanup {
    pub(crate) fn unknown(&self, machine: &str, common: &str, path: &str) -> bool {
        lock(&self.0)
            .unknown
            .contains_key(&(machine.into(), common.into(), path.into()))
    }
}
fn rows(value: &Value, focus: &str) -> Result<Vec<Item>, String> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for row in value["worktrees"].as_array().ok_or(CHANGED)? {
        if row["kind"] != "linked" || (!focus.is_empty() && row["path"] != focus) {
            continue;
        }
        let path = text(row, "path");
        if !identity(path, 8192) || !seen.insert(path.to_owned()) {
            return Err(CHANGED.into());
        }
        if result.len() < LIMIT {
            result.push(Item {
                raw: row.clone(),
                outcome: None,
            });
        }
    }
    if serde_json::to_vec(&result.iter().map(|r| &r.raw).collect::<Vec<_>>())
        .map_err(|_| CHANGED)?
        .len()
        > 1024 * 1024
    {
        return Err("The review is too large. Review one worktree at a time.".into());
    }
    Ok(result)
}
#[derive(Serialize)]
pub struct Row {
    id: usize,
    path: String,
    branch: String,
    state: String,
    /// The table cell; `details` carries every line for its tooltip.
    brief: String,
    details: String,
    selectable: bool,
    checked: bool,
}
#[derive(Serialize)]
pub struct View {
    id: String,
    revision: u64,
    computer: String,
    machine: String,
    repository: String,
    focus: String,
    rows: Vec<Row>,
    busy: bool,
    summary: String,
    unknown: bool,
}
#[derive(Serialize)]
pub struct Confirmation {
    id: String,
    paths: Vec<String>,
    branches: Vec<String>,
    ignored: Vec<String>,
    missing: bool,
    consent: Option<String>,
    process_warning: Option<String>,
    button: String,
    machine: String,
}
fn details(row: &Value) -> String {
    let mut lines = review::summary(row);
    if review::local_data(row) {
        lines.push(format!(
            "Ignored files: {}, {}.",
            review::entries(row["ignored_count"].as_u64().unwrap_or(0)),
            review::data_size(row["ignored_bytes"].as_f64().unwrap_or(0.))
        ));
        for item in row["ignored"].as_array().into_iter().flatten().take(5) {
            lines.push(format!(
                "{} ({})",
                text(item, "path"),
                review::data_size(item["bytes"].as_f64().unwrap_or(0.))
            ));
        }
    }
    lines.join("\n").chars().take(6000).collect()
}
impl Dialog {
    fn take_plan(&mut self, id: &str, consent: bool, retained: usize) -> Result<Plan, String> {
        if self.busy {
            return Err(CHANGED.into());
        }
        let plan = self.plan.take().filter(|p| p.id == id).ok_or(CHANGED)?;
        if (plan.consent && !consent) || retained + plan.calls.len() > LIMIT {
            return Err("Confirm the current review, or inspect retained unknown outcomes before removing another folder.".into());
        }
        Ok(plan)
    }
    fn view(&self) -> View {
        let rows: Vec<Row> = self
            .rows
            .iter()
            .enumerate()
            .map(|(id, item)| {
                let raw = &item.raw;
                let (state, detail) = match &item.outcome {
                    Some(review::Removed::Done { reply }) => (
                        if reply["status"] == "forgotten" {
                            "Forgotten"
                        } else {
                            "Removed"
                        },
                        text(reply, "branch_error").to_owned(),
                    ),
                    Some(review::Removed::Refused { detail, .. }) => ("Kept", detail.clone()),
                    Some(review::Removed::Unknown { detail }) => ("Unknown", detail.clone()),
                    None => (
                        if review::process_reviewable(raw) {
                            "Check running programs"
                        } else {
                            match text(raw, "verdict") {
                                "ready" => "Ready",
                                "review" => "Review",
                                "missing" => "Missing",
                                _ => "Blocked",
                            }
                        },
                        details(raw),
                    ),
                };
                let selectable = item.outcome.is_none() && eligible(raw);
                Row {
                    id,
                    path: text(raw, "path").into(),
                    branch: text(raw, "branch").into(),
                    state: state.into(),
                    // A result shows its message; a fresh row the design's one-line summary.
                    brief: if item.outcome.is_some() {
                        detail.lines().next().unwrap_or_default().to_owned()
                    } else {
                        review::summary_line(raw)
                    },
                    details: detail,
                    selectable,
                    checked: selectable
                        && !review::local_data(raw)
                        && !review::process_reviewable(raw)
                        && (raw["verdict"] != "review" || !self.focus.is_empty()),
                }
            })
            .collect();
        let counts = |state: &str| rows.iter().filter(|r| r.state == state).count();
        let unknown = counts("Unknown") > 0;
        let summary = if rows.iter().any(|r| self.rows[r.id].outcome.is_some()) {
            format!(
                "{} removed  {} kept  {} unknown",
                counts("Removed") + counts("Forgotten"),
                counts("Kept"),
                counts("Unknown")
            )
        } else if rows.is_empty() {
            if self.focus.is_empty() {
                "This repository has no linked worktrees.".into()
            } else {
                "Git no longer lists this worktree.".into()
            }
        } else {
            let parts: Vec<String> = [
                (counts("Ready"), "ready"),
                (
                    counts("Review") + counts("Check running programs"),
                    "to review",
                ),
                (counts("Missing"), "missing"),
                (counts("Blocked"), "blocked"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, s)| format!("{n} {s}"))
            .collect();
            format!(
                "{}{}",
                parts.join(", ") + ".",
                if self.partial {
                    " Only part of the repository was reviewed."
                } else {
                    ""
                }
            )
        };
        View {
            id: self.id.clone(),
            revision: self.revision,
            computer: self.computer.clone(),
            machine: self.label.clone(),
            repository: self.repository.clone(),
            focus: self.focus.clone(),
            rows,
            busy: self.busy,
            summary,
            unknown,
        }
    }
    fn prepare(
        &mut self,
        revision: u64,
        selected: &[usize],
        branches: bool,
    ) -> Result<Confirmation, String> {
        if self.busy || self.revision != revision || selected.is_empty() || selected.len() > LIMIT {
            return Err(CHANGED.into());
        }
        let mut seen = BTreeSet::new();
        let mut calls = Vec::new();
        let mut picked = Vec::new();
        for &id in selected {
            if !seen.insert(id) {
                return Err(CHANGED.into());
            }
            let item = self.rows.get(id).ok_or(CHANGED)?;
            if item.outcome.is_some() || !eligible(&item.raw) {
                return Err(CHANGED.into());
            }
            let row = &item.raw;
            picked.push(row);
            calls.push((
                id,
                review::Removal {
                    path: text(row, "path").into(),
                    common_dir: self.common.clone(),
                    fingerprint: text(row, "fingerprint").into(),
                    delete_branch: branches
                        && row["merged"] == true
                        && !text(row, "branch").is_empty(),
                    ignored_fingerprint: if review::local_data(row) {
                        text(row, "ignored_fingerprint").into()
                    } else {
                        String::new()
                    },
                    process_fingerprint: if review::process_reviewable(row) {
                        text(row, "process_fingerprint").into()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                },
            ));
        }
        let c = review::remove_confirm(&picked);
        let id = uuid::Uuid::new_v4().to_string();
        let confirmation = Confirmation {
            id: id.clone(),
            paths: picked.iter().map(|r| text(r, "path").into()).collect(),
            branches: picked
                .iter()
                .filter(|r| branches && r["merged"] == true && !text(r, "branch").is_empty())
                .map(|r| text(r, "branch").into())
                .collect(),
            ignored: picked
                .iter()
                .filter(|r| review::local_data(r))
                .map(|r| {
                    format!(
                        "{}: {}, {}\n{}",
                        text(r, "path"),
                        review::entries(r["ignored_count"].as_u64().unwrap_or(0)),
                        review::data_size(r["ignored_bytes"].as_f64().unwrap_or(0.)),
                        r["ignored"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .take(5)
                            .map(|i| format!(
                                "{} ({})",
                                text(i, "path"),
                                review::data_size(i["bytes"].as_f64().unwrap_or(0.))
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect(),
            missing: picked.iter().any(|r| r["verdict"] == "missing"),
            consent: c.consent,
            process_warning: c.process_warning,
            button: c.button,
            machine: self.label.clone(),
        };
        self.plan = Some(Plan {
            id,
            calls,
            consent: confirmation.consent.is_some(),
        });
        Ok(confirmation)
    }
}
fn apply_unknown(model: &mut Model) {
    if let Some(d) = &mut model.dialog {
        for item in &mut d.rows {
            if let Some(detail) = model.unknown.get(&(
                d.machine.clone(),
                d.common.clone(),
                text(&item.raw, "path").into(),
            )) {
                item.outcome = Some(review::Removed::Unknown {
                    detail: detail.clone(),
                });
            }
        }
        for ((machine, common, path), detail) in &model.unknown {
            if machine == &d.machine
                && common == &d.common
                && (d.focus.is_empty() || path == &d.focus)
                && !d.rows.iter().any(|r| text(&r.raw, "path") == path)
            {
                // Retain terminal uncertainty even after Git stops listing
                // the folder. It is never converted into a Missing action.
                if d.rows.len() == LIMIT {
                    let Some(at) = d
                        .rows
                        .iter()
                        .rposition(|r| !matches!(r.outcome, Some(review::Removed::Unknown { .. })))
                    else {
                        continue;
                    };
                    d.rows.remove(at);
                    d.partial = true;
                }
                d.rows.push(Item {
                    raw: serde_json::json!({"path":path,"kind":"linked"}),
                    outcome: Some(review::Removed::Unknown {
                        detail: detail.clone(),
                    }),
                });
            }
        }
    }
}
fn apply_termination_unknown(
    model: &mut Model,
    term: &crate::termination_cleanup::TerminationCleanup,
) {
    if let Some(dialog) = &mut model.dialog {
        for item in &mut dialog.rows {
            if term.unknown(&dialog.machine, &dialog.common, text(&item.raw, "path")) {
                item.outcome = Some(review::Removed::Unknown {
                    detail: "An unknown result will not be retried.".into(),
                });
            }
        }
    }
}

pub(crate) fn machine(app: &crate::commands::AppState, computer: &str) -> Result<String, String> {
    if crate::actions::blocked() || !app.pump().online(computer) {
        return Err("Reconnect this computer before reviewing worktrees.".into());
    }
    app.pump()
        .machine(&ComputerId(computer.into()))
        .and_then(|m| m.machine_id)
        .filter(|id| zerus_client_core::creation::canonical_uuid(id))
        .ok_or_else(|| "Refresh this computer's identity before reviewing worktrees.".into())
}
#[tauri::command]
pub async fn worktree_cleanup_open(
    app: State<'_, crate::commands::AppState>,
    runtime: AppHandle,
    state: State<'_, WorktreeCleanup>,
    catalogs: State<'_, crate::newsession::NewSession>,
    computer: String,
    path: String,
    focus: String,
) -> Result<View, String> {
    if (computer.len() > 320 || computer.chars().any(char::is_control))
        || !identity(&path, 8192)
        || (!focus.is_empty() && !identity(&focus, 8192))
    {
        return Err(CHANGED.into());
    }
    let machine = machine(&app, &computer)?;
    let label = if computer.is_empty() {
        app.pump()
            .machine(&ComputerId(computer.clone()))
            .and_then(|m| m.host)
            .unwrap_or_else(|| "this computer".into())
    } else {
        computer.clone()
    };
    {
        let mut m = lock(&state.0);
        if m.reading
            || m.manual.as_ref().is_some_and(|d| d.busy)
            || m.dialog.as_ref().is_some_and(|d| d.busy)
        {
            return Err("A worktree review is already running.".into());
        }
        m.reading = true;
        m.reminders.invalidate();
        m.manual = None;
        if let Some(d) = &mut m.dialog {
            d.plan = None;
            d.revision += 1;
        }
    }
    let catalogs = catalogs.inner().clone();
    let own = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let result: Result<Dialog, String> = (|| {
            let data = catalogs.catalog(&ComputerId(computer.clone()), &path, true)?;
            let common = text(&data, "common_dir");
            if data["state"] != "ok"
                || !identity(common, 8192)
                || (!focus.is_empty()
                    && !data["worktrees"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|r| r["path"] == focus && r["kind"] == "linked"))
            {
                return Err(CHANGED.into());
            }
            let value = read(&computer, &machine, common, &focus)?;
            let rows = rows(&value, &focus)?;
            Ok(Dialog {
                id: uuid::Uuid::new_v4().to_string(),
                revision: 1,
                label,
                computer,
                machine,
                repository: path,
                common: common.into(),
                focus,
                rows,
                busy: false,
                partial: value["partial"] == true
                    || value["worktrees"]
                        .as_array()
                        .is_some_and(|a| a.len() > LIMIT),
                plan: None,
            })
        })();
        let mut m = lock(&own.0);
        m.reading = false;
        let dialog = result?;
        m.dialog = Some(dialog);
        apply_unknown(&mut m);
        apply_termination_unknown(
            &mut m,
            &runtime.state::<crate::termination_cleanup::TerminationCleanup>(),
        );
        Ok(m.dialog.as_ref().unwrap().view())
    })
    .await
    .map_err(|e| e.to_string())?;
    result
}
#[tauri::command]
pub fn worktree_cleanup_view(
    state: State<'_, WorktreeCleanup>,
    id: String,
) -> Result<View, String> {
    let m = lock(&state.0);
    let d = m.dialog.as_ref().filter(|d| d.id == id).ok_or(CHANGED)?;
    Ok(d.view())
}
#[tauri::command]
pub async fn worktree_cleanup_refresh(
    state: State<'_, WorktreeCleanup>,
    runtime: AppHandle,
    id: String,
) -> Result<View, String> {
    let (computer, machine, common, focus) = {
        let mut m = lock(&state.0);
        if m.reading || m.manual.as_ref().is_some_and(|d| d.busy) {
            return Err(CHANGED.into());
        }
        let d = m
            .dialog
            .as_mut()
            .filter(|d| d.id == id && !d.busy)
            .ok_or(CHANGED)?;
        d.busy = true;
        d.plan = None;
        (
            d.computer.clone(),
            d.machine.clone(),
            d.common.clone(),
            d.focus.clone(),
        )
    };
    let own = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = read(&computer, &machine, &common, &focus).and_then(|v| rows(&v, &focus));
        let mut m = lock(&own.0);
        let d = m.dialog.as_mut().filter(|d| d.id == id).ok_or(CHANGED)?;
        d.busy = false;
        d.revision += 1;
        let mut rows = match result {
            Ok(rows) => rows,
            Err(error) => {
                for item in &mut d.rows {
                    if item.outcome.is_none() {
                        item.outcome = Some(review::Removed::Refused {
                            reply: serde_json::json!({"status":"kept"}),
                            detail: error.clone(),
                        });
                    }
                }
                return Err(error);
            }
        };
        for old in &d.rows {
            if matches!(
                old.outcome,
                Some(review::Removed::Done { .. } | review::Removed::Unknown { .. })
            ) {
                rows.retain(|r| r.raw["path"] != old.raw["path"]);
                if rows.len() < LIMIT {
                    rows.push(Item {
                        raw: old.raw.clone(),
                        outcome: old.outcome.clone(),
                    });
                }
            }
        }
        d.rows = rows;
        apply_unknown(&mut m);
        apply_termination_unknown(
            &mut m,
            &runtime.state::<crate::termination_cleanup::TerminationCleanup>(),
        );
        Ok(m.dialog.as_ref().unwrap().view())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn worktree_cleanup_prepare(
    state: State<'_, WorktreeCleanup>,
    id: String,
    revision: u64,
    selected: Vec<usize>,
    branches: bool,
) -> Result<Confirmation, String> {
    let mut m = lock(&state.0);
    if m.reading || m.manual.as_ref().is_some_and(|d| d.busy) {
        return Err(CHANGED.into());
    }
    let d = m.dialog.as_mut().filter(|d| d.id == id).ok_or(CHANGED)?;
    d.prepare(revision, &selected, branches)
}
#[tauri::command]
pub fn worktree_cleanup_confirm(
    app: AppHandle,
    state: State<'_, WorktreeCleanup>,
    catalogs: State<'_, crate::newsession::NewSession>,
    id: String,
    confirmation: String,
    consent: bool,
) -> Result<View, String> {
    let mut m = lock(&state.0);
    if m.reading || m.manual.as_ref().is_some_and(|d| d.busy) {
        return Err(CHANGED.into());
    }
    let quota = m.unknown.len();
    let d = m
        .dialog
        .as_mut()
        .filter(|d| d.id == id && !d.busy)
        .ok_or(CHANGED)?;
    let plan = d.take_plan(&confirmation, consent, quota)?;
    d.busy = true;
    d.revision += 1;
    let view = d.view();
    let own = state.inner().clone();
    let catalogs = catalogs.inner().clone();
    drop(m);
    tauri::async_runtime::spawn_blocking(move || {
        for (at, removal) in plan.calls {
            let (computer, expected) = {
                let m = lock(&own.0);
                let Some(d) = m.dialog.as_ref().filter(|d| d.id == id) else {
                    return;
                };
                (d.computer.clone(), d.machine.clone())
            };
            let term = app.state::<crate::termination_cleanup::TerminationCleanup>();
            let result = if term.unknown(&expected, &removal.common_dir, &removal.path) {
                review::Removed::Unknown {
                    detail: "An unknown result will not be retried.".into(),
                }
            } else if term.unknown_overflow() {
                review::Removed::Refused{reply:serde_json::json!({"status":"kept"}),detail:"Too many unconfirmed removals are retained. Review them before removing more worktrees.".into()}
            } else if machine(&app.state::<crate::commands::AppState>(), &computer).as_ref()
                != Ok(&expected)
            {
                review::Removed::Refused {
                    reply: serde_json::json!({"status":"kept"}),
                    detail: "Computer identity changed. Reconnect and review again.".into(),
                }
            } else {
                remove(&computer, &expected, &removal)
            };
            catalogs.invalidate_worktrees(&ComputerId(computer.clone()), &removal.common_dir);
            let mut m = lock(&own.0);
            if matches!(&result, review::Removed::Done { .. }) {
                m.reminders
                    .removed(&computer, &expected, &removal.common_dir);
            }
            if matches!(&result, review::Removed::Unknown { .. })
                || matches!(&result,review::Removed::Refused{reply,..} if reply["status"]!="kept")
            {
                let detail = match &result {
                    review::Removed::Unknown { detail }
                    | review::Removed::Refused { detail, .. } => detail.clone(),
                    _ => String::new(),
                };
                m.unknown
                    .insert((expected, removal.common_dir, removal.path), detail);
            }
            if let Some(d) = m.dialog.as_mut().filter(|d| d.id == id) {
                d.rows[at].outcome = Some(result);
                d.revision += 1;
            }
            apply_unknown(&mut m);
            let _ = app.emit("worktree-cleanup-changed", ());
        }
        let mut m = lock(&own.0);
        if let Some(d) = m.dialog.as_mut().filter(|d| d.id == id) {
            d.busy = false;
            d.revision += 1;
        }
        let _ = app.emit("worktree-cleanup-changed", ());
    });
    Ok(view)
}

#[tauri::command]
pub fn worktree_cleanup_close(state: State<'_, WorktreeCleanup>, id: String) -> Result<(), String> {
    let mut m = lock(&state.0);
    let d = m
        .dialog
        .as_mut()
        .filter(|d| d.id == id && !d.busy)
        .ok_or(CHANGED)?;
    d.plan = None;
    d.revision += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ready(path: &str) -> Value {
        json!({"path":path,"kind":"linked","branch":"feature","head":"original-head","verdict":"ready","fingerprint":"original-proof","merged":true,"ignored_count":0,"reasons":[]})
    }
    fn dialog(source: Value) -> Dialog {
        Dialog {
            id: "dialog-1".into(),
            revision: 7,
            computer: "example-peer".into(),
            machine: "machine-1".into(),
            label: "example-peer".into(),
            repository: "/srv/example".into(),
            common: "/srv/example/.git".into(),
            focus: String::new(),
            rows: rows(&source, "").unwrap(),
            busy: false,
            partial: false,
            plan: None,
        }
    }
    #[test]
    fn confirmation_keeps_original_fingerprints_and_needs_explicit_local_consent() {
        let mut raw = ready("/srv/example/work");
        raw["verdict"] = json!("review");
        raw["ignored_count"] = json!(2);
        raw["ignored_bytes"] = json!(4096);
        raw["ignored_fingerprint"] = json!("ignored-proof");
        raw["ignored"] = json!([{"path":"target/","bytes":4096}]);
        let mut d = dialog(json!({"worktrees":[raw]}));
        assert!(
            !d.view().rows[0].checked,
            "local files need an explicit selection"
        );
        let c = d.prepare(7, &[0], false).unwrap();
        assert!(c.branches.is_empty());
        assert!(c.ignored[0].contains("2 entries, 4.00 KiB"));
        assert!(c.ignored[0].contains("target/"));
        assert!(d.take_plan(&c.id, false, 0).is_err());
        assert!(
            d.take_plan(&c.id, true, 0).is_err(),
            "declined/consumed confirmation is never reused"
        );
        let c = d.prepare(7, &[0], true).unwrap();
        assert_eq!(c.branches, ["feature"]);
        let p = d.take_plan(&c.id, true, 0).unwrap();
        let removal = &p.calls[0].1;
        assert_eq!(removal.fingerprint, "original-proof");
        assert_eq!(removal.ignored_fingerprint, "ignored-proof");
        assert_eq!(removal.common_dir, "/srv/example/.git");
        assert!(removal.delete_branch);
        assert!(removal.discard_fingerprint.is_empty());
        assert!(!removal.preserve_archive_restore);
        assert!(d.take_plan(&c.id, true, 0).is_err());
        let c = d.prepare(7, &[0], false).unwrap();
        assert!(
            !d.take_plan(&c.id, true, 0).unwrap().calls[0]
                .1
                .delete_branch,
            "branch choice is never remembered"
        );
    }
    #[test]
    fn stale_duplicate_blocked_and_unknown_selections_cannot_prepare_an_effect() {
        let mut protected = ready("/srv/example/protected");
        protected["verdict"] = json!("blocked");
        protected["discardable"] = json!(true);
        protected["discard_fingerprint"] = json!("discard");
        let mut d = dialog(json!({"worktrees":[ready("/srv/example/work"),protected]}));
        for (revision, ids) in [
            (6, vec![0]),
            (7, vec![0, 0]),
            (7, vec![2]),
            (7, vec![1]),
            (7, vec![]),
        ] {
            assert!(d.prepare(revision, &ids, false).is_err());
        }
        d.rows[0].outcome = Some(review::Removed::Unknown {
            detail: "not confirmed".into(),
        });
        assert!(!d.view().rows[0].selectable);
        assert!(d.prepare(7, &[0], false).is_err());
        d.rows[0].outcome = None;
        let c = d.prepare(7, &[0], false).unwrap();
        d.plan = None;
        d.revision += 1;
        assert!(
            d.take_plan(&c.id, true, 0).is_err(),
            "Cancel/refresh revokes the pending confirmation"
        );
        let c = d.prepare(8, &[0], false).unwrap();
        assert!(
            d.take_plan(&c.id, true, LIMIT).is_err(),
            "unknown outcome bounds never evict history to admit writes"
        );
    }
    #[test]
    fn only_exact_reviewed_merged_branches_and_narrow_process_acceptance_are_offered() {
        let merged = ready("/srv/example/merged");
        let mut unmerged = ready("/srv/example/unmerged");
        unmerged["merged"] = json!(false);
        unmerged["branch"] = json!("unmerged");
        let mut process = ready("/srv/example/process");
        process["verdict"] = json!("blocked");
        process["process_fingerprint"] = json!("five-minute-proof");
        process["reasons"] = json!([{"code":"processes_unavailable"}]);
        process["branch"] = json!("");
        let mut d = dialog(json!({"worktrees":[merged,unmerged,process]}));
        let c = d.prepare(7, &[0, 1, 2], true).unwrap();
        assert_eq!(c.branches, ["feature"]);
        assert!(c.process_warning.is_some());
        assert!(c.consent.is_some());
        let p = d.take_plan(&c.id, true, 0).unwrap();
        assert!(p.calls[0].1.delete_branch);
        assert!(!p.calls[1].1.delete_branch);
        assert!(!p.calls[2].1.delete_branch);
        assert_eq!(p.calls[2].1.process_fingerprint, "five-minute-proof");
        assert!(
            !serde_json::to_string(&d.view())
                .unwrap()
                .contains("original-proof"),
            "the webview cannot substitute a fingerprint"
        );
    }
    #[test]
    fn unknown_outcomes_survive_refresh_and_reopen_even_if_git_no_longer_lists_the_path() {
        let mut m = Model {
            dialog: Some(dialog(json!({"worktrees":[ready("/srv/example/new")]}))),
            ..Default::default()
        };
        for i in 0..LIMIT {
            m.unknown.insert(
                (
                    "machine-1".into(),
                    "/srv/example/.git".into(),
                    format!("/srv/example/unknown-{i}"),
                ),
                "Not confirmed".into(),
            );
        }
        apply_unknown(&mut m);
        let d = m.dialog.as_mut().unwrap();
        let view = d.view();
        assert_eq!(view.rows.len(), LIMIT);
        assert!(view
            .rows
            .iter()
            .all(|r| r.state == "Unknown" && !r.selectable));
        assert!(d.prepare(7, &[0], false).is_err());
        m.dialog = Some(dialog(json!({"worktrees":[]})));
        apply_unknown(&mut m);
        assert_eq!(m.dialog.as_ref().unwrap().view().rows.len(), LIMIT);
        m.dialog.as_mut().unwrap().machine = "replacement-machine".into();
        m.dialog.as_mut().unwrap().rows.clear();
        apply_unknown(&mut m);
        assert!(
            m.dialog.as_ref().unwrap().view().rows.is_empty(),
            "an old identity never supplies proof for a replacement"
        );
    }
    #[test]
    fn reviews_are_bounded_scoped_and_refuse_ambiguous_paths() {
        assert!(rows(
            &json!({"worktrees":[ready("/srv/example/w"),ready("/srv/example/w")]}),
            ""
        )
        .is_err());
        let source = json!({"worktrees":(0..512).map(|i|ready(&format!("/srv/example/w{i}"))).collect::<Vec<_>>()});
        assert_eq!(rows(&source, "").unwrap().len(), LIMIT);
        assert_eq!(rows(&source, "/srv/example/w5").unwrap().len(), 1);
        assert!(rows(
            &json!({"worktrees":[{"kind":"linked","path":"bad\nidentity"}]}),
            ""
        )
        .is_err());
    }
}
