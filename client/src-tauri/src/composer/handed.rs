//! A Send handed to the desktop service (zerus-5wmh.66.4.4, slice 3, the
//! deferred_send kind): the route of the one original and the read-only
//! checks of its result.
//!
//! Only the Send action that reserved and durably saved the checkpoint
//! offers its original ([`Composers::offer`]). A reload or a reopened session
//! never offers anything: it only looks the held identity up. While the
//! service still reports the original in progress, the draft stays uncertain
//! without Allow retry, shows [`STILL_SENDING`] as a normal status and is
//! checked again with a bounded back-off, only while its session is open and
//! the window is shown. A settled result ends it as the direct path would:
//! Recorded clears the draft, a child that never started releases its
//! identity, and Unknown stays uncertain for Allow retry. A service that
//! cannot be read leaves it uncertain with Allow retry.
//!
//! A "Compact and continue" the service takes (zerus-5wmh.66.4.8, the
//! compact_to_send kind) is handed over the same way from its compaction's
//! call ([`Composers::hand_compaction`]): from the moment its offer is
//! written, the draft is a delivery of that message held by the service,
//! shown with [`STILL_SENDING`] and a locked Cancel send, and its result ends
//! it as a handed send's would.
//!
//! A "Resume and send" of a stopped Claude, Kimi or Codex session the
//! service takes (zerus-5wmh.66.4.9 and .10, the session_resume kind) is
//! handed over the same way from its start's call
//! ([`Composers::hand_start`]). A fresh start and a resume with compaction
//! stay direct, and so does a Codex session for a service that does not
//! observe its resumed agent.
use super::send::{Call, Delivery};
use super::*;
use crate::desktop_service::compact_to_send::{self, CompactAnswer};
use crate::desktop_service::deferred_send::{self, Checked, SendAnswer, STILL_SENDING};
use crate::desktop_service::session_resume::{self, ResumeAnswer};
use zerus_client_core::actions::Outcome;
use zerus_client_core::attachments::StoredFile;
use zerus_client_core::desktop::compact_to_send::Plan as CompactPlan;
use zerus_client_core::desktop::deferred_send::Plan;
use zerus_client_core::desktop::session_resume::Plan as ResumePlan;
use zerus_client_core::operation_recovery::deferred_input::{
    Checkpoint, Kind, Start, SESSION_RESUME_VERSION,
};

/// The first check after a hand-over, and the longest wait between two.
const FIRST: f64 = 2.0;
const LONGEST: f64 = 30.0;

