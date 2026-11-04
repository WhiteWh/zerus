//! Polling for a computer whose `hgs` predates the event stream, as the Qt
//! client does: `hgs [@host] ls --json` every 60 s (15 s timeout), and for each
//! followed session `inspect NAME --after CURSOR --skip-processes` every 2.5 s
//! and its processes (`inspect NAME`) every 10 s, each with at most one read
//! in flight. Reads run on their own threads through [`run_limited`], so their
//! output is bounded too; the supervisor applies the replies as it would the
//! stream's frames. A probe that finds the stream (after an update) ends the
//! polling.
use crate::actions::{Exit, Invocation};
use crate::reads::READ_LIMIT;
use crate::run::run_limited;
use crate::ComputerId;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// Incremental listing decode for the future raw source adapter. The existing
/// GUI runner and polling schedule retain their current behavior.
pub mod listing;
/// Ephemeral private staging for the future raw legacy source adapter.
pub mod spool;

/// How often a computer's listing is read.
pub const LISTING: Duration = Duration::from_secs(60);
/// How long a listing may take.
pub const LISTING_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a followed session's events and summary are read.
pub const ACTIVITY: Duration = Duration::from_millis(2500);
/// How often a followed session's processes are read.
pub const PROCESSES: Duration = Duration::from_secs(10);

/// One read of the polling.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Read {
    Listing,
    /// Events after `after` (None: the newest window) and the summary.
    Activity {
        key: String,
        name: String,
        after: Option<i64>,
    },
    Processes {
        key: String,
        name: String,
    },
}

impl Read {
    /// The `hgs` arguments after the computer.
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Listing => vec!["ls".into(), "--json".into()],
            Self::Activity { name, after, .. } => vec![
                "inspect".into(),
                name.clone(),
                "--after".into(),
                after.unwrap_or(0).to_string(),
                "--skip-processes".into(),
            ],
            // The newest possible cursor: processes without events.
            Self::Processes { name, .. } => vec![
                "inspect".into(),
                name.clone(),
                "--after".into(),
                i64::MAX.to_string(),
            ],
        }
    }

    /// How long the read may take on `computer`.
    pub fn timeout(&self, computer: &ComputerId) -> Duration {
        match self {
            Self::Listing => LISTING_TIMEOUT,
            _ if computer.is_local() => Duration::from_secs(5),
            _ => Duration::from_secs(15),
        }
    }

    fn slot(&self) -> Slot {
        match self {
            Self::Listing => Slot::Listing,
            Self::Activity { key, .. } => Slot::Activity(key.clone()),
            Self::Processes { key, .. } => Slot::Processes(key.clone()),
        }
    }
}

/// Runs one read with `hgs` on `computer`: the reply's JSON, or why there is
/// none.
pub fn run(hgs: &Path, computer: &ComputerId, read: &Read) -> Result<Value, String> {
    run_with(computer, read, |invocation| {
        run_limited(hgs, invocation, &[], READ_LIMIT)
            .map_err(|error| format!("the reply passed {} bytes", error.cap))
    })
}

pub(crate) fn run_accounted(
    hgs: &Path,
    computer: &ComputerId,
    read: &Read,
    stop: &std::sync::atomic::AtomicBool,
    resources: std::sync::Arc<dyn Send + Sync>,
) -> Result<Value, String> {
    run_with(computer, read, |invocation| {
        crate::run::run_accounted(hgs, invocation, &[], READ_LIMIT, stop, resources.clone())
            .map_err(|error| match error {
                crate::run::Stopped::Cancelled => "cancelled".into(),
                crate::run::Stopped::TooLarge(error) => {
                    format!("the reply passed {} bytes", error.cap)
                }
            })
    })
}
fn run_with(
    computer: &ComputerId,
    read: &Read,
    run: impl FnOnce(&Invocation) -> Result<Exit, String>,
) -> Result<Value, String> {
    let mut args: Vec<String> = computer
        .alias()
        .map(|alias| format!("@{alias}"))
        .into_iter()
        .collect();
    args.extend(read.args());
    let invocation = Invocation {
        args,
        stdin: None,
        timeout: read.timeout(computer),
        request_id: None,
    };
    match run(&invocation) {
        Ok(Exit::Code {
            code: 0, stdout, ..
        }) => serde_json::from_slice(&stdout).map_err(|error| format!("invalid reply: {error}")),
        Ok(Exit::Code { code, stderr, .. }) => {
            Err(failure(&stderr, &format!("hgs exited with code {code}")))
        }
        Ok(Exit::Abnormal { stderr }) => Err(failure(&stderr, "hgs ended abnormally")),
        Ok(Exit::TimedOut) => Err("hgs did not answer in time".into()),
        Ok(Exit::StartFailed(error)) => Err(error),
        Ok(Exit::WriteFailed) => Err("hgs did not take the request".into()),
        Err(error) => Err(error),
    }
}

