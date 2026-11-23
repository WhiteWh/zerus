//! What this computer last learned of each member's task replica, kept
//! beside the catalog in `replica-members.json`: what its last verified
//! hello said (the replica protocol, the task schema it writes, its replica
//! store) and each project's last exchange with it (zerus-cg5h.4 S2b.2 C2).
//! Information only: it grants nothing. Every record is bound to the exact
//! verified observation it came from, a changed observation retires the
//! completions recorded under the old one, and a member's entry goes at the
//! membership change itself.
use super::store::{replace, LockedStore, Peer};
use crate::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const FILE: &str = "replica-members.json";
/// A file larger than this is damaged and read as empty.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Even unchanged document failures are tried again after one hour.
pub const DIVERGENCE_RETRY_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The verified node this entry belongs to.
    pub node: String,
    /// The replica protocol its last hello spoke; none when it said none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u64>,
    /// Its last verified hello had no replica key at all: it does not share
    /// tasks, so it holds none of a project's documents. A replica part
    /// present in a form this cannot read is not this: it stays eligible and
    /// keeps a cutover open as an unknown protocol.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_replica: bool,
    /// The highest task schema its last hello advertised; none for an hgs
    /// that does not say, which counts as older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_schema: Option<i64>,
    /// The identity of its replica store from that hello.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
    /// When that hello was answered, in milliseconds.
    #[serde(default)]
    pub seen_at: u64,
    /// Each project's last exchange with it, under that observation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, Exchanged>,
}

/// How one project's last exchange with a member ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchanged {
    /// Both sides' whole inventories were exchanged: every page, nothing
    /// refused, no task damaged or left out on either side.
    pub complete: bool,
    /// The readable documents were exchanged in full, whether or not the
    /// project was complete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exchanged: bool,
    /// When it ended, in milliseconds.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Incomplete>,
    /// A failed exchange's exact digest pair. The enclosing Member binds it
    /// to the verified node/store/protocol/task schema; older hgs ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence: Option<Divergence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub local_digest: String,
    pub peer_digest: String,
    /// None when an older peer does not advertise its executable version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hgs_version: Option<String>,
}

impl Divergence {
    pub fn of(local: Option<&String>, peer: Option<&String>, version: Option<&str>) -> Option<Self> {
        let version = version.filter(|version| !version.is_empty() && version.len() <= 128)?;
        let (local, peer) = (local?, peer?);
        if ![local, peer].iter().all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())) {
            return None;
        }
        Some(Self {
            local_digest: local.clone(),
            peer_digest: peer.clone(),
            hgs_version: Some(version.to_owned()),
        })
    }
}

/// A memo can postpone only the exact document failure already observed.
/// Missing evidence, another identity/digest/version, clock rollback or the
/// retry deadline always makes another exchange due. Its original at stays.
pub fn deferred(member: Option<&Member>, observed: &Observed, project: &str, current: &Divergence, at: u64) -> bool {
    let Some(member) = member.filter(|member| observed.holds(member)) else { return false };
    // Without a known store incarnation we cannot establish the same peer.
    if observed.store.as_deref().is_none_or(|store| !zerus_replica::ids::is_uuid(store))
        || current.hgs_version.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    member.projects.get(project).is_some_and(|exchange| {
        !exchange.complete
            && matches!(exchange.reason, Some(Incomplete::RefusedDocuments | Incomplete::Malformed))
            && exchange.divergence.as_ref() == Some(current)
            && at.checked_sub(exchange.at).is_some_and(|elapsed| elapsed < DIVERGENCE_RETRY_MS)
    })
}

/// Why an exchange did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incomplete {
    /// The member could not be reached, or its answer could not be read.
    Unreachable,
    /// The project here was held by another command, or could not be read.
    Busy,
    /// A document was refused on either side.
    RefusedDocuments,
    /// The pages one sync may take ran out; the next sync continues.
    PageBudget,
    /// The member answered as another computer.
    ChangedIdentity,
    /// A task of the project does not load on one side: its readable
    /// documents can agree while the project is not complete.
    DamagedTasks,
    /// The member's digests left the project out (busy or unreadable there).
    Omitted,
    /// The member's answer did not say, in a form this reads, what it holds,
    /// refused or found damaged: absence is never taken for none.
    Malformed,
}

