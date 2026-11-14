//! U60: model servers - LLM inference on this computer or the LAN.
//!
//! A model server is any computer that serves an OpenAI- or Anthropic-
//! compatible API. The configuration is local to this computer (`model-servers.json`
//! in the private config folder), never committed and never shared; the API key
//! lives in the same private file. Control commands are argv templates: they are
//! run with an argv list, never through a shell, locally or over hgs's own SSH
//! transport. Without them a server is read only: its models come from
//! `GET /v1/models`.
use crate::{
    cli::{Error, Result},
    config::Config,
    platform,
    state::storage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

/// The private per-computer file: servers and their keys, never committed.
const FILE: &str = "model-servers.json";
const MAX_FILE: u64 = 256 * 1024;
const STATUS_SECS: u64 = 20;
const LOGS_SECS: u64 = 20;
const UP_SECS: u64 = 120;
const MAX_OUTPUT: usize = 256 * 1024;
const MAX_MODELS: usize = 500;
const MAX_LINES: usize = 2000;
const MAX_MODEL_CHARS: usize = 200;
const MAX_KEY: usize = 8192;
const MIN_CONTEXT: u64 = 4096;
const MAX_CONTEXT: u64 = 10_000_000;
/// How long a Claude launch waits for the server's model list (its /model picker).
const PICKER_SECS: u64 = 5;

/// One configured server. The key is never shown by `list`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Server {
    pub name: String,
    pub base_url: String,
    /// openai or anthropic: the API the server speaks.
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// One of this computer's Machines, or an SSH alias; empty means this computer.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ssh: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub up: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub down: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    /// The time limit of a detached up: 30 minutes when unset.
    #[serde(skip_serializing_if = "is_zero")]
    pub up_minutes: u32,
    /// The owner, 2026-11-02: each model's context window in tokens, as the
    /// server runs it. A session on that model tells its agent the window, so
    /// it compacts at the server's limit instead of an assumed one.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub contexts: std::collections::BTreeMap<String, u64>,
    /// Owner spec 2026-11-14: empty (the v1 contract) or `model-catalog/2`,
    /// whose catalog gives each service's variants, live context and API names.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub protocol: String,
    /// model-catalog/2 through the SSH transport: an argv printing the catalog.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub catalog: Vec<String>,
    /// model-catalog/2 through HTTP: the catalog API root and its private key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<CatalogApi>,
    /// Selects a service's variant: an argv with `{model}` and `{variant}`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variant: Vec<String>,
}

/// The HTTP door of a model-catalog/2 server (`GET /v1/catalog`, model actions).
/// The key stays in the private file, as the API key does.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CatalogApi {
    pub base_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
}

/// The catalog protocol this hgs reads.
pub(crate) const CATALOG_PROTOCOL: &str = "model-catalog/2";

#[derive(Default, Deserialize, Serialize)]
struct Store {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    servers: Vec<Server>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn path(config: &Config) -> std::path::PathBuf {
    config.dir.join(FILE)
}

/// The configured servers; a missing file is no server at all.
pub(crate) fn load(config: &Config) -> Result<Vec<Server>> {
    let path = path(config);
    let opened = std::fs::OpenOptions::new().read(true).open(&path);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(Error::new(1, "Cannot read the model servers configuration")),
    };
    #[cfg(windows)]
    if !crate::private_stores::ensure_file(&path, &file).unwrap_or(false) {
        return Err(Error::new(1, "The model servers configuration must be private to this Windows user"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|_| Error::new(1, "Cannot read the model servers configuration"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(Error::new(1, "The model servers configuration is too large"));
    }
    let store: Store = serde_json::from_slice(&bytes)
        .map_err(|error| Error::new(1, format!("Invalid model servers configuration: {error}")))?;
    let mut servers = Vec::new();
    for server in store.servers {
        servers.push(checked(server)?);
    }
    Ok(servers)
}

/// Writes the servers to the private file; the key stays only there. Settings >
/// Model servers writes the configuration this way (`save --json`).
pub(crate) fn save(config: &Config, servers: &[Server]) -> Result<()> {
    for server in servers {
        checked(server.clone())?;
    }
    std::fs::create_dir_all(&config.dir).map_err(|_| Error::new(1, "Cannot create the configuration folder"))?;
    platform::private_dir(&config.dir).map_err(|error| Error::new(1, error.to_string()))?;
    let store = Store { version: 1, servers: servers.to_vec() };
    let text = serde_json::to_string_pretty(&store).map_err(|error| Error::new(1, error.to_string()))?;
    storage::atomic_private(&path(config), &text).map_err(|error| Error::new(1, error.to_string()))
}

/// A name for a server, a model or an SSH alias: no spaces, quotes or shell
/// syntax. A model may name a provider (`provider/model`).
fn safe_name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_.:/@+".contains(ch))
}

/// A server name or an SSH alias: no spaces, colons or shell syntax.
fn safe_word(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))
}

/// One model name as a server or a user may write it (no spaces, quotes or
/// shell syntax), bounded.
pub(crate) fn checked_model(value: &str) -> Result<String> {
    let value = value.trim();
    if !safe_name(value, MAX_MODEL_CHARS) || value.starts_with('-') {
        return Err(Error::new(1, "A model name uses letters, digits and -_.:/@+ only, without a leading -"));
    }
    Ok(value.to_owned())
}

/// `http://` or `https://` followed by `HOST[:PORT][/path]`, no credentials and
/// no query.
pub(super) fn checked_base_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("http://").or_else(|| value.strip_prefix("https://")) else { return false };
    if rest.is_empty() || rest.contains(['?', '#', '@', ' ', '\t', '\n']) || value.len() > 2000 {
        return false;
    }
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(host, port)| {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            ""
        } else {
            host
        }
    });
    !host.is_empty() && !host.starts_with(':')
}

/// The server root: the configured base without its trailing `/v1` or slash, so
/// both spellings (with and without `/v1`) name the same server and every
/// consumer derives its own URL from it.
pub(crate) fn root(server: &Server) -> String {
    let base = server.base_url.trim_end_matches('/');
    match base.strip_suffix("/v1") {
        Some(root) => root.trim_end_matches('/').to_owned(),
        None => base.to_owned(),
    }
}

/// The API's model list: `root/v1/models`.
pub(crate) fn models_url(server: &Server) -> String {
    format!("{}/v1/models", root(server))
}

/// The base an OpenAI-compatible client needs: `root/v1`.
pub(crate) fn openai_base(server: &Server) -> String {
    format!("{}/v1", root(server))
}

/// The base an Anthropic client needs: the root (Claude Code appends
/// `/v1/messages` itself).
pub(crate) fn anthropic_base(server: &Server) -> String {
    root(server)
}

/// Validates one server: its name, API, SSH alias, key, model names and argv
/// templates (argv lists, never shell lines; `{model}`/`{server}` only). The
/// API URL is stored as its root, so a later edit cannot pair `/v1` twice.
pub(crate) fn checked(server: Server) -> Result<Server> {
    if !safe_word(&server.name, 64) || server.name.starts_with(['-', '.']) {
        return Err(Error::new(1, "A server name uses letters, digits and -_. only"));
    }
    if !checked_base_url(&server.base_url) {
        return Err(Error::new(1, "The API URL must be http:// or https://HOST[:PORT][/path], without credentials or a query"));
    }
    if !matches!(server.kind.as_str(), "openai" | "anthropic") {
        return Err(Error::new(1, "A server kind is openai or anthropic"));
    }
    if !server.ssh.is_empty() && (!safe_word(&server.ssh, 255) || server.ssh.starts_with('-')) {
        return Err(Error::new(1, "The SSH host is one of this computer's Machines or an SSH alias"));
    }
    if server.key.chars().count() > MAX_KEY || server.key.chars().any(char::is_control) {
        return Err(Error::new(1, "The API key is at most 8192 characters without control characters"));
    }
    for (field, argv) in [("status", &server.status), ("up", &server.up), ("down", &server.down), ("logs", &server.logs), ("catalog", &server.catalog), ("variant", &server.variant)] {
        if argv.len() > 64 || argv.iter().any(|arg| arg.len() > 2000 || arg.chars().any(|ch| ch.is_control())) {
            return Err(Error::new(1, format!("The {field} command is an argv list of at most 64 arguments")));
        }
        if argv.iter().any(|arg| arg.contains('{') || arg.contains('}')) {
            for arg in argv {
                let mut rest = arg.as_str();
                while let Some(at) = rest.find('{') {
                    let tail = &rest[at + 1..];
                    let Some(end) = tail.find('}') else { return Err(Error::new(1, format!("The {field} command has an unclosed placeholder"))) };
                    let placeholder = &tail[..end];
                    if !(matches!(placeholder, "model" | "server") || (field == "variant" && placeholder == "variant")) {
                        return Err(Error::new(1, if field == "variant" {
                            "The variant command names a placeholder other than {model}, {variant} or {server}".to_owned()
                        } else {
                            format!("The {field} command names a placeholder other than {{model}} or {{server}}")
                        }));
                    }
                    rest = &tail[end + 1..];
                }
            }
        }
        if !argv.is_empty() && !argv.iter().any(|arg| arg.contains("{model}")) && matches!(field, "up" | "down" | "logs" | "variant") {
            return Err(Error::new(1, format!("The {field} command names the model with {{model}}")));
        }
        if field == "variant" && !argv.is_empty() && !argv.iter().any(|arg| arg.contains("{variant}")) {
            return Err(Error::new(1, "The variant command names the variant with {variant}"));
        }
    }
    if !server.protocol.is_empty() && server.protocol != CATALOG_PROTOCOL {
        return Err(Error::new(1, format!("The protocol is {CATALOG_PROTOCOL}, or empty for the plain status contract")));
    }
    let api = match server.api {
        Some(api) => {
            if !checked_base_url(&api.base_url) {
                return Err(Error::new(1, "The catalog API URL must be http:// or https://HOST[:PORT][/path], without credentials or a query"));
            }
            if api.key.chars().count() > MAX_KEY || api.key.chars().any(char::is_control) {
                return Err(Error::new(1, "The catalog API key is at most 8192 characters without control characters"));
            }
            Some(CatalogApi { base_url: api.base_url.trim_end_matches('/').to_owned(), key: api.key })
        }
        None => None,
    };
    if server.protocol.is_empty() && (api.is_some() || !server.catalog.is_empty() || !server.variant.is_empty()) {
        return Err(Error::new(1, format!("catalog, api and variant need protocol {CATALOG_PROTOCOL}")));
    }
    if server.protocol == CATALOG_PROTOCOL && api.is_none() && server.catalog.is_empty() {
        return Err(Error::new(1, format!("A {CATALOG_PROTOCOL} server reads its catalog with a catalog command or an api")));
    }
    let server = Server { api, ..server };
    if server.contexts.len() > MAX_MODELS {
        return Err(Error::new(1, "At most 500 model context windows"));
    }
    for (model, tokens) in &server.contexts {
        checked_model(model).map_err(|_| Error::new(1, format!("The context window of {model} names no valid model")))?;
        if !(MIN_CONTEXT..=MAX_CONTEXT).contains(tokens) {
            return Err(Error::new(1, format!("The context window of {model} is 4096 to 10000000 tokens")));
        }
    }
    let minutes = if server.up_minutes == 0 { 30 } else { server.up_minutes };
    if !(1..=240).contains(&minutes) {
        return Err(Error::new(1, "The up time limit is 1 to 240 minutes"));
    }
    let base_url = root(&server);
    Ok(Server { up_minutes: minutes, base_url, ..server })
}

/// The public row: the key is never included, only whether one is set.
pub(crate) fn public(server: &Server) -> Value {
    json!({
        "name": server.name,
        "base_url": server.base_url,
        "kind": server.kind,
        "ssh": server.ssh,
        "key": !server.key.is_empty(),
        "command": {
            "status": !server.status.is_empty(),
            "up": !server.up.is_empty(),
            "down": !server.down.is_empty(),
            "logs": !server.logs.is_empty(),
        },
        "up_minutes": server.up_minutes,
        // U60 (#656): the argv templates Settings edits; never the key.
        "argv": {"status": server.status, "up": server.up, "down": server.down, "logs": server.logs,
            "catalog": server.catalog, "variant": server.variant},
        "contexts": server.contexts,
        "protocol": server.protocol,
        // The catalog API's root and whether its key is set; never the key.
        "api": server.api.as_ref().map(|api| json!({"base_url": api.base_url, "key": !api.key.is_empty()})),
        "variant": !server.variant.is_empty() || server.api.is_some(),
    })
}

