//! Explicit post-Terminate cleanup. Existing core state machines and guarded
//! CLI calls own every decision; this module holds bounded opaque UI handles.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, VecDeque},
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, State};
use zerus_client_core::{default_hgs, run_limited, ComputerId, Exit, Invocation, Outcome, Target};
use zerus_client_core::{
    termination_cleanup::{
        retained_notice, ArchiveIdentity, Capture, Cleanup, Consents, Entry, Status, Step,
        STATE_CHANGED,
    },
    worktree_review as review, worktrees,
};

const LIMIT: usize = review::CLEANUP_TARGETS;
const PREF: &str = "workspace/removeWorktreeAfterTermination";
const OFFER_AGE: Duration = Duration::from_secs(10 * 60);
#[derive(Default)]
pub struct TerminationCleanup(Arc<Inner>);
#[derive(Default)]
struct Inner {
    model: Mutex<Model>,
}
#[derive(Default)]
struct Model {
    offers: VecDeque<Offer>,
    jobs: VecDeque<Job>,
    reserved: usize,
    reads: BTreeSet<String>,
    revision: u64,
    overflow_kept: usize,
    overflow_unknown: usize,
    restoring: BTreeSet<String>,
}
impl TerminationCleanup {
    pub(crate) fn unknown_overflow(&self) -> bool {
        lock(&self.0.model).overflow_unknown > 0
    }
    pub(crate) fn unknown(&self, machine: &str, common: &str, path: &str) -> bool {
        let model = lock(&self.0.model);
        model.jobs.iter().flat_map(|j| &j.cleanup.entries).any(|e| {
            e.machine == machine
                && e.common_dir == common
                && e.row["path"] == path
                && e.status == Some(Status::Unconfirmed)
        })
    }
}

struct Offer {
    id: String,
    capture: Capture,
    at: Instant,
}
struct Job {
    id: String,
    cleanup: Cleanup,
    busy: bool,
    phase: &'static str,
    manual_review: bool,
    source: Option<Target>,
    archive: Option<ArchiveIdentity>,
    restore_supported: bool,
}

