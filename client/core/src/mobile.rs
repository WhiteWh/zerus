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

pub mod computer_codes;
pub mod connector_actions;
pub mod enrollment;
pub mod legacy;
pub mod page;
pub mod phone_actions;

/// The help line that shows `hgs mobile` exists; an older `hgs` would read
/// "mobile" as an agent to launch.
pub const PHONES_MARKER: &str = "mobile phones|invite|revoke-phone";
pub const STOP_MARKER: &str = "Workspace stop ABI: service-preflight-v1";
pub const STOP_UNAVAILABLE: &str = "Stop is not available with this connector version. You can change the workspace while the connector is stopped.";
pub const WORKSPACE_UNCONFIRMED: &str = "Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.";
/// The connector's status reply, at most.
const STATUS_BYTES: usize = 64 * 1024;
/// Any other reply, at most.
const REPLY_BYTES: usize = 1024 * 1024;
const HELP_BYTES: usize = 256 * 1024;

/// What the connector's child never inherits: the agent's attribution and
/// the terminal it may run in.
pub const CONNECTOR_UNSET: [&str; 8] = [
    "HGS_SESSION",
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
    if cfg!(windows) {
        if let Some(root) = absolute("HGS_CONFIG_DIR") {
            return root.join("mobile").join("connector.json");
        }
        let home = absolute("HOME").or_else(home_dir).unwrap_or_default();
        return home
            .join(".config")
            .join("hgs")
            .join("mobile")
            .join("connector.json");
    }
    let root = absolute("XDG_CONFIG_HOME")
        .or_else(|| home_dir().map(|home| home.join(".config")))
        .unwrap_or_default();
    root.join("hgs").join("mobile").join("connector.json")
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

/// WorkspaceSettings::connectorProgram: beside an absolute `hgs`, beside the
/// client, or on the PATH (the client's own folder when none is found).
pub fn connector_program(hgs: &Path, client_dir: Option<&Path>) -> PathBuf {
    let name = if cfg!(windows) {
        "zerus-mobile-connector.exe"
    } else {
        "zerus-mobile-connector"
    };
    if hgs.is_absolute() {
        if let Some(beside) = hgs
            .parent()
            .map(|dir| dir.join(name))
            .filter(|path| executable(path))
        {
            return beside;
        }
    }
    let bundled = client_dir.map(|dir| dir.join(name));
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

/// The status reply, checked as Qt checks it; a failure is the connector's
/// own short error, or Qt's.
pub fn status(exit: Option<Exit>) -> Result<Workspace, String> {
    let unconfirmed = "Workspace operation was not confirmed. Refresh status before continuing. Verify that the matching computer connector is installed.";
    let (success, stdout) = match exit {
        Some(Exit::Code { code, stdout, .. }) => (code == 0, stdout),
        _ => return Err(unconfirmed.into()),
    };
    let reply = match serde_json::from_slice::<Value>(&stdout) {
        Ok(Value::Object(reply)) if stdout.len() <= STATUS_BYTES => reply,
        _ => return Err(unconfirmed.into()),
    };
    let ok = reply.get("ok").is_none_or(|ok| ok == &Value::Bool(true));
    if success && ok && status_shape(&reply) {
        return Ok(workspace(&reply));
    }
    let error = text(&reply, "error");
    Err(if error.is_empty() || error.chars().count() > 1024 {
        unconfirmed.into()
    } else {
        error.into()
    })
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
    let exit = crate::run::run_without(connector, &call, &[], &CONNECTOR_UNSET, STATUS_BYTES)
        .map_err(|_| WORKSPACE_UNCONFIRMED)?;
    let Exit::Code {
        code: 0, stdout, ..
    } = exit
    else {
        return Err(WORKSPACE_UNCONFIRMED.into());
    };
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| WORKSPACE_UNCONFIRMED)?;
    if !value.is_object() || value.get("ok").is_some_and(|ok| ok != &Value::Bool(true)) {
        return Err(WORKSPACE_UNCONFIRMED.into());
    }
    Ok(value)
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
fn posix_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/@=+,%".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// One PowerShell word. `@` starts a splat and `,` an array there, so both
/// are quoted; a quote doubles inside single quotes.
fn powershell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/=+%".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "''"))
    }
}

/// One word of the terminal the person types the command into: PowerShell
/// on Windows, a POSIX shell elsewhere.
fn shell_word(text: &str) -> String {
    if cfg!(windows) {
        powershell_word(text)
    } else {
        posix_word(text)
    }
}

/// The `hgs` command Qt's panel runs for `action` (PhonePairing.cpp
/// `pinned`): the workspace and relay the person saw are pinned, so a
/// computer whose connection changed meanwhile refuses it.
pub fn pinned(host: &str, action: &str, id: &str, workspace: &str, server: &str) -> String {
    let mut words = vec!["hgs".to_owned()];
    if !host.is_empty() {
        words.push(shell_word(&format!("@{host}")));
    }
    words.extend(["mobile".to_owned(), action.to_owned()]);
    if !id.is_empty() {
        words.push(shell_word(id));
    }
    if !workspace.is_empty() && !server.is_empty() {
        words.extend([
            "--workspace".to_owned(),
            shell_word(workspace),
            "--server".to_owned(),
            shell_word(server),
        ]);
    }
    words.push("--json".to_owned());
    words.join(" ")
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
    let help = crate::run::run_limited(
        hgs,
        &Invocation {
            args: routed(host, &["--help"]),
            ..invocation(&[], probe_time)
        },
        &[],
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
        crate::run::run_limited(hgs, &call, &[], REPLY_BYTES).ok(),
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
