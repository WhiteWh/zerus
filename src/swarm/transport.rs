use super::{
    catalog::{self, Catalog, Key, Operation},
    replica,
    replica_members::Incomplete,
    service,
    session_ids::Twin,
    store::{LockedStore, Peer},
};
use crate::{config::Config, platform};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ERRORS: usize = 64 * 1024;
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchange {
    pub swarm_id: String,
    pub known: BTreeSet<String>,
    pub operations: Vec<Operation>,
    /// Key kinds the requester parses. Older members neither send nor accept
    /// this field; responses to them contain only legacy kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<BTreeSet<String>>,
    /// Session ID memberships (zerus-ucck P2c-1), exchanged only between
    /// members that list the `session_id` kind; never sent to another.
    /// Each is read on its own: one that does not parse is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twins: Option<Vec<Value>>,
    /// The twins the requester has; present when it takes them in return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_twins: Option<BTreeSet<String>>,
    /// Primary ID operations are never sent to a peer that has not advertised
    /// this independent capability. Unlike twins, malformed primary data fails
    /// the whole exchange; no partial projection can become primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_memberships: Option<Vec<Operation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_id_memberships: Option<BTreeSet<String>>,
}

pub fn remote(config: &Config, alias: &str, action: &str, input: Option<&Value>) -> Result<Value> {
    run(config, alias, &["swarm", action], input)
}

/// `remote`, ended at `deadline` (never later than an ordinary call): one
/// past it fails before it connects, and one still running then is stopped.
pub fn remote_until(
    config: &Config,
    alias: &str,
    action: &str,
    input: Option<&Value>,
    deadline: Instant,
) -> Result<Value> {
    ensure!(Instant::now() < deadline, "no time left to reach {alias}");
    // The ABSOLUTE deadline goes down to the call, never a duration whose
    // clock starts later (review-n on #1452).
    let deadline = deadline.min(Instant::now() + CALL_LIMIT);
    run_within(config, alias, &["swarm", action], input, deadline)
}

#[cfg(test)]
pub(crate) use crate::remote_shell::TEST_SSH;

#[cfg(test)]
thread_local! {
    /// Delays after spawn and successful wait (review-n's seams on #1452).
    pub(crate) static AFTER_SPAWN: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    pub(crate) static AFTER_WAIT: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// How long one call to a member may run.
pub(crate) const CALL_LIMIT: Duration = Duration::from_secs(25);

/// The twins among `values` that parse; the rest are skipped, as derived data.
fn parsed(values: &[Value]) -> Vec<Twin> {
    values
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}

/// The peer's catalog with only the kinds this hgs parses. An older peer
/// ignores `--kinds` and its input, and its catalog holds no newer kind.
fn export(config: &Config, alias: &str) -> Result<Value> {
    run(
        config,
        alias,
        &["swarm", "export", "--kinds"],
        Some(&json!({"kinds": catalog::kinds()})),
    )
}

/// `hgs WORDS…` on the member behind `alias`, with `input` as JSON on its
/// standard input; its JSON answer.
pub fn run(config: &Config, alias: &str, words: &[&str], input: Option<&Value>) -> Result<Value> {
    run_within(config, alias, words, input, Instant::now() + CALL_LIMIT)
}

fn run_within(
    config: &Config,
    alias: &str,
    words: &[&str],
    input: Option<&Value>,
    deadline: Instant,
) -> Result<Value> {
    ensure!(
        config.peers.iter().any(|p| p == alias) && !config.is_self(alias),
        "choose a locally configured remote machine"
    );
    ensure!(
        !alias.starts_with('-') && !alias.chars().any(char::is_whitespace),
        "invalid connection alias"
    );
    let label_attempt = (words == ["swarm", "hello"])
        .then(|| super::session_labels::begin_peer(config, alias, deadline))
        .flatten();
    let bytes = serde_json::to_vec(&input.unwrap_or(&json!({})))?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "swarm exchange is too large"
    );
    let wsl = crate::wsl::profile(config, alias);
    if wsl.is_none() && crate::windows_peer::is_windows(config, alias) {
        let args = words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
        if let Some(answer) = crate::peer_channel::request_until(config, alias, &args, Some(bytes.clone()), deadline) {
            // A taken request, including an unknown outcome, is never replayed
            // over another SSH connection. Only None takes the legacy path.
            let answer = answer.map_err(anyhow::Error::msg)?;
            let answer = decode_answer(alias, &answer.stdout, &answer.stderr, answer.code == 0, deadline)?;
            super::session_labels::finish_peer(config, label_attempt, &answer);
            return Ok(answer);
        }
    }
    // WhiteWh fork: a WSL profile runs its own transport; any other peer goes
    // through the command syntax its login shell reads (remote_shell).
    let remote = if wsl.is_some() {
        None
    } else {
        Some(crate::remote_shell::hgs_within(config, alias, words, deadline)?)
    };
    let mut command = if let Some(profile) = wsl {
        crate::wsl::command(profile, &words.iter().map(|word| word.to_string()).collect::<Vec<_>>())?
    } else {
        let remote = remote.as_deref().unwrap_or_default();
        #[cfg(test)]
        let ssh = TEST_SSH
            .with(|program| program.borrow().clone())
            .map(std::process::Command::new)
            .map(Ok)
            .unwrap_or_else(|| crate::ssh_bytes::command(remote))?;
        #[cfg(not(test))]
        let ssh = crate::ssh_bytes::command(remote)?;
        ssh
    };
    if let Some(remote) = remote {
        command
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
            ])
            .args(crate::machines::ssh_options(config, alias))
            .arg(alias)
            .arg(remote);
    }
    ensure!(Instant::now() < deadline, "connection to {alias} timed out before start");
    let ssh = platform::spawn_ssh(&mut command, true, true, true).context("start swarm connection")?;
    #[cfg(test)]
    thread::sleep(AFTER_SPAWN.with(|delay| delay.get()));
    let mut child = ssh.child;
    let expired = || Instant::now() >= deadline;
    if expired() {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("connection to {alias} timed out as it started");
    }
    let mut stdin = ssh.stdin.unwrap();
    let mut stdout = ssh.stdout.unwrap();
    let mut stderr = ssh.stderr.unwrap();
    let _writer = thread::spawn(move || stdin.write_all(&bytes));
    // The readers report through channels, so draining them is bounded by
    // the call's limit too: a descendant that keeps the pipe open past it
    // is left behind, never waited for (review-n on #1452).
    let (output_sent, output_read) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.by_ref().take(MAX_BYTES + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = output_sent.send(read);
    });
    let (errors_sent, errors_read) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stderr.by_ref().take(MAX_ERRORS as u64).read_to_end(&mut bytes).map(|_| bytes);
        let _ = errors_sent.send(read);
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if expired() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(25));
    };
    #[cfg(test)]
    thread::sleep(AFTER_WAIT.with(|delay| delay.get()));
    ensure!(status.is_some(), "connection to {alias} timed out");
    let drained = |read: &std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| -> Result<Vec<u8>> {
        let left = deadline.saturating_duration_since(Instant::now());
        match read.recv_timeout(left) {
            Ok(bytes) => Ok(bytes?),
            Err(_) => anyhow::bail!("connection to {alias} timed out while its answer was read"),
        }
    };
    let output = drained(&output_read)?;
    let errors = drained(&errors_read)?;
    let answer = decode_answer(alias, &output, &errors, status.unwrap().success(), deadline)?;
    super::session_labels::finish_peer(config, label_attempt, &answer);
    Ok(answer)
}

