//! The resumed session's readiness, the condition for the send
//! (zerus-5wmh.66.4.9, slice 3). After the resume drained and its own exact
//! receipt recorded the resumed run, and only within the same live
//! operation, the send may start once the selected session's own record of
//! that run shows its agent takes a message: the CLI's own record gates
//! ([`crate::state::resumed_readiness`]), read without any effect. `hgs send`
//! checks every gate again, its terminal included, under its input lock
//! before its first byte.
//!
//! A resumed Codex stays pending until its first message and shows that it
//! takes one only on its terminal (zerus-5wmh.66.4.10). For such a session
//! ([`Plan::observed`]) the record only says whether it ended; while it has
//! not, the send's own effect-free observation (`hgs send <session>
//! --observe --json`, run by the caller in its own process group from the
//! service's pinned image) decides, and the record is read again after it.
//! A missing, late, oversized or malformed reply, or one that the send
//! would not be taken now, is never ready: the wait reads again.
//!
//! `hgs session-action` records the resume once the resumed run's
//! supervisor wrote the session's record of that run, or, for a slow start,
//! while its terminal is positively alive (zerus-5sl7r); until the record
//! exists the wait reads again. A record whose supervisor exactly exited and
//! that records no end ends the wait at once: waiting longer would only hold
//! every later operation behind it. Exact exit is no process at its pid, or
//! another kernel birth there than the one pinned when its recorded start
//! matched ([`Supervisor`]); a start that differs while a process lives
//! proves nothing, as a wall clock set since moves it. Each read keeps the
//! birth an earlier read pinned. A record that cannot be read
//! ([`Readiness::Unrecorded`]) proves nothing by itself; each readable one
//! names its supervisor for the runtime's rule on a read gap.
//! The record is read from the held original root, through the same proof of
//! exactly one record as the target check, with the root checked before and
//! after; a replaced root ends the wait.
use super::receipt::Roots;
use crate::state::ResumedReadiness;
use serde_json::json;
use std::time::Instant;
use zerus_client_core::desktop::session_resume::Plan;

/// The resumed session failed, ended or left its conversation.
pub(super) const READY_ENDED: &str = "session_resume_ready_ended";
/// The resumed run is gone without recording an end: its recorded
/// supervisor exactly exited (zerus-5sl7r).
pub(super) const READY_GONE: &str = "session_resume_ready_gone";
/// The state path no longer names the held original root.
pub(super) const ROOT_CHANGED: &str = "session_resume_root_changed";
/// The read itself was lost (its worker ended without a result).
pub(super) const UNREAD: &str = "session_resume_ready_unread";
/// The largest observation reply read: the CLI's own bound on its refusal
/// (1024 characters, each at most four bytes), its framing and its newline.
pub(super) const OBSERVATION_BYTES: usize = 8 << 10;

/// What one read of the resumed session says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// The resumed run's record shows its agent takes a message (for an
    /// observed session: the send's own observation says so, and its record
    /// shows it has not ended).
    Ready,
    /// Not yet: the agent is still starting or busy. Read again.
    Waiting,
    /// No record of the resumed run could be read (none yet, a read gap or
    /// none proven): no evidence either way. Read again.
    Unrecorded,
    /// The wait ends without the send, for the named reason.
    Ended(&'static str),
}

/// The reply of `hgs send <session> --observe --json`, exactly as it prints
/// it. Its refusal is the person's text in Terminal, never read here.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Observed {
    ready: bool,
    #[serde(rename = "reason")]
    _reason: String,
}

/// A resumed run's supervisor, by the pid and start its record names and
/// the kernel birth pinned once its start matched
/// ([`crate::state::ResumedSupervisor`]).
pub(super) use crate::state::ResumedSupervisor as Supervisor;

/// What the selected session's record of a run says.
enum Record {
    /// None could be read.
    Missing,
    /// Its supervisor exactly exited, and it records no end.
    Gone,
    /// It is there, by the record rule alone, with the supervisor it names.
    Is(ResumedReadiness, Option<Supervisor>),
}