type SendFn = dyn Fn(&str, &Checkpoint, &[u8]) -> SendAnswer + Send + Sync;
type CheckFn = dyn Fn(&Plan) -> Checked + Send + Sync;
type HandingFn<'a> = dyn Fn(&CompactPlan) -> bool + 'a;
type CompactFn =
    dyn Fn(&str, &Checkpoint, &[u8], &[u8], &HandingFn<'_>) -> CompactAnswer + Send + Sync;
type CompactCheckFn = dyn Fn(&CompactPlan) -> Checked + Send + Sync;
type ResumeHandingFn<'a> = dyn Fn(&ResumePlan) -> bool + 'a;
type ResumeFn =
    dyn Fn(&str, &Checkpoint, &[u8], &[u8], &ResumeHandingFn<'_>) -> ResumeAnswer + Send + Sync;
/// A check of a handed resume, with the run its resume's receipt returned.
type ResumeCheckFn = dyn Fn(&ResumePlan) -> (Checked, Option<String>) + Send + Sync;

/// The selected desktop service's deferred send route.
pub struct DeferredRoute {
    /// Offers the original once and waits, bounded, for its result.
    pub send: Arc<SendFn>,
    /// One read-only lookup by the exact plan. It never sends anything.
    pub check: Arc<CheckFn>,
}

/// The selected desktop service's compact-to-send route.
pub struct CompactRoute {
    /// Offers the original once, calling its hand-over hook with the plan
    /// right before the offer is written, and waits, bounded, for its
    /// result.
    pub offer: Arc<CompactFn>,
    /// One read-only lookup by the exact plan. It never compacts or sends.
    pub check: Arc<CompactCheckFn>,
}

/// The selected desktop service's session resume route.
pub struct ResumeRoute {
    /// Offers the original once, calling its hand-over hook with the plan
    /// right before the offer is written, and waits, bounded, for its
    /// result.
    pub offer: Arc<ResumeFn>,
    /// One read-only lookup by the exact plan, with the run its resume's
    /// receipt returned once recorded. It never resumes or sends.
    pub check: Arc<ResumeCheckFn>,
}

/// The original a handed draft holds: a send, a compact-to-send or a
/// resume-then-send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HandedPlan {
    Send(Plan),
    Compact(CompactPlan),
    Resume(ResumePlan),
}
impl HandedPlan {
    /// The identity the draft's checkpoint holds.
    fn held(&self) -> &str {
        match self {
            Self::Send(plan) => &plan.request_id,
            Self::Compact(plan) => &plan.compaction_request_id,
            Self::Resume(plan) => &plan.resume_request_id,
        }
    }
    /// The request of its message's local row: the send.
    pub(crate) fn row(&self) -> &str {
        match self {
            Self::Send(plan) => &plan.request_id,
            Self::Compact(plan) => &plan.send_request_id,
            Self::Resume(plan) => &plan.send_request_id,
        }
    }
    fn message_sha256(&self) -> &str {
        match self {
            Self::Send(plan) => &plan.message_sha256,
            Self::Compact(plan) => &plan.message_sha256,
            Self::Resume(plan) => &plan.message_sha256,
        }
    }
}

/// The continuation a hand-over ends: the compaction of a "Compact and
/// continue" or the start of a "Resume and send".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Continuation {
    Compaction,
    Start,
}
impl Continuation {
    fn start(self) -> bool {
        self == Self::Start
    }
}

/// What the service says about a continuation it was offered, either kind;
/// a settled resume names the run its message's row belongs to.
enum Continued {
    Settled(SendAnswer, Option<String>),
    Pending(HandedPlan),
    Direct,
}
impl From<CompactAnswer> for Continued {
    fn from(answer: CompactAnswer) -> Self {
        match answer {
            CompactAnswer::Settled(answer) => Self::Settled(answer, None),
            CompactAnswer::Pending(plan) => Self::Pending(HandedPlan::Compact(plan)),
            CompactAnswer::Direct => Self::Direct,
        }
    }
}
impl From<ResumeAnswer> for Continued {
    fn from(answer: ResumeAnswer) -> Self {
        match answer {
            ResumeAnswer::Settled(answer, run) => Self::Settled(answer, run),
            ResumeAnswer::Pending(plan) => Self::Pending(HandedPlan::Resume(plan)),
            ResumeAnswer::Direct => Self::Direct,
        }
    }
}
/// A continuation's two exact stdins, as its direct calls would get them.
type StdinsFn<'a> = dyn Fn(&Delivery, &Checkpoint) -> Option<(Vec<u8>, Vec<u8>)> + 'a;
/// Offers a continuation once with its stdins and hand-over hook.
type OfferFn<'a> =
    dyn Fn(&Delivery, &Checkpoint, &[u8], &[u8], &dyn Fn(HandedPlan) -> bool) -> Continued + 'a;

/// An original the service holds: what its checks need.
#[derive(Clone, Debug)]
pub(crate) struct Handed {
    pub(crate) plan: HandedPlan,
    /// When the next check is due, on the hub clock.
    pub(crate) next: f64,
    pub(crate) delay: f64,
    /// A check is under way.
    pub(crate) checking: bool,
}

impl Handed {
    fn new(plan: HandedPlan, now: f64) -> Self {
        Handed {
            plan,
            next: now + FIRST,
            delay: FIRST,
            checking: false,
        }
    }
    /// A handed "Compact and continue" or "Resume and send": Cancel send
    /// stays shown, locked.
    pub(crate) fn locked(&self) -> bool {
        matches!(self.plan, HandedPlan::Compact(_) | HandedPlan::Resume(_))
    }
}

