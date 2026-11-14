//! Task 67: model servers (Qt ModelServers, ModelServersPage and
//! ModelServerSettings over `hgs model-server`). This computer's own
//! configuration: every call is `hgs model-server ...` here, never `@host`,
//! and the desktop never reads the private file or sees a key. Up, Down,
//! Logs, status, save and test are hgs's own operations, so the commands on
//! the server run exactly as they do from Qt. Nothing here retries.
//!
//! hgs answers an operation's failure with a JSON object (`"status":
//! "error"`) and exit 1; like Qt, any JSON object on stdout is the reply,
//! whatever the exit, and anything else is the call's failure.
use crate::actions::{failure_detail, Exit, Invocation};
use crate::text::Text;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

pub const REPLY_LIMIT: usize = 4 * 1024 * 1024;
const MAX_SERVERS: usize = 64;

/// One `hgs model-server` operation (Qt's Controller calls).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    List,
    Status {
        server: String,
    },
    /// Detached: the receipt names its run.
    Up {
        server: String,
        model: String,
    },
    UpResult {
        id: String,
    },
    Sessions {
        server: String,
    },
    /// Confirmed by the owner first (`--yes`).
    Down {
        server: String,
        model: String,
    },
    Logs {
        server: String,
        #[serde(default)]
        model: String,
    },
    /// model-catalog/2: select a service's variant, confirmed by the owner
    /// first (`--yes`); a running service restarts (`--restart`).
    Variant {
        server: String,
        model: String,
        variant: String,
        #[serde(default)]
        restart: bool,
    },
    Save {
        servers: Vec<Value>,
    },
    Test {
        server: Value,
    },
}

fn word(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.len() <= 512
        && !value.chars().any(char::is_control)
}

impl Op {
    /// The checked call. Every argument is one argv word; JSON goes on stdin.
    pub fn invocation(&self) -> Result<Invocation, String> {
        let mut args: Vec<String> = vec!["model-server".into()];
        let mut stdin = None;
        let seconds = match self {
            Op::List => {
                args.extend(["list".into(), "--json".into()]);
                30
            }
            Op::Status { server } => {
                if !word(server) {
                    return Err("Not a model server.".into());
                }
                args.extend(["status".into(), server.clone(), "--json".into()]);
                60
            }
            Op::Up { server, model } => {
                if !word(server) || !word(model) {
                    return Err("Not a model server and model.".into());
                }
                args.extend([
                    "up".into(),
                    server.clone(),
                    model.clone(),
                    "--detach".into(),
                ]);
                60
            }
            Op::UpResult { id } => {
                if !crate::teams::action::canonical_uuid(id) {
                    return Err("Not a request id.".into());
                }
                args.extend(["up-result".into(), id.clone(), "--json".into()]);
                30
            }
            Op::Sessions { server } => {
                if !word(server) {
                    return Err("Not a model server.".into());
                }
                args.extend(["sessions".into(), server.clone(), "--json".into()]);
                30
            }
            Op::Down { server, model } => {
                if !word(server) || !word(model) {
                    return Err("Not a model server and model.".into());
                }
                args.extend(["down".into(), server.clone(), model.clone(), "--yes".into()]);
                180
            }
            Op::Logs { server, model } => {
                if !word(server) || (!model.is_empty() && !word(model)) {
                    return Err("Not a model server and model.".into());
                }
                args.extend(["logs".into(), server.clone()]);
                if !model.is_empty() {
                    args.push(model.clone());
                }
                args.extend(["--lines".into(), "200".into()]);
                60
            }
            Op::Variant {
                server,
                model,
                variant,
                restart,
            } => {
                if !word(server) || !word(model) || !word(variant) {
                    return Err("Not a model server, model and variant.".into());
                }
                args.extend([
                    "variant".into(),
                    server.clone(),
                    model.clone(),
                    variant.clone(),
                    "--yes".into(),
                    "--detach".into(),
                ]);
                if *restart {
                    args.push("--restart".into());
                }
                180
            }
            Op::Save { servers } => {
                if servers.len() > MAX_SERVERS {
                    return Err("At most 64 model servers".into());
                }
                args.extend(["save".into(), "--json".into()]);
                stdin = Some(
                    json!({"by": "human", "servers": servers})
                        .to_string()
                        .into_bytes(),
                );
                30
            }
            Op::Test { server } => {
                if !server.is_object() {
                    return Err("Not a model server.".into());
                }
                args.extend(["test".into(), "--json".into()]);
                stdin = Some(server.to_string().into_bytes());
                60
            }
        };
        Ok(Invocation {
            args,
            stdin,
            timeout: Duration::from_secs(seconds),
            request_id: None,
        })
    }
}

