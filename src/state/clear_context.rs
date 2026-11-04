//! Explicit native /clear, never a prompt or an automatic cache reaction.
//! Codex's reset panel names the session folder as Codex prints it: `/…` or
//! `~/…` on Unix, and on Windows a drive path or `~\…` below the platform home,
//! both compared without case (`shown_path`).
use super::*;
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request { request_id: String, expected_run_id: String, expected_conversation_id: String,
    #[serde(default)] dismiss_prompt: bool }

pub(super) fn observe_start(record: &mut Value, event: &Value) {
    let previous = string(record, "conversation_id").to_owned();
    let next = string(event, "session_id");
    if previous.is_empty() {
        // A fresh start's first conversation draws the same boundary as /clear.
        let earlier = string(&record["fresh_from"], "conversation_id").to_owned();
        if !earlier.is_empty() && earlier != next {
            record["session_clear"] = json!({"at":record["fresh_from"]["at"],"run_id":record["run_id"],
                "conversation_id":next,"previous_conversation_id":earlier,"source":"fresh_start"});
        }
        record.as_object_mut().unwrap().remove("fresh_from");
        return;
    }
    if previous == next { return; }
    let requested = record["clear_context_request"]["conversation_id"] == previous;
    if string(event, "source") == "clear" || requested {
        // The agent starts over, but the local Activity timeline continues:
        // earlier conversations stay readable above the boundary.
        let mut earlier = earlier_conversations(record).as_array().cloned().unwrap_or_default();
        earlier.retain(|id| id != next);
        earlier.push(json!(previous));
        if earlier.len() > CLEARED_CONVERSATIONS { earlier.drain(..earlier.len() - CLEARED_CONVERSATIONS); }
        record["cleared_conversations"] = json!(earlier);
        // A reset observed in Terminal already journaled its boundary.
        let journaled = awaiting_start(record);
        let at = if journaled { record["session_clear"]["at"].clone() } else { json!(now()) };
        record["session_clear"] = json!({"at":at,"run_id":record["run_id"],
            "conversation_id":next,"previous_conversation_id":previous,"source":"native_hook","journaled":journaled});
        // Only the accepted, real clear SessionStart carries this transaction
        // across the active-write/archive-delete crash cut. A reset panel alone
        // never settles an archive restore.
        if string(event,"source") == "clear" && resume_input::pending(record) {
            if let Some(request) = restore_request(record).cloned() {
                record["session_clear"]["restore_archive_id"] = request["restore_archive_id"].clone();
                record["session_clear"]["restore_archive_run_id"] = request["restore_archive_run_id"].clone();
                record["session_clear"]["restore_clear_request_id"] = request["request_id"].clone();
                // Publish the successor and stop exposing the unconfirmed-restore
                // marker in ONE active-record write. Resident pre-fix finalizers
                // only understand that old marker. The original request remains
                // their existing Clear refusal until finalization succeeds.
                record["restore_archive_cleanup"] = json!({"phase":"committed",
                    "run_id":record["run_id"],"conversation_id":next,
                    "previous_conversation_id":previous,"archive_id":request["restore_archive_id"],
                    "archive_run_id":request["restore_archive_run_id"],"request_id":request["request_id"]});
                record["clear_context_request"]["restore_archive_cleanup_pending"] = json!(true);
                record.as_object_mut().unwrap().remove("restore_archive_id");
            }
        }
    } else {
        for key in ["session_clear", "cleared_conversations"] { record.as_object_mut().unwrap().remove(key); }
    }
}

const CLEARED_CONVERSATIONS: usize = 32;

/// The id a SessionStart reports is the conversation Zerus follows (H7): a
/// resume or compaction may continue in a new transcript, so the record takes
/// the agent's id instead of refusing it. Only a clear starts a new Activity
/// timeline; a resume keeps it and notes the change. Returns whether the
/// conversation changed.
pub(super) fn follow(record: &mut Value, event: &Value) -> bool {
    let previous = string(record, "conversation_id").to_owned();
    let next = string(event, "session_id");
    if next.is_empty() || next == previous {
        return false;
    }
    observe_start(record, event);
    let source = string(event, "source");
    if matches!(source, "resume" | "compact") && !previous.is_empty() {
        record["session_rebound"] = json!({"at":now(),"run_id":record["run_id"],
            "previous_conversation_id":previous,"conversation_id":next,"source":source});
    }
    record["conversation_id"] = json!(next);
    true
}

/// A resume or compaction may legitimately open a new conversation (H7).
pub(super) fn resumed_session(event: &Value) -> bool {
    matches!(string(event, "source"), "resume" | "compact")
}

/// The Activity event that records a session following a new conversation
/// after a resume or compaction (H7).
pub(super) fn rebound_event(record: &Value) -> Option<Value> {
    let rebound = &record["session_rebound"];
    if rebound["run_id"] != record["run_id"]
        || rebound["conversation_id"] != record["conversation_id"]
        || rebound["previous_conversation_id"] == rebound["conversation_id"]
        || rebound["at"].as_f64().is_none_or(|at| at <= 0.0)
    {
        return None;
    }
    Some(json!({"type":"SessionRebound","at":rebound["at"],"run_id":record["run_id"],
        "agent_id":"","detail":"","activity_key":format!("rebound:{}:{}:{}",
            string(record,"run_id"),string(record,"conversation_id"),rebound["at"])}))
}

/// `resume --fresh` replaces a stopped binding; its Activity continues like a
/// clear. The new conversation is unknown until the agent's SessionStart.
pub(super) fn carry_fresh_start(previous: &Value, record: &mut Value) {
    let mut earlier = earlier_conversations(previous).as_array().cloned().unwrap_or_default();
    let id = string(previous, "conversation_id");
    if !id.is_empty() {
        earlier.push(json!(id));
        record["fresh_from"] = json!({"conversation_id":id,"at":now()});
    }
    if earlier.len() > CLEARED_CONVERSATIONS { earlier.drain(..earlier.len() - CLEARED_CONVERSATIONS); }
    if !earlier.is_empty() { record["cleared_conversations"] = json!(earlier); }
    if let Some(anchor) = previous.get("journal_name") { record["journal_name"] = anchor.clone(); }
    // A fresh conversation keeps the session's explicit per-launch permission
    // choice; account defaults stay dynamic because they never create this field.
    if let Some(mode) = previous.get("permission_override")
        .filter(|value| value.as_str().is_some_and(|mode| !mode.is_empty()))
    {
        record["permission_override"] = mode.clone();
    }
}

// Earlier conversations of this session's Activity, oldest first. Only
// confirmed clears add to it; another conversation starts a new timeline.
pub(super) fn earlier_conversations(record: &Value) -> Value {
    let current = string(record, "conversation_id");
    // A clear confirmed before this list existed still names its predecessor.
    let ids = record["cleared_conversations"].as_array().cloned().unwrap_or_else(|| {
        let clear = &record["session_clear"];
        if clear["conversation_id"] == current { vec![clear["previous_conversation_id"].clone()] } else { Vec::new() }
    });
    json!(ids.into_iter().filter(|id| id.as_str().is_some_and(|id| !id.is_empty() && id != current)).collect::<Vec<_>>())
}

pub(super) fn awaiting_start(record: &Value) -> bool {
    record["session_clear"]["awaiting_session_start"] == true
        && record["session_clear"]["run_id"] == record["run_id"]
        && record["session_clear"]["conversation_id"] == record["conversation_id"]
}

fn codex_reset_panel(screen: &str, cwd: &str) -> bool {
    codex_reset_panel_below(screen, cwd, &home())
}

// Codex 0.166 rate_limits.rs emits these fixed warnings on a fresh /clear
// surface too. They are display only; the owned process/empty composer checks
// below still decide whether that surface can confirm the original clear.
fn codex_usage_notice(row: &str) -> bool {
    static NOTICE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    NOTICE.get_or_init(|| regex::Regex::new(concat!(
        r"^⚠ (?:Heads up, you have less than (?:5|10|25|50)% of your (?:5h|weekly) limit left\. Run /status for a breakdown\.",
        r"|(?:5h|weekly) limit: (?:[0-9]|[1-9][0-9]|100)% left · resets at (?:[1-9]|1[0-2]):[0-5][0-9] [AP]M",
        r"(?: on (?:[1-9]|[12][0-9]|3[01]) (?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec))? · /status)$"
    )).expect("fixed native usage notice")).is_match(row)
}

// Codex prints the folder below the home as `~/…`, or as `~\…` on Windows,
// where its drive path and that form compare without case (`shown_path`).
fn codex_reset_panel_below(screen: &str, cwd: &str, home: &Path) -> bool {
    let windows = shown_path::drive(cwd);
    if !(windows || Path::new(cwd).is_absolute()) { return false; }
    // Codex keeps its header above the first history prompt until the screen
    // is cleared and draws each history prompt as a `›` row above the live
    // composer, so the header alone is no reset. The empty state has exactly
    // one `›` row, the composer at the left edge, with only the header block
    // above it; the composer check then needs the cursor there and no draft.
    let lines: Vec<_> = screen.lines().collect();
    let mut prompts = lines.iter().enumerate().filter(|(_, line)| input::codex_prompt(line.trim_start()).is_some());
    let (Some((composer, input)), None) = (prompts.next(), prompts.next()) else { return false; };
    // Its text is the dim placeholder, one of the two that both reviewed
    // releases draw, or nothing: a draft fails here before the composer check.
    let placeholder = |text: &str| text.is_empty() || text.strip_prefix(' ')
        .is_some_and(|text| ["Ask Codex to do anything", "Ask a follow-up question"].contains(&text));
    if !input::codex_prompt(input).map(str::trim_end).is_some_and(placeholder) { return false; }
    let mut rows: Vec<_> = lines[..composer].iter().map(|line| line.trim())
        .filter(|line| !line.is_empty() && !codex_usage_notice(line)).collect();
    // The released idle screen can include its settled decorative logo. Match
    // the complete known pose; arbitrary braille, prose and partial frames do
    // not confirm a reset. A replay remains unconfirmed until it settles.
    let logo: Vec<_> = include_str!("data/codex-empty-state-60x21.txt").lines()
        .filter(|line| !line.starts_with('#')).collect();
    if rows.ends_with(&logo) { rows.truncate(rows.len() - logo.len()); }
    if !(2..=6).contains(&rows.len()) || !rows[0].starts_with(">_ OpenAI Codex (v")
        || !rows[0].ends_with(')') { return false; }
    let short = if windows { home.to_str().and_then(|home| shown_path::home_alias(cwd, home)) } else {
        Path::new(cwd).strip_prefix(home).ok().map(|path| {
            if path.as_os_str().is_empty() { "~".to_owned() } else { format!("~/{}",path.display()) }
        })
    };
    if !shown_path::equal(rows[1], cwd, windows)
        && short.is_none_or(|short| !shown_path::equal(rows[1], &short, windows)) { return false; }
    let tail = if rows.get(2).is_some_and(|row| row.starts_with("permissions: ")) {
        &rows[3..]
    } else { &rows[2..] };
    // Resident Codex 0.160.1 keeps one randomly chosen empty-state greeting.
    // Match the released finite vocabulary, including terminal line wrapping;
    // arbitrary assistant output and unknown startup panels still fail closed.
    let greeting = tail.join(" ");
    tail.is_empty() || include_str!("data/codex-0.160.1-greetings.txt").lines()
        .filter(|line| !line.starts_with('#')).any(|line| line == greeting)
}

// Codex defers its clear SessionStart hook until the next user turn and does
// not materialize the new rollout yet. Observe only the complete reset panel
// in the exact live empty composer; keep the native ID until its real hook.
pub(super) fn observe_terminal(record: &mut Value) -> Result<bool> {
    if cleanup_pending(record) { return Ok(false); }
    // A resume awaiting its deferred SessionStart has no hook-confirmed idle
    // state: its exact process must still own the restored conversation.
    let resumed = resume_clearable(record);
    if !string(record,"restore_archive_id").is_empty() && restore_request(record).is_none() {
        return Ok(false);
    }
    // The failed-turn path deliberately preserves the missing idle hook. A
    // matching explicit request may confirm its native reset through the same
    // full panel/composer checks, without rewriting the session's work state.
    let requested_failure = record["clear_context_request"].is_object()
        && record["clear_context_request"]["conversation_id"] == record["conversation_id"]
        && failed_turn(record);
    if string(record,"agent") != "codex" || awaiting_start(record)
        || (!resumed && !requested_failure && (record["activity"] != "idle" || record["phase"] != "idle"))
        || string(record,"conversation_id").is_empty()
        || (string(record,"prompt").is_empty() && string(record,"last_message").is_empty()
            && !record["clear_context_request"].is_object()) { return Ok(false); }
    // Ordinary idle polls only need one read-only capture. Perform the more
    // expensive process and composer checks only for a matching reset panel.
    if !codex_reset_panel(&kimi_tui_choice::capture(record)?,string(record,"cwd")) { return Ok(false); }
    if resumed { resume_input::owned(record)?; }
    if requested_failure && !failed_turn(record) { return Err("Native work changed before clear confirmation".into()); }
    kimi_tui_choice::identity(record)?;
    input::checked_terminal(record,true)?;
    if !codex_reset_panel(&kimi_tui_choice::capture(record)?,string(record,"cwd")) { return Ok(false); }
    kimi_tui_choice::identity(record)?;
    record["session_clear"] = json!({"at":now(),"run_id":record["run_id"],
        "conversation_id":record["conversation_id"],"previous_conversation_id":record["conversation_id"],
        "source":"native_terminal","awaiting_session_start":true});
    for key in ["prompt","last_message","last_error"] { record.as_object_mut().unwrap().remove(key); }
    if let Some(mut clear) = event(record) {
        clear["hook_event_name"] = json!("SessionCleared");
        journal::log_event(record,&clear)?;
    }
    write(record)?;
    Ok(true)
}

fn matching_resume_request(record: &Value) -> bool {
    let request = &record["clear_context_request"];
    uuid::Uuid::parse_str(string(request, "request_id")).is_ok()
        && request["conversation_id"] == record["conversation_id"]
        && request["at"].as_f64().is_some_and(|at| at > 0.0)
        && (request.get("run_id").is_none() || request["run_id"] == record["run_id"])
}

/// Capture only the real main SessionEnd of an already requested, verified
/// pending resume. A generic end cannot reopen any input or clear path.
pub(super) fn ending_resume(record: &Value) -> bool {
    resume_clearable(record) && matching_resume_request(record)
}

/// SessionEnd can precede Codex's deferred clear SessionStart. Only its exact
/// hook-time request/run/conversation marker preserves that pending resume;
/// any newer native event, work or changed binding keeps the ordinary refusal.
/// The first submitted input still awaits that real start, as on any resume.
pub(super) fn ended_resume(record: &Value) -> bool {
    let end = &record["session_end"];
    ended_request(record)
        && end["clear_request_id"] == record["clear_context_request"]["request_id"]
        && end["clear_event_at"].as_f64().is_some_and(|at| at > 0.0)
        && end["clear_event_at"] == record["last_event_at"]
}

fn ended_request(record: &Value) -> bool {
    let end = &record["session_end"];
    matching_resume_request(record)
        && !string(record, "run_id").is_empty()
        && end["run_id"] == record["run_id"]
        && end["conversation_id"] == record["conversation_id"]
        && end["at"].as_f64().is_some_and(|at| at >= record["clear_context_request"]["at"].as_f64().unwrap_or(f64::INFINITY))
        && !telemetry::work_remains(record)
}

pub(super) fn recovering_resume(record: &Value) -> bool {
    if string(record, "error") != codex_clear_recovery::MISMATCH
        || !string(record, "restore_archive_id").is_empty() || cleanup_pending(record) || pause_active(record) {
        return false;
    }
    let mut original = record.clone();
    original.as_object_mut().unwrap().remove("error");
    let end = &original["session_end"];
    let marked = end.get("clear_request_id").is_some() || end.get("clear_event_at").is_some();
    resume_input::pending_binding(&original) && ended_request(&original)
        && (!marked || ended_resume(&original))
}

pub(super) fn event(record: &Value) -> Option<Value> {
    let clear = &record["session_clear"];
    if clear["run_id"] != record["run_id"] || clear["conversation_id"] != record["conversation_id"]
        || clear["at"].as_f64().is_none_or(|at| at <= 0.0) { return None; }
    Some(json!({"type":"SessionCleared","at":clear["at"],"run_id":record["run_id"],
        "agent_id":"","detail":"","activity_key":format!("clear:{}:{}:{}",
            string(record,"run_id"),string(record,"conversation_id"),clear["at"])}))
}

fn unblocked(record: &Value) -> bool {
    blocked(record).is_none()
}
fn blocked(record: &Value) -> Option<String> {
    let error = string(record,"error");
    Some(if pause_active(record) { "Wait for the session to finish pausing".into() }
    else if !error.is_empty() { format!("The session reported an error: {}", journal::clipped(&json!(error),240)) }
    else if cleanup_pending(record) || (!string(record,"restore_archive_id").is_empty() && string(record,"expected_id").is_empty()) {
        "Wait for the restored archive cleanup before clearing again".into()
    }
    // A confirmed Codex clear keeps its request until the deferred SessionStart.
    else if awaiting_start(record) { "The cleared conversation starts with the next message".into() }
    else if record["clear_context_request"].is_object() {
        "The requested context clear is not confirmed yet. Check Terminal".into()
    } else if record["compact_context_request"]["status"] == "uncertain" {
        "The last compaction was not confirmed. Check Terminal".into()
    } else if compact_context::pending(record) { "Wait for compaction to finish".into() }
    else { return None; })
}
pub(super) fn available(record: &Value, live: bool) -> bool {
    unavailable(record, live).is_none()
}
/// Why native clear and compaction are unavailable, in the person's words;
/// `None` exactly when `available`. A busy session names the work that keeps
/// it from Ready, as the session tree lists it: the main agent's turn and
/// tools, and each working subagent with its current tool.
pub(super) fn unavailable(record: &Value, live: bool) -> Option<String> {
    if !live { return Some("The agent is not running".into()); }
    if !AGENTS.contains(&string(record,"agent")) { return Some("This provider does not support native context actions".into()); }
    if string(record,"conversation_id").is_empty() { return Some("Wait for a confirmed conversation".into()); }
    if let Some(reason) = blocked(record) { return Some(reason); }
    if !string(record,"expected_id").is_empty() { return Some("The agent has not confirmed the resumed conversation yet".into()); }
    // Kimi profile groups can outlast the main turn's Stop, which records the
    // session idle; like the listing, wait for them and other remaining work.
    if telemetry::settled(record) { return None; }
    Some(waiting(record, string(record,"phase")))
}
/// `unavailable` as an inspection reports it. A native chooser recognized
/// only in the terminal, such as Kimi's context choice, leaves the saved
/// activity idle; the summary shows it as a question or an approval, and
/// dispatch refuses it (`input::checked_ready`), so both actions wait for it.
pub(super) fn unavailable_shown(record: &Value, live: bool, shown: &Value) -> Option<String> {
    unavailable(record, live).or_else(|| answer_first(string(shown,"phase")).map(Into::into))
}
/// What an idle-only action waits for while the session shows a question or
/// an approval, or `None` for any other phase. Clear, compaction, forks and
/// effort changes share it for a chooser that only the terminal shows.
pub(super) fn answer_first(phase: &str) -> Option<&'static str> {
    match phase {
        "input" => Some("Wait until the agent's question is answered"),
        "approval" => Some("Wait until the agent's approval request is answered"),
        _ => None,
    }
}
fn waiting(record: &Value, phase: &str) -> String {
    if let Some(reason) = answer_first(phase) { return reason.into(); }
    match phase {
        "compacting" => "Wait for compaction to finish".into(),
        phase => {
            let work = working(record, ["working","tool"].contains(&phase));
            if work.is_empty() { "Wait until the agent reports Ready".into() }
            else { format!("Wait until the agent reports Ready. Still working: {work}") }
        }
    }
}
/// The work recorded as running, at most three items and a count of the rest.
/// The main agent counts only while its own turn runs: an interrupted or
/// failed turn has ended, and its state awaits the agent's next report.
/// Children are named as the listing names them, Kimi profiles by group.
fn working(record: &Value, main_turn: bool) -> String {
    let mut items = Vec::new();
    if main_turn && record["main_done"] != true {
        let mut tools: Vec<_> = record["active_tools"].as_object().into_iter().flatten().map(|(_, tool)| tool).collect();
        tools.sort_by(|a,b| a["started"].as_f64().unwrap_or(0.0).total_cmp(&b["started"].as_f64().unwrap_or(0.0)));
        let mut names: Vec<String> = Vec::new();
        for tool in tools {
            let name = journal::clipped(&tool["name"],60);
            if !name.is_empty() && !names.contains(&name) { names.push(name); }
        }
        items.push(match names.as_slice() {
            [] => "main agent".to_owned(),
            [only] => format!("main agent running {only}"),
            [first, second] => format!("main agent running {first} and {second}"),
            [first, rest @ ..] => format!("main agent running {first} and {} other tools", rest.len()),
        });
    }
    let mut children: Vec<_> = record["subagents"].as_object().into_iter().flatten()
        .filter(|(_, child)| telemetry::child_working(record, child) && telemetry::listed(record, child)).collect();
    children.sort_by(|(a_id,a),(b_id,b)| b["updated"].as_f64().unwrap_or(0.0)
        .total_cmp(&a["updated"].as_f64().unwrap_or(0.0)).then_with(|| a_id.cmp(b_id)));
    for (id, child) in children {
        let name = journal::clipped(&child["name"],60);
        let label = if name.is_empty() { journal::clipped(&json!(id),60) } else { name };
        let tool = journal::clipped(&child["current_tool"],60);
        items.push(if tool.is_empty() { format!("subagent “{label}”") } else { format!("subagent “{label}” running {tool}") });
    }
    if telemetry::grouped_active(record) > 0 {
        for (profile, group) in record["subagent_groups"].as_object().into_iter().flatten() {
            let count = group["active_count"].as_u64().unwrap_or(0);
            let profile = journal::clipped(&json!(profile),60);
            match count {
                0 => {}
                1 => items.push(format!("1 “{profile}” subagent")),
                _ => items.push(format!("{count} “{profile}” subagents")),
            }
        }
    }
    if items.len() > 3 {
        let rest = items.len() - 3;
        items.truncate(3);
        items.push(format!("and {rest} more"));
    }
    items.join(", ")
}
/// Codex defers SessionStart after `hgs resume` until the next turn. A verified
/// resume may clear before it: the clear's own SessionStart then names the new
/// conversation (`resumed_start`). Compaction keeps requiring `available`.
pub(super) fn resumed(record: &Value, live: bool) -> bool {
    live && unblocked(record) && resume_clearable(record) && resume_input::ready(record).is_ok()
}
/// `resumed` for an inspection whose summary already verified the resume.
pub(super) fn resumed_verified(record: &Value, live: bool, verified: bool) -> bool {
    live && unblocked(record) && verified && resume_clearable(record)
}