/// U60 (#656): one server as Settings sends it. `was` is its previous name; a
/// request without `key` keeps the key stored under `was`, `"key": null`
/// removes it. Every error names the server and the field.
fn requested(mut entry: Value, stored: &[Server]) -> Result<(Server, String)> {
    let Some(object) = entry.as_object_mut() else { return Err(Error::new(1, "Each model server is a JSON object")) };
    let label = object.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or("(unnamed)").to_owned();
    let was = match object.remove("was") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(was)) => was,
        Some(_) => return Err(Error::new(1, format!("Server {label}: was is the server's previous name"))),
    };
    let key = match object.remove("key") {
        None => stored.iter().find(|server| !was.is_empty() && server.name == was).map(|server| server.key.clone()).unwrap_or_default(),
        Some(Value::Null) => String::new(),
        Some(Value::String(key)) => key,
        Some(_) => return Err(Error::new(1, format!("Server {label}: key is a string, or null to remove it"))),
    };
    // The catalog API key follows the API key's rule: absent keeps the key
    // stored under `was` while its API root is unchanged, null removes it.
    let previous_api = stored.iter().find(|server| !was.is_empty() && server.name == was).and_then(|server| server.api.clone());
    if let Some(api) = entry.get_mut("api").and_then(Value::as_object_mut) {
        match api.get("key") {
            None => {
                let root = api.get("base_url").and_then(Value::as_str).unwrap_or_default().trim_end_matches('/').to_owned();
                if let Some(previous) = previous_api.filter(|previous| previous.base_url == root) {
                    api.insert("key".into(), Value::String(previous.key));
                }
            }
            Some(Value::Null) => {
                api.remove("key");
            }
            Some(Value::String(_)) => {}
            Some(_) => return Err(Error::new(1, format!("Server {label}: api.key is a string, or null to remove it"))),
        }
    }
    let mut server: Server = serde_json::from_value(entry).map_err(|error| Error::new(1, format!("Server {label}: {error}")))?;
    server.key = key;
    let server = checked(server).map_err(|error| Error::new(1, format!("Server {label}: {}", error.message)))?;
    Ok((server, was))
}

/// U60 (#656): `hgs model-server save --json` - Settings replaces the whole
/// list; only you change it (the commands run on the server).
pub(crate) fn save_request(config: &Config, input: &Value) -> Result<Value> {
    super::owner_request(input, "the model servers").map_err(|error| Error::new(1, error))?;
    let stored = load(config)?;
    let entries = input["servers"].as_array().ok_or_else(|| Error::new(1, "The request carries \"servers\": a list"))?;
    if entries.len() > 64 {
        return Err(Error::new(1, "At most 64 model servers"));
    }
    let mut servers: Vec<Server> = Vec::new();
    for entry in entries {
        let (server, _) = requested(entry.clone(), &stored)?;
        if servers.iter().any(|other| other.name == server.name) {
            return Err(Error::new(1, format!("Server {}: name is already used by another server", server.name)));
        }
        servers.push(server);
    }
    save(config, &servers)?;
    Ok(json!({"status": "saved", "servers": servers.iter().map(public).collect::<Vec<_>>()}))
}

/// U60 (#656): `hgs model-server test --json` - status once for one server as
/// Settings shows it, saved or not; nothing is written. A changed base URL
/// never reuses the stored key: a test can only send it to the same root.
pub(crate) fn test_request(config: &Config, input: &Value) -> Result<std::result::Result<Value, Failure>> {
    let stored = load(config)?;
    let server = test_server(input, &stored)?;
    Ok(status(config, &server))
}

/// The server a `test` request names: without an explicit key, the stored key
/// is kept only while the root is unchanged (L2).
fn test_server(input: &Value, stored: &[Server]) -> Result<Server> {
    let explicit_key = input.get("key").is_some();
    let (mut server, was) = requested(input.clone(), stored)?;
    if !explicit_key && !was.is_empty() {
        let same = stored.iter().find(|old| old.name == was).is_some_and(|old| root(old) == root(&server));
        if !same {
            server.key.clear();
        }
    }
    Ok(server)
}

/// Substitutes `{model}` and `{server}` in every argument; the values are
/// validated names, so the argv reaches the server as an argv, never a shell.
pub(super) fn filled(argv: &[String], model: &str, server: &str) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.replace("{model}", model).replace("{server}", server))
        .collect()
}

/// How a command or a request failed. The kinds are distinct and nothing here
/// retries.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure {
    Unreachable(String),
    Refused(String),
    TimedOut(String),
    Unauthorized(String),
}

impl Failure {
    fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "unreachable",
            Self::Refused(_) => "refused",
            Self::TimedOut(_) => "timed_out",
            Self::Unauthorized(_) => "unauthorized",
        }
    }
    fn detail(&self) -> &str {
        match self {
            Self::Unreachable(detail) | Self::Refused(detail) | Self::TimedOut(detail) | Self::Unauthorized(detail) => detail,
        }
    }
    pub(crate) fn value(&self) -> Value {
        json!({"status": "error", "error": self.kind(), "detail": self.detail()})
    }
}

#[derive(Clone)]
pub(crate) struct Run {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[cfg(test)]
type FakeCommand = Box<dyn Fn(&Server, &[String], Duration) -> std::result::Result<Run, Failure>>;

#[cfg(test)]
thread_local! {
    static FAKE: std::cell::RefCell<Option<FakeCommand>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_fake_command(fake: Option<FakeCommand>) {
    FAKE.with(|slot| *slot.borrow_mut() = fake);
}

fn execute(mut command: Command, timeout: Duration, ssh: bool) -> std::result::Result<Run, Failure> {
    command.stdin(Stdio::null());
    match platform::output_until(&mut command, timeout) {
        Ok(out) => {
            let stdout: String = String::from_utf8_lossy(&out.stdout).chars().take(MAX_OUTPUT).collect();
            let stderr: String = String::from_utf8_lossy(&out.stderr).chars().take(2000).collect();
            Ok(Run { code: out.status.code(), stdout, stderr })
        }
        Err(platform::RunError::Stopped) => {
            let where_ = if ssh { "over SSH" } else { "on this computer" };
            Err(Failure::TimedOut(format!("The command did not finish in {} s {where_}", timeout.as_secs())))
        }
        Err(platform::RunError::TooLarge) => Err(Failure::Refused("The command wrote more output than allowed".into())),
        Err(platform::RunError::Spawn(error)) | Err(platform::RunError::Io(error)) => {
            if ssh {
                Err(Failure::Unreachable(format!("SSH did not start: {error}")))
            } else {
                Err(Failure::Refused(format!("The command did not start: {error}")))
            }
        }
        Err(platform::RunError::Failed) | Err(platform::RunError::Unsupported) => Err(Failure::Refused("The command could not run".into())),
    }
}

/// The ssh command that runs one argv on the server, with BatchMode and the
/// peer's login-shell quoting. No argument passes through a local shell, and
/// `-n` keeps the client from reading this process's stdin (Windows OpenSSH
/// never sends its end of NUL, so a remote command that reads stdin would wait
/// for the time limit).
fn ssh_argv(config: &Config, server: &Server, argv: &[String]) -> Vec<String> {
    let mut ssh = vec![platform::ssh_program().to_string_lossy().into_owned()];
    ssh.extend(crate::machines::ssh_options(config, &server.ssh));
    ssh.extend([
        "-n".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=3".to_owned(),
    ]);
    ssh.push(server.ssh.clone());
    ssh.push(platform::remote_join(argv));
    ssh
}

/// A configured Windows Machine has no POSIX login shell for the remote argv:
/// its control commands are refused rather than quoted for the wrong shell.
fn windows_machine(config: &Config, server: &Server) -> bool {
    !server.ssh.is_empty() && crate::windows_peer::is_windows(config, &server.ssh)
}

/// Runs one control template: an argv locally, or the same argv over hgs's own
/// SSH transport (BatchMode, no shell on this side). A non-zero exit is a
/// refusal; SSH's own 255 is an unreachable host.
pub(super) fn run_template(config: &Config, server: &Server, argv: &[String], timeout: Duration) -> std::result::Result<Run, Failure> {
    if argv.is_empty() {
        return Err(Failure::Refused("The command is empty".into()));
    }
    if windows_machine(config, server) {
        return Err(Failure::Refused(format!(
            "{} is a Windows Machine: its control command needs a POSIX login shell; run it there by hand",
            server.ssh
        )));
    }
    #[cfg(test)]
    if let Some(fake) = FAKE.with(|slot| slot.borrow().as_ref().map(|fake| fake(server, argv, timeout))) {
        return fake;
    }
    let ssh = !server.ssh.is_empty();
    let command = if ssh {
        let argv = ssh_argv(config, server, argv);
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command.env("GIT_TERMINAL_PROMPT", "0");
        command
    } else {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    };
    let run = execute(command, timeout, ssh)?;
    if run.code == Some(0) {
        return Ok(run);
    }
    let detail = first_line(&run.stderr).or_else(|| first_line(&run.stdout)).unwrap_or_else(|| format!("The command failed ({})", run.code.map_or_else(|| "no exit code".into(), |code| code.to_string())));
    if ssh && run.code == Some(255) {
        return Err(Failure::Unreachable(detail));
    }
    Err(Failure::Refused(detail))
}

fn first_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|line| !line.is_empty()).map(|line| line.chars().take(300).collect())
}

/// The server's status: the control command when configured, else the API's
/// model list. A command may print the Zerus JSON contract or `NAME: STATE`
/// lines with an optional memory line; other lines are kept raw.
pub(crate) fn status(config: &Config, server: &Server) -> std::result::Result<Value, Failure> {
    if super::model_catalog::is_catalog(server) {
        return super::model_catalog::status(config, server);
    }
    let value = if server.status.is_empty() {
        let models = api_models(config, server)?;
        json!({"server": server.name, "source": "api", "models": models, "memory": Value::Null, "raw": []})
    } else {
        let argv = filled(&server.status, "", &server.name);
        let run = run_template(config, server, &argv, Duration::from_secs(STATUS_SECS))?;
        let parsed = parse_status(&run.stdout);
        json!({"server": server.name, "source": "command", "models": parsed.models, "memory": parsed.memory, "raw": parsed.raw})
    };
    Ok(value)
}

#[derive(Default)]
struct Parsed {
    models: Vec<Value>,
    memory: Value,
    raw: Vec<String>,
}

fn number(text: &str) -> Option<f64> {
    text.trim().trim_end_matches(|ch: char| ch.is_ascii_alphabetic()).parse::<f64>().ok()
}

/// `NAME: STATE` splits at the last `": "` (or, without one, the last `':'`),
/// so a model name carrying a tag (`coder-a:q4: running`) stays whole and Down
/// targets exactly that model.
fn split_state(line: &str) -> Option<(&str, &str)> {
    match line.rsplit_once(": ") {
        Some(pair) => Some(pair),
        None => line.rsplit_once(':'),
    }
}

/// A model's optional `since`: seconds since the epoch, or an RFC 3339 time;
/// anything else is left out.
fn since(value: &Value) -> Option<Value> {
    match value {
        Value::Number(number) => number.as_f64().filter(|seconds| *seconds > 0.0 && seconds.is_finite()).map(|seconds| json!(seconds)),
        Value::String(text) => chrono::DateTime::parse_from_rfc3339(text.trim()).ok().map(|time| json!(time.timestamp() as f64)),
        _ => None,
    }
}

fn parse_status(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(models) = value["models"].as_array() {
                for model in models.iter().take(MAX_MODELS) {
                    let Some(name) = model["name"].as_str().filter(|name| !name.is_empty() && !name.starts_with('-')) else { continue };
                    let state = model["state"].as_str().unwrap_or("unknown").to_ascii_lowercase();
                    let mut row = json!({"name": name, "state": state});
                    // #703: when the state began, if the command says so.
                    if let Some(since) = since(&model["since"]) {
                        row["since"] = since;
                    }
                    parsed.models.push(row);
                }
                if let Some(memory) = value["memory"].as_object() {
                    if let (Some(free), Some(total)) = (memory["free"].as_f64(), memory["total"].as_f64()) {
                        parsed.memory = json!({"free": free, "total": total});
                    }
                }
                return parsed;
            }
        }
    }
    for line in text.lines().take(MAX_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        // A server's own tool may answer in Russian ("Свободно памяти: 43.6 ГиБ
        // из 121.6"): free first, then total, as in English.
        let memory = ["memory", "free memory", "ram", "свободно памяти", "память"].iter().any(|prefix| lower.starts_with(prefix));
        if parsed.memory.is_null() && memory {
            let numbers: Vec<f64> = trimmed
                .split(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-')
                .filter(|part| !part.is_empty())
                .filter_map(number)
                .collect();
            if numbers.len() >= 2 {
                parsed.memory = json!({"free": numbers[0], "total": numbers[1]});
                continue;
            }
        }
        if let Some((name, state)) = split_state(trimmed) {
            let (name, state) = (name.trim(), state.trim());
            if !name.is_empty() && !name.starts_with('-') && !name.contains(char::is_whitespace) && !state.is_empty() && state.split_whitespace().count() <= 3 {
                parsed.models.push(json!({"name": name, "state": state.to_ascii_lowercase()}));
                continue;
            }
        }
        parsed.raw.push(line.to_owned());
    }
    parsed
}

