//! Task #18 of team c536f0e5 (P2.6b, design 4.6 and open question 5; owner
//! 2026-11-23): the lead's team worker follows the catalog of each computer
//! a member works on, instead of listing it every tick.
//!
//! - One `hgs @PEER watch --json` per peer (through the lasting channel where
//!   the peer has one, P2.7). Its `catalog` topic is always on: the rows are
//!   exactly the `sessions` of `hgs ls --json --local --no-archives` there, and
//!   `catalog.meta` carries the listing's other fields. The worker composes
//!   the peer's listing from them, as `hgs @PEER ls` would have returned it.
//! - A delta that changes a session's state (a turn ending, a session
//!   starting, stopping or going away) wakes the worker at once; a change of
//!   a busy session's times alone only updates the rows.
//! - A peer whose stream is not following (still connecting, ended, a gap,
//!   silent past [`SILENT`]) is listed as before, at most every
//!   [`FALLBACK_AGE`], and the stream starts again after a growing delay.
//! - A member computer whose catalog changed is asked for its member outbox
//!   (reports kept there when the member cannot reach this computer) at once,
//!   not after the 15 s collection gap.
// The streams run in the worker only; tests drive the parts.
#![cfg_attr(test, allow(dead_code))]
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use zerus_watch_protocol::{topic, Body};

/// A stream that sent nothing (not even its 10 s heartbeat) for this long is
/// not followed.
const SILENT: Duration = Duration::from_secs(25);
/// A fallback listing of a peer is used again for this long (the worker's own
/// listing is never older than 3 s either).
const FALLBACK_AGE: Duration = Duration::from_secs(3);
/// The first and the longest delay before an ended stream starts again.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(60);
/// A stream that followed this long starts again after the first delay.
const RETRY_RESET: Duration = Duration::from_secs(60);

/// The row fields whose change is a change of the session's state for the
/// worker; anything else (times, tool details, samples) only updates the row.
const STATE_FIELDS: &[&str] = &[
    "name",
    "session_state",
    "state",
    "state_reason",
    "process_state",
    "runtime_state",
    "activity",
    "phase",
    "turn_started",
    "reply_id",
    "build",
    "provider_errors",
    "provider_error",
    "subagent_active_count",
    "conversation_id",
    "cwd",
];

/// One peer's catalog as its stream showed it.
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    rows: BTreeMap<String, Value>,
    /// The pages of a snapshot still arriving.
    pending: Option<BTreeMap<String, Value>>,
    meta: Option<Value>,
    host: Option<String>,
    /// A complete snapshot arrived and no gap or end came after it.
    ready: bool,
    last_frame: Option<Instant>,
}

