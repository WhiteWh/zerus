//! The connector runtime (`Connector.run`, `connector.py:1259-1350`): a
//! heartbeat loop that projects the native listing, and a request loop that
//! flushes the outbox, long-polls the relay and runs each claimed request in
//! claim order, on one current_thread runtime.
//!
//! Reviewed differences from Python: claims wait for the first capability
//! probe, at most 3 s (a); a result the relay keeps refusing is retired
//! locally after three spaced attempts while other rows and polling go on
//! (b); a stored row the relay could not accept uploads as a fixed
//! substitute (g); SIGTERM and SIGINT stop claiming and let the request in
//! progress finish its native call within that call's own timeout (h).
use crate::accounts::{self, AccountCache, Refresh};
use crate::capabilities::{Flags, PROBE_TIMEOUT, REPROBE_SECONDS};
use crate::execute::{Fatal, Outcome};
use crate::fleet::{DynNative, Fleet, PeerNative};
use crate::http::{Relay, RelayError, MAX_POLL};
use crate::journal::{is_mutation, Journal, JournalError};
use crate::native::{native_argv, Call, Native, NativeError, NativeResult, Output};
use crate::projects::{self, is_archived};
use crate::request::{self, receipt};
use crate::validate::Context;
use futures_util::future::{self, Either};
use http::Method;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::time::Duration;
use tokio::sync::Notify;
use zerus_relay::json;
use zerus_relay::protocol;

/// nginx's default `client_max_body_size` on `/v1/`, and the relay's node limit.
pub const WIRE_LIMIT: usize = 1024 * 1024;
/// The relay parses every node body within these limits.
pub const RELAY_DEPTH: usize = 32;
pub const RELAY_VALUES: usize = 50_000;
/// Claims wait at most this long for the first capability probe (a).
pub const FIRST_PROBE_WAIT: Duration = Duration::from_secs(3);
/// A refused result gets this many attempts, this far apart (b).
pub const REFUSED_ATTEMPTS: u32 = 3;
pub const REFUSED_SPACING: f64 = 30.0;
pub const PRESENCE_RETRY: f64 = 300.0;
pub const PROJECT_SECONDS: f64 = 15.0;
pub const SWARM_TIMEOUT: f64 = 3.0;
/// `hgs task digests --json`, read with the catalog (seconds).
pub const TASK_DIGESTS_TIMEOUT: f64 = 5.0;

pub const RETIRED: &str = "Relay retired a result claim; preserving local outcome";
pub const REJECTED: &str = "Relay rejected a stored result; preserving local outcome";
/// The receipt uploaded for a stored row the relay could not accept (g).
pub const STORED_INVALID: &str =
    "the stored native result could not be uploaded; this request will not execute again";
pub const SNAPSHOT_TOO_LARGE: &str = "native snapshot exceeds relay size limit";
pub const INVALID_SNAPSHOT: &str = "native snapshot has invalid format";
const INVALID_LIST: &str = "relay returned invalid request list";
const LIST_TOO_LARGE: &str = "relay request list exceeds size limit";

/// A log line on stderr in Python's `%(levelname)s: %(message)s` form. A
/// closed stderr loses the line; it never stops the connector.
pub fn log(level: &str, message: &str) {
    use std::io::Write;
    #[cfg(feature = "test-hooks")]
    if crate::hooks::write_captured_stderr(level, message) {
        return;
    }
    let _ = writeln!(std::io::stderr(), "{level}: {message}");
}

pub const TASK_EXCHANGE_UNAVAILABLE: &str = "Configured hgs lacks mobile task exchange and digests; phone task sync is unavailable. Point connector.json hgs_path at the maintained hgs, or update the pinned CLI.";

/// A one-shot flag that tasks can wait for.
#[derive(Default)]
pub struct Event {
    set: Cell<bool>,
    notify: Notify,
}

impl Event {
    pub fn set(&self) {
        if !self.set.replace(true) {
            self.notify.notify_waiters();
        }
    }
    pub fn is_set(&self) -> bool {
        self.set.get()
    }
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

/// Machine caches, as `_init_machine_context` creates them.
#[derive(Default)]
pub struct State {
    pub sessions: HashSet<String>,
    /// Provider identities from the current listing, scoped to this machine.
    pub opencode_sessions: HashSet<String>,
    pub archives: HashSet<(String, String)>,
    pub snapshot_seen_at: Option<f64>,
    pub attention: HashMap<(String, String, String), Value>,
    pub attention_cursor: usize,
    pub project_cache: Option<Value>,
    /// Bounded native-only exact ID presence, never a relay schema extension.
    pub project_memberships: projects::PrimaryMemberships,
    pub project_next_poll: f64,
    pub capabilities_next_poll: f64,
    /// The latest listing's `desktop_presence`, sent beside the snapshot.
    pub presence: Value,
    pub presence_retry_at: f64,
    pub accounts: AccountCache,
    /// Verified attachment and file reads kept for their next ranges.
    pub reads: crate::files::Reads,
    /// Upload attempts the relay refused, by request (b).
    pub refused: HashMap<String, (u32, f64)>,
    /// The heartbeat's archive preview, its descriptor, the listing
    /// revision it reflects and its next refresh.
    pub archive_preview: Vec<Value>,
    pub archive_preview_meta: Value,
    pub archive_preview_revision: Option<String>,
    pub archive_preview_next: f64,
    /// Archive pairs admitted by a page, read or lookup, with when.
    pub paged_archives: HashMap<(String, String), f64>,
    /// The listed session ID of each live run in the latest listing
    /// (`session_selector::index`).
    pub session_ids: HashMap<String, String>,
}

impl State {
    /// The caches of a new context: nothing listed and no archive preview.
    pub fn fresh() -> Self {
        State {
            presence: Value::Null,
            archive_preview_meta: crate::archives::unavailable(),
            ..State::default()
        }
    }
}

/// Settings fixed at start.
#[derive(Debug, Clone)]
pub struct Settings {
    pub interval: f64,
    /// The relay binding, also the local project fallback identity.
    pub binding: String,
    /// Python's `subprocess_timeout`, for `min(self.timeout, 1)`.
    pub default_timeout: f64,
    /// Divides the spacing of refused uploads (test hook).
    pub clock_scale: f64,
}

pub struct Connector<N: Native> {
    pub native: Rc<N>,
    /// One journal for this computer and every peer context.
    pub journal: Rc<Journal>,
    pub settings: Settings,
    pub flags: Rc<Cell<Flags>>,
    /// A peer context's gateway capabilities: its calls pass the gateway's
    /// own mobile peer entry point first.
    gateway_flags: Option<Rc<Cell<Flags>>>,
    task_exchange_warning_logged: Cell<bool>,
    pub state: RefCell<State>,
    pub snapshot_ready: Event,
    /// The first capability probe has finished (`fleet.probe_complete`).
    pub probed: Event,
    pub stopping: Rc<Event>,
    /// The one-hop gateway; only the main connector has one.
    pub fleet: Option<Fleet>,
    /// A peer context: its native layer is the gateway's peer envelope.
    pub peer: bool,
    /// The local listing as `hgs watch --json` keeps it (main connector only).
    watch: crate::watch_listing::WatchListing,
    started: tokio::time::Instant,
    wall: Rc<dyn Fn() -> f64>,
}

tokio::task_local! {
    /// One immutable current-session route for one claimed request. It never
    /// propagates into another task or supplies native execution authority.
    pub(crate) static SESSION_LABEL_SELECTION: Option<crate::session_selector::Selection>;
    /// `LOCAL_MACHINE_GUARD`: while a request routed to this computer runs,
    /// its native calls go through the guarded local entry point.
    static LOCAL_MACHINE: String;
    static ENCRYPTED_LOCAL_MACHINE: String;
}

/// Why one iteration of the request loop stopped.
#[derive(Debug)]
pub enum LoopError {
    /// A ConnectorError text: logged, then the loop backs off.
    Warning(String),
    /// The journal failed: the connector stops.
    Fatal(JournalError),
}

impl From<RelayError> for LoopError {
    fn from(error: RelayError) -> Self {
        LoopError::Warning(error.message().into())
    }
}

impl<N: Native + 'static> Connector<N> {
    pub fn new(
        native: N,
        journal: Journal,
        settings: Settings,
        wall: Box<dyn Fn() -> f64>,
    ) -> Self {
        let native = Rc::new(native);
        let gateway: Rc<dyn DynNative> = native.clone();
        Connector {
            native,
            journal: Rc::new(journal),
            settings,
            flags: Rc::new(Cell::new(Flags::default().for_native_platform())),
            gateway_flags: None,
            task_exchange_warning_logged: Cell::new(false),
            state: RefCell::new(State::fresh()),
            snapshot_ready: Event::default(),
            probed: Event::default(),
            stopping: Rc::default(),
            fleet: Some(Fleet::new(gateway)),
            peer: false,
            watch: Default::default(),
            started: tokio::time::Instant::now(),
            wall: Rc::from(wall),
        }
    }

