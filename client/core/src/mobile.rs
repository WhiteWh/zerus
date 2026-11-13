//! Settings > Mobile connection reads and explicit phone actions: the
//! workspace and its connector as `zerus-mobile-connector --workspace status`
//! reports them (WorkspaceSettings.h), the phones each computer's mobile
//! connection lists (`hgs [@host] mobile phones --json`, PhonePairing.cpp)
//! and the shared phone notification timing (`hgs swarm preference`,
//! MobileDeliverySettings.h). Every call is bounded in time and size and runs
//! off the main thread; [`page`] lets one of each kind run at a time.
//!
//! The status read writes nothing and opens no connection. The phones read
//! is Qt's: on an encrypted workspace that computer's connector lists its
//! devices, holding its private workspace state while it asks the relay,
//! and may refresh its saved activation. No call here starts or stops the
//! connector, changes its service or the workspace, pairs or revokes a
//! phone; [`phone_actions`] supplies those separate guarded calls.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::actions::{Exit, Invocation};

pub mod autostart;
pub mod computer_codes;
pub mod connector_actions;
pub mod enrollment;
pub mod legacy;
pub mod local_services;
pub mod page;
pub mod phone_actions;

/// The help line that shows `hgs mobile` exists; an older `hgs` would read
/// "mobile" as an agent to launch.
pub const PHONES_MARKER: &str = "mobile phones|invite|revoke-phone";
pub const STOP_MARKER: &str = "Workspace stop ABI: service-preflight-v1";
pub const STOP_UNAVAILABLE: &str = "Zerus can't stop the connector on this computer. You can change the workspace while the connector is stopped.";
/// The connector's closed result for a Stop its preflight refused before offering
/// any marker. Only this, bound to the call, ever means that nothing changed.
pub const STOP_REFUSED_BEFORE_EFFECT: &str = "stop-refused-before-effect";
/// What the person reads for that refusal: nothing happened, and why.
pub const STOP_REFUSED: &str =
    "Zerus can't stop this connector because it can't confirm how it was started. Nothing changed.";

/// How a lifecycle call that may send Stop ended without an answer. Everything
/// but the closed refusal is an unknown outcome, whatever its text says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    StopRefused,
    Other(String),
}
impl Failure {
    pub fn message(&self) -> String {
        match self {
            Failure::StopRefused => STOP_REFUSED.into(),
            Failure::Other(error) => error.clone(),
        }
    }
}
impl From<String> for Failure {
    fn from(error: String) -> Self {
        Failure::Other(error)
    }
}
impl From<&str> for Failure {
    fn from(error: &str) -> Self {
        Failure::Other(error.into())
    }
}
pub const WORKSPACE_UNCONFIRMED: &str = "Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.";
/// The connector's status reply, at most.
const STATUS_BYTES: usize = 64 * 1024;
/// Any other reply, at most.
const REPLY_BYTES: usize = 1024 * 1024;
const HELP_BYTES: usize = 256 * 1024;

/// What the connector's child never inherits: the agent's attribution and
/// the terminal it may run in.
pub const CONNECTOR_UNSET: [&str; 9] = [
    "HGS_SESSION",
    "HGS_SESSION_ID",
    "HGS_RUN_ID",
    "HGS_EXPECTED_ID",
    "HGS_AGENT",
    "HGS_HOST_PANE",
    "HGS_HOST_SESSION",
    "TMUX",
    "TMUX_PANE",
];

/// RelaySettings::configPath: the connector's configuration.
pub fn config_path() -> PathBuf {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    zerus_watch_protocol::session_labels::configuration_path(
        if cfg!(windows) {
            absolute("HOME").or_else(home_dir)
        } else {
            home_dir()
        },
        absolute("XDG_CONFIG_HOME"),
        absolute("HGS_CONFIG_DIR"),
        cfg!(windows),
    )
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Use the packaged Windows connector before a separately staged/older CLI's
/// sibling. Other platforms retain the CLI, client, PATH search order.
pub fn connector_program(hgs: &Path, client_dir: Option<&Path>) -> PathBuf {
    let name = if cfg!(windows) {
        "zerus-mobile-connector.exe"
    } else {
        "zerus-mobile-connector"
    };
    let bundled = client_dir.map(|dir| dir.join(name));
    #[cfg(windows)]
    if let Some(path) = bundled.as_ref().filter(|path| executable(path)) {
        return path.clone();
    }
    if hgs.is_absolute() {
        if let Some(beside) = hgs
            .parent()
            .map(|dir| dir.join(name))
            .filter(|path| executable(path))
        {
            return beside;
        }
    }
    if let Some(bundled) = bundled.as_ref().filter(|path| executable(path)) {
        return bundled.clone();
    }
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| executable(path))
    });
    found.or(bundled).unwrap_or_else(|| PathBuf::from(name))
}