impl Catalog {
    /// Applies one frame; true when a session's state changed (a row came,
    /// went, or changed one of [`STATE_FIELDS`]), or the catalog became
    /// ready or stopped being so.
    pub(crate) fn apply(&mut self, body: Body, at: Instant) -> bool {
        self.last_frame = Some(at);
        match body {
            Body::Hello(hello) => {
                self.host = Some(hello.host);
                false
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG => {
                if snapshot.page == 0 {
                    self.pending = Some(BTreeMap::new());
                }
                let pending = self.pending.get_or_insert_with(BTreeMap::new);
                for item in snapshot.items.unwrap_or_default() {
                    if let Some(key) = item["key"].as_str() {
                        pending.insert(key.to_owned(), item["row"].clone());
                    }
                }
                if !snapshot.last {
                    return false;
                }
                let rows = self.pending.take().unwrap_or_default();
                let changed = !self.ready || differs(&self.rows, &rows);
                self.rows = rows;
                self.ready = true;
                changed
            }
            Body::Snapshot(snapshot) if snapshot.topic == topic::CATALOG_META => {
                self.meta = snapshot.value;
                false
            }
            Body::Delta(delta) if delta.topic == topic::CATALOG => {
                let mut changed = !delta.remove.is_empty();
                for item in delta.upsert {
                    changed |= self.rows.get(&item.key).is_none_or(|old| state_changed(old, &item.row));
                    self.rows.insert(item.key, item.row);
                }
                for removal in delta.remove {
                    self.rows.remove(&removal.key);
                }
                changed && self.ready
            }
            Body::Gap(gap) if gap.topic == topic::CATALOG => {
                let was = self.ready;
                self.ready = false;
                self.pending = None;
                was
            }
            _ => false,
        }
    }

    /// The stream ended: nothing it showed is followed any more.
    fn ended(&mut self) -> bool {
        let was = self.ready;
        *self = Self::default();
        was
    }

    /// Whether the rows are the peer's now: a complete snapshot, no gap
    /// since, and a frame within [`SILENT`].
    pub(crate) fn following(&self, now: Instant) -> bool {
        self.ready && self.last_frame.is_some_and(|at| now.saturating_duration_since(at) < SILENT)
    }

    /// The peer's listing as `hgs @PEER ls --json --local --no-archives`
    /// returns it: `catalog.meta`'s fields with the rows as `sessions`.
    pub(crate) fn listing(&self, peer: &str) -> Value {
        let mut listing = self.meta.as_ref().and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
        if !listing.get("host").is_some_and(Value::is_string) {
            listing.insert("host".into(), json!(self.host.clone().unwrap_or_else(|| peer.to_owned())));
        }
        listing.insert("ok".into(), json!(true));
        listing.insert("sessions".into(), Value::Array(self.rows.values().cloned().collect()));
        Value::Object(listing)
    }
}

fn state_changed(old: &Value, new: &Value) -> bool {
    STATE_FIELDS.iter().any(|field| old.get(field) != new.get(field))
}

fn differs(old: &BTreeMap<String, Value>, new: &BTreeMap<String, Value>) -> bool {
    old.len() != new.len()
        || old.iter().zip(new.iter()).any(|((old_key, old_row), (new_key, new_row))| old_key != new_key || state_changed(old_row, new_row))
}

/// The peers' listings for a tick: the followed catalog of each peer whose
/// stream follows it, else what `fallback` lists. Also returns the peers
/// listed by `fallback`.
pub(crate) fn compose(
    peers: &[String],
    catalogs: &BTreeMap<String, Arc<Mutex<Catalog>>>,
    now: Instant,
    mut fallback: impl FnMut(&str) -> Value,
) -> (Vec<Value>, Vec<String>) {
    let mut listings = Vec::new();
    let mut listed = Vec::new();
    for peer in peers {
        let followed = catalogs.get(peer).and_then(|catalog| {
            let catalog = catalog.lock().unwrap_or_else(PoisonError::into_inner);
            catalog.following(now).then(|| catalog.listing(peer))
        });
        match followed {
            Some(listing) => listings.push(listing),
            None => {
                listings.push(fallback(peer));
                listed.push(peer.clone());
            }
        }
    }
    (listings, listed)
}

/// One peer's stream: a thread that runs and restarts the watch.
struct Follower {
    catalog: Arc<Mutex<Catalog>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<std::process::Child>>>,
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What a follower runs: `hgs @PEER watch --json` in the worker, a fake in
/// tests.
type Spawn = Arc<dyn Fn(&str) -> std::io::Result<std::process::Child> + Send + Sync>;

impl Follower {
    fn start(peer: String, spawn: Spawn, changed: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        let follower = Self { catalog: Arc::default(), stop: Arc::default(), child: Arc::default() };
        let (catalog, stop, slot) = (follower.catalog.clone(), follower.stop.clone(), follower.child.clone());
        let _ = std::thread::Builder::new().name(format!("hgs-team-catalog-{peer}")).spawn(move || {
            let mut delay = RETRY_FIRST;
            while !stop.load(Ordering::SeqCst) {
                let started = Instant::now();
                if let Ok(mut child) = spawn(&peer) {
                    let stdout = child.stdout.take();
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(child);
                    // Stopped while it started: the drop found no child to end.
                    if stop.load(Ordering::SeqCst) {
                        if let Some(mut child) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        break;
                    }
                    if let Some(stdout) = stdout {
                        read(stdout, &catalog, &|| changed(&peer));
                    }
                    if let Some(mut child) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                if catalog.lock().unwrap_or_else(PoisonError::into_inner).ended() {
                    changed(&peer);
                }
                delay = if started.elapsed() >= RETRY_RESET { RETRY_FIRST } else { delay };
                let until = Instant::now() + delay;
                while !stop.load(Ordering::SeqCst) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(100));
                }
                delay = (delay * 2).min(RETRY_LONGEST);
            }
        });
        follower
    }
}

/// Applies the stream's frames until it ends, telling `changed` after each
/// frame that changed a session's state.
fn read(stdout: impl std::io::Read, catalog: &Mutex<Catalog>, changed: &dyn Fn()) {
    use std::io::BufRead;
    for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { return };
        let Ok(frame) = zerus_watch_protocol::decode(&line) else { continue };
        if let Body::Error(error) = &frame.body {
            if error.fatal {
                return;
            }
        }
        let moved = catalog.lock().unwrap_or_else(PoisonError::into_inner).apply(frame.body, Instant::now());
        if moved {
            changed();
        }
    }
}

