//! Task replicas between members. After the catalog, every project whose
//! digest differs exchanges inventories and the updates each side lacks, in
//! pages; members whose hgs has no replica support are skipped and named in
//! Settings (docs/superpowers/specs/2026-10-18-task-replication-loro-design.md).
use super::replica_members::{self, Incomplete, Observed, Outcome};
use super::{store::LockedStore, transport};
use crate::config::Config;
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zerus_replica::{Damaged, Document, Inventory, ProjectStore, Update, SCHEMA, TASK_SCHEMA};

/// The replica exchange this hgs speaks; members with another one are skipped.
pub const PROTOCOL: u64 = 1;
/// The most update bytes one page carries; a larger document travels alone.
const PAGE: usize = 1 << 20;
/// Pages of one project in one sync; the next sync continues.
const MAX_PAGES: usize = 64;
/// How long a digest waits for a project another command holds; the project is
/// left out until the next pass.
const PROJECT_WAIT: Duration = Duration::from_secs(2);

fn root(config: &Config) -> PathBuf {
    config.state.join("replica")
}

/// What this computer holds of its stored projects: each readable one's
/// inventory digest, how many of its tasks do not load (one count for every
/// readable project, 0 included: a missing count is never taken for none),
/// and the projects left out (busy or unreadable this time, named in the
/// log). A project that cannot be read never stops the catalog or the others.
#[derive(Default, Serialize)]
struct Digests {
    projects: BTreeMap<String, String>,
    damaged: BTreeMap<String, usize>,
    omitted: BTreeSet<String>,
}

fn digests(config: &Config) -> Result<Digests> {
    let root = root(config);
    let mut out = Digests::default();
    for id in ProjectStore::stored(&root)? {
        match ProjectStore::open_within(&crate::tasks::replica_root(config)?, &id, PROJECT_WAIT) {
            Ok(Some(store)) => match store.inventory() {
                Ok((inventory, damaged)) => {
                    out.projects.insert(id.clone(), inventory.digest());
                    out.damaged.insert(id, damaged.len());
                }
                Err(error) => {
                    eprintln!("hgs swarm: the tasks of {id:?} cannot be read: {error}");
                    out.omitted.insert(id);
                }
            },
            Ok(None) => {
                eprintln!("hgs swarm: the tasks of {id:?} are busy; they are left out this time");
                out.omitted.insert(id);
            }
            Err(error) => {
                eprintln!("hgs swarm: the tasks of {id:?} cannot be read: {error}");
                out.omitted.insert(id);
            }
        }
    }
    Ok(out)
}

/// The projects this computer knows: alive in its catalog or stored here.
fn known(config: &Config) -> BTreeSet<String> {
    LockedStore::load(config)
        .map(|store| alive(config, &store))
        .unwrap_or_default()
        .into_iter()
        .chain(ProjectStore::stored(&root(config)).unwrap_or_default())
        .collect()
}

/// The replica capability `hello` advertises: the protocol, the replica
/// schema, the highest task schema this hgs writes, this computer's replica
/// store, every stored project's digest, how many of its tasks do not load
/// (`damaged`, by project, for each digest) and the projects left out
/// (`omitted`). A store
/// identity that cannot be read is null and named in the log; it never
/// hides the rest.
pub fn hello(config: &Config) -> Result<Value> {
    let store = crate::tasks::replica_root(config).map_err(anyhow::Error::from).and_then(|root| ProjectStore::store_id(&root).map_err(anyhow::Error::from))
        .map_err(|error| eprintln!("hgs swarm: the replica store has no identity: {error}"))
        .ok();
    let digests = digests(config)?;
    Ok(json!({"protocol": PROTOCOL, "schema": SCHEMA, "task_schema": TASK_SCHEMA,
              "store": store, "projects": digests.projects, "damaged": digests.damaged,
              "omitted": digests.omitted}))
}

/// Bounded background protocol work, independent of native task Start.
#[derive(Default)]
pub(crate) struct CutoverScan { after_project:String, loaded:bool }
impl CutoverScan {
    pub(crate) fn advance(&mut self,config:&Config)->Result<bool> {
        self.advance_with(config,&super::fence::Ssh(config))
    }
    pub(crate) fn advance_with(&mut self,config:&Config,net:&dyn super::fence::Net)->Result<bool> {
        // Disposable progress, shared with one-shot manual sync. It carries
        // no version, root or authority and is never held over networking.
        let cursor=config.dir.join("swarm/task-cutover-cursor");
        if !self.loaded {
            self.after_project=super::read_regular(&cursor,4096).ok()
                .and_then(|bytes|String::from_utf8(bytes).ok()).unwrap_or_default();
            self.loaded=true;
        }
        let mut projects=ProjectStore::stored(&root(config))?;
        projects.sort();
        let Some(project)=projects.iter().find(|project|*project>&self.after_project).or_else(||projects.first()).cloned() else {return Ok(false);};
        // Advance before inspection or migration: a busy, held, missing or
        // unprovable task/project cannot starve the following one.
        self.after_project=project.clone();
        let _=super::store::replace(&cursor,project.as_bytes());
        let candidate={
            let Some(store)=ProjectStore::open_within(&root(config),&project,PROJECT_WAIT)? else {return Ok(false);};
            crate::tasks::cutover_candidate(&store)
        };
        let Some(uuid)=candidate else {return Ok(false);};
        // The index and project writer are both gone before any network.
        super::fence::catch_up_writer(config,net,&project,&uuid,Instant::now()+Duration::from_secs(5))?;
        Ok(true)
    }
}

/// Changes whenever any stored project changes; the worker exchanges at once.
pub fn fingerprint(config: &Config) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&digests(config)?)?)))
}

/// One update on the wire: the document and its Loro updates in base64.
/// Requests and updates ignore fields a later hgs adds.
#[derive(Serialize, Deserialize)]
pub(crate) struct Wire {
    document: Document,
    bytes: String,
    /// Only the native response producer sets this after reading its
    /// already admitted immutable pin. Requests never consume it as proof.
    #[serde(default,skip_serializing_if="Option::is_none")]
    admitted_seal: Option<String>,
}

pub(crate) fn encode(updates: Vec<Update>) -> Vec<Wire> {
    updates
        .into_iter()
        .map(|u| Wire {
            document: u.document,
            bytes: STANDARD.encode(u.bytes),
            admitted_seal:None,
        })
        .collect()
}

fn encode_admitted(store:&ProjectStore, updates:Vec<Update>) -> Vec<Wire> {
    encode(updates).into_iter().map(|mut wire| {
        if let Document::Task(uuid)=&wire.document {
            wire.admitted_seal=store.admitted_cutover_identity(uuid).ok().flatten();
        }
        wire
    }).collect()
}

fn bootstrap_source<'a>(reply:&'a Value, observed:Option<&str>)->Option<&'a str> {
    reply["store"].as_str().filter(|store| zerus_replica::ids::is_uuid(store) && Some(*store)==observed)
}

