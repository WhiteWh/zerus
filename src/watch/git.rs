//! The `git` topic: the `hgs git-status --json` object of each subscribed
//! checkout folder, sent as `state` keyed by the folder as subscribed. Worker
//! threads run the code `git-status` runs, so the stream's loop never waits for
//! Git. A folder is checked again when a stat of its Git metadata changes,
//! after a file-changing tool finished in a session under it, when its remote
//! check expires, and at least every 30 s; at most once a second. `state` goes
//! out only when the object changed apart from its sampling times.
use super::catalog::{self, Digest32};
use super::stamps;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zerus_watch_protocol as protocol;

/// Every folder is checked at least this often, for what neither its Git
/// metadata nor a session shows, such as an edit in another program.
pub(super) const SAFETY: Duration = Duration::from_secs(30);
/// Checks of one folder start at least this far apart, so a burst of changes
/// checks it once.
pub(super) const GAP: Duration = Duration::from_secs(1);
/// P1.7: a folder whose check is slow waits this many times its last
/// check's duration between starts: a large checkout that a session edits
/// all the time ran `git status` back to back.
const COST_FACTOR: u32 = 3;
/// The longest gap a slow check earns; the safety interval is no longer.
const MAX_GAP: Duration = SAFETY;
/// `git-status` reuses a remote check for this many seconds, and the desktop
/// shows a remote as verified as long. A folder is checked again when its
/// remote check expires, so a verified remote stays verified.
const REMOTE_TTL: f64 = 60.0;
/// Fields that change on every check. A new remote check changes
/// `remote_checked_at`, which is sent.
const VOLATILE: [&[&str]; 2] = [&["sampled_at"], &["remote_age_seconds"]];
/// The longest folder path a subscription takes, in bytes.
pub(super) const PATH_BYTES: usize = 4096;
/// Threads that run checks: as many as the desktop ran `git-status` children.
pub(super) const WORKERS: usize = 2;
/// Checks queued or running at most; due checks beyond it wait.
const QUEUE: usize = protocol::GIT_SUBSCRIPTIONS;
/// Files of the git and common directories whose stats start a check.
const METADATA: [&str; 5] = ["HEAD", "index", "packed-refs", "FETCH_HEAD", "reftable/tables.list"];

/// One check for a worker.
#[derive(Debug)]
pub(super) struct Job {
    pub token: u64,
    pub path: String,
    /// The files the previous check watched, stamped before this one runs.
    pub watched: Vec<PathBuf>,
    /// A check another stream made at or after this Unix time, of the same
    /// metadata, is current enough to take instead of running Git.
    pub fresh_after: f64,
    cancelled: Arc<AtomicBool>,
}

/// A finished check.
pub(super) struct Done {
    pub token: u64,
    pub path: String,
    /// The `git-status --json` object, an error of `git-status` itself, or
    /// None when the subscription ended before the check started.
    pub value: Option<Result<Value, String>>,
    /// The files to watch from now on, and their stamp before the check when
    /// they are the ones it started with.
    pub watched: Vec<PathBuf>,
    pub stamp: u64,
    /// When the check this result came from ran, if another stream ran it.
    pub cached_at: Option<f64>,
}

/// Runs one check with `status` (`git-status` for a folder).
pub(super) fn check(job: Job, status: impl Fn(&Path) -> Result<Value, String>, cache: Option<&Path>) -> Done {
    let Job { token, path, watched: before, fresh_after, cancelled } = job;
    if cancelled.load(Ordering::SeqCst) {
        return Done { token, path, value: None, watched: before, stamp: 0, cached_at: None };
    }
    // Stamped first: a change while Git runs is seen on the next pulse.
    let stamp = stamps::files(&before);
    let mut cached = cache.and_then(|dir| shared::read(dir, &path, &before, stamp, fresh_after, unix_now()));
    // Another stream checking the folder now is waited for: its check may
    // be current for this one too.
    let _held = match (cache, &cached) {
        (Some(dir), None) => {
            let held = shared::lock(dir, &path);
            cached = shared::read(dir, &path, &before, stamp, fresh_after, unix_now());
            held
        }
        _ => None,
    };
    let started = unix_now();
    let cached_at = cached.as_ref().map(|(at, _)| *at);
    let value = match cached {
        Some((_, value)) => Ok(value),
        None => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| status(Path::new(&path))))
            .unwrap_or_else(|_| Err("the git status check failed".into())),
    };
    let now_watched = match &value {
        Ok(value) => watched(&path, value),
        Err(_) => before.clone(),
    };
    let stamp = if now_watched == before { stamp } else { stamps::files(&now_watched) };
    // Kept with the files the next check watches, as this stream holds them.
    if let (Some(dir), Ok(value), None) = (cache, &value, cached_at) {
        shared::write(dir, &path, &now_watched, stamp, started, value);
    }
    Done { token, path, value: Some(value), watched: now_watched, stamp, cached_at }
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Checks shared between the streams of one computer: each real check is kept
/// in a small file per folder, and a stream takes one that ran after it needed
/// it, of unchanged Git metadata, instead of running Git again. Streams then
/// share one schedule, so a computer with several clients checks each folder
/// about as often as one stream would.
pub(super) mod shared {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    /// Entries older than this are removed now and then.
    const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
    /// How long a check waits for another stream's check of the folder; Git
    /// status gives up after 3 s.
    const WAIT: Duration = Duration::from_secs(4);
    static WRITES: AtomicUsize = AtomicUsize::new(0);

