//! One owned desktop subscriber and closed native intents. No daemon launch or legacy fallback.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc, Mutex, OnceLock, PoisonError, Weak,
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod activation;

#[derive(Default)]
pub(crate) struct Observers {
    pub projections: Vec<Projection>,
    pub notification_controls: Option<crate::desktop_notification_controls::Setup>,
    pub retained_sources: Option<retained_import::Inputs>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub activation: Option<activation::Sink>,
}

pub(crate) mod audit;
mod cache;
mod catalog;
mod catalog_relay;
pub(crate) mod creation_clone;
use catalog::catalog_loop;
mod details;
pub mod frontend;
pub(crate) mod notification_controls;
mod read_import;
pub(crate) mod retained_import;

/// Registered at setup before start_source. Some contains a verified service
/// watch frame; None invalidates the prior projection. Callbacks must not block.
pub type Projection = Arc<dyn Fn(Option<&Value>) + Send + Sync>;

/// Captured before startup exclusion and before any producer is constructed.
pub fn mode() -> bool {
    static MODE: OnceLock<bool> = OnceLock::new();
    *MODE.get_or_init(|| service::enabled(std::env::var(service::MODE_ENV).ok().as_deref()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Retry(u64),
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
        let snapshot = shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut dirty = shared.dirty.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stopped.load(Ordering::Acquire)
            || shared.stopped.load(Ordering::Acquire)
            || snapshot.epoch.as_deref() != Some(&self.epoch)
            || dirty.sources.get(&self.computer) != Some(&self.source)
        {
            return;
        }
        match self.value {
            CatalogValue::Row(change) => {
                observe(&change);
                pump.push(*change);
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
                pump.push(Change::Computer {
                    computer,
                    // A contiguous delta range remains live even when a newer
                    // head needs another catch-up. Pending display changes do
                    // not mean this computer lost its connection.
                    status: if through.is_some() {
                        ComputerStatus::Live
                    } else {
                        ComputerStatus::Reconnecting { attempt: 0 }
                    },
                });
                let _ = done.send(());
            }
        }
    }
}

struct Shared {
    creation_rpc: Gate,
    notification_controls: Arc<crate::desktop_notification_controls::Controller>,
    notification_supported: AtomicBool,
    ownership: Arc<crate::desktop_ownership::Controller>,
    ownership_supported: AtomicBool,
    hgs: PathBuf,
    configuration: Mutex<Option<String>>,
    audit_help: audit::help::Cache,
    relay: Mutex<Option<Arc<zerus_client_core::desktop::relay::Connection>>>,
    pump: crate::pump::Pump,
    sink: Mutex<Option<SyncSender<Data>>>,
    snapshot: Mutex<Snapshot>,
    // Bounded display-only names from a checked runtime roster. Missing or
    // disconnected projections retain last-known choices, never a known zero.
    peer_choices: Mutex<Option<BTreeSet<String>>>,
    dirty: Mutex<Dirty>,
    changed: Notify,
    rpc: Gate,
    cache_rpc: Gate,
    cache: Mutex<Option<cache::Pending>>,
    // Long activation reads never consume fleet/details admission capacity.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    activation_rpc: Gate,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    activation_retry: Mutex<Option<Duration>>,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    activation_backoffs: std::sync::atomic::AtomicUsize,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    activation_permanent: AtomicBool,
    allowed: bool,
    stopped: AtomicBool,
    cancellation: Mutex<Option<Arc<AtomicBool>>>,
    responder: Option<Arc<zerus_client_core::desktop::Responder>>,
    reads: Mutex<BTreeMap<String, ReadIntent>>,
    details: Mutex<details::Hub>,
    details_changed: Notify,
    organization: Mutex<Option<zerus_client_core::OrganizationSnapshot>>,
    frontend: frontend::Frontend,
    read_import: Mutex<read_import::State>,
    retained_import: Option<Arc<retained_import::Report>>,
    projections: Vec<Projection>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
                projections,
                notification_controls: None,
                retained_sources: None,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            creation_rpc: Gate::new(1).unwrap(),
            notification_controls: crate::desktop_notification_controls::Controller::new(
                observers.notification_controls,
            ),
            notification_supported: AtomicBool::new(false),
            ownership,
            ownership_supported: AtomicBool::new(false),
            hgs,
            relay: Mutex::new(None),
            pump,
            sink: Mutex::new(Some(sink)),
            snapshot: Mutex::new(Snapshot::default()),
            peer_choices: Mutex::new(None),
            dirty: Mutex::new(Dirty::default()),
            changed: Notify::new(),
            rpc: Gate::new(4).unwrap(),
            cache_rpc: Gate::new(1).unwrap(),
            cache: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            activation_rpc: Gate::new(1).unwrap(),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            activation_retry: Mutex::new(None),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            activation_backoffs: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            activation_permanent: AtomicBool::new(false),
            allowed,
            stopped: AtomicBool::new(false),
            cancellation: Mutex::new(None),
            configuration: Mutex::new(None),
            audit_help: audit::help::Cache::default(),
            responder,
            reads: Mutex::new(BTreeMap::new()),
            details: Mutex::new(details::Hub::default()),
            details_changed: Notify::new(),
            organization: Mutex::new(None),
            frontend: frontend::Frontend::default(),
            read_import: Mutex::new(read_import::State::default()),
            retained_import: observers.retained_sources.map(retained_import::Report::new),
            projections: observers.projections,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            activation: observers.activation,
        });
        let weak = Arc::downgrade(&shared);
        shared.notification_controls.hook(Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.publish_notification_controls();
            }
        }));
        if let Some(report) = &shared.retained_import {
            let weak = Arc::downgrade(&shared);
            report.hook_creations(Arc::new(move || {
                if let Some(shared) = weak.upgrade() {
                    shared.publish_retained_import();
                }
            }));
        }
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
    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub(crate) async fn ownership_rearm(self: &Arc<Self>) -> crate::desktop_ownership::View {
        if self.shared.retained_import.is_some() {
            let relay = self
                .shared
                .relay
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let configuration = self
                .shared
                .configuration
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let cancel = self
                .shared
                .cancellation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            match (relay, configuration, cancel) {
                (Some(relay), Some(configuration), Some(cancel)) => {
                    retained_import::before_grant(&self.shared, relay, configuration, cancel, true)
                        .await
                }
                _ => retained_import::record_error(
                    &self.shared,
                    "retained_import_connection_unavailable",
                ),
            }
        }
        let view = self
            .shared
            .ownership
            .request(
                Some(zerus_client_core::desktop::ownership::Action::Rearm),
                None,
                None,
            )
            .await;
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Rearm)
            .await;
        if self.shared.ownership.granted() {
            // Rearm already made its one explicit report attempt above.
            self.retry_connection();
        }
        view
    }
    pub(crate) async fn ownership_result(&self) -> crate::desktop_ownership::View {
        let view = self.shared.ownership.request(None, None, None).await;
        self.shared
            .publish_ownership(crate::desktop_ownership::Phase::Result)
            .await;
        view
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
        if let Some(report) = &self.shared.retained_import {
            report.request_retry();
        }
        self.retry_connection();
    }
    fn retry_connection(&self) {
        if self.shared.stopped.load(Ordering::Acquire) {
            return;
        }
        read_import::retry(self.shared.clone());
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
            if let Control::Retry(n) = control {
                *control = n.checked_add(1).map_or(Control::Stop, Control::Retry);
            }
        });
    }
    /// Nudge a non-live catalog, preserving healthy and already-pending reads.
    /// The local Refresh entry point also reaches halted peers: service mode
    /// has no legacy GUI peer listing for the tray/UI Refresh loops to visit.
    pub fn reconnect(&self, computer: &str) -> bool {
        if self.shared.stopped.load(Ordering::Acquire) {
            return false;
        }
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !snapshot.connected {
            drop(snapshot);
            self.retry();
            return true;
        }
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
        if let Some(report) = &self.shared.retained_import {
            report.close();
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        let notification_closed = self.shared.notification_controls.close();
        let retained_closed = self
            .shared
            .retained_import
            .as_ref()
            .map(|report| report.close())
            .unwrap_or_default();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            let drained = (notification_closed.wait_until(until)
                == zerus_client_core::desktop::drain::Observation::Idle)
                && drained;
            let drained = retained_closed.iter().all(|closed| {
                closed.wait_until(until) == zerus_client_core::desktop::drain::Observation::Idle
            }) && drained;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
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

impl Shared {
    fn publish_retained_import(&self) {
        let report = self.retained_import.as_ref().map(|report| report.status());
        let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        snapshot.retained_source_import = report;
        self.pump.service(snapshot.clone());
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
        let view = serde_json::to_value(view).ok();
        let mut current = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
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
        let mut current = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        current.notification_preferences = value;
        self.pump.service(current.clone());
    }
    fn publish(&self, mut snapshot: Snapshot) {
        snapshot.notification_preferences = self
            .notification_controls
            .view()
            .and_then(|v| serde_json::to_value(v).ok());
        snapshot.notification_ownership = serde_json::to_value(self.ownership.view()).ok();
        snapshot.pending_read_import = Some(
            self.read_import
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .status(),
        );
        snapshot.retained_source_import =
            self.retained_import.as_ref().map(|report| report.status());
        let mut current = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        *current = snapshot.clone();
        self.pump.service(snapshot);
    }
    fn unavailable(&self, code: &str) {
        for projection in &self.projections {
            projection(None);
        }
        self.publish(Snapshot {
            code: code.into(),
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
        let mut dirty = self.dirty.lock().unwrap_or_else(PoisonError::into_inner);
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
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .epoch
            .as_deref()
            == Some(epoch)
            && self
                .dirty
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sources
                .get(computer)
                .is_some_and(|current| source.matches(current))
    }
    fn invalidate(&self) -> Vec<String> {
        let mut dirty = self.dirty.lock().unwrap_or_else(PoisonError::into_inner);
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
        Some("automatic_approvals" | "maintenance_configure" | "cache_maintenance")
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

async fn preflight(shared: &Arc<Shared>, stop: Arc<AtomicBool>) -> Result<String, &'static str> {
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
    if !std::str::from_utf8(&help).ok().is_some_and(|help| {
        help.lines().any(|line| line.trim() == HELP)
            && help
                .lines()
                .any(|line| line.trim() == service::CHANGES_HELP)
            && help.lines().any(|line| line.trim() == service::RELAY_HELP)
    }) {
        return Err("incompatible_service_cli");
    }
    let bytes = command(
        shared,
        vec!["desktop".into(), "status".into(), "--json".into()],
        None,
        stop,
    )
    .await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_service_response")?;
    let configuration = value["configuration"]
        .as_str()
        .filter(|value| zerus_client_core::desktop::digest(value))
        .ok_or("incompatible_service")?;
    if value["catalog_reads"]["changes_abi"] != service::CHANGES_ABI {
        return Err("incompatible_catalog_changes");
    }
    let mut state = service::State::new(configuration.into())?;
    state.status(&value)?;
    *shared
        .configuration
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(configuration.into());
    shared.observe_peer_choices(&state, &value);
    if let Some(responder) = &shared.responder {
        responder.configure_once(configuration.into());
    }
    shared.publish(state.snapshot().clone());
    Ok(configuration.into())
}

async fn run(shared: Arc<Shared>, mut control: watch::Receiver<Control>) {
    loop {
        let requested = *control.borrow_and_update();
        if requested == Control::Stop || shared.stopped.load(Ordering::Acquire) {
            break;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *shared
            .cancellation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(stop.clone());
        let result = connection(shared.clone(), &mut control, stop.clone()).await;
        shared.notification_controls.disconnect();
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
        shared.unavailable(result.err().unwrap_or("service_unavailable"));
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
        if *control.borrow() == Control::Stop {
            break;
        }
        // Retry is explicit and reuses this one worker after its children join.
        if *control.borrow() != requested {
            continue;
        }
        if control.changed().await.is_err() {
            break;
        }
    }
}

async fn connection(
    shared: Arc<Shared>,
    control: &mut watch::Receiver<Control>,
    stop: Arc<AtomicBool>,
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
    let explicit = shared
        .retained_import
        .as_ref()
        .is_some_and(|report| report.take_retry());
    retained_import::before_grant(
        &shared,
        relay.clone(),
        configuration.clone(),
        stop.clone(),
        explicit,
    )
    .await;
    if shared.ownership_supported.load(Ordering::Acquire) {
        shared
            .ownership
            .startup_grant(configuration.clone(), stop.clone())
            .await;
    } else {
        shared.ownership.unsupported();
    }
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
    command.env_remove("HGS_SESSION").env_remove("HGS_RUN_ID");
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            recover_grant(shared, configuration, stop, cancelled).await;
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
                        details::frame(&shared,&frame,&stop).await?;
                        for projection in &shared.projections { projection(Some(&frame)); }
                        last_frame = tokio::time::Instant::now();
                        shared.publish(state.snapshot().clone());
                        if frame["t"] == "hello" && notification_worker.is_none()
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
                                        let known = shared.dirty.lock().unwrap_or_else(PoisonError::into_inner).sources.contains_key(name);
                                        if !known {
                                            shared.dirty(name, SourceVersion {incarnation:computer.incarnation,generation:computer.generation,
                                                online:computer.online,..SourceVersion::default()})?;
                                        }
                                        // This roster discovers sources. Catalog hints/pages
                                        // bind their scope and coverage; transport readiness
                                        // alone cannot repeatedly re-baseline cached rows.
                                        shared.details.lock().unwrap_or_else(PoisonError::into_inner).source(name,computer.generation)?;
                                        shared.details_changed.notify_one();
                                    }
                                }
                            }
                            service::Update::Catalog { computer, removed } => {
                                if removed {
                                    { let mut dirty = shared.dirty.lock().unwrap_or_else(PoisonError::into_inner); dirty.progress.remove(&computer); dirty.heads.remove(&computer); dirty.sources.remove(&computer); dirty.pending.remove(&computer); dirty.halted.remove(&computer); }
                                    emit(&shared, Change::Computer { computer: ComputerId(computer.clone()), status: ComputerStatus::Ended }, &stop).await?;
                                    emit(&shared, Change::Replace { computer: ComputerId(computer), rows: vec![] }, &stop).await?;
                                } else if shared.catalog_hint(&computer, SourceVersion::from(&frame["value"]),
                                    serde_json::from_value(frame["value"]["revision"].clone()).map_err(|_| "invalid_catalog_revision")?, frame["value"]["reset_required"] == true)? {
                                    emit(&shared, Change::Computer { computer: ComputerId(computer), status: ComputerStatus::Probing }, &stop).await?;
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
    if let Some(worker) = notification_worker {
        let _ = worker.await;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    loop {
        let wake = cancelled.notified();
        if stop.load(Ordering::Acquire)
            || shared.stopped.load(Ordering::Acquire)
            || !shared.ownership.retryable_grant()
        {
            return;
        }
        tokio::select! {
            biased;
            _ = wake => return,
            _ = tokio::time::sleep(Duration::from_secs(DELAYS[step])) => (),
        }
        if stop.load(Ordering::Acquire)
            || shared.stopped.load(Ordering::Acquire)
            || !shared.ownership.retryable_grant()
        {
            return;
        }
        shared
            .ownership
            .request(
                Some(zerus_client_core::desktop::ownership::Action::Grant),
                Some(configuration.clone()),
                Some(stop.clone()),
            )
            .await;
        shared
            .publish_ownership(crate::desktop_ownership::Phase::RetryGrant)
            .await;
        step = (step + 1).min(DELAYS.len() - 1);
    }
}

#[tauri::command]
pub fn desktop_service_mode() -> bool {
    mode()
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
) -> Result<Value, String> {
    frontend::approval_command(
        source.desktop_service().filter(|_| mode()),
        request_id,
        enabled,
    )
    .await
}

#[cfg(all(test, unix))]
mod tests;