/// The initiator has already bound this response to the exact node/store of
/// its fresh hello. Only this qualified response lane may bootstrap a pin;
/// ordinary imports (including requests) keep the field as candidate data.
fn import_answer(store:&ProjectStore, wire:Vec<Wire>, own_node:&str, source_node:&str, source_store:Option<&str>, capable:bool) -> Result<zerus_replica::Imported> {
    let mut qualified=0usize;
    let mut remaining=Vec::new();
    for update in wire {
        let scope=match (&update.document,&update.admitted_seal,source_store) {
            (Document::Task(uuid),Some(seal),Some(source)) if capable =>Some((uuid.clone(),seal.clone(),source.to_owned())),
            _=>None,
        };
        let bytes=STANDARD.decode(&update.bytes)?;
        if let Some((uuid,seal,source))=scope {
            if store.admitted_cutover_identity(&uuid).ok().flatten().as_ref()!=Some(&seal) {
                let adopted=(|| -> Result<()> {
                    store.settle_cutover(&uuid)?;
                    if let Some((original,own))=store.original_cutover_proposal(&uuid)?.zip(store.pending_cutover_capture(&uuid)?) {
                        store.catch_up_cutover(&original,&own,source_node,&source,&seal)?;
                    }
                    let (capture,local)=store.prepare_cutover(&uuid,own_node)?;
                    let proposal=if let Some(original)=store.original_cutover_proposal(&uuid)? {original} else {
                        let candidate=if local.is_empty() {zerus_replica::TaskDoc::from_bytes(&bytes)?} else {
                            let mut candidate=zerus_replica::TaskDoc::from_bytes(&local)?;
                            candidate.import(&bytes)?;
                            candidate
                        };
                        candidate.snapshot()?
                    };
                    store.catch_up_cutover(&proposal,&capture,source_node,&source,&seal)?;
                    Ok(())
                })();
                if adopted.is_ok() {qualified+=1;continue;}
            }
        }
        remaining.push(Update{document:update.document,bytes});
    }
    let mut imported=store.import_updates(remaining)?;
    imported.changed+=qualified;
    Ok(imported)
}

