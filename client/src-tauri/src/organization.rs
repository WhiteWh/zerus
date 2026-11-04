//! The shared project organization on the Tauri side (zerus-5wmh.31, .18).
//! This computer's `organization` topic feeds the core's write controller
//! (`swarm::sync::Controller`). The presented organization goes to the pump's
//! grouping, which sends the webview each row's project and the headers.
//! Edits run as one `hgs swarm apply` at a time on this module's thread,
//! which also saves the local layout. A failed apply stays pending in the
//! controller, and this thread sends it again on the next tick: 5 s later, or
//! when the topic's next state arrives (SwarmController.cpp:67-73, 96-101).
//! Pending edits are kept in a draft file, written before their apply and
//! removed once they settled; a restart loads it and sends it after the first
//! state, as Qt does (SessionsWindow.cpp:2496, 1661; SwarmController.cpp:76-89).
//! A draft file that cannot be used is never applied: it is moved aside
//! before any new draft is written, and the list shows why until the first
//! state. [`Organization::flush`] checkpoints the files as the app quits. The
//! webview never receives the organization itself.
//!
//! Locks: the writer's, then the hub's, then the pump's (a publish hands the
//! pump the presented organization). The writer orders the draft and layout
//! writes of the worker and `flush`. The snapshot source, `hgs`, the files,
//! the edits' answers and the Projects callback all run without the hub's
//! lock, `hgs` also without the writer's, and the worker is woken after the
//! hub's lock is released. The Projects callback runs under the `announced`
//! lock, one call at a time. A pass that applies or settles edits drops the
//! writer's lock before its call; the calls made where `save_draft` releases
//! the hub's lock, in an idle pass and in `flush` still run under it. So the
//! callback must never block on the main thread, which takes the writer's
//! lock in `flush` as the app quits, and must never take the writer's lock.
//! Run-token I/O has a dedicated worker and RunsWriter lock. Copy the worker's
//! handle under Shared.writer and release that lock before requesting a pass;
//! only the token thread takes RunsWriter. Its coalescing mailbox never holds
//! the I/O lock. Startup reads and all token checkpoints run off main. The exit
//! path waits at most 500 ms before independently flushing draft and layout.

pub mod clones;
pub mod folders;
pub mod overview;
pub mod swarm_dialog;

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read as _};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tauri::State;
use zerus_client_core::reads::{Read, ReadError};
use zerus_client_core::swarm::draft::{self, Draft, DraftWrite, Load};
use zerus_client_core::swarm::edit::Intent;
use zerus_client_core::swarm::grouping::{by_name, SyncView};
use zerus_client_core::swarm::runs;
use zerus_client_core::swarm::sync::{Controller, Due, EditOutcome, Observation, Ticket, STOPPED};
use zerus_client_core::swarm::{self, Folder};
use zerus_client_core::{run_limited, Change, Exit, OrganizationSnapshot, OrganizationUpdate};

use crate::actions::REPLAY_NEEDS_HGS;
use crate::pump::Pump;

/// The layout is saved at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// The longest the worker waits, in milliseconds, when nothing is due sooner.
const IDLE_MS: u64 = 1_000;
/// An explicit exit waits at most this long for the token checkpoint.
const RUNS_EXIT_WAIT: Duration = Duration::from_millis(500);
/// A failing write logs at most one line in this long.
const LOG_EVERY: Duration = Duration::from_secs(60);

/// The base and versions of the latest state: `Fleet::organization_snapshot`
/// live, the replay's own catalog in a replay.
pub type SnapshotSource = Arc<dyn Fn() -> Option<OrganizationSnapshot> + Send + Sync>;
/// Hands the presented `swarm get` object (none before the first state),
/// the default project and the controller's status line to the Projects
/// screen, as Qt's `statusChanged` reaches its Projects page
/// (SessionsWindow.cpp:1653).
/// It runs without the hub's lock, one call at a time, and a call never
/// follows a newer one; it must not call back into [`Organization`]. A
/// panic in it is caught and logged.
///
/// It must never block on the main thread and never take the writer's lock:
/// some calls run under that lock (see the module's lock list), and some on
/// the main thread itself, in [`Organization::flush`] as the app quits. A
/// call that waited for the main thread there would deadlock the quit.
pub type Presented = Arc<dyn Fn(Option<Arc<Value>>, String, (String, bool)) + Send + Sync>;

/// One call of [`Presented`], made once the hub's lock is released.
type Announce = (Presented, Option<Arc<Value>>, String, (String, bool));

/// The layout file in the app data directory; a replay keeps its own.
pub fn layout_file(replay: bool) -> &'static str {
    if replay {
        "organization-layout-replay.json"
    } else {
        "organization-layout.json"
    }
}

/// The pending draft beside the layout; a replay keeps its own.
pub fn draft_file(replay: bool) -> &'static str {
    if replay {
        draft::REPLAY_FILE
    } else {
        draft::FILE
    }
}

/// The switch seeds run_groups from the same read-only store as Settings.
/// Replays and development identities never read Qt for organization data.
pub fn run_seed_source(
    identifier: &str,
    replay: bool,
) -> Option<zerus_client_core::settings::QtStore> {
    crate::settings::seed_source(identifier, replay)
}

#[derive(Default)]
struct Hub {
    sync: Controller,
    source: Option<SnapshotSource>,
    waiting: HashMap<Ticket, SyncSender<EditOutcome>>,
    presented: Option<Presented>,
    /// The revision and status line Projects last received.
    shown: Option<(u64, String, bool, Option<String>)>,
    /// Settled edits' answers, sent once the lock is released.
    answers: Vec<(SyncSender<EditOutcome>, EditOutcome)>,
    /// The Projects call, numbered, made once the lock is released.
    announce: Option<(u64, Announce)>,
    announcements: u64,
    /// Why the saved draft was not applied, shown until the first state is adopted.
    notice: Option<String>,
    /// The worker is gone: the list says so, and edits answer `Kept(STOPPED)`.
    stopped: bool,
    /// Renames included in the next pending-draft checkpoint. Never acknowledge
    /// tokens before that checkpoint is durable, even if sync.edit succeeded.
    run_acks: VecDeque<(String, String)>,
    qt_recovery: Option<QtRecovery>,
    recovery_seen: Option<(String, bool)>,
    recovery_due: Option<String>,
    presentation_due: bool,
    catalog: Option<swarm_dialog::Context>,
    source_generation: u64,
    /// Only an adopted current source observation can qualify local metadata.
    /// Kept out of Controller: it is never shared state or apply input.
    windows_read: Option<(u64, u64, String)>,
    /// One startup attempt once both the local watch and catalog are current.
    /// Failed/absent sources stay unhandled on disk for the next startup.
    windows_import_attempted: bool,
}

#[derive(Clone)]
struct QtRecovery {
    source: zerus_client_core::settings::QtStore,
    settings: zerus_client_core::settings::Settings,
}

impl Hub {
    fn acknowledge_after_draft(&mut self, from: String, to: String) {
        // A failed draft can outlive arbitrarily many observations. Dropping
        // an old acknowledgement is conservative: its saved token stays old.
        if self.run_acks.len() >= runs::MAX_RUNS {
            self.run_acks.pop_front();
        }
        self.run_acks.push_back((from, to));
    }
}

struct Shared {
    hub: Mutex<Hub>,
    wake: Condvar,
    /// The last Projects call made, so a late one never overtakes a newer one.
    announced: Mutex<u64>,
    /// The draft and layout writes of the worker and of `flush`.
    writer: Mutex<Writer>,
    pump: Pump,
    hgs: PathBuf,
    allowed: bool,
    layout: PathBuf,
    /// Unsupported/unreadable originals are retained until a fresh startup.
    layout_writable: bool,
    draft: PathBuf,
    clock: Instant,
    /// One Qt seed read at a time; failed reads leave the persisted mark unset.
    seed_started: std::sync::atomic::AtomicBool,
    /// Passes the worker started, so a test can tell when it acted.
    #[cfg(test)]
    passes: std::sync::atomic::AtomicUsize,
    /// Passes the worker finished with nothing due, so a test can tell it ran.
    #[cfg(test)]
    parked: std::sync::atomic::AtomicUsize,
    /// Makes the worker's next pass panic, so a test can stop it.
    #[cfg(test)]
    fail: std::sync::atomic::AtomicBool,
}

/// The managed state of the organization commands; clones share it. The
/// worker thread runs while a clone is left.
#[derive(Clone)]
pub struct Organization(Arc<Shared>);

impl Organization {
    /// Only the production switch supplies a source. Recovery is queued by
    /// the existing local organization topic and runs on this hub's worker.
    pub fn recover_qt(
        &self,
        source: Option<zerus_client_core::settings::QtStore>,
        settings: zerus_client_core::settings::Settings,
    ) {
        if !self.0.allowed {
            return;
        }
        let Some(source) = source else {
            return;
        };
        let mut hub = self.lock();
        hub.qt_recovery = Some(QtRecovery { source, settings });
        hub.presentation_due = true;
        if let Some(value) = hub.sync.observed_value() {
            Self::queue_recovery(&mut hub, &value);
        }
        drop(hub);
        self.0.wake.notify_all();
    }

    fn queue_recovery(hub: &mut Hub, value: &Value) {
        if hub.qt_recovery.is_none() || hub.stopped {
            return;
        }
        let (Some(node), Some(initialized)) =
            (value["node_id"].as_str(), value["initialized"].as_bool())
        else {
            return;
        };
        let identity = (node.to_owned(), initialized);
        if hub.recovery_seen.as_ref() != Some(&identity) {
            hub.recovery_seen = Some(identity);
            hub.recovery_due = Some(node.to_owned());
        }
    }

    fn recover_presentation(&self, recovery: &QtRecovery) -> Result<(), String> {
        if !self.0.layout_writable || self.0.pump.presentation_seeded()? {
            return Ok(());
        }
        // The shared record builder uses the old switch time only to record
        // a later Qt mtime. That observation never blocks this new family.
        let seed_at = recovery
            .settings
            .seed_record()
            .and_then(|record| record["at"].as_u64());
        let seed = swarm::layout::qt_presentation(&recovery.source, &self.0.layout, seed_at)?;
        let mut writer = self.writer();
        self.0.pump.seed_presentation(seed)?;
        writer.save_layout(&self.0.pump, &self.0.layout, self.0.layout_writable);
        drop(writer);
        let mut hub = self.lock();
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
        Ok(())
    }

    fn recover_windows_folders(&self) -> Result<(), String> {
        let (node, generation, revision, catalog, source, recovery) = {
            let mut hub = self.lock();
            if hub.windows_import_attempted {
                return Ok(());
            }
            let Some(node) = self.windows_read_node(&hub) else {
                return Ok(());
            };
            let (Some(catalog), Some(source), Some(recovery)) = (
                hub.sync.observed_value(),
                hub.source.clone(),
                hub.qt_recovery.clone(),
            ) else {
                return Ok(());
            };
            hub.windows_import_attempted = true;
            (
                node,
                hub.source_generation,
                hub.sync.revision(),
                catalog,
                source,
                recovery,
            )
        };
        let Some(layout) = self.0.pump.windows_folders_import_layout()? else {
            return Ok(());
        };
        // The fixed local Qt source is supplied only by the switch identity.
        // Its explicit folder machine IDs are checked against the independent
        // current local watch and hgs catalog, never aliases or path spellings.
        // No organization key means no call to the nonempty-source importer,
        // and no durable empty/handled family is manufactured.
        if recovery
            .source
            .snapshot()?
            .is_none_or(|snapshot| !snapshot.values.contains_key(swarm::qt_import::KEY))
        {
            return Ok(());
        }
        let context = swarm::layout::WindowsFoldersContext {
            source_node: &node,
            current_node: &node,
            catalog: &catalog,
        };
        let seed_at = recovery
            .settings
            .seed_record()
            .and_then(|record| record["at"].as_u64());
        let Some(seed) = swarm::layout::qt_windows_folders(
            &layout,
            &recovery.source,
            &self.0.layout,
            &context,
            seed_at,
        )?
        else {
            return Ok(());
        };
        // Source callbacks run outside our locks. A source change during the
        // Qt read invalidates the attempt; commit rechecks the current catalog
        // and live project IDs under the same writer as every Layout save.
        let Some(snapshot) = source() else {
            return Ok(());
        };
        let mut writer = self.writer();
        let mut hub = self.lock();
        if generation != hub.source_generation
            || revision != hub.sync.revision()
            || self.windows_read_node(&hub).as_deref() != Some(&node)
            || snapshot.node_id != node
            || snapshot.digest != catalog["digest"].as_str().unwrap_or_default()
            || snapshot.swarm_id != catalog["swarm_id"].as_str().unwrap_or_default()
            || snapshot.base != catalog["organization"]
        {
            return Ok(());
        }
        let Some(current) = hub.sync.observed_value() else {
            return Ok(());
        };
        let context = swarm::layout::WindowsFoldersContext {
            source_node: &node,
            current_node: &node,
            catalog: &current,
        };
        self.0.pump.seed_windows_folders(seed, &context)?;
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
        writer.save_layout(&self.0.pump, &self.0.layout, self.0.layout_writable);
        Ok(())
    }
    pub fn new(
        pump: Pump,
        hgs: PathBuf,
        allowed: bool,
        layout: PathBuf,
        draft_path: PathBuf,
        replay: bool,
    ) -> Self {
        // An absent layout may be created. An unreadable/unsupported original
        // is preserved: neither the Qt seed nor a later UI save replaces it.
        let loaded = match zerus_client_core::swarm::layout::read(&layout) {
            Ok(Some(bytes)) => pump.load_layout(&bytes),
            Ok(None) => Ok(()),
            Err(_) => Err("the project layout could not be read"),
        };
        let layout_writable = loaded.is_ok();
        if let Err(error) = loaded {
            eprintln!(
                "zerus-client: {error}; the original is retained and layout saves are paused"
            );
        }
        // A blocked replay sends nothing, so it takes up no draft either.
        let saved = if allowed {
            saved_draft(&draft_path)
        } else {
            Saved::default()
        };
        let organization = Self(Arc::new(Shared {
            hub: Mutex::default(),
            wake: Condvar::new(),
            announced: Mutex::new(0),
            writer: Mutex::new(Writer::new(
                saved.aside,
                pump.clone(),
                layout.with_file_name(if replay {
                    runs::REPLAY_FILE
                } else {
                    runs::FILE
                }),
            )),
            pump,
            hgs,
            allowed,
            layout,
            layout_writable,
            draft: draft_path,
            clock: Instant::now(),
            seed_started: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            passes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            parked: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            fail: std::sync::atomic::AtomicBool::new(false),
        }));
        {
            let mut hub = organization.lock();
            // The last run's pending edits (Qt's start, SwarmController.cpp:76-89):
            // shown at once, sent after the first state.
            if let Some(draft) = saved.draft {
                hub.sync.restore(draft);
            }
            hub.notice = saved.notice;
            organization.publish(&mut hub);
            organization.release(hub);
        }
        organization.spawn();
        organization
    }

    pub fn seed_runs(&self, source: Option<zerus_client_core::settings::QtStore>) {
        if let Some(source) = source {
            let _ = self
                .seed_runs_with(move || zerus_client_core::swarm::layout::qt_run_groups(&source));
        }
    }

