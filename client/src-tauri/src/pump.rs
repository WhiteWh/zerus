//! From the core's change sink to `next_batch`: one thread drains the sink
//! into the coalescer, and `next_batch` takes everything pending, parking
//! up to [`PARK`] while nothing is. A mirror of the last-known rows, statuses,
//! and machine cards lets a reloaded webview start from the full catalog. The
//! organization never reaches the webview: [`Grouping`] turns it into each
//! row's project and the headers (zerus-5wmh.31). A [`Tap`] sees every change
//! as it arrives (the menu bar's desk).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::Value;
use zerus_client_core::swarm::edit::Intent;
use zerus_client_core::swarm::grouping::{ArchivedRow, GroupOut, Grouping, SyncView};
use zerus_client_core::swarm::layout::Layout;
use zerus_client_core::views::FleetEntry;
use zerus_client_core::{
    Batch, Change, Coalescer, ComputerId, ComputerStatus, MachineMeta, TeamsUpdate, ViewRow,
};

/// Task 235: a computer still showing its cached rows: its id, when they were
/// saved and the rows.
type StaleRows = (String, f64, Vec<(String, ViewRow)>);

mod service;
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
    /// Task 8: where a computer's stream team heads go (the Teams pollers).
    team_heads: Option<TeamHeadsHook>,
    /// Task #16 of team c536f0e5 (P2.5): where a stream's team views go.
    team_views: Option<TeamViewsHook>,
    terminations: terminations::Terminations,
    service: Option<service::Cached>,
    service_changed: bool,
    /// Task 235: computers showing their cached rows, and the marks the next
    /// batch carries (saved at, or None once live rows replaced them).
    stale: BTreeMap<String, f64>,
    stale_out: BTreeMap<String, Option<f64>>,
    /// The cached rows of each stale computer, for a resend (a second
    /// `start` or a reloaded webview) before its live listing arrives.
    stale_rows: BTreeMap<String, Vec<(String, ViewRow)>>,
    /// A live listing changed since the cache was last written.
    cache_dirty: bool,
    /// Task 235: the first live listing and the first rows out were logged.
    first_live_logged: bool,
    first_rows_out_logged: bool,
    first_take_logged: bool,
    /// Task 315: the desktop's team default mute (Qt's `FleetState::applyMute`)
    /// from each computer's team list, applied to every row before the tap.
    team_mutes: zerus_client_core::team_mutes::TeamMutes,
}

/// Task 8: receives a computer's stream team heads (None: poll).
pub type TeamHeadsHook =
    Arc<dyn Fn(&ComputerId, Option<Arc<zerus_client_core::teams::Heads>>) + Send + Sync>;
/// Task #16 of team c536f0e5 (P2.5): receives a computer's stream team
/// views (None: its poller reads them).
pub type TeamViewsHook =
    Arc<dyn Fn(&ComputerId, Option<zerus_client_core::teams::ViewRows>) + Send + Sync>;

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

pub(crate) struct GitScope {
    pub source: Option<(String, u64, u64)>,
    pub path: String,
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

    /// Task 8: hands each computer's stream team heads to `hook` (the Teams
    /// pollers); they never reach the webview.
    pub fn on_team_heads(&self, hook: TeamHeadsHook) {
        self.lock().team_heads = Some(hook);
    }

    /// Task #16 of team c536f0e5 (P2.5): hands each computer's stream team
    /// views to `hook` (the Teams pollers); they never reach the webview.
    pub fn on_team_views(&self, hook: TeamViewsHook) {
        self.lock().team_views = Some(hook);
    }

    /// Queues one change and wakes a parked `take`.
    pub fn push(&self, change: Change) {
        if let Change::TeamHeads { computer, heads } = change {
            // Called without the pump's lock: the hook takes the pollers'.
            let hook = self.lock().team_heads.clone();
            if let Some(hook) = hook {
                hook(&computer, heads);
            }
            return;
        }
        if let Change::TeamViews { computer, views } = change {
            let hook = self.lock().team_views.clone();
            if let Some(hook) = hook {
                hook(&computer, views);
            }
            return;
        }
        let mut inner = self.lock();
        inner.publish(change);
        self.wake(inner);
    }

