//! The send flow's rules, ported from the Qt client: why the composer cannot
//! send to a session now, why a context action is unavailable, and the two
//! continuations that send a waiting draft later: resume-then-send and
//! compact-to-send.
//!
//! Reference: `tray/src/SessionsWindow.cpp` (`messageBlockReason`,
//! `contextActionReason`, `contextReasonSentence`, `canStartStopped`,
//! `startToSend`, `continueAfterStart`, `compactToSend`,
//! `continueAfterCompact`, `pollCompactContinuations`).
use crate::actions::{Action, Outcome};
use crate::text::Text;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The agents the composer sends to; any other is typed into in Terminal.
pub const MESSAGE_AGENTS: [&str; 5] = ["codex", "claude", "kimi", "dsh", "opencode"];

/// What the client knows about a session besides its inspected details.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockFacts {
    /// This session's message waits for it to start (resume-then-send).
    pub start_send_waiting: bool,
    /// This session's message waits for a compaction (compact-to-send).
    pub compacting: bool,
    /// A context reset of this session is in flight.
    pub clearing: bool,
    /// The session is being terminated.
    pub terminating: bool,
    /// A model settings change of this session is in flight.
    pub settings_changing: bool,
    /// Its computer is online.
    pub online: bool,
    /// The row's state, such as `running` or `paused`.
    pub state: String,
    /// A session update or rename is in flight.
    pub updating: bool,
    /// The agent command, such as `claude`.
    pub cmd: String,
    /// The last inspection failed.
    pub inspect_error: bool,
}