/// Clear alone may reset an exactly verified archive resume. Keep the shared
/// pause/send/settings predicates unchanged, including their restore fence.
pub(super) fn resume_clearable(record: &Value) -> bool {
    resume_input::pending(record)
        && record["input_pending_at"].as_f64().unwrap_or(0.0) == 0.0
        && !telemetry::work_remains(record)
        && (string(record, "restore_archive_id").is_empty()
            || archive::restore_source(record).is_ok())
}

fn restore_request(record: &Value) -> Option<&Value> {
    let request = &record["clear_context_request"];
    (!string(record, "restore_archive_id").is_empty()
        && request["restore_archive_id"] == record["restore_archive_id"]
        && request["run_id"] == record["run_id"]
        && !string(record, "run_id").is_empty()
        && request["conversation_id"] == record["conversation_id"]
        && !string(request, "restore_archive_run_id").is_empty()
        && uuid::Uuid::parse_str(string(request, "request_id")).is_ok())
    .then_some(request)
}

/// Presence is a refusal fence even if either half was lost or malformed.
/// No one-sided marker supplies authority to delete an archive or start over.
pub(super) fn cleanup_pending(record: &Value) -> bool {
    record.get("restore_archive_cleanup").is_some()
        || record.get("restore_archive_clear_source").is_some()
        || record["clear_context_request"].get("restore_archive_cleanup_pending").is_some()
}

