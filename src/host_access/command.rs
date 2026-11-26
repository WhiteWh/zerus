//! `hgs [@host] host-access ...`: the multi-person access routes.
//!
//! ```text
//! host-access status --json                      mode and caller (no secrets)
//! host-access bind --json                        the person binds this state folder
//! host-access support --json                     provider support matrix
//! host-access share publish|memory|list|read|continue --json   (JSON stdin)
//! host-access compute --json                     one mediated file/process call (JSON stdin)
//! host-access remote-tools serve --host H --root R            stdio MCP server
//! host-access remote-compute claude-args --host H --root R --json
//! ```
use super::gate::{self, Refusal};
use super::runtime::{self, Mode};
use super::{compute, providers, shared};
use crate::cli::{Error, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).map(String::as_str)
}

fn stdin_json(limit: usize) -> std::result::Result<Vec<u8>, Refusal> {
    let mut bytes = Vec::new();
    std::io::stdin().take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| Refusal::new("request-invalid", "stdin unreadable"))?;
    if bytes.len() > limit {
        return Err(Refusal::new("too-large", "the request exceeds its bound"));
    }
    Ok(crate::state::without_bom(&bytes).to_vec())
}

fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> std::result::Result<T, Refusal> {
    serde_json::from_slice(bytes).map_err(|_| Refusal::new("request-invalid", "malformed request; unknown fields are refused"))
}

/// The status view: never a key, path to a credential or token.
pub fn status(mode: &Mode, state_root: &Path) -> Value {
    let mut view = json!({"capability":gate::CAPABILITY,"contract":super::CONTRACT});
    match mode {
        Mode::Legacy => view["mode"] = json!("legacy"),
        Mode::Broken(code) => {
            view["mode"] = json!("broken");
            view["reason"] = json!(code);
        }
        Mode::Unbound { policy, .. } => {
            view["mode"] = json!("unbound");
            view["policy_revision"] = json!(policy.revision);
        }
        Mode::Bound { policy, principal } => {
            view["mode"] = json!("bound");
            view["person"] = json!(principal.person);
            view["label"] = json!(policy.labels.get(&principal.person));
            view["context"] = json!(principal.context);
            view["policy_revision"] = json!(policy.revision);
            let projects: Vec<Value> = policy
                .projects
                .iter()
                .filter_map(|(project, members)| members.get(&principal.person).map(|role| json!({"project":project,"role":role.name()})))
                .collect();
            view["projects"] = json!(projects);
            view["bound"] = json!(matches!(gate::read_binding(state_root), Ok(Some(_))));
        }
    }
    view
}

fn print(value: Value) -> Result<i32> {
    println!("{value}");
    Ok(0)
}

pub fn dispatch(args: &[String], dry: bool) -> Result<i32> {
    if dry {
        return Err(Error::new(1, "host-access does not support --dry-run"));
    }
    let mode = runtime::current();
    let state_root = crate::state::state_root();
    let op = args.first().map(String::as_str).unwrap_or("");
    match (op, &args[1.min(args.len())..]) {
        ("status", [json]) if json == "--json" => print(status(&mode, &state_root)),
        ("support", [json]) if json == "--json" => print(providers::matrix()),
        ("bind", [json]) if json == "--json" => {
            if crate::cli::agent_caller() {
                return Err(Refusal::new("person-only", "binding a state folder to a person is the person's own action").into());
            }
            let binding = gate::bind(&mode, &state_root, now())?;
            print(serde_json::to_value(binding).expect("binding serializes"))
        }
        ("share", [action, json]) if json == "--json" => share(&mode, &state_root, action),
        ("compute", [json]) if json == "--json" => print(compute::serve_stdin(&mode, &state_root)?),
        ("remote-tools", rest) if rest.first().is_some_and(|a| a == "serve") => {
            let host = flag(rest, "--host").ok_or_else(|| Error::new(2, "remote-tools serve needs --host"))?;
            let root = flag(rest, "--root").ok_or_else(|| Error::new(2, "remote-tools serve needs --root"))?;
            let hgs = crate::self_exec::current()?;
            compute::serve_mcp(&hgs, host, root)?;
            Ok(0)
        }
        ("remote-compute", rest) if rest.first().is_some_and(|a| a == "claude-args") && rest.last().is_some_and(|a| a == "--json") => {
            let host = flag(rest, "--host").ok_or_else(|| Error::new(2, "claude-args needs --host"))?;
            let root = flag(rest, "--root").ok_or_else(|| Error::new(2, "claude-args needs --root"))?;
            let hgs = crate::self_exec::current()?;
            print(json!({"argv":compute::claude_args(&hgs, host, root)?,"mode":"local-agent-remote-compute"}))
        }
        _ => Err(Error::new(
            2,
            "usage: hgs [@host] host-access status|support|bind --json | share publish|memory|list|read|continue --json | compute --json | remote-tools serve --host H --root R | remote-compute claude-args --host H --root R --json",
        )),
    }
}

fn share(mode: &Mode, state_root: &Path, action: &str) -> Result<i32> {
    let principal = gate::require_bound(mode, state_root)?;
    let policy = mode.policy().expect("bound mode has a policy");
    let shared_root = runtime::operator_dir().join("shared");
    runtime::operator_owned_dir(&shared_root)
        .map_err(|code| Refusal::new("shared-root-untrusted", format!("the operator's shared project root is not protected ({code})")))?;
    let store = shared::Store { shared_root, state_root: state_root.to_path_buf(), policy, principal: &principal, now: now() };
    let body = stdin_json(2 * 1024 * 1024)?;
    let value = match action {
        "publish" => {
            let request: shared::PublishRequest = parse(&body)?;
            // Provenance comes from the caller's own state; a session that is
            // not theirs cannot be named.
            crate::state::session_operations().map_err(|e| Error::new(1, e))?;
            let session_id = crate::state::session_id_of(&request.session)
                .ok_or_else(|| Refusal::new("session-unknown", "name one of your own live sessions"))?;
            store.publish(&request, &body, &session_id)?
        }
        "memory" => store.add_memory(&parse(&body)?, &body)?,
        "list" => store.list(&parse(&body)?)?,
        "read" => store.read(&parse(&body)?)?,
        "continue" => store.continue_with_own_account(&parse(&body)?, &body)?,
        _ => return Err(Error::new(2, "usage: hgs host-access share publish|memory|list|read|continue --json")),
    };
    print(value)
}
