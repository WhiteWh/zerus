//! Mobile reaches the local machine or one enabled, directly configured SSH peer.
//! The remote shell sees only fixed helper commands; native argv travels as data.
use crate::{cli, config::Config, machines, platform, swarm};
use anyhow::{bail, ensure, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt},
    },
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR: usize = 64 * 1024;
const MAX_PEERS: usize = 32;
const CAPABILITY: &str = "mobile-peer ABI 1";
const IDENTITY_HELPER: &str = "~/.local/bin/hgs swarm __mobile-peer-identity --json";
const LOCAL_HELPER: &str = "~/.local/bin/hgs swarm __mobile-peer-local --json";
const REMEMBERED: &str = "swarm/mobile-peers.json";
const REMEMBERED_LOCK: &str = "swarm/mobile-peers.lock";
const REMEMBERED_BYTES: u64 = 64 * 1024;
static TRANSPORT_STOP: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Helper {
    Help,
    Identity,
    Local,
    OwnIdentity,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransportRequest {
    schema: u32,
    via: String,
    helper: Helper,
    request: Option<LocalRequest>,
    parent_pid: u32,
    deadline_ms: u64,
}

fn monotonic_ms() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0,
        "mobile transport clock unavailable"
    );
    Ok(time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000)
}

extern "C" fn stop_transport(_: libc::c_int) {
    TRANSPORT_STOP.store(true, Ordering::Relaxed);
}

enum CaptureMode {
    Supervisor,
    OwnedSsh { parent_pid: u32, deadline_ms: u64 },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    schema: u32,
    target_machine_id: String,
    argv: Vec<String>,
    payload: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerRequest {
    schema: u32,
    via: String,
    target_machine_id: String,
    argv: Vec<String>,
    payload: Option<Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema: u32,
    machine_id: String,
    name: String,
}

/// An identity this gateway itself verified through one configured alias.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Verified {
    machine_id: String,
    name: String,
}

/// Last verified peer identities, bound to the local identity that observed them.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Remembered {
    schema: u32,
    local: String,
    peers: BTreeMap<String, Verified>,
}

fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

fn text(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= max && !value.chars().any(char::is_control)
}

fn alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn direct(config: &Config, via: &str) -> bool {
    alias(via)
        && !config.is_self(via)
        && config.peers.iter().any(|p| p == via)
        && config.machines.get(via).is_none_or(|m| m.enabled)
}

fn input<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "mobile request is too large");
    serde_json::from_slice(crate::state::without_bom(&bytes)).context("invalid mobile request")
}

fn options<'a>(
    args: &'a [String],
    switches: &[&str],
    values: &[&str],
) -> Result<BTreeMap<&'a str, &'a str>> {
    let mut result = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        ensure!(!result.contains_key(flag), "duplicate native option");
        if switches.contains(&flag) {
            result.insert(flag, "");
            index += 1;
        } else {
            ensure!(values.contains(&flag), "native option is not allowed");
            let value = args.get(index + 1).context("missing native option value")?;
            ensure!(
                text(value, 4096, flag == "--conversation")
                    && !value.starts_with('-')
                    && !value.starts_with('@'),
                "invalid native option value"
            );
            result.insert(flag, value.as_str());
            index += 2;
        }
    }
    Ok(result)
}

/// The help line (`cli::USAGE`) once [`selects_session`] holds (zerus-ucck
/// P1c-2b); the connector reads it.
#[cfg(test)]
const SESSION_ID_MARKER: &str = "mobile-peer-session-id-v1";

/// The scoped JSON commands whose session a phone mutation may also select
/// by ID.
const SESSION_ID_COMMANDS: &[&str] = &[
    "send",
    "send-now",
    "answer",
    "interrupt",
    "settings",
    "clear-context",
    "compact-context",
    "session-action",
];

/// The `session-action` actions that may; restore never does.
const SESSION_ID_ACTIONS: &[&str] = &[
    "pause",
    "resume",
    "rename",
    "fork",
    "archive",
    "forget",
    "terminate",
    "remove_queued",
];

/// A listed command's `[session, --session-id, ID, --json]` with a canonical
/// session ID, for a live session only: never an archived conversation, and
/// a `session-action` only for a listed action. The selection itself is the
/// CLI's own (`--session-id`), verified at every reader.
fn selects_session(op: &str, rest: &[String], payload: Option<&Value>) -> bool {
    let [name, flag, id, json] = rest else {
        return false;
    };
    SESSION_ID_COMMANDS.contains(&op)
        && session(name)
        && flag == "--session-id"
        && crate::state::session_id::canonical(id)
        && uuid(id)
        && json == "--json"
        && payload.is_some_and(|payload| {
            payload.get("archive_id").is_none()
                && (op != "session-action"
                    || payload
                        .get("action")
                        .and_then(Value::as_str)
                        .is_some_and(|action| SESSION_ID_ACTIONS.contains(&action)))
        })
}

fn scoped(payload: &Option<Value>) -> Result<()> {
    let value = payload
        .as_ref()
        .context("scoped native payload is required")?;
    ensure!(value.is_object(), "native payload must be an object");
    ensure!(
        value
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(uuid),
        "native request UUID is required"
    );
    ensure!(
        value
            .get("expected_run_id")
            .and_then(Value::as_str)
            .is_some_and(|v| text(v, 128, false)),
        "native run identity is required"
    );
    ensure!(
        value
            .get("expected_conversation_id")
            .and_then(Value::as_str)
            .is_some_and(|v| text(v, 256, true)),
        "native conversation identity is required"
    );
    Ok(())
}

/// C1 stdin: exactly the run/conversation identity and two nullable marks.
fn read_mark(payload: &Option<Value>) -> Result<()> {
    let value = payload.as_ref().context("read mark payload is required")?;
    let object = value.as_object().context("native payload must be an object")?;
    ensure!(
        object.len() == 4
            && ["run_id", "conversation_id", "reply_id", "attention_signature"]
                .iter()
                .all(|key| object.contains_key(*key)),
        "invalid read mark fields"
    );
    ensure!(
        value["run_id"].as_str().is_some_and(|v| text(v, 128, false))
            && value["conversation_id"].as_str().is_some_and(|v| text(v, 256, false)),
        "native run and conversation identity are required"
    );
    for key in ["reply_id", "attention_signature"] {
        ensure!(
            value[key].is_null() || value[key].as_str().is_some_and(|v| text(v, 256, false)),
            "invalid read mark value"
        );
    }
    Ok(())
}

/// New-label auxiliary calls carry one fixed selector immediately after the
/// session. Validate the original CLOSED command shape using a temporary argv;
/// dispatch retains the original selector. This does not admit a new operation
/// or reinterpret a status/recovery/actor plan.
fn selected_auxiliary(request: &LocalRequest) -> Result<Option<LocalRequest>> {
    const AUXILIARY: &[&str] = &[
        "inspect",
        "processes",
        "history",
        "history-projection",
        "terminal",
        "hooks",
        "read",
        "notifications",
        "file-read",
        "attachment",
        "voice",
    ];
    let [command, name, flag, id, tail @ ..] = request.argv.as_slice() else {
        return Ok(None);
    };
    if !AUXILIARY.contains(&command.as_str()) || flag != "--session-id" {
        return Ok(None);
    }
    ensure!(
        session(name) && crate::state::session_id::canonical(id),
        "invalid native session ID"
    );
    ensure!(
        !tail
            .iter()
            .any(|word| matches!(word.as_str(), "--archive" | "--session-id")),
        "ambiguous or archived session ID selection"
    );
    ensure!(
        request
            .payload
            .as_ref()
            .is_none_or(|p| p.get("archive_id").is_none()),
        "archived session ID selection"
    );
    Ok(Some(LocalRequest {
        schema: request.schema,
        target_machine_id: request.target_machine_id.clone(),
        argv: [vec![command.clone(), name.clone()], tail.to_vec()].concat(),
        payload: request.payload.clone(),
    }))
}