/// The connector packaged beside this app, with no staged-CLI or PATH fallback:
/// what automatic start may run. Missing or unusable means unavailable.
pub fn packaged_connector(client_dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "zerus-mobile-connector.exe"
    } else {
        "zerus-mobile-connector"
    };
    let path = client_dir.join(name);
    executable(&path).then_some(path)
}

/// Exact selection for a local nested CLI: PATH alone cannot override an old
/// connector beside the staged hgs, and a CLI without a sibling must not reach
/// another connector on PATH. Never send this local path to a peer.
const CONNECTOR_ENV: &str = "ZERUS_CLIENT_MOBILE_CONNECTOR_PATH";

/// The connector the status probe uses ([`connector_program`] with this app's
/// folder), for a local call only, and only as an absolute path.
fn phone_connector(hgs: &Path, call: &Invocation, directory: Option<&Path>) -> Option<String> {
    if call.args.first().is_some_and(|arg| arg.starts_with('@')) {
        return None;
    }
    let selected = connector_program(hgs, directory);
    selected
        .is_absolute()
        .then(|| selected.to_str().map(str::to_owned))
        .flatten()
}

/// A phone call through the CLI with the connector the status probe selected,
/// on every platform (zerus-ihrzs): the probe and the nested launch name the
/// same program. Without an absolute selection the CLI keeps its own order.
fn run_phone_cli(hgs: &Path, call: &Invocation, cap: usize) -> Result<Exit, crate::run::TooLarge> {
    let directory = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    run_phone_cli_from(hgs, call, cap, directory.as_deref())
}

fn run_phone_cli_from(
    hgs: &Path,
    call: &Invocation,
    cap: usize,
    client_dir: Option<&Path>,
) -> Result<Exit, crate::run::TooLarge> {
    let connector = phone_connector(hgs, call, client_dir);
    run_phone_cli_in(hgs, call, cap, connector.as_deref())
}

fn run_phone_cli_in(
    hgs: &Path,
    call: &Invocation,
    cap: usize,
    connector: Option<&str>,
) -> Result<Exit, crate::run::TooLarge> {
    let environment = connector.map(|path| [(CONNECTOR_ENV, path)]);
    // Also remove an inherited selection on remote reads/actions. The remote
    // computer retains its own existing connector discovery.
    crate::run::run_without(
        hgs,
        call,
        environment.as_ref().map_or(&[], |v| v.as_slice()),
        if connector.is_some() {
            &[]
        } else {
            &[CONNECTOR_ENV]
        },
        cap,
    )
}

