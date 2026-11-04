//! Durable agent/session bindings. Histories remain owned by the agent CLIs.
//!
//! Version-one manifests and journal rows deliberately remain compatible with
//! the original helper: an upgrade must never orphan a saved conversation.
mod answer_channel;
mod agent_hint;
mod archive;
pub(crate) mod archive_catalog;
mod claude_trust;
mod claude_permission;
mod claude_resume;
mod claude_usage_limit;
mod claude_question;
mod claude_statusline;
mod claude_subagents;
mod attachments;
mod computer_files;
pub(crate) mod voice;
mod interrupt;
mod clear_context;
mod codex_clear_recovery;
mod compact_context;
mod continuations;
mod cache_hint;
mod claude_approval;
mod attempts;
pub(crate) mod backend;
pub(crate) mod dsh;
pub(crate) mod model_servers;
pub(crate) mod opencode;
mod effort;
mod external_agents;
mod fork;
mod goals;
mod hooks;
mod command_guard;
pub(crate) mod hook_health;
mod input;
mod input_hold;
mod input_queue;
mod journal;
mod kimi_cache_hint;
mod kimi_messages;
mod kimi_approval;
mod kimi_goal;
mod kimi_plan;
mod kimi_trust;
mod kimi_tui_choice;
mod lifecycle;
mod kimi_launch;
mod provider_errors;
mod recovery;
mod service_worker;
mod session_limits;
mod provider_messages;
mod processes;
mod services;
mod prompt_suggestion;
mod question_terminal;
mod questions;
mod codex_questions;
mod codex_approval;
mod codex_trust;
mod codex_hooks_trust;
mod codex_update;
mod codex_goal_resume;
mod codex_rate_limit;
mod read_marks;
mod notifications;
mod recipes;
mod rename;
mod resume_input;
mod scan_cache;
mod schedules;
mod waits;
pub(crate) mod watch_source;
mod search;
pub(crate) mod search_index;
pub(crate) mod session_graph;
mod storage;
mod subagents;
mod subagents_confirm;
mod session_upload;
mod session_state;
mod switch_account;
mod builds;
mod native_subagents;
mod supervisor;
mod tasks;
// WhiteWh fork: agent teams.
pub(crate) mod teams;
pub(crate) mod iron_rules;
mod telemetry;
mod terminate;
mod untracked_shell;
mod session_action;
pub(crate) use session_action::status_machine as lifecycle_receipt_machine;
mod shown_path;
mod startup_choice;
mod terminal;
mod conversation_history;
mod usage;
mod workspace;
mod git_status;
pub(crate) mod repositories;
mod worktrees;
mod worktree_requests;
mod worktree_cleanup;
mod worktree_caches;
mod worktree_inspection;
mod worktree_lifecycle;
mod worktree_maintenance;

/// Read the selected account's native catalog without starting an agent.
pub(crate) fn account_models(provider: &str, home: &std::path::Path) -> serde_json::Value {
    effort::account_models(provider, home)
}

/// Explicit native copy snapshot; no session is created or modified.
pub(crate) fn download_snapshot(path: &str) -> Result<Value> {
    computer_files::download_snapshot(path)
}

/// Current bounded native Git evidence for exact project/worktree placement.
pub(crate) fn worktree_catalog(path: &std::path::Path) -> serde_json::Value {
    if let Err(detail) = local_sessions() {
        return json!({"state":"unavailable","detail":detail,"stale":true,"worktrees":[]});
    }
    worktrees::catalog(path, true)
}
pub(crate) fn cached_worktree_catalog(path: &std::path::Path) -> Option<serde_json::Value> {
    worktrees::cached_catalog(path)
}
/// One optional launch inference shares one total Git deadline.
pub(crate) fn worktree_catalog_until(path: &std::path::Path, deadline: std::time::Instant) -> serde_json::Value {
    worktrees::catalog_until(path, true, deadline)
}
mod launch_project;
pub(crate) use launch_project::with_launch_binding;
mod session_project;
pub(crate) use session_project::with_session_project_binding;
pub(crate) mod launches;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use storage::*;

/// Tests drive the real command entry point (`teams::commands::run`) against a
/// private state directory: the team store, the worker lock and the session
/// records all live there, on this thread only. It is restored even when the
/// test panics.
#[cfg(test)]
pub(crate) fn with_test_state_root<T>(root: &Path, run: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            storage::set_test_root(None);
            teams::commands::set_test_caller_identities(None);
        }
    }
    storage::set_test_root(Some(root.to_path_buf()));
    // The test process may itself run inside a session; the command must see
    // no ambient identity so it acts as the owner.
    teams::commands::set_test_caller_identities(Some((None, None, None)));
    let _restore = Restore;
    run()
}

/// The state directory the Windows session host serves (`HGS_STATE_DIR`, else
/// the default below the home directory), not yet created or canonical.
#[cfg(windows)]
pub(crate) fn host_state_root() -> PathBuf {
    storage::root()
}

pub(super) type Result<T> = std::result::Result<T, String>;
pub(super) const AGENTS: &[&str] = &["claude", "codex", "kimi"];

/// The entry points other modules call fail first where this computer runs no
/// local sessions (a Windows control console), as they always have there.
pub(super) fn local_sessions() -> Result<()> {
    if crate::platform::local_sessions() {
        Ok(())
    } else {
        Err(crate::platform::LOCAL_SESSIONS_UNSUPPORTED.into())
    }
}

/// `local_sessions` for `dispatch`, which the processes of a session reach:
/// `hgs __state` from its tracking hooks and a tracked `--run` supervisor. On
/// Windows it also admits a session of an isolated test host, which may run
/// where local sessions are off (`host::gate::test_session`, spec D2); every
/// other entry point stays a control console's there.
pub(crate) fn session_operations() -> Result<()> {
    let enabled = local_sessions();
    #[cfg(windows)]
    if enabled.is_err() && crate::host::gate::test_session() {
        return Ok(());
    }
    enabled
}