    fn seed_runs_with(
        &self,
        read: impl FnOnce() -> Result<serde_json::Map<String, Value>, String> + Send + 'static,
    ) -> Option<std::thread::JoinHandle<()>> {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.0.layout_writable
            || self.0.pump.runs_seeded()
            || self.0.seed_started.swap(true, SeqCst)
        {
            return None;
        }
        let shared = Arc::downgrade(&self.0);
        let started = std::thread::Builder::new()
            .name("zerus-run-seed".into())
            .spawn(move || {
                // Neither the pump nor the hub, writer or a strong organization
                // reference is held across Qt's file/cfprefsd I/O and JSON parsing.
                let groups = read();
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                match groups {
                    Ok(groups) => {
                        shared.pump.seed_runs(&groups);
                        shared.wake.notify_all();
                    }
                    Err(error) => {
                        shared.seed_started.store(false, SeqCst);
                        eprintln!("zerus-client: the archive project seed was not loaded: {error}");
                    }
                }
            });
        match started {
            Ok(worker) => Some(worker),
            Err(error) => {
                self.0.seed_started.store(false, SeqCst);
                eprintln!("zerus-client: the archive project seed worker did not start: {error}");
                None
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Hub> {
        self.0.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn writer(&self) -> MutexGuard<'_, Writer> {
        self.0.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Copy the handle under Shared.writer, then release it before taking
    /// RunsWriter. No code holds both locks, including the exit path.
    fn runs_writer(&self) -> Arc<RunsWorker> {
        Arc::clone(&self.writer().runs)
    }

    fn now(&self) -> u64 {
        u64::try_from(self.0.clock.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Repeated start_source calls keep the same Live source (webview reload).
    /// Do not invalidate an accepted observation when that source did not change.
    pub fn set_initial_source(&self, source: SnapshotSource) {
        self.replace_source(source, true);
    }

    #[cfg(all(test, unix))]
    pub fn set_source(&self, source: SnapshotSource) {
        self.replace_source(source, false);
    }

    fn replace_source(&self, source: SnapshotSource, only_absent: bool) {
        let mut hub = self.lock();
        if only_absent && hub.source.is_some() {
            return;
        }
        if let Some(generation) = hub.source_generation.checked_add(1) {
            hub.source_generation = generation;
        } else {
            hub.stopped = true;
        }
        hub.windows_read = None;
        hub.source = Some(source);
        self.publish(&mut hub);
        self.release(hub);
    }

    /// The last `swarm get` observed (its `worker`, `peers` and
    /// `preferences` included), as Qt's SwarmController hands its snapshot
    /// to Settings; None before the first. Shared as observed: pending edits
    /// are not applied to it, so nothing is rebuilt under the hub's lock.
    pub fn swarm_snapshot(&self) -> Option<Arc<Value>> {
        self.lock().sync.observed_value()
    }

    pub fn set_presented(&self, presented: Presented) {
        let mut hub = self.lock();
        hub.presented = Some(presented);
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
    }

    /// This computer's organization changes, from the drain thread.
    pub fn observe(&self, change: &Change) {
        // This callback precedes Pump::push. Drop the proof before a new hello
        // or a disconnect can leave the previous card cached in the pump.
        let identity_changed = match change {
            Change::Machine { computer, machine } if computer.is_local() => {
                self.0.pump.machine(computer).is_none_or(|old| {
                    old.machine_id != machine.machine_id
                        || old.platform != machine.platform
                        || old.ok != machine.ok
                        || old.local_sessions != machine.local_sessions
                })
            }
            _ => false,
        };
        if identity_changed
            || matches!(change, Change::Computer { computer, status } if computer.is_local() && *status != zerus_client_core::ComputerStatus::Live)
        {
            let mut hub = self.lock();
            hub.windows_read = None;
            self.publish(&mut hub);
            self.release(hub);
            return;
        }
        let Change::Organization { computer, update } = change else {
            return;
        };
        if !computer.is_local() {
            return;
        }
        let (generation, source) = {
            let hub = self.lock();
            (hub.source_generation, hub.source.clone())
        };
        let mut hub = match update {
            OrganizationUpdate::State {
                value,
                digest,
                membership,
            } => {
                // Asked without the hub's lock: the source takes its own.
                let Some(snapshot) = source.and_then(|source| source()) else {
                    self.invalidate_windows_read(generation);
                    return;
                };
                // Base and versions of one digest; a newer state is on its way otherwise.
                if snapshot.digest != *digest
                    || value["node_id"].as_str().unwrap_or_default() != snapshot.node_id
                    || value["swarm_id"].as_str().unwrap_or_default() != snapshot.swarm_id
                    || value["organization"] != snapshot.base
                {
                    self.invalidate_windows_read(generation);
                    return;
                }
                let mut hub = self.lock();
                if generation != hub.source_generation {
                    return;
                }
                let now = self.now();
                hub.windows_read = None;
                let adopted = if !self.0.hub.is_poisoned()
                    && swarm_dialog::settle(&mut hub, &snapshot, now)
                {
                    hub.shown = None;
                    true
                } else {
                    let observation = Observation {
                        digest: digest.clone(),
                        value: Arc::clone(value),
                        versions: snapshot.versions,
                        membership: Arc::clone(membership),
                    };
                    let adopted = hub.sync.observe(observation, now);
                    if adopted {
                        // The list shows a catalog now; the dropped draft's notice goes.
                        hub.notice = None;
                    }
                    adopted
                };
                if adopted {
                    hub.windows_read =
                        Some((hub.source_generation, hub.sync.revision(), snapshot.node_id));
                }
                // A held pre-operation observation is not a new recovery source.
                if let Some(current) = hub.sync.observed_value() {
                    Self::queue_recovery(&mut hub, &current);
                }
                hub
            }
            OrganizationUpdate::Failed { message, .. } => {
                let mut hub = self.lock();
                if generation != hub.source_generation {
                    return;
                }
                hub.sync.source_failed(message);
                hub.windows_read = None;
                hub
            }
            OrganizationUpdate::Ended => {
                let mut hub = self.lock();
                if generation != hub.source_generation {
                    return;
                }
                hub.sync.source_ended();
                hub.windows_read = None;
                hub
            }
        };
        // Every update, since a state can change readiness without a new
        // revision (the first one after a restored draft, or after an end).
        self.publish(&mut hub);
        self.release(hub);
        // After a failed apply, a state is the tick that sends it again.
        self.0.wake.notify_all();
    }

    fn view(&self, hub: &Hub) -> SyncView {
        let mut view = hub.sync.view();
        view.notice = hub.notice.clone();
        view.layout_save_paused = !self.0.layout_writable;
        if !self.0.allowed {
            view.ready = false;
            view.reason = Some(REPLAY_NEEDS_HGS.into());
        }
        if hub.stopped {
            view.ready = false;
            view.reason = Some(STOPPED.into());
        }
        view
    }

    /// Hands the pump what the list shows, and queues the Projects call when
    /// the revision or the status line changed. The call carries the
    /// presented catalog once there is one, and the status line always: a
    /// source that fails before its first state still reaches Projects.
    fn publish(&self, hub: &mut Hub) {
        let view = self.view(hub);
        let windows_node = self.windows_read_node(hub);
        let shown = (
            hub.sync.revision(),
            view.status.clone(),
            view.status_error,
            windows_node.clone(),
        );
        match hub.sync.presented() {
            Some((organization, membership)) => {
                self.0
                    .pump
                    .present(organization, membership, hub.sync.revision(), view)
            }
            None => self.0.pump.set_sync(view),
        }
        if hub.shown.as_ref() == Some(&shown) {
            return;
        }
        let Some(presented) = hub.presented.clone() else {
            return;
        };
        let value = hub.sync.presented_value().map(|value| {
            windows_node
                .as_deref()
                .and_then(|node| {
                    self.0
                        .pump
                        .local_windows_projects(&value["organization"], node)
                })
                .map_or_else(
                    || Arc::clone(&value),
                    |organization| {
                        let mut shown = (*value).clone();
                        shown["organization"] = organization;
                        Arc::new(shown)
                    },
                )
        });
        let status = (shown.1.clone(), shown.2);
        hub.announcements += 1;
        hub.announce = Some((
            hub.announcements,
            (presented, value, self.0.pump.default_project(), status),
        ));
        hub.shown = Some(shown);
    }

    fn windows_read_node(&self, hub: &Hub) -> Option<String> {
        let (generation, revision, node) = hub.windows_read.as_ref()?;
        if !self.0.allowed
            || !self.0.layout_writable
            || hub.stopped
            || hub.presentation_due
            || hub.recovery_due.is_some()
            || !hub.sync.ready()
            || hub.sync.busy()
            || hub.sync.pending()
            || *generation != hub.source_generation
            || *revision != hub.sync.revision()
        {
            return None;
        }
        self.0
            .pump
            .local_windows_node()
            .filter(|current| current == node)
    }

    fn invalidate_windows_read(&self, generation: u64) {
        let mut hub = self.lock();
        if generation == hub.source_generation {
            hub.windows_read = None;
            self.publish(&mut hub);
        }
        self.release(hub);
    }

    /// Drops the lock, then answers the settled edits and makes the queued
    /// Projects call. A panic in that call is logged, so it stops neither
    /// the worker nor the drain thread.
    fn release(&self, mut hub: MutexGuard<'_, Hub>) {
        let answers = std::mem::take(&mut hub.answers);
        let announce = hub.announce.take();
        drop(hub);
        for (sender, outcome) in answers {
            let _ = sender.send(outcome);
        }
        if let Some((number, (presented, value, default, status))) = announce {
            let mut announced = self
                .0
                .announced
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if number > *announced {
                *announced = number;
                let call = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    presented(value, default, status)
                }));
                if call.is_err() {
                    eprintln!(
                        "zerus-client: the Projects screen's update panicked; project sync goes on"
                    );
                }
            }
        }
    }

    /// A project's folders in the organization the list shows (Qt's
    /// `m_organization`, which New session copies, SessionsWindow.cpp:2923):
    /// at most [`swarm::FOLDERS`], read under the hub's lock without `hgs`.
    /// An unknown project, or no state yet, has none.
    pub fn project_folders(&self, project: &str) -> Vec<Folder> {
        let hub = self.lock();
        hub.sync
            .presented()
            .and_then(|(organization, _)| {
                swarm::projects(organization)
                    .iter()
                    .find(|listed| swarm::project_id(listed) == project)
                    .map(swarm::folders)
            })
            .unwrap_or_default()
    }

    /// Creation placement reads the same presented model as the Sessions
    /// list, including edits accepted but not yet synchronized.
    pub(crate) fn creation_projects(&self, local: &str) -> Vec<zerus_client_core::launch::Project> {
        let hub = self.lock();
        hub.sync
            .presented()
            .map(|(organization, _)| {
                swarm::projects(organization)
                    .iter()
                    .take(swarm::MAX_PROJECTS)
                    .map(|project| zerus_client_core::launch::Project {
                        id: swarm::project_id(project).into(),
                        name: project["name"].as_str().unwrap_or_default().into(),
                        folders: swarm::folders(project)
                            .into_iter()
                            .map(|folder| zerus_client_core::launch::ProjectFolder {
                                machine: if folder.machine == local {
                                    String::new()
                                } else {
                                    folder.machine
                                },
                                path: folder.path,
                            })
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The observed catalog's `swarm_id`, empty before the first state: New
    /// session infers a folder's project only from this swarm
    /// (SessionsWindow.cpp:2922). It is top-level, so pending edits never
    /// change it and the organization is not rebuilt for it.
    pub fn swarm_id(&self) -> String {
        let value = self.lock().sync.observed_value();
        value
            .and_then(|value| value["swarm_id"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// [`Organization::submit_with`] for an intent built beforehand (the
    /// tests, which run on Unix).
    #[cfg(all(test, unix))]
    pub fn submit(&self, intent: Intent) -> EditOutcome {
        self.submit_with(move |_| intent)
    }

    fn choose_default(&self, project: &str) -> Result<(), String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let mut hub = self.lock();
        if let Some(reason) = self.view(&hub).reason {
            return Err(reason);
        }
        self.0.pump.set_default_project(project)?;
        // The shared revision is unchanged. Projects still needs its new
        // device-local default, through the existing presentation callback.
        hub.shown = None;
        self.publish(&mut hub);
        self.release(hub);
        self.0.wake.notify_all();
        Ok(())
    }

    /// The Controller's current refusal; after-bind edits only wait while
    /// its first state has not arrived, never on a later apply.
    pub(crate) fn creation_refusal(&self) -> Option<String> {
        if !self.0.allowed {
            return Some(REPLAY_NEEDS_HGS.into());
        }
        let hub = self.lock();
        if hub.stopped {
            return Some(STOPPED.into());
        }
        hub.sync.view().reason
    }

    /// Records a creation's edit without waiting for swarm apply, as Qt's
    /// resolvePlacement edits its local model (SessionsWindow.cpp:1539-1569).
    /// A later sync failure belongs to the organization's status line.
    pub(crate) fn accept(&self, intent: impl FnOnce(&Pump) -> Intent) -> Result<(), String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let mut hub = self.lock();
        if hub.stopped {
            return Err(STOPPED.into());
        }
        let intent = intent(&self.0.pump);
        // Qt's addFolder returns without changed() for these no-ops.
        // The Controller's readiness refusal still takes precedence.
        if matches!(&intent, Intent::AddFolder { .. })
            && hub.sync.ready()
            && hub.sync.presented().is_some_and(|(current, _)| {
                zerus_client_core::swarm::edit::apply(current, &intent)
                    .is_ok_and(|next| next == *current)
            })
        {
            return Ok(());
        }
        hub.sync.edit(&intent, self.now())?;
        self.0.pump.edited(&intent);
        self.publish(&mut hub);
        self.release(hub);
        self.0.wake.notify_all();
        Ok(())
    }

    /// Runs one edit; answers once the first apply that carries it ended.
    /// A failed one answers `Kept`, and the edit stays pending for the next
    /// tick. The intent is built under the hub's lock from the pump's
    /// grouping (hub, then pump: the documented order), so Delete and Sort
    /// read their members from the organization the edit applies to, never
    /// from one a state replaced in between.
    fn submit_with(&self, intent: impl FnOnce(&Pump) -> Intent) -> EditOutcome {
        self.submit_checked(move |pump| Ok(intent(pump)))
    }

    fn submit_checked(&self, intent: impl FnOnce(&Pump) -> Result<Intent, String>) -> EditOutcome {
        if !self.0.allowed {
            return EditOutcome::Refused {
                detail: REPLAY_NEEDS_HGS.into(),
            };
        }
        let answer = {
            let mut hub = self.lock();
            if hub.stopped {
                return EditOutcome::Kept {
                    detail: STOPPED.into(),
                };
            }
            let intent = match intent(&self.0.pump) {
                Ok(intent) => intent,
                Err(detail) => return EditOutcome::Refused { detail },
            };
            let ticket = match hub.sync.edit(&intent, self.now()) {
                Ok(ticket) => ticket,
                Err(detail) => return EditOutcome::Refused { detail },
            };
            self.0.pump.edited(&intent);
            let (sender, receiver) = sync_channel(1);
            hub.waiting.insert(ticket, sender);
            self.publish(&mut hub);
            self.release(hub);
            receiver
        };
        self.0.wake.notify_all();
        answer.recv().unwrap_or(EditOutcome::Kept {
            detail: STOPPED.into(),
        })
    }

    /// Writes the pending draft and the layout now, as the app quits. It
    /// takes the worker's writer, so their writes never interleave, and it
    /// never runs `hgs`: an edit made while an apply runs is in the draft,
    /// which the next start sends. It runs on the main thread, and so does
    /// a Projects call it makes ([`Presented`]).
    pub fn flush(&self) {
        // No token serialization or I/O on main. A slow token write must
        // never hold up the independent draft/layout checkpoint after 500 ms.
        self.runs_writer().flush(RUNS_EXIT_WAIT);
        let mut writer = self.writer();
        let hub = self.save_draft(self.lock(), &mut writer);
        self.release(hub);
        writer.save_layout(&self.0.pump, &self.0.layout, self.0.layout_writable);
    }

    fn settle(hub: &mut Hub, settled: Vec<(Ticket, EditOutcome)>) {
        for (ticket, outcome) in settled {
            if let Some(sender) = hub.waiting.remove(&ticket) {
                hub.answers.push((sender, outcome));
            }
        }
    }

    fn spawn(&self) {
        let shared = Arc::downgrade(&self.0);
        std::thread::Builder::new()
            .name("zerus-organization".into())
            .spawn(move || {
                let _stop = Stop(shared.clone());
                // Each pass waits at most a second, so the thread ends soon
                // after the last handle.
                while let Some(shared) = shared.upgrade() {
                    Organization(shared).pass();
                }
            })
            .expect("the organization thread starts");
    }

    /// One pass of the worker: renames, the draft file, a due apply, the
    /// layout, then a wait until the next flush or tick (at most 1 s), or
    /// until a state or an edit wakes it.
    fn pass(&self) {
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering::SeqCst;
            self.0.passes.fetch_add(1, SeqCst);
            if self.0.fail.load(SeqCst) {
                panic!("a test stops the organization worker");
            }
        }
        let presentation = {
            let mut hub = self.lock();
            if hub.stopped || !std::mem::take(&mut hub.presentation_due) {
                None
            } else {
                hub.qt_recovery.clone()
            }
        };
        if let Some(recovery) = presentation {
            if let Err(detail) = self.recover_presentation(&recovery) {
                let mut hub = self.lock();
                hub.sync.source_failed(&detail);
                hub.windows_read = None;
                self.publish(&mut hub);
                self.release(hub);
            }
            return;
        }
        // No source or settings I/O and no CLI call holds the hub, pump or
        // writer lock. A failed false-state attempt retries at the next
        // startup; a later initialized state can reconcile a lost reply.
        let recovery = {
            let mut hub = self.lock();
            if hub.stopped {
                None
            } else {
                let recovery = hub.recovery_due.take().zip(hub.qt_recovery.clone());
                if recovery.is_some() {
                    hub.windows_read = None;
                }
                recovery
            }
        };
        if let Some((node, recovery)) = recovery {
            if let Err(detail) = swarm::qt_import::recover(
                &self.0.hgs,
                &[],
                &recovery.source,
                &recovery.settings,
                &node,
            ) {
                let mut hub = self.lock();
                hub.sync.source_failed(&detail);
                hub.windows_read = None;
                self.publish(&mut hub);
                self.release(hub);
            }
            return;
        }
        if self.catalog_pass() {
            return;
        }
        if let Err(detail) = self.recover_windows_folders() {
            let mut hub = self.lock();
            hub.notice = Some(detail);
            hub.shown = None;
            self.publish(&mut hub);
            self.release(hub);
        }
        self.runs_writer().request(false);
        let mut writer = self.writer();
        let mut hub = self.lock();
        self.renames(&mut hub);
        self.archive_moves(&mut hub);
        hub = self.save_draft(hub, &mut writer);
        match hub.sync.due(self.now()) {
            Due::Apply(invocation) => {
                // The list shows the apply running while `hgs` runs without
                // the hub's lock, and `flush` may write meanwhile. The
                // writer's lock goes first, so `flush`, which the main thread
                // runs as the app quits, never waits behind the Projects call.
                self.publish(&mut hub);
                drop(writer);
                self.release(hub);
                let exit = run_limited(&self.0.hgs, &invocation, &[], Read::Swarm.limit())
                    .unwrap_or_else(|too| Exit::Abnormal {
                        stderr: ReadError::TooLarge { limit: too.cap }.to_string(),
                    });
                let mut hub = self.lock();
                // A failure keeps the edits; the controller arms the next tick.
                let settled = hub.sync.finish(exit, self.now());
                Self::settle(&mut hub, settled);
                self.publish(&mut hub);
                self.release(hub);
                return;
            }
            // Edits that changed nothing shared, or (no tickets) a state the
            // guard held back adopted at its end: a new revision either way.
            Due::Settled(settled) => {
                Self::settle(&mut hub, settled);
                self.publish(&mut hub);
                drop(writer);
                self.release(hub);
                return;
            }
            Due::Nothing => {}
        }
        // The drain invokes observe before updating Pump. A Live status can
        // therefore arrive just after the accepted organization. Recheck on
        // this existing idle pass; no read, poller or cached-source adoption.
        if hub
            .shown
            .as_ref()
            .is_some_and(|shown| shown.3 != self.windows_read_node(&hub))
        {
            self.publish(&mut hub);
        }
        self.release(hub);
        if writer.saved.elapsed() >= SAVE_EVERY {
            writer.save_layout(&self.0.pump, &self.0.layout, self.0.layout_writable);
        }
        drop(writer);
        let hub = self.lock();
        #[cfg(test)]
        self.0
            .parked
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let now = self.now();
        let wait = hub
            .sync
            .next_due()
            .map_or(IDLE_MS, |at| at.saturating_sub(now))
            .clamp(1, IDLE_MS);
        drop(
            self.0
                .wake
                .wait_timeout(hub, Duration::from_millis(wait))
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// Renames the list saw become `Renamed` edits while edits are accepted
    /// (renameSession). Until then they stay with the grouping, and a late
    /// one changes nothing; a blocked replay, which edits nothing, drops them.
    /// A rename whose old name is no member of the presented organization
    /// (a fallback row) makes no edit, as Qt's renameSession returns for an
    /// unknown id (SessionOrganization.cpp:206).
    fn renames(&self, hub: &mut Hub) {
        if !self.0.allowed {
            let _ = self.0.pump.take_renames();
            return;
        }
        if !hub.sync.ready() {
            return;
        }
        let mut renamed = false;
        for (from, to) in self.0.pump.take_renames() {
            // Read for each one: an earlier rename may have made `from` a member.
            let member = hub
                .sync
                .presented()
                .is_some_and(|(_, membership)| membership.contains_key(&from));
            if !member {
                if hub.sync.pending() {
                    // An old member can be absent because of our own unsaved
                    // rename. A duplicate/late-load candidate must not commit
                    // its token before that pending draft is recoverable.
                    hub.acknowledge_after_draft(from, to);
                } else {
                    self.0.pump.acknowledge_rename(&from, &to);
                }
                continue;
            }
            let intent = Intent::Renamed { from, to };
            if hub.sync.edit(&intent, self.now()).is_ok() {
                self.0.pump.edited(&intent);
                if let Intent::Renamed { from, to } = intent {
                    hub.acknowledge_after_draft(from, to);
                }
                renamed = true;
            }
        }
        if renamed {
            self.publish(hub);
        }
    }

    fn archive_moves(&self, hub: &mut Hub) {
        if !self.0.allowed || hub.stopped || !hub.sync.ready() {
            return;
        }
        let mut edited = false;
        for (id, intent) in self.0.pump.archive_moves() {
            if hub.sync.edit(&intent, self.now()).is_ok() {
                self.0.pump.archive_move_submitted(id);
                self.0.pump.edited(&intent);
                edited = true;
            }
        }
        if edited {
            self.publish(hub);
        }
    }

    /// Writes the draft file while it lags the pending edits, under the
    /// writer's lock and without the hub's: an edit is on disk before the
    /// apply that carries it, and the file goes once the edits settled
    /// (SessionsWindow.cpp:2496). Only a write that succeeded is confirmed;
    /// a failed one is offered again on the next pass.
    fn save_draft<'a>(
        &'a self,
        mut hub: MutexGuard<'a, Hub>,
        writer: &mut Writer,
    ) -> MutexGuard<'a, Hub> {
        while let Some((change, write)) = hub.sync.take_draft() {
            let archive_acks = self.0.pump.submitted_termination_ids();
            self.release(hub);
            let written = writer.store_draft(&self.0.draft, &write);
            hub = self.lock();
            if !written {
                return hub;
            }
            hub.sync.draft_written(change);
            for id in archive_acks {
                self.0.pump.archive_move_saved(id);
            }
        }
        // No draft changes lag disk. This also releases a no-op queued behind
        // an already durable pending draft without waiting for another edit.
        for (from, to) in std::mem::take(&mut hub.run_acks) {
            self.0.pump.acknowledge_rename(&from, &to);
        }
        hub
    }
}

/// The draft of the last run, as `new` takes it up.
#[derive(Default)]
struct Saved {
    draft: Option<Draft>,
    /// An unusable draft is still in place: the writer moves it first.
    aside: bool,
    /// Why the draft was not applied, for the list.
    notice: Option<String>,
}

/// The draft of the last run. One that cannot be used is never applied: the
/// reason goes to the log and the list, and the file aside before any new
/// draft replaces it.
fn saved_draft(path: &Path) -> Saved {
    match draft::load(path) {
        Load::Missing => Saved::default(),
        Load::Loaded(draft) => Saved {
            draft: Some(draft),
            ..Saved::default()
        },
        Load::Dropped(reason) => {
            let notice = format!("The saved project changes were not applied: {reason}");
            eprintln!("zerus-client: {notice}");
            let aside = match set_aside(path) {
                Ok(()) => false,
                Err(error) => {
                    eprintln!("zerus-client: the unusable draft could not be moved aside: {error}");
                    true
                }
            };
            Saved {
                draft: None,
                aside,
                notice: Some(notice),
            }
        }
    }
}

/// Moves an unusable draft file aside (`draft::set_aside`); one already
/// gone is fine.
fn set_aside(path: &Path) -> io::Result<()> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    match draft::set_aside(path, seconds) {
        Ok(aside) => {
            eprintln!(
                "zerus-client: the unusable draft was moved to {}",
                aside.display()
            );
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Lets a repeating failure through at most once per [`LOG_EVERY`].
#[derive(Default)]
struct Throttle(Option<Instant>);

impl Throttle {
    fn allow(&mut self) -> bool {
        let allowed = self.0.is_none_or(|at| at.elapsed() >= LOG_EVERY);
        if allowed {
            self.0 = Some(Instant::now());
        }
        allowed
    }
}

/// Draft/layout state and the independent token worker's handle. Copy that
/// handle briefly under Shared.writer; never wait for token I/O with this lock.
struct Writer {
    /// Separate lock: stuck token I/O never blocks the draft/layout writer.
    runs: Arc<RunsWorker>,
    /// An unusable draft is still in place; no draft is written before it moved.
    aside: bool,
    saved: Instant,
    /// The newest layout whose write failed, written again with the next save.
    layout: Option<Vec<u8>>,
    draft_log: Throttle,
    layout_log: Throttle,
    /// Layout saves done, so a test can tell one more ran.
    #[cfg(test)]
    saves: usize,
}

impl Writer {
    fn new(aside: bool, pump: Pump, path: PathBuf) -> Self {
        Self {
            runs: Arc::new(RunsWorker::new(pump, path)),
            aside,
            saved: Instant::now(),
            layout: None,
            draft_log: Throttle::default(),
            layout_log: Throttle::default(),
            #[cfg(test)]
            saves: 0,
        }
    }

    /// Whether the draft file now holds `write`.
    fn store_draft(&mut self, path: &Path, write: &DraftWrite) -> bool {
        if self.aside {
            if let Err(error) = set_aside(path) {
                if self.draft_log.allow() {
                    eprintln!("zerus-client: the unusable draft could not be moved aside: {error}");
                }
                return false;
            }
            self.aside = false;
        }
        match draft::store(path, write) {
            Ok(()) => true,
            Err(error) => {
                if self.draft_log.allow() {
                    eprintln!("zerus-client: the pending project changes were not saved: {error}");
                }
                false
            }
        }
    }

    /// Saves a changed layout through a temporary file and a rename.
    fn save_layout(&mut self, pump: &Pump, path: &Path, writable: bool) {
        self.saved = Instant::now();
        if !writable {
            return;
        }
        let bytes = match pump.take_layout() {
            // Newer than any layout kept from a failed write, which goes.
            Some(bytes) => {
                self.layout = None;
                Some(bytes)
            }
            None => self.layout.take(),
        };
        if let Some(bytes) = bytes {
            if let Err(error) = draft::write(path, &bytes) {
                if self.layout_log.allow() {
                    eprintln!("zerus-client: the project layout was not saved: {error}");
                }
                self.layout = Some(bytes);
            }
        }
        #[cfg(test)]
        {
            self.saves += 1;
        }
    }
}

/// A coalescing mailbox, independent of the file writer's state/lock. A slow
/// token write cannot delay an organization pass or accumulate worker threads.
#[derive(Default)]
struct RunsRequests {
    tick: bool,
    force: bool,
    requested: u64,
    completed: u64,
    stop: bool,
    stopped: bool,
}

struct RunsWorker {
    requests: Arc<(Mutex<RunsRequests>, Condvar)>,
    #[cfg(all(test, unix))]
    state: Arc<Mutex<RunsWriter>>,
}

impl RunsWorker {
    fn new(pump: Pump, path: PathBuf) -> Self {
        let requests = Arc::new((
            Mutex::new(RunsRequests {
                tick: true,
                ..RunsRequests::default()
            }),
            Condvar::new(),
        ));
        let state = Arc::new(Mutex::new(RunsWriter::default()));
        let queue = requests.clone();
        let writer = state.clone();
        std::thread::Builder::new()
            .name("zerus-run-identities".into())
            .spawn(move || {
                let (lock, wake) = &*queue;
                loop {
                    let mut request = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    while !request.tick && !request.force && !request.stop {
                        request = wake.wait(request).unwrap_or_else(PoisonError::into_inner);
                    }
                    if request.stop {
                        request.stopped = true;
                        wake.notify_all();
                        break;
                    }
                    let force = std::mem::take(&mut request.force);
                    request.tick = false;
                    let checkpoint = request.requested;
                    drop(request);
                    writer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .save(&pump, &path, force);
                    let mut request = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    request.completed = checkpoint;
                    wake.notify_all();
                }
            })
            .expect("the run identities thread starts");
        Self {
            requests,
            #[cfg(all(test, unix))]
            state,
        }
    }

    fn request(&self, force: bool) -> u64 {
        let (lock, wake) = &*self.requests;
        let mut request = lock.lock().unwrap_or_else(PoisonError::into_inner);
        request.tick = true;
        if force {
            request.force = true;
            request.requested += 1;
        }
        let checkpoint = request.requested;
        wake.notify_all();
        checkpoint
    }

    fn flush(&self, bound: Duration) {
        let checkpoint = self.request(true);
        let (lock, wake) = &*self.requests;
        let request = lock.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            wake.wait_timeout_while(request, bound, |request| request.completed < checkpoint)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    #[cfg(all(test, unix))]
    fn stop(&self) {
        let (lock, wake) = &*self.requests;
        let mut request = lock.lock().unwrap();
        request.stop = true;
        wake.notify_all();
        let (request, _) = wake
            .wait_timeout_while(request, Duration::from_secs(5), |request| !request.stopped)
            .unwrap();
        assert!(request.stopped, "run identities worker did not stop");
    }
}

impl Drop for RunsWorker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.requests;
        lock.lock().unwrap_or_else(PoisonError::into_inner).stop = true;
        wake.notify_all();
    }
}

enum RunsFile {
    Missing,
    Oversize,
    Bytes(Vec<u8>),
}

fn read_runs(path: &Path) -> io::Result<RunsFile> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RunsFile::Missing),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(runs::MAX_BYTES + 1).read_to_end(&mut bytes)?;
    // The size bound takes precedence over version protection. An oversized
    // file is never prefix-parsed, even when it claims a newer version.
    Ok(if bytes.len() as u64 > runs::MAX_BYTES {
        RunsFile::Oversize
    } else {
        RunsFile::Bytes(bytes)
    })
}

/// Only the dedicated thread takes this lock. Snapshot copying is bounded;
/// serialization and file I/O run without the pump, hub or Shared.writer.
struct RunsWriter {
    loaded: bool,
    /// A newer file stays untouched throughout this run, including exit.
    read_only: bool,
    saved: Instant,
    pending: Option<runs::Snapshot>,
    log: Throttle,
    #[cfg(test)]
    stuck: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    thread: Option<std::thread::ThreadId>,
}

impl Default for RunsWriter {
    fn default() -> Self {
        Self {
            loaded: false,
            read_only: false,
            saved: Instant::now(),
            pending: None,
            log: Throttle::default(),
            #[cfg(test)]
            stuck: None,
            #[cfg(test)]
            thread: None,
        }
    }
}

impl RunsWriter {
    fn save(&mut self, pump: &Pump, path: &Path, force: bool) {
        if self.read_only {
            return;
        }
        if !self.loaded {
            match read_runs(path) {
                Ok(RunsFile::Missing) => {}
                Ok(RunsFile::Oversize) => {
                    eprintln!(
                        "zerus-client: run identities exceed the size limit; starting afresh"
                    );
                }
                Ok(RunsFile::Bytes(bytes)) => match pump.load_runs(&bytes) {
                    Ok(()) => {}
                    Err(runs::DecodeError::Newer(version)) => {
                        eprintln!("zerus-client: run identities use newer version {version}; leaving the file untouched");
                        self.read_only = true;
                        return;
                    }
                    Err(runs::DecodeError::Invalid(_)) => {
                        eprintln!("zerus-client: invalid run identities; starting afresh");
                    }
                },
                Err(error) => {
                    if self.log.allow() {
                        eprintln!("zerus-client: the run identities were not loaded: {error}");
                    }
                    // Transient I/O errors retry without replacing the history.
                    return;
                }
            }
            self.loaded = true;
        }
        if !force && self.saved.elapsed() < SAVE_EVERY {
            return;
        }
        self.saved = Instant::now();
        let snapshot = pump.take_runs().or_else(|| self.pending.take());
        let Some(snapshot) = snapshot else { return };
        // A new snapshot supersedes the bytes of any failed write.
        self.pending = None;
        #[cfg(test)]
        {
            self.thread = Some(std::thread::current().id());
            if let Some((entered, release)) = self.stuck.take() {
                let _ = entered.send(());
                let _ = release.recv();
            }
        }
        let bytes = snapshot.to_json();
        if let Err(error) = draft::write(path, &bytes) {
            if self.log.allow() {
                eprintln!("zerus-client: the run identities were not saved: {error}");
            }
            self.pending = Some(snapshot);
        }
    }
}

/// Runs when the worker ends. After a panic the hub outlives it: the list
/// shows that project sync stopped, and the edits waiting on the worker, and
/// later ones, answer `Kept(STOPPED)`.
struct Stop(Weak<Shared>);

impl Drop for Stop {
    fn drop(&mut self) {
        let Some(shared) = self.0.upgrade() else {
            return;
        };
        let organization = Organization(shared);
        let mut hub = organization.lock();
        hub.stopped = true;
        let waiting = std::mem::take(&mut hub.waiting);
        for sender in waiting.into_values() {
            hub.answers.push((
                sender,
                EditOutcome::Kept {
                    detail: STOPPED.into(),
                },
            ));
        }
        organization.publish(&mut hub);
        organization.release(hub);
    }
}

/// Runs `intent` off the main thread; it is built under the hub's lock
/// ([`Organization::submit_with`]).
async fn edit(
    state: State<'_, Organization>,
    intent: impl FnOnce(&Pump) -> Intent + Send + 'static,
) -> Result<EditOutcome, String> {
    let organization = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || organization.submit_with(intent))
        .await
        .map_err(|error| error.to_string())
}

/// Move to project for one session or a selection: one `swarm apply` with
/// every member, from one snapshot. Members are `"<host>\n<session name>"`;
/// `before` places them before that member (a drop).
#[tauri::command]
pub async fn move_to_project(
    state: State<'_, Organization>,
    members: Vec<String>,
    project: String,
    before: Option<String>,
) -> Result<EditOutcome, String> {
    edit(state, move |_| Intent::Move {
        members,
        project,
        before,
    })
    .await
}

/// New project…, with the sessions it takes (none from the list's menu).
#[derive(serde::Serialize)]
pub struct CreatedProject {
    #[serde(flatten)]
    outcome: EditOutcome,
    /// The writer's reserved identity, never a name-based lookup.
    project: String,
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, Organization>,
    name: String,
    members: Vec<String>,
) -> Result<CreatedProject, String> {
    let project = uuid::Uuid::new_v4().to_string();
    let id = project.clone();
    let outcome = edit(state, move |_| Intent::Create { id, name, members }).await?;
    Ok(CreatedProject { outcome, project })
}

/// Uses the existing shared organization writer; local vivid never enters it.
#[tauri::command]
pub async fn set_project_color(
    state: State<'_, Organization>,
    project: String,
    color: String,
) -> Result<EditOutcome, String> {
    edit(state, move |_| Intent::Color { project, color }).await
}

#[tauri::command]
pub async fn rename_project(
    state: State<'_, Organization>,
    project: String,
    name: String,
) -> Result<EditOutcome, String> {
    edit(state, move |_| Intent::Rename { project, name }).await
}

/// Delete project…: its members and the rows it lists go to `destination`.
#[tauri::command]
pub async fn delete_project(
    state: State<'_, Organization>,
    project: String,
    destination: String,
) -> Result<EditOutcome, String> {
    edit(state, move |pump| {
        let members = pump
            .members_of(&project)
            .into_iter()
            .map(|(member, _)| member)
            .collect();
        Intent::Delete {
            project,
            destination,
            members,
        }
    })
    .await
}

#[tauri::command]
pub async fn move_project(
    state: State<'_, Organization>,
    project: String,
    before: Option<String>,
) -> Result<EditOutcome, String> {
    edit(state, move |_| Intent::Reorder { project, before }).await
}

/// Sort sessions by name: the project's listed rows, by title.
#[tauri::command]
pub async fn sort_project(
    state: State<'_, Organization>,
    project: String,
) -> Result<EditOutcome, String> {
    edit(state, move |pump| {
        let members = by_name(pump.members_of(&project));
        Intent::Sort { project, members }
    })
    .await
}

/// New session's copy of a project's folders (SessionsWindow.cpp:2923,
/// NewSessionDialog.cpp:651): [`Organization::project_folders`], off the
/// main thread. It never runs `hgs`.
#[tauri::command(async)]
pub fn project_folders(state: State<'_, Organization>, project: String) -> Vec<Folder> {
    state.project_folders(&project)
}

/// The default is this computer's local presentation; never a shared edit.
#[tauri::command(async)]
pub fn set_default_project(state: State<'_, Organization>, project: String) -> Result<(), String> {
    state.choose_default(&project)
}

/// Expand or collapse a project: this device's layout, no `hgs`. Off the
/// main thread, as every command that takes a lock: the pump's lock may be
/// held by a grouping pass over every row.
#[tauri::command(async)]
pub fn set_project_collapsed(state: State<'_, Organization>, project: String, collapsed: bool) {
    state.0.pump.set_collapsed(&project, collapsed);
}

#[cfg(all(test, unix))]
mod tests {
    mod swarm_conflicts;
    mod swarm_dialog_cases;
    mod windows_folders;
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn local_default_publishes_projects_and_uses_only_the_layout_writer() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let org = fake.organization(&pump, true);
        org.set_source(snapshot("d1"));
        org.observe(&state("d1"));
        assert!(org.choose_default("missing").is_err());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        org.set_presented(Arc::new(move |_, default, _| {
            seen.lock().unwrap().push(default);
        }));
        org.choose_default("alpha").unwrap();
        assert_eq!(pump.default_project(), "alpha");
        assert_eq!(
            calls.lock().unwrap().last().map(String::as_str),
            Some("alpha")
        );
        org.flush();
        assert_eq!(
            zerus_client_core::swarm::layout::Layout::from_json(
                &std::fs::read(fake.layout()).unwrap()
            )
            .default_project(),
            Some("alpha")
        );
        assert!(fake.applies().is_empty());
        fake.finish(org);
    }

    #[test]
    fn creation_keeps_the_edit_outcome_and_returns_its_exact_project() {
        for outcome in [
            EditOutcome::Done { conflicts: 0 },
            EditOutcome::Kept {
                detail: "offline".into(),
            },
            EditOutcome::Refused {
                detail: "not ready".into(),
            },
        ] {
            let mut expected = serde_json::to_value(&outcome).unwrap();
            expected["project"] = json!("reserved-project");
            assert_eq!(
                serde_json::to_value(CreatedProject {
                    outcome,
                    project: "reserved-project".into(),
                })
                .unwrap(),
                expected
            );
        }
    }
    use zerus_client_core::swarm::edit::shared;
    use zerus_client_core::{ComputerId, ViewRow};

    const R: &str = "workstation\nclaude/sample-project/review";
    const R2: &str = "workstation\nclaude/sample-project/review-2";
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn full(digest: &str) -> Value {
        json!({"schema": 1, "node_id": "node-1", "digest": digest, "initialized": true, "machines": [], "peers": {}, "conflicts": [],
               "organization": {"version": 2, "default_project": "ungrouped", "projects": [
                   {"id": "alpha", "name": "Alpha", "color": "#3fb950", "accessible": true, "folders": [], "sessions": [R]},
                   {"id": "ungrouped", "name": "Inbox", "color": "#8d9baa", "accessible": true, "folders": [], "sessions": []}]},
               "versions": {}})
    }

    /// A scratch folder with the core's fake hgs, its `swarm apply` answering
    /// `apply` in turn. The fake runs as a child of the tests, so it holds
    /// `crate::PROCESS_TREE` shared.
    struct Fake {
        dir: PathBuf,
        hgs: PathBuf,
        _tree: std::sync::RwLockReadGuard<'static, ()>,
    }

    impl Fake {
        fn new(apply: Value) -> Self {
            let tree = crate::PROCESS_TREE
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let dir = std::env::temp_dir().join(format!(
                "zerus-organization-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let hgs = dir.join("hgs");
            // A symlink to the checked-in script, never a copy (see
            // core/tests/actions.rs): a new executable written while another
            // test thread forks fails to exec with ETXTBSY.
            std::os::unix::fs::symlink(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../core/tests/support/fake_hgs_swarm.py"),
                &hgs,
            )
            .unwrap();
            let config = json!({"get": [{"stdout": full("d1")}], "apply": apply});
            std::fs::write(
                dir.join("config.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            Self {
                dir,
                hgs,
                _tree: tree,
            }
        }

        /// Every apply answers with the catalog at `d2`.
        fn landing() -> Self {
            Self::new(json!([{"stdout": full("d2")}]))
        }

        fn layout(&self) -> PathBuf {
            self.dir.join("layout.json")
        }

        fn draft(&self) -> PathBuf {
            self.dir.join("draft.json")
        }

        fn runs(&self) -> PathBuf {
            self.dir.join(zerus_client_core::swarm::runs::FILE)
        }

        fn organization(&self, pump: &Pump, allowed: bool) -> Organization {
            Organization::new(
                pump.clone(),
                self.hgs.clone(),
                allowed,
                self.layout(),
                self.draft(),
                false,
            )
        }

        /// The `swarm apply` lines of the fake's log.
        fn applies(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("log.ndjson"))
                .unwrap_or_default()
                .lines()
                .filter(|line| line.contains("\"apply\""))
                .map(str::to_owned)
                .collect()
        }

        /// The edits settled after `count` applies: the draft written before
        /// the first one is gone.
        fn settled_after(&self, count: usize) -> bool {
            self.applies().len() >= count && !self.draft().exists()
        }

        /// Drops the hub and waits for its worker to end, so nothing writes
        /// into the folder or runs the fake once it is removed.
        fn finish(self, organization: Organization) {
            let runs = organization.runs_writer();
            let shared = Arc::downgrade(&organization.0);
            drop(organization);
            wait_until("the worker to end", || shared.strong_count() == 0);
            runs.stop();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Polls `done` every 20 ms for at most 10 s.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn presentation_import_checkpoints_with_its_record_and_a_second_start_never_reads_qt() {
        let fake = Fake::landing();
        let qt = fake.dir.join("synthetic-qt.conf");
        let org = json!({"version":2,"default_project":"beta","projects":[
            {"id":"alpha","name":"Alpha","collapsed":true,"sessions":["peer\ncodex/example/two","peer\ncodex/example/one"]},
            {"id":"beta","name":"Beta","sessions":[]}]});
        let original = format!(
            "[workspace]\norganization={}\n",
            serde_json::to_string(&org.to_string()).unwrap()
        );
        std::fs::write(&qt, &original).unwrap();
        let mtime = std::fs::metadata(&qt).unwrap().modified().unwrap();
        std::fs::write(
            fake.layout(),
            json!({"version":1,"collapsed":[],"order":{},"default_project":null,
            "run_groups":[["peer\noriginal-run","next-run-project"]],"runs_seeded":true})
            .to_string(),
        )
        .unwrap();
        let settings_file = fake.dir.join("settings.json");
        std::fs::write(
            &settings_file,
            json!({"version":1,"seed":{"at":0},"values":{}}).to_string(),
        )
        .unwrap();
        let settings = zerus_client_core::settings::Settings::open(settings_file, None);
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        pump.present(&org, Arc::new(Default::default()), 1, SyncView::default());
        organization.recover_qt(
            Some(zerus_client_core::settings::QtStore::Ini(qt.clone())),
            settings.clone(),
        );
        wait_until(
            "the presentation family and values to checkpoint together",
            || {
                std::fs::read(fake.layout())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|value| value["g3_organization_presentation"]["version"] == 1)
            },
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(fake.layout()).unwrap()).unwrap();
        assert_eq!(saved["collapsed"], json!(["alpha"]));
        assert_eq!(saved["default_project"], "beta");
        assert_eq!(pump.default_project(), "beta");
        assert_eq!(
            saved["run_groups"],
            json!([["peer\noriginal-run", "next-run-project"]])
        );
        assert_eq!(
            saved["g3_organization_presentation"]["source_mtime_after_switch"],
            true
        );
        assert_eq!(std::fs::read_to_string(&qt).unwrap(), original);
        assert_eq!(std::fs::metadata(&qt).unwrap().modified().unwrap(), mtime);
        assert!(
            !fake.dir.join("log.ndjson").exists(),
            "local presentation runs no hgs command"
        );
        let runs = organization.runs_writer();
        let shared = Arc::downgrade(&organization.0);
        drop(organization);
        wait_until("the first presentation worker to end", || {
            shared.strong_count() == 0
        });
        runs.stop();

        std::fs::write(&qt, b"[workspace]\norganization=malformed\n").unwrap();
        let before = std::fs::read(fake.layout()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        let before_parked = parked(&organization);
        organization.recover_qt(
            Some(zerus_client_core::settings::QtStore::Ini(qt)),
            settings.clone(),
        );
        wait_until("the handled family's startup pass to finish", || {
            parked(&organization) > before_parked
        });
        assert!(pump.presentation_seeded().unwrap());
        assert!(!organization.lock().sync.view().status_error);
        organization.flush();
        assert_eq!(std::fs::read(fake.layout()).unwrap(), before);
        assert!(!fake.dir.join("log.ndjson").exists());
        drop(settings);
        fake.finish(organization);
    }

    #[test]
    fn a_refused_layout_also_blocks_presentation_import_without_reading_qt() {
        let fake = Fake::landing();
        let original = b"{\"version\":99,\"unknown\":\"retained\"}";
        std::fs::write(fake.layout(), original).unwrap();
        let organization = fake.organization(&Pump::default(), true);
        let settings =
            zerus_client_core::settings::Settings::open(fake.dir.join("settings.json"), None);
        let recovery = QtRecovery {
            source: zerus_client_core::settings::QtStore::Ini(fake.dir.clone()),
            settings,
        };
        // The source is a directory: reading it would fail. The refused target
        // must short-circuit before opening any source or replacing the layout.
        assert!(organization.recover_presentation(&recovery).is_ok());
        organization.flush();
        assert_eq!(std::fs::read(fake.layout()).unwrap(), original);
        fake.finish(organization);
    }

    #[test]
    fn qt_recovery_uses_the_existing_worker_without_holding_ui_or_checkpoint_locks() {
        let fake = Fake::landing();
        let node = "11111111-1111-4111-8111-111111111111";
        let qt = fake.dir.join("synthetic-qt.conf");
        let org = json!({"version":2,"projects":[{"id":"alpha","name":"Qt project","color":"#123456","sessions":[],"folders":[]}]});
        // QSettings text JSON: the entire value is quoted, not a string list.
        let ini = format!(
            "[workspace]\norganization={}\n",
            serde_json::to_string(&org.to_string()).unwrap()
        );
        std::fs::write(&qt, &ini).unwrap();
        let hash = zerus_client_core::settings::QtStore::Ini(qt.clone())
            .snapshot()
            .unwrap()
            .unwrap()
            .sha256;
        std::fs::write(fake.dir.join("config.json"),json!({"g3-status":[{"gate":"recovery-gate","stdout":{"version":1,"node_id":node,"initialized":false,"gui_import_pending":false}}],
            "g3-import":[{"stdout":{"schema":1,"node_id":node,"initialized":true,"organization":org,"g3_import":{"version":1,"source_sha256":hash}}}]}).to_string()).unwrap();
        let settings =
            zerus_client_core::settings::Settings::open(fake.dir.join("settings.json"), None);
        let organization = fake.organization(&Pump::default(), true);
        organization.recover_qt(
            Some(zerus_client_core::settings::QtStore::Ini(qt.clone())),
            settings.clone(),
        );
        let mut observed = full("d1");
        observed["node_id"] = json!(node);
        observed["initialized"] = json!(false);
        organization.set_source(snapshot_of(&observed));
        organization.observe(&state_of(&observed));
        wait_until("the held recovery status", || {
            std::fs::read_to_string(fake.dir.join("log.ndjson"))
                .is_ok_and(|log| log.contains("--g3-status"))
        });
        let (sent, received) = std::sync::mpsc::channel();
        let flushing = organization.clone();
        let worker = std::thread::spawn(move || {
            flushing.flush();
            sent.send(()).unwrap();
        });
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("a held CLI must not block checkpoint locks");
        worker.join().unwrap();
        organization.observe(&state_of(&observed)); // same false state never queues another import
        std::fs::write(fake.dir.join("recovery-gate"), b"release").unwrap();
        wait_until("the completed recovery record", || {
            settings.organization_import().unwrap()
                == zerus_client_core::settings::OrganizationImport::Handled
        });
        let log = std::fs::read_to_string(fake.dir.join("log.ndjson")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("--g3-status"))
                .count(),
            1
        );
        assert_eq!(
            log.lines().filter(|line| line.contains("\"--g3\"")).count(),
            1
        );
        assert_eq!(std::fs::read_to_string(qt).unwrap(), ini);
        drop(settings);
        fake.finish(organization);
    }

    /// The passes the worker finished.
    fn parked(organization: &Organization) -> usize {
        organization.0.parked.load(Ordering::SeqCst)
    }

    /// The passes the worker started.
    fn passes(organization: &Organization) -> usize {
        organization.0.passes.load(Ordering::SeqCst)
    }

    #[test]
    fn only_a_live_switch_identity_reads_qt_for_the_run_seed() {
        assert!(run_seed_source("dev.zerus.next", false).is_none());
        assert!(run_seed_source("dev.zerus.next.phase2", false).is_none());
        assert!(run_seed_source(crate::settings::SWITCH_IDENTITY, true).is_none());
        assert_eq!(
            run_seed_source(crate::settings::SWITCH_IDENTITY, false),
            crate::settings::seed_source(crate::settings::SWITCH_IDENTITY, false)
        );
    }

    #[test]
    fn a_blocked_run_seed_does_not_stall_catalog_changes_and_merges_the_current_layout() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        let org = full("d1")["organization"].clone();
        pump.present(
            &org,
            Arc::new(zerus_client_core::swarm::members(&org)),
            1,
            SyncView::default(),
        );
        let (entered, started) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let seed = organization
            .seed_runs_with(move || {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok(
                    json!({"workstation\nlive":"alpha", "workstation\nold":"alpha"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
            })
            .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        // A duplicate startup callback neither reads again nor blocks.
        let duplicate = organization.seed_runs_with(|| panic!("the seed is already being read"));
        let changed = pump.clone();
        let (flowed, flow) = std::sync::mpsc::channel();
        let drain = std::thread::spawn(move || {
            changed.push(Change::Upsert {
                computer: ComputerId::local(),
                key: "run:live".into(),
                row: ViewRow {
                    machine: "workstation".into(),
                    name: "codex/sample-project/implicit".into(),
                    state: "running".into(),
                    run_id: Some("live".into()),
                    ..ViewRow::default()
                },
            });
            changed.set_collapsed("alpha", true);
            flowed.send(()).unwrap();
        });
        let result = flow.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        drain.join().unwrap();
        seed.join().unwrap();
        assert!(duplicate.is_none(), "the second call starts no reader");
        assert!(
            result.is_ok(),
            "the catalog flowed before the Qt read finished"
        );
        wait_until("the run seed to merge", || pump.runs_seeded());
        organization.flush();
        let saved = zerus_client_core::swarm::layout::Layout::from_json(
            &std::fs::read(fake.layout()).unwrap(),
        );
        assert!(saved.collapsed("alpha"));
        assert_eq!(
            saved.run_project("workstation", "live"),
            None,
            "live implicit membership clears the stale seed"
        );
        assert_eq!(saved.run_project("workstation", "old"), Some("alpha"));
        assert!(organization
            .seed_runs_with(|| panic!("a saved seed never reads Qt again"))
            .is_none());
        fake.finish(organization);
    }

    #[test]
    fn a_run_seed_read_error_leaves_the_mark_pending_and_can_be_retried() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization
            .seed_runs_with(|| Err("synthetic seed read error".into()))
            .unwrap()
            .join()
            .unwrap();
        wait_until("the seed read to fail", || {
            !organization.0.seed_started.load(Ordering::SeqCst)
        });
        assert!(!pump.runs_seeded());
        organization
            .seed_runs_with(|| {
                Ok(json!({"workstation\nold":"alpha"})
                    .as_object()
                    .unwrap()
                    .clone())
            })
            .unwrap()
            .join()
            .unwrap();
        wait_until("the retry to seed", || pump.runs_seeded());
        organization.flush();
        let saved = zerus_client_core::swarm::layout::Layout::from_json(
            &std::fs::read(fake.layout()).unwrap(),
        );
        assert_eq!(saved.run_project("workstation", "old"), Some("alpha"));
        fake.finish(organization);
    }

    #[test]
    fn layout_refusal_preserves_original_and_blocks_seed_and_later_saves() {
        for original in [
            b"not json".as_slice(),
            br#"{"version":99,"private_extension":"retain"}"#,
        ] {
            let fake = Fake::landing();
            std::fs::write(fake.layout(), original).unwrap();
            let before = std::fs::metadata(fake.layout())
                .unwrap()
                .modified()
                .unwrap();
            let pump = Pump::default();
            let organization = fake.organization(&pump, false);
            assert!(!organization.0.layout_writable);
            assert!(
                organization
                    .view(&organization.0.hub.lock().unwrap())
                    .layout_save_paused
            );
            assert!(organization
                .seed_runs_with(|| panic!("blocked target never reads Qt"))
                .is_none());
            pump.set_collapsed("alpha", true);
            organization.flush();
            assert_eq!(std::fs::read(fake.layout()).unwrap(), original);
            assert_eq!(
                std::fs::metadata(fake.layout())
                    .unwrap()
                    .modified()
                    .unwrap(),
                before
            );
            fake.finish(organization);
        }
        let fake = Fake::landing();
        std::fs::create_dir(fake.layout()).unwrap();
        std::fs::write(fake.layout().join("unknown"), b"retained").unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, false);
        pump.set_collapsed("alpha", true);
        organization.flush();
        assert_eq!(
            std::fs::read(fake.layout().join("unknown")).unwrap(),
            b"retained"
        );
        fake.finish(organization);
    }

    #[test]
    fn layout_extensions_survive_an_ordinary_presentation_save() {
        let fake = Fake::landing();
        let original = json!({"version":1,"collapsed":["alpha"],"future":{"private":"retained"}});
        std::fs::write(fake.layout(), serde_json::to_vec(&original).unwrap()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, false);
        assert!(organization.0.layout_writable);
        assert!(
            !organization
                .view(&organization.0.hub.lock().unwrap())
                .layout_save_paused
        );
        pump.set_collapsed("beta", true);
        organization.flush();
        let saved: Value = serde_json::from_slice(&std::fs::read(fake.layout()).unwrap()).unwrap();
        assert_eq!(saved["future"], original["future"]);
        assert!(saved["collapsed"]
            .as_array()
            .unwrap()
            .contains(&json!("alpha")));
        assert!(saved["collapsed"]
            .as_array()
            .unwrap()
            .contains(&json!("beta")));
        fake.finish(organization);
    }

    #[test]
    fn a_persisted_run_seed_mark_skips_qt_on_the_next_start() {
        let fake = Fake::landing();
        let mut layout = zerus_client_core::swarm::layout::Layout::default();
        layout.seed_runs(&serde_json::Map::new());
        std::fs::write(fake.layout(), layout.to_json()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        assert!(pump.runs_seeded());
        assert!(!organization.0.seed_started.load(Ordering::SeqCst));
        let reader = organization.seed_runs_with(|| panic!("a persisted seed never reads Qt"));
        let skipped = reader.is_none();
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        fake.finish(organization);
        assert!(skipped);
    }

    #[test]
    fn a_blocked_run_seed_never_keeps_the_organization_alive_during_quit() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        let weak = Arc::downgrade(&organization.0);
        let (entered, started) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let seed = organization
            .seed_runs_with(move || {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok(serde_json::Map::new())
            })
            .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(organization);
        let until = Instant::now() + Duration::from_secs(2);
        while weak.strong_count() != 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let ended = weak.strong_count() == 0;
        release.send(()).unwrap();
        seed.join().unwrap();
        wait_until("the organization to end", || weak.strong_count() == 0);
        std::fs::remove_dir_all(fake.dir).unwrap();
        assert!(ended, "quit did not wait for the seed read");
    }

    /// The layout saves done; taken under the writer's lock, so a save in
    /// progress is counted once it ended.
    fn saves(organization: &Organization) -> usize {
        organization.writer().saves
    }

    /// Whether the list's headers name `project` `name`, taking the batch.
    fn header_named(pump: &Pump, project: &str, name: &str) -> bool {
        pump.take(Duration::ZERO).grouping.is_some_and(|grouping| {
            grouping
                .headers
                .iter()
                .any(|header| header.id == project && header.name == name)
        })
    }

    /// The input of a logged `swarm apply`.
    fn input(line: &str) -> Value {
        let entry: Value = serde_json::from_str(line).unwrap();
        serde_json::from_str(entry["stdin"].as_str().unwrap()).unwrap()
    }

    fn state(digest: &str) -> Change {
        let value = full(digest);
        let mut without = value.clone();
        without.as_object_mut().unwrap().remove("versions");
        Change::Organization {
            computer: ComputerId::local(),
            update: OrganizationUpdate::State {
                value: Arc::new(without),
                digest: digest.into(),
                membership: Arc::new(zerus_client_core::swarm::members(&value["organization"])),
            },
        }
    }

    fn snapshot(digest: &'static str) -> SnapshotSource {
        snapshot_of(&full(digest))
    }

    fn snapshot_of(value: &Value) -> SnapshotSource {
        let snapshot = OrganizationSnapshot {
            node_id: value["node_id"].as_str().unwrap_or_default().into(),
            swarm_id: value["swarm_id"].as_str().unwrap_or_default().into(),
            digest: value["digest"].as_str().unwrap_or_default().into(),
            base: value["organization"].clone(),
            versions: value["versions"].clone(),
        };
        Arc::new(move || Some(snapshot.clone()))
    }

    #[test]
    fn source_and_state_need_the_same_node_and_catalog_even_when_digest_matches() {
        let fake = Fake::landing();
        let org = fake.organization(&Pump::default(), true);
        org.set_source(snapshot("d1"));
        let mut other = full("d1");
        other["node_id"] = json!("other-node");
        org.observe(&state_of(&other));
        assert!(org.swarm_snapshot().is_none());
        other["node_id"] = json!("node-1");
        other["swarm_id"] = json!("other-catalog");
        org.observe(&state_of(&other));
        assert!(org.swarm_snapshot().is_none());
        org.observe(&state("d1"));
        assert!(org.swarm_snapshot().is_some());
        assert!(fake.applies().is_empty());
        fake.finish(org);
    }

    fn finished_archive(pump: &Pump) -> uuid::Uuid {
        let computer = ComputerId::local();
        let target = zerus_client_core::Target {
            computer: String::new(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
        };
        let row = ViewRow {
            machine: "workstation".into(),
            name: target.name.clone(),
            run_id: Some(target.run_id.clone()),
            created: 7,
            state: "running".into(),
            ..Default::default()
        };
        pump.push(Change::Computer {
            computer: computer.clone(),
            status: zerus_client_core::ComputerStatus::Live,
        });
        pump.push(Change::Upsert {
            computer: computer.clone(),
            key: "run:run-1".into(),
            row: row.clone(),
        });
        pump.termination_succeeded(pump.capture_termination(&target).unwrap());
        let ids = pump.archive_termination_ids(&computer);
        let archived = ViewRow {
            state: "archived".into(),
            archive_id: Some("00000000-0000-4000-8000-000000000001".into()),
            ..row
        };
        pump.observe_archive_terminations(&computer, &ids, &[archived]);
        pump.push(Change::Remove {
            computer,
            key: "run:run-1".into(),
            reason: Some("archived".into()),
        });
        ids[0]
    }

    #[test]
    fn a_verified_archive_placement_reaches_one_real_controller_apply() {
        let mut reply = full("d2");
        reply["organization"]["projects"][0]["sessions"]
            .as_array_mut()
            .unwrap()
            .push(json!(
                "workstation\narchive\n00000000-0000-4000-8000-000000000001"
            ));
        let fake = Fake::new(json!([{"stdout":reply}]));
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        let writer = organization.writer();
        let mut initial = full("d1");
        initial["organization"]["projects"][1]["sessions"] =
            json!(["workstation\narchive\n00000000-0000-4000-8000-000000000001"]);
        let base = initial["organization"].clone();
        organization.set_source(Arc::new(move || {
            Some(OrganizationSnapshot {
                node_id: "node-1".into(),
                swarm_id: String::new(),
                digest: "d1".into(),
                base: base.clone(),
                versions: json!({}),
            })
        }));
        let membership = Arc::new(zerus_client_core::swarm::members(&initial["organization"]));
        initial.as_object_mut().unwrap().remove("versions");
        organization.observe(&Change::Organization {
            computer: ComputerId::local(),
            update: OrganizationUpdate::State {
                value: Arc::new(initial),
                digest: "d1".into(),
                membership,
            },
        });
        finished_archive(&pump);
        pump.push(Change::Machine {
            computer: ComputerId::local(),
            machine: zerus_client_core::MachineMeta {
                host: Some("workstation".into()),
                ..Default::default()
            },
        });
        let mut page = [ViewRow {
            state: "archived".into(),
            archive_id: Some("00000000-0000-4000-8000-000000000001".into()),
            run_id: Some("run-1".into()),
            ..Default::default()
        }];
        let snapshot = pump
            .place_archived(&ComputerId::local(), 37, 0, &mut page)
            .unwrap();
        assert_eq!(
            snapshot[0].group, "ungrouped",
            "explicit archive membership precedes remembered live placement"
        );
        pump.take(Duration::ZERO);
        drop(writer);
        organization.0.wake.notify_all();
        wait_until("the archive placement apply", || fake.settled_after(1));
        let delta = pump.take(Duration::ZERO).groups;
        let archived = delta
            .iter()
            .find(|place| place.key.starts_with("archive:"))
            .unwrap();
        assert_eq!(
            (archived.group.as_str(), archived.archive_request),
            ("alpha", Some(37))
        );
        assert_eq!(snapshot[0].group, "ungrouped");
        assert_eq!(fake.applies().len(), 1);
        let sent = input(&fake.applies()[0]);
        let members = zerus_client_core::swarm::members(&sent["desired"]);
        assert_eq!(
            members
                .get("workstation\narchive\n00000000-0000-4000-8000-000000000001")
                .map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            pump.termination_count(),
            0,
            "the edit was checkpointed before its apply"
        );
        assert!(pump.archive_moves().is_empty());
        assert_eq!(
            organization
                .lock()
                .sync
                .presented()
                .unwrap()
                .1
                .get("workstation\narchive\n00000000-0000-4000-8000-000000000001")
                .map(String::as_str),
            Some("alpha")
        );
        fake.finish(organization);
    }

    #[test]
    fn an_archive_placement_waits_for_readiness_and_a_durable_pending_draft() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        let mut writer = organization.writer();
        finished_archive(&pump);
        {
            let mut hub = organization.lock();
            organization.archive_moves(&mut hub);
            assert!(
                pump.submitted_termination_ids().is_empty(),
                "not-ready placement is held"
            );
        }
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        std::fs::create_dir(fake.draft()).unwrap();
        let mut hub = organization.lock();
        organization.archive_moves(&mut hub);
        assert_eq!(pump.submitted_termination_ids().len(), 1);
        assert!(
            pump.archive_moves().is_empty(),
            "submitted once while its draft is pending"
        );
        let mut hub = organization.save_draft(hub, &mut writer);
        assert_eq!(
            pump.submitted_termination_ids().len(),
            1,
            "a failed checkpoint retains the acknowledgement"
        );
        assert_eq!(pump.termination_count(), 1);
        organization.archive_moves(&mut hub);
        assert_eq!(
            pump.submitted_termination_ids().len(),
            1,
            "the failed save never resubmits the edit"
        );
        std::fs::remove_dir(fake.draft()).unwrap();
        let hub = organization.save_draft(hub, &mut writer);
        assert!(pump.submitted_termination_ids().is_empty());
        assert_eq!(pump.termination_count(), 0);
        drop(hub);
        drop(writer);
        fake.finish(organization);
    }

    #[test]
    fn archive_placement_never_edits_a_blocked_replay_or_stopped_hub() {
        for allowed in [false, true] {
            let fake = Fake::landing();
            let pump = Pump::default();
            let organization = fake.organization(&pump, allowed);
            let writer = organization.writer();
            organization.set_source(snapshot("d1"));
            organization.observe(&state("d1"));
            finished_archive(&pump);
            let mut hub = organization.lock();
            assert!(hub.sync.ready());
            hub.stopped = allowed;
            assert_eq!(
                pump.archive_moves().len(),
                1,
                "the placement is otherwise ready"
            );
            organization.archive_moves(&mut hub);
            assert!(!hub.sync.pending());
            assert!(pump.submitted_termination_ids().is_empty());
            assert!(fake.applies().is_empty());
            drop(hub);
            drop(writer);
            fake.finish(organization);
        }
    }

    fn row(key: &str, name: &str) -> Change {
        Change::Upsert {
            computer: ComputerId::local(),
            key: key.into(),
            row: ViewRow {
                machine: "workstation".into(),
                name: name.into(),
                ..ViewRow::default()
            },
        }
    }

    fn move_to_inbox() -> Intent {
        Intent::Move {
            members: vec![R.into()],
            project: "ungrouped".into(),
            before: None,
        }
    }

    #[test]
    fn creation_placement_returns_while_swarm_apply_is_held() {
        let fake = Fake::new(json!([{"stdout": full("d2"), "gate": "go"}]));
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let accepting = organization.clone();
        let (send, answer) = sync_channel(1);
        let edit = std::thread::spawn(move || {
            let _ = send.send(accepting.accept(|_| move_to_inbox()));
        });
        let accepted = answer.recv_timeout(Duration::from_secs(2));
        wait_until("the held apply", || !fake.applies().is_empty());
        let no_waiter = organization.lock().waiting.is_empty();
        std::fs::write(fake.dir.join("go"), b"").unwrap();
        edit.join().unwrap();
        assert!(no_waiter, "accept registers no apply waiter");
        assert_eq!(accepted, Ok(Ok(())), "placement waited for swarm apply");
        fake.finish(organization);
    }

    #[test]
    fn creation_placement_refuses_not_ready_replay_and_stopped_with_their_details() {
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), true);
        let detail = organization
            .lock()
            .sync
            .edit(&move_to_inbox(), organization.now())
            .unwrap_err();
        assert_eq!(organization.accept(|_| move_to_inbox()), Err(detail));
        organization.lock().stopped = true;
        assert_eq!(
            organization.accept(|_| move_to_inbox()),
            Err(STOPPED.into())
        );
        fake.finish(organization);
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), false);
        assert_eq!(
            organization.accept(|_| move_to_inbox()),
            Err(REPLAY_NEEDS_HGS.into())
        );
        assert!(fake.applies().is_empty());
        fake.finish(organization);
    }

    #[test]
    fn creation_placement_reads_the_accepted_project_and_folder_before_apply() {
        let fake = Fake::new(json!([{"stdout": full("d2"), "gate": "go"}]));
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        organization
            .accept(|_| Intent::Create {
                id: "reserved".into(),
                name: "Repository".into(),
                members: Vec::new(),
            })
            .unwrap();
        for _ in 0..2 {
            organization
                .accept(|_| Intent::AddFolder {
                    id: "folder-id".into(),
                    project: "reserved".into(),
                    machine: "workstation".into(),
                    path: "/srv/repo".into(),
                    name: String::new(),
                    style: zerus_client_core::paths::Style::Posix,
                })
                .unwrap();
        }
        let projects = organization.creation_projects("workstation");
        let created = projects
            .iter()
            .find(|project| project.id == "reserved")
            .unwrap();
        assert_eq!(created.name, "Repository");
        assert_eq!(
            created.folders.len(),
            1,
            "Continue setup adds the same folder idempotently"
        );
        assert_eq!(created.folders[0].machine, "");
        assert_eq!(created.folders[0].path, "/srv/repo");
        std::fs::write(fake.dir.join("go"), b"").unwrap();
        fake.finish(organization);
    }

    #[test]
    fn a_blocked_replay_refuses_edits_with_the_replay_text() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, false);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let outcome = organization.submit(move_to_inbox());
        assert_eq!(
            outcome,
            EditOutcome::Refused {
                detail: REPLAY_NEEDS_HGS.into()
            }
        );
        let grouping = pump.take(crate::pump::PARK).grouping.unwrap();
        assert!(grouping.loaded && !grouping.sync.ready);
        assert_eq!(grouping.sync.reason.as_deref(), Some(REPLAY_NEEDS_HGS));
        assert!(fake.applies().is_empty());
        fake.finish(organization);
    }

    // A replay without ZERUS_CLIENT_HGS would run the real CLI: it sends no draft.
    #[test]
    fn a_blocked_replay_never_sends_a_saved_draft() {
        let fake = Fake::landing();
        let base = full("d1")["organization"].clone();
        let mut desired = base.clone();
        desired["projects"][0]["sessions"] = json!([]);
        let saved = Draft::new(base, desired, serde_json::Map::new()).to_json();
        draft::write(&fake.draft(), &saved).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, false);
        assert!(
            !pump
                .take(crate::pump::PARK)
                .grouping
                .is_some_and(|g| g.loaded),
            "the draft is not taken up, so the list shows none"
        );
        organization.set_source(snapshot("d1"));
        let passes = parked(&organization);
        organization.observe(&state("d1"));
        wait_until("a pass after the first state", || {
            parked(&organization) > passes
        });
        assert!(fake.applies().is_empty());
        assert_eq!(
            std::fs::read(fake.draft()).unwrap(),
            saved,
            "left as it was"
        );
        fake.finish(organization);
    }

    #[test]
    fn a_state_of_another_digest_than_the_snapshot_is_skipped() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d2"));
        organization.observe(&state("d1"));
        assert!(
            !pump
                .take(crate::pump::PARK)
                .grouping
                .is_some_and(|g| g.loaded),
            "versions of another state are never paired"
        );
        organization.observe(&state("d2"));
        assert!(pump.take(crate::pump::PARK).grouping.unwrap().loaded);
        fake.finish(organization);
    }

