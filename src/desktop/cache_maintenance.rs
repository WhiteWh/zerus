//! One explicitly requested, native-authenticated cache pass. Slow preparation
//! precedes a FRESH original native child; no permit is renewed. The actor owns
//! all storage, effect reservations, original workers and revoke accounting.
use super::{
    store::{Location, Store},
    ui_authority::Validated,
};
use crate::platform;
use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zerus_client_core::{desktop::cache_maintenance as wire, download_cache::maintenance as cache};
use zerus_desktop_ledger::operations::cache as ledger;

/// Assigned in merge order after the actual notification-controls migration.
pub(super) const SCHEMA: i64 = 18;
pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    ledger::migrate(tx)
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn hash(value: &impl serde::Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
pub(super) fn available(store: &Store) -> Result<bool> {
    Ok(store.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='desktop_cache_operations')",[],|r|r.get(0))?)
}
pub(super) fn undrained(store: &Store) -> Result<u64> {
    ledger::undrained(&store.db)
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Query {
    CacheMaintenanceResult {
        request_id: String,
        original_request_id: String,
    },
    CacheMaintenancePolicy {
        request_id: String,
        root: String,
    },
}
pub(super) fn query(store: &Store, configuration: &str, value: &Value) -> Value {
    let id = value["request_id"].as_str().unwrap_or("");
    let result = (|| -> Result<Value> {
        ensure!(
            serde_json::to_vec(value)?.len() <= 16 << 10 && available(store)?,
            "cache query unavailable"
        );
        match serde_json::from_value::<Query>(value.clone())? {
            Query::CacheMaintenanceResult {
                request_id,
                original_request_id,
            } => {
                ensure!(
                    zerus_desktop_migration::canonical_uuid(&request_id),
                    "invalid cache query"
                );
                let row = ledger::load(&store.db, &original_request_id)?;
                if let Some(row) = &row {
                    ensure!(
                        row.original.configuration == configuration,
                        "cache configuration changed"
                    );
                }
                Ok(
                    json!({"op":"cache_maintenance_result","request_id":request_id,"original_request_id":original_request_id,
                    "accepted":true,"durable":false,"found":row.is_some(),"result":row.as_ref().map(status).transpose()?}),
                )
            }
            Query::CacheMaintenancePolicy { request_id, root } => {
                ensure!(
                    zerus_desktop_migration::canonical_uuid(&request_id)
                        && root.len() <= 8192
                        && Path::new(&root).is_absolute()
                        && !root.chars().any(char::is_control),
                    "invalid cache query"
                );
                let (revision, limit) = store
                    .db
                    .query_row(
                        "SELECT revision,bytes_limit FROM desktop_cache_roots WHERE root=?1",
                        [&root],
                        |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)),
                    )
                    .optional()?
                    .unwrap_or((0, 0));
                Ok(
                    json!({"request_id":request_id,"abi":wire::ABI,"configuration":configuration,"root":root,"revision":revision,"limit":limit,"configured":revision>0}),
                )
            }
        }
    })();
    result.unwrap_or_else(|_| refused(id, "cache_result_unavailable"))
}
fn refused(id: &str, code: &str) -> Value {
    json!({"request_id":id,"op":"cache_maintenance","accepted":false,"durable":false,"pending":false,"code":code})
}
fn status(row: &ledger::Record) -> Result<Value> {
    let (phase, admitted, pending) = match row.phase {
        ledger::Phase::Preparing => (wire::Phase::Preparing, false, true),
        ledger::Phase::Prepared => (wire::Phase::Prepared, false, true),
        ledger::Phase::Reserved => (wire::Phase::Reserved, true, true),
        ledger::Phase::Settled => (wire::Phase::Settled, true, false),
        ledger::Phase::Refused => (wire::Phase::Refused, false, false),
        ledger::Phase::Unknown => (wire::Phase::Unknown, true, false),
    };
    let files = row
        .receipt
        .as_ref()
        .map(|r| r.files.as_slice())
        .unwrap_or(&[]);
    let view = wire::Status {
        abi: wire::ABI.into(),
        configuration: row.original.configuration.clone(),
        preparation_id: row.original.preparation_id.clone(),
        operation_id: row.original.operation_id.clone(),
        revision: row.original.revision + u64::from(row.original.policy_change),
        limit: row.original.limit,
        policy_saved: row.original.policy_change,
        phase,
        binding_sha256: row.binding_sha256.clone(),
        removed: files
            .iter()
            .filter(|f| **f == ledger::FileState::Removed)
            .count(),
        kept: files
            .iter()
            .filter(|f| **f == ledger::FileState::Kept)
            .count(),
        unknown: if row.phase == ledger::Phase::Unknown && row.receipt.is_none() {
            row.binding.as_ref().map_or(0, |b| b.candidates.len())
        } else {
            files
                .iter()
                .filter(|f| **f == ledger::FileState::Unknown)
                .count()
        },
        drained: !row.undrained,
    };
    let mut value = serde_json::to_value(view)?;
    value["request_id"] = json!(row.original.operation_id);
    value["op"] = json!("cache_maintenance");
    value["accepted"] = json!(admitted);
    value["durable"] = json!(true);
    value["pending"] = json!(pending || row.undrained);
    Ok(value)
}
fn binding(value: &cache::Binding) -> Result<ledger::Binding> {
    ensure!(value.valid(), "invalid actual cache freeze");
    let cache::Binding {
        root,
        directory,
        index_sha256,
        index_identity,
        limit,
        candidates,
    } = value;
    Ok(ledger::Binding {
        root: root.clone(),
        directory: directory.clone(),
        index_sha256: index_sha256.clone(),
        index_identity: index_identity.clone(),
        limit: *limit,
        candidates: candidates
            .iter()
            .map(|c| {
                let cache::Candidate {
                    name,
                    bytes,
                    sha256,
                    object,
                    identity,
                    content_id,
                } = c;
                ledger::Candidate {
                    name: name.clone(),
                    bytes: *bytes,
                    sha256: sha256.clone(),
                    object: object.clone(),
                    identity: identity.clone(),
                    content_id: content_id.clone(),
                }
            })
            .collect(),
    })
}
fn receipt(value: &cache::Receipt) -> ledger::Receipt {
    let cache::Receipt {
        index_sha256,
        files,
        bytes,
        count,
    } = value;
    ledger::Receipt {
        index_sha256: index_sha256.clone(),
        bytes: *bytes,
        count: *count,
        files: files
            .iter()
            .map(|f| match f {
                cache::State::Removed => ledger::FileState::Removed,
                cache::State::Kept => ledger::FileState::Kept,
                cache::State::Unknown => ledger::FileState::Unknown,
            })
            .collect(),
    }
}
enum Command {
    Cancel,
    Execute {
        binding_sha256: String,
        allowed: Vec<bool>,
    },
}
enum Event {
    Prepared(cache::Binding, cache::Receipt),
    StartEffects {
        binding_sha256: String,
        reply: mpsc::SyncSender<EffectPermit>,
    },
    Done(Option<cache::Receipt>),
}
/// Only the original actor Job can release this after its committed mark.
struct EffectPermit;
struct Job {
    original: ledger::Original,
    cancel: Arc<AtomicBool>,
    events: mpsc::Receiver<Event>,
    commands: mpsc::SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
    joined: bool,
    result: Option<cache::Receipt>,
    binding_sha256: Option<String>,
    until: Instant,
    timed_out: bool,
}
#[cfg(test)]
struct TestHold {
    entered: mpsc::SyncSender<()>,
    release: std::sync::Mutex<mpsc::Receiver<()>>,
    after_started: bool,
}
#[cfg(test)]
impl TestHold {
    fn wait(&self) {
        let _ = self.entered.send(());
        let _ = self
            .release
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv();
    }
}
pub(super) struct Runtime {
    configuration: String,
    epoch: String,
    writer: String,
    owner: Arc<platform::FileLock>,
    job: Option<Job>,
    stopping: bool,
    error: bool,
    #[cfg(test)]
    hold: Option<Arc<TestHold>>,
}
impl Runtime {
    pub(super) fn new(location: &Location, epoch: &str, owner: Arc<platform::FileLock>) -> Self {
        Self {
            configuration: location.configuration.clone(),
            epoch: epoch.into(),
            writer: id(),
            owner,
            job: None,
            stopping: false,
            error: false,
            #[cfg(test)]
            hold: None,
        }
    }
    pub(super) fn frontend_request(&mut self, store: &mut Store, permit: Validated) -> Value {
        let request_id = permit.id().to_owned();
        match self.request(store, permit) {
            Ok(mut value) => {
                value["request_id"] = json!(request_id);
                value
            }
            Err(_) => refused(&request_id, "cache_maintenance_refused"),
        }
    }
    fn request(&mut self, store: &mut Store, permit: Validated) -> Result<Value> {
        ensure!(
            !self.stopping && !self.error && available(store)?,
            "cache runtime unavailable"
        );
        let (request_id, command) = permit.cache_parts()?;
        match command {
            wire::Command::Prepare {
                operation_id,
                configuration,
                root,
                limit,
                revision,
                timing,
                policy_change,
            } => {
                ensure!(
                    configuration == self.configuration,
                    "cache configuration changed"
                );
                let original = ledger::Original {
                    preparation_id: request_id,
                    operation_id,
                    configuration,
                    root,
                    limit,
                    defer_ms: timing.delay_ms(),
                    policy_change,
                    revision,
                    service_epoch: self.epoch.clone(),
                    writer_epoch: self.writer.clone(),
                    lease: id(),
                };
                let fingerprint = hash(
                    &json!({"operation_id":original.operation_id,"configuration":original.configuration,
                    "root":original.root,"limit":limit,"revision":revision,"defer_ms":original.defer_ms,"policy_change":policy_change}),
                )?;
                let tx = store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                permit.check_cache()?; // original kernel/native freshness AFTER BEGIN
                let fresh = ledger::begin(&tx, &original, &fingerprint)?;
                tx.commit()?;
                if fresh {
                    ensure!(self.job.is_none(), "original cache worker still held");
                    if self.spawn(original.clone()).is_err() {
                        let tx = store.db.transaction()?;
                        // spawn failed before returning an original thread.
                        ledger::finish_joined(&tx, &original, None)?;
                        tx.commit()?;
                    }
                }
                status(
                    &ledger::load(&store.db, &original.preparation_id)?
                        .context("cache intake missing")?,
                )
            }
            wire::Command::Execute {
                configuration,
                preparation_id,
                binding_sha256,
            } => {
                ensure!(
                    configuration == self.configuration,
                    "cache configuration changed"
                );
                let row =
                    ledger::load(&store.db, &preparation_id)?.context("cache freeze missing")?;
                ensure!(
                    row.original.operation_id == request_id
                        && row.binding_sha256.as_ref() == Some(&binding_sha256),
                    "cache original changed"
                );
                if row.phase != ledger::Phase::Prepared {
                    // An existing receipt is never another dispatch permission.
                    return status(&row);
                }
                let job = self.job.as_mut().context("original cache worker missing")?;
                ensure!(
                    job.original == row.original
                        && !job.joined
                        && job.thread.as_ref().is_some_and(|t| !t.is_finished())
                        && job.binding_sha256.as_ref() == Some(&binding_sha256),
                    "cache freeze expired"
                );
                let tx = store
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                permit.check_cache()?; // a FRESH original child bound to this freeze
                ensure!(Instant::now() < job.until, "cache freeze expired");
                let allowed = ledger::reserve(&tx, &job.original, &binding_sha256)?;
                tx.commit()?; // every original content identity reserved BEFORE unlink
                job.until += Duration::from_millis(job.original.defer_ms);
                let _ = job.commands.try_send(Command::Execute {
                    binding_sha256,
                    allowed,
                });
                status(&ledger::load(&store.db, &request_id)?.context("cache reservation missing")?)
            }
        }
    }
    fn spawn(&mut self, original: ledger::Original) -> Result<()> {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let selected = original.clone();
        let owner = self.owner.clone();
        #[cfg(test)]
        let hold = self.hold.clone();
        let started = Instant::now();
        let until = started + cache::DEADLINE;
        let (send, events) = mpsc::sync_channel(2);
        let (commands, receive) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("desktop-cache".into())
            .spawn(move || {
                let result = (|| -> Option<cache::Receipt> {
                    let now: i64 = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()?
                        .as_millis()
                        .try_into()
                        .ok()?;
                    let eligibility = now.checked_add(selected.defer_ms as i64)?;
                    let prepared = cache::Prepared::open(
                        Path::new(&selected.root),
                        selected.limit,
                        eligibility,
                        &stop,
                    )
                    .ok()?;
                    let frozen = prepared.binding().clone();
                    let expected = hash(&binding(&frozen).ok()?).ok()?;
                    let kept = prepared.kept_receipt();
                    send.send(Event::Prepared(frozen.clone(), kept.clone()))
                        .ok()?;
                    let Command::Execute {
                        binding_sha256,
                        allowed,
                    } = receive
                        .recv_timeout(until.saturating_duration_since(Instant::now()))
                        .ok()?
                    else {
                        return None;
                    };
                    if binding_sha256 != expected {
                        return None;
                    }
                    #[cfg(test)]
                    if let Some(hold) = hold.as_ref().filter(|h| !h.after_started) {
                        hold.wait();
                    }
                    let prepared = if selected.defer_ms > 0 {
                        drop(prepared); // release the Qt lock during the opening grace
                        let due = started + Duration::from_millis(selected.defer_ms);
                        if stop.load(Ordering::Acquire) {
                            return Some(kept);
                        }
                        if !due.saturating_duration_since(Instant::now()).is_zero()
                            && !matches!(
                                receive.recv_timeout(due.saturating_duration_since(Instant::now())),
                                Err(mpsc::RecvTimeoutError::Timeout)
                            )
                        {
                            return Some(kept);
                        }
                        if stop.load(Ordering::Acquire) {
                            return Some(kept);
                        }
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .ok()?
                            .as_millis()
                            .try_into()
                            .ok()?;
                        let current = match cache::Prepared::open(
                            Path::new(&selected.root),
                            selected.limit,
                            now,
                            &stop,
                        ) {
                            Ok(current) => current,
                            Err(_) => return Some(kept),
                        };
                        // Delayed execution is only the ORIGINAL admitted fixed
                        // work. A changed index/file/root keeps the copies; it
                        // cannot turn the earlier consent into a new cache pass.
                        if current.binding() != &frozen {
                            return Some(kept);
                        }
                        current
                    } else {
                        prepared
                    };
                    if stop.load(Ordering::Acquire) || !allowed.iter().any(|allow| *allow) {
                        return Some(kept);
                    }
                    let (reply, permission) = mpsc::sync_channel(1);
                    send.send(Event::StartEffects {
                        binding_sha256: expected,
                        reply,
                    })
                    .ok()?;
                    let due = until + Duration::from_millis(selected.defer_ms);
                    let _permit = loop {
                        if stop.load(Ordering::Acquire) || Instant::now() >= due {
                            return Some(kept);
                        }
                        match permission.recv_timeout(
                            due.saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(50)),
                        ) {
                            Ok(permit) => break permit,
                            Err(mpsc::RecvTimeoutError::Timeout) => (),
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Some(kept),
                        }
                    };
                    if stop.load(Ordering::Acquire) || Instant::now() >= due {
                        return Some(kept);
                    }
                    #[cfg(test)]
                    if let Some(hold) = hold.as_ref().filter(|h| h.after_started) {
                        hold.wait();
                    }
                    Some(prepared.execute(&allowed, &stop))
                })();
                let _ = send.send(Event::Done(result));
                drop(owner); // original serve owner through physical I/O/return
            })?;
        self.job = Some(Job {
            original,
            cancel,
            events,
            commands,
            thread: Some(thread),
            joined: false,
            result: None,
            binding_sha256: None,
            until,
            timed_out: false,
        });
        Ok(())
    }
    pub(super) fn poll(&mut self, store: &mut Store) {
        if self.poll_inner(store).is_err() {
            self.error = true;
        }
    }
    fn poll_inner(&mut self, store: &mut Store) -> Result<()> {
        let Some(job) = &mut self.job else {
            return Ok(());
        };
        if !job.timed_out && Instant::now() >= job.until {
            job.cancel.store(true, Ordering::Release);
            let _ = job.commands.try_send(Command::Cancel);
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            ledger::expire(&tx, &job.original)?;
            tx.commit()?;
            job.timed_out = true;
        }
        for _ in 0..2 {
            match job.events.try_recv() {
                Ok(Event::Prepared(frozen, kept)) => {
                    if self.stopping || job.timed_out {
                        job.cancel.store(true, Ordering::Release);
                        continue;
                    }
                    let value = binding(&frozen)?;
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    match ledger::prepared(&tx, &job.original, &value, &receipt(&kept)) {
                        Ok(hash) => {
                            tx.commit()?;
                            job.binding_sha256 = Some(hash);
                        }
                        Err(_) => {
                            job.cancel.store(true, Ordering::Release);
                        }
                    }
                }
                Ok(Event::StartEffects {
                    binding_sha256,
                    reply,
                }) => {
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    // Recheck the CURRENT original after BEGIN, not a cached
                    // readiness Boolean or an expired frontend permit.
                    let current = !self.stopping
                        && !self.error
                        && !job.timed_out
                        && !job.cancel.load(Ordering::Acquire)
                        && Instant::now() < job.until
                        && !job.joined
                        && job.thread.as_ref().is_some_and(|t| !t.is_finished())
                        && job.original.configuration == self.configuration
                        && job.original.service_epoch == self.epoch
                        && job.original.writer_epoch == self.writer
                        && job.binding_sha256.as_deref() == Some(binding_sha256.as_str());
                    if current && ledger::start_effects(&tx, &job.original, &binding_sha256).is_ok()
                    {
                        tx.commit()?;
                        let _ = reply.try_send(EffectPermit);
                    } else {
                        job.cancel.store(true, Ordering::Release);
                    }
                }
                Ok(Event::Done(result)) => job.result = result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if !job.joined && job.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            let worker = job.thread.take().context("original cache join missing")?;
            if worker.join().is_err() {
                job.result = None;
            }
            // Completion may have arrived between try_recv(Empty) and the
            // physical join. The joined original can send no further frames.
            for _ in 0..2 {
                if let Ok(Event::Done(result)) = job.events.try_recv() {
                    job.result = result;
                }
            }
            job.joined = true;
        }
        if job.joined {
            let result = job.result.as_ref().map(receipt);
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            ledger::finish_joined(&tx, &job.original, result.as_ref())?;
            tx.commit()?;
            self.job = None;
            self.error = false;
        }
        Ok(())
    }
    pub(super) fn request_stop(&mut self, store: &mut Store) -> Result<()> {
        self.stopping = true;
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Release);
            let _ = job.commands.try_send(Command::Cancel);
        }
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ledger::revoke(&tx)?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn drained(&self) -> bool {
        self.job.is_none() && !self.error
    }
    pub(super) fn status(&self) -> Value {
        json!({"ready":!self.stopping&&!self.error,"drained":self.drained(),"stopping":self.stopping,"busy":self.job.is_some(),
            "code":if self.stopping {"draining"} else if self.error {"cache_unavailable"} else {"running"}})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Release);
            let _ = job.commands.try_send(Command::Cancel);
        }
        // Dropping a JoinHandle detaches; it never clears SQL undrained. The
        // original worker retains the actual owner lock through physical exit.
    }
}

#[cfg(test)]
mod tests;

/// Migration tests start from today's Store and explicitly reconstruct an
/// older schema. They must remove this leaf too, rather than make production
/// migration silently accept a partial/pre-existing table set.
#[cfg(test)]
pub(super) fn remove_from_downgraded_fixture(db: &rusqlite::Connection) {
    db.execute_batch("DROP TRIGGER desktop_cache_unknown_preserve; DROP TRIGGER desktop_cache_unknown_immutable;
        DROP TABLE desktop_cache_unknown; DROP TABLE desktop_cache_files; DROP TABLE desktop_cache_operations;
        DROP TABLE desktop_cache_roots;").unwrap();
}
