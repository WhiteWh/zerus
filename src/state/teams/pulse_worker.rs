//! Task 185: a member's turn-end pulse is built off the dispatcher's tick.
//!
//! The build reads the member's session (`hgs inspect --skip-processes`, 15 s
//! bound). A member on a hung ssh host made that a 15 s wait inside the tick
//! for every one of its turn ends; so the tick only queues the job and goes on.
//! One thread works the queue in order and ends when it is empty; the next tick
//! stores what it built. Both queues are bounded, and nothing here touches the
//! store: the dispatcher owns it.
use super::pulses;
use super::store::Store;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

/// How long one session read may take: the bound the tick itself used to pay.
const READ: Duration = Duration::from_secs(15);
/// The most jobs waiting: a queue this deep means the dispatcher is behind and
/// the oldest job goes.
const JOBS: usize = 64;
/// The most built pulses kept for the tick; the oldest goes.
const BUILT: usize = 256;
/// The most built pulses one tick stores.
pub(super) const TAKE: usize = 16;

/// What a turn-end pulse is built from, all of it read on the tick (the team
/// record it already loaded), so the worker needs no store access.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Job {
    pub team: String,
    pub member: String,
    pub task: u32,
    /// The task's title, for the pulse's "#12 Title" line.
    pub title: String,
    /// The member's folder, to show changed files relative to it.
    pub folder: String,
    pub host: String,
    pub session: String,
    /// Only the session's events after this moment are the turn's.
    pub since: f64,
    /// The moment the turn ended, the pulse's time.
    pub at: f64,
}

/// A pulse the worker built.
pub(crate) struct Ready {
    pub job: Job,
    pub done: String,
    pub next: String,
    pub blocked: String,
}

pub(super) type Builder = fn(&Job) -> Option<(String, String, String)>;

/// The turn's summary from the member's own tool events; None when the session
/// could not be read or the turn did nothing concrete. `--skip-processes` keeps
/// a pulse read to the conversation: the member's process list is large and the
/// summary never uses it.
pub(super) fn build(job: &Job) -> Option<(String, String, String)> {
    let output = super::dispatcher::hgs(
        &job.host,
        &["inspect", &job.session, "--after", "0", "--skip-processes"],
        None,
        READ,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let events = value.get("events").and_then(Value::as_array)?;
    pulses::auto(job.task, &job.title, events, job.since, &job.folder)
}

/// Stores a built pulse as a row of the team's working tier (task 133). A store
/// that refuses the write loses the pulse; a turn end never fails for one.
pub(super) fn apply(store: &Store, job: &Job, (done, next, blocked): (String, String, String)) {
    let pulse = pulses::Pulse {
        team: job.team.clone(),
        member: job.member.clone(),
        task: job.task,
        done,
        next,
        blocked,
        at: job.at,
        source: "auto".into(),
        ..Default::default()
    };
    let _ = store.put_pulse(&pulse);
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    built: VecDeque<Ready>,
    /// Whether the worker thread is running.
    running: bool,
}

pub(super) struct Worker {
    state: Mutex<State>,
    build: Builder,
}

/// The worker the dispatcher uses.
pub(super) static SAMPLER: Worker = Worker::new(build);

impl Worker {
    pub(super) const fn new(build: Builder) -> Self {
        Self {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                built: VecDeque::new(),
                running: false,
            }),
            build,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Queues a turn end's build. Returns at once; the worker thread starts if
    /// it is not running.
    pub(super) fn queue(&'static self, job: Job) {
        {
            let mut state = self.lock();
            if state.queue.len() >= JOBS {
                state.queue.pop_front();
            }
            state.queue.push_back(job);
        }
        self.start();
    }

    /// What the worker built since the last pass, oldest first, at most TAKE:
    /// the tick stores them, so a slow store never holds the worker thread.
    pub(super) fn take(&self) -> Vec<Ready> {
        let mut state = self.lock();
        let take = state.built.len().min(TAKE);
        state.built.drain(..take).collect()
    }

    /// Task #17 of team c536f0e5 (P2.6a): a turn end's build is queued, being
    /// built or waiting for the tick to store it, so the worker ticks again.
    pub(super) fn busy(&self) -> bool {
        let state = self.lock();
        !state.queue.is_empty() || state.running || !state.built.is_empty()
    }

    /// Whether nothing is queued or being built (for tests).
    #[cfg(test)]
    pub(super) fn idle(&self) -> bool {
        let state = self.lock();
        state.queue.is_empty() && !state.running
    }

    fn start(&'static self) {
        {
            let mut state = self.lock();
            if state.running || state.queue.is_empty() {
                return;
            }
            state.running = true;
        }
        if std::thread::Builder::new().name("pulse-build".into()).spawn(move || self.work()).is_err() {
            let mut state = self.lock();
            state.running = false;
            state.queue.clear();
            state.built.clear();
        }
    }

    fn work(&self) {
        loop {
            let job = {
                let mut state = self.lock();
                match state.queue.pop_front() {
                    Some(job) => job,
                    None => {
                        state.running = false;
                        return;
                    }
                }
            };
            // A build that panics loses its pulse, never the thread.
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.build)(&job)))
                .ok()
                .flatten();
            let Some((done, next, blocked)) = built else { continue };
            let mut state = self.lock();
            if state.built.len() >= BUILT {
                state.built.pop_front();
            }
            state.built.push_back(Ready { job, done, next, blocked });
        }
    }
}

#[cfg(test)]
#[path = "tests/pulse_worker.rs"]
mod tests;