/// The accepted native hook retains its exact source and explicit request.
/// Legacy 055 records remain readable; they are not newly written in that form.
pub(super) fn restore_committed(record: &Value) -> bool {
    let clear = &record["session_clear"];
    let boundary = record["agent"] == "codex"
        && string(record, "expected_id").is_empty()
        && clear["at"].as_f64().is_some_and(|at| at > 0.0)
        && clear["awaiting_session_start"] != true
        && clear["source"] == "native_hook"
        && clear["run_id"] == record["run_id"]
        && !string(record, "run_id").is_empty()
        && !string(record, "conversation_id").is_empty()
        && clear["conversation_id"] == record["conversation_id"]
        && !string(clear, "previous_conversation_id").is_empty()
        && clear["previous_conversation_id"] != record["conversation_id"]
        && !string(clear, "restore_archive_run_id").is_empty()
        && uuid::Uuid::parse_str(string(clear, "restore_clear_request_id")).is_ok();
    if !boundary { return false; }
    if !cleanup_pending(record) {
        return !string(record, "restore_archive_id").is_empty()
            && clear["restore_archive_id"] == record["restore_archive_id"];
    }
    let anchor = &record["restore_archive_cleanup"];
    let request = &record["clear_context_request"];
    string(record, "restore_archive_id").is_empty()
        && anchor["phase"] == "committed"
        && anchor["run_id"] == record["run_id"]
        && anchor["conversation_id"] == record["conversation_id"]
        && anchor["previous_conversation_id"] == clear["previous_conversation_id"]
        && uuid::Uuid::parse_str(string(anchor, "archive_id")).is_ok()
        && anchor["archive_id"] == clear["restore_archive_id"]
        && anchor["archive_run_id"] == clear["restore_archive_run_id"]
        && anchor["request_id"] == clear["restore_clear_request_id"]
        && request["restore_archive_cleanup_pending"] == true
        && request["request_id"] == anchor["request_id"]
        && request["run_id"] == anchor["run_id"]
        && request["conversation_id"] == anchor["previous_conversation_id"]
        && request["restore_archive_id"] == anchor["archive_id"]
        && request["restore_archive_run_id"] == anchor["archive_run_id"]
}
pub(super) fn restored_successor(record: &Value, archived: &Value) -> bool {
    let clear = &record["session_clear"];
    restore_committed(record)
        && archived["agent"] == record["agent"]
        && clear["previous_conversation_id"] == archived["conversation_id"]
        && clear["restore_archive_id"] == archived["archive_id"]
        && clear["restore_archive_run_id"] == archived["run_id"]
}
/// Claude sends no hook when Esc stops a turn, so the session stays
/// Interrupted until its next message while its prompt already takes /clear,
/// whose SessionStart reports Ready. The clear needs no remaining work and the
/// empty prompt verified as for that message (`input::checked_interrupted`).
/// Compaction keeps requiring `available`.
pub(super) fn interrupted(record: &Value, live: bool) -> bool {
    live && unblocked(record) && !string(record,"conversation_id").is_empty()
        && string(record,"expected_id").is_empty() && stopped(record)
}
/// The turn state `interrupted` needs. Dispatch rechecks it before Enter, as
/// `telemetry::settled` for Ready, once its own clear request is recorded.
pub(super) fn stopped(record: &Value) -> bool {
    record["agent"] == "claude" && record["phase"] == "interrupted" && !telemetry::work_remains(record)
}