/// Both transports keep the same deadline, output limit and response contract.
fn decode_answer(alias: &str, output: &[u8], errors: &[u8], success: bool, deadline: Instant) -> Result<Value> {
    // A ready answer is still refused when its original budget has expired.
    ensure!(Instant::now() < deadline, "connection to {alias} timed out");
    ensure!(errors.len() <= MAX_ERRORS, "remote catalog error output is too large");
    ensure!(
        success,
        "{alias}: {}",
        String::from_utf8_lossy(&errors).trim()
    );
    ensure!(
        output.len() as u64 <= MAX_BYTES,
        "remote catalog is too large"
    );
    let answer = serde_json::from_slice(output)
        .context("remote HGS does not provide a valid swarm response; update HGS on that machine",
    )?;
    ensure!(Instant::now() < deadline, "connection to {alias} timed out");
    Ok(answer)
}

pub fn hello(config: &Config, inventory: bool) -> Result<Value> {
    let mut store = LockedStore::load(config)?;
    store.save()?;
    let mut result = json!({"schema":1,"node_id":store.data.node_id,"swarm_id":store.data.catalog.swarm_id,"digest":store.data.catalog.digest(),"initialized":store.data.imported,"kinds":catalog::advertised(),
        "twins":store.data.twins.digest()});
    result["kinds"].as_array_mut().unwrap().push(json!(super::drive_folders::CAPABILITY));
    result["drive_folders"] = serde_json::to_value(super::drive_folders::inventory_locked(&store)?)?;
    if let Some(primary) = &store.data.id_memberships {
        result["id_memberships"] = json!(primary.digest());
        if inventory {
            result["known_id_memberships"] = json!(primary.known());
        }
    } else {
        result["kinds"]
            .as_array_mut()
            .unwrap()
            .retain(|k| k != super::session_memberships::CAPABILITY);
    }
    if inventory {
        result["known"] = json!(store.data.catalog.operations.keys().collect::<Vec<_>>());
        result["known_twins"] = json!(store.data.twins.known());
    }
    // For the fence (C3c1): every machine this catalog knows has joined (a
    // writer this computer may have exchanged tasks with, whether or not it
    // is a peer), and the membership incarnation.
    // One more than a universe covers is enough to close it.
    let writers: Vec<String> =
        super::fence::catalog_machines(&store).into_iter().take(super::fence::MAX_NODES + 1).collect();
    let membership = store.membership();
    // The verified direct peers, for the fence's routes (C3c1).
    let peers: std::collections::BTreeSet<String> = store
        .data
        .peers
        .values()
        .filter(|peer| !peer.node.is_empty())
        .map(|peer| peer.node.clone())
        .collect();
    // Project stores are opened after the catalog lock is released. Tasks
    // that cannot be read never stop the catalog. Only hello reads them.
    drop(store);
    if inventory {
        return Ok(result);
    }
    // Business/fence hello never waits for a connector. Configured slots
    // require the separate bounded background capability observation.
    result["session_labels"] = super::session_labels::observe_absent(config).unwrap_or(Value::Null);
    match replica::hello(config) {
        Ok(mut replica) => {
            replica["peers"] = json!(peers);
            replica["writers"] = json!(writers);
            replica["membership"] = json!(membership);
            replica["fence"] = json!(super::fence::FENCE);
            result["replica"] = replica
        }
        Err(error) => {
            eprintln!("hgs swarm: tasks: {error:#}");
            // A replica this computer cannot read now is still a replica:
            // an explicit part keeps the member eligible and its fence
            // closed. Only an older build sends no part, sharing no tasks.
            result["replica"] = json!({"protocol": replica::PROTOCOL, "unavailable": format!("{error:#}")});
        }
    }
    Ok(result)
}

