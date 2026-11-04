//! Project swarm uses the existing organization worker; one bounded dialog/job.
//! No extra poller, writer, source store or automatic Join/Resolve retry.
pub mod conflicts;
use super::*;
use serde::Serialize;
use std::sync::atomic::AtomicBool;
use zerus_client_core::{
    reads,
    swarm::{
        connection, dialog,
        sync::{ExternalTicket, EXTERNAL_CHANGED},
    },
    ComputerId, Invocation,
};

const EXPIRED: &str = "This project swarm dialog expired. Open it again.";
const BUSY: &str = "A project swarm request is already running.";

#[derive(Serialize)]
pub struct View {
    id: String,
    catalog: Option<dialog::CatalogView>,
    projects: Vec<connection::Project>,
    aliases: Vec<String>,
    preview: Option<PreviewView>,
    status: String,
    error: bool,
    busy: bool,
    changing: bool,
    can_sync: bool,
    can_join: bool,
    mode: &'static str,
    can_preview: bool,
}
#[derive(Serialize)]
struct PreviewView {
    id: String,
    alias: String,
    initialized: bool,
    same_swarm: bool,
    projects: Vec<connection::Project>,
}
/// Last actual child/input/output charge only wakes the same worker. It never
/// locks the hub, emits private data or establishes service drain authority.
struct Resources(Weak<Shared>);
impl Drop for Resources {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.wake.notify_all();
        }
    }
}
#[derive(Clone)]
pub(super) enum Action {
    Config,
    Preview(String),
    Join {
        mappings: Vec<(String, String)>,
        keep: bool,
    },
    Sync,
    Resolve {
        review: String,
        confirmed: bool,
    },
}
enum Step {
    Config,
    PreviewConfig(String),
    Preview(String, String, Arc<Value>),
    JoinConfig,
    JoinLocal,
    SyncLocal,
    ResolveLocal,
    Mutation,
    Fresh,
}
struct Work {
    step: Step,
    invocation: Invocation,
}
enum Stage {
    Idle,
    Queued(Work),
    Running,
    Draining {
        work: Work,
        result: Result<Exit, String>,
        resources: Weak<Resources>,
    },
    Source,
}
pub(super) struct Context {
    id: String,
    visible: bool,
    disabled: bool,
    stage: Stage,
    aliases: Vec<String>,
    preview: Option<connection::Preview>,
    preview_key: String,
    mode: &'static str,
    configuration: String,
    status: String,
    error: bool,
    changing: bool,
    ticket: Option<ExternalTicket>,
    expected: Option<Observation>,
    outcome: Option<Result<String, String>>,
    after: Option<Action>,
    join: Option<(Vec<(String, String)>, bool)>,
    review_reading: bool,
    review: Option<conflicts::Selected>,
    resolving: Option<conflicts::Selected>,
}
impl Context {
    fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            visible: true,
            disabled: false,
            stage: Stage::Idle,
            aliases: Vec::new(),
            preview: None,
            preview_key: String::new(),
            mode: "config",
            configuration: String::new(),
            status: String::new(),
            error: false,
            changing: false,
            ticket: None,
            expected: None,
            outcome: None,
            after: None,
            join: None,
            review_reading: false,
            review: None,
            resolving: None,
        }
    }
    fn active(&self) -> bool {
        self.review_reading || !matches!(self.stage, Stage::Idle | Stage::Source)
    }
    fn view(&self, hub: &Hub) -> Result<View, String> {
        let snapshot = hub.sync.observed_value();
        let catalog = snapshot
            .as_deref()
            .map(|s| dialog::view(s, hub.sync.revision()))
            .transpose()?;
        let projects = snapshot
            .as_deref()
            .map(connection::local_projects)
            .transpose()?
            .unwrap_or_default();
        let can_sync = !self.disabled
            && hub.sync.ready()
            && !hub.sync.busy()
            && !hub.sync.pending()
            && hub.sync.next_due().is_none()
            && !hub.presentation_due
            && hub.recovery_due.is_none()
            && !self.active()
            && snapshot
                .as_deref()
                .is_some_and(|s| s["initialized"] == true);
        Ok(View {
            id: self.id.clone(),
            catalog,
            projects,
            aliases: self.aliases.clone(),
            preview: self.preview.as_ref().map(|p| PreviewView {
                id: self.preview_key.clone(),
                alias: p.alias().into(),
                initialized: p.initialized(),
                same_swarm: p.same_swarm(),
                projects: p.projects().to_vec(),
            }),
            status: self.status.clone(),
            error: self.error,
            busy: self.active(),
            changing: self.changing && matches!(self.stage, Stage::Queued(_) | Stage::Running),
            can_sync,
            can_join: can_sync
                && self.preview.as_ref().is_some_and(|p| {
                    p.initialized() && snapshot.as_deref().is_some_and(|s| p.matches_local(s))
                }),
            mode: self.mode,
            can_preview: !self.disabled && !self.active() && !self.aliases.is_empty(),
        })
    }
}