/// `zerus-mobile-connector --workspace status --config PATH`, `{}` on stdin
/// (70 s, as Qt allows).
pub fn status_invocation(config: &Path) -> Invocation {
    Invocation {
        args: vec![
            "--workspace".into(),
            "status".into(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ],
        stdin: Some(b"{}".to_vec()),
        timeout: Duration::from_secs(70),
        request_id: None,
    }
}

fn hex(value: &Value, size: usize) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == size
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// RelaySettings::validUrl, as strict: HTTPS with a host, no credentials,
/// query, fragment, spaces or control characters.
pub fn valid_relay(text: &str) -> bool {
    if text.is_empty()
        || text.chars().count() > 2048
        || text.chars().any(|c| (c as u32) < 33 || c as u32 == 127)
    {
        return false;
    }
    let Some(rest) = text.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && !authority.starts_with(':')
        && !text.contains('?')
        && !text.contains('#')
}

fn text<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn lifecycle(state: &Value, running: &Value, generation: &Value) -> bool {
    let (Some(state), Some(running)) = (state.as_str(), running.as_bool()) else {
        return false;
    };
    match state {
        "stopped" => !running && generation.is_null(),
        "running" | "stopping" => running && hex(generation, 32),
        _ => false,
    }
}

fn migration_shape(migration: &Value, state: &str) -> bool {
    let reason = migration["reason"].as_str().unwrap_or("");
    let Some(retryable) = migration["retryable"].as_bool() else {
        return false;
    };
    if state == "migration-draining" {
        return migration["phase"] == "draining"
            && retryable
            && matches!(reason, "draining" | "drain-timeout");
    }
    state == "migration-blocked"
        && migration["phase"] == "blocked"
        && !retryable
        && matches!(
            reason,
            "unsafe-unit"
                | "unsupported-runtime"
                | "unsupported-platform"
                | "identity-changed"
                | "service-definition-conflict"
                | "service-unconfirmed"
                | "journal-unavailable"
        )
}

/// WorkspaceSettings::statusShape.
fn status_shape(value: &Map<String, Value>) -> bool {
    let get = |key: &str| value.get(key).unwrap_or(&Value::Null);
    let Some(encrypted) = get("encrypted").as_bool() else {
        return false;
    };
    let Some(revision) = get("config_revision").as_str() else {
        return false;
    };
    if !revision.is_empty() && !hex(get("config_revision"), 64) {
        return false;
    }
    let state = text(value, "state");
    if state == "unconfigured" || state == "legacy" {
        if encrypted {
            return false;
        }
        let pending = get("pending")["state"].as_str().unwrap_or("");
        if pending.starts_with("migration-") {
            return get("migration").is_object() && migration_shape(get("migration"), pending);
        }
        return !value.contains_key("migration");
    }
    if state == "pending-activation" {
        return encrypted;
    }
    if (state != "active" && state != "retired")
        || !encrypted
        || !valid_relay(text(value, "server_url"))
        || text(value, "node_id").is_empty()
        || text(value, "workspace_id").is_empty()
    {
        return false;
    }
    lifecycle(get("connector_state"), get("running"), get("generation"))
}

/// The workspace as the page shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Workspace {
    /// The connector's state: active, retired, legacy, pending-activation
    /// or unconfigured.
    pub state: String,
    pub encrypted: bool,
    pub summary: String,
    pub connection: String,
    /// A saved connection waiting for its result.
    pub pending: Option<String>,
    pub running: bool,
    pub connector_state: String,
    /// Fresh help capability; never inferred from workspace encryption.
    pub stop_supported: bool,
    /// The connection's revision; a change refreshes the phones.
    pub config_revision: String,
    /// Public relay identity; invitation results must match this snapshot.
    pub server_url: String,
    pub node_id: String,
    pub generation: Option<String>,
    pub pending_state: Option<String>,
    pub pending_node_id: Option<String>,
    pub pending_action: Option<String>,
    pub migration_reason: Option<String>,
    pub controls: Controls,
}

/// WorkspaceSettings::controls: which of the workspace's actions the page
/// shows in this state. Writable controls also require a fresh effect admission.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Controls {
    pub change: bool,
    /// Create or join a workspace.
    pub enrollment: bool,
    pub start: bool,
    pub stop: bool,
    /// The pending connection's action: Check connection, Continue
    /// connecting or Disconnect legacy locally and switch….
    pub check: Option<String>,
    /// Give, copy and cancel a computer code.
    pub invitations: bool,
}

fn controls(state: &str, running: bool, pending: Option<&Value>) -> Controls {
    let active = state == "active";
    let pending_state = pending.and_then(|pending| pending["state"].as_str());
    Controls {
        change: active || state == "legacy" || state == "retired",
        enrollment: !active && state != "retired" && state != "legacy",
        start: active,
        stop: active || (state == "retired" && running),
        check: pending.map(|_| {
            match pending_state {
                Some("legacy-retirement-unconfirmed") => "Disconnect legacy locally and switch…",
                Some(state) if state.starts_with("migration-") => "Continue connecting",
                _ => "Check connection",
            }
            .to_owned()
        }),
        invitations: active,
    }
}

