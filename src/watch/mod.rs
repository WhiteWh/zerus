//! `hgs [@host] watch --json`: one long-lived NDJSON event stream per computer
//! (ABI `watch-v1`, frames in `crates/watch-protocol`). The catalog topic
//! carries the rows of `hgs ls --json --local --no-archives`; Activity carries
//! what `hgs inspect` returns for subscribed sessions, and git what `hgs
//! git-status --json` returns for subscribed checkout folders. Change detection
//! is a 250 ms pulse over cheap stamps and the journal's data version, on
//! `std::thread` only; on Windows and Linux, file notifications under the
//! state folder run the scan at once and, while they work, the stamps are
//! otherwise scanned every 2 s (`crate::state::changes`, off with
//! `HGS_CHANGE_BUS=0`). A changed binding or journal row derives only its
//! session's row again; a 5 s probe watches what no file shows and the whole
//! listing runs every 5 s. See "Event stream" in docs/reference.md.
mod activity;
mod catalog;
mod git;
mod processes;
pub(crate) mod stamps;
mod organization;
mod tasks;
mod team;
mod writer;

use crate::cli::{Error, Result};
use crate::config::Config;
use crate::state::changes;
use crate::state::watch_source as source;
use activity::Subscription;
use catalog::{Digest32, Sent};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use writer::{Item, Writer};
use zerus_watch_protocol::{self as protocol, reason, topic, Body, Control};

/// How often sources are checked.
const PULSE: Duration = Duration::from_millis(250);
/// Task #11: a change bus hint runs the pulse at once, but at most this
/// soon after the previous one, so files that keep changing (the journal of
/// a working session) cannot run it more often than ten times a second.
const HINT_GAP: Duration = Duration::from_millis(100);
/// Task #12 (P2.1b): while the change bus is healthy, the stamps and the
/// journal are scanned on its hints and otherwise this often (the safety
/// scan of design 4.1); the 250 ms pulse keeps the subscriptions' own timers
/// and the sources outside the state folder (git, processes, tasks, ...).
const SAFETY_SCAN: Duration = Duration::from_secs(2);
/// After an overflow the stamps are scanned on every pulse this long.
const OVERFLOW_HOLD: Duration = Duration::from_secs(300);
/// A listing or a row refresh waits this long after a change, so a burst
/// derives once.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Listings, and row refreshes, start at least this far apart, and at least
/// four times the previous one's duration apart.
const LISTING_GAP: Duration = Duration::from_millis(500);
/// What no file shows is probed this often: a session that starts, ends, is
/// killed, renamed, attached or detached, an exited agent process, ended
/// background work, a session that has not reported a hook event yet
/// (startup choosers) and DeepSeek and OpenCode hosts.
const PROBE: Duration = Duration::from_secs(5);
/// How often the stream looks for a replaced executable; a replacement must
/// read the same twice in a row, so the stream ends 5 to 10 s after it.
const REPLACEMENT_CHECK: Duration = PROBE;
/// How long an ending stream waits for its last frames to be written.
const END_DRAIN: Duration = Duration::from_secs(2);
/// The whole listing runs at least this often, for what neither a file nor
/// the probe shows: terminal choosers and status lines of sessions that have
/// reported hooks, and native transcripts between hook events. The owner
/// chose 5 s (2026-10-27, zerus-604d) and, for task #13 (P2.2, 2026-11-23),
/// to try 60 s with rows derived on change and at their time deadlines:
/// `HGS_WATCH_FULL_SECONDS` sets it, 5 restores the old behaviour.
const FULL_DEFAULT: Duration = Duration::from_secs(60);
/// Drift lines a sweep writes at most.
const DRIFT_LINES: usize = 20;

/// `HGS_WATCH_FULL_SECONDS`: whole seconds from 1; anything else is the default.
fn full_interval(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds >= 1)
        .map_or(FULL_DEFAULT, Duration::from_secs)
}

/// Task #13: how the whole listing is read; tests use a fake.
type ListFn = fn(&Config) -> std::result::Result<Value, String>;
/// Task #13: the terminal's structure when a listing starts; tests use a fake.
type StructureFn = fn() -> Option<u64>;
/// Task #13: where a drift line goes; tests count instead of logging.
type DriftFn = fn(&Value);

fn list_local(config: &Config) -> std::result::Result<Value, String> {
    crate::cli::local_listing(config).map_err(|error| error.message)
}

fn structure_now() -> Option<u64> {
    source::structure_now().ok()
}

fn log_drift(line: &Value) {
    let _ = crate::state::slow_calls::append(&crate::state::slow_calls::log_path(), &line.to_string());
}

/// Task #13 (P2.2): rows a sweep listed otherwise than the loop kept them,
/// with the top-level fields that differ (volatile fields aside). Rows in
/// `skip` were changing while the listing ran, and only keys both hold count:
/// an added or removed row is a structure change, which lists anyway.
fn drift(kept: &[(String, Value)], listed: &[(String, Value)], skip: &BTreeSet<String>) -> Vec<(String, Vec<String>)> {
    let kept: BTreeMap<&str, &Value> = kept.iter().map(|(key, row)| (key.as_str(), row)).collect();
    let mut found = Vec::new();
    for (key, row) in listed {
        let Some(old) = kept.get(key.as_str()) else { continue };
        if row["name"].as_str().is_some_and(|name| skip.contains(name)) || catalog::row_digest(old) == catalog::row_digest(row) {
            continue;
        }
        let (old, new) = (catalog::masked(old, &catalog::VOLATILE_ROW), catalog::masked(row, &catalog::VOLATILE_ROW));
        let names: BTreeSet<&String> = old.as_object().into_iter().chain(new.as_object()).flat_map(|fields| fields.keys()).collect();
        let fields = names.into_iter().filter(|field| old.get(field.as_str()) != new.get(field.as_str())).cloned().collect();
        found.push((key.clone(), fields));
    }
    found
}
/// `catalog.meta` with only fresh metrics or presence goes out this rarely.
const META_INTERVAL: Duration = Duration::from_secs(10);
/// A changed session's summary is re-inspected at most this often.
const STATE_GAP: Duration = Duration::from_millis(500);
/// Unchanged summaries are re-inspected after this long: provider messages
/// between tools exist only in native transcripts.
const BUSY_REFRESH: Duration = Duration::from_secs(2);
const IDLE_REFRESH: Duration = Duration::from_secs(15);
/// DeepSeek and OpenCode answer from their own hosts.
const NATIVE_REFRESH: Duration = Duration::from_secs(2);
/// How often a subscription without a followed transcript looks for one.
const TRANSCRIPT_RETRY: Duration = Duration::from_secs(2);
/// With more than this queued, catalog changes wait and coalesce.
const DEFER_BYTES: usize = protocol::QUEUE_BYTES / 4;
/// A repeated source failure is reported at most this often.
const ERROR_REPEAT: Duration = Duration::from_secs(60);
/// The largest page of catalog items.
const PAGE_LIMIT: usize = protocol::PAGE_BYTES - 4096;

enum Input {
    Line(String),
    TooLong,
    /// Control lines arrived faster than they were handled; some were dropped.
    Dropped,
    /// A git status check finished on a worker.
    Git(git::Done),
    /// A process inventory read finished on a worker.
    Processes(processes::Done),
    /// A task list read finished on a worker.
    Tasks(tasks::Done),
    /// An organization read finished on its worker.
    Organization(organization::Done),
    /// A read of the team heads finished on its worker.
    Team(team::Done),
    /// Task 235: a whole listing finished on its own thread.
    Listing(ListingDone),
    /// The DeepSeek and OpenCode rows were read from their hosts on a thread.
    NativeRows(NativeDone),
    /// Rows of live tracked sessions were derived again on a thread.
    Rows(RowsDone),
    /// A probe of what no file shows finished on a thread.
    Probe(ProbeDone),
    DriveFolders(organization::Done),
    /// P1.5: a subscription's summary was inspected again on a thread.
    Inspected(InspectDone),
    /// Task #11 (P2.1a): the change bus noticed files under the state folder.
    Changed(changes::Note),
    /// Task #14 (P2.4): the terminal's change counters moved; None when it
    /// has none (tmux, a session host that predates them).
    Counters(Option<crate::state::backend::Counters>),
    End,
}

/// Task #14 (P2.4): how often the counters thread asks the session host's
/// `list` (one pipe round trip, no process sample).
const COUNTERS_POLL: Duration = Duration::from_millis(100);
/// A pane whose output counter moved is derived again once it stood still
/// this long, so a chooser drawn in pieces derives once...
const SETTLE: Duration = Duration::from_millis(300);
/// ...and at most this often while it keeps moving (a busy screen).
const BUSY_EVERY: Duration = Duration::from_secs(2);
/// Without counters (tmux, an older session host) the sweep stays at the
/// earlier 5 s, for choosers and status lines no file shows.
const LEGACY_FULL: Duration = Duration::from_secs(5);

/// Task #14: what the terminal's counters say so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Counted {
    /// Not answered yet.
    Unknown,
    /// The terminal has none: the 5 s sweep stays.
    Unsupported,
    /// The last structure counter.
    Live(u64),
}

/// Task #14: one pane's output counter and its settling.
#[derive(Debug)]
struct Screen {
    count: u64,
    /// The last change seen that no derivation covered yet.
    changed: Option<Instant>,
    /// Since when it has kept moving without a derivation.
    moving_since: Option<Instant>,
}

/// Task #11: how changed rows are derived; tests use a fake.
type DeriveFn = fn(&BTreeSet<String>) -> std::result::Result<Option<(Vec<Value>, u64)>, String>;

/// What a summary refresh reads: the inspection and, for a journal session,
/// its timeline (journal name and conversation).
type Inspection = std::result::Result<(Value, Option<source::Timeline>), String>;

/// P1.5: how a summary refresh reads a session; tests use a slow fake.
type InspectFn = fn(name: &str, after: i64, native: bool) -> Inspection;

fn inspect_session(name: &str, after: i64, native: bool) -> Inspection {
    let inspection = source::inspect(name, after).map_err(|error| error.to_string())?;
    let timeline = if native { None } else { source::timeline(name).ok().flatten() };
    Ok((inspection, timeline))
}

/// P1.5: a summary refresh that ran on its own thread. It applies only to
/// the subscription that asked for it (`key` and `token`) under the name it
/// was read for; a result for an ended, replaced or restarted subscription
/// is dropped.
struct InspectDone {
    key: String,
    token: u64,
    name: String,
    result: Inspection,
}

/// Rows derived again between listings. Deriving them waits for the session
/// state lock and reads each session, which takes seconds on a busy computer:
/// on a thread, the loop keeps answering meanwhile.
struct RowsDone {
    started: Instant,
    wanted: BTreeSet<String>,
    result: std::result::Result<Option<(Vec<Value>, u64)>, String>,
}

/// A probe's findings, with the listing's metrics and presence as they are now.
struct ProbeDone {
    started: Instant,
    result: std::result::Result<source::Probe, String>,
    metrics: Value,
    presence: Option<Value>,
}

/// The DeepSeek and OpenCode rows between listings. Their hosts answer them,
/// and a host that is slow or stuck answers only at its request limit (35 s):
/// read on the loop, that kept every subscription, control and Activity frame
/// of the stream waiting, so a session opened meanwhile showed no Activity.
struct NativeDone {
    started: Instant,
    result: std::result::Result<Vec<Value>, String>,
}

/// What became of a subscription's first frames.
enum Started {
    Sent,
    /// The inspection failed for now (a busy state lock, say): the
    /// subscription stays and its first frames are read again.
    Later,
    /// The session cannot be followed as asked.
    Refused,
}

/// The first wait before a subscription's first frames are read again; it
/// doubles up to [`START_RETRY_MAX`].
const START_RETRY: Duration = Duration::from_secs(1);
const START_RETRY_MAX: Duration = Duration::from_secs(8);

/// Task 235: a whole listing, built off the loop so Activity, controls and
/// pulses keep flowing while it runs (it took seconds every ~28 s).
struct ListingDone {
    started: Instant,
    result: std::result::Result<Value, String>,
}

/// Task 235: at most one listing runs; one asked for meanwhile runs after it.
#[derive(Default)]
struct ListingGate {
    running: bool,
    again: bool,
}

impl ListingGate {
    /// True when a listing should start now; otherwise it is remembered.
    fn start(&mut self) -> bool {
        if self.running {
            self.again = true;
            return false;
        }
        self.running = true;
        true
    }

    /// The running listing ended; true when another was asked for meanwhile.
    fn done(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.again)
    }
}

/// Control lines read but not yet handled; beyond it lines are dropped, so a
/// client that only writes cannot grow this process.
const PENDING_CONTROLS: usize = 256;

pub(crate) fn run(config: &Config, args: &[String], dry: bool) -> Result<i32> {
    if dry || args != ["--json"] {
        return Err(Error::new(1, "usage: hgs [@host] watch --json   (NDJSON frames on stdout, control operations on stdin)"));
    }
    let (inputs, received) = mpsc::channel();
    let closed = inputs.clone();
    let checks = inputs.clone();
    let writer = Writer::spawn(
        std::io::stdout(),
        protocol::QUEUE_BYTES,
        Duration::from_millis(protocol::HEARTBEAT_MS),
        move || {
            let _ = closed.send(Input::End);
        },
    );
    let pending = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&pending);
    let halt = writer.clone();
    std::thread::Builder::new()
        .name("hgs-watch-control".into())
        // EOF stops the writer at once, so a wait for queue room ends too.
        .spawn(move || read_controls(std::io::stdin().lock(), inputs, &counted, move || halt.halt()))
        .map_err(|error| Error::new(1, format!("cannot read control operations: {error}")))?;
    let mut watch = Watch::new(config, writer, checks);
    watch.hello();
    watch.list(Instant::now());
    watch.emit_catalog();
    watch.start_bus();
    watch.start_counters();
    loop {
        let input = received.recv_timeout(watch.deadline().saturating_duration_since(Instant::now()));
        if matches!(input, Ok(Input::Line(_) | Input::TooLong)) {
            pending.fetch_sub(1, Ordering::SeqCst);
        }
        if !watch.turn(input) {
            return Ok(0);
        }
    }
}

/// Reads control lines until EOF, then runs `ended`. A line longer than the
/// limit is skipped and reported, never buffered whole; so are lines beyond
/// `PENDING_CONTROLS` not yet handled.
fn read_controls<R: BufRead>(mut input: R, inputs: mpsc::Sender<Input>, pending: &AtomicUsize, ended: impl FnOnce()) {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut dropping = false;
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if buffer.is_empty() {
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let chunk = &buffer[..newline.unwrap_or(buffer.len())];
        if !too_long && line.len() + chunk.len() > protocol::CONTROL_LINE_BYTES {
            too_long = true;
            line.clear();
        }
        if !too_long {
            line.extend_from_slice(chunk);
        }
        let used = chunk.len() + usize::from(newline.is_some());
        input.consume(used);
        if newline.is_some() {
            let message = if too_long {
                Input::TooLong
            } else {
                Input::Line(String::from_utf8_lossy(&line).into_owned())
            };
            line.clear();
            too_long = false;
            if pending.load(Ordering::SeqCst) >= PENDING_CONTROLS {
                if !dropping && inputs.send(Input::Dropped).is_err() {
                    break;
                }
                dropping = true;
                continue;
            }
            dropping = false;
            pending.fetch_add(1, Ordering::SeqCst);
            if inputs.send(message).is_err() {
                break;
            }
        }
    }
    if !line.is_empty() && !too_long && pending.load(Ordering::SeqCst) < PENDING_CONTROLS {
        pending.fetch_add(1, Ordering::SeqCst);
        let _ = inputs.send(Input::Line(String::from_utf8_lossy(&line).into_owned()));
    }
    ended();
    let _ = inputs.send(Input::End);
}

/// Sessions whose new journal rows record a finished file-changing tool.
#[derive(Default)]
enum Writers {
    #[default]
    None,
    /// Rows of these journal names, with the latest one's time.
    Names(Vec<(String, Option<f64>)>),
    /// Too many rows to tell.
    All,
}

