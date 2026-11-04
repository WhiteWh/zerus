//! All computers of the fleet: this one first, then the peers it lists.
//!
//! [`Fleet::start`] runs this computer's stream at once. Its `catalog.meta`
//! names the peers in `peers`, the SSH aliases `hgs @alias` takes; each gets
//! its own `hgs @alias watch --json`, keyed by the alias, never by the host
//! name its stream reports. A listing without a `peers` array (an older host)
//! keeps the previous peers, and a peer's own `peers` are ignored. A removed
//! alias stops its stream: its rows leave and it reports
//! [`ComputerStatus::Ended`](crate::ComputerStatus::Ended). Every change goes to
//! the caller's sink, which a [`Coalescer`](crate::Coalescer) usually drains.
use crate::stream::{self, StreamEvent, Timing};
use crate::{Change, ComputerId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use zerus_watch_protocol::{Control, SessionRef};
mod resources;

pub use crate::stream::FollowError;

/// Whether an alias from a listing can name a peer on the command line.
pub fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 255
        && !alias.starts_with(['-', '@'])
        && alias
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control())
}

struct Shared {
    hgs: PathBuf,
    timing: Timing,
    /// Whether the peers the local listing names get streams.
    peers: bool,
    streams: Mutex<BTreeMap<ComputerId, resources::Stream>>,
    resources: Option<resources::Graph>,
    /// The peers the local listing last named; None until one names them.
    listed: Mutex<Option<BTreeSet<String>>>,
    /// None once the fleet stops: no stream starts after that.
    events: Mutex<Option<Sender<StreamEvent>>>,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn start(
        &self,
        streams: &mut BTreeMap<ComputerId, resources::Stream>,
        computer: ComputerId,
    ) -> bool {
        let Some(events) = guard(&self.events).clone() else {
            return false;
        };
        let resources = match &self.resources {
            Some(graph) => match graph.admit() {
                Some(held) => Some(held),
                None => {
                    let _ = events.send(StreamEvent::Change(Change::Computer {
                        computer,
                        status: crate::ComputerStatus::Reconnecting { attempt: 1 },
                    }));
                    return false;
                }
            },
            None => None,
        };
        let handle = match &resources {
            Some(held) => stream::spawn_accounted(
                &self.hgs,
                &computer,
                self.timing.clone(),
                events,
                held.clone(),
            )
            .expect("start an accounted stream supervisor"),
            None => stream::spawn(&self.hgs, &computer, self.timing.clone(), events),
        };
        streams.insert(computer, resources::Stream { handle, resources });
        true
    }

    /// Starts the new peers and stops the removed ones (on their own threads,
    /// so their exits delay no other computer's changes).
    fn set_peers(&self, peers: &BTreeSet<String>) {
        let mut streams = guard(&self.streams);
        let removed: Vec<ComputerId> = streams
            .keys()
            .filter(|computer| computer.alias().is_some_and(|alias| !peers.contains(alias)))
            .cloned()
            .collect();
        for computer in removed {
            if let Some(handle) = streams.remove(&computer) {
                thread::spawn(move || handle.stop());
            }
        }
        for alias in peers {
            let computer = ComputerId(alias.clone());
            if !streams.contains_key(&computer) {
                self.start(&mut streams, computer);
            }
        }
    }
}

/// The streams of all computers; dropping it stops them.
pub struct Fleet {
    shared: Arc<Shared>,
    router: Option<JoinHandle<()>>,
}

/// Non-owning stop request. Keeping this control never charges a finished graph.
#[derive(Clone)]
pub struct FleetControl(Weak<Shared>);
impl FleetControl {
    pub fn request_stop(&self) {
        let Some(shared) = self.0.upgrade() else {
            return;
        };
        guard(&shared.events).take();
        if let Some(graph) = &shared.resources {
            graph.close();
        }
        for stream in guard(&shared.streams).values() {
            stream.request_stop();
        }
    }
}

