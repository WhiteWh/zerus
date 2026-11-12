//! Structured provider questions. Hooks preserve new requests; Kimi's own wire
//! transcript recovers the exact interaction identity for already-running TUIs.
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom};
use std::time::{Duration, Instant};

const MAX_WIRE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 128 * 1024;

fn text(value: &Value, max: usize) -> Result<String> {
    let value = value.as_str().unwrap_or("");
    if value.chars().count() > max
        || value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err("question text exceeds supported limits or contains control characters".into());
    }
    Ok(value.into())
}

pub(super) fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub(super) fn normalize(items: &Value) -> Result<Value> {
    let items = items.as_array().ok_or("questions must be an array")?;
    if items.is_empty() || items.len() > 4 {
        return Err("question requests must contain one to four questions".into());
    }
    let mut questions = Vec::new();
    let mut seen_questions = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let question = text(&item["question"], 8192)?;
        if question.trim().is_empty() || !seen_questions.insert(question.clone()) {
            return Err("question text is empty or duplicated".into());
        }
        let options = item["options"]
            .as_array()
            .ok_or("question options are missing")?;
        if !(2..=4).contains(&options.len()) {
            return Err("each question must have two to four options".into());
        }
        let mut choices = Vec::new();
        let mut seen_options = BTreeSet::new();
        for (option_index, option) in options.iter().enumerate() {
            let label = text(&option["label"], 1024)?;
            if label.trim().is_empty() || !seen_options.insert(label.clone()) {
                return Err("option labels are empty or duplicated".into());
            }
            choices.push(
                json!({"id":format!("opt_{index}_{option_index}"),"label":label,
                "description":text(&option["description"],4096)?}),
            );
        }
        questions.push(json!({"id":format!("q_{index}"),"question":question,
            "header":text(&item["header"],128)?,"body":text(&item["body"],8192)?,
            "other_label":text(item.get("other_label").or_else(||item.get("otherLabel")).unwrap_or(&Value::Null),1024)?,
            "other_description":text(item.get("other_description").or_else(||item.get("otherDescription")).unwrap_or(&Value::Null),4096)?,
            "multi_select":item.get("multi_select").or_else(||item.get("multiSelect")).and_then(Value::as_bool).unwrap_or(false),
            "allow_other":true,"options":choices}));
    }
    Ok(json!(questions))
}

fn tool_id(event: &Value) -> &str {
    ["tool_use_id", "tool_call_id"]
        .iter()
        .map(|key| string(event, key))
        .find(|value| !value.is_empty())
        .unwrap_or("")
}

pub(super) fn fingerprint(card: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            json!({"question_id":card["question_id"],
        "tool_call_id":card["tool_call_id"],"run_id":card["run_id"],
        "conversation_id":card["conversation_id"],"questions":card["questions"]})
            .to_string()
        )
    )
}

fn card(
    record: &Value,
    id: &str,
    tool: &str,
    items: &Value,
    source: &str,
    at: f64,
) -> Result<Value> {
    if id.is_empty() || id.len() > 240 || !token(tool) {
        return Err("missing question interaction identity".into());
    }
    let mut card = json!({"question_id":id,"tool_call_id":tool,"agent_id":"main",
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],
        "created_at":at,"source":source,"questions":normalize(items)?,
        "can_answer":false,"answer_transport":"","answer_unavailable_reason":"Open Terminal to answer this question."});
    card["question_hash"] = json!(fingerprint(&card));
    Ok(card)
}

pub(super) fn from_hook(record: &Value, event: &Value) -> Option<Value> {
    if string(event, "hook_event_name") != "PreToolUse"
        || string(event, "tool_name") != "AskUserQuestion"
        || !string(event, "agent_id").is_empty()
    {
        return None;
    }
    let tool = tool_id(event);
    card(
        record,
        &format!("tool:{tool}"),
        tool,
        &event["tool_input"]["questions"],
        "hook",
        now(),
    )
    .ok()
}