/// WorkspaceSettings::render, the summary in the client's words where Qt
/// separates values with a dot.
pub fn workspace(value: &Map<String, Value>) -> Workspace {
    let state = text(value, "state");
    let running = value
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (summary, connection) = match state {
        "active" => {
            let named = |key: &str, fallback: &str| match text(value, key) {
                "" => fallback.to_owned(),
                name => name.to_owned(),
            };
            let connector = match text(value, "connector_state") {
                "running" => "Connector: running",
                "stopping" => "Connector: draining its current request",
                _ => "Connector: stopped",
            };
            (
                format!(
                    "{} on {}\nPrivate session content is encrypted.",
                    named("workspace_name", "Workspace"),
                    named("computer_name", "Computer")
                ),
                connector.to_owned(),
            )
        }
        "retired" => (
            "This computer was removed from its workspace.".to_owned(),
            if running {
                "The retired connector is draining. You can stop its current generation before changing workspace."
            } else {
                "Choose Change workspace to create a workspace or join with a new invitation. Saved local content stays on this computer."
            }
            .to_owned(),
        ),
        "legacy" => (
            "Legacy connection: session content is visible to this relay.".to_owned(),
            "Choose Change workspace to create or join an encrypted workspace.".to_owned(),
        ),
        "pending-activation" => (
            "Workspace activation is incomplete.".to_owned(),
            "Check the saved connection before continuing.".to_owned(),
        ),
        _ => (
            "No workspace connected".to_owned(),
            "Create a workspace or use a computer invitation from an enrolled computer.".to_owned(),
        ),
    };
    let pending_value = value.get("pending").filter(|pending| pending.is_object());
    let pending = pending_value.map(|pending| {
        if pending["state"] == "legacy-retirement-unconfirmed" {
            "The new encrypted workspace is ready. The old relay has not confirmed retirement. You can explicitly disconnect its local configuration before switching."
        } else {
            "A workspace connection is saved and waiting. Check its result before starting another connection."
        }
        .to_owned()
    });
    Workspace {
        state: state.to_owned(),
        encrypted: value
            .get("encrypted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary,
        connection,
        pending,
        running,
        connector_state: text(value, "connector_state").to_owned(),
        stop_supported: false,
        config_revision: text(value, "config_revision").to_owned(),
        server_url: text(value, "server_url").to_owned(),
        node_id: text(value, "node_id").to_owned(),
        generation: value
            .get("generation")
            .and_then(Value::as_str)
            .map(str::to_owned),
        pending_state: pending_value
            .and_then(|v| v["state"].as_str())
            .map(str::to_owned),
        pending_node_id: pending_value
            .and_then(|v| v["node_id"].as_str())
            .map(str::to_owned),
        pending_action: pending_value
            .and_then(|v| v["action"].as_str())
            .map(str::to_owned),
        migration_reason: value
            .get("migration")
            .and_then(|v| v["reason"].as_str())
            .map(str::to_owned),
        controls: controls(state, running, pending_value),
    }
}

fn workspace_failure(reason: &str) -> String {
    format!("Workspace operation was not confirmed: {reason}. Refresh status before continuing.")
}

fn connector_available(connector: &Path) -> Result<(), String> {
    // Bare names still use PATH. An absolute bundled selection can name a
    // missing file; distinguish that from a loader failure without echoing paths.
    if connector.is_absolute()
        && matches!(std::fs::metadata(connector), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        return Err(workspace_failure(
            "the computer connector is missing; reinstall the matching Zerus package",
        ));
    }
    Ok(())
}

fn workspace_reply(exit: Option<Exit>, show_refusal: bool) -> Result<Map<String, Value>, String> {
    let (code, stdout) = match exit {
        Some(Exit::Code { code, stdout, .. }) => (code, stdout),
        Some(Exit::StartFailed(_)) => return Err(workspace_failure(
            "the computer connector could not start; verify its executable and runtime dependencies",
        )),
        Some(Exit::TimedOut) => return Err(workspace_failure("the computer connector timed out")),
        Some(Exit::WriteFailed) => return Err(workspace_failure("the workspace request input could not be delivered")),
        Some(Exit::Abnormal { .. }) => return Err(workspace_failure("the computer connector ended abnormally")),
        None => return Err(workspace_failure("the computer connector reply exceeded its limit")),
    };
    if stdout.len() > STATUS_BYTES {
        return Err(workspace_failure(
            "the computer connector reply exceeded its limit",
        ));
    }
    let reply = match serde_json::from_slice::<Value>(&stdout) {
        Ok(Value::Object(reply)) => reply,
        _ => {
            return Err(if show_refusal {
                workspace_failure(&format!(
                "the computer connector returned an invalid workspace reply (exit {code}, 0x{:08x}); verify the matching connector version",
                code as u32,
            ))
            } else {
                workspace_failure("the computer connector returned an invalid workspace reply; verify the matching connector version")
            })
        }
    };
    let ok = reply.get("ok").is_none_or(|ok| ok == &Value::Bool(true));
    if code == 0 && ok {
        return Ok(reply);
    }
    let error = text(&reply, "error");
    Err(
        if !show_refusal
            || error.is_empty()
            || error.chars().count() > 1024
            || error.chars().any(char::is_control)
        {
            if show_refusal {
                workspace_failure(&format!(
                    "the computer connector refused the workspace request (exit {code})"
                ))
            } else {
                workspace_failure("the computer connector refused the workspace request")
            }
        } else {
            error.into()
        },
    )
}

/// Validate the workspace protocol separately from startup/exit and connector
/// refusals. No child stderr or private input becomes a diagnostic.
pub fn status(exit: Option<Exit>) -> Result<Workspace, String> {
    let reply = workspace_reply(exit, true)?;
    if !status_shape(&reply) {
        return Err(workspace_failure(
            "the computer connector workspace status is incompatible; update the matching Zerus package",
        ));
    }
    Ok(workspace(&reply))
}

/// Reads the workspace's status. Blocks; never on the main thread.
pub fn read_status(connector: &Path, config: &Path) -> Result<Workspace, String> {
    read_status_in(connector, config, &[])
}

/// [`read_status`] from a parent environment holding `parent_env`: the
/// terminal's variables and the agent's attribution never reach the
/// connector, whatever the parent has.
pub fn read_status_in(
    connector: &Path,
    config: &Path,
    parent_env: &[(&str, &str)],
) -> Result<Workspace, String> {
    connector_available(connector)?;
    let mut workspace = status(
        crate::run::run_without(
            connector,
            &status_invocation(config),
            parent_env,
            &CONNECTOR_UNSET,
            STATUS_BYTES,
        )
        .ok(),
    )?;
    // Bounded read on the existing workspace read/action lane, no new poller.
    let probe = Invocation {
        args: vec!["--help".into()],
        stdin: None,
        timeout: Duration::from_secs(5),
        request_id: None,
    };
    workspace.stop_supported = matches!(crate::run::run_without(connector, &probe, parent_env, &CONNECTOR_UNSET, 16_384),
        Ok(Exit::Code { code: 0, stdout, .. }) if String::from_utf8_lossy(&stdout).lines().any(|line| line == STOP_MARKER));
    Ok(workspace)
}

/// Existing workspace protocol, on private stdin, with no raw error exposure.
/// Callers admit a typed action and freshly bind the exact configuration first.
fn workspace_request(
    connector: &Path,
    config: &Path,
    action: &str,
    payload: &Value,
) -> Result<Value, String> {
    let exit = workspace_call(connector, config, action, payload)?;
    workspace_reply(exit, false).map(Value::Object)
}

fn workspace_call(
    connector: &Path,
    config: &Path,
    action: &str,
    payload: &Value,
) -> Result<Option<Exit>, String> {
    connector_available(connector)?;
    let call = Invocation {
        args: vec![
            "--workspace".into(),
            action.into(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ],
        stdin: Some(serde_json::to_vec(payload).map_err(|_| WORKSPACE_UNCONFIRMED)?),
        timeout: Duration::from_secs(70),
        request_id: None,
    };
    Ok(crate::run::run_without(connector, &call, &[], &CONNECTOR_UNSET, STATUS_BYTES).ok())
}

/// The one Stop call of the connector's workspace protocol, for this exact
/// configuration and generation.
fn workspace_stop(
    connector: &Path,
    config: &Path,
    revision: &str,
    generation: &str,
) -> Result<Value, Failure> {
    let payload = serde_json::json!({"expected_revision":revision,"generation":generation});
    let exit = workspace_call(connector, config, "stop", &payload)?;
    if stop_refused(&exit, revision, generation) {
        return Err(Failure::StopRefused);
    }
    workspace_reply(exit, false)
        .map(Value::Object)
        .map_err(Failure::from)
}

/// Only the connector's closed result for this very call: not its error text, a
/// timeout, a malformed reply, or a refusal naming another generation.
fn stop_refused(exit: &Option<Exit>, revision: &str, generation: &str) -> bool {
    let Some(Exit::Code {
        code: 1, stdout, ..
    }) = exit
    else {
        return false;
    };
    let Ok(Value::Object(reply)) = serde_json::from_slice::<Value>(stdout) else {
        return false;
    };
    reply.get("ok") == Some(&Value::Bool(false))
        && reply.get("refusal").and_then(Value::as_str) == Some(STOP_REFUSED_BEFORE_EFFECT)
        && reply.get("generation").and_then(Value::as_str) == Some(generation)
        && reply.get("expected_revision").and_then(Value::as_str) == Some(revision)
}

/// HgsClient's failureDetail for an `hgs` call that did not finish cleanly;
/// None for a clean exit. No exit means output past `limit`.
fn failure(hgs: &Path, exit: &Option<Exit>, limit: usize) -> Option<String> {
    match exit {
        Some(Exit::Code { code: 0, .. }) => None,
        Some(Exit::TimedOut) => Some("timed out".into()),
        Some(Exit::StartFailed(error)) => {
            Some(format!("cannot start '{}': {error}", hgs.display()))
        }
        Some(Exit::Code { code, stderr, .. }) => {
            let detail: String = stderr.trim().chars().take(4000).collect();
            Some(if detail.is_empty() {
                format!("exit code {code}")
            } else {
                detail
            })
        }
        Some(Exit::Abnormal { stderr }) if !stderr.trim().is_empty() => {
            Some(stderr.trim().chars().take(4000).collect())
        }
        Some(_) => Some("exit code -1".into()),
        None => Some(format!("reply exceeds {limit} bytes")),
    }
}

/// A clean reply's object; `invalid` names an unreadable one.
fn object(hgs: &Path, exit: Option<Exit>, invalid: &str) -> Result<Map<String, Value>, String> {
    if let Some(error) = failure(hgs, &exit, REPLY_BYTES) {
        return Err(error);
    }
    let Some(Exit::Code { stdout, .. }) = exit else {
        return Err(invalid.into());
    };
    match serde_json::from_slice::<Value>(&stdout) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(invalid.into()),
    }
}

fn invocation(args: &[&str], timeout: Duration) -> Invocation {
    Invocation {
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        stdin: None,
        timeout,
        request_id: None,
    }
}

fn routed(host: &str, args: &[&str]) -> Vec<String> {
    let mut routed = Vec::new();
    if !host.is_empty() {
        routed.push(format!("@{host}"));
    }
    routed.extend(args.iter().map(|arg| (*arg).to_owned()));
    routed
}

/// `hgs machine ls` (8 s): this computer, then every enabled peer alias.
pub fn hosts(hgs: &Path) -> Result<Vec<String>, String> {
    let reply = object(
        hgs,
        crate::run::run_limited(
            hgs,
            &invocation(&["machine", "ls"], Duration::from_secs(8)),
            &[],
            REPLY_BYTES,
        )
        .ok(),
        "Invalid machine settings response",
    )?;
    let mut hosts = vec![String::new()];
    for row in reply
        .get("machines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let alias = row["alias"].as_str().unwrap_or("");
        if row["enabled"].as_bool() == Some(true)
            && !alias.is_empty()
            && !hosts.iter().any(|host| host == alias)
        {
            hosts.push(alias.to_owned());
        }
    }
    Ok(hosts)
}

/// A phone paired through a workspace.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Phone {
    pub id: String,
    pub name: String,
    /// iOS, Android, or "—".
    pub platform: String,
    /// Seconds since the epoch; None when unknown.
    pub paired_at: Option<f64>,
    pub last_seen_at: Option<f64>,
    /// The computer it was added from; None when not recorded.
    pub added_from: Option<String>,
    /// The command that revokes it, pinned as Qt pins it.
    pub revoke: String,
}