    /// `peer_context(machine_id, native)`: a context with its own caches and
    /// capabilities, the gateway's journal and stop, and no fleet.
    pub fn peer_context(&self, machine_id: &str, native: PeerNative) -> Connector<PeerNative> {
        Connector {
            native: Rc::new(native),
            journal: self.journal.clone(),
            settings: Settings {
                binding: machine_id.to_owned(),
                ..self.settings.clone()
            },
            flags: Rc::new(Cell::new(Flags::default())),
            gateway_flags: Some(Rc::clone(&self.flags)),
            task_exchange_warning_logged: Cell::new(false),
            state: RefCell::new(State::fresh()),
            snapshot_ready: Event::default(),
            probed: Event::default(),
            stopping: self.stopping.clone(),
            fleet: None,
            peer: true,
            watch: Default::default(),
            started: self.started,
            wall: self.wall.clone(),
        }
    }

    /// `fleet.available`: peer markers, a relay that took the manifest and
    /// a verified local machine.
    pub fn gateway_available(&self) -> bool {
        self.fleet.as_ref().is_some_and(|fleet| {
            self.flags.get().peers
                && self.relay_accepts_manifest(fleet)
                && fleet.local_id.borrow().is_some()
        })
    }

    /// How long a relay that refused the gateway waits for its next try (d),
    /// shortened by the test-hooks clock scale.
    fn relay_retry(&self) -> f64 {
        crate::fleet::RELAY_RETRY / self.settings.clock_scale
    }

    /// Whether the relay takes the extended heartbeat: after a refusal,
    /// again once its retry time has come (d).
    fn relay_accepts_manifest(&self, fleet: &Fleet) -> bool {
        if !fleet.relay_supported.get() && self.clock() >= fleet.relay_retry_at.get() {
            fleet.relay_supported.set(true);
        }
        fleet.relay_supported.get()
    }