impl Fleet {
    pub fn control(&self) -> FleetControl {
        FleetControl(Arc::downgrade(&self.shared))
    }
    /// Bypasses one offline source's retry delay. Repeated pending requests,
    /// online computers and computers without a source do nothing.
    pub fn reconnect(&self, computer: &ComputerId) -> bool {
        let mut streams = guard(&self.shared.streams);
        if let Some(stream) = streams.get(computer) {
            return stream.reconnect();
        }
        if self.shared.resources.is_some()
            && guard(&self.shared.listed)
                .as_ref()
                .is_some_and(|peers| peers.contains(&computer.0))
        {
            return self.shared.start(&mut streams, computer.clone());
        }
        false
    }

    /// Follows this computer and the peers it lists through `hgs` (a path or
    /// a name on `PATH`, see [`crate::default_hgs`]); changes go to `sink`.
    pub fn start(hgs: PathBuf, sink: Sender<Change>) -> Self {
        Self::start_with(hgs, sink, Timing::default())
    }

    pub fn start_with(hgs: PathBuf, sink: Sender<Change>, timing: Timing) -> Self {
        Self::launch(hgs, sink, timing, true, None)
    }

    /// Follows this computer only, without streams for its peers.
    pub fn start_local(hgs: PathBuf, sink: Sender<Change>, timing: Timing) -> Self {
        Self::launch(hgs, sink, timing, false, None)
    }

    /// Account the existing graph without adding a reader or pump. The host
    /// must retain this same span through its sink consumer and callbacks.
    pub fn start_accounted(
        hgs: PathBuf,
        sink: Sender<Change>,
        timing: Timing,
        resources: Arc<dyn Send + Sync>,
    ) -> Self {
        Self::launch(
            hgs,
            sink,
            timing,
            true,
            Some(resources::Graph::new(resources)),
        )
    }