/// The reply: any JSON object, whatever the exit (hgs's own failures are
/// objects too); otherwise why the call failed.
pub fn reply(exit: Exit) -> Result<Value, String> {
    if let Exit::Code { stdout, .. } = &exit {
        if let Ok(value) = serde_json::from_slice::<Value>(stdout) {
            if value.as_object().is_some_and(|o| !o.is_empty()) {
                return Ok(value);
            }
        }
        if let Exit::Code { code: 0, .. } = exit {
            return Err("Invalid model server response. Update hgs and try again.".into());
        }
    }
    Err(failure_detail(exit))
}

pub fn run(hgs: &std::path::Path, op: &Op) -> Result<Value, String> {
    let call = op.invocation()?;
    let exit = crate::run::run_limited(hgs, &call, &[], REPLY_LIMIT)
        .map_err(|_| "The model server reply is too large.".to_owned())?;
    reply(exit)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .take(64)
        .collect()
}

/// #703 (#658 N2): the server's root, without a typed trailing /v1 or /.
pub fn root_of(base_url: &str) -> String {
    let mut root = base_url.trim().trim_end_matches('/').to_owned();
    if let Some(stripped) = root.strip_suffix("/v1") {
        root = stripped.to_owned();
    }
    root.trim_end_matches('/').to_owned()
}
pub fn models_url(base_url: &str) -> String {
    format!("{}/v1/models", root_of(base_url))
}
/// What an agent of this kind is given: root/v1 (OpenAI), root (Anthropic).
pub fn agent_url(base_url: &str, kind: &str) -> String {
    if kind == "anthropic" {
        root_of(base_url)
    } else {
        format!("{}/v1", root_of(base_url))
    }
}

