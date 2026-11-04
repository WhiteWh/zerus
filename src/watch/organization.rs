//! The `organization` topic: the shared project catalog as `hgs swarm get`
//! shows it, as keyed rows a client applies like the catalog's. A worker
//! thread may bootstrap a missing catalog only when Qt settings are definitely
//! absent. Initialized reads stay lock-free, never saving or supervising the
//! catalog worker. The catalog is read again when a stat of its files changes and at
//! least every 30 s, at most once a second. The first rows go out as a paged
//! `snapshot`; later changes as one `delta` of the changed rows, or a new
//! snapshot when they would not fit one frame.
//!
//! Rows:
//! - `organization`: the snapshot without `versions`, `written` and the
//!   projects' contents; its `organization.projects` lists the project IDs in
//!   order. It carries `digest`, which identifies the catalog, and `worker`.
//! - `project:<id>`: the project without `sessions`, with `session_chunks`,
//!   the number of its member rows.
//! - `members:<id>:<n>`: `{sessions}`, at most [`CHUNK`] sessions in order.
//! - `version:<token>`: one `versions` entry, the operation IDs of a key.
use super::catalog::{self, Digest32};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// The catalog is read at least this often, for what its files do not show,
/// such as the worker's state.
pub(super) const SAFETY: Duration = Duration::from_secs(30);
/// Reads start at least this far apart.
pub(super) const GAP: Duration = Duration::from_secs(1);
/// Sessions per member row.
pub(super) const CHUNK: usize = 1000;

/// One read for the worker.
pub(super) struct Job {
    pub token: u64,
    /// Only an explicit subscription may try a first-use bootstrap.
    bootstrap: bool,
    cancelled: Arc<AtomicBool>,
}

/// A finished read.
pub(super) struct Done {
    pub token: u64,
    /// The snapshot, an error of it, or None when the subscription ended
    /// before the read started.
    pub value: Option<Result<Value, String>>,
}

/// Runs one read with `snapshot` (the catalog as `swarm get` shows it).
pub(super) fn read(job: Job, snapshot: impl Fn() -> Result<Value, String>) -> Done {
    let Job { token, cancelled, .. } = job;
    if cancelled.load(Ordering::SeqCst) {
        return Done { token, value: None };
    }
    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&snapshot))
        .unwrap_or_else(|_| Err("the organization read failed".into()));
    Done { token, value: Some(value) }
}

/// The worker thread that reads the catalog with this computer's configuration.
pub(super) struct Pool {
    jobs: mpsc::SyncSender<Job>,
}

impl Pool {
    pub(super) fn spawn(finished: impl Fn(Done) + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("hgs-watch-organization".into())
            .spawn(move || {
                let config = crate::config::Config::load().map_err(|error| error.to_string());
                let snapshot = |first| match &config {
                    Ok(config) => crate::swarm::organization(config, first).map_err(|error| format!("{error:#}")),
                    Err(error) => Err(error.clone()),
                };
                while let Ok(job) = queue.recv() {
                    let first = job.bootstrap;
                    finished(read(job, || snapshot(first)));
                }
            })
            .expect("start the organization worker");
        Self { jobs }
    }

    /// Reject without blocking the watch loop or losing the original token.
    pub(super) fn run(&self, job: Job) -> Result<(), mpsc::TrySendError<Job>> {
        self.jobs.try_send(job)
    }
}

/// The rows of one snapshot, by key.
pub(super) fn rows(snapshot: &Value) -> BTreeMap<String, Value> {
    let mut rows = BTreeMap::new();
    let mut summary = snapshot.clone();
    if let Some(object) = summary.as_object_mut() {
        object.remove("versions");
        object.remove("written");
    }
    let projects = snapshot["organization"]["projects"].as_array().cloned().unwrap_or_default();
    let mut ids = Vec::with_capacity(projects.len());
    for mut project in projects {
        let Some(id) = project["id"].as_str().map(str::to_owned) else { continue };
        let sessions = project.as_object_mut().and_then(|object| object.remove("sessions"));
        let sessions = sessions.and_then(|sessions| sessions.as_array().cloned()).unwrap_or_default();
        let chunks: Vec<&[Value]> = sessions.chunks(CHUNK).collect();
        project["session_chunks"] = json!(chunks.len());
        for (index, chunk) in chunks.into_iter().enumerate() {
            rows.insert(format!("members:{id}:{index}"), json!({"sessions": chunk}));
        }
        rows.insert(format!("project:{id}"), project);
        ids.push(Value::String(id));
    }
    if summary["organization"].is_object() {
        summary["organization"]["projects"] = Value::Array(ids);
    }
    rows.insert("organization".into(), summary);
    for (token, operations) in snapshot["versions"].as_object().into_iter().flatten() {
        rows.insert(format!("version:{token}"), operations.clone());
    }
    rows
}

