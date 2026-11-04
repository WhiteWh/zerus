//! Source-only output of the existing watch supervisor, without its client
//! projections. One channel belongs to one service configuration. The caller
//! must own that configuration and verify handoff before starting any source.
//! Creating a channel starts nothing; no registration or writer authority here.
use crate::{legacy::spool, stream, ComputerId, ComputerStatus, Timing};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};
use zerus_watch_protocol::{CatalogItem, Control, Frame};

pub const INGRESS_BYTES: usize = 16 << 20;
pub const INGRESS_ITEMS: usize = 1024;
pub const CONNECTIONS: usize = 4;
pub const SOURCES: usize = 64;
pub const CONTROL_BATCH_BYTES: usize = 8192;
pub const CONTROL_BATCHES: usize = 128;
pub const CONTROL_BYTES: usize = 1 << 20;
// Leave room for one lifecycle status per admitted source even when every
// data permit is held in reader-to-supervisor queues rather than the output.
const DATA_BYTES: usize = INGRESS_BYTES - SOURCES * 512;
const DATA_ITEMS: usize = INGRESS_ITEMS - SOURCES;

/// A registration generation survives stream reconnects, but is never reused
/// when a stopped alias is started again on this channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub computer: ComputerId,
    pub incarnation: u64,
}
/// Provisional older-peer replacement. Only Complete commits the preceding
/// Begin and numbered pages; failure/status/replacement abandons staging.
/// This is local transport data, never a synthesized watch hello or authority.
#[derive(Debug)]
pub enum Legacy {
    Begin {
        metadata: serde_json::Value,
        rows: usize,
        pages: usize,
        input_bytes: usize,
    },
    Page {
        index: usize,
        items: Vec<CatalogItem>,
    },
    Complete,
}
#[derive(Debug)]
pub enum Event {
    Legacy {
        source: Identity,
        generation: u64,
        update: Box<Legacy>,
        observed_at: Instant,
    },
    Frame {
        source: Identity,
        generation: u64,
        frame: Box<Frame>,
        received_at: Instant,
    },
    Status {
        source: Identity,
        generation: u64,
        status: ComputerStatus,
        observed_at: Instant,
    },
}
pub struct Item {
    event: Event,
    _permit: Permit,
}
/// Opaque retained ingress admission for a host that moves the decoded payload
/// into its own event type. Release only after applying or discarding the event.
pub struct IngressPermit {
    _permit: Permit,
}
impl Item {
    /// Keep this item until its frame has been applied or explicitly discarded.
    /// Its permit counts both queued and consumer-owned work against the cap.
    pub fn event(&self) -> &Event {
        &self.event
    }
    pub fn into_parts(self) -> (Event, IngressPermit) {
        (
            self.event,
            IngressPermit {
                _permit: self._permit,
            },
        )
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: usize,
    pub items: usize,
    pub connecting: usize,
    pub sources: usize,
    /// Admitted supervisor lifetimes, including pending I/O after handle stop.
    pub workers: usize,
    pub readers: usize,
    pub legacy_spools: usize,
    pub legacy_reserved_bytes: usize,
    pub high_bytes: usize,
    pub high_items: usize,
    pub controls: usize,
    pub control_bytes: usize,
}
pub(crate) struct ControlBatch {
    pub generation: u64,
    pub bytes: Vec<u8>,
    shared: Arc<Shared>,
}
impl Drop for ControlBatch {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.usage.controls -= 1;
        state.usage.control_bytes -= self.bytes.len();
        self.shared.wake.notify_all();
    }
}
#[derive(Default)]
struct State {
    usage: Usage,
    closed: bool,
    next: u64,
    sources: BTreeMap<ComputerId, Registered>,
    readers: BTreeSet<ComputerId>,
}
struct Registered {
    incarnation: u64,
    cancel: Option<stream::SourceCancel>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn control_batch(
        self: &Arc<Self>,
        identity: &Identity,
        generation: u64,
        controls: &[Control],
    ) -> Option<ControlBatch> {
        use std::io::Write;
        if generation == 0 || controls.is_empty() || controls.len() > 3 {
            return None;
        }
        let mut encoded = crate::fsx::checkpoint::Bounded {
            bytes: Vec::new(),
            limit: CONTROL_BATCH_BYTES,
        };
        for control in controls {
            serde_json::to_writer(&mut encoded, control).ok()?;
            encoded.write_all(b"\n").ok()?;
        }
        let mut state = self.lock();
        if state.closed
            || state
                .sources
                .get(&identity.computer)
                .is_none_or(|r| r.incarnation != identity.incarnation)
            || state.usage.controls >= CONTROL_BATCHES
            || state.usage.control_bytes > CONTROL_BYTES - encoded.bytes.len()
        {
            return None;
        }
        state.usage.controls += 1;
        state.usage.control_bytes += encoded.bytes.len();
        Some(ControlBatch {
            generation,
            bytes: encoded.bytes,
            shared: self.clone(),
        })
    }
    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.wake
            .wait_timeout(state, Duration::from_millis(50))
            .unwrap_or_else(|e| e.into_inner())
            .0
    }
}
pub(crate) struct Permit {
    shared: Arc<Shared>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.usage.bytes -= self.bytes;
        state.usage.items -= 1;
        self.shared.wake.notify_all();
    }
}
pub(crate) struct Connection {
    shared: Arc<Shared>,
}
pub(crate) struct Reader {
    shared: Arc<Shared>,
    computer: ComputerId,
}
impl Drop for Reader {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.readers.remove(&self.computer);
        state.usage.readers -= 1;
        self.shared.wake.notify_all();
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.lock().usage.connecting -= 1;
        self.shared.wake.notify_all();
    }
}
struct Registration {
    shared: Arc<Shared>,
    identity: Identity,
}
struct Worker(Arc<Shared>);
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.lock().usage.workers -= 1;
        self.0.wake.notify_all();
    }
}

