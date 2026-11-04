//! Service-owned read acknowledgements and explicit automatic-approval policy.
//! The lifecycle owns activation/handoff. Executors never own SQLite or grant
//! themselves permission from a catalog row, UI flag or readiness response.
mod catalog;
mod state;
#[cfg(test)]
mod tests;

use super::{fleet, store::Store};
use crate::{approval_log, config::Config};
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_client_core::{
    actions, approvals::automatic as policy, Action, Exit, Invocation, Outcome, Target,
};
use zerus_desktop_ledger::reads;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    state::migrate(db)?;
    db.execute_batch(
        "CREATE INDEX desktop_delivery_catalog_name
        ON desktop_catalog_rows(generation,CASE WHEN json_valid(json) THEN json_extract(json,'$.name') END,id);",
    )?;
    Ok(())
}
const READS: usize = reads::MAX_IN_FLIGHT;
const APPROVALS: usize = 2;
const SLOTS: usize = READS + APPROVALS;
const RECENT: usize = 4096;
const SCAN: Duration = Duration::from_secs(1);
const COOLDOWN: Duration = Duration::from_secs(5);
const UNKNOWN: &str = "Answer delivery could not be confirmed. Check Activity or Terminal.";

/// Receipt lookup is read-only and remains available after revoke/restart,
/// without constructing a delivery executor or requiring a writer grant.
pub(super) fn result_request(store: &Store, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"code":code});
    let Ok(state::Request::DeliveryResult {
        request_id,
        original_request_id,
    }) = serde_json::from_value(value.clone())
    else {
        return refused("invalid_delivery_request");
    };
    state::result(&store.db, &request_id, &original_request_id)
        .unwrap_or_else(|_| refused("delivery_store_refused"))
}

pub(super) fn decorate_passive(store: &Store, value: &mut Value) {
    let enabled = state::policy(&store.db).ok().map(|policy| policy.enabled);
    value["automatic_approvals"] = json!(enabled);
    value["lifecycle"]["background"]["approvals"]["enabled"] = json!(enabled);
    for component in ["reads", "approvals"] {
        // No runtime is not evidence that an older owner has drained.
        value["lifecycle"]["background"][component]["drained"] = false.into();
    }
}

/// A reachable authenticated frontend can turn the durable opt-in OFF even
/// without background workers. No enable, generic JSON or writer grant here.
pub(super) fn passive_frontend_request(
    store: &mut Store,
    authority: super::ui_authority::Validated,
) -> Value {
    let id = authority.id().to_owned();
    let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"code":code});
    if authority.operation() != "automatic_approvals" {
        return refused("desktop_background_unavailable");
    }
    match authority.into_request() {
        Ok(zerus_client_core::desktop::ui_requests::Request::AutomaticApprovals {
            enabled: false,
            ..
        }) => state::set_policy(&mut store.db, &id, false)
            .unwrap_or_else(|_| refused("delivery_store_refused")),
        Ok(_) => refused("desktop_background_unavailable"),
        Err(_) => refused("frontend_identity_expired"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Approval,
}
struct Lease {
    kind: Kind,
}
type Runner = Arc<dyn Fn(&Invocation, usize, &AtomicBool, Arc<Lease>) -> Exit + Send + Sync>;

struct Audit {
    id: i64,
    request: String,
}
enum Phase {
    Read(reads::Delivery),
    Inspect(catalog::Candidate),
    Answer {
        candidate: catalog::Candidate,
        audit: Audit,
    },
}
impl Phase {
    fn candidate(&self) -> Option<&catalog::Candidate> {
        match self {
            Self::Inspect(candidate) | Self::Answer { candidate, .. } => Some(candidate),
            _ => None,
        }
    }
}
#[derive(Clone)]
enum Completion {
    NotStarted,
    Unknown,
    Read(reads::Outcome),
    Inspection(Option<Value>),
    Answer {
        status: &'static str,
        detail: String,
    },
}
enum Work {
    Read(reads::ReadMark),
    Inspect(policy::Candidate),
    Answer { target: Target, action: Action },
}
struct Job {
    phase: Phase,
    lease: Arc<Lease>,
    cancel: Arc<AtomicBool>,
    receive: mpsc::Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
    result: Option<Completion>,
}
impl Job {
    fn not_started(phase: Phase, lease: Arc<Lease>) -> Self {
        let (_, receive) = mpsc::sync_channel(1);
        Self {
            phase,
            lease,
            cancel: Arc::new(AtomicBool::new(false)),
            receive,
            thread: None,
            result: Some(Completion::NotStarted),
        }
    }
    fn ready(&mut self) -> bool {
        if self.result.is_some() {
            return true;
        }
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| !thread.is_finished())
        {
            return false;
        }
        let joined = self
            .thread
            .take()
            .expect("finished original worker")
            .join()
            .is_ok();
        // Freeze completeness at the original joined receipt. Later resource
        // closure cannot upgrade the result while its SQL finish is retried.
        self.result = Some(if joined && Arc::strong_count(&self.lease) == 1 {
            self.receive.try_recv().unwrap_or(Completion::Unknown)
        } else {
            Completion::Unknown
        });
        true
    }
}