fn failure(stderr: &str, otherwise: &str) -> String {
    let text: String = stderr.trim().chars().take(2000).collect();
    if text.is_empty() {
        otherwise.to_owned()
    } else {
        text
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Listing,
    Activity(String),
    Processes(String),
}

/// How often each kind of read runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intervals {
    pub listing: Duration,
    pub activity: Duration,
    pub processes: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            listing: LISTING,
            activity: ACTIVITY,
            processes: PROCESSES,
        }
    }
}

/// When each read is due and which are in flight.
#[derive(Debug, Default)]
pub struct Schedule {
    every: Intervals,
    due: BTreeMap<Slot, Instant>,
    running: BTreeMap<Slot, ()>,
    started: BTreeMap<Slot, Instant>,
    refreshes: BTreeSet<Slot>,
    /// Consecutive failed listings; the status's attempt.
    failures: u32,
    /// A listing succeeded since polling started.
    listed: bool,
}

/// The sessions to poll: (key, name) of followed Activity with its cursor
/// (None before its first window), and of followed processes.
pub struct Follows<'a> {
    pub activity: &'a [(String, String, Option<i64>)],
    pub processes: &'a [(String, String)],
}

impl Schedule {
    /// Raw-source resnapshots share the listing slot and a minimum one-second
    /// start gap across generations. The GUI's offline refresh is unchanged.
    pub(crate) fn refresh_listing_bounded(&mut self, now: Instant) {
        let slot = Slot::Listing;
        let Some(started) = self.started.get(&slot).copied() else {
            return;
        };
        self.refreshes.insert(slot.clone());
        if !self.running.contains_key(&slot) {
            self.due
                .insert(slot, now.max(started + Duration::from_secs(1)));
        }
    }
    /// Retry an offline listing through its existing single-flight schedule.
    pub(crate) fn refresh_listing(&mut self, now: Instant) {
        if !self.running.contains_key(&Slot::Listing) {
            self.due.insert(Slot::Listing, now);
        }
    }

    /// The last listing failed and no scheduled/manual retry owns its slot.
    pub(crate) fn listing_retryable(&self) -> bool {
        self.failures > 0 && !self.running.contains_key(&Slot::Listing)
    }

    pub fn new(every: Intervals) -> Self {
        Self {
            every,
            ..Self::default()
        }
    }

    /// The reads due at `now` that are not in flight, now marked in flight;
    /// follows that ended lose their schedule, new ones are due at once.
    pub fn due(&mut self, now: Instant, follows: &Follows) -> Vec<Read> {
        let mut wanted: Vec<Read> = vec![Read::Listing];
        wanted.extend(
            follows
                .activity
                .iter()
                .map(|(key, name, after)| Read::Activity {
                    key: key.clone(),
                    name: name.clone(),
                    after: *after,
                }),
        );
        wanted.extend(follows.processes.iter().map(|(key, name)| Read::Processes {
            key: key.clone(),
            name: name.clone(),
        }));
        let slots: Vec<Slot> = wanted.iter().map(Read::slot).collect();
        self.due.retain(|slot, _| slots.contains(slot));
        self.started.retain(|slot, _| slots.contains(slot));
        self.refreshes.retain(|slot| slots.contains(slot));
        let mut reads = Vec::new();
        for read in wanted {
            let slot = read.slot();
            if self.running.contains_key(&slot) {
                continue;
            }
            let at = *self.due.entry(slot.clone()).or_insert(now);
            if at <= now {
                self.started.insert(slot.clone(), now);
                self.refreshes.remove(&slot);
                self.running.insert(slot, ());
                reads.push(read);
            }
        }
        reads
    }

    /// A read finished at `now`: the next of its kind is due one interval
    /// after.
    pub fn done(&mut self, read: &Read, now: Instant) {
        let slot = read.slot();
        self.running.remove(&slot);
        let every = match read {
            Read::Listing => self.every.listing,
            Read::Activity { .. } => self.every.activity,
            Read::Processes { .. } => self.every.processes,
        };
        let due = if self.refreshes.contains(&slot) {
            now.max(self.started.get(&slot).copied().unwrap_or(now) + Duration::from_secs(1))
        } else {
            now + every
        };
        self.due.insert(slot, due);
    }

