//! The Teams views' reads (task #6): one core [`TeamsPoller`] per computer a
//! window follows, feeding the pump. The Team tab, the plan graph, Waiting
//! and New team get their projected views in the per-frame batches
//! (`batch.teams`, keyed `list`, `waiting`, `team:<id>`, ...); the webview
//! never runs or parses an `hgs team` read. [`team_action`] runs the team
//! actions (task #7). A replay never runs `hgs`.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tauri::State;
use zerus_client_core::fleet::valid_alias;
use zerus_client_core::teams::read::valid_team;
use zerus_client_core::teams::catalog_edit::{
    schema, CatalogEdit, CatalogEditor, CatalogEditorView, FieldSpec, Kind, RemoveImpact,
    IMPORT_BYTES,
};
use zerus_client_core::teams::read::TeamRead;
use zerus_client_core::teams::view::{builds as builds_view, tests as tests_view, BuildsView, TestsView};
use zerus_client_core::teams::{Heads, ViewRows, WaitingShare};
use zerus_client_core::{default_hgs, ComputerId, TeamCall, TeamOutcome, TeamsFollow, TeamsPoller};

use crate::commands::AppState;

/// Computers whose Teams views are followed at once.
const COMPUTERS: usize = 16;

/// The pollers by computer.
pub struct Teams {
    pollers: Mutex<BTreeMap<ComputerId, TeamsPoller>>,
    /// Task #44: the catalog editor's working copy by computer.
    editors: Arc<Mutex<BTreeMap<ComputerId, CatalogEditor>>>,
    /// Task 8: each computer's latest stream team heads, for a poller that
    /// starts later; None or absent: it polls.
    heads: Mutex<BTreeMap<ComputerId, Arc<Heads>>>,
    /// Task #16 of team c536f0e5 (P2.5): each computer's stream team views
    /// so far, for a poller that starts later; absent: it reads them.
    views: Mutex<BTreeMap<ComputerId, BTreeMap<String, serde_json::Value>>>,
    /// Task #10: the window is hidden or minimized: the pollers read only
    /// Waiting, at the always-on watcher's pace.
    hidden: AtomicBool,
    /// Task #10: this computer's newest Waiting reply, which the always-on
    /// watcher (`team_watch`) reuses instead of reading its own.
    waiting: WaitingShare,
    replay: bool,
    /// Upstream's native team task actions (its Tasks screen's Send to team),
    /// kept for its task services; the fork's own teams do not use them.
    actions: Arc<crate::desktop_service::team_person_actions::Actions>,
}

impl Teams {
    pub fn new(replay: bool) -> Self {
        Self {
            pollers: Mutex::new(BTreeMap::new()),
            editors: Arc::new(Mutex::new(BTreeMap::new())),
            heads: Mutex::new(BTreeMap::new()),
            views: Mutex::new(BTreeMap::new()),
            hidden: AtomicBool::new(false),
            waiting: WaitingShare::default(),
            replay,
            actions: Arc::default(),
        }
    }

    pub(crate) fn native_actions(&self) -> Arc<crate::desktop_service::team_person_actions::Actions> {
        self.actions.clone()
    }

    /// Task #10: the window was hidden or minimized (`true`) or shown again
    /// (`window_visibility`). Every poller, and one that starts later, follows it.
    pub fn hidden(&self, hidden: bool) {
        self.hidden.store(hidden, Ordering::SeqCst);
        for poller in self.lock().values() {
            poller.hidden(hidden);
        }
    }