    fn file(dir: &Path, path: &str, extension: &str) -> PathBuf {
        let hash = Sha256::digest(path.as_bytes());
        let name: String = hash[..16].iter().map(|byte| format!("{byte:02x}")).collect();
        dir.join(format!("{name}.{extension}"))
    }

    /// Holds the folder's check while it runs; None when another stream's
    /// check outlasts the wait, and the check runs anyway.
    pub(in super::super) fn lock(dir: &Path, path: &str) -> Option<crate::platform::FileLock> {
        crate::state::watch_source::lock_until(&file(dir, path, "lock"), Instant::now() + WAIT).ok().flatten()
    }

    /// The kept check of `path` with these watched files and stamp, made at or
    /// after `fresh_after`, and its time; its remote age grown by its own age.
    pub(in super::super) fn read(dir: &Path, path: &str, watched: &[PathBuf], stamp: u64, fresh_after: f64, now: f64) -> Option<(f64, Value)> {
        let entry: Value = serde_json::from_slice(&std::fs::read(file(dir, path, "json")).ok()?).ok()?;
        let at = entry["checked_at"].as_f64()?;
        let same = entry["path"] == path
            && entry["stamp"].as_str() == Some(stamp.to_string().as_str())
            && serde_json::to_value(watched).is_ok_and(|watched| entry["watched"] == watched);
        if !same || at < fresh_after || at > now + 1.0 {
            return None;
        }
        let mut value = entry["value"].clone();
        if let Some(age) = value.get("remote_age_seconds").and_then(Value::as_f64) {
            value["remote_age_seconds"] = serde_json::json!(age + (now - at).max(0.0));
        }
        Some((at, value))
    }

    /// Keeps a real check; one that saw files change while it ran is not kept.
    /// The folder is private: states name branches and paths.
    pub(in super::super) fn write(dir: &Path, path: &str, watched: &[PathBuf], stamp: u64, at: f64, value: &Value) {
        if value.get("state").and_then(Value::as_str) == Some("changed_during_check") || crate::platform::private_dir(dir).is_err() {
            return;
        }
        let Ok(watched) = serde_json::to_value(watched) else { return };
        let entry = serde_json::json!({"path": path, "watched": watched, "stamp": stamp.to_string(), "checked_at": at, "value": value});
        if let Ok(mut temporary) = tempfile::Builder::new().prefix(".hgs-").tempfile_in(dir) {
            if temporary.write_all(entry.to_string().as_bytes()).is_ok() {
                let _ = crate::platform::persist(temporary, file(dir, path, "json"));
            }
        }
        if WRITES.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) {
            prune(dir);
        }
    }

    pub(in super::super) fn prune(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|meta| meta.modified()).ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > KEEP);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Worker threads that run checks in the order they were asked for.
pub(super) struct Pool {
    jobs: mpsc::Sender<Job>,
}

impl Pool {
    /// Starts `workers` threads; `finished` receives every check.
    pub(super) fn spawn(workers: usize, cache: Option<PathBuf>, finished: impl Fn(Done) + Clone + Send + 'static) -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let queue = Arc::new(Mutex::new(queue));
        for index in 0..workers {
            let queue = Arc::clone(&queue);
            let finished = finished.clone();
            let cache = cache.clone();
            std::thread::Builder::new()
                .name(format!("hgs-watch-git-{index}"))
                .spawn(move || loop {
                    let job = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).recv();
                    let Ok(job) = job else { return };
                    finished(check(job, crate::state::watch_source::git_status, cache.as_deref()));
                })
                .expect("start a git status worker");
        }
        Self { jobs }
    }

    pub(super) fn run(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Why a subscription was refused.
#[derive(Debug, PartialEq)]
pub(super) enum Refusal {
    BadRequest(&'static str),
    TooMany,
}

/// One followed folder.
struct Folder {
    token: u64,
    cancelled: Arc<AtomicBool>,
    /// The folder as subscribed, as Git resolved it and its checkout's root:
    /// a session working under one of them changes it.
    scope: Vec<PathBuf>,
    /// Git metadata whose stats start a check, and their last stamp.
    watched: Vec<PathBuf>,
    stamp: Option<u64>,
    running: bool,
    /// Something changed while the check ran: check again.
    again: bool,
    /// When the latest check started.
    started: Instant,
    /// How long the last finished check took, from its start to its result.
    cost: Duration,
    due: Option<Instant>,
    /// When the latest change not yet checked happened (Unix time): only a
    /// check that started after it is current.
    trigger_at: Option<f64>,
    /// The digest of the state the client holds.
    sent: Option<Digest32>,
    /// A state not sent yet, with its digest.
    unsent: Option<(Value, Digest32)>,
}

/// The git subscriptions of one stream and when each folder is checked.
pub(super) struct Git {
    folders: BTreeMap<String, Folder>,
    tokens: u64,
    rev: u64,
    /// Checks queued or running, including cancelled ones.
    queued: usize,
}

/// An absolute folder path without control characters, within the limit.
fn valid(path: &str) -> bool {
    !path.is_empty() && path.len() <= PATH_BYTES && Path::new(path).is_absolute() && !path.chars().any(char::is_control)
}

/// A ref name that stays inside its folder.
fn relative_ref(name: &str) -> Option<&Path> {
    let path = Path::new(name);
    (!name.is_empty()
        && name.len() <= 1024
        && !name.chars().any(char::is_control)
        && path.components().all(|component| matches!(component, Component::Normal(_))))
    .then_some(path)
}

/// An absolute path field of a `git-status` object.
fn absolute<'a>(value: &'a Value, key: &str) -> Option<&'a Path> {
    value.get(key).and_then(Value::as_str).filter(|text| valid(text)).map(Path::new)
}