/// One transient request per greeted generation, independent of input-queue
/// pressure. Closing the connection cancels it; it never follows a reconnect.
#[derive(Default)]
pub(crate) struct Resnapshot(Mutex<(u64, bool)>);
impl Resnapshot {
    fn lock(&self) -> MutexGuard<'_, (u64, bool)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn open(&self, generation: u64) {
        *self.lock() = (generation, false);
    }
    pub fn close(&self) {
        *self.lock() = (0, false);
    }
    pub fn accepting(&self, generation: u64) -> bool {
        let state = self.lock();
        generation > 0 && state.0 == generation && !state.1
    }
    fn request(&self, generation: u64) -> bool {
        let mut state = self.lock();
        if generation == 0 || state.0 != generation || state.1 {
            return false;
        }
        state.1 = true;
        true
    }
    pub fn take(&self, generation: u64) -> bool {
        let mut state = self.lock();
        if state.0 != generation || !state.1 {
            return false;
        }
        *state = (0, false);
        true
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if state
            .sources
            .get(&self.identity.computer)
            .is_some_and(|entry| entry.incarnation == self.identity.incarnation)
        {
            state.sources.remove(&self.identity.computer);
            state.usage.sources -= 1;
        }
        self.shared.wake.notify_all();
    }
}
#[derive(Clone)]
pub struct Sender {
    spawn: fn(&mut Command) -> io::Result<Child>,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Item>,
    legacy: Option<LegacyConfig>,
}
/// Runs on the existing supervisor, after reader/spool admission. Supply a
/// new empty private ephemeral file through the host's platform checks. This
/// layer opens no paths. Blocked I/O retains its admissions after handle stop.
pub type SpoolFactory = Arc<dyn Fn() -> io::Result<Box<dyn spool::Storage>> + Send + Sync>;
#[derive(Clone)]
pub(crate) struct LegacyConfig {
    pub factory: SpoolFactory,
    pub pool: spool::Pool,
}
pub struct Receiver {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<Item>,
}
/// hgs passes its platform::spawn_in_turn here so all of its threaded child
/// creation shares the macOS descriptor-inheritance guard. No environment or
/// arguments are changed by this hook.
pub fn channel(spawn: fn(&mut Command) -> io::Result<Child>) -> (Sender, Receiver) {
    channel_inner(spawn, None)
}
/// Opt into bounded older-peer listings. One shared two-slot spool pool covers
/// every alias on this channel. Creating the channel starts no work.
pub fn channel_with_legacy(
    spawn: fn(&mut Command) -> io::Result<Child>,
    factory: SpoolFactory,
) -> (Sender, Receiver) {
    channel_inner(
        spawn,
        Some(LegacyConfig {
            factory,
            pool: spool::Pool::default(),
        }),
    )
}
fn channel_inner(
    spawn: fn(&mut Command) -> io::Result<Child>,
    legacy: Option<LegacyConfig>,
) -> (Sender, Receiver) {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = mpsc::channel();
    (
        Sender {
            spawn,
            shared: shared.clone(),
            tx,
            legacy,
        },
        Receiver { shared, rx },
    )
}
impl Receiver {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Item, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.closed = true;
        for entry in state.sources.values() {
            if let Some(cancel) = &entry.cancel {
                cancel.cancel();
            }
        }
        self.shared.wake.notify_all();
    }
}
impl Sender {
    pub fn usage(&self) -> Usage {
        let mut usage = self.shared.lock().usage;
        if let Some(legacy) = &self.legacy {
            usage.legacy_spools = legacy.pool.active();
            usage.legacy_reserved_bytes = usage.legacy_spools * spool::SPOOL_BYTES;
        }
        usage
    }
    pub fn spawn(&self, hgs: &Path, computer: &ComputerId, timing: Timing) -> io::Result<Handle> {
        if !computer.is_local() && !crate::fleet::valid_alias(&computer.0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid source alias",
            ));
        }
        let identity = {
            let mut state = self.shared.lock();
            if state.closed
                || state.sources.len() >= SOURCES
                || state.usage.workers >= SOURCES
                || state.sources.contains_key(computer)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "source already owned, closed or at capacity",
                ));
            }
            state.next = state
                .next
                .checked_add(1)
                .ok_or_else(|| io::Error::other("source identity exhausted"))?;
            let identity = Identity {
                computer: computer.clone(),
                incarnation: state.next,
            };
            state.sources.insert(
                computer.clone(),
                Registered {
                    incarnation: identity.incarnation,
                    cancel: None,
                },
            );
            state.usage.sources += 1;
            state.usage.workers += 1;
            identity
        };
        let slot = Registration {
            shared: self.shared.clone(),
            identity: identity.clone(),
        };
        let endpoint = Endpoint {
            sender: self.clone(),
            identity,
            resnapshot: Arc::default(),
            cancel: None,
            _worker: Some(Arc::new(Worker(self.shared.clone()))),
        };
        let resnapshot = endpoint.resnapshot.clone();
        let stream = stream::spawn_source(hgs, computer, timing, endpoint)?;
        Ok(Handle {
            stream,
            resnapshot,
            _slot: slot,
        })
    }
    fn reserve(&self, bytes: usize, stop: &AtomicBool) -> Option<Permit> {
        self.reserve_with(bytes, stop, DATA_BYTES, DATA_ITEMS, None)
    }
    fn reserve_with(
        &self,
        bytes: usize,
        stop: &AtomicBool,
        cap: usize,
        items: usize,
        until: Option<Instant>,
    ) -> Option<Permit> {
        if bytes > cap {
            return None;
        }
        let mut state = self.shared.lock();
        loop {
            if state.closed
                || stop.load(Ordering::Relaxed)
                || until.is_some_and(|at| Instant::now() >= at)
            {
                return None;
            }
            if state.usage.bytes <= cap - bytes && state.usage.items < items {
                state.usage.bytes += bytes;
                state.usage.items += 1;
                state.usage.high_bytes = state.usage.high_bytes.max(state.usage.bytes);
                state.usage.high_items = state.usage.high_items.max(state.usage.items);
                return Some(Permit {
                    shared: self.shared.clone(),
                    bytes,
                });
            }
            state = self.shared.wait(state);
        }
    }
    fn connect(&self, stop: &AtomicBool) -> Option<Connection> {
        let mut state = self.shared.lock();
        loop {
            if state.closed || stop.load(Ordering::Relaxed) {
                return None;
            }
            if state.usage.connecting < CONNECTIONS {
                state.usage.connecting += 1;
                return Some(Connection {
                    shared: self.shared.clone(),
                });
            }
            state = self.shared.wait(state);
        }
    }
}
/// Holds the unique source registration until this handle stops.
/// The service must retire this incarnation itself when stopping intentionally;
/// cancellation never waits for an Ended event to fit in a full channel.
/// Stop bounds its join; pending background I/O retains worker/reader admission.
/// A future writer drain must wait for workers, readers and connections to
/// reach zero, plus any component-specific reservations, before taking over.
pub struct Handle {
    stream: stream::StreamHandle,
    resnapshot: Arc<Resnapshot>,
    _slot: Registration,
}
impl Handle {
    /// One atomic, nonblocking queue admission for this original raw source.
    /// The supervisor rechecks its wire generation before writing any line.
    /// Admission is not an upstream acknowledgement; callers fence with Pong.
    pub fn controls(&self, generation: u64, controls: &[Control]) -> bool {
        if !self.resnapshot.accepting(generation) {
            return false;
        }
        let Some(batch) =
            self._slot
                .shared
                .control_batch(&self._slot.identity, generation, controls)
        else {
            return false;
        };
        self.stream.source_controls(batch)
    }

