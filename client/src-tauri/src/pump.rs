//! From the core's change sink to `next_batch`: one thread drains the sink
//! into the coalescer, and `next_batch` takes everything pending, parking
//! up to [`PARK`] while nothing is. A mirror of the last-known rows, statuses,
//! and machine cards lets a reloaded webview start from the full catalog. The
//! organization never reaches the webview: [`Grouping`] turns it into each
//! row's project and the headers (zerus-5wmh.31). A [`Tap`] sees every change
//! as it arrives (the menu bar's desk).

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::Value;
use zerus_client_core::swarm::edit::Intent;
use zerus_client_core::swarm::grouping::{ArchivedRow, GroupOut, Grouping, SyncView};
use zerus_client_core::swarm::layout::Layout;
use zerus_client_core::views::FleetEntry;
use zerus_client_core::{
    Batch, Change, Coalescer, ComputerId, ComputerStatus, MachineMeta, ViewRow,
};

mod terminations;
pub(crate) use terminations::{Termination, MAX_TERMINATIONS};

/// The longest `next_batch` waits for a change before it returns an empty batch.
pub const PARK: Duration = Duration::from_millis(250);

/// Receives every change before the coalescer folds it (the desk thread).
pub type Tap = Box<dyn Fn(&Change) + Send>;

/// A fixed-budget cursor, never a cloned fleet or an unbounded candidate queue.
#[derive(Default)]
pub struct ApprovalCursor {
    computer: String,
    row: usize,
}

#[derive(Default)]
struct Inner {
    coalescer: Coalescer,
    /// A change arrived since the last take.
    dirty: bool,
    mirror: Mirror,
    grouping: Grouping,
    tap: Option<Tap>,
    terminations: terminations::Terminations,
    service: Option<zerus_client_core::desktop::service::Snapshot>,
    service_changed: bool,
}

/// Shared by the drain thread and the commands.
#[derive(Clone, Default)]
pub struct Pump(
    Arc<(Mutex<Inner>, Condvar)>,
    /// Takes about to wait, so a test can order a change after the park.
    #[cfg(test)]
    Arc<std::sync::atomic::AtomicUsize>,
);

/// Action callbacks must not keep the catalog (and its tap/desk) alive.
#[derive(Clone)]
pub(crate) struct WeakPump(
    Weak<(Mutex<Inner>, Condvar)>,
    #[cfg(test)] Weak<std::sync::atomic::AtomicUsize>,
);

impl WeakPump {
    pub(crate) fn upgrade(&self) -> Option<Pump> {
        Some(Pump(
            self.0.upgrade()?,
            #[cfg(test)]
            self.1.upgrade()?,
        ))
    }
}

impl Pump {
    pub fn approval_candidates(
        &self,
        cursor: &mut ApprovalCursor,
        limit: usize,
        eligible: impl Fn(&zerus_client_core::approvals::automatic::Candidate) -> bool,
    ) -> Vec<zerus_client_core::approvals::automatic::Candidate> {
        if limit == 0 {
            return Vec::new();
        }
        use std::ops::Bound::{Excluded, Unbounded};
        let inner = self.lock();
        let mut found: Vec<zerus_client_core::approvals::automatic::Candidate> = Vec::new();
        for _ in 0..512 {
            let Some((computer, rows)) = inner
                .mirror
                .computers
                .range(cursor.computer.clone()..)
                .next()
                .or_else(|| inner.mirror.computers.first_key_value())
            else {
                break;
            };
            if computer != &cursor.computer {
                cursor.computer = computer.clone();
                cursor.row = 0;
            }
            let machine = rows.machine.as_ref();
            if rows.status != Some(ComputerStatus::Live)
                || machine.is_none_or(|m| m.ok != Some(true) || m.machine_id.is_none())
                || cursor.row >= rows.order.len()
            {
                cursor.computer = inner
                    .mirror
                    .computers
                    .range::<String, _>((Excluded(computer), Unbounded))
                    .next()
                    .map_or_else(String::new, |(id, _)| id.clone());
                cursor.row = 0;
                continue;
            }
            let key = &rows.order[cursor.row];
            cursor.row += 1;
            let Some(row) = rows.rows.get(key) else {
                continue;
            };
            if let Some(mut target) =
                zerus_client_core::approvals::automatic::candidate(computer, row)
            {
                target.catalog_key = key.clone();
                target.machine_id = machine.unwrap().machine_id.clone().unwrap();
                if !found.iter().any(|old| old.key == target.key) && eligible(&target) {
                    found.push(target);
                    if found.len() >= limit.min(2) {
                        break;
                    }
                }
            }
        }
        found
    }

    pub fn approval_current(
        &self,
        target: &zerus_client_core::approvals::automatic::Candidate,
    ) -> bool {
        let inner = self.lock();
        inner
            .mirror
            .computers
            .get(&target.target.computer)
            .is_some_and(|rows| {
                rows.status == Some(ComputerStatus::Live)
                    && rows.machine.as_ref().is_some_and(|m| {
                        m.ok == Some(true) && m.machine_id.as_deref() == Some(&target.machine_id)
                    })
                    && rows.rows.get(&target.catalog_key).is_some_and(|row| {
                        zerus_client_core::approvals::automatic::same(target, row)
                    })
            })
    }
    pub fn poster(&self, computer: &str, key: &str, message: &str) -> Option<Arc<[u8]>> {
        self.lock()
            .coalescer
            .poster(&ComputerId::from(computer), key, message)
    }

