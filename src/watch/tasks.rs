//! The `tasks` topic: one project's task list as `hgs task list --project ID
//! --json` prints it, sent as paged `snapshot` frames keyed by the project as
//! subscribed. Worker threads read the list, so the stream's loop never waits
//! for the task store. A project is read again when a stat of its store
//! changes and at least every 30 s, at most once a second; the snapshot goes
//! out only when the list changed. Its last page carries `{project, damaged,
//! digest}`, the digest identifying the list.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Every project is read at least this often, for what its store does not
/// show, such as a session label that changed.
pub(super) const SAFETY: Duration = Duration::from_secs(30);
/// Reads of one project start at least this far apart.
pub(super) const GAP: Duration = Duration::from_secs(1);
/// Threads that read lists.
pub(super) const WORKERS: usize = 1;
/// The longest project a subscription names, as the task store accepts it.
pub(super) const PROJECT_BYTES: usize = 128;

/// One read for a worker.
pub(super) struct Job {
    pub token: u64,
    pub key: String,
    cancelled: Arc<AtomicBool>,
}

/// A finished read.
pub(super) struct Done {
    pub token: u64,
    pub key: String,
    /// The `task list` object, an error of it, or None when the
    /// subscription ended before the read started.
    pub value: Option<Result<Value, String>>,
}

/// Runs one read with `list` (`task list --project KEY --json`).
pub(super) fn read(job: Job, list: impl Fn(&str) -> Result<Value, String>) -> Done {
    let Job { token, key, cancelled } = job;
    if cancelled.load(Ordering::SeqCst) {
        return Done { token, key, value: None };
    }
    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| list(&key)))
        .unwrap_or_else(|_| Err("the task list failed".into()));
    Done { token, key, value: Some(value) }
}

/// Worker threads that read lists with this computer's configuration.
pub(super) struct Pool {
    jobs: mpsc::SyncSender<Job>,
}

impl Pool {
    /// Starts `workers` threads; `finished` receives every read.
    pub(super) fn spawn(workers: usize, finished: impl Fn(Done) + Clone + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::sync_channel::<Job>(zerus_watch_protocol::TASKS_SUBSCRIPTIONS);
        let queue = Arc::new(Mutex::new(queue));
        for index in 0..workers {
            let queue = Arc::clone(&queue);
            let finished = finished.clone();
            std::thread::Builder::new()
                .name(format!("hgs-watch-tasks-{index}"))
                .spawn(move || {
                    let config = crate::config::Config::load().map_err(|error| error.to_string());
                    let list = |project: &str| match &config {
                        Ok(config) => crate::tasks::listing(config, project).map_err(|error| error.to_string()),
                        Err(error) => Err(error.clone()),
                    };
                    loop {
                        let job = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).recv();
                        let Ok(job) = job else { return };
                        finished(read(job, list));
                    }
                })
                .expect("start a task list worker");
        }
        Self { jobs }
    }

    /// Reject without blocking the watch loop or losing the original token.
    pub(super) fn run(&self, job: Job) -> Result<(), mpsc::TrySendError<Job>> {
        self.jobs.try_send(job)
    }
}

/// One followed project.
struct Project {
    token: u64,
    cancelled: Arc<AtomicBool>,
    running: bool,
    /// Its store changed while a read ran: read again.
    again: bool,
    started: Instant,
    due: Option<Instant>,
    /// The store folder, once a list named the project's ID, and its stamp.
    store: Option<Vec<PathBuf>>,
    stamp: Option<u64>,
    /// The digest of the list the client holds.
    sent: Option<String>,
    /// A list not sent yet, with its digest.
    unsent: Option<(Value, String)>,
}

/// The tasks subscriptions of one stream and when each project is read.
pub(super) struct Tasks {
    projects: BTreeMap<String, Project>,
    tokens: u64,
}

