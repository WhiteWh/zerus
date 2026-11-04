//! One fixed local clone executor, independent of the UI lifetime. Native
//! admission, each new external step and original resource drain are separate.
use super::{
    lease::{Consent, Lease, Machine},
    ledger,
};
use crate::{
    config::Config,
    desktop::{
        custody, fleet,
        store::{HandoffLock, Location, Store},
        ui_authority,
    },
    platform,
    state::repositories::{self, Step},
};
use anyhow::{ensure, Context, Result};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, mpsc, Arc},
    thread::{self, JoinHandle},
    time::Duration,
};
use zerus_client_core::{
    desktop::creation_clone::{self as wire, Phase, Scope},
    run::custody::InheritedLock,
};
use zerus_desktop_ledger::operations::{
    custody::{Prepared, Record},
    Ticket,
};

struct StepRequest {
    ticket: String,
    step: Step,
    reply: mpsc::SyncSender<std::result::Result<(), String>>,
}
#[cfg(test)]
type Executor = Arc<
    dyn Fn(&wire::Plan, &repositories::Control<'_>) -> std::result::Result<Value, String>
        + Send
        + Sync,
>;
enum Work {
    Preparing {
        ticket: Ticket,
        consent: Arc<Consent>,
        worker: JoinHandle<()>,
        result: mpsc::Receiver<Result<(Prepared, Arc<InheritedLock>)>>,
    },
    Running {
        ticket: Ticket,
        consent: Arc<Consent>,
        worker: JoinHandle<()>,
        steps: mpsc::Receiver<StepRequest>,
        result: mpsc::Receiver<std::result::Result<Value, String>>,
        _custody: Arc<InheritedLock>,
    },
    Draining {
        id: String,
        consent: Arc<Consent>,
    },
}
pub(in crate::desktop) struct Runtime {
    #[cfg(test)]
    executor: Option<Executor>,
    scope: Scope,
    location: Location,
    owner: Arc<platform::FileLock>,
    exclusion: Option<Arc<HandoffLock>>,
    machine: Option<Machine>,
    lease: Option<Arc<Lease>>,
    // Only an original native admission inserts here. Startup/import never does.
    admitted: BTreeMap<String, Arc<Consent>>,
    work: Option<Work>,
    stopping: bool,
    failed: bool,
    charges: Option<u64>,
}
impl Runtime {
    pub fn new(config: &Config, epoch: &str, owner: Arc<platform::FileLock>) -> Result<Self> {
        let location = Location::new(config)?;
        Ok(Self {
            #[cfg(test)]
            executor: None,
            scope: Scope {
                configuration: location.configuration.clone(),
                service_epoch: epoch.into(),
            },
            location,
            owner,
            exclusion: None,
            machine: None,
            lease: None,
            admitted: BTreeMap::new(),
            work: None,
            stopping: false,
            failed: false,
            charges: None,
        })
    }
    pub fn observe(&mut self, event: &fleet::FleetEvent) {
        let old = self.machine.clone();
        Machine::observe(&mut self.machine, event);
        if self.machine != old {
            self.invalidate();
        }
    }
    pub fn reject(&mut self, event: &fleet::FleetEvent) {
        let old = self.machine.clone();
        Machine::reject(&mut self.machine, event);
        if self.machine != old {
            self.invalidate();
        }
    }
    fn invalidate(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.invalidate();
        }
        for consent in self.admitted.values() {
            consent.stop();
        }
    }
    fn refresh(&mut self, store: &Store, exclusion: Option<Arc<HandoffLock>>) {
        if self.stopping {
            self.exclusion = None;
            self.invalidate();
            return;
        }
        self.exclusion = exclusion;
        if self.stopping || self.failed || !store.creation_recovery_ok() {
            self.invalidate();
            return;
        }
        if self.lease.as_ref().is_some_and(|lease| {
            lease
                .current(
                    &store.db,
                    &self.location,
                    self.machine.as_ref(),
                    self.exclusion.as_ref(),
                )
                .is_ok()
        }) {
            return;
        }
        self.invalidate();
        if let (Some(machine), Some(exclusion)) = (self.machine.clone(), self.exclusion.clone()) {
            self.lease = Lease::new(
                &store.db,
                self.scope.clone(),
                &self.location,
                machine,
                self.owner.clone(),
                exclusion,
            )
            .ok();
        }
    }
    pub fn frontend(
        &mut self,
        store: &mut Store,
        authority: ui_authority::Validated,
        exclusion: Option<Arc<HandoffLock>>,
    ) -> Value {
        let id = authority.id().to_owned();
        self.refresh(store, exclusion);
        let result = (|| -> Result<wire::Admission> {
            ensure!(
                !self.stopping && !self.failed && store.creation_recovery_ok(),
                "creation owner unavailable"
            );
            ensure!(
                self.known_charges(&store.db)?,
                "creation original resources retained"
            );
            let lease = self
                .lease
                .as_ref()
                .context("creation lease unavailable")?
                .clone();
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Recheck the original opaque two-second proof AFTER writer lock.
            let intent = authority.creation_clone()?;
            ensure!(intent.scope == self.scope, "creation connection changed");
            let verified = intent.verify().map_err(anyhow::Error::msg)?;
            lease.current(
                &tx,
                &self.location,
                self.machine.as_ref(),
                self.exclusion.as_ref(),
            )?;
            let original = ledger::load(&tx, &id)?;
            let consent = Consent::admitted(verified, lease.clone())?;
            let row = ledger::admit(
                &tx,
                &consent.original,
                &lease.writer_epoch,
                chrono::Utc::now().timestamp_millis(),
            )?;
            tx.commit()?;
            if original.is_none() {
                self.admitted.insert(id.clone(), consent);
            }
            Ok(wire::Admission {
                abi: wire::ABI.into(),
                request_id: id.clone(),
                scope: self.scope.clone(),
                accepted: true,
                durable: true,
                code: String::new(),
                receipt: Some(row.receipt),
            })
        })();
        match result {
            Ok(value) => json!(value),
            Err(_) => json!(wire::Admission {
                abi: wire::ABI.into(),
                request_id: id,
                scope: self.scope.clone(),
                accepted: false,
                durable: false,
                code: "creation_clone_unavailable".into(),
                receipt: None
            }),
        }
    }
    fn check(
        &self,
        db: &rusqlite::Connection,
        consent: &Consent,
        row: &ledger::Row,
        ticket: Option<&Ticket>,
    ) -> Result<()> {
        ensure!(!self.stopping && !self.failed, "creation owner stopping");
        ensure!(
            self.lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &consent.lease)),
            "creation lease replaced"
        );
        consent.lease.current(
            db,
            &self.location,
            self.machine.as_ref(),
            self.exclusion.as_ref(),
        )?;
        consent.check(row, ticket)
    }
    fn step(&self, store: &mut Store, consent: &Consent, ticket: &Ticket, request: StepRequest) {
        let result = (|| -> Result<()> {
            ensure!(
                request.ticket == ticket.fingerprint()?,
                "creation step ticket changed"
            );
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = ledger::load(&tx, &ticket.admission_id)?.context("creation disappeared")?;
            self.check(&tx, consent, &row, Some(ticket))?;
            ledger::advance(&tx, &row, request.step.ordinal())?;
            tx.commit()?;
            Ok(())
        })()
        .map_err(|_| "creation_step_refused".to_owned());
        let _ = request.reply.send(result);
    }
    pub fn poll(&mut self, store: &mut Store, exclusion: Option<Arc<HandoffLock>>) {
        // Also advance exact original custody inside a bounded drain turn;
        // takeover must not depend on returning to the outer serve loop.
        store.recover_operation_custody(&self.location);
        self.refresh(store, exclusion);
        if self.poll_inner(store).is_err() {
            self.failed = true;
            self.invalidate();
        }
        self.charges = ledger::undrained(&store.db).ok();
    }
    fn poll_inner(&mut self, store: &mut Store) -> Result<()> {
        if let Some(work) = self.work.take() {
            match work {
                Work::Preparing {
                    ticket,
                    consent,
                    worker,
                    result,
                } => {
                    if !worker.is_finished() {
                        self.work = Some(Work::Preparing {
                            ticket,
                            consent,
                            worker,
                            result,
                        });
                        return Ok(());
                    }
                    let prepared = worker
                        .join()
                        .ok()
                        .and_then(|_| result.try_recv().ok())
                        .and_then(Result::ok);
                    let Some((prepared, held)) = prepared else {
                        self.retire(store, &ticket.admission_id)?;
                        return Ok(());
                    };
                    let record = Record {
                        prepared,
                        helper: None,
                    };
                    let reserved = (|| -> Result<()> {
                        let tx = store
                            .db
                            .transaction_with_behavior(TransactionBehavior::Immediate)?;
                        let row = ledger::load(&tx, &ticket.admission_id)?
                            .context("creation disappeared")?;
                        self.check(&tx, &consent, &row, None)?;
                        ensure!(ticket.lease == consent.lease.id, "creation lease changed");
                        ledger::reserve(&tx, &row, &ticket, &record)?;
                        let row = ledger::load(&tx, &ticket.admission_id)?
                            .context("creation disappeared")?;
                        ledger::running(&tx, &row)?;
                        tx.commit()?;
                        Ok(())
                    })();
                    if reserved.is_err() {
                        drop(held);
                        self.retire(store, &ticket.admission_id)?;
                        return Ok(());
                    }
                    let (step_send, steps) = mpsc::sync_channel(1);
                    let (done_send, result) = mpsc::sync_channel(1);
                    let selected = consent.clone();
                    let lock = held.clone();
                    let original = ticket.fingerprint()?;
                    #[cfg(test)]
                    let executor = self.executor.clone();
                    let state = self
                        .location
                        .root
                        .parent()
                        .context("missing state root")?
                        .to_path_buf();
                    let spawned =
                        thread::Builder::new()
                            .name("creation-clone".into())
                            .spawn(move || {
                                let before = |step| {
                                    if selected.cancel.load(Ordering::Acquire) {
                                        return Err("creation_step_refused".into());
                                    }
                                    let (reply, receive) = mpsc::sync_channel(1);
                                    step_send
                                        .send(StepRequest {
                                            ticket: original.clone(),
                                            step,
                                            reply,
                                        })
                                        .map_err(|_| "creation_step_unconfirmed".to_owned())?;
                                    receive
                                        .recv_timeout(Duration::from_secs(5))
                                        .map_err(|_| "creation_step_unconfirmed".to_owned())??;
                                    if selected.cancel.load(Ordering::Acquire) {
                                        return Err("creation_step_refused".into());
                                    }
                                    Ok(())
                                };
                                let plan = &selected.original.plan;
                                let control = repositories::Control {
                                    before: &before,
                                    cancelled: &selected.cancel,
                                    custody: &lock,
                                };
                                #[cfg(test)]
                                let value = match executor {
                                    Some(executor) => executor(plan, &control),
                                    None => repositories::clone_owned(
                                        &state,
                                        &plan.clone_request_id,
                                        &plan.url,
                                        &plan.parent,
                                        &plan.name,
                                        &control,
                                    ),
                                };
                                #[cfg(not(test))]
                                let value = repositories::clone_owned(
                                    &state,
                                    &plan.clone_request_id,
                                    &plan.url,
                                    &plan.parent,
                                    &plan.name,
                                    &control,
                                );
                                let _ = done_send.send(value);
                            });
                    match spawned {
                        Ok(worker) => {
                            self.work = Some(Work::Running {
                                ticket,
                                consent,
                                worker,
                                steps,
                                result,
                                _custody: held,
                            })
                        }
                        Err(_) => {
                            drop(held);
                            self.retire(store, &ticket.admission_id)?;
                        }
                    }
                }
                Work::Running {
                    ticket,
                    consent,
                    worker,
                    steps,
                    result,
                    _custody,
                } => {
                    // The worker requests one step at a time; do not drain an
                    // unbounded channel or accept an arbitrary executor string.
                    if let Ok(request) = steps.try_recv() {
                        self.step(store, &consent, &ticket, request);
                    }
                    if !worker.is_finished() {
                        self.work = Some(Work::Running {
                            ticket,
                            consent,
                            worker,
                            steps,
                            result,
                            _custody,
                        });
                        return Ok(());
                    }
                    let repository = worker
                        .join()
                        .ok()
                        .and_then(|_| result.try_recv().ok())
                        .and_then(std::result::Result::ok);
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row =
                        ledger::load(&tx, &ticket.admission_id)?.context("creation disappeared")?;
                    ensure!(
                        row.ticket.as_ref() == Some(&ticket),
                        "creation original ticket changed"
                    );
                    ledger::result(&tx, &row, repository, "awaiting_resource_drain")?;
                    tx.commit()?;
                    // Release only after the original worker joined (including
                    // its Git lifeline watcher). Exact kernel recovery still
                    // accounts for descendant-held copies before settlement.
                    drop(_custody);
                    self.work = Some(Work::Draining {
                        id: ticket.admission_id,
                        consent,
                    });
                }
                Work::Draining { id, consent } => {
                    let row = ledger::load(&store.db, &id)?.context("creation disappeared")?;
                    if row.receipt.drained {
                        self.admitted.remove(&id);
                    } else {
                        self.work = Some(Work::Draining { id, consent });
                    }
                }
            }
            return Ok(());
        }
        let cancelled = self
            .admitted
            .iter()
            .find_map(|(id, c)| c.cancel.load(Ordering::Acquire).then_some(id.clone()));
        if let Some(id) = cancelled {
            self.retire(store, &id)?;
            return Ok(());
        }
        if self.stopping || self.failed {
            self.lease = None;
            self.exclusion = None;
            return Ok(());
        }
        if !self.known_charges(&store.db)? {
            return Ok(());
        }
        let Some(row) = ledger::pending(&store.db)? else {
            return Ok(());
        };
        if row.receipt.phase != Phase::Queued {
            return Ok(());
        }
        let id = row.verified.plan.clone_request_id.clone();
        let Some(consent) = self.admitted.get(&id).cloned() else {
            self.retire(store, &id)?;
            return Ok(());
        };
        self.check(&store.db, &consent, &row, None)?;
        let ticket = Ticket {
            operation_id: row
                .verified
                .binding
                .operation_id()
                .context("invalid clone key")?,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: id,
            binding_sha256: row
                .verified
                .binding
                .fingerprint()
                .context("invalid clone binding")?,
            writer_epoch: consent.lease.writer_epoch.clone(),
            service_epoch: self.scope.service_epoch.clone(),
            lease: consent.lease.id.clone(),
        };
        let original = ticket.clone();
        let held = consent.clone();
        let location = Location {
            root: self.location.root.clone(),
            configuration: self.location.configuration.clone(),
        };
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("creation-prepare".into())
            .spawn(move || {
                let _original = held;
                let _ = send.send(custody::prepare(&location, &original));
            })?;
        self.work = Some(Work::Preparing {
            ticket,
            consent,
            worker,
            result,
        });
        Ok(())
    }
    fn retire(&mut self, store: &mut Store, id: &str) -> Result<()> {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = ledger::load(&tx, id)?.context("creation disappeared")?;
        if !row.receipt.drained {
            ledger::retire(&tx, &row)?;
        }
        tx.commit()?;
        self.admitted.remove(id);
        Ok(())
    }
    pub fn request_stop(&mut self) {
        self.stopping = true;
        self.invalidate();
        self.exclusion = None;
    }
    fn known_charges(&self, db: &rusqlite::Connection) -> Result<bool> {
        Ok(ledger::charged_ids(db)?
            .iter()
            .all(|id| self.admitted.contains_key(id)))
    }
    pub fn drained(&self) -> bool {
        self.stopping && self.work.is_none() && self.admitted.is_empty() && self.charges == Some(0)
    }
    pub fn status(&self) -> Value {
        json!({"profile":wire::PROFILE,"ready":!self.stopping&&!self.failed&&self.lease.is_some(),
            "code":if self.failed {"creation_owner_unavailable"}else if self.drained(){"drained"}else if self.stopping{"draining"}else if self.lease.is_some(){"running"}else{"awaiting_handoff"},
            "drained":self.drained(),"admitted":self.admitted.len(),"working":self.work.is_some(),"undrained":self.charges})
    }
    #[cfg(test)]
    pub(super) fn fixture_poll_steps_without_refresh(&mut self, store: &mut Store) -> Result<()> {
        // Isolate the actor's per-step check from refresh's worker cancellation.
        self.poll_inner(store)
    }
    #[cfg(test)]
    pub(super) fn fixture_executor(&mut self, machine: &str, executor: Executor) {
        self.machine = Some(Machine::fixture(machine));
        self.executor = Some(executor);
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
