//! Shared metadata only. Transport credentials and per-device presentation stay
//! local; the swarm-wide phone delivery choice is shared. The legacy catalog is
//! one locked, atomically replaced store; initialized snapshots read it lock-free.
//! Drive folders use their own independently durable graph and initialization
//! witness under that lock, never a claimed two-file transaction.
mod adoption;
mod bootstrap;
mod catalog;
mod diagnostics;
pub(crate) use diagnostics::diagnostic_machine_label;
pub(crate) mod drive_folders;
pub(crate) mod fence;
pub(crate) mod drive_footprint;
mod launch_project;
mod infer_project;
pub(crate) use infer_project::{
    directory_project, infer_new_session, inherit_fork_project, place_child_session, placing_parent,
};
mod session_project;
mod session_ids;
mod session_memberships;
mod session_membership_peers;
pub(crate) mod session_labels;
pub(crate) use session_project::validate_payload as validate_session_project;
mod model;
mod replica;
mod replica_members;
mod service;
/// The running executable and its replacement, for long-lived commands.
pub(crate) use service::{Executable, Replacement};
mod store;
pub(crate) use store::read_snapshot as read_catalog_snapshot;
mod transport;
mod wait;

use crate::{cli, config::Config};
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::io::Read;
use store::LockedStore;

pub(crate) use catalog::MOBILE_DELIVERY;
pub(crate) use replica::{exchange_project as exchange_tasks, replicate};
pub(crate) use replica_members::open_reason as cutover_open;
#[cfg(test)]
pub(crate) use replica_members::Incomplete;
pub(crate) use replica_members::Member as ReplicaMember;

/// Why a bounded read found nothing to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unread {
    Missing,
    /// A link, FIFO, device, socket or folder: refused, never opened to wait.
    NotRegular,
    Oversized,
    Unreadable,
}

/// At most `cap` bytes of the regular file at `path`, without waiting
/// (#1471 review): these reads run under the catalog and project locks
/// against the caller's own deadline. Anything but a regular file at the
/// path is refused before it is opened. A swap after that check still cannot
/// follow, block or overrun: the open never follows a final link and never
/// waits for a FIFO's writer (`O_NOFOLLOW | O_NONBLOCK`; Windows opens a
/// reparse point itself), and the opened handle must be a regular file
/// within the cap.
pub(crate) fn read_regular(
    path: &std::path::Path,
    cap: u64,
) -> std::result::Result<Vec<u8>, Unread> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(Unread::NotRegular),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Unread::Missing),
        Err(_) => return Err(Unread::Unreadable),
    }
    read_opened(path, cap)
}

/// `read_regular` past its check: the open and the handle's own checks.
fn read_opened(path: &std::path::Path, cap: u64) -> std::result::Result<Vec<u8>, Unread> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    let file = match crate::platform::no_follow(&mut options).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Unread::Missing),
        Err(_) => return Err(Unread::Unreadable),
    };
    let metadata = file.metadata().map_err(|_| Unread::Unreadable)?;
    if !metadata.is_file() {
        return Err(Unread::NotRegular);
    }
    // One byte past the cap tells an oversized file, even one still growing.
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unread::Unreadable)?;
    if bytes.len() as u64 > cap {
        return Err(Unread::Oversized);
    }
    Ok(bytes)
}

/// A verified member that receives the projects' tasks (its hgs speaks the
/// replica protocol), with what this computer last learned of its replica.
pub(crate) struct TaskMember {
    pub alias: String,
    pub node: String,
    pub replica: Option<ReplicaMember>,
}

/// Every member that may hold the projects' tasks, by alias, read without
/// the lock. Only a member whose verified hello established that it shares
/// no tasks at all is left out; an unknown or another replica protocol stays
/// in (and keeps a cutover open). Information only.
pub(crate) fn task_members(config: &Config) -> Result<Vec<TaskMember>> {
    let snapshot = store::read_snapshot(config)?;
    let mut records = replica_members::read(config);
    let mut members = Vec::new();
    for (alias, peer) in snapshot["peers"].as_object().into_iter().flatten() {
        let Some(node) = peer["node"].as_str() else {
            continue;
        };
        let replica = records.remove(alias).filter(|record| record.node == node);
        if replica.as_ref().is_some_and(|record| record.no_replica) {
            continue;
        }
        members.push(TaskMember {
            alias: alias.clone(),
            node: node.to_owned(),
            replica,
        });
    }
    Ok(members)
}
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
    // A partial or unknown adoption is disclosed beside the main-only read.
    if let Some(adoption) = adoption::availability(&config.dir.join("swarm")) {
        snapshot["adoption"] = adoption;
    }
    Ok(snapshot)
}

#[cfg(all(test, unix))]
pub(crate) fn fixture_project(config: &Config, machine: &str, project: &str) {
    let mut store = LockedStore::load_bootstrap(config).unwrap();
    store.data.node_id = machine.into();
    store.data.import_missing(&json!({"projects":[{"id":project,"name":"example","color":"#123456","sessions":[],"folders":[]}]})).unwrap();
    store.data.imported = true;
    store.save().unwrap();
}

