//! One owned desktop subscriber and closed native intents. No daemon launch or legacy fallback.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak,
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, watch, Notify},
};
use zerus_client_core::{
    desktop::{
        drain::{Gate, Role},
        service::{self, CatalogPage, ChangePage, Revision, Snapshot},
    },
    Change, ComputerId, ComputerStatus,
};

const RPC_WAIT: Duration = Duration::from_secs(5);
const HELP: &str = "Desktop service ABI: desktop-v1";
const SOURCES: usize = 64;
const KEY_BYTES: usize = 32 << 20;
const CATALOG_KEYS: usize = 1_000_000;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) mod activation;

#[derive(Default)]
pub(crate) struct Observers {
    #[cfg(all(test, target_os = "linux"))]
    pub mode_helpers: Option<PathBuf>,
    pub projections: Vec<Projection>,
    pub notification_controls: Option<crate::desktop_notification_controls::Setup>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub activation: Option<activation::Sink>,
}

pub(crate) mod audit;
mod cache;
mod catalog;
mod catalog_relay;
/// The composer's "Compact and continue" handed to the service
/// (zerus-5wmh.66.4.8, slice 3). Only Linux and macOS route it; Windows
/// keeps it direct.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) mod compact_to_send;
pub(crate) mod creation_clone;
pub(crate) mod creation_launch;
pub(crate) mod creation_worktree;
// Its one caller, the task editor's Service-mode routing, lands together with
// attachments by reference (zerus-5wmh.66.4.4.2); until then it is unused.
/// A question card's answer handed to the service (zerus-5wmh.66.4.7,
/// slice 3). Only Linux and macOS route it; Windows keeps every answer direct.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) mod deferred_answer;
/// The composer's Send handed to the service (zerus-5wmh.66.4.4, slice 3).
/// Only Linux and macOS route it; Windows keeps every Send direct.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) mod deferred_send;
/// The composer's "Resume and send" handed to the service
/// (zerus-5wmh.66.4.9, slice 3). Only Linux and macOS route it; Windows
/// keeps it direct.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) mod session_resume;
#[allow(dead_code)]
pub(crate) mod task_message;
// Only Linux advertises the review; elsewhere its probe refuses.
pub(crate) mod task_message_inputs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod task_start_reconcile;
use catalog::catalog_loop;
mod details;
mod fleet_requests;
pub mod frontend;
mod mode_transition;
pub(crate) mod notification_controls;
mod question_route_activity;
mod question_route_child;
mod read_import;
mod reconnect;
mod state_refusal;
mod status;
pub(crate) mod termination_outcome;

/// Registered at setup before start_source. Some contains a verified service
/// watch frame; None invalidates the prior projection. Callbacks must not block.
pub type Projection = Arc<dyn Fn(Option<&Value>) + Send + Sync>;

/// Captured before startup exclusion and before any producer is constructed.
// 0: configured startup; 1: consumer awaiting explicit recovery; 2: resumed consumer.
static STARTUP_RECOVERY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

static MODE: OnceLock<bool> = OnceLock::new();

/// Capture the source before the canonical responder exists. A missing or
/// unconfirmed registration keeps the existing guarded Native startup.
pub(crate) fn prepare_mode(eligible: bool) {
    MODE.get_or_init(|| {
        let value = std::env::var_os(service::MODE_ENV);
        startup_mode(
            value.as_deref(),
            eligible,
            &zerus_client_core::default_hgs(),
        )
    });
}

fn startup_mode(value: Option<&std::ffi::OsStr>, eligible: bool, hgs: &std::path::Path) -> bool {
    let registered = value.is_none()
        && eligible
        && cfg!(any(target_os = "linux", target_os = "macos"))
        && mode_transition::registered_for_default(hgs);
    service::enabled(value.map(|value| value.to_str().unwrap_or("")), registered)
}

pub fn mode() -> bool {
    STARTUP_RECOVERY.load(Ordering::Acquire) != 0
        || *MODE.get_or_init(|| {
            let value = std::env::var_os(service::MODE_ENV);
            // An invalid explicit value is distinct from the platform default.
            service::enabled(
                value.as_deref().map(|value| value.to_str().unwrap_or("")),
                false,
            )
        })
}

/// Before any GUI producer exists, a failed native takeover can only move to
/// the consumer. This never changes the manager environment or grants a writer.
pub(crate) fn enter_startup_recovery() {
    STARTUP_RECOVERY.store(1, Ordering::Release);
}

fn startup_recovery() -> bool {
    STARTUP_RECOVERY.load(Ordering::Acquire) == 1
}

/// Task #440: whether the window still waits in startup recovery.
pub(crate) fn in_startup_recovery() -> bool {
    startup_recovery()
}