    pub fn strip_posters(&self, computer: &ComputerId, key: &str, value: &mut Value) {
        self.lock().coalescer.strip_posters(computer, key, value);
    }
    pub(crate) fn downgrade(&self) -> WeakPump {
        WeakPump(
            Arc::downgrade(&self.0),
            #[cfg(test)]
            Arc::downgrade(&self.1),
        )
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0 .0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues one change and wakes a parked `take`.
    pub fn push(&self, change: Change) {
        let mut inner = self.lock();
        if let Some(tap) = &inner.tap {
            tap(&change);
        }
        inner.mirror.observe(&change);
        inner.grouping.observe(&change);
        inner.coalescer.push(change);
        self.wake(inner);
    }

    /// Marks the pump dirty and wakes a parked `take`.
    fn wake(&self, mut inner: MutexGuard<'_, Inner>) {
        inner.dirty = true;
        drop(inner);
        self.0 .1.notify_all();
    }

    /// Wakes a parked `take` without a change: a hub's view changed.
    pub fn poke(&self) {
        self.lock().dirty = true;
        self.0 .1.notify_all();
    }

    /// Hands every later change to `tap` as well.
    pub fn tap(&self, tap: Tap) {
        self.lock().tap = Some(tap);
    }

    /// Everything pending, waiting up to `park` while nothing is.
    pub fn take(&self, park: Duration) -> Batch {
        let inner = self.lock();
        #[cfg(test)]
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (mut inner, _) = self
            .0
             .1
            .wait_timeout_while(inner, park, |inner| !inner.dirty)
            .unwrap_or_else(PoisonError::into_inner);
        inner.dirty = false;
        let mut batch = inner.coalescer.take();
        // The organization stays in Rust; the webview gets each row's project
        // and the headers (zerus-5wmh.31), only when they changed.
        batch.organizations.clear();
        let (groups, grouping) = inner.grouping.take();
        batch.groups = groups;
        batch.grouping = grouping;
        if std::mem::take(&mut inner.service_changed) {
            batch.desktop_service = inner.service.clone();
        }
        batch
    }

    /// Queues the full last-known catalog again (a reloaded webview starts empty).
    pub fn resend(&self) {
        let mut inner = self.lock();
        inner.grouping.resend();
        inner.service_changed = inner.service.is_some();
        let changes = inner.mirror.changes();
        for change in changes {
            inner.coalescer.push(change);
        }
        self.wake(inner);
    }

    pub fn service(&self, snapshot: zerus_client_core::desktop::service::Snapshot) {
        let mut inner = self.lock();
        if inner.service.as_ref() == Some(&snapshot) {
            return;
        }
        inner.service = Some(snapshot);
        inner.service_changed = true;
        self.wake(inner);
    }

    /// Membership only: do not clone another complete row snapshot while
    /// bounded service pages flow through the existing mirror and coalescer.
    pub fn service_catalog_finish(
        &self,
        computer: &ComputerId,
        seen: &std::collections::BTreeSet<String>,
    ) {
        let missing: Vec<String> = self
            .lock()
            .mirror
            .computers
            .get(&computer.0)
            .map(|rows| {
                rows.rows
                    .keys()
                    .filter(|key| !seen.contains(*key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for key in missing {
            self.push(Change::Remove {
                computer: computer.clone(),
                key,
                reason: None,
            });
        }
    }

    pub fn drain_service(
        &self,
        changes: Receiver<crate::desktop_service::Data>,
        observe: impl Fn(&Change) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let pump = self.clone();
        std::thread::Builder::new()
            .name("zerus-service-view".into())
            .spawn(move || {
                for data in changes {
                    match data {
                        crate::desktop_service::Data::Change(change) => {
                            observe(&change);
                            pump.push(*change);
                        }
                        crate::desktop_service::Data::Catalog(delivery) => {
                            delivery.apply(&pump, &observe)
                        }
                        crate::desktop_service::Data::Detail(delivery) => {
                            delivery.apply(&pump, &observe)
                        }
                    }
                }
            })
            .expect("the service view thread starts")
    }

    /// The organization the list shows; `revision` changes whenever it does.
    pub fn present(
        &self,
        organization: &Value,
        membership: Arc<BTreeMap<String, String>>,
        revision: u64,
        sync: SyncView,
    ) {
        let mut inner = self.lock();
        inner.grouping.present(organization, membership, revision);
        inner.grouping.set_sync(sync);
        self.wake(inner);
    }

    pub fn set_sync(&self, sync: SyncView) {
        let mut inner = self.lock();
        inner.grouping.set_sync(sync);
        self.wake(inner);
    }

    /// An edit the user made, applied to the list at once.
    pub fn edited(&self, intent: &Intent) {
        let mut inner = self.lock();
        inner.grouping.edited(intent);
        self.wake(inner);
    }

    pub fn set_default_project(&self, project: &str) -> Result<(), String> {
        let mut inner = self.lock();
        inner.grouping.set_default_project(project)?;
        self.wake(inner);
        Ok(())
    }

    pub fn set_collapsed(&self, project: &str, collapsed: bool) {
        let mut inner = self.lock();
        inner.grouping.set_collapsed(project, collapsed);
        self.wake(inner);
    }

    pub fn members_of(&self, project: &str) -> Vec<(String, String)> {
        self.lock().grouping.members_of(project)
    }

    pub fn default_project(&self) -> String {
        self.lock().grouping.default_project().to_owned()
    }

    pub fn take_renames(&self) -> Vec<(String, String)> {
        self.lock().grouping.take_renames()
    }

    pub fn take_layout(&self) -> Option<Vec<u8>> {
        self.lock().grouping.take_layout()
    }

    pub fn load_layout(&self, bytes: &[u8]) -> Result<(), &'static str> {
        let layout = Layout::try_from_json(bytes)?;
        let mut inner = self.lock();
        inner.grouping.replace_layout(layout);
        self.wake(inner);
        Ok(())
    }

    /// Read projection only, bound to the current local watch hello. A cached,
    /// polled or offline card cannot establish ownership of device-local data.
    pub fn local_windows_node(&self) -> Option<String> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get("")?;
        let machine = rows.machine.as_ref()?;
        (rows.status == Some(ComputerStatus::Live)
            && machine.platform.as_deref() == Some("windows")
            && machine.ok == Some(true)
            && machine.local_sessions != Some(false))
        .then(|| machine.machine_id.clone())
        .flatten()
    }

    pub fn local_windows_projects(&self, organization: &Value, node: &str) -> Option<Value> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get("")?;
        let machine = rows.machine.as_ref()?;
        if rows.status != Some(ComputerStatus::Live)
            || machine.platform.as_deref() != Some("windows")
            || machine.ok != Some(true)
            || machine.local_sessions == Some(false)
            || machine.machine_id.as_deref() != Some(node)
        {
            return None;
        }
        inner
            .grouping
            .local_windows_projects(organization, node)
            .ok()
    }

    pub fn take_runs(&self) -> Option<zerus_client_core::swarm::runs::Snapshot> {
        self.lock().grouping.take_runs()
    }

    /// Called by the organization worker, never the main thread. Decode
    /// before taking the pump lock; rows may already have arrived.
    pub fn load_runs(
        &self,
        bytes: &[u8],
    ) -> Result<(), zerus_client_core::swarm::runs::DecodeError> {
        let runs = zerus_client_core::swarm::runs::Runs::from_json(bytes)?;
        let mut inner = self.lock();
        inner.grouping.replace_runs(runs);
        self.wake(inner);
        Ok(())
    }

    pub fn acknowledge_rename(&self, from: &str, to: &str) {
        let mut inner = self.lock();
        inner.grouping.acknowledge_rename(from, to);
        self.wake(inner);
    }

    pub fn runs_seeded(&self) -> bool {
        self.lock().grouping.runs_seeded()
    }

    pub fn presentation_seeded(&self) -> Result<bool, &'static str> {
        self.lock().grouping.presentation_seeded()
    }

    pub fn windows_folders_import_layout(&self) -> Result<Option<Layout>, &'static str> {
        self.lock().grouping.windows_folders_import_layout()
    }