/// The same exact identifiers the Archive view restores. No local path or
/// cleanup fingerprint crosses back from the webview, including on overflow.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreSource {
    pub target: Target,
    pub machine: String,
    pub archive_id: String,
    /// Display bookkeeping only; never used to admit the restore operation.
    pub status: Status,
    pub overflow: bool,
}
#[derive(Serialize)]
pub struct RetainedCapture {
    pub id: Option<String>,
    pub restore: Option<RestoreSource>,
    pub notice: String,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
fn bounded_run(hgs: &Path, inv: &Invocation) -> Exit {
    run_limited(hgs, inv, &[], review::REVIEW_LIMIT).unwrap_or(Exit::StartFailed(
        "The reply is too large. Check the folder before continuing.".into(),
    ))
}
pub(crate) fn probe(
    hgs: &Path,
    target: &Target,
    runner: &impl Fn(&Path, &Invocation) -> Exit,
) -> Result<(), String> {
    let mut inv = zerus_client_core::details::details_invocation(target, None);
    inv = worktrees::guard_probe(&inv);
    inv.timeout = Duration::from_secs(5);
    let exit = runner(hgs, &inv);
    let supported = matches!(&exit, Exit::Code{code:0,stdout,..} if stdout.len() <= 64*1024 && String::from_utf8_lossy(stdout).contains("scoped native lifecycle action"));
    worktrees::guard_supported(exit)?;
    if !supported {
        return Err("Update hgs before removing a working folder after Terminate.".into());
    }
    Ok(())
}
pub(crate) fn inspection(
    hgs: &Path,
    target: &Target,
    machine: &str,
    runner: &impl Fn(&Path, &Invocation) -> Exit,
) -> Result<Value, String> {
    let mut inv = zerus_client_core::details::details_invocation(target, None);
    worktrees::guard(&mut inv.args, machine);
    match runner(hgs, &inv) {
        Exit::Code {
            code: 0, stdout, ..
        } if stdout.len() <= review::REVIEW_LIMIT => {
            serde_json::from_slice(&stdout).map_err(|_| STATE_CHANGED.into())
        }
        _ => Err(STATE_CHANGED.into()),
    }
}

pub(crate) struct ReadGuard(Arc<Inner>, String);
impl TerminationCleanup {
    pub(crate) fn read_terminal_identity(&self) -> Result<ReadGuard, String> {
        self.0.read()
    }
}
impl Drop for ReadGuard {
    fn drop(&mut self) {
        lock(&self.0.model).reads.remove(&self.1);
    }
}
impl Inner {
    fn read(self: &Arc<Self>) -> Result<ReadGuard, String> {
        let mut m = lock(&self.model);
        if m.reads.len() >= 4 {
            return Err(
                "A folder inspection is already running. Try again after it finishes.".into(),
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        m.reads.insert(id.clone());
        Ok(ReadGuard(Arc::clone(self), id))
    }
}

#[derive(Serialize)]
pub struct OfferView {
    id: String,
    checked: bool,
}
#[derive(Serialize)]
pub struct Row {
    id: String,
    label: String,
    path: String,
    machine: String,
    computer: String,
    branch: String,
    size: String,
    status: &'static str,
    reason: String,
    details: String,
    can_check: bool,
    ready: bool,
    busy: bool,
    source: Option<Target>,
    notice: Option<String>,
    restore: Option<RestoreSource>,
}
#[derive(Serialize)]
pub struct View {
    revision: u64,
    rows: Vec<Row>,
    busy: bool,
    cleaning: bool,
    removed: usize,
    kept: usize,
    unknown: usize,
    required: Consents,
    text: String,
    title: String,
}
fn leaf(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .into()
}
fn ready(entry: &zerus_client_core::termination_cleanup::Entry) -> bool {
    !matches!(entry.status, Some(Status::Removed | Status::Unconfirmed))
        && entry.verified
        && (review::removable(&entry.row)
            || review::process_reviewable(&entry.row)
            || (entry.row["discardable"] == true
                && entry.row["discard_fingerprint"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())))
}
impl Model {
    fn retain(
        &mut self,
        target: &Target,
        machine: &str,
        report: &zerus_client_core::WorktreeCleanup,
        restore_supported: bool,
    ) -> Option<RetainedCapture> {
        self.offers.retain(|offer| offer.at.elapsed() < OFFER_AGE);
        if !matches!(report.status.as_str(), "kept" | "unconfirmed")
            || report.path.len() > 32 * 1024
            || report.common_dir.len() > 32 * 1024
            || [report.path.as_str(), report.common_dir.as_str()]
                .into_iter()
                .any(|p| {
                    p.chars().any(char::is_control)
                        || !(zerus_client_core::paths::absolute(
                            p,
                            zerus_client_core::paths::Style::Posix,
                        ) || zerus_client_core::paths::absolute(
                            p,
                            zerus_client_core::paths::Style::Windows,
                        ))
                })
        {
            return None;
        }
        let archive = report
            .archive
            .as_deref()
            .filter(|a| a.matches(target))
            .cloned();
        let fallback = json!({});
        let notice = retained_notice(
            report
                .review_row()
                .or_else(|| (report.status == "unconfirmed").then_some(&fallback)),
            archive.as_ref().is_some_and(|a| !a.launch_owned),
            report.status == "unconfirmed",
        );
        let mut restore = archive
            .as_ref()
            .filter(|_| restore_supported && zerus_client_core::creation::canonical_uuid(machine))
            .map(|a| RestoreSource {
                target: target.clone(),
                machine: machine.into(),
                archive_id: a.archive_id.clone(),
                status: if report.status == "unconfirmed" {
                    Status::Unconfirmed
                } else {
                    Status::Kept
                },
                overflow: false,
            });
        if let Some(job) = self.jobs.iter().find(|j| {
            j.source.as_ref() == Some(target)
                && j.cleanup
                    .entries
                    .first()
                    .is_some_and(|e| e.row["path"] == report.path)
        }) {
            let original = self.view().rows.into_iter().find(|row| row.id == job.id)?;
            return Some(RetainedCapture {
                id: Some(job.id.clone()),
                restore: original.restore,
                notice: original
                    .notice
                    .unwrap_or_else(|| "Working folder kept for review.".into()),
            });
        }
        if self.jobs.len() + self.offers.len() + self.reserved >= LIMIT {
            if let Some(restore) = &mut restore {
                restore.overflow = true;
            }
            if report.status == "unconfirmed" {
                self.overflow_unknown = self.overflow_unknown.saturating_add(1);
            } else {
                self.overflow_kept = self.overflow_kept.saturating_add(1);
            }
            return Some(RetainedCapture {
                id: None,
                restore,
                notice,
            });
        }
        let owner = archive
            .as_ref()
            .filter(|a| a.launch_owned)
            .map_or("", |a| a.owner_id.as_str());
        let mut entry = Entry::terminated(
            &target.computer,
            machine,
            &report.path,
            &report.common_dir,
            owner,
            Some(report),
        )?;
        entry.offline = !zerus_client_core::creation::canonical_uuid(machine);
        entry.status = Some(if report.status == "unconfirmed" {
            Status::Unconfirmed
        } else {
            Status::Kept
        });
        entry.detail = report.reason.clone().unwrap_or_default();
        if let Some(row) = report.review_row() {
            entry.row = row.clone();
            entry.verified = true;
        }
        if report.details_omitted {
            entry.review_error =
                "The reply is too large. Check the folder before continuing.".into();
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.jobs.push_back(Job {
            id: id.clone(),
            cleanup: Cleanup::retained(vec![entry]),
            busy: false,
            phase: "checking",
            manual_review: false,
            source: Some(target.clone()),
            archive,
            restore_supported,
        });
        Some(RetainedCapture {
            id: Some(id),
            restore,
            notice,
        })
    }
    fn view(&self) -> View {
        let mut rows = Vec::new();
        let (mut removed, mut kept, mut unknown) = (0, self.overflow_kept, self.overflow_unknown);
        let mut required = Consents::default();
        for job in &self.jobs {
            let restoring =
                job.source
                    .as_ref()
                    .zip(job.archive.as_ref())
                    .is_some_and(|(target, archive)| {
                        self.restoring
                            .contains(&format!("{}\n{}", target.computer, archive.archive_id))
                    });
            for entry in &job.cleanup.entries {
                let path = entry.row["path"].as_str().unwrap_or("");
                let status = match entry.status {
                    Some(Status::Removed) => {
                        removed += 1;
                        "removed"
                    }
                    Some(Status::Unconfirmed) => {
                        unknown += 1;
                        "unknown"
                    }
                    Some(Status::Kept) => {
                        kept += 1;
                        "kept"
                    }
                    None => {
                        if job.busy && !restoring {
                            job.phase
                        } else {
                            "ready"
                        }
                    }
                };
                let unowned = job.archive.as_ref().is_some_and(|a| !a.launch_owned);
                let reason = if unowned {
                    "Not created by Zerus; review the folder manually".into()
                } else if !entry.detail.is_empty() {
                    entry.detail.clone()
                } else if !entry.review_error.is_empty() {
                    entry.review_error.clone()
                } else if entry.status == Some(Status::Kept) {
                    job.cleanup.retained_reason().unwrap_or_default()
                } else {
                    review::summary_line(&entry.row)
                };
                let r = if job.manual_review {
                    job.cleanup.required()
                } else {
                    Consents::default()
                };
                required.local_files |= r.local_files;
                required.discard_changes |= r.discard_changes;
                required.unchecked_processes |= r.unchecked_processes;
                let computer = if entry.host.is_empty() {
                    "this computer"
                } else {
                    entry.host.as_str()
                };
                let details=format!("Folder: {path}\nComputer: {}\n{}\n{}\nAfter removal: conversations and branches remain. Restoring an archived session may require recreating its working folder.", computer,reason,review::decision(&entry.row).join("\n"));
                rows.push(Row {
                    id: job.id.clone(),
                    label: leaf(path),
                    path: path.into(),
                    machine: if entry.host.is_empty() {
                        "this computer".into()
                    } else {
                        entry.host.clone()
                    },
                    computer: entry.host.clone(),
                    branch: entry.row["branch"].as_str().unwrap_or("—").into(),
                    size: review::size(&entry.row),
                    status: if unowned && status == "kept" {
                        "review"
                    } else {
                        status
                    },
                    reason: reason.chars().take(4000).collect(),
                    details: details.chars().take(128 * 1024).collect(),
                    can_check: !job.busy
                        && job.cleanup.can_refresh()
                        && zerus_client_core::creation::canonical_uuid(&entry.machine),
                    ready: !job.busy
                        && job.manual_review
                        && ready(entry)
                        && zerus_client_core::creation::canonical_uuid(&entry.machine),
                    busy: job.busy,
                    source: job.source.clone(),
                    notice: matches!(entry.status, Some(Status::Kept | Status::Unconfirmed)).then(
                        || {
                            let fallback = json!({});
                            retained_notice(
                                if entry.verified {
                                    Some(&entry.row)
                                } else if entry.status == Some(Status::Unconfirmed) {
                                    Some(&fallback)
                                } else {
                                    None
                                },
                                unowned,
                                entry.status == Some(Status::Unconfirmed),
                            )
                        },
                    ),
                    restore: job
                        .archive
                        .as_ref()
                        .zip(job.source.as_ref())
                        .filter(|(a, t)| {
                            job.restore_supported
                                && a.matches(t)
                                && zerus_client_core::creation::canonical_uuid(&entry.machine)
                                && entry.status != Some(Status::Removed)
                        })
                        .map(|(a, t)| RestoreSource {
                            target: t.clone(),
                            machine: entry.machine.clone(),
                            archive_id: a.archive_id.clone(),
                            status: entry.status.unwrap_or(Status::Kept),
                            overflow: false,
                        }),
                });
            }
        }
        let busy = self.jobs.iter().any(|j| j.busy) || !self.restoring.is_empty();
        let cleaning = self.jobs.iter().any(|job| {
            job.busy
                && !job.source.as_ref().zip(job.archive.as_ref()).is_some_and(
                    |(target, archive)| {
                        self.restoring
                            .contains(&format!("{}\n{}", target.computer, archive.archive_id))
                    },
                )
        });
        let text = if cleaning {
            "Cleaning working folders…".into()
        } else if kept == 1 && unknown == 0 && removed == 0 {
            format!(
                "Folder kept: {}.",
                rows.first().map(|r| r.label.as_str()).unwrap_or("")
            )
        } else {
            // Only what the chip still holds: a finished removal has its own
            // notice, so a "0 removed" beside it would read as a contradiction.
            let parts: Vec<String> = [(removed, "removed"), (kept, "kept"), (unknown, "unknown")]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, word)| format!("{count} {word}"))
                .collect();
            if parts.is_empty() {
                "Working folders need review".into()
            } else {
                format!("Working folders: {}", parts.join("  "))
            }
        };
        let mut title = text.clone();
        if let Some(reason) = self.jobs.iter().find_map(|j| j.cleanup.retained_reason()) {
            title.push(' ');
            title.push_str(&reason);
        }
        if unknown > 0 {
            title.push_str(" An unknown result will not be retried.");
        }
        let overflow = self.overflow_kept.saturating_add(self.overflow_unknown);
        if overflow > 0 {
            title.push_str(&format!(
                " {overflow} additional folders need review in Projects > Worktrees."
            ));
        }
        View {
            revision: self.revision,
            rows,
            busy,
            cleaning,
            removed,
            kept,
            unknown,
            required,
            text,
            title,
        }
    }
}
fn changed(app: &AppHandle, model: &mut Model) {
    model.revision = model.revision.wrapping_add(1);
    let _ = app.emit("termination-cleanup-changed", ());
}

impl TerminationCleanup {
    pub(crate) fn observe(
        &self,
        app: &AppHandle,
        target: &Target,
        machine: &str,
        outcome: &Outcome,
        restore_supported: bool,
    ) -> Option<RetainedCapture> {
        let Outcome::Done {
            cleanup: Some(report),
            ..
        } = outcome
        else {
            return None;
        };
        let mut model = lock(&self.0.model);
        let result = model.retain(target, machine, report, restore_supported);
        if result.is_some() {
            changed(app, &mut model);
        }
        result
    }
}

fn restore_identity(source: &RestoreSource, value: &Value) -> bool {
    zerus_client_core::creation::canonical_uuid(&source.machine)
        && zerus_client_core::creation::canonical_uuid(&source.archive_id)
        && !source.target.run_id.is_empty()
        && !source.target.conversation_id.is_empty()
        && value["state"] == "archived"
        && value["name"] == source.target.name
        && value["run_id"] == source.target.run_id
        && value["conversation_id"] == source.target.conversation_id
        && value["archive_id"] == source.archive_id
}

/// Metadata only, through the existing bounded reader and physical-computer
/// guard. The existing action writer performs the actual archive restore.
pub(crate) fn restore_preflight(source: &RestoreSource, hgs: &Path) -> Result<(), String> {
    let mut inv =
        zerus_client_core::details::details_invocation(&source.target, Some(&source.archive_id));
    if let Some(index) = inv.args.iter().position(|a| a == "--after") {
        inv.args[index + 1] = i64::MAX.to_string();
    }
    worktrees::guard(&mut inv.args, &source.machine);
    match worktrees::run_guarded(hgs, &inv, bounded_run)? {
        Exit::Code {
            code: 0, stdout, ..
        } => {
            let value: Value = serde_json::from_slice(&stdout).map_err(|_| STATE_CHANGED)?;
            if restore_identity(source, &value) {
                Ok(())
            } else {
                Err(STATE_CHANGED.into())
            }
        }
        exit => match zerus_client_core::actions::classify(
            &source.target,
            &zerus_client_core::Action::Restore {
                archive_id: source.archive_id.clone(),
            },
            &inv,
            exit,
        ) {
            Outcome::Failed { detail } => Err(detail),
            _ => Err(STATE_CHANGED.into()),
        },
    }
}

#[tauri::command]
pub async fn termination_cleanup_restore(
    app: AppHandle,
    fleet: State<'_, crate::commands::AppState>,
    state: State<'_, TerminationCleanup>,
    actions: State<'_, crate::actions::Actions>,
    source: RestoreSource,
) -> Result<Outcome, String> {
    if crate::worktree_cleanup::machine(&fleet, &source.target.computer)? != source.machine {
        return Err(STATE_CHANGED.into());
    }
    let _read = state.0.read()?;
    let restore_key = format!("{}\n{}", source.target.computer, source.archive_id);
    let id = {
        let mut model = lock(&state.0.model);
        if model.restoring.contains(&restore_key) {
            return Err("busy".into());
        }
        let id = if let Some(job) = model.jobs.iter_mut().find(|j| {
            j.source.as_ref() == Some(&source.target)
                && j.archive
                    .as_ref()
                    .is_some_and(|a| a.archive_id == source.archive_id)
        }) {
            if job.busy {
                return Err("busy".into());
            }
            job.busy = true;
            Some(job.id.clone())
        } else {
            None
        };
        model.restoring.insert(restore_key.clone());
        changed(&app, &mut model);
        id
    };
    let result = crate::actions::restore_captured(&actions, source.clone()).await;
    let mut model = lock(&state.0.model);
    model.restoring.remove(&restore_key);
    if matches!(&result, Ok(Outcome::Done { .. })) {
        if let Some(id) = id {
            model.jobs.retain(|j| j.id != id);
        } else if source.overflow && source.status == Status::Unconfirmed {
            model.overflow_unknown = model.overflow_unknown.saturating_sub(1);
        } else if source.overflow {
            model.overflow_kept = model.overflow_kept.saturating_sub(1);
        }
    } else if let Some(job) = id.and_then(|id| model.jobs.iter_mut().find(|j| j.id == id)) {
        job.busy = false;
    }
    changed(&app, &mut model);
    result
}
impl Inner {
    fn drive(self: &Arc<Self>, app: AppHandle, id: String, mut step: Step) {
        let own = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            loop {
                match step {
                    Step::Review {
                        host,
                        common_dir,
                        worktree,
                        ..
                    } => {
                        let mut inv = review::review_invocation(
                            &ComputerId(host.clone()),
                            &common_dir,
                            &worktree,
                        );
                        // Every follow-up stays bound to the original owning computer.
                        let machine = {
                            let m = lock(&own.model);
                            let Some(j) = m.jobs.iter().find(|j| j.id == id) else {
                                return;
                            };
                            j.cleanup.entries[0].machine.clone()
                        };
                        worktrees::guard(&mut inv.args, &machine);
                        let result = worktrees::run_guarded(&default_hgs(), &inv, bounded_run)
                            .and_then(|exit| review::review(exit, &host, false))
                            .and_then(|value| {
                                if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 1024 * 1024) {
                                    Ok(value)
                                } else {
                                    Err("The folder review is too large. Inspect it in Projects > Worktrees.".into())
                                }
                            });
                        let mut m = lock(&own.model);
                        let Some(j) = m.jobs.iter_mut().find(|j| j.id == id) else {
                            return;
                        };
                        step = j.cleanup.reviewed(
                            &host,
                            &common_dir,
                            result.as_ref().map_err(Clone::clone),
                        );
                        j.phase = if matches!(step, Step::Remove { .. }) {
                            "removing"
                        } else {
                            "checking"
                        };
                        changed(&app, &mut m);
                    }
                    Step::Remove { host, removal, .. } => {
                        let request = uuid::Uuid::new_v4().to_string();
                        let mut inv = review::remove_invocation(
                            &ComputerId(host.clone()),
                            &removal,
                            &request,
                        );
                        let machine = {
                            let m = lock(&own.model);
                            let Some(j) = m.jobs.iter().find(|j| j.id == id) else {
                                return;
                            };
                            j.cleanup.entries[0].machine.clone()
                        };
                        worktrees::guard(&mut inv.args, &machine);
                        let result = match worktrees::run_guarded(&default_hgs(), &inv, bounded_run)
                        {
                            Ok(exit) => review::removed(exit, &host, &request, &removal),
                            Err(detail) => review::Removed::Refused {
                                reply: json!({"status":"kept"}),
                                detail,
                            },
                        };
                        let mut m = lock(&own.model);
                        let Some(j) = m.jobs.iter_mut().find(|j| j.id == id) else {
                            return;
                        };
                        step = j.cleanup.removed(&result);
                        changed(&app, &mut m);
                    }
                    Step::Idle | Step::Done => {
                        let mut m = lock(&own.model);
                        let Some(j) = m.jobs.iter_mut().find(|j| j.id == id) else {
                            return;
                        };
                        j.busy = false;
                        if j.cleanup
                            .entries
                            .iter()
                            .all(|e| e.status == Some(Status::Removed))
                        {
                            let labels = j
                                .cleanup
                                .entries
                                .iter()
                                .map(|e| leaf(e.row["path"].as_str().unwrap_or("")))
                                .collect::<Vec<_>>();
                            m.jobs.retain(|j| j.id != id);
                            for label in labels {
                                let _ = app.emit("termination-folder-removed", label);
                            }
                        }
                        changed(&app, &mut m);
                        break;
                    }
                }
            }
        });
    }
}
#[tauri::command]
pub fn termination_cleanup_view(state: State<'_, TerminationCleanup>) -> View {
    lock(&state.0.model).view()
}
#[tauri::command]
pub async fn termination_cleanup_offer(
    app: State<'_, crate::commands::AppState>,
    state: State<'_, TerminationCleanup>,
    settings: State<'_, crate::settings::Store>,
    target: Target,
) -> Result<Option<OfferView>, String> {
    if crate::actions::blocked() {
        return Ok(None);
    }
    let row = app
        .pump()
        .row_named(&target.computer, &target.name)
        .ok_or(STATE_CHANGED)?;
    if row.state != "running"
        || row.run_id.as_deref() != Some(&target.run_id)
        || row.conversation_id.as_deref() != Some(&target.conversation_id)
        || row.machine.is_empty()
    {
        return Err(STATE_CHANGED.into());
    }
    let _read = state.0.read()?;
    let call_target = target.clone();
    let machine = app
        .pump()
        .machine(&ComputerId(target.computer.clone()))
        .and_then(|m| m.machine_id)
        .filter(|id| zerus_client_core::creation::canonical_uuid(id))
        .ok_or(STATE_CHANGED)?;
    let capture = tauri::async_runtime::spawn_blocking(move || {
        let hgs = default_hgs();
        probe(&hgs, &call_target, &bounded_run)?;
        let value = inspection(&hgs, &call_target, &machine, &bounded_run)?;
        Capture::from_inspection(&call_target, &machine, &value)
    })
    .await
    .map_err(|e| e.to_string())??;
    let Some(capture) = capture else {
        return Ok(None);
    };
    let mut m = lock(&state.0.model);
    m.offers.retain(|o| o.at.elapsed() < OFFER_AGE);
    if m.offers.len() + m.jobs.len() + m.reserved >= LIMIT {
        return Err("Review the retained working folders before starting another cleanup.".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    m.offers.push_back(Offer {
        id: id.clone(),
        capture,
        at: Instant::now(),
    });
    Ok(Some(OfferView {
        id,
        checked: settings.0.get(PREF).as_bool().unwrap_or(false),
    }))
}
#[tauri::command]
pub fn termination_cleanup_cancel(state: State<'_, TerminationCleanup>, id: String) {
    lock(&state.0.model).offers.retain(|o| o.id != id);
}
#[tauri::command]
pub async fn termination_cleanup_confirm(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    actions: State<'_, crate::actions::Actions>,
    settings: State<'_, crate::settings::Store>,
    id: String,
    delay: Option<crate::action_delay::Reference>,
) -> Result<Outcome, String> {
    use tauri::Manager;
    if crate::actions::blocked() {
        return Err(crate::actions::REPLAY_NEEDS_HGS.into());
    }
    let capture = {
        let mut m = lock(&state.0.model);
        let index = m
            .offers
            .iter()
            .position(|o| o.id == id)
            .ok_or(STATE_CHANGED)?;
        let o = m.offers.remove(index).ok_or(STATE_CHANGED)?;
        if o.at.elapsed() >= OFFER_AGE {
            return Err(STATE_CHANGED.into());
        }
        let delayed = delay
            .as_ref()
            .map(|reference| {
                app.state::<crate::action_delay::Delays>().take(
                    reference,
                    &o.capture.target,
                    &zerus_client_core::Action::Terminate,
                )
            })
            .transpose()?;
        m.reserved += 1;
        (o.capture, delayed)
    };
    let (capture, delayed) = capture;
    // A remembered checkbox default never changes the action's admission.
    let _ = settings.0.set(PREF, Value::Bool(true));
    let outcome = if delayed.is_some() {
        crate::actions::dispatch_cleanup_reserved(&actions, capture.clone(), delayed).await
    } else {
        crate::actions::dispatch_cleanup(&actions, capture.clone()).await
    };
    let mut m = lock(&state.0.model);
    m.reserved -= 1;
    if let Ok(Outcome::Done { cleanup, receipt }) = &outcome {
        if let Some(entry) = capture.entry(cleanup.as_ref()) {
            let mut cleanup = Cleanup::after_termination(vec![entry]);
            let step = cleanup.step();
            m.jobs.push_back(Job {
                id: id.clone(),
                cleanup,
                busy: true,
                phase: "checking",
                manual_review: false,
                source: Some(capture.target.clone()),
                restore_supported: true,
                archive: Some(ArchiveIdentity {
                    version: 1,
                    archive_id: receipt["result_target"]["archive_id"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                    name: capture.target.name.clone(),
                    run_id: capture.target.run_id.clone(),
                    conversation_id: capture.target.conversation_id.clone(),
                    owner_id: capture.owner_id.clone(),
                    launch_owned: !capture.owner_id.is_empty(),
                }),
            });
            changed(&app, &mut m);
            drop(m);
            state.0.drive(app, id, step);
        }
    }
    outcome
}
#[tauri::command]
pub fn termination_cleanup_check(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
) -> Result<View, String> {
    let mut m = lock(&state.0.model);
    let j = m
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or(STATE_CHANGED)?;
    if j.busy || !j.cleanup.can_refresh() {
        return Err("An unknown result will not be retried.".into());
    }
    j.manual_review = true;
    j.cleanup.review_remaining();
    let step = j.cleanup.step();
    j.busy = true;
    j.phase = "checking";
    changed(&app, &mut m);
    let view = m.view();
    drop(m);
    state.0.drive(app, id, step);
    Ok(view)
}
#[tauri::command]
pub fn termination_cleanup_remove(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    revision: u64,
    mut consents: Consents,
) -> Result<View, String> {
    let mut m = lock(&state.0.model);
    if m.revision != revision || m.jobs.iter().any(|j| j.busy) || !m.restoring.is_empty() {
        return Err("Folder changed. Refresh its cleanup review.".into());
    }
    consents.merged_branches = false;
    let mut calls = Vec::new();
    for j in &mut m.jobs {
        if !j.manual_review {
            continue;
        }
        let step = j.cleanup.remove(consents);
        if matches!(step, Step::Remove { .. }) {
            j.busy = true;
            j.phase = "removing";
            j.manual_review = false;
            calls.push((j.id.clone(), step));
        }
    }
    changed(&app, &mut m);
    let view = m.view();
    drop(m);
    for (id, step) in calls {
        state.0.drive(app.clone(), id, step)
    }
    Ok(view)
}
#[tauri::command]
pub fn termination_cleanup_keep(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    revision: u64,
) -> Result<View, String> {
    let mut m = lock(&state.0.model);
    if m.revision != revision || m.jobs.iter().any(|j| j.busy) || !m.restoring.is_empty() {
        return Err("Cleanup changed or is still running. Review the recorded outcomes.".into());
    }
    m.jobs.clear();
    m.overflow_kept = 0;
    m.overflow_unknown = 0;
    changed(&app, &mut m);
    Ok(m.view())
}

/// Read-only inspection of an opaque recorded row; never accepts a local root
/// from the webview and never refreshes its removal fingerprint implicitly.
#[tauri::command]
pub async fn termination_cleanup_inspect(
    state: State<'_, TerminationCleanup>,
    id: String,
    file: String,
    offset: usize,
) -> Result<Value, String> {
    if crate::actions::blocked() {
        return Err(crate::actions::REPLAY_NEEDS_HGS.into());
    }
    if file.len() > 4096 || file.chars().any(char::is_control) || offset > 1_000_000 {
        return Err(STATE_CHANGED.into());
    }
    let _read = state.0.read()?;
    let (host, machine, path, common) = {
        let m = lock(&state.0.model);
        let j = m.jobs.iter().find(|j| j.id == id).ok_or(STATE_CHANGED)?;
        if j.busy {
            return Err("Cleanup is still running.".into());
        }
        let e = &j.cleanup.entries[0];
        (
            e.host.clone(),
            e.machine.clone(),
            e.row["path"].as_str().unwrap_or("").to_owned(),
            e.common_dir.clone(),
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::worktree_cleanup::inspect(&host, &machine, &path, &common, &file, offset)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerus_client_core::termination_cleanup::Entry;
    const MACHINE: &str = "11111111-1111-4111-8111-111111111111";
    const ARCHIVE: &str = "22222222-2222-4222-8222-222222222222";
    fn source() -> Target {
        Target {
            computer: "example-peer".into(),
            name: "claude/example/check".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
        }
    }
    fn report(status: &str) -> zerus_client_core::WorktreeCleanup {
        serde_json::from_value(json!({"status":status,"path":"/srv/example/work","common_dir":"/srv/example/.git",
            "reason":"private path /srv/example/secret",
            "archive":{"version":1,"archive_id":ARCHIVE,"name":source().name,"run_id":"run-1","conversation_id":"conversation-1","owner_id":"owner-1","launch_owned":true},
            "review":{"path":"/srv/example/work","kind":"linked","sessions":["private/session"],"changes":2}})).unwrap()
    }
    #[test]
    fn legacy_end_actions_complete_without_offering_guarded_restore() {
        use zerus_client_core::{Action, Exit};
        for action in [Action::Archive, Action::Terminate] {
            for probe in ["supported", "legacy", "timeout", "failed"] {
                let target = source();
                let actions = crate::actions::Actions::new(
                    std::env::temp_dir()
                        .join(format!("zerus-legacy-end-{}.json", uuid::Uuid::new_v4())),
                );
                let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let seen = Arc::clone(&count);
                let held = actions.clone();
                let original = target.clone();
                let expected = action.clone();
                let (outcome, supported) =
                    tauri::async_runtime::block_on(crate::actions::dispatch_end_with(
                        &actions,
                        target.clone(),
                        action.clone(),
                        MACHINE.into(),
                        move |_, inv| {
                            assert!(held
                                .begin(crate::actions::Lane::key(&original, &expected), &expected)
                                .is_none());
                            if inv.args.last().is_some_and(|s| s == "--help") {
                                return match probe {
                                    "timeout" => Exit::TimedOut,
                                    "failed" => Exit::Code {
                                        code: 1,
                                        stdout: Vec::new(),
                                        stderr: "unavailable".into(),
                                    },
                                    _ => Exit::Code {
                                        code: 0,
                                        stdout: if probe == "supported" {
                                            b"guarded-create-v1".to_vec()
                                        } else {
                                            b"hgs stable help".to_vec()
                                        },
                                        stderr: String::new(),
                                    },
                                };
                            }
                            assert_eq!(
                                zerus_client_core::worktrees::guarded(inv),
                                probe == "supported"
                            );
                            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Exit::Code {
                                code: 0,
                                stdout: Vec::new(),
                                stderr: format!(
                                    "ZERUS_WORKTREE_CLEANUP {}\n",
                                    serde_json::to_string(&report("kept")).unwrap()
                                ),
                            }
                        },
                    ))
                    .unwrap();
                assert_eq!(
                    count.load(std::sync::atomic::Ordering::Relaxed),
                    usize::from(matches!(probe, "supported" | "legacy"))
                );
                assert_eq!(supported, probe == "supported");
                if let Outcome::Done {
                    cleanup: Some(report),
                    ..
                } = outcome
                {
                    let mut model = Model::default();
                    let captured = model.retain(&target, MACHINE, &report, supported).unwrap();
                    assert_eq!(captured.restore.is_some(), supported);
                    assert_eq!(model.view().rows[0].restore.is_some(), supported);
                    assert!(
                        !model.jobs[0].cleanup.entries[0].offline,
                        "physical identity remains available for review"
                    );
                } else {
                    assert!(
                        !matches!(probe, "supported" | "legacy"),
                        "{probe}: {outcome:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn observed_archive_is_passive_exact_and_has_only_counted_notice_text() {
        let mut model = Model::default();
        let capture = model
            .retain(&source(), MACHINE, &report("kept"), true)
            .unwrap();
        assert!(capture.restore.is_some());
        assert!(!model.jobs[0].cleanup.busy());
        assert_eq!(
            capture.notice,
            "Working folder kept for review: 1 other session, 2 changed files."
        );
        let before = model.jobs[0].cleanup.clone();
        let view = model.view();
        assert_eq!(
            view.rows[0].notice.as_deref(),
            Some(capture.notice.as_str())
        );
        assert!(!view.busy);
        assert_eq!(model.jobs[0].cleanup, before);
        let repeated = model
            .retain(&source(), MACHINE, &report("unconfirmed"), true)
            .unwrap();
        assert_eq!(repeated.notice, capture.notice);
        assert_eq!(repeated.restore.unwrap().status, Status::Kept);
        assert_eq!(model.jobs.len(), 1, "admitted outcomes are immutable");
        assert_eq!(model.jobs[0].cleanup, before);
        let mut foreign = report("kept");
        foreign.archive.as_mut().unwrap().run_id = "another-run".into();
        assert!(Model::default()
            .retain(&source(), MACHINE, &foreign, true)
            .unwrap()
            .restore
            .is_none());
    }
    #[test]
    fn overflow_counts_outcomes_and_keeps_exact_restore_without_a_cleanup_row() {
        let mut model = Model::default();
        for _ in 0..LIMIT {
            model.jobs.push_back(job(Some(Status::Kept)));
        }
        let before = model.jobs[0].cleanup.clone();
        let capture = model
            .retain(&source(), MACHINE, &report("unconfirmed"), true)
            .unwrap();
        assert!(capture.id.is_none());
        assert!(capture.restore.unwrap().overflow);
        assert!(capture.notice.ends_with("an unknown result."));
        let view = model.view();
        assert_eq!(view.rows.len(), LIMIT);
        assert_eq!(view.kept, LIMIT);
        assert_eq!(view.unknown, 1);
        assert!(view
            .title
            .contains("1 additional folders need review in Projects > Worktrees."));
        assert_eq!(model.jobs[0].cleanup, before);
    }
    #[test]
    fn unowned_rows_use_the_prescribed_manual_review_state() {
        let mut report = report("kept");
        report.archive.as_mut().unwrap().launch_owned = false;
        let mut model = Model::default();
        model.retain(&source(), MACHINE, &report, true);
        let view = model.view();
        assert_eq!(view.rows[0].status, "review");
        assert_eq!(
            view.rows[0].reason,
            "Not created by Zerus; review the folder manually"
        );
        assert!(!view.rows[0].ready);
        assert!(view.rows[0]
            .notice
            .as_ref()
            .unwrap()
            .contains("not created by Zerus"));
    }
    #[test]
    fn restore_recheck_needs_the_exact_still_archived_source() {
        let source = RestoreSource {
            target: source(),
            machine: MACHINE.into(),
            archive_id: ARCHIVE.into(),
            status: Status::Unconfirmed,
            overflow: false,
        };
        let value = json!({"state":"archived","name":source.target.name,"run_id":"run-1","conversation_id":"conversation-1","archive_id":ARCHIVE});
        assert!(restore_identity(&source, &value));
        for key in ["state", "name", "run_id", "conversation_id", "archive_id"] {
            let mut changed = value.clone();
            changed[key] = json!("changed");
            assert!(!restore_identity(&source, &changed), "{key}");
        }
        assert!(!restore_identity(
            &RestoreSource {
                machine: "alias".into(),
                ..source
            },
            &value
        ));
    }

    #[test]
    fn restoring_disables_review_without_claiming_to_clean_the_folder() {
        let mut model = Model::default();
        model.retain(&source(), MACHINE, &report("kept"), true);
        model.jobs[0].busy = true;
        model
            .restoring
            .insert(format!("{}\n{}", source().computer, ARCHIVE));
        let view = model.view();
        assert!(view.busy);
        assert!(!view.cleaning);
        assert_eq!(view.text, "Folder kept: work.");
        assert!(!view.rows[0].can_check);
    }

    #[test]
    fn a_host_label_never_substitutes_for_physical_computer_identity() {
        let mut model = Model::default();
        let captured = model
            .retain(&source(), "example-peer", &report("kept"), true)
            .unwrap();
        assert!(captured.restore.is_none());
        assert!(model.jobs[0].cleanup.entries[0].offline);
        assert!(!model.view().rows[0].can_check);
        assert!(!model.view().rows[0].ready);
    }
    fn job(status: Option<Status>) -> Job {
        let mut entry = Entry::terminated(
            "example-peer",
            MACHINE,
            "/srv/example/work",
            "/srv/example/.git",
            "owner-1",
            None,
        )
        .unwrap();
        entry.status = status;
        Job {
            id: "job-1".into(),
            cleanup: Cleanup::after_termination(vec![entry]),
            busy: false,
            phase: "checking",
            manual_review: false,
            source: None,
            archive: None,
            restore_supported: false,
        }
    }
    #[test]
    fn unknown_projection_is_exact_and_overflow_does_not_invent_per_folder_outcomes() {
        let state = TerminationCleanup::default();
        lock(&state.0.model)
            .jobs
            .push_back(job(Some(Status::Unconfirmed)));
        assert!(state.unknown(MACHINE, "/srv/example/.git", "/srv/example/work"));
        assert!(!state.unknown(MACHINE, "/srv/example/.git", "/srv/example/other"));
        assert!(!state.unknown("other", "/srv/example/.git", "/srv/example/work"));
        lock(&state.0.model).overflow_unknown = 1;
        assert!(state.unknown_overflow());
        assert!(!state.unknown(MACHINE, "/srv/example/.git", "/srv/example/other"));
    }
    #[test]
    fn retained_status_is_passive_and_unknown_never_becomes_a_ready_target() {
        let mut m = Model::default();
        m.jobs.push_back(job(Some(Status::Unconfirmed)));
        let view = m.view();
        assert_eq!(view.unknown, 1);
        assert!(!view.rows[0].can_check);
        assert!(!view.rows[0].ready);
        assert!(view
            .title
            .contains("An unknown result will not be retried."));
        let original = m.jobs[0].cleanup.clone();
        let _ = m.view();
        assert_eq!(
            m.jobs[0].cleanup, original,
            "opening only reads recorded outcomes"
        );
        m.jobs.push_back(job(Some(Status::Kept)));
        let view = m.view();
        assert_eq!(view.kept, 1);
        assert!(view.rows[1].can_check);
        assert_eq!(view.text, "Working folders: 1 kept  1 unknown");
        m.jobs[1].busy = true;
        assert_eq!(m.view().text, "Cleaning working folders…");
    }
    #[test]
    fn a_background_kept_folder_is_not_a_manual_removal_without_check_again() {
        let mut j = job(Some(Status::Kept));
        j.cleanup.entries[0].verified = true;
        j.cleanup.entries[0].row =
            json!({"path":"/srv/example/work","removable":true,"fingerprint":"f"});
        let mut m = Model::default();
        m.jobs.push_back(j);
        assert!(!m.view().rows[0].ready);
        m.jobs[0].manual_review = true;
        // The UI-ready predicate shares the existing core validator.
        assert_eq!(m.view().rows[0].ready, ready(&m.jobs[0].cleanup.entries[0]));
    }
    #[test]
    fn current_inspection_reads_are_bounded_and_charge_until_their_guard_ends() {
        let inner = Arc::new(Inner::default());
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(inner.read().unwrap())
        }
        assert!(inner.read().is_err());
        held.pop();
        assert!(inner.read().is_ok());
        drop(held);
        assert!(lock(&inner.model).reads.is_empty());
    }
    #[test]
    fn old_or_oversized_help_never_admits_the_optional_scoped_action() {
        let target = Target {
            computer: "example-peer".into(),
            name: "codex/example/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        };
        for text in ["guarded-create-v1", "scoped native lifecycle action", ""] {
            assert!(probe(Path::new("fake-hgs"), &target, &|_, inv| {
                assert_eq!(inv.args, ["@example-peer", "--help"]);
                assert_eq!(inv.timeout.as_secs(), 5);
                Exit::Code {
                    code: 0,
                    stdout: text.as_bytes().to_vec(),
                    stderr: String::new(),
                }
            })
            .is_err());
        }
        assert!(probe(Path::new("fake-hgs"), &target, &|_, _| Exit::Code {
            code: 0,
            stdout: b"guarded-create-v1 scoped native lifecycle action".to_vec(),
            stderr: String::new()
        })
        .is_ok());
    }
}
