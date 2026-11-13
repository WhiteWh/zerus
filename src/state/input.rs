//! Deliberate, identity-checked input to an existing interactive agent.
//! A receipt acknowledges terminal submission, never a model response. Once input
//! may have reached the PTY we retain its request ID and never automatically retry.
//! Codex's one-line status footer names its folder as `/…` or `~/…`, and with a
//! Windows platform home also as a drive path or `~\…`. Under inbox ConPTY the
//! hidden Claude and Kimi cursor stays at the end of the last painted row, so
//! their empty-composer checks fail closed there (spike S3).
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Duration;

pub(crate) mod composer;
mod diagnostic;
pub(crate) mod settle;
pub(crate) mod task_refusal;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) mod native_send;

/// Only this module's public dispatch constructs this provenance. Internal
/// submit* callers, including those using Origin::Person, cannot mint it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) struct PublicSend(());

const MAX_TEXT: usize = 64 * 1024;
const MAX_FILE: usize = 10 * 1024 * 1024;
const MAX_TOTAL: usize = 20 * 1024 * 1024;
const MAX_JSON: usize = 29 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    name: String,
    mime: String,
    data_base64: String,
    #[serde(default)]
    reference: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    text: String,
    expected_run_id: String,
    #[serde(default)]
    expected_conversation_id: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    expected_compaction_id: String,
    /// U35: hold the message in hgs while the previous input is unconfirmed
    /// (clients that read `input_hold_supported`); otherwise it is refused.
    #[serde(default)]
    queue_when_unconfirmed: bool,
}

fn validate(request: &Request) -> Result<Vec<Vec<u8>>> {
    uuid::Uuid::parse_str(&request.request_id).map_err(|_| "request_id must be a UUID")?;
    if request.expected_run_id.is_empty() || request.expected_run_id.len() > 128 {
        return Err("expected_run_id is required; refresh the session first".into());
    }
    if request.text.len() > MAX_TEXT {
        return Err("message exceeds 64 KiB".into());
    }
    if request
        .text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("message contains terminal control characters".into());
    }
    // A slash command's first token is /name. Absolute paths have another
    // slash in that token and are ordinary message text, even /help/file.png.
    let text = request.text.trim_start();
    let first = text.split_whitespace().next().unwrap_or("");
    let slash_command = first.strip_prefix('/').is_some_and(|name| !name.contains('/'));
    if slash_command || text.starts_with('!') {
        return Err("use Terminal for slash commands and shell commands".into());
    }
    if request.attachments.len() > 8 {
        return Err("at most 8 attachments are allowed".into());
    }
    if request.text.trim().is_empty() && request.attachments.is_empty() {
        return Err("message is empty".into());
    }
    let mut total = 0;
    attachments::validate_references(
        &request
            .attachments
            .iter()
            .map(|f| f.reference.as_str())
            .collect::<Vec<_>>(),
    )?;
    request
        .attachments
        .iter()
        .map(|attachment| {
            if attachment.name.is_empty()
                || attachment.name.len() > 255
                || attachment.name.chars().any(char::is_control)
                || attachment.mime.len() > 100
                || attachment.mime.chars().any(char::is_control)
            {
                return Err("invalid attachment metadata".into());
            }
            if attachment.data_base64.len() > MAX_FILE.div_ceil(3) * 4 {
                return Err("attachment exceeds 10 MiB".into());
            }
            let bytes = STANDARD
                .decode(&attachment.data_base64)
                .map_err(|_| "invalid attachment base64")?;
            if bytes.is_empty() || bytes.len() > MAX_FILE {
                return Err("attachments must be nonempty and at most 10 MiB".into());
            }
            total += bytes.len();
            if total > MAX_TOTAL {
                return Err("attachments exceed 20 MiB in total".into());
            }
            Ok(bytes)
        })
        .collect()
}

pub(super) fn validate_launch_prompt(text: &str) -> Result<()> {
    validate(&Request {
        request_id: uuid::Uuid::new_v4().to_string(), text: text.into(),
        expected_run_id: "launch".into(), expected_conversation_id: String::new(),
        attachments: Vec::new(), expected_compaction_id: String::new(), queue_when_unconfirmed: false,
    }).map(|_| ())
}

/// Provider TUIs can defer SessionStart until their first submitted turn.
/// Only supervised fresh runs or verifiable native Codex forks are candidates;
/// checked_input separately proves a fork's owned child history before delivery.
pub(super) fn first_message_candidate(record: &Value) -> bool {
    record["run_identity_version"] == 1
        && record["supervisor"].is_object()
        && AGENTS.contains(&string(record, "agent"))
        && !string(record, "run_id").is_empty()
        && string(record, "conversation_id").is_empty()
        && string(record, "expected_id").is_empty()
        && string(record, "requested_id").is_empty()
        && (string(record, "fork_parent_id").is_empty() || resume_input::fork_pending(record))
        && string(record, "error").is_empty()
        && record.get("session_end").is_none()
        && record["last_event_at"].as_f64().unwrap_or(0.0) == 0.0
        && record["input_pending_at"].as_f64().unwrap_or(0.0) == 0.0
        && ["", "unknown", "idle"].contains(&string(record, "activity"))
        && ["", "unknown", "idle"].contains(&string(record, "phase"))
        && record["startup_kind"] != "resume"
}

pub(super) fn first_message_summary(record: &Value, live_pane: bool) -> Value {
    if !string(record, "conversation_id").is_empty() {
        return json!({"first_message_can_send":false});
    }
    let reason = if record["input_pending_at"].as_f64().unwrap_or(0.0) > 0.0 {
        "Your first message was sent. Waiting for the agent to confirm this conversation."
            .to_owned()
    } else if !first_message_candidate(record) {
        "Waiting for the agent to confirm the requested conversation. Open Terminal to finish startup.".to_owned()
    } else if !live_pane {
        "The agent is not running. Open Terminal to finish startup.".to_owned()
    } else {
        checked_input(string(record, "name"), string(record, "run_id"), "", true)
            .and_then(|r| checked_terminal(&r, true))
            .err()
            .unwrap_or_default()
    };
    json!({"first_message_can_send":reason.is_empty(),"first_message_reason":reason})
}

fn checked_record(name: &str, request: &Request) -> Result<Value> {
    let record = checked_input(
        name,
        &request.expected_run_id,
        &request.expected_conversation_id,
        true,
    )?;
    compact_context::check_continuation(&record, &request.expected_compaction_id)?;
    Ok(record)
}

pub(super) fn checked_ready(name: &str, run_id: &str, conversation_id: &str) -> Result<Value> {
    checked_input(name, run_id, conversation_id, false)
}

pub(super) fn launch_ready(name: &str, run_id: &str, conversation_id: &str) -> Result<()> {
    checked_terminal(&checked_input(name, run_id, conversation_id, true)?, true)
}

/// A resume before its deferred SessionStart, verified as for its first message.
pub(super) fn checked_resumed(name: &str, run_id: &str, conversation_id: &str) -> Result<Value> {
    let record = checked_input(name, run_id, conversation_id, true)?;
    if !clear_context::resume_clearable(&record) {
        return Err("The agent is no longer waiting for its resumed conversation; refresh".into());
    }
    Ok(record)
}

/// A Claude turn stopped with Esc, whose prompt may take /clear before Ready.
pub(super) fn checked_interrupted(name: &str, run_id: &str, conversation_id: &str) -> Result<Value> {
    let record = checked_input(name, run_id, conversation_id, true)?;
    if !clear_context::stopped(&record) {
        return Err("The session is no longer interrupted; refresh before clearing".into());
    }
    Ok(record)
}

/// Explicit reset after a typed native failed-turn completion. This checks the
/// same live identity again immediately before the sole /clear submission.
pub(super) fn checked_failed(name: &str, run_id: &str, conversation_id: &str) -> Result<Value> {
    let record = checked_input(name, run_id, conversation_id, true)?;
    if !clear_context::failed_turn(&record) {
        return Err("The failed native turn is no longer settled; refresh before clearing".into());
    }
    Ok(record)
}

/// Session settings may precede the first user message, when Codex/Kimi have
/// not created a conversation yet. Other idle-only actions keep checked_ready.
pub(super) fn checked_settings(name: &str, run_id: &str, conversation_id: &str) -> Result<Value> {
    if !conversation_id.is_empty() { return checked_ready(name, run_id, conversation_id); }
    let record = checked_input(name, run_id, conversation_id, true)?;
    if !first_message_ready(&record) {
        return Err("The new session is not ready for model settings; refresh or open Terminal".into());
    }
    Ok(record)
}

/// A new session waiting at its first prompt with no work remaining, as the
/// listing reports it; its settings may change before the first message.
pub(super) fn first_message_ready(record: &Value) -> bool {
    first_message_candidate(record) && !telemetry::work_remains(record)
}

/// Settings changes, clear and compaction require an entirely idle session:
/// Ready as the listing reports it, which also gates forks.
fn idle_only(record: &Value) -> Result<()> {
    if telemetry::settled(record) {
        return Ok(());
    }
    Err("agent is busy or waiting for approval; wait for Ready or use Terminal".into())
}

/// Failed and interrupted turns may already accept another message at the native
/// composer. Verify readiness without clearing their recorded outcome.
pub(super) fn attention_message_summary(record: &Value, output: &mut Value, live_pane: bool) {
    let prefix = if output["phase"] == "error" && output["provider_error"].is_object() {
        "error"
    } else if output["phase"] == "interrupted" {
        "interrupted"
    } else {
        return;
    };
    let result = if live_pane {
        checked_input(
            string(record, "name"),
            string(record, "run_id"),
            string(record, "conversation_id"),
            true,
        )
        .and_then(|current| {
            checked_terminal(&current, true).map_err(|error| {
                // Stop returns ordinary Codex follow-ups to its input as one draft.
                if prefix == "interrupted" && error == DRAFT && input_queue::returned_by_stop(&current) {
                    RETURNED_FOLLOW_UPS.into()
                } else {
                    error
                }
            })
        })
    } else {
        Err("The agent is not running. Resume it before sending.".into())
    };
    output[format!("{prefix}_message_can_send")] = json!(result.is_ok());
    output[format!("{prefix}_message_reason")] = json!(result.err().unwrap_or_default());
}

/// What a resumed session's record says about a send to it now
/// (zerus-5wmh.66.4.9): the record gates of `hgs send` that change while a
/// resumed Claude or Kimi agent starts, read without any effect (no terminal
/// query, lock or write; its processes from /proc only). A continuation
/// condition only: `hgs send` checks every gate again under its input lock,
/// the panels shown only on the terminal included. A Codex resume pending its
/// first message shows its readiness only on its terminal, so it is never
/// ready here: the service asks `hgs send <session> --observe --json` for it
/// (zerus-5wmh.66.4.10).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// The record shows the agent takes a message now.
    Ready,
    /// Still starting, busy or not yet confirmed: read again.
    Starting,
    /// The resumed session failed, ends or left its conversation: no send.
    Ended,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn resumed_readiness(record: &Value) -> Readiness {
    if terminating::check_mutation(record).is_err()
        || !string(record, "error").is_empty()
        || !string(record, "archive_id").is_empty()
        || string(record, "conversation_id").is_empty()
    {
        return Readiness::Ended;
    }
    let unconfirmed = input_unconfirmed(record);
    if resume_input::pending(record)
        || !string(record, "expected_id").is_empty()
        || string(record, "activity") != "idle"
        || string(record, "phase") != "idle"
        || unconfirmed
        || settings_unconfirmed(record)
        || pause_active(record)
        || !process_alive(record)
    {
        return Readiness::Starting;
    }
    Readiness::Ready
}

/// Whether the agent has not confirmed the last input hgs typed (zerus-iitw8).
/// Only an input-acceptance event (UserPromptSubmit, a queued prompt, a turn or
/// task start) or hgs's own native proof sets `input_seen_at`; a busy agent's
/// later tool events never do. A record written before this rule has no
/// `input_seen_at` and keeps the event rule it was written under.
pub(crate) fn input_unconfirmed(record: &Value) -> bool {
    let seen = if record.get("input_seen_at").is_some() { "input_seen_at" } else { "last_event_at" };
    record["input_pending_at"].as_f64().is_some_and(|pending| {
        record[seen].as_f64().unwrap_or(0.0) <= pending
            // The person cleared the composer since (Terminal's Clear input,
            // zerus-wkj9l): what was typed is gone from it, by their choice.
            && record["input_cleared_at"].as_f64().unwrap_or(0.0) <= pending
    })
}

/// How long after `input_pending_at` a send may still hold its proof window:
/// the paste, `settle::WINDOW` and the wait for the lock to record its outcome.
/// An `in_progress` receipt older than this belongs to a sender that ended
/// without recording one; its input is stuck, and the person may clear it.
pub(crate) const PROOF_LIMIT: Duration = Duration::from_secs(40);

/// Whether a send to this run, from any client, is still in its proof window
/// (zerus-wkj9l.1): its input is unconfirmed, recent, and its receipt is
/// `in_progress` with no delivery outcome yet. Clear input refuses then: the
/// message being delivered may sit in the composer, and clearing it would
/// discard it and settle the send's pending gate. A send that ended uncertain
/// (its receipt names a `delivery`), was submitted or is older than
/// [`PROOF_LIMIT`] never blocks: an uncertain or stuck input stays clearable.
///
/// The window is measured on the wall clock (`now()` against
/// `input_pending_at`); the record keeps no monotonic or boot-time stamp of
/// its input. A backward clock step therefore keeps a crashed sender's
/// `in_progress` receipt in flight for longer, until the clock passes
/// `input_pending_at` + [`PROOF_LIMIT`] again: by at most the size of the
/// step, and only for a sender that never recorded an outcome. That errs
/// toward refusing Clear input, never toward discarding a message being
/// delivered (zerus-wkj9l.5 item 3).
pub(crate) fn send_in_flight(record: &Value) -> bool {
    let root = absolute_root().ok();
    in_flight(record, now(), |request| {
        let path = root.as_ref()?.join("input_receipts").join(format!("{request}.json"));
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    })
}

fn in_flight(record: &Value, at: f64, receipt: impl FnOnce(&str) -> Option<Value>) -> bool {
    if !input_unconfirmed(record) {
        return false;
    }
    let pending = record["input_pending_at"].as_f64().unwrap_or(0.0);
    if at - pending > PROOF_LIMIT.as_secs_f64() {
        return false;
    }
    let request = string(record, "input_pending_request");
    // A send recorded before this field names no receipt: its recent pending
    // input decides, as does a receipt that cannot be read yet.
    let Some(request) = uuid::Uuid::parse_str(request).ok().map(|id| id.to_string()).filter(|id| id == request) else {
        return true;
    };
    match receipt(&request) {
        // The named receipt decides only when it is this pending input's own:
        // submit_core writes its `submitted_at` as `input_pending_at`. Another
        // writer of `input_pending_at` (the ready-send keeper's `mark_input`)
        // leaves an earlier send's request beside a newer time; that receipt
        // says nothing about this input, so the recent pending input decides,
        // as for a receipt that cannot be read (zerus-wkj9l.5 item 2).
        Some(receipt) if receipt["submitted_at"].as_f64() == Some(pending) => {
            receipt["status"] == "in_progress"
                && receipt.get("delivery").is_none_or(Value::is_null)
                && string(&receipt, "run_id") == string(record, "run_id")
        }
        _ => true,
    }
}

fn checked_input(
    name: &str,
    run_id: &str,
    conversation_id: &str,
    allow_working: bool,
) -> Result<Value> {
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, run_id, conversation_id)?;
    let record = read(name)?;
    checked_bound_record(record, run_id, conversation_id, allow_working)
}

