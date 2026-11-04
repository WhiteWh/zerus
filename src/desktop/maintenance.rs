//! Periodic update discovery and recovery-policy replication. The background
//! owner supplies activation and authenticated frontend routing; this module
//! never registers a generic IPC operation, installs software or runs recovery.
#[path = "maintenance/io.rs"]
mod io;
#[path = "maintenance/state.rs"]
mod state;
#[cfg(test)]
#[path = "maintenance/tests.rs"]
mod tests;

use super::Store;
#[cfg(unix)]
use crate::platform;
use crate::{
    config::Config,
    desktop::{feed, fleet},
};
use anyhow::{ensure, Result};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_client_core::{
    machine::MachineMeta,
    maintenance::{self as dto, Configuration, RecoveryProjection, Request, UpdateProjection},
    recovery_settings::{self as recovery, Call, Effect},
};

pub(crate) use state::migrate;

/// A bounded read-only lookup remains available after revoke/passive startup.
/// durable:true describes the ORIGINAL configuration, not a new query write.
pub(crate) fn result_request(store: &Store, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let result = (|| -> Result<Value> {
        ensure!(
            serde_json::to_vec(value)?.len() <= dto::CONFIG_LIMIT,
            "maintenance query too large"
        );
        let Request::Result {
            request_id,
            original_request_id,
        } = serde_json::from_value(value.clone())?
        else {
            anyhow::bail!("not a maintenance result query");
        };
        ensure!(
            canonical(&request_id) && canonical(&original_request_id),
            "invalid query ID"
        );
        let stored = state::load(&store.db)?;
        Ok(receipt(Some(&stored), &request_id, &original_request_id))
    })();
    result.unwrap_or_else(
        |_| json!({"request_id":id,"accepted":false,"durable":false,"code":"maintenance_refused"}),
    )
}
fn receipt(stored: Option<&state::Stored>, id: &str, original: &str) -> Value {
    match stored.and_then(|s| s.receipts.get(original)) {
        Some(receipt) => {
            json!({"request_id":id,"original_request_id":original,"accepted":true,"durable":true,
            "revision":receipt.revision,"recovery_save":receipt.recovery_save,"configured":true,
            "current":stored.is_some_and(|s|s.revision==receipt.revision)})
        }
        None => {
            json!({"request_id":id,"original_request_id":original,"accepted":false,"code":"unknown_request"})
        }
    }
}
const SLOTS: usize = 4;
const CALLS: usize = zerus_desktop_catalog::MAX_SOURCES as usize;
const FRESH: Duration = Duration::from_secs(30);
const TIMER: i64 = 15 * 60;
struct Lease;
#[derive(Clone)]
enum Work {
    Update(dto::UpdateConfiguration),
    Recovery(Call),
}
enum Answer {
    Update(Box<UpdateProjection>),
    Recovery(Result<serde_json::Map<String, Value>, String>),
}
type Runner = Arc<dyn Fn(Work, &AtomicBool, Arc<Lease>) -> Answer + Send + Sync>;
struct Job {
    revision: u64,
    origin: u64,
    work: Work,
    lease: Arc<Lease>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    receive: mpsc::Receiver<Answer>,
}
struct Source {
    incarnation: i64,
    generation: i64,
    at: Instant,
    card: MachineMeta,
    peers: Vec<String>,
}
pub(crate) struct Runtime {
    epoch: String,
    runner: Runner,
    stored: Option<state::Stored>,
    sync: recovery::RecoverySync,
    source: Option<Source>,
    origin: u64,
    next_recovery: Instant,
    next_update: i64,
    pending: VecDeque<Call>,
    jobs: Vec<Job>,
    stopping: bool,
    error: Option<&'static str>,
}
impl Runtime {
    /// Only the admitted background runtime calls this. Starting it alone does
    /// no network/CLI I/O: the actor must load a committed configuration first.
    pub fn start(config: &Config, epoch: &str) -> Result<Self> {
        let hgs = std::env::current_exe()?;
        let environment = [
            (
                "HGS_CONFIG_DIR".to_owned(),
                config.dir.to_string_lossy().into_owned(),
            ),
            (
                "HGS_STATE_DIR".to_owned(),
                config.state.to_string_lossy().into_owned(),
            ),
        ];
        Self::with_runner(
            epoch,
            Arc::new(move |work, cancel, lease| match work {
                Work::Update(config) => {
                    Answer::Update(Box::new(io::update(&config, cancel, lease)))
                }
                Work::Recovery(call) => {
                    let mut invocation = recovery::invocation(&call);
                    // The owned runner requires a real, closed stdin also for get.
                    invocation.stdin.get_or_insert_with(Vec::new);
                    let environment: Vec<_> = environment
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect();
                    let result = zerus_client_core::run::run_owned(
                        &hgs,
                        &invocation,
                        &environment,
                        cancel,
                        spawn,
                        lease,
                    )
                    .map_err(|_| {
                        "Recovery request did not complete. Reload its state before saving."
                            .to_owned()
                    })
                    .and_then(|stdout| {
                        recovery::answer(zerus_client_core::Exit::Code {
                            code: 0,
                            stdout,
                            stderr: String::new(),
                        })
                    });
                    Answer::Recovery(result)
                }
            }),
        )
    }
    fn with_runner(epoch: &str, runner: Runner) -> Result<Self> {
        ensure!(
            !epoch.is_empty() && epoch.len() <= 128,
            "invalid maintenance epoch"
        );
        Ok(Self {
            epoch: epoch.into(),
            runner,
            stored: None,
            sync: recovery::RecoverySync::new(""),
            source: None,
            origin: 0,
            next_recovery: Instant::now(),
            next_update: 0,
            pending: VecDeque::new(),
            jobs: Vec::new(),
            stopping: false,
            error: None,
        })
    }
    fn load(&mut self, store: &Store) -> Result<()> {
        if self.stored.is_none() {
            let mut stored = state::load(&store.db)?;
            for view in stored.updates.values_mut() {
                view.current_origin = false;
                view.cached = view.feed.valid;
                view.available = false;
                view.busy = false;
                view.ready = false;
            }
            self.sync =
                recovery::RecoverySync::new(&Value::Object(stored.policy.clone()).to_string());
            self.stored = Some(stored);
        }
        Ok(())
    }
    /// Consume the original native permit at the mutation boundary. Bodies,
    /// build identities and ready flags cannot construct this authority.
    pub fn frontend_request(
        &mut self,
        store: &mut Store,
        authority: crate::desktop::ui_authority::Validated,
    ) -> Value {
        let id = authority.id().to_owned();
        let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"code":code});
        match authority.into_request() {
            Ok(zerus_client_core::desktop::ui_requests::Request::MaintenanceConfigure {
                request_id,
                update,
                recovery,
            }) => {
                let value = serde_json::to_value(Request::Configure {
                    request_id,
                    update: update.map(|v| *v),
                    recovery: recovery.map(|v| *v),
                });
                match value {
                    Ok(value) => self.request(store, &value),
                    Err(_) => refused("maintenance_refused"),
                }
            }
            _ => refused("frontend_identity_expired"),
        }
    }
    fn request(&mut self, store: &mut Store, value: &Value) -> Value {
        let id = value
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        match self.request_inner(store, value) {
            Ok(result) => result,
            Err(_) => {
                json!({"request_id":id,"accepted":false,"durable":false,"code":"maintenance_refused"})
            }
        }
    }
    fn request_inner(&mut self, store: &mut Store, value: &Value) -> Result<Value> {
        ensure!(
            serde_json::to_vec(value)?.len() <= dto::CONFIG_LIMIT,
            "maintenance request too large"
        );
        self.load(store)?;
        let request: Request = serde_json::from_value(value.clone())?;
        match request {
            Request::Result {
                request_id,
                original_request_id,
            } => {
                ensure!(
                    canonical(&request_id) && canonical(&original_request_id),
                    "invalid request ID"
                );
                Ok(self.receipt(&request_id, &original_request_id))
            }
            Request::Configure {
                request_id,
                update,
                recovery,
            } => {
                ensure!(canonical(&request_id), "maintenance unavailable");
                let mut config = Configuration { update, recovery };
                ensure!(config.valid(), "invalid configuration");
                let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&config)?));
                let old = self.stored.as_ref().unwrap();
                if let Some(receipt) = old.receipts.get(&request_id) {
                    ensure!(receipt.fingerprint == fingerprint, "request changed");
                    return Ok(self.receipt(&request_id, &request_id));
                }
                ensure!(
                    !self.stopping && self.error.is_none(),
                    "maintenance unavailable"
                );

                let edit = config
                    .recovery
                    .as_mut()
                    .and_then(|c| c.policy.take().map(|form| (c.base.clone(), form)));
                let policy = if let Some((base, form)) = &edit {
                    ensure!(
                        old.receipts
                            .values()
                            .filter(|r| r.recovery_save == "unknown")
                            .count()
                            < state::UNKNOWN_REQUESTS,
                        "unsettled recovery edit history full"
                    );
                    ensure!(
                        self.recovery_current(store) && self.sync.can_save() && !self.sync.busy(),
                        "policy not ready"
                    );
                    ensure!(
                        recovery::compare(base, self.sync.envelope()) == std::cmp::Ordering::Equal,
                        "policy base changed"
                    );
                    let policy =
                        recovery::policy(base, &form.clone().into()).map_err(anyhow::Error::msg)?;
                    ensure!(
                        serde_json::to_vec(&policy)?.len() <= dto::POLICY_LIMIT,
                        "policy too large"
                    );
                    Some(policy)
                } else {
                    None
                };
                let mut next = old.clone();
                next.revision = next
                    .revision
                    .checked_add(1)
                    .filter(|n| *n <= i64::MAX as u64)
                    .ok_or_else(|| anyhow::anyhow!("revision exhausted"))?;
                if next.configuration.as_ref().and_then(|c| c.update.as_ref())
                    != config.update.as_ref()
                {
                    for view in next.updates.values_mut() {
                        view.current_origin = false;
                        view.available = false;
                        view.ready = false;
                    }
                }
                // Carry the UI's existing historical shared-policy cache into
                // the same core model. It grants no machine/source currentness.
                let seed = config.recovery.as_ref().filter(|recovery| {
                    edit.is_none()
                        && !recovery.base.is_empty()
                        && (next.policy.is_empty()
                            || recovery::compare(&recovery.base, &next.policy)
                                == std::cmp::Ordering::Greater)
                });
                if let Some(recovery) = seed {
                    next.policy = recovery.base.clone();
                }
                next.configuration = Some(config);
                // An edit is spent BEFORE dispatch. Restart retains unknown,
                // never the form/operation, so a lost reply cannot replay it.
                next.receipts.insert(
                    request_id.clone(),
                    state::Receipt {
                        fingerprint,
                        revision: next.revision,
                        recovery_save: if edit.is_some() {
                            "unknown"
                        } else {
                            "not_requested"
                        }
                        .into(),
                    },
                );
                state::save(&mut store.db, &mut next)?;
                for job in &self.jobs {
                    job.cancel.store(true, Ordering::Release);
                }
                self.pending.clear();
                self.stored = Some(next);
                if let (Some((base, _)), Some(policy)) = (edit, policy) {
                    let effects = self.sync.save(policy, &base);
                    if self.effects(store, effects).is_err() {
                        self.error = Some("maintenance_recovery_unknown");
                        self.request_stop();
                    }
                } else {
                    self.sync = recovery::RecoverySync::new(
                        &Value::Object(self.stored.as_ref().unwrap().policy.clone()).to_string(),
                    );
                }
                self.next_update = 0;
                self.next_recovery = Instant::now();
                Ok(self.receipt(&request_id, &request_id))
            }
        }
    }
    fn receipt(&self, id: &str, original: &str) -> Value {
        receipt(self.stored.as_ref(), id, original)
    }
    /// Only AFTER a successful application of the original owned FleetEvent.
    pub fn observe(&mut self, store: &Store, event: &fleet::FleetEvent) {
        if event
            .packet
            .scope()
            .is_some_and(|(computer, _)| computer.is_empty())
            && self.observe_inner(store, event).is_err()
        {
            self.invalidate();
        }
    }
    pub fn reject(&mut self, event: &fleet::FleetEvent) {
        if event
            .packet
            .scope()
            .is_none_or(|(computer, _)| computer.is_empty())
        {
            self.invalidate();
        }
    }
    fn observe_inner(&mut self, store: &Store, event: &fleet::FleetEvent) -> Result<()> {
        self.observe_packet(store, &event.packet)
    }
    fn observe_packet(&mut self, store: &Store, packet: &feed::Packet) -> Result<()> {
        let (_, generation) = packet
            .scope()
            .ok_or_else(|| anyhow::anyhow!("missing source"))?;
        ensure!(
            !matches!(
                packet,
                feed::Packet::Offline { .. } | feed::Packet::Remove { .. }
            ),
            "source offline"
        );
        let (incarnation, actual, raw): (i64,i64,String) = store.db.query_row(
            "SELECT incarnation,generation,meta FROM desktop_catalog_sources WHERE computer='' AND epoch=?1 AND active=1 AND complete=1", [&self.epoch], |r| {
                let raw = r.get_ref(2)?.as_str().map_err(|_| rusqlite::Error::InvalidQuery)?;
                if raw.len() > zerus_desktop_catalog::PAGE_BYTES { return Err(rusqlite::Error::InvalidQuery); }
                Ok((r.get(0)?,r.get(1)?,raw.to_owned()))
            })?;
        ensure!(
            u64::try_from(actual).ok() == Some(generation),
            "source changed"
        );
        let meta: Value = serde_json::from_str(&raw)?;
        let peers = meta
            .get("peers")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("missing peer set"))?;
        ensure!(peers.len() < CALLS, "peer capacity");
        let mut names = Vec::new();
        for peer in peers {
            let peer = peer
                .as_str()
                .filter(|s| zerus_client_core::fleet::valid_alias(s))
                .ok_or_else(|| anyhow::anyhow!("invalid peer"))?;
            if !names.iter().any(|n| n == peer) {
                names.push(peer.to_owned());
            }
        }
        if self.source.as_ref().is_some_and(|s| {
            s.incarnation != incarnation
                || s.generation != actual
                || s.peers != names
                || s.card.local_sessions != MachineMeta::project(&meta).local_sessions
        }) {
            self.invalidate();
        }
        self.source = Some(Source {
            incarnation,
            generation: actual,
            at: Instant::now(),
            card: MachineMeta::project(&meta),
            peers: names,
        });
        Ok(())
    }
    fn recovery_current(&self, store: &Store) -> bool {
        let Some(source) = &self.source else {
            return false;
        };
        source.at.elapsed() <= FRESH && store.db.query_row(
            "SELECT incarnation,generation FROM desktop_catalog_sources WHERE computer='' AND epoch=?1 AND active=1 AND complete=1", [&self.epoch],
            |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional().ok().flatten() == Some((source.incarnation,source.generation))
    }
    fn invalidate(&mut self) {
        self.source = None;
        self.origin = self.origin.saturating_add(1);
        self.pending.clear();
        for job in &self.jobs {
            if matches!(job.work, Work::Recovery(_)) {
                job.cancel.store(true, Ordering::Release);
            }
        }
        let policy = self
            .stored
            .as_ref()
            .map(|s| Value::Object(s.policy.clone()).to_string())
            .unwrap_or_default();
        self.sync = recovery::RecoverySync::new(&policy);
    }
    fn effects(&mut self, store: &mut Store, effects: Vec<Effect>) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Call(call) => {
                    ensure!(self.pending.len() < CALLS, "recovery queue full");
                    ensure!(
                        serde_json::to_vec(&call.data)?.len() <= dto::POLICY_LIMIT,
                        "recovery policy too large"
                    );
                    self.pending.push_back(call);
                }
                Effect::Persist(policy) => {
                    ensure!(
                        policy.len() <= dto::POLICY_LIMIT,
                        "recovery policy too large"
                    );
                    let mut next = self.stored.as_ref().unwrap().clone();
                    next.policy = serde_json::from_str(&policy)?;
                    state::save(&mut store.db, &mut next)?;
                    self.stored = Some(next);
                }
            }
        }
        Ok(())
    }
    /// No network/CLI I/O; only completed workers are joined here. Results are fenced by configuration
    /// revision and native-source origin, then committed here before projection.
    pub fn poll(&mut self, store: &mut Store) {
        if self.poll_inner(store).is_err() {
            self.error = Some("maintenance_store_unavailable");
            self.request_stop();
        }
    }
    fn poll_inner(&mut self, store: &mut Store) -> Result<()> {
        self.load(store)?;
        let mut index = 0;
        while index < self.jobs.len() {
            if !self.jobs[index]
                .thread
                .as_ref()
                .is_some_and(|t| t.is_finished())
            {
                index += 1;
                continue;
            }
            let mut job = self.jobs.remove(index);
            let joined = job.thread.take().unwrap().join().is_ok();
            if !joined || Arc::strong_count(&job.lease) != 1 {
                self.error = Some("maintenance_io_unknown");
                // Retain the original lease. A pipe retaining it is not quiet.
                self.jobs.push(job);
                self.request_stop();
                break;
            }
            let answer = job.receive.try_recv().ok();
            let current = !job.cancel.load(Ordering::Acquire)
                && job.revision == self.stored.as_ref().unwrap().revision;
            if !current {
                continue;
            }
            match (job.work, answer) {
                (Work::Update(config), Some(Answer::Update(mut update))) => {
                    let old = self
                        .stored
                        .as_ref()
                        .unwrap()
                        .updates
                        .get(config.channel.as_str());
                    update.last_attempt = old.map_or(0, |v| v.last_attempt);
                    if !update.ready {
                        if let Some(old) = old {
                            update.feed = old.feed.clone();
                            update.last_success = old.last_success;
                            update.cached = update.feed.valid;
                        }
                    }
                    update.busy = false;
                    update.current_origin = update.ready;
                    let mut next = self.stored.as_ref().unwrap().clone();
                    next.updates.insert(config.channel.as_str().into(), *update);
                    state::save(&mut store.db, &mut next)?;
                    self.stored = Some(next);
                }
                (Work::Recovery(call), Some(Answer::Recovery(result)))
                    if job.origin == self.origin =>
                {
                    let saved = call.op == recovery::Op::Set && result.is_ok();
                    let effects = self.sync.answered(call.id, result);
                    self.effects(store, effects)?;
                    if call.op == recovery::Op::Set {
                        let mut next = self.stored.as_ref().unwrap().clone();
                        for receipt in next
                            .receipts
                            .values_mut()
                            .filter(|r| r.revision == job.revision && r.recovery_save == "unknown")
                        {
                            receipt.recovery_save = if saved && self.sync.error().is_empty() {
                                "saved"
                            } else {
                                "unknown"
                            }
                            .into();
                        }
                        state::save(&mut store.db, &mut next)?;
                        self.stored = Some(next);
                    }
                }
                (Work::Recovery(_), _) if job.origin != self.origin => (),
                _ => anyhow::bail!("missing maintenance result"),
            }
        }
        if self.stopping {
            return Ok(());
        }
        let Some(config) = self.stored.as_ref().and_then(|s| s.configuration.clone()) else {
            return Ok(());
        };
        if config.recovery.is_some() && self.recovery_current(store) {
            let source = self.source.as_ref().unwrap();
            let due = Instant::now() >= self.next_recovery;
            let effects = recovery::tick(
                &mut self.sync,
                Some(&source.card),
                Some(source.peers.clone()),
                due,
            );
            if due {
                self.next_recovery = Instant::now() + recovery::REFRESH;
            }
            self.effects(store, effects)?;
        } else if self.source.is_some() && !self.recovery_current(store) {
            self.invalidate();
        }
        let now = unix_now();
        if let Some(update) = config.update.filter(|c| c.automatic) {
            if now >= self.next_update
                && !self.jobs.iter().any(|j| matches!(j.work, Work::Update(_)))
                && self.jobs.len() < SLOTS
            {
                self.next_update = now + TIMER;
                let mut next = self.stored.as_ref().unwrap().clone();
                let state = next
                    .updates
                    .entry(update.channel.as_str().into())
                    .or_default();
                let interval = zerus_client_core::updates::automatic_interval(
                    if zerus_client_core::updates::windows_build(&state.installation) {
                        zerus_client_core::updates::Channel::Stable
                    } else {
                        update.channel
                    },
                ) as i64;
                if state.last_attempt <= 0
                    || state.last_attempt > now
                    || now - state.last_attempt >= interval
                {
                    state.last_attempt = now;
                    state.channel = Some(update.channel);
                    state.automatic = true;
                    state.busy = false;
                    state.current_origin = false;
                    state::save(&mut store.db, &mut next)?;
                    self.stored = Some(next);
                    self.launch(Work::Update(update))?;
                }
            }
        }
        while self.jobs.len() < SLOTS && self.recovery_current(store) {
            let Some(call) = self.pending.pop_front() else {
                break;
            };
            self.launch(Work::Recovery(call))?;
        }
        Ok(())
    }
    fn launch(&mut self, work: Work) -> Result<()> {
        ensure!(self.jobs.len() < SLOTS, "maintenance slots full");
        let (send, receive) = mpsc::sync_channel(1);
        let runner = self.runner.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let lease = Arc::new(Lease);
        let resource = lease.clone();
        let input = work.clone();
        let thread = thread::Builder::new()
            .name("hgs-maintenance".into())
            .spawn(move || {
                let result = runner(input, &cancelled, resource);
                let _ = send.send(result);
            })?;
        self.jobs.push(Job {
            revision: self.stored.as_ref().unwrap().revision,
            origin: self.origin,
            work,
            lease,
            cancel,
            thread: Some(thread),
            receive,
        });
        Ok(())
    }
    pub fn status(&self) -> Value {
        let config = self.stored.as_ref().and_then(|s| s.configuration.as_ref());
        let mut update = config
            .and_then(|c| c.update.as_ref())
            .map(|c| {
                let mut view = self
                    .stored
                    .as_ref()
                    .and_then(|s| s.updates.get(c.channel.as_str()))
                    .cloned()
                    .unwrap_or_default();
                view.channel = Some(c.channel);
                view.automatic = c.automatic;
                view.busy = self.jobs.iter().any(|j| {
                    matches!(j.work, Work::Update(_)) && !j.cancel.load(Ordering::Acquire)
                });
                // A persisted feed is cached, never a result about a different GUI.
                view.current_origin &= !self.stopping;
                view.cached = !view.current_origin && view.feed.valid;
                view
            })
            .unwrap_or_default();
        if self.stopping {
            update.current_origin = false;
        }
        let recovery_enabled = config.is_some_and(|c| c.recovery.is_some());
        let recovery_ready = recovery_enabled
            && self
                .source
                .as_ref()
                .is_some_and(|s| s.at.elapsed() <= FRESH)
            && !self.stopping;
        let recovery = RecoveryProjection {
            ready: recovery_ready,
            current_origin: recovery_ready,
            status: self.sync.status(),
            error: self.sync.error().into(),
            busy: self.sync.busy(),
            can_save: recovery_ready && self.sync.can_save(),
            known: self.sync.known(),
            has_machines: self.sync.has_machines(),
            has_policy: !self.sync.envelope().is_empty(),
            saves: self.sync.saves(),
            envelope: self.sync.envelope().clone(),
        };
        serde_json::to_value(dto::Projection {
            abi: dto::ABI.into(),
            epoch: self.epoch.clone(),
            revision: self.stored.as_ref().map(|s| s.revision),
            ready: self.stored.is_some() && !self.stopping && self.error.is_none(),
            error: self.error.map(str::to_owned),
            configuration: config.cloned(),
            updates: update,
            recovery,
        })
        .expect("maintenance projection contains only JSON values")
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.pending.clear();
        for job in &self.jobs {
            job.cancel.store(true, Ordering::Release);
        }
    }
    pub fn drained(&self) -> bool {
        self.stopping && self.jobs.is_empty()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
fn canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
}
fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        platform::spawn_in_turn(command)
    }
    #[cfg(not(unix))]
    {
        command.spawn()
    }
}