    fn launch(
        hgs: PathBuf,
        sink: Sender<Change>,
        timing: Timing,
        peers: bool,
        resources: Option<resources::Graph>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            hgs,
            timing,
            peers,
            resources,
            streams: Mutex::new(BTreeMap::new()),
            listed: Mutex::new(None),
            events: Mutex::new(Some(tx)),
        });
        {
            let mut streams = guard(&shared.streams);
            shared.start(&mut streams, ComputerId::local());
        }
        let router = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("zerus-fleet".into())
                .spawn(move || route(&shared, rx, sink))
                .expect("start the fleet router")
        };
        Self {
            shared,
            router: Some(router),
        }
    }

    /// Follows a session's Activity: a subscription with `after`, or the
    /// newest `tail` events (at most 100). `key` must be the session's catalog
    /// key. At most 16 sessions per computer; a followed session stays
    /// followed across reconnects until [`Self::unfollow`] or its end.
    pub fn follow(
        &self,
        computer: &ComputerId,
        key: &str,
        session: SessionRef,
        after: Option<i64>,
        tail: u32,
    ) -> Result<(), FollowError> {
        if session.key() != key {
            return Err(FollowError::KeyMismatch);
        }
        let streams = guard(&self.shared.streams);
        let handle = streams.get(computer).ok_or(FollowError::UnknownComputer)?;
        handle.follow(session, after, tail).map(|_| ())
    }

    /// Stops following; false when it was not followed.
    pub fn unfollow(&self, computer: &ComputerId, key: &str) -> bool {
        guard(&self.shared.streams)
            .get(computer)
            .is_some_and(|handle| handle.unfollow(key))
    }

    /// Follows a session's processes on one computer (see
    /// [`crate::processes`]), independent of its Activity follow; returns the
    /// session's catalog key. A followed session stays followed across
    /// reconnects until [`Self::unfollow_processes`] or its end.
    pub fn follow_processes(
        &self,
        computer: &ComputerId,
        session: SessionRef,
    ) -> Result<String, FollowError> {
        let streams = guard(&self.shared.streams);
        let handle = streams.get(computer).ok_or(FollowError::UnknownComputer)?;
        handle.follow_processes(session)
    }

    /// Changes one processes subscription's interval (1–600 seconds), keeping
    /// its current inventory and carrying the choice across reconnects.
    pub fn set_processes_interval(
        &self,
        computer: &ComputerId,
        key: &str,
        interval_ms: u64,
    ) -> Result<(), FollowError> {
        let streams = guard(&self.shared.streams);
        streams
            .get(computer)
            .ok_or(FollowError::UnknownComputer)?
            .set_processes_interval(key, interval_ms)
    }

    /// Requests one read after the server's minimum gap, without clearing state.
    pub fn refresh_processes(&self, computer: &ComputerId, key: &str) -> Result<(), FollowError> {
        let streams = guard(&self.shared.streams);
        streams
            .get(computer)
            .ok_or(FollowError::UnknownComputer)?
            .refresh_processes(key)
    }

    /// Stops following a session's processes; false when it was not followed.
    pub fn unfollow_processes(&self, computer: &ComputerId, key: &str) -> bool {
        guard(&self.shared.streams)
            .get(computer)
            .is_some_and(|handle| handle.unfollow_processes(key))
    }

    /// The sessions whose processes a computer follows, by key.
    pub fn followed_processes(&self, computer: &ComputerId) -> Vec<String> {
        guard(&self.shared.streams)
            .get(computer)
            .map(|stream| stream.followed_processes())
            .unwrap_or_default()
    }

    /// Follows a project's task list on one computer (see [`crate::tasks`]);
    /// returns its key, the project ID. A followed project stays followed
    /// across reconnects until [`Self::unfollow_tasks`] or its end.
    pub fn follow_tasks(
        &self,
        computer: &ComputerId,
        project: &str,
    ) -> Result<String, FollowError> {
        let streams = guard(&self.shared.streams);
        let handle = streams.get(computer).ok_or(FollowError::UnknownComputer)?;
        handle.follow_tasks(project)
    }

    /// Applies the preferences the host confirmed for a Mute or Unmute to the
    /// session's row on `target.computer` at once; the window and the menu
    /// bar get the changed row as any change. False for a computer without
    /// a stream.
    pub fn apply_notifications(
        &self,
        target: &crate::actions::Target,
        preferences: serde_json::Value,
    ) -> bool {
        guard(&self.shared.streams)
            .get(&ComputerId(target.computer.clone()))
            .is_some_and(|handle| handle.apply_notifications(target.clone(), preferences))
    }

    /// Stops following a project's task list; false when it was not followed.
    pub fn unfollow_tasks(&self, computer: &ComputerId, project: &str) -> bool {
        guard(&self.shared.streams)
            .get(computer)
            .is_some_and(|handle| handle.unfollow_tasks(project))
    }

    /// The projects whose task lists a computer follows.
    pub fn followed_tasks(&self, computer: &ComputerId) -> Vec<String> {
        guard(&self.shared.streams)
            .get(computer)
            .map(|stream| stream.followed_tasks())
            .unwrap_or_default()
    }

    /// Follows a computer's shared project catalog (see
    /// [`crate::organization`]), usually only this computer's. It stays
    /// followed across reconnects until [`Self::unfollow_organization`].
    pub fn follow_organization(&self, computer: &ComputerId) -> Result<(), FollowError> {
        let streams = guard(&self.shared.streams);
        let handle = streams.get(computer).ok_or(FollowError::UnknownComputer)?;
        handle.follow_organization()
    }

    /// Stops following a computer's organization; false when it was not.
    pub fn unfollow_organization(&self, computer: &ComputerId) -> bool {
        guard(&self.shared.streams)
            .get(computer)
            .is_some_and(|stream| stream.unfollow_organization())
    }

    /// The organization the view shows and the versions observed with it, for
    /// one digest: what `swarm apply {base, desired, versions}` starts from.
    pub fn organization_snapshot(
        &self,
        computer: &ComputerId,
    ) -> Option<crate::OrganizationSnapshot> {
        guard(&self.shared.streams)
            .get(computer)
            .and_then(|stream| stream.organization_snapshot())
    }

    /// Writes a control line (a `ping`, say) to one computer's stream.
    pub fn send(&self, computer: &ComputerId, control: Control) -> bool {
        guard(&self.shared.streams)
            .get(computer)
            .is_some_and(|handle| handle.send(control))
    }

    /// The peers this computer's listing names (valid aliases only), whether
    /// or not they get streams; None until a listing with a `peers` array has
    /// arrived. An older host's listing without one keeps the previous peers.
    pub fn peers(&self) -> Option<BTreeSet<String>> {
        guard(&self.shared.listed).clone()
    }

    /// The computers with a stream, this computer first.
    pub fn computers(&self) -> Vec<ComputerId> {
        guard(&self.shared.streams).keys().cloned().collect()
    }

    /// The sessions a computer follows, by key.
    pub fn followed(&self, computer: &ComputerId) -> Vec<String> {
        guard(&self.shared.streams)
            .get(computer)
            .map(|stream| stream.followed())
            .unwrap_or_default()
    }

    /// Diagnostic for the dormant host barrier; poison never becomes drain.
    pub fn accounting_available(&self) -> bool {
        self.shared
            .resources
            .as_ref()
            .is_some_and(resources::Graph::available)
            && !self.shared.events.is_poisoned()
            && !self.shared.streams.is_poisoned()
            && !self.shared.listed.is_poisoned()
    }

    /// Ends every stream (each reports `Ended` with no rows) and waits for them.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.shared.resources.is_some() {
            self.control().request_stop();
        } else {
            guard(&self.shared.events).take();
        }
        let handles: Vec<resources::Stream> = std::mem::take(&mut *guard(&self.shared.streams))
            .into_values()
            .collect();
        let stops: Vec<_> = handles
            .into_iter()
            .map(|handle| thread::spawn(move || handle.stop()))
            .collect();
        let deadline = Instant::now() + Duration::from_secs(1);
        for stop in stops {
            finish(stop, self.shared.resources.is_some(), deadline);
        }
        if let Some(router) = self.router.take() {
            finish(router, self.shared.resources.is_some(), deadline);
        }
    }
}

