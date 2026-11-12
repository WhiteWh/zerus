//! The open session's part of the composer, ported from `SessionsWindow`:
//! the block reason (`messageBlockReason`, S1), the cache chip and its
//! details (S2), the session status Qt shows in the composer
//! (`setSessionStatus`), and the inspect when no Activity state arrived.
//!
//! The delivery (`sendMessage`, `deliverMessage`, `finishMessage`): the
//! guarded send of the held draft, the cold cache question before it, and
//! the local message rows (S4) that show what was sent until the agent's
//! own history does. Unknown delivery never authorizes replay:
//! - the durable "sending" save comes before any call, and only its answer
//!   can start the call; a closed hub (the app quits) starts nothing, and a
//!   draft saved as sending reloads as uncertain;
//! - one request id per delivery, made right before its one call: the local
//!   row, the CLI request and the receipt check carry it;
//! - an uncertain outcome (or a call whose thread died) never sends again
//!   by itself: the draft stays, marked uncertain, until Allow retry, and its
//!   row offers no Retry;
//! - every path that could send re-checks the composer: Send (with the text
//!   epoch its text is based on), a question's answer, Retry, and the last
//!   gate before the save (`deliver`).
//!
//! The continuations (`startToSend`, `compactToSend`, S3): a stopped
//! session asks how to start, then sends the exact draft once the new run
//! is ready; a cold cache can compact first and send after the compaction
//! completes. They keep the delivery's rules:
//! - the waiting draft is saved durably before the start or compaction is
//!   asked for, and only that save's answer can ask; a closed hub asks for
//!   nothing;
//! - every call has its own request id; an `Err` or a panic is an uncertain
//!   outcome, which ends the continuation: its message is never sent;
//! - every step, outcome and cancel names the continuation's revision (the
//!   revision of its waiting save), so a late one never ends a newer
//!   continuation or the delivery that followed it;
//! - the send at the end is an ordinary delivery: its durable "sending"
//!   save, its one request id and `still_valid` all apply.
//!
//! Clear context (`clearContext`): the identity the dialog showed must still
//! hold, and one reset runs at a time across all sessions.
//!
//! The ticker recomputes the open composer's session part once a second, as
//! Qt's cache chip refreshes, and sleeps while none is open and no
//! continuation waits. It wakes the pump only when that part changed: the
//! chip's minute, or a row or status no batch announced. A write mark wakes
//! it at once (`Actions::on_writes`). It also ends a continuation past its
//! limit and polls the continuations every 2.5 s (`pollCompactContinuations`).

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use zerus_client_core::attachments::{self, Attachment, StoredFile};
use zerus_client_core::cache::{self, Chip, ColdAction, ColdPrompt};
use zerus_client_core::local_messages::{self, Inspect, LocalMessage, LocalMessages, Status};
use zerus_client_core::operation_recovery::deferred_input::{
    message_sha256, Checkpoint, Intent, Kind, Start,
};
use zerus_client_core::send::{
    self as rules, block_reason, BlockFacts, CompactSend, DraftMark, Session, StartChoice,
    StartQuestion, StartSend, Step,
};
use zerus_client_core::session_usage;
use zerus_client_core::terminal::{self, TerminalFacts};
use zerus_client_core::{Action, Exit, Outcome, Target, TooLarge, ViewRow};

use super::{Composer, Composers, Hub, SessionRef, SubagentView, EVENTS};
use crate::actions::WriteKind;

/// No Activity state this long after opening: inspect.
const DETAILS_WAIT: f64 = 3.0;
/// A failed inspect is tried again this often while nothing streams (Qt's
/// `m_activityPollMs`, `SessionsWindow.h:565`; its timer, 2013-2016).
const RETRY_WAIT: f64 = 2.5;
/// The ticker's period: Qt's cache chip refreshes once a second.
const TICK: Duration = Duration::from_secs(1);
/// How often a waiting continuation reads its session (Qt's `m_compactTimer`,
/// `SessionsWindow.cpp:979`).
const POLL: f64 = 2.5;
/// The composer's availability while Send can start a stopped session
/// (`SessionsWindow.cpp:3987-3988`).
const STOPPED: &str = "Session stopped. Send asks whether to resume it or start fresh.";

/// The session's state for the composer view.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SessionView {
    pub(crate) subagent: Option<SubagentView>,
    pub(crate) available: bool,
    pub(crate) unavailable_reason: String,
    pub(crate) block_reason: Option<String>,
    pub(crate) attachments_available: bool,
    pub(crate) attachments_reason: String,
    pub(crate) session_status: String,
    pub(crate) session_status_error: bool,
    pub(crate) session_blocks_input: bool,
    pub(crate) inspect_error: bool,
    pub(crate) cache: Chip,
    pub(crate) cache_warning: String,
    pub(crate) cache_details: String,
    pub(crate) context: session_usage::ContextChip,
    pub(crate) clear: ClearControl,
}

/// What Qt's session hint reads (`SessionsWindow.cpp:3913-3922, 3955-3958`).
/// `question_error` is `pending_questions_error` (A2-1); Piece E fills
/// `restore_error` (`last_restore_error`) and `occupied` (another session
/// uses this archived session's name).
#[derive(Default)]
pub(crate) struct HintFacts<'a> {
    pub(crate) terminating: bool,
    pub(crate) online: bool,
    pub(crate) archived: bool,
    /// `m_inspectError`, its whole text.
    pub(crate) inspect_error: Option<&'a str>,
    pub(crate) question_error: &'a str,
    pub(crate) restore_error: &'a str,
    pub(crate) occupied: bool,
}

/// The composer's session status (`setSessionStatus(contextVisible ? hint
/// : "", contextError, contextError && !messageProblem.isEmpty())`,
/// `SessionsWindow.cpp:3984-3986`); `blocked`: a block reason stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SessionStatus {
    pub(crate) text: String,
    pub(crate) error: bool,
    pub(crate) blocks_input: bool,
}

pub(crate) fn session_status(facts: &HintFacts<'_>, blocked: bool) -> SessionStatus {
    let restore_failed = facts.archived && !facts.restore_error.is_empty();
    let error = !facts.online
        || facts.inspect_error.is_some()
        || !facts.question_error.is_empty()
        || restore_failed;
    if !error && !(facts.archived && facts.occupied) {
        return SessionStatus::default();
    }
    // The first six links of the hint chain: one of them holds whenever the
    // status shows.
    let text = if facts.terminating {
        "Waiting for the session to stop. Its native conversation history is kept.".to_owned()
    } else if !facts.online {
        "Machine unavailable. Showing the last known state; actions will return when it reconnects."
            .to_owned()
    } else if restore_failed {
        format!("Last restore failed: {}", facts.restore_error)
    } else if let Some(error) = facts.inspect_error {
        error.to_owned()
    } else if !facts.question_error.is_empty() {
        format!(
            "Could not load agent questions: {}. Check Terminal.",
            facts.question_error
        )
    } else {
        "Another active or saved session uses this name. Restore becomes available when that name is free."
            .to_owned()
    };
    SessionStatus {
        text,
        error,
        blocks_input: error && blocked,
    }
}

/// An inspect's answer as Qt reads it (`HgsClient::requestInspection` and
/// `failureDetail`, `HgsClient.cpp:466-479, 931-944`): the JSON object, or
/// the detail of "Could not refresh activity: %1". `run_limited`'s cap
/// stands for Qt's unparseable reply.
pub(crate) fn inspection(hgs: &Path, read: Result<Exit, TooLarge>) -> Result<Value, String> {
    const INVALID: &str = "Invalid activity response";
    let stderr = |text: &str, fallback: String| {
        let text: String = text.trim().chars().take(4000).collect();
        if text.is_empty() {
            fallback
        } else {
            text
        }
    };
    match read {
        Err(_) => Err(INVALID.into()),
        Ok(Exit::Code {
            code: 0, stdout, ..
        }) => match serde_json::from_slice::<Value>(&stdout) {
            Ok(value) if value.is_object() => Ok(value),
            _ => Err(INVALID.into()),
        },
        Ok(Exit::Code {
            code, stderr: text, ..
        }) => Err(stderr(&text, format!("exit code {code}"))),
        Ok(Exit::Abnormal { stderr: text }) => Err(stderr(&text, "hgs ended abnormally".into())),
        Ok(Exit::TimedOut) => Err("timed out".into()),
        Ok(Exit::StartFailed(error)) => Err(format!("cannot start '{}': {error}", hgs.display())),
        Ok(Exit::WriteFailed) => Err("could not send the request".into()),
    }
}

/// The agent command the send rules read: the row says `deepseek` for `dsh`.
pub(crate) fn cmd(row: &ViewRow) -> String {
    if row.agent == "deepseek" {
        "dsh".into()
    } else {
        row.agent.clone()
    }
}

/// Local time as the cache details show it (Qt's `d MMM HH:mm:ss`, English
/// month names); formatted in Rust, never in the webview.
fn local_time(seconds: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(seconds, 0)
        .single()
        .map(|time| time.format("%-d %b %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// The cache expiry of `details` in local time, as `cache::details` names
/// it: the seconds and their text. Callers format it before they take the
/// hub lock, since local time may read the time zone files.
pub(crate) fn expiry(details: &Value) -> Option<(i64, String)> {
    let at = details["session_usage"]["prompt_cache"]["expires_at"]
        .as_f64()
        .filter(|at| *at > 0.0)? as i64;
    Some((at, local_time(at)))
}

/// The row's run, conversation and state an inspect read: a change of any
/// makes the inspected details stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Basis {
    run_id: Option<String>,
    conversation_id: Option<String>,
    state: String,
}

impl Basis {
    pub(crate) fn of(row: &ViewRow) -> Self {
        Basis {
            run_id: row.run_id.clone(),
            conversation_id: row.conversation_id.clone(),
            state: row.state.clone(),
        }
    }
}

/// The open session's row, online state, details and facts.
pub(crate) struct Context {
    pub(crate) session: SessionRef,
    pub(crate) key: String,
    pub(crate) row: ViewRow,
    pub(crate) online: bool,
    pub(crate) details: Option<Arc<Value>>,
    pub(crate) facts: BlockFacts,
}

fn native_run<'a>(row: &'a ViewRow, details: Option<&'a Value>) -> &'a str {
    row.run_id
        .as_deref()
        .filter(|run| !run.is_empty())
        .or_else(|| {
            details
                .map(|details| text(details, "run_id"))
                .filter(|run| !run.is_empty())
        })
        .unwrap_or("")
}

impl Context {
    /// Only the open child's own inspection, never its parent's state or another child's.
    pub(crate) fn child_details(&self) -> Option<&Value> {
        let child = self
            .session
            .subagent
            .as_ref()
            .filter(|child| child.valid())?;
        let details = self.details.as_deref()?;
        (text(details, "agent_id") == child.agent_id
            && text(details, "parent_conversation_id") == child.parent_conversation_id)
            .then_some(details)
    }

    pub(crate) fn reason(&self) -> Option<String> {
        if let Some(reason) = block_reason(&self.facts, self.details.as_deref()) {
            return Some(reason);
        }
        self.session.subagent.as_ref()?;
        let details = self.child_details();
        if details.is_some_and(|details| flag(details, "send_supported")) {
            None
        } else if details.is_some() {
            Some("Read only. This agent is managed by the parent conversation.".into())
        } else {
            Some("Loading subagent activity…".into())
        }
    }

    /// Qt's `recorded`: session data that is not live (`SessionsWindow.cpp:3783`).
    pub(crate) fn recorded(&self) -> bool {
        !self.online
            || matches!(self.row.state.as_str(), "archived" | "paused")
            || self.row.process_state == "exited"
            || self.facts.inspect_error
    }
}

impl Composers {
    pub(crate) fn now(&self) -> f64 {
        (self.0.deps.clock)()
    }

    /// The open session as the catalog and the followed Activity show it now.
    pub(crate) fn context(&self, hub: &Hub) -> Option<Context> {
        let session = hub.open.clone()?;
        let key = hub.open_key.clone()?;
        let (mut row, online) = (self.0.deps.rows)(&session.computer, &session.key, &session.name)?;
        let archived = session.archived();
        if archived {
            // Never borrow a current run's identity, abilities or status for
            // a recorded draft (SessionsWindow.cpp:3454,4393).
            row = ViewRow {
                name: session.name.clone(),
                state: "archived".into(),
                archive_id: session.archive_id.clone(),
                ..ViewRow::default()
            };
        }
        let followed = !archived
            && hub.followed.computer == session.computer
            && hub.followed.key == session.key;
        let details = if followed {
            hub.followed.details.clone()
        } else {
            None
        };
        let write = if archived {
            None
        } else {
            (self.0.deps.facts)(
                &session.computer,
                &session.name,
                native_run(&row, details.as_deref()),
            )
        };
        let facts = BlockFacts {
            // Qt's `m_startSend.key`, `m_compacts` and `m_clearKey`
            // (`messageBlockReason`, 4388-4390): this session's own.
            start_send_waiting: hub.start.as_ref().is_some_and(|(p, _)| p.key == key),
            compacting: hub.compacts.contains_key(&key),
            clearing: write == Some(WriteKind::Clear) || hub.clearing.as_ref() == Some(&key),
            terminating: write == Some(WriteKind::Terminate),
            settings_changing: write == Some(WriteKind::Settings),
            online,
            state: row.state.clone(),
            updating: matches!(write, Some(WriteKind::Update | WriteKind::Settings)),
            cmd: cmd(&row),
            inspect_error: followed && hub.followed.inspect_error.is_some(),
        };
        Some(Context {
            session,
            key,
            row,
            online,
            details,
            facts,
        })
    }

    /// One `inspect` of the open session, off the UI thread; its value
    /// stands in for the stream's state until the next state frame. As Qt's
    /// `inspect()` (`SessionsWindow.cpp:3467-3492`), it reads nothing while
    /// the stream keeps the details or the computer is offline, and one
    /// inspect runs at a time. The tick calls it for the fallback and its
    /// retry; Tasks 4b and later call it after their actions, and Task 6's
    /// Refresh, where Qt calls `inspect()`.
    pub(crate) fn refresh_details(&self) {
        self.read_details(false);
    }

    fn read_details(&self, waiting_send: bool) {
        let mut hub = self.lock();
        if hub.closed
            || hub.followed.inspecting.is_some()
            || (hub.followed.streamed && !waiting_send)
        {
            return;
        }
        let Some(ctx) = self.context(&hub) else {
            return;
        };
        let followed =
            hub.followed.computer == ctx.session.computer && hub.followed.key == ctx.session.key;
        if !followed || !ctx.online || ctx.session.archived() {
            return;
        }
        hub.next_inspect += 1;
        let number = hub.next_inspect;
        hub.followed.inspecting = Some(number);
        hub.followed.inspected = Some(Basis::of(&ctx.row));
        hub.followed.inspected_at = self.now();
        let observed = hub.followed.details.clone();
        drop(hub);
        let target = Target {
            computer: ctx.session.computer.clone(),
            name: ctx.session.name.clone(),
            run_id: ctx.row.run_id.clone().unwrap_or_default(),
            conversation_id: ctx.row.conversation_id.clone().unwrap_or_default(),
        };
        let parent_run = target.run_id.clone();
        let parent = target.conversation_id.clone();
        let agent = ctx
            .session
            .subagent
            .as_ref()
            .map(|child| child.agent_id.clone());
        let archive = ctx.session.archive_id;
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || {
            let read = (me.0.deps.inspect)(&target, archive.as_deref(), agent.as_deref()).and_then(
                |mut value| {
                    if agent.as_ref().is_some_and(|agent| {
                        text(&value, "run_id") != parent_run
                            || text(&value, "agent_id") != *agent
                            || text(&value, "parent_conversation_id") != parent
                    }) {
                        return Err("The subagent changed or is unavailable".into());
                    }
                    // The details keep no events: they join the followed window
                    // apart, as Qt's inspection reply joins `m_events`.
                    let replace = value["replace_events"].as_bool() == Some(true);
                    let events = value
                        .as_object_mut()
                        .and_then(|fields| fields.remove("events"));
                    let expiry = expiry(&value);
                    Ok((Arc::new(value), expiry, (replace, events)))
                },
            );
            let mut hub = me.lock();
            // Another session was opened, or the follow ended, meanwhile:
            // this answer is not the current one.
            if hub.followed.inspecting != Some(number) {
                return;
            }
            hub.followed.inspecting = None;
            // A waiting send needs its own readiness/compaction poll even
            // when a previous stream frame said "starting". A frame received
            // during that read still wins over its response.
            let mut accepted_details = false;
            let unchanged = match (&observed, &hub.followed.details) {
                (Some(before), Some(now)) => Arc::ptr_eq(before, now),
                (None, None) => true,
                _ => false,
            };
            if !hub.followed.streamed || (waiting_send && unchanged) {
                match read {
                    Ok((details, expiry, events)) => {
                        accepted_details = true;
                        hub.followed.inspect_error = None;
                        hub.followed.details = Some(details);
                        hub.followed.expiry = expiry;
                        let (replace, events) = events;
                        merge_events(&mut hub.followed.events, replace, events);
                    }
                    Err(detail) => {
                        hub.followed.inspect_error =
                            Some(format!("Could not refresh activity: {detail}"))
                    }
                }
            }
            drop(hub);
            me.after_observe(accepted_details);
        }));
    }

    /// Refresh in the composer's status line: one read of the open session
    /// while `handle` is still its composer (Qt's `sessionRefreshRequested`,
    /// `SessionsWindow.cpp:1031-1034`, which refreshes the listing and inspects).
    /// Offline, wake only this computer's existing source outside its back-off.
    pub fn refresh(&self, handle: u64) {
        let reconnect = {
            let hub = self.lock();
            if hub.closed
                || !hub
                    .open_key
                    .as_ref()
                    .and_then(|key| hub.composers.get(key))
                    .is_some_and(|composer| composer.handle == handle)
            {
                return;
            }
            self.context(&hub)
                .filter(|ctx| !ctx.online)
                .map(|ctx| ctx.session.computer)
        };
        if let Some(computer) = reconnect {
            // Source callbacks run without the hub lock and keep the captured
            // computer even if another session opens meanwhile.
            (self.0.deps.reconnect)(&computer);
        } else {
            self.refresh_details();
        }
    }

    /// The detail tab's retry interval (`poll_seconds`); a value that is not
    /// a positive number is Activity's.
    pub fn set_poll(&self, seconds: Option<f64>) {
        self.lock().poll = seconds.filter(|s| s.is_finite() && *s > 0.0);
    }

    /// Whether the tick inspects. Never read yet: once no Activity state
    /// arrived within 3 s of opening, and at once after the follow ended.
    /// Read: a failure again every 2.5 s (the tab's `poll` while another
    /// detail tab shows), and any read again when the row's
    /// run, conversation or state changed.
    fn wants_details(&self, hub: &Hub, now: f64) -> bool {
        let followed = &hub.followed;
        if hub.open.as_ref().is_none_or(SessionRef::archived)
            || followed.inspecting.is_some()
            || followed.streamed
        {
            return false;
        }
        match &followed.inspected {
            None => followed.details.is_some() || now - followed.opened_at >= DETAILS_WAIT,
            Some(basis) => {
                (followed.inspect_error.is_some()
                    && now - followed.inspected_at >= hub.poll.unwrap_or(RETRY_WAIT))
                    || self
                        .context(hub)
                        .is_some_and(|ctx| Basis::of(&ctx.row) != *basis)
            }
        }
    }

    /// Once a second while a composer is open or a continuation waits: the
    /// session part of its view (the cache chip's minute, and a write, row or
    /// status no batch announced), the inspect fallback, and the
    /// continuations' limits and polls.
    pub fn tick(&self) {
        let now = self.now();
        let mut hub = self.lock();
        let seen = hub
            .open_key
            .as_ref()
            .and_then(|key| hub.composers.get(key))
            .map(|composer| session_view(self, &hub, composer));
        let changed = hub.seen != seen;
        hub.seen = seen;
        let wants = self.wants_details(&hub, now);
        drop(hub);
        if wants {
            self.refresh_details();
        }
        if changed {
            self.changed();
        }
        self.tick_continuations(now);
        self.retry_answer_saves();
        self.push_check();
        self.tick_handed(now);
        self.tick_handed_answers(now);
    }

    /// Ticks once a second while a composer is open, until the app quits.
    pub fn start_ticker(&self) {
        self.start_ticker_every(TICK);
    }

    fn start_ticker_every(&self, every: Duration) -> JoinHandle<()> {
        let me = self.clone();
        std::thread::Builder::new()
            .name("zerus-composer".into())
            .spawn(move || {
                while me.wait_open() {
                    std::thread::sleep(every);
                    me.tick();
                }
            })
            .expect("the composer ticker starts")
    }

    /// Sleeps while no composer is open and no continuation waits, so a
    /// closed composer never wakes the app; false once the app quits. A
    /// continuation's poll belongs to its accepted send, whatever is open
    /// (`SessionsWindow.cpp:4586-4587`).
    fn wait_open(&self) -> bool {
        let hub = self
            .0
            .opened
            .wait_while(self.lock(), |hub| {
                let waiting = hub.open.is_none()
                    && hub.start.is_none()
                    && hub.compacts.is_empty()
                    && !hub.closed;
                #[cfg(test)]
                self.0
                    .parked
                    .store(waiting, std::sync::atomic::Ordering::SeqCst);
                waiting
            })
            .unwrap_or_else(PoisonError::into_inner);
        !hub.closed
    }
}

/// The open session's part of the composer view (`renderDetails`,
/// `SessionsWindow.cpp:3783-3791, 3982-3989`).
pub(crate) fn session_view(composers: &Composers, hub: &Hub, composer: &Composer) -> SessionView {
    let Some(ctx) = composers.context(hub).filter(|ctx| ctx.key == composer.key) else {
        return SessionView {
            unavailable_reason: "Select a session.".into(),
            attachments_available: true,
            ..SessionView::default()
        };
    };
    let details = ctx.details.as_deref().unwrap_or(&Value::Null);
    let now = composers.now();
    let reason = ctx.reason();
    let subagent = ctx.session.subagent.as_ref().map(|child| {
        let inspected = ctx.child_details();
        let send_supported = inspected.is_some_and(|details| flag(details, "send_supported"));
        let hint = inspected.map_or_else(
            || "Loading subagent activity…".into(),
            |details| {
                format!(
                    "{}{}{}",
                    text(details, "history_scope"),
                    if flag(details, "history_truncated") {
                        ". Showing recent activity."
                    } else {
                        ""
                    },
                    if send_supported {
                        " Messages go directly to this agent."
                    } else {
                        " Read only. This agent is managed by the parent conversation."
                    }
                )
            },
        );
        SubagentView {
            agent_id: child.agent_id.clone(),
            send_supported,
            hint,
        }
    });
    // A stopped main session that Send can start stays available; a child never starts its parent.
    let can_start = subagent.is_none()
        && reason.is_some()
        && rules::can_start_stopped(
            &Composers::rules_session(&ctx),
            Composers::start_busy(hub, &ctx),
        );
    let (available, unavailable_reason) = match &reason {
        Some(_) if can_start => (true, STOPPED.to_owned()),
        Some(reason) => (false, reason.clone()),
        None => (true, String::new()),
    };
    let status = session_status(
        &HintFacts {
            terminating: ctx.facts.terminating,
            online: ctx.online,
            archived: ctx.row.state == "archived",
            inspect_error: ctx
                .facts
                .inspect_error
                .then_some(hub.followed.inspect_error.as_deref())
                .flatten(),
            // Qt shows `pending_questions_error` only here (3913-3918, 3984-3986).
            question_error: details
                .get("pending_questions_error")
                .and_then(Value::as_str)
                .unwrap_or(""),
            ..HintFacts::default()
        },
        reason.is_some(),
    );
    let attachments_available = ctx.facts.cmd != "opencode"
        && details
            .get("attachments_supported")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    let attachments_reason = details
        .get("attachments_reason")
        .and_then(Value::as_str)
        .unwrap_or(
            "This native adapter does not support file attachments. Your existing files remain saved; remove them to send text.",
        )
        .to_owned();
    let recorded = ctx.recorded();
    // The expiry was formatted when the details came; local time may read
    // files, so it is never formatted under the hub lock.
    let expiry = hub.followed.expiry.as_ref();
    let local = |seconds: i64| match expiry {
        Some((at, text)) if *at == seconds => text.clone(),
        _ => local_time(seconds),
    };
    SessionView {
        subagent,
        available,
        unavailable_reason,
        clear: clear_control(hub, &ctx, details),
        block_reason: reason,
        attachments_available,
        attachments_reason,
        session_status: status.text,
        session_status_error: status.error,
        session_blocks_input: status.blocks_input,
        inspect_error: ctx.facts.inspect_error,
        cache: cache::chip(details, now),
        cache_warning: cache::warning(details, now),
        cache_details: cache::details(details, recorded, local),
        // Qt's `m_contextUsage->setData(m_details["session_usage"], recorded)` (3788).
        context: session_usage::context(&details["session_usage"], recorded),
    }
}

/// The retry interval of a failed read while `tab` shows (Qt's poll timer,
/// `SessionsWindow.cpp:2013-2016`): Settings > Processes' background seconds
/// behind Terminal or Native UI, its active seconds behind Processes, and
/// Activity's 2.5 s (None) otherwise.
pub(crate) fn poll_seconds(tab: &str, number: impl Fn(&str) -> f64) -> Option<f64> {
    match tab {
        "terminal" | "native" => Some(number("processes/backgroundSeconds")),
        "processes" => Some(number("processes/activeSeconds")),
        _ => None,
    }
}

/// An inspection's events joined to the followed window as Qt joins them to
/// `m_events` (`SessionsWindow.cpp:3639-3651`): the window replaced when the
/// reply says so, else one event per `seq`; the last 500 stay.
fn merge_events(kept: &mut VecDeque<Value>, replace: bool, events: Option<Value>) {
    let Some(Value::Array(events)) = events else {
        return;
    };
    if replace {
        kept.clear();
    }
    let known: HashSet<i64> = kept
        .iter()
        .filter_map(|event| event["seq"].as_i64())
        .collect();
    let skip = events.len().saturating_sub(EVENTS);
    for event in events.into_iter().skip(skip) {
        if event["seq"]
            .as_i64()
            .is_some_and(|seq| known.contains(&seq))
        {
            continue;
        }
        kept.push_back(event);
    }
    while kept.len() > EVENTS {
        kept.pop_front();
    }
}

/// `deliveryFinished(false, …)` when the session changed before the call.
pub(crate) const CHANGED: &str = "The session changed. Your draft was kept without sending.";
/// A question's draft changed before its answer (`SessionsWindow.cpp:4845`).
const DRAFT_CHANGED: &str = "Draft changed. Send it when you are ready.";
/// A failed row names files that cannot be read back (`read_stored`'s text).
const UNAVAILABLE: &str =
    "The saved attachment is unavailable or changed. Attach it again before sending.";
/// Only the latest questions can be answered.
const PROMPTS: u64 = 8;

/// What `send_message`, `send_answer` and `local_retry` answer.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendReply {
    /// The delivery started; its end arrives with the composer view.
    Sending,
    /// Nothing happened: another send or question runs, the draft is not
    /// the one Send was pressed on, or the answer is stale. A refused Send
    /// hands its text back under a new epoch.
    Busy,
    /// The composer shows `reason` (`deliveryFinished(false, reason)`).
    Blocked { reason: String },
    /// A window notice (`showNotice(text, error)`): `error` is Qt's error
    /// tone, for a block reason or a read failure; a plain notice otherwise.
    Notice { text: String, error: bool },
    /// Ask "Continue with a cold cache?"; answer with `token`.
    Cold { token: u64, prompt: ColdPrompt },
    /// Ask "Start the session to send?"; answer with `token`.
    Start {
        token: u64,
        question: Box<StartQuestion>,
    },
    /// The cold cache's Clear context: open the Clear session dialog with
    /// the view's `clear` (`sendMessage` 4843).
    ClearContext { token: u64 },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Answer {
    Cold { action: ColdAction },
    Start { choice: StartChoice },
    Cancel,
}

/// Clear session and the cache popover's Clear context, with the dialog
/// the confirmation shows (`clearContext`, `renderDetails` 3790-3804).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClearControl {
    /// Clear session (the header and the details, Qt's `canClear`, 3798).
    pub enabled: bool,
    /// The Claude chooser variant: Esc first, then the clear.
    pub dismiss: bool,
    pub title: String,
    pub text: String,
    pub confirm: String,
    /// The identity the dialog shows; the confirmation sends it back.
    pub run: String,
    pub conversation: String,
    /// The cache popover's Clear context (3792): native clear support, no
    /// chooser.
    pub cache_enabled: bool,
    /// A context reset runs (Qt's "Clearing…" label, 3804).
    pub running: bool,
    /// Why the host cannot clear now, as a sentence for the tooltip
    /// (`explainContextAction`, 3795-3803): `reason` for Clear session,
    /// `cache_reason` for the popover; empty for the usual tooltip.
    pub reason: String,
    pub cache_reason: String,
}

/// A Retry between its checks and the read of its cached files.
pub(crate) struct RetryBegun {
    handle: u64,
    id: String,
    ctx: Context,
    row: LocalMessage,
    files: Vec<StoredFile>,
}

/// A failed row's message, sent again by Retry.
#[derive(Clone, Debug)]
pub(crate) struct Retry {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
}

/// A question waiting for its answer, with the identity it was asked about
/// and the message it asked to send. Qt's question is modal, so it sends
/// what Send held; here the draft must still hold it when the answer comes.
#[derive(Clone, Debug)]
pub(crate) struct Prompt {
    pub(crate) key: String,
    pub(crate) handle: u64,
    pub(crate) session: SessionRef,
    pub(crate) run: String,
    pub(crate) conversation: String,
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) retry: Option<Retry>,
    /// The start question's session as the catalog showed it (S3's
    /// `Session`); None for the cold cache question.
    pub(crate) rules: Option<Session>,
    /// The start question's draft mark.
    pub(crate) mark: DraftMark,
}

/// A continuation's own part: the session it waits for, the identity its
/// call names and the message it sends at the end.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub(crate) key: String,
    pub(crate) session: SessionRef,
    /// The run and conversation its call names: the stopped run for a start
    /// (Qt's `resumeSession` and `startFreshSession` with `target`, 4759-4760),
    /// the compacted conversation for a compaction (`requestCompactContext`,
    /// 4561).
    pub(crate) run: String,
    pub(crate) conversation: String,
    /// The failed row a compaction sends again; its message, not the draft.
    pub(crate) retry: Option<Retry>,
    /// The revision of its waiting save, which `Composer.delivering` holds
    /// while it waits.
    pub(crate) revision: u64,
    /// The details its session last showed (Qt's pending send evidence).
    pub(crate) evidence: Option<Arc<Value>>,
    /// Its poll's inspect runs: one at a time.
    pub(crate) polling: bool,
    /// The request id of its start or compaction call, reserved with the
    /// checkpoint of its waiting save (zerus-5wmh.66.4.4); set by `wait`.
    pub(crate) request_id: String,
    /// Where that call is: an end while it runs, or after an uncertain
    /// outcome, leaves the draft holding its identity.
    pub(crate) call: Call,
}

#[cfg(test)]
std::thread_local! {
    /// Runs once, on this thread, right after `reconcile` leaves the hub lock:
    /// a test's barrier in the window where another call can run.
    static AFTER_RECONCILE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// How a delivery or continuation ends ([`Composers::finish`]).
pub(super) struct End<'a> {
    pub(super) ok: bool,
    pub(super) detail: &'a str,
    pub(super) uncertain: bool,
    pub(super) preserve: bool,
}

/// A continuation's start or compaction call (zerus-5wmh.66.4.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Call {
    /// Not asked for: an end resolves its identity, nothing ran.
    #[default]
    NotCalled,
    /// Asked for and not answered yet: physically in flight.
    Running,
    /// Answered uncertainly: it may have run, and no later answer comes.
    Unresolved,
    /// A definite outcome: the call ran, or was refused before it ran.
    Settled,
}

/// The most continuation calls tracked as ended while in flight. Past it the
/// oldest goes: its late answer then resolves nothing, and its draft keeps
/// the identity until the user allows a retry (zerus-5wmh.66.4.4).
pub(crate) const ORPHANS: usize = 32;

/// A start or compaction call whose continuation ended while it was still
/// in flight: its late answer decides the identity its draft holds.
#[derive(Clone, Debug)]
pub(crate) struct Orphan {
    pub(crate) request: String,
    pub(crate) key: String,
    /// The notice its end showed, shown again if a definite answer resolves it.
    pub(crate) notice: String,
}

/// Tracks `orphan`, the oldest going past [`ORPHANS`].
pub(crate) fn track_orphan(orphans: &mut VecDeque<Orphan>, orphan: Orphan) {
    while orphans.len() >= ORPHANS {
        orphans.pop_front();
    }
    orphans.push_back(orphan);
}

/// One delivery: what is sent, to which run, with which consent.
#[derive(Clone, Debug)]
pub(crate) struct Delivery {
    pub(crate) key: String,
    pub(crate) session: SessionRef,
    pub(crate) created: i64,
    /// The row's run and conversation when Send was pressed: Qt's
    /// `observedIdentityMatches` lets the listing catch up with the details.
    pub(crate) row_run: String,
    pub(crate) row_conversation: String,
    pub(crate) run: String,
    pub(crate) conversation: String,
    /// The details the send was checked against (Qt's send evidence); for
    /// a session no longer open they stand for its current ones.
    pub(crate) evidence: Option<Arc<Value>>,
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) retry_id: Option<String>,
    pub(crate) compaction: Option<String>,
    pub(crate) consent: bool,
    /// The composer's draft is not the message (a Retry): keep the draft.
    pub(crate) preserve: bool,
    /// The revision of its "sending" save, set by `deliver`.
    pub(crate) revision: u64,
    /// Its one request id, reserved by `deliver` with the checkpoint saved
    /// in that "sending" save (zerus-5wmh.66.4.4); the dispatch uses it.
    pub(crate) request_id: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Qt's `draftMatches`: the same text and the same files, in order.
fn matches_draft(composer: &Composer, text: &str, attachments: &[Attachment]) -> bool {
    composer.draft.text == text && composer.draft.attachments == attachments
}

impl Context {
    pub(crate) fn run(&self) -> String {
        self.details
            .as_deref()
            .map(|d| text(d, "run_id").to_owned())
            .unwrap_or_default()
    }

    pub(crate) fn conversation(&self) -> String {
        self.details
            .as_deref()
            .map(|details| {
                if self.session.subagent.is_some() {
                    // hgs send names the parent conversation and routes with agent_id.
                    text(details, "parent_conversation_id").to_owned()
                } else {
                    text(details, "conversation_id").to_owned()
                }
            })
            .unwrap_or_default()
    }
}

impl Composers {
    /// Asks a question about the open session; its answer brings this back.
    /// It holds the message: the retry's, or the draft Send was pressed on.
    pub(crate) fn ask(&self, hub: &mut Hub, ctx: &Context, retry: Option<Retry>) -> u64 {
        hub.next_token += 1;
        let token = hub.next_token;
        // Qt's question is modal: a new one replaces its key's older one,
        // and only the latest few can be answered.
        hub.prompts
            .retain(|old, prompt| *old + PROMPTS > token && prompt.key != ctx.key);
        let composer = hub.composers.get(&ctx.key);
        let (text, attachments) = match (&retry, composer) {
            (Some(retry), _) => (retry.text.clone(), retry.attachments.clone()),
            (None, Some(composer)) => (
                composer.draft.text.clone(),
                composer.draft.attachments.clone(),
            ),
            (None, None) => Default::default(),
        };
        let prompt = Prompt {
            key: ctx.key.clone(),
            handle: composer.map_or(0, |c| c.handle),
            session: ctx.session.clone(),
            run: ctx.run(),
            conversation: ctx.conversation(),
            text,
            attachments,
            retry,
            rules: None,
            mark: DraftMark::default(),
        };
        hub.prompts.insert(token, prompt);
        token
    }

    /// Asks the start question (`startToSend`): it holds the session as the
    /// catalog shows it and the draft's mark, which the answer compares.
    fn ask_start(&self, hub: &mut Hub, ctx: &Context, session: Session) -> u64 {
        let token = self.ask(hub, ctx, None);
        let mark = Self::draft_mark(hub, &ctx.key);
        if let Some(prompt) = hub.prompts.get_mut(&token) {
            prompt.rules = Some(session);
            prompt.mark = mark;
        }
        token
    }

    /// Send pressed (`MessageComposer::send` then `SessionsWindow::sendMessage`).
    /// `text` is the editor's text and `epoch` the text epoch it is based on:
    /// text typed over an older draft (a remove, an Undo, a hand-back) is
    /// never sent. A refusal hands the text back under a new epoch, so an
    /// editor locked for the send unlocks.
    pub fn send_message(
        &self,
        handle: u64,
        rev: u64,
        epoch: u64,
        text: String,
        position: i64,
        anchor: i64,
    ) -> SendReply {
        self.edit(handle, rev, epoch, text.clone(), position, anchor);
        let mut hub = self.lock();
        let reply = self.send_held(&mut hub, handle, epoch, &text);
        if matches!(reply, SendReply::Busy | SendReply::Blocked { .. }) {
            self.hand_back(hub, handle);
        } else {
            drop(hub);
        }
        reply
    }

    pub(super) fn send_held(
        &self,
        hub: &mut Hub,
        handle: u64,
        epoch: u64,
        text: &str,
    ) -> SendReply {
        let Some(ctx) = self.context(hub) else {
            return SendReply::Busy;
        };
        let Some(composer) = hub.composers.get(&ctx.key).filter(|c| c.handle == handle) else {
            return SendReply::Busy;
        };
        if hub.closed
            || !composer.loaded
            || composer.busy()
            || composer.uncertain
            || composer.text_epoch != epoch
            || composer.draft.text != text
        {
            return SendReply::Busy;
        }
        // A required question card stands in for the editor and Send (Qt's
        // setInputVisible): nothing is sent while it shows.
        if !self.question_part(hub).1 {
            return SendReply::Busy;
        }
        // A stopped session asks how to start before its block reason
        // (`sendMessage` 4809, `startToSend` 4707), while Qt's Send is
        // enabled (`updateControls`, `MessageComposer.cpp:1185`).
        let session = Self::rules_session(&ctx);
        if ctx.session.subagent.is_none()
            && rules::can_start_stopped(&session, Self::start_busy(hub, &ctx))
        {
            if !self
                .view_of(hub, &ctx.key)
                .is_some_and(|view| view.can_send)
            {
                return SendReply::Busy;
            }
            // The Context chip's text (`m_contextUsage->text()`, 4709).
            let details = ctx.details.as_deref().unwrap_or(&Value::Null);
            let usage = session_usage::context(&details["session_usage"], ctx.recorded());
            let question = rules::start_question(&session, &usage.text);
            let token = self.ask_start(hub, &ctx, session);
            return SendReply::Start {
                token,
                question: Box::new(question),
            };
        }
        if let Some(reason) = ctx.reason() {
            if let Some(composer) = hub.composers.get_mut(&ctx.key) {
                composer.show_error(reason.clone());
            }
            return SendReply::Blocked { reason };
        }
        // Qt's Send is disabled otherwise (`updateControls`,
        // `MessageComposer.cpp:1185`), and `send` returns (814).
        if !self
            .view_of(hub, &ctx.key)
            .is_some_and(|view| view.can_send)
        {
            return SendReply::Busy;
        }
        let (message, attachments) = (
            composer.draft.text.clone(),
            composer.draft.attachments.clone(),
        );
        self.consent_or_deliver(hub, &ctx, message, attachments, None)
    }