/// What a check of a held identity started from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Basis {
    /// The draft's own hand-over: a missing answer is state 5.
    Handed,
    /// A reload's lookup of an identity of unknown path: a missing answer
    /// changes nothing.
    Reload,
}

/// The exact stdin of the one `hgs send` call, as the direct path builds it.
fn stdin(
    name: &str,
    checkpoint: &Checkpoint,
    text: &str,
    attachments: &[Attachment],
) -> Option<Vec<u8>> {
    let target = zerus_client_core::Target {
        computer: String::new(),
        name: name.into(),
        run_id: checkpoint.run_id.clone(),
        conversation_id: checkpoint.conversation_id.clone(),
    };
    zerus_client_core::actions::send_input(
        &target,
        text,
        attachments,
        None,
        None,
        &checkpoint.request_id,
    )
    .ok()
}

impl Composers {
    /// Sets the route (the app, at start): Linux and macOS in
    /// desktop-service mode. Without one every Send goes direct.
    #[cfg(any(target_os = "linux", target_os = "macos", test))]
    pub fn set_deferred(&self, route: DeferredRoute) {
        *lock(&self.0.deferred) = Some(Arc::new(route));
    }

    fn deferred(&self) -> Option<Arc<DeferredRoute>> {
        lock(&self.0.deferred).clone()
    }

    /// Sets the compact-to-send route (the app, at start): Linux and macOS
    /// in desktop-service mode. Without one every "Compact and continue"
    /// goes direct.
    #[cfg(any(target_os = "linux", target_os = "macos", test))]
    pub fn set_compact(&self, route: CompactRoute) {
        *lock(&self.0.compact) = Some(Arc::new(route));
    }

    fn compact(&self) -> Option<Arc<CompactRoute>> {
        lock(&self.0.compact).clone()
    }

    /// Sets the session resume route (the app, at start): Linux and macOS in
    /// desktop-service mode. Without one every "Resume and send" goes
    /// direct.
    #[cfg(any(target_os = "linux", target_os = "macos", test))]
    pub fn set_resume(&self, route: ResumeRoute) {
        *lock(&self.0.resume) = Some(Arc::new(route));
    }

    /// Whether a session resume route is set: a "Resume and send" may be
    /// offered to the service.
    pub(crate) fn resume_routed(&self) -> bool {
        lock(&self.0.resume).is_some()
    }

    fn resume(&self) -> Option<Arc<ResumeRoute>> {
        lock(&self.0.resume).clone()
    }

    /// The window is hidden or shown again (the webview's visibility). A
    /// shown window checks a held send at once.
    pub fn set_hidden(&self, hidden: bool) {
        let now = self.now();
        let mut hub = self.lock();
        if hub.hidden == hidden {
            return;
        }
        hub.hidden = hidden;
        if !hidden {
            Self::restart_checks(&mut hub, now);
        }
    }

    /// The open composer's held send, and its session's held answers, are
    /// checked at once, their back-off starting over: on opening the
    /// session, and on showing the window.
    pub(crate) fn restart_checks(hub: &mut Hub, now: f64) {
        let Some(key) = hub.open_key.clone() else {
            return;
        };
        super::questions::restart_answer_checks(hub, &key, now);
        if let Some(handed) = hub
            .composers
            .get_mut(&key)
            .and_then(|composer| composer.handed.as_mut())
        {
            handed.next = now;
            handed.delay = FIRST;
        }
    }

