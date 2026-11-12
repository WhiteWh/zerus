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
#[cfg(any(target_os = "linux", target_os = "macos"))]
use zerus_client_core::run;
use zerus_client_core::{default_hgs, run_limited, ComputerId, Exit, Invocation, Outcome, Target};
use zerus_client_core::{
    termination_cleanup::{
        retained_notice, ArchiveIdentity, Capture, Cleanup, Consents, Entry, Status, Step,
        STATE_CHANGED,
    },
    worktree_review as review, worktrees,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod program_pipeline;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::desktop_service::termination_outcome::programs::pipeline as programs;

pub mod manual;
mod presentation;

const LIMIT: usize = review::CLEANUP_TARGETS;
const PREF: &str = "workspace/removeWorktreeAfterTermination";
const OFFER_AGE: Duration = Duration::from_secs(10 * 60);
#[derive(Default)]
pub struct TerminationCleanup(Arc<Inner>);
#[derive(Default)]
struct Inner {
    model: Mutex<Model>,
    reads: Arc<Mutex<BTreeSet<String>>>,
    pending: Arc<crate::desktop_service::termination_outcome::Pending>,
}
#[derive(Default)]
struct Model {
    offers: VecDeque<Offer>,
    manual: Option<manual::Offer>,
    jobs: VecDeque<Job>,
    reserved: usize,
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
    selected: Option<crate::desktop_service::termination_outcome::Selected>,
    at: Instant,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    programs: Option<programs::Offer>,
}
struct Job {
    id: String,
    cleanup: Cleanup,
    busy: bool,
    phase: &'static str,
    manual_review: bool,
    dismissed: bool,
    // Presentation retention, not consent/custody: expiry may release the
    // program pipeline while its incomplete/unknown result still needs review.
    program_review_pending: bool,
    program_retirement: Option<ProgramRetirement>,
    // Set once a result that is not a reviewable refusal released its
    // physically retired program (zerus-1y897.5). The row stays for lookup;
    // it never offers Check again, Force or a removal consent.
    lookup_only: bool,
    // Set only where the watcher's physical_retirement() proved the program
    // released (zerus-y7quu). A program that merely went missing, such as a
    // panicked Force offer task's dropped stop, is not physical proof.
    physically_released: bool,
    source: Option<Target>,
    archive: Option<ArchiveIdentity>,
    restore_supported: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    programs: Option<program_pipeline::ProgramJob>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    reservation: Option<program_pipeline::ProgramReservation>,
}
impl Job {
    fn has_launch_owner(&self) -> bool {
        self.cleanup
            .entries
            .iter()
            .all(|entry| entry.exit_cleanup && !entry.expected_owner_id.is_empty())
    }
    fn clearable(&self) -> bool {
        !self.busy
            && !self.unfinished_programs()
            && !self.program_review_pending
            && self
                .cleanup
                .entries
                .iter()
                .all(|entry| entry.status != Some(Status::Unconfirmed))
    }
    fn dismissible(&self) -> bool {
        !self.busy
            && !self.unfinished_programs()
            && (!self.program_review_pending || self.released_lookup_only())
    }
    /// zerus-yxopj: a lookup-only row whose program custody has physically
    /// retired has nothing left to wait for. Dismiss may drop it; it still
    /// never offers Check again, Force, a consent or a removal.
    fn released_lookup_only(&self) -> bool {
        self.lookup_only && self.physically_released && !self.has_programs()
    }

    fn unfinished_programs(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.programs
                .as_ref()
                .is_some_and(|program| !program.normal_refusal())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
    fn has_programs(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.programs.is_some()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
    fn reviewable_refusal(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.reservation.is_none()
                && self
                    .programs
                    .as_ref()
                    .is_some_and(program_pipeline::ProgramJob::reviewable_refusal)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
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

// Read resources must not own/re-enter the cleanup model: a retained native
// context may be dropped while that model lock is held on cancel/Keep/expiry.
pub(crate) struct ReadGuard(Arc<Mutex<BTreeSet<String>>>, String);
impl TerminationCleanup {
    pub(crate) fn read_terminal_identity(&self) -> Result<ReadGuard, String> {
        self.0.read()
    }
}
impl Drop for ReadGuard {
    fn drop(&mut self) {
        lock(&self.0).remove(&self.1);
    }
}
impl Inner {
    fn read(self: &Arc<Self>) -> Result<ReadGuard, String> {
        let mut reads = lock(&self.reads);
        if reads.len() >= 4 {
            return Err(
                "A folder inspection is already running. Try again after it finishes.".into(),
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        reads.insert(id.clone());
        Ok(ReadGuard(self.reads.clone(), id))
    }
}

#[derive(Serialize)]
pub struct OfferView {
    id: String,
    checked: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    programs: Option<Vec<programs::ProgramView>>,
}
#[derive(Clone, Serialize)]
pub(crate) struct ProgramView {
    pub pid: u32,
    pub label: String,
}
#[derive(Serialize)]
pub struct ForceView {
    confirmation: String,
    programs: Vec<ProgramView>,
    remaining_millis: u64,
    revision: u64,
}
/// Closed presentation evidence; never a permit, effect result or drain proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) enum ProgramRetirement {
    Expired,
    SourceUnavailable,
    HolderUnavailable,
    OfferUnavailable,
    StopUnconfirmed,
    CleanupSettled,
    CleanupUnconfirmed,
}
fn force_window(window: &tauri::Webview) -> Result<(), String> {
    if window.label() != crate::MAIN_WINDOW
        || (!window.window().is_visible().unwrap_or(false)
            && !window.window().is_minimized().unwrap_or(false))
    {
        return Err(STATE_CHANGED.into());
    }
    Ok(())
}
#[tauri::command]
pub async fn termination_program_force_offer(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
) -> Result<ForceView, String> {
    force_window(&window)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        program_pipeline::force_offer(state.0.clone(), app, id).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, state, id);
        Err(STATE_CHANGED.into())
    }
}
#[tauri::command]
pub async fn termination_program_force_confirm(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
    confirmation: String,
) -> Result<View, String> {
    force_window(&window)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        program_pipeline::force_confirm(state.0.clone(), app, id, confirmation).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, state, id, confirmation);
        Err(STATE_CHANGED.into())
    }
}
#[tauri::command]
pub fn termination_program_force_cancel(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
    confirmation: String,
) -> Result<View, String> {
    force_window(&window)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        program_pipeline::force_cancel(&state.0, &app, &id, &confirmation)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, state, id, confirmation);
        Err(STATE_CHANGED.into())
    }
}
#[derive(Serialize)]
pub struct Row {
    id: String,
    folder_key: String,
    can_dismiss: bool,
    can_remove_anyway: bool,
    sessions: Vec<String>,
    processes: Vec<ProgramView>,
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
    can_force: bool,
    force_revision: Option<u64>,
    program_retirement: Option<ProgramRetirement>,
    ready: bool,
    busy: bool,
    /// What the job is doing right now, separate from the entry's outcome.
    activity: Option<&'static str>,
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
    can_clear: bool,
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
/// The folders core cleanup would remove (zerus-k2ga: one predicate, so the
/// counted folders and the removed ones are the same).
fn ready(entry: &zerus_client_core::termination_cleanup::Entry) -> bool {
    entry.eligible()
}
impl Model {
    fn retain(
        &mut self,
        target: &Target,
        machine: &str,
        report: &zerus_client_core::WorktreeCleanup,
        restore_supported: bool,
    ) -> Option<RetainedCapture> {
        self.prune_dismissed();
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
            dismissed: false,
            program_review_pending: false,
            program_retirement: None,
            lookup_only: false,
            physically_released: false,
            source: Some(target.clone()),
            archive,
            restore_supported,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            programs: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            reservation: None,
        });
        Some(RetainedCapture {
            id: Some(id),
            restore,
            notice,
        })
    }
    fn view(&self) -> View {
        let mut rows = Vec::new();
        let mut required = Consents::default();
        for job in self.jobs.iter().filter(|job| !job.dismissed) {
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
                let status = if job.busy && !restoring && job.phase == "stopping" {
                    "stopping"
                } else {
                    match entry.status {
                        Some(Status::Removed) => "removed",
                        Some(Status::Unconfirmed) => "unknown",
                        Some(Status::Kept) => "kept",
                        None => {
                            if job.busy && !restoring {
                                job.phase
                            } else {
                                "ready"
                            }
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
                let r = if job.manual_review && job.has_launch_owner() {
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
                let presentation = job.cleanup.presentation_row(entry);
                let details=format!("Folder: {path}\nComputer: {}\n{}\n{}\nAfter removal: conversations and branches remain. Restoring an archived session may require recreating its working folder.", computer,reason,review::decision(&presentation).join("\n"));
                rows.push(Row {
                    id: job.id.clone(),
                    folder_key: presentation::folder_key(entry),
                    can_dismiss: self.can_dismiss(job),
                    can_remove_anyway: self.manual_entry(&job.id).is_ok(),
                    sessions: entry.row["sessions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .take(1024)
                        .map(str::to_owned)
                        .collect(),
                    processes: entry.row["processes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(64)
                        .filter_map(|p| {
                            Some(ProgramView {
                                pid: u32::try_from(p["pid"].as_u64()?).ok()?,
                                label: p["command"]
                                    .as_str()
                                    .or_else(|| p["label"].as_str())
                                    .unwrap_or("Program")
                                    .chars()
                                    .take(2000)
                                    .collect(),
                            })
                        })
                        .collect(),
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
                    can_force: {
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        {
                            !job.busy
                                && !job.lookup_only
                                && job.program_retirement.is_none()
                                && job
                                    .programs
                                    .as_ref()
                                    .is_some_and(program_pipeline::ProgramJob::can_force)
                        }
                        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                        {
                            false
                        }
                    },
                    force_revision: {
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        {
                            match &job.programs {
                                Some(program_pipeline::ProgramJob::Force { revision, .. })
                                    if !job.busy && job.program_retirement.is_none() =>
                                {
                                    Some(*revision)
                                }
                                _ => None,
                            }
                        }
                        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                        {
                            None
                        }
                    },
                    program_retirement: job.program_retirement,
                    can_check: self.can_dismiss(job)
                        && !self.unknown_folder(job)
                        && !job.busy
                        && !job.lookup_only
                        && (!job.has_programs() || job.reviewable_refusal())
                        && job.cleanup.can_refresh()
                        && zerus_client_core::creation::canonical_uuid(&entry.machine),
                    ready: self.can_dismiss(job)
                        && !self.unknown_folder(job)
                        && !job.busy
                        && !job.lookup_only
                        && !job.has_programs()
                        && job.manual_review
                        && job.has_launch_owner()
                        && ready(entry)
                        && zerus_client_core::creation::canonical_uuid(&entry.machine),
                    busy: job.busy,
                    activity: if restoring {
                        Some("restoring")
                    } else {
                        job.busy.then_some(job.phase)
                    },
                    source: job.source.clone(),
                    notice: matches!(entry.status, Some(Status::Kept | Status::Unconfirmed)).then(
                        || {
                            let fallback = json!({});
                            retained_notice(
                                if entry.verified {
                                    Some(&presentation)
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
        let counts = presentation::folder_counts(&rows);
        let removed = counts.0;
        let kept = self.overflow_kept.saturating_add(counts.1);
        let unknown = self.overflow_unknown.saturating_add(counts.2);
        // Running work has its own status bar item, so a held result always
        // names itself here, even while another folder is still running.
        let held = removed > 0 || kept > 0 || unknown > 0;
        let only_kept = (kept == 1 && unknown == 0 && removed == 0)
            .then(|| rows.iter().find(|r| matches!(r.status, "kept" | "review")))
            .flatten();
        let text = if cleaning && !held {
            "Cleaning working folders…".into()
        } else if let Some(row) = only_kept {
            format!("Folder kept: {}.", row.label)
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
        if let Some(reason) = self
            .jobs
            .iter()
            .filter(|j| !j.dismissed)
            .find_map(|j| j.cleanup.retained_reason())
        {
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
            can_clear: self.restoring.is_empty()
                && (self.overflow_kept > 0 || self.jobs.iter().any(|job| self.clearable(job))),
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
    fn drive(self: &Arc<Self>, app: AppHandle, id: String, mut step: Step, allow_remove: bool) {
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
                        if !allow_remove {
                            let mut m = lock(&own.model);
                            let Some(j) = m.jobs.iter_mut().find(|j| j.id == id) else {
                                return;
                            };
                            j.busy = false;
                            changed(&app, &mut m);
                            break;
                        }
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

fn offer_candidate(row: &zerus_client_core::ViewRow, target: &Target) -> Result<bool, String> {
    if row.name != target.name
        || row.state != "running"
        || target.run_id.is_empty()
        || row.run_id.as_deref() != Some(&target.run_id)
        || row.conversation_id.as_deref().unwrap_or("") != target.conversation_id
        || row.machine.is_empty()
    {
        return Err(STATE_CHANGED.into());
    }
    // The optional cleanup protocol requires a confirmed native conversation.
    // Its absence never blocks the ordinary run-bound Terminate action.
    Ok(!target.conversation_id.is_empty() && row.git_worktree != Some(false))
}
#[tauri::command]
pub async fn termination_cleanup_offer(
    handle: AppHandle,
    app: State<'_, crate::commands::AppState>,
    state: State<'_, TerminationCleanup>,
    actions: State<'_, crate::actions::Actions>,
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
    if !offer_candidate(&row, &target)? {
        return Ok(None);
    }
    let read = Arc::new(state.0.read()?);
    let machine = app
        .pump()
        .machine(&ComputerId(target.computer.clone()))
        .and_then(|m| m.machine_id)
        .filter(|id| zerus_client_core::creation::canonical_uuid(id))
        .ok_or(STATE_CHANGED)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if target.computer.is_empty() {
        let running = Arc::new(
            actions
                .begin(
                    crate::actions::Lane::key(&target, &zerus_client_core::Action::Terminate),
                    &zerus_client_core::Action::Terminate,
                )
                .ok_or("busy")?,
        );
        let pending = state.0.pending.clone();
        let native = if crate::desktop_service::mode() {
            None
        } else {
            use tauri::Manager;
            Some(
                handle
                    .try_state::<crate::desktop_ownership::NativeReady>()
                    .ok_or("worktree_programs_native_origin_unavailable")?
                    .program_source()?,
            )
        };
        let service =
            if crate::desktop_service::mode() {
                Some(app.desktop_service().ok_or_else(
                    crate::desktop_service::termination_outcome::missing_offer_service,
                )?)
            } else {
                None
            };
        let offer = tauri::async_runtime::spawn_blocking(move || {
            if let Some(service) = service {
                let (capture, selected) =
                    crate::desktop_service::termination_outcome::Selected::inspect(
                        service,
                        &target,
                        &machine,
                        Arc::new((read, running.clone())),
                    )?;
                capture
                    .map(|capture| programs::Offer::service(selected, capture, running, pending))
                    .transpose()
            } else {
                programs::Offer::native(
                    native.ok_or(STATE_CHANGED)?,
                    &target,
                    &machine,
                    running,
                    pending,
                    read,
                )
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        let Some(offer) = offer else {
            return Ok(None);
        };
        let program_list = offer.programs();
        let capture = offer.capture().clone();
        let id = uuid::Uuid::new_v4().to_string();
        let mut m = lock(&state.0.model);
        m.offers.retain(|o| o.at.elapsed() < OFFER_AGE);
        m.prune_dismissed();
        if m.offers.len() + m.jobs.len() + m.reserved >= LIMIT {
            return Err(
                "Review the retained working folders before starting another cleanup.".into(),
            );
        }
        m.offers.push_back(Offer {
            id: id.clone(),
            capture,
            selected: None,
            at: Instant::now(),
            programs: Some(offer),
        });
        drop(m);
        program_pipeline::expire_offer(&state.0, &id);
        return Ok(Some(OfferView {
            id,
            checked: settings.0.get(PREF).as_bool().unwrap_or(false),
            programs: Some(program_list),
        }));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (handle, actions);
    let service = if crate::desktop_service::mode() {
        Some(
            app.desktop_service()
                .ok_or_else(crate::desktop_service::termination_outcome::missing_offer_service)?,
        )
    } else {
        None
    };
    let (capture, selected) = tauri::async_runtime::spawn_blocking(move || {
        if let Some(service) = service {
            let (capture, selected) =
                crate::desktop_service::termination_outcome::Selected::inspect(
                    service, &target, &machine, read,
                )?;
            return Ok((capture, Some(selected)));
        }
        let _read = read;
        let hgs = default_hgs();
        probe(&hgs, &target, &bounded_run)?;
        let value = inspection(&hgs, &target, &machine, &bounded_run)?;
        Capture::from_inspection(&target, &machine, &value).map(|capture| (capture, None))
    })
    .await
    .map_err(|e| e.to_string())??;
    let Some(capture) = capture else {
        return Ok(None);
    };
    let mut m = lock(&state.0.model);
    m.offers.retain(|o| o.at.elapsed() < OFFER_AGE);
    m.prune_dismissed();
    if m.offers.len() + m.jobs.len() + m.reserved >= LIMIT {
        return Err("Review the retained working folders before starting another cleanup.".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    m.offers.push_back(Offer {
        id: id.clone(),
        capture,
        selected,
        at: Instant::now(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        programs: None,
    });
    Ok(Some(OfferView {
        id,
        checked: settings.0.get(PREF).as_bool().unwrap_or(false),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        programs: None,
    }))
}
#[tauri::command]
pub async fn termination_cleanup_cancel(
    state: State<'_, TerminationCleanup>,
    id: String,
) -> Result<(), String> {
    let offer = {
        let mut m = lock(&state.0.model);
        m.offers
            .iter()
            .position(|o| o.id == id)
            .and_then(|index| m.offers.remove(index))
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(programs) = offer.and_then(|offer| offer.programs) {
        return tauri::async_runtime::spawn_blocking(move || programs.close())
            .await
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    drop(offer);
    Ok(())
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
        (o, delayed)
    };
    let (offer, delayed) = capture;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(programs) = offer.programs {
        let _ = settings.0.set(PREF, Value::Bool(true));
        return program_pipeline::confirm(state.0.clone(), app, &actions, id, programs, delayed)
            .await;
    }
    let (capture, selected) = (offer.capture, offer.selected);
    // A remembered checkbox default never changes the action's admission.
    let _ = settings.0.set(PREF, Value::Bool(true));
    let outcome = match (crate::desktop_service::mode(), selected) {
        (true, Some(selected)) => {
            match crate::desktop_service::termination_outcome::Confirmation::new(
                id.clone(),
                capture.clone(),
                selected,
                state.0.pending.clone(),
            ) {
                Ok(native) => {
                    crate::actions::dispatch_cleanup_native(&actions, native, delayed).await
                }
                Err(error) => Err(error),
            }
        }
        (false, None) if delayed.is_some() => {
            crate::actions::dispatch_cleanup_reserved(&actions, capture.clone(), delayed).await
        }
        (false, None) => crate::actions::dispatch_cleanup(&actions, capture.clone()).await,
        _ => Err(STATE_CHANGED.into()),
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
                dismissed: false,
                program_review_pending: false,
                program_retirement: None,
                lookup_only: false,
                physically_released: false,
                source: Some(capture.target.clone()),
                restore_supported: true,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                programs: None,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                reservation: None,
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
            state.0.drive(app, id, step, true);
        }
    }
    outcome
}
#[tauri::command]
pub async fn termination_cleanup_check(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
) -> Result<View, String> {
    check_blocking(state.0.clone(), app, id, changed, |app, inner, id, step| {
        inner.drive(app.clone(), id, step, false)
    })
    .await
}
/// The Check again command body, for any app (zerus-wfynn). A known refusal's
/// retirement observes and closes its original source, and that close waits
/// for the holder to end, bounded only by the owner's retention deadline.
/// Tauri runs a synchronous command inline on the GTK main thread, so the
/// whole check runs on a blocking task instead; the row is reserved and busy
/// meanwhile, and its guards, reservation and single review are unchanged.
async fn check_blocking<A: Send + 'static>(
    inner: Arc<Inner>,
    app: A,
    id: String,
    emit: fn(&A, &mut Model),
    run_review: fn(&A, Arc<Inner>, String, Step),
) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app = &app;
        check_with_runtime(
            &inner,
            id,
            |model| emit(app, model),
            |inner, id, step| run_review(app, inner, id, step),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}
fn check_with_runtime(
    inner: &Arc<Inner>,
    id: String,
    emit: impl Fn(&mut Model) + Copy,
    run_review: impl Fn(Arc<Inner>, String, Step) + Copy,
) -> Result<View, String> {
    let mut m = lock(&inner.model);
    if m.jobs
        .iter()
        .find(|j| j.id == id)
        .is_some_and(|j| j.dismissed || j.lookup_only || m.unknown_folder(j))
    {
        return Err("An unknown result will not be retried.".into());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let revision = m.revision.wrapping_add(1);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let (step, program, reservation) = {
        let j = m
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or(STATE_CHANGED)?;
        if j.busy || !j.cleanup.can_refresh() {
            return Err("An unknown result will not be retried.".into());
        }
        let (program, reservation) = if j.has_programs() {
            if !j.reviewable_refusal() {
                return Err("An unknown result will not be retried.".into());
            }
            let reservation = match j.reservation.clone() {
                Some(reservation) if reservation.revision == revision => reservation,
                Some(_) => return Err(STATE_CHANGED.into()),
                None => {
                    let (request_id, binding, capture) =
                        j.programs.as_ref().expect("retained program").identity();
                    program_pipeline::ProgramReservation {
                        revision,
                        request_id,
                        binding,
                        capture,
                    }
                }
            };
            j.reservation = Some(reservation.clone());
            let program = j.programs.take();
            (program, Some(reservation))
        } else {
            (None, None)
        };
        j.manual_review = false;
        j.cleanup.review_remaining();
        let step = j.cleanup.step();
        j.busy = true;
        j.phase = "checking";
        (step, program, reservation)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let (step, program, reservation): (_, Option<()>, Option<()>) = {
        let j = m
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or(STATE_CHANGED)?;
        if j.busy || !j.cleanup.can_refresh() {
            return Err("An unknown result will not be retried.".into());
        }
        j.manual_review = false;
        j.cleanup.review_remaining();
        let step = j.cleanup.step();
        j.busy = true;
        j.phase = "checking";
        (step, None, None)
    };
    emit(&mut m);
    drop(m);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(program) = program {
        let reservation = reservation.expect("program reservation");
        let retired = program.retirement();
        let (start_review, view) = {
            let mut m = lock(&inner.model);
            let j = m
                .jobs
                .iter_mut()
                .find(|j| j.id == id)
                .ok_or(STATE_CHANGED)?;
            if !program.matches(&reservation)
                || j.reservation.as_ref() != Some(&reservation)
                || j.programs.is_some()
            {
                if j.reservation.as_ref() == Some(&reservation) && j.programs.is_none() {
                    j.programs = Some(program);
                    j.busy = false;
                    j.program_review_pending = true;
                    emit(&mut m);
                }
                return Err(STATE_CHANGED.into());
            }
            let start_review = match retired {
                run::Retirement::Retired => {
                    j.programs = None;
                    j.reservation = None;
                    j.program_review_pending = false;
                    j.manual_review = true;
                    true
                }
                run::Retirement::Pending => {
                    j.programs = Some(program);
                    j.program_review_pending = true;
                    false
                }
                run::Retirement::Failed => {
                    j.programs = Some(program);
                    j.busy = false;
                    j.program_review_pending = true;
                    false
                }
            };
            emit(&mut m);
            let view = m.view();
            (start_review, view)
        };
        if start_review {
            run_review(inner.clone(), id, step);
        }
        return Ok(view);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (program, reservation);
    let mut m = lock(&inner.model);
    let j = m
        .jobs
        .iter_mut()
        .find(|j| j.id == id)
        .ok_or(STATE_CHANGED)?;
    j.manual_review = true;
    let view = m.view();
    drop(m);
    run_review(inner.clone(), id, step);
    Ok(view)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_refused_check(stopped: programs::Stopped) -> serde_json::Value {
    assert!(stopped.reviewable_refusal());
    let mut entry = Entry::terminated(
        "studio",
        "studio",
        "/home/example/repo-work-1",
        "/home/example/repo/.git",
        "owner-1",
        None,
    )
    .unwrap();
    entry.machine = "11111111-1111-4111-8111-111111111111".into();
    // Online: Check again really reviews the folder (zerus-1y897.3).
    entry.offline = false;
    entry.status = Some(Status::Kept);
    let state = TerminationCleanup::default();
    // The program path's own job, published through production record_stop
    // (zerus-1y897.3): a known refusal must leave a retained cleanup.
    let mut job = Job {
        id: "job".into(),
        cleanup: Cleanup::after_termination(vec![entry]),
        busy: true,
        phase: "stopping",
        manual_review: false,
        dismissed: false,
        program_review_pending: true,
        program_retirement: None,
        lookup_only: false,
        physically_released: false,
        source: None,
        archive: None,
        restore_supported: true,
        programs: None,
        reservation: None,
    };
    assert!(program_pipeline::record_stop(&mut job, Ok(stopped)));
    lock(&state.0.model).jobs.push_back(job);
    let projection = lock(&state.0.model).view();
    assert!(projection.rows[0].can_check);
    assert!(!projection.rows[0].ready);
    assert!(!projection.rows[0].can_force);
    let serialized = serde_json::to_value(&projection).unwrap();
    let checked = check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |inner, _, mut step| loop {
            match step {
                Step::Review {
                    host, common_dir, ..
                } => {
                    let mut model = lock(&inner.model);
                    let job = model.jobs.iter_mut().find(|job| job.id == "job").unwrap();
                    // A fresh review that finds the folder ready to remove.
                    let review = json!({"state": "ok", "common_dir": "/home/example/repo/.git",
                        "worktrees": [{"kind": "linked", "verdict": "ready", "path": "/home/example/repo-work-1",
                        "fingerprint": "f1", "ownership_id": "owner-1", "zerus_owned": true,
                        "archive_restore_ready": true, "reasons": [], "changes": 0, "untracked": 0,
                        "disk_complete": true, "ignored_complete": true, "ignored_count": 0,
                        "integration": {"kind": "ancestor", "base": "main"}, "merged": true}]});
                    step = job.cleanup.reviewed(&host, &common_dir, Ok(&review));
                }
                Step::Remove { .. } => panic!("Check again must not remove"),
                Step::Idle | Step::Done => {
                    lock(&inner.model).jobs[0].busy = false;
                    break;
                }
            }
        },
    )
    .unwrap();
    assert!(checked.rows[0].busy);
    assert!(!checked.rows[0].can_force);
    let settled = lock(&state.0.model).view();
    assert!(!settled.busy);
    // No automatic removal is pending and the explicit one stays available.
    let mut cleanup = lock(&state.0.model).jobs[0].cleanup.clone();
    assert!(!cleanup.busy(), "Check again left the Core cleanup running");
    assert!(
        matches!(cleanup.remove(Default::default()), Step::Remove { .. }),
        "explicit removal must stay available after the fresh review"
    );
    assert!(settled.rows[0].can_check);
    // The ready folder is offered for the user's explicit Remove only.
    assert!(settled.rows[0].ready);
    assert!(!settled.rows[0].can_force);
    // Projection only (zerus-1y897.5): on this reviewed row the lookup-only
    // flag alone withholds Check again and the Remove consent.
    lock(&state.0.model).jobs[0].lookup_only = true;
    let lookup = lock(&state.0.model).view();
    assert!(!lookup.rows[0].can_check);
    assert!(!lookup.rows[0].ready);
    serialized
}

/// zerus-1y897.5: a real incomplete stop whose source is gone, published by
/// production record_stop and released by the production watcher pass. With
/// `force`, a Force offer over it was still open when the source went away.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_expired_release(stopped: programs::Stopped, force: bool) {
    assert!(stopped.can_force());
    assert!(!stopped.reviewable_refusal());
    assert!(!stopped.current());
    // zerus-yxopj: the Force variant's folder is not Zerus-owned, so only the
    // lookup-only guard keeps "Remove anyway" from its released row.
    let owner = if force { "" } else { "owner-1" };
    let mut entry = Entry::terminated(
        "studio",
        "studio",
        "/home/example/repo-work-1",
        "/home/example/repo/.git",
        owner,
        None,
    )
    .unwrap();
    entry.machine = "11111111-1111-4111-8111-111111111111".into();
    entry.offline = false;
    let state = TerminationCleanup::default();
    let mut job = Job {
        id: "job".into(),
        cleanup: Cleanup::after_termination(vec![entry]),
        busy: true,
        phase: "stopping",
        manual_review: false,
        dismissed: false,
        program_review_pending: true,
        program_retirement: None,
        lookup_only: false,
        physically_released: false,
        source: None,
        archive: None,
        restore_supported: true,
        programs: None,
        reservation: None,
    };
    assert!(program_pipeline::record_stop(&mut job, Ok(stopped)));
    if force {
        let Some(program_pipeline::ProgramJob::Stopped(stopped)) = job.programs.take() else {
            panic!("published stop")
        };
        job.programs = Some(program_pipeline::ProgramJob::Force {
            id: "force".into(),
            revision: 1,
            offer: Box::new(programs::fixture_force(*stopped)),
        });
    } else {
        // Projection only: the flag alone also withholds a usable Force.
        assert!(job.programs.as_ref().unwrap().can_force());
        lock(&state.0.model).jobs.push_back(job);
        assert!(lock(&state.0.model).view().rows[0].can_force);
        lock(&state.0.model).jobs[0].lookup_only = true;
        assert!(!lock(&state.0.model).view().rows[0].can_force);
        job = lock(&state.0.model).jobs.pop_front().unwrap();
        job.lookup_only = false;
    }
    lock(&state.0.model).jobs.push_back(job);
    // zerus-yxopj: while its program is held, the row cannot be dismissed.
    assert!(!lock(&state.0.model).view().rows[0].can_dismiss);
    assert!(lock(&state.0.model).dismiss("job").is_err());
    let emitted = std::cell::Cell::new(0);
    assert!(matches!(
        program_pipeline::watch_once(&state.0, "job", |_| emitted.set(emitted.get() + 1)),
        program_pipeline::Watch::Stop
    ));
    assert_eq!(emitted.get(), 1);
    // Behaviour first: the native admission refuses Check again.
    assert!(check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |_, _, _| panic!("a released unrefused result was reviewed"),
    )
    .is_err());
    {
        let model = lock(&state.0.model);
        let job = &model.jobs[0];
        assert!(job.programs.is_none(), "the retired program was kept");
        assert!(job.lookup_only);
        assert!(job.physically_released);
        assert!(!job.busy);
        assert!(job.program_review_pending);
        assert_eq!(
            job.program_retirement,
            Some(ProgramRetirement::SourceUnavailable)
        );
        assert!(job
            .cleanup
            .entries
            .iter()
            .all(|entry| entry.status == Some(Status::Kept)));
        let view = model.view();
        let row = &view.rows[0];
        assert!(!row.can_check);
        assert!(!row.ready);
        assert!(!row.can_force);
        assert_eq!(row.force_revision, None);
        assert!(
            row.can_dismiss,
            "a released lookup-only row could not be dismissed"
        );
        assert!(!row.can_remove_anyway);
        assert!(
            model.manual_entry("job").is_err(),
            "a released lookup-only row offered Remove anyway"
        );
        assert!(!job.manual_review, "a released row reached Remove");
        assert!(!view.can_clear);
    }
    // Dismiss only drops the settled row; nothing else is granted.
    let mut model = lock(&state.0.model);
    model.dismiss("job").unwrap();
    assert!(model.jobs.is_empty(), "the dismissed row stayed");
}

/// The program path's own job after production record_stop published a real
/// incomplete stop whose source is gone.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn published_incomplete(stopped: programs::Stopped) -> Job {
    assert!(stopped.can_force());
    assert!(!stopped.current());
    let mut job = stopping_job();
    assert!(program_pipeline::record_stop(&mut job, Ok(stopped)));
    job
}

/// The program path's own job while its original stop runs.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn stopping_job() -> Job {
    let mut entry = Entry::terminated(
        "studio",
        "studio",
        "/home/example/repo-work-1",
        "/home/example/repo/.git",
        "owner-1",
        None,
    )
    .unwrap();
    entry.machine = "11111111-1111-4111-8111-111111111111".into();
    entry.offline = false;
    Job {
        id: "job".into(),
        cleanup: Cleanup::after_termination(vec![entry]),
        busy: true,
        phase: "stopping",
        manual_review: false,
        dismissed: false,
        program_review_pending: true,
        program_retirement: None,
        lookup_only: false,
        physically_released: false,
        source: None,
        archive: None,
        restore_supported: true,
        programs: None,
        reservation: None,
    }
}

/// zerus-1y897.7: a stale Force stop click after the source went away. The
/// refused offer hands back the SAME stop with its custody; the native Check
/// admission stays closed while it is held, and only the watcher's physical
/// retirement releases it. `lost` is a blocking offer that never returned.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_refused_offer(stopped: programs::Stopped, lost: programs::Stopped) {
    let check = |state: &TerminationCleanup| {
        check_with_runtime(
            &state.0,
            "job".into(),
            |model| model.revision = model.revision.wrapping_add(1),
            |_, _, _| panic!("an unrefused stop was reviewed"),
        )
    };
    let state = TerminationCleanup::default();
    let mut job = published_incomplete(stopped);
    let (request_id, _, _) = job.programs.as_ref().unwrap().identity();
    let taken = program_pipeline::take_force(&mut job).unwrap();
    assert!(job.busy);
    let offered = taken.offer_force();
    assert!(program_pipeline::record_offer(&mut job, 1, Ok(offered)).is_err());
    assert_eq!(
        job.programs.as_ref().map(|program| program.identity().0),
        Some(request_id),
        "the refused offer released the stop"
    );
    assert!(!job.busy);
    assert!(!job.lookup_only);
    assert_eq!(
        job.program_retirement,
        Some(ProgramRetirement::OfferUnavailable)
    );
    lock(&state.0.model).jobs.push_back(job);
    assert!(check(&state).is_err());
    assert!(lock(&state.0.model).jobs[0].programs.is_some());
    assert!(matches!(
        program_pipeline::watch_once(&state.0, "job", |_| {}),
        program_pipeline::Watch::Stop
    ));
    assert!(check(&state).is_err());
    {
        let model = lock(&state.0.model);
        assert!(
            model.jobs[0].programs.is_none(),
            "the retired stop was kept"
        );
        assert!(model.jobs[0].lookup_only);
        assert!(
            model.jobs[0].physically_released,
            "the watcher's release was not physical"
        );
        assert!(!model.jobs[0].busy);
    }

    // zerus-y7quu: the real force_offer meets a real JoinError. Its blocking
    // offer task panics and drops the stop inside the task; nothing proves a
    // physical release, so the row stays lookup-only and undismissable.
    let state = TerminationCleanup::default();
    lock(&state.0.model)
        .jobs
        .push_back(published_incomplete(lost));
    program_pipeline::fixture_lose_offer("job");
    let inner = state.0.clone();
    let offered = std::thread::spawn(move || {
        tauri::async_runtime::block_on(program_pipeline::force_offer(
            inner,
            TestApp::default(),
            "job".into(),
        ))
    })
    .join()
    .unwrap();
    assert!(offered.is_err(), "a lost offer task offered Force");
    assert!(check(&state).is_err());
    let mut model = lock(&state.0.model);
    assert!(!model.jobs[0].busy, "a lost offer left the row busy");
    assert!(model.jobs[0].lookup_only);
    assert!(
        !model.jobs[0].physically_released,
        "a lost offer claimed physical release"
    );
    assert!(model.jobs[0].programs.is_none());
    {
        let view = model.view();
        let row = &view.rows[0];
        assert!(!row.can_check && !row.ready && !row.can_force);
        assert!(!row.can_remove_anyway);
        assert!(
            !row.can_dismiss,
            "a lost offer's row became dismissible without physical proof"
        );
    }
    assert!(
        model.dismiss("job").is_err(),
        "a lost offer's row was dismissed"
    );
}

/// zerus-1y897.8: an unconfirmed stop is lookup-only and keeps whatever
/// custody its producer handed back until the watcher's physical retirement:
/// a stale Force confirmation (`offered`) and stops whose source went away
/// before their phase settled (`incomplete`, `refused`).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_unconfirmed_stops(
    offered: programs::Stopped,
    incomplete: programs::Stopped,
    refused: programs::Stopped,
) {
    fn check(state: &TerminationCleanup) -> Result<View, String> {
        check_with_runtime(
            &state.0,
            "job".into(),
            |model| model.revision = model.revision.wrapping_add(1),
            |_, _, _| panic!("an unconfirmed stop was reviewed"),
        )
    }
    fn assert_held(state: &TerminationCleanup, request_id: String, live: bool) {
        assert_eq!(
            lock(&state.0.model).jobs[0]
                .programs
                .as_ref()
                .map(|program| program.identity().0),
            Some(request_id),
            "the unconfirmed stop was released"
        );
        assert!(check(state).is_err());
        {
            let mut model = lock(&state.0.model);
            let job = &mut model.jobs[0];
            assert!(live, "the held stop is not watched");
            assert!(
                program_pipeline::take_force(job).is_err(),
                "an unconfirmed stop offered Force"
            );
            assert!(job.lookup_only);
            assert!(!job.busy);
            assert_eq!(
                job.program_retirement,
                Some(ProgramRetirement::StopUnconfirmed)
            );
        }
        // zerus-yxopj: a held stop, even a refusal, cannot be dismissed.
        assert!(!lock(&state.0.model).view().rows[0].can_dismiss);
        assert!(
            lock(&state.0.model).dismiss("job").is_err(),
            "a held stop was dismissed"
        );
        // The passive watcher's own passes; only physical retirement releases.
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match program_pipeline::watch_once(&state.0, "job", |_| {}) {
                program_pipeline::Watch::Continue => {
                    assert!(
                        std::time::Instant::now() < until,
                        "the held stop never retired"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                program_pipeline::Watch::Stop => break,
                program_pipeline::Watch::Review(_) => panic!("an unconfirmed stop was reviewed"),
            }
        }
        assert!(check(state).is_err());
        let model = lock(&state.0.model);
        assert!(
            model.jobs[0].programs.is_none(),
            "the retired stop was kept"
        );
        assert!(model.jobs[0].lookup_only);
        assert!(model.jobs[0].physically_released);
        let row = &model.view().rows[0];
        assert!(!row.can_check && !row.ready && !row.can_force);
        assert!(
            row.can_dismiss,
            "a released lookup-only row could not be dismissed"
        );
        drop(model);
        let mut model = lock(&state.0.model);
        model.dismiss("job").unwrap();
        assert!(model.jobs.is_empty(), "the dismissed row stayed");
    }

    let state = TerminationCleanup::default();
    let mut job = published_incomplete(offered);
    let (request_id, _, _) = job.programs.as_ref().unwrap().identity();
    let Some(program_pipeline::ProgramJob::Stopped(stopped)) = job.programs.take() else {
        panic!("published stop")
    };
    // The Force dialog that was still open when the source went away.
    job.programs = Some(program_pipeline::ProgramJob::Force {
        id: "force".into(),
        revision: 1,
        offer: Box::new(programs::fixture_force(*stopped)),
    });
    let offer = program_pipeline::take_confirm(&mut job, "force").unwrap();
    assert!(job.busy);
    let live = program_pipeline::record_stop(&mut job, program_pipeline::confirm_force(offer));
    lock(&state.0.model).jobs.push_back(job);
    assert_held(&state, request_id, live);

    for stopped in [incomplete, refused] {
        let state = TerminationCleanup::default();
        let mut job = stopping_job();
        let (request_id, _, _) = stopped.identity();
        let live = program_pipeline::record_stop(&mut job, program_pipeline::settle(stopped));
        lock(&state.0.model).jobs.push_back(job);
        assert_held(&state, request_id, live);
    }

    // A refused original stop hands nothing back: only the lookup-only row.
    let state = TerminationCleanup::default();
    let mut job = stopping_job();
    assert!(!program_pipeline::record_stop(
        &mut job,
        Err(program_pipeline::Unconfirmed { held: None }),
    ));
    lock(&state.0.model).jobs.push_back(job);
    assert!(check(&state).is_err());
    let model = lock(&state.0.model);
    assert!(model.jobs[0].lookup_only);
    let row = &model.view().rows[0];
    assert!(!row.can_check && !row.ready && !row.can_force);
}

/// zerus-1y897.9: a refused original normal stop, a refused cleanup of a
/// Complete stop and a removal whose source went away keep their custody,
/// lookup-only and watched, until the watcher's physical retirement.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_held_custody(
    original: programs::Continuation,
    complete: programs::Stopped,
    removal: programs::Removal,
) {
    fn check(state: &TerminationCleanup) -> Result<View, String> {
        check_with_runtime(
            &state.0,
            "job".into(),
            |model| model.revision = model.revision.wrapping_add(1),
            |_, _, _| panic!("held custody was reviewed"),
        )
    }
    fn held(state: &TerminationCleanup, live: bool, retirement: ProgramRetirement) {
        assert!(check(state).is_err());
        let model = lock(&state.0.model);
        let job = &model.jobs[0];
        assert!(live, "the held custody is not watched");
        assert!(job.lookup_only);
        assert!(!job.busy);
        assert_eq!(job.program_retirement, Some(retirement));
        let row = &model.view().rows[0];
        assert!(!row.can_check && !row.ready && !row.can_force && !row.can_dismiss);
    }
    // The passive watcher's own passes; only physical retirement releases.
    fn released(state: &TerminationCleanup) {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match program_pipeline::watch_once(&state.0, "job", |_| {}) {
                program_pipeline::Watch::Continue => {
                    assert!(
                        std::time::Instant::now() < until,
                        "the held custody never retired"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                program_pipeline::Watch::Stop => break,
                program_pipeline::Watch::Review(_) => panic!("held custody was reviewed"),
            }
        }
        assert!(check(state).is_err());
        let model = lock(&state.0.model);
        assert!(
            model.jobs[0].programs.is_none(),
            "the retired custody was kept"
        );
        assert!(model.jobs[0].lookup_only);
        // zerus-yxopj: the released row may be dismissed; an unknown removal
        // outcome stays recorded for its folder and is never replayed.
        assert!(
            model.view().rows[0].can_dismiss,
            "a released lookup-only row could not be dismissed"
        );
        let unknown = model.jobs[0]
            .cleanup
            .entries
            .iter()
            .any(|entry| entry.status == Some(Status::Unconfirmed));
        drop(model);
        let mut model = lock(&state.0.model);
        model.dismiss("job").unwrap();
        if unknown {
            assert!(model.jobs[0].dismissed);
            assert!(
                model.unknown_folder(&model.jobs[0]),
                "the unknown removal outcome was dropped"
            );
            assert!(model.view().rows.is_empty());
        } else {
            assert!(model.jobs.is_empty(), "the dismissed row stayed");
        }
        drop(model);
        assert!(check(state).is_err());
    }

    // Continuation::normal refused: the source went away before the stop.
    let state = TerminationCleanup::default();
    let mut job = stopping_job();
    let live = program_pipeline::record_stop(
        &mut job,
        program_pipeline::stop_original(Box::new(original)),
    );
    assert!(
        matches!(
            job.programs,
            Some(program_pipeline::ProgramJob::Original(_))
        ),
        "the refused original was released"
    );
    lock(&state.0.model).jobs.push_back(job);
    held(&state, live, ProgramRetirement::StopUnconfirmed);
    released(&state);

    // A Complete stop whose cleanup is refused, then a removal that never
    // settled: each source went away first.
    let (complete_id, _, _) = complete.identity();
    let (removal_id, _, _) = removal.identity();
    for (request_id, removed) in [
        (
            complete_id,
            program_pipeline::run_removal(Box::new(complete)),
        ),
        (
            removal_id,
            program_pipeline::settle_removal(Box::new(removal)),
        ),
    ] {
        let state = TerminationCleanup::default();
        let mut job = stopping_job();
        job.phase = "removing";
        let (gone, live) = program_pipeline::record_removal(&mut job, removed);
        assert_eq!(
            job.programs.as_ref().map(|program| program.identity().0),
            Some(request_id),
            "the refused removal released its custody"
        );
        assert!(!gone);
        assert!(job
            .cleanup
            .entries
            .iter()
            .all(|entry| entry.status == Some(Status::Unconfirmed)));
        lock(&state.0.model).jobs.push_back(job);
        held(&state, live, ProgramRetirement::CleanupUnconfirmed);
        released(&state);
    }
}

/// A recording app for the program pipeline's real producers.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[derive(Clone, Default)]
struct TestApp(Arc<Mutex<Vec<String>>>);
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl TestApp {
    fn events(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl program_pipeline::PipelineApp for TestApp {
    fn changed(&self, model: &mut Model) {
        model.revision = model.revision.wrapping_add(1);
        lock(&self.0).push("changed".into());
    }
    fn drive(&self, inner: &Arc<Inner>, id: String, _: Step) {
        lock(&self.0).push(format!("drive {id}"));
        if let Some(job) = lock(&inner.model).jobs.iter_mut().find(|job| job.id == id) {
            job.busy = false;
        }
    }
    fn removed(&self, label: String) {
        lock(&self.0).push(format!("removed {label}"));
    }
}

/// Waits for the real passive watcher to settle the only job.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn watcher_released(state: &TerminationCleanup, failure: &str) {
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        {
            let model = lock(&state.0.model);
            if model.jobs[0].programs.is_none() && !model.jobs[0].busy {
                return;
            }
        }
        assert!(std::time::Instant::now() < until, "{failure}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits for the real passive watcher to leave the only job as a lookup-only
/// row and requires the physical retirement that makes that a release
/// (zerus-y4hv7). A lost offer task drops the stop inside the task and also
/// leaves a lookup-only row without a program, but it never sets the marker.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn watcher_released_lookup_only(state: &TerminationCleanup, failure: &str) {
    watcher_released(state, failure);
    let model = lock(&state.0.model);
    assert!(model.jobs[0].lookup_only, "{failure}");
    assert!(
        model.jobs[0].physically_released,
        "{failure}: a lookup-only row was accepted without physical proof"
    );
}

/// zerus-fojet: production publish records a real refusal and starts the real
/// passive watcher, which retires it; Check again then reviews the retained
/// cleanup and never removes.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_published_refusal(stopped: programs::Stopped) {
    assert!(stopped.reviewable_refusal());
    assert!(!stopped.current());
    let app = TestApp::default();
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::publish(state.0.clone(), app.clone(), "job".into(), Ok(stopped));
    assert_eq!(
        app.events().first().map(String::as_str),
        Some("changed"),
        "publish recorded nothing"
    );
    assert!(
        !lock(&state.0.model).jobs[0].cleanup.busy(),
        "a known refusal kept the background cleanup"
    );
    watcher_released(&state, "publish did not start the passive watcher");
    {
        let model = lock(&state.0.model);
        let job = &model.jobs[0];
        assert!(!job.program_review_pending);
        assert!(!job.lookup_only);
        let row = &model.view().rows[0];
        assert!(row.can_check && !row.ready && !row.can_force);
    }
    check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |inner, _, mut step| loop {
            match step {
                Step::Review {
                    host, common_dir, ..
                } => {
                    let mut model = lock(&inner.model);
                    let job = model.jobs.iter_mut().find(|job| job.id == "job").unwrap();
                    let review = json!({"state": "ok", "common_dir": "/home/example/repo/.git",
                        "worktrees": [{"kind": "linked", "verdict": "ready", "path": "/home/example/repo-work-1",
                        "fingerprint": "f1", "ownership_id": "owner-1", "zerus_owned": true,
                        "archive_restore_ready": true, "reasons": [], "changes": 0, "untracked": 0,
                        "disk_complete": true, "ignored_complete": true, "ignored_count": 0,
                        "integration": {"kind": "ancestor", "base": "main"}, "merged": true}]});
                    step = job.cleanup.reviewed(&host, &common_dir, Ok(&review));
                }
                Step::Remove { .. } => panic!("Check again must not remove"),
                Step::Idle | Step::Done => {
                    lock(&inner.model).jobs[0].busy = false;
                    break;
                }
            }
        },
    )
    .unwrap();
    let settled = lock(&state.0.model).view();
    assert!(settled.rows[0].ready && !settled.busy);
    assert!(!app.events().iter().any(|event| event.starts_with("drive")));
}

/// zerus-guttv/zerus-g5hh5: settle and settle_removal hand back a phase whose
/// retirement failed while its source is still current, without closing it;
/// the row is lookup-only and held.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_current_holds(stopped: programs::Stopped, removal: programs::Removal) {
    assert!(stopped.current() && removal.current());
    let app = TestApp::default();
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::publish(
        state.0.clone(),
        app.clone(),
        "job".into(),
        program_pipeline::settle(stopped),
    );
    // Each producer is checked before the next one runs: both phases share
    // the same original source.
    let held_current = |state: &TerminationCleanup| {
        lock(&state.0.model).jobs[0]
            .programs
            .as_ref()
            .is_some_and(program_pipeline::ProgramJob::current)
    };
    assert!(held_current(&state), "settle closed a current source");
    let removal_state = TerminationCleanup::default();
    let mut job = stopping_job();
    job.phase = "removing";
    let (gone, live) = program_pipeline::record_removal(
        &mut job,
        program_pipeline::settle_removal(Box::new(removal)),
    );
    assert!(!gone && live);
    lock(&removal_state.0.model).jobs.push_back(job);
    assert!(
        held_current(&removal_state),
        "settle_removal closed a current source"
    );
    for (state, retirement) in [
        (&state, ProgramRetirement::StopUnconfirmed),
        (&removal_state, ProgramRetirement::CleanupUnconfirmed),
    ] {
        assert!(held_current(state), "a held phase was closed");
        {
            let model = lock(&state.0.model);
            let job = &model.jobs[0];
            assert!(job.lookup_only);
            assert!(!job.busy);
            assert_eq!(job.program_retirement, Some(retirement));
        }
        assert!(check_with_runtime(
            &state.0,
            "job".into(),
            |model| model.revision = model.revision.wrapping_add(1),
            |_, _, _| panic!("a held phase was reviewed"),
        )
        .is_err());
    }
    // The real watcher keeps the held stop while its source is current.
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        lock(&state.0.model).jobs[0].programs.is_some(),
        "the watcher released a current held stop"
    );
    // Teardown: the shared source stays current for the later controls.
    lock(&state.0.model).jobs.clear();
    lock(&removal_state.0.model).jobs.clear();
}

/// zerus-fojet/guttv/g5hh5: the real async glue. force_offer and force_confirm
/// run on their own executor; the original's blocking half and a Complete
/// stop's refused cleanup publish for real; each starts the real watcher,
/// which alone releases the held custody as a lookup-only row.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_glue(
    offered: programs::Stopped,
    confirmed: programs::Stopped,
    original: programs::Continuation,
    complete: programs::Stopped,
) {
    fn executor<T: Send + 'static>(run: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::spawn(run).join().unwrap()
    }
    fn lookup_only(state: &TerminationCleanup, retirement: ProgramRetirement) {
        let model = lock(&state.0.model);
        assert!(model.jobs[0].lookup_only);
        assert!(model.jobs[0].physically_released);
        assert_eq!(model.jobs[0].program_retirement, Some(retirement));
    }
    let app = TestApp::default();

    // A stale Force stop click after the source went away.
    let state = TerminationCleanup::default();
    lock(&state.0.model)
        .jobs
        .push_back(published_incomplete(offered));
    let (inner, copy) = (state.0.clone(), app.clone());
    assert!(executor(
        move || tauri::async_runtime::block_on(program_pipeline::force_offer(
            inner,
            copy,
            "job".into()
        ))
    )
    .is_err());
    watcher_released_lookup_only(&state, "force_offer did not watch the returned stop");
    lookup_only(&state, ProgramRetirement::OfferUnavailable);

    // A stale Force confirmation; its blocking half publishes.
    let state = TerminationCleanup::default();
    let mut job = published_incomplete(confirmed);
    let Some(program_pipeline::ProgramJob::Stopped(stopped)) = job.programs.take() else {
        panic!("published stop")
    };
    job.programs = Some(program_pipeline::ProgramJob::Force {
        id: "force".into(),
        revision: 1,
        offer: Box::new(programs::fixture_force(*stopped)),
    });
    lock(&state.0.model).jobs.push_back(job);
    let (inner, copy) = (state.0.clone(), app.clone());
    assert!(executor(
        move || tauri::async_runtime::block_on(program_pipeline::force_confirm(
            inner,
            copy,
            "job".into(),
            "force".into()
        ))
    )
    .is_ok());
    watcher_released_lookup_only(&state, "force_confirm never published its result");
    lookup_only(&state, ProgramRetirement::StopUnconfirmed);

    // The original's blocking half after its source went away.
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::stop_and_publish(state.0.clone(), app.clone(), "job".into(), original);
    watcher_released_lookup_only(&state, "the original's stop was never published or watched");
    lookup_only(&state, ProgramRetirement::StopUnconfirmed);

    // publish sends a Complete stop to finish_cleanup, whose cleanup is refused.
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::publish(state.0.clone(), app.clone(), "job".into(), Ok(complete));
    watcher_released_lookup_only(
        &state,
        "finish_cleanup did not watch the refused cleanup's stop",
    );
    lookup_only(&state, ProgramRetirement::CleanupUnconfirmed);
    assert!(lock(&state.0.model).jobs[0]
        .cleanup
        .entries
        .iter()
        .all(|entry| entry.status == Some(Status::Unconfirmed)));
    assert!(!app
        .events()
        .iter()
        .any(|event| event.starts_with("removed")));
}

/// One online review that finds the fixture folder ready; never removes.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn ready_review(inner: Arc<Inner>, mut step: Step) {
    loop {
        match step {
            Step::Review {
                host, common_dir, ..
            } => {
                let mut model = lock(&inner.model);
                let job = model.jobs.iter_mut().find(|job| job.id == "job").unwrap();
                let review = json!({"state": "ok", "common_dir": "/home/example/repo/.git",
                    "worktrees": [{"kind": "linked", "verdict": "ready", "path": "/home/example/repo-work-1",
                    "fingerprint": "f1", "ownership_id": "owner-1", "zerus_owned": true,
                    "archive_restore_ready": true, "reasons": [], "changes": 0, "untracked": 0,
                    "disk_complete": true, "ignored_complete": true, "ignored_count": 0,
                    "integration": {"kind": "ancestor", "base": "main"}, "merged": true}]});
                step = job.cleanup.reviewed(&host, &common_dir, Ok(&review));
            }
            Step::Remove { .. } => panic!("Check again must not remove"),
            Step::Idle | Step::Done => {
                lock(&inner.model).jobs[0].busy = false;
                break;
            }
        }
    }
}

/// zerus-1y897.6 (current source): Check again on a published known refusal
/// observes and closes for real; the holder replies Closed but its output stays
/// open, so the close stays outstanding while the real watcher leaves the
/// reserved empty slot alone. Once released, the same click starts exactly one
/// review.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_delayed_close_check(
    stopped: programs::Stopped,
    release: impl FnOnce() + Send + 'static,
) {
    static REVIEWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    use std::sync::atomic::Ordering::SeqCst;
    assert!(stopped.reviewable_refusal());
    assert!(stopped.current());
    let app = TestApp::default();
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::publish(state.0.clone(), app.clone(), "job".into(), Ok(stopped));
    assert!(lock(&state.0.model).view().rows[0].can_check);
    let (inner, clicker) = (state.0.clone(), app.clone());
    let (polled, first_poll) = std::sync::mpsc::channel();
    // zerus-wfynn: the GTK main thread's side of the real command. One poll
    // must yield while the close is outstanding; the rest finishes off-thread.
    let click = std::thread::spawn(move || {
        let mut command = Box::pin(check_blocking(
            inner,
            clicker,
            "job".into(),
            |app: &TestApp, model| program_pipeline::PipelineApp::changed(app, model),
            |_, inner, _, step| {
                REVIEWS.fetch_add(1, SeqCst);
                ready_review(inner, step)
            },
        ));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = polled.send(std::future::Future::poll(command.as_mut(), &mut context).is_pending());
        tauri::async_runtime::block_on(command)
    });
    let mut release = Some(release);
    let yielded = first_poll.recv_timeout(Duration::from_secs(3));
    if yielded.is_err() {
        // End a failing run promptly: let the blocked close finish.
        if let Some(release) = release.take() {
            release();
        }
    }
    assert_eq!(
        yielded,
        Ok(true),
        "Check again blocked its calling thread while the close was outstanding"
    );
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        {
            let model = lock(&state.0.model);
            let job = &model.jobs[0];
            if job.programs.is_none() && job.reservation.is_some() && job.busy {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < until,
            "Check again never reserved the stop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Several watcher passes while the real close is outstanding.
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        !click.is_finished(),
        "the close did not wait for the holder"
    );
    {
        let model = lock(&state.0.model);
        let job = &model.jobs[0];
        assert!(job.programs.is_none() && job.reservation.is_some() && job.busy);
    }
    assert_eq!(REVIEWS.load(SeqCst), 0, "a review started before the close");
    assert!(check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |_, _, _| panic!("a second click reviewed"),
    )
    .is_err());
    assert!(lock(&state.0.model).dismiss("job").is_err());
    if let Some(release) = release.take() {
        release();
    }
    assert!(click.join().unwrap().is_ok());
    assert_eq!(REVIEWS.load(SeqCst), 1, "the click did not review once");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(REVIEWS.load(SeqCst), 1);
    assert!(
        !app.events().iter().any(|event| event.starts_with("drive")),
        "the watcher started a second review"
    );
    let model = lock(&state.0.model);
    let job = &model.jobs[0];
    assert!(job.programs.is_none() && job.reservation.is_none() && !job.busy);
    assert!(model.view().rows[0].ready);
}

/// zerus-1y897.6 (expired source): a known refusal is published after its
/// source deadline passed. The click's own retirement cancels the owner and
/// reads its real join as Pending (the holder's output stays open), so the
/// click keeps its reservation; the real watcher keeps reading Pending until
/// the holder output is released, then drives exactly one review for it.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn assert_real_expired_reserved_review(
    stopped: programs::Stopped,
    release: impl FnOnce() + Send + 'static,
) {
    assert!(stopped.reviewable_refusal());
    assert_eq!(stopped.remaining_millis(), 0);
    let app = TestApp::default();
    let state = TerminationCleanup::default();
    lock(&state.0.model).jobs.push_back(stopping_job());
    program_pipeline::publish(state.0.clone(), app.clone(), "job".into(), Ok(stopped));
    // The click lands before the new watcher's first pass, which sleeps first.
    let clicked = check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |_, _, _| panic!("Check again reviewed a pending stop"),
    )
    .unwrap();
    assert!(clicked.rows[0].busy);
    let reserved = |state: &TerminationCleanup| {
        let model = lock(&state.0.model);
        let job = &model.jobs[0];
        job.reservation.is_some() && job.busy
    };
    assert!(reserved(&state), "the pending click lost its reservation");
    assert!(lock(&state.0.model).jobs[0].programs.is_some());
    assert!(check_with_runtime(
        &state.0,
        "job".into(),
        |model| model.revision = model.revision.wrapping_add(1),
        |_, _, _| panic!("a second click reviewed"),
    )
    .is_err());
    assert!(lock(&state.0.model).dismiss("job").is_err());
    let drives = |app: &TestApp| {
        app.events()
            .iter()
            .filter(|event| event.as_str() == "drive job")
            .count()
    };
    // Several real watcher passes read Pending while the output stays open.
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        reserved(&state),
        "the watcher dropped the pending reservation"
    );
    assert_eq!(
        drives(&app),
        0,
        "a review started before the holder retired"
    );
    assert_eq!(
        lock(&state.0.model).jobs[0].program_retirement,
        Some(ProgramRetirement::Expired)
    );
    release();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while drives(&app) == 0 {
        assert!(
            std::time::Instant::now() < until,
            "the watcher never drove the reserved review"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        drives(&app),
        1,
        "the reserved click reviewed more than once"
    );
    let model = lock(&state.0.model);
    let job = &model.jobs[0];
    assert!(job.programs.is_none() && job.reservation.is_none() && job.manual_review);
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
    let unknown_folders = m.unknown_folders();
    let mut removing_folders = BTreeSet::new();
    for j in &mut m.jobs {
        if j.dismissed
            || unknown_folders.contains(&presentation::job_key(j))
            || !j.manual_review
            || !j.has_launch_owner()
            || j.has_programs()
        {
            continue;
        }
        if removing_folders.contains(&presentation::job_key(j)) {
            continue;
        }
        let step = j.cleanup.remove(consents);
        if matches!(step, Step::Remove { .. }) {
            removing_folders.insert(presentation::job_key(j));
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
        state.0.drive(app.clone(), id, step, true)
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
    if m.revision != revision || !m.restoring.is_empty() {
        return Err("Cleanup changed or is still running. Review the recorded outcomes.".into());
    }
    m.clear_settled_results();
    changed(&app, &mut m);
    Ok(m.view())
}

/// Dismiss one folder result. Unknown evidence stays in the native model.
#[tauri::command]
pub fn termination_cleanup_dismiss(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
    revision: u64,
) -> Result<View, String> {
    let mut model = lock(&state.0.model);
    if model.revision != revision {
        return Err(STATE_CHANGED.into());
    }
    model.dismiss(&id)?;
    changed(&app, &mut model);
    Ok(model.view())
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

    /// zerus-wfynn: Tauri runs a synchronous command inline on the GTK main
    /// thread. Only these reviewed cleanup commands stay synchronous, and none
    /// reaches a program retirement, observation or close; Check again, whose
    /// retirement can wait for a holder close, must stay async.
    #[test]
    fn only_reviewed_cleanup_commands_run_on_the_main_thread() {
        let sources = [
            include_str!("termination_cleanup.rs"),
            include_str!("termination_cleanup/manual.rs"),
            include_str!("termination_cleanup/presentation.rs"),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            include_str!("termination_cleanup/program_pipeline.rs"),
        ];
        // The braces of the item that starts at `at`, inclusive.
        fn body(source: &str, at: usize) -> &str {
            let open = at + source[at..].find('{').unwrap();
            let mut depth = 0;
            let close = source[open..]
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map(|(offset, _)| open + offset)
                .unwrap();
            &source[open..=close]
        }
        fn non_blocking(name: &str, body: &str) {
            for blocking in [
                "retirement(",
                ".close(",
                "observe(",
                "check_with_runtime(",
                "offer_force(",
                ".confirm(",
                "run_removal(",
                "settle(",
            ] {
                assert!(
                    !body.contains(blocking),
                    "{name} runs `{blocking}` on the main thread"
                );
            }
        }
        let marker = ["#[tauri::", "command]"].concat();
        let mut synchronous = Vec::new();
        for source in sources {
            for (at, _) in source.match_indices(&marker) {
                let signature = source[at + marker.len()..].trim_start();
                let Some(rest) = signature.strip_prefix("pub fn ") else {
                    continue;
                };
                let name = rest.split('(').next().unwrap();
                non_blocking(name, body(source, at));
                synchronous.push(name.to_owned());
            }
        }
        synchronous.sort();
        assert_eq!(
            synchronous,
            [
                "termination_cleanup_dismiss",
                "termination_cleanup_keep",
                "termination_cleanup_manual_cancel",
                "termination_cleanup_manual_confirm",
                "termination_cleanup_remove",
                "termination_cleanup_view",
                "termination_program_force_cancel",
            ]
        );
        // The reviewed helpers those commands call directly stay non-blocking.
        // Each needle is built here so it cannot match its own literal above.
        for helper in [
            "view",
            "dismiss",
            "clear_settled_results",
            "admit_manual",
            "confirm_manual",
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            "force_cancel",
        ] {
            let needle = ["fn ", helper, "("].concat();
            let (source, at) = sources
                .iter()
                .find_map(|source| source.find(&needle).map(|at| (*source, at)))
                .unwrap_or_else(|| panic!("{helper} is missing"));
            non_blocking(helper, body(source, at));
        }
        assert!(sources[0].contains("pub async fn termination_cleanup_check("));
    }
    /// zerus-y2oxp: Check again only reviews; its review passes
    /// allow_remove = false and never removes by itself. A test cannot build
    /// the Wry AppHandle the command's review needs, so the command and its
    /// blocking body are pinned to their reviewed text, ignoring whitespace.
    #[test]
    fn check_again_reviews_without_removing() {
        let source = include_str!("termination_cleanup.rs");
        let production = &source[..source.find(&["mod ", "tests {"].concat()).unwrap()];
        let tokens = |text: &str| text.split_whitespace().collect::<String>();
        let item = |name: &str| {
            let at = production
                .find(&["fn ", name, "("].concat())
                .or_else(|| production.find(&["fn ", name, "<"].concat()))
                .unwrap_or_else(|| panic!("{name} is missing"));
            let open = at + production[at..].find('{').unwrap();
            let mut depth = 0;
            let close = production[open..]
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map(|(offset, _)| open + offset)
                .unwrap();
            tokens(&production[at..=close])
        };
        let command = item("termination_cleanup_check");
        assert!(
            command.contains(&tokens("inner.drive(app.clone(), id, step, false)")),
            "Check again's review may remove"
        );
        assert_eq!(
            command,
            tokens(EXPECTED_CHECK),
            "the Check again command changed"
        );
        assert_eq!(
            item("check_blocking"),
            tokens(EXPECTED_BLOCKING),
            "the Check again body changed"
        );
    }
    const EXPECTED_CHECK: &str = r#"
fn termination_cleanup_check(
    app: AppHandle,
    state: State<'_, TerminationCleanup>,
    id: String,
) -> Result<View, String> {
    check_blocking(state.0.clone(), app, id, changed, |app, inner, id, step| {
        inner.drive(app.clone(), id, step, false)
    })
    .await
}"#;
    const EXPECTED_BLOCKING: &str = r#"
fn check_blocking<A: Send + 'static>(
    inner: Arc<Inner>,
    app: A,
    id: String,
    emit: fn(&A, &mut Model),
    run_review: fn(&A, Arc<Inner>, String, Step),
) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app = &app;
        check_with_runtime(
            &inner,
            id,
            |model| emit(app, model),
            |inner, id, step| run_review(app, inner, id, step),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}"#;
    #[test]
    fn raw_unavailable_git_metadata_cannot_hide_the_confirmed_cleanup_offer() {
        let target = source();
        // workspace::empty emits false for both not_repo and unavailable.
        // Only the former proves this is an ordinary folder.
        for (metadata, inspect) in [("unavailable", true), ("not_repo", false), ("ok", false)] {
            let raw = serde_json::json!({
                "name": target.name, "tracked": true, "run_id": target.run_id,
                "conversation_id": target.conversation_id,
                "git_root": "", "git_branch": "", "git_worktree": false,
                "git_worktree_name": "", "git_detached": false, "git_common_dir": "",
                "git_metadata_state": metadata
            });
            let mut row = zerus_client_core::view::row(&raw);
            row.machine = "fixture-machine".into();
            assert_eq!(offer_candidate(&row, &target), Ok(inspect), "{metadata}");
            let wire = serde_json::to_value(row).unwrap();
            assert_eq!(
                wire.get("git_worktree")
                    .and_then(serde_json::Value::as_bool),
                (!inspect).then_some(false),
                "{metadata}"
            );
        }
    }
    #[test]
    fn known_ordinary_folder_never_needs_the_offer_pipeline_before_a_first_turn() {
        let mut target = source();
        target.conversation_id.clear();
        let mut row = zerus_client_core::ViewRow {
            name: target.name.clone(),
            state: "running".into(),
            run_id: Some(target.run_id.clone()),
            conversation_id: None,
            machine: "fixture-machine".into(),
            git_worktree: Some(false),
            ..Default::default()
        };
        assert_eq!(offer_candidate(&row, &target), Ok(false));
        row.git_worktree = None;
        assert_eq!(offer_candidate(&row, &target), Ok(false));
        target.conversation_id = "first-conversation".into();
        row.conversation_id = Some(target.conversation_id.clone());
        assert_eq!(
            offer_candidate(&row, &target),
            Ok(true),
            "unreported metadata still gets inspected"
        );
        row.git_worktree = Some(true);
        assert_eq!(offer_candidate(&row, &target), Ok(true));
        row.git_worktree = Some(false);
        row.conversation_id = Some("replacement-conversation".into());
        assert!(offer_candidate(&row, &target).is_err());
        row.conversation_id = Some(target.conversation_id.clone());
        row.run_id = Some("replacement".into());
        assert!(offer_candidate(&row, &target).is_err());
    }
    use zerus_client_core::termination_cleanup::Entry;
    #[test]
    fn a_native_read_resource_can_settle_while_the_cleanup_model_is_locked() {
        let inner = Arc::new(Inner::default());
        let read = inner.read().unwrap();
        let (start, started) = std::sync::mpsc::sync_channel(0);
        let (done, settled) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            started.recv().unwrap();
            drop(read);
            done.send(()).unwrap();
        });
        let model = lock(&inner.model);
        start.send(()).unwrap();
        let independent = settled.recv_timeout(Duration::from_secs(5)).is_ok();
        // Always release/join, including the old-order RED control.
        drop(model);
        worker.join().unwrap();
        assert!(
            independent,
            "cancel/Keep/expiry must not re-enter the held model lock"
        );
        assert!(lock(&inner.reads).is_empty());
        assert_eq!(
            Arc::strong_count(&inner),
            1,
            "read slots never retain the model"
        );
    }
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
        use zerus_client_core::{end_action::Captured, Action, Exit};
        // Each end runs from its undo window's capture (zerus-sa9qn.25); the
        // guard probe and its legacy fallback apply to that call.
        for (action, state) in [(Action::Archive, "paused"), (Action::Terminate, "running")] {
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
                let captured = Captured {
                    target: target.clone(),
                    action: action.clone(),
                    state: state.into(),
                };
                let (outcome, supported) =
                    tauri::async_runtime::block_on(crate::actions::dispatch_captured_end_with(
                        &actions,
                        captured,
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
                            if inv.args.iter().any(|s| s == "inspect") {
                                // The capture's fresh metadata read.
                                let value = serde_json::json!({"name":original.name,"run_id":original.run_id,
                                    "conversation_id":original.conversation_id,"state":state});
                                return Exit::Code {
                                    code: 0,
                                    stdout: serde_json::to_vec(&value).unwrap(),
                                    stderr: String::new(),
                                };
                            }
                            assert!(inv.args.iter().any(|s| s == "session-action"), "{inv:?}");
                            let body: serde_json::Value =
                                serde_json::from_slice(inv.stdin.as_deref().unwrap()).unwrap();
                            assert_eq!(body["expected_run_id"], original.run_id.as_str());
                            assert_eq!(body["expected_conversation_id"], original.conversation_id.as_str());
                            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let receipt = serde_json::json!({"request_id":body["request_id"],"name":original.name,
                                "run_id":original.run_id,"conversation_id":original.conversation_id,"status":"completed"});
                            Exit::Code {
                                code: 0,
                                stdout: serde_json::to_vec(&receipt).unwrap(),
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
        assert_eq!(
            view.kept, 1,
            "the native capacity counts jobs; the view counts folders"
        );
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
        assert_eq!(view.text, "Folder kept: work.");
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
            dismissed: false,
            program_review_pending: false,
            program_retirement: None,
            lookup_only: false,
            physically_released: false,
            source: None,
            archive: None,
            restore_supported: false,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            programs: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            reservation: None,
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
        assert_eq!(view.kept, 0);
        assert!(
            !view.rows[1].can_check,
            "an unknown sibling fences the whole folder"
        );
        assert_eq!(view.text, "Working folders: 1 unknown");
        m.jobs[1].busy = true;
        assert_eq!(m.view().text, "Working folders: 1 unknown");
    }
    #[test]
    fn clearing_settled_results_preserves_program_review_busy_and_unknown_rows() {
        let mut m = Model::default();
        let mut settled = job(Some(Status::Kept));
        settled.id = "settled".into();
        settled.cleanup.entries[0].row["path"] = json!("/srv/example/settled");
        m.jobs.push_back(settled);
        let mut programs = job(Some(Status::Kept));
        programs.id = "programs".into();
        // The live pipeline sets this before its stop starts. It survives the
        // holder's expiry: loss of Force consent is not a completed stop.
        programs.program_review_pending = true;
        assert!(!programs.has_programs());
        m.jobs.push_back(programs);
        let mut busy = job(None);
        busy.id = "busy".into();
        busy.busy = true;
        m.jobs.push_back(busy);
        let mut unknown = job(Some(Status::Unconfirmed));
        unknown.id = "unknown".into();
        m.jobs.push_back(unknown);
        m.overflow_kept = 2;
        m.overflow_unknown = 1;
        let originals: Vec<_> = m.jobs.iter().skip(1).map(|j| j.cleanup.clone()).collect();
        assert!(m.view().can_clear);
        m.clear_settled_results();
        assert_eq!(
            m.jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
            ["programs", "busy", "unknown"]
        );
        assert_eq!(
            m.jobs.iter().map(|j| j.cleanup.clone()).collect::<Vec<_>>(),
            originals
        );
        assert_eq!(m.overflow_kept, 0);
        assert_eq!(m.overflow_unknown, 1);
        assert!(!m.view().can_clear);
        m.clear_settled_results();
        assert_eq!(m.jobs.len(), 3);
    }
    #[test]
    fn program_retirement_is_closed_presentation_and_never_changes_the_cleanup_outcome() {
        let mut m = Model::default();
        let mut j = job(Some(Status::Kept));
        j.program_review_pending = true;
        let original = j.cleanup.clone();
        m.jobs.push_back(j);
        for (reason, code) in [
            (ProgramRetirement::Expired, "expired"),
            (ProgramRetirement::SourceUnavailable, "source_unavailable"),
            (ProgramRetirement::HolderUnavailable, "holder_unavailable"),
            (ProgramRetirement::OfferUnavailable, "offer_unavailable"),
            (ProgramRetirement::StopUnconfirmed, "stop_unconfirmed"),
            (ProgramRetirement::CleanupSettled, "cleanup_settled"),
            (ProgramRetirement::CleanupUnconfirmed, "cleanup_unconfirmed"),
        ] {
            m.jobs[0].program_retirement = Some(reason);
            let view = m.view();
            assert!(!view.rows[0].can_force);
            assert_eq!(view.rows[0].force_revision, None);
            assert_eq!(view.rows[0].status, "kept");
            assert_eq!(
                serde_json::to_value(view.rows[0].program_retirement).unwrap(),
                code
            );
            m.clear_settled_results();
            assert_eq!(m.jobs[0].cleanup, original);
        }
    }
    #[test]
    fn ordinary_known_use_is_a_settled_kept_result() {
        for row in [
            json!({"process_count":1}),
            json!({"processes":[{"pid":42}]}),
            json!({"reasons":[{"code":"processes"}]}),
        ] {
            let mut j = job(Some(Status::Kept));
            j.cleanup.entries[0].row = row;
            let mut m = Model::default();
            m.jobs.push_back(j);
            assert!(m.view().can_clear);
            m.clear_settled_results();
            assert!(m.jobs.is_empty());
        }
    }
    #[test]
    fn a_shared_folder_is_counted_once_and_dismisses_settled_origins_together() {
        let mut m = Model::default();
        for id in ["first", "second", "third"] {
            let mut j = job(Some(Status::Kept));
            j.id = id.into();
            j.cleanup.entries[0].row["sessions"] = json!(["claude/example/live"]);
            j.cleanup.entries[0].row["processes"] = json!([{"pid":42,"command":"report-server"}]);
            m.jobs.push_back(j);
        }
        let view = m.view();
        assert_eq!(view.kept, 1);
        assert_eq!(view.rows.len(), 3);
        assert!(view.rows.iter().all(|r| r.can_dismiss));
        assert_eq!(view.rows[0].sessions, ["claude/example/live"]);
        assert_eq!(view.rows[0].processes[0].pid, 42);
        m.dismiss("second").unwrap();
        assert!(m.jobs.is_empty());
    }
    #[test]
    fn a_pending_stop_keeps_every_origin_of_its_folder_in_review() {
        for busy in [false, true] {
            let mut m = Model::default();
            let mut pending = job(Some(Status::Kept));
            pending.id = "pending".into();
            pending.busy = busy;
            pending.program_review_pending = !busy;
            m.jobs.push_back(pending);
            let mut settled = job(Some(Status::Kept));
            settled.id = "settled".into();
            m.jobs.push_back(settled);
            let before: Vec<_> = m.jobs.iter().map(|j| j.cleanup.clone()).collect();
            assert!(!m.view().can_clear);
            assert!(m.dismiss("settled").is_err());
            m.clear_settled_results();
            assert_eq!(m.jobs.len(), 2);
            assert_eq!(
                m.jobs.iter().map(|j| j.cleanup.clone()).collect::<Vec<_>>(),
                before
            );
        }
    }
    #[test]
    fn unknown_dismissal_hides_only_one_result_and_preserves_its_native_fence() {
        let state = TerminationCleanup::default();
        {
            let mut m = lock(&state.0.model);
            for id in ["first", "second"] {
                let mut j = job(Some(Status::Unconfirmed));
                j.id = id.into();
                m.jobs.push_back(j);
            }
            m.clear_settled_results();
            assert_eq!(m.view().rows.len(), 2);
            let before = m.jobs[0].cleanup.clone();
            m.dismiss("first").unwrap();
            assert_eq!(m.view().rows.len(), 1);
            assert_eq!(m.jobs[0].cleanup, before);
            m.dismiss("second").unwrap();
            m.prune_dismissed();
            assert!(m.view().rows.is_empty());
            assert_eq!(m.jobs.len(), 2);
            assert_eq!(m.unknown_folders().len(), 1);
            let mut later = job(Some(Status::Kept));
            later.id = "later".into();
            later.manual_review = true;
            m.jobs.push_back(later);
            let row = m.view().rows.pop().unwrap();
            assert!(!row.can_check && !row.ready);
        }
        assert!(state.unknown(MACHINE, "/srv/example/.git", "/srv/example/work"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn physically_settled_program_cleanup_kept_or_unknown_can_be_dismissed() {
        use zerus_client_core::worktree_programs::cleanup::{Reason, Report, State};
        for status in [State::Kept, State::Unknown] {
            let state = TerminationCleanup::default();
            {
                let mut m = lock(&state.0.model);
                let mut j = job(Some(Status::Kept));
                j.program_review_pending = true;
                j.busy = true;
                j.cleanup.entries[0].row["process_count"] = json!(1);
                let report = Report {
                    request_id: "original-removal".into(),
                    binding: "original-binding".into(),
                    state: status,
                    reason: Some(if status == State::Kept {
                        Reason::Changed
                    } else {
                        Reason::Unconfirmed
                    }),
                };
                assert!(!program_pipeline::record_cleanup_result(
                    &mut j,
                    &Ok(report),
                    true
                ));
                assert_eq!(
                    j.program_retirement,
                    Some(if status == State::Kept {
                        ProgramRetirement::CleanupSettled
                    } else {
                        ProgramRetirement::CleanupUnconfirmed
                    })
                );
                m.jobs.push_back(j);
                assert!(m.view().rows[0].can_dismiss);
                assert_eq!(m.view().can_clear, status == State::Kept);
                m.dismiss("job-1").unwrap();
                assert!(m.view().rows.is_empty());
            }
            assert_eq!(
                state.unknown(MACHINE, "/srv/example/.git", "/srv/example/work"),
                status == State::Unknown
            );
        }
        let mut j = job(Some(Status::Kept));
        j.program_review_pending = true;
        j.busy = true;
        assert!(!program_pipeline::record_cleanup_result(
            &mut j,
            &Err("physical close unconfirmed".into()),
            false
        ));
        assert_eq!(
            j.program_retirement,
            Some(ProgramRetirement::CleanupUnconfirmed)
        );
        let mut m = Model::default();
        m.jobs.push_back(j);
        assert!(!m.view().can_clear && !m.view().rows[0].can_dismiss);
        assert!(m.dismiss("job-1").is_err());
    }

    #[test]
    fn a_running_folder_never_replaces_a_held_result_in_the_chip_text() {
        let mut m = Model::default();
        let mut running = job(None);
        running.id = "running".into();
        running.cleanup.entries[0].row = json!({"path":"/srv/example/running"});
        running.busy = true;
        m.jobs.push_back(running);
        assert_eq!(m.view().text, "Cleaning working folders…");

        m.jobs.push_back(job(Some(Status::Kept)));
        let view = m.view();
        assert!(view.cleaning);
        assert_eq!(
            view.text, "Folder kept: work.",
            "not the running folder's name"
        );

        m.jobs.back_mut().unwrap().cleanup.entries[0].status = Some(Status::Unconfirmed);
        assert_eq!(m.view().text, "Working folders: 1 unknown");
    }
    #[test]
    fn a_kept_result_past_the_row_cap_is_counted_rather_than_named() {
        let m = Model {
            overflow_kept: 1,
            ..Model::default()
        };
        assert_eq!(m.view().text, "Working folders: 1 kept");
    }
    #[test]
    fn activity_follows_the_running_job_and_stays_apart_from_its_outcome() {
        let mut m = Model::default();
        m.jobs.push_back(job(None));
        assert_eq!(m.view().rows[0].activity, None, "not running");
        m.jobs[0].busy = true;
        for phase in ["checking", "removing", "stopping"] {
            m.jobs[0].phase = phase;
            assert_eq!(m.view().rows[0].activity, Some(phase));
        }
        m.jobs[0].cleanup.entries[0].status = Some(Status::Kept);
        m.jobs[0].phase = "checking";
        let view = m.view();
        assert_eq!(view.rows[0].status, "kept");
        assert_eq!(view.rows[0].activity, Some("checking"));
        m.jobs[0].busy = false;
        assert_eq!(m.view().rows[0].activity, None);
    }
    #[test]
    fn a_restoring_row_reports_the_restore_and_not_a_cleanup_phase() {
        let mut model = Model::default();
        model.retain(&source(), MACHINE, &report("kept"), true);
        model.jobs[0].busy = true;
        model
            .restoring
            .insert(format!("{}\n{}", source().computer, ARCHIVE));
        assert_eq!(model.view().rows[0].activity, Some("restoring"));
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
    /// zerus-k2ga: a dirty folder beside hidden programs is ready, counted
    /// and needs both consents, alone or beside a clean folder, so the
    /// counted folders are the ones core cleanup removes.
    #[test]
    fn a_dirty_folder_beside_hidden_programs_is_ready_counted_and_needs_both_consents() {
        let combined = json!({"path":"/srv/example/work","kind":"linked","verdict":"blocked","fingerprint":"f",
            "reasons":[{"code":"changes"},{"code":"processes_unavailable"}],"discardable":false,
            "discardable_with_process_consent":true,"discard_fingerprint":"d","process_fingerprint":"p"});
        let reviewed = |path: &str, row: Value, id: &str| {
            let mut j = job(Some(Status::Kept));
            j.id = id.into();
            j.cleanup.entries[0] = Entry::terminated(
                "example-peer",
                MACHINE,
                path,
                "/srv/example/.git",
                "owner-1",
                None,
            )
            .unwrap();
            j.cleanup.entries[0].status = Some(Status::Kept);
            j.cleanup.entries[0].verified = true;
            j.cleanup.entries[0].row = row;
            j.manual_review = true;
            j
        };
        let mut m = Model::default();
        m.jobs
            .push_back(reviewed("/srv/example/work", combined.clone(), "job-1"));
        let v = m.view();
        assert!(
            v.rows[0].ready,
            "the dirty folder alone is a removal target"
        );
        assert!(v.required.discard_changes && v.required.unchecked_processes);
        let clean = json!({"path":"/srv/example/other","kind":"linked","verdict":"ready","fingerprint":"g","reasons":[]});
        m.jobs
            .push_back(reviewed("/srv/example/other", clean, "job-2"));
        let v = m.view();
        assert_eq!(
            v.rows.iter().map(|r| r.ready).collect::<Vec<_>>(),
            [true, true]
        );
        assert!(v.required.discard_changes && v.required.unchecked_processes);
        for (job, row) in m.jobs.iter().zip(&v.rows) {
            assert_eq!(
                row.ready,
                job.cleanup.entries[0].eligible(),
                "counted as core removes"
            );
        }
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
        assert!(lock(&inner.reads).is_empty());
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