    /// The cold cache check, then the delivery (`sendMessage` 4815-4852).
    fn consent_or_deliver(
        &self,
        hub: &mut Hub,
        ctx: &Context,
        text: String,
        attachments: Vec<Attachment>,
        retry: Option<Retry>,
    ) -> SendReply {
        let details = ctx.details.as_deref().unwrap_or(&Value::Null);
        let now = self.now();
        if ctx.session.subagent.is_none() && cache::expired(details, now) {
            // Qt's dialog offers Clear context while no reset runs anywhere
            // (`m_clearRequest`, 4825).
            let prompt = cache::cold_prompt(
                details,
                self.compacting(hub, &ctx.key),
                ctx.facts.clearing || hub.clearing.is_some(),
                now,
            );
            let token = self.ask(hub, ctx, retry);
            return SendReply::Cold { token, prompt };
        }
        let delivery = self.delivery(hub, ctx, text, attachments, retry, None, false);
        if self.deliver(hub, delivery) {
            SendReply::Sending
        } else {
            SendReply::Busy
        }
    }

    /// Whether a compaction waits to send for `key` (Qt's `m_compacts`).
    pub(crate) fn compacting(&self, hub: &Hub, key: &str) -> bool {
        hub.compacts.contains_key(key)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delivery(
        &self,
        hub: &Hub,
        ctx: &Context,
        text: String,
        attachments: Vec<Attachment>,
        retry: Option<Retry>,
        compaction: Option<String>,
        consent: bool,
    ) -> Delivery {
        // Qt's `preserveDraft`: a Retry whose message is not the draft.
        let preserve = retry.is_some()
            && hub
                .composers
                .get(&ctx.key)
                .is_some_and(|c| !matches_draft(c, &text, &attachments));
        Delivery {
            key: ctx.key.clone(),
            session: ctx.session.clone(),
            created: ctx.row.created,
            row_run: ctx.row.run_id.clone().unwrap_or_default(),
            row_conversation: ctx.row.conversation_id.clone().unwrap_or_default(),
            run: ctx.run(),
            conversation: ctx.conversation(),
            evidence: ctx.details.clone(),
            text,
            attachments,
            retry_id: retry.map(|r| r.id),
            compaction,
            consent,
            preserve,
            revision: 0,
            request_id: String::new(),
        }
    }

    /// The answer to a question asked by this hub. It re-checks everything:
    /// the session's identity, that the composer is neither busy nor (for
    /// its own draft) uncertain, and that the draft still holds the message,
    /// so an older question never resends a draft whose delivery is unknown.
    pub fn answer(&self, token: u64, answer: Answer) -> SendReply {
        let mut hub = self.lock();
        let Some(prompt) = hub.prompts.remove(&token) else {
            return SendReply::Busy;
        };
        // The cold cache question takes cold answers, the start question
        // start answers.
        let start = match answer {
            Answer::Cancel => return SendReply::Busy,
            Answer::Cold { .. } if prompt.rules.is_some() => return SendReply::Busy,
            Answer::Start { .. } if prompt.rules.is_none() => return SendReply::Busy,
            Answer::Start { choice } => Some(choice),
            Answer::Cold { .. } => None,
        };
        let Some(ctx) = self.context(&hub) else {
            return SendReply::Busy;
        };
        // Qt returns quietly when the cold question's session changed
        // meanwhile (4841-4842). The start question compares the catalog's
        // session instead, with a notice (`startToSend` 4730-4733).
        if start.is_none()
            && (ctx.key != prompt.key
                || ctx.session.computer != prompt.session.computer
                || ctx.session.name != prompt.session.name
                || ctx.run() != prompt.run
                || ctx.conversation() != prompt.conversation)
        {
            return SendReply::Busy;
        }
        let Some(composer) = hub
            .composers
            .get(&prompt.key)
            .filter(|c| c.handle == prompt.handle)
        else {
            return SendReply::Busy;
        };
        if hub.closed
            || !composer.loaded
            || composer.busy()
            || (prompt.retry.is_none() && composer.uncertain)
            || (prompt.retry.is_some() && composer.checkpoint.is_some())
        {
            return SendReply::Busy;
        }
        let action = match (start, &answer) {
            (Some(choice), _) => return self.start_to_send(&mut hub, &ctx, prompt, choice),
            (None, Answer::Cold { action }) => *action,
            _ => return SendReply::Busy,
        };
        match action {
            ColdAction::Send => {
                if let Some(reason) = ctx.reason() {
                    return SendReply::Notice {
                        text: reason,
                        error: true,
                    };
                }
                if prompt.retry.is_none()
                    && !matches_draft(composer, &prompt.text, &prompt.attachments)
                {
                    return SendReply::Notice {
                        text: DRAFT_CHANGED.into(),
                        error: false,
                    };
                }
                let delivery = self.delivery(
                    &hub,
                    &ctx,
                    prompt.text,
                    prompt.attachments,
                    prompt.retry,
                    None,
                    true,
                );
                if self.deliver(&mut hub, delivery) {
                    SendReply::Sending
                } else {
                    SendReply::Busy
                }
            }
            ColdAction::Compact => self.compact_to_send(&mut hub, &ctx, prompt),
            // `clearContext()` (4843): the Clear session dialog.
            ColdAction::Clear => SendReply::ClearContext { token },
        }
    }

    /// `prepareSendingAsync`, the last gate: the draft is saved as "sending"
    /// before anything runs, so a crash afterwards reloads it as uncertain,
    /// and only that save's answer can start the call. An uncertain draft is
    /// never sent again here: only a Retry that keeps it passes. A delivery
    /// drops the open questions of its key and clears Undo, so no older
    /// question resends and Undo never brings a sent file back.
    pub(crate) fn deliver(&self, hub: &mut Hub, mut delivery: Delivery) -> bool {
        if hub.closed {
            return false;
        }
        // `deliverMessage`: a Retry's row must still be a definite failure.
        if let Some(id) = &delivery.retry_id {
            let retained = hub
                .local
                .rows(&delivery.key)
                .iter()
                .any(|row| &row.id == id && row.status == Status::Error && !row.uncertain);
            if !retained {
                return false;
            }
        }
        let Some(composer) = hub.composers.get_mut(&delivery.key) else {
            return false;
        };
        if !composer.loaded
            || composer.sending
            || composer.attaching
            || composer.renaming
            || (composer.uncertain && !delivery.preserve)
            // A Retry never replaces the draft's unresolved identity.
            || (delivery.preserve && composer.checkpoint.is_some())
        {
            return false;
        }
        composer.sending = true;
        composer.waiting = None;
        composer.kept = delivery
            .preserve
            .then(|| (composer.error, composer.notice.clone()));
        composer.error = false;
        composer.notice = "Saving before sending…".into();
        composer.removed = None;
        // The durable save takes the next save revision: an older save's
        // answer, or its retry, is ignored from here on.
        composer.saves += 1;
        composer.saving = true;
        let (key, handle, revision) = (composer.key.clone(), composer.handle, composer.saves);
        composer.delivering = Some(revision);
        delivery.revision = revision;
        // The send's request id is reserved now and saved with the draft in
        // this "sending" save, before any call: the original intent and
        // identity, never a permission.
        let checkpoint = Checkpoint::reserve(Intent {
            kind: Kind::Send,
            draft_revision: revision,
            message_sha256: message_digest(&delivery.text, &delivery.attachments),
            run_id: delivery.run.clone(),
            conversation_id: delivery.conversation.clone(),
            question: None,
            start: None,
            retry_of: delivery.retry_id.clone(),
            consent: delivery.consent,
        });
        delivery.request_id = checkpoint.request_id.clone();
        composer.checkpoint = Some(checkpoint);
        self.0.store.save_durable(&key, composer.stored(), {
            let me = self.clone();
            move |saved| me.prepared(delivery, handle, revision, saved)
        });
        hub.prompts.retain(|_, prompt| prompt.key != key);
        self.changed();
        true
    }

    /// The durable save answered (on the store's writer).
    fn prepared(&self, delivery: Delivery, handle: u64, revision: u64, saved: Result<(), String>) {
        let mut hub = self.lock();
        // Only the answer of the newest save of this composer counts (Qt's
        // `m_saveRevision`, `MessageComposer.cpp:741`).
        let Some(composer) = hub.composers.get_mut(&delivery.key).filter(|c| {
            c.handle == handle && c.saves == revision && c.sending && c.delivering == Some(revision)
        }) else {
            return;
        };
        composer.saving = false;
        if let Err(error) = saved {
            composer.sending = false;
            composer.delivering = None;
            // Nothing was sent: its reserved identity goes with the save.
            composer.checkpoint = None;
            composer.kept = None;
            composer.notice.clear();
            composer.save_failed = true;
            composer.storage_error = Some(error);
            // The editor, locked for the send, unlocks.
            composer.text_epoch += 1;
            let key = composer.key.clone();
            drop(hub);
            self.retry_save(key, handle, revision);
            self.changed();
            return;
        }
        composer.save_failed = false;
        composer.storage_error = None;
        // The app quits: the draft is on disk as "sending" and reloads as
        // uncertain; nothing starts now.
        if hub.closed {
            return;
        }
        drop(hub);
        if !self.still_valid(&delivery) {
            self.ended(&delivery, false, CHANGED, false);
            return;
        }
        {
            let mut hub = self.lock();
            if hub.closed {
                return;
            }
            if let Some(composer) = hub.composers.get_mut(&delivery.key) {
                composer.notice = "Sending…".into();
                // Qt clears the editor once the save holds the draft (751).
                composer.text_epoch += 1;
            }
        }
        self.changed();
        let me = self.clone();
        (self.0.deps.spawn)(Box::new(move || me.send_now(delivery)));
    }

    /// Caches the files for Retry, records the local row, runs the one call.
    fn send_now(&self, delivery: Delivery) {
        let key = delivery.key.clone();
        let cache = self.cache();
        let files = match attachments::prepare(cache.root(), &delivery.attachments) {
            Ok(files) => files,
            Err(error) => return self.ended(&delivery, false, &error, false),
        };
        let me = self.clone();
        cache.prune_later(&self.0.deps.spawn, Arc::new(move || me.retry_files()));
        if !self.still_valid(&delivery) {
            return self.ended(&delivery, false, CHANGED, false);
        }
        // One request id per delivery, reserved by `deliver` and saved with
        // the draft before this dispatch: the local row, the CLI request and
        // the receipt check carry it, and no other delivery reuses it.
        let request_id = delivery.request_id.clone();
        if request_id.is_empty() {
            return self.ended(&delivery, false, CHANGED, false);
        }
        let now = self.now();
        {
            let mut hub = self.lock();
            if hub.closed {
                return;
            }
            // Qt (`deliverMessage`, 4905): the delivery ended meanwhile, or a
            // newer one took its place; either way this one calls nothing.
            if !hub
                .composers
                .get(&key)
                .is_some_and(|c| c.sending && c.delivering == Some(delivery.revision))
            {
                drop(hub);
                return self.ended(&delivery, false, CHANGED, false);
            }
            hub.local.sending(
                &key,
                &request_id,
                delivery.retry_id.as_deref(),
                &delivery.text,
                &files,
                &delivery.run,
                &delivery.conversation,
                now,
            );
            hub.queue.pusher.dispatched(
                &delivery.key,
                &delivery.text,
                &request_id,
                &delivery.run,
                &delivery.conversation,
            );
        }
        self.changed();
        let target = Target {
            computer: delivery.session.computer.clone(),
            name: delivery.session.name.clone(),
            run_id: delivery.run.clone(),
            conversation_id: delivery.conversation.clone(),
        };
        let action = Action::Send {
            text: delivery.text.clone(),
            attachments: delivery.attachments.clone(),
            agent_id: delivery
                .session
                .subagent
                .as_ref()
                .map(|child| child.agent_id.clone()),
            expected_compaction_id: delivery.compaction.clone(),
        };
        // The desktop service takes it, decided before any offer; once
        // offered, its answer stands and nothing falls back.
        let offered =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.offer(&delivery)));
        // The service's own uncertain line replaces the direct path's prefix.
        let service = matches!(offered, Ok(Some(_)));
        let called = match offered {
            Ok(Some(answer)) => match self.offered(&delivery, answer) {
                Some(outcome) => Ok(Ok(outcome)),
                // Handed over: its row keeps sending until a check settles it.
                None => {
                    self.push_finished(&request_id, false, true);
                    return;
                }
            },
            Ok(None) => {
                let dispatch = &self.0.deps.dispatch;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(target, action, request_id.clone())
                }))
            }
            // The offer's thread died: it may have reached the service.
            Err(panic) => Err(panic),
        };
        let outcome = match called {
            Ok(Ok(outcome)) => outcome,
            // A2 answers a busy send lane and every refusal before the call as
            // `Failed`; an `Err` (or a panic) means the call's thread died, so
            // it may have run.
            Ok(Err(error)) => Outcome::Uncertain { detail: error },
            Err(_) => Outcome::Uncertain {
                detail: "The send ended abnormally.".into(),
            },
        };
        self.delivered(&delivery, &outcome, service);
    }

    /// Records the end of `delivery`'s one call in its row and its draft;
    /// `service`: the desktop service's own line, shown as is.
    pub(super) fn delivered(&self, delivery: &Delivery, outcome: &Outcome, service: bool) {
        let (key, request_id) = (&delivery.key, &delivery.request_id);
        let detail = if service {
            self.lock().local.finished_as_is(key, request_id, outcome)
        } else {
            self.lock().local.finished(key, request_id, outcome)
        };
        let ok = matches!(outcome, Outcome::Done { .. });
        let uncertain = matches!(outcome, Outcome::Uncertain { .. });
        self.push_finished(request_id, ok, uncertain);
        self.ended(delivery, ok, &detail, uncertain);
        // `finishMessage`: the open session is read again (`inspect()`).
        if self.lock().open_key.as_deref() == Some(key.as_str()) {
            self.refresh_details();
        }
    }

    /// Qt's `stillValid` (`SessionsWindow.cpp:4880-4904`).
    pub(crate) fn still_valid(&self, delivery: &Delivery) -> bool {
        let now = self.now();
        let expired = |details: Option<&Value>| details.is_some_and(|d| cache::expired(d, now));
        // Only a main session's expired cache asks for consent.
        if delivery.session.subagent.is_none()
            && !delivery.consent
            && expired(delivery.evidence.as_deref())
        {
            return false;
        }
        let hub = self.lock();
        let Some((row, online)) = (self.0.deps.rows)(
            &delivery.session.computer,
            &delivery.session.key,
            &delivery.session.name,
        ) else {
            return false;
        };
        // `observedIdentityMatches`: the row still shows the run it showed,
        // or the listing caught up with the details' run.
        let (row_run, row_conversation) = (
            row.run_id.as_deref().unwrap_or(""),
            row.conversation_id.as_deref().unwrap_or(""),
        );
        let observed = (row_run == delivery.row_run
            && row_conversation == delivery.row_conversation)
            || (row_run == delivery.run && row_conversation == delivery.conversation);
        if row.name != delivery.session.name
            || row.created != delivery.created
            || !observed
            || !online
        {
            return false;
        }
        // A termination, or any session update or rename (Qt's `m_pending`).
        if (self.0.deps.facts)(
            &delivery.session.computer,
            &delivery.session.name,
            &delivery.run,
        )
        .is_some()
        {
            return false;
        }
        if let Some(id) = &delivery.retry_id {
            let retained = hub
                .local
                .rows(&delivery.key)
                .iter()
                .any(|row| &row.id == id && row.status == Status::Error && !row.uncertain);
            if !retained {
                return false;
            }
        }
        let open = hub.open_key.as_deref() == Some(delivery.key.as_str());
        let ctx = if open { self.context(&hub) } else { None };
        if open && ctx.is_none() {
            return false;
        }
        // The open session's current details, else the evidence the send
        // was checked against.
        let evidence = match &ctx {
            Some(ctx) => ctx.details.clone(),
            None => delivery.evidence.clone(),
        };
        if let Some(details) = evidence.as_deref() {
            let identity = delivery.session.subagent.as_ref().map_or_else(
                || text(details, "conversation_id") == delivery.conversation,
                |child| {
                    text(details, "parent_conversation_id") == delivery.conversation
                        && text(details, "agent_id") == child.agent_id
                        && flag(details, "send_supported")
                },
            );
            if text(details, "run_id") != delivery.run
                || !identity
                || text(details, "runtime_state") != "live"
                || text(details, "process_state") != "running"
                || (delivery.session.subagent.is_none()
                    && !delivery.consent
                    && expired(Some(details)))
            {
                return false;
            }
        } else if open {
            return false;
        }
        !ctx.is_some_and(|ctx| ctx.reason().is_some())
    }

    /// `MessageComposer::deliveryFinished`. A Retry that kept the draft
    /// gives the draft back its marks; otherwise a success clears the draft,
    /// and an uncertain end marks it, so Send waits for Allow retry. Tests
    /// only: it finishes whatever the composer holds when it runs, so the
    /// history clear applies `finish_in` under the lock that decided it.
    #[cfg(test)]
    pub(crate) fn delivery_finished(
        &self,
        key: &str,
        ok: bool,
        detail: &str,
        uncertain: bool,
        preserve: bool,
    ) {
        self.finish(key, None, ok, detail, uncertain, preserve);
    }

    /// The end of `delivery`: it ends the composer's send only while it is
    /// still the delivery under way.
    fn ended(&self, delivery: &Delivery, ok: bool, detail: &str, uncertain: bool) {
        let revision = Some(delivery.revision);
        self.finish(
            &delivery.key,
            revision,
            ok,
            detail,
            uncertain,
            delivery.preserve,
        );
    }

    fn finish(
        &self,
        key: &str,
        revision: Option<u64>,
        ok: bool,
        detail: &str,
        uncertain: bool,
        preserve: bool,
    ) {
        let end = End {
            ok,
            detail,
            uncertain,
            preserve,
        };
        self.finish_in(&mut self.lock(), key, revision, end);
        self.reconcile();
        self.changed();
    }

    /// `finish`'s change to the composer, under the caller's lock.
    pub(super) fn finish_in(&self, hub: &mut Hub, key: &str, revision: Option<u64>, end: End<'_>) {
        let End {
            ok,
            detail,
            uncertain,
            preserve,
        } = end;
        if let Some(composer) = hub.composers.get_mut(key) {
            // A delivery that already ended, or that a newer one replaced,
            // ends nothing.
            if revision.is_some_and(|revision| composer.delivering != Some(revision)) {
                return;
            }
            composer.delivering = None;
            composer.sending = false;
            composer.waiting = None;
            composer.gave_up = None;
            // A definite end sent nothing more; an uncertain one keeps the
            // original identity for its receipt.
            if !uncertain {
                composer.checkpoint = None;
            }
            let kept = composer.kept.take();
            if preserve {
                if let Some((error, notice)) = kept {
                    composer.error = error;
                    composer.notice = notice;
                }
                // The draft now holds the Retry's unresolved identity: it
                // stays uncertain until the user allows a retry.
                if uncertain {
                    composer.error = true;
                    composer.uncertain = true;
                    composer.notice = detail.to_owned();
                }
            } else {
                composer.error = !ok;
                composer.uncertain = uncertain;
                composer.notice = if ok {
                    "Submitted to terminal".into()
                } else {
                    detail.to_owned()
                };
                if ok {
                    composer.draft = attachments::Draft::default();
                    composer.position = 0;
                    composer.anchor = 0;
                    composer.attachments_changed();
                }
            }
            composer.text_epoch += 1;
            self.save(composer);
        }
    }

    /// The cached files every failed, definite local row still needs, of
    /// every session S4 keeps rows for. Only those rows are copied under the
    /// hub lock; their files are read after it.
    pub(crate) fn retry_files(&self) -> HashSet<PathBuf> {
        let failed: Vec<LocalMessage> = {
            let hub = self.lock();
            hub.local
                .keys()
                .flat_map(|key| hub.local.rows(key))
                .filter(|row| row.status == Status::Error && !row.uncertain)
                .cloned()
                .collect()
        };
        failed
            .iter()
            .filter_map(LocalMessages::stored_files)
            .flatten()
            .map(|file| file.local_path)
            .collect()
    }

    /// Drops the open session's local rows its history now shows
    /// (`reconcileMessages`); a failed row delivered after all clears a
    /// draft that still holds it. True when a row went.
    pub(crate) fn reconcile(&self) -> bool {
        let (removed, cleared) = {
            let mut guard = self.lock();
            let Some(key) = guard.open_key.clone() else {
                return false;
            };
            if guard.local.rows(&key).is_empty() {
                return false;
            }
            let hub = &mut *guard;
            let child = hub.open.as_ref().and_then(|open| open.subagent.clone());
            let details = hub.followed.details.clone().map(|details| {
                if child.is_none() {
                    return details;
                }
                let mut adjusted = (*details).clone();
                adjusted["conversation_id"] = adjusted["parent_conversation_id"].clone();
                Arc::new(adjusted)
            });
            let reconciled = hub.local.reconcile(
                &key,
                details.as_deref().unwrap_or(&Value::Null),
                hub.followed.events.make_contiguous(),
            );
            let clear = hub.composers.get(&key).is_some_and(|c| {
                !c.sending
                    && c.draft.attachments.is_empty()
                    && reconciled.delivered.contains(&c.draft.text)
                    // History carries only text, never a request id: it
                    // never settles a held identity, which stays until
                    // evidence carrying its id, or the user's Allow retry
                    // (zerus-5wmh.66.4.4). A draft holding none keeps the
                    // text match.
                    && c.checkpoint.is_none()
            });
            // Applied under the lock that decided it, so a send admitted
            // after this check is never cleared by it (zerus-5wmh.66.4.4).
            if clear {
                let end = End {
                    ok: true,
                    detail: "",
                    uncertain: false,
                    preserve: false,
                };
                self.finish_in(hub, &key, None, end);
            }
            (reconciled.removed > 0, clear)
        };
        #[cfg(test)]
        AFTER_RECONCILE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        if cleared {
            self.reconcile();
            self.changed();
        }
        removed
    }

    /// Retry on a failed row (`messageAction` "retry"). Blocking: it reads
    /// the cached files back between `retry_begin` and `retry_finish`. Only
    /// a definite failure (S4's `retryable`) is ever sent again, as a new
    /// submission with its own request id.
    pub fn retry(&self, handle: u64, id: &str) -> SendReply {
        let begun = match self.retry_begin(handle, id) {
            Ok(begun) => begun,
            Err(reply) => return reply,
        };
        let read = attachments::read_stored(self.cache().root(), &begun.files);
        self.retry_finish(begun, read)
    }

    /// Retry's checks before the read (`messageAction`, 4484-4492): the row
    /// a definite failure of the run the session shows, the composer idle,
    /// no block reason and no other read of this session's files.
    pub(crate) fn retry_begin(&self, handle: u64, id: &str) -> Result<RetryBegun, SendReply> {
        let mut hub = self.lock();
        let Some(ctx) = self.context(&hub) else {
            return Err(SendReply::Busy);
        };
        if hub.closed
            || !hub
                .composers
                .get(&ctx.key)
                // A draft holding an unresolved identity takes no Retry
                // until the user allows a retry (zerus-5wmh.66.4.4).
                .is_some_and(|c| c.handle == handle && !c.busy() && c.checkpoint.is_none())
        {
            return Err(SendReply::Busy);
        }
        let Some(row) = hub
            .local
            .retryable(&ctx.key, id, &ctx.run(), &ctx.conversation())
            .cloned()
        else {
            return Err(SendReply::Busy);
        };
        if let Some(reason) = ctx.reason() {
            return Err(SendReply::Notice {
                text: reason,
                error: true,
            });
        }
        let Some(files) = LocalMessages::stored_files(&row) else {
            return Err(SendReply::Notice {
                text: UNAVAILABLE.into(),
                error: true,
            });
        };
        if !hub.retry_reads.insert(ctx.key.clone()) {
            return Err(SendReply::Busy);
        }
        Ok(RetryBegun {
            handle,
            id: id.to_owned(),
            ctx,
            row,
            files,
        })
    }

    /// Retry after the read (`messageAction`, 4497-4521).
    pub(crate) fn retry_finish(
        &self,
        begun: RetryBegun,
        read: Result<Vec<Attachment>, String>,
    ) -> SendReply {
        let RetryBegun {
            handle,
            id,
            ctx,
            row,
            ..
        } = begun;
        let mut hub = self.lock();
        hub.retry_reads.remove(&ctx.key);
        let attachments = match read {
            Ok(attachments) => attachments,
            Err(error) => {
                return SendReply::Notice {
                    text: error,
                    error: true,
                }
            }
        };
        if hub.closed {
            return SendReply::Busy;
        }
        // Qt returns quietly when the session or its run changed during the
        // read, or a write or a send began (4501-4509).
        let Some((now_row, online)) =
            (self.0.deps.rows)(&ctx.session.computer, &ctx.session.key, &ctx.session.name)
        else {
            return SendReply::Busy;
        };
        let open = self.context(&hub).filter(|now| now.key == ctx.key);
        let evidence = match &open {
            Some(now) => now.details.clone(),
            None => ctx.details.clone(),
        };
        let evidence = evidence.as_deref().unwrap_or(&Value::Null);
        let observed = (now_row.run_id == ctx.row.run_id
            && now_row.conversation_id == ctx.row.conversation_id)
            || (now_row.run_id.as_deref() == Some(row.run_id.as_str())
                && now_row.conversation_id.as_deref() == Some(row.conversation_id.as_str()));
        let unchanged = now_row.name == ctx.session.name
            && now_row.created == ctx.row.created
            && observed
            && online
            && (self.0.deps.facts)(&ctx.session.computer, &ctx.session.name, &row.run_id).is_none()
            && text(evidence, "run_id") == row.run_id
            && text(evidence, "conversation_id") == row.conversation_id
            && text(evidence, "runtime_state") == "live"
            && text(evidence, "process_state") == "running"
            // An identity reserved during the read is never replaced.
            && hub
                .composers
                .get(&ctx.key)
                .is_some_and(|c| c.handle == handle && !c.busy() && c.checkpoint.is_none())
            && hub
                .local
                .retryable(&ctx.key, &id, &row.run_id, &row.conversation_id)
                .is_some_and(|now| *now == row);
        if !unchanged {
            return SendReply::Busy;
        }
        let retry = Retry {
            id,
            text: row.text.clone(),
            attachments: attachments.clone(),
        };
        match open {
            // `sendMessage(key, text, files, id)` (4515): its block reason
            // ends in the composer's error (4811), then the cold question.
            Some(now) => {
                if let Some(reason) = now.reason() {
                    if let Some(composer) = hub.composers.get_mut(&now.key) {
                        composer.show_error(reason.clone());
                    }
                    drop(hub);
                    self.changed();
                    return SendReply::Blocked { reason };
                }
                self.consent_or_deliver(&mut hub, &now, row.text, attachments, Some(retry))
            }
            // Another session opened during the read: a warm retry stays
            // with its own session; a cold one needs the question there
            // (4517-4521).
            None => {
                if cache::expired(evidence, self.now()) {
                    return SendReply::Busy;
                }
                let delivery =
                    self.delivery(&hub, &ctx, row.text, attachments, Some(retry), None, false);
                if self.deliver(&mut hub, delivery) {
                    SendReply::Sending
                } else {
                    SendReply::Busy
                }
            }
        }
    }

    /// Delete on a failed row.
    pub fn delete_local(&self, handle: u64, id: &str) {
        let mut hub = self.lock();
        let Some(key) = hub.open_key.clone() else {
            return;
        };
        if hub.composers.get(&key).is_some_and(|c| c.handle == handle) && hub.local.delete(&key, id)
        {
            drop(hub);
            self.changed();
        }
    }

    /// Inspect on a failed row: Native UI, Terminal, or a refresh with Qt's
    /// notice (`messageAction` "inspect").
    pub fn inspect_local(&self, handle: u64, id: &str) -> Option<Inspect> {
        let hub = self.lock();
        let ctx = self.context(&hub)?;
        hub.composers.get(&ctx.key).filter(|c| c.handle == handle)?;
        hub.local
            .rows(&ctx.key)
            .iter()
            .find(|row| row.id == id && row.status == Status::Error)?;
        let facts = TerminalFacts {
            cmd: ctx.facts.cmd.clone(),
            terminal_supported: ctx.row.terminal_supported,
            terminal_reported: ctx.row.terminal_reported,
            terminal_attach_supported: ctx.row.terminal_attach_supported,
            terminal_attach_reported: ctx.row.terminal_attach_reported,
            ..TerminalFacts::default()
        };
        let details = ctx.details.as_deref().unwrap_or(&Value::Null);
        let routed = local_messages::inspect(
            &ctx.facts.cmd,
            terminal::supported(&facts, details),
            &ctx.row.machine,
        );
        drop(hub);
        if matches!(routed, Inspect::Refresh { .. }) {
            self.refresh_details();
        }
        Some(routed)
    }

    #[cfg(test)]
    pub(crate) fn delivery_for_test(&self, handle: u64, message: &str) -> Delivery {
        let hub = self.lock();
        let ctx = self.context(&hub).unwrap();
        assert!(hub
            .composers
            .get(&ctx.key)
            .is_some_and(|c| c.handle == handle));
        self.delivery(
            &hub,
            &ctx,
            message.to_owned(),
            Vec::new(),
            None,
            None,
            false,
        )
    }
}

/// S3's cancel texts this side gives itself (`SessionsWindow.cpp:4559, 4756`).
const COMPACT_CANCELLED: &str = "Compaction was cancelled. Your draft was kept without sending.";
const START_CANCELLED: &str = "Starting was cancelled. Your draft was kept without sending.";
/// Clear session refused by another write of its session (4391, 4395).
const TERMINATING: &str = "The session is terminating. Your draft stays here.";
const UPDATING: &str = "Wait for the session update to finish.";
/// The end of a start or compaction call whose thread died.
const ABNORMAL: &str = "The request ended abnormally.";
/// Clear session's notices (`SessionsWindow.cpp:1025`).
const CLEARED: &str = "Context cleared. Your draft is ready to send.";
const CLEAR_SENT: &str =
    "Clear command sent. Waiting for the agent to confirm the new conversation…";

/// The exact message a dispatch carries, as its checkpoint binds it.
pub(crate) fn message_digest(text: &str, attachments: &[Attachment]) -> String {
    message_sha256(
        text,
        attachments.iter().map(|a| {
            (
                a.name.as_str(),
                a.mime.as_str(),
                a.reference.as_str(),
                a.bytes.as_ref(),
            )
        }),
    )
}

/// A call's outcome: a lane already taken (`actions::BUSY`) is a definite
/// refusal with `refused` as its text; any other `Err` (the call's thread
/// died) and a panic are uncertain.
pub(crate) fn outcome_of(
    called: std::thread::Result<Result<Outcome, String>>,
    refused: &str,
) -> Outcome {
    match called {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) if error == crate::actions::BUSY => Outcome::Failed {
            detail: refused.into(),
        },
        Ok(Err(error)) => Outcome::Uncertain { detail: error },
        Err(_) => Outcome::Uncertain {
            detail: ABNORMAL.into(),
        },
    }
}

/// The text and attachment digests S3 compares at every step. Each digest
/// covers what Qt's `draftMatches` compares (`MessageComposer.cpp:668-676`):
/// the name, type, reference and bytes. They are computed once per
/// attachment list, not per check: while a continuation waits, every
/// Activity state and poll compares the mark.
fn mark(composer: &mut Composer) -> DraftMark {
    let digests = match &composer.digests {
        Some((list, digests)) if *list == composer.attachments_rev => digests.clone(),
        _ => {
            let digests: Vec<String> = composer.draft.attachments.iter().map(digest).collect();
            composer.digests = Some((composer.attachments_rev, digests.clone()));
            digests
        }
    };
    DraftMark {
        text: composer.draft.text.clone(),
        attachments: digests,
    }
}

fn digest(attachment: &Attachment) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in [
        attachment.name.as_bytes(),
        attachment.mime.as_bytes(),
        attachment.reference.as_bytes(),
        &attachment.bytes,
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A session row as S3 reads it (`Session`).
fn rules_of_row(
    key: &str,
    session: &SessionRef,
    row: &ViewRow,
    online: bool,
    terminating: bool,
) -> Session {
    Session {
        key: key.to_owned(),
        host: session.computer.clone(),
        name: row.name.clone(),
        created: row.created,
        run_id: row.run_id.clone().unwrap_or_default(),
        conversation_id: row.conversation_id.clone().unwrap_or_default(),
        state: row.state.clone(),
        online,
        terminating,
        tracked: row.tracked,
        cmd: cmd(row),
        resumable: row.resumable,
    }
}

/// The continuation of `key` named by `revision`: the start or the compaction.
pub(super) fn pending_mut<'a>(
    hub: &'a mut Hub,
    key: &str,
    start: bool,
    revision: u64,
) -> Option<&'a mut Pending> {
    let pending = if start {
        hub.start.as_mut().map(|(p, _)| p).filter(|p| p.key == key)
    } else {
        hub.compacts.get_mut(key).map(|(p, _)| p)
    };
    pending.filter(|p| p.revision == revision)
}

/// The start continuation `revision` of `key`, read-only.
fn pending_of<'a>(hub: &'a Hub, key: &str, revision: u64) -> Option<&'a Pending> {
    hub.start
        .as_ref()
        .map(|(p, _)| p)
        .filter(|p| p.key == key && p.revision == revision)
}

/// Takes the continuation `revision` of `key` out of the hub.
fn take_pending(hub: &mut Hub, key: &str, start: bool, revision: u64) -> Option<Pending> {
    pending_mut(hub, key, start, revision)?;
    if start {
        hub.start.take().map(|(p, _)| p)
    } else {
        hub.compacts.remove(key).map(|(p, _)| p)
    }
}

/// Clear session and the cache popover's Clear context for the open
/// session (`renderDetails` 3790-3804, `clearContext` 4638-4655).
fn clear_control(hub: &Hub, ctx: &Context, details: &Value) -> ClearControl {
    if ctx.session.subagent.is_some() {
        return ClearControl {
            run: ctx.run(),
            conversation: ctx.conversation(),
            ..ClearControl::default()
        };
    }
    // A Claude chooser hides the native composer: clearing first dismisses it.
    let supported = flag(details, "clear_context_supported");
    let dismiss = !supported;
    let (name, machine) = (&ctx.session.name, &ctx.row.machine);
    // Qt's `m_clearRequest`: one reset at a time across all sessions.
    let running = hub.clearing.is_some() || ctx.facts.clearing;
    let sending = hub.composers.get(&ctx.key).is_some_and(|c| c.sending);
    let reason = rules::context_action_reason(details, "clear");
    ClearControl {
        enabled: ctx.online
            && ctx.row.state != "archived"
            && !running
            && !sending
            && !ctx.facts.inspect_error
            && zerus_client_core::actions::clearable(details),
        dismiss,
        title: "Clear session?".into(),
        text: if dismiss {
            format!("Claude in {name} on {machine} is waiting for a choice in Terminal. Dismiss it and clear the conversation context?\n\nThis presses Esc once, as in Terminal: a pending approval is denied and a question stays unanswered. Then the agent’s native clear command runs. Your unsent draft will be kept.")
        } else {
            format!("Clear the conversation context in {name} on {machine}?\n\nThis runs the agent’s native clear command. Future messages will start without the current conversation context. Your unsent draft will be kept.")
        },
        confirm: if dismiss {
            "Dismiss and clear"
        } else {
            "Clear session"
        }
        .into(),
        run: ctx.run(),
        conversation: ctx.conversation(),
        cache_enabled: ctx.online && !running && supported,
        running,
        // An offline computer's last inspection no longer explains it, and a
        // chooser still allows Dismiss and clear (3795-3803).
        reason: if ctx.online && !flag(details, "clear_context_dismiss") {
            rules::reason_sentence(&reason)
        } else {
            String::new()
        },
        cache_reason: if ctx.online {
            rules::reason_sentence(&reason)
        } else {
            String::new()
        },
    }
}

impl Composers {
    /// Clear session for an unselected menu row, using that row's own inspect
    /// and the same control as the open composer (Qt's checking tooltip and
    /// inspectionReady). The identities must still match the listed row.
    pub fn clear_control_for(&self, session: SessionRef) -> Option<ClearControl> {
        let (row, online) = (self.0.deps.rows)(&session.computer, &session.key, &session.name)?;
        // The listed row's durable ID names its composer's draft key, as the
        // webview opened it (zerus-fsht5).
        if let (Some(caller), Some(listed)) =
            (session.session_id.as_deref(), row.session_id.as_deref())
        {
            if caller != listed {
                return None;
            }
        }
        let session = SessionRef {
            session_id: row
                .session_id
                .clone()
                .or_else(|| session.session_id.clone()),
            ..session
        };
        let target = Target {
            computer: session.computer.clone(),
            name: session.name.clone(),
            run_id: row.run_id.clone().unwrap_or_default(),
            conversation_id: row.conversation_id.clone().unwrap_or_default(),
        };
        let details = (self.0.deps.inspect)(&target, session.archive_id.as_deref(), None).ok()?;
        if details["run_id"].as_str() != Some(target.run_id.as_str())
            || details["conversation_id"].as_str() != Some(target.conversation_id.as_str())
        {
            return None;
        }
        let hub = self.lock();
        // The composer's draft key, the one `hub.clearing` and the sending
        // check use; the catalog key never names a composer.
        let key = super::session_draft_key(&session);
        let clearing = (self.0.deps.facts)(&session.computer, &session.name, &target.run_id)
            == Some(WriteKind::Clear)
            || hub.clearing.as_ref() == Some(&key);
        let facts = BlockFacts {
            clearing,
            online,
            state: row.state.clone(),
            cmd: cmd(&row),
            ..BlockFacts::default()
        };
        let context = Context {
            session,
            key,
            row,
            online,
            details: Some(Arc::new(details)),
            facts,
        };
        Some(clear_control(
            &hub,
            &context,
            context.details.as_deref().unwrap(),
        ))
    }