/// What to send next.
#[derive(Debug, PartialEq)]
pub(super) enum Plan {
    /// Every row, in pages for `snapshot` frames.
    Snapshot(Vec<Vec<(String, Value)>>),
    /// The changed rows and the removed keys, for one `delta` frame.
    Delta { upsert: Vec<(String, Value)>, remove: Vec<String> },
}

/// The organization subscription of one stream and when it is read.
pub(super) struct Organization {
    subscribed: bool,
    token: u64,
    cancelled: Arc<AtomicBool>,
    running: bool,
    again: bool,
    started: Instant,
    due: Option<Instant>,
    /// An initial read not yet admitted still has its original bootstrap choice.
    retry_bootstrap: bool,
    files: Vec<PathBuf>,
    stamp: Option<u64>,
    /// The rows of the latest read, with their digests.
    current: Option<BTreeMap<String, (Value, Digest32)>>,
    /// The digests of the rows the client holds; None sends a snapshot.
    sent: Option<BTreeMap<String, Digest32>>,
    rev: i64,
}

fn encoded(key: &str, row: &Value) -> usize {
    key.len() + row.to_string().len() + 24
}

impl Organization {
    pub(super) fn new() -> Self {
        Self {
            subscribed: false,
            token: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
            running: false,
            again: false,
            started: Instant::now(),
            due: None,
            retry_bootstrap: false,
            files: Vec::new(),
            stamp: None,
            current: None,
            sent: None,
            rev: 0,
        }
    }

    fn job(&mut self, now: Instant, bootstrap: bool) -> Job {
        self.token += 1;
        self.running = true;
        self.again = false;
        self.started = now;
        self.due = None;
        Job { token: self.token, bootstrap, cancelled: Arc::clone(&self.cancelled) }
    }

    /// Follows the organization, watching `files`; following it again sends
    /// every row again.
    pub(super) fn subscribe(&mut self, files: Vec<PathBuf>, stamp: u64, now: Instant) -> Job {
        self.unsubscribe();
        self.subscribed = true;
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.files = files;
        self.stamp = Some(stamp);
        self.job(now, true)
    }

    pub(super) fn unsubscribe(&mut self) -> bool {
        let was = std::mem::take(&mut self.subscribed);
        self.cancelled.store(true, Ordering::SeqCst);
        self.running = false;
        self.retry_bootstrap = false;
        self.current = None;
        self.sent = None;
        was
    }

    /// The read whose time came: changed files, or the safety interval.
    pub(super) fn pulse(&mut self, now: Instant, stamp: impl Fn(&[PathBuf]) -> u64) -> Option<Job> {
        if !self.subscribed {
            return None;
        }
        let current = stamp(&self.files);
        if self.stamp != Some(current) {
            self.stamp = Some(current);
            if self.running {
                self.again = true;
            } else if self.due.is_none() {
                self.due = Some(now.max(self.started + GAP));
            }
        }
        let due = self.due.is_some_and(|at| now >= at) || now >= self.started + SAFETY;
        if self.running || !due {
            return None;
        }
        let bootstrap = std::mem::take(&mut self.retry_bootstrap);
        Some(self.job(now, bootstrap))
    }

    /// Queue pressure is not a read failure. Retry only the still-current job.
    pub(super) fn defer(&mut self, job: Job, now: Instant) {
        if self.subscribed && job.token == self.token {
            self.running = false;
            self.due = Some(now + GAP);
            self.retry_bootstrap = job.bootstrap;
        }
    }