    #[test]
    fn an_edit_runs_one_apply_and_answers_with_its_outcome() {
        let fake = Fake::landing();
        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let outcome = organization.submit(move_to_inbox());
        assert_eq!(outcome, EditOutcome::Done { conflicts: 0 });
        assert_eq!(fake.applies().len(), 1, "nothing is pending once it landed");
        wait_until("the layout to be saved", || fake.layout().exists());
        wait_until("the draft to go", || fake.settled_after(1));
        assert_eq!(fake.applies().len(), 1);
        fake.finish(organization);
    }

    // SwarmController.cpp:67-73, 96-101: the edit stays, and the topic's next
    // state sends it again. Projects shows the failure until the resend lands,
    // as Qt's statusChanged reaches it (SessionsWindow.cpp:1653).
    #[test]
    fn a_failed_apply_answers_kept_and_the_next_state_sends_it_once_more() {
        const FAILED: &str =
            "Project sync unavailable: hgs: session belongs to more than one project";
        let refused =
            json!({"exit": 1, "stderr": "hgs: session belongs to more than one project\n"});
        let fake = Fake::new(json!([refused, {"stdout": full("d2")}]));
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let lines: Arc<Mutex<Vec<(String, bool)>>> = Arc::default();
        {
            let lines = Arc::clone(&lines);
            organization.set_presented(Arc::new(move |_, _, status| {
                lines.lock().unwrap().push(status);
            }));
        }
        let last = || lines.lock().unwrap().last().cloned();
        let outcome = organization.submit(move_to_inbox());
        assert_eq!(
            outcome,
            EditOutcome::Kept {
                detail: FAILED.into()
            }
        );
        // The answer goes out just before the Projects call, both after the lock.
        wait_until("Projects to get the failure", || {
            last() == Some((FAILED.into(), true))
        });
        assert_eq!(fake.applies().len(), 1);
        assert!(fake.draft().exists(), "the kept edit stays on disk");
        let before = passes(&organization);
        organization.observe(&state("d1"));
        // The 5 s tick would take five idle passes; the state sends it at once.
        wait_until("the resend, or three more passes", || {
            fake.applies().len() >= 2 || passes(&organization) > before + 2
        });
        assert!(
            fake.applies().len() >= 2,
            "the state sent it again within two passes"
        );
        wait_until("the resend to land", || fake.settled_after(2));
        // The worker made the call before the pass that removed the draft.
        assert_eq!(
            last(),
            Some((
                "Local catalog. Connect another computer to sync projects.".into(),
                false
            )),
            "the landed catalog's line replaces the failure"
        );
        let log = fake.applies();
        assert_eq!(log.len(), 2, "one resend, and no more once it landed");
        assert_eq!(
            input(&log[0]),
            input(&log[1]),
            "the same base, desired and versions"
        );
        fake.finish(organization);
    }