    /// The catalog's view of the open session for S3.
    fn rules_session(ctx: &Context) -> Session {
        rules_of_row(
            &ctx.key,
            &ctx.session,
            &ctx.row,
            ctx.online,
            ctx.facts.terminating,
        )
    }

    /// A continuation's session as the catalog shows it now, open or not
    /// (Qt finds it in `m_entries` by key).
    fn rules_of(&self, key: &str, session: &SessionRef) -> Option<Session> {
        let (row, online) = (self.0.deps.rows)(&session.computer, &session.key, &session.name)?;
        let write = (self.0.deps.facts)(
            &session.computer,
            &session.name,
            row.run_id.as_deref().unwrap_or(""),
        );
        Some(rules_of_row(
            key,
            session,
            &row,
            online,
            write == Some(WriteKind::Terminate),
        ))
    }

    /// `canStartStopped`'s busy part (4677-4679): an update runs, another
    /// start waits anywhere, or this session compacts or sends.
    fn start_busy(hub: &Hub, ctx: &Context) -> bool {
        ctx.facts.updating
            || hub.start.is_some()
            || hub.compacts.contains_key(&ctx.key)
            || hub.composers.get(&ctx.key).is_some_and(|c| c.sending)
    }

    fn draft_mark(hub: &mut Hub, key: &str) -> DraftMark {
        hub.composers.get_mut(key).map(mark).unwrap_or_default()
    }

    /// The followed details of `key` when it is the open session.
    fn open_details(hub: &Hub, key: &str) -> Option<Arc<Value>> {
        let open = hub.open.as_ref()?;
        let followed = hub.followed.computer == open.computer && hub.followed.key == open.key;
        if hub.open_key.as_deref() != Some(key) || !followed {
            return None;
        }
        hub.followed.details.clone()
    }

    /// "Start the session to send?" answered (`startToSend` 4727-4764):
    /// S3 compares the session and the draft again, then the draft waits.
    fn start_to_send(
        &self,
        hub: &mut Hub,
        ctx: &Context,
        prompt: Prompt,
        choice: StartChoice,
    ) -> SendReply {
        let Some(original) = prompt.rules else {
            return SendReply::Busy;
        };
        let current = Self::rules_session(ctx);
        let now_mark = Self::draft_mark(hub, &prompt.key);
        let begun = StartSend::begin(
            &original,
            Some(&current),
            Self::start_busy(hub, ctx),
            choice,
            prompt.mark,
            &now_mark,
            self.now(),
        );
        let (start, waiting) = match begun {
            Ok(begun) => begun,
            // "The session changed" is an error notice, "Draft changed" a
            // plain one (4732, 4734).
            Err(text) => {
                let error = text != DRAFT_CHANGED;
                return SendReply::Notice { text, error };
            }
        };
        let pending = Pending {
            key: prompt.key.clone(),
            session: ctx.session.clone(),
            run: original.run_id.clone(),
            conversation: original.conversation_id.clone(),
            retry: None,
            revision: 0,
            evidence: ctx.details.clone(),
            polling: false,
            request_id: String::new(),
            call: Call::NotCalled,
        };
        hub.start = Some((pending, start));
        if self.wait(hub, &prompt.key, waiting, true) {
            SendReply::Sending
        } else {
            hub.start = None;
            SendReply::Busy
        }
    }

    /// The cold cache's "Compact and continue" (`sendMessage` 4844-4847,
    /// `compactToSend` 4536-4564).
    fn compact_to_send(&self, hub: &mut Hub, ctx: &Context, prompt: Prompt) -> SendReply {
        let details = ctx.details.as_deref().unwrap_or(&Value::Null);
        // Qt's dialog offers it only with native support and no compaction
        // waiting for this session (4822); `compactToSend` returns
        // while one waits (4539).
        if !flag(details, "compact_context_supported") || hub.compacts.contains_key(&prompt.key) {
            return SendReply::Busy;
        }
        let Some(composer) = hub.composers.get(&prompt.key) else {
            return SendReply::Busy;
        };
        // The draft must still be the message, a Retry's too (4845, 4539).
        if !matches_draft(composer, &prompt.text, &prompt.attachments) {
            return SendReply::Notice {
                text: DRAFT_CHANGED.into(),
                error: false,
            };
        }
        // An uncertain draft never waits (`prepareWaitingToSendAsync`,
        // `MessageComposer.cpp:708`). Qt then ends the continuation through
        // `deliveryFinished`, which clears the uncertain mark; here it stays.
        if composer.uncertain {
            return SendReply::Notice {
                text: COMPACT_CANCELLED.into(),
                error: true,
            };
        }
        let mark = Self::draft_mark(hub, &prompt.key);
        let (compact, waiting) = CompactSend::begin(
            &Self::rules_session(ctx),
            &prompt.run,
            &prompt.conversation,
            mark,
            prompt.retry.as_ref().map(|r| r.id.clone()),
            self.now(),
        );
        let key = prompt.key.clone();
        let pending = Pending {
            key: prompt.key,
            session: ctx.session.clone(),
            run: prompt.run,
            conversation: prompt.conversation,
            retry: prompt.retry,
            revision: 0,
            evidence: ctx.details.clone(),
            polling: false,
            request_id: String::new(),
            call: Call::NotCalled,
        };
        hub.compacts.insert(key.clone(), (pending, compact));
        if self.wait(hub, &key, waiting, false) {
            SendReply::Sending
        } else {
            hub.compacts.remove(&key);
            SendReply::Busy
        }
    }

    /// `prepareWaitingToSendAsync` (`MessageComposer.cpp:706-720`): the
    /// composer holds the draft as waiting and saves it durably through its
    /// save revision; only that save's answer can start the continuation's
    /// call. The continuation is named by that revision from here on.
    /// False when the composer cannot wait (Qt's `done(false)`).
    fn wait(&self, hub: &mut Hub, key: &str, waiting: String, start: bool) -> bool {
        if hub.closed {
            return false;
        }
        // What the start or compaction call names: its run and conversation,
        // the message waiting to be sent and, for a start, how it starts.
        let named = if start {
            hub.start
                .as_ref()
                .filter(|(p, _)| p.key == key)
                .map(|(p, s)| (p.clone(), Some(s.choice())))
        } else {
            hub.compacts.get(key).map(|(p, _)| (p.clone(), None))
        };
        let Some(composer) = hub.composers.get_mut(key) else {
            return false;
        };
        if !composer.loaded
            || composer.sending
            || composer.uncertain
            || composer.attaching
            || composer.renaming
        {
            return false;
        }
        composer.waiting = Some(waiting);
        composer.saves += 1;
        composer.saving = true;
        let (handle, revision) = (composer.handle, composer.saves);
        composer.delivering = Some(revision);
        // The call's request id is reserved now and saved with the waiting
        // draft before it is asked for (zerus-5wmh.66.4.4): intent and
        // identity only. A reload never continues it.
        let (run_id, conversation_id, retry, choice) = match &named {
            Some((pending, choice)) => (
                pending.run.clone(),
                pending.conversation.clone(),
                pending.retry.clone(),
                *choice,
            ),
            None => Default::default(),
        };
        let message_sha256 = match &retry {
            Some(retry) => message_digest(&retry.text, &retry.attachments),
            None => message_digest(&composer.draft.text, &composer.draft.attachments),
        };
        let intent = Intent {
            kind: if start {
                Kind::ResumeToSend
            } else {
                Kind::CompactToSend
            },
            draft_revision: revision,
            message_sha256,
            run_id,
            conversation_id,
            question: None,
            start: start.then_some(match choice {
                Some(StartChoice::ResumeCompact) => Start::ResumeCompact,
                Some(StartChoice::Fresh) => Start::Fresh,
                Some(StartChoice::Resume) | None => Start::Resume,
            }),
            retry_of: retry.map(|r| r.id),
            consent: false,
        };
        // A compaction also reserves the send it waits for (version 2), so
        // the desktop service can take the whole compact-to-send; the
        // direct path reserves its send again when it sends. A resume the
        // service may take (a local Claude, Kimi or Codex session, while a
        // session resume route is set) reserves its send too, with the receipted
        // resume (version 3); every other start keeps version 1.
        let resumable = start
            && choice == Some(StartChoice::Resume)
            && self.resume_routed()
            && named.as_ref().is_some_and(|(pending, _)| {
                pending.session.computer.is_empty()
                    && pending.session.subagent.is_none()
                    && pending.session.name.split('/').next().is_some_and(|agent| {
                        zerus_client_core::desktop::session_resume::AGENTS.contains(&agent)
                    })
            });
        let checkpoint = if resumable {
            Checkpoint::reserve_session_resume(intent).expect("a resume's checkpoint")
        } else if start {
            Checkpoint::reserve(intent)
        } else {
            Checkpoint::reserve_compact_to_send(intent).expect("a compaction's checkpoint")
        };
        let request_id = checkpoint.request_id.clone();
        composer.checkpoint = Some(checkpoint);
        let stored = composer.stored();
        let pending = if start {
            hub.start.as_mut().map(|(p, _)| p).filter(|p| p.key == key)
        } else {
            hub.compacts.get_mut(key).map(|(p, _)| p)
        };
        if let Some(pending) = pending {
            pending.revision = revision;
            pending.request_id = request_id;
        }
        // Like a delivery, a waiting send drops its key's open questions.
        hub.prompts.retain(|_, prompt| prompt.key != key);
        let me = self.clone();
        let owned = key.to_owned();
        self.0.store.save_durable(key, stored, move |saved| {
            me.waiting_saved(&owned, handle, revision, start, saved)
        });
        // The ticker polls while a continuation waits.
        self.0.opened.notify_all();
        self.changed();
        true
    }

    /// The waiting draft's durable save answered (on the store's writer):
    /// S3's `prepared` decides whether the start or compaction is asked
    /// for (`startToSend` 4741-4762, `compactToSend` 4547-4563). A closed
    /// hub (the app quits) asks for nothing; the draft reloads as saved.
    fn waiting_saved(
        &self,
        key: &str,
        handle: u64,
        revision: u64,
        start: bool,
        saved: Result<(), String>,
    ) {
        let mut hub = self.lock();
        let mut ok = saved.is_ok();
        let mut failed = false;
        match hub.composers.get_mut(key).filter(|c| c.handle == handle) {
            Some(composer) if composer.saves == revision => {
                composer.saving = false;
                composer.save_failed = saved.is_err();
                composer.storage_error = saved.err();
                failed = composer.save_failed;
            }
            // A newer save went meanwhile (`MessageComposer.cpp:713`).
            _ => ok = false,
        }
        if hub.closed {
            return;
        }
        let pending = pending_mut(&mut hub, key, start, revision).map(|p| p.clone());
        let step = match pending {
            None => Step::Idle,
            Some(pending) => {
                let current = self.rules_of(key, &pending.session);
                let open = Self::open_details(&hub, key);
                let evidence = if hub.open_key.as_deref() == Some(key) {
                    open.clone()
                } else {
                    pending.evidence.clone()
                };
                let write = (self.0.deps.facts)(
                    &pending.session.computer,
                    &pending.session.name,
                    evidence
                        .as_deref()
                        .map(|details| text(details, "run_id"))
                        .unwrap_or(""),
                );
                // Qt's `m_pending` (and its rename).
                let busy = matches!(write, Some(WriteKind::Update | WriteKind::Settings));
                if start {
                    hub.start.as_mut().map_or(Step::Idle, |(_, start)| {
                        start.prepared(ok, current.as_ref(), evidence.as_deref(), busy)
                    })
                } else {
                    hub.compacts
                        .get_mut(key)
                        .map_or(Step::Idle, |(_, compact)| {
                            compact.prepared(
                                ok,
                                current.as_ref(),
                                evidence.as_deref(),
                                open.as_deref(),
                                busy,
                            )
                        })
                }
            }
        };
        drop(hub);
        if failed {
            self.retry_save(key.to_owned(), handle, revision);
        }
        self.step(key, start, revision, step, None);
        self.changed();
    }

    /// Carries out S3's answer for the continuation `revision` of `key`.
    /// `read` is the reading the answer came from (`inspected`): the
    /// compaction after a start and the send at the end name its identity.
    fn step(&self, key: &str, start: bool, revision: u64, step: Step, read: Option<&Arc<Value>>) {
        if start {
            let ready = match &step {
                Step::Send { run_id, .. } => Some(run_id.as_str()),
                Step::Compact(..) => read.map(|read| text(read, "run_id")),
                _ => None,
            };
            if ready.is_some_and(|run| self.listing_behind(key, revision, run)) {
                return;
            }
        }
        match step {
            Step::Idle => {}
            Step::Cancelled(text) => self.cancel(key, start, revision, &text),
            Step::Run(action) => self.request(key, start, revision, action),
            Step::Compact(compact, waiting) => {
                if let Some(read) = read {
                    self.compact_after_start(key, revision, compact, waiting, read);
                }
            }
            Step::Send {
                run_id,
                conversation_id,
                compaction_id,
            } => self.send_after(
                key,
                start,
                revision,
                (run_id, conversation_id, compaction_id),
                read,
            ),
        }
    }

    /// zerus-77ljy: a started run can read ready (an inspect, or the
    /// followed details) before the listing replaces the stopped row. Judged
    /// against that stale row, which still asks to resume, the waiting
    /// message would end with CHANGED. Until the listed row shows the ready
    /// run the start keeps waiting: its next reading or the poll decides
    /// again, within its limit. A missing row is not waited for, and the
    /// send's own checks (`still_valid`) stay as they are.
    fn listing_behind(&self, key: &str, revision: u64, run: &str) -> bool {
        let hub = self.lock();
        let session = match (&hub.open, hub.open_key.as_deref()) {
            (Some(open), Some(open_key)) if open_key == key => open.clone(),
            _ => match pending_of(&hub, key, revision) {
                Some(pending) => pending.session.clone(),
                None => return false,
            },
        };
        (self.0.deps.rows)(&session.computer, &session.key, &session.name)
            .is_some_and(|(row, _)| row.run_id.as_deref().unwrap_or("") != run)
    }