/// Task #440 (C): when the window restarts itself to finish starting (unix
/// ms; 0 = no restart planned), shown with the takeover refusal.
static RECOVERY_RESTART_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Task 180: the code of the check that refused the native takeover (a fixed
/// code, never child text), shown with its next step instead of an empty
/// list or a bare "not ready".
static STARTUP_RECOVERY_REASON: std::sync::Mutex<&'static str> = std::sync::Mutex::new("");

pub(crate) fn record_startup_recovery_reason(code: &'static str) {
    *STARTUP_RECOVERY_REASON.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = code;
}

fn startup_recovery_reason() -> Option<Value> {
    let code = *STARTUP_RECOVERY_REASON.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (startup_recovery() && !code.is_empty()).then(|| json!({"code": code, "check": crate::takeover_refusal::check(code)}))
}

fn ownership_display(
    view: crate::desktop_ownership::View,
    activation: mode_transition::View,
    business_ready: bool,
) -> Option<Value> {
    let mut value = serde_json::to_value(view).ok()?;
    value["native_recovery"] = json!(startup_recovery());
    if let Some(reason) = startup_recovery_reason() {
        value["native_recovery_reason"] = reason;
    }
    let restart_at = RECOVERY_RESTART_AT.load(Ordering::Acquire);
    if startup_recovery() && restart_at > 0 {
        value["native_recovery_restart_at"] = json!(restart_at);
    }
    value["activation"] = serde_json::to_value(activation).ok()?;
    value["activation"]["business_ready"] = business_ready.into();
    Some(value)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Retry(u64),
    Reconnect(u64),
    Stop,
}

#[derive(Default)]
struct Dirty {
    sources: BTreeMap<String, SourceVersion>,
    heads: BTreeMap<String, Revision>,
    progress: BTreeMap<String, Revision>,
    pending: BTreeSet<String>,
    halted: BTreeSet<String>,
    running: Option<String>,
    next: u64,
}
impl Dirty {
    fn restart(&mut self, computer: &str) -> Result<Option<SourceVersion>, &'static str> {
        let Some(source) = self.sources.get_mut(computer) else {
            return Ok(None);
        };
        self.next = self
            .next
            .checked_add(1)
            .ok_or("source_generation_exhausted")?;
        source.serial = self.next;
        self.heads.remove(computer);
        self.progress.remove(computer);
        self.halted.remove(computer);
        self.pending.insert(computer.into());
        Ok(Some(source.clone()))
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct SourceVersion {
    incarnation: u64,
    generation: u64,
    projection: Option<String>,
    online: bool,
    serial: u64,
}
impl SourceVersion {
    fn from(value: &Value) -> Self {
        Self {
            incarnation: value["incarnation"].as_u64().unwrap_or(0),
            generation: value["generation"].as_u64().unwrap_or(0),
            projection: value["projection"].as_str().map(str::to_owned),
            online: value["online"] == true,
            serial: 0,
        }
    }
    fn identity(&self) -> service::CatalogIdentity {
        service::CatalogIdentity {
            incarnation: self.incarnation,
            generation: self.generation,
            projection: self.projection.clone(),
        }
    }
    fn matches(&self, current: &Self) -> bool {
        self.serial == current.serial
            && self.incarnation == current.incarnation
            && self.generation == current.generation
            && self.online == current.online
            && self
                .projection
                .as_ref()
                .is_none_or(|p| current.projection.as_ref() == Some(p))
    }
}

pub enum Data {
    Change(Box<Change>),
    Catalog(CatalogDelivery),
    Detail(details::Delivery),
}

/// Display provenance for a per-session receipt, never writer authority.
/// Cache projection/cursor retirement does not change the original source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationScope {
    computer: String,
    configuration: String,
    epoch: String,
    incarnation: u64,
    generation: u64,
}

#[cfg(test)]
impl NotificationScope {
    /// A service scope for pump tests; real scopes come from catalog delivery.
    pub(crate) fn for_test(computer: &str) -> Self {
        Self {
            computer: computer.into(),
            configuration: "test-configuration".into(),
            epoch: "test-epoch".into(),
            incarnation: 1,
            generation: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NotificationReceipt {
    pub(crate) scope: NotificationScope,
    pub(crate) key: String,
    pub(crate) provider: String,
}

/// Recheck identity at the view drain, not just when an RPC finishes. Frames
/// already queued before a removal, reconnect or new generation cannot reappear.
pub struct CatalogDelivery {
    shared: Weak<Shared>,
    computer: String,
    epoch: String,
    source: SourceVersion,
    stopped: Arc<AtomicBool>,
    value: CatalogValue,
}
enum CatalogValue {
    Row(Box<Change>),
    /// A delta removal after its revision's upserts. One revision is one
    /// native catalog frame, whose removal is `replaced` when that frame lists
    /// the session's name again (`listed`); the journal drops the reason.
    Removal {
        key: String,
        listed: Arc<BTreeSet<String>>,
    },
    Finish {
        through: Option<Revision>,
        keys: Option<BTreeSet<String>>,
        done: oneshot::Sender<()>,
    },
}
impl CatalogDelivery {
    pub fn apply(self, pump: &crate::pump::Pump, observe: &impl Fn(&Change)) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // Same lock order as Shared::current. Hold admission through the push so
        // an invalidation cannot race the last membership decision.
        let snapshot = shared.snapshot.lock();
        let mut dirty = shared.dirty.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stopped.load(Ordering::Acquire)
            || shared.stopped.load(Ordering::Acquire)
            || snapshot.epoch.as_deref() != Some(&self.epoch)
            || dirty.sources.get(&self.computer) != Some(&self.source)
        {
            return;
        }
        if let Some(configuration) = shared
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            pump.notification_scope(
                &self.computer,
                NotificationScope {
                    computer: self.computer.clone(),
                    configuration,
                    epoch: self.epoch.clone(),
                    incarnation: self.source.incarnation,
                    generation: self.source.generation,
                },
            );
        }
        match self.value {
            CatalogValue::Row(mut change) => {
                if let Change::Upsert { row, .. } = change.as_mut() {
                    row.catalog_source = Some((
                        self.epoch.clone(),
                        self.source.incarnation,
                        self.source.generation,
                    ));
                }
                observe(&change);
                pump.push(*change);
                shared.details_changed.notify_one();
            }
            CatalogValue::Removal { key, listed } => {
                // The serial drain's mirror holds exactly the rows delivered
                // before this removal, so the removed run's name is current.
                let reason = pump
                    .row(&self.computer, &key)
                    .filter(|old| listed.contains(&old.name))
                    .map(|_| "replaced".to_owned());
                let change = Change::Remove {
                    computer: ComputerId(self.computer),
                    key,
                    reason,
                };
                observe(&change);
                pump.push(change);
                shared.details_changed.notify_one();
            }
            CatalogValue::Finish {
                through,
                keys,
                done,
            } => {
                // Only baseline PLUS a fully consumed contiguous delta walk
                // establishes membership, even when a newer hint is pending.
                let clean = through.is_some_and(|through| {
                    dirty
                        .heads
                        .get(&self.computer)
                        .is_none_or(|head| *head <= through)
                });
                if let Some(through) = through {
                    dirty.progress.insert(self.computer.clone(), through);
                }
                if clean || through.is_none() {
                    dirty.pending.remove(&self.computer);
                } else {
                    dirty.pending.insert(self.computer.clone());
                    shared.changed.notify_one();
                }
                let computer = ComputerId(self.computer);
                if let (Some(keys), Some(_)) = (keys, through) {
                    pump.service_catalog_finish(&computer, &keys);
                }
                // A contiguous delta range remains live even when a newer head
                // needs another catch-up. Pending display changes do not mean
                // this computer lost its connection, and neither does a still
                // staging view of an online source that was already Live: its
                // completion hint re-baselines it (zerus-bkbb).
                let live = through.is_some() || (self.source.online && pump.live(&computer.0));
                let change = Change::Computer {
                    computer,
                    status: if live {
                        ComputerStatus::Live
                    } else {
                        ComputerStatus::Reconnecting { attempt: 0 }
                    },
                };
                observe(&change);
                pump.push(change);
                let _ = done.send(());
                shared.details_changed.notify_one();
            }
        }
    }
}

/// Immutable checked read identity, independent of transient display state.
/// It grants no manager, process or writer authority.
#[derive(Clone)]
struct ReadOnlyOriginal {
    configuration: String,
    epoch: String,
    generation: u64,
}

struct Shared {
    /// Test fence: the original frame's report wake has returned. A hello
    /// projection alone runs earlier and can conceal a missing job wake.
    #[cfg(test)]
    report_frame_finished: std::sync::atomic::AtomicUsize,
    creation_rpc: Gate,
    notification_controls: Arc<crate::desktop_notification_controls::Controller>,
    notification_supported: AtomicBool,
    ownership: Arc<crate::desktop_ownership::Controller>,
    ownership_supported: AtomicBool,
    mode_transition: Arc<mode_transition::Controller>,
    hgs: PathBuf,
    configuration: Mutex<Option<String>>,
    audit_help: audit::help::Cache,
    /// Task 176: hgs's refusal of a desktop state folder that is not private.
    state_refusal: state_refusal::Seen,
    relay: Mutex<Option<Arc<zerus_client_core::desktop::relay::Connection>>>,
    pump: crate::pump::Pump,
    sink: Mutex<Option<SyncSender<Data>>>,
    /// Admission decisions read the locked state; status reads its copy.
    snapshot: status::Status,
    // Bounded display-only names from a checked runtime roster. Missing or
    // disconnected projections retain last-known choices, never a known zero.
    peer_choices: Mutex<Option<BTreeSet<String>>>,
    dirty: Mutex<Dirty>,
    changed: Notify,
    rpc: Gate,
    cache_rpc: Gate,
    cache: Mutex<Option<cache::Pending>>,
    // Long activation reads never consume fleet/details admission capacity.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    activation_rpc: Gate,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
    activation_retry: Mutex<Option<Duration>>,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
    activation_backoffs: std::sync::atomic::AtomicUsize,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
    activation_passive_waits: std::sync::atomic::AtomicUsize,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
    activation_permanent: AtomicBool,
    allowed: bool,
    stopped: AtomicBool,
    /// Once observed, connection checks cannot retry imports or startup Grant.
    read_only_reconnect: AtomicBool,
    read_only_original: OnceLock<ReadOnlyOriginal>,
    cancellation: Mutex<Option<Arc<AtomicBool>>>,
    responder: Option<Arc<zerus_client_core::desktop::Responder>>,
    reads: Mutex<BTreeMap<String, ReadIntent>>,
    details: Mutex<details::Hub>,
    details_changed: Notify,
    organization: Mutex<Option<zerus_client_core::OrganizationSnapshot>>,
    frontend: frontend::Frontend,
    /// The reviewed observations a confirmation may name, each sent once.
    held_inputs: task_message_inputs::Reviewed,
    read_import: Mutex<read_import::State>,
    projections: Vec<Projection>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    activation: Option<activation::Sink>,
}

struct ReadIntent {
    id: String,
    in_flight: bool,
    uncertain: bool,
}

pub struct Service {
    shared: Arc<Shared>,
    control: watch::Sender<Control>,
    join: Mutex<Option<oneshot::Receiver<()>>>,
    drain: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Service {
    pub(crate) fn notification_receipt(
        &self,
        target: &zerus_client_core::actions::Target,
    ) -> Option<NotificationReceipt> {
        let scope = self.notification_scope(&target.computer)?;
        self.shared.pump.notification_receipt(scope, target)
    }

    pub(crate) fn notification_scope(&self, computer: &str) -> Option<NotificationScope> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return None;
        }
        let snapshot = self.shared.snapshot.lock();
        if !snapshot.connected {
            return None;
        }
        let dirty = self
            .shared
            .dirty
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let source = dirty.sources.get(computer).filter(|source| source.online)?;
        Some(NotificationScope {
            computer: computer.into(),
            configuration: self
                .shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()?,
            epoch: snapshot.epoch.clone()?,
            incarnation: source.incarnation,
            generation: source.generation,
        })
    }

    pub(crate) fn apply_notification_preferences(
        &self,
        expected: &NotificationReceipt,
        target: &zerus_client_core::actions::Target,
        preferences: &Value,
    ) {
        let scope = &expected.scope;
        if self.shared.stopped.load(Ordering::Acquire) || scope.computer != target.computer {
            return;
        }
        // Hold the same scope admission as catalog delivery through Pump publish.
        let snapshot = self.shared.snapshot.lock();
        let dirty = self
            .shared
            .dirty
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !snapshot.connected
            || snapshot.epoch.as_ref() != Some(&scope.epoch)
            || self
                .shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                != Some(&scope.configuration)
            || !dirty.sources.get(&target.computer).is_some_and(|source| {
                source.online
                    && source.incarnation == scope.incarnation
                    && source.generation == scope.generation
            })
        {
            return;
        }
        self.shared
            .pump
            .apply_notification_preferences(expected, target, preferences);
    }

    #[cfg(all(test, unix))]
    pub fn start(
        hgs: PathBuf,
        pump: crate::pump::Pump,
        sink: SyncSender<Data>,
        allowed: bool,
        drain: std::thread::JoinHandle<()>,
        responder: Option<Arc<zerus_client_core::desktop::Responder>>,
        projections: Vec<Projection>,
    ) -> Arc<Self> {
        Self::start_observed(
            hgs,
            pump,
            sink,
            allowed,
            drain,
            responder,
            Observers {
                #[cfg(all(test, target_os = "linux"))]
                mode_helpers: None,
                projections,
                notification_controls: None,
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                activation: None,
            },
        )
    }
    pub(crate) fn start_observed(
        hgs: PathBuf,
        pump: crate::pump::Pump,
        sink: SyncSender<Data>,
        allowed: bool,
        drain: std::thread::JoinHandle<()>,
        responder: Option<Arc<zerus_client_core::desktop::Responder>>,
        observers: Observers,
    ) -> Arc<Self> {
        let ownership = crate::desktop_ownership::Controller::new(
            hgs.clone(),
            if allowed { responder.clone() } else { None },
            zerus_client_core::desktop::ownership::Mode::Service,
        );
        let shared = Arc::new(Shared {
            #[cfg(test)]
            report_frame_finished: std::sync::atomic::AtomicUsize::new(0),
            creation_rpc: Gate::new(1).unwrap(),
            notification_controls: crate::desktop_notification_controls::Controller::new(
                observers.notification_controls,
            ),
            notification_supported: AtomicBool::new(false),
            ownership,
            ownership_supported: AtomicBool::new(false),
            mode_transition: {
                let owner = mode_transition::Controller::new();
                #[cfg(all(test, target_os = "linux"))]
                owner.fixture_helpers(observers.mode_helpers);
                owner
            },
            hgs,
            relay: Mutex::new(None),
            pump,
            sink: Mutex::new(Some(sink)),
            snapshot: status::Status::new(Snapshot::default()),
            peer_choices: Mutex::new(None),
            dirty: Mutex::new(Dirty::default()),
            changed: Notify::new(),
            rpc: Gate::new(4).unwrap(),
            cache_rpc: Gate::new(1).unwrap(),
            cache: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            activation_rpc: Gate::new(1).unwrap(),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
            activation_retry: Mutex::new(None),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
            activation_backoffs: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
            activation_passive_waits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
            activation_permanent: AtomicBool::new(false),
            allowed,
            stopped: AtomicBool::new(false),
            read_only_reconnect: AtomicBool::new(false),
            read_only_original: OnceLock::new(),
            cancellation: Mutex::new(None),
            configuration: Mutex::new(None),
            audit_help: audit::help::Cache::default(),
            state_refusal: state_refusal::Seen::default(),
            responder,
            reads: Mutex::new(BTreeMap::new()),
            details: Mutex::new(details::Hub::default()),
            details_changed: Notify::new(),
            organization: Mutex::new(None),
            frontend: frontend::Frontend::default(),
            held_inputs: task_message_inputs::Reviewed::default(),
            read_import: Mutex::new(read_import::State::default()),
            projections: observers.projections,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            activation: observers.activation,
        });
        let weak = Arc::downgrade(&shared);
        shared.notification_controls.hook(Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.publish_notification_controls();
            }
        }));
        let (control, receiver) = watch::channel(Control::Retry(0));
        let (finished, join) = oneshot::channel();
        let service = Arc::new(Self {
            shared: shared.clone(),
            control,
            join: Mutex::new(Some(join)),
            drain: Mutex::new(Some(drain)),
        });
        shared.publish(Snapshot::default());
        tauri::async_runtime::spawn(async move {
            run(shared, receiver).await;
            let _ = finished.send(());
        });
        service
    }
    /// The service status, without waiting for a view callback that holds
    /// the admission lock (zerus-hj2s).
    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot.copy()
    }
    pub(crate) async fn ownership_rearm(self: &Arc<Self>) -> crate::desktop_ownership::View {
        // An uncertain activation has its own retained original. Resume never
        // replaces it with a Rearm or a new manager attempt.
        if self.shared.read_checks_only()
            || self.shared.mode_transition.blocks_resume()
            || self.shared.ownership.view().pending
        {
            return self.ownership_result().await;
        }
        self.shared.mode_transition.confirmed_scheduling();
        let view = if self.shared.ownership.granted() {
            self.shared.ownership.view()
        } else {
            self.shared
                .ownership
                .request(
                    Some(zerus_client_core::desktop::ownership::Action::Rearm),
                    None,
                    None,
                )
                .await
        };
        if self.shared.ownership.granted() {
            let configuration = self
                .shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(configuration) = configuration {
                self.shared
                    .mode_transition
                    .activate(self.shared.hgs.clone(), configuration)
                    .await;
            }
        }
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Rearm)
            .await;
        if self.shared.ownership.granted() {
            let _ = STARTUP_RECOVERY.compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
            // Rearm already made its one explicit report attempt above.
            self.wake_connection(true);
        }
        view
    }
    /// Task #440 (A): one read-only re-check while the window waits in
    /// startup recovery. When a running service now grants ownership,
    /// `ownership_result` leaves recovery (1 -> 2) and the connection is woken,
    /// so the list loads without a window restart. Nothing is started or
    /// acted on; true once recovery has been left.
    pub(crate) async fn recheck_recovery(&self) -> bool {
        if !startup_recovery() || self.shared.stopped.load(Ordering::Acquire) {
            return false;
        }
        let _ = self.ownership_result().await;
        if startup_recovery() {
            return false;
        }
        self.wake_connection(false);
        true
    }
    /// Task #440 (C): the window restarts itself at `at` (unix ms); the
    /// ownership notice says so ahead of it.
    pub(crate) async fn announce_restart(&self, at: u64) {
        RECOVERY_RESTART_AT.store(at, Ordering::Release);
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Result)
            .await;
    }
    pub(crate) async fn ownership_result(&self) -> crate::desktop_ownership::View {
        let view = self.shared.ownership.request(None, None, None).await;
        let configuration = self
            .shared
            .configuration
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(configuration) = configuration {
            self.shared
                .mode_transition
                .observe(self.shared.hgs.clone(), configuration)
                .await;
        }
        if self.shared.ownership.granted() {
            let _ = STARTUP_RECOVERY.compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
        }
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Result)
            .await;
        view
    }
    /// The FIRST explicit background choice (zerus-yfbd.1). Consumes only the
    /// opaque continuation of its own native confirmation. A missing current
    /// Grant is never made up here (no Rearm or NewOrigin): it keeps its own
    /// explicit recovery action, and the native helper checks it again.
    async fn apply_first_choice(
        self: &Arc<Self>,
        prepared: Option<mode_transition::FirstPrepared>,
    ) -> Result<Value, String> {
        let Some(prepared) = prepared else {
            return Ok(Value::Null);
        };
        let transition = &self.shared.mode_transition;
        transition
            .validate_first(&prepared)
            .await
            .map_err(str::to_owned)?;
        if self.shared.read_checks_only() {
            return Err("manual_refresh_required".into());
        }
        // Consent is for the displayed scope, not a later CLI upgrade or a
        // replacement endpoint discovered while the dialog was open.
        match preflight_for(
            &self.shared,
            Arc::new(AtomicBool::new(false)),
            PreflightMode::FirstChoice(&prepared),
        )
        .await
        {
            Ok(configuration) if configuration != prepared.configuration() => {
                return Err("activation_scope_changed".into())
            }
            Ok(_) => (),
            Err(code) => return Err(code.into()),
        }
        if self.shared.read_checks_only() {
            return Err("manual_refresh_required".into());
        }
        // Preflight can wait; revalidate the SAME opaque continuation.
        transition
            .validate_first(&prepared)
            .await
            .map_err(str::to_owned)?;
        if self.shared.read_checks_only() {
            return Err("manual_refresh_required".into());
        }
        if !self.shared.ownership.granted() || !self.shared.ownership.view().code.is_empty() {
            return Err("ownership_rearm_required".into());
        }
        let offered = transition
            .apply_first(prepared)
            .await
            .map_err(str::to_owned)?;
        let settled = transition.first_settled(&offered);
        if settled {
            // This SAME native-confirmed choice settled. Observation alone
            // never enters this continuation or clears a prior deferral.
            transition.confirmed_scheduling();
        }
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Result)
            .await;
        if settled {
            // Read-only: the reconnect observes the background service; it is
            // not a trigger for Grant, import or manager work.
            self.wake_connection(true);
        }
        Ok(json!({ "request_id": offered }))
    }
    /// Consumes only the opaque continuation returned by the native-confirmation
    /// controller. Review and a later mode choice remain separate operations.
    async fn apply_mode_action(
        self: &Arc<Self>,
        prepared: Option<mode_transition::Prepared>,
    ) -> Result<Value, String> {
        use mode_transition::ReviewAction;
        if let Some(prepared) = prepared {
            let action = prepared.action();
            self.shared
                .mode_transition
                .validate(&prepared)
                .await
                .map_err(str::to_owned)?;
            if action != ReviewAction::Review && self.shared.read_checks_only() {
                return Err("manual_refresh_required".into());
            }
            if action != ReviewAction::Review {
                // Consent is for the original scope, not a later CLI upgrade or
                // replacement endpoint discovered while the dialog was open.
                match preflight_for(
                    &self.shared,
                    Arc::new(AtomicBool::new(false)),
                    PreflightMode::ReviewedChoice(&prepared),
                )
                .await
                {
                    Ok(configuration) if configuration != prepared.configuration() => {
                        return Err("activation_scope_changed".into())
                    }
                    Ok(_) | Err("service_unavailable") => (),
                    Err(code) => return Err(code.into()),
                }
                if self.shared.read_checks_only() {
                    return Err("manual_refresh_required".into());
                }
            }
            // Preflight can wait while the original mode charge makes its
            // ordinary observe busy. Revalidate the SAME opaque continuation.
            self.shared
                .mode_transition
                .validate(&prepared)
                .await
                .map_err(str::to_owned)?;
            if action != ReviewAction::Review && self.shared.read_checks_only() {
                return Err("manual_refresh_required".into());
            }
            // Review never reaches this branch. A new background choice may make
            // its explicit Rearm against the current connected service; an absent
            // actor supplies no inferred grant. The native mode helper independently
            // checks the actual database before any effect.
            let same_scope = self
                .shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref()
                == Some(prepared.configuration());
            if prepared.background()
                && same_scope
                && self.snapshot().connected
                && !self.shared.ownership.granted()
            {
                let check = self.shared.mode_transition.effect_check(&prepared);
                let shared = self.shared.clone();
                let checked = Arc::new(move || {
                    if shared.stopped.load(Ordering::Acquire) {
                        return Err("ownership_stopped");
                    }
                    if shared.read_checks_only() {
                        return Err("manual_refresh_required");
                    }
                    check()?;
                    if shared.read_checks_only() {
                        return Err("manual_refresh_required");
                    }
                    Ok(())
                });
                self.shared
                    .ownership
                    .request_checked(
                        zerus_client_core::desktop::ownership::Action::Rearm,
                        prepared.configuration().into(),
                        checked,
                    )
                    .await
                    .map_err(str::to_owned)?;
                if !self.shared.ownership.granted() {
                    self.shared
                        .publish_ownership(crate::desktop_ownership::Phase::Result)
                        .await;
                    return Err("ownership_unconfirmed".into());
                }
            }
            if action != ReviewAction::Review && self.shared.read_checks_only() {
                return Err("manual_refresh_required".into());
            }
            let offered = self.shared.mode_transition.apply(prepared).await;
            if let Some(offered) = offered
                .filter(|_| action == ReviewAction::Background && !self.shared.read_checks_only())
            {
                // This continuation still belongs to the SECOND confirmation.
                // No status/Retry/backoff callback can enter this positive path.
                let stop = Arc::new(AtomicBool::new(false));
                if let Ok(configuration) = preflight(&self.shared, stop.clone()).await {
                    let snapshot = self.snapshot();
                    if let (Some(epoch), Some(generation)) = (snapshot.epoch, snapshot.generation) {
                        if let Some(grant) = self
                            .shared
                            .ownership
                            .current_grant(configuration.clone(), stop.clone())
                            .await
                        {
                            if self
                                .shared
                                .mode_transition
                                .business_allowed(&grant, epoch, generation, offered)
                                .await
                                && !self.shared.read_checks_only()
                            {
                                // Only THIS settled, scoped native-confirmed
                                // background choice may clear an old deferral.
                                self.shared.mode_transition.confirmed_scheduling();
                                let _ = STARTUP_RECOVERY.compare_exchange(
                                    1,
                                    2,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                );
                                if self.shared.business_permitted() && !stop.load(Ordering::Acquire)
                                {
                                    self.shared.ownership.keep_epoch_refresh(grant, stop).await;
                                }
                            }
                        }
                    }
                }
            }
            if action != ReviewAction::Review {
                self.wake_connection(true);
            }
        }
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Result)
            .await;
        serde_json::to_value(self.shared.mode_transition.view())
            .map_err(|_| "activation_view_unavailable".into())
    }
    fn refresh_binding(&self) -> Result<ReadOnlyOriginal, &'static str> {
        self.shared
            .original_read_scope()?
            .cloned()
            .ok_or("refresh_original_unavailable")
    }
    async fn refresh_status(&self) -> Result<mode_transition::refresh::View, String> {
        let original = self.refresh_binding().map_err(str::to_owned)?;
        Ok(self
            .shared
            .mode_transition
            .observe_refresh(
                self.shared.hgs.clone(),
                original.configuration,
                original.epoch,
            )
            .await)
    }
    fn refresh_fence(
        &self,
        display: &mode_transition::refresh::Offer,
    ) -> Result<mode_transition::refresh::Fence, &'static str> {
        let original = self.refresh_binding()?;
        if original.configuration != display.configuration || original.epoch != display.epoch {
            return Err("refresh_scope_changed");
        }
        let shared = self.shared.clone();
        Ok(Arc::new(move || {
            if shared.stopped.load(Ordering::Acquire) {
                return Err("refresh_scope_changed");
            }
            let current = shared
                .original_read_scope()?
                .ok_or("refresh_scope_changed")?;
            if current.configuration != original.configuration
                || current.epoch != original.epoch
                || shared
                    .configuration
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_deref()
                    != Some(&original.configuration)
            {
                return Err("refresh_scope_changed");
            }
            Ok(())
        }))
    }
    /// Settings choices only: offline and generation-zero computers remain
    /// selectable. These names grant neither freshness nor writer authority.
    pub fn peers(&self) -> Option<BTreeSet<String>> {
        self.shared
            .peer_choices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub fn retry(&self) {
        // Task #54 removed the retained import that ran here; Retry only
        // reconnects.
        self.retry_connection();
    }
    /// A check belongs to the displayed original service and starts no source.
    pub fn check_status(&self, epoch: &str) -> Result<(), String> {
        if self.shared.stopped.load(Ordering::Acquire)
            || self
                .shared
                .original_read_scope()
                .ok()
                .flatten()
                .is_none_or(|original| original.epoch != epoch)
        {
            return Err("The original background service connection changed. Reopen Settings to check its status.".into());
        }
        self.retry_connection();
        Ok(())
    }

    fn retry_connection(&self) {
        self.wake_connection(false);
    }
    fn wake_connection(&self, read_only: bool) {
        if self.shared.stopped.load(Ordering::Acquire) {
            return;
        }
        if !read_only
            && !self.shared.read_checks_only()
            && !startup_recovery()
            && !self.shared.mode_transition.blocks_resume()
            && self.shared.mode_transition.startup_permitted()
        {
            read_import::retry(self.shared.clone());
        }
        if let Some(cancel) = self
            .shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            cancel.store(true, Ordering::Release);
        }
        self.control.send_modify(|control| {
            if let Control::Retry(n) | Control::Reconnect(n) = control {
                *control = n.checked_add(1).map_or(Control::Stop, |next| {
                    if read_only {
                        Control::Reconnect(next)
                    } else {
                        Control::Retry(next)
                    }
                });
            }
        });
    }
    /// Nudge a non-live catalog, preserving healthy and already-pending reads.
    /// The local Refresh entry point also reaches halted peers: service mode
    /// has no legacy GUI peer listing for the tray/UI Refresh loops to visit.
    /// The service's own supervisors of those computers end their restart
    /// waits too (see [`Self::nudge_fleet`]).
    pub fn reconnect(&self, computer: &str) -> bool {
        if self.shared.stopped.load(Ordering::Acquire) {
            return false;
        }
        let snapshot = self.shared.snapshot.lock();
        if !snapshot.connected {
            drop(snapshot);
            self.retry();
            return true;
        }
        self.nudge_fleet(fleet_requests::targets(&snapshot, computer));
        let mut dirty = self
            .shared
            .dirty
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let targets: Vec<_> = dirty
            .sources
            .keys()
            .filter(|target| {
                (computer.is_empty() || target.as_str() == computer)
                    && !dirty.progress.contains_key(*target)
                    && !dirty.pending.contains(*target)
                    && dirty.running.as_ref() != Some(*target)
            })
            .cloned()
            .collect();
        let mut changed = false;
        for target in targets {
            changed |= matches!(dirty.restart(&target), Ok(Some(_)));
        }
        if changed {
            self.shared.changed.notify_one();
        }
        changed
    }
    /// The read-only Activity lines of one local session's submitted
    /// question routes (zerus-sa9qn.33): one bounded service read by the
    /// session's durable ID. It sends, settles and grants nothing.
    pub async fn question_route_activity(&self, session_id: String) -> Result<Value, &'static str> {
        if !question_route_activity::canonical(&session_id) {
            return Err("invalid_question_route_activity");
        }
        let stop = self
            .shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or("service_unavailable")?;
        let id = uuid::Uuid::new_v4().to_string();
        let receipt = request(
            &self.shared,
            question_route_activity::query(&id, &session_id),
            stop,
        )
        .await?;
        question_route_activity::routes(&receipt, &id, &session_id)
            .map(|routes| json!({"routes": routes}))
            .ok_or("invalid_service_receipt")
    }
    /// The latch of one local child (zerus-sa9qn.50), read-only, for the
    /// person's next compare-and-swap.
    pub async fn question_route_child_status(
        &self,
        session_id: String,
    ) -> Result<Value, &'static str> {
        if !question_route_activity::canonical(&session_id) {
            return Err("invalid_question_route_child_request");
        }
        let stop = self
            .shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or("service_unavailable")?;
        let id = uuid::Uuid::new_v4().to_string();
        let receipt = request(
            &self.shared,
            question_route_child::query(&id, &session_id),
            stop,
        )
        .await?;
        question_route_child::status(&receipt, &id, &session_id).ok_or("invalid_service_receipt")
    }
    pub async fn read_marks(
        &self,
        marks: Vec<zerus_client_core::readmarks::ReadMark>,
    ) -> Result<Vec<zerus_client_core::readmarks::ReadMark>, String> {
        if self
            .shared
            .read_import
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .blocked()
        {
            return Err("Pending read marks have not been transferred to the service yet.".into());
        }
        if marks.len() > service::CATALOG_ROWS {
            return Err("Select at most 128 read marks at once.".into());
        }
        let mut committed = Vec::new();
        for mark in marks {
            if !self.snapshot().component_ready("reads") {
                return Err(
                    "Read marks are unavailable until the background service is ready.".into(),
                );
            }
            let key = serde_json::to_string(&mark).map_err(|_| "Invalid read intent.")?;
            if key.len() > zerus_watch_protocol::CONTROL_LINE_BYTES / 2 || mark.is_empty() {
                return Err("Invalid read intent.".into());
            }
            let (id, uncertain) = {
                let mut reads = self
                    .shared
                    .reads
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if reads.len() >= 256 && !reads.contains_key(&key) {
                    return Err(
                        "Too many unconfirmed read intents. Retry after the service reconnects."
                            .into(),
                    );
                }
                let intent = reads.entry(key.clone()).or_insert_with(|| ReadIntent {
                    id: uuid::Uuid::new_v4().to_string(),
                    in_flight: false,
                    uncertain: false,
                });
                if intent.in_flight {
                    return Err("This read intent is already being submitted.".into());
                }
                intent.in_flight = true;
                (intent.id.clone(), intent.uncertain)
            };
            let stop = self
                .shared
                .cancellation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .ok_or("Background service is unavailable.")?;
            let result = async {
                if uncertain {
                    let query = uuid::Uuid::new_v4().to_string();
                    let receipt = request(
                        &self.shared,
                        json!({"op":"delivery_result","request_id":query,"original_request_id":id}),
                        stop.clone(),
                    )
                    .await?;
                    if receipt["request_id"] != query
                        || receipt["op"] != "delivery_result"
                        || receipt["original_request_id"] != id
                        || receipt["accepted"] != true
                    {
                        return Err("invalid_service_receipt");
                    }
                    if service::read_committed(&receipt["result"], &id) {
                        return Ok(());
                    }
                    if receipt["found"] != false {
                        return Err("read_commit_unconfirmed");
                    }
                }
                // Reissuing a read uses its SAME durable UUID, including after
                // an uncertain result. Policy-setting requests never use this lane.
                let receipt = request(
                    &self.shared,
                    json!({"op":"read_intent","request_id":id,"mark":mark}),
                    stop,
                )
                .await?;
                if service::read_committed(&receipt, &id) {
                    Ok(())
                } else {
                    Err("read_commit_unconfirmed")
                }
            }
            .await;
            {
                let mut reads = self
                    .shared
                    .reads
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if result.is_ok() {
                    reads.remove(&key);
                } else if let Some(intent) = reads.get_mut(&key) {
                    intent.in_flight = false;
                    intent.uncertain = true;
                }
            }
            result.map_err(|_| "The background service has not confirmed this read mark. Retry keeps its request identity.".to_owned())?;
            committed.push(mark);
        }
        Ok(committed)
    }
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.notification_controls.close();
        self.shared.ownership.stop();
        self.shared.mode_transition.close();
        self.shared.audit_help.clear();
        if let Some(cancel) = self
            .shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            cancel.store(true, Ordering::Release);
        }
        self.shared
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        self.shared.rpc.close();
        self.shared.cache_rpc.close();
        self.shared.creation_rpc.close();
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        self.shared.activation_rpc.close();
        let _ = self.control.send(Control::Stop);
    }
    pub async fn join(&self) -> Result<(), String> {
        self.stop();
        let receiver = self
            .join
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(receiver) = receiver {
            receiver
                .await
                .map_err(|_| "The service connection did not stop.".to_owned())?;
        }
        let drain = self
            .drain
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(drain) = drain {
            join_view(drain, RPC_WAIT).await?;
        }
        let closed = self.shared.rpc.close();
        let cache_closed = self.shared.cache_rpc.close();
        let creation_closed = self.shared.creation_rpc.close();
        let ownership_closed = self.shared.ownership.close();
        let mode_closed = self.shared.mode_transition.close();
        let notification_closed = self.shared.notification_controls.close();
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        let activation_closed = self.shared.activation_rpc.close();
        let drained = tauri::async_runtime::spawn_blocking(move || {
            let until = std::time::Instant::now() + RPC_WAIT;
            let drained =
                closed.wait_until(until) == zerus_client_core::desktop::drain::Observation::Idle;
            let drained = (cache_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            let drained = (creation_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            let drained = (ownership_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            let drained = (mode_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            let drained = (notification_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            let drained = (activation_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            drained
        })
        .await
        .map_err(|error| error.to_string())?;
        if !drained {
            return Err("The service connection still has unconfirmed helper work.".into());
        }
        Ok(())
    }
}
async fn join_view(drain: std::thread::JoinHandle<()>, wait: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + wait;
    while !drain.is_finished() {
        if tokio::time::Instant::now() >= deadline {
            return Err("The service view did not stop before the quit deadline.".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drain
        .join()
        .map_err(|_| "The service view did not stop.".to_owned())
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Takes an admission lock (`Shared::snapshot` or `Shared::dirty`) without
/// occupying a runtime worker while a view callback holds it (zerus-bck0).
/// CatalogDelivery::apply keeps both across its view callback; a contended
/// lock is waited for inside `block_in_place` on the multi-thread runtime,
/// which hands this worker's tasks to another thread so timers and commands
/// keep running. Elsewhere (the view drain thread, a blocking-pool thread, a
/// current-thread runtime, where `block_in_place` would panic) it waits as
/// before. Callers hold no other std lock: nested acquisitions take `lock()`.
/// Not for tasks inside a Tokio LocalSet: a LocalSet on the multi-thread
/// runtime still reports MultiThread, cannot be detected here, and makes a
/// contended `block_in_place` panic (zerus-uq02). The app runs no LocalSet;
/// `no_production_code_runs_a_local_set` keeps it that way.
fn admission<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    match lock.try_lock() {
        Ok(guard) => return guard,
        Err(TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => {}
    }
    let pooled = tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
    if pooled {
        tokio::task::block_in_place(|| lock.lock().unwrap_or_else(PoisonError::into_inner))
    } else {
        lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Shared {
    fn read_checks_only(&self) -> bool {
        self.read_only_original.get().is_some() || self.read_only_reconnect.load(Ordering::Acquire)
    }

    fn original_read_scope(&self) -> Result<Option<&ReadOnlyOriginal>, &'static str> {
        if let Some(original) = self.read_only_original.get() {
            return Ok(Some(original));
        }
        if !self.read_only_reconnect.load(Ordering::Acquire) {
            return Ok(None);
        }
        Err("original_service_changed")
    }

    /// Only checked preflight/stream status can bind the original read scope.
    fn observe_manual_refresh(
        &self,
        configuration: &str,
        snapshot: &Snapshot,
    ) -> Result<(), &'static str> {
        if snapshot.executable_upgrade != Some(service::ExecutableUpgrade::ManualRefreshRequired) {
            return Ok(());
        }
        let epoch = snapshot
            .epoch
            .as_deref()
            .ok_or("original_service_changed")?;
        let generation = snapshot.generation.ok_or("original_service_changed")?;
        // A missing binding after the latch is set is never rebuilt from a
        // later reply, even if that reply presents a healthy replacement.
        self.original_read_scope()?;
        let original = self.read_only_original.get_or_init(|| ReadOnlyOriginal {
            configuration: configuration.into(),
            epoch: epoch.into(),
            generation,
        });
        if original.configuration != configuration || original.epoch != epoch {
            return Err("original_service_changed");
        }
        self.read_only_reconnect.store(true, Ordering::Release);
        Ok(())
    }

    fn business_permitted(&self) -> bool {
        self.business_permitted_for(&self.snapshot.copy())
    }
    fn business_permitted_for(&self, snapshot: &Snapshot) -> bool {
        if self.read_checks_only() || startup_recovery() {
            return false;
        }
        if !self.mode_transition.blocks_resume() {
            return true;
        }
        self.ownership.view().code.is_empty()
            && self.mode_transition.business_permitted(
                snapshot,
                self.ownership.view().status.as_ref(),
                self.ownership.granted(),
            )
    }
    fn business_scheduling_permitted(&self) -> bool {
        self.business_permitted()
            && ((!self.mode_transition.blocks_resume() && self.mode_transition.startup_permitted())
                || self.mode_transition.scheduling_permitted())
    }
    fn observe_peer_choices(&self, state: &service::State, value: &Value) {
        // A runtime draining its sources is not a peer-membership removal.
        // The parser already checked configuration/epoch and the <=64 roster.
        if value["background_runtime"]["state"] != "running" {
            return;
        }
        if let Some(sources) = state.sources() {
            // A restarted runtime initially knows only its ungreeted local
            // source. That warming roster cannot remove last-known choices.
            if !sources
                .iter()
                .any(|source| source.computer.is_empty() && source.generation > 0)
            {
                return;
            }
            *self
                .peer_choices
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(
                sources
                    .iter()
                    .filter(|source| !source.computer.is_empty())
                    .map(|source| source.computer.clone())
                    .collect(),
            );
        }
    }
    async fn publish_ownership(&self, phase: crate::desktop_ownership::Phase) {
        let owner = self.ownership.clone();
        let view = tauri::async_runtime::spawn_blocking(move || owner.record_view(phase))
            .await
            .unwrap_or_else(|_| self.ownership.view());
        let view = ownership_display(view, self.mode_transition.view(), self.business_permitted());
        let mut current = self.snapshot.lock();
        current.notification_ownership = view;
        self.pump.service(current.clone());
    }
    fn publish_notification_controls(&self) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let value = self
            .notification_controls
            .view()
            .and_then(|v| serde_json::to_value(v).ok());
        let mut current = self.snapshot.lock();
        current.notification_preferences = value;
        self.pump.service(current.clone());
    }
    fn publish(&self, mut snapshot: Snapshot) {
        snapshot.notification_preferences = self
            .notification_controls
            .view()
            .and_then(|v| serde_json::to_value(v).ok());
        snapshot.notification_ownership = ownership_display(
            self.ownership.view(),
            self.mode_transition.view(),
            self.business_permitted_for(&snapshot),
        );
        snapshot.pending_read_import = Some(
            self.read_import
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .status(),
        );
        snapshot.state_refusal = self.state_refusal.shown();
        let mut current = self.snapshot.lock();
        *current = snapshot.clone();
        self.pump.service(snapshot);
    }
    fn unavailable(&self, code: &str, reconnecting: bool) {
        for projection in &self.projections {
            projection(None);
        }
        let last = self.snapshot.copy();
        let manual = self.read_checks_only();
        let original = manual.then(|| self.read_only_original.get()).flatten();
        self.publish(Snapshot {
            executable_upgrade: manual.then_some(service::ExecutableUpgrade::ManualRefreshRequired),
            epoch: original.map(|scope| scope.epoch.clone()),
            generation: original.map(|scope| {
                if last.epoch.as_deref() == Some(scope.epoch.as_str()) {
                    last.generation.unwrap_or(scope.generation)
                } else {
                    scope.generation
                }
            }),
            code: code.into(),
            reconnecting,
            ..Snapshot::default()
        });
    }

    fn dirty(&self, computer: &str, source: SourceVersion) -> Result<bool, &'static str> {
        self.catalog_hint(computer, source, None, false)
    }
    fn catalog_hint(
        &self,
        computer: &str,
        mut source: SourceVersion,
        head: Option<Revision>,
        reset: bool,
    ) -> Result<bool, &'static str> {
        if !source.identity().valid() {
            return Err("invalid_catalog_scope");
        }
        let mut dirty = admission(&self.dirty);
        if !dirty.sources.contains_key(computer) && dirty.sources.len() >= SOURCES {
            return Err("source_capacity");
        }
        let scope_changed = dirty.sources.get(computer).is_none_or(|old| {
            old.incarnation != source.incarnation
                || old.generation != source.generation
                || old.online != source.online
                || (old.projection.is_some() && old.projection != source.projection)
        });
        // Repeated hints (including incomplete/reset-required hints) cannot
        // spend another retry budget for the same failed source scope.
        if !scope_changed && dirty.halted.contains(computer) {
            return Ok(false);
        }
        let replaced = reset || scope_changed;
        if replaced {
            dirty.next = dirty
                .next
                .checked_add(1)
                .ok_or("source_generation_exhausted")?;
            source.serial = dirty.next;
            dirty.heads.remove(computer);
            dirty.progress.remove(computer);
            dirty.halted.remove(computer);
        } else {
            source.serial = dirty.sources[computer].serial;
        }
        dirty.sources.insert(computer.into(), source);
        if let Some(head) = head {
            dirty
                .heads
                .entry(computer.into())
                .and_modify(|old| *old = (*old).max(head))
                .or_insert(head);
        }
        let caught = dirty
            .progress
            .get(computer)
            .is_some_and(|since| dirty.heads.get(computer).is_none_or(|head| head <= since));
        if !caught {
            dirty.pending.insert(computer.into());
            self.changed.notify_one();
        }
        Ok(replaced)
    }
    fn current(&self, computer: &str, source: &SourceVersion, epoch: &str) -> bool {
        self.snapshot.lock().epoch.as_deref() == Some(epoch)
            && self
                .dirty
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sources
                .get(computer)
                .is_some_and(|current| source.matches(current))
    }
    fn invalidate(&self) -> Vec<String> {
        let mut dirty = admission(&self.dirty);
        let computers = dirty.sources.keys().cloned().collect();
        dirty.heads.clear();
        dirty.progress.clear();
        dirty.sources.clear();
        dirty.pending.clear();
        dirty.halted.clear();
        dirty.running = None;
        computers
    }
}

async fn emit(shared: &Shared, change: Change, stop: &AtomicBool) -> Result<(), &'static str> {
    emit_data(shared, Data::Change(Box::new(change)), stop).await
}
async fn emit_data(
    shared: &Shared,
    mut change: Data,
    stop: &AtomicBool,
) -> Result<(), &'static str> {
    loop {
        if stop.load(Ordering::Acquire) || shared.stopped.load(Ordering::Acquire) {
            return Err("stopped");
        }
        let sink = shared
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or("frontend_closed")?;
        match sink.try_send(change) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Err("frontend_closed"),
            Err(std::sync::mpsc::TrySendError::Full(value)) => change = value,
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn command(
    shared: &Arc<Shared>,
    args: Vec<String>,
    stdin: Option<String>,
    stop: Arc<AtomicBool>,
) -> Result<Vec<u8>, &'static str> {
    command_limited(shared, args, stdin, stop, service::FRAME_BYTES).await
}

async fn command_limited(
    shared: &Arc<Shared>,
    args: Vec<String>,
    stdin: Option<String>,
    stop: Arc<AtomicBool>,
    limit: usize,
) -> Result<Vec<u8>, &'static str> {
    if !shared.allowed {
        return Err("replay_transport_unavailable");
    }
    if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return Err("stopped");
    }
    let charge = shared
        .rpc
        .admit(Role::Fleet)
        .map_err(|_| "service_transport_busy")?;
    let relay = shared
        .relay
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or("service_unavailable")?;
    let inv = zerus_client_core::Invocation {
        args,
        stdin: stdin.map(String::into_bytes),
        timeout: RPC_WAIT,
        request_id: None,
    };
    command_accounted(shared, relay, inv, stop, limit, Arc::new(charge), None).await
}

/// Admission also covers any retained original child/pipe after an uncertain return.
async fn command_accounted(
    shared: &Arc<Shared>,
    relay: Arc<zerus_client_core::desktop::relay::Connection>,
    inv: zerus_client_core::Invocation,
    stop: Arc<AtomicBool>,
    limit: usize,
    resources: Arc<dyn Send + Sync>,
    image: Option<Arc<audit::help::Image>>,
) -> Result<Vec<u8>, &'static str> {
    let state = shared.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !state.allowed {
            return Err("replay_transport_unavailable");
        }
        let hgs = relay.executable()?;
        if state.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return Err("stopped");
        }
        if image.as_ref().is_some_and(|image| !image.current(hgs)) {
            return Err("service_helper_changed");
        }
        let guard: Arc<dyn Send + Sync> = Arc::new((resources, image.clone(), relay.clone()));
        match zerus_client_core::run::run_accounted(hgs, &inv, &[], limit, &stop, guard) {
            Ok(zerus_client_core::Exit::Code {
                code: 0, stdout, ..
            }) => {
                if let Some(image) = image {
                    if !image.current(relay.executable()?) {
                        return Err("service_helper_changed");
                    }
                }
                Ok(stdout)
            }
            Ok(zerus_client_core::Exit::Code { stderr, .. }) => {
                // Task 176: a refused state folder is named to the person.
                state.state_refusal.observe(&stderr);
                Err("service_request_unconfirmed")
            }
            _ => Err("service_request_unconfirmed"),
        }
    })
    .await
    .map_err(|_| "service_request_unconfirmed")?
}