    /// Takes a finished read; an error is returned for the client.
    pub(super) fn done(&mut self, done: Done) -> Option<String> {
        if !self.subscribed || done.token != self.token {
            return None;
        }
        self.running = false;
        if std::mem::take(&mut self.again) {
            self.due = Some(self.started + GAP);
        }
        match done.value? {
            Ok(snapshot) => {
                let rows = rows(&snapshot);
                self.current = Some(rows.into_iter().map(|(key, row)| {
                    let digest = catalog::digest(&row);
                    (key, (row, digest))
                }).collect());
                None
            }
            Err(error) => Some(error),
        }
    }

    /// The frames that bring the client to the latest read; None when it is
    /// current. A delta larger than `limit` becomes a snapshot.
    pub(super) fn plan(&self, limit: usize) -> Option<Plan> {
        let current = self.current.as_ref()?;
        let snapshot = || {
            let rows: Vec<(String, Value)> = current.iter().map(|(key, (row, _))| (key.clone(), row.clone())).collect();
            let pages = catalog::pages(rows, |(key, row)| encoded(key, row), limit);
            Plan::Snapshot(if pages.is_empty() { vec![Vec::new()] } else { pages })
        };
        let Some(sent) = &self.sent else { return Some(snapshot()) };
        let upsert: Vec<(String, Value)> = current
            .iter()
            .filter(|(key, (_, digest))| sent.get(*key) != Some(digest))
            .map(|(key, (row, _))| (key.clone(), row.clone()))
            .collect();
        let remove: Vec<String> = sent.keys().filter(|key| !current.contains_key(*key)).cloned().collect();
        if upsert.is_empty() && remove.is_empty() {
            return None;
        }
        let size: usize = upsert.iter().map(|(key, row)| encoded(key, row)).sum::<usize>()
            + remove.iter().map(|key| key.len() + 32).sum::<usize>();
        Some(if size > limit { snapshot() } else { Plan::Delta { upsert, remove } })
    }

    /// The client now holds the latest read.
    pub(super) fn sent(&mut self) {
        self.sent = self
            .current
            .as_ref()
            .map(|current| current.iter().map(|(key, (_, digest))| (key.clone(), *digest)).collect());
    }

    /// A send that stopped partway: the next plan is a snapshot.
    pub(super) fn resend(&mut self) {
        self.sent = None;
    }

    /// The cursor of the next frames.
    pub(super) fn next_rev(&mut self) -> i64 {
        self.rev += 1;
        self.rev
    }
}