    /// The start or compaction call (Qt's `resumeSession`,
    /// `startFreshSession` and `requestCompactContext`) off the UI thread,
    /// with its own request id. A taken lane is a definite refusal; any
    /// other `Err` or a panic is uncertain. Either way S3 ends the
    /// continuation, and its message is never sent.
    fn request(&self, key: &str, start: bool, revision: u64, action: Action) {
        let (target, id) = {
            let mut hub = self.lock();
            if hub.closed {
                return;
            }
            let Some(pending) = pending_mut(&mut hub, key, start, revision) else {
                return;
            };
            (
                Target {
                    computer: pending.session.computer.clone(),
                    name: pending.session.name.clone(),
                    run_id: pending.run.clone(),
                    conversation_id: pending.conversation.clone(),
                },
                // Reserved with the waiting save's checkpoint.
                pending.request_id.clone(),
            )
        };
        let me = self.clone();
        let owned = key.to_owned();
        (self.0.deps.spawn)(Box::new(move || {
            // Qt asks right in the save's callback (4762, 4561): a Cancel
            // send or Stop that came in between leaves nothing to ask for.
            {
                let mut hub = me.lock();
                if hub.closed {
                    return;
                }
                let Some(pending) = pending_mut(&mut hub, &owned, start, revision) else {
                    return;
                };
                // From here the call may run: an end before its definite
                // outcome leaves the draft holding its identity.
                if !id.is_empty() {
                    pending.call = Call::Running;
                }
            }
            // The desktop service takes the whole compact-to-send or
            // resume-then-send, decided before any offer; once offered,
            // nothing falls back.
            if !start && !id.is_empty() && me.hand_compaction(&owned, revision) {
                me.changed();
                return;
            }
            if start && !id.is_empty() && me.hand_start(&owned, revision) {
                me.changed();
                return;
            }
            let dispatch = &me.0.deps.dispatch;
            let request = id.clone();
            // Without its reserved id nothing is called: a definite refusal.
            let called = if id.is_empty() {
                Ok(Err(crate::actions::BUSY.to_owned()))
            } else {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(target, action, id)
                }))
            };
            // A taken lane refused before anything ran: Qt's cancel text
            // (4756, 4559), never an uncertain end.
            let refused = if start {
                START_CANCELLED
            } else {
                COMPACT_CANCELLED
            };
            let outcome = outcome_of(called, refused);
            let (next, polling) = {
                let mut hub = me.lock();
                let definite = !matches!(outcome, Outcome::Uncertain { .. });
                let live = match pending_mut(&mut hub, &owned, start, revision) {
                    // Answered: never in flight again, before any handler
                    // can end the continuation.
                    Some(pending) => {
                        pending.call = if definite {
                            Call::Settled
                        } else {
                            Call::Unresolved
                        };
                        true
                    }
                    None => {
                        me.settle_orphan(&mut hub, &request, definite);
                        false
                    }
                };
                match (live, start) {
                    (false, _) => (Step::Idle, false),
                    (true, true) => match hub.start.as_mut() {
                        Some((_, start)) => (start.start_finished(&outcome), false),
                        None => (Step::Idle, false),
                    },
                    (true, false) => match hub.compacts.get_mut(&owned) {
                        Some((_, compact)) => {
                            let next = compact.compact_finished(&outcome);
                            (next, compact.polling())
                        }
                        None => (Step::Idle, false),
                    },
                }
            };
            me.step(&owned, start, revision, next, None);
            // What the open session showed while the call ran counts, and
            // an accepted compaction is read at once (975).
            me.continue_open();
            if polling {
                me.poll(&owned, false, revision);
            }
            me.changed();
        }));
    }

    /// "Resume, compact and send" once the new run is ready
    /// (`continueAfterStart` 4796-4800): the start ends, and the compaction
    /// of the conversation it opened waits for its own durable save.
    fn compact_after_start(
        &self,
        key: &str,
        revision: u64,
        compact: CompactSend,
        waiting: String,
        read: &Arc<Value>,
    ) {
        let mut hub = self.lock();
        let Some(mut pending) = take_pending(&mut hub, key, true, revision) else {
            return;
        };
        // The waiting draft ended meanwhile: nothing more runs.
        if hub
            .composers
            .get(key)
            .is_none_or(|c| c.delivering != Some(revision))
        {
            return;
        }
        pending.run = text(read, "run_id").to_owned();
        pending.conversation = text(read, "conversation_id").to_owned();
        pending.evidence = Some(Arc::clone(read));
        pending.polling = false;
        hub.compacts.insert(key.to_owned(), (pending, compact));
        if !self.wait(&mut hub, key, waiting, false) {
            hub.compacts.remove(key);
            drop(hub);
            self.finish(key, Some(revision), false, COMPACT_CANCELLED, false, false);
        }
    }

    /// The continuation's send (`deliverMessage` with full-context consent,
    /// 4801 and 4623). The continuation goes first, so a repeated reading
    /// never sends twice (4619-4622); then the ordinary delivery runs with
    /// its durable "sending" save, its own request id and `still_valid`.
    fn send_after(
        &self,
        key: &str,
        start: bool,
        revision: u64,
        (run, conversation, compaction): (String, String, Option<String>),
        read: Option<&Arc<Value>>,
    ) {
        let mut hub = self.lock();
        let Some(pending) = take_pending(&mut hub, key, start, revision) else {
            return;
        };
        if hub.closed {
            return;
        }
        // Only the draft that waits for this continuation goes.
        let Some(composer) = hub
            .composers
            .get_mut(key)
            .filter(|c| c.delivering == Some(revision))
        else {
            return;
        };
        // The session was seen ready: the start or compaction is resolved,
        // and the send reserves its own identity before its call.
        composer.checkpoint = None;
        let composer = &*composer;
        let (text, attachments) = match &pending.retry {
            Some(retry) => (retry.text.clone(), retry.attachments.clone()),
            None => (
                composer.draft.text.clone(),
                composer.draft.attachments.clone(),
            ),
        };
        // A Retry's message is not the draft: a newer draft stays.
        let preserve = pending.retry.is_some() && !matches_draft(composer, &text, &attachments);
        let session = match (&hub.open, hub.open_key.as_deref()) {
            (Some(open), Some(open_key)) if open_key == key => open.clone(),
            _ => pending.session.clone(),
        };
        let Some((row, _)) = (self.0.deps.rows)(&session.computer, &session.key, &session.name)
        else {
            drop(hub);
            return self.finish(key, Some(revision), false, CHANGED, false, false);
        };
        let delivery = Delivery {
            key: key.to_owned(),
            session,
            created: row.created,
            row_run: row.run_id.unwrap_or_default(),
            row_conversation: row.conversation_id.unwrap_or_default(),
            run,
            conversation,
            // The reading that showed the session ready (Qt's `data`).
            evidence: read.cloned().or(pending.evidence),
            text,
            attachments,
            retry_id: pending.retry.map(|r| r.id),
            compaction,
            consent: true,
            preserve,
            revision: 0,
            request_id: String::new(),
        };
        if !self.deliver(&mut hub, delivery) {
            drop(hub);
            self.finish(key, Some(revision), false, CHANGED, false, false);
        }
    }

    /// The late outcome of a start or compaction whose continuation ended
    /// while it ran (zerus-5wmh.66.4.4). A definite one resolves the held
    /// identity: the draft, if it still holds exactly that identity, shows
    /// its end as a plain error again. An uncertain one leaves it held until
    /// the user allows a retry.
    pub(super) fn settle_orphan(&self, hub: &mut Hub, request: &str, definite: bool) {
        let Some(at) = hub.orphans.iter().position(|o| o.request == request) else {
            return;
        };
        let Some(Orphan { key, notice, .. }) = hub.orphans.remove(at) else {
            return;
        };
        if !definite {
            return;
        }
        let held = |c: &&mut Composer| {
            c.checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.request_id == request)
        };
        if let Some(composer) = hub.composers.get_mut(&key).filter(held) {
            composer.checkpoint = None;
            composer.uncertain = false;
            composer.error = true;
            composer.notice = notice;
            self.save(composer);
        }
    }

    /// Ends the continuation `revision` without sending
    /// (`cancelStartContinuation`, `cancelCompactContinuation`): the draft
    /// stays and shows `text`. A continuation already gone ends nothing
    /// (4531, 4768), and the composer ends only while it still waits for
    /// this one, so a late cancel never ends a newer continuation or the
    /// delivery that followed.
    pub(super) fn cancel(&self, key: &str, start: bool, revision: u64, text: &str) {
        let pending = {
            let mut hub = self.lock();
            let Some(pending) = take_pending(&mut hub, key, start, revision) else {
                return;
            };
            // Its start or compaction was asked for and has no definite
            // outcome: it may have run. The draft keeps that call's identity
            // and stays uncertain until the user allows a retry
            // (zerus-5wmh.66.4.4). A call still in flight is tracked, in the
            // same lock, so its own late definite answer can resolve it; one
            // that answered uncertainly has no later answer to wait for.
            if matches!(pending.call, Call::Running | Call::Unresolved) {
                if pending.call == Call::Running {
                    let orphan = Orphan {
                        request: pending.request_id.clone(),
                        key: key.to_owned(),
                        notice: text.to_owned(),
                    };
                    track_orphan(&mut hub.orphans, orphan);
                }
                let detail = local_messages::uncertain_detail(text);
                let end = End {
                    ok: false,
                    detail: &detail,
                    uncertain: true,
                    preserve: false,
                };
                self.finish_in(&mut hub, key, Some(revision), end);
                drop(hub);
                self.reconcile();
                self.changed();
                return;
            }
            pending
        };
        self.finish(key, Some(revision), false, text, false, false);
        // A waiting send given up at its time limit says the session did not
        // get there; the session can still get there later (zerus-le5s). Its
        // last reading names the target a later reading must match.
        if [rules::NOT_READY, rules::NOT_COMPACTED].contains(&text) {
            let seen = |key: &str| {
                pending
                    .evidence
                    .as_deref()
                    .and_then(|details| details.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            if let Some(composer) = self.lock().composers.get_mut(key) {
                if composer.error && composer.notice == text && !seen("run_id").is_empty() {
                    composer.gave_up = Some(super::GaveUp {
                        notice: text.to_owned(),
                        run: seen("run_id"),
                        conversation: seen("conversation_id"),
                        phase: seen("phase"),
                        activity: seen("activity"),
                    });
                }
            }
        }
    }

    /// A given-up waiting send's notice goes once the open session shows it
    /// moved on: a reading of the same run and conversation (a fresh one, when
    /// the give-up saw none yet) that went from what it showed then to
    /// working, or to ready. A stale or other target's reading, or a notice
    /// another one replaced since, keeps it.
    pub(crate) fn settle_gave_up(&self) {
        let mut hub = self.lock();
        let Some(key) = hub.open_key.clone() else {
            return;
        };
        let Some(details) = Self::open_details(&hub, &key) else {
            return;
        };
        let text = |key: &str| details.get(key).and_then(Value::as_str).unwrap_or("");
        let (phase, activity) = (text("phase"), text("activity"));
        let working = activity == "busy";
        let ready = phase == "idle"
            && activity == "idle"
            && text("runtime_state") == "live"
            && text("process_state") == "running";
        let Some(composer) = hub.composers.get_mut(&key) else {
            return;
        };
        let Some(gave_up) = &composer.gave_up else {
            return;
        };
        if !gave_up.shown(&composer.notice, composer.error) {
            composer.gave_up = None;
            return;
        }
        let target = text("run_id") == gave_up.run
            && (gave_up.conversation.is_empty() || text("conversation_id") == gave_up.conversation);
        let moved = (gave_up.phase.as_str(), gave_up.activity.as_str()) != (phase, activity);
        if target && moved && (working || ready) {
            composer.gave_up = None;
            if !composer.uncertain {
                composer.notice.clear();
                composer.error = false;
            }
        }
    }

    /// Every reading of a continuation's session (an Activity state of the
    /// open session, an inspect, a poll): S3's `inspected`, the
    /// compaction's first, as Qt calls `continueAfterCompact` before
    /// `continueAfterStart` (3607).
    fn continue_with(&self, key: &str, details: &Arc<Value>) {
        if !details.as_object().is_some_and(|fields| !fields.is_empty()) {
            return;
        }
        let now = self.now();
        let (compact, start) = {
            let mut hub = self.lock();
            let session = hub
                .compacts
                .get(key)
                .map(|(p, _)| p.session.clone())
                .or_else(|| {
                    hub.start
                        .as_ref()
                        .filter(|(p, _)| p.key == key)
                        .map(|(p, _)| p.session.clone())
                });
            let Some(session) = session.filter(|_| !hub.closed) else {
                return;
            };
            let draft_now = Self::draft_mark(&mut hub, key);
            let write =
                (self.0.deps.facts)(&session.computer, &session.name, text(details, "run_id"));
            // Qt's `m_terminating` and `m_pending` (4618, 4787).
            let terminating = write == Some(WriteKind::Terminate);
            let updating = matches!(write, Some(WriteKind::Update | WriteKind::Settings));
            let compact = hub.compacts.get_mut(key).map(|(pending, compact)| {
                pending.evidence = Some(Arc::clone(details));
                let step = compact.inspected(details, &draft_now, terminating, now);
                (pending.revision, step)
            });
            let start = match hub.start.as_mut() {
                Some((pending, start)) if pending.key == key => {
                    pending.evidence = Some(Arc::clone(details));
                    let step = start.inspected(details, &draft_now, updating, terminating, now);
                    Some((pending.revision, step))
                }
                _ => None,
            };
            (compact, start)
        };
        if let Some((revision, step)) = compact {
            self.step(key, false, revision, step, Some(details));
        }
        if let Some((revision, step)) = start {
            self.step(key, true, revision, step, Some(details));
        }
    }

    /// The open session's followed details to its continuations.
    pub(crate) fn continue_open(&self) {
        let open = {
            let hub = self.lock();
            hub.open_key
                .clone()
                .and_then(|key| Self::open_details(&hub, &key).map(|details| (key, details)))
        };
        if let Some((key, details)) = open {
            self.continue_with(&key, &details);
        }
    }

    /// The continuations' limits (180 s for a start, 600 s for a
    /// compaction) and their 2.5 s poll (`pollCompactContinuations`,
    /// 4566-4590): a start always, a compaction once its request was
    /// accepted.
    fn tick_continuations(&self, now: f64) {
        let ended: Vec<(String, bool, u64, Step)> = {
            let hub = self.lock();
            hub.start
                .iter()
                .map(|(p, s)| (p.key.clone(), true, p.revision, s.tick(now)))
                .chain(
                    hub.compacts
                        .iter()
                        .map(|(key, (p, c))| (key.clone(), false, p.revision, c.tick(now))),
                )
                .filter(|(.., step)| *step != Step::Idle)
                .collect()
        };
        for (key, start, revision, step) in ended {
            self.step(&key, start, revision, step, None);
        }
        let due: Vec<(String, bool, u64)> = {
            let mut hub = self.lock();
            if hub.closed || now - hub.last_poll < POLL {
                return;
            }
            hub.last_poll = now;
            hub.start
                .iter()
                .map(|(p, _)| (p.key.clone(), true, p.revision))
                .chain(
                    hub.compacts
                        .iter()
                        .filter(|(_, (_, c))| c.polling())
                        .map(|(key, (p, _))| (key.clone(), false, p.revision)),
                )
                .collect()
        };
        for (key, start, revision) in due {
            self.poll(&key, start, revision);
        }
    }

    /// One reading of a continuation's session. The open one reads through
    /// `refresh_details`, so its details are the ones the send is checked
    /// against. A continuation polls even with a stream: its last state may
    /// predate resume readiness or compaction completion. Another session gets its own inspect,
    /// one at a time per continuation, whose details only the continuation
    /// sees. An offline computer is not read (4573, 4585).
    fn poll(&self, key: &str, start: bool, revision: u64) {
        let target = {
            let mut hub = self.lock();
            if hub.closed {
                return;
            }
            let open = hub.open_key.as_deref() == Some(key);
            let rows = &self.0.deps.rows;
            let Some(pending) = pending_mut(&mut hub, key, start, revision) else {
                return;
            };
            if open {
                None
            } else {
                let session = &pending.session;
                let online = rows(&session.computer, &session.key, &session.name)
                    .is_some_and(|(_, online)| online);
                if pending.polling || !online {
                    return;
                }
                pending.polling = true;
                Some(Target {
                    computer: session.computer.clone(),
                    name: session.name.clone(),
                    run_id: pending.run.clone(),
                    conversation_id: pending.conversation.clone(),
                })
            }
        };
        let Some(target) = target else {
            return self.read_details(true);
        };
        let me = self.clone();
        let owned = key.to_owned();
        (self.0.deps.spawn)(Box::new(move || {
            let read = (me.0.deps.inspect)(&target, None, None);
            if let Some(pending) = pending_mut(&mut me.lock(), &owned, start, revision) {
                pending.polling = false;
            }
            if let Ok(mut details) = read {
                if let Some(fields) = details.as_object_mut() {
                    fields.remove("events");
                }
                me.continue_with(&owned, &Arc::new(details));
                me.changed();
            }
        }));
    }

    /// Cancel send (`SessionsWindow.cpp:963-968`): the open session's
    /// waiting send ends and its draft unlocks. The start or compaction
    /// itself goes on; a delivery already under way is not a waiting send.
    pub fn cancel_send(&self, handle: u64) {
        self.cancel_open(handle, rules::CANCELLED_BY_USER);
    }

    /// The composer's Stop, before it interrupts the turn (1187-1191): a
    /// waiting send ends first. The Stop entry keeps Qt's other checks
    /// (`interrupt_supported`, one interrupt at a time).
    /// Returns the Stop's number for a later refusal ([`Self::interrupt_failed`]),
    /// 0 when nothing was kept.
    pub fn interrupted(&self, handle: u64) -> u64 {
        self.cancel_open(handle, rules::INTERRUPTED);
        self.remember_interrupted(handle)
    }

    /// As in Terminal, the prompt of the interrupted turn returns to the
    /// message field (`SessionsWindow.cpp:1192-1197`): the open session's
    /// turn prompt is kept now, bound to its run and conversation, and goes
    /// back once the turn reads interrupted (`restore_interrupted`).
    fn remember_interrupted(&self, handle: u64) -> u64 {
        let mut hub = self.lock();
        let Some(key) = hub.open_key.clone() else {
            return 0;
        };
        if hub
            .open
            .as_ref()
            .is_some_and(|open| open.subagent.is_some())
            || !hub.composers.get(&key).is_some_and(|c| c.handle == handle)
        {
            return 0;
        }
        let prompt = hub
            .followed
            .details
            .clone()
            .and_then(|details| super::interrupted::prompt_of(&details, hub.local.rows(&key)));
        let Some(mut prompt) = prompt else {
            hub.interrupted.set(&key, None);
            return 0;
        };
        hub.next_stop += 1;
        prompt.stop = hub.next_stop;
        let stop = prompt.stop;
        hub.interrupted.set(&key, Some(prompt));
        stop
    }

    /// The interrupt of Stop `stop` was refused: its turn goes on, and its
    /// prompt is not given back (Qt drops it when the request fails, 1204).
    /// Bound to that Stop, whichever session is open by then (zerus-btmir).
    pub fn interrupt_failed(&self, stop: u64) {
        if stop != 0 {
            self.lock().interrupted.refused(stop);
        }
    }

    fn cancel_open(&self, handle: u64, text: &str) {
        let (key, waiting) = {
            let hub = self.lock();
            let Some(key) = hub.open_key.clone() else {
                return;
            };
            if !hub.composers.get(&key).is_some_and(|c| c.handle == handle) {
                return;
            }
            let compact = hub.compacts.get(&key).map(|(p, _)| (false, p.revision));
            let start = hub
                .start
                .as_ref()
                .filter(|(p, _)| p.key == key)
                .map(|(p, _)| (true, p.revision));
            (key, [compact, start])
        };
        for (start, revision) in waiting.into_iter().flatten() {
            self.cancel(&key, start, revision, text);
        }
    }

    /// Clear session confirmed (`clearContext` 4638-4670): the identity the
    /// dialog showed (run, conversation, the dismiss variant) must still
    /// hold, as Qt checks after its dialog (4660-4664), and no reset runs
    /// anywhere. Blocking: it answers with Qt's notice once the call ended
    /// (1021-1028), while the session is still open; `Sending` when another
    /// session is open by then, as Qt shows nothing.
    pub fn clear_context(
        &self,
        handle: u64,
        run: &str,
        conversation: &str,
        dismiss: bool,
    ) -> SendReply {
        self.clear_context_using(handle, run, conversation, dismiss, |target, action, id| {
            (self.0.deps.dispatch)(target, action, id)
        })
    }

    pub(crate) fn clear_context_using(
        &self,
        handle: u64,
        run: &str,
        conversation: &str,
        dismiss: bool,
        dispatch: impl FnOnce(Target, Action, String) -> Result<Outcome, String>,
    ) -> SendReply {
        let (key, target) = {
            let mut hub = self.lock();
            let Some(ctx) = self.context(&hub) else {
                return SendReply::Busy;
            };
            if hub.closed
                || ctx.session.subagent.is_some()
                || !hub
                    .composers
                    .get(&ctx.key)
                    .is_some_and(|c| c.handle == handle)
            {
                return SendReply::Busy;
            }
            let details = ctx.details.as_deref().unwrap_or(&Value::Null);
            let control = clear_control(&hub, &ctx, details);
            if !control.enabled
                || control.run != run
                || control.conversation != conversation
                || control.dismiss != dismiss
            {
                return SendReply::Busy;
            }
            hub.clearing = Some(ctx.key.clone());
            let target = Target {
                computer: ctx.session.computer.clone(),
                name: ctx.session.name.clone(),
                run_id: control.run,
                conversation_id: control.conversation,
            };
            (ctx.key, target)
        };
        self.finish_clear(key, target, dismiss, dispatch)
    }

    /// A delayed confirmation belongs to its captured session, independent of
    /// the currently selected composer. Inspect its own current control again;
    /// the dispatcher still checks the captured identity at the write boundary.
    pub(crate) fn clear_captured_using(
        &self,
        captured: &zerus_client_core::end_action::Captured,
        dispatch: impl FnOnce(Target, Action, String) -> Result<Outcome, String>,
    ) -> SendReply {
        let Action::ClearContext { dismiss_prompt } = captured.action else {
            return SendReply::Busy;
        };
        if !captured.valid() {
            return SendReply::Busy;
        }
        let session = SessionRef {
            computer: captured.target.computer.clone(),
            key: format!("run:{}", captured.target.run_id),
            name: captured.target.name.clone(),
            ..SessionRef::default()
        };
        // The capture names no durable ID: take the listed row's, so the key
        // is the open composer's own `id:` draft key, not its name key
        // (zerus-fsht5). With the name key the sending check missed it, its
        // view never showed the clear running, and the clear answered as if
        // the session had closed.
        let session_id = (self.0.deps.rows)(&session.computer, &session.key, &session.name)
            .and_then(|(row, _)| row.session_id);
        let session = SessionRef {
            session_id,
            ..session
        };
        let Some(control) = self.clear_control_for(session.clone()) else {
            return SendReply::Busy;
        };
        let key = super::session_draft_key(&session);
        {
            let mut hub = self.lock();
            if hub.closed
                || hub.clearing.is_some()
                || hub.composers.get(&key).is_some_and(|c| c.sending)
                || !control.enabled
                || control.run != captured.target.run_id
                || control.conversation != captured.target.conversation_id
                || control.dismiss != dismiss_prompt
            {
                return SendReply::Busy;
            }
            hub.clearing = Some(key.clone());
        }
        self.finish_clear(key, captured.target.clone(), dismiss_prompt, dispatch)
    }

    fn finish_clear(
        &self,
        key: String,
        target: Target,
        dismiss: bool,
        dispatch: impl FnOnce(Target, Action, String) -> Result<Outcome, String>,
    ) -> SendReply {
        self.changed();
        let action = Action::ClearContext {
            dismiss_prompt: dismiss,
        };
        let (computer, name, run) = (
            target.computer.clone(),
            target.name.clone(),
            target.run_id.clone(),
        );
        let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch(target, action, uuid::Uuid::new_v4().to_string())
        }));
        // A taken lane refused before anything ran: the block reason Qt
        // would show for that write (`messageBlockReason`, 4391 and 4395).
        let refused = if (self.0.deps.facts)(&computer, &name, &run) == Some(WriteKind::Terminate) {
            TERMINATING
        } else {
            UPDATING
        };
        let outcome = outcome_of(called, refused);
        let open = {
            let mut hub = self.lock();
            hub.clearing = None;
            hub.open_key.as_deref() == Some(key.as_str()) && !hub.closed
        };
        self.changed();
        if !open {
            return SendReply::Sending;
        }
        // Qt reads the session again (`inspect()`, 1026).
        self.refresh_details();
        match outcome {
            Outcome::Done { receipt, .. } => SendReply::Notice {
                text: if text(&receipt, "status") == "confirmed" {
                    CLEARED
                } else {
                    CLEAR_SENT
                }
                .into(),
                error: false,
            },
            Outcome::Failed { detail } | Outcome::Uncertain { detail } => SendReply::Notice {
                text: detail,
                error: true,
            },
        }
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn send_message(
    state: State<'_, Composers>,
    handle: u64,
    rev: u64,
    epoch: u64,
    text: String,
    position: i64,
    anchor: i64,
) -> Result<SendReply, String> {
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        composers.send_message(handle, rev, epoch, text, position, anchor)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Off the main thread: Compact and continue hashes the draft's files
/// (at most 20 MiB) the first time.
#[tauri::command]
pub async fn send_answer(
    state: State<'_, Composers>,
    token: u64,
    answer: Answer,
) -> Result<SendReply, String> {
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || composers.answer(token, answer))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn local_retry(
    state: State<'_, Composers>,
    handle: u64,
    id: String,
) -> Result<SendReply, String> {
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || composers.retry(handle, &id))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn local_delete(state: State<'_, Composers>, handle: u64, id: String) {
    state.delete_local(handle, &id);
}

#[tauri::command]
pub fn local_inspect(state: State<'_, Composers>, handle: u64, id: String) -> Option<Inspect> {
    state.inspect_local(handle, &id)
}

#[tauri::command]
pub fn send_cancel(state: State<'_, Composers>, handle: u64) {
    state.cancel_send(handle);
}

#[tauri::command]
pub fn send_interrupted(state: State<'_, Composers>, handle: u64) -> u64 {
    state.interrupted(handle)
}

/// The interrupt of the Stop `send_interrupted` numbered was refused.
#[tauri::command]
pub fn send_interrupt_failed(state: State<'_, Composers>, stop: u64) {
    state.interrupt_failed(stop);
}

/// Blocking: it answers once the reset ended (`Composers::clear_context`).
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_context(
    app: tauri::AppHandle,
    state: State<'_, Composers>,
    handle: u64,
    run: String,
    conversation: String,
    dismiss: bool,
    delay: Option<crate::action_delay::Reference>,
) -> Result<SendReply, String> {
    use tauri::Manager;
    let delayed = delay
        .as_ref()
        .map(|reference| {
            app.state::<crate::action_delay::Delays>().take_clear(
                reference,
                &run,
                &conversation,
                dismiss,
            )
        })
        .transpose()?;
    let actions = app.state::<crate::actions::Actions>().inner().clone();
    let machine = delayed
        .as_ref()
        .map(|(captured, _)| {
            crate::worktree_cleanup::machine(
                &app.state::<crate::commands::AppState>(),
                &captured.target.computer,
            )
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let composers = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some((captured, claim)) = delayed {
            let confirmation = captured.clone();
            composers.clear_captured_using(&confirmation, move |target, action, request_id| {
                if captured.target == target && captured.action == action {
                    tauri::async_runtime::block_on(crate::actions::dispatch_delayed(
                        &actions, captured, claim, machine, request_id,
                    ))
                } else {
                    Err(zerus_client_core::end_action::STATE_CHANGED.into())
                }
            })
        } else {
            composers.clear_context(handle, &run, &conversation, dismiss)
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::composer::tests::{dir, drafts, hold, session, until, Scratch, KEY};
    use crate::composer::{
        lock, ComposerView, Composers, Config, Deps, Job, Rows, Spawn, SubagentRef,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use zerus_client_core::drafts::DraftStore;
    use zerus_client_core::{ActivityOut, Batch, ComputerId, RowOut};

    const OFFLINE: &str =
        "Machine unavailable. Showing the last known state; actions will return when it reconnects.";

    /// What the fake `dispatch` answers a call with.
    type Answers = Box<dyn Fn(&Action, &str) -> Outcome + Send>;
    /// Which calls the fake `dispatch` finds their lane taken for.
    type Taken = Box<dyn Fn(&Action) -> bool + Send>;

    /// The session, clock, inspect and calls a test controls.
    pub(crate) struct Fake {
        pub(crate) row: Mutex<ViewRow>,
        pub(crate) listed: Mutex<bool>,
        pub(crate) now: Mutex<f64>,
        pub(crate) inspected: Mutex<Option<Value>>,
        pub(crate) inspects: AtomicUsize,
        pub(crate) inspected_targets: Mutex<Vec<Target>>,
        reconnects: Mutex<Vec<String>>,
        pub(crate) online: Mutex<bool>,
        pub(crate) write: Mutex<Option<WriteKind>>,
        pub(crate) calls: Mutex<Vec<(Target, Action, String)>>,
        pub(crate) outcome: Mutex<Answers>,
        /// The calls whose lane is taken: `dispatch` refuses them with
        /// `actions::BUSY` before anything runs, as `dispatch_with` does.
        pub(crate) taken: Mutex<Taken>,
    }

    pub(crate) fn row(name: &str) -> ViewRow {
        ViewRow {
            name: name.into(),
            agent: "claude".into(),
            state: "running".into(),
            tracked: true,
            run_id: Some("run-1".into()),
            conversation_id: Some("conv-1".into()),
            ..ViewRow::default()
        }
    }

    pub(crate) fn send_hub_on(store: &Arc<DraftStore>) -> (Composers, Arc<Fake>) {
        send_hub_with(
            store,
            Arc::new(|job: Job| {
                std::thread::spawn(job);
            }),
        )
    }

    /// `send_hub_on` with the test's own `spawn`.
    pub(crate) fn send_hub_with(store: &Arc<DraftStore>, spawn: Spawn) -> (Composers, Arc<Fake>) {
        send_hub_reconnecting(store, spawn, None, None)
    }

    fn send_hub_reconnecting(
        store: &Arc<DraftStore>,
        spawn: Spawn,
        reconnect: Option<super::super::Reconnect>,
        rows: Option<Rows>,
    ) -> (Composers, Arc<Fake>) {
        let fake = Arc::new(Fake {
            row: Mutex::new(row("claude/sample-project/review")),
            listed: Mutex::new(true),
            now: Mutex::new(1_793_000_100.0),
            inspected: Mutex::new(None),
            inspects: AtomicUsize::new(0),
            inspected_targets: Mutex::new(Vec::new()),
            reconnects: Mutex::new(Vec::new()),
            online: Mutex::new(true),
            write: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            outcome: Mutex::new(Box::new(receipt)),
            taken: Mutex::new(Box::new(|_| false)),
        });
        let (f1, f2, f3, f4) = (fake.clone(), fake.clone(), fake.clone(), fake.clone());
        let f5 = fake.clone();
        let f6 = fake.clone();
        let composers = Composers::new(
            Arc::clone(store),
            Config {
                cache: store.root().with_file_name("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn,
                notify: Arc::new(|| {}),
                clock: Arc::new(move || *lock(&f1.now)),
                rows: rows.unwrap_or_else(|| {
                    Arc::new(move |_, _, _| {
                        if *lock(&f2.listed) {
                            Some((lock(&f2.row).clone(), *lock(&f2.online)))
                        } else {
                            None
                        }
                    })
                }),
                facts: Arc::new(move |_, _, _| *lock(&f4.write)),
                reconnect: reconnect.unwrap_or_else(|| {
                    Arc::new(move |computer| {
                        lock(&f6.reconnects).push(computer.to_owned());
                    })
                }),
                inspect: Arc::new(move |target, _, _| {
                    f3.inspects.fetch_add(1, Ordering::SeqCst);
                    lock(&f3.inspected_targets).push(target.clone());
                    lock(&f3.inspected)
                        .clone()
                        .ok_or_else(|| "timed out".to_owned())
                }),
                dispatch: Arc::new(move |target, action, id| {
                    if (lock(&f5.taken))(&action) {
                        return Err(crate::actions::BUSY.into());
                    }
                    let out = (lock(&f5.outcome))(&action, &id);
                    lock(&f5.calls).push((target, action, id));
                    Ok(out)
                }),
            },
        );
        (composers, fake)
    }

    pub(crate) fn send_hub(root: &Scratch) -> (Composers, Arc<Fake>) {
        send_hub_on(&drafts(root))
    }

    pub(crate) fn send_hub_on_rows(root: &Scratch, rows: Rows) -> (Composers, Arc<Fake>) {
        send_hub_reconnecting(
            &drafts(root),
            Arc::new(|job: Job| {
                std::thread::spawn(job);
            }),
            None,
            Some(rows),
        )
    }

    pub(crate) fn ready() -> Value {
        json!({"tracked": true, "run_id": "run-1", "conversation_id": "conv-1", "runtime_state": "live", "process_state": "running",
               "phase": "idle", "activity": "idle", "compact_context_supported": true, "clear_context_supported": true})
    }

    fn child_session() -> SessionRef {
        SessionRef {
            subagent: Some(SubagentRef {
                agent_id: "agent-7".into(),
                parent_conversation_id: "conv-1".into(),
            }),
            ..session()
        }
    }

    fn child_ready() -> Value {
        let mut details = ready();
        details["agent_id"] = json!("agent-7");
        details["parent_conversation_id"] = json!("conv-1");
        details["conversation_id"] = json!("conv-1/agent-7");
        details["send_supported"] = json!(true);
        details["history_scope"] = json!("Native subagent history");
        details
    }

    pub(crate) fn state(details: Value) -> Batch {
        Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "state".into(),
                payload: json!({"value": details, "rev": 1}),
            }],
            ..Batch::default()
        }
    }

    pub(crate) fn opened(c: &Composers, details: Value) -> ComposerView {
        c.open(Some(session()));
        c.observe(&state(details));
        until(c, |v| v.loaded)
    }

    fn archive_session() -> SessionRef {
        SessionRef {
            key: "archive:10000000-0000-4000-8000-000000000001".into(),
            archive_id: Some("10000000-0000-4000-8000-000000000001".into()),
            ..session()
        }
    }

    #[test]
    fn an_archived_draft_stays_editable_and_separate_from_a_live_session_of_its_name() {
        let root = dir("archive-composer-draft");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let live = opened(&c, ready());
        c.edit(live.handle, 1, live.text_epoch, "live draft".into(), 10, 10);
        let archived = archive_session();
        c.open(Some(archived.clone()));
        let view = until(&c, |v| v.loaded);
        assert!(view.text.is_empty());
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Resume the session to send a message.")
        );
        assert!(!view.available && !view.can_send && !view.session_blocks_input);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "archive draft".into(),
            13,
            4,
        );
        let view = c.current().unwrap();
        assert_eq!(
            (view.text.as_str(), view.position, view.anchor),
            ("archive draft", 13, 4)
        );
        assert!(matches!(
            c.send_message(view.handle, 2, view.text_epoch, view.text.clone(), 13, 4),
            SendReply::Blocked { .. }
        ));
        assert!(lock(&fake.calls).is_empty());
        c.open(Some(session()));
        assert_eq!(until(&c, |v| v.loaded).text, "live draft");
        c.flush();
        store.flush();
        let (restarted, _) = send_hub_on(&store);
        restarted.open(Some(archived));
        let view = until(&restarted, |v| v.loaded);
        assert_eq!(
            (view.text.as_str(), view.position, view.anchor),
            ("archive draft", 13, 4)
        );
        assert!(!view.can_send && !view.session_blocks_input);
    }

    #[test]
    fn an_archived_composer_never_uses_live_details_or_inspects_its_name() {
        let root = dir("archive-composer-details");
        let jobs: Arc<Mutex<Vec<Job>>> = Default::default();
        let queued = Arc::clone(&jobs);
        let (c, fake) = send_hub_with(&drafts(&root), Arc::new(move |job| lock(&queued).push(job)));
        let mut details = ready();
        details["input_queue"] =
            json!({"id": "live-queue", "messages": [{"text": "live message"}]});
        opened(&c, details.clone());
        *lock(&fake.inspected) = Some(details.clone());
        *lock(&fake.write) = Some(WriteKind::Terminate);
        c.open(Some(archive_session()));
        let view = until(&c, |v| v.loaded);
        assert!(
            !view.session_blocks_input,
            "a live same-name write does not lock the archived draft"
        );
        for _ in 0..3 {
            c.observe(&state(details.clone()));
            *lock(&fake.now) += 10.0;
            c.tick();
            c.refresh(view.handle);
        }
        {
            let hub = c.lock();
            assert!(c.context(&hub).unwrap().details.is_none());
            assert!(!c.wants_details(&hub, *lock(&fake.now)));
        }
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
        assert!(
            lock(&jobs).is_empty(),
            "no composer inspection is even scheduled"
        );
        // Even a state frame addressed to the archive cannot lend it a live queue.
        let mut addressed = state(details);
        addressed.activity[0].key = archive_session().key;
        c.observe(&addressed);
        assert!(c.context(&c.lock()).unwrap().details.is_none());
        let view = c.current().unwrap();
        assert!(view.question.is_none() && view.queue.is_none() && !view.clear.enabled);
        assert!(!c.send_queue_now(None));
        assert!(lock(&fake.calls).is_empty());
        *lock(&fake.online) = false;
        assert_eq!(
            c.current().unwrap().block_reason.as_deref(),
            Some("Machine unavailable. Your draft stays here.")
        );
    }

    #[test]
    fn an_archive_identity_refuses_send_with_either_key_or_id() {
        let root = dir("archive-composer-key");
        let (c, fake) = send_hub(&root);
        let mut key_only = archive_session();
        key_only.archive_id = None;
        let mut id_only = archive_session();
        id_only.key = session().key;
        for archived in [key_only, id_only] {
            c.open(Some(archived));
            let view = until(&c, |v| v.loaded);
            assert_eq!(
                view.block_reason.as_deref(),
                Some("Resume the session to send a message.")
            );
            assert!(matches!(
                c.send_push(
                    view.handle,
                    1,
                    view.text_epoch,
                    "keep this draft".into(),
                    15,
                    15
                ),
                SendReply::Busy
            ));
            assert!(lock(&fake.calls).is_empty());
            assert!(c.lock().queue.pusher.active().is_none());
        }
    }

    #[test]
    fn an_archived_composer_opens_without_a_live_catalog_row() {
        let root = dir("archive-composer-no-live-row");
        let store = drafts(&root);
        assert!(crate::composer::archived_row("run:run-1", &session().name).is_none());
        let c = Composers::new(
            Arc::clone(&store),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, key, name| {
                    crate::composer::archived_row(key, name).map(|row| (row, true))
                }),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| panic!("archived composer must not inspect")),
                dispatch: Arc::new(|_, _, _| panic!("archived composer must not dispatch")),
            },
        );
        c.open(Some(archive_session()));
        let view = until(&c, |v| v.loaded);
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Resume the session to send a message.")
        );
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "archive without a live row".into(),
            25,
            25,
        );
        assert_eq!(c.current().unwrap().text, "archive without a live row");
        c.refresh(view.handle);
        c.tick();
    }

    #[test]
    fn a_ready_session_takes_messages() {
        let root = dir("ready");
        let (c, _) = send_hub(&root);
        let view = opened(&c, ready());
        assert!(
            view.available && view.block_reason.is_none() && view.unavailable_reason.is_empty()
        );
        let handle = view.handle;
        c.edit(handle, 1, view.text_epoch, "hello".into(), 5, 5);
        assert!(c.current().unwrap().can_send);
    }

    #[test]
    fn a_block_reason_is_shown() {
        let root = dir("blocked");
        let (c, _) = send_hub(&root);
        let mut details = ready();
        details["phase"] = json!("approval");
        let view = opened(&c, details);
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Respond to the agent in the Terminal tab.")
        );
        assert_eq!(
            view.unavailable_reason,
            "Respond to the agent in the Terminal tab."
        );
        assert!(!view.available);
        c.edit(view.handle, 1, view.text_epoch, "hi".into(), 2, 2);
        let view = c.current().unwrap();
        assert!(!view.can_send);
        assert_eq!(view.status, "Respond to the agent in the Terminal tab.");
    }

    #[test]
    fn the_cache_chip_counts_down_in_minutes() {
        let root = dir("chip");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["session_usage"] =
            json!({"prompt_cache": {"status": "warm", "expires_at": 1_793_000_100.0 + 540.0}});
        let view = opened(&c, details);
        assert_eq!(
            (view.cache.label.as_str(), view.cache.short_label.as_str()),
            ("Cache ~9m", "~9m")
        );
        assert!(view
            .cache_details
            .starts_with("Estimated expiry from the TTL reported in native usage: "));
        *lock(&fake.now) += 240.0;
        c.tick();
        assert_eq!(c.current().unwrap().cache.label, "Cache ~5m");
    }

    #[test]
    fn without_a_state_the_session_is_inspected_once() {
        let root = dir("inspect");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        assert_eq!(view.block_reason.as_deref(), Some("Checking the session…"));
        *lock(&fake.now) += 3.5;
        c.tick();
        until(&c, |v| v.block_reason.is_none());
        c.tick();
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_fallback_waits_three_seconds_after_opening() {
        let root = dir("inspect-wait");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&fake.now) += 2.9;
        c.tick();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
        *lock(&fake.now) += 0.2;
        c.tick();
        until(&c, |v| v.block_reason.is_none());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_state_after_a_failed_inspect_clears_its_reason() {
        let root = dir("inspect-failed");
        let (c, fake) = send_hub(&root);
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&fake.now) += 3.5;
        c.tick();
        until(&c, |v| {
            v.block_reason.as_deref() == Some("Refresh the session before sending.")
        });
        c.observe(&state(ready()));
        assert!(c.current().unwrap().block_reason.is_none());
    }

    /// Waits until `done` holds, within five seconds.
    pub(crate) fn eventually(done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_chip_changes_when_its_minute_does_not_on_the_clocks() {
        let root = dir("chip-boundary");
        let (c, fake) = send_hub(&root);
        let start = *lock(&fake.now);
        // Ten minutes from a time that is not on a whole minute.
        let mut details = ready();
        details["session_usage"] =
            json!({"prompt_cache": {"status": "warm", "expires_at": start + 545.0}});
        assert_eq!(opened(&c, details).cache.label, "Cache ~10m");
        assert!(c.take_view().is_some());
        c.tick();
        assert!(c.take_view().is_none(), "the same minute wakes nothing");
        *lock(&fake.now) = start + 5.0;
        c.tick();
        assert_eq!(c.take_view().unwrap().cache.label, "Cache ~9m");
        *lock(&fake.now) = start + 544.0;
        c.tick();
        assert_eq!(c.take_view().unwrap().cache.label, "Cache ~1m");
        *lock(&fake.now) = start + 545.0;
        c.tick();
        let view = c.take_view().unwrap();
        assert_eq!(view.cache.label, "Cold cache");
        assert_eq!(
            view.cache_warning,
            "The estimated cache lifetime has ended. Continuing may rebuild the cached context."
        );
    }

    #[test]
    fn a_tick_without_an_open_composer_wakes_nothing() {
        let root = dir("tick-closed");
        let store = drafts(&root);
        let woken = Arc::new(AtomicUsize::new(0));
        let count = woken.clone();
        let now = Arc::new(Mutex::new(1_793_000_100.0));
        let clock = now.clone();
        let c = Composers::new(
            Arc::clone(&store),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
                clock: Arc::new(move || *lock(&clock)),
                rows: Arc::new(|_, _, _| Some((row("claude/sample-project/review"), true))),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| panic!("nothing is open to inspect")),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        // The stored draft keys' first answer wakes the pump once.
        eventually(|| woken.load(Ordering::SeqCst) > 0);
        let before = woken.load(Ordering::SeqCst);
        for minute in 1..5 {
            *lock(&now) += 60.0 * f64::from(minute);
            c.tick();
        }
        assert_eq!(woken.load(Ordering::SeqCst), before);
    }

    #[test]
    fn the_ticker_sleeps_while_no_composer_is_open() {
        let root = dir("ticker");
        let store = drafts(&root);
        let clocks = Arc::new(AtomicUsize::new(0));
        let counted = clocks.clone();
        let c = Composers::new(
            Arc::clone(&store),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    1_793_000_100.0
                }),
                rows: Arc::new(|_, _, _| None),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        let parked = || c.0.parked.load(Ordering::SeqCst);
        let ticker = c.start_ticker_every(Duration::from_millis(5));
        eventually(parked);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(clocks.load(Ordering::SeqCst), 0, "nothing open: no tick");
        c.open(Some(session()));
        let opened = clocks.load(Ordering::SeqCst);
        eventually(|| clocks.load(Ordering::SeqCst) > opened + 3);
        c.open(None);
        // Parked again: from here on the ticker reads no clock.
        eventually(parked);
        let closed = clocks.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            clocks.load(Ordering::SeqCst),
            closed,
            "closed again: no tick"
        );
        c.flush();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            ticker.join().unwrap();
            let _ = done.send(());
        });
        ended
            .recv_timeout(Duration::from_secs(5))
            .expect("the ticker ends when the app quits");
    }

    #[test]
    fn nothing_is_inspected_while_the_computer_is_offline() {
        let root = dir("inspect-offline");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        *lock(&fake.online) = false;
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&fake.now) += 10.0;
        c.tick();
        c.refresh_details();
        assert!(c.lock().followed.inspecting.is_none(), "none started");
        std::thread::sleep(Duration::from_millis(50));
        let view = c.current().unwrap();
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Machine unavailable. Your draft stays here.")
        );
        assert!(view.cache_details.ends_with("Last recorded session data."));
        assert_eq!(view.status, OFFLINE);
        assert!(view.status_error && view.session_blocks_input);
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
        *lock(&fake.online) = true;
        c.tick();
        until(&c, |v| v.block_reason.is_none());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_inspect_is_retried_and_a_read_is_repeated_on_a_change() {
        let root = dir("inspect-again");
        let (c, fake) = send_hub(&root);
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        // The n-th inspect has answered.
        let reads = |n| {
            eventually(|| {
                fake.inspects.load(Ordering::SeqCst) == n && c.lock().followed.inspecting.is_none()
            })
        };
        let wait = |seconds: f64| {
            *lock(&fake.now) += seconds;
            c.tick();
        };
        wait(3.5);
        reads(1);
        let view = c.current().unwrap();
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Refresh the session before sending.")
        );
        assert_eq!(view.status, "Could not refresh activity: timed out");
        // Qt polls every 2.5 s while nothing streams.
        wait(1.0);
        wait(1.0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1, "not before 2.5 s");
        wait(0.5);
        reads(2);
        *lock(&fake.inspected) = Some(ready());
        wait(2.5);
        reads(3);
        until(&c, |v| v.block_reason.is_none() && v.status.is_empty());
        // A read is repeated only when the row's run, conversation or state changed.
        wait(10.0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fake.inspects.load(Ordering::SeqCst),
            3,
            "a success is not polled"
        );
        lock(&fake.row).run_id = Some("run-2".into());
        wait(0.1);
        reads(4);
        lock(&fake.row).conversation_id = Some("conv-2".into());
        wait(0.1);
        reads(5);
        lock(&fake.row).state = "paused".into();
        wait(0.1);
        reads(6);
        for _ in 0..3 {
            wait(1.0);
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 6);
    }

    /// Behind Terminal or Native UI a failed read waits Qt's background
    /// interval (`SessionsWindow.cpp:2013-2016`); back on Activity, 2.5 s.
    #[test]
    fn behind_terminal_a_failed_inspect_waits_the_background_interval() {
        let root = dir("inspect-background");
        let (c, fake) = send_hub(&root);
        let settings = |name: &str| match name {
            "processes/backgroundSeconds" => 30.0,
            "processes/activeSeconds" => 2.5,
            _ => 0.0,
        };
        c.set_poll(poll_seconds("terminal", settings));
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        let reads = |n| {
            eventually(|| {
                fake.inspects.load(Ordering::SeqCst) == n && c.lock().followed.inspecting.is_none()
            })
        };
        let wait = |seconds: f64| {
            *lock(&fake.now) += seconds;
            c.tick();
        };
        wait(3.5);
        reads(1);
        wait(2.5);
        wait(10.0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1, "not before 30 s");
        wait(17.5);
        reads(2);
        c.set_poll(poll_seconds("activity", settings));
        wait(2.5);
        reads(3);
    }

    #[test]
    fn the_tab_picks_qts_poll_interval() {
        let settings = |name: &str| match name {
            "processes/backgroundSeconds" => 45.0,
            "processes/activeSeconds" => 1.5,
            _ => 0.0,
        };
        assert_eq!(poll_seconds("terminal", settings), Some(45.0));
        assert_eq!(poll_seconds("native", settings), Some(45.0));
        assert_eq!(poll_seconds("processes", settings), Some(1.5));
        assert_eq!(poll_seconds("activity", settings), None);
        let root = dir("poll-bounds");
        let (c, _) = send_hub(&root);
        c.set_poll(Some(30.0));
        assert_eq!(c.lock().poll, Some(30.0));
        for odd in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            c.set_poll(Some(odd));
            assert_eq!(c.lock().poll, None, "{odd}");
        }
    }

    /// Refresh reads the open session, and only for its own composer.
    #[test]
    fn offline_refresh_nudges_only_its_own_computer_without_inspecting() {
        let root = dir("refresh-offline");
        let (c, fake) = send_hub(&root);
        *lock(&fake.online) = false;
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.refresh(view.handle + 1);
        assert!(lock(&fake.reconnects).is_empty());
        c.refresh(view.handle);
        assert_eq!(*lock(&fake.reconnects), ["studio"]);
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
        c.open(None);
        c.refresh(view.handle);
        assert_eq!(*lock(&fake.reconnects), ["studio"]);
    }

    #[test]
    fn offline_refresh_after_the_composer_closes_does_nothing() {
        let root = dir("refresh-closed");
        let (c, fake) = send_hub(&root);
        *lock(&fake.online) = false;
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.flush();
        c.refresh(view.handle);
        assert!(lock(&fake.reconnects).is_empty());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn offline_refresh_keeps_its_computer_after_unlocking_and_a_selection_change() {
        let root = dir("refresh-captured");
        let (entered, received) = std::sync::mpsc::sync_channel(1);
        let (release, held) = std::sync::mpsc::sync_channel(1);
        let held = Mutex::new(held);
        let (c, fake) = send_hub_reconnecting(
            &drafts(&root),
            Arc::new(|job| {
                std::thread::spawn(job);
            }),
            Some(Arc::new(move |computer| {
                entered.send(computer.to_owned()).unwrap();
                lock(&held).recv_timeout(Duration::from_secs(2)).unwrap();
            })),
            None,
        );
        *lock(&fake.online) = false;
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        let refreshing = c.clone();
        let thread = std::thread::spawn(move || refreshing.refresh(view.handle));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            "studio"
        );
        c.open(Some(SessionRef {
            computer: "other".into(),
            ..session()
        }));
        release.send(()).unwrap();
        thread.join().unwrap();
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn refresh_reads_the_open_session_for_its_composer_only() {
        let root = dir("refresh");
        let (c, fake) = send_hub(&root);
        c.open(Some(session()));
        let view = until(&c, |v| v.loaded);
        c.refresh(view.handle + 1);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0, "another handle");
        c.refresh(view.handle);
        eventually(|| {
            fake.inspects.load(Ordering::SeqCst) == 1 && c.lock().followed.inspecting.is_none()
        });
        let view = until(&c, |v| v.session_status_error);
        assert_eq!(view.session_status, "Could not refresh activity: timed out");
        assert!(view.inspect_error, "the read error is in the view");
        *lock(&fake.inspected) = Some(ready());
        c.refresh(view.handle);
        let view = until(&c, |v| v.block_reason.is_none() && !v.session_status_error);
        assert!(!view.inspect_error);
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 2);
        // Nothing is read while the stream keeps the details.
        c.observe(&state(ready()));
        c.refresh(view.handle);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 2, "streamed");
        assert!(
            lock(&fake.reconnects).is_empty(),
            "online behavior unchanged"
        );
    }

    /// R1's Context chip: the reported context size as a share of its window.
    #[test]
    fn the_context_chip_shows_the_reported_usage() {
        let root = dir("context-chip");
        let (c, _) = send_hub(&root);
        let mut details = ready();
        details["session_usage"] = json!({"context": {"used": 85_300, "limit": 200_000}});
        let view = opened(&c, details);
        assert!(view.context.visible);
        assert_eq!(view.context.text, "43% (85.3k/200k)");
        let shown = serde_json::to_value(&view).unwrap();
        assert_eq!(shown["context"]["tone"], "success");
        assert_eq!(shown["context"]["visible"], true);
        let root = dir("context-chip-none");
        let (c, _) = send_hub(&root);
        let view = opened(&c, ready());
        assert!(!view.context.visible && view.context.text.is_empty());
    }

    #[test]
    fn the_stream_keeps_the_details_and_an_end_inspects() {
        let root = dir("inspect-streamed");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        opened(&c, ready());
        *lock(&fake.now) += 10.0;
        lock(&fake.row).state = "paused".into();
        c.tick();
        c.refresh_details();
        assert!(c.lock().followed.inspecting.is_none(), "none started");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fake.inspects.load(Ordering::SeqCst),
            0,
            "Qt reads nothing while streamed"
        );
        lock(&fake.row).state = "running".into();
        c.observe(&Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "ended".into(),
                payload: Value::Null,
            }],
            ..Batch::default()
        });
        assert!(
            c.current().unwrap().block_reason.is_none(),
            "the details stay, as in Qt"
        );
        c.tick();
        reads_settle(&c, &fake, 1);
        assert!(c.current().unwrap().block_reason.is_none());
    }

    /// The n-th inspect has answered.
    fn reads_settle(c: &Composers, fake: &Fake, n: usize) {
        eventually(|| {
            fake.inspects.load(Ordering::SeqCst) == n && c.lock().followed.inspecting.is_none()
        });
    }

    pub(crate) fn ended() -> Batch {
        Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "ended".into(),
                payload: Value::Null,
            }],
            ..Batch::default()
        }
    }

    #[test]
    fn an_end_after_states_reads_the_session_again() {
        let root = dir("inspect-end");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&fake.now) += 3.5;
        c.tick();
        reads_settle(&c, &fake, 1);
        c.observe(&state(ready()));
        c.observe(&state(ready()));
        // The row is unchanged: only the end says to read again.
        c.observe(&ended());
        *lock(&fake.now) += 0.1;
        c.tick();
        reads_settle(&c, &fake, 2);
        let view = c.current().unwrap();
        assert!(view.block_reason.is_none(), "{view:?}");
    }

    /// The composers with an inspect that waits for the test's word before it
    /// answers with `answer(n)` (n counts from 1) .
    fn gated(
        root: &Scratch,
        answer: fn(usize) -> Value,
    ) -> (
        Composers,
        Arc<Mutex<f64>>,
        std::sync::mpsc::Sender<()>,
        Arc<AtomicUsize>,
    ) {
        let now = Arc::new(Mutex::new(1_793_000_100.0));
        let clock = now.clone();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let answered = Arc::new(AtomicUsize::new(0));
        let counted = answered.clone();
        let c = Composers::new(
            drafts(root),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(move || *lock(&clock)),
                rows: Arc::new(|_, _, name| Some((row(name), true))),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(move |_, _, _| {
                    let _ = lock(&gate).recv_timeout(Duration::from_secs(5));
                    Ok(answer(counted.fetch_add(1, Ordering::SeqCst) + 1))
                }),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        (c, now, release, answered)
    }

    fn approval() -> Value {
        let mut details = ready();
        details["phase"] = json!("approval");
        details
    }

    #[test]
    fn a_late_inspect_never_overwrites_a_newer_state() {
        let root = dir("inspect-newer");
        let (c, now, release, answered) = gated(&root, |_| ready());
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&now) += 3.5;
        c.tick();
        assert!(c.lock().followed.inspecting.is_some());
        c.observe(&state(approval()));
        release.send(()).unwrap();
        eventually(|| {
            answered.load(Ordering::SeqCst) == 1 && c.lock().followed.inspecting.is_none()
        });
        assert_eq!(
            c.current().unwrap().block_reason.as_deref(),
            Some("Respond to the agent in the Terminal tab."),
            "the state frame is newer than the inspect"
        );
    }

    #[test]
    fn an_end_drops_the_inspect_under_way() {
        let root = dir("inspect-ended");
        let (c, now, release, answered) =
            gated(&root, |n| if n == 1 { ready() } else { approval() });
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&now) += 3.5;
        c.tick();
        assert!(c.lock().followed.inspecting.is_some());
        c.observe(&state(approval()));
        c.observe(&ended());
        assert!(c.lock().followed.inspecting.is_none());
        release.send(()).unwrap();
        eventually(|| answered.load(Ordering::SeqCst) == 1);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            c.current().unwrap().block_reason.as_deref(),
            Some("Respond to the agent in the Terminal tab."),
            "the answer started before the state is dropped"
        );
        // The end reads anew, and that answer counts.
        c.tick();
        release.send(()).unwrap();
        until(&c, |v| {
            v.block_reason.as_deref() == Some("Respond to the agent in the Terminal tab.")
                && answered.load(Ordering::SeqCst) == 2
        });
    }

    #[test]
    fn an_inspect_of_a_session_no_longer_open_is_dropped() {
        let root = dir("inspect-late");
        let store = drafts(&root);
        let now = Arc::new(Mutex::new(1_793_000_100.0));
        let clock = now.clone();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let inspects = Arc::new(AtomicUsize::new(0));
        let counted = inspects.clone();
        let c = Composers::new(
            Arc::clone(&store),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(move || *lock(&clock)),
                rows: Arc::new(|_, key, name| {
                    let mut row = row(name);
                    row.run_id = Some(key.trim_start_matches("run:").into());
                    Some((row, true))
                }),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(move |target, _, _| {
                    // The first inspect waits until the test lets it answer.
                    if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        let _ = lock(&gate).recv_timeout(Duration::from_secs(5));
                    }
                    let mut details = ready();
                    details["run_id"] = json!(target.run_id);
                    Ok(details)
                }),
                dispatch: Arc::new(|_, _, _| Err("no call in these tests".into())),
            },
        );
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&now) += 3.5;
        c.tick();
        eventually(|| inspects.load(Ordering::SeqCst) == 1);
        let mut other = session();
        other.key = "run:run-2".into();
        other.name = "claude/sample-project/other".into();
        c.open(Some(other));
        until(&c, |v| v.loaded && v.key.ends_with("/other"));
        release.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let view = c.current().unwrap();
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Checking the session…"),
            "the first session's answer is not the second's"
        );
        *lock(&now) += 3.5;
        c.tick();
        until(&c, |v| v.block_reason.is_none());
        assert_eq!(
            c.lock().followed.details.as_ref().unwrap()["run_id"],
            "run-2"
        );
        assert_eq!(inspects.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_batch_with_the_open_row_or_its_status_changes_the_view_at_once() {
        let root = dir("row-batch");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        assert!(c.take_view().unwrap().available);
        lock(&fake.row).state = "paused".into();
        assert!(c.take_view().is_none(), "nothing announced it yet");
        let mut paused = row("claude/sample-project/review");
        paused.state = "paused".into();
        // Another session's row changes nothing.
        let mut other = paused.clone();
        other.name = "claude/sample-project/other".into();
        let upsert = |key: &str, row: ViewRow| Batch {
            upserts: vec![RowOut {
                computer: ComputerId::from("studio"),
                key: key.into(),
                row,
            }],
            ..Batch::default()
        };
        c.observe(&upsert("run:run-2", other));
        assert!(c.take_view().is_none());
        c.observe(&upsert("run:run-1", paused));
        assert_eq!(
            c.take_view().unwrap().block_reason.as_deref(),
            Some("Resume the session to send a message.")
        );
        lock(&fake.row).state = "running".into();
        *lock(&fake.online) = false;
        c.observe(&Batch {
            computers: vec![zerus_client_core::ComputerOut {
                computer: ComputerId::from("studio"),
                status: "reconnecting".into(),
                attempt: 1,
            }],
            ..Batch::default()
        });
        assert_eq!(
            c.take_view().unwrap().block_reason.as_deref(),
            Some("Machine unavailable. Your draft stays here.")
        );
    }

    #[test]
    fn a_write_blocks_messages_and_the_tick_shows_it() {
        let root = dir("write");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        assert!(c.take_view().is_some());
        c.tick();
        assert!(c.take_view().is_none(), "nothing changed");
        *lock(&fake.write) = Some(WriteKind::Update);
        c.tick();
        let view = c.take_view().unwrap();
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Wait for the session update to finish.")
        );
        *lock(&fake.write) = Some(WriteKind::Terminate);
        c.tick();
        assert_eq!(
            c.take_view().unwrap().block_reason.as_deref(),
            Some("The session is terminating. Your draft stays here.")
        );
        *lock(&fake.write) = None;
        c.tick();
        assert!(c.take_view().unwrap().available);
    }

    #[test]
    fn files_wait_for_an_agent_that_takes_them() {
        let root = dir("attachments");
        let file = root.join("notes.txt");
        std::fs::write(&file, "notes").unwrap();
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        c.attach_paths(
            view.handle,
            vec![file],
            zerus_client_core::attachments::Selection::default(),
        );
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        assert!(view.can_send && view.attachments_available);
        assert!(c.can_attach(view.handle));
        lock(&fake.row).agent = "opencode".into();
        let view = c.current().unwrap();
        assert!(!view.attachments_available && !view.can_send);
        assert!(!c.can_attach(view.handle), "Qt's canAttachFiles");
        assert_eq!(view.status, "This native adapter does not support file attachments. Your existing files remain saved; remove them to send text.");
        assert_eq!(view.attachments_reason, view.status);
        lock(&fake.row).agent = "claude".into();
        let mut details = ready();
        details["attachments_supported"] = json!(false);
        details["attachments_reason"] = json!("This agent takes no files here.");
        c.observe(&state(details));
        let view = c.current().unwrap();
        assert_eq!(view.status, "This agent takes no files here.");
        assert!(!view.can_send);
    }

    #[test]
    fn a_deepseek_row_is_dsh_to_the_rules() {
        let root = dir("dsh");
        let (c, fake) = send_hub(&root);
        lock(&fake.row).agent = "deepseek".into();
        let mut details = ready();
        details["auth_required"] = json!(true);
        assert_eq!(
            opened(&c, details).block_reason.as_deref(),
            Some("Sign in to DeepSeek above. Your draft stays here.")
        );
    }

    #[test]
    fn the_expiry_is_local_time_in_qts_form() {
        let text = local_time(1_793_000_640);
        let parts: Vec<&str> = text.split(' ').collect();
        assert_eq!(parts.len(), 3, "{text}");
        assert!(
            (1..=31).contains(&parts[0].parse::<u32>().unwrap()),
            "{text}"
        );
        assert!(
            ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
                .contains(&parts[1]),
            "{text}"
        );
        assert!(
            parts[2].len() == 8 && parts[2].split(':').all(|n| n.len() == 2),
            "{text}"
        );
        assert_eq!(
            expiry(&json!({"session_usage": {"prompt_cache": {"expires_at": 1_793_000_640.5}}})),
            Some((1_793_000_640, text))
        );
        assert_eq!(expiry(&ready()), None);
    }

    #[test]
    fn the_session_status_follows_qts_hint_chain() {
        let status = |facts: HintFacts<'_>, blocked: bool| session_status(&facts, blocked);
        let online = || HintFacts {
            online: true,
            ..HintFacts::default()
        };
        assert_eq!(status(online(), true), SessionStatus::default());
        let offline = status(HintFacts::default(), true);
        assert_eq!(
            (offline.text.as_str(), offline.error, offline.blocks_input),
            (OFFLINE, true, true)
        );
        assert!(
            !status(HintFacts::default(), false).blocks_input,
            "no block reason: no lock"
        );
        let text = |facts: HintFacts<'_>| status(facts, true).text;
        assert_eq!(
            text(HintFacts {
                terminating: true,
                ..HintFacts::default()
            }),
            "Waiting for the session to stop. Its native conversation history is kept."
        );
        assert_eq!(
            text(HintFacts {
                archived: true,
                restore_error: "occupied",
                inspect_error: Some("Could not refresh activity: timed out"),
                ..HintFacts::default()
            }),
            OFFLINE,
            "offline comes before every error"
        );
        assert_eq!(
            text(HintFacts {
                archived: true,
                restore_error: "name taken",
                inspect_error: Some("Could not refresh activity: timed out"),
                question_error: "bad",
                ..online()
            }),
            "Last restore failed: name taken"
        );
        assert_eq!(
            text(HintFacts {
                inspect_error: Some("Could not refresh activity: timed out"),
                question_error: "bad",
                ..online()
            }),
            "Could not refresh activity: timed out"
        );
        assert_eq!(
            text(HintFacts {
                question_error: "bad",
                ..online()
            }),
            "Could not load agent questions: bad. Check Terminal."
        );
        let occupied = status(
            HintFacts {
                archived: true,
                occupied: true,
                ..online()
            },
            true,
        );
        assert_eq!(
            occupied.text,
            "Another active or saved session uses this name. Restore becomes available when that name is free."
        );
        assert!(
            !occupied.error && !occupied.blocks_input,
            "shown, not an error"
        );
        assert_eq!(
            status(
                HintFacts {
                    occupied: true,
                    restore_error: "x",
                    ..online()
                },
                true
            ),
            SessionStatus::default(),
            "only an archived session shows them"
        );
    }

    #[test]
    fn offline_the_status_locks_the_files_and_send() {
        let root = dir("status-offline");
        let files: Vec<std::path::PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                let file = root.join(format!("{name}.txt"));
                std::fs::write(&file, *name).unwrap();
                file
            })
            .collect();
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        let at = zerus_client_core::attachments::Selection::default();
        c.attach_paths(view.handle, files[..2].to_vec(), at);
        let view = until(&c, |v| v.attachments.len() == 2 && !v.attaching);
        assert!(view.can_send && !view.session_blocks_input && view.status.is_empty());
        c.remove(view.handle, 1);
        assert_eq!(c.current().unwrap().removed.as_deref(), Some("b.txt"));
        *lock(&fake.online) = false;
        let view = c.current().unwrap();
        assert_eq!(view.status, OFFLINE);
        assert!(view.status_error && view.session_blocks_input && !view.can_send);
        assert_eq!(
            view.unavailable_reason,
            "Machine unavailable. Your draft stays here."
        );
        assert!(!c.can_attach(view.handle));
        // Qt's `canEditAttachments`: no remove, Undo or attach while locked;
        // each refusal hands the text back.
        c.remove(view.handle, 0);
        let after = c.current().unwrap();
        assert_eq!(after.attachments.len(), 1, "no remove while locked");
        assert!(
            after.text_epoch > view.text_epoch,
            "the text is handed back"
        );
        c.undo_remove(view.handle);
        let undone = c.current().unwrap();
        assert_eq!(undone.attachments.len(), 1, "no Undo while locked");
        assert!(undone.text_epoch > after.text_epoch);
        c.attach_paths(view.handle, vec![files[2].clone()], at);
        let view = c.current().unwrap();
        assert!(
            !view.attaching && view.attachments.len() == 1,
            "no attach while locked"
        );
        *lock(&fake.online) = true;
        let view = c.current().unwrap();
        assert!(view.status.is_empty() && !view.session_blocks_input && view.can_send);
        assert!(c.can_attach(view.handle));
        c.undo_remove(view.handle);
        assert_eq!(c.current().unwrap().attachments.len(), 2);
    }

    #[test]
    fn a_failed_read_shows_its_reason_and_locks() {
        let root = dir("status-read");
        let (c, _) = send_hub(&root);
        let view = opened(&c, ready());
        c.edit(view.handle, 1, view.text_epoch, "hi".into(), 2, 2);
        assert!(c.current().unwrap().can_send);
        c.lock().followed.inspect_error = Some("Could not refresh activity: timed out".into());
        let view = c.current().unwrap();
        assert_eq!(view.status, "Could not refresh activity: timed out");
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Refresh the session before sending.")
        );
        assert!(view.status_error && view.session_blocks_input && !view.can_send);
    }

    #[test]
    fn inspection_reads_like_qt() {
        let hgs = Path::new("hgs");
        let code = |code: i32, stdout: &str, stderr: &str| {
            inspection(
                hgs,
                Ok(Exit::Code {
                    code,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.into(),
                }),
            )
        };
        assert_eq!(
            code(0, r#"{"run_id": "run-1"}"#, ""),
            Ok(json!({"run_id": "run-1"}))
        );
        let invalid = Err("Invalid activity response".to_owned());
        assert_eq!(code(0, "[1]", ""), invalid);
        assert_eq!(code(0, "not json", ""), invalid);
        assert_eq!(
            code(2, "", "  no such session \n"),
            Err("no such session".into())
        );
        assert_eq!(code(3, "", " "), Err("exit code 3".into()));
        assert_eq!(code(1, "", &"x".repeat(5000)), Err("x".repeat(4000)));
        assert_eq!(inspection(hgs, Ok(Exit::TimedOut)), Err("timed out".into()));
        assert_eq!(
            inspection(hgs, Ok(Exit::StartFailed("not found".into()))),
            Err("cannot start 'hgs': not found".into())
        );
        assert_eq!(
            inspection(
                hgs,
                Ok(Exit::Abnormal {
                    stderr: String::new()
                })
            ),
            Err("hgs ended abnormally".into())
        );
        assert_eq!(
            inspection(
                hgs,
                Err(TooLarge {
                    cap: 8 << 20,
                    stderr: String::new()
                })
            ),
            invalid
        );
    }

    #[test]
    fn a_write_shows_at_once_without_the_tick() {
        use crate::actions::{Actions, Lane};
        let root = dir("write-hook");
        let actions = Actions::new(root.join("marks.json"));
        let facts = actions.clone();
        let delivered = Arc::new(AtomicUsize::new(0));
        let calls = delivered.clone();
        let c = Composers::new(
            drafts(&root),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, _, name| Some((row(name), true))),
                facts: Arc::new(move |computer, name, run| {
                    facts.session_write_for_run(computer, name, run, || (Some(row(name)), true))
                }),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(move |_, action, id| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(receipt(&action, &id))
                }),
            },
        );
        actions.on_writes(c.changed_hook());
        opened(&c, ready());
        assert!(c.take_view().unwrap().available);
        let target = zerus_client_core::Target {
            computer: "studio".into(),
            name: "claude/sample-project/review".into(),
            run_id: "run-2".into(),
            conversation_id: "conv-2".into(),
        };
        let pause = actions
            .begin(
                Lane::key(&target, &zerus_client_core::Action::Pause),
                &zerus_client_core::Action::Pause,
            )
            .unwrap();
        assert_eq!(
            c.take_view().unwrap().block_reason.as_deref(),
            Some("Wait for the session update to finish."),
            "the target's own write, shown before any tick"
        );
        drop(pause);
        assert!(c.take_view().unwrap().available);
        for (computer, name) in [
            ("studio", "claude/sample-project/other"),
            ("laptop", "claude/sample-project/review"),
        ] {
            let mut other = target.clone();
            other.computer = computer.into();
            other.name = name.into();
            for action in [
                zerus_client_core::Action::Pause,
                zerus_client_core::Action::Terminate,
            ] {
                let held = actions.begin(Lane::key(&other, &action), &action).unwrap();
                assert!(
                    c.current().unwrap().available,
                    "another target's write never blocks input"
                );
                drop(held);
                assert!(c.current().unwrap().available);
            }
        }
        // The original write remains held while another target's saved
        // message reaches delivery, including the final native facts check.
        let mut other = target.clone();
        other.name = "claude/sample-project/other".into();
        let held = actions
            .begin(
                Lane::key(&other, &zerus_client_core::Action::Terminate),
                &zerus_client_core::Action::Terminate,
            )
            .unwrap();
        let view = c.current().unwrap();
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "allowed".into(), 7, 7),
            SendReply::Sending
        );
        until(&c, |view| !view.sending);
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        assert!(
            actions
                .begin(
                    Lane::key(&other, &zerus_client_core::Action::Pause),
                    &zerus_client_core::Action::Pause
                )
                .is_none(),
            "delivery never releases the other target's custody"
        );
        drop(held);
        // Question availability consumes the same scoped native fact.
        use crate::composer::questions::tests::{asking, card_json, with_card};
        c.observe(&state(asking(vec![card_json("tool:t1")])));
        assert!(with_card(&c).question.unwrap().view.available);
        let mut other = target.clone();
        other.name = "claude/sample-project/other".into();
        let held = actions
            .begin(
                Lane::key(&other, &zerus_client_core::Action::Terminate),
                &zerus_client_core::Action::Terminate,
            )
            .unwrap();
        assert!(c.current().unwrap().question.unwrap().view.available);
        drop(held);
        let held = actions
            .begin(
                Lane::key(&target, &zerus_client_core::Action::Terminate),
                &zerus_client_core::Action::Terminate,
            )
            .unwrap();
        assert!(!c.current().unwrap().question.unwrap().view.available);
        drop(held);
        assert!(c.current().unwrap().question.unwrap().view.available);
    }

    #[test]
    fn review_rename_of_a_held_target_never_reenables_its_composer() {
        use crate::actions::{Actions, Lane};
        let root = dir("review-held-rename");
        let actions = Actions::new(root.join("marks.json"));
        let facts = actions.clone();
        let delivered = Arc::new(AtomicUsize::new(0));
        let calls = delivered.clone();
        let omit_run = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let catalog = omit_run.clone();
        let observed = omit_run.clone();
        let c = Composers::new(
            drafts(&root),
            Config {
                cache: root.join("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(move |_, _, name| {
                    let mut row = row(name);
                    if catalog.load(Ordering::SeqCst) {
                        row.run_id = None;
                    }
                    Some((row, true))
                }),
                facts: Arc::new(move |computer, name, run| {
                    facts.session_write_for_run(computer, name, run, || {
                        let mut row = row(name);
                        if observed.load(Ordering::SeqCst) {
                            row.run_id = None;
                        }
                        (Some(row), true)
                    })
                }),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch: Arc::new(move |_, action, id| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(receipt(&action, &id))
                }),
            },
        );
        actions.on_writes(c.changed_hook());
        opened(&c, ready());
        let target = zerus_client_core::Target {
            computer: "studio".into(),
            name: session().name,
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
        };
        let held = actions
            .begin(Lane::key(&target, &Action::Terminate), &Action::Terminate)
            .unwrap();
        assert!(
            !c.current().unwrap().available,
            "original target is initially blocked"
        );
        let mut renamed = session();
        renamed.name = "claude/sample-project/renamed".into();
        c.open(Some(renamed));
        let view = until(&c, |v| v.loaded);
        let became_available = view.available;
        let result = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "same original after rename".into(),
            26,
            26,
        );
        until(&c, |v| !v.sending);
        let reached = delivered.load(Ordering::SeqCst);
        omit_run.store(true, Ordering::SeqCst);
        assert!(
            !c.current().unwrap().available,
            "native details keep the original run bound when the row omits it"
        );
        use crate::composer::questions::tests::{asking, card_json, with_card};
        c.observe(&state(asking(vec![card_json("tool:t1")])));
        let question_available = with_card(&c).question.unwrap().view.available;
        // Keep and then release the SAME original target's write claim.
        assert!(actions
            .begin(Lane::key(&target, &Action::Pause), &Action::Pause)
            .is_none());
        drop(held);
        assert!(!became_available,"same run's composer became available while its original write remained held; send={result:?}, dispatched={reached}");
        assert_eq!(
            reached, 0,
            "no same-run delivery while original write is held"
        );
        assert!(
            !question_available,
            "the same original run's question remains blocked after rename"
        );
    }

    pub(crate) fn receipt(action: &Action, id: &str) -> Outcome {
        Outcome::Done {
            receipt: json!({"status": "submitted", "request_id": id, "kind": format!("{action:?}").split_whitespace().next()}),
            cleanup: None,
        }
    }

    pub(crate) fn sent(fake: &Fake) -> Vec<(Target, Action, String)> {
        lock(&fake.calls).clone()
    }

    #[test]
    fn send_runs_the_held_draft_once_and_clears_it() {
        let root = dir("send-once");
        let file = root.join("a.txt");
        std::fs::write(&file, "attached").unwrap();
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        c.attach_paths(
            view.handle,
            vec![file],
            zerus_client_core::attachments::Selection { start: 0, end: 0 },
        );
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.edit(
            view.handle,
            4,
            view.text_epoch,
            "hello [File #1] ".into(),
            0,
            0,
        );
        assert!(c.current().unwrap().can_send);
        assert_eq!(
            c.send_message(
                view.handle,
                5,
                view.text_epoch,
                "hello [File #1] ".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.text.is_empty());
        let calls = sent(&fake);
        assert_eq!(calls.len(), 1);
        let (target, action, id) = &calls[0];
        assert_eq!(
            (
                target.computer.as_str(),
                target.run_id.as_str(),
                target.conversation_id.as_str()
            ),
            ("studio", "run-1", "conv-1")
        );
        let Action::Send {
            text,
            attachments,
            agent_id,
            expected_compaction_id,
        } = action
        else {
            panic!("not a send")
        };
        assert_eq!(text, "hello [File #1] ");
        assert_eq!(&*attachments[0].bytes, b"attached");
        assert!(agent_id.is_none() && expected_compaction_id.is_none());
        assert!(view.attachments.is_empty());
        assert_eq!(view.local.len(), 1);
        assert_eq!(
            &view.local[0].id, id,
            "the row carries the request's own id"
        );
        assert_eq!(
            view.local[0].status,
            zerus_client_core::local_messages::Status::Sent
        );
    }

    #[test]
    fn a_child_send_uses_the_parent_run_and_conversation_with_the_agent_id() {
        let root = dir("child-send");
        let (c, fake) = send_hub(&root);
        c.open(Some(child_session()));
        c.observe(&state(child_ready()));
        let view = until(&c, |v| v.loaded && v.available);
        assert!(view.input_visible);
        assert_eq!(
            view.key,
            "studio\nclaude/sample-project/review\nconv-1\nagent-7"
        );
        assert_eq!(
            view.subagent.unwrap().hint,
            "Native subagent history Messages go directly to this agent."
        );
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "for the child".into(),
            13,
            13,
        );
        let view = c.current().unwrap();
        assert_eq!(
            c.send_message(
                view.handle,
                2,
                view.text_epoch,
                "for the child".into(),
                13,
                13
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.text.is_empty());
        assert_eq!(view.local.len(), 1);
        let calls = sent(&fake);
        assert_eq!(calls.len(), 1);
        let (target, action, _) = &calls[0];
        assert_eq!(
            (target.run_id.as_str(), target.conversation_id.as_str()),
            ("run-1", "conv-1")
        );
        let Action::Send { text, agent_id, .. } = action else {
            panic!("not a send")
        };
        assert_eq!(text, "for the child");
        assert_eq!(agent_id.as_deref(), Some("agent-7"));
    }

    #[test]
    fn a_read_only_child_never_borrows_parent_send_queue_or_clear() {
        let root = dir("child-read-only");
        let (c, fake) = send_hub(&root);
        c.open(Some(child_session()));
        let mut unsupported = child_ready();
        unsupported["send_supported"] = json!(false);
        c.observe(&state(unsupported));
        let view = until(&c, |v| v.loaded);
        assert!(!view.available && !view.input_visible);
        c.edit(view.handle, 1, view.text_epoch, "must stay".into(), 10, 10);
        let view = c.current().unwrap();
        assert!(matches!(
            c.send_message(view.handle, 2, view.text_epoch, "must stay".into(), 10, 10),
            SendReply::Blocked { .. }
        ));
        assert_eq!(
            c.send_push(view.handle, 3, view.text_epoch, "must stay".into(), 10, 10),
            SendReply::Busy
        );
        assert!(!c.send_queue_now(None));
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Busy
        );
        c.observe(&state(ready()));
        let view = c.current().unwrap();
        assert!(!view.available && !view.subagent.unwrap().send_supported);
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn parent_and_child_drafts_survive_navigation_separately() {
        let root = dir("child-drafts");
        let (c, _) = send_hub(&root);
        let parent = opened(&c, ready());
        c.edit(
            parent.handle,
            1,
            parent.text_epoch,
            "parent draft".into(),
            12,
            12,
        );
        c.open(Some(child_session()));
        c.observe(&state(child_ready()));
        let child = until(&c, |v| v.loaded && v.available);
        c.edit(
            child.handle,
            1,
            child.text_epoch,
            "child draft".into(),
            11,
            11,
        );
        c.open(Some(session()));
        let parent = until(&c, |v| v.loaded);
        assert_eq!(parent.text, "parent draft");
        c.open(Some(child_session()));
        c.observe(&state(child_ready()));
        let child = until(&c, |v| v.loaded && v.available);
        assert_eq!(child.text, "child draft");
        assert_ne!(parent.key, child.key);
    }

    #[test]
    fn a_blocked_send_runs_nothing() {
        let root = dir("blocked-send");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["phase"] = json!("approval");
        let view = opened(&c, details);
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "hi".into(), 2, 2),
            SendReply::Blocked {
                reason: "Respond to the agent in the Terminal tab.".into()
            }
        );
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_cold_cache_asks_first_and_sends_only_after_consent() {
        let root = dir("cold");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        assert_eq!(view.cache.label, "Cold cache");
        let SendReply::Cold { token, prompt } =
            c.send_message(view.handle, 1, view.text_epoch, "hi".into(), 2, 2)
        else {
            panic!("no question")
        };
        assert_eq!(prompt.title, "Continue with a cold cache?");
        assert!(sent(&fake).is_empty());
        assert_eq!(c.answer(token, Answer::Cancel), SendReply::Busy);
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Busy,
            "a cancelled question is gone"
        );
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "hi".into(), 2, 2)
        else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Sending
        );
        until(&c, |v| !v.sending);
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn an_uncertain_send_is_never_retried() {
        let root = dir("uncertain");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| {
            Outcome::Uncertain { detail: "Delivery timed out. The message may have reached the agent; inspect Terminal before retrying.".into() }
        });
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        assert_eq!(view.text, "maybe", "the draft comes back");
        assert!(!view.can_send);
        let row = &view.local[0];
        assert!(row.uncertain);
        assert_eq!(row.error, "Delivery is not confirmed. Check Terminal before retrying. Delivery timed out. The message may have reached the agent; inspect Terminal before retrying.");
        assert_eq!(c.retry(view.handle, &row.id), SendReply::Busy);
        assert_eq!(
            c.send_message(view.handle, 2, view.text_epoch, "maybe".into(), 5, 5),
            SendReply::Busy,
            "Send waits for Allow retry"
        );
        c.flush();
        store.flush();
        drop(c);
        let (again, _) = send_hub_on(&store);
        let view = opened(&again, ready());
        assert!(
            view.uncertain && !view.can_send,
            "a restart keeps it uncertain"
        );
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn a_stale_question_never_resends_an_uncertain_draft() {
        let root = dir("stale-question");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        let SendReply::Cold { token: first, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5)
        else {
            panic!("no question")
        };
        let SendReply::Cold { token: second, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5)
        else {
            panic!("no question")
        };
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "Delivery timed out.".into(),
        });
        assert_eq!(
            c.answer(
                second,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Sending
        );
        until(&c, |v| !v.sending && v.uncertain);
        assert_eq!(
            c.answer(
                first,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Busy,
            "the older question went with the delivery"
        );
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn a_send_whose_save_is_queued_at_exit_never_starts() {
        let root = dir("exit-send");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let view = opened(&c, ready());
        // Hold the writer, so the "sending" save waits in the queue.
        let (release, held) = std::sync::mpsc::channel::<()>();
        store.load("hold", move |_| {
            let _ = held.recv();
        });
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "at exit".into(), 7, 7),
            SendReply::Sending
        );
        c.flush();
        release.send(()).unwrap();
        store.flush();
        assert!(sent(&fake).is_empty(), "nothing starts once the app quits");
        let saved = zerus_client_core::drafts::read_draft(
            &root.join("drafts"),
            crate::composer::tests::KEY,
        );
        assert!(
            saved.draft.uncertain,
            "the draft saved as sending reloads as uncertain"
        );
    }

    #[test]
    fn a_definite_failure_retries_its_own_text_and_keeps_a_newer_draft() {
        let root = dir("retry-row");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "first".into(), 5, 5);
        let view = until(&c, |v| {
            !v.sending && v.local.first().is_some_and(|r| !r.error.is_empty())
        });
        let id = view.local[0].id.clone();
        c.edit(
            view.handle,
            9,
            view.text_epoch,
            "a newer draft".into(),
            13,
            13,
        );
        *lock(&fake.outcome) = Box::new(receipt);
        assert_eq!(c.retry(view.handle, &id), SendReply::Sending);
        let view = until(&c, |v| {
            !v.sending && v.local.iter().all(|r| r.error.is_empty())
        });
        let calls = sent(&fake);
        assert_eq!(calls.len(), 2);
        let Action::Send { text, .. } = &calls[1].1 else {
            panic!()
        };
        assert_eq!(text, "first");
        assert_ne!(
            calls[0].2, calls[1].2,
            "a retry is a new submission with its own request id"
        );
        assert_eq!(view.text, "a newer draft");
        assert_eq!(view.local.len(), 1, "the retry replaced the failed row");
    }

    #[test]
    fn the_history_reconciles_sent_rows() {
        let root = dir("reconcile");
        let (c, _) = send_hub(&root);
        let view = opened(&c, ready());
        c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "shown in history".into(),
            0,
            0,
        );
        until(&c, |v| !v.sending && v.local.len() == 1);
        let mut batch = state(ready());
        batch.activity.push(ActivityOut {
            computer: ComputerId::from("studio"), key: "run:run-1".into(), kind: "events".into(),
            payload: json!({"events": [{"seq": 9, "type": "UserPromptSubmit", "detail": "shown in history", "at": 1_793_000_100.0}], "cursor": 9}),
        });
        c.observe(&batch);
        until(&c, |v| v.local.is_empty());
    }

    #[test]
    fn a_session_that_changed_is_not_sent_to() {
        let root = dir("changed");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        let delivery = c.delivery_for_test(view.handle, "hi");
        assert!(c.still_valid(&delivery));
        lock(&fake.row).created += 1;
        assert!(
            !c.still_valid(&delivery),
            "another session under the same name"
        );
        lock(&fake.row).created -= 1;
        lock(&fake.row).run_id = Some("run-2".into());
        assert!(!c.still_valid(&delivery), "another run");
    }

    // The guards each have a test of their own below, so a mutation of one
    // is caught even where another guard stands behind it.

    /// Jobs the hub spawns, held until the test runs them.
    pub(crate) type Held = Arc<Mutex<Vec<Job>>>;

    pub(crate) fn held_spawn() -> (Spawn, Held) {
        let jobs: Held = Arc::default();
        let queue = Arc::clone(&jobs);
        (Arc::new(move |job: Job| lock(&queue).push(job)), jobs)
    }

    fn run_held(jobs: &Held) {
        let taken: Vec<Job> = std::mem::take(&mut *lock(jobs));
        for job in taken {
            job();
        }
    }

    /// The oldest held job, once one is there (within five seconds).
    pub(crate) fn next_job(jobs: &Held) -> Job {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let job = {
                let mut queue = lock(jobs);
                (!queue.is_empty()).then(|| queue.remove(0))
            };
            if let Some(job) = job {
                return job;
            }
            assert!(std::time::Instant::now() < deadline, "no job came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub(crate) fn other_session() -> SessionRef {
        SessionRef {
            key: "run:run-2".into(),
            name: "claude/sample-project/other".into(),
            ..session()
        }
    }

    /// A definite failure of `text`, sent from the open composer: its row id.
    pub(crate) fn failed_send(c: &Composers, fake: &Fake, text: &str) -> String {
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let view = c.current().unwrap();
        c.send_message(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            text.into(),
            0,
            0,
        );
        let view = until(c, |v| {
            !v.sending && v.local.iter().any(|row| row.status == Status::Error)
        });
        *lock(&fake.outcome) = Box::new(receipt);
        view.local[0].id.clone()
    }

    /// Waits until `done` holds, within five seconds.
    fn settles(done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn retry_pins_follow_the_rows_not_the_sessions_opened() {
        let root = dir("send-retry-pins");
        let file = root.join("a.txt");
        std::fs::write(&file, "attached").unwrap();
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let view = opened(&c, ready());
        c.attach_paths(
            view.handle,
            vec![file],
            zerus_client_core::attachments::Selection::default(),
        );
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.send_message(view.handle, 1, view.text_epoch, view.text.clone(), 0, 0);
        let view = until(&c, |v| {
            !v.sending && v.local.first().is_some_and(|r| !r.error.is_empty())
        });
        let stored = LocalMessages::stored_files(&view.local[0]).unwrap();
        let pinned = || c.retry_files();
        assert_eq!(pinned(), HashSet::from([stored[0].local_path.clone()]));
        // The hub forgot which sessions it opened (past 512 of them); S4
        // still keeps the row, so Retry still needs its file.
        c.lock().opened.clear();
        assert_eq!(pinned(), HashSet::from([stored[0].local_path.clone()]));
    }

    #[test]
    fn a_warm_retry_stays_with_its_session_when_another_opens() {
        let root = dir("send-retry-away");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        let id = failed_send(&c, &fake, "first");
        let Ok(begun) = c.retry_begin(view.handle, &id) else {
            panic!("no retry")
        };
        // Another session opens while the cached files are read.
        c.open(Some(other_session()));
        until(&c, |v| v.loaded && v.key.ends_with("/other"));
        let read = attachments::read_stored(c.cache().root(), &begun.files);
        assert_eq!(c.retry_finish(begun, read), SendReply::Sending);
        settles(|| sent(&fake).len() == 2 && !c.lock().composers[KEY].sending);
        let calls = sent(&fake);
        let (target, action, _) = &calls[1];
        assert_eq!(
            (target.name.as_str(), target.run_id.as_str()),
            ("claude/sample-project/review", "run-1"),
            "the retry's own session"
        );
        let Action::Send { text, .. } = action else {
            panic!("not a send")
        };
        assert_eq!(text, "first");
        let view = c.current().unwrap();
        assert!(view.key.ends_with("/other") && !view.sending && view.local.is_empty());
    }

    #[test]
    fn a_cold_retry_never_reaches_a_session_no_longer_open() {
        let root = dir("send-retry-away-cold");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "first".into(), 0, 0)
        else {
            panic!("no question")
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && !v.local.is_empty());
        let id = view.local[0].id.clone();
        *lock(&fake.outcome) = Box::new(receipt);
        let Ok(begun) = c.retry_begin(view.handle, &id) else {
            panic!("no retry")
        };
        c.open(Some(other_session()));
        until(&c, |v| v.loaded && v.key.ends_with("/other"));
        let read = attachments::read_stored(c.cache().root(), &begun.files);
        assert_eq!(c.retry_finish(begun, read), SendReply::Busy);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(sent(&fake).len(), 1, "only the first send");
        assert!(!c.lock().composers[KEY].sending);
    }

    #[test]
    fn a_write_that_starts_during_the_save_keeps_the_draft() {
        let root = dir("send-write-mid");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let view = opened(&c, ready());
        let release = hold(&store);
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "mid write".into(), 9, 9),
            SendReply::Sending
        );
        // This target's update begins while its draft save is pending.
        *lock(&fake.write) = Some(WriteKind::Update);
        release.send(()).unwrap();
        let view = until(&c, |v| !v.sending);
        assert_eq!(view.status, CHANGED);
        assert_eq!(view.text, "mid write");
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
        // The same target must still be checked when another session is open.
        *lock(&fake.write) = None;
        let view = c.current().unwrap();
        let release = hold(&store);
        assert_eq!(
            c.send_message(view.handle, 2, view.text_epoch, "mid write".into(), 9, 9),
            SendReply::Sending
        );
        c.open(Some(other_session()));
        *lock(&fake.write) = Some(WriteKind::Update);
        release.send(()).unwrap();
        settles(|| !c.lock().composers[KEY].sending);
        assert_eq!(c.lock().composers[KEY].notice, CHANGED);
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn an_older_deliverys_call_never_runs_after_a_newer_one_started() {
        let root = dir("send-older-call");
        let store = drafts(&root);
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "once".into(), 4, 4),
            SendReply::Sending
        );
        let older = next_job(&jobs);
        // A cancel (Task 5) ends the send, and the draft is sent anew.
        c.delivery_finished(KEY, false, "Cancelled.", false, false);
        let view = c.current().unwrap();
        assert!(!view.sending && view.text == "once");
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "once".into(), 4, 4),
            SendReply::Sending
        );
        let newer = next_job(&jobs);
        older();
        assert!(sent(&fake).is_empty(), "the older delivery calls nothing");
        assert!(c.current().unwrap().sending, "nor ends the newer one");
        newer();
        until(&c, |v| !v.sending);
        assert_eq!(sent(&fake).len(), 1);
        assert_eq!(c.current().unwrap().local.len(), 1);
    }

    #[test]
    fn a_block_reason_at_the_answer_is_an_error_notice() {
        let root = dir("send-question-blocked");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "asked".into(), 5, 5)
        else {
            panic!("no question")
        };
        details["phase"] = json!("approval");
        c.observe(&state(details));
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Notice {
                text: "Respond to the agent in the Terminal tab.".into(),
                error: true,
            }
        );
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_send_over_an_older_text_epoch_is_refused_and_handed_back() {
        let root = dir("send-epoch");
        let file = root.join("a.txt");
        std::fs::write(&file, "attached").unwrap();
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        c.attach_paths(
            view.handle,
            vec![file],
            zerus_client_core::attachments::Selection::default(),
        );
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        // The mention is gone from the text, so the remove keeps the text
        // and changes only the files.
        c.edit(view.handle, 1, view.text_epoch, "hi".into(), 2, 2);
        let older = view.text_epoch;
        c.remove(view.handle, 0);
        let removed = c.current().unwrap();
        assert!(
            removed.text_epoch > older && removed.attachments.is_empty() && removed.text == "hi"
        );
        assert_eq!(
            c.send_message(view.handle, 2, older, "hi".into(), 2, 2),
            SendReply::Busy
        );
        assert!(
            c.current().unwrap().text_epoch > removed.text_epoch,
            "the text is handed back"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            sent(&fake).is_empty(),
            "text typed over an older draft is never sent"
        );
        // The editor's text must be the draft's too.
        let view = c.current().unwrap();
        c.edit(view.handle, 3, view.text_epoch, "hi".into(), 2, 2);
        assert_eq!(
            c.send_message(
                view.handle,
                3,
                view.text_epoch,
                "not what it holds".into(),
                2,
                2
            ),
            SendReply::Busy
        );
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_newer_save_keeps_a_queued_send_from_starting() {
        let root = dir("send-revision");
        let store = drafts(&root);
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        let view = opened(&c, ready());
        let release = hold(&store);
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "queued".into(), 6, 6),
            SendReply::Sending
        );
        // Another write to the key took the next save revision meanwhile:
        // the "sending" save's answer is no longer the newest.
        c.lock().composers.get_mut(KEY).unwrap().saves += 1;
        release.send(()).unwrap();
        store.flush();
        assert!(
            lock(&jobs).is_empty(),
            "an older save's answer starts nothing"
        );
        run_held(&jobs);
        assert!(sent(&fake).is_empty());
    }

    /// A hub over `store` whose calls go to `dispatch`.
    fn dispatching(store: &Arc<DraftStore>, dispatch: crate::composer::Dispatch) -> Composers {
        Composers::new(
            Arc::clone(store),
            Config {
                cache: store.root().with_file_name("cache"),
                qt_drafts: None,
                dev_drafts: Vec::new(),
            },
            Deps {
                spawn: Arc::new(|job: Job| {
                    std::thread::spawn(job);
                }),
                notify: Arc::new(|| {}),
                clock: Arc::new(|| 1_793_000_100.0),
                rows: Arc::new(|_, _, name| Some((row(name), true))),
                facts: Arc::new(|_, _, _| None),
                reconnect: Arc::new(|_| {}),
                inspect: Arc::new(|_, _, _| Err("timed out".into())),
                dispatch,
            },
        )
    }

    #[test]
    fn a_failed_saves_retry_never_overtakes_a_send() {
        let root = dir("send-save-retry");
        let blocker = root.join("blocked");
        std::fs::write(&blocker, "a file where the store's folder belongs").unwrap();
        let store = Arc::new(DraftStore::open(blocker.join("drafts")));
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        let view = opened(&c, ready());
        c.edit(view.handle, 1, view.text_epoch, "kept".into(), 4, 4);
        let view = until(&c, |v| v.status_error);
        // Qt's 2 s retry of the failed save, held until the send's save waits.
        let retry = next_job(&jobs);
        std::fs::remove_file(&blocker).unwrap();
        let release = hold(&store);
        // Send comes at the same rev, so its edit saves nothing of its own.
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "kept".into(), 4, 4),
            SendReply::Sending
        );
        // The retry comes due while the "sending" save waits: it finds a
        // newer revision and saves nothing.
        retry();
        release.send(()).unwrap();
        store.flush();
        let call = next_job(&jobs);
        call();
        let view = until(&c, |v| !v.sending);
        assert!(view.text.is_empty() && !view.status_error, "{view:?}");
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn a_call_whose_thread_died_is_uncertain() {
        for (name, panics) in [("send-died-err", false), ("send-died-panic", true)] {
            let root = dir(name);
            let store = drafts(&root);
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&calls);
            let c = dispatching(
                &store,
                Arc::new(move |_, _, _| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    if panics {
                        panic!("the call's thread ended");
                    }
                    Err("the call's thread ended".into())
                }),
            );
            let view = opened(&c, ready());
            assert_eq!(
                c.send_message(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5),
                SendReply::Sending
            );
            let view = until(&c, |v| !v.sending && v.uncertain);
            assert_eq!(view.text, "maybe", "the draft stays");
            assert!(view.local[0].uncertain && !view.can_send, "{view:?}");
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "called once, never again");
        }
    }

    #[test]
    fn the_last_gate_never_sends_an_uncertain_draft() {
        let root = dir("send-gate");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        c.edit(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5);
        let delivery = c.delivery_for_test(view.handle, "maybe");
        {
            let mut hub = c.lock();
            hub.composers.get_mut(KEY).unwrap().uncertain = true;
            assert!(
                !c.deliver(&mut hub, delivery),
                "an uncertain draft never reaches its save"
            );
            assert!(!hub.composers[KEY].sending);
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn an_uncertain_draft_is_never_asked_about() {
        let root = dir("send-uncertain-cold");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        c.edit(view.handle, 1, view.text_epoch, "maybe".into(), 5, 5);
        c.lock().composers.get_mut(KEY).unwrap().uncertain = true;
        let view = c.current().unwrap();
        assert_eq!(
            c.send_message(view.handle, 2, view.text_epoch, "maybe".into(), 5, 5),
            SendReply::Busy
        );
        assert!(c.lock().prompts.is_empty(), "no question to answer later");
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_cache_that_turns_cold_before_the_call_sends_nothing() {
        let root = dir("send-turns-cold");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let start = *lock(&fake.now);
        let mut details = ready();
        details["session_usage"] =
            json!({"prompt_cache": {"status": "warm", "expires_at": start + 30.0}});
        let view = opened(&c, details);
        let release = hold(&store);
        assert_eq!(
            c.send_message(
                view.handle,
                1,
                view.text_epoch,
                "warm when pressed".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        // It expires before the "sending" save lands: no consent was given.
        *lock(&fake.now) = start + 60.0;
        release.send(()).unwrap();
        let view = until(&c, |v| !v.sending);
        assert_eq!(view.status, CHANGED);
        assert_eq!(view.text, "warm when pressed", "the draft is kept");
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_hub_closed_before_the_call_starts_nothing() {
        // Closed while the "sending" save waits: its answer spawns nothing.
        let root = dir("send-closed-save");
        let store = drafts(&root);
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        let view = opened(&c, ready());
        let release = hold(&store);
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "at exit".into(), 7, 7),
            SendReply::Sending
        );
        c.flush();
        release.send(()).unwrap();
        store.flush();
        assert!(lock(&jobs).is_empty(), "the save's answer starts nothing");
        assert!(sent(&fake).is_empty());
        // Closed after the save, before the call's thread runs: no call.
        let root = dir("send-closed-call");
        let store = drafts(&root);
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "at exit".into(), 7, 7),
            SendReply::Sending
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while lock(&jobs).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the save never answered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        c.flush();
        run_held(&jobs);
        assert!(sent(&fake).is_empty(), "nothing starts once the app quits");
        assert!(
            c.current().unwrap().local.is_empty(),
            "no row for a call never made"
        );
        store.flush();
        let saved = zerus_client_core::drafts::read_draft(&root.join("drafts"), KEY);
        assert!(saved.draft.uncertain && saved.draft.draft.text == "at exit");
    }

    #[test]
    fn a_send_clears_undo() {
        let root = dir("send-undo");
        let files: Vec<std::path::PathBuf> = ["a", "b"]
            .iter()
            .map(|name| {
                let file = root.join(format!("{name}.txt"));
                std::fs::write(&file, *name).unwrap();
                file
            })
            .collect();
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let view = opened(&c, ready());
        c.attach_paths(
            view.handle,
            files,
            zerus_client_core::attachments::Selection::default(),
        );
        let view = until(&c, |v| v.attachments.len() == 2 && !v.attaching);
        c.remove(view.handle, 1);
        let view = c.current().unwrap();
        assert_eq!(view.removed.as_deref(), Some("b.txt"));
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, view.text.clone(), 0, 0),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.status == "hgs: refused");
        assert_eq!(
            view.removed, None,
            "Undo never brings back a file of a sent message"
        );
        c.undo_remove(view.handle);
        assert_eq!(c.current().unwrap().attachments.len(), 1);
    }

    #[test]
    fn a_failed_sending_save_sends_nothing_and_unlocks() {
        let root = dir("send-save-failed");
        let blocker = root.join("blocked");
        std::fs::write(&blocker, "a file where the store's folder belongs").unwrap();
        let store = Arc::new(DraftStore::open(blocker.join("drafts")));
        let (c, fake) = send_hub_on(&store);
        let view = opened(&c, ready());
        let before = view.text_epoch;
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "unsaved".into(), 7, 7),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending);
        assert!(
            view.status.starts_with("Draft not saved") && view.status_error,
            "{view:?}"
        );
        assert_eq!(view.text, "unsaved");
        assert!(view.text_epoch > before, "the editor unlocks");
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn a_new_question_replaces_its_composers_older_one() {
        let root = dir("send-question-again");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        let SendReply::Cold { token: first, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "asked".into(), 5, 5)
        else {
            panic!("no question")
        };
        let SendReply::Cold { token: second, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "asked".into(), 5, 5)
        else {
            panic!("no question")
        };
        assert_eq!(c.lock().prompts.len(), 1, "one open question per composer");
        assert_eq!(
            c.answer(
                first,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Busy
        );
        assert_eq!(
            c.answer(
                second,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Sending
        );
        until(&c, |v| !v.sending);
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn a_question_whose_draft_changed_sends_nothing() {
        let root = dir("send-question-changed");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold", "tokens": 120000});
        let view = opened(&c, details);
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "asked".into(), 5, 5)
        else {
            panic!("no question")
        };
        c.edit(
            view.handle,
            2,
            view.text_epoch,
            "asked, then changed".into(),
            19,
            19,
        );
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Notice {
                text: "Draft changed. Send it when you are ready.".into(),
                error: false,
            }
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(sent(&fake).is_empty());
    }

    #[test]
    fn inspected_events_reconcile_rows_too() {
        let root = dir("send-inspected");
        let (c, fake) = send_hub(&root);
        let mut details = ready();
        // Seen by the inspect that the send's end starts (3.5 s on).
        details["events"] = json!([{"seq": 3, "type": "UserPromptSubmit", "detail": "seen by inspect", "at": 1_793_000_103.5}]);
        *lock(&fake.inspected) = Some(details);
        c.open(Some(session()));
        until(&c, |v| v.loaded);
        *lock(&fake.now) += 3.5;
        c.tick();
        let view = until(&c, |v| v.block_reason.is_none());
        assert!(
            c.lock()
                .followed
                .details
                .as_ref()
                .unwrap()
                .get("events")
                .is_none(),
            "the details keep no events"
        );
        c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "seen by inspect".into(),
            0,
            0,
        );
        until(&c, |v| !v.sending && v.local.is_empty());
        assert_eq!(sent(&fake).len(), 1);
    }

    #[test]
    fn the_followed_events_are_bounded() {
        let root = dir("send-events");
        let (c, _) = send_hub(&root);
        opened(&c, ready());
        let events = |kind: &str, seqs: std::ops::Range<i64>| Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: kind.into(),
                payload: json!({"events": seqs.map(|seq| json!({"seq": seq})).collect::<Vec<_>>()}),
            }],
            ..Batch::default()
        };
        c.observe(&events("events", 0..300));
        c.observe(&events("events", 300..600));
        let seqs = || {
            c.lock()
                .followed
                .events
                .iter()
                .map(|e| e["seq"].as_i64().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(seqs(), (100..600).collect::<Vec<_>>(), "Qt keeps 500");
        c.observe(&events("replace", 7..9));
        assert_eq!(seqs(), vec![7, 8]);
    }

    #[test]
    fn a_session_that_leaves_the_catalog_drops_its_rows() {
        let root = dir("send-forget");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "failed".into(), 6, 6);
        until(&c, |v| !v.sending && v.local.len() == 1);
        let removed = |reason: &str| Batch {
            removes: vec![zerus_client_core::KeyOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                reason: Some(reason.into()),
            }],
            ..Batch::default()
        };
        c.observe(&removed("replaced"));
        assert_eq!(
            c.current().unwrap().local.len(),
            1,
            "a replaced key is the same session"
        );
        c.observe(&removed("archived"));
        assert!(c.current().unwrap().local.is_empty());
    }

    #[test]
    fn a_definite_failure_offers_retry_inspect_and_delete() {
        let root = dir("send-row-actions");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        let mut details = ready();
        details["terminal_supported"] = json!(true);
        let view = opened(&c, details);
        c.send_message(view.handle, 1, view.text_epoch, "failed".into(), 6, 6);
        let view = until(&c, |v| !v.sending && v.local.len() == 1);
        let id = view.local[0].id.clone();
        assert!(!view.local[0].uncertain, "a definite failure");
        assert_eq!(c.inspect_local(view.handle, &id), Some(Inspect::Terminal));
        assert_eq!(c.inspect_local(view.handle, "not-a-row"), None);
        // Retry: a new submission whose row replaces this one.
        assert_eq!(c.retry(view.handle, &id), SendReply::Sending);
        let view = until(&c, |v| {
            !v.sending && v.local.len() == 1 && v.local[0].id != id
        });
        let again = view.local[0].id.clone();
        assert_eq!(sent(&fake).len(), 2);
        c.delete_local(view.handle + 1, &again);
        assert_eq!(
            c.current().unwrap().local.len(),
            1,
            "another composer's handle deletes nothing"
        );
        c.delete_local(view.handle, &again);
        assert!(c.current().unwrap().local.is_empty());
        assert_eq!(sent(&fake).len(), 2);
    }
}

