//! Shared metadata only. Transport credentials and per-device presentation stay
//! local; the swarm-wide phone delivery choice is shared. Writes use one
//! locked, atomically replaced store; initialized snapshots read it lock-free.
mod bootstrap;
mod catalog;
mod launch_project;
mod infer_project;
pub(crate) use infer_project::{directory_project, infer_new_session};
mod session_project;
#[cfg(unix)]
pub(crate) use session_project::validate_payload as validate_session_project;
mod model;
mod replica;
mod service;
mod store;
pub(crate) use store::read_snapshot as read_catalog_snapshot;
mod transport;

use crate::{cli, config::Config};
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::io::Read;
use store::LockedStore;

#[cfg(unix)]
pub(crate) use catalog::MOBILE_DELIVERY;
pub(crate) use replica::{exchange_project as exchange_tasks, replicate};
#[cfg(unix)]
pub(crate) use store::replace;

/// Machine UUIDs this computer itself obtained through each configured SSH
/// alias (`swarm hello`/`export`, then pinned by `bind`). Bindings stay local
/// and are never exchanged. Shared catalog data, including machine names,
/// redirects and the `machines` listing, comes from other members and is not
/// identity evidence. Lock-free like `preferences`; a damaged or foreign store
/// reports none.
#[cfg(unix)]
pub(crate) fn verified_bindings(
    config: &Config,
    local: &str,
) -> std::collections::BTreeMap<String, String> {
    #[derive(serde::Deserialize)]
    struct Local {
        node_id: String,
        #[serde(default)]
        bindings: std::collections::BTreeMap<String, store::Binding>,
    }
    std::fs::read(config.dir.join("swarm/catalog.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Local>(&bytes).ok())
        .filter(|store| store.node_id == local)
        .map(|store| {
            store
                .bindings
                .into_iter()
                .filter(|(alias, binding)| {
                    binding.verified && !config.is_self(alias) && binding.id != local
                })
                .map(|(alias, binding)| (alias, binding.id))
                .collect()
        })
        .unwrap_or_default()
}

/// `hgs ARGS…` on the swarm member behind `alias`; its JSON answer.
pub(crate) fn remote_hgs(config: &Config, alias: &str, args: &[String]) -> Result<Value> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    transport::run(config, alias, &words, None)
}

pub(crate) fn remote_hgs_input(config: &Config, alias: &str, args: &[String], input: &Value) -> Result<Value> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    transport::run(config, alias, &words, Some(input))
}

/// Shared preferences for `hgs ls --json --local`. Lock-free: the store is
/// replaced atomically and listings never wait for a sync. A missing or
/// unreadable catalog reports the defaults.
/// `{running, current}` for the background catalog worker.
pub(crate) fn worker_status(config: &Config) -> Value {
    service::status(config).json()
}

/// The catalog as `swarm get` shows it, with `worker`. Only a first read on a
/// computer without Qt settings may bootstrap; initialized reads remain
/// lock-free, without saving or supervising the worker.
pub(crate) fn organization(config: &Config, first: bool) -> Result<Value> {
    let mut snapshot = if first { bootstrap::read_snapshot(config)? } else { store::read_snapshot(config)? };
    snapshot["worker"] = service::status(config).json();
    Ok(snapshot)
}

/// The files [`organization`] reads, for a reader that follows changes,
/// with the worker's marker, which a restarted worker rewrites.
pub(crate) fn organization_files(config: &Config) -> Vec<std::path::PathBuf> {
    let mut files = store::snapshot_files(config);
    files.push(config.dir.join("swarm").join(service::MARKER));
    files
}