/// Validate complete argv shapes before calling the CLI's permissive legacy dispatcher.
/// Session operations retain their native scoped-payload validation and receipts.
fn validate(request: &LocalRequest) -> Result<()> {
    ensure!(
        request.schema == 1 && uuid(&request.target_machine_id),
        "invalid mobile target"
    );
    let args = &request.argv;
    ensure!(!args.is_empty() && args.len() <= 24, "invalid native argv");
    ensure!(
        args.iter().all(|v| text(v, 4096, true)),
        "invalid native argv"
    );
    ensure!(
        request.payload.as_ref().is_none_or(Value::is_object),
        "native payload must be an object"
    );
    if let Some(original_shape) = selected_auxiliary(request)? {
        return validate(&original_shape);
    }
    let op = args[0].as_str();
    let rest = &args[1..];
    let no_payload = || -> Result<()> {
        ensure!(
            request.payload.is_none(),
            "this native operation has no payload"
        );
        Ok(())
    };
    match op {
        "__state" => {
            ensure!(
                rest.len() == 4
                    && rest[0] == "delivery"
                    && rest[1] == "receipt"
                    && session(&rest[2])
                    && rest[3] == "--json",
                "delivery requires the exact receipt read ABI"
            );
            scoped(&request.payload)?;
            ensure!(
                request
                    .payload
                    .as_ref()
                    .is_some_and(|p| p["expected_machine_id"] == request.target_machine_id),
                "original receipt computer changed"
            );
        }
        "analytics" => {
            ensure!(rest == ["--json"], "analytics requires its fixed JSON ABI");
            let payload = request.payload.as_ref().context("analytics payload is required")?;
            ensure!(payload["action"] == "summary", "mobile analytics only permits summary reads");
            crate::analytics::validate_request(payload).map_err(anyhow::Error::msg)?;
        }
        "voice" => {
            ensure!(rest.len() == 2 && session(&rest[0]) && rest[1] == "--json", "voice requires a session and its fixed JSON ABI");
            crate::state::voice::validate_request(request.payload.as_ref().context("voice payload is required")?)
                .map_err(anyhow::Error::msg)?;
        }
        "speech" => {
            ensure!(rest == ["--json"], "speech requires its fixed JSON ABI");
            crate::speech::validate_request(request.payload.as_ref().context("speech payload is required")?)?;
        }
        "--help" => {
            ensure!(rest.is_empty(), "invalid help request");
            no_payload()?;
        }
        "ls" => {
            let flags = options(rest, &["--json", "--local", "--no-archives"], &[])?;
            ensure!(flags.len() >= 2 && flags.contains_key("--json") && flags.contains_key("--local"), "snapshot must be local JSON");
            no_payload()?;
        }
        "archives" => {
            let payload = request.payload.as_ref().context("archive query required")?;
            if rest == ["lookup", "--json"] {
                crate::state::archive_catalog::validate_lookup(payload).map_err(anyhow::Error::msg)?;
            } else {
                ensure!(rest == ["--json"], "archives requires its fixed JSON ABI");
                crate::state::archive_catalog::validate(payload).map_err(anyhow::Error::msg)?;
            }
        }
        "swarm" => {
            if rest == ["session-project", "--json"] || rest == ["assign-session", "--json"] {
                let payload = request.payload.as_ref().context("project assignment request required")?;
                crate::swarm::validate_session_project(payload, rest[0] == "assign-session")?;
                ensure!(payload["expected_machine_id"] == request.target_machine_id, "project assignment target computer changed");
                ensure!(payload["name"].as_str().is_some_and(session), "invalid native session");
            } else if rest == ["assign-launch", "--json"] {
                scoped(&request.payload)?;
                ensure!(
                    request
                        .payload
                        .as_ref()
                        .and_then(|v| v["name"].as_str())
                        .is_some_and(session),
                    "invalid native session"
                );
                // The local native ABI validates project/swarm/folder identity,
                // exact launch UUID and its durable metadata receipt as one action.
            } else if rest.len() == 5
                && rest[0] == "preference"
                && rest[1] == "set"
                && rest[2] == "mobile_delivery"
                && crate::swarm::MOBILE_DELIVERY.contains(&rest[3].as_str())
                && rest[4] == "--json"
            {
                // The gateway connector changes the swarm-wide delivery choice.
                no_payload()?;
            } else {
                ensure!(
                    rest.len() == 1 && matches!(rest[0].as_str(), "get" | "hello"),
                    "swarm operation is not allowed"
                );
                no_payload()?;
            }
        }
        "account" => {
            ensure!(
                rest == ["ls"]
                    || (rest.len() == 2
                        && rest[0] == "inspect"
                        && !rest[1].starts_with(['-', '@'])
                        && !rest[1].is_empty())
                    || (rest.len() == 3
                        && rest[0] == "inspect"
                        && rest[1] == "--session"
                        && session(&rest[2])),
                "account operation is not allowed"
            );
            no_payload()?;
        }
        "dirs" => {
            if rest.first().is_some_and(|value| value == "--create") {
                ensure!(rest.len() == 4 && rest[2] == "--request-id" && uuid(&rest[3]),
                    "directory creation requires an exact request UUID");
                let path = &rest[1];
                let leaf = path.rsplit('/').next().unwrap_or("");
                ensure!(path.starts_with('/') && text(path, 4096, false)
                    && !matches!(leaf, "" | "." | ".."), "invalid directory creation path");
            } else {
                ensure!(rest.len() == 1 && session(&rest[0]), "invalid directory request");
            }
            no_payload()?;
        }
        // The phone's task exchange (docs/superpowers/specs/2026-10-18-task-replication-loro-design.md, "Phone").
        "task" => match rest {
            [action, json] if action == "exchange" && json == "--json" => {
                ensure!(request.payload.as_ref().is_some_and(Value::is_object), "the task exchange needs its JSON request");
            }
            [action, json] if action == "digests" && json == "--json" => no_payload()?,
            [action, json] if ["mobile-send", "mobile-cancel", "mobile-done"].contains(&action.as_str()) && json == "--json" => {
                ensure!(request.payload.as_ref().is_some_and(Value::is_object), "the task action needs its JSON request");
            }
            _ => bail!("native operation is not allowed"),
        },
        "repository" => {
            no_payload()?;
            let command = rest.first().context("repository action required")?;
            ensure!(
                ["check", "clone", "status"].contains(&command.as_str()),
                "invalid repository action"
            );
            // These are values of fixed native options, including literal folder
            // names beginning with '-' or '@'; they never become routing tokens.
            let mut values = BTreeMap::new();
            let mut tokens = rest[1..].iter();
            while let Some(flag) = tokens.next() {
                ensure!(
                    !values.contains_key(flag.as_str()),
                    "duplicate repository option"
                );
                let value = if flag == "--json" {
                    ""
                } else {
                    ensure!(
                        ["--url", "--parent", "--name", "--request-id"].contains(&flag.as_str()),
                        "repository option is not allowed"
                    );
                    let value = tokens.next().context("repository option value required")?;
                    ensure!(text(value, 4096, false), "invalid repository option value");
                    value.as_str()
                };
                values.insert(flag.as_str(), value);
            }
            ensure!(
                values.contains_key("--json")
                    && values.len()
                        == match command.as_str() {
                            "check" => 4,
                            "clone" => 5,
                            _ => 2,
                        },
                "incomplete repository request"
            );
            if command != "check" {
                ensure!(
                    values.get("--request-id").is_some_and(|v| uuid(v)),
                    "repository requires an exact request UUID"
                );
            }
            if command != "status" {
                for key in ["--url", "--parent", "--name"] {
                    ensure!(values.contains_key(key), "incomplete repository request");
                }
                let parent = values["--parent"];
                ensure!(
                    parent == "~" && command == "check"
                        || parent.starts_with('/') && !parent.starts_with("//"),
                    "repository parent must be absolute"
                );
                let name = values["--name"];
                ensure!(
                    text(name, 240, false)
                        && !matches!(name, "." | "..")
                        && !name.contains(['/', '\\']),
                    "invalid repository folder name"
                );
                ensure!(
                    !values.contains_key("--request-id") || command == "clone",
                    "unexpected repository request UUID"
                );
            } else {
                ensure!(
                    values.contains_key("--request-id"),
                    "repository status requires its original UUID"
                );
            }
        }
        "git-status" => {
            no_payload()?;
            let values = options(rest, &["--json"], &["--path"])?;
            ensure!(
                values.len() == 2 && values.contains_key("--json"),
                "Git status requires its exact path and JSON options"
            );
            ensure!(
                values.get("--path").is_some_and(|path| {
                    text(path, 4096, false) && std::path::Path::new(path).is_absolute()
                }),
                "Git status path must be absolute"
            );
        }
        "worktrees" => {
            no_payload()?;
            let create = rest.first().is_some_and(|v| v == "create");
            let values = if create {
                options(
                    &rest[1..],
                    &["--json", "--mobile"],
                    &[
                        "--path",
                        "--common-dir",
                        "--destination",
                        "--branch",
                        "--base",
                        "--request-id",
                    ],
                )?
            } else {
                options(rest, &["--json", "--refresh"], &["--path"])?
            };
            ensure!(
                values.len() == if create { 8 } else { 3 },
                "incomplete worktree request"
            );
            for field in if create {
                vec!["--path", "--common-dir", "--destination"]
            } else {
                vec!["--path"]
            } {
                ensure!(
                    values
                        .get(field)
                        .is_some_and(|v| std::path::Path::new(v).is_absolute()),
                    "worktree path must be absolute"
                );
            }
            if create {
                ensure!(
                    values.contains_key("--mobile"),
                    "mobile creation deadline is required"
                );
                ensure!(
                    values.get("--request-id").is_some_and(|v| uuid(v)),
                    "invalid worktree request UUID"
                );
                for field in ["--branch", "--base"] {
                    ensure!(
                        values
                            .get(field)
                            .is_some_and(|v| text(v, 256, false) && !v.starts_with('-')),
                        "invalid worktree branch or revision"
                    );
                }
            }
        }
        "inspect" => {
            ensure!(
                rest.first().is_some_and(|v| session(v)),
                "invalid native session"
            );
            let flags = options(
                &rest[1..],
                &["--skip-processes", "--current-read"],
                &["--archive", "--agent", "--after"],
            )?;
            if let Some(after) = flags.get("--after") {
                ensure!(after.parse::<u64>().is_ok(), "invalid history cursor");
            }
            // As in the CLI, only a main inspection names what a read mark
            // may acknowledge now.
            ensure!(
                !flags.contains_key("--current-read")
                    || !(flags.contains_key("--archive") || flags.contains_key("--agent")),
                "current read is only for a main inspection"
            );
            if let Some(id) = flags.get("--archive") {
                ensure!(uuid(id), "invalid archive UUID");
            }
            no_payload()?;
        }
        "recovery" => {
            ensure!(
                rest == ["action", "--scoped-json"],
                "recovery operation is not allowed"
            );
            scoped(&request.payload)?;
            ensure!(
                request
                    .payload
                    .as_ref()
                    .and_then(|v| v["name"].as_str())
                    .is_some_and(session),
                "invalid native session"
            );
        }
        "read" => {
            ensure!(
                rest.len() == 2 && session(&rest[0]) && rest[1] == "--json",
                "native read requires scoped JSON"
            );
            read_mark(&request.payload)?;
        }
        "notifications" => {
            ensure!(
                rest.len() == 2 && session(&rest[0]) && rest[1] == "--json",
                "native notifications requires scoped JSON"
            );
            let value = request.payload.as_ref().context("notification preference payload is required")?;
            let object = value.as_object().context("native payload must be an object")?;
            ensure!(
                object.len() == 4 && ["run_id", "conversation_id", "muted", "expected_revision"]
                    .iter().all(|key| object.contains_key(*key)),
                "invalid notification preference fields"
            );
            ensure!(
                value["run_id"].as_str().is_some_and(|v| text(v, 128, false) && v.len() <= 128)
                    && value["conversation_id"].as_str().is_some_and(|v| text(v, 256, false) && v.len() <= 256)
                    && value["muted"].is_boolean()
                    && value["expected_revision"].as_i64().is_some_and(|r| r >= 0),
                "invalid notification preference identity or revision"
            );
        }
        "hooks" => {
            ensure!(rest.len()==2 && session(&rest[0]) && rest[1]=="--json", "hook health requires scoped JSON");
            scoped(&request.payload)?;
            ensure!(request.payload.as_ref().and_then(Value::as_object).is_some_and(|object| object.len()==3 && object.keys().all(|key| ["request_id","expected_run_id","expected_conversation_id"].contains(&key.as_str()))), "hook health is read-only");
        }
        "history" | "history-projection" | "terminal" | "session-action" | "send" | "send-now" | "settings" | "answer"
        | "interrupt" | "clear-context" | "compact-context" | "effort" | "file-read" => {
            ensure!(
                (rest.len() == 2 && session(&rest[0]) && rest[1] == "--json")
                    || selects_session(op, rest, request.payload.as_ref()),
                "native operation requires scoped JSON"
            );
            scoped(&request.payload)?;
        }
        "processes" => {
            ensure!(
                rest.first().is_some_and(|v| session(v)),
                "invalid native session"
            );
            let flags = options(
                &rest[1..],
                &[],
                &[
                    "--output",
                    "--stop",
                    "--run",
                    "--conversation",
                    "--generation",
                    "--archive",
                ],
            )?;
            ensure!(
                flags.contains_key("--output") != flags.contains_key("--stop"),
                "choose one native process action"
            );
            ensure!(
                flags.contains_key("--run") && flags.contains_key("--conversation"),
                "native process identity is required"
            );
            if let Some(id) = flags.get("--archive") {
                ensure!(uuid(id), "invalid archive UUID");
            }
            no_payload()?;
        }
        "attachment" => {
            ensure!(
                rest.first().is_some_and(|v| session(v)),
                "invalid native session"
            );
            if rest.len() == 3 && rest[1] == "--stage" && rest[2] == "--json" {
                scoped(&request.payload)?;
            } else {
                let flags = options(
                    &rest[1..],
                    &[],
                    &[
                        "--request",
                        "--index",
                        "--conversation",
                        "--archive",
                        "--agent",
                    ],
                )?;
                ensure!(
                    flags.get("--request").is_some_and(|v| uuid(v))
                        && flags
                            .get("--index")
                            .is_some_and(|v| v.parse::<u8>().is_ok_and(|n| n < 8))
                        && flags.contains_key("--conversation"),
                    "invalid attachment identity"
                );
                if let Some(id) = flags.get("--archive") {
                    ensure!(uuid(id), "invalid archive UUID");
                }
                no_payload()?;
            }
        }
        "claude" | "codex" | "kimi" | "dsh" | "opencode" => {
            ensure!(
                rest.len() == 7 || rest.len() == 9 || (op=="opencode" && rest.len()==11),
                "native launch shape is not allowed"
            );
            ensure!(
                session(&rest[0])
                    && rest[1] == "--new"
                    && rest[2] == "-n"
                    && session(&rest[3])
                    && rest[4] == "-d"
                    && rest[5] == "--launch-id"
                    && uuid(&rest[6]),
                "native launch shape is not allowed"
            );
            if rest.len() >= 9 {
                ensure!(
                    rest[7] == "--account" && session(&rest[8]),
                    "native launch account is invalid"
                );
            }
            if rest.len()==11 {ensure!(rest[9]=="--model" && rest[10].split_once('/').is_some_and(|(provider,model)|!provider.is_empty() && !model.is_empty()) && text(&rest[10],256,true),"invalid OpenCode model");}
            no_payload()?;
        }
        _ => bail!("native operation is not allowed"),
    }
    Ok(())
}