fn text<'a>(details: &'a Value, key: &str) -> &'a str {
    details.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(details: &Value, key: &str) -> bool {
    details.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The agent's own verdict: sendable when `can_send`, else its reason (as Qt
/// reads it: a reported string, even empty, replaces `fallback`).
fn agent_gate(details: &Value, can_send: &str, reason: &str, fallback: &str) -> Option<String> {
    if flag(details, can_send) {
        return None;
    }
    let reason = details
        .get(reason)
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    (!reason.is_empty()).then(|| reason.to_owned())
}

/// Why the composer cannot send to this session now, `None` when it can.
/// `details` is the session's inspection, `None` until one arrived. The rules
/// apply in Qt's order; the first that holds gives the reason.
pub fn block_reason(facts: &BlockFacts, details: Option<&Value>) -> Option<String> {
    let block = |reason: &str| Some(reason.to_owned());
    if facts.start_send_waiting {
        return block("Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit.");
    }
    if facts.compacting {
        return block("Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit.");
    }
    if facts.clearing {
        return block("Wait for the context reset to finish. Your draft stays here.");
    }
    if facts.terminating {
        return block("The session is terminating. Your draft stays here.");
    }
    if facts.settings_changing {
        return block("Wait for the model settings change to finish.");
    }
    if !facts.online {
        return block("Machine unavailable. Your draft stays here.");
    }
    if facts.state != "running" {
        return block("Resume the session to send a message.");
    }
    if facts.updating {
        return block("Wait for the session update to finish.");
    }
    if !MESSAGE_AGENTS.contains(&facts.cmd.as_str()) {
        return block("Use the Terminal tab to type into this session.");
    }
    if facts.inspect_error {
        return block("Refresh the session before sending.");
    }
    let details = match details {
        Some(details) if details.as_object().is_some_and(|object| !object.is_empty()) => details,
        _ => return block("Checking the session…"),
    };
    if !flag(details, "tracked") {
        return block("This session has no HGS tracking. Use Terminal to send a message.");
    }
    if text(details, "run_id").is_empty() {
        return block(
            "Waiting for the agent to finish starting. Open Terminal to check login or setup.",
        );
    }
    if text(details, "runtime_state") != "live" || text(details, "process_state") != "running" {
        return block("The agent is not running. Resume it before sending.");
    }
    let opencode = facts.cmd == "opencode";
    if facts.cmd == "dsh" && flag(details, "auth_required") {
        return block("Sign in to DeepSeek above. Your draft stays here.");
    }
    if !text(details, "error").is_empty() {
        return block(if opencode {
            "Refresh the native session and check OpenCode configuration on this computer before sending."
        } else {
            "Check the agent in the Terminal tab before sending."
        });
    }
    if !text(details, "expected_id").is_empty() {
        return agent_gate(
            details,
            "resume_message_can_send",
            "resume_message_reason",
            "Waiting for the restored conversation. Open Terminal to check startup.",
        );
    }
    // Codex reports SessionStart only with its first turn: the CLI can verify a
    // fresh run's native prompt before a conversation ID exists.
    if text(details, "conversation_id").is_empty() {
        return agent_gate(
            details,
            "first_message_can_send",
            "first_message_reason",
            "Finish login or setup in Terminal. Sending becomes available when the agent is ready.",
        );
    }
    let phase = text(details, "phase");
    if phase == "approval" || phase == "input" {
        return block(if opencode {
            "Answer the pending native request in Activity before sending."
        } else {
            "Respond to the agent in the Terminal tab."
        });
    }
    if phase == "interrupted" {
        return agent_gate(
            details,
            "interrupted_message_can_send",
            "interrupted_message_reason",
            "Waiting for the agent's input prompt after interruption. Open Terminal to check.",
        );
    }
    if phase == "error" && details.get("provider_error").is_some_and(Value::is_object) {
        return agent_gate(
            details,
            "error_message_can_send",
            "error_message_reason",
            "Check the agent in Terminal before retrying.",
        );
    }
    let activity = text(details, "activity");
    let ready = activity == "idle" && phase == "idle";
    let working = activity == "busy" && ["idle", "working", "tool", "compacting"].contains(&phase);
    if !ready && !working {
        return block("Waiting for the agent to confirm its input state.");
    }
    None
}

/// Why a context action (`"compact"` or `"clear"`) is unavailable, as the
/// host reports it (such as a subagent or tool still working); empty when it
/// is supported or no reason was given. Whitespace is collapsed.
pub fn context_action_reason(details: &Value, action: &str) -> String {
    if flag(details, &format!("{action}_context_supported")) {
        return String::new();
    }
    text(details, &format!("{action}_context_reason"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A reason as a sentence: a final full stop unless it already ends with
/// `.`, `!`, `?` or `…`.
pub fn reason_sentence(reason: &str) -> String {
    if reason.is_empty() || reason.ends_with(['.', '!', '?', '…']) {
        reason.to_owned()
    } else {
        format!("{reason}.")
    }
}

/// The draft a continuation sends: its text and attachment digests. The
/// composer's draft must still match it at every step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMark {
    pub text: String,
    pub attachments: Vec<String>,
}

/// A session as the catalog shows it now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub key: String,
    pub host: String,
    pub name: String,
    pub created: i64,
    pub run_id: String,
    pub conversation_id: String,
    pub state: String,
    pub online: bool,
    pub terminating: bool,
    pub tracked: bool,
    pub cmd: String,
    pub resumable: bool,
}

/// The texts of a pending send's ends that the user or the session caused.
pub const CANCELLED_BY_USER: &str = "Pending send cancelled. Your draft is unchanged.";
pub const INTERRUPTED: &str = "Interrupted. Your draft was kept without sending.";
const CHANGED: &str = "The session changed. Your draft was kept without sending.";
const DRAFT_CHANGED: &str = "Draft changed. Send it when you are ready.";
const NOT_READY: &str =
    "The agent did not become ready. Check Terminal; your draft was kept without sending.";
/// How long a start may take before its message is given up.
pub const START_LIMIT: f64 = 180.0;
/// How long a compaction may take before its message is given up.
pub const COMPACT_LIMIT: f64 = 600.0;

/// Whether a stopped session can start to take a message (`canStartStopped`).
/// `busy`: a session update, rename or settings change runs, another
/// start-to-send waits anywhere, or this session compacts or sends.
pub fn can_start_stopped(session: &Session, busy: bool) -> bool {
    session.online
        && matches!(session.state.as_str(), "paused" | "stopped")
        && session.tracked
        && matches!(session.cmd.as_str(), "codex" | "claude" | "kimi")
        && !session.terminating
        && !busy
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartChoice {
    Resume,
    ResumeCompact,
    Fresh,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOption {
    pub choice: StartChoice,
    pub label: Text,
    pub enabled: bool,
    pub tooltip: Option<Text>,
}

/// "Start the session to send?": Cancel is the default and the escape answer.
/// The title already says the message is sent, so each choice names only how
/// the session starts, and each paragraph opens with the choice it explains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartQuestion {
    pub title: Text,
    pub text: Text,
    pub informative: Text,
    pub options: Vec<StartOption>,
}

/// The question before a stopped session takes a message. `context` is the
/// composer's context usage text, empty when unknown.
pub fn start_question(session: &Session, context: &str) -> StartQuestion {
    let resume = if context.is_empty() {
        crate::text!("Resume continues the saved conversation, and the message is sent with its full context.")
    } else {
        crate::text!(
            "Resume continues the saved conversation, and the message is sent with its full context (%1).",
            context
        )
    };
    let option = |choice, label, enabled, tooltip| StartOption {
        choice,
        label,
        enabled,
        tooltip,
    };
    StartQuestion {
        title: crate::text!("Start the session to send?"),
        text: crate::text!(
            "%1 is stopped. How should it start before your message is sent?",
            session.name.as_str()
        ),
        // The paragraphs follow the buttons' order.
        informative: crate::text!(
            "%1\n\n%2\n\n%3",
            resume,
            crate::text!("Compact and resume summarizes the saved history first, then sends the message. Compaction uses tokens and may omit details."),
            crate::text!("Start fresh begins a new conversation with the same folder, account and model. Earlier messages stay readable in Activity."),
        ),
        options: vec![
            option(
                StartChoice::Resume,
                crate::text!("Resume"),
                session.resumable,
                None,
            ),
            option(
                StartChoice::ResumeCompact,
                crate::text!("Compact and resume"),
                session.resumable,
                Some(crate::text!("Resume the saved conversation, run native /compact when ready, then send this exact message after successful completion.")),
            ),
            option(
                StartChoice::Fresh,
                crate::text!("Start fresh"),
                true,
                None,
            ),
        ],
    }
}

/// What a continuation asks of its caller.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Nothing yet.
    Idle,
    /// Run this action for the session; report its outcome back.
    Run(Action),
    /// Send the waiting draft now with full-context consent, expecting this
    /// run and conversation (and compaction).
    Send {
        run_id: String,
        conversation_id: String,
        compaction_id: Option<String>,
    },
    /// Compact before sending: keep this continuation and show its text.
    Compact(CompactSend, String),
    /// Ended without sending; the draft stays. Show this.
    Cancelled(String),
}

fn evidence_differs(evidence: Option<&Value>, run: &str, conversation: &str) -> bool {
    evidence.is_some_and(|evidence| {
        evidence
            .as_object()
            .is_some_and(|object| !object.is_empty())
            && (text(evidence, "run_id") != run
                || text(evidence, "conversation_id") != conversation)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartPhase {
    Preparing,
    Starting,
    Waiting,
}

/// Resume-then-send: one at a time across all sessions, as in Qt.
#[derive(Clone, Debug, PartialEq)]
pub struct StartSend {
    session: Session,
    draft: DraftMark,
    choice: StartChoice,
    started: f64,
    phase: StartPhase,
}

impl StartSend {
    /// The answer to the start question. `current` is the session again and
    /// `draft_now` the composer's draft; the waiting text comes back with the
    /// continuation, or a notice when nothing starts.
    pub fn begin(
        original: &Session,
        current: Option<&Session>,
        busy: bool,
        choice: StartChoice,
        draft: DraftMark,
        draft_now: &DraftMark,
        now: f64,
    ) -> Result<(StartSend, String), String> {
        let unchanged = current.is_some_and(|current| {
            current.key == original.key
                && current.run_id == original.run_id
                && current.conversation_id == original.conversation_id
                && can_start_stopped(current, busy)
        });
        if !unchanged {
            return Err(CHANGED.into());
        }
        if *draft_now != draft {
            return Err(DRAFT_CHANGED.into());
        }
        let waiting = if choice == StartChoice::ResumeCompact {
            "Waiting for the session to start, then compact. This exact message will be sent automatically after compaction. Cancel send to edit."
        } else {
            "Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit."
        };
        Ok((
            StartSend {
                session: original.clone(),
                draft,
                choice,
                started: now,
                phase: StartPhase::Preparing,
            },
            waiting.into(),
        ))
    }

    pub fn key(&self) -> &str {
        &self.session.key
    }

    /// The composer holds the draft as waiting (`ok`): start the session if
    /// it is still the one asked about. `busy`: a session update or rename
    /// runs.
    pub fn prepared(
        &mut self,
        ok: bool,
        current: Option<&Session>,
        evidence: Option<&Value>,
        busy: bool,
    ) -> Step {
        if self.phase != StartPhase::Preparing {
            return Step::Idle;
        }
        let original = &self.session;
        let same = current.is_some_and(|current| {
            current.host == original.host
                && current.name == original.name
                && current.created == original.created
                && current.run_id == original.run_id
                && current.conversation_id == original.conversation_id
                && current.online
                && !current.terminating
                && current.state != "running"
        });
        if !ok
            || !same
            || busy
            || evidence_differs(evidence, &original.run_id, &original.conversation_id)
        {
            return Step::Cancelled(
                "Starting was cancelled. Your draft was kept without sending.".into(),
            );
        }
        self.phase = StartPhase::Starting;
        Step::Run(if self.choice == StartChoice::Fresh {
            Action::StartFresh
        } else {
            Action::Resume
        })
    }

    /// The start's outcome: a failure ends the continuation with its detail.
    pub fn start_finished(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::Done { .. } => {
                self.phase = StartPhase::Waiting;
                Step::Idle
            }
            Outcome::Failed { detail } | Outcome::Uncertain { detail } => {
                Step::Cancelled(detail.clone())
            }
        }
    }

    /// Every inspection of the session while it starts (`continueAfterStart`).
    /// `pending`: the start itself still runs.
    pub fn inspected(
        &mut self,
        details: &Value,
        draft_now: &DraftMark,
        pending: bool,
        terminating: bool,
        now: f64,
    ) -> Step {
        if self.phase == StartPhase::Preparing {
            return Step::Idle;
        }
        if *draft_now != self.draft {
            return Step::Cancelled(DRAFT_CHANGED.into());
        }
        if now - self.started > START_LIMIT {
            return Step::Cancelled(NOT_READY.into());
        }
        let run = text(details, "run_id");
        let conversation = text(details, "conversation_id");
        if pending || run.is_empty() || run == self.session.run_id {
            return Step::Idle;
        }
        let same_conversation = conversation == self.session.conversation_id;
        let unexpected = if self.choice == StartChoice::Fresh {
            same_conversation
        } else {
            !conversation.is_empty() && !same_conversation
        };
        if unexpected {
            return Step::Cancelled(
                "The agent opened an unexpected conversation. Your draft was kept without sending."
                    .into(),
            );
        }
        let ready = flag(details, "tracked")
            && text(details, "runtime_state") == "live"
            && text(details, "process_state") == "running"
            && text(details, "error").is_empty()
            && text(details, "phase") == "idle"
            && text(details, "activity") == "idle"
            && !terminating
            && (text(details, "expected_id").is_empty()
                || flag(details, "resume_message_can_send"))
            && (!conversation.is_empty() || flag(details, "first_message_can_send"));
        if !ready {
            return Step::Idle;
        }
        if self.choice == StartChoice::ResumeCompact {
            if conversation.is_empty() || !flag(details, "compact_context_supported") {
                return Step::Idle;
            }
            let (compact, waiting) = CompactSend::begin(
                &self.session,
                run,
                conversation,
                self.draft.clone(),
                None,
                now,
            );
            return Step::Compact(compact, waiting);
        }
        Step::Send {
            run_id: run.into(),
            conversation_id: conversation.into(),
            compaction_id: None,
        }
    }

    /// The 2.5 s poll: past 180 s the message is given up.
    pub fn tick(&self, now: f64) -> Step {
        if now - self.started > START_LIMIT {
            Step::Cancelled(NOT_READY.into())
        } else {
            Step::Idle
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CompactPhase {
    Preparing,
    Requesting,
    Waiting(String),
    Sent,
}

/// Compact-to-send for one session: native /compact, then this exact draft
/// after the compaction completes. The send names the compaction, which the
/// CLI verifies too.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactSend {
    key: String,
    host: String,
    name: String,
    run_id: String,
    conversation_id: String,
    draft: DraftMark,
    /// The failed local message this send retries, if any.
    pub retry_id: Option<String>,
    started: f64,
    phase: CompactPhase,
}

impl CompactSend {
    /// Starts waiting; the text is the composer's waiting notice.
    pub fn begin(
        session: &Session,
        run_id: &str,
        conversation_id: &str,
        draft: DraftMark,
        retry_id: Option<String>,
        now: f64,
    ) -> (CompactSend, String) {
        (
            CompactSend {
                key: session.key.clone(),
                host: session.host.clone(),
                name: session.name.clone(),
                run_id: run_id.into(),
                conversation_id: conversation_id.into(),
                draft,
                retry_id,
                started: now,
                phase: CompactPhase::Preparing,
            },
            "Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit.".into(),
        )
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The composer holds the draft as waiting: ask for the compaction if the
    /// session is still the same run and conversation. `details` is the
    /// selected session's inspection when this is the selected session.
    pub fn prepared(
        &mut self,
        ok: bool,
        current: Option<&Session>,
        evidence: Option<&Value>,
        details: Option<&Value>,
        busy: bool,
    ) -> Step {
        if self.phase != CompactPhase::Preparing {
            return Step::Idle;
        }
        let same = current.is_some_and(|current| {
            current.host == self.host
                && current.name == self.name
                && current.online
                && !current.terminating
        });
        let moved = details.is_some_and(|details| {
            text(details, "run_id") != self.run_id
                || text(details, "conversation_id") != self.conversation_id
        });
        if !ok
            || !same
            || busy
            || moved
            || evidence_differs(evidence, &self.run_id, &self.conversation_id)
        {
            return Step::Cancelled(
                "Compaction was cancelled. Your draft was kept without sending.".into(),
            );
        }
        self.phase = CompactPhase::Requesting;
        Step::Run(Action::CompactContext)
    }

    /// The compaction request's outcome: its receipt names the compaction.
    pub fn compact_finished(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::Done { receipt, .. } => {
                self.phase = CompactPhase::Waiting(text(receipt, "request_id").to_owned());
                Step::Idle
            }
            Outcome::Failed { detail } | Outcome::Uncertain { detail } => {
                Step::Cancelled(detail.clone())
            }
        }
    }

    /// Every inspection while it compacts (`continueAfterCompact`). The send
    /// is returned once; later inspections give nothing.
    pub fn inspected(
        &mut self,
        details: &Value,
        draft_now: &DraftMark,
        terminating: bool,
        now: f64,
    ) -> Step {
        if matches!(self.phase, CompactPhase::Preparing | CompactPhase::Sent) {
            return Step::Idle;
        }
        if text(details, "run_id") != self.run_id
            || text(details, "conversation_id") != self.conversation_id
        {
            return Step::Cancelled(
                "Conversation changed. Your draft was kept without sending.".into(),
            );
        }
        if now - self.started > COMPACT_LIMIT {
            return Step::Cancelled(
                "Compaction was not confirmed. Check Terminal; your draft was kept without sending.".into(),
            );
        }
        if *draft_now != self.draft {
            return Step::Cancelled(DRAFT_CHANGED.into());
        }
        let CompactPhase::Waiting(command) = &self.phase else {
            return Step::Idle;
        };
        if command.is_empty() {
            return Step::Idle;
        }
        let result = &details["compact_context_request"];
        if text(result, "request_id") != command {
            return Step::Idle;
        }
        match text(result, "status") {
            "unchanged" => {
                return Step::Cancelled(
                    "No history could be compacted. Your draft was kept without sending.".into(),
                )
            }
            "failed" | "cancelled" | "uncertain" => return Step::Cancelled(
                "Compaction did not complete. Check Terminal; your draft was kept without sending."
                    .into(),
            ),
            _ => {}
        }
        let settled = text(result, "status") == "completed"
            && text(details, "phase") == "idle"
            && text(details, "activity") == "idle"
            && text(details, "runtime_state") == "live"
            && text(details, "process_state") == "running"
            && flag(details, "tracked")
            && text(details, "error").is_empty()
            && text(details, "expected_id").is_empty()
            && !terminating;
        if !settled {
            return Step::Idle;
        }
        let compaction = command.clone();
        // The intent goes before the send: a repeated inspection never sends
        // twice.
        self.phase = CompactPhase::Sent;
        Step::Send {
            run_id: self.run_id.clone(),
            conversation_id: self.conversation_id.clone(),
            compaction_id: Some(compaction),
        }
    }

    /// The 2.5 s poll: past 600 s the message is given up.
    pub fn tick(&self, now: f64) -> Step {
        if now - self.started > COMPACT_LIMIT {
            Step::Cancelled("Compaction was not confirmed. Check Terminal; your draft was kept without sending.".into())
        } else {
            Step::Idle
        }
    }

    /// Whether the compaction is accepted and the session should be polled
    /// for its result.
    pub fn polling(&self) -> bool {
        matches!(&self.phase, CompactPhase::Waiting(command) if !command.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready() -> (BlockFacts, Value) {
        (
            BlockFacts {
                online: true,
                state: "running".into(),
                cmd: "claude".into(),
                ..BlockFacts::default()
            },
            json!({"tracked": true, "run_id": "run-1", "conversation_id": "conv-1",
                   "runtime_state": "live", "process_state": "running",
                   "phase": "idle", "activity": "idle"}),
        )
    }

    /// Clears the rule that answered.
    type Clear = dyn Fn(&mut BlockFacts, &mut Option<Value>);

    /// Every rule holds at once; each step clears the rule that answered, so
    /// the next reason proves the order.
    #[test]
    fn reasons_follow_qt_order() {
        let mut facts = BlockFacts {
            start_send_waiting: true,
            compacting: true,
            clearing: true,
            terminating: true,
            settings_changing: true,
            online: false,
            state: "paused".into(),
            updating: true,
            cmd: "aider".into(),
            inspect_error: true,
        };
        let mut details: Option<Value> = None;
        let mut steps: Vec<(&str, Box<Clear>)> = vec![
            ("Waiting for the session to start. This exact message will be sent automatically when ready. Cancel send to edit.",
             Box::new(|f, _| f.start_send_waiting = false)),
            ("Waiting for compaction. This exact message will be sent automatically after it completes. Cancel send to edit.",
             Box::new(|f, _| f.compacting = false)),
            ("Wait for the context reset to finish. Your draft stays here.", Box::new(|f, _| f.clearing = false)),
            ("The session is terminating. Your draft stays here.", Box::new(|f, _| f.terminating = false)),
            ("Wait for the model settings change to finish.", Box::new(|f, _| f.settings_changing = false)),
            ("Machine unavailable. Your draft stays here.", Box::new(|f, _| f.online = true)),
            ("Resume the session to send a message.", Box::new(|f, _| f.state = "running".into())),
            ("Wait for the session update to finish.", Box::new(|f, _| f.updating = false)),
            ("Use the Terminal tab to type into this session.", Box::new(|f, _| f.cmd = "dsh".into())),
            ("Refresh the session before sending.", Box::new(|f, _| f.inspect_error = false)),
            ("Checking the session…", Box::new(|_, d| *d = Some(json!({"tracked": false, "auth_required": true,
                "error": "boom", "expected_id": "x", "phase": "approval", "activity": "busy"})))),
            ("This session has no HGS tracking. Use Terminal to send a message.",
             Box::new(|_, d| d.as_mut().unwrap()["tracked"] = json!(true))),
            ("Waiting for the agent to finish starting. Open Terminal to check login or setup.",
             Box::new(|_, d| d.as_mut().unwrap()["run_id"] = json!("run-1"))),
            ("The agent is not running. Resume it before sending.", Box::new(|_, d| {
                let d = d.as_mut().unwrap();
                d["runtime_state"] = json!("live");
                d["process_state"] = json!("running");
            })),
            ("Sign in to DeepSeek above. Your draft stays here.",
             Box::new(|_, d| d.as_mut().unwrap()["auth_required"] = json!(false))),
            ("Check the agent in the Terminal tab before sending.",
             Box::new(|_, d| d.as_mut().unwrap()["error"] = json!(""))),
            ("Waiting for the restored conversation. Open Terminal to check startup.",
             Box::new(|_, d| d.as_mut().unwrap()["expected_id"] = json!(""))),
            ("Finish login or setup in Terminal. Sending becomes available when the agent is ready.",
             Box::new(|_, d| d.as_mut().unwrap()["conversation_id"] = json!("conv-1"))),
            ("Respond to the agent in the Terminal tab.",
             Box::new(|_, d| d.as_mut().unwrap()["phase"] = json!("interrupted"))),
            ("Waiting for the agent's input prompt after interruption. Open Terminal to check.", Box::new(|_, d| {
                let d = d.as_mut().unwrap();
                d["phase"] = json!("error");
                d["provider_error"] = json!({"code": 429});
            })),
            ("Check the agent in Terminal before retrying.", Box::new(|_, d| {
                let d = d.as_mut().unwrap();
                d["phase"] = json!("waiting");
                d["activity"] = json!("idle");
            })),
            ("Waiting for the agent to confirm its input state.",
             Box::new(|_, d| d.as_mut().unwrap()["phase"] = json!("idle"))),
        ];
        for (index, (reason, clear)) in steps.iter_mut().enumerate() {
            assert_eq!(
                block_reason(&facts, details.as_ref()).as_deref(),
                Some(*reason),
                "step {index}"
            );
            clear(&mut facts, &mut details);
        }
        assert_eq!(block_reason(&facts, details.as_ref()), None);
    }

    #[test]
    fn agents_report_their_own_verdicts() {
        let (facts, base) = ready();
        assert_eq!(block_reason(&facts, Some(&base)), None);
        let mut details = base.clone();
        details["expected_id"] = json!("restored");
        details["resume_message_reason"] = json!("Restoring the transcript.");
        assert_eq!(
            block_reason(&facts, Some(&details)).as_deref(),
            Some("Restoring the transcript.")
        );
        details["resume_message_can_send"] = json!(true);
        assert_eq!(block_reason(&facts, Some(&details)), None);
        // As in Qt, a reported empty reason replaces the fallback and blocks nothing.
        let mut details = base.clone();
        details["conversation_id"] = json!("");
        details["first_message_reason"] = json!("");
        assert_eq!(block_reason(&facts, Some(&details)), None);
        details["first_message_reason"] = json!(7);
        assert_eq!(
            block_reason(&facts, Some(&details)).as_deref(),
            Some("Finish login or setup in Terminal. Sending becomes available when the agent is ready.")
        );
        // An empty object is no inspection yet.
        assert_eq!(
            block_reason(&facts, Some(&json!({}))).as_deref(),
            Some("Checking the session…")
        );
    }

    #[test]
    fn opencode_and_busy_agents() {
        let (mut facts, base) = ready();
        facts.cmd = "opencode".into();
        let mut details = base.clone();
        details["error"] = json!("provider unreachable");
        assert_eq!(
            block_reason(&facts, Some(&details)).as_deref(),
            Some("Refresh the native session and check OpenCode configuration on this computer before sending.")
        );
        let mut details = base.clone();
        details["phase"] = json!("input");
        assert_eq!(
            block_reason(&facts, Some(&details)).as_deref(),
            Some("Answer the pending native request in Activity before sending.")
        );
        // A busy agent takes messages while working, using a tool or compacting.
        for phase in ["idle", "working", "tool", "compacting"] {
            let mut details = base.clone();
            details["activity"] = json!("busy");
            details["phase"] = json!(phase);
            assert_eq!(block_reason(&facts, Some(&details)), None, "{phase}");
        }
        let mut details = base.clone();
        details["activity"] = json!("busy");
        details["phase"] = json!("starting");
        assert!(block_reason(&facts, Some(&details)).is_some());
        // An error phase without a provider error is not the agent's verdict.
        let mut details = base;
        details["phase"] = json!("error");
        assert_eq!(
            block_reason(&facts, Some(&details)).as_deref(),
            Some("Waiting for the agent to confirm its input state.")
        );
    }

    #[test]
    fn context_action_reasons_are_collapsed_sentences() {
        let details = json!({"compact_context_supported": false,
                             "compact_context_reason": "  A subagent is\n still working ",
                             "clear_context_supported": true, "clear_context_reason": "ignored"});
        assert_eq!(
            context_action_reason(&details, "compact"),
            "A subagent is still working"
        );
        assert_eq!(context_action_reason(&details, "clear"), "");
        assert_eq!(context_action_reason(&json!({}), "clear"), "");
        assert_eq!(
            reason_sentence("A subagent is still working"),
            "A subagent is still working."
        );
        assert_eq!(reason_sentence("Done!"), "Done!");
        assert_eq!(reason_sentence("Wait…"), "Wait…");
        assert_eq!(reason_sentence(""), "");
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use serde_json::json;

    fn stopped() -> Session {
        Session {
            key: "studio\nclaude/sample-project/review".into(),
            host: "studio".into(),
            name: "claude/sample-project/review".into(),
            created: 1_700,
            run_id: "run-1".into(),
            conversation_id: "conv-1".into(),
            state: "stopped".into(),
            online: true,
            terminating: false,
            tracked: true,
            cmd: "claude".into(),
            resumable: true,
        }
    }

    fn draft() -> DraftMark {
        DraftMark {
            text: "hello".into(),
            attachments: vec![],
        }
    }

    fn ready(run: &str, conversation: &str) -> Value {
        json!({"run_id": run, "conversation_id": conversation, "tracked": true, "runtime_state": "live",
               "process_state": "running", "phase": "idle", "activity": "idle", "compact_context_supported": true})
    }

    fn begun(choice: StartChoice) -> StartSend {
        let session = stopped();
        let (mut start, _) = StartSend::begin(
            &session,
            Some(&session),
            false,
            choice,
            draft(),
            &draft(),
            0.0,
        )
        .unwrap();
        start.prepared(true, Some(&session), None, false);
        start.start_finished(&Outcome::Done {
            receipt: Value::Null,
            cleanup: None,
        });
        start
    }

    #[test]
    fn a_stopped_session_is_asked_how_to_start() {
        let session = stopped();
        assert!(can_start_stopped(&session, false));
        assert!(!can_start_stopped(&session, true));
        assert!(!can_start_stopped(
            &Session {
                cmd: "opencode".into(),
                ..session.clone()
            },
            false
        ));
        assert!(!can_start_stopped(
            &Session {
                state: "running".into(),
                ..session.clone()
            },
            false
        ));
        let question = start_question(
            &Session {
                resumable: false,
                ..session.clone()
            },
            "41% of 200K",
        );
        assert_eq!(question.title.english(), "Start the session to send?");
        assert_eq!(
            question.text.english(),
            "claude/sample-project/review is stopped. How should it start before your message is sent?"
        );
        // The session name stays verbatim, never a catalog lookup.
        assert_eq!(
            question.text.args,
            [crate::text::Arg::Verbatim(
                "claude/sample-project/review".into()
            )]
        );
        // Each paragraph opens with the button it explains, in the buttons' order.
        assert_eq!(
            question.informative.english(),
            "Resume continues the saved conversation, and the message is sent with its full context (41% of 200K).\n\n\
             Compact and resume summarizes the saved history first, then sends the message. Compaction uses tokens and may omit details.\n\n\
             Start fresh begins a new conversation with the same folder, account and model. Earlier messages stay readable in Activity."
        );
        let options: Vec<_> = question
            .options
            .iter()
            .map(|o| (o.choice, o.label.english(), o.enabled, o.tooltip.is_some()))
            .collect();
        assert_eq!(
            options,
            [
                (StartChoice::Resume, "Resume".to_owned(), false, false),
                (
                    StartChoice::ResumeCompact,
                    "Compact and resume".to_owned(),
                    false,
                    true
                ),
                (StartChoice::Fresh, "Start fresh".to_owned(), true, false)
            ]
        );
        assert!(start_question(&session, "")
            .informative
            .english()
            .starts_with(
                "Resume continues the saved conversation, and the message is sent with its full context.\n\n"
            ));
    }

    #[test]
    fn a_resumed_session_takes_the_message_once_it_is_ready() {
        let session = stopped();
        let (mut start, waiting) = StartSend::begin(
            &session,
            Some(&session),
            false,
            StartChoice::Resume,
            draft(),
            &draft(),
            0.0,
        )
        .unwrap();
        assert!(waiting.starts_with("Waiting for the session to start."));
        assert_eq!(
            start.inspected(&ready("run-2", "conv-1"), &draft(), false, false, 1.0),
            Step::Idle,
            "preparing"
        );
        assert_eq!(
            start.prepared(true, Some(&session), None, false),
            Step::Run(Action::Resume)
        );
        assert_eq!(
            start.prepared(true, Some(&session), None, false),
            Step::Idle,
            "once"
        );
        start.start_finished(&Outcome::Done {
            receipt: Value::Null,
            cleanup: None,
        });
        assert_eq!(
            start.inspected(&ready("run-2", "conv-1"), &draft(), true, false, 2.0),
            Step::Idle,
            "start pending"
        );
        assert_eq!(
            start.inspected(&ready("run-1", "conv-1"), &draft(), false, false, 2.0),
            Step::Idle,
            "old run"
        );
        let mut busy = ready("run-2", "conv-1");
        busy["activity"] = json!("busy");
        assert_eq!(
            start.inspected(&busy, &draft(), false, false, 3.0),
            Step::Idle
        );
        assert_eq!(
            start.inspected(&ready("run-2", "conv-1"), &draft(), false, false, 4.0),
            Step::Send {
                run_id: "run-2".into(),
                conversation_id: "conv-1".into(),
                compaction_id: None
            }
        );
        // A resume that opens another conversation is refused.
        let mut other = begun(StartChoice::Resume);
        assert_eq!(
            other.inspected(&ready("run-2", "conv-9"), &draft(), false, false, 4.0),
            Step::Cancelled(
                "The agent opened an unexpected conversation. Your draft was kept without sending."
                    .into()
            )
        );
    }

    #[test]
    fn a_fresh_start_must_open_a_new_conversation() {
        let mut fresh = begun(StartChoice::Fresh);
        assert_eq!(
            fresh.inspected(&ready("run-2", "conv-1"), &draft(), false, false, 1.0),
            Step::Cancelled(
                "The agent opened an unexpected conversation. Your draft was kept without sending."
                    .into()
            )
        );
        let session = stopped();
        let (mut fresh, _) = StartSend::begin(
            &session,
            Some(&session),
            false,
            StartChoice::Fresh,
            draft(),
            &draft(),
            0.0,
        )
        .unwrap();
        assert_eq!(
            fresh.prepared(true, Some(&session), None, false),
            Step::Run(Action::StartFresh)
        );
        fresh.start_finished(&Outcome::Done {
            receipt: Value::Null,
            cleanup: None,
        });
        // Before its first message a fresh conversation has no ID; the agent
        // must say it takes one.
        let mut first = ready("run-2", "");
        assert_eq!(
            fresh.inspected(&first, &draft(), false, false, 1.0),
            Step::Idle
        );
        first["first_message_can_send"] = json!(true);
        assert_eq!(
            fresh.inspected(&first, &draft(), false, false, 1.0),
            Step::Send {
                run_id: "run-2".into(),
                conversation_id: String::new(),
                compaction_id: None
            }
        );
    }

    #[test]
    fn starts_are_refused_or_given_up_with_qt_texts() {
        let session = stopped();
        let renamed = Session {
            run_id: "run-0".into(),
            ..session.clone()
        };
        assert_eq!(
            StartSend::begin(
                &session,
                Some(&renamed),
                false,
                StartChoice::Resume,
                draft(),
                &draft(),
                0.0
            )
            .unwrap_err(),
            "The session changed. Your draft was kept without sending."
        );
        let edited = DraftMark {
            text: "hello!".into(),
            attachments: vec![],
        };
        assert_eq!(
            StartSend::begin(
                &session,
                Some(&session),
                false,
                StartChoice::Resume,
                draft(),
                &edited,
                0.0
            )
            .unwrap_err(),
            "Draft changed. Send it when you are ready."
        );
        let (mut start, _) = StartSend::begin(
            &session,
            Some(&session),
            false,
            StartChoice::Resume,
            draft(),
            &draft(),
            0.0,
        )
        .unwrap();
        let running = Session {
            state: "running".into(),
            ..session.clone()
        };
        assert_eq!(
            start.prepared(true, Some(&running), None, false),
            Step::Cancelled("Starting was cancelled. Your draft was kept without sending.".into())
        );
        let mut failed = begun(StartChoice::Resume);
        assert_eq!(
            failed.start_finished(&Outcome::Failed {
                detail: "resume failed".into()
            }),
            Step::Cancelled("resume failed".into())
        );
        let mut slow = begun(StartChoice::Resume);
        assert_eq!(slow.tick(180.0), Step::Idle);
        assert_eq!(slow.tick(180.5), Step::Cancelled(NOT_READY.into()));
        assert_eq!(
            slow.inspected(&ready("run-2", "conv-1"), &edited, false, false, 5.0),
            Step::Cancelled(DRAFT_CHANGED.into())
        );
    }

    #[test]
    fn compact_to_send_waits_for_its_compaction_and_sends_once() {
        let mut start = begun(StartChoice::ResumeCompact);
        let mut unsupported = ready("run-2", "conv-1");
        unsupported["compact_context_supported"] = json!(false);
        assert_eq!(
            start.inspected(&unsupported, &draft(), false, false, 1.0),
            Step::Idle
        );
        let Step::Compact(mut compact, waiting) =
            start.inspected(&ready("run-2", "conv-1"), &draft(), false, false, 1.0)
        else {
            panic!("compact first")
        };
        assert!(waiting.starts_with("Waiting for compaction."));
        let session = Session {
            run_id: "run-2".into(),
            state: "running".into(),
            ..stopped()
        };
        assert_eq!(
            compact.prepared(
                true,
                Some(&session),
                None,
                Some(&ready("run-2", "conv-1")),
                false
            ),
            Step::Run(Action::CompactContext)
        );
        assert!(!compact.polling());
        compact.compact_finished(&Outcome::Done {
            receipt: json!({"request_id": "cmd-1"}),
            cleanup: None,
        });
        assert!(compact.polling());
        let mut details = ready("run-2", "conv-1");
        assert_eq!(
            compact.inspected(&details, &draft(), false, 2.0),
            Step::Idle,
            "no result yet"
        );
        details["compact_context_request"] = json!({"request_id": "cmd-0", "status": "completed"});
        assert_eq!(
            compact.inspected(&details, &draft(), false, 2.0),
            Step::Idle,
            "another compaction"
        );
        details["compact_context_request"] = json!({"request_id": "cmd-1", "status": "running"});
        assert_eq!(
            compact.inspected(&details, &draft(), false, 2.0),
            Step::Idle
        );
        details["compact_context_request"] = json!({"request_id": "cmd-1", "status": "completed"});
        assert_eq!(
            compact.inspected(&details, &draft(), false, 3.0),
            Step::Send {
                run_id: "run-2".into(),
                conversation_id: "conv-1".into(),
                compaction_id: Some("cmd-1".into())
            }
        );
        assert_eq!(
            compact.inspected(&details, &draft(), false, 3.5),
            Step::Idle,
            "never twice"
        );
    }

    #[test]
    fn compactions_end_with_qt_texts() {
        let session = Session {
            state: "running".into(),
            ..stopped()
        };
        let waiting = |status: &str| {
            let (mut compact, _) =
                CompactSend::begin(&session, "run-1", "conv-1", draft(), None, 0.0);
            compact.prepared(true, Some(&session), None, None, false);
            compact.compact_finished(&Outcome::Done {
                receipt: json!({"request_id": "cmd"}),
                cleanup: None,
            });
            let mut details = ready("run-1", "conv-1");
            details["compact_context_request"] = json!({"request_id": "cmd", "status": status});
            (compact, details)
        };
        let (mut compact, details) = waiting("unchanged");
        assert_eq!(
            compact.inspected(&details, &draft(), false, 1.0),
            Step::Cancelled(
                "No history could be compacted. Your draft was kept without sending.".into()
            )
        );
        let (mut compact, details) = waiting("uncertain");
        assert_eq!(
            compact.inspected(&details, &draft(), false, 1.0),
            Step::Cancelled(
                "Compaction did not complete. Check Terminal; your draft was kept without sending."
                    .into()
            )
        );
        let (mut compact, _) = waiting("running");
        assert_eq!(
            compact.inspected(&ready("run-1", "conv-2"), &draft(), false, 1.0),
            Step::Cancelled("Conversation changed. Your draft was kept without sending.".into())
        );
        let (compact, _) = waiting("running");
        assert!(
            matches!(compact.tick(600.5), Step::Cancelled(text) if text.starts_with("Compaction was not confirmed."))
        );
        let (mut refused, _) = CompactSend::begin(&session, "run-1", "conv-1", draft(), None, 0.0);
        assert_eq!(
            refused.prepared(
                true,
                Some(&session),
                None,
                Some(&ready("run-1", "conv-2")),
                false
            ),
            Step::Cancelled(
                "Compaction was cancelled. Your draft was kept without sending.".into()
            )
        );
        let (mut failed, _) = CompactSend::begin(
            &session,
            "run-1",
            "conv-1",
            draft(),
            Some("m-1".into()),
            0.0,
        );
        failed.prepared(true, Some(&session), None, None, false);
        assert_eq!(
            failed.compact_finished(&Outcome::Failed {
                detail: "Invalid acknowledgement. Check Terminal before retrying.".into()
            }),
            Step::Cancelled("Invalid acknowledgement. Check Terminal before retrying.".into())
        );
        assert_eq!(failed.retry_id.as_deref(), Some("m-1"));
    }
}
