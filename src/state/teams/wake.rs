//! Task #17 of team c536f0e5 (P2.6a, design 4.6 and P2.6; owner 2026-11-23:
//! nothing waits longer than 3 s to be seen): the team worker waits on the
//! change bus instead of sleeping a fixed tick.
//!
//! - Events wake it at once (after the bus's 50 ms debounce): a commit to the
//!   team store by another connection, a session input of the state folder
//!   (the files `hgs watch` stamps), a file in a member's outbox, the stop
//!   request and a replaced `hgs` executable. The worker's own commits do not
//!   tick it: `PRAGMA data_version` on its own store connection moves only
//!   for other connections.
//! - At least every [`WAKE`] it runs a cheap check (the store's
//!   `data_version`, the listing reuse rule of P1.6, the members' outbox
//!   folders, work the last tick left in flight) and ticks only when one of
//!   them moved; a full tick runs at least every [`SAFETY`] anyway.
//! - Without the bus (`HGS_CHANGE_BUS=0`, a platform without a watcher, or a
//!   watcher that failed) every wake is a full tick, as before.
//!
//! Each wake and each tick's reason is counted in `tick.json` (`wakes`).
use crate::state::changes::{self, Bus, Hint, Note};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The longest wait between two wakes (owner, 2026-11-23).
pub(crate) const WAKE: Duration = Duration::from_secs(3);
/// A full tick runs at least this often, whatever the checks say (design
/// P2.6: the first release's bound for steps that run on time alone).
pub(crate) const SAFETY: Duration = Duration::from_secs(15);
/// At most this many member outbox folders are watched; the others are
/// seen by the cheap check.
const OUTBOXES: usize = 64;

/// The stop request, relative to the state folder.
const STOP: &str = "teams/worker.stop";

/// Why the worker woke before its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Event {
    /// A team store file changed (still to be checked: maybe its own commit).
    Store,
    /// A session input of the state folder changed.
    Session,
    /// A file in a member's outbox changed.
    Outbox,
    Stop,
    Executable,
    /// Notifications were lost or a watcher failed: anything may have changed.
    Overflow,
    /// Task #18 (P2.6b): a member computer's catalog stream showed a
    /// session's state change.
    Remote,
}

impl Event {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Event::Store => "store",
            Event::Session => "session",
            Event::Outbox => "outbox",
            Event::Stop => "stop",
            Event::Executable => "executable",
            Event::Overflow => "overflow",
            Event::Remote => "remote",
        }
    }
}

enum Signal {
    Root(Hint),
    Outbox,
    Executable,
    Remote,
    Failed { outbox: Option<PathBuf>, error: String },
}

/// Which notified paths of the state folder can wake the worker: the stop
/// request and what `hgs watch` stamps (the team store included), but not
/// the `teams` folder itself, whose entries the worker writes every tick.
fn keep_root(relative: &Path) -> bool {
    relative == Path::new(STOP) || (relative != Path::new("teams") && crate::watch::stamps::relevant(relative))
}

fn keep_any(_: &Path) -> bool {
    true
}

static EXECUTABLE_NAME: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();

fn keep_executable(relative: &Path) -> bool {
    EXECUTABLE_NAME.get().is_some_and(|name| relative.file_name() == Some(name.as_os_str()) && relative.parent() == Some(Path::new("")))
}

/// What a hint of the state folder names.
pub(crate) fn classify(hint: &Hint) -> BTreeSet<Event> {
    if hint.overflow {
        return BTreeSet::from([Event::Overflow]);
    }
    hint.paths
        .iter()
        .map(|path| {
            if path == Path::new(STOP) {
                Event::Stop
            } else if path.to_str().is_some_and(|path| path.replace('\\', "/").starts_with("teams/teams.sqlite3")) {
                Event::Store
            } else {
                Event::Session
            }
        })
        .collect()
}

/// The worker's change bus: the state folder, the executable's folder and
/// the members' outbox folders, feeding one channel.
pub(crate) struct Waker {
    tx: mpsc::Sender<Signal>,
    rx: mpsc::Receiver<Signal>,
    root: Option<Bus>,
    _executable: Option<Bus>,
    outboxes: BTreeMap<PathBuf, Bus>,
    /// Why the bus does not run, when it does not: every wake is a tick.
    pub(crate) fault: Option<String>,
}

impl Waker {
    /// Starts the watchers; returns once they are armed. A refused state
    /// folder leaves the worker on full ticks; a refused executable folder
    /// leaves the stat each wake does.
    pub(crate) fn start(root: &Path, executable: Option<&Path>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut waker = Self { tx, rx, root: None, _executable: None, outboxes: BTreeMap::new(), fault: None };
        if !changes::enabled() {
            waker.fault = Some("HGS_CHANGE_BUS=0".into());
            return waker;
        }
        let sink = waker.tx.clone();
        match Bus::start(root, keep_root, move |note| {
            sink.send(match note {
                Note::Hint(hint) => Signal::Root(hint),
                Note::Failed(error) => Signal::Failed { outbox: None, error },
            })
            .is_ok()
        }) {
            Ok(bus) => waker.root = Some(bus),
            Err(error) => waker.fault = Some(error),
        }
        if let Some((folder, name)) = executable.and_then(|path| Some((path.parent()?, path.file_name()?))) {
            if EXECUTABLE_NAME.get_or_init(|| name.to_owned()) == name {
                let sink = waker.tx.clone();
                waker._executable = Bus::start(folder, keep_executable, move |note| {
                    matches!(note, Note::Hint(_)) && sink.send(Signal::Executable).is_ok()
                })
                .ok();
            }
        }
        waker
    }