/// The selected session's record of `run` by the record rule alone (no
/// terminal), from the held root; its supervisor keeps the birth `known`
/// pinned.
fn recorded(
    roots: &Roots,
    plan: &Plan,
    run: &str,
    known: Option<&Supervisor>,
) -> Result<Record, &'static str> {
    if !roots.root_current() {
        return Err(ROOT_CHANGED);
    }
    let record = crate::desktop::original_store::session_record(
        roots,
        &plan.session,
        run,
        &plan.conversation_id,
    );
    if !roots.root_current() {
        return Err(ROOT_CHANGED);
    }
    let Some(record) = record else {
        return Ok(Record::Missing);
    };
    let mut supervisor = Supervisor::of(&record).map(|supervisor| supervisor.keeping(known));
    Ok(match crate::state::resumed_readiness(&record) {
        ResumedReadiness::Ended => Record::Is(ResumedReadiness::Ended, supervisor),
        _ if supervisor.as_mut().is_some_and(Supervisor::exited) => Record::Gone,
        readiness => Record::Is(readiness, supervisor),
    })
}

/// Reads the resumed run from the held root. For an observed session whose
/// record shows it has not ended, `observe` runs the send's own observation
/// with exactly its arguments and stdin and returns its reply within
/// `until`, or nothing; it is never called for any other session.
pub(super) fn read(
    roots: &Roots,
    plan: &Plan,
    run: &str,
    until: Instant,
    observe: impl FnOnce(&[String], &[u8], Instant) -> Option<Vec<u8>>,
) -> Readiness {
    read_supervised(roots, plan, run, until, None, observe).0
}

/// [`read`], with the supervisor the record read last names; it keeps the
/// birth `known` pinned.
pub(super) fn read_supervised(
    roots: &Roots,
    plan: &Plan,
    run: &str,
    until: Instant,
    known: Option<&Supervisor>,
    observe: impl FnOnce(&[String], &[u8], Instant) -> Option<Vec<u8>>,
) -> (Readiness, Option<Supervisor>) {
    match recorded(roots, plan, run, known) {
        Err(code) => (Readiness::Ended(code), None),
        Ok(Record::Missing) => (Readiness::Unrecorded, None),
        Ok(Record::Gone) => (Readiness::Ended(READY_GONE), None),
        Ok(Record::Is(ResumedReadiness::Ended, _)) => (Readiness::Ended(READY_ENDED), None),
        Ok(Record::Is(_, supervisor)) if plan.observed() => observed(
            roots,
            plan,
            run,
            until,
            supervisor.as_ref().or(known),
            observe,
        ),
        Ok(Record::Is(ResumedReadiness::Ready, supervisor)) => (Readiness::Ready, supervisor),
        Ok(Record::Is(ResumedReadiness::Starting, supervisor)) => (Readiness::Waiting, supervisor),
    }
}

/// The send's own observation of an observed session, then its record
/// again: an ended session or a replaced root ends the wait whatever the
/// reply said.
fn observed(
    roots: &Roots,
    plan: &Plan,
    run: &str,
    until: Instant,
    known: Option<&Supervisor>,
    observe: impl FnOnce(&[String], &[u8], Instant) -> Option<Vec<u8>>,
) -> (Readiness, Option<Supervisor>) {
    let input = json!({"expected_run_id": run, "expected_conversation_id": plan.conversation_id})
        .to_string();
    let reply = observe(&plan.observe_args(), input.as_bytes(), until);
    let ready = Instant::now() <= until
        && reply.is_some_and(|bytes| {
            bytes.len() <= OBSERVATION_BYTES
                && serde_json::from_slice::<Observed>(&bytes).is_ok_and(|reply| reply.ready)
        });
    match recorded(roots, plan, run, known) {
        Err(code) => (Readiness::Ended(code), None),
        Ok(Record::Gone) => (Readiness::Ended(READY_GONE), None),
        Ok(Record::Is(ResumedReadiness::Ended, _)) => (Readiness::Ended(READY_ENDED), None),
        Ok(Record::Is(_, supervisor)) if ready => (Readiness::Ready, supervisor),
        Ok(Record::Missing) => (Readiness::Unrecorded, None),
        Ok(Record::Is(_, supervisor)) => (Readiness::Waiting, supervisor),
    }
}

/// The spawn turn's last check of the send's session, from its record
/// alone: it shows the agent takes a message, or, for an observed session,
/// that it has not ended (its observation ran just before the spawn turn).
pub(super) fn current(roots: &Roots, plan: &Plan, run: &str) -> bool {
    match recorded(roots, plan, run, None) {
        Ok(Record::Is(ResumedReadiness::Ready, _)) => true,
        Ok(Record::Is(ResumedReadiness::Starting, _)) => plan.observed(),
        _ => false,
    }
}