    // Only a write that succeeded is confirmed (`Controller::draft_written`);
    // a failed one is offered again on a later pass, and the apply goes anyway.
    #[test]
    fn a_draft_that_could_not_be_written_is_written_on_a_later_pass() {
        let refused = json!({"exit": 1, "stderr": "hgs: the catalog is busy\n"});
        let fake = Fake::new(json!([refused]));
        let organization = fake.organization(&Pump::default(), true);
        // A folder in the draft's place once the hub started: every write fails.
        std::fs::create_dir(fake.draft()).unwrap();
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let outcome = organization.submit(move_to_inbox());
        assert_eq!(
            outcome,
            EditOutcome::Kept {
                detail: "Project sync unavailable: hgs: the catalog is busy".into()
            }
        );
        assert!(fake.draft().is_dir(), "nothing was written");
        std::fs::remove_dir(fake.draft()).unwrap();
        wait_until("the draft to be written", || fake.draft().is_file());
        let Load::Loaded(saved) = draft::load(&fake.draft()) else {
            panic!("not a draft");
        };
        assert_eq!(saved.desired["projects"][1]["sessions"], json!([R]));
        fake.finish(organization);
    }

    // Qt loads its draft at start (SessionsWindow.cpp:275-276, 1661) and sends it
    // once its first read answered (SwarmController.cpp:76-89, 197, 203-205).
    #[test]
    fn a_saved_draft_waits_for_the_first_state_then_one_apply_sends_it() {
        let fake = Fake::landing();
        let base = full("d1")["organization"].clone();
        let mut desired = base.clone();
        desired["projects"][0]["sessions"] = json!([]);
        desired["projects"][1]["sessions"] = json!([R]);
        let versions = serde_json::Map::new();
        draft::write(
            &fake.draft(),
            &Draft::new(base.clone(), desired.clone(), versions.clone()).to_json(),
        )
        .unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        assert!(
            pump.take(crate::pump::PARK)
                .grouping
                .is_some_and(|g| g.loaded),
            "the list shows the draft before any state"
        );
        wait_until("a pass of the worker", || parked(&organization) >= 1);
        assert!(fake.applies().is_empty(), "nothing before the first state");
        assert!(fake.draft().exists());
        organization.observe(&state("d1"));
        wait_until("the draft to land", || fake.settled_after(1));
        let log = fake.applies();
        assert_eq!(log.len(), 1, "exactly one apply");
        assert_eq!(
            input(&log[0]),
            json!({"base": shared(&base), "desired": shared(&desired), "versions": versions}),
            "the saved draft"
        );
        fake.finish(organization);
    }