/// WhiteWh fork: whether the agent team dispatcher runs; nothing before the
/// first team.
pub(crate) fn team_worker_running() -> Option<bool> {
    teams::store::path().exists().then(|| crate::status::locked(&teams::dispatcher::lock_path()))
}

/// Whether the automatic recovery worker holds its lock; unknown without local
/// sessions.
pub(crate) fn recovery_worker_running() -> Option<bool> {
    local_sessions().ok()?;
    Some(crate::status::locked(&root().join("recovery/worker.lock")))
}

pub(crate) fn account_context(name: &str) -> Result<Value> {
    local_sessions()?;
    if opencode::exists(name) { return opencode::account_context(name); }
    if dsh::exists(name) {
        return dsh::account_context(name);
    }
    let _guard = lock(None)?;
    let record = read(name)?;
    let snapshot = live()?;
    if snapshot.contains_key(name) && !matches(&record, snapshot.get(name)) {
        return Err("Session account binding is no longer current.".into());
    }
    Ok(account_binding(&record))
}

fn account_binding(record: &Value) -> Value {
    let provider = string(record, "agent");
    let id = if string(record, "account_id").is_empty() {
        format!("native-{provider}")
    } else {
        string(record, "account_id").to_owned()
    };
    let agent_home = if string(record, "agent_home").is_empty() {
        home().join(if provider == "kimi" {
            ".kimi-code".into()
        } else {
            format!(".{provider}")
        })
    } else {
        PathBuf::from(string(record, "agent_home"))
    };
    json!({"provider":provider,"id":id,"home":agent_home,"run_id":record["run_id"]})
}

/// Tests that change HGS_STATE_DIR share this lock (they run in one process).
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn home_var(agent: &str) -> Result<&'static str> {
    match agent {
        "claude" => Ok("CLAUDE_CONFIG_DIR"),
        "codex" => Ok("CODEX_HOME"),
        "kimi" => Ok("KIMI_CODE_HOME"),
        _ => Err(format!("unsupported agent: {agent}")),
    }
}

pub(super) fn now() -> f64 {
    #[cfg(test)]
    if let Some(at) = TEST_NOW.with(std::cell::Cell::get) {
        return at;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
thread_local! {
    static TEST_NOW: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Freeze protocol timestamps on this fixture thread, not monotonic wait budgets.
#[cfg(test)]
pub(super) fn with_test_now<T>(at: f64, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<f64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_NOW.with(|clock| clock.set(self.0));
        }
    }
    let _restore = Restore(TEST_NOW.with(|clock| clock.replace(Some(at))));
    run()
}

/// H2, N34: JSON piped from PowerShell 5.1 starts with a UTF-8 BOM; every
/// stdin reader parses the bytes after it.
pub(crate) fn without_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// H2: one JSON request from bytes: a leading UTF-8 BOM is dropped (PowerShell
/// 5.1 adds one) and input that is not UTF-8 is refused with the hint that
/// does not need a pipe. Every `--json` reader goes through this.
pub(crate) fn json_request(bytes: &[u8]) -> Result<Value> {
    let bytes = without_bom(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| "Input is not UTF-8 (PowerShell 5.1: use --json-file)")?;
    serde_json::from_str(text).map_err(|e| e.to_string())
}

pub(super) fn stdin_json() -> Result<Value> {
    let mut input = Vec::new();
    io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    json_request(&input)
}

pub(super) fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// C6: a global setting only you change, from a process outside every agent
/// session. It is the gate the team owner paths use: no caller record, no dsh
/// conversation, no hgs session or run marker, and the request says it is you
/// (`"by":"human"`). An agent that runs `hgs subagents-confirm set` or
/// `hgs session-upload-block set` therefore changes nothing, whatever it pipes.
pub(crate) fn owner_request(input: &Value, what: &str) -> Result<()> {
    let (dsh, session, run) = teams::commands::caller_identities();
    if session_graph::caller_record().is_some() || dsh.is_some() || session.is_some() || run.is_some() {
        return Err(format!("An agent session cannot change {what}"));
    }
    if string(input, "by") != "human" {
        return Err("Add \"by\":\"human\" to act as yourself".into());
    }
    Ok(())
}

pub(super) fn home() -> PathBuf {
    crate::platform::home_dir().unwrap_or_else(|| "/".into())
}

fn directories(path: &str, hidden: bool, creation: bool) -> Result<Value> {
    if path.chars().any(|c| c < ' ') {
        return Err("directory paths cannot contain control characters".into());
    }
    let expanded = if path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(path)
    };
    let directory = crate::platform::canonical(&expanded).map_err(|e| format!("{path}: {e}"))?;
    if !directory.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let mut entries = Vec::new();
    let listing=if creation { None }else{Some(std::fs::read_dir(&directory).map_err(|e|e.to_string())?)};
    for entry in listing.into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if (!hidden && name.starts_with('.')) || name.chars().any(|c| c < ' ') {
            continue;
        }
        if entry.path().is_dir() {
            entries.push(json!({"name": name, "path": entry.path(),
                "symlink": entry.file_type().map(|kind| kind.is_symlink()).unwrap_or(false)}));
        }
    }
    entries.sort_by(|a, b| {
        string(a, "name")
            .to_lowercase()
            .cmp(&string(b, "name").to_lowercase())
            .then_with(|| string(a, "name").cmp(string(b, "name")))
    });
    let truncated = entries.len() > 2000;
    entries.truncate(2000);
    let mut result = json!({"path": directory, "parent": directory.parent().unwrap_or(Path::new("/")),
        "home": home(), "directories": entries, "truncated": truncated});
    if let Ok(config) = crate::config::Config::load() {
        if creation {
            result["computer_id"]=json!(crate::swarm::local_node_id(&config).map_err(|e|e.to_string())?);
            result["creation_identity_v1"]=json!(true);
        }
        if let Some(project) = crate::swarm::directory_project(&config, &directory) {
            result["project_inference"] = project;
        }
    }
    Ok(result)
}