async fn request(
    shared: &Arc<Shared>,
    value: Value,
    stop: Arc<AtomicBool>,
) -> Result<Value, &'static str> {
    // This lane never acquires frontend approval-setting authority.
    if matches!(
        value["op"].as_str(),
        Some(
            "automatic_approvals"
                | "question_routes"
                | "question_route_child"
                | "maintenance_configure"
                | "cache_maintenance"
        )
    ) {
        return Err("frontend_route_required");
    }
    let input = serde_json::to_string(&value).map_err(|_| "invalid_service_request")?;
    if input.len() > zerus_watch_protocol::CONTROL_LINE_BYTES {
        return Err("service_request_too_large");
    }
    let bytes = command(
        shared,
        vec!["desktop".into(), "request".into(), "--json".into()],
        Some(input),
        stop,
    )
    .await?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid_service_response")
}

enum PreflightMode<'a> {
    Connection,
    // Only a borrow of the SAME opaque native confirmation can select plain
    // observation. Its original charge remains held through the preflight;
    // callers still revalidate before/after and keep native effect admission.
    FirstChoice(&'a mode_transition::FirstPrepared),
    ReviewedChoice(&'a mode_transition::Prepared),
}
impl PreflightMode<'_> {
    fn check_configuration(&self, configuration: &str) -> Result<(), &'static str> {
        let expected = match self {
            Self::Connection => return Ok(()),
            Self::FirstChoice(prepared) => prepared.configuration(),
            Self::ReviewedChoice(prepared) => prepared.configuration(),
        };
        if expected != configuration {
            return Err("activation_scope_changed");
        }
        Ok(())
    }
    async fn observe(self, shared: &Arc<Shared>, configuration: &str) {
        match self {
            Self::Connection => {
                shared
                    .mode_transition
                    .observe_preflight(shared.hgs.clone(), configuration.into())
                    .await
            }
            Self::FirstChoice(_) | Self::ReviewedChoice(_) => {
                shared
                    .mode_transition
                    .observe(shared.hgs.clone(), configuration.into())
                    .await
            }
        }
    }
}
/// `hgs desktop status --json`, with task 176's repair of a refused state
/// folder: `hgs desktop prepare-state --json` (what the service start runs
/// first) and one more status. A refusal that stays ends the connection with
/// its own code and is shown; any success clears it.
async fn status_checked(shared: &Arc<Shared>, stop: Arc<AtomicBool>, repair: bool) -> Result<Vec<u8>, &'static str> {
    let status = || vec!["desktop".to_owned(), "status".to_owned(), "--json".to_owned()];
    shared.state_refusal.take();
    let mut result = command(shared, status(), None, stop.clone()).await;
    if result.is_err() && repair && shared.state_refusal.take().is_some() {
        let _ = command(shared, vec!["desktop".into(), "prepare-state".into(), "--json".into()], None, stop.clone()).await;
        shared.state_refusal.take();
        result = command(shared, status(), None, stop).await;
    }
    match result {
        Ok(bytes) => {
            shared.state_refusal.show(None);
            Ok(bytes)
        }
        Err(code) => match shared.state_refusal.take() {
            Some(refusal) => {
                shared.state_refusal.show(Some(refusal));
                Err(state_refusal::CODE)
            }
            None => {
                // Another failure: an old refusal no longer describes it.
                shared.state_refusal.show(None);
                Err(code)
            }
        },
    }
}