/// Rebuilds the `swarm get` snapshot (without `written`) from rows, as a
/// client does; for tests of the encoding.
#[cfg(test)]
fn rebuild(rows: &BTreeMap<String, Value>) -> Value {
    let mut snapshot = rows["organization"].clone();
    let ids = snapshot["organization"]["projects"].as_array().cloned().unwrap_or_default();
    let projects: Vec<Value> = ids
        .iter()
        .filter_map(Value::as_str)
        .map(|id| {
            let mut project = rows[&format!("project:{id}")].clone();
            let chunks = project["session_chunks"].as_u64().unwrap_or(0);
            let sessions: Vec<Value> = (0..chunks)
                .flat_map(|index| rows[&format!("members:{id}:{index}")]["sessions"].as_array().cloned().unwrap_or_default())
                .collect();
            let object = project.as_object_mut().unwrap();
            object.remove("session_chunks");
            object.insert("sessions".into(), Value::Array(sessions));
            project
        })
        .collect();
    snapshot["organization"]["projects"] = Value::Array(projects);
    let versions: serde_json::Map<String, Value> = rows
        .iter()
        .filter_map(|(key, row)| Some((key.strip_prefix("version:")?.to_owned(), row.clone())))
        .collect();
    snapshot["versions"] = Value::Object(versions);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn snapshot(sessions: usize, moved: bool) -> Value {
        let members = |prefix: &str, count: usize| -> Vec<Value> {
            (0..count).map(|index| json!(format!("workstation\n{prefix}/{index}"))).collect()
        };
        let mut alpha = members("codex/alpha", sessions);
        let mut beta = members("claude/beta", 2);
        if moved {
            beta.push(alpha.pop().unwrap());
        }
        json!({
            "schema": 1, "node_id": "node-1", "swarm_id": "swarm-1", "digest": if moved { "d2" } else { "d1" },
            "initialized": true, "conflicts": [], "preferences": {"mobile_delivery": "all"},
            "machines": [{"id": "node-1", "name": "workstation", "connection": null, "local": true}],
            "peers": {}, "worker": {"running": true, "current": true}, "written": {},
            "organization": {"version": 2, "default_project": "ungrouped", "project_order": ["alpha", "beta"],
                "projects": [
                    {"id": "alpha", "name": "Alpha", "color": "#336699", "accessible": true, "folders": [], "sessions": alpha},
                    {"id": "beta", "name": "Beta", "color": "#996633", "accessible": true, "folders": [], "sessions": beta}
                ]},
            "versions": {"project:alpha:name": ["op-1"], "member:x": [if moved { "op-3" } else { "op-2" }]}
        })
    }

    fn without_written(mut value: Value) -> Value {
        value.as_object_mut().unwrap().remove("written");
        value
    }

    #[test]
    fn unsubscribed_callback_cannot_cross_the_detail_fence() {
        let mut state = Organization::new();
        let old = state.subscribe(Vec::new(), 0, Instant::now());
        state.unsubscribe();
        let current = state.subscribe(Vec::new(), 0, Instant::now());
        assert!(old.token < current.token);
        state.done(Done { token: old.token, value: Some(Ok(snapshot(1, false))) });
        assert!(state.plan(524288).is_none());
        state.done(Done { token: current.token, value: Some(Ok(snapshot(1, false))) });
        assert!(state.plan(524288).is_some());
    }

    #[test]
    fn slow_read_and_two_resubscriptions_deliver_latest_rows_without_error() {
        let start = Instant::now();
        let (jobs, receiver) = mpsc::sync_channel::<Job>(1);
        let pool = Pool { jobs };
        let (started, ready) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        let (finished, results) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut first = true;
            while let Ok(job) = receiver.recv() {
                let done = read(job, || {
                    if first {
                        started.send(()).unwrap();
                        blocked.recv_timeout(SECOND * 5).unwrap();
                    }
                    Ok(snapshot(1, false))
                });
                first = false;
                finished.send(done).unwrap();
            }
        });
        let mut state = Organization::new();
        assert!(pool.run(state.subscribe(Vec::new(), 0, start)).is_ok());
        ready.recv_timeout(SECOND * 5).unwrap(); // The original read is actually running.
        state.unsubscribe();
        assert!(pool.run(state.subscribe(Vec::new(), 0, start)).is_ok());
        state.unsubscribe();
        let latest = state.subscribe(Vec::new(), 0, start);
        let Err(mpsc::TrySendError::Full(job)) = pool.run(latest) else { panic!("the superseded job fills the slot") };
        state.defer(job, start);
        assert!(state.pulse(start + GAP / 2, |_| 0).is_none());
        release.send(()).unwrap();
        for _ in 0..2 {
            assert_eq!(state.done(results.recv_timeout(SECOND * 5).unwrap()), None);
        }
        assert!(state.plan(1 << 20).is_none(), "stale completions cannot supply rows");
        let retry = state.pulse(start + GAP, |_| 0).expect("retry before the safety interval");
        assert!(retry.bootstrap, "no initial read was admitted for this subscription");
        assert!(pool.run(retry).is_ok());
        assert_eq!(state.done(results.recv_timeout(SECOND * 5).unwrap()), None);
        assert!(matches!(state.plan(1 << 20), Some(Plan::Snapshot(_))));
        drop(pool);
        worker.join().unwrap();
    }

    #[test]
    fn rows_rebuild_the_snapshot_and_chunk_large_projects() {
        let original = snapshot(2500, false);
        let rows = rows(&original);
        assert_eq!(rows["project:alpha"]["session_chunks"], 3);
        assert_eq!(rows["members:alpha:2"]["sessions"].as_array().unwrap().len(), 500);
        assert!(rows["organization"].get("versions").is_none());
        assert_eq!(rows["organization"]["organization"]["projects"], json!(["alpha", "beta"]));
        assert_eq!(rebuild(&rows), without_written(original));
        let empty = json!({"schema": 1, "initialized": false, "organization": {"version": 2, "default_project": "ungrouped", "projects": []}, "versions": {}});
        assert_eq!(rebuild(&super::rows(&empty)), empty);
    }

    #[test]
    fn only_a_new_subscription_may_attempt_bootstrap() {
        let start = Instant::now();
        let mut organization = Organization::new();
        let first = organization.subscribe(vec![], 1, start);
        assert!(first.bootstrap);
        organization.done(Done { token: first.token, value: Some(Err("uncertain Qt presence".into())) });
        let changed = organization.pulse(start + GAP, |_| 2).unwrap();
        assert!(!changed.bootstrap, "a refresh must not retry an undecidable probe");
        organization.done(Done { token: changed.token, value: Some(Err("still unavailable".into())) });
        let safety = organization.pulse(start + SAFETY * 2, |_| 2).unwrap();
        assert!(!safety.bootstrap);
        let retried = organization.subscribe(vec![], 2, start + SAFETY * 3);
        assert!(retried.bootstrap, "an explicit subscription may try again");
    }

    #[test]
    fn the_first_rows_are_a_snapshot_and_a_change_is_a_delta_of_changed_rows() {
        let start = Instant::now();
        let mut organization = Organization::new();
        let job = organization.subscribe(vec![PathBuf::from("/config/swarm/catalog.json")], 1, start);
        assert_eq!(organization.plan(1 << 20), None, "nothing read yet");
        assert_eq!(organization.done(Done { token: job.token, value: Some(Ok(snapshot(3, false))) }), None);
        let Some(Plan::Snapshot(pages)) = organization.plan(1 << 20) else { panic!("a snapshot first") };
        assert_eq!(pages.len(), 1);
        organization.sent();
        assert_eq!(organization.plan(1 << 20), None);
        // The files changed: a read a second after the previous one started.
        assert!(organization.pulse(start + SECOND / 2, |_| 2).is_none());
        let job = organization.pulse(start + SECOND, |_| 2).expect("changed files");
        organization.done(Done { token: job.token, value: Some(Ok(snapshot(3, true))) });
        let Some(Plan::Delta { upsert, remove }) = organization.plan(1 << 20) else { panic!("a delta") };
        let keys: Vec<&str> = upsert.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["members:alpha:0", "members:beta:0", "organization", "version:member:x"]);
        assert!(remove.is_empty());
        // Too large for a frame: a snapshot instead.
        assert!(matches!(organization.plan(64), Some(Plan::Snapshot(_))));
        organization.sent();
        // A project that leaves removes its rows; a partial send resends all.
        let mut fewer = snapshot(3, true);
        fewer["organization"]["projects"].as_array_mut().unwrap().pop();
        let job = organization.pulse(start + SAFETY * 2, |_| 2).expect("the safety read");
        organization.done(Done { token: job.token, value: Some(Ok(fewer)) });
        let Some(Plan::Delta { remove, .. }) = organization.plan(1 << 20) else { panic!("a delta") };
        assert_eq!(remove, ["members:beta:0", "project:beta"]);
        organization.resend();
        assert!(matches!(organization.plan(1 << 20), Some(Plan::Snapshot(_))));
    }

    #[test]
    fn errors_are_reported_and_ended_or_stale_reads_are_dropped() {
        let start = Instant::now();
        let mut organization = Organization::new();
        let job = organization.subscribe(vec![], 1, start);
        assert_eq!(organization.done(Done { token: job.token, value: Some(Err("damaged".into())) }), Some("damaged".into()));
        let stale = organization.subscribe(vec![], 1, start);
        let fresh = organization.subscribe(vec![], 1, start);
        assert_eq!(organization.done(Done { token: stale.token, value: Some(Ok(snapshot(1, false))) }), None);
        assert_eq!(organization.plan(1 << 20), None, "a superseded read is dropped");
        assert!(organization.unsubscribe());
        assert_eq!(organization.done(Done { token: fresh.token, value: Some(Ok(snapshot(1, false))) }), None);
        assert!(read(fresh, || panic!("a cancelled read ran")).value.is_none());
        assert!(organization.pulse(start + SAFETY, |_| 9).is_none(), "not subscribed");
        let job = organization.subscribe(vec![], 1, start);
        assert_eq!(read(job, || panic!("boom")).value, Some(Err("the organization read failed".into())));
    }
}