/// One workspace's phones, as one computer's mobile connection lists them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PhoneList {
    /// The computer it was read through; empty for this one.
    pub host: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub encrypted: bool,
    pub server_url: String,
    /// The command that pairs a phone, pinned as Qt pins it.
    pub invite: String,
    pub phones: Vec<Phone>,
}

/// One POSIX shell word: as it is when nothing in it needs quoting.
/// `shell::shell_quote`, which has the same plain characters and also keeps
/// the word whole when it is pasted into fish (zerus-sa9qn.43).
fn posix_word(text: &str) -> String {
    crate::shell::shell_quote(text)
}

/// One PowerShell word. `@` starts a splat and `,` an array there, so both
/// are quoted; inside single quotes every quote PowerShell closes on doubles:
/// `'` and U+2018 to U+201B, as `shell::power_shell_quote` (zerus-sa9qn.38).
/// None for a word Windows PowerShell 5.1 would not hand `hgs.exe` whole
/// (`shell::power_shell_passes_unchanged`: a `"`, or a spaced word ending in a
/// backslash, splits it into other arguments) or with U+201C to U+201E, which
/// PowerShell also reads as `"` (zerus-sa9qn.43).
fn powershell_word(text: &str) -> Option<String> {
    if !crate::shell::power_shell_passes_unchanged(text)
        || text.contains(['\u{201c}', '\u{201d}', '\u{201e}'])
    {
        return None;
    }
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/=+%".contains(c));
    if plain {
        return Some(text.to_owned());
    }
    let mut word = String::from("'");
    for c in text.chars() {
        word.push(c);
        if matches!(c, '\'' | '\u{2018}'..='\u{201b}') {
            word.push(c);
        }
    }
    word.push('\'');
    Some(word)
}