pub fn exchange(config: &Config, request: Exchange) -> Result<Value> {
    let mut store = LockedStore::load(config)?;
    store
        .data
        .catalog
        .merge(&request.swarm_id, request.operations)?;
    if let Some(twins) = request.twins {
        store.data.twins.merge(&request.swarm_id, parsed(&twins))?;
    }
    let primary_capable = request
        .kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains(super::session_memberships::CAPABILITY));
    ensure!(
        primary_capable
            || (request.id_memberships.is_none() && request.known_id_memberships.is_none()),
        "ID membership capability was not negotiated"
    );
    if let Some(operations) = request.id_memberships {
        store
            .data
            .id_memberships
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ID memberships unavailable"))?
            .merge(&store.data.catalog, &request.swarm_id, operations)?;
    }
    // Incoming compatible twins are promoted too, before any later label move.
    if let Some(primary) = &mut store.data.id_memberships {
        primary.migrate(&store.data.catalog, &store.data.twins)?;
    }
    let id_memberships = request
        .known_id_memberships
        .as_ref()
        .map(|known| {
            store
                .data
                .id_memberships
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("ID memberships unavailable"))
                .map(|p| p.missing(known))
        })
        .transpose()?;
    let twins = request
        .known_twins
        .as_ref()
        .map(|known| store.data.twins.missing(known));
    let missing: Vec<_> = store
        .data
        .catalog
        .operations
        .values()
        .filter(|op| {
            !request.known.contains(&op.id) && catalog::accepted(request.kinds.as_ref(), &op.key)
        })
        .cloned()
        .collect();
    store.save()?;
    let mut response = json!({"schema":1,"node_id":store.data.node_id,"swarm_id":store.data.catalog.swarm_id,"operations":missing});
    if let Some(twins) = twins {
        response["twins"] = json!(twins);
    }
    if let Some(primary) = id_memberships {
        response["id_memberships"] = json!(primary);
    }
    Ok(response)
}

pub fn sync_peer(config: &Config, alias: &str) -> Result<Value> {
    // Information only: with no verified hello, or no catalog sync after
    // it, every project known here is incomplete for the member.
    let identity = match catalog_sync(config, alias) {
        Ok(identity) => identity,
        Err((reason, error)) => {
            replica::unreachable(config, alias, reason);
            return Err(error);
        }
    };
    replica::sync_peer(config, alias, &identity)?;
    let mut local = LockedStore::load(config)?;
    if let Some(peer) = local.data.peers.get_mut(alias) {
        peer.last_sync = now();
        peer.error.clear();
    }
    local.save()?;
    drop(local);
    let _ =
        super::session_labels::refresh_peer(config, alias, Instant::now() + Duration::from_secs(2));
    Ok(identity)
}

/// The member's verified hello and the catalog exchange after it; a failure
/// says whether the member answered as another computer.
fn catalog_sync(config: &Config, alias: &str,
) -> std::result::Result<Value, (Incomplete, anyhow::Error)> {
    let unreachable = |error: anyhow::Error| (Incomplete::Unreachable, error);
    let identity = remote(config, alias, "hello", None).map_err(unreachable)?;
    let local = LockedStore::load(config).map_err(unreachable)?;
    let peer = local
        .data
        .peers
        .get(alias)
        .ok_or_else(|| unreachable(anyhow::anyhow!("machine is not enrolled in this swarm")))?;
    if !(identity["schema"] == 1 && identity["node_id"] == peer.node) {
        return Err((
            Incomplete::ChangedIdentity,
            anyhow::anyhow!("the machine behind {alias} changed identity"),
        ));
    }
    let identity = catalog_exchange(config, alias, identity, local).map_err(unreachable)?;
    Ok(drive_sync_report(config, alias, identity))
}

