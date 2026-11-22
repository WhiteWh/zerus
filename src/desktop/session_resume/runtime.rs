//! One fixed local resume-then-send executor (zerus-5wmh.66.4.9, slice 3),
//! independent of the UI lifetime. Native admission, each original child and
//! its resource drain are separate; only the drain, through the CLI's own
//! receipt, settles a child. After a resume its own exact receipt recorded,
//! the same live operation waits, off the actor and within one never-renewed
//! budget, for the resumed run's record to show its agent takes a message,
//! and only then prepares, reserves and spawns the send bound to that run. A
//! resumed Codex shows that only on its terminal: the send's own
//! effect-free observation child decides it, in each read of the wait and
//! once more right before the send's spawn turn (zerus-5wmh.66.4.10). A
//! restart never resumes a wait.
// Only the Linux runner starts an original child; macOS refuses every
// original at admission.
#[cfg(target_os = "linux")]
use super::child;
use super::child::Ended;
use super::ledger::{self, Child};
use super::ready::{self, Readiness};
use super::receipt::{self, Evidence, Observed, Roots};
use super::{consent::Consent, target};
use crate::desktop::creation_clone::lease::{Lease, Machine};
use crate::desktop::custody::Observation;
use crate::desktop::not_admitted;
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
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, mpsc, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use zerus_client_core::image::linux::Pin;
use zerus_client_core::{
    desktop::session_resume::{self as wire, Scope, Step},
    run::custody::InheritedLock,
};
use zerus_desktop_ledger::operations::{
    custody::{Prepared, Record},
    Ticket,
};

/// The refusal of every macOS original until a qualified child seam exists.
pub(super) const MAC_UNAVAILABLE: &str = "session_resume_native_image_unavailable";
/// The conservative refusal: the request may have been admitted.
const UNAVAILABLE: &str = "session_resume_unavailable";
/// How far one admission got before it ended (zerus-tlb9q).
#[derive(Clone, Copy)]
enum Proof {
    /// Before the writer transaction: nothing read yet.
    Before,
    /// In the writer transaction, before the ledger's read succeeded.
    Unprovable,
    /// The ledger has no row for this request, read in this transaction.
    NoRow,
    /// The ledger has a row for this request.
    Existing,
    /// The admission itself was entered.
    Admitting,
}
/// Each whole child, the CLI's own lock wait included.
#[cfg(target_os = "linux")]
pub(super) const DEADLINE: Duration = Duration::from_secs(30);
/// The one bounded deadline of the receipt read after the physical drain,
/// never renewed: a later result is Unknown.
pub(super) const READ_WAIT: Duration = Duration::from_secs(5);
/// How often a draining original's custody is observed again.
const PROBE_EVERY: Duration = Duration::from_millis(250);
/// The wait for the resumed agent to take a message: one budget from the
/// resume's drain, never renewed, as long as the direct path waits.
pub(super) const WAIT: Duration = Duration::from_secs(180);
/// How often the waiting operation reads the resumed session again.
const POLL_EVERY: Duration = Duration::from_millis(500);
/// How long a read gap lasts, from its first unreadable sample, before the
/// wait asks whether the run's last proven supervisor exited (zerus-5sl7r).
/// An atomic record update can fail one held read; only that supervisor's
/// exact exit, never the gap alone, ends the wait.
pub(super) const RECORD_GRACE: Duration = Duration::from_secs(10);
/// The deadline of one observation of a resumed Codex, never past the
/// wait's own (zerus-5wmh.66.4.10).
pub(super) const OBSERVATION_WAIT: Duration = Duration::from_secs(5);
/// The whole operation from its own first turn, when it gets the work slot:
/// both children, both receipt reads, the wait and a margin. Nothing starts
/// after it. Time queued behind another operation does not count
/// (zerus-qw7wp).
pub(super) const CHAIN: Duration = Duration::from_secs(270);
#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;
/// Stands in for the observation child in tests: it receives exactly the
/// arguments and stdin the child would be given, and returns its reply.
#[cfg(test)]
type Observer = Arc<dyn Fn(&[String], &[u8]) -> Option<Vec<u8>> + Send + Sync>;

/// Stands in for the one child in tests: it receives the plan, exactly the
/// arguments and stdin the child would be given, the custody lock and the
/// original's resources guard.
#[cfg(test)]
type Executor = Arc<
    dyn Fn(&wire::Plan, &[String], &[u8], &Arc<InheritedLock>, &Arc<dyn Send + Sync>) -> Ended
        + Send
        + Sync,