/// The keeper already holds the exact record, Root and SID. Reuse the native
/// gates without selecting a label, repairing an index or reacquiring input.
pub(in crate::state) fn checked_bound_record(
    record: Value, run_id: &str, conversation_id: &str, allow_working: bool,
) -> Result<Value> {
    terminating::check_mutation(&record)?;
    if string(&record, "run_id") != run_id || string(&record, "conversation_id") != conversation_id
    {
        return Err("session identity changed; refresh before sending".into());
    }
    if !matches(&record, panes_of(&live()?, &record)) || !process_alive(&record) {
        return Err("the exact agent process is no longer running; resume it explicitly".into());
    }
    if claude_approval::current(&record).is_some() {
        return Err("Claude is waiting for tool or safety approval. Review the request in Activity or Terminal before sending a message.".into());
    }
    if claude_resume::current(&record).is_some() {
        return Err("Claude is waiting for a conversation resume choice. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if claude_usage_limit::current(&record).is_some() {
        return Err("Claude is waiting for a usage-limit choice. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if claude_permission::current(&record).is_some() {
        return Err("Claude is waiting for a permission mode choice. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if claude_trust::current(&record).is_some() {
        return Err("Claude is waiting for folder trust approval. Choose an option in Activity or Terminal before sending a message.".into());
    }
    // A message typed now would go into the chooser.
    if claude_approval::idle_choice(&record).is_some() {
        return Err("Claude is waiting for a choice in Terminal. Answer it there before sending a message.".into());
    }
    if codex_trust::current(&record).is_some() {
        return Err("Codex is waiting for folder trust approval. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if codex_hooks_trust::current(&record).is_some() {
        return Err("Codex is waiting for hook trust review. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if codex_update::current(&record).is_some() {
        return Err("Codex is waiting for an update choice. Choose an option in Activity or Terminal before sending a message.".into());
    }
    if codex_rate_limit::current(&record).is_some() {
        return Err(codex_rate_limit::WAIT.into());
    }
    if codex_goal_resume::current(&record).is_some() {
        return Err(codex_goal_resume::WAIT.into());
    }
    if kimi_trust::current(&record).is_some() {
        return Err("Kimi is waiting for folder trust approval. Choose an option in Activity or Terminal before sending a message.".into());
    }
    let first_message =
        allow_working && conversation_id.is_empty() && first_message_candidate(&record);
    if first_message && !string(&record, "fork_parent_id").is_empty() {
        resume_input::fork_ready(&record)?;
    }
    let resumed_message = allow_working && resume_input::pending(&record);
    if resumed_message {
        resume_input::ready(&record)?;
    }
    if (string(&record, "conversation_id").is_empty() && !first_message)
        || (!string(&record, "expected_id").is_empty() && !resumed_message)
        || !string(&record, "error").is_empty()
        || pause_active(&record)
    {
        return Err(
            "agent identity is unconfirmed or the session is pausing; open Terminal".into(),
        );
    }
    if kimi_cache_hint::current(&record).is_some() {
        return Err("Kimi is waiting for your context choice. Choose an option in Activity or Terminal before sending another message.".into());
    }
    let ready = string(&record, "activity") == "idle" && string(&record, "phase") == "idle";
    let working = string(&record, "activity") == "busy"
        && ["idle", "working", "tool", "compacting"].contains(&string(&record, "phase"));
    let provider_failed = record["phase"] == "error" && record["provider_error"].is_object();
    let interrupted = record["phase"] == "interrupted";
    if allow_working && !ready && !working && !provider_failed && !interrupted && !first_message && !resumed_message
    {
        return Err(
            "agent is waiting for input, approval, or has an unknown state; open Terminal".into(),
        );
    }
    // Text can be submitted to a working agent's native composer. The terminal
    // checks below still require an empty recognized prompt, never a dialog.
    if !allow_working {
        idle_only(&record)?;
    }
    // zerus-iitw8.5: another send is still entering its message.
    if input_typing(&record).is_some() {
        return Err(TYPING_REFUSAL.into());
    }
    if !input_confirmed(&record) {
        return Err(UNCONFIRMED.into());
    }
    // Task #7: checked again on every send. The agent's own evidence (its
    // empty prompt showing the requested model and effort) confirms the
    // change; `hgs settings --check --accept` settles it on the person's
    // explicit confirmation, at an empty prompt only.
    if settings_unconfirmed(&record) && !effort::settings_evidence(&record).confirmed {
        return Err(SETTINGS_UNCONFIRMED.into());
    }
    Ok(record)
}

pub(super) const UNCONFIRMED: &str = "previous input has not been confirmed by the agent; inspect Terminal before sending again";

/// zerus-iitw8.5: another send's `input_typing` marker still holds the session.
pub(super) const TYPING_REFUSAL: &str = "previous input has not been confirmed yet: another message is being entered into this session; try again in a few seconds";

/// U35: the agent has taken the previous input. An input-acceptance event after
/// it confirms it (`input_unconfirmed`, zerus-iitw8): UserPromptSubmit at once for an
/// idle Claude or Codex, Kimi's UserPromptQueued while it works (dsh confirms in
/// its own RPC). A working
/// Claude or Codex queues the text in its TUI and reports UserPromptSubmit only
/// when that turn ends, so there the text shown in its queue panel confirms it.
fn input_confirmed(record: &Value) -> bool {
    if !input_unconfirmed(record) {
        return true;
    }
    if !["claude", "codex"].contains(&string(record, "agent")) {
        return false;
    }
    let request = string(record, "input_pending_request");
    if uuid::Uuid::parse_str(request).is_err() {
        return false;
    }
    let Ok(contents) = absolute_root().and_then(|root| {
        fs::read_to_string(root.join("input_receipts").join(format!("{request}.json"))).map_err(|e| e.to_string())
    }) else {
        return false;
    };
    let receipt: Value = serde_json::from_str(&contents).unwrap_or_default();
    receipt["status"] == "submitted"
        && kimi_tui_choice::capture(record)
            .is_ok_and(|screen| queued_on_screen(string(record, "agent"), &screen, string(&receipt, "submitted_text")))
}

/// Whether `screen` shows `text` in the agent's own queue panel.
fn queued_on_screen(agent: &str, screen: &str, text: &str) -> bool {
    !text.trim().is_empty() && input_queue::panel_text(agent, screen).is_some_and(|panel| paste_shown(&panel, text))
}

/// Keys of a model or effort change the agent may not have read yet. Unlike
/// sent text it is not a first message, so a new session keeps its first
/// message state; a later native event settles it.
pub(super) fn settings_unconfirmed(record: &Value) -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if effort::picker_entered() {
        match effort::picker_own_marker(record) {
            Ok(true) => return false,
            Err(_) => return true,
            Ok(false) => {},
        }
    }
    settings_unconfirmed_metadata(record)
}

fn settings_unconfirmed_metadata(record: &Value) -> bool {
    record["settings_unconfirmed_at"]
        .as_f64()
        .is_some_and(|at| record["last_event_at"].as_f64().unwrap_or(0.0) <= at)
}

/// Task #7: the refusal names what gets through: the agent showing the change
/// (checked on every send, by Check again and by each inspection), or Continue
/// anyway once Terminal shows its empty prompt.
const SETTINGS_UNCONFIRMED: &str = "the agent has not shown the last model or effort change yet; use Check again once it shows the new model and effort, or Continue anyway when Terminal shows its empty prompt";

/// Capture ANSI attributes so a faint Codex placeholder is distinguishable from
/// a user's existing draft. Never clear/edit the terminal's current draft.
/// Reverse video marks the caret Claude draws itself (`claude_caret_placeholder`).
#[derive(Clone, Copy, Default)]
struct TerminalStyle {
    dim: bool,
    background: bool,
    inverse: bool,
}

fn styled_cells(row: &str, style: &mut TerminalStyle) -> Vec<(char, TerminalStyle)> {
    let mut chars = row.chars().peekable();
    let mut result = Vec::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    if ch == 'm' {
                        // Extended color components are data, not SGR instructions.
                        let parameters: Vec<_> = code.split(';').collect();
                        let mut i = 0;
                        while i < parameters.len() {
                            match parameters[i] {
                                "" | "0" => *style = TerminalStyle::default(),
                                "22" => style.dim = false,
                                "2" => style.dim = true,
                                "7" => style.inverse = true,
                                "27" => style.inverse = false,
                                "49" => style.background = false,
                                "38" | "48" | "58" => {
                                    if parameters[i] == "48" {
                                        style.background = true;
                                    }
                                    i += if parameters.get(i + 1) == Some(&"2") {
                                        4
                                    } else {
                                        2
                                    };
                                }
                                code if code.parse::<u8>().is_ok_and(|code| {
                                    (40..=47).contains(&code) || (100..=107).contains(&code)
                                }) =>
                                {
                                    style.background = true
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                    }
                    break;
                }
                code.push(ch);
            }
        } else {
            result.push((ch, *style));
        }
    }
    result
}

fn styled_row(row: &str) -> Vec<(char, bool)> {
    styled_cells(row, &mut TerminalStyle::default())
        .into_iter()
        .map(|(ch, style)| (ch, style.dim))
        .collect()
}

/// Claude paints its own caret in reverse video over the first letter of a dim
/// placeholder in its empty composer, such as "Press up to edit queued
/// messages" while a message is queued or an idle suggestion. Accept exactly
/// that shape from the cursor column: one non-blank reverse cell, immediately
/// followed by dim text (possibly a space after a one-letter word), then only
/// blank or dim cells without reverse video, with visible dim continuation.
/// The caret on a draft's first letter is followed by normal text, and a lone
/// reverse character has no dim continuation; both remain drafts.
fn claude_caret_placeholder(cells: &[(char, TerminalStyle)]) -> bool {
    let [(caret, caret_style), rest @ ..] = cells else {
        return false;
    };
    caret_style.inverse
        && !caret.is_whitespace()
        && rest.first().is_some_and(|(_, style)| style.dim)
        && rest
            .iter()
            .any(|(ch, style)| style.dim && !ch.is_whitespace())
        && rest
            .iter()
            .all(|(ch, style)| !style.inverse && (style.dim || ch.is_whitespace()))
}

/// Released Codex uses a double chevron for Ultra's live composer. This is
/// only its prefix; callers still verify the cursor, styles and full input
/// boundary, or an exact command/draft under the usual native identity guards.
pub(super) fn codex_prompt(row: &str) -> Option<&str> {
    row.strip_prefix('›').or_else(|| row.strip_prefix('»'))
}

fn prompt_empty(agent: &str, row: &str, previous: &str, screen: &str, cursor: usize) -> bool {
    let styled = styled_row(row);
    let plain: String = styled.iter().map(|(ch, _)| ch).collect();
    let previous: String = styled_row(previous).iter().map(|(ch, _)| ch).collect();
    match agent {
        "codex" => {
            cursor == 2
                && codex_prompt(&plain).is_some_and(|tail| tail.is_empty() || tail.starts_with(' '))
                && styled
                    .iter()
                    .skip(2)
                    .all(|(ch, dim)| ch.is_whitespace() || *dim)
        }
        "claude" => {
            // Native Claude uses a non-breaking space after its prompt glyph.
            let mut chars = plain.chars();
            cursor == 2
                && matches!(chars.next(), Some('❯' | '>'))
                && chars.next().is_none_or(char::is_whitespace)
                && (styled
                    .iter()
                    .skip(2)
                    .all(|(ch, dim)| ch.is_whitespace() || *dim)
                    || styled_cells(row, &mut TerminalStyle::default())
                        .get(2..)
                        .is_some_and(claude_caret_placeholder))
        }
        "kimi" => {
            // kimi-code 2.x uses a boxed > prompt; its shell mode uses !.
            if cursor == 5
                && plain.starts_with(" │ > ")
                && previous.trim_start().starts_with('╭')
                && plain.chars().skip(5).collect::<String>().trim() == "│"
            {
                return true;
            }
            let recent: Vec<_> = screen.lines().rev().take(5).collect();
            let agent_mode = recent.iter().any(|line| {
                let plain: String = styled_row(line).iter().map(|(ch, _)| ch).collect();
                let words: Vec<_> = plain.split_whitespace().collect();
                words.iter().take(4).any(|word| *word == "agent")
                    && !words.iter().take(4).any(|word| *word == "shell")
            });
            // The older Python kimi-cli uses an unboxed panel and mode footer.
            agent_mode && (cursor == 1 && plain.trim().is_empty() && previous.contains(" input "))
        }
        _ => false,
    }
}

/// Kimi's startup progress frame: its spinner-led preparation banner is the
/// first non-blank row and the rest of the frame holds only the bordered
/// progress box. A live session has content above its composer and its status
/// footer below the box, so a transcript row that quotes the banner cannot
/// make the session look like it is still preparing.
fn kimi_preparing(screen: &str) -> bool {
    let rows: Vec<String> = screen
        .lines()
        .map(|row| styled_row(row).iter().map(|(ch, _)| ch).collect())
        .collect();
    let Some(first) = rows.iter().position(|row| !row.trim().is_empty()) else {
        return false;
    };
    spinner_banner(&rows[first])
        && rows[first + 1..].iter().all(|row| {
            let row = row.trim();
            row.is_empty() || progress_box_row(row)
        })
}

/// The preparation banner's own row: a spinner cell, then its progress line.
fn spinner_banner(row: &str) -> bool {
    let row = row.trim();
    let mut chars = row.chars();
    matches!(chars.next(), Some(ch) if ('\u{2800}'..='\u{28ff}').contains(&ch))
        && chars.as_str().trim_start().starts_with("Preparing Kimi Code")
}

/// A row of the startup progress box: its borders and the bordered `> ` row.
fn progress_box_row(row: &str) -> bool {
    row.starts_with(['╭', '│', '╰'])
        && row.chars().all(|ch| ch.is_whitespace() || "╭╮╰╯─│>".contains(ch))
}

/// Cursor at the start is not evidence that the entire composer is empty: a
/// multiline draft can continue underneath it. Scan until the provider's input
/// boundary, preserving ANSI state across rows as tmux capture-pane does.
pub(super) fn composer_empty(agent: &str, screen: &str, cursor_x: usize, cursor_y: usize) -> bool {
    composer_empty_for_model(agent, screen, cursor_x, cursor_y, None)
}

/// Claude Code 2.1.312 draws the input as `rule, ❯, rule, footer` with an effort
/// line above the top rule ("● high · /effort"), and parks its idle caret on
/// any of those rows. The empty prompt row makes the frame unambiguous: a draft
/// fills that row and this never accepts it. Older layouts keep their own
/// prompt-row check; this only answers where the caret sits.
fn claude_input_frame_empty(screen: &str, cursor_y: usize) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let plain = |index: usize| -> Option<String> {
        lines.get(index).map(|row| styled_row(row).iter().map(|(ch, _)| ch).collect())
    };
    let empty_prompt = |index: usize| {
        plain(index).is_some_and(|text| {
            let mut chars = text.chars();
            matches!(chars.next(), Some('❯' | '>')) && chars.all(char::is_whitespace)
        })
    };
    let rule = |index: usize| {
        plain(index).is_some_and(|text| {
            !text.starts_with(char::is_whitespace)
                && text.chars().count() >= 12
                && text.chars().all(|ch| "─━╌".contains(ch))
        })
    };
    let effort = |index: usize| {
        plain(index).is_some_and(|text| {
            let text = text.trim();
            text.starts_with('●') && text.contains("/effort")
        })
    };
    for composer in 0..lines.len() {
        if !empty_prompt(composer) || !rule(composer + 1) {
            continue;
        }
        // Only the 2.1.312 frame: the effort line, the top rule, the composer,
        // the bottom rule and its footer. An older layout with the effort on
        // the footer keeps its fail-closed caret handling.
        if composer < 2 || !rule(composer - 1) || !effort(composer - 2) {
            continue;
        }
        // The caret may rest anywhere around this frame: on it, above it, or on
        // a blank row below it. Text below the footer (a dialog) means the
        // screen is not a plain empty input.
        let footer = composer + 2;
        let blank = |index: usize| {
            lines.get(index).is_none_or(|row| styled_row(row).iter().all(|(ch, _)| ch.is_whitespace()))
        };
        if cursor_y <= footer || ((footer + 1)..=cursor_y).all(blank) {
            return true;
        }
    }
    false
}

// A local model has no GPT prefix. Accept its native footer only when the
// model/effort label matches this exact session's confirmed or launch model.
fn codex_model_footer(footer: &str, model: Option<&str>) -> bool {
    let Some(model) = model.filter(|model| !model.is_empty()) else { return false; };
    let label = footer.trim_start().split(" · ").next().unwrap_or("");
    let mut words = label.split_whitespace();
    words.next().is_some_and(|word| word.eq_ignore_ascii_case(model))
        && words.next().is_some_and(|effort| {
            ["default", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"].contains(&effort)
        })
        && words.next().is_none()
}

/// Codex's one-line model/folder status: this session's exact model or a GPT
/// label, then a folder.
fn codex_status_row(plain: &str, model: Option<&str>) -> bool {
    let footer = plain.to_ascii_lowercase();
    (codex_model_footer(plain, model) || footer.trim_start().starts_with("gpt-"))
        && footer.split_once(" · ").is_some_and(|(_, path)| {
            shown_path::status_folder(path, crate::platform::home().as_deref())
        })
}

fn codex_warning_footer(row: &str) -> bool {
    let plain: String = styled_row(row).iter().map(|(ch, _)| ch).collect();
    let Some(rest) = plain.trim().strip_prefix("⚠ ") else { return false; };
    let Some((count, tail)) = rest.split_once(' ') else { return false; };
    !count.is_empty() && count.len() <= 9 && count.bytes().all(|ch| ch.is_ascii_digit())
        && count.parse::<u32>().is_ok_and(|value| value > 0)
        && matches!(tail, "warning · f2 to view" | "warnings · f2 to view")
}

/// Native 0.167 adds the agent-picker hint before the idle shortcuts hint.
/// Accept only that fixed row and its optional existing warning indicator;
/// arbitrary trailing content still cannot terminate a draft.
fn codex_shortcuts_footer(plain: &str) -> bool {
    let plain = plain.trim();
    if plain.starts_with("? for shortcuts") { return true; }
    let Some(tail) = plain.strip_prefix("← for agents · ? for shortcuts") else { return false; };
    tail.is_empty() || (tail.starts_with(' ') && codex_warning_footer(tail))
}

pub(super) fn codex_footer_model(record: &Value) -> Option<&str> {
    if let Some(model) = record["model"].as_str().filter(|model| !model.is_empty()) {
        return Some(model);
    }
    // SessionStart can be deferred until the first turn. In that case the
    // saved native launch argv, not an unrelated account/model, is evidence.
    let args = record["base"].as_array()?;
    let mut model = None;
    for (index, arg) in args.iter().enumerate() {
        let Some(arg) = arg.as_str() else { continue; };
        if arg == "--" { break; }
        if ["-m", "--model"].contains(&arg) {
            model = args.get(index + 1).and_then(Value::as_str);
        } else if let Some(value) = arg.strip_prefix("--model=") {
            model = Some(value);
        }
    }
    model
}

fn composer_empty_for_model(agent: &str, screen: &str, cursor_x: usize, cursor_y: usize, model: Option<&str>) -> bool {
    // Kimi's startup progress screen draws its own bordered `> ` box before
    // the TUI opens a session (zerus-vg4mm). Its row and borders match the
    // boxed composer below, but nothing can be typed into it: the launch
    // prompt would be sent into a frame that is about to be replaced.
    if agent == "kimi" && kimi_preparing(screen) {
        return false;
    }
    let lines: Vec<_> = screen.lines().collect();
    let Some(row) = lines.get(cursor_y) else {
        return false;
    };
    let previous = cursor_y
        .checked_sub(1)
        .and_then(|y| lines.get(y))
        .copied()
        .unwrap_or("");
    if !prompt_empty(agent, row, previous, screen, cursor_x) {
        // Claude Code 2.1.312 frames the composer with an unindented rule above
        // and below and can leave its idle caret outside the prompt row (on the
        // rule or the footer) with the effort line above it. The empty frame is
        // the evidence then; a draft fills the prompt row and is refused here.
        if agent == "claude" && claude_input_frame_empty(screen, cursor_y) {
            return true;
        }
        return false;
    }
    let placeholder = styled_row(row)
        .iter()
        .skip(cursor_x)
        .any(|(ch, dim)| !ch.is_whitespace() && *dim);
    let mut style = TerminalStyle::default();
    let mut input_background = false;
    for (index, row) in lines.iter().enumerate().take(cursor_y + 1) {
        let cells = styled_cells(row, &mut style);
        if index == cursor_y {
            input_background =
                style.background || cells.iter().skip(cursor_x).any(|(_, cell)| cell.background);
        }
    }
    let boxed_kimi = agent == "kimi" && cursor_x == 5;
    let mut previous_blank = false;
    for (index, row) in lines.iter().enumerate().skip(cursor_y + 1) {
        let cells = styled_cells(row, &mut style);
        let plain: String = cells.iter().map(|(ch, _)| ch).collect();
        if boxed_kimi {
            // Input text always stays inside │…│; the separate bottom border
            // cannot be confused with a continuation beginning with spaces.
            if plain.starts_with(" ╰")
                && plain.trim_end().ends_with('╯')
                && plain
                    .chars()
                    .all(|ch| ch.is_whitespace() || "╰╯─".contains(ch))
            {
                return true;
            }
            if !plain.starts_with(" │") || plain.chars().skip(2).collect::<String>().trim() != "│"
            {
                return false;
            }
            continue;
        }
        if plain.trim().is_empty() {
            previous_blank = true;
            continue;
        }
        // Claude and Python kimi-cli terminate the input with an unindented
        // full-width separator. Draft continuation lines have an input indent.
        if matches!(agent, "claude" | "kimi")
            && !plain.starts_with(char::is_whitespace)
            && plain.chars().count() >= 12
            && plain.chars().all(|ch| "─━╌".contains(ch))
        {
            return true;
        }
        if agent == "codex"
            && (input_background || placeholder)
            && previous_blank
            && cells
                .iter()
                .filter(|(ch, _)| !ch.is_whitespace())
                .all(|(_, cell)| !cell.background)
            && index + 2 >= lines.len()
        {
            // Codex's shaded composer ends at its unshaded model/help footer.
            // Unknown layouts fail closed instead of treating arbitrary text
            // as status. A configured footer can always use the Terminal tab.
            let footer = plain.to_ascii_lowercase();
            let exact_model = codex_model_footer(&plain, model);
            if exact_model || ["gpt-", "context", "tokens", "? for shortcuts", "%"]
                .iter()
                .any(|token| footer.contains(token))
            {
                // Codex can render its composer without a shaded background
                // (for example an unfocused pane or a terminal theme). A dim
                // placeholder plus the actual shortcuts footer identifies this
                // layout; an arbitrary blank first line of a draft does not.
                // Codex 0.153 can show only its model/path footer, without
                // shortcuts. Require the dim placeholder and exact one-line
                // model/path shape; drafts never retain that placeholder.
                // Windows Codex prints `~\…` or a drive path (`shown_path`).
                let single_line_footer = placeholder
                    && lines[index + 1..].iter().all(|line| line.trim().is_empty() || codex_warning_footer(line))
                    && codex_status_row(&plain, model);
                return input_background || single_line_footer
                    || lines.last().is_some_and(|line| {
                        let plain: String = styled_row(line).iter().map(|(ch, _)| ch).collect();
                        codex_shortcuts_footer(&plain)
                    });
            }
        }
        return false;
    }
    !boxed_kimi
}

pub(super) fn checked_terminal(record: &Value, empty: bool) -> Result<()> {
    terminal_check(record, empty, false)
}

pub(in crate::state) fn checked_send_terminal(record: &Value) -> Result<()> {
    terminal_check(record, true, true)
}

fn terminal_check(record: &Value, empty: bool, sending: bool) -> Result<()> {
    terminal_check_on(record, empty, sending, backend::current())
}

/// The same native floor using this operation's explicit owned backend.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn checked_terminal_on(record: &Value, empty: bool, terminal: &dyn backend::TerminalBackend) -> Result<()> {
    terminal_check_on(record, empty, false, terminal)
}

fn terminal_check_on(record: &Value, empty: bool, sending: bool, terminal: &dyn backend::TerminalBackend) -> Result<()> {
    terminating::check_mutation(record)?;
    let pane = string(record, "pane");
    if !pane
        .strip_prefix('%')
        .is_some_and(|s| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err("invalid tracked pane".into());
    }
    let claim = backend::InputClaim {
        pane,
        // The pane's session is the record's own terminal (P3b).
        name: terminal_name(record),
        run_id: string(record, "run_id"),
        agent: string(record, "agent"),
        pid: record["pid"].as_u64(),
    };
    let state = terminal.input_state(&claim)?;
    if empty {
        // Kimi replaces its progress screen with the session while the launch
        // prompt waits, so its decision frame must be one read (zerus-vg4mm).
        let (state, screen) = if string(record, "agent") == "kimi" {
            stable_frame(terminal, &claim, state)?
        } else {
            let screen = terminal.capture(pane, backend::Capture::Styled)?;
            (state, String::from_utf8_lossy(&screen).into_owned())
        };
        let screen = screen.as_str();
        let cursor_y: usize = state
            .cursor_y
            .parse()
            .map_err(|_| "invalid terminal cursor")?;
        let cursor_x: usize = state
            .cursor_x
            .parse()
            .map_err(|_| "invalid terminal cursor")?;
        // A native redraw may move the composer between cursor and screen
        // reads. Never type using coordinates from a different frame.
        let stable = if sending && matches!(string(record, "agent"), "codex" | "claude") {
            let after = terminal.input_state(&claim)?;
            state.cursor_x == after.cursor_x && state.cursor_y == after.cursor_y
        } else { true };
        if !stable || !composer_empty_for_model(
            string(record, "agent"),
            screen,
            cursor_x,
            cursor_y,
            codex_footer_model(record),
        ) {
            if sending {
                diagnostic::save(record, screen, cursor_x, cursor_y, stable);
                if matches!(string(record, "agent"), "codex" | "claude")
                    && string(record, "activity") == "busy"
                    && matches!(string(record, "phase"), "idle" | "working" | "tool" | "compacting")
                {
                    return Err("agent busy: terminal queue prompt is unrecognized or contains a draft; wait for the agent or open Terminal; nothing was submitted".into());
                }
            }
            // U53 diagnostics: one native run prints which check refused and
            // where the caret and its row are, behind a flag only a test sets.
            if nonempty_env("HGS_INPUT_TRACE").is_some() {
                let agent = string(record, "agent");
                let lines: Vec<&str> = screen.lines().collect();
                let row = lines.get(cursor_y).copied().unwrap_or("");
                let previous = cursor_y
                    .checked_sub(1)
                    .and_then(|y| lines.get(y))
                    .copied()
                    .unwrap_or("");
                let prompt = prompt_empty(agent, row, previous, &screen, cursor_x);
                return Err(format!(
                    "{DRAFT} [trace: agent={agent} cursor=({cursor_x},{cursor_y}) prompt_empty={prompt} row={row:?}]"
                ));
            }
            return Err(DRAFT.into());
        }
    }
    Ok(())
}

/// One decision frame for Kimi: its cursor and styled screen from the same
/// read. The TUI redraws the composer as its progress screen is replaced
/// (zerus-vg4mm), so the pair is re-read until the cursor stops moving,
/// bounded: a terminal that keeps moving is refused rather than guessed.
/// `first` is the cursor read taken before the empty check; each retry takes
/// only the after-capture read and reuses it as the next attempt's before, so
/// a stable frame costs one read beyond that first one (zerus-0vjx5).
fn stable_frame(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    first: backend::InputState,
) -> Result<(backend::InputState, String)> {
    const ATTEMPTS: usize = 5;
    let mut before = first;
    for attempt in 0..ATTEMPTS {
        let screen = terminal.capture(claim.pane, backend::Capture::Styled)?;
        let after = terminal.input_state(claim)?;
        if before.cursor_x == after.cursor_x && before.cursor_y == after.cursor_y {
            return Ok((before, String::from_utf8_lossy(&screen).into_owned()));
        }
        before = after;
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Err("the terminal redrew while its prompt was read; nothing was submitted".into())
}

const DRAFT: &str = "terminal has a draft, dialog, or unrecognized prompt; open Terminal and return to an empty agent prompt";
/// The send-block reason while Codex's input holds exactly the follow-ups Stop returned.
const RETURNED_FOLLOW_UPS: &str =
    "The queued follow-ups are back in Codex's input as an unsent draft. Submit or clear them in Terminal.";

/// The whole draft in Codex's input: its composer and indented continuation rows,
/// then a blank row before the model/folder status row of its footer in the last
/// two screen rows. The cursor must be above that blank row; any other layout is unread.
pub(super) fn codex_draft(record: &Value, screen: &str, cursor_y: usize) -> Option<String> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let model = codex_footer_model(record);
    let footer = (lines.len().saturating_sub(2)..lines.len())
        .find(|&index| codex_status_row(lines[index], model))?;
    let (start, first) = lines[..footer]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, row)| Some((index, codex_prompt(row)?)))?;
    let rows = &lines[start + 1..footer];
    if !(start..footer - 1).contains(&cursor_y)
        || rows.last().is_none_or(|row| !row.is_empty())
        || rows.iter().any(|row| !row.is_empty() && !row.starts_with("  "))
    {
        return None;
    }
    let first = first.strip_prefix(' ').unwrap_or(first);
    let rows = rows.iter().map(|row| row.get(2..).unwrap_or(""));
    Some(std::iter::once(first).chain(rows).collect::<Vec<_>>().join("\n").trim_end().to_owned())
}