    pub(crate) fn notification_scope(
        &self,
        computer: &str,
        scope: crate::desktop_service::NotificationScope,
    ) {
        let mut inner = self.lock();
        let rows = inner.mirror.computers.entry(computer.into()).or_default();
        if rows.notification_scope.as_ref() != Some(&scope) {
            rows.notification_rows.clear();
            rows.notification_scope = Some(scope);
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn notification_index_len(&self, computer: &str) -> usize {
        self.lock()
            .mirror
            .computers
            .get(computer)
            .map_or(0, |rows| {
                rows.notification_rows
                    .values()
                    .flat_map(|conversations| conversations.values())
                    .map(BTreeSet::len)
                    .sum()
            })
    }

    #[cfg(test)]
    pub(crate) fn question_index_len(&self, computer: &str) -> usize {
        self.lock()
            .mirror
            .computers
            .get(computer)
            .map_or(0, |rows| {
                rows.question_rows
                    .values()
                    .flat_map(HashMap::values)
                    .flat_map(HashMap::values)
                    .map(BTreeSet::len)
                    .sum()
            })
    }

    pub(crate) fn apply_notification_preferences(
        &self,
        receipt: &crate::desktop_service::NotificationReceipt,
        target: &zerus_client_core::actions::Target,
        preferences: &Value,
    ) {
        let mut inner = self.lock();
        let Some(rows) = inner.mirror.computers.get_mut(&target.computer) else {
            return;
        };
        if rows.status != Some(ComputerStatus::Live)
            || rows.notification_scope.as_ref() != Some(&receipt.scope)
        {
            return;
        }
        let Some(row) = rows
            .rows
            .get(&receipt.key)
            .filter(|row| row.notify.cmd == receipt.provider)
        else {
            return;
        };
        let mut row = row.clone();
        if !zerus_client_core::notification_preferences::apply(&mut row, target, preferences) {
            return;
        }
        let key = receipt.key.clone();
        let change = Change::Upsert {
            computer: ComputerId::from(target.computer.as_str()),
            key,
            row,
        };
        inner.publish(change);
        self.wake(inner);
    }

    pub(crate) fn notification_receipt(
        &self,
        scope: crate::desktop_service::NotificationScope,
        target: &zerus_client_core::actions::Target,
    ) -> Option<crate::desktop_service::NotificationReceipt> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get(&target.computer)?;
        if rows.status != Some(ComputerStatus::Live)
            || rows.notification_scope.as_ref() != Some(&scope)
        {
            return None;
        }
        let (key, row) = rows.rows.iter().find(|(_, row)| {
            row.shared_notifications
                && !row.notify.cmd.is_empty()
                && row.state != "archived"
                && row.name == target.name
                && row.run_id.as_deref() == Some(&target.run_id)
                && row.conversation_id.as_deref() == Some(&target.conversation_id)
        })?;
        Some(crate::desktop_service::NotificationReceipt {
            scope,
            key: key.clone(),
            provider: row.notify.cmd.clone(),
        })
    }

    /// Marks the pump dirty and wakes a parked `take`.
    fn wake(&self, mut inner: MutexGuard<'_, Inner>) {
        inner.dirty = true;
        drop(inner);
        self.0 .1.notify_all();
    }
}

impl Inner {
    /// Apply receipt retention and native's optional question keep before
    /// either the tray tap or the webview sees an incoming row. The indexes
    /// name only service-scoped rows in this mirror.
    fn publish(&mut self, mut change: Change) {
        let mut remuted = Vec::new();
        match &mut change {
            Change::Remove { .. } => self.cache_dirty = true,
            Change::Upsert { computer, row, .. } => {
                self.cache_dirty = true;
                self.team_mutes.apply(&computer.0, row);
                if let Some(rows) = self.mirror.computers.get_mut(&computer.0) {
                    rows.keep_notifications(row);
                    rows.keep_questions(row);
                }
            }
            Change::Replace {
                computer,
                rows: incoming,
            } => {
                // Task 235: the live listing replaces the cached rows.
                self.cache_dirty = true;
                if !std::mem::replace(&mut self.first_live_logged, true) {
                    crate::startup_log::phase("first_live_catalog", None);
                }
                if self.stale.remove(&computer.0).is_some() {
                    self.stale_rows.remove(&computer.0);
                    self.stale_out.insert(computer.0.clone(), None);
                }
                for (_, row) in incoming.iter_mut() {
                    self.team_mutes.apply(&computer.0, row);
                }
                if let Some(rows) = self.mirror.computers.get_mut(&computer.0) {
                    for (_, row) in incoming {
                        rows.keep_notifications(row);
                        rows.keep_questions(row);
                    }
                }
            }
            // Task 315: a team list that changes the muted members changes
            // the rows already shown, after this change.
            Change::Teams { computer, update: TeamsUpdate::List(list), .. } if self.team_mutes.observe(&computer.0, list) => {
                remuted = self.remuted();
            }
            _ => {}
        }
        if let Some(tap) = &self.tap {
            tap(&change);
        }
        self.mirror.observe(&change);
        self.grouping.observe(&change);
        self.coalescer.push(change);
        for change in remuted {
            self.publish(change);
        }
    }

    /// Task 315: the rows whose team default mute changed, as upserts.
    fn remuted(&self) -> Vec<Change> {
        let mut out = Vec::new();
        for (computer, rows) in &self.mirror.computers {
            for (key, row) in &rows.rows {
                let mut row = row.clone();
                if self.team_mutes.apply(computer, &mut row) {
                    out.push(Change::Upsert { computer: ComputerId::from(computer.as_str()), key: key.clone(), row });
                }
            }
        }
        out
    }
}

impl Pump {
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
        self.take_with_clock(park, std::time::Instant::now)
    }

    fn take_with_clock(&self, park: Duration, now: impl FnOnce() -> std::time::Instant) -> Batch {
        let inner = self.lock();
        #[cfg(test)]
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (mut inner, _) = self
            .0
             .1
            .wait_timeout_while(inner, park, |inner| !inner.dirty)
            .unwrap_or_else(PoisonError::into_inner);
        inner.dirty = false;
        if !std::mem::replace(&mut inner.first_take_logged, true) {
            crate::startup_log::phase("first_batch_taken", None);
        }
        let mut batch = inner.coalescer.take();
        // The organization stays in Rust; the webview gets each row's project
        // and the headers (zerus-5wmh.31), only when they changed.
        batch.organizations.clear();
        let (groups, grouping) = inner.grouping.take();
        batch.groups = groups;
        batch.grouping = grouping;
        if std::mem::take(&mut inner.service_changed) {
            batch.desktop_service = inner.service.as_ref().map(|cached| cached.replay(now()));
        }
        batch.stale = std::mem::take(&mut inner.stale_out);
        // Task 235: the first rows the webview gets, cached or live.
        if !inner.first_rows_out_logged && batch.replaces.iter().any(|replace| !replace.rows.is_empty()) {
            inner.first_rows_out_logged = true;
            let cached = batch.stale.values().any(Option::is_some);
            crate::startup_log::phase(if cached { "first_rows_out_cached" } else { "first_rows_out_live" }, None);
        }
        batch
    }