    pub fn seed_windows_folders(
        &self,
        seed: zerus_client_core::swarm::layout::WindowsFoldersSeed,
        context: &zerus_client_core::swarm::layout::WindowsFoldersContext<'_>,
    ) -> Result<bool, &'static str> {
        let mut inner = self.lock();
        let changed = inner.grouping.seed_windows_folders(seed, context)?;
        if changed {
            self.wake(inner);
        }
        Ok(changed)
    }

    /// Only the worker reads Qt; merge against the current Next values under
    /// the pump lock and let the existing layout writer checkpoint the result.
    pub fn seed_presentation(
        &self,
        seed: zerus_client_core::swarm::layout::PresentationSeed,
    ) -> Result<(), &'static str> {
        let mut inner = self.lock();
        inner.grouping.seed_presentation(seed)?;
        self.wake(inner);
        Ok(())
    }

    /// Qt was read and parsed on the seed worker; merge only its bounded
    /// run memory, without replacing layout changes made during that read.
    pub fn seed_runs(&self, groups: &serde_json::Map<String, Value>) {
        let mut inner = self.lock();
        inner.grouping.seed_runs(groups);
        self.wake(inner);
    }

    /// Read-only project membership for a bounded public-content reply. Search
    /// never admits new identities to the mutation/placement page cache.
    pub fn place_search_archives(
        &self,
        computer: &ComputerId,
        hits: &mut [zerus_client_core::content_search::Hit],
    ) -> Result<(), String> {
        if hits.len() > zerus_client_core::content_search::MAX_HITS {
            return Err("Invalid search response".into());
        }
        let inner = self.lock();
        let machine = computer
            .alias()
            .map(str::to_owned)
            .or_else(|| {
                inner
                    .mirror
                    .computers
                    .get(&computer.0)?
                    .machine
                    .as_ref()?
                    .host
                    .clone()
            })
            .unwrap_or_default();
        for hit in hits.iter_mut().filter(|hit| !hit.archive_id.is_empty()) {
            hit.group = inner.grouping.archive_project(&ArchivedRow {
                computer: computer.clone(),
                key: format!("archive:{}", hit.archive_id),
                machine: machine.clone(),
                archive_id: hit.archive_id.clone(),
                run_id: hit.run_id.clone(),
            });
        }
        Ok(())
    }

    /// One checked archive page, resolved against one organization snapshot.
    /// The local host comes from hello/meta even when its live list is empty;
    /// a peer always uses its configured alias (as live catalog rows do).
    pub fn place_archived(
        &self,
        computer: &ComputerId,
        request: u64,
        generation: u64,
        rows: &mut [ViewRow],
    ) -> Result<Vec<GroupOut>, &'static str> {
        if rows.len() > zerus_client_core::reads::ARCHIVE_PAGE_ROWS {
            return Err(zerus_client_core::reads::ARCHIVE_PAGE_INVALID);
        }
        let mut inner = self.lock();
        if !inner
            .mirror
            .computers
            .get(&computer.0)
            .is_some_and(|source| {
                source.status != Some(ComputerStatus::Ended)
                    && source.archive_generation == generation
            })
        {
            return Err("superseded");
        }
        let machine = computer
            .alias()
            .map(str::to_owned)
            .or_else(|| {
                inner
                    .mirror
                    .computers
                    .get(&computer.0)?
                    .machine
                    .as_ref()?
                    .host
                    .clone()
            })
            .filter(|machine| !machine.is_empty());
        if !rows.is_empty() && machine.is_none() {
            return Err(zerus_client_core::reads::ARCHIVE_IDENTITY_INVALID);
        }
        let machine = machine.unwrap_or_default();
        let identities: Vec<_> = rows
            .iter_mut()
            .map(|row| {
                let id = row
                    .archive_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or(zerus_client_core::reads::ARCHIVE_IDENTITY_INVALID)?;
                row.machine.clone_from(&machine);
                Ok(ArchivedRow {
                    computer: computer.clone(),
                    key: format!("archive:{id}"),
                    machine: machine.clone(),
                    archive_id: id.to_owned(),
                    run_id: row.run_id.clone().unwrap_or_default(),
                })
            })
            .collect::<Result<_, &'static str>>()?;
        let out = inner
            .grouping
            .retain_archive_page(computer, request, &identities)
            .ok_or("superseded")?;
        self.wake(inner);
        Ok(out)
    }

    /// Pin the source before archive I/O; an ended/restarted source invalidates it.
    pub fn archive_generation(&self, computer: &ComputerId) -> u64 {
        self.lock()
            .mirror
            .computers
            .get(&computer.0)
            .map_or(0, |rows| rows.archive_generation)
    }

    /// Reset loaded placement identities without conflating it with hiding Archive.
    pub fn reset_archive_pages(&self, through: u64) {
        let mut inner = self.lock();
        inner.grouping.reset_archive_pages(through);
        self.wake(inner);
    }

    /// A computer's machine card, once its listing named one.
    pub fn machine(&self, computer: &ComputerId) -> Option<MachineMeta> {
        self.lock()
            .mirror
            .computers
            .get(&computer.0)
            .and_then(|rows| rows.machine.clone())
    }

    /// The fleet as the screens see it: every computer whose status has not
    /// ended, this computer first (the mirror is keyed by the id's string,
    /// and "" sorts first), with its machine card's host, platform and the
    /// control-console fields. Without a card a computer keeps local sessions.
    pub fn fleet(&self) -> Vec<FleetEntry> {
        let inner = self.lock();
        inner
            .mirror
            .computers
            .iter()
            .filter_map(|(computer, rows)| {
                let status = rows.status?;
                if status == ComputerStatus::Ended {
                    return None;
                }
                let machine = rows.machine.as_ref();
                Some(FleetEntry {
                    computer: ComputerId(computer.clone()),
                    status: status.name().to_owned(),
                    attempt: status.attempt(),
                    local_sessions: machine.and_then(|m| m.local_sessions).unwrap_or(true),
                    local_host: machine.is_some_and(|m| m.local_host.is_some()),
                    host: machine.and_then(|m| m.host.clone()),
                    platform: machine.and_then(|m| m.platform.clone()),
                })
            })
            .collect()
    }

    /// The last-known row of `(computer, catalog key)`.
    pub fn row(&self, computer: &str, key: &str) -> Option<ViewRow> {
        self.lock()
            .mirror
            .computers
            .get(computer)?
            .rows
            .get(key)
            .cloned()
    }

    /// The computer's live row of that name (a session that started a new
    /// run has a new catalog key). A scan of one computer's rows, only when
    /// the key is gone.
    pub fn row_named(&self, computer: &str, name: &str) -> Option<ViewRow> {
        self.lock()
            .mirror
            .computers
            .get(computer)?
            .rows
            .values()
            .find(|row| row.name == name && row.state != "archived")
            .cloned()
    }

    /// A current live row for one launch, using the catalog's bounded
    /// per-computer index rather than scanning the fleet for every creation.
    pub fn row_for_launch(&self, computer: &str, launch: &str) -> Option<ViewRow> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get(computer)?;
        rows.rows.get(rows.launches.get(launch)?).cloned()
    }

    /// Only a live watch source can replace creation's poll. Inspect at most
    /// the supplied launch IDs under one catalog lock; legacy listings keep
    /// polling even when they contain a nonempty ended-launch list.
    pub fn launch_bindings(
        &self,
        computer: &str,
        launches: &[String],
    ) -> Option<zerus_client_core::creation::BindingListing> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get(computer)?;
        if rows.status != Some(ComputerStatus::Live) {
            return None;
        }
        let machine = rows.machine.as_ref()?;
        let ended_launches = machine.ended_launches.clone()?;
        let sessions = launches
            .iter()
            .filter_map(|launch| rows.rows.get(rows.launches.get(launch)?))
            .map(|row| zerus_client_core::creation::BindingSession {
                launch_id: row.launch_id.clone().unwrap_or_default(),
                cmd: row.notify.cmd.clone(),
                name: row.name.clone(),
                state: row.state.clone(),
            })
            .collect();
        Some(zerus_client_core::creation::BindingListing {
            online: machine.ok == Some(true),
            sessions,
            ended_launches,
        })
    }

    /// Whether the computer counts as online, as the frontend's
    /// `computerOffline` reads it: neither reconnecting nor ended.
    pub fn online(&self, computer: &str) -> bool {
        self.lock()
            .mirror
            .computers
            .get(computer)
            .and_then(|rows| rows.status)
            .is_none_or(|status| {
                !matches!(
                    status,
                    ComputerStatus::Reconnecting { .. } | ComputerStatus::Ended
                )
            })
    }

    /// Drains a change sink on its own thread until every sender is gone,
    /// running `observe` on each change just before it is queued.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained for observers that need only the pre-push callback"
        )
    )]
    pub fn drain_with(
        &self,
        changes: Receiver<Change>,
        observe: impl Fn(&Change) + Send + 'static,
    ) {
        let pump = self.clone();
        std::thread::Builder::new()
            .name("zerus-client-pump".into())
            .spawn(move || {
                for change in changes {
                    observe(&change);
                    pump.push(change);
                }
            })
            .expect("the pump thread starts");
    }
    /// Runs the existing observer before queuing, then signals an interested
    /// consumer after the mirror is current. Neither callback holds the pump
    /// lock, and the change is moved without copying its rows.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Preserved unaccounted callback adapter")
    )]
    pub fn drain_with_after(
        &self,
        changes: Receiver<Change>,
        before: impl Fn(&Change) -> bool + Send + 'static,
        after: impl Fn() + Send + 'static,
    ) {
        self.drain_after(changes, before, after, None);
    }
    /// Retains the source group through EOF, queued changes and both actual
    /// observer callbacks. No queue, poller or row copy is added.
    pub fn drain_accounted(
        &self,
        changes: Receiver<Change>,
        before: impl Fn(&Change) -> bool + Send + 'static,
        after: impl Fn() + Send + 'static,
        resources: Arc<dyn Send + Sync>,
    ) {
        self.drain_after(changes, before, after, Some(resources));
    }
    fn drain_after(
        &self,
        changes: Receiver<Change>,
        before: impl Fn(&Change) -> bool + Send + 'static,
        after: impl Fn() + Send + 'static,
        resources: Option<Arc<dyn Send + Sync>>,
    ) {
        let pump = self.clone();
        std::thread::Builder::new()
            .name("zerus-client-pump".into())
            .spawn(move || {
                let _resources = resources;
                for change in changes {
                    let interested = before(&change);
                    pump.push(change);
                    if interested {
                        after();
                    }
                }
            })
            .expect("the pump thread starts");
    }
}