async fn preflight(shared: &Arc<Shared>, stop: Arc<AtomicBool>) -> Result<String, &'static str> {
    preflight_for(shared, stop, PreflightMode::Connection).await
}
async fn preflight_for(
    shared: &Arc<Shared>,
    stop: Arc<AtomicBool>,
    mode: PreflightMode<'_>,
) -> Result<String, &'static str> {
    let _classification = match mode {
        PreflightMode::Connection => Some(shared.mode_transition.preflight_scope()),
        PreflightMode::FirstChoice(_) | PreflightMode::ReviewedChoice(_) => None,
    };
    let original = shared.original_read_scope()?;
    let help = command(shared, vec!["--help".into()], None, stop.clone()).await?;
    shared.notification_supported.store(
        std::str::from_utf8(&help).ok().is_some_and(|text| {
            [
                zerus_client_core::desktop::notification_control::HELP_MARKER,
                zerus_client_core::desktop::ui_requests::HELP_MARKER,
            ]
            .iter()
            .all(|marker| text.lines().any(|line| line.trim() == *marker))
        }),
        Ordering::Release,
    );
    shared.ownership_supported.store(
        std::str::from_utf8(&help).ok().is_some_and(|text| {
            [
                zerus_client_core::desktop::ownership::HELP_MARKER,
                zerus_client_core::desktop::ui_requests::HELP_MARKER,
            ]
            .iter()
            .all(|marker| text.lines().any(|line| line.trim() == *marker))
        }),
        Ordering::Release,
    );
    if std::str::from_utf8(&help).ok().is_some_and(|text| {
        text.lines().any(|line| {
            line.trim() == zerus_client_core::desktop::ownership::observation::HELP_MARKER
        })
    }) {
        shared.mode_transition.require_classification();
    }
    if !std::str::from_utf8(&help).ok().is_some_and(|help| {
        help.lines().any(|line| line.trim() == HELP)
            && help
                .lines()
                .any(|line| line.trim() == service::CHANGES_HELP)
            && help.lines().any(|line| line.trim() == service::RELAY_HELP)
    }) {
        return Err("incompatible_service_cli");
    }
    // Task 176: a state folder hgs refuses as not private is repaired once by
    // the service start's own preparation, never on a read-only or recovery
    // connection; a refusal that stays is shown with its folder and entry.
    let repair = matches!(mode, PreflightMode::Connection)
        && !shared.read_checks_only()
        && !startup_recovery()
        && state_refusal::can_prepare(&help);
    let bytes = status_checked(shared, stop, repair).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_service_response")?;
    // A successful status CLI can report an absent socket. Its unavailable
    // response has no capability table; this is not a protocol incompatibility.
    if value["running"] == false {
        // The interrupted registration journal is reachable even while its
        // service is absent. This is a bounded read, before any Grant/import
        // or manager path; a manual-refresh original never changes scope.
        if let Some(configuration) = value["configuration"]
            .as_str()
            .filter(|s| zerus_client_core::desktop::digest(s))
        {
            if original.is_some_and(|scope| scope.configuration != configuration) {
                return Err("original_service_changed");
            }
            mode.check_configuration(configuration)?;
            mode.observe(shared, configuration).await;
        }
        return Err("service_unavailable");
    }
    let configuration = value["configuration"]
        .as_str()
        .filter(|value| zerus_client_core::desktop::digest(value))
        .ok_or("incompatible_service")?;
    if value["catalog_reads"]["changes_abi"] != service::CHANGES_ABI {
        return Err("incompatible_catalog_changes");
    }
    let mut state = service::State::new(configuration.into())?;
    state.status(&value)?;
    if original.is_some_and(|scope| {
        state.snapshot().epoch.as_deref() != Some(scope.epoch.as_str())
            || scope.configuration != configuration
    }) {
        return Err("original_service_changed");
    }
    mode.check_configuration(configuration)?;
    *shared
        .configuration
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(configuration.into());
    shared.observe_peer_choices(&state, &value);
    if let Some(responder) = &shared.responder {
        responder.configure_once(configuration.into());
    }
    shared.observe_manual_refresh(configuration, state.snapshot())?;
    mode.observe(shared, configuration).await;
    shared.publish(state.snapshot().clone());
    Ok(configuration.into())
}