fn create_directory(path: &str, request_id: &str) -> Result<Value> {
    if path.is_empty() || path.len() > 4096 || path.chars().any(|c| c.is_control()) {
        return Err("directory paths must be nonempty, at most 4096 UTF-8 bytes and free of control characters".into());
    }
    let leaf = path.rsplit(|c| c == '/' || (cfg!(windows) && c == '\\')).next().unwrap_or("");
    if matches!(leaf, "" | "." | "..") {
        return Err("choose a new folder name without a trailing separator".into());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let relative = Path::new(rest);
        if relative.has_root() || matches!(relative.components().next(), Some(std::path::Component::Prefix(_))) {
            return Err("home-relative paths cannot start with a root or drive prefix".into());
        }
    }
    if !uuid::Uuid::parse_str(request_id)
        .is_ok_and(|id| !id.is_nil() && id.to_string() == request_id)
    {
        return Err("--request-id must be a canonical nonempty UUID".into());
    }
    let expanded = if path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(path)
    };
    expanded.file_name().ok_or("choose a new folder name")?;
    let parent = expanded.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = crate::platform::canonical(parent)
        .map_err(|e| format!("parent folder must already exist: {e}"))?;
    if !parent.is_dir() {
        return Err("parent is not a directory".into());
    }
    // One atomic mkdir: never overwrite, create intermediate parents or retry.
    std::fs::create_dir(&expanded).map_err(|e| format!("{path}: {e}"))?;
    let directory = crate::platform::canonical(&expanded)
        .map_err(|e| format!("folder creation could not be confirmed; inspect {path}: {e}"))?;
    Ok(json!({"status":"created", "request_id":request_id,
        "requested_path":path, "path":directory, "parent":parent}))
}

#[cfg(all(test, windows))]
#[test]
fn home_relative_creation_rejects_windows_roots_and_drive_prefixes_before_effects() {
    for path in ["~//root/child", "~/\\root\\child", "~/C:/child", "~/C:child"] {
        let error = create_directory(path, "11111111-1111-4111-8111-111111111111").unwrap_err();
        assert!(error.contains("root or drive prefix"), "{path}: {error}");
    }
}

/// Enrich the CLI's tmux snapshot without a subprocess or a JSON stdin pipe.
pub fn merge_snapshot(box_value: Value) -> Result<Value> {
    merge_listing(box_value, &BTreeMap::new())
}

/// `merge_snapshot` of a listing; `ids` holds each listed session's immutable
/// ID (`$N`) by its listed name.
pub fn merge_listing(box_value: Value, ids: &BTreeMap<String, String>) -> Result<Value> {
    merge_listing_archives(box_value, ids, true)
}

pub fn merge_listing_archives(mut box_value: Value, ids: &BTreeMap<String, String>, include_archives: bool) -> Result<Value> {
    local_sessions()?;
    // A rename renames the terminal session and moves its binding under the
    // state lock. Read the panes and the saved records under it as well, so a
    // listing sees a rename before it began or after it finished, never a
    // pane under the new name with its binding still under the old one: that
    // session would list twice (zerus-gd02). Archive reconciliation already
    // read both under this lock; the listing reuses those reads and releases
    // the lock before it summarizes the sessions.
    let ((snapshot, names), (archived, bindings)) = if root().exists() {
        let _guard = lock(None)?;
        let (snapshot, names) = live_sessions()?;
        let saved = if include_archives { archive::reconcile_locked(&snapshot)? } else { (Vec::new(), archive::reconcile_active_locked(&snapshot)?) };
        ((snapshot, names), saved)
    } else {
        (live_sessions()?, Default::default())
    };
    let marks = read_marks::load();
    let notification_preferences = notifications::load();
    let sessions = box_value
        .get_mut("sessions")
        .and_then(Value::as_array_mut)
        .ok_or("invalid session snapshot")?;
    // The terminal listed these sessions before `snapshot`. Every live session
    // has a pane, so one that the later snapshot lacks ended in between: it is
    // not live, and its saved binding lists below with a state instead of as
    // an untracked live row. A session renamed in between keeps its ID: it
    // lists once, under the name that `snapshot` and its binding now hold.
    let current = |listed: &str| ids.get(listed).and_then(|id| names.get(id)).cloned();
    let held: std::collections::BTreeSet<String> = sessions
        .iter()
        .filter_map(|session| current(string(session, "name")))
        .collect();
    sessions.retain_mut(|session| {
        let listed = string(session, "name").to_owned();
        match current(&listed) {
            Some(name) => {
                if name != listed {
                    let mut parts = name.splitn(3, '/');
                    session["cmd"] = json!(parts.next().unwrap_or(""));
                    session["project"] = json!(parts.next().filter(|s| !s.is_empty()));
                    session["tag"] = json!(parts.next().filter(|s| !s.is_empty()));
                    session["name"] = json!(name);
                }
                true
            }
            // Unknown or ended ID: the name decides, unless another listed
            // session was renamed to it.
            None => snapshot.contains_key(&listed) && !held.contains(&listed),
        }
    });
    let mut by_name = BTreeMap::new();
    for (index, session) in sessions.iter().enumerate() {
        by_name.insert(string(session, "name").to_owned(), index);
    }
    for session in sessions.iter_mut() {
        session
            .as_object_mut()
            .ok_or("invalid session")?
            .extend(telemetry::unavailable().as_object().unwrap().clone());
    }
    // Bounds question indexing for the whole listing.
    let shared = Shared { marks: &marks, preferences: notification_preferences.as_ref(), budget: read_marks::question_budget() };
    let mut process_work = processes::WorkSample::default();
    for record in bindings {
        if archive::equivalent(&record, &archived) {
            continue;
        }
        let name = string(&record, "name");
        if let Some(index) = by_name.get(name) {
            if matches(&record, snapshot.get(name)) {
                live_row(&mut sessions[*index], record, &shared, &mut process_work)?;
            }
            continue;
        }
        if string(&record, "conversation_id").is_empty() {
            continue;
        }
        // A record whose name was reused by a different live pane is not a
        // stopped session. Never offer its old identity as that pane's identity.
        if snapshot.contains_key(name) {
            continue;
        }
        let parts: Vec<&str> = name.splitn(3, '/').collect();
        let mut session = json!({"name": name, "cmd": record["agent"],
            "project": parts.get(1), "tag": parts.get(2), "attached": 0, "clients": [],
            "created": record["created"], "state": if record["paused"].as_bool().unwrap_or(false) {"paused"} else {"stopped"},
            "resumable": true});
        session.as_object_mut().unwrap().extend(
            journal::summary(&record, false)
                .as_object()
                .unwrap()
                .clone(),
        );
        read_marks::enrich_listing(&mut session, &marks, journal::journal_name(&record), Ok(Some(Vec::new())));
        notifications::enrich(&mut session, notification_preferences.as_ref(), journal::journal_name(&record));
        sessions.push(session);
    }
    for record in archived {
        let name = string(&record, "name");
        let parts: Vec<&str> = name.splitn(3, '/').collect();
        let mut session = json!({"name": name, "cmd": record["agent"], "project": parts.get(1),
            "tag": parts.get(2), "attached": 0, "clients": [], "created": record["created"],
            "state": "archived", "resumable": !string(&record, "conversation_id").is_empty(),
            "archive_id": record["archive_id"], "archived_at": record["archived_at"]});
        session.as_object_mut().unwrap().extend(
            journal::summary(&record, false)
                .as_object()
                .unwrap()
                .clone(),
        );
        read_marks::enrich_listing(&mut session, &marks, journal::journal_name(&record), Ok(Some(Vec::new())));
        notifications::enrich(&mut session, notification_preferences.as_ref(), journal::journal_name(&record));
        sessions.push(session);
    }
    sessions.extend(dsh::snapshots()?);
    sessions.extend(opencode::snapshots()?);
    finish_rows(sessions, &marks, &names);
    Ok(box_value)
}