/// The API's model list, `GET /v1/models`, with the server's key. http uses the
/// small client below; https runs the system curl, whose TLS is the system's,
/// so hgs needs no TLS crate and the agents speak TLS themselves anyway.
fn api_models(config: &Config, server: &Server) -> std::result::Result<Vec<Value>, Failure> {
    Ok(api_models_within(config, server, http_timeout())?
        .into_iter()
        .map(|mut model| {
            model["state"] = json!("available");
            model
        })
        .collect())
}

/// What the system curl did. `Missing` (no curl on this computer) and
/// `TimedOut` are not exit codes; they are reported as their own errors.
pub(crate) enum CurlRun {
    Ran(Run),
    Missing,
    TimedOut,
    Failed(String),
}

const CURL_MISSING: &str = "curl was not found: install curl to read an https model server";

/// The system curl: `curl.exe` on Windows 10+, `curl` elsewhere.
fn curl_program() -> &'static str {
    if cfg!(windows) { "curl.exe" } else { "curl" }
}

#[cfg(test)]
pub(crate) type FakeCurl = Box<dyn Fn(&[String], Option<&str>) -> CurlRun>;

#[cfg(test)]
thread_local! {
    static FAKE_CURL: std::cell::RefCell<Option<FakeCurl>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_fake_curl(fake: Option<FakeCurl>) {
    FAKE_CURL.with(|slot| *slot.borrow_mut() = fake);
}

fn curl_run(argv: &[String], config: Option<&str>, timeout: Duration) -> CurlRun {
    #[cfg(test)]
    if let Some(fake) = FAKE_CURL.with(|slot| slot.borrow().as_ref().map(|fake| fake(argv, config))) {
        return fake;
    }
    // The header text is only read by the test fake; curl reads it from its
    // --config file.
    let _ = config;
    if platform::which(curl_program()).is_none() {
        return CurlRun::Missing;
    }
    let mut command = Command::new(curl_program());
    command.args(argv);
    match platform::output_until(&mut command, timeout) {
        Ok(out) => CurlRun::Ran(Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).chars().take(MAX_OUTPUT).collect(),
            stderr: String::from_utf8_lossy(&out.stderr).chars().take(2000).collect(),
        }),
        Err(platform::RunError::Stopped) => CurlRun::TimedOut,
        Err(platform::RunError::Spawn(error)) | Err(platform::RunError::Io(error)) => CurlRun::Failed(error.to_string()),
        Err(_) => CurlRun::Failed("curl could not run".into()),
    }
}

/// The key reaches curl in a private config file (`--config`), never on the
/// command line: a process listing or a log keeps only the file path.
fn curl_header(server: &Server) -> std::result::Result<Option<String>, Failure> {
    if server.key.is_empty() {
        return Ok(None);
    }
    let key = header_safe(&server.key)?.replace('\\', "\\\\").replace('"', "\\\"");
    let lines = match server.kind.as_str() {
        "anthropic" => vec![format!("header = \"x-api-key: {key}\""), "header = \"anthropic-version: 2023-06-01\"".to_owned()],
        _ => vec![format!("header = \"Authorization: Bearer {key}\"")],
    };
    Ok(Some(lines.join("\n") + "\n"))
}

/// `--write-out` appends the status code after the body; split it off.
fn split_status(stdout: &str) -> (String, Option<u16>) {
    match stdout.rsplit_once('\n') {
        Some((body, code)) if code.trim().len() == 3 && code.trim().bytes().all(|b| b.is_ascii_digit()) => {
            (body.to_owned(), code.trim().parse().ok())
        }
        _ => (stdout.to_owned(), None),
    }
}

fn curl_failure(code: Option<u16>, body: &str, stderr: &str) -> Failure {
    match code {
        Some(401 | 403) => Failure::Unauthorized(format!("The server refused the API key (HTTP {})", code.unwrap_or_default())),
        Some(code) => http_failure(code, body),
        None => Failure::Refused(first_line(stderr).or_else(|| first_line(body)).unwrap_or_else(|| "curl printed no HTTP status".into())),
    }
}

/// The https model list through the system curl: `--fail-with-body` keeps the
/// error body and `--write-out` the status code. Curl's exit codes map to the
/// same failures as the http client: 6/7 unreachable, 28 timed out, 22 an HTTP
/// error (401/403 unauthorized), anything else refused.
fn curl_get(config: &Config, server: &Server, url: &str) -> std::result::Result<(u16, String), Failure> {
    curl_send(config, server, url, None)
}

/// `curl_get` with an optional JSON body, sent as a POST.
pub(super) fn curl_send(config: &Config, server: &Server, url: &str, body: Option<&str>) -> std::result::Result<(u16, String), Failure> {
    let timeout = http_timeout();
    let mut argv = vec![
        "-sS".to_owned(),
        "--max-time".to_owned(),
        timeout.as_secs().to_string(),
        "--fail-with-body".to_owned(),
        "--write-out".to_owned(),
        "\n%{http_code}".to_owned(),
    ];
    let header = curl_header(server)?;
    let mut header_file = None;
    if let Some(header) = &header {
        platform::private_dir(&config.dir).map_err(|error| Failure::Refused(error.to_string()))?;
        let mut file = tempfile::Builder::new()
            .prefix(".model-server-")
            .suffix(".conf")
            .tempfile_in(&config.dir)
            .map_err(|error| Failure::Refused(format!("The private header file could not be created: {error}")))?;
        file.write_all(header.as_bytes())
            .map_err(|error| Failure::Refused(format!("The private header file could not be written: {error}")))?;
        file.flush().ok();
        argv.push("--config".into());
        argv.push(file.path().display().to_string());
        header_file = Some(file);
    }
    if let Some(body) = body {
        argv.extend(["-X".to_owned(), "POST".to_owned(), "-H".to_owned(), "Content-Type: application/json".to_owned(), "--data-binary".to_owned(), body.to_owned()]);
    }
    argv.push(url.to_owned());
    let run = curl_run(&argv, header.as_deref(), timeout + Duration::from_secs(5));
    drop(header_file);
    match run {
        CurlRun::Missing => Err(Failure::Refused(CURL_MISSING.into())),
        CurlRun::TimedOut => Err(Failure::TimedOut(format!("The server did not answer in {} s", timeout.as_secs()))),
        CurlRun::Failed(detail) => Err(Failure::Refused(format!("curl could not run: {detail}"))),
        CurlRun::Ran(run) => {
            let (body, status) = split_status(&run.stdout);
            match run.code {
                Some(0) => status
                    .map(|code| (code, body))
                    .ok_or_else(|| Failure::Refused("curl printed no HTTP status".into())),
                Some(22) => Err(curl_failure(status, &body, &run.stderr)),
                Some(6 | 7) => Err(Failure::Unreachable(first_line(&run.stderr).unwrap_or_else(|| "The server did not answer".into()))),
                Some(28) => Err(Failure::TimedOut(format!("The server did not answer in {} s", timeout.as_secs()))),
                _ => Err(Failure::Refused(
                    first_line(&run.stderr)
                        .or_else(|| first_line(&body))
                        .unwrap_or_else(|| format!("curl failed ({})", run.code.map_or_else(|| "no exit code".into(), |code| code.to_string()))),
                )),
            }
        }
    }
}

/// The status read's time limit; tests shorten it.
fn http_timeout() -> Duration {
    #[cfg(test)]
    if let Some(timeout) = TEST_TIMEOUT.with(std::cell::Cell::get) {
        return timeout;
    }
    Duration::from_secs(STATUS_SECS)
}

#[cfg(test)]
thread_local! {
    static TEST_TIMEOUT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_http_timeout(timeout: Option<Duration>) {
    TEST_TIMEOUT.with(|slot| slot.set(timeout));
}

/// A minimal HTTP/1.1 GET with a deadline for an http server (an https server
/// is read through the system curl, `curl_get`). The status line decides:
/// 401/403 is unauthorized, another failure is refused, a connect error
/// unreachable and a read timeout timed_out.
fn http_get(url: &str, server: &Server, timeout: Duration) -> std::result::Result<(u16, String), Failure> {
    http_send("GET", url, server, timeout, None)
}

/// `http_get` with a method and an optional JSON body (model-catalog/2 actions).
pub(super) fn http_send(method: &str, url: &str, server: &Server, timeout: Duration, body: Option<&str>) -> std::result::Result<(u16, String), Failure> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(Failure::Refused("The API URL must be http:// or https://".into()));
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().map_err(|_| Failure::Refused("The API port is not a number".into()))?),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err(Failure::Refused("The API URL has no host".into()));
    }
    let addresses = (host, port).to_socket_addrs().map_err(|error| Failure::Unreachable(format!("The server address did not resolve: {error}")))?;
    let mut stream = None;
    let mut last = String::new();
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout.min(Duration::from_secs(10))) {
            Ok(found) => {
                stream = Some(found);
                break;
            }
            Err(error) => last = error.to_string(),
        }
    }
    let mut stream = stream.ok_or_else(|| Failure::Unreachable(format!("The server did not answer: {last}")))?;
    stream.set_read_timeout(Some(timeout)).map_err(|error| Failure::Refused(error.to_string()))?;
    stream.set_write_timeout(Some(timeout)).map_err(|error| Failure::Refused(error.to_string()))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\nUser-Agent: hgs model-server\r\n"
    );
    if let Some(body) = body {
        request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    if !server.key.is_empty() {
        match server.kind.as_str() {
            "anthropic" => {
                request.push_str(&format!("x-api-key: {}\r\nanthropic-version: 2023-06-01\r\n", header_safe(&server.key)?));
            }
            _ => request.push_str(&format!("Authorization: Bearer {}\r\n", header_safe(&server.key)?)),
        }
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    stream.write_all(request.as_bytes()).map_err(|error| Failure::Refused(error.to_string()))?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > MAX_OUTPUT {
                    return Err(Failure::Refused("The model list is larger than allowed".into()));
                }
            }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(Failure::TimedOut(format!("The server did not answer in {} s", timeout.as_secs())));
            }
            Err(error) => return Err(Failure::Refused(format!("The server closed the connection: {error}"))),
        }
    }
    parse_http(&bytes)
}

/// A key is quoted as a header value: no CR/LF or other control characters.
fn header_safe(key: &str) -> std::result::Result<String, Failure> {
    if key.chars().any(char::is_control) {
        return Err(Failure::Refused("The API key has a control character".into()));
    }
    Ok(key.to_owned())
}

fn parse_http(bytes: &[u8]) -> std::result::Result<(u16, String), Failure> {
    let text = String::from_utf8_lossy(bytes);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let code: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| Failure::Refused("The server's answer is not HTTP".into()))?;
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim().to_ascii_lowercase();
        if key == "transfer-encoding" && value.trim().eq_ignore_ascii_case("chunked") {
            chunked = true;
        }
        if key == "content-length" {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let body = if chunked {
        decoded_chunks(body).map_err(Failure::Refused)?
    } else if let Some(length) = length {
        body[..body.len().min(length)].to_owned()
    } else {
        body.to_owned()
    };
    Ok((code, body))
}

fn decoded_chunks(mut body: &str) -> std::result::Result<String, String> {
    let mut out = String::new();
    loop {
        let Some((size, rest)) = body.split_once("\r\n") else { return Err("Invalid chunked answer".into()) };
        let size = usize::from_str_radix(size.split(';').next().unwrap_or_default().trim(), 16).map_err(|_| "Invalid chunk size".to_string())?;
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err("A chunk is shorter than its size".into());
        }
        out.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
}

/// `HTTP` maps the code to the failure the caller reports.
fn http_failure(code: u16, body: &str) -> Failure {
    let detail = body.chars().take(300).collect::<String>();
    if matches!(code, 401 | 403) {
        return Failure::Unauthorized(format!("The server refused the API key (HTTP {code})"));
    }
    Failure::Refused(if detail.trim().is_empty() { format!("The server answered HTTP {code}") } else { format!("HTTP {code}: {detail}") })
}

/// The receipt of `up`: it runs the template now, or starts it as a detached
/// hgs run with the server's time limit.
pub(crate) fn up(config: &Config, server: &Server, model: &str, detach: bool) -> std::result::Result<Value, Failure> {
    let model = checked_model(model).map_err(|error| Failure::Refused(error.message))?;
    if server.up.is_empty() {
        if let Some(api) = server.api.as_ref().filter(|_| super::model_catalog::is_catalog(server)) {
            return super::model_catalog::api_up(config, server, api, &model);
        }
        return Err(Failure::Refused(format!("{} has no up command; it is read only", server.name)));
    }
    if windows_machine(config, server) {
        return Err(Failure::Refused(format!(
            "{} is a Windows Machine: its up command needs a POSIX login shell; run it there by hand",
            server.ssh
        )));
    }
    let argv = filled(&server.up, &model, &server.name);
    if detach {
        let request = uuid::Uuid::new_v4().to_string();
        let label = format!("up {} {model}", server.name);
        let argv = detached_argv(config, server, &argv);
        crate::state::builds::start_argv_for(&request, &label, &argv, &std::env::current_dir().map_err(|error| Failure::Refused(error.to_string()))?, json!({"name":"","run_id":"","conversation_id":""}), server.up_minutes, &[])
            .map_err(Failure::Refused)?;
        return Ok(json!({"status":"started","server":server.name,"model":model,"request_id":request,
            "run":"hgs run","timeout_minutes":server.up_minutes}));
    }
    let run = run_template(config, server, &argv, Duration::from_secs(UP_SECS))?;
    Ok(json!({"status":"ran","server":server.name,"model":model,"exit":run.code}))
}

