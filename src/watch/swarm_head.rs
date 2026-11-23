//! The `swarm` topic (task #20 of team c536f0e5, P2.6c, design 4.5): this
//! computer's swarm head as one row, so a member that holds this computer's
//! stream (its desktop) asks its own swarm worker to sync with this computer
//! the moment its catalog or tasks change, instead of waiting for the
//! periodic sync. Propagation is then change-driven in both directions,
//! although only one side may open connections (a Windows PC without an SSH
//! server).
//!
//! The row `swarm` is `{catalog, tasks}` (`crate::swarm::head`): the digest of
//! the catalog's operations and the fingerprint of the stored tasks, as the
//! swarm worker compares them, so a sync's own bookkeeping never moves it. It
//! is read on a worker thread when a stat of the catalog or of a stored
//! project changes and at least every 30 s, at most once a second (the
//! `team` topic's schedule), and sent as a snapshot, then as deltas.
use super::team::{self, Job, Plan};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

/// The topic's one key.
pub(super) const KEY: &str = "swarm";

/// A finished read, or None when the subscription ended before it started.
pub(super) struct Done {
    pub token: u64,
    pub value: Option<Result<BTreeMap<String, Value>, String>>,
}

/// The rows of one read of `head`.
pub(super) fn rows(head: impl FnOnce() -> anyhow::Result<Value>) -> Result<BTreeMap<String, Value>, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(head)) {
        Ok(Ok(value)) => Ok(BTreeMap::from([(KEY.to_owned(), value)])),
        Ok(Err(error)) => Err(format!("{error:#}")),
        Err(_) => Err("the swarm head read failed".into()),
    }
}

/// The worker thread that reads the head.
pub(super) struct Pool {
    jobs: mpsc::SyncSender<Job>,
}

impl Pool {
    pub(super) fn spawn(config: crate::config::Config, finished: impl Fn(Done) + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("hgs-watch-swarm".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    let value = (!job.cancelled()).then(|| rows(|| crate::swarm::head(&config)));
                    finished(Done { token: job.token, value });
                }
            })
            .expect("start the swarm head worker");
        Self { jobs }
    }

    pub(super) fn run(&self, job: Job) -> Result<(), mpsc::TrySendError<Job>> {
        self.jobs.try_send(job)
    }
}

/// The swarm subscription of one stream: the `team` topic's schedule and
/// plans over its one row.
pub(super) struct SwarmHead {
    state: team::Team,
}

impl SwarmHead {
    pub(super) fn new() -> Self {
        Self { state: team::Team::new() }
    }

    pub(super) fn subscribe(&mut self, files: Vec<PathBuf>, stamp: u64, now: Instant) -> Job {
        self.state.subscribe(files, stamp, now)
    }

    pub(super) fn unsubscribe(&mut self) -> bool {
        self.state.unsubscribe()
    }

    pub(super) fn pulse(&mut self, now: Instant, stamp: impl Fn(&[PathBuf]) -> u64) -> Option<Job> {
        self.state.pulse(now, stamp)
    }

    pub(super) fn defer(&mut self, job: Job, now: Instant) {
        self.state.defer(job, now);
    }

    pub(super) fn done(&mut self, done: Done) -> Option<String> {
        self.state.done_rows(done.token, done.value)
    }

    pub(super) fn plan(&self, limit: usize) -> Option<Plan> {
        self.state.plan(limit)
    }

    pub(super) fn sent(&mut self) {
        self.state.sent();
    }

    pub(super) fn missed(&mut self) {
        self.state.missed();
    }

    pub(super) fn next_rev(&mut self) -> i64 {
        self.state.next_rev()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn head(catalog: &str) -> anyhow::Result<Value> {
        Ok(json!({"catalog": catalog, "tasks": "t"}))
    }

    #[test]
    fn the_head_is_one_row_sent_again_only_when_it_moved() {
        let start = Instant::now();
        let mut swarm = SwarmHead::new();
        assert!(swarm.pulse(start, |_| 0).is_none(), "nothing before a subscription");
        let job = swarm.subscribe(Vec::new(), 0, start);
        assert_eq!(swarm.done(Done { token: job.token, value: Some(rows(|| head("a"))) }), None);
        let Some(Plan::Snapshot { pages, .. }) = swarm.plan(1 << 20) else { panic!("a snapshot first") };
        assert_eq!(pages[0], vec![("swarm".to_owned(), json!({"catalog":"a","tasks":"t"}))]);
        swarm.sent();
        // The catalog's stat moved but not its operations: nothing is sent.
        let job = swarm.pulse(start + Duration::from_secs(1), |_| 1).expect("the stat moved");
        swarm.done(Done { token: job.token, value: Some(rows(|| head("a"))) });
        assert!(swarm.plan(1 << 20).is_none());
        let job = swarm.pulse(start + Duration::from_secs(2), |_| 2).expect("moved again");
        swarm.done(Done { token: job.token, value: Some(rows(|| head("b"))) });
        let Some(Plan::Delta { upsert, .. }) = swarm.plan(1 << 20) else { panic!("a delta") };
        assert_eq!(upsert, vec![("swarm".to_owned(), json!({"catalog":"b","tasks":"t"}))]);
        // A failed read is reported.
        swarm.sent();
        let job = swarm.pulse(start + team::SAFETY * 2, |_| 2).unwrap();
        assert_eq!(swarm.done(Done { token: job.token, value: Some(rows(|| anyhow::bail!("catalog busy"))) }).as_deref(), Some("catalog busy"));
        assert!(rows(|| panic!("boom")).is_err());
    }
}