fn read_work(step: Step, read: Read) -> Result<Work, String> {
    Ok(Work {
        step,
        invocation: reads::invocation(&ComputerId::local(), &read).map_err(|e| e.to_string())?,
    })
}
fn object(exit: Exit) -> Result<Value, String> {
    match exit {
        Exit::Code {
            code: 0, stdout, ..
        } => serde_json::from_slice::<Value>(&stdout)
            .map_err(|_| connection::INVALID.to_owned())
            .and_then(|v| {
                if v.is_object() {
                    Ok(v)
                } else {
                    Err(connection::INVALID.into())
                }
            }),
        Exit::Code { stderr, .. } | Exit::Abnormal { stderr } => Err(if stderr.trim().is_empty() {
            connection::INVALID.into()
        } else {
            stderr.chars().take(4000).collect()
        }),
        Exit::TimedOut => Err(ReadError::TimedOut.to_string()),
        Exit::StartFailed(message) => Err(message.chars().take(4000).collect()),
        Exit::WriteFailed => Err("hgs did not take the request".into()),
    }
}
impl Context {
    fn queue(&mut self, hub: &mut Hub, action: Action) -> Result<(), String> {
        if self.active() {
            return Err(BUSY.into());
        }
        if self.ticket.is_some() {
            if matches!(action, Action::Join { .. } | Action::Resolve { .. }) {
                return Err(EXTERNAL_CHANGED.into());
            }
            self.after = Some(action);
            self.expected = None;
            self.stage = Stage::Queued(read_work(Step::Fresh, Read::Swarm)?);
            self.status = "Reading the project catalog…".into();
            self.mode = "reconcile";
            return Ok(());
        }
        if matches!(
            action,
            Action::Join { .. } | Action::Sync | Action::Resolve { .. }
        ) && (hub.presentation_due || hub.recovery_due.is_some())
        {
            return Err(swarm::sync::EXTERNAL_BUSY.into());
        }
        self.error = false;
        match action {
            Action::Config => {
                self.mode = "config";
                self.stage = Stage::Queued(read_work(Step::Config, Read::Machines)?);
            }
            Action::Preview(alias) => {
                connection::preview_invocation(&alias)?;
                if !self.aliases.contains(&alias) {
                    return Err(EXTERNAL_CHANGED.into());
                }
                self.preview = None;
                self.mode = "preview";
                self.status = "Reading the other project catalog…".into();
                self.stage = Stage::Queued(read_work(Step::PreviewConfig(alias), Read::Machines)?);
            }
            Action::Join { mappings, keep } => {
                let snapshot = hub.sync.observed_value().ok_or(EXTERNAL_CHANGED)?;
                self.preview
                    .as_ref()
                    .ok_or(EXTERNAL_CHANGED)?
                    .join(&snapshot, &mappings, keep)?;
                let generation = hub.source_generation.checked_add(1).ok_or(STOPPED)?;
                self.ticket = Some(hub.sync.begin_external(hub.sync.revision())?);
                hub.source_generation = generation;
                self.join = Some((mappings, keep));
                self.changing = true;
                self.mode = "join";
                self.status = "Connecting and merging catalogs…".into();
                self.stage = Stage::Queued(read_work(Step::JoinConfig, Read::Machines)?);
            }
            Action::Sync => {
                let generation = hub.source_generation.checked_add(1).ok_or(STOPPED)?;
                self.ticket = Some(hub.sync.begin_external(hub.sync.revision())?);
                hub.source_generation = generation;
                self.join = None;
                self.preview = None;
                self.changing = true;
                self.mode = "sync";
                self.status = "Synchronizing projects…".into();
                self.stage = Stage::Queued(read_work(Step::SyncLocal, Read::Swarm)?);
            }
            Action::Resolve { review, confirmed } => {
                let selected = self
                    .review
                    .as_ref()
                    .filter(|r| r.id == review)
                    .ok_or(EXTERNAL_CHANGED)?;
                let snapshot = hub.sync.observed_value().ok_or(EXTERNAL_CHANGED)?;
                selected.invocation(&snapshot, confirmed)?;
                let generation = hub.source_generation.checked_add(1).ok_or(STOPPED)?;
                self.ticket = Some(hub.sync.begin_external(hub.sync.revision())?);
                hub.source_generation = generation;
                self.resolving = self.review.take();
                self.join = None;
                self.preview = None;
                self.changing = true;
                self.mode = "resolve";
                self.status = "Saving your choice to the shared catalog…".into();
                self.stage = Stage::Queued(read_work(Step::ResolveLocal, Read::Swarm)?);
            }
        }
        Ok(())
    }
    fn failed(&mut self, error: String) {
        self.status = error;
        self.error = true;
        self.changing = false;
        self.stage = Stage::Idle;
        if self.ticket.is_none() {
            self.preview = None;
            self.join = None;
            self.resolving = None;
        }
    }
    fn result(
        &mut self,
        hub: &Hub,
        step: Step,
        result: Result<Exit, String>,
    ) -> Result<(), String> {
        let exit = result?;
        match step {
            Step::Config => {
                let config = reads::decode(&Read::Machines, exit).map_err(|e| e.to_string())?;
                self.aliases = connection::configured_aliases(&config)?;
                self.stage = Stage::Idle;
            }
            Step::PreviewConfig(alias) => {
                let config = reads::decode(&Read::Machines, exit).map_err(|e| e.to_string())?;
                let revision = connection::configuration_revision(&config, &alias)?;
                let local = hub.sync.observed_value().ok_or(EXTERNAL_CHANGED)?;
                self.aliases = connection::configured_aliases(&config)?;
                self.stage = Stage::Queued(Work {
                    step: Step::Preview(alias.clone(), revision, local),
                    invocation: connection::preview_invocation(&alias)?,
                });
            }
            Step::Preview(alias, revision, local) => {
                let reply = object(exit)?;
                let preview = connection::Preview::checked(&alias, &local, &reply)?;
                self.status = if !preview.initialized() {
                    format!("Open Projects in Zerus on {alias} first to import its catalog.")
                } else if preview.same_swarm() {
                    format!("Ready to connect to {alias}.")
                } else {
                    format!("Review the project mapping, then connect to {alias}.")
                };
                self.configuration = revision;
                self.preview = Some(preview);
                self.preview_key = uuid::Uuid::new_v4().to_string();
                self.stage = Stage::Idle;
            }
            Step::JoinConfig => {
                let config = reads::decode(&Read::Machines, exit).map_err(|e| e.to_string())?;
                let preview = self.preview.as_ref().ok_or(EXTERNAL_CHANGED)?;
                if connection::configuration_revision(&config, preview.alias())?
                    != self.configuration
                {
                    return Err(zerus_client_core::machine_connections::CHANGED.into());
                }
                self.stage = Stage::Queued(read_work(Step::JoinLocal, Read::Swarm)?);
            }
            Step::JoinLocal => {
                let local = reads::decode(&Read::Swarm, exit).map_err(|e| e.to_string())?;
                let (mapping, keep) = self.join.as_ref().ok_or(EXTERNAL_CHANGED)?;
                self.stage = Stage::Queued(Work {
                    step: Step::Mutation,
                    invocation: self
                        .preview
                        .as_ref()
                        .ok_or(EXTERNAL_CHANGED)?
                        .join(&local, mapping, *keep)?,
                });
            }
            Step::SyncLocal => {
                let local = reads::decode(&Read::Swarm, exit).map_err(|e| e.to_string())?;
                let ticket = self.ticket.as_ref().ok_or(EXTERNAL_CHANGED)?;
                if local["schema"] != 1
                    || local["node_id"] != ticket.snapshot()["node_id"]
                    || local["swarm_id"] != ticket.snapshot()["swarm_id"]
                    || local["initialized"] != true
                {
                    return Err(EXTERNAL_CHANGED.into());
                }
                self.stage = Stage::Queued(Work {
                    step: Step::Mutation,
                    invocation: connection::sync_invocation(),
                });
            }
            Step::ResolveLocal => {
                let local = reads::decode(&Read::Swarm, exit).map_err(|e| e.to_string())?;
                self.stage = Stage::Queued(Work {
                    step: Step::Mutation,
                    invocation: self
                        .resolving
                        .as_ref()
                        .ok_or(EXTERNAL_CHANGED)?
                        .invocation(&local, true)?,
                });
            }
            Step::Mutation => {
                self.outcome=Some(object(exit).and_then(|reply| {
                    if self.join.is_some() {
                        if reply["joined"] != true || !reply["snapshot"].is_object() {return Err(connection::INVALID.into());}
                        Ok(format!("Connected to {}.",self.preview.as_ref().ok_or(EXTERNAL_CHANGED)?.alias()))
                    }else if self.resolving.is_some() {
                        if reply["schema"] != 1 || !reply["conflicts"].is_array() {return Err(connection::INVALID.into());}
                        let ticket=self.ticket.as_ref().ok_or(EXTERNAL_CHANGED)?;
                        if reply["node_id"] != ticket.snapshot()["node_id"] || reply["swarm_id"] != ticket.snapshot()["swarm_id"] {return Err(connection::INVALID.into());}
                        self.resolving.as_ref().unwrap().saved_status(&reply)
                    }else{Ok("Synchronization finished. Connection details are on the Computers tab.".into())}
                }));
                self.stage = Stage::Queued(read_work(Step::Fresh, Read::Swarm)?);
            }
            Step::Fresh => {
                let value = reads::decode(&Read::Swarm, exit).map_err(|e| e.to_string())?;
                let ticket = self.ticket.as_ref().ok_or(EXTERNAL_CHANGED)?;
                if value["node_id"] != ticket.snapshot()["node_id"] || value["initialized"] != true
                {
                    return Err(EXTERNAL_CHANGED.into());
                }
                if self.join.is_some()
                    && self.outcome.as_ref().is_some_and(Result::is_ok)
                    && !self.preview.as_ref().is_some_and(|p| p.joined_in(&value))
                {
                    self.outcome = Some(Err(connection::INVALID.into()));
                }
                if self.outcome.as_ref().is_some_and(Result::is_ok) {
                    if let Some(review) = &self.resolving {
                        self.outcome = Some(review.saved_status(&value));
                    }
                }
                self.expected = Some(Observation::from_reply(value).ok_or(connection::INVALID)?);
                self.stage = Stage::Source;
                self.changing = false;
            }
        }
        Ok(())
    }
}

