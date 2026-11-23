//! Task #20 of team c536f0e5 (P2.6c, design 4.6 and P2.6): the swarm worker
//! waits for a change instead of looking every 3 s.
//!
//! - The change bus (`state::changes`) watches the swarm folder of the
//!   configuration (the catalog and the session ID files) and the replica
//!   folder of the state (each project's changes, tasks and inventory). A
//!   burst wakes the worker once it has been quiet for [`QUIET`].
//! - A file in the swarm folder's `wake` folder names a member (its alias)
//!   whose catalog or tasks changed: the desktop touches it when the
//!   member's watch stream shows its `swarm` head moved, so the worker pulls
//!   from that member at once. The worker removes the files it read.
//! - Files the worker writes itself on every pass (locks, the task cutover
//!   cursor, the label cache, the worker records, SQLite indexes) never wake
//!   it.
//! - Without the bus (`HGS_CHANGE_BUS=0`, a folder the watcher refuses) the
//!   worker keeps its 3 s pass.
use crate::config::Config;
use crate::state::changes::{self, Bus, Note};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A burst of changes runs a round once nothing changed for this long.
pub(crate) const QUIET: Duration = Duration::from_millis(200);
/// The folder of wake files, in the configuration's swarm folder.
pub(crate) const WAKE: &str = "wake";
/// The most wake files one wake reads.
const WAKE_FILES: usize = 64;

/// What woke the worker.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Woke {
    /// The catalog or the tasks changed here (or anything may have).
    pub changed: bool,
    /// Members whose own catalog or tasks changed, by alias.
    pub peers: BTreeSet<String>,
}

enum Signal {
    Swarm(changes::Hint),
    Replica,
    Failed(String),
}

/// Which notified paths of the swarm folder count.
pub(crate) fn keep_swarm(relative: &Path) -> bool {
    let mut parts = relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned());
    let Some(first) = parts.next() else { return false };
    if first == WAKE {
        return true;
    }
    parts.next().is_none() && (first == "catalog.json" || (first.starts_with("session-id-") && first.ends_with(".json")))
}

/// Which notified paths of the replica folder count: a project's changes,
/// tasks, inventory or identity, never its indexes or locks.
///
/// A bare project folder never counts (task #26): on Windows every create or
/// delete of a file in it (the task index's `-wal` and `-shm`, opened on
/// every pass) also reports the folder itself, so the worker woke itself
/// about a hundred times a second. A new project shows through its
/// `project-id`, inventory, changes or tasks; a removed one through the
/// removal of those files, and the periodic pass catches anything else.
pub(crate) fn keep_replica(relative: &Path) -> bool {
    let parts: Vec<String> = relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    let Some(name) = parts.last() else { return false };
    if name.ends_with(".lock") || name.ends_with("-shm") || name.ends_with("-wal") || name.ends_with("-journal") || name.ends_with(".sqlite3") || name.starts_with('.') {
        return false;
    }
    match parts.get(1).map(String::as_str) {
        Some("changes" | "tasks") => true,
        Some("inventory.json" | "project-id") => parts.len() == 2,
        _ => false,
    }
}

/// The aliases named by wake files under `dir`; the files are removed.
pub(crate) fn take_wakes(dir: &Path) -> BTreeSet<String> {
    let mut peers = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return peers };
    for entry in entries.flatten().take(WAKE_FILES) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with(".tmp") {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
        // Only members are synced; a name that cannot be one is dropped.
        if name.len() <= 255 && !name.starts_with(['-', '@']) && !name.chars().any(|c| c.is_whitespace() || c.is_control()) {
            peers.insert(name);
        }
    }
    peers
}

/// The worker's change bus.
pub(crate) struct Wait {
    rx: mpsc::Receiver<Signal>,
    _buses: Vec<Bus>,
    wake_dir: PathBuf,
    /// Why the bus does not run, when it does not.
    pub(crate) fault: Option<String>,
}