/// One word of the terminal the person types the command into: PowerShell
/// on Windows, a POSIX shell elsewhere. None when that shell cannot be
/// given it whole.
fn shell_word(text: &str) -> Option<String> {
    if cfg!(windows) {
        powershell_word(text)
    } else {
        Some(posix_word(text))
    }
}

/// The `hgs` command Qt's panel runs for `action` (PhonePairing.cpp
/// `pinned`): the workspace and relay the person saw are pinned, so a
/// computer whose connection changed meanwhile refuses it. Empty, like the
/// desktop's own rows, when a word cannot be written for the person's shell.
pub fn pinned(host: &str, action: &str, id: &str, workspace: &str, server: &str) -> String {
    let words = || -> Option<Vec<String>> {
        let mut words = vec!["hgs".to_owned()];
        if !host.is_empty() {
            words.push(shell_word(&format!("@{host}"))?);
        }
        words.extend(["mobile".to_owned(), action.to_owned()]);
        if !id.is_empty() {
            words.push(shell_word(id)?);
        }
        if !workspace.is_empty() && !server.is_empty() {
            words.extend([
                "--workspace".to_owned(),
                shell_word(workspace)?,
                "--server".to_owned(),
                shell_word(server)?,
            ]);
        }
        words.push("--json".to_owned());
        Some(words)
    };
    words().map(|words| words.join(" ")).unwrap_or_default()
}