    #[test]
    fn an_unusable_draft_is_moved_aside_and_never_sent() {
        let fake = Fake::landing();
        std::fs::write(fake.draft(), br#"{"version": 1, "base": {"projects": ["#).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        assert!(!fake.draft().exists(), "moved before the worker starts");
        let notice = pump.take(crate::pump::PARK).grouping.unwrap().sync.notice;
        assert!(
            notice.as_deref().is_some_and(|notice| notice.starts_with(
                "The saved project changes were not applied: the draft file is not a draft: "
            )),
            "the list says why: {notice:?}"
        );
        let aside: Vec<String> = std::fs::read_dir(&fake.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("draft.json.") && name.ends_with(".bad"))
            .collect();
        assert_eq!(aside.len(), 1, "kept beside it: {aside:?}");
        organization.set_source(snapshot("d1"));
        let passes = parked(&organization);
        organization.observe(&state("d1"));
        let grouping = pump.take(crate::pump::PARK).grouping.unwrap();
        assert!(grouping.loaded);
        assert_eq!(grouping.sync.notice, None, "gone with the first state");
        wait_until("a pass after the first state", || {
            parked(&organization) > passes
        });
        assert!(fake.applies().is_empty());
        assert!(!fake.draft().exists());
        fake.finish(organization);
    }

    // A layout kept from a failed write is older than one
    // taken later, and never overwrites it.
    #[test]
    fn a_layout_that_could_not_be_saved_never_overwrites_a_newer_one() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        // A folder in the layout's place: saves fail and keep their bytes.
        std::fs::create_dir(fake.layout()).unwrap();
        pump.set_collapsed("alpha", true);
        let first = saves(&organization);
        wait_until("a failed save", || saves(&organization) > first);
        assert!(fake.layout().is_dir());
        {
            // Between two saves, the folder goes and a newer layout comes.
            let _writer = organization.writer();
            std::fs::remove_dir(fake.layout()).unwrap();
            pump.set_collapsed("alpha", false);
            pump.set_collapsed("ungrouped", true);
        }
        wait_until("the newer layout", || fake.layout().is_file());
        let newer = std::fs::read(fake.layout()).unwrap();
        assert!(String::from_utf8_lossy(&newer).contains("ungrouped"));
        let after = saves(&organization);
        wait_until("one more save", || saves(&organization) > after);
        assert_eq!(
            std::fs::read(fake.layout()).unwrap(),
            newer,
            "the failed layout never comes back"
        );
        fake.finish(organization);
    }

