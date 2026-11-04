//! Bounded GUI-owned overview reads. The existing catalog/review/inspection
//! stack performs every I/O. Display rows never admit a removal or G1 capture.
use super::*;
use crate::worktree_cleanup::WorktreeCleanup;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use zerus_client_core::{creation::canonical_uuid, worktree_overview as view, ComputerId};

const CHANGED: &str = "This worktree overview changed. Open it again.";
fn locked<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    m.lock().map_err(|_| CHANGED.into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone)]
struct Binding {
    source: view::Source,
    machine: String,
}
struct Data {
    completed: Vec<bool>,
    updated: Vec<Option<u64>>,
    failed: Vec<bool>,
    owners: std::collections::BTreeMap<String, String>,
    model: view::Model,
    common: Vec<String>,
    done: usize,
    revision: u64,
}
struct Dataset {
    bindings: Vec<Binding>,
    data: Mutex<Data>,
    closed: AtomicBool,
    refresh: bool,
}
struct Window {
    id: String,
    current: Arc<Dataset>,
    pending: Option<Arc<Dataset>>,
    all: bool,
}
#[derive(Default)]
struct StateModel {
    window: Option<Window>,
    queue: VecDeque<(Arc<Dataset>, usize, Calls)>,
    inspections: usize,
    workers: usize,
}
#[derive(Clone, Default)]
pub struct Overview(Arc<Mutex<StateModel>>);
type Catalog = Arc<dyn Fn(&str, &str, bool) -> Result<Value, String> + Send + Sync>;
type Snapshot = Arc<dyn Fn(&str, &str) -> Value + Send + Sync>;
type Review = Arc<dyn Fn(&str, &str, &str) -> Result<Value, String> + Send + Sync>;
type Unknown = Arc<dyn Fn(&str, &str, &str) -> bool + Send + Sync>;
#[derive(Clone)]
struct Calls {
    catalog: Catalog,
    snapshot: Snapshot,
    review: Review,
    unknown: Unknown,
    changed: Arc<dyn Fn(&str) + Send + Sync>,
    org: Organization,
}
#[derive(Serialize)]
pub struct Header {
    id: String,
    revision: u64,
    reading: bool,
    processed: usize,
    total: usize,
    incomplete: usize,
    pending: usize,
    repositories: usize,
    updated: Option<u64>,
    partial: bool,
}
#[derive(Serialize)]
pub struct Page {
    #[serde(flatten)]
    header: Header,
    page: view::Page,
}