/// The argv a field shows: an argument with a space, or empty, in quotes.
pub fn argv_text(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg.is_empty() || arg.contains(' ') {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// QProcess::splitCommand: spaces separate, double quotes group, and three
/// quotes in a row are one literal quote.
pub fn argv_from_text(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let (mut quotes, mut quoted) = (0, false);
    for ch in text.trim().chars() {
        if ch == '"' {
            quotes += 1;
            if quotes == 3 {
                quotes = 0;
                current.push(ch);
            }
            continue;
        }
        if quotes > 0 {
            if quotes == 1 {
                quoted = !quoted;
            }
            quotes = 0;
        }
        if !quoted && ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// One server as `list` reports it (never its key).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Server {
    pub name: String,
    pub base_url: String,
    pub kind: String,
    pub ssh: String,
    pub has_key: bool,
    pub up_minutes: u64,
    pub contexts: Value,
    pub status: Vec<String>,
    pub up: Vec<String>,
    pub down: Vec<String>,
    pub logs: Vec<String>,
    /// model-catalog/2 (owner spec 2026-11-14), empty for the plain contract.
    pub protocol: String,
    pub catalog: Vec<String>,
    pub variant: Vec<String>,
    /// The catalog API's root and whether its key is set (never the key).
    pub api_url: String,
    pub has_api_key: bool,
    /// Without control commands: read only, no Up, Down or Logs.
    pub read_only: bool,
    pub agent_url: String,
    pub models_url: String,
    /// The fields Settings shows, as argv text.
    pub status_text: String,
    pub up_text: String,
    pub down_text: String,
    pub logs_text: String,
}

impl Server {
    fn from_row(row: &Value) -> Option<Self> {
        let name = text(row, "name");
        if name.is_empty() {
            return None;
        }
        let argv = &row["argv"];
        let (status, up, down, logs) = (
            strings(&argv["status"]),
            strings(&argv["up"]),
            strings(&argv["down"]),
            strings(&argv["logs"]),
        );
        let base_url = text(row, "base_url");
        let kind = text(row, "kind");
        let protocol = text(row, "protocol");
        let api_url = row["api"]["base_url"].as_str().unwrap_or_default().to_owned();
        Some(Self {
            read_only: status.is_empty() && up.is_empty() && down.is_empty() && logs.is_empty() && api_url.is_empty(),
            catalog: strings(&argv["catalog"]),
            variant: strings(&argv["variant"]),
            has_api_key: row["api"]["key"] == true,
            api_url,
            protocol,
            agent_url: agent_url(&base_url, &kind),
            models_url: models_url(&base_url),
            status_text: argv_text(&status),
            up_text: argv_text(&up),
            down_text: argv_text(&down),
            logs_text: argv_text(&logs),
            name,
            ssh: text(row, "ssh"),
            has_key: row["key"] == true,
            up_minutes: row["up_minutes"].as_u64().unwrap_or(0),
            contexts: row
                .get("contexts")
                .filter(|c| c.as_object().is_some_and(|o| !o.is_empty()))
                .cloned()
                .unwrap_or(Value::Null),
            base_url,
            kind,
            status,
            up,
            down,
            logs,
        })
    }

    /// The server as `save --json` takes it, without a key (Qt's Server::json).
    pub fn json(&self) -> Value {
        let mut out = json!({"name": self.name, "base_url": self.base_url, "kind": self.kind});
        if !self.ssh.is_empty() {
            out["ssh"] = json!(self.ssh);
        }
        if self.up_minutes > 0 {
            out["up_minutes"] = json!(self.up_minutes);
        }
        if self.contexts.is_object() {
            out["contexts"] = self.contexts.clone();
        }
        for (field, argv) in [
            ("status", &self.status),
            ("up", &self.up),
            ("down", &self.down),
            ("logs", &self.logs),
            ("catalog", &self.catalog),
            ("variant", &self.variant),
        ] {
            if !argv.is_empty() {
                out[field] = json!(argv);
            }
        }
        if !self.protocol.is_empty() {
            out["protocol"] = json!(self.protocol);
        }
        // Without `key` the stored catalog key stays (same API root).
        if !self.api_url.is_empty() {
            out["api"] = json!({"base_url": self.api_url});
        }
        out
    }

    /// A model-catalog/2 server.
    pub fn catalog_server(&self) -> bool {
        self.protocol == "model-catalog/2"
    }
}

/// `list --json` (or a saved reply): the servers with a name, at most 64.
pub fn parse_list(value: &Value) -> Vec<Server> {
    value["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_SERVERS)
        .filter_map(Server::from_row)
        .collect()
}

/// One model of a status read.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Model {
    pub name: String,
    /// The state as shown: "listed" for a model the API lists.
    pub state: String,
    pub label: Text,
    /// Seconds since the epoch, when the status says.
    pub since: i64,
    /// New session…: a running or listed model.
    pub usable: bool,
    pub can_down: bool,
    pub can_up: bool,
    /// model-catalog/2: what the catalog says of this service.
    pub catalog: Option<CatalogModel>,
}

/// One variant ("quality") of a catalog service.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Variant {
    pub id: String,
    pub context: u64,
    pub needs_gib: f64,
    pub tok_s: f64,
    pub code_tok_s: f64,
    pub selected: bool,
    pub running: bool,
}

/// One API name of a catalog service and its wire (anthropic or openai).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ApiModel {
    pub id: String,
    pub wire: String,
}

/// The catalog's own facts of a service, as the Model servers tab shows them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CatalogModel {
    /// Started outside the server's tool: shown without actions.
    pub managed: bool,
    pub variant: String,
    pub variants: Vec<Variant>,
    /// The live window (the selected variant's while stopped); 0 when unknown.
    pub context: u64,
    pub context_max: u64,
    /// A manual `contexts` entry that overrides the catalog.
    pub context_manual: u64,
    pub memory_gib: f64,
    pub needs_gib: f64,
    /// fits, does not fit, unloads all others, blocked by exclusive; empty while running.
    pub fit: String,
    pub tok_s: f64,
    pub code_tok_s: f64,
    pub api_models: Vec<ApiModel>,
    pub can_variant: bool,
    pub use_for: String,
}

