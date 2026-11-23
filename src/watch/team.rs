//! The `team` topic (task 8): every team's head on this computer, so a client
//! reads a team again with `hgs team` when it changed instead of polling its
//! reads every few seconds. A worker thread reads the heads from the team
//! store with one indexed query; the stream's loop never waits for it. The
//! store is read again when a stat of it or its rollback journal changes and
//! at least every 30 s, at most once a second.
//!
//! Rows, keyed like the organization topic's:
//! - `team:<id>`: `{id, name, lead, status, revision, updated}`; every change
//!   of a team (a command, the dispatcher, a member's report) raises its
//!   `revision`, so a client compares revisions;
//! - `teams`: `{count, truncated}`, the number of rows and whether teams
//!   beyond [`zerus_watch_protocol::TEAM_ROWS`] (the least recently changed)
//!   were left out.
//!
//! The first rows go out as a paged `snapshot`; a later change as one `delta`
//! of the changed rows and removed keys. A delta is only offered: when the
//! client is behind, the topic gets `gap` (`overflow`) and a fresh snapshot
//! once it caught up.
use super::catalog::{self, Digest32};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// The store is read at least this often.
pub(super) const SAFETY: Duration = Duration::from_secs(30);
/// Reads start at least this far apart.
pub(super) const GAP: Duration = Duration::from_secs(1);

/// One read for the worker.
pub(super) struct Job {
    pub token: u64,
    cancelled: Arc<AtomicBool>,
}

/// A finished read: the heads and whether more exist, an error, or None when
/// the subscription ended before the read started.
pub(super) struct Done {
    pub token: u64,
    pub value: Option<Result<(Vec<Value>, bool), String>>,
}

/// Runs one read with `heads` (the store's heads, at most TEAM_ROWS).
pub(super) fn read(job: Job, heads: impl Fn() -> Result<(Vec<Value>, bool), String>) -> Done {
    let Job { token, cancelled } = job;
    if cancelled.load(Ordering::SeqCst) {
        return Done { token, value: None };
    }
    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&heads)).unwrap_or_else(|_| Err("the team read failed".into()));
    Done { token, value: Some(value) }
}

/// The worker thread that reads the heads.
pub(super) struct Pool {
    jobs: mpsc::SyncSender<Job>,
}

impl Pool {
    pub(super) fn spawn(store: PathBuf, finished: impl Fn(Done) + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("hgs-watch-team".into())
            .spawn(move || {
                let heads = || {
                    crate::state::teams::store::heads_at(&store, zerus_watch_protocol::TEAM_ROWS).map_err(|error| format!("{error:#}"))
                };
                while let Ok(job) = queue.recv() {
                    finished(read(job, heads));
                }
            })
            .expect("start the team worker");
        Self { jobs }
    }

    /// Reject without blocking the watch loop or losing the original token.
    pub(super) fn run(&self, job: Job) -> Result<(), mpsc::TrySendError<Job>> {
        self.jobs.try_send(job)
    }
}

/// The rows of one read, by key.
pub(super) fn rows(heads: &[Value], truncated: bool) -> BTreeMap<String, Value> {
    let mut rows: BTreeMap<String, Value> = heads
        .iter()
        .filter_map(|head| Some((format!("team:{}", head["id"].as_str()?), head.clone())))
        .collect();
    rows.insert("teams".into(), json!({"count": rows.len(), "truncated": truncated}));
    rows
}

/// What to send next.
#[derive(Debug, PartialEq)]
pub(super) enum Plan {
    /// Every row, in pages for `snapshot` frames; `gap` first when the
    /// client missed a delta.
    Snapshot { gap: bool, pages: Vec<Vec<(String, Value)>> },
    /// The changed rows and the removed keys, for one `delta` frame.
    Delta { upsert: Vec<(String, Value)>, remove: Vec<String> },
}

/// The team subscription of one stream and when the store is read.
pub(super) struct Team {
    subscribed: bool,
    token: u64,
    cancelled: Arc<AtomicBool>,
    running: bool,
    again: bool,
    started: Instant,
    due: Option<Instant>,
    files: Vec<PathBuf>,
    stamp: Option<u64>,
    /// The rows of the latest read, with their digests.
    current: Option<BTreeMap<String, (Value, Digest32)>>,
    /// The digests of the rows the client holds; None sends a snapshot.
    sent: Option<BTreeMap<String, Digest32>>,
    /// A delta the client did not take: `gap`, then a snapshot.
    missed: bool,
    rev: i64,
}