pub(super) struct Runtime {
    epoch: String,
    run: Runner,
    approval_root: PathBuf,
    journal: Option<Connection>,
    sources: catalog::Sources,
    jobs: Vec<Job>,
    leases: Vec<Weak<Lease>>,
    recent: VecDeque<(String, Instant)>,
    policy: Option<state::Policy>,
    read_status: Value,
    read_error: Option<&'static str>,
    approval_error: Option<&'static str>,
    writer: bool,
    writer_token: Option<String>,
    stopping: bool,
    scan_at: Instant,
}
impl Runtime {
    /// No child, registration, service activation or journal constructor here.
    pub fn start(config: &Config, epoch: &str) -> Result<Self> {
        ensure!(state::canonical(epoch), "invalid delivery service epoch");
        let hgs = std::env::current_exe()?;
        // Config's selectors are UTF-8 strings. Pass the resolved selection,
        // rather than falling back to another config/state in a native child.
        let dir = config
            .dir
            .to_str()
            .context("delivery configuration path is not UTF-8")?
            .to_owned();
        let state_dir = config
            .state
            .to_str()
            .context("delivery state path is not UTF-8")?
            .to_owned();
        let run = Arc::new(
            move |inv: &Invocation, cap, cancel: &AtomicBool, lease: Arc<Lease>| {
                zerus_client_core::run::run_accounted(
                    &hgs,
                    inv,
                    &[("HGS_CONFIG_DIR", &dir), ("HGS_STATE_DIR", &state_dir)],
                    cap,
                    cancel,
                    lease,
                )
                .unwrap_or_else(|_| Exit::Abnormal {
                    stderr: String::new(),
                })
            },
        );
        Ok(Self::new(epoch, config.state.join("approval-audit"), run))
    }
    fn new(epoch: &str, approval_root: PathBuf, run: Runner) -> Self {
        Self {
            epoch: epoch.into(),
            run,
            approval_root,
            journal: None,
            sources: catalog::Sources::new(epoch),
            jobs: Vec::new(),
            leases: Vec::new(),
            recent: VecDeque::new(),
            policy: None,
            read_status: Value::Null,
            read_error: None,
            approval_error: None,
            writer: false,
            writer_token: None,
            stopping: false,
            scan_at: Instant::now(),
        }
    }

    /// Call only after successfully applying this actor's own fleet event.
    pub fn observe(&mut self, store: &Store, event: &fleet::FleetEvent) {
        if self.sources.observe(store, event).is_err() {
            self.read_error = Some("delivery_source_unavailable");
        }
        for job in &self.jobs {
            if job
                .phase
                .candidate()
                .is_some_and(|candidate| !self.sources.same(store, candidate).unwrap_or(false))
            {
                job.cancel.store(true, Ordering::Release);
            }
        }
    }

    /// All durable responses follow the actor commit. Retrying the exact ID
    /// returns its original receipt without reapplying a newer policy.
    pub fn request(&mut self, store: &mut Store, value: &Value) -> Value {
        let id = value["request_id"].as_str().unwrap_or("");
        let refused = |code| json!({"request_id":id,"accepted":false,"durable":false,"code":code});
        if value["op"] == "automatic_approvals" {
            return refused("frontend_authentication_required");
        }
        let Ok(request) = serde_json::from_value::<state::Request>(value.clone()) else {
            return refused("invalid_delivery_request");
        };
        if !state::canonical(request.id()) {
            return refused("invalid_request_id");
        }
        if self.stopping && !matches!(&request, state::Request::DeliveryResult { .. }) {
            return refused("delivery_stopping");
        }
        let result = match request {
            state::Request::ReadIntent { request_id, mark } => {
                state::enqueue(&mut store.db, &request_id, &mark.into(), now())
            }
            state::Request::DeliveryResult {
                request_id,
                original_request_id,
            } => state::result(&store.db, &request_id, &original_request_id),
        };
        result.unwrap_or_else(|_| refused("delivery_store_refused"))
    }

