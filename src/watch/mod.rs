//! `hgs [@host] watch --json`: one long-lived NDJSON event stream per computer
//! (ABI `watch-v1`, frames in `crates/watch-protocol`). The catalog topic
//! carries the rows of `hgs ls --json --local --no-archives`; Activity carries
//! what `hgs inspect` returns for subscribed sessions, and git what `hgs
//! git-status --json` returns for subscribed checkout folders. Change detection
//! is a 250 ms pulse over cheap stamps and the journal's data version, on
//! `std::thread` only. A changed binding or journal row derives only its
//! session's row again; a 5 s probe watches what no file shows and the whole
//! listing runs every 5 s. See "Event stream" in docs/reference.md.
mod activity;
mod catalog;
mod git;
mod processes;
mod stamps;
mod organization;
mod tasks;
mod writer;

use crate::cli::{Error, Result};
use crate::config::Config;
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
/// The whole listing runs at least this often, for what neither a file nor
/// the probe shows: terminal choosers and status lines of sessions that have
/// reported hooks, and native transcripts between hook events. 15 s would
/// cut the watcher's CPU on a busy computer but show such a chooser up to 15 s
/// late; the owner chose 5 s (2026-10-27, zerus-604d).
const FULL: Duration = Duration::from_secs(5);
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
    End,
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
    let mut next_pulse = Instant::now() + PULSE;
    loop {
        let now = Instant::now();
        let deadline = [watch.listing_due, watch.refresh_due].into_iter().flatten().fold(next_pulse, Instant::min);
        let input = received.recv_timeout(deadline.saturating_duration_since(now));
        if matches!(input, Ok(Input::Line(_) | Input::TooLong)) {
            pending.fetch_sub(1, Ordering::SeqCst);
        }
        if watch.out.writer.stopped() {
            return Ok(0);
        }
        match input {
            Ok(Input::Line(text)) => watch.control(&text),
            Ok(Input::TooLong) => watch.out.error(
                "bad_request",
                &format!("control operations are at most {} bytes", protocol::CONTROL_LINE_BYTES),
                None,
                None,
            ),
            Ok(Input::Dropped) => watch.out.error(
                "bad_request",
                &format!("more than {PENDING_CONTROLS} control operations were waiting; later ones were dropped"),
                None,
                None,
            ),
            Ok(Input::Git(done)) => watch.git_done(done),
            Ok(Input::Processes(done)) => watch.processes_done(done),
            Ok(Input::Tasks(done)) => watch.tasks_done(done),
            Ok(Input::Organization(done)) => watch.organization_done(done),
            Ok(Input::End) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if watch.out.writer.stopped() {
            return Ok(0);
        }
        let now = Instant::now();
        if now >= next_pulse {
            next_pulse = now + PULSE;
            watch.pulse(now);
        }
        if watch.listing_due.is_some_and(|due| now >= due) {
            watch.list(now);
        }
        if watch.refresh_due.is_some_and(|due| now >= due) {
            watch.refresh_rows(now);
        }
        watch.emit_catalog();
        watch.resync_activity();
        watch.flush_git();
        watch.flush_processes();
        watch.flush_tasks();
        watch.flush_organization();
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
    // Row refreshes between listings.
    stale: BTreeSet<String>,
    stale_native: bool,
    refresh_due: Option<Instant>,
    refreshed_at: Option<Instant>,
    refresh_cost: Duration,
    probed_at: Option<Instant>,
    // Activity.
    journal: Option<rusqlite::Connection>,
    data_version: Option<i64>,
    journal_max: i64,
    subs: Vec<Subscription>,
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
    writers: Writers,
}

impl<'a> Watch<'a> {
    fn new(config: &'a Config, writer: Writer, git_results: mpsc::Sender<Input>) -> Self {
        let root = source::state_root();
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
            stale: BTreeSet::new(),
            stale_native: false,
            refresh_due: None,
            refreshed_at: None,
            refresh_cost: Duration::ZERO,
            probed_at: None,
            journal: None,
            data_version: None,
            journal_max: 0,
            subs: Vec::new(),
            git: git::Git::new(),
            git_pool: None,
            git_results,
            processes: processes::Processes::new(),
            processes_pool: None,
            tasks: tasks::Tasks::new(),
            tasks_pool: None,
            organization: organization::Organization::new(),
            organization_pool: None,
            writers: Writers::None,
        }
    }

    fn hello(&mut self) {
        let hello = protocol::Hello {
            machine_id: crate::swarm::local_node_id(self.config).ok(),
            host: self.config.host().into(),
            hgs_version: env!("CARGO_PKG_VERSION").into(),
            stream: uuid::Uuid::new_v4().to_string(),
            heartbeat_ms: protocol::HEARTBEAT_MS,
            max_frame_bytes: protocol::MAX_FRAME_BYTES,
            topics: [topic::CATALOG, topic::CATALOG_META, topic::ACTIVITY, topic::GIT, topic::PROCESSES, topic::TASKS, topic::ORGANIZATION]
                .map(str::to_owned)
                .to_vec(),
            limits: protocol::Limits::default(),
        };
        self.out.put(&Body::Hello(hello));
        self.open_journal();
    }

    // ---- Catalog -----------------------------------------------------------

    fn schedule(&mut self, at: Instant) {
        let gap = LISTING_GAP.max(self.listing_cost * 4);
        let due = self.listed_at.map_or(at, |started| at.max(started + gap));
        self.listing_due = Some(self.listing_due.map_or(due, |current| current.min(due)));
    }

    fn schedule_refresh(&mut self, at: Instant) {
        let gap = LISTING_GAP.max(self.refresh_cost * 4);
        let due = self.refreshed_at.map_or(at, |started| at.max(started + gap));
        self.refresh_due = Some(self.refresh_due.map_or(due, |current| current.min(due)));
    }

    /// Re-runs the `ls --json --local --no-archives` builder.
    fn list(&mut self, now: Instant) {
        self.listing_due = None;
        self.listed_at = Some(now);
        // The listing covers every row refresh and probe asked for so far.
        self.probed_at = Some(now);
        self.stale.clear();
        self.stale_native = false;
        self.refresh_due = None;
        // The structure is taken first: a change during the listing lists again.
        self.structure = source::structure_now().ok();
        match crate::cli::local_listing(self.config) {
            Ok(listing) => {
                let (rows, meta) = catalog::split(listing);
                self.digests = rows.iter().map(|(_, row)| catalog::row_digest(row)).collect();
                self.rows = rows;
                self.meta = meta;
                self.listed = true;
                self.pending = true;
            }
            Err(error) => self.out.failure("listing_failed", &error.message, Some(topic::CATALOG), None),
        }
        self.listing_cost = now.elapsed();
    }

    /// Derives again the rows whose inputs changed since they were listed; a
    /// whole listing follows when only it can tell.
    fn refresh_rows(&mut self, now: Instant) {
        self.refresh_due = None;
        self.refreshed_at = Some(now);
        let wanted = std::mem::take(&mut self.stale);
        let native = std::mem::take(&mut self.stale_native);
        match self.derive(&wanted, native) {
            Ok(true) => {}
            Ok(false) => self.schedule(now),
            Err(error) => {
                self.out.failure("listing_failed", &error, Some(topic::CATALOG), None);
                self.schedule(now);
            }
        }
        self.refresh_cost = now.elapsed();
    }

    /// Replaces the rows of the live tracked sessions in `wanted` and, with
    /// `native`, the DeepSeek and OpenCode rows. False when a whole listing
    /// must decide: the terminal changed, a session is no longer live under
    /// its binding, or a row would change its key or its place.
    fn derive(&mut self, wanted: &BTreeSet<String>, native: bool) -> std::result::Result<bool, String> {
        if !self.listed {
            return Ok(false);
        }
        let mut rows = Vec::new();
        if !wanted.is_empty() {
            let Some((derived, structure)) = source::rows(wanted)? else { return Ok(false) };
            if Some(structure) != self.structure {
                return Ok(false);
            }
            rows = derived;
        }
        if native {
            let derived = source::native_rows()?;
            let listed: BTreeSet<&str> = self
                .rows
                .iter()
                .filter(|(_, row)| row["name"].as_str().is_some_and(source::native))
                .map(|(key, _)| key.as_str())
                .collect();
            let keys: Vec<String> = derived.iter().map(catalog::key).collect();
            if keys.len() != listed.len() || keys.iter().any(|key| !listed.contains(key.as_str())) {
                return Ok(false);
            }
            rows.extend(derived);
        }
        let mut updates = Vec::with_capacity(rows.len());
        for row in rows {
            let key = catalog::key(&row);
            let Some(index) = self.rows.iter().position(|(listed, _)| *listed == key) else { return Ok(false) };
            if self.rows[index].1["name"] != row["name"] {
                return Ok(false);
            }
            updates.push((index, row));
        }
        for (index, row) in updates {
            self.digests[index] = catalog::row_digest(&row);
            self.rows[index].1 = row;
        }
        self.pending = true;
        Ok(true)
    }

    /// What no file shows: the terminal's structure, agent processes and
    /// their work, sessions without a hook event yet and DeepSeek and
    /// OpenCode hosts. The listing's metrics and presence are refreshed too.
    fn probe(&mut self, now: Instant) {
        self.probed_at = Some(now);
        if !self.listed {
            // The last listing failed: try again.
            self.schedule(now);
            return;
        }
        let rows: Vec<&Value> = self.rows.iter().map(|(_, row)| row).collect();
        let probe = match source::probe(&rows) {
            Ok(probe) if Some(probe.structure) == self.structure => probe,
            // A changed terminal, or one this probe cannot read: list it whole.
            _ => {
                self.schedule(now);
                return;
            }
        };
        let native = self.rows.iter().any(|(_, row)| row["name"].as_str().is_some_and(source::native));
        if !probe.stale.is_empty() || native {
            self.stale.extend(probe.stale);
            self.stale_native |= native;
            self.schedule_refresh(now);
        }
        // `metrics` and `desktop_presence` as the listing reports them now.
        self.meta["metrics"] = crate::metrics::snapshot(&self.config.state);
        match crate::presence::listing(&self.config.state, crate::presence::now()) {
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

    fn pulse(&mut self, now: Instant) {
        let journal = self.poll_journal();
        let scan = stamps::scan(&self.root);
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
        if whole {
            self.schedule(now + DEBOUNCE);
        } else if !names.is_empty() {
            self.stale.extend(names);
            self.schedule_refresh(now + DEBOUNCE);
        }
        if self.probed_at.is_none_or(|at| now >= at + PROBE) {
            self.probe(now);
        }
        if self.listed_at.is_none_or(|at| now >= at + FULL) {
            self.schedule(now);
        }
        self.activity(now);
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
    }

    fn open_journal(&mut self) {
        if self.journal.is_some() {
            return;
        }
        self.journal = source::journal();
        if let Some(db) = &self.journal {
            self.data_version = source::data_version(db).ok();
            self.journal_max = source::bounds(db).map_or(0, |(_, max)| max);
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
        self.data_version = Some(version);
        let Ok((_, max)) = source::bounds(db) else { return Journal::Unknown };
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
        for index in 0..self.subs.len() {
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

    /// Re-inspects one session; sends `state` when its summary changed, and
    /// for DeepSeek and OpenCode a replacement of their event window.
    fn refresh(&mut self, index: usize) {
        let after = if self.subs[index].native { 0 } else { self.journal_max.max(self.subs[index].cursor) };
        let sub = &mut self.subs[index];
        sub.refreshed = Instant::now();
        sub.dirty = false;
        let inspection = match source::inspect(&sub.name, after) {
            Ok(inspection) => inspection,
            Err(error) => {
                self.out.failure("inspect_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return;
            }
        };
        if inspection["run_id"].as_str().zip(sub.run_id.as_deref()).is_some_and(|(seen, wanted)| seen != wanted) {
            return;
        }
        if !sub.native {
            if let Ok(Some(timeline)) = source::timeline(&sub.name) {
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
            self.start(index, 0, true);
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

    fn subscribe_tasks(&mut self, project: Option<&str>) {
        let Some(project) = project.filter(|project| {
            !project.is_empty() && project.len() <= tasks::PROJECT_BYTES && !project.chars().any(char::is_control)
        }) else {
            self.out.error("bad_request", "a tasks subscription names its project ID", Some(topic::TASKS), None);
            return;
        };
        if !self.tasks.contains(project) && self.tasks.len() >= protocol::TASKS_SUBSCRIPTIONS {
            self.out.error(
                "too_many_subscriptions",
                &format!("a stream follows the tasks of at most {} projects", protocol::TASKS_SUBSCRIPTIONS),
                Some(topic::TASKS),
                Some(project),
            );
            return;
        }
        let job = self.tasks.subscribe(project, Instant::now());
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
                    self.tasks_done(tasks::Done { token: job.token, key: job.key, value: Some(Err("Task list worker is unavailable".into())) });
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
            topic::TASKS => return self.subscribe_tasks(request.project.as_deref()),
            topic::ORGANIZATION => return self.subscribe_organization(),
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
        if !self.start(index, after, false) {
            self.subs.pop();
        }
    }

    /// The first frames of a subscription, or its resumption after a gap.
    /// False when it cannot follow the requested session.
    fn start(&mut self, index: usize, after: i64, resumed: bool) -> bool {
        let (minimum, maximum) = self.journal.as_ref().and_then(|db| source::bounds(db).ok()).unwrap_or((0, 0));
        let sub = &mut self.subs[index];
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
                self.out.error("inspect_failed", &error, Some(topic::ACTIVITY), Some(&sub.key));
                return false;
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
                return false;
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
        true
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
                Input::End => "<end>".into(),
            })
            .collect()
    }

    fn lines(input: &[u8]) -> Vec<String> {
        read(input, 0)
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