/// What changed in the Activity journal since the previous pulse.
enum Journal {
    Unchanged,
    /// Rows of these journal names were added.
    Names(BTreeSet<String>),
    /// Rows were added or the journal was replaced, for names not known.
    Unknown,
}

/// Frames out, with repeated source failures limited.
struct Out {
    writer: Writer,
    reported: BTreeMap<String, Instant>,
}

impl Out {
    fn encode(body: &Body) -> Option<String> {
        protocol::encode_body(body).ok()
    }

    /// Queues a frame unless the client fell behind (false).
    fn offer(&self, body: &Body, coalesce: Option<String>) -> bool {
        match Self::encode(body) {
            Some(body) => self.writer.offer(Item { coalesce, body }),
            None => false,
        }
    }

    /// Queues a frame, waiting for room.
    fn put(&self, body: &Body) -> bool {
        match Self::encode(body) {
            Some(body) => self.writer.put(Item { coalesce: None, body }),
            None => false,
        }
    }

    /// Writes the stream's last frame within `limit`, or halts the stream.
    fn finish(&self, body: &Body, limit: Duration) -> bool {
        match Self::encode(body) {
            Some(body) => self.writer.finish(Item { coalesce: None, body }, limit),
            None => {
                self.writer.halt();
                false
            }
        }
    }

    fn error(&mut self, code: &str, message: &str, topic: Option<&str>, key: Option<&str>) {
        let body = Body::Error(protocol::ErrorFrame {
            code: code.into(),
            message: message.chars().take(2048).collect(),
            topic: topic.map(str::to_owned),
            key: key.map(str::to_owned),
            fatal: false,
        });
        self.put(&body);
    }

    /// An error of a source that is retried: once per minute per message.
    fn failure(&mut self, code: &str, message: &str, topic: Option<&str>, key: Option<&str>) {
        let id = format!("{code}\n{}\n{message}", key.unwrap_or(""));
        let now = Instant::now();
        if self.reported.get(&id).is_some_and(|at| now.duration_since(*at) < ERROR_REPEAT) {
            return;
        }
        self.reported.retain(|_, at| now.duration_since(*at) < ERROR_REPEAT);
        self.reported.insert(id, now);
        self.error(code, message, topic, key);
    }
}

struct Watch<'a> {
    config: &'a Config,
    out: Out,
    root: PathBuf,
    // The latest listing, and what the client holds of it.
    rows: Vec<(String, Value)>,
    digests: Vec<Digest32>,
    meta: Value,
    listed: bool,
    pending: bool,
    sent: BTreeMap<String, Sent>,
    revision: i64,
    snapshot_sent: bool,
    catalog_resync: bool,
    meta_sent: Option<(Digest32, Digest32, Instant)>,
    // Listing schedule.
    scan: stamps::Scan,
    /// Each binding file's session and journal names (None: no binding).
    identities: BTreeMap<PathBuf, Option<(String, String)>>,
    /// The terminal's structure when the rows were derived.
    structure: Option<u64>,
    listing_due: Option<Instant>,
    listed_at: Option<Instant>,
    listing_cost: Duration,
    /// Task 235: the listing thread's gate.
    listing: ListingGate,
    // Row refreshes between listings.
    stale: BTreeSet<String>,
    /// Sessions whose rows were derived again while a listing ran.
    relisted: BTreeSet<String>,
    refresh_due: Option<Instant>,
    refreshed_at: Option<Instant>,
    refresh_cost: Duration,
    // The DeepSeek and OpenCode rows between listings, read on a thread.
    native_due: Option<Instant>,
    native_at: Option<Instant>,
    native_cost: Duration,
    native_running: bool,
    /// One derivation and one probe run at a time, each on its own thread.
    rows_running: bool,
    probe_running: bool,
    probed_at: Option<Instant>,
    // Activity.
    journal: Option<rusqlite::Connection>,
    data_version: Option<i64>,
    journal_max: i64,
    subs: Vec<Subscription>,
    /// P1.5: summary refreshes run on threads through this; each gets a new
    /// token from `inspect_tokens`.
    inspect: InspectFn,
    inspect_tokens: u64,
    // Git.
    git: git::Git,
    /// Started with the first check; its results return as `Input::Git`.
    git_pool: Option<git::Pool>,
    git_results: mpsc::Sender<Input>,
    processes: processes::Processes,
    processes_pool: Option<processes::Pool>,
    tasks: tasks::Tasks,
    tasks_pool: Option<tasks::Pool>,
    organization: organization::Organization,
    organization_pool: Option<organization::Pool>,
    team: team::Team,
    team_pool: Option<team::Pool>,
    drive_folders: organization::Organization,
    drive_folders_pool: Option<organization::Pool>,
    writers: Writers,
    /// The file this stream started from (zerus-5wmh.66.2.46), the last
    /// replacement seen at its path, and when to look again.
    executable: Option<crate::swarm::Executable>,
    replacement: Option<crate::swarm::Replacement>,
    replacement_due: Instant,
    /// When the next pulse runs: every [`PULSE`], and at once on a hint.
    pulse_due: Instant,
    pulsed_at: Option<Instant>,
    /// Task #12: when a pulse next scans the stamps and the journal: every
    /// pulse without a healthy bus, else on a hint or after [`SAFETY_SCAN`].
    scan_due: Instant,
    overflowed_at: Option<Instant>,
    /// Task #11 (P2.1a): file notifications under the state folder; None
    /// when switched off (`HGS_CHANGE_BUS=0`), refused or failed.
    bus: Option<changes::Bus>,
    /// The bus asked for this pulse: it debounced the burst already.
    hinted: bool,
    /// A bus fault is logged once per stream.
    bus_reported: bool,
    derive: DeriveFn,
    /// Task #13 (P2.2): the whole listing's interval (`HGS_WATCH_FULL_SECONDS`).
    full: Duration,
    /// A change asked for the next listing; without one it is a sweep, whose
    /// rows are compared with the kept ones (the drift check).
    asked: bool,
    /// The running listing is a sweep, and the names that were changing when
    /// it started (pending or in flight), which the drift check skips.
    sweeping: bool,
    drift_skip: BTreeSet<String>,
    /// Names a row derivation in flight is deriving.
    deriving: BTreeSet<String>,
    /// Each row's next time-driven change (`session_state::valid_until`), by
    /// key, in wall-clock seconds, and when the earliest is due.
    deadlines: BTreeMap<String, f64>,
    time_due: Option<Instant>,
    lister: ListFn,
    structure_of: StructureFn,
    drift_log: DriftFn,
    /// Task #14 (P2.4): `HGS_WATCH_FULL_SECONDS` was given, so it holds even
    /// without counters.
    full_explicit: bool,
    counted: Counted,
    screens: BTreeMap<String, Screen>,
    /// When the next screen settles or is due as busy.
    screens_due: Option<Instant>,
}

impl<'a> Watch<'a> {
    fn new(config: &'a Config, writer: Writer, git_results: mpsc::Sender<Input>) -> Self {
        Self::with_root(config, writer, git_results, source::state_root())
    }

    fn with_root(config: &'a Config, writer: Writer, git_results: mpsc::Sender<Input>, root: PathBuf) -> Self {
        // Stamps are taken before the first listing: a change during it lists again.
        let scan = stamps::scan(&root);
        let identities = scan.bindings.keys().map(|path| (path.clone(), source::binding_identity(path))).collect();
        Self {
            config,
            out: Out { writer, reported: BTreeMap::new() },
            root,
            rows: Vec::new(),
            digests: Vec::new(),
            meta: json!({}),
            listed: false,
            pending: false,
            sent: BTreeMap::new(),
            revision: 0,
            snapshot_sent: false,
            catalog_resync: false,
            meta_sent: None,
            scan,
            identities,
            structure: None,
            listing_due: None,
            listed_at: None,
            listing_cost: Duration::ZERO,
            listing: ListingGate::default(),
            stale: BTreeSet::new(),
            relisted: BTreeSet::new(),
            refresh_due: None,
            refreshed_at: None,
            refresh_cost: Duration::ZERO,
            native_due: None,
            native_at: None,
            native_cost: Duration::ZERO,
            native_running: false,
            rows_running: false,
            probe_running: false,
            probed_at: None,
            journal: None,
            data_version: None,
            journal_max: 0,
            subs: Vec::new(),
            inspect: inspect_session,
            inspect_tokens: 0,
            git: git::Git::new(),
            git_pool: None,
            git_results,
            processes: processes::Processes::new(),
            processes_pool: None,
            tasks: tasks::Tasks::new(),
            tasks_pool: None,
            organization: organization::Organization::new(),
            organization_pool: None,
            team: team::Team::new(),
            team_pool: None,
            drive_folders: organization::Organization::new_drive(),
            drive_folders_pool: None,
            writers: Writers::None,
            executable: crate::swarm::Executable::current(),
            replacement: None,
            replacement_due: Instant::now() + REPLACEMENT_CHECK,
            pulse_due: Instant::now() + PULSE,
            pulsed_at: None,
            scan_due: Instant::now() + PULSE,
            overflowed_at: None,
            bus: None,
            hinted: false,
            bus_reported: false,
            derive: source::rows,
            full: full_interval(std::env::var("HGS_WATCH_FULL_SECONDS").ok().as_deref()),
            asked: false,
            sweeping: false,
            drift_skip: BTreeSet::new(),
            deriving: BTreeSet::new(),
            deadlines: BTreeMap::new(),
            time_due: None,
            lister: list_local,
            structure_of: structure_now,
            drift_log: log_drift,
            full_explicit: std::env::var_os("HGS_WATCH_FULL_SECONDS").is_some(),
            counted: Counted::Unknown,
            screens: BTreeMap::new(),
            screens_due: None,
        }
    }

    /// When the loop must wake without input.
    fn deadline(&self) -> Instant {
        [self.listing_due, self.refresh_due, self.native_due, self.time_due, self.screens_due, Some(self.replacement_due)]
            .into_iter()
            .flatten()
            .fold(self.pulse_due, Instant::min)
    }