#[cfg(test)]
mod continuations {
    use super::tests::{
        ended, eventually, failed_send, held_spawn, next_job, opened, other_session, ready,
        receipt, row, send_hub, send_hub_on, send_hub_on_rows, send_hub_with, state, Fake, Held,
    };
    use super::*;
    use crate::composer::tests::{dir, drafts, hold, session, until, KEY};
    use crate::composer::{lock, Job};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use zerus_client_core::{ActivityOut, Batch, ComputerId};

    const WAITING_START: &str = "Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit.";
    const WAITING_COMPACT: &str = "Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit.";
    const OTHER: &str = "studio\nclaude/sample-project/other";

    fn stopped(fake: &Fake) {
        let mut row = lock(&fake.row);
        row.state = "paused".into();
        row.resumable = true;
    }

    fn called(fake: &Fake, action: &Action) -> bool {
        lock(&fake.calls).iter().any(|(_, a, _)| a == action)
    }

    /// The messages sent: their target and the compaction they waited for.
    fn sends(fake: &Fake) -> Vec<(Target, Option<String>)> {
        lock(&fake.calls)
            .iter()
            .filter_map(|(target, action, _)| match action {
                Action::Send {
                    expected_compaction_id,
                    ..
                } => Some((target.clone(), expected_compaction_id.clone())),
                _ => None,
            })
            .collect()
    }