impl Organization {
    fn catalog_lock(&self) -> Result<MutexGuard<'_, Hub>, String> {
        self.0.hub.lock().map_err(|_| STOPPED.into())
    }
    pub(super) fn catalog_request(
        &self,
        id: Option<&str>,
        action: Option<Action>,
        close: bool,
    ) -> Result<View, String> {
        let mut hub = self.catalog_lock()?;
        if hub.stopped {
            return Err(STOPPED.into());
        }
        if id.is_none()
            && hub
                .catalog
                .as_ref()
                .is_none_or(|c| !c.visible && !c.active() && c.ticket.is_none())
        {
            let mut context = Context::new();
            context.disabled = !self.0.allowed;
            if self.0.allowed {
                context.queue(&mut hub, Action::Config)?;
            } else {
                context.status = REPLAY_NEEDS_HGS.into();
                context.error = true;
            }
            hub.catalog = Some(context);
        }
        let current = hub.catalog.as_ref().ok_or(EXPIRED)?;
        if id.is_some_and(|id| id != current.id) {
            return Err(EXPIRED.into());
        }
        // A status pull must not announce itself: the event subscriber would
        // otherwise turn this read into an unbounded view/event feedback loop.
        if id.is_some() && action.is_none() && !close {
            return current.view(&hub);
        }
        let mut context = hub.catalog.take().ok_or(EXPIRED)?;
        let result = (|| {
            if close {
                if context.changing && matches!(context.stage, Stage::Queued(_) | Stage::Running) {
                    return Err(BUSY.into());
                }
                context.visible = false;
                context.review = None;
                if context.ticket.is_none() && matches!(context.stage, Stage::Queued(_)) {
                    context.stage = Stage::Idle;
                    context.preview = None;
                }
            } else {
                if id.is_none() {
                    context.visible = true;
                }
                if let Some(action) = action {
                    if !context.visible {
                        return Err(EXPIRED.into());
                    }
                    if !self.0.allowed {
                        return Err(REPLAY_NEEDS_HGS.into());
                    }
                    context.queue(&mut hub, action)?;
                }
            }
            context.view(&hub)
        })();
        hub.catalog = Some(context);
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
        self.0.wake.notify_all();
        result
    }
    /// One command at a time, on the existing organization worker. A returned
    /// runner result is not idle until its actual child and pipes have drained.
    pub(super) fn catalog_pass(&self) -> bool {
        if self.0.hub.is_poisoned() {
            return false;
        }
        let work = {
            let mut hub = self.lock();
            let Some(context) = hub.catalog.as_mut() else {
                return false;
            };
            match std::mem::replace(&mut context.stage, Stage::Idle) {
                Stage::Queued(work) => {
                    context.stage = Stage::Running;
                    Some(work)
                }
                Stage::Draining {
                    work,
                    result,
                    resources,
                } if resources.upgrade().is_none() => {
                    let mut context = hub.catalog.take().unwrap();
                    let needs_settlement =
                        context.ticket.is_some() && !matches!(work.step, Step::Fresh);
                    if let Err(error) = context.result(&hub, work.step, result) {
                        if needs_settlement {
                            context.outcome = Some(Err(error));
                            context.stage = read_work(Step::Fresh, Read::Swarm)
                                .map(Stage::Queued)
                                .unwrap_or(Stage::Idle);
                        } else {
                            context.failed(error);
                        }
                    }
                    hub.catalog = Some(context);
                    hub.shown = None;
                    self.publish(&mut hub);
                    self.release(hub);
                    self.catalog_settle_source();
                    return true;
                }
                other => {
                    context.stage = other;
                    None
                }
            }
        };
        let Some(work) = work else {
            return false;
        };
        let resources = Arc::new(Resources(Arc::downgrade(&self.0)));
        let weak = Arc::downgrade(&resources);
        let result = zerus_client_core::run::run_accounted(
            &self.0.hgs,
            &work.invocation,
            &[],
            Read::Swarm.limit(),
            &AtomicBool::new(false),
            resources.clone(),
        )
        .map_err(|e| match e {
            zerus_client_core::run::Stopped::TooLarge(too) => {
                ReadError::TooLarge { limit: too.cap }.to_string()
            }
            zerus_client_core::run::Stopped::Cancelled => "hgs request cancelled".into(),
        });
        drop(resources);
        let mut hub = self.lock();
        if let Some(context) = hub.catalog.as_mut() {
            context.stage = Stage::Draining {
                work,
                result,
                resources: weak,
            };
        }
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
        true
    }
    fn catalog_settle_source(&self) {
        if self.0.hub.is_poisoned() {
            return;
        }
        let source = {
            let hub = self.lock();
            if hub
                .catalog
                .as_ref()
                .is_none_or(|c| !matches!(c.stage, Stage::Source))
            {
                return;
            }
            hub.source.clone()
        };
        let Some(source) = source.and_then(|source| source()) else {
            return;
        };
        let mut hub = self.lock();
        if settle(&mut hub, &source, self.now()) {
            hub.shown = None;
            self.publish(&mut hub);
            self.release(hub);
            self.0.wake.notify_all();
        }
    }
}
/// The source cache must name the exact post-call catalog and causal heads.
/// The caller's generation check excludes callbacks already in flight.
pub(super) fn settle(hub: &mut Hub, source: &OrganizationSnapshot, now: u64) -> bool {
    let Some(mut context) = hub.catalog.take() else {
        return false;
    };
    let ready = matches!(context.stage, Stage::Source)
        && context.expected.as_ref().is_some_and(|expected| {
            source.node_id == expected.value["node_id"].as_str().unwrap_or_default()
                && source.swarm_id == expected.value["swarm_id"].as_str().unwrap_or_default()
                && source.digest == expected.digest
                && source.base == expected.value["organization"]
                && source.versions == expected.versions
        });
    let changed = if ready {
        match hub.source_generation.checked_add(1) {
            Some(generation) => {
                let expected = context.expected.take().unwrap();
                match hub
                    .sync
                    .finish_external(context.ticket.as_ref().unwrap(), expected, now)
                {
                    Ok(()) => {
                        hub.source_generation = generation;
                        context.ticket = None;
                        context.preview = None;
                        context.join = None;
                        context.review = None;
                        context.resolving = None;
                        context.changing = false;
                        context.stage = Stage::Idle;
                        match context
                            .outcome
                            .take()
                            .unwrap_or_else(|| Err(connection::INVALID.into()))
                        {
                            Ok(status) => {
                                context.status = status;
                                context.error = false;
                            }
                            Err(error) => {
                                context.status = error;
                                context.error = true;
                            }
                        }
                        if let Some(action) = context.after.take() {
                            if let Err(error) = context.queue(hub, action) {
                                context.failed(error);
                            }
                        }
                    }
                    Err(error) => context.failed(error),
                }
                true
            }
            None => {
                context.failed(STOPPED.into());
                true
            }
        }
    } else {
        false
    };
    hub.catalog = Some(context);
    changed
}