    /// One pass of the loop for `input`, or for a deadline when it timed
    /// out. False when the stream ends.
    fn turn(&mut self, input: std::result::Result<Input, mpsc::RecvTimeoutError>) -> bool {
        if self.out.writer.stopped() {
            return false;
        }
        // Everything below runs on the loop, and every subscription, control
        // and Activity frame waits for it: a pass that takes a second or more
        // leaves a `watch-loop` line naming its steps in the slow-calls log.
        let mut phases = crate::state::slow_calls::Phases::start();
        match input {
            Ok(Input::Line(text)) => self.control(&text),
            Ok(Input::TooLong) => self.out.error(
                "bad_request",
                &format!("control operations are at most {} bytes", protocol::CONTROL_LINE_BYTES),
                None,
                None,
            ),
            Ok(Input::Dropped) => self.out.error(
                "bad_request",
                &format!("more than {PENDING_CONTROLS} control operations were waiting; later ones were dropped"),
                None,
                None,
            ),
            Ok(Input::Git(done)) => self.git_done(done),
            Ok(Input::Processes(done)) => self.processes_done(done),
            Ok(Input::Tasks(done)) => self.tasks_done(done),
            Ok(Input::Organization(done)) => self.organization_done(done),
            Ok(Input::Team(done)) => self.team_done(done),
            Ok(Input::Listing(done)) => self.listing_done(done),
            Ok(Input::NativeRows(done)) => self.native_done(done),
            Ok(Input::Rows(done)) => self.rows_done(done),
            Ok(Input::Probe(done)) => self.probe_done(done),
            Ok(Input::DriveFolders(done)) => self.drive_folders_done(done),
            Ok(Input::Inspected(done)) => self.inspected(done),
            Ok(Input::Changed(note)) => self.changed(note, Instant::now()),
            Ok(Input::Counters(counters)) => self.counted(counters, Instant::now()),
            Ok(Input::End) | Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        phases.mark("input");
        if self.out.writer.stopped() {
            return false;
        }
        let now = Instant::now();
        if now >= self.pulse_due {
            self.pulse_due = now + PULSE;
            self.pulsed_at = Some(now);
            let scan = now >= self.scan_due;
            if scan {
                self.scan_due = now + self.scan_interval(now);
            }
            self.pulse(now, scan, &mut phases);
        }
        if self.time_due.is_some_and(|due| now >= due) {
            self.timed(now);
            phases.mark("timed");
        }
        if self.screens_due.is_some_and(|due| now >= due) {
            self.settle(now);
            phases.mark("screens");
        }
        if self.listing_due.is_some_and(|due| now >= due) {
            self.list(now);
            phases.mark("list");
        }
        if self.refresh_due.is_some_and(|due| now >= due) {
            self.refresh_rows(now);
            phases.mark("rows");
        }
        if self.native_due.is_some_and(|due| now >= due) {
            self.refresh_native(now);
            phases.mark("native");
        }
        if now >= self.replacement_due && self.replaced(now) {
            self.end_replaced();
            return false;
        }
        phases.mark("replacement");
        self.emit_catalog();
        phases.mark("catalog");
        self.resync_activity();
        phases.mark("resync");
        self.flush_git();
        self.flush_processes();
        self.flush_tasks();
        self.flush_organization();
        self.flush_team();
        self.flush_drive_folders();
        phases.mark("flush");
        phases.finish("watch-loop", "");
        true
    }

    // ---- Change bus (task #11, P2.1a) --------------------------------------

    /// Starts the file notifications on the state folder. A hint brings the
    /// next scan forward; the stamps stay the truth. While the bus is healthy
    /// the stamps are scanned every [`SAFETY_SCAN`] without a hint; a
    /// refused or failed watcher keeps the scan on every 250 ms pulse.
    /// macOS keeps the pulse (owner, 2026-11-23).
    fn start_bus(&mut self) {
        if !changes::enabled() || !changes::supported() {
            return;
        }
        let results = self.git_results.clone();
        match changes::Bus::start(&self.root, stamps::relevant, move |note| results.send(Input::Changed(note)).is_ok()) {
            Ok(bus) => self.bus = Some(bus),
            Err(error) => self.bus_fault(&error),
        }
    }

    /// A hint runs the pulse now (at most every [`HINT_GAP`]), without the
    /// pulse's own debounce: the bus waited for the burst to end. An
    /// overflow is a hint that names every input, which a pulse scans anyway.
    fn changed(&mut self, note: changes::Note, now: Instant) {
        match note {
            changes::Note::Hint(hint) => {
                if hint.overflow {
                    self.overflowed_at = Some(now);
                    self.bus_fault("notifications overflowed; the stamps were scanned whole");
                }
                self.hinted = true;
                let at = self.pulsed_at.map_or(now, |last| now.max(last + HINT_GAP));
                self.pulse_due = self.pulse_due.min(at);
                self.scan_due = self.scan_due.min(at);
            }
            changes::Note::Failed(error) => {
                self.bus = None;
                self.bus_fault(&error);
            }
        }
    }

    /// How long until the next scan without a hint: [`SAFETY_SCAN`] while
    /// the bus runs and has not overflowed for [`OVERFLOW_HOLD`], else the pulse.
    fn scan_interval(&self, now: Instant) -> Duration {
        let healthy = self.bus.is_some() && self.overflowed_at.is_none_or(|at| now >= at + OVERFLOW_HOLD);
        if healthy {
            SAFETY_SCAN
        } else {
            PULSE
        }
    }

    /// One `change-bus` line in the slow-calls log per stream: the pulse
    /// covers what the bus misses, so a fault is worth knowing, not repeating.
    fn bus_fault(&mut self, error: &str) {
        if std::mem::replace(&mut self.bus_reported, true) {
            return;
        }
        let line = json!({"at":crate::state::now(),"command":"change-bus","subject":error.chars().take(200).collect::<String>(),
            "pid":std::process::id()});
        let _ = crate::state::slow_calls::append(&crate::state::slow_calls::log_path(), &line.to_string());
    }

    /// An update put another ready executable at this stream's path, and
    /// the next look found the same file: a file still being written, a
    /// missing path or one not yet executable never ends the stream.
    fn replaced(&mut self, now: Instant) -> bool {
        self.replacement_due = now + REPLACEMENT_CHECK;
        let seen = self.executable.as_ref().and_then(crate::swarm::Executable::replacement);
        let settled = seen.is_some() && seen == self.replacement;
        self.replacement = seen;
        settled
    }

    /// Ends the stream so that its client starts the new executable: a
    /// stream left running would keep the old code's summaries beside the
    /// new CLI's commands. Clients keep their rows across the restart.
    /// One deadline covers the frame's room in the queue and its write; past
    /// it the stream halts and the process ends, closing stdout (EOF).
    fn end_replaced(&mut self) {
        self.out.finish(
            &Body::Error(protocol::ErrorFrame {
                code: protocol::code::HGS_REPLACED.into(),
                message: "the hgs executable was replaced; start the stream again to run the new version".into(),
                topic: None,
                key: None,
                fatal: true,
            }),
            END_DRAIN,
        );
    }

    fn hello(&mut self) {
        let hello = protocol::Hello {
            machine_id: crate::swarm::local_node_id(self.config).ok(),
            host: self.config.host().into(),
            hgs_version: env!("CARGO_PKG_VERSION").into(),
            stream: uuid::Uuid::new_v4().to_string(),
            heartbeat_ms: protocol::HEARTBEAT_MS,
            max_frame_bytes: protocol::MAX_FRAME_BYTES,
            topics: [topic::CATALOG, topic::CATALOG_META, topic::ACTIVITY, topic::GIT, topic::PROCESSES, topic::TASKS, topic::ORGANIZATION, topic::TEAM, topic::DRIVE_FOLDERS]
                .map(str::to_owned)
                .to_vec(),
            limits: protocol::Limits::default(),
        };
        self.out.put(&Body::Hello(hello));
        self.open_journal();
    }

    // ---- Catalog -----------------------------------------------------------

    /// A change asks for a whole listing.
    fn schedule(&mut self, at: Instant) {
        self.asked = true;
        self.schedule_listing(at);
    }

    /// Task #13: the periodic sweep; unless a change asks for the same
    /// listing, its rows are compared with the kept ones.
    fn schedule_sweep(&mut self, at: Instant) {
        self.schedule_listing(at);
    }

    fn schedule_listing(&mut self, at: Instant) {
        let gap = LISTING_GAP.max(self.listing_cost * 4);
        let due = self.listed_at.map_or(at, |started| at.max(started + gap));
        self.listing_due = Some(self.listing_due.map_or(due, |current| current.min(due)));
    }

    fn schedule_refresh(&mut self, at: Instant) {
        let gap = LISTING_GAP.max(self.refresh_cost * 4);
        let due = self.refreshed_at.map_or(at, |started| at.max(started + gap));
        self.refresh_due = Some(self.refresh_due.map_or(due, |current| current.min(due)));
    }

    fn schedule_native(&mut self, at: Instant) {
        let gap = LISTING_GAP.max(self.native_cost * 4);
        let due = self.native_at.map_or(at, |started| at.max(started + gap));
        self.native_due = Some(self.native_due.map_or(due, |current| current.min(due)));
    }

    /// Re-runs the `ls --json --local --no-archives` builder on its own
    /// thread (task 235): the loop keeps serving Activity and controls; the
    /// rows arrive as [`Input::Listing`]. One asked for while one runs
    /// follows it.
    fn list(&mut self, now: Instant) {
        self.listing_due = None;
        if !self.listing.start() {
            return;
        }
        self.listed_at = Some(now);
        // The listing covers every row refresh and probe asked for so far.
        self.probed_at = Some(now);
        self.sweeping = !std::mem::take(&mut self.asked);
        self.drift_skip = std::mem::take(&mut self.stale);
        self.drift_skip.extend(self.deriving.iter().cloned());
        self.refresh_due = None;
        self.native_due = None;
        // The structure is taken first: a change during the listing lists again.
        self.structure = (self.structure_of)();
        let config = self.config.clone();
        let results = self.git_results.clone();
        let lister = self.lister;
        let spawned = std::thread::Builder::new().name("hgs-watch-listing".into()).spawn(move || {
            let result = lister(&config);
            let _ = results.send(Input::Listing(ListingDone { started: now, result }));
        });
        if let Err(error) = spawned {
            self.listing_done(ListingDone { started: now, result: Err(format!("cannot start the listing: {error}")) });
        }
    }

    /// A listing's rows replace what the loop holds.
    fn listing_done(&mut self, done: ListingDone) {
        match done.result {
            Ok(listing) => {
                let (mut rows, meta) = catalog::split(listing);
                // A listing reads each session once, near its start, and a
                // slow one lands much later: a row derived meanwhile is newer
                // than the listing's and stays, so an answered approval or a
                // finished turn does not come back. It is derived once more.
                let again = std::mem::take(&mut self.relisted);
                if std::mem::take(&mut self.sweeping) && self.listed {
                    self.check_drift(&rows, &again);
                }
                if !again.is_empty() {
                    let newer: BTreeMap<&str, &Value> = self
                        .rows
                        .iter()
                        .filter(|(_, row)| row["name"].as_str().is_some_and(|name| again.contains(name)))
                        .map(|(key, row)| (key.as_str(), row))
                        .collect();
                    for (key, row) in &mut rows {
                        if let Some(kept) = newer.get(key.as_str()).filter(|kept| kept["name"] == row["name"]) {
                            *row = (*kept).clone();
                        }
                    }
                    self.stale.extend(again);
                    self.schedule_refresh(Instant::now());
                }
                self.digests = rows.iter().map(|(_, row)| catalog::row_digest(row)).collect();
                self.rows = rows;
                self.meta = meta;
                self.listed = true;
                self.pending = true;
                self.deadlines.clear();
                let keys: Vec<String> = self.rows.iter().map(|(key, _)| key.clone()).collect();
                self.note_deadlines(keys);
            }
            Err(error) => {
                self.sweeping = false;
                self.relisted.clear();
                self.out.failure("listing_failed", &error, Some(topic::CATALOG), None);
            }
        }
        self.listing_cost = done.started.elapsed();
        if self.listing.done() {
            self.schedule(Instant::now());
        }
    }

    /// Derives again, on a thread, the rows whose inputs changed since they
    /// were listed; they arrive as [`Input::Rows`]. One derivation runs at a
    /// time: names that change meanwhile wait for the next.
    fn refresh_rows(&mut self, now: Instant) {
        self.refresh_due = None;
        if self.rows_running {
            return;
        }
        self.refreshed_at = Some(now);
        let wanted = std::mem::take(&mut self.stale);
        if wanted.is_empty() {
            return;
        }
        if !self.listed {
            self.schedule(now);
            return;
        }
        if self.listing.running {
            self.relisted.extend(wanted.iter().cloned());
        }
        self.rows_running = true;
        self.deriving = wanted.clone();
        let results = self.git_results.clone();
        let names = wanted.clone();
        let derive = self.derive;
        let spawned = std::thread::Builder::new().name("hgs-watch-rows".into()).spawn(move || {
            let result = derive(&names);
            let _ = results.send(Input::Rows(RowsDone { started: now, wanted: names, result }));
        });
        if let Err(error) = spawned {
            self.rows_done(RowsDone { started: now, wanted, result: Err(format!("cannot derive the rows: {error}")) });
        }
    }

    /// Replaces the rows of the live tracked sessions that were derived
    /// again. A whole listing follows when only it can tell: the terminal
    /// changed, a session is no longer live under its binding, or a row would
    /// change its key or its place.
    fn rows_done(&mut self, done: RowsDone) {
        self.rows_running = false;
        self.deriving.clear();
        self.refresh_cost = done.started.elapsed();
        let now = Instant::now();
        match done.result {
            // A listing that started meanwhile read these sessions after this
            // derivation may have: its rows stay, and these are derived again.
            Ok(_) if self.listed_at.is_some_and(|listed| listed > done.started) => {
                self.stale.extend(done.wanted);
            }
            Ok(Some((derived, structure))) if Some(structure) == self.structure => {
                if !self.replace_rows(derived) {
                    self.schedule(now);
                }
            }
            Ok(_) => self.schedule(now),
            Err(error) => {
                self.out.failure("listing_failed", &error, Some(topic::CATALOG), None);
                self.schedule(now);
            }
        }
        if !self.stale.is_empty() {
            self.schedule_refresh(now);
        }
    }

    /// Puts derived rows in the place of their listed ones. False, changing
    /// nothing, when one of them is not listed under its key and name.
    fn replace_rows(&mut self, rows: Vec<Value>) -> bool {
        let mut updates = Vec::with_capacity(rows.len());
        for row in rows {
            let key = catalog::key(&row);
            let Some(index) = self.rows.iter().position(|(listed, _)| *listed == key) else { return false };
            if self.rows[index].1["name"] != row["name"] {
                return false;
            }
            updates.push((index, row));
        }
        let mut updated = BTreeSet::new();
        for (index, row) in updates {
            self.rows[index].1 = row;
            updated.insert(index);
        }
        catalog::redecorate(&mut self.rows, &mut self.digests, &updated);
        self.pending = true;
        let keys: Vec<String> = updated.iter().map(|index| self.rows[*index].0.clone()).collect();
        self.note_deadlines(keys);
        true
    }

    // ---- Time-driven rows and the drift check (task #13, P2.2) -------------

    /// Notes the next time-driven change of the rows under `keys`, derived
    /// just now, and when the earliest of all rows is due.
    fn note_deadlines(&mut self, keys: Vec<String>) {
        let wall = crate::state::now();
        for key in keys {
            let until = self.rows.iter().find(|(listed, _)| *listed == key).and_then(|(_, row)| crate::state::session_state::valid_until(row, wall));
            match until {
                Some(until) => self.deadlines.insert(key, until),
                None => self.deadlines.remove(&key),
            };
        }
        self.deadlines.retain(|key, _| self.rows.iter().any(|(listed, _)| listed == key));
        self.time_due = self.deadlines.values().copied().reduce(f64::min).map(|until| {
            // A little after the moment, so the derivation reads past it.
            Instant::now() + Duration::from_secs_f64((until - wall).max(0.0)) + Duration::from_millis(20)
        });
    }

    /// Rows whose time-driven change came are derived again: tracked rows
    /// alone, DeepSeek and OpenCode rows from their hosts, any other row by
    /// a whole listing.
    fn timed(&mut self, now: Instant) {
        self.time_due = None;
        let wall = crate::state::now();
        let due: Vec<String> = self.deadlines.iter().filter(|(_, until)| **until <= wall).map(|(key, _)| key.clone()).collect();
        let (mut names, mut native, mut whole) = (BTreeSet::new(), false, false);
        for key in &due {
            self.deadlines.remove(key);
            let Some((_, row)) = self.rows.iter().find(|(listed, _)| listed == key) else { continue };
            let name = row["name"].as_str().unwrap_or("");
            if source::native(name) {
                native = true;
            } else if row["tracked"] == true && !name.is_empty() {
                names.insert(name.to_owned());
            } else {
                whole = true;
            }
        }
        if whole {
            self.schedule(now);
        }
        if native {
            self.schedule_native(now);
        }
        if !names.is_empty() {
            self.stale.extend(names);
            self.schedule_refresh(now);
        }
        // The rest, once their rows come back or their moment comes.
        let wall = crate::state::now();
        self.time_due = self.deadlines.values().copied().reduce(f64::min).map(|until| {
            Instant::now() + Duration::from_secs_f64((until - wall).max(0.0)) + Duration::from_millis(20)
        });
    }

    // ---- Session host change counters (task #14, P2.4) ----------------------

    /// The sweep's interval: `HGS_WATCH_FULL_SECONDS` or 60 s, but the
    /// earlier 5 s while the terminal is known to have no counters (tmux, a
    /// session host that predates them) and no interval was given.
    fn sweep_interval(&self) -> Duration {
        if self.counted == Counted::Unsupported && !self.full_explicit {
            self.full.min(LEGACY_FULL)
        } else {
            self.full
        }
    }

    /// Asks the terminal's counters on a thread of its own every
    /// [`COUNTERS_POLL`], sending them only when they moved. A terminal
    /// without counters is said once and the thread ends; a host that does
    /// not answer for now is asked again later.
    fn start_counters(&mut self) {
        let results = self.git_results.clone();
        let _ = std::thread::Builder::new().name("hgs-watch-counters".into()).spawn(move || {
            let mut last = None;
            loop {
                match source::counters() {
                    Ok(None) => {
                        let _ = results.send(Input::Counters(None));
                        return;
                    }
                    Ok(Some(counters)) => {
                        if last.as_ref() != Some(&counters) {
                            if results.send(Input::Counters(Some(counters.clone()))).is_err() {
                                return;
                            }
                            last = Some(counters);
                        }
                    }
                    Err(_) => std::thread::sleep(PROBE),
                }
                std::thread::sleep(COUNTERS_POLL);
            }
        });
    }

    /// New counters: a moved structure lists every row (a session started,
    /// ended or was renamed); a moved pane waits to settle (`settle`).
    fn counted(&mut self, counters: Option<crate::state::backend::Counters>, now: Instant) {
        let Some(counters) = counters else {
            self.counted = Counted::Unsupported;
            self.screens.clear();
            self.screens_due = None;
            return;
        };
        let first = self.counted == Counted::Unknown || self.counted == Counted::Unsupported;
        if self.counted != Counted::Live(counters.structure) && !first {
            self.schedule(now + DEBOUNCE);
        }
        self.counted = Counted::Live(counters.structure);
        self.screens.retain(|name, _| counters.output.contains_key(name));
        for (name, count) in counters.output {
            let screen = self.screens.entry(name).or_insert(Screen { count, changed: None, moving_since: None });
            if screen.count != count {
                screen.count = count;
                screen.changed = Some(now);
                screen.moving_since.get_or_insert(now);
            }
        }
        self.settle(now);
    }

    /// Derives again the tracked rows whose pane stood still for [`SETTLE`]
    /// after it moved, or kept moving for [`BUSY_EVERY`]; notes when the
    /// next is due.
    fn settle(&mut self, now: Instant) {
        let mut names = BTreeSet::new();
        for (name, screen) in &mut self.screens {
            let Some(changed) = screen.changed else { continue };
            let settled = now >= changed + SETTLE;
            let busy = screen.moving_since.is_some_and(|since| now >= since + BUSY_EVERY);
            if settled || busy {
                names.insert(name.clone());
                screen.moving_since = if settled { None } else { Some(now) };
                if settled {
                    screen.changed = None;
                }
            }
        }
        self.screens_due = self
            .screens
            .values()
            .filter_map(|screen| {
                let changed = screen.changed?;
                Some(screen.moving_since.map_or(changed + SETTLE, |since| (changed + SETTLE).min(since + BUSY_EVERY)))
            })
            .min();
        let tracked: BTreeSet<String> = self
            .rows
            .iter()
            .filter(|(_, row)| row["tracked"] == true)
            .filter_map(|(_, row)| row["name"].as_str())
            .filter(|name| names.contains(*name))
            .map(str::to_owned)
            .collect();
        if !tracked.is_empty() {
            self.stale.extend(tracked);
            self.schedule_refresh(now);
        }
    }

    /// Logs the rows a sweep found otherwise than they were kept: one
    /// `watch-drift` line per row (at most [`DRIFT_LINES`]) naming the fields.
    /// The listing wins either way.
    fn check_drift(&mut self, listed: &[(String, Value)], again: &BTreeSet<String>) {
        let mut skip = std::mem::take(&mut self.drift_skip);
        skip.extend(again.iter().cloned());
        skip.extend(self.rows.iter().filter_map(|(_, row)| row["name"].as_str()).filter(|name| source::native(name)).map(str::to_owned));
        for (key, fields) in drift(&self.rows, listed, &skip).into_iter().take(DRIFT_LINES) {
            (self.drift_log)(&json!({"at":crate::state::now(),"command":"watch-drift","subject":key,"pid":std::process::id(),"fields":fields}));
        }
    }

    /// Reads the DeepSeek and OpenCode rows again on their own thread; the
    /// rows arrive as [`Input::NativeRows`]. At most one read runs: a host
    /// that does not answer holds that thread, never the loop.
    fn refresh_native(&mut self, now: Instant) {
        self.native_due = None;
        if self.native_running || !self.listed {
            return;
        }
        self.native_running = true;
        self.native_at = Some(now);
        let results = self.git_results.clone();
        let spawned = std::thread::Builder::new().name("hgs-watch-native".into()).spawn(move || {
            let result = source::native_rows();
            let _ = results.send(Input::NativeRows(NativeDone { started: now, result }));
        });
        if let Err(error) = spawned {
            self.native_done(NativeDone { started: now, result: Err(format!("cannot read the DeepSeek and OpenCode rows: {error}")) });
        }
    }

    /// The DeepSeek and OpenCode rows replace their listed ones; a whole
    /// listing follows when they are no longer the same sessions.
    fn native_done(&mut self, done: NativeDone) {
        self.native_running = false;
        self.native_cost = done.started.elapsed();
        // A listing that started after this read holds these rows already, or newer ones.
        if self.listing.running || self.listed_at.is_some_and(|listed| listed > done.started) {
            return;
        }
        let now = Instant::now();
        let derived = match done.result {
            Ok(derived) => derived,
            Err(error) => {
                self.out.failure("listing_failed", &error, Some(topic::CATALOG), None);
                self.schedule(now);
                return;
            }
        };
        let same = {
            let listed: BTreeSet<&str> = self
                .rows
                .iter()
                .filter(|(_, row)| row["name"].as_str().is_some_and(source::native))
                .map(|(key, _)| key.as_str())
                .collect();
            let keys: Vec<String> = derived.iter().map(catalog::key).collect();
            keys.len() == listed.len() && keys.iter().all(|key| listed.contains(key.as_str()))
        };
        if !same || !self.replace_rows(derived) {
            self.schedule(now);
        }
    }

    /// What no file shows: the terminal's structure, agent processes and
    /// their work, sessions without a hook event yet and DeepSeek and
    /// OpenCode hosts. The listing's metrics and presence are refreshed too.
    /// It samples the terminal and every agent process, so it runs on a
    /// thread; its findings arrive as [`Input::Probe`].
    fn probe(&mut self, now: Instant) {
        self.probed_at = Some(now);
        if !self.listed {
            // The last listing failed: try again.
            self.schedule(now);
            return;
        }
        if self.probe_running {
            return;
        }
        self.probe_running = true;
        let rows: Vec<Value> = self.rows.iter().map(|(_, row)| row.clone()).collect();
        let config = self.config.clone();
        let results = self.git_results.clone();
        let spawned = std::thread::Builder::new().name("hgs-watch-probe".into()).spawn(move || {
            let listed: Vec<&Value> = rows.iter().collect();
            let result = source::probe(&listed);
            // `metrics` and `desktop_presence` as the listing reports them now.
            let metrics = crate::metrics::snapshot(&config.state);
            let presence = crate::presence::listing(&config.state, crate::presence::now());
            let _ = results.send(Input::Probe(ProbeDone { started: now, result, metrics, presence }));
        });
        if spawned.is_err() {
            self.probe_running = false;
            self.schedule(now);
        }
    }

    /// A finished probe asks for the rows it found stale, or for a whole
    /// listing when the terminal changed or could not be read.
    fn probe_done(&mut self, done: ProbeDone) {
        self.probe_running = false;
        let now = Instant::now();
        // A listing that started meanwhile covers what this probe looked for.
        if self.listing.running || self.listed_at.is_some_and(|listed| listed > done.started) {
            return;
        }
        let probe = match done.result {
            Ok(probe) if Some(probe.structure) == self.structure => probe,
            // A changed terminal, or one this probe cannot read: list it whole.
            _ => {
                self.schedule(now);
                return;
            }
        };
        let native = self.rows.iter().any(|(_, row)| row["name"].as_str().is_some_and(source::native));
        if !probe.stale.is_empty() {
            self.stale.extend(probe.stale);
            self.schedule_refresh(now);
        }
        if native {
            self.schedule_native(now);
        }
        self.meta["metrics"] = done.metrics;
        match done.presence {
            Some(presence) => self.meta["desktop_presence"] = presence,
            None => {
                if let Some(meta) = self.meta.as_object_mut() {
                    meta.remove("desktop_presence");
                }
            }
        }
        self.pending = true;
    }

    /// Sends what changed since the client's copy, unless the client is
    /// behind: then the latest listing waits, so changes coalesce per key.
    fn emit_catalog(&mut self) {
        if !self.listed || !(self.pending || self.catalog_resync) || self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let mut removed = BTreeMap::new();
        if self.catalog_resync || !self.snapshot_sent {
            if self.catalog_resync {
                self.out.put(&Body::Gap(protocol::Gap { topic: topic::CATALOG.into(), key: None, reason: reason::OVERFLOW.into() }));
            }
            self.catalog_resync = false;
            self.snapshot();
            self.snapshot_sent = true;
        } else {
            let change = catalog::diff(&self.sent, &self.rows, &self.digests);
            if !change.is_empty() {
                removed = self.delta(change);
            }
            self.send_meta(false);
        }
        self.pending = false;
        self.follow_catalog(&removed);
    }

    fn item(&self, index: usize) -> Value {
        let (key, row) = &self.rows[index];
        json!({"key": key, "row": row})
    }

    /// Rows too large for any frame are left out with an error: read them
    /// with `hgs ls --json --local`.
    fn oversized(&mut self, index: usize, size: usize) -> bool {
        if size <= activity::PAYLOAD_LIMIT {
            return false;
        }
        let key = self.rows[index].0.clone();
        self.out.failure(
            "row_too_large",
            &format!("catalog row exceeds the frame limit ({size} bytes); read it with hgs ls --json --local"),
            Some(topic::CATALOG),
            Some(&key),
        );
        true
    }

    fn snapshot(&mut self) {
        let mut items = Vec::new();
        for index in 0..self.rows.len() {
            let item = self.item(index);
            let size = item.to_string().len();
            if !self.oversized(index, size) {
                items.push((item, size));
            }
        }
        self.revision += 1;
        let pages = catalog::pages(items, |(_, size)| *size, PAGE_LIMIT);
        let count = pages.len();
        for (page, items) in pages.into_iter().enumerate() {
            let body = Body::Snapshot(protocol::Snapshot {
                topic: topic::CATALOG.into(),
                key: None,
                items: Some(items.into_iter().map(|(item, _)| item).collect()),
                value: None,
                page: page as u32,
                last: page + 1 == count,
                cursor: self.revision,
            });
            if !self.out.put(&body) {
                return;
            }
        }
        self.sent = self
            .rows
            .iter()
            .zip(&self.digests)
            .map(|((key, row), digest)| (key.clone(), Sent::of(row, *digest)))
            .collect();
        self.send_meta(true);
    }

    /// Sends a delta; returns the removed keys with their reasons.
    fn delta(&mut self, change: catalog::Change) -> BTreeMap<String, &'static str> {
        /// One entry of a delta: a removal with its reason, or a row.
        enum Entry {
            Remove(String, &'static str),
            Upsert(usize),
        }
        let listed: BTreeSet<&str> = self.rows.iter().filter_map(|(_, row)| row["name"].as_str()).collect();
        let mut entries = Vec::new();
        let mut removed = BTreeMap::new();
        for key in change.remove {
            let Some(old) = self.sent.get(&key) else { continue };
            let why = catalog::removal_reason(old, &listed, source::archived, source::saved);
            removed.insert(key.clone(), why);
            entries.push((Entry::Remove(key.clone(), why), key.len() + 48));
        }
        for index in change.upsert {
            let size = self.item(index).to_string().len();
            if self.oversized(index, size) {
                let (key, row) = &self.rows[index];
                self.sent.insert(key.clone(), Sent::of(row, self.digests[index]));
            } else {
                entries.push((Entry::Upsert(index), size));
            }
        }
        for page in catalog::pages(entries, |(_, size)| *size, PAGE_LIMIT) {
            if page.is_empty() {
                continue;
            }
            let mut upsert = Vec::new();
            let mut remove = Vec::new();
            for (entry, _) in &page {
                match entry {
                    Entry::Remove(key, why) => remove.push(protocol::Removal { key: key.clone(), reason: Some((*why).into()) }),
                    Entry::Upsert(index) => upsert.push(protocol::CatalogItem {
                        key: self.rows[*index].0.clone(),
                        row: self.rows[*index].1.clone(),
                    }),
                }
            }
            self.revision += 1;
            let body = Body::Delta(protocol::Delta { topic: topic::CATALOG.into(), upsert, remove, cursor: self.revision });
            if !self.out.offer(&body, None) {
                self.catalog_resync = true;
                return removed;
            }
            for (entry, _) in page {
                match entry {
                    Entry::Remove(key, _) => {
                        self.sent.remove(&key);
                    }
                    Entry::Upsert(index) => {
                        let (key, row) = &self.rows[index];
                        self.sent.insert(key.clone(), Sent::of(row, self.digests[index]));
                    }
                }
            }
        }
        removed
    }

    /// `catalog.meta`: at once when a lasting field changed, at most every
    /// 10 s when only metrics or presence did.
    fn send_meta(&mut self, force: bool) {
        let lasting = catalog::digest(&catalog::masked(&self.meta, &catalog::VOLATILE_META));
        let full = catalog::digest(&self.meta);
        let due = match self.meta_sent {
            None => true,
            Some((sent_lasting, sent_full, at)) => {
                sent_lasting != lasting || (sent_full != full && at.elapsed() >= META_INTERVAL)
            }
        };
        if !(force || due) {
            return;
        }
        let size = self.meta.to_string().len();
        if size > activity::PAYLOAD_LIMIT {
            // Never expected; a resync would only meet the same limit again.
            self.out.failure(
                "meta_too_large",
                &format!("catalog.meta exceeds the frame limit ({size} bytes); read it with hgs ls --json --local"),
                Some(topic::CATALOG_META),
                None,
            );
            self.meta_sent = Some((lasting, full, Instant::now()));
            return;
        }
        let body = Body::Snapshot(protocol::Snapshot {
            topic: topic::CATALOG_META.into(),
            key: None,
            items: None,
            value: Some(self.meta.clone()),
            page: 0,
            last: true,
            cursor: self.revision,
        });
        if self.out.offer(&body, Some(format!("snapshot|{}", topic::CATALOG_META))) {
            self.meta_sent = Some((lasting, full, Instant::now()));
        } else {
            self.catalog_resync = true;
        }
    }

    /// Activity subscriptions follow their catalog rows: a rename changes the
    /// name, a changed row refreshes the summary, a removed row ends them.
    fn follow_catalog(&mut self, removed: &BTreeMap<String, &'static str>) {
        let rows: BTreeMap<&str, usize> = self.rows.iter().enumerate().map(|(index, (key, _))| (key.as_str(), index)).collect();
        let mut ended = Vec::new();
        for (position, sub) in self.subs.iter_mut().enumerate() {
            match rows.get(sub.key.as_str()) {
                Some(index) => {
                    let row = &self.rows[*index].1;
                    sub.listed = true;
                    if let Some(name) = row["name"].as_str().filter(|name| *name != sub.name) {
                        sub.name = name.to_owned();
                        sub.dirty = true;
                    }
                    if sub.row != Some(self.digests[*index]) {
                        sub.row = Some(self.digests[*index]);
                        sub.dirty = true;
                    }
                    sub.busy = activity::busy(row);
                }
                None if sub.listed => ended.push((position, removed.get(&sub.key).copied().unwrap_or(reason::ENDED))),
                None => {}
            }
        }
        for (position, why) in ended.into_iter().rev() {
            let sub = self.subs.remove(position);
            self.out.error(
                "subscription_ended",
                &format!("{} left the catalog ({why})", sub.name),
                Some(topic::ACTIVITY),
                Some(&sub.key),
            );
        }
        // Processes subscriptions follow their rows the same way; a changed
        // row reads the session again.
        if !self.processes.is_empty() {
            let rows: BTreeMap<&str, (String, Digest32)> = self
                .rows
                .iter()
                .zip(&self.digests)
                .map(|((key, row), digest)| (key.as_str(), (row["name"].as_str().unwrap_or_default().to_owned(), *digest)))
                .collect();
            for (key, name) in self.processes.follow(|key| rows.get(key).cloned(), Instant::now()) {
                let why = removed.get(&key).copied().unwrap_or(reason::ENDED);
                self.out.error("subscription_ended", &format!("{name} left the catalog ({why})"), Some(topic::PROCESSES), Some(&key));
            }
        }
    }

    // ---- Pulse -------------------------------------------------------------

    /// `scan`: the stamps and the journal are due (task #12: every pulse
    /// without a healthy change bus, else on a hint or the safety scan).
    fn pulse(&mut self, now: Instant, scan: bool, phases: &mut crate::state::slow_calls::Phases) {
        if scan {
            self.scan_inputs(now, phases);
        }
        self.pulse_timers(now, phases);
    }

    /// What files under the state folder and the journal show: changed
    /// bindings derive their rows again, other inputs list the whole.
    fn scan_inputs(&mut self, now: Instant, phases: &mut crate::state::slow_calls::Phases) {
        let journal = self.poll_journal();
        phases.mark("journal");
        let scan = stamps::scan(&self.root);
        phases.mark("stamps");
        let (changed, removed) = scan.changed_bindings(&self.scan);
        // Inputs other than bindings, and added or removed bindings, change
        // the whole listing; a changed binding changes its own row.
        let mut whole = scan.global != self.scan.global || removed || matches!(journal, Journal::Unknown);
        let mut names = BTreeSet::new();
        for path in changed {
            let identity = source::binding_identity(&path);
            match &identity {
                Some((name, _)) if self.scan.bindings.contains_key(&path) => {
                    names.insert(name.clone());
                }
                _ => whole = true,
            }
            self.identities.insert(path, identity);
        }
        if removed {
            self.identities.retain(|path, _| scan.bindings.contains_key(path));
        }
        // Journal rows change the rows of the sessions they belong to. Rows
        // of other names belong to no listed binding.
        if let Journal::Names(journals) = &journal {
            for (name, journal) in self.identities.values().flatten() {
                if journals.contains(journal) {
                    names.insert(name.clone());
                }
            }
        }
        self.scan = scan;
        // A pulse the change bus asked for comes after its own debounce.
        let debounce = if std::mem::take(&mut self.hinted) { Duration::ZERO } else { DEBOUNCE };
        if whole {
            self.schedule(now + debounce);
        } else if !names.is_empty() {
            self.stale.extend(names);
            self.schedule_refresh(now + debounce);
        }
        phases.mark("bindings");
    }

    /// The probe, the whole listing and the subscriptions' own timers.
    fn pulse_timers(&mut self, now: Instant, phases: &mut crate::state::slow_calls::Phases) {
        if self.probed_at.is_none_or(|at| now >= at + PROBE) {
            self.probe(now);
            phases.mark("probe");
        }
        if self.listed_at.is_none_or(|at| now >= at + self.sweep_interval()) {
            self.schedule_sweep(now);
        }
        self.activity(now);
        phases.mark("activity");
        self.pulse_git(now);
        if !self.processes.is_empty() {
            let jobs = self.processes.pulse(now);
            self.run_processes(jobs);
        }
        if !self.tasks.is_empty() {
            let jobs = self.tasks.pulse(now, stamps::files);
            self.run_tasks(jobs);
        }
        if let Some(job) = self.organization.pulse(now, stamps::files) {
            self.run_organization(job);
        }
        if let Some(job) = self.team.pulse(now, stamps::files) {
            self.run_team(job);
        }
        if let Some(job) = self.drive_folders.pulse(now, stamps::files) {
            self.run_drive_folders(job);
        }
        phases.mark("topics");
    }

    fn open_journal(&mut self) {
        if self.journal.is_some() {
            return;
        }
        self.journal = source::journal();
        if let Some(db) = &self.journal {
            self.data_version = source::data_version(db).ok();
            self.journal_max = source::bounds(db).map_or(0, |(_, max)| max);
            // A newly available or reopened journal may already contain a
            // file-changing call missed while it was unavailable. Recheck
            // only the existing Git subscriptions, without replaying history.
            if !self.git.is_empty() {
                self.writers = Writers::All;
            }
            // Rows written before this connection opened: read after each cursor.
            for sub in self.subs.iter_mut().filter(|sub| !sub.native) {
                sub.more = true;
            }
        }
    }

    /// The journal names with new rows. Subscriptions with new rows read them.
    fn poll_journal(&mut self) -> Journal {
        self.open_journal();
        let Some(db) = &self.journal else { return Journal::Unchanged };
        let version = match source::data_version(db) {
            Ok(version) => version,
            Err(_) => {
                self.journal = None;
                return Journal::Unchanged;
            }
        };
        if Some(version) == self.data_version {
            return Journal::Unchanged;
        }
        let Ok((_, max)) = source::bounds(db) else { return Journal::Unknown };
        // A busy bounds read must retry this version on the next pulse.
        self.data_version = Some(version);
        if max < self.journal_max {
            // A replaced journal: every cursor is meaningless now.
            for sub in self.subs.iter_mut().filter(|sub| !sub.native) {
                sub.resync = Some(reason::RESET);
            }
            self.journal_max = max;
            return Journal::Unknown;
        }
        if max == self.journal_max {
            return Journal::Unchanged;
        }
        const SCAN: usize = 10_000;
        let names: Option<BTreeSet<String>> = if max - self.journal_max > SCAN as i64 {
            None
        } else {
            source::changed_names(db, self.journal_max, SCAN).ok().map(|names| names.into_iter().collect())
        };
        if !self.git.is_empty() {
            self.writers = match names.is_some().then(|| source::writing_names(db, self.journal_max, SCAN)) {
                Some(Ok(writers)) if writers.is_empty() => Writers::None,
                Some(Ok(writers)) => Writers::Names(writers),
                _ => Writers::All,
            };
        }
        for sub in self.subs.iter_mut().filter(|sub| !sub.native) {
            let touched = match (&names, &sub.journal) {
                (Some(names), Some(journal)) => names.contains(journal),
                _ => true,
            };
            if touched {
                sub.more = true;
            }
        }
        self.journal_max = max;
        names.map_or(Journal::Unknown, Journal::Names)
    }

    fn activity(&mut self, now: Instant) {
        let mut refused = Vec::new();
        for index in 0..self.subs.len() {
            if let Some((after, at, wait)) = self.subs[index].start {
                if now >= at {
                    match self.start(index, after, false) {
                        Started::Sent => self.subs[index].start = None,
                        Started::Later => {
                            let wait = (wait * 2).min(START_RETRY_MAX);
                            self.subs[index].start = Some((after, Instant::now() + wait, wait));
                        }
                        Started::Refused => refused.push(index),
                    }
                }
                continue;
            }
            if self.subs[index].resync.is_some() {
                continue;
            }
            if self.subs[index].more {
                self.read_events(index);
            }
            self.read_transcript(index, now);
            if self.subs[index].resync.is_some() {
                continue;
            }
            let sub = &self.subs[index];
            // P1.5: one refresh per subscription at a time. A change seen
            // meanwhile keeps `dirty`, so one more follows the result.
            if sub.inspecting.is_some() {
                continue;
            }
            let interval = if sub.native {
                NATIVE_REFRESH
            } else if sub.busy {
                BUSY_REFRESH
            } else {
                IDLE_REFRESH
            };
            let since = now.saturating_duration_since(sub.refreshed);
            if (sub.dirty && since >= STATE_GAP) || since >= interval {
                self.refresh(index);
            }
        }
        for index in refused.into_iter().rev() {
            self.subs.remove(index);
        }
    }

    /// Journal rows after the subscription's cursor, as `inspect` shapes them.
    fn read_events(&mut self, index: usize) {
        let Some(db) = &self.journal else { return };
        let sub = &mut self.subs[index];
        sub.more = false;
        let timeline = match source::timeline(&sub.name) {
            Ok(Some(timeline)) => timeline,
            Ok(None) => return,
            Err(error) => {
                self.out.failure("inspect_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return;
            }
        };
        if sub.run_id.as_deref().is_some_and(|run| run != timeline.run_id()) {
            // The name now holds another run; the catalog removes or renames ours.
            sub.dirty = true;
            return;
        }
        sub.journal = Some(timeline.journal().to_owned());
        follow_conversation(sub, &timeline);
        let events = match timeline.events(db, sub.cursor, protocol::ACTIVITY_READ, false) {
            Ok(events) => events,
            Err(error) => {
                self.out.failure("journal_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return;
            }
        };
        sub.more = events.len() == protocol::ACTIVITY_READ;
        if events.is_empty() {
            return;
        }
        sub.dirty = true;
        for page in activity::event_pages(events, activity::PAYLOAD_LIMIT) {
            let cursor = page.last().and_then(|event| event["seq"].as_i64()).unwrap_or(sub.cursor);
            let body = Body::Events(protocol::Events {
                topic: topic::ACTIVITY.into(),
                key: sub.key.clone(),
                events: page,
                cursor,
            });
            if !self.out.offer(&body, None) {
                sub.resync = Some(reason::OVERFLOW);
                sub.more = false;
                return;
            }
            sub.cursor = cursor;
        }
    }

    /// Messages between tools from the followed Codex or Claude transcript,
    /// each as an `event` frame; a later revision of a queued one replaces it.
    fn read_transcript(&mut self, index: usize, now: Instant) {
        let sub = &mut self.subs[index];
        if sub.native {
            return;
        }
        let Some(tail) = sub.transcript.as_mut() else {
            if now < sub.transcript_at {
                return;
            }
            sub.transcript_at = now + TRANSCRIPT_RETRY;
            let Ok(Some(timeline)) = source::timeline(&sub.name) else { return };
            if sub.run_id.as_deref().is_none_or(|run| run == timeline.run_id()) {
                sub.transcript = timeline.transcript();
                // Followed from its end: the next summary carries what came before.
                sub.dirty |= sub.transcript.is_some();
            }
            return;
        };
        let messages = match tail.read() {
            source::TailRead::Messages(messages) => messages,
            source::TailRead::Ended => {
                sub.transcript = None;
                sub.transcript_at = now;
                return;
            }
        };
        for mut message in messages {
            let id = activity::message_id(&message);
            let Some(rev) = sub.message_rev(&id, &message) else { continue };
            activity::shrink_event(&mut message, activity::PAYLOAD_LIMIT);
            let coalesce = format!("event|{}|{}|{id}", topic::ACTIVITY, sub.key);
            let body = Body::Event(protocol::EventUpdate { topic: topic::ACTIVITY.into(), key: sub.key.clone(), id, rev, event: message });
            if !self.out.offer(&body, Some(coalesce)) {
                sub.resync = Some(reason::OVERFLOW);
                return;
            }
        }
    }

    /// Re-inspects one session on a thread (P1.5: an inspection takes
    /// seconds for a large transcript, and on the loop every frame of the
    /// stream waited for it). The result arrives as [`Input::Inspected`].
    fn refresh(&mut self, index: usize) {
        // Events after this are not wanted: the journal rows go out as
        // `events` frames, so the inspection reads only the summary.
        let after = if self.subs[index].native { 0 } else { self.journal_max.max(self.subs[index].cursor) };
        self.inspect_tokens += 1;
        let token = self.inspect_tokens;
        let sub = &mut self.subs[index];
        sub.refreshed = Instant::now();
        sub.dirty = false;
        sub.inspecting = Some(token);
        let (key, name, native) = (sub.key.clone(), sub.name.clone(), sub.native);
        let inspect = self.inspect;
        let results = self.git_results.clone();
        let (thread_key, thread_name) = (key.clone(), name.clone());
        let spawned = std::thread::Builder::new().name("hgs-watch-inspect".into()).spawn(move || {
            let result = inspect(&thread_name, after, native);
            let _ = results.send(Input::Inspected(InspectDone { key: thread_key, token, name: thread_name, result }));
        });
        if let Err(error) = spawned {
            self.inspected(InspectDone { key, token, name, result: Err(format!("cannot start the inspection: {error}")) });
        }
    }

    /// Applies a refresh: sends `state` when the summary changed, and for
    /// DeepSeek and OpenCode a replacement of their event window. A result
    /// whose subscription ended, was replaced or restarted, or was renamed
    /// meanwhile is dropped; a renamed one is read again.
    fn inspected(&mut self, done: InspectDone) {
        let Some(sub) = self.subs.iter_mut().find(|sub| sub.key == done.key && sub.inspecting == Some(done.token)) else {
            return;
        };
        sub.inspecting = None;
        if sub.name != done.name {
            sub.dirty = true;
            return;
        }
        let (inspection, timeline) = match done.result {
            Ok(read) => read,
            Err(error) => {
                self.out.failure("inspect_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return;
            }
        };
        if inspection["run_id"].as_str().zip(sub.run_id.as_deref()).is_some_and(|(seen, wanted)| seen != wanted) {
            return;
        }
        if !sub.native {
            if let Some(timeline) = timeline {
                sub.journal = Some(timeline.journal().to_owned());
                follow_conversation(sub, &timeline);
            }
        } else {
            let events = inspection["events"].as_array().cloned().unwrap_or_default();
            let digest = catalog::digest(&Value::Array(events.clone()));
            if sub.events != Some(digest) && send_window(&self.out, sub, events, 0) {
                sub.events = Some(digest);
            }
        }
        send_state(&self.out, sub, &inspection);
    }

    /// Topics the client fell behind on: `gap`, then a tail snapshot.
    fn resync_activity(&mut self) {
        for index in 0..self.subs.len() {
            let Some(why) = self.subs[index].resync else { continue };
            if self.subs[index].start.is_some() {
                // Its first frames are still owed: they are a tail anyway.
                self.subs[index].resync = None;
                continue;
            }
            if self.out.writer.backlog() > DEFER_BYTES {
                return;
            }
            self.out.put(&Body::Gap(protocol::Gap {
                topic: topic::ACTIVITY.into(),
                key: Some(self.subs[index].key.clone()),
                reason: why.into(),
            }));
            self.subs[index].resync = None;
            self.subs[index].state = None;
            self.subs[index].events = None;
            // A failed inspection leaves the tail owed; it is read again.
            if matches!(self.start(index, 0, true), Started::Later) {
                self.subs[index].start = Some((0, Instant::now() + START_RETRY, START_RETRY));
            }
        }
    }

    // ---- Git ---------------------------------------------------------------

    fn subscribe_git(&mut self, path: Option<&str>) {
        let Some(path) = path else {
            self.out.error("bad_request", "a git subscription names its folder as path", Some(topic::GIT), None);
            return;
        };
        match self.git.subscribe(path, Instant::now()) {
            Ok(job) => self.run_git(job),
            Err(git::Refusal::BadRequest(message)) => self.out.error("bad_request", message, Some(topic::GIT), Some(path)),
            Err(git::Refusal::TooMany) => self.out.error(
                "too_many_subscriptions",
                &format!("a stream follows at most {} git folders", protocol::GIT_SUBSCRIPTIONS),
                Some(topic::GIT),
                Some(path),
            ),
        }
    }

    /// Hands checks to the git workers, started with the first one.
    fn run_git(&mut self, jobs: impl IntoIterator<Item = git::Job>) {
        for job in jobs {
            let pool = self.git_pool.get_or_insert_with(|| {
                let results = self.git_results.clone();
                git::Pool::spawn(git::WORKERS, Some(source::state_root().join("watch-git")), move |done| {
                    let _ = results.send(Input::Git(done));
                })
            });
            pool.run(job);
        }
    }

    /// Sessions that finished a file-changing tool, then stat changes and
    /// checks whose time came.
    fn pulse_git(&mut self, now: Instant) {
        match std::mem::take(&mut self.writers) {
            Writers::None => {}
            Writers::All => self.git.touch_all(now),
            Writers::Names(journals) => {
                let journals: BTreeMap<&str, Option<f64>> = journals.iter().map(|(journal, at)| (journal.as_str(), *at)).collect();
                let names: BTreeMap<&str, Option<f64>> = self
                    .identities
                    .values()
                    .flatten()
                    .filter_map(|(name, journal)| Some((name.as_str(), *journals.get(journal.as_str())?)))
                    .collect();
                for (_, row) in &self.rows {
                    let Some(at) = row["name"].as_str().and_then(|name| names.get(name)) else { continue };
                    let dirs: Vec<PathBuf> = ["cwd", "cwd_canonical", "git_root"]
                        .iter()
                        .filter_map(|field| row[*field].as_str().filter(|dir| !dir.is_empty()))
                        .map(PathBuf::from)
                        .collect();
                    self.git.touch(&dirs, *at, now);
                }
            }
        }
        if self.git.is_empty() {
            return;
        }
        let jobs = self.git.pulse(now, stamps::files);
        self.run_git(jobs);
    }

    fn git_done(&mut self, done: git::Done) {
        let key = done.path.clone();
        if let Some(error) = self.git.done(done, Instant::now()) {
            self.out.failure("git_status_failed", &error, Some(topic::GIT), Some(&key));
        }
    }

    /// Sends changed git states unless the client is behind; they wait and
    /// the latest of each folder goes out once it catches up.
    fn flush_git(&mut self) {
        if !self.git.pending() || self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let out = &mut self.out;
        self.git.flush(|key, value, rev| {
            let body = Body::State(protocol::State { topic: topic::GIT.into(), key: key.into(), value: value.clone(), rev });
            match Out::encode(&body) {
                Some(body) => out.writer.offer(Item { coalesce: Some(format!("state|{}|{key}", topic::GIT)), body }),
                None => {
                    // Never expected: a git-status object is a few hundred bytes.
                    out.failure("state_too_large", "git status exceeds the frame limit; read it with hgs git-status", Some(topic::GIT), Some(key));
                    true
                }
            }
        });
    }

    // ---- Processes ---------------------------------------------------------

    fn subscribe_processes(&mut self, session: Option<protocol::SessionRef>, interval_ms: Option<u64>, refresh: bool) {
        let Some(session) = session
            .filter(|session| !session.name.is_empty() && session.name.len() <= 1024 && !session.name.chars().any(char::is_control))
        else {
            self.out.error("bad_request", "a processes subscription names its session", Some(topic::PROCESSES), None);
            return;
        };
        let key = session.key();
        if !self.processes.contains(&key) && self.processes.len() >= protocol::PROCESSES_SUBSCRIPTIONS {
            self.out.error(
                "too_many_subscriptions",
                &format!("a stream follows the processes of at most {} sessions", protocol::PROCESSES_SUBSCRIPTIONS),
                Some(topic::PROCESSES),
                Some(&key),
            );
            return;
        }
        // Additive options change scheduling without dropping the inventory
        // or cancelling its read. An older client's plain resubscribe still
        // replaces the subscription and sends its first state as before.
        let now = Instant::now();
        if (interval_ms.is_some() || refresh) && self.processes.configure(&key, interval_ms, refresh, now) {
            return;
        }
        // A renamed session keeps its run key; follow its current name.
        let row = self.rows.iter().position(|(row_key, _)| *row_key == key);
        let name = row.and_then(|index| self.rows[index].1["name"].as_str()).unwrap_or(&session.name).to_owned();
        let digest = row.map(|index| self.digests[index]);
        let job = initial_process_subscription(&mut self.processes, &key, &name,
            session.run_id.filter(|run| !run.is_empty()), digest, interval_ms, now);
        self.run_processes([job]);
    }

    /// Hands reads to the processes workers, started with the first one.
    fn run_processes(&mut self, jobs: impl IntoIterator<Item = processes::Job>) {
        for job in jobs {
            let pool = self.processes_pool.get_or_insert_with(|| {
                let results = self.git_results.clone();
                processes::Pool::spawn(processes::WORKERS, move |done| {
                    let _ = results.send(Input::Processes(done));
                })
            });
            match pool.run(job) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(job)) => self.processes.defer(job, Instant::now()),
                Err(mpsc::TrySendError::Disconnected(job)) => {
                    self.processes_done(processes::Done { token: job.token, key: job.key, value: Some(Err("Process inventory worker is unavailable".into())) });
                }
            }
        }
    }

    fn processes_done(&mut self, done: processes::Done) {
        let key = done.key.clone();
        if let processes::Outcome::Failed(error) = self.processes.done(done) {
            self.out.failure("inspect_failed", &error, Some(topic::PROCESSES), Some(&key));
        }
    }

    /// Sends changed inventories unless the client is behind; they wait and
    /// the latest of each session goes out once it catches up.
    fn flush_processes(&mut self) {
        if !self.processes.pending() || self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let out = &mut self.out;
        self.processes.flush(|key, value, rev| {
            let mut value = value.clone();
            activity::shrink_state(&mut value, activity::PAYLOAD_LIMIT);
            let body = Body::State(protocol::State { topic: topic::PROCESSES.into(), key: key.into(), value, rev });
            match Out::encode(&body) {
                Some(body) => out.writer.offer(Item { coalesce: Some(format!("state|{}|{key}", topic::PROCESSES)), body }),
                None => true,
            }
        });
    }

    // ---- Tasks -------------------------------------------------------------

    fn subscribe_tasks(&mut self, project: Option<&str>, window: Option<u32>) {
        let Some(project) = project.filter(|project| {
            !project.is_empty() && project.len() <= tasks::PROJECT_BYTES && !project.chars().any(char::is_control)
        }) else {
            self.out.error("bad_request", "a tasks subscription names its project ID", Some(topic::TASKS), None);
            return;
        };
        if window.is_some_and(|rows| rows == 0 || rows as usize > protocol::TASKS_WINDOW) {
            self.out.error(
                "bad_request",
                &format!("a tasks window is between 1 and {} rows of each stage", protocol::TASKS_WINDOW),
                Some(topic::TASKS),
                Some(project),
            );
            return;
        }
        if !self.tasks.contains(project) && self.tasks.len() >= protocol::TASKS_SUBSCRIPTIONS {
            self.out.error(
                "too_many_subscriptions",
                &format!("a stream follows the tasks of at most {} projects", protocol::TASKS_SUBSCRIPTIONS),
                Some(topic::TASKS),
                Some(project),
            );
            return;
        }
        let job = self.tasks.subscribe(project, window, Instant::now());
        self.run_tasks([job]);
    }

    /// Hands reads to the task list worker, started with the first one.
    fn run_tasks(&mut self, jobs: impl IntoIterator<Item = tasks::Job>) {
        for job in jobs {
            let pool = self.tasks_pool.get_or_insert_with(|| {
                let results = self.git_results.clone();
                tasks::Pool::spawn(tasks::WORKERS, move |done| {
                    let _ = results.send(Input::Tasks(done));
                })
            });
            match pool.run(job) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(job)) => self.tasks.defer(job, Instant::now()),
                Err(mpsc::TrySendError::Disconnected(job)) => {
                    let tasks::Job { token, key, .. } = job;
                    self.tasks_done(tasks::Done { token, key, value: Some(Err("Task list worker is unavailable".into())) });
                }
            }
        }
    }