    /// The call of `action`, once it was made.
    fn call_of(fake: &Fake, action: &Action) -> Target {
        lock(&fake.calls)
            .iter()
            .find(|(_, a, _)| a == action)
            .map(|(target, ..)| target.clone())
            .expect("called")
    }

    /// Compactions answer with this request id.
    fn compacts_as(fake: &Fake, id: &'static str) {
        *lock(&fake.outcome) = Box::new(move |action, request| match action {
            Action::CompactContext => Outcome::Done {
                receipt: json!({"status": "submitted", "request_id": id}),
                cleanup: None,
            },
            _ => receipt(action, request),
        });
    }

    /// Waits until the waiting continuation's call has its definite outcome,
    /// so a later time limit or cancel ends a call that already answered: an
    /// end while it runs keeps its identity (zerus-5wmh.66.4.4).
    fn settled(c: &Composers, start: bool) {
        eventually(|| {
            let hub = c.lock();
            let call = if start {
                hub.start.as_ref().map(|(pending, _)| pending.call)
            } else {
                hub.compacts.get(KEY).map(|(pending, _)| pending.call)
            };
            call == Some(Call::Settled)
        });
    }

    /// Every call, with the checkpoint the session's draft held on disk at
    /// that moment (zerus-5wmh.66.4.4). Calls are then answered as before.
    type Seen = std::sync::Arc<Mutex<Vec<(Action, String, Option<Checkpoint>)>>>;
    fn record_checkpoints(fake: &Fake, root: &crate::composer::tests::Scratch) -> Seen {
        let seen = Seen::default();
        let previous = std::mem::replace(&mut *lock(&fake.outcome), Box::new(receipt));
        let (record, store) = (seen.clone(), root.join("drafts"));
        *lock(&fake.outcome) = Box::new(move |action, id| {
            let saved = zerus_client_core::drafts::read_draft(&store, KEY)
                .draft
                .checkpoint
                .map(|saved| *saved);
            lock(&record).push((action.clone(), id.to_owned(), saved));
            previous(action, id)
        });
        seen
    }

    fn on_disk(root: &crate::composer::tests::Scratch) -> Option<Checkpoint> {
        zerus_client_core::drafts::read_draft(&root.join("drafts"), KEY)
            .draft
            .checkpoint
            .map(|saved| *saved)
    }