    /// A one-shot processes read, including while polling an older CLI. Keep
    /// one in flight and a one-second minimum gap; repeated requests coalesce.
    pub fn refresh_processes(&mut self, key: &str, now: Instant) -> bool {
        let slot = Slot::Processes(key.into());
        let Some(started) = self.started.get(&slot).copied() else {
            return false;
        };
        self.refreshes.insert(slot.clone());
        if !self.running.contains_key(&slot) {
            self.due
                .insert(slot, now.max(started + Duration::from_secs(1)));
        }
        true
    }

    /// When the next read not in flight is due.
    pub fn next(&self) -> Option<Instant> {
        self.due
            .iter()
            .filter(|(slot, _)| !self.running.contains_key(*slot))
            .map(|(_, at)| *at)
            .min()
    }

    /// A listing's outcome: Some(attempt) after a failure, None after a
    /// success; and whether this is the first success.
    pub fn listed(&mut self, ok: bool) -> (Option<u32>, bool) {
        if ok {
            let first = !self.listed || self.failures > 0;
            self.listed = true;
            self.failures = 0;
            (None, first)
        } else {
            self.failures += 1;
            (Some(self.failures), false)
        }
    }

    /// Whether a listing has succeeded since polling started.
    pub fn has_listed(&self) -> bool {
        self.listed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_listing_resnapshots_keep_a_gap_across_generations() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        let follows = Follows {
            activity: &[],
            processes: &[],
        };
        schedule.due(now, &follows);
        schedule.done(&Read::Listing, now);
        schedule.refresh_listing_bounded(now);
        schedule.refresh_listing_bounded(now + Duration::from_millis(10));
        assert!(schedule
            .due(now + Duration::from_millis(999), &follows)
            .is_empty());
        assert_eq!(
            schedule.due(now + Duration::from_secs(1), &follows),
            [Read::Listing]
        );
        schedule.refresh_listing_bounded(now + Duration::from_secs(1));
        schedule.done(&Read::Listing, now + Duration::from_secs(1));
        assert!(schedule
            .due(now + Duration::from_millis(1999), &follows)
            .is_empty());
        assert_eq!(
            schedule.due(now + Duration::from_secs(2), &follows),
            [Read::Listing]
        );
    }

    #[test]
    fn an_offline_listing_retry_uses_the_existing_single_flight_slot() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        let follows = Follows {
            activity: &[],
            processes: &[],
        };
        assert_eq!(schedule.due(now, &follows), [Read::Listing]);
        schedule.listed(false);
        assert!(
            !schedule.listing_retryable(),
            "a running listing owns its retry"
        );
        schedule.refresh_listing(now);
        schedule.done(&Read::Listing, now);
        assert!(schedule.listing_retryable());
        assert!(
            schedule.due(now, &follows).is_empty(),
            "no extra read while pending"
        );
        schedule.refresh_listing(now);
        schedule.refresh_listing(now);
        assert_eq!(schedule.due(now, &follows), [Read::Listing]);
        assert!(!schedule.listing_retryable());
        assert!(schedule.due(now, &follows).is_empty());
        schedule.done(&Read::Listing, now);
        schedule.listed(true);
        assert!(
            !schedule.listing_retryable(),
            "a healthy source needs no retry"
        );
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_process_refresh_is_one_bounded_read_after_the_gap() {
        let start = Instant::now();
        let mut schedule = Schedule::new(Intervals::default());
        let followed = [("run:r".into(), "codex/p/t".into())];
        let follows = Follows {
            activity: &[],
            processes: &followed,
        };
        let first = schedule.due(start, &follows);
        let read = first
            .iter()
            .find(|read| matches!(read, Read::Processes { .. }))
            .unwrap();
        for _ in 0..2 {
            assert!(schedule.refresh_processes("run:r", start));
        }
        schedule.done(read, start + Duration::from_millis(100));
        assert!(schedule
            .due(start + Duration::from_millis(999), &follows)
            .is_empty());
        let refreshed = schedule.due(start + SECOND, &follows);
        assert_eq!(refreshed.len(), 1);
        schedule.done(&refreshed[0], start + SECOND);
        assert!(schedule.due(start + SECOND * 2, &follows).is_empty());
        assert!(!schedule.refresh_processes("missing", start));
    }