impl Wait {
    pub(crate) fn start(config: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let swarm = config.dir.join("swarm");
        let wake_dir = swarm.join(WAKE);
        let mut wait = Self { rx, _buses: Vec::new(), wake_dir, fault: None };
        if !changes::enabled() {
            wait.fault = Some("HGS_CHANGE_BUS=0".into());
            return wait;
        }
        let _ = crate::platform::private_dir(&wait.wake_dir);
        let sink = tx.clone();
        let swarm_bus = Bus::start(&swarm, keep_swarm, move |note| {
            sink.send(match note {
                Note::Hint(hint) => Signal::Swarm(hint),
                Note::Failed(error) => Signal::Failed(error),
            })
            .is_ok()
        });
        let replica = config.state.join("replica");
        let _ = std::fs::create_dir_all(&replica);
        let replica_bus = Bus::start(&replica, keep_replica, move |note| {
            tx.send(match note {
                Note::Hint(_) => Signal::Replica,
                Note::Failed(error) => Signal::Failed(error),
            })
            .is_ok()
        });
        match (swarm_bus, replica_bus) {
            (Ok(swarm), Ok(replica)) => wait._buses = vec![swarm, replica],
            (Err(error), _) | (_, Err(error)) => wait.fault = Some(error),
        }
        wait
    }

    /// Whether changes wake the worker; otherwise it keeps its 3 s pass.
    pub(crate) fn listening(&self) -> bool {
        self.fault.is_none()
    }

