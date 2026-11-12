//! One fixed local deferred send executor (zerus-5wmh.66.4.4, slice 3),
//! independent of the UI lifetime. Native admission, the one original child
//! and the original resource drain are separate; only the drain, through the
//! CLI's own receipt, settles a send.
// Only the Linux runner starts an original child; macOS refuses every
// original at admission.
#[cfg(target_os = "linux")]
use super::child;
use super::child::Ended;
use super::receipt::{self, Evidence, Observed, Roots};
use super::{admitted::Admitted, ledger, target, Reservation};
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
    desktop::deferred_send::{self as wire, Phase, Scope},
    run::custody::InheritedLock,
};
use zerus_desktop_ledger::operations::{
    custody::{Prepared, Record},
    Ticket,
};

/// The refusal of every macOS original until a qualified child seam exists.
pub(super) const MAC_UNAVAILABLE: &str = "deferred_send_native_image_unavailable";
/// What one owner-approved question route admission did (zerus-sa9qn.8).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(in crate::desktop) enum Routed {
    /// The route's reservation and the queued original committed together.
    Admitted,
    /// The route's own reservation recorded a final refusal; no original.
    Refused,
    /// Nothing was written: the owner, its lease or capacity is unavailable,
    /// or the request already has an original.
    Unavailable,
    /// The person's switch was off at the reservation: nothing was written.
    Off,
}
/// The conservative refusal: the request may have been admitted.
const UNAVAILABLE: &str = "deferred_send_unavailable";
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
/// The whole send, the CLI's own lock wait included (G1 condition 6).
#[cfg(any(test, target_os = "linux"))]
pub(super) const DEADLINE: Duration = Duration::from_secs(30);
/// The one bounded deadline of the receipt read after the physical drain,
/// never renewed: a later result is Unknown.
pub(super) const READ_WAIT: Duration = Duration::from_secs(5);
/// How often a draining original's custody is observed again.
const PROBE_EVERY: Duration = Duration::from_millis(250);
#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;

/// Stands in for the one child in tests: it receives the plan, exactly the
/// arguments and stdin the child would be given, the custody lock and the
/// original's resources guard.
#[cfg(test)]
pub(in crate::desktop) type Executor = Arc<
    dyn Fn(&wire::Plan, &[String], &[u8], &Arc<InheritedLock>, &Arc<dyn Send + Sync>) -> Ended
        + Send
        + Sync,
