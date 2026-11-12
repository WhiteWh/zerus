//! The router of the owner-approved question route (zerus-sa9qn.8, slice a),
//! Linux only in v1. It observes the local catalog on every fleet event,
//! independently of the notification setting, and keeps a bounded,
//! deduplicated queue of settling edges. A settled edge is read with at most
//! one bounded `hgs inspect` (plus its `hgs reference`) in flight per child
//! and [`READS`] in all, each with its own timeout, and read again
//! immediately before the reservation. Message text is never polled.
use super::{ledger, policy};
use crate::{
    config::Config,
    desktop::{
        deferred_send::{self, Routed},
        feed, fleet,
        store::{HandoffLock, Store},
    },
};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_client_core::desktop::{
    deferred_send as wire,
    question_route::{self as route, Child, Edge, Question, Record},
};
use zerus_desktop_catalog::{self as catalog, changes};

/// The service's own computer in the catalog.
const LOCAL: &str = "";
/// How long an edge settles before its first read.
pub(super) const SETTLE: Duration = Duration::from_secs(3);
/// The bounded queue of settling edges, one per catalog row.
pub(super) const PENDING: usize = 64;
/// Reads in flight at once, at most one per child.
pub(super) const READS: usize = 2;
/// Each `hgs inspect` or `hgs reference` read's own timeout.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const READ_BYTES: usize = 4 << 20;
/// Edges already read, by catalog row and attention signature.
const DONE: usize = 1024;
const SETTLE_PER_TURN: usize = 8;
const QUESTIONS_PER_READ: usize = 4;

/// One bounded read of a child: its `hgs inspect NAME --after i64::MAX`
/// and its `hgs reference NAME --json --run RUN --conversation ID`.
pub(super) struct Fetched {
    pub inspect: Value,
    pub reference: Value,
}
pub(super) type Reader = Arc<dyn Fn(&Edge) -> Option<Fetched> + Send + Sync>;

struct Source {
    scope: changes::Scope,
    since: i64,
    baseline: Option<catalog::Cursor>,
    baselined: bool,
    delta: Option<changes::Cursor>,
}
struct Candidate {
    edge: Edge,
    due: Instant,
}
enum Step {
    First,
    /// The read immediately before the reservation, of these questions.
    Confirm(Vec<Question>),
}
struct Read {
    key: String,
    edge: Edge,
    step: Step,
    worker: JoinHandle<()>,
    result: mpsc::Receiver<Option<Fetched>>,
}
#[derive(Default)]
struct Counts {
    routed: u64,
    refused: u64,
    unavailable: u64,
    dropped: u64,
    off: u64,
}

pub(in crate::desktop) struct Runtime {
    epoch: String,
    /// The CLI state root, where each child's own switch is kept.
    state: PathBuf,
    /// The person's global switch was off at the last turn: nothing is read.
    off: bool,
    reader: Option<Reader>,
    source: Option<Source>,
    candidates: BTreeMap<String, Candidate>,
    done: VecDeque<(String, String)>,
    seen: BTreeSet<(String, String)>,
    reads: Vec<Read>,
    stopping: bool,
    error: Option<String>,
    counts: Counts,
    #[cfg(test)]
    clock: Option<Instant>,
}

/// The production reader: the service's own image, with the service's
/// configuration and state roots.
fn reader(config: &Config) -> Result<Reader> {
    use zerus_client_core::{
        actions::{Exit, Invocation},
        run::run_limited,
    };
    let hgs = crate::self_exec::current()?;
    let env: Vec<(String, String)> = vec![
        ("HOME".into(), config.home.clone()),
        (
            "HGS_CONFIG_DIR".into(),
            config.dir.to_str().context("configuration path")?.into(),
        ),
        (
            "HGS_STATE_DIR".into(),
            config.state.to_str().context("state path")?.into(),
        ),
    ];
    Ok(Arc::new(move |edge: &Edge| {
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let read = |args: Vec<String>| -> Option<Value> {
            let invocation = Invocation {
                args,
                stdin: None,
                timeout: READ_TIMEOUT,
                request_id: None,
            };
            match run_limited(&hgs, &invocation, &env, READ_BYTES) {
                Ok(Exit::Code {
                    code: 0, stdout, ..
                }) => serde_json::from_slice(&stdout).ok(),
                _ => None,
            }
        };
        let inspect = read(vec![
            "inspect".into(),
            edge.name.clone(),
            "--after".into(),
            i64::MAX.to_string(),
            "--skip-processes".into(),
        ])?;
        let reference = read(vec![
            "reference".into(),
            edge.name.clone(),
            "--json".into(),
            "--run".into(),
            edge.run_id.clone(),
            "--conversation".into(),
            edge.conversation_id.clone(),
        ])?;
        Some(Fetched { inspect, reference })
    }))
}