/// A checkout's own Git directory: `.git`, or the folder a `.git` file names
/// (linked worktrees and submodules).
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let metadata = std::fs::metadata(&dot).ok()?;
    if metadata.is_dir() {
        return Some(dot);
    }
    if metadata.len() > 4096 {
        return None;
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(|| root.join(target))
}

/// The files whose stats start a check of `path`, given its last status:
/// `HEAD`, `index`, `packed-refs`, `FETCH_HEAD` and the reftable list in the
/// checkout's git and common directories, the current branch's ref and its
/// upstream's tracking ref. `.git` in the folder itself shows a repository
/// that appears there.
pub(super) fn watched(path: &str, value: &Value) -> Vec<PathBuf> {
    let mut files = vec![Path::new(path).join(".git")];
    let (Some(root), Some(common)) = (absolute(value, "root"), absolute(value, "common_dir")) else {
        return files;
    };
    let own = git_dir(root).unwrap_or_else(|| common.to_owned());
    let mut dirs = vec![own];
    if dirs[0] != common {
        dirs.push(common.to_owned());
    }
    for dir in &dirs {
        files.extend(METADATA.iter().map(|name| dir.join(name)));
    }
    if let Some(branch) = value.get("branch").and_then(Value::as_str).and_then(relative_ref) {
        for dir in &dirs {
            files.push(dir.join("refs/heads").join(branch));
        }
    }
    if let Some(upstream) = value.get("upstream").and_then(Value::as_str).and_then(relative_ref) {
        files.push(common.join("refs/remotes").join(upstream));
        files.push(common.join("refs/heads").join(upstream));
    }
    files.sort();
    files.dedup();
    files
}

/// Where sessions that change a folder work: the folder as subscribed, as
/// Git resolved it, and its checkout's root.
fn scope(path: &str, value: &Value) -> Vec<PathBuf> {
    let mut scope = vec![PathBuf::from(path)];
    for key in ["path", "root"] {
        if let Some(found) = absolute(value, key) {
            if !scope.iter().any(|known| known == found) {
                scope.push(found.to_owned());
            }
        }
    }
    scope
}

/// P1.7: how far apart a folder's checks start after one that took `cost`:
/// `max(GAP, COST_FACTOR x cost)`, at most `MAX_GAP`.
fn gap(cost: Duration) -> Duration {
    cost.saturating_mul(COST_FACTOR).clamp(GAP, MAX_GAP)
}

impl Folder {
    /// The earliest start of the next check.
    fn earliest(&self) -> Instant {
        self.started + gap(self.cost)
    }
}

/// A change at `at` (Unix time; when it was seen if unknown) makes the folder
/// due; only a check that started after it is current.
fn trigger(folder: &mut Folder, at: Option<f64>, now: Instant) {
    let seen = unix_now();
    let at = at.filter(|at| at.is_finite() && *at <= seen).unwrap_or(seen);
    folder.trigger_at = Some(folder.trigger_at.map_or(at, |known| known.max(at)));
    if folder.running {
        folder.again = true;
        return;
    }
    let at = now.max(folder.earliest());
    folder.due = Some(folder.due.map_or(at, |due| due.min(at)));
}