    fn tasks_done(&mut self, done: tasks::Done) {
        let key = done.key.clone();
        let config = self.config;
        let store = |id: &str| {
            let dir = crate::tasks::store_dir(config, id);
            vec![dir.join("tasks"), dir]
        };
        if let Some(error) = self.tasks.done(done, store) {
            self.out.failure("tasks_failed", &error, Some(topic::TASKS), Some(&key));
        }
    }

    /// Sends changed task lists as paged snapshots unless the client is
    /// behind; the latest of each project goes out once it catches up.
    fn flush_tasks(&mut self) {
        if !self.tasks.pending() || self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let out = &mut self.out;
        self.tasks.flush(|key, listing, digest| {
            let items = listing["tasks"].as_array().cloned().unwrap_or_default();
            let mut pages = activity::event_pages(items, PAGE_LIMIT);
            if pages.is_empty() {
                pages.push(Vec::new());
            }
            let count = pages.len();
            for (page, items) in pages.into_iter().enumerate() {
                let last = page + 1 == count;
                let body = Body::Snapshot(protocol::Snapshot {
                    topic: topic::TASKS.into(),
                    key: Some(key.into()),
                    items: Some(items),
                    value: last.then(|| tasks::summary(listing, digest)),
                    page: page as u32,
                    last,
                    cursor: 0,
                });
                if !out.put(&body) {
                    return false;
                }
            }
            true
        });
    }