fn seconds(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n > 0.0)
}

fn platform(value: &Value) -> String {
    match value.as_str() {
        Some("ios") => "iOS",
        Some("android") => "Android",
        _ => "—",
    }
    .into()
}

/// `hgs [@host] mobile phones --json` after the help probe; None when that
/// computer has no mobile connection.
pub fn phones(hgs: &Path, host: &str) -> Result<Option<PhoneList>, String> {
    let strip = |error: String| {
        error
            .strip_prefix("hgs: ")
            .map(str::to_owned)
            .unwrap_or(error)
    };
    let probe_time = Duration::from_secs(if host.is_empty() { 5 } else { 15 });
    let help = run_phone_cli(
        hgs,
        &Invocation {
            args: routed(host, &["--help"]),
            ..invocation(&[], probe_time)
        },
        HELP_BYTES,
    )
    .ok();
    if let Some(error) = failure(hgs, &help, HELP_BYTES) {
        return Err(strip(error));
    }
    let marked = matches!(&help, Some(Exit::Code { stdout, .. }) if String::from_utf8_lossy(stdout).contains(PHONES_MARKER));
    if !marked {
        let computer = if host.is_empty() {
            "this computer"
        } else {
            host
        };
        return Err(format!(
            "Update Zerus on {computer} to manage phones from here."
        ));
    }
    let call = Invocation {
        args: routed(host, &["mobile", "phones", "--json"]),
        ..invocation(&[], Duration::from_secs(40))
    };
    let reply = object(
        hgs,
        run_phone_cli(hgs, &call, REPLY_BYTES).ok(),
        "Invalid phone management response",
    )
    .map_err(strip)?;
    if reply.get("configured") != Some(&Value::Bool(true)) {
        return Ok(None);
    }
    let workspace_id = text(&reply, "workspace_id");
    let server_url = text(&reply, "server_url");
    let phones = reply
        .get("devices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|device| Phone {
            id: device["id"].as_str().unwrap_or("").to_owned(),
            name: device["name"].as_str().unwrap_or("").to_owned(),
            platform: platform(&device["platform"]),
            paired_at: seconds(&device["paired_at"]),
            last_seen_at: seconds(&device["last_seen_at"]),
            added_from: device["added_by"]
                .as_object()
                .filter(|added| !added.is_empty())
                .map(|added| {
                    added
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                }),
            revoke: pinned(
                host,
                "revoke-phone",
                device["id"].as_str().unwrap_or(""),
                workspace_id,
                server_url,
            ),
        })
        .collect();
    Ok(Some(PhoneList {
        host: host.to_owned(),
        workspace_id: workspace_id.to_owned(),
        workspace_name: text(&reply, "workspace_name").to_owned(),
        encrypted: reply.get("encrypted") == Some(&Value::Bool(true)),
        server_url: server_url.to_owned(),
        invite: pinned(host, "invite", "", workspace_id, server_url),
        phones,
    }))
}