/// The argv of a detached up: the template itself locally, or hgs's ssh command
/// with the argv quoted for the peer. The runner starts it as an argv, so
/// Windows never sees a POSIX-quoted shell line (N5).
fn detached_argv(config: &Config, server: &Server, argv: &[String]) -> Vec<String> {
    if server.ssh.is_empty() {
        argv.to_vec()
    } else {
        ssh_argv(config, server, argv)
    }
}

/// #703: a detached up's outcome, read by its receipt's `request_id`: the run's
/// record while it runs and once it ended (exit, time limit, the last lines of
/// its output). Only a detached up's own run is read, never another run.
pub(crate) fn up_result(id: &str) -> std::result::Result<Value, Failure> {
    let unknown = || Failure::Refused(format!("No detached up with request_id {id}"));
    if uuid::Uuid::parse_str(id).map(|parsed| parsed.hyphenated().to_string() != id).unwrap_or(true) {
        return Err(unknown());
    }
    let record = crate::state::builds::record(id, crate::state::now()).ok_or_else(unknown)?;
    let label = record["label"].as_str().unwrap_or_default();
    if !label.starts_with("up ") || !record["argv"].is_array() {
        return Err(unknown());
    }
    let mut words = label.splitn(3, ' ').skip(1);
    let (server, model) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
    let tail = record["tail"].as_str().unwrap_or_default();
    let tail: String = tail.chars().rev().take(UP_TAIL_CHARS).collect::<Vec<_>>().into_iter().rev().collect();
    Ok(json!({"request_id":id,"server":server,"model":model,"status":record["status"],"exit":record["exit"],
        "timed_out":record["timed_out"].as_bool().unwrap_or(false),"error":record["error"],"tail":tail,
        "started":record["started"],"ended":record["ended"],"timeout_secs":record["timeout_secs"]}))
}

/// The last characters of a detached up's output that `up-result` returns.
const UP_TAIL_CHARS: usize = 1000;

/// #703: this computer's live sessions that run on a server, with their model,
/// so Down can say whose sessions stop getting answers. Only live sessions are
/// read: their bindings, one each.
pub(crate) fn server_sessions(config: &Config, server: &Server) -> std::result::Result<Value, Failure> {
    let records = live_records()?;
    // model-catalog/2: a session uses a service by any of its API names, and
    // an account whose own gateway is this catalog's counts too.
    if super::model_catalog::is_catalog(server) {
        if let Some((_, catalog)) = super::model_catalog::cached(config, server) {
            return Ok(json!({"server":server.name,"sessions":super::model_catalog::sessions_using(server, &catalog, records.into_iter())}));
        }
    }
    Ok(json!({"server":server.name,"sessions":sessions_using(&server.name, records.into_iter())}))
}

/// This computer's live session records (name, binding), bounded.
pub(super) fn live_records() -> std::result::Result<Vec<(String, Value)>, Failure> {
    let live = super::storage::live().map_err(Failure::Refused)?;
    Ok(live.keys().take(MAX_LIVE_SESSIONS).filter_map(|name| super::storage::read(name).ok().map(|record| (name.clone(), record))).collect())
}

/// The sessions among `records` (name, binding) whose binding names `server`.
fn sessions_using(server: &str, records: impl Iterator<Item = (String, Value)>) -> Vec<Value> {
    records
        .filter(|(_, record)| record["model_server"]["server"].as_str() == Some(server))
        .map(|(name, record)| json!({"name":name,"model":record["model_server"]["model"].as_str().unwrap_or_default()}))
        .collect()
}

/// The live sessions `sessions` reads at most.
const MAX_LIVE_SESSIONS: usize = 2000;

/// `down` asks first: a server may be shared, so it runs only with `--yes`
/// (the receipt says what it would do).
pub(crate) fn down(config: &Config, server: &Server, model: &str, yes: bool) -> std::result::Result<Value, Failure> {
    let model = checked_model(model).map_err(|error| Failure::Refused(error.message))?;
    let catalog = super::model_catalog::is_catalog(server);
    let api = server.api.as_ref().filter(|_| catalog && server.down.is_empty());
    if server.down.is_empty() && api.is_none() {
        return Err(Failure::Refused(format!("{} has no down command; it is read only", server.name)));
    }
    if !yes {
        let mut receipt = json!({"status":"confirm","server":server.name,"model":model,
            "note":format!("Down stops a model on {}; it may be shared. Pass --yes to run the down command.", server.name)});
        if catalog {
            receipt["sessions"] = json!(super::model_catalog::down_sessions(config, server, &model));
        }
        return Ok(receipt);
    }
    if let Some(api) = api {
        return super::model_catalog::api_down(config, server, api, &model);
    }
    let argv = filled(&server.down, &model, &server.name);
    let run = run_template(config, server, &argv, Duration::from_secs(UP_SECS))?;
    Ok(json!({"status":"ran","server":server.name,"model":model,"exit":run.code}))
}

/// `logs` is bounded: at most `lines` lines and a fixed output and time bound.
pub(crate) fn logs(config: &Config, server: &Server, model: Option<&str>, lines: usize) -> std::result::Result<Value, Failure> {
    let model = match model {
        Some(model) => checked_model(model).map_err(|error| Failure::Refused(error.message))?,
        None => String::new(),
    };
    let lines = lines.clamp(1, MAX_LINES);
    let text = if server.logs.is_empty() {
        match server.api.as_ref().filter(|_| super::model_catalog::is_catalog(server) && !model.is_empty()) {
            Some(api) => super::model_catalog::api_logs(config, api, &model, lines)?,
            None => return Err(Failure::Refused(format!("{} has no logs command", server.name))),
        }
    } else {
        let argv = filled(&server.logs, &model, &server.name);
        run_template(config, server, &argv, Duration::from_secs(LOGS_SECS))?.stdout
    };
    let collected: Vec<String> = text.lines().map(str::to_owned).collect();
    let truncated = collected.len() > lines;
    let taken: Vec<String> = collected.into_iter().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect();
    Ok(json!({"status":"ran","server":server.name,"model":model,"lines":taken,"truncated":truncated}))
}

/// The session-scoped provider settings of an agent on a server's model: the
/// non-secret environment, the secret environment (which only `hgs --run`
/// reads, from a private file) and extra launch arguments. Never a global
/// agent configuration. OpenCode has no per-session provider contract yet, so a
/// model server does not offer it; a global provider entry would break the
/// session scope.
#[derive(Debug)]
pub(crate) struct SessionSettings {
    pub env: Vec<String>,
    pub secrets: Vec<String>,
    pub args: Vec<String>,
    /// model-catalog/2: what the catalog resolved for this launch.
    pub resolved: Option<super::model_catalog::Resolved>,
}

/// The provider id a session uses for its server, safe for an agent's config
/// key: `hgs-<server>`, lowercased, letters/digits/- only.
fn provider_id(server: &Server) -> String {
    format!(
        "hgs-{}",
        server
            .name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
    )
}

/// A Codex `-c` inline table value: quoted strings with `"` and `\` escaped.
fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(crate) fn session_settings(config: &Config, server_name: &str, agent: &str, model: &str) -> Result<SessionSettings> {
    let server = load(config)?.into_iter().find(|server| server.name == server_name)
        .ok_or_else(|| Error::new(1, format!("Unknown model server {server_name}")))?;
    let model = checked_model(model)?;
    let key = if server.key.is_empty() { "local".to_owned() } else { server.key.clone() };
    let mut settings = SessionSettings { env: Vec::new(), secrets: Vec::new(), args: Vec::new(), resolved: None };
    // model-catalog/2: the API name of this agent's wire, that wire's base URL
    // and the service's live window come from the catalog (one server row for
    // both agents); without any catalog read the row's own fields apply.
    let resolved = if super::model_catalog::is_catalog(&server) && matches!(agent, "claude" | "codex") {
        super::model_catalog::resolve(config, &server, agent, &model)?
    } else {
        None
    };
    let model = resolved.as_ref().map_or(model, |resolved| resolved.api_model.clone());
    let context = match &resolved {
        Some(resolved) => resolved.context,
        None => server.contexts.get(&model).copied(),
    };
    if let Some(resolved) = &resolved {
        settings.env.push(format!("HGS_MODEL_SERVER_SERVICE={}", resolved.service));
        settings.env.push(format!("HGS_MODEL_SERVER_API_MODEL={}", resolved.api_model));
        if !resolved.variant.is_empty() {
            settings.env.push(format!("HGS_MODEL_SERVER_VARIANT={}", resolved.variant));
        }
        if let Some(tokens) = resolved.context {
            settings.env.push(format!("HGS_MODEL_SERVER_CONTEXT={tokens}"));
        }
    }
    match agent {
        "claude" => {
            if resolved.is_none() && server.kind != "anthropic" {
                return Err(Error::new(1, "Claude Code needs an Anthropic-compatible server"));
            }
            let base = resolved.as_ref().map_or_else(|| anthropic_base(&server), |resolved| resolved.base_url.clone());
            settings.env.push(format!("ANTHROPIC_BASE_URL={base}"));
            for slot in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_SMALL_FAST_MODEL"] {
                settings.env.push(format!("{slot}={model}"));
            }
            settings.env.push("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1".into());
            settings.env.push("API_TIMEOUT_MS=1800000".into());
            // Claude Code assumes 200k for a model it does not know; the
            // server's own window replaces that (CLAUDE_CODE_MAX_CONTEXT_TOKENS
            // applies to an ID without claude- and [1m]).
            if let Some(tokens) = context {
                settings.env.push(format!("CLAUDE_CODE_MAX_CONTEXT_TOKENS={tokens}"));
            }
            // Its gateway discovery keeps only claude/anthropic IDs, so /model
            // would offer this one model: the server's list goes to a settings
            // file of this launch (the global configuration stays as it is).
            if let Some(path) = picker_settings(config, &server, &model) {
                settings.args.extend(["--settings".to_owned(), path.to_string_lossy().into_owned()]);
            }
            settings.secrets.push(format!("ANTHROPIC_AUTH_TOKEN={key}"));
        }
        "codex" => {
            if resolved.is_none() && server.kind != "openai" {
                return Err(Error::new(1, "Codex needs an OpenAI-compatible server"));
            }
            // A session-scoped provider: the built-in openai provider speaks the
            // Responses API and may use the ChatGPT login, so a local
            // chat-completions server gets its own provider with its own key
            // variable. The global Codex configuration is not touched. The wire
            // API follows the installed Codex (#709 L7): chat is deprecated in
            // recent releases and removed from the newest ones, where a session
            // using it would fail at its first turn.
            let provider = provider_id(&server);
            let wire = wire_api_for(installed_codex_version());
            let table = format!(
                "{{name={},base_url={},env_key=\"HGS_MODEL_SERVER_KEY\",wire_api=\"{wire}\"}}",
                toml_string(&server.name),
                toml_string(&resolved.as_ref().map_or_else(|| openai_base(&server), |resolved| resolved.base_url.clone())),
            );
            settings.args.extend([
                "-c".to_owned(),
                format!("model_providers.{provider}={table}"),
                "-c".to_owned(),
                format!("model_provider={provider}"),
                "--model".to_owned(),
                model.clone(),
            ]);
            if let Some(tokens) = context {
                settings.args.extend(["-c".to_owned(), format!("model_context_window={tokens}")]);
            }
            settings.secrets.push(format!("HGS_MODEL_SERVER_KEY={key}"));
        }
        "opencode" => {
            return Err(Error::new(1, "OpenCode is not offered for a model server; start Codex or Claude Code"));
        }
        other => return Err(Error::new(1, format!("A model server session starts claude or codex, not {other}"))),
    }
    settings.resolved = resolved;
    Ok(settings)
}