/// The files [`organization`] reads, for a reader that follows changes,
/// with the worker's marker, which a restarted worker rewrites.
pub(crate) fn organization_files(config: &Config) -> Vec<std::path::PathBuf> {
    let mut files = store::snapshot_files(config);
    files.push(config.dir.join("swarm").join(service::MARKER));
    files.extend(adoption::followed_files(&config.dir.join("swarm")));
    files
}

/// Task #20 of team c536f0e5 (P2.6c): this computer's swarm head for the
/// watch stream's `swarm` topic: the digest of the catalog's operations and
/// the fingerprint of the stored tasks, both as the worker compares them, so
/// a sync's own bookkeeping never moves it. Read only: nothing is saved.
pub(crate) fn head(config: &Config) -> anyhow::Result<Value> {
    let catalog = store::LockedStore::load(config)?.data.catalog.digest();
    Ok(serde_json::json!({"catalog": catalog, "tasks": replica::fingerprint(config)?}))
}

/// Task #20: the files whose stat moves with the head: the catalog, the
/// replica folder and each stored project's changes, tasks and inventory.
pub(crate) fn head_files(config: &Config) -> Vec<std::path::PathBuf> {
    let mut files = vec![config.dir.join("swarm/catalog.json")];
    let replica = config.state.join("replica");
    files.push(replica.clone());
    if let Ok(entries) = std::fs::read_dir(&replica) {
        for entry in entries.flatten().take(512) {
            let project = entry.path();
            if project.is_dir() {
                files.extend(["changes", "tasks", "inventory.json"].map(|name| project.join(name)));
            }
        }
    }
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
        .filter_map(|p| {
            Some((
                p["id"].as_str()?.to_owned(),
                p["name"].as_str().unwrap_or_default().to_owned(),
            ))
        })
        .collect())
}