pub(crate) fn preferences(config: &Config) -> Value {
    std::fs::read(config.dir.join("swarm/catalog.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<store::Store>(&bytes).ok())
        .map_or_else(catalog::default_preferences, |store| {
            store.catalog.preferences()
        })
}

fn preference(config: &Config, args: &[String]) -> Result<Value> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let args = args.strip_suffix(&["--json"]).unwrap_or(&args[..]);
    match args {
        ["get"] => Ok(LockedStore::load(config)?.data.catalog.preferences()),
        ["set", field, value] => {
            let mut store = LockedStore::load(config)?;
            let actor = store.data.node_id.clone();
            store
                .data
                .catalog
                .set(&actor, catalog::Key::preference(field), json!(value))?;
            store.save()?;
            let mut result = store.data.catalog.preferences();
            result["ok"] = json!(true);
            Ok(result)
        }
        _ => bail!("usage: hgs swarm preference get --json | set mobile_delivery VALUE --json"),
    }
}

/// Live catalog projects as (ID, name), in the shared order.
pub(crate) fn projects(config: &Config) -> Result<Vec<(String, String)>> {
    let snapshot = LockedStore::load(config)?.snapshot(config);
    Ok(snapshot["organization"]["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?.to_owned(), p["name"].as_str().unwrap_or_default().to_owned())))
        .collect())
}

/// The live catalog project (ID, name) that this computer's session `name`
/// belongs to, if any.
pub(crate) fn session_project(config: &Config, name: &str) -> Result<Option<(String, String)>> {
    let snapshot = LockedStore::load(config)?.snapshot(config);
    // The snapshot names this computer's sessions "<this host>\n<session>".
    let member = format!("{}\n{name}", config.host());
    Ok(snapshot["organization"]["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["sessions"].as_array().is_some_and(|all| all.iter().any(|s| s.as_str() == Some(member.as_str()))))
        .and_then(|p| Some((p["id"].as_str()?.to_owned(), p["name"].as_str().unwrap_or_default().to_owned()))))
}

/// Persist and return the machine identity without retaining the catalog lock.
pub(crate) fn local_node_id(config: &Config) -> Result<String> {
    let mut store = LockedStore::load(config)?;
    let id = uuid::Uuid::parse_str(&store.data.node_id)?;
    ensure!(
        !id.is_nil() && id.to_string() == store.data.node_id,
        "invalid persisted machine identity"
    );
    store.save()?;
    Ok(store.data.node_id.clone())
}

/// Validate an already admitted existing catalog without loading, initializing
/// or saving a store. The receipt reader owns its bounded private file read.
pub(crate) fn existing_node_id(bytes: &[u8]) -> Result<String> {
    let data: store::Store = serde_json::from_slice(bytes)?;
    data.catalog.validate()?;
    let id = uuid::Uuid::parse_str(&data.node_id)?;
    ensure!(!id.is_nil() && id.to_string() == data.node_id, "invalid persisted machine identity");
    Ok(data.node_id)
}

fn input() -> Result<Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(transport::MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= transport::MAX_BYTES,
        "swarm request is too large"
    );
    Ok(serde_json::from_slice(crate::state::without_bom(&bytes))?)
}

pub fn dispatch(config: &Config, args: &[String], dry: bool) -> cli::Result<i32> {
    if dry {
        return Err(cli::Error::new(
            1,
            "swarm commands do not support --dry-run",
        ));
    }
    match execute(config, args) {
        Ok(value) => {
            println!("{}", value);
            Ok(0)
        }
        Err(error) => Err(cli::Error::new(1, format!("{error:#}"))),
    }
}

fn execute(config: &Config, args: &[String]) -> Result<Value> {
    let action = args.first().map(String::as_str).unwrap_or("get");
    let alias = || {
        args.get(1)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("choose a configured machine"))
    };
    // Commands that read or write preferences restart a worker too old to read them.
    let worker = matches!(action, "get" | "preference" | "exchange" | "join" | "sync")
        .then(|| service::supervise(config));
    match action {
        "session-project" | "assign-session" => {
            ensure!(args.len() == 2 && args[1] == "--json", "usage: hgs swarm {action} --json");
            Ok(session_project::execute(config, input()?, action == "assign-session"))
        }
        "assign-launch" => {
            ensure!(args.len() == 2 && args[1] == "--json", "usage: hgs swarm assign-launch --json");
            Ok(launch_project::execute(config, input()?))
        }
        "preference" => preference(config, &args[1..]),
        "worker" => { transport::worker()?; Ok(Value::Null) }
        "hello" | "inventory" => transport::hello(config,action == "inventory"),
        "preview" => transport::preview(config,alias()?),
        "bind" => transport::bind(config,alias()?),
        "join" => transport::join(config,alias()?,serde_json::from_value(input()?)?),
        "exchange" => transport::exchange(config,serde_json::from_value(input()?)?),
        "replica-inventory" => replica::inventory(config,serde_json::from_value(input()?)?),
        "replica-exchange" => replica::exchange(config,serde_json::from_value(input()?)?),
        "sync" => transport::sync(config),
        "get" | "initialize" | "apply" | "export" | "resolve" | "disconnect" => {
            // Read stdin before taking the lock: slow clients cannot monopolize it.
            let request = if matches!(action,"initialize"|"apply"|"resolve") { input()? } else { Value::Null };
            if action == "initialize" && args.iter().any(|arg|arg == "--g3-status") {
                ensure!(args == ["initialize", "--g3-status", "--json"], "usage: hgs swarm initialize --g3-status --json");
                return bootstrap::g3_status(config, request);
            }
            let g3 = if action == "initialize" && args.iter().any(|arg|arg == "--g3") {
                ensure!(args == ["initialize", "--g3", "--json"], "usage: hgs swarm initialize --g3 --json");
                Some(bootstrap::G3Import::parse(request.clone())?)
            } else { None };
            let mut store = LockedStore::load(config)?;
            let previous: std::collections::BTreeSet<_> = store.data.catalog.operations.keys().cloned().collect();
            match action {
                "get" => { bootstrap::maybe(&mut store, config)?; }
                "initialize" => {
                    if let Some(import) = &g3 { import.initialize(&mut store)?; }
                    else { bootstrap::initialize(&mut store, &request)?; }
                }
                "apply" => { ensure!(store.data.imported,"initialize the local project catalog first"); store.data.apply(serde_json::from_value(request)?)?; }
                "resolve" => {
                    let key: catalog::Key = serde_json::from_str(request["key"].as_str().unwrap_or_default())?;
                    let versions: Vec<String> = serde_json::from_value(request["versions"].clone())?;
                    let actor = store.data.node_id.clone();
                    store.data.catalog.write(&actor,key,request["value"].clone(),versions)?;
                }
                "disconnect" => { store.data.peers.remove(alias()?); }
                _ => {}
            }
            if action != "initialize" { store.save()?; }
            if action == "export" { return Ok(json!({"schema":1,"node_id":store.data.node_id,"initialized":store.data.imported,"catalog":store.data.catalog})); }
            let mut snapshot = store.snapshot(config);
            if let Some(import) = &g3 { snapshot["g3_import"] = import.receipt(); }
            snapshot["worker"] = worker.unwrap_or_else(|| service::status(config)).json();
            let mut written: std::collections::BTreeMap<String,Vec<String>> = std::collections::BTreeMap::new();
            for op in store.data.catalog.operations.values().filter(|op| !previous.contains(&op.id)) {
                written.entry(store.data.catalog.canonical_key(&op.key).token()).or_default().push(op.id.clone());
            }
            snapshot["written"] = json!(written);
            Ok(snapshot)
        }
        _ => bail!("usage: hgs swarm get | initialize | apply | preview PEER | join PEER | sync | worker | resolve | disconnect PEER | preference get|set | replica-inventory | replica-exchange"),
    }
}