/// The settings file a Claude launch on `server` passes with `--settings`: its
/// /model picker lists the server's models, the session's own first, and only
/// them (the cloud models answer nothing behind this server). Named by its
/// content, so launches with the same list share one file. None when the list
/// cannot be read within PICKER_SECS: the session still starts on its model.
pub(crate) fn picker_settings(config: &Config, server: &Server, model: &str) -> Option<PathBuf> {
    let started = std::time::Instant::now();
    // model-catalog/2: only anthropic-wire API names, with their live window.
    let catalog_names = super::model_catalog::is_catalog(server).then(|| super::model_catalog::picker_names(config, server)).flatten();
    let windows: std::collections::BTreeMap<String, u64> = catalog_names
        .iter()
        .flatten()
        .filter_map(|(name, tokens)| tokens.map(|tokens| (name.clone(), tokens)))
        .collect();
    let mut names: Vec<String> = match catalog_names {
        Some(names) => names.into_iter().map(|(name, _)| name).filter(|name| checked_model(name).is_ok()).collect(),
        None => api_models_within(config, server, Duration::from_secs(PICKER_SECS))
            .ok()?
            .iter()
            .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
            .filter(|name| checked_model(name).is_ok())
            .collect(),
    };
    if started.elapsed() > Duration::from_secs(PICKER_SECS) {
        return None;
    }
    // The session's model first; an account launch names none.
    if !model.is_empty() {
        names.retain(|name| name != model);
        names.insert(0, model.to_owned());
    }
    if names.is_empty() {
        return None;
    }
    let options: Vec<Value> = names
        .iter()
        .map(|name| {
            let mut row = json!({"model": name, "description": format!("{} model server", server.name)});
            if let Some(tokens) = server.contexts.get(name).or_else(|| windows.get(name)) {
                row["description"] = json!(format!("{} model server, {}k context", server.name, tokens / 1000));
            }
            row
        })
        .collect();
    let text = json!({"modelPicker": {"options": options, "replaceBuiltInOptions": true}}).to_string();
    let digest = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(text.as_bytes());
        hash.iter().take(8).map(|byte| format!("{byte:02x}")).collect::<String>()
    };
    let dir = config.state.join("model-servers");
    platform::private_dir(&dir).ok()?;
    let path = dir.join(format!("picker-{digest}.json"));
    if !path.exists() {
        storage::atomic_private(&path, &text).ok()?;
    }
    Some(path)
}

/// The gateway a Claude account's own settings point Claude Code at
/// (`env.ANTHROPIC_BASE_URL` in `<home>/settings.json`, as the GB10 account
/// has), with its token; None for an account that talks to Anthropic.
pub(crate) fn account_gateway(home: &Path) -> Option<Server> {
    let settings: Value = serde_json::from_slice(&std::fs::read(home.join("settings.json")).ok()?).ok()?;
    let env = &settings["env"];
    let base_url = env["ANTHROPIC_BASE_URL"].as_str().filter(|url| !url.is_empty())?;
    let key = env["ANTHROPIC_AUTH_TOKEN"].as_str().or_else(|| env["ANTHROPIC_API_KEY"].as_str()).unwrap_or_default();
    checked(Server { name: "gateway".into(), base_url: base_url.into(), kind: "anthropic".into(), key: key.into(), ..Default::default() }).ok()
}

/// `--settings <picker>` for a Claude session record behind a gateway (its
/// model server, else its account's own gateway), for a resume or a fresh
/// start of a session that began before its launch carried one. Empty for any
/// other session, or when the list cannot be read.
pub(crate) fn resume_picker_args(record: &Value) -> Vec<String> {
    if record["agent"].as_str() != Some("claude") {
        return Vec::new();
    }
    let Ok(config) = crate::config::Config::load() else { return Vec::new() };
    let model = record["model_server"]["model"].as_str().unwrap_or_default();
    let server = match record["model_server"]["server"].as_str().filter(|name| !name.is_empty()) {
        Some(name) => load(&config).ok().and_then(|servers| servers.into_iter().find(|server| server.name == name)),
        None => record["agent_home"].as_str().filter(|home| !home.is_empty()).and_then(|home| account_gateway(Path::new(home))),
    };
    server
        .and_then(|server| picker_settings(&config, &server, model))
        .map(|path| vec!["--settings".to_owned(), path.to_string_lossy().into_owned()])
        .unwrap_or_default()
}