/// What every row of one listing shares besides its process sample.
struct Shared<'a> {
    marks: &'a Value,
    preferences: Option<&'a notifications::Store>,
    /// Bounds question indexing for the whole listing.
    budget: std::time::Instant,
}

/// The row of a live session whose pane runs its saved binding `record`.
/// `hgs watch` re-derives single rows through this same step
/// (`watch_source::rows`).
fn live_row(session: &mut Value, record: Value, shared: &Shared, process_work: &mut processes::WorkSample) -> Result<()> {
    // An approval answered in Terminal sends no hook.
    let record = claude_approval::reconcile(record);
    session["resumable"] = json!(
        !string(&record, "conversation_id").is_empty()
            && string(&record, "error").is_empty()
    );
    let summary = journal::summary(&record, true);
    session["process_work"] = process_work.summary(&record, summary["activity"] == "idle" && summary["phase"] == "idle");
    session
        .as_object_mut()
        .ok_or("invalid session")?
        .extend(summary.as_object().ok_or("invalid summary")?.clone());
    let questions = read_marks::pending_questions(&record, session, true, shared.budget);
    read_marks::enrich_listing(session, shared.marks, journal::journal_name(&record), questions);
    notifications::enrich(session, shared.preferences, journal::journal_name(&record));
    Ok(())
}

/// The last steps of every listed row. `names` holds the session names of the
/// pane snapshot by session ID.
fn finish_rows(sessions: &mut [Value], marks: &Value, names: &BTreeMap<String, String>) {
    // DeepSeek and untracked live sessions are owned by their names.
    for session in sessions.iter_mut() {
        if session.get("attention_signature").is_none() {
            let owner = string(session, "name").to_owned();
            let questions=if session["agent"]=="opencode" {session["pending_questions"].as_array().cloned().unwrap_or_default()}else{Vec::new()};
            read_marks::enrich_listing(session, marks, &owner, Ok(Some(questions)));
        }
    }

    for session in sessions.iter_mut() {
        if session.get("notification_preferences").is_none() {
            session["notification_preferences"] = Value::Null;
        }
    }

    // U53: every row names its account, label included; the catalog is read once.
    let labels = crate::config::Config::load().ok().map(|config| crate::accounts::labels(&config));
    for session in sessions.iter_mut() {
        if session.get("account_label").is_none() && session.get("account_id").is_some() {
            let id = string(session, "account_id").to_owned();
            let label = labels.as_ref().and_then(|labels| labels.get(&id).cloned()).unwrap_or_else(|| id.clone());
            session["account_label"] = json!(label);
        }
    }
    // The pane folders are read after the lock is released, when a session may
    // have been renamed since `snapshot`. They go by the session IDs, which a
    // rename keeps, to the names that `snapshot` listed (zerus-18u1).
    workspace::enrich(sessions, &workspace::snapshot_cwds(names));
    waits::decorate(sessions);
    // U30: after the waits, which name a coordination reason.
    session_state::decorate(sessions);
}

/// The rows a terminal listing starts from, before `merge_listing` adds what
/// the saved bindings say.
pub(crate) fn listed_rows(listing: &backend::Listing) -> Vec<Value> {
    listing.sessions.iter().map(|row| {
        let name = row.name.as_str();
        let mut parts = name.splitn(3, '/'); let cmd = parts.next().unwrap_or("");
        let project = parts.next().filter(|s| !s.is_empty()); let tag = parts.next().filter(|s| !s.is_empty());
        let ttys: Vec<&str> = listing.clients.iter().filter_map(|client| if client.session == name { Some(client.terminal.as_str()) } else { None }).collect();
        json!({"name":name,"cmd":cmd,"project":project,"tag":tag,"attached":row.attached,"clients":ttys,"created":row.created,"launch_id":row.launch_id})
    }).collect()
}

/// Each listed session's immutable ID (`$N`) by its listed name.
pub(crate) fn listed_ids(listing: &backend::Listing) -> BTreeMap<String, String> {
    listing.sessions.iter().map(|row| (row.name.clone(), row.id.clone())).collect()
}

