//! The reads behind the Overview, Machines and Accounts screens, and the
//! Projects screen: one hub that knows which screens are visible, runs the
//! due `hgs` reads off the main thread (`zerus_client_core::reads`, bounded
//! and timed out there), keeps the replies and hands the webview projected
//! view models (`zerus_client_core::views`). Which read runs when is decided
//! in `views::schedule`; this file wires the clock, the threads and the event.
//!
//! Every command answers at once with the current view and starts what is
//! due in the background. A screen's reads run only while it shows, or on an
//! explicit Refresh, selection or "Test connection"; hidden, only Qt's
//! account warm-up runs (one `account ls` per newly listed host). Projects
//! reads nothing: [`Screens::organization`] hands it the organization the
//! Sessions list groups by, with the write controller's status line. A
//! changed view emits `screens-changed` with the screen's name; the frontend
//! then pulls it again. A replay reads only with an explicit
//! `ZERUS_CLIENT_HGS`, as the actions do.
//!
//! New session reads one computer's accounts on the same hub
//! (`launch_accounts`): its `account ls` and the limits of its profiles go
//! through the hub's catalogs and usage queue, never a second store. The open
//! dialog's computer is watched until `launch_accounts_close`: only the first
//! catalog filed after the dialog's ask answers it (Qt filters its replies by
//! `m_accountRequest`), its rows are grouped when that answer is good, and
//! later catalogs and limits only refresh them, as Qt's dialog does; each of
//! its computer's replies emits `launch-accounts-changed` with the computer,
//! and the dialog pulls its rows again (`launch_account_rows`), as Qt's dialog
//! follows the usage store.
//!
//! The one-second ticker does no per-profile work: it compares the fleet and
//! steps the account reads only when the fleet changed, a catalog arrived, an
//! account screen showed or the earliest due time passed. A view is
//! projected again only after a read finished, the fleet changed, or account
//! reads started while Overview or Accounts shows (their busy state).

pub mod account_edits;
pub mod machine_setup;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::State;
use zerus_client_core::launch;
use zerus_client_core::paths::{self, Style};
use zerus_client_core::reads::{self, Read, ReadError};
use zerus_client_core::views::accounts::{
    self, Account, AccountsDisplay, AccountsState, AccountsView, Profile, UsageRef,
};
use zerus_client_core::views::machines::{MachinesState, MachinesView};
use zerus_client_core::views::projects::{ProjectsView, SwarmState};
use zerus_client_core::views::schedule::{Screen, USAGE_INTERVAL};
use zerus_client_core::views::session_usage::{SessionUsageRef, SessionUsageState};
use zerus_client_core::views::usage::Usage;
use zerus_client_core::views::FleetEntry;
use zerus_client_core::ComputerId;

/// What a replay without an explicit `hgs` shows instead of reading.
pub const REPLAY_NEEDS_HGS: &str = "Reads in replay mode need ZERUS_CLIENT_HGS.";

/// The screens that show the accounts view.
const ACCOUNT_SCREENS: [Screen; 2] = [Screen::Overview, Screen::Accounts];
/// The screens whose view follows the fleet.
const FLEET_SCREENS: [Screen; 3] = [Screen::Overview, Screen::Projects, Screen::Accounts];
const PROJECTION_FAILED: &str = "Could not update this screen.";
const READ_FAILED: &str = "Could not refresh this screen.";

/// Existing view fields plus their computation order under the hub lock.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revisioned<T> {
    #[serde(flatten)]
    pub value: T,
    pub revision: u64,
}

impl<T> std::ops::Deref for Revisioned<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// One read, run off the UI thread.
pub type Job = Box<dyn FnOnce() + Send>;
pub type Spawn = Arc<dyn Fn(Job) + Send + Sync>;
pub type Notify = Arc<dyn Fn(Screen) + Send + Sync>;
pub type FleetSource = Arc<dyn Fn() -> Vec<FleetEntry> + Send + Sync>;
/// Milliseconds since the hub started.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
/// Told when a reply for New session's computer was filed.
pub type LaunchNotify = Arc<dyn Fn(&ComputerId) + Send + Sync>;

/// The most profiles of one computer New session lists.
const LAUNCH_PROFILES: usize = 256;
/// How long `launch_accounts` waits for a catalog read: its 25 s limit and
/// the read helper's margin.
const LAUNCH_WAIT: Duration = Duration::from_secs(40);

/// New session's account rows of one computer (`NewSessionDialog::loadAccounts`
/// and `updateAccounts`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LaunchAccounts {
    /// A catalog reply (good or failed) was filed since the dialog chose
    /// this computer.
    pub loaded: bool,
    /// "Could not load accounts: %1" for the last reply's failure (Qt's
    /// `failureDetail`); the rows stay those of the last good catalog since
    /// the dialog chose this computer, none without one.
    pub error: Option<String>,
    /// The catalog's raw rows, at most 256, for the OpenCode fields.
    pub profiles: Vec<Value>,
    /// `views::accounts::group` over its launchable profiles, each member with
    /// the dialog's usage and each group with its members' best.
    pub groups: Vec<Account>,
}

/// The open dialog's computer (`m_accountHost` with `m_accounts`,
/// `m_accountGroups` and the status line).
struct LaunchWatch {
    computer: ComputerId,
    /// The dialog's agents in Qt's order: its requested agent joins them.
    agents: Vec<String>,
    /// A catalog reply was filed since the dialog chose this computer.
    replied: bool,
    error: Option<String>,
    /// The last good catalog's groups and the folder style they were made
    /// for; later limits refresh them, the next catalog or style regroups.
    groups: Option<(Style, Vec<Account>)>,
    /// `launch_accounts` calls still waiting for this computer's catalog: they
    /// ask for its limits themselves once they wake.
    waiting: usize,
    /// The reply that answers the last ask (`launch_start`) was filed: the
    /// first one since. Later replies only update the shared store (Qt's
    /// `m_accountRequest`, NewSessionDialog.cpp:385).
    answered: bool,
    /// The last ask was Refresh and its limits were not asked yet: taken
    /// when they are, so it forces them once (`std::exchange(m_forceUsage,
    /// false)`, 387).
    force: bool,
}

impl LaunchWatch {
    fn agents(&self) -> Vec<&str> {
        self.agents.iter().map(String::as_str).collect()
    }
}

/// The dialog's agents (221-222): `launch::AGENTS`, then a requested agent
/// that is neither among them nor the shell.
fn launch_agents(agent: Option<&str>) -> Vec<String> {
    let mut agents: Vec<String> = launch::AGENTS.iter().map(|a| (*a).to_owned()).collect();
    if let Some(agent) = agent.filter(|a| !a.is_empty() && *a != "sh") {
        if !agents.iter().any(|a| a == agent) {
            agents.push(agent.to_owned());
        }
    }
    agents
}

/// A computer's folder style (`folderStyle`, NewSessionDialog.cpp:108-110).
fn style_of(entry: &FleetEntry) -> Style {
    paths::of(entry.platform.as_deref(), entry.computer.is_local())
}

/// The watched computer's fleet entry. One outside the fleet (its stream
/// ended, or no computer of the fleet is called so) is a peer whose stream
/// ended: its folders are POSIX and its limits are not read.
fn launch_entry(hub: &Hub, computer: &ComputerId) -> FleetEntry {
    hub.fleet
        .iter()
        .find(|entry| entry.computer == *computer)
        .cloned()
        .unwrap_or_else(|| FleetEntry {
            computer: computer.clone(),
            status: "ended".into(),
            attempt: 0,
            local_sessions: true,
            local_host: false,
            host: None,
            platform: None,
        })
}

/// The hosts whose account reads the hub keeps (the warm-up and pruning):
/// every machine that keeps accounts, and New session's computer while the
/// dialog watches it, which Qt reads directly whatever the Accounts page lists
/// (865-874). The page polls only [`account_hosts`].
fn hub_hosts(hub: &Hub) -> Vec<ComputerId> {
    let mut hosts = account_hosts(&hub.fleet);
    if let Some(watch) = &hub.launch {
        if !hosts.contains(&watch.computer) {
            hosts.push(watch.computer.clone());
        }
    }
    hosts
}

/// The launch profiles of the watched computer's current catalog.
fn launch_members(hub: &Hub, watch: &LaunchWatch) -> Option<(FleetEntry, Vec<Profile>)> {
    let entry = launch_entry(hub, &watch.computer);
    let data = hub.accounts.catalogs.get(&watch.computer)?.data.as_ref()?;
    let members = accounts::host_profiles(&entry, data, &hub.accounts, &watch.agents())
        .into_iter()
        .map(|(profile, _)| profile)
        .collect();
    Some((entry, members))
}

#[derive(Default)]
struct Hub {
    visible: BTreeSet<Screen>,
    fleet: Vec<FleetEntry>,
    accounts: AccountsState,
    display: AccountsDisplay,
    machines: MachinesState,
    swarm: SwarmState,
    /// The selected session's account usage and the view the webview last saw.
    session_usage: SessionUsageState,
    session_shown: Option<Value>,
    /// The view each visible screen last showed, so a change emits once.
    shown: BTreeMap<Screen, Value>,
    /// Computation order, independent of which IPC request finishes last.
    view_revision: u64,
    /// Screens a reply, the fleet or starting account reads touched since the
    /// last publish.
    dirty: BTreeSet<Screen>,
    /// The account reads need a fresh look: the fleet changed, a catalog
    /// arrived or an account screen showed.
    stale: bool,
    /// When the next catalog or inspect falls due, while an account screen shows.
    next_due: Option<u64>,
    /// The open New session dialog's computer.
    launch: Option<LaunchWatch>,
    #[cfg(test)]
    work: Work,
}

/// What the hub did so far: account steps and projections for an event.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Work {
    steps: usize,
    projections: usize,
}

impl Hub {
    fn accounts_shown(&self) -> bool {
        ACCOUNT_SCREENS
            .iter()
            .any(|screen| self.visible.contains(screen))
    }

    /// Takes the fleet as the pump has it now. A change steps the account
    /// reads again and re-projects the screens that show the fleet.
    fn follow(&mut self, fleet: Vec<FleetEntry>) {
        if self.fleet != fleet {
            self.fleet = fleet;
            self.stale = true;
            self.dirty.extend(FLEET_SCREENS);
        }
    }

    /// The queued inspects that start now: at most two in flight, each on a
    /// host that is still reachable. The selected session goes first.
    fn dispatch(&mut self) -> Vec<Run> {
        let Self {
            fleet,
            accounts: state,
            session_usage,
            ..
        } = self;
        let reachable = |host: &ComputerId| {
            fleet
                .iter()
                .any(|entry| &entry.computer == host && accounts::reachable(entry))
        };
        let mut runs: Vec<Run> = session_usage
            .dispatch(&reachable, state)
            .into_iter()
            .map(Run::SessionUsage)
            .collect();
        runs.extend(state.dispatch(&reachable).into_iter().map(Run::Usage));
        runs
    }
}

type SessionNotify = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    edits: account_edits::Edits,
    hub: Mutex<Hub>,
    hgs: PathBuf,
    allowed: bool,
    spawn: Spawn,
    notify: Notify,
    fleet: FleetSource,
    clock: Clock,
    /// Woken whenever an `account ls` reply is filed.
    catalog_done: Condvar,
    launch_notify: OnceLock<LaunchNotify>,
    session_notify: OnceLock<SessionNotify>,
    #[cfg(test)]
    panic_projection: std::sync::atomic::AtomicU8,
    #[cfg(test)]
    panic_read: std::sync::atomic::AtomicBool,
}

/// The managed state of the screen commands; clones share it.
#[derive(Clone)]
pub struct Screens(Arc<Shared>);

/// Existing Machines page admission, held across an explicit local action.
struct MachineWork(Screens);
impl Drop for MachineWork {
    fn drop(&mut self) {
        let mut hub = self.0.lock();
        hub.machines.finish_write();
        hub.dirty.insert(Screen::Machines);
        drop(hub);
        self.0.publish();
    }
}

/// A read the hub decided to start.
#[derive(Clone)]
enum Run {
    Catalog(ComputerId),
    Usage(UsageRef),
    SessionUsage(SessionUsageRef),
    Machines,
    Resolve(String),
    Check(String),
}

/// Every machine that keeps accounts. The hub always passes this full list:
/// the account state forgets every host it is not given.
fn account_hosts(fleet: &[FleetEntry]) -> Vec<ComputerId> {
    accounts::hosts(fleet)
        .into_iter()
        .map(|host| host.computer)
        .collect()
}

fn message(reply: Result<Value, ReadError>) -> Result<Value, String> {
    reply.map_err(|error| error.to_string())
}

/// Records a view the webview now holds for the visible screens that show it.
fn remember<T: Serialize>(hub: &mut Hub, screens: &[Screen], view: &T) -> u64 {
    if let Ok(value) = serde_json::to_value(view) {
        for screen in screens {
            if hub.visible.contains(screen) {
                hub.shown.insert(*screen, value.clone());
            }
        }
    }
    hub.view_revision += 1;
    hub.view_revision
}

