//! One fixed local task message executor, independent of the UI lifetime.
//! Native admission, the message step and original resource drain are
//! separate.
use super::child::{self, Ended};
use super::{consent::Consent, ledger, staging};

mod reference;
mod review;
#[cfg(test)]
pub(super) use review::LISTING_SPAWN_FAILS;

/// The one removal slot: a custody-input removal or restart recovery, or one
/// explicit review's claim (zerus-pi2k9). Either excludes the other, the
/// sweep, admission of that input and a second review.
pub(super) enum Slot {
    Removal(reference::Removal),
    Review(Box<review::Review>),
}
use crate::desktop::creation_clone::lease::{Lease, Machine};
use crate::{
    config::Config,
    desktop::{
        custody, fleet,
        store::{HandoffLock, Location, Store},
        ui_authority,
    },
    platform,
};
use anyhow::{ensure, Context, Result};
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
#[cfg(target_os = "linux")]
use std::time::Instant;
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, mpsc, Arc},
    thread::{self, JoinHandle},
    time::Duration,
};
#[cfg(target_os = "linux")]
use zerus_client_core::image::linux::Pin;
use zerus_client_core::{
    desktop::task_message::{self as wire, Phase, Scope},
    run::custody::InheritedLock,
};
use zerus_desktop_ledger::operations::{
    custody::{Prepared, Record},
    Ticket,
};

/// The refusal of every macOS original until a qualified child seam exists.
pub(super) const MAC_UNAVAILABLE: &str = "task_message_native_image_unavailable";

/// The checked steps of a task message: the original child only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Message,
}
impl Step {
    fn ordinal(self) -> usize {
        match self {
            Self::Message => 0,
        }
    }
}
struct StepRequest {
    ticket: String,
    step: Step,
    reply: mpsc::SyncSender<std::result::Result<(), String>>,
}
/// Stands in for the one child in tests: it receives the plan, exactly the
/// arguments and stdin that `child::run` would have been given, the custody
/// lock and the original's resources guard.
#[cfg(test)]
type Executor = Arc<
    dyn Fn(&wire::Plan, &[String], &[u8], &Arc<InheritedLock>, &Arc<dyn Send + Sync>) -> Ended
        + Send
        + Sync,