/// The whole draft in Claude's input: the prompt row right under its upper
/// separator, then indented rows up to the lower separator. The cursor must
/// stand inside it, so a dialog or the transcript's own prompt rows never
/// count; dim placeholder text is not a draft. `screen` is the styled capture.
pub(super) fn claude_draft(screen: &str, cursor_y: usize) -> Option<String> {
    let mut style = TerminalStyle::default();
    let rows: Vec<String> = screen
        .lines()
        .map(|row| styled_cells(row, &mut style).into_iter().map(|(ch, cell)| if cell.dim { ' ' } else { ch }).collect())
        .collect();
    let separator = |row: &str| {
        let row = row.trim_end();
        !row.starts_with(char::is_whitespace) && row.chars().count() >= 12 && row.chars().all(|ch| "─━╌".contains(ch))
    };
    let tail = |row: &str| -> Option<String> {
        let mut chars = row.strip_prefix('❯').or_else(|| row.strip_prefix('>'))?.chars();
        match chars.next() {
            None => Some(String::new()),
            // Native Claude uses a non-breaking space after its prompt glyph.
            Some(ch) if ch.is_whitespace() => Some(chars.collect()),
            _ => None,
        }
    };
    let start = (1..=cursor_y.min(rows.len().checked_sub(1)?)).rev().find(|&i| tail(&rows[i]).is_some())?;
    if !separator(&rows[start - 1]) {
        return None;
    }
    let end = (start + 1..rows.len().min(start + 400)).find(|&i| separator(&rows[i]))?;
    if !(start..end).contains(&cursor_y) {
        return None;
    }
    let first = tail(&rows[start])?;
    let rest = rows[start + 1..end].iter().map(|row| row.strip_prefix("  ").unwrap_or(row.trim_start()));
    let text = std::iter::once(first.as_str()).chain(rest).map(str::trim_end).collect::<Vec<_>>().join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Task 121: the draft in Kimi's input, from a recorded Kimi screen. Two
/// layouts speak: Kimi 2.5.x draws the composer as a `│…│` box closed by a
/// `╰…╯` border; older ones write a `>` prompt row with indented continuation
/// rows ended by a full-width rule. The box's rows are read up from the border
/// under the cursor; the plain prompt row is the last `>` row. A dialog or a
/// trust panel has neither and reads as no draft.
pub(super) fn kimi_draft(screen: &str, cursor_y: usize) -> Option<String> {
    let lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let border = (cursor_y + 1..lines.len()).find(|&index| {
        let row = lines[index].trim_start();
        row.starts_with('╰') && row.ends_with('╯') && row.chars().all(|ch| "╰─╯".contains(ch))
    });
    if let Some(border) = border {
        let mut rows: Vec<&str> = Vec::new();
        for row in lines[..border].iter().rev() {
            let Some(inner) = row
                .strip_prefix(' ')
                .and_then(|row| row.strip_prefix('│'))
                .and_then(|row| row.strip_suffix('│'))
            else {
                break;
            };
            rows.push(inner.trim_end());
        }
        rows.reverse();
        let text = rows.join("\n");
        let text = text.trim_start();
        let text = text.strip_prefix('>').unwrap_or(text).trim_start().trim_end();
        return (!text.is_empty()).then(|| text.to_owned());
    }
    let prompt = lines.iter().rposition(|row| {
        let row = row.trim_start();
        row.starts_with('>') && !row.starts_with(">>")
    })?;
    let mut rows = vec![lines[prompt].trim_start().trim_start_matches('>').trim_start()];
    for row in &lines[prompt + 1..] {
        let trimmed = row.trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.chars().count() >= 12 && trimmed.chars().all(|ch| "─━╌".contains(ch)) {
            break;
        }
        if !row.starts_with(char::is_whitespace) {
            break;
        }
        rows.push(row.trim_start());
    }
    let text = rows.join("\n");
    let text = text.trim_end();
    (!text.trim().is_empty()).then(|| text.to_owned())
}

/// The draft in the native composer, for the owner to see and submit from any
/// window (owner, 2026-10-28: text left in Terminal blocked every send).
/// Claude reads the styled capture, Codex the plain one, Kimi the plain one
/// (both its box and its plain prompt row); other agents none.
pub(super) fn native_draft(record: &Value, styled: &str, plain: &str, cursor_y: usize) -> Option<String> {
    match string(record, "agent") {
        "claude" => claude_draft(styled, cursor_y),
        "codex" => codex_draft(record, plain, cursor_y).filter(|draft| !draft.trim().is_empty()),
        "kimi" => kimi_draft(plain, cursor_y),
        _ => None,
    }
}

pub(super) fn safe_filename(name: &str, index: usize) -> String {
    let name: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ['.', '_', '-'].contains(&ch) {
                ch
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    format!("{:02}-{}", index + 1, name)
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len() == 4 && args[1] == "--confirm" && args[3] == "--json" {
        println!("{}", confirm_late(&args[0], &args[2])?);
        return Ok(0);
    }
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs send <session> --json (JSON from stdin), or hgs send <session> --confirm <request-id> --json".into());
    }
    let mut input = Vec::new();
    io::stdin()
        .take((MAX_JSON + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    if input.len() > MAX_JSON {
        return Err("message payload exceeds 29 MiB".into());
    }
    // Task 161: the lead's message telling a member to wait is a park instead.
    // Task 156: the lead's decision for a member is recorded with hgs team
    // decide, never only messaged.
    // Task 173: a team lead's message to its member naming a task is bound to
    // that task's attempt, or refused with the hand-out to use instead.
    let parsed = serde_json::from_slice::<Value>(&input).ok();
    let text = parsed.as_ref().and_then(|value| value["text"].as_str().map(str::to_owned)).unwrap_or_default();
    // Task #30 (team ede724c4): a "task" names the task the message is for;
    // a task the text only mentions is a reference.
    let task = parsed.as_ref().and_then(|value| value["task"].as_u64()).and_then(|id| u32::try_from(id).ok());
    super::teams::wait_loops::lead_send_check(&args[0], &text).map_err(|error| error.to_string())?;
    super::teams::decisions::lead_send_check(&args[0], &text).map_err(|error| error.to_string())?;
    let check = super::teams::shadow::lead_send(&args[0], &text, task).map_err(|error| error.to_string())?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(result) = native_send::front(&PublicSend(()), &args[0], &input) {
        println!("{}", result?);
        return Ok(0);
    }
    let receipt = submit(&args[0], &input, None)?;
    super::teams::shadow::record(&check.bindings, &text, "hgs send");
    if let Some(note) = super::teams::shadow::reference_note(&check.references) {
        eprintln!("hgs: {note}");
    }
    println!("{receipt}");
    Ok(0)
}

/// The refusal while a late check finds no proof.
const LATE_UNCERTAIN: &str =
    "delivery uncertain: Claude's transcript does not record this message; inspect Terminal before retrying";
/// The largest receipt a late check reads.
const LATE_RECEIPT_BYTES: u64 = 4 << 20;

/// WhiteWh fork (owner, 2026-11-13): `hgs send <session> --confirm <request>
/// --json`. A Claude delivery left uncertain, its proof missing when the
/// settle window closed, is submitted once Claude's transcript records it
/// after all: the window's own proof (a user row or queue entry carrying the
/// message, stamped after the receipt), read later. Nothing is typed or sent.
/// It prints the receipt once it is submitted, and refuses while the
/// transcript shows nothing, for another session, run or conversation, and
/// for a request that never reached the terminal.
fn confirm_late(name: &str, request_id: &str) -> Result<Value> {
    let request_id = uuid::Uuid::parse_str(request_id)
        .map_err(|_| "request ID must be a UUID")?
        .to_string();
    let path = absolute_root()?
        .join("input_receipts")
        .join(format!("{request_id}.json"));
    // The transcript is read without the input lock; the receipt is read again
    // under it before it changes.
    let receipt = late_receipt(&path, name, &request_id)?;
    if receipt["status"] == "submitted" {
        return Ok(receipt);
    }
    let record = read(name)?;
    if !settle::applies(string(&record, "agent"))
        || string(&record, "run_id") != string(&receipt, "run_id")
        || string(&record, "conversation_id") != string(&receipt, "conversation_id")
    {
        return Err(LATE_UNCERTAIN.into());
    }
    let transcript = PathBuf::from(string(&record, "transcript"));
    let message = receipt["submitted_text"].as_str().or(receipt["text"].as_str()).unwrap_or("").to_owned();
    let since = receipt["submitted_at"].as_f64().unwrap_or(f64::MAX);
    let offset = receipt["transcript_offset"].as_u64().unwrap_or(0);
    if !transcript.is_absolute() || message.trim().is_empty() {
        return Err(LATE_UNCERTAIN.into());
    }
    let proof = settle::late_proof(&transcript, offset, &message, since).ok_or(LATE_UNCERTAIN)?;
    let _guard = lock(None)?;
    let mut current = late_receipt(&path, name, &request_id)?;
    if current["status"] == "submitted" {
        return Ok(current);
    }
    if current != receipt {
        return Err(LATE_UNCERTAIN.into());
    }
    current["status"] = json!("submitted");
    current["delivery"] = json!("late");
    current["native"] = json!(proof.name());
    current["confirmed_at"] = json!(now());
    receipt_index::persist(&path, &current)?;
    Ok(current)
}

/// The receipt of `request_id` when it is this session's and either
/// submitted or a terminal delivery left uncertain.
fn late_receipt(path: &Path, name: &str, request_id: &str) -> Result<Value> {
    let size = fs::metadata(path).map_err(|_| "no delivery of this request is known")?.len();
    if size > LATE_RECEIPT_BYTES {
        return Err("the delivery receipt is too large to read".into());
    }
    let receipt: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("the delivery receipt cannot be read: {e}"))?;
    if string(&receipt, "request_id") != request_id || string(&receipt, "name") != name {
        return Err("this request was not sent to this session".into());
    }
    match string(&receipt, "status") {
        "submitted" => Ok(receipt),
        "in_progress" if receipt["delivery"] == "uncertain" && string(&receipt, "transport") == "terminal" => Ok(receipt),
        "in_progress" => Err("the request is still being delivered".into()),
        status => Err(format!("the request was not delivered ({status})")),
    }
}

/// The observation's stdin: one exact run and conversation of the session
/// (zerus-5wmh.66.4.10).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveRequest {
    expected_run_id: String,
    #[serde(default)]
    expected_conversation_id: String,
}
/// The observation's largest stdin.
const MAX_OBSERVE_INPUT: u64 = 8 << 10;
/// The longest refusal an observation reply carries, in characters.
const MAX_OBSERVE_REASON: usize = 1024;

/// `hgs send <session> --observe --json`: whether `hgs send` would take a
/// message for this exact run and conversation now, by its own gates (the
/// record, the exact live process, a pending resume's own verification and
/// the empty native composer), read without any effect. It takes no input
/// lock, types nothing (pending settings included), and writes no receipt,
/// attachment, diagnostic capture, record, attention or read mark. A
/// continuation condition only (zerus-5wmh.66.4.10): `hgs send` checks every
/// gate again under its input lock before its first byte. Only a Codex
/// session, whose readiness only its terminal shows, is observed; the CLI's
/// first disposition (`cli::observe_gate`) is the only caller.
pub(super) fn observe_dispatch(name: &str) -> Result<i32> {
    if !zerus_client_core::desktop::session_resume::observed(name) || name.contains('@') {
        return Err("hgs send --observe reads a local Codex session only".into());
    }
    let mut input = Vec::new();
    io::stdin()
        .take(MAX_OBSERVE_INPUT + 1)
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    if input.len() as u64 > MAX_OBSERVE_INPUT {
        return Err("observation payload exceeds 8 KiB".into());
    }
    let request: ObserveRequest =
        serde_json::from_slice(super::without_bom(&input)).map_err(|e| format!("invalid observation payload: {e}"))?;
    if request.expected_run_id.is_empty()
        || request.expected_run_id.len() > 128
        || request.expected_conversation_id.len() > 128
    {
        return Err("expected_run_id is required; refresh the session first".into());
    }
    // Among sessions that share the label, the request's own identities
    // select, as for a send, but read only: no index repair.
    let (ready, reason) =
        match super::resolve_label_unrepaired(name).and_then(|()| observe(name, &request)) {
            Ok(()) => (true, String::new()),
            Err(error) => (false, error.chars().take(MAX_OBSERVE_REASON).collect()),
        };
    println!("{}", json!({"ready": ready, "reason": reason}));
    Ok(0)
}

/// The send's own gates before its lock, read only: the record and its
/// process ([`checked_input`], which verifies a pending resume), then the
/// empty composer without the send's diagnostic capture.
fn observe(name: &str, request: &ObserveRequest) -> Result<()> {
    let record = checked_input(
        name,
        &request.expected_run_id,
        &request.expected_conversation_id,
        true,
    )?;
    checked_terminal(&record, true)
}

/// Plain text for the session's current run and conversation (task prompts).
pub(super) fn submit_task(session: &zerus_replica::SessionKey, text: &str, attachments: &[Value]) -> Result<Value> {
    let name = session.session.as_str();
    let record = read(name)?;
    if !session.session_id.is_empty() {
        let key = if string(&record, "launch_id").is_empty() { "run_id" } else { "launch_id" };
        if session.session_id != format!("{key}:{}", string(&record, key)) {
            return Err("task session identity changed; refresh the task".into());
        }
    }
    let request = json!({"request_id": uuid::Uuid::new_v4().to_string(), "text": text,
        "expected_run_id": string(&record, "run_id"),
        "expected_conversation_id": string(&record, "conversation_id"), "attachments": attachments});
    submit(name, request.to_string().as_bytes(), None)
}

pub(super) fn task_reply(session: &zerus_replica::SessionKey, dispatch: &str, id: &str, text: &str, attachments: &[Value]) -> Result<Value> {
    let record = read(&session.session)?;
    let key = if string(&record, "launch_id").is_empty() { "run_id" } else { "launch_id" };
    if session.session_id.is_empty() || session.session_id != format!("{key}:{}", string(&record, key)) {
        return Err("task execution identity changed; refresh before sending".into());
    }
    if let Ok(dispatch) = uuid::Uuid::parse_str(dispatch) {
        let bytes = fs::read(absolute_root()?.join("input_receipts").join(format!("{dispatch}.json")))
            .map_err(|_| "the task dispatch receipt is unavailable; delivery is not authorized")?;
        let receipt: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if receipt["status"] != "submitted" || string(&receipt,"run_id") != string(&record,"run_id")
            || !string(&receipt,"conversation_id").is_empty() && receipt["conversation_id"] != record["conversation_id"] {
            return Err("the task's dispatched run or conversation changed; refresh or hand off the task explicitly".into());
        }
    }
    let request = json!({"request_id":id,"text":text,"attachments":attachments,
        "expected_run_id":string(&record,"run_id"),"expected_conversation_id":string(&record,"conversation_id")});
    let parsed: Request = serde_json::from_value(request.clone()).map_err(|e| e.to_string())?;
    validate(&parsed)?;
    Ok(request)
}

/// Recovery is read-only, even when no receipt exists. Absence after a crash
/// does not authorize another terminal submission.
pub(super) fn task_reply_receipt(name: &str, request: &Value) -> Result<Option<Value>> {
    match task_reply_lookup(name, request)? {
        Lookup::Absent => Ok(None),
        Lookup::Found(receipt) => Ok(Some(receipt)),
        Lookup::Other => Err("message receipt identity or content does not match".into()),
    }
}

/// What the receipt store holds for a request, read only (#1519 R2). A
/// receipt that cannot be read or parsed is an error: after a possible
/// delivery it is unknown, never "nothing was sent".
pub(crate) enum Lookup {
    Absent,
    /// This exact request's receipt (its digest and name).
    Found(Value),
    /// A readable receipt of other content or for another session under the
    /// same ID: this request was not the one it records.
    Other,
}

pub(super) fn task_reply_lookup(name: &str, request: &Value) -> Result<Lookup> {
    let parsed: Request = serde_json::from_value(request.clone()).map_err(|e| e.to_string())?;
    // The submission names its receipt by the canonical form of the ID.
    let request_id = uuid::Uuid::parse_str(&parsed.request_id)
        .map_err(|_| "invalid message identity")?
        .to_string();
    let digest = format!("{:x}", Sha256::digest(request.to_string().as_bytes()));
    lookup(name, &request_id, &digest)
}

fn lookup(name: &str, request_id: &str, digest: &str) -> Result<Lookup> {
    let path = absolute_root()?
        .join("input_receipts")
        .join(format!("{request_id}.json"));
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lookup::Absent),
        Err(e) => return Err(e.to_string()),
    };
    let receipt: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    // Only a receipt that decodes whole into the schema this writes can tell
    // another request apart; anything else is unknown evidence (#1519 R3).
    let stored: Stored = serde_json::from_value(receipt.clone())
        .map_err(|e| format!("the receipt is malformed: {e}"))?;
    let hex = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if stored.request_id != request_id
        || stored.name.is_empty()
        || !hex(&stored.digest)
        || !matches!(stored.status.as_str(), "in_progress" | "submitted")
    {
        return Err("the receipt is malformed".into());
    }
    let _ = (
        &stored.run_id,
        &stored.conversation_id,
        stored.first_message,
    );
    if stored.digest != digest || stored.name != name {
        return Ok(Lookup::Other);
    }
    Ok(Lookup::Found(receipt))
}