    /// Waits until `until` or the first change, then until the burst has
    /// been quiet for [`QUIET`].
    pub(crate) fn wait(&mut self, until: Instant) -> Woke {
        let mut woke = Woke::default();
        let first = match until.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => self.rx.recv_timeout(left).ok(),
            _ => self.rx.try_recv().ok(),
        };
        let Some(first) = first else { return woke };
        let mut wakes = false;
        self.note(first, &mut woke, &mut wakes);
        while let Ok(signal) = self.rx.recv_timeout(QUIET) {
            self.note(signal, &mut woke, &mut wakes);
        }
        if wakes {
            woke.peers = take_wakes(&self.wake_dir);
        }
        woke
    }

    fn note(&mut self, signal: Signal, woke: &mut Woke, wakes: &mut bool) {
        match signal {
            Signal::Swarm(hint) => {
                if hint.overflow {
                    woke.changed = true;
                    *wakes = true;
                    return;
                }
                for path in &hint.paths {
                    if path.components().next().is_some_and(|first| first.as_os_str() == WAKE) {
                        *wakes = true;
                    } else {
                        woke.changed = true;
                    }
                }
            }
            Signal::Replica => woke.changed = true,
            Signal::Failed(error) => {
                woke.changed = true;
                *wakes = true;
                self._buses.clear();
                self.fault = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_catalog_the_tasks_and_wake_files_wake_the_worker() {
        for path in ["catalog.json", "session-id-memberships.json", "wake/msi-old", "wake"] {
            assert!(keep_swarm(Path::new(path)), "{path}");
        }
        for path in ["catalog.lock", "worker.lock", "worker.json", "task-cutover-cursor", "session-labels-cache/attempt", "membership.json", "backup-before-join-x.json", "sub/catalog.json"] {
            assert!(!keep_swarm(Path::new(path)), "{path}");
        }
        for path in ["p1/changes/0001.loro", "p1/changes", "p1/tasks/t.json", "p1/inventory.json", "p1/project-id"] {
            assert!(keep_replica(Path::new(path)), "{path}");
        }
        for path in ["store-id", "store-id.lock", "p1/task-index-v3.sqlite3", "p1/task-index-v3.sqlite3-wal", "p1/project.lock", "p1/inventory.repaired", "p1/changes/.tmp1"] {
            assert!(!keep_replica(Path::new(path)), "{path}");
        }
    }

    /// Task #26: on Windows a file created or removed in a project folder
    /// also reports the folder itself, so a bare project folder never wakes
    /// the worker, and neither does any file of the task index.
    #[test]
    fn a_bare_project_folder_and_the_task_index_never_wake_the_worker() {
        let project = "0894e8ae31bcda453e537913f4a8557d625836a9c87a316fe04f12093d26bd46";
        for path in [project.to_owned(), "p2".to_owned(), String::new()] {
            assert!(!keep_replica(Path::new(&path)), "{path:?}");
        }
        for index in ["task-index-v3.sqlite3", "task-index-v3.sqlite3-wal", "task-index-v3.sqlite3-shm", "task-index-v3.sqlite3-journal", "task-index-v2.sqlite3", ".lock"] {
            let path = format!("{project}/{index}");
            assert!(!keep_replica(Path::new(&path)), "{path}");
        }
        // A project still shows through its own files.
        assert!(keep_replica(&Path::new(project).join("project-id")));
        assert!(keep_replica(&Path::new(project).join("tasks").join("t.loro")));
    }

    #[cfg(windows)]
    fn config(root: &Path) -> crate::config::Config {
        crate::config::Config {
            home: root.to_string_lossy().into_owned(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec!["example-local".into()],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        }
    }

    /// Task #26's regression: an idle pass reads every stored project's
    /// tasks through its SQLite index, whose `-wal` and `-shm` are created
    /// and removed each time. On a real bus that wakes nothing.
    #[cfg(windows)]
    #[test]
    fn an_idle_pass_over_a_project_index_wakes_nothing() {
        use zerus_replica::{Actor, NewTask, ProjectStore, Stage, Target, TaskDoc};
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        std::fs::create_dir_all(config.dir.join("swarm")).unwrap();
        let project = "example";
        let store = ProjectStore::open(&crate::tasks::replica_root(&config).unwrap(), project).unwrap();
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
        drop(store);
        let fingerprint = crate::swarm::replica::fingerprint(&config).unwrap();
        let mut wait = Wait::start(&config);
        assert!(wait.listening(), "{:?}", wait.fault);
        // The worker's pass: the local digest and a task cutover step, which
        // opens the project's task index and rewrites the cursor.
        let mut cutover = crate::swarm::replica::CutoverScan::default();
        for _ in 0..2 {
            assert_eq!(crate::swarm::replica::fingerprint(&config).unwrap(), fingerprint);
            assert!(!cutover.advance(&config).unwrap(), "no task to cut over");
        }
        // Project folders are named by a hash of the project.
        let folders = std::fs::read_dir(config.state.join("replica")).unwrap().flatten().filter(|entry| entry.path().is_dir());
        let index = folders.flat_map(|folder| std::fs::read_dir(folder.path()).unwrap().flatten()).any(|entry| entry.file_name().to_string_lossy().starts_with("task-index-"));
        assert!(index, "the pass read the tasks through the index");
        assert_eq!(wait.wait(Instant::now() + Duration::from_millis(600)), Woke::default());
        // A task saved by someone else still wakes it.
        let store = ProjectStore::open(&crate::tasks::replica_root(&config).unwrap(), project).unwrap();
        let mut task = store.load(task.uuid()).unwrap();
        task.comment("Changed", &Actor::Person, 2).unwrap();
        store.save(&mut task).unwrap();
        assert!(wait.wait(Instant::now() + Duration::from_secs(5)).changed);
    }

    #[test]
    fn wake_files_name_members_and_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["msi-old", "mac", "bad alias", ".hidden", "x.tmp"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let peers = take_wakes(dir.path());
        assert_eq!(peers, BTreeSet::from(["mac".to_owned(), "msi-old".to_owned()]));
        let left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        assert!(!left.contains(&"msi-old".to_owned()) && left.contains(&".hidden".to_owned()), "{left:?}");
        assert!(take_wakes(&dir.path().join("missing")).is_empty());
    }

    /// The acceptance's first half, measured here: a catalog write wakes the
    /// worker once the burst is quiet, well within the 2 s budget.
    #[cfg(windows)]
    #[test]
    fn a_catalog_write_wakes_the_worker_after_the_quiet_time() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        std::fs::create_dir_all(config.dir.join("swarm")).unwrap();
        let mut wait = Wait::start(&config);
        assert!(wait.listening(), "{:?}", wait.fault);
        // Its own files: nothing.
        std::fs::write(config.dir.join("swarm/task-cutover-cursor"), b"p").unwrap();
        assert_eq!(wait.wait(Instant::now() + Duration::from_millis(400)), Woke::default());
        let written = Instant::now();
        std::fs::write(config.dir.join("swarm/catalog.json"), b"{}").unwrap();
        let woke = wait.wait(Instant::now() + Duration::from_secs(5));
        let took = written.elapsed();
        assert!(woke.changed && woke.peers.is_empty(), "{woke:?}");
        assert!(took >= QUIET && took < Duration::from_secs(1), "{took:?}");
        // A wake file names its member and is removed.
        std::fs::write(config.dir.join("swarm/wake/msi-old"), b"").unwrap();
        let woke = wait.wait(Instant::now() + Duration::from_secs(5));
        assert_eq!(woke.peers, BTreeSet::from(["msi-old".to_owned()]));
        assert!(!config.dir.join("swarm/wake/msi-old").exists());
        // A replica change.
        std::fs::create_dir_all(config.state.join("replica/p1/changes")).unwrap();
        let woke = wait.wait(Instant::now() + Duration::from_secs(5));
        assert!(woke.changed);
    }
}