>;
/// The original's physical resources: the SAME custody lock and its consent,
/// which holds the lease and with it the serve-owner loan. Every original pipe
/// keeps a copy until its real end, so neither the custody drain nor a
/// takeover can pass while a descendant still holds one.
struct Original {
    _custody: Arc<InheritedLock>,
    _consent: Arc<Consent>,
}
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
        result: mpsc::Receiver<Ended>,
        resources: Arc<Original>,
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
    /// macOS refuses every original (see [`MAC_UNAVAILABLE`]).
    mac_fence: bool,
    /// This service's own running image, pinned once when it started from the
    /// kernel's `/proc/self/exe`. Each child executes that pinned image, never
    /// a reopened pathname, and it is never refreshed from the installed path
    /// or same-version metadata; without it (a test image, or an image already
    /// gone) no child starts.
    #[cfg(target_os = "linux")]
    image: Option<Arc<Pin>>,
    /// The held staging and custody-input directories; without them the
    /// reference form is refused.
    roots: Option<Arc<staging::Roots>>,
    /// The one reference original being prepared off the actor.
    admitting: Option<Box<reference::Admitting>>,
    /// The one custody-input removal, restart recovery or review off the
    /// actor.
    removal: Option<Slot>,
    /// Published inputs whose admission was refused, for exact removal.
    refused: Vec<(String, staging::Expected, Option<std::fs::File>)>,
    /// The original custody inputs this process still holds for their exact
    /// removal: duplicates of their open file descriptions, so their inodes
    /// cannot be reused. Never a reader of any original pipe, so they never
    /// keep an original from draining. At most one per charged record.
    retained: BTreeMap<String, std::fs::File>,
}
impl Runtime {
    pub fn new(config: &Config, epoch: &str, owner: Arc<platform::FileLock>) -> Result<Self> {
        let location = Location::new(config)?;
        let location_root = location.root.clone();
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
            mac_fence: cfg!(target_os = "macos"),
            #[cfg(target_os = "linux")]
            image: crate::self_exec::ensure()
                .and_then(|()| Pin::open(std::path::Path::new("/proc/self/exe")))
                .ok()
                .map(Arc::new),
            roots: staging::Roots::open(&location_root).ok().map(Arc::new),
            admitting: None,
            removal: None,
            refused: Vec::new(),
            retained: BTreeMap::new(),
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
        if let Some(Slot::Review(review)) = &self.removal {
            review.stop();
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
        // No qualified seam yet attests the code a macOS child runs before it
        // receives private input: refuse before any preparation, row or child.
        if self.mac_fence {
            return json!(wire::Admission {
                abi: wire::ABI.into(),
                request_id: id,
                scope: self.scope.clone(),
                accepted: false,
                durable: false,
                code: MAC_UNAVAILABLE.into(),
                receipt: None
            });
        }
        self.refresh(store, exclusion);
        let result = (|| -> Result<wire::Admission> {
            ensure!(
                !self.stopping && !self.failed && store.creation_recovery_ok(),
                "task message owner unavailable"
            );
            ensure!(
                self.known_charges(&store.db)?,
                "task message original resources retained"
            );
            let lease = self
                .lease
                .as_ref()
                .context("task message lease unavailable")?
                .clone();
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Recheck the original opaque two-second proof AFTER writer lock.
            let intent = authority.task_message()?;
            ensure!(
                intent.scope == self.scope,
                "task message connection changed"
            );
            // The reference form needs its staged attachments, which this
            // service does not prepare yet: refused before any row.
            ensure!(
                intent.form.is_inline(),
                "task message reference form unavailable"
            );
            let verified = intent.verify().map_err(anyhow::Error::msg)?;
            lease.current(
                &tx,
                &self.location,
                self.machine.as_ref(),
                self.exclusion.as_ref(),
            )?;
            let original = ledger::load(&tx, &id)?;
            // A request whose custody input was ever recorded (reserved,
            // orphaned, failed, removed or released) never gets a new row,
            // reference or inline.
            ensure!(
                original.is_some() || super::inputs::load(&tx, &id)?.is_none(),
                "task message input exists"
            );
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
                code: "task_message_unavailable".into(),
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
        ensure!(
            !self.stopping && !self.failed,
            "task message owner stopping"
        );
        ensure!(
            self.lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &consent.lease)),
            "task message lease replaced"
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
                "task message step ticket changed"
            );
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row =
                ledger::load(&tx, &ticket.admission_id)?.context("task message disappeared")?;
            self.check(&tx, consent, &row, Some(ticket))?;
            ledger::advance(&tx, &row, request.step.ordinal())?;
            tx.commit()?;
            Ok(())
        })()
        .map_err(|_| "task_message_step_refused".to_owned());
        let _ = request.reply.send(result);
    }
    pub fn poll(&mut self, store: &mut Store, exclusion: Option<Arc<HandoffLock>>) {
        // Also advance exact original custody inside a bounded drain turn;
        // takeover must not depend on returning to the outer serve loop.
        store.recover_operation_custody(&self.location);
        self.refresh(store, exclusion);
        if self.poll_inner(store).is_err() || self.poll_reference(store).is_err() {
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
                            .context("task message disappeared")?;
                        self.check(&tx, &consent, &row, None)?;
                        ensure!(
                            ticket.lease == consent.lease.id,
                            "task message lease changed"
                        );
                        ledger::reserve(&tx, &row, &ticket, &record)?;
                        let row = ledger::load(&tx, &ticket.admission_id)?
                            .context("task message disappeared")?;
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
                    let resources = Arc::new(Original {
                        _custody: held,
                        _consent: consent.clone(),
                    });
                    let guard: Arc<dyn Send + Sync> = resources.clone();
                    let original = ticket.fingerprint()?;
                    #[cfg(test)]
                    let executor = self.executor.clone();
                    #[cfg(target_os = "linux")]
                    let image = self.image.clone();
                    let spawned =
                        thread::Builder::new()
                            .name("task-message".into())
                            .spawn(move || {
                                let before = |step: Step| -> std::result::Result<(), String> {
                                    if selected.cancel.load(Ordering::Acquire) {
                                        return Err("task_message_step_refused".into());
                                    }
                                    let (reply, receive) = mpsc::sync_channel(1);
                                    step_send
                                        .send(StepRequest {
                                            ticket: original.clone(),
                                            step,
                                            reply,
                                        })
                                        .map_err(|_| "task_message_step_unconfirmed".to_owned())?;
                                    receive.recv_timeout(Duration::from_secs(5)).map_err(
                                        |_| "task_message_step_unconfirmed".to_owned(),
                                    )??;
                                    if selected.cancel.load(Ordering::Acquire) {
                                        return Err("task_message_step_refused".into());
                                    }
                                    Ok(())
                                };
                                // One checked step, then the one original child.
                                let value = match before(Step::Message) {
                                    Err(_) => Ended::Unknown("task_message_step_refused"),
                                    Ok(()) => message(
                                        &selected,
                                        &lock,
                                        &guard,
                                        #[cfg(target_os = "linux")]
                                        image,
                                        #[cfg(test)]
                                        executor,
                                    ),
                                };
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
                                resources,
                            })
                        }
                        Err(_) => {
                            drop(resources);
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
                    resources,
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
                            resources,
                        });
                        return Ok(());
                    }
                    let ended = worker
                        .join()
                        .ok()
                        .and_then(|_| result.try_recv().ok())
                        .unwrap_or(Ended::Unknown("task_message_unobserved"));
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = ledger::load(&tx, &ticket.admission_id)?
                        .context("task message disappeared")?;
                    ensure!(
                        row.ticket.as_ref() == Some(&ticket),
                        "task message original ticket changed"
                    );
                    match &ended {
                        // Exit 0 counts only with the exact outcome its reply
                        // proves; a non-zero exit proves nothing either way.
                        Ended::Exited { code: 0, stdout } => {
                            match wire::Outcome::from_reply(&row.verified.plan, stdout) {
                                Some(outcome) => ledger::result(
                                    &tx,
                                    &row,
                                    Some(&ledger::Exit {
                                        code: 0,
                                        outcome: Some(outcome),
                                    }),
                                    "awaiting_resource_drain",
                                )?,
                                None => ledger::result(
                                    &tx,
                                    &row,
                                    None,
                                    "task_message_reply_unconfirmed",
                                )?,
                            }
                        }
                        Ended::Exited { code, .. } => ledger::result(
                            &tx,
                            &row,
                            Some(&ledger::Exit {
                                code: *code,
                                outcome: None,
                            }),
                            "awaiting_resource_drain",
                        )?,
                        // Durable before the drain, which alone settles it.
                        Ended::NotStarted => ledger::not_started(&tx, &row)?,
                        Ended::Unknown(code) => ledger::result(&tx, &row, None, code)?,
                    }
                    tx.commit()?;
                    // The logical result is durable. Release this copy only
                    // after the worker joined: an original pipe that is still
                    // open keeps its own copy until its real end. Every copy
                    // holds the custody lock, so the kernel recovery that
                    // drains the row also waits for all of them.
                    drop(resources);
                    self.work = Some(Work::Draining {
                        id: ticket.admission_id,
                        consent,
                    });
                }
                Work::Draining { id, consent } => {
                    let row = ledger::load(&store.db, &id)?.context("task message disappeared")?;
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
        let id = row.verified.plan.request_id.clone();
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
                .context("invalid message key")?,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: id,
            binding_sha256: row
                .verified
                .binding
                .fingerprint()
                .context("invalid message binding")?,
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
            .name("task-message-prepare".into())
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
        let row = ledger::load(&tx, id)?.context("task message disappeared")?;
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
        self.stopping
            && self.work.is_none()
            && self.admitting.is_none()
            && self.removal.is_none()
            && self.refused.is_empty()
            && self.admitted.is_empty()
            && self.charges == Some(0)
    }
    pub fn status(&self) -> Value {
        json!({"profile":wire::PROFILE,"ready":!self.stopping&&!self.failed&&self.lease.is_some(),
            "code":if self.failed {"task_message_owner_unavailable"}else if self.drained(){"drained"}else if self.stopping{"draining"}else if self.lease.is_some(){"running"}else{"awaiting_handoff"},
            "drained":self.drained(),"admitted":self.admitted.len(),"working":self.work.is_some(),"undrained":self.charges})
    }
    #[cfg(test)]
    pub(super) fn fixture_executor(&mut self, machine: &str, executor: Executor) {
        self.machine = Some(Machine::fixture(machine));
        self.executor = Some(executor);
    }
    /// Sets the macOS fence, so the actor's shared logic is tested everywhere
    /// and the fence itself on every platform.
    #[cfg(test)]
    pub(super) fn fixture_mac_fence(&mut self, on: bool) {
        self.mac_fence = on;
    }
    /// Pins `path` as this runtime's own image, so the original path runs a
    /// real child without an executor.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn fixture_image(&mut self, path: &std::path::Path) {
        self.image = Some(Arc::new(Pin::open(path).unwrap()));
    }
    /// The fixture source without an executor: the original path runs, and a
    /// test image's own-image refusal stops it before any child.
    #[cfg(test)]
    pub(super) fn fixture_machine(&mut self, machine: &str) {
        self.machine = Some(Machine::fixture(machine));
    }
}
/// The one original child: the exact call with its frozen stdin, the record's
/// own or, for the reference form, the exact custody input through its kept
/// handle. Nothing exists before `child::run`, so every refusal here never
/// started.
fn message(
    consent: &Consent,
    lock: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    #[cfg(target_os = "linux")] image: Option<Arc<Pin>>,
    #[cfg(test)] executor: Option<Executor>,
) -> Ended {
    let original = &consent.original;
    let call = match consent.input {
        Some(_) => None,
        None => match original.call(false) {
            Some(call) => Some(call),
            None => return Ended::NotStarted,
        },
    };
    let (args, input, timeout) = match (&consent.input, &call) {
        (Some(input), _) => (
            input.args.clone(),
            child::Stdin::File(&input.file, input.bytes),
            input.timeout,
        ),
        (None, Some(call)) => {
            let invocation = call.invocation();
            (
                invocation.args,
                child::Stdin::Bytes(call.input.as_bytes()),
                invocation.timeout,
            )
        }
        (None, None) => return Ended::NotStarted,
    };
    // The test executor receives exactly what the child would.
    #[cfg(test)]
    if let Some(executor) = executor {
        let Ok(bytes) = input.to_vec() else {
            return Ended::NotStarted;
        };
        return executor(&original.plan, &args, &bytes, lock, resources);
    }
    #[cfg(target_os = "linux")]
    if let Some(image) = image {
        let deadline = Instant::now() + timeout;
        return pinned_stdin(&image, &args, input, lock, resources, deadline);
    }
    // No pinned image (a unit-test image, or one already gone), and macOS,
    // whose admission refuses first: nothing starts.
    let _ = (args, input, lock, resources, timeout);
    Ended::NotStarted
}
/// One original child of the pinned image. The final check, last before the
/// spawn, requires the image still at its path; the child then executes the
/// retained descriptor, so a replacement after that check never runs and
/// never receives the input. A refused check means no child ever existed.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn pinned(
    image: &Pin,
    args: &[String],
    input: &[u8],
    lock: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    deadline: Instant,
) -> Ended {
    pinned_stdin(
        image,
        args,
        child::Stdin::Bytes(input),
        lock,
        resources,
        deadline,
    )
}
/// [`pinned`] with either kind of stdin.
#[cfg(target_os = "linux")]
fn pinned_stdin(
    image: &Pin,
    args: &[String],
    input: child::Stdin<'_>,
    lock: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    deadline: Instant,
) -> Ended {
    child::run_stdin(
        image.executable(),
        args,
        input,
        lock,
        resources,
        deadline,
        |command| {
            if !image.current() {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            crate::desktop::original_child::inherit_pinned(image, command)
        },
    )
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