pub(crate) fn main(window: &tauri::Webview) -> Result<(), String> {
    if window.label() == crate::MAIN_WINDOW {
        Ok(())
    } else {
        Err("Open Worktrees from the main window.".into())
    }
}
fn sources(org: &Organization, all: bool, refresh: bool) -> Result<Arc<Dataset>, String> {
    if !org.0.allowed {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let local = org
        .0
        .pump
        .machine(&ComputerId::local())
        .and_then(|m| m.host)
        .ok_or("Waiting for this computer's identity.")?;
    let plan = {
        let hub = org.lock();
        let (value, _) = hub.sync.presented().ok_or(CHANGED)?;
        view::plan(value, &local, all)
    };
    let bindings = plan
        .sources
        .into_iter()
        .map(|source| {
            let machine = org
                .0
                .pump
                .machine(&ComputerId(source.computer.clone()))
                .and_then(|m| m.machine_id)
                .filter(|m| canonical_uuid(m))
                .unwrap_or_default();
            Binding { source, machine }
        })
        .collect::<Vec<_>>();
    let common = vec![String::new(); bindings.len()];
    let mut model = view::Model::default();
    model.partial = plan.partial;
    Ok(Arc::new(Dataset {
        bindings,
        data: Mutex::new(Data {
            completed: vec![false; common.len()],
            updated: vec![None; common.len()],
            failed: vec![false; common.len()],
            owners: Default::default(),
            model,
            common,
            done: 0,
            revision: 1,
        }),
        closed: AtomicBool::new(false),
        refresh,
    }))
}
fn current(org: &Organization, binding: &Binding, online: bool) -> Result<(), String> {
    let hub = org.lock();
    let (value, _) = hub.sync.presented().ok_or(CHANGED)?;
    if !binding
        .source
        .references
        .iter()
        .any(|r| swarm::folder_edit::original(value, &r.project, &r.folder).is_ok())
    {
        return Err(CHANGED.into());
    }
    let meta = org
        .0
        .pump
        .machine(&ComputerId(binding.source.computer.clone()))
        .ok_or(CHANGED)?;
    if !canonical_uuid(&binding.machine)
        || meta.ok == Some(false)
        || meta.machine_id.as_deref() != Some(&binding.machine)
        || binding
            .source
            .references
            .iter()
            .any(|r| !r.folder.machine_id.is_empty() && r.folder.machine_id != binding.machine)
    {
        return Err(CHANGED.into());
    }
    if online && !org.0.pump.online(&binding.source.computer) {
        return Err("Reconnect this computer to check the folder.".into());
    }
    Ok(())
}
impl Overview {
    fn start(
        &self,
        org: &Organization,
        all: bool,
        refresh: bool,
        calls: Calls,
    ) -> Result<Header, String> {
        let dataset = sources(org, all, refresh)?;
        {
            let mut state = locked(&self.0)?;
            if refresh {
                let window = state.window.as_mut().ok_or(CHANGED)?;
                if window.pending.is_some()
                    || locked(&window.current.data)?.done < window.current.bindings.len()
                {
                    return Err("Worktrees are still being read.".into());
                }
                if dataset.bindings.is_empty() {
                    window.current.closed.store(true, Ordering::Release);
                    window.current = Arc::clone(&dataset);
                } else {
                    window.pending = Some(Arc::clone(&dataset));
                }
            } else {
                if let Some(old) = state.window.take() {
                    old.current.closed.store(true, Ordering::Release);
                    if let Some(pending) = old.pending {
                        pending.closed.store(true, Ordering::Release);
                    }
                }
                state.queue.clear();
                state.window = Some(Window {
                    id: uuid::Uuid::new_v4().to_string(),
                    current: Arc::clone(&dataset),
                    pending: None,
                    all,
                });
            }
            for index in 0..dataset.bindings.len() {
                state
                    .queue
                    .push_back((Arc::clone(&dataset), index, calls.clone()));
            }
        }
        self.launch()?;
        self.header()
    }
    fn launch(&self) -> Result<(), String> {
        loop {
            {
                let mut state = locked(&self.0)?;
                if state.workers >= 2 || state.queue.is_empty() {
                    break;
                }
                state.workers += 1;
            }
            let own = self.clone();
            if std::thread::Builder::new()
                .name("worktrees-overview".into())
                .spawn(move || own.worker())
                .is_err()
            {
                let mut state = locked(&self.0)?;
                state.workers = state.workers.saturating_sub(1);
                return Err("Could not start the worktree reader.".into());
            }
        }
        Ok(())
    }
    fn worker(&self) {
        loop {
            let work = {
                let Ok(mut state) = locked(&self.0) else {
                    return;
                };
                match state.queue.pop_front() {
                    Some(work) => work,
                    None => {
                        state.workers = state.workers.saturating_sub(1);
                        return;
                    }
                }
            };
            let (dataset, index, calls) = work;
            if dataset.closed.load(Ordering::Acquire) {
                continue;
            }
            let binding = &dataset.bindings[index];
            let mut catalog = (calls.snapshot)(&binding.source.computer, &binding.source.path);
            let mut checked = None;
            let mut live = false;
            let mut failed = false;
            if !dataset.closed.load(Ordering::Acquire) && current(&calls.org, binding, true).is_ok()
            {
                match (calls.catalog)(
                    &binding.source.computer,
                    &binding.source.path,
                    dataset.refresh,
                ) {
                    Ok(value) if current(&calls.org, binding, true).is_ok() => {
                        catalog = value;
                        live = true;
                    }
                    _ => failed = true,
                }
                if live
                    && catalog["state"] == "ok"
                    && catalog["worktrees"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|r| r["kind"] == "linked"))
                {
                    if let Some(common) = catalog["common_dir"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 8192)
                    {
                        if !dataset.closed.load(Ordering::Acquire)
                            && current(&calls.org, binding, true).is_ok()
                        {
                            match (calls.review)(&binding.source.computer, &binding.machine, common)
                            {
                                Ok(value) if current(&calls.org, binding, true).is_ok() => {
                                    checked = Some(value)
                                }
                                _ => failed = true,
                            }
                        }
                    }
                }
            } else {
                failed = true;
            }
            if dataset.closed.load(Ordering::Acquire) {
                continue;
            }
            let mut roots = catalog["worktrees"]
                .as_array()
                .map(|a| a.iter().take(512).collect::<Vec<_>>())
                .unwrap_or_default();
            let fallback = serde_json::json!({"path":binding.source.path,"kind":"folder","available":catalog["state"]=="not_repo"});
            if roots.is_empty() {
                roots.push(&fallback);
            }
            let common = catalog["common_dir"].as_str().unwrap_or("");
            let mut data = match locked(&dataset.data) {
                Ok(data) => data,
                Err(_) => continue,
            };
            data.model.partial |= catalog["worktrees"]
                .as_array()
                .is_some_and(|a| a.len() > 512)
                || catalog["partial"] == true
                || checked.as_ref().is_some_and(|v| v["partial"] == true);
            let mut paths = std::collections::BTreeMap::new();
            for root in &roots {
                *paths
                    .entry(root["path"].as_str().unwrap_or(""))
                    .or_insert(0usize) += 1;
            }
            for root in roots {
                if paths.get(root["path"].as_str().unwrap_or("")) != Some(&1) {
                    data.model.partial = true;
                    continue;
                }
                let mut reviews = checked
                    .as_ref()
                    .and_then(|value| value["worktrees"].as_array())
                    .into_iter()
                    .flatten()
                    .filter(|r| r["path"] == root["path"]);
                let review = reviews.next();
                let review = if reviews.next().is_some()
                    || review.is_some_and(|r| r["kind"] != root["kind"])
                {
                    data.model.partial = true;
                    None
                } else {
                    review
                };
                let scope = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&(&binding.machine, &binding.source.computer, common))
                            .unwrap_or_default(),
                    )
                );
                if let Some(mut row) =
                    view::project(&scope, index, &binding.source, root, review, live, now())
                {
                    if (calls.unknown)(&binding.machine, common, &row.path) {
                        row.category = 3;
                        row.fresh = false;
                        row.state = "Result unknown".into();
                        row.remains = "An unknown result will not be retried.".into();
                    }
                    if let Some(previous) = data.model.row(&row.id) {
                        row.source = previous.source;
                        row.projects.extend(previous.projects.iter().cloned());
                        row.project_names
                            .extend(previous.project_names.iter().cloned());
                        row.projects.sort();
                        row.projects.dedup();
                        row.project_names.sort();
                        row.project_names.dedup();
                    }
                    let id = row.id.clone();
                    if data.model.put(row) {
                        if let Some(checked) = review {
                            data.owners.insert(id, owner(checked));
                        }
                    }
                } else {
                    data.model.partial = true;
                }
            }
            data.common[index] = common.into();
            data.completed[index] = true;
            data.updated[index] = Some(now());
            data.failed[index] = failed;
            data.done += 1;
            data.revision += 1;
            drop(data);
            let id = {
                let Ok(mut state) = locked(&self.0) else {
                    continue;
                };
                let Some(window) = &mut state.window else {
                    continue;
                };
                if !Arc::ptr_eq(&window.current, &dataset)
                    && !window
                        .pending
                        .as_ref()
                        .is_some_and(|p| Arc::ptr_eq(p, &dataset))
                {
                    continue;
                }
                if window
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &dataset))
                    && locked(&dataset.data).is_ok_and(|d| d.done == dataset.bindings.len())
                {
                    if let (Ok(old), Ok(mut next)) =
                        (locked(&window.current.data), locked(&dataset.data))
                    {
                        let ids = old
                            .model
                            .selection()
                            .into_iter()
                            .filter(|id| {
                                next.model.row(id).is_some_and(|r| r.category != 4)
                                    && (old
                                        .owners
                                        .get(id)
                                        .zip(next.owners.get(id))
                                        .is_none_or(|(a, b)| a == b))
                            })
                            .collect::<Vec<_>>();
                        next.model.replace_selection(&ids);
                        for id in ids {
                            if let Some(owner) = old.owners.get(&id) {
                                next.owners.entry(id).or_insert_with(|| owner.clone());
                            }
                        }
                    }
                    window.current.closed.store(true, Ordering::Release);
                    window.current = Arc::clone(&dataset);
                    window.pending = None;
                }
                window.id.clone()
            };
            (calls.changed)(&id);
        }
    }
    fn header(&self) -> Result<Header, String> {
        self.scoped_header(None)
    }
    fn scoped_header(&self, filter: Option<&view::Filter>) -> Result<Header, String> {
        let state = locked(&self.0)?;
        let window = state.window.as_ref().ok_or(CHANGED)?;
        let dataset = window.pending.as_ref().unwrap_or(&window.current);
        let data = locked(&dataset.data)?;
        let scoped: Vec<_> = dataset
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| {
                filter.is_none_or(|f| {
                    (f.project.is_empty()
                        || binding
                            .source
                            .references
                            .iter()
                            .any(|r| r.project == f.project))
                        && (f.computer.is_empty() || binding.source.machine == f.computer)
                })
            })
            .map(|(i, _)| i)
            .collect();
        let repositories = scoped
            .iter()
            .filter(|i| !data.common[**i].is_empty())
            .map(|i| (&dataset.bindings[*i].machine, &data.common[*i]))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let processed = scoped.iter().filter(|i| data.completed[**i]).count();
        let incomplete = scoped.iter().filter(|i| data.failed[**i]).count();
        let total = scoped.len();
        Ok(Header {
            id: window.id.clone(),
            revision: data.revision,
            reading: window.pending.is_some() || data.done < dataset.bindings.len(),
            processed,
            total,
            incomplete,
            pending: total.saturating_sub(processed),
            repositories,
            updated: scoped.iter().filter_map(|i| data.updated[*i]).max(),
            partial: data.model.partial,
        })
    }
    fn page(&self, id: &str, filter: &view::Filter, index: usize) -> Result<Page, String> {
        let header = self.scoped_header(Some(filter))?;
        if header.id != id {
            return Err(CHANGED.into());
        }
        let state = locked(&self.0)?;
        let window = state
            .window
            .as_ref()
            .filter(|w| w.id == id)
            .ok_or(CHANGED)?;
        let data = locked(&window.current.data)?;
        Ok(Page {
            header,
            page: data
                .model
                .page(filter, index, now())
                .map_err(str::to_owned)?,
        })
    }
    pub(crate) fn targets(&self, id: &str) -> Result<Vec<Target>, String> {
        let state = locked(&self.0)?;
        let window = state
            .window
            .as_ref()
            .filter(|w| w.id == id && w.pending.is_none())
            .ok_or(CHANGED)?;
        let data = locked(&window.current.data)?;
        if data.done < window.current.bindings.len() {
            return Err(CHANGED.into());
        }
        let ids = data.model.cleanup_selection().map_err(str::to_owned)?;
        if ids.is_empty() {
            return Err("Select worktrees to review.".into());
        }
        ids.into_iter()
            .map(|id| {
                let row = data.model.row(&id).ok_or(CHANGED)?;
                let common = data
                    .common
                    .get(row.source)
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .ok_or(CHANGED)?;
                Ok(Target {
                    id,
                    binding: window
                        .current
                        .bindings
                        .get(row.source)
                        .cloned()
                        .ok_or(CHANGED)?,
                    path: row.path.clone(),
                    branch: row.branch.clone(),
                    common,
                    owner: data.owners.get(&row.id).cloned(),
                })
            })
            .collect()
    }
    pub(crate) fn inspection(&self) -> Result<Inspection, String> {
        let mut state = locked(&self.0)?;
        if state.inspections >= 2 {
            return Err("A worktree inspection is still running.".into());
        }
        state.inspections += 1;
        Ok(Inspection(self.clone()))
    }
    fn close(&self, id: &str) -> Result<(), String> {
        let mut state = locked(&self.0)?;
        if state.window.as_ref().is_none_or(|w| w.id != id) {
            return Err(CHANGED.into());
        }
        let window = state.window.take().ok_or(CHANGED)?;
        window.current.closed.store(true, Ordering::Release);
        if let Some(pending) = window.pending {
            pending.closed.store(true, Ordering::Release);
        }
        state.queue.clear();
        Ok(())
    }
}

