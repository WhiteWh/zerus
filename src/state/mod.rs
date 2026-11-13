//! Durable agent/session bindings. Histories remain owned by the agent CLIs.
//!
//! Version-one manifests and journal rows deliberately remain compatible with
//! the original helper: an upgrade must never orphan a saved conversation.
mod answer_channel;
mod agent_hint;
mod archive;
pub(crate) mod archive_catalog;
mod archive_references;
mod claude_trust;
mod claude_permission;
mod claude_resume;
mod claude_usage_limit;
mod claude_bypass;
mod claude_question;
mod claude_statusline;
mod claude_subagents;
mod attachments;
mod delivery;
mod receipt_index;
mod computer_files;
pub(crate) mod voice;
mod interrupt;
mod clear_context;
mod codex_clear_recovery;
mod codex_sqlite;
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
mod end_children;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod guarded_recovery;
mod external_agents;
mod fork;
mod goals;
mod hooks;
mod heartbeat;
mod command_guard;
pub(crate) mod hook_health;
mod input;
mod input_hold;
pub(crate) use input::task_refusal;
pub(crate) use input::{Lookup as ReceiptLookup, Submission};
// The session resume actor's readiness rule (zerus-5wmh.66.4.9), on its
// platforms only.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use input::{resumed_readiness, Readiness as ResumedReadiness};
mod input_queue;
mod journal;
mod keep_warm;
mod kimi_cache_hint;
mod kimi_messages;
mod kimi_completion;
mod kimi_approval;
mod kimi_goal;
mod kimi_plan;
mod kimi_trust;
mod kimi_tui_choice;
pub(crate) mod kimi_permissions;
mod lifecycle;
mod kimi_launch;
mod provider_errors;
mod recovery;
mod service_worker;
mod session_limits;
pub(crate) mod session_id;
mod session_index;
mod labels;
/// How every reader decides what the label index proves (zerus-ucck P3a);
/// the native readers that need it outside this module run on Unix.
#[cfg(unix)]
pub(crate) use labels::prove as prove_label_index;
/// How every reader of the ID layout checks a record's own identity.
#[cfg(unix)]
pub(crate) use storage::is_id_layout_record;
mod provider_messages;
mod provider_binding;
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
/// The per-child switch of the owner-approved question route (zerus-sa9qn.32).
pub(crate) mod question_routing;
pub(crate) mod parent_link;
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
pub(crate) mod project_memory;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod task_reconcile;
mod subagents;
mod subagents_confirm;
mod session_upload;
mod session_state;
pub(crate) mod slow_calls;
mod switch_account;
mod switch_provider;
mod builds;
pub(crate) mod build_cache_server;
pub(crate) mod build_dirs;
pub(crate) mod subagent_lifecycle;
mod native_subagents;
mod supervisor;
mod tasks;
// WhiteWh fork: agent teams.
pub(crate) mod teams;
pub(crate) mod iron_rules;
pub(crate) mod task_controls;
mod telemetry;
mod terminate;
mod terminating;
mod untracked_shell;
mod session_action;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use session_action::fixture_receipt as lifecycle_fixture_receipt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use session_action::observe_held as observe_held_lifecycle_receipt;
pub(crate) use session_action::{
    observe_original as observe_lifecycle_receipt, status_machine as lifecycle_receipt_machine,
};
mod shown_path;
mod startup_choice;
mod claude_theme;
mod terminal;
mod conversation_history;
mod usage;
mod usage_samples;
mod workspace;
mod git_status;
pub(crate) mod repositories;
mod launch_observation;
pub(crate) mod launch_identity;
/// Evidence about original launch `launch` under this process's own state
/// root (`HGS_STATE_DIR` or its default), read within `deadline`. A root that
/// cannot be resolved without creating it is Unknown.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn observe_own_launch(
    launch: &str,
    deadline: std::time::Instant,
) -> zerus_client_core::desktop::creation_launch::Observation {
    match crate::platform::canonical(storage::root()) {
        Ok(root) => launch_observation::observe(&root, launch, deadline),
        Err(_) => zerus_client_core::desktop::creation_launch::Observation::Unknown,
    }
}
mod worktrees;
mod worktree_requests;
// The creation worktree actor's only entry into the owned effect.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use {
    worktree_requests::{create_owned as create_worktree_owned, original_receipt as worktree_request_receipt, ReceiptRoot as WorktreeReceiptRoot},
    worktrees::{Control as WorktreeControl, Step as WorktreeStep},
};
mod worktree_cleanup;
mod worktree_caches;
mod worktree_inspection;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod worktree_programs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use worktree_programs::{ProgramRemovalClaim, ProgramStopClaim};
mod worktree_lifecycle;
mod worktree_lock;
mod worktree_maintenance;