fn catalog_model(row: &Value) -> CatalogModel {
    let number = |value: &Value| value.as_f64().unwrap_or(0.0);
    CatalogModel {
        managed: row["managed"] != false,
        variant: text(row, "variant"),
        variants: row["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .take(64)
            .filter_map(|variant| {
                let id = variant["id"].as_str().filter(|id| word(id))?.to_owned();
                Some(Variant {
                    id,
                    context: variant["context"].as_u64().unwrap_or(0),
                    needs_gib: number(&variant["needs_gib"]),
                    tok_s: number(&variant["tok_s"]),
                    code_tok_s: number(&variant["code_tok_s"]),
                    selected: variant["selected"] == true,
                    running: variant["running"] == true,
                })
            })
            .collect(),
        context: row["context"].as_u64().unwrap_or(0),
        context_max: row["context_max"].as_u64().unwrap_or(0),
        context_manual: row["context_manual"].as_u64().unwrap_or(0),
        memory_gib: number(&row["memory_gib"]),
        needs_gib: number(&row["needs_gib"]),
        fit: text(row, "fit"),
        tok_s: number(&row["speed"]["tok_s"]),
        code_tok_s: number(&row["speed"]["code_tok_s"]),
        api_models: row["api_models"]
            .as_array()
            .into_iter()
            .flatten()
            .take(16)
            .filter_map(|api| Some(ApiModel { id: api["id"].as_str()?.to_owned(), wire: text(api, "wire") }))
            .collect(),
        can_variant: row["managed"] != false && row["actions"]["variant"] != false,
        use_for: text(row, "use_for").chars().take(400).collect(),
    }
}

/// A catalog state in the tab's words: ready is running, starting is loading.
fn catalog_state(state: &str) -> String {
    match state {
        "ready" => "running".into(),
        "starting" => "loading".into(),
        other => other.into(),
    }
}

/// Qt's stateLabel: Running, Loading, Stopped, Listed (API), else the state.
pub fn state_label(state: &str, source: &str) -> Text {
    match state {
        "" if source == "api" => crate::text!("Listed"),
        "" => crate::text!("Unknown"),
        "listed" => crate::text!("Listed"),
        "running" => crate::text!("Running"),
        "loading" => crate::text!("Loading"),
        "stopped" => crate::text!("Stopped"),
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            Text::verbatim(
                chars
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default(),
            )
        }
    }
}

/// One status read (Qt's Status, with the page's derived texts).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    /// unreachable, refused, timed_out, unauthorized, or failed (hgs itself); empty when read.
    pub error: String,
    pub detail: String,
    /// The server's pill when the read failed.
    pub error_label: Text,
    /// Why it was not read, as the card says it.
    pub problem: Text,
    /// api or command.
    pub source: String,
    pub models: Vec<Model>,
    pub free: f64,
    pub total: f64,
    /// "38.2 of 48.0 GB free"; empty when not reported.
    pub memory: Text,
    /// Under 15 % free.
    pub low: bool,
    /// Used share, 0 to 1.
    pub used: f64,
    /// Lines neither JSON nor NAME: STATE.
    pub raw: Vec<String>,
    pub running: usize,
    /// Running or loading: what Down all stops.
    pub active: Vec<String>,
    /// model-catalog/2: the server did not answer; this is its cached catalog.
    pub stale: bool,
    /// When the shown catalog was read (seconds since the epoch), 0 otherwise.
    pub fetched_at: i64,
}

fn error_label(error: &str) -> Text {
    match error {
        "unreachable" => crate::text!("Unreachable"),
        "timed_out" => crate::text!("Timed out"),
        "unauthorized" => crate::text!("Key refused"),
        "refused" => crate::text!("Refused"),
        _ => crate::text!("Not read"),
    }
}

pub fn memory_text(free: f64, total: f64) -> Text {
    crate::text!(
        "%1 of %2 GB free",
        format!("{free:.1}"),
        format!("{total:.1}")
    )
}