    /// Task 235: shows each cached computer's last rows at once, marked
    /// stale, unless live rows of it are already here. They go to the
    /// webview only: never the tap (no notification, question or attention
    /// from an old list), the mirror or the cache; the live listing replaces
    /// them. Returns the computers seeded.
    pub fn seed_cached(&self, cached: Vec<zerus_client_core::catalog_cache::Cached>) -> usize {
        let mut inner = self.lock();
        let mut seeded = 0;
        for cached in cached {
            if inner.mirror.computers.contains_key(&cached.computer) || inner.stale.contains_key(&cached.computer) {
                continue;
            }
            inner.stale_rows.insert(cached.computer.clone(), cached.rows.clone());
            let change = Change::Replace { computer: ComputerId(cached.computer.clone()), rows: cached.rows };
            inner.grouping.observe(&change);
            inner.coalescer.push(change);
            inner.stale.insert(cached.computer.clone(), cached.saved_at);
            inner.stale_out.insert(cached.computer, Some(cached.saved_at));
            seeded += 1;
        }
        if seeded > 0 {
            self.wake(inner);
        }
        seeded
    }

    /// Task 235: the live listings to cache, when one changed since the last
    /// call; computers still showing cached rows are left out.
    pub fn cache_snapshot(&self, saved_at: f64) -> Option<Vec<zerus_client_core::catalog_cache::Cached>> {
        let mut inner = self.lock();
        if !std::mem::take(&mut inner.cache_dirty) {
            return None;
        }
        let stale: BTreeSet<String> = inner.stale.keys().cloned().collect();
        let mut out = Vec::new();
        for change in inner.mirror.changes() {
            if let Change::Replace { computer, rows } = change {
                if !stale.contains(&computer.0) {
                    out.push(zerus_client_core::catalog_cache::Cached { computer: computer.0, saved_at, rows });
                }
            }
        }
        Some(out)
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
        // Task 235: a computer still showing its cached rows keeps them: the
        // mirror holds none of its live rows yet, only its status.
        let stale: Vec<StaleRows> = inner
            .stale
            .iter()
            .map(|(computer, saved)| (computer.clone(), *saved, inner.stale_rows.get(computer).cloned().unwrap_or_default()))
            .collect();
        for (computer, saved, rows) in stale {
            inner.coalescer.push(Change::Replace { computer: ComputerId(computer.clone()), rows });
            inner.stale_out.insert(computer, Some(saved));
        }
        self.wake(inner);
    }

    pub fn service(&self, snapshot: zerus_client_core::desktop::service::Snapshot) {
        self.service_at(snapshot, std::time::Instant::now());
    }

    fn service_at(
        &self,
        snapshot: zerus_client_core::desktop::service::Snapshot,
        received: std::time::Instant,
    ) {
        let mut inner = self.lock();
        if inner
            .service
            .as_ref()
            .is_some_and(|cached| cached.matches(&snapshot))
        {
            return;
        }
        inner.service = Some(service::Cached::receive(
            snapshot,
            inner.service.as_ref(),
            received,
        ));
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
        // Task 235: the service's pages name live rows one by one; once its
        // listing is whole, the cached rows give way to exactly these. The
        // tap saw each live row already, so this goes to the webview only.
        let mut inner = self.lock();
        if inner.stale.remove(&computer.0).is_some() {
            inner.stale_rows.remove(&computer.0);
            let rows = inner
                .mirror
                .computers
                .get(&computer.0)
                .map(|rows| {
                    rows.order
                        .iter()
                        .filter_map(|key| rows.rows.get(key).map(|row| (key.clone(), row.clone())))
                        .collect()
                })
                .unwrap_or_default();
            inner.coalescer.push(Change::Replace { computer: computer.clone(), rows });
            inner.stale_out.insert(computer.0.clone(), None);
            self.wake(inner);
        }
    }

    #[cfg(all(test, unix))]
    pub fn drain_service(
        &self,
        changes: Receiver<crate::desktop_service::Data>,
        observe: impl Fn(&Change) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        self.drain_service_with_after(
            changes,
            move |change| {
                observe(change);
                false
            },
            || {},
        )
    }