/// The model ids `server` lists, within `timeout`; empty when it cannot be read.
pub(crate) fn model_names(config: &Config, server: &Server, timeout: Duration) -> Vec<String> {
    api_models_within(config, server, timeout)
        .map(|models| models.iter().filter_map(|model| model["name"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// `api_models` with its own time bound, for a launch that must not wait long.
fn api_models_within(config: &Config, server: &Server, timeout: Duration) -> std::result::Result<Vec<Value>, Failure> {
    let url = models_url(server);
    let (code, body) = if url.starts_with("https://") {
        curl_get(config, server, &url)?
    } else {
        http_get(&url, server, timeout.min(http_timeout()))?
    };
    if !(200..300).contains(&code) {
        return Err(http_failure(code, &body));
    }
    let value: Value = serde_json::from_str(&body).map_err(|_| Failure::Refused("The model list is not JSON".into()))?;
    let data = value["data"].as_array().or_else(|| value["models"].as_array()).ok_or_else(|| Failure::Refused("The model list has no data array".into()))?;
    Ok(data
        .iter()
        .take(MAX_MODELS)
        .filter_map(|model| model["id"].as_str().or_else(|| model["name"].as_str()))
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
        .map(|name| json!({"name": name}))
        .collect())
}

/// The installed Codex's version from `codex --version` (warnings may come
/// first on stderr; the version line is on stdout). None when Codex is absent.
fn installed_codex_version() -> Option<(u64, u64, u64)> {
    let mut command = Command::new("codex");
    command.arg("--version");
    let out = platform::output_until(&mut command, Duration::from_secs(10)).ok()?;
    parse_version(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// The first `MAJOR.MINOR.PATCH` in `text`; a line naming `codex-cli` wins over
/// a version-looking number in a warning.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let version = |line: &str| {
        line.split(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .filter(|part| part.contains('.'))
            .find_map(|part| {
                let mut numbers = part.split('.').filter(|number| !number.is_empty());
                let major = numbers.next()?.parse().ok()?;
                let minor = numbers.next()?.parse().ok()?;
                let patch = numbers.next().unwrap_or("0").parse().ok()?;
                Some((major, minor, patch))
            })
    };
    text.lines()
        .filter(|line| line.to_ascii_lowercase().contains("codex-cli"))
        .find_map(version)
        .or_else(|| text.lines().find_map(version))
}

/// The version from which Codex deprecates `chat` in favour of `responses`.
const RESPONSES_FROM: (u64, u64, u64) = (0, 100, 0);

/// Codex's wire API for a version: `chat` before `RESPONSES_FROM`, `responses`
/// from then on. An unknown Codex gets `responses`: that is the current
/// default, and the newest releases no longer serve `chat` at all (#709 L7).
fn wire_api_for(version: Option<(u64, u64, u64)>) -> &'static str {
    match version {
        Some(version) if version < RESPONSES_FROM => "chat",
        _ => "responses",
    }
}

/// The settings a resume needs for a saved session: its non-secret environment
/// and its secret lines, which are written to a private file only right before
/// the backend launch (#709 L1). A server that is no longer configured is an
/// error that names it (#658 N4).
pub(crate) fn resume_environment(config: &Config, server: &str, agent: &str, model: &str) -> Result<(Vec<String>, Vec<String>)> {
    let settings = session_settings(config, server, agent, model)?;
    Ok((settings.env, settings.secrets))
}

/// How long an unread secret file may stay before a sweep removes it: a launch
/// whose `hgs --run` never read it (a failed start, a crash) leaves one behind.
const SECRET_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Removes the session secret files a launch left unread (older than
/// `SECRET_MAX_AGE`), so a failed start cannot pile them up (#709 L1).
pub(crate) fn sweep_secret_files(state_dir: &Path) {
    let dir = state_dir.join("model-servers");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("session-") || !(name.ends_with(".env") || name.contains(".env.")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > SECRET_MAX_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Writes the session's secret environment to a private file and returns its
/// path: `hgs --run` reads it there, applies it to the agent and removes it, so
/// the key never appears on a command line, in the tmux start command or in
/// `--dry-run`.
pub(crate) fn write_secret_file(state_dir: &Path, secrets: &[String]) -> Result<PathBuf> {
    sweep_secret_files(state_dir);
    let dir = state_dir.join("model-servers");
    platform::private_dir(&dir).map_err(|error| Error::new(1, error.to_string()))?;
    let path = dir.join(format!("session-{}.env", uuid::Uuid::new_v4()));
    storage::atomic_private(&path, &secrets.join("\n")).map_err(|error| Error::new(1, error.to_string()))?;
    Ok(path)
}

/// `hgs --run`: takes the private secret file the launch left, applies it to
/// this process (the agent inherits it), and removes the file. No file means a
/// session without a model server.
pub(crate) fn apply_session_secrets() -> Result<()> {
    let Some(path) = std::env::var_os("HGS_MODEL_SERVER_ENV").filter(|value| !value.is_empty()) else { return Ok(()) };
    std::env::remove_var("HGS_MODEL_SERVER_ENV");
    let path = PathBuf::from(path);
    let text = std::fs::read_to_string(&path).map_err(|error| Error::new(1, format!("Cannot read the session's model server settings: {error}")))?;
    let _ = std::fs::remove_file(&path);
    for line in text.lines().filter(|line| !line.is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

/// `hgs model-server list|status|up|up-result|sessions|down|logs|save|test`.
pub(crate) fn dispatch(config: &Config, args: &[String]) -> Result<i32> {
    let command = args.first().map(String::as_str).unwrap_or("list");
    let servers = load(config)?;
    match command {
        "" | "list" => {
            let rows: Vec<Value> = servers.iter().map(public).collect();
            if args.iter().any(|arg| arg == "--json") {
                println!("{}", json!({"servers": rows}));
            } else if rows.is_empty() {
                println!("No model servers on this computer.");
            } else {
                for row in rows {
                    let control = ["status", "up", "down", "logs"].into_iter().filter(|key| row["command"][*key] == true).collect::<Vec<_>>().join(",");
                    println!("{}\t{}\t{}{}{}", row["name"].as_str().unwrap_or_default(), row["base_url"].as_str().unwrap_or_default(), row["kind"].as_str().unwrap_or_default(),
                        if row["ssh"].as_str().unwrap_or_default().is_empty() { String::new() } else { format!(" via {}", row["ssh"].as_str().unwrap_or_default()) },
                        if control.is_empty() { String::new() } else { format!(" [{control}]") });
                }
            }
            Ok(0)
        }
        "up-result" => {
            let id = args.get(1).filter(|arg| !arg.starts_with('-')).ok_or_else(|| Error::new(2, "hgs model-server up-result REQUEST_ID --json"))?;
            match up_result(id) {
                Ok(value) => { println!("{value}"); Ok(0) }
                Err(failure) => fail(&failure),
            }
        }
        "variant" => {
            let usage = || Error::new(2, "hgs model-server variant SERVER MODEL VARIANT [--restart] [--yes] [--detach]");
            let positional: Vec<&String> = args.iter().skip(1).filter(|arg| !arg.starts_with('-')).collect();
            let [name, model, variant] = positional.as_slice() else { return Err(usage()) };
            let server = servers.iter().find(|server| &server.name == *name).ok_or_else(|| Error::new(1, format!("Unknown model server {name}")))?;
            let has = |flag: &str| args.iter().any(|arg| arg == flag);
            match super::model_catalog::variant(config, server, model, variant, has("--restart"), has("--yes"), has("--detach")) {
                Ok(value) => { println!("{value}"); Ok(0) }
                Err(failure) => fail(&failure),
            }
        }
        "status" | "up" | "down" | "logs" | "sessions" => {
            let name = args.get(1).filter(|arg| !arg.starts_with('-')).ok_or_else(|| Error::new(2, format!("hgs model-server {command} SERVER {}", if command == "status" { "" } else { "MODEL" })))?;
            let server = servers.iter().find(|server| &server.name == name).ok_or_else(|| Error::new(1, format!("Unknown model server {name}")))?;
            let json_out = args.iter().any(|arg| arg == "--json");
            let flag = |name: &str| args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).map(String::as_str);
            match command {
                "sessions" => match server_sessions(config, server) {
                    Ok(value) => { println!("{value}"); Ok(0) }
                    Err(failure) => fail(&failure),
                },
                "status" => match status(config, server) {
                    Ok(value) => {
                        if json_out { println!("{value}"); } else { print_status(&value); }
                        Ok(0)
                    }
                    Err(failure) => fail(&failure),
                },
                "up" => {
                    let model = args.get(2).filter(|arg| !arg.starts_with('-')).ok_or_else(|| Error::new(2, "hgs model-server up SERVER MODEL"))?;
                    match up(config, server, model, args.iter().any(|arg| arg == "--detach")) {
                        Ok(value) => { println!("{value}"); Ok(0) }
                        Err(failure) => fail(&failure),
                    }
                }
                "down" => {
                    let model = args.get(2).filter(|arg| !arg.starts_with('-')).ok_or_else(|| Error::new(2, "hgs model-server down SERVER MODEL [--yes]"))?;
                    let yes = args.iter().any(|arg| arg == "--yes");
                    match down(config, server, model, yes) {
                        Ok(value) => { println!("{value}"); Ok(0) }
                        Err(failure) => fail(&failure),
                    }
                }
                _ => {
                    let model = args.get(2).filter(|arg| !arg.starts_with('-'));
                    let lines = flag("--lines").and_then(|lines| lines.parse::<usize>().ok()).unwrap_or(200);
                    match logs(config, server, model.map(String::as_str), lines) {
                        Ok(value) => { println!("{value}"); Ok(0) }
                        Err(failure) => fail(&failure),
                    }
                }
            }
        }
        "save" | "test" => {
            let input = super::stdin_json().map_err(|error| Error::new(1, error))?;
            if command == "save" {
                println!("{}", save_request(config, &input)?);
                return Ok(0);
            }
            match test_request(config, &input)? {
                Ok(value) => {
                    println!("{value}");
                    Ok(0)
                }
                Err(failure) => fail(&failure),
            }
        }
        "set" => {
            let _ = servers;
            Err(Error::new(1, "The model servers are configured in Settings > Model servers; the file is model-servers.json in the private config folder"))
        }
        other => Err(Error::new(2, format!("Unknown model-server command {other}; use list, status, up, up-result, sessions, down, variant, logs, save or test"))),
    }
}

fn fail(failure: &Failure) -> Result<i32> {
    println!("{}", failure.value());
    Ok(1)
}

fn print_status(value: &Value) {
    for model in value["models"].as_array().into_iter().flatten() {
        println!("{}: {}", model["name"].as_str().unwrap_or_default(), model["state"].as_str().unwrap_or_default());
    }
    if value["memory"].is_object() {
        println!("memory: {}/{}", value["memory"]["free"], value["memory"]["total"]);
    }
    for line in value["raw"].as_array().into_iter().flatten() {
        println!("{}", line.as_str().unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn config(dir: &std::path::Path) -> Config {
        Config {
            home: dir.display().to_string(),
            dir: dir.to_path_buf(),
            state: dir.join("state"),
            selves: Vec::new(),
            peers: Vec::new(),
            base_peers: Vec::new(),
            peers_from_environment: false,
            machines: std::collections::BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        }
    }

    fn server() -> Server {
        checked(Server {
            name: "lab".into(),
            base_url: "http://127.0.0.1:8080".into(),
            kind: "openai".into(),
            key: "sk-test".into(),
            ssh: String::new(),
            ..Default::default()
        })
        .unwrap()
    }

    /// U60 (#656): Settings saves the whole list through `save --json`: a
    /// server without `key` keeps the one stored under its previous name,
    /// `null` removes it, errors name the server and field, nothing returns a
    /// key, and only you save it.
    #[test]
    fn settings_save_keeps_or_removes_the_key_and_never_returns_it() {
        let dir = tempfile::tempdir().unwrap();
        // The test process may run inside an agent session; save is the owner's.
        crate::state::with_test_state_root(dir.path(), || {
            let config = config(dir.path());
            save(&config, &[server()]).unwrap();
            let human = |servers: Value| json!({"by": "human", "servers": servers});
            // Renamed, its key kept through `was`; a second one added without a key.
            let saved = save_request(&config, &human(json!([
                {"was": "lab", "name": "lab-a", "base_url": "http://127.0.0.1:8080", "kind": "openai",
                 "status": ["serve", "status"], "down": ["serve", "down", "{model}"]},
                {"name": "gate", "base_url": "https://gate.example/v1", "kind": "anthropic"}
            ]))).unwrap();
            assert_eq!(saved["status"], "saved");
            assert!(!saved.to_string().contains("sk-test"), "{saved}");
            assert_eq!(saved["servers"][0]["key"], true);
            assert_eq!(saved["servers"][0]["argv"]["down"], json!(["serve", "down", "{model}"]));
            let found = load(&config).unwrap();
            assert_eq!((found[0].name.as_str(), found[0].key.as_str()), ("lab-a", "sk-test"));
            assert_eq!((found[1].name.as_str(), found[1].key.as_str()), ("gate", ""));
            // A new key replaces it; null removes it.
            save_request(&config, &human(json!([{"was": "lab-a", "name": "lab-a", "base_url": "http://127.0.0.1:8080", "kind": "openai", "key": "sk-new"}]))).unwrap();
            assert_eq!(load(&config).unwrap()[0].key, "sk-new");
            save_request(&config, &human(json!([{"was": "lab-a", "name": "lab-a", "base_url": "http://127.0.0.1:8080", "kind": "openai", "key": null}]))).unwrap();
            assert_eq!(load(&config).unwrap()[0].key, "");
            // Errors name the server and the field; nothing is written.
            let refused = |servers: Value| save_request(&config, &human(servers)).unwrap_err().message;
            let error = refused(json!([{"name": "bad", "base_url": "ftp://x", "kind": "openai"}]));
            assert!(error.starts_with("Server bad: The API URL"), "{error}");
            let error = refused(json!([{"name": "bad", "base_url": "http://x", "kind": "openai", "colour": "red"}]));
            assert!(error.starts_with("Server bad: unknown field `colour`"), "{error}");
            let error = refused(json!([{"name": "x", "base_url": "http://x", "kind": "openai"}, {"name": "x", "base_url": "http://y", "kind": "openai"}]));
            assert!(error.contains("Server x: name is already used"), "{error}");
            let error = refused(json!([{"name": "x", "base_url": "http://x", "kind": "openai", "up": ["serve", "up"]}]));
            assert!(error.starts_with("Server x: The up command"), "{error}");
            assert_eq!(load(&config).unwrap().len(), 1);
            // Only you: no "by":"human", nothing saved.
            let error = save_request(&config, &json!({"servers": []})).unwrap_err().message;
            assert!(error.contains("by"), "{error}");
            assert_eq!(load(&config).unwrap().len(), 1);
        });
    }

    /// U60 (#658 L2): `test --json` reads status once for an unsaved server;
    /// the stored key of `was` is used only while the root stays the same, so a
    /// test can never send it to another listener, and nothing is written.
    #[test]
    fn settings_test_never_reuses_the_key_for_another_root() {
        fn answer(port: u16) -> (std::sync::Arc<std::sync::Mutex<String>>, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let heard = seen.clone();
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                *heard.lock().unwrap() = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let body = r#"{"data":[{"id":"small"},{"id":"large"}]}"#;
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            (seen, handle)
        }
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        // A stored server on a root of its own, with its key.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let root = listener.local_addr().unwrap().port();
        drop(listener);
        let mut stored = server();
        stored.base_url = format!("http://127.0.0.1:{root}");
        save(&config, &[stored.clone()]).unwrap();
        let before = std::fs::read(path(&config)).unwrap();
        // The same root keeps the stored key.
        let (seen, handle) = answer(root);
        let value = test_request(&config, &json!({"was": "lab", "name": "lab-edit", "base_url": format!("http://127.0.0.1:{root}"), "kind": "openai"}))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        assert_eq!(value["source"], "api");
        assert_eq!(value["models"].as_array().unwrap().len(), 2);
        assert!(seen.lock().unwrap().contains("Bearer sk-test"), "the stored key of `was` is used for the same root");
        // Another root gets no key at all.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let other_root = listener.local_addr().unwrap().port();
        drop(listener);
        let (seen, handle) = answer(other_root);
        let value = test_request(&config, &json!({"was": "lab", "name": "lab-edit", "base_url": format!("http://127.0.0.1:{other_root}"), "kind": "openai"}))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        assert_eq!(value["source"], "api");
        assert!(!seen.lock().unwrap().to_ascii_lowercase().contains("authorization"), "another root never sees the stored key");
        assert_eq!(std::fs::read(path(&config)).unwrap(), before, "a test writes nothing");
        // A refused field is an error, not a status.
        assert!(test_request(&config, &json!({"name": "x", "base_url": "nope", "kind": "openai"})).unwrap_err().message.starts_with("Server x:"));
    }

    #[test]
    fn servers_come_from_the_private_file_and_list_hides_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        assert!(load(&config).unwrap().is_empty(), "a missing configuration lists none");
        save(&config, &[server()]).unwrap();
        let found = load(&config).unwrap();
        assert_eq!(found, [server()]);
        let row = public(&found[0]);
        assert_eq!(row["name"], "lab");
        assert_eq!(row["key"], true, "list says a key is set");
        assert!(!row.to_string().contains("sk-test"), "the key is never shown: {row}");
        #[cfg(windows)]
        assert!(platform::verify_private(&path(&config), platform::Private::File).unwrap());
        // https is accepted and saved: many LAN gateways serve TLS only.
        let mut https = server();
        https.base_url = "https://192.0.2.10:8443/api".into();
        assert_eq!(checked(https.clone()).unwrap(), https);
        save(&config, &[https.clone()]).unwrap();
        assert_eq!(load(&config).unwrap(), [https]);
        save(&config, &[server()]).unwrap();
        // Invalid fields are refused before anything is written.
        let broken = |change: &dyn Fn(&mut Server)| {
            let mut server = server();
            change(&mut server);
            checked(server).unwrap_err().to_string()
        };
        assert!(broken(&|s| s.name = "a b".into()).contains("server name"));
        assert!(broken(&|s| s.base_url = "ftp://lab".into()).contains("http://"));
        assert!(broken(&|s| s.base_url = "https://user:key@lab".into()).contains("http://"));
        assert!(broken(&|s| s.kind = "grpc".into()).contains("openai or anthropic"));
        assert!(broken(&|s| s.ssh = "-oProxyCommand".into()).contains("SSH host"));
        assert!(broken(&|s| s.up = vec!["serve".into(), "up".into()]).contains("{model}"));
        assert!(broken(&|s| s.up = vec!["serve".into(), "{host}".into()]).contains("placeholder"));
    }

    /// The rejected gap: https servers are read through the system curl. The key
    /// travels in the private `--config` file, never on the command line; curl's
    /// exit codes keep the four failures distinct, and a missing curl is refused
    /// by name (sessions still work because the agents speak TLS themselves).
    #[test]
    fn an_https_server_is_read_with_curl_and_the_key_stays_off_the_argv() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut server = server();
        server.base_url = "https://192.0.2.10:8443".into();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(Vec<String>, Option<String>)>::new()));
        let record = seen.clone();
        set_fake_curl(Some(Box::new(move |argv: &[String], header: Option<&str>| {
            let at = argv.iter().position(|arg| arg == "--config").expect("a key needs --config");
            let path = std::path::Path::new(&argv[at + 1]);
            assert!(path.is_file(), "the private header file exists while curl runs: {path:?}");
            assert_eq!(std::fs::read_to_string(path).unwrap(), header.unwrap(), "curl reads the key from the private file");
            record.borrow_mut().push((argv.to_vec(), header.map(str::to_owned)));
            CurlRun::Ran(Run { code: Some(0), stdout: "{\"data\":[{\"id\":\"small\"}]}\n200".into(), stderr: String::new() })
        })));
        let value = status(&config, &server).unwrap();
        assert_eq!(value["source"], "api");
        assert_eq!(value["models"], json!([{"name":"small","state":"available"}]));
        let (argv, header) = seen.borrow()[0].clone();
        let line = argv.join(" ");
        assert!(line.contains("--fail-with-body") && line.contains("--max-time"), "{argv:?}");
        assert!(argv.last().unwrap().ends_with("/v1/models"), "{argv:?}");
        assert!(!line.contains("sk-test"), "the key never reaches the argv: {argv:?}");
        assert!(header.as_deref().unwrap().contains("Authorization: Bearer sk-test"), "{header:?}");
        // 401 is unauthorized; a timeout, a connect failure and a missing curl
        // are their own errors.
        set_fake_curl(Some(Box::new(|_: &[String], _: Option<&str>| {
            CurlRun::Ran(Run { code: Some(22), stdout: "{\"error\":\"no key\"}\n401".into(), stderr: String::new() })
        })));
        assert_eq!(status(&config, &server).unwrap_err().kind(), "unauthorized");
        set_fake_curl(Some(Box::new(|_: &[String], _: Option<&str>| CurlRun::TimedOut)));
        assert_eq!(status(&config, &server).unwrap_err().kind(), "timed_out");
        set_fake_curl(Some(Box::new(|_: &[String], _: Option<&str>| {
            CurlRun::Ran(Run { code: Some(7), stdout: String::new(), stderr: "Failed to connect".into() })
        })));
        assert_eq!(status(&config, &server).unwrap_err().kind(), "unreachable");
        set_fake_curl(Some(Box::new(|_: &[String], _: Option<&str>| CurlRun::Missing)));
        let missing = status(&config, &server).unwrap_err();
        assert_eq!(missing.kind(), "refused");
        assert!(missing.detail().contains("curl was not found"), "{missing:?}");
        // A server without a key passes no --config.
        set_fake_curl(Some(Box::new(|argv: &[String], header: Option<&str>| {
            assert!(header.is_none() && !argv.iter().any(|arg| arg == "--config"), "{argv:?}");
            CurlRun::Ran(Run { code: Some(0), stdout: "{\"data\":[]}\n200".into(), stderr: String::new() })
        })));
        let mut open = server.clone();
        open.key.clear();
        assert_eq!(status(&config, &open).unwrap()["models"], json!([]));
        set_fake_curl(None);
    }

    #[test]
    fn a_status_command_runs_with_argv_and_reads_the_contract_or_the_lines() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.ssh = "labhost".into();
        server.status = vec!["serve".into(), "status".into()];
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let record = seen.clone();
        set_fake_command(Some(Box::new(move |server: &Server, argv: &[String], _| {
            record.borrow_mut().push((server.name.clone(), server.ssh.clone(), argv.to_vec()));
            Ok(Run { code: Some(0), stdout: r#"{"models":[{"name":"small","state":"RUNNING"}],"memory":{"free":12.5,"total":64}}"#.into(), stderr: String::new() })
        })));
        let value = status(&config, &server).unwrap();
        assert_eq!(value["source"], "command");
        assert_eq!(value["models"], json!([{"name":"small","state":"running"}]));
        assert_eq!(value["memory"], json!({"free":12.5,"total":64.0}));
        assert_eq!(*seen.borrow(), [("lab".to_owned(), "labhost".to_owned(), vec!["serve".to_owned(), "status".to_owned()])]);
        // The command reaches the server over hgs's SSH transport: BatchMode,
        // the host, and the argv as one quoted remote command.
        let ssh = ssh_argv(&config, &server, &["serve".to_owned(), "status".to_owned()]);
        assert!(ssh.iter().any(|arg| arg == "BatchMode=yes"), "{ssh:?}");
        assert!(ssh.iter().any(|arg| arg == "-n"), "the client reads no stdin: {ssh:?}");
        assert!(ssh.contains(&"labhost".to_owned()), "{ssh:?}");
        assert_eq!(ssh.last().unwrap(), "serve status", "{ssh:?}");
        // The line format, an unknown line kept raw, a memory line, and a model
        // name with tags (N7: split at the last ": ").
        set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| {
            Ok(Run { code: Some(0), stdout: "small: ready\ncoder-a:q4: running\nqueue depth 3\nmemory: 8/32 GB\n".into(), stderr: String::new() })
        })));
        let value = status(&config, &server).unwrap();
        assert_eq!(value["models"], json!([{"name":"small","state":"ready"},{"name":"coder-a:q4","state":"running"}]));
        assert_eq!(value["memory"], json!({"free":8.0,"total":32.0}));
        assert_eq!(value["raw"], json!(["queue depth 3"]));
        // A Russian memory line reads the same.
        set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| {
            Ok(Run { code: Some(0), stdout: "small: выключена\nСвободно памяти: 43.6 ГиБ из 121.6\n".into(), stderr: String::new() })
        })));
        let value = status(&config, &server).unwrap();
        assert_eq!(value["memory"], json!({"free":43.6,"total":121.6}));
        assert_eq!(value["models"], json!([{"name":"small","state":"выключена"}]));
        // A refusal and an unreachable host are distinct.
        set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| Err(Failure::Refused("no such model".into())))));
        assert_eq!(status(&config, &server).unwrap_err().kind(), "refused");
        set_fake_command(Some(Box::new(|_: &Server, _: &[String], _| Err(Failure::Unreachable("ssh: connect refused".into())))));
        assert_eq!(status(&config, &server).unwrap_err().kind(), "unreachable");
        set_fake_command(None);
    }

    #[test]
    fn a_read_only_server_lists_its_models_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"small\"},{\"id\":\"big\"}]}").unwrap();
        });
        let mut server = server();
        server.base_url = format!("http://127.0.0.1:{port}");
        let value = status(&config, &server).unwrap();
        handle.join().unwrap();
        assert_eq!(value["source"], "api");
        assert_eq!(value["models"], json!([{"name":"small","state":"available"},{"name":"big","state":"available"}]));
    }

    #[test]
    fn http_failures_are_distinct_and_never_retried() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        // Unauthorized.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").unwrap();
        });
        let mut server = server();
        server.base_url = format!("http://127.0.0.1:{port}");
        assert_eq!(status(&config, &server).unwrap_err().kind(), "unauthorized");
        handle.join().unwrap();
        // Refused (HTTP 500).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\n\r\nboom").unwrap();
        });
        server.base_url = format!("http://127.0.0.1:{port}");
        assert_eq!(status(&config, &server).unwrap_err().kind(), "refused");
        handle.join().unwrap();
        // Timed out: the port is open but never answers.
        set_http_timeout(Some(Duration::from_millis(300)));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(2));
        });
        server.base_url = format!("http://127.0.0.1:{port}");
        let failure = status(&config, &server).unwrap_err();
        assert_eq!(failure.kind(), "timed_out");
        handle.join().unwrap();
        set_http_timeout(None);
        // Unreachable: a port nothing listens on.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        server.base_url = format!("http://127.0.0.1:{port}");
        assert_eq!(status(&config, &server).unwrap_err().kind(), "unreachable");
    }

    #[test]
    fn up_down_and_logs_are_bounded_and_ask_first() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.up = vec!["serve".into(), "up".into(), "--name".into(), "{model}".into()];
        server.down = vec!["serve".into(), "down".into(), "{model}".into()];
        server.logs = vec!["serve".into(), "logs".into(), "{model}".into()];
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Vec<String>>::new()));
        let record = seen.clone();
        set_fake_command(Some(Box::new(move |_: &Server, argv: &[String], _| {
            record.borrow_mut().push(argv.to_vec());
            Ok(Run { code: Some(0), stdout: (0..100).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n"), stderr: String::new() })
        })));
        // A model name with shell syntax never reaches the command.
        assert!(up(&config, &server, "small; rm -rf /", false).unwrap_err().kind() == "refused");
        let started = up(&config, &server, "small", false).unwrap();
        assert_eq!((started["status"].as_str(), started["model"].as_str()), (Some("ran"), Some("small")));
        assert_eq!(seen.borrow()[0], ["serve", "up", "--name", "small"]);
        // down asks first and says so, and only --yes runs it.
        let asked = down(&config, &server, "small", false).unwrap();
        assert_eq!(asked["status"], "confirm");
        assert!(asked["note"].as_str().unwrap().contains("shared"));
        assert_eq!(seen.borrow().len(), 1, "nothing ran on the question");
        let ran = down(&config, &server, "small", true).unwrap();
        assert_eq!((ran["status"].as_str(), seen.borrow().len()), (Some("ran"), 2));
        // logs keeps the last lines only, and says it was truncated.
        let logs_value = logs(&config, &server, Some("small"), 3).unwrap();
        assert_eq!(logs_value["lines"], json!(["line 97", "line 98", "line 99"]));
        assert_eq!(logs_value["truncated"], true);
        set_fake_command(None);
    }

    #[test]
    fn a_detached_up_runs_as_an_hgs_run_with_the_time_limit() {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            let config = config(&dir.path().join("config"));
            std::fs::create_dir_all(&config.dir).unwrap();
            let mut server = server();
            server.up = vec!["serve".into(), "up".into(), "{model}".into()];
            server.up_minutes = 7;
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(true));
            let started = up(&config, &server, "small", true).unwrap();
            assert_eq!(started["status"], "started");
            assert_eq!(started["timeout_minutes"], 7);
            let id = started["request_id"].as_str().unwrap();
            assert!(uuid::Uuid::parse_str(id).is_ok(), "{id}");
            let record = crate::state::builds::record(id, crate::state::now()).unwrap();
            assert_eq!(record["label"], "up lab small");
            assert_eq!(record["timeout_secs"], 7 * 60);
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(false));
        });
    }

    #[test]
    fn a_detached_ups_outcome_is_read_by_its_request_id_and_only_its_own() {
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            let config = config(&dir.path().join("config"));
            std::fs::create_dir_all(&config.dir).unwrap();
            let mut server = server();
            #[cfg(windows)]
            let failing = vec!["cmd.exe".to_owned(), "/C".to_owned(), "echo loading {model} failed& exit 3".to_owned()];
            #[cfg(not(windows))]
            let failing = vec!["sh".to_owned(), "-c".to_owned(), "echo loading {model} failed; exit 3".to_owned()];
            server.up = failing;
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(true));
            let started = up(&config, &server, "small", true).unwrap();
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(false));
            let id = started["request_id"].as_str().unwrap();
            // The run's real outcome, not only that it started.
            let done = up_result(id).unwrap();
            assert_eq!((done["status"].as_str(), done["exit"].as_i64()), (Some("ran"), Some(3)), "{done}");
            assert_eq!((done["server"].as_str(), done["model"].as_str()), (Some("lab"), Some("small")));
            assert!(done["tail"].as_str().unwrap().contains("loading small failed"), "{done}");
            assert_eq!(done["timed_out"], false);
            // An unknown id, a malformed one and another kind of run are refused alike.
            for other in ["55555555-5555-4555-8555-555555555555", "../builds", ""] {
                assert!(matches!(up_result(other), Err(Failure::Refused(_))), "{other}");
            }
            let build = "66666666-6666-4666-8666-666666666666";
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(true));
            crate::state::builds::start_for(build, "cargo test", "exit 0", dir.path(), json!({"name":"","run_id":"","conversation_id":""}), 1, &[]).unwrap();
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(false));
            assert!(crate::state::builds::record(build, crate::state::now()).is_some());
            assert!(matches!(up_result(build), Err(Failure::Refused(_))), "another run's output stays its own");
        });
    }

    #[test]
    fn a_status_keeps_a_models_since_when_it_is_a_time() {
        let parsed = parse_status(r#"{"models":[{"name":"a","state":"running","since":1761800000},
            {"name":"b","state":"running","since":"2026-10-30T09:12:00+04:00"},
            {"name":"c","state":"loading","since":"after lunch"},{"name":"d","state":"stopped","since":-5}]}"#);
        assert_eq!(json!(parsed.models), json!([
            {"name":"a","state":"running","since":1761800000.0},
            {"name":"b","state":"running","since":1793337120.0},
            {"name":"c","state":"loading"},{"name":"d","state":"stopped"}]));
    }

    #[test]
    fn sessions_on_a_server_are_named_with_their_model() {
        let records = vec![
            ("codex/orbit/a".to_owned(), json!({"agent":"codex","model_server":{"server":"lab","model":"coder-a"}})),
            ("claude/orbit/b".to_owned(), json!({"agent":"claude"})),
            ("codex/orbit/c".to_owned(), json!({"agent":"codex","model_server":{"server":"other","model":"coder-a"}})),
            ("codex/orbit/d".to_owned(), json!({"agent":"codex","model_server":{"server":"lab","model":"chat-general"}})),
        ];
        assert_eq!(json!(sessions_using("lab", records.into_iter())), json!([
            {"name":"codex/orbit/a","model":"coder-a"},{"name":"codex/orbit/d","model":"chat-general"}]));
    }

    #[test]
    fn both_base_url_spellings_derive_the_same_urls() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.base_url = "http://127.0.0.1:8080/v1".into();
        let with_v1 = checked(server.clone()).unwrap();
        assert_eq!(with_v1.base_url, "http://127.0.0.1:8080", "the root is stored, not /v1");
        assert_eq!(models_url(&with_v1), "http://127.0.0.1:8080/v1/models");
        assert_eq!(openai_base(&with_v1), "http://127.0.0.1:8080/v1");
        assert_eq!(anthropic_base(&with_v1), "http://127.0.0.1:8080", "Claude Code appends /v1/messages");
        save(&config, &[with_v1]).unwrap();
        // A root spelling derives the same URLs and the same session settings.
        let mut root = server;
        root.base_url = "http://127.0.0.1:8080".into();
        let root = checked(root).unwrap();
        assert_eq!(models_url(&root), models_url(&load(&config).unwrap()[0]));
        let codex = session_settings(&config, "lab", "codex", "small").unwrap();
        assert!(codex.args[1].contains("base_url=\"http://127.0.0.1:8080/v1\""), "{:?}", codex.args);
        assert!(!codex.args[1].contains("/v1/v1"), "{:?}", codex.args);
        let mut anthropic = load(&config).unwrap()[0].clone();
        anthropic.kind = "anthropic".into();
        save(&config, &[anthropic]).unwrap();
        let claude = session_settings(&config, "lab", "claude", "small").unwrap();
        assert!(claude.env.iter().any(|line| line == "ANTHROPIC_BASE_URL=http://127.0.0.1:8080"), "{:?}", claude.env);
    }

    /// The owner, 2026-11-02: a Claude session on GB10 offered one model in
    /// /model and assumed a 200k window. The server's models fill the picker of
    /// that launch, and a model's configured window reaches the agent.
    #[test]
    fn a_claude_launch_lists_the_servers_models_and_each_agent_gets_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = r#"{"data":[{"id":"coder"},{"id":"small"},{"id":"-rf"}]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let mut server = server();
        server.kind = "anthropic".into();
        server.base_url = format!("http://127.0.0.1:{port}");
        server.contexts.insert("small".into(), 131072);
        save(&config, &[checked(server.clone()).unwrap()]).unwrap();
        let claude = session_settings(&config, "lab", "claude", "small").unwrap();
        handle.join().unwrap();
        assert!(claude.env.contains(&"CLAUDE_CODE_MAX_CONTEXT_TOKENS=131072".to_owned()), "{:?}", claude.env);
        assert_eq!(claude.args[0], "--settings", "{:?}", claude.args);
        let picker: Value = serde_json::from_str(&std::fs::read_to_string(&claude.args[1]).unwrap()).unwrap();
        let models: Vec<&str> = picker["modelPicker"]["options"].as_array().unwrap().iter().map(|row| row["model"].as_str().unwrap()).collect();
        assert_eq!(models, ["small", "coder"], "the session's model first; a dash name is never offered");
        assert_eq!(picker["modelPicker"]["replaceBuiltInOptions"], true);
        // Codex takes the window as its own setting; a model without one sets nothing.
        let mut codex_server = server.clone();
        codex_server.kind = "openai".into();
        save(&config, &[checked(codex_server).unwrap()]).unwrap();
        let codex = session_settings(&config, "lab", "codex", "small").unwrap();
        assert!(codex.args.windows(2).any(|pair| pair == ["-c", "model_context_window=131072"]), "{:?}", codex.args);
        assert!(!session_settings(&config, "lab", "codex", "coder").unwrap().args.iter().any(|arg| arg.contains("model_context_window")));
        // An unreachable list leaves only the picker out: the session still starts.
        set_http_timeout(Some(Duration::from_millis(300)));
        let mut gone = server.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        gone.base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        drop(listener);
        save(&config, &[checked(gone).unwrap()]).unwrap();
        let claude = session_settings(&config, "lab", "claude", "small").unwrap();
        set_http_timeout(None);
        assert!(!claude.args.iter().any(|arg| arg == "--settings"), "{:?}", claude.args);
        // A window outside the bounds, or for no valid model, is refused.
        let mut bad = server.clone();
        bad.contexts.insert("small".into(), 12);
        assert!(checked(bad).unwrap_err().message.contains("4096 to 10000000"));
        let mut bad = server;
        bad.contexts.insert("-rf".into(), 131072);
        assert!(checked(bad).is_err());
    }

    /// The owner, 2026-11-02: the GB10 account (its settings point Claude
    /// Code at a gateway) gets the gateway's models in /model; an account
    /// that talks to Anthropic gets nothing from here.
    #[test]
    fn a_gateway_accounts_launch_lists_the_gateways_models() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let body = r#"{"data":[{"id":"coder"},{"id":"small"}]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let home = dir.path().join("account");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("settings.json"), json!({"env":{"ANTHROPIC_BASE_URL":format!("http://127.0.0.1:{port}"),"ANTHROPIC_AUTH_TOKEN":"sk-x"}}).to_string()).unwrap();
        let server = account_gateway(&home).unwrap();
        assert_eq!((server.kind.as_str(), server.key.as_str()), ("anthropic", "sk-x"));
        let path = picker_settings(&config, &server, "").unwrap();
        handle.join().unwrap();
        let picker: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let models: Vec<&str> = picker["modelPicker"]["options"].as_array().unwrap().iter().map(|row| row["model"].as_str().unwrap()).collect();
        assert_eq!(models, ["coder", "small"], "no empty row for a launch that names no model");
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("settings.json"), r#"{"model":"opus"}"#).unwrap();
        assert!(account_gateway(&plain).is_none());
    }

    #[test]
    fn model_names_with_colons_are_kept_whole_and_dash_names_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        // N7: the last ": " separates the state, so a tag stays in the name.
        let parsed = parse_status("coder-a:q4: running\n-rf: running\ncoder: stopped\n");
        assert_eq!(json!(parsed.models), json!([{"name":"coder-a:q4","state":"running"},{"name":"coder","state":"stopped"}]));
        assert_eq!(parsed.raw, vec!["-rf: running".to_owned()], "a leading dash is offered by no model list");
        // L3: a leading '-' is refused and never offered.
        assert!(checked_model("-rf").is_err());
        assert!(checked_model("coder-a:q4").is_ok());
        let mut server = server();
        server.down = vec!["serve".into(), "down".into(), "{model}".into()];
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Vec<String>>::new()));
        let record = seen.clone();
        set_fake_command(Some(Box::new(move |_: &Server, argv: &[String], _| {
            record.borrow_mut().push(argv.to_vec());
            Ok(Run { code: Some(0), stdout: String::new(), stderr: String::new() })
        })));
        down(&config, &server, "coder-a:q4", true).unwrap();
        assert_eq!(seen.borrow()[0], ["serve", "down", "coder-a:q4"], "Down targets the tagged model exactly");
        assert!(down(&config, &server, "-rf", true).is_err());
        assert_eq!(seen.borrow().len(), 1, "the leading dash never reaches a template");
        set_fake_command(None);
    }

    #[test]
    fn ssh_commands_use_hgs_transport_and_refuse_a_windows_machine() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut server = server();
        server.ssh = "labhost".into();
        server.status = vec!["serve".into(), "status".into()];
        let ssh = ssh_argv(&config, &server, &["serve".to_owned(), "status".to_owned()]);
        assert!(ssh.iter().any(|arg| arg == "-n"), "no stdin: {ssh:?}");
        assert!(ssh.iter().any(|arg| arg == "BatchMode=yes"));
        // L1: a Windows Machine has no POSIX login shell for the remote argv.
        crate::windows_peer::record(&config, "labhost", true).unwrap();
        let failure = status(&config, &server).unwrap_err();
        assert_eq!(failure.kind(), "refused");
        assert!(failure.detail().contains("Windows Machine"), "{failure:?}");
    }

    #[test]
    fn a_detached_up_starts_an_argv_and_an_ssh_argv_without_a_shell_line() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.up = vec!["serve".into(), "up".into(), "{model}".into()];
        server.up_minutes = 7;
        // Local: the template's argv itself.
        assert_eq!(detached_argv(&config, &server, &["serve".into(), "up".into(), "small".into()]), ["serve", "up", "small"]);
        // Over SSH: hgs's ssh argv, one argument per element, no shell line.
        let mut remote = server.clone();
        remote.ssh = "labhost".into();
        let argv = detached_argv(&config, &remote, &["serve".into(), "up".into(), "small".into()]);
        assert!(argv.iter().any(|arg| arg == "-n") && argv.contains(&"labhost".to_owned()), "{argv:?}");
        assert_eq!(argv.last().unwrap(), "serve up small", "the remote argv is one quoted word");
        crate::state::with_test_state_root(dir.path(), || {
            std::fs::create_dir_all(&config.dir).unwrap();
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(true));
            let started = up(&config, &server, "small", true).unwrap();
            assert_eq!(started["status"], "started");
            let id = started["request_id"].as_str().unwrap();
            let record = crate::state::builds::record(id, crate::state::now()).unwrap();
            assert_eq!(record["argv"], json!(["serve", "up", "small"]), "the runner takes the argv, not a shell line");
            assert_eq!(record["timeout_secs"], 7 * 60);
            crate::state::builds::INLINE_RUNNER.with(|flag| flag.set(false));
        });
    }

    #[test]
    fn a_resume_reapplies_the_server_or_refuses_when_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.kind = "anthropic".into();
        save(&config, &[server]).unwrap();
        let (env, secrets) = resume_environment(&config, "lab", "claude", "small").unwrap();
        assert!(env.iter().any(|line| line == "ANTHROPIC_BASE_URL=http://127.0.0.1:8080"), "{env:?}");
        assert_eq!(secrets, ["ANTHROPIC_AUTH_TOKEN=sk-test"]);
        assert!(!config.state.join("model-servers").exists(), "building the resume spec writes no key file (#709 L1)");
        // Only the launch writes it, right before the backend start.
        let path = write_secret_file(&config.state, &secrets).unwrap();
        assert!(path.starts_with(&config.state), "{path:?}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("ANTHROPIC_AUTH_TOKEN=sk-test"));
        // A server that is gone refuses the resume by name.
        let error = resume_environment(&config, "gone", "claude", "small").unwrap_err().to_string();
        assert!(error.contains("Unknown model server gone"), "{error}");
    }

    #[test]
    fn unread_secret_files_are_swept_after_an_hour() {
        let dir = tempfile::tempdir().unwrap();
        let old = write_secret_file(dir.path(), &["K=v".into()]).unwrap();
        let fresh = write_secret_file(dir.path(), &["K=v".into()]).unwrap();
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(earlier).unwrap();
        sweep_secret_files(dir.path());
        assert!(!old.exists(), "an unread key file older than an hour is removed");
        assert!(fresh.exists(), "a fresh key file stays for its launch");
    }

    #[test]
    fn the_codex_wire_api_follows_the_installed_cli() {
        // The mapping: chat before the deprecation, responses from then on and
        // when the CLI is unknown (the newest releases no longer serve chat).
        assert_eq!(wire_api_for(None), "responses");
        assert_eq!(wire_api_for(Some((0, 99, 9))), "chat");
        assert_eq!(wire_api_for(Some((0, 100, 0))), "responses");
        assert_eq!(wire_api_for(Some((0, 164, 0))), "responses");
        assert_eq!(parse_version("codex-cli 0.164.0"), Some((0, 164, 0)));
        assert_eq!(parse_version("warning: path codex-arg0 1.2.3\ncodex-cli 0.164.0"), Some((0, 164, 0)));
        // The installed CLI decides: the provider table carries its wire API.
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        save(&config, &[server()]).unwrap();
        let codex = session_settings(&config, "lab", "codex", "small").unwrap();
        let installed = installed_codex_version();
        let expected = wire_api_for(installed);
        assert!(codex.args[1].contains(&format!("wire_api=\"{expected}\"")), "{:?}", codex.args);
        if let Some(version) = installed {
            assert!(version >= RESPONSES_FROM, "the installed Codex is recent enough for responses: {version:?}");
            assert_eq!(expected, "responses");
        }
    }

    #[test]
    fn hgs_run_takes_the_secret_file_and_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_secret_file(dir.path(), &["HGS_MODEL_SERVER_KEY=sk-secret".into()]).unwrap();
        assert!(path.is_file());
        std::env::set_var("HGS_MODEL_SERVER_ENV", &path);
        apply_session_secrets().unwrap();
        assert_eq!(std::env::var("HGS_MODEL_SERVER_KEY").unwrap(), "sk-secret");
        assert!(!path.exists(), "the private file is removed after the read");
        assert!(std::env::var_os("HGS_MODEL_SERVER_ENV").is_none());
        std::env::remove_var("HGS_MODEL_SERVER_KEY");
    }

    #[test]
    fn the_codex_flags_are_the_installed_clis() {
        let Some(codex) = platform::which("codex") else { return };
        let mut command = Command::new(codex);
        command.arg("--help");
        let Ok(out) = platform::output_until(&mut command, Duration::from_secs(20)) else { return };
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("-c") || text.contains("--config"), "codex --help names its config override: {text}");
        assert!(text.contains("--model") || text.contains("-m"), "codex --help names --model: {text}");
    }

    #[test]
    fn a_session_gets_its_own_provider_settings_and_writes_no_global_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let mut server = server();
        server.kind = "openai".into();
        save(&config, &[server.clone()]).unwrap();
        let before = std::fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>();
        // Codex: a session-scoped custom provider, never its built-in OpenAI
        // provider or the ChatGPT login (N6). The key travels as the provider's
        // env_key, kept off every command line (N3).
        let codex = session_settings(&config, "lab", "codex", "small").unwrap();
        assert!(codex.env.is_empty(), "no global OpenAI variables: {:?}", codex.env);
        assert_eq!(codex.secrets, ["HGS_MODEL_SERVER_KEY=sk-test"]);
        assert_eq!(codex.args[0], "-c");
        assert!(codex.args[1].starts_with("model_providers.hgs-lab={name=\"lab\",base_url=\"http://127.0.0.1:8080/v1\""), "{:?}", codex.args);
        assert!(codex.args[1].contains("env_key=\"HGS_MODEL_SERVER_KEY\""));
        assert!(codex.args[1].contains(&format!("wire_api=\"{}\"", wire_api_for(installed_codex_version()))));
        assert_eq!(&codex.args[2..], ["-c", "model_provider=hgs-lab", "--model", "small"]);
        // N1: OpenCode has no per-session provider; it is not offered.
        let error = session_settings(&config, "lab", "opencode", "small").unwrap_err().to_string();
        assert!(error.contains("OpenCode is not offered"), "{error}");
        // Anthropic-compatible settings for Claude Code: the root, and the
        // token only in the secret set.
        let mut anthropic = server.clone();
        anthropic.kind = "anthropic".into();
        save(&config, &[anthropic]).unwrap();
        let claude = session_settings(&config, "lab", "claude", "small").unwrap();
        assert!(claude.env.iter().any(|line| line == "ANTHROPIC_BASE_URL=http://127.0.0.1:8080"), "{:?}", claude.env);
        assert!(claude.secrets.iter().any(|line| line == "ANTHROPIC_AUTH_TOKEN=sk-test"), "{:?}", claude.secrets);
        assert!(claude.env.iter().any(|line| line == "ANTHROPIC_MODEL=small"));
        assert!(claude.args.is_empty());
        // The agents' kinds must match the server's.
        assert!(session_settings(&config, "lab", "codex", "small").unwrap_err().to_string().contains("OpenAI"));
        let after = std::fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>();
        assert_eq!(before, after, "only the configuration file exists: no global agent settings");
    }
}