/// Qt's parseStatus for `server`, or a failed call (`error`).
pub fn parse_status(server: &Server, reply: Result<&Value, &str>) -> Status {
    let mut out = Status {
        free: -1.0,
        total: -1.0,
        ..Default::default()
    };
    let value = match reply {
        Err(why) => {
            out.error = "failed".into();
            out.detail = why.chars().take(2000).collect();
            out.error_label = error_label("failed");
            out.problem = problem(server, &out.error, &out.detail);
            return out;
        }
        Ok(value) => value,
    };
    if value["status"] == "error" {
        out.error = value["error"].as_str().unwrap_or("refused").to_owned();
        out.detail = text(value, "detail").chars().take(2000).collect();
        out.error_label = error_label(&out.error);
        out.problem = problem(server, &out.error, &out.detail);
        return out;
    }
    out.source = text(value, "source");
    let catalog = out.source == "catalog";
    if catalog {
        out.stale = value["stale"] == true;
        out.fetched_at = value["fetched_at"].as_f64().unwrap_or(0.0) as i64;
        if out.stale {
            out.error = text(value, "error");
            out.detail = text(value, "detail").chars().take(2000).collect();
            out.error_label = error_label(&out.error);
        }
    }
    let api = !server.api_url.is_empty();
    for row in value["models"].as_array().into_iter().flatten().take(256) {
        let name = text(row, "name");
        if name.is_empty() {
            continue;
        }
        let raw_state = text(row, "state").to_lowercase();
        let state = if raw_state.is_empty() && out.source == "api" {
            "listed".to_owned()
        } else if catalog {
            catalog_state(&raw_state)
        } else {
            raw_state
        };
        let facts = catalog.then(|| catalog_model(row));
        let managed = facts.as_ref().is_none_or(|facts| facts.managed);
        // A catalog row says which actions it allows; the server needs a
        // command or the API for them.
        let allows = |action: &str| !catalog || row["actions"][action] != false;
        out.models.push(Model {
            label: state_label(&state, &out.source),
            usable: managed && (state == "running" || state == "listed"),
            can_down: managed
                && allows("down")
                && (!server.down.is_empty() || (catalog && api))
                && (state == "running" || state == "loading"),
            can_up: managed
                && allows("up")
                && (!server.up.is_empty() || (catalog && api))
                && matches!(state.as_str(), "stopped" | "unloaded" | "idle"),
            since: row["since"].as_f64().unwrap_or(0.0) as i64,
            catalog: facts,
            name,
            state,
        });
    }
    if let Some(memory) = value["memory"].as_object() {
        out.free = memory.get("free").and_then(Value::as_f64).unwrap_or(-1.0);
        out.total = memory.get("total").and_then(Value::as_f64).unwrap_or(-1.0);
    }
    if out.free >= 0.0 && out.total > 0.0 {
        out.memory = memory_text(out.free, out.total);
        out.low = out.free / out.total < 0.15;
        out.used = (1.0 - out.free / out.total).clamp(0.0, 1.0);
    }
    out.raw = strings(&value["raw"]);
    out.running = out.models.iter().filter(|m| m.state == "running").count();
    out.active = out
        .models
        .iter()
        .filter(|m| m.state == "running" || m.state == "loading")
        .map(|m| m.name.clone())
        .collect();
    out
}

/// Why a server was not read, as its card says (Qt serverCard).
fn problem(server: &Server, error: &str, detail: &str) -> Text {
    let mut why = if error == "unauthorized" {
        crate::text!("The server refused the API key: %1", detail.to_owned())
    } else if detail.is_empty() {
        error_label(error)
    } else {
        Text::verbatim(detail.trim_end_matches('.'))
    };
    if server.read_only && error != "failed" {
        why = crate::text!("GET %1: %2", server.models_url.clone(), why);
    }
    crate::text!("%1. Zerus tries again only when you ask.", why)
}

/// What Up's receipt says.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UpStart {
    /// Detached and running: poll `UpResult` with `id` until `limit_ms` passes.
    pub started: bool,
    pub id: String,
    pub limit_ms: u64,
    /// When it did not start: the outcome as the notice says it.
    pub outcome: Option<UpOutcome>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UpOutcome {
    pub text: Text,
    pub error: bool,
}

/// Up's receipt: started, or (refused, or an hgs that ran it at once) its outcome.
pub fn up_started(server: &str, model: &str, reply: Result<&Value, &str>) -> UpStart {
    if let Ok(value) = reply {
        let id = text(value, "request_id");
        if value["status"] == "started" && !id.is_empty() {
            // The run's own time limit, and two minutes for its runner to report.
            let minutes = value["timeout_minutes"].as_u64().unwrap_or(30);
            return UpStart {
                started: true,
                id,
                limit_ms: minutes * 60_000 + 120_000,
                outcome: None,
            };
        }
    }
    UpStart {
        started: false,
        id: String::new(),
        limit_ms: 0,
        outcome: Some(up_outcome(server, model, reply)),
    }
}