    #[test]
    fn an_idle_process_refresh_waits_for_the_original_read_gap() {
        let start = Instant::now();
        let mut schedule = Schedule::new(Intervals::default());
        let followed = [("run:r".into(), "codex/p/t".into())];
        let follows = Follows {
            activity: &[],
            processes: &followed,
        };
        let first = schedule.due(start, &follows);
        let read = first
            .iter()
            .find(|read| matches!(read, Read::Processes { .. }))
            .unwrap();
        schedule.done(read, start + Duration::from_millis(100));
        for _ in 0..2 {
            assert!(schedule.refresh_processes("run:r", start + Duration::from_millis(100)));
        }
        assert!(schedule
            .due(start + Duration::from_millis(999), &follows)
            .is_empty());
        let refreshed = schedule.due(start + SECOND, &follows);
        assert_eq!(refreshed.len(), 1);
        assert!(matches!(refreshed[0], Read::Processes { .. }));
        schedule.done(&refreshed[0], start + SECOND);
        assert!(schedule.refresh_processes("run:r", start + SECOND * 4));
        assert_eq!(
            schedule.due(start + SECOND * 4, &follows).len(),
            1,
            "an idle read past the gap refreshes immediately"
        );
    }

    #[test]
    fn reads_follow_the_qt_cadence_with_one_in_flight_each() {
        let start = Instant::now();
        let mut schedule = Schedule::new(Intervals::default());
        let activity = vec![("run:r1".to_owned(), "codex/p/one".to_owned(), None)];
        let processes = vec![("run:r1".to_owned(), "codex/p/one".to_owned())];
        let follows = Follows {
            activity: &activity,
            processes: &processes,
        };
        let first = schedule.due(start, &follows);
        assert_eq!(first.len(), 3, "everything is due at once");
        assert!(
            schedule.due(start + SECOND, &follows).is_empty(),
            "all in flight"
        );
        for read in &first {
            schedule.done(read, start + SECOND);
        }
        assert_eq!(schedule.next(), Some(start + SECOND + ACTIVITY));
        let next = schedule.due(start + SECOND + ACTIVITY, &follows);
        assert!(matches!(next.as_slice(), [Read::Activity { .. }]));
        schedule.done(&next[0], start + SECOND * 4);
        let later = schedule.due(start + SECOND * 11, &follows);
        assert_eq!(later.len(), 2, "processes after 10 s, activity again");
        // An ended follow loses its schedule; the listing stays.
        for read in &later {
            schedule.done(read, start + SECOND * 11);
        }
        let none = Follows {
            activity: &[],
            processes: &[],
        };
        assert!(schedule.due(start + SECOND * 30, &none).is_empty());
        assert_eq!(schedule.next(), Some(start + SECOND + LISTING));
        assert!(matches!(
            schedule.due(start + SECOND + LISTING, &none).as_slice(),
            [Read::Listing]
        ));
    }

    #[test]
    fn listings_count_failures_and_reads_have_their_arguments() {
        let mut schedule = Schedule::new(Intervals::default());
        assert_eq!(schedule.listed(false), (Some(1), false));
        assert_eq!(schedule.listed(false), (Some(2), false));
        assert!(!schedule.has_listed());
        assert_eq!(schedule.listed(true), (None, true));
        assert_eq!(schedule.listed(true), (None, false));
        assert_eq!(schedule.listed(false), (Some(1), false));
        assert_eq!(schedule.listed(true), (None, true), "back after a failure");
        let activity = Read::Activity {
            key: "run:r".into(),
            name: "codex/p/t".into(),
            after: Some(41),
        };
        assert_eq!(
            activity.args(),
            ["inspect", "codex/p/t", "--after", "41", "--skip-processes"]
        );
        let window = Read::Activity {
            key: "run:r".into(),
            name: "codex/p/t".into(),
            after: None,
        };
        assert_eq!(window.args()[3], "0");
        let processes = Read::Processes {
            key: "run:r".into(),
            name: "codex/p/t".into(),
        };
        assert_eq!(
            processes.args(),
            ["inspect", "codex/p/t", "--after", &i64::MAX.to_string()]
        );
        assert_eq!(Read::Listing.args(), ["ls", "--json"]);
        let peer = ComputerId::from("studio");
        assert_eq!(activity.timeout(&peer), Duration::from_secs(15));
        assert_eq!(
            activity.timeout(&ComputerId::local()),
            Duration::from_secs(5)
        );
        assert_eq!(Read::Listing.timeout(&ComputerId::local()), LISTING_TIMEOUT);
    }
}