/// Fixed-size evidence of the exact observed checkout nonce, including absence.
pub(crate) fn owner(row: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&row["ownership_id"]).unwrap_or_default())
    )
}

#[derive(Clone)]
pub(crate) struct Target {
    pub id: String,
    binding: Binding,
    pub path: String,
    pub branch: String,
    pub common: String,
    pub owner: Option<String>,
}
impl Target {
    #[cfg(test)]
    pub(crate) fn fixture(index: usize) -> Self {
        let path = format!("/srv/example/work-{index}");
        Self {
            id: format!("row-{index}"),
            path,
            branch: format!("branch-{index}"),
            common: "/srv/example/.git".into(),
            owner: None,
            binding: Binding {
                machine: "11111111-1111-4111-8111-111111111111".into(),
                source: view::Source {
                    computer: String::new(),
                    machine: "example-host".into(),
                    path: "/srv/example".into(),
                    references: Vec::new(),
                },
            },
        }
    }
    pub fn computer(&self) -> &str {
        &self.binding.source.computer
    }
    pub fn machine(&self) -> &str {
        &self.binding.machine
    }
    pub fn label(&self) -> &str {
        &self.binding.source.machine
    }
    pub fn check(&self, org: &Organization) -> Result<(), String> {
        current(org, &self.binding, true)
    }
}