fn session(value: &str) -> bool {
    text(value, 4096, false) && !value.starts_with(['-', '@'])
}

struct Output {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "cannot prepare mobile transport"
    );
    Ok(())
}

fn read_pipe<T: Read>(pipe: &mut Option<T>, bytes: &mut Vec<u8>, cap: usize) -> Result<()> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(());
    };
    let mut buffer = [0; 16 * 1024];
    // A bounded turn prevents a continuously writing child starving the deadline.
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *pipe = None;
                break;
            }
            Ok(n) => {
                ensure!(bytes.len() + n <= cap, "mobile output is too large");
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// No reader threads or blocking joins: inherited pipe descriptors cannot extend
/// the whole deadline. Only this owned helper/SSH child is stopped on failure.
fn capture_control(
    mut command: Command,
    input: &[u8],
    deadline: Instant,
    cap: usize,
    mode: CaptureMode,
) -> Result<Output> {
    ensure!(Instant::now() < deadline, "mobile helper timed out");
    if let CaptureMode::OwnedSsh {
        parent_pid,
        deadline_ms,
    } = mode
    {
        ensure!(
            crate::platform::parent_process_id() == parent_pid
                && !TRANSPORT_STOP.load(Ordering::Relaxed)
                && monotonic_ms()? < deadline_ms,
            "mobile transport parent changed"
        );
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Discovery workers spawn side by side: a transport started in between
    // would inherit this one's pipes and hold its input open (`spawn_in_turn`).
    let mut child = platform::spawn_in_turn(&mut command).context("mobile helper unavailable")?;
    let result = (|| {
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        nonblocking(stdin.as_ref().unwrap().as_raw_fd())?;
        nonblocking(stdout.as_ref().unwrap().as_raw_fd())?;
        nonblocking(stderr.as_ref().unwrap().as_raw_fd())?;
        let mut sent = 0;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut status = None;
        loop {
            if let CaptureMode::OwnedSsh {
                parent_pid,
                deadline_ms,
            } = mode
            {
                ensure!(
                    !TRANSPORT_STOP.load(Ordering::Relaxed)
                        && crate::platform::parent_process_id() == parent_pid,
                    "mobile transport cancelled; delivery may be uncertain"
                );
                ensure!(
                    monotonic_ms()? < deadline_ms,
                    "mobile helper timed out; delivery may be uncertain"
                );
            }
            ensure!(
                Instant::now() < deadline,
                "mobile helper timed out; delivery may be uncertain"
            );
            if sent == input.len() {
                stdin = None;
            }
            if let Some(writer) = stdin.as_mut() {
                match writer.write(&input[sent..input.len().min(sent + 65536)]) {
                    Ok(n) => sent += n,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                        stdin = None;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            read_pipe(&mut stdout, &mut out, cap)?;
            read_pipe(&mut stderr, &mut err, MAX_ERROR)?;
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status.filter(|_| stdout.is_none() && stderr.is_none()) {
                return Ok(Output {
                    code: platform::code(status),
                    stdout: out,
                    stderr: err,
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        if matches!(mode, CaptureMode::Supervisor) {
            // The supervisor must reap its SSH child before it exits. Killing
            // the supervisor first would destroy that cancellation watchdog.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let grace = Instant::now() + Duration::from_secs(1);
            while Instant::now() < grace {
                if child.try_wait().ok().flatten().is_some() {
                    return result;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn transport(
    via: &str,
    helper: Helper,
    request: Option<LocalRequest>,
    deadline: Instant,
    cap: usize,
) -> Result<Output> {
    // Sample the wire clock first: the conversion may shorten, never renew,
    // the caller's absolute budget while preparation/serialization runs.
    let now_ms = monotonic_ms()?;
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    ensure!(
        remaining > 0 && remaining <= transport_budget(helper, request.as_ref()).as_millis(),
        "invalid mobile transport deadline"
    );
    let envelope = TransportRequest {
        schema: 1,
        via: via.to_owned(),
        helper,
        request,
        parent_pid: std::process::id(),
        deadline_ms: now_ms + remaining as u64,
    };
    let bytes = serde_json::to_vec(&envelope)?;
    ensure!(bytes.len() <= MAX_BYTES, "mobile request is too large");
    let mut command = Command::new(crate::self_exec::current()?);
    command.args(["swarm", "__mobile-peer-transport", "--json"]);
    capture_control(command, &bytes, deadline, cap, CaptureMode::Supervisor)
}

fn supervise() -> Result<i32> {
    // This helper performs no native operations itself. It owns exactly one
    // SSH/identity child and survives cancellation of the gateway wrapper.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            stop_transport as *const () as libc::sighandler_t,
        );
    }
    let envelope: TransportRequest = input()?;
    let started = Instant::now();
    let now = monotonic_ms()?;
    ensure!(
        envelope.schema == 1
            && envelope.parent_pid > 1
            && crate::platform::parent_process_id() == envelope.parent_pid
            && !TRANSPORT_STOP.load(Ordering::Relaxed),
        "mobile transport parent changed"
    );
    ensure!(
        envelope.deadline_ms > now
            && u128::from(envelope.deadline_ms - now)
                <= transport_budget(envelope.helper, envelope.request.as_ref()).as_millis(),
        "invalid mobile transport deadline"
    );
    // The wire clock is rounded down to milliseconds. Discard that partial
    // millisecond so converting back to Instant cannot extend the wire budget.
    let remaining = (envelope.deadline_ms - now).saturating_sub(1);
    let deadline = started + Duration::from_millis(remaining);
    let config = Config::load()?;
    let (command, bytes, cap) = match envelope.helper {
        Helper::OwnIdentity => {
            ensure!(
                envelope.via.is_empty() && envelope.request.is_none(),
                "invalid local identity transport"
            );
            let mut command = Command::new(crate::self_exec::current()?);
            command.args(["swarm", "__mobile-peer-identity", "--json"]);
            (command, vec![], 4096)
        }
        Helper::Help | Helper::Identity => {
            ensure!(
                direct(&config, &envelope.via) && envelope.request.is_none(),
                "invalid mobile transport route"
            );
            let (helper, cap) = if matches!(envelope.helper, Helper::Help) {
                ("~/.local/bin/hgs --help", 256 * 1024)
            } else {
                (IDENTITY_HELPER, 4096)
            };
            (ssh(&config, &envelope.via, helper, deadline)?, vec![], cap)
        }
        Helper::Local => {
            ensure!(
                direct(&config, &envelope.via),
                "invalid mobile transport route"
            );
            let request = envelope
                .request
                .context("missing mobile transport request")?;
            validate(&request)?;
            (
                ssh(&config, &envelope.via, LOCAL_HELPER, deadline)?,
                serde_json::to_vec(&request)?,
                MAX_BYTES,
            )
        }
    };
    ensure!(
        crate::platform::parent_process_id() == envelope.parent_pid
            && !TRANSPORT_STOP.load(Ordering::Relaxed),
        "mobile transport parent changed"
    );
    let output = capture_control(
        command,
        &bytes,
        deadline,
        cap,
        CaptureMode::OwnedSsh {
            parent_pid: envelope.parent_pid,
            deadline_ms: envelope.deadline_ms,
        },
    )?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(output.code)
}

fn ssh(config: &Config, via: &str, helper: &str, deadline: Instant) -> Result<Command> {
    // Each helper is a fixed source constant; request bytes stay on stdin.
    let words: Vec<_> = helper.strip_prefix("~/.local/bin/hgs ").context("invalid fixed peer helper")?.split_whitespace().collect();
    let helper = crate::remote_shell::hgs_within(config, via, &words, deadline)?;
    let mut command = crate::ssh_bytes::command(&helper)?;
    command.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=3",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=1",
    ]);
    command.args(machines::ssh_options(config, via));
    command.arg(via).arg(helper);
    Ok(command)
}

fn supported(via: &str, deadline: Instant) -> Result<()> {
    let output = transport(
        via,
        Helper::Help,
        None,
        deadline.min(Instant::now() + Duration::from_secs(3)),
        256 * 1024,
    )?;
    ensure!(
        output.code == 0 && String::from_utf8_lossy(&output.stdout).contains(CAPABILITY),
        "mobile peer upgrade required"
    );
    Ok(())
}

fn own_identity(config: &Config) -> Result<Identity> {
    ensure!(text(config.host(), 256, false), "invalid machine name");
    Ok(Identity {
        schema: 1,
        machine_id: swarm::local_node_id(config)?,
        name: config.host().to_owned(),
    })
}

fn identity(output: Output) -> Result<Identity> {
    ensure!(output.code == 0, "mobile peer unavailable");
    let identity: Identity = serde_json::from_slice(&output.stdout)?;
    ensure!(
        identity.schema == 1 && uuid(&identity.machine_id) && text(&identity.name, 256, false),
        "invalid mobile peer identity"
    );
    Ok(identity)
}

fn verified(id: &Verified, local: &str) -> bool {
    uuid(&id.machine_id) && id.machine_id != local && text(&id.name, 256, false)
}

/// Identities this gateway verified itself through each configured alias. A
/// damaged record, or one observed under another local identity (a copied or
/// reset configuration), is ignored. It only describes offline routes; it never
/// authorizes dispatch, whose destination verifies its own UUID again.
fn remembered(config: &Config, local: &str) -> BTreeMap<String, Verified> {
    let record = fs::File::open(config.dir.join(REMEMBERED))
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(REMEMBERED_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= REMEMBERED_BYTES).then_some(())?;
            serde_json::from_slice::<Remembered>(&bytes).ok()
        });
    match record {
        Some(record) if record.schema == 1 && record.local == local => record
            .peers
            .into_iter()
            .filter(|(via, id)| alias(via) && verified(id, local))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// Keep currently direct aliases and replace the identities verified just now.
/// A removed or disabled edge forgets its identity. Writes happen only on a
/// change; a writer that keeps the lock beyond a brief wait postpones this one.
fn remember(
    config: &Config,
    local: &str,
    direct: &BTreeSet<String>,
    now: &BTreeMap<String, Verified>,
    previous: &BTreeMap<String, Verified>,
) -> Result<()> {
    let merge = |current: &BTreeMap<String, Verified>| {
        let mut next: BTreeMap<_, _> = current
            .iter()
            .filter(|(via, _)| direct.contains(*via))
            .map(|(via, id)| (via.clone(), id.clone()))
            .collect();
        next.extend(now.iter().map(|(via, id)| (via.clone(), id.clone())));
        next
    };
    if merge(previous) == *previous {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(config.dir.join("swarm"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(config.dir.join(REMEMBERED_LOCK))?;
    // A forked child can briefly share this descriptor's lock before its exec
    // closes it; wait for moments, then let a long-held lock postpone the write.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while let Err(error) = lock.try_lock_exclusive() {
        if !crate::platform::lock_contended(&error) {
            return Err(error.into());
        }
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _lock = crate::platform::FileLock::held(lock);
    let current = remembered(config, local);
    let next = merge(&current);
    if next == current {
        return Ok(());
    }
    let record = Remembered {
        schema: 1,
        local: local.to_owned(),
        peers: next,
    };
    swarm::replace(&config.dir.join(REMEMBERED), &serde_json::to_vec(&record)?)
}

fn discovery(config: &Config) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let local = identity(transport(
        "",
        Helper::OwnIdentity,
        None,
        deadline.min(Instant::now() + Duration::from_secs(3)),
        4096,
    )?)?;
    let direct_peers: BTreeSet<_> = config
        .peers
        .iter()
        .filter(|via| direct(config, via))
        .cloned()
        .collect();
    ensure!(
        direct_peers.len() <= MAX_PEERS,
        "too many direct mobile peers (maximum 32)"
    );
    let peers: Vec<_> = direct_peers.iter().cloned().collect();
    let mut outcomes = Vec::new();
    // Four owned worker threads, each bounded by the shared discovery deadline.
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for worker in 0..4 {
            let sender = sender.clone();
            let peers = &peers;
            scope.spawn(move || {
                for via in peers.iter().skip(worker).step_by(4) {
                    let outcome = (|| {
                        supported(via, deadline)?;
                        identity(transport(via, Helper::Identity, None, deadline.min(Instant::now() + Duration::from_secs(3)), 4096)?)
                    })();
                    if sender.send((via.clone(), outcome.ok())).is_err() { return; }
                }
            });
        }
        drop(sender);
        outcomes.extend(receiver);
    });
    outcomes.sort_by(|a, b| a.0.cmp(&b.0));
    let now: BTreeMap<_, _> = outcomes
        .iter()
        .filter_map(|(via, id)| {
            let id = id.as_ref()?;
            Some((
                via.clone(),
                Verified {
                    machine_id: id.machine_id.clone(),
                    name: id.name.clone(),
                },
            ))
        })
        .filter(|(_, id)| verified(id, &local.machine_id))
        .collect();
    let previous = remembered(config, &local.machine_id);
    if let Err(error) = remember(config, &local.machine_id, &direct_peers, &now, &previous) {
        eprintln!("hgs: cannot remember verified mobile peers: {error:#}");
    }
    // Unreachable peers keep the identity this gateway last verified through
    // the same alias: first its own mobile record, then the local swarm binding
    // pinned by `swarm hello`. Shared catalog data is never identity evidence.
    let mut bindings = None;
    let rows: Vec<_> = outcomes
        .into_iter()
        .map(|(via, outcome)| match outcome {
            Some(id) => json!({"via":via,"machine_id":id.machine_id,"name":id.name,"online":true,"error":null,"identity":"verified"}),
            None => {
                let last = previous.get(&via).cloned().or_else(|| {
                    bindings
                        .get_or_insert_with(|| swarm::verified_bindings(config, &local.machine_id))
                        .get(&via)
                        .map(|id| Verified { machine_id: id.clone(), name: via.clone() })
                        .filter(|id| verified(id, &local.machine_id))
                });
                match last {
                    Some(id) => json!({"via":via,"machine_id":id.machine_id,"name":id.name,"online":false,"error":"unavailable","identity":"last_verified"}),
                    None => json!({"via":via,"machine_id":null,"name":via,"online":false,"error":"unavailable","identity":null}),
                }
            }
        })
        .collect();
    Ok(json!({"schema":1,"local":{"machine_id":local.machine_id,"name":local.name},"peers":rows}))
}

fn local(config: &Config, request: LocalRequest) -> Result<i32> {
    validate(&request)?;
    let machine = if request
        .argv
        .get(..2)
        .is_some_and(|v| v == ["__state", "delivery"])
    {
        crate::state::lifecycle_receipt_machine(config).map_err(anyhow::Error::msg)?
    } else {
        swarm::local_node_id(config)?
    };
    ensure!(
        machine == request.target_machine_id,
        "mobile target identity changed; refresh computers"
    );
    // Rebind stdin to a private anonymous file, then invoke the existing native
    // dispatcher in this same process. No shell, routing, or second hop exists.
    let mut input = tempfile::tempfile()?;
    if let Some(payload) = request.payload {
        serde_json::to_writer(&mut input, &payload)?;
    }
    input.rewind()?;
    ensure!(
        unsafe { libc::dup2(input.as_raw_fd(), libc::STDIN_FILENO) } >= 0,
        "cannot prepare native input"
    );
    cli::dispatch(request.argv).map_err(anyhow::Error::new)
}

fn peer_budget(argv: &[String]) -> Duration {
    let acquisition = argv.first().is_some_and(|op| op == "repository")
        && argv.get(1).is_some_and(|op| op == "clone");
    Duration::from_secs(if acquisition { 80 } else { 30 })
}

fn transport_budget(helper: Helper, request: Option<&LocalRequest>) -> Duration {
    if matches!(helper, Helper::Local) {
        if let Some(request) = request.filter(|request| validate(request).is_ok()) {
            return peer_budget(&request.argv);
        }
    }
    Duration::from_secs(30)
}

/// An agent's `mobile-peer` call (zerus-ppd26.11).
const AGENT_REFUSED: &str = "hgs mobile-peer forwards a phone's request, which the other computer runs as the person; an agent never forwards one";

fn execute(config: &Config, args: &[String]) -> Result<i32> {
    ensure!(
        args.len() == 2 && args[1] == "--json",
        "mobile commands require --json"
    );
    match args[0].as_str() {
        "__mobile-peer-identity" => {
            println!("{}", serde_json::to_string(&own_identity(config)?)?);
            Ok(0)
        }
        "mobile-peers" => {
            println!("{}", discovery(config)?);
            Ok(0)
        }
        "__mobile-peer-local" => local(config, input()?),
        "__mobile-peer-transport" => supervise(),
        "mobile-peer" => {
            // The peer's __mobile-peer-local runs the phone's request as the
            // person, so an agent's process never forwards one; the
            // connector's own call carries no agent marker (zerus-ppd26.11).
            ensure!(!crate::cli::agent_caller(), AGENT_REFUSED);
            let request: PeerRequest = input()?;
            let config = &Config::load()?;
            ensure!(
                direct(config, &request.via),
                "mobile route is not an enabled direct peer"
            );
            let local = LocalRequest {
                schema: request.schema,
                target_machine_id: request.target_machine_id,
                argv: request.argv,
                payload: request.payload,
            };
            validate(&local)?;
            // Only acquisition uses the extended bounded route. Its connector
            // wrapper ends at 75 seconds, before the ordinary 90-second stop.
            let deadline = Instant::now() + peer_budget(&local.argv);
            supported(&request.via, deadline)?;
            // A capability probe may take seconds; a disabled/reconfigured route
            // must not be authorized by the earlier snapshot of local settings.
            let config = &Config::load()?;
            ensure!(
                direct(config, &request.via),
                "mobile route is not an enabled direct peer"
            );
            let max_output = if local.argv.first().is_some_and(|op| op == "analytics" || op == "history-projection") {
                1024 * 1024
            } else { MAX_BYTES };
            let output = transport(
                &request.via,
                Helper::Local,
                Some(local),
                deadline,
                max_output,
            )?;
            std::io::stdout().write_all(&output.stdout)?;
            std::io::stderr().write_all(&output.stderr)?;
            Ok(output.code)
        }
        _ => bail!("unknown mobile command"),
    }
}

pub(crate) fn dispatch(config: &Config, args: &[String]) -> cli::Result<i32> {
    execute(config, args).map_err(|e| cli::Error::new(1, format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn repository_acquisition_only_admits_exact_native_actions() {
        let check = [
            "repository",
            "check",
            "--json",
            "--url",
            "git@forge:repo.git",
            "--parent",
            "/fixture",
            "--name",
            "new checkout",
        ];
        validate(&request(&check, None)).unwrap();
        assert_eq!(
            peer_budget(&request(&check, None).argv),
            Duration::from_secs(30)
        );
        for leaf in ["-literal", "@literal"] {
            let mut literal = check;
            literal[8] = leaf;
            validate(&request(&literal, None)).unwrap();
        }
        let mut clone = check.to_vec();
        clone[1] = "clone";
        clone.extend(["--request-id", ID]);
        validate(&request(&clone, None)).unwrap();
        assert_eq!(
            peer_budget(&request(&clone, None).argv),
            Duration::from_secs(80)
        );
        for argv in [
            ["worktrees", "create"],
            ["repository", "status"],
            ["repository-extra", "clone"],
        ] {
            assert_eq!(
                peer_budget(&request(&argv, None).argv),
                Duration::from_secs(30)
            );
        }
        validate(&request(
            &["repository", "status", "--json", "--request-id", ID],
            None,
        ))
        .unwrap();
        for bad in [
            vec!["repository", "status", "--json"],
            vec!["repository", "clone", "--json"],
            vec!["repository", "worker", "--json"],
            vec!["repository", "status", "--json", "--request-id", "not-uuid"],
        ] {
            assert!(validate(&request(&bad, None)).is_err());
        }
        for extra in [
            vec!["--request-id", ID],
            vec!["--url", "git@forge:other"],
            vec!["--force"],
            vec!["--depth", "1"],
        ] {
            let mut bad = check.to_vec();
            bad.extend(extra);
            assert!(validate(&request(&bad, None)).is_err());
        }
        assert!(validate(&request(&check, Some(json!({"command":"arbitrary"})))).is_err());
    }

    fn request(argv: &[&str], payload: Option<Value>) -> LocalRequest {
        LocalRequest {
            schema: 1,
            target_machine_id: ID.into(),
            argv: argv.iter().map(|v| (*v).into()).collect(),
            payload,
        }
    }

    fn scope() -> Value {
        json!({"request_id":ID,"expected_run_id":"run","expected_conversation_id":"conversation"})
    }

    /// A peer connector learns `--current-read` from the peer's own help and
    /// adds it to every main inspection (zerus-a4mub.9). Like the CLI, which
    /// names a current read only for a main inspection, an archive or
    /// subagent inspection never carries it.
    #[test]
    fn a_main_inspection_may_name_its_current_read() {
        let sid = "11111111-1111-4111-8111-111111111111";
        for argv in [
            vec!["inspect", "codex/project", "--current-read"],
            vec!["inspect", "dsh/project", "--after", "12", "--current-read"],
            vec!["inspect", "opencode/project", "--skip-processes", "--current-read"],
            vec!["inspect", "codex/project", "--session-id", sid, "--current-read"],
        ] {
            validate(&request(&argv, None)).unwrap();
            assert!(validate(&request(&argv, Some(scope()))).is_err(), "{argv:?}");
        }
        for argv in [
            vec!["inspect", "codex/project", "--current-read", "--current-read"],
            vec!["inspect", "codex/project", "--current-read", "yes"],
        ] {
            assert!(validate(&request(&argv, None)).is_err(), "{argv:?}");
        }
    }

    /// The scope check, not just the option list: each of these shapes is
    /// valid without `--current-read`.
    #[test]
    fn an_archive_or_subagent_inspection_never_names_a_current_read() {
        let sid = "11111111-1111-4111-8111-111111111111";
        for (plain, current) in [
            (vec!["inspect", "codex/project", "--archive", ID], vec!["inspect", "codex/project", "--archive", ID, "--current-read"]),
            (vec!["inspect", "codex/project", "--archive", ID], vec!["inspect", "codex/project", "--current-read", "--archive", ID]),
            (vec!["inspect", "codex/project", "--agent", "child", "--skip-processes"],
             vec!["inspect", "codex/project", "--agent", "child", "--skip-processes", "--current-read"]),
            (vec!["inspect", "codex/project", "--agent", "child"], vec!["inspect", "codex/project", "--current-read", "--agent", "child"]),
            (vec!["inspect", "codex/project", "--session-id", sid, "--agent", "child"],
             vec!["inspect", "codex/project", "--session-id", sid, "--agent", "child", "--current-read"]),
        ] {
            validate(&request(&plain, None)).unwrap();
            assert!(validate(&request(&current, None)).is_err(), "{current:?}");
        }
    }

    #[test]
    fn auxiliary_ids_validate_only_the_original_closed_shape_and_never_strip_at_dispatch() {
        let sid = "11111111-1111-4111-8111-111111111111";
        for argv in [
            vec!["inspect", "codex/project"],
            vec![
                "inspect",
                "codex/project",
                "--skip-processes",
                "--after",
                "12",
            ],
        ] {
            let mut selected = argv[..2].to_vec();
            selected.extend(["--session-id", sid]);
            selected.extend_from_slice(&argv[2..]);
            let input = request(&selected, None);
            let unchanged = input.argv.clone();
            validate(&input).unwrap();
            assert_eq!(input.argv, unchanged);
            assert!(
                validate(&request(&argv, Some(scope()))).is_err(),
                "inspect still has no payload"
            );
            assert!(validate(&request(&selected, Some(scope()))).is_err());
        }
        for argv in [
            vec![
                "inspect",
                "codex/project",
                "--session-id",
                sid,
                "--archive",
                ID,
            ],
            vec![
                "inspect",
                "codex/project",
                "--session-id",
                sid,
                "--session-id",
                sid,
            ],
            vec![
                "inspect",
                "codex/project",
                "--skip-processes",
                "--session-id",
                sid,
            ],
            vec!["inspect", "codex/project", "--session-id", "not-uuid"],
            vec![
                "session-action-status",
                "codex/project",
                "--session-id",
                sid,
                "--json",
            ],
            vec!["recovery", "action", "--session-id", sid, "--scoped-json"],
        ] {
            assert!(validate(&request(&argv, None)).is_err(), "{argv:?}");
        }
    }

    /// zerus-ucck P1c-2b: a listed scoped command may select its live session
    /// by a canonical ID in exactly one position; nothing else changes.
    #[test]
    fn a_session_id_is_accepted_only_after_the_session_of_listed_live_operations() {
        const SESSION: &str = "3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
        fn selected(op: &str) -> [&str; 5] {
            [op, "codex/project", "--session-id", SESSION, "--json"]
        }
        let action = |name: &str| {
            let mut payload = scope();
            payload["action"] = json!(name);
            payload
        };
        for op in [
            "send",
            "send-now",
            "answer",
            "interrupt",
            "settings",
            "clear-context",
            "compact-context",
        ] {
            assert!(
                validate(&request(&selected(op), Some(scope()))).is_ok(),
                "{op}"
            );
            // An archived conversation never selects by ID.
            let mut archived = scope();
            archived["archive_id"] = json!(ID);
            assert!(
                validate(&request(&selected(op), Some(archived))).is_err(),
                "{op}"
            );
            assert!(validate(&request(&selected(op), None)).is_err(), "{op}");
        }
        for name in [
            "pause",
            "resume",
            "rename",
            "fork",
            "archive",
            "forget",
            "terminate",
            "remove_queued",
        ] {
            assert!(
                validate(&request(&selected("session-action"), Some(action(name)))).is_ok(),
                "{name}"
            );
            let mut archived = action(name);
            archived["archive_id"] = json!(ID);
            assert!(
                validate(&request(&selected("session-action"), Some(archived))).is_err(),
                "{name}"
            );
        }
        for name in ["restore", "", "unknown"] {
            assert!(
                validate(&request(&selected("session-action"), Some(action(name)))).is_err(),
                "{name}"
            );
        }
        assert!(validate(&request(&selected("session-action"), Some(scope()))).is_err());
        // New label-aware auxiliary reads retain the exact original scoped
        // payload rules. Unlisted operation kinds still refuse the selector.
        for op in [
            "history",
            "history-projection",
            "terminal",
            "file-read",
        ] {
            assert!(
                validate(&request(&selected(op), Some(scope()))).is_ok(),
                "{op}"
            );
        }
        assert!(validate(&request(&selected("effort"), Some(scope()))).is_err());
        // Only a canonical ID, once, right after the session.
        for argv in [
            vec![
                "send",
                "codex/project",
                "--session-id",
                &SESSION.to_uppercase(),
                "--json",
            ],
            vec![
                "send",
                "codex/project",
                "--session-id",
                "not-an-id",
                "--json",
            ],
            vec![
                "send",
                "codex/project",
                "--session-id",
                "00000000-0000-0000-0000-000000000000",
                "--json",
            ],
            vec!["send", "codex/project", "--session-id", "", "--json"],
            vec!["send", "codex/project", "--json", "--session-id", SESSION],
            vec!["send", "--session-id", SESSION, "codex/project", "--json"],
            vec!["send", "--session-id", SESSION, "--json"],
            vec!["send", "codex/project", "--session-id", SESSION],
            vec![
                "send",
                "codex/project",
                "--session-id",
                SESSION,
                "--session-id",
                SESSION,
                "--json",
            ],
            vec!["send", "codex/project", "--session", SESSION, "--json"],
            vec!["send", "@peer", "--session-id", SESSION, "--json"],
            vec!["send", "-x", "--session-id", SESSION, "--json"],
        ] {
            assert!(
                validate(&request(&argv, Some(scope()))).is_err(),
                "{argv:?}"
            );
        }
        // The name form is unchanged.
        assert!(validate(&request(
            &["send", "codex/project", "--json"],
            Some(scope())
        ))
        .is_ok());
        assert!(crate::cli::usage()
            .lines()
            .any(|line| line.trim() == SESSION_ID_MARKER));
    }

    #[test]
    fn peer_fixed_projection_shape_preserves_the_exact_scope() {
        let mut payload = scope();
        payload["history_epoch"] = json!("epoch");
        payload["projection_after"] = json!(0);
        payload["projection_until"] = json!(7);
        let argv = ["history-projection", "codex/project", "--json"];
        assert!(validate(&request(&argv, Some(payload.clone()))).is_ok());
        for argv in [vec!["history-projection"], vec!["history-projection", "codex/project"],
            vec!["history-projection", "codex/project", "--json", "--clear"],
            vec!["history-projection", "@next", "--json"]] {
            assert!(validate(&request(&argv, Some(payload.clone()))).is_err());
        }
        for payload in [None, Some(json!({})), Some(Value::Null)] {
            assert!(validate(&request(&argv, payload)).is_err());
        }
        let mut wrong = request(&argv, Some(payload));
        wrong.target_machine_id = "not-a-machine-uuid".into();
        assert!(validate(&wrong).is_err());
    }

    #[test]
    fn analytics_peer_reads_cannot_enable_purge_or_extend_the_range() {
        let argv = ["analytics", "--json"];
        let payload = json!({"schema":1,"action":"summary","from":"2026-01-01","through":"2026-03-31"});
        assert!(validate(&request(&argv, Some(payload.clone()))).is_ok());
        for (key, value) in [("action", json!("purge")), ("action", json!("configure")),
            ("enabled", json!(true)), ("through", json!("2026-04-01")),
            ("schema", json!(true)), ("path", json!("/fixture/private"))] {
            let mut bad = payload.clone();
            bad[key] = value;
            assert!(validate(&request(&argv, Some(bad))).is_err());
        }
        for argv in [vec!["analytics"], vec!["analytics", "--json", "@next"],
            vec!["analytics", "--json", "--purge"]] {
            assert!(validate(&request(&argv, Some(payload.clone()))).is_err());
        }
        assert!(validate(&request(&["analytics", "--json"], None)).is_err());
    }

    #[test]
    fn archive_recovery_admits_only_exact_metadata_lookup() {
        let argv = ["archives", "lookup", "--json"];
        let payload = json!({"name":"codex/example/old","archive_id":ID});
        assert!(validate(&request(&argv, Some(payload.clone()))).is_ok());
        for bad in [json!({"name":"@peer","archive_id":ID}), json!({"name":"codex/example/old","archive_id":"bad"}),
                    json!({"name":"codex/example/old","archive_id":ID,"action":"forget"})] {
            assert!(validate(&request(&argv, Some(bad))).is_err());
        }
        assert!(validate(&request(&["archives", "lookup", "--json", "--all"], Some(payload))).is_err());
        assert!(validate(&request(&argv, None)).is_err());
    }

    #[test]
    fn the_phone_s_task_exchange_admits_only_its_two_json_commands() {
        let payload = || Some(json!({"schema": 1, "project": "project-1", "inventory": {"project": {}, "tasks": {}}}));
        assert!(validate(&request(&["task", "exchange", "--json"], payload())).is_ok());
        assert!(validate(&request(&["task", "digests", "--json"], None)).is_ok());
        for action in ["mobile-send", "mobile-cancel", "mobile-done"] {
            assert!(validate(&request(&["task", action, "--json"], payload())).is_ok(), "{action}");
        }
        for (args, body) in [
            (vec!["task", "exchange"], payload()),
            (vec!["task", "exchange", "--json"], None),
            (vec!["task", "exchange", "--json", "extra"], payload()),
            (vec!["task", "digests", "--json"], payload()),
            (vec!["task", "send", "T-a1b2c3", "--json"], None),
            (vec!["task", "list", "--project", "p"], None),
            (vec!["task", "mobile-send"], payload()),
            (vec!["task", "mobile-done", "--json"], None),
            (vec!["task", "mobile-cancel", "--json", "extra"], payload()),
        ] {
            assert!(validate(&request(&args, body)).is_err(), "{args:?}");
        }
    }

    #[test]
    fn directory_creation_admits_only_one_exact_computer_scoped_mkdir() {
        let argv = ["dirs", "--create", "/fixture/new ' проект", "--request-id", ID];
        assert!(validate(&request(&argv, None)).is_ok());
        assert!(validate(&request(&argv, Some(scope()))).is_err());
        for path in ["relative", "~/project", "/", "/fixture/new/", "/fixture/.", "/fixture/..", "/fixture/bad\npath"] {
            let mut bad = argv; bad[2] = path;
            assert!(validate(&request(&bad, None)).is_err(), "{path:?}");
        }
        for args in [
            vec!["dirs", "--create", "/fixture/new"],
            vec!["dirs", "--create", "/fixture/new", "--request-id", "bad"],
            vec!["dirs", "--create", "/fixture/new", "--request-id", ID, "--parents"],
            vec!["dirs", "--create", "/fixture/new", "--request-id", ID, "--request-id", ID],
        ] { assert!(validate(&request(&args, None)).is_err()); }
    }

    #[test]
    fn opencode_native_launch_admits_only_its_fixed_model_and_account_shape() {
        let valid=["opencode","/fixture/project","--new","-n","fixture","-d","--launch-id",ID,"--account","native-opencode","--model","ollama/qwen3-coder:30b"];
        assert!(validate(&request(&valid,None)).is_ok());
        for agent in ["codex","dsh","sh"] {let mut wrong=valid;wrong[0]=agent;assert!(validate(&request(&wrong,None)).is_err());}
        let mut wrong=valid;wrong[10]="--hostname";assert!(validate(&request(&wrong,None)).is_err());
        let mut wrong=valid;wrong[11]="invalid";assert!(validate(&request(&wrong,None)).is_err());
    }
    #[test]
    fn supports_existing_connector_shapes_without_global_dispatch() {
        for argv in [
            vec!["--help"],
            vec!["ls", "--json", "--local"],
            vec!["swarm", "get"],
            vec!["swarm", "hello"],
            vec!["account", "ls"],
            vec!["account", "inspect", "native-codex"],
            vec!["account", "inspect", "--session", "codex/project"],
            vec!["dirs", "/fixture/ordinary folder"],
            vec![
                "inspect",
                "codex/project",
                "--after",
                "2",
                "--agent",
                "child",
                "--skip-processes",
            ],
            vec![
                "processes",
                "codex/project",
                "--output",
                "process",
                "--run",
                "run",
                "--conversation",
                "conversation",
            ],
            vec![
                "processes",
                "codex/project",
                "--stop",
                "process",
                "--run",
                "run",
                "--conversation",
                "",
                "--generation",
                "generation",
            ],
            vec![
                "attachment",
                "codex/project",
                "--request",
                ID,
                "--index",
                "0",
                "--conversation",
                "conversation",
            ],
            vec![
                "codex",
                "/fixture/project",
                "--new",
                "-n",
                "fixture",
                "-d",
                "--launch-id",
                ID,
            ],
            vec![
                "kimi",
                "/fixture/project",
                "--new",
                "-n",
                "fixture",
                "-d",
                "--launch-id",
                ID,
                "--account",
                "fixture-account",
            ],
        ] {
            assert!(validate(&request(&argv, None)).is_ok(), "{argv:?}");
        }
        for op in [
            "history",
            "terminal",
            "session-action",
            "send",
            "send-now",
            "settings",
            "answer",
            "interrupt",
            "clear-context",
            "compact-context",
            "effort",
            "file-read",
        ] {
            assert!(
                validate(&request(&[op, "codex/project", "--json"], Some(scope()))).is_ok(),
                "{op}"
            );
            assert!(
                validate(&request(&[op, "codex/project", "--json"], None)).is_err(),
                "{op}"
            );
        }
        assert!(validate(&request(
            &["attachment", "codex/project", "--stage", "--json"],
            Some(scope())
        ))
        .is_ok());
        let mut recovery = scope();
        recovery["name"] = json!("codex/project");
        assert!(validate(&request(
            &["recovery", "action", "--scoped-json"],
            Some(recovery)
        ))
        .is_ok());
        let mut assignment = scope();
        assignment["name"] = json!("codex/project");
        assert!(validate(&request(
            &["swarm", "assign-launch", "--json"],
            Some(assignment)
        ))
        .is_ok());
        assert!(validate(&request(&["swarm", "assign-launch", "--json"], None)).is_err());
    }

    #[test]
    fn project_grouping_peer_calls_keep_exact_fixed_commands_and_machine() {
        for command in ["session-project","assign-session"] {
            let argv=["swarm",command,"--json"];
            let mut payload=scope();payload["name"]=json!("codex/example/task");payload["expected_machine_id"]=json!(ID);
            if command=="assign-session" { payload["swarm_id"]=json!(ID);payload["project_id"]=json!("logical");payload["membership_token"]=json!("a".repeat(64));payload["catalog_epoch"]=json!("b".repeat(64)); }
            assert!(validate(&request(&argv,Some(payload.clone()))).is_ok());
            for (field,value) in [("directory",json!("/unexpected")),("organization",json!({})),("name",json!("@peer")),("expected_machine_id",json!("22222222-2222-4222-8222-222222222222"))] {
                let mut bad=payload.clone();bad[field]=value;assert!(validate(&request(&argv,Some(bad))).is_err());
            }
            assert!(validate(&request(&["swarm",command,"--json","@next"],Some(payload))).is_err());
            assert!(validate(&request(&argv,None)).is_err());
        }
    }

    #[test]
    fn voice_peer_calls_cannot_start_workers_or_use_client_routes() {
        let mut value=scope();value["action"]=json!("profiles");
        assert!(validate(&request(&["voice","codex/project","--json"],Some(value.clone()))).is_ok());
        assert!(validate(&request(&["voice","worker"],Some(value.clone()))).is_err());
        value["action"]=json!("submit");
        value["speech"]=json!({"machine_id":"client-alias","profile_id":"fixture"});
        value["audio"]=json!({"format":"pcm_s16le","sample_rate":16000,"channels":1,"data_base64":"AAAAAA=="});
        assert!(validate(&request(&["voice","codex/project","--json"],Some(value.clone()))).is_err());
        value["speech"]["machine_id"]=json!(ID);
        assert!(validate(&request(&["voice","codex/project","--json"],Some(value))).is_ok());
    }

    #[test]
    fn shared_delivery_setting_accepts_only_exact_known_values() {
        let set = ["swarm", "preference", "set", "mobile_delivery", "away_10", "--json"];
        assert!(validate(&request(&set, None)).is_ok());
        assert!(validate(&request(&set, Some(json!({})))).is_err());
        for argv in [
            vec!["swarm", "preference", "set", "mobile_delivery", "later", "--json"],
            vec!["swarm", "preference", "set", "theme", "away", "--json"],
            vec!["swarm", "preference", "set", "mobile_delivery", "away"],
            vec!["swarm", "preference", "get", "--json"],
        ] {
            assert!(validate(&request(&argv, None)).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn git_status_admits_only_an_absolute_bounded_native_path() {
        let prefix = if cfg!(windows) {
            "C:\\fixture\\"
        } else {
            "/fixture/"
        };
        let path = format!("{prefix}checkout ' проект");
        for argv in [
            vec!["git-status", "--path", path.as_str(), "--json"],
            vec!["git-status", "--json", "--path", path.as_str()],
        ] {
            assert!(validate(&request(&argv, None)).is_ok(), "{argv:?}");
        }
        let mut longest = format!("{prefix}{}", "é".repeat((4096 - prefix.len()) / 2));
        if longest.len() < 4096 {
            longest.push('a');
        }
        assert_eq!(longest.len(), 4096);
        assert!(validate(&request(
            &["git-status", "--path", &longest, "--json"],
            None
        ))
        .is_ok());
        longest.push('é');
        assert!(longest.chars().count() < 4096);
        assert!(validate(&request(
            &["git-status", "--path", &longest, "--json"],
            None
        ))
        .is_err());
    }

    #[test]
    fn git_status_refuses_payloads_relative_paths_and_extra_options() {
        let path = if cfg!(windows) {
            "C:\\fixture\\checkout"
        } else {
            "/fixture/checkout"
        };
        let read = ["git-status", "--path", path, "--json"];
        for payload in [scope(), json!({}), Value::Null] {
            assert!(validate(&request(&read, Some(payload))).is_err());
        }
        for invalid in [
            "",
            "relative",
            "~/checkout",
            "@peer",
            "-option",
            "/fixture/bad\npath",
            "/fixture/bad\tpath",
            "/fixture/bad\u{85}path",
        ] {
            assert!(
                validate(&request(&["git-status", "--path", invalid, "--json"], None)).is_err(),
                "{invalid:?}"
            );
        }
        for argv in [
            vec!["git-status"],
            vec!["git-status", "--json"],
            vec!["git-status", "--path", path],
            vec!["git-status", "--path"],
            vec!["git-status", "--path", path, "--json", "--json"],
            vec!["git-status", "--path", path, "--json", "--path", path],
            vec!["git-status", "--path", path, "--json", "--refresh"],
            vec!["git-status", "--path", path, "--json", "--git-dir", path],
            vec!["git-status", "--path", path, "--json", "extra"],
        ] {
            assert!(validate(&request(&argv, None)).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn worktree_operations_are_exact_and_creation_is_bounded() {
        let read = ["worktrees", "--path", "/repo", "--refresh", "--json"];
        assert!(validate(&request(&read, None)).is_ok());
        let create = [
            "worktrees",
            "create",
            "--path",
            "/repo",
            "--common-dir",
            "/repo/.git",
            "--destination",
            "/linked",
            "--branch",
            "feature",
            "--base",
            "HEAD",
            "--request-id",
            ID,
            "--json",
            "--mobile",
        ];
        assert!(validate(&request(&create, None)).is_ok());
        assert!(validate(&request(&create[..create.len() - 1], None)).is_err());
        let mut extra = create.to_vec();
        extra.extend(["--shell", "bad"]);
        assert!(validate(&request(&extra, None)).is_err());
        assert!(validate(&request(&read, Some(scope()))).is_err());
    }

    #[test]
    fn denies_forwarding_secrets_shell_and_unscoped_mutations() {
        for argv in [
            vec!["@peer", "send", "codex/project", "--json"],
            vec!["--run", "sh"],
            vec!["sh", "-c", "echo fixture"],
            vec!["__state", "run", "sh"],
            vec!["mobile-peer", "--json"],
            vec!["__mobile-peer-local", "--json"],
            vec!["ls", "--json"],
            vec!["ls", "--json", "--local", "--json"],
            vec!["swarm", "inventory"],
            vec!["swarm", "sync"],
            vec!["swarm", "export"],
            vec!["account", "export", "fixture"],
            vec!["account", "copy", "fixture", "peer"],
            vec!["account", "inspect", "fixture", "--refresh"],
            vec!["send", "codex/project", "text"],
            vec!["processes", "codex/project", "--stop", "process"],
            vec!["inspect", "@peer"],
            vec!["inspect", "codex/project", "--dry-run"],
            vec![
                "codex",
                "/fixture/project",
                "--new",
                "-n",
                "fixture",
                "-d",
                "--launch-id",
                ID,
                "--",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
            vec![
                "attachment",
                "codex/project",
                "--request",
                ID,
                "--index",
                "8",
                "--conversation",
                "conversation",
            ],
        ] {
            assert!(validate(&request(&argv, None)).is_err(), "{argv:?}");
        }
        for field in ["via", "path", "depth", "command", "expected_machine_id"] {
            let mut value = serde_json::to_value(request(&["--help"], None)).unwrap();
            value[field] = json!("fixture");
            assert!(
                serde_json::from_value::<LocalRequest>(value).is_err(),
                "{field}"
            );
        }
        for route in [
            "-oProxyCommand=fixture",
            "peer;fixture",
            "user@peer",
            "peer/child",
            "peer\nchild",
            "",
        ] {
            assert!(!alias(route));
        }
    }

    #[test]
    fn read_marks_require_an_exact_scoped_payload() {
        let mark = json!({"run_id":"run","conversation_id":"conversation","reply_id":null,"attention_signature":"a".repeat(64)});
        assert!(validate(&request(&["read", "codex/project", "--json"], Some(mark.clone()))).is_ok());
        assert!(validate(&request(&["read", "codex/project", "--json"], None)).is_err());
        assert!(validate(&request(&["read", "@peer", "--json"], Some(mark.clone()))).is_err());
        assert!(validate(&request(&["read", "codex/project"], Some(mark.clone()))).is_err());
        let mut extra = mark.clone();
        extra["request_id"] = json!(ID);
        assert!(validate(&request(&["read", "codex/project", "--json"], Some(extra))).is_err());
        let mut empty = mark;
        empty["run_id"] = json!("");
        assert!(validate(&request(&["read", "codex/project", "--json"], Some(empty))).is_err());
    }

    #[test]
    fn notifications_require_exact_non_read_payload_before_peer_dispatch() {
        let preference = json!({"run_id":"run","conversation_id":"conversation","muted":true,"expected_revision":0});
        let argv = ["notifications", "codex/project", "--json"];
        assert!(validate(&request(&argv, Some(preference.clone()))).is_ok());
        for args in [vec!["notifications", "codex/project"], vec!["notifications", "@peer", "--json"],
            vec!["notifications", "codex/project", "--json", "--archive", ID]] {
            assert!(validate(&request(&args, Some(preference.clone()))).is_err());
        }
        assert!(validate(&request(&argv, None)).is_err());
        for (key, value) in [("muted", json!(1)), ("expected_revision", json!(true)),
            ("expected_revision", json!(-1)), ("expected_revision", json!(0.0)),
            ("run_id", json!("é".repeat(65))), ("conversation_id", json!("")), ("request_id", json!(ID))] {
            let mut bad = preference.clone(); bad[key] = value;
            assert!(validate(&request(&argv, Some(bad))).is_err());
        }
    }

    fn fixture_config(dir: &std::path::Path) -> Config {
        Config {
            home: dir.to_str().unwrap().into(),
            dir: dir.join("config"),
            state: dir.join("state"),
            selves: vec!["example-local".into()],
            peers: vec!["peer".into(), "other".into()],
            base_peers: vec![],
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        }
    }

    fn known(machine_id: &str, name: &str) -> Verified {
        Verified {
            machine_id: machine_id.into(),
            name: name.into(),
        }
    }

    #[test]
    fn remembered_identities_follow_direct_edges_and_the_observing_identity() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let config = fixture_config(temp.path());
        let local = uuid::Uuid::new_v4().to_string();
        let peer = uuid::Uuid::new_v4().to_string();
        let path = config.dir.join(REMEMBERED);
        let direct: BTreeSet<String> = ["peer".into(), "other".into()].into();
        let now = BTreeMap::from([("peer".to_string(), known(&peer, "Peer"))]);
        remember(&config, &local, &direct, &now, &BTreeMap::new()).unwrap();
        let previous = remembered(&config, &local);
        assert_eq!(previous, now);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Unchanged observations do not rewrite the record.
        let inode = fs::metadata(&path).unwrap().ino();
        remember(&config, &local, &direct, &now, &previous).unwrap();
        remember(&config, &local, &direct, &BTreeMap::new(), &previous).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        // Another local identity (a copied or reset configuration) inherits nothing.
        assert!(remembered(&config, &uuid::Uuid::new_v4().to_string()).is_empty());
        // A removed or disabled edge forgets its identity.
        let only_other: BTreeSet<String> = ["other".into()].into();
        remember(&config, &local, &only_other, &BTreeMap::new(), &previous).unwrap();
        assert!(remembered(&config, &local).is_empty());
        // Invalid entries are never reported, and damaged records report none.
        for record in [
            json!({"schema":1,"local":local,"peers":{"peer":{"machine_id":local,"name":"Self"}}}),
            json!({"schema":1,"local":local,"peers":{"-oProxyCommand=x":{"machine_id":peer,"name":"Peer"}}}),
            json!({"schema":1,"local":local,"peers":{"peer":{"machine_id":"not-a-uuid","name":"Peer"}}}),
            json!({"schema":1,"local":local,"peers":{"peer":{"machine_id":peer,"name":"bad\nname"}}}),
            json!({"schema":2,"local":local,"peers":{"peer":{"machine_id":peer,"name":"Peer"}}}),
            json!({"schema":1,"local":local,"peers":{"peer":{"machine_id":peer,"name":"Peer","via":"x"}}}),
        ] {
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(remembered(&config, &local).is_empty(), "{record}");
        }
        fs::write(&path, b"{damaged").unwrap();
        assert!(remembered(&config, &local).is_empty());
        let mut large = serde_json::to_vec(
            &json!({"schema":1,"local":local,"peers":{"peer":{"machine_id":peer,"name":"Peer"}}}),
        )
        .unwrap();
        large.extend(std::iter::repeat_n(b' ', REMEMBERED_BYTES as usize));
        fs::write(&path, large).unwrap();
        assert!(remembered(&config, &local).is_empty());
        // A damaged record is replaced by the next verified observation.
        remember(&config, &local, &direct, &now, &BTreeMap::new()).unwrap();
        assert_eq!(remembered(&config, &local), now);
    }

    #[test]
    fn only_verified_local_swarm_bindings_are_identity_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let config = fixture_config(temp.path());
        let local = uuid::Uuid::new_v4().to_string();
        let (peer, placeholder) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        fs::create_dir_all(config.dir.join("swarm")).unwrap();
        let store = json!({"node_id":local,"catalog":{"shared":"ignored"},"bindings":{
            "peer":{"id":peer,"verified":true},
            "other":{"id":placeholder,"verified":false},
            "example-local":{"id":local,"verified":true},
            "loop":{"id":local,"verified":true}}});
        fs::write(
            config.dir.join("swarm/catalog.json"),
            serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();
        assert_eq!(
            swarm::verified_bindings(&config, &local),
            BTreeMap::from([("peer".to_string(), peer)])
        );
        assert!(swarm::verified_bindings(&config, &placeholder).is_empty());
        fs::write(config.dir.join("swarm/catalog.json"), b"{damaged").unwrap();
        assert!(swarm::verified_bindings(&config, &local).is_empty());
    }

    #[test]
    fn a_transport_never_inherits_pipes_another_worker_is_still_creating() {
        // This thread stands in for another discovery worker that holds the
        // spawn turn between creating its pipes and marking them close-on-exec,
        // as Rust does on macOS. High numbers keep its pipe apart from the
        // descriptor the listing opens.
        let turn = platform::spawn_turn();
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let creating: Vec<i32> = fds
            .iter()
            .map(|fd| unsafe {
                let high = libc::fcntl(*fd, libc::F_DUPFD, 64);
                libc::close(*fd);
                high
            })
            .collect();
        assert!(creating.iter().all(|fd| *fd >= 64), "{creating:?}");
        let listing = thread::spawn(|| {
            let mut command = Command::new("ls");
            command.arg("/dev/fd");
            let deadline = Instant::now() + Duration::from_secs(30);
            capture_control(command, &[], deadline, 4096, CaptureMode::Supervisor)
        });
        thread::sleep(Duration::from_millis(200));
        for fd in &creating {
            assert_eq!(
                unsafe { libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        drop(turn);
        let output = listing.join().unwrap().unwrap();
        assert_eq!(output.code, 0);
        let open: Vec<i32> = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter_map(|fd| fd.parse().ok())
            .collect();
        assert!(open.contains(&0), "{open:?}");
        for fd in creating {
            assert!(!open.contains(&fd), "transport inherited {fd}: {open:?}");
            unsafe { libc::close(fd) };
        }
    }
}