    /// Monotonic seconds since start (Python's `time.monotonic()`).
    pub fn clock(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Wall-clock seconds (Python's `time.time()`).
    pub fn wall(&self) -> f64 {
        (self.wall)()
    }

    /// `Connector.native`: no call starts once the connector is stopping. A
    /// peer context calls through its route as it is; a request routed to
    /// this computer goes through the guarded local entry point.
    pub async fn native(&self, mut call: Call) -> NativeResult {
        if self.stopping.is_set() {
            return Err(NativeError::Stopping);
        }
        if call
            .argv
            .get(..2)
            .is_some_and(|v| v == ["__state", "delivery"])
        {
            // This read checks its physical machine inside the closed entry,
            // using the read-only catalog path. Do not wrap an older executable
            // in --expect-machine: its old guard could initialize that catalog.
            let machine = LOCAL_MACHINE
                .try_with(Clone::clone)
                .ok()
                .or_else(|| ENCRYPTED_LOCAL_MACHINE.try_with(Clone::clone).ok());
            if machine.is_some_and(|m| {
                call.payload
                    .as_ref()
                    .is_none_or(|p| p["expected_machine_id"] != m)
            }) {
                return Err(NativeError::Failed(crate::fleet::LOCAL_CHANGED));
            }
            return self.native.call(call).await;
        }
        // WhiteWh fork: a Windows gateway names folders as `/D:/...` to the
        // phone and as `D:\...` to hgs (windows_paths.rs).
        if self.flags.get().windows_gateway && !self.peer {
            crate::windows_paths::native_argv(&mut call.argv);
            if let Some(payload) = call.payload.as_mut() {
                crate::windows_paths::native_payload(payload);
            }
            let shared_catalog = call.argv == ["swarm", "get"];
            let mut result = self.native_call(call).await;
            if let Ok(Output::Json(value)) = &mut result {
                crate::windows_paths::phone_output(value);
                if shared_catalog {
                    let mut local = self
                        .native_call(Call::new(["project", "ls", "--json"]).timeout(5.0))
                        .await
                        .map(Output::json)
                        .unwrap_or(Value::Null);
                    crate::windows_paths::phone_output(&mut local);
                    crate::windows_paths::add_local_folders(value, &local);
                }
            }
            return result;
        }
        self.native_call(call).await
    }

    async fn native_call(&self, mut call: Call) -> NativeResult {
        if let Some(selected) = SESSION_LABEL_SELECTION
            .try_with(Clone::clone)
            .ok()
            .flatten()
        {
            if let Some(argv) = crate::session_selector::auxiliary_argv(&call.argv, &selected) {
                call.argv = argv;
                call = call.selecting(selected);
            }
        }
        if self.peer {
            return self.native.call(call).await;
        }
        let encrypted_machine = ENCRYPTED_LOCAL_MACHINE.try_with(Clone::clone).ok();
        if let Ok(machine) = LOCAL_MACHINE.try_with(Clone::clone) {
            let envelope = json!({"schema": 1, "target_machine_id": machine,
                                  "argv": call.argv, "payload": call.payload.take()});
            call.argv = ["swarm", "__mobile-peer-local", "--json"]
                .map(String::from)
                .into();
            call.payload = Some(envelope);
        }
        call.argv = native_argv(call.argv);
        if let Some(machine) = encrypted_machine {
            call.argv
                .splice(0..0, ["--expect-machine".to_owned(), machine]);
        }
        self.native.call(call).await
    }

    /// A fresh request can route its auxiliary reads only through the same
    /// native path that advertises complete label support. No renderer ID or
    /// label lookup supplies this hint; a missing/ambiguous run stays absent.
    pub(crate) fn labels_selection(
        &self,
        session: &str,
        payload: Option<&Value>,
    ) -> Option<crate::session_selector::Selection> {
        let payload = payload?;
        if payload.get("archive_id").is_some() || session.is_empty() {
            return None;
        }
        let flags = self.flags.get();
        let through_peer = self.peer || LOCAL_MACHINE.try_with(|_| ()).is_ok();
        if !flags.session_labels
            || !flags.session_selector
            || (through_peer && !flags.peer_session_id)
            || self.gateway_flags.as_ref().is_some_and(|f| {
                let f = f.get();
                !f.session_labels || !f.peer_session_id
            })
        {
            return None;
        }
        let run = payload
            .get("expected_run_id")
            .or_else(|| payload.get("run_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        if payload
            .get("expected_run_id")
            .zip(payload.get("run_id"))
            .is_some_and(|(a, b)| a != b)
        {
            return None;
        }
        let id = self.state.borrow().session_ids.get(run)?.clone();
        Some(crate::session_selector::Selection {
            name: session.into(),
            id,
        })
    }

    /// The session a phone mutation also selects by ID (zerus-ucck P1c-2b):
    /// the listed session ID of the request's exact run, once every CLI on the
    /// call's path lists the markers. A call through a mobile peer entry point,
    /// from a peer context or a request routed to this computer, also needs
    /// that CLI's [`crate::session_selector::PEER_MARKER`], and a peer's call
    /// the gateway's as well. Anything else keeps the name form.
    pub fn selection(
        &self,
        operation: &str,
        session: &str,
        payload: &Value,
    ) -> Option<crate::session_selector::Selection> {
        if !crate::session_selector::selects(operation, payload) {
            return None;
        }
        let flags = self.flags.get();
        let through_peer_entry = self.peer || LOCAL_MACHINE.try_with(|_| ()).is_ok();
        let gateway = self.gateway_flags.as_ref().map(|gateway| gateway.get());
        if !flags.session_selector
            || (through_peer_entry && !flags.peer_session_id)
            || gateway.is_some_and(|gateway| !gateway.peer_session_id)
        {
            return None;
        }
        let run = payload
            .get("expected_run_id")
            .and_then(Value::as_str)
            .filter(|run| !run.is_empty())?;
        let id = self.state.borrow().session_ids.get(run)?.clone();
        Some(crate::session_selector::Selection {
            name: session.to_owned(),
            id,
        })
    }

    /// Runs `future` with this computer's guarded native entry point.
    pub async fn guarded<T>(machine: String, future: impl Future<Output = T>) -> T {
        LOCAL_MACHINE.scope(machine, future).await
    }

    pub async fn encrypted_guarded<T>(machine: String, future: impl Future<Output = T>) -> T {
        ENCRYPTED_LOCAL_MACHINE.scope(machine, future).await
    }

    pub fn context(&self) -> Context {
        let state = self.state.borrow();
        Context {
            sessions: state.sessions.clone(),
            archives: state.archives.clone(),
            snapshot_age: state
                .snapshot_seen_at
                .map_or(f64::INFINITY, |seen| self.clock() - seen),
            paged: state
                .paged_archives
                .iter()
                .filter(|(_, at)| self.clock() - **at <= crate::archives::PAGED_SECONDS)
                .map(|(pair, _)| pair.clone())
                .collect(),
            archive_supported: self.flags.get().archive,
        }
    }

    /// `probe_capabilities`: at most every 300 s; a failed probe keeps the
    /// previous legacy flags (a). Projection alone is revoked while a due probe
    /// is in flight, and stays unavailable after failure or non-text help.
    pub async fn probe_capabilities(&self) {
        {
            let mut state = self.state.borrow_mut();
            if self.clock() < state.capabilities_next_poll {
                return;
            }
            state.capabilities_next_poll = self.clock() + REPROBE_SECONDS;
        }
        let mut flags = self.flags.get();
        flags.history_projection = false;
        flags.send_receipt = false;
        flags.session_labels = false;
        self.flags.set(flags);
        if let Ok(Output::Text(help)) = self
            .native(Call::new(["--help"]).timeout(PROBE_TIMEOUT).text())
            .await
        {
            let flags = Flags::from_help(&help).for_native_platform();
            if !self.peer
                && !flags.task_exchange
                && !self.task_exchange_warning_logged.replace(true)
            {
                log("WARNING", TASK_EXCHANGE_UNAVAILABLE);
            }
            self.flags.set(flags);
        }
        self.probed.set();
    }

    /// The local `ls --json --local --no-archives` listing from the event
    /// stream while it is live, which it starts on first use; None for a peer,
    /// a host without paged archives or watch-v1, and a request routed to this
    /// computer, whose calls take the guarded entry point.
    fn streamed_listing(&self, paged: bool) -> Option<Value> {
        let guarded = LOCAL_MACHINE.try_with(|_| ()).is_ok()
            || ENCRYPTED_LOCAL_MACHINE.try_with(|_| ()).is_ok();
        if self.peer || !paged || !self.flags.get().watch || guarded || self.stopping.is_set() {
            return None;
        }
        let native = Rc::clone(&self.native);
        native.command()?;
        self.watch.follow(
            move || native.command().expect("the native CLI is a process"),
            self.stopping.clone(),
        );
        self.watch.listing()
    }

    /// `Connector.snapshot()`: the listing as phones may see it.
    pub async fn snapshot(self: &Rc<Self>) -> Result<Value, String> {
        // The archive marker decides the listing's shape, so it is current first.
        self.probe_capabilities().await;
        let paged = self.flags.get().archive;
        let mut argv = vec!["ls", "--json", "--local"];
        if paged {
            argv.push("--no-archives");
        }
        let listing = match self.streamed_listing(paged) {
            Some(listing) => listing,
            None => self
                .native(Call::new(argv))
                .await
                .map_err(native_text)?
                .json(),
        };
        let Value::Object(mut snapshot) = listing else {
            return Err(INVALID_SNAPSHOT.into());
        };
        let mut sessions = match snapshot.remove("sessions") {
            Some(Value::Array(sessions)) => sessions,
            _ => return Err(INVALID_SNAPSHOT.into()),
        };
        snapshot.insert("mobile_archive".into(), crate::archives::unavailable());
        if paged {
            // A paging CLI lists live sessions only; a bounded recent window
            // of archives serves phones that only read snapshots.
            self.refresh_archive_preview(snapshot.get("archive_revision"))
                .await;
            let state = self.state.borrow();
            sessions.extend(state.archive_preview.iter().cloned());
            snapshot.insert("mobile_archive".into(), state.archive_preview_meta.clone());
        }
        if self.flags.get().windows_gateway {
            sessions.retain(crate::capabilities::windows_session);
        }
        // Native SSH aliases and the peer-of-peer inventory stay local.
        snapshot.remove("peers");
        // Ended launches carry the last screen lines of failed starts for the
        // desktop's placeholders; terminal text never leaves this computer.
        snapshot.remove("ended_launches");
        // Presence ages on every listing: outside the snapshot it keeps the
        // relay's unchanged-snapshot shortcut.
        let presence = snapshot.remove("desktop_presence");
        for row in sessions.iter_mut() {
            // Observation clocks are freshness evidence, not a change in the
            // background work itself.
            if let Some(Value::Object(work)) = row.get_mut("process_work") {
                work.remove("sampled_at");
            }
        }
        {
            let mut state = self.state.borrow_mut();
            state.presence = presence.filter(Value::is_object).unwrap_or(Value::Null);
            state.sessions = sessions
                .iter()
                .filter_map(Value::as_object)
                .filter(|row| !is_archived(row))
                .filter_map(|row| row.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            state.opencode_sessions = sessions
                .iter()
                .filter(|row| row.get("cmd").and_then(Value::as_str) == Some("opencode"))
                .filter_map(|row| row.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            state.session_ids =
                crate::session_selector::index(sessions.iter().filter_map(Value::as_object));
            state.archives = sessions
                .iter()
                .filter_map(Value::as_object)
                .filter(|row| is_archived(row))
                .filter_map(|row| {
                    let name = row.get("name").and_then(Value::as_str)?;
                    let archive = row.get("archive_id").and_then(Value::as_str)?;
                    Some((name.to_owned(), archive.to_owned()))
                })
                .collect();
            state.snapshot_seen_at = Some(self.clock());
        }
        self.snapshot_ready.set();
        let host = snapshot.get("host").cloned();
        let ((), projects) =
            future::join(self.read_attention(&sessions), self.read_projects(host)).await;
        self.apply_attention(&mut sessions);
        for row in &mut sessions {
            crate::opencode_privacy::row(row);
        }
        let mut projects =
            projects.unwrap_or_else(|| projects::unavailable(&self.settings.binding));
        self.state
            .borrow()
            .project_memberships
            .apply(&mut sessions, &projects);
        if paged {
            // Archive membership is applied to explicit pages; thousands of
            // IDs do not belong in every live-session heartbeat.
            if let Some(Value::Array(groups)) = projects.get_mut("projects") {
                groups.retain(Value::is_object);
                for group in groups.iter_mut().filter_map(Value::as_object_mut) {
                    group.remove("archives");
                }
            }
        }
        snapshot.insert("sessions".into(), Value::Array(sessions));
        snapshot.insert("mobile_projects".into(), projects);
        let accounts = self.update_accounts().await;
        snapshot.insert("mobile_accounts".into(), accounts);
        self.probe_capabilities().await;
        let capabilities = self
            .flags
            .get()
            .advertised_for(self.fleet.is_some(), self.gateway_available());
        snapshot.insert("mobile_capabilities".into(), capabilities);
        Ok(Value::Object(snapshot))
    }

    /// The archive preview of a paging CLI, read again when the listing's
    /// archive revision changes and otherwise every 300 s (15 s after a
    /// failure). An old preview is dropped before a refresh can fail.
    async fn refresh_archive_preview(&self, revision: Option<&Value>) {
        let Some(revision) = revision.and_then(Value::as_str) else {
            return;
        };
        let now = self.clock();
        {
            let mut state = self.state.borrow_mut();
            if state.archive_preview_revision.as_deref() == Some(revision)
                && now < state.archive_preview_next
            {
                return;
            }
            state.archive_preview.clear();
            state.archive_preview_meta = crate::archives::unavailable();
        }
        let windows = self.flags.get().windows_gateway;
        let call = crate::archives::call(false, windows)
            .payload(json!({}))
            .timeout(5.0);
        let preview = match self.native(call).await {
            Ok(output) => {
                let page = output.json();
                if windows && !crate::archives::supported_page(&page) {
                    None
                } else {
                    crate::archives::preview(&page).ok()
                }
            }
            Err(_) => None,
        };
        let mut state = self.state.borrow_mut();
        match preview {
            Some((rows, meta)) => {
                state.archive_preview = rows;
                state.archive_preview_meta = meta;
            }
            None => {
                state.archive_preview_meta["error"] = json!(crate::archives::PREVIEW_ERROR);
            }
        }
        let available = state.archive_preview_meta["available"] == json!(true);
        state.archive_preview_revision = Some(revision.to_owned());
        state.archive_preview_next = now + if available { 300.0 } else { 15.0 };
    }

    /// The attention key of a live tracked Codex row the scan may inspect.
    fn attention_key(row: &Value) -> Option<(String, String, String)> {
        let row = row.as_object()?;
        let text = |key: &str| row.get(key).and_then(Value::as_str);
        if text("cmd") != Some("codex")
            || row.get("tracked") != Some(&json!(true))
            || text("runtime_state") != Some("live")
            || text("process_state") != Some("running")
            || is_archived(row)
            || matches!(text("state"), Some("paused" | "stopped"))
        {
            return None;
        }
        let (name, run, conversation) = (text("name")?, text("run_id")?, text("conversation_id")?);
        let control = |c: char| (c as u32) < 32 || c as u32 == 127;
        if name.is_empty()
            || run.is_empty()
            || conversation.is_empty()
            || name.starts_with(['@', '-'])
            || name.contains('@')
            || name.chars().any(control)
        {
            return None;
        }
        Some((name.to_owned(), run.to_owned(), conversation.to_owned()))
    }

    /// `enrich_attention`'s native part: inspect two eligible sessions per
    /// poll, without submitting input, and refresh their cached cards. Only
    /// question IDs and hashes leave the computer.
    async fn read_attention(&self, sessions: &[Value]) {
        let eligible: Vec<_> = sessions.iter().filter_map(Self::attention_key).collect();
        let selected: Vec<(String, String, String)> = {
            let mut state = self.state.borrow_mut();
            state.attention.retain(|key, _| eligible.contains(key));
            if eligible.is_empty() {
                state.attention_cursor = 0;
                return;
            }
            let selected: Vec<_> = (0..eligible.len().min(2))
                .map(|offset| eligible[(state.attention_cursor + offset) % eligible.len()].clone())
                .collect();
            state.attention_cursor = (state.attention_cursor + selected.len()) % eligible.len();
            selected
        };
        let timeout = self.settings.default_timeout.min(1.0);
        let inspections = selected.iter().map(|key| async move {
            let selection = self.labels_selection(&key.0, Some(&json!({"expected_run_id":key.1})));
            let detail = SESSION_LABEL_SELECTION
                .scope(
                    selection,
                    self.native(Call::new(["inspect", key.0.as_str()]).timeout(timeout)),
                )
                .await;
            (key, detail)
        });
        for (key, detail) in future::join_all(inspections).await {
            let cards = detail
                .ok()
                .and_then(|detail| attention_cards(&detail.json(), key));
            let mut state = self.state.borrow_mut();
            match cards {
                Some(cards) => {
                    state.attention.insert(key.clone(), cards);
                }
                None => {
                    state.attention.remove(key);
                }
            }
        }
    }

    fn apply_attention(&self, sessions: &mut [Value]) {
        let state = self.state.borrow();
        for row in sessions.iter_mut() {
            let Some(key) = Self::attention_key(row) else {
                continue;
            };
            if let (Some(cards), Some(row)) = (state.attention.get(&key), row.as_object_mut()) {
                let merged = merge_attention(row.get("mobile_attention"), cards);
                row.insert("mobile_attention".into(), merged);
            }
        }
    }

    /// `enrich_projects`' native part: the catalog every 15 s, the cached
    /// one (marked stale after a failed read) in between.
    async fn read_projects(&self, host: Option<Value>) -> Option<Value> {
        let due = {
            let mut state = self.state.borrow_mut();
            let due = self.clock() >= state.project_next_poll;
            if due {
                state.project_next_poll = self.clock() + PROJECT_SECONDS;
            }
            due
        };
        if due {
            let snapshot = json!({"host": host.unwrap_or(Value::Null)});
            let mut fresh = self
                .native(Call::new(["swarm", "get"]).timeout(SWARM_TIMEOUT))
                .await
                .ok()
                .and_then(|native| {
                    let raw = native.json();
                    if self.flags.get().session_labels
                        && raw.get("session_id_memberships").is_none()
                    {
                        return None;
                    }
                    let primary = projects::PrimaryMemberships::read(&raw).ok()?;
                    let mut catalog = if primary.available() {
                        projects::normalize(&raw, &snapshot, &self.settings.binding).ok()?
                    } else {
                        projects::unavailable(&self.settings.binding)
                    };
                    primary.project(&mut catalog);
                    Some((catalog, primary))
                });
            let primary = fresh.as_ref().map(|(_, primary)| primary.clone());
            let mut fresh = fresh.take().map(|(catalog, _)| catalog);
            // Each project's phone-view digest; a failed read leaves them out.
            let digests = |catalog: &&mut Value| {
                self.flags.get().task_exchange && catalog["available"] == json!(true)
            };
            if let Some(catalog) = fresh.as_mut().filter(digests) {
                let call = Call::new(["task", "digests", "--json"]).timeout(TASK_DIGESTS_TIMEOUT);
                if let Ok(native) = self.native(call).await {
                    projects::apply_task_digests(catalog, &native.json());
                }
            }
            let mut state = self.state.borrow_mut();
            if let Some(primary) = primary {
                state.project_memberships = primary;
            } else if self.flags.get().session_labels {
                state.project_memberships = projects::PrimaryMemberships::Unavailable;
            }
            let cache = match fresh {
                Some(fresh) => fresh,
                None => {
                    let mut stale = state
                        .project_cache
                        .clone()
                        .unwrap_or_else(|| projects::unavailable(&self.settings.binding));
                    stale["stale"] = json!(true);
                    stale
                }
            };
            state.project_cache = Some(cache);
        }
        self.state.borrow().project_cache.clone()
    }

    /// `AccountCache.update()`: the catalog every 30 s, then one usage
    /// refresh in the background when one is due.
    async fn update_accounts(self: &Rc<Self>) -> Value {
        let due = {
            let mut state = self.state.borrow_mut();
            let due = state.accounts.catalog_due(self.clock());
            if due {
                state.accounts.next_catalog = self.clock() + accounts::CATALOG_SECONDS;
            }
            due
        };
        if due {
            let fresh = match self
                .native(Call::new(["account", "ls"]).timeout(accounts::CATALOG_TIMEOUT))
                .await
            {
                Ok(raw) => accounts::catalog(&raw.json()),
                Err(_) => Err(crate::pyvalue::PyError::Type),
            };
            let wall = self.wall();
            self.state.borrow_mut().accounts.apply_catalog(fresh, wall);
        }
        let refresh = self.state.borrow_mut().accounts.next_refresh(self.clock());
        if let Some(refresh) = refresh {
            let connector = Rc::clone(self);
            // Tokio task locals are not inherited by spawned tasks. Capture
            // this observation's immutable binding before its scope ends;
            // legacy observations keep their existing unguarded native argv.
            let machine = ENCRYPTED_LOCAL_MACHINE.try_with(Clone::clone).ok();
            tokio::task::spawn_local(async move {
                let refresh = connector.refresh_account(refresh);
                match machine {
                    Some(machine) => Self::encrypted_guarded(machine, refresh).await,
                    None => refresh.await,
                }
            });
        }
        let wall = self.wall();
        self.state.borrow().accounts.view(wall)
    }

    async fn refresh_account(&self, refresh: Refresh) {
        let call = Call::new(["account", "inspect", refresh.identity.as_str()])
            .timeout(accounts::INSPECT_TIMEOUT);
        let raw = self.native(call).await.ok().map(Output::json);
        self.state
            .borrow_mut()
            .accounts
            .finish_refresh(&refresh, raw.as_ref());
    }

    /// One heartbeat: the projected snapshot with presence beside it, and
    /// with a usable gateway the peer manifest. A relay that refuses the
    /// manifest with 400, 404 or 422 gets the plain heartbeat, and the
    /// manifest again after 300 s (d).
    pub async fn heartbeat(self: &Rc<Self>, relay: &Relay) -> Result<(), String> {
        let snapshot = self.snapshot().await?;
        let presence = self.state.borrow().presence.clone();
        let available = self.gateway_available();
        let body = match &self.fleet {
            Some(fleet) => fleet.heartbeat(snapshot, available),
            None => Map::from_iter([("snapshot".to_owned(), snapshot)]),
        };
        let path = "/v1/node/heartbeat";
        match self
            .post_with_presence(relay, path, body.clone(), presence.clone())
            .await
        {
            Ok(()) => {
                if let Some(fleet) = &self.fleet {
                    fleet.confirm_manifest(&body);
                }
                Ok(())
            }
            Err(RelayError::Status(400 | 404 | 422)) if body.len() > 1 => {
                if let Some(fleet) = &self.fleet {
                    fleet.relay_supported.set(false);
                    fleet.relay_retry_at.set(self.clock() + self.relay_retry());
                }
                let mut snapshot = body["snapshot"].clone();
                if let Some(Value::Array(features)) =
                    snapshot.pointer_mut("/mobile_capabilities/features")
                {
                    features.retain(|feature| feature != "gateway_one_hop");
                }
                let plain = Map::from_iter([("snapshot".to_owned(), snapshot)]);
                self.post_with_presence(relay, path, plain, presence)
                    .await
                    .map_err(|error| error.message().to_owned())
            }
            Err(error) => Err(error.message().to_owned()),
        }
    }

    /// `Fleet.discover`: the native inventory decides the routes; an
    /// invalid or failed inventory withdraws them all, rather than keep a
    /// stale alias usable after an outage.
    pub async fn discover(&self) {
        let Some(fleet) = &self.fleet else {
            return;
        };
        let _discovery = fleet.discovery.lock().await;
        if !self.flags.get().peers {
            return;
        }
        let mut call =
            Call::new(["swarm", "mobile-peers", "--json"]).timeout(crate::fleet::INVENTORY_TIMEOUT);
        call.max_output_bytes = Some(crate::fleet::INVENTORY_BYTES);
        let inventory = match self.native(call).await {
            Ok(output) => crate::fleet::inventory(&output.json()).ok(),
            Err(_) => None,
        };
        let routes = match inventory {
            Some((local, routes)) => {
                *fleet.local_id.borrow_mut() = Some(local);
                routes
            }
            None => {
                log("WARNING", "Direct peer discovery unavailable");
                Default::default()
            }
        };
        *fleet.routes.borrow_mut() = routes.clone();
        let mut contexts = fleet.contexts.borrow_mut();
        contexts.retain(|identity, _| routes.contains_key(identity));
        for (identity, route) in &routes {
            if contexts.contains_key(identity) {
                continue;
            }
            let native = PeerNative {
                gateway: fleet.gateway.clone(),
                routes: fleet.routes.clone(),
                slots: fleet.slots.clone(),
                identity: identity.clone(),
                machine: route.machine_id.clone(),
                default_timeout: self.settings.default_timeout,
            };
            let context = self.peer_context(&route.machine_id, native);
            contexts.insert(identity.clone(), Rc::new(context));
        }
    }

    /// `Fleet.publish`: every online route the relay accepted gets its
    /// peer's heartbeat. One unavailable peer never affects another route.
    pub async fn publish(&self, relay: &Relay) {
        let Some(fleet) = &self.fleet else {
            return;
        };
        let routes: Vec<(String, crate::fleet::Route)> = fleet
            .routes
            .borrow()
            .iter()
            .map(|(identity, route)| (identity.clone(), route.clone()))
            .collect();
        let peers = routes.into_iter().map(|(identity, route)| async move {
            let key = (identity.clone(), route.machine_id.clone());
            if !route.online || !fleet.published.borrow().contains(&key) {
                return;
            }
            let Some(context) = fleet.contexts.borrow().get(&identity).cloned() else {
                log("WARNING", "Peer route was withdrawn during observation");
                return;
            };
            let Ok(snapshot) = context.snapshot().await else {
                log("WARNING", "Peer heartbeat unavailable");
                return;
            };
            let body = Map::from_iter([
                ("machine_id".to_owned(), json!(route.machine_id)),
                ("snapshot".to_owned(), snapshot),
            ]);
            let size = json::python_default_len(&Value::Object(body.clone()), true);
            if size.is_err() || size.is_ok_and(|size| size > crate::fleet::MAX_PEER_SNAPSHOT) {
                log("WARNING", "Peer heartbeat unavailable");
                return;
            }
            let current = fleet.routes.borrow().get(&identity).cloned();
            if current.as_ref() != Some(&route) || !fleet.published.borrow().contains(&key) {
                return;
            }
            let presence = context.state.borrow().presence.clone();
            let path = format!("/v1/node/peers/{identity}/heartbeat");
            if let Err(error) = self.post_with_presence(relay, &path, body, presence).await {
                // Only an unsupported method identifies an incompatible relay.
                if error.status() == Some(405) {
                    fleet.relay_supported.set(false);
                    fleet.relay_retry_at.set(self.clock() + self.relay_retry());
                    fleet.published.borrow_mut().clear();
                }
                log("WARNING", "Peer heartbeat unavailable");
            }
        });
        future::join_all(peers).await;
    }

    /// `Fleet.run`: discovery and peer heartbeats every interval while the
    /// CLI and the relay support the gateway.
    pub async fn gateway(&self, relay: &Relay) {
        let interval = self.settings.interval;
        loop {
            if let Some(fleet) = &self.fleet {
                if self.flags.get().peers && self.relay_accepts_manifest(fleet) {
                    self.discover().await;
                    self.publish(relay).await;
                }
            }
            let pause = Duration::try_from_secs_f64(interval).unwrap_or(Duration::MAX);
            tokio::time::sleep(pause).await;
        }
    }

    /// Presence beside a snapshot. An older relay refuses the additive field
    /// with 400 or 422: send without it and probe again after five minutes.
    async fn post_with_presence(
        &self,
        relay: &Relay,
        path: &str,
        body: Map<String, Value>,
        presence: Value,
    ) -> Result<(), RelayError> {
        let plain = Value::Object(body);
        if self.clock() < self.state.borrow().presence_retry_at {
            return post(relay, path, &plain).await;
        }
        let mut with = plain.clone();
        with["presence"] = presence;
        match post(relay, path, &with).await {
            Err(RelayError::Status(400 | 422)) => {}
            other => return other,
        }
        post(relay, path, &plain).await?;
        // Only a plain success proves the field, not other body content, was refused.
        self.state.borrow_mut().presence_retry_at = self.clock() + PRESENCE_RETRY;
        Ok(())
    }

    /// `heartbeats()`: forever, backing off up to 60 s after a failure.
    pub async fn heartbeats(self: &Rc<Self>, relay: &Relay) {
        let interval = self.settings.interval;
        let mut delay = interval;
        loop {
            match self.heartbeat(relay).await {
                Ok(()) => delay = interval,
                Err(error) => {
                    log("WARNING", &format!("Heartbeat: {error}"));
                    delay = (delay * 2.0).max(interval).min(60.0);
                }
            }
            // Any finite `poll_interval` of at least 1 s is valid; one beyond
            // Duration's range sleeps as long as tokio can, as Python's would.
            let pause = Duration::try_from_secs_f64(delay).unwrap_or(Duration::MAX);
            tokio::time::sleep(pause).await;
        }
    }

    /// The body to upload for a stored receipt: its text as stored, or a
    /// fixed substitute when the relay could not accept it (g). The local
    /// row is never rewritten.
    pub fn upload_body(text: &str, request_id: &str, operation: &str) -> Vec<u8> {
        let acceptable = text.len() <= WIRE_LIMIT
            && request::parse(text.as_bytes(), RELAY_DEPTH, RELAY_VALUES)
                .is_ok_and(|value| protocol::result(&value, request_id).is_ok());
        if acceptable {
            return text.as_bytes().to_vec();
        }
        let state = if is_mutation(operation) {
            "uncertain"
        } else {
            "failed"
        };
        let substitute = receipt(state, Value::Null, Some(STORED_INVALID));
        json::canonical(&substitute)
            .expect("a fixed receipt encodes")
            .into_bytes()
    }

    /// `flush()`: upload undelivered receipts. A 404 or 409 retires the
    /// claim; a 400, 413 or 422 is retried at most twice more, 30 s apart,
    /// and then the row is marked delivered locally (b). Any other failure
    /// stops this flush, as in Python.
    pub async fn flush(&self, relay: &Relay) -> Result<(), LoopError> {
        self.journal.prune(self.wall()).map_err(LoopError::Fatal)?;
        let spacing = REFUSED_SPACING / self.settings.clock_scale;
        let rows = self.journal.pending_rows().map_err(LoopError::Fatal)?;
        for (request_id, operation, text) in rows {
            let waiting = self
                .state
                .borrow()
                .refused
                .get(&request_id)
                .is_some_and(|(_, last)| self.clock() - last < spacing);
            if waiting {
                continue;
            }
            let body = Self::upload_body(&text, &request_id, &operation);
            let path = format!("/v1/node/requests/{request_id}/result");
            match relay
                .call(Method::POST, &path, Some(body), crate::http::MAX_RESPONSE)
                .await
            {
                Ok(_) => {}
                Err(RelayError::Status(404 | 409)) => {
                    // Relay expiry or restart can retire a claim: keep the
                    // local outcome and its no-replay identity, retire the upload.
                    log("WARNING", RETIRED);
                }
                Err(RelayError::Status(400 | 413 | 422)) => {
                    let attempts = {
                        let mut state = self.state.borrow_mut();
                        let entry = state.refused.entry(request_id.clone()).or_insert((0, 0.0));
                        entry.0 += 1;
                        entry.1 = self.clock();
                        entry.0
                    };
                    if attempts < REFUSED_ATTEMPTS {
                        continue;
                    }
                    log("WARNING", REJECTED);
                }
                Err(error) => return Err(error.into()),
            }
            // Accepted, retired or given up: no attempt of this row is due
            // any more, so it no longer shortens the long poll.
            self.state.borrow_mut().refused.remove(&request_id);
            self.journal
                .delivered(&request_id, self.wall())
                .map_err(LoopError::Fatal)?;
        }
        Ok(())
    }

    /// Claims wait for the listing and, at most 3 s, for the first probe (a).
    async fn ready_to_claim(&self) {
        self.snapshot_ready.wait().await;
        if !self.probed.is_set() {
            let _ = tokio::time::timeout(FIRST_PROBE_WAIT, self.probed.wait()).await;
        }
    }

    /// The long-poll wait: 25 s, or less while a refused result waits for its
    /// next attempt, so the next flush comes when that attempt is due (b).
    pub fn poll_wait(&self) -> u64 {
        let spacing = REFUSED_SPACING / self.settings.clock_scale;
        let now = self.clock();
        let due = self
            .state
            .borrow()
            .refused
            .values()
            .map(|(_, last)| last + spacing - now)
            .fold(f64::INFINITY, f64::min);
        if due.is_finite() {
            due.ceil().clamp(1.0, 25.0) as u64
        } else {
            25
        }
    }

    /// One poll after a flush: the claimed requests.
    async fn poll(&self, relay: &Relay) -> Result<Vec<Value>, LoopError> {
        self.flush(relay).await?;
        self.ready_to_claim().await;
        // A gateway whose machine is not yet verified claims soon again.
        let pending = !self.probed.is_set()
            || self.fleet.as_ref().is_some_and(|fleet| {
                self.flags.get().peers
                    && self.relay_accepts_manifest(fleet)
                    && fleet.local_id.borrow().is_none()
            });
        let wait = if pending { 1 } else { self.poll_wait() };
        let one_hop = if self.gateway_available() {
            "&gateway_one_hop=1"
        } else {
            ""
        };
        let path = format!("/v1/node/requests?wait={wait}{one_hop}");
        let batch = relay.call(Method::GET, &path, None, MAX_POLL).await?;
        let Some(Value::Object(mut batch)) = batch else {
            return Err(LoopError::Warning(INVALID_LIST.into()));
        };
        let Some(Value::Array(requests)) = batch.remove("requests") else {
            return Err(LoopError::Warning(INVALID_LIST.into()));
        };
        if requests.len() > 100 {
            return Err(LoopError::Warning(LIST_TOO_LARGE.into()));
        }
        Ok(requests)
    }

    /// One iteration of `requests()`: `Ok(false)` once the connector stops.
    async fn iteration(&self, relay: &Relay) -> Result<bool, LoopError> {
        let Some(requests) = until_stopped(&self.stopping, self.poll(relay)).await else {
            return Ok(false);
        };
        let requests = requests?;
        let idle = requests.is_empty();
        for request in requests {
            if self.stopping.is_set() {
                // Claimed but never started: the relay's claim expires.
                return Ok(false);
            }
            let outcome = self
                .execute(request)
                .await
                .map_err(|Fatal(error)| LoopError::Fatal(error))?;
            if let Outcome::Rejected(error) = outcome {
                log("WARNING", &format!("Rejected relay request: {error}"));
            }
        }
        if self.stopping.is_set() {
            return Ok(false);
        }
        match until_stopped(&self.stopping, self.flush(relay)).await {
            None => return Ok(false),
            Some(result) => result?,
        }
        if idle {
            let pause = tokio::time::sleep(Duration::from_millis(250));
            return Ok(until_stopped(&self.stopping, pause).await.is_some());
        }
        Ok(true)
    }

    /// `requests()`, until a stop signal: requests run one at a time in claim
    /// order, and a request in progress always finishes.
    pub async fn requests(&self, relay: &Relay) -> Result<(), Fatal> {
        let mut delay = 1.0f64;
        loop {
            match self.iteration(relay).await {
                Ok(true) => delay = 1.0,
                Ok(false) => return Ok(()),
                Err(LoopError::Fatal(error)) => return Err(Fatal(error)),
                Err(LoopError::Warning(error)) => {
                    log("WARNING", &format!("Requests: {error}"));
                    let pause = tokio::time::sleep(Duration::from_secs_f64(delay));
                    if until_stopped(&self.stopping, pause).await.is_none() {
                        return Ok(());
                    }
                    delay = (delay * 2.0).min(60.0);
                }
            }
        }
    }
}

/// `future`, unless the connector starts stopping first.
pub async fn until_stopped<T>(stopping: &Event, future: impl Future<Output = T>) -> Option<T> {
    let stop = pin!(stopping.wait());
    let future = pin!(future);
    match future::select(future, stop).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(((), _)) => None,
    }
}

/// A POST of a JSON body, within the wire and parse limits the relay and
/// its nginx impose, so a body they would refuse is never sent.
async fn post(relay: &Relay, path: &str, body: &Value) -> Result<(), RelayError> {
    let encoded = json::canonical(body).map_err(|_| RelayError::Failed(SNAPSHOT_TOO_LARGE))?;
    if encoded.len() > WIRE_LIMIT || !within_relay_limits(encoded.as_bytes()) {
        return Err(RelayError::Failed(SNAPSHOT_TOO_LARGE));
    }
    relay
        .call(
            Method::POST,
            path,
            Some(encoded.into_bytes()),
            crate::http::MAX_RESPONSE,
        )
        .await
        .map(drop)
}

/// The relay's depth and value bounds for a node body.
pub fn within_relay_limits(raw: &[u8]) -> bool {
    json::parse(raw, RELAY_DEPTH, RELAY_VALUES).is_ok()
}

fn native_text(error: NativeError) -> String {
    match error {
        NativeError::Failed(text) => text.into(),
        NativeError::Unicode => crate::execute::GENERIC.into(),
        NativeError::Stopping => crate::journal::RESTART_ERROR.into(),
        NativeError::SessionChanged => crate::session_selector::CHANGED.into(),
    }
}

/// The attention cards of one inspection, or `None` to drop the cache entry.
fn attention_cards(detail: &Value, key: &(String, String, String)) -> Option<Value> {
    let (_, run, conversation) = key;
    let detail = detail.as_object()?;
    let text = |key: &str| detail.get(key).and_then(Value::as_str);
    if text("run_id") != Some(run.as_str())
        || text("conversation_id") != Some(conversation.as_str())
        || text("runtime_state") != Some("live")
        || text("process_state") != Some("running")
    {
        return None;
    }
    let questions = detail.get("pending_questions")?.as_array()?;
    let mut cards: Vec<(String, String, bool)> = Vec::new();
    for card in questions {
        let Some(card) = card.as_object() else {
            continue;
        };
        let field = |key: &str| card.get(key).and_then(Value::as_str);
        if field("run_id") != Some(run.as_str())
            || field("conversation_id") != Some(conversation.as_str())
        {
            continue;
        }
        let (Some(id), Some(hash)) = (field("question_id"), field("question_hash")) else {
            continue;
        };
        if id.is_empty() || hash.is_empty() {
            continue;
        }
        let submitted = card
            .get("answer_delivery")
            .and_then(Value::as_object)
            .is_some_and(|delivery| {
                delivery.get("status").and_then(Value::as_str) == Some("submitted")
            });
        // Phones decide whether to notify; blocking items keep their exact shape.
        let optional = card.get("optional") == Some(&Value::Bool(true))
            && field("source") == Some("codex_async");
        if !submitted {
            cards.push((id.into(), hash.into(), optional));
        }
    }
    // Python's stable sort by (question_id, question_hash).
    cards.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    Some(Value::Array(
        cards
            .into_iter()
            .map(|(id, hash, optional)| {
                let mut item = json!({"question_id": id, "question_hash": hash});
                if optional {
                    item["optional"] = Value::Bool(true);
                }
                item
            })
            .collect(),
    ))
}

/// The connector's question cards, after the row's own Zerus alerts (task 95:
/// a stalled lead's `team_lead_stall` item on the lead's row; task 97: an
/// `owner_deferred` request of the owner queue; task 113: a member's raised
/// `team_terminal_prompt`). Only those alerts are kept:
/// every other row item is the cards' to replace.
fn merge_attention(row: Option<&Value>, cards: &Value) -> Value {
    let mut merged: Vec<Value> = row
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            matches!(
                item.get("kind").and_then(Value::as_str),
                Some("team_lead_stall" | "owner_deferred" | "team_terminal_prompt")
            )
        })
        .cloned()
        .collect();
    merged.extend(cards.as_array().into_iter().flatten().cloned());
    Value::Array(merged)
}

#[cfg(test)]
mod attention_merge_tests {
    use super::*;

    #[test]
    fn a_team_alert_on_the_row_survives_the_question_cards() {
        let alert = json!({"question_id":"team-lead-stall:t:2","question_hash":"h","kind":"team_lead_stall"});
        let stale = json!({"question_id":"old","question_hash":"o"});
        let cards = json!([{"question_id":"q1","question_hash":"h1"}]);
        let merged = merge_attention(Some(&json!([alert.clone(), stale])), &cards);
        assert_eq!(
            merged,
            json!([alert, {"question_id":"q1","question_hash":"h1"}])
        );
        // Task 97: a deferred request of the owner queue is kept as well.
        let deferred =
            json!({"question_id":"owner-deferred:ab","question_hash":"d","kind":"owner_deferred"});
        assert_eq!(
            merge_attention(Some(&json!([deferred.clone()])), &cards),
            json!([deferred, {"question_id":"q1","question_hash":"h1"}])
        );
        // Task 113: a member's Terminal-only prompt raised on the lead's row.
        let prompt = json!({"question_id":"team-terminal-prompt:t:m:q","question_hash":"p","kind":"team_terminal_prompt"});
        assert_eq!(
            merge_attention(Some(&json!([prompt.clone()])), &cards),
            json!([prompt, {"question_id":"q1","question_hash":"h1"}])
        );
        // Without a team alert the cards replace the row's items, as before.
        assert_eq!(
            merge_attention(
                Some(&json!([{"question_id":"old","question_hash":"o"}])),
                &cards
            ),
            cards
        );
        assert_eq!(merge_attention(None, &cards), cards);
    }
}

#[cfg(test)]
mod encrypted_machine_tests {
    use super::*;

    const MACHINE: &str = "00000000-0000-4000-8000-000000000020";
    const OTHER_MACHINE: &str = "00000000-0000-4000-8000-000000000021";

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    struct NativeFixture {
        machine: RefCell<String>,
        calls: RefCell<Vec<Vec<String>>>,
        inspected: Cell<usize>,
    }
    impl Native for NativeFixture {
        async fn call(&self, call: Call) -> NativeResult {
            self.calls.borrow_mut().push(call.argv.clone());
            let argv = if call.argv.first().map(String::as_str) == Some("--expect-machine") {
                if call.argv.get(1) != Some(&*self.machine.borrow()) {
                    return Err(NativeError::Failed(crate::fleet::LOCAL_CHANGED));
                }
                &call.argv[2..]
            } else {
                &call.argv[..]
            };
            match argv
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["account", "ls"] => Ok(Output::Json(json!({"profiles": [{
                    "id": "native-codex", "provider": "codex", "label": "Fixture",
                    "installed": true, "account_status": {"status": "ok", "checked_at": 800}
                }]}))),
                ["account", "inspect", "native-codex"] => {
                    self.inspected.set(self.inspected.get() + 1);
                    Ok(Output::Json(
                        json!({"id": "native-codex", "provider": "codex",
                        "status": "ok", "checked_at": 1000}),
                    ))
                }
                _ => Err(NativeError::Failed(crate::native::FAILED)),
            }
        }
    }

    struct Fixture {
        connector: Rc<Connector<NativeFixture>>,
        _temporary: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let journal =
                Journal::open(&temporary.path().join("state"), "machine-scope", 1000.0).unwrap();
            let connector = Rc::new(Connector::new(
                NativeFixture {
                    machine: RefCell::new(MACHINE.into()),
                    calls: RefCell::default(),
                    inspected: Cell::new(0),
                },
                journal,
                Settings {
                    interval: 1.0,
                    binding: "machine-scope".into(),
                    default_timeout: 1.0,
                    clock_scale: 1.0,
                },
                Box::new(|| 1000.0),
            ));
            Self {
                connector,
                _temporary: temporary,
            }
        }
        async fn finish_refresh(&self) {
            tokio::time::timeout(Duration::from_secs(1), async {
                while self.connector.state.borrow().accounts.refreshing {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("The one queued account refresh must finish");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn label_reads_need_every_cli_marker_and_one_unambiguous_original_run() {
        use crate::session_selector::{LABELS_MARKER, PEER_MARKER, SELECTOR_MARKER};
        let f = Fixture::new();
        let name = "codex/example/labels";
        let id = "11111111-1111-4111-8111-111111111111";
        f.connector
            .state
            .borrow_mut()
            .session_ids
            .insert("run".into(), id.into());
        let payload = json!({"expected_run_id":"run"});
        let flags = |markers: &[&str]| crate::capabilities::Flags::from_help(&markers.join("\n"));
        let all = [LABELS_MARKER, SELECTOR_MARKER, PEER_MARKER];
        for (markers, expected) in [(&all[..], true), (&all[..2], false), (&all[1..], false)] {
            f.connector.flags.set(flags(markers));
            let selection = Connector::<NativeFixture>::guarded(MACHINE.into(), async {
                f.connector.labels_selection(name, Some(&payload))
            })
            .await;
            assert_eq!(selection.is_some(), expected, "{markers:?}");
        }
        f.connector.flags.set(flags(&all[..2]));
        assert_eq!(
            f.connector
                .labels_selection(name, Some(&payload))
                .unwrap()
                .id,
            id
        );
        assert!(f
            .connector
            .labels_selection(
                name,
                Some(&json!({"expected_run_id":"run","run_id":"other"}))
            )
            .is_none());
        assert!(f
            .connector
            .labels_selection(
                name,
                Some(&json!({"expected_run_id":"run","archive_id":"old"}))
            )
            .is_none());
        assert!(f
            .connector
            .labels_selection(name, Some(&json!({"expected_run_id":"unlisted"})))
            .is_none());
        let fleet = f.connector.fleet.as_ref().unwrap();
        let peer = f.connector.peer_context(
            MACHINE,
            crate::fleet::PeerNative {
                gateway: fleet.gateway.clone(),
                routes: fleet.routes.clone(),
                slots: fleet.slots.clone(),
                identity: "peer".into(),
                machine: MACHINE.into(),
                default_timeout: 1.0,
            },
        );
        peer.state
            .borrow_mut()
            .session_ids
            .insert("run".into(), id.into());
        for (gateway, own, expected) in [
            (&all[..], &all[..], true),
            (&all[1..], &all[..], false),
            (&all[..], &all[..2], false),
        ] {
            f.connector.flags.set(flags(gateway));
            peer.flags.set(flags(own));
            assert_eq!(
                peer.labels_selection(name, Some(&payload)).is_some(),
                expected
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delayed_account_refresh_keeps_its_original_encrypted_machine_binding() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let f = Fixture::new();
                Connector::<NativeFixture>::encrypted_guarded(
                    MACHINE.into(),
                    f.connector.update_accounts(),
                )
                .await;
                assert!(ENCRYPTED_LOCAL_MACHINE.try_with(Clone::clone).is_err());
                assert_eq!(f.connector.native.calls.borrow().len(), 1);
                assert!(f.connector.state.borrow().accounts.refreshing);
                // The refresh first runs after its originating scope has ended,
                // while an unrelated observation has another machine binding.
                Connector::<NativeFixture>::encrypted_guarded(
                    OTHER_MACHINE.into(),
                    f.finish_refresh(),
                )
                .await;
                assert_eq!(
                    *f.connector.native.calls.borrow(),
                    [
                        argv(&["--expect-machine", MACHINE, "account", "ls"]),
                        argv(&[
                            "--expect-machine",
                            MACHINE,
                            "account",
                            "inspect",
                            "native-codex"
                        ]),
                    ]
                );
                assert_eq!(f.connector.native.inspected.get(), 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delayed_account_refresh_refuses_a_changed_native_machine() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let f = Fixture::new();
                Connector::<NativeFixture>::encrypted_guarded(
                    MACHINE.into(),
                    f.connector.update_accounts(),
                )
                .await;
                assert_eq!(f.connector.native.calls.borrow().len(), 1);
                *f.connector.native.machine.borrow_mut() = OTHER_MACHINE.into();
                f.finish_refresh().await;
                assert_eq!(
                    f.connector.native.calls.borrow()[1],
                    [
                        "--expect-machine",
                        MACHINE,
                        "account",
                        "inspect",
                        "native-codex"
                    ]
                );
                assert_eq!(f.connector.native.inspected.get(), 0);
                let view = f.connector.state.borrow().accounts.view(1000.0);
                assert_eq!(view["accounts"][0]["usage"]["refresh_error"], true);
                assert_eq!(view["accounts"][0]["usage"]["checked_at"], 800);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_account_refresh_does_not_inject_an_encrypted_machine_binding() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let f = Fixture::new();
                f.connector.update_accounts().await;
                assert_eq!(f.connector.native.calls.borrow().len(), 1);
                Connector::<NativeFixture>::encrypted_guarded(
                    OTHER_MACHINE.into(),
                    f.finish_refresh(),
                )
                .await;
                assert_eq!(
                    *f.connector.native.calls.borrow(),
                    [
                        argv(&["account", "ls"]),
                        argv(&["account", "inspect", "native-codex"])
                    ]
                );
                assert_eq!(f.connector.native.inspected.get(), 1);
            })
            .await;
    }
}

#[cfg(test)]
mod attention_tests {
    use super::attention_cards;
    use serde_json::json;

    #[test]
    fn only_optional_codex_async_cards_carry_the_optional_marker() {
        let key = (
            "codex/example".to_owned(),
            "run".to_owned(),
            "conv".to_owned(),
        );
        let card = |id: &str, extra: serde_json::Value| {
            let mut card = json!({"run_id": "run", "conversation_id": "conv",
                "question_id": id, "question_hash": format!("h-{id}"),
                "questions": [{"question": "private"}]});
            card.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            card
        };
        let detail = json!({"run_id": "run", "conversation_id": "conv",
        "runtime_state": "live", "process_state": "running", "pending_questions": [
            card("codex-async:c:0", json!({"source": "codex_async", "optional": true})),
            card("blocking", json!({"source": "hook"})),
            card("not-codex", json!({"source": "hook", "optional": true})),
            card("loose", json!({"source": "codex_async", "optional": 1})),
            card("done", json!({"source": "codex_async", "optional": true,
                "answer_delivery": {"status": "submitted"}})),
            card("other-run", json!({"run_id": "old", "source": "codex_async",
                "optional": true})),
        ]});
        assert_eq!(
            attention_cards(&detail, &key),
            Some(json!([
                {"question_id": "blocking", "question_hash": "h-blocking"},
                {"question_id": "codex-async:c:0", "question_hash": "h-codex-async:c:0",
                 "optional": true},
                {"question_id": "loose", "question_hash": "h-loose"},
                {"question_id": "not-codex", "question_hash": "h-not-codex"},
            ]))
        );
    }
}