    /// Task #18 (P2.6b): what a member computer's catalog stream wakes the
    /// worker with; it works with or without the file notifications.
    pub(crate) fn notifier(&self) -> Notifier {
        Notifier(self.tx.clone())
    }

    /// Whether events wake the worker; otherwise every wake is a full tick.
    pub(crate) fn listening(&self) -> bool {
        self.root.is_some() && self.fault.is_none()
    }

    /// Watches the outbox folders of these member folders (those that
    /// exist), at most [`OUTBOXES`]; a folder no longer named is let go.
    pub(crate) fn watch_outboxes(&mut self, folders: &BTreeSet<PathBuf>) {
        if !self.listening() {
            return;
        }
        let wanted: BTreeSet<PathBuf> = folders.iter().map(|folder| super::outbox::dir(folder)).filter(|dir| dir.is_dir()).take(OUTBOXES).collect();
        self.outboxes.retain(|dir, _| wanted.contains(dir));
        for dir in wanted {
            if self.outboxes.contains_key(&dir) {
                continue;
            }
            let sink = self.tx.clone();
            let failed = dir.clone();
            let started = Bus::start(&dir, keep_any, move |note| {
                sink.send(match note {
                    Note::Hint(_) => Signal::Outbox,
                    Note::Failed(error) => Signal::Failed { outbox: Some(failed.clone()), error },
                })
                .is_ok()
            });
            if let Ok(bus) = started {
                self.outboxes.insert(dir, bus);
            }
        }
    }

    /// Waits until `until` or the first event, and returns the events that
    /// arrived (none: the deadline passed).
    pub(crate) fn wait(&mut self, until: Instant) -> BTreeSet<Event> {
        let mut events = BTreeSet::new();
        let first = match until.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => self.rx.recv_timeout(left).ok(),
            _ => self.rx.try_recv().ok(),
        };
        let Some(first) = first else { return events };
        let rest: Vec<Signal> = self.rx.try_iter().collect();
        for signal in std::iter::once(first).chain(rest) {
            match signal {
                Signal::Root(hint) => events.extend(classify(&hint)),
                Signal::Outbox => {
                    events.insert(Event::Outbox);
                }
                Signal::Executable => {
                    events.insert(Event::Executable);
                }
                Signal::Remote => {
                    events.insert(Event::Remote);
                }
                Signal::Failed { outbox: Some(dir), .. } => {
                    self.outboxes.remove(&dir);
                    events.insert(Event::Overflow);
                }
                Signal::Failed { outbox: None, error } => {
                    self.root = None;
                    self.outboxes.clear();
                    self.fault = Some(error);
                    events.insert(Event::Overflow);
                }
            }
        }
        events
    }
}

/// Task #18 (P2.6b): wakes the worker from another thread.
#[derive(Clone)]
pub(crate) struct Notifier(mpsc::Sender<Signal>);

impl Notifier {
    /// False once the worker no longer listens.
    pub(crate) fn remote(&self) -> bool {
        self.0.send(Signal::Remote).is_ok()
    }
}

/// `PRAGMA data_version` of the worker's own store connection: it moves
/// only when another connection commits.
pub(crate) fn data_version(db: &rusqlite::Connection) -> Option<i64> {
    db.query_row("PRAGMA data_version", [], |row| row.get(0)).ok()
}

/// The cheap check of a wake, each read only when the ones before found
/// nothing (the listing's probe last: it costs the most).
pub(crate) struct Check<S, F, O, L> {
    pub listening: bool,
    pub last_tick_quiet: bool,
    pub since_tick: Duration,
    pub store_moved: S,
    pub in_flight: F,
    pub outboxes_moved: O,
    pub listing_current: L,
}

/// Whether this wake runs a full tick, and why; None: nothing moved.
pub(crate) fn tick_reason<S, F, O, L>(events: &BTreeSet<Event>, check: Check<S, F, O, L>) -> Option<&'static str>
where
    S: FnOnce() -> bool,
    F: FnOnce() -> Option<&'static str>,
    O: FnOnce() -> bool,
    L: FnOnce() -> bool,
{
    // Task #18: a member computer's stream works without the file
    // notifications too.
    if events.contains(&Event::Remote) && !check.listening {
        return Some(Event::Remote.name());
    }
    if !check.listening {
        return Some("no bus");
    }
    for event in [Event::Overflow, Event::Store, Event::Outbox, Event::Session, Event::Remote] {
        if events.contains(&event) {
            return Some(event.name());
        }
    }
    if check.since_tick >= SAFETY {
        return Some("safety");
    }
    if !check.last_tick_quiet {
        return Some("busy");
    }
    if (check.store_moved)() {
        return Some("store");
    }
    if let Some(why) = (check.in_flight)() {
        return Some(why);
    }
    if (check.outboxes_moved)() {
        return Some("outbox");
    }
    if !(check.listing_current)() {
        return Some("listing");
    }
    None
}

/// The wakes and ticks since the worker started, for `tick.json`.
#[derive(Default)]
pub(crate) struct Counts {
    since: f64,
    wakes: BTreeMap<&'static str, u64>,
    ticks: BTreeMap<&'static str, u64>,
}

impl Counts {
    pub(crate) fn new(at: f64) -> Self {
        Self { since: at, ..Default::default() }
    }

    /// A wake: by an event, its own commit, or the deadline.
    pub(crate) fn wake(&mut self, why: &'static str) {
        *self.wakes.entry(why).or_default() += 1;
    }

    pub(crate) fn tick(&mut self, why: &'static str) {
        *self.ticks.entry(why).or_default() += 1;
    }

    pub(crate) fn value(&self, listening: bool) -> Value {
        json!({"since":self.since,"bus":listening,"wakes":self.wakes,"ticks":self.ticks})
    }
}

#[cfg(test)]
#[path = "tests/wake.rs"]
mod tests;