/// One `up-result` read: still running, or its outcome.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UpPoll {
    pub running: bool,
    pub outcome: Option<UpOutcome>,
}

pub fn up_poll(
    server: &str,
    model: &str,
    id: &str,
    reply: Result<&Value, &str>,
    expired: bool,
) -> UpPoll {
    if let Ok(value) = reply {
        if value["status"] == "running" {
            if !expired {
                return UpPoll {
                    running: true,
                    outcome: None,
                };
            }
            let why = crate::text!(
                "No result within its time limit; it may still be running (hgs run %1)",
                id.to_owned()
            );
            return UpPoll {
                running: false,
                outcome: Some(failed_up(server, model, why)),
            };
        }
    }
    UpPoll {
        running: false,
        outcome: Some(up_outcome(server, model, reply)),
    }
}

fn failed_up(server: &str, model: &str, why: Text) -> UpOutcome {
    UpOutcome {
        text: crate::text!(
            "Up %1 on %2 failed: %3",
            model.to_owned(),
            server.to_owned(),
            why
        ),
        error: true,
    }
}

/// The last line a run printed, for a failure's reason.
fn last_line(tail: &str) -> String {
    tail.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
        .unwrap_or_default()
}

/// #703 (#658 N5): the run's own outcome, not only that it started.
pub fn up_outcome(server: &str, model: &str, reply: Result<&Value, &str>) -> UpOutcome {
    let why: Option<Text> = match reply {
        Err(error) => Some(Text::verbatim(error)),
        Ok(value) => {
            let status = value["status"].as_str().unwrap_or_default();
            let exit = value["exit"].as_i64().unwrap_or(-1);
            if status == "error" {
                Some(Text::verbatim(
                    value["detail"]
                        .as_str()
                        .or(value["error"].as_str())
                        .unwrap_or_default(),
                ))
            } else if value["timed_out"] == true {
                Some(crate::text!(
                    "stopped at its %1 min limit",
                    (value["timeout_secs"].as_i64().unwrap_or(0) / 60).to_string()
                ))
            } else if status == "refused" {
                Some(match value["error"].as_str() {
                    Some(error) => Text::verbatim(error),
                    None => crate::text!("it did not run"),
                })
            } else if status == "ran" && exit != 0 {
                let line = last_line(value["tail"].as_str().unwrap_or_default());
                Some(if line.is_empty() {
                    crate::text!("exit %1", exit.to_string())
                } else {
                    crate::text!("exit %1: %2", exit.to_string(), line)
                })
            } else {
                None
            }
        }
    };
    match why {
        None => UpOutcome {
            text: crate::text!(
                "Up %1 on %2 finished: exit 0. Nothing starts it again on its own.",
                model.to_owned(),
                server.to_owned()
            ),
            error: false,
        },
        Some(why) => failed_up(server, model, why),
    }
}

/// Down's failure for one model, if any (`model: why` in the notice).
pub fn down_failure(reply: Result<&Value, &str>) -> Option<Text> {
    match reply {
        Err(error) => Some(Text::verbatim(error)),
        Ok(value) if value["status"] == "error" => Some(Text::verbatim(text(value, "detail"))),
        Ok(value) => match value["exit"].as_i64().unwrap_or(0) {
            0 => None,
            exit => Some(crate::text!("exit %1", exit.to_string())),
        },
    }
}