    /// Task #10: this computer's newest Waiting reply for the watcher.
    pub fn waiting_share(&self) -> &WaitingShare {
        &self.waiting
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ComputerId, TeamsPoller>> {
        self.pollers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follows `follow` on `computer`; following nothing stops its poller.
    pub fn follow(
        &self,
        pump: &crate::pump::Pump,
        computer: ComputerId,
        follow: TeamsFollow,
    ) -> Result<(), String> {
        if self.replay {
            return Err("Teams need hgs; a replay has none.".into());
        }
        let mut pollers = self.lock();
        if follow == TeamsFollow::default() {
            if let Some(poller) = pollers.get(&computer) {
                // Its views end through the batch; then the thread stops.
                poller.follow(follow);
            }
            pollers.remove(&computer);
            return Ok(());
        }
        if !pollers.contains_key(&computer) {
            if pollers.len() >= COMPUTERS {
                return Err("Too many computers follow their teams at once.".into());
            }
            let pump = pump.clone();
            let poller = TeamsPoller::spawn(default_hgs(), computer.clone(), move |change| {
                pump.push(change)
            })
            .map_err(|error| format!("The Teams reader did not start: {error}"))?;
            if let Some(heads) = self
                .heads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&computer)
            {
                poller.heads(Some(Arc::clone(heads)));
            }
            if let Some(rows) = self.views.lock().unwrap_or_else(PoisonError::into_inner).get(&computer) {
                poller.views(Some(Arc::new(rows.iter().map(|(key, row)| (key.clone(), row.clone())).collect())));
            }
            if self.hidden.load(Ordering::SeqCst) {
                poller.hidden(true);
            }
            if computer.is_local() {
                poller.share_waiting(Some(self.waiting.clone()));
            }
            pollers.insert(computer.clone(), poller);
        }
        if let Some(poller) = pollers.get(&computer) {
            poller.follow(follow);
        }
        Ok(())
    }

    /// Task #16 of team c536f0e5 (P2.5): rows of a computer's stream
    /// `team.views` topic, or None when it stopped giving them; its poller
    /// applies them instead of reading those views.
    pub fn views(&self, computer: &ComputerId, views: Option<ViewRows>) {
        {
            let mut known = self.views.lock().unwrap_or_else(PoisonError::into_inner);
            match &views {
                Some(rows) => {
                    let held = known.entry(computer.clone()).or_default();
                    for (key, row) in rows.iter() {
                        held.insert(key.clone(), row.clone());
                    }
                }
                None => {
                    known.remove(computer);
                }
            }
        }
        if let Some(poller) = self.lock().get(computer) {
            poller.views(views);
        }
    }

    /// Task 8: the team heads of a computer's watch stream (its `team`
    /// topic), or None while it gives none. With heads its poller reads a
    /// team again when its revision moved instead of every 3 s.
    pub fn heads(&self, computer: &ComputerId, heads: Option<Arc<Heads>>) {
        {
            let mut known = self.heads.lock().unwrap_or_else(PoisonError::into_inner);
            match &heads {
                Some(heads) => {
                    known.insert(computer.clone(), Arc::clone(heads));
                }
                None => {
                    known.remove(computer);
                }
            }
        }
        if let Some(poller) = self.lock().get(computer) {
            poller.heads(heads);
        }
    }

    fn with(&self, computer: &ComputerId, ask: impl FnOnce(&TeamsPoller) -> bool) -> bool {
        self.lock().get(computer).is_some_and(ask)
    }
}

/// The computer a command names: empty is this computer.
fn computer(alias: &str) -> Result<ComputerId, String> {
    if alias.is_empty() {
        return Ok(ComputerId::local());
    }
    if !valid_alias(alias) {
        return Err("Not a computer alias.".into());
    }
    Ok(ComputerId::from(alias))
}

/// Follows the Teams views of one computer: the team list, Waiting and at
/// most one team (its tab, plan and New team form). All false and no team
/// stops reading that computer.
/// Task 72 (F3): off the main thread, which a cold start keeps busy (the
/// window, WebView2, the tray): the first Waiting read is asked at once.
#[tauri::command(async)]
pub fn teams_follow(
    state: State<'_, AppState>,
    teams: State<'_, Teams>,
    computer: String,
    list: bool,
    waiting: bool,
    team: Option<String>,
    catalog: bool,
) -> Result<(), String> {
    let team = team.filter(|team| !team.is_empty());
    if team.as_deref().is_some_and(|team| !valid_team(team)) {
        return Err("Not a team id.".into());
    }
    let target = self::computer(&computer)?;
    // Task #16 of team c536f0e5 (P2.5): the computer's stream sends the
    // followed views as rows (a stream without the topic: they are read).
    let views = (list || waiting || team.is_some()).then(|| team.clone());
    if views.is_none() {
        teams.views(&target, None);
    }
    state.follow_team_views(&target, views);
    teams.follow(
        &state.pump(),
        target,
        TeamsFollow {
            list,
            waiting,
            team,
            catalog,
        },
    )
}

/// Loads the watched team's feed page before the oldest event shown. False:
/// there is none, the feed is at its cap, or a page is already asked for.
#[tauri::command]
pub fn teams_older(teams: State<'_, Teams>, computer: String) -> Result<bool, String> {
    let target = self::computer(&computer)?;
    Ok(teams.with(&target, TeamsPoller::older))
}

/// Loads the next older Waiting page; false when there is none or one is
/// already asked for.
#[tauri::command]
pub fn teams_waiting_older(teams: State<'_, Teams>, computer: String) -> Result<bool, String> {
    let target = self::computer(&computer)?;
    Ok(teams.with(&target, TeamsPoller::waiting_older))
}

/// Reads the watched team's time spans after task `after`.
#[tauri::command]
pub fn teams_spans(teams: State<'_, Teams>, computer: String, after: u64) -> Result<bool, String> {
    let target = self::computer(&computer)?;
    Ok(teams.with(&target, |poller| poller.spans(after)))
}

/// Task 51: the Waiting page's item notifications, through the client's
/// native notifier (the same path as attention notices).
#[derive(serde::Deserialize)]
pub struct WaitingNotice {
    token: String,
    title: String,
    body: String,
}

#[tauri::command]
pub fn team_waiting_notify(
    app: tauri::AppHandle,
    raised: Vec<WaitingNotice>,
    cleared: Vec<String>,
) {
    crate::desk::deliver_waiting(
        &app,
        cleared,
        raised
            .into_iter()
            .map(|notice| zerus_client_core::attention::Notice {
                token: notice.token,
                title: notice.title,
                body: notice.body,
            })
            .collect(),
    );
}

/// Reads the watched team's profile.
#[tauri::command]
pub fn teams_profile(teams: State<'_, Teams>, computer: String) -> Result<bool, String> {
    let target = self::computer(&computer)?;
    Ok(teams.with(&target, TeamsPoller::profile))
}

/// Task 41: the Tests mode's queue and one page of results, read together off
/// the UI thread. The mode calls this only while it is open.
#[tauri::command]
pub async fn team_tests(computer: String, team: String, before: Option<f64>) -> Result<TestsView, String> {
    if !valid_team(&team) {
        return Err("Not a team id.".into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        let queue = zerus_client_core::teams::read::read(
            &default_hgs(),
            &target,
            &TeamRead::TestQueue { team: team.clone() },
        )
        .map_err(|error| error.to_string())?;
        let results = zerus_client_core::teams::read::read(
            &default_hgs(),
            &target,
            &TeamRead::TestResults { team: team.clone(), before },
        )
        .map_err(|error| error.to_string())?;
        Ok(tests_view(&team, &queue, &results))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Task 199: the Tests tab's Builds (`hgs team builds`), read on request off
/// the UI thread while the tab shows them.
#[tauri::command]
pub async fn team_builds(computer: String, team: String) -> Result<BuildsView, String> {
    if !valid_team(&team) {
        return Err("Not a team id.".into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        let value = zerus_client_core::teams::read::read(&default_hgs(), &target, &TeamRead::Builds { team: team.clone() })
            .map_err(|error| error.to_string())?;
        Ok(builds_view(&team, &value))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Task 92: the profile's Time view of one scope (a phase, or the plan
/// without one), read on request off the UI thread: the status, the scope's
/// profile, its timelines in bounded pages and the window's test runs,
/// projected by client/core (`teams::timeline`).
#[tauri::command]
pub async fn team_timeline(
    teams: State<'_, Teams>,
    computer: String,
    team: String,
    phase: Option<u64>,
) -> Result<zerus_client_core::teams::timeline::TimelineView, String> {
    if teams.replay {
        return Err("Teams need hgs; a replay has none.".into());
    }
    if !valid_team(&team) {
        return Err("Not a team id.".into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64());
        zerus_client_core::teams::timeline::read(&default_hgs(), &target, &team, phase, now)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Runs one team action (task #7) as a guarded `hgs team` call with a
/// request identity, off the main thread. A destructive action or an
/// approval comes back as `confirm` until the call says `confirmed`; an
/// `uncertain` outcome is never sent again by itself. After a `done` or
/// `uncertain` outcome the computer's followed views are read again at once.
#[tauri::command]
pub async fn team_action(teams: State<'_, Teams>, call: TeamCall) -> Result<TeamOutcome, String> {
    if teams.replay {
        return Err("Teams need hgs; a replay has none.".into());
    }
    let target = self::computer(&call.computer)?;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::teams::action::run(&default_hgs(), &call)
    })
    .await
    .map_err(|error| error.to_string())?;
    if matches!(
        outcome,
        TeamOutcome::Done { .. } | TeamOutcome::Uncertain { .. }
    ) {
        teams.with(&target, |poller| {
            poller.refresh();
            true
        });
    }
    Ok(outcome)
}

/// Task 50: the models a catalog role's account offers (its account, or the
/// agent's native one), for the catalog editor's model and effort menus.
#[tauri::command]
pub async fn team_role_models(
    teams: State<'_, Teams>,
    computer: String,
    agent: String,
    account: String,
) -> Result<Vec<zerus_client_core::teams::role_models::RoleModel>, String> {
    if teams.replay {
        return Err("Teams need hgs; a replay has none.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::teams::role_models::read(&default_hgs(), &computer, &agent, &account)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Task 68: reads or writes one project's or session's iron rules (Qt's
/// IronRulesPanel): the guarded `hgs rules` call client/core builds.
#[tauri::command]
pub async fn iron_rules(
    teams: State<'_, Teams>,
    call: zerus_client_core::iron_rules::RulesCall,
) -> Result<zerus_client_core::iron_rules::RulesOutcome, String> {
    if teams.replay {
        return Err("Iron rules need hgs; a replay has none.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::iron_rules::run(&default_hgs(), &call)
    })
    .await
    .map_err(|error| error.to_string())
}

/// Task 126: reads or writes one project's memory (RustCodeLore switch, store
/// folder, verified move): the guarded `hgs project config` call client/core
/// builds.
#[tauri::command]
pub async fn project_memory(
    teams: State<'_, Teams>,
    call: zerus_client_core::project_memory::MemoryCall,
) -> Result<zerus_client_core::project_memory::MemoryOutcome, String> {
    if teams.replay {
        return Err("Project memory needs hgs; a replay has none.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::project_memory::run(&default_hgs(), &call)
    })
    .await
    .map_err(|error| error.to_string())
}

/// zerus-un7n phase 5/6: multi-person host access on one computer: the
/// caller's status, the provider support matrix and shared project history
/// and memory. client/core probes the host's capability marker first; the
/// host enforces every rule, so nothing here is a security boundary.
#[tauri::command]
pub async fn host_access(
    teams: State<'_, Teams>,
    call: zerus_client_core::host_access::Call,
) -> Result<zerus_client_core::host_access::Outcome, String> {
    if teams.replay {
        return Err("Host access needs hgs; a replay has none.".into());
    }
    tauri::async_runtime::spawn_blocking(move || zerus_client_core::host_access::run(&default_hgs(), &call))
        .await
        .map_err(|error| error.to_string())
}

/// Task 11: reads one team once, as a Waiting for you answer does before it
/// sends (Qt's TeamWaitingAnswer::read): the projected `team status`.
#[tauri::command]
pub async fn team_read(
    teams: State<'_, Teams>,
    computer: String,
    team: String,
) -> Result<zerus_client_core::teams::view::TeamView, String> {
    if teams.replay {
        return Err("Teams need hgs; a replay has none.".into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        let read = zerus_client_core::teams::read::TeamRead::Status { team };
        zerus_client_core::teams::read::read(&default_hgs(), &target, &read)
            .map(|value| zerus_client_core::teams::view::team(&value))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

const REPLAY: &str = "Teams need hgs; a replay has none.";

/// One selected task, including archived history, read off the UI thread.
#[tauri::command]
pub async fn team_task_read(
    teams: State<'_, Teams>,
    computer: String,
    team: String,
    task: u32,
) -> Result<zerus_client_core::teams::view::TaskDetailView, String> {
    if teams.replay {
        return Err(REPLAY.into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        let read = TeamRead::Task { team, task };
        zerus_client_core::teams::read::read(&default_hgs(), &target, &read)
            .map(|value| zerus_client_core::teams::view::task_detail(&value))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}
const NOT_OPEN: &str = "Open the catalog editor first.";

type Editors = Arc<Mutex<BTreeMap<ComputerId, CatalogEditor>>>;

fn lock_editors(editors: &Editors) -> MutexGuard<'_, BTreeMap<ComputerId, CatalogEditor>> {
    editors.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads the saved catalog into the computer's editor: the first time, or
/// after it changed (unsaved edits move onto the newer version, Qt's
/// rebase). Blocks; callers run it off the main thread.
fn load_editor(editors: &Editors, computer: &ComputerId) -> Result<CatalogEditorView, String> {
    let reply = zerus_client_core::teams::read::read(&default_hgs(), computer, &TeamRead::Catalog)
        .map_err(|error| error.to_string())?;
    let mut editors = lock_editors(editors);
    if !editors.contains_key(computer) && editors.len() >= COMPUTERS {
        return Err("Too many computers edit their catalogs at once.".into());
    }
    let editor = editors.entry(computer.clone()).or_default();
    editor.load(&reply)?;
    Ok(editor.view())
}

/// Task #44: the fields Qt's catalog editor shows for each kind.
#[tauri::command]
pub fn catalog_editor_schema() -> BTreeMap<&'static str, Vec<FieldSpec>> {
    [Kind::Role, Kind::Template, Kind::Rule, Kind::Pipeline]
        .into_iter()
        .map(|kind| (kind.list(), schema(kind)))
        .collect()
}

/// Task #44: the catalog editor of a computer, read (again) from hgs.
#[tauri::command]
pub async fn catalog_editor_load(
    teams: State<'_, Teams>,
    computer: String,
) -> Result<CatalogEditorView, String> {
    if teams.replay {
        return Err(REPLAY.into());
    }
    let target = self::computer(&computer)?;
    let editors = Arc::clone(&teams.editors);
    tauri::async_runtime::spawn_blocking(move || load_editor(&editors, &target))
        .await
        .map_err(|error| error.to_string())?
}

/// Task #44: one edit, checked in core; the editor's new view, or why the
/// edit changed nothing.
#[tauri::command]
pub fn catalog_editor_edit(
    teams: State<'_, Teams>,
    computer: String,
    edit: CatalogEdit,
) -> Result<CatalogEditorView, String> {
    let target = self::computer(&computer)?;
    let mut editors = lock_editors(&teams.editors);
    let editor = editors.get_mut(&target).ok_or(NOT_OPEN)?;
    editor.edit(&edit)?;
    Ok(editor.view())
}

/// Task #44: Revert: drops the unsaved edits (the webview asked the owner).
#[tauri::command]
pub fn catalog_editor_revert(
    teams: State<'_, Teams>,
    computer: String,
) -> Result<CatalogEditorView, String> {
    let target = self::computer(&computer)?;
    let mut editors = lock_editors(&teams.editors);
    let editor = editors.get_mut(&target).ok_or(NOT_OPEN)?;
    editor.revert();
    Ok(editor.view())
}

/// Task #44: Save catalog: `hgs team catalog set` with the working copy over
/// the version it was read at, guarded by its request id. Saved: the editor
/// reads the new version. A newer version refused it: the editor reads that
/// one and the edits move onto it, as in Qt; nothing was written.
#[tauri::command]
pub async fn catalog_editor_save(
    teams: State<'_, Teams>,
    computer: String,
    request_id: Option<String>,
) -> Result<TeamOutcome, String> {
    if teams.replay {
        return Err(REPLAY.into());
    }
    let target = self::computer(&computer)?;
    let action = lock_editors(&teams.editors)
        .get(&target)
        .ok_or(NOT_OPEN)?
        .save_action();
    write_catalog(&teams, computer, target, action, request_id).await
}

/// Task #44: Reset planning rules (U28): saves the saved catalog without its
/// planning rules so hgs fills in its built-in ones; the webview asked the
/// owner. Refused while edits are unsaved.
#[tauri::command]
pub async fn catalog_editor_reset_rules(
    teams: State<'_, Teams>,
    computer: String,
    request_id: Option<String>,
) -> Result<TeamOutcome, String> {
    if teams.replay {
        return Err(REPLAY.into());
    }
    let target = self::computer(&computer)?;
    let action = lock_editors(&teams.editors)
        .get(&target)
        .ok_or(NOT_OPEN)?
        .reset_rules_action()
        .ok_or("Save or revert your edits first.")?;
    write_catalog(&teams, computer, target, action, request_id).await
}

/// Task #44: what deleting an item takes with it, for the confirmation.
#[tauri::command]
pub fn catalog_editor_impact(
    teams: State<'_, Teams>,
    computer: String,
    kind: Kind,
    id: String,
) -> Result<RemoveImpact, String> {
    let target = self::computer(&computer)?;
    let editors = lock_editors(&teams.editors);
    Ok(editors.get(&target).ok_or(NOT_OPEN)?.remove_impact(kind, &id))
}

/// Writes a catalog through `hgs team catalog set` and reads the result
/// into the editor (see [`catalog_editor_save`]).
async fn write_catalog(
    teams: &Teams,
    computer: String,
    target: ComputerId,
    action: zerus_client_core::TeamAction,
    request_id: Option<String>,
) -> Result<TeamOutcome, String> {
    let editors = Arc::clone(&teams.editors);
    let reread = target.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let call = TeamCall {
            computer,
            team: String::new(),
            team_name: String::new(),
            action,
            confirmed: true,
            request_id,
        };
        let outcome = zerus_client_core::teams::action::run(&default_hgs(), &call);
        match &outcome {
            TeamOutcome::Done { .. } => {
                if let Some(editor) = lock_editors(&editors).get_mut(&reread) {
                    editor.saved();
                }
                let _ = load_editor(&editors, &reread);
            }
            TeamOutcome::Failed { detail, .. } if detail.contains("catalog changed") => {
                let _ = load_editor(&editors, &reread);
            }
            _ => {}
        }
        outcome
    })
    .await
    .map_err(|error| error.to_string())?;
    teams.with(&target, |poller| {
        poller.refresh();
        true
    });
    Ok(outcome)
}

/// Task #44: Export…: asks where, then writes the working copy there. None:
/// the owner cancelled; Some: the path written.
#[tauri::command]
pub async fn catalog_editor_export(
    app: tauri::AppHandle,
    teams: State<'_, Teams>,
    computer: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let target = self::computer(&computer)?;
    let text = lock_editors(&teams.editors)
        .get(&target)
        .ok_or(NOT_OPEN)?
        .export_text();
    let window = crate::main_window(&app);
    let handle = app.clone();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = handle
            .dialog()
            .file()
            .set_title("Export team catalog")
            .set_file_name("zerus-team-catalog.json")
            .add_filter("JSON", &["json"]);
        if let Some(window) = &window {
            dialog = dialog.set_parent(window);
        }
        dialog.blocking_save_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(path) = chosen.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };
    let shown = path.display().to_string();
    let written = tauri::async_runtime::spawn_blocking(move || std::fs::write(&path, text.as_bytes()))
        .await
        .map_err(|error| error.to_string())?;
    written.map_err(|_| format!("Could not write {shown}"))?;
    Ok(Some(shown))
}

/// Task #44: Import…: asks for a file and reads it into the working copy,
/// unsaved until Save. None: the owner cancelled.
#[tauri::command]
pub async fn catalog_editor_import(
    app: tauri::AppHandle,
    teams: State<'_, Teams>,
    computer: String,
) -> Result<Option<CatalogEditorView>, String> {
    use std::io::Read;
    use tauri_plugin_dialog::DialogExt;
    let target = self::computer(&computer)?;
    if !lock_editors(&teams.editors).contains_key(&target) {
        return Err(NOT_OPEN.into());
    }
    let window = crate::main_window(&app);
    let handle = app.clone();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = handle
            .dialog()
            .file()
            .set_title("Import team catalog")
            .add_filter("JSON", &["json"]);
        if let Some(window) = &window {
            dialog = dialog.set_parent(window);
        }
        dialog.blocking_pick_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(path) = chosen.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };
    let shown = path.display().to_string();
    let read = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let file = std::fs::File::open(&path).map_err(|_| format!("Could not read {shown}"))?;
        let mut bytes = Vec::new();
        file.take(IMPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| format!("Could not read {shown}"))?;
        String::from_utf8(bytes).map_err(|_| format!("{shown} is not a team catalog"))
    })
    .await
    .map_err(|error| error.to_string())?;
    let text = read?;
    let mut editors = lock_editors(&teams.editors);
    let editor = editors.get_mut(&target).ok_or(NOT_OPEN)?;
    editor.import_text(&text)?;
    Ok(Some(editor.view()))
}

/// Task #44: the routing tab's Try it: the template a goal would use.
#[tauri::command]
pub async fn catalog_route(
    teams: State<'_, Teams>,
    computer: String,
    goal: String,
) -> Result<serde_json::Value, String> {
    if teams.replay {
        return Err(REPLAY.into());
    }
    let target = self::computer(&computer)?;
    tauri::async_runtime::spawn_blocking(move || {
        zerus_client_core::teams::read::read(
            &default_hgs(),
            &target,
            &TeamRead::CatalogRoute { goal },
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_name_a_real_computer_and_a_replay_runs_no_hgs() {
        assert_eq!(computer("").unwrap(), ComputerId::local());
        assert_eq!(computer("studio").unwrap(), ComputerId::from("studio"));
        assert!(computer("-oProxyCommand=x").is_err());
        let teams = Teams::new(true);
        let pump = crate::pump::Pump::default();
        let follow = TeamsFollow {
            list: true,
            ..TeamsFollow::default()
        };
        assert!(teams.follow(&pump, ComputerId::local(), follow).is_err());
        assert!(teams.lock().is_empty());
        // Following nothing on a computer that has no poller changes nothing.
        let live = Teams::new(false);
        live.follow(&pump, ComputerId::local(), TeamsFollow::default())
            .unwrap();
        assert!(live.lock().is_empty());
        assert!(!live.with(&ComputerId::local(), TeamsPoller::older));
    }
}