/// The fields every input receipt holds, as `submit_core` writes them.
#[derive(Deserialize)]
struct Stored {
    request_id: String,
    name: String,
    run_id: String,
    conversation_id: String,
    first_message: bool,
    digest: String,
    status: String,
}

/// The receipt of request `request_id` to `name` whose exact bytes hashed to
/// `digest`, read only: an attempt's retained delivery binding recovers it
/// without the original request (C3 A3), typed as `task_reply_lookup`.
pub(super) fn task_reply_receipt_bound(
    name: &str,
    request_id: &str,
    digest: &str,
) -> Result<Lookup> {
    let request_id = uuid::Uuid::parse_str(request_id)
        .map_err(|_| "invalid message identity")?
        .to_string();
    lookup(name, &request_id, digest)
}

pub(super) fn submit(name: &str, input: &[u8], recovery_id: Option<&str>) -> Result<Value> {
    submit_with(name, input, recovery_id.map_or(Origin::Person, Origin::Recovery))
}

/// A held message, by its deliverer, through the same checks as any send.
pub(super) fn submit_held(name: &str, input: &[u8]) -> Result<Value> {
    submit_with(name, input, Origin::Held)
}

/// Who a submission comes from. Only the person's own input cancels automatic
/// recovery; question answers and keep-warm pings never type pending settings.
#[derive(Clone, Copy)]
enum Origin<'a> {
    Person,
    Answer,
    /// A queued message (U35), taken by its deliverer; it cancels recovery like the person's.
    Held,
    Recovery(&'a str),
    KeepWarm(&'a str),
}

/// A keep-warm ping (`keep_warm::TEXT`) for ping `id`. Like a task reply it
/// tells a refusal before any input from a possibly delivered ping.
pub(super) fn submit_keep_warm(name: &str, input: &[u8], id: &str) -> Submission {
    match submit_core(name, input, Origin::KeepWarm(id)) {
        Ok(receipt) => Submission::Receipt(receipt),
        Err(Failure::NotOffered(witness)) => Submission::NotOffered(*witness),
        Err(Failure::Unknown(error)) => Submission::Unknown(error),
    }
}

/// `submit` for a question's answer: it refuses, before any key, when pending
/// model or effort settings would be typed into the terminal first. Only the
/// answer's own paste may reach the pane, so a refusal proves nothing was
/// sent.
pub(super) fn submit_answer(name: &str, input: &[u8]) -> Result<Value> {
    submit_with(name, input, Origin::Answer)
}

fn submit_with(name: &str, input: &[u8], origin: Origin) -> Result<Value> {
    submit_core(name, input, origin).map_err(|failure| match failure {
        Failure::NotOffered(witness) => witness.reason.clone(),
        Failure::Unknown(error) => error,
    })
}

/// A live, closed witness that ONE exact request's payload was never offered
/// to its terminal (C3 activation; desktop-u's (a)). Only `submit_core` mints
/// one, inside the exact submission that observed it: under the input lock no
/// receipt existed for the request's ID in this receipt store, and the
/// submission refused before any settings key was typed and before the
/// payload's receipt marker was written. Its fields bind it to that request;
/// nothing reconstructs one from text, an exit status or a missing file.
#[derive(Debug)]
pub(crate) struct NotOffered {
    request_id: String,
    name: String,
    run_id: String,
    conversation_id: String,
    digest: String,
    receipts: PathBuf,
    pub(crate) reason: String,
}

impl NotOffered {
    /// Whether this witness is for exactly this request, submitted to `name`
    /// with this run, conversation and payload digest.
    pub(crate) fn binds(
        &self,
        request_id: &str,
        name: &str,
        run_id: &str,
        conversation_id: &str,
        digest: &str,
    ) -> bool {
        self.request_id == request_id
            && self.name == name
            && self.run_id == run_id
            && self.conversation_id == conversation_id
            && self.digest == digest
            && self
                .receipts
                .ends_with(format!("input_receipts/{request_id}.json"))
    }
}

/// What one exact task submission came to.
pub(crate) enum Submission {
    /// The request's exact submitted receipt.
    Receipt(Value),
    /// Positively never offered (`NotOffered`).
    NotOffered(NotOffered),
    /// Anything else: the payload may have reached the terminal.
    Unknown(String),
}

/// `submit` for a task reply that must tell a refusal before any input from
/// one that may have delivered it.
pub(super) fn submit_witnessed(name: &str, input: &[u8]) -> Submission {
    match submit_core(name, input, Origin::Person) {
        Ok(receipt) => Submission::Receipt(receipt),
        Err(Failure::NotOffered(witness)) => Submission::NotOffered(*witness),
        Err(Failure::Unknown(error)) => Submission::Unknown(error),
    }
}

/// How a submission failed.
enum Failure {
    NotOffered(Box<NotOffered>),
    Unknown(String),
}

/// What the receipt store already holds for a request ID, read under the
/// input lock. Only a positively absent receipt lets a later refusal witness
/// that nothing was offered; an existing one decides by itself.
enum Existing {
    Absent,
    /// The exact submitted acknowledgement: replayed as is.
    Submitted(Value),
    /// Possibly delivered before (in progress), unreadable, corrupt, or
    /// another payload under the same ID: never "not offered".
    Unknown(String),
}

fn existing(receipt_path: &Path, digest: &str, name: &str, held: bool) -> Existing {
    match fs::symlink_metadata(receipt_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Existing::Absent,
        Err(error) => return Existing::Unknown(error.to_string()),
        Ok(_) => {}
    }
    let receipt = match fs::read_to_string(receipt_path)
        .map_err(|e| e.to_string())
        .and_then(|contents| serde_json::from_str::<Value>(&contents).map_err(|e| e.to_string()))
    {
        Ok(receipt) => receipt,
        Err(error) => return Existing::Unknown(error),
    };
    existing_value_for(receipt, digest, name, held)
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn existing_value(receipt: Value, digest: &str, name: &str) -> Existing {
    existing_value_for(receipt, digest, name, false)
}

/// U35: `held` is the held-message deliverer's own read.
fn existing_value_for(receipt: Value, digest: &str, name: &str, held: bool) -> Existing {
    if string(&receipt, "digest") != digest || string(&receipt, "name") != name {
        return Existing::Unknown("request ID was already used for different content".into());
    }
    match string(&receipt, "status") {
        // Replay the exact durable acknowledgement. Reformatting parsed
        // floats can round its subsecond timestamp differently.
        "submitted" => Existing::Submitted(receipt),
        // U35: a held message answers with its receipt until its deliverer takes it.
        "queued" if held => Existing::Absent,
        "cancelled" if held => Existing::Unknown(input_hold::CANCELLED.into()),
        "queued" | "cancelled" | "failed" => Existing::Submitted(receipt),
        _ => Existing::Unknown(
            "delivery uncertain: this request may already be in Terminal; do not resend automatically"
                .into(),
        ),
    }
}

/// A refusal before the receipt marker: a witness that nothing was offered,
/// unless pending settings may already have been typed, which leaves it
/// unknown (desktop-u's (a)).
fn before_input(typed: bool, witness: NotOffered) -> Failure {
    if typed {
        Failure::Unknown(witness.reason)
    } else {
        Failure::NotOffered(Box::new(witness))
    }
}

/// zerus-iitw8.5: a Claude send releases the state lock while it waits for its
/// paste to settle and for Claude's proof. Its marker on the record keeps every
/// other input to that session out meanwhile: sends and session actions
/// (`checked_input`), Terminal input and Clear input (`terminal::check_handoff`).
/// It lasts the send's window plus slack, so a crash never blocks for long. The
/// window starts after the paste, and so does the marker's last term
/// (zerus-iitw8.8): slow work before the paste never lets it lapse inside it.
const TYPING: &str = "input_typing";
const TYPING_SECONDS: f64 = settle::WINDOW.as_secs_f64() + 4.0;

/// The request ID of the send typing into this session now, if any.
pub(crate) fn input_typing(record: &Value) -> Option<&str> {
    let marker = &record[TYPING];
    let left = marker["until"].as_f64()? - now();
    // Expired, or further ahead than any send's window: a clock stepped back.
    (left > 0.0 && left <= TYPING_SECONDS).then(|| marker["request_id"].as_str()).flatten()
}

/// Whether `current` still carries this send's live marker on the same run.
fn typing_ours(current: &Value, request_id: &str, run_id: &str) -> bool {
    string(current, "run_id") == run_id && input_typing(current) == Some(request_id)
}

/// Starts this send's marker term again when its window starts, after the
/// paste (zerus-iitw8.8). The send has held the state lock since it wrote the
/// marker, so the marker is still its own however long the paste took; the
/// term stays one window plus slack.
fn typing_from_paste(name: &str, request_id: &str, run_id: &str) -> Result<()> {
    let mut current = read(name)?;
    if string(&current, "run_id") != run_id || current[TYPING]["request_id"] != request_id {
        return Err("the session changed while the message was pasted; no Enter was pressed".into());
    }
    current[TYPING]["until"] = json!(now() + TYPING_SECONDS);
    write(&mut current)
}

/// Ends this send's marker, under the state lock. Claude's own record of the
/// message, in its transcript or queue, also confirms the input
/// (zerus-iitw8.6): its UserPromptSubmit hook may be missing or late, and a
/// queued prompt has none until it is dequeued. An emptied composer alone
/// never does (zerus-iitw8.8). Only this send's own pending input is
/// confirmed, never a later one.
fn end_typing(name: &str, record: &Value, request_id: &str, submitted_at: f64, recorded: bool) {
    let Ok(mut current) = read(name) else {
        return;
    };
    if string(&current, "run_id") != string(record, "run_id") {
        return;
    }
    let ours = current[TYPING]["request_id"] == request_id;
    if ours {
        if let Some(fields) = current.as_object_mut() {
            fields.remove(TYPING);
        }
    }
    let seen = recorded && current["input_pending_at"].as_f64() == Some(submitted_at);
    if seen {
        current["input_seen_at"] = json!(now());
    }
    if ours || seen {
        let _ = write(&mut current);
    }
}

fn submit_core(
    name: &str,
    input: &[u8],
    origin: Origin,
) -> std::result::Result<Value, Failure> {
    let unknown = Failure::Unknown;
    // N34: a BOM is not part of the request or of its digest.
    let input = crate::state::without_bom(input);
    let request: Request =
        serde_json::from_slice(input).map_err(|e| unknown(format!("invalid send payload: {e}")))?;
    let files = validate(&request).map_err(unknown)?;
    let digest = format!("{:x}", Sha256::digest(input));
    let request_id = uuid::Uuid::parse_str(&request.request_id)
        .unwrap()
        .to_string();
    let root = absolute_root().map_err(unknown)?;
    let receipt_path = root
        .join("input_receipts")
        .join(format!("{request_id}.json"));
    let mut guard = Some(lock(None).map_err(unknown)?);
    match existing(&receipt_path, &digest, name, matches!(origin, Origin::Held)) {
        Existing::Absent => {}
        Existing::Submitted(receipt) => return Ok(receipt),
        Existing::Unknown(error) => return Err(unknown(error)),
    }
    // From here no receipt existed for this ID. A refusal before any settings
    // key and before the receipt marker witnesses that nothing was offered;
    // once pending settings may have been typed, it is unknown.
    let mut typed = false;
    let refused = |typed: bool, reason: String| {
        before_input(
            typed,
            NotOffered {
                request_id: request_id.clone(),
                name: name.to_owned(),
                run_id: request.expected_run_id.clone(),
                conversation_id: request.expected_conversation_id.clone(),
                digest: digest.clone(),
                receipts: receipt_path.clone(),
                reason,
            },
        )
    };
    // Task 121: a delivery whose Enter did not go leaves its own paste in the
    // composer (`unsent_paste`). Before this delivery pastes, that exact paste
    // is finished once, so a Zerus message never stays unsent there; a draft
    // the person changed, another run or another conversation is left alone.
    finish_leftover(name, &request).map_err(|e| refused(typed, e))?;
    // U35: an opted-in message waits behind held ones and while the previous
    // input is unconfirmed; every other refusal stays.
    let opted = request.queue_when_unconfirmed
        && !matches!(origin, Origin::Held | Origin::Recovery(_) | Origin::KeepWarm(_));
    let checked = if opted && !input_hold::items(name).is_empty() {
        Err(UNCONFIRMED.to_owned())
    } else {
        checked_record(name, &request)
    };
    let record = match checked {
        Err(error) if opted && (error == UNCONFIRMED || error == TYPING_REFUSAL) => {
            let record = read(name).map_err(|e| refused(typed, e))?;
            if string(&record, "run_id") != request.expected_run_id
                || string(&record, "conversation_id") != request.expected_conversation_id
            {
                return Err(refused(typed, "session identity changed; refresh before sending".into()));
            }
            return input_hold::hold(name, &request_id, &digest, input, &record, request.text.trim()).map_err(unknown);
        }
        checked => checked.map_err(|e| refused(typed, e))?,
    };
    if matches!(origin, Origin::Answer) && effort::pending_applies(&record) {
        return Err(refused(
            typed,
            "model or effort settings would be typed first; answer this question in Terminal"
                .into(),
        ));
    }
    if matches!(origin, Origin::KeepWarm(_)) && effort::pending_applies(&record) {
        return Err(refused(typed, "model or effort settings are pending; no keep-warm ping was typed".into()));
    }
    // Pending settings may be typed here, whatever this returns.
    typed = effort::pending_applies(&record);
    let mut record = effort::apply_pending(name, record).map_err(unknown)?;
    match origin {
        Origin::Recovery(id) => recovery::check_submission(&record, id).map_err(|e| refused(typed, e))?,
        Origin::KeepWarm(id) => keep_warm::check_submission(&record, id).map_err(|e| refused(typed, e))?,
        Origin::Person | Origin::Answer | Origin::Held => {}
    }
    checked_send_terminal(&record).map_err(|e| refused(typed, e))?;
    let attachments_root = root.join("attachments");
    private_dir(&attachments_root).map_err(|e| refused(typed, e))?;
    let attachments = tempfile::Builder::new()
        .prefix("message-")
        .tempdir_in(attachments_root)
        .map_err(|e| refused(typed, e.to_string()))?;
    crate::platform::restrict_dir(attachments.path()).map_err(|e| refused(typed, e.to_string()))?;
    let mut paths = Vec::new();
    for (index, (attachment, bytes)) in request.attachments.iter().zip(files.iter()).enumerate() {
        let path = attachments
            .path()
            .join(safe_filename(&attachment.name, index));
        let mut file =
            crate::platform::private_open(OpenOptions::new().create_new(true).write(true))
                .open(&path)
                .map_err(|e| refused(typed, e.to_string()))?;
        file.write_all(bytes)
            .map_err(|e| refused(typed, e.to_string()))?;
        file.sync_all().map_err(|e| refused(typed, e.to_string()))?;
        paths.push(
            json!({"name":attachment.name,"mime":attachment.mime,"path":path,"bytes":bytes.len(),"reference":attachment.reference}),
        );
    }
    let text = attachments::terminal_text(request.text.trim(), &paths);
    // Claude and Kimi treat a trailing backslash plus the next Enter as line
    // continuation. Paste a final newline with the literal text so our single
    // Enter submits it; never retry Enter, which could answer a subsequently
    // opened dialog. Codex also needs that newline after a trailing @ token:
    // Enter would otherwise select its file/mention completion instead of
    // submitting. A space still has token affinity; a newline moves the cursor
    // off the token. Native submission trims the newline; receipts keep `text`.
    let agent = string(&record, "agent").to_owned();
    let pasted = if (matches!(agent.as_str(), "claude" | "kimi") && text.ends_with('\\'))
        || (agent == "codex"
            && text.split_whitespace().next_back().is_some_and(|token| token.starts_with('@')))
    {
        format!("{text}\n")
    } else {
        text.clone()
    };
    let buffer = format!("hgs-input-{request_id}");
    let terminal = backend::current();
    // A staging error may leave a buffer loaded: unknown, never a witness.
    if !terminal
        .stage(&buffer, pasted.as_bytes(), backend::StageErrors::Read)
        .map_err(unknown)?
    {
        return Err(refused(
            typed,
            "could not stage terminal input; nothing was submitted".into(),
        ));
    }
    let cleanup = || terminal.discard(&buffer);
    // Recheck immediately before the first input byte. All concurrent hgs writes
    // share this lock; a crash/exit can still race the PTY, so receipts are durable.
    if let Err(error) = checked_record(name, &request).and_then(|r| checked_send_terminal(&r)) {
        cleanup();
        return Err(refused(typed, error));
    }
    let checked = match origin {
        Origin::Recovery(id) => read(name).and_then(|record| recovery::check_submission(&record, id)),
        Origin::KeepWarm(id) => read(name).and_then(|record| keep_warm::check_submission(&record, id)),
        Origin::Person | Origin::Answer | Origin::Held => Ok(()),
    };
    if let Err(error) = checked {
        cleanup();
        return Err(refused(typed, error));
    }
    let submitted_at = now();
    if matches!(origin, Origin::Person | Origin::Answer | Origin::Held) {
        recovery::cancel_for_input(&record).map_err(unknown)?;
    }
    let mut receipt = json!({"request_id":request_id,"name":name,"run_id":request.expected_run_id,
        "conversation_id":string(&record,"conversation_id"),"first_message":string(&record,"conversation_id").is_empty(),"digest":digest,"status":"in_progress",
        "submitted_at":submitted_at,"submitted_text":text,"text":request.text.trim(),"attachments":paths,"transport":"terminal"});
    if let Some(target) = session_graph::target_receipt(&record) {
        delivery::bind(&mut receipt, &record, &target);
        receipt["graph_target"] = target;
    }
    if let Origin::KeepWarm(id) = origin {
        receipt["origin"] = json!("keep_warm");
        record["keep_warm_pending"] = json!({"request_id":request_id,"ping":id,"submitted_at":submitted_at});
    }
    // The marker may be published even when its write reports an error.
    if let Err(error) = receipt_index::persist(&receipt_path, &receipt) {
        cleanup();
        return Err(unknown(error));
    }
    // Preserve attachments after any possible input: the agent may read them later.
    let _ = attachments.keep();
    record["input_pending_at"] = json!(submitted_at);
    record["input_pending_request"] = json!(request_id);
    // Task 98: the composer was empty, so an earlier delivery's leftover
    // paste is gone.
    if let Some(fields) = record.as_object_mut() {
        fields.remove("unsent_paste");
    }
    // From this send on the record follows the acceptance rule.
    if record.get("input_seen_at").is_none() {
        record["input_seen_at"] = json!(0.0);
    }
    // zerus-iitw8: Claude needs native proof, and its waits release the state
    // lock (zerus-iitw8.5): this send's marker keeps other input out until then.
    let native = settle::applies(string(&record, "agent"));
    if native {
        record[TYPING] = json!({"request_id": request_id, "until": submitted_at + TYPING_SECONDS});
    }
    if let Err(error) = write(&mut record) {
        cleanup();
        return Err(unknown(error));
    }
    let pane = string(&record, "pane");
    // Task 98: the composer's draft right after our own paste. When the Enter
    // does not go, this hash is how Zerus later knows the leftover is its own.
    let mut pasted_draft: Option<String> = None;
    // Claude's transcript is read from here.
    let mut transcript = native.then(|| settle::Transcript::of(&record)).flatten();
    // Where a later proof starts (`confirm_late`).
    let transcript_offset = transcript.as_ref().map(settle::Transcript::position);
    let claim = backend::InputClaim {
        pane,
        name: terminal_name(&record),
        run_id: string(&record, "run_id"),
        agent: string(&record, "agent"),
        pid: record["pid"].as_u64(),
    };
    // One window for the settle and the proof, set once after the paste.
    let mut deadline = std::time::Instant::now() + settle::WINDOW;
    let mut residue: Option<String> = None;
    let mut pasted_at = now();
    let result: Result<()> = (|| {
        pasted_at = now();
        terminal.paste(&buffer, pane, backend::Paste::Composer)?;
        deadline = std::time::Instant::now() + settle::WINDOW;
        if native {
            // Still under the lock: the marker's term starts with the window.
            typing_from_paste(name, &request_id, string(&record, "run_id"))?;
            // Enter only once the composer shows the paste settled: an Enter
            // while Claude still processes pasted images submits nothing. The
            // wait reads only the terminal and holds no lock, so every other
            // session's hooks run meanwhile (zerus-iitw8.5).
            guard = None;
            loop {
                let shown = settle::settle(terminal, &claim, deadline).map_err(|composer| {
                    residue = composer;
                    "Claude's composer never showed the pasted message settled; no Enter was pressed".to_string()
                })?;
                guard = writer_until(deadline)?;
                if guard.is_none() {
                    return Err("the state lock stayed busy until the window ended; no Enter was pressed".into());
                }
                // Under the lock again: still this send's marker on the same
                // run, and the composer still shows exactly what settled.
                if !typing_ours(&read(name)?, &request_id, string(&record, "run_id")) {
                    return Err("the session changed while the message settled; no Enter was pressed".into());
                }
                if settle::still(terminal, &claim, &shown) {
                    break;
                }
                // It changed meanwhile: settle again, unlocked, in the same window.
                guard = None;
            }
        } else {
            // Let bracketed-paste handlers finish before Enter.
            std::thread::sleep(Duration::from_millis(180));
        }
        // Never send a second Enter as a retry: that could approve a new
        // dialog or duplicate a turn.
        #[cfg(windows)]
        if string(&record, "agent") == "codex" {
            settle_codex_paste(terminal, pane, &pasted);
        }
        pasted_draft = super::terminal::read_draft(&record)
            .ok()
            .and_then(|(draft, hash)| draft.filter(|draft| !draft.trim().is_empty()).map(|_| hash));
        if !process_alive(&record) || !matches(&record, panes_of(&live()?, &record)) {
            return Err("agent changed after paste".into());
        }
        checked_terminal(&record, false)?;
        terminal.send(pane, &[backend::Key::Named("Enter")])?;
        // Task 121: Kimi 2.5 may take that Enter as the paste's own confirm
        // step; while the composer still holds exactly our paste, one more
        // Enter submits it. A draft that still shows it afterwards is left
        // recorded as unsent, never pasted again.
        if agent == "kimi" {
            if let Some(hash) = pasted_draft.as_deref() {
                let ours = || {
                    super::terminal::read_draft(&record).ok().is_some_and(|(draft, current)| {
                        draft.is_some_and(|draft| !draft.trim().is_empty()) && current == hash
                    })
                };
                let wait = |limit: Duration| {
                    let deadline = std::time::Instant::now() + limit;
                    while std::time::Instant::now() < deadline {
                        if !ours() { return true; }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    !ours()
                };
                if !wait(Duration::from_millis(1_500)) {
                    terminal.send(pane, &[backend::Key::Named("Enter")])?;
                    if !wait(Duration::from_millis(1_500)) {
                        return Err("Kimi still shows the pasted message after Enter; nothing was pasted again".into());
                    }
                }
            }
        }
        if !native {
            receipt["status"] = json!("submitted");
            receipt_index::persist(&receipt_path, &receipt)?;
        }
        Ok(())
    })();
    cleanup();
    // The receipt and the record are written under the state lock again.
    if result.is_err() && guard.is_none() {
        guard = Some(lock(None).map_err(unknown)?);
    }
    let uncertain = |receipt: &mut Value, error: String, residue: Option<String>| {
        // The receipt stays `in_progress` (never resent) and names why and
        // what the composer held, bounded text only.
        receipt["delivery"] = json!("uncertain");
        receipt["uncertain_reason"] = json!(error);
        receipt["composer"] = json!(residue);
        // Where a later proof starts (`confirm_late`).
        if let Some(offset) = transcript_offset {
            receipt["transcript_offset"] = json!(offset);
        }
        let _ = receipt_index::persist(&receipt_path, receipt);
        crate::analytics::adapters::delivery(&root, &record, receipt, crate::analytics::adapters::Delivery::Uncertain);
        let shown = residue
            .map(|text| {
                let short: String = text.chars().take(200).collect();
                format!("; the composer holds: {short}{}", if text.chars().count() > 200 { "…" } else { "" })
            })
            .unwrap_or_default();
        unknown(format!("delivery uncertain: {error}{shown}; inspect Terminal before retrying"))
    };
    if let Err(error) = result {
        if let Some(hash) = &pasted_draft {
            record_unsent_paste(name, &receipt_path, &mut receipt, &request_id, hash);
        }
        if native {
            end_typing(name, &record, &request_id, submitted_at, false);
        }
        return Err(uncertain(&mut receipt, error, residue));
    }
    if native {
        // Claude runs its UserPromptSubmit hook, which takes this lock, before
        // it records the message: wait for the proof without holding it. The
        // typing marker keeps other input out meanwhile.
        drop(guard.take());
        let proof = settle::confirm(terminal, &claim, transcript.as_mut(), &text, pasted_at, deadline);
        let _guard = lock(None).map_err(unknown)?;
        end_typing(name, &record, &request_id, submitted_at, matches!(proof, Ok(proof) if proof.recorded()));
        match proof {
            Ok(proof) => {
                receipt["status"] = json!("submitted");
                receipt["native"] = json!(proof.name());
                receipt_index::persist(&receipt_path, &receipt).map_err(unknown)?;
            }
            Err(composer) => {
                return Err(uncertain(
                    &mut receipt,
                    "Claude recorded no submission of this message".into(),
                    composer,
                ))
            }
        }
    }
    crate::analytics::adapters::delivery(&root, &record, &receipt, crate::analytics::adapters::Delivery::Submitted);
    Ok(receipt)
}

/// Task 98: a delivery pasted but its Enter did not go: the receipt and the
/// session record keep the composer's draft hash, so `hgs terminal`'s
/// submit_own_paste can finish exactly this paste once. Best effort: a
/// failure only leaves the leftover for the owner, as before.
fn record_unsent_paste(name: &str, receipt_path: &Path, receipt: &mut Value, request_id: &str, hash: &str) {
    receipt["pasted_draft_hash"] = json!(hash);
    let _ = atomic(receipt_path, &receipt.to_string());
    if let Ok(mut fresh) = read(name) {
        if fresh["run_id"] == receipt["run_id"] && fresh["conversation_id"] == receipt["conversation_id"] {
            fresh["unsent_paste"] = json!({"request_id":request_id,"draft_hash":hash,"run_id":receipt["run_id"],
                "conversation_id":receipt["conversation_id"],"at":now()});
            let _ = write(&mut fresh);
        }
    }
}

/// Task 121: finishes the delivery's own leftover paste once with Enter before
/// the next delivery. The record's `unsent_paste` and the composer's draft hash
/// must still match exactly (the run, conversation, pane, process and draft);
/// anything else is the owner's and is left. The claim is durable before the
/// Enter, so a second delivery never finishes it again; an Enter that does not
/// go is uncertain and stops the next delivery instead of pasting over it.
fn finish_leftover(name: &str, request: &Request) -> Result<()> {
    let mut record = read(name)?;
    if record["unsent_paste"]["request_id"].as_str().is_none_or(str::is_empty) {
        return Ok(());
    }
    let Ok((draft, hash)) = super::terminal::read_draft(&record) else { return Ok(()); };
    let Ok(delivery) = super::terminal::own_paste(&record, draft.as_deref(), &hash) else { return Ok(()); };
    let pane = string(&record, "pane").to_owned();
    let Some(fields) = record.as_object_mut() else { return Ok(()); };
    fields.remove("unsent_paste");
    write(&mut record)?;
    if let Err(error) = backend::current().send(&pane, &[backend::Key::Named("Enter")]) {
        return Err(format!(
            "delivery uncertain: the leftover paste may have been submitted ({error}); inspect Terminal before retrying"
        ));
    }
    let receipt = absolute_root()?.join("input_receipts").join(format!("{delivery}.json"));
    if let Ok(bytes) = std::fs::read(&receipt) {
        if let Ok(mut original) = serde_json::from_slice::<Value>(&bytes) {
            if original["status"] != "submitted" {
                original["status"] = json!("submitted");
                original["finished_by"] = json!(request.request_id);
                atomic(&receipt, &original.to_string())?;
            }
        }
    }
    Ok(())
}

/// Codex on Windows reads console key events, not VT input, so the pseudoconsole
/// turns a bracketed paste into typed keys, and Codex's paste-burst handling makes
/// an Enter within 120 ms of the last of them a newline. Wait until the composer
/// shows the paste (or Codex's placeholder for a long one), then past that
/// window; when it never shows within the limit, Enter goes as before.
#[cfg(windows)]
fn settle_codex_paste(terminal: &dyn backend::TerminalBackend, pane: &str, pasted: &str) {
    let limit = std::time::Instant::now()
        + Duration::from_millis(2_000 + 4 * pasted.len().min(4_000) as u64);
    while std::time::Instant::now() < limit {
        let shown = terminal
            .capture(pane, backend::Capture::Plain)
            .is_ok_and(|screen| paste_shown(&String::from_utf8_lossy(&screen), pasted));
        if shown {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(200));
}

/// Whether `screen` shows all of `pasted` (wrapping, indentation and borders
/// aside), only its end when it is too long to fit, or Codex's placeholder for a
/// long paste. The whole text counts, as a reply's closing tag repeats its opening.
fn paste_shown(screen: &str, pasted: &str) -> bool {
    let squeeze = |text: &str| -> String {
        text.chars()
            .filter(|ch| !ch.is_whitespace() && !matches!(ch, '│' | '┃' | '|'))
            .collect()
    };
    if screen.contains("[Pasted Content") {
        return true;
    }
    let pasted: Vec<char> = squeeze(pasted).chars().collect();
    let shown: String = pasted[pasted.len().saturating_sub(2_000)..].iter().collect();
    !shown.is_empty() && squeeze(screen).contains(&shown)
}

/// The sends carrying --observe that `state::dispatch` refuses before
/// provider routing (zerus-5wmh.66.4.10.1): a DeepSeek, an OpenCode and a
/// Codex session's, in either flag order. Shared with the Windows
/// control-console probe, which admits them in an isolated profile
/// (zerus-be8zv).
#[cfg(test)]
pub(crate) const OBSERVE_SENDS: [[&str; 4]; 4] = [
    ["send", "dsh/example/session", "--observe", "--json"],
    ["send", "opencode/example/session", "--observe", "--json"],
    ["send", "codex/example/session", "--observe", "--json"],
    ["send", "codex/example/session", "--json", "--observe"],
];

#[cfg(test)]
mod tests {
    use super::*;

    /// zerus-5wmh.66.4.10.1: a send carrying --observe that reaches the state
    /// dispatch by any route but the CLI's first gate (`cli::observe_gate`)
    /// refuses there, before the OpenCode and DeepSeek routing whose own
    /// sends would ignore the flag. Unix always admits local sessions. On
    /// Windows that depends on the profile, so `state`'s control-console
    /// probe checks the same sends in an isolated one (zerus-be8zv).
    #[cfg(unix)]
    #[test]
    fn a_send_with_observe_refuses_before_provider_routing() {
        for args in OBSERVE_SENDS {
            let args: Vec<String> = args.map(str::to_owned).into();
            let refusal = crate::state::dispatch(&args).unwrap_err();
            assert!(refusal.contains("is taken only as"), "{args:?}: {refusal}");
        }
    }

    /// zerus-5wmh.66.4.9: a resumed session's record alone, never its
    /// terminal, decides whether a deferred send may start; only a confirmed,
    /// idle, live agent is ready, and a pending Codex resume never is.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_resumed_record_is_ready_only_when_confirmed_idle_and_alive() {
        let pid = std::process::id();
        let ready = json!({"name": "claude/resumed", "agent": "claude", "run_id": "run-2",
            "conversation_id": "conversation-1", "expected_id": "", "activity": "idle",
            "phase": "idle", "error": "", "pid": pid, "process_start": process_start(pid)});
        assert_eq!(resumed_readiness(&ready), Readiness::Ready);
        let with = |change: &dyn Fn(&mut Value)| {
            let mut copy = ready.clone();
            change(&mut copy);
            resumed_readiness(&copy)
        };
        // Still starting: not yet confirmed, busy, unconfirmed input or
        // settings, or no live agent process.
        for (key, value) in [
            ("expected_id", json!("conversation-1")),
            ("activity", json!("busy")),
            ("phase", json!("starting")),
            ("activity", json!("unknown")),
            ("input_pending_at", json!(5.0)),
            ("settings_unconfirmed_at", json!(5.0)),
            ("pid", json!(0)),
            ("process_start", json!("other")),
        ] {
            assert_eq!(
                with(&|r| r[key] = value.clone()),
                Readiness::Starting,
                "{key}"
            );
        }
        // A confirmed later event clears an earlier pending input.
        assert_eq!(
            with(&|r| {
                r["input_pending_at"] = json!(5.0);
                r["last_event_at"] = json!(6.0);
            }),
            Readiness::Ready
        );
        // Ended: the supervisor failed, the session left its conversation,
        // is archived or is being terminated.
        assert_eq!(
            with(&|r| r["error"] = json!("launch failed")),
            Readiness::Ended
        );
        assert_eq!(
            with(&|r| r["conversation_id"] = json!("")),
            Readiness::Ended
        );
        assert_eq!(
            with(&|r| r["archive_id"] = json!("00000000-0000-4000-8000-000000000001")),
            Readiness::Ended
        );
        // A Codex resume pending its first message shows readiness only on
        // its terminal: never ready from its record (zerus-5wmh.66.4.10).
        let codex = with(&|r| {
            r["agent"] = json!("codex");
            r["run_identity_version"] = json!(1);
            r["startup_kind"] = json!("resume");
            r["supervisor"] = json!({"pid": pid, "start": process_start(pid)});
            r["expected_id"] = json!("conversation-1");
            r["activity"] = json!("unknown");
            r["phase"] = json!("unknown");
        });
        assert_eq!(codex, Readiness::Starting);
    }

    /// desktop-u's (a): what the receipt store already holds decides by
    /// itself. Only a positively absent receipt lets a later refusal witness
    /// that nothing was offered; an in-progress, corrupt, unreadable or
    /// mismatched one is unknown, and a submitted one is replayed.
    #[test]
    fn an_existing_receipt_decides_and_never_witnesses_no_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input_receipts").join("id.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(matches!(existing(&path, "d", "n", false), Existing::Absent));
        let write = |receipt: Value| fs::write(&path, receipt.to_string()).unwrap();
        write(json!({"digest": "d", "name": "n", "status": "submitted"}));
        assert!(
            matches!(existing(&path, "d", "n", false), Existing::Submitted(r) if r["status"] == "submitted")
        );
        for (receipt, digest, name) in [
            (
                json!({"digest": "d", "name": "n", "status": "in_progress"}),
                "d",
                "n",
            ),
            (
                json!({"digest": "d", "name": "n", "status": "submitted"}),
                "other",
                "n",
            ),
            (
                json!({"digest": "d", "name": "n", "status": "submitted"}),
                "d",
                "other",
            ),
        ] {
            write(receipt);
            assert!(matches!(
                existing(&path, digest, name, false),
                Existing::Unknown(_)
            ));
        }
        fs::write(&path, b"{corrupt").unwrap();
        assert!(matches!(existing(&path, "d", "n", false), Existing::Unknown(_)));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(existing(&path, "d", "n", false), Existing::Unknown(_)));
        // A store that cannot even be inspected is unknown, never absent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::remove_dir(&path).unwrap();
            let store = path.parent().unwrap();
            fs::set_permissions(store, fs::Permissions::from_mode(0o000)).unwrap();
            let inspected = existing(&path, "d", "n", false);
            fs::set_permissions(store, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(matches!(inspected, Existing::Unknown(_)));
        }
    }

    /// A refusal before the marker is a witness only while no pending
    /// setting may have been typed; after one may have, it is unknown.
    #[test]
    fn a_refusal_after_a_pending_setting_is_unknown() {
        let witness = || NotOffered {
            request_id: "id".into(),
            name: "n".into(),
            run_id: "r".into(),
            conversation_id: "c".into(),
            digest: "d".into(),
            receipts: PathBuf::from("state/input_receipts/id.json"),
            reason: "refused".into(),
        };
        assert!(
            matches!(before_input(false, witness()), Failure::NotOffered(w) if w.binds("id", "n", "r", "c", "d"))
        );
        assert!(
            matches!(before_input(true, witness()), Failure::Unknown(reason) if reason == "refused")
        );
        let bound = witness();
        for (id, name, run, conversation, digest) in [
            ("other", "n", "r", "c", "d"),
            ("id", "other", "r", "c", "d"),
            ("id", "n", "other", "c", "d"),
            ("id", "n", "r", "other", "d"),
            ("id", "n", "r", "c", "other"),
        ] {
            assert!(!bound.binds(id, name, run, conversation, digest));
        }
    }

    #[test]
    fn a_late_proof_submits_an_uncertain_claude_delivery_once() {
        // WhiteWh fork (owner, 2026-11-13): Claude recorded the message after
        // the settle window closed; nothing is typed again.
        let dir = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(dir.path(), || {
            const ID: &str = "6b2f385b-126e-4abe-ae7f-1c6425b5575b";
            let transcript = dir.path().join("t.jsonl");
            fs::write(&transcript, "").unwrap();
            let mut record = json!({"version":1,"created":100,"name":"claude/x/lead","agent":"claude","run_id":"run",
                "conversation_id":"conv","transcript":transcript.to_string_lossy()});
            write(&mut record).unwrap();
            let receipts = absolute_root().unwrap().join("input_receipts");
            fs::create_dir_all(&receipts).unwrap();
            let path = receipts.join(format!("{ID}.json"));
            let receipt = json!({"request_id":ID,"name":"claude/x/lead","run_id":"run","conversation_id":"conv",
                "status":"in_progress","delivery":"uncertain","transport":"terminal","submitted_at":1_794_515_751.8,
                "text":"so is svn ready?","submitted_text":"so is svn ready?",
                "uncertain_reason":"Claude recorded no submission of this message"});
            fs::write(&path, receipt.to_string()).unwrap();
            let on_disk = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(confirm_late("claude/x/lead", ID).unwrap_err(), LATE_UNCERTAIN);
            assert_eq!(on_disk(), receipt, "no proof changes nothing");
            assert!(confirm_late("claude/x/other", ID).is_err(), "another session's request");
            assert!(confirm_late("claude/x/lead", "not-a-request").is_err());
            let row = json!({"type":"user","timestamp":"2026-11-12T20:35:58.505Z","message":{"content":"so is svn ready?"}});
            fs::write(&transcript, format!("{row}\n")).unwrap();
            let done = confirm_late("claude/x/lead", ID).unwrap();
            assert_eq!(
                (string(&done, "status"), string(&done, "delivery"), string(&done, "native")),
                ("submitted", "late", "transcript")
            );
            assert_eq!(on_disk(), done);
            assert_eq!(confirm_late("claude/x/lead", ID).unwrap(), done, "the same receipt again");
            // A moved conversation proves nothing about the old one.
            let other = "11111111-2222-3333-4444-555555555555";
            let mut moved = receipt.clone();
            moved["request_id"] = json!(other);
            moved["conversation_id"] = json!("older");
            fs::write(receipts.join(format!("{other}.json")), moved.to_string()).unwrap();
            assert_eq!(confirm_late("claude/x/lead", other).unwrap_err(), LATE_UNCERTAIN);
        });
    }

    #[test]
    fn previous_input_is_confirmed_by_a_later_event_or_the_native_queue_panel() {
        // Any native event after the input confirms it (UserPromptSubmit,
        // Kimi's UserPromptQueued); one at the same moment does not.
        assert!(input_confirmed(&json!({"agent":"kimi"})));
        assert!(!input_confirmed(&json!({"agent":"kimi","input_pending_at":10.0,"last_event_at":10.0})));
        assert!(input_confirmed(&json!({"agent":"kimi","input_pending_at":10.0,"last_event_at":10.5})));
        // Without a recorded request, a working Claude waits for its next event.
        assert!(!input_confirmed(&json!({"agent":"claude","input_pending_at":10.0,"last_event_at":9.0})));
        // A working Claude or Codex shows the text in its own queue panel.
        let claude = "● Thinking…\n\n❯ please also check the\n  migrations\nctrl+x ctrl+s to send now\n\n❯ \n";
        assert!(queued_on_screen("claude", claude, "please also check the migrations"));
        assert!(!queued_on_screen("claude", claude, "a different message"));
        assert!(!queued_on_screen("claude", "❯ please also check the migrations\n", "please also check the migrations"));
        assert!(!queued_on_screen("claude", claude, "  "));
        let codex = "• Working (4s • esc to interrupt)\n\n• Queued follow-up inputs\n  ↳ run the tests again\n    shift+← edit last queued message\n\n\n› Ask Codex to do anything\n";
        assert!(queued_on_screen("codex", codex, "run the tests again"));
        // Kimi reports the queued prompt as an event instead.
        assert!(!queued_on_screen("kimi", claude, "please also check the migrations"));
    }

    #[test]
    fn unread_settings_keys_wait_for_a_native_event() {
        let mut record = json!({"settings_unconfirmed_at": 10.0, "last_event_at": 9.0});
        assert!(settings_unconfirmed(&record));
        record["last_event_at"] = json!(10.0);
        assert!(settings_unconfirmed(&record));
        record["last_event_at"] = json!(10.5);
        assert!(!settings_unconfirmed(&record));
        assert!(!settings_unconfirmed(&json!({"last_event_at": 0.0})));
    }

    #[test]
    fn idle_only_dispatch_and_new_session_settings_follow_the_listing() {
        // Clear, compaction and settings dispatch accept the Ready session the
        // listing reports, though a replaced Kimi hook entry still says working.
        let first = |record: &Value| {
            let mut record = record.clone();
            record["conversation_id"] = json!("");
            record["last_event_at"] = json!(0);
            record["run_identity_version"] = json!(1);
            record["supervisor"] = json!({});
            record
        };
        let stale = telemetry::stale_hook_child();
        assert!(idle_only(&stale).is_ok());
        assert!(first_message_candidate(&first(&stale)) && first_message_ready(&first(&stale)));
        for (case, record) in telemetry::remaining_work() {
            assert_eq!(idle_only(&record).unwrap_err(),
                "agent is busy or waiting for approval; wait for Ready or use Terminal", "{case}");
            assert!(!first_message_ready(&first(&record)), "{case}");
        }
    }
    #[test]
    fn a_paste_shows_by_its_end_through_wrapping_or_as_a_placeholder() {
        let envelope = "<send_user_message_question_reply>\n[{\"answer\":\"Two\",\"question\":\"Which option?\"}]\n</send_user_message_question_reply>";
        // Codex wraps the composer's rows; the end still reads through.
        let screen = "› <send_user_message_question_reply>\n  [{\"answer\":\"Two\",\"quest\n  ion\":\"Which option?\"}]\n  </send_user_message_question_reply>\n\n  GPT default · ~\\project";
        assert!(paste_shown(screen, envelope));
        // Only the start so far: not shown yet.
        assert!(!paste_shown("› <send_user_message_question_reply>\n  [{\"answer\":", envelope));
        assert!(paste_shown("› [Pasted Content 4096 chars]", &"x".repeat(4096)));
        assert!(!paste_shown("› ", ""));
    }
    #[test]
    fn entire_input_region_must_be_empty() {
        assert!(!composer_empty(
            "kimi",
            " ╭────────────╮\n │ > │        │\n ╰────────────╯",
            5,
            1
        ));
        assert!(!composer_empty(
            "kimi",
            " ╭────────────╮\n │ >          │\n │  │         │\n ╰────────────╯",
            5,
            1
        ));
        assert!(!composer_empty(
            "codex",
            "›\n  existing second line\n",
            2,
            0
        ));
        assert!(!composer_empty(
            "claude",
            "❯\n  existing second line\n──────────────\nstatus",
            2,
            0
        ));
        assert!(composer_empty("claude", "❯\n──────────────\nstatus", 2, 0));
        assert!(!composer_empty(
            "kimi",
            " ╭────╮\n │ >  │\n │ old│\n ╰────╯\nstatus",
            5,
            1
        ));
        assert!(composer_empty(
            "kimi",
            "\x1b[38;2;90;90;90m ╭────╮\n │ >  │\n ╰────╯\nstatus",
            5,
            1
        ));
        let codex = "\x1b[48;2;61;64;64m› \x1b[2mAsk anything\x1b[0m\x1b[48;2;61;64;64m\n\n\x1b[49m  gpt-6-astra · 80% context";
        assert!(composer_empty("codex", codex, 2, 0));
        assert!(!composer_empty(
            "codex",
            &codex.replace("\n\n", "\n  existing\n\n"),
            2,
            0
        ));
        // A color component equal to49 must not reset an explicit RGB background.
        let colored_draft = "\x1b[48;2;61;64;64m› \n\n\x1b[38;2;49;49;49m  gpt-6-astra text";
        assert!(!composer_empty("codex", colored_draft, 2, 0));
    }
    #[test]
    fn claude_nonbreaking_prompt_space_is_empty_but_real_drafts_are_not() {
        // Claude 2.1.289 uses NBSP after the native prompt glyph.
        let screen = "Ready\n\x1b[39m❯\u{a0}\n──────────────\n  auto mode on";
        assert!(composer_empty("claude", screen, 2, 1));
        for replacement in ["❯\u{a0}draft", "❯\u{a0}\n  second line", "❯\u{a0}Yes, trust this folder", "$ "] {
            assert!(!composer_empty("claude", &screen.replace("❯\u{a0}", replacement), 2, 1));
        }
        assert!(!composer_empty("claude", screen, 3, 1));
    }
    #[test]
    fn claude_2312_effort_line_and_framed_composer_keep_an_empty_input() {
        // Native 2.1.312 layout: header, a blank body, the effort line, then
        // rule, composer, rule, footer. The caret may rest anywhere in that
        // frame while the composer is empty.
        let rule = "─".repeat(80);
        let mut lines: Vec<String> = vec![
            " ▐▛███▛█   Claude Code v2.1.312".into(),
            "▝▜█████▛▘  Sonnet 4.6 · API Usage Billing".into(),
            " ▝▝   ▝▝   C:\\work\\example".into(),
        ];
        lines.extend(std::iter::repeat_n(String::new(), 8));
        lines.push(format!("{:>62}● high · /effort", ""));
        lines.push(rule.clone());
        lines.push("❯\u{a0}".into());
        lines.push(rule.clone());
        lines.push("  ⏸ manual mode on · ? for shortcuts · ← for agents".into());
        let screen = lines.join("\n");
        let composer = 13;
        for y in [composer - 2, composer - 1, composer, composer + 1, composer + 2] {
            assert!(composer_empty("claude", &screen, 2, y), "caret row {y}");
        }
        // A draft fills the composer row: no frame, whatever the caret row.
        let drafted = screen.replace("❯\u{a0}", "❯ fix the build");
        for y in [composer - 2, composer - 1, composer + 1, composer + 2] {
            assert!(!composer_empty("claude", &drafted, 2, y), "drafted row {y}");
        }
        // A dialog that replaces the composer row is not an empty input.
        let dialog = screen.replace("❯\u{a0}", "╭─ Question ─╮");
        assert!(!composer_empty("claude", &dialog, 2, composer));
        // A hidden caret parked on a blank row below the footer is the same
        // input; text there (a dialog) is never accepted.
        let mut parked = lines.clone();
        parked.extend([String::new(), String::new(), String::new()]);
        assert!(composer_empty("claude", &parked.join("\n"), 0, composer + 4));
        let mut text_below = lines.clone();
        text_below.push("  Approve command?".into());
        assert!(!composer_empty("claude", &text_below.join("\n"), 0, composer + 3));
    }

    #[test]
    fn claude_draft_reads_the_composer_only() {
        let rule = "────────────────────────────────────────";
        let screen = |input: &str| format!("❯ Use the tool now.\n\n● Done.\n\n{rule}\n{input}\n{rule}\n  ⏸ manual mode on · ? for shortcuts\n");
        assert_eq!(claude_draft(&screen("❯\u{a0}fix the build"), 5).as_deref(), Some("fix the build"));
        let multi = screen("❯ first line\n  second line\n    indented");
        assert_eq!(claude_draft(&multi, 7).as_deref(), Some("first line\nsecond line\n  indented"));
        // Empty, a dim placeholder, the cursor outside, or a chooser: no draft.
        assert_eq!(claude_draft(&screen("❯\u{a0}"), 5), None);
        assert_eq!(claude_draft(&screen("❯ \x1b[2mTry \"fix lint errors\"\x1b[0m"), 5), None);
        assert_eq!(claude_draft(&screen("❯ fix the build"), 0), None);
        assert_eq!(claude_draft(&screen("❯ fix the build"), 7), None);
        let chooser = format!("{rule}\n ☐ Model switch\n\n❯ 1. Switch automatically\n  2. Stay\n{rule}\n  3. Chat about this\n");
        assert_eq!(claude_draft(&chooser, 3), None);
        // No lower separator: an unknown layout is unread.
        assert_eq!(claude_draft(&format!("{rule}\n❯ draft\n"), 1), None);
        let record = json!({"agent":"claude"});
        assert_eq!(native_draft(&record, &screen("❯ fix it"), "", 5).as_deref(), Some("fix it"));
        assert_eq!(native_draft(&json!({"agent":"kimi"}), &screen("❯ fix it"), "", 5), None);
    }
    #[test]
    fn styled_cells_track_reverse_video_without_reading_color_data() {
        let cells = styled_cells(
            "\x1b[38;5;7ma\x1b[7mb\x1b[27mc\x1b[7;2md\x1b[0me\x1b[7mf\x1b[me\x1b[48;2;7;7;7mg",
            &mut TerminalStyle::default(),
        );
        let inverse: String = cells
            .iter()
            .map(|(ch, style)| match style.inverse {
                true => ch.to_ascii_uppercase(),
                false => *ch,
            })
            .collect();
        assert_eq!(inverse, "aBcDeFeg");
        assert!(cells[3].1.dim && !cells[4].1.dim && cells[7].1.background);
    }
    #[test]
    fn claude_placeholder_under_its_drawn_caret_is_empty_but_drafts_are_not() {
        // Claude Code 2.1.314 while a message is queued: an empty composer with
        // a dim placeholder whose first letter carries Claude's reverse caret.
        let rule = "─".repeat(40);
        let queued =
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0;2mress up to edit queued messages\x1b[0m";
        let screen = |row: &str| {
            format!(
                "  ctrl+x ctrl+s to send now\n\n· Bunning… (2m 34s · ↓ 12.2k tokens)\n  ⎿  Tip: Use /btw to ask a quick side question without interrupting Claude's current work\n\n{rule}\n{row}\n{rule}\n\n  ⏵⏵ bypass permissions on · 1 shell"
            )
        };
        assert!(composer_empty("claude", &screen(queued), 2, 6));
        // Any placeholder drawn this way, such as an idle suggestion after NBSP.
        let suggestion = "\x1b[38;5;246m❯\u{a0}\x1b[7mT\x1b[0;2mry \"fix the failing test\"\x1b[0m";
        assert!(composer_empty("claude", &screen(suggestion), 2, 6));
        // The cursor must be on the caret, and a draft continuing under the
        // placeholder row is still a draft.
        assert!(!composer_empty("claude", &screen(queued), 3, 6));
        let continued = screen(&format!("{queued}\n  second line"));
        assert!(!composer_empty("claude", &continued, 2, 6));
        for draft in [
            // The caret on the first letter of normal text.
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0mress up to edit queued messages",
            // A lone reverse character, also before blank space or later dim text.
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0m",
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0m   ",
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0m \x1b[2mress up\x1b[0m",
            // Normal text after the dim run, or reverse video beyond one cell.
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0;2mress up\x1b[0m draft",
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[2mress up\x1b[0m",
            "\x1b[38;5;246m❯ \x1b[7m\x1b[39mP\x1b[0;2mress \x1b[7mu\x1b[27mp\x1b[0m",
            // Reverse video switched off before the letter.
            "\x1b[38;5;246m❯ \x1b[7m\x1b[27mP\x1b[0;2mress up\x1b[0m",
            // Text before the caret.
            "\x1b[38;5;246m❯ x\x1b[7mP\x1b[0;2mress up\x1b[0m",
        ] {
            assert!(!composer_empty("claude", &screen(draft), 2, 6), "{draft:?}");
            assert!(!composer_empty("claude", &screen(draft), 3, 6), "{draft:?}");
        }
        // A question's Other editor draws the same caret, but is never a composer.
        let other = include_str!("../../tests/fixtures/claude-question-other.txt")
            .replace("Type something.", "\x1b[7mT\x1b[0;2mype something.\x1b[0m");
        let y = other.lines().position(|row| row.contains("ype ")).unwrap();
        for x in [2, 5] {
            assert!(!composer_empty("claude", &other, x, y));
        }
        // Codex and Kimi rules are unchanged: a reverse cell is not their placeholder.
        let codex = "\x1b[1m›\x1b[0m \x1b[7mA\x1b[0;2msk anything\x1b[0m";
        assert!(!prompt_empty("codex", codex, "", "", 2));
        let kimi = " │ > \x1b[7mA\x1b[0;2msk\x1b[0m │";
        assert!(!prompt_empty("kimi", kimi, " ╭──────────╮", "", 5));
    }
    #[test]
    fn claude_one_letter_ghost_suggestion_is_empty_only_with_dim_continuation() {
        // Synthetic shape: Claude's inverse caret covers the first letter;
        // the following space and predicted words are faint, not user input.
        let row = "\x1b[39m❯ \x1b[7mI\x1b[0;2m can add a test\x1b[0m";
        let screen = |row: &str| format!("Ready\n──────────────\n{row}\n──────────────\nstatus");
        assert!(composer_empty("claude", &screen(row), 2, 2));
        assert!(!composer_empty("claude", &screen(row), 3, 2));
        for changed in [
            row.replace("0;2m", "0m"),
            row.replace(" can", " \x1b[22mcan\x1b[2m"),
            row.replace(" can", " \x1b[7mcan\x1b[27m"),
            "\x1b[39m❯ \x1b[7mI\x1b[0;2m   \x1b[0m".into(),
            "\x1b[39m❯ \x1b[7mI\x1b[0m \x1b[2mcan add a test\x1b[0m".into(),
            format!("{row}\n  multiline draft"),
        ] {
            assert!(
                !composer_empty("claude", &screen(&changed), 2, 2),
                "{changed:?}"
            );
        }
    }
    #[test]
    fn placeholder_is_not_a_draft_or_dialog() {
        assert!(prompt_empty(
            "codex",
            "\x1b[1m›\x1b[0m \x1b[2mAsk anything\x1b[0m",
            "",
            "",
            2
        ));
        assert!(!prompt_empty("codex", "› do something", "", "", 2));
        assert!(!prompt_empty(
            "codex",
            "› \x1b[38;2;22;2;2mdo something",
            "",
            "",
            2
        ));
        assert!(!prompt_empty("codex", "› approve", "", "", 9));
        assert!(prompt_empty("claude", "❯ ", "", "", 2));
        assert!(!prompt_empty("kimi", "$ ", "", "shell ~/repo", 2));
        assert!(prompt_empty(
            "kimi",
            " ",
            "── input ──",
            "agent (kimi-k2) ~/repo",
            1
        ));
    }
    #[test]
    fn codex_unshaded_composer_with_two_line_status_bar() {
        // Native capture of the unfocused Codex composer; all user content in
        // the status bar is replaced. Colors are optional, not input state.
        let screen = "Working\n\n\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  \x1b[38;2;246;226;183mGPT-6-Astra xhigh\x1b[39m · /project · Session title    Pursuing goal (1h)\n  \x1b[1m?\x1b[0m for shortcuts";
        assert!(composer_empty("codex", screen, 2, 2));
        // Real drafts, including a blank first line followed by text resembling
        // a status bar, are still rejected. Never clear or append to them.
        assert!(!composer_empty(
            "codex",
            &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", ""),
            2,
            2
        ));
        assert!(!composer_empty(
            "codex",
            &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", "a draft"),
            2,
            2
        ));
        assert!(!composer_empty(
            "codex",
            &screen.replace("\n\n  \x1b[38", "\n  draft continuation\n\n  \x1b[38"),
            2,
            2
        ));
        assert!(!composer_empty(
            "codex",
            &screen.replace("?\x1b[0m for shortcuts", "Approve command?"),
            2,
            2
        ));
    }
    #[test]
    fn codex_0167_agents_shortcuts_footer_preserves_the_empty_composer_boundary() {
        let screen = "\n\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  GPT-6.1-Sol default · /fixture\n  ← for agents · ? for shortcuts                  ⚠ 4 warnings · f2 to view";
        assert!(composer_empty("codex", screen, 2, 1));
        for footer in ["← for agents · ? for shortcuts", "← for agents · ? for shortcuts   ⚠ 1 warning · f2 to view", "? for shortcuts"] {
            assert!(composer_empty("codex", &screen.replace("← for agents · ? for shortcuts                  ⚠ 4 warnings · f2 to view", footer), 2, 1));
        }
        for footer in ["← for agents · ? for shortcuts draft", "← for agents · ? for shortcuts ⚠ 4 warnings · f2 to view draft", "← for agents · ? for shortcuts ⚠ 0 warnings · f2 to view", "← for agents · ? for shortcuts ⚠ 4 warnings · enter to approve", "← for agents · ? for shortcuts\n  more input", "← for agents · Approve command?"] {
            assert!(!composer_empty("codex", &screen.replace("← for agents · ? for shortcuts                  ⚠ 4 warnings · f2 to view", footer), 2, 1), "{footer}");
        }
        for draft in ["", "retained draft", "\n  draft continuation"] {
            assert!(!composer_empty("codex", &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", draft), 2, 1));
        }
        assert!(!composer_empty("codex", screen, 3, 1));
    }

    #[test]
    fn codex_warning_footer_does_not_turn_an_empty_queue_composer_into_a_draft() {
        // Codex 0.166's separate warning row can occupy the shortcuts row.
        let screen = "Working\n\n\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  GPT-6.1-Sol default · /fixture\n    ⚠ \x1b[33m2 warnings\x1b[0m · \x1b[1mf2\x1b[0m to view";
        assert!(composer_empty("codex", screen, 2, 2));
        for row in ["⚠ 2 warnings · enter to approve", "⚠ 0 warnings · f2 to view", "⚠ 2 warnings · f2 to view draft", "unknown footer"] {
            assert!(!composer_empty("codex", &screen.replace("    ⚠ \x1b[33m2 warnings\x1b[0m · \x1b[1mf2\x1b[0m to view", row), 2, 2));
        }
        for draft in ["", "my draft", "\n  another draft line"] {
            assert!(!composer_empty("codex", &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", draft), 2, 2));
        }
        assert!(!composer_empty("codex", screen, 3, 2));
    }

    #[test]
    fn codex_ultra_composer_keeps_cursor_draft_and_footer_guards() {
        let screen = "\x1b[1m\x1b[38;2;186;130;255m»\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  GPT-6.1-Sol ultra · /fixture\n  ? for shortcuts";
        assert!(composer_empty("codex", screen, 2, 0));
        assert!(!composer_empty("codex", screen, 3, 0));
        for replacement in ["draft", "\n  multiline draft", ""] {
            assert!(!composer_empty("codex", &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", replacement), 2, 0));
        }
        assert!(!composer_empty("codex", &screen.replace("? for shortcuts", "Approve command?"), 2, 0));
        assert!(!composer_empty("claude", screen, 2, 0));
        let record = json!({"model":"gpt-6.1-sol"});
        assert_eq!(codex_draft(&record, "» retained draft\n\n  GPT-6.1-Sol ultra · /fixture\n  ? for shortcuts", 0).as_deref(), Some("retained draft"));
    }
    #[test]
    fn codex_unshaded_single_line_model_footer() {
        let screen = "Provider error\n\n\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  \x1b[38;2;246;226;183mgpt-5.6-sol default\x1b[2m\x1b[39m · \x1b[0m\x1b[38;2;171;223;167m/private/project\n";
        assert!(composer_empty("codex", screen, 2, 2));
        assert!(composer_empty("codex", &screen.replace("/private/project", "~/project         ⚠ 3 warnings · f2 to view"), 2, 2));
        for replacement in ["", "my draft", "\n  second line"] {
            assert!(!composer_empty("codex", &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", replacement), 2, 2));
        }
        assert!(!composer_empty("codex", &screen.replace("gpt-5.6-sol default", "Approve command?"), 2, 2));
        assert!(!composer_empty("codex", &(screen.to_owned() + "  more input"), 2, 2));
    }
    #[test]
    fn codex_local_model_footer_uses_exact_session_model() {
        // Released Codex 0.162.1 with an Ollama model, dim empty placeholder,
        // unshaded composer and its ordinary model/path footer.
        let screen = "Ready\n\n\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n\n  \x1b[38;2;246;226;183mqwen3-coder:30b default\x1b[39m · \x1b[38;2;171;223;167m/project\x1b[39m\n  \x1b[1m?\x1b[0m for shortcuts";
        let model = Some("qwen3-coder:30b");
        assert!(composer_empty_for_model("codex", screen, 2, 2, model));
        let single = screen.replace("\n  \x1b[1m?\x1b[0m for shortcuts", "\n");
        assert!(composer_empty_for_model("codex", &single, 2, 2, model));
        assert!(!composer_empty_for_model("codex", screen, 2, 2, None));
        assert!(!composer_empty_for_model("codex", screen, 2, 2, Some("another-model")));
        for replacement in ["", "my draft", "\n  qwen3-coder:30b default · /project"] {
            assert!(!composer_empty_for_model("codex", &screen.replace("\x1b[2mAsk Codex to do anything\x1b[0m", replacement), 2, 2, model));
        }
        for footer in ["Approve command?", "qwen3-coder:30b approve", "qwen3-coder:30b default trailing draft"] {
            assert!(!composer_empty_for_model("codex", &screen.replace("qwen3-coder:30b default", footer), 2, 2, model));
        }
        assert!(!composer_empty_for_model("codex", &single.replace("/project", "arbitrary text"), 2, 2, model));
        assert!(!composer_empty_for_model("codex", &(screen.to_owned() + "\n  more input"), 2, 2, model));
        assert!(!composer_empty_for_model("codex", screen, 3, 2, model));
    }
    #[test]
    fn windows_composers_under_conpty() {
        // Spike S3 (bundled ConPTY): Codex 0.162.0 prints `~\…` in its footer;
        // Claude 2.1.296 draws its normal-screen layout. Inbox ConPTY leaves
        // Claude's hidden cursor at the end of the last row, which fails closed.
        let codex = include_str!("../../tests/fixtures/codex-composer-empty-windows.txt");
        assert!(composer_empty("codex", codex, 2, 36));
        let single = codex.replace("\n  \x1b[1m?\x1b[0m for shortcuts", "\n");
        assert_ne!(single, codex);
        let windows_home = crate::platform::home().is_some_and(|home| shown_path::drive(&home));
        assert_eq!(composer_empty("codex", &single, 2, 36), windows_home);
        assert!(!composer_empty(
            "codex",
            &single.replace(r"~\zerus-dev", r"\\server\zerus-dev"),
            2,
            36
        ));
        let claude = include_str!("../../tests/fixtures/claude-composer-empty-windows.txt");
        assert!(composer_empty("claude", claude, 2, 6));
        assert!(!composer_empty("claude", claude, 118, 8));
    }
    #[test]
    fn codex_footer_model_tracks_native_identity_or_launch_model() {
        assert_eq!(codex_footer_model(&json!({"model":"confirmed", "base":["codex","-m","launch"]})), Some("confirmed"));
        assert_eq!(codex_footer_model(&json!({"base":["codex","--oss","--local-provider","ollama","-m","qwen3-coder:30b"]})), Some("qwen3-coder:30b"));
        assert_eq!(codex_footer_model(&json!({"base":["codex","--model=first","--model","last"]})), Some("last"));
        assert_eq!(codex_footer_model(&json!({"base":["codex","--","--model=prompt"]})), None);
        assert_eq!(codex_footer_model(&json!({"base":["codex","--model"]})), None);
    }
    #[test]
    fn malicious_terminal_input_and_attachment_paths_are_rejected() {
        let mut r = Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            text: "hello".into(),
            expected_run_id: "run".into(),
            expected_conversation_id: String::new(),
            expected_compaction_id: String::new(),
            attachments: vec![],
            queue_when_unconfirmed: false,
        };
        assert!(validate(&r).is_ok());
        for text in [
            "hi\x1b[201~", "hello\rbye", " /exit", "/help args", "/help arg/with/slashes",
            "/clear\n/tmp/file", "/", "/ /tmp/file", "!rm test",
        ] {
            r.text = text.into();
            assert!(validate(&r).is_err(), "{text:?}");
        }
        assert_eq!(
            safe_filename("../../bad image.png", 0),
            "01-.._.._bad_image.png"
        );
    }

    #[test]
    fn absolute_paths_are_messages_including_command_named_directories() {
        for text in [
            "/tmp/capture.png What is shown?", " /help/capture.png\nDescribe this.",
            "/tmp/a b.png", "//server/share/file", "C:\\fixture\\capture.png", "The path is /tmp/file",
        ] {
            assert!(validate_launch_prompt(text).is_ok(), "{text:?}");
        }
    }

    // Task 121: a recorded Kimi 2.5.x box (its `│…│` composer) and the older
    // `>` prompt row both read as the draft a delivery left; a dialog does not.
    const KIMI_BOX: &str = include_str!("../../tests/fixtures/kimi-2.5-boxed-paste.txt");
    const KIMI_PLAIN: &str = include_str!("../../tests/fixtures/kimi-2.1-plain-draft.txt");

    #[test]
    fn kimi_composer_drafts_are_read_from_recorded_screens() {
        let rows: Vec<&str> = KIMI_BOX.lines().collect();
        let cursor = rows.iter().position(|row| row.contains("[Zerus team")).unwrap();
        let draft = kimi_draft(KIMI_BOX, cursor).unwrap();
        assert!(draft.starts_with("[Zerus team · from: Zerus · starved] The lead's task got stuck in the"), "{draft}");
        assert!(draft.contains("prompt, and this message waits there until Enter reaches it."), "{draft}");
        // An empty box has nothing to submit.
        let empty = " ╭──────────────────────────────╮\n │ >                            │\n ╰──────────────────────────────╯\n";
        assert!(kimi_draft(empty, 1).is_none(), "{empty}");
        // The plain composer: the prompt row and its continuation line.
        let plain: Vec<&str> = KIMI_PLAIN.lines().collect();
        let plain_cursor = plain.iter().position(|row| row.contains("a typed draft")).unwrap();
        assert_eq!(
            kimi_draft(KIMI_PLAIN, plain_cursor).unwrap(),
            "a typed draft that no event submitted\nwith a continuation line"
        );
        // A folder-trust dialog is no composer.
        assert!(kimi_draft(include_str!("../../tests/fixtures/kimi-folder-trust.txt"), 5).is_none());
    }

    /// zerus-vg4mm: Kimi's startup progress screen draws its own bordered `> `
    /// box that matches the composer. A launch prompt decided from it is sent
    /// into the frame the TUI is already replacing.
    #[test]
    fn kimi_startup_progress_box_is_not_the_prompt() {
        let splash = "\n ⠋ Preparing Kimi Code…\n\n ╭────────────╮\n │ >          │\n ╰────────────╯\n";
        assert!(!composer_empty("kimi", splash, 5, 4));
        // The real capture styles the spinner and the cursor cell.
        let styled = "\n  \x1b[38;2;79;168;255m⠋\x1b[39m Preparing Kimi Code…\n\n ╭────────────╮\n \x1b[38;2;90;90;90m│\x1b[39m > \x1b[7m \x1b[0m          \x1b[38;2;90;90;90m│\x1b[39m\n ╰────────────╯\n";
        assert!(!composer_empty("kimi", styled, 5, 4));
        // The same box is the prompt once the progress screen is gone.
        let settled = "\n\n\n ╭────────────╮\n │ >          │\n ╰────────────╯\n";
        assert!(composer_empty("kimi", settled, 5, 4));
        // A transcript that merely quotes the words is not the progress screen.
        let quoted = " Preparing Kimi Code… was quoted\n\n ╭────────────╮\n │ >          │\n ╰────────────╯\n";
        assert!(composer_empty("kimi", quoted, 5, 3));
        // A transcript row that quotes the spinner banner inside a live session
        // is not the startup frame: content is above it and the status footer
        // is below the composer (zerus-hh0ky).
        let transcript = "\n ╭────────────────────────────╮\n │  Welcome to Kimi Code!     │\n ╰────────────────────────────╯\n   ⠋ Preparing Kimi Code…\n\n ╭────────────╮\n │ >          │\n ╰────────────╯\n Never Ask  fixture  ~/project with spaces\n                          context: 0% (0/32k)\n";
        assert!(composer_empty("kimi", transcript, 5, 7));
        // A session frame that still shows the preparation banner above the
        // composer keeps its status footer, so it is not the startup frame.
        let late_banner = "  ⠋ Preparing Kimi Code…\n\n ╭────────────╮\n │ >          │\n ╰────────────╯\n Never Ask  fixture  ~/project with spaces\n                          context: 0% (0/32k)\n";
        assert!(composer_empty("kimi", late_banner, 5, 3));
    }

    /// zerus-vg4mm: a Kimi decision read must not pair the cursor of one frame
    /// with the screen of another. A real draft and a screen that never settles
    /// are still refused.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn kimi_decision_frame_is_read_until_the_cursor_settles() {
        use std::collections::VecDeque;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Frames {
            cursors: Mutex<VecDeque<(&'static str, &'static str)>>,
            screens: Mutex<VecDeque<&'static str>>,
            reads: AtomicUsize,
            captures: AtomicUsize,
        }
        impl Frames {
            fn new(cursors: Vec<(&'static str, &'static str)>, screens: Vec<&'static str>) -> Self {
                Self {
                    cursors: Mutex::new(VecDeque::from(cursors)),
                    screens: Mutex::new(VecDeque::from(screens)),
                    reads: AtomicUsize::new(0),
                    captures: AtomicUsize::new(0),
                }
            }
            fn counts(&self) -> (usize, usize) {
                (self.reads.load(Ordering::Relaxed), self.captures.load(Ordering::Relaxed))
            }
        }
        impl backend::TerminalBackend for Frames {
            fn input_state(&self, _claim: &backend::InputClaim) -> Result<backend::InputState> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                let (x, y) = self.cursors.lock().unwrap().pop_front().expect("extra cursor read");
                Ok(backend::InputState { cursor_x: x.into(), cursor_y: y.into() })
            }
            fn capture(&self, _pane: &str, _mode: backend::Capture) -> Result<Vec<u8>> {
                self.captures.fetch_add(1, Ordering::Relaxed);
                Ok(self.screens.lock().unwrap().pop_front().expect("extra capture").as_bytes().to_vec())
            }
        }
        let empty = " ╭────────────╮\n │ >          │\n ╰────────────╯";
        let draft = " ╭────────────╮\n │ > hello    │\n ╰────────────╯";
        let record = json!({"name": "kimi/p/initial", "agent": "kimi", "run_id": "run-1", "pane": "%1"});
        // The progress screen's cursor row (4) then the session's (1): the
        // first pair is torn, the next agrees, so the launch proceeds.  The
        // check reads the cursor once before the empty branch (reused as the
        // first attempt's before) and once after each capture: 3 reads, 2
        // captures for one retry (zerus-0vjx5).
        let frames = Frames::new(
            vec![("5", "4"), ("5", "1"), ("5", "1")],
            vec![empty, empty],
        );
        terminal_check_on(&record, true, false, &frames).unwrap();
        assert_eq!(frames.counts(), (3, 2), "the first cursor read is reused");
        // A stable draft still refuses with the draft reason: one cursor read
        // before and one after the capture.
        let frames = Frames::new(vec![("5", "1"), ("5", "1")], vec![draft]);
        assert_eq!(terminal_check_on(&record, true, false, &frames).unwrap_err().to_string(), DRAFT);
        assert_eq!(frames.counts(), (2, 1));
        // A screen that never settles is refused after the bounded reads:
        // one cursor read before and one after each of the five captures.
        let frames = Frames::new(
            vec![("5", "1"), ("5", "4"), ("5", "1"), ("5", "4"), ("5", "1"), ("5", "4")],
            vec![empty; 5],
        );
        let refusal = terminal_check_on(&record, true, false, &frames).unwrap_err().to_string();
        assert!(refusal.contains("redrew"), "{refusal}");
        assert_eq!(frames.counts(), (6, 5));
    }
}

#[cfg(test)]
mod clear_input_tests {
    use super::*;

    /// zerus-wkj9l.4 (M1): the person's Clear input settles exactly the
    /// unconfirmed input typed before it, never a newer one.
    #[test]
    fn a_clear_settles_only_the_input_pending_before_it() {
        let pending = |cleared: Option<f64>| {
            let mut record = json!({"input_pending_at": 10.0, "input_seen_at": 0.0});
            if let Some(at) = cleared {
                record["input_cleared_at"] = json!(at);
            }
            input_unconfirmed(&record)
        };
        assert!(pending(None));
        assert!(!pending(Some(11.0)), "a clear after the input settles it");
        assert!(pending(Some(10.0)), "a clear at the same time does not");
        assert!(pending(Some(9.0)), "an earlier clear never settles a newer input");
    }

    /// zerus-wkj9l.1: only a send whose receipt is still `in_progress` without a
    /// delivery outcome, recent and for this run, holds Clear input back.
    #[test]
    fn only_a_send_still_in_its_proof_window_holds_clear_input_back() {
        let request = "6f9a1b2c-3d4e-4f5a-8b6c-7d8e9f0a1b2c";
        let record = json!({"run_id": "run", "input_pending_at": 100.0, "input_seen_at": 0.0,
            "input_pending_request": request});
        let receipt = |status: &str, delivery: Option<&str>| {
            let mut receipt = json!({"request_id": request, "run_id": "run", "status": status, "submitted_at": 100.0});
            if let Some(delivery) = delivery {
                receipt["delivery"] = json!(delivery);
            }
            receipt
        };
        let at = 105.0;
        let mut asked = Vec::new();
        assert!(in_flight(&record, at, |id| { asked.push(id.to_owned()); Some(receipt("in_progress", None)) }));
        assert_eq!(asked, [request]);
        // Ended uncertain or submitted: the input stays clearable.
        assert!(!in_flight(&record, at, |_| Some(receipt("in_progress", Some("uncertain")))));
        assert!(!in_flight(&record, at, |_| Some(receipt("submitted", None))));
        // Another run's receipt under this ID is not this send.
        assert!(!in_flight(&record, at, |_| Some(json!({"run_id": "other", "status": "in_progress", "submitted_at": 100.0}))));
        // An unreadable receipt within the window holds back, as does a send
        // recorded before the request field; after the limit nothing does.
        assert!(in_flight(&record, at, |_| None));
        let mut older = record.clone();
        older.as_object_mut().unwrap().remove("input_pending_request");
        assert!(in_flight(&older, at, |_| panic!("no receipt is named")));
        let late = 100.0 + PROOF_LIMIT.as_secs_f64() + 1.0;
        assert!(!in_flight(&record, late, |_| Some(receipt("in_progress", None))));
        // A confirmed or cleared input is no longer in flight.
        let mut seen = record.clone();
        seen["input_seen_at"] = json!(101.0);
        assert!(!in_flight(&seen, at, |_| Some(receipt("in_progress", None))));
        let mut cleared = record.clone();
        cleared["input_cleared_at"] = json!(101.0);
        assert!(!in_flight(&cleared, at, |_| Some(receipt("in_progress", None))));
        // A malformed request field never names a path.
        let mut odd = record.clone();
        odd["input_pending_request"] = json!("../../etc/passwd");
        assert!(in_flight(&odd, at, |_| panic!("no path is read for a malformed ID")));
    }

    /// zerus-wkj9l.5 item 2: the ready-send keeper's `mark_input` writes a newer
    /// `input_pending_at` without `input_pending_request`, so an earlier send's
    /// request stays beside it. That earlier receipt, even submitted or ended
    /// uncertain, never settles the newer input's window; only the receipt
    /// whose `submitted_at` is this `input_pending_at` decides.
    #[test]
    fn an_earlier_sends_receipt_never_decides_a_newer_pending_input() {
        let request = "6f9a1b2c-3d4e-4f5a-8b6c-7d8e9f0a1b2c";
        let record = json!({"run_id": "run", "input_pending_at": 200.0, "input_seen_at": 0.0,
            "input_pending_request": request});
        let earlier = |status: &str, delivery: Option<&str>| {
            let mut receipt = json!({"request_id": request, "run_id": "run", "status": status, "submitted_at": 100.0});
            if let Some(delivery) = delivery {
                receipt["delivery"] = json!(delivery);
            }
            receipt
        };
        let at = 205.0;
        assert!(in_flight(&record, at, |_| Some(earlier("submitted", None))));
        assert!(in_flight(&record, at, |_| Some(earlier(
            "in_progress",
            Some("uncertain")
        ))));
        assert!(in_flight(&record, at, |_| Some(
            json!({"run_id": "other", "status": "submitted", "submitted_at": 100.0})
        )));
        // The conservative rule still ends with the window.
        let late = 200.0 + PROOF_LIMIT.as_secs_f64() + 1.0;
        assert!(!in_flight(&record, late, |_| Some(earlier(
            "submitted",
            None
        ))));
        // This input's own receipt still decides.
        let own = json!({"request_id": request, "run_id": "run", "status": "submitted", "submitted_at": 200.0});
        assert!(!in_flight(&record, at, |_| Some(own.clone())));
    }

    /// zerus-wkj9l.5 item 3: the window is on the wall clock. After a backward
    /// step a crashed sender's `in_progress` receipt stays in flight until the
    /// clock passes `input_pending_at` + PROOF_LIMIT again: Clear input is
    /// refused a little longer, never allowed during a send.
    #[test]
    fn a_backward_clock_step_keeps_an_unfinished_send_in_flight_until_the_window_passes_again() {
        let request = "6f9a1b2c-3d4e-4f5a-8b6c-7d8e9f0a1b2c";
        let record = json!({"run_id": "run", "input_pending_at": 1000.0, "input_seen_at": 0.0,
            "input_pending_request": request});
        let crashed = json!({"request_id": request, "run_id": "run", "status": "in_progress", "submitted_at": 1000.0});
        for stepped_back_to in [999.0, 900.0, 1000.0 - 3600.0] {
            assert!(
                in_flight(&record, stepped_back_to, |_| Some(crashed.clone())),
                "{stepped_back_to}"
            );
        }
        assert!(in_flight(
            &record,
            1000.0 + PROOF_LIMIT.as_secs_f64(),
            |_| Some(crashed.clone())
        ));
        assert!(!in_flight(
            &record,
            1000.0 + PROOF_LIMIT.as_secs_f64() + 1.0,
            |_| Some(crashed.clone())
        ));
    }
}