impl Incomplete {
    pub fn as_str(self) -> &'static str {
        match self {
            Incomplete::Unreachable => "unreachable",
            Incomplete::Busy => "busy",
            Incomplete::RefusedDocuments => "refused_documents",
            Incomplete::PageBudget => "page_budget",
            Incomplete::ChangedIdentity => "changed_identity",
            Incomplete::DamagedTasks => "damaged_tasks",
            Incomplete::Omitted => "omitted",
            Incomplete::Malformed => "malformed",
        }
    }
}

/// One project's outcome: complete, or why not and whether its readable
/// documents were exchanged all the same.
pub type Outcome = std::result::Result<(), (Incomplete, bool)>;

/// What a verified hello showed of a member. Records are written only for
/// this exact observation: a changed computer, store, protocol or schema
/// retires the completions of the old one, and an outcome that arrives for
/// an observation that is no longer the member's is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub node: String,
    pub protocol: Option<u64>,
    pub no_replica: bool,
    pub task_schema: Option<i64>,
    pub store: Option<String>,
}

impl Observed {
    /// What the hello `identity` (verified to come from `node`) says. Only a
    /// hello without the replica key proves the member shares no tasks.
    pub fn of(node: &str, identity: &Value) -> Self {
        let replica = &identity["replica"];
        Self {
            node: node.to_owned(),
            protocol: replica["protocol"].as_u64(),
            no_replica: identity.get("replica").is_none(),
            task_schema: replica["task_schema"].as_i64(),
            store: replica["store"].as_str().map(str::to_owned),
        }
    }

    fn holds(&self, member: &Member) -> bool {
        member.node == self.node
            && member.protocol == self.protocol
            && member.no_replica == self.no_replica
            && member.task_schema == self.task_schema
            && member.store == self.store
    }
}

/// Now, in milliseconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn path(config: &Config) -> PathBuf {
    config.dir.join("swarm").join(FILE)
}

fn read_at(path: &Path) -> BTreeMap<String, Member> {
    super::read_regular(path, MAX_BYTES)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The first reason `record` keeps `project`'s cutover open (C2), with when
/// it was observed: never seen, another replica protocol, an older task
/// schema, no valid store, no exchange of the project, one not complete
/// (whatever reason it gives, or none), or none since `latest`, the latest
/// cutover write here. It fails closed: anything not positively known as
/// complete keeps it open. Shared by the cutover status and the first
/// floor's bootstrap gate (C3 activation), so both read one definition.
pub fn open_reason(
    record: Option<&Member>,
    project: &str,
    latest: Option<i64>,
) -> Option<(&'static str, Option<i64>)> {
    let Some(replica) = record else {
        return Some(("unseen", None));
    };
    let seen = Some(replica.seen_at as i64);
    if replica.protocol != Some(super::replica::PROTOCOL) {
        return Some(("protocol", seen));
    }
    if replica
        .task_schema
        .is_none_or(|schema| schema < zerus_replica::TASK_SCHEMA)
    {
        return Some(("task_schema", seen));
    }
    if replica
        .store
        .as_deref()
        .is_none_or(|store| !zerus_replica::ids::is_uuid(store))
    {
        return Some(("store", seen));
    }
    let Some(exchanged) = replica.projects.get(project) else {
        return Some(("no_inventory", None));
    };
    let at = Some(exchanged.at as i64);
    if !exchanged.complete {
        return Some((
            exchanged.reason.map_or("incomplete", Incomplete::as_str),
            at,
        ));
    }
    if latest.is_some_and(|latest| (exchanged.at as i64) < latest) {
        return Some(("inventory_before_cutover", at));
    }
    None
}

/// Every member's entry, read without the lock. A missing, oversized or
/// damaged file reads as none; nothing at the path makes the read wait.
pub fn read(config: &Config) -> BTreeMap<String, Member> {
    read_at(&path(config))
}

/// Every member's entry, for a decision that must fail closed (C3e's
/// certificate reuse, desktop-u on #1471): a missing file is no entries, but
/// a damaged, oversized, unreadable or irregular one is an error, never an
/// empty record set that would compare as "nothing changed".
pub fn read_strict(
    config: &Config,
) -> std::result::Result<BTreeMap<String, Member>, super::Unread> {
    match super::read_regular(&path(config), MAX_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| super::Unread::Unreadable),
        Err(super::Unread::Missing) => Ok(BTreeMap::new()),
        Err(unread) => Err(unread),
    }
}