/// One existing project name for native review, from bounded committed
/// primary bytes. It takes no catalog writer lock, creates no catalog/lock
/// file, and supplies no caller, membership or absence authority.
pub(crate) fn existing_project_name(config: &Config, id: &str) -> Result<String> {
    use anyhow::Context as _;
    adoption::guard_load(config)?;
    ensure!(
        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control),
        "invalid project ID"
    );
    let bytes = read_regular(&config.dir.join("swarm/catalog.json"), 128 * 1024 * 1024).map_err(
        |error| anyhow::anyhow!("The existing project catalog is unavailable ({error:?})."),
    )?;
    let data: store::Store =
        serde_json::from_slice(&bytes).context("read the existing project catalog")?;
    data.catalog.validate()?;
    let values = data.catalog.values();
    adoption::guard_load(config)?;
    ensure!(
        values.get(&catalog::Key::project(id, "alive")) == Some(&json!(true)),
        "no project {id:?} in the catalog"
    );
    Ok(values
        .get(&catalog::Key::project(id, "name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The catalog lock, held for a test until dropped.
#[cfg(test)]
pub(crate) fn lock_catalog_for_tests(config: &Config) -> impl Sized {
    LockedStore::load(config).expect("a test catalog")
}

/// This computer's identity and the catalog's projects (ID, name), read in
/// one catalog admission waited for no later than `deadline`: none when the
/// catalog is busy past it. For a command whose whole budget starts at its
/// entry (the fence check).
/// This computer's node and the catalog's projects (ID, name).
pub(crate) type IdentityAndProjects = (String, Vec<(String, String)>);

pub(crate) fn identity_and_projects_until(
    config: &Config,
    deadline: std::time::Instant,
) -> Result<Option<IdentityAndProjects>> {
    let Some(mut store) = LockedStore::load_until(config, deadline)? else {
        return Ok(None);
    };
    let id = uuid::Uuid::parse_str(&store.data.node_id)?;
    ensure!(
        !id.is_nil() && id.to_string() == store.data.node_id,
        "invalid persisted machine identity"
    );
    store.save()?;
    let snapshot = store.snapshot(config);
    let projects = snapshot["organization"]["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?.to_owned(), p["name"].as_str().unwrap_or_default().to_owned())))
        .collect();
    Ok(Some((store.data.node_id.clone(), projects)))
}

/// The live catalog project (ID, name) that this computer's session `name`
/// belongs to, if any.
pub(crate) fn session_project(config: &Config, name: &str) -> Result<Option<(String, String)>> {
    session_project_in(&LockedStore::load(config)?.snapshot(config), config, name)
}

/// `session_project`, waiting for the catalog no later than `deadline`
/// (#1519 review: an attempt's whole entry counts against its budget).
/// None when it is busy past it.
pub(crate) fn session_project_until(
    config: &Config,
    name: &str,
    deadline: std::time::Instant,
) -> Result<Option<Option<(String, String)>>> {
    let Some(store) = LockedStore::load_until(config, deadline)? else {
        return Ok(None);
    };
    session_project_in(&store.snapshot(config), config, name).map(Some)
}

fn session_project_in(
    snapshot: &Value,
    config: &Config,
    name: &str,
) -> Result<Option<(String, String)>> {
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn save_test_node_id(config: &Config, id: &str) -> Result<()> {
    let mut store = LockedStore::load(config)?;
    store.data.node_id = id.into();
    store.save()
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

fn adoption_input(cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "adoption request is too large");
    adoption::unique_bytes(&bytes, "adoption request")?;
    Ok(bytes)
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
        Err(error) => {
            let adoption = matches!(args.first().map(String::as_str), Some("adopt" | "adoption-preview" | "adoption-status"));
            let message = format!("{error:#}");
            let message = if adoption {
                message.chars().take(2048).collect()
            } else {
                message
            };
            Err(cli::Error::new(1, message))
        }
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
        "adoption-preview" => {
            ensure!(args.len() == 2 && args[1] == "--json" || args.len() == 3 && args[2] == "--json", "usage: hgs swarm adoption-preview [PEER] --json");
            if args.len() == 3 {
                adoption::preview_remote(config, alias()?)
            } else {
                adoption::preview_destination(config, adoption_input(64 * 1024)?)
            }
        }
        "adopt" => {
            ensure!(args == ["adopt"] || args == ["adopt", "--json"] || args.len() == 3 && args[2] == "--json", "usage: hgs swarm adopt [PEER] --json");
            if args.len() == 3 {
                adoption::adopt_source(config, alias()?, adoption_input(32 * 1024 * 1024)?)
            } else {
                adoption::adopt_destination(config, adoption_input(32 * 1024 * 1024)?)
            }
        }
        "adoption-status" => {
            ensure!(args.len() == 2 && args[1] == "--json" || args.len() == 3 && args[2] == "--json", "usage: hgs swarm adoption-status [PEER] --json");
            if args.len() == 3 {
                adoption::status_remote(config, alias()?, adoption_input(64 * 1024)?)
            } else {
                adoption::status_destination(config, adoption_input(64 * 1024)?)
            }
        }
        "session-labels-context" => {
            ensure!(args == ["session-labels-context", "--json"], "usage: hgs swarm session-labels-context --json");
            Ok(serde_json::to_value(session_labels::context(config)?)?)
        }
        "session-labels-gate" => {
            ensure!(args == ["session-labels-gate", "--json"], "usage: hgs swarm session-labels-gate --json");
            Ok(json!({"schema":1,"ready":session_labels::cached(config)}))
        }
        "session-labels" => {
            ensure!(args == ["session-labels", "--json"] || args == ["session-labels"], "usage: hgs swarm session-labels --json");
            session_labels::observe(config, std::time::Instant::now() + std::time::Duration::from_secs(2))
        }
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
        "replica-relay" => fence::relay(config,serde_json::from_value(input()?)?),
        "replica-task" => fence::task(config,serde_json::from_value(input()?)?),
        "sync" => transport::sync(config),
        "drive-folders" => drive_folders::dispatch(config, &args[1..]),
        "get" | "initialize" | "apply" | "export" | "resolve" | "disconnect" => {
            // Read stdin before taking the lock: slow clients cannot monopolize it.
            // `export --kinds` reads the kinds its requester parses.
            let kinds_requested = action == "export" && args.iter().any(|arg| arg == "--kinds");
            let request = if matches!(action,"initialize"|"apply"|"resolve") || kinds_requested { input()? } else { Value::Null };
            if action == "initialize" && args.iter().any(|arg|arg == "--g3-status") {
                ensure!(args == ["initialize", "--g3-status", "--json"], "usage: hgs swarm initialize --g3-status --json");
                return bootstrap::g3_status(config, request);
            }
            let g3 = if action == "initialize" && args.iter().any(|arg|arg == "--g3") {
                ensure!(args == ["initialize", "--g3", "--json"], "usage: hgs swarm initialize --g3 --json");
                Some(bootstrap::G3Import::parse(request.clone())?)
            } else { None };
            let g3_bootstrap = if action == "initialize" && args.iter().any(|arg|arg == "--g3-bootstrap") {
                ensure!(args == ["initialize", "--g3-bootstrap", "--json"], "usage: hgs swarm initialize --g3-bootstrap --json");
                Some(bootstrap::G3Bootstrap::parse(request.clone())?)
            } else { None };
            let export_kinds = request["kinds"].clone();
            let mut store = LockedStore::load(config)?;
            let previous: std::collections::BTreeSet<_> = store.data.catalog.operations.keys().cloned().collect();
            match action {
                "get" => { bootstrap::maybe(&mut store, config)?; }
                "initialize" => {
                    if let Some(import) = &g3 { import.initialize(&mut store)?; }
                    else if let Some(first) = &g3_bootstrap { first.bootstrap(&mut store)?; }
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
            if action == "export" {
                return Ok(json!({"schema":1,"node_id":store.data.node_id,"initialized":store.data.imported,
                    "catalog":catalog::exported(&store.data.catalog, &export_kinds)}));
            }
            let mut snapshot = store.snapshot(config);
            if let Some(import) = &g3 { snapshot["g3_import"] = import.receipt(); }
            if let Some(first) = &g3_bootstrap { snapshot["g3_bootstrap"] = first.receipt(); }
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

#[cfg(all(test, unix))]
#[path = "../teams/cli/tests/provider_catalog.rs"]
pub(crate) mod provider_fixture;