fn finish(thread: JoinHandle<()>, bounded: bool, deadline: Instant) {
    if bounded {
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !thread.is_finished() {
            return;
        } // The actual worker still retains its resource span.
    }
    let _ = thread.join();
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Forwards changes and follows the local listing's peers. It ends when every
/// stream has stopped and the fleet holds no sender any more.
fn route(shared: &Shared, rx: Receiver<StreamEvent>, sink: Sender<Change>) {
    let mut peers: Option<BTreeSet<String>> = None;
    while let Ok(event) = rx.recv() {
        match event {
            StreamEvent::Change(change) => {
                let _ = sink.send(change);
            }
            StreamEvent::Peers {
                computer,
                peers: listed,
            } => {
                if !computer.is_local() {
                    continue;
                }
                let wanted: BTreeSet<String> = listed
                    .into_iter()
                    .filter(|alias| valid_alias(alias))
                    .collect();
                *guard(&shared.listed) = Some(wanted.clone());
                if shared.peers && peers.as_ref() != Some(&wanted) {
                    shared.set_peers(&wanted);
                    peers = Some(wanted);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_must_be_one_safe_word() {
        for alias in ["studio", "build-01", "mac.example.com", "user@host"] {
            assert!(valid_alias(alias), "{alias}");
        }
        let long = "a".repeat(256);
        for alias in [
            "",
            "-oProxyCommand=x",
            "@studio",
            "two words",
            "tab\tbed",
            "line\n",
            long.as_str(),
        ] {
            assert!(!valid_alias(alias), "{alias:?}");
        }
    }
}