/// The child's machine and exact reference, from its own `hgs reference`.
fn reference(fetched: &Fetched, edge: &Edge) -> Option<(String, String)> {
    let value = &fetched.reference;
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    (value["schema"] == 1
        && text("name") == edge.name
        && text("run_id") == edge.run_id
        && text("conversation_id") == edge.conversation_id
        && text("uri").starts_with("zerus://session?"))
    .then(|| (text("machine_id"), text("uri")))
}

impl Runtime {
    pub fn new(config: &Config, epoch: &str) -> Self {
        Self::with_reader(epoch, reader(config).ok(), config.state.clone())
    }
    fn with_reader(epoch: &str, reader: Option<Reader>, state: PathBuf) -> Self {
        Self {
            epoch: epoch.into(),
            state,
            off: false,
            reader,
            source: None,
            candidates: BTreeMap::new(),
            done: VecDeque::new(),
            seen: BTreeSet::new(),
            reads: Vec::new(),
            stopping: false,
            error: None,
            counts: Counts::default(),
            #[cfg(test)]
            clock: None,
        }
    }
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.clock {
            return now;
        }
        Instant::now()
    }
    fn fail(&mut self, error: anyhow::Error) {
        self.error = Some(format!("{error:#}"));
        self.reset();
    }
    fn reset(&mut self) {
        self.source = None;
        self.candidates.clear();
    }

    /// Called after the actor's own successful `Broker::apply` of `event`.
    pub fn observe(&mut self, store: &Store, event: &fleet::FleetEvent) {
        if let Err(error) = self.observe_packet(store, &event.packet) {
            self.fail(error);
        }
    }
    pub(super) fn observe_packet(&mut self, store: &Store, packet: &feed::Packet) -> Result<()> {
        if self.stopping || self.error.is_some() {
            return Ok(());
        }
        let Some((computer, generation)) = packet.scope() else {
            return Ok(());
        };
        // Only this computer's own rows: a remote listing never routes.
        if computer != LOCAL {
            return Ok(());
        }
        if matches!(
            packet,
            feed::Packet::Offline { .. } | feed::Packet::Remove { .. }
        ) {
            self.reset();
            return Ok(());
        }
        let mark = changes::watermark(&store.db, LOCAL)?;
        if !mark.complete
            || !mark.online
            || mark.scope.source.service_epoch != self.epoch
            || mark.scope.source.generation != generation
        {
            self.reset();
            return Ok(());
        }
        if self.source.as_ref().is_none_or(|s| s.scope != mark.scope) {
            self.reset();
            self.source = Some(Source {
                scope: mark.scope,
                since: mark.revision.context("missing route watermark")?,
                baseline: None,
                baselined: false,
                delta: None,
            });
        }
        Ok(())
    }
    pub fn reject(&mut self, event: &fleet::FleetEvent) {
        if event
            .packet
            .scope()
            .is_none_or(|(computer, _)| computer == LOCAL)
        {
            self.reset();
        }
    }
    fn current(&self, store: &Store) -> Result<bool> {
        let Some(source) = &self.source else {
            return Ok(false);
        };
        let mark = changes::watermark(&store.db, LOCAL)?;
        Ok(mark.complete
            && mark.online
            && mark.scope == source.scope
            && mark.scope.source.service_epoch == self.epoch)
    }

    pub fn poll(
        &mut self,
        store: &mut Store,
        send: &mut deferred_send::Runtime,
        exclusion: Option<Arc<HandoffLock>>,
    ) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.turn(store, send, exclusion) {
            self.fail(error);
        }
    }
    fn turn(
        &mut self,
        store: &mut Store,
        send: &mut deferred_send::Runtime,
        exclusion: Option<Arc<HandoffLock>>,
    ) -> Result<()> {
        // Settlement continues while stopping or off; nothing new starts.
        self.settle(store)?;
        // The person's global switch (zerus-sa9qn.32). Off reads nothing and
        // forgets the queue; on again reads the catalog from a new baseline,
        // so a question still pending and supervised then counts once, and
        // one answered while off is never seen. The reservation reads the
        // switch again in its own transaction.
        if !policy::enabled(&store.db)? {
            if !self.off {
                self.off = true;
                self.candidates.clear();
            }
        } else if self.off {
            self.off = false;
            // Every edge is considered afresh (zerus-sa9qn.45): a question
            // read before routing went off, and still pending, routes once;
            // the ledger never routes a question identity twice.
            self.seen.clear();
            self.done.clear();
            self.rebaseline(store)?;
        }
        self.collect(store, send, exclusion)?;
        if self.stopping || self.off {
            return Ok(());
        }
        self.fill(store)?;
        // Edges wait in the bounded queue until the actor can admit.
        if send.ready() {
            self.start()?;
        }
        Ok(())
    }
    /// Settles reserved routes from the actor's receipts only, never from a
    /// child's exit or reply.
    fn settle(&mut self, store: &mut Store) -> Result<()> {
        for (key, request_id) in ledger::reserved(&store.db, SETTLE_PER_TURN)? {
            let receipt = deferred_send::receipt(&store.db, &request_id)?;
            if let Some((state, code)) = ledger::settled(receipt.as_ref()) {
                ledger::settle(&mut store.db, &key, state, &code)?;
            }
        }
        Ok(())
    }
    /// Reads the local catalog again from a new baseline.
    fn rebaseline(&mut self, store: &Store) -> Result<()> {
        if let Some(source) = self.source.as_mut() {
            let mark = changes::watermark(&store.db, LOCAL)?;
            source.baselined = false;
            source.baseline = None;
            source.delta = None;
            source.since = mark.revision.context("route rebaseline watermark")?;
        }
        Ok(())
    }
    /// One bounded page of the local catalog: its baseline once (questions
    /// already pending count as new edges once), then its changes.
    fn fill(&mut self, store: &Store) -> Result<()> {
        if !self.current(store)? {
            return Ok(());
        }
        let source = self.source.as_mut().context("route source")?;
        let rows: Vec<(String, Option<String>)> = if !source.baselined {
            let page = catalog::page(&store.db, LOCAL, source.baseline.as_ref())?;
            if !page.complete {
                return Ok(());
            }
            source.baselined = page.next.is_none();
            source.baseline = page.next;
            page.rows
                .into_iter()
                .map(|r| (r.key, Some(r.json)))
                .collect()
        } else {
            match changes::page(
                &store.db,
                &source.scope,
                source.since,
                source.delta.as_ref(),
            ) {
                Ok(page) => {
                    if page.next.is_none() {
                        source.since = page.through;
                    }
                    source.delta = page.next;
                    page.items.into_iter().map(|c| (c.key, c.json)).collect()
                }
                Err(error) if error.downcast_ref::<changes::RetentionGap>().is_some() => {
                    // A lost interval is read again from a new baseline.
                    let mark = changes::watermark(&store.db, LOCAL)?;
                    source.baselined = false;
                    source.baseline = None;
                    source.delta = None;
                    source.since = mark.revision.context("route reset watermark")?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        };
        for (key, raw) in rows {
            self.consider(key, raw.as_deref());
        }
        Ok(())
    }
    fn consider(&mut self, key: String, raw: Option<&str>) {
        let edge = raw
            .filter(|raw| raw.len() <= catalog::PAGE_BYTES)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|row| route::edge(&row));
        let Some(edge) = edge else {
            self.candidates.remove(&key);
            return;
        };
        if self.seen.contains(&(key.clone(), edge.signature.clone())) {
            return;
        }
        let due = self.now() + SETTLE;
        if let Some(candidate) = self.candidates.get_mut(&key) {
            if candidate.edge != edge {
                candidate.edge = edge;
                candidate.due = due;
            }
            return;
        }
        if self.candidates.len() >= PENDING {
            self.counts.dropped += 1;
            return;
        }
        self.candidates.insert(key, Candidate { edge, due });
    }
    fn remember(&mut self, key: &str, signature: &str) {
        let entry = (key.to_owned(), signature.to_owned());
        if self.seen.insert(entry.clone()) {
            self.done.push_back(entry);
        }
        while self.done.len() > DONE {
            if let Some(old) = self.done.pop_front() {
                self.seen.remove(&old);
            }
        }
    }
    /// Forgets an edge that was read but not reserved because routing was
    /// off (zerus-sa9qn.45), so a later change of its row reads it again.
    /// The ledger, not this set, keeps a question from routing twice.
    fn forget(&mut self, key: &str, signature: &str) {
        let entry = (key.to_owned(), signature.to_owned());
        if self.seen.remove(&entry) {
            self.done.retain(|other| other != &entry);
        }
    }
    fn start(&mut self) -> Result<()> {
        let now = self.now();
        while self.reads.len() < READS && self.reader.is_some() {
            let busy: BTreeSet<&str> = self
                .reads
                .iter()
                .map(|r| r.edge.session_id.as_str())
                .collect();
            let Some(key) = self
                .candidates
                .iter()
                .find(|(_, c)| c.due <= now && !busy.contains(c.edge.session_id.as_str()))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            let candidate = self.candidates.remove(&key).context("route candidate")?;
            self.remember(&key, &candidate.edge.signature);
            self.spawn(key, candidate.edge, Step::First)?;
        }
        Ok(())
    }
    fn spawn(&mut self, key: String, edge: Edge, step: Step) -> Result<()> {
        let reader = self.reader.clone().context("route reader unavailable")?;
        let target = edge.clone();
        let (done, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("question-route-read".into())
            .spawn(move || {
                let _ = done.send(reader(&target));
            })?;
        self.reads.push(Read {
            key,
            edge,
            step,
            worker,
            result,
        });
        Ok(())
    }
    fn collect(
        &mut self,
        store: &mut Store,
        send: &mut deferred_send::Runtime,
        exclusion: Option<Arc<HandoffLock>>,
    ) -> Result<()> {
        let mut index = 0;
        while index < self.reads.len() {
            if !self.reads[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let Read {
                key,
                edge,
                step,
                worker,
                result,
            } = self.reads.remove(index);
            let fetched = worker
                .join()
                .ok()
                .and_then(|()| result.try_recv().ok())
                .flatten();
            let Some((machine, uri)) = fetched.as_ref().and_then(|f| reference(f, &edge)) else {
                continue;
            };
            if self.stopping {
                continue;
            }
            let inspect = &fetched.as_ref().context("route read")?.inspect;
            let found = route::questions(inspect, &edge, &machine);
            match step {
                Step::First => {
                    let mut fresh = Vec::new();
                    for question in found {
                        if !ledger::known(&store.db, &question.identity.key())? {
                            fresh.push(question);
                        }
                    }
                    fresh.truncate(QUESTIONS_PER_READ);
                    // Off reads nothing more; on again considers every
                    // edge afresh.
                    if !fresh.is_empty() && !self.off {
                        self.spawn(key, edge, Step::Confirm(fresh))?;
                    }
                }
                Step::Confirm(expected) => {
                    // Only a question still pending, unchanged, in this
                    // second read is reserved.
                    for question in expected.iter().filter(|q| found.contains(q)) {
                        self.route(
                            store,
                            send,
                            exclusion.clone(),
                            (&key, &edge),
                            question,
                            &uri,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    /// The child's own catalog row by key, in the visible local snapshot.
    fn row(store: &Store, key: &str) -> Result<Option<Value>> {
        let raw: Option<String> = store
            .db
            .query_row(
                "SELECT r.json FROM desktop_catalog_rows r JOIN desktop_catalog_sources s
                 ON s.visible=r.generation WHERE s.computer=?1 AND r.key=?2
                 AND length(CAST(r.json AS BLOB))<=?3",
                params![LOCAL, key, catalog::PAGE_BYTES as i64],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
    }
    /// The one visible local row with this session ID, from the indexed
    /// session expression; none when absent or ambiguous.
    pub(super) fn session_row(store: &Store, session_id: &str) -> Result<Option<Value>> {
        super::session_row(&store.db, session_id)
    }
    fn route(
        &mut self,
        store: &mut Store,
        send: &mut deferred_send::Runtime,
        exclusion: Option<Arc<HandoffLock>>,
        (key, edge): (&str, &Edge),
        question: &Question,
        reference: &str,
    ) -> Result<()> {
        if ledger::known(&store.db, &question.identity.key())? {
            return Ok(());
        }
        // The child is still this supervised, quiet edge in its current row.
        let still = Self::row(store, key)?
            .and_then(|row| route::edge(&row))
            .is_some_and(|now| {
                now.session_id == edge.session_id
                    && now.run_id == edge.run_id
                    && now.conversation_id == edge.conversation_id
                    && now.parent_session_id == edge.parent_session_id
            });
        if !still {
            return Ok(());
        }
        // The parent's own current live row of this computer.
        let Some(parent) = Self::session_row(store, &edge.parent_session_id)?
            .and_then(|row| route::parent(&row, edge))
        else {
            return Ok(());
        };
        let record = Record::new(
            question,
            Child {
                name: edge.name.clone(),
                reference: reference.into(),
            },
            parent,
        );
        let at = chrono::Utc::now().timestamp_millis();
        let intent = record.stdin().and_then(|original| {
            Some(wire::Intent {
                scope: send.scope().clone(),
                session: record.parent.session.clone(),
                request_id: record.request_id(),
                checkpoint: serde_json::to_string(&record).ok()?,
                original,
            })
        });
        let Some(intent) = intent.filter(|intent| intent.clone().verify_route().is_ok()) else {
            // Its fixed text or send is not valid: never routed.
            if ledger::refuse(&mut store.db, &record, at, ledger::INVALID).is_ok() {
                self.counts.refused += 1;
            }
            return Ok(());
        };
        let state = self.state.clone();
        match send.route(store, intent, exclusion, |tx| {
            ledger::reserve(tx, &record, at, &state)
        }) {
            Routed::Admitted => self.counts.routed += 1,
            Routed::Refused => self.counts.refused += 1,
            Routed::Unavailable => self.counts.unavailable += 1,
            Routed::Off => {
                self.counts.off += 1;
                self.forget(key, &edge.signature);
            }
        }
        Ok(())
    }

    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.candidates.clear();
    }
    pub fn status(&self) -> Value {
        let code = if self.error.is_some() {
            "question_route_unavailable"
        } else if self.reader.is_none() {
            "question_route_reader_unavailable"
        } else if self.stopping {
            "stopping"
        } else if self.off {
            "off"
        } else if self.source.is_some() {
            "running"
        } else {
            "awaiting_catalog"
        };
        json!({"profile": route::CAPABILITY, "code": code,
            "authority": "owner_approved_route", "pending": self.candidates.len(),
            "reading": self.reads.len(), "routed": self.counts.routed,
            "refused": self.counts.refused, "unavailable": self.counts.unavailable,
            "dropped": self.counts.dropped, "off": self.counts.off})
    }

    #[cfg(test)]
    pub(super) fn fixture(epoch: &str, reader: Reader, state: PathBuf) -> Self {
        Self::with_reader(epoch, Some(reader), state)
    }
    #[cfg(test)]
    pub(super) fn fixture_clock(&mut self, now: Instant) {
        self.clock = Some(now);
    }
    #[cfg(test)]
    pub(super) fn fixture_off(&self) -> u64 {
        self.counts.off
    }
    #[cfg(test)]
    pub(super) fn fixture_counts(&self) -> (u64, u64, u64, u64) {
        let c = &self.counts;
        (c.routed, c.refused, c.unavailable, c.dropped)
    }
    #[cfg(test)]
    pub(super) fn fixture_pending(&self) -> (usize, usize) {
        (self.candidates.len(), self.reads.len())
    }
    #[cfg(test)]
    pub(super) fn fixture_remembered(&self) -> (usize, usize) {
        (self.seen.len(), self.done.len())
    }
}