pub(crate) struct Inspection(Overview);
impl Drop for Inspection {
    fn drop(&mut self) {
        if let Ok(mut state) = locked(&self.0 .0) {
            state.inspections = state.inspections.saturating_sub(1);
        }
    }
}

fn calls(
    org: Organization,
    catalogs: crate::newsession::NewSession,
    cleanup: WorktreeCleanup,
    app: tauri::AppHandle,
) -> Calls {
    use tauri::{Emitter, Manager};
    let snapshots = catalogs.clone();
    let outcomes = app.clone();
    Calls {
        org,
        catalog: Arc::new(move |computer, path, refresh| {
            catalogs.catalog(&ComputerId(computer.into()), path, refresh)
        }),
        snapshot: Arc::new(move |computer, path| {
            snapshots.snapshot(&ComputerId(computer.into()), path)
        }),
        review: Arc::new(|computer, machine, common| {
            crate::worktree_cleanup::read(computer, machine, common, "")
        }),
        unknown: Arc::new(move |machine, common, path| {
            cleanup.unknown(machine, common, path)
                || outcomes
                    .state::<crate::termination_cleanup::TerminationCleanup>()
                    .unknown(machine, common, path)
        }),
        changed: Arc::new(move |id| {
            let _ = app.emit("worktrees-overview-changed", serde_json::json!({"id":id}));
        }),
    }
}
#[tauri::command]
pub async fn worktree_overview_open(
    window: tauri::Webview,
    app: tauri::AppHandle,
    state: tauri::State<'_, Overview>,
    org: tauri::State<'_, Organization>,
    catalogs: tauri::State<'_, crate::newsession::NewSession>,
    cleanup: tauri::State<'_, WorktreeCleanup>,
    all: bool,
) -> Result<Header, String> {
    main(&window)?;
    let (own, org) = (state.inner().clone(), org.inner().clone());
    let calls = calls(
        org.clone(),
        catalogs.inner().clone(),
        cleanup.inner().clone(),
        app,
    );
    tauri::async_runtime::spawn_blocking(move || own.start(&org, all, false, calls))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn worktree_overview_refresh(
    window: tauri::Webview,
    app: tauri::AppHandle,
    state: tauri::State<'_, Overview>,
    org: tauri::State<'_, Organization>,
    catalogs: tauri::State<'_, crate::newsession::NewSession>,
    cleanup: tauri::State<'_, WorktreeCleanup>,
    id: String,
) -> Result<Header, String> {
    main(&window)?;
    let own = state.inner().clone();
    let org = org.inner().clone();
    let calls = calls(
        org.clone(),
        catalogs.inner().clone(),
        cleanup.inner().clone(),
        app,
    );
    tauri::async_runtime::spawn_blocking(move || {
        let all = {
            let state = locked(&own.0)?;
            state
                .window
                .as_ref()
                .filter(|w| w.id == id)
                .ok_or(CHANGED)?
                .all
        };
        own.start(&org, all, true, calls)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn worktree_overview_page(
    window: tauri::Webview,
    state: tauri::State<'_, Overview>,
    id: String,
    filter: view::Filter,
    index: usize,
) -> Result<Page, String> {
    main(&window)?;
    let own = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || own.page(&id, &filter, index))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn worktree_overview_select(
    window: tauri::Webview,
    state: tauri::State<'_, Overview>,
    id: String,
    ids: Vec<String>,
    replace: bool,
) -> Result<(), String> {
    main(&window)?;
    if ids.len() > view::ROWS
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(CHANGED.into());
    }
    let own = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = locked(&own.0)?;
        let window = state
            .window
            .as_ref()
            .filter(|w| w.id == id)
            .ok_or(CHANGED)?;
        let mut data = locked(&window.current.data)?;
        if ids
            .iter()
            .any(|id| data.model.row(id).is_none_or(|r| r.category == 4))
        {
            return Err(CHANGED.into());
        }
        if replace {
            data.model.replace_selection(&ids);
        } else {
            for id in ids {
                let next = !data.model.is_selected(&id);
                data.model.select(&id, next);
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn worktree_overview_candidates(
    window: tauri::Webview,
    state: tauri::State<'_, Overview>,
    id: String,
    filter: view::Filter,
) -> Result<(), String> {
    main(&window)?;
    let own = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = locked(&own.0)?;
        let window = state
            .window
            .as_ref()
            .filter(|w| w.id == id && w.pending.is_none())
            .ok_or(CHANGED)?;
        let mut data = locked(&window.current.data)?;
        if data.done < window.current.bindings.len() {
            return Err(CHANGED.into());
        }
        data.model
            .select_candidates(&filter, now())
            .map_err(str::to_owned)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn worktree_overview_close(
    window: tauri::Webview,
    state: tauri::State<'_, Overview>,
    id: String,
) -> Result<(), String> {
    main(&window)?;
    state.close(&id)
}
#[tauri::command]
pub async fn worktree_overview_inspect(
    window: tauri::Webview,
    state: tauri::State<'_, Overview>,
    org: tauri::State<'_, Organization>,
    id: String,
    row: String,
    file: String,
    offset: usize,
) -> Result<Value, String> {
    main(&window)?;
    let own = state.inner().clone();
    let org = org.inner().clone();
    let slot = own.inspection()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _slot = slot;
        let (binding, path, common, dataset) = {
            let state = locked(&own.0)?;
            let window = state
                .window
                .as_ref()
                .filter(|w| w.id == id)
                .ok_or(CHANGED)?;
            let data = locked(&window.current.data)?;
            let row = data.model.row(&row).ok_or(CHANGED)?;
            (
                window
                    .current
                    .bindings
                    .get(row.source)
                    .cloned()
                    .ok_or(CHANGED)?,
                row.path.clone(),
                data.common.get(row.source).cloned().ok_or(CHANGED)?,
                Arc::clone(&window.current),
            )
        };
        current(&org, &binding, true)?;
        let value = crate::worktree_cleanup::inspect(
            &binding.source.computer,
            &binding.machine,
            &path,
            &common,
            &file,
            offset,
        )?;
        current(&org, &binding, true)?;
        if dataset.closed.load(Ordering::Acquire) {
            return Err(CHANGED.into());
        }
        Ok(value)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    const MACHINE: &str = "11111111-1111-4111-8111-111111111111";
    struct Fixture {
        org: Organization,
        dir: PathBuf,
    }
    impl Fixture {
        fn new(count: usize) -> Self {
            let dir = std::env::temp_dir().join(format!("zerus-overview-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let org = Organization::new(
                Pump::default(),
                dir.join("never-called-hgs"),
                true,
                dir.join("layout"),
                dir.join("draft"),
                false,
            );
            let this = Self { org, dir };
            this.machine(MACHINE);
            this.folders(count);
            this
        }
        fn machine(&self, machine: &str) {
            self.org.0.pump.push(Change::Machine {
                computer: ComputerId::local(),
                machine: zerus_client_core::MachineMeta {
                    host: Some("example-host".into()),
                    machine_id: Some(machine.into()),
                    ok: Some(true),
                    ..Default::default()
                },
            });
        }
        fn folders(&self, count: usize) {
            let value = json!({"schema":1,"initialized":true,"node_id":"example","digest":uuid::Uuid::new_v4().to_string(),"organization":{"version":2,"projects":[{"id":"alpha","name":"Alpha","accessible":true,"folders":(0..count).map(|n|json!({"id":format!("f{n}"),"machine":"example-host","path":format!("/srv/example/{n}")})).collect::<Vec<_>>()}]}});
            assert!(self
                .org
                .lock()
                .sync
                .observe(Observation::from_reply(value).unwrap(), 0));
        }
        fn calls(&self) -> Calls {
            Calls {
                org: self.org.clone(),
                snapshot: Arc::new(|_, _| Value::Null),
                catalog: Arc::new(|_, path, _| Ok(catalog(path))),
                review: Arc::new(|_, _, path| Ok(review(path, "owner-1"))),
                unknown: Arc::new(|_, _, _| false),
                changed: Arc::new(|_| {}),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.org.flush();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn catalog(path: &str) -> Value {
        json!({"state":"ok","common_dir":format!("{path}/.git"),"worktrees":[{"path":path,"kind":"main","available":true},{"path":format!("{path}/linked"),"kind":"linked","available":true,"branch":"example"}]})
    }
    fn review(common: &str, owner: &str) -> Value {
        json!({"worktrees":[{"path":format!("{}/linked",common.trim_end_matches("/.git")),"kind":"linked","available":true,"verdict":"ready","fingerprint":"original-fingerprint","ownership_id":owner,"changes":0,"untracked":0,"integration":{"kind":"ancestor","base":"main"}}]})
    }
    fn until(mut ready: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < end, "overview did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn settled(own: &Overview) {
        until(|| own.header().is_ok_and(|h| !h.reading));
    }
    fn linked(own: &Overview, id: &str) -> Vec<view::Row> {
        own.page(id, &view::Filter::default(), 0)
            .unwrap()
            .page
            .groups
            .into_iter()
            .flat_map(|g| g.rows)
            .filter(|r| r.kind == "linked")
            .collect()
    }
    fn selected(own: &Overview, id: &str) {
        let state = locked(&own.0).unwrap();
        let w = state.window.as_ref().unwrap();
        locked(&w.current.data).unwrap().model.select(id, true);
    }
    #[test]
    fn default_is_idle_and_live_reads_are_bounded_across_close_and_reopen() {
        let f = Fixture::new(4);
        let own = Overview::default();
        assert_eq!(locked(&own.0).unwrap().workers, 0);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let seen = Arc::new(AtomicUsize::new(0));
        let mut calls = f.calls();
        let g = gate.clone();
        let count = seen.clone();
        calls.catalog = Arc::new(move |_, path, _| {
            count.fetch_add(1, Ordering::SeqCst);
            let (m, c) = &*g;
            let wait = c
                .wait_timeout_while(m.lock().unwrap(), Duration::from_secs(5), |open| !*open)
                .unwrap();
            assert!(*wait.0);
            Ok(catalog(path))
        });
        let first = own.start(&f.org, false, false, calls).unwrap();
        until(|| seen.load(Ordering::SeqCst) == 2);
        own.close(&first.id).unwrap();
        let new_count = Arc::new(AtomicUsize::new(0));
        let next = new_count.clone();
        let mut calls = f.calls();
        calls.catalog = Arc::new(move |_, path, _| {
            next.fetch_add(1, Ordering::SeqCst);
            Ok(catalog(path))
        });
        let second = own.start(&f.org, false, false, calls).unwrap();
        assert_eq!(locked(&own.0).unwrap().workers, 2);
        assert_eq!(new_count.load(Ordering::SeqCst), 0);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        settled(&own);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(new_count.load(Ordering::SeqCst), 4);
        assert_eq!(linked(&own, &second.id).len(), 4);
        assert!(own.page(&first.id, &view::Filter::default(), 0).is_err());
        own.close(&second.id).unwrap();
        until(|| locked(&own.0).unwrap().workers == 0);
    }
    #[test]
    fn physical_or_original_folder_drift_refuses_new_reads_and_live_results() {
        let f = Fixture::new(1);
        let dataset = sources(&f.org, false, false).unwrap();
        let b = &dataset.bindings[0];
        f.folders(0);
        assert!(current(&f.org, b, true).is_err());
        f.folders(1);
        f.machine("22222222-2222-4222-8222-222222222222");
        assert!(current(&f.org, b, true).is_err());
        let own = Overview::default();
        let mut calls = f.calls();
        let org = f.org.clone();
        calls.catalog = Arc::new(move |_, path, _| {
            org.0.pump.push(Change::Machine {
                computer: ComputerId::local(),
                machine: zerus_client_core::MachineMeta {
                    host: Some("example-host".into()),
                    machine_id: Some(MACHINE.into()),
                    ..Default::default()
                },
            });
            Ok(catalog(path))
        });
        let head = own.start(&f.org, false, false, calls).unwrap();
        settled(&own);
        assert_eq!(own.header().unwrap().incomplete, 1);
        assert!(linked(&own, &head.id).is_empty());
    }
    #[test]
    fn refresh_retains_exact_survivors_but_replacement_ownership_and_empty_catalog_drop_selection()
    {
        let f = Fixture::new(2);
        let own = Overview::default();
        let head = own.start(&f.org, false, false, f.calls()).unwrap();
        settled(&own);
        let rows = linked(&own, &head.id);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            selected(&own, &row.id);
        }
        f.folders(1);
        own.start(&f.org, false, true, f.calls()).unwrap();
        settled(&own);
        assert_eq!(
            own.page(&head.id, &view::Filter::default(), 0)
                .unwrap()
                .page
                .selected,
            1
        );
        let mut calls = f.calls();
        calls.review = Arc::new(|_, _, path| Ok(review(path, "replacement-owner")));
        own.start(&f.org, false, true, calls).unwrap();
        settled(&own);
        assert_eq!(
            own.page(&head.id, &view::Filter::default(), 0)
                .unwrap()
                .page
                .selected,
            0
        );
        f.folders(0);
        own.start(&f.org, false, true, f.calls()).unwrap();
        settled(&own);
        assert_eq!(
            own.page(&head.id, &view::Filter::default(), 0)
                .unwrap()
                .page
                .total,
            0
        );
    }
    #[test]
    fn unknown_outcomes_never_become_candidates_and_duplicate_reviews_are_not_trusted() {
        let f = Fixture::new(1);
        let own = Overview::default();
        let mut calls = f.calls();
        calls.unknown = Arc::new(|_, _, _| true);
        let head = own.start(&f.org, false, false, calls).unwrap();
        settled(&own);
        let rows = linked(&own, &head.id);
        assert_eq!(rows[0].category, 3);
        assert!(!rows[0].fresh);
        let mut calls = f.calls();
        calls.review = Arc::new(|_, _, path| {
            let mut v = review(path, "owner");
            let duplicate = v["worktrees"][0].clone();
            v["worktrees"].as_array_mut().unwrap().push(duplicate);
            Ok(v)
        });
        own.start(&f.org, false, true, calls).unwrap();
        settled(&own);
        assert!(own.header().unwrap().partial);
        assert_eq!(linked(&own, &head.id)[0].category, 3);
        let mut calls = f.calls();
        calls.catalog = Arc::new(|_, path, _| {
            let mut v = catalog(path);
            let duplicate = v["worktrees"][1].clone();
            v["worktrees"].as_array_mut().unwrap().push(duplicate);
            Ok(v)
        });
        own.start(&f.org, false, true, calls).unwrap();
        settled(&own);
        assert!(own.header().unwrap().partial);
        assert!(linked(&own, &head.id).is_empty());
    }
    #[test]
    fn ownership_index_only_keeps_admitted_rows_and_fixed_size_nonce_digests() {
        let f = Fixture::new(17);
        let own = Overview::default();
        let mut calls = f.calls();
        calls.catalog = Arc::new(|_, path, _| {
            Ok(
                json!({"state":"ok","common_dir":format!("{path}/.git"),"worktrees":(0..512).map(|i|json!({"path":format!("{path}/work-{i}"),"kind":"linked","available":true})).collect::<Vec<_>>()}),
            )
        });
        calls.review = Arc::new(|_, _, common| {
            Ok(
                json!({"worktrees":(0..512).map(|i|json!({"path":format!("{}/work-{i}",common.trim_end_matches("/.git")),"kind":"linked","verdict":"ready","fingerprint":"proof","ownership_id":"owner"})).collect::<Vec<_>>()}),
            )
        });
        own.start(&f.org, false, false, calls).unwrap();
        settled(&own);
        let state = locked(&own.0).unwrap();
        let data = locked(&state.window.as_ref().unwrap().current.data).unwrap();
        assert!(data.model.partial);
        assert_eq!(data.owners.len(), view::ROWS);
        assert!(data.owners.values().all(|v| v.len() == 64));
        assert_eq!(
            owner(&json!({"ownership_id":"x".repeat(64*1024)})).len(),
            64
        );
    }
    #[test]
    fn inspection_slots_remain_charged_after_the_window_closes() {
        let f = Fixture::new(0);
        let own = Overview::default();
        let head = own.start(&f.org, false, false, f.calls()).unwrap();
        let a = own.inspection().unwrap();
        let b = own.inspection().unwrap();
        own.close(&head.id).unwrap();
        own.start(&f.org, false, false, f.calls()).unwrap();
        assert!(own.inspection().is_err());
        drop(a);
        assert!(own.inspection().is_ok());
        drop(b);
        assert_eq!(locked(&own.0).unwrap().inspections, 0);
    }
}