    // ---- Organization ------------------------------------------------------

    fn subscribe_organization(&mut self) {
        let files = crate::swarm::organization_files(self.config);
        let stamp = stamps::files(&files);
        let job = self.organization.subscribe(files, stamp, Instant::now());
        self.run_organization(job);
    }

    /// Hands a read to the organization worker, started with the first one.
    fn run_organization(&mut self, job: organization::Job) {
        let pool = self.organization_pool.get_or_insert_with(|| {
            let results = self.git_results.clone();
            organization::Pool::spawn(move |done| {
                let _ = results.send(Input::Organization(done));
            })
        });
        match pool.run(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => self.organization.defer(job, Instant::now()),
            Err(mpsc::TrySendError::Disconnected(job)) => {
                self.organization_done(organization::Done { token: job.token, value: Some(Err("Organization worker is unavailable".into())) });
            }
        }
    }

    fn organization_done(&mut self, done: organization::Done) {
        if let Some(error) = self.organization.done(done) {
            self.out.failure("organization_failed", &error, Some(topic::ORGANIZATION), None);
        }
    }

    /// Sends the organization's changed rows unless the client is behind:
    /// the first rows as a paged snapshot, later ones as one delta.
    fn flush_organization(&mut self) {
        if self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let Some(plan) = self.organization.plan(PAGE_LIMIT) else { return };
        let cursor = self.organization.next_rev();
        let item = |(key, row): (String, Value)| json!({"key": key, "row": row});
        let delivered = match plan {
            organization::Plan::Snapshot(pages) => {
                let count = pages.len();
                pages.into_iter().enumerate().all(|(page, rows)| {
                    self.out.put(&Body::Snapshot(protocol::Snapshot {
                        topic: topic::ORGANIZATION.into(),
                        key: None,
                        items: Some(rows.into_iter().map(item).collect()),
                        value: None,
                        page: page as u32,
                        last: page + 1 == count,
                        cursor,
                    }))
                })
            }
            organization::Plan::Delta { upsert, remove } => self.out.put(&Body::Delta(protocol::Delta {
                topic: topic::ORGANIZATION.into(),
                upsert: upsert.into_iter().map(|(key, row)| protocol::CatalogItem { key, row }).collect(),
                remove: remove.into_iter().map(|key| protocol::Removal { key, reason: None }).collect(),
                cursor,
            })),
        };
        if delivered {
            self.organization.sent();
        } else {
            self.organization.resend();
        }
    }

