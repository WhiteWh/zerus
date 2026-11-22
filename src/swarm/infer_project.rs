//! Optional initial grouping. Never initialize a catalog, replace any explicit
//! membership (including a tombstone), infer by a path prefix, or create Git data.
use super::{
    catalog::{Catalog, Key, Operation},
    store::{LockedStore, Store},
};
use crate::config::Config;
use crate::state::Pinned;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

const INBOX: &str = "ungrouped";

/// Called only after a genuinely new native binding was created, never resume,
/// fork, listing, GUI inspection or adoption. Failures leave normal launch alone.
pub(crate) fn infer_new_session(config: &Config, name: &str, directory: &Path) {
    if !config.dir.join("swarm/catalog.json").is_file() {
        return;
    }
    let end = Instant::now() + Duration::from_secs(3);
    let attempt = (|| -> anyhow::Result<()> {
        let mut store = LockedStore::try_load_scoped(config)?;
        let project = infer(&store.data, name, directory, end, |directory| {
            crate::state::worktree_catalog_until(directory, end)
        });
        if let Some(project) = project {
            if Instant::now() >= end {
                return Ok(());
            }
            write_membership(&mut store, name, &project)?;
        }
        Ok(())
    })();
    // Contention, inaccessible folders and optional metadata failures never
    // become an agent start failure, an automatic retry, or a default assignment.
    let _ = attempt;
}