    pub fn frontend_request(
        &mut self,
        store: &mut Store,
        authority: super::ui_authority::Validated,
    ) -> Value {
        let request_id = authority.id().to_owned();
        let refused =
            |code| json!({"request_id":request_id,"accepted":false,"durable":false,"code":code});
        let Ok(zerus_client_core::desktop::ui_requests::Request::AutomaticApprovals {
            enabled,
            ..
        }) = authority.into_request()
        else {
            return refused("frontend_identity_expired");
        };
        if !state::canonical(&request_id) {
            return refused("invalid_request_id");
        }
        match state::policy_receipt(&store.db, &request_id, enabled) {
            Ok(Some(receipt)) => return receipt,
            Err(_) => return refused("delivery_store_refused"),
            Ok(None) => (),
        }
        if enabled
            && (!self.approvals_ready()
                || self.writer_epoch(store).ok().flatten().as_ref() != self.writer_token.as_ref())
        {
            return refused("desktop_background_unavailable");
        }
        if !enabled {
            self.cancel_approvals();
        }
        let result = state::set_policy(&mut store.db, &request_id, enabled);
        if result.is_err() {
            self.approval_error = Some("approval_policy_uncommitted");
            self.cancel_approvals();
        } else {
            if matches!(
                self.approval_error,
                Some("approval_policy_uncommitted" | "approval_policy_unavailable")
            ) {
                self.approval_error = None;
            }
            self.refresh_policy(store);
        }
        result.unwrap_or_else(|_| refused("delivery_store_refused"))
    }