    // ---- Team --------------------------------------------------------------

    fn subscribe_team(&mut self) {
        let store = crate::state::teams::store::path();
        let files = crate::state::teams::store::files(&store);
        let stamp = stamps::files(&files);
        let job = self.team.subscribe(files, stamp, Instant::now());
        self.run_team(job);
    }

    /// Hands a read to the team worker, started with the first one.
    fn run_team(&mut self, job: team::Job) {
        let pool = self.team_pool.get_or_insert_with(|| {
            let results = self.git_results.clone();
            team::Pool::spawn(crate::state::teams::store::path(), move |done| {
                let _ = results.send(Input::Team(done));
            })
        });
        match pool.run(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => self.team.defer(job, Instant::now()),
            Err(mpsc::TrySendError::Disconnected(job)) => {
                self.team_done(team::Done { token: job.token, value: Some(Err("Team worker is unavailable".into())) });
            }
        }
    }

    fn team_done(&mut self, done: team::Done) {
        if let Some(error) = self.team.done(done) {
            self.out.failure("team_failed", &error, Some(topic::TEAM), None);
        }
    }

    /// Sends the teams' changed heads: the first rows as a paged snapshot,
    /// later ones as one offered delta. A delta the client is too far behind
    /// to take is a `gap`, then a snapshot once it caught up.
    fn flush_team(&mut self) {
        if self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let Some(plan) = self.team.plan(PAGE_LIMIT) else { return };
        let cursor = self.team.next_rev();
        let item = |(key, row): (String, Value)| json!({"key": key, "row": row});
        match plan {
            team::Plan::Snapshot { gap, pages } => {
                if gap && !self.out.put(&Body::Gap(protocol::Gap { topic: topic::TEAM.into(), key: None, reason: reason::OVERFLOW.into() })) {
                    return;
                }
                let count = pages.len();
                let delivered = pages.into_iter().enumerate().all(|(page, rows)| {
                    self.out.put(&Body::Snapshot(protocol::Snapshot {
                        topic: topic::TEAM.into(),
                        key: None,
                        items: Some(rows.into_iter().map(item).collect()),
                        value: None,
                        page: page as u32,
                        last: page + 1 == count,
                        cursor,
                    }))
                });
                if delivered {
                    self.team.sent();
                } else {
                    self.team.missed();
                }
            }
            team::Plan::Delta { upsert, remove } => {
                let body = Body::Delta(protocol::Delta {
                    topic: topic::TEAM.into(),
                    upsert: upsert.into_iter().map(|(key, row)| protocol::CatalogItem { key, row }).collect(),
                    remove: remove.into_iter().map(|key| protocol::Removal { key, reason: None }).collect(),
                    cursor,
                });
                if self.out.offer(&body, None) {
                    self.team.sent();
                } else {
                    self.team.missed();
                }
            }
        }
    }

    fn subscribe_drive_folders(&mut self) {
        let files = crate::swarm::drive_folders::watch_files(self.config);
        let stamp = stamps::files(&files);
        let job = self.drive_folders.subscribe(files, stamp, Instant::now());
        self.run_drive_folders(job);
    }

    fn run_drive_folders(&mut self, job: organization::Job) {
        let pool = self.drive_folders_pool.get_or_insert_with(|| {
            let results = self.git_results.clone();
            organization::Pool::spawn_drive(move |done| {
                let _ = results.send(Input::DriveFolders(done));
            })
        });
        match pool.run(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => self.drive_folders.defer(job, Instant::now()),
            Err(mpsc::TrySendError::Disconnected(job)) => {
                self.drive_folders_done(organization::Done { token: job.token, value: Some(Err("Drive-folder worker is unavailable".into())) });
            }
        }
    }

    fn drive_folders_done(&mut self, done: organization::Done) {
        if let Some(error) = self.drive_folders.done(done) {
            self.out.failure("drive_folders_failed", &error, Some(topic::DRIVE_FOLDERS), None);
        }
    }

    fn flush_drive_folders(&mut self) {
        if self.out.writer.backlog() > DEFER_BYTES {
            return;
        }
        let Some(plan) = self.drive_folders.plan(PAGE_LIMIT) else { return };
        let cursor = self.drive_folders.next_rev();
        let item = |(key, row): (String, Value)| json!({"key": key, "row": row});
        let delivered = match plan {
            organization::Plan::Snapshot(pages) => {
                let count = pages.len();
                pages.into_iter().enumerate().all(|(page, rows)| {
                    self.out.put(&Body::Snapshot(protocol::Snapshot {
                        topic: topic::DRIVE_FOLDERS.into(),
                        key: None,
                        items: Some(rows.into_iter().map(item).collect()),
                        value: None,
                        page: page as u32,
                        last: page + 1 == count,
                        cursor,
                    }))
                })
            }
            organization::Plan::Delta { upsert, remove } => self.out.put(&Body::Delta(protocol::Delta {
                topic: topic::DRIVE_FOLDERS.into(),
                upsert: upsert.into_iter().map(|(key, row)| protocol::CatalogItem { key, row }).collect(),
                remove: remove.into_iter().map(|key| protocol::Removal { key, reason: None }).collect(),
                cursor,
            })),
        };
        if delivered {
            self.drive_folders.sent();
        } else {
            self.drive_folders.resend();
        }
    }

    // ---- Control -----------------------------------------------------------

    fn control(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let operation = match protocol::decode_control(text) {
            Ok(operation) => operation,
            Err(error) => {
                self.out.error("bad_request", &format!("invalid control operation: {error}"), None, None);
                return;
            }
        };
        match operation {
            Control::Ping(ping) => {
                self.out.put(&Body::Pong(protocol::Pong { id: ping.id }));
            }
            Control::Subscribe(subscription) => self.subscribe(subscription),
            Control::Unsubscribe(subscription) => {
                if subscription.topic == topic::GIT {
                    if let Some(path) = &subscription.path {
                        self.git.unsubscribe(path);
                    }
                } else if subscription.topic == topic::TASKS {
                    if let Some(project) = &subscription.project {
                        self.tasks.unsubscribe(project);
                    }
                } else if subscription.topic == topic::ORGANIZATION {
                    self.organization.unsubscribe();
                } else if subscription.topic == topic::TEAM {
                    self.team.unsubscribe();
                } else if subscription.topic == topic::DRIVE_FOLDERS {
                    self.drive_folders.unsubscribe();
                } else if let Some(session) = subscription.session.as_ref().filter(|_| subscription.topic == topic::PROCESSES) {
                    self.processes.unsubscribe(&session.key());
                } else if let Some(session) = subscription.session.filter(|_| subscription.topic == topic::ACTIVITY) {
                    let key = session.key();
                    self.subs.retain(|sub| sub.key != key);
                }
            }
        }
    }

    fn subscribe(&mut self, request: protocol::Subscription) {
        match request.topic.as_str() {
            topic::CATALOG | topic::CATALOG_META => return,
            topic::GIT => return self.subscribe_git(request.path.as_deref()),
            topic::PROCESSES => return self.subscribe_processes(request.session, request.interval_ms, request.refresh == Some(true)),
            topic::TASKS => return self.subscribe_tasks(request.project.as_deref(), request.window),
            topic::ORGANIZATION => return self.subscribe_organization(),
            topic::TEAM => return self.subscribe_team(),
            topic::DRIVE_FOLDERS => return self.subscribe_drive_folders(),
            topic::ACTIVITY => {}
            other => {
                self.out.error(
                    "unsupported_topic",
                    &format!("watch-v1 does not stream {other}; keep reading it with its command"),
                    Some(other),
                    None,
                );
                return;
            }
        }
        let Some(session) = request
            .session
            .filter(|session| !session.name.is_empty() && session.name.len() <= 1024 && !session.name.chars().any(char::is_control))
        else {
            self.out.error("bad_request", "an Activity subscription names its session", Some(topic::ACTIVITY), None);
            return;
        };
        let after = request.after.unwrap_or(0);
        if after < 0 {
            self.out.error("bad_request", "after must be nonnegative", Some(topic::ACTIVITY), None);
            return;
        }
        let key = session.key();
        if let Some(before) = request.before {
            return self.older(&key, before, request.tail);
        }
        self.subs.retain(|sub| sub.key != key);
        if self.subs.len() >= protocol::ACTIVITY_SUBSCRIPTIONS {
            self.out.error(
                "too_many_subscriptions",
                &format!("a stream follows at most {} sessions", protocol::ACTIVITY_SUBSCRIPTIONS),
                Some(topic::ACTIVITY),
                Some(&key),
            );
            return;
        }
        // A renamed session keeps its run key; follow its current name.
        let row = self.rows.iter().position(|(row_key, _)| *row_key == key);
        let name = row
            .and_then(|index| self.rows[index].1["name"].as_str())
            .unwrap_or(&session.name)
            .to_owned();
        let tail = request.tail.map_or(protocol::ACTIVITY_TAIL, |tail| (tail as usize).min(protocol::ACTIVITY_TAIL));
        let mut sub = Subscription::new(key, name.clone(), session.run_id.filter(|run| !run.is_empty()), source::native(&name), tail);
        if let Some(index) = row {
            sub.listed = true;
            sub.row = Some(self.digests[index]);
            sub.busy = activity::busy(&self.rows[index].1);
        }
        self.subs.push(sub);
        let index = self.subs.len() - 1;
        self.open_journal();
        match self.start(index, after, false) {
            Started::Sent => {}
            // The client was told nothing it acts on, and would wait for a
            // first page that never comes: keep the subscription and read again.
            Started::Later => self.subs[index].start = Some((after, Instant::now() + START_RETRY, START_RETRY)),
            Started::Refused => {
                self.subs.pop();
            }
        }
    }