pub(super) fn observe(record: &mut Value, event: &Value) {
    claude_approval::observe(record,event);
    if !string(event, "agent_id").is_empty() {
        return;
    }
    let kind = string(event, "hook_event_name");
    if kind=="PermissionRequest" {
        record["pending_approval"]=event.clone();
        record["pending_approval_at"]=json!(now());
    } else if ["SessionStart","SessionEnd","Interrupt","Stop","StopFailure","PostToolUse","PostToolUseFailure","UserPromptSubmit"].contains(&kind) {
        record.as_object_mut().unwrap().remove("pending_approval");
        record.as_object_mut().unwrap().remove("pending_approval_at");
    }
    if matches!(
        kind,
        "SessionStart" | "SessionEnd" | "Interrupt" | "Stop" | "StopFailure"
    ) {
        record.as_object_mut().unwrap().remove("pending_questions");
        return;
    }
    let mut pending = record["pending_questions"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(card) = from_hook(record, event) {
        pending.insert(string(&card, "tool_call_id").into(), card);
    } else if matches!(kind, "PostToolUse" | "PostToolUseFailure") {
        let tool = tool_id(event);
        if !tool.is_empty() {
            pending.remove(tool);
        } else if string(event, "tool_name") == "AskUserQuestion" {
            pending.clear();
        }
    }
    record["pending_questions"] = json!(pending);
}

pub(super) fn kimi_home(record: &Value) -> PathBuf {
    let specified = string(record, "agent_home");
    if specified.is_empty() {
        return home().join(".kimi-code");
    }
    let path = PathBuf::from(specified);
    if path.is_absolute() {
        path
    } else {
        Path::new(string(record, "launch_dir")).join(path)
    }
}

pub(super) fn wire_paths(record: &Value) -> Result<Vec<PathBuf>> {
    if string(record, "agent") != "kimi" {
        return Ok(vec![]);
    }
    let session = string(record, "conversation_id");
    if !token(session) {
        return Ok(vec![]);
    }
    let sessions = kimi_home(record).join("sessions");
    let Ok(root) = crate::platform::canonical(&sessions) else {
        return Ok(vec![]);
    };
    let directories = fs::read_dir(&root).map_err(|error| error.to_string())?;
    let mut paths = Vec::new();
    for directory in directories.take(2000).flatten() {
        let path = directory
            .path()
            .join(session)
            .join("agents/main/wire.jsonl");
        if let Ok(resolved) = crate::platform::canonical(&path) {
            if resolved.starts_with(&root) && resolved.is_file() {
                paths.push(resolved);
            }
        }
    }
    if paths.len() > 1 {
        return Err("Kimi conversation exists in multiple workspace histories".into());
    }
    Ok(paths)
}

fn wire_events(record: &Value) -> Result<Vec<Value>> {
    let mut events = Vec::new();
    for path in wire_paths(record)? {
        let mut file = File::open(&path).map_err(|error| error.to_string())?;
        let size = file.metadata().map_err(|error| error.to_string())?.len();
        let offset = size.saturating_sub(MAX_WIRE_BYTES);
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(MAX_WIRE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if (offset > 0 && index == 0)
                || line.len() > MAX_PAYLOAD_BYTES as usize
                || line.is_empty()
            {
                continue;
            }
            if !line.windows(12).any(|part| part == b"interaction.") {
                continue;
            }
            if let Ok(event) = serde_json::from_slice::<Value>(line) {
                if string(&event, "agentId") == "main"
                    && string(&event, "type").starts_with("interaction.")
                {
                    events.push(event);
                }
            }
        }
    }
    Ok(events)
}

fn recover_wire(record: &Value, events: &[Value]) -> BTreeMap<String, Value> {
    let mut pending = BTreeMap::new();
    for event in events {
        let id = string(event, "id");
        if id.is_empty() {
            continue;
        }
        match string(event, "type") {
            "interaction.request" if string(event, "kind") == "question" => {
                let tool = string(event, "toolCallId");
                let active = record["active_tools"]
                    .get(tool)
                    .is_some_and(|tool| string(tool, "name") == "AskUserQuestion");
                if !active {
                    continue;
                }
                if let Ok(card) = card(
                    record,
                    id,
                    tool,
                    &event["request"]["questions"],
                    "kimi_wire",
                    event["time"].as_f64().unwrap_or(0.0) / 1000.0,
                ) {
                    pending.insert(id.into(), card);
                }
            }
            "interaction.resolved" | "interaction.cancelled" | "interaction.dismissed" => {
                pending.remove(id);
            }
            _ => {}
        }
    }
    pending
}

/// Current normalized identities without availability checks (adapter guard API).
pub(super) fn current(record: &Value) -> Result<Vec<Value>> {
    Ok(collect(record, None, false, true)?.unwrap_or_default().0)
}

/// Like `current`, but None when the Codex question index cannot be brought up
/// to date by `deadline`; its progress is kept for the next call.
pub(super) fn current_until(record: &Value, deadline: std::time::Instant) -> Result<Option<Vec<Value>>> {
    Ok(collect(record, Some(deadline), false, false)?.map(|(cards, _)| cards))
}

/// The cards, and whether the Codex question index was complete by
/// `deadline`. An incomplete index is None, unless `partial` keeps the other
/// cards: then it adds no Codex question and nothing inferred from their
/// absence (zerus-a4mub.8). `listing` marks a question listing (`current`
/// and `inspection`): only a listing may fit a clipped Claude approval into
/// its unviewed pane. A deadline alone never decides that.
fn collect(
    record: &Value,
    deadline: Option<std::time::Instant>,
    partial: bool,
    listing: bool,
) -> Result<Option<(Vec<Value>, bool)>> {
    let mut by_tool = BTreeMap::new();
    if let Some(pending) = record["pending_questions"].as_object() {
        for card in pending.values() {
            let tool = string(card, "tool_call_id");
            if card["run_id"] == record["run_id"]
                && card["conversation_id"] == record["conversation_id"]
                && record["active_tools"]
                    .get(tool)
                    .is_some_and(|tool| string(tool, "name") == "AskUserQuestion")
            {
                by_tool.insert(tool.to_owned(), card.clone());
            }
        }
    }
    let events = wire_events(record)?;
    // Native interactions supersede hook-only identities, including when a
    // resolution reaches the wire before the corresponding PostToolUse hook.
    for event in &events {
        if string(event, "type") == "interaction.request" && string(event, "kind") == "question" {
            by_tool.remove(string(event, "toolCallId"));
        }
    }
    for card in recover_wire(record, &events).into_values() {
        by_tool.insert(string(&card, "tool_call_id").into(), card);
    }
    let mut cards: Vec<_> = by_tool.into_values().collect();
    cards.extend(kimi_plan::recover(record, &events));
    cards.extend(kimi_approval::recover(record, &events));
    let mut complete = true;
    match deadline {
        None => cards.extend(codex_questions::current(record)?),
        Some(deadline) => match codex_questions::current_until(record, deadline)? {
            Some(codex) => cards.extend(codex),
            None if partial => complete = false,
            None => return Ok(None),
        },
    }
    // Only a listing may fit a clipped approval into its unviewed pane.
    let approval=if listing {claude_approval::listed(record)} else {claude_approval::current(record)};
    if let Some(approval)=approval{cards.push(approval);}
    if let Some(mode) = claude_permission::current(record) {
        cards.push(mode);
    }
    if let Some(resume) = claude_resume::current(record) { cards.push(resume); }
    if let Some(limit) = claude_usage_limit::current(record) { cards.push(limit); }
    if let Some(trust) = claude_trust::current(record) {
        cards.push(trust);
    }
    if let Some(trust) = codex_trust::current(record) {cards.push(trust);}
    if let Some(trust) = codex_hooks_trust::current(record) {cards.push(trust);}
    if let Some(update) = codex_update::current(record) {cards.push(update);}
    if let Some(goal) = codex_goal_resume::current(record) {cards.push(goal);}
    if let Some(limit) = codex_rate_limit::current(record) {cards.push(limit);}
    if let Some(trust) = kimi_trust::current(record) {
        cards.push(trust);
    }
    if let Some(hint) = kimi_cache_hint::current(record) {
        cards.push(hint);
    }
    if let Some(terminal) = claude_approval::terminal_card(record, &cards) {
        cards.push(terminal);
    }
    if cards.is_empty() && complete {
        cards.extend(startup_choice::current(record));
    }
    cards.sort_by(|a, b| {
        a["created_at"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["created_at"].as_f64().unwrap_or(0.0))
    });
    Ok(Some((cards, complete)))
}

/// The current cards of the deferred answer kind's sources
/// (`deferred_answer::SOURCES`), rebuilt with pure reads only, in
/// `collect`'s order: no fit, no `claude_approval` listing, no Codex question
/// index or startup lock, nothing written. The three Kimi wire sources get
/// `can_answer`, `answer_transport` and `answer_unavailable_reason` exactly
/// as `inspection` sets them, from their pure availability checks; a panel
/// that `inspection` would have fitted but is clipped now therefore yields no
/// card or no transport, and the comparison fails closed.
pub(super) fn observe_current(record: &Value) -> Result<Vec<Value>> {
    let events = wire_events(record)?;
    let mut cards: Vec<Value> = recover_wire(record, &events).into_values().collect();
    cards.extend(kimi_plan::recover(record, &events));
    cards.extend(kimi_approval::recover(record, &events));
    cards.extend(
        [
            claude_permission::current(record),
            claude_resume::current(record),
            claude_usage_limit::current(record),
            claude_trust::current(record),
            codex_trust::current(record),
            codex_hooks_trust::current(record),
            codex_update::current(record),
            codex_rate_limit::current(record),
            kimi_trust::current(record),
            kimi_cache_hint::current(record),
        ]
        .into_iter()
        .flatten(),
    );
    let sources = zerus_client_core::desktop::deferred_answer::SOURCES;
    cards.retain(|card| sources.contains(&string(card, "source")));
    for card in &mut cards {
        let source = string(card, "source").to_owned();
        if string(record, "agent") != "kimi"
            || ![kimi_plan::SOURCE, kimi_approval::SOURCE, "kimi_wire"].contains(&source.as_str())
        {
            continue;
        }
        let available = if source == kimi_plan::SOURCE {
            kimi_plan::available(record, card)
        } else if source == kimi_approval::SOURCE {
            kimi_approval::available(record, card)
        } else {
            super::question_terminal::available(record, card)
        };
        match available {
            Ok(()) => {
                card["can_answer"] = json!(true);
                card["answer_transport"] = json!("kimi_tui");
                card["answer_unavailable_reason"] = json!("");
            }
            Err(error) => card["answer_unavailable_reason"] = json!(error),
        }
    }
    cards.sort_by(|a, b| {
        a["created_at"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["created_at"].as_f64().unwrap_or(0.0))
    });
    Ok(cards)
}

/// The observation's stdin: one question of one exact run and conversation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveRequest {
    question_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
}
/// The largest observation reply.
const MAX_OBSERVATION_BYTES: usize = 64 << 10;