async fn run(shared: Arc<Shared>, mut control: watch::Receiver<Control>) {
    let mut backoff = reconnect::Backoff::default();
    let mut automatic = false;
    loop {
        let requested = *control.borrow_and_update();
        if matches!(requested, Control::Reconnect(_)) {
            automatic = true; // The explicit action already settled its one mutation.
        }
        if requested == Control::Stop || shared.stopped.load(Ordering::Acquire) {
            break;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(stop.clone());
        let mut greeted = None;
        let result = connection(
            shared.clone(),
            &mut control,
            stop.clone(),
            automatic || shared.read_checks_only(),
            &mut greeted,
        )
        .await;
        let code = result.err().unwrap_or("service_unavailable");
        let retry = backoff
            .next(
                code,
                greeted.is_some_and(|at: tokio::time::Instant| {
                    at.elapsed() >= Duration::from_secs(30)
                }),
            )
            .map(|delay| tokio::time::Instant::now() + delay);
        shared.notification_controls.disconnect();
        *shared.relay.lock().unwrap_or_else(PoisonError::into_inner) = None;
        stop.store(true, Ordering::Release);
        shared.audit_help.clear();
        let computers = shared.invalidate();
        let changes = shared
            .details
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .disconnect();
        *shared
            .organization
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        for change in changes {
            let _ = emit(&shared, change, &AtomicBool::new(false)).await;
        }
        shared.unavailable(code, retry.is_some());
        for computer in computers {
            let _ = emit(
                &shared,
                Change::Computer {
                    computer: ComputerId(computer),
                    status: ComputerStatus::Reconnecting { attempt: 0 },
                },
                &AtomicBool::new(false),
            )
            .await;
        }
        if !shared.stopped.load(Ordering::Acquire)
            && shared.ownership_supported.load(Ordering::Acquire)
        {
            // A revoked stream can end while the passive authenticated endpoint
            // remains available. Read its status; never auto-rearm at EOF.
            shared.ownership.request(None, None, None).await;
        }
        shared
            .publish_ownership(crate::desktop_ownership::Phase::ConnectionClosed)
            .await;
        if *control.borrow() == Control::Stop {
            break;
        }
        // Reuse this one worker only after its original children have joined.
        // Explicit Retry interrupts the delay and retains its existing intents.
        if *control.borrow() != requested {
            automatic = false;
            backoff.reset();
            continue;
        }
        if let Some(retry_at) = retry {
            tokio::select! {
                biased;
                changed = control.changed() => {
                    if changed.is_err() { break; }
                    automatic = false;
                    backoff.reset();
                }
                _ = tokio::time::sleep_until(retry_at) => automatic = true,
            }
        } else {
            if control.changed().await.is_err() {
                break;
            }
            automatic = false;
            backoff.reset();
        }
    }
}

async fn connection(
    shared: Arc<Shared>,
    control: &mut watch::Receiver<Control>,
    stop: Arc<AtomicBool>,
    automatic: bool,
    greeted: &mut Option<tokio::time::Instant>,
) -> Result<(), &'static str> {
    shared
        .publish_ownership(crate::desktop_ownership::Phase::Connecting)
        .await;
    let ordinary = shared.hgs.clone();
    let relay = Arc::new(
        tauri::async_runtime::spawn_blocking(move || {
            zerus_client_core::desktop::relay::Connection::select(&ordinary)
        })
        .await
        .map_err(|_| "paired_desktop_cli_unavailable")??,
    );
    if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return Err("stopped");
    }
    *shared.relay.lock().unwrap_or_else(PoisonError::into_inner) = Some(relay.clone());
    let configuration = preflight(&shared, stop.clone()).await?;
    if shared.snapshot.copy().executable_upgrade
        == Some(service::ExecutableUpgrade::ManualRefreshRequired)
    {
        return Err("manual_refresh_required");
    }
    // Preflight observed the journal even when the actor was unavailable.
    // Review never lends authority to the positive import or Grant paths.
    // Retry/backoff and all reviewed-history reconnects remain read-only.
    // A separate confirmed choice or its checked cold read may make business
    // eligible. Only the explicit continuation may schedule startup work.
    let business = !automatic
        && !startup_recovery()
        && !shared.read_checks_only()
        && !shared.mode_transition.blocks_resume()
        && shared.mode_transition.startup_permitted();
    if shared.ownership_supported.load(Ordering::Acquire) {
        if !business {
            // Recovery is read-only until the owner chooses Resume. Reopening a read stream is never a new Grant/Rearm or replay of
            // a retained migration, approval, send or other native intent.
            shared
                .ownership
                .request(None, Some(configuration.clone()), Some(stop.clone()))
                .await;
        } else {
            shared
                .ownership
                .startup_grant(configuration.clone(), stop.clone())
                .await;
        }
    } else {
        shared.ownership.unsupported();
    }
    mode_transition::startup(&shared, !business, &configuration).await;
    shared
        .publish_ownership(crate::desktop_ownership::Phase::StartupGrant)
        .await;
    if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return Err("stopped");
    }
    // Retain/join the synchronous native verifier off the UI/runtime actor,
    // including when a stop arrives. No child starts after that stop.
    let hgs = tauri::async_runtime::spawn_blocking(move || {
        relay.executable().map(std::path::Path::to_owned)
    })
    .await
    .map_err(|_| "paired_desktop_cli_unavailable")??;
    if shared.stopped.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return Err("stopped");
    }
    let mut command = tokio::process::Command::new(hgs);
    command
        .args(["desktop", "watch", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::child_environment::restore(command.as_std_mut());
    command
        .env_remove("HGS_SESSION")
        .env_remove("HGS_SESSION_ID")
        .env_remove("HGS_RUN_ID");
    let mut child = command.spawn().map_err(|_| "service_unavailable")?;
    // Keep stdin open: EOF is the proxy's explicit detach instruction.
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or("service_unavailable")?;
    let mut stderr = child.stderr.take().ok_or("service_unavailable")?;
    let mut state = service::State::new(configuration.clone())?;
    let mut buffer = Vec::new();
    let (mut out, mut err) = ([0u8; 8192], [0u8; 4096]);
    let (mut error_bytes, mut stderr_open) = (0usize, true);
    let catalog_shared = shared.clone();
    let catalog_stop = stop.clone();
    let catalog_configuration = configuration.clone();
    let mut catalogs = tauri::async_runtime::spawn(async move {
        catalog_loop(catalog_shared, catalog_stop, catalog_configuration).await
    });
    let detail_shared = shared.clone();
    let detail_stop = stop.clone();
    let mut detail_worker =
        tauri::async_runtime::spawn(async move { details::run(detail_shared, detail_stop).await });
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let activation_worker = {
        let (state, cancelled, configuration) =
            (shared.clone(), stop.clone(), configuration.clone());
        tauri::async_runtime::spawn(async move {
            activation::run(state, configuration, cancelled).await;
        })
    };
    let grant_cancel = Arc::new(Notify::new());
    let grant_worker = {
        let (shared, stop, cancelled, configuration) = (
            shared.clone(),
            stop.clone(),
            grant_cancel.clone(),
            configuration.clone(),
        );
        tauri::async_runtime::spawn(async move {
            // Freeze the preflight decision: a later read-only classification
            // can never start this original connection's positive recovery.
            if business && !startup_recovery() && !shared.mode_transition.blocks_resume() {
                recover_grant(shared, configuration, stop, cancelled).await;
            }
        })
    };
    let classification_ready = Arc::new(Notify::new());
    let classification_worker = {
        let (shared, stop, ready, configuration, stopping) = (
            shared.clone(),
            stop.clone(),
            classification_ready.clone(),
            configuration.clone(),
            grant_cancel.clone(),
        );
        tauri::async_runtime::spawn(async move {
            mode_transition::classification::connected(
                shared,
                configuration,
                stop,
                ready,
                stopping,
            )
            .await;
        })
    };
    let mut last_frame = tokio::time::Instant::now();
    let mut notification_worker = None;
    let result = async {
        loop {
            tokio::select! {
                biased;
                _ = control.changed() => return Err("service_reconnecting"),
                _ = tokio::time::sleep_until(last_frame + Duration::from_secs(30)) => return Err("service_stale"),
                value = &mut catalogs => return value.map_err(|_| "catalog_unavailable")?,
                value = &mut detail_worker => return value.map_err(|_| "details_unavailable")?,
                count = stdout.read(&mut out) => {
                    let count = count.map_err(|_| "service_unavailable")?;
                    if count == 0 { return Err("service_unavailable"); }
                    for part in out[..count].split_inclusive(|byte| *byte == b'\n') {
                        if buffer.len() + part.len() > service::FRAME_BYTES { return Err("service_frame_too_large"); }
                        buffer.extend_from_slice(part);
                        if !part.ends_with(b"\n") { continue; }
                        let frame: Value = serde_json::from_slice(&buffer).map_err(|_| "invalid_service_frame")?;
                        buffer.clear();
                        let update = state.frame(&frame)?;
                        if frame["t"] == "hello" { *greeted = Some(tokio::time::Instant::now()); }
                        details::frame(&shared,&frame,&stop).await?;
                        for projection in &shared.projections { projection(Some(&frame)); }
                        last_frame = tokio::time::Instant::now();
                        shared.observe_manual_refresh(&configuration, state.snapshot())?;
                        shared.publish(state.snapshot().clone());
                        if frame["t"] == "hello" { classification_ready.notify_one(); }
                        if state.snapshot().executable_upgrade == Some(service::ExecutableUpgrade::ManualRefreshRequired) {
                            return Err("manual_refresh_required");
                        }
                        #[cfg(test)]
                        shared.report_frame_finished.fetch_add(1, Ordering::Release);
                        if frame["t"] == "hello" && notification_worker.is_none()
                            && !startup_recovery()
                            && shared.notification_supported.load(Ordering::Acquire)
                            && shared.notification_controls.enabled() {
                            if let Some(epoch) = state.snapshot().epoch.clone() {
                                let relay = shared.relay.lock().unwrap_or_else(PoisonError::into_inner).clone();
                                if let Some(relay) = relay {
                                    let owner = shared.notification_controls.clone();
                                    let responder = shared.responder.clone();
                                    let cancelled = stop.clone();
                                    let supported = shared.notification_supported.load(Ordering::Acquire);
                                    let scope = zerus_client_core::desktop::notification_control::Scope {
                                        configuration: configuration.clone(), service_epoch: epoch,
                                    };
                                    notification_worker = Some(tauri::async_runtime::spawn(async move {
                                        if let Some(session) = owner.connect(scope, relay, responder, cancelled, supported).await {
                                            notification_controls::run(owner, session).await;
                                        }
                                    }));
                                }
                            }
                        }
                        match update {
                            service::Update::Status => {
                                shared.observe_peer_choices(&state, &frame["value"]);
                                if let Some(computers) = state.sources() {
                                    for computer in computers {
                                        // Generation zero has not greeted the service yet.
                                        if computer.generation == 0 { continue; }
                                        let name = &computer.computer;
                                        let known = admission(&shared.dirty).sources.contains_key(name);
                                        if !known {
                                            shared.dirty(name, SourceVersion {incarnation:computer.incarnation,generation:computer.generation,
                                                online:computer.online,..SourceVersion::default()})?;
                                        }
                                        // This roster discovers sources. Catalog hints/pages
                                        // bind their scope and coverage; transport readiness
                                        // alone cannot repeatedly re-baseline cached rows.
                                        shared.details.lock().unwrap_or_else(PoisonError::into_inner).source_state(name,computer.generation,computer.online)?;
                                        shared.details_changed.notify_one();
                                    }
                                }
                            }
                            service::Update::Catalog { computer, removed } => {
                                if removed {
                                    { let mut dirty = admission(&shared.dirty); dirty.progress.remove(&computer); dirty.heads.remove(&computer); dirty.sources.remove(&computer); dirty.pending.remove(&computer); dirty.halted.remove(&computer); }
                                    emit(&shared, Change::Computer { computer: ComputerId(computer.clone()), status: ComputerStatus::Ended }, &stop).await?;
                                    emit(&shared, Change::Replace { computer: ComputerId(computer), rows: vec![] }, &stop).await?;
                                } else {
                                    let source = SourceVersion::from(&frame["value"]);
                                    // A rebuilt view of a source that stays online (a native
                                    // resnapshot) is no connection change: a Live computer keeps
                                    // Live while it re-baselines, so its rows never blink (zerus-bkbb).
                                    let refresh = source.online && shared.pump.live(&computer);
                                    if shared.catalog_hint(&computer, source,
                                        serde_json::from_value(frame["value"]["revision"].clone()).map_err(|_| "invalid_catalog_revision")?, frame["value"]["reset_required"] == true)? && !refresh {
                                        emit(&shared, Change::Computer { computer: ComputerId(computer), status: ComputerStatus::Probing }, &stop).await?;
                                    }
                                }
                            }
                            service::Update::Other => (),
                        }
                    }
                }
                count = stderr.read(&mut err), if stderr_open => {
                    let count = count.map_err(|_| "service_unavailable")?;
                    stderr_open = count != 0;
                    error_bytes += count;
                    if error_bytes > 64 << 10 { return Err("service_error_output_limit"); }
                }
            }
        }
    }.await;
    stop.store(true, Ordering::Release);
    shared.notification_controls.disconnect();
    grant_cancel.notify_waiters();
    shared.changed.notify_one();
    shared.details_changed.notify_one();
    drop(stdin);
    let _ = grant_worker.await;
    // A cancelled classifier can still own a bounded read (and its original
    // child/pipe loans). Give it a short join grace, then detach without abort:
    // its stop check suppresses publication and its retained resources drain
    // physically in that same worker, without delaying this watch's teardown.
    let _ = tokio::time::timeout(Duration::from_millis(100), classification_worker).await;
    if let Some(worker) = notification_worker {
        let _ = worker.await;
    }
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let _ = activation_worker.await;
    // Only this owned local proxy is stopped; the desktop service and native agents remain untouched.
    if child.kill().await.is_err() && child.wait().await.is_err() {
        shared.stopped.store(true, Ordering::Release);
        if !catalogs.inner().is_finished() {
            let _ = catalogs.await;
        }
        if !detail_worker.inner().is_finished() {
            let _ = detail_worker.await;
        }
        return Err("service_transport_unverified");
    }
    if !catalogs.inner().is_finished() {
        let _ = catalogs.await;
    }
    if !detail_worker.inner().is_finished() {
        let _ = detail_worker.await;
    }
    result
}

/// Keep a slow ownership drain recoverable without delaying the watch/catalog
/// connection. One worker per original connection, joined on every exit.
async fn recover_grant(
    shared: Arc<Shared>,
    configuration: String,
    stop: Arc<AtomicBool>,
    cancelled: Arc<Notify>,
) {
    const DELAYS: [u64; 5] = [1, 2, 5, 10, 30];
    let mut step = 0;
    // An original whose verified-exit result is unknown gets one read-only
    // lookup of its exact identity. It is never resent; only a definitive
    // not-applied result (or a matched transient refusal) admits a fresh Grant.
    let mut looked_up = None::<String>;
    let next = |looked_up: &Option<String>| {
        if stop.load(Ordering::Acquire)
            || shared.stopped.load(Ordering::Acquire)
            || shared.read_checks_only()
            || shared.mode_transition.blocks_resume()
            || !shared.mode_transition.startup_permitted()
        {
            return None;
        }
        match shared.ownership.uncertain_grant() {
            Some(original) if looked_up.as_ref() != Some(&original) => Some(Some(original)),
            Some(_) => None,
            None => shared.ownership.retryable_grant().then_some(None),
        }
    };
    loop {
        let wake = cancelled.notified();
        if next(&looked_up).is_none() {
            return;
        }
        tokio::select! {
            biased;
            _ = wake => return,
            _ = tokio::time::sleep(Duration::from_secs(DELAYS[step])) => (),
        }
        let Some(lookup) = next(&looked_up) else {
            return;
        };
        let phase = match lookup {
            Some(original) => {
                looked_up = Some(original);
                shared
                    .ownership
                    .request(None, Some(configuration.clone()), Some(stop.clone()))
                    .await;
                crate::desktop_ownership::Phase::Result
            }
            None => {
                #[cfg(test)]
                recovery_pause::reached(&shared.hgs).await;
                if shared.read_checks_only()
                    || shared.mode_transition.blocks_resume()
                    || !shared.mode_transition.startup_permitted()
                {
                    return;
                }
                shared
                    .ownership
                    .recovery_grant(configuration.clone(), stop.clone())
                    .await;
                crate::desktop_ownership::Phase::RetryGrant
            }
        };
        shared.publish_ownership(phase).await;
        step = (step + 1).min(DELAYS.len() - 1);
    }
}

/// Test seam: hold one service's recovery after it chose a fresh Grant step
/// and before admission, so a concurrent Check can land in that window. It
/// awaits rather than blocks: a blocked worker can stall the runtime's timers.
#[cfg(test)]
pub(crate) mod recovery_pause {
    use std::{
        path::{Path, PathBuf},
        sync::{mpsc, Mutex},
        time::Duration,
    };
    use tokio::sync::oneshot;
    type Hook = (PathBuf, mpsc::Sender<()>, oneshot::Receiver<()>);
    // One hook per service executable, so concurrent tests never clobber.
    static HOOKS: Mutex<Vec<Hook>> = Mutex::new(Vec::new());
    // Armed only by the Linux ownership tests.
    #[cfg(target_os = "linux")]
    pub(crate) fn arm(hgs: PathBuf, entered: mpsc::Sender<()>, resume: oneshot::Receiver<()>) {
        HOOKS.lock().unwrap().push((hgs, entered, resume));
    }
    pub(super) async fn reached(hgs: &Path) {
        let hook = {
            let mut hooks = HOOKS.lock().unwrap();
            let Some(at) = hooks.iter().position(|(armed, ..)| armed == hgs) else {
                return;
            };
            hooks.swap_remove(at)
        };
        let (_, entered, resume) = hook;
        let _ = entered.send(());
        let _ = tokio::time::timeout(Duration::from_secs(30), resume).await;
    }
}

#[tauri::command]
pub fn desktop_service_mode() -> bool {
    mode()
}

#[tauri::command]
pub fn desktop_service_check_status(
    source: tauri::State<'_, crate::commands::AppState>,
    epoch: String,
) -> Result<(), String> {
    if !mode() {
        return Err("Background service mode is not enabled.".into());
    }
    source
        .desktop_service()
        .ok_or("The original background service connection is unavailable. Reopen Zerus to check status.")?
        .check_status(&epoch)
}

#[tauri::command]
pub fn desktop_service_retry(
    source: tauri::State<'_, crate::commands::AppState>,
) -> Result<(), String> {
    if !mode() {
        return Err("Background service mode is not enabled.".into());
    }
    if let Some(service) = source.desktop_service() {
        service.retry();
    } else {
        source.start_source()?;
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_service_ownership_rearm(
    source: tauri::State<'_, crate::commands::AppState>,
) -> Result<crate::desktop_ownership::View, String> {
    let service = source
        .desktop_service()
        .ok_or("ownership_service_mode_required")?;
    Ok(service.ownership_rearm().await)
}
#[tauri::command]
pub async fn desktop_service_ownership_result(
    source: tauri::State<'_, crate::commands::AppState>,
) -> Result<crate::desktop_ownership::View, String> {
    let service = source
        .desktop_service()
        .ok_or("ownership_service_mode_required")?;
    Ok(service.ownership_result().await)
}

/// A bounded observation only. Retry and reconnect never call the refresh operation.
#[tauri::command]
pub async fn desktop_service_refresh_status(
    source: tauri::State<'_, crate::commands::AppState>,
) -> Result<mode_transition::refresh::View, String> {
    source
        .desktop_service()
        .ok_or("refresh_service_unavailable")?
        .refresh_status()
        .await
}

#[tauri::command]
pub async fn desktop_service_refresh(
    app: tauri::AppHandle,
    source: tauri::State<'_, crate::commands::AppState>,
    display: mode_transition::refresh::Offer,
) -> Result<mode_transition::refresh::View, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    if crate::actions::blocked() {
        return Err("replay_transport_unavailable".into());
    }
    let service = source
        .desktop_service()
        .ok_or("refresh_service_unavailable")?;
    let fence = service.refresh_fence(&display).map_err(str::to_owned)?;
    let window = crate::main_window(&app).ok_or("The main window is unavailable.")?;
    let prepared = service.shared.mode_transition.confirm_refresh(display, fence, move |display| {
        let text = |message: zerus_client_core::text::Text| crate::native_text::render(&message);
        let title = text(zerus_client_core::text!("Refresh background service"));
        let body = text(zerus_client_core::text!("Zerus will stop the original background service after its work has drained, then start the updated CLI in the same mode. Background access and the selected mode stay unchanged. After the refresh, close and reopen Zerus to reconnect."));
        app.dialog().message(format!("{body}\n\n{}: {}\n{}: {}",
            text(zerus_client_core::text!("Original service")), display.epoch,
            text(zerus_client_core::text!("Configuration")), display.configuration))
            .title(title.clone()).parent(&window).kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(title, text(zerus_client_core::text!("Cancel")))).blocking_show()
    }).await.map_err(str::to_owned)?;
    if let Some(prepared) = prepared {
        service
            .shared
            .mode_transition
            .apply_refresh(prepared)
            .await
            .map_err(str::to_owned)
    } else {
        Ok(service.shared.mode_transition.refresh_view())
    }
}

/// Two distinct native confirmations. The renderer selects only the displayed
/// original and the action; it cannot supply consent, a native caller or a PID.
#[tauri::command]
pub async fn desktop_service_mode_action(
    app: tauri::AppHandle,
    source: tauri::State<'_, crate::commands::AppState>,
    display: mode_transition::ReviewView,
    action: mode_transition::ReviewAction,
) -> Result<Value, String> {
    use mode_transition::ReviewAction;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    if crate::actions::blocked() {
        return Err("replay_transport_unavailable".into());
    }
    let service = source
        .desktop_service()
        .ok_or("ownership_service_mode_required")?;
    if action != ReviewAction::Review && service.shared.read_checks_only() {
        return Err("manual_refresh_required".into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable.")?;
    let prepared = service.shared.mode_transition.confirm(display, action, move |display, action| {
        let text = |message: zerus_client_core::text::Text| crate::native_text::render(&message);
        let title = match action {
            ReviewAction::Review => text(zerus_client_core::text!("Review interrupted mode change")),
            ReviewAction::Background => text(zerus_client_core::text!("Use background mode")),
            ReviewAction::Passive => text(zerus_client_core::text!("Use passive mode")),
        };
        let message = match action {
            ReviewAction::Review if display.passive_available => text(zerus_client_core::text!("Zerus still cannot confirm whether the earlier mode change finished. This review does not start or stop the service or resume background work. You may need to restart Windows and review again.")),
            ReviewAction::Review => text(zerus_client_core::text!("Zerus still cannot confirm whether the earlier mode change finished. This review does not start or stop the service or resume background work. You may need to restart your computer and review again.")),
            ReviewAction::Background => text(zerus_client_core::text!("This is a new choice, made after the review. Zerus turns on background work once the running service confirms it, then checks the registration before using background mode.")),
            ReviewAction::Passive => text(zerus_client_core::text!("This is a new choice, made after the review. Zerus stops background work and keeps the service available for the open app. The earlier change still has no confirmed result.")),
        };
        app.dialog().message(format!("{message}\n\n{}: {}\n{}: {}",
            text(zerus_client_core::text!("Original request")), display.request_id,
            text(zerus_client_core::text!("Configuration")), display.configuration))
            .title(title.clone()).parent(&window).kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(title, text(zerus_client_core::text!("Cancel"))))
            .blocking_show()
    }).await.map_err(str::to_owned)?;
    service.apply_mode_action(prepared).await
}

/// One native confirmation for the FIRST explicit background choice on a
/// plain passive Windows registration. The renderer selects only the
/// displayed offer; it cannot supply consent, a native caller or a PID.
#[tauri::command]
pub async fn desktop_service_first_choice(
    app: tauri::AppHandle,
    source: tauri::State<'_, crate::commands::AppState>,
    display: mode_transition::FirstView,
) -> Result<Value, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    if crate::actions::blocked() {
        return Err("replay_transport_unavailable".into());
    }
    let service = source
        .desktop_service()
        .ok_or("ownership_service_mode_required")?;
    if service.shared.read_checks_only() {
        return Err("manual_refresh_required".into());
    }
    if !service.shared.ownership.granted() || !service.shared.ownership.view().code.is_empty() {
        return Err("ownership_rearm_required".into());
    }
    let window = crate::main_window(&app).ok_or("The main window is unavailable.")?;
    let prepared = service
        .shared
        .mode_transition
        .confirm_first(display, move |_display| {
            let text = |message: zerus_client_core::text::Text| crate::native_text::render(&message);
            let title = text(zerus_client_core::text!("Use background mode"));
            app.dialog()
                .message(text(zerus_client_core::text!("Zerus turns on background work on this computer and runs your sessions through the background service. Nothing else changes until the service confirms it.")))
                .title(title.clone())
                .parent(&window)
                .kind(MessageDialogKind::Info)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    title,
                    text(zerus_client_core::text!("Cancel")),
                ))
                .blocking_show()
        })
        .await
        .map_err(str::to_owned)?;
    service.apply_first_choice(prepared).await
}