/// The Phones panel: one list per workspace, this computer first, then the
/// peers in Machines order, and the status lines under it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Phones {
    pub lists: Vec<PhoneList>,
    pub lines: Vec<String>,
}

/// PhonePairing::Panel::finish and render: `read` holds each host's answer
/// in `hosts`' order; `machines` the machine list's failure, if any.
pub fn phones_view(
    read: Vec<(String, Result<Option<PhoneList>, String>)>,
    machines: Option<String>,
    windows: bool,
) -> Phones {
    let mut lists = Vec::new();
    let mut problems = Vec::new();
    if let Some(error) = machines {
        problems.push(format!("Could not read the machine list: {error}"));
    }
    let mut seen = HashSet::new();
    for (host, answer) in read {
        match answer {
            Ok(Some(list)) => {
                if !list.workspace_id.is_empty() && seen.insert(list.workspace_id.clone()) {
                    lists.push(list);
                }
            }
            Ok(None) => {}
            Err(error) => {
                let name = if host.is_empty() {
                    "this computer".to_owned()
                } else {
                    host
                };
                problems.push(format!("{name}: {error}"));
            }
        }
    }
    let mut lines = Vec::new();
    if lists.is_empty() {
        lines.push(if windows {
            "Open Settings → Mobile connection to create a workspace or join one by code, then start the connector and refresh Phones."
        } else {
            "No computer here has a mobile connection. Set one up in Settings → Mobile connection on a computer with local sessions enabled, then refresh Phones."
        }
        .to_owned());
    } else {
        let count: usize = lists.iter().map(|list| list.phones.len()).sum();
        lines.push(if count == 1 {
            "1 phone".to_owned()
        } else {
            format!("{count} phones")
        });
    }
    lines.extend(problems);
    Phones { lists, lines }
}

/// The shared phone notification timing's values, as Qt offers them.
pub const DELIVERY: [&str; 6] = [
    "immediate",
    "away",
    "away_5",
    "away_10",
    "away_15",
    "away_30",
];

/// `hgs swarm preference get --json`, or `set mobile_delivery VALUE --json`
/// for one of [`DELIVERY`] (10 s): the value the catalog holds.
pub fn delivery(hgs: &Path, set: Option<&str>) -> Result<String, String> {
    let args: Vec<&str> = match set {
        Some(value) if DELIVERY.contains(&value) => vec![
            "swarm",
            "preference",
            "set",
            "mobile_delivery",
            value,
            "--json",
        ],
        Some(_) => return Err("unexpected response from hgs".into()),
        None => vec!["swarm", "preference", "get", "--json"],
    };
    let exit = crate::run::run_limited(
        hgs,
        &invocation(&args, Duration::from_secs(10)),
        &[],
        REPLY_BYTES,
    )
    .ok();
    let reply = object(hgs, exit, "Invalid project catalog response").map_err(|error| {
        if error.is_empty() {
            "hgs did not respond".to_owned()
        } else {
            error
        }
    })?;
    let value = text(&reply, "mobile_delivery");
    let saved = set.is_none() || reply.get("ok") == Some(&Value::Bool(true));
    if saved && DELIVERY.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err("unexpected response from hgs".into())
    }
}

#[cfg(test)]
mod tests;