impl Git {
    pub(super) fn new() -> Self {
        Self { folders: BTreeMap::new(), tokens: 0, rev: 0, queued: 0 }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    /// Follows a folder and checks it now, or once the queue has room. A
    /// folder already followed starts over and its state is sent again.
    pub(super) fn subscribe(&mut self, path: &str, now: Instant) -> Result<Option<Job>, Refusal> {
        if !valid(path) {
            return Err(Refusal::BadRequest("a git subscription names an absolute folder path"));
        }
        if let Some(old) = self.folders.remove(path) {
            old.cancelled.store(true, Ordering::SeqCst);
        } else if self.folders.len() >= protocol::GIT_SUBSCRIPTIONS {
            return Err(Refusal::TooMany);
        }
        self.tokens += 1;
        self.folders.insert(
            path.to_owned(),
            Folder {
                token: self.tokens,
                cancelled: Arc::new(AtomicBool::new(false)),
                scope: vec![PathBuf::from(path)],
                watched: vec![Path::new(path).join(".git")],
                stamp: None,
                running: false,
                again: false,
                started: now,
                cost: Duration::ZERO,
                due: Some(now),
                // A new subscription takes no earlier check.
                trigger_at: Some(unix_now()),
                sent: None,
                unsent: None,
            },
        );
        Ok(self.start(path, now))
    }

    /// Stops following a folder; false when it was not followed.
    pub(super) fn unsubscribe(&mut self, path: &str) -> bool {
        match self.folders.remove(path) {
            Some(folder) => {
                folder.cancelled.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    fn start(&mut self, path: &str, now: Instant) -> Option<Job> {
        if self.queued >= QUEUE {
            return None;
        }
        let folder = self.folders.get_mut(path)?;
        folder.running = true;
        folder.again = false;
        folder.started = now;
        folder.due = None;
        self.queued += 1;
        // Without a change since the last check, any check newer than the
        // safety interval is current.
        let fresh_after = folder.trigger_at.unwrap_or(unix_now() - SAFETY.as_secs_f64());
        Some(Job {
            token: folder.token,
            path: path.to_owned(),
            watched: folder.watched.clone(),
            fresh_after,
            cancelled: Arc::clone(&folder.cancelled),
        })
    }

    /// Folders that hold one of `dirs`, where sessions changed files, are
    /// checked again soon. `at` is when the journal recorded the change: a
    /// check another stream started after it is current.
    pub(super) fn touch(&mut self, dirs: &[PathBuf], at: Option<f64>, now: Instant) {
        for folder in self.folders.values_mut() {
            if dirs.iter().any(|dir| folder.scope.iter().any(|scope| dir.starts_with(scope))) {
                trigger(folder, at, now);
            }
        }
    }

    /// Every folder is checked again soon (too many journal rows to tell).
    pub(super) fn touch_all(&mut self, now: Instant) {
        for folder in self.folders.values_mut() {
            trigger(folder, None, now);
        }
    }

    /// Stats each idle folder's Git metadata with `stamp`; returns the checks
    /// to start now: changed folders and those whose time came, oldest first.
    pub(super) fn pulse(&mut self, now: Instant, stamp: impl Fn(&[PathBuf]) -> u64) -> Vec<Job> {
        let mut due = Vec::new();
        for (path, folder) in self.folders.iter_mut().filter(|(_, folder)| !folder.running) {
            let current = stamp(&folder.watched);
            if folder.stamp.is_some_and(|known| known != current) {
                trigger(folder, None, now);
            }
            folder.stamp = Some(current);
            if let Some(at) = folder.due.filter(|at| *at <= now) {
                due.push((at, path.clone()));
            }
        }
        due.sort();
        due.into_iter().filter_map(|(_, path)| self.start(&path, now)).collect()
    }

    /// Takes a finished check. Its state waits for `flush` when it differs
    /// from what the client holds. Returns the error of a failed check.
    pub(super) fn done(&mut self, done: Done, now: Instant) -> Option<String> {
        self.queued = self.queued.saturating_sub(1);
        let folder = self.folders.get_mut(&done.path).filter(|folder| folder.token == done.token)?;
        folder.running = false;
        folder.cost = now.saturating_duration_since(folder.started);
        // A shared check sets the schedule: the next one is due a second
        // after the stream that ran it checks again.
        let mut next = match done.cached_at {
            Some(at) => {
                let left = at + (SAFETY + GAP).as_secs_f64() - unix_now();
                now + Duration::from_secs_f64(left.clamp(GAP.as_secs_f64(), (SAFETY + GAP).as_secs_f64()))
            }
            None => folder.started + SAFETY,
        };
        let mut again = std::mem::take(&mut folder.again);
        let value = match done.value {
            Some(Ok(value)) => value,
            outcome => {
                folder.due = Some(if again { now.max(folder.earliest()) } else { next });
                return outcome.and_then(Result::err);
            }
        };
        // The check covered every change before it started; one seen while it
        // ran stays for the next.
        if !again {
            folder.trigger_at = None;
        }
        folder.watched = done.watched;
        folder.stamp = Some(done.stamp);
        folder.scope = scope(&done.path, &value);
        if let Some(age) = value.get("remote_age_seconds").and_then(Value::as_f64).filter(|age| age.is_finite() && *age >= 0.0) {
            let left = Duration::from_secs_f64((REMOTE_TTL - age).clamp(0.0, REMOTE_TTL) + 0.25);
            next = next.min(now + left);
        }
        if value.get("state").and_then(Value::as_str) == Some("changed_during_check") {
            again = true;
            folder.trigger_at = Some(unix_now());
        }
        if again {
            next = next.min(now);
        }
        folder.due = Some(next.max(folder.earliest()));
        let digest = catalog::digest(&catalog::masked(&value, &VOLATILE));
        folder.unsent = (folder.sent != Some(digest)).then_some((value, digest));
        None
    }

    /// Whether a state waits to be sent.
    pub(super) fn pending(&self) -> bool {
        self.folders.values().any(|folder| folder.unsent.is_some())
    }

    /// Sends the states not sent yet through `send(key, value, rev)`. One the
    /// writer refuses stays for the next call, and so do the ones after it.
    pub(super) fn flush(&mut self, mut send: impl FnMut(&str, &Value, u64) -> bool) {
        for (path, folder) in self.folders.iter_mut() {
            let Some((value, digest)) = folder.unsent.take() else { continue };
            self.rev += 1;
            if send(path, &value, self.rev) {
                folder.sent = Some(digest);
            } else {
                folder.unsent = Some((value, digest));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    const SECOND: Duration = Duration::from_secs(1);

    /// An absolute folder on every platform.
    fn folder(name: &str) -> String {
        if cfg!(windows) {
            format!("C:\\work\\{name}")
        } else {
            format!("/work/{name}")
        }
    }

    fn inside(name: &str, below: &str) -> PathBuf {
        Path::new(&folder(name)).join(below)
    }

    fn status(path: &str, branch: &str, changed: u64) -> Value {
        json!({"state":"ok","path":path,"root":path,"common_dir":format!("{path}/.git"),"branch":branch,
            "upstream":format!("origin/{branch}"),"changed_files":changed,"remote_state":"no_upstream","sampled_at":1.0})
    }

    fn finish(git: &mut Git, job: Job, value: Value, now: Instant) -> Option<String> {
        let watched = watched(&job.path, &value);
        git.done(Done { token: job.token, path: job.path, value: Some(Ok(value)), watched, stamp: 7, cached_at: None }, now)
    }

    fn sent(git: &mut Git) -> Vec<(String, Value, u64)> {
        let mut out = Vec::new();
        git.flush(|key, value, rev| {
            out.push((key.to_owned(), value.clone(), rev));
            true
        });
        out
    }

    #[test]
    fn subscriptions_take_absolute_folders_up_to_the_limit() {
        let now = Instant::now();
        let mut git = Git::new();
        for path in ["", "relative/path", "/with\nnewline"] {
            assert!(matches!(git.subscribe(path, now), Err(Refusal::BadRequest(_))), "{path:?}");
        }
        assert!(matches!(git.subscribe(&format!("/{}", "x".repeat(PATH_BYTES)), now), Err(Refusal::BadRequest(_))));
        let mut jobs = 0;
        for index in 0..protocol::GIT_SUBSCRIPTIONS {
            jobs += usize::from(git.subscribe(&folder(&format!("folder-{index}")), now).unwrap().is_some());
        }
        assert_eq!(jobs, QUEUE);
        assert_eq!(git.subscribe(&folder("one-more"), now).unwrap_err(), Refusal::TooMany);
        // The same folder again replaces its subscription and does not count twice.
        let job = git.subscribe(&folder("folder-0"), now).unwrap();
        assert!(job.is_none(), "the queue is full until checks finish");
        assert!(git.unsubscribe(&folder("folder-1")));
        assert!(!git.unsubscribe(&folder("folder-1")));
        assert!(git.subscribe(&folder("one-more"), now).is_ok());
    }

    #[test]
    fn a_state_is_sent_once_per_change_ignoring_sampling_times() {
        let now = Instant::now();
        let mut git = Git::new();
        let job = git.subscribe(&folder("sample"), now).unwrap().unwrap();
        assert_eq!(job.watched, [inside("sample", ".git")]);
        assert!(finish(&mut git, job, status(&folder("sample"), "main", 0), now).is_none());
        let first = sent(&mut git);
        assert_eq!(first.len(), 1);
        assert_eq!((first[0].0.as_str(), first[0].1["changed_files"].as_u64(), first[0].2), (folder("sample").as_str(), Some(0), 1));
        // Nothing runs again before the safety tick unless something changes.
        assert!(git.pulse(now + 10 * SECOND, |_| 7).is_empty());
        let job = git.pulse(now + SAFETY, |_| 7).pop().expect("safety tick");
        let mut same = status(&folder("sample"), "main", 0);
        same["sampled_at"] = json!(2.0);
        same["remote_age_seconds"] = json!(3.5);
        finish(&mut git, job, same, now + SAFETY);
        assert!(!git.pending());
        assert!(sent(&mut git).is_empty());
        let job = git.pulse(now + 2 * SAFETY, |_| 7).pop().unwrap();
        finish(&mut git, job, status(&folder("sample"), "main", 3), now + 2 * SAFETY);
        let changed = sent(&mut git);
        assert_eq!((changed[0].1["changed_files"].as_u64(), changed[0].2), (Some(3), 2));
        // A refused frame stays pending and goes out on the next flush.
        let job = git.pulse(now + 3 * SAFETY, |_| 7).pop().unwrap();
        finish(&mut git, job, status(&folder("sample"), "main", 4), now + 3 * SAFETY);
        git.flush(|_, _, _| false);
        assert!(git.pending());
        assert_eq!(sent(&mut git)[0].1["changed_files"].as_u64(), Some(4));
        // Subscribing again sends the state again even when unchanged.
        let job = git.subscribe(&folder("sample"), now + 4 * SAFETY).unwrap().unwrap();
        finish(&mut git, job, status(&folder("sample"), "main", 4), now + 4 * SAFETY);
        assert_eq!(sent(&mut git).len(), 1);
    }

    #[test]
    fn metadata_changes_and_sessions_start_checks_at_most_once_a_second() {
        let now = Instant::now();
        let mut git = Git::new();
        let job = git.subscribe(&folder("sample"), now).unwrap().unwrap();
        finish(&mut git, job, status(&folder("sample"), "main", 0), now);
        sent(&mut git);
        // The first pulse after the check compares with the check's stamp.
        let stamp = Cell::new(7);
        assert!(git.pulse(now + SECOND / 4, |_| stamp.get()).is_empty());
        stamp.set(8);
        let job = git.pulse(now + SECOND / 2, |_| stamp.get()).pop();
        assert!(job.is_none(), "within a second of the previous check");
        let job = git.pulse(now + SECOND, |_| stamp.get()).pop().expect("a changed HEAD or index starts a check");
        // A change while the check runs checks again one second after it started.
        git.touch(&[inside("sample", "src")], None, now + SECOND);
        finish(&mut git, job, status(&folder("sample"), "main", 1), now + SECOND + SECOND / 10);
        assert!(git.pulse(now + SECOND + SECOND / 2, |_| 7).is_empty());
        let job = git.pulse(now + 2 * SECOND, |_| 7).pop().expect("the change during the check");
        finish(&mut git, job, status(&folder("sample"), "main", 1), now + 2 * SECOND);
        // Sessions elsewhere change nothing; under the folder or its root they do.
        git.touch(&[PathBuf::from(folder("other")), PathBuf::from(folder("samples"))], None, now + 5 * SECOND);
        assert!(git.pulse(now + 5 * SECOND, |_| 7).is_empty());
        git.touch(&[PathBuf::from(folder("sample"))], None, now + 5 * SECOND);
        git.touch(&[inside("sample", "deep/folder")], None, now + 5 * SECOND);
        let jobs = git.pulse(now + 5 * SECOND, |_| 7);
        assert_eq!(jobs.len(), 1, "a burst checks once");
        finish(&mut git, jobs.into_iter().next().unwrap(), status(&folder("sample"), "main", 1), now + 5 * SECOND);
        git.touch_all(now + 7 * SECOND);
        assert_eq!(git.pulse(now + 7 * SECOND, |_| 7).len(), 1);
    }

    /// P1.7: a folder's checks start `max(1 s, 3 x the last check's
    /// duration)` apart, at most 30 s.
    #[test]
    fn a_slow_check_spaces_the_next_by_three_times_its_cost_up_to_thirty_seconds() {
        assert_eq!(gap(Duration::ZERO), SECOND);
        assert_eq!(gap(SECOND / 4), SECOND);
        assert_eq!(gap(2 * SECOND), 6 * SECOND);
        assert_eq!(gap(20 * SECOND), Duration::from_secs(30));
        let now = Instant::now();
        let mut git = Git::new();
        let large = folder("large");
        let job = git.subscribe(&large, now).unwrap().unwrap();
        // The check took 4 s: a change waits until 12 s after it started.
        finish(&mut git, job, status(&large, "main", 0), now + 4 * SECOND);
        sent(&mut git);
        git.touch(&[PathBuf::from(&large)], None, now + 5 * SECOND);
        assert!(git.pulse(now + 11 * SECOND, |_| 7).is_empty(), "within three times its cost");
        let job = git.pulse(now + 12 * SECOND, |_| 7).pop().expect("three times its cost after it started");
        // A quick check brings the gap back to one second.
        finish(&mut git, job, status(&large, "main", 1), now + 12 * SECOND + SECOND / 10);
        git.touch(&[PathBuf::from(&large)], None, now + 12 * SECOND + SECOND / 5);
        assert!(git.pulse(now + 12 * SECOND + SECOND / 2, |_| 7).is_empty());
        let job = git.pulse(now + 13 * SECOND, |_| 7).pop().expect("one second after a quick check");
        // A check of 15 s waits the 30 s cap, the safety interval.
        finish(&mut git, job, status(&large, "main", 1), now + 28 * SECOND);
        git.touch(&[PathBuf::from(&large)], None, now + 29 * SECOND);
        assert!(git.pulse(now + 42 * SECOND, |_| 7).is_empty());
        assert_eq!(git.pulse(now + 43 * SECOND, |_| 7).len(), 1, "capped at 30 s after it started");
    }

    #[test]
    fn an_expiring_remote_check_is_renewed_and_cancelled_checks_are_dropped() {
        let now = Instant::now();
        let mut git = Git::new();
        let job = git.subscribe(&folder("sample"), now).unwrap().unwrap();
        let mut verified = status(&folder("sample"), "main", 0);
        verified["remote_state"] = json!("verified");
        verified["remote_checked_at"] = json!(100.0);
        verified["remote_age_seconds"] = json!(50.0);
        finish(&mut git, job, verified.clone(), now);
        assert_eq!(sent(&mut git).len(), 1);
        assert!(git.pulse(now + 9 * SECOND, |_| 7).is_empty());
        let job = git.pulse(now + 11 * SECOND, |_| 7).pop().expect("the remote check expires after 10 s");
        // A new remote check is a change; its age is not.
        verified["remote_checked_at"] = json!(161.0);
        verified["remote_age_seconds"] = json!(0.0);
        finish(&mut git, job, verified, now + 11 * SECOND);
        let renewed = sent(&mut git);
        assert_eq!((renewed.len(), renewed[0].1["remote_checked_at"].as_f64()), (1, Some(161.0)));
        // A check that outlives its subscription is dropped, and a cancelled
        // job never runs Git.
        let job = git.pulse(now + 71 * SECOND, |_| 7).pop().unwrap();
        assert!(git.unsubscribe(&folder("sample")));
        let ran = Cell::new(false);
        let done = check(
            job,
            |_| {
                ran.set(true);
                Ok(Value::Null)
            },
            None,
        );
        assert!(!ran.get() && done.value.is_none());
        assert!(git.done(done, now + 72 * SECOND).is_none());
        assert!(!git.pending());
        assert_eq!(git.queued, 0);
    }

    #[test]
    fn failed_checks_report_their_error_and_retry_on_the_safety_tick() {
        let now = Instant::now();
        let mut git = Git::new();
        let job = git.subscribe(&folder("sample"), now).unwrap().unwrap();
        let subscribed = job.fresh_after;
        let done = check(job, |_| Err("Local sessions are unsupported".into()), None);
        assert_eq!(git.done(done, now).as_deref(), Some("Local sessions are unsupported"));
        assert!(!git.pending());
        assert!(git.pulse(now + SAFETY - SECOND, |_| 7).is_empty());
        // The retry still takes only a check newer than the subscription.
        let retry = git.pulse(now + SAFETY, |_| 7).pop().expect("the safety tick");
        assert_eq!(retry.fresh_after, subscribed);
        let watched = retry.watched.clone();
        git.done(Done { token: retry.token, path: retry.path, value: Some(Ok(status(&folder("sample"), "main", 0))), watched, stamp: 7, cached_at: None }, now + SAFETY);
        let next = git.pulse(now + 2 * SAFETY, |_| 7).pop().expect("the next safety tick");
        // After a check succeeded, the safety interval sets what is current.
        assert!(next.fresh_after < subscribed, "{} against {subscribed}", next.fresh_after);
    }

    #[test]
    fn watched_metadata_covers_both_git_directories_and_the_tracking_ref() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        std::fs::create_dir_all(main.join(".git/worktrees/linked")).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/linked").display())).unwrap();
        let common = main.join(".git");
        let value = json!({"state":"ok","path":linked,"root":linked,"common_dir":common,"branch":"feature/x","upstream":"origin/feature/x"});
        let files = watched(linked.to_str().unwrap(), &value);
        let own = main.join(".git/worktrees/linked");
        for expected in [
            linked.join(".git"),
            own.join("HEAD"),
            own.join("index"),
            own.join("FETCH_HEAD"),
            common.join("HEAD"),
            common.join("packed-refs"),
            common.join("refs/heads/feature/x"),
            common.join("refs/remotes/origin/feature/x"),
        ] {
            assert!(files.contains(&expected), "{} not in {files:?}", expected.display());
        }
        // A ref name never leaves its folder; a folder without Git watches `.git` only.
        let escaping = json!({"state":"ok","root":main,"common_dir":common,"branch":"../../outside"});
        assert!(watched(main.to_str().unwrap(), &escaping).iter().all(|file| file.starts_with(&main)));
        assert_eq!(watched(&folder("plain"), &json!({"state":"not_repo"})), [inside("plain", ".git")]);
        // A commit replaces the branch ref and the index: the stamp changes.
        let before = stamps::files(&files);
        std::fs::create_dir_all(common.join("refs/heads/feature")).unwrap();
        std::fs::write(common.join("refs/heads/feature/x"), "0".repeat(40)).unwrap();
        assert_ne!(stamps::files(&files), before);
    }

    #[test]
    fn a_check_another_stream_ran_after_it_was_needed_is_taken_instead_of_running_git() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("watch-git");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let path = repo.to_str().unwrap().to_owned();
        let runs = Cell::new(0);
        let status = |_: &Path| {
            runs.set(runs.get() + 1);
            let mut value = status(&path, "main", runs.get());
            value["remote_age_seconds"] = json!(5.0);
            Ok(value)
        };
        let mut first = Git::new();
        let job = first.subscribe(&path, Instant::now()).unwrap().unwrap();
        let done = check(job, status, Some(&cache));
        assert_eq!((runs.get(), done.cached_at), (1, None));
        let watched = done.watched.clone();
        let first_value = done.value.clone().unwrap().unwrap();
        first.done(done, Instant::now());
        // A new subscription of another stream checks for itself.
        let mut second = Git::new();
        let now = Instant::now();
        let job = second.subscribe(&path, now).unwrap().unwrap();
        let done = check(job, status, Some(&cache));
        assert_eq!((runs.get(), done.cached_at), (2, None));
        second.done(done, now);
        // Its safety check takes the newer check the first stream kept.
        let later = now + SAFETY;
        let job = second.pulse(later, stamps::files).pop().expect("the safety check");
        assert_eq!(job.watched, watched);
        let done = check(job, status, Some(&cache));
        let cached_at = done.cached_at.expect("the kept check is taken");
        assert_eq!(runs.get(), 2);
        let value = done.value.clone().unwrap().unwrap();
        assert_eq!(value["changed_files"], json!(2));
        assert!(value["remote_age_seconds"].as_f64().unwrap() >= 5.0, "the remote age grows with the kept check's");
        assert_ne!(value, first_value);
        // The next check is due when the stream that ran it checks again.
        second.done(done, later);
        let next = cached_at + (SAFETY + GAP).as_secs_f64() - unix_now();
        let due = second.folders[&path].due.unwrap() - later;
        assert!((due.as_secs_f64() - next).abs() < 0.5, "{due:?} against {next}");
        // A change seen after the kept check runs Git again.
        second.touch(&[repo.join("src")], None, later);
        let job = second.pulse(later + SAFETY, stamps::files).pop().expect("the triggered check");
        assert!(job.fresh_after > cached_at);
        assert_eq!(check(job, status, Some(&cache)).cached_at, None);
        assert_eq!(runs.get(), 3);
        // So do changed Git metadata and a changed watched list.
        let job = Job { token: 1, path: path.clone(), watched: watched.clone(), fresh_after: 0.0, cancelled: Arc::default() };
        assert!(check(job, status, Some(&cache)).cached_at.is_some());
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
        let job = Job { token: 1, path: path.clone(), watched: watched.clone(), fresh_after: 0.0, cancelled: Arc::default() };
        assert_eq!(check(job, status, Some(&cache)).cached_at, None);
        let job = Job { token: 1, path: path.clone(), watched: vec![repo.join(".git")], fresh_after: 0.0, cancelled: Arc::default() };
        assert_eq!(check(job, status, Some(&cache)).cached_at, None);
        assert_eq!(runs.get(), 5);
    }

    #[test]
    fn a_check_that_saw_files_change_is_not_kept_and_old_entries_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let watched = vec![PathBuf::from("/work/a/.git")];
        let mut value = status("/work/a", "main", 1);
        value["state"] = json!("changed_during_check");
        shared::write(dir.path(), "/work/a", &watched, 3, 100.0, &value);
        assert!(shared::read(dir.path(), "/work/a", &watched, 3, 0.0, 101.0).is_none());
        value["state"] = json!("ok");
        shared::write(dir.path(), "/work/a", &watched, 3, 100.0, &value);
        assert_eq!(shared::read(dir.path(), "/work/a", &watched, 3, 100.0, 101.0), Some((100.0, value.clone())));
        // Too old, from the future, of another stamp or another folder.
        assert!(shared::read(dir.path(), "/work/a", &watched, 3, 100.5, 101.0).is_none());
        assert!(shared::read(dir.path(), "/work/a", &watched, 3, 0.0, 98.0).is_none());
        assert!(shared::read(dir.path(), "/work/a", &watched, 4, 0.0, 101.0).is_none());
        assert!(shared::read(dir.path(), "/work/b", &watched, 3, 0.0, 101.0).is_none());
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|entry| entry.path()).collect();
        assert_eq!(files.len(), 1, "only the entry file is left: {files:?}");
        // A day-old entry goes; a fresh one stays.
        let old = std::fs::File::options().write(true).open(&files[0]).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(25 * 60 * 60)).unwrap();
        drop(old);
        shared::write(dir.path(), "/work/b", &watched, 3, 100.0, &value);
        shared::prune(dir.path());
        assert!(!files[0].exists());
        assert!(shared::read(dir.path(), "/work/b", &watched, 3, 0.0, 101.0).is_some());
    }

    #[test]
    fn a_stream_waits_for_a_running_check_it_can_take_and_journal_times_set_what_is_current() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("watch-git");
        let path = folder("busy");
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let slow = |_: &Path| {
            runs.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            Ok(status(&path, "main", 1))
        };
        // As a stream holds them after a first check.
        let files = watched(&path, &status(&path, "main", 1));
        let job = |fresh_after| Job { token: 1, path: path.clone(), watched: files.clone(), fresh_after, cancelled: Arc::default() };
        let needed = unix_now();
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| check(job(needed), slow, Some(&cache)));
            std::thread::sleep(Duration::from_millis(100));
            let second = scope.spawn(|| check(job(needed), slow, Some(&cache)));
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the second stream took the running check");
        assert_eq!(first.cached_at, None);
        assert!(second.cached_at.is_some_and(|at| at >= needed));
        assert_eq!(first.value, second.value);
        // A check that started before the change it needs runs Git again.
        assert_eq!(check(job(unix_now()), slow, Some(&cache)).cached_at, None);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // The journal's time of a change is what a check must follow; a time
        // not yet reached is taken as now.
        let now = Instant::now();
        let mut git = Git::new();
        let started = git.subscribe(&path, now).unwrap().unwrap();
        git.done(Done { token: started.token, path: path.clone(), value: Some(Ok(status(&path, "main", 1))), watched: vec![], stamp: 0, cached_at: None }, now);
        let changed = unix_now() - 5.0;
        git.touch(&[PathBuf::from(&path)], Some(changed - 10.0), now);
        git.touch(&[PathBuf::from(&path)], Some(changed), now);
        let job = git.pulse(now + SECOND, |_| 0).pop().expect("the touched folder");
        assert_eq!(job.fresh_after, changed);
        git.done(Done { token: job.token, path: path.clone(), value: Some(Ok(status(&path, "main", 1))), watched: vec![], stamp: 0, cached_at: None }, now + SECOND);
        let before = unix_now();
        git.touch(&[PathBuf::from(&path)], Some(before + 60.0), now + SECOND);
        let job = git.pulse(now + 2 * SECOND, |_| 0).pop().expect("the touched folder");
        assert!(job.fresh_after >= before && job.fresh_after <= unix_now());
    }
}