    /// Service deliveries validate their scope before observing a change.
    /// Wake consumers only after admission and the mirror update have finished.
    pub fn drain_service_with_after(
        &self,
        changes: Receiver<crate::desktop_service::Data>,
        before: impl Fn(&Change) -> bool + Send + 'static,
        after: impl Fn() + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let pump = self.clone();
        std::thread::Builder::new()
            .name("zerus-service-view".into())
            .spawn(move || {
                for data in changes {
                    let interested = std::cell::Cell::new(false);
                    let observe = |change: &Change| {
                        interested.set(before(change) || interested.get());
                    };
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
                    if interested.get() {
                        after();
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

    /// N6 (owner, 2026-11-08): a project folder of this computer on Windows,
    /// edited in the device-local family; the organization republishes and
    /// saves the layout, never the shared catalog.
    pub fn edit_windows_folder(
        &self,
        organization: &serde_json::Value,
        project: &str,
        node: &str,
        change: &zerus_client_core::swarm::folder_edit::Change,
    ) -> Result<(), String> {
        let mut inner = self.lock();
        inner.grouping.edit_local_windows_folder(organization, node, project, change)?;
        self.wake(inner);
        Ok(())
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

    /// One coherent current card observation; reconnecting cards remain data
    /// for presentation but cannot establish native action currentness.
    pub(crate) fn live_machine(&self, computer: &ComputerId) -> Option<MachineMeta> {
        let inner = self.lock();
        let rows = inner.mirror.computers.get(&computer.0)?;
        if rows.status != Some(ComputerStatus::Live) {
            return None;
        }
        rows.machine.clone()
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

    /// Only compact native Git routing facts, never a clone of child/details data.
    pub(crate) fn git_scope(&self, computer: &str, key: &str) -> Option<GitScope> {
        let inner = self.lock();
        let row = inner.mirror.computers.get(computer)?.rows.get(key)?;
        Some(GitScope {
            source: row.catalog_source.clone(),
            path: zerus_client_core::git::path(row)?.to_owned(),
        })
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

    /// The listed durable session ID of the computer's row for `run`, its
    /// catalog key `run:<id>`, without copying the row (zerus-ucck P1c).
    pub fn run_session_id(&self, computer: &str, run: &str) -> Option<String> {
        if run.is_empty() {
            return None;
        }
        let inner = self.lock();
        let row = inner
            .mirror
            .computers
            .get(computer)?
            .rows
            .get(&zerus_watch_protocol::key_for(Some(run), ""))?;
        (row.run_id.as_deref() == Some(run))
            .then(|| row.session_id.clone())
            .flatten()
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

    /// Whether the view currently shows the computer Live, exactly.
    pub fn live(&self, computer: &str) -> bool {
        self.lock()
            .mirror
            .computers
            .get(computer)
            .and_then(|rows| rows.status)
            == Some(ComputerStatus::Live)
    }

    /// Keep cache warm's fleet poll: each computer whose listing is current
    /// (streamed or polled, never cached or reconnecting), with the rows
    /// `pick` keeps, under one lock. Rows `pick` leaves are not copied.
    pub(crate) fn current_rows(
        &self,
        pick: impl Fn(&ViewRow) -> bool,
    ) -> Vec<(String, Vec<ViewRow>)> {
        let inner = self.lock();
        inner
            .mirror
            .computers
            .iter()
            .filter(|(computer, rows)| {
                matches!(
                    rows.status,
                    Some(ComputerStatus::Live | ComputerStatus::Polled)
                ) && !inner.stale.contains_key(*computer)
            })
            .map(|(computer, rows)| {
                (
                    computer.clone(),
                    rows.rows
                        .values()
                        .filter(|&row| pick(row))
                        .cloned()
                        .collect(),
                )
            })
            .collect()
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

type NotificationRows = HashMap<String, HashMap<String, BTreeSet<(Reverse<i64>, String)>>>;
/// Session name -> archive -> conversation -> keys, in native key order.
type QuestionRows = HashMap<String, HashMap<String, HashMap<String, BTreeSet<String>>>>;

#[derive(Default)]
struct Rows {
    archive_generation: u64,
    status: Option<ComputerStatus>,
    /// The latest restart schedule, replayed after the status; an `ended`
    /// status drops it.
    retry: zerus_client_core::Retry,
    /// The latest card data; an `ended` status drops it.
    machine: Option<MachineMeta>,
    order: Vec<String>,
    rows: HashMap<String, ViewRow>,
    launches: HashMap<String, String>,
    notification_scope: Option<crate::desktop_service::NotificationScope>,
    /// Provider/conversation -> highest revision, then native key order.
    /// At most one key per current mirror row; no row or payload copies.
    notification_rows: NotificationRows,
    /// Service-scoped rows with a known optional question count, for the
    /// keep native mode applies per frame (zerus-tsdc). It follows the rows,
    /// not the scope: native keeps a count across a reconnect's snapshot.
    /// At most one key per current mirror row; no row or payload copies.
    question_rows: QuestionRows,
}

impl Rows {
    fn keep_notifications(&self, row: &mut ViewRow) {
        let Some((_, key)) = self
            .notification_rows
            .get(&row.notify.cmd)
            .and_then(|conversations| {
                conversations.get(row.conversation_id.as_deref().unwrap_or(""))
            })
            .and_then(|keys| keys.first())
        else {
            return;
        };
        if let Some(previous) = self.rows.get(key) {
            zerus_client_core::notification_preferences::keep(previous, row);
        }
    }

    /// Native `Before::keep_questions`: an incoming row whose question index
    /// is incomplete keeps the count and IDs of the first current row (in key
    /// order) of its session, archive and conversation with a known count.
    fn keep_questions(&self, row: &mut ViewRow) {
        use zerus_client_core::optional_questions::{archive, conversation, keep, unknown};
        if self.notification_scope.is_none() || !unknown(row) {
            return;
        }
        let Some(key) = self
            .question_rows
            .get(&row.name)
            .and_then(|archives| archives.get(archive(row)))
            .and_then(|conversations| conversations.get(conversation(row)))
            .and_then(BTreeSet::first)
        else {
            return;
        };
        if let Some(previous) = self.rows.get(key) {
            keep(previous, row);
        }
    }

    fn forget_questions(&mut self, key: &str, row: &ViewRow) {
        use zerus_client_core::optional_questions::{archive, conversation};
        let Some(archives) = self.question_rows.get_mut(&row.name) else {
            return;
        };
        if let Some(conversations) = archives.get_mut(archive(row)) {
            if let Some(keys) = conversations.get_mut(conversation(row)) {
                keys.remove(key);
                if keys.is_empty() {
                    conversations.remove(conversation(row));
                }
            }
            if conversations.is_empty() {
                archives.remove(archive(row));
            }
        }
        if archives.is_empty() {
            self.question_rows.remove(&row.name);
        }
    }

    fn forget_notifications(&mut self, key: &str, row: &ViewRow) {
        let Some(conversation) = row.conversation_id.as_deref() else {
            return;
        };
        if let Some(conversations) = self.notification_rows.get_mut(&row.notify.cmd) {
            if let Some(keys) = conversations.get_mut(conversation) {
                keys.remove(&(
                    Reverse(row.notification_revision.unwrap_or(-1)),
                    key.to_owned(),
                ));
                if keys.is_empty() {
                    conversations.remove(conversation);
                }
            }
            if conversations.is_empty() {
                self.notification_rows.remove(&row.notify.cmd);
            }
        }
    }

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
            self.forget_notifications(key, &previous);
            self.forget_questions(key, &previous);
        } else {
            self.order.push(key.to_owned());
        }
        if self.notification_scope.is_some() && zerus_client_core::optional_questions::known(row) {
            use zerus_client_core::optional_questions::{archive, conversation};
            self.question_rows
                .entry(row.name.clone())
                .or_default()
                .entry(archive(row).to_owned())
                .or_default()
                .entry(conversation(row).to_owned())
                .or_default()
                .insert(key.to_owned());
        }
        if self.notification_scope.is_some() && row.notify.preferences.is_some() {
            if let Some(conversation) = row.conversation_id.as_deref().filter(|c| !c.is_empty()) {
                self.notification_rows
                    .entry(row.notify.cmd.clone())
                    .or_default()
                    .entry(conversation.into())
                    .or_default()
                    .insert((Reverse(row.notification_revision.unwrap_or(-1)), key.into()));
            }
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
            self.forget_notifications(key, &row);
            self.forget_questions(key, &row);
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
                entry.notification_rows.clear();
                entry.question_rows.clear();
                for (key, row) in rows {
                    entry.upsert(key, row);
                }
            }
            Change::Computer { computer, status } => {
                let entry = self.computers.entry(computer.0.clone()).or_default();
                entry.status = Some(*status);
                if *status == ComputerStatus::Ended {
                    entry.notification_scope = None;
                    entry.notification_rows.clear();
                    entry.archive_generation = entry.archive_generation.saturating_add(1);
                    entry.machine = None;
                    entry.retry = zerus_client_core::Retry::Idle;
                }
            }
            Change::Retry { computer, retry } => {
                self.computers.entry(computer.0.clone()).or_default().retry = *retry;
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
            | Change::Tasks { .. }
            | Change::Git { .. }
            | Change::Teams { .. }
            | Change::TeamHeads { .. }
            | Change::TeamViews { .. } => {}
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
            if rows.retry != zerus_client_core::Retry::Idle {
                out.push(Change::Retry {
                    computer: ComputerId(computer.clone()),
                    retry: rows.retry,
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

    fn cached(computer: &str, keys: &[&str]) -> zerus_client_core::catalog_cache::Cached {
        zerus_client_core::catalog_cache::Cached {
            computer: computer.into(),
            saved_at: 1_700_000_000.0,
            rows: keys.iter().map(|key| ((*key).to_owned(), row(&format!("claude/p/{key}")))).collect(),
        }
    }

    /// Task 235: the last list on disk reaches the webview at once, marked
    /// stale, never the tap; the live listing replaces it and clears the
    /// mark; only live rows are written back to the cache.
    #[test]
    fn cached_rows_show_marked_stale_until_the_live_listing_replaces_them() {
        let pump = Pump::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        pump.tap(Box::new(move |change: &Change| {
            let _ = sender.send(change.computer().clone());
        }));
        assert_eq!(pump.seed_cached(vec![cached("", &["a", "b"]), cached("studio", &["c"])]), 2);
        assert!(receiver.try_recv().is_err(), "the tap never sees cached rows");
        let batch = pump.take(Duration::ZERO);
        assert_eq!(batch.replaces.len(), 2);
        assert_eq!(batch.stale.get(""), Some(&Some(1_700_000_000.0)));
        assert_eq!(batch.stale.get("studio"), Some(&Some(1_700_000_000.0)));
        assert!(pump.cache_snapshot(1.0).is_none(), "nothing live to write yet");
        // The live listing of this computer replaces its cached rows.
        pump.push(Change::Replace { computer: ComputerId(String::new()), rows: vec![("a".into(), row("claude/p/a"))] });
        let batch = pump.take(Duration::ZERO);
        assert_eq!(batch.stale.get(""), Some(&None), "the mark is cleared");
        assert!(!batch.stale.contains_key("studio"), "the other computer stays stale");
        assert_eq!(batch.replaces[0].rows.len(), 1);
        let written = pump.cache_snapshot(2.0).expect("a live listing changed");
        assert_eq!(written.len(), 1, "a computer still showing cached rows is not written back");
        assert_eq!((written[0].computer.as_str(), written[0].rows.len(), written[0].saved_at), ("", 1, 2.0));
        assert!(pump.cache_snapshot(3.0).is_none(), "written only when something changed");
        pump.push(upsert("b", "claude/p/b"));
        assert_eq!(pump.cache_snapshot(4.0).unwrap()[0].rows.len(), 2);
    }

    /// Task 315 (F2): as Qt's FleetState, the members of a team that is not
    /// archived (here a done one) are muted by the desktop's default: the rows
    /// already shown change when the list arrives, later rows come muted, the
    /// tap (notifications, menu bar) sees it, and the lead stays unmuted.
    #[test]
    fn a_team_list_mutes_its_members_in_shown_and_later_rows() {
        use std::sync::Arc;
        use zerus_client_core::teams::view::{ListedMember, SessionView, TeamList, TeamSummary};
        let member = |id: &str, name: &str| ListedMember {
            id: id.into(),
            lead: id == "lead",
            session: Some(SessionView { name: name.into(), ..SessionView::default() }),
            ..ListedMember::default()
        };
        let list = |status: &str| TeamList {
            teams: vec![TeamSummary {
                status: status.into(),
                lead: "lead".into(),
                members: vec![member("lead", "claude/p/lead"), member("cook", "dsh/p/cook")],
                ..TeamSummary::default()
            }],
            truncated: false,
        };
        let teams = |status: &str| {
            Change::teams(
                ComputerId(String::new()),
                "list".into(),
                TeamsUpdate::List(Arc::new(list(status))),
            )
        };
        let pump = Pump::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        pump.tap(Box::new(move |change: &Change| {
            if let Change::Upsert { row, .. } = change {
                let _ = sender.send((row.name.clone(), row.muted));
            }
        }));
        pump.push(Change::Replace {
            computer: ComputerId(String::new()),
            rows: vec![("lead".into(), row("claude/p/lead")), ("cook".into(), row("dsh/p/cook"))],
        });
        let muted = |key: &str| {
            let inner = pump.lock();
            let row = &inner.mirror.computers[""].rows[key];
            (row.muted, row.team_muted)
        };
        assert_eq!(muted("cook"), (false, false));
        pump.push(teams("done"));
        assert_eq!(muted("cook"), (true, true));
        assert_eq!(muted("lead"), (false, false), "the lead is never muted by the default");
        assert_eq!(receiver.try_recv().ok(), Some(("dsh/p/cook".to_owned(), true)));
        // The same list again changes nothing.
        pump.push(teams("done"));
        assert!(receiver.try_recv().is_err());
        // A later listing of the member comes muted.
        pump.push(upsert("cook", "dsh/p/cook"));
        assert_eq!(muted("cook"), (true, true));
        // Archived: the member is free again.
        pump.push(teams("archived"));
        assert_eq!(muted("cook"), (false, false));
        let batch = pump.take(Duration::ZERO);
        assert!(!batch.replaces.is_empty());
    }

    /// Task 235: a second `start` (or a reloaded webview) replays the mirror,
    /// which holds only the computer's status yet: its cached rows and mark
    /// come back with it, never an empty list over them.
    #[test]
    fn a_resend_keeps_the_cached_rows_until_the_live_listing() {
        let pump = Pump::default();
        pump.seed_cached(vec![cached("", &["a", "b"])]);
        pump.push(Change::Computer { computer: ComputerId(String::new()), status: ComputerStatus::Probing });
        let _ = pump.take(Duration::ZERO);
        pump.resend();
        let batch = pump.take(Duration::ZERO);
        let rows: Vec<usize> = batch.replaces.iter().filter(|replace| replace.computer.0.is_empty()).map(|replace| replace.rows.len()).collect();
        assert_eq!(rows, [2], "the cached rows, not the mirror's empty list");
        assert_eq!(batch.stale.get(""), Some(&Some(1_700_000_000.0)));
        pump.push(Change::Replace { computer: ComputerId(String::new()), rows: vec![("a".into(), row("claude/p/a"))] });
        let _ = pump.take(Duration::ZERO);
        pump.resend();
        let batch = pump.take(Duration::ZERO);
        assert_eq!(batch.replaces.iter().find(|replace| replace.computer.0.is_empty()).map(|replace| replace.rows.len()), Some(1));
        assert!(batch.stale.is_empty(), "no stale mark after the live listing");
    }

    /// Task 235: a computer whose live rows are here already is never
    /// overwritten by its cached rows, and a seed happens once.
    #[test]
    fn cached_rows_never_cover_live_ones() {
        let pump = Pump::default();
        pump.push(upsert("live", "claude/p/live"));
        assert_eq!(pump.seed_cached(vec![cached("", &["old"])]), 0);
        assert_eq!(pump.seed_cached(vec![cached("mac", &["x"])]), 1);
        assert_eq!(pump.seed_cached(vec![cached("mac", &["y"])]), 0, "seeded once");
        let batch = pump.take(Duration::ZERO);
        assert!(batch.replaces.iter().all(|replace| !replace.computer.0.is_empty()), "the live computer keeps its rows");
    }

    /// Task 235: the desktop service names live rows page by page; once its
    /// listing is whole, the cached rows give way to exactly those.
    #[test]
    fn a_whole_service_listing_replaces_the_cached_rows() {
        let pump = Pump::default();
        pump.seed_cached(vec![cached("", &["old", "kept"])]);
        let _ = pump.take(Duration::ZERO);
        pump.push(upsert("kept", "claude/p/kept"));
        let seen: BTreeSet<String> = ["kept".to_owned()].into();
        pump.service_catalog_finish(&ComputerId(String::new()), &seen);
        let batch = pump.take(Duration::ZERO);
        assert_eq!(batch.stale.get(""), Some(&None));
        let keys: Vec<&str> = batch.replaces[0].rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["kept"], "the old cached row is gone");
    }

    fn upsert(key: &str, name: &str) -> Change {
        Change::Upsert {
            computer: ComputerId(String::new()),
            key: key.into(),
            row: row(name),
        }
    }

    fn waiting_service(
        incarnation: u64,
        serial: u64,
        remaining: u64,
    ) -> zerus_client_core::desktop::service::Snapshot {
        serde_json::from_value(serde_json::json!({
            "connected":true,"epoch":"fixture","generation":1,"background":{},"attention_counts":null,
            "automatic_approvals":false,"details_available":true,"code":"",
            "fleet_computers":[{"id":"studio","status":"reconnecting","attempt":2,"incarnation":incarnation,
                "retry":{"phase":"waiting","serial":serial,"attempt":2,"delay_ms":10000,"remaining_ms":remaining}}]
        })).unwrap()
    }

    fn service_remaining(pump: &Pump, at: Instant) -> u64 {
        pump.take_with_clock(Duration::ZERO, || at)
            .desktop_service
            .unwrap()
            .fleet_computers
            .unwrap()[0]
            .retry
            .as_ref()
            .unwrap()
            .remaining_ms
            .unwrap()
    }

    #[test]
    fn service_reconnect_replay_ages_after_a_webview_reload() {
        let pump = Pump::default();
        let arrived = Instant::now();
        pump.service_at(waiting_service(7, 3, 5000), arrived);
        assert_eq!(service_remaining(&pump, arrived), 5000);
        // The new webview has no previous performance.now() anchor. Its replay
        // must already exclude time spent in the native cache.
        pump.resend();
        assert_eq!(
            service_remaining(&pump, arrived + Duration::from_secs(3)),
            2000
        );
        pump.resend();
        assert_eq!(
            service_remaining(&pump, arrived + Duration::from_secs(8)),
            0
        );
    }

    #[test]
    fn service_reconnect_later_frames_only_shorten_the_same_wait() {
        let pump = Pump::default();
        let arrived = Instant::now();
        let first = waiting_service(7, 3, 5000);
        pump.service_at(first.clone(), arrived);
        assert_eq!(service_remaining(&pump, arrived), 5000);
        // Republishing an unrelated field cannot give the cached wait a new receipt time.
        let mut unrelated = first;
        unrelated.code = "unrelated_status".into();
        pump.service_at(unrelated, arrived + Duration::from_secs(2));
        assert_eq!(
            service_remaining(&pump, arrived + Duration::from_secs(3)),
            2000
        );
        let later = arrived + Duration::from_secs(3);
        pump.service_at(waiting_service(7, 3, 4000), later);
        assert_eq!(service_remaining(&pump, later), 2000);
        pump.service_at(waiting_service(7, 3, 500), later);
        assert_eq!(service_remaining(&pump, later), 500);
        let expired = later + Duration::from_millis(500);
        pump.resend();
        assert_eq!(service_remaining(&pump, expired), 0);
        // A new wait or supervisor gets its own receipt.
        pump.service_at(waiting_service(7, 4, 4000), expired);
        assert_eq!(service_remaining(&pump, expired), 4000);
        pump.service_at(waiting_service(8, 4, 6000), expired);
        assert_eq!(service_remaining(&pump, expired), 6000);
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
        // Task #60: the event ends the take, not its timeout (the floor).
        let batch = pump.take(crate::test_wait::FLOOR);
        assert!(started.elapsed() < crate::test_wait::FLOOR);
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
        let batch = pump.take(crate::test_wait::FLOOR);
        assert!(started.elapsed() < crate::test_wait::FLOOR && batch.is_empty());
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

    #[test]
    fn live_machine_returns_the_same_complete_identity_only_for_a_live_card() {
        let pump = Pump::default();
        let computer = ComputerId("selected".into());
        assert!(pump.live_machine(&computer).is_none());
        let card = MachineMeta {
            machine_id: Some("00000000-0000-4000-8000-000000000001".into()),
            host: Some("selected-host".into()),
            ok: Some(true),
            ..MachineMeta::default()
        };
        pump.push(Change::Machine {
            computer: computer.clone(),
            machine: card.clone(),
        });
        assert_eq!(pump.machine(&computer), Some(card.clone()));
        assert!(
            pump.live_machine(&computer).is_none(),
            "an unclassified cached card is not Live"
        );
        pump.push(Change::Computer {
            computer: computer.clone(),
            status: ComputerStatus::Live,
        });
        assert_eq!(pump.live_machine(&computer), Some(card));
    }

    #[test]
    fn live_machine_refuses_reconnecting_and_ended_even_with_a_retained_card() {
        let pump = Pump::default();
        let computer = ComputerId("selected".into());
        pump.push(Change::Computer {
            computer: computer.clone(),
            status: ComputerStatus::Live,
        });
        pump.push(machine("selected", 10.0));
        let held = pump.machine(&computer).unwrap();
        pump.push(Change::Computer {
            computer: computer.clone(),
            status: ComputerStatus::Reconnecting { attempt: 1 },
        });
        assert_eq!(
            pump.machine(&computer),
            Some(held),
            "presentation keeps last-known data"
        );
        assert!(pump.live_machine(&computer).is_none());
        pump.push(Change::Computer {
            computer: computer.clone(),
            status: ComputerStatus::Ended,
        });
        assert!(pump.live_machine(&computer).is_none());
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

    /// A service-scoped row of `name` and `conversation` with a listed
    /// optional question count, or with an incomplete index (`None`).
    fn asked(name: &str, conversation: &str, count: Option<(i64, &[&str])>) -> ViewRow {
        let mut row = ViewRow {
            name: name.into(),
            conversation_id: Some(conversation.into()),
            ..ViewRow::default()
        };
        row.notify.optional_reported = true;
        row.notify.optional_count = count.map_or(-1, |(count, _)| count);
        row.notify.optional_ids = count.map_or_else(Vec::new, |(_, ids)| {
            ids.iter().map(|id| (*id).to_owned()).collect()
        });
        row.optional_question_count = count.map(|(count, _)| count);
        row
    }

    fn service_pump() -> (Pump, std::sync::mpsc::Receiver<Change>) {
        let pump = Pump::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        pump.tap(Box::new(move |change: &Change| {
            let _ = sender.send(change.clone());
        }));
        pump.notification_scope("", crate::desktop_service::NotificationScope::for_test(""));
        (pump, receiver)
    }

    fn put(pump: &Pump, key: &str, row: ViewRow) {
        pump.push(Change::Upsert {
            computer: ComputerId(String::new()),
            key: key.into(),
            row,
        });
    }

    fn questions(row: &ViewRow) -> (Option<i64>, i64, Vec<String>) {
        (
            row.optional_question_count,
            row.notify.optional_count,
            row.notify.optional_ids.clone(),
        )
    }

    #[test]
    fn service_rows_keep_the_last_known_optional_questions_like_native_mode() {
        // zerus-tsdc: a resume's successor run lists its question index as
        // incomplete; native mode keeps the paused run's count and IDs.
        let (pump, tap) = service_pump();
        put(
            &pump,
            "run:a",
            asked("codex/p/one", "c1", Some((2, &["q1", "q2"]))),
        );
        put(&pump, "run:b", asked("codex/p/one", "c1", None));
        let kept = questions(&pump.row("", "run:b").unwrap());
        assert_eq!(kept, (Some(2), 2, vec!["q1".to_owned(), "q2".to_owned()]));
        // The tray tap sees the kept row too, not only the webview.
        let tapped: Vec<_> = tap.try_iter().collect();
        assert!(
            matches!(tapped.last(), Some(Change::Upsert { key, row, .. })
            if key == "run:b" && questions(row) == kept)
        );
        pump.push(Change::Remove {
            computer: ComputerId(String::new()),
            key: "run:a".into(),
            reason: Some("replaced".into()),
        });
        // The zero count of a session without questions is kept as well,
        // from the same run's previous version.
        put(&pump, "run:z", asked("codex/p/zero", "c9", Some((0, &[]))));
        put(&pump, "run:z", asked("codex/p/zero", "c9", None));
        assert_eq!(
            questions(&pump.row("", "run:z").unwrap()),
            (Some(0), 0, vec![])
        );
        // Another conversation, session or archive starts unknown.
        put(&pump, "run:c", asked("codex/p/one", "c2", None));
        put(&pump, "run:d", asked("codex/p/two", "c1", None));
        let mut archived = asked("codex/p/one", "c1", None);
        archived.state = "archived".into();
        archived.archive_id = Some("a1".into());
        put(&pump, "archive:a1", archived);
        for key in ["run:c", "run:d", "archive:a1"] {
            assert_eq!(
                questions(&pump.row("", key).unwrap()),
                (None, -1, vec![]),
                "{key}"
            );
        }
        // A listed count is the host's answer, never replaced.
        put(
            &pump,
            "run:b",
            asked("codex/p/one", "c1", Some((1, &["q3"]))),
        );
        assert_eq!(
            questions(&pump.row("", "run:b").unwrap()),
            (Some(1), 1, vec!["q3".to_owned()])
        );
    }

    #[test]
    fn a_removed_predecessor_is_not_kept_and_native_mode_is_untouched() {
        // A paged Start fresh removes the old run before its successor lands:
        // native mode has no previous row then, and neither has the pump.
        let (pump, _tap) = service_pump();
        put(
            &pump,
            "run:a",
            asked("codex/p/one", "c1", Some((2, &["q1", "q2"]))),
        );
        pump.push(Change::Remove {
            computer: ComputerId(String::new()),
            key: "run:a".into(),
            reason: Some("replaced".into()),
        });
        put(&pump, "run:b", asked("codex/p/one", "c1", None));
        assert_eq!(
            questions(&pump.row("", "run:b").unwrap()),
            (None, -1, vec![])
        );
        // Without a service scope (native mode) CatalogState already kept
        // what native keeps; the pump changes nothing and indexes nothing.
        let native = Pump::default();
        put(
            &native,
            "run:a",
            asked("codex/p/one", "c1", Some((2, &["q1"]))),
        );
        put(&native, "run:b", asked("codex/p/one", "c1", None));
        assert_eq!(
            questions(&native.row("", "run:b").unwrap()),
            (None, -1, vec![])
        );
        assert_eq!(native.question_index_len(""), 0);
    }

    #[test]
    fn a_complete_service_snapshot_keeps_from_the_rows_it_replaces() {
        // A resnapshot (restart, resync) is native's replace_rows: its rows
        // keep from the rows before it, in key order.
        let (pump, _tap) = service_pump();
        put(
            &pump,
            "run:b",
            asked("codex/p/one", "c1", Some((3, &["q1", "q2", "q3"]))),
        );
        put(
            &pump,
            "run:c",
            asked("codex/p/one", "c1", Some((1, &["q4"]))),
        );
        pump.push(Change::Replace {
            computer: ComputerId(String::new()),
            rows: vec![("run:d".into(), asked("codex/p/one", "c1", None))],
        });
        assert_eq!(
            questions(&pump.row("", "run:d").unwrap()),
            (
                Some(3),
                3,
                vec!["q1".to_owned(), "q2".to_owned(), "q3".to_owned()]
            )
        );
        // The replaced rows leave the index; the kept row is known again.
        assert_eq!(pump.question_index_len(""), 1);
    }

    #[test]
    fn the_question_index_names_at_most_one_key_per_current_row() {
        let (pump, _tap) = service_pump();
        for round in 0..3 {
            for n in 0..20 {
                put(
                    &pump,
                    &format!("run:{n}"),
                    asked(&format!("codex/p/{n}"), "c1", Some((round, &[]))),
                );
            }
        }
        assert_eq!(pump.question_index_len(""), 20);
        // A row that moves to an unknown conversation leaves its old place
        // (an unknown row of the same conversation keeps its count and stays);
        // a removal leaves too.
        put(&pump, "run:0", asked("codex/p/0", "c2", None));
        put(&pump, "run:2", asked("codex/p/2", "c1", None));
        assert_eq!(questions(&pump.row("", "run:2").unwrap()).1, 2);
        pump.push(Change::Remove {
            computer: ComputerId(String::new()),
            key: "run:1".into(),
            reason: None,
        });
        assert_eq!(pump.question_index_len(""), 18);
        // A new service scope (reconnect) keeps the rows and their index.
        pump.notification_scope(
            "",
            crate::desktop_service::NotificationScope::for_test("other"),
        );
        assert_eq!(pump.question_index_len(""), 18);
    }
}