    pub fn request_stop(&self) {
        self.stream.request_stop();
    }
    /// Only joins an already finished supervisor. Pending reader/helper guards
    /// remain in Sender::usage even after this returns true.
    pub fn join_if_finished(&mut self) -> bool {
        self.stream.join_source_if_finished()
    }
    /// Early owner drop, without waiting on upstream I/O. Not a drain barrier.
    pub fn detach(self) {
        self.stream.detach_source();
    }
    pub fn identity(&self) -> &Identity {
        &self._slot.identity
    }
    pub fn reconnect(&self) -> bool {
        self.stream.reconnect()
    }
    /// Ask the current greeted watch to reconnect for a complete snapshot.
    /// Nonblocking and coalesced; false for stale/offline/already-pending work.
    /// This transient request is cancelled if that generation ends meanwhile.
    /// Keep consuming ingress while it closes; queued old frames retain their
    /// original generation and must not restore coverage.
    pub fn resnapshot(&self, generation: u64) -> bool {
        if !self.resnapshot.request(generation) {
            return false;
        }
        if self.stream.wake_source_resnapshot() {
            true
        } else {
            self.resnapshot.close();
            false
        }
    }
    pub fn stop(self) {
        drop(self);
    }
}
#[derive(Clone)]
pub(crate) struct Endpoint {
    sender: Sender,
    identity: Identity,
    pub resnapshot: Arc<Resnapshot>,
    cancel: Option<stream::SourceCancel>,
    _worker: Option<Arc<Worker>>,
}
impl Endpoint {
    pub fn legacy_config(&self) -> Option<LegacyConfig> {
        self.sender.legacy.clone()
    }
    pub fn spawn_fn(&self) -> fn(&mut Command) -> io::Result<Child> {
        self.sender.spawn
    }
    pub fn legacy(
        &self,
        generation: u64,
        update: Legacy,
        observed_at: Instant,
        stop: &AtomicBool,
        until: Instant,
    ) -> bool {
        let bytes = match &update {
            Legacy::Begin { metadata, .. } => serde_json::to_vec(metadata).map(|b| b.len() + 1024),
            Legacy::Page { items, .. } => serde_json::to_vec(items).map(|b| b.len() + 1024),
            Legacy::Complete => Ok(512),
        };
        let Some(permit) = bytes.ok().and_then(|bytes| {
            self.sender
                .reserve_with(bytes, stop, DATA_BYTES, DATA_ITEMS, Some(until))
        }) else {
            return false;
        };
        self.sender
            .tx
            .send(Item {
                event: Event::Legacy {
                    source: self.identity.clone(),
                    generation,
                    update: Box::new(update),
                    observed_at,
                },
                _permit: permit,
            })
            .is_ok()
    }
    pub fn set_cancel(&mut self, cancel: stream::SourceCancel) {
        let mut state = self.sender.shared.lock();
        if state.closed {
            cancel.cancel();
        }
        if let Some(entry) = state.sources.get_mut(&self.identity.computer) {
            if entry.incarnation == self.identity.incarnation {
                entry.cancel = Some(cancel.clone());
            }
        }
        self.cancel = Some(cancel);
    }
    /// A descendant retaining a pipe must not create a leaked reader on every
    /// retry. Refuse another reader group until both old pipes end.
    pub fn reader(&self) -> Option<Reader> {
        let mut state = self.sender.shared.lock();
        if state.closed
            || self
                .cancel
                .as_ref()
                .is_some_and(stream::SourceCancel::cancelled)
            || state.readers.len() >= SOURCES
            || !state.readers.insert(self.identity.computer.clone())
        {
            return None;
        }
        state.usage.readers += 1;
        Some(Reader {
            shared: self.sender.shared.clone(),
            computer: self.identity.computer.clone(),
        })
    }
    pub fn resources(&self) -> Option<Arc<dyn Send + Sync>> {
        self._worker
            .as_ref()
            .map(|worker| worker.clone() as Arc<dyn Send + Sync>)
    }
    pub fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        (self.sender.spawn)(command)
    }

    pub fn reserve(&self, bytes: usize, stop: &AtomicBool) -> Option<Permit> {
        self.sender.reserve(bytes, stop)
    }
    pub fn connect(&self, stop: &AtomicBool) -> Option<Connection> {
        self.sender.connect(stop)
    }
    pub fn frame(
        &self,
        generation: u64,
        frame: Box<Frame>,
        received_at: Instant,
        permit: Permit,
    ) -> bool {
        self.sender
            .tx
            .send(Item {
                event: Event::Frame {
                    source: self.identity.clone(),
                    generation,
                    frame,
                    received_at,
                },
                _permit: permit,
            })
            .is_ok()
    }
    pub fn status(&self, generation: u64, status: ComputerStatus, stop: &AtomicBool) -> bool {
        let Some(permit) = self
            .sender
            .reserve_with(512, stop, INGRESS_BYTES, INGRESS_ITEMS, None)
        else {
            return false;
        };
        self.status_reserved(generation, status, permit)
    }
    pub fn status_reserved(&self, generation: u64, status: ComputerStatus, permit: Permit) -> bool {
        self.sender
            .tx
            .send(Item {
                event: Event::Status {
                    source: self.identity.clone(),
                    generation,
                    status,
                    observed_at: Instant::now(),
                },
                _permit: permit,
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests;