/// Keeps only the entries of current members, at the membership change
/// itself (the catalog store's save): a member removed and enrolled again
/// starts with no hello and no inventory. The catalog lock is held.
pub fn prune(path: &Path, peers: &BTreeMap<String, Peer>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut members = read_at(path);
    let before = members.len();
    members.retain(|alias, member| peers.get(alias).is_some_and(|peer| peer.node == member.node));
    if members.len() != before {
        replace(path, &serde_json::to_vec(&members)?)?;
    }
    Ok(())
}

/// Changes the member behind `alias` under the catalog lock, only while its
/// enrolment is still `node`; `projects` are the projects this computer
/// still holds or knows alive. False when nothing was written.
fn update(
    config: &Config,
    alias: &str,
    node: &str,
    projects: Option<&BTreeSet<String>>,
    change: impl FnOnce(&mut Member) -> bool,
) -> Result<bool> {
    let store = LockedStore::load(config)?;
    if store.data.peers.get(alias).is_none_or(|peer| peer.node != node) {
        return Ok(false);
    }
    let mut members = read(config);
    members.retain(|alias, member| {
        store
            .data
            .peers
            .get(alias)
            .is_some_and(|peer| peer.node == member.node)
    });
    let member = members.entry(alias.to_owned()).or_default();
    if member.node != node {
        *member = Member {
            node: node.to_owned(),
            ..Member::default()
        };
    }
    if !change(member) {
        return Ok(false);
    }
    if let Some(projects) = projects {
        for member in members.values_mut() {
            member.projects.retain(|project, _| projects.contains(project));
        }
    }
    replace(&path(config), &serde_json::to_vec(&members)?)?;
    drop(store);
    Ok(true)
}

/// Records what a verified hello showed of the member behind `alias`, only
/// while `observed.node` is still its enrolment. Any change from the last
/// observation retires the project completions recorded under it.
pub fn seen(config: &Config, alias: &str, observed: &Observed, at: u64) -> Result<bool> {
    update(config, alias, &observed.node, None, |member| {
        if !observed.holds(member) {
            member.projects.clear();
        }
        member.protocol = observed.protocol;
        member.no_replica = observed.no_replica;
        member.task_schema = observed.task_schema;
        member.store = observed.store.clone();
        member.seen_at = at;
        true
    })
}

/// Records projects' outcomes for the member behind `alias` in one write,
/// only while its enrolment and its recorded observation are still exactly
/// `observed`: an outcome begun under another observation is refused.
pub fn exchanged(
    config: &Config,
    alias: &str,
    observed: &Observed,
    outcomes: &[(String, Outcome)],
    projects: &BTreeSet<String>,
    at: u64,
) -> Result<bool> {
    exchanged_with_memos(config, alias, observed, outcomes, projects, &BTreeMap::new(), at)
}