/// #703: the names of this computer's live sessions on a server, by model.
pub fn sessions_by_model(
    reply: Result<&Value, &str>,
) -> Result<BTreeMap<String, Vec<String>>, Text> {
    let value = reply.map_err(Text::verbatim)?;
    if value["status"] == "error" {
        return Err(Text::verbatim(text(value, "detail")));
    }
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in value["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(4096)
    {
        // model-catalog/2: a session belongs to its service, whichever API
        // name it uses.
        let service = text(row, "service");
        let key = if service.is_empty() { text(row, "model") } else { service };
        out.entry(key).or_default().push(text(row, "name"));
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Logs {
    pub lines: Vec<String>,
    pub truncated: bool,
}

pub fn logs(reply: Result<&Value, &str>) -> Result<Logs, Text> {
    let value = reply.map_err(Text::verbatim)?;
    if value["status"] == "error" {
        return Err(Text::verbatim(text(value, "detail")));
    }
    Ok(Logs {
        lines: strings_unbounded(&value["lines"]),
        truncated: value["truncated"] == true,
    })
}
fn strings_unbounded(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .take(10_000)
        .collect()
}

/// Settings' form, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Form {
    /// The server being edited; empty for a new one.
    pub was: String,
    pub name: String,
    pub base_url: String,
    pub kind: String,
    pub ssh: String,
    /// "", "replace" or "remove".
    pub key_change: String,
    pub key: String,
    pub status: String,
    pub up: String,
    pub down: String,
    pub logs: String,
}

impl Form {
    /// Qt's formServer: the key only when typed or removed, otherwise the
    /// stored one stays; contexts and up_minutes kept as they were.
    pub fn server(&self, stored: &[Server], with_control: bool) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name.trim()));
        out.insert("base_url".into(), json!(self.base_url.trim()));
        out.insert(
            "kind".into(),
            json!(if self.kind == "anthropic" {
                "anthropic"
            } else {
                "openai"
            }),
        );
        if !self.ssh.trim().is_empty() {
            out.insert("ssh".into(), json!(self.ssh.trim()));
        }
        if with_control {
            for (field, typed) in [
                ("status", &self.status),
                ("up", &self.up),
                ("down", &self.down),
                ("logs", &self.logs),
            ] {
                let argv = argv_from_text(typed);
                if !argv.is_empty() {
                    out.insert(field.into(), json!(argv));
                }
            }
        }
        if let Some(old) = stored
            .iter()
            .find(|s| !self.was.is_empty() && s.name == self.was)
        {
            if old.up_minutes > 0 {
                out.insert("up_minutes".into(), json!(old.up_minutes));
            }
            if old.contexts.is_object() {
                out.insert("contexts".into(), old.contexts.clone());
            }
        }
        if !self.was.is_empty() {
            out.insert("was".into(), json!(self.was));
        }
        if self.key_change == "remove" {
            out.insert("key".into(), Value::Null);
        } else if !self.key.is_empty() {
            out.insert("key".into(), json!(self.key));
        }
        Value::Object(out)
    }
}

fn kept(server: &Server) -> Value {
    let mut other = server.json();
    other["was"] = json!(server.name);
    other
}

/// The whole list Save sends: this form in its server's place (or added).
pub fn saved_list(stored: &[Server], form: &Form) -> Vec<Value> {
    let mut out = Vec::new();
    let mut replaced = false;
    for server in stored {
        if !form.was.is_empty() && server.name == form.was {
            out.push(form.server(stored, true));
            replaced = true;
        } else {
            out.push(kept(server));
        }
    }
    if !replaced {
        out.push(form.server(stored, true));
    }
    out
}

/// The list Delete server sends: every other server, keys kept.
pub fn without(stored: &[Server], name: &str) -> Vec<Value> {
    stored.iter().filter(|s| s.name != name).map(kept).collect()
}

/// Save's refusal, if any.
pub fn save_refusal(reply: Result<&Value, &str>) -> Option<Text> {
    match reply {
        Err(error) => Some(Text::verbatim(error)),
        Ok(value) if value["status"] != "saved" => Some(match value["detail"].as_str() {
            Some(detail) => Text::verbatim(detail),
            None => crate::text!("Not saved"),
        }),
        Ok(_) => None,
    }
}

/// A Test result line (Qt showResult): ok, warn or error, and its text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TestResult {
    pub tone: String,
    pub text: Text,
    /// The status command's raw lines (Test status only).
    pub raw: Vec<String>,
}