    // Quitting writes what the worker could not yet. An
    // edit made while an apply runs is on disk after `flush`, which runs no hgs.
    #[test]
    fn flush_writes_an_edit_made_while_an_apply_runs() {
        let fake = Fake::new(json!([{"stdout": full("d2"), "gate": "go"}]));
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let first = {
            let organization = organization.clone();
            std::thread::spawn(move || organization.submit(move_to_inbox()))
        };
        wait_until("the apply to start", || fake.applies().len() == 1);
        let second = {
            let organization = organization.clone();
            std::thread::spawn(move || {
                organization.submit(Intent::Rename {
                    project: "alpha".into(),
                    name: "Beta".into(),
                })
            })
        };
        wait_until("the rename in the list", || {
            header_named(&pump, "alpha", "Beta")
        });
        let Load::Loaded(before) = draft::load(&fake.draft()) else {
            panic!("no draft before the apply");
        };
        assert_eq!(before.desired["projects"][0]["name"], "Alpha");
        organization.flush();
        let Load::Loaded(saved) = draft::load(&fake.draft()) else {
            panic!("no draft after flush");
        };
        assert_eq!(saved.desired["projects"][0]["name"], "Beta");
        assert_eq!(saved.desired["projects"][1]["sessions"], json!([R]));
        assert_eq!(fake.applies().len(), 1, "flush runs no hgs");
        std::fs::write(fake.dir.join("go"), b"").unwrap();
        assert_eq!(first.join().unwrap(), EditOutcome::Done { conflicts: 0 });
        assert_eq!(second.join().unwrap(), EditOutcome::Done { conflicts: 0 });
        fake.finish(organization);
    }