    /// Offers `delivery` to the desktop service when it takes it, with the
    /// exact stdin its direct call would get. `None`: the direct path sends
    /// it, decided before any offer. Called by `send_now` once its local row
    /// shows it sending, never on a reload.
    pub(crate) fn offer(&self, delivery: &Delivery) -> Option<SendAnswer> {
        let route = self.deferred()?;
        // v1: a main session on this computer, a plain send.
        if !delivery.session.computer.is_empty()
            || delivery.session.subagent.is_some()
            || delivery
                .compaction
                .as_ref()
                .is_some_and(|id| !id.is_empty())
        {
            return None;
        }
        let checkpoint = self
            .lock()
            .composers
            .get(&delivery.key)
            .and_then(|composer| composer.checkpoint.clone())
            .filter(|checkpoint| {
                checkpoint.kind == Kind::Send && checkpoint.request_id == delivery.request_id
            })?;
        let stdin = stdin(
            &delivery.session.name,
            &checkpoint,
            &delivery.text,
            &delivery.attachments,
        )?;
        match (route.send)(&delivery.session.name, &checkpoint, &stdin) {
            SendAnswer::Direct => None,
            answer => Some(answer),
        }
    }

    /// The end of an offered send, as `send_now` records it.
    pub(crate) fn offered(&self, delivery: &Delivery, answer: SendAnswer) -> Option<Outcome> {
        Some(match answer {
            SendAnswer::Recorded(outcome) => Outcome::Done {
                receipt: serde_json::json!({"request_id": outcome.request_id}),
                cleanup: None,
            },
            SendAnswer::Unsent(detail) => Outcome::Failed { detail },
            SendAnswer::Unconfirmed(detail) => Outcome::Uncertain { detail },
            SendAnswer::Pending(plan) => {
                self.handed_over(delivery, HandedPlan::Send(plan));
                return None;
            }
            SendAnswer::Direct => unreachable!("decided before the offer"),
        })
    }

    /// The service still sends it: the delivery ends uncertain, its row
    /// keeps sending, and the draft holds its identity without Allow retry.
    fn handed_over(&self, delivery: &Delivery, plan: HandedPlan) {
        let now = self.now();
        let mut hub = self.lock();
        let key = delivery.key.clone();
        let current = hub
            .composers
            .get(&key)
            .is_some_and(|c| c.delivering == Some(delivery.revision));
        let end = send::End {
            ok: false,
            detail: STILL_SENDING,
            uncertain: true,
            preserve: delivery.preserve,
        };
        self.finish_in(&mut hub, &key, Some(delivery.revision), end);
        if current {
            if let Some(composer) = hub.composers.get_mut(&key) {
                composer.error = false;
                composer.notice = STILL_SENDING.into();
                composer.handed = Some(Handed::new(plan, now));
            }
        }
        drop(hub);
        self.changed();
    }