    fn refresh_policy(&mut self, store: &Store) {
        match state::policy(&store.db) {
            Ok(policy) => {
                if self
                    .policy
                    .is_some_and(|old| old.revision != policy.revision)
                    || !policy.enabled
                {
                    self.cancel_approvals();
                }
                self.policy = Some(policy);
            }
            Err(_) => {
                self.approval_error = Some("approval_policy_unavailable");
                self.cancel_approvals();
            }
        }
    }
    fn writer_epoch(&self, store: &Store) -> Result<Option<String>> {
        let value: Option<(Option<String>, String, bool)> = store
            .db
            .query_row(
                "SELECT epoch,service_epoch,revoked FROM desktop_writer WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(value.and_then(|(epoch, service, revoked)| {
            epoch.filter(|epoch| !revoked && service == self.epoch && state::canonical(epoch))
        }))
    }
    fn cancel_approvals(&self) {
        for job in &self.jobs {
            if job.phase.candidate().is_some() {
                job.cancel.store(true, Ordering::Release);
            }
        }
    }
    fn lease(&mut self, kind: Kind) -> Option<Arc<Lease>> {
        self.leases.retain(|lease| lease.strong_count() > 0);
        let limit = if kind == Kind::Read { READS } else { APPROVALS };
        if self.leases.len() >= SLOTS
            || self
                .leases
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|lease| lease.kind == kind)
                .count()
                >= limit
        {
            return None;
        }
        let lease = Arc::new(Lease { kind });
        self.leases.push(Arc::downgrade(&lease));
        Some(lease)
    }
    fn launch(&self, phase: Phase, inv: Invocation, work: Work, lease: Arc<Lease>) -> Job {
        let (send, receive) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, resource, run) = (cancel.clone(), lease.clone(), self.run.clone());
        let thread = thread::Builder::new()
            .name("desktop-delivery".into())
            .spawn(move || {
                let result = if stop.load(Ordering::Acquire) {
                    Completion::NotStarted
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let cap = if matches!(&work, Work::Inspect(_)) {
                            8 << 20
                        } else {
                            1 << 20
                        };
                        let exit = run(&inv, cap, &stop, resource);
                        match work {
                            Work::Inspect(target) => Completion::Inspection(match exit {
                                Exit::Code {
                                    code: 0, stdout, ..
                                } => serde_json::from_slice(&stdout)
                                    .ok()
                                    .and_then(|details| policy::question(&target, &details)),
                                _ => None,
                            }),
                            Work::Read(mark) => {
                                let target = Target {
                                    computer: mark.computer.clone(),
                                    name: mark.name.clone(),
                                    run_id: mark.run_id.clone(),
                                    conversation_id: mark.conversation_id.clone(),
                                };
                                let action = Action::MarkRead {
                                    reply_id: mark.reply_id.clone(),
                                    attention_signature: mark.attention_signature.clone(),
                                };
                                let outcome = match actions::classify(&target, &action, &inv, exit)
                                {
                                    Outcome::Done { receipt, .. } => reads::Outcome::Delivered {
                                        applied_all: zerus_client_core::readsync::applied_all(
                                            &mark, &receipt,
                                        ),
                                    },
                                    _ => reads::Outcome::Failed,
                                };
                                Completion::Read(outcome)
                            }
                            Work::Answer { target, action } => {
                                let (status, detail) =
                                    match actions::classify(&target, &action, &inv, exit) {
                                        Outcome::Done { .. } => ("approved", String::new()),
                                        Outcome::Failed { detail } => ("failed", detail),
                                        Outcome::Uncertain { detail } => ("unknown", detail),
                                    };
                                Completion::Answer {
                                    status,
                                    detail: detail.chars().take(2000).collect(),
                                }
                            }
                        }
                    }))
                    .unwrap_or(Completion::Unknown)
                };
                let _ = send.try_send(result);
            });
        match thread {
            Ok(thread) => Job {
                phase,
                lease,
                cancel,
                receive,
                thread: Some(thread),
                result: None,
            },
            Err(_) => Job::not_started(phase, lease),
        }
    }

    /// No network, executor wait, whole-journal scan or UI callback on the actor.
    /// Native I/O lives on at most four read and two approval workers.
    pub fn poll(&mut self, store: &mut Store) {
        self.refresh_policy(store);
        let writer = match self.writer_epoch(store) {
            Ok(writer) => writer,
            Err(_) => {
                self.read_error = Some("delivery_writer_unavailable");
                self.approval_error = Some("delivery_writer_unavailable");
                None
            }
        };
        let changed = self.writer_token != writer;
        self.writer_token = writer.clone();
        self.writer = writer.is_some();
        if self.stopping || !self.writer || changed {
            for job in &self.jobs {
                job.cancel.store(true, Ordering::Release);
            }
        }
        let jobs = std::mem::take(&mut self.jobs);
        for mut job in jobs {
            if !job.ready() {
                self.jobs.push(job);
                continue;
            }
            match self.finish(store, &mut job) {
                Ok(true) => self.jobs.push(job),
                Ok(false) => (),
                Err(_) => {
                    match job.phase {
                        Phase::Read(_) => self.read_error = Some("read_receipt_uncommitted"),
                        _ => self.approval_error = Some("approval_receipt_uncommitted"),
                    }
                    self.jobs.push(job); // Same durable finish only, never native delivery.
                }
            }
        }
        if self.read_error == Some("read_receipt_uncommitted")
            && !self
                .jobs
                .iter()
                .any(|job| job.result.is_some() && matches!(&job.phase, Phase::Read(_)))
        {
            self.read_error = None;
        }
        if self.approval_error == Some("approval_receipt_uncommitted")
            && !self
                .jobs
                .iter()
                .any(|job| job.result.is_some() && job.phase.candidate().is_some())
        {
            self.approval_error = None;
        }
        self.leases.retain(|lease| lease.strong_count() > 0);
        self.read_status = store.read_status();
        self.read_status["admissions_paused"] = json!(
            self.stopping
                || !self.writer
                || self.read_error.is_some()
                || self.read_status["recovery_error"] != false
        );
        let Some(writer) = writer.filter(|_| !self.stopping) else {
            return;
        };
        if self.read_error.is_none() && self.read_status["recovery_error"] == false {
            if let Ok(computers) = self.sources.eligible(store) {
                for _ in 0..READS {
                    let Some(lease) = self.lease(Kind::Read) else {
                        break;
                    };
                    let delivery = match reads::reserve_for(
                        &mut store.db,
                        &writer,
                        &self.epoch,
                        &uuid::Uuid::new_v4().to_string(),
                        now(),
                        &computers,
                    ) {
                        Ok(Some(delivery)) => delivery,
                        Ok(None) => break,
                        Err(_) => {
                            self.read_error = Some("read_reservation_unavailable");
                            break;
                        }
                    };
                    let mark = delivery.pending.mark.clone();
                    let target = Target {
                        computer: mark.computer.clone(),
                        name: mark.name.clone(),
                        run_id: mark.run_id.clone(),
                        conversation_id: mark.conversation_id.clone(),
                    };
                    let action = Action::MarkRead {
                        reply_id: mark.reply_id.clone(),
                        attention_signature: mark.attention_signature.clone(),
                    };
                    let phase = Phase::Read(delivery);
                    let job = match actions::invocation(&target, &action, "") {
                        Ok(inv) => self.launch(phase, inv, Work::Read(mark), lease),
                        Err(_) => Job::not_started(phase, lease),
                    };
                    self.jobs.push(job);
                }
            }
        }
        if Instant::now() >= self.scan_at {
            self.scan_at = Instant::now() + SCAN;
            if self.sources.observe_reads(store).is_err() {
                self.read_error = Some("read_observation_unavailable");
            }
            match self.sources.page(store) {
                Ok(candidates) => {
                    if self.policy.is_some_and(|p| p.enabled) && self.approval_error.is_none() {
                        for candidate in candidates {
                            self.inspect(store, candidate);
                        }
                    }
                }
                Err(_) => self.read_error = Some("delivery_catalog_unavailable"),
            }
        }
    }

    fn inspect(&mut self, store: &Store, candidate: catalog::Candidate) {
        let key = format!(
            "{:x}",
            Sha256::digest(
                json!([
                    candidate.target.target.computer,
                    candidate.target.target.name,
                    candidate.target.target.run_id,
                    candidate.target.target.conversation_id
                ])
                .to_string()
            )
        );
        let now = Instant::now();
        while self
            .recent
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) >= COOLDOWN)
        {
            self.recent.pop_front();
        }
        if self.recent.iter().any(|(old, _)| old == &key)
            || self
                .jobs
                .iter()
                .filter_map(|job| job.phase.candidate())
                .any(|old| old.target.key == candidate.target.key)
            || !self.sources.same(store, &candidate).unwrap_or(false)
        {
            return;
        }
        let Some(lease) = self.lease(Kind::Approval) else {
            return;
        };
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back((key, now));
        let inv = zerus_client_core::details::details_invocation(&candidate.target.target, None);
        let work = Work::Inspect(candidate.target.clone());
        let job = self.launch(Phase::Inspect(candidate), inv, work, lease);
        self.jobs.push(job);
    }

    fn finish(&mut self, store: &mut Store, job: &mut Job) -> Result<bool> {
        let complete = Arc::strong_count(&job.lease) == 1;
        let result = job.result.as_ref().expect("joined completion");
        match &job.phase {
            Phase::Read(delivery) => {
                let outcome = match result {
                    Completion::NotStarted => reads::Outcome::NotSent,
                    Completion::Read(outcome) if complete => *outcome,
                    _ => reads::Outcome::Failed,
                };
                reads::finish(&mut store.db, delivery, outcome, now())?;
            }
            Phase::Answer { audit, .. } => {
                let (status, detail) = match result {
                    Completion::NotStarted => ("cancelled", policy::CANCELLED),
                    Completion::Answer { status, detail } if complete => (*status, detail.as_str()),
                    _ => ("unknown", UNKNOWN),
                };
                let db = self
                    .journal
                    .as_ref()
                    .context("missing owned approval journal")?;
                let reply=approval_log::finish(db,&json!({"id":audit.id,"request_id":audit.request,"status":status,"detail":detail}))
                    .map_err(|_|anyhow::anyhow!("approval finish unavailable"))?;
                ensure!(
                    reply["id"] == audit.id && reply["status"] == status,
                    "approval finish identity changed"
                );
            }
            Phase::Inspect(candidate) => {
                if !complete
                    || self.stopping
                    || !self.writer
                    || job.cancel.load(Ordering::Acquire)
                    || !self.policy.is_some_and(|p| p.enabled)
                    || self
                        .approval_error
                        .is_some_and(|error| error != "approval_receipt_uncommitted")
                    || !self.sources.same(store, candidate)?
                {
                    return Ok(false);
                }
                let Completion::Inspection(Some(question)) = result else {
                    return Ok(false);
                };
                if policy::once(&candidate.target.provider, question).is_none() {
                    return Ok(false);
                }
                if self.journal.is_none() {
                    self.journal = Some(
                        approval_log::open(&self.approval_root)
                            .map_err(|_| anyhow::anyhow!("approval journal unavailable"))?,
                    );
                }
                let host = if candidate.target.target.computer.is_empty() {
                    "local"
                } else {
                    &candidate.target.target.computer
                };
                let reply=approval_log::begin(self.journal.as_mut().expect("opened actor journal"),
                    &json!({"host":host,"session":candidate.target.target.name,"provider":candidate.target.provider,"question":question}))
                    .map_err(|_|anyhow::anyhow!("approval reservation unavailable"))?;
                let Some(reservation) =
                    policy::reserved(&reply, &candidate.target.provider, question)
                        .map_err(anyhow::Error::msg)?
                else {
                    return Ok(false);
                };
                let action = Action::Answer {
                    question: question.clone(),
                    answers: reservation.answers,
                };
                let target = candidate.target.target.clone();
                let audit = Audit {
                    id: reservation.id,
                    request: reservation.request,
                };
                let inv = actions::invocation(&target, &action, &audit.request);
                let phase = Phase::Answer {
                    candidate: candidate.clone(),
                    audit,
                };
                *job = match inv {
                    Ok(inv) => self.launch(
                        phase,
                        inv,
                        Work::Answer { target, action },
                        job.lease.clone(),
                    ),
                    Err(_) => Job::not_started(phase, job.lease.clone()),
                };
                // Retain the reserved Answer phase even if executor creation
                // fails. Its cancelled finish can retry without re-reserving.
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn request_stop(&mut self) {
        self.stopping = true;
        for job in &self.jobs {
            job.cancel.store(true, Ordering::Release);
        }
    }
    pub fn drained(&self) -> bool {
        self.stopping
            && self.jobs.is_empty()
            && self.leases.iter().all(|lease| lease.strong_count() == 0)
    }
    fn reads_ready(&self) -> bool {
        self.writer
            && !self.stopping
            && self.read_error.is_none()
            && self.read_status["recovery_error"] == false
    }
    fn approvals_ready(&self) -> bool {
        self.writer && !self.stopping && self.policy.is_some() && self.approval_error.is_none()
    }
    fn component(&self, ready: bool, policy: Option<bool>) -> Value {
        let code = if self.stopping {
            if self.drained() {
                "drained"
            } else {
                "draining"
            }
        } else if !self.writer {
            "awaiting_handoff"
        } else if !ready {
            "unavailable"
        } else if policy == Some(false) {
            "disabled"
        } else {
            "running"
        };
        json!({"ready":ready,"code":code,"drained":self.drained()})
    }
    /// Canonical display projection consumed by Next. Mutation admission uses
    /// the typed proof and current writer query, never these JSON booleans.
    pub fn decorate(&self, value: &mut Value) {
        value["lifecycle"]["background"]["reads"] = self.component(self.reads_ready(), None);
        let enabled = self.policy.map(|policy| policy.enabled);
        let mut approvals = self.component(self.approvals_ready(), enabled);
        approvals["enabled"] = json!(enabled);
        value["lifecycle"]["background"]["approvals"] = approvals;
        value["automatic_approvals"] = json!(enabled);
    }
    pub fn status(&self) -> Value {
        let active = self.jobs.len();
        let retained = self
            .leases
            .iter()
            .filter(|lease| lease.strong_count() > 0)
            .count();
        json!({"abi":"desktop-delivery-v1","service_epoch":self.epoch,"stopping":self.stopping,
            "writer_ready":self.writer,"active":active,"retained_slots":retained,"slot_limit":SLOTS,
            "reads":{"queue":self.read_status,"error":self.read_error,"limit":READS},
            "approvals":{"enabled":self.policy.map(|p|p.enabled),"revision":self.policy.map(|p|p.revision),
                "error":self.approval_error,"limit":APPROVALS},"drained":self.drained()})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