fn main(window: &tauri::Webview) -> Result<(), String> {
    if window.label() == crate::MAIN_WINDOW {
        Ok(())
    } else {
        Err("Open Project swarm from the main window.".into())
    }
}
async fn command(
    org: Organization,
    id: Option<String>,
    action: Option<Action>,
    close: bool,
) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || org.catalog_request(id.as_deref(), action, close))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn project_swarm_open(
    window: tauri::Webview,
    state: State<'_, Organization>,
) -> Result<View, String> {
    main(&window)?;
    command(state.inner().clone(), None, None, false).await
}
#[tauri::command]
pub async fn project_swarm_view(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
) -> Result<View, String> {
    main(&window)?;
    command(state.inner().clone(), Some(id), None, false).await
}
#[tauri::command]
pub async fn project_swarm_preview(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
    alias: String,
) -> Result<View, String> {
    main(&window)?;
    command(
        state.inner().clone(),
        Some(id),
        Some(Action::Preview(alias)),
        false,
    )
    .await
}
#[tauri::command]
pub async fn project_swarm_join(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
    mappings: Vec<(String, String)>,
    keep: bool,
) -> Result<View, String> {
    main(&window)?;
    if mappings.len() > zerus_client_core::swarm::MAX_PROJECTS {
        return Err(connection::INVALID.into());
    }
    command(
        state.inner().clone(),
        Some(id),
        Some(Action::Join { mappings, keep }),
        false,
    )
    .await
}
#[tauri::command]
pub async fn project_swarm_sync(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
) -> Result<View, String> {
    main(&window)?;
    command(state.inner().clone(), Some(id), Some(Action::Sync), false).await
}
#[tauri::command]
pub async fn project_swarm_close(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
) -> Result<View, String> {
    main(&window)?;
    command(state.inner().clone(), Some(id), None, true).await
}