    #[test]
    fn a_send_saves_its_reserved_request_id_with_the_draft_before_the_call() {
        let root = dir("checkpoint-send");
        let (c, fake) = send_hub(&root);
        let seen = record_checkpoints(&fake, &root);
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "hello".into(), 5, 5),
            SendReply::Sending
        );
        until(&c, |v| !v.sending && v.text.is_empty());
        let seen = lock(&seen).clone();
        assert_eq!(seen.len(), 1);
        let (action, id, saved) = &seen[0];
        assert!(matches!(action, Action::Send { .. }));
        let saved = saved.as_ref().expect("saved before the call");
        assert!(saved.valid());
        assert_eq!(&saved.request_id, id, "the call uses the reserved id");
        assert_eq!(saved.kind, Kind::Send);
        assert_eq!(saved.receipts, "input_receipts");
        assert_eq!(
            (saved.run_id.as_str(), saved.conversation_id.as_str()),
            ("run-1", "conv-1")
        );
        assert_eq!(saved.message_sha256, message_digest("hello", &[]));
        // A definite end sent the message: its identity goes with the draft.
        eventually(|| on_disk(&root).is_none());
    }

    #[test]
    fn an_uncertain_send_keeps_its_identity_and_a_restart_never_sends_it() {
        let root = dir("checkpoint-uncertain");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|action, id| match action {
            Action::Send { .. } => Outcome::Uncertain {
                detail: "connection lost".into(),
            },
            _ => receipt(action, id),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "maybe sent".into(), 0, 0);
        until(&c, |v| !v.sending && v.uncertain);
        let id = sent_ids(&fake);
        assert_eq!(id.len(), 1);
        let kept = on_disk(&root).expect("an uncertain outcome keeps the identity");
        assert_eq!(kept.request_id, id[0]);
        // A restart loads it as data: nothing is sent from it.
        let (again, fresh) = send_hub_on(&store);
        let view = opened(&again, ready());
        assert!(view.uncertain && !view.can_send);
        again.observe(&state(ready()));
        eventually(|| on_disk(&root).is_some_and(|saved| saved == kept));
        assert!(lock(&fresh.calls).is_empty(), "nothing is re-sent");
    }

    #[test]
    fn a_definite_failure_ends_the_identity() {
        let root = dir("checkpoint-failed");
        let (c, fake) = send_hub(&root);
        *lock(&fake.outcome) = Box::new(|action, id| match action {
            Action::Send { .. } => Outcome::Failed {
                detail: "refused".into(),
            },
            _ => receipt(action, id),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "refused".into(), 0, 0);
        let view = until(&c, |v| !v.sending && v.status_error);
        assert!(!view.uncertain);
        // The draft's next save carries no identity: nothing was sent.
        eventually(|| on_disk(&root).is_none());
    }

    #[test]
    fn a_compaction_and_the_send_after_it_each_reserve_their_own_id() {
        let root = dir("checkpoint-compact");
        let (c, fake) = send_hub(&root);
        compacts_as(&fake, "cmp-1");
        let seen = record_checkpoints(&fake, &root);
        let mut details = cold();
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "compact me".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        until(&c, |_| called(&fake, &Action::CompactContext));
        details["compact_context_request"] = json!({"request_id": "cmp-1", "status": "completed"});
        c.observe(&state(details.clone()));
        c.observe(&state(details));
        until(&c, |v| !v.sending && !v.compacting);
        let seen = lock(&seen).clone();
        assert_eq!(seen.len(), 2);
        let (compact, compact_id, compact_saved) = &seen[0];
        assert_eq!(compact, &Action::CompactContext);
        let compact_saved = compact_saved.as_ref().expect("saved before the compaction");
        assert_eq!(&compact_saved.request_id, compact_id);
        assert_eq!(compact_saved.kind, Kind::CompactToSend);
        assert_eq!(compact_saved.receipts, "compact_receipts");
        assert_eq!(
            compact_saved.message_sha256,
            message_digest("compact me", &[])
        );
        let (send, send_id, send_saved) = &seen[1];
        assert!(matches!(send, Action::Send { .. }));
        let send_saved = send_saved.as_ref().expect("saved before the send");
        assert_eq!(&send_saved.request_id, send_id);
        assert_eq!(send_saved.kind, Kind::Send);
        assert_ne!(compact_id, send_id, "one id per dispatch");
        eventually(|| on_disk(&root).is_none());
    }

    #[test]
    fn a_resume_reserves_its_id_with_the_waiting_draft_before_the_call() {
        let root = dir("checkpoint-resume");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let seen = record_checkpoints(&fake, &root);
        let mut details = ready();
        details["runtime_state"] = json!("stopped");
        details["process_state"] = json!("exited");
        let view = opened(&c, details);
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "after start".into(), 0, 0)
        else {
            panic!("no question")
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |_| called(&fake, &Action::Resume));
        let seen = lock(&seen).clone();
        let (action, id, saved) = &seen[0];
        assert_eq!(action, &Action::Resume);
        let saved = saved.as_ref().expect("saved before the resume");
        assert!(saved.valid());
        assert_eq!(&saved.request_id, id);
        assert_eq!(saved.kind, Kind::ResumeToSend);
        assert_eq!(saved.receipts, "resume_input");
        assert_eq!(saved.start, Some(Start::Resume));
        assert_eq!(saved.run_id, "run-1", "the stopped run it names");
    }

    /// review-n's #1316 probe (zerus-5wmh.66.4.4).
    #[test]
    fn uncertain_continuations_keep_their_original_checkpoint() {
        let mut retained = Vec::new();
        for start in [false, true] {
            let root = dir(if start {
                "probe-resume-unknown"
            } else {
                "probe-compact-unknown"
            });
            let store = drafts(&root);
            let (c, fake) = send_hub_on(&store);
            if start {
                stopped(&fake);
            }
            *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
                detail: "lost reply".into(),
            });
            let mut details = if start { ready() } else { cold() };
            if start {
                details["runtime_state"] = json!("stopped");
                details["process_state"] = json!("exited");
            }
            let seen = record_checkpoints(&fake, &root);
            let view = opened(&c, details);
            let first = c.send_message(view.handle, 1, view.text_epoch, "same draft".into(), 0, 0);
            let token = match first {
                SendReply::Cold { token, .. } | SendReply::Start { token, .. } => token,
                other => panic!("no first question: {other:?}"),
            };
            let answer = if start {
                Answer::Start {
                    choice: StartChoice::Resume,
                }
            } else {
                Answer::Cold {
                    action: ColdAction::Compact,
                }
            };
            assert_eq!(c.answer(token, answer), SendReply::Sending);
            let view = until(&c, |v| !v.sending && !v.compacting && v.status_error);
            store.flush();
            let held = on_disk(&root);
            let seen_first = lock(&seen).clone();
            assert_eq!(seen_first.len(), 1);
            assert_eq!(
                seen_first[0].2.as_ref().unwrap().request_id,
                seen_first[0].1
            );
            eprintln!(
                "start={start} after Unknown: uncertain={} held_checkpoint={}",
                view.uncertain,
                held.is_some()
            );
            // A normal Send/Compact or Send/Resume, with NO Allow retry.
            let second = c.send_message(view.handle, 2, view.text_epoch, "same draft".into(), 0, 0);
            if let SendReply::Cold { token, .. } | SendReply::Start { token, .. } = second {
                let answer = if start {
                    Answer::Start {
                        choice: StartChoice::Resume,
                    }
                } else {
                    Answer::Cold {
                        action: ColdAction::Compact,
                    }
                };
                c.answer(token, answer);
                until(&c, |v| !v.sending && !v.compacting && v.status_error);
                store.flush();
            }
            let calls = lock(&seen).clone();
            eprintln!(
                "start={start} without Allow retry: calls={} new_id={}",
                calls.len(),
                calls.len() == 2 && calls[0].1 != calls[1].1
            );
            retained.push(held.is_some_and(|cp| cp.request_id == seen_first[0].1));
        }
        assert_eq!(
            retained,
            vec![true, true],
            "both uncertain continuations must retain the original identity"
        );
    }

    /// zerus-1wchz: a compaction `hgs` may have typed ends uncertain, so the
    /// draft keeps that call's identity and Send stays locked until Allow
    /// retry; a refusal before any input still ends definitely and frees the
    /// draft. The real core classifier decides each end.
    #[test]
    fn a_compaction_that_may_have_typed_keeps_its_identity_until_allow_retry() {
        use zerus_client_core::actions::{classify, invocation};
        type Case = (&'static str, fn() -> Exit, bool, &'static str);
        let cases: [Case; 9] = [
            (
                "start",
                || Exit::StartFailed("hgs is missing".into()),
                false,
                "hgs is missing",
            ),
            (
                "refused",
                || Exit::Code {
                    code: 1,
                    stdout: Vec::new(),
                    stderr: "hgs: The session is no longer ready\n".into(),
                },
                false,
                "hgs: The session is no longer ready",
            ),
            (
                "typed",
                || {
                    Exit::Code {
                    code: 1,
                    stdout: Vec::new(),
                    stderr: "hgs: Compact delivery uncertain: pane gone. Check Terminal before retrying\n".into(),
                }
                },
                true,
                "hgs: Compact delivery uncertain: pane gone. Check Terminal before retrying",
            ),
            (
                "duplicate",
                || {
                    Exit::Code {
                    code: 1,
                    stdout: Vec::new(),
                    stderr: "hgs: This compact was already attempted. Check Terminal before trying again\n".into(),
                }
                },
                true,
                "hgs: This compact was already attempted. Check Terminal before trying again",
            ),
            (
                "peer",
                || Exit::Code {
                    code: 255,
                    stdout: Vec::new(),
                    stderr: "ssh: connection closed\n".into(),
                },
                true,
                "ssh: connection closed",
            ),
            (
                "timeout",
                || Exit::TimedOut,
                true,
                "Request timed out. Check Terminal before retrying.",
            ),
            (
                "write",
                || Exit::WriteFailed,
                true,
                "Transfer failed. Check Terminal before retrying.",
            ),
            (
                "abnormal",
                || Exit::Abnormal {
                    stderr: "signal 9\n".into(),
                },
                true,
                "signal 9",
            ),
            (
                "acknowledgement",
                || Exit::Code {
                    code: 0,
                    stdout: b"{}".to_vec(),
                    stderr: String::new(),
                },
                true,
                "Invalid acknowledgement. Check Terminal before retrying.",
            ),
        ];
        for (name, exit, uncertain, detail) in cases {
            let root = dir(&format!("typed-compact-{name}"));
            let store = drafts(&root);
            let (c, fake) = send_hub_on(&store);
            *lock(&fake.outcome) = Box::new(move |action, id| match action {
                Action::CompactContext => {
                    let target = Target {
                        computer: String::new(),
                        name: "claude/example/review".into(),
                        run_id: "run-1".into(),
                        conversation_id: "conv-1".into(),
                    };
                    let inv = invocation(&target, action, id).unwrap();
                    classify(&target, action, &inv, exit())
                }
                _ => receipt(action, id),
            });
            let seen = record_checkpoints(&fake, &root);
            let view = opened(&c, cold());
            let SendReply::Cold { token, .. } =
                c.send_message(view.handle, 1, view.text_epoch, "compact me".into(), 0, 0)
            else {
                panic!("{name}: no cold question")
            };
            let compact = Answer::Cold {
                action: ColdAction::Compact,
            };
            assert_eq!(c.answer(token, compact), SendReply::Sending, "{name}");
            let view = until(&c, |v| !v.sending && !v.compacting && v.status_error);
            store.flush();
            let held = on_disk(&root);
            let first = lock(&seen)[0].1.clone();
            assert_eq!(view.uncertain, uncertain, "{name}");
            let again = |rev| {
                let view = c.current().unwrap();
                c.send_message(view.handle, rev, view.text_epoch, "compact me".into(), 0, 0)
            };
            if uncertain {
                assert_eq!(
                    view.status,
                    zerus_client_core::local_messages::uncertain_detail(detail),
                    "{name}"
                );
                assert_eq!(
                    held.map(|checkpoint| checkpoint.request_id),
                    Some(first),
                    "{name}: the draft keeps the compaction's identity"
                );
                assert_eq!(again(2), SendReply::Busy, "{name}: locked");
                c.allow_retry(view.handle);
                assert!(!c.current().unwrap().uncertain, "{name}");
                assert!(
                    matches!(again(3), SendReply::Cold { .. }),
                    "{name}: Allow retry frees it"
                );
            } else {
                assert_eq!(view.status, detail, "{name}");
                assert!(held.is_none(), "{name}: no identity kept");
                assert!(
                    matches!(again(2), SendReply::Cold { .. }),
                    "{name}: a definite refusal frees the draft"
                );
            }
            assert_eq!(lock(&seen).len(), 1, "{name}: one call");
        }
    }

    /// review-n's #1316 probe (zerus-5wmh.66.4.4).
    #[test]
    fn an_inflight_cancel_or_timeout_does_not_lose_an_unknown_action() {
        let mut kept = Vec::new();
        for cancel in [true, false] {
            let root = dir(if cancel {
                "probe-cancel-inflight"
            } else {
                "probe-timeout-inflight"
            });
            let store = drafts(&root);
            let (c, fake) = send_hub_on(&store);
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let release_rx = Mutex::new(release_rx);
            *lock(&fake.outcome) = Box::new(move |_, _| {
                entered_tx.send(()).unwrap();
                lock(&release_rx)
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Outcome::Uncertain {
                    detail: "lost reply after cancellation".into(),
                }
            });
            let view = opened(&c, cold());
            let SendReply::Cold { token, .. } =
                c.send_message(view.handle, 1, view.text_epoch, "pending".into(), 0, 0)
            else {
                panic!()
            };
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact,
                },
            );
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let original = on_disk(&root).expect("saved before live call");
            if cancel {
                c.cancel_send(view.handle);
            } else {
                *lock(&fake.now) += 601.0;
                c.tick();
            }
            store.flush();
            let before_reply = on_disk(&root);
            release_tx.send(()).unwrap();
            eventually(|| lock(&fake.calls).len() == 1);
            store.flush();
            let after_reply = on_disk(&root);
            eprintln!(
                "inflight cancel={cancel}: held_before_reply={} held_after_unknown={} uncertain={}",
                before_reply.is_some(),
                after_reply.is_some(),
                c.current().unwrap().uncertain
            );
            kept.push(after_reply.is_some_and(|cp| cp.request_id == original.request_id));
        }
        assert_eq!(
            kept,
            vec![true, true],
            "cancelling the pending message does not prove the native action never ran"
        );
    }

    /// review-n's #1316 probe (zerus-5wmh.66.4.4).
    #[test]
    fn a_loaded_checkpoint_alone_fences_an_implicit_new_attempt() {
        let root = dir("probe-held-reload");
        let store = drafts(&root);
        let cp = Checkpoint::reserve(Intent {
            kind: Kind::CompactToSend,
            draft_revision: 1,
            message_sha256: message_digest("kept draft", &[]),
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
            question: None,
            start: None,
            retry_of: None,
            consent: false,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        store.save_durable(
            KEY,
            zerus_client_core::drafts::StoredDraft {
                draft: zerus_client_core::attachments::Draft {
                    text: "kept draft".into(),
                    ..Default::default()
                },
                checkpoint: Some(Box::new(cp.clone())),
                ..Default::default()
            },
            move |r| tx.send(r).unwrap(),
        );
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let (c, fake) = send_hub_on(&store);
        let view = opened(&c, ready());
        eprintln!(
            "checkpoint-only reload: uncertain={} can_send={}",
            view.uncertain, view.can_send
        );
        let reply = c.send_message(view.handle, 1, view.text_epoch, "kept draft".into(), 0, 0);
        if reply == SendReply::Sending {
            until(&c, |v| !v.sending);
        }
        let calls = lock(&fake.calls).clone();
        eprintln!(
            "checkpoint-only reload: reply={reply:?} calls={} replaced_original={}",
            calls.len(),
            calls.first().is_some_and(|(_, _, id)| id != &cp.request_id)
        );
        assert!(
            view.uncertain && calls.is_empty(),
            "a held original is not an unrestricted new send"
        );
    }

    /// review-n's #1316 probe (zerus-5wmh.66.4.4).
    #[test]
    fn a_row_retry_does_not_overwrite_another_uncertain_identity() {
        let root = dir("probe-row-retry");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "definitely refused".into(),
        });
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(
                view.handle,
                1,
                view.text_epoch,
                "first refused".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.status_error);
        let failed_row = view.local[0].id.clone();
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "second unknown".into(),
        });
        assert_eq!(
            c.send_message(
                view.handle,
                2,
                view.text_epoch,
                "second uncertain".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let held = on_disk(&root).expect("unknown original retained");
        *lock(&fake.outcome) = Box::new(receipt);
        let reply = c.retry(view.handle, &failed_row);
        if reply == SendReply::Sending {
            until(&c, |v| !v.sending);
            store.flush();
        }
        eprintln!(
            "row Retry without Allow retry: reply={reply:?} calls={} retained_original={}",
            lock(&fake.calls).len(),
            on_disk(&root).is_some_and(|cp| cp.request_id == held.request_id)
        );
        assert_eq!(
            reply,
            SendReply::Busy,
            "one unresolved identity must not be overwritten by an unrelated row Retry"
        );
    }

    /// A start or compaction ended while its call ran keeps its identity
    /// and fences a new attempt; only its own late definite answer resolves
    /// it (zerus-5wmh.66.4.4).
    #[test]
    fn a_call_ended_while_it_runs_is_resolved_only_by_its_own_definite_answer() {
        let root = dir("checkpoint-inflight-definite");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        *lock(&fake.outcome) = Box::new(move |action, id| match action {
            Action::CompactContext => {
                entered_tx.send(()).unwrap();
                lock(&release_rx)
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Outcome::Done {
                    receipt: json!({"status": "submitted", "request_id": "cmp-1"}),
                    cleanup: None,
                }
            }
            _ => receipt(action, id),
        });
        let view = opened(&c, cold());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "pending".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let original = on_disk(&root).expect("saved before the call");
        c.cancel_send(view.handle);
        let view = until(&c, |v| v.waiting.is_none() && v.uncertain);
        store.flush();
        assert_eq!(
            on_disk(&root).as_ref(),
            Some(&original),
            "held while it runs"
        );
        assert_eq!(
            c.send_message(view.handle, 2, view.text_epoch, "pending".into(), 0, 0),
            SendReply::Busy,
            "no new attempt while its outcome is unknown"
        );
        release_tx.send(()).unwrap();
        let view = until(&c, |v| !v.uncertain);
        assert_eq!(view.status, rules::CANCELLED_BY_USER);
        assert!(view.status_error && view.text == "pending");
        eventually(|| on_disk(&root).is_none());
        assert!(c.lock().orphans.is_empty());
        assert!(sends(&fake).is_empty());
    }

    /// A Retry's file read runs unlocked: an identity the draft reserved
    /// meanwhile is never replaced (zerus-5wmh.66.4.4).
    #[test]
    fn an_identity_reserved_during_a_retry_s_read_is_never_replaced() {
        let root = dir("checkpoint-retry-read");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        let id = failed_send(&c, &fake, "again");
        let view = c.current().unwrap();
        let Ok(begun) = c.retry_begin(view.handle, &id) else {
            panic!("begun")
        };
        let held = crate::composer::tests::held_checkpoint();
        c.lock().composers.get_mut(KEY).unwrap().checkpoint = Some(held.clone());
        assert_eq!(c.retry_finish(begun, Ok(Vec::new())), SendReply::Busy);
        assert_eq!(c.lock().composers[KEY].checkpoint.as_ref(), Some(&held));
        assert_eq!(sends(&fake).len(), 1, "only the failed send");
        // Without it the same Retry goes: the refusal was the held identity.
        c.lock().composers.get_mut(KEY).unwrap().checkpoint = None;
        let Ok(begun) = c.retry_begin(view.handle, &id) else {
            panic!("begun")
        };
        assert_eq!(c.retry_finish(begun, Ok(Vec::new())), SendReply::Sending);
        until(&c, |v| !v.sending);
        assert_eq!(sends(&fake).len(), 2);
    }

    /// A Retry that keeps another draft and ends uncertain leaves that draft
    /// holding the Retry's identity until Allow retry (zerus-5wmh.66.4.4).
    #[test]
    fn an_uncertain_row_retry_leaves_the_draft_holding_its_identity() {
        let root = dir("checkpoint-retry-uncertain");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        let id = failed_send(&c, &fake, "again");
        let view = c.current().unwrap();
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "newer".into(),
            0,
            0,
        );
        let view = until(&c, |v| v.text == "newer");
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "lost".into(),
        });
        assert_eq!(c.retry(view.handle, &id), SendReply::Sending);
        let view = until(&c, |v| !v.sending && v.uncertain);
        assert_eq!(view.text, "newer", "the Retry kept the draft");
        let held = c.lock().composers[KEY].checkpoint.clone().expect("held");
        assert_eq!(held.retry_of.as_deref(), Some(id.as_str()));
        assert_eq!(held.message_sha256, message_digest("again", &[]));
        eventually(|| on_disk(&root).as_ref() == Some(&held));
        assert_eq!(
            c.send_message(
                view.handle,
                view.rev + 1,
                view.text_epoch,
                "newer".into(),
                0,
                0
            ),
            SendReply::Busy,
            "Send waits for Allow retry"
        );
    }

    /// review-n's #1316 R2 probe (zerus-5wmh.66.4.4).
    #[test]
    fn completed_uncertain_calls_do_not_accumulate_dead_orphans() {
        let mut remaining = Vec::new();
        for start in [false, true] {
            let root = dir(if start {
                "probe-r2-resume-orphans"
            } else {
                "probe-r2-compact-orphans"
            });
            let store = drafts(&root);
            let (c, fake) = send_hub_on(&store);
            if start {
                stopped(&fake);
            }
            *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
                detail: "reply lost, call ended".into(),
            });
            let mut details = if start { ready() } else { cold() };
            if start {
                details["runtime_state"] = json!("stopped");
                details["process_state"] = json!("exited");
            }
            opened(&c, details);
            for n in 0..8 {
                let view = c.current().unwrap();
                let ask = c.send_message(
                    view.handle,
                    view.rev + 1,
                    view.text_epoch,
                    "kept".into(),
                    0,
                    0,
                );
                let token = match ask {
                    SendReply::Start { token, .. } | SendReply::Cold { token, .. } => token,
                    x => panic!("{x:?}"),
                };
                let answer = if start {
                    Answer::Start {
                        choice: StartChoice::Resume,
                    }
                } else {
                    Answer::Cold {
                        action: ColdAction::Compact,
                    }
                };
                assert_eq!(c.answer(token, answer), SendReply::Sending);
                let view = until(&c, |v| v.uncertain && v.waiting.is_none());
                store.flush();
                assert_eq!(lock(&fake.calls).len(), n + 1);
                c.allow_retry(view.handle);
                store.flush();
                assert!(on_disk(&root).is_none());
            }
            let count = c.lock().orphans.len();
            eprintln!(
                "start={start}: completed_calls={} retained_orphans={count}",
                lock(&fake.calls).len()
            );
            remaining.push(count);
        }
        assert_eq!(
            remaining,
            vec![0, 0],
            "a completed uncertain callback has no late callback to remove an orphan"
        );
    }

    /// review-n's #1316 R2 probe (zerus-5wmh.66.4.4).
    #[test]
    fn an_old_definite_orphan_reply_never_clears_a_newer_identity() {
        let root = dir("probe-r2-orphan-newer");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        *lock(&fake.outcome) = Box::new(move |action, _| match action {
            Action::CompactContext => {
                entered_tx.send(()).unwrap();
                lock(&release_rx)
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Outcome::Done {
                    receipt: json!({"request_id":"old-compact", "status":"submitted"}),
                    cleanup: None,
                }
            }
            Action::Send { .. } => Outcome::Uncertain {
                detail: "new send uncertain".into(),
            },
            _ => panic!("unexpected action"),
        });
        let view = opened(&c, cold());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "old draft".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let old = on_disk(&root).unwrap();
        c.cancel_send(view.handle);
        until(&c, |v| v.uncertain && v.waiting.is_none());
        c.allow_retry(view.handle);
        c.observe(&state(ready()));
        let view = c.current().unwrap();
        assert_eq!(
            c.send_message(
                view.handle,
                view.rev + 1,
                view.text_epoch,
                "new draft".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        eventually(|| {
            let hub = c.lock();
            let composer = &hub.composers[KEY];
            composer.sending && !composer.saving
        });
        store.flush();
        let newer = on_disk(&root).unwrap();
        assert_ne!(old.request_id, newer.request_id);
        release_tx.send(()).unwrap();
        until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        assert_eq!(on_disk(&root).as_ref(), Some(&newer));
        assert!(c.lock().orphans.is_empty());
        assert_eq!(lock(&fake.calls).len(), 2);
    }

    /// Only calls still in flight are tracked, and never more than
    /// [`ORPHANS`]: the oldest goes first (zerus-5wmh.66.4.4).
    #[test]
    fn the_calls_ended_in_flight_are_tracked_within_a_bound() {
        let mut orphans = VecDeque::new();
        for n in 0..ORPHANS + 5 {
            let orphan = Orphan {
                request: format!("call-{n}"),
                key: KEY.into(),
                notice: String::new(),
            };
            track_orphan(&mut orphans, orphan);
        }
        assert_eq!(orphans.len(), ORPHANS);
        assert_eq!(orphans.front().map(|o| o.request.as_str()), Some("call-5"));
        assert_eq!(
            orphans.back().map(|o| o.request.clone()),
            Some(format!("call-{}", ORPHANS + 4))
        );
    }

    /// Allow retry ends the held identity and retires the tracking of its
    /// call still in flight; that call's late answer then changes nothing
    /// (zerus-5wmh.66.4.4).
    #[test]
    fn allow_retry_retires_a_call_still_in_flight() {
        let root = dir("checkpoint-orphan-allow-retry");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        *lock(&fake.outcome) = Box::new(move |action, id| match action {
            Action::CompactContext => {
                entered_tx.send(()).unwrap();
                lock(&release_rx)
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Outcome::Done {
                    receipt: json!({"status": "submitted", "request_id": "cmp-1"}),
                    cleanup: None,
                }
            }
            _ => receipt(action, id),
        });
        let view = opened(&c, cold());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "pending".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        c.cancel_send(view.handle);
        let view = until(&c, |v| v.waiting.is_none() && v.uncertain);
        assert_eq!(c.lock().orphans.len(), 1, "tracked while in flight");
        c.allow_retry(view.handle);
        assert!(c.lock().orphans.is_empty(), "retired with its identity");
        eventually(|| on_disk(&root).is_none());
        release_tx.send(()).unwrap();
        eventually(|| lock(&fake.calls).len() == 1);
        c.flush();
        let view = c.current().unwrap();
        assert!(!view.uncertain && view.status.is_empty(), "nothing changed");
        assert!(on_disk(&root).is_none());
    }

    /// review-n's #1316 R2 probe (zerus-5wmh.66.4.4).
    #[test]
    fn old_message_history_never_resolves_a_newer_compact_identity() {
        let root = dir("probe-r2-history-identity");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "reply lost".into(),
        });
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "same draft".into(), 0, 0),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        let old = on_disk(&root).unwrap();
        assert_eq!(view.local.len(), 1);
        c.allow_retry(view.handle);
        c.observe(&state(cold()));
        let view = c.current().unwrap();
        let SendReply::Cold { token, .. } = c.send_message(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "same draft".into(),
            0,
            0,
        ) else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Sending
        );
        until(&c, |v| v.uncertain && v.waiting.is_none());
        store.flush();
        let newer = on_disk(&root).unwrap();
        assert_eq!(newer.kind, Kind::CompactToSend);
        assert_ne!(old.request_id, newer.request_id);
        let mut late = state(cold());
        late.activity.push(ActivityOut {
            computer: ComputerId::from("studio"), key: "run:run-1".into(), kind: "events".into(),
            payload: json!({"events":[{"type":"UserPromptSubmit","detail":"same draft","at":*lock(&fake.now)}]}),
        });
        c.observe(&late);
        store.flush();
        let view = c.current().unwrap();
        eprintln!(
            "old input event vs compact: uncertain={} text={:?} checkpoint_kept={}",
            view.uncertain,
            view.text,
            on_disk(&root).as_ref() == Some(&newer)
        );
        assert_eq!(
            on_disk(&root).as_ref(),
            Some(&newer),
            "an old message event does not settle a newer compaction's receipt"
        );
        assert!(view.uncertain && view.text == "same draft");
        assert!(
            view.history_text_match,
            "same text is only an advisory, never the newer receipt"
        );
    }

    #[test]
    fn history_hint_is_exact_scoped_and_updates_without_local_rows() {
        let root = dir("history-advisory");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "reply lost".into(),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "exact draft".into(), 0, 0);
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let held = on_disk(&root).unwrap();
        assert!(!view.history_text_match);
        assert!(c.lock().local.delete(KEY, &view.local[0].id));
        c.take_view();
        let replace = |kind: &str, text: &str, agent: &str| {
            let mut batch = Batch::default();
            batch.activity.push(ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "replace".into(),
                payload: json!({"events":[{"type":kind,"detail":text,"agent_id":agent}]}),
            });
            batch
        };
        c.observe(&replace("UserPromptSubmit", "exact draft", ""));
        assert!(
            c.take_view()
                .expect("history-only update publishes the hint")
                .history_text_match
        );
        for (kind, text, agent, shown) in [
            ("UserPromptSubmit", "exact draft suffix", "", false),
            ("UserPromptSubmit", " exact draft", "", false),
            ("ToolUse", "exact draft", "", false),
            ("UserPromptSubmit", "exact draft", "other-child", false),
            ("UserPromptQueued", "exact draft", "", true),
        ] {
            c.observe(&replace(kind, text, agent));
            let view = c.current().unwrap();
            assert_eq!(view.history_text_match, shown);
            assert!(view.uncertain && view.text == "exact draft");
            assert_eq!(c.lock().composers[KEY].checkpoint.as_ref(), Some(&held));
        }
        for field in ["run_id", "conversation_id"] {
            let mut wrong = ready();
            wrong[field] = json!("another");
            c.lock().followed.details = Some(Arc::new(wrong));
            assert!(!c.current().unwrap().history_text_match);
        }
        c.lock().followed.details = Some(Arc::new(ready()));
        let view = c.current().unwrap();
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "edited".into(),
            0,
            0,
        );
        assert!(!c.current().unwrap().history_text_match);
        let view = c.current().unwrap();
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "exact draft".into(),
            0,
            0,
        );
        assert!(c.current().unwrap().history_text_match);
        store.flush();
        assert_eq!(
            on_disk(&root).as_ref(),
            Some(&held),
            "no inferred receipt or checkpoint change"
        );
        assert_eq!(lock(&fake.calls).len(), 1, "no new send from the hint");
        c.allow_retry(view.handle);
        assert!(!c.current().unwrap().history_text_match);
    }

    /// History carries only text: it never settles a held identity, not
    /// even its own send's row, while a draft holding none keeps the text
    /// match (zerus-5wmh.66.4.4).
    #[test]
    fn text_only_history_never_settles_a_held_send() {
        let root = dir("checkpoint-history-own");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        let shown = |text: &str, at: f64| {
            let mut batch = state(ready());
            batch.activity.push(ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-1".into(),
                kind: "events".into(),
                payload: json!({"events": [{"type": "UserPromptSubmit", "detail": text, "at": at}]}),
            });
            batch
        };
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "reply lost".into(),
        });
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "maybe sent".into(), 0, 0),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let held = on_disk(&root).expect("held");
        assert_eq!(view.local[0].id, held.request_id, "its row carries its id");
        let now = *lock(&fake.now);
        c.observe(&shown("maybe sent", now));
        let view = until(&c, |v| v.local.is_empty());
        store.flush();
        assert!(
            view.uncertain && view.text == "maybe sent",
            "the draft stays"
        );
        assert!(
            view.history_text_match,
            "advisory only, despite the removed local row"
        );
        assert_eq!(on_disk(&root).as_ref(), Some(&held), "its identity stays");
        // Only the user's Allow retry ends it.
        c.allow_retry(view.handle);
        eventually(|| on_disk(&root).is_none());
        let view = c.current().unwrap();
        assert!(!view.history_text_match, "Allow retry hides the advisory");
        // A definite failure holds no identity: its text alone clears it.
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "refused".into(),
        });
        assert_eq!(
            c.send_message(
                view.handle,
                view.rev + 1,
                view.text_epoch,
                "plain".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        until(&c, |v| !v.sending && v.status_error && v.local.len() == 1);
        assert!(c.lock().composers[KEY].checkpoint.is_none());
        c.observe(&shown("plain", now));
        let view = until(&c, |v| v.local.is_empty());
        assert!(view.text.is_empty());
    }

    /// review-n's #1316 R3 probe (zerus-5wmh.66.4.4).
    #[test]
    fn an_old_text_event_never_proves_a_second_same_text_send() {
        let root = dir("probe-r3-two-sends");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "reply lost".into(),
        });
        let view = opened(&c, ready());
        c.send_message(view.handle, 1, view.text_epoch, "same draft".into(), 0, 0);
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let older = on_disk(&root).unwrap();
        c.allow_retry(view.handle);
        c.observe(&state(ready()));
        let view = c.current().unwrap();
        assert_eq!(
            c.send_message(
                view.handle,
                view.rev + 1,
                view.text_epoch,
                "same draft".into(),
                0,
                0
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let newer = on_disk(&root).unwrap();
        assert_ne!(older.request_id, newer.request_id);
        assert_eq!(view.local.len(), 1);
        assert_eq!(view.local[0].id, newer.request_id);
        let mut late = state(ready());
        late.activity.push(ActivityOut { computer: ComputerId::from("studio"), key: "run:run-1".into(), kind: "events".into(),
            payload: json!({"events":[{"type":"UserPromptSubmit","detail":"same draft","at":*lock(&fake.now)}]}) });
        c.observe(&late);
        store.flush();
        let view = c.current().unwrap();
        eprintln!(
            "two sends / one text event: local_rows={} uncertain={} text={:?} newer_kept={}",
            view.local.len(),
            view.uncertain,
            view.text,
            on_disk(&root).as_ref() == Some(&newer)
        );
        assert_eq!(
            on_disk(&root).as_ref(),
            Some(&newer),
            "a text match cannot manufacture proof of the second send's request ID"
        );
        assert!(view.uncertain && view.text == "same draft");
    }

    /// review-n's #1316 R3 probe (zerus-5wmh.66.4.4).
    #[test]
    fn an_evicted_inflight_entry_cannot_clear_or_misattribute_a_checkpoint() {
        for definite in [false, true] {
            let root = dir(if definite {
                "probe-r3-evict-definite"
            } else {
                "probe-r3-evict-unknown"
            });
            let store = drafts(&root);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let (c, fake) = send_hub_with(
                &store,
                Arc::new(move |job| {
                    let tx = done_tx.clone();
                    std::thread::spawn(move || {
                        job();
                        let _ = tx.send(std::thread::current().id());
                    });
                }),
            );
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let release_rx = Mutex::new(release_rx);
            *lock(&fake.outcome) = Box::new(move |_, id| {
                entered_tx.send(std::thread::current().id()).unwrap();
                lock(&release_rx)
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                if definite {
                    Outcome::Done {
                        receipt: json!({"status":"submitted","request_id":id}),
                        cleanup: None,
                    }
                } else {
                    Outcome::Uncertain {
                        detail: "lost".into(),
                    }
                }
            });
            let view = opened(&c, cold());
            let SendReply::Cold { token, .. } =
                c.send_message(view.handle, 1, view.text_epoch, "held".into(), 0, 0)
            else {
                panic!()
            };
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact,
                },
            );
            let caller_thread = entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let held = on_disk(&root).unwrap();
            c.cancel_send(view.handle);
            until(&c, |v| v.uncertain && v.waiting.is_none());
            let retained;
            {
                let mut hub = c.lock();
                assert_eq!(hub.orphans.front().unwrap().request, held.request_id);
                // One actual running callback plus 32 newer independent tracking entries.
                for n in 0..ORPHANS {
                    track_orphan(
                        &mut hub.orphans,
                        Orphan {
                            request: uuid::Uuid::new_v4().to_string(),
                            key: format!("other-{n}"),
                            notice: format!("notice-{n}"),
                        },
                    );
                }
                assert_eq!(hub.orphans.len(), ORPHANS);
                assert!(hub.orphans.iter().all(|o| o.request != held.request_id));
                retained = hub
                    .orphans
                    .iter()
                    .map(|o| (o.request.clone(), o.key.clone(), o.notice.clone()))
                    .collect::<Vec<_>>();
            }
            store.flush();
            assert_eq!(on_disk(&root).as_ref(), Some(&held));
            release_tx.send(()).unwrap();
            loop {
                if done_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    == caller_thread
                {
                    break;
                }
            }
            assert_eq!(lock(&fake.calls).len(), 1);
            c.flush();
            store.flush();
            let hub = c.lock();
            assert_eq!(hub.composers[KEY].checkpoint.as_ref(), Some(&held));
            assert!(hub.composers[KEY].uncertain && hub.composers[KEY].draft.text == "held");
            assert_eq!(
                hub.orphans
                    .iter()
                    .map(|o| (o.request.clone(), o.key.clone(), o.notice.clone()))
                    .collect::<Vec<_>>(),
                retained
            );
            eprintln!("eviction definite={definite}: original held, 32 other entries unchanged");
        }
    }

    /// review-n's #1316 R4 probe (zerus-5wmh.66.4.4).
    #[test]
    fn a_history_clear_cannot_overwrite_a_new_checkpoint() {
        let root = dir("probe-r4-history-admission-gap");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "refused".into(),
        });
        let view = opened(&c, ready());
        assert_eq!(
            c.send_message(view.handle, 1, view.text_epoch, "old text".into(), 0, 0),
            SendReply::Sending
        );
        until(&c, |v| !v.sending && v.status_error && v.local.len() == 1);
        store.flush();
        assert!(on_disk(&root).is_none());
        let mut late = state(ready());
        late.activity.push(ActivityOut {
            computer: ComputerId::from("studio"), key: "run:run-1".into(), kind: "events".into(),
            payload: json!({"events":[{"type":"UserPromptSubmit","detail":"old text","at":*lock(&fake.now)}]})
        });
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let observing = c.clone();
        let worker = std::thread::spawn(move || {
            AFTER_RECONCILE.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }))
            });
            observing.observe(&late);
        });
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("reached actual reconcile gap");
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "new reply lost".into(),
        });
        let view = c.current().unwrap();
        let begun = c.send_message(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "new held input".into(),
            0,
            0,
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        store.flush();
        let held = on_disk(&root).expect("new input has its durable checkpoint");
        let before = (view.text.clone(), view.uncertain);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        store.flush();
        let view = c.current().unwrap();
        let after = on_disk(&root);
        eprintln!("history-clear gap: begun={begun:?} before={before:?} after_text={:?} after_uncertain={} new_checkpoint_kept={}", view.text, view.uncertain, after.as_ref()==Some(&held));
        assert_eq!(
            after.as_ref(),
            Some(&held),
            "an old text-only clear must not erase the input reserved while the lock was released"
        );
        assert_eq!(view.text, "new held input");
        assert!(view.uncertain);
    }

    fn sent_ids(fake: &Fake) -> Vec<String> {
        lock(&fake.calls)
            .iter()
            .filter(|(_, action, _)| matches!(action, Action::Send { .. }))
            .map(|(_, _, id)| id.clone())
            .collect()
    }

    fn cold() -> Value {
        let mut details = ready();
        details["cache_hint"] = json!({"status": "cold"});
        details
    }

    /// A state of the other session (`other_session`).
    fn other_state(details: Value) -> Batch {
        Batch {
            activity: vec![ActivityOut {
                computer: ComputerId::from("studio"),
                key: "run:run-2".into(),
                kind: "state".into(),
                payload: json!({"value": details, "rev": 1}),
            }],
            ..Batch::default()
        }
    }

    fn revision(c: &Composers, start: bool) -> u64 {
        let hub = c.lock();
        if start {
            hub.start.as_ref().map(|(p, _)| p.revision).unwrap()
        } else {
            hub.compacts.get(KEY).map(|(p, _)| p.revision).unwrap()
        }
    }

    #[test]
    fn a_stopped_session_asks_how_to_start_and_sends_once_ready() {
        let root = dir("start");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let mut details = ready();
        details["runtime_state"] = json!("stopped");
        details["process_state"] = json!("exited");
        details["session_usage"] = json!({"context": {"used": 85_300, "limit": 200_000}});
        let view = opened(&c, details);
        assert!(view.available, "Send stays enabled to ask");
        assert_eq!(
            view.unavailable_reason,
            "Session stopped. Send asks whether to resume it or start fresh."
        );
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Resume the session to send a message.")
        );
        let SendReply::Start { token, question } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "after start".into(),
            11,
            11,
        ) else {
            panic!("no question")
        };
        assert_eq!(question.title.english(), "Start the session to send?");
        let informative = question.informative.english();
        assert!(
            informative.contains("full context (43% (85.3k/200k)).\n\n"),
            "{informative}"
        );
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Resume
                }
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| v.waiting.is_some());
        assert!(view.cancel_send && !view.can_send && !view.compacting);
        // Another start waits: Send asks nothing more (`canStartStopped`).
        assert!(!view.available);
        assert_eq!(view.unavailable_reason, WAITING_START);
        until(&c, |_| called(&fake, &Action::Resume));
        // The resume names the stopped run.
        assert_eq!(call_of(&fake, &Action::Resume).run_id, "run-1");
        // The new run reports ready: the exact draft goes out, once.
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        c.observe(&state(after.clone()));
        c.observe(&state(after));
        until(&c, |v| v.waiting.is_none() && !v.sending);
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0.run_id, "run-2");
        let view = c.current().unwrap();
        assert!(view.text.is_empty() && !view.cancel_send);
    }

    /// zerus-77ljy: the resumed run can read ready (an inspect, or the
    /// followed details) before the listing replaces the stopped row. The
    /// waiting message is not judged against that stale row, which still
    /// asks to resume and would end it with CHANGED: it waits until the
    /// listing shows the ready run, then goes once, to that run.
    #[test]
    fn a_resumed_send_waits_for_the_listing_to_show_the_ready_run() {
        let root = dir("start-listing-behind");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let mut details = ready();
        details["runtime_state"] = json!("stopped");
        details["process_state"] = json!("exited");
        let view = opened(&c, details);
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "after resume".into(),
            12,
            12,
        ) else {
            panic!("no question")
        };
        let choice = StartChoice::Resume;
        assert_eq!(
            c.answer(token, Answer::Start { choice }),
            SendReply::Sending
        );
        until(&c, |v| v.waiting.as_deref() == Some(WAITING_START));
        until(&c, |_| called(&fake, &Action::Resume));
        // The new run reads ready while the listing still shows the
        // stopped run: nothing is sent and the draft keeps waiting.
        let mut after = ready();
        after["run_id"] = json!("run-2");
        c.observe(&state(after.clone()));
        c.observe(&state(after.clone()));
        std::thread::sleep(Duration::from_millis(200));
        let view = c.current().unwrap();
        assert!(
            sends(&fake).is_empty(),
            "sent before the listing showed the new run"
        );
        assert_ne!(view.status, CHANGED);
        assert_eq!(view.waiting.as_deref(), Some(WAITING_START));
        assert_eq!(view.text, "after resume");
        // The listing catches up: the next reading sends the exact draft once.
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        c.observe(&state(after.clone()));
        c.observe(&state(after));
        until(&c, |v| v.waiting.is_none() && !v.sending);
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0.run_id, "run-2");
        let view = c.current().unwrap();
        assert!(view.text.is_empty() && view.status != CHANGED);
    }

    /// zerus-dhnd: Start fresh opens Codex without a conversation, and Codex
    /// reports no status before the first message creates one. The waiting
    /// message must go through the first-message path, not wait forever.
    #[test]
    fn a_fresh_start_without_a_status_takes_the_waiting_message_as_its_first() {
        let root = dir("fresh-first");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "first".into(), 5, 5)
        else {
            panic!("no question")
        };
        let choice = StartChoice::Fresh;
        assert_eq!(
            c.answer(token, Answer::Start { choice }),
            SendReply::Sending
        );
        until(&c, |v| v.waiting.as_deref() == Some(WAITING_START));
        until(&c, |_| called(&fake, &Action::StartFresh));
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
            row.conversation_id = None;
        }
        // `hgs inspect` as the owner's session showed it after the clear.
        let cleared = json!({"tracked": true, "run_id": "run-2", "conversation_id": null,
            "runtime_state": "live", "process_state": "running",
            "phase": "unknown", "activity": "unknown", "activity_summary": "Status unavailable",
            "cleared_conversations": ["conv-1"],
            "first_message_can_send": true, "first_message_reason": "",
            "clear_context_supported": false, "clear_context_reason": "Wait for a confirmed conversation"});
        c.observe(&state(cleared.clone()));
        c.observe(&state(cleared));
        until(&c, |v| v.waiting.is_none() && !v.sending);
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        // The new run's first message, never the cleared conversation.
        assert_eq!(
            (
                sends[0].0.run_id.as_str(),
                sends[0].0.conversation_id.as_str()
            ),
            ("run-2", "")
        );
        assert!(c.current().unwrap().text.is_empty());
    }

    #[test]
    fn a_compaction_must_complete_before_the_send() {
        let root = dir("compact");
        let (c, fake) = send_hub(&root);
        compacts_as(&fake, "cmp-1");
        let mut details = cold();
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, prompt } =
            c.send_message(view.handle, 1, view.text_epoch, "compact me".into(), 0, 0)
        else {
            panic!()
        };
        assert!(prompt
            .choices
            .iter()
            .any(|ch| ch.action == ColdAction::Compact && ch.enabled));
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| v.compacting && v.cancel_send);
        assert_eq!(view.waiting.as_deref(), Some(WAITING_COMPACT));
        assert_eq!(view.block_reason.as_deref(), Some(WAITING_COMPACT));
        until(&c, |_| called(&fake, &Action::CompactContext));
        let target = call_of(&fake, &Action::CompactContext);
        assert_eq!(
            (target.run_id.as_str(), target.conversation_id.as_str()),
            ("run-1", "conv-1")
        );
        details["compact_context_request"] = json!({"request_id": "cmp-1", "status": "running"});
        c.observe(&state(details.clone()));
        // A reading without details is no evidence of a changed conversation.
        c.observe(&state(json!({})));
        assert!(sends(&fake).is_empty());
        assert!(c.current().unwrap().compacting);
        details["compact_context_request"] = json!({"request_id": "cmp-1", "status": "completed"});
        c.observe(&state(details.clone()));
        c.observe(&state(details));
        until(&c, |v| !v.sending && !v.compacting);
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].1.as_deref(), Some("cmp-1"));
    }

    #[test]
    fn cancel_send_keeps_the_draft_and_an_interrupt_names_itself() {
        let root = dir("cancel");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "kept".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Fresh,
            },
        );
        until(&c, |v| v.waiting.is_some());
        settled(&c, true);
        // Another composer's Cancel send is not this one's.
        c.cancel_send(view.handle + 100);
        assert!(c.current().unwrap().waiting.is_some());
        c.cancel_send(view.handle);
        let view = until(&c, |v| v.waiting.is_none());
        assert_eq!(
            view.status,
            "Pending send cancelled. Your draft is unchanged."
        );
        assert!(view.status_error && !view.cancel_send);
        assert_eq!(view.text, "kept");
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "kept".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |v| v.waiting.is_some());
        settled(&c, true);
        c.interrupted(view.handle);
        assert_eq!(
            until(&c, |v| v.waiting.is_none()).status,
            "Interrupted. Your draft was kept without sending."
        );
        assert!(sends(&fake).is_empty());
    }

    fn turn(phase: &str) -> Value {
        let mut details = ready();
        details["phase"] = json!(phase);
        details["activity"] = json!(phase);
        details["prompt"] = json!("Fix the build");
        details
    }

    /// zerus-5wmh.13.4, Qt's interrupted prompt (`SessionsWindow.cpp:1192-1197,2279-2307`).
    /// Rust offers it; the webview takes it with an ordinary edit (zerus-c0xz8).
    #[test]
    fn stop_offers_the_interrupted_prompt_to_an_empty_field() {
        let root = dir("interrupted-prompt");
        let (c, _fake) = send_hub(&root);
        let view = opened(&c, turn("working"));
        c.interrupted(view.handle);
        // Still stopping: nothing yet.
        c.observe(&state(turn("tool")));
        assert!(c.current().unwrap().offer.is_none());
        c.observe(&state(turn("interrupted")));
        let offered = until(&c, |v| v.offer.is_some());
        let offer = offered.offer.clone().unwrap();
        assert_eq!(offer.text, "Fix the build");
        // The draft itself is untouched until the webview takes the offer.
        assert!(offered.text.is_empty());
        assert_eq!(offered.text_epoch, view.text_epoch);
        c.edit(
            offered.handle,
            1,
            offered.text_epoch,
            offer.text.clone(),
            13,
            13,
        );
        let taken = until(&c, |v| v.text == "Fix the build");
        assert!(taken.offer.is_none());
        // Offered once: a later interrupted state does not offer it again.
        c.edit(taken.handle, 2, taken.text_epoch, String::new(), 0, 0);
        c.observe(&state(turn("interrupted")));
        assert!(until(&c, |v| v.text.is_empty()).offer.is_none());
    }

    #[test]
    fn a_draft_the_person_started_wins_over_the_interrupted_prompt() {
        let root = dir("interrupted-draft");
        let (c, _fake) = send_hub(&root);
        // Typed before the state reads interrupted: no offer.
        let view = opened(&c, turn("working"));
        c.interrupted(view.handle);
        c.edit(
            view.handle,
            1,
            view.text_epoch,
            "my next message".into(),
            15,
            15,
        );
        c.observe(&state(turn("interrupted")));
        let view = until(&c, |v| v.text == "my next message");
        assert!(view.offer.is_none());
        // Typed after the offer, before the webview took it: the edit ends it.
        c.edit(view.handle, 2, view.text_epoch, String::new(), 0, 0);
        c.observe(&state(turn("working")));
        c.interrupted(view.handle);
        c.observe(&state(turn("interrupted")));
        let offered = until(&c, |v| v.offer.is_some());
        c.edit(
            offered.handle,
            3,
            offered.text_epoch,
            "typed meanwhile".into(),
            15,
            15,
        );
        let kept = until(&c, |v| v.text == "typed meanwhile");
        assert!(kept.offer.is_none());
    }

    #[test]
    fn a_turn_that_ends_another_way_or_a_refused_stop_offers_nothing() {
        let root = dir("interrupted-other");
        let (c, _fake) = send_hub(&root);
        let view = opened(&c, turn("working"));
        // The turn ended by itself before it read interrupted.
        c.interrupted(view.handle);
        c.observe(&state(turn("idle")));
        c.observe(&state(turn("interrupted")));
        assert!(c.current().unwrap().offer.is_none());
        // Another run took the session.
        c.observe(&state(turn("working")));
        c.interrupted(view.handle);
        let mut moved = turn("interrupted");
        moved["run_id"] = json!("run-2");
        c.observe(&state(moved));
        c.observe(&state(turn("interrupted")));
        assert!(c.current().unwrap().offer.is_none());
        // The interrupt request was refused: another Stop's number drops nothing.
        c.observe(&state(turn("working")));
        let stop = c.interrupted(view.handle);
        assert_ne!(stop, 0);
        c.interrupt_failed(stop + 100);
        c.interrupt_failed(stop);
        c.observe(&state(turn("interrupted")));
        let view = c.current().unwrap();
        assert!(view.offer.is_none() && view.text.is_empty());
    }

    /// zerus-le5s: the start gave up at 180 s, then the session moved on
    /// (the owner unblocked it in Terminal). Its red notice must go once the
    /// session shows it, or once the kept draft is cleared, never sooner.
    fn gave_up_at(c: &Composers, fake: &Fake, details: &Value) -> super::super::ComposerView {
        stopped(fake);
        let view = opened(c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "kept".into(), 4, 4)
        else {
            panic!("no question")
        };
        let choice = StartChoice::Resume;
        assert_eq!(
            c.answer(token, Answer::Start { choice }),
            SendReply::Sending
        );
        until(c, |_| called(fake, &Action::Resume));
        settled(c, true);
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        c.observe(&state(details.clone()));
        *lock(&fake.now) += 181.0;
        c.tick();
        let view = until(c, |v| v.waiting.is_none());
        assert_eq!(view.status, rules::NOT_READY);
        assert!(view.status_error && view.text == "kept");
        view
    }

    /// The notice and its error colour are gone; the status line may show
    /// the session's own block reason instead.
    fn gone(view: &super::super::ComposerView) -> bool {
        view.status != rules::NOT_READY && !view.status_error
    }

    #[test]
    fn a_given_up_start_notice_goes_once_the_session_moves_on() {
        // Given up while the session still reported no status.
        let root = dir("gave-up-moved");
        let (c, fake) = send_hub(&root);
        let mut unknown = ready();
        unknown["run_id"] = json!("run-2");
        unknown["phase"] = json!("unknown");
        unknown["activity"] = json!("unknown");
        gave_up_at(&c, &fake, &unknown);
        // The same reading again changes nothing.
        c.observe(&state(unknown.clone()));
        assert_eq!(c.current().unwrap().status, rules::NOT_READY);
        // A new turn starts: the notice goes, the kept draft stays.
        let mut working = unknown;
        working["phase"] = json!("working");
        working["activity"] = json!("busy");
        c.observe(&state(working));
        let view = c.current().unwrap();
        assert!(gone(&view), "{view:?}");
        assert_eq!(view.text, "kept");
        assert!(sends(&fake).is_empty());

        // Given up while the session looked ready but could not take it: the
        // same ready reading keeps the notice, a turn clears it.
        let root = dir("gave-up-ready");
        let (c, fake) = send_hub(&root);
        let mut stuck = ready();
        stuck["run_id"] = json!("run-2");
        stuck["expected_id"] = json!("conv-1");
        stuck["resume_message_can_send"] = json!(false);
        gave_up_at(&c, &fake, &stuck);
        c.observe(&state(stuck.clone()));
        assert_eq!(c.current().unwrap().status, rules::NOT_READY);
        let mut busy = stuck;
        busy["activity"] = json!("busy");
        busy["phase"] = json!("working");
        c.observe(&state(busy.clone()));
        assert!(gone(&c.current().unwrap()));
        // Ready again after a given-up start that saw it working.
        let root = dir("gave-up-busy");
        let (c, fake) = send_hub(&root);
        gave_up_at(&c, &fake, &busy);
        let mut idle = busy;
        idle["activity"] = json!("idle");
        idle["phase"] = json!("idle");
        idle["expected_id"] = json!("");
        c.observe(&state(idle));
        assert!(gone(&c.current().unwrap()));
    }

    /// Given up while the session still reported no status (review of #1220).
    fn timed_out_unknown(c: &Composers, fake: &Fake) -> (super::super::ComposerView, Value) {
        let mut details = ready();
        details["run_id"] = json!("run-2");
        details["phase"] = json!("unknown");
        details["activity"] = json!("unknown");
        let view = gave_up_at(c, fake, &details);
        (view, details)
    }

    #[test]
    fn a_given_up_start_never_dismisses_a_newer_error() {
        let root = dir("gave-up-new-error");
        let (c, fake) = send_hub(&root);
        let (view, mut details) = timed_out_unknown(&c, &fake);
        assert!(
            c.can_attach(view.handle),
            "attachment control must be available"
        );
        c.attach_paths(
            view.handle,
            vec![root.join("missing.txt")],
            zerus_client_core::attachments::Selection::default(),
        );
        let failed = until(&c, |v| {
            !v.attaching && v.status_error && v.status != rules::NOT_READY
        });
        assert!(!failed.status.is_empty());
        details["phase"] = json!("working");
        details["activity"] = json!("busy");
        c.observe(&state(details));
        let after = c.current().unwrap();
        assert_eq!(after.status, failed.status, "the newer error stays");
        assert!(after.status_error);
    }

    #[test]
    fn a_given_up_start_notice_stays_for_another_run_or_conversation() {
        for (key, value) in [("run_id", "run-1"), ("conversation_id", "conv-9")] {
            let root = dir(&format!("gave-up-other-{key}"));
            let (c, fake) = send_hub(&root);
            let (_, mut details) = timed_out_unknown(&c, &fake);
            details[key] = json!(value);
            details["phase"] = json!("working");
            details["activity"] = json!("busy");
            c.observe(&state(details));
            assert_eq!(c.current().unwrap().status, rules::NOT_READY, "{key}");
        }
    }

    #[test]
    fn a_given_up_start_notice_goes_with_its_cleared_draft() {
        let root = dir("gave-up-cleared");
        let (c, fake) = send_hub(&root);
        let mut unknown = ready();
        unknown["run_id"] = json!("run-2");
        unknown["phase"] = json!("unknown");
        unknown["activity"] = json!("unknown");
        let view = gave_up_at(&c, &fake, &unknown);
        // Editing the kept draft keeps the notice; clearing it removes it.
        c.edit(view.handle, 2, view.text_epoch, "kep".into(), 3, 3);
        assert_eq!(c.current().unwrap().status, rules::NOT_READY);
        c.edit(view.handle, 3, view.text_epoch, String::new(), 0, 0);
        let view = c.current().unwrap();
        assert!(gone(&view), "{view:?}");
    }

    #[test]
    fn a_start_that_never_becomes_ready_gives_up_after_180_s() {
        let root = dir("timeout");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "late".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |v| v.waiting.is_some());
        settled(&c, true);
        *lock(&fake.now) += 179.0;
        c.tick();
        assert!(c.current().unwrap().waiting.is_some(), "still within 180 s");
        *lock(&fake.now) += 2.0;
        c.tick();
        let view = until(&c, |v| v.waiting.is_none());
        assert_eq!(
            view.status,
            "The agent did not become ready. Check Terminal; your draft was kept without sending."
        );
        assert!(sends(&fake).is_empty());
    }

    #[test]
    fn a_compaction_that_never_completes_gives_up_after_600_s() {
        let root = dir("compact-timeout");
        let (c, fake) = send_hub(&root);
        compacts_as(&fake, "cmp-1");
        let view = opened(&c, cold());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "slow".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        until(&c, |_| called(&fake, &Action::CompactContext));
        settled(&c, false);
        *lock(&fake.now) += 601.0;
        c.tick();
        let view = until(&c, |v| !v.compacting);
        assert_eq!(
            view.status,
            "Compaction was not confirmed. Check Terminal; your draft was kept without sending."
        );
        assert_eq!(view.text, "slow");
        assert!(sends(&fake).is_empty());
    }

    #[test]
    fn an_unselected_clear_control_uses_the_rows_inspection() {
        let root = dir("clear-unselected");
        let (c, fake) = send_hub(&root);
        *lock(&fake.inspected) = Some(ready());
        let control = c.clear_control_for(session()).unwrap();
        assert!(control.enabled && !control.dismiss && control.cache_enabled);
        assert_eq!(
            (control.run.as_str(), control.conversation.as_str()),
            ("run-1", "conv-1")
        );
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 1);
        *lock(&fake.inspected) = Some(json!({
            "run_id": "somebody-else",
            "conversation_id": "conv-1",
            "clear_context_supported": true
        }));
        assert!(c.clear_control_for(session()).is_none());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 2);
        *lock(&fake.listed) = false;
        assert!(c.clear_control_for(session()).is_none());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn confirmed_clear_survives_selection_but_refuses_changed_native_control() {
        let root = dir("clear-captured");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        *lock(&fake.inspected) = Some(ready());
        let captured = zerus_client_core::end_action::Captured {
            target: Target {
                computer: session().computer,
                name: session().name,
                run_id: "run-1".into(),
                conversation_id: "conv-1".into(),
            },
            action: Action::ClearContext {
                dismiss_prompt: false,
            },
            state: "running".into(),
        };
        c.open(None);
        let mut calls = Vec::new();
        let reply = c.clear_captured_using(&captured, |target, action, _| {
            calls.push((target, action));
            Ok(Outcome::Done {
                receipt: json!({"status": "confirmed"}),
                cleanup: None,
            })
        });
        assert_eq!(reply, SendReply::Sending);
        assert_eq!(calls, [(captured.target.clone(), captured.action.clone())]);
        assert!(c.lock().clearing.is_none());

        for details in [
            json!({"run_id": "run-2", "conversation_id": "conv-1", "clear_context_supported": true}),
            json!({"run_id": "run-1", "conversation_id": "conv-2", "clear_context_supported": true}),
            json!({"run_id": "run-1", "conversation_id": "conv-1", "clear_context_supported": false}),
        ] {
            *lock(&fake.inspected) = Some(details);
            assert_eq!(
                c.clear_captured_using(&captured, |_, _, _| panic!("changed target dispatched")),
                SendReply::Busy
            );
        }
        *lock(&fake.inspected) = Some(ready());
        *lock(&fake.online) = false;
        assert_eq!(
            c.clear_captured_using(&captured, |_, _, _| panic!("offline target dispatched")),
            SendReply::Busy
        );
        *lock(&fake.online) = true;
        c.lock().clearing = Some("another-session".into());
        assert_eq!(
            c.clear_captured_using(&captured, |_, _, _| panic!("concurrent reset dispatched")),
            SendReply::Busy
        );
    }

    /// zerus-fsht5: a confirmed Clear of the open session keyed by its durable
    /// ID guards and answers that open composer, not a name key.
    #[test]
    fn confirmed_clear_of_the_open_id_keyed_session_answers_its_notice_and_guards_its_composer() {
        const ID: &str = "11111111-2222-4333-8444-555555555555";
        let root = dir("clear-captured-id");
        let (c, fake) = send_hub(&root);
        lock(&fake.row).session_id = Some(ID.into());
        let id_session = SessionRef {
            session_id: Some(ID.into()),
            ..session()
        };
        c.open(Some(id_session));
        c.observe(&state(ready()));
        until(&c, |v| v.loaded);
        let open_key = c.lock().open_key.clone().unwrap();
        assert!(open_key.contains(ID), "the webview opens the id key");
        *lock(&fake.inspected) = Some(ready());
        let captured = zerus_client_core::end_action::Captured {
            target: Target {
                computer: session().computer,
                name: session().name,
                run_id: "run-1".into(),
                conversation_id: "conv-1".into(),
            },
            action: Action::ClearContext {
                dismiss_prompt: false,
            },
            state: "running".into(),
        };
        // The open composer of that session is sending: nothing is cleared.
        c.lock().composers.get_mut(&open_key).unwrap().sending = true;
        assert_eq!(
            c.clear_captured_using(&captured, |_, _, _| panic!("cleared while sending")),
            SendReply::Busy
        );
        c.lock().composers.get_mut(&open_key).unwrap().sending = false;
        // Details read by inspection, so the refresh after the clear is visible.
        c.lock().followed.streamed = false;
        let before = fake.inspects.load(Ordering::SeqCst);
        let mut clearing = None;
        let reply = c.clear_captured_using(&captured, |_, _, _| {
            clearing = c.lock().clearing.clone();
            Ok(Outcome::Done {
                receipt: json!({"status": "confirmed"}),
                cleanup: None,
            })
        });
        assert_eq!(
            reply,
            SendReply::Notice {
                text: CLEARED.into(),
                error: false
            }
        );
        assert_eq!(clearing.as_deref(), Some(open_key.as_str()));
        assert!(c.lock().clearing.is_none());
        // One inspection checks the control, then one refresh reads the details again.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fake.inspects.load(Ordering::SeqCst) < before + 2
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        while c.lock().followed.inspecting.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(fake.inspects.load(Ordering::SeqCst), before + 2);
        // A row's own control (named without its ID) sees its composer's send
        // under the same draft key: Clear stays disabled while it sends.
        c.lock().composers.get_mut(&open_key).unwrap().sending = true;
        let control = c
            .clear_control_for(SessionRef {
                session_id: None,
                ..session()
            })
            .unwrap();
        assert!(
            !control.enabled,
            "a sending composer disables its row's Clear"
        );
        c.lock().composers.get_mut(&open_key).unwrap().sending = false;
    }

    #[test]
    fn clear_control_refuses_a_caller_id_that_disagrees_with_the_listed_row() {
        const ROW_ID: &str = "11111111-2222-4333-8444-555555555555";
        const CALLER_ID: &str = "66666666-7777-4333-8444-888888888888";
        let root = dir("clear-control-row-id");
        let (c, fake) = send_hub(&root);
        lock(&fake.row).session_id = Some(ROW_ID.into());
        *lock(&fake.inspected) = Some(ready());
        assert!(c
            .clear_control_for(SessionRef {
                session_id: Some(CALLER_ID.into()),
                ..session()
            })
            .is_none());
        assert_eq!(fake.inspects.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn delayed_clear_of_a_renamed_row_uses_its_run_and_durable_draft_key() {
        const ID: &str = "11111111-2222-4333-8444-555555555555";
        let root = dir("clear-captured-renamed");
        let listed = Arc::new(Mutex::new({
            let mut row = row(&session().name);
            row.session_id = Some(ID.into());
            row
        }));
        let rows = Arc::clone(&listed);
        let (c, fake) = send_hub_on_rows(
            &root,
            Arc::new(move |_, key, name| {
                let row = lock(&rows).clone();
                (key == format!("run:{}", row.run_id.as_deref().unwrap_or_default())
                    || name == row.name)
                    .then_some((row, true))
            }),
        );
        let id_session = SessionRef {
            session_id: Some(ID.into()),
            ..session()
        };
        c.open(Some(id_session));
        c.observe(&state(ready()));
        until(&c, |v| v.loaded);
        let open_key = c.lock().open_key.clone().unwrap();
        assert!(open_key.contains(ID));
        lock(&listed).name = "claude/sample-project/renamed".into();
        *lock(&fake.inspected) = Some(ready());
        let captured = zerus_client_core::end_action::Captured {
            target: Target {
                computer: session().computer,
                name: session().name,
                run_id: "run-1".into(),
                conversation_id: "conv-1".into(),
            },
            action: Action::ClearContext {
                dismiss_prompt: false,
            },
            state: "running".into(),
        };
        c.lock().composers.get_mut(&open_key).unwrap().sending = true;
        assert_eq!(
            c.clear_captured_using(&captured, |_, _, _| panic!("cleared while sending")),
            SendReply::Busy
        );
        c.lock().composers.get_mut(&open_key).unwrap().sending = false;
        let mut clearing = None;
        let reply = c.clear_captured_using(&captured, |target, action, _| {
            assert_eq!(target, captured.target);
            assert_eq!(action, captured.action);
            clearing = c.lock().clearing.clone();
            Ok(Outcome::Done {
                receipt: json!({"status": "confirmed"}),
                cleanup: None,
            })
        });
        assert_eq!(
            reply,
            SendReply::Notice {
                text: CLEARED.into(),
                error: false
            }
        );
        assert_eq!(clearing.as_deref(), Some(open_key.as_str()));
        assert!(c.lock().clearing.is_none());
        assert!(lock(&fake.inspected_targets)
            .iter()
            .all(|target| target.name == captured.target.name));
    }

    #[test]
    fn delayed_clear_with_an_absent_run_does_not_touch_the_reused_name() {
        const OTHER_ID: &str = "66666666-7777-4333-8444-888888888888";
        let root = dir("clear-captured-absent-run");
        let mut other_row = row(&session().name);
        other_row.run_id = Some("run-2".into());
        other_row.conversation_id = Some("conv-2".into());
        other_row.session_id = Some(OTHER_ID.into());
        let listed = other_row.clone();
        let (c, fake) = send_hub_on_rows(
            &root,
            Arc::new(move |_, key, name| {
                (key == "run:run-2" || (key == "run:run-1" && name == listed.name))
                    .then_some((listed.clone(), true))
            }),
        );
        c.open(Some(SessionRef {
            key: "run:run-2".into(),
            session_id: Some(OTHER_ID.into()),
            ..session()
        }));
        let other_key = c.lock().open_key.clone().unwrap();
        assert!(other_key.contains(OTHER_ID));
        until(&c, |v| v.loaded);
        let before = c.current().unwrap();
        let mut details = ready();
        details["run_id"] = json!("run-2");
        details["conversation_id"] = json!("conv-2");
        *lock(&fake.inspected) = Some(details);
        let captured = zerus_client_core::end_action::Captured {
            target: Target {
                computer: session().computer,
                name: session().name,
                run_id: "run-1".into(),
                conversation_id: "conv-1".into(),
            },
            action: Action::ClearContext {
                dismiss_prompt: false,
            },
            state: "running".into(),
        };
        assert_eq!(
            c.clear_captured_using(&captured, |_, _, _| panic!("reused name dispatched")),
            SendReply::Busy
        );
        assert_eq!(c.current().unwrap(), before);
        assert!(c.lock().clearing.is_none());
        assert!(lock(&fake.calls).is_empty());
    }

    #[test]
    fn clear_context_runs_with_the_identity_the_dialog_showed() {
        let root = dir("clear");
        let (c, fake) = send_hub(&root);
        lock(&fake.row).machine = "studio".into();
        let view = opened(&c, ready());
        assert!(view.clear.enabled && !view.clear.dismiss && view.clear.cache_enabled);
        assert_eq!(view.clear.title, "Clear session?");
        assert_eq!(view.clear.confirm, "Clear session");
        assert_eq!(view.clear.text, "Clear the conversation context in claude/sample-project/review on studio?\n\nThis runs the agent’s native clear command. Future messages will start without the current conversation context. Your unsent draft will be kept.");
        assert_eq!(
            (view.clear.run.as_str(), view.clear.conversation.as_str()),
            ("run-1", "conv-1")
        );
        // Qt answers once the call ended (`sessionActionFinished`, 1025).
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: CLEAR_SENT.into(),
                error: false
            }
        );
        let (target, action, _) = lock(&fake.calls).last().cloned().unwrap();
        assert_eq!(
            action,
            Action::ClearContext {
                dismiss_prompt: false
            }
        );
        assert_eq!(
            (target.run_id.as_str(), target.conversation_id.as_str()),
            ("run-1", "conv-1")
        );
        *lock(&fake.outcome) = Box::new(|_, id| Outcome::Done {
            receipt: json!({"status": "confirmed", "request_id": id}),
            cleanup: None,
        });
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: CLEARED.into(),
                error: false
            }
        );
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Failed {
            detail: "hgs: refused".into(),
        });
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: "hgs: refused".into(),
                error: true
            }
        );
        // A call whose thread died is no success either, and runs once.
        let tries = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&tries);
        *lock(&fake.outcome) = Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            panic!("the call's thread died")
        });
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: ABNORMAL.into(),
                error: true
            }
        );
        assert_eq!(tries.load(Ordering::SeqCst), 1, "one call, not repeated");
        assert!(
            c.current().unwrap().clear.enabled,
            "one ended, the next may run"
        );
    }

    /// A taken write lane refuses before anything runs: Qt's block reason
    /// for that write, in the error tone (4391, 4395), never "busy".
    #[test]
    fn a_clear_refused_by_a_taken_lane_names_the_write() {
        let root = dir("clear-busy");
        let (c, fake) = send_hub(&root);
        *lock(&fake.taken) = Box::new(|action| matches!(action, Action::ClearContext { .. }));
        let view = opened(&c, ready());
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: "Wait for the session update to finish.".into(),
                error: true
            }
        );
        // The session's own termination holds the lane (Qt's `canClear`
        // does not look at it, 3798-3799).
        *lock(&fake.write) = Some(WriteKind::Terminate);
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Notice {
                text: "The session is terminating. Your draft stays here.".into(),
                error: true
            }
        );
        *lock(&fake.write) = None;
        let view = c.current().unwrap();
        assert!(!view.uncertain && view.clear.enabled && !view.clear.running);
        assert!(lock(&fake.calls).is_empty());
    }

    #[test]
    fn a_clear_confirmed_for_another_conversation_does_nothing() {
        let root = dir("clear-moved");
        let (c, fake) = send_hub(&root);
        let view = opened(&c, ready());
        let shown = view.clear.clone();
        let mut moved = ready();
        moved["conversation_id"] = json!("conv-2");
        c.observe(&state(moved));
        assert_eq!(
            c.clear_context(view.handle, &shown.run, &shown.conversation, shown.dismiss),
            SendReply::Busy
        );
        // Nor for another run, or the other variant of the dialog.
        assert_eq!(
            c.clear_context(view.handle, "run-0", "conv-2", false),
            SendReply::Busy
        );
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-2", true),
            SendReply::Busy
        );
        assert!(
            lock(&fake.calls).is_empty(),
            "Qt returns quietly when the session changed under its dialog"
        );
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-2", false),
            SendReply::Notice {
                text: CLEAR_SENT.into(),
                error: false
            }
        );
    }

    #[test]
    fn clear_session_follows_qts_rules() {
        let root = dir("clear-rules");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        lock(&fake.row).machine = "studio".into();
        let view = opened(&c, ready());
        assert!(view.clear.enabled && view.clear.reason.is_empty());
        // A Claude chooser: Dismiss and clear, never from the cache popover.
        let mut chooser = ready();
        chooser["clear_context_supported"] = json!(false);
        chooser["clear_context_dismiss"] = json!(true);
        chooser["clear_context_reason"] = json!("a   question waits");
        c.observe(&state(chooser.clone()));
        let clear = c.current().unwrap().clear;
        assert!(clear.enabled && clear.dismiss && !clear.cache_enabled);
        assert_eq!(clear.confirm, "Dismiss and clear");
        assert_eq!(clear.text, "Claude in claude/sample-project/review on studio is waiting for a choice in Terminal. Dismiss it and clear the conversation context?\n\nThis presses Esc once, as in Terminal: a pending approval is denied and a question stays unanswered. Then the agent’s native clear command runs. Your unsent draft will be kept.");
        assert_eq!(
            (clear.reason.as_str(), clear.cache_reason.as_str()),
            ("", "a question waits.")
        );
        // Neither: nothing clears, and both say why.
        chooser["clear_context_dismiss"] = json!(false);
        c.observe(&state(chooser));
        let clear = c.current().unwrap().clear;
        assert!(!clear.enabled && !clear.cache_enabled);
        assert_eq!(clear.reason, "a question waits.");
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", true),
            SendReply::Busy
        );
        c.observe(&state(ready()));
        // Offline: nothing clears, and the last reading explains nothing.
        *lock(&fake.online) = false;
        c.tick();
        let clear = c.current().unwrap().clear;
        assert!(!clear.enabled && !clear.cache_enabled && clear.cache_reason.is_empty());
        *lock(&fake.online) = true;
        // An archived session.
        lock(&fake.row).state = "archived".into();
        c.tick();
        assert!(!c.current().unwrap().clear.enabled);
        lock(&fake.row).state = "running".into();
        c.tick();
        assert!(c.current().unwrap().clear.enabled);
        // A reset of this session marked by the actions (`WriteKind::Clear`).
        *lock(&fake.write) = Some(WriteKind::Clear);
        c.tick();
        let view = c.current().unwrap();
        assert!(view.clear.running && !view.clear.enabled);
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Wait for the context reset to finish. Your draft stays here.")
        );
        *lock(&fake.write) = None;
        c.tick();
        // While its own message is sent (Qt's `isSending`).
        let release = hold(&store);
        let view = c.current().unwrap();
        c.send_message(view.handle, 1, view.text_epoch, "hello".into(), 5, 5);
        let view = until(&c, |v| v.sending);
        assert!(!view.clear.enabled && view.clear.cache_enabled);
        assert_eq!(
            c.clear_context(view.handle, "run-1", "conv-1", false),
            SendReply::Busy
        );
        release.send(()).unwrap();
        until(&c, |v| !v.sending);
        // After a failed read (`m_inspectError`).
        c.observe(&ended());
        *lock(&fake.now) += 1.0;
        c.tick();
        let view = until(&c, |v| !v.session_status.is_empty());
        assert!(!view.clear.enabled);
        assert!(!lock(&fake.calls)
            .iter()
            .any(|(_, a, _)| matches!(a, Action::ClearContext { .. })));
    }

    #[test]
    fn a_reset_blocks_its_session_and_runs_one_at_a_time() {
        let root = dir("clear-running");
        let (c, fake) = send_hub(&root);
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        *lock(&fake.outcome) = Box::new(move |action, id| {
            if matches!(action, Action::ClearContext { .. }) {
                let _ = lock(&gate).recv_timeout(std::time::Duration::from_secs(5));
            }
            receipt(action, id)
        });
        let view = opened(&c, ready());
        let worker = {
            let c = c.clone();
            let handle = view.handle;
            std::thread::spawn(move || c.clear_context(handle, "run-1", "conv-1", false))
        };
        let view = until(&c, |v| v.clear.running);
        assert!(!view.clear.enabled && !view.clear.cache_enabled);
        assert_eq!(
            view.block_reason.as_deref(),
            Some("Wait for the context reset to finish. Your draft stays here.")
        );
        // Another session: no reset while one runs anywhere, and its cold
        // cache question offers none (`m_clearRequest`, 4825).
        c.open(Some(other_session()));
        c.observe(&other_state(cold()));
        let other = until(&c, |v| v.loaded && v.key == OTHER);
        assert!(other.clear.running && !other.clear.enabled);
        assert!(other.block_reason.is_none(), "the reset is not its own");
        assert_eq!(
            c.clear_context(other.handle, "run-1", "conv-1", false),
            SendReply::Busy
        );
        let SendReply::Cold { token, prompt } =
            c.send_message(other.handle, 1, other.text_epoch, "elsewhere".into(), 0, 0)
        else {
            panic!("no cold question")
        };
        assert!(prompt
            .choices
            .iter()
            .any(|ch| ch.action == ColdAction::Clear && !ch.enabled));
        c.answer(token, Answer::Cancel);
        release.send(()).unwrap();
        // Its notice is for its own session only (1024).
        assert_eq!(worker.join().unwrap(), SendReply::Sending);
        let other = until(&c, |v| !v.clear.running);
        assert!(other.clear.enabled);
        assert_eq!(
            lock(&fake.calls)
                .iter()
                .filter(|(_, a, _)| matches!(a, Action::ClearContext { .. }))
                .count(),
            1
        );
    }

    /// Task 4b review M1: a cancel names the revision it cancels. Every
    /// cancel that arrives once the continuation's send began ends nothing.
    #[test]
    fn a_late_cancel_never_ends_the_delivery_that_followed() {
        let root = dir("late-cancel");
        let (c, fake) = send_hub(&root);
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        *lock(&fake.outcome) = Box::new(move |action, id| match action {
            Action::CompactContext => Outcome::Done {
                receipt: json!({"status": "submitted", "request_id": "cmp-1"}),
                cleanup: None,
            },
            Action::Send { .. } => {
                let _ = lock(&gate).recv_timeout(std::time::Duration::from_secs(5));
                receipt(action, id)
            }
            _ => receipt(action, id),
        });
        let mut details = cold();
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "goes once".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        until(&c, |_| called(&fake, &Action::CompactContext));
        let waited = revision(&c, false);
        details["compact_context_request"] = json!({"request_id": "cmp-1", "status": "completed"});
        c.observe(&state(details));
        let view = until(&c, |v| v.sending);
        assert!(!view.compacting && !view.cancel_send);
        c.cancel_send(view.handle);
        c.interrupted(view.handle);
        c.cancel(KEY, false, waited, rules::CANCELLED_BY_USER);
        c.step(
            KEY,
            false,
            waited,
            Step::Cancelled("Compaction was not confirmed.".into()),
            None,
        );
        assert!(c.current().unwrap().sending, "the delivery goes on");
        release.send(()).unwrap();
        let view = until(&c, |v| !v.sending);
        assert!(view.text.is_empty() && !view.status_error && !view.uncertain);
        assert_eq!(sends(&fake).len(), 1);
    }

    #[test]
    fn a_stale_step_never_touches_a_newer_continuation() {
        let root = dir("stale-step");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "first".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |v| v.waiting.is_some());
        settled(&c, true);
        let first = revision(&c, true);
        c.cancel_send(view.handle);
        let view = until(&c, |v| v.waiting.is_none());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "first".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Fresh,
            },
        );
        until(&c, |v| v.waiting.is_some());
        let second = revision(&c, true);
        assert_ne!(first, second);
        // The first continuation's late answers.
        c.step(
            KEY,
            true,
            first,
            Step::Cancelled(rules::INTERRUPTED.into()),
            None,
        );
        let ready_run = Arc::new(ready());
        c.step(
            KEY,
            true,
            first,
            Step::Send {
                run_id: "run-9".into(),
                conversation_id: "conv-9".into(),
                compaction_id: None,
            },
            Some(&ready_run),
        );
        c.cancel(KEY, true, first, rules::CANCELLED_BY_USER);
        let view = c.current().unwrap();
        assert_eq!(view.waiting.as_deref(), Some(WAITING_START));
        assert_eq!(revision(&c, true), second);
        assert!(sends(&fake).is_empty());
    }

    #[test]
    fn a_new_stream_frame_wins_over_a_waiting_send_readiness_poll() {
        let root = dir("resume-stream-read-race");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "only when ready".into(),
            0,
            0,
        ) else {
            panic!("start question");
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |_| called(&fake, &Action::Resume));
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        let mut starting = after.clone();
        starting["phase"] = json!("starting");
        c.observe(&state(starting));
        let mut held = lock(&fake.inspected);
        *held = Some(after.clone());
        *lock(&fake.now) += POLL;
        c.tick();
        eventually(|| fake.inspects.load(Ordering::SeqCst) == 1);
        let mut approval = after.clone();
        approval["phase"] = json!("approval");
        c.observe(&state(approval.clone()));
        drop(held);
        eventually(|| c.lock().followed.inspecting.is_none());
        assert_eq!(c.lock().followed.details.as_deref(), Some(&approval));
        assert!(sends(&fake).is_empty());
        c.observe(&state(after));
        until(&c, |v| v.waiting.is_none() && !v.sending);
        assert_eq!(sends(&fake).len(), 1);
    }

    #[test]
    fn resumed_send_polls_past_a_streamed_starting_snapshot_without_switching_views() {
        let root = dir("resume-streamed-starting");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        compacts_as(&fake, "cmp-polled");
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "send once after resume".into(),
            0,
            0,
        ) else {
            panic!("start question");
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::ResumeCompact,
            },
        );
        until(&c, |_| called(&fake, &Action::Resume));
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        let mut starting = after.clone();
        starting["phase"] = json!("starting");
        c.observe(&state(starting));
        *lock(&fake.inspected) = Some(after.clone());
        *lock(&fake.now) += POLL;
        c.tick();
        until(&c, |_| called(&fake, &Action::CompactContext));
        assert_eq!(call_of(&fake, &Action::CompactContext).run_id, "run-2");
        assert!(sends(&fake).is_empty());
        after["compact_context_request"] = json!({"request_id":"cmp-polled","status":"completed"});
        *lock(&fake.inspected) = Some(after);
        *lock(&fake.now) += POLL;
        c.tick();
        until(&c, |v| v.waiting.is_none() && !v.sending);
        assert_eq!(sends(&fake).len(), 1);
        assert_eq!(sends(&fake)[0].0.run_id, "run-2");
        assert_eq!(sends(&fake)[0].1.as_deref(), Some("cmp-polled"));
        *lock(&fake.now) += POLL;
        c.tick();
        assert_eq!(sends(&fake).len(), 1);
    }

    #[test]
    fn resume_compact_and_send_compacts_the_new_conversation_first() {
        let root = dir("resume-compact");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        compacts_as(&fake, "cmp-2");
        let view = opened(&c, ready());
        let SendReply::Start { token, question } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "summarize first".into(),
            0,
            0,
        ) else {
            panic!()
        };
        assert!(question.options.iter().all(|option| option.enabled));
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::ResumeCompact,
            },
        );
        let view = until(&c, |v| v.waiting.is_some());
        assert_eq!(view.waiting.as_deref(), Some("Waiting for the session to start, then compact. This exact message will be sent automatically after compaction. Cancel send to edit."));
        until(&c, |_| called(&fake, &Action::Resume));
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        c.observe(&state(after.clone()));
        let view = until(&c, |v| v.compacting);
        assert_eq!(view.waiting.as_deref(), Some(WAITING_COMPACT));
        until(&c, |_| called(&fake, &Action::CompactContext));
        let target = call_of(&fake, &Action::CompactContext);
        assert_eq!(
            (target.run_id.as_str(), target.conversation_id.as_str()),
            ("run-2", "conv-1")
        );
        assert!(sends(&fake).is_empty());
        after["compact_context_request"] = json!({"request_id": "cmp-2", "status": "completed"});
        c.observe(&state(after));
        until(&c, |v| !v.sending && !v.compacting && v.waiting.is_none());
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0.run_id, "run-2");
        assert_eq!(sends[0].1.as_deref(), Some("cmp-2"));
    }

    /// zerus-77ljy: "Resume, compact and send" also waits for the listing to
    /// show the ready run before it asks for the compaction of that run.
    #[test]
    fn resume_compact_waits_for_the_listing_to_show_the_ready_run() {
        let root = dir("resume-compact-listing-behind");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        compacts_as(&fake, "cmp-3");
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "compact after the listing".into(),
            0,
            0,
        ) else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::ResumeCompact,
            },
        );
        until(&c, |v| v.waiting.is_some());
        until(&c, |_| called(&fake, &Action::Resume));
        let mut after = ready();
        after["run_id"] = json!("run-2");
        c.observe(&state(after.clone()));
        c.observe(&state(after.clone()));
        std::thread::sleep(Duration::from_millis(200));
        let view = c.current().unwrap();
        assert!(
            !called(&fake, &Action::CompactContext),
            "compacted before the listing showed the new run"
        );
        assert!(!view.compacting && view.waiting.is_some() && view.status != CHANGED);
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        c.observe(&state(after.clone()));
        until(&c, |v| v.compacting);
        until(&c, |_| called(&fake, &Action::CompactContext));
        assert_eq!(call_of(&fake, &Action::CompactContext).run_id, "run-2");
        after["compact_context_request"] = json!({"request_id": "cmp-3", "status": "completed"});
        c.observe(&state(after));
        until(&c, |v| !v.sending && !v.compacting && v.waiting.is_none());
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0.run_id, "run-2");
    }

    /// zerus-le5s: Compact and resume on Codex, which refuses /compact until
    /// the resumed conversation is confirmed, and confirms it only with a new
    /// turn. The waiting message must go, not wait for compaction for ever.
    #[test]
    fn resume_compact_sends_when_the_resumed_conversation_cannot_compact_yet() {
        let root = dir("resume-unconfirmed");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            1,
            view.text_epoch,
            "restart the report".into(),
            0,
            0,
        ) else {
            panic!("no question")
        };
        let choice = StartChoice::ResumeCompact;
        assert_eq!(
            c.answer(token, Answer::Start { choice }),
            SendReply::Sending
        );
        until(&c, |_| called(&fake, &Action::Resume));
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        // `hgs inspect` as the owner's session showed it.
        let unconfirmed = json!({"tracked": true, "run_id": "run-2", "conversation_id": "conv-1",
            "expected_id": "conv-1", "runtime_state": "live", "process_state": "running",
            "phase": "idle", "activity": "idle", "conversation_state": "unknown",
            "resume_message_can_send": true, "resume_message_reason": "",
            "compact_context_supported": false,
            "compact_context_reason": "The agent has not confirmed the resumed conversation yet"});
        c.observe(&state(unconfirmed.clone()));
        c.observe(&state(unconfirmed));
        until(&c, |v| v.waiting.is_none() && !v.sending && !v.compacting);
        let sends = sends(&fake);
        assert_eq!(sends.len(), 1);
        assert_eq!(
            (
                sends[0].0.run_id.as_str(),
                sends[0].0.conversation_id.as_str(),
                sends[0].1.as_deref()
            ),
            ("run-2", "conv-1", None)
        );
        assert!(!called(&fake, &Action::CompactContext));
        assert!(c.current().unwrap().text.is_empty());
    }

    #[test]
    fn an_answer_of_the_other_kind_does_nothing() {
        let root = dir("answer-kind");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, cold());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "which".into(), 0, 0)
        else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Send
                }
            ),
            SendReply::Busy
        );
        lock(&fake.row).state = "running".into();
        c.tick();
        let view = c.current().unwrap();
        let SendReply::Cold { token, prompt } =
            c.send_message(view.handle, 2, view.text_epoch, "which".into(), 0, 0)
        else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Fresh
                }
            ),
            SendReply::Busy
        );
        // Clear context opens the Clear session dialog.
        let view = c.current().unwrap();
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 3, view.text_epoch, "which".into(), 0, 0)
        else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Clear
                }
            ),
            SendReply::ClearContext { token }
        );
        // Compact and continue is refused where Qt's dialog disables it.
        assert!(prompt
            .choices
            .iter()
            .any(|ch| ch.action == ColdAction::Compact && ch.enabled));
        let mut unsupported = cold();
        unsupported["compact_context_supported"] = json!(false);
        c.observe(&state(unsupported));
        let view = c.current().unwrap();
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 4, view.text_epoch, "which".into(), 0, 0)
        else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Busy
        );
        assert!(lock(&fake.calls).is_empty());
        assert!(c.current().unwrap().waiting.is_none());
    }

    #[test]
    fn a_start_answer_after_a_change_keeps_the_draft_with_qts_notice() {
        let root = dir("start-changed");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "one".into(), 0, 0)
        else {
            panic!()
        };
        lock(&fake.row).run_id = Some("run-7".into());
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Resume
                }
            ),
            SendReply::Notice {
                text: CHANGED.into(),
                error: true
            }
        );
        lock(&fake.row).run_id = Some("run-1".into());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "one".into(), 0, 0)
        else {
            panic!()
        };
        c.edit(view.handle, 3, view.text_epoch, "two".into(), 3, 3);
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Resume
                }
            ),
            SendReply::Notice {
                text: DRAFT_CHANGED.into(),
                error: false
            }
        );
        let view = c.current().unwrap();
        assert!(view.waiting.is_none() && view.text == "two");
        // Another session was opened meanwhile: Qt names the change too.
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 4, view.text_epoch, "two".into(), 0, 0)
        else {
            panic!()
        };
        c.open(Some(other_session()));
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Fresh
                }
            ),
            SendReply::Notice {
                text: CHANGED.into(),
                error: true
            }
        );
        assert!(lock(&fake.calls).is_empty());
    }

    #[test]
    fn a_start_whose_waiting_save_answers_after_the_exit_asks_for_nothing() {
        let root = dir("start-exit");
        let store = drafts(&root);
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&store, spawn);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "on quit".into(), 0, 0)
        else {
            panic!()
        };
        let release = hold(&store);
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Resume
                }
            ),
            SendReply::Sending
        );
        c.flush();
        release.send(()).unwrap();
        // The waiting save answered (the store writes in order): no call
        // was handed to a thread.
        store.flush();
        assert!(lock(&jobs).is_empty());
        assert!(lock(&fake.calls).is_empty());
    }

    #[test]
    fn an_uncertain_start_ends_the_wait_and_never_sends() {
        let root = dir("start-uncertain");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        *lock(&fake.outcome) = Box::new(|action, id| match action {
            Action::Resume => Outcome::Uncertain {
                detail: "hgs: lost".into(),
            },
            _ => receipt(action, id),
        });
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "maybe".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        // The start may have run: the draft keeps its identity and stays
        // uncertain until Allow retry (zerus-5wmh.66.4.4).
        let lost = local_messages::uncertain_detail("hgs: lost");
        let view = until(&c, |v| v.waiting.is_none() && v.status == lost);
        assert!(view.status_error && view.uncertain && view.text == "maybe");
        let held = c.lock().composers[KEY].checkpoint.clone().expect("held");
        assert_eq!(held.kind, Kind::ResumeToSend);
        assert!(
            c.lock().orphans.is_empty(),
            "a call that answered is not in flight"
        );
        eventually(|| on_disk(&root).as_ref() == Some(&held));
        assert_eq!(
            c.send_message(view.handle, 2, view.text_epoch, "maybe".into(), 0, 0),
            SendReply::Busy,
            "a new attempt waits for Allow retry"
        );
        c.allow_retry(view.handle);
        eventually(|| on_disk(&root).is_none());
        let view = c.current().unwrap();
        assert!(!view.uncertain);
        // A panicking call ends it too, after its one call.
        let tries = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&tries);
        *lock(&fake.outcome) = Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            panic!("the call's thread died")
        });
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 3, view.text_epoch, "maybe".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        let abnormal = local_messages::uncertain_detail(ABNORMAL);
        let view = until(&c, |v| v.waiting.is_none() && v.status == abnormal);
        assert!(view.status_error && view.uncertain);
        let again = c.lock().composers[KEY].checkpoint.clone().expect("held");
        assert_ne!(
            again.request_id, held.request_id,
            "a new id after Allow retry"
        );
        // A new run that reports ready afterwards sends nothing.
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        // A send would begin within `observe` (its delivery marks the
        // composer sending before it returns).
        c.observe(&state(after));
        assert!(!c.current().unwrap().sending && c.lock().start.is_none());
        assert!(sends(&fake).is_empty());
        assert_eq!(tries.load(Ordering::SeqCst), 1, "one call, not repeated");
    }

    #[test]
    fn an_uncertain_draft_never_waits_for_a_compaction() {
        let root = dir("compact-uncertain");
        let (c, fake) = send_hub(&root);
        opened(&c, ready());
        let id = failed_send(&c, &fake, "same");
        // Another message ends uncertain; the draft is then edited back to
        // the failed one, and stays uncertain.
        *lock(&fake.outcome) = Box::new(|_, _| Outcome::Uncertain {
            detail: "lost".into(),
        });
        let view = c.current().unwrap();
        c.send_message(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "other".into(),
            0,
            0,
        );
        let view = until(&c, |v| !v.sending && v.uncertain);
        *lock(&fake.outcome) = Box::new(receipt);
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "same".into(),
            4,
            4,
        );
        let view = until(&c, |v| v.text == "same" && v.uncertain);
        c.observe(&state(cold()));
        // Its unresolved identity takes no Retry until Allow retry
        // (zerus-5wmh.66.4.4).
        assert_eq!(c.retry(view.handle, &id), SendReply::Busy);
        assert!(!called(&fake, &Action::CompactContext));
        // A draft uncertain from before identities were kept (none held)
        // still never waits for a compaction.
        c.lock().composers.get_mut(KEY).unwrap().checkpoint = None;
        let SendReply::Cold { token, .. } = c.retry(view.handle, &id) else {
            panic!("no cold question")
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Notice {
                text: COMPACT_CANCELLED.into(),
                error: true
            }
        );
        let view = c.current().unwrap();
        assert!(view.uncertain && !view.compacting && view.waiting.is_none());
        assert!(!called(&fake, &Action::CompactContext));
    }

    #[test]
    fn a_retry_compacts_only_its_own_draft_and_its_row_must_stay() {
        let root = dir("compact-retry");
        let (c, fake) = send_hub(&root);
        compacts_as(&fake, "cmp-3");
        let mut details = ready();
        opened(&c, details.clone());
        let id = failed_send(&c, &fake, "again");
        compacts_as(&fake, "cmp-3");
        details["cache_hint"] = json!({"status": "cold"});
        c.observe(&state(details.clone()));
        // The draft is another message: Qt's compaction needs the retried
        // one (`draftMatches`, 4844).
        let view = c.current().unwrap();
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "newer".into(),
            5,
            5,
        );
        let SendReply::Cold { token, .. } = c.retry(view.handle, &id) else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Notice {
                text: DRAFT_CHANGED.into(),
                error: false
            }
        );
        let view = c.current().unwrap();
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "again".into(),
            5,
            5,
        );
        let SendReply::Cold { token, .. } = c.retry(view.handle, &id) else {
            panic!()
        };
        assert_eq!(
            c.answer(
                token,
                Answer::Cold {
                    action: ColdAction::Compact
                }
            ),
            SendReply::Sending
        );
        let view = until(&c, |v| v.compacting);
        until(&c, |_| called(&fake, &Action::CompactContext));
        // The failed row goes before the compaction completes.
        c.delete_local(view.handle, &id);
        details["compact_context_request"] = json!({"request_id": "cmp-3", "status": "completed"});
        c.observe(&state(details));
        let view = until(&c, |v| !v.compacting && v.waiting.is_none());
        assert_eq!(view.status, CHANGED);
        assert_eq!(view.text, "again");
        assert_eq!(sends(&fake).len(), 1, "only the first, failed send");
    }

    #[test]
    fn a_compaction_for_a_session_no_longer_open_is_polled_and_sent_there() {
        let root = dir("compact-away");
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&drafts(&root), spawn);
        compacts_as(&fake, "cmp-4");
        let details = cold();
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "away".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        // The waiting save answers on the store's writer; the compaction's
        // call is the first job.
        let call = next_job(&jobs);
        c.open(Some(other_session()));
        c.observe(&other_state(ready()));
        call();
        assert!(called(&fake, &Action::CompactContext));
        // Accepted: its session is read at once (975), one read at a time.
        let mut done = details;
        done["compact_context_request"] = json!({"request_id": "cmp-4", "status": "completed"});
        *lock(&fake.inspected) = Some(done);
        eventually(|| lock(&jobs).len() == 1);
        for _ in 0..2 {
            *lock(&fake.now) += 3.0;
            c.tick();
        }
        assert_eq!(lock(&jobs).len(), 1, "one read at a time");
        // An offline computer is not read.
        *lock(&fake.online) = false;
        *lock(&fake.inspected) = None;
        next_job(&jobs)();
        *lock(&fake.now) += 3.0;
        c.tick();
        assert!(lock(&jobs).is_empty());
        *lock(&fake.online) = true;
        let mut done = cold();
        done["compact_context_request"] = json!({"request_id": "cmp-4", "status": "completed"});
        *lock(&fake.inspected) = Some(done);
        *lock(&fake.now) += 3.0;
        c.tick();
        assert_eq!(lock(&jobs).len(), 1);
        // The poll's reading sends the waiting draft to its own session.
        run_all(&jobs);
        eventually(|| {
            run_all(&jobs);
            sends(&fake).len() == 1
        });
        let (target, compaction) = sends(&fake).remove(0);
        assert_eq!(target.name, "claude/sample-project/review");
        assert_eq!(compaction.as_deref(), Some("cmp-4"));
        eventually(|| {
            run_all(&jobs);
            c.lock()
                .composers
                .get(KEY)
                .is_some_and(|composer| !composer.sending && composer.draft.text.is_empty())
        });
    }

    /// `canStartStopped`'s busy part: a session that stopped while its own
    /// compaction waits or its message is sent does not offer a start.
    #[test]
    fn a_stopped_session_busy_with_its_own_send_offers_no_start() {
        let root = dir("start-busy");
        let store = drafts(&root);
        let (c, fake) = send_hub_on(&store);
        compacts_as(&fake, "cmp-6");
        let view = opened(&c, cold());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "busy".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        until(&c, |v| v.compacting);
        stopped(&fake);
        c.tick();
        let view = c.current().unwrap();
        assert!(!view.available);
        assert_eq!(view.unavailable_reason, WAITING_COMPACT);
        c.cancel_send(view.handle);
        let view = until(&c, |v| !v.compacting);
        assert!(view.available, "stopped and idle: Send asks again");
        // Its own message under way.
        lock(&fake.row).state = "running".into();
        c.observe(&state(ready()));
        let view = c.current().unwrap();
        let release = hold(&store);
        c.send_message(view.handle, 2, view.text_epoch, "busy".into(), 4, 4);
        until(&c, |v| v.sending);
        stopped(&fake);
        c.tick();
        let view = c.current().unwrap();
        assert!(!view.available);
        assert_eq!(
            view.unavailable_reason,
            "Resume the session to send a message."
        );
        release.send(()).unwrap();
        until(&c, |v| !v.sending);
    }

    /// The draft mark holds each file's name, type, reference and bytes
    /// (`draftMatches`), hashed once per attachment list.
    #[test]
    fn a_start_answer_compares_the_files_by_name_and_bytes() {
        let root = dir("start-files");
        let (a, b) = (root.join("a.txt"), root.join("b.txt"));
        std::fs::write(&a, "same bytes").unwrap();
        std::fs::write(&b, "same bytes").unwrap();
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let at = zerus_client_core::attachments::Selection { start: 0, end: 0 };
        let view = opened(&c, ready());
        c.attach_paths(view.handle, vec![a], at);
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        // The message keeps the file without its mention.
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "files".into(),
            5,
            5,
        );
        let view = until(&c, |v| v.text == "files");
        let SendReply::Start { token, .. } = c.send_message(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "files".into(),
            5,
            5,
        ) else {
            panic!()
        };
        // Another file with the same bytes takes its place, and the text is
        // the same again.
        c.remove(view.handle, 0);
        let view = until(&c, |v| v.attachments.is_empty());
        c.attach_paths(view.handle, vec![b], at);
        let view = until(&c, |v| v.attachments.len() == 1 && !v.attaching);
        c.edit(
            view.handle,
            view.rev + 1,
            view.text_epoch,
            "files".into(),
            5,
            5,
        );
        until(&c, |v| v.text == "files");
        assert_eq!(
            c.answer(
                token,
                Answer::Start {
                    choice: StartChoice::Resume
                }
            ),
            SendReply::Notice {
                text: DRAFT_CHANGED.into(),
                error: false
            }
        );
        assert!(lock(&fake.calls).is_empty());
    }

    /// A taken write lane refuses the call before anything runs: Qt's cancel
    /// texts (4756, 4559) in the error tone, and nothing uncertain.
    #[test]
    fn a_taken_lane_ends_a_continuation_with_qts_cancel_text() {
        let root = dir("lane-taken");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        *lock(&fake.taken) =
            Box::new(|action| matches!(action, Action::Resume | Action::CompactContext));
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "taken".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        let view = until(&c, |v| v.status == START_CANCELLED);
        assert!(view.waiting.is_none() && view.status_error && !view.uncertain);
        assert_eq!(view.text, "taken");
        lock(&fake.row).state = "running".into();
        c.observe(&state(cold()));
        let view = c.current().unwrap();
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "taken".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        let view = until(&c, |v| v.status == COMPACT_CANCELLED);
        assert!(!view.compacting && view.waiting.is_none());
        assert!(view.status_error && !view.uncertain);
        assert!(lock(&fake.calls).is_empty());
    }

    /// Qt asks in the waiting save's callback (4762, 4561): a Cancel send or
    /// Stop between that answer and the call's thread leaves no call.
    #[test]
    fn a_cancel_before_the_call_asks_for_nothing() {
        let root = dir("cancel-before-call");
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&drafts(&root), spawn);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "not yet".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        let call = next_job(&jobs);
        c.cancel_send(view.handle);
        let view = until(&c, |v| v.waiting.is_none());
        call();
        assert!(lock(&fake.calls).is_empty());
        lock(&fake.row).state = "running".into();
        c.observe(&state(cold()));
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 2, view.text_epoch, "not yet".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        let call = next_job(&jobs);
        c.interrupted(view.handle);
        let view = until(&c, |v| !v.compacting);
        assert_eq!(view.status, rules::INTERRUPTED);
        call();
        assert!(lock(&fake.calls).is_empty());
        assert!(lock(&jobs).is_empty());
    }

    #[test]
    fn a_start_for_a_session_no_longer_open_is_polled_and_sent_there() {
        let root = dir("start-away");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "away".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |_| called(&fake, &Action::Resume));
        c.open(Some(other_session()));
        c.observe(&other_state(ready()));
        // The new run reports ready to the poll only.
        {
            let mut row = lock(&fake.row);
            row.state = "running".into();
            row.run_id = Some("run-2".into());
        }
        let mut after = ready();
        after["run_id"] = json!("run-2");
        *lock(&fake.inspected) = Some(after);
        assert!(sends(&fake).is_empty());
        *lock(&fake.now) += 3.0;
        c.tick();
        eventually(|| sends(&fake).len() == 1);
        let (target, compaction) = sends(&fake).remove(0);
        assert_eq!(
            (target.name.as_str(), target.run_id.as_str()),
            ("claude/sample-project/review", "run-2")
        );
        assert_eq!(compaction, None);
        eventually(|| {
            c.lock()
                .composers
                .get(KEY)
                .is_some_and(|composer| !composer.sending && composer.draft.text.is_empty())
        });
        assert!(c.lock().start.is_none());
    }

    /// Runs the held jobs, those they hold included.
    fn run_all(jobs: &Held) {
        loop {
            let taken: Vec<Job> = std::mem::take(&mut *lock(jobs));
            if taken.is_empty() {
                return;
            }
            for job in taken {
                job();
            }
        }
    }

    /// Task 4b's refeed: an outcome that arrives after the session's state
    /// still reads it.
    #[test]
    fn a_compaction_accepted_after_its_completion_showed_still_sends() {
        let root = dir("compact-refeed");
        let (spawn, jobs) = held_spawn();
        let (c, fake) = send_hub_with(&drafts(&root), spawn);
        compacts_as(&fake, "cmp-5");
        let mut details = cold();
        let view = opened(&c, details.clone());
        let SendReply::Cold { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "in order".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Cold {
                action: ColdAction::Compact,
            },
        );
        let call = next_job(&jobs);
        details["compact_context_request"] = json!({"request_id": "cmp-5", "status": "completed"});
        c.observe(&state(details));
        assert!(sends(&fake).is_empty());
        call();
        eventually(|| {
            run_all(&jobs);
            sends(&fake).len() == 1
        });
        assert_eq!(sends(&fake)[0].1.as_deref(), Some("cmp-5"));
    }

    #[test]
    fn the_ticker_keeps_a_waiting_send_alive_while_no_composer_is_open() {
        let root = dir("start-closed");
        let (c, fake) = send_hub(&root);
        stopped(&fake);
        let view = opened(&c, ready());
        let SendReply::Start { token, .. } =
            c.send_message(view.handle, 1, view.text_epoch, "later".into(), 0, 0)
        else {
            panic!()
        };
        c.answer(
            token,
            Answer::Start {
                choice: StartChoice::Resume,
            },
        );
        until(&c, |v| v.waiting.is_some());
        settled(&c, true);
        c.open(None);
        let ticker = c.start_ticker_every(std::time::Duration::from_millis(5));
        *lock(&fake.now) += 181.0;
        eventually(|| c.lock().start.is_none());
        assert_eq!(
            c.lock().composers.get(KEY).map(|composer| composer.notice.clone()),
            Some(
                "The agent did not become ready. Check Terminal; your draft was kept without sending."
                    .into()
            )
        );
        // Nothing waits any more: it sleeps again.
        eventually(|| c.0.parked.load(std::sync::atomic::Ordering::SeqCst));
        c.flush();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            ticker.join().unwrap();
            let _ = done.send(());
        });
        ended
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the ticker ends when the app quits");
    }
}