impl Screens {
    pub fn new(
        hgs: PathBuf,
        allowed: bool,
        spawn: Spawn,
        notify: Notify,
        fleet: FleetSource,
        clock: Clock,
    ) -> Self {
        Self(Arc::new(Shared {
            hub: Mutex::new(Hub::default()),
            edits: account_edits::Edits::default(),
            hgs,
            allowed,
            spawn,
            notify,
            fleet,
            clock,
            catalog_done: Condvar::new(),
            launch_notify: OnceLock::new(),
            session_notify: OnceLock::new(),
            #[cfg(test)]
            panic_projection: std::sync::atomic::AtomicU8::new(0),
            #[cfg(test)]
            panic_read: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Where New session hears of its computer's replies; set once.
    pub fn on_launch_change(&self, notify: LaunchNotify) {
        let _ = self.0.launch_notify.set(notify);
    }

    /// Where the composer hears that its session's usage changed; set once.
    pub fn on_session_usage_change(&self, notify: SessionNotify) {
        let _ = self.0.session_notify.set(notify);
    }

    /// The app's hub: `default_hgs()` on Tauri's blocking pool, the fleet from
    /// the pump's mirror, `screens-changed` to the webview, and a ticker.
    pub fn tauri(app: tauri::AppHandle, pump: crate::pump::Pump) -> Self {
        use tauri::Emitter;
        let var = |name: &str| std::env::var(name).ok();
        let allowed = crate::actions::actions_allowed(
            var("ZERUS_CLIENT_REPLAY").as_deref(),
            var("ZERUS_CLIENT_HGS").as_deref(),
        );
        let started = Instant::now();
        let launch_app = app.clone();
        let session_app = app.clone();
        let screens = Self::new(
            zerus_client_core::default_hgs(),
            allowed,
            Arc::new(|job: Job| {
                tauri::async_runtime::spawn_blocking(job);
            }),
            Arc::new(move |screen: Screen| {
                let _ = app.emit("screens-changed", screen.name());
            }),
            Arc::new(move || pump.fleet()),
            Arc::new(move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        screens.on_launch_change(Arc::new(move |computer: &ComputerId| {
            let _ = launch_app.emit("launch-accounts-changed", computer.0.clone());
        }));
        screens.on_session_usage_change(Arc::new(move || {
            let _ = session_app.emit("session-usage-changed", ());
        }));
        screens.start_ticker();
        screens
    }

    fn lock(&self) -> MutexGuard<'_, Hub> {
        self.0.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> u64 {
        (self.0.clock)()
    }

    /// The hub, following the fleet as the pump has it now.
    fn current(&self) -> MutexGuard<'_, Hub> {
        let fleet = (self.0.fleet)();
        let mut hub = self.lock();
        hub.follow(fleet);
        hub
    }

    fn start_ticker(&self) {
        let screens = self.clone();
        std::thread::Builder::new()
            .name("zerus-screens".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                screens.tick();
            })
            .expect("the screens ticker starts");
    }

    /// One policy step, every second: follow the fleet, keep the warm-up and
    /// the visible screens' reads due, report changed views. With nothing
    /// changed and nothing due it costs a fleet comparison.
    pub fn tick(&self) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tick_once())).is_err() {
            eprintln!("zerus-client: screens tick panicked");
        }
    }

    fn tick_once(&self) {
        let runs = {
            let now = self.now();
            let mut hub = self.current();
            let mut runs = self.accounts_due(&mut hub, now, false);
            runs.extend(self.session_due(&mut hub, now, false));
            runs
        };
        self.run(runs);
        self.publish();
        self.publish_session_usage();
    }

    /// The selected session's usage read, due on the same five-minute clock
    /// and two-slot dispatch as the account screens.
    fn session_due(&self, hub: &mut Hub, now: u64, force: bool) -> Vec<Run> {
        if !self.0.allowed {
            return Vec::new();
        }
        let online = hub.session_usage.current().is_some_and(|target| {
            zerus_client_core::views::session_usage::online(&hub.fleet, &target.host)
        });
        hub.session_usage.ensure(now, online, force);
        hub.dispatch()
    }

    /// The account reads that start now: one `account ls` per newly listed
    /// host always, and while Overview or Accounts shows (or on Refresh,
    /// `force`) the catalogs and inspects due. Looks only when something may
    /// have changed: the fleet, a catalog, a shown screen or the due time.
    fn accounts_due(&self, hub: &mut Hub, now: u64, force: bool) -> Vec<Run> {
        let due = force || hub.stale || hub.next_due.is_some_and(|at| now >= at);
        if !self.0.allowed || !due {
            return Vec::new();
        }
        #[cfg(test)]
        {
            hub.work.steps += 1;
        }
        hub.stale = false;
        hub.next_due = None;
        let hosts = hub_hosts(hub);
        let mut runs: Vec<Run> = hub
            .accounts
            .warm(&hosts, now)
            .into_iter()
            .map(Run::Catalog)
            .collect();
        let shown = hub.accounts_shown();
        if shown || force {
            // The page's own hosts only: New session's computer outside them is
            // read on the dialog's asks, never every 30 s, and a read of it
            // never holds back the page's Refresh.
            let listed = account_hosts(&hub.fleet);
            runs.extend(
                hub.accounts
                    .poll_catalogs(&listed, now, force)
                    .into_iter()
                    .map(Run::Catalog),
            );
            let refs = accounts::refs(&hub.fleet, &hub.accounts);
            for (r, reachable) in &refs {
                hub.accounts.ensure_usage(r, *reachable, now, force);
            }
            if shown {
                hub.next_due = hub.accounts.next_due(&listed, &refs);
            }
        }
        runs.extend(hub.dispatch());
        // Reads that start show at once (AccountsPage::request calls
        // updateRefreshState): the busy state, "Updating… N remaining" and a
        // disabled Refresh. A step that starts nothing publishes nothing.
        if !runs.is_empty() && shown {
            hub.dirty.extend(ACCOUNT_SCREENS);
        }
        runs
    }

    fn run(&self, runs: impl IntoIterator<Item = Run>) {
        for job in runs {
            let screens = self.clone();
            (self.0.spawn)(Box::new(move || screens.read_guarded(job)));
        }
    }

    /// Runs one read (blocking, off the UI thread), files its reply and
    /// starts what it made due.
    fn read_guarded(&self, job: Run) {
        let recovery = job.clone();
        let mut settled = false;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.read(job, &mut settled, false)
        }))
        .is_err()
        {
            eprintln!("zerus-client: screen read worker panicked");
            // A notification can panic after the reply was filed and a newer
            // read started. Never clear that newer read's slot.
            if !settled {
                self.read_failed(recovery);
            }
        }
    }

    fn read(&self, job: Run, settled: &mut bool, failed: bool) {
        #[cfg(test)]
        if self
            .0
            .panic_read
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            panic!("injected read panic");
        }
        let hgs = &self.0.hgs;
        let local = ComputerId::local();
        let mut heard: Option<ComputerId> = None;
        let runs = match job {
            Run::Catalog(host) => {
                // Qt's requestAccounts texts, for the Accounts page as for New session.
                let reply = if failed {
                    Err(READ_FAILED.into())
                } else {
                    launch::read_accounts(hgs, &host)
                };
                let now = if failed {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.now()))
                        .unwrap_or(0)
                } else {
                    self.now()
                };
                let (runs, computer) = self.catalog_reply(host, reply, now, false);
                heard = computer;
                runs
            }
            Run::Usage(r) => {
                let reply = if failed {
                    None
                } else {
                    reads::read(hgs, &r.host, &Read::AccountUsage { id: r.id.clone() }).ok()
                };
                let mut hub = self.lock();
                hub.accounts.finish_usage(&r, reply);
                if hub
                    .launch
                    .as_ref()
                    .is_some_and(|watch| watch.computer == r.host)
                {
                    heard = Some(r.host.clone());
                }
                // The profile falls due again five minutes after this attempt.
                let attempted = hub
                    .accounts
                    .usage
                    .get(&r.key())
                    .and_then(|slot| slot.attempted);
                if let Some(at) = attempted.filter(|_| hub.accounts_shown()) {
                    let at = at.saturating_add(USAGE_INTERVAL);
                    hub.next_due = Some(hub.next_due.map_or(at, |due| due.min(at)));
                }
                hub.dirty.extend(ACCOUNT_SCREENS);
                hub.dispatch()
            }
            Run::SessionUsage(target) => {
                let reply = if failed {
                    None
                } else {
                    reads::read(
                        hgs,
                        &target.host,
                        &Read::SessionAccountUsage {
                            name: target.name.clone(),
                        },
                    )
                    .ok()
                };
                let mut hub = self.lock();
                let Hub {
                    session_usage,
                    accounts,
                    ..
                } = &mut *hub;
                session_usage.finish(&target, reply, accounts);
                hub.dispatch()
            }
            Run::Machines => {
                let reply = if failed {
                    Err(READ_FAILED.into())
                } else {
                    message(reads::read(hgs, &local, &Read::Machines))
                };
                let mut hub = self.lock();
                hub.machines.finish_list(reply);
                hub.dirty.insert(Screen::Machines);
                Vec::new()
            }
            Run::Resolve(alias) => {
                let read = Read::MachineResolve {
                    alias: alias.clone(),
                };
                let reply = if failed {
                    Err(READ_FAILED.into())
                } else {
                    message(reads::read(hgs, &local, &read))
                };
                let mut hub = self.lock();
                hub.machines.finish_resolve(&alias, reply);
                hub.dirty.insert(Screen::Machines);
                Vec::new()
            }
            Run::Check(alias) => {
                let read = Read::MachineCheck {
                    alias: alias.clone(),
                };
                let reply = if failed {
                    Err(READ_FAILED.into())
                } else {
                    message(reads::read(hgs, &local, &read))
                };
                let mut hub = self.lock();
                hub.machines.finish_check(&alias, reply);
                hub.dirty.insert(Screen::Machines);
                Vec::new()
            }
        };
        *settled = true;
        self.run(runs);
        self.publish();
        self.publish_session_usage();
        if let (Some(computer), Some(notify)) = (heard, self.0.launch_notify.get()) {
            notify(&computer);
        }
    }

    /// A list read and a catalog mutation complete through the same path:
    /// New session waiters, queued refreshes and usage reads all hear it.
    fn catalog_reply(
        &self,
        host: ComputerId,
        reply: Result<Value, String>,
        now: u64,
        mutation: bool,
    ) -> (Vec<Run>, Option<ComputerId>) {
        let failure = reply.as_ref().err().cloned();
        let mut heard = None;
        let mut limits = Vec::new();
        let mut hub = self.lock();
        // A queued follow-up is the page's (its forced poll), for a host it lists.
        let listed = account_hosts(&hub.fleet);
        let previous_error = hub
            .accounts
            .catalogs
            .get(&host)
            .and_then(|slot| slot.error.clone());
        let again = hub.accounts.finish_catalog(&host, reply, now, &listed);
        // Qt reports a failed edit through its notice bus, without turning
        // the retained catalog into a failed list read. Dialog waiters still
        // receive the failure and cannot launch from an unconfirmed reply.
        if mutation && failure.is_some() {
            if let Some(slot) = hub.accounts.catalogs.get_mut(&host) {
                slot.error = previous_error;
            }
        }
        hub.stale = true;
        hub.dirty.extend(ACCOUNT_SCREENS);
        if hub
            .launch
            .as_ref()
            .is_some_and(|watch| watch.computer == host)
        {
            heard = Some(host.clone());
        }
        // Only the first reply since the dialog's ask answers it; a later
        // one, such as the page's poll, only updated the shared store.
        if hub
            .launch
            .as_ref()
            .is_some_and(|watch| watch.computer == host && !watch.answered)
        {
            let grouped = match &failure {
                // Grouped when the catalog arrives (`m_regroupAccounts`, 380-386, 883).
                None => hub.launch.as_ref().and_then(|watch| {
                    let (entry, members) = launch_members(&hub, watch)?;
                    Some((style_of(&entry), accounts::group(&members)))
                }),
                Some(_) => None,
            };
            let mut late: Option<bool> = None;
            if let Some(watch) = hub.launch.as_mut() {
                watch.replied = true;
                watch.answered = true;
                match failure {
                    None => {
                        watch.error = None;
                        watch.groups = grouped;
                        // No `launch_accounts` waits for it any more: the
                        // catalog arrived late, and Qt's accountsReady
                        // asks for its limits (380-388), forced once.
                        if watch.waiting == 0 {
                            late = Some(std::mem::take(&mut watch.force));
                        }
                    }
                    Some(detail) => {
                        watch.error = Some(format!("Could not load accounts: {detail}"));
                    }
                }
            }
            if let Some(force) = late {
                limits = self.launch_limits(&mut hub, &host, force);
            }
        }
        let mut runs: Vec<Run> = again.then_some(Run::Catalog(host)).into_iter().collect();
        runs.append(&mut limits);
        runs.extend(self.accounts_due(&mut hub, now, false));
        self.0.catalog_done.notify_all();
        (runs, heard)
    }

    /// A panic files a failed reply through the existing completion path,
    /// including New session's catalog wake and usage notifications. No CLI
    /// call is made on this path.
    fn read_failed(&self, job: Run) {
        self.read(job, &mut false, true);
    }

    /// One `screens-changed` per visible screen whose view changed since the
    /// webview last saw it. Only the screens a reply, the fleet or starting
    /// account reads touched are projected again.
    fn publish(&self) {
        let changed: Vec<Screen> = {
            let mut hub = self.lock();
            let mut changed = Vec::new();
            for screen in std::mem::take(&mut hub.dirty) {
                if !hub.visible.contains(&screen) {
                    continue;
                }
                let value = self.view_of(&mut hub, screen);
                if hub.shown.get(&screen) != Some(&value) {
                    hub.shown.insert(screen, value);
                    changed.push(screen);
                }
            }
            changed
        };
        for screen in changed {
            (self.0.notify)(screen);
        }
    }

    fn session_usage_of(&self, hub: &Hub) -> Usage {
        let online = hub.session_usage.current().is_some_and(|target| {
            zerus_client_core::views::session_usage::online(&hub.fleet, &target.host)
        });
        hub.session_usage.view(online)
    }

    fn publish_session_usage(&self) {
        let notify = {
            let mut hub = self.lock();
            let value = serde_json::to_value(self.session_usage_of(&hub)).unwrap_or(Value::Null);
            if hub.session_shown.as_ref() == Some(&value) {
                None
            } else {
                hub.session_shown = Some(value);
                self.0.session_notify.get().cloned()
            }
        };
        if let Some(notify) = notify {
            notify();
        }
    }

    /// Follows one live session, or None to stop. `force` is Refresh usage.
    /// The answer carries the current cached values while any read runs.
    pub fn session_usage(&self, target: Option<SessionUsageRef>, force: bool) -> Usage {
        let runs = {
            let now = self.now();
            let mut hub = self.current();
            hub.session_usage.follow(target);
            self.session_due(&mut hub, now, force)
        };
        self.run(runs);
        let mut hub = self.lock();
        let view = self.session_usage_of(&hub);
        hub.session_shown = serde_json::to_value(&view).ok();
        view
    }

    fn view_of(&self, hub: &mut Hub, screen: Screen) -> Value {
        #[cfg(test)]
        {
            hub.work.projections += 1;
        }
        match screen {
            Screen::Overview | Screen::Accounts => serde_json::to_value(self.accounts_of(hub)),
            Screen::Machines => serde_json::to_value(self.machines_of(hub)),
            Screen::Projects => serde_json::to_value(self.projects_of(hub)),
        }
        .unwrap_or(Value::Null)
    }

    fn project_or<T>(
        &self,
        screen: Screen,
        project: impl FnOnce() -> T,
        failed: impl FnOnce() -> T,
    ) -> T {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if self
                .0
                .panic_projection
                .load(std::sync::atomic::Ordering::Relaxed)
                == screen as u8 + 1
            {
                panic!("injected projection panic");
            }
            project()
        }))
        .unwrap_or_else(|_| {
            eprintln!("zerus-client: {:?} screen projection panicked", screen);
            failed()
        })
    }

    fn accounts_of(&self, hub: &mut Hub) -> AccountsView {
        let mut view = self.project_or(
            Screen::Accounts,
            || accounts::project(&hub.fleet, &hub.accounts, &mut hub.display),
            || AccountsView {
                snapshot: true,
                notices: vec![PROJECTION_FAILED.into()],
                ..Default::default()
            },
        );
        if !self.0.allowed {
            view.notices.insert(0, REPLAY_NEEDS_HGS.into());
            view.pending = 0;
            view.busy = false;
            view.snapshot = true;
        }
        view
    }

    fn machines_of(&self, hub: &Hub) -> MachinesView {
        let mut view = self.project_or(
            Screen::Machines,
            || hub.machines.view(),
            || MachinesView {
                error: Some(PROJECTION_FAILED.into()),
                ..Default::default()
            },
        );
        if !self.0.allowed {
            view.error = Some(REPLAY_NEEDS_HGS.into());
        }
        view
    }

    fn projects_of(&self, hub: &Hub) -> ProjectsView {
        let peers: Vec<String> = hub
            .fleet
            .iter()
            .filter_map(|entry| entry.computer.alias().map(str::to_owned))
            .collect();
        // A blocked replay keeps the controller's line: Projects reads
        // nothing, and the organization's own `ready` blocks its edits.
        self.project_or(
            Screen::Projects,
            || hub.swarm.view(&peers),
            || ProjectsView {
                status: PROJECTION_FAILED.into(),
                status_error: true,
                ..Default::default()
            },
        )
    }

    /// A screen shown or hidden. Showing Machines reads the list; Overview
    /// and Accounts make the account reads due; Projects reads nothing.
    /// Hiding both account screens drops their queued reads; reads already
    /// running finish.
    pub fn set_visible(&self, screen: Screen, visible: bool) {
        let runs = {
            let now = self.now();
            let mut hub = self.current();
            let mut runs = Vec::new();
            if visible {
                if hub.visible.insert(screen) && self.0.allowed {
                    match screen {
                        Screen::Machines => {
                            if hub.machines.start_list(now) {
                                runs.push(Run::Machines);
                            }
                        }
                        Screen::Overview | Screen::Accounts => hub.stale = true,
                        Screen::Projects => {}
                    }
                }
            } else if hub.visible.remove(&screen) {
                hub.shown.remove(&screen);
                if ACCOUNT_SCREENS.contains(&screen) && !hub.accounts_shown() {
                    hub.accounts.cancel_queued();
                    hub.next_due = None;
                }
            }
            runs.extend(self.accounts_due(&mut hub, now, false));
            runs
        };
        self.run(runs);
        self.publish();
    }

    /// The accounts view. `force` is Refresh: forced `ls` and inspects,
    /// ignored while account reads run (Qt's `reload`). A pull while neither
    /// Overview nor Accounts shows starts nothing.
    pub fn accounts(&self, force: bool) -> Revisioned<AccountsView> {
        let runs = {
            let now = self.now();
            let mut hub = self.current();
            if force && !hub.accounts.reading() {
                self.accounts_due(&mut hub, now, true)
            } else if !force && hub.accounts_shown() {
                self.accounts_due(&mut hub, now, false)
            } else {
                Vec::new()
            }
        };
        self.run(runs);
        let mut hub = self.lock();
        let view = self.accounts_of(&mut hub);
        let revision = remember(&mut hub, &ACCOUNT_SCREENS, &view);
        Revisioned {
            value: view,
            revision,
        }
    }

    /// Sessions' account picker refreshes catalog bindings only. It neither
    /// pretends the Accounts screen is visible nor queues profile inspects.
    pub fn filter_accounts(&self, force: bool) -> Revisioned<AccountsView> {
        if force && self.0.allowed {
            let mut hub = self.current();
            let hosts = account_hosts(&hub.fleet);
            let now = self.now();
            let runs: Vec<Run> = hosts
                .into_iter()
                .filter_map(|host| {
                    hub.accounts
                        .ensure_catalog(&host, now)
                        .then_some(Run::Catalog(host))
                })
                .collect();
            drop(hub);
            self.run(runs);
        }
        let mut hub = self.current();
        let view = self.accounts_of(&mut hub);
        let revision = remember(&mut hub, &ACCOUNT_SCREENS, &view);
        Revisioned {
            value: view,
            revision,
        }
    }

    fn machines_answer(&self) -> Revisioned<MachinesView> {
        let mut hub = self.lock();
        let view = self.machines_of(&hub);
        let revision = remember(&mut hub, &[Screen::Machines], &view);
        Revisioned {
            value: view,
            revision,
        }
    }

    /// The Machines view; `force` is Refresh (skipped while a list read or a
    /// check runs). The list has no poll, so a plain pull starts nothing.
    pub fn machines(&self, force: bool) -> Revisioned<MachinesView> {
        let start = force && self.0.allowed && {
            let now = self.now();
            self.lock().machines.start_list(now)
        };
        if start {
            self.run([Run::Machines]);
        }
        self.machines_answer()
    }

    /// One explicit local profile write. The existing CLI owns the config file
    /// lock/revision; this hub only serializes the page's admission.
    pub fn change_machine(
        &self,
        request: zerus_client_core::machine_connections::Request,
    ) -> Result<(), String> {
        use zerus_client_core::machine_connections;
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let call = {
            let mut hub = self.lock();
            let list = hub
                .machines
                .list
                .as_ref()
                .ok_or(machine_connections::CHANGED)?;
            let call = machine_connections::prepare(list, &request)?;
            if !hub.machines.start_write() {
                return Err("A machine request is already running.".into());
            }
            hub.dirty.insert(Screen::Machines);
            call
        };
        let _writing = MachineWork(self.clone());
        self.publish();
        let exit = zerus_client_core::run_limited(
            &self.0.hgs,
            &call,
            &[],
            machine_connections::RESPONSE_BYTES,
        )
        .map_err(|_| machine_connections::UNCERTAIN.to_owned())?;
        machine_connections::checked(exit, &request.alias)?;
        let reply = message(reads::read(
            &self.0.hgs,
            &ComputerId::local(),
            &Read::Machines,
        ));
        self.lock().machines.finish_list(reply);
        Ok(())
    }

    /// Open the saved alias in the configured terminal, including offline
    /// aliases whose host key or login needs manual repair. No remote probe.
    pub fn ssh_machine(
        &self,
        reference: zerus_client_core::machine_connections::Reference,
        terminal: &mut zerus_client_core::terminal_launch::Launcher,
    ) -> Result<Option<String>, String> {
        use zerus_client_core::machine_connections;
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        {
            let mut hub = self.lock();
            let list = hub
                .machines
                .list
                .as_ref()
                .ok_or(machine_connections::CHANGED)?;
            machine_connections::ssh_arguments(list, &reference)?;
            if !hub.machines.start_write() {
                return Err("A machine request is already running.".into());
            }
            hub.dirty.insert(Screen::Machines);
        }
        let _work = MachineWork(self.clone());
        self.publish();
        let fresh = message(reads::read(
            &self.0.hgs,
            &ComputerId::local(),
            &Read::Machines,
        ))?;
        let args = machine_connections::ssh_arguments(&fresh, &reference)?;
        terminal.open_program(&self.0.hgs.to_string_lossy(), &args)
    }

    /// The selected peer's effective connection, read on selection.
    pub fn resolve(&self, alias: String) -> Revisioned<MachinesView> {
        let start = self.0.allowed && self.lock().machines.start_resolve(&alias);
        if start {
            self.run([Run::Resolve(alias)]);
        }
        self.machines_answer()
    }

    /// "Test connection" (`machine check`, up to 25 s): never while the list
    /// reads or another check runs.
    pub fn check(&self, alias: String) -> Revisioned<MachinesView> {
        let start = self.0.allowed && self.lock().machines.start_check(&alias);
        if start {
            self.run([Run::Check(alias)]);
        }
        self.machines_answer()
    }

    /// New session's computer (`loadAccounts`, 865-874): its `account ls`
    /// starts on every open, computer change and Refresh, sharing one that
    /// runs. Another computer, or a new dialog (not `refresh`), starts with no
    /// rows until a reply is filed (Qt clears `m_accounts`). A computer the
    /// Accounts page does not list is read too, as Qt's dialog reads the
    /// selected computer directly (865-874): a computer that cannot be read
    /// answers with hgs's error. The first catalog of the computer filed from
    /// now on (a shared one's too) answers this ask. The caller waits for the
    /// catalog until [`Self::launch_waited`].
    fn launch_start(&self, computer: &ComputerId, refresh: bool, agent: Option<&str>) -> Vec<Run> {
        let now = self.now();
        let mut hub = self.current();
        let agents = launch_agents(agent);
        match hub.launch.as_mut() {
            Some(watch) if refresh && watch.computer == *computer => {
                watch.agents = agents;
                watch.waiting += 1;
                watch.answered = false;
                watch.force = true;
            }
            _ => {
                hub.launch = Some(LaunchWatch {
                    computer: computer.clone(),
                    agents,
                    replied: false,
                    error: None,
                    groups: None,
                    waiting: 1,
                    answered: false,
                    force: refresh,
                })
            }
        }
        if hub.accounts.ensure_catalog(computer, now) {
            hub.dirty.extend(ACCOUNT_SCREENS);
            vec![Run::Catalog(computer.clone())]
        } else {
            Vec::new()
        }
    }

    /// The dialog closed: its computer is no longer watched, and its replies
    /// tell nobody.
    pub fn launch_close(&self) {
        let mut hub = self.lock();
        hub.launch = None;
        // The next step forgets its reads when the Accounts page lists no such computer.
        hub.stale = true;
    }

    /// A `launch_accounts` call stopped waiting: once its ask was answered
    /// successfully it asks for the limits of the catalog (`requestUsage`),
    /// taking the Refresh's force so that it applies once. A failed answer asks
    /// for none. A good reply filed after the last caller stopped waiting asks
    /// for them itself.
    fn launch_waited(&self, computer: &ComputerId) -> Vec<Run> {
        let mut hub = self.lock();
        let (force, failed) = match hub.launch.as_mut() {
            Some(watch) if watch.computer == *computer => {
                watch.waiting = watch.waiting.saturating_sub(1);
                if !watch.answered {
                    return Vec::new();
                }
                (std::mem::take(&mut watch.force), watch.error.is_some())
            }
            _ => return Vec::new(),
        };
        if failed {
            return Vec::new();
        }
        self.launch_limits(&mut hub, computer, force)
    }

    /// Waits while the computer's catalog reads, at most [`LAUNCH_WAIT`].
    fn launch_wait(&self, computer: &ComputerId) {
        let until = Instant::now() + LAUNCH_WAIT;
        let mut hub = self.lock();
        while hub
            .accounts
            .catalogs
            .get(computer)
            .is_some_and(|slot| slot.pending)
        {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            hub = self
                .0
                .catalog_done
                .wait_timeout(hub, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The limits of the computer's installed launch profiles
    /// (`requestUsage`): those not checked within five minutes, all on
    /// Refresh. Returns the inspects that start now.
    #[cfg(all(test, unix))]
    fn launch_usage(&self, computer: &ComputerId, force: bool) -> Vec<Run> {
        let mut hub = self.lock();
        self.launch_limits(&mut hub, computer, force)
    }

    /// [`Self::launch_usage`] on the locked hub.
    fn launch_limits(&self, hub: &mut Hub, computer: &ComputerId, force: bool) -> Vec<Run> {
        let now = self.now();
        let Some(watch) = hub
            .launch
            .as_ref()
            .filter(|watch| watch.computer == *computer)
        else {
            return Vec::new();
        };
        let Some((entry, members)) = launch_members(hub, watch) else {
            return Vec::new();
        };
        let reachable = accounts::reachable(&entry);
        for profile in members {
            if profile.installed {
                let r = UsageRef {
                    host: profile.host,
                    id: profile.id,
                    home: profile.home,
                    provider: profile.provider,
                    auth_revision: profile.auth_revision,
                };
                hub.accounts.ensure_usage(&r, reachable, now, force);
            }
        }
        hub.dirty.extend(ACCOUNT_SCREENS);
        hub.dispatch()
    }

    /// The watched computer's rows; reads nothing. Members and each row's
    /// best usage are current (`refreshUsage`, 990-1000); the grouping is the
    /// last good catalog's, made again only when the folder style changed
    /// (883). Another computer than the dialog's has none.
    pub fn launch_rows(&self, computer: &ComputerId) -> LaunchAccounts {
        let mut hub = self.lock();
        let Some(watch) = hub
            .launch
            .as_ref()
            .filter(|watch| watch.computer == *computer)
        else {
            return LaunchAccounts::default();
        };
        if !watch.replied {
            return LaunchAccounts::default();
        }
        let error = watch.error.clone();
        let Some((style, groups)) = watch.groups.as_ref() else {
            return LaunchAccounts {
                loaded: true,
                error,
                ..LaunchAccounts::default()
            };
        };
        let Some((entry, members)) = launch_members(&hub, watch) else {
            return LaunchAccounts {
                loaded: true,
                error,
                ..LaunchAccounts::default()
            };
        };
        let groups = if style_of(&entry) == *style {
            accounts::refresh_groups(groups, &members)
        } else {
            let regrouped = accounts::group(&members);
            if let Some(watch) = hub.launch.as_mut() {
                watch.groups = Some((style_of(&entry), regrouped.clone()));
            }
            regrouped
        };
        let profiles: Vec<Value> = hub
            .accounts
            .catalogs
            .get(computer)
            .and_then(|slot| slot.data.as_ref())
            .and_then(|data| data.get("profiles"))
            .and_then(Value::as_array)
            .map(|rows| rows.iter().take(LAUNCH_PROFILES).cloned().collect())
            .unwrap_or_default();
        LaunchAccounts {
            loaded: true,
            error,
            profiles,
            groups,
        }
    }

    /// New session's accounts of one computer (`loadAccounts` with
    /// `requestUsage`): reads its catalog (sharing a running read) and waits
    /// for it, then asks for its profiles' limits (forced once on `refresh`)
    /// and answers the rows. `refresh` keeps the dialog's rows on failure; an
    /// open or another computer starts without them. `agent` is the dialog's
    /// requested agent. Blocks; the limits arrive later.
    pub fn launch_accounts(
        &self,
        computer: &ComputerId,
        refresh: bool,
        agent: Option<&str>,
    ) -> Result<LaunchAccounts, String> {
        if !self.0.allowed {
            return Err(REPLAY_NEEDS_HGS.into());
        }
        let runs = self.launch_start(computer, refresh, agent);
        self.run(runs);
        self.launch_wait(computer);
        let runs = self.launch_waited(computer);
        self.run(runs);
        self.publish();
        Ok(self.launch_rows(computer))
    }

    /// The Projects view. It follows the fleet (its unconnected computers)
    /// and starts no read.
    pub fn projects(&self) -> Revisioned<ProjectsView> {
        let mut hub = self.current();
        let view = self.projects_of(&hub);
        let revision = remember(&mut hub, &[Screen::Projects], &view);
        Revisioned {
            value: view,
            revision,
        }
    }

    /// The organization the Sessions list groups by, from the organization
    /// hub (`crate::organization::Presented`): its `swarm get` object, this
    /// device's default project and the write controller's status line, as
    /// Qt's `SwarmController::statusChanged` reaches
    /// `ProjectsDialog::setSwarmStatus` (SessionsWindow.cpp:1653). An empty
    /// status keeps the line the object gives.
    ///
    /// It takes only this hub's lock, projects and emits: it runs on the
    /// organization's threads, sometimes under its writer's lock, which the
    /// main thread takes in `flush` as the app quits. Tauri's emit queues
    /// the event for the event loop and never waits for the main thread (as
    /// long as tauri's `tracing` feature stays off: with it, `eval_script`
    /// waits for the event loop's answer).
    pub fn organization(
        &self,
        value: Option<Arc<Value>>,
        default_project: String,
        status: (String, bool),
    ) {
        {
            let mut hub = self.lock();
            hub.swarm.set_default(Some(default_project));
            if let Some(value) = value {
                hub.swarm.finish(Ok((*value).clone()));
            }
            let (status, error) = status;
            if !status.is_empty() {
                hub.swarm.set_status(status, error);
            }
            hub.dirty.insert(Screen::Projects);
        }
        self.publish();
    }
}

#[tauri::command(async)]
pub fn screen_visible(
    state: State<'_, Screens>,
    screen: Screen,
    visible: bool,
) -> Result<(), String> {
    state.set_visible(screen, visible);
    Ok(())
}

/// The selected session's cached usage; `target` null stops later reads.
#[tauri::command(async)]
pub fn session_usage(
    state: State<'_, Screens>,
    target: Option<SessionUsageRef>,
    force: bool,
) -> Usage {
    state.session_usage(target, force)
}

#[tauri::command(async)]
pub fn accounts_view(
    state: State<'_, Screens>,
    force: bool,
) -> Result<Revisioned<AccountsView>, String> {
    Ok(state.accounts(force))
}

#[tauri::command(async)]
pub fn account_filters_view(
    state: State<'_, Screens>,
    force: bool,
) -> Result<Revisioned<AccountsView>, String> {
    Ok(state.filter_accounts(force))
}

#[tauri::command(async)]
pub fn machines_view(
    state: State<'_, Screens>,
    force: bool,
) -> Result<Revisioned<MachinesView>, String> {
    Ok(state.machines(force))
}

#[tauri::command]
pub async fn machine_change(
    state: State<'_, Screens>,
    request: zerus_client_core::machine_connections::Request,
) -> Result<(), String> {
    let screens = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || screens.change_machine(request))
        .await
        .map_err(|error| error.to_string())?
}

/// Explicit Machines click: use the existing terminal/console host and hgs.
#[tauri::command]
pub async fn machine_ssh(
    app: tauri::AppHandle,
    state: State<'_, Screens>,
    reference: zerus_client_core::machine_connections::Reference,
) -> Result<Option<String>, String> {
    use tauri::Manager;
    if crate::actions::blocked() {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let screens = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        let settings = crate::desk::terminal_settings(&dir);
        let mut terminal = zerus_client_core::terminal_launch::Launcher::new(settings);
        screens.ssh_machine(reference, &mut terminal)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Choose a path only. The draft owns late-result checks; no key bytes are read.
#[tauri::command]
pub async fn machine_pick_key(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    if crate::actions::blocked() {
        return Err(REPLAY_NEEDS_HGS.into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable.")?;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose SSH private key")
            .set_parent(&window)
            .blocking_pick_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(file) = picked else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|_| "Choose a local SSH key file.".to_owned())?;
    if !path.is_absolute() {
        return Err("Choose a local SSH key file.".into());
    }
    let path = path
        .to_str()
        .ok_or("Choose a local SSH key file.")?
        .to_owned();
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("Choose an absolute SSH key path or a path starting with ~/ .".into());
    }
    Ok(Some(path))
}

#[tauri::command(async)]
pub fn machine_resolve(
    state: State<'_, Screens>,
    alias: String,
) -> Result<Revisioned<MachinesView>, String> {
    Ok(state.resolve(alias))
}

#[tauri::command(async)]
pub fn machine_check(
    state: State<'_, Screens>,
    alias: String,
) -> Result<Revisioned<MachinesView>, String> {
    Ok(state.check(alias))
}

#[tauri::command(async)]
pub fn projects_view(state: State<'_, Screens>) -> Result<Revisioned<ProjectsView>, String> {
    Ok(state.projects())
}

/// New session's accounts of one computer, on its open, a computer change
/// (`refresh` false) and Refresh (`refresh` true); `agent` is the dialog's
/// requested agent.
#[tauri::command]
pub async fn launch_accounts(
    state: State<'_, Screens>,
    computer: String,
    refresh: bool,
    agent: Option<String>,
) -> Result<LaunchAccounts, String> {
    let screens = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        screens.launch_accounts(&ComputerId(computer), refresh, agent.as_deref())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The dialog closed: `launch-accounts-changed` stops.
#[tauri::command(async)]
pub fn launch_accounts_close(state: State<'_, Screens>) {
    state.launch_close();
}

/// The rows again, after `launch-accounts-changed` or the dialog's minute;
/// reads nothing.
#[tauri::command(async)]
pub fn launch_account_rows(state: State<'_, Screens>, computer: String) -> LaunchAccounts {
    state.launch_rows(&ComputerId(computer))
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn screens() -> Screens {
        Screens::new(
            PathBuf::from("unused-hgs"),
            true,
            Arc::new(|_| panic!("a view-only test started a read")),
            Arc::new(|_| {}),
            Arc::new(Vec::new),
            Arc::new(|| 0),
        )
    }

    #[test]
    fn returned_views_are_stamped_in_computation_order_without_changing_content_comparisons() {
        let screens = screens();
        screens
            .lock()
            .visible
            .extend([Screen::Accounts, Screen::Overview]);
        let first = screens.accounts(false);
        let second = screens.accounts(false);
        let projects = screens.projects();
        assert!(first.revision < second.revision && second.revision < projects.revision);
        let serialized = serde_json::to_value(&second).unwrap();
        assert_eq!(serialized["revision"], second.revision);
        for (key, value) in serde_json::to_value(&second.value)
            .unwrap()
            .as_object()
            .unwrap()
        {
            assert_eq!(&serialized[key], value, "existing field {key}");
        }
        let mut hub = screens.lock();
        assert_eq!(hub.shown[&Screen::Accounts], hub.shown[&Screen::Overview]);
        assert!(hub.shown[&Screen::Accounts].get("revision").is_none());
        hub.dirty.insert(Screen::Accounts);
        drop(hub);
        // Revisions do not turn identical content into a changed notification.
        screens.publish();
        assert_eq!(
            screens.lock().shown[&Screen::Accounts],
            serde_json::to_value(second.value).unwrap()
        );
    }

    #[test]
    fn a_projection_panic_returns_a_clear_screen_error_and_the_next_projection_recovers() {
        for screen in [Screen::Accounts, Screen::Machines, Screen::Projects] {
            let screens = screens();
            screens
                .0
                .panic_projection
                .store(screen as u8 + 1, Ordering::Relaxed);
            let view = match screen {
                Screen::Accounts => serde_json::to_value(screens.accounts(false)).unwrap(),
                Screen::Machines => serde_json::to_value(screens.machines(false)).unwrap(),
                _ => serde_json::to_value(screens.projects()).unwrap(),
            };
            match screen {
                Screen::Accounts => {
                    assert_eq!(view["notices"], json!([PROJECTION_FAILED]));
                    assert_eq!(view["busy"], false);
                    assert_eq!(view["pending"], 0);
                }
                Screen::Machines => {
                    assert_eq!(view["error"], PROJECTION_FAILED);
                    assert_eq!(view["busy"], false);
                }
                _ => {
                    assert_eq!(view["status"], PROJECTION_FAILED);
                    assert_eq!(view["status_error"], true);
                }
            }
            screens.0.panic_projection.store(0, Ordering::Relaxed);
            let recovered = match screen {
                Screen::Accounts => serde_json::to_value(screens.accounts(false)).unwrap(),
                Screen::Machines => serde_json::to_value(screens.machines(false)).unwrap(),
                _ => serde_json::to_value(screens.projects()).unwrap(),
            };
            assert!(!recovered.to_string().contains(PROJECTION_FAILED));
            assert!(recovered["revision"].as_u64().unwrap() > view["revision"].as_u64().unwrap());
        }
    }

    #[test]
    fn a_panicking_tick_does_not_stop_the_next_tick() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fleet = Arc::clone(&calls);
        let screens = Screens::new(
            PathBuf::from("unused-hgs"),
            true,
            Arc::new(|_| panic!("a tick-only test started a read")),
            Arc::new(|_| {}),
            Arc::new(move || {
                if fleet.fetch_add(1, Ordering::Relaxed) == 0 {
                    panic!("injected fleet projection panic");
                }
                Vec::new()
            }),
            Arc::new(|| 0),
        );
        screens.tick();
        screens.tick();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// The core's fake hgs (client/core/tests/support/fake_hgs_reads.py) with the view fixtures.
    struct Fake {
        dir: PathBuf,
        hgs: PathBuf,
    }

    impl Fake {
        fn new(replies: Value) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "zerus-screens-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests");
            let hgs = dir.join("hgs");
            std::os::unix::fs::symlink(core.join("support/fake_hgs_reads.py"), &hgs).unwrap();
            let config = json!({"fixtures": core.join("fixtures/views"), "replies": replies});
            std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
            Self { dir, hgs }
        }

        /// Every call so far, as its argv joined by spaces.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("log.ndjson"))
                .unwrap_or_default()
                .lines()
                .map(|line| {
                    let call: Value = serde_json::from_str(line).unwrap();
                    call["argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        }
    }

    impl Fake {
        /// Replaces one argv's reply.
        fn set_reply(&self, argv: &str, reply: Value) {
            let path = self.dir.join("config.json");
            let mut config: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            config["replies"][argv] = reply;
            std::fs::write(path, config.to_string()).unwrap();
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A hub on the injected clock. Its reads wait in `jobs` until the test
    /// runs them (deterministic), except on `threaded` hubs.
    struct Harness {
        screens: Screens,
        clock: Arc<AtomicU64>,
        notified: Arc<Mutex<Vec<Screen>>>,
        fleet: Arc<Mutex<Vec<FleetEntry>>>,
        jobs: Arc<Mutex<VecDeque<Job>>>,
        fake: Fake,
        /// The fake runs as a child of the tests (see `crate::PROCESS_TREE`).
        _tree: std::sync::RwLockReadGuard<'static, ()>,
    }

    impl Harness {
        fn at(&self, ms: u64) {
            self.clock.store(ms, Ordering::SeqCst);
        }

        fn count(&self, argv: &str) -> usize {
            self.fake
                .calls()
                .iter()
                .filter(|call| call.as_str() == argv)
                .count()
        }

        /// Reads started and not run yet.
        fn queued(&self) -> usize {
            self.jobs.lock().unwrap().len()
        }

        /// Runs the read queued at `index` while the others stay in flight;
        /// what it starts queues behind them.
        fn run(&self, index: usize) {
            let job = self.jobs.lock().unwrap().remove(index);
            job.expect("a queued read")();
        }

        /// Runs every queued read, and the reads they start, until none is left.
        fn drain(&self) {
            loop {
                let next = self.jobs.lock().unwrap().pop_front();
                let Some(job) = next else { break };
                job();
            }
        }

        /// The events since the last call.
        fn events(&self) -> Vec<Screen> {
            std::mem::take(&mut *self.notified.lock().unwrap())
        }

        fn set_fleet(&self, fleet: Vec<FleetEntry>) {
            *self.fleet.lock().unwrap() = fleet;
        }

        fn work(&self) -> Work {
            self.screens.lock().work
        }

        /// The view the last event or pull handed the webview for `screen`.
        fn published(&self, screen: Screen) -> Value {
            let hub = self.screens.lock();
            hub.shown.get(&screen).cloned().unwrap_or_default()
        }
    }

    fn local() -> FleetEntry {
        FleetEntry {
            computer: ComputerId::local(),
            status: "live".into(),
            attempt: 0,
            local_sessions: true,
            local_host: false,
            host: Some("workstation".into()),
            platform: Some("linux".into()),
        }
    }

    fn peer(alias: &str, status: &str, attempt: u32) -> FleetEntry {
        FleetEntry {
            computer: ComputerId::from(alias),
            status: status.into(),
            attempt,
            local_sessions: true,
            local_host: false,
            host: Some(format!("{alias}.example")),
            platform: Some("linux".into()),
        }
    }

    fn build(replies: Value, allowed: bool, threaded: bool) -> Harness {
        let tree = crate::PROCESS_TREE
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fake = Fake::new(replies);
        let clock = Arc::new(AtomicU64::new(0));
        let notified = Arc::new(Mutex::new(Vec::new()));
        let fleet = Arc::new(Mutex::new(vec![local()]));
        let jobs: Arc<Mutex<VecDeque<Job>>> = Arc::new(Mutex::new(VecDeque::new()));
        let spawn: Spawn = if threaded {
            Arc::new(|job: Job| {
                std::thread::spawn(job);
            })
        } else {
            let jobs = jobs.clone();
            Arc::new(move |job: Job| jobs.lock().unwrap().push_back(job))
        };
        let screens = Screens::new(
            fake.hgs.clone(),
            allowed,
            spawn,
            {
                let notified = notified.clone();
                Arc::new(move |screen: Screen| notified.lock().unwrap().push(screen))
            },
            {
                let fleet = fleet.clone();
                Arc::new(move || fleet.lock().unwrap().clone())
            },
            {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
        );
        Harness {
            screens,
            clock,
            notified,
            fleet,
            jobs,
            fake,
            _tree: tree,
        }
    }

    fn harness(replies: Value, allowed: bool) -> Harness {
        build(replies, allowed, false)
    }

    fn member(view: &AccountsView, id: &str) -> accounts::Profile {
        view.accounts
            .iter()
            .flat_map(|a| &a.members)
            .find(|m| m.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is listed"))
    }

    const INSPECTS: [&str; 3] = [
        "account inspect claude-work --refresh",
        "account inspect native-claude --refresh",
        "account inspect native-codex --refresh",
    ];

    /// The core's `swarm get` fixture (`core/tests/fixtures/views/swarm.json`).
    fn swarm_fixture() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures/views/swarm.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn local_accounts() -> Value {
        json!({
            "account ls": {"file": "accounts-local.json"},
            "@studio account ls": {"file": "accounts-studio.json"},
            "account inspect claude-work --refresh": {"file": "inspect-claude-work.json"},
            "account inspect native-claude --refresh": {"stderr": "hgs: inspect failed\n", "exit": 1},
            "account inspect native-codex --refresh": {"file": "inspect-native-codex.json"},
        })
    }

    #[test]
    fn a_catalog_panic_wakes_new_session_and_notifies_without_replaying_the_read() {
        let h = harness(launch_replies(false), true);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let heard_by_callback = Arc::clone(&heard);
        h.screens.on_launch_change(Arc::new(move |computer| {
            heard_by_callback.lock().unwrap().push(computer.clone());
        }));
        let here = ComputerId::local();
        let runs = h.screens.launch_start(&here, false, None);
        h.screens.run(runs);
        let waiting = h.screens.clone();
        let (ready, started) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let hub = waiting.lock();
            assert!(!hub.launch.as_ref().unwrap().answered);
            ready.send(()).unwrap();
            let mut hub = hub;
            loop {
                let (next, timeout) = waiting
                    .0
                    .catalog_done
                    .wait_timeout(hub, Duration::from_secs(2))
                    .unwrap();
                hub = next;
                assert!(
                    !timeout.timed_out(),
                    "the catalog completion must wake the waiter"
                );
                if hub.launch.as_ref().unwrap().answered {
                    break;
                }
            }
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        waiter.join().unwrap();
        let view = h.screens.launch_rows(&here);
        assert!(view.loaded);
        assert_eq!(
            view.error.as_deref(),
            Some("Could not load accounts: Could not refresh this screen.")
        );
        assert_eq!(
            heard.lock().unwrap().as_slice(),
            std::slice::from_ref(&here)
        );
        assert_eq!(
            h.count("account ls"),
            0,
            "panic recovery must not replay the CLI read"
        );
        assert!(!h.screens.lock().accounts.reading());
        let runs = h.screens.launch_waited(&here);
        assert!(runs.is_empty());
        launch_ask(&h, &here, true);
        assert!(h.screens.launch_rows(&here).error.is_none());
        assert_eq!(h.count("account ls"), 1, "only the explicit Refresh reads");
    }

    #[test]
    fn panicking_catalog_and_usage_jobs_release_their_slots_and_allow_refresh() {
        let h = harness(local_accounts(), true);
        h.screens.set_visible(Screen::Accounts, true);
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        {
            let hub = h.screens.lock();
            assert!(!hub.accounts.reading());
            assert_eq!(
                hub.accounts.catalogs[&ComputerId::local()].error.as_deref(),
                Some(READ_FAILED)
            );
        }
        h.screens.accounts(true);
        h.run(0);
        assert_eq!(h.queued(), 2);
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        h.drain();
        let view = h.screens.accounts(false);
        assert_eq!(view.pending, 0);
        assert!(!view.busy && !h.screens.lock().accounts.reading());
        assert!(member(&view, "claude-work").usage.refresh_error);
        h.screens.accounts(true);
        h.drain();
        assert!(
            !member(&h.screens.accounts(false), "claude-work")
                .usage
                .refresh_error
        );
    }

    #[test]
    fn panicking_machine_list_resolve_and_check_jobs_release_the_exact_slot() {
        let h = harness(
            json!({
                "machine ls": {"file": "machines.json"},
                "machine resolve studio": {"file": "resolve-studio.json"},
                "machine check studio": {"file": "check-studio.json"}
            }),
            true,
        );
        h.screens.machines(true);
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        let view = h.screens.machines(false);
        assert!(!view.busy && !view.refreshing);
        assert_eq!(view.error.as_deref(), Some(READ_FAILED));
        h.screens.machines(true);
        h.drain();
        assert!(h.screens.machines(false).error.is_none());
        h.screens.resolve("studio".into());
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        assert!(h
            .screens
            .machines(false)
            .effective
            .as_ref()
            .is_some_and(|view| !view.pending && view.error));
        h.screens.resolve("studio".into());
        h.drain();
        assert!(h
            .screens
            .machines(false)
            .effective
            .as_ref()
            .is_some_and(|view| !view.pending && !view.error));
        h.screens.check("studio".into());
        h.screens.0.panic_read.store(true, Ordering::Relaxed);
        h.run(0);
        let view = h.screens.machines(false);
        assert!(!view.busy);
        assert!(view
            .check
            .as_ref()
            .is_some_and(|view| !view.pending && view.error));
        h.screens.check("studio".into());
        h.drain();
        assert!(h
            .screens
            .machines(false)
            .check
            .as_ref()
            .is_some_and(|view| !view.pending && view.text.starts_with("SSH connected:")));
    }

    #[test]
    fn a_late_notification_panic_does_not_clear_a_newer_read() {
        let _tree = crate::PROCESS_TREE
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let fake = Fake::new(json!({"machine ls": {"file": "machines.json"}}));
        let jobs = Arc::new(Mutex::new(VecDeque::<Job>::new()));
        let queued = Arc::clone(&jobs);
        let owner = Arc::new(std::sync::OnceLock::<std::sync::Weak<Shared>>::new());
        let notifying = Arc::clone(&owner);
        let panic_next = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let panicking = Arc::clone(&panic_next);
        let screens = Screens::new(
            fake.hgs.clone(),
            true,
            Arc::new(move |job| queued.lock().unwrap().push_back(job)),
            Arc::new(move |screen| {
                if screen == Screen::Machines && panicking.swap(false, Ordering::Relaxed) {
                    Screens(notifying.get().unwrap().upgrade().unwrap()).machines(true);
                    panic!("injected notification panic after a new read started");
                }
            }),
            Arc::new(Vec::new),
            Arc::new(|| 0),
        );
        assert!(owner.set(Arc::downgrade(&screens.0)).is_ok());
        screens.set_visible(Screen::Machines, true);
        panic_next.store(true, Ordering::Relaxed);
        let job = jobs.lock().unwrap().pop_front().unwrap();
        job();
        assert_eq!(jobs.lock().unwrap().len(), 1);
        assert!(
            screens.machines(false).busy,
            "the old job must not clear the new read"
        );
        let job = jobs.lock().unwrap().pop_front().unwrap();
        job();
        assert!(!screens.machines(false).busy);
    }

    // Projects never reads the catalog: the organization hub hands it over.
    #[test]
    fn projects_show_the_organization_they_are_handed_and_never_read_it() {
        let h = harness(json!({"swarm get": {"file": "swarm.json"}}), true);
        h.screens.set_visible(Screen::Projects, true);
        for second in 1..=12 {
            h.at(second * 1000);
            h.screens.tick();
        }
        // Every read started so far runs: the account warm-up, and nothing else.
        h.drain();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            h.count("swarm get"),
            0,
            "the organization topic feeds Projects"
        );
        assert!(!h.screens.projects().loaded);
        let swarm = swarm_fixture();
        let failed = "Project sync unavailable: hgs did not answer in time";
        // Showing Projects published its empty view; only the call's event counts.
        h.events();
        // Before the first state the call carries the status line alone.
        h.screens
            .organization(None, "sample".into(), (failed.into(), true));
        let view = h.screens.projects();
        assert!(!view.loaded);
        assert_eq!((view.status.as_str(), view.status_error), (failed, true));
        h.screens.organization(
            Some(Arc::new(swarm)),
            "sample".into(),
            (failed.into(), true),
        );
        let view = h.screens.projects();
        assert!(view.loaded);
        assert!(view
            .projects
            .iter()
            .any(|p| p.id == "sample" && p.is_default));
        assert_eq!(
            (view.status.as_str(), view.status_error),
            (failed, true),
            "the controller's status line"
        );
        eventually("a change event", || {
            h.notified.lock().unwrap().contains(&Screen::Projects)
        });
    }

    // Review Focus 4.
    #[test]
    fn sessions_account_picker_refreshes_catalogs_without_profile_or_session_inspects() {
        let h = harness(local_accounts(), true);
        assert!(h.screens.filter_accounts(true).catalog_loading);
        h.drain();
        let view = h.screens.filter_accounts(false);
        assert!(!view.catalog_loading);
        assert!(!view.accounts.is_empty());
        assert_eq!(h.count("account ls"), 1);
        assert!(h.fake.calls().iter().all(|call| !call.contains("inspect")));
        h.screens.filter_accounts(true);
        h.drain();
        assert_eq!(h.count("account ls"), 2);
        assert!(h.fake.calls().iter().all(|call| !call.contains("inspect")));
    }

    #[test]
    fn a_failing_or_slow_inspect_leaves_the_others_and_waits_five_minutes() {
        let h = harness(local_accounts(), true);
        h.screens.set_visible(Screen::Accounts, true);
        assert_eq!(h.queued(), 1, "the catalog first");
        h.run(0);
        assert_eq!(h.queued(), 2, "two inspects at once, the third waits");
        // claude-work is slow: the failing one answers and the third takes its turn.
        h.run(1);
        assert_eq!(h.queued(), 2);
        h.run(1);
        let view = h.screens.accounts(false);
        assert_eq!(
            member(&view, "native-codex").usage.status,
            "signed_out",
            "the others still load"
        );
        let failed = member(&view, "native-claude");
        assert!(failed.usage.refresh_error);
        assert_eq!(failed.usage.status, "unavailable");
        assert_eq!(view.pending, 1, "only the slow one is still read");
        h.drain();
        let view = h.screens.accounts(false);
        assert!(view.pending == 0 && !view.busy && view.snapshot);
        assert_eq!(member(&view, "claude-work").usage.status, "ok");
        for call in INSPECTS {
            assert_eq!(h.count(call), 1, "{call}");
        }
        // The catalog is read again after 30 s, the inspects only after 5 minutes.
        h.at(60_000);
        h.screens.tick();
        assert_eq!(h.queued(), 1);
        h.drain();
        assert_eq!(h.count("account ls"), 2);
        h.at(299_999);
        h.screens.tick();
        h.drain();
        assert_eq!(h.count("account ls"), 3);
        assert_eq!(
            h.count("account inspect native-claude --refresh"),
            1,
            "no retry inside five minutes"
        );
        h.at(300_000);
        h.events();
        h.screens.tick();
        assert_eq!(h.queued(), 2, "the re-inspect batch starts");
        assert_eq!(h.events(), [Screen::Accounts], "and shows at once");
        let shown = h.published(Screen::Accounts);
        assert_eq!(
            (shown["busy"].clone(), shown["pending"].clone()),
            (json!(true), json!(3))
        );
        h.drain();
        for call in INSPECTS {
            assert_eq!(h.count(call), 2, "{call}");
        }
        // Refresh forces both, and is ignored while account reads run (Qt's reload).
        h.at(301_000);
        assert!(h.screens.accounts(true).busy);
        assert_eq!(h.queued(), 3, "the catalog and two inspects");
        h.screens.accounts(true);
        assert_eq!(h.queued(), 3);
        h.drain();
        assert_eq!(h.count("account ls"), 4);
        for call in INSPECTS {
            assert_eq!(h.count(call), 3, "{call}");
        }
    }

    // Controller ruling: no reads for hidden screens.
    #[test]
    fn hidden_account_screens_drop_queued_reads_and_only_warm_new_hosts() {
        let h = harness(local_accounts(), true);
        h.screens.set_visible(Screen::Overview, true);
        h.run(0);
        assert_eq!(h.queued(), 2);
        h.screens.set_visible(Screen::Overview, false);
        h.drain();
        assert_eq!(h.count(INSPECTS[0]), 1, "running reads finish");
        assert_eq!(h.count(INSPECTS[2]), 0, "the queued inspect is dropped");
        for at in [40_000, 400_000] {
            h.at(at);
            h.screens.tick();
            h.screens.accounts(false);
        }
        assert_eq!(h.queued(), 0, "hidden: nothing falls due");
        // A newly listed host is still warmed, once, without inspects.
        h.set_fleet(vec![local(), peer("studio", "live", 0)]);
        h.screens.accounts(false);
        assert_eq!(h.queued(), 0, "a hidden pull starts nothing");
        h.screens.tick();
        assert_eq!(h.queued(), 1);
        h.drain();
        assert_eq!(h.count("@studio account ls"), 1);
        assert!(h
            .fake
            .calls()
            .iter()
            .all(|call| !call.starts_with("@studio account inspect")));
        h.at(500_000);
        h.screens.tick();
        assert_eq!(h.queued(), 0, "a host is warmed once");
        // Showing Accounts makes every read due again, the dropped inspect among them.
        h.screens.set_visible(Screen::Accounts, true);
        h.drain();
        assert_eq!(h.count("account ls"), 2);
        assert_eq!(h.count("@studio account ls"), 2);
        assert_eq!(h.count(INSPECTS[2]), 1);
        assert_eq!(
            h.count("@studio account inspect claude-studio --refresh"),
            1
        );
    }

    // Controller ruling: no per-profile work on the ticker.
    #[test]
    fn the_ticker_works_only_after_a_fleet_change_a_reply_or_a_due_time() {
        let h = harness(local_accounts(), true);
        h.set_fleet(vec![local(), peer("studio", "live", 0)]);
        h.screens.set_visible(Screen::Accounts, true);
        h.drain();
        h.screens.accounts(false);
        h.events();
        let idle = h.work();
        for second in 1..=29 {
            h.at(second * 1000);
            h.screens.tick();
        }
        assert_eq!(
            h.work(),
            idle,
            "idle ticks step no account read and project no view"
        );
        assert!(h.events().is_empty() && h.queued() == 0);
        // A peer that drops out changes the view: one event, then quiet again.
        h.set_fleet(vec![local(), peer("studio", "reconnecting", 1)]);
        h.screens.tick();
        h.screens.tick();
        assert_eq!(h.events(), [Screen::Accounts]);
        let changed = h.work();
        assert_eq!(
            (changed.steps, changed.projections),
            (idle.steps + 1, idle.projections + 1)
        );
        // The catalogs fall due 30 s after they started; the reads that
        // start show at once (Qt's updateRefreshState), in one projection.
        h.at(30_000);
        h.screens.tick();
        assert_eq!(h.queued(), 2);
        assert_eq!(h.events(), [Screen::Accounts]);
        assert_eq!(h.published(Screen::Accounts)["busy"], json!(true));
        let busy = h.work();
        assert_eq!(busy.projections, changed.projections + 1);
        h.screens.tick();
        assert_eq!(h.work(), busy, "nothing due: nothing published");
        assert!(h.events().is_empty() && h.queued() == 2);
        h.drain();
        assert_eq!(h.count("account ls"), 2);
        assert_eq!(h.published(Screen::Accounts)["busy"], json!(false));
    }

    #[test]
    fn machine_setup_rechecks_and_runs_only_the_original_confirmed_call() {
        let snapshot = json!({"local":"fixture-local","revision":"a".repeat(64),"machines":[{"alias":"fixture-peer"}]});
        let command = format!(
            "machine setup fixture-peer --revision {} --source /synthetic/source",
            "a".repeat(64)
        );
        let h = harness(
            json!({"machine ls":{"stdout":snapshot.to_string()},(command.clone()):{"stdout":"setup complete"}}),
            true,
        );
        h.screens.lock().machines.finish_list(Ok(snapshot));
        let result = h
            .screens
            .setup_machine(
                machine_setup::Request::Install {
                    reference: zerus_client_core::machine_connections::Reference {
                        alias: "fixture-peer".into(),
                        revision: "a".repeat(64),
                    },
                    source: "/synthetic/source".into(),
                },
                None,
            )
            .unwrap();
        assert!(result.success);
        assert_eq!(h.count("machine ls"), 1);
        assert_eq!(h.count(&command), 1);
        assert!(!h.screens.machines(false).busy);
    }
    #[test]
    fn machine_setup_local_enable_checks_current_console_and_original_identity() {
        use zerus_client_core::machine::{LocalHost, MachineMeta};
        let h = harness(
            json!({"session-host status --json":{"stdout":"{\"enabled\":false}"},"session-host install --json":{"stdout":"{\"enabled\":true}"}}),
            true,
        );
        let meta = MachineMeta {
            machine_id: Some("original".into()),
            platform: Some("windows".into()),
            local_sessions: Some(false),
            local_host: Some(LocalHost {
                state: Some("not-installed".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(h
            .screens
            .setup_machine(
                machine_setup::Request::Enable {
                    machine_id: "replaced".into()
                },
                Some(meta.clone())
            )
            .is_err());
        assert_eq!(h.count("session-host install --json"), 0);
        let result = h
            .screens
            .setup_machine(
                machine_setup::Request::Enable {
                    machine_id: "original".into(),
                },
                Some(meta),
            )
            .unwrap();
        assert!(result.success);
        assert_eq!(h.count("session-host status --json"), 1);
        assert_eq!(h.count("session-host install --json"), 1);
    }

    struct MachineTerminal(Arc<Mutex<Vec<zerus_client_core::terminal_launch::Start>>>);
    impl zerus_client_core::terminal_launch::Desktop for MachineTerminal {
        fn find(&self, _: &str) -> Option<String> {
            None
        }
        fn env(&self, _: &str) -> Option<String> {
            None
        }
        fn exists(&self, _: &str) -> bool {
            false
        }
        fn home(&self) -> String {
            "/synthetic/home".into()
        }
        fn console_host(&self) -> String {
            "unused".into()
        }
        fn start(
            &mut self,
            start: &zerus_client_core::terminal_launch::Start,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(start.clone());
            Ok(())
        }
    }

    #[test]
    fn machine_ssh_rechecks_the_saved_alias_and_uses_the_maintained_terminal_route() {
        use zerus_client_core::{
            machine_connections::Reference,
            terminal_launch::{Launcher, NoKonsole, Platform, Settings},
        };
        let snapshot = json!({"local":"fixture-local","revision":"a".repeat(64),"machines":[{"alias":"fixture-peer","enabled":false}]});
        let h = harness(json!({"machine ls":{"stdout":snapshot.to_string()}}), true);
        h.screens.lock().machines.finish_list(Ok(snapshot));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = Launcher::with(
            Settings {
                template: "fixture-terminal --command {cmd}".into(),
                ..Default::default()
            },
            Platform::Linux,
            Box::new(MachineTerminal(calls.clone())),
            Box::new(NoKonsole),
        );
        let reference = Reference {
            alias: "fixture-peer".into(),
            revision: "a".repeat(64),
        };
        assert_eq!(h.screens.ssh_machine(reference, &mut terminal), Ok(None));
        assert_eq!(
            h.count("machine ls"),
            1,
            "one local recheck, no remote connection probe"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "fixture-terminal");
        assert_eq!(
            calls[0].arguments,
            [
                "--command",
                &format!("{} machine ssh fixture-peer", h.fake.hgs.display())
            ]
        );
        assert!(!h.screens.machines(false).busy);
    }

    #[test]
    fn machine_ssh_refuses_a_changed_revision_before_any_terminal_launch() {
        use zerus_client_core::{
            machine_connections::{Reference, CHANGED},
            terminal_launch::{Launcher, NoKonsole, Platform, Settings},
        };
        let snapshot = json!({"local":"fixture-local","revision":"a".repeat(64),"machines":[{"alias":"fixture-peer"}]});
        let mut changed = snapshot.clone();
        changed["revision"] = json!("b".repeat(64));
        let h = harness(json!({"machine ls":{"stdout":changed.to_string()}}), true);
        h.screens.lock().machines.finish_list(Ok(snapshot));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = Launcher::with(
            Settings {
                template: "fixture-terminal --command {cmd}".into(),
                ..Default::default()
            },
            Platform::Linux,
            Box::new(MachineTerminal(calls.clone())),
            Box::new(NoKonsole),
        );
        assert_eq!(
            h.screens.ssh_machine(
                Reference {
                    alias: "fixture-peer".into(),
                    revision: "a".repeat(64)
                },
                &mut terminal
            ),
            Err(CHANGED.into())
        );
        assert!(calls.lock().unwrap().is_empty());
        assert!(!h.screens.machines(false).busy);
    }

    #[test]
    fn machine_profile_write_uses_the_saved_revision_then_refreshes_the_same_hub() {
        use zerus_client_core::machine_connections::{Change, Request};
        let old = json!({"local":"fixture-local","revision":"a".repeat(64),
            "machines":[{"alias":"fixture-peer","source":"local","inherited":true}]});
        let mut updated = old.clone();
        updated["revision"] = json!("b".repeat(64));
        let command = format!("machine reset fixture-peer --revision {}", "a".repeat(64));
        let h = harness(
            json!({
                command.clone():{"stdout":json!({"ok":true,"alias":"fixture-peer","dry_run":false}).to_string()},
                "machine ls":{"stdout":updated.to_string()}
            }),
            true,
        );
        h.screens.lock().machines.finish_list(Ok(old));
        let req = Request {
            alias: "fixture-peer".into(),
            revision: "a".repeat(64),
            change: Change::Reset,
        };
        assert!(h.screens.change_machine(req.clone()).is_ok());
        assert_eq!(h.count(&command), 1);
        assert_eq!(h.count("machine ls"), 1);
        assert_eq!(
            h.screens.machines(false).connection_revision,
            "b".repeat(64)
        );
        assert!(!h.screens.machines(false).busy);
        assert!(h.screens.change_machine(req).is_err());
        assert_eq!(
            h.count(&command),
            1,
            "stale original write never reaches hgs again"
        );
    }

    #[test]
    fn machine_profile_unknown_reply_releases_the_page_lane_without_replay() {
        use zerus_client_core::machine_connections::{Change, Request, UNCERTAIN};
        let command = format!("machine remove fixture-peer --revision {}", "a".repeat(64));
        let h = harness(json!({command.clone():{"stdout":"not-json"}}), true);
        h.screens.lock().machines.finish_list(Ok(json!({"local":"fixture-local","revision":"a".repeat(64),"machines":[{"alias":"fixture-peer"}]})));
        let req = Request {
            alias: "fixture-peer".into(),
            revision: "a".repeat(64),
            change: Change::Remove,
        };
        assert_eq!(h.screens.change_machine(req), Err(UNCERTAIN.into()));
        assert!(!h.screens.machines(false).busy);
        assert_eq!(h.count(&command), 1);
        assert_eq!(
            h.queued(),
            0,
            "unknown result queues neither retry nor another effect"
        );
        assert_eq!(
            h.count("machine ls"),
            0,
            "failed write keeps the reviewed snapshot"
        );
    }

    #[test]
    fn machine_profile_replay_and_an_active_page_read_refuse_before_writing() {
        use zerus_client_core::machine_connections::{Change, Request};
        let h = harness(json!({}), false);
        let req = Request {
            alias: "fixture-peer".into(),
            revision: "a".repeat(64),
            change: Change::Remove,
        };
        assert_eq!(
            h.screens.change_machine(req.clone()),
            Err(REPLAY_NEEDS_HGS.into())
        );
        assert!(h.fake.calls().is_empty());
        let h = harness(json!({}), true);
        let mut hub = h.screens.lock();
        hub.machines.finish_list(Ok(json!({"local":"fixture-local","revision":"a".repeat(64),"machines":[{"alias":"fixture-peer"}]})));
        assert!(hub.machines.start_check("fixture-peer"));
        drop(hub);
        assert!(h.screens.change_machine(req).is_err());
        assert!(h.fake.calls().is_empty());
    }

    #[test]
    fn machines_list_on_show_resolve_on_selection_and_check_only_on_request() {
        let h = harness(
            json!({
                "machine ls": {"file": "machines.json"},
                "machine resolve studio": {"file": "resolve-studio.json"},
                "machine check studio": {"file": "check-studio.json"},
            }),
            true,
        );
        h.screens.tick();
        h.drain();
        assert_eq!(h.count("account ls"), 1, "the account warm-up");
        h.screens.machines(false);
        assert_eq!(h.queued(), 0, "a hidden pull reads nothing");
        h.screens.set_visible(Screen::Machines, true);
        h.screens.set_visible(Screen::Machines, true);
        assert!(h.screens.machines(false).refreshing);
        assert_eq!(h.queued(), 1, "one list read on show");
        h.drain();
        let view = h.screens.machines(false);
        assert!(view.loaded && !view.refreshing);
        assert_eq!(view.machines.len(), 3);
        assert_eq!(h.events(), [Screen::Machines]);
        h.screens.resolve("studio".into());
        h.drain();
        assert!(h
            .screens
            .machines(false)
            .effective
            .as_ref()
            .is_some_and(|e| !e.pending));
        assert_eq!(
            h.count("machine check studio"),
            0,
            "a check runs only on request"
        );
        assert!(h.screens.check("studio".into()).busy);
        h.screens.check("studio".into());
        h.screens.machines(true);
        assert_eq!(
            h.queued(),
            1,
            "one check at a time, and no list read meanwhile"
        );
        h.drain();
        assert!(h
            .screens
            .machines(false)
            .check
            .as_ref()
            .is_some_and(|c| !c.pending));
        h.screens.machines(true);
        h.screens.check("studio".into());
        assert_eq!(h.queued(), 1, "no check while the list reads");
        h.drain();
        assert_eq!(h.count("machine ls"), 2);
        assert_eq!(h.count("machine check studio"), 1);
    }

    #[test]
    fn a_replay_without_hgs_never_reads() {
        let h = harness(json!({}), false);
        for screen in [
            Screen::Overview,
            Screen::Machines,
            Screen::Projects,
            Screen::Accounts,
        ] {
            h.screens.set_visible(screen, true);
        }
        h.at(40_000);
        h.screens.tick();
        assert_eq!(
            h.screens.resolve("studio".into()).error.as_deref(),
            Some(REPLAY_NEEDS_HGS)
        );
        h.screens.check("studio".into());
        h.screens.machines(true);
        let accounts = h.screens.accounts(true);
        let projects = h.screens.projects();
        assert_eq!(h.queued(), 0, "no read ever starts");
        assert!(h.fake.calls().is_empty());
        assert_eq!(accounts.notices, [REPLAY_NEEDS_HGS]);
        assert!(accounts.snapshot && accounts.pending == 0 && !accounts.busy);
        assert!(!projects.loaded && projects.status.is_empty());
        // Projects reads nothing, so it keeps the project sync line it is
        // handed; the fixture's own line names a conflict.
        let line = "Local catalog. Connect another computer to sync projects.";
        h.screens.organization(
            Some(Arc::new(swarm_fixture())),
            "sample".into(),
            (line.into(), false),
        );
        let projects = h.screens.projects();
        assert!(projects.loaded);
        assert_eq!(
            (projects.status.as_str(), projects.status_error),
            (line, false)
        );
        assert!(h.fake.calls().is_empty(), "and still reads nothing");
    }

    /// An `account ls` reply of the Qt fixture's shape (test_newsessiondialog.cpp).
    fn launch_catalog(rows: &[(&str, &str, &str)]) -> Value {
        let profiles: Vec<Value> = rows
            .iter()
            .map(|(id, provider, email)| {
                json!({"id": id, "provider": provider, "label": "Review", "home": format!("/profiles/{id}"),
                       "installed": true, "is_default": false,
                       "account_status": {"status": if *provider == "dsh" { "signed_out" } else { "ok" },
                                          "checked_at": 100.0, "identity": {"email": email}}})
            })
            .collect();
        json!({"profiles": profiles, "revision": "r1"})
    }

    fn reply(value: &Value) -> Value {
        json!({ "stdout": value.to_string() })
    }

    fn inspect(id: &str, provider: &str, email: &str) -> Value {
        reply(
            &json!({"id": id, "provider": provider, "home": format!("/profiles/{id}"),
            "status": if provider == "dsh" { "signed_out" } else { "ok" }, "checked_at": 200.0,
            "identity": {"email": email}, "windows": [], "balances": []}),
        )
    }

    fn launch_replies(duplicates: bool) -> Value {
        let mut local = vec![
            ("work", "codex", "shared@example.invalid"),
            ("claude", "claude", "shared@example.invalid"),
            ("native-dsh", "dsh", "shared@example.invalid"),
            ("native-opencode", "opencode", ""),
        ];
        if duplicates {
            local.push(("claude-copy", "claude", "shared@example.invalid"));
        }
        let studio = [
            ("mac-claude", "claude", "mac@example.invalid"),
            ("native-dsh", "dsh", "mac@example.invalid"),
        ];
        let mut replies = json!({
            "account ls": reply(&launch_catalog(&local)),
            "@studio account ls": reply(&launch_catalog(&studio)),
        });
        for (id, provider, email) in local {
            replies[format!("account inspect {id} --refresh")] = inspect(id, provider, email);
        }
        for (id, provider, email) in studio {
            replies[format!("@studio account inspect {id} --refresh")] =
                inspect(id, provider, email);
        }
        replies
    }

    fn members(view: &LaunchAccounts) -> Vec<Vec<String>> {
        view.groups
            .iter()
            .map(|group| group.members.iter().map(|m| m.id.clone()).collect())
            .collect()
    }

    // accountAliasesMatchAccountsGroupingAndKeepExactBinding and
    // chooserAndAccountsPageShowSameAccountsForEachComputer: one computer's
    // rows group as the Accounts page groups that computer's profiles.
    #[test]
    fn launch_accounts_group_one_computer_as_the_accounts_page_does() {
        let h = build(launch_replies(true), true, true);
        h.set_fleet(vec![local(), peer("studio", "live", 0)]);
        let view = h
            .screens
            .launch_accounts(&ComputerId::local(), false, None)
            .unwrap();
        assert!(view.loaded && view.error.is_none());
        assert_eq!(view.profiles.len(), 5, "the raw rows");
        assert_eq!(
            members(&view),
            [
                vec!["work"],
                vec!["claude", "claude-copy"],
                vec!["native-dsh"],
                vec!["native-opencode"]
            ]
        );
        eventually("the limits arrived", || {
            let rows = h.screens.launch_rows(&ComputerId::local());
            rows.groups
                .iter()
                .all(|g| g.usage.checked_at == Some(200.0) || g.provider == "opencode")
        });
        h.screens.set_visible(Screen::Accounts, true);
        eventually("the Accounts page read everything", || {
            let page = h.screens.accounts(false);
            page.pending == 0 && !page.busy && page.snapshot
        });
        let page = h.screens.accounts(false);
        for computer in [ComputerId::local(), ComputerId::from("studio")] {
            let view = h.screens.launch_accounts(&computer, false, None).unwrap();
            let shown: BTreeSet<String> = view.groups.iter().map(|g| g.key.clone()).collect();
            let listed: BTreeSet<String> = page
                .accounts
                .iter()
                .filter(|a| a.members.iter().any(|m| m.host == computer))
                .map(|a| a.key.clone())
                .collect();
            assert_eq!(shown, listed, "{computer:?}");
            assert!(view
                .groups
                .iter()
                .flat_map(|g| &g.members)
                .all(|m| m.host == computer));
        }
    }

    // localWindowsHidesDeepSeekAndOpenCode.
    #[test]
    fn the_local_windows_computer_offers_neither_deepseek_nor_opencode() {
        let h = build(launch_replies(false), true, true);
        let mut windows = local();
        windows.platform = Some("windows".into());
        h.set_fleet(vec![windows, peer("studio", "live", 0)]);
        let here = h
            .screens
            .launch_accounts(&ComputerId::local(), false, None)
            .unwrap();
        let providers: Vec<&str> = here.groups.iter().map(|g| g.provider.as_str()).collect();
        assert_eq!(providers, ["codex", "claude"]);
        assert_eq!(
            here.profiles.len(),
            4,
            "the raw rows stay for the OpenCode fields"
        );
        let studio = h
            .screens
            .launch_accounts(&ComputerId::from("studio"), false, None)
            .unwrap();
        assert!(studio.groups.iter().any(|g| g.provider == "dsh"));
        assert!(h
            .fake
            .calls()
            .iter()
            .all(|call| !call.contains("inspect native-opencode")
                && !call.starts_with("account inspect native-dsh")));
    }

    // emptyComputerCatalogOffersOnlyShell.
    #[test]
    fn an_empty_catalog_gives_no_rows_and_reads_no_limits() {
        let h = build(
            json!({"account ls": reply(&json!({"profiles": [], "revision": "r"}))}),
            true,
            true,
        );
        let view = h
            .screens
            .launch_accounts(&ComputerId::local(), false, None)
            .unwrap();
        assert!(view.loaded && view.groups.is_empty() && view.profiles.is_empty());
        std::thread::sleep(Duration::from_millis(100));
        assert!(h.fake.calls().iter().all(|call| !call.contains("inspect")));
    }

    // NewSessionDialog::loadAccounts (865-874): every open, computer change
    // and Refresh reads the catalog; a running read is shared.
    #[test]
    fn every_open_reads_the_catalog_and_shares_a_running_one() {
        let h = harness(launch_replies(false), true);
        let local_id = ComputerId::local();
        let runs = h.screens.launch_start(&local_id, false, None);
        assert_eq!(runs.len(), 1);
        h.screens.run(runs);
        assert!(
            h.screens.launch_start(&local_id, false, None).is_empty(),
            "the running read is shared"
        );
        assert!(h.screens.launch_start(&local_id, true, None).is_empty());
        h.drain();
        assert_eq!(h.count("account ls"), 1, "no follow-up of a shared read");
        let runs = h.screens.launch_start(&local_id, false, None);
        assert_eq!(runs.len(), 1, "a moment later a new dialog reads again");
        h.screens.run(runs);
        h.drain();
        let runs = h.screens.launch_start(&local_id, true, None);
        assert_eq!(runs.len(), 1, "and so does Refresh");
        h.screens.run(runs);
        h.drain();
        assert_eq!(h.count("account ls"), 3);
        let usage = h.screens.launch_usage(&local_id, false);
        assert_eq!(usage.len(), 2, "two inspects at once");
        h.screens.run(usage);
        h.drain();
        assert_eq!(h.count("account inspect work --refresh"), 1);
        assert!(
            h.screens.launch_usage(&local_id, false).is_empty(),
            "limits checked within five minutes"
        );
        assert_eq!(
            h.screens.launch_usage(&local_id, true).len(),
            2,
            "Refresh forces them"
        );
        h.drain();
        // Rows only read nothing.
        h.screens.launch_rows(&local_id);
        assert_eq!(h.queued(), 0);
        // A computer the Accounts page does not list is read for the dialog too (865-874).
        let gone = ComputerId::from("gone");
        assert_eq!(h.screens.launch_start(&gone, false, None).len(), 1);
        assert!(
            !h.screens.launch_rows(&gone).loaded,
            "until its reply is filed"
        );
    }

    // accountsReady (380-388) asks for limits whenever the catalog arrives:
    // after the waiting `launch_accounts` stopped waiting, the filed reply
    // asks itself; while one waits, it asks on waking. Either way once.
    #[test]
    fn a_catalog_filed_after_the_wait_asks_for_its_limits_once() {
        let inspects = |h: &Harness| -> Vec<usize> {
            ["work", "claude", "native-dsh", "native-opencode"]
                .iter()
                .map(|id| h.count(&format!("account inspect {id} --refresh")))
                .collect()
        };
        let here = ComputerId::local();
        // Late: the caller gave up before the reply was filed.
        let h = harness(launch_replies(false), true);
        let runs = h.screens.launch_start(&here, false, None);
        h.screens.run(runs);
        assert!(
            h.screens.launch_waited(&here).is_empty(),
            "no catalog yet, no limits"
        );
        h.drain();
        assert_eq!(
            inspects(&h),
            [1, 1, 1, 1],
            "the late catalog asked for its limits"
        );
        assert_eq!(h.screens.launch_rows(&here).groups.len(), 4);
        h.drain();
        assert_eq!(inspects(&h), [1, 1, 1, 1], "once");
        // Direct: the reply is filed while the caller waits; it asks on waking.
        let h = harness(launch_replies(false), true);
        let runs = h.screens.launch_start(&here, false, None);
        h.screens.run(runs);
        h.drain();
        assert_eq!(
            inspects(&h),
            [0, 0, 0, 0],
            "the waiting caller asks, not the reply"
        );
        let runs = h.screens.launch_waited(&here);
        h.screens.run(runs);
        h.drain();
        assert_eq!(inspects(&h), [1, 1, 1, 1]);
        // A late catalog of a Refresh forces its limits.
        let runs = h.screens.launch_start(&here, true, None);
        h.screens.run(runs);
        assert!(
            h.screens.launch_waited(&here).is_empty(),
            "the earlier catalog is not this ask's answer"
        );
        h.drain();
        assert_eq!(
            inspects(&h),
            [2, 2, 2, 2],
            "the late Refresh catalog forced them, once"
        );
    }

    /// Every inspect of the launch fixture's local profiles so far.
    fn launch_inspects(h: &Harness) -> usize {
        ["work", "claude", "native-dsh", "native-opencode"]
            .iter()
            .map(|id| h.count(&format!("account inspect {id} --refresh")))
            .sum()
    }

    /// The dialog's ask as `launch_accounts` makes it, on the queued reads.
    fn launch_ask(h: &Harness, computer: &ComputerId, refresh: bool) {
        let runs = h.screens.launch_start(computer, refresh, None);
        h.screens.run(runs);
        h.drain();
        let runs = h.screens.launch_waited(computer);
        h.screens.run(runs);
        h.drain();
    }

    // Qt uses a Refresh's force once (`std::exchange(m_forceUsage, false)`,
    // 385-387) and takes only its own request's reply: the page's later
    // polls of that computer never force the limits again.
    #[test]
    fn a_dialog_refresh_forces_the_limits_once_while_overview_polls() {
        let h = harness(launch_replies(false), true);
        let here = ComputerId::local();
        h.screens.set_visible(Screen::Overview, true);
        h.drain();
        launch_ask(&h, &here, false);
        let opened = launch_inspects(&h);
        h.at(1_000);
        launch_ask(&h, &here, true);
        let refreshed = launch_inspects(&h);
        assert_eq!(
            refreshed,
            opened + 4,
            "the Refresh forced each profile once"
        );
        for at in [31_000, 62_000, 93_000, 124_000] {
            h.at(at);
            h.screens.tick();
            h.drain();
        }
        assert_eq!(
            h.count("account ls"),
            7,
            "the first read, the two asks and four polls"
        );
        assert_eq!(
            launch_inspects(&h),
            refreshed,
            "no later reply forced them again"
        );
    }

    // Two waiters of the same Refresh consume its force once, not once each.
    #[test]
    fn concurrent_launch_callers_take_the_refresh_force_once() {
        let h = harness(launch_replies(false), true);
        let here = ComputerId::local();
        let runs = h.screens.launch_start(&here, true, None);
        h.screens.run(runs);
        assert!(
            h.screens.launch_start(&here, true, None).is_empty(),
            "the second caller shares the read"
        );
        h.drain();
        let first = h.screens.launch_waited(&here);
        h.screens.run(first);
        h.drain();
        let second = h.screens.launch_waited(&here);
        h.screens.run(second);
        h.drain();
        assert_eq!(launch_inspects(&h), 4, "only the first waiter forces");
    }

    // Qt's dialog takes only its own request's reply (`m_accountRequest`,
    // 385, 394): a later catalog of its computer, such as the page's poll,
    // updates the shared store and is heard, but it is not the dialog's
    // answer. The next ask is answered again.
    #[test]
    fn a_later_catalog_is_not_the_dialogs_answer() {
        let h = harness(launch_replies(false), true);
        let heard = Arc::new(AtomicUsize::new(0));
        {
            let heard = Arc::clone(&heard);
            h.screens.on_launch_change(Arc::new(move |_: &ComputerId| {
                heard.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let here = ComputerId::local();
        h.screens.set_visible(Screen::Overview, true);
        h.drain();
        launch_ask(&h, &here, false);
        let answer = h.screens.launch_rows(&here);
        assert!(answer.loaded && answer.error.is_none(), "{answer:?}");
        assert_eq!(answer.groups.len(), 4);
        let busy = "hgs: account catalog busy\n";
        h.fake
            .set_reply("account ls", json!({"stderr": busy, "exit": 1}));
        let before = heard.load(Ordering::SeqCst);
        h.at(31_000);
        h.screens.tick();
        assert_eq!(h.queued(), 1, "the page's poll");
        h.drain();
        assert_eq!(h.count("account ls"), 3);
        assert!(
            heard.load(Ordering::SeqCst) > before,
            "the dialog hears of it"
        );
        let later = h.screens.launch_rows(&here);
        assert_eq!(later.error, None, "its failure is not the dialog's answer");
        assert_eq!(members(&later), members(&answer));
        launch_ask(&h, &here, true);
        assert_eq!(
            h.screens.launch_rows(&here).error.as_deref(),
            Some("Could not load accounts: hgs: account catalog busy"),
            "the Refresh's own reply answers it"
        );
    }

    // A failed answer to the dialog's own request asks for no limits, even
    // when the rows still show the last good catalog (NewSessionDialog 380-395).
    #[test]
    fn a_failed_dialog_answer_asks_for_no_limits() {
        let h = harness(launch_replies(false), true);
        let here = ComputerId::local();
        launch_ask(&h, &here, false);
        let before = launch_inspects(&h);
        h.fake.set_reply(
            "account ls",
            json!({"stderr": "hgs: account catalog busy\n", "exit": 1}),
        );
        let runs = h.screens.launch_start(&here, true, None);
        h.screens.run(runs);
        h.drain();
        let runs = h.screens.launch_waited(&here);
        assert!(runs.is_empty(), "a failed answer starts no limit reads");
        h.screens.run(runs);
        h.drain();
        assert_eq!(launch_inspects(&h), before);
        assert_eq!(
            h.screens.launch_rows(&here).error.as_deref(),
            Some("Could not load accounts: hgs: account catalog busy")
        );
    }

    // The page polls only the computers it lists: New session's computer
    // outside them is read on the dialog's asks, kept while the dialog
    // watches it, and no read of it holds back the Accounts Refresh.
    #[test]
    fn the_page_never_polls_the_dialogs_unlisted_computer() {
        let mut replies = launch_replies(false);
        replies["@lab account ls"] = reply(&launch_catalog(&[(
            "lab-claude",
            "claude",
            "lab@example.invalid",
        )]));
        let h = harness(replies, true);
        let lab = ComputerId::from("lab");
        h.screens.set_visible(Screen::Accounts, true);
        h.drain();
        launch_ask(&h, &lab, false);
        assert_eq!(h.count("@lab account ls"), 1);
        h.at(31_000);
        h.screens.tick();
        assert_eq!(h.queued(), 1, "only the listed computer's catalog fell due");
        h.run(0);
        assert_eq!(h.queued(), 0);
        h.at(32_000);
        assert!(h.screens.accounts(true).busy, "the Refresh started at once");
        assert!(h.queued() > 0);
        h.drain();
        assert_eq!(h.count("account ls"), 3);
        for at in [62_000, 93_000] {
            h.at(at);
            h.screens.tick();
            h.drain();
        }
        assert_eq!(h.count("@lab account ls"), 1, "never polled");
        assert_eq!(
            members(&h.screens.launch_rows(&lab)),
            [vec!["lab-claude"]],
            "kept while the dialog watches it"
        );
    }

    // Qt's dialog reads the selected computer directly (865-874): one the
    // Accounts page does not list is read, kept while the dialog watches it
    // and forgotten after; one hgs cannot read answers with its error.
    #[test]
    fn a_computer_outside_the_accounts_hosts_is_read_for_the_dialog() {
        let mut replies = launch_replies(false);
        replies["@lab account ls"] = reply(&launch_catalog(&[(
            "lab-claude",
            "claude",
            "lab@example.invalid",
        )]));
        replies["@gone account ls"] = json!({"stderr": "hgs: Unknown machine gone.\n", "exit": 2});
        let h = build(replies, true, true);
        let lab = ComputerId::from("lab");
        let view = h.screens.launch_accounts(&lab, false, None).unwrap();
        assert!(view.loaded && view.error.is_none(), "{view:?}");
        assert_eq!(members(&view), [vec!["lab-claude"]]);
        h.screens.tick();
        assert_eq!(
            members(&h.screens.launch_rows(&lab)),
            [vec!["lab-claude"]],
            "the hub's step keeps it while the dialog watches it"
        );
        assert_eq!(h.count("@lab account ls"), 1, "no warm-up read of its own");
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            h.fake
                .calls()
                .iter()
                .all(|call| !call.starts_with("@lab account inspect")),
            "a computer without a live stream reads no limits"
        );
        let gone = h
            .screens
            .launch_accounts(&ComputerId::from("gone"), false, None)
            .unwrap();
        assert!(gone.loaded && gone.groups.is_empty());
        assert_eq!(
            gone.error.as_deref(),
            Some("Could not load accounts: hgs: Unknown machine gone.")
        );
        h.screens.launch_close();
        h.screens.tick();
        let hub = h.screens.lock();
        assert!(
            !hub.accounts.catalogs.contains_key(&lab)
                && !hub
                    .accounts
                    .catalogs
                    .contains_key(&ComputerId::from("gone")),
            "forgotten once the dialog closed"
        );
    }

    // Review: updateAccounts groups only when the catalog arrives (879-883);
    // later limits refresh those rows (refreshUsage 990-1000).
    #[test]
    fn later_limits_never_regroup_the_dialogs_rows_until_the_next_catalog() {
        let rows = [
            ("claude-a", "claude", "a@example.invalid"),
            ("claude-b", "claude", "b@example.invalid"),
        ];
        let mut replies = json!({"account ls": reply(&launch_catalog(&rows))});
        for (id, provider, _) in rows {
            replies[format!("account inspect {id} --refresh")] =
                inspect(id, provider, "same@example.invalid");
        }
        let h = build(replies, true, true);
        let local_id = ComputerId::local();
        let first = h.screens.launch_accounts(&local_id, false, None).unwrap();
        assert_eq!(members(&first), [vec!["claude-a"], vec!["claude-b"]]);
        eventually("both limits arrived", || {
            h.screens
                .launch_rows(&local_id)
                .groups
                .iter()
                .all(|g| g.usage.identity.email == "same@example.invalid")
        });
        let later = h.screens.launch_rows(&local_id);
        assert_eq!(
            members(&later),
            [vec!["claude-a"], vec!["claude-b"]],
            "still two rows"
        );
        assert_eq!(later.groups[0].key, first.groups[0].key);
        assert_eq!(later.groups[1].members[0].usage.checked_at, Some(200.0));
        // The next catalog groups them as one account.
        let joined = h.screens.launch_accounts(&local_id, true, None).unwrap();
        assert_eq!(members(&joined), [vec!["claude-a", "claude-b"]]);
    }

    // loadAccounts clears the rows for another computer (869-871); a failure
    // reads "Could not load accounts: %1" with failureDetail, and Refresh
    // keeps the rows it had.
    #[test]
    fn a_computer_change_clears_the_rows_and_a_failure_reads_as_qt() {
        let h = build(launch_replies(false), true, true);
        h.set_fleet(vec![local(), peer("studio", "live", 0)]);
        let studio = ComputerId::from("studio");
        let here = ComputerId::local();
        assert_eq!(
            h.screens
                .launch_accounts(&studio, false, None)
                .unwrap()
                .groups
                .len(),
            2
        );
        h.fake.set_reply("@studio account ls", json!({"exit": 3}));
        h.screens.launch_accounts(&here, false, None).unwrap();
        let failed = h.screens.launch_accounts(&studio, false, None).unwrap();
        assert!(failed.loaded);
        assert_eq!(
            failed.error.as_deref(),
            Some("Could not load accounts: exit code 3")
        );
        assert!(
            failed.groups.is_empty() && failed.profiles.is_empty(),
            "nothing of the earlier catalog"
        );
        assert!(
            h.screens.launch_rows(&here).groups.is_empty(),
            "another computer has no rows now"
        );
        let kept = h.screens.launch_accounts(&here, false, None).unwrap();
        assert_eq!(kept.groups.len(), 4);
        h.fake.set_reply("account ls", json!({"stdout": "[]"}));
        let refreshed = h.screens.launch_accounts(&here, true, None).unwrap();
        assert_eq!(
            refreshed.error.as_deref(),
            Some("Could not load accounts: Invalid account catalog response")
        );
        assert_eq!(refreshed.groups.len(), 4, "Refresh keeps the rows it had");
        h.fake.set_reply(
            "account ls",
            json!({"stderr": "hgs: account catalog busy\n", "exit": 1}),
        );
        let busy = h.screens.launch_accounts(&here, true, None).unwrap();
        assert_eq!(
            busy.error.as_deref(),
            Some("Could not load accounts: hgs: account catalog busy")
        );
        h.fake
            .set_reply("account ls", launch_replies(false)["account ls"].clone());
        assert_eq!(
            h.screens.launch_accounts(&here, true, None).unwrap().error,
            None
        );
    }

    #[test]
    fn the_dialogs_computer_hears_of_its_replies_until_it_closes() {
        let h = harness(launch_replies(false), true);
        let heard = Arc::new(Mutex::new(Vec::new()));
        {
            let heard = Arc::clone(&heard);
            h.screens
                .on_launch_change(Arc::new(move |computer: &ComputerId| {
                    heard.lock().unwrap().push(computer.clone());
                }));
        }
        h.set_fleet(vec![local(), peer("studio", "live", 0)]);
        let runs = h
            .screens
            .launch_start(&ComputerId::from("studio"), false, None);
        h.screens.run(runs);
        h.drain();
        let runs = h.screens.launch_usage(&ComputerId::from("studio"), false);
        h.screens.run(runs);
        h.drain();
        let got = heard.lock().unwrap().clone();
        assert!(got.len() >= 3, "the catalog and both inspects: {got:?}");
        assert!(
            got.iter()
                .all(|computer| computer == &ComputerId::from("studio")),
            "only the dialog's computer"
        );
        // Closed: its later replies tell nobody.
        let runs = h.screens.launch_usage(&ComputerId::from("studio"), true);
        h.screens.launch_close();
        h.screens.run(runs);
        h.drain();
        assert_eq!(heard.lock().unwrap().len(), got.len());
        assert!(!h.screens.launch_rows(&ComputerId::from("studio")).loaded);
    }

    // NewSessionDialog 221-222: the requested agent joins the dialog's agents.
    #[test]
    fn a_requested_agent_joins_the_provider_order() {
        let rows = [
            ("alpha-a", "alpha", "a@example.invalid"),
            ("zeta-a", "zeta", "z@example.invalid"),
            ("work", "codex", "w@example.invalid"),
        ];
        let h = build(
            json!({"account ls": reply(&launch_catalog(&rows))}),
            true,
            true,
        );
        let order = |agent: Option<&str>| -> Vec<String> {
            let view = h
                .screens
                .launch_accounts(&ComputerId::local(), false, agent)
                .unwrap();
            view.groups.iter().map(|g| g.provider.clone()).collect()
        };
        assert_eq!(order(Some("zeta")), ["codex", "zeta", "alpha"]);
        assert_eq!(order(None), ["codex", "alpha", "zeta"]);
        assert_eq!(
            order(Some("sh")),
            ["codex", "alpha", "zeta"],
            "the shell is never a provider"
        );
    }

    #[test]
    fn a_replay_without_hgs_reads_no_launch_accounts() {
        let h = harness(json!({}), false);
        assert_eq!(
            h.screens.launch_accounts(&ComputerId::local(), true, None),
            Err(REPLAY_NEEDS_HGS.to_owned())
        );
        assert_eq!(h.queued(), 0);
        assert!(h.fake.calls().is_empty());
    }

    fn eventually(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < until, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The one test on real threads, as the app runs its reads: two slow
    /// inspects overlap, and every read still answers once.
    #[test]
    fn reads_run_on_their_own_threads() {
        let mut replies = local_accounts();
        for call in [INSPECTS[0], INSPECTS[2]] {
            replies[call]["sleep"] = json!(0.3);
        }
        let h = build(replies, true, true);
        h.screens.set_visible(Screen::Accounts, true);
        eventually("every inspect answered", || {
            let view = h.screens.accounts(false);
            view.pending == 0 && !view.busy && view.snapshot
        });
        for call in INSPECTS {
            assert_eq!(h.count(call), 1, "{call}");
        }
        assert_eq!(
            member(&h.screens.accounts(false), "claude-work")
                .usage
                .status,
            "ok"
        );
    }

    #[test]
    fn a_sessions_usage_read_is_bounded_and_releases_its_shared_slot() {
        const CALL: &str = "account inspect --session claude/sample-project/review --refresh";
        let h = harness(
            json!({
                CALL: {"stdout": json!({"status": "ok", "id": "claude-work", "provider": "claude", "run_id": "run-1", "checked_at": 123, "windows": []}).to_string()}
            }),
            true,
        );
        let target = SessionUsageRef {
            host: ComputerId::local(),
            name: "claude/sample-project/review".into(),
            run_id: "run-1".into(),
        };
        let first = h.screens.session_usage(Some(target.clone()), false);
        assert!(first.refreshing);
        h.drain();
        let view = h.screens.session_usage(Some(target.clone()), false);
        assert_eq!(view.id, "claude-work");
        assert_eq!(view.checked_at, Some(123.0));
        assert!(!view.refreshing && !view.refresh_error);
        assert_eq!(h.count(CALL), 1);
        h.screens.session_usage(Some(target.clone()), true);
        h.drain();
        assert_eq!(h.count(CALL), 2);
        h.at(300_000);
        h.screens.tick();
        h.drain();
        assert_eq!(
            h.count(CALL),
            3,
            "the timed refresh can start after completion"
        );
        h.screens.session_usage(None, false);
        h.at(600_000);
        h.screens.tick();
        assert_eq!(h.count(CALL), 3, "no selected session starts no more reads");
    }
}