/// A typed Codex task completion may precede its missing idle hook. Only an
/// explicit clear uses this path; provider attention and other idle actions
/// retain their existing state. Confirmed tools, children and native jobs win.
pub(super) fn failed_turn(record: &Value) -> bool {
    record["agent"] == "codex" && !string(record,"conversation_id").is_empty()
        && string(record,"expected_id").is_empty() && !telemetry::work_remains(record)
        && provider_errors::completed_failure(record)
        && processes::WorkSample::default().summary(record,true)["count"] == 0
}
pub(super) fn failed(record: &Value, live: bool) -> bool {
    live && unblocked(record) && failed_turn(record) && input::checked_terminal(record,true).is_ok()
}
/// Codex reports a clear of a resumed conversation only with the SessionStart
/// it defers to the next turn, under the new conversation's ID. That clear
/// confirms the resume when it was requested or observed for the expected
/// conversation, or the exact supervised Codex still holds its rollout open.
pub(super) fn resumed_start(record: &Value, event: &Value) -> bool {
    // The trusted real hook can settle an old binary's pending clear too. This
    // fallback is never used by readiness/input or terminal-only confirmation.
    let pending = resume_input::pending(record)
        || (resume_input::pending_binding(record) && ended_request(record));
    if !string(record,"restore_archive_id").is_empty() {
        return string(event,"source") == "clear" && resume_input::pending(record)
            && restore_request(record).is_some_and(|request| archive::restore_source(record)
                .is_ok_and(|archived| archived["run_id"] == request["restore_archive_run_id"]));
    }
    string(event,"source") == "clear"
        && pending
        && (awaiting_start(record) || record["clear_context_request"]["conversation_id"] == record["conversation_id"]
            || resume_input::owned(record).is_ok())
}
/// Claude waits in a chooser that hides its composer. Only an explicit request
/// dismisses it, with one Esc that answers it as in Terminal.
pub(super) fn dismissable(record: &Value, live: bool) -> bool {
    live && record["agent"]=="claude"
        && !string(record,"conversation_id").is_empty()
        && ["input","approval"].contains(&string(record,"phase"))
        && !pause_active(record) && string(record,"error").is_empty()
        && string(record,"expected_id").is_empty()
        && !record["clear_context_request"].is_object()
        && !awaiting_start(record)
        && !compact_context::pending(record)
}
const STARTUP_CHOICES: [&str; 4] = ["claude_resume", "claude_permission_mode", "claude_folder_trust", "claude_usage_limit"];
/// Esc can end a launch at a startup chooser; Activity answers those instead.
pub(super) fn startup_choice(cards: &Value) -> bool {
    cards.as_array().into_iter().flatten().any(|card| STARTUP_CHOICES.contains(&string(card,"source")))
}
fn checked_chooser(name: &str, request: &Request) -> Result<Value> {
    let record=read(name)?;
    if string(&record,"run_id")!=request.expected_run_id || string(&record,"conversation_id")!=request.expected_conversation_id {
        return Err("session identity changed; refresh before clearing".into());
    }
    if !matches(&record,live()?.get(name)) || !process_alive(&record) {
        return Err("the exact agent process is no longer running; resume it explicitly".into());
    }
    if !dismissable(&record,true) {return Err("Claude is no longer waiting in a chooser; refresh before clearing".into());}
    if claude_resume::current(&record).is_some() || claude_permission::current(&record).is_some() || claude_trust::current(&record).is_some() || claude_usage_limit::current(&record).is_some() {
        return Err("Answer Claude's startup choice in Activity or Terminal before clearing".into());
    }
    input::checked_terminal(&record,false)?;
    if !claude_approval::chooser_visible(&kimi_tui_choice::capture(&record)?) {
        return Err("Claude's chooser is no longer visible; refresh before clearing".into());
    }
    Ok(record)
}
fn command_row(agent: &str, row: &str, x: usize, command: &str) -> bool {
    let before=row.chars().take(x).collect::<String>();
    let after=row.chars().skip(x).collect::<String>();
    match agent {
        "codex" => input::codex_prompt(before.trim()).is_some_and(|text|text.trim()==command) && after.trim().is_empty(),
        "claude" => [format!("❯ {command}"),format!("> {command}")].iter().any(|prompt|prompt==before.replace('\u{a0}'," ").trim()) && after.trim().is_empty(),
        // Kimi 2.1 shows an argument hint such as <instruction> after the command.
        "kimi" => before.trim()==format!("│ > {command}") && after.trim().strip_suffix('│').is_some_and(|hint|{
            let hint=hint.trim();
            hint.is_empty() || (hint.len()<=40 && hint.starts_with('<') && hint.ends_with('>')
                && !hint[1..hint.len()-1].contains(['<','>']))
        }),
        _ => false,
    }
}
/// Kimi 2.1 turns an Enter that follows typed text within about 0.1 s into a
/// newline. Wait this long after a typed command, then recheck it and press Enter.
pub(super) const COMMAND_SETTLE: Duration = Duration::from_millis(250);
pub(super) fn ready(record: &Value, command: &str) -> Result<bool> {
    let cursor=backend::current().pane_info(backend::Target::Pane(string(record,"pane")),
        &[backend::PaneField::CursorX,backend::PaneField::CursorY])?;
    let Some((x,y))=cursor.pair() else {return Ok(false)};
    let (Ok(x),Ok(y))=(x.parse::<usize>(),y.parse::<usize>()) else {return Ok(false)};
    let screen=kimi_tui_choice::capture(record)?;
    Ok(screen.lines().nth(y).is_some_and(|row|command_row(string(record,"agent"),row,x,command)))
}
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len()!=2 || args[1]!="--json" {return Err("usage: hgs clear-context <session> --json".into());}
    let mut bytes=Vec::new();io::stdin().take(8193).read_to_end(&mut bytes).map_err(|e|e.to_string())?;
    if bytes.len()>8192 {return Err("Clear request is too large".into());}
    let request:Request=serde_json::from_slice(crate::state::without_bom(&bytes)).map_err(|e|e.to_string())?;
    let uuid=uuid::Uuid::parse_str(&request.request_id).map_err(|_|"Invalid request identity")?;
    let name=&args[0];
    if opencode::exists(name) {return Err("OpenCode context reset is not supported by this adapter".into());}
    if dsh::exists(name) {return Err("This DeepSeek harness does not expose a native context reset. Use its native UI.".into());}
    let mut guard=Some(lock(None)?);
    let current=read(name)?;
    // A resume before its deferred SessionStart is verified as for its first message.
    let resuming=resume_clearable(&current);
    let stopped=!resuming && interrupted(&current,true);
    let failed=!resuming && !stopped && self::failed(&current,true);
    let dismiss=request.dismiss_prompt && !resuming && !stopped && !failed && !available(&current,true);
    let mut record=if dismiss {checked_chooser(name,&request)?} else {
        let record=if resuming {input::checked_resumed(name,&request.expected_run_id,&request.expected_conversation_id)?}
            else if stopped {input::checked_interrupted(name,&request.expected_run_id,&request.expected_conversation_id)?}
            else if failed {input::checked_failed(name,&request.expected_run_id,&request.expected_conversation_id)?}
            else {input::checked_ready(name,&request.expected_run_id,&request.expected_conversation_id)?};
        if !(available(&record,true) || resuming && resumed(&record,true) || stopped && interrupted(&record,true)
            || failed && self::failed(&record,true)) {
            return Err("Wait for the agent to become idle before clearing context".into());
        }
        input::checked_terminal(&record,true)?;
        record
    };
    if record["pending_settings"].is_object() {return Err("Wait for the pending model settings before clearing context".into());}
    let path=absolute_root()?.join("clear_receipts").join(format!("{uuid}.json"));
    if path.exists() {return Err("This clear was already attempted. Check Terminal before trying again".into());}
    recovery::cancel_for_input(name)?;
    record["clear_context_request"]=json!({"request_id":request.request_id,"run_id":request.expected_run_id,
        "conversation_id":request.expected_conversation_id,"at":now()});
    if resuming && !string(&record,"restore_archive_id").is_empty() {
        let archived = archive::restore_source(&record)?;
        record["clear_context_request"]["run_id"] = record["run_id"].clone();
        record["clear_context_request"]["restore_archive_id"] = record["restore_archive_id"].clone();
        record["clear_context_request"]["restore_archive_run_id"] = archived["run_id"].clone();
    }
    write(&mut record)?;
    atomic(&path,&json!({"status":"in_progress","run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id}).to_string())?;
    // No Ctrl-U: never discard a native draft. Escape only on explicit request.
    let result=(|| {
        if dismiss {
            // One Esc answers the chooser as in Terminal: an approval is denied,
            // a question stays unanswered. It is never repeated.
            backend::current().send(string(&record,"pane"),&[backend::Key::Named("Escape")])?;
            guard=None; // Hooks reporting the dismissal must be able to commit.
            let deadline=Instant::now()+Duration::from_secs(3);
            loop {
                let current=kimi_tui_choice::identity(&record)?;
                if !claude_approval::chooser_visible(&kimi_tui_choice::capture(&current)?)
                    && input::checked_terminal(&current,true).is_ok() {break;}
                if Instant::now()>=deadline {return Err("Claude's chooser did not close after Esc".into());}
                std::thread::sleep(Duration::from_millis(40));
            }
            guard=Some(lock(None)?);
            input::checked_terminal(&kimi_tui_choice::identity(&record)?,true)?;
        }
        backend::current().send(string(&record,"pane"),&[backend::Key::Literal("/clear")])?;
        let deadline=Instant::now()+Duration::from_secs(3);
        loop {
            let current=kimi_tui_choice::identity(&record)?;
            if ready(&current,"/clear")? {break;}
            if Instant::now()>=deadline {return Err("Native /clear input could not be verified".into());}
            std::thread::sleep(Duration::from_millis(40));
        }
        std::thread::sleep(COMMAND_SETTLE);
        let current=if dismiss {kimi_tui_choice::identity(&record)?}
            else if resuming {
                resume_input::owned(&record)?;
                let current=kimi_tui_choice::identity(&record)?;
                if !resume_clearable(&current) {return Err("The resumed clear target changed before /clear".into());}
                if let Some(request)=restore_request(&record) {
                    let archived=archive::restore_source(&current)?;
                    if archived["run_id"] != request["restore_archive_run_id"] {
                        return Err("The restored archive changed before /clear".into());
                    }
                }
                current
            }
            else if stopped {input::checked_interrupted(name,&request.expected_run_id,&request.expected_conversation_id)?}
            else if failed {input::checked_failed(name,&request.expected_run_id,&request.expected_conversation_id)?}
            else {input::checked_ready(name,&request.expected_run_id,&request.expected_conversation_id)?};
        kimi_tui_choice::identity(&record)?;
        input::checked_terminal(&current,false)?;
        if !ready(&current,"/clear")? {return Err("The native input changed before /clear".into());}
        backend::current().send(string(&current,"pane"),&[backend::Key::Named("Enter")])?;
        Ok(())
    })();
    drop(guard); // SessionStart hooks must be able to commit the new identity.
    result.map_err(|e:String|format!("Clear delivery uncertain: {e}. Check Terminal before retrying"))?;
    let deadline=Instant::now()+Duration::from_secs(4);
    let mut status="submitted";
    loop {
        let mut current=read(name)?;
        if current["run_id"]!=record["run_id"] || current["pid"]!=record["pid"] {break;}
        if !string(&current,"conversation_id").is_empty() && current["conversation_id"]!=record["conversation_id"] {
            status="confirmed";break;
        }
        {
            let _guard=lock(None)?;
            current=read(name)?;
            if current["run_id"]==record["run_id"] && current["pid"]==record["pid"]
                && (observe_terminal(&mut current).unwrap_or(false) || awaiting_start(&current)) {
                status="confirmed";break;
            }
        }
        if Instant::now()>=deadline {break;}
        std::thread::sleep(Duration::from_millis(80));
    }
    let receipt=json!({"request_id":request.request_id,"name":name,"run_id":request.expected_run_id,
        "conversation_id":request.expected_conversation_id,"status":status});
    atomic(&path,&receipt.to_string())?;println!("{receipt}");Ok(0)
}
#[cfg(test)] mod tests {
    use super::*;

    #[test]
    fn a_resumed_session_follows_its_new_transcript() {
        // H7: the hook reports the new conversation; the record follows it and
        // keeps the Activity timeline (no clear boundary).
        let mut record = json!({"run_id":"r","conversation_id":"old","agent":"claude"});
        assert!(follow(&mut record, &json!({"session_id":"new","source":"resume"})));
        assert_eq!(record["conversation_id"], "new");
        let rebound = &record["session_rebound"];
        assert_eq!(
            (rebound["previous_conversation_id"].as_str(), rebound["conversation_id"].as_str()),
            (Some("old"), Some("new"))
        );
        assert_eq!(rebound["source"], "resume");
        assert!(record["session_clear"].is_null());
        assert!(resumed_session(&json!({"source":"resume"})));
        assert!(rebound_event(&record).is_some());
        // Once bound, the same start changes nothing.
        assert!(!follow(&mut record, &json!({"session_id":"new","source":"resume"})));
    }

    #[test]
    fn a_compaction_follows_the_new_transcript_too() {
        let mut record = json!({"run_id":"r","conversation_id":"old"});
        assert!(follow(&mut record, &json!({"session_id":"after","source":"compact"})));
        assert_eq!(record["conversation_id"], "after");
        assert_eq!(record["session_rebound"]["source"], "compact");
        assert!(resumed_session(&json!({"source":"compact"})));
        assert!(rebound_event(&record).is_some());
    }

    #[test]
    fn a_clear_still_starts_a_new_timeline() {
        let mut record = json!({"run_id":"r","conversation_id":"old"});
        assert!(follow(&mut record, &json!({"session_id":"new","source":"clear"})));
        assert_eq!(record["session_clear"]["previous_conversation_id"], "old");
        assert!(record["session_rebound"].is_null());
        assert!(!resumed_session(&json!({"source":"clear"})));
    }

    #[test] fn unavailable_context_actions_name_the_blocking_condition_and_work() {
        let idle=json!({"agent":"claude","run_id":"run","conversation_id":"conversation","activity":"idle","phase":"idle",
            "main_done":true,"active_tools":{},"subagents":{}});
        let check=|record:&Value,live:bool,expected:Option<&str>|{
            let reason=unavailable(record,live);
            assert_eq!(reason.as_deref(),expected,"{record}");
            assert_eq!(available(record,live),reason.is_none(),"{record}");
        };
        check(&idle,true,None);
        check(&idle,false,Some("The agent is not running"));
        let changed=|key:&str,value:Value|{let mut record=idle.clone();record[key]=value;record};
        check(&changed("agent",json!("opencode")),true,Some("This provider does not support native context actions"));
        check(&changed("conversation_id",json!("")),true,Some("Wait for a confirmed conversation"));
        let pid=std::process::id();
        check(&changed("pausing",json!({"pid":pid,"start":process_start(pid)})),true,Some("Wait for the session to finish pausing"));
        check(&changed("error",json!("agent resumed a different conversation")),true,
            Some("The session reported an error: agent resumed a different conversation"));
        check(&changed("clear_context_request",json!({"request_id":"r","conversation_id":"conversation"})),true,
            Some("The requested context clear is not confirmed yet. Check Terminal"));
        let awaiting=changed("session_clear",json!({"run_id":"run","conversation_id":"conversation","awaiting_session_start":true}));
        check(&awaiting,true,Some("The cleared conversation starts with the next message"));
        let mut requested=awaiting.clone();requested["clear_context_request"]=json!({"request_id":"r","conversation_id":"conversation"});
        check(&requested,true,Some("The cleared conversation starts with the next message"));
        for status in ["submitted","compacting"] {
            check(&changed("compact_context_request",json!({"status":status})),true,Some("Wait for compaction to finish"));
        }
        check(&changed("compact_context_request",json!({"status":"uncertain"})),true,
            Some("The last compaction was not confirmed. Check Terminal"));
        for status in ["completed","failed","cancelled"] { check(&changed("compact_context_request",json!({"status":status})),true,None); }
        check(&changed("expected_id",json!("conversation")),true,Some("The agent has not confirmed the resumed conversation yet"));
        let busy=|phase:&str|{let mut record=idle.clone();record["activity"]=json!("busy");record["phase"]=json!(phase);record};
        check(&busy("compacting"),true,Some("Wait for compaction to finish"));
        check(&busy("input"),true,Some("Wait until the agent's question is answered"));
        check(&busy("approval"),true,Some("Wait until the agent's approval request is answered"));
        // Busy evidence without a recorded worker still explains the wait.
        check(&busy("working"),true,Some("Wait until the agent reports Ready"));
        check(&changed("activity",json!("unknown")),true,Some("Wait until the agent reports Ready"));
        // The parent finished its turn; one workflow child still works.
        let mut child=busy("working");
        child["subagents"]=json!({"worker":{"name":"workflow-subagent","state":"working","current_tool":"Bash","updated":2.0},
            "done":{"name":"Explore","state":"finished","updated":3.0}});
        check(&child,true,Some("Wait until the agent reports Ready. Still working: subagent “workflow-subagent” running Bash"));
        // The main turn names its tools once each, in start order; newer children
        // come first, and more than three items are counted.
        let mut many=busy("tool");many["main_done"]=json!(false);
        many["active_tools"]=json!({"b":{"name":"Read","started":2.0},"a":{"name":"Bash","started":1.0},"c":{"name":"Bash","started":3.0}});
        many["subagents"]=json!({"one":{"name":"general-purpose","state":"working","updated":1.0},
            "two":{"name":"","state":"working","current_tool":"Grep","updated":4.0},
            "three":{"name":"Explore","state":"working","updated":2.0},"four":{"name":"Plan","state":"working","updated":3.0}});
        check(&many,true,Some("Wait until the agent reports Ready. Still working: main agent running Bash and Read, \
            subagent “two” running Grep, subagent “Plan”, and 2 more"));
        many["active_tools"]["d"]=json!({"name":"Edit","started":4.0});many["subagents"]=json!({});
        check(&many,true,Some("Wait until the agent reports Ready. Still working: main agent running Bash and 2 other tools"));
        many["active_tools"]=json!({});
        check(&many,true,Some("Wait until the agent reports Ready. Still working: main agent"));
        // An interrupted or failed turn has ended: only its children still work.
        for phase in ["interrupted","error","unknown"] {
            let mut ended=many.clone();ended["activity"]=json!("unknown");ended["phase"]=json!(phase);
            ended["active_tools"]=json!({"a":{"name":"Bash","started":1.0}});
            ended["subagents"]=json!({"worker":{"name":"workflow-subagent","state":"working"}});
            check(&ended,true,Some("Wait until the agent reports Ready. Still working: subagent “workflow-subagent”"));
            ended["subagents"]=json!({});
            check(&ended,true,Some("Wait until the agent reports Ready"));
        }
        // Kimi profile groups count their active tasks.
        let mut kimi=busy("working");kimi["agent"]=json!("kimi");
        kimi["subagent_groups"]=json!({"coder":{"active_count":2},"reviewer":{"active_count":1},"idle":{"active_count":0}});
        check(&kimi,true,Some("Wait until the agent reports Ready. Still working: 2 “coder” subagents, 1 “reviewer” subagent"));
        // A native Kimi roster counts only its own children.
        kimi["kimi_native_roster"]=json!(true);
        kimi["subagents"]=json!({"hook":{"name":"coder","state":"working"},"native":{"name":"agent-1","state":"working","native_roster":true}});
        check(&kimi,true,Some("Wait until the agent reports Ready. Still working: subagent “agent-1”"));
        // The main turn's Stop can leave the saved activity idle while the
        // listing still shows work: Kimi profile groups, open tools or working
        // children keep both actions unavailable.
        let groups_waiting="Wait until the agent reports Ready. Still working: 2 “coder” subagents, 1 “reviewer” subagent";
        let mut groups=changed("agent",json!("kimi"));
        groups["subagent_groups"]=json!({"coder":{"active_count":2},"reviewer":{"active_count":1},"idle":{"active_count":0}});
        check(&groups,true,Some(groups_waiting));
        check(&groups,false,Some("The agent is not running"));
        // A Kimi hook names only a profile: its entry stands for that group,
        // which the listing counts instead of one invented child.
        groups["subagents"]=json!({"coder":{"name":"coder","state":"working"},"reviewer":{"name":"reviewer","state":"finished"}});
        check(&groups,true,Some(groups_waiting));
        let mut working_groups=groups.clone();working_groups["activity"]=json!("busy");working_groups["phase"]=json!("working");
        check(&working_groups,true,Some(groups_waiting));
        // An external child is listed beside the profile groups.
        groups["subagents"]["helper"]=json!({"name":"helper","state":"working","external":true,"updated":1.0});
        check(&groups,true,Some("Wait until the agent reports Ready. Still working: subagent “helper”, \
            2 “coder” subagents, 1 “reviewer” subagent"));
        // A native roster replaces the profile counts with its own children.
        groups["subagents"]=json!({});groups["kimi_native_roster"]=json!(true);
        check(&groups,true,None);
        groups["subagent_groups"]["coder"]["active_count"]=json!(0);groups["subagent_groups"]["reviewer"]["active_count"]=json!(0);
        groups.as_object_mut().unwrap().remove("kimi_native_roster");
        check(&groups,true,None);
        let mut tool=idle.clone();tool["active_tools"]=json!({"a":{"name":"Bash","started":1.0}});
        check(&tool,true,Some("Wait until the agent reports Ready"));
        let mut children=idle.clone();
        children["subagents"]=json!({"worker":{"name":"Explore","state":"working","current_tool":"Grep"}});
        check(&children,true,Some("Wait until the agent reports Ready. Still working: subagent “Explore” running Grep"));
        // Finished and unknown children are no current work.
        for state in ["finished","unknown"] {
            children["subagents"]["worker"]["state"]=json!(state);
            check(&children,true,None);
        }
    }
    #[test] fn a_claude_turn_stopped_with_escape_may_clear_once_nothing_works() {
        // Claude sends no hook after Esc; the prompt check happens at dispatch.
        let turn=json!({"agent":"claude","run_id":"run","conversation_id":"conversation","activity":"unknown",
            "phase":"interrupted","main_done":false,"active_tools":{},"subagents":{}});
        assert!(interrupted(&turn,true));
        assert!(!interrupted(&turn,false));
        // Compaction keeps waiting for Ready.
        assert_eq!(unavailable(&turn,true).as_deref(),Some("Wait until the agent reports Ready"));
        let changed=|key:&str,value:Value|{let mut record=turn.clone();record[key]=value;record};
        for (key,value) in [("agent",json!("codex")),("agent",json!("kimi")),("phase",json!("error")),("phase",json!("working")),
            ("conversation_id",json!("")),("expected_id",json!("conversation")),("error",json!("failed")),
            ("active_tools",json!({"a":{"name":"Bash","started":1.0}})),
            ("subagents",json!({"worker":{"name":"Explore","state":"working"}})),
            ("compact_context_request",json!({"status":"submitted"}))] {
            assert!(!interrupted(&changed(key,value.clone()),true),"{key}={value}");
        }
        // Dispatch's own recorded request does not end the stopped turn.
        let requested=changed("clear_context_request",json!({"request_id":"r","conversation_id":"conversation"}));
        assert!(!interrupted(&requested,true));
        assert!(stopped(&requested));
    }
    #[test] fn context_actions_agree_with_dispatch_on_the_ready_state_the_listing_reports() {
        // Inspect offers clear and compaction exactly when dispatch accepts
        // the session (input::checked_ready), whatever a replaced hook says.
        assert_eq!(unavailable(&telemetry::stale_hook_child(),true),None);
        for (case,record) in telemetry::remaining_work() {
            assert!(unavailable(&record,true).is_some_and(|reason| reason.starts_with("Wait ")),"{case}");
        }
    }
    #[test] fn terminal_choosers_keep_inspected_context_actions_waiting() {
        // The summary shows a terminal-only chooser over an idle saved record;
        // dispatch refuses it, so inspect reports both actions unavailable.
        let idle=json!({"agent":"kimi","run_id":"run","conversation_id":"conversation","activity":"idle","phase":"idle",
            "main_done":true,"active_tools":{},"subagents":{}});
        let shown=|phase:&str| json!({"activity":if phase=="idle" {"idle"} else {"busy"},"phase":phase});
        assert_eq!(unavailable_shown(&idle,true,&shown("input")).as_deref(),Some("Wait until the agent's question is answered"));
        assert_eq!(unavailable_shown(&idle,true,&shown("approval")).as_deref(),
            Some("Wait until the agent's approval request is answered"));
        assert_eq!(unavailable_shown(&idle,true,&shown("idle")),None);
        // A provider failure shown over an idle saved record keeps both
        // actions, as dispatch does.
        assert_eq!(unavailable_shown(&idle,true,&json!({"activity":"attention","phase":"error"})),None);
        // Earlier reasons keep their precedence.
        assert_eq!(unavailable_shown(&idle,false,&shown("input")).as_deref(),Some("The agent is not running"));
        let mut working=idle.clone();working["active_tools"]=json!({"a":{"name":"Bash","started":1.0}});
        working["activity"]=json!("busy");working["phase"]=json!("tool");working["main_done"]=json!(false);
        assert_eq!(unavailable_shown(&working,true,&shown("approval")),unavailable(&working,true));
    }
    #[test] fn clear_hooks_require_a_changed_conversation_and_keep_the_observed_time() {
        let original=json!({"run_id":"run","conversation_id":"old"});
        let mut record=original.clone();
        observe_start(&mut record,&json!({"session_id":"old","source":"clear"}));
        assert!(record["session_clear"].is_null());
        observe_start(&mut record,&json!({"session_id":"new","source":"resume"}));
        assert!(record["session_clear"].is_null());
        record["clear_context_request"]=json!({"conversation_id":"old"});
        observe_start(&mut record,&json!({"session_id":"new"}));
        record["conversation_id"]=json!("new");
        assert!(event(&record).is_some());
        record["run_id"]=json!("other");assert!(event(&record).is_none());
        record=original;
        record["session_clear"]=json!({"at":123.0,"run_id":"run","conversation_id":"old","awaiting_session_start":true});
        observe_start(&mut record,&json!({"session_id":"new","source":"clear"}));
        record["conversation_id"]=json!("new");
        assert_eq!(event(&record).unwrap()["at"],123.0);
        assert!(!awaiting_start(&record));
    }
    #[test] fn a_resumed_clear_start_needs_its_clear_of_the_expected_conversation() {
        let pending=json!({"agent":"codex","run_identity_version":1,"startup_kind":"resume","supervisor":{"pid":1,"start":"s"},
            "run_id":"run","conversation_id":"old","expected_id":"old","activity":"unknown","phase":"unknown","last_event_at":0});
        let clear=json!({"hook_event_name":"SessionStart","session_id":"new","source":"clear"});
        // Without the exact live process holding the expected rollout, only a
        // clear that Zerus requested or observed confirms the resume.
        assert!(!resumed_start(&pending,&clear));
        let mut requested=pending.clone();requested["clear_context_request"]=json!({"conversation_id":"old"});
        assert!(resumed_start(&requested,&clear));
        requested["clear_context_request"]["conversation_id"]=json!("other");assert!(!resumed_start(&requested,&clear));
        let mut observed=pending.clone();
        observed["session_clear"]=json!({"at":7.0,"run_id":"run","conversation_id":"old","previous_conversation_id":"old",
            "awaiting_session_start":true});
        assert!(resumed_start(&observed,&clear));
        for source in ["startup","resume",""] {
            let mut other=clear.clone();other["source"]=json!(source);assert!(!resumed_start(&observed,&other),"{source}");
        }
        for (key,value) in [("restore_archive_id",json!("archive")),("startup_kind",json!("new")),("expected_id",json!("other")),
            ("error",json!("failed")),("activity",json!("idle")),("last_event_at",json!(1.0)),("agent",json!("claude"))] {
            let mut other=observed.clone();other[key]=value;assert!(!resumed_start(&other,&clear),"{key}");
        }
        let mut stale=observed.clone();stale["session_clear"]["run_id"]=json!("earlier");assert!(!resumed_start(&stale,&clear));
        // The hook keeps the boundary observed in Terminal.
        observe_start(&mut observed,&clear);observed["conversation_id"]=json!("new");
        assert_eq!(event(&observed).unwrap()["at"],7.0);assert_eq!(observed["session_clear"]["journaled"],true);
        assert_eq!(earlier_conversations(&observed),json!(["old"]));
    }
    #[test] fn a_resume_clears_only_when_verified_and_never_compacts_unconfirmed() {
        let pending=json!({"agent":"codex","run_identity_version":1,"startup_kind":"resume","supervisor":{"pid":1,"start":"s"},
            "run_id":"run","conversation_id":"old","expected_id":"old","activity":"unknown","phase":"unknown","last_event_at":0});
        // Compaction, like `available`, needs the confirmed conversation; a
        // resume needs its exact live process, absent here.
        assert!(!available(&pending,true));assert!(!resumed(&pending,true));assert!(!resumed(&pending,false));
    }
    #[test] fn only_the_requested_resumed_clear_can_survive_its_exact_main_end() {
        let mut record=json!({"agent":"codex","run_identity_version":1,"startup_kind":"resume","supervisor":{},
            "run_id":"run","conversation_id":"old","expected_id":"old","activity":"unknown","phase":"unknown","last_event_at":0,
            "clear_context_request":{"request_id":"11111111-1111-4111-8111-111111111111","run_id":"run","conversation_id":"old","at":1.0}});
        assert!(ending_resume(&record));
        record["session_end"]=json!({"run_id":"run","conversation_id":"old","at":2.0});
        record["last_event_at"]=json!(2.1);
        assert!(!resume_input::pending(&record));
        assert!(resumed_start(&record,&json!({"source":"clear"})));
        assert!(!resumed_start(&record,&json!({"source":"resume"})));
        record["session_end"]["clear_request_id"]=record["clear_context_request"]["request_id"].clone();
        record["session_end"]["clear_event_at"]=record["last_event_at"].clone();
        assert!(resume_input::pending(&record));
        assert!(resumed_start(&record,&json!({"source":"clear"})));
        assert!(!resumed_start(&record,&json!({"source":"resume"})));
        let mut sent=record.clone();sent["input_pending_at"]=json!(3.0);
        assert!(resume_input::pending(&sent));assert!(!resume_clearable(&sent));
        assert!(resumed_start(&sent,&json!({"source":"clear"})));
        for (key,value) in [("run_id",json!("other")),("conversation_id",json!("other")),("at",json!(0.5)),
            ("clear_request_id",json!("other")),("clear_event_at",json!(1.5))] {
            let mut changed=record.clone();changed["session_end"][key]=value;
            assert!(!resume_input::pending(&changed),"end {key}");
        }
        for (key,value) in [("last_event_at",json!(3.0)),
            ("active_tools",json!({"a":{"name":"Bash","started":2.0}})),("expected_id",json!("other")),
            ("activity",json!("busy")),("startup_kind",json!("new")),("agent",json!("claude"))] {
            let mut changed=record.clone();changed[key]=value;
            assert!(!resume_input::pending(&changed),"record {key}");
        }
        for (key,value) in [("run_id",json!("stale")),("conversation_id",json!("other")),("at",json!(3.0)),
            ("request_id",json!("22222222-2222-4222-8222-222222222222"))] {
            let mut changed=record.clone();changed["clear_context_request"][key]=value;
            assert!(!resume_input::pending(&changed),"request {key}");
        }
    }
    #[test] fn reset_panel_accepts_only_the_fixed_native_usage_notices() {
        let warning="⚠ Heads up, you have less than 10% of your weekly limit left. Run /status for a breakdown.";
        let footer="⚠ weekly limit: 10% left · resets at 11:22 PM on 7 Nov · /status";
        let screen=format!(">_ OpenAI Codex (v0.166.0)\n/fixture\npermissions: default\n\n{warning}\n\n{footer}\n\n› Ask Codex to do anything\n\nGPT-6.1-Sol default · /fixture\n? for shortcuts");
        assert!(codex_reset_panel(&screen,"/fixture"));
        for changed in [screen.replace(warning,"⚠ An unknown warning"),screen.replace("10% left","101% left"),
            screen.replace("11:22 PM","13:22 PM"),screen.replace("Run /status for a breakdown.","Approve this command."),
            screen.replace("› Ask Codex to do anything","› my draft"),format!("Previous conversation\n{screen}")] {
            assert!(!codex_reset_panel(&changed,"/fixture"),"{changed}");
        }
    }
    #[test] fn fresh_starts_continue_the_activity_timeline_like_a_clear() {
        let previous=json!({"run_id":"old-run","conversation_id":"second","cleared_conversations":["first"],"journal_name":"anchor","permission_override":"native:{\"mode\":\"acceptEdits\"}"});
        let mut record=json!({"run_id":"run","conversation_id":null});
        carry_fresh_start(&previous,&mut record);
        assert_eq!(earlier_conversations(&record),json!(["first","second"]));
        assert_eq!(record["journal_name"],"anchor");
        assert_eq!(record["permission_override"],"native:{\"mode\":\"acceptEdits\"}");
        observe_start(&mut record,&json!({"session_id":"third","source":"startup"}));
        record["conversation_id"]=json!("third");
        assert!(record["fresh_from"].is_null());
        assert_eq!(record["session_clear"]["previous_conversation_id"],"second");
        assert!(event(&record).is_some());
        assert_eq!(earlier_conversations(&record),json!(["first","second"]));
        // An unconfirmed binding has no conversation of its own to carry.
        let mut unconfirmed=json!({"run_id":"run"});
        carry_fresh_start(&json!({"run_id":"old","conversation_id":null}),&mut unconfirmed);
        assert!(unconfirmed["fresh_from"].is_null());assert!(unconfirmed["cleared_conversations"].is_null());
    }
    #[test] fn confirmed_clears_keep_the_earlier_activity_timeline() {
        let mut record=json!({"run_id":"run","conversation_id":"first"});
        for (next,previous) in [("second","first"),("third","second")] {
            observe_start(&mut record,&json!({"session_id":next,"source":"clear"}));
            record["conversation_id"]=json!(next);
            assert_eq!(record["session_clear"]["previous_conversation_id"],previous);
        }
        assert_eq!(earlier_conversations(&record),json!(["first","second"]));
        assert_eq!(record["session_clear"]["journaled"],false);
        // A clear first confirmed in Terminal was already journaled.
        record["session_clear"]=json!({"at":7.0,"run_id":"run","conversation_id":"third","awaiting_session_start":true});
        observe_start(&mut record,&json!({"session_id":"fourth","source":"clear"}));
        assert_eq!(record["session_clear"]["journaled"],true);
        record["conversation_id"]=json!("fourth");
        assert_eq!(earlier_conversations(&record),json!(["first","second","third"]));
        for index in 0..40 {
            let next=format!("later-{index}");
            observe_start(&mut record,&json!({"session_id":next,"source":"clear"}));
            record["conversation_id"]=json!(next);
        }
        let earlier=earlier_conversations(&record);
        assert_eq!(earlier.as_array().unwrap().len(),CLEARED_CONVERSATIONS);
        assert_eq!(earlier[CLEARED_CONVERSATIONS-1],"later-38");
        // Clears confirmed by an earlier version continue from their predecessor.
        let mut legacy=json!({"run_id":"run","conversation_id":"second",
            "session_clear":{"conversation_id":"second","previous_conversation_id":"first"}});
        assert_eq!(earlier_conversations(&legacy),json!(["first"]));
        observe_start(&mut legacy,&json!({"session_id":"third","source":"clear"}));
        legacy["conversation_id"]=json!("third");
        assert_eq!(earlier_conversations(&legacy),json!(["first","second"]));
        legacy["session_clear"]=json!({"conversation_id":"third","previous_conversation_id":"third","awaiting_session_start":true});
        legacy.as_object_mut().unwrap().remove("cleared_conversations");
        assert_eq!(earlier_conversations(&legacy),json!([]));
        // Another conversation is not a continuation of this timeline.
        observe_start(&mut record,&json!({"session_id":"resumed","source":"resume"}));
        assert!(record["cleared_conversations"].is_null() && record["session_clear"].is_null());
        assert_eq!(earlier_conversations(&record),json!([]));
    }
    // A POSIX working directory, not absolute on Windows; the Windows forms
    // are covered by windows_reset_panel_names_the_folder_below_the_platform_home.
    #[cfg(unix)]
    #[test] fn only_the_complete_native_empty_reset_header_matches() {
        let panel="\n >_ OpenAI Codex (v0.162.0)\n /fixture\n permissions: YOLO mode\n\n\n› Ask Codex to do anything\n GPT-6.1-Sol default\n ? for shortcuts\n";
        assert!(codex_reset_panel(panel,"/fixture"));
        for screen in [panel.replace("/fixture","/other"),panel.replace("/fixture","/fix…"),
            panel.replace("permissions: YOLO mode","To get started, describe a task"),
            panel.replace("\n\n›","\nPrevious agent response\n›"),
            panel.replace(">_ OpenAI Codex (v0.162.0)","Quoted reset header"),
            panel.replace("\n >_","\nSome user message\n >_"),">_ OpenAI Codex (v0.162.0)\n/fixture\n".to_owned(),
            // The composer is the only prompt row, at the left edge.
            panel.replace("\n\n\n›","\n\n› Previous request\n\n›"),panel.replace("\n› Ask","\n  › Ask"),
            panel.replace("› Ask Codex to do anything","›Ask Codex to do anything"),
            panel.replace(" ? for shortcuts"," › 1. Selected option")] {
            assert!(!codex_reset_panel(&screen,"/fixture"),"{screen}");
        }
    }
    // Real Linux captures of one session after its first answered message and
    // after /clear: Codex 0.162.0 at 95x47 with animations, the folder outside
    // the home printed in full, and resident 0.160.1 at 110x35 in reduced
    // motion below the home (`~/work`). The header stays above the history
    // until /clear; only the empty state below it is a reset.
    #[cfg(unix)]
    #[test] fn the_header_above_an_answered_first_message_is_not_a_reset() {
        for (answered, cleared, cwd) in [
            (include_str!("../../tests/fixtures/codex-first-answer.txt"),
             include_str!("../../tests/fixtures/codex-clear-result.txt"), "/fixture/project"),
            (include_str!("../../tests/fixtures/codex-first-answer-0.160.1.txt"),
             include_str!("../../tests/fixtures/codex-clear-result-0.160.1.txt"), "/home/example/work")] {
            let home = Path::new("/home/example");
            assert!(codex_reset_panel_below(cleared, cwd, home), "{cleared}");
            assert!(!codex_reset_panel_below(answered, cwd, home), "{answered}");
            // Without the history it would be the reset panel: the history,
            // not the folder or the composer, is what fails.
            let history = ["› First fixture message", "• Fixture answer.", "Worked for <1s • 10:52 AM"];
            let unanswered = answered.lines().filter(|line| !history.contains(&line.trim())).collect::<Vec<_>>().join("\n");
            assert!(codex_reset_panel_below(&unanswered, cwd, home), "{unanswered}");
            for kept in history {
                let partial = answered.lines().filter(|line| line.trim() == kept || !history.contains(&line.trim()))
                    .collect::<Vec<_>>().join("\n");
                assert!(!codex_reset_panel_below(&partial, cwd, home), "{partial}");
            }
            // Any other prompt row, even below the composer, fails closed.
            assert!(!codex_reset_panel_below(&format!("{cleared}› Ask Codex to do anything\n"), cwd, home));
        }
        // Byte-exact 0.162.0 screens at 110x35: the reset, a draft in its
        // composer, and a draft left in the history above the composer.
        let home = Path::new("/home/example");
        let result = include_str!("../../tests/fixtures/codex-clear-110x35-result.txt");
        assert!(codex_reset_panel_below(result, "/home/example/project", home), "{result}");
        assert!(codex_reset_panel_below(&result.replacen("› Ask Codex to do anything", "› Ask a follow-up question", 1),
            "/home/example/project", home));
        for screen in [include_str!("../../tests/fixtures/codex-clear-110x35-draft.txt"),
            include_str!("../../tests/fixtures/codex-clear-110x35-history.txt"),
            &result.replacen("› Ask Codex to do anything", "› Ask Codex to do anything else", 1)] {
            assert!(!codex_reset_panel_below(screen, "/home/example/project", home), "{screen}");
        }
    }
    // A POSIX working directory, not absolute on Windows; the Windows forms
    // are covered by windows_reset_panel_names_the_folder_below_the_platform_home.
    #[cfg(unix)]
    #[test] fn resident_native_greetings_match_without_accepting_other_prose() {
        let header = ">_ OpenAI Codex (v0.160.1)\n/fixture\npermissions: YOLO mode\n\n";
        let footer = "\n\n› Ask Codex to do anything\nGPT-6.1-Sol default\n? for shortcuts\n";
        for greeting in include_str!("data/codex-0.160.1-greetings.txt").lines().filter(|line| !line.starts_with('#')) {
            assert!(codex_reset_panel(&format!("{header}{greeting}{footer}"), "/fixture"), "{greeting}");
            if let Some((first, last)) = greeting.rsplit_once(' ') {
                assert!(codex_reset_panel(&format!("{header}{first}\n{last}{footer}"), "/fixture"));
            }
        }
        for prose in ["Previous agent response", "Resuming session…", "Clear failed", "Unknown greeting",
            "What are we cooking up?\nPrevious agent response", "What are we cooking up?\nWhat are we cooking up?"] {
            assert!(!codex_reset_panel(&format!("{header}{prose}{footer}"), "/fixture"), "{prose}");
        }
    }
    // A POSIX working directory, not absolute on Windows; the Windows forms
    // are covered by windows_reset_panel_names_the_folder_below_the_platform_home.
    #[cfg(unix)]
    #[test] fn only_the_complete_released_idle_logo_is_decoration() {
        let header = ">_ OpenAI Codex (v0.162.0)\n/fixture\npermissions: YOLO mode\n\n";
        let logo = include_str!("data/codex-empty-state-60x21.txt").lines()
            .filter(|line| !line.starts_with('#')).collect::<Vec<_>>().join("\n");
        let footer = "\n\n› Ask Codex to do anything\nGPT-6.1-Sol default\n? for shortcuts\n";
        assert!(codex_reset_panel(&format!("{header}{logo}{footer}"), "/fixture"));
        assert!(codex_reset_panel(&format!("{header}What are we cooking up?\n{logo}{footer}"), "/fixture"));
        for body in [logo.replacen('⣀', "⣁", 1), logo.lines().skip(1).collect::<Vec<_>>().join("\n"),
            format!("{logo}\nPrevious answer"), format!("Previous answer\n{logo}"),
            format!("{logo}\n{logo}"), "⣿⣿⣿\n⣿⣿⣿".to_owned()] {
            assert!(!codex_reset_panel(&format!("{header}{body}{footer}"), "/fixture"), "{body}");
        }
        assert!(!codex_reset_panel(&format!("{header}{logo}{footer}"), "/other"));
    }
    #[test] fn windows_reset_panel_names_the_folder_below_the_platform_home() {
        // Codex 0.162.0 under bundled ConPTY after /clear (spike S3), with its
        // idle logo; the account folder is the synthetic user01.
        let panel = include_str!("../../tests/fixtures/codex-clear-result-windows.txt");
        let cwd = r"C:\Users\user01\zerus-dev\p2\s3\homes\codex-commands\work";
        let home = Path::new(r"C:\Users\user01");
        assert!(codex_reset_panel_below(panel, cwd, home));
        // Windows names ignore case, in the folder and in the home.
        assert!(codex_reset_panel_below(panel, &cwd.to_lowercase(), home));
        assert!(codex_reset_panel_below(panel, cwd, Path::new(r"c:\users\USER01\")));
        let shown = r"~\zerus-dev\p2\s3\homes\codex-commands\work";
        assert!(codex_reset_panel_below(&panel.replacen(shown, cwd, 1), cwd, home));
        assert!(codex_reset_panel_below(&panel.replacen(shown, r"C:\USERS\user01\zerus-dev\p2\s3\homes\codex-commands\work", 1), cwd, home));
        for (cwd, home) in [(r"C:\Users\user01\zerus-dev\p2\s3\homes\codex-commands", r"C:\Users\user01"),
            (cwd, r"C:\Users\other"), (cwd, r"D:\Users\user01"), (cwd, "/home/example"), ("/fixture", r"C:\Users\user01")] {
            assert!(!codex_reset_panel_below(panel, cwd, Path::new(home)), "{cwd} {home}");
        }
        for row in [r"~/zerus-dev/p2/s3/homes/codex-commands/work", r"~\zerus-dev\p2\s3\homes\codex-commands", "~"] {
            assert!(!codex_reset_panel_below(&panel.replacen(shown, row, 1), cwd, home), "{row}");
        }
        assert!(!codex_reset_panel_below(&panel.replace("› Ask Codex", "Previous answer\n› Ask Codex"), cwd, home));
        // The header stays above a first answered message in both Windows
        // forms; a history prompt above the composer is never the reset panel.
        for row in [shown, cwd] {
            let screen = panel.replacen(shown, row, 1);
            assert!(!codex_reset_panel_below(&screen.replacen("permissions: YOLO mode",
                "permissions: YOLO mode\n\n› First fixture message\n\n• Fixture answer.", 1), cwd, home), "{row}");
        }
        // The Linux captures before and after /clear, with the Windows folder.
        let answered = include_str!("../../tests/fixtures/codex-first-answer.txt");
        let cleared = include_str!("../../tests/fixtures/codex-clear-result.txt");
        for row in [shown, cwd, r"c:\users\USER01\zerus-dev\p2\s3\homes\codex-commands\work"] {
            assert!(codex_reset_panel_below(&cleared.replace("/fixture/project", row), cwd, home), "{row}");
            assert!(!codex_reset_panel_below(&answered.replace("/fixture/project", row), cwd, home), "{row}");
        }
    }
    #[test] fn only_a_waiting_claude_chooser_can_be_dismissed_for_a_clear() {
        let waiting=json!({"agent":"claude","conversation_id":"conversation","activity":"busy","phase":"input"});
        assert!(dismissable(&waiting,true));assert!(!dismissable(&waiting,false));
        let mut approval=waiting.clone();approval["phase"]=json!("approval");assert!(dismissable(&approval,true));
        for (key,value) in [("agent",json!("codex")),("phase",json!("idle")),("phase",json!("working")),
            ("conversation_id",json!("")),("error",json!("failed")),("expected_id",json!("next")),
            ("clear_context_request",json!({"request_id":"pending"}))] {
            let mut other=waiting.clone();other[key]=value;assert!(!dismissable(&other,true),"{key}");
        }
        // Esc can end a launch at a startup chooser; only Activity answers those.
        assert!(startup_choice(&json!([{"source":"claude_resume"}])));
        assert!(startup_choice(&json!([{"source":"hook"},{"source":"claude_folder_trust"}])));
        assert!(!startup_choice(&json!([{"source":"claude_terminal_choice"},{"source":"claude_tool_approval"}])));
    }
    #[test] fn dismissal_is_an_explicit_optional_request_field() {
        let base=json!({"request_id":"r","expected_run_id":"run","expected_conversation_id":"c"});
        assert!(!serde_json::from_value::<Request>(base.clone()).unwrap().dismiss_prompt);
        let mut forced=base.clone();forced["dismiss_prompt"]=json!(true);
        assert!(serde_json::from_value::<Request>(forced).unwrap().dismiss_prompt);
        let mut unknown=base;unknown["force"]=json!(true);assert!(serde_json::from_value::<Request>(unknown).is_err());
    }
    #[test] fn exact_command_only() {
        assert!(command_row("codex","› /clear",8,"/clear"));
        assert!(command_row("codex","» /clear",8,"/clear"));
        assert!(command_row("claude","❯\u{a0}/clear",8,"/clear"));
        assert!(command_row("kimi"," │ > /clear       │",11,"/clear"));
        assert!(command_row("kimi"," │ > /compact  <instruction>          │",13,"/compact"));
        for row in [" │ > /compact extra  │"," │ > /compact  <a> <b> │"," │ > /compact  <instruction"] {assert!(!command_row("kimi",row,13,"/compact"));}
        for row in ["› /clear extra","› draft /clear","» /clear extra","» draft /clear","shell$ /clear"] {assert!(!command_row("codex",row,8,"/clear"));}
    }
}
