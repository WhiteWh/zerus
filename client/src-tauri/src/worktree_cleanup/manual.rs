//! Overview selection adapter for the existing explicit manual state machine.
//! Targets originate only in the native overview, never caller-supplied paths.
use super::*;
use crate::organization::{
    overview::{self, Overview, Target},
    Organization,
};
use tauri::Manager;
use zerus_client_core::termination_cleanup::{Cleanup, Consents, Entry, Status, Step};
type Checked = Arc<dyn Fn(&Target) -> Result<Value, String> + Send + Sync>;
type Remove = Arc<dyn Fn(&Target, &review::Removal) -> review::Removed + Send + Sync>;
#[derive(Clone)]
struct Calls {
    review: Checked,
    remove: Remove,
    changed: Arc<dyn Fn() + Send + Sync>,
    invalidate: Arc<dyn Fn(&Target) + Send + Sync>,
    unknown: Arc<dyn Fn(&Target) -> bool + Send + Sync>,
}
pub(super) struct Dialog {
    id: String,
    revision: u64,
    targets: Vec<Target>,
    cleanup: Cleanup,
    pub(super) busy: bool,
    active: Option<usize>,
    removing: bool,
    stopping: bool,
    closed: bool,
    attempted: bool,
}
#[derive(Serialize)]
pub struct Row {
    id: String,
    path: String,
    label: String,
    computer: String,
    machine: String,
    branch: String,
    size: String,
    status: &'static str,
    reason: String,
    details: String,
    ready: bool,
    can_check: bool,
    forgotten: bool,
}
#[derive(Serialize)]
pub struct View {
    id: String,
    revision: u64,
    rows: Vec<Row>,
    busy: bool,
    cleaning: bool,
    stopping: bool,
    attempted: bool,
    removed: usize,
    kept: usize,
    unknown: usize,
    required: Consents,
}
fn eligible(e: &Entry) -> bool {
    e.verified
        && !matches!(e.status, Some(Status::Removed | Status::Unconfirmed))
        && (review::removable(&e.row)
            || review::process_reviewable(&e.row)
            || (e.row["discardable"] == true && !text(&e.row, "discard_fingerprint").is_empty()))
}
impl Dialog {
    fn view(&self) -> View {
        View {
            id: self.id.clone(),
            revision: self.revision,
            busy: self.busy,
            cleaning: self.removing && self.busy,
            stopping: self.stopping,
            attempted: self.attempted,
            removed: self.cleanup.count(Status::Removed),
            kept: self.cleanup.count(Status::Kept),
            unknown: self.cleanup.count(Status::Unconfirmed),
            required: self.cleanup.required(),
            rows: self
                .cleanup
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| self.row(i, e))
                .collect(),
        }
    }
    fn row(&self, i: usize, e: &Entry) -> Row {
        let t = &self.targets[i];
        let state = if self.busy && self.active == Some(i) {
            if self.removing {
                "removing"
            } else {
                "checking"
            }
        } else if e.status == Some(Status::Removed) {
            "removed"
        } else if e.status == Some(Status::Unconfirmed) {
            "unknown"
        } else if eligible(e) {
            "ready"
        } else if !e.verified && self.busy {
            "checking"
        } else {
            "kept"
        };
        let reason = if !e.detail.is_empty() {
            e.detail.clone()
        } else if !e.review_error.is_empty() {
            e.review_error.clone()
        } else {
            review::summary_line(&e.row)
        };
        let mut details=vec![t.path.clone(),reason.clone(),"After removal: conversations remain. Restoring an archived session may require recreating its working folder.".into()];
        details.extend(review::summary(&e.row));
        if review::local_data(&e.row) {
            details.push(format!(
                "Delete local data: {} {}, {} in total.",
                e.row["ignored_count"].as_u64().unwrap_or(0),
                if e.row["ignored_count"] == 1 {
                    "entry"
                } else {
                    "entries"
                },
                review::data_size(e.row["ignored_bytes"].as_f64().unwrap_or(0.))
            ));
        }
        for item in e.row["ignored"].as_array().into_iter().flatten().take(20) {
            details.push(format!(
                "Delete local data: {} ({})",
                text(item, "path"),
                review::data_size(item["bytes"].as_f64().unwrap_or(0.))
            ));
        }
        Row {
            id: t.id.clone(),
            path: t.path.clone(),
            label: t
                .path
                .rsplit(['/', '\\'])
                .find(|s| !s.is_empty())
                .unwrap_or(&t.path)
                .into(),
            computer: t.computer().into(),
            machine: t.label().into(),
            branch: text(&e.row, "branch").into(),
            size: review::size(&e.row),
            status: state,
            reason,
            details: details.join("\n"),
            forgotten: e.forgotten,
            ready: eligible(e),
            can_check: !self.busy && e.status == Some(Status::Kept),
        }
    }
    fn step(&mut self, step: &Step) {
        self.active = match step {
            Step::Review { index, .. } | Step::Remove { index, .. } => Some(*index),
            _ => None,
        };
        self.removing = matches!(step, Step::Remove { .. });
        self.busy = self.active.is_some();
        self.revision += 1;
    }
}
fn refused(detail: String) -> review::Removed {
    review::Removed::Refused {
        reply: serde_json::json!({"status":"kept"}),
        detail,
    }
}
fn refuse_overflow(app: &AppHandle) -> Result<(), String> {
    if app
        .state::<crate::termination_cleanup::TerminationCleanup>()
        .unknown_overflow()
    {
        Err("Too many unconfirmed removals are retained. Review them before removing more worktrees.".into())
    } else {
        Ok(())
    }
}