    /// One page of a followed session's Activity before the journal `seq`
    /// `before`, as `older` frames: at most `tail` rows (up to
    /// [`protocol::ACTIVITY_OLDER`]), where the next older page starts and
    /// whether one exists. The subscription, its cursor and its state stay as
    /// they are, and nothing is inspected. A failed read still ends: the
    /// client stops waiting and may ask again.
    fn older(&mut self, key: &str, before: i64, tail: Option<u32>) {
        let Some(index) = self.subs.iter().position(|sub| sub.key == key) else {
            self.out.error("bad_request", "an older Activity page belongs to a followed session", Some(topic::ACTIVITY), Some(key));
            return;
        };
        if before <= 0 {
            self.out.error("bad_request", "before must be positive", Some(topic::ACTIVITY), Some(key));
            return;
        }
        let limit = tail.map_or(protocol::ACTIVITY_TAIL, |tail| (tail as usize).clamp(1, protocol::ACTIVITY_OLDER));
        self.open_journal();
        let sub = &self.subs[index];
        // A DeepSeek or OpenCode window, a session without a journal or a
        // name that now holds another run has no earlier page.
        let none = || Ok((Vec::new(), before, false));
        let page = match (&self.journal, sub.native) {
            (Some(db), false) => match source::timeline(&sub.name) {
                Ok(Some(timeline)) if sub.run_id.as_deref().is_none_or(|run| run == timeline.run_id()) => {
                    timeline.events_before(db, before, limit)
                }
                Ok(_) => none(),
                Err(error) => Err(error),
            },
            _ => none(),
        };
        let (events, cursor, more) = match page {
            Ok(page) => page,
            Err(error) => {
                self.out.failure("journal_failed", &error, Some(topic::ACTIVITY), Some(key));
                (Vec::new(), before, true)
            }
        };
        let mut pages = activity::event_pages(events, activity::PAYLOAD_LIMIT);
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        let count = pages.len();
        for (page, events) in pages.into_iter().enumerate() {
            let last = page + 1 == count;
            let body = Body::Older(protocol::Older {
                topic: topic::ACTIVITY.into(),
                key: key.to_owned(),
                events,
                cursor,
                last,
                more: last && more,
            });
            if !self.out.put(&body) {
                return;
            }
        }
    }

    /// The first frames of a subscription, or its resumption after a gap.
    fn start(&mut self, index: usize, after: i64, resumed: bool) -> Started {
        let (minimum, maximum) = self.journal.as_ref().and_then(|db| source::bounds(db).ok()).unwrap_or((0, 0));
        let sub = &mut self.subs[index];
        // P1.5: a refresh still running read before these first frames.
        sub.inspecting = None;
        let gap = if resumed || after == 0 || sub.native {
            None
        } else if after > maximum {
            Some(reason::RESET)
        } else if after < minimum - 1 {
            Some(reason::RETENTION)
        } else {
            None
        };
        // The transcript is followed from before the first summary is read, so
        // no message falls between them; one in both is the same `event` ID.
        if !sub.native && sub.transcript.is_none() {
            if let Ok(Some(timeline)) = source::timeline(&sub.name) {
                if sub.run_id.as_deref().is_none_or(|run| run == timeline.run_id()) {
                    sub.transcript = timeline.transcript();
                }
            }
            sub.transcript_at = Instant::now() + TRANSCRIPT_RETRY;
        }
        let inspection = match source::inspect(&sub.name, if gap.is_some() { 0 } else { after }) {
            Ok(inspection) => inspection,
            Err(error) => {
                self.out.failure("inspect_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return Started::Later;
            }
        };
        if let (Some(seen), Some(wanted)) = (inspection["run_id"].as_str(), sub.run_id.as_deref()) {
            if seen != wanted {
                self.out.error(
                    "session_changed",
                    &format!("{} now runs {seen}, not {wanted}", sub.name),
                    Some(topic::ACTIVITY),
                    Some(&sub.key),
                );
                return Started::Refused;
            }
        }
        sub.refreshed = Instant::now();
        let mut events = inspection["events"].as_array().cloned().unwrap_or_default();
        if !sub.native {
            if let Ok(Some(timeline)) = source::timeline(&sub.name) {
                sub.journal = Some(timeline.journal().to_owned());
            }
        }
        if sub.native {
            sub.events = Some(catalog::digest(&Value::Array(events.clone())));
            send_window(&self.out, sub, events, 0);
        } else {
            let cursor = inspection["cursor"].as_i64().unwrap_or(after);
            if gap.is_some() || resumed || after == 0 {
                events.drain(..events.len().saturating_sub(sub.tail));
            }
            if let Some(why) = gap {
                self.out.put(&Body::Gap(protocol::Gap { topic: topic::ACTIVITY.into(), key: Some(sub.key.clone()), reason: why.into() }));
            }
            sub.more = after > 0 && gap.is_none() && events.len() >= protocol::ACTIVITY_READ;
            if gap.is_some() || resumed {
                send_window(&self.out, sub, events, cursor);
            } else {
                for page in activity::event_pages(events, activity::PAYLOAD_LIMIT) {
                    let page_cursor = page.last().and_then(|event| event["seq"].as_i64()).unwrap_or(cursor);
                    self.out.put(&Body::Events(protocol::Events {
                        topic: topic::ACTIVITY.into(),
                        key: sub.key.clone(),
                        events: page,
                        cursor: page_cursor,
                    }));
                }
            }
            sub.cursor = cursor;
        }
        send_state(&self.out, sub, &inspection);
        Started::Sent
    }
}

/// A conversation that changed (cleared context, a resumed run) is followed in
/// its own transcript from the next pulse.
fn follow_conversation(sub: &mut Subscription, timeline: &source::Timeline) {
    if sub.transcript.as_ref().is_some_and(|tail| tail.conversation() != timeline.conversation()) {
        sub.transcript = None;
        sub.transcript_at = Instant::now();
    }
}

/// An Activity event window as a snapshot: a DeepSeek or OpenCode window
/// (cursor 0), or a journal tail after a gap. Pages share the window's cursor.
fn send_window(out: &Out, sub: &mut Subscription, events: Vec<Value>, cursor: i64) -> bool {
    let pages = activity::event_pages(events, activity::PAYLOAD_LIMIT);
    let count = pages.len();
    for (page, items) in pages.into_iter().enumerate() {
        let body = Body::Snapshot(protocol::Snapshot {
            topic: topic::ACTIVITY.into(),
            key: Some(sub.key.clone()),
            items: Some(items),
            value: None,
            page: page as u32,
            last: page + 1 == count,
            cursor,
        });
        if !out.put(&body) {
            return false;
        }
    }
    true
}

/// `state` when the summary changed apart from observation times.
fn send_state(out: &Out, sub: &mut Subscription, inspection: &Value) {
    let mut value = activity::state_value(inspection);
    sub.busy = activity::busy(&value);
    let digest = activity::state_digest(&value);
    if sub.state == Some(digest) {
        return;
    }
    activity::shrink_state(&mut value, activity::PAYLOAD_LIMIT);
    sub.rev += 1;
    let body = Body::State(protocol::State { topic: topic::ACTIVITY.into(), key: sub.key.clone(), value, rev: sub.rev });
    if out.offer(&body, Some(format!("state|{}|{}", topic::ACTIVITY, sub.key))) {
        sub.state = Some(digest);
    }
}

/// The first subscription and every reconnect apply the requested cadence
/// before starting their read. Kept separate from worker dispatch for tests.
fn initial_process_subscription(processes: &mut processes::Processes, key: &str, name: &str,
    run: Option<String>, digest: Option<Digest32>, interval_ms: Option<u64>, now: Instant) -> processes::Job {
    let job = processes.subscribe(key, name, run, digest, now);
    processes.configure(key, interval_ms, false, now);
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_process_subscription_and_reconnect_apply_the_interval() {
        let mut processes = processes::Processes::new();
        for millis in [1_000, 2_500, 600_000] {
            let now = Instant::now();
            let job = initial_process_subscription(&mut processes, "run:r", "codex/p/t",
                Some("r".into()), None, Some(millis), now);
            processes.done(processes::Done { token: job.token, key: job.key, value: Some(Ok(
                json!({"run_id":"r", "processes":{}, "process_work":{}}))) });
            assert!(processes.pulse(now + Duration::from_millis(millis - 1)).is_empty());
            assert_eq!(processes.pulse(now + Duration::from_millis(millis)).len(), 1);
            assert!(processes.unsubscribe("run:r"));
        }
    }

    fn read(input: &[u8], pending: usize) -> Vec<String> {
        let (sender, receiver) = mpsc::channel();
        let ended = std::cell::Cell::new(false);
        read_controls(input, sender, &AtomicUsize::new(pending), || ended.set(true));
        assert!(ended.get());
        receiver
            .into_iter()
            .map(|input| match input {
                Input::Line(text) => text,
                Input::TooLong => "<too long>".into(),
                Input::Dropped => "<dropped>".into(),
                Input::Git(done) => format!("<git {}>", done.path),
                Input::Processes(done) => format!("<processes {}>", done.key),
                Input::Tasks(done) => format!("<tasks {}>", done.key),
                Input::Organization(_) => "<organization>".into(),
                Input::Team(_) => "<team>".into(),
                Input::Listing(_) => "<listing>".into(),
                Input::NativeRows(_) => "<native rows>".into(),
                Input::Rows(_) => "<rows>".into(),
                Input::Probe(_) => "<probe>".into(),
                Input::DriveFolders(_) => "<drive_folders>".into(),
                Input::Inspected(done) => format!("<inspected {}>", done.key),
                Input::Changed(_) => "<changed>".into(),
                Input::Counters(_) => "<counters>".into(),
                Input::End => "<end>".into(),
            })
            .collect()
    }

    /// Task 235: one listing runs at a time; one asked for meanwhile runs
    /// once after it, never twice and never lost.
    #[test]
    fn a_listing_asked_for_while_one_runs_follows_it_once() {
        let mut gate = ListingGate::default();
        assert!(gate.start(), "the first listing starts");
        assert!(!gate.start(), "a second waits");
        assert!(!gate.start(), "and a third is the same one");
        assert!(gate.done(), "it follows the running one");
        assert!(gate.start());
        assert!(!gate.done(), "nothing else was asked for");
        assert!(gate.start(), "the gate is free again");
    }

    fn lines(input: &[u8]) -> Vec<String> {
        read(input, 0)
    }

    /// The stream's stdout in a test.
    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        /// Waits up to 5 s for `text` in what was written.
        fn shows(&self, text: &str) -> bool {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if String::from_utf8_lossy(&self.0.lock().unwrap()).contains(text) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        }
    }

    const SLOW_INSPECTION: Duration = Duration::from_millis(800);

    fn slow_inspection(name: &str, _after: i64, _native: bool) -> Inspection {
        std::thread::sleep(SLOW_INSPECTION);
        Ok((json!({"name":name,"run_id":"r","status":"idle"}), None))
    }

    fn test_config(dir: &std::path::Path) -> Config {
        Config {
            home: dir.to_string_lossy().into_owned(),
            dir: dir.to_owned(),
            state: dir.to_owned(),
            selves: Vec::new(),
            peers: Vec::new(),
            base_peers: Vec::new(),
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        }
    }

    fn inspected(received: &mpsc::Receiver<Input>) -> InspectDone {
        match received.recv_timeout(Duration::from_secs(10)) {
            Ok(Input::Inspected(done)) => done,
            _ => panic!("the inspection did not return"),
        }
    }