fn encoded(key: &str, row: &Value) -> usize {
    key.len() + row.to_string().len() + 24
}

impl Team {
    pub(super) fn new() -> Self {
        Self {
            subscribed: false,
            token: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
            running: false,
            again: false,
            started: Instant::now(),
            due: None,
            files: Vec::new(),
            stamp: None,
            current: None,
            sent: None,
            missed: false,
            rev: 0,
        }
    }

    fn job(&mut self, now: Instant) -> Job {
        self.token += 1;
        self.running = true;
        self.again = false;
        self.started = now;
        self.due = None;
        Job { token: self.token, cancelled: Arc::clone(&self.cancelled) }
    }

    /// Follows the teams, watching `files`; following them again sends every
    /// row again.
    pub(super) fn subscribe(&mut self, files: Vec<PathBuf>, stamp: u64, now: Instant) -> Job {
        self.unsubscribe();
        self.subscribed = true;
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.files = files;
        self.stamp = Some(stamp);
        self.job(now)
    }

    pub(super) fn unsubscribe(&mut self) -> bool {
        let was = std::mem::take(&mut self.subscribed);
        self.cancelled.store(true, Ordering::SeqCst);
        self.running = false;
        self.current = None;
        self.sent = None;
        self.missed = false;
        was
    }

    /// The read whose time came: a changed store, or the safety interval.
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
        Some(self.job(now))
    }

    /// Queue pressure is not a read failure. Retry only the still-current job.
    pub(super) fn defer(&mut self, job: Job, now: Instant) {
        if self.subscribed && job.token == self.token {
            self.running = false;
            self.due = Some(now + GAP);
        }
    }

    /// Takes a finished read; an error is returned for the client, and the
    /// next good read is a `gap` and a snapshot.
    pub(super) fn done(&mut self, done: Done) -> Option<String> {
        if !self.subscribed || done.token != self.token {
            return None;
        }
        self.running = false;
        if std::mem::take(&mut self.again) {
            self.due = Some(self.started + GAP);
        }
        match done.value? {
            Ok((heads, truncated)) => {
                let rows = rows(&heads, truncated);
                self.current = Some(rows.into_iter().map(|(key, row)| {
                    let digest = catalog::digest(&row);
                    (key, (row, digest))
                }).collect());
                None
            }
            Err(error) => {
                // Task #6 of team c536f0e5 (N3): the client drops its rows on
                // `team_failed`; the next good read sends `gap` and every row
                // again, not a delta it cannot apply (or nothing).
                self.current = None;
                self.missed();
                Some(error)
            }
        }
    }

    /// The frames that bring the client to the latest read; None when it is
    /// current. A delta larger than `limit` becomes a snapshot.
    pub(super) fn plan(&self, limit: usize) -> Option<Plan> {
        let current = self.current.as_ref()?;
        let snapshot = |gap: bool| {
            let rows: Vec<(String, Value)> = current.iter().map(|(key, (row, _))| (key.clone(), row.clone())).collect();
            let pages = catalog::pages(rows, |(key, row)| encoded(key, row), limit);
            Plan::Snapshot { gap, pages: if pages.is_empty() { vec![Vec::new()] } else { pages } }
        };
        if self.missed {
            return Some(snapshot(true));
        }
        let Some(sent) = &self.sent else { return Some(snapshot(false)) };
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
        Some(if size > limit { snapshot(false) } else { Plan::Delta { upsert, remove } })
    }

    /// The client now holds the latest read.
    pub(super) fn sent(&mut self) {
        self.missed = false;
        self.sent = self
            .current
            .as_ref()
            .map(|current| current.iter().map(|(key, (_, digest))| (key.clone(), *digest)).collect());
    }

    /// The client did not take a delta (it is behind): `gap`, then a snapshot.
    pub(super) fn missed(&mut self) {
        self.missed = true;
        self.sent = None;
    }

    /// The cursor of the next frames.
    pub(super) fn next_rev(&mut self) -> i64 {
        self.rev += 1;
        self.rev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn head(id: &str, revision: i64, status: &str) -> Value {
        json!({"id":id,"name":format!("Team {id}"),"lead":"lead","status":status,"revision":revision,"updated":revision as f64})
    }

    fn finish(team: &mut Team, job: Job, heads: Vec<Value>) -> Option<String> {
        team.done(read(job, || Ok((heads.clone(), false))))
    }

    fn keys(rows: &[(String, Value)]) -> Vec<&str> {
        rows.iter().map(|(key, _)| key.as_str()).collect()
    }

    #[test]
    fn subscribe_sends_a_snapshot_of_every_head_then_deltas_of_the_changed_ones() {
        let start = Instant::now();
        let mut team = Team::new();
        assert!(team.pulse(start, |_| 0).is_none(), "nothing before a subscription");
        let job = team.subscribe(vec![PathBuf::from("teams.sqlite3")], 1, start);
        assert_eq!(finish(&mut team, job, vec![head("a", 3, "running"), head("b", 1, "planning")]), None);
        let Some(Plan::Snapshot { gap: false, pages }) = team.plan(1 << 20) else { panic!("a snapshot first") };
        assert_eq!(pages.len(), 1);
        assert_eq!(keys(&pages[0]), ["team:a", "team:b", "teams"]);
        assert_eq!(pages[0][2].1, json!({"count":2,"truncated":false}));
        team.sent();
        assert_eq!(team.plan(1 << 20), None, "the client is current");
        // A command raised a's revision and b left: one delta with both.
        let job = team.pulse(start + SAFETY, |_| 1).expect("the safety read");
        finish(&mut team, job, vec![head("a", 4, "running")]);
        let Some(Plan::Delta { upsert, remove }) = team.plan(1 << 20) else { panic!("a delta") };
        assert_eq!(keys(&upsert), ["team:a", "teams"]);
        assert_eq!(upsert[0].1["revision"], 4);
        assert_eq!(remove, ["team:b"]);
        team.sent();
        // The same heads again send nothing.
        let job = team.pulse(start + SAFETY * 2, |_| 1).expect("the safety read");
        finish(&mut team, job, vec![head("a", 4, "running")]);
        assert_eq!(team.plan(1 << 20), None);
    }

    #[test]
    fn a_changed_store_reads_again_a_second_after_the_previous_read() {
        let start = Instant::now();
        let stamp = std::cell::Cell::new(1u64);
        let mut team = Team::new();
        let job = team.subscribe(vec![PathBuf::from("teams.sqlite3")], 1, start);
        finish(&mut team, job, vec![head("a", 1, "running")]);
        assert!(team.pulse(start + SECOND / 2, |_| stamp.get()).is_none(), "nothing changed");
        stamp.set(2);
        assert!(team.pulse(start + SECOND / 2, |_| stamp.get()).is_none(), "at most once a second");
        let running = team.pulse(start + SECOND, |_| stamp.get()).expect("the store changed");
        // A change while the read runs reads again once it finished.
        stamp.set(3);
        assert!(team.pulse(start + SECOND * 2, |_| stamp.get()).is_none(), "a read is running");
        finish(&mut team, running, vec![head("a", 2, "running")]);
        assert!(team.pulse(start + SECOND * 2, |_| stamp.get()).is_some());
    }

    #[test]
    fn a_missed_delta_is_a_gap_and_then_a_snapshot() {
        let start = Instant::now();
        let mut team = Team::new();
        let job = team.subscribe(Vec::new(), 0, start);
        finish(&mut team, job, vec![head("a", 1, "running")]);
        team.sent();
        let job = team.pulse(start + SAFETY, |_| 0).unwrap();
        finish(&mut team, job, vec![head("a", 2, "stuck")]);
        assert!(matches!(team.plan(1 << 20), Some(Plan::Delta { .. })));
        // The client was behind: the delta was not taken.
        team.missed();
        let Some(Plan::Snapshot { gap: true, pages }) = team.plan(1 << 20) else { panic!("gap, then a snapshot") };
        assert_eq!(pages[0][0].1["revision"], 2);
        team.sent();
        assert_eq!(team.plan(1 << 20), None);
    }

    /// Task #6 of team c536f0e5 (N3): a failed read makes the client drop its
    /// rows, so the next good read is `gap` and a snapshot, even of rows that
    /// did not change, instead of a delta or nothing.
    #[test]
    fn a_failed_read_is_followed_by_a_gap_and_a_snapshot() {
        let start = Instant::now();
        let mut team = Team::new();
        let job = team.subscribe(Vec::new(), 0, start);
        finish(&mut team, job, vec![head("a", 1, "running")]);
        team.sent();
        let job = team.pulse(start + SAFETY, |_| 0).unwrap();
        assert_eq!(team.done(Done { token: job.token, value: Some(Err("database is locked".into())) }), Some("database is locked".into()));
        assert_eq!(team.plan(1 << 20), None, "nothing until a good read");
        let job = team.pulse(start + SAFETY * 2, |_| 0).unwrap();
        finish(&mut team, job, vec![head("a", 1, "running")]);
        let Some(Plan::Snapshot { gap: true, pages }) = team.plan(1 << 20) else { panic!("gap, then a snapshot") };
        assert_eq!(keys(&pages[0]), ["team:a", "teams"]);
        team.sent();
        assert_eq!(team.plan(1 << 20), None);
    }

    #[test]
    fn errors_are_reported_and_ended_or_stale_reads_are_dropped() {
        let start = Instant::now();
        let mut team = Team::new();
        let job = team.subscribe(Vec::new(), 0, start);
        assert_eq!(team.done(Done { token: job.token, value: Some(Err("database is locked".into())) }), Some("database is locked".into()));
        let old = team.subscribe(Vec::new(), 0, start);
        let current = team.subscribe(Vec::new(), 0, start);
        assert_eq!(finish(&mut team, old, vec![head("old", 1, "running")]), None);
        assert_eq!(team.plan(1 << 20), None, "a read of an earlier subscription is dropped");
        assert!(team.unsubscribe());
        assert!(read(current, || panic!("a cancelled read ran")).value.is_none());
        let job = team.subscribe(Vec::new(), 0, start);
        assert_eq!(read(job, || panic!("boom")).value, Some(Err("the team read failed".into())));
        // A large delta is a snapshot.
        let job = team.subscribe(Vec::new(), 0, start);
        finish(&mut team, job, vec![head("a", 1, "running")]);
        team.sent();
        let job = team.pulse(start + SAFETY, |_| 0).unwrap();
        finish(&mut team, job, (0..50).map(|n| head(&format!("t{n}"), 1, "running")).collect());
        assert!(matches!(team.plan(256), Some(Plan::Snapshot { gap: false, .. })));
        assert_eq!(rows(&[head("a", 1, "running")], true)["teams"], json!({"count":1,"truncated":true}));
    }

    #[test]
    fn heads_come_from_the_store_newest_first_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teams/teams.sqlite3");
        assert_eq!(crate::state::teams::store::heads_at(&path, 10).unwrap(), (Vec::new(), false), "no store yet");
        let mut store = crate::state::teams::store::Store::open_at(&path, true).unwrap();
        for (n, id) in ["older", "newer", "newest"].iter().enumerate() {
            let team = crate::state::teams::model::Team { id: (*id).into(), name: format!("Team {id}"), lead: "lead".into(), ..Default::default() };
            store.create(&format!("11111111-1111-4111-8111-11111111111{n}"), &json!({"n":n}), team, vec![]).unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        store
            .mutate(None, "older", None, |team, _| {
                team.name = "Renamed".into();
                // Task #58: a free team whose phase 2 was locked to standard.
                team.plan_mode = crate::state::teams::model::PlanMode::Free;
                team.phase_modes.insert(2, crate::state::teams::model::PlanMode::Standard);
                Ok(Value::Null)
            })
            .unwrap();
        let (heads, more) = crate::state::teams::store::heads_at(&path, 2).unwrap();
        assert!(more);
        assert_eq!(heads.iter().map(|head| head["id"].as_str().unwrap()).collect::<Vec<_>>(), ["older", "newest"]);
        assert_eq!((heads[0]["name"].as_str(), heads[0]["revision"].as_i64(), heads[0]["lead"].as_str()), (Some("Renamed"), Some(2), Some("lead")));
        assert_eq!(heads[0]["status"], "planning");
        // Task #58: the watch carries the team's mode and each phase's own; a
        // team stored without them reads standard with no phase of its own.
        assert_eq!((&heads[0]["plan_mode"], &heads[0]["phase_modes"]), (&json!("free"), &json!({"2":"standard"})));
        assert_eq!((&heads[1]["plan_mode"], &heads[1]["phase_modes"]), (&json!("standard"), &json!({})));
        let files = crate::state::teams::store::files(&path);
        assert!(files[1].to_string_lossy().ends_with("teams.sqlite3-journal"));
    }
}