/// Store an attempted exchange and its optional divergence memo atomically,
/// under the same observation guard as the outcome. Skipped projects are not
/// present in outcomes, so neither their status nor retry time is rewritten.
pub fn exchanged_with_memos(
    config: &Config,
    alias: &str,
    observed: &Observed,
    outcomes: &[(String, Outcome)],
    projects: &BTreeSet<String>,
    memos: &BTreeMap<String, Divergence>,
    at: u64,
) -> Result<bool> {
    if outcomes.is_empty() {
        return Ok(false);
    }
    update(config, alias, &observed.node, Some(projects), |member| {
        if !observed.holds(member) {
            return false;
        }
        for (project, outcome) in outcomes {
            let (complete, exchanged, reason) = match outcome {
                Ok(()) => (true, true, None),
                Err((reason, exchanged)) => (false, *exchanged, Some(*reason)),
            };
            member.projects.insert(
                project.clone(),
                Exchanged {
                    complete,
                    exchanged,
                    at,
                    reason,
                    divergence: memos.get(project).filter(|_| matches!(reason, Some(Incomplete::RefusedDocuments | Incomplete::Malformed))).cloned(),
                },
            );
        }
        true
    })
}

/// Records that no verified hello came from the member behind `alias`:
/// every project known here is incomplete for the observation recorded for
/// its current enrolment (never complete for another).
pub fn unreachable(
    config: &Config,
    alias: &str,
    reason: Incomplete,
    projects: &BTreeSet<String>,
    at: u64,
) -> Result<bool> {
    let Some(node) = LockedStore::load(config)?
        .data
        .peers
        .get(alias)
        .map(|peer| peer.node.clone())
    else {
        return Ok(false);
    };
    update(config, alias, &node, Some(projects), |member| {
        for project in projects {
            member.projects.insert(
                project.clone(),
                Exchanged {
                    complete: false,
                    exchanged: false,
                    at,
                    reason: Some(reason),
                    divergence: None,
                },
            );
        }
        !projects.is_empty()
    })
}