#[tauri::command]
pub async fn desktop_service_read_marks(
    source: tauri::State<'_, crate::commands::AppState>,
    marks: Vec<zerus_client_core::readmarks::ReadMark>,
) -> Result<Vec<zerus_client_core::readmarks::ReadMark>, String> {
    if !mode() {
        return Err("Background service mode is not enabled.".into());
    }
    let service = source
        .desktop_service()
        .ok_or("Background service is unavailable.")?;
    service.read_marks(marks).await
}

/// The existing confirmation calls the dedicated native frontend route.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_service_automatic_approvals(
    source: tauri::State<'_, crate::commands::AppState>,
    request_id: String,
    enabled: bool,
    categories: Option<Vec<String>>,
) -> Result<Value, String> {
    frontend::approval_command(
        source.desktop_service().filter(|_| mode()),
        request_id,
        enabled,
        categories,
    )
    .await
}

/// The person's latch on one child's question route (zerus-sa9qn.50),
/// through the native frontend lane only.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn desktop_service_question_route_child(
    source: tauri::State<'_, crate::commands::AppState>,
    request_id: String,
    session_id: String,
    run_id: String,
    conversation_id: String,
    person_off: bool,
    expected_revision: u64,
) -> Result<Value, String> {
    frontend::question_route_child_command(
        source.desktop_service().filter(|_| mode()),
        zerus_client_core::desktop::ui_requests::Request::QuestionRouteChild {
            request_id,
            session_id,
            run_id,
            conversation_id,
            person_off,
            expected_revision,
        },
    )
    .await
}