fn catalog_exchange(config: &Config, alias: &str, identity: Value, mut local: LockedStore,
) -> Result<Value> {
    ensure!(
        identity["swarm_id"] == local.data.catalog.swarm_id,
        "{alias} belongs to a different swarm"
    );
    local
        .data
        .bind(alias, identity["node_id"].as_str().unwrap())?;
    // Older members reject unknown catalog kinds. They keep receiving the kinds
    // they parse; Settings names them until they are updated.
    let peer_kinds: Option<BTreeSet<String>> =
        serde_json::from_value(identity["kinds"].clone()).ok();
    if peer_kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains("preference"))
    {
        local.outdated.remove(alias);
    } else {
        let node = identity["node_id"].as_str().unwrap().to_owned();
        local.outdated.insert(alias.into(), node);
    }
    // Negotiate the primary graph separately from the older twins format.
    let primary_supported = peer_kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains(super::session_memberships::CAPABILITY));
    let primary = local.data.id_memberships.is_some() && primary_supported;
    let twins = peer_kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains(catalog::SESSION_ID_KIND));
    if twins {
        let node = identity["node_id"].as_str().unwrap().to_owned();
        local.session_ids.insert(alias.into(), node);
    } else {
        local.session_ids.remove(alias);
    }
    super::session_membership_peers::observed(
        local.path.parent().expect("catalog directory"), &local.data.peers,
        alias, identity["node_id"].as_str().unwrap(), primary_supported,
    )?;
    local.save()?;
    if identity["digest"] != local.data.catalog.digest()
        || (twins && identity["twins"] != local.data.twins.digest())
        || (primary
            && local
                .data
                .id_memberships
                .as_ref()
                .is_some_and(|p| identity["id_memberships"] != p.digest()))
    {
        drop(local);
        let inventory = remote(config, alias, "inventory", None)?;
        let local = LockedStore::load(config)?;
        let peer = local
            .data
            .peers
            .get(alias)
            .ok_or_else(|| anyhow::anyhow!("swarm connection was removed during sync"))?;
        ensure!(
            inventory["node_id"] == peer.node
                && inventory["swarm_id"] == local.data.catalog.swarm_id,
            "remote identity changed during sync"
        );
        let known: BTreeSet<String> = serde_json::from_value(inventory["known"].clone())?;
        let request = Exchange {
            swarm_id: local.data.catalog.swarm_id.clone(),
            known: local.data.catalog.operations.keys().cloned().collect(),
            operations: local
                .data
                .catalog
                .operations
                .values()
                .filter(|op| {
                    !known.contains(&op.id) && catalog::accepted(peer_kinds.as_ref(), &op.key)
                })
                .cloned()
                .collect(),
            kinds: peer_kinds.as_ref().map(|_| {
                let mut kinds = catalog::kinds();
                if primary {
                    kinds.insert(super::session_memberships::CAPABILITY.into());
                }
                kinds
            }),
            twins: twins.then(|| {
                let known: BTreeSet<String> =
                    serde_json::from_value(inventory["known_twins"].clone()).unwrap_or_default();
                local
                    .data
                    .twins
                    .missing(&known)
                    .into_iter()
                    .filter_map(|twin| serde_json::to_value(twin).ok())
                    .collect()
            }),
            known_twins: twins.then(|| local.data.twins.known()),
            id_memberships: if primary {
                // Unlike derived twins, an invalid primary inventory is not
                // read as empty. It is a failed exchange.
                let known: BTreeSet<String> =
                    serde_json::from_value(inventory["known_id_memberships"].clone())?;
                Some(local.data.id_memberships.as_ref().unwrap().missing(&known))
            } else {
                None
            },
            known_id_memberships: primary
                .then(|| local.data.id_memberships.as_ref().unwrap().known()),
        };
        let node = peer.node.clone();
        drop(local);
        let response = remote(
            config,
            alias,
            "exchange",
            Some(&serde_json::to_value(request)?),
        )?;
        ensure!(
            response["node_id"] == node,
            "remote identity changed during exchange"
        );
        let mut local = LockedStore::load(config)?;
        ensure!(
            local.data.peers.get(alias).is_some_and(|p| p.node == node),
            "swarm connection changed during exchange"
        );
        let swarm = response["swarm_id"].as_str().unwrap_or_default();
        local
            .data
            .catalog
            .merge(swarm, serde_json::from_value(response["operations"].clone())?,
        )?;
        if let Some(received) = response["twins"].as_array().filter(|_| twins) {
            local.data.twins.merge(swarm, parsed(received))?;
        }
        if primary {
            local
                .data
                .id_memberships
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("ID memberships unavailable"))?
                .merge(
                    &local.data.catalog,
                    swarm,
                    serde_json::from_value(response["id_memberships"].clone())?,
                )?;
        }
        if let Some(primary) = &mut local.data.id_memberships {
            primary.migrate(&local.data.catalog, &local.data.twins)?;
        }
        local.save()?;
    } else {
        drop(local);
    }
    Ok(identity)
}

fn drive_sync_report(config: &Config, alias: &str, mut identity: Value) -> Value {
    let capable = serde_json::from_value::<BTreeSet<String>>(identity["kinds"].clone())
        .is_ok_and(|kinds| kinds.contains(super::drive_folders::CAPABILITY));
    identity["drive_sync"] = if !capable {
        json!({"status":"not_advertised"})
    } else {
        match drive_sync(config, alias) {
            Ok(report) => report,
            Err(error) => json!({"status":"error","error":format!("{error:#}")}),
        }
    };
    identity
}