/// A fork's initial grouping, written once by this computer's catalog owner
/// after the fork's binding exists and its launch released the state lock:
/// the project of its source session (or of the archived source), or Inbox
/// when that project no longer exists. Never initializes a catalog or replaces
/// an explicit membership (including a tombstone). Each attempt holds the state
/// lock only to try the provider's binding lock once, confirm that `name` is
/// still run `run_id` with `conversation`, and try the catalog lock once, so
/// neither a busy catalog nor a busy provider lock holds the state lock
/// (zerus-wx3k, zerus-jx5o). Attempts share one 3 s cap; a failure never fails
/// the fork.
pub(crate) fn inherit_fork_project(
    config: &Config,
    name: &str,
    source: &str,
    archive: Option<&str>,
    run_id: &str,
    conversation: &Value,
) {
    if !config.dir.join("swarm/catalog.json").is_file() {
        return;
    }
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let attempt = crate::state::with_pinned_session_binding(name, run_id, conversation, end, || {
            let Ok(mut store) = LockedStore::try_load_scoped(config) else {
                return Ok(false);
            };
            if let Some(project) = inherited(&store.data, name, source, archive) {
                write_membership(&mut store, name, &project)?;
            }
            anyhow::Ok(true)
        });
        if !retry(&attempt) || Instant::now() >= end {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
/// This computer's membership of `name`, with its ID twin, saved alone.
fn write_membership(store: &mut LockedStore, name: &str, project: &str) -> anyhow::Result<()> {
    let key = Key::Session {
        machine: store.data.catalog.machine(&store.data.node_id),
        session: name.into(),
    };
    let actor = store.data.node_id.clone();
    store.data.catalog.set(&actor, key, json!(project))?;
    let local = [(actor.clone(), name.to_owned())];
    super::session_ids::write(&store.data.catalog, &mut store.data.twins, &actor, &local)?;
    store.save_catalog_only()
}

/// A parent link that places a new child (zerus-d5twt): one this computer
/// proved from its own process ancestry (`launch`). A claimed, adopted or
/// forwarded link stays presentation only and leaves folder inference alone.
pub(crate) fn placing_parent(link: &Value) -> bool {
    crate::state::parent_link::valid(link) && link["source"] == "launch"
}

/// A new child session's initial project (zerus-d5twt), written like a
/// fork's once the child's binding exists and its launch released the state
/// and provider locks: the project of the tracked parent that launched it,
/// or an explicit Inbox when that project no longer exists. A parent in
/// Inbox, without a single membership or with an unresolved one decides
/// nothing, and the child's folder is inferred in the same attempt, as for a
/// session without a parent. The parent's project wins over folder inference,
/// so a session tree stays together. Never initializes a catalog or replaces
/// an explicit membership (including a tombstone), and never moves the parent
/// or any existing session. Each attempt holds the state lock only to pin
/// `name` to `run_id` and `conversation` and tries the provider and catalog
/// locks once; a busy one is retried outside the state lock until the shared
/// 3 s cap. A failure never fails the launch.
pub(crate) fn place_child_session(
    config: &Config,
    name: &str,
    directory: &Path,
    run_id: &str,
    conversation: &Value,
    parent: &Value,
) {
    if !config.dir.join("swarm/catalog.json").is_file() {
        return;
    }
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let attempt =
            crate::state::with_pinned_session_binding(name, run_id, conversation, end, || {
                let Ok(mut store) = LockedStore::try_load_scoped(config) else {
                    return Ok(false);
                };
                let project = match from_parent(&store.data, name, parent) {
                    Parent::Project(project) => Some(project),
                    Parent::Keep => None,
                    Parent::Folder => infer(&store.data, name, directory, end, |directory| {
                        crate::state::worktree_catalog_until(directory, end)
                    }),
                };
                if let Some(project) = project.filter(|_| Instant::now() < end) {
                    write_membership(&mut store, name, &project)?;
                }
                anyhow::Ok(true)
            });
        if !retry(&attempt) || Instant::now() >= end {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
/// One deferred attempt, called only by this computer's catalog owner while
/// the queue holds this exact creation's state and provider binding locks.
/// Busy/unreadable evidence stays pending; any primary or label membership,
/// even a tombstone, wins over this creation hint.
pub(super) fn complete_pending(
    config: &Config,
    record: &Value,
    intent: &Value,
    end: Instant,
) -> anyhow::Result<bool> {
    let Ok(mut store) = LockedStore::try_load_scoped(config) else {
        return Ok(false);
    };
    if !store.data.imported {
        return Ok(true);
    }
    let name = record["name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing placement name"))?;
    let id = record["session_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing placement ID"))?;
    let actor = store.data.node_id.clone();
    let primary =
        store.data.id_memberships.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Initial placement membership evidence is unavailable")
        })?;
    if primary.resolve(&store.data.catalog, &actor, id)?.is_some()
        || store.data.catalog.heads().contains_key(&Key::Session {
            machine: store.data.catalog.machine(&actor),
            session: name.into(),
        })
    {
        return Ok(true);
    }
    let mut incomplete = false;
    let project = match from_parent(&store.data, name, &intent["parent"]) {
        Parent::Project(project) => Some(project),
        Parent::Keep => return Ok(true),
        Parent::Folder => infer(
            &store.data,
            name,
            Path::new(
                intent["directory"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing placement directory"))?,
            ),
            end,
            |directory| {
                let evidence = crate::state::worktree_catalog_until(directory, end);
                incomplete = !matches!(evidence["state"].as_str(), Some("ok" | "not_repo"))
                    || evidence["stale"] == true
                    || evidence["partial"] == true;
                evidence
            },
        ),
    };
    if Instant::now() >= end {
        return Ok(false);
    }
    if let Some(project) = project {
        store
            .data
            .id_memberships
            .as_mut()
            .expect("checked primary evidence")
            .set(&store.data.catalog, &actor, &actor, id, json!(project))?;
        write_membership(&mut store, name, &project)?;
    } else if incomplete {
        return Ok(false);
    }
    Ok(true)
}

/// What a new child's parent decides for the child's initial project.
#[derive(Debug, PartialEq)]
enum Parent {
    /// Write this membership: the parent's live project, or Inbox.
    Project(String),
    /// Nothing from the parent: infer the child's folder.
    Folder,
    /// Write nothing: an existing membership or no initialized catalog.
    Keep,
}
fn from_parent(store: &Store, name: &str, link: &Value) -> Parent {
    if !store.imported {
        return Parent::Keep;
    }
    let machine = store.catalog.machine(&store.node_id);
    let key = Key::Session {
        machine: machine.clone(),
        session: name.into(),
    };
    if store.catalog.heads().contains_key(&key) {
        return Parent::Keep;
    }
    if !placing_parent(link) || link["machine"].as_str() != Some(store.node_id.as_str()) {
        return Parent::Folder;
    }
    let heads = store.catalog.effective_heads();
    let Some(project) = parent_membership(store, &heads, &machine, link) else {
        return Parent::Folder;
    };
    if project == INBOX {
        return Parent::Folder;
    }
    let alive = Key::project(&project, "alive");
    let live = heads
        .get(&alive)
        .and_then(|versions| Catalog::selected(&alive, versions))
        .is_some_and(|op| op.value == true);
    Parent::Project(if live { project } else { INBOX.into() })
}
/// The parent's single current project, read as a project move reads it: its
/// ID membership when it has one, else its label's. Unassigned, unresolved or
/// unreadable gives `None`.
fn parent_membership(
    store: &Store,
    heads: &BTreeMap<Key, Vec<&Operation>>,
    machine: &str,
    link: &Value,
) -> Option<String> {
    let id = link["session_id"].as_str().unwrap_or_default();
    if let Some(primary) = store.id_memberships.as_ref().filter(|_| !id.is_empty()) {
        match primary.resolve(&store.catalog, &store.node_id, id) {
            Ok(Some(record)) if record.conflict => return None,
            Ok(Some(record)) => return record.value.as_str().map(str::to_owned),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    let key = Key::Session {
        machine: machine.into(),
        session: link["name"].as_str()?.into(),
    };
    let [only] = heads.get(&key)?.as_slice() else {
        return None;
    };
    only.value.as_str().map(str::to_owned)
}
/// Whether a fork's project attempt may run again: a busy lock or catalog,
/// with nothing held now. Done, a changed binding or a failure ends it.
fn retry(attempt: &anyhow::Result<Pinned<anyhow::Result<bool>>>) -> bool {
    matches!(attempt, Ok(Pinned::Busy | Pinned::Written(Ok(false))))
}
/// The fork's membership, or `None` to write nothing. A source without a
/// membership (or explicitly unassigned) leaves the fork without one, so both
/// follow the same default project. Inbox, a project that no longer exists and
/// an unresolved concurrent choice all become an explicit Inbox membership.
fn inherited(store: &Store, name: &str, source: &str, archive: Option<&str>) -> Option<String> {
    if !store.imported {
        return None;
    }
    let machine = store.catalog.machine(&store.node_id);
    let key = Key::Session {
        machine: machine.clone(),
        session: name.into(),
    };
    if store.catalog.heads().contains_key(&key) {
        return None;
    }
    let from = Key::Session {
        machine,
        session: match archive.filter(|id| !id.is_empty()) {
            Some(id) => format!("archive\n{id}"),
            None => source.into(),
        },
    };
    let heads = store.catalog.effective_heads();
    let [only] = heads.get(&from)?.as_slice() else {
        return Some(INBOX.into());
    };
    let project = only.value.as_str()?;
    let alive = |id: &str| {
        let key = Key::project(id, "alive");
        heads
            .get(&key)
            .and_then(|versions| Catalog::selected(&key, versions))
            .is_some_and(|op| op.value == true)
    };
    Some(if alive(project) { project } else { INBOX }.into())
}

/// An additive hint for `dirs`: optional read of an existing catalog, never
/// enrollment, initialization or membership mutation. Older peers omit it.
pub(crate) fn directory_project(config: &Config, directory: &Path) -> Option<Value> {
    if !config.dir.join("swarm/catalog.json").is_file() {
        return None;
    }
    let end = Instant::now() + Duration::from_secs(3);
    let store = LockedStore::try_load_scoped(config).ok()?;
    let project = infer_directory(&store.data, directory, end, |path| {
        crate::state::cached_worktree_catalog(path).unwrap_or(Value::Null)
    })?;
    Some(json!({"schema":1,"swarm_id":store.data.catalog.swarm_id,"project_id":project}))
}
fn infer(
    store: &Store,
    name: &str,
    directory: &Path,
    end: Instant,
    catalog: impl FnOnce(&Path) -> Value,
) -> Option<String> {
    let key = Key::Session {
        machine: store.catalog.machine(&store.node_id),
        session: name.into(),
    };
    if store.catalog.heads().contains_key(&key) {
        return None;
    }
    infer_directory(store, directory, end, catalog)
}
fn infer_directory(
    store: &Store,
    directory: &Path,
    end: Instant,
    catalog: impl FnOnce(&Path) -> Value,
) -> Option<String> {
    if !store.imported || Instant::now() >= end {
        return None;
    }
    let local = store.catalog.machine(&store.node_id);
    let directory = crate::platform::canonical(directory)
        .ok()
        .filter(|path| path.is_dir())?;
    let values = store.catalog.values();
    let mut anchors = Vec::new();
    for (key, value) in &values {
        if !matches!(key, Key::Folder { .. })
            || value.is_null()
            || value["machine"]
                .as_str()
                .is_none_or(|id| store.catalog.machine(id) != local)
        {
            continue;
        }
        let project = value["project"].as_str()?;
        if project == "ungrouped"
            || values.get(&Key::project(project, "alive")) != Some(&json!(true))
        {
            continue;
        }
        // A bounded complete candidate set; never decide from a truncated scan.
        if anchors.len() >= 512 || Instant::now() >= end {
            return None;
        }
        let path = crate::platform::canonical(value["path"].as_str()?)
            .ok()
            .filter(|path| path.is_dir())?;
        anchors.push((project.to_owned(), path));
    }
    if anchors.is_empty() || Instant::now() >= end {
        return None;
    }
    let mut projects: BTreeSet<String> = anchors
        .iter()
        .filter(|(_, path)| *path == directory)
        .map(|(id, _)| id.clone())
        .collect();
    let evidence = catalog(&directory);
    if Instant::now() >= end {
        return None;
    }
    if evidence["state"] == "ok"
        && evidence["stale"] == false
        && evidence["partial"] == false
        && evidence["path"].as_str() == directory.to_str()
        && evidence["common_dir"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_absolute())
    {
        let entries = evidence["worktrees"].as_array()?;
        let contains = |path: &Path| {
            entries.iter().any(|tree| {
                tree["available"] == true
                    && matches!(tree["kind"].as_str(), Some("main" | "linked"))
                    && tree["path"].as_str() == path.to_str()
            })
        };
        if contains(&directory) {
            for (id, path) in anchors {
                if contains(&path) {
                    projects.insert(id);
                }
            }
        }
    }
    // Exact canonical folder membership stands independently of Git. Missing
    // or stale repository evidence never establishes a related worktree.
    if projects.len() == 1 {
        projects.pop_first()
    } else {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::swarm::catalog::{new_id, Catalog};
    use std::collections::BTreeMap;
    /// zerus-jx5o: a busy provider lock, state lock or catalog retries
    /// within the cap; a write, a changed binding or a failure ends it.
    #[test]
    fn a_busy_attempt_retries_and_every_other_outcome_ends() {
        assert!(retry(&Ok(Pinned::Busy)));
        assert!(retry(&Ok(Pinned::Written(Ok(false)))));
        assert!(!retry(&Ok(Pinned::Written(Ok(true)))));
        assert!(!retry(&Ok(Pinned::Changed)));
        assert!(!retry(&Ok(Pinned::Written(Err(anyhow::anyhow!("save failed"))))));
        assert!(!retry(&Err(anyhow::anyhow!("lock failed"))));
    }
    fn store() -> Store {
        Store {
            node_id: new_id(),
            catalog: Catalog::default(),
            imported: true,
            bindings: BTreeMap::new(),
            peers: BTreeMap::new(),
            twins: Default::default(),
            id_memberships: None,
        }
    }
    fn folder(store: &mut Store, id: &str, project: &str, machine: &str, path: &Path) {
        store
            .catalog
            .set(&store.node_id, Key::project(project, "alive"), json!(true))
            .unwrap();
        store
            .catalog
            .set(
                &store.node_id,
                Key::Folder { id: id.into() },
                json!({"project":project,"machine":machine,"path":path,"name":""}),
            )
            .unwrap();
    }
    fn choose(store: &Store, path: &Path, evidence: Value) -> Option<String> {
        infer(
            store,
            "codex/example/new",
            path,
            Instant::now() + Duration::from_secs(3),
            |_| evidence,
        )
    }
    /// zerus-zjsj: a fork takes its source's live project, else an explicit
    /// Inbox; an unplaced source leaves it unplaced. Nothing fails the fork.
    #[test]
    fn a_fork_inherits_its_sources_project_or_inbox() {
        let mut store = store();
        let local = store.node_id.clone();
        let machine = store.catalog.machine(&local);
        let member = |session: &str| Key::Session {
            machine: machine.clone(),
            session: session.into(),
        };
        let set = |store: &mut Store, key: Key, value: Value| {
            store.catalog.set(&local, key, value).unwrap();
        };
        for project in ["work", "other"] {
            set(&mut store, Key::project(project, "alive"), json!(true));
        }
        set(&mut store, member("codex/p/source"), json!("work"));
        set(&mut store, member("archive\narchive-1"), json!("work"));
        set(&mut store, member("codex/p/inbox"), json!("ungrouped"));
        set(&mut store, member("codex/p/lost"), json!("never-created"));
        set(&mut store, member("codex/p/cleared"), Value::Null);
        // Two devices chose different projects; the desktop asks for a review.
        let mut remote = store.catalog.clone();
        remote
            .set(&new_id(), member("codex/p/split"), json!("other"))
            .unwrap();
        set(&mut store, member("codex/p/split"), json!("work"));
        store
            .catalog
            .merge(
                &remote.swarm_id.clone(),
                remote.operations.into_values().collect(),
            )
            .unwrap();
        let from = |store: &Store, source: &str, archive: Option<&str>| {
            inherited(store, "codex/p/fork", source, archive)
        };
        let work = Some("work".to_owned());
        let inbox = Some("ungrouped".to_owned());
        assert_eq!(from(&store, "codex/p/source", None), work);
        assert_eq!(from(&store, "codex/p/gone", Some("archive-1")), work);
        for source in ["codex/p/inbox", "codex/p/lost", "codex/p/split"] {
            assert_eq!(from(&store, source, None), inbox, "{source}");
        }
        // No membership or an explicit unassignment: the same default project.
        for source in ["codex/p/unplaced", "codex/p/cleared"] {
            assert_eq!(from(&store, source, None), None, "{source}");
        }
        assert_eq!(from(&store, "codex/p/gone", Some("archive-2")), None);
        // An explicit membership of the fork, even Inbox or a tombstone, stays.
        for value in [json!("ungrouped"), Value::Null, json!("other")] {
            let mut explicit = store.clone();
            set(&mut explicit, member("codex/p/fork"), value);
            assert_eq!(from(&explicit, "codex/p/source", None), None);
        }
        // A project deleted since: Inbox.
        let mut retired = store.clone();
        set(&mut retired, Key::project("work", "alive"), json!(false));
        assert_eq!(from(&retired, "codex/p/source", None), inbox);
        // An uninitialized catalog is never written.
        store.imported = false;
        assert_eq!(from(&store, "codex/p/source", None), None);
    }

    /// zerus-d5twt: a child its proven parent launched takes the parent's
    /// live project, else an explicit Inbox for a deleted one; any other
    /// parent leaves it to folder inference. An existing membership stays.
    #[test]
    fn a_child_takes_its_proven_parents_project_or_inbox() {
        let mut store = store();
        let local = store.node_id.clone();
        let machine = store.catalog.machine(&local);
        let member = |session: &str| Key::Session {
            machine: machine.clone(),
            session: session.into(),
        };
        let set = |store: &mut Store, key: Key, value: Value| {
            store.catalog.set(&local, key, value).unwrap();
        };
        for project in ["work", "other", "gone"] {
            set(&mut store, Key::project(project, "alive"), json!(true));
        }
        set(&mut store, member("claude/p/lead"), json!("work"));
        set(&mut store, member("claude/p/inbox"), json!("ungrouped"));
        set(&mut store, member("claude/p/cleared"), Value::Null);
        set(&mut store, member("claude/p/orphan"), json!("gone"));
        set(&mut store, member("claude/p/lost"), json!("never-created"));
        set(&mut store, Key::project("gone", "alive"), json!(false));
        let mut remote = store.catalog.clone();
        remote
            .set(&new_id(), member("claude/p/split"), json!("other"))
            .unwrap();
        set(&mut store, member("claude/p/split"), json!("work"));
        store
            .catalog
            .merge(
                &remote.swarm_id.clone(),
                remote.operations.into_values().collect(),
            )
            .unwrap();
        let link = |name: &str, source: &str| {
            json!({"schema":1,"machine":local,"host":"example","session_id":"",
                "name":name,"conversation_id":"conversation","run_id":"run","source":source,"at":1})
        };
        let child = |store: &Store, link: &Value| from_parent(store, "codex/p/child", link);
        let work = Parent::Project("work".into());
        let inbox = Parent::Project("ungrouped".into());
        assert_eq!(child(&store, &link("claude/p/lead", "launch")), work);
        for parent in ["claude/p/orphan", "claude/p/lost"] {
            assert_eq!(child(&store, &link(parent, "launch")), inbox, "{parent}");
        }
        // Inbox, no single membership or an unassigned parent: folder inference.
        for parent in [
            "claude/p/inbox",
            "claude/p/cleared",
            "claude/p/split",
            "claude/p/unplaced",
        ] {
            assert_eq!(
                child(&store, &link(parent, "launch")),
                Parent::Folder,
                "{parent}"
            );
        }
        // Only this computer's own process proof places a child.
        for source in ["claimed", "adopt", "manual", "forwarded"] {
            assert_eq!(
                child(&store, &link("claude/p/lead", source)),
                Parent::Folder,
                "{source}"
            );
        }
        let mut elsewhere = link("claude/p/lead", "launch");
        elsewhere["machine"] = json!(new_id());
        assert_eq!(child(&store, &elsewhere), Parent::Folder);
        let mut malformed = link("claude/p/lead", "launch");
        malformed.as_object_mut().unwrap().remove("schema");
        assert_eq!(child(&store, &malformed), Parent::Folder);
        // An explicit membership of the child, even Inbox or a tombstone, stays.
        for value in [json!("ungrouped"), Value::Null, json!("other")] {
            let mut explicit = store.clone();
            set(&mut explicit, member("codex/p/child"), value);
            assert_eq!(
                child(&explicit, &link("claude/p/lead", "launch")),
                Parent::Keep
            );
        }
        // A project deleted since the parent joined it: Inbox.
        let mut retired = store.clone();
        set(&mut retired, Key::project("work", "alive"), json!(false));
        assert_eq!(child(&retired, &link("claude/p/lead", "launch")), inbox);
        // The parent's ID membership is read first, as a project move reads it.
        let parent_id = "44444444-4444-4444-8444-444444444444";
        let mut by_id = store.clone();
        let mut primary = super::super::session_memberships::Memberships::empty(&by_id.catalog);
        primary
            .set(&by_id.catalog, &local, &local, parent_id, json!("other"))
            .unwrap();
        by_id.id_memberships = Some(primary);
        let mut renamed = link("claude/p/renamed", "launch");
        renamed["session_id"] = json!(parent_id);
        assert_eq!(child(&by_id, &renamed), Parent::Project("other".into()));
        let mut labelled = link("claude/p/lead", "launch");
        labelled["session_id"] = json!(parent_id);
        assert_eq!(child(&by_id, &labelled), Parent::Project("other".into()));
        // Without an ID record, the label's membership decides.
        labelled["session_id"] = json!("55555555-5555-4555-8555-555555555555");
        assert_eq!(child(&by_id, &labelled), work);
        // An unassigned ID record wins over its label too.
        let mut cleared = by_id.clone();
        cleared
            .id_memberships
            .as_mut()
            .unwrap()
            .set(&cleared.catalog, &local, &local, parent_id, Value::Null)
            .unwrap();
        labelled["session_id"] = json!(parent_id);
        assert_eq!(child(&cleared, &labelled), Parent::Folder);
        // An uninitialized catalog is never written.
        store.imported = false;
        assert_eq!(
            child(&store, &link("claude/p/lead", "launch")),
            Parent::Keep
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_exact_non_git_folder_is_initial_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let mut store = store();
        let local = store.node_id.clone();
        folder(&mut store, "folder", "work", &local, &path);
        assert_eq!(
            choose(&store, &path, json!({"state":"not_repo"})),
            Some("work".into())
        );
        assert!(!path.join(".git").exists());
        let link = path.join("alias");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            choose(&store, &link, json!({"state":"not_repo"})),
            Some("work".into())
        );
        let nested = path.join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(choose(&store, &nested, json!({"state":"not_repo"})), None); // no prefix guess
        for value in [Value::Null, json!("ungrouped"), json!("manual")] {
            let mut explicit = store.clone();
            explicit
                .catalog
                .set(
                    &local,
                    Key::Session {
                        machine: local.clone(),
                        session: "codex/example/new".into(),
                    },
                    value,
                )
                .unwrap();
            assert_eq!(choose(&explicit, &path, json!({"state":"not_repo"})), None);
        }
    }
    #[cfg(unix)]
    #[test]
    fn exact_catalog_worktrees_require_complete_same_machine_unambiguous_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let anchor = root.join("main");
        let linked = root.join("linked");
        std::fs::create_dir(&anchor).unwrap();
        std::fs::create_dir(&linked).unwrap();
        let evidence = json!({"state":"ok","stale":false,"partial":false,"path":linked,"common_dir":root.join("common"),
            "worktrees":[{"path":anchor,"kind":"main","available":true},{"path":linked,"kind":"linked","available":true}]});
        let mut store = store();
        let local = store.node_id.clone();
        folder(&mut store, "anchor", "work", &local, &anchor);
        assert_eq!(
            choose(&store, &linked, evidence.clone()),
            Some("work".into())
        );
        for field in ["stale", "partial"] {
            let mut changed = evidence.clone();
            changed[field] = json!(true);
            assert_eq!(choose(&store, &linked, changed), None);
        }
        for changed in [
            json!({"state":"timeout"}),
            json!({"state":"ok","stale":false,"partial":false,"path":linked,"common_dir":"relative","worktrees":[]}),
        ] {
            assert_eq!(choose(&store, &linked, changed), None);
        }
        for field in ["available", "kind"] {
            let mut changed = evidence.clone();
            changed["worktrees"][0][field] = if field == "available" {
                json!(false)
            } else {
                json!("bare")
            };
            assert_eq!(choose(&store, &linked, changed), None);
        }
        let mut remote = store.clone();
        let other = new_id();
        folder(&mut remote, "anchor", "work", &other, &anchor);
        assert_eq!(choose(&remote, &linked, evidence.clone()), None);
        folder(&mut store, "second", "other", &local, &linked);
        assert_eq!(choose(&store, &linked, evidence), None);
    }
    #[cfg(unix)]
    #[test]
    fn expired_or_uninitialized_inference_does_not_consult_git() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store();
        let never = |_: &Path| -> Value {
            panic!("Git must not run");
        };
        assert_eq!(
            infer(&store, "one", dir.path(), Instant::now(), never),
            None
        );
        store.imported = false;
        assert_eq!(
            infer(
                &store,
                "one",
                dir.path(),
                Instant::now() + Duration::from_secs(3),
                never
            ),
            None
        );
    }
}