fn calls(org: Organization, catalogs: crate::newsession::NewSession, app: AppHandle) -> Calls {
    let review_org = org.clone();
    let notify = app.clone();
    let reviews = app.clone();
    let removals = app.clone();
    Calls {
        review: Arc::new(move |t| {
            refuse_overflow(&reviews)?;
            t.check(&review_org)?;
            let value = read(t.computer(), t.machine(), &t.common, &t.path)?;
            t.check(&review_org)?;
            Ok(value)
        }),
        remove: Arc::new(move |t, r| {
            match refuse_overflow(&removals).and_then(|()| t.check(&org)) {
                Ok(()) => remove(t.computer(), t.machine(), r),
                Err(e) => refused(e),
            }
        }),
        unknown: Arc::new(move |t| {
            app.state::<crate::termination_cleanup::TerminationCleanup>()
                .unknown(t.machine(), &t.common, &t.path)
        }),
        changed: Arc::new(move || {
            let _ = notify.emit("worktree-manual-changed", ());
        }),
        invalidate: Arc::new(move |t| {
            catalogs.invalidate_worktrees(&ComputerId(t.computer().into()), &t.common)
        }),
    }
}
impl WorktreeCleanup {
    fn begin_manual(&self, targets: Vec<Target>) -> Result<(View, Step), String> {
        if targets.is_empty() || targets.len() > LIMIT {
            return Err("Select at most 64 worktrees for one cleanup.".into());
        }
        let mut m = lock(&self.0);
        if m.reading
            || m.dialog.as_ref().is_some_and(|d| d.busy)
            || m.manual.as_ref().is_some_and(|d| d.busy)
        {
            return Err("A worktree review is already running.".into());
        }
        let entries = targets
            .iter()
            .map(|t| {
                let unknown =
                    m.unknown
                        .get(&(t.machine().into(), t.common.clone(), t.path.clone()));
                Entry {
                    host: t.computer().into(),
                    machine: t.machine().into(),
                    common_dir: t.common.clone(),
                    exit_cleanup: false,
                    expected_owner_id: String::new(),
                    row: serde_json::json!({"path":t.path,"kind":"linked","branch":t.branch}),
                    status: unknown.map(|_| Status::Unconfirmed),
                    detail: unknown.cloned().unwrap_or_default(),
                    review_error: String::new(),
                    verified: false,
                    offline: false,
                    forgotten: false,
                }
            })
            .collect();
        let mut dialog = Dialog {
            id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            targets,
            cleanup: Cleanup::manual(entries),
            busy: false,
            active: None,
            removing: false,
            stopping: false,
            closed: false,
            attempted: false,
        };
        let step = dialog.cleanup.step();
        dialog.step(&step);
        let v = dialog.view();
        m.dialog = None;
        m.manual = Some(dialog);
        Ok((v, step))
    }
    fn manual_view(&self, id: &str) -> Result<View, String> {
        let m = lock(&self.0);
        Ok(m.manual
            .as_ref()
            .filter(|d| d.id == id)
            .ok_or(CHANGED)?
            .view())
    }
    fn manual_remove(
        &self,
        id: &str,
        revision: u64,
        consents: Consents,
    ) -> Result<(View, Step), String> {
        let mut m = lock(&self.0);
        if m.reading || m.dialog.as_ref().is_some_and(|d| d.busy) {
            return Err(CHANGED.into());
        }
        let quota = m.unknown.len();
        let d = m
            .manual
            .as_mut()
            .filter(|d| d.id == id && !d.busy && !d.closed && d.revision == revision)
            .ok_or(CHANGED)?;
        if d.cleanup.entries.iter().filter(|e| eligible(e)).count() + quota > LIMIT {
            return Err("Too many unconfirmed removals are retained. Review them before removing more worktrees.".into());
        }
        let required = d.cleanup.required();
        if (required.local_files && !consents.local_files)
            || (required.discard_changes && !consents.discard_changes)
            || (required.unchecked_processes && !consents.unchecked_processes)
        {
            return Err("Confirm the required cleanup choices before removing folders.".into());
        }
        let step = d.cleanup.remove(consents);
        if !matches!(step, Step::Remove { .. }) {
            return Err(CHANGED.into());
        }
        d.attempted = true;
        d.step(&step);
        Ok((d.view(), step))
    }
    fn manual_check(&self, id: &str) -> Result<(View, Step), String> {
        let mut m = lock(&self.0);
        if m.reading || m.dialog.as_ref().is_some_and(|d| d.busy) {
            return Err(CHANGED.into());
        }
        let d = m
            .manual
            .as_mut()
            .filter(|d| d.id == id && !d.busy && !d.closed)
            .ok_or(CHANGED)?;
        if !d
            .cleanup
            .entries
            .iter()
            .any(|e| e.status == Some(Status::Kept))
        {
            return Err(CHANGED.into());
        }
        d.cleanup.review_remaining();
        d.attempted = false;
        d.stopping = false;
        let step = d.cleanup.step();
        d.step(&step);
        Ok((d.view(), step))
    }
    fn drive_manual(&self, id: String, mut step: Step, calls: Calls) {
        loop {
            let (index, target) = {
                let m = lock(&self.0);
                let Some(d) = m.manual.as_ref().filter(|d| d.id == id) else {
                    return;
                };
                match step {
                    Step::Review { index, .. } | Step::Remove { index, .. } => {
                        (index, d.targets[index].clone())
                    }
                    _ => return,
                }
            };
            match step {
                Step::Review {
                    ref host,
                    ref common_dir,
                    ..
                } => {
                    if (calls.unknown)(&target)
                        || self.unknown(target.machine(), &target.common, &target.path)
                    {
                        let mut m = lock(&self.0);
                        let Some(d) = m.manual.as_mut().filter(|d| d.id == id) else {
                            return;
                        };
                        d.cleanup.entries[index].status = Some(Status::Unconfirmed);
                        d.cleanup.entries[index].verified = false;
                        d.cleanup.entries[index].detail =
                            "An unknown result will not be retried.".into();
                        step = d.cleanup.step();
                        d.step(&step);
                        drop(m);
                        (calls.changed)();
                        continue;
                    }
                    let result=(calls.review)(&target).and_then(|value| {
                        let rows=value["worktrees"].as_array().ok_or(CHANGED)?;
                        let mut found=rows.iter().filter(|r|r["path"]==target.path);
                        let row=found.next().ok_or(CHANGED)?;
                        if found.next().is_some() || row["kind"]!="linked" {return Err(CHANGED.into());}
                        if serde_json::to_vec(row).map_or(true,|b|b.len()>64*1024) {
                            return Err("The folder review is too large. Inspect it before continuing.".into());
                        }
                        if target.owner.as_ref().is_some_and(|o|overview::owner(row)!=*o) {
                            return Err("Checkout ownership changed. Refresh Worktrees and select the folder again.".into());
                        }
                        Ok(value)
                    });
                    let mut m = lock(&self.0);
                    let Some(d) = m.manual.as_mut().filter(|d| d.id == id) else {
                        return;
                    };
                    if let Ok(v) = &result {
                        if d.targets[index].owner.is_none() {
                            d.targets[index].owner = Some(
                                v["worktrees"]
                                    .as_array()
                                    .and_then(|a| a.iter().find(|r| r["path"] == target.path))
                                    .map(overview::owner)
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    step =
                        d.cleanup
                            .reviewed(host, common_dir, result.as_ref().map_err(Clone::clone));
                    if d.closed {
                        d.busy = false;
                        d.active = None;
                        d.revision += 1;
                        drop(m);
                        (calls.changed)();
                        return;
                    }
                    d.step(&step);
                }
                Step::Remove { ref removal, .. } => {
                    let already_unknown =
                        self.unknown(target.machine(), &target.common, &target.path);
                    let result = if already_unknown || (calls.unknown)(&target) {
                        review::Removed::Unknown {
                            detail: "An unknown result will not be retried.".into(),
                        }
                    } else {
                        (calls.remove)(&target, removal)
                    };
                    (calls.invalidate)(&target);
                    let mut m = lock(&self.0);
                    if matches!(result, review::Removed::Unknown { .. })
                        || matches!(&result,review::Removed::Refused{reply,..} if reply["status"]!="kept")
                    {
                        let detail = match &result {
                            review::Removed::Unknown { detail }
                            | review::Removed::Refused { detail, .. } => detail.clone(),
                            _ => String::new(),
                        };
                        m.unknown.insert(
                            (
                                target.machine().into(),
                                target.common.clone(),
                                target.path.clone(),
                            ),
                            detail,
                        );
                    }
                    let Some(d) = m.manual.as_mut().filter(|d| d.id == id) else {
                        return;
                    };
                    step = d.cleanup.removed(&result);
                    d.step(&step);
                }
                _ => return,
            }
            (calls.changed)();
        }
    }
}
fn drive(state: WorktreeCleanup, id: String, step: Step, calls: Calls) {
    tauri::async_runtime::spawn_blocking(move || state.drive_manual(id, step, calls));
}
#[tauri::command]
pub async fn worktree_overview_review(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, WorktreeCleanup>,
    overview: State<'_, Overview>,
    org: State<'_, Organization>,
    catalogs: State<'_, crate::newsession::NewSession>,
    id: String,
) -> Result<View, String> {
    overview::main(&window)?;
    let own = state.inner().clone();
    let target = overview.inner().clone();
    let calls = calls(org.inner().clone(), catalogs.inner().clone(), app);
    let (view, step) =
        tauri::async_runtime::spawn_blocking(move || own.begin_manual(target.targets(&id)?))
            .await
            .map_err(|e| e.to_string())??;
    drive(state.inner().clone(), view.id.clone(), step, calls);
    Ok(view)
}
#[tauri::command]
pub async fn worktree_manual_view(
    window: tauri::Webview,
    state: State<'_, WorktreeCleanup>,
    id: String,
) -> Result<View, String> {
    overview::main(&window)?;
    let own = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || own.manual_view(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn worktree_manual_remove(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, WorktreeCleanup>,
    catalogs: State<'_, crate::newsession::NewSession>,
    id: String,
    revision: u64,
    consents: Consents,
) -> Result<View, String> {
    overview::main(&window)?;
    let org = app.state::<Organization>().inner().clone();
    let own = state.inner().clone();
    let key = id.clone();
    let (view, step) =
        tauri::async_runtime::spawn_blocking(move || own.manual_remove(&key, revision, consents))
            .await
            .map_err(|e| e.to_string())??;
    drive(
        state.inner().clone(),
        id,
        step,
        calls(org, catalogs.inner().clone(), app),
    );
    Ok(view)
}
#[tauri::command]
pub async fn worktree_manual_check(
    window: tauri::Webview,
    app: AppHandle,
    state: State<'_, WorktreeCleanup>,
    org: State<'_, Organization>,
    catalogs: State<'_, crate::newsession::NewSession>,
    id: String,
) -> Result<View, String> {
    overview::main(&window)?;
    let own = state.inner().clone();
    let key = id.clone();
    let (view, step) = tauri::async_runtime::spawn_blocking(move || own.manual_check(&key))
        .await
        .map_err(|e| e.to_string())??;
    drive(
        state.inner().clone(),
        id,
        step,
        calls(org.inner().clone(), catalogs.inner().clone(), app),
    );
    Ok(view)
}
#[tauri::command]
pub fn worktree_manual_stop(
    window: tauri::Webview,
    state: State<'_, WorktreeCleanup>,
    id: String,
    close: bool,
) -> Result<View, String> {
    overview::main(&window)?;
    let mut m = lock(&state.0);
    let d = m.manual.as_mut().filter(|d| d.id == id).ok_or(CHANGED)?;
    d.closed |= close;
    d.stopping |= d.busy;
    d.cleanup.stop();
    d.revision += 1;
    Ok(d.view())
}
#[tauri::command]
pub async fn worktree_manual_inspect(
    window: tauri::Webview,
    state: State<'_, WorktreeCleanup>,
    app: AppHandle,
    id: String,
    row: String,
    file: String,
    offset: usize,
) -> Result<Value, String> {
    overview::main(&window)?;
    let slot = app.state::<Overview>().inspection()?;
    let own = state.inner().clone();
    let org = app.state::<Organization>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _slot = slot;
        let target = {
            let m = lock(&own.0);
            let d = m
                .manual
                .as_ref()
                .filter(|d| d.id == id && !d.closed && !d.busy)
                .ok_or(CHANGED)?;
            d.targets
                .iter()
                .find(|t| t.id == row)
                .cloned()
                .ok_or(CHANGED)?
        };
        target.check(&org)?;
        let v = inspect(
            target.computer(),
            target.machine(),
            &target.path,
            &target.common,
            &file,
            offset,
        )?;
        target.check(&org)?;
        let m = lock(&own.0);
        if !m.manual.as_ref().is_some_and(|d| d.id == id && !d.closed) {
            return Err(CHANGED.into());
        }
        Ok(v)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn checked(t: &Target) -> Value {
        json!({"state":"ok","common_dir":t.common,"worktrees":[{"path":t.path,"branch":t.branch,"kind":"linked","verdict":"ready","available":true,"fingerprint":"original-proof","ownership_id":"original-owner","merged":true,"changes":0,"untracked":0,"integration":{"kind":"ancestor","base":"main"}}]})
    }
    fn calls() -> Calls {
        Calls {
            review: Arc::new(|t| Ok(checked(t))),
            remove: Arc::new(|t, _| review::Removed::Done {
                reply: json!({"status":"removed","path":t.path,"common_dir":t.common}),
            }),
            unknown: Arc::new(|_| false),
            changed: Arc::new(|| {}),
            invalidate: Arc::new(|_| {}),
        }
    }
    fn open(state: &WorktreeCleanup, count: usize, ops: Calls) -> View {
        let (v, step) = state
            .begin_manual((0..count).map(Target::fixture).collect())
            .unwrap();
        state.drive_manual(v.id.clone(), step, ops);
        state.manual_view(&v.id).unwrap()
    }
    #[test]
    fn opening_reviews_only_and_consented_removal_uses_original_fingerprints_once() {
        let state = WorktreeCleanup::default();
        let mut ops = calls();
        ops.review = Arc::new(|t| {
            let mut v = checked(t);
            let r = &mut v["worktrees"][0];
            r["verdict"] = json!("review");
            r["ignored_count"] = json!(1);
            r["ignored_bytes"] = json!(4096);
            r["ignored_fingerprint"] = json!("original-ignored");
            r["ignored"] = json!([{"path":"target/","bytes":4096}]);
            Ok(v)
        });
        let issued = Arc::new(Mutex::new(Vec::new()));
        let seen = issued.clone();
        ops.remove = Arc::new(move |t, r| {
            seen.lock().unwrap().push(r.clone());
            review::Removed::Done {
                reply: json!({"status":"removed","path":t.path,"common_dir":t.common}),
            }
        });
        let view = open(&state, 2, ops.clone());
        assert!(issued.lock().unwrap().is_empty());
        assert_eq!(view.rows.iter().filter(|r| r.ready).count(), 2);
        assert!(view.required.local_files);
        assert!(view.rows[0].details.contains("1 entry, 4.00 KiB"));
        assert!(state
            .manual_remove(&view.id, view.revision, Consents::default())
            .is_err());
        let (active, step) = state
            .manual_remove(
                &view.id,
                view.revision,
                Consents {
                    local_files: true,
                    merged_branches: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(state
            .manual_remove(&view.id, view.revision, Consents::default())
            .is_err());
        assert!(active.busy);
        state.drive_manual(view.id.clone(), step, ops);
        assert_eq!(state.manual_view(&view.id).unwrap().removed, 2);
        for r in issued.lock().unwrap().iter() {
            assert_eq!(r.fingerprint, "original-proof");
            assert_eq!(r.ignored_fingerprint, "original-ignored");
            assert!(r.delete_branch);
            assert!(!r.preserve_archive_restore);
            assert!(r.discard_fingerprint.is_empty());
        }
    }
    #[test]
    fn replaced_owner_duplicate_or_large_rows_cannot_become_removal_targets() {
        for mode in 0..3 {
            let state = WorktreeCleanup::default();
            let mut target = Target::fixture(0);
            target.owner = Some(overview::owner(&checked(&target)["worktrees"][0]));
            let mut ops = calls();
            ops.review = Arc::new(move |t| {
                let mut v = checked(t);
                if mode == 0 {
                    v["worktrees"][0]["ownership_id"] = json!("replacement");
                } else if mode == 1 {
                    let row = v["worktrees"][0].clone();
                    v["worktrees"].as_array_mut().unwrap().push(row);
                } else {
                    v["worktrees"][0]["opaque"] = json!("x".repeat(64 * 1024));
                }
                Ok(v)
            });
            let (v, step) = state.begin_manual(vec![target]).unwrap();
            state.drive_manual(v.id.clone(), step, ops);
            let v = state.manual_view(&v.id).unwrap();
            assert_eq!(v.kept, 1);
            assert!(!v.rows[0].ready);
            assert!(state
                .manual_remove(&v.id, v.revision, Consents::default())
                .is_err());
        }
    }
    #[test]
    fn unknown_is_terminal_across_check_again_and_reopening_and_post_termination_evidence() {
        let state = WorktreeCleanup::default();
        let mut ops = calls();
        ops.remove = Arc::new(|t, _| {
            if t.id == "row-0" {
                review::Removed::Unknown {
                    detail: "Unknown after issue".into(),
                }
            } else {
                refused("Verified refusal".into())
            }
        });
        let v = open(&state, 2, ops.clone());
        let (_, step) = state
            .manual_remove(&v.id, v.revision, Consents::default())
            .unwrap();
        state.drive_manual(v.id.clone(), step, ops.clone());
        let v = state.manual_view(&v.id).unwrap();
        assert_eq!((v.unknown, v.kept), (1, 1));
        let (_, step) = state.manual_check(&v.id).unwrap();
        state.drive_manual(v.id.clone(), step, ops);
        let v = state.manual_view(&v.id).unwrap();
        assert!(!v.rows[0].ready);
        assert!(v.rows[1].ready);
        let v = open(&state, 1, calls());
        assert_eq!(v.unknown, 1);
        assert!(!v.rows[0].can_check);
        let mut ops = calls();
        ops.unknown = Arc::new(|_| true);
        ops.review = Arc::new(|_| panic!("unknown evidence must not be reviewed"));
        let v = open(&state, 2, ops);
        assert_eq!(v.unknown, 2);
    }
    #[test]
    fn closing_during_an_owned_removal_finishes_it_and_keeps_the_unstarted_members() {
        let state = WorktreeCleanup::default();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut ops = calls();
        let g = gate.clone();
        let count = seen.clone();
        ops.remove = Arc::new(move |t, _| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let wait =
                g.1.wait_timeout_while(
                    g.0.lock().unwrap(),
                    std::time::Duration::from_secs(5),
                    |v| !*v,
                )
                .unwrap();
            assert!(*wait.0);
            review::Removed::Done {
                reply: json!({"status":"removed","path":t.path,"common_dir":t.common}),
            }
        });
        let v = open(&state, 3, ops.clone());
        let (_, step) = state
            .manual_remove(&v.id, v.revision, Consents::default())
            .unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| state.drive_manual(v.id.clone(), step, ops));
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while seen.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                assert!(std::time::Instant::now() < end);
                std::thread::yield_now();
            }
            assert!(state.begin_manual(vec![Target::fixture(4)]).is_err());
            {
                let mut m = lock(&state.0);
                let d = m.manual.as_mut().unwrap();
                d.closed = true;
                d.stopping = true;
                d.cleanup.stop();
            }
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            worker.join().unwrap();
        });
        let v = state.manual_view(&v.id).unwrap();
        assert_eq!((v.removed, v.kept), (1, 2));
        assert!(!v.busy);
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[test]
    fn target_and_uncertainty_caps_refuse_the_whole_action_before_delivery() {
        let state = WorktreeCleanup::default();
        assert!(state
            .begin_manual((0..65).map(Target::fixture).collect())
            .is_err());
        assert!(lock(&state.0).manual.is_none());
        let v = open(&state, 2, calls());
        {
            let mut m = lock(&state.0);
            for n in 0..63 {
                m.unknown.insert(
                    ("other-machine".into(), "/other".into(), format!("/{n}")),
                    "unconfirmed".into(),
                );
            }
        }
        assert!(state
            .manual_remove(&v.id, v.revision, Consents::default())
            .is_err());
        assert!(!state.manual_view(&v.id).unwrap().busy);
    }
}
