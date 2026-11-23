//! The `team.views` topic (task #16 of team c536f0e5, P2.5, design 4.5): the
//! Teams views a desktop follows, as keyed rows, so it reads none of them
//! with an `hgs team` child of its own.
//!
//! Rows, each exactly what its read returns (`hgs team <op> --json` with the
//! desktop's input, `zerus_client_core::teams::TeamRead::payload`):
//! - `list`: `team list` with `{include_cancelled: true, limit: 200}`;
//! - `waiting`: `team waiting` with `{limit: 100}`;
//! - for the team the subscription names (`team`): `status:<id>` (`team
//!   status` with `{team, include_cancelled: true}`), `inbox:<id>` (`team
//!   inbox` with `{team}`) and `feed:<id>` (`team events` with `{team, limit:
//!   50}`), the newest feed page.
//!
//! A read that fails is the row `{"error":{"code":"failed","message"}}`; a row
//! larger than [`ROW_BYTES`] is `{"error":{"code":"too_large","bytes"}}`: the
//! client reads that view with its command. Older pages, spans and the
//! profile stay commands.
//!
//! The rows are read on a worker thread, when a stat of the team store
//! changes and at least every 30 s, at most once a second (the `team`
//! topic's schedule), so each key changes at most once a second. The first
//! rows go out as a paged `snapshot`, later changes as one offered `delta` of
//! the changed rows; a client too far behind gets `gap` and a snapshot.
use super::team::{self, Job, Plan};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

/// The largest row sent; a larger one is an error row read with its command.
pub(super) const ROW_BYTES: usize = zerus_watch_protocol::PAGE_BYTES - 4096;
/// The teams a list row holds, as the desktop's list read asks.
const LIST_LIMIT: u64 = 200;
/// The Waiting items of the first page, as the desktop reads it.
const WAITING_PAGE: u64 = 100;
/// The events of the newest feed page, as the desktop reads it.
const FEED_PAGE: u64 = 50;

/// A finished read, or None when the subscription ended before it started.
pub(super) struct Done {
    pub token: u64,
    pub value: Option<Result<BTreeMap<String, Value>, String>>,
}

/// The reads of the rows: the key, the `hgs team` operation and its input.
pub(super) fn reads(team: Option<&str>) -> Vec<(String, &'static str, Value)> {
    let mut reads = vec![
        ("list".to_owned(), "list", json!({"include_cancelled": true, "limit": LIST_LIMIT})),
        ("waiting".to_owned(), "waiting", json!({"limit": WAITING_PAGE})),
    ];
    if let Some(team) = team.filter(|team| !team.is_empty()) {
        reads.push((format!("status:{team}"), "status", json!({"team": team, "include_cancelled": true})));
        reads.push((format!("inbox:{team}"), "inbox", json!({"team": team})));
        reads.push((format!("feed:{team}"), "events", json!({"team": team, "limit": FEED_PAGE})));
    }
    reads
}

/// Every row for `team`, read with `read` (`commands::view` in the server).
pub(super) fn rows(team: Option<&str>, read: impl Fn(&str, &Value) -> anyhow::Result<Value>) -> BTreeMap<String, Value> {
    reads(team)
        .into_iter()
        .map(|(key, op, input)| {
            let row = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(op, &input))) {
                Ok(Ok(value)) => {
                    let bytes = value.to_string().len();
                    if bytes > ROW_BYTES {
                        json!({"error":{"code":"too_large","bytes":bytes,"message":format!("The {op} view is {bytes} bytes; read it with hgs team {op}")}})
                    } else {
                        value
                    }
                }
                Ok(Err(error)) => json!({"error":{"code":"failed","message":format!("{error:#}")}}),
                Err(_) => json!({"error":{"code":"failed","message":format!("the {op} view failed")}}),
            };
            (key, row)
        })
        .collect()
}

/// The worker thread that reads the rows.
pub(super) struct Pool {
    jobs: mpsc::SyncSender<(Job, Option<String>)>,
}

impl Pool {
    pub(super) fn spawn(finished: impl Fn(Done) + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::sync_channel::<(Job, Option<String>)>(1);
        std::thread::Builder::new()
            .name("hgs-watch-team-views".into())
            .spawn(move || {
                while let Ok((job, team)) = queue.recv() {
                    let value = (!job.cancelled()).then(|| Ok(rows(team.as_deref(), crate::state::teams::commands::view)));
                    finished(Done { token: job.token, value });
                }
            })
            .expect("start the team views worker");
        Self { jobs }
    }

    /// Rejects without blocking the watch loop or losing the job.
    pub(super) fn run(&self, job: Job, team: Option<String>) -> Result<(), mpsc::TrySendError<(Job, Option<String>)>> {
        self.jobs.try_send((job, team))
    }
}

/// The team views subscription of one stream: the `team` topic's schedule
/// and plans, over these rows.
pub(super) struct TeamViews {
    state: team::Team,
    team: Option<String>,
}

impl TeamViews {
    pub(super) fn new() -> Self {
        Self { state: team::Team::new(), team: None }
    }

    /// Follows the views (and `team`'s, when named); following again sends
    /// every row again.
    pub(super) fn subscribe(&mut self, team: Option<String>, files: Vec<PathBuf>, stamp: u64, now: Instant) -> (Job, Option<String>) {
        self.team = team.filter(|team| !team.is_empty());
        (self.state.subscribe(files, stamp, now), self.team.clone())
    }

    pub(super) fn unsubscribe(&mut self) -> bool {
        self.team = None;
        self.state.unsubscribe()
    }

    /// The read whose time came, with the team it reads.
    pub(super) fn pulse(&mut self, now: Instant, stamp: impl Fn(&[PathBuf]) -> u64) -> Option<(Job, Option<String>)> {
        self.state.pulse(now, stamp).map(|job| (job, self.team.clone()))
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
#[path = "team_views_tests.rs"]
mod tests;