/// The latch of one local child (zerus-sa9qn.50), read-only; unavailable
/// outside background service mode.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_service_question_route_child_status(
    source: tauri::State<'_, crate::commands::AppState>,
    session_id: String,
) -> Result<Value, String> {
    source
        .desktop_service()
        .filter(|_| mode())
        .ok_or("question_route_child_unavailable")?
        .question_route_child_status(session_id)
        .await
        .map_err(str::to_owned)
}

/// The read-only Activity lines of a local session's submitted question
/// routes (zerus-sa9qn.33); none outside background service mode.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_service_question_route_activity(
    source: tauri::State<'_, crate::commands::AppState>,
    session_id: String,
) -> Result<Value, String> {
    match source.desktop_service().filter(|_| mode()) {
        Some(service) => service
            .question_route_activity(session_id)
            .await
            .map_err(str::to_owned),
        None => Ok(json!({"routes": []})),
    }
}

/// The person's global switch of the owner-approved question route
/// (zerus-sa9qn.32), through the native frontend lane only.
#[tauri::command(rename_all = "snake_case")]
pub async fn desktop_service_question_routes(
    source: tauri::State<'_, crate::commands::AppState>,
    request_id: String,
    enabled: bool,
) -> Result<Value, String> {
    frontend::question_routes_command(
        source.desktop_service().filter(|_| mode()),
        request_id,
        enabled,
    )
    .await
}

#[cfg(test)]
mod passive_projection_tests;
#[cfg(all(test, unix))]
mod tests;