/// Select a tracked conversation or explicit pending request by exact ID/home.
/// Archives are separate instances and require an explicit archive restore.
pub fn find_requested_session(agent: &str, requested: &str) -> Result<Option<String>> {
    local_sessions()?;
    archive::reconcile()?;
    let _guard = lock(None)?;
    let snapshot = live()?;
    let archived = archive::records()?;
    let mut names = Vec::new();
    for record in records()? {
        if string(&record, "agent") != agent
            || requested.is_empty()
            || archive::equivalent(&record, &archived)
            || !attempts::agent_home_matches(&record, agent)?
        {
            continue;
        }
        let name = string(&record, "name");
        if snapshot.contains_key(name) && !matches(&record, snapshot.get(name)) {
            continue;
        }
        let confirmed = string(&record, "conversation_id") == requested;
        let pending = string(&record, "conversation_id").is_empty()
            && string(&record, "requested_id") == requested
            && string(&record, "expected_id") == requested
            && if snapshot.contains_key(name) {
                string(&record, "error").is_empty()
            } else {
                attempts::retryable(&record, Some(requested))
            };
        if !confirmed && !pending {
            continue;
        }
        names.push(name.to_owned());
    }
    match names.len() {
        0 => Ok(None),
        1 => Ok(names.pop()),
        _ => Err(format!(
            "conversation {requested} is tracked under multiple names ({}); choose one with -n",
            names.join(", ")
        )),
    }
}

pub fn startup_failure(name: &str) -> Result<Option<String>> {
    local_sessions()?;
    attempts::startup_failure(name)
}

/// True means an exact confirmed binding; false is a matching pending request.
/// A live name must still belong to the same pane/run, not merely an old record.
pub fn check_requested_session(name: &str, agent: &str, requested: &str) -> Result<bool> {
    local_sessions()?;
    let _guard = lock(None)?;
    let snapshot = live()?;
    if !record_path(name).exists() {
        return if snapshot.contains_key(name) {
            Err(format!(
                "{name}: live session is not tracked; choose another name"
            ))
        } else {
            Ok(false)
        };
    }
    let record = read(name)?;
    let is_live = snapshot.contains_key(name);
    if string(&record, "agent") != agent
        || !attempts::agent_home_matches(&record, agent)?
        || (is_live && !matches(&record, snapshot.get(name)))
    {
        return Err(format!("{name}: existing session belongs to a different or untracked context; choose another name"));
    }
    if !requested.is_empty() && string(&record, "conversation_id") == requested {
        if is_live && !string(&record, "error").is_empty() {
            return Err(format!(
                "{name}: live conversation identity is unconfirmed; choose another name"
            ));
        }
        return Ok(true);
    }
    let pending_matches = !requested.is_empty()
        && string(&record, "conversation_id").is_empty()
        && string(&record, "expected_id") == requested
        && string(&record, "requested_id") == requested;
    if pending_matches
        && ((is_live && string(&record, "error").is_empty())
            || (!is_live && attempts::retryable(&record, Some(requested))))
    {
        return Ok(false);
    }
    Err(format!("{name}: existing session does not match requested conversation {requested}; choose another name"))
}

pub fn fork_session(
    name: &str,
    tag: Option<&str>,
    archive_id: Option<&str>,
    expected_run: Option<&str>,
    expected_conversation: Option<&str>,
    dry: bool,
) -> Result<String> {
    local_sessions()?;
    fork::start(
        name,
        tag,
        archive_id,
        expected_run,
        expected_conversation,
        dry,
    )
}

pub(crate) fn reference_details(name: &str, archive: &str, agent: &str) -> Result<Value> {
    local_sessions()?;
    if opencode::exists(name) { if !archive.is_empty() {return Err("OpenCode archives are unavailable".into());} return opencode::inspect(name, if agent.is_empty() {None} else {Some(agent)}); }
    if dsh::exists(name) {
        if !archive.is_empty() { return Err("DeepSeek archives are unavailable".into()); }
        return dsh::inspect(name, if agent.is_empty() { None } else { Some(agent) });
    }
    let archive = if archive.is_empty() { None } else { Some(archive) };
    if agent.is_empty() { journal::inspection(name, 0, archive, false) }
    else { subagents::inspection(name, agent, archive) }
}

pub(crate) fn inspect_reference(reference: &crate::session_reference::Reference) -> Result<Value> {
    local_sessions()?;
    let matches_reference = |record: &Value| {
        string(record, "name").split('/').next() == reference.name.split('/').next()
            && string(record, "archive_id") == reference.archive
            && if reference.conversation.is_empty() {
                string(record, "run_id") == reference.run && string(record, "conversation_id").is_empty()
            } else { string(record, "conversation_id") == reference.conversation }
    };
    let name = {
        let _guard = lock(None)?;
        let mut candidates = if reference.archive.is_empty() { records()? } else { archive::records()? };
        if reference.archive.is_empty() { candidates.extend(dsh::snapshots()?); candidates.extend(opencode::snapshots()?); }
        let mut matching = candidates.iter().filter(|record| matches_reference(record));
        let first = matching.next().ok_or("The referenced conversation is no longer bound to a saved session; it may have been cleared or forgotten")?;
        if matching.next().is_some() { return Err("Ambiguous session reference: multiple saved bindings match this conversation".into()); }
        string(first, "name").to_owned()
    };
    let parent = reference_details(&name, &reference.archive, "")?;
    if !matches_reference(&parent) { return Err("Session identity changed while resolving its reference".into()); }
    let mut output = if reference.agent.is_empty() { parent }
        else { reference_details(&name, &reference.archive, &reference.agent)? };
    if !reference.agent.is_empty() && output["parent_conversation_id"].as_str() != Some(reference.conversation.as_str()) {
        return Err("Parent conversation changed while resolving its child reference".into());
    }
    if reference.archive.is_empty() && reference.agent.is_empty() && !opencode::exists(&name) && !dsh::exists(&name) {
        read_marks::observation::enrich(&mut output);
    }
    output["session_reference"] = json!({"machine_id":reference.machine,"host":reference.host,"original_name":reference.name,
        "name":name,"conversation_id":reference.conversation,"agent_id":reference.agent});
    Ok(output)
}

/// Pastes a task prompt or cancel message into `name` for its current run and
/// conversation, the way `hgs send` does, and returns the durable receipt.
pub(crate) fn submit_task_message(session: &zerus_replica::SessionKey, text: &str, attachments: &[Value]) -> Result<Value> {
    local_sessions()?;
    input::submit_task(session, text, attachments)
}