#[cfg(test)]
mod tests {
    //! review-n and desktop-u on #1432: records are bound to the exact
    //! verified observation and go with the membership.
    use super::*;
    use serde_json::json;
    use std::fs;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";

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
        enrol(&config, "node-a");
        (dir, config)
    }

    fn enrol(config: &Config, node: &str) {
        let mut store = LockedStore::load(config).unwrap();
        store.data.peers.insert(
            "peer".into(),
            Peer {
                node: node.into(),
                ..Peer::default()
            },
        );
        store.save().unwrap();
    }

    fn observed(node: &str, store: &str) -> Observed {
        Observed::of(
            node,
            &json!({"replica": {"protocol": 1, "task_schema": 7, "store": store}}),
        )
    }

    fn complete(config: &Config, observed: &Observed) -> bool {
        exchanged(config, "peer", observed, &[("p".into(), Ok(()))], &BTreeSet::from(["p".into()]), 20)
            .unwrap()
    }

    fn record(config: &Config) -> Option<Member> {
        read(config).remove("peer")
    }

    #[test]
    fn divergence_memos_are_optional_and_bound_to_the_complete_observation() {
        let (_dir, config) = fixture();
        let observed = observed("node-a", A);
        let local = "a".repeat(64);
        let peer = "b".repeat(64);
        let memo = Divergence::of(Some(&local), Some(&peer), Some("1.58.0")).unwrap();
        assert!(Divergence::of(Some(&local), Some(&peer), None).is_none());
        assert!(Divergence::of(Some(&local), None, Some("1.58.0")).is_none());
        let legacy: Exchanged = serde_json::from_value(json!({"complete":false,"at":10,"reason":"refused_documents"})).unwrap();
        assert!(legacy.divergence.is_none(), "older records never establish an unchanged digest pair");
        seen(&config, "peer", &observed, 10).unwrap();
        let projects = BTreeSet::from(["p".into()]);
        let memos = BTreeMap::from([("p".into(), memo.clone())]);
        exchanged_with_memos(&config, "peer", &observed, &[("p".into(), Err((Incomplete::RefusedDocuments, false)))], &projects, &memos, 10).unwrap();
        let current = record(&config).unwrap();
        assert!(deferred(Some(&current), &observed, "p", &memo, 11));
        for change in [
            Observed { node: "node-b".into(), ..observed.clone() },
            Observed { store: Some(B.into()), ..observed.clone() },
            Observed { protocol: Some(2), ..observed.clone() },
            Observed { task_schema: Some(8), ..observed.clone() },
            Observed { no_replica: true, ..observed.clone() },
        ] {
            assert!(!deferred(Some(&current), &change, "p", &memo, 11), "{change:?}");
        }
        assert!(!deferred(Some(&current), &observed, "p", &Divergence { hgs_version: Some("1.58.1".into()), ..memo.clone() }, 11));
        assert!(!deferred(Some(&current), &observed, "p", &memo, 10 + DIVERGENCE_RETRY_MS));
        assert!(!deferred(Some(&current), &observed, "p", &memo, 9));
        // Neither an outage, a transient failure nor success preserves a
        // prior document-failure memo. Its next exchange must be attempted.
        unreachable(&config, "peer", Incomplete::Unreachable, &projects, 12).unwrap();
        assert!(record(&config).unwrap().projects["p"].divergence.is_none());
        for outcome in [Err((Incomplete::Busy, false)), Ok(())] {
            exchanged_with_memos(&config, "peer", &observed, &[("p".into(), outcome)], &projects, &memos, 13).unwrap();
            assert!(record(&config).unwrap().projects["p"].divergence.is_none());
        }
        let changed = Observed { store: Some(B.into()), ..observed.clone() };
        seen(&config, "peer", &changed, 14).unwrap();
        assert!(!exchanged_with_memos(&config, "peer", &observed, &[("p".into(), Err((Incomplete::Malformed, false)))], &projects, &memos, 15).unwrap());
        assert!(record(&config).unwrap().projects.is_empty(), "a late memo cannot cross store incarnations");
    }

    /// review-win-c on #1471: no read of the records waits on what is at
    /// their path. The ordinary read takes anything unusable as none; the
    /// strict one (certificate reuse) refuses it and takes only a missing
    /// file as none.
    #[test]
    fn the_records_are_read_without_waiting_and_strictly_when_asked() {
        use super::super::Unread;
        let (_dir, config) = fixture();
        let file = path(&config);
        assert_eq!(
            (read(&config), read_strict(&config)),
            (BTreeMap::new(), Ok(BTreeMap::new()))
        );
        assert!(seen(&config, "peer", &observed("node-a", A), 10).unwrap());
        assert_eq!(read_strict(&config).unwrap(), read(&config));
        assert_eq!(read(&config).len(), 1);
        fs::write(&file, b"{damaged").unwrap();
        assert_eq!(
            (read(&config), read_strict(&config)),
            (BTreeMap::new(), Err(Unread::Unreadable))
        );
        #[cfg(unix)]
        {
            fs::remove_file(&file).unwrap();
            let fifo = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            let (sent, received) = std::sync::mpsc::channel();
            let reader = config.clone();
            std::thread::spawn(move || {
                let _ = sent.send((read(&reader), read_strict(&reader)));
            });
            let Ok(both) = received.recv_timeout(std::time::Duration::from_secs(5)) else {
                let _ = fs::OpenOptions::new().write(true).open(&file);
                panic!("a read of the records waited on a FIFO");
            };
            assert_eq!(both, (BTreeMap::new(), Err(Unread::NotRegular)));
        }
    }

    #[test]
    fn a_changed_observation_retires_the_old_one_s_completions() {
        let (_dir, config) = fixture();
        let a = observed("node-a", A);
        assert!(seen(&config, "peer", &a, 10).unwrap());
        assert!(complete(&config, &a));
        assert!(record(&config).unwrap().projects["p"].complete);
        // The same hello again keeps them.
        assert!(seen(&config, "peer", &a, 11).unwrap());
        assert!(record(&config).unwrap().projects["p"].complete);
        // Another store, protocol or schema starts again.
        let changes = [
            observed("node-a", B),
            Observed { protocol: Some(2), ..a.clone() },
            Observed { task_schema: Some(6), ..a.clone() },
            Observed { store: None, ..a.clone() },
        ];
        for change in changes {
            assert!(seen(&config, "peer", &a, 12).unwrap());
            assert!(complete(&config, &a));
            assert!(seen(&config, "peer", &change, 13).unwrap());
            assert!(record(&config).unwrap().projects.is_empty(), "{change:?}");
            // An outcome begun under the old observation is refused.
            assert!(!complete(&config, &a), "{change:?}");
            assert!(record(&config).unwrap().projects.is_empty());
        }
    }

    #[test]
    fn a_late_outcome_or_hello_is_never_credited_to_a_replaced_member() {
        let (_dir, config) = fixture();
        let a = observed("node-a", A);
        assert!(seen(&config, "peer", &a, 10).unwrap());
        // The alias moves to node-b while node-a's exchange is outstanding.
        enrol(&config, "node-b");
        assert!(!complete(&config, &a));
        assert!(record(&config).is_none_or(|m| m.projects.is_empty() && m.node != "node-b"));
        // A hello verified for node-a is not node-b's capability either.
        assert!(!seen(&config, "peer", &a, 11).unwrap());
        assert!(record(&config).is_none_or(|m| m.node != "node-b" || m.store.is_none()));
        // node-b's own hello and outcome are its own.
        let b = observed("node-b", B);
        assert!(seen(&config, "peer", &b, 12).unwrap());
        assert!(complete(&config, &b));
        let member = record(&config).unwrap();
        assert_eq!((member.node.as_str(), member.store.as_deref()), ("node-b", Some(B)));
    }

    #[test]
    fn a_removed_member_enrolled_again_starts_with_nothing() {
        let (_dir, config) = fixture();
        let a = observed("node-a", A);
        assert!(seen(&config, "peer", &a, 10).unwrap());
        assert!(complete(&config, &a));
        let mut store = LockedStore::load(&config).unwrap();
        store.data.peers.remove("peer");
        store.save().unwrap();
        drop(store);
        assert!(record(&config).is_none(), "pruned at the removal itself");
        enrol(&config, "node-a");
        let members = crate::swarm::task_members(&config).unwrap();
        assert_eq!(members.len(), 1);
        assert!(members[0].replica.is_none());
    }

    #[test]
    fn an_unreachable_member_has_no_complete_project() {
        let (_dir, config) = fixture();
        let a = observed("node-a", A);
        assert!(seen(&config, "peer", &a, 10).unwrap());
        assert!(complete(&config, &a));
        let known = BTreeSet::from(["p".to_owned(), "q".to_owned()]);
        assert!(unreachable(&config, "peer", Incomplete::Unreachable, &known, 30).unwrap());
        let member = record(&config).unwrap();
        for project in ["p", "q"] {
            let outcome = &member.projects[project];
            assert!(!outcome.complete && !outcome.exchanged);
            assert_eq!(outcome.reason, Some(Incomplete::Unreachable));
        }
        // The observation is kept: it is the member's last verified one.
        assert_eq!(member.store.as_deref(), Some(A));
    }

    /// review-n on #1432 R2: only an absent replica key proves a member has
    /// no replica; a present one this cannot read keeps it eligible, as an
    /// unknown protocol.
    #[test]
    fn only_an_absent_replica_part_proves_the_member_has_no_replica() {
        let (_dir, config) = fixture();
        let none = Observed::of("node-a", &json!({"schema": 1}));
        assert!(none.no_replica && none.protocol.is_none());
        assert!(seen(&config, "peer", &none, 10).unwrap());
        assert!(crate::swarm::task_members(&config).unwrap().is_empty());
        for value in [Value::Null, json!(false), json!(true), json!(1), json!("unreadable"), json!([]), json!({})] {
            let present = Observed::of("node-a", &json!({"replica": value}));
            assert!(!present.no_replica && present.protocol.is_none(), "{value}");
            assert!(seen(&config, "peer", &present, 20).unwrap());
            let members = crate::swarm::task_members(&config).unwrap();
            assert_eq!(members.len(), 1, "{value}: the member stays eligible");
            assert!(members[0].replica.as_ref().is_some_and(|r| !r.no_replica && r.protocol.is_none()));
        }
        let other = Observed::of("node-a", &json!({"replica": {"protocol": 2}}));
        assert!(!other.no_replica && other.protocol == Some(2));
    }
}