/// The last-known rows (in arrival order), status and machine card of every
/// computer.
#[derive(Default)]
struct Mirror {
    /// Keyed by the computer id's string, so the core's id type needs no traits.
    computers: BTreeMap<String, Rows>,
}

#[derive(Default)]
struct Rows {
    archive_generation: u64,
    status: Option<ComputerStatus>,
    /// The latest card data; an `ended` status drops it.
    machine: Option<MachineMeta>,
    order: Vec<String>,
    rows: HashMap<String, ViewRow>,
    launches: HashMap<String, String>,
}

impl Rows {
    fn forget_launch(&mut self, key: &str, row: &ViewRow) {
        if let Some(launch) = &row.launch_id {
            if self
                .launches
                .get(launch)
                .is_some_and(|indexed| indexed == key)
            {
                self.launches.remove(launch);
            }
        }
    }

    fn upsert(&mut self, key: &str, row: &ViewRow) {
        if let Some(previous) = self.rows.insert(key.to_owned(), row.clone()) {
            self.forget_launch(key, &previous);
        } else {
            self.order.push(key.to_owned());
        }
        if row.state != "archived" {
            if let Some(launch) = row.launch_id.as_ref().filter(|launch| !launch.is_empty()) {
                self.launches.insert(launch.clone(), key.into());
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(row) = self.rows.remove(key) {
            self.forget_launch(key, &row);
            self.order.retain(|known| known != key);
        }
    }
}

impl Mirror {
    fn observe(&mut self, change: &Change) {
        match change {
            Change::Upsert { computer, key, row } => self
                .computers
                .entry(computer.0.clone())
                .or_default()
                .upsert(key, row),
            Change::Remove { computer, key, .. } => {
                if let Some(rows) = self.computers.get_mut(&computer.0) {
                    rows.remove(key);
                }
            }
            Change::Replace { computer, rows } => {
                let entry = self.computers.entry(computer.0.clone()).or_default();
                entry.order.clear();
                entry.rows.clear();
                entry.launches.clear();
                for (key, row) in rows {
                    entry.upsert(key, row);
                }
            }
            Change::Computer { computer, status } => {
                let entry = self.computers.entry(computer.0.clone()).or_default();
                entry.status = Some(*status);
                if *status == ComputerStatus::Ended {
                    entry.archive_generation = entry.archive_generation.saturating_add(1);
                    entry.machine = None;
                }
            }
            Change::Machine { computer, machine } => {
                self.computers
                    .entry(computer.0.clone())
                    .or_default()
                    .machine = Some(machine.clone())
            }
            Change::Organization { .. }
            | Change::Activity { .. }
            | Change::Processes { .. }
            | Change::Tasks { .. } => {}
        }
    }

    fn changes(&self) -> Vec<Change> {
        let mut out = Vec::new();
        for (computer, rows) in &self.computers {
            if let Some(status) = &rows.status {
                out.push(Change::Computer {
                    computer: ComputerId(computer.clone()),
                    status: *status,
                });
            }
            if let Some(machine) = &rows.machine {
                out.push(Change::Machine {
                    computer: ComputerId(computer.clone()),
                    machine: machine.clone(),
                });
            }
            let list = rows
                .order
                .iter()
                .filter_map(|key| rows.rows.get(key).map(|row| (key.clone(), row.clone())))
                .collect();
            out.push(Change::Replace {
                computer: ComputerId(computer.clone()),
                rows: list,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use zerus_client_core::OrganizationUpdate;

    fn row(name: &str) -> ViewRow {
        ViewRow {
            name: name.into(),
            ..ViewRow::default()
        }
    }

    fn upsert(key: &str, name: &str) -> Change {
        Change::Upsert {
            computer: ComputerId(String::new()),
            key: key.into(),
            row: row(name),
        }
    }

    #[test]
    fn the_tap_sees_every_change() {
        let pump = Pump::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        pump.tap(Box::new(move |change: &Change| {
            let _ = sender.send(change.computer().clone());
        }));
        pump.push(Change::Computer {
            computer: ComputerId::from("mac"),
            status: ComputerStatus::Live,
        });
        assert_eq!(receiver.try_recv(), Ok(ComputerId::from("mac")));
    }

    #[test]
    fn accounted_consumer_retains_eof_and_queued_changes_through_both_callbacks() {
        use zerus_client_core::desktop::drain::{Gate, Observation, Role};
        let pump = Pump::default();
        let gate = Gate::new(1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (entered, events) = std::sync::mpsc::channel();
        let after_entered = entered.clone();
        let (resume_before, before) = std::sync::mpsc::channel();
        let (resume_after, after) = std::sync::mpsc::channel();
        pump.drain_accounted(
            rx,
            move |_| {
                entered.send("before").unwrap();
                before.recv_timeout(Duration::from_secs(3)).unwrap();
                true
            },
            move || {
                after_entered.send("after").unwrap();
                after.recv_timeout(Duration::from_secs(3)).unwrap();
            },
            Arc::new(gate.admit(Role::Fleet).unwrap()),
        );
        let closed = gate.close();
        tx.send(upsert("run:a", "codex/example/a")).unwrap();
        tx.send(upsert("run:b", "codex/example/b")).unwrap();
        drop(tx);
        for key in ["run:a", "run:b"] {
            assert_eq!(
                events.recv_timeout(Duration::from_secs(3)).unwrap(),
                "before"
            );
            assert!(pump.row("", key).is_none());
            assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
            resume_before.send(()).unwrap();
            assert_eq!(
                events.recv_timeout(Duration::from_secs(3)).unwrap(),
                "after"
            );
            assert!(pump.row("", key).is_some());
            assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
            resume_after.send(()).unwrap();
        }
        assert_eq!(
            closed.wait_until(Instant::now() + Duration::from_secs(3)),
            Observation::Idle
        );
    }

    #[test]
    fn rows_are_looked_up_by_key_and_name() {
        let pump = Pump::default();
        pump.push(upsert("run:a", "claude/p/a"));
        pump.push(Change::Computer {
            computer: ComputerId(String::new()),
            status: ComputerStatus::Live,
        });
        pump.push(Change::Computer {
            computer: ComputerId("studio".into()),
            status: ComputerStatus::Reconnecting { attempt: 1 },
        });
        let _ = pump.take(PARK);
        assert_eq!(pump.row("", "run:a").unwrap().name, "claude/p/a");
        assert_eq!(pump.row_named("", "claude/p/a").unwrap().name, "claude/p/a");
        assert!(pump.online(""));
        assert!(!pump.online("studio"));
        assert!(
            pump.online("unknown"),
            "as the frontend reads it: only reconnecting or ended is offline"
        );
    }

    #[test]
    fn a_consumed_after_wake_reads_the_applied_mirror() {
        let pump = Pump::default();
        let reader = pump.clone();
        let (changes, input) = std::sync::mpsc::channel();
        let (wake, wakes) = std::sync::mpsc::sync_channel(1);
        let (consumed, consumes) = std::sync::mpsc::sync_channel(1);
        let (seen, snapshots) = std::sync::mpsc::channel();
        let consumer = std::thread::spawn(move || {
            while wakes.recv().is_ok() {
                seen.send(reader.row("", "run:a")).unwrap();
                consumed.send(()).unwrap();
            }
        });
        pump.drain_with_after(
            input,
            |change| matches!(change, Change::Upsert { .. } | Change::Remove { .. }),
            move || {
                wake.send(()).unwrap();
                // Only this test callback waits: force consumption before the
                // drain proceeds, so an early wake fails deterministically.
                consumes.recv_timeout(Duration::from_secs(2)).unwrap();
            },
        );
        changes
            .send(Change::Upsert {
                computer: ComputerId::local(),
                key: "run:a".into(),
                row: row("codex/p/a"),
            })
            .unwrap();
        changes
            .send(Change::Machine {
                computer: ComputerId::local(),
                machine: MachineMeta::default(),
            })
            .unwrap();
        changes
            .send(Change::Remove {
                computer: ComputerId::local(),
                key: "run:a".into(),
                reason: None,
            })
            .unwrap();
        let first = snapshots.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = snapshots.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(changes);
        consumer.join().unwrap();
        assert_eq!(first.unwrap().name, "codex/p/a");
        assert!(second.is_none(), "the remove is current too");
        assert!(
            snapshots.try_recv().is_err(),
            "uninterested changes do not wake"
        );
    }

    #[test]
    fn launch_lookup_tracks_upserts_replacements_removals_and_computers() {
        let pump = Pump::default();
        let mut launched = row("codex/p/tag");
        launched.launch_id = Some("old".into());
        let push = |computer: &str, key: &str, row: ViewRow| {
            pump.push(Change::Upsert {
                computer: ComputerId(computer.into()),
                key: key.into(),
                row,
            })
        };
        push("", "run:a", launched.clone());
        push("studio", "run:a", launched.clone());
        assert!(pump.row_for_launch("", "old").is_some());
        launched.launch_id = Some("new".into());
        push("", "run:a", launched.clone());
        assert!(pump.row_for_launch("", "old").is_none());
        assert!(pump.row_for_launch("studio", "old").is_some());
        assert!(pump.row_for_launch("", "new").is_some());
        launched.state = "archived".into();
        push("", "run:a", launched);
        assert!(pump.row_for_launch("", "new").is_none());
        pump.push(Change::Replace {
            computer: ComputerId("studio".into()),
            rows: Vec::new(),
        });
        assert!(pump.row_for_launch("studio", "old").is_none());
        assert!(
            pump.lock().mirror.computers["studio"].launches.is_empty(),
            "a replacement cannot retain historical launch keys"
        );
        let mut launched = row("codex/p/tag");
        launched.launch_id = Some("removed".into());
        push("", "run:b", launched);
        pump.push(Change::Remove {
            computer: ComputerId::local(),
            key: "run:b".into(),
            reason: None,
        });
        assert!(pump.row_for_launch("", "removed").is_none());
        assert!(
            pump.lock().mirror.computers[""].launches.is_empty(),
            "removed rows cannot grow an index of past launches"
        );
    }

    #[test]
    fn a_name_finds_only_a_live_row() {
        let pump = Pump::default();
        let mut archived = row("claude/p/a");
        archived.state = "archived".into();
        pump.push(Change::Upsert {
            computer: ComputerId(String::new()),
            key: "archive:a-1".into(),
            row: archived,
        });
        assert!(pump.row_named("", "claude/p/a").is_none());
        assert!(pump.row("", "run:gone").is_none() && pump.row("studio", "run:a").is_none());
        pump.push(Change::Computer {
            computer: ComputerId(String::new()),
            status: ComputerStatus::Ended,
        });
        assert!(!pump.online(""));
    }

    #[test]
    fn take_parks_while_nothing_is_pending() {
        let pump = Pump::default();
        let started = Instant::now();
        let batch = pump.take(Duration::from_millis(60));
        assert!(started.elapsed() >= Duration::from_millis(55));
        assert!(batch.upserts.is_empty() && batch.replaces.is_empty() && batch.activity.is_empty());
    }

    #[test]
    fn a_change_wakes_a_parked_take() {
        let pump = Pump::default();
        let (tx, rx) = std::sync::mpsc::channel();
        pump.drain_with(rx, |_| {});
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            tx.send(upsert("run:a", "claude/p/a")).unwrap();
        });
        let started = Instant::now();
        let batch = pump.take(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(batch.upserts.len(), 1);
        sender.join().unwrap();
    }

    #[test]
    fn a_poke_wakes_a_parked_take() {
        let pump = Pump::default();
        let _ = pump.take(Duration::ZERO);
        let waker = pump.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            waker.poke();
        });
        let started = Instant::now();
        let batch = pump.take(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(2) && batch.is_empty());
    }

    #[test]
    fn a_burst_becomes_one_batch_with_the_latest_row_per_key() {
        let pump = Pump::default();
        for n in 0..5000 {
            pump.push(upsert(&format!("run:{}", n % 50), &format!("claude/p/{n}")));
        }
        let batch = pump.take(PARK);
        assert_eq!(batch.upserts.len(), 50);
        assert!(batch.upserts.iter().all(|r| r
            .row
            .name
            .ends_with(&format!("{}", 4950 + r.key[4..].parse::<usize>().unwrap()))));
    }

    #[test]
    fn resend_queues_the_last_known_catalog() {
        let pump = Pump::default();
        pump.push(Change::Replace {
            computer: ComputerId(String::new()),
            rows: vec![("run:a".into(), row("a")), ("run:b".into(), row("b"))],
        });
        pump.push(Change::Remove {
            computer: ComputerId(String::new()),
            key: "run:a".into(),
            reason: Some("archived".into()),
        });
        pump.push(upsert("run:c", "c"));
        pump.push(Change::Computer {
            computer: ComputerId(String::new()),
            status: ComputerStatus::Live,
        });
        let _ = pump.take(PARK);

        pump.resend();
        let batch = pump.take(PARK);
        assert_eq!(batch.replaces.len(), 1);
        let keys: Vec<_> = batch.replaces[0]
            .rows
            .iter()
            .map(|r| r.key.as_str())
            .collect();
        assert_eq!(keys, ["run:b", "run:c"]);
        assert_eq!(batch.computers[0].status, "live");
    }

    #[test]
    fn the_fleet_lists_unended_computers_with_their_cards_this_computer_first() {
        let pump = Pump::default();
        for (computer, status) in [
            ("studio", ComputerStatus::Reconnecting { attempt: 2 }),
            ("", ComputerStatus::Live),
            ("old", ComputerStatus::Ended),
        ] {
            pump.push(Change::Computer {
                computer: ComputerId(computer.into()),
                status,
            });
        }
        let card = MachineMeta {
            host: Some("workstation".into()),
            platform: Some("windows".into()),
            local_sessions: Some(false),
            local_host: Some(zerus_client_core::machine::LocalHost::default()),
            ..MachineMeta::default()
        };
        pump.push(Change::Machine {
            computer: ComputerId(String::new()),
            machine: card,
        });
        let fleet = pump.fleet();
        let rows: Vec<(&str, &str, u32)> = fleet
            .iter()
            .map(|e| (e.computer.0.as_str(), e.status.as_str(), e.attempt))
            .collect();
        assert_eq!(rows, [("", "live", 0), ("studio", "reconnecting", 2)]);
        assert!(!fleet[0].local_sessions && fleet[0].local_host);
        assert_eq!(
            (fleet[0].host.as_deref(), fleet[0].platform.as_deref()),
            (Some("workstation"), Some("windows"))
        );
        assert!(
            fleet[1].local_sessions && fleet[1].host.is_none(),
            "no card yet: local sessions assumed"
        );
    }

    fn machine(computer: &str, cpu: f64) -> Change {
        let mut meta = MachineMeta {
            host: Some(format!("{computer}-host")),
            ..MachineMeta::default()
        };
        meta.metrics.cpu_percent = Some(cpu);
        Change::Machine {
            computer: ComputerId(computer.into()),
            machine: meta,
        }
    }

    #[test]
    fn resend_replays_the_latest_machine_card_and_none_for_an_ended_computer() {
        let pump = Pump::default();
        for (computer, status) in [("", ComputerStatus::Live), ("studio", ComputerStatus::Live)] {
            pump.push(Change::Computer {
                computer: ComputerId(computer.into()),
                status,
            });
        }
        pump.push(machine("", 10.0));
        pump.push(machine("", 20.0));
        pump.push(machine("studio", 5.0));
        pump.push(Change::Replace {
            computer: ComputerId("studio".into()),
            rows: vec![],
        });
        pump.push(Change::Computer {
            computer: ComputerId("studio".into()),
            status: ComputerStatus::Ended,
        });
        let _ = pump.take(PARK);

        pump.resend();
        let batch = pump.take(PARK);
        assert_eq!(batch.machines.len(), 1, "an ended computer replays no card");
        assert_eq!(batch.machines[0].computer, ComputerId(String::new()));
        assert_eq!(batch.machines[0].machine.metrics.cpu_percent, Some(20.0));
        let statuses: Vec<_> = batch
            .computers
            .iter()
            .map(|c| (c.computer.0.as_str(), c.status.as_str()))
            .collect();
        assert_eq!(statuses, [("", "live"), ("studio", "ended")]);
        // The replayed changes put each card right after its status.
        let replayed = Mirror::changes(&pump.lock().mirror);
        let kinds: Vec<_> = replayed
            .iter()
            .map(|change| match change {
                Change::Computer { .. } => "computer",
                Change::Machine { .. } => "machine",
                Change::Replace { .. } => "replace",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["computer", "machine", "replace", "computer", "replace"]
        );
    }

    fn organization() -> Value {
        serde_json::json!({"default_project": "ungrouped", "projects": [
            {"id": "alpha", "name": "Alpha", "sessions": ["workstation\nclaude/p/a"]},
            {"id": "ungrouped", "name": "Inbox", "sessions": []}]})
    }

    fn local() -> ComputerId {
        ComputerId(String::new())
    }

    fn present(pump: &Pump, organization: &Value) {
        pump.present(
            organization,
            Arc::new(zerus_client_core::swarm::members(organization)),
            1,
            SyncView::default(),
        );
    }

    fn a_row() -> Change {
        Change::Replace {
            computer: local(),
            rows: vec![(
                "run:a".into(),
                ViewRow {
                    machine: "workstation".into(),
                    name: "claude/p/a".into(),
                    ..ViewRow::default()
                },
            )],
        }
    }

    #[test]
    fn the_batch_carries_each_rows_project_and_the_headers_never_the_organization() {
        let pump = Pump::default();
        pump.push(a_row());
        let organization = organization();
        pump.push(Change::Organization {
            computer: local(),
            update: OrganizationUpdate::State {
                value: Arc::new(serde_json::json!({"organization": organization.clone()})),
                digest: "d1".into(),
                membership: Arc::default(),
            },
        });
        present(&pump, &organization);
        let batch = pump.take(PARK);
        assert!(
            batch.organizations.is_empty(),
            "the organization stays in Rust"
        );
        assert_eq!(batch.groups.len(), 1);
        assert_eq!(batch.groups[0].group, "alpha");
        assert_eq!(batch.grouping.as_ref().unwrap().headers.len(), 2);
        let json = serde_json::to_string(&batch).unwrap();
        assert!(!json.contains("organizations") && !json.contains("\"sessions\""));
        pump.resend();
        let again = pump.take(PARK);
        assert_eq!(
            again.groups.len(),
            1,
            "a reloaded webview gets every row's project again"
        );
        assert!(again.grouping.is_some());
        assert!(again.organizations.is_empty());
    }

    /// Parks a `take` on another thread, runs `change` once it is waiting
    /// (the lock is held until then), and returns the batch it woke with.
    fn wake_with(pump: &Pump, change: impl FnOnce(&Pump)) -> Batch {
        let before = pump.1.load(std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let parked = {
            let pump = pump.clone();
            std::thread::spawn(move || tx.send(pump.take(Duration::from_secs(30))).unwrap())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while pump.1.load(std::sync::atomic::Ordering::SeqCst) == before {
            assert!(Instant::now() < deadline, "the take never parked");
            std::thread::yield_now();
        }
        change(pump);
        let batch = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the change woke the parked take");
        parked.join().unwrap();
        batch
    }

    #[test]
    fn a_collapse_wakes_a_parked_take() {
        let pump = Pump::default();
        pump.push(a_row());
        present(&pump, &organization());
        let _ = pump.take(PARK);
        let batch = wake_with(&pump, |pump| pump.set_collapsed("alpha", true));
        assert!(batch.grouping.unwrap().headers[0].collapsed);
    }

    #[test]
    fn presenting_an_organization_wakes_a_parked_take() {
        let pump = Pump::default();
        pump.push(a_row());
        let _ = pump.take(PARK);
        let batch = wake_with(&pump, |pump| present(pump, &organization()));
        assert_eq!(batch.groups[0].group, "alpha");
        assert!(batch.grouping.is_some());
    }

    #[test]
    fn a_loaded_layout_keeps_the_observed_rows() {
        let pump = Pump::default();
        pump.push(a_row());
        present(&pump, &organization());
        let _ = pump.take(PARK);
        pump.load_layout(br#"{"collapsed":["alpha"]}"#).unwrap();
        let batch = pump.take(PARK);
        assert!(batch.grouping.unwrap().headers[0].collapsed);
        pump.resend();
        let again = pump.take(PARK);
        assert_eq!(
            again.groups.len(),
            1,
            "the observed row survives the layout"
        );
        assert_eq!(again.groups[0].group, "alpha");
    }

    #[test]
    fn resend_replays_the_grouping_of_the_latest_organization_until_its_rows_are_replaced_empty() {
        let pump = Pump::default();
        pump.push(Change::Computer {
            computer: local(),
            status: ComputerStatus::Live,
        });
        pump.push(a_row());
        pump.push(Change::Organization {
            computer: local(),
            update: OrganizationUpdate::Failed {
                code: "organization_failed".into(),
                message: "damaged".into(),
            },
        });
        present(&pump, &organization());
        let _ = pump.take(PARK);
        pump.resend();
        let batch = pump.take(PARK);
        assert!(batch.organizations.is_empty());
        assert_eq!(batch.groups.len(), 1);
        assert!(batch.grouping.is_some());
        pump.push(Change::Organization {
            computer: local(),
            update: OrganizationUpdate::Ended,
        });
        pump.push(Change::Replace {
            computer: local(),
            rows: vec![],
        });
        pump.push(Change::Computer {
            computer: local(),
            status: ComputerStatus::Ended,
        });
        let _ = pump.take(PARK);
        pump.resend();
        let batch = pump.take(PARK);
        assert!(batch.organizations.is_empty());
        assert!(
            batch.groups.is_empty(),
            "no stale project for an ended computer's rows"
        );
    }
}