pub(crate) fn prepare_task_reply(session: &zerus_replica::SessionKey, dispatch: &str, id: &str, text: &str, attachments: &[Value]) -> Result<Value> {
    local_sessions()?;
    input::task_reply(session, dispatch, id, text, attachments)
}
pub(crate) fn submit_task_reply(name: &str, request: &Value) -> Result<Value> {
    input::submit(name, request.to_string().as_bytes(), None)
}
pub(crate) fn task_reply_receipt(name: &str, request: &Value) -> Result<Option<Value>> {
    input::task_reply_receipt(name, request)
}

/// Task routing uses saved identity metadata only: no provider inspection,
/// history, process reconciliation or native API calls for each task row.
pub(crate) fn task_bindings() -> Result<Vec<Value>> {
    task_bindings_in(&root())
}

/// The record fields task routing reads (`tasks::sessions`).
const TASK_BINDING_FIELDS: [&str; 6] = ["name", "run_id", "launch_id", "journal_name", "legacy_shadow", "rename_shadow"];

/// A saved record reduced to [`TASK_BINDING_FIELDS`]. Every other field, such
/// as up to 128 shell jobs with their full command texts, is skipped while
/// parsing rather than built into a value.
struct TaskBinding(serde_json::Map<String, Value>);

impl<'de> serde::Deserialize<'de> for TaskBinding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Fields;
        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = TaskBinding;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a session record object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<TaskBinding, A::Error> {
                let mut row = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if TASK_BINDING_FIELDS.contains(&key.as_str()) {
                        row.insert(key, map.next_value()?);
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(TaskBinding(row))
            }
        }
        deserializer.deserialize_map(Fields)
    }
}

/// [`task_bindings`] below the state directory `root`. A record that cannot be
/// read is reported and skipped; one removed meanwhile is skipped silently.
/// One buffer, bounded by the session graph's record limit, serves every
/// record: a record it indexes also routes tasks.
pub(crate) fn task_bindings_in(root: &Path) -> Result<Vec<Value>> {
    use std::fs;
    let limit = session_graph::MAX_RECORD;
    let mut bytes = Vec::new();
    let mut rows = Vec::new();
    for directory in [root.to_owned(), root.join("bindings"), root.join("dsh/bindings"), root.join("opencode/bindings")] {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().is_none_or(|s| s != "json") || stem.len() != 64 || !stem.bytes().all(|c| c.is_ascii_hexdigit()) { continue; }
            let row = (|| -> Result<Option<Value>> {
                let file = match crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.to_string()),
                };
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                let too_large = || format!("larger than {} MiB", limit >> 20);
                if !metadata.is_file() { return Err("not a regular file".into()); }
                if metadata.len() > limit { return Err(too_large()); }
                bytes.clear();
                file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                if bytes.len() as u64 > limit { return Err(too_large()); }
                // Parser text may quote private record data; report its position only.
                let TaskBinding(row) = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid saved binding at line {} column {}", e.line(), e.column()))?;
                let row = Value::Object(row);
                Ok((row["rename_shadow"] != true && !string(&row, "name").is_empty() && !string(&row, "run_id").is_empty()).then_some(row))
            })();
            match row {
                Ok(Some(row)) => rows.push(row),
                Ok(None) => {}
                Err(error) => eprintln!("hgs: cannot read {} for task routing: {error}", path.display()),
            }
        }
    }
    Ok(rows)
}