/// The digest a list's last page carries.
pub(super) fn digest(listing: &Value) -> String {
    let hash = Sha256::digest(listing.to_string().as_bytes());
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Tasks {
    pub(super) fn new() -> Self {
        Self { projects: BTreeMap::new(), tokens: 0 }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.projects.len()
    }

    pub(super) fn contains(&self, key: &str) -> bool {
        self.projects.contains_key(key)
    }

    fn job(&mut self, key: &str, now: Instant, stamp: Option<u64>) -> Option<Job> {
        self.tokens += 1;
        let token = self.tokens;
        let project = self.projects.get_mut(key)?;
        project.token = token;
        project.running = true;
        project.again = false;
        project.started = now;
        project.due = None;
        // Stamped first: a change while the list is read is seen on the next pulse.
        if stamp.is_some() {
            project.stamp = stamp;
        }
        Some(Job { token, key: key.to_owned(), cancelled: Arc::clone(&project.cancelled) })
    }

    /// Follows a project; one already followed sends its list again.
    pub(super) fn subscribe(&mut self, key: &str, now: Instant) -> Job {
        self.unsubscribe(key);
        self.projects.insert(
            key.to_owned(),
            Project {
                token: 0,
                cancelled: Arc::new(AtomicBool::new(false)),
                running: false,
                again: false,
                started: now,
                due: None,
                store: None,
                stamp: None,
                sent: None,
                unsent: None,
            },
        );
        self.job(key, now, None).expect("the project was just inserted")
    }

    pub(super) fn unsubscribe(&mut self, key: &str) -> bool {
        match self.projects.remove(key) {
            Some(project) => {
                project.cancelled.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Reads whose time came: a changed store stamp, or the safety interval.
    pub(super) fn pulse(&mut self, now: Instant, stamp: impl Fn(&[PathBuf]) -> u64) -> Vec<Job> {
        let mut due = Vec::new();
        for (key, project) in &mut self.projects {
            let current = project.store.as_deref().map(&stamp);
            if current.is_some() && current != project.stamp {
                if project.running {
                    project.again = true;
                } else if project.due.is_none() {
                    project.due = Some(now.max(project.started + GAP));
                }
            }
            if !project.running && (project.due.is_some_and(|at| now >= at) || now >= project.started + SAFETY) {
                due.push((key.clone(), current));
            }
        }
        due.into_iter().filter_map(|(key, stamp)| self.job(&key, now, stamp)).collect()
    }

    /// Queue pressure is not a read failure. Retry only the still-current job.
    pub(super) fn defer(&mut self, job: Job, now: Instant) {
        if let Some(project) = self.projects.get_mut(&job.key).filter(|project| project.token == job.token) {
            project.running = false;
            project.due = Some(now + GAP);
        }
    }

    /// Takes a finished read; an error is returned for the client.
    pub(super) fn done(&mut self, done: Done, store: impl Fn(&str) -> Vec<PathBuf>) -> Option<String> {
        let project = self.projects.get_mut(&done.key).filter(|project| project.token == done.token)?;
        project.running = false;
        if std::mem::take(&mut project.again) {
            project.due = Some(project.started + GAP);
        }
        let listing = match done.value? {
            Ok(listing) => listing,
            Err(error) => return Some(error),
        };
        if project.store.is_none() {
            if let Some(id) = listing["project"]["id"].as_str() {
                project.store = Some(store(id));
            }
        }
        let digest = digest(&listing);
        if project.sent.as_deref() != Some(digest.as_str()) {
            project.unsent = Some((listing, digest));
        }
        None
    }

    pub(super) fn pending(&self) -> bool {
        self.projects.values().any(|project| project.unsent.is_some())
    }

    /// Hands each unsent list to `send` with its digest; one `send` refuses
    /// stays unsent.
    pub(super) fn flush(&mut self, mut send: impl FnMut(&str, &Value, &str) -> bool) {
        for (key, project) in &mut self.projects {
            let Some((listing, digest)) = project.unsent.take() else { continue };
            if send(key, &listing, &digest) {
                project.sent = Some(digest);
            } else {
                project.unsent = Some((listing, digest));
                return;
            }
        }
    }
}

/// The last page's value: the list without its tasks, and its digest.
pub(super) fn summary(listing: &Value, digest: &str) -> Value {
    json!({"project": listing["project"], "damaged": listing["damaged"], "digest": digest})
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn listing(id: &str, texts: &[&str]) -> Value {
        json!({"project":{"id":id,"name":"orchard"},"damaged":[],
               "tasks":texts.iter().map(|text| json!({"text":text})).collect::<Vec<_>>()})
    }

    fn result(job: &Job, value: Value) -> Done {
        Done { token: job.token, key: job.key.clone(), value: Some(Ok(value)) }
    }

    #[test]
    fn unsubscribed_callback_cannot_cross_the_detail_fence() {
        let mut state = Tasks::new();
        let old = state.subscribe("project", Instant::now());
        state.unsubscribe("project");
        let current = state.subscribe("project", Instant::now());
        assert!(old.token < current.token);
        state.done(result(&old, listing("project", &["old"])), |_| Vec::new());
        assert!(!state.pending());
        state.done(result(&current, listing("project", &["current"])), |_| Vec::new());
        assert!(state.pending());
    }

    #[test]
    fn a_full_queue_defers_the_current_subscription_until_cancelled_jobs_drain() {
        let start = Instant::now();
        let (jobs, receiver) = mpsc::sync_channel(zerus_watch_protocol::TASKS_SUBSCRIPTIONS);
        let pool = Pool { jobs };
        let mut state = Tasks::new();
        for _ in 0..zerus_watch_protocol::TASKS_SUBSCRIPTIONS {
            assert!(pool.run(state.subscribe("project", start)).is_ok());
        }
        let Err(mpsc::TrySendError::Full(job)) = pool.run(state.subscribe("project", start)) else { panic!("bounded queue is full") };
        state.defer(job, start);
        assert!(state.pulse(start + GAP / 2, |_| 0).is_empty());
        for old in receiver.try_iter() {
            let done = read(old, |_| panic!("a cancelled task must not read"));
            assert!(done.value.is_none());
            assert_eq!(state.done(done, |_| Vec::new()), None);
        }
        assert!(!state.pending());
        let retry = state.pulse(start + GAP, |_| 0).pop().expect("retry before the safety interval");
        assert!(pool.run(retry).is_ok());
        assert_eq!(state.done(read(receiver.try_recv().unwrap(), |_| Ok(listing("project", &["latest"]))), |_| Vec::new()), None);
        let mut rows = Vec::new();
        state.flush(|_, list, _| { rows = list["tasks"].as_array().unwrap().clone(); true });
        assert_eq!(rows, vec![json!({"text":"latest"})]);
    }

    #[test]
    fn a_changed_store_or_the_safety_interval_reads_again_and_only_a_changed_list_is_sent() {
        let start = Instant::now();
        let stamp = std::cell::Cell::new(1u64);
        let mut tasks = Tasks::new();
        let first = tasks.subscribe("project-1", start);
        assert_eq!(first.key, "project-1");
        let store = |id: &str| vec![PathBuf::from(format!("/state/replica/{id}"))];
        assert_eq!(tasks.done(result(&first, listing("project-1", &["a"])), store), None);
        let mut sent = Vec::new();
        tasks.flush(|key, value, digest| {
            sent.push((key.to_owned(), value["tasks"].as_array().unwrap().len(), digest.to_owned()));
            true
        });
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].2, digest(&listing("project-1", &["a"])));
        // The first pulse records the store's stamp; no read is due.
        assert!(tasks.pulse(start + SECOND / 2, |_| stamp.get()).is_empty());
        // A changed stamp reads again, a second after the previous read started.
        stamp.set(2);
        assert!(tasks.pulse(start + SECOND / 2, |_| stamp.get()).is_empty());
        let again = tasks.pulse(start + SECOND, |_| stamp.get()).pop().expect("due");
        // The same list sends nothing; a changed one does.
        tasks.done(result(&again, listing("project-1", &["a"])), store);
        assert!(!tasks.pending());
        let safety = tasks.pulse(start + SECOND + SAFETY, |_| stamp.get()).pop().expect("safety read");
        tasks.done(result(&safety, listing("project-1", &["a", "b"])), store);
        assert!(tasks.pending());
        // A change while a read runs reads again once it finished, a second
        // after it started.
        stamp.set(3);
        let running = tasks.pulse(start + SECOND * 40, |_| stamp.get()).pop().expect("changed store");
        stamp.set(4);
        assert!(tasks.pulse(start + SECOND * 40, |_| stamp.get()).is_empty(), "a read is running");
        tasks.done(result(&running, listing("project-1", &["a", "b"])), store);
        assert_eq!(tasks.pulse(start + SECOND * 41, |_| stamp.get()).len(), 1);
        let mut refused = 0;
        tasks.flush(|_, _, _| {
            refused += 1;
            false
        });
        assert_eq!(refused, 1);
        assert!(tasks.pending());
    }

    #[test]
    fn errors_are_reported_and_ended_subscriptions_drop_late_results() {
        let start = Instant::now();
        let mut tasks = Tasks::new();
        let job = tasks.subscribe("missing", start);
        let failed = Done { token: job.token, key: job.key.clone(), value: Some(Err("unknown project".into())) };
        assert_eq!(tasks.done(failed, |_| vec![]), Some("unknown project".into()));
        let job = tasks.subscribe("project-2", start);
        assert!(tasks.unsubscribe("project-2"));
        let late = result(&job, listing("project-2", &["x"]));
        assert_eq!(tasks.done(late, |_| vec![]), None);
        assert!(!tasks.pending());
        assert!(read(job, |_| panic!("a cancelled read ran")).value.is_none());
        let job = tasks.subscribe("project-3", start);
        assert_eq!(read(job, |_| panic!("boom")).value, Some(Err("the task list failed".into())));
        assert_eq!(summary(&listing("p", &["a"]), "d"), json!({"project":{"id":"p","name":"orchard"},"damaged":[],"digest":"d"}));
    }
}