/// `api`: Test API (the model list at `models_url`); `status`: the status command.
pub fn test_result(what: &str, base_url: &str, reply: Result<&Value, &str>) -> TestResult {
    let read = (what == "api").then(|| models_url(base_url));
    let failed = match reply {
        Err(error) => Some(Text::verbatim(error)),
        Ok(value) if value["status"] == "error" => Some(Text::verbatim(
            value["detail"]
                .as_str()
                .or(value["error"].as_str())
                .unwrap_or_default(),
        )),
        Ok(_) => None,
    };
    if let Some(why) = failed {
        let text = match read {
            Some(url) => crate::text!("GET %1: %2", url, why),
            None => why,
        };
        return TestResult {
            tone: "error".into(),
            text,
            raw: Vec::new(),
        };
    }
    let value = reply.unwrap_or(&Value::Null);
    let status = parse_status(&Server::default(), Ok(value));
    let count = models_count(status.models.len());
    if status.source == "api" {
        let url = read.unwrap_or_else(|| "/v1/models".into());
        return TestResult {
            tone: "ok".into(),
            text: crate::text!("GET %1 answered: %2", url, count),
            raw: Vec::new(),
        };
    }
    let raw = if what == "status" {
        status.raw.clone()
    } else {
        Vec::new()
    };
    if !status.raw.is_empty() && status.models.is_empty() {
        return TestResult {
            tone: "warn".into(),
            text: crate::text!("Neither the JSON contract nor NAME: STATE lines: shown as raw text; the models come from the API"),
            raw,
        };
    }
    let text = if status.memory.is_empty() {
        count
    } else {
        crate::text!("%1, %2", count, status.memory.clone())
    };
    TestResult {
        tone: "ok".into(),
        text,
        raw,
    }
}

fn models_count(n: usize) -> Text {
    if n == 1 {
        crate::text!("1 model")
    } else {
        crate::text!("%1 models", n.to_string())
    }
}

/// Test logs: reads the saved server's logs once, with a model its status listed.
pub fn logs_test(
    stored: Option<&Server>,
    typed: &str,
    model: &str,
    reply: Option<Result<&Value, &str>>,
) -> TestResult {
    let error = |text: Text| TestResult {
        tone: "error".into(),
        text,
        raw: Vec::new(),
    };
    match stored {
        Some(server) if server.logs == argv_from_text(typed) => {}
        _ => {
            return error(crate::text!(
                "Save the server first: Logs reads the saved command"
            ))
        }
    }
    if model.is_empty() {
        return error(crate::text!(
            "Read the status first: Logs needs one of the server's models"
        ));
    }
    match reply.map(logs) {
        Some(Ok(read)) => TestResult {
            tone: "ok".into(),
            text: crate::text!(
                "%1: %2 lines read",
                model.to_owned(),
                read.lines.len().to_string()
            ),
            raw: Vec::new(),
        },
        Some(Err(why)) => error(why),
        None => error(crate::text!("Not read")),
    }
}

/// New session on a model server (Qt launchModelServerSession):
/// `hgs AGENT FOLDER --new -n TAG --launch-id ID -d --model-server SERVER --model MODEL`.
pub fn session_launch(
    agent: &str,
    folder: &str,
    tag: &str,
    server: &str,
    model: &str,
    launch_id: &str,
) -> Result<Invocation, String> {
    if !matches!(agent, "codex" | "claude")
        || folder.trim().is_empty()
        || folder.starts_with('-')
        || folder.chars().any(char::is_control)
        || !word(tag)
        || tag.chars().count() > 60
        || !word(server)
        || !word(model)
        || !crate::teams::action::canonical_uuid(launch_id)
    {
        return Err("Choose an agent, a folder, a name and a model.".into());
    }
    Ok(Invocation {
        args: vec![
            agent.into(),
            folder.into(),
            "--new".into(),
            "-n".into(),
            tag.into(),
            "--launch-id".into(),
            launch_id.into(),
            "-d".into(),
            "--model-server".into(),
            server.into(),
            "--model".into(),
            model.into(),
        ],
        stdin: None,
        timeout: Duration::from_secs(90),
        request_id: None,
    })
}

/// Starts the launch: done when hgs exits 0; otherwise why not.
pub fn launch(hgs: &std::path::Path, call: &Invocation) -> Result<(), String> {
    match crate::run::run_limited(hgs, call, &[], 1024 * 1024) {
        Ok(Exit::Code { code: 0, .. }) => Ok(()),
        Ok(other) => Err(failure_detail(other)),
        Err(_) => Err("The session's reply is too large.".into()),
    }
}

#[cfg(test)]
mod tests;