/// Compatibility entry point used by `hgs __state` and the legacy hook shim.
pub fn dispatch(args: &[String]) -> Result<i32> {
    if args.first().map(String::as_str)==Some("rules") { return iron_rules::dispatch(&args[1..]); }
    // Receipt observation needs neither a native host nor a state writer.
    if args.first().map(String::as_str)==Some("session-action-status") { return session_action::status_dispatch(&args[1..]); }
    // Coordination metadata never starts or controls a native session host.
    if args.first().map(String::as_str)==Some("wait") { return waits::dispatch(&args[1..]); }
    // WhiteWh fork: agent teams.
    if args.first().map(String::as_str)==Some("team") { return teams::commands::dispatch(&args[1..]); }
    if args.first().map(String::as_str)==Some("repository") { return repositories::dispatch(&args[1..]); }
    #[cfg(unix)]
    if args.first().map(String::as_str)==Some("repository-git-worker") { return repositories::git_worker(&args[1..]); }
    #[cfg(unix)]
    if args.first().map(String::as_str)==Some("worktree-request-worker") { return worktree_requests::worker(&args[1..]); }
    // Settings this computer keeps for its hooks (U19, U22): a control console
    // reads and changes them too.
    if args.first().map(String::as_str)==Some("subagents-confirm") { return subagents_confirm::dispatch(&args[1..]); }
    if args.first().map(String::as_str)==Some("session-upload-block") { return session_upload::dispatch(&args[1..]); }
    session_operations()?;
    let Some((command, args)) = args.split_first() else {
        return Err("missing state operation".into());
    };
    // U53: the owner moves one running session to another account of its agent.
    if command == "switch-account" { return switch_account::dispatch(args); }
    // WhiteWh fork: `hgs run`, a build Zerus tracks as the session's work (U30).
    if command == "build" { return builds::dispatch(args); }
    if command == "worktrees" { return worktrees::dispatch(args); }
    if command == "git-status" { return git_status::dispatch(args); }
    if command == "git-diff" { return git_status::dispatch_diff(args); }
    if command == "dirs" {
        if args.first().map(String::as_str) == Some("--create") {
            let request_id;
            let (path, id) = match args {
                [_, path] => {
                    request_id = uuid::Uuid::new_v4().to_string();
                    (path, request_id.as_str())
                }
                [_, path, option, id] if option == "--request-id" => (path, id.as_str()),
                _ => return Err("usage: hgs dirs --create <path> [--request-id UUID]".into()),
            };
            println!("{}", create_directory(path, id)?);
            return Ok(0);
        }
        let creation=args.first().map(String::as_str)==Some("--creation");
        let args=if creation { &args[1..] }else{args};
        let hidden = args.first().map(String::as_str) == Some("--hidden");
        let paths = if hidden { &args[1..] } else { args };
        if paths.len() > 1 {
            return Err("usage: hgs dirs [--hidden] [path]".into());
        }
        println!(
            "{}",
            directories(paths.first().map(String::as_str).unwrap_or("~"), hidden, creation)?
        );
        return Ok(0);
    }
    if command == "new-tag" {
        println!("work-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        return Ok(0);
    }
    // Launch arguments belong to the agent, including a possible --dry-run.
    if command == "run" {
        return lifecycle::run(args);
    }
    if command == "hook" {
        return hooks::hook();
    }
    if command == "answer-channel" {
        return answer_channel::run();
    }
    if command == "graph" { return session_graph::dispatch(args); }
    if command == "statusline" {
        return claude_statusline::run();
    }
    if command == "voice" { return voice::dispatch(args); }
    if command == "file-read" { return computer_files::dispatch(args); }
    if command == "attachment" {
        return attachments::dispatch(args);
    }
    if command == "recovery" {
        return recovery::dispatch(args);
    }
    if command == "read" { return read_marks::dispatch(args); }
    if command == "notifications" { return notifications::dispatch(args); }
    if command == "history" { return conversation_history::dispatch(args); }
    if command == "history-projection" { return conversation_history::projection_dispatch(args); }
    if command == "terminal" { return terminal::dispatch(args); }
    if command == "session-action" { return session_action::dispatch(args); }
    if command == "compact-context" { return compact_context::dispatch(args); }
    if command == "clear-context" { return clear_context::dispatch(args); }
    if command == "interrupt" { return interrupt::dispatch(args); }
    if command == "processes" { return processes::dispatch(args); }
    if args.first().is_some_and(|name| opencode::exists(name)) { return opencode::dispatch(command,args); }
    if args.first().is_some_and(|name| dsh::exists(name)) {
        return dsh::dispatch(command, args);
    }
    if command == "settings" {
        return effort::dispatch_settings(args);
    }
    if command == "effort" {
        return effort::dispatch(args);
    }
    if command == "search" {
        return search::dispatch(args);
    }
    if command == "schedules" {
        return Err("Native schedule management is available for DeepSeek sessions; other agents report schedules read-only".into());
    }
    if command == "send-now" { return input_queue::dispatch(args); }
    if command == "send-cancel" { return input_hold::dispatch_cancel(args); }
    if command == "deliver-held" { return input_hold::deliver(args); }
    if command == "send" {
        return input::dispatch(args);
    }
    if command == "answer" {
        return questions::dispatch(args);
    }
    let dry = args.iter().any(|arg| arg == "--dry-run");
    let args: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--dry-run")
        .cloned()
        .collect();
    match command.as_str() {
        "terminate" => return terminate::dispatch(&args, dry),
        "inspect" => {
            let name = args
                .first()
                .ok_or("usage: hgs inspect <session> [--archive ID] [--after cursor] [--skip-processes] [--read-observation]")?;
            let mut after = 0;
            let mut archive_id = None;
            let mut agent_id = None;
            let mut include_processes = true;
            let mut read_observation = false;
            let mut index = 1;
            while index < args.len() {
                if args[index] == "--skip-processes" { include_processes = false; index += 1; continue; }
                if args[index] == "--read-observation" { read_observation = true; index += 1; continue; }
                let value = args.get(index + 1).ok_or("missing inspect option value")?;
                match args[index].as_str() {
                    "--after" => {
                        after = value
                            .parse::<i64>()
                            .map_err(|_| "cursor must be nonnegative")?
                    }
                    "--archive" => archive_id = Some(value.as_str()),
                    "--agent" => agent_id = Some(value.as_str()),
                    _ => return Err("invalid inspect option".into()),
                }
                index += 2;
            }
            if after < 0 {
                return Err("cursor must be nonnegative".into());
            }
            let mut output = if let Some(id) = agent_id {
                subagents::inspection(name, id, archive_id)?
            } else {
                journal::inspection(name, after, archive_id, include_processes)?
            };
            if read_observation && archive_id.is_none() && agent_id.is_none() {
                read_marks::observation::enrich(&mut output);
            }
            // #569: this computer's refused private stores, with their fix;
            // empty when none is refused.
            if output.is_object() {
                output["private_stores"] = json!(crate::private_stores::refused());
            }
            println!("{output}");
        }
        "archive" => {
            if args.len() != 1 {
                return Err("usage: hgs archive <session>".into());
            }
            return archive::manual(&args[0], dry);
        }
        "pause" => return lifecycle::pause(&args, dry),
        "resume" => return lifecycle::resume(&args, dry),
        "fresh" => return lifecycle::fresh(&args, dry),
        "rename" => return rename::dispatch(&args, dry),
        "merge" => println!("{}", merge_snapshot(stdin_json()?)?),
        "list" => {
            let snapshot = live()?;
            let box_value = merge_snapshot(json!({"sessions": []}))?;
            for session in box_value["sessions"].as_array().unwrap() {
                if !snapshot.contains_key(string(session, "name"))
                    && string(session, "state") != "archived"
                {
                    println!(
                        "  {:40} {} (hgs resume)",
                        string(session, "name"),
                        string(session, "state")
                    );
                }
            }
        }
        "exists" => {
            archive::reconcile()?;
            let name = args.first().ok_or("missing session name")?;
            return Ok(if attempts::binding_exists(name)? {
                0
            } else {
                1
            });
        }
        "forget" => {
            if args.len() == 3 && args[1] == "--archive" {
                archive::forget(&args[0], &args[2], dry)?;
                return Ok(0);
            }
            if args.len() != 1 {
                return Err("usage: hgs forget <session> [--archive ID]".into());
            }
            if dry {
                println!("hgs: would forget {}", args[0]);
                return Ok(0);
            }
            let _guard = lock(None)?;
            if record_path(&args[0]).exists() {
                rename::remove_legacy(&read(&args[0])?)?;
            }
            match std::fs::remove_file(record_path(args.first().ok_or("missing session name")?)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        _ => return Err(format!("unknown state operation: {command}")),
    }
    Ok(0)
}

#[cfg(test)]
mod task_binding_tests {
    use super::*;
    use std::fs;

    #[test]
    fn task_bindings_keep_routing_fields_of_any_reasonable_record() {
        let state = tempfile::tempdir().unwrap();
        let root = state.path();
        let file = |n: u8| root.join(format!("{n:064x}.json"));
        let jobs: serde_json::Map<String, Value> = (0..128)
            .map(|i| (format!("main:call-{i}"), json!({"command": "y".repeat(12_000), "output": "z".repeat(4096)})))
            .collect();
        let large = json!({"version":1,"name":"codex/big","run_id":"run-big","launch_id":"launch-big","journal_name":"codex/first",
            "legacy_shadow":{"name":"codex/older","run_id":"run-big"},"conversation_id":"conversation","shell_jobs":jobs});
        fs::write(file(1), serde_json::to_vec(&large).unwrap()).unwrap();
        assert!(fs::metadata(file(1)).unwrap().len() > 1024 * 1024);
        // Damaged and absurdly large records are reported and skipped; other
        // JSON files are not session records.
        fs::write(file(2), br#"{"name":"codex/damaged","run_id":"#).unwrap();
        fs::write(file(3), br#"["codex/listed","run-listed"]"#).unwrap();
        fs::File::create(file(4)).unwrap().set_len(session_graph::MAX_RECORD + 1).unwrap();
        fs::write(root.join("presence.json"), br#"{"name":"codex/other","run_id":"run-other"}"#).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(file(1), file(5)).unwrap();
        let rows = task_bindings_in(root).unwrap();
        assert_eq!(rows, vec![json!({"name":"codex/big","run_id":"run-big","launch_id":"launch-big","journal_name":"codex/first",
            "legacy_shadow":{"name":"codex/older","run_id":"run-big"}})]);
        assert_eq!(task_bindings_in(&root.join("missing")).unwrap(), Vec::<Value>::new());
    }
}

#[cfg(all(test, not(unix)))]
mod control_console_tests {
    use super::*;
    use crate::platform::LOCAL_SESSIONS_UNSUPPORTED as UNSUPPORTED;

    /// Set for the process `entry_points_answer_as_a_control_console` starts.
    const PROBE: &str = "HGS_TEST_CONTROL_CONSOLE";

    /// Run by `entry_points_answer_as_a_control_console` in a process whose
    /// profile is a new temporary folder: whether local sessions are enabled is
    /// the `local-sessions` marker in the profile (spec D2), and the user running
    /// these tests may have enabled them.
    #[test]
    #[ignore = "run by entry_points_answer_as_a_control_console"]
    fn control_console_probe() {
        if std::env::var_os(PROBE).is_none() {
            return;
        }
        assert_eq!(dispatch(&["list".into()]).unwrap_err(), UNSUPPORTED);
        assert_eq!(
            merge_snapshot(json!({"sessions":[]})).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(account_context("claude/p").unwrap_err(), UNSUPPORTED);
        assert_eq!(startup_failure("claude/p").unwrap_err(), UNSUPPORTED);
        assert_eq!(recovery_worker_running(), None);
        assert_eq!(
            worktree_catalog(Path::new(r"C:\work")),
            json!({"state":"unavailable","detail":UNSUPPORTED,"stale":true,"worktrees":[]})
        );
        let error = with_launch_binding("claude/p", |_| Ok(())).unwrap_err();
        assert_eq!(error.to_string(), UNSUPPORTED);

        // The sessions of an isolated test host reach `dispatch` (their hooks)
        // without the marker: under the override, for an existing state
        // directory that HGS_STATE_DIR names and that is not the profile's
        // default one. Nothing else opens.
        use crate::host::gate::ALLOW_UNSTAGED;
        let hook = || dispatch(&["hook".into()]);
        for name in ["HGS_SESSION", "HGS_RUN_ID"] {
            std::env::remove_var(name);
        }
        let state = PathBuf::from(std::env::var_os("HGS_STATE_DIR").unwrap());
        std::env::set_var(ALLOW_UNSTAGED, "1");
        assert_eq!(hook().unwrap_err(), UNSUPPORTED);
        std::fs::create_dir_all(&state).unwrap();
        assert_eq!(hook(), Ok(0));
        assert_eq!(session_operations(), Ok(()));
        assert_eq!(local_sessions().unwrap_err(), UNSUPPORTED);
        assert_eq!(
            merge_snapshot(json!({"sessions":[]})).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(startup_failure("claude/p").unwrap_err(), UNSUPPORTED);
        std::env::set_var(ALLOW_UNSTAGED, "true");
        assert_eq!(hook().unwrap_err(), UNSUPPORTED);
        std::env::set_var(ALLOW_UNSTAGED, "1");
        let profile = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
        let default = profile.join(r".local\state\hgs\sessions");
        std::fs::create_dir_all(&default).unwrap();
        std::env::set_var("HGS_STATE_DIR", &default);
        assert_eq!(hook().unwrap_err(), UNSUPPORTED);
        std::env::set_var("HGS_STATE_DIR", &state);
        std::env::remove_var(ALLOW_UNSTAGED);
        assert_eq!(hook().unwrap_err(), UNSUPPORTED);

        // The marker alone opens the entry points.
        let layout = crate::host::staging::Layout::current().unwrap();
        assert!(crate::host::staging::enable(&layout.marker).unwrap());
        assert_eq!(local_sessions(), Ok(()));
    }

    #[test]
    fn entry_points_answer_as_a_control_console() {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "state::control_console_tests::control_console_probe",
                "--exact",
                "--ignored",
                "--test-threads=1",
            ])
            .env(PROBE, "1")
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .env("HGS_STATE_DIR", dir.path().join("state"))
            .env_remove("HGS_CONFIG_DIR")
            .env_remove("XDG_STATE_HOME")
            .env_remove(crate::host::gate::ALLOW_UNSTAGED)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // The probe ran, rather than matching no test.
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