pub(crate) fn decode(wire: Vec<Wire>) -> Result<Vec<Update>> {
    wire.into_iter()
        .map(|w| {
            Ok(Update {
                document: w.document,
                bytes: STANDARD.decode(w.bytes)?,
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub struct Request {
    /// The requester's swarm; members of another one are refused.
    swarm_id: String,
    project: String,
    #[serde(default)]
    inventory: Option<Inventory>,
    #[serde(default)]
    updates: Vec<Wire>,
}

/// Admits a member's request: it comes from this swarm and names a project
/// this member knows (stored here, or alive in the catalog). This member's node.
fn admit(config: &Config, request: &Request) -> Result<String> {
    admit_project(config, &request.swarm_id, &request.project)
}

/// `admit` for any replica request naming `swarm_id` and `project`.
pub(crate) fn admit_project(config: &Config, swarm_id: &str, project: &str) -> Result<String> {
    let store = LockedStore::load(config)?;
    admitted_project(config, store, swarm_id, project)
}

/// `admit_project`, waiting for the catalog lock no later than `deadline`:
/// none when the catalog is busy past it.
pub(crate) fn admit_project_until(
    config: &Config,
    swarm_id: &str,
    project: &str,
    deadline: std::time::Instant,
) -> Result<Option<String>> {
    let Some(store) = LockedStore::load_until(config, deadline)? else {
        return Ok(None);
    };
    admitted_project(config, store, swarm_id, project).map(Some)
}

fn admitted_project(config: &Config, store: LockedStore, swarm_id: &str, project: &str) -> Result<String> {
    ensure!(swarm_id == store.data.catalog.swarm_id, "the request comes from another swarm");
    let node = store.data.node_id.clone();
    let alive = alive(config, &store);
    drop(store);
    ensure!(
        alive.contains(project) || ProjectStore::stored(&root(config))?.iter().any(|id| id == project),
        "no project {project:?} here"
    );
    Ok(node)
}

/// The projects alive in this member's catalog.
fn alive(config: &Config, store: &LockedStore) -> BTreeSet<String> {
    store.snapshot(config)["organization"]["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect()
}

/// `swarm replica-inventory`: what this member holds of the project.
pub fn inventory(config: &Config, request: Request) -> Result<Value> {
    let node = admit(config, &request)?;
    let store = ProjectStore::open(&crate::tasks::replica_root(config)?, &request.project)?;
    let (inventory, damaged) = store.inventory()?;
    Ok(json!({"node_id": node, "store": answering_store(config), "inventory": inventory,
              "damaged": damaged.len()}))
}

/// This member's replica store as it answers, read at that moment: a root
/// replaced since its hello answers with another one (zerus-cg5h.4.5). One
/// that cannot be read is null and named in the log.
fn answering_store(config: &Config) -> Option<String> {
    crate::tasks::replica_root(config).map_err(anyhow::Error::from).and_then(|root| ProjectStore::store_id(&root).map_err(anyhow::Error::from))
        .map_err(|error| eprintln!("hgs swarm: the replica store has no identity: {error}"))
        .ok()
}

/// `swarm replica-exchange`: imports the requester's updates and answers with
/// a page of what its inventory lacks.
pub fn exchange(config: &Config, request: Request) -> Result<Value> {
    let node = admit(config, &request)?;
    let theirs = request
        .inventory
        .ok_or_else(|| anyhow::anyhow!("a replica exchange needs the requester's inventory"))?;
    let store = ProjectStore::open(&crate::tasks::replica_root(config)?, &request.project)?;
    let imported = store.import_updates(decode(request.updates)?)?;
    let (ours, damaged) = store.inventory()?;
    let (updates, more) = store.updates_for(&ours, &theirs, PAGE)?;
    // How many of its tasks do not load now, after this page: a task
    // damaged since the inventory the requester read is counted too.
    Ok(json!({"node_id": node, "store": answering_store(config), "imported": imported.changed,
              "refused": imported.refused, "updates": encode_admitted(&store,updates), "more": more,
              "cutover_capability":super::fence::FENCE,
              "damaged": damaged.len()}))
}

/// What a member's hello says it holds, or none when that is not in a form
/// this reads: such a map is never taken for an empty, complete one, and a
/// project with no damaged count in it is never taken for a healthy one.
struct Theirs {
    projects: BTreeMap<String, String>,
    damaged: BTreeMap<String, usize>,
    omitted: BTreeSet<String>,
}

impl Theirs {
    /// Missing projects are not advertised empty inventories. Exchange them
    /// once to obtain both inventories and health counts, rather than treating
    /// two absent digests as agreement with permanently unknown health.
    fn agrees(&self, local: &Digests, project: &str) -> bool {
        local.projects.get(project).is_some_and(|digest| self.projects.get(project) == Some(digest))
    }
}

fn theirs(remote: &Value) -> Option<Theirs> {
    Some(Theirs {
        projects: serde_json::from_value(remote["projects"].clone()).ok()?,
        damaged: serde_json::from_value(remote["damaged"].clone()).ok()?,
        omitted: serde_json::from_value(remote["omitted"].clone()).ok()?,
    })
}

/// What one project's exchange showed. It is complete only when every
/// refusal list and damaged count, here and there, is known and empty; a
/// missing or unreadable one is unknown (`malformed`), never none. A known
/// problem is named first.
#[derive(Default)]
struct Shown {
    refused: bool,
    refusals_unknown: bool,
    /// A reply did not name, in a form this reads, the store that answered.
    store_unknown: bool,
    damaged: bool,
    damage_unknown: bool,
}

impl Shown {
    /// Whether a side refused documents of the page, when it said so.
    fn refusals(&mut self, refused: Option<bool>) {
        match refused {
            Some(refused) => self.refused |= refused,
            None => self.refusals_unknown = true,
        }
    }

    /// How many tasks of the project do not load on a side, when it said so.
    fn damage(&mut self, count: Option<u64>) {
        match count {
            Some(count) => self.damaged |= count > 0,
            None => self.damage_unknown = true,
        }
    }

    /// The equal-digest case: the readable documents agree on both sides,
    /// so only the damaged counts of `project` decide.
    fn agreeing(local: &Digests, theirs: &Theirs, project: &str) -> Outcome {
        let mut shown = Shown::default();
        shown.damage(local.damaged.get(project).map(|&count| count as u64));
        shown.damage(theirs.damaged.get(project).map(|&count| count as u64));
        shown.outcome()
    }

    fn outcome(&self) -> Outcome {
        if self.refused {
            Err((Incomplete::RefusedDocuments, false))
        } else if self.refusals_unknown || self.store_unknown {
            Err((Incomplete::Malformed, false))
        } else if self.damaged {
            Err((Incomplete::DamagedTasks, true))
        } else if self.damage_unknown {
            Err((Incomplete::Malformed, true))
        } else {
            Ok(())
        }
    }

    /// A page that changed nothing on either side: something is refused, or
    /// unknown when the member did not say what it did with the page.
    fn stalled(&self) -> Outcome {
        if (self.refusals_unknown || self.store_unknown) && !self.refused {
            Err((Incomplete::Malformed, false))
        } else {
            Err((Incomplete::RefusedDocuments, false))
        }
    }
}

/// Exchanges every project whose digest differs with the member behind
/// `alias`, whose `hello` answered `identity` (verified to come from its
/// node), and records an outcome for every project known here under that
/// exact observation.
pub fn sync_peer(config: &Config, alias: &str, identity: &Value, force: bool) -> Result<()> {
    sync_peer_with(config, alias, identity, force, replica_members::now(), |project, observed| {
        exchange_pages(config, alias, project, observed, Some(PROJECT_WAIT))
    })
}

fn sync_peer_with(
    config: &Config,
    alias: &str,
    identity: &Value,
    force: bool,
    at: u64,
    mut exchange: impl FnMut(&str, Option<&str>) -> std::result::Result<Outcome, Failed>,
) -> Result<()> {
    let remote = &identity["replica"];
    let supported = remote["protocol"] == PROTOCOL;
    let node = identity["node_id"].as_str().unwrap_or_default().to_owned();
    let mut store = LockedStore::load(config)?;
    if supported {
        store.replica_outdated.remove(alias);
    } else {
        store.replica_outdated.insert(alias.into(), node.clone());
    }
    store.save()?;
    let alive = alive(config, &store);
    drop(store);
    // Information only: what the member advertised, once verified.
    let observed = Observed::of(&node, identity);
    if let Err(error) = replica_members::seen(config, alias, &observed, at) {
        eprintln!("hgs swarm: {alias}: its replica could not be recorded: {error:#}");
    }
    if !supported {
        return Ok(());
    }
    // This computer's own pass failing is an outcome too: every project
    // known here is incomplete for this observation, never left complete.
    let local = match digests(config) {
        Ok(local) => local,
        Err(error) => {
            let outcomes: Vec<(String, Outcome)> = known(config)
                .into_iter()
                .map(|project| (project, Err((Incomplete::Busy, false))))
                .collect();
            record(config, alias, &observed, &outcomes);
            return Err(error);
        }
    };
    let theirs = theirs(remote);
    let previous = (!force).then(|| replica_members::read(config).remove(alias)).flatten();
    let version = identity["version"].as_str();
    let mut projects = known(config);
    projects.extend(local.projects.keys().cloned());
    projects.extend(local.omitted.iter().cloned());
    let known = projects.clone();
    if let Some(theirs) = &theirs {
        projects.extend(theirs.projects.keys().cloned());
        projects.extend(theirs.omitted.iter().cloned());
    }
    // A project that fails never keeps the others from their exchange.
    let mut failed = None;
    let mut outcomes = Vec::new();
    let mut memos = BTreeMap::new();
    for project in &projects {
        let pair = replica_members::Divergence::of(
            local.projects.get(project),
            theirs.as_ref().and_then(|theirs| theirs.projects.get(project)),
            version,
        );
        let mut attempted = false;
        let outcome = if local.omitted.contains(project) {
            Err((Incomplete::Busy, false))
        } else if theirs.as_ref().is_some_and(|t| t.omitted.contains(project)) {
            Err((Incomplete::Omitted, false))
        } else {
            let held = local.projects.contains_key(project)
                && theirs.as_ref().is_some_and(|t| t.projects.contains_key(project));
            // A project gone from the catalog is exchanged only while both still hold it.
            if !held && !alive.contains(project) {
                Err((Incomplete::Omitted, false))
            } else if let Some(t) = theirs.as_ref().filter(|t| t.agrees(&local, project)) {
                // Equal digests: the readable documents agree, and the project
                // is complete only when both sides say no task of it is damaged.
                Shown::agreeing(&local, t, project)
            } else {
                if pair.as_ref().is_some_and(|pair| replica_members::deferred(previous.as_ref(), &observed, project, pair, at)) {
                    // Keep the exact outcome and its attempt time. A skip is
                    // not new evidence and must not postpone the hourly retry.
                    continue;
                }
                attempted = true;
                match exchange(project, observed.store.as_deref()) {
                    Ok(outcome) => outcome,
                    Err((reason, error)) => {
                        failed.get_or_insert(error);
                        Err((reason, false))
                    }
                }
            }
        };
        // A malformed map never makes an exchange that did not happen complete.
        let outcome = match (outcome, theirs.is_some()) {
            (Ok(()), false) => Ok(()),
            (Err((Incomplete::Omitted, exchanged)), false) => Err((Incomplete::Malformed, exchanged)),
            (outcome, _) => outcome,
        };
        if known.contains(project) {
            if attempted && matches!(outcome, Err((Incomplete::RefusedDocuments | Incomplete::Malformed, _))) {
                if let Some(pair) = pair { memos.insert(project.clone(), pair); }
            }
            outcomes.push((project.clone(), outcome));
        }
    }
    record_with_memos(config, alias, &observed, &outcomes, &memos, at);
    failed.map_or(Ok(()), Err)
}

/// Records exchanges' outcomes for the member under `observed`
/// (information only); a failure to record is named in the log and never
/// fails the sync.
fn record(config: &Config, alias: &str, observed: &Observed, outcomes: &[(String, Outcome)]) {
    record_with_memos(config, alias, observed, outcomes, &BTreeMap::new(), replica_members::now())
}

fn record_with_memos(config: &Config, alias: &str, observed: &Observed, outcomes: &[(String, Outcome)], memos: &BTreeMap<String, replica_members::Divergence>, at: u64) {
    if outcomes.is_empty() { return; }
    let known = known(config);
    let saved = if memos.is_empty() {
        replica_members::exchanged(config, alias, observed, outcomes, &known, at)
    } else {
        replica_members::exchanged_with_memos(config, alias, observed, outcomes, &known, memos, at)
    };
    match saved {
        Ok(true) => {}
        Ok(false) => eprintln!("hgs swarm: {alias}: its exchanges belong to an earlier hello; they are not recorded"),
        Err(error) => eprintln!("hgs swarm: {alias}: its exchanges could not be recorded: {error:#}"),
    }
}

/// No verified hello came from the member behind `alias`: every project
/// known here is incomplete for it, for `reason` (information only).
pub fn unreachable(config: &Config, alias: &str, reason: Incomplete) {
    if let Err(error) = replica_members::unreachable(config, alias, reason, &known(config), replica_members::now()) {
        eprintln!("hgs swarm: {alias}: its exchanges could not be recorded: {error:#}");
    }
}

/// Brings this project's tasks here and on the member behind `alias` to the
/// same versions now, for a send or cancel that runs there.
pub fn replicate(config: &Config, alias: &str, project: &str) -> Result<()> {
    let identity = transport::remote(config, alias, "hello", None)
        .map_err(|error| anyhow::anyhow!("{alias} cannot be reached: {error:#}"))?;
    let store = LockedStore::load(config)?;
    let peer = store
        .data
        .peers
        .get(alias)
        .ok_or_else(|| anyhow::anyhow!("{alias} is not a member of this swarm"))?;
    ensure!(identity["node_id"] == peer.node, "the computer behind {alias} changed identity");
    let observed = Observed::of(&peer.node, &identity);
    drop(store);
    ensure!(identity["replica"]["protocol"] == PROTOCOL, "update Zerus on {alias} to send tasks there");
    if let Err(error) = replica_members::seen(config, alias, &observed, replica_members::now()) {
        eprintln!("hgs swarm: {alias}: its replica could not be recorded: {error:#}");
    }
    // Copies that already agree need no exchange.
    let (inventory, damaged) = ProjectStore::open(&crate::tasks::replica_root(config)?, project)?.inventory()?;
    let theirs = theirs(&identity["replica"]);
    if let Some(t) = theirs.as_ref().filter(|t| t.projects.get(project) == Some(&inventory.digest())) {
        let local = Digests {
            damaged: BTreeMap::from([(project.to_owned(), damaged.len())]),
            ..Digests::default()
        };
        record(config, alias, &observed, &[(project.to_owned(), Shown::agreeing(&local, t, project))]);
        return Ok(());
    }
    let (outcome, result) = match exchange_pages(config, alias, project, observed.store.as_deref(), None) {
        Ok(outcome) => (outcome, Ok(())),
        Err((reason, error)) => (Err((reason, false)), Err(error)),
    };
    let outcome = match (outcome, theirs.is_some()) {
        (Ok(()), false) => Ok(()),
        (outcome, _) => outcome,
    };
    record(config, alias, &observed, &[(project.to_owned(), outcome)]);
    result
}

/// Exchanges one project's tasks with the member behind `alias`, page by
/// page, waiting for the project here as long as another command holds it.
/// No hello verified this member for it, so nothing is recorded.
pub fn exchange_project(config: &Config, alias: &str, project: &str) -> Result<()> {
    exchange_pages(config, alias, project, None, None)
        .map(|_| ())
        .map_err(|(_, error)| error)
}

type Failed = (Incomplete, anyhow::Error);

/// The pages of one project's exchange: complete, or why not and whether
/// the readable documents were exchanged; a failure says why too. Complete
/// needs every page, and every reply saying, in a form this reads, that
/// nothing was refused and no task is damaged on either side. A reply whose
/// pages (`inventory`, `updates`, `more`) do not read stops the exchange as
/// `malformed`. Its evidence (`imported` with `refused`, and `damaged`, which
/// members before C2 do not send) missing or unreadable keeps the documents
/// moving and the outcome `malformed`.
fn exchange_pages(
    config: &Config,
    alias: &str,
    project: &str,
    observed: Option<&str>,
    wait: Option<Duration>,
) -> std::result::Result<Outcome, Failed> {
    let root = crate::tasks::replica_root(config).map_err(|error| (Incomplete::Busy, error.into()))?;
    let unreachable = |error: anyhow::Error| (Incomplete::Unreachable, error);
    let malformed = |error: anyhow::Error| (Incomplete::Malformed, error.context(format!("{alias} sent an invalid replica answer")));
    let changed_identity = || (Incomplete::ChangedIdentity, anyhow::anyhow!("the computer behind {alias} changed identity"));
    // Each reply comes from the store the hello showed (`observed`); another
    // one means its replica root was replaced since, and one not named in a
    // form this reads leaves the outcome unknown (zerus-cg5h.4.5).
    let answered = |reply: &Value, shown: &mut Shown| -> std::result::Result<(), Failed> {
        let store = reply["store"].as_str().filter(|store| zerus_replica::ids::is_uuid(store));
        match (store, observed) {
            (Some(store), Some(observed)) if store == observed => Ok(()),
            (Some(_), Some(_)) => Err((
                Incomplete::ChangedIdentity,
                anyhow::anyhow!("the replica store behind {alias} changed since its hello"),
            )),
            _ => {
                shown.store_unknown = true;
                Ok(())
            }
        }
    };
    // None: held by another command longer than `wait`.
    let open = |root: &PathBuf| -> std::result::Result<Option<ProjectStore>, Failed> {
        match wait {
            Some(wait) => ProjectStore::open_within(root, project, wait),
            None => ProjectStore::open(root, project).map(Some),
        }
        .map_err(|error| (Incomplete::Busy, error.into()))
    };
    let busy = || {
        eprintln!("hgs swarm: {alias}: the tasks of {project:?} are busy; the next sync continues");
        Ok(Err((Incomplete::Busy, false)))
    };
    let (node, swarm, own_node) = {
        let store = LockedStore::load(config).map_err(unreachable)?;
        let peer = store
            .data
            .peers
            .get(alias)
            .ok_or_else(|| (Incomplete::ChangedIdentity, anyhow::anyhow!("{alias} is not a member of this swarm")))?;
        (peer.node.clone(), store.data.catalog.swarm_id.clone(), store.data.node_id.clone())
    };
    let mut shown = Shown::default();
    for _ in 0..MAX_PAGES {
        let reply = transport::remote(
            config,
            alias,
            "replica-inventory",
            Some(&json!({"swarm_id": swarm, "project": project})),
        )
        .map_err(unreachable)?;
        if reply["node_id"] != node.as_str() {
            return Err(changed_identity());
        }
        answered(&reply, &mut shown)?;
        let theirs: Inventory = serde_json::from_value(reply["inventory"].clone()).map_err(|e| malformed(e.into()))?;
        shown.damage(reply["damaged"].as_u64());
        // The project's lock is held only between network calls.
        let (mine, page, more_mine) = {
            let Some(store) = open(&root)? else {
                return busy();
            };
            let (mine, here) = store.inventory().map_err(|e| (Incomplete::Busy, e.into()))?;
            shown.damage(Some(here.len() as u64));
            let (page, more) = store.updates_for(&mine, &theirs, PAGE).map_err(|e| (Incomplete::Busy, e.into()))?;
            (mine, page, more)
        };
        let sent = page.len();
        let response = transport::remote(
            config,
            alias,
            "replica-exchange",
            Some(&json!({"swarm_id": swarm, "project": project, "inventory": mine, "updates": encode(page)})),
        )
        .map_err(unreachable)?;
        if response["node_id"] != node.as_str() {
            return Err(changed_identity());
        }
        answered(&response, &mut shown)?;
        let updates:Vec<Wire> = serde_json::from_value(response["updates"].clone()).map_err(|e| malformed(e.into()))?;
        let Some(more_theirs) = response["more"].as_bool() else {
            return Err(malformed(anyhow::anyhow!("no page marker")));
        };
        // What the member did with this page: both its count and its list of
        // refusals, each in a form this reads, or unknown. It changed or
        // refused at most the documents the page carried; a larger count is
        // unknown too, never added up.
        let import = response["imported"]
            .as_u64()
            .and_then(|imported| usize::try_from(imported).ok())
            .zip(serde_json::from_value::<Vec<Damaged>>(response["refused"].clone()).ok())
            .filter(|(imported, refused)| imported.checked_add(refused.len()).is_some_and(|count| count <= sent));
        if let Some((_, refused)) = &import {
            for document in refused {
                eprintln!("hgs swarm: {alias}: task {} of {project} was refused by the peer: {}", document.uuid, document.error);
            }
        }
        shown.refusals(import.as_ref().map(|(_, refused)| !refused.is_empty()));
        // The member's count after this page, not only before it.
        shown.damage(response["damaged"].as_u64());
        let mut changed = import.map_or(0, |(imported, _)| imported);
        if !updates.is_empty() {
            let Some(store) = open(&root)? else {
                return busy();
            };
            let imported = import_answer(&store,updates,&own_node,&node,bootstrap_source(&response,observed),response["cutover_capability"]==super::fence::FENCE).map_err(|e| (Incomplete::Busy, e))?;
            changed += imported.changed;
            // A refused document is named in the worker's log; the others are kept.
            for document in &imported.refused {
                eprintln!("hgs swarm: {alias}: task {} of {project} was refused: {}", document.uuid, document.error);
            }
            shown.refusals(Some(!imported.refused.is_empty()));
        }
        if !more_mine && !more_theirs {
            // And this computer's count once more, after its own import.
            let Some(store) = open(&root)? else {
                return busy();
            };
            let (_, here) = store.inventory().map_err(|e| (Incomplete::Busy, e.into()))?;
            shown.damage(Some(here.len() as u64));
            return Ok(shown.outcome());
        }
        // A page that changed nothing on either side would come again unchanged.
        if changed == 0 {
            eprintln!("hgs swarm: {alias}: the rest of the tasks of {project} were refused; the next sync tries again");
            return Ok(shown.stalled());
        }
    }
    eprintln!("hgs swarm: {alias}: the tasks of {project} did not finish in {MAX_PAGES} pages; the next sync continues");
    Ok(Err((Incomplete::PageBudget, false)))
}

#[cfg(test)]
mod tests {
    //! review-n and desktop-u on #1432: complete means the project's whole
    //! inventory, and every project known here gets an outcome.
    use super::*;
    use crate::swarm::store::Peer;
    use std::fs;
    use zerus_replica::{Actor, NewTask, Stage, Target, TaskDoc};

    const NODE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const STORE: &str = "11111111-1111-4111-8111-111111111111";

    fn fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            home: dir.path().to_str().unwrap().into(),
            dir: dir.path().join("config"),
            state: dir.path().join("state"),
            selves: vec!["example-local".into()],
            peers: vec!["peer".into()],
            base_peers: vec![],
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        };
        let mut store = LockedStore::load(&config).unwrap();
        store.data.peers.insert(
            "peer".into(),
            Peer {
                node: NODE.into(),
                ..Peer::default()
            },
        );
        store.save().unwrap();
        drop(store);
        (dir, config)
    }

    fn add(config: &Config, project: &str) -> String {
        let store = ProjectStore::open(&crate::tasks::replica_root(config).unwrap(), project).unwrap();
        let mut task = TaskDoc::create(NewTask {
            project,
            id: &store.unused_short_id().unwrap(),
            text: "Add rate limiting",
            stage: Stage::Todo,
            target: Target::Any,
            author: Actor::Person,
            rank: "V",
            now: 1,
        })
        .unwrap();
        store.save(&mut task).unwrap();
        task.uuid().to_owned()
    }

    /// A member's hello with no damaged task: a count of 0 for every project.
    fn hello(projects: Value) -> Value {
        let damaged: BTreeMap<String, usize> = projects
            .as_object()
            .map(|projects| projects.keys().map(|p| (p.clone(), 0)).collect())
            .unwrap_or_default();
        json!({"node_id": NODE, "replica": {"protocol": PROTOCOL, "task_schema": TASK_SCHEMA,
            "store": STORE, "projects": projects, "damaged": damaged, "omitted": []}})
    }

    fn outcome(config: &Config, project: &str) -> replica_members::Exchanged {
        replica_members::read(config)["peer"].projects[project].clone()
    }

    /// A complete outcome recorded earlier, under the same observation.
    fn complete_before(config: &Config, project: &str) {
        let observed = Observed::of(NODE, &hello(json!({})));
        replica_members::seen(config, "peer", &observed, 10).unwrap();
        let known = BTreeSet::from([project.to_owned()]);
        replica_members::exchanged(config, "peer", &observed, &[(project.into(), Ok(()))], &known, 20)
            .unwrap();
        assert!(outcome(config, project).complete);
    }

    #[test]
    fn a_hello_alone_cannot_fill_in_a_missing_response_store() {
        assert_eq!(bootstrap_source(&json!({}),Some(STORE)),None);
        assert_eq!(bootstrap_source(&json!({"store":STORE}),None),None);
        assert_eq!(bootstrap_source(&json!({"store":NODE}),Some(STORE)),None);
        assert_eq!(bootstrap_source(&json!({"store":STORE}),Some(STORE)),Some(STORE));
    }

    #[test]
    fn absent_project_digests_require_an_exchange() {
        // A catalog project with no tasks has no store/digest on either PC.
        // None == None used to skip its exchange forever, then report missing
        // health counts as malformed. Only actual equal digests may skip it.
        for (ours, theirs, agrees) in [
            (None, None, false),
            (Some("empty"), None, false),
            (None, Some("empty"), false),
            (Some("a"), Some("b"), false),
            (Some("empty"), Some("empty"), true),
        ] {
            let local = Digests {
                projects: ours.map(|digest| BTreeMap::from([("p".into(), digest.into())])).unwrap_or_default(),
                ..Digests::default()
            };
            let remote = super::theirs(&json!({
                "projects": theirs.map(|digest| BTreeMap::from([("p", digest)])).unwrap_or_default(),
                "damaged": {}, "omitted": [],
            })).unwrap();
            assert_eq!(remote.agrees(&local, "p"), agrees, "{ours:?}, {theirs:?}");
        }
    }

    #[test]
    fn unchanged_document_failures_wait_until_hourly_or_explicit_retry() {
        for reason in [Incomplete::RefusedDocuments, Incomplete::Malformed] {
            let (_dir, config) = fixture();
            add(&config, "p");
            let mut remote = hello(json!({"p": "a".repeat(64)}));
            remote["version"] = json!("1.58.0");
            let calls = std::cell::Cell::new(0);
            let mut member = |project: &str, _: Option<&str>| {
                assert_eq!(project, "p");
                calls.set(calls.get() + 1);
                Ok(Err((reason, false)))
            };
            sync_peer_with(&config, "peer", &remote, false, 1_000, &mut member).unwrap();
            assert_eq!(calls.get(), 1);
            let first = outcome(&config, "p");
            assert!(first.divergence.is_some());
            // Frequent skipped rounds keep the actual attempt time, rather
            // than extending the retry forever or claiming another exchange.
            for at in [1_001, 2_000, 1_000 + replica_members::DIVERGENCE_RETRY_MS - 1] {
                sync_peer_with(&config, "peer", &remote, false, at, &mut member).unwrap();
                assert_eq!(calls.get(), 1);
                assert_eq!(outcome(&config, "p"), first);
            }
            let due = 1_000 + replica_members::DIVERGENCE_RETRY_MS;
            sync_peer_with(&config, "peer", &remote, false, due, &mut member).unwrap();
            assert_eq!(calls.get(), 2);
            sync_peer_with(&config, "peer", &remote, true, due + 1, &mut member).unwrap();
            assert_eq!(calls.get(), 3, "manual sync never waits for the hourly retry");
            sync_peer_with(&config, "peer", &remote, false, due, &mut member).unwrap();
            assert_eq!(calls.get(), 4, "clock rollback cannot extend a cached failure");
        }
    }

    #[test]
    fn a_changed_digest_or_member_observation_retries_at_once() {
        for change in ["local", "peer", "store", "node", "schema", "version"] {
            let (_dir, config) = fixture();
            let uuid = add(&config, "p");
            let mut remote = hello(json!({"p": "a".repeat(64)}));
            remote["version"] = json!("1.58.0");
            let calls = std::cell::Cell::new(0);
            let mut member = |_: &str, _: Option<&str>| {
                calls.set(calls.get() + 1);
                Ok(Err((Incomplete::RefusedDocuments, false)))
            };
            sync_peer_with(&config, "peer", &remote, false, 1_000, &mut member).unwrap();
            sync_peer_with(&config, "peer", &remote, false, 1_001, &mut member).unwrap();
            assert_eq!(calls.get(), 1, "{change}");
            match change {
                "local" => {
                    let store = ProjectStore::open(&root(&config), "p").unwrap();
                    let mut task = store.load(&uuid).unwrap();
                    task.comment("Changed", &Actor::Person, 2).unwrap();
                    store.save(&mut task).unwrap();
                }
                "peer" => remote["replica"]["projects"]["p"] = json!("b".repeat(64)),
                "store" => remote["replica"]["store"] = json!("22222222-2222-4222-8222-222222222222"),
                "node" => {
                    let mut catalog = LockedStore::load(&config).unwrap();
                    catalog.data.peers.get_mut("peer").unwrap().node = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
                    remote["node_id"] = json!("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
                    catalog.save().unwrap();
                }
                "schema" => remote["replica"]["task_schema"] = json!(TASK_SCHEMA + 1),
                "version" => remote["version"] = json!("1.58.1"),
                _ => unreachable!(),
            }
            sync_peer_with(&config, "peer", &remote, false, 1_002, &mut member).unwrap();
            assert_eq!(calls.get(), 2, "{change} must invalidate the memo");
        }
    }

    #[test]
    fn transient_failures_and_older_peers_never_defer_an_exchange() {
        for reason in [Incomplete::Busy, Incomplete::Unreachable, Incomplete::PageBudget,
            Incomplete::ChangedIdentity, Incomplete::DamagedTasks, Incomplete::Omitted] {
            let (_dir, config) = fixture();
            add(&config, "p");
            let mut remote = hello(json!({"p": "a".repeat(64)}));
            remote["version"] = json!("1.58.0");
            let calls = std::cell::Cell::new(0);
            let mut member = |_: &str, _: Option<&str>| {
                calls.set(calls.get() + 1);
                Ok(Err((reason, false)))
            };
            for at in [1_000, 1_001] {
                sync_peer_with(&config, "peer", &remote, false, at, &mut member).unwrap();
            }
            assert_eq!(calls.get(), 2, "{reason:?}");
            assert!(outcome(&config, "p").divergence.is_none());
        }
        for missing in ["version", "store", "digest"] {
            let (_dir, config) = fixture();
            add(&config, "p");
            let mut remote = hello(json!({"p": "a".repeat(64)}));
            remote["version"] = json!("1.58.0");
            match missing {
                "version" => { remote.as_object_mut().unwrap().remove("version"); }
                "store" => remote["replica"]["store"] = Value::Null,
                "digest" => remote["replica"]["projects"]["p"] = json!("unknown"),
                _ => unreachable!(),
            }
            let calls = std::cell::Cell::new(0);
            let mut member = |_: &str, _: Option<&str>| {
                calls.set(calls.get() + 1);
                Ok(Err((Incomplete::RefusedDocuments, false)))
            };
            for at in [1_000, 1_001] {
                sync_peer_with(&config, "peer", &remote, false, at, &mut member).unwrap();
            }
            assert_eq!(calls.get(), 2, "unknown {missing} cannot establish an unchanged failure");
        }
    }

    #[test]
    fn six_settled_divergences_make_no_exchange_calls_in_100_seconds() {
        let (_dir, config) = fixture();
        let projects: BTreeMap<String, String> = (0..6).map(|n| {
            let project = format!("p{n}");
            add(&config, &project);
            (project, "a".repeat(64))
        }).collect();
        let mut remote = hello(json!(projects));
        remote["version"] = json!("1.58.0");
        let calls = std::cell::Cell::new(0);
        let mut member = |_: &str, _: Option<&str>| {
            calls.set(calls.get() + 1);
            Ok(Err((Incomplete::RefusedDocuments, false)))
        };
        sync_peer_with(&config, "peer", &remote, false, 0, &mut member).unwrap();
        assert_eq!(calls.get(), 6, "one initial exchange per conflicting project");
        // The worker's 30 second cadence plus an extra wake at the end of a
        // 100 second steady window.
        // Each callback represents its inventory + exchange RPC pair.
        for at in [30_000, 60_000, 90_000, 100_000] {
            sync_peer_with(&config, "peer", &remote, false, at, &mut member).unwrap();
            assert_eq!(calls.get(), 6);
            for project in projects.keys() {
                let saved = outcome(&config, project);
                assert_eq!(saved.at, 0);
                assert!(!saved.complete);
            }
        }
        // Postponing those six never prevents a different project advancing.
        add(&config, "new-project");
        remote["replica"]["projects"]["new-project"] = json!("b".repeat(64));
        remote["replica"]["damaged"]["new-project"] = json!(0);
        sync_peer_with(&config, "peer", &remote, false, 101_000, &mut member).unwrap();
        assert_eq!(calls.get(), 7);
    }

    #[test]
    fn the_protocol_only_worker_rotates_projects_even_when_none_can_advance() {
        let (_dir,config)=fixture();
        for project in ["a","b","c"] {add(&config,project);}
        struct NoCalls;
        impl super::super::fence::Net for NoCalls {
            fn call(&self,_:&str,_:super::super::fence::Route<'_>,_:&str,_:Option<&Value>,_:Instant)->Result<Value> {
                panic!("an index candidate cannot itself authorize networking or task adoption")
            }
        }
        let mut worker=CutoverScan::default();
        for expected in ["a","b","c","a"] {
            assert!(!worker.advance_with(&config,&NoCalls).unwrap());
            assert_eq!(worker.after_project,expected);
        }
        let mut manual=CutoverScan::default();
        assert!(!manual.advance_with(&config,&NoCalls).unwrap());
        assert_eq!(manual.after_project,"b","one-shot sync continues the persisted project rotation");
    }

    #[test]
    fn a_qualified_sync_resumes_the_original_after_a_pin_before_canonical_cut() {
        let source_root=tempfile::tempdir().unwrap();
        let source=ProjectStore::open(source_root.path(),"example").unwrap();
        let mut task=TaskDoc::create(NewTask {project:"example",id:"T-abcdef",text:"Original",stage:Stage::Todo,
            target:Target::Any,author:Actor::Person,rank:"V",now:1}).unwrap();
        source.save(&mut task).unwrap();
        let source_store=ProjectStore::store_id(source_root.path()).unwrap();
        let dir=tempfile::tempdir().unwrap();
        let receiver=ProjectStore::open(dir.path(),"example").unwrap();
        let own="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let (capture,_)=receiver.prepare_cutover(task.uuid(),own).unwrap();
        let bytes=task.snapshot().unwrap();
        receiver.retain_cutover_proposal(&bytes,&capture).unwrap();
        let tasks=receiver.dir().join("tasks");
        let prepare=tasks.join(format!("{}.prepare",task.uuid()));
        let proposal=tasks.join(format!("{}.proposal",task.uuid()));
        let original=[fs::read(&prepare).unwrap(),fs::read(&proposal).unwrap()];
        receiver.catch_up_cutover(&bytes,&capture,NODE,&source_store,&task.cutover_seal().unwrap().unwrap().identity()).unwrap();
        // Recreate the exact partial state using the real pin produced above.
        for ext in ["loro","updates","managed"] {
            let path=tasks.join(format!("{}.{ext}",task.uuid()));
            if path.exists(){fs::remove_file(path).unwrap();}
        }
        fs::write(&prepare,&original[0]).unwrap();
        fs::write(&proposal,&original[1]).unwrap();
        assert!(receiver.load(task.uuid()).is_err());
        let wire=encode_admitted(&source,vec![Update{document:Document::Task(task.uuid().into()),bytes}]);
        let imported=import_answer(&receiver,wire,own,NODE,Some(&source_store),true).unwrap();
        assert!(imported.refused.is_empty());
        assert_eq!(receiver.load(task.uuid()).unwrap().version(),task.version());
        assert!(!prepare.exists() && !proposal.exists());
    }

    #[test]
    fn only_a_bound_already_admitted_source_bootstraps_a_fresh_receiver() {
        let source_root = tempfile::tempdir().unwrap();
        let source = ProjectStore::open(source_root.path(), "example").unwrap();
        let mut task = TaskDoc::create(NewTask {
            project: "example", id: "T-abcdef", text: "Original", stage: Stage::Todo,
            target: Target::Any, author: Actor::Person, rank: "V", now: 1,
        }).unwrap();
        source.save(&mut task).unwrap();
        let source_store = ProjectStore::store_id(source_root.path()).unwrap();
        let update = || vec![Update {document: Document::Task(task.uuid().into()), bytes: task.snapshot().unwrap()}];
        for (qualified, known_store) in [(false, true), (true, false)] {
            let dir = tempfile::tempdir().unwrap();
            let receiver = ProjectStore::open(dir.path(), "example").unwrap();
            let outcome = import_answer(&receiver, encode_admitted(&source, update()),
                "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", NODE,
                known_store.then_some(source_store.as_str()), qualified).unwrap();
            assert_eq!(outcome.changed, 0);
            assert_eq!(outcome.refused.len(), 1);
            assert!(receiver.admitted_cutover_identity(task.uuid()).unwrap().is_none());
        }
        let dir = tempfile::tempdir().unwrap();
        let receiver = ProjectStore::open(dir.path(), "example").unwrap();
        let ordinary = receiver.import_updates(update()).unwrap();
        assert_eq!(ordinary.refused.len(), 1, "a snapshot is only candidate data");
        let outcome = import_answer(&receiver, encode_admitted(&source, update()),
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", NODE, Some(&source_store), true).unwrap();
        assert_eq!(outcome.changed, 1);
        assert!(outcome.refused.is_empty());
        assert_eq!(receiver.admitted_cutover_identity(task.uuid()).unwrap(), source.admitted_cutover_identity(task.uuid()).unwrap());
        let mut next = receiver.load(task.uuid()).unwrap();
        next.comment("After automatic catch-up", &Actor::Person, 3).unwrap();
        receiver.save(&mut next).unwrap();
    }

    #[test]
    fn a_current_relay_cannot_annotate_unadmitted_candidate_bytes_as_source_proof() {
        let dir = tempfile::tempdir().unwrap();
        let source = ProjectStore::open(dir.path(), "example").unwrap();
        let mut task = TaskDoc::create(NewTask {
            project: "example", id: "T-abcdef", text: "Legacy root", stage: Stage::Todo,
            target: Target::Any, author: Actor::Person, rank: "V", now: 1,
        }).unwrap();
        task.floor().unwrap();
        task.seal_cutover(BTreeMap::new(), BTreeMap::new()).unwrap();
        source.save_legacy_for_test(&mut task).unwrap();
        let wire = encode_admitted(&source, vec![Update {document:Document::Task(task.uuid().into()), bytes:task.snapshot().unwrap()}]);
        assert!(wire[0].admitted_seal.is_none(), "a copied seal cannot mint source custody");
    }

    #[test]
    fn a_project_left_out_of_both_digest_passes_is_not_complete() {
        let (_dir, config) = fixture();
        add(&config, "p");
        complete_before(&config, "p");
        let _held = ProjectStore::open(&root(&config), "p").unwrap();
        sync_peer(&config, "peer", &hello(json!({})), true).unwrap();
        let after = outcome(&config, "p");
        assert!(!after.complete && !after.exchanged);
        assert_eq!(after.reason, Some(Incomplete::Busy));
    }

    #[test]
    fn equal_readable_digests_with_a_damaged_task_are_not_complete() {
        let (_dir, config) = fixture();
        add(&config, "p");
        let broken = add(&config, "p");
        let store = ProjectStore::open(&root(&config), "p").unwrap();
        fs::write(
            store.dir().join("tasks").join(format!("{broken}.loro")),
            b"damaged",
        )
        .unwrap();
        let digest = store.inventory().unwrap().0.digest();
        drop(store);
        complete_before(&config, "p");
        sync_peer(&config, "peer", &hello(json!({"p": digest})), true).unwrap();
        let after = outcome(&config, "p");
        assert!(!after.complete && after.exchanged, "the readable subset agrees, the project is not complete");
        assert_eq!(after.reason, Some(Incomplete::DamagedTasks));
        // The member saying a task of it is damaged there does the same.
        let (_dir, config) = fixture();
        add(&config, "p");
        let digest = ProjectStore::open(&root(&config), "p").unwrap().inventory().unwrap().0.digest();
        complete_before(&config, "p");
        let mut remote = hello(json!({"p": digest}));
        remote["replica"]["damaged"] = json!({"p": 1});
        sync_peer(&config, "peer", &remote, true).unwrap();
        assert_eq!(outcome(&config, "p").reason, Some(Incomplete::DamagedTasks));
    }

    /// review-n on #1432 R3: a hello that holds the project but gives no
    /// damaged count for it never makes it complete (a count in another
    /// form makes the whole map unreadable, below). The explicit 0 is the
    /// control.
    #[test]
    fn a_project_with_no_damaged_count_in_the_hello_is_not_complete() {
        for damaged in [json!({}), json!({"q": 0}), json!({"p": 0})] {
            let (_dir, config) = fixture();
            add(&config, "p");
            let digest = ProjectStore::open(&root(&config), "p").unwrap().inventory().unwrap().0.digest();
            complete_before(&config, "p");
            let mut remote = hello(json!({"p": digest}));
            remote["replica"]["damaged"] = damaged.clone();
            sync_peer(&config, "peer", &remote, true).unwrap();
            let after = outcome(&config, "p");
            if damaged == json!({"p": 0}) {
                assert!(after.complete, "{after:?}");
            } else {
                assert!(!after.complete && after.exchanged, "{damaged}: {after:?}");
                assert_eq!(after.reason, Some(Incomplete::Malformed), "{damaged}");
            }
        }
        // The hello this computer sends gives every readable project its count.
        let (_dir, config) = fixture();
        add(&config, "p");
        assert_eq!(super::hello(&config).unwrap()["damaged"], json!({"p": 0}));
    }

    /// The evidence of one exchange decides in order: a known refusal, then
    /// unknown refusals, known damage, unknown damage; only all known and
    /// none is complete.
    #[test]
    fn exchange_evidence_is_complete_only_when_known_and_none() {
        let shown = |steps: &[(Option<bool>, Option<u64>)]| {
            let mut shown = Shown::default();
            for &(refused, damaged) in steps {
                shown.refusals(refused);
                shown.damage(damaged);
            }
            shown
        };
        assert_eq!(shown(&[(Some(false), Some(0))]).outcome(), Ok(()));
        assert_eq!(shown(&[(Some(false), Some(0)), (None, Some(0))]).outcome(), Err((Incomplete::Malformed, false)));
        assert_eq!(shown(&[(Some(false), None)]).outcome(), Err((Incomplete::Malformed, true)));
        assert_eq!(shown(&[(Some(false), None), (Some(false), Some(2))]).outcome(), Err((Incomplete::DamagedTasks, true)));
        assert_eq!(shown(&[(None, None), (Some(true), Some(0))]).outcome(), Err((Incomplete::RefusedDocuments, false)));
        assert_eq!(shown(&[(Some(false), Some(0))]).stalled(), Err((Incomplete::RefusedDocuments, false)));
        assert_eq!(shown(&[(None, Some(0))]).stalled(), Err((Incomplete::Malformed, false)));
        assert_eq!(shown(&[(None, Some(0)), (Some(true), Some(0))]).stalled(), Err((Incomplete::RefusedDocuments, false)));
    }

    /// desktop-u on #1432 R2: this computer's own digest pass failing (its
    /// replica root unreadable) records every project known here as
    /// incomplete, apart from a per-project busy and a failed hello.
    #[test]
    fn a_failed_local_digest_pass_never_keeps_a_prior_complete_inventory() {
        let (_dir, config) = fixture();
        add(&config, "p");
        let mut catalog = LockedStore::load(&config).unwrap();
        let node = catalog.data.node_id.clone();
        catalog
            .data
            .catalog
            .set(&node, crate::swarm::catalog::Key::project("p", "alive"), json!(true))
            .unwrap();
        catalog.save().unwrap();
        drop(catalog);
        complete_before(&config, "p");
        let path = root(&config);
        fs::rename(&path, config.state.join("replica-kept")).unwrap();
        fs::write(&path, b"not a directory").unwrap();
        assert!(sync_peer(&config, "peer", &hello(json!({})), true).is_err());
        let after = outcome(&config, "p");
        assert!(!after.complete && !after.exchanged, "{after:?}");
        assert_eq!(after.reason, Some(Incomplete::Busy));
    }

    #[test]
    fn a_malformed_project_map_never_makes_a_project_complete() {
        for projects in [json!("not a map"), json!({"p": 7}), Value::Null] {
            let (_dir, config) = fixture();
            add(&config, "p");
            complete_before(&config, "p");
            sync_peer(&config, "peer", &hello(projects.clone()), true).unwrap();
            let after = outcome(&config, "p");
            assert!(!after.complete, "{projects}");
            assert_eq!(after.reason, Some(Incomplete::Malformed), "{projects}");
        }
        // Equal digests with a damaged map, or a count, in another form.
        for damaged in [json!("none"), Value::Null, json!({"p": "0"}), json!({"p": -1}), json!({"p": 0.5})] {
            let (_dir, config) = fixture();
            add(&config, "p");
            let digest = ProjectStore::open(&root(&config), "p").unwrap().inventory().unwrap().0.digest();
            complete_before(&config, "p");
            let mut remote = hello(json!({"p": digest}));
            remote["replica"]["damaged"] = damaged.clone();
            sync_peer(&config, "peer", &remote, true).unwrap();
            let after = outcome(&config, "p");
            assert!(!after.complete, "{damaged}");
            assert_eq!(after.reason, Some(Incomplete::Malformed), "{damaged}");
        }
    }
}