/// `hgs answer <session> --observe --json`: the consent identity of one
/// pending question of the deferred answer kind's sources, read without any
/// effect ([`observe_current`]). It never answers, locks, fits a panel,
/// writes a receipt or writes anything else.
fn observe_dispatch(name: &str) -> Result<i32> {
    let mut input = Vec::new();
    io::stdin()
        .take(MAX_PAYLOAD_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| error.to_string())?;
    if input.len() as u64 > MAX_PAYLOAD_BYTES {
        return Err("observation payload exceeds 128 KiB".into());
    }
    let request: ObserveRequest = serde_json::from_slice(&input)
        .map_err(|error| format!("invalid observation payload: {error}"))?;
    if request.expected_run_id.is_empty() || request.question_id.is_empty() {
        return Err("question identities are required".into());
    }
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    let record = read(name)?;
    // The answer path's own identity and availability rule, read only.
    let current = string(&record, "run_id") == request.expected_run_id
        && string(&record, "conversation_id") == request.expected_conversation_id
        && matches(&record, panes_of(&live()?, &record))
        && process_alive(&record)
        && !pause_active(&record)
        && string(&record, "error").is_empty()
        && (string(&record, "expected_id").is_empty() || startup_question_id(&request.question_id));
    let card = if current {
        observe_current(&record)?
            .into_iter()
            .find(|card| string(card, "question_id") == request.question_id)
    } else {
        None
    };
    let reply = json!({"found": card.is_some(),
        "consent_identity": card.map(|card| zerus_client_core::questions::consent_identity(&card))});
    let text = reply.to_string();
    if text.len() > MAX_OBSERVATION_BYTES {
        return Err("observation exceeds 64 KiB".into());
    }
    println!("{text}");
    Ok(0)
}