    /// Each tick: the open composer's due check, and a reloaded draft's one
    /// lookup. Nothing while the window is hidden.
    pub(crate) fn tick_handed(&self, now: f64) {
        let (deferred, compact, resume) = (self.deferred(), self.compact(), self.resume());
        if deferred.is_none() && compact.is_none() && resume.is_none() {
            return;
        }
        let job = {
            let mut hub = self.lock();
            if hub.hidden || hub.closed {
                return;
            }
            let (Some(key), Some(open)) = (hub.open_key.clone(), hub.open.clone()) else {
                return;
            };
            let Some(composer) = hub.composers.get_mut(&key) else {
                return;
            };
            if let Some(handed) = composer.handed.as_mut() {
                if handed.checking || now < handed.next {
                    return;
                }
                handed.checking = true;
                Some((key, composer.handle, handed.plan.clone(), Basis::Handed))
            } else if composer.loaded
                && composer.uncertain
                && !composer.probed
                && !composer.busy()
                && open.computer.is_empty()
                && open.subagent.is_none()
            {
                // A reload's one lookup of the held identity, when the
                // unchanged draft still rebuilds its exact original.
                composer.probed = true;
                let (text, attachments) = (&composer.draft.text, &composer.draft.attachments);
                composer
                    .checkpoint
                    .as_ref()
                    .filter(|checkpoint| {
                        send::message_digest(text, attachments) == checkpoint.message_sha256
                    })
                    .and_then(|checkpoint| match checkpoint.kind {
                        Kind::Send if deferred.is_some() => {
                            let input = stdin(&open.name, checkpoint, text, attachments)?;
                            deferred_send::plan(&open.name, checkpoint, &input)
                                .map(HandedPlan::Send)
                        }
                        Kind::CompactToSend if compact.is_some() => {
                            let (compaction, input) =
                                compact_to_send::stdins(&open.name, checkpoint, text, attachments)?;
                            compact_to_send::plan(&open.name, checkpoint, &compaction, &input)
                                .map(HandedPlan::Compact)
                        }
                        Kind::ResumeToSend
                            if resume.is_some() && checkpoint.version == SESSION_RESUME_VERSION =>
                        {
                            let (start, input) =
                                session_resume::stdins(&open.name, checkpoint, text, attachments)?;
                            session_resume::plan(&open.name, checkpoint, &start, &input)
                                .map(HandedPlan::Resume)
                        }
                        _ => None,
                    })
                    .map(|plan| (key, composer.handle, plan, Basis::Reload))
            } else {
                None
            }
        };
        let Some((key, handle, plan, basis)) = job else {
            return;
        };
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            // A check that panicked, or of a kind without its route, knows
            // nothing: never a held state 6.
            let (checked, run) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &plan {
                    HandedPlan::Send(plan) => deferred.map(|route| ((route.check)(plan), None)),
                    HandedPlan::Compact(plan) => compact.map(|route| ((route.check)(plan), None)),
                    HandedPlan::Resume(plan) => resume.map(|route| (route.check)(plan)),
                }))
                .ok()
                .flatten()
                .unwrap_or((Checked::Unavailable, None));
            me.checked(&key, handle, &plan, basis, checked, run.as_deref());
        }));
    }

    /// Applies one check's answer while the draft still holds that identity;
    /// `run`: the run a handed resume's receipt returned, which its
    /// message's row then belongs to.
    fn checked(
        &self,
        key: &str,
        handle: u64,
        plan: &HandedPlan,
        basis: Basis,
        checked: Checked,
        run: Option<&str>,
    ) {
        let now = self.now();
        let mut hub = self.lock();
        let held = hub.composers.get(key).is_some_and(|c| {
            c.handle == handle
                && !c.busy()
                && c.checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.request_id == plan.held())
                && (basis == Basis::Reload || c.handed.is_some())
        });
        if !held {
            if let Some(handed) = hub.composers.get_mut(key).and_then(|c| c.handed.as_mut()) {
                handed.checking = false;
            }
            return;
        }
        let outcome = match checked {
            Checked::InProgress => {
                if let Some(composer) = hub.composers.get_mut(key) {
                    let handed = composer
                        .handed
                        .get_or_insert_with(|| Handed::new(plan.clone(), now));
                    handed.checking = false;
                    handed.delay = if basis == Basis::Reload {
                        FIRST
                    } else {
                        (handed.delay * 2.0).min(LONGEST)
                    };
                    handed.next = now + handed.delay;
                    composer.error = false;
                    composer.notice = STILL_SENDING.into();
                }
                drop(hub);
                self.changed();
                return;
            }
            Checked::Unavailable if basis == Basis::Reload => return,
            Checked::Unavailable => Outcome::Uncertain {
                detail: deferred_send::UNCONFIRMED.into(),
            },
            Checked::Settled(SendAnswer::Recorded(outcome)) => Outcome::Done {
                receipt: serde_json::json!({"request_id": outcome.request_id}),
                cleanup: None,
            },
            Checked::Settled(SendAnswer::Unsent(detail)) => Outcome::Failed { detail },
            Checked::Settled(SendAnswer::Unconfirmed(detail)) => Outcome::Uncertain { detail },
            Checked::Settled(_) => Outcome::Uncertain {
                detail: deferred_send::UNCONFIRMED.into(),
            },
        };
        if let Some(run) = run {
            hub.local.rebind_run(key, plan.row(), run);
        }
        // The service could not tell what happened: a Terminal draft may hold
        // it (hgs refuses a send while one is there), so it is read once.
        let unknown = matches!(outcome, Outcome::Uncertain { .. });
        let detail = hub.local.finished_as_is(key, plan.row(), &outcome);
        if let Some(composer) = hub.composers.get_mut(key) {
            composer.handed = None;
            match outcome {
                Outcome::Done { .. } => {
                    composer.uncertain = false;
                    composer.error = false;
                    composer.checkpoint = None;
                    composer.notice = "Submitted to terminal".into();
                    // Only the unchanged draft was the message.
                    if send::message_digest(&composer.draft.text, &composer.draft.attachments)
                        == plan.message_sha256()
                    {
                        composer.draft = attachments::Draft::default();
                        composer.position = 0;
                        composer.anchor = 0;
                        composer.attachments_changed();
                    }
                }
                // Nothing ran: the identity goes and the draft stays.
                Outcome::Failed { .. } => {
                    composer.uncertain = false;
                    composer.error = true;
                    composer.checkpoint = None;
                    composer.notice = detail;
                }
                // Unknown for good: uncertain until Allow retry.
                Outcome::Uncertain { .. } => {
                    composer.uncertain = true;
                    composer.error = true;
                    composer.notice = detail;
                }
            }
            composer.text_epoch += 1;
            self.save(composer);
        }
        drop(hub);
        self.reconcile();
        self.changed();
        if unknown {
            self.draft_check(false);
        }
    }

    /// The compaction call of a "Compact and continue" (`request`), offered
    /// to the desktop service when it takes the whole compact-to-send, with
    /// the exact stdin each direct call would get, from the action that
    /// reserved and saved its version 2 checkpoint. False: the direct path
    /// compacts and sends it, decided before any offer. True: the hand-over
    /// ended this continuation, whatever the service answered.
    pub(crate) fn hand_compaction(&self, key: &str, revision: u64) -> bool {
        let Some(route) = self.compact() else {
            return false;
        };
        self.hand_continuation(
            key,
            revision,
            Continuation::Compaction,
            &|delivery, checkpoint| {
                compact_to_send::stdins(
                    &delivery.session.name,
                    checkpoint,
                    &delivery.text,
                    &delivery.attachments,
                )
            },
            &|delivery, checkpoint, compaction, input, hand| {
                (route.offer)(
                    &delivery.session.name,
                    checkpoint,
                    compaction,
                    input,
                    &|plan| hand(HandedPlan::Compact(plan.clone())),
                )
                .into()
            },
        )
    }

    /// The start call of a "Resume and send" (`request`), offered to the
    /// desktop service when it takes the whole resume-then-send, with the
    /// exact resume stdin and send template, from the action that reserved
    /// and saved its version 3 checkpoint. False: the direct path starts and
    /// sends it, decided before any offer (a fresh start, a resume with
    /// compaction or a version 1 checkpoint always; a Codex session for a
    /// service without the Codex readiness marker). True:
    /// the hand-over ended this continuation, whatever the service answered.
    pub(crate) fn hand_start(&self, key: &str, revision: u64) -> bool {
        let Some(route) = self.resume() else {
            return false;
        };
        self.hand_continuation(
            key,
            revision,
            Continuation::Start,
            &|delivery, checkpoint| {
                session_resume::stdins(
                    &delivery.session.name,
                    checkpoint,
                    &delivery.text,
                    &delivery.attachments,
                )
            },
            &|delivery, checkpoint, resume, input, hand| {
                (route.offer)(&delivery.session.name, checkpoint, resume, input, &|plan| {
                    hand(HandedPlan::Resume(plan.clone()))
                })
                .into()
            },
        )
    }

    /// One continuation's hand-over, either kind: its delivery and
    /// checkpoint, its two exact stdins, the files a Retry of its row needs,
    /// then the one offer. False: the direct path runs it, decided before
    /// any offer. True: the hand-over ended this continuation.
    fn hand_continuation(
        &self,
        key: &str,
        revision: u64,
        kind: Continuation,
        stdins: &StdinsFn<'_>,
        offer: &OfferFn<'_>,
    ) -> bool {
        let Some((delivery, checkpoint)) = self.continuation_delivery(key, revision, kind) else {
            return false;
        };
        let Some((first, input)) = stdins(&delivery, &checkpoint) else {
            return false;
        };
        // The files a Retry of its row needs; without them the direct path
        // fails the send as it always has.
        let Ok(files) = attachments::prepare(self.cache().root(), &delivery.attachments) else {
            return false;
        };
        let handed = std::cell::Cell::new(false);
        let offered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            offer(&delivery, &checkpoint, &first, &input, &|plan| {
                let taken = self.handing(&delivery, &files, plan, kind, &checkpoint.request_id);
                handed.set(taken);
                taken
            })
        }));
        let answer = match offered {
            Ok(answer) => answer,
            // The offer's thread died: once handed it may have reached the
            // service; before, nothing did.
            Err(_) if handed.get() => Continued::Settled(
                SendAnswer::Unconfirmed(deferred_send::UNCONFIRMED.into()),
                None,
            ),
            Err(_) => Continued::Settled(SendAnswer::Unsent(deferred_send::NOT_SENT.into()), None),
        };
        if !handed.get() {
            let text = match answer {
                Continued::Direct => return false,
                Continued::Settled(SendAnswer::Unsent(text), _) => text,
                // Nothing is offered before the hand-over.
                _ => deferred_send::NOT_SENT.into(),
            };
            // Nothing reached the service and nothing ran: the continuation
            // ends with its draft, or, if it already ended meanwhile, its
            // held identity is resolved.
            let mut hub = self.lock();
            match send::pending_mut(&mut hub, key, kind.start(), revision) {
                Some(pending) => {
                    pending.call = Call::Settled;
                    drop(hub);
                    self.cancel(key, kind.start(), revision, &text);
                }
                None => self.settle_orphan(&mut hub, &checkpoint.request_id, true),
            }
            return true;
        }
        let (answer, run) = match answer {
            Continued::Pending(plan) => {
                self.handed_over(&delivery, plan);
                self.push_finished(&delivery.request_id, false, true);
                return true;
            }
            Continued::Settled(answer, run) => (Some(answer), run),
            Continued::Direct => (None, None),
        };
        let outcome = match answer {
            Some(SendAnswer::Recorded(outcome)) => Outcome::Done {
                receipt: serde_json::json!({"request_id": outcome.request_id}),
                cleanup: None,
            },
            Some(SendAnswer::Unsent(detail)) => Outcome::Failed { detail },
            _ => Outcome::Uncertain {
                detail: deferred_send::UNCONFIRMED.into(),
            },
        };
        // The message's row belongs to the run its resume returned, never
        // the stopped one it was recorded under (zerus-5wmh.66.4.9.3).
        if let Some(run) = &run {
            self.lock().local.rebind_run(key, &delivery.request_id, run);
        }
        if let Some(composer) = self.lock().composers.get_mut(key) {
            if composer.handed.as_ref().is_some_and(Handed::locked) {
                composer.handed = None;
            }
        }
        self.delivered(&delivery, &outcome, true);
        true
    }

    /// The message a continuation waits to send, as the delivery a hand-over
    /// becomes, and the checkpoint its waiting save holds: version 2 for a
    /// compaction, version 3 of a resume for a start. None when the service
    /// cannot take it: another computer, a subagent, a checkpoint without its
    /// reserved send, or a message that is no longer the one it reserved.
    fn continuation_delivery(
        &self,
        key: &str,
        revision: u64,
        kind: Continuation,
    ) -> Option<(Delivery, Checkpoint)> {
        let hub = self.lock();
        let pending = match kind {
            Continuation::Compaction => hub.compacts.get(key).map(|(pending, _)| pending),
            Continuation::Start => hub
                .start
                .as_ref()
                .map(|(pending, _)| pending)
                .filter(|pending| pending.key == key),
        }
        .filter(|pending| pending.revision == revision)?;
        if !pending.session.computer.is_empty() || pending.session.subagent.is_some() {
            return None;
        }
        let composer = hub
            .composers
            .get(key)
            .filter(|c| c.delivering == Some(revision))?;
        let checkpoint = composer.checkpoint.clone().filter(|checkpoint| {
            let own = match kind {
                Continuation::Compaction => checkpoint.kind == Kind::CompactToSend,
                Continuation::Start => {
                    checkpoint.kind == Kind::ResumeToSend
                        && checkpoint.version == SESSION_RESUME_VERSION
                        && checkpoint.start == Some(Start::Resume)
                }
            };
            own && checkpoint.request_id == pending.request_id
                && checkpoint.send_request_id.is_some()
        })?;
        let (text, attachments) = match &pending.retry {
            Some(retry) => (retry.text.clone(), retry.attachments.clone()),
            None => (
                composer.draft.text.clone(),
                composer.draft.attachments.clone(),
            ),
        };
        if send::message_digest(&text, &attachments) != checkpoint.message_sha256 {
            return None;
        }
        // A Retry's message is not the draft: a newer draft stays.
        let preserve = pending.retry.is_some()
            && (composer.draft.text != text || composer.draft.attachments != attachments);
        let session = pending.session.clone();
        let (row, _) = (self.0.deps.rows)(&session.computer, &session.key, &session.name)?;
        let delivery = Delivery {
            key: key.to_owned(),
            session,
            created: row.created,
            row_run: row.run_id.unwrap_or_default(),
            row_conversation: row.conversation_id.unwrap_or_default(),
            run: pending.run.clone(),
            conversation: pending.conversation.clone(),
            evidence: pending.evidence.clone(),
            text,
            attachments,
            retry_id: pending.retry.as_ref().map(|retry| retry.id.clone()),
            compaction: (kind == Continuation::Compaction).then(|| checkpoint.request_id.clone()),
            consent: true,
            preserve,
            revision,
            request_id: checkpoint.send_request_id.clone()?,
        };
        Some((delivery, checkpoint))
    }

    /// Right before the offer is written: while the continuation still
    /// waits on this call (`held`, its checkpoint's request), it ends and the
    /// draft becomes the delivery the service holds. Its row shows the
    /// message sending, its status the service's line from the start, and
    /// Cancel send stays shown, locked. False: the continuation ended
    /// meanwhile, and nothing is offered.
    fn handing(
        &self,
        delivery: &Delivery,
        files: &[StoredFile],
        plan: HandedPlan,
        kind: Continuation,
        held: &str,
    ) -> bool {
        let now = self.now();
        let mut hub = self.lock();
        let key = &delivery.key;
        let live = send::pending_mut(&mut hub, key, kind.start(), delivery.revision)
            .is_some_and(|pending| pending.request_id == held)
            && hub
                .composers
                .get(key)
                .is_some_and(|c| c.delivering == Some(delivery.revision));
        if hub.closed || !live {
            return false;
        }
        match kind {
            Continuation::Compaction => {
                hub.compacts.remove(key);
            }
            Continuation::Start => hub.start = None,
        }
        hub.local.sending(
            key,
            &delivery.request_id,
            delivery.retry_id.as_deref(),
            &delivery.text,
            files,
            &delivery.run,
            &delivery.conversation,
            now,
        );
        hub.queue.pusher.dispatched(
            key,
            &delivery.text,
            &delivery.request_id,
            &delivery.run,
            &delivery.conversation,
        );
        if let Some(composer) = hub.composers.get_mut(key) {
            composer.sending = true;
            composer.waiting = None;
            composer.kept = delivery
                .preserve
                .then(|| (composer.error, composer.notice.clone()));
            composer.error = false;
            composer.notice = STILL_SENDING.into();
            // Its own call checks it until the bound; the ticks after.
            composer.handed = Some(Handed {
                checking: true,
                ..Handed::new(plan, now)
            });
            composer.text_epoch += 1;
            self.save(composer);
        }
        drop(hub);
        self.changed();
        true
    }
}

#[cfg(test)]
mod tests;