    // A worker that stopped shows in the list before
    // anyone tries an edit, and edits answer Kept.
    #[test]
    fn a_stopped_worker_shows_in_the_list_before_any_edit() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        assert!(pump.take(crate::pump::PARK).grouping.unwrap().sync.ready);
        organization.0.fail.store(true, Ordering::SeqCst);
        organization.0.wake.notify_all();
        wait_until("the list to show it", || {
            pump.take(Duration::ZERO).grouping.is_some_and(|grouping| {
                !grouping.sync.ready && grouping.sync.reason.as_deref() == Some(STOPPED)
            })
        });
        assert_eq!(
            organization.submit(move_to_inbox()),
            EditOutcome::Kept {
                detail: STOPPED.into()
            }
        );
        assert!(fake.applies().is_empty());
        fake.finish(organization);
    }

    // A panic in the Projects callback is logged, and the
    // worker goes on to the next edit.
    #[test]
    fn a_panicking_projects_callback_leaves_the_worker_running() {
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        organization.set_presented(Arc::new(|_, _, _| {
            if std::thread::current().name() == Some("zerus-organization") {
                panic!("the Projects callback panics on the worker");
            }
        }));
        assert_eq!(
            organization.submit(move_to_inbox()),
            EditOutcome::Done { conflicts: 0 }
        );
        let rename = Intent::Rename {
            project: "alpha".into(),
            name: "Beta".into(),
        };
        assert_eq!(
            organization.submit(rename),
            EditOutcome::Done { conflicts: 0 },
            "the worker still runs edits"
        );
        fake.finish(organization);
    }

    /// Makes `intent` in the controller without a publish, so the worker's
    /// next pass that acts on it makes the Projects call: at the start of the
    /// apply, or in a pass that settles it. That call blocks, on the worker
    /// only, until the test lets it go; meanwhile `flush` must finish.
    /// Answers the applies run once the edit settled.
    fn flush_beside_a_slow_projects_call(intent: Intent) -> usize {
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let (entered, on_entry) = sync_channel::<()>(1);
        let (go, gate) = sync_channel::<()>(1);
        let gate = Mutex::new(gate);
        organization.set_presented(Arc::new(move |_, _, _| {
            if std::thread::current().name() == Some("zerus-organization") {
                let _ = entered.try_send(());
                // Once `go` is dropped, later calls pass at once.
                let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(10));
            }
        }));
        {
            let mut hub = organization.lock();
            let now = organization.now();
            hub.sync.edit(&intent, now).unwrap();
        }
        organization.0.wake.notify_all();
        on_entry
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker's Projects call");
        let (flushed, on_flush) = sync_channel(1);
        {
            let organization = organization.clone();
            std::thread::spawn(move || {
                organization.flush();
                let _ = flushed.send(());
            });
        }
        let quit = on_flush.recv_timeout(Duration::from_secs(5));
        drop(go);
        assert!(
            quit.is_ok(),
            "flush waited behind the Projects call on the worker"
        );
        wait_until("the edit to settle", || !fake.draft().exists());
        let applies = fake.applies().len();
        fake.finish(organization);
        applies
    }

    // The main thread runs `flush` as the app
    // quits, and it never waits behind the Projects call at the start of an
    // apply, which runs after the writer's lock is dropped.
    #[test]
    fn flush_never_waits_behind_the_projects_call_at_the_start_of_an_apply() {
        assert_eq!(flush_beside_a_slow_projects_call(move_to_inbox()), 1);
    }

    // The same for a pass that settles an edit without an apply.
    #[test]
    fn flush_never_waits_behind_the_projects_call_of_a_settled_pass() {
        let same = Intent::Rename {
            project: "alpha".into(),
            name: "Alpha".into(),
        };
        assert_eq!(
            flush_beside_a_slow_projects_call(same),
            0,
            "nothing shared changed"
        );
    }

    // The grouping keeps a rename it saw before the first state;
    // the worker hands it over only once edits are accepted (sync.rs).
    #[test]
    fn a_rename_seen_before_the_first_state_is_sent_once_edits_are_accepted() {
        let fake = Fake::landing();
        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review"));
        pump.push(row("run:1", "claude/sample-project/review-2"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        wait_until("a pass before the first state", || {
            parked(&organization) >= 1
        });
        assert!(fake.applies().is_empty());
        organization.observe(&state("d1"));
        wait_until("the rename to land", || fake.settled_after(1));
        let log = fake.applies();
        assert_eq!(log.len(), 1);
        let sent = input(&log[0]);
        assert_eq!(sent["base"]["projects"][0]["sessions"], json!([R]));
        assert_eq!(sent["desired"]["projects"][0]["sessions"], json!([R2]));
        fake.finish(organization);
    }

    /// Qt's persisted m_runs discovers a rename even without an old-name
    /// upsert this time. Rows may arrive before the background file load.
    #[test]
    fn a_saved_run_migrates_a_rename_made_while_closed_with_one_apply() {
        for rows_first in [false, true] {
            let fake = Fake::landing();
            let previous = Pump::default();
            previous.push(row("run:1", "claude/sample-project/review"));
            draft::write(&fake.runs(), &previous.take_runs().unwrap().to_json()).unwrap();
            let pump = Pump::default();
            if rows_first {
                pump.push(row("run:1", "claude/sample-project/review-2"));
            }
            let organization = fake.organization(&pump, true);
            organization.set_source(snapshot("d1"));
            organization.observe(&state("d1"));
            if !rows_first {
                pump.push(row("run:1", "claude/sample-project/review-2"));
            }
            wait_until("the persisted rename to land", || fake.settled_after(1));
            let log = fake.applies();
            assert_eq!(log.len(), 1);
            assert_eq!(
                input(&log[0])["base"]["projects"][0]["sessions"],
                json!([R])
            );
            assert_eq!(
                input(&log[0])["desired"]["projects"][0]["sessions"],
                json!([R2])
            );
            fake.finish(organization);
        }
    }

    #[test]
    fn a_rename_seen_before_readiness_is_recovered_after_a_second_start() {
        let fake = Fake::landing();
        let previous = Pump::default();
        previous.push(row("run:1", "claude/sample-project/review"));
        draft::write(&fake.runs(), &previous.take_runs().unwrap().to_json()).unwrap();
        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        let organization = fake.organization(&pump, true);
        wait_until("startup without a usable catalog", || {
            parked(&organization) >= 1
        });
        assert!(!organization.lock().sync.ready());
        flush_with_deadline(&organization);
        assert!(fake.applies().is_empty());
        let weak = Arc::downgrade(&organization.0);
        drop(organization);
        wait_until("the first worker to end", || weak.strong_count() == 0);

        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        wait_until("the rename recovered on the next start", || {
            fake.settled_after(1)
        });
        assert_eq!(fake.applies().len(), 1);
        assert_eq!(
            input(&fake.applies()[0])["desired"]["projects"][0]["sessions"],
            json!([R2])
        );
        fake.finish(organization);
    }

    /// A blocked exit must fail an assertion instead of hanging the suite.
    fn flush_with_deadline(organization: &Organization) {
        let organization = organization.clone();
        let (done, finished) = sync_channel(1);
        std::thread::spawn(move || {
            organization.flush();
            let _ = done.send(());
        });
        finished
            .recv_timeout(Duration::from_secs(2))
            .expect("exit flush exceeded its deadline");
    }

    #[test]
    fn the_token_checkpoint_cannot_overtake_a_failed_rename_draft() {
        let fake = Fake::landing();
        let previous = Pump::default();
        previous.push(row("run:1", "claude/sample-project/review"));
        draft::write(&fake.runs(), &previous.take_runs().unwrap().to_json()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        wait_until("startup load", || {
            organization.runs_writer().state.lock().unwrap().loaded
        });
        // Stop the apply worker deterministically; exercise its rename/draft
        // path directly while the independent token writer remains live.
        organization.0.fail.store(true, Ordering::SeqCst);
        organization.0.wake.notify_all();
        wait_until("the apply worker to stop", || organization.lock().stopped);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        std::fs::create_dir(fake.draft()).unwrap();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        {
            let mut writer = organization.writer();
            let mut hub = organization.lock();
            organization.renames(&mut hub);
            assert_eq!(hub.run_acks.len(), 1);
            let hub = organization.save_draft(hub, &mut writer);
            assert_eq!(
                hub.run_acks.len(),
                1,
                "failed draft retains the acknowledgement"
            );
        }
        // Pin the crash gap before any later load could hide an early ack:
        // the token writer gets a full turn while the draft is still failed.
        organization.runs_writer().flush(RUNS_EXIT_WAIT);
        let mut saved = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(saved.observe("workstation", "1", "probe"), Some(R.into()));
        // A load retry can reobserve the same rename behind the unsaved
        // desired catalog. Its local absence is not proof of a durable move.
        pump.load_runs(&std::fs::read(fake.runs()).unwrap())
            .unwrap();
        {
            let mut hub = organization.lock();
            organization.renames(&mut hub);
            assert!(hub.run_acks.len() >= 2);
        }
        flush_with_deadline(&organization);
        let mut saved = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(saved.observe("workstation", "1", "probe"), Some(R.into()));
        let runs = organization.runs_writer();
        let weak = Arc::downgrade(&organization.0);
        drop(organization);
        wait_until("the first hub to go", || weak.strong_count() == 0);
        runs.stop();
        std::fs::remove_dir(fake.draft()).unwrap();

        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        wait_until("the rename after the failed save and restart", || {
            fake.settled_after(1)
        });
        assert_eq!(fake.applies().len(), 1);
        fake.finish(organization);
    }

    #[test]
    fn a_successful_rename_draft_releases_its_token_checkpoint() {
        let fake = Fake::landing();
        let previous = Pump::default();
        previous.push(row("run:1", "claude/sample-project/review"));
        draft::write(&fake.runs(), &previous.take_runs().unwrap().to_json()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        wait_until("startup load", || {
            organization.runs_writer().state.lock().unwrap().loaded
        });
        organization.0.fail.store(true, Ordering::SeqCst);
        organization.0.wake.notify_all();
        wait_until("the apply worker to stop", || organization.lock().stopped);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        pump.push(row("run:1", "claude/sample-project/review-2"));
        {
            let mut writer = organization.writer();
            let mut hub = organization.lock();
            organization.renames(&mut hub);
            assert_eq!(hub.run_acks.len(), 1);
            let hub = organization.save_draft(hub, &mut writer);
            assert!(hub.run_acks.is_empty());
        }
        let Load::Loaded(saved) = draft::load(&fake.draft()) else {
            panic!("the rename draft was not saved")
        };
        assert_eq!(saved.desired["projects"][0]["sessions"], json!([R2]));
        // Reobserve behind an already durable pending draft. No second file
        // write is needed before this candidate's token is safe to commit.
        pump.load_runs(&std::fs::read(fake.runs()).unwrap())
            .unwrap();
        {
            let mut writer = organization.writer();
            let mut hub = organization.lock();
            organization.renames(&mut hub);
            assert_eq!(hub.run_acks.len(), 1);
            assert!(hub.sync.take_draft().is_none());
            let hub = organization.save_draft(hub, &mut writer);
            assert!(hub.run_acks.is_empty());
        }
        organization.runs_writer().flush(RUNS_EXIT_WAIT);
        let mut saved = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(saved.observe("workstation", "1", "probe"), Some(R2.into()));
        fake.finish(organization);
    }

    #[test]
    fn a_persisted_rename_already_migrated_by_the_other_writer_needs_no_apply() {
        let fake = Fake::landing();
        let previous = Pump::default();
        previous.push(row("run:1", "claude/sample-project/review"));
        draft::write(&fake.runs(), &previous.take_runs().unwrap().to_json()).unwrap();
        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d2"));
        let mut migrated = state("d2");
        let Change::Organization {
            update: OrganizationUpdate::State {
                value, membership, ..
            },
            ..
        } = &mut migrated
        else {
            unreachable!()
        };
        Arc::make_mut(value)["organization"]["projects"][0]["sessions"] = json!([R2]);
        *membership = Arc::new(zerus_client_core::swarm::members(&value["organization"]));
        organization.observe(&migrated);
        let first = parked(&organization);
        organization.0.wake.notify_all();
        wait_until("startup token processing", || {
            parked(&organization) > first + 1
        });
        assert!(fake.applies().is_empty());
        assert!(!fake.draft().exists());
        fake.finish(organization);
    }

    #[test]
    fn a_layout_only_change_does_not_rewrite_the_runs_file() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        pump.push(row("run:1", "claude/sample-project/review"));
        wait_until("the first run save", || fake.runs().is_file());
        let before = std::fs::read(fake.runs()).unwrap();
        let modified = std::fs::metadata(fake.runs()).unwrap().modified().unwrap();
        pump.set_collapsed("alpha", true);
        let first = saves(&organization);
        wait_until("a layout-only save", || saves(&organization) > first);
        assert_eq!(std::fs::read(fake.runs()).unwrap(), before);
        assert_eq!(
            std::fs::metadata(fake.runs()).unwrap().modified().unwrap(),
            modified
        );
        fake.finish(organization);
    }

    #[test]
    fn exit_checkpoints_a_run_observed_inside_the_debounce_off_main() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        pump.push(row("run:exit", "claude/sample-project/review"));
        flush_with_deadline(&organization);
        assert!(fake.runs().is_file());
        let runs = organization.runs_writer();
        assert_ne!(
            runs.state.lock().unwrap().thread,
            Some(std::thread::current().id())
        );
        let mut saved = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(
            saved.observe("workstation", "exit", "renamed"),
            Some(R.into())
        );
        fake.finish(organization);
    }

    #[test]
    fn an_immediate_exit_loads_old_tokens_before_writing_new_ones() {
        let fake = Fake::landing();
        let old = Pump::default();
        old.push(row("run:previous", "claude/sample-project/previous"));
        draft::write(&fake.runs(), &old.take_runs().unwrap().to_json()).unwrap();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        pump.push(row("run:new", "claude/sample-project/new"));
        flush_with_deadline(&organization);
        let mut saved = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(
            saved.observe("workstation", "previous", "next"),
            Some("workstation\nclaude/sample-project/previous".into())
        );
        assert_eq!(
            saved.observe("workstation", "new", "next"),
            Some("workstation\nclaude/sample-project/new".into())
        );
        fake.finish(organization);
    }

    #[test]
    fn a_stuck_runs_write_bounds_exit_and_does_not_block_the_draft_or_layout() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        wait_until("startup", || parked(&organization) >= 1);
        let (entered, writing) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let runs = organization.runs_writer();
        {
            let mut writer = runs.state.lock().unwrap();
            writer.stuck = Some((entered, blocked));
            writer.saved = Instant::now() - SAVE_EVERY;
        }
        pump.push(row("run:stuck", "claude/sample-project/review"));
        organization.0.wake.notify_all();
        writing.recv_timeout(Duration::from_secs(5)).unwrap();
        // The normal background run save is now held in I/O. Its independent
        // lock leaves both the shared writer and the pump available.
        wait_until(
            "shared writer availability during token serialization",
            || organization.0.writer.try_lock().is_ok(),
        );
        let answer = submitted(&organization, move_to_inbox());
        assert!(matches!(
            answer.recv_timeout(Duration::from_secs(5)),
            Ok(EditOutcome::Done { .. })
        ));
        let normal = saves(&organization);
        pump.set_collapsed("alpha", true);
        wait_until("normal layout progress during token serialization", || {
            saves(&organization) > normal
        });
        {
            let mut hub = organization.lock();
            hub.sync.edit(&move_to_inbox(), organization.now()).unwrap();
        }
        pump.set_collapsed("alpha", true);
        let before = saves(&organization);
        let started = Instant::now();
        flush_with_deadline(&organization);
        assert!(started.elapsed() >= RUNS_EXIT_WAIT);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            saves(&organization) > before,
            "layout save ran concurrently with runs I/O"
        );
        assert!(fake.layout().is_file());
        assert!(
            fake.draft().is_file(),
            "pending edits were flushed despite the stuck token write"
        );
        release.send(()).unwrap();
        // The detached exit checkpoint also finishes before the fake is removed.
        wait_until("the held run write", || fake.runs().is_file());
        drop(runs.state.lock().unwrap());
        fake.finish(organization);
    }

    #[test]
    fn replay_run_tokens_never_touch_the_live_file() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let organization = Organization::new(
            pump.clone(),
            fake.hgs.clone(),
            false,
            fake.dir.join("custom-layout.json"),
            fake.dir.join(draft_file(true)),
            true,
        );
        pump.push(row("run:replay", "claude/sample-project/review"));
        flush_with_deadline(&organization);
        assert!(fake.dir.join(runs::REPLAY_FILE).is_file());
        assert!(!fake.runs().exists());
        fake.finish(organization);
    }

    #[test]
    fn pending_token_acknowledgements_are_bounded_oldest_first() {
        let mut hub = Hub::default();
        for n in 0..=runs::MAX_RUNS {
            hub.acknowledge_after_draft(n.to_string(), format!("new-{n}"));
        }
        assert_eq!(hub.run_acks.len(), runs::MAX_RUNS);
        assert_eq!(hub.run_acks.front().unwrap().0, "1");
        assert_eq!(hub.run_acks.back().unwrap().0, runs::MAX_RUNS.to_string());
    }

    #[test]
    fn token_writes_debounce_for_one_second() {
        assert_eq!(SAVE_EVERY, Duration::from_secs(1));
        let fake = Fake::landing();
        let pump = Pump::default();
        let mut writer = RunsWriter::default();
        pump.push(row("run:1", "claude/sample-project/review"));
        writer.saved = Instant::now() + Duration::from_secs(60);
        writer.save(&pump, &fake.runs(), false);
        assert!(!fake.runs().exists(), "early ticks do not write");
        writer.saved = Instant::now() - SAVE_EVERY;
        writer.save(&pump, &fake.runs(), false);
        assert!(fake.runs().is_file());
    }

    #[test]
    fn an_io_error_leaves_the_history_and_retries_loading() {
        let fake = Fake::landing();
        std::fs::create_dir(fake.runs()).unwrap();
        let pump = Pump::default();
        let mut writer = RunsWriter::default();
        pump.push(row("run:new", "claude/sample-project/new"));
        for _ in 0..2 {
            writer.save(&pump, &fake.runs(), true);
            assert!(!writer.loaded && !writer.read_only);
            assert!(fake.runs().is_dir());
        }
        std::fs::remove_dir(fake.runs()).unwrap();
        pump.push(Change::Remove {
            computer: ComputerId::local(),
            key: "run:new".into(),
            reason: None,
        });
        let old = Pump::default();
        old.push(row("run:old", "claude/sample-project/old"));
        draft::write(&fake.runs(), &old.take_runs().unwrap().to_json()).unwrap();
        writer.save(&pump, &fake.runs(), true);
        assert!(writer.loaded);
        let json: Value = serde_json::from_slice(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(
            json["runs"].as_array().unwrap().len(),
            2,
            "old history plus observations whose rows left"
        );
    }

    #[test]
    fn malformed_supported_history_is_replaced_from_current_observations() {
        for bytes in [
            b"{broken".as_slice(),
            br#"{"version":1,"runs":{}}"#,
            br#"{"runs":false}"#,
            b"[]",
        ] {
            let fake = Fake::landing();
            std::fs::write(fake.runs(), bytes).unwrap();
            let pump = Pump::default();
            let mut writer = RunsWriter::default();
            pump.push(row("run:new", "claude/sample-project/new"));
            writer.save(&pump, &fake.runs(), true);
            assert!(writer.loaded && !writer.read_only);
            let json: Value = serde_json::from_slice(&std::fs::read(fake.runs()).unwrap()).unwrap();
            assert_eq!(
                json["runs"],
                json!([["workstation\nnew", "workstation\nclaude/sample-project/new"]])
            );
        }
    }

    #[test]
    fn oversize_history_is_replaced_without_truncate_parsing() {
        use std::io::Write;
        for version in [1, 2] {
            let fake = Fake::landing();
            let mut file = std::fs::File::create(fake.runs()).unwrap();
            write!(file, "{{\"version\":{version},\"runs\":[],\"unused\":\"").unwrap();
            let block = [b'x'; 4096];
            for _ in 0..=runs::MAX_BYTES / block.len() as u64 {
                file.write_all(&block).unwrap();
            }
            file.write_all(br#""}"#).unwrap();
            drop(file);
            assert!(matches!(
                read_runs(&fake.runs()).unwrap(),
                RunsFile::Oversize
            ));
            let pump = Pump::default();
            let mut writer = RunsWriter::default();
            pump.push(row("run:new", "claude/sample-project/new"));
            writer.save(&pump, &fake.runs(), true);
            assert!(writer.loaded && !writer.read_only);
            let json: Value = serde_json::from_slice(&std::fs::read(fake.runs()).unwrap()).unwrap();
            assert_eq!(json["runs"].as_array().unwrap().len(), 1);
            assert!(std::fs::metadata(fake.runs()).unwrap().len() < runs::MAX_BYTES);
        }
    }

    #[test]
    fn newer_history_is_untouched_across_saves_and_exit() {
        for bytes in [
            br#"{"version":2,"runs":[]}"#.as_slice(),
            br#"{"version":3,"runs":{"new":"shape"}}"#,
        ] {
            let fake = Fake::landing();
            std::fs::write(fake.runs(), bytes).unwrap();
            let pump = Pump::default();
            pump.push(row("run:new", "claude/sample-project/new"));
            let organization = fake.organization(&pump, true);
            wait_until("the newer version to become read-only", || {
                organization.runs_writer().state.lock().unwrap().read_only
            });
            for n in 0..3 {
                pump.push(row(&format!("run:{n}"), "claude/sample-project/new"));
                organization.runs_writer().request(false);
                flush_with_deadline(&organization);
                assert_eq!(std::fs::read(fake.runs()).unwrap(), bytes);
            }
            // A newer file makes this whole run read-only, even if another
            // writer replaces it with a supported version before a later quit.
            let replacement = br#"{"version":1,"runs":[]}"#;
            std::fs::write(fake.runs(), replacement).unwrap();
            flush_with_deadline(&organization);
            assert_eq!(std::fs::read(fake.runs()).unwrap(), replacement);
            assert!(pump.take_runs().is_some(), "observations stay in memory");
            fake.finish(organization);
        }
    }

    #[test]
    fn a_failed_runs_save_is_replaced_by_the_newer_tokens() {
        let fake = Fake::landing();
        let pump = Pump::default();
        let mut writer = RunsWriter {
            loaded: true,
            ..RunsWriter::default()
        };
        std::fs::create_dir(fake.runs()).unwrap();
        pump.push(row("run:1", "claude/sample-project/review"));
        writer.save(&pump, &fake.runs(), true);
        assert!(writer.pending.is_some());
        std::fs::remove_dir(fake.runs()).unwrap();
        pump.push(row("run:1", "claude/sample-project/review-2"));
        pump.acknowledge_rename(R, R2);
        writer.save(&pump, &fake.runs(), true);
        assert!(writer.pending.is_none());
        let mut runs = runs::Runs::from_json(&std::fs::read(fake.runs()).unwrap()).unwrap();
        assert_eq!(runs.observe("workstation", "1", "again"), Some(R2.into()));
    }

    /// Runs `submit` on a thread; its answer comes back on the receiver.
    fn submitted(
        organization: &Organization,
        intent: Intent,
    ) -> std::sync::mpsc::Receiver<EditOutcome> {
        let (sender, receiver) = sync_channel(1);
        let organization = organization.clone();
        std::thread::spawn(move || {
            let _ = sender.send(organization.submit(intent));
        });
        receiver
    }

    /// The same move again while its apply runs leaves the desired state as
    /// sent: that apply answers both, and the second `submit` returns.
    #[test]
    fn an_edit_repeated_while_its_apply_runs_is_answered_by_that_apply() {
        let fake = Fake::new(json!([{"stdout": full("d2"), "gate": "go"}]));
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let first = submitted(&organization, move_to_inbox());
        wait_until("the apply to start", || fake.applies().len() == 1);
        let second = submitted(&organization, move_to_inbox());
        wait_until("the second edit to wait", || {
            organization.lock().waiting.len() == 2
        });
        std::fs::write(fake.dir.join("go"), b"").unwrap();
        let done = Ok(EditOutcome::Done { conflicts: 0 });
        assert_eq!(first.recv_timeout(Duration::from_secs(10)), done);
        assert_eq!(
            second.recv_timeout(Duration::from_secs(10)),
            done,
            "the second submit returns with the apply that wrote its state"
        );
        wait_until("the draft to go", || fake.settled_after(1));
        assert_eq!(fake.applies().len(), 1, "no second apply");
        fake.finish(organization);
    }

    /// What Projects receives: whether a catalog came with the call, and the
    /// status line.
    type Calls = Arc<Mutex<Vec<(Option<String>, (String, bool))>>>;

    fn recording(organization: &Organization) -> Calls {
        let calls: Calls = Arc::default();
        let kept = Arc::clone(&calls);
        organization.set_presented(Arc::new(move |value, _, status| {
            let digest = value.map(|value| value["digest"].as_str().unwrap_or("").to_owned());
            kept.lock().unwrap().push((digest, status));
        }));
        calls
    }

    /// A source that fails before its first state: Projects gets Qt's
    /// "Project sync unavailable: …" line without a catalog, as the Qt
    /// GUI's `swarmFailed` reaches its Projects page.
    #[test]
    fn a_source_that_fails_before_any_state_reaches_projects() {
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), true);
        let calls = recording(&organization);
        organization.observe(&Change::Organization {
            computer: ComputerId::local(),
            update: OrganizationUpdate::Failed {
                code: "unavailable".into(),
                message: "hgs: unknown command swarm".into(),
            },
        });
        assert_eq!(
            calls.lock().unwrap().last().cloned(),
            Some((
                None,
                (
                    "Project sync unavailable: hgs: unknown command swarm".into(),
                    true
                )
            ))
        );
        fake.finish(organization);
    }

    /// A saved draft whose apply fails on every tick: Projects shows the
    /// first state's catalog with the failure line, not an empty screen.
    #[test]
    fn a_saved_draft_whose_apply_fails_reaches_projects() {
        let refused =
            json!({"exit": 1, "stderr": "hgs: unknown field version; reload the catalog\n"});
        let fake = Fake::new(json!([refused]));
        let base = full("d1")["organization"].clone();
        let mut desired = base.clone();
        desired["projects"][0]["sessions"] = json!([]);
        desired["projects"][1]["sessions"] = json!([R]);
        draft::write(
            &fake.draft(),
            &Draft::new(base, desired, serde_json::Map::new()).to_json(),
        )
        .unwrap();
        let organization = fake.organization(&Pump::default(), true);
        let calls = recording(&organization);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let failed = (
            Some("d1".to_owned()),
            (
                "Project sync unavailable: hgs: unknown field version; reload the catalog".into(),
                true,
            ),
        );
        wait_until("Projects to get the failure", || {
            calls.lock().unwrap().last() == Some(&failed)
        });
        fake.finish(organization);
    }

    /// Delete and Sort read their members with the hub's lock held, so a
    /// state adopted between the read and the edit cannot slip in.
    #[test]
    fn delete_and_sort_read_their_members_under_the_hub_lock() {
        let fake = Fake::landing();
        let pump = Pump::default();
        pump.push(row("run:1", "claude/sample-project/review"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let outcome = {
            let held = Arc::clone(&held);
            let shared = Arc::clone(&organization.0);
            organization.submit_with(move |pump| {
                held.store(shared.hub.try_lock().is_err(), Ordering::SeqCst);
                Intent::Sort {
                    project: "alpha".into(),
                    members: by_name(pump.members_of("alpha")),
                }
            })
        };
        assert!(held.load(Ordering::SeqCst), "built under the hub's lock");
        assert_eq!(outcome, EditOutcome::Done { conflicts: 0 });
        fake.finish(organization);
    }

    /// A fallback row's rename seen before the first state changes nothing
    /// shared: no `Renamed` edit, so no ticket, revision or draft write (Qt's
    /// renameSession returns early for an unknown id).
    #[test]
    fn a_fallback_row_renamed_before_the_first_state_makes_no_edit() {
        let fake = Fake::landing();
        let pump = Pump::default();
        pump.push(row("run:2", "claude/sample-project/other"));
        pump.push(row("run:2", "claude/sample-project/other-2"));
        let organization = fake.organization(&pump, true);
        organization.set_source(snapshot("d1"));
        organization.observe(&state("d1"));
        let revision = organization.lock().sync.revision();
        let passes = parked(&organization);
        organization.0.wake.notify_all();
        wait_until("two passes after the first state", || {
            parked(&organization) > passes + 1
        });
        assert_eq!(
            organization.lock().sync.revision(),
            revision,
            "no Renamed edit"
        );
        assert!(fake.applies().is_empty());
        assert!(!fake.draft().exists());
        fake.finish(organization);
    }

    /// The topic's rows for a `swarm get` object, as the server sends them
    /// (src/watch/organization.rs): the summary, a row per project and its
    /// members, and the versions.
    fn topic_rows(full: &Value) -> std::collections::BTreeMap<String, Value> {
        let mut rows = std::collections::BTreeMap::new();
        let mut summary = full.clone();
        summary.as_object_mut().unwrap().remove("versions");
        let projects = full["organization"]["projects"].as_array().unwrap();
        summary["organization"]["projects"] =
            json!(projects.iter().map(|p| p["id"].clone()).collect::<Vec<_>>());
        rows.insert("organization".to_owned(), summary);
        for project in projects {
            let id = project["id"].as_str().unwrap();
            let mut row = project.clone();
            let sessions = row.as_object_mut().unwrap().remove("sessions").unwrap();
            row["session_chunks"] = json!(1);
            rows.insert(format!("project:{id}"), row);
            rows.insert(format!("members:{id}:0"), json!({ "sessions": sessions }));
        }
        rows
    }

    /// The state the core reports for `full` (`OrganizationState::take`).
    fn state_of(full: &Value) -> Change {
        let mut value = full.clone();
        value.as_object_mut().unwrap().remove("versions");
        Change::Organization {
            computer: ComputerId::local(),
            update: OrganizationUpdate::State {
                membership: Arc::new(zerus_client_core::swarm::members(&value["organization"])),
                digest: value["digest"].as_str().unwrap().to_owned(),
                value: Arc::new(value),
            },
        }
    }

    /// New session's copy of a project's folders (SessionsWindow.cpp:2923):
    /// the same from the topic's rows and from `swarm get`, read without hgs.
    #[test]
    fn project_folders_are_the_same_from_the_topic_and_from_swarm_get() {
        let mut catalog = full("d1");
        catalog["swarm_id"] = json!("swarm-1");
        catalog["organization"]["projects"][0]["folders"] = json!([
            {"id": "f1", "machine": "workstation", "machine_id": "m-1", "path": "/home/example/src/sample-project", "name": ""},
            {"machine": "studio", "path": "/srv/docs", "name": "Docs", "machine_name": "Studio"},
            {"id": "f3", "machine": "", "path": "/srv/no-computer"},
            {"id": "f4", "machine": "workstation", "path": "relative"},
            {"id": "f5", "machine": "workstation", "path": "/home/example/src/sample-project"}]);
        let rebuilt = zerus_client_core::organization::rebuild(&topic_rows(&catalog)).unwrap();
        let mut answers = Vec::new();
        for value in [rebuilt, catalog.clone()] {
            let fake = Fake::landing();
            let organization = fake.organization(&Pump::default(), true);
            assert!(
                organization.project_folders("alpha").is_empty(),
                "no state yet"
            );
            assert_eq!(organization.swarm_id(), "");
            organization.set_source(snapshot_of(&value));
            organization.observe(&state_of(&value));
            answers.push(organization.project_folders("alpha"));
            assert!(organization.project_folders("gone").is_empty());
            assert!(organization.project_folders("ungrouped").is_empty());
            assert_eq!(organization.swarm_id(), "swarm-1");
            fake.finish(organization);
        }
        assert_eq!(answers[0], answers[1]);
        let keys: Vec<&str> = answers[0].iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["f1", "studio\n/srv/docs"]);
        assert_eq!(answers[0][1].id, "", "a missing id stays empty");
        assert_eq!(
            serde_json::to_value(&answers[0][1]).unwrap(),
            json!({"id": "", "key": "studio\n/srv/docs", "machine": "studio", "path": "/srv/docs", "name": "Docs", "machine_id": "", "machine_name": "Studio"})
        );
    }

    #[test]
    fn project_folders_answer_at_most_folders() {
        let mut catalog = full("d1");
        let many: Vec<Value> = (0..swarm::FOLDERS + 5)
            .map(|n| json!({"id": format!("f{n}"), "machine": "workstation", "path": format!("/srv/{n}")}))
            .collect();
        catalog["organization"]["projects"][0]["folders"] = json!(many);
        let fake = Fake::landing();
        let organization = fake.organization(&Pump::default(), true);
        organization.set_source(snapshot_of(&catalog));
        organization.observe(&state_of(&catalog));
        let folders = organization.project_folders("alpha");
        assert_eq!(folders.len(), swarm::FOLDERS);
        assert_eq!(
            folders[swarm::FOLDERS - 1].key,
            format!("f{}", swarm::FOLDERS - 1)
        );
        fake.finish(organization);
    }
}