fn drive_local(
    config: &Config,
    alias: &str,
    node: &str,
    swarm: &str,
) -> Result<(LockedStore, super::drive_folders::DriveInventory)> {
    let local = LockedStore::load_scoped(config)?;
    ensure!(
        local.data.peers.get(alias).is_some_and(|peer| peer.node == node)
            && local.data.catalog.swarm_id == swarm,
        "swarm connection changed during drive exchange"
    );
    let inventory = super::drive_folders::inventory_locked(&local)?;
    ensure!(inventory.swarm_id == swarm, "local drive swarm changed");
    Ok((local, inventory))
}

fn drive_sync(config: &Config, alias: &str) -> Result<Value> {
    let (node, swarm) = {
        let local = LockedStore::load_scoped(config)?;
        let peer = local
            .data
            .peers
            .get(alias)
            .ok_or_else(|| anyhow::anyhow!("machine is not enrolled in this swarm"))?;
        (peer.node.clone(), local.data.catalog.swarm_id.clone())
    };
    let (_, local) = drive_local(config, alias, &node, &swarm)?;
    let mut remote: super::drive_folders::DriveInventory = serde_json::from_value(run(
        config,
        alias,
        &["swarm", "drive-folders", "inventory", "--json"],
        None,
    )?)?;
    ensure!(remote.node_id == node && remote.swarm_id == swarm, "drive peer changed during inventory");
    ensure!(remote.status != "unavailable", "remote drive graph is unavailable");
    ensure!(local.status != "unavailable", "local drive graph is unavailable");
    if local.status == "uninitialized" && remote.status == "uninitialized" {
        return Ok(json!({"status":"uninitialized"}));
    }
    for round in 0..=(super::drive_folders::MAX_OPERATIONS / super::drive_folders::MAX_BATCH + 1) {
        let (locked, local) = drive_local(config, alias, &node, &swarm)?;
        ensure!(remote.node_id == node && remote.swarm_id == swarm, "drive peer changed during exchange");
        ensure!(local.status != "unavailable" && remote.status != "unavailable", "drive graph is unavailable");
        let initialize_remote = local.status == "ready" && remote.status == "uninitialized";
        let initialize_local = local.status == "uninitialized" && remote.status == "ready";
        let operations = super::drive_folders::missing_locked(&locked, &remote.known)?;
        let request = super::drive_folders::ExchangeRequest {
            source: local.clone(),
            kinds: BTreeSet::from([super::drive_folders::CAPABILITY.to_owned()]),
            operations,
            initialize: initialize_remote,
        };
        ensure!(
            serde_json::to_vec(&request)?.len() as u64 <= super::drive_folders::MAX_EXCHANGE_BYTES,
            "drive exchange is too large"
        );
        let before = (local.known.clone(), remote.known.clone());
        drop(locked);
        let response: super::drive_folders::ExchangeResponse = serde_json::from_value(run(
            config,
            alias,
            &["swarm", "drive-folders", "exchange", "--json"],
            Some(&serde_json::to_value(request)?),
        )?)?;
        let next_remote = response.source.clone();
        let (locked, _) = drive_local(config, alias, &node, &swarm)?;
        let next_local = super::drive_folders::accept_exchange(&locked, &node, response, initialize_local)?;
        drop(locked);
        remote = next_remote;
        if next_local.known == remote.known {
            if next_local.status == "uninitialized" && remote.status == "uninitialized" {
                return Ok(json!({"status":"uninitialized"}));
            }
            ensure!(
                next_local.status == "ready" && remote.status == "ready" && next_local.digest == remote.digest,
                "drive frontier did not reconcile"
            );
            return Ok(json!({"status":"ready","operations":next_local.known.len(),"digest":next_local.digest}));
        }
        ensure!(before != (next_local.known.clone(), remote.known.clone()), "drive exchange made no progress");
        if round == super::drive_folders::MAX_OPERATIONS / super::drive_folders::MAX_BATCH + 1 {
            anyhow::bail!("drive exchange exceeded its bounded rounds");
        }
    }
    unreachable!("drive exchange round always returns or refuses")
}