>;
/// The original's physical resources: the SAME custody lock and its consent,
/// which holds the lease and with it the serve-owner loan. Every original
/// pipe keeps a copy until its real end, so neither the custody drain nor a
/// takeover can pass while a descendant still holds one.
struct Original {
    _custody: Arc<InheritedLock>,
    _consent: Arc<Consent>,
}
/// What preparation opens off the actor before a child: the exact custody
/// and that child's receipt store under the original root.
type Prepared2 = (Prepared, Arc<InheritedLock>, Arc<Roots>);
/// One drain observation: the custody, and after its release the receipt
/// read from the original store, with the objects that read used.
enum Drain {
    Held,
    Unavailable,
    Released {
        observation: Observation,
        cli: Observed,
        evidence: Evidence,
    },
}
struct Probe {
    worker: JoinHandle<()>,
    result: mpsc::Receiver<(Record, Drain)>,
}
/// One read of the waiting operation's resumed session, off the actor.
struct ReadyProbe {
    worker: JoinHandle<()>,
    result: mpsc::Receiver<(Readiness, Option<ready::Supervisor>)>,
}
/// What one original child runs: its fixed arguments, its exact stdin and
/// the run its spawn turn checks the session against (the original run for
/// the resume, the resumed one for the send).
struct Launch {
    args: Vec<String>,
    input: Vec<u8>,
    run: String,
}
enum Work {
    Preparing {
        child: Child,
        ticket: Ticket,
        consent: Arc<Consent>,
        worker: JoinHandle<()>,
        result: mpsc::Receiver<Result<Prepared2>>,
        /// The operation's ownership token, once its resume reserved.
        token: Option<Arc<()>>,
    },
    Running {
        child: Child,
        ticket: Ticket,
        consent: Arc<Consent>,
        worker: JoinHandle<()>,
        result: mpsc::Receiver<Ended>,
        resources: Arc<Original>,
        roots: Arc<Roots>,
        token: Arc<()>,
    },
    Draining {
        child: Child,
        id: String,
        consent: Arc<Consent>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<Probe>,
        next: Instant,
    },
    Waiting {
        id: String,
        consent: Arc<Consent>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<ReadyProbe>,
        until: Instant,
        /// The reads of the resumed run's record so far.
        gap: Gap,
        next: Instant,
    },
}
/// What the wait knows of the resumed run's record between its reads
/// (zerus-5sl7r).
#[derive(Debug, Default)]
struct Gap {
    /// The supervisor the last readable record of the run named.
    supervisor: Option<ready::Supervisor>,
    /// The first unreadable read since the last readable one.
    since: Option<Instant>,
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
    /// This epoch's sealed negatives: they outlive this runtime
    /// ([`crate::desktop::not_admitted`], zerus-tlb9q).
    fences: Arc<crate::desktop::not_admitted::Fences>,
    /// This service's own running image, pinned once from the kernel's
    /// `/proc/self/exe`. Each child executes that pinned image; without it (a
    /// test image, or an image already gone) no child starts.
    #[cfg(target_os = "linux")]
    image: Option<Arc<Pin>>,
    /// The receipt read's deadline after the drain ([`READ_WAIT`]).
    read_wait: Duration,
    /// The wait's budget ([`WAIT`]), its read interval ([`POLL_EVERY`]) and
    /// the whole operation's bound from its first turn ([`CHAIN`]).
    wait: Duration,
    poll_every: Duration,
    chain: Duration,
    /// The grace for a missing record of the resumed run ([`RECORD_GRACE`]).
    record_grace: Duration,
    /// One observation's deadline ([`OBSERVATION_WAIT`]).
    observation_wait: Duration,
    /// Stands in for the observation child.
    #[cfg(test)]
    observer: Option<Observer>,
    /// Runs in the final spawn turn, before the consent veto.
    #[cfg(test)]
    spawn_hook: Option<Hook>,
    /// Runs in the drain worker after the receipt bytes are read.
    #[cfg(test)]
    read_hook: Option<Hook>,
    /// Runs first in each preparation worker.
    #[cfg(test)]
    prepare_hook: Option<Hook>,
    /// Runs first in each read of the waiting operation's resumed session.
    #[cfg(test)]
    ready_hook: Option<Hook>,
}
impl Runtime {
    pub fn new(
        config: &Config,
        epoch: &str,
        owner: Arc<platform::FileLock>,
        fences: Arc<crate::desktop::not_admitted::Fences>,
    ) -> Result<Self> {
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
            mac_fence: cfg!(target_os = "macos"),
            fences,
            #[cfg(target_os = "linux")]
            image: crate::self_exec::ensure()
                .and_then(|()| Pin::open(std::path::Path::new("/proc/self/exe")))
                .ok()
                .map(Arc::new),
            read_wait: READ_WAIT,
            wait: WAIT,
            poll_every: POLL_EVERY,
            record_grace: RECORD_GRACE,
            chain: CHAIN,
            observation_wait: OBSERVATION_WAIT,
            #[cfg(test)]
            observer: None,
            #[cfg(test)]
            spawn_hook: None,
            #[cfg(test)]
            read_hook: None,
            #[cfg(test)]
            prepare_hook: None,
            #[cfg(test)]
            ready_hook: None,
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
        if self.failed || !store.creation_recovery_ok() {
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
    fn refusal(&self, id: String, code: &str) -> Value {
        json!(wire::Admission {
            abi: wire::ABI.into(),
            request_id: id,
            scope: self.scope.clone(),
            accepted: false,
            durable: false,
            code: code.into(),
            receipt: None
        })
    }
    /// The one admission of an original from the native frontend lane. The
    /// permit is consumed after BEGIN IMMEDIATE, with the same lease.
    pub fn frontend(
        &mut self,
        store: &mut Store,
        authority: ui_authority::Validated,
        exclusion: Option<Arc<HandoffLock>>,
    ) -> Value {
        let id = authority.id().to_owned();
        // Only this epoch's own request is ever answered not admitted or
        // sealed; another connection's stays unavailable (zerus-tlb9q).
        let ours = authority.deferred_input_scope() == Some(&self.scope);
        // A request this epoch answered not admitted never admits: a copy of
        // it queued behind that refusal gets the same answer.
        if ours && self.fences.session_resume.sealed(&id) {
            return self.refusal(id, wire::NOT_ADMITTED);
        }
        // No qualified seam yet attests the code a macOS child runs before it
        // receives private input: refuse before any row or child.
        if self.mac_fence {
            return self.refusal(id, MAC_UNAVAILABLE);
        }
        self.refresh(store, exclusion);
        let mut proof = Proof::Before;
        let result = (|| -> Result<wire::Admission> {
            ensure!(
                !self.stopping && !self.failed && store.creation_recovery_ok(),
                "session resume owner unavailable"
            );
            ensure!(
                self.known_charges(&store.db)?,
                "session resume original resources retained"
            );
            let lease = self
                .lease
                .as_ref()
                .context("session resume lease unavailable")?
                .clone();
            proof = Proof::Unprovable;
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // The ledger's word on this request, read first in this writer
            // transaction: only a successful empty read lets a refusal
            // before the admission settle as never started.
            let original = ledger::load(&tx, &id)?;
            proof = if original.is_none() {
                Proof::NoRow
            } else {
                Proof::Existing
            };
            // The original opaque permit, consumed AFTER the writer lock.
            let intent = authority.session_resume()?;
            ensure!(
                intent.scope == self.scope,
                "session resume connection changed"
            );
            let verified = intent.verify().map_err(anyhow::Error::msg)?;
            lease.current(
                &tx,
                &self.location,
                self.machine.as_ref(),
                self.exclusion.as_ref(),
            )?;
            let consent = Consent::admitted(verified, lease.clone(), self.chain)?;
            if original.is_none() {
                // Invalid/incomplete accounting is not a capacity proof.
                proof = Proof::Unprovable;
                let capacity = ledger::capacity(&tx, &consent.original)?;
                proof = Proof::NoRow;
                ensure!(capacity, "session resume capacity unavailable");
            }
            // From here on nothing proves the request was not admitted.
            proof = Proof::Admitting;
            let row = ledger::admit(
                &tx,
                &consent.original,
                &lease.writer_epoch,
                chrono::Utc::now().timestamp_millis(),
            )?;
            tx.commit()?;
            // The same original again gets only its own receipt; only the
            // first admission holds live consent.
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
        match (result, proof) {
            (Ok(value), _) => json!(value),
            // Refused before the admission, with no row in this transaction.
            (Err(_), Proof::NoRow) if ours => self.not_admitted(id),
            // Refused before the transaction: a fresh writer transaction on
            // the same held store proves the absence first.
            (Err(_), Proof::Before)
                if ours && not_admitted::no_row(&mut store.db, |tx| ledger::load(tx, &id)) =>
            {
                self.not_admitted(id)
            }
            (Err(_), _) => self.refusal(id, UNAVAILABLE),
        }
    }
    /// The closed refusal of a request proven never admitted, sealed for the
    /// epoch before the reply; a full fence mints none (zerus-tlb9q).
    fn not_admitted(&self, id: String) -> Value {
        if self.fences.session_resume.seal(&id) {
            self.refusal(id, wire::NOT_ADMITTED)
        } else {
            self.refusal(id, UNAVAILABLE)
        }
    }
    fn check(
        &self,
        db: &rusqlite::Connection,
        consent: &Consent,
        row: &ledger::Row,
        ticket: Option<(Child, &Ticket)>,
    ) -> Result<()> {
        ensure!(
            !self.stopping && !self.failed,
            "session resume owner stopping"
        );
        ensure!(
            self.lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &consent.lease)),
            "session resume lease replaced"
        );
        consent.lease.current(
            db,
            &self.location,
            self.machine.as_ref(),
            self.exclusion.as_ref(),
        )?;
        consent.check(row, ticket)
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
                    child,
                    ticket,
                    consent,
                    worker,
                    result,
                    token,
                } => {
                    if !worker.is_finished() {
                        self.work = Some(Work::Preparing {
                            child,
                            ticket,
                            consent,
                            worker,
                            result,
                            token,
                        });
                        return Ok(());
                    }
                    let prepared = worker
                        .join()
                        .ok()
                        .and_then(|_| result.try_recv().ok())
                        .and_then(Result::ok);
                    let Some((prepared, held, roots)) = prepared else {
                        self.retire(store, &ticket.admission_id)?;
                        return Ok(());
                    };
                    let record = Record {
                        prepared,
                        helper: None,
                    };
                    let id = ticket.admission_id.clone();
                    // The reservation commits before the worker exists. The
                    // child's exact stdin comes from the row it reserves: the
                    // send's is the template bound to the recorded resume.
                    let reserved = (|| -> Result<Launch> {
                        let tx = store
                            .db
                            .transaction_with_behavior(TransactionBehavior::Immediate)?;
                        let row = ledger::load(&tx, &id)?.context("session resume disappeared")?;
                        self.check(&tx, &consent, &row, None)?;
                        ensure!(
                            ticket.lease == consent.lease.id,
                            "session resume lease changed"
                        );
                        let plan = &row.verified.plan;
                        let launch = match child {
                            Child::Resume => Launch {
                                args: plan.resume_args().to_vec(),
                                input: row.verified.intent.resume.as_bytes().to_vec(),
                                run: plan.run_id.clone(),
                            },
                            Child::Send => {
                                let (resumed, stdin, _) =
                                    row.bound().context("session resume send is not bound")?;
                                Launch {
                                    args: plan.send_args().to_vec(),
                                    input: stdin.into_bytes(),
                                    run: resumed.result_target.run_id.clone(),
                                }
                            }
                        };
                        ledger::reserve(&tx, &row, child, &ticket, &record)?;
                        tx.commit()?;
                        Ok(launch)
                    })();
                    let Ok(launch) = reserved else {
                        drop(held);
                        self.retire(store, &id)?;
                        return Ok(());
                    };
                    // This runtime drains its own original from the store it
                    // opened; Store recovery skips it while the token lives,
                    // through the wait and both children.
                    let token = token.unwrap_or_else(|| Arc::new(()));
                    store
                        .resume_owned()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(id.clone(), Arc::downgrade(&token));
                    let (done_send, result) = mpsc::sync_channel(1);
                    let selected = consent.clone();
                    let lock = held.clone();
                    let resources = Arc::new(Original {
                        _custody: held,
                        _consent: consent.clone(),
                    });
                    let guard: Arc<dyn Send + Sync> = resources.clone();
                    let store_roots = roots.clone();
                    #[cfg(test)]
                    let executor = self.executor.clone();
                    #[cfg(test)]
                    let hook = self.spawn_hook.clone();
                    #[cfg(test)]
                    let observer = self.observer.clone();
                    let observation_wait = self.observation_wait;
                    #[cfg(target_os = "linux")]
                    let image = self.image.clone();
                    let name = match child {
                        Child::Resume => "session-resume-resume",
                        Child::Send => "session-resume-send",
                    };
                    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
                        // The target, the send's readiness and the consent
                        // veto run in the final spawn turn, from these
                        // retained roots.
                        let value = original(
                            child,
                            &selected,
                            &store_roots,
                            &lock,
                            &guard,
                            &launch,
                            observation_wait,
                            #[cfg(target_os = "linux")]
                            image,
                            #[cfg(test)]
                            executor,
                            #[cfg(test)]
                            observer,
                            #[cfg(test)]
                            hook,
                        );
                        let _ = done_send.send(value);
                    });
                    match spawned {
                        Ok(worker) => {
                            self.work = Some(Work::Running {
                                child,
                                ticket,
                                consent,
                                worker,
                                result,
                                resources,
                                roots,
                                token,
                            })
                        }
                        Err(_) => {
                            drop(resources);
                            drop(token);
                            self.retire(store, &id)?;
                        }
                    }
                }
                Work::Running {
                    child,
                    ticket,
                    consent,
                    worker,
                    result,
                    resources,
                    roots,
                    token,
                } => {
                    if !worker.is_finished() {
                        self.work = Some(Work::Running {
                            child,
                            ticket,
                            consent,
                            worker,
                            result,
                            resources,
                            roots,
                            token,
                        });
                        return Ok(());
                    }
                    let ended = worker
                        .join()
                        .ok()
                        .and_then(|_| result.try_recv().ok())
                        .unwrap_or(Ended::Unknown("session_resume_unobserved"));
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = ledger::load(&tx, &ticket.admission_id)?
                        .context("session resume disappeared")?;
                    ensure!(
                        row.reserved(child).map(|r| &r.ticket) == Some(&ticket),
                        "session resume original ticket changed"
                    );
                    match ended {
                        // Durable before the drain, which alone settles it.
                        Ended::NotStarted => ledger::not_started(&tx, &row)?,
                        // The exit proves nothing; the CLI's receipt, read
                        // at the drain, decides.
                        Ended::Exited { .. } => ledger::ended(&tx, &row, ledger::AWAITING_DRAIN)?,
                        Ended::Unknown(code) => ledger::ended(&tx, &row, code)?,
                    }
                    tx.commit()?;
                    // Release this copy only after the worker joined: an
                    // original pipe that is still open keeps its own copy
                    // until its real end, and every copy holds the custody
                    // lock, so the drain waits for all of them.
                    drop(resources);
                    self.work = Some(Work::Draining {
                        child,
                        id: ticket.admission_id,
                        consent,
                        roots,
                        token,
                        probe: None,
                        next: Instant::now(),
                    });
                }
                Work::Draining {
                    child,
                    id,
                    consent,
                    roots,
                    token,
                    probe,
                    next,
                } => return self.drain(store, child, id, consent, roots, token, probe, next),
                Work::Waiting {
                    id,
                    consent,
                    roots,
                    token,
                    probe,
                    until,
                    gap,
                    next,
                } => return self.wait(store, id, consent, roots, token, probe, until, gap, next),
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
        let id = row.id().to_owned();
        if row.receipt.resume.step != Step::Queued || !row.receipt.code.is_empty() {
            // An operation past its resume with no live work lost its wait:
            // it never resumes one.
            if row.receipt.resume.step.terminal() && row.receipt.send.step == Step::Queued {
                self.retire(store, &id)?;
            }
            return Ok(());
        }
        let Some(consent) = self.admitted.get(&id).cloned() else {
            self.retire(store, &id)?;
            return Ok(());
        };
        self.check(&store.db, &consent, &row, None)?;
        // The operation's first turn: its whole bound starts now, not at its
        // admission, so a wait ahead of it in the one work slot cannot use
        // it up (zerus-qw7wp).
        consent.chain();
        self.prepare(&row, Child::Resume, consent, None)
    }
    /// Starts `child`'s preparation off the actor: its exact custody and its
    /// receipt store. The resume's store is the CLI state root opened now;
    /// the send's is the same held root (`carry`), never resolved again.
    fn prepare(
        &mut self,
        row: &ledger::Row,
        child: Child,
        consent: Arc<Consent>,
        carry: Option<(Arc<Roots>, Arc<()>)>,
    ) -> Result<()> {
        let id = row.id().to_owned();
        let ticket = Ticket {
            operation_id: row
                .verified
                .binding
                .operation_id()
                .context("invalid session resume key")?,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: id,
            binding_sha256: row
                .verified
                .binding
                .fingerprint()
                .context("invalid session resume binding")?,
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
        let (roots, token) = match carry {
            Some((roots, token)) => (Some(roots), Some(token)),
            None => (None, None),
        };
        #[cfg(test)]
        let hook = self.prepare_hook.clone();
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("session resume-prepare".into())
            .spawn(move || {
                #[cfg(test)]
                if let Some(hook) = &hook {
                    hook();
                }
                let _original = held;
                let prepared = match roots {
                    // The send's folder under the held original root.
                    Some(roots) => roots.sibling(receipt::INPUT_FOLDER).map_err(Into::into),
                    // The original receipt store, opened before any child:
                    // the CLI's state root, of which this service's root is a
                    // folder.
                    None => location
                        .root
                        .parent()
                        .context("session resume state unavailable")
                        .and_then(|state| Ok(Roots::open(state, receipt::RESUME_FOLDER)?)),
                }
                .and_then(|roots| {
                    let (prepared, held) = custody::prepare(&location, &original)?;
                    Ok((prepared, held, Arc::new(roots)))
                });
                let _ = send.send(prepared);
            })?;
        self.work = Some(Work::Preparing {
            child,
            ticket,
            consent,
            worker,
            result,
            token,
        });
        Ok(())
    }
    /// One drain turn of this runtime's own child: the custody probe off the
    /// actor and, after its release, the one bounded read of the CLI's
    /// receipt from the original store. The worker holds the serve-owner and
    /// the original's consent loans and the ownership token until it
    /// physically ends, so neither a stop nor a teardown passes it. After a
    /// recorded resume, the same operation waits.
    #[allow(clippy::too_many_arguments)]
    fn drain(
        &mut self,
        store: &mut Store,
        child: Child,
        id: String,
        consent: Arc<Consent>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<Probe>,
        next: Instant,
    ) -> Result<()> {
        let keep = |me: &mut Self, probe: Option<Probe>, next: Instant| {
            me.work = Some(Work::Draining {
                child,
                id: id.clone(),
                consent: consent.clone(),
                roots: roots.clone(),
                token: token.clone(),
                probe,
                next,
            });
        };
        if let Some(probe) = probe {
            if !probe.worker.is_finished() {
                keep(self, Some(probe), next);
                return Ok(());
            }
            let drained = probe
                .worker
                .join()
                .ok()
                .and_then(|_| probe.result.try_recv().ok());
            return match drained {
                Some((
                    record,
                    Drain::Released {
                        observation,
                        cli,
                        evidence,
                    },
                )) => {
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = ledger::load(&tx, &id)?.context("session resume disappeared")?;
                    ensure!(
                        row.reserved(child).map(|r| &r.custody) == Some(&record),
                        "session resume custody scope changed"
                    );
                    let settled = if row.receipt.drained {
                        tx.commit()?;
                        row.receipt
                    } else {
                        ledger::drained(tx, row, observation, &cli, evidence)?
                    };
                    if child == Child::Resume && !settled.drained {
                        // One budget from here, never renewed, within the
                        // operation's own deadline.
                        let until = (Instant::now() + self.wait).min(consent.chain());
                        self.work = Some(Work::Waiting {
                            id: id.clone(),
                            consent: consent.clone(),
                            roots: roots.clone(),
                            token: token.clone(),
                            probe: None,
                            until,
                            gap: Gap::default(),
                            next: Instant::now(),
                        });
                    } else {
                        self.admitted.remove(&id);
                    }
                    Ok(())
                }
                Some((_, Drain::Held)) => {
                    keep(self, None, Instant::now() + PROBE_EVERY);
                    Ok(())
                }
                Some((_, Drain::Unavailable)) | None => {
                    keep(self, None, Instant::now() + PROBE_EVERY);
                    anyhow::bail!("session resume original custody unavailable")
                }
            };
        }
        if Instant::now() < next {
            keep(self, None, next);
            return Ok(());
        }
        let row = ledger::load(&store.db, &id)?.context("session resume disappeared")?;
        if row.receipt.drained {
            self.admitted.remove(&id);
            return Ok(());
        }
        let record = row
            .reserved(child)
            .map(|r| r.custody.clone())
            .context("session resume has no original custody")?;
        let plan = row.verified.plan.clone();
        let resume = row.verified.intent.resume.clone();
        let bound = row
            .bound()
            .map(|(resumed, _, digest)| (resumed.clone(), digest));
        let location = Location {
            root: self.location.root.clone(),
            configuration: self.location.configuration.clone(),
        };
        let loans = (self.owner.clone(), consent.clone(), token.clone());
        let store_roots = roots.clone();
        let wait = self.read_wait;
        #[cfg(test)]
        let hook = self.read_hook.clone();
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("session resume-drain".into())
            .spawn(move || {
                let _loans = loans;
                let observation = crate::desktop::custody::probe(&location, &record);
                let drain = match observation {
                    observation @ Observation::Released { .. } => {
                        // Read while the released lock is held: no child of
                        // the original can still write its receipt. One
                        // deadline, never renewed: a late read is Unknown.
                        let deadline = Instant::now() + wait;
                        let between = || {
                            #[cfg(test)]
                            if let Some(hook) = &hook {
                                hook();
                            }
                        };
                        let (cli, evidence) = match (child, &bound) {
                            (Child::Resume, _) => {
                                receipt::resume(&store_roots, &plan, resume.as_bytes(), &between)
                            }
                            (Child::Send, Some((resumed, digest))) => {
                                receipt::send(&store_roots, &plan, resumed, digest, &between)
                            }
                            // A send is reserved only after a recorded resume.
                            (Child::Send, None) => {
                                (Observed::Unknown(receipt::UNAVAILABLE), Evidence::default())
                            }
                        };
                        let cli = if Instant::now() > deadline {
                            Observed::Unknown(receipt::TIMED_OUT)
                        } else {
                            cli
                        };
                        Drain::Released {
                            observation,
                            cli,
                            evidence,
                        }
                    }
                    Observation::Held => Drain::Held,
                    Observation::Unavailable => Drain::Unavailable,
                };
                let _ = send.send((record, drain));
            });
        match worker {
            Ok(worker) => keep(self, Some(Probe { worker, result }), next),
            Err(error) => {
                keep(self, None, Instant::now() + PROBE_EVERY);
                return Err(error.into());
            }
        }
        Ok(())
    }
    /// One turn of the wait for the resumed agent to take a message, in the
    /// same live operation only. A stop, a revoke or a lost source ends it at
    /// once; so does a replaced root, an ended session, the budget or the
    /// deadline. Each read runs off the actor with the operation's loans, and
    /// only a ready session prepares the send.
    #[allow(clippy::too_many_arguments)]
    fn wait(
        &mut self,
        store: &mut Store,
        id: String,
        consent: Arc<Consent>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<ReadyProbe>,
        until: Instant,
        mut gap: Gap,
        next: Instant,
    ) -> Result<()> {
        // A read in flight finishes first: it holds the loans, and it is
        // bounded.
        let ready = match probe {
            Some(probe) if !probe.worker.is_finished() => {
                self.work = Some(Work::Waiting {
                    id,
                    consent,
                    roots,
                    token,
                    probe: Some(probe),
                    until,
                    gap,
                    next,
                });
                return Ok(());
            }
            Some(probe) => Some(
                probe
                    .worker
                    .join()
                    .ok()
                    .and_then(|_| probe.result.try_recv().ok())
                    .unwrap_or((Readiness::Ended(ready::UNREAD), None)),
            ),
            None => None,
        };
        let row = ledger::load(&store.db, &id)?.context("session resume disappeared")?;
        if consent.cancel.load(Ordering::Acquire)
            || self.check(&store.db, &consent, &row, None).is_err()
        {
            return self.end_wait(store, &id, ledger::VETOED);
        }
        // The wait's own budget ends at `until` (never past the operation's
        // deadline): a ready read that ends after it, however it began,
        // starts nothing.
        let expired = if until >= consent.chain() {
            ledger::CHAIN_EXPIRED
        } else {
            ledger::WAIT_EXPIRED
        };
        match ready {
            Some((Readiness::Ready, _)) if Instant::now() < until => {
                return self.prepare(&row, Child::Send, consent, Some((roots, token)));
            }
            Some((Readiness::Ready, _)) => return self.end_wait(store, &id, expired),
            Some((Readiness::Ended(code), _)) => return self.end_wait(store, &id, code),
            Some((Readiness::Waiting, supervisor)) => {
                gap.since = None;
                if supervisor.is_some() {
                    gap.supervisor = supervisor;
                }
            }
            // A read gap ends the wait only once it lasted the grace from its
            // first unreadable sample and the run's last proven supervisor
            // exactly exited (zerus-5sl7r). A run never recorded, or one whose
            // supervisor runs or cannot be read, keeps the wait.
            Some((Readiness::Unrecorded, _)) => {
                let since = *gap.since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.record_grace
                    && gap
                        .supervisor
                        .as_mut()
                        .is_some_and(ready::Supervisor::exited)
                {
                    return self.end_wait(store, &id, ready::READY_GONE);
                }
            }
            None => {}
        }
        let now = Instant::now();
        if now >= until {
            return self.end_wait(store, &id, expired);
        }
        if now < next {
            self.work = Some(Work::Waiting {
                id,
                consent,
                roots,
                token,
                probe: None,
                until,
                gap,
                next,
            });
            return Ok(());
        }
        let plan = row.verified.plan.clone();
        let run = row
            .bound()
            .map(|(resumed, _, _)| resumed.result_target.run_id.clone())
            .context("session resume is not waiting")?;
        let loans = (self.owner.clone(), consent.clone(), token.clone());
        let store_roots = roots.clone();
        let observation_wait = self.observation_wait;
        let observing = consent.clone();
        // The birth an earlier read pinned stays with the same supervisor.
        let known = gap.supervisor.clone();
        #[cfg(target_os = "linux")]
        let image = self.image.clone();
        #[cfg(test)]
        let observer = self.observer.clone();
        #[cfg(test)]
        let hook = self.ready_hook.clone();
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("session-resume-wait".into())
            .spawn(move || {
                let _loans = loans;
                #[cfg(test)]
                if let Some(hook) = &hook {
                    hook();
                }
                // A resumed Codex's observation ends with the wait at the latest.
                let deadline = (Instant::now() + observation_wait).min(until);
                let readiness = ready::read_supervised(
                    &store_roots,
                    &plan,
                    &run,
                    deadline,
                    known.as_ref(),
                    |args, input, until| {
                        observe(
                            args,
                            input,
                            until,
                            &observing,
                            #[cfg(target_os = "linux")]
                            image.as_ref(),
                            #[cfg(test)]
                            observer.as_ref(),
                        )
                    },
                );
                let _ = send.send(readiness);
            })?;
        self.work = Some(Work::Waiting {
            id,
            consent,
            roots,
            token,
            probe: Some(ReadyProbe { worker, result }),
            until,
            gap,
            next: now + self.poll_every,
        });
        Ok(())
    }
    /// Ends the wait without the send, for the closed reason `code`.
    fn end_wait(&mut self, store: &mut Store, id: &str, code: &str) -> Result<()> {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = ledger::load(&tx, id)?.context("session resume disappeared")?;
        if !row.receipt.drained {
            ledger::wait_ended(&tx, &row, code)?;
        }
        tx.commit()?;
        self.admitted.remove(id);
        Ok(())
    }
    fn retire(&mut self, store: &mut Store, id: &str) -> Result<()> {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = ledger::load(&tx, id)?.context("session resume disappeared")?;
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
            "code":if self.failed {"session_resume_owner_unavailable"}else if self.drained(){"drained"}else if self.stopping{"draining"}else if self.lease.is_some(){"running"}else{"awaiting_handoff"},
            "drained":self.drained(),"admitted":self.admitted.len(),"working":self.work.is_some(),"undrained":self.charges})
    }
    #[cfg(test)]
    pub(super) fn fixture_executor(&mut self, machine: &str, executor: Executor) {
        self.machine = Some(Machine::fixture(machine));
        self.executor = Some(executor);
    }
    /// The fixture source without an executor: the original path runs, and a
    /// test image's own-image refusal stops it before any child.
    #[cfg(test)]
    pub(super) fn fixture_machine(&mut self, machine: &str) {
        self.machine = Some(Machine::fixture(machine));
    }
    /// Stands in for the observation child of a resumed Codex, with this
    /// deadline for each observation.
    #[cfg(test)]
    pub(super) fn fixture_observer(&mut self, observer: Option<Observer>, wait: Duration) {
        self.observer = observer;
        self.observation_wait = wait;
    }
    /// Runs `hook` first in each read of the waiting operation's resumed
    /// session.
    #[cfg(test)]
    pub(super) fn fixture_ready_hook(&mut self, hook: Hook) {
        self.ready_hook = Some(hook);
    }
    /// Runs `hook` first in each preparation worker.
    #[cfg(test)]
    pub(super) fn fixture_prepare_hook(&mut self, hook: Hook) {
        self.prepare_hook = Some(hook);
    }
    /// Sets the wait's budget, its read interval and the operation's bound
    /// from its first turn.
    #[cfg(test)]
    pub(super) fn fixture_wait(&mut self, wait: Duration, every: Duration, chain: Duration) {
        self.wait = wait;
        self.poll_every = every;
        self.chain = chain;
    }
    /// Sets the grace for a missing record of the resumed run.
    #[cfg(test)]
    pub(super) fn fixture_record_grace(&mut self, grace: Duration) {
        self.record_grace = grace;
    }
    /// `Some(None)` proves a live queued consent whose own turn has not
    /// started its clock; `None` means no such original was admitted.
    #[cfg(test)]
    pub(super) fn fixture_chain_started(&self, id: &str) -> Option<Option<Instant>> {
        self.admitted
            .get(id)
            .map(|consent| consent.fixture_chain_started())
    }
    #[cfg(test)]
    pub(super) fn fixture_mac_fence(&mut self, on: bool) {
        self.mac_fence = on;
    }
    /// Runs `hook` in the final spawn turn, after the worker's own checks and
    /// before the consent veto and the spawn.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn fixture_spawn_hook(&mut self, hook: Hook) {
        self.spawn_hook = Some(hook);
    }
    /// Runs `hook` in the drain worker after the receipt bytes are read, and
    /// sets the read's deadline.
    #[cfg(test)]
    pub(super) fn fixture_read(&mut self, hook: Option<Hook>, wait: Duration) {
        self.read_hook = hook;
        self.read_wait = wait;
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn fixture_image(&mut self, path: &std::path::Path) {
        self.image = Some(Arc::new(Pin::open(path).unwrap()));
    }
}
/// The send's observation child of a resumed Codex (zerus-5wmh.66.4.10):
/// this computer's own pinned image in its own process group, with no
/// custody, or the test observer. Its reply within `until`, or nothing;
/// without a pinned image nothing runs.
fn observe(
    args: &[String],
    input: &[u8],
    until: Instant,
    consent: &Consent,
    #[cfg(target_os = "linux")] image: Option<&Arc<Pin>>,
    #[cfg(test)] observer: Option<&Observer>,
) -> Option<Vec<u8>> {
    #[cfg(test)]
    if let Some(observer) = observer {
        return observer(args, input);
    }
    #[cfg(target_os = "linux")]
    if let Some(image) = image {
        return child::observe(image.executable(), args, input, until, |command| {
            // A consent stopped since the read began starts nothing.
            if consent.cancel.load(Ordering::Acquire) || !image.current() {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            crate::desktop::original_child::inherit_pinned(image, command)
        });
    }
    let _ = (args, input, until, consent);
    None
}

/// One original child: `hgs session-action <session> --json` or `hgs send
/// <session> --json`, each with its exact stdin. Nothing exists before the
/// spawn, so every refusal here never started.
#[allow(clippy::too_many_arguments)]
fn original(
    child: Child,
    consent: &Consent,
    roots: &Roots,
    lock: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    launch: &Launch,
    observation_wait: Duration,
    #[cfg(target_os = "linux")] image: Option<Arc<Pin>>,
    #[cfg(test)] executor: Option<Executor>,
    #[cfg(test)] observer: Option<Observer>,
    #[cfg(test)] hook: Option<Hook>,
) -> Ended {
    let plan = &consent.original.plan;
    let (args, input) = (&launch.args, launch.input.as_slice());
    // The send to a resumed Codex starts only after the send's own
    // observation, run once more here before the spawn turn, says the
    // resumed agent takes it now (zerus-5wmh.66.4.10). Every other child
    // has no observation.
    #[cfg(target_os = "linux")]
    let observer_image = image.clone();
    let observed = || {
        child == Child::Resume
            || !plan.observed()
            || ready::read(
                roots,
                plan,
                &launch.run,
                (Instant::now() + observation_wait).min(consent.chain()),
                |args, input, until| {
                    observe(
                        args,
                        input,
                        until,
                        consent,
                        #[cfg(target_os = "linux")]
                        observer_image.as_ref(),
                        #[cfg(test)]
                        observer.as_ref(),
                    )
                },
            ) == Readiness::Ready
    };
    // Last before the spawn, after every slow check and the turn wait: the
    // selected session's actual record of the run this child expects (the
    // original run for the resume, the resumed one for the send) and, for
    // the send, its readiness by the record (for a resumed Codex: not
    // ended), read again from the retained original roots; then a consent
    // stopped since the worker began (a revoke, a lost source, a stop).
    let current = || {
        target::current(roots, plan, &launch.run)
            && (child == Child::Resume || ready::current(roots, plan, &launch.run))
            && !consent.cancel.load(Ordering::Acquire)
    };
    // The test executor receives exactly what the child would; it stands in
    // for the spawn turn, so the veto runs before it as in `child::run`.
    #[cfg(test)]
    if let Some(executor) = executor {
        if !observed() {
            return Ended::NotStarted;
        }
        if let Some(hook) = &hook {
            hook();
        }
        // The chain stands in for the runner's own deadline check, last.
        if !current() || Instant::now() >= consent.chain() {
            return Ended::NotStarted;
        }
        return executor(plan, args, input, lock, resources);
    }
    #[cfg(target_os = "linux")]
    if image.is_some() && !observed() {
        return Ended::NotStarted;
    }
    #[cfg(target_os = "linux")]
    if let Some(image) = image {
        let deadline = (Instant::now() + DEADLINE).min(consent.chain());
        return child::run(
            image.executable(),
            args,
            input,
            lock,
            resources,
            deadline,
            |command| {
                #[cfg(test)]
                if let Some(hook) = &hook {
                    hook();
                }
                // The runner's own deadline check follows, bounded by the
                // chain.
                if !current() || !image.current() {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                crate::desktop::original_child::inherit_pinned(&image, command)
            },
        );
    }
    // No pinned image (a unit-test image, or one already gone), and macOS,
    // whose admission refuses first: nothing starts.
    let _ = (observed, current, args, input, lock, resources);
    #[cfg(test)]
    let _ = hook;
    Ended::NotStarted
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