    /// P1.5: a summary refresh runs on a thread. The loop answers a control
    /// line while a slow inspection runs; the result applies to the
    /// subscription that asked for it, and one for a replaced or renamed
    /// subscription is dropped.
    #[test]
    fn a_slow_inspection_leaves_the_loop_free_and_a_stale_result_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let sink = Sink::default();
        let writer = Writer::spawn(sink.clone(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, dir.path().to_owned());
        watch.inspect = slow_inspection;
        let subscription = || Subscription::new("run:r".into(), "claude/p/t".into(), Some("r".into()), false, 10);
        watch.subs.push(subscription());

        let started = Instant::now();
        watch.refresh(0);
        assert!(started.elapsed() < SLOW_INSPECTION / 2, "the refresh is handed off");
        assert!(watch.subs[0].inspecting.is_some());
        watch.control(r#"{"op":"ping","id":7}"#);
        assert!(sink.shows(r#""t":"pong""#), "the ping is answered");
        assert!(started.elapsed() < SLOW_INSPECTION, "while the inspection still runs");
        assert!(matches!(received.try_recv(), Err(mpsc::TryRecvError::Empty)));
        watch.inspected(inspected(&received));
        assert_eq!((watch.subs[0].rev, watch.subs[0].inspecting), (1, None), "its state was sent");
        assert!(sink.shows(r#""t":"state""#));

        // The subscription is replaced (subscribed again) while one runs.
        watch.refresh(0);
        watch.subs[0] = subscription();
        watch.inspected(inspected(&received));
        assert_eq!((watch.subs[0].rev, watch.subs[0].state), (0, None), "a replaced subscription's result is dropped");

        // The session is renamed while one runs: dropped, and read again.
        watch.refresh(0);
        watch.subs[0].name = "claude/p/renamed".into();
        watch.inspected(inspected(&received));
        assert_eq!(watch.subs[0].rev, 0);
        assert!(watch.subs[0].dirty && watch.subs[0].inspecting.is_none(), "a renamed session is read again");

        // Its first frames restart it: a refresh still running is dropped.
        watch.refresh(0);
        watch.subs[0].inspecting = None;
        watch.inspected(inspected(&received));
        assert_eq!(watch.subs[0].rev, 0, "a restarted subscription's result is dropped");
    }

    /// Task #11: a hint brings the pulse forward, never closer than
    /// `HINT_GAP` to the previous pulse; a failed bus is dropped.
    #[test]
    fn a_hint_runs_the_pulse_at_once_but_not_closer_than_the_gap() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let writer = Writer::spawn(Sink::default(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, _received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, dir.path().to_owned());
        let hint = || changes::Note::Hint(changes::Hint { paths: [PathBuf::from("read-marks.json")].into(), overflow: false });
        let now = Instant::now();
        watch.changed(hint(), now);
        assert_eq!(watch.pulse_due, now, "after a quiet time, at once");
        assert!(watch.hinted, "without the pulse's own debounce");
        (watch.pulsed_at, watch.pulse_due) = (Some(now), now + PULSE);
        watch.changed(hint(), now + Duration::from_millis(10));
        assert_eq!(watch.pulse_due, now + HINT_GAP, "not sooner than the gap after the last pulse");
        // Never log to the real state folder from a test.
        watch.bus_reported = true;
        watch.changed(changes::Note::Failed("gone".into()), now);
        assert!(watch.bus.is_none());
    }

    /// Task #12 (P2.1b): with a healthy bus the stamps are scanned every 2 s
    /// while the pulse keeps 250 ms; without one, or for 5 minutes after an
    /// overflow, on every pulse.
    #[test]
    fn the_stamps_are_scanned_every_two_seconds_while_the_bus_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let writer = Writer::spawn(Sink::default(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, _received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, dir.path().to_owned());
        let now = Instant::now();
        assert_eq!(watch.scan_interval(now), PULSE, "without a bus, every pulse");
        if !changes::supported() || !changes::enabled() {
            return;
        }
        watch.start_bus();
        assert!(watch.bus.is_some());
        assert_eq!(watch.scan_interval(now), SAFETY_SCAN);

        // A pulse with nothing else due: the next scan is 2 s away, the next pulse 250 ms.
        let now = Instant::now();
        (watch.listed, watch.listed_at, watch.probed_at) = (true, Some(now), Some(now));
        (watch.pulse_due, watch.scan_due) = (now, now);
        assert!(watch.turn(Err(mpsc::RecvTimeoutError::Timeout)));
        let after = Instant::now();
        assert!(watch.scan_due >= now + SAFETY_SCAN && watch.scan_due <= after + SAFETY_SCAN);
        assert!(watch.pulse_due <= after + PULSE);
        let scanned = watch.scan_due;
        watch.pulse_due = Instant::now();
        assert!(watch.turn(Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(watch.scan_due, scanned, "a pulse between scans does not scan");

        // A hint brings the scan forward.
        let hint = changes::Note::Hint(changes::Hint { paths: [PathBuf::from("read-marks.json")].into(), overflow: false });
        watch.pulsed_at = None;
        let now = Instant::now();
        watch.changed(hint, now);
        assert_eq!((watch.scan_due, watch.pulse_due), (now, now));

        // An overflow keeps the scan on every pulse for a while.
        watch.bus_reported = true;
        watch.changed(changes::Note::Hint(changes::Hint { paths: BTreeSet::new(), overflow: true }), now);
        assert_eq!(watch.scan_interval(now + Duration::from_secs(1)), PULSE);
        assert_eq!(watch.scan_interval(now + OVERFLOW_HOLD), SAFETY_SCAN);
    }

    /// Task #13: `HGS_WATCH_FULL_SECONDS` takes whole seconds from 1; 5 is
    /// the old behaviour; anything else is the 60 s default.
    #[test]
    fn the_sweep_interval_is_switchable() {
        assert_eq!(full_interval(None), Duration::from_secs(60));
        assert_eq!(full_interval(Some("5")), Duration::from_secs(5));
        assert_eq!(full_interval(Some(" 300 ")), Duration::from_secs(300));
        for wrong in ["0", "-5", "1.5", "", "soon"] {
            assert_eq!(full_interval(Some(wrong)), FULL_DEFAULT, "{wrong:?}");
        }
    }

    /// Task #13: the drift check names the rows a sweep listed otherwise, and
    /// their fields, but not volatile fields, rows that were changing, or
    /// rows only one side holds.
    #[test]
    fn drift_names_changed_rows_and_their_fields() {
        let row = |name: &str, run: &str, state: &str, sampled: f64| {
            json!({"name":name,"run_id":run,"session_state":state,"process_work":{"count":0,"sampled_at":sampled}})
        };
        let keyed = |rows: Vec<Value>| catalog::split(json!({"sessions":rows})).0;
        let kept = keyed(vec![row("c/p/a", "1", "cooling", 1.0), row("c/p/b", "2", "working", 1.0), row("c/p/c", "3", "cold", 1.0), row("c/p/d", "4", "cold", 1.0)]);
        let listed = keyed(vec![row("c/p/a", "1", "cooling", 9.0), row("c/p/b", "2", "user", 9.0), row("c/p/c", "3", "cooling", 9.0), row("c/p/e", "5", "cold", 9.0)]);
        let skip = BTreeSet::from(["c/p/c".to_owned()]);
        assert_eq!(drift(&kept, &listed, &skip), vec![(catalog::key(&listed[1].1), vec!["session_state".to_owned()])]);
        assert!(drift(&kept, &kept, &BTreeSet::new()).is_empty());
    }

    /// Task #13: the model state of the property test, read by the fake
    /// listing and derivation (function pointers cannot hold it).
    static MODEL: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    static LISTINGS: AtomicUsize = AtomicUsize::new(0);
    static DERIVED: AtomicUsize = AtomicUsize::new(0);
    static DRIFTS: AtomicUsize = AtomicUsize::new(0);

    /// The rows of the model's bindings as a listing makes them now: Cooling
    /// until `ttl` after `last_active`, Cold after.
    fn model_rows(wanted: Option<&BTreeSet<String>>) -> Vec<Value> {
        let root = MODEL.lock().unwrap().clone().expect("the model's folder");
        let wall = crate::state::now();
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap().flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with(".hgs-") || !file.ends_with(".json") {
                continue;
            }
            let Ok(record) = serde_json::from_slice::<Value>(&std::fs::read(entry.path()).unwrap_or_default()) else { continue };
            let name = record["name"].as_str().unwrap_or("").to_owned();
            if wanted.is_some_and(|wanted| !wanted.contains(&name)) {
                continue;
            }
            let (ttl, last) = (record["ttl"].as_f64().unwrap(), record["last_active"].as_f64().unwrap());
            let state = if wall - last < ttl { "cooling" } else { "cold" };
            rows.push(json!({"name":name,"run_id":record["run"],"tracked":true,"status":record["status"],
                "session_state":state,"last_active":last,"cache_ttl":{"seconds":ttl}}));
        }
        rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        rows
    }

    fn model_listing(_: &Config) -> std::result::Result<Value, String> {
        LISTINGS.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"sessions": model_rows(None)}))
    }

    fn model_derive(wanted: &BTreeSet<String>) -> std::result::Result<Option<(Vec<Value>, u64)>, String> {
        DERIVED.fetch_add(1, Ordering::SeqCst);
        Ok(Some((model_rows(Some(wanted)), 7)))
    }

    fn model_structure() -> Option<u64> {
        Some(7)
    }

    fn count_drift(_: &Value) {
        DRIFTS.fetch_add(1, Ordering::SeqCst);
    }

    /// Runs the loop as `run` does for `span`.
    fn drive(watch: &mut Watch, received: &mpsc::Receiver<Input>, span: Duration) {
        let until = Instant::now() + span;
        while Instant::now() < until {
            let wait = watch.deadline().min(until).saturating_duration_since(Instant::now()).min(Duration::from_millis(20));
            assert!(watch.turn(received.recv_timeout(wait)));
        }
    }

    /// Task #13 (P2.2), the property: whatever sequence of binding changes,
    /// new sessions and time passing, the rows the loop keeps by deriving
    /// only what changed (and what its time moved) equal a whole listing
    /// made from scratch, and a sweep then finds no drift.
    #[test]
    fn rows_kept_incrementally_equal_a_listing_from_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        *MODEL.lock().unwrap() = Some(root.clone());
        let write = |index: usize, status: &str, last: f64, ttl: f64| {
            let path = root.join(format!("{index:064x}.json"));
            let temp = root.join(format!(".hgs-{index}"));
            let record = json!({"name":format!("claude/p/s{index}"),"run":format!("r{index}"),"status":status,"last_active":last,"ttl":ttl});
            std::fs::write(&temp, record.to_string()).unwrap();
            std::fs::rename(&temp, &path).unwrap();
        };
        let wall = crate::state::now();
        for index in 0..6 {
            // Half are Cooling and turn Cold during the test.
            if index % 2 == 0 { write(index, "idle", wall - 1000.0, 300.0) } else { write(index, "idle", wall, 1.2) }
        }
        let config = test_config(&root);
        let writer = Writer::spawn(Sink::default(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, root.clone());
        watch.lister = model_listing;
        watch.derive = model_derive;
        watch.structure_of = model_structure;
        watch.drift_log = count_drift;
        watch.full = Duration::from_secs(3600);
        watch.list(Instant::now());
        drive(&mut watch, &received, Duration::from_millis(600));
        assert!(watch.listed);
        let first = LISTINGS.load(Ordering::SeqCst);

        // A fixed pseudo-random sequence: changed bindings, sessions turning
        // Cooling again, and two new sessions.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut sessions = 6;
        for step in 0..14 {
            let pick = (next() % sessions as u64) as usize;
            match (next() % 4, step) {
                (_, 5 | 10) => {
                    write(sessions, "new", crate::state::now(), 300.0);
                    sessions += 1;
                }
                (0 | 1, _) => write(pick, &format!("step-{step}"), wall - 1000.0, 300.0),
                _ => write(pick, "again", crate::state::now(), 0.9),
            }
            drive(&mut watch, &received, Duration::from_millis(350));
        }
        // Every Cooling row turns Cold within its TTL; let the loop follow.
        drive(&mut watch, &received, Duration::from_millis(2500));

        let scratch = catalog::split(json!({"sessions": model_rows(None)})).0;
        let masked = |rows: &[(String, Value)]| -> Vec<(String, Value)> {
            rows.iter().map(|(key, row)| (key.clone(), catalog::masked(row, &catalog::VOLATILE_ROW))).collect()
        };
        assert_eq!(masked(&watch.rows), masked(&scratch), "incremental rows differ from a listing from scratch");
        assert!(DERIVED.load(Ordering::SeqCst) > 0, "changed rows were derived alone");
        assert!(LISTINGS.load(Ordering::SeqCst) - first < 14, "not every change listed every row");

        // A sweep finds nothing to report.
        DRIFTS.store(0, Ordering::SeqCst);
        watch.schedule_sweep(Instant::now());
        drive(&mut watch, &received, Duration::from_millis(600));
        assert_eq!(DRIFTS.load(Ordering::SeqCst), 0, "a sweep found drift");
        assert_eq!(masked(&watch.rows), masked(&catalog::split(json!({"sessions": model_rows(None)})).0));
        *MODEL.lock().unwrap() = None;
    }

    static CHOOSER_DERIVES: AtomicUsize = AtomicUsize::new(0);

    fn chooser_rows(wanted: &BTreeSet<String>) -> std::result::Result<Option<(Vec<Value>, u64)>, String> {
        CHOOSER_DERIVES.fetch_add(1, Ordering::SeqCst);
        Ok(Some((wanted.iter().map(|name| json!({"name":name,"run_id":"r1","tracked":true,"phase":"input"})).collect(), 7)))
    }

    /// Task #14 (P2.4): a chooser the pane draws moves its output counter;
    /// once the counter stood still for 300 ms the row is derived once and
    /// reaches the client in under 0.5 s. A screen that keeps moving derives
    /// every 2 s, a moved structure lists every row, and a terminal without
    /// counters keeps the 5 s sweep.
    #[test]
    fn a_chooser_reaches_the_stream_in_under_half_a_second() {
        use crate::state::backend::Counters;
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let sink = Sink::default();
        let writer = Writer::spawn(sink.clone(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, dir.path().to_owned());
        watch.derive = chooser_rows;
        let row = json!({"name":"claude/p/t","run_id":"r1","tracked":true,"phase":"idle"});
        watch.rows = vec![(catalog::key(&row), row)];
        watch.digests = watch.rows.iter().map(|(_, row)| catalog::row_digest(row)).collect();
        (watch.listed, watch.pending, watch.structure) = (true, true, Some(7));
        let now = Instant::now();
        (watch.listed_at, watch.probed_at) = (Some(now), Some(now));
        watch.pulse_due = now + Duration::from_secs(60);
        watch.emit_catalog();
        assert!(sink.shows(r#""t":"snapshot""#));
        let counters = |structure: u64, count: u64| Some(Counters { structure, output: BTreeMap::from([("claude/p/t".to_string(), count)]) });

        assert_eq!(watch.sweep_interval(), FULL_DEFAULT, "nothing known yet");
        watch.counted(counters(1, 10), Instant::now());
        assert!(watch.stale.is_empty() && watch.screens_due.is_none(), "the first counters derive nothing");
        assert_eq!(watch.sweep_interval(), FULL_DEFAULT);

        // The chooser is drawn in three pieces.
        let drawn = Instant::now();
        for count in [11, 12, 13] {
            assert!(watch.turn(Ok(Input::Counters(counters(1, count)))));
            std::thread::sleep(Duration::from_millis(30));
        }
        let latency = loop {
            let wait = watch.deadline().saturating_duration_since(Instant::now()).min(Duration::from_millis(5));
            assert!(watch.turn(received.recv_timeout(wait)));
            if String::from_utf8_lossy(&sink.0.lock().unwrap()).contains(r#""t":"delta""#) {
                break drawn.elapsed();
            }
            assert!(drawn.elapsed() < Duration::from_secs(3), "no delta");
        };
        assert!(latency >= SETTLE && latency < Duration::from_millis(500), "the chooser came after {latency:?}");
        assert_eq!(CHOOSER_DERIVES.load(Ordering::SeqCst), 1, "a chooser drawn in pieces derives once");

        // A screen that keeps moving for 2.5 s derives once, at 2 s.
        CHOOSER_DERIVES.store(0, Ordering::SeqCst);
        let moving = Instant::now();
        let mut count = 20;
        while moving.elapsed() < Duration::from_millis(2500) {
            count += 1;
            assert!(watch.turn(Ok(Input::Counters(counters(1, count)))));
            let until = Instant::now() + Duration::from_millis(100);
            while Instant::now() < until {
                let wait = watch.deadline().min(until).saturating_duration_since(Instant::now()).min(Duration::from_millis(5));
                assert!(watch.turn(received.recv_timeout(wait)));
            }
        }
        assert_eq!(CHOOSER_DERIVES.load(Ordering::SeqCst), 1, "a busy screen derives every 2 s");

        // A moved structure (a session started, ended or was renamed) lists every row.
        assert!(watch.listing_due.is_none());
        watch.counted(counters(2, count), Instant::now());
        assert!(watch.listing_due.is_some());

        // A terminal without counters keeps the earlier 5 s sweep.
        watch.counted(None, Instant::now());
        assert_eq!(watch.sweep_interval(), LEGACY_FULL);
        assert!(watch.screens.is_empty() && watch.screens_due.is_none());
    }

    fn working_rows(wanted: &BTreeSet<String>) -> std::result::Result<Option<(Vec<Value>, u64)>, String> {
        Ok(Some((wanted.iter().map(|name| json!({"name":name,"run_id":"r1","status":"working"})).collect(), 7)))
    }

    /// Task #11 (P2.1a): a binding replaced by rename reaches the client as a
    /// catalog delta within 300 ms through the change bus alone: the timer
    /// pulse, the listing and the probe are all far away.
    #[cfg(windows)]
    #[test]
    fn a_binding_written_by_rename_produces_a_catalog_delta_within_300_ms() {
        if !changes::enabled() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let name = "claude/p/bus";
        let binding = root.join(format!("{}.json", "c".repeat(64)));
        std::fs::write(&binding, json!({"name":name,"status":"idle"}).to_string()).unwrap();
        let config = test_config(&root);
        let sink = Sink::default();
        let writer = Writer::spawn(sink.clone(), protocol::QUEUE_BYTES, Duration::from_secs(3600), || {});
        let (results, received) = mpsc::channel();
        let mut watch = Watch::with_root(&config, writer, results, root.clone());
        watch.derive = working_rows;
        let row = json!({"name":name,"run_id":"r1","status":"idle"});
        watch.rows = vec![(catalog::key(&row), row)];
        watch.digests = watch.rows.iter().map(|(_, row)| catalog::row_digest(row)).collect();
        (watch.listed, watch.pending, watch.structure) = (true, true, Some(7));
        let now = Instant::now();
        (watch.listed_at, watch.probed_at) = (Some(now), Some(now));
        watch.pulse_due = now + Duration::from_secs(60);
        watch.emit_catalog();
        assert!(sink.shows(r#""t":"snapshot""#));
        watch.start_bus();
        assert!(watch.bus.is_some(), "the bus watches the state folder");

        let temp = root.join(".hgs-next");
        std::fs::write(&temp, json!({"name":name,"status":"working"}).to_string()).unwrap();
        let written = Instant::now();
        std::fs::rename(&temp, &binding).unwrap();
        let latency = loop {
            let wait = watch.deadline().saturating_duration_since(Instant::now()).min(Duration::from_millis(10));
            assert!(watch.turn(received.recv_timeout(wait)));
            let text = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
            if text.contains(r#""t":"delta""#) {
                assert!(text.contains("working"), "{text}");
                break written.elapsed();
            }
            assert!(written.elapsed() < Duration::from_secs(3), "no delta: {text}");
        };
        // The timer pulse was a minute away: only the hint ran the pulse.
        assert!(latency < Duration::from_millis(300), "the delta came {latency:?} after the write");
    }

    #[test]
    fn control_lines_are_framed_bounded_and_end_at_eof() {
        assert_eq!(lines(b"{\"op\":\"ping\"}\n\n{\"op\":\"ping\",\"id\":2}"), ["{\"op\":\"ping\"}", "", "{\"op\":\"ping\",\"id\":2}", "<end>"]);
        let mut long = vec![b'x'; protocol::CONTROL_LINE_BYTES + 10];
        long.extend_from_slice(b"\nnext\n");
        assert_eq!(lines(&long), ["<too long>", "next", "<end>"]);
        assert_eq!(lines(b""), ["<end>"]);
        // A client that writes faster than lines are handled loses the excess.
        assert_eq!(read(b"a\nb\nc", PENDING_CONTROLS), ["<dropped>", "<end>"]);
    }
}