/// Read the selected account's native catalog without starting an agent.
pub(crate) fn account_models(provider: &str, home: &std::path::Path) -> serde_json::Value {
    effort::account_models(provider, home)
}
/// The DeepSeek account's model catalog from its resident host, read only.
pub(crate) fn dsh_account_models(home: &std::path::Path) -> serde_json::Value {
    dsh::account_models(home)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn remember_picker_caller_machine(config:&crate::config::Config,expected:&str) {
    effort::remember_picker_caller_machine(config,expected);
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
pub(crate) use session_project::{with_pinned_session_binding, with_session_project_binding, Pinned};
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

/// Task #349: the state root the private journal writes under, a test's own
/// state root included (the same folder every state-derived store uses).
pub(crate) fn state_root() -> PathBuf {
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
    if terminal_live(&snapshot, &record)? && !matches(&record, panes_of(&snapshot, &record)) {
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

/// Task #44 (team ede724c4): how long a fixed piece of CPU work takes on this
/// machine right now (the fastest of three runs of two million SipHash
/// rounds). A test bounds its own work as a multiple of it, never by a
/// wall-clock figure a loaded machine exceeds.
#[cfg(test)]
pub(crate) fn reference_work() -> std::time::Duration {
    use std::hash::{Hash, Hasher};
    (0..3)
        .map(|_| {
            let started = std::time::Instant::now();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for n in 0..2_000_000u64 {
                n.hash(&mut hasher);
            }
            std::hint::black_box(hasher.finish());
            started.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// Task #44: runs `work` and fails when it took more than `ratio` times the
/// machine's reference work, measured right before and right after it (the
/// slower of the two). Load slows the reference as much as the work, so only
/// work that grew (a quadratic loop, a scan per item) exceeds the ratio.
#[cfg(test)]
pub(crate) fn within_reference<T>(ratio: u32, label: &str, work: impl FnOnce() -> T) -> T {
    let before = reference_work();
    let started = std::time::Instant::now();
    let out = work();
    let took = started.elapsed();
    let reference = before.max(reference_work());
    eprintln!("{label}: {took:?}, {:.1} times the reference work ({reference:?})", took.as_secs_f64() / reference.as_secs_f64().max(1e-9));
    assert!(
        took <= reference * ratio,
        "{label}: {took:?} is more than {ratio} times this machine's reference work ({reference:?})"
    );
    out
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
    // Task 193 (owner decision, 2026-11-07): a listing reads committed state and
    // never takes or waits for the writer's state lock. The pane snapshot and
    // the bindings can lag one writer change (about 2 s); a rename is still
    // followed by session ID below, and a writer's next reconcile records what
    // this read did not. A row read while a write is in flight is its last
    // committed state.
    let mut phases = slow_calls::Phases::start();
    let ((snapshot, mut names), (archived, bindings)) = if root().exists() {
        let (snapshot, names) = live_sessions()?;
        let saved = if include_archives {
            (archive::records()?, storage::records()?)
        } else {
            (Vec::new(), archive::read_active(&snapshot)?)
        };
        ((snapshot, names), saved)
    } else {
        (live_sessions()?, Default::default())
    };
    phases.mark("reads");
    let marks = read_marks::load();
    let notification_preferences = notifications::load();
    phases.mark("marks");
    let keep_warm_settings = keep_warm::load();
    let question_routing = question_routing::load();
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
    let shared = Shared { marks: &marks, preferences: notification_preferences.as_ref(), keep_warm: keep_warm_settings.as_ref(), question_routing: question_routing.as_ref(), budget: read_marks::question_budget() };
    let mut process_work = processes::WorkSample::default();
    // Task 234: the rows loop's parts, summed over its rows.
    let (mut live_ms, mut probe_ms, mut saved_ms, mut saved_rows) = (0u64, 0u64, 0u64, 0u64);
    let ms = |at: std::time::Instant| at.elapsed().as_millis() as u64;
    for record in bindings {
        if archive::equivalent(&record, &archived) {
            continue;
        }
        let name = string(&record, "name");
        // Live rows are the terminals' (P3b): a record finds its row by its
        // own terminal, and that row then lists under the record's label.
        // A row under its terminal name that is another session's never
        // hides this one (#1445 review): it lists as saved below.
        if let Some(index) = by_name.get(terminal_name(&record)) {
            if matches(&record, panes_of(&snapshot, &record)) {
                label_row(&mut sessions[*index], &record);
                workspace::relabel(&mut names, terminal_name(&record), name);
                let at = std::time::Instant::now();
                live_row(&mut sessions[*index], record, &shared, &mut process_work)?;
                live_ms += ms(at);
                continue;
            }
        }
        if string(&record, "conversation_id").is_empty() {
            continue;
        }
        // Only the session's own launch still starting keeps its saved row
        // out; a terminal another session holds under its terminal name never
        // hides it (#1445 review), nor does one that cannot be read.
        let at = std::time::Instant::now();
        let starting = terminal_live(&snapshot, &record).unwrap_or(false);
        probe_ms += ms(at);
        if starting {
            continue;
        }
        let at = std::time::Instant::now();
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
        keep_warm::enrich(&mut session, keep_warm_settings.as_ref(), &record);
        question_routing::enrich(&mut session, question_routing.as_ref());
        sessions.push(session);
        saved_ms += ms(at);
        saved_rows += 1;
    }
    phases.note("rows_live", live_ms);
    phases.note("rows_terminal_probe", probe_ms);
    phases.note("rows_saved", saved_ms);
    phases.note("rows_saved_count", saved_rows);
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
    phases.mark("rows");
    sessions.extend(dsh::snapshots()?);
    phases.mark("dsh");
    sessions.extend(opencode::snapshots()?);
    phases.mark("opencode");
    finish_rows(sessions, &marks, &names);
    phases.mark("finish_rows");
    // Task 193: a listing over SLOW_MS is one slow-calls line with its phases.
    phases.finish("ls", string(&box_value, "host"));
    Ok(box_value)
}

/// What every row of one listing shares besides its process sample.
struct Shared<'a> {
    marks: &'a Value,
    preferences: Option<&'a notifications::Store>,
    keep_warm: Option<&'a keep_warm::Store>,
    question_routing: Option<&'a question_routing::Store>,
    /// Bounds question indexing for the whole listing.
    budget: std::time::Instant,
}

/// The row of a live session whose pane runs its saved binding `record`.
/// `hgs watch` re-derives single rows through this same step
/// (`watch_source::rows`).
/// A live row of a terminal named otherwise than its session (P3b) lists
/// under the session's label, as every other row does.
fn label_row(session: &mut Value, record: &Value) {
    let name = string(record, "name");
    if string(session, "name") == name {
        return;
    }
    let mut parts = name.splitn(3, '/');
    session["cmd"] = json!(parts.next().unwrap_or(""));
    session["project"] = json!(parts.next().filter(|s| !s.is_empty()));
    session["tag"] = json!(parts.next().filter(|s| !s.is_empty()));
    session["name"] = json!(name);
}

fn live_row(session: &mut Value, record: Value, shared: &Shared, process_work: &mut processes::WorkSample) -> Result<()> {
    // Task 193: a listing's per-session cost, named in slow-calls when a row
    // takes 100 ms or more (the rows a listing cannot afford).
    let mut phases = slow_calls::Phases::start();
    // An approval answered in Terminal sends no hook.
    let record = kimi_completion::reconcile(claude_approval::reconcile(record));
    session["resumable"] = json!(
        !string(&record, "conversation_id").is_empty()
            && string(&record, "error").is_empty()
    );
    let summary = journal::summary(&record, true);
    phases.mark("summary");
    session["process_work"] = process_work.summary(&record, summary["activity"] == "idle" && summary["phase"] == "idle");
    // Task 278: the background jobs its hooks registered, whatever its activity.
    session["background_jobs"] = process_work.registered(&record, true);
    session
        .as_object_mut()
        .ok_or("invalid session")?
        .extend(summary.as_object().ok_or("invalid summary")?.clone());
    let questions = read_marks::pending_questions(&record, session, true, shared.budget);
    let pending = questions
        .as_ref()
        .ok()
        .and_then(|questions| questions.as_deref());
    parent_link::mark_human_request(session, pending, false);
    read_marks::enrich_listing(session, shared.marks, journal::journal_name(&record), questions);
    phases.mark("questions");
    notifications::enrich(session, shared.preferences, journal::journal_name(&record));
    phases.mark("notifications");
    keep_warm::enrich(session, shared.keep_warm, &record);
    phases.mark("keep_warm");
    question_routing::enrich(session, shared.question_routing);
    phases.mark("question_routing");
    if phases.total_ms() >= 100 {
        phases.finish_at("ls-row", string(session, "name"), 100);
    }
    Ok(())
}

/// The last steps of every listed row. `names` holds the session names of the
/// pane snapshot by session ID.
fn finish_rows(sessions: &mut [Value], marks: &Value, names: &BTreeMap<String, String>) {
    // Task 234: the parts of the listing's last pass, named in slow-calls.
    let mut phases = slow_calls::Phases::start();
    // DeepSeek and untracked live sessions are owned by their names.
    for session in sessions.iter_mut() {
        if session.get("attention_signature").is_none() {
            let owner = string(session, "name").to_owned();
            let questions=if session["agent"]=="opencode" {session["pending_questions"].as_array().cloned().unwrap_or_default()}else{Vec::new()};
            read_marks::enrich_listing(session, marks, &owner, Ok(Some(questions)));
        }
    }
    phases.mark("marks");

    for session in sessions.iter_mut() {
        if session.get("notification_preferences").is_none() {
            session["notification_preferences"] = Value::Null;
        }
    }

    // U53: every row names its account, label included; the catalog is read once.
    let labels = crate::config::Config::load().ok().map(|config| crate::accounts::labels(&config));
    phases.mark("account_labels");
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
    phases.mark("labels");
    let cwds = workspace::snapshot_cwds(names);
    phases.mark("cwds");
    workspace::enrich(sessions, &cwds);
    phases.mark("workspace");
    waits::decorate(sessions);
    phases.mark("waits");
    parent_link::decorate(sessions);
    crate::accounts::marks::decorate(sessions);
    phases.mark("parents");
    // U30: after the waits, which name a coordination reason.
    session_state::decorate(sessions);
    phases.mark("session_state");
    phases.finish_at("ls-finish", "", 100);
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
        if terminal_live(&snapshot, &record)? && !matches(&record, panes_of(&snapshot, &record)) {
            continue;
        }
        let confirmed = string(&record, "conversation_id") == requested;
        let pending = string(&record, "conversation_id").is_empty()
            && string(&record, "requested_id") == requested
            && string(&record, "expected_id") == requested
            && if terminal_live(&snapshot, &record)? {
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
/// The terminal session of the session `name` names (zerus-ucck P3b): its
/// record's terminal, read through the command's selection. With no record,
/// `name` itself names a terminal, as an untracked shell's does. A record
/// that cannot be read is an error, never the label's terminal.
pub(crate) fn terminal_of(name: &str) -> Result<String> {
    if local_sessions().is_err() {
        return Ok(name.to_owned());
    }
    // Presence without following a link (#1445 review): a record that is
    // there but cannot be read, a dangling link included, refuses; and a
    // selected record that vanished keeps the command's selection, so it
    // refuses too, never routing to the label's terminal.
    match std::fs::symlink_metadata(record_path(name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            session_id::verify_absent(name)?;
            Ok(name.to_owned())
        }
        Err(error) => Err(format!("{name}: {error}")),
        Ok(_) => read(name).map(|record| terminal_name(&record).to_owned()),
    }
}

pub fn check_requested_session(name: &str, agent: &str, requested: &str) -> Result<bool> {
    local_sessions()?;
    let _guard = lock(None)?;
    let snapshot = live()?;
    if !storage::label_taken(name) {
        return if snapshot.contains_key(name) {
            Err(format!(
                "{name}: live session is not tracked; choose another name"
            ))
        } else {
            Ok(false)
        };
    }
    let record = read(name)?;
    let is_live = terminal_live(&snapshot, &record)?;
    if string(&record, "agent") != agent
        || !attempts::agent_home_matches(&record, agent)?
        || (is_live && !matches(&record, panes_of(&snapshot, &record)))
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

/// Whether `record` is the session a reference names: its provider, archive,
/// conversation (or run, for a reference without one) and, when the
/// reference carries one, its durable session ID.
fn matches_reference(reference: &crate::session_reference::Reference, record: &Value) -> bool {
    string(record, "name").split('/').next() == reference.name.split('/').next()
        && string(record, "archive_id") == reference.archive
        && (reference.session.is_empty() || string(record, "session_id") == reference.session)
        && if reference.conversation.is_empty() {
            string(record, "run_id") == reference.run && string(record, "conversation_id").is_empty()
        } else { string(record, "conversation_id") == reference.conversation }
}

/// The current name of the one saved session a reference names, following
/// renames: a resolution, never an authorisation (zerus-ucck P1c).
pub(crate) fn resolve_reference(reference: &crate::session_reference::Reference) -> Result<String> {
    local_sessions()?;
    let _guard = lock(None)?;
    let mut candidates = if reference.archive.is_empty() { records()? } else { archive::records()? };
    if reference.archive.is_empty() { candidates.extend(dsh::snapshots()?); candidates.extend(opencode::snapshots()?); }
    let mut matching = candidates.iter().filter(|record| matches_reference(reference, record));
    let first = matching.next().ok_or("The referenced conversation is no longer bound to a saved session; it may have been cleared or forgotten")?;
    if matching.next().is_some() { return Err("Ambiguous session reference: multiple saved bindings match this conversation".into()); }
    Ok(string(first, "name").to_owned())
}

/// Effort and Settings resolve metadata before their keeper; ordinary reference resolution
/// acquires Root and refreshes presentation data, so it is not this lane.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn resolve_effort_reference(
    reference: &crate::session_reference::Reference,
) -> Result<String> {
    local_sessions()?;
    if !reference.archive.is_empty() {
        return Err(
            "The reference names an archived conversation; hgs effort needs a saved session".into(),
        );
    }
    let records = if reference.session.is_empty() {
        session_index::metadata_records_unrepaired()?
    } else {
        session_index::metadata_records_for_id_unrepaired(&reference.session)?
    };
    let mut matching = records
        .iter()
        .filter(|record| matches_reference(reference, record));
    let first=matching.next().ok_or("The referenced conversation is no longer bound to a saved session; it may have been cleared or forgotten")?;
    if matching.next().is_some() {
        return Err(
            "Ambiguous session reference: multiple saved bindings match this conversation".into(),
        );
    }
    let name = string(first, "name").to_owned();
    effort_select_metadata(
        first,
        if reference.conversation.is_empty() {
            &reference.run
        } else {
            ""
        },
        &reference.conversation,
    )?;
    Ok(name)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn effort_first_metadata(name: &str) -> Result<Value> {
    match provider_binding::Provider::named(name) {
        Some(provider) => provider.read_label(name),
        None => {
            let path = storage::label_record_path(name);
            let raw = labels::read_file(&path, storage::ID_RECORD_BYTES)
                .map_err(|e| e.to_string())?
                .ok_or("Metadata first holder is absent")?;
            storage::verify_metadata_at(&path, name, &raw)
        }
    }
}

/// Reusing a metadata selector can add constraints, never erase a reference's
/// original run/conversation or change its already selected ID route.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn effort_select_metadata(record: &Value, run: &str, conversation: &str) -> Result<()> {
    let name = string(record, "name");
    let id = string(record, "session_id");
    let id = if session_id::canonical(id) { id } else { "" };
    let prior = session_id::selected();
    if (!run.is_empty() && string(record, "run_id") != run)
        || (!conversation.is_empty() && string(record, "conversation_id") != conversation)
        || prior.as_ref().is_some_and(|prior| {
            prior.name != name
                || (!prior.session_id.is_empty() && prior.session_id != id)
                || (!prior.run.is_empty() && prior.run != string(record, "run_id"))
                || (!prior.conversation.is_empty()
                    && prior.conversation != string(record, "conversation_id"))
                || (prior.id_layout && !storage::in_id_layout(record))
        })
    {
        return Err("Original Effort selector identity or route changed".into());
    }
    let run = prior
        .as_ref()
        .map(|prior| prior.run.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(run);
    let conversation = prior
        .as_ref()
        .map(|prior| prior.conversation.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(conversation);
    session_id::select_guarded_route(name, id, run, conversation, storage::in_id_layout(record));
    Ok(())
}

/// Exact-ID frontend selection is a metadata route, never native admission.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn resolve_effort_id(id: &str) -> std::result::Result<String, Unresolved> {
    let records =
        session_index::metadata_records_for_id_unrepaired(id).map_err(|text| Unresolved {
            text,
            value: None,
            code: 1,
        })?;
    let mut names: Vec<_> = records
        .iter()
        .map(|record| string(record, "name").to_owned())
        .collect();
    names.sort();
    names.dedup();
    let name = match names.as_slice() {
        [] => {
            return Err(Unresolved {
                text: format!("No live or saved session has ID {id}"),
                value: None,
                code: 1,
            })
        }
        [name] => name.clone(),
        _ => {
            return Err(Unresolved {
                text: format!("Several sessions carry ID {id}: {}", names.join(", ")),
                value: None,
                code: session_id::AMBIGUOUS,
            })
        }
    };
    // The lookup retained the actual verified layout. Never re-probe and
    // fall back to a first label holder after an ID file disappears.
    let record = records
        .iter()
        .find(|record| storage::in_id_layout(record))
        .or_else(|| records.first())
        .ok_or_else(|| Unresolved {
            text: "The metadata session route disappeared".into(),
            value: None,
            code: 1,
        })?;
    effort_select_metadata(record, "", "").map_err(|text| Unresolved {
        text,
        value: None,
        code: 1,
    })?;
    Ok(name)
}

pub(crate) fn inspect_reference(reference: &crate::session_reference::Reference) -> Result<Value> {
    let name = resolve_reference(reference)?;
    let parent = reference_details(&name, &reference.archive, "")?;
    if !matches_reference(reference, &parent) { return Err("Session identity changed while resolving its reference".into()); }
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
/// `submit_task_reply` for an attempt's input: an exact submitted receipt,
/// a live witness that the payload was never offered, or unknown.
pub(crate) fn submit_task_reply_witnessed(name: &str, request: &Value) -> input::Submission {
    input::submit_witnessed(name, request.to_string().as_bytes())
}
pub(crate) fn task_reply_receipt_bound(
    name: &str,
    request_id: &str,
    digest: &str,
) -> Result<ReceiptLookup> {
    input::task_reply_receipt_bound(name, request_id, digest)
}
/// The receipt store's answer for `request`, typed (#1519 R2).
pub(crate) fn task_reply_lookup(name: &str, request: &Value) -> Result<ReceiptLookup> {
    input::task_reply_lookup(name, request)
}
pub(crate) fn task_reply_receipt(name: &str, request: &Value) -> Result<Option<Value>> {
    input::task_reply_receipt(name, request)
}

/// Task routing uses saved identity metadata only: no provider inspection,
/// history, process reconciliation or native API calls for each task row.
pub(crate) fn task_bindings() -> Result<Vec<Value>> {
    task_bindings_in(&root())
}

/// How a session command carries the identities it expects (zerus-ucck P3c).
pub(crate) enum LabelGuard {
    /// It carries none: a label several sessions share is refused at once.
    None,
    /// Given on its command line: they select now.
    Given { run: String, conversation: String },
    /// They arrive with its request, read later: the choice waits for them
    /// ([`expect`]), and every read of the label refuses until then.
    Request,
}

/// A label resolution that refused (zerus-ucck P3a, P3c): its text, the
/// ambiguity report's `--json` value when there is one, and its exit status.
#[derive(Debug)]
pub(crate) struct Unresolved {
    pub text: String,
    pub value: Option<Value>,
    pub code: i32,
}
impl Unresolved {
    fn ambiguous((text, value): (String, Value)) -> Self {
        Self { text, value: Some(value), code: session_id::AMBIGUOUS }
    }
}

/// zerus-ucck P3a: a label a command names without a selection, resolved
/// among its candidates. A label whose only holder is in the ID layout selects
/// that session, so every read of the label in this command reaches it. A
/// label several sessions share is decided by the command's `guard` (P3c):
/// its expected run, and conversation when given, select the one session that
/// carries them; with none, or with several left, it is the ambiguity report,
/// its text and its `--json` value, with exit status 5. A candidate set that
/// cannot be proven complete is refused the same way, never settled on the
/// label's first holder.
/// `hgs send <session> --observe --json` for the CLI's own first
/// disposition (`cli::observe_gate`, zerus-5wmh.66.4.10.1).
pub(crate) fn observe_send(name: &str) -> Result<i32> {
    input::observe_dispatch(name)
}

/// [`resolve_label`] for the send's effect-free observation: the same
/// candidates and the same deferral to the request's identities as a send
/// (`LabelGuard::Request`), without the index repair, which may write.
pub(super) fn resolve_label_unrepaired(name: &str) -> Result<()> {
    let candidates = labels::candidates(name)?;
    match (candidates.first, candidates.by_id.as_slice()) {
        (_, []) => {}
        (false, [only]) => session_id::select(name, string(only, "session_id")),
        _ => {
            let report = ambiguity_report(name, &candidates);
            session_id::defer(name, report.0, report.1);
        }
    }
    Ok(())
}

/// Same ambiguity/Request-guard disposition, with no pre-keeper tmux, Root,
/// cache or external telemetry. A report cannot assert native liveness here.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn resolve_effort_label(
    name: &str,
    guard: LabelGuard,
) -> std::result::Result<(), Unresolved> {
    let fail = |text| Unresolved {
        text,
        value: None,
        code: session_id::AMBIGUOUS,
    };
    let candidates = labels::candidates_metadata(name).map_err(fail)?;
    match (candidates.first, candidates.by_id.as_slice()) {
        (_, []) => Ok(()),
        (false, [only]) => {
            effort_select_metadata(only, "", "").map_err(|text| Unresolved {
                text,
                value: None,
                code: 1,
            })?;
            Ok(())
        }
        _ => {
            let mut records = candidates.by_id;
            if candidates.first {
                records.insert(0, effort_first_metadata(name).map_err(fail)?);
            }
            let rows=records.iter().map(|record|json!({"session_id":record["session_id"],"run_id":record["run_id"],
                "conversation_id":record["conversation_id"],"state":"unknown","cwd":record["cwd"],"created":record["created"]})).collect();
            let report = session_id::ambiguity(name, rows);
            match guard {
                LabelGuard::None => Err(Unresolved::ambiguous(report)),
                LabelGuard::Request => {
                    session_id::defer(name, report.0, report.1);
                    Ok(())
                }
                LabelGuard::Given { run, conversation } => {
                    if run.is_empty() && conversation.is_empty() {
                        return Err(Unresolved::ambiguous(report));
                    }
                    let chosen: Vec<_> = records
                        .iter()
                        .filter(|record| {
                            (run.is_empty() || string(record, "run_id") == run)
                                && (conversation.is_empty()
                                    || string(record, "conversation_id") == conversation)
                        })
                        .collect();
                    match chosen.as_slice() {
                        [] => {
                            if session_id::selected().is_none() {
                                session_id::select_guarded_route(
                                    name,
                                    "",
                                    &run,
                                    &conversation,
                                    false,
                                );
                            }
                            Err(Unresolved {
                                text: stale(name),
                                value: None,
                                code: 1,
                            })
                        }
                        [only] => {
                            effort_select_metadata(only, &run, &conversation).map_err(|text| {
                                Unresolved {
                                    text,
                                    value: None,
                                    code: 1,
                                }
                            })?;
                            Ok(())
                        }
                        _ => Err(Unresolved::ambiguous(report)),
                    }
                }
            }
        }
    }
}

pub(crate) fn resolve_label(name: &str, guard: LabelGuard) -> std::result::Result<(), Unresolved> {
    labels::repair();
    let candidates = labels::candidates(name)
        .map_err(|text| Unresolved { text, value: None, code: session_id::AMBIGUOUS })?;
    match (candidates.first, candidates.by_id.as_slice()) {
        (_, []) => Ok(()),
        (false, [only]) => {
            session_id::select(name, string(only, "session_id"));
            Ok(())
        }
        _ => {
            let report = ambiguity_report(name, &candidates);
            match guard {
                LabelGuard::None => Err(Unresolved::ambiguous(report)),
                LabelGuard::Given { run, conversation } => {
                    match select_among(name, &run, &conversation, report).map_err(Unresolved::ambiguous)? {
                        true => Ok(()),
                        // None carries them: the stale refusal (exit 1), as
                        // for one session that changed, never a resolution.
                        false => Err(Unresolved { text: stale(name), value: None, code: 1 }),
                    }
                }
                LabelGuard::Request => {
                    session_id::defer(name, report.0, report.1);
                    Ok(())
                }
            }
        }
    }
}

/// zerus-ucck P3c: for a command that is not resolved at dispatch, such as
/// `hgs hooks NAME` or a scoped recovery action, the identities its request
/// expects select among the sessions that share `name`, as at dispatch.
pub(crate) fn select_by(name: &str, run: &str, conversation: &str) -> Result<()> {
    resolve_label(name, LabelGuard::Given { run: run.into(), conversation: conversation.into() }).map_err(
        |refusal| match refusal.code {
            session_id::AMBIGUOUS => session_id::refuse((refusal.text, refusal.value.unwrap_or(Value::Null))),
            _ => refusal.text,
        },
    )
}

/// zerus-ucck P3c: the identities a command's request expects, `run` and
/// `conversation`, select among the sessions that share `name` when its
/// ambiguity was deferred to them. Exactly one carrying them is selected
/// with them. None carrying them is the stale refusal, as for one session
/// that changed, and every later read of `name` refuses too (#1454 review).
/// Several, or no identity at all, is the ambiguity report (exit status 5).
/// Without a deferred ambiguity this does nothing.
pub(crate) fn expect(name: &str, run: &str, conversation: &str) -> Result<()> {
    let Some(report) = session_id::take_deferred(name) else {
        return Ok(());
    };
    match select_among(name, run, conversation, report).map_err(session_id::refuse)? {
        true => Ok(()),
        false => Err(stale(name)),
    }
}

/// The refusal when no session sharing `name` carries the identities a
/// command expects.
fn stale(name: &str) -> String {
    session_id::selected()
        .filter(|selection| selection.name == name)
        .map_or_else(|| format!("{name} changed; refresh and retry"), |selection| selection.changed())
}

/// Selects among the sessions sharing `name` the one carrying the identities
/// a command expects, keeping them with it (`session_id::select_guarded`):
/// `true` when exactly one does. `false` when none does: the guarded selection
/// then has no session ID, so every record under `name` is refused as changed
/// (#1454 review). Several, or no identities at all, is the report.
fn select_among(
    name: &str,
    run: &str,
    conversation: &str,
    report: (String, Value),
) -> std::result::Result<bool, (String, Value)> {
    if run.is_empty() && conversation.is_empty() {
        return Err(report);
    }
    let candidates = labels::candidates(name).map_err(|error| (error, report.1.clone()))?;
    let mut records = Vec::new();
    if candidates.first {
        records.push(labels::read_first(name).map_err(|error| (error, report.1.clone()))?);
    }
    records.extend(candidates.by_id);
    let chosen: Vec<&Value> = records
        .iter()
        .filter(|record| {
            (run.is_empty() || string(record, "run_id") == run)
                && (conversation.is_empty() || string(record, "conversation_id") == conversation)
        })
        .collect();
    match chosen.as_slice() {
        [] => {
            session_id::select_guarded(name, "", run, conversation);
            Ok(false)
        }
        [one] => {
            let id = Some(string(one, "session_id")).filter(|id| session_id::canonical(id)).unwrap_or_default();
            session_id::select_guarded(name, id, run, conversation);
            Ok(true)
        }
        _ => Err(report),
    }
}

/// The ambiguity report of a label several sessions share: its text and its
/// `--json` value.
fn ambiguity_report(name: &str, candidates: &labels::Candidates) -> (String, Value) {
    let snapshot = live().unwrap_or_default();
    let state = |record: &Value| {
        if matches(record, panes_of(&snapshot, record)) { "live" } else { "saved" }
    };
    let mut listed: Vec<Value> = Vec::new();
    if candidates.first {
        if let Ok(record) = labels::read_first(name) {
            listed.push(json!({"session_id":record["session_id"],"run_id":record["run_id"],
                "conversation_id":record["conversation_id"],"state":state(&record),
                "cwd":record["cwd"],"created":record["created"]}));
        }
    }
    for record in &candidates.by_id {
        listed.push(json!({"session_id":record["session_id"],"run_id":record["run_id"],
            "conversation_id":record["conversation_id"],"state":"saved",
            "cwd":record["cwd"],"created":record["created"]}));
    }
    session_id::ambiguity(name, listed)
}

/// The durable session ID of this computer's live or saved session `name`,
/// from its record or provider binding (zerus-ucck P2c); None for an archive,
/// an ID-less record or no session.
// The swarm's tests resolve their own sessions, never the real state.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn session_id_of(name: &str) -> Option<String> {
    let record = record_path(name)
        .exists()
        .then(|| read(name).ok())
        .flatten()
        .or_else(|| dsh::binding(name).ok())
        .or_else(|| opencode::binding(name).ok())?;
    let id = string(&record, "session_id");
    (session_id::canonical(id) && string(&record, "archive_id").is_empty()).then(|| id.to_owned())
}

/// The record fields task routing reads (`tasks::sessions`).
const TASK_BINDING_FIELDS: [&str; 7] =
    ["name", "run_id", "launch_id", "journal_name", "launch_label", "legacy_shadow", "rename_shadow"];

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
    for name in storage::BINDING_DIRS {
        let directory = root.join(name);
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let hashed = stem.len() == 64 && stem.bytes().all(|c| c.is_ascii_hexdigit());
            let named = match name { "dsh/bindings" | "opencode/bindings" => hashed, _ => storage::binding_stem(name, stem) };
            if path.extension().is_none_or(|s| s != "json") || !named { continue; }
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

/// Task #34: what other modules' tests need of `builds`' test seams.
#[cfg(test)]
pub(crate) mod builds_test_support {
    pub(crate) fn write_qa_binding() {
        super::builds::tests::write_qa_binding();
    }

    /// Task 108: a live `hgs run` claim with this command, for busy checks.
    pub(crate) fn running_claim(id: &str, cwd: &std::path::Path, command: &str) -> std::path::PathBuf {
        let path = super::builds::test_running_claim(id, cwd, "hgs_run");
        let mut claim: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        claim["command"] = serde_json::json!(command);
        std::fs::write(&path, claim.to_string()).unwrap();
        path
    }
    pub(crate) fn set_dsh_session(conversation: Option<&str>) {
        super::builds::TEST_DSH_SESSION.with(|value| *value.borrow_mut() = conversation.map(str::to_owned));
    }
}

/// Task #34: a DeepSeek tool command's own session: the binding of the
/// conversation its harness exports as DSH_SESSION_ID.
pub(crate) fn dsh_caller_record() -> Option<Value> {
    builds::dsh_caller()
}

/// Compatibility entry point used by `hgs __state` and the legacy hook shim.
/// Task #28 (2): a sandboxed member's `hgs ls` or `hgs inspect`, answered by
/// its team worker; None for every other command and process.
pub(crate) fn sandbox_read(args: &[String]) -> Option<Result<i32>> {
    builds::sandbox_read(args)
}

pub fn dispatch(args: &[String]) -> Result<i32> {
    // Fixed self-image owner, before any provider lookup or local writer. Its
    // inherited endpoint/frame are synchronization and cancellation only; the
    // fixed Effort/Settings operation preserves its original native authority.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if args.first().map(String::as_str)==Some("native-picker-custodian-v1") {
        if args.len()!=1 {return Err("Unexpected picker custodian arguments".into());}
        return effort::picker_custodian_entry();
    }
    if args.first().map(String::as_str)==Some("receipt-index-worker") {
        if args.len()!=1 {return Err("Unexpected receipt worker arguments".into());}
        return receipt_index::worker();
    }
    if args.first().map(String::as_str)==Some("receipt-index-page") {
        return attachments::page_dispatch(&args[1..]);
    }
    if args.first().map(String::as_str)==Some("rules") { return iron_rules::dispatch(&args[1..]); }
    // Receipt observation needs neither a native host nor a state writer.
    if args.first().map(String::as_str)==Some("session-action-status") { return session_action::status_dispatch(&args[1..]); }
    if args.first().map(String::as_str)==Some("delivery") { return delivery::dispatch(&args[1..]); }
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
    // Only this explicit flag ends a parent's live child sessions after it
    // (zerus-sa9qn.17). Before provider routing, so a DeepSeek or OpenCode
    // parent refuses with its own message and no child is touched.
    if matches!(command.as_str(), "terminate" | "archive") && args.iter().any(|arg| arg == end_children::FLAG) {
        return end_children::dispatch(command, args);
    }
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
    if command == "keep-warm" { return keep_warm::dispatch(args); }
    if command == "question-routing" { return question_routing::dispatch(args); }
    if command == "history" { return conversation_history::dispatch(args); }
    if command == "history-projection" { return conversation_history::projection_dispatch(args); }
    if command == "terminal" { return terminal::dispatch(args); }
    if command == "session-action" { return session_action::dispatch(args); }
    if command == "compact-context" { return compact_context::dispatch(args); }
    if command == "clear-context" { return clear_context::dispatch(args); }
    if command == "interrupt" { return interrupt::dispatch(args); }
    if command == "processes" { return processes::dispatch(args); }
    if command == "parent" { return parent_link::dispatch(args); }
    // The send's effect-free observation is taken only by the CLI's own
    // first disposition (`cli::observe_gate`), never here: a send carrying
    // --observe by another route (the private `__state` ABI, a caller of
    // this dispatch) refuses before provider routing, whose own sends would
    // ignore the flag (zerus-5wmh.66.4.10.1).
    if command == "send" && args.iter().any(|arg| arg == "--observe") {
        return Err(
            "hgs send --observe is taken only as `hgs send <codex session> --observe --json`"
                .into(),
        );
    }
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
                .ok_or("usage: hgs inspect <session> [--archive ID] [--after cursor] [--skip-processes] [--read-observation] [--current-read]")?;
            let mut after = 0;
            let mut archive_id = None;
            let mut agent_id = None;
            let mut include_processes = true;
            let mut read_observation = false;
            let mut current_read = false;
            let mut index = 1;
            while index < args.len() {
                if args[index] == "--skip-processes" { include_processes = false; index += 1; continue; }
                if args[index] == "--read-observation" { read_observation = true; index += 1; continue; }
                if args[index] == "--current-read" { current_read = true; index += 1; continue; }
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
                journal::inspection_with(name, after, archive_id, include_processes, current_read)?
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
            let forgotten = record_path(&args[0]).exists().then(|| read(&args[0])).transpose()?;
            if let Some(record) = &forgotten {
                // Its own record, never its label's holder (P3a).
                storage::remove_record(record)?;
            }
        }
        _ => return Err(format!("unknown state operation: {command}")),
    }
    Ok(0)
}

#[cfg(all(test, unix))]
mod hosted_ancestry_tests;
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
        // A send carrying --observe refuses at admission here, before any
        // provider routing (zerus-be8zv).
        for refusal in observe_refusals() {
            assert_eq!(refusal, UNSUPPORTED);
        }
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
        // Admitted, a send carrying --observe meets dispatch's own refusal,
        // still before provider routing (zerus-5wmh.66.4.10.1, zerus-be8zv).
        for refusal in observe_refusals() {
            assert!(refusal.contains("is taken only as"), "{refusal}");
        }
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
        for refusal in observe_refusals() {
            assert!(refusal.contains("is taken only as"), "{refusal}");
        }
    }

    /// `dispatch`'s refusal of each send carrying --observe that its unit
    /// test covers on Unix (`input::OBSERVE_SENDS`, zerus-be8zv).
    fn observe_refusals() -> Vec<String> {
        input::OBSERVE_SENDS
            .into_iter()
            .map(|send| dispatch(&send.map(str::to_owned)).unwrap_err())
            .collect()
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