pub fn sync(config: &Config) -> Result<Value> {
    let mut local = LockedStore::load(config)?;
    local.save()?;
    let peers: Vec<_> = local.data.peers.keys().cloned().collect();
    drop(local);
    let mut result = BTreeMap::new();
    for alias in peers {
        match sync_peer(config, &alias) {
            Ok(identity) => {
                result.insert(alias, json!({"ok":true,"catalog":{"status":"ready"},"drive_folders":identity["drive_sync"]}));
            }
            Err(error) => {
                let message = format!("{error:#}");
                let mut store = LockedStore::load(config)?;
                if let Some(peer) = store.data.peers.get_mut(&alias) {
                    peer.error = message.clone();
                }
                store.save()?;
                result.insert(alias, json!({"ok":false,"error":message}));
            }
        }
    }
    let _ = super::session_labels::refresh_local(config, Instant::now() + Duration::from_secs(2));
    if let Err(error)=replica::CutoverScan::default().advance(config) {
        eprintln!("hgs swarm: the bounded task cutover pass remains pending: {error:#}");
    }
    Ok(json!(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join {
    pub node_id: String,
    pub swarm_id: String,
    #[serde(default)]
    pub project_map: BTreeMap<String, String>,
    #[serde(default)]
    pub prefer_peer_memberships: bool,
}

pub fn bind(config: &Config, alias: &str) -> Result<Value> {
    let identity = remote(config, alias, "hello", None)?;
    ensure!(
        identity["schema"] == 1,
        "unsupported remote catalog protocol"
    );
    let mut local = LockedStore::load(config)?;
    local
        .data
        .bind(alias, identity["node_id"].as_str().unwrap_or_default())?;
    local.save()?;
    Ok(local.snapshot(config))
}

pub fn preview(config: &Config, alias: &str) -> Result<Value> {
    let remote = export(config, alias)?;
    let catalog: Catalog = serde_json::from_value(remote["catalog"].clone())?;
    catalog.validate()?;
    let mut local = LockedStore::load(config)?;
    local.save()?;
    let values = catalog.values();
    let projects: Vec<_> = values
        .iter()
        .filter_map(|(key, value)| match key {
            Key::Project { id, field } if field == "alive" && value == true => {
                Some(json!({"id":id,"name":values.get(&Key::project(id,"name"))}))
            }
            _ => None,
        })
        .collect();
    Ok(
        json!({"node_id":remote["node_id"],"swarm_id":catalog.swarm_id,"same_swarm":local.data.catalog.swarm_id == catalog.swarm_id,"projects":projects,"initialized":remote["initialized"]}),
    )
}

pub fn join(config: &Config, alias: &str, request: Join) -> Result<Value> {
    let remote = export(config, alias)?;
    ensure!(
        remote["node_id"] == request.node_id,
        "machine identity changed since preview; preview again"
    );
    let catalog: Catalog = serde_json::from_value(remote["catalog"].clone())?;
    catalog.validate()?;
    ensure!(
        catalog.swarm_id == request.swarm_id,
        "swarm changed since preview; preview again"
    );
    ensure!(
        remote["initialized"] == true,
        "open Projects in Zerus on the other machine first to import its existing catalog"
    );
    let mut local = LockedStore::load(config)?;
    ensure!(
        local.data.imported,
        "open Projects in Zerus on this machine first to import its existing catalog"
    );
    ensure!(
        request.node_id != local.data.node_id,
        "cannot join this machine to itself"
    );
    let same = local.data.catalog.swarm_id == catalog.swarm_id;
    ensure!(
        same || local.data.peers.is_empty(),
        "already enrolled in another swarm; merging entire swarms needs a separate migration"
    );
    super::drive_folders::allow_join(&local, same)?;
    // Normalize existing session identities before the initial membership merge.
    local.data.bind(alias, &request.node_id)?;
    if !same {
        let old = local.data.catalog.values();
        let remote_values = catalog.values();
        for (from, to) in &request.project_map {
            ensure!(
                old.get(&Key::project(from, "alive")) == Some(&json!(true))
                    && remote_values.get(&Key::project(to, "alive")) == Some(&json!(true)),
                "project mapping no longer matches both catalogs"
            );
        }
        local.backup("before-join")?;
        ensure!(
            local.data.id_memberships.is_some(),
            "ID memberships unavailable; cannot change swarms"
        );
        local.data.catalog = catalog;
        let mut existing = local.data.catalog.values();
        for (mut key, mut value) in old {
            // The swarm's delivery choice and project order win over the
            // joining machine's own.
            if matches!(key, Key::Preference { .. } | Key::Order { .. })
                && existing.contains_key(&key)
            {
                continue;
            }
            match &mut key {
                Key::Project { id, .. } => {
                    if let Some(target) = request.project_map.get(id) {
                        *id = target.clone();
                    }
                }
                Key::Folder { .. } if !value.is_null() => {
                    if let Some(target) = value["project"]
                        .as_str()
                        .and_then(|id| request.project_map.get(id))
                    {
                        value["project"] = json!(target);
                    }
                }
                Key::Session { .. } => {
                    if let Some(target) = value.as_str().and_then(|id| request.project_map.get(id))
                    {
                        value = json!(target);
                    }
                }
                Key::Order { .. } => {
                    // Mapped projects are already in the swarm's catalog; keep
                    // the first place of each.
                    let mut seen = BTreeSet::new();
                    let ids = value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str);
                    value = ids
                        .map(|id| request.project_map.get(id).map_or(id, String::as_str))
                        .filter(|id| seen.insert(id.to_owned()))
                        .map(|id| json!(id))
                        .collect();
                }
                _ => {}
            }
            // Compare only after mapping the joining catalog's project IDs.
            // Keeping peer placement never promotes its Inbox above a concrete
            // local project. Distinct concrete choices retain the explicit
            // preview's keep-peer policy; without it they remain causal heads.
            if request.prefer_peer_memberships && matches!(key, Key::Session { .. })
                && existing.get(&key).is_some_and(|peer| {
                    peer.as_str()
                    .is_some_and(|id| {
                        (id != "ungrouped" || value == "ungrouped")
                        && existing.get(&Key::project(id, "alive")) == Some(&json!(true))
                    })
                })
            {
                continue;
            }
            if existing.get(&key) != Some(&value) {
                existing.insert(key.clone(), value.clone());
                let actor = local.data.node_id.clone();
                local.data.catalog.write(&actor, key, value, vec![])?;
            }
        }
    } else {
        local.data.catalog.merge(
            &catalog.swarm_id,
            catalog.operations.into_values().collect(),
        )?;
    }
    if !same {
        local
            .data
            .id_memberships
            .as_mut()
            .unwrap()
            .rehome(&local.data.catalog, &request.project_map)?;
    }
    local.data.bind(alias, &request.node_id)?;
    local.data.peers.insert(
        alias.into(),
        Peer {
            node: request.node_id,
            ..Peer::default()
        },
    );
    local.save()?;
    drop(local);
    // Enrollment is durable even if the final exchange is interrupted. The
    // worker retries it; do not misleadingly report the join as rolled back.
    let exchange = sync(config)?;
    let local = LockedStore::load(config)?;
    Ok(json!({"joined":true,"sync":exchange,"snapshot":local.snapshot(config)}))
}

pub fn worker() -> Result<()> {
    let config = Config::load()?;
    let executable = service::Executable::current();
    let mut initial = LockedStore::load(&config)?;
    initial.save()?;
    drop(initial);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.dir.join("swarm/worker.lock"))?;
    // Commands checking for a stale worker probe the lock for an instant.
    // Unlike the recovery and voice workers (state::service_worker), no client
    // starts this one detached: only the service runs it on Linux and macOS,
    // and the desktop on Windows. A lock held for longer belongs to a worker
    // started by hand.
    let mut attempts = 0;
    while let Err(error) = fs2::FileExt::try_lock_exclusive(&lock) {
        attempts += 1;
        if attempts == 20 {
            return Err(error).context("a swarm worker is already running");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _lock = crate::platform::FileLock::held(lock);
    if let Err(error) = service::record(&config) {
        eprintln!("hgs swarm: record the worker version: {error:#}");
    }
    let mut schedule = Schedule::default();
    let mut placements = crate::state::placement::Scan::default();
    let mut cutover=replica::CutoverScan::default();
    loop {
        // An upgrade replaced this program: exit so that the service manager
        // starts the new version. Catalog errors never end the loop.
        ensure!(
            !executable.as_ref().is_some_and(service::Executable::replaced),
            "the hgs executable was replaced; exiting so that the new version starts"
        );
        match Config::load()
            .map_err(anyhow::Error::from)
            .and_then(|config| {
                schedule.round(&mut Live(&config))?;
                if let Err(error) = placements.advance(&config, |record, intent, end| {
                    super::infer_project::complete_pending(&config, record, intent, end)
                }) {
                    eprintln!("hgs swarm: initial placement remains pending: {error:#}");
                }
                if let Err(error)=cutover.advance(&config) {
                    eprintln!("hgs swarm: the bounded task cutover pass remains pending: {error:#}");
                }
                if super::session_labels::due_local(&config) {
                    let _ = super::session_labels::refresh_local(
                        &config,
                        Instant::now() + Duration::from_secs(2),
                    );
                }
                Ok(())
            }) {
            Ok(()) => {}
            Err(error) => eprintln!("hgs swarm: {error:#}"),
        }
        thread::sleep(Duration::from_secs(3));
    }
}

/// When the worker contacts each member.
#[derive(Default)]
struct Schedule {
    /// Per member alias: when its periodic sync is due, and its consecutive
    /// failures.
    members: BTreeMap<String, (u64, u64)>,
    /// The local change digest read at the start of the last complete round.
    last_digest: String,
}

/// What a worker round reads and does: this computer and its members in the
/// worker, a fake in tests.
trait Members {
    /// The local change digest (the catalog and the tasks of every project)
    /// and the aliases of the members.
    fn local(&mut self) -> Result<(String, Vec<String>)>;
    fn now(&self) -> u64;
    fn sync(&mut self, alias: &str) -> Result<()>;
    /// Records that the sync with `alias` failed.
    fn failed(&mut self, alias: &str, error: &anyhow::Error) -> Result<()>;
}

impl Schedule {
    /// One round: syncs each member that is due and, after a local change to
    /// the catalog or to any task, at once each member that is not backing off.
    fn round(&mut self, members: &mut impl Members) -> Result<()> {
        let (digest, aliases) = members.local()?;
        let changed = digest != self.last_digest;
        for alias in aliases {
            let (due, failures) = self.members.get(&alias).copied().unwrap_or_default();
            if members.now() < due && !(changed && failures == 0) {
                continue;
            }
            match members.sync(&alias) {
                Ok(()) => {
                    self.members.insert(alias, (members.now() + 30, 0));
                }
                Err(error) => {
                    members.failed(&alias, &error)?;
                    let failures = (failures + 1).min(5);
                    self.members.insert(
                        alias,
                        (members.now() + (15 * (1 << failures)).min(300), failures),
                    );
                }
            }
        }
        // The digest read at the START of this round, never one read after
        // it: a change made while the round ran, by a person or by the round's
        // own imports, then differs and syncs on the next round instead of
        // waiting for the periodic one (zerus-jrvnr). An import costs at most
        // one round that finds nothing new.
        self.last_digest = digest;
        Ok(())
    }
}

/// This computer and its members, as the worker syncs them.
struct Live<'a>(&'a Config);

impl Members for Live<'_> {
    fn local(&mut self) -> Result<(String, Vec<String>)> {
        let mut store = LockedStore::load(self.0)?;
        store.save()?;
        let catalog = store.data.catalog.digest();
        let aliases = store.data.peers.keys().cloned().collect();
        drop(store);
        let digest = catalog + &replica::fingerprint(self.0).unwrap_or_default();
        Ok((digest, aliases))
    }
    fn now(&self) -> u64 {
        now()
    }
    fn sync(&mut self, alias: &str) -> Result<()> {
        sync_peer(self.0, alias).map(|_| ())
    }
    fn failed(&mut self, alias: &str, error: &anyhow::Error) -> Result<()> {
        let mut store = LockedStore::load(self.0)?;
        if let Some(peer) = store.data.peers.get_mut(alias) {
            peer.error = format!("{error:#}");
        }
        store.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_and_ssh_answers_share_the_same_response_checks() {
        let deadline = Instant::now() + Duration::from_secs(3);
        assert_eq!(decode_answer("example-peer", br#"{"ok":true}"#, b"", true, deadline).unwrap(), json!({"ok":true}));
        assert!(decode_answer("example-peer", b"invalid", b"", true, deadline).unwrap_err().to_string().contains("valid swarm response"));
        assert!(decode_answer("example-peer", b"{}", b"refused", false, deadline).unwrap_err().to_string().contains("refused"));
        assert!(decode_answer("example-peer", b"{}", &vec![b'x'; MAX_ERRORS + 1], false, deadline).unwrap_err().to_string().contains("too large"));
        assert!(decode_answer("example-peer", b"{}", b"", true, Instant::now()).unwrap_err().to_string().contains("timed out"));
        assert!(decode_answer("example-peer", &vec![b' '; MAX_BYTES as usize + 1], b"", true, deadline).unwrap_err().to_string().contains("too large"));
    }

    /// Members whose syncs, and a person, change the local digest.
    #[derive(Default)]
    struct Fake {
        /// The local change digest.
        digest: u64,
        clock: u64,
        /// Each sync, by round.
        synced: Vec<Vec<String>>,
        /// Called during each sync with the round number and the alias.
        during: Option<fn(&mut Fake, usize, &str)>,
        failing: BTreeSet<String>,
    }

    impl Members for Fake {
        fn local(&mut self) -> Result<(String, Vec<String>)> {
            Ok((self.digest.to_string(), vec!["b".into(), "c".into()]))
        }
        fn now(&self) -> u64 {
            self.clock
        }
        fn sync(&mut self, alias: &str) -> Result<()> {
            self.synced.last_mut().unwrap().push(alias.into());
            if let Some(during) = self.during {
                during(self, self.synced.len(), alias);
            }
            ensure!(!self.failing.contains(alias), "{alias} is unreachable");
            Ok(())
        }
        fn failed(&mut self, _: &str, _: &anyhow::Error) -> Result<()> {
            Ok(())
        }
    }

    impl Fake {
        fn round(&mut self, schedule: &mut Schedule) {
            self.synced.push(Vec::new());
            schedule.round(self).unwrap();
        }
    }

    #[test]
    fn a_change_made_while_a_round_runs_syncs_on_the_next_round_once() {
        let mut fake = Fake {
            during: Some(|fake, round, alias| {
                // A person adds a task while the first round talks to b, and
                // the periodic fourth round imports from c.
                if (round, alias) == (1, "b") || (round, alias) == (4, "c") {
                    fake.digest += 1;
                }
            }),
            ..Fake::default()
        };
        let mut schedule = Schedule::default();
        for round in 1..=6 {
            if round == 4 {
                fake.clock += 30;
            }
            fake.round(&mut schedule);
        }
        // Each change syncs every member on the next round, which finds
        // nothing new; the round after it is quiet again.
        assert_eq!(
            fake.synced,
            [
                vec!["b", "c"],
                vec!["b", "c"],
                vec![],
                vec!["b", "c"],
                vec!["b", "c"],
                vec![]
            ],
            "a change made during a round waited for the periodic sync"
        );
    }

    #[test]
    fn without_a_change_only_due_members_sync_and_a_failing_one_backs_off() {
        let mut fake = Fake {
            failing: ["c".to_owned()].into(),
            ..Fake::default()
        };
        let mut schedule = Schedule::default();
        fake.round(&mut schedule);
        fake.round(&mut schedule);
        fake.clock += 30;
        fake.round(&mut schedule);
        // A local change syncs b at once; c waits for its backoff.
        fake.digest += 1;
        fake.round(&mut schedule);
        fake.clock += 30;
        fake.round(&mut schedule);
        assert_eq!(
            fake.synced,
            [vec!["b", "c"], vec![], vec!["b", "c"], vec!["b"], vec!["b"]]
        );
        assert_eq!(schedule.members["c"].1, 2);
    }
}