>;
/// The original's physical resources: the SAME custody lock and its live admission,
/// which holds the lease and with it the serve-owner loan. Every original
/// pipe keeps a copy until its real end, so neither the custody drain nor a
/// takeover can pass while a descendant still holds one.
struct Original {
    _custody: Arc<InheritedLock>,
    _live: Arc<Admitted>,
}
/// What preparation opens off the actor before any child: the exact custody
/// and the original receipt store.
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
enum Work {
    Preparing {
        ticket: Ticket,
        live: Arc<Admitted>,
        worker: JoinHandle<()>,
        result: mpsc::Receiver<Result<Prepared2>>,
    },
    Running {
        ticket: Ticket,
        live: Arc<Admitted>,
        worker: JoinHandle<()>,
        result: mpsc::Receiver<Ended>,
        resources: Arc<Original>,
        roots: Arc<Roots>,
        token: Arc<()>,
    },
    Draining {
        id: String,
        live: Arc<Admitted>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<Probe>,
        next: Instant,
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
    admitted: BTreeMap<String, Arc<Admitted>>,
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
    /// Runs in the final spawn turn, before the admission veto.
    #[cfg(test)]
    spawn_hook: Option<Hook>,
    /// Runs in the drain worker after the receipt bytes are read.
    #[cfg(test)]
    read_hook: Option<Hook>,
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
            #[cfg(test)]
            spawn_hook: None,
            #[cfg(test)]
            read_hook: None,
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
        for live in self.admitted.values() {
            live.stop();
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
        if ours && self.fences.deferred_send.sealed(&id) {
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
                "deferred send owner unavailable"
            );
            ensure!(
                self.known_charges(&store.db)?,
                "deferred send original resources retained"
            );
            let lease = self
                .lease
                .as_ref()
                .context("deferred send lease unavailable")?
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
            let intent = authority.deferred_send()?;
            ensure!(
                intent.scope == self.scope,
                "deferred send connection changed"
            );
            let verified = intent.verify().map_err(anyhow::Error::msg)?;
            lease.current(
                &tx,
                &self.location,
                self.machine.as_ref(),
                self.exclusion.as_ref(),
            )?;
            let live = Admitted::person_consent(verified, lease.clone())?;
            if original.is_none() {
                // A corrupt or incomplete budget is not a capacity proof.
                proof = Proof::Unprovable;
                let capacity = ledger::capacity(&tx, &live.original)?;
                proof = Proof::NoRow;
                ensure!(capacity, "deferred send capacity unavailable");
            }
            // From here on nothing proves the request was not admitted.
            proof = Proof::Admitting;
            let row = ledger::admit(
                &tx,
                &live.original,
                &lease.writer_epoch,
                chrono::Utc::now().timestamp_millis(),
            )?;
            tx.commit()?;
            // The same original again gets only its own receipt; only the
            // first admission holds the live admission.
            if original.is_none() {
                self.admitted.insert(id.clone(), live);
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
    /// Whether an admission can be attempted now: running with a current
    /// lease, and not fenced.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn ready(&self) -> bool {
        !self.stopping && !self.failed && !self.mac_fence && self.lease.is_some()
    }
    /// This runtime's connection scope: a route's intent names it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    /// The one admission of an owner-approved, service-originated question
    /// route (zerus-sa9qn.8). It takes no UI permit and constructs no
    /// person's consent: [`Admitted::owner_approved_route`] admits only the
    /// fixed [`wire::Kind::RouteQuestion`] original that
    /// [`wire::Intent::verify_route`] re-derives. `reserve` runs in the same
    /// BEGIN IMMEDIATE transaction, after every actor check and before the
    /// original row: it writes the route's own ledger row and returns
    /// whether the route proceeds, was refused as final, or is off by the
    /// person's switch (nothing written). Any error writes nothing.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn route(
        &mut self,
        store: &mut Store,
        intent: wire::Intent,
        exclusion: Option<Arc<HandoffLock>>,
        reserve: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<Reservation>,
    ) -> Routed {
        // v1 routes only through the Linux child seam; macOS refuses every
        // original until its seam is qualified.
        if self.mac_fence || cfg!(not(target_os = "linux")) {
            return Routed::Unavailable;
        }
        self.refresh(store, exclusion);
        let id = intent.request_id.clone();
        let result = (|| -> Result<Routed> {
            ensure!(
                !self.stopping && !self.failed && store.creation_recovery_ok(),
                "question route owner unavailable"
            );
            ensure!(
                self.known_charges(&store.db)?,
                "deferred send original resources retained"
            );
            let lease = self
                .lease
                .as_ref()
                .context("question route lease unavailable")?
                .clone();
            ensure!(intent.scope == self.scope, "question route scope changed");
            let verified = intent.verify_route().map_err(anyhow::Error::msg)?;
            let tx = store
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // An existing original of this request is never admitted again.
            if ledger::load(&tx, &id)?.is_some() {
                return Ok(Routed::Unavailable);
            }
            lease.current(
                &tx,
                &self.location,
                self.machine.as_ref(),
                self.exclusion.as_ref(),
            )?;
            let live = Admitted::owner_approved_route(verified, lease.clone())?;
            if !ledger::capacity(&tx, &live.original)? {
                return Ok(Routed::Unavailable);
            }
            match reserve(&tx)? {
                // Dropping the transaction rolls back: nothing is written.
                Reservation::Off => return Ok(Routed::Off),
                Reservation::Refused => {
                    tx.commit()?;
                    return Ok(Routed::Refused);
                }
                Reservation::Full => {
                    tx.commit()?;
                    return Ok(Routed::Unavailable);
                }
                Reservation::Reserved => {}
            }
            ledger::admit(
                &tx,
                &live.original,
                &lease.writer_epoch,
                chrono::Utc::now().timestamp_millis(),
            )?;
            tx.commit()?;
            self.admitted.insert(id, live);
            Ok(Routed::Admitted)
        })();
        result.unwrap_or(Routed::Unavailable)
    }
    /// The closed refusal of a request proven never admitted, sealed for the
    /// epoch before the reply; a full fence mints none (zerus-tlb9q).
    fn not_admitted(&self, id: String) -> Value {
        if self.fences.deferred_send.seal(&id) {
            self.refusal(id, wire::NOT_ADMITTED)
        } else {
            self.refusal(id, UNAVAILABLE)
        }
    }
    fn check(
        &self,
        db: &rusqlite::Connection,
        live: &Admitted,
        row: &ledger::Row,
        ticket: Option<&Ticket>,
    ) -> Result<()> {
        ensure!(
            !self.stopping && !self.failed,
            "deferred send owner stopping"
        );
        ensure!(
            self.lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &live.lease)),
            "deferred send lease replaced"
        );
        live.lease.current(
            db,
            &self.location,
            self.machine.as_ref(),
            self.exclusion.as_ref(),
        )?;
        live.check(row, ticket)
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
                    live,
                    worker,
                    result,
                } => {
                    if !worker.is_finished() {
                        self.work = Some(Work::Preparing {
                            ticket,
                            live,
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
                    let Some((prepared, held, roots)) = prepared else {
                        self.retire(store, &ticket.admission_id)?;
                        return Ok(());
                    };
                    let record = Record {
                        prepared,
                        helper: None,
                    };
                    let token = Arc::new(());
                    let id = ticket.admission_id.clone();
                    let reserved = (|| -> Result<()> {
                        let tx = store
                            .db
                            .transaction_with_behavior(TransactionBehavior::Immediate)?;
                        let row = ledger::load(&tx, &id)?.context("deferred send disappeared")?;
                        self.check(&tx, &live, &row, None)?;
                        ensure!(ticket.lease == live.lease.id, "deferred send lease changed");
                        ledger::reserve(&tx, &row, &ticket, &record)?;
                        tx.commit()?;
                        Ok(())
                    })();
                    if reserved.is_err() {
                        drop(held);
                        self.retire(store, &id)?;
                        return Ok(());
                    }
                    // This runtime drains its own original from the store it
                    // opened; Store recovery skips it while the token lives.
                    store
                        .send_owned()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(id.clone(), Arc::downgrade(&token));
                    let (done_send, result) = mpsc::sync_channel(1);
                    let selected = live.clone();
                    let lock = held.clone();
                    let resources = Arc::new(Original {
                        _custody: held,
                        _live: live.clone(),
                    });
                    let guard: Arc<dyn Send + Sync> = resources.clone();
                    let store_roots = roots.clone();
                    #[cfg(test)]
                    let executor = self.executor.clone();
                    #[cfg(test)]
                    let hook = self.spawn_hook.clone();
                    #[cfg(target_os = "linux")]
                    let image = self.image.clone();
                    let spawned =
                        thread::Builder::new()
                            .name("deferred-send".into())
                            .spawn(move || {
                                // The selected session's actual backend and run;
                                // the admission veto runs again in the final spawn
                                // turn.
                                let value = if selected.cancel.load(Ordering::Acquire)
                                    || !target::current(&store_roots, &selected.original.plan)
                                {
                                    Ended::NotStarted
                                } else {
                                    send(
                                        &selected,
                                        &lock,
                                        &guard,
                                        #[cfg(target_os = "linux")]
                                        image,
                                        #[cfg(test)]
                                        executor,
                                        #[cfg(test)]
                                        hook,
                                    )
                                };
                                let _ = done_send.send(value);
                            });
                    match spawned {
                        Ok(worker) => {
                            self.work = Some(Work::Running {
                                ticket,
                                live,
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
                    ticket,
                    live,
                    worker,
                    result,
                    resources,
                    roots,
                    token,
                } => {
                    if !worker.is_finished() {
                        self.work = Some(Work::Running {
                            ticket,
                            live,
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
                        .unwrap_or(Ended::Unknown("deferred_send_unobserved"));
                    let tx = store
                        .db
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let row = ledger::load(&tx, &ticket.admission_id)?
                        .context("deferred send disappeared")?;
                    ensure!(
                        row.ticket.as_ref() == Some(&ticket),
                        "deferred send original ticket changed"
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
                        id: ticket.admission_id,
                        live,
                        roots,
                        token,
                        probe: None,
                        next: Instant::now(),
                    });
                }
                Work::Draining {
                    id,
                    live,
                    roots,
                    token,
                    probe,
                    next,
                } => return self.drain(store, id, live, roots, token, probe, next),
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
        let Some(live) = self.admitted.get(&id).cloned() else {
            self.retire(store, &id)?;
            return Ok(());
        };
        self.check(&store.db, &live, &row, None)?;
        let ticket = Ticket {
            operation_id: row
                .verified
                .binding
                .operation_id()
                .context("invalid send key")?,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: row.receipt.revision + 1,
            admission_id: id,
            binding_sha256: row
                .verified
                .binding
                .fingerprint()
                .context("invalid send binding")?,
            writer_epoch: live.lease.writer_epoch.clone(),
            service_epoch: self.scope.service_epoch.clone(),
            lease: live.lease.id.clone(),
        };
        let original = ticket.clone();
        let held = live.clone();
        let location = Location {
            root: self.location.root.clone(),
            configuration: self.location.configuration.clone(),
        };
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("deferred-send-prepare".into())
            .spawn(move || {
                let _original = held;
                // The original receipt store, opened before any child: the
                // CLI's state root, of which this service's root is a folder.
                let prepared = location
                    .root
                    .parent()
                    .context("deferred send state unavailable")
                    .and_then(|state| Ok(Arc::new(Roots::open(state, receipt::FOLDER)?)))
                    .and_then(|roots| {
                        let (prepared, held) = custody::prepare(&location, &original)?;
                        Ok((prepared, held, roots))
                    });
                let _ = send.send(prepared);
            })?;
        self.work = Some(Work::Preparing {
            ticket,
            live,
            worker,
            result,
        });
        Ok(())
    }
    /// One drain turn of this runtime's own original: the custody probe off
    /// the actor and, after its release, the one bounded read of the CLI's
    /// receipt from the original store. The worker holds the serve-owner and
    /// the original's admission loans and the ownership token until it
    /// physically ends, so neither a stop nor a teardown passes it.
    #[allow(clippy::too_many_arguments)]
    fn drain(
        &mut self,
        store: &mut Store,
        id: String,
        live: Arc<Admitted>,
        roots: Arc<Roots>,
        token: Arc<()>,
        probe: Option<Probe>,
        next: Instant,
    ) -> Result<()> {
        let keep = |me: &mut Self, probe: Option<Probe>, next: Instant| {
            me.work = Some(Work::Draining {
                id: id.clone(),
                live: live.clone(),
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
                    let row = ledger::load(&tx, &id)?.context("deferred send disappeared")?;
                    ensure!(
                        row.custody.as_ref() == Some(&record),
                        "deferred send custody scope changed"
                    );
                    if row.receipt.drained {
                        tx.commit()?;
                    } else {
                        ledger::drained(tx, row, observation, &cli, evidence)?;
                    }
                    self.admitted.remove(&id);
                    Ok(())
                }
                Some((_, Drain::Held)) => {
                    keep(self, None, Instant::now() + PROBE_EVERY);
                    Ok(())
                }
                Some((_, Drain::Unavailable)) | None => {
                    keep(self, None, Instant::now() + PROBE_EVERY);
                    anyhow::bail!("deferred send original custody unavailable")
                }
            };
        }
        if Instant::now() < next {
            keep(self, None, next);
            return Ok(());
        }
        let row = ledger::load(&store.db, &id)?.context("deferred send disappeared")?;
        if row.receipt.drained {
            self.admitted.remove(&id);
            return Ok(());
        }
        let record = row
            .custody
            .clone()
            .context("deferred send has no original custody")?;
        let plan = row.verified.plan.clone();
        let location = Location {
            root: self.location.root.clone(),
            configuration: self.location.configuration.clone(),
        };
        let loans = (self.owner.clone(), live.clone(), token.clone());
        let store_roots = roots.clone();
        let wait = self.read_wait;
        #[cfg(test)]
        let hook = self.read_hook.clone();
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("deferred-send-drain".into())
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
                        let (cli, evidence) = receipt::read(&store_roots, &plan, &between);
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
    fn retire(&mut self, store: &mut Store, id: &str) -> Result<()> {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = ledger::load(&tx, id)?.context("deferred send disappeared")?;
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
            "code":if self.failed {"deferred_send_owner_unavailable"}else if self.drained(){"drained"}else if self.stopping{"draining"}else if self.lease.is_some(){"running"}else{"awaiting_handoff"},
            "drained":self.drained(),"admitted":self.admitted.len(),"working":self.work.is_some(),"undrained":self.charges})
    }
    #[cfg(test)]
    pub(in crate::desktop) fn fixture_executor(&mut self, machine: &str, executor: Executor) {
        self.machine = Some(Machine::fixture(machine));
        self.executor = Some(executor);
    }
    /// The fixture source without an executor: the original path runs, and a
    /// test image's own-image refusal stops it before any child.
    #[cfg(test)]
    pub(super) fn fixture_machine(&mut self, machine: &str) {
        self.machine = Some(Machine::fixture(machine));
    }
    /// Constructs a live admission of `original` with this runtime's lease
    /// under the person's consent (`route` false) or the owner-approved route
    /// (`route` true), and drops it: each authority admits only its own kind.
    /// Only the Linux question route tests use it.
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::desktop) fn fixture_admit_as(
        &self,
        original: wire::Verified,
        route: bool,
    ) -> Result<()> {
        let lease = self
            .lease
            .clone()
            .context("deferred send lease unavailable")?;
        if route {
            Admitted::owner_approved_route(original, lease).map(drop)
        } else {
            Admitted::person_consent(original, lease).map(drop)
        }
    }
    #[cfg(test)]
    pub(in crate::desktop) fn fixture_mac_fence(&mut self, on: bool) {
        self.mac_fence = on;
    }
    /// Runs `hook` in the final spawn turn, after the worker's own checks and
    /// before the admission veto and the spawn.
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
/// The one original child: `hgs send <session> --json` with the frozen
/// stdin. Nothing exists before the spawn, so every refusal here never
/// started.
fn send(
    live: &Admitted,
    lock: &Arc<InheritedLock>,
    resources: &Arc<dyn Send + Sync>,
    #[cfg(target_os = "linux")] image: Option<Arc<Pin>>,
    #[cfg(test)] executor: Option<Executor>,
    #[cfg(test)] hook: Option<Hook>,
) -> Ended {
    let plan = &live.original.plan;
    let args = plan.args().to_vec();
    let input = live.original.intent.original.as_bytes();
    // The test executor receives exactly what the child would; it stands in
    // for the spawn turn, so the veto runs before it as in `child::run`.
    #[cfg(test)]
    if let Some(executor) = executor {
        if let Some(hook) = &hook {
            hook();
        }
        if live.cancel.load(Ordering::Acquire) {
            return Ended::NotStarted;
        }
        return executor(plan, &args, input, lock, resources);
    }
    #[cfg(target_os = "linux")]
    if let Some(image) = image {
        let deadline = Instant::now() + DEADLINE;
        return child::run(
            image.executable(),
            &args,
            input,
            lock,
            resources,
            deadline,
            |command| {
                #[cfg(test)]
                if let Some(hook) = &hook {
                    hook();
                }
                // Last before the spawn: an admission stopped since the worker
                // began (a revoke, a lost source, a stop) starts nothing.
                if live.cancel.load(Ordering::Acquire) || !image.current() {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                image.inherit(command)
            },
        );
    }
    // No pinned image (a unit-test image, or one already gone), and macOS,
    // whose admission refuses first: nothing starts.
    let _ = (args, input, lock, resources);
    #[cfg(test)]
    let _ = hook;
    Ended::NotStarted
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
    }
}