/// The worker's followers, by peer.
#[derive(Default)]
struct Followers {
    peers: BTreeMap<String, Follower>,
    /// Peers whose catalog changed since the worker last asked.
    changed: BTreeSet<String>,
}

static FOLLOWERS: Mutex<Option<Followers>> = Mutex::new(None);
static NOTIFIER: Mutex<Option<super::wake::Notifier>> = Mutex::new(None);
/// The fallback listings, with when each was read.
static FALLBACK: Mutex<BTreeMap<String, (Instant, Value)>> = Mutex::new(BTreeMap::new());

/// The worker's wake for a changed catalog.
pub(crate) fn set_notifier(notifier: Option<super::wake::Notifier>) {
    *NOTIFIER.lock().unwrap_or_else(PoisonError::into_inner) = notifier;
}

fn changed(peer: &str) {
    if let Some(followers) = FOLLOWERS.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        followers.changed.insert(peer.to_owned());
    }
    if let Some(notifier) = NOTIFIER.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        notifier.remote();
    }
}

/// `hgs @PEER watch --json`, as no session's own child; its stdin stays open
/// (the watch ends when it closes, also on the peer).
fn spawn_watch(peer: &str) -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command.args([format!("@{peer}").as_str(), "watch", "--json"]);
    for name in ["TMUX", "HGS_HOST_PANE", "HGS_SESSION", "HGS_RUN_ID", "DSH_SESSION_ID"] {
        command.env_remove(name);
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
}

/// Follows exactly these peers: a stream starts for a new one and ends for
/// one no longer named. Nothing starts in tests.
pub(crate) fn follow(peers: &[String]) {
    follow_with(peers, Arc::new(spawn_watch));
}

fn follow_with(peers: &[String], spawn: Spawn) {
    let wanted: BTreeSet<&String> = peers.iter().collect();
    let mut followers = FOLLOWERS.lock().unwrap_or_else(PoisonError::into_inner);
    let followers = followers.get_or_insert_with(Followers::default);
    followers.peers.retain(|peer, _| wanted.contains(peer));
    followers.changed.retain(|peer| wanted.contains(peer));
    // Tests start their followers themselves, never a real watch.
    if cfg!(test) {
        return;
    }
    for peer in wanted {
        if !followers.peers.contains_key(peer) {
            let follower = Follower::start(peer.clone(), spawn.clone(), Arc::new(changed));
            followers.peers.insert(peer.clone(), follower);
        }
    }
}

/// The peers' listings for this tick (see [`compose`]); `list` reads one
/// peer the old way.
pub(crate) fn listings(peers: &[String], list: impl Fn(&str) -> Value) -> Vec<Value> {
    let catalogs: BTreeMap<String, Arc<Mutex<Catalog>>> = FOLLOWERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|followers| followers.peers.iter().map(|(peer, follower)| (peer.clone(), follower.catalog.clone())).collect())
        .unwrap_or_default();
    let now = Instant::now();
    compose(peers, &catalogs, now, |peer| {
        let cache = FALLBACK.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, listing)) = cache.get(peer).filter(|(at, _)| now.saturating_duration_since(*at) < FALLBACK_AGE) {
            return listing.clone();
        }
        drop(cache);
        let listing = list(peer);
        FALLBACK.lock().unwrap_or_else(PoisonError::into_inner).insert(peer.to_owned(), (Instant::now(), listing.clone()));
        listing
    })
    .0
}

/// Why the worker cannot rely on events for a followed peer: one of them is
/// not followed now, so it is listed on every wake.
pub(crate) fn unfollowed() -> Option<&'static str> {
    let followers = FOLLOWERS.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    followers
        .as_ref()
        .is_some_and(|followers| {
            followers.peers.values().any(|follower| !follower.catalog.lock().unwrap_or_else(PoisonError::into_inner).following(now))
        })
        .then_some("remote listing")
}

/// The peers whose catalog changed since the last call.
pub(crate) fn take_changed() -> BTreeSet<String> {
    FOLLOWERS.lock().unwrap_or_else(PoisonError::into_inner).as_mut().map(|followers| std::mem::take(&mut followers.changed)).unwrap_or_default()
}

#[cfg(test)]
#[path = "tests/remote_catalog.rs"]
mod tests;