pub(super) fn wire_response(record: &Value, question_id: &str) -> Result<Option<Value>> {
    // A closure belongs only to the latest request with this ID, never a reused one.
    for event in wire_events(record)?.into_iter().rev() {
        if string(&event, "id") != question_id {
            continue;
        }
        match string(&event, "type") {
            "interaction.request" => return Ok(None),
            "interaction.resolved" | "interaction.cancelled" | "interaction.dismissed" => {
                return Ok(Some(event));
            }
            kind if kind.starts_with("interaction.") => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

pub(super) fn response(record: &Value, card: &Value) -> Result<Option<Value>> {
    if string(card,"source")=="codex_async" {
        codex_questions::response(record,card)
    } else if string(record,"agent")=="claude" && string(card,"source")=="hook" {
        claude_question::response(record,card)
    } else {
        wire_response(record,string(card,"question_id"))
    }
}

/// The pending questions of an inspection, and whether they are complete.
/// The Codex question index works only until `deadline` and keeps its
/// progress; until it is complete, no Codex question is listed or offered
/// and the list is not complete (zerus-a4mub.8).
pub(super) fn inspection(
    record: &Value,
    live_pane: bool,
    deadline: std::time::Instant,
) -> Result<(Value, bool)> {
    if !live_pane || !process_alive(record) {
        return Ok((json!([]), true));
    }
    kimi_trust::fit(record);
    match collect(record, Some(deadline), true, true).map(Option::unwrap_or_default) {
        Ok((mut pending, complete)) => {
            // The same complete index decides whether a Codex question is unique.
            let codex: Vec<Value> = pending
                .iter()
                .filter(|c| string(c, "source") == "codex_async")
                .cloned()
                .collect();
            for card in &mut pending {
                if string(card,"source")=="codex_async" {
                    card["can_skip"]=json!(card["answer_delivery"]["status"] != "submitted");
                    codex_questions::fit(record,card);
                    match codex_questions::available_among(record,card,&codex) {
                        Ok(())=>{card["can_answer"]=json!(true);card["answer_transport"]=json!("codex_tui");card["answer_unavailable_reason"]=json!("");}
                        Err(error)=>card["answer_unavailable_reason"]=json!(error),
                    }
                }
                if string(record,"agent")=="claude" && string(card,"source")=="hook" {
                    match answer_channel::route(record, card) {
                        // Claude's own hook waits beside the panel; the screen is not needed.
                        Ok(answer_channel::Route::Channel(_)) => { card["can_answer"]=json!(true); card["answer_transport"]=json!("claude_hook"); card["answer_unavailable_reason"]=json!(""); }
                        // An answer already belongs to the channel: never offer a second one.
                        Ok(answer_channel::Route::Locked) => { card["can_answer"]=json!(false); card["answer_transport"]=json!(""); card["answer_unavailable_reason"]=json!(answer_channel::LOCKED); }
                        Ok(answer_channel::Route::Screen) => {
                            claude_question::fit(record, card);
                            match claude_question::available(record,card) {
                                Ok(()) => { card["can_answer"]=json!(true); card["answer_transport"]=json!("claude_tui"); card["answer_unavailable_reason"]=json!(""); }
                                Err(error) => card["answer_unavailable_reason"]=json!(error),
                            }
                        }
                        Err(error) => card["answer_unavailable_reason"]=json!(error),
                    }
                }
                if string(record, "agent") == "kimi" && string(card, "source") == kimi_plan::SOURCE {
                    match kimi_plan::available(record, card) {
                        Ok(()) => {
                            card["can_answer"] = json!(true);
                            card["answer_transport"] = json!("kimi_tui");
                            card["answer_unavailable_reason"] = json!("");
                        }
                        Err(error) => card["answer_unavailable_reason"] = json!(error),
                    }
                }
                if string(record, "agent") == "kimi" && string(card, "source") == kimi_approval::SOURCE {
                    kimi_approval::fit(record, card);
                    match kimi_approval::available(record, card) {
                        Ok(()) => {
                            card["can_answer"] = json!(true);
                            card["answer_transport"] = json!("kimi_tui");
                            card["answer_unavailable_reason"] = json!("");
                        }
                        Err(error) => card["answer_unavailable_reason"] = json!(error),
                    }
                }
                if string(record, "agent") == "kimi" && string(card, "source") == "kimi_wire" {
                    super::question_terminal::fit(record, card);
                    match super::question_terminal::available(record, card) {
                        Ok(()) => {
                            card["can_answer"] = json!(true);
                            card["answer_transport"] = json!("kimi_tui");
                            card["answer_unavailable_reason"] = json!("");
                        }
                        Err(error) => card["answer_unavailable_reason"] = json!(error),
                    }
                }
            }
            Ok((json!(pending), complete))
        }
        Err(error) => Err(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerRequest {
    request_id: String,
    expected_run_id: String,
    expected_conversation_id: String,
    question_id: String,
    expected_question_hash: String,
    answers: Vec<Answer>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    question_id: String,
    #[serde(default)]
    selected_option_ids: Vec<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    skip: bool,
}

fn validated_answers(card: &Value, answers: &[Answer]) -> Result<Value> {
    let questions = card["questions"]
        .as_array()
        .ok_or("invalid pending question")?;
    if answers.len() != questions.len() {
        return Err("answer every question before submitting".into());
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for question in questions {
        let id = string(question, "id");
        let answer = answers
            .iter()
            .find(|answer| answer.question_id == id)
            .ok_or("missing answer for a question")?;
        if !seen.insert(&answer.question_id)
            || answers
                .iter()
                .filter(|answer| answer.question_id == id)
                .count()
                != 1
        {
            return Err("duplicate question answer".into());
        }
        if answer.skip {
            if card["optional"] != true || string(card,"source")!="codex_async" || !answer.text.is_empty() || !answer.selected_option_ids.is_empty() {
                return Err("only optional questions can be skipped without an answer".into());
            }
            normalized.push(json!({"question_id":id,"skip":true,"selected_option_ids":[],"text":""}));
            continue;
        }
        if answer.text.len() > 4096 || answer.text.chars().any(char::is_control) {
            return Err("Other answers must be a single line of at most 4096 UTF-8 bytes".into());
        }
        let other = answer.text.trim();
        if other.starts_with(['/', '!']) {
            return Err(
                "Other answers beginning with a command must be entered in Terminal".into(),
            );
        }
        let options = question["options"]
            .as_array()
            .ok_or("invalid question options")?;
        let mut selected = BTreeSet::new();
        for id in &answer.selected_option_ids {
            if !options.iter().any(|option| string(option, "id") == id) || !selected.insert(id) {
                return Err("unknown or duplicate answer option".into());
            }
        }
        if selected.is_empty() && other.is_empty() {
            return Err("choose an option or write an Other answer for every question".into());
        }
        if question["multi_select"] != true && selected.len() + usize::from(!other.is_empty()) != 1
        {
            return Err("this question accepts only one answer".into());
        }
        if !other.is_empty() && question["allow_other"] != true {
            return Err("this question does not accept an Other answer".into());
        }
        normalized.push(
            json!({"question_id":id,"selected_option_ids":answer.selected_option_ids,"text":other}),
        );
    }
    Ok(json!(normalized))
}

fn startup_question_id(id: &str) -> bool {
    ["codex-hooks-trust:", "codex-update:", "codex-goal-resume:", "codex-trust:", "kimi-trust:", "claude-trust:", "claude-permissions:", "claude-resume:", "claude-usage-limit:"].iter().any(|prefix| id.starts_with(prefix))
}

fn matching_record(name: &str, request: &AnswerRequest) -> Result<Value> {
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    let record = read(name)?;
    terminating::check_mutation(&record)?;
    if string(&record, "run_id") != request.expected_run_id
        || string(&record, "conversation_id") != request.expected_conversation_id
    {
        return Err("session identity changed; refresh before answering".into());
    }
    if !matches(&record, panes_of(&live()?, &record))
        || !process_alive(&record)
        || pause_active(&record)
        || !string(&record, "error").is_empty()
        || (!string(&record, "expected_id").is_empty()
            && !startup_question_id(&request.question_id))
    {
        return Err("the exact agent is no longer available for this question".into());
    }
    Ok(record)
}

fn response_matches(question: &Value, answers: &Value, event: &Value) -> bool {
    if string(question, "source") == kimi_plan::SOURCE {
        return kimi_plan::response_matches(question, answers, event);
    }
    if string(question, "source") == kimi_approval::SOURCE {
        return kimi_approval::response_matches(question, answers, event);
    }
    if string(event, "type") != "interaction.resolved" {
        return false;
    }
    let response = &event["response"];
    let response = response.get("answers").unwrap_or(response);
    let Some(actual) = response.as_object() else {
        return false;
    };
    let Some(items) = question["questions"].as_array() else {
        return false;
    };
    let Some(answers) = answers.as_array() else {
        return false;
    };
    if actual.len() != items.len() {
        return false;
    }
    items.iter().zip(answers).all(|(question, answer)| {
        actual
            .get(string(question, "question"))
            .and_then(Value::as_str)
            == Some(answer_text(question, answer).as_str())
    })
}

/// The text Claude records for one validated answer: the chosen labels in
/// source option order, then any Other text, joined by ", ".
pub(super) fn answer_text(question: &Value, answer: &Value) -> String {
    let selected = answer["selected_option_ids"].as_array();
    let mut labels: Vec<&str> = question["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|option| selected.is_some_and(|ids| ids.contains(&option["id"])))
        .map(|option| string(option, "label"))
        .collect();
    if !string(answer, "text").is_empty() {
        labels.push(string(answer, "text"));
    }
    labels.join(", ")
}

fn log_answer(record: &Value, question: &Value, response: &Value) -> Result<()> {
    // Keep every question represented even when a custom answer is long. The
    // full chosen text remains in the private receipt and provider transcript.
    let summary = question["questions"]
        .as_array()
        .ok_or("missing questions")?
        .iter()
        .map(|item| {
            let answer = &response["response"]["answers"][string(item, "question")];
            format!(
                "{} → {}",
                journal::clipped(&item["question"], 120),
                journal::clipped(answer, 150)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    journal::log_event(
        record,
        &json!({"hook_event_name":"QuestionAnswered",
        "tool_name":"AskUserQuestion","question_id":question["question_id"],
        "question_hash":question["question_hash"],"tool_call_id":question["tool_call_id"],
        "at":response["time"].as_f64().unwrap_or(0.0)/1000.0,"body":summary}),
    )
}

/// The lock every answer to run `run_id` holds while it drives the terminal.
/// Fitting a clipped panel (`question_terminal::fit_unviewed`) takes it too.
pub(super) fn answer_lock(run_id: &str) -> Result<PathBuf> {
    Ok(absolute_root()?
        .join("question_locks")
        .join(format!("{:x}.lock", Sha256::digest(run_id.as_bytes()))))
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    if args.len() == 3 && args[1] == "--observe" && args[2] == "--json" {
        return observe_dispatch(&args[0]);
    }
    if args.len() != 2 || args[1] != "--json" {
        return Err("usage: hgs answer <session> --json (JSON from stdin)".into());
    }
    let mut input = Vec::new();
    io::stdin()
        .take(MAX_PAYLOAD_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|error| error.to_string())?;
    if input.len() as u64 > MAX_PAYLOAD_BYTES {
        return Err("answer payload exceeds128 KiB".into());
    }
    let request: AnswerRequest = serde_json::from_slice(&input)
        .map_err(|error| format!("invalid answer payload: {error}"))?;
    let request_id = uuid::Uuid::parse_str(&request.request_id)
        .map_err(|_| "request_id must be a UUID")?
        .to_string();
    if request.expected_run_id.is_empty()
        || (request.expected_conversation_id.is_empty()
            && !startup_question_id(&request.question_id))
        || request.expected_question_hash.len() != 64
    {
        return Err("question identities are required; refresh before answering".into());
    }
    let digest = format!("{:x}", Sha256::digest(&input));
    let receipt_path = absolute_root()?
        .join("question_receipts")
        .join(format!("{request_id}.json"));
    let name = &args[0];
    // Serialize all answers to this run, including requests arriving through
    // different hosts or carrying different UUIDs. Provider hooks never take
    // this lock, and must remain free to acknowledge completion.
    let _answer_guard = lock(Some(&answer_lock(&request.expected_run_id)?))?;
    // Only the preparation is locked. Awaiting the UI must not block provider
    // hooks which confirm the very answer that we are waiting for.
    let (record, question, answers, mut receipt) = {
        let _guard = lock(None)?;
        if receipt_path.exists() {
            let contents = fs::read_to_string(&receipt_path).map_err(|error| error.to_string())?;
            let receipt: Value =
                serde_json::from_str(&contents).map_err(|error| error.to_string())?;
            if string(&receipt, "digest") != digest || string(&receipt, "name") != name {
                return Err("answer request ID was already used for different content".into());
            }
            if receipt["status"] == "answered" || receipt["status"] == "submitted" {
                println!("{contents}");
                return Ok(0);
            }
            if receipt["status"] == "rejected" {
                return Err(string(&receipt, "error").to_owned());
            }
            return Err("delivery uncertain: this answer request may already be in Terminal; do not resend automatically".into());
        }
        let record = matching_record(name, &request)?;
        let question = current(&record)?
            .into_iter()
            .find(|question| string(question, "question_id") == request.question_id)
            .ok_or("question is no longer pending; refresh before answering")?;
        if string(&question, "question_hash") != request.expected_question_hash {
            return Err("question changed; refresh before answering".into());
        }
        if !(["codex_hooks_trust", "codex_update", "codex_goal_resume", "codex_rate_limit", "codex_folder_trust", "codex_async", "kimi_wire", kimi_plan::SOURCE, kimi_approval::SOURCE, "kimi_cache_hint", "kimi_folder_trust", "claude_folder_trust", "claude_permission_mode", "claude_resume", "claude_usage_limit", "claude_tool_approval"]
            .contains(&string(&question, "source"))
            || string(&record,"agent")=="claude" && string(&question,"source")=="hook")
        {
            return Err("this question has no native response identity; open Terminal".into());
        }
        let answers = validated_answers(&question, &request.answers)?;
        if string(&question,"source")=="codex_async" {
            if question["answer_delivery"]["status"] == "submitted" {
                return Err("An answer was already submitted. Wait for Codex to record it.".into());
            }
            if answers[0]["skip"]!=true {codex_questions::available(&record,&question)?;}
        } else if string(&record,"agent")=="claude" && string(&question,"source")=="hook" {
            match answer_channel::route(&record, &question)? {
                answer_channel::Route::Channel(_) => {}
                answer_channel::Route::Locked => return Err(answer_channel::LOCKED.into()),
                answer_channel::Route::Screen => claude_question::available(&record,&question)?,
            }
        } else if string(&question, "source") == "claude_tool_approval" {
            claude_approval::available(&record,&question)?;
        } else if string(&question, "source") == "claude_permission_mode" {
            claude_permission::available(&record, &question)?;
        } else if string(&question, "source") == "claude_resume" {
            claude_resume::available(&record, &question)?;
        } else if string(&question, "source") == "claude_usage_limit" {
            claude_usage_limit::available(&record, &question)?;
        } else if string(&question, "source") == "claude_folder_trust" {
            claude_trust::available(&record, &question)?;
        } else if string(&question, "source") == "codex_hooks_trust" {
            codex_hooks_trust::available(&record, &question)?;
        } else if string(&question, "source") == "codex_update" {
            codex_update::available(&record, &question)?;
        } else if string(&question, "source") == "codex_rate_limit" {
            codex_rate_limit::available(&record, &question)?;
        } else if string(&question, "source") == "codex_goal_resume" {
            codex_goal_resume::available(&record, &question)?;
        } else if string(&question, "source") == "codex_folder_trust" {
            codex_trust::available(&record, &question)?;
        } else if string(&question, "source") == "kimi_folder_trust" {
            kimi_trust::available(&record, &question)?;
        } else if string(&question, "source") == "kimi_cache_hint" {
            kimi_cache_hint::available(&record, &question)?;
        } else if string(&question, "source") == kimi_plan::SOURCE {
            kimi_plan::available(&record, &question)?;
        } else if string(&question, "source") == kimi_approval::SOURCE {
            kimi_approval::available(&record, &question)?;
        } else {
            super::question_terminal::available(&record, &question)?;
        }
        let receipt = json!({"status":"in_progress","request_id":request_id,"name":name,
            "run_id":request.expected_run_id,"conversation_id":request.expected_conversation_id,
            "question_id":request.question_id,"question_hash":request.expected_question_hash,"digest":digest,
            "submitted_at":now(),"answers":answers});
        receipt_index::persist(&receipt_path, &receipt)?;
        (record, question, answers, receipt)
    };
    let tui_choice =
        ["codex_hooks_trust", "codex_update", "codex_goal_resume", "codex_rate_limit", "codex_folder_trust", "kimi_cache_hint", "kimi_folder_trust", "claude_folder_trust", "claude_permission_mode", "claude_resume", "claude_usage_limit", "claude_tool_approval"].contains(&string(&question, "source"));
    if string(&question,"source")=="codex_async" {
        let status = match codex_questions::answer(&record,&question,&answers,&request_id) {
            Ok(status) => status,
            Err(error) => {
                if !error.contains("delivery uncertain") {receipt["status"]=json!("rejected");receipt["error"]=json!(error);let _=receipt_index::persist(&receipt_path,&receipt);}
                return Err(error);
            }
        };
        receipt["status"]=json!(status);receipt["skipped"]=json!(answers[0]["skip"]==true);
        if status == "answered" { receipt["answered_at"]=json!(now()); }
        receipt_index::persist(&receipt_path,&receipt).map_err(|e|format!("delivery uncertain: {e}"))?;
        println!("{receipt}");return Ok(0);
    }
    // Update now says whether Codex still runs its installer.
    let mut update_phase = None;
    let delivery = if string(&record,"agent")=="claude" && string(&question,"source")=="hook" {
        let screen = || {
            claude_question::available(&record, &question)?;
            claude_question::answer(&record, &question, &answers)
        };
        match answer_channel::route(&record, &question) {
            Ok(answer_channel::Route::Channel(dir)) => {
                let offer = json!({"request_id":request_id,"answers":answer_channel::native_answers(&question,&answers)});
                answer_channel::deliver(&dir, &offer, Duration::from_secs(3), Duration::from_secs(3), screen)
                    .map(|transport| receipt["transport"] = json!(transport))
            }
            // Another request's answer reached the channel since preparation: nothing was sent.
            Ok(answer_channel::Route::Locked) => Err(answer_channel::LOCKED.into()),
            Ok(answer_channel::Route::Screen) => screen().map(|()| receipt["transport"] = json!("claude_tui")),
            Err(error) => Err(error),
        }
    } else if string(&question, "source") == "claude_tool_approval" {
        claude_approval::answer(&record,&question,&answers,&request_id)
            .map(|transport| receipt["transport"] = json!(transport))
    } else if string(&question, "source") == "claude_permission_mode" {
        claude_permission::answer(&record, &question, &answers)
    } else if string(&question, "source") == "claude_resume" {
        claude_resume::answer(&record, &question, &answers)
    } else if string(&question, "source") == "claude_usage_limit" {
        claude_usage_limit::answer(&record, &question, &answers)
    } else if string(&question, "source") == "claude_folder_trust" {
        claude_trust::answer(&record, &question, &answers)
    } else if string(&question, "source") == "codex_hooks_trust" {
        codex_hooks_trust::answer(&record, &question, &answers)
    } else if string(&question, "source") == "codex_update" {
        codex_update::answer(&record, &question, &answers).map(|phase| update_phase = phase)
    } else if string(&question, "source") == "codex_rate_limit" {
        codex_rate_limit::answer(&record, &question, &answers)
    } else if string(&question, "source") == "codex_goal_resume" {
        codex_goal_resume::answer(&record, &question, &answers)
    } else if string(&question, "source") == "codex_folder_trust" {
        codex_trust::answer(&record, &question, &answers)
    } else if string(&question, "source") == "kimi_folder_trust" {
        kimi_trust::answer(&record, &question, &answers)
    } else if string(&question, "source") == "kimi_cache_hint" {
        kimi_cache_hint::answer(&record, &question, &answers)
    } else if string(&question, "source") == kimi_plan::SOURCE {
        kimi_plan::answer(&record, &question, &answers)
    } else if string(&question, "source") == kimi_approval::SOURCE {
        kimi_approval::answer(&record, &question, &answers)
    } else {
        super::question_terminal::answer(&record, &question, &answers)
    };
    if let Err(error) = delivery {
        if !error.contains("delivery uncertain") {
            receipt["status"] = json!("rejected");
            receipt["error"] = json!(error);
            // No terminal input was sent, so even a failed receipt update
            // cannot turn this definite rejection into a possible delivery.
            let _ = receipt_index::persist(&receipt_path, &receipt);
        }
        return Err(error);
    }
    if tui_choice {
        // Hook review and the native installer continue in Terminal.
        if (question["source"] == "codex_hooks_trust" && answers[0]["selected_option_ids"][0] == "review")
            || (question["source"] == "codex_update" && answers[0]["selected_option_ids"][0] == "update_now")
        {
            receipt["open_terminal"] = json!(true);
        }
        if let Some(phase) = update_phase {
            receipt["codex_update"] = json!(phase);
        }
        let id = answers[0]["selected_option_ids"][0].as_str().unwrap_or("");
        let item = &question["questions"][0];
        let label = item["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| string(o, "id") == id)
            .map(|o| string(o, "label"))
            .unwrap_or("");
        // A denial with feedback records the person's own text as a denial.
        let feedback;
        let label = if label.is_empty() && !string(&answers[0], "text").is_empty() {
            feedback = format!("Denied: {}", string(&answers[0], "text"));
            feedback.as_str()
        } else {
            label
        };
        if question["source"] == "claude_usage_limit" && ["Switch to usage credits", "Switch to extra usage"].contains(&label) {
            receipt["open_terminal"] = json!(true);
        }
        let response =
            json!({"time":now()*1000.,"response":{"answers":{string(item,"question"):label}}});
        log_answer(&record, &question, &response)
            .map_err(|e| format!("delivery uncertain: {e}"))?;
        receipt["status"] = json!("answered");
        receipt["answered_at"] = json!(now());
        receipt_index::persist(&receipt_path, &receipt)
            .map_err(|e| format!("delivery uncertain: {e}"))?;
        println!("{receipt}");
        return Ok(0);
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(response) = response(&record, &question).map_err(|error| {
            format!("delivery uncertain: cannot read provider acknowledgement: {error}")
        })? {
            if !response_matches(&question, &answers, &response) {
                return Err("delivery uncertain: provider resolved this question with a different answer; inspect Terminal".into());
            }
            let response = if string(&question, "source") == kimi_plan::SOURCE {
                kimi_plan::journal_response(&question, &response)
            } else if string(&question, "source") == kimi_approval::SOURCE {
                kimi_approval::journal_response(&question, &response)
            } else {
                response
            };
            log_answer(&record, &question, &response).map_err(|error| {
                format!(
                    "delivery uncertain: answer confirmed but journal could not be saved: {error}"
                )
            })?;
            receipt["status"] = json!("answered");
            receipt["answered_at"] = json!(now());
            receipt_index::persist(&receipt_path, &receipt)
                .map_err(|error| format!("delivery uncertain: {error}"))?;
            println!("{receipt}");
            return Ok(0);
        }
        if Instant::now() >= deadline {
            return Err("delivery uncertain: provider has not confirmed the answer; inspect Terminal before retrying".into());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_text_matches_response_format() {
        let card = json!({"questions": normalize(&json!([
            {"question":"Which checks?","header":"Checks","multiSelect":true,
                "options":[{"label":"Unit"},{"label":"Integration"},{"label":"UI"}]},
            {"question":"Anything else?","header":"Notes","options":[{"label":"Nothing"},{"label":"Later"}]}])).unwrap()});
        let answers = json!([
            {"question_id":"q_0","selected_option_ids":["opt_0_2","opt_0_0"],"text":"Lint, quickly"},
            {"question_id":"q_1","selected_option_ids":[],"text":"Ship it"}]);
        let items = card["questions"].as_array().unwrap();
        // Source option order, then Other: what Claude's own panel records.
        assert_eq!(answer_text(&items[0], &answers[0]), "Unit, UI, Lint, quickly");
        assert_eq!(answer_text(&items[1], &answers[1]), "Ship it");
        let event = json!({"type":"interaction.resolved","response":{"answers":{
            "Which checks?":answer_text(&items[0], &answers[0]),"Anything else?":answer_text(&items[1], &answers[1])}}});
        assert!(response_matches(&card, &answers, &event));
    }
    /// Only a question listing may grow an unviewed pane to fit a clipped
    /// Claude approval (`claude_approval::listed`); guards read the screen as
    /// it is. A deadline or a partial Codex index must never turn a listing
    /// into a guard: batch 5 once dropped pane fitting from inspection() so.
    #[test]
    fn listings_fit_clipped_claude_approvals_and_guards_do_not() {
        use super::super::claude_approval::LAST_FIT;
        let pid = std::process::id();
        // Idle and without a supervisor: every reader stays off the terminal
        // and the state root, and the approval read still records its fit.
        let record = json!({"agent":"claude","run_id":"run","conversation_id":"conversation","phase":"idle",
            "pid":pid,"process_start":super::super::storage::process_start(pid)});
        let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(5);
        let fit = |read: &dyn Fn()| {
            LAST_FIT.with(|last| last.set(None));
            read();
            LAST_FIT.with(std::cell::Cell::get)
        };
        assert_eq!(
            fit(&|| drop(inspection(&record, true, deadline()).unwrap())),
            Some(true),
            "inspection() is a listing"
        );
        assert_eq!(
            fit(&|| drop(current(&record).unwrap())),
            Some(true),
            "current() is a listing"
        );
        assert_eq!(
            fit(&|| drop(current_until(&record, deadline()).unwrap())),
            Some(false),
            "current_until() is a guard"
        );
    }
    fn record() -> Value {
        json!({"agent":"kimi","run_id":"run","conversation_id":"session_test","active_tools":{"tool_test":{"name":"AskUserQuestion"}}})
    }
    fn items() -> Value {
        json!([{"question":"What should we do?","header":"Task","options":[{"label":"Build","description":"Create it"},{"label":"Review"}]}])
    }
    fn event() -> Value {
        json!({"type":"interaction.request","id":"question_test","kind":"question","agentId":"main","toolCallId":"tool_test","request":{"questions":items()},"time":1000})
    }
    #[test]
    fn normalizes_hooks_and_recovers_exact_wire_identity() {
        let mut record = record();
        let hook = json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_call_id":"tool_test","tool_input":{"questions":items()}});
        observe(&mut record, &hook);
        assert_eq!(
            record["pending_questions"]["tool_test"]["questions"][0]["options"][0]["id"],
            "opt_0_0"
        );
        let recovered = recover_wire(&record, &[event()]);
        assert_eq!(recovered["question_test"]["question_id"], "question_test");
        assert_eq!(recovered["question_test"]["created_at"], 1.0);
        assert_eq!(
            recovered["question_test"]["questions"][0]["allow_other"],
            true
        );
        observe(
            &mut record,
            &json!({"hook_event_name":"PostToolUse","tool_call_id":"tool_test"}),
        );
        assert_eq!(record["pending_questions"], json!({}));
    }
    #[test]
    fn a_new_prompt_resolves_the_previous_permission_request() {
        let mut record = record();
        observe(&mut record, &json!({"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion"}));
        assert!(record["pending_approval"].is_object());
        // Claude accepts a prompt only after its chooser closed, even when
        // no other hook reported how it was answered.
        observe(&mut record, &json!({"hook_event_name":"UserPromptSubmit","prompt":"next"}));
        assert!(record.get("pending_approval").is_none() && record.get("pending_approval_at").is_none());
    }
    #[test]
    fn ignores_resolved_or_inactive_questions_and_rejects_bad_choices() {
        assert!(recover_wire(
            &record(),
            &[
                event(),
                json!({"type":"interaction.resolved","id":"question_test"})
            ]
        )
        .is_empty());
        let mut stopped = record();
        stopped["active_tools"] = json!({});
        assert!(recover_wire(&stopped, &[event()]).is_empty());
        let card = card(
            &record(),
            "question_test",
            "tool_test",
            &items(),
            "kimi_wire",
            1.0,
        )
        .unwrap();
        let good = Answer { skip:false,
            question_id: "q_0".into(),
            selected_option_ids: vec!["opt_0_0".into()],
            text: String::new(),
        };
        assert!(validated_answers(&card, &[good]).is_ok());
        let bad = Answer { skip:false,
            question_id: "q_0".into(),
            selected_option_ids: vec!["opt_0_9".into()],
            text: String::new(),
        };
        assert!(validated_answers(&card, &[bad]).is_err());
        let multiline = Answer { skip:false,
            question_id: "q_0".into(),
            selected_option_ids: vec![],
            text: "one\ntwo".into(),
        };
        assert!(validated_answers(&card, &[multiline]).is_err());
    }
    #[test]
    fn kimi_tool_approvals_on_the_main_wire_are_listed() {
        // No terminal: every screen-based card source sees a capture error.
        struct Silent;
        impl backend::TerminalBackend for Silent {}
        let home = tempfile::tempdir().unwrap();
        let wire = home.path().join("sessions/workspace/session_test/agents/main");
        fs::create_dir_all(&wire).unwrap();
        let request = json!({"type":"interaction.request","agentId":"main","id":"approval_test","kind":"approval",
            "toolCallId":"tool_bash","request":{"id":"approval_test","sessionId":"session_test","agentId":"main","turnId":1,
            "toolCallId":"tool_bash","toolName":"Bash","action":"Running: ls -la",
            "display":{"kind":"command","command":"ls -la","cwd":"/workspace/example","language":"bash"}},"time":2000});
        fs::write(wire.join("wire.jsonl"), format!("{request}\n")).unwrap();
        let record = json!({"agent":"kimi","agent_home":home.path(),"run_id":"run","conversation_id":"session_test",
            "active_tools":{"tool_bash":{"name":"Bash"}}});
        let cards = backend::with_fake(&Silent, || current(&record)).unwrap();
        let approvals: Vec<_> = cards.iter().filter(|card| card["source"] == "kimi_tool_approval").collect();
        assert_eq!(approvals.len(), 1, "{cards:?}");
        assert_eq!((approvals[0]["question_id"].clone(), approvals[0]["tool_call_id"].clone()),
            (json!("approval_test"), json!("tool_bash")));
    }
    #[test]
    fn a_reused_request_id_never_acknowledges_the_new_request_with_the_old_resolution() {
        let home = tempfile::tempdir().unwrap();
        let wire = home.path().join("sessions/workspace/session_test/agents/main");
        fs::create_dir_all(&wire).unwrap();
        let path = wire.join("wire.jsonl");
        let request = json!({"type":"interaction.request","agentId":"main","id":"approval_reused"});
        let old = json!({"type":"interaction.resolved","agentId":"main","id":"approval_reused","response":{"selectedLabel":"old"}});
        let new = json!({"type":"interaction.resolved","agentId":"main","id":"approval_reused","response":{"selectedLabel":"new"}});
        let record = json!({"agent":"kimi","agent_home":home.path(),"run_id":"run","conversation_id":"session_test"});
        fs::write(&path, format!("{request}\n{old}\n{request}\n")).unwrap();
        assert!(wire_response(&record, "approval_reused").unwrap().is_none());
        fs::write(&path, format!("{request}\n{old}\n{request}\n{new}\n")).unwrap();
        assert_eq!(
            wire_response(&record, "approval_reused").unwrap().unwrap()["response"]["selectedLabel"],
            "new"
        );
        fs::write(&path, format!("{request}\n{old}\n{request}\n{}\n", json!({"type":"interaction.unknown","agentId":"main","id":"approval_reused"}))).unwrap();
        assert!(wire_response(&record, "approval_reused").unwrap().is_none());
    }

    #[test]
    fn acknowledgement_must_contain_exact_answers() {
        let card = card(
            &record(),
            "question_test",
            "tool_test",
            &items(),
            "kimi_wire",
            1.0,
        )
        .unwrap();
        let answers = json!([{"question_id":"q_0","selected_option_ids":["opt_0_0"],"text":""}]);
        assert!(response_matches(
            &card,
            &answers,
            &json!({"type":"interaction.resolved","response":{"answers":{"What should we do?":"Build"}}})
        ));
        assert!(!response_matches(
            &card,
            &answers,
            &json!({"type":"interaction.resolved","response":{"answers":{"What should we do?":"Review"}}})
        ));
    }
}
