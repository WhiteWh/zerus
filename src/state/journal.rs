use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::Digest;
use std::fs::OpenOptions;
use std::sync::OnceLock;

/// Final replies are the conversation itself, not an excerpt: Activity reads
/// them from the journal once the provider transcript window has moved on.
pub(super) const REPLY_LIMIT: usize = 32_000;

/// Claude records text pasted into its prompt as
/// `<pasted_content id="…">\n…\n</pasted_content id="…">`. The person sent only
/// the inner text, which is also what HGS delivery receipts recorded. Claude
/// 2.1.296 also frames a prompt that is only pasted text with blank lines
/// around it; that framing is not text the person sent either.
pub(super) fn pasted_text(text: &str) -> std::borrow::Cow<'_, str> {
    const OPEN: &str = "<pasted_content id=\"";
    if !text.contains(OPEN) {
        return text.into();
    }
    let (mut result, mut rest) = (String::with_capacity(text.len()), text);
    let (mut typed, mut lead) = (false, None);
    while let Some(start) = rest.find(OPEN) {
        let Some((id, body)) = rest[start + OPEN.len()..].split_once("\">") else { break };
        if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
            break;
        }
        let close = format!("</pasted_content id=\"{id}\">");
        let Some(end) = body.find(&close) else { break };
        let inner = body[..end].strip_prefix('\n').unwrap_or(&body[..end]);
        typed |= !rest[..start].trim().is_empty();
        lead.get_or_insert(start);
        result.push_str(&rest[..start]);
        result.push_str(inner.strip_suffix('\n').unwrap_or(inner));
        rest = &body[end + close.len()..];
    }
    if rest.len() == text.len() {
        return text.into();
    }
    if !typed && rest.trim().is_empty() {
        return result.split_off(lead.unwrap_or(0)).into();
    }
    result.push_str(rest);
    result.into()
}

pub(super) fn clipped(value: &Value, limit: usize) -> String {
    let Some(value) = value.as_str() else {
        return String::new();
    };
    static ANSI: OnceLock<regex::Regex> = OnceLock::new();
    let stripped = ANSI
        .get_or_init(|| regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap())
        .replace_all(value, "");
    let mut characters = stripped
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || *c >= ' ');
    let mut result: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

/// WhiteWh fork: the start of the desktop's cache keepalive message.
pub(crate) const CACHE_KEEPALIVE: &str = "<zerus-cache-keepalive>";

/// Claude hands finished background work and other sessions' messages to the
/// agent as a "user" prompt made of XML envelopes the person never wrote: task
/// notifications, subagent reports and cross-session messages. Several may
/// arrive together, and a journal excerpt may be clipped.
pub(super) fn system_prompt(text: &str) -> Option<Value> {
    // WhiteWh fork: the desktop keeps an idle conversation's prompt cache warm
    // with this envelope; it is neither the person's message nor a reply.
    if text.starts_with(CACHE_KEEPALIVE) {
        return Some(json!({"kind":"cache_keepalive","summary":"Cache warmed","status":"","report":"","agent_id":"","sender":""}));
    }
    // This complete XML subset is a declared sender label, never authentication.
    // Keep Claude's native envelope parser below unchanged.
    if text.starts_with("<zerus-message") {
        let (sender, reference, body) = crate::session_reference::sender_message(text)?;
        return Some(json!({"kind":"peer_message","summary":"","status":"","report":body,
            "agent_id":"","sender":sender,"sender_reference":reference}));
    }
    fn envelope<'a>(body: &'a str, close: &str) -> Option<(&'a str, &'a str)> {
        match body.find(close) {
            Some(end) => Some((&body[..end], body[end + close.len()..].trim_start())),
            None if body.ends_with('…') => Some((body, "")),
            None => None,
        }
    }
    let mut rest = text.trim();
    let (mut summaries, mut status, mut reports, mut agent) = (Vec::new(), String::new(), Vec::new(), String::new());
    let (mut messages, mut sender) = (Vec::new(), String::new());
    while !rest.is_empty() {
        if let Some(body) = rest.strip_prefix("<task-notification>") {
            let (inner, after) = envelope(body, "</task-notification>")?;
            let field = |tag: &str| {
                let value = inner.split_once(&format!("<{tag}>"))?.1.split_once(&format!("</{tag}>"))?.0;
                Some(["&lt;", "&gt;", "&quot;", "&apos;", "&amp;"].iter().zip(["<", ">", "\"", "'", "&"])
                    .fold(value.trim().to_owned(), |text, (from, to)| text.replace(from, to)))
            };
            status = field("status").unwrap_or_default();
            summaries.push(field("summary").filter(|s| !s.is_empty()).unwrap_or_else(|| {
                if status.is_empty() { "Background task finished".into() } else { format!("Background task {status}") }
            }));
            rest = after;
        } else if let Some(body) = rest.strip_prefix("<agent-message from=\"") {
            let (from, body) = body.split_once("\">")?;
            if from.is_empty() || from.len() > 128 || !from.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
                return None;
            }
            let (inner, after) = envelope(body, "</agent-message>")?;
            // A hand-back starts with Claude's framing paragraph and indents
            // every line of the report itself.
            const FRAME: &str = "The report follows:\n";
            let inner = inner.trim_start_matches('\n');
            let report = match inner.find(FRAME) {
                Some(at) if inner.starts_with('[') => &inner[at + FRAME.len()..],
                _ => inner,
            };
            reports.push(report.lines().map(|line| line.strip_prefix("  ").unwrap_or(line)).collect::<Vec<_>>().join("\n").trim().to_owned());
            if agent.is_empty() { agent = from.to_owned(); }
            rest = after;
        } else {
            // Another local Claude session wrote to this one; the user did not.
            let body = rest.strip_prefix("<cross-session-message ")?;
            let (attributes, body) = body.split_once('>')?;
            let attribute = |name: &str| attributes.split_once(&format!("{name}=\""))
                .and_then(|(_, value)| value.split_once('"')).map(|(value, _)| value.trim());
            let name = attribute("from-name").filter(|n| !n.is_empty()).or_else(|| attribute("from")).unwrap_or_default();
            if name.is_empty() || name.len() > 200 {
                return None;
            }
            let (inner, after) = envelope(body, "</cross-session-message>")?;
            messages.push(inner.trim().to_owned());
            if sender.is_empty() { sender = name.to_owned(); }
            rest = after;
        }
    }
    let kind = if !reports.is_empty() { "subagent_report" } else if !messages.is_empty() { "peer_message" } else if !summaries.is_empty() { "task_notification" } else { return None };
    reports.extend(messages);
    Some(json!({"kind": kind, "summary": summaries.join("\n"), "status": status,
        "report": reports.join("\n\n"), "agent_id": agent, "sender": sender}))
}

/// Show background work handed to the agent as a notice rather than the
/// person's message, including journal rows written before HGS recognized it.
pub(super) fn mark_system_prompt(record: &Value, event: &mut Value, prompt: &str) {
    if !string(event, "agent_id").is_empty()
        || !["UserPromptSubmit", "UserPromptQueued", "TurnStarted"].contains(&string(event, "type"))
        || event["origin"].is_string()
    {
        return;
    }
    let text = if prompt.is_empty() { string(event, "detail") } else { prompt };
    let Some(notice) = system_prompt(text) else { return };
    let report = string(&notice, "report");
    let child = &record["subagents"][string(&notice, "agent_id")];
    // Name the subagent as Claude does in its terminal; older reports fall
    // back to their first line.
    let summary = if notice["kind"] == "peer_message" {
        format!("Message from {}", string(&notice, "sender"))
    } else if !string(&notice, "summary").is_empty() {
        string(&notice, "summary").to_owned()
    } else if !string(child, "description").is_empty() {
        format!("Agent \"{}\" finished", string(child, "description"))
    } else {
        report.lines().map(|line| line.trim_start_matches('#').trim()).find(|line| !line.is_empty()).unwrap_or("Subagent report").to_owned()
    };
    event["origin"] = notice["kind"].clone();
    event["detail"] = json!(clipped(&json!(summary), 1200));
    event["task_status"] = notice["status"].clone();
    if !report.is_empty() {
        event["report"] = json!(clipped(&notice["report"], 32000));
        event["from_agent"] = notice["agent_id"].clone();
    }
    if notice["kind"] == "peer_message" {
        event["sender"] = notice["sender"].clone();
        if !string(&notice, "sender_reference").is_empty() {
            event["sender_reference"] = notice["sender_reference"].clone();
        }
    }
}

/// The person's latest request. Older records may hold background work.
pub(super) fn user_prompt(record: &Value) -> &Value {
    static NONE: Value = Value::Null;
    if system_prompt(string(record, "prompt")).is_some() { &NONE } else { &record["prompt"] }
}

fn detail(event: &Value) -> String {
    if event["hook_event_name"] == "StopFailure" {
        for key in [
            "error_message",
            "error_details",
            "last_assistant_message",
            "error",
            "error_type",
        ] {
            if !string(event, key).is_empty() {
                return clipped(&event[key], 1200);
            }
        }
    }
    let tool = &event["tool_input"];
    [
        (&event["prompt"], 1200),
        (&event["error_message"], 1200),
        (&event["error"], 1200),
        (&tool["command"], 1200),
        (&tool["file_path"], 1200),
        (&tool["path"], 1200),
        (&tool["description"], 1200),
        (&event["last_assistant_message"], REPLY_LIMIT),
        (&event["response"], REPLY_LIMIT),
        (&event["body"], 1200),
        (&event["description"], 1200),
    ]
    .into_iter()
    .find(|(value, _)| value.as_str().map(|s| !s.is_empty()).unwrap_or(false))
    .map(|(value, limit)| clipped(value, limit))
    .unwrap_or_default()
}

pub(super) fn event_db() -> Result<Connection> {
    private_dir(&root())?;
    let path = root().join("events.sqlite3");
    crate::platform::private_open(
        OpenOptions::new().create(true).truncate(false).read(true).write(true),
    )
    .open(&path)
    .map_err(|e| e.to_string())?;
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY, name TEXT, conversation TEXT, payload TEXT);
        CREATE INDEX IF NOT EXISTS events_session ON events(name, conversation, seq);
        CREATE INDEX IF NOT EXISTS events_main_replies ON events(name, conversation, seq)
        WHERE json_extract(payload, '$.type') = 'Stop' AND json_extract(payload, '$.agent_id') = '';
        CREATE INDEX IF NOT EXISTS events_main_messages ON events(name, conversation, seq)
        WHERE COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main')
          AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop');").map_err(|e| e.to_string())?;
    Ok(db)
}

// Kimi names what opened a turn: the person, a scheduled job, a task...
fn turn_origin<'a>(record: &Value, event: &'a Value) -> Option<&'a str> {
    let origin = string(event, "origin_kind");
    (string(record, "agent") == "kimi"
        && string(event, "hook_event_name") == "TurnStarted"
        && (1..=40).contains(&origin.len())
        && origin.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    .then_some(origin)
}

fn kimi_turn_id(record: &Value, event: &Value) -> Option<u64> {
    if string(record, "agent") != "kimi" || !["TurnStarted", "Stop"].contains(&string(event, "hook_event_name")) {
        return None;
    }
    event["turn_id"].as_u64().filter(|id| *id <= i64::MAX as u64)
}

pub(super) fn log_event(record: &Value, event: &Value) -> Result<()> {
    // Kimi's heartbeat and Claude's model switch change state, not Activity;
    // the model selector shows the switch.
    if ["SessionHeartbeat", "PostModelSwitch"].contains(&string(event, "hook_event_name")) {
        return Ok(());
    }
    let mut data = json!({"type": event["hook_event_name"], "at": now(), "run_id": record["run_id"],
        "agent_id": clipped(&event["agent_id"], 160), "tool": clipped(&event["tool_name"], 100), "detail": detail(event)});
    if string(event, "hook_event_name") == "SessionCleared" {
        data["at"] = event["at"].clone();
        data["activity_key"] = event["activity_key"].clone();
    }
    if let Some(id)=processes::event_job(record,event) {data["process_id"]=json!(id);}
    mark_system_prompt(record, &mut data, event["prompt"].as_str().unwrap_or(""));
    if string(record, "agent") == "kimi"
        && string(event, "agent_id") == "main"
        && !["SubagentStart", "SubagentStop"].contains(&string(event, "hook_event_name"))
    {
        data["agent_id"] = json!("");
    }
    if let Some(origin) = turn_origin(record, event) {
        data["turn_origin"] = json!(origin);
    }
    if let Some(id) = kimi_turn_id(record, event) {
        data["kimi_turn_id"] = json!(id);
        data["kimi_conversation_id"] = record["conversation_id"].clone();
        if ["kimi_wire","kimi_hook"].contains(&string(event,"turn_identity_source")) {
            data["turn_identity_source"] = event["turn_identity_source"].clone();
        }
    }
    if let Some(question) = questions::from_hook(record, event) {
        data["question_request"] = question;
    }
    if string(event, "hook_event_name") == "QuestionAnswered" {
        for key in ["question_id", "question_hash", "tool_call_id"] {
            data[key] = event[key].clone();
        }
        if event["at"].as_f64().is_some_and(|at| at > 0.0) {
            data["at"] = event["at"].clone();
        }
    }
    let mut db = event_db()?;
    let transaction = db.transaction().map_err(|e| e.to_string())?;
    transaction
        .execute(
            "INSERT INTO events(name, conversation, payload) VALUES (?, ?, ?)",
            params![
                journal_name(record),
                record["conversation_id"].as_str(),
                data.to_string()
            ],
        )
        .map_err(|e| e.to_string())?;
    let seq = transaction.last_insert_rowid();
    if seq % 100 == 0 {
        transaction
            .execute("DELETE FROM events WHERE seq <= ?", [seq - 50000])
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    crate::analytics::native::collect(&root(), record, event);
    crate::analytics::adapters::hook(&root(), record, event);
    Ok(())
}

pub(super) fn normalized_activity(record: &Value, live_pane: bool) -> Value {
    let alive = live_pane && process_alive(record);
    let mut activity = record.get("activity").cloned().unwrap_or(json!("unknown"));
    let mut phase = record.get("phase").cloned().unwrap_or(json!("unknown"));
    if alive && codex_approval::running(record) {
        phase = json!(if record["active_tools"].as_object().is_some_and(|tools| !tools.is_empty()) {
            "tool"
        } else {
            "working"
        });
    }
    // Legacy SessionEnd was overloaded as process death. It is not evidence of
    // either process death or idleness; old manifests remain restorable.
    if live_pane && (activity == "ended" || phase == "ended") && alive {
        activity = json!("unknown");
        phase = json!("unknown");
    }
    if alive && telemetry::grouped_active(record) > 0 && activity == "idle" {
        activity = json!("busy");
        phase = json!("working");
    }
    json!({"activity": activity, "phase": phase,
        "conversation_state": record.get("conversation_state").cloned().unwrap_or_else(||
            json!(if string(record, "activity") == "ended" { "ended" } else { "active" })),
        "runtime_state": if live_pane {"live"} else {"stopped"},
        "process_state": if alive {"running"} else if string(record, "process_start").is_empty() {"unknown"} else {"exited"}})
}

// A durable Stop event identifies a response, even if busy/idle transitions were
// missed between polls. Child stops and initial SessionStart are not replies.
// Reading the retained journal also covers sessions created before this feature.
fn latest_reply(record: &Value, recurring_schedules: Option<u64>) -> Result<Value> {
    let conversation = string(record, "conversation_id");
    if conversation.is_empty() {
        return Ok(json!({}));
    }
    let db = event_db()?;
    let reply = |seq: i64, at: Value, scheduled: bool| {
        json!({"reply_id":format!("{}:{}", seq, at), "reply_at":at, "reply_scheduled":scheduled})
    };
    if string(record, "agent") == "kimi" {
        let skip = recurring_schedules.is_some_and(|count| count > 0);
        return Ok(kimi_reply(&db, record, skip)?
            .map_or(json!({}), |(seq, at, scheduled)| reply(seq, at, scheduled)));
    }
    let result: Option<(i64, String)> = db
        .query_row(
            "SELECT seq, payload FROM events WHERE name = ? AND conversation = ?
         AND json_extract(payload, '$.type') = 'Stop' AND json_extract(payload, '$.agent_id') = ''
         ORDER BY seq DESC LIMIT 1",
            params![journal_name(record), conversation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((seq, payload)) = result else {
        return Ok(json!({}));
    };
    let event: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    if !keepalive_turn(&db, record, seq)? {
        return Ok(reply(seq, event["at"].clone(), false));
    }
    // U30: the latest turn only warmed the cache; the cache stays warm from it.
    let mut found = real_reply(&db, record, seq)?.map_or(json!({}), |(seq, at)| reply(seq, at, false));
    found["kept_warm_at"] = event["at"].clone();
    Ok(found)
}

// WhiteWh fork: whether the turn of this main Stop opened with the desktop's
// cache keepalive. Only the latest turn is read; a conversation that has had
// no keepalive costs one indexed row.
fn keepalive_turn(db: &Connection, record: &Value, stop: i64) -> Result<bool> {
    let opening: Option<String> = db
        .query_row(
            "SELECT payload FROM events WHERE name = ? AND conversation = ?
           AND COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main')
           AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop')
           AND seq < ? AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserMessage','Stop')
         ORDER BY seq DESC LIMIT 1",
            params![journal_name(record), string(record, "conversation_id"), stop],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(opening.is_some_and(|payload| keepalive_opening(&payload)))
}

fn keepalive_opening(payload: &str) -> bool {
    serde_json::from_str::<Value>(payload).is_ok_and(|event| {
        event["type"] != "Stop"
            && (event["origin"] == "cache_keepalive" || string(&event, "detail").starts_with(CACHE_KEEPALIVE))
    })
}

// The latest main Stop whose turn the person or the agent opened. Keepalive
// turns follow each other for at most a day, a few rows each.
fn real_reply(db: &Connection, record: &Value, before: i64) -> Result<Option<(i64, Value)>> {
    let mut statement = db
        .prepare(
            "SELECT seq, json_extract(payload, '$.type'), payload FROM events
         WHERE name = ? AND conversation = ?
           AND COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main')
           AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop')
           AND seq < ?
         ORDER BY seq DESC LIMIT 4000",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(params![journal_name(record), string(record, "conversation_id"), before], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|e| e.to_string())?;
    let mut stop: Option<(i64, Value)> = None;
    for row in rows {
        let (seq, kind, payload) = row.map_err(|e| e.to_string())?;
        match kind.as_deref() {
            Some("Stop") => {
                // A turn without a recorded opening is not a keepalive.
                if let Some(found) = stop.take() {
                    return Ok(Some(found));
                }
                let event: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                stop = Some((seq, event["at"].clone()));
            }
            Some("UserPromptSubmit" | "UserMessage") => {
                if let Some(found) = stop.take() {
                    if !keepalive_opening(&payload) {
                        return Ok(Some(found));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(stop)
}

// A scheduled job opens its turn without a prompt from the person; the latest
// opening before a Stop decides, so a prompt queued meanwhile is the person's.
// While a recurring job remains, its routine ticks are not replies: they would
// mark the conversation unread every few minutes. Returns (seq, at, scheduled).
fn kimi_reply(db: &Connection, record: &Value, skip_scheduled: bool) -> Result<Option<(i64, Value, bool)>> {
    let mut statement = db
        .prepare(
            "SELECT seq, json_extract(payload, '$.type'), json_extract(payload, '$.turn_origin'), payload FROM events
         WHERE name = ? AND conversation = ?
           AND COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main')
           AND json_extract(payload, '$.type') IN ('UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop')
         ORDER BY seq DESC LIMIT 4000",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(
            params![journal_name(record), string(record, "conversation_id")],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut stop: Option<(i64, Value)> = None;
    for row in rows {
        let (seq, kind, origin, payload) = row.map_err(|e| e.to_string())?;
        match (kind.as_deref(), stop.take()) {
            (Some("AgentMessage"), pending) => stop = pending,
            (Some("Stop"), None) => {
                let event: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                stop = Some((seq, event["at"].clone()));
            }
            (Some("TurnStarted"), Some((seq, at))) => {
                let scheduled = matches!(origin.as_deref(), Some("cron_job" | "cron_missed"));
                if !scheduled || !skip_scheduled {
                    return Ok(Some((seq, at, scheduled)));
                }
            }
            (_, Some((seq, at))) => return Ok(Some((seq, at, false))),
            (_, None) => {}
        }
    }
    Ok(stop.map(|(seq, at)| (seq, at, false)))
}

/// A session's status. Its readers of the live pane (the effort footer and
/// every terminal chooser) share one capture of it.
pub(super) fn summary(record: &Value, live_pane: bool) -> Value {
    kimi_tui_choice::observing(|| observed_summary(record, live_pane))
}

fn observed_summary(record: &Value, live_pane: bool) -> Value {
    let mut output = normalized_activity(record, live_pane);
    output["conversation_id"] = record["conversation_id"].clone();
    output
        .as_object_mut()
        .unwrap()
        .extend(goals::summary(record).as_object().unwrap().clone());
    if let Ok(reply) = latest_reply(record, output["schedules"]["recurring"].as_u64()) {
        output
            .as_object_mut()
            .unwrap()
            .extend(reply.as_object().unwrap().clone());
    }
    if !string(record, "launch_id").is_empty() {
        output["launch_id"] = record["launch_id"].clone();
    }
    let last_tool = record["active_tools"]
        .as_object()
        .filter(|_| live_pane && output["activity"] == "busy")
        .and_then(|tools| {
            tools.values().max_by(|a, b| {
                a["started"]
                    .as_f64()
                    .unwrap_or_default()
                    .total_cmp(&b["started"].as_f64().unwrap_or_default())
            })
        });
    let tool = last_tool.map(|tool| string(tool, "name")).unwrap_or("");
    let detail = last_tool.map(|tool| string(tool, "detail")).unwrap_or("");
    let telemetry = telemetry::summary(record, &output, tool, detail);
    output
        .as_object_mut()
        .unwrap()
        .extend(telemetry.as_object().unwrap().clone());
    output.as_object_mut().unwrap().extend(json!({"tracked": true, "run_id":record["run_id"], "worktree_owner_id":record["worktree_owner"]["id"], "model": string(record, "model"),
        "cwd":record["cwd"].as_str().unwrap_or_else(||string(record,"launch_dir")), "cwd_source":if live_pane {"hook"} else {"saved"},
        "prompt": clipped(user_prompt(record), 240), "last_event_at": record.get("last_event_at").unwrap_or(&json!(0)),
        "turn_started": record.get("turn_started").unwrap_or(&json!(0)),
        "compaction_started": if record["phase"] == "compacting" { record.get("compaction_started").or_else(||record.get("last_main_progress_at")).unwrap_or(&json!(0)).clone() } else { json!(0) },
        "current_tool": last_tool.map(|tool| string(tool, "name")).unwrap_or(""),
        "tool_detail": last_tool.map(|tool| string(tool, "detail")).unwrap_or(""),
        // U30: a tool call running past the threshold makes the session wait.
        "tool_started": last_tool.and_then(|tool| tool["started"].as_f64())}).as_object().unwrap().clone());
    output.as_object_mut().unwrap().extend(
        effort::summary(record, live_pane)
            .as_object()
            .unwrap()
            .clone(),
    );
    output.as_object_mut().unwrap().extend(
        fork::summary(record, live_pane)
            .as_object()
            .unwrap()
            .clone(),
    );
    let resumed = resume_input::summary_check(record, live_pane);
    output.as_object_mut().unwrap().extend(session_action::summary(record,live_pane,&resumed).as_object().unwrap().clone());
    output["terminal_supported"] = json!(live_pane && string(record,"archive_id").is_empty() && process_alive(record));
    output["terminal_reason"] = json!(if output["terminal_supported"] == true { "" } else { backend::TERMINAL_NEEDS_PANE.text() });
    output["subagent_count"] = output["subagent_active_count"].clone();
    let account = account_binding(record);
    output["account_id"] = account["id"].clone();
    output["account_home"] = account["home"].clone();
    if ["input", "approval", "error"].contains(&string(&output, "phase")) {
        // Stable across heartbeats, different for a new question even when the
        // intervening working state fell between two desktop polls.
        output["attention_id"] = json!(record["active_tools"]
            .as_object()
            .map(|tools| tools.keys().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default());
        if output["phase"] != "error" {
            output["attention_started"] = record.get("attention_started").cloned().unwrap_or(json!(0));
        }
    }
    resume_input::enrich(&mut output, &resumed);
    provider_errors::enrich(record, &mut output, live_pane);
    if live_pane {
        // Native startup choosers have no hook. A later recognized panel wins;
        // an unrecognized chooser before a conversation stays visible read-only.
        let recognized = terminal_chooser(record);
        if let Some((question, phase, detail)) = &recognized {
            output["activity"] = json!("busy");
            output["phase"] = json!(phase);
            output["attention_id"] = question["question_id"].clone();
            output["status_detail"] = json!(detail);
        }
        // A resumed conversation reported this panel as an unknown terminal state.
        if output["attention_id"].as_str().is_some_and(|id| id.starts_with("codex-goal-resume:")) {
            output["activity_detail"] = output["status_detail"].clone();
            output["resume_message_reason"] = json!(codex_goal_resume::WAIT);
        }
        let fallback = if recognized.is_some() { None } else { startup_choice::current(record) };
        let shown = recognized.is_some() || fallback.is_some();
        if let Some(question) = fallback {
            output["activity"] = json!("busy");
            output["phase"] = json!("input");
            output["attention_id"] = question["question_id"].clone();
            output["status_detail"] = json!(startup_choice::detail(record, &question));
        }
        if shown {
            wait_for_chooser(&mut output);
        }
    }
    recovery::enrich(record, &mut output);
    output
}

/// Same read-only status observations as a listing, with no reconciliation,
/// adoption, fitting or action. A reply failure refuses an unmute baseline.
pub(super) fn notification_summary(record: &Value, live_pane: bool) -> Result<Value> {
    let mut output = summary(record, live_pane);
    let reply = latest_reply(record, output["schedules"]["recurring"].as_u64())?;
    output.as_object_mut().unwrap().extend(reply.as_object().unwrap().clone());
    Ok(output)
}

/// The native chooser the terminal shows, which no hook reports: its
/// question, the phase the summary shows and its detail. A later recognized
/// panel wins. Dispatch refuses each of them (`input::checked_input`).
pub(super) fn terminal_chooser(record: &Value) -> Option<(Value, &'static str, &'static str)> {
    type Current = fn(&Value) -> Option<Value>;
    let choosers: [(Current, &str, &str); 13] = [
        // First: any panel a later, known chooser recognizes wins.
        (claude_approval::idle_choice, "input", "Claude is waiting for your choice in Terminal"),
        (claude_approval::current, "approval", "Claude is waiting for your tool or safety approval"),
        (claude_resume::current, "approval", "Claude is waiting for your conversation resume choice"),
        (claude_usage_limit::current, "input", "Claude is waiting for your usage-limit choice"),
        (claude_permission::current, "approval", "Claude is waiting for your default permission mode choice"),
        (claude_trust::current, "approval", "Claude is waiting for folder trust approval"),
        (codex_trust::current, "approval", "Codex is waiting for folder trust approval"),
        (codex_hooks_trust::current, "approval", "Codex is waiting for hook trust review"),
        (codex_update::current, "input", "Codex is waiting for an update choice"),
        (codex_rate_limit::current, "input", "Codex is waiting for your rate-limit model choice; open Activity or Terminal"),
        (codex_goal_resume::current, "input", "Codex is waiting for your paused goal choice"),
        (kimi_trust::current, "approval", "Kimi is waiting for folder trust approval"),
        (kimi_cache_hint::current, "input", "Kimi is waiting for your context choice"),
    ];
    choosers
        .into_iter()
        .filter_map(|(current, phase, detail)| current(record).map(|question| (question, phase, detail)))
        .next_back()
}

/// A chooser only the terminal shows leaves the saved activity idle, so the
/// summary offered the actions that need Ready before it recognized the
/// chooser. Dispatch refuses the chooser, as for clear and compaction: a live
/// fork and an effort change wait until it is answered, and a settings change
/// is scheduled for Ready instead of applied now.
fn wait_for_chooser(output: &mut Value) {
    if let Some(reason) = clear_context::answer_first(string(output, "phase")) {
        fork::wait_for_chooser(output, reason);
        effort::wait_for_chooser(output, reason);
    }
}

const MESSAGE_EVENTS_LIMIT: usize = 100;
const MESSAGE_EVENTS_BYTES: usize = 256 * 1024;

// Activity continues across confirmed clears: rows of the current conversation
// and of the conversations it replaced. Sequence numbers are global, so a
// reader's cursor stays valid when a clear changes the conversation.
pub(super) const TIMELINE: &str = "name=?1 AND (conversation IS ?2 OR conversation IN (SELECT value FROM json_each(?3)))";

// The activity tail includes tools and children. Keep main public messages in
// a separate bounded window so a cold reader does not lose its newest prompt
// merely because that turn has produced more than one hundred tool events.
fn message_events(db: &Connection, name: &str, conversation: Option<&str>, earlier: &str) -> Result<(Vec<Value>, bool)> {
    let mut query = db.prepare(&format!("SELECT seq,payload FROM events WHERE {TIMELINE}
        AND COALESCE(json_extract(payload, '$.agent_id'), '') IN ('', 'main')
        AND json_extract(payload, '$.type') IN ('SessionCleared','UserPromptSubmit','UserPromptQueued','UserMessage','TurnStarted','QuestionAnswered','AgentMessage','Stop')
        ORDER BY seq DESC LIMIT 101")).map_err(|e| e.to_string())?;
    let rows = query.query_map(params![name, conversation, earlier], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let (mut events, mut bytes, mut truncated) = (Vec::new(), 2, false);
    for row in rows {
        let (seq, payload) = row.map_err(|e| e.to_string())?;
        if events.len() == MESSAGE_EVENTS_LIMIT { truncated = true; break; }
        let mut event: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        event["seq"] = json!(seq);
        let size = event.to_string().len() + usize::from(!events.is_empty());
        if bytes + size > MESSAGE_EVENTS_BYTES { truncated = true; break; }
        bytes += size;
        events.push(event);
    }
    events.reverse();
    Ok((events, truncated))
}

pub(super) fn inspection(name: &str, after: i64, archive_id: Option<&str>, include_processes: bool) -> Result<Value> {
    // An approval answered in Terminal sends no hook; settling it takes the lock.
    if archive_id.is_none() && record_path(name).exists() {
        if let Ok(record) = read(name) {
            claude_approval::reconcile(record);
        }
    }
    let _guard = lock(None)?;
    let untracked = |cwds| {
        let mut value = json!({"name": name, "tracked": false, "events": [], "cursor": 0});
        value
            .as_object_mut()
            .unwrap()
            .extend(telemetry::unavailable().as_object().unwrap().clone());
        workspace::enrich(std::slice::from_mut(&mut value), &cwds);
        match untracked_shell::capture(name) {
            Ok(identity) => value["untracked_terminal"] = json!(identity),
            Err(_) => {
                value["untracked_terminal_reason"] =
                    json!(zerus_client_core::untracked_shell::UNVERIFIED)
            }
        }
        value
    };
    if archive_id.is_none() && !record_path(name).exists() {
        drop(_guard);
        return Ok(untracked(workspace::pane_cwds()));
    }
    let mut record = if let Some(id) = archive_id {
        archive::read_archive(name, id)?
    } else {
        read(name)?
    };
    let (snapshot, names) = live_sessions()?;
    let panes = if archive_id.is_some() {
        None
    } else {
        snapshot.get(name)
    };
    if panes.is_some() && !matches(&record, panes) {
        drop(_guard);
        return Ok(untracked(workspace::snapshot_cwds(&names)));
    }
    if archive_id.is_none() && panes.is_some() {
        let _ = clear_context::observe_terminal(&mut record);
    }
    let mut output = json!({});
    for key in [
        "name",
        "conversation_id",
        "run_id",
        "cwd",
        "agent",
        "model",
        "phase",
        "activity",
        "prompt",
        "last_message",
        "last_error",
        "active_tools",
        "subagents",
        "subagent_groups",
        "subagent_groups_complete",
        "task_lists",
        "last_event_at",
        "turn_started",
        "compaction_started",
        "created",
        "updated",
        "archive_id",
        "archived_at",
        "completion_source",
        "completion_reason",
        "exit_code",
        "termination_signal",
        "exited_at",
        "session_end",
        "last_restore_error",
        "launch_prompt",
        "error",
        "expected_id",
        "requested_id",
        "prompt_suggestion",
    ] {
        if let Some(value) = record.get(key) {
            output[key] = value.clone();
        }
    }
    output.as_object_mut().unwrap().extend(
        summary(&record, panes.is_some())
            .as_object()
            .unwrap()
            .clone(),
    );
    // U53: the account's label beside its id, which a move reads back.
    if let Ok(config) = crate::config::Config::load() {
        output["account_label"] = json!(crate::accounts::label(&config, string(&output, "account_id")));
    }
    // The compact list preview must not shorten the existing inspector field.
    if !user_prompt(&record).is_null() {
        output["prompt"] = record["prompt"].clone();
    }
    output["tracked"] = json!(true);
    if archive_id.is_some() {
        output["state"] = json!("archived");
    }
    drop(_guard);
    if archive_id.is_none() {
        output.as_object_mut().unwrap().extend(
            input::first_message_summary(&record, panes.is_some())
                .as_object()
                .unwrap()
                .clone(),
        );
        input::attention_message_summary(&record, &mut output, panes.is_some());
    }
    match questions::inspection(&record, panes.is_some()) {
        Ok(pending) => output["pending_questions"] = pending,
        Err(error) => {
            output["pending_questions"] = json!([]);
            output["pending_questions_error"] = json!(error);
        }
    }
    output["input_queue"] = if archive_id.is_none() { input_queue::inspection(&record, panes.is_some()).unwrap_or(Value::Null) } else { Value::Null };
    output["session_usage"] = if clear_context::awaiting_start(&record) {
        json!({"status":"unavailable","source":record["agent"],"scope":"conversation"})
    } else {
        let mut usage = usage::read(&record);
        // Only for the model this inspection reports: a switch makes it stale.
        claude_statusline::apply(&record, string(&output, "model"), &mut usage);
        if string(&record, "agent") == "claude" {
            session_state::observe_ttl(string(&output, "account_id"), &usage);
        }
        usage
    };
    output["session_clear"] = clear_context::event(&record).unwrap_or(Value::Null);
    output["clear_context_request"] = record["clear_context_request"].clone();
    if include_processes {
        output["processes"] = processes::snapshot(&record, archive_id.is_none() && panes.is_some() && process_alive(&record));
        output["process_work"] = processes::WorkSample::default().summary(&record,
            archive_id.is_none() && panes.is_some() && output["activity"] == "idle" && output["phase"] == "idle");
    }
    // Each unavailable action names its reason; an older host omits it.
    let context_reason = if archive_id.is_some() { Some("Restore the archived session first".to_owned()) }
        else { clear_context::unavailable_shown(&record,panes.is_some(),&output) };
    output["compact_context_supported"] = json!(context_reason.is_none());
    output["compact_context_reason"] = json!(context_reason.clone().unwrap_or_default());
    output["compact_context_request"] = record["compact_context_request"].clone();
    // The summary above verified a pending resume once; its message and
    // actions reuse that result instead of checking the terminal again.
    // So did the summary of an interrupted Claude turn's prompt.
    let interrupted = archive_id.is_none() && clear_context::interrupted(&record,panes.is_some());
    let failed = archive_id.is_none() && clear_context::failed(&record,panes.is_some());
    output["clear_context_supported"] = json!(archive_id.is_none() && (context_reason.is_none()
        || clear_context::resumed_verified(&record,panes.is_some(),output["resume_message_can_send"] == true)
        || interrupted && output["interrupted_message_can_send"] == true || failed));
    output["clear_context_reason"] = json!(if output["clear_context_supported"] == true { String::new() }
        else if interrupted && !string(&output,"interrupted_message_reason").is_empty() {
            string(&output,"interrupted_message_reason").to_owned()
        } else { context_reason.unwrap_or_default() });
    output["clear_context_dismiss"] = json!(archive_id.is_none() && output["clear_context_supported"] != true
        && clear_context::dismissable(&record,panes.is_some()) && !clear_context::startup_choice(&output["pending_questions"]));
    output["interrupt_supported"] = json!(archive_id.is_none() && interrupt::available(&record,panes.is_some()));
    cache_hint::enrich(&record,&mut output,archive_id.is_none()&&panes.is_some());
    let messages = if string(&record, "agent") == "kimi" {
        kimi_messages::read_with_turns(&record).map(|(messages, turns, complete)| {
            output["kimi_turns"] = json!(turns);
            output["kimi_turns_complete"] = json!(complete);
            messages
        })
    } else { provider_messages::read(&record) };
    match messages {
        Ok(messages) => output["provider_messages"] = json!(messages),
        Err(error) => output["provider_messages_error"] = json!(error),
    }
    // Optional-question indexing must never suppress successfully read replies.
    match codex_questions::messages(&record) {
        Ok(questions) => {
            let mut messages = output["provider_messages"].as_array().cloned().unwrap_or_default();
            messages.extend(questions);
            messages.sort_by(|a,b|a["at"].as_f64().unwrap_or(0.).total_cmp(&b["at"].as_f64().unwrap_or(0.)));
            if messages.len()>100 { messages.drain(..messages.len()-100); }
            output["provider_messages"] = json!(messages);
        }
        Err(error) => output["pending_questions_error"] = json!(error),
    }
    if clear_context::awaiting_start(&record) {
        output["prompt"] = json!("");
        output["last_message"] = json!("");
        output["provider_messages"] = json!([]);
        output["kimi_turns"] = json!([]);
        output["kimi_turns_complete"] = json!(false);
        output["cache_hint"] = Value::Null;
    }
    let db = event_db()?;
    let journal_name = journal_name(&record);
    let conversation = record["conversation_id"].as_str();
    let cleared = clear_context::earlier_conversations(&record);
    let earlier = cleared.to_string();
    output["cleared_conversations"] = cleared;
    let oldest: Option<i64> = db
        .query_row(
            &format!("SELECT MIN(seq) FROM events WHERE {TIMELINE}"),
            params![journal_name, conversation, earlier],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let rows = if after > 0 {
        timeline_rows(&db, journal_name, conversation, &earlier, after, 200, false)?
    } else {
        timeline_rows(&db, journal_name, conversation, &earlier, after, 100, true)?
    };
    output["cursor"] = json!(rows.last().map(|(seq, _)| *seq).unwrap_or(after));
    output["history_truncated"] =
        json!(after > 0 && oldest.map(|seq| after < seq - 1).unwrap_or(false));
    let mut events = Vec::new();
    for (seq, payload) in rows {
        events.push(timeline_event(&record, seq, &payload)?);
    }
    output["events"] = json!(events);
    let (messages, truncated) = message_events(&db, journal_name, conversation, &earlier)?;
    output["message_events"] = json!(messages);
    output["message_events_truncated"] = json!(truncated);
    output["message_events_limit"] = json!(MESSAGE_EVENTS_LIMIT);
    output["message_events_max_bytes"] = json!(MESSAGE_EVENTS_BYTES);
    output["attachment_messages"] = attachments::messages(&record, "");
    // U35: messages hgs holds until the agent takes the previous one, and that
    // this hgs holds them for a client that asks (`queue_when_unconfirmed`).
    output["input_hold_supported"] = json!(true);
    output["held_inputs"] = input_hold::inspection(string(&record, "name"));
    // #511 N5: the latest held messages that were not delivered (failed or
    // uncertain), so the composer that sent one restores its text.
    output["held_settled"] = input_hold::settled(string(&record, "name"));
    output["voice_messages"] = conversation_history::inspected_voice_messages(&record, &output, None);
    // The lock was released after `snapshot`, so a rename may have landed since.
    // The pane folders go by the session IDs, which a rename keeps, to the name
    // this inspection reports (zerus-byyw, as in `merge_listing`).
    workspace::enrich(std::slice::from_mut(&mut output), &workspace::snapshot_cwds(&names));
    waits::decorate(std::slice::from_mut(&mut output));
    notifications::enrich(&mut output, notifications::load().as_ref(), journal_name);
    // U30: after the notification preferences, which say whether it is muted.
    session_state::decorate(std::slice::from_mut(&mut output));
    Ok(output)
}

/// A session timeline's rows after `after`, oldest first: the first `limit`
/// of them, or with `newest` the last `limit`. `inspection` and `hgs watch`
/// both read Activity through it and `timeline_event`.
pub(super) fn timeline_rows(db: &Connection, journal_name: &str, conversation: Option<&str>, earlier: &str,
    after: i64, limit: usize, newest: bool) -> Result<Vec<(i64, String)>> {
    let mut statement = db.prepare(&if newest {
        format!("SELECT seq, payload FROM events WHERE {TIMELINE} AND seq>?4 ORDER BY seq DESC LIMIT ?5")
    } else { format!("SELECT seq, payload FROM events WHERE {TIMELINE} AND seq>?4 ORDER BY seq LIMIT ?5") }).map_err(|e| e.to_string())?;
    let mut rows: Vec<(i64, String)> = statement
        .query_map(params![journal_name, conversation, earlier, after, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if newest {
        rows.reverse();
    }
    Ok(rows)
}

/// One Activity event as `inspect` returns it.
pub(super) fn timeline_event(record: &Value, seq: i64, payload: &str) -> Result<Value> {
    let mut event: Value = serde_json::from_str(payload).map_err(|e| e.to_string())?;
    event["seq"] = json!(seq);
    mark_system_prompt(record, &mut event, "");
    Ok(event)
}

pub(super) fn journal_name(record: &Value) -> &str {
    if string(record, "journal_name").is_empty() {
        string(record, "name")
    } else {
        string(record, "journal_name")
    }
}

pub(super) fn update_activity(record: &mut Value, event: &Value) {
    if stale_kimi_turn(record, event) { return; }
    repair_kimi_main(record);
    repair_claude_internal(record);
    let was_busy = record["activity"] == "busy";
    let kind = string(event, "hook_event_name");
    let timestamp = now();
    record["last_event_at"] = json!(timestamp);
    let mut children = record["subagents"].as_object().cloned().unwrap_or_default();
    let root_agent = string(record, "agent") == "kimi" && string(event, "agent_id") == "main";
    if ["SubagentStart", "SubagentStop"].contains(&kind)
        || (!root_agent && !string(event, "agent_id").is_empty())
    {
        if claude_internal(record, event, kind) {
            return;
        }
        let mut child = clipped(&event["agent_id"], 160);
        if root_agent {
            child.clear();
        }
        if child.is_empty() {
            child = clipped(&event["agent_name"], 160);
        }
        if child.is_empty() {
            child = "subagent".into();
        }
        let mut name = clipped(&event["agent_type"], 160);
        if name.is_empty() {
            name = clipped(&event["agent_name"], 160);
        }
        let has_label = !name.is_empty();
        if name.is_empty() {
            name = child.clone();
        }
        let info = children
            .entry(child)
            .or_insert_with(|| json!({"name": name, "started": timestamp}));
        if has_label {
            info["name"] = json!(name);
        }
        if kind == "SubagentStart" {
            info["announced"] = json!(true);
        }
        // Claude names a subagent by its Agent call's description. Keep it only
        // when one unclaimed Agent call can have started this subagent.
        if kind == "SubagentStart" && info.get("description").is_none() {
            let claimed: Vec<Value> = record["subagents"].as_object().into_iter().flatten()
                .filter_map(|(_, other)| other.get("agent_call").cloned()).collect();
            let calls: Vec<(&String, &Value)> = record["active_tools"].as_object().into_iter().flatten()
                .filter(|(id, tool)| ["Agent", "Task"].contains(&string(tool, "name")) && !claimed.contains(&json!(id)))
                .collect();
            if let [(id, tool)] = calls[..] {
                info["agent_call"] = json!(id);
                info["description"] = json!(clipped(&tool["detail"], 160));
            }
        }
        if ["Stop", "SubagentStop"].contains(&kind) {
            let result = detail(event);
            if string(info, "state") != "finished"
                || (!result.is_empty() && result != string(info, "detail"))
            {
                info["reply_id"] = json!(format!("{timestamp}"));
            }
        }
        // A Claude child's StopFailure ends its turn with a provider error, such
        // as a usage limit; Claude sends no SubagentStop after it. The child is
        // settled as an error, which never keeps its parent working. A newer
        // event of the same child resumes it as usual.
        let failed = kind == "StopFailure" && string(record, "agent") == "claude";
        info["state"] = json!(if failed || ["SubagentStop", "SessionEnd", "Stop"].contains(&kind) {
            "finished"
        } else {
            "working"
        });
        let snippet = detail(event);
        if !snippet.is_empty() {
            info["detail"] = json!(snippet);
        }
        if failed {
            info["display_state"] = json!("error");
        } else if ["StopFailure", "Interrupt"].contains(&kind) {
            info["display_state"] = json!("unknown");
        } else {
            info.as_object_mut().unwrap().remove("display_state");
        }
        // Any newer child event ends its pending request (hooks.rs sets a new one).
        info.as_object_mut().unwrap().remove("attention_id");
        if kind == "PreToolUse" {
            info["current_tool"] = json!(clipped(&event["tool_name"], 120));
        } else if [
            "PostToolUse",
            "PostToolUseFailure",
            "SubagentStop",
            "Stop",
            "SessionEnd",
            "StopFailure",
            "Interrupt",
        ]
        .contains(&kind)
        {
            info["current_tool"] = json!("");
        }
        info["updated"] = json!(timestamp);
        info["tracking_evidence"] = json!({"source":"hook","kind":kind,"at":timestamp});
        info.as_object_mut().unwrap().remove("completion_source");
        if record["main_done"].as_bool().unwrap_or(false) {
            let tools_busy = record["active_tools"]
                .as_object()
                .map(|tools| !tools.is_empty())
                .unwrap_or(false);
            let busy = tools_busy
                || children
                    .values()
                    .any(|child| telemetry::child_working(record, child));
            record["activity"] = json!(if busy { "busy" } else { "idle" });
            record["phase"] = json!(if tools_busy {
                "tool"
            } else if busy {
                "working"
            } else {
                "idle"
            });
        }
        if children.len() > 64 {
            let remove: Vec<String> = children
                .iter()
                .filter(|(_, child)| string(child, "state") == "finished")
                .take(children.len() - 64)
                .map(|(key, _)| key.clone())
                .collect();
            for key in remove {
                children.remove(&key);
                record["subagents_completed_pruned"] =
                    json!(record["subagents_completed_pruned"].as_u64().unwrap_or(0) + 1);
            }
        }
        record["subagents"] = json!(children);
        return;
    }
    record["subagents"] = json!(children);
    if ["UserPromptSubmit", "UserPromptQueued"].contains(&kind) {
        record["recovery_user_at"] = json!(timestamp);
        record["recovery_user_text"] = json!(event["prompt"].as_str().map(str::to_owned).unwrap_or_else(||
            event["prompt"].as_array().into_iter().flatten().filter(|v|v["type"]=="text")
                .map(|v|string(v,"text")).collect::<Vec<_>>().join("\n")));
    }
    if kind == "Stop" { record["recovery_success_at"] = json!(timestamp); }
    if ["Interrupt", "SessionEnd"].contains(&kind) { record["recovery_cancel_at"] = json!(timestamp); }
    if [
        "SessionStart",
        "UserPromptSubmit",
        "UserPromptQueued",
        "TurnStarted",
        "TaskStarted",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "PermissionRequest",
        "PermissionResult",
        "PreCompact",
        "PostCompact",
        "Stop",
        "Interrupt",
    ]
    .contains(&kind)
    {
        record["last_main_progress_at"] = json!(timestamp);
        record.as_object_mut().unwrap().remove("provider_error");
    }
    if event["model"].is_string() {
        record["model"] = json!(clipped(&event["model"], 120));
    }
    if kind == "PostModelSwitch" {
        effort::observe_switch(record, event);
    }
    for field in ["effort", "reasoning_effort", "thinking_effort"] {
        // Claude hooks report the turn's applied effort as {"level": ...}.
        let value = event[field].as_str().or(event[field]["level"].as_str());
        if let Some(value) = value.filter(|value| effort::valid(value)) {
            record["effort"] = json!(value);
        }
    }
    let previous_phase = string(record, "phase").to_owned();
    let mut tools = record["active_tools"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if matches!(
        kind,
        "UserPromptSubmit"
            | "UserPromptQueued"
            | "TurnStarted"
            | "TaskStarted"
            | "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "PermissionRequest"
            | "PermissionResult"
    ) {
        // Positive evidence of main-agent work invalidates a previous Stop,
        // including providers that omit UserPromptSubmit before a tool call.
        record["main_done"] = json!(false);
    }
    match kind {
        "UserPromptSubmit" | "UserPromptQueued" | "TurnStarted" | "TaskStarted" => {
            record["activity"] = json!("busy");
            if kind != "UserPromptQueued" || record["phase"] != "compacting" {
                record["phase"] = json!("working");
            }
            record["conversation_state"] = json!("active");
            record["main_done"] = json!(false);
            // Queuing another message while working must not restart the
            // timer of the turn still executing in the native terminal.
            if kind != "UserPromptQueued"
                || !was_busy
                || record["turn_started"].as_f64().unwrap_or(0.0) <= 0.0
            {
                record["turn_started"] = json!(timestamp);
            }
            record["last_error"] = json!("");
            // Background work starts a turn, but it is not the person's request.
            if !string(event, "prompt").is_empty() && system_prompt(string(event, "prompt")).is_none() {
                record["prompt"] = json!(clipped(&event["prompt"], 4000));
            }
        }
        "PreToolUse" => {
            let mut tool = clipped(&event["tool_name"], 120);
            if tool.is_empty() {
                tool = "Tool".into();
            }
            let key = ["tool_use_id", "tool_call_id"]
                .iter()
                .map(|key| string(event, key))
                .find(|key| !key.is_empty())
                .unwrap_or(&tool);
            tools.insert(
                key.to_owned(),
                json!({"name": tool, "detail": detail(event), "started": timestamp}),
            );
            record["activity"] = json!("busy");
            record["phase"] = json!(if ["AskUserQuestion", "request_user_input", "AskUser"]
                .contains(&tool.as_str())
            {
                "input"
            } else {
                "tool"
            });
        }
        "PermissionRequest" => {
            record["activity"] = json!("busy");
            // Claude routes AskUserQuestion through its permission prompt too.
            record["phase"] = json!(if claude_approval::question_tool(string(event, "tool_name")) {
                "input"
            } else {
                "approval"
            });
        }
        "PermissionResult" => {
            record["activity"] = json!("busy");
            record["phase"] = json!(if tools.is_empty() { "working" } else { "tool" });
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let key = ["tool_use_id", "tool_call_id", "tool_name"]
                .iter()
                .map(|key| string(event, key))
                .find(|key| !key.is_empty())
                .unwrap_or("");
            if tools.remove(key).is_none() {
                tools.retain(|_, tool| string(tool, "name") != string(event, "tool_name"));
            }
            record["activity"] = json!("busy");
            record["phase"] = json!(if tools.is_empty() { "working" } else { "tool" });
            if kind == "PostToolUseFailure" {
                record["last_error"] = json!(detail(event));
            }
        }
        "PreCompact" => {
            if record["phase"] != "compacting" {
                record["phase_before_compact"] =
                    record.get("phase").cloned().unwrap_or(json!("working"));
                record["compaction_started"] = json!(timestamp);
            }
            record["activity"] = json!("busy");
            record["phase"] = json!("compacting");
        }
        "PostCompact" => {
            record.as_object_mut().unwrap().remove("compaction_started");
            record["phase"] = record
                .as_object_mut()
                .unwrap()
                .remove("phase_before_compact")
                .unwrap_or(json!("working"));
            record["activity"] = json!(if record["phase"] == "idle" {
                "idle"
            } else {
                "busy"
            });
        }
        "Stop" => {
            record["main_done"] = json!(true);
            tools.clear();
            let busy = record["subagents"]
                .as_object()
                .unwrap()
                .values()
                .any(|child| telemetry::child_working(record, child));
            record["activity"] = json!(if busy { "busy" } else { "idle" });
            record["phase"] = json!(if busy { "working" } else { "idle" });
            record["last_message"] = json!(clipped(
                if string(event, "last_assistant_message").is_empty() {
                    &event["response"]
                } else {
                    &event["last_assistant_message"]
                },
                4000
            ));
        }
        "Interrupt" | "StopFailure" => {
            record["activity"] = json!("unknown");
            record["main_done"] = json!(false);
            record["phase"] = json!(if kind == "Interrupt" {
                "interrupted"
            } else {
                "error"
            });
            tools.clear();
            record["last_error"] = json!(detail(event));
            if kind == "StopFailure" {
                let mut failure = provider_errors::event(
                    &format!("{}:{timestamp}", string(record, "run_id")),
                    timestamp,
                    &detail(event),
                    "provider_hook",
                );
                // Keep native type/code evidence when the rolling-window text
                // would otherwise erase a permanent refusal during projection.
                if provider_errors::rolling_rate_limit(&detail(event)) {
                    failure["native_error_type"] = json!(clipped(&event["error_type"], 1200));
                    failure["native_error_code"] = json!(clipped(&event["error"], 1200));
                }
                let category = provider_errors::native_kind(&failure).unwrap_or_else(|| provider_errors::category(&format!(
                    "{} {} {}",
                    string(event, "error_type"),
                    string(event, "error"),
                    detail(event)
                )));
                if category != "provider" || provider_errors::native_kind(&failure).is_some() {
                    failure["error_kind"] = json!(category);
                }
                // Keep a provider deadline separate from the chosen backoff.
                for (key,scale) in [("retry_after",1.0),("retry_after_seconds",1.0),("retry_after_ms",0.001)] {
                    if let Some(delay)=event[key].as_f64().filter(|v|v.is_finite()&&*v>0.0) {
                        failure["retry_not_before"]=json!(timestamp+delay*scale);
                    }
                }
                session_limits::enrich(&mut failure);
                record["provider_error"] = failure;
            }
        }
        "SessionEnd" => {
            record["conversation_state"] = json!("ended");
            let children_busy = record["subagents"]
                .as_object()
                .unwrap()
                .values()
                .any(|child| telemetry::child_working(record, child));
            // A hook ends a conversation scope, not necessarily the CLI process.
            // Preserve positive idle evidence; never turn ongoing work into idle.
            if record["activity"] != "idle" || !tools.is_empty() || children_busy {
                record["activity"] = json!("unknown");
                record["phase"] = json!("unknown");
                record["main_done"] = json!(false);
            }
        }
        "Notification"
            if ["permission_prompt", "agent_needs_input"]
                .contains(&string(event, "notification_type")) =>
        {
            record["activity"] = json!("busy");
            // Claude repeats a pending tool approval as a permission_prompt
            // notification; it must not turn into an ordinary input request.
            if record["phase"] != "approval" {
                record["phase"] = json!("input");
            }
        }
        _ => {}
    }
    // A request without a native identity (a permission prompt without a tool
    // call, a tool keyed by its name) is identified by when it started. Repeated
    // prompt notifications keep it; every new prompt or question tool starts one.
    // The end of a compaction restores the waiting request, and a prompt repeated
    // with the same native tool IDs, tool and input is that request again.
    let phase = string(record, "phase");
    if ["approval", "input"].contains(&phase) {
        let native = !tools.is_empty() && tools.iter().all(|(key, tool)| key != string(tool, "name"));
        let request = if native {
            let mut ids: Vec<&String> = tools.keys().collect();
            ids.sort();
            let input = format!("{:x}", sha2::Sha256::digest(event["tool_input"].to_string().as_bytes()));
            json!([ids, string(event, "tool_name"), input])
        } else {
            Value::Null
        };
        let repeated = (kind == "PostCompact" && record.get("attention_started").is_some())
            || (kind == "PermissionRequest"
                && previous_phase == "approval"
                && !request.is_null()
                && record["attention_request"] == request);
        if !repeated
            && (phase != previous_phase
                || (kind == "PermissionRequest" && phase == "approval")
                || (kind == "PreToolUse" && phase == "input"))
        {
            record["attention_started"] = json!(timestamp);
            record["attention_request"] = request;
        }
    }
    record["active_tools"] = json!(tools);
}

// Native turn IDs are monotonic, but the observation-only TurnStarted hook can
// arrive after its blocking Stop. Keep one clock per exact run/conversation;
// receiving an old start must neither reopen a completed turn nor erase work
// from a genuinely newer prompt. Other agents and unbound events are unchanged.
fn stale_kimi_turn(record: &mut Value, event: &Value) -> bool {
    if !["", "main"].contains(&string(event, "agent_id")) { return false; }
    let Some(id) = kimi_turn_id(record, event) else { return false };
    let run = string(record, "run_id");
    let conversation = string(record, "conversation_id");
    if run.is_empty() || conversation.is_empty() { return false; }
    let mut clock = record["kimi_turn_clock"].clone();
    if string(&clock, "run_id") != run || string(&clock, "conversation_id") != conversation {
        clock = json!({"run_id":run,"conversation_id":conversation});
    }
    let stopped = clock["stopped"].as_u64().filter(|id| *id <= i64::MAX as u64);
    let started = clock["started"].as_u64().filter(|id| *id <= i64::MAX as u64);
    let (key, stale) = if string(event, "hook_event_name") == "TurnStarted" {
        ("started", stopped.is_some_and(|done| id <= done)
            || started.is_some_and(|active| id < active))
    } else {
        ("stopped", started.is_some_and(|active| id < active)
            || stopped.is_some_and(|done| id < done))
    };
    if !stale { clock[key] = json!(id); }
    record["kimi_turn_clock"] = clock;
    stale
}

// Kimi's reserved "main" agent ID describes the root, not a Swarm child.
// Older hooks left it permanently working, even after positive Stop evidence.
pub(super) fn repair_kimi_main(record: &mut Value) {
    if string(record, "agent") != "kimi" {
        return;
    }
    let removed = record["subagents"]
        .as_object_mut()
        .and_then(|children| children.remove("main"))
        .is_some();
    if removed
        && record["main_done"] == true
        && record["phase"] == "working"
        && record["active_tools"]
            .as_object()
            .is_none_or(|tools| tools.is_empty())
        && record["subagents"].as_object().is_none_or(|children| {
            !children
                .values()
                .any(|child| telemetry::child_working(record, child))
        })
        && telemetry::grouped_active(record) == 0
    {
        record["activity"] = json!("idle");
        record["phase"] = json!("idle");
    }
}

// Claude's internal agents (progress summaries, the suggested next message)
// report hook events under their own agent ID, but never SubagentStart or an
// agent type. They are no subagents of the conversation: a lone tool call from
// one would otherwise stay working, since no SubagentStop ever follows it.
fn claude_internal(record: &Value, event: &Value, kind: &str) -> bool {
    string(record, "agent") == "claude"
        && kind != "SubagentStart"
        && !string(event, "agent_id").is_empty()
        && string(event, "agent_type").is_empty()
        && string(event, "agent_name").is_empty()
        && record["subagents"]
            .get(clipped(&event["agent_id"], 160))
            .is_none()
}

// Older hooks recorded those internal agents as subagents named by their own
// ID, and a lone tool call left the session working after its turn ended.
// A started subagent is announced, carries its agent type, or is external.
pub(super) fn repair_claude_internal(record: &mut Value) {
    if string(record, "agent") != "claude" {
        return;
    }
    let Some(children) = record["subagents"].as_object_mut() else {
        return;
    };
    let before = children.len();
    children.retain(|id, child| {
        child["external"] == true || child["announced"] == true || string(child, "name") != id
    });
    if children.len() != before {
        settle_children(record);
    }
}

pub(super) fn settle_children(record: &mut Value) {
    if record["main_done"] == true
        && record["phase"] == "working"
        && record["active_tools"]
            .as_object()
            .is_none_or(|tools| tools.is_empty())
        && record["subagents"].as_object().is_none_or(|children| {
            !children
                .values()
                .any(|child| telemetry::child_working(record, child))
        })
        && telemetry::grouped_active(record) == 0
    {
        record["activity"] = json!("idle");
        record["phase"] = json!("idle");
    }
}

#[cfg(test)]
mod attention_regressions {
    use super::*;

    #[test]
    fn native_kimi_turn_identity_retains_zero_without_inventing_ids_for_other_hooks() {
        let record = json!({"agent":"kimi"});
        let mut event = json!({"hook_event_name":"TurnStarted", "turn_id":0});
        assert_eq!(kimi_turn_id(&record, &event), Some(0));
        event["turn_id"] = json!(1);
        assert_eq!(kimi_turn_id(&record, &event), Some(1));
        event["hook_event_name"] = json!("Stop");
        assert_eq!(kimi_turn_id(&record, &event), Some(1));
        assert_eq!(kimi_turn_id(&json!({"agent":"claude"}), &event), None);
        event["hook_event_name"] = json!("UserPromptSubmit");
        assert_eq!(kimi_turn_id(&record, &event), None);
        event["hook_event_name"] = json!("TurnStarted");
        for invalid in [Value::Null, json!(-1), json!("0"), json!(u64::MAX)] {
            event["turn_id"] = invalid;
            assert_eq!(kimi_turn_id(&record, &event), None);
        }
    }

    fn kimi_clock_record() -> Value {
        json!({"agent":"kimi","run_id":"current-run","conversation_id":"current-conversation",
            "activity":"idle","phase":"idle","active_tools":{},"subagents":{}})
    }

    #[test]
    fn kimi_delayed_start_after_its_stop_keeps_the_completed_turn_idle_but_new_work_is_busy() {
        let mut record=kimi_clock_record();
        update_activity(&mut record,&json!({"hook_event_name":"UserPromptSubmit","prompt":"Follow-up after initial launch"}));
        // Exact CI order: seq7 UserPromptSubmit, seq8 Stop, seq9 TurnStarted
        // native turn1. Stop's ID is bound from its awaited native wire prefix.
        update_activity(&mut record,&json!({"hook_event_name":"Stop","turn_id":1,"turn_identity_source":"kimi_wire"}));
        assert_eq!((&record["activity"],&record["phase"],&record["main_done"]),(&json!("idle"),&json!("idle"),&json!(true)));
        let completed=record.clone();
        update_activity(&mut record,&json!({"hook_event_name":"TurnStarted","agent_id":"main","turn_id":1,"origin_kind":"user","prompt":"Follow-up after initial launch"}));
        assert_eq!(record,completed,"late start must not reset timers, main_done or prompt");
        let mut continued=completed.clone();
        update_activity(&mut continued,&json!({"hook_event_name":"PreToolUse","tool_use_id":"same-turn-call","tool_name":"Shell"}));
        let working=continued.clone();
        update_activity(&mut continued,&json!({"hook_event_name":"TurnStarted","turn_id":1}));
        assert_eq!(continued,working,"a delayed start cannot erase fresh positive work either");
        assert_eq!(continued["phase"],"tool");
        update_activity(&mut record,&json!({"hook_event_name":"TurnStarted","turn_id":2,"prompt":"genuinely newer"}));
        assert_eq!((&record["activity"],&record["phase"],&record["main_done"]),(&json!("busy"),&json!("working"),&json!(false)));
        let newer=record.clone();
        for kind in ["TurnStarted","Stop"] {
            update_activity(&mut record,&json!({"hook_event_name":kind,"turn_id":1}));
            assert_eq!(record,newer,"old {kind} must not change newer work");
        }
        update_activity(&mut record,&json!({"hook_event_name":"Stop","turn_id":2}));
        assert_eq!(record["phase"],"idle");
    }

    #[test]
    fn kimi_completion_clock_is_scoped_and_does_not_turn_unknown_or_other_agent_work_idle() {
        let mut completed=kimi_clock_record();
        update_activity(&mut completed,&json!({"hook_event_name":"Stop","turn_id":0}));
        let snapshot=completed.clone();
        update_activity(&mut completed,&json!({"hook_event_name":"TurnStarted","turn_id":0}));
        assert_eq!(completed,snapshot,"native turn zero is a real identity");
        for key in ["run_id","conversation_id"] {
            let mut changed=completed.clone();changed[key]=json!("replacement");
            update_activity(&mut changed,&json!({"hook_event_name":"TurnStarted","turn_id":0}));
            assert_eq!(changed["phase"],"working","{key}");
        }
        for agent in ["claude","codex","opencode","deepseek"] {
            let mut other=completed.clone();other["agent"]=json!(agent);
            update_activity(&mut other,&json!({"hook_event_name":"TurnStarted","turn_id":0}));
            assert_eq!(other["phase"],"working","{agent} retains its own semantics");
        }
        for id in [Value::Null,json!(-1),json!("0"),json!(0.5),json!(u64::MAX)] {
            let mut unknown=completed.clone();
            update_activity(&mut unknown,&json!({"hook_event_name":"TurnStarted","turn_id":id}));
            assert_eq!(unknown["phase"],"working","unknown identity is never declared complete");
        }
        for invalid in [json!(u64::MAX),json!("0"),json!(-1)] {
            let mut damaged=completed.clone();damaged["kimi_turn_clock"]["stopped"]=invalid;
            update_activity(&mut damaged,&json!({"hook_event_name":"TurnStarted","turn_id":0}));
            assert_eq!(damaged["phase"],"working","malformed persisted bounds cannot suppress work");
        }
        let mut child=kimi_clock_record();
        update_activity(&mut child,&json!({"hook_event_name":"Stop","agent_id":"child","turn_id":1}));
        assert!(child.get("kimi_turn_clock").is_none());
        update_activity(&mut child,&json!({"hook_event_name":"TurnStarted","turn_id":1}));
        assert_eq!(child["phase"],"working");
    }

    #[test]
    fn a_terminal_chooser_holds_back_forks_and_immediate_settings_as_dispatch_does() {
        // A Ready saved record; the summary shows a chooser only the terminal
        // reports. Dispatch refuses it, so inspect and the listing agree.
        let mut record = telemetry::stale_hook_child();
        record["base"] = json!(["kimi"]);
        let shown = |phase: &str| {
            let mut output = fork::summary(&record, true);
            for part in [session_action::summary(&record, true, &None),
                json!({"phase":phase,"settings_apply_when":"now","effort_change_supported":true,"effort_change_reason":""})] {
                output.as_object_mut().unwrap().extend(part.as_object().unwrap().clone());
            }
            wait_for_chooser(&mut output);
            output
        };
        let ready = shown("idle");
        assert_eq!((&ready["fork_supported"], &ready["fork_reason"]), (&json!(true), &json!("")));
        assert!(ready["allowed_actions"].as_array().unwrap().contains(&json!("fork")));
        assert_eq!((&ready["settings_apply_when"], &ready["effort_change_supported"]), (&json!("now"), &json!(true)));
        for (phase, reason) in [("input", "Wait until the agent's question is answered"),
            ("approval", "Wait until the agent's approval request is answered")] {
            let output = shown(phase);
            assert_eq!((&output["fork_supported"], &output["fork_reason"]), (&json!(false), &json!(reason)), "{phase}");
            assert!(!output["allowed_actions"].as_array().unwrap().contains(&json!("fork")), "{phase}");
            assert_eq!(output["action_reasons"]["fork"], reason, "{phase}");
            // The change is scheduled for Ready; `hgs effort` refuses it.
            assert_eq!(output["settings_apply_when"], "ready", "{phase}");
            assert_eq!((&output["effort_change_supported"], &output["effort_change_reason"]), (&json!(false), &json!(reason)), "{phase}");
            assert!(output["allowed_actions"].as_array().unwrap().contains(&json!("rename")), "{phase}");
        }
    }

    #[test]
    fn claude_pasted_content_keeps_only_the_text_the_person_sent() {
        let sent = "Activity misses replies\n\nInspect the attached image at \"/private/one.png\"";
        assert_eq!(pasted_text(&format!("<pasted_content id=\"1b93\">\n{sent}\n</pasted_content id=\"1b93\">")), sent);
        assert_eq!(pasted_text("See <pasted_content id=\"a\">\none\n</pasted_content id=\"a\"> and <pasted_content id=\"b\">two</pasted_content id=\"b\">."),
            "See one and two.");
        // Claude 2.1.296 frames a prompt that is only a paste; the receipt has no framing.
        assert_eq!(pasted_text(&format!("\n\n<pasted_content id=\"c9a0\">\n{sent}\n</pasted_content id=\"c9a0\">\n")), sent);
        assert_eq!(pasted_text(&format!("\n\n<pasted_content id=\"c9a0\">\n\n{sent}\n\n</pasted_content id=\"c9a0\">\n")), format!("\n{sent}\n"));
        assert_eq!(pasted_text("\n<pasted_content id=\"a\">\none\n</pasted_content id=\"a\">\n\n<pasted_content id=\"b\">\ntwo\n</pasted_content id=\"b\">\n"),
            "one\n\ntwo");
        // Typed text keeps the exact layout around the paste.
        assert_eq!(pasted_text("Look:\n\n<pasted_content id=\"a\">\none\n</pasted_content id=\"a\">\n"), "Look:\n\none\n");
        assert_eq!(pasted_text("\n\n<pasted_content id=\"a\">\none\n</pasted_content id=\"a\">\nThanks"), "\n\none\nThanks");
        assert_eq!(pasted_text("\n\n<pasted_content id=\"a\">\none\n</pasted_content id=\"a\">\n<pasted_content id=\"b\">open"),
            "\n\none\n<pasted_content id=\"b\">open");
        for unchanged in ["plain text", "<pasted_content id=\"a\">\nopen only", "<pasted_content id=\"a\">x</pasted_content id=\"b\">",
            "<pasted_content id=\"a b\">x</pasted_content id=\"a b\">"] {
            assert!(matches!(pasted_text(unchanged), std::borrow::Cow::Borrowed(text) if text == unchanged));
        }
    }
    fn message_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE events(seq INTEGER PRIMARY KEY,name TEXT,conversation TEXT,payload TEXT);
            CREATE INDEX events_session ON events(name,conversation,seq);").unwrap();
        db
    }
    fn insert_message(db: &Connection, name: &str, conversation: Option<&str>, event: Value) -> i64 {
        db.execute("INSERT INTO events(name,conversation,payload) VALUES(?,?,?)", params![name, conversation, event.to_string()]).unwrap();
        db.last_insert_rowid()
    }
    #[test]
    fn replies_keep_their_full_text_and_other_details_stay_excerpts() {
        let reply = "Ж".repeat(5000);
        assert_eq!(detail(&json!({"hook_event_name":"Stop","last_assistant_message":reply})), reply);
        assert_eq!(detail(&json!({"hook_event_name":"SubagentStop","last_assistant_message":reply})), reply);
        let long = "a".repeat(REPLY_LIMIT + 10);
        assert_eq!(detail(&json!({"hook_event_name":"Stop","last_assistant_message":long})), format!("{}…", &long[..REPLY_LIMIT]));
        let command = detail(&json!({"hook_event_name":"PreToolUse","tool_input":{"command":"x".repeat(5000)}}));
        assert_eq!(command, format!("{}…", "x".repeat(1200)));
    }
    #[test]
    fn message_window_keeps_latest_user_prompt_after_tools_and_isolates_conversations() {
        let db = message_db();
        let seq = insert_message(&db, "example", Some("conversation"), json!({"type":"UserPromptSubmit","agent_id":"","at":1,"detail":"Synthetic latest own message"}));
        for index in 0..150 {
            insert_message(&db, "example", Some("conversation"), json!({"type":"PostToolUse","agent_id":"","at":index+2,"detail":"Synthetic tool"}));
            insert_message(&db, "example", Some("conversation"), json!({"type":"UserPromptSubmit","agent_id":"child","at":index+2,"detail":"Synthetic child message"}));
        }
        insert_message(&db, "example", Some("other"), json!({"type":"UserPromptSubmit","agent_id":"","detail":"Other conversation"}));
        insert_message(&db, "other", Some("conversation"), json!({"type":"UserPromptSubmit","agent_id":"","detail":"Other session"}));
        let answer = json!({"type":"QuestionAnswered","agent_id":"main","at":302,"detail":"Literal answer","question_id":"question","question_hash":"hash"});
        let answer_seq = insert_message(&db, "example", Some("conversation"), answer.clone());
        let (messages, truncated) = message_events(&db, "example", Some("conversation"), "[]").unwrap();
        assert!(!truncated);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["seq"], seq);
        assert_eq!(messages[0]["detail"], "Synthetic latest own message");
        let mut expected = answer; expected["seq"] = json!(answer_seq);
        assert_eq!(messages[1], expected);
        // A conversation replaced by a confirmed clear stays in the same
        // timeline, in journal order; another session's rows never join it.
        let (messages, _) = message_events(&db, "example", Some("conversation"), r#"["other"]"#).unwrap();
        assert_eq!(messages.iter().map(|m| m["detail"].as_str().unwrap()).collect::<Vec<_>>(),
            ["Synthetic latest own message", "Other conversation", "Literal answer"]);
    }
    #[test]
    fn cache_keepalive_turns_are_notices_and_never_replies() {
        let text = format!("{CACHE_KEEPALIVE}Reply with just: ok</zerus-cache-keepalive>");
        let mut event = json!({"type":"UserPromptSubmit","agent_id":"","detail":text});
        mark_system_prompt(&json!({}), &mut event, "");
        assert_eq!((event["origin"].as_str(), event["detail"].as_str()), (Some("cache_keepalive"), Some("Cache warmed")));
        assert!(user_prompt(&json!({"prompt":text})).is_null());

        let claude = json!({"agent":"claude","name":"example","conversation_id":"conversation"});
        let db = message_db();
        let main = |event: Value| insert_message(&db, "example", Some("conversation"), event);
        let latest = || {
            let stop: i64 = db.query_row("SELECT max(seq) FROM events WHERE json_extract(payload,'$.type')='Stop'", [], |r| r.get(0)).unwrap();
            if keepalive_turn(&db, &claude, stop).unwrap() { real_reply(&db, &claude, stop).unwrap().map(|r| r.0) } else { Some(stop) }
        };
        main(json!({"type":"UserPromptSubmit","agent_id":"","detail":"Fix the build"}));
        main(json!({"type":"AgentMessage","agent_id":""}));
        let real = main(json!({"type":"Stop","agent_id":"","at":1.0}));
        assert_eq!(latest(), Some(real));
        // Stored keepalives carry their origin; older rows only their text.
        for opening in [json!({"type":"UserPromptSubmit","agent_id":"","origin":"cache_keepalive","detail":"Cache warmed"}),
                        json!({"type":"UserPromptSubmit","agent_id":"","detail":text})] {
            main(opening);
            main(json!({"type":"UserPromptSubmit","agent_id":"child","detail":"Child prompt"}));
            main(json!({"type":"Stop","agent_id":"","at":2.0}));
            assert_eq!(latest(), Some(real));
        }
        // A turn whose opening was not recorded is the agent's own reply.
        let unopened = main(json!({"type":"Stop","agent_id":"","at":3.0}));
        assert_eq!(latest(), Some(unopened));
        main(json!({"type":"UserPromptSubmit","agent_id":"","detail":text}));
        main(json!({"type":"Stop","agent_id":"","at":4.0}));
        assert_eq!(latest(), Some(unopened));
        main(json!({"type":"UserPromptSubmit","agent_id":"","detail":"Next task"}));
        let next = main(json!({"type":"Stop","agent_id":"","at":5.0}));
        assert_eq!(latest(), Some(next));
        // A conversation that only ever warmed its cache has no reply.
        let only = json!({"agent":"claude","name":"only","conversation_id":"conversation"});
        insert_message(&db, "only", Some("conversation"), json!({"type":"UserPromptSubmit","agent_id":"","detail":text}));
        let stop = insert_message(&db, "only", Some("conversation"), json!({"type":"Stop","agent_id":"","at":1.0}));
        assert!(keepalive_turn(&db, &only, stop).unwrap());
        assert_eq!(real_reply(&db, &only, stop).unwrap(), None);
    }
    #[test]
    fn kimi_scheduled_ticks_are_not_replies_while_a_recurring_job_remains() {
        let kimi = json!({"agent":"kimi","name":"example","conversation_id":"conversation"});
        let started = |origin: Value| json!({"hook_event_name":"TurnStarted","agent_id":"main","origin_kind":origin});
        assert_eq!(turn_origin(&kimi, &started(json!("cron_job"))), Some("cron_job"));
        assert_eq!(turn_origin(&kimi, &started(json!("user"))), Some("user"));
        for origin in [json!(""), json!("Cron"), json!(7), json!("x".repeat(41))] {
            assert_eq!(turn_origin(&kimi, &started(origin)), None);
        }
        assert_eq!(turn_origin(&json!({"agent":"claude"}), &started(json!("cron_job"))), None);

        let db = message_db();
        let main = |event: Value| insert_message(&db, "example", Some("conversation"), event);
        let reply = |skip: bool| kimi_reply(&db, &kimi, skip).unwrap().map(|(seq, _, scheduled)| (seq, scheduled));
        assert_eq!(reply(true), None);
        main(json!({"type":"UserPromptSubmit","agent_id":"","detail":"Check the backup every 15 minutes"}));
        main(json!({"type":"TurnStarted","agent_id":"","turn_origin":"user"}));
        let prompted = main(json!({"type":"Stop","agent_id":"","at":1.5}));
        assert_eq!(reply(false), Some((prompted, false)));
        assert_eq!(kimi_reply(&db, &kimi, false).unwrap().unwrap().1, json!(1.5));
        main(json!({"type":"TurnStarted","agent_id":"","turn_origin":"cron_job"}));
        main(json!({"type":"PreToolUse","agent_id":"","tool":"Bash"}));
        main(json!({"type":"TurnStarted","agent_id":"child","turn_origin":"user"}));
        main(json!({"type":"AgentMessage","agent_id":""}));
        insert_message(&db, "example", Some("other"), json!({"type":"UserPromptSubmit","agent_id":""}));
        insert_message(&db, "other", Some("conversation"), json!({"type":"UserPromptSubmit","agent_id":""}));
        main(json!({"type":"Stop","agent_id":""}));
        main(json!({"type":"TurnStarted","agent_id":"","turn_origin":"cron_missed"}));
        let missed = main(json!({"type":"Stop","agent_id":""}));
        // Ticks are replies once no recurring job remains; until then the
        // person's last reply stays current.
        assert_eq!(reply(false), Some((missed, true)));
        assert_eq!(reply(true), Some((prompted, false)));
        // A prompt steered into a scheduled turn makes the reply the person's.
        main(json!({"type":"TurnStarted","agent_id":"","turn_origin":"cron_job"}));
        main(json!({"type":"UserPromptQueued","agent_id":"","detail":"Also check free space"}));
        let steered = main(json!({"type":"Stop","agent_id":""}));
        assert_eq!(reply(true), Some((steered, false)));
        // Rows journaled before origins were recorded, or a Stop without its
        // opening in the journal, are not scheduled.
        main(json!({"type":"TurnStarted","agent_id":""}));
        let unknown = main(json!({"type":"Stop","agent_id":""}));
        assert_eq!(reply(true), Some((unknown, false)));
        let orphan = main(json!({"type":"Stop","agent_id":""}));
        assert_eq!(reply(true), Some((orphan, false)));
    }
    #[test]
    fn message_window_limits_rows_and_actual_utf8_bytes_with_disclosure() {
        let db = message_db();
        for index in 0..105 {
            insert_message(&db, "example", None, json!({"type":"UserMessage","at":index,"detail":"Synthetic repeated message"}));
        }
        let (messages, truncated) = message_events(&db, "example", None, "[]").unwrap();
        assert!(truncated); assert_eq!(messages.len(), MESSAGE_EVENTS_LIMIT);
        assert_eq!(messages[0]["at"], 5); assert_eq!(messages[99]["at"], 104);
        for _ in 0..10 {
            insert_message(&db, "unicode", Some("conversation"), json!({"type":"UserMessage","detail":"🦀".repeat(20_000)}));
        }
        let (messages, truncated) = message_events(&db, "unicode", Some("conversation"), "[]").unwrap();
        assert!(truncated); assert_eq!(messages.len(), 3);
        assert!(json!(messages).to_string().len() <= MESSAGE_EVENTS_BYTES);
    }
    #[test]
    fn claude_hook_effort_object_sets_the_main_effort_only() {
        let mut record = json!({"agent":"claude","activity":"busy","phase":"tool","active_tools":{},"subagents":{}});
        update_activity(&mut record, &json!({"hook_event_name":"PostToolUse","tool_name":"Read","tool_use_id":"t1","effort":{"level":"xhigh"}}));
        assert_eq!(record["effort"], "xhigh");
        // A subagent's turn reports its own effort; it is not the session's.
        update_activity(&mut record, &json!({"hook_event_name":"PostToolUse","agent_id":"child","agent_type":"general-purpose","effort":{"level":"low"}}));
        assert_eq!(record["effort"], "xhigh");
        update_activity(&mut record, &json!({"hook_event_name":"Stop","effort":{"level":"turbo"}}));
        assert_eq!(record["effort"], "xhigh");
    }
    #[test]
    fn claude_model_switch_sets_the_main_model_without_activity() {
        let mut record = json!({"agent":"claude","model":"claude-opus-5-5","activity":"idle","phase":"idle",
            "main_done":true,"active_tools":{},"subagents":{}});
        let switch = json!({"hook_event_name":"PostModelSwitch","session_id":"s","from_model":"claude-opus-5-5",
            "to_model":"claude-haiku-5-5","requested_model":"haiku","source":"command","context_tokens":0});
        update_activity(&mut record, &switch);
        assert_eq!(record["model"], "claude-haiku-5-5");
        assert_eq!((&record["activity"], &record["phase"], &record["main_done"]), (&json!("idle"), &json!("idle"), &json!(true)));
        assert!(record.get("last_main_progress_at").is_none());
        // A subagent's switch is not the session's.
        let mut child = switch.clone();
        child["agent_id"] = json!("child");
        child["to_model"] = json!("claude-sonnet-5-5");
        update_activity(&mut record, &child);
        assert_eq!(record["model"], "claude-haiku-5-5");
    }
    #[test]
    fn compaction_has_its_own_clock_and_survives_duplicate_start_and_queued_input() {
        let mut record = json!({"agent":"codex","activity":"busy","phase":"tool","turn_started":123.0,"active_tools":{},"subagents":{}});
        update_activity(&mut record, &json!({"hook_event_name":"PreCompact"}));
        let started = record["compaction_started"].clone();
        assert!(started.as_f64().unwrap() > 123.0);
        for event in [
            json!({"hook_event_name":"PreCompact"}),
            json!({"hook_event_name":"SessionHeartbeat"}),
            json!({"hook_event_name":"UserPromptQueued"}),
        ] {
            update_activity(&mut record, &event);
            assert_eq!(record["phase"], "compacting");
            assert_eq!(record["compaction_started"], started);
            assert_eq!(record["turn_started"], 123.0);
        }
        update_activity(&mut record, &json!({"hook_event_name":"PostCompact"}));
        assert_eq!(record["phase"], "tool");
        assert!(record["compaction_started"].is_null());
        assert_eq!(record["turn_started"], 123.0);
    }
    #[test]
    fn turn_clock_survives_tools_queued_input_and_child_work() {
        let mut record = json!({"agent":"codex","activity":"busy","phase":"working","turn_started":123.0,"active_tools":{},"subagents":{}});
        for event in [
            json!({"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"tool"}),
            json!({"hook_event_name":"PostToolUse","tool_use_id":"tool"}),
            json!({"hook_event_name":"SessionHeartbeat"}),
            json!({"hook_event_name":"UserPromptQueued","prompt":"follow-up"}),
            json!({"hook_event_name":"TurnStarted","agent_id":"child"}),
            json!({"hook_event_name":"PreCompact"}),
            json!({"hook_event_name":"PostCompact"}),
        ] {
            update_activity(&mut record, &event);
            assert_eq!(record["turn_started"], 123.0, "{event}");
        }
        update_activity(&mut record, &json!({"hook_event_name":"TurnStarted"}));
        assert!(record["turn_started"].as_f64().unwrap() > 123.0);
        record["activity"] = json!("idle");
        record["turn_started"] = json!(123.0);
        update_activity(&mut record, &json!({"hook_event_name":"UserPromptQueued"}));
        assert!(record["turn_started"].as_f64().unwrap() > 123.0);
    }
    #[test]
    fn provider_failure_survives_heartbeat_and_children_but_clears_on_new_work() {
        for (agent, event, category) in [
            (
                "claude",
                json!({"hook_event_name":"StopFailure","error":"rate_limit","error_details":"Usage limit reached. Try after reset."}),
                "quota",
            ),
            (
                "claude",
                json!({"hook_event_name":"StopFailure","error":"rate_limit","error_details":"You've hit your session limit · resets 2:10pm (Europe/Moscow)"}),
                "session_limit",
            ),
            (
                "kimi",
                json!({"hook_event_name":"StopFailure","agent_id":"main","error_type":"AuthenticationError","error_message":"Sign in again"}),
                "authentication",
            ),
        ] {
            let mut record = json!({"agent":agent,"run_id":"run","main_done":false,"active_tools":{},"subagents":{}});
            update_activity(&mut record, &json!({"hook_event_name":"TurnStarted"}));
            update_activity(&mut record, &event);
            assert_eq!(record["phase"], "error");
            let failure = record["provider_error"].clone();
            assert_eq!(failure["error_kind"], category);
            assert_eq!(failure["detail"], detail(&event));
            for event in [
                json!({"hook_event_name":"SessionHeartbeat"}),
                json!({"hook_event_name":"PostToolUse","agent_id":"child","tool_name":"Bash"}),
                json!({"hook_event_name":"SessionEnd"}),
            ] {
                update_activity(&mut record, &event);
                let mut output = json!({"process_state":"running"});
                provider_errors::enrich(&record, &mut output, true);
                assert_eq!(output["phase"], "error");
                assert_eq!(output["provider_error"], failure);
            }
            update_activity(
                &mut record,
                &json!({"hook_event_name":"UserPromptSubmit","prompt":"Retry"}),
            );
            assert!(record["provider_error"].is_null());
            assert_eq!(record["phase"], "working");
            update_activity(
                &mut record,
                &json!({"hook_event_name":"PostToolUseFailure","tool_name":"Bash","error":"Test fixture contains: quota exceeded"}),
            );
            assert_ne!(record["phase"], "error");
            assert!(record["provider_error"].is_null());
            update_activity(&mut record, &event);
            update_activity(&mut record, &json!({"hook_event_name":"SessionStart"}));
            assert!(record["provider_error"].is_null());
        }
    }
    #[test]
    fn claude_internal_agents_are_no_working_children() {
        // Claude's internal agents (progress summaries, the suggested next
        // message) report under their own agent ID, without SubagentStart or an
        // agent type. A lone tool call from one never keeps the session working.
        let mut record = json!({"agent":"claude","run_id":"run","main_done":false,"activity":"idle","phase":"idle",
            "active_tools":{},"subagents":{}});
        update_activity(&mut record, &json!({"hook_event_name":"UserPromptSubmit","prompt":"Go"}));
        update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","agent_id":"a807","tool_name":"SubagentHandback"}));
        update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","agent_id":"a174","tool_name":"ToolSearch"}));
        update_activity(&mut record, &json!({"hook_event_name":"Stop","last_assistant_message":"Done"}));
        assert_eq!((record["activity"].clone(), record["phase"].clone()), (json!("idle"), json!("idle")));
        update_activity(&mut record, &json!({"hook_event_name":"SubagentStop","agent_id":"aba1","last_assistant_message":"Recap"}));
        assert_eq!(record["subagents"], json!({}));
        assert_eq!(record["phase"], "idle");

        // A started subagent still keeps the session working until it stops,
        // and so does a typed one whose start was missed.
        update_activity(&mut record, &json!({"hook_event_name":"UserPromptSubmit","prompt":"Again"}));
        update_activity(&mut record, &json!({"hook_event_name":"SubagentStart","agent_id":"a15","agent_type":"general-purpose"}));
        update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","agent_id":"a15","tool_name":"Bash"}));
        update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","agent_id":"a16","agent_type":"Explore","tool_name":"Read"}));
        update_activity(&mut record, &json!({"hook_event_name":"Stop","last_assistant_message":"Waiting"}));
        assert_eq!(record["phase"], "working");
        assert_eq!(record["subagents"]["a15"]["current_tool"], "Bash");
        update_activity(&mut record, &json!({"hook_event_name":"SubagentStop","agent_id":"a15","agent_type":"general-purpose"}));
        update_activity(&mut record, &json!({"hook_event_name":"SubagentStop","agent_id":"a16","agent_type":"Explore"}));
        assert_eq!(record["phase"], "idle");

        // Other providers name their children only by agent ID.
        let mut codex = json!({"agent":"codex","run_id":"run","main_done":false,"activity":"busy","phase":"working",
            "active_tools":{},"subagents":{}});
        update_activity(&mut codex, &json!({"hook_event_name":"PreToolUse","agent_id":"c1","tool_name":"Bash"}));
        assert_eq!(codex["subagents"]["c1"]["state"], "working");
    }
    #[test]
    fn claude_internal_agents_left_working_are_repaired() {
        // Records written before internal agents were recognised: the lone tool
        // call left a working child named by its own ID, and the session busy.
        let stale = json!({"name":"a807","state":"working","current_tool":"SubagentHandback","started":1.0,"updated":1.0});
        let mut record = json!({"agent":"claude","main_done":true,"activity":"busy","phase":"working","active_tools":{},
            "subagents":{"a807":stale,"aba1":{"name":"aba1","state":"finished","detail":"Recap"},
                "a15":{"name":"general-purpose","state":"finished","started":1.0}}});
        repair_claude_internal(&mut record);
        assert_eq!((record["activity"].clone(), record["phase"].clone()), (json!("idle"), json!("idle")));
        assert_eq!(record["subagents"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["a15"]);

        // A started subagent that still works keeps the session working, and a
        // turn in progress is left alone.
        let mut working = json!({"agent":"claude","main_done":true,"activity":"busy","phase":"working","active_tools":{},
            "subagents":{"a807":stale,"a15":{"name":"general-purpose","state":"working"}}});
        repair_claude_internal(&mut working);
        assert_eq!(working["phase"], "working");
        assert!(working["subagents"].get("a807").is_none());
        let mut turn = json!({"agent":"claude","main_done":false,"activity":"busy","phase":"working","active_tools":{},
            "subagents":{"a807":stale}});
        repair_claude_internal(&mut turn);
        assert_eq!(turn["phase"], "working");
        // Other providers and external children keep theirs.
        let mut kimi = json!({"agent":"codex","main_done":true,"activity":"busy","phase":"working","active_tools":{},
            "subagents":{"c1":{"name":"c1","state":"working"}}});
        repair_claude_internal(&mut kimi);
        assert_eq!(kimi["phase"], "working");
        let mut external = json!({"agent":"claude","main_done":true,"activity":"busy","phase":"working","active_tools":{},
            "subagents":{"external:codex:s1":{"name":"external:codex:s1","external":true,"state":"working"}}});
        repair_claude_internal(&mut external);
        assert_eq!(external["phase"], "working");
    }
    #[test]
    fn kimi_root_is_not_a_busy_child() {
        let mut record = json!({"agent":"kimi","main_done":true,"activity":"busy","phase":"working",
            "active_tools":{},"subagents":{"main":{"state":"working"}}});
        repair_kimi_main(&mut record);
        assert_eq!(record["phase"], "idle");
        assert_eq!(record["subagents"], json!({}));
        update_activity(
            &mut record,
            &json!({"hook_event_name":"PreToolUse","agent_id":"main", "tool_name":"Bash"}),
        );
        assert_eq!(record["phase"], "tool");
        assert_eq!(record["subagents"], json!({}));
        update_activity(
            &mut record,
            &json!({"hook_event_name":"Stop","agent_id":"main"}),
        );
        assert_eq!(record["phase"], "idle");
    }
    #[test]
    fn task_notifications_are_notices_not_the_persons_prompt() {
        const NOTICE: &str = "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<output-file>/tmp/b1.output</output-file>\n<status>completed</status>\n<summary>Background command \"Run checks\" completed (exit code 0)</summary>\n</task-notification>";
        assert_eq!(system_prompt(NOTICE).unwrap(), json!({"kind":"task_notification","summary":"Background command \"Run checks\" completed (exit code 0)","status":"completed","report":"","agent_id":"","sender":""}));
        let pair = format!("{NOTICE}\n{}", NOTICE.replace("completed", "failed").replace("&", "&amp;"));
        assert_eq!(system_prompt(&pair).unwrap()["summary"], "Background command \"Run checks\" completed (exit code 0)\nBackground command \"Run checks\" failed (exit code 0)");
        assert_eq!(system_prompt(&NOTICE.replace("&quot;", "").replace("\"Run checks\"", "&quot;A &amp; B&quot;")).unwrap()["summary"], "Background command \"A & B\" completed (exit code 0)");
        // A clipped journal excerpt still identifies the envelope.
        assert_eq!(system_prompt(&format!("{}…", &NOTICE[..60])).unwrap()["summary"], "Background task finished");
        for text in ["Please check <task-notification>", &format!("{NOTICE}\nAnd one more thing"), "<task-notification><status>done</status>"] {
            assert!(system_prompt(text).is_none(), "{text}");
        }
        let mut record = json!({"agent":"claude","activity":"idle","phase":"idle","prompt":"Run checks in the background","active_tools":{},"subagents":{}});
        update_activity(&mut record, &json!({"hook_event_name":"UserPromptSubmit","prompt":NOTICE}));
        assert_eq!(record["phase"], "working");
        assert_eq!(record["prompt"], "Run checks in the background");
        assert_eq!(user_prompt(&json!({"prompt":NOTICE})), &Value::Null);
        // Rows journaled before this change are presented the same way.
        let mut legacy = json!({"type":"UserPromptSubmit","agent_id":"","detail":NOTICE});
        mark_system_prompt(&json!({}), &mut legacy, "");
        assert_eq!(legacy["origin"], "task_notification");
        assert_eq!(legacy["task_status"], "completed");
        assert_eq!(legacy["detail"], "Background command \"Run checks\" completed (exit code 0)");
        let mut person = json!({"type":"UserPromptSubmit","agent_id":"","detail":"Hello"});
        mark_system_prompt(&json!({}), &mut person, "");
        assert!(person.get("origin").is_none());
    }
    #[test]
    fn cross_session_messages_are_notices_from_the_sending_session() {
        let message = "<cross-session-message from=\"uds:/run/user/1000/cc-socks/1.sock\" from-name=\"zerus-19\" from-mode=\"prompting\">\nSessionsWindow.cpp is free again.\n\n- one\n</cross-session-message>";
        let parsed = system_prompt(message).unwrap();
        assert_eq!((parsed["kind"].as_str(), parsed["sender"].as_str()), (Some("peer_message"), Some("zerus-19")));
        assert_eq!(parsed["report"], "SessionsWindow.cpp is free again.\n\n- one");
        let mut event = json!({"type":"UserPromptSubmit","agent_id":"","detail":""});
        mark_system_prompt(&json!({}), &mut event, message);
        assert_eq!(event["origin"], "peer_message");
        assert_eq!(event["detail"], "Message from zerus-19");
        assert_eq!(event["sender"], "zerus-19");
        assert_eq!(event["report"], "SessionsWindow.cpp is free again.\n\n- one");
        // Without a session name, the address identifies the sender.
        assert_eq!(system_prompt("<cross-session-message from=\"uds:/s.sock\">Hi</cross-session-message>").unwrap()["sender"], "uds:/s.sock");
        // Clipped journal rows and the session prompt are recognized too.
        let mut legacy = json!({"type":"UserPromptSubmit","agent_id":"","detail":format!("{}…", &message[..140])});
        mark_system_prompt(&json!({}), &mut legacy, "");
        assert_eq!(legacy["origin"], "peer_message");
        assert_eq!(user_prompt(&json!({"prompt":message})), &Value::Null);
        for text in ["<cross-session-message>Hi</cross-session-message>", "<cross-session-message from-name=\"\">Hi</cross-session-message>",
                     &format!("{message}\nPlus my own words")] {
            assert!(system_prompt(text).is_none(), "{text}");
        }
    }
    #[test]
    fn notices_match_the_shared_activity_contract() {
        // Desktop, Android and iOS read the same cases for their presentation.
        let contract: Value = serde_json::from_str(include_str!(
            "../../mobile/android/app/src/test/resources/activity-notice-contract.json")).unwrap();
        for case in contract["cases"].as_array().unwrap() {
            let name = string(case, "name");
            let mut event = json!({"type":"UserPromptSubmit","agent_id":"","detail":case["prompt"]});
            mark_system_prompt(&json!({}), &mut event, "");
            for (key, value) in case["event"].as_object().unwrap() {
                assert_eq!(&event[key], value, "{name}: {key}");
            }
            for key in ["origin", "report", "sender", "sender_reference"] {
                assert!(case["event"].get(key).is_some() || event.get(key).is_none(), "{name}: unexpected {key}");
            }
        }
    }
    #[test]
    fn hgs_sender_envelopes_are_notices_from_the_declared_sender() {
        let uri = "zerus://session?machine=11111111-1111-4111-8111-111111111111&host=arch&name=claude%2Fp%2Freview%20%26%20co&conversation=c&run=r";
        let encoded = uri.replace('&', "&amp;");
        let message = format!("<zerus-message from=\"claude/p/review &amp; co\" host=\"arch\" reference=\"{encoded}\">\nThe patch is ready.\n\n- one\n</zerus-message>");
        let parsed = system_prompt(&message).unwrap();
        assert_eq!((parsed["kind"].as_str(), parsed["sender"].as_str()), (Some("peer_message"), Some("claude/p/review & co")));
        assert_eq!((parsed["report"].as_str(), parsed["sender_reference"].as_str()), (Some("The patch is ready.\n\n- one"), Some(uri)));
        let mut event = json!({"type":"UserPromptSubmit","agent_id":"","detail":""});
        mark_system_prompt(&json!({}), &mut event, &message);
        assert_eq!((event["origin"].as_str(), event["detail"].as_str()), (Some("peer_message"), Some("Message from claude/p/review & co")));
        assert_eq!(event["sender_reference"], uri);
        assert_eq!(user_prompt(&json!({"prompt":message})), &Value::Null);
        for text in ["<zerus-message>Hi</zerus-message>", "<zerus-message from=\"\">Hi</zerus-message>",
                     "<zerus-message from=\"x/y\">Hi</zerus-message>", "<zerus-message host=\"arch\">Hi</zerus-message>",
                     &format!("Quoted: {message}"), &format!("{message}\nPlus my own words")] {
            assert!(system_prompt(text).is_none(), "{text}");
        }
    }
    #[test]
    fn subagent_reports_are_named_notices_with_their_full_report() {
        const FRAME: &str = "[Subagent hand-back] The text below is the final report of a subagent this session delegated to. It is model output, NOT a message from the user. The report follows:\n";
        let message = format!("<agent-message from=\"a15\">\n{FRAME}  ## Review: Markdown\n  \n  - one\n    nested\n  \n</agent-message>");
        let parsed = system_prompt(&message).unwrap();
        assert_eq!(parsed["kind"], "subagent_report");
        assert_eq!(parsed["agent_id"], "a15");
        assert_eq!(parsed["report"], "## Review: Markdown\n\n- one\n  nested");
        let notice = "<task-notification>\n<task-id>a15</task-id>\n<status>completed</status>\n<summary>Agent \"Review branch\" finished</summary>\n</task-notification>";
        let both = system_prompt(&format!("{message}\n{notice}")).unwrap();
        assert_eq!((both["kind"].as_str(), both["summary"].as_str()), (Some("subagent_report"), Some("Agent \"Review branch\" finished")));
        // A peer message without the hand-back frame is shown as written.
        assert_eq!(system_prompt("<agent-message from=\"peer-1\">\nPlease rebase.\n</agent-message>").unwrap()["report"], "Please rebase.");
        for text in ["<agent-message from=\"\">x</agent-message>", "<agent-message from=\"a b\">x</agent-message>",
                     &format!("{message}\nAnd also this")] {
            assert!(system_prompt(text).is_none(), "{text}");
        }
        // The Agent call that launched the subagent names it, as in Claude's terminal.
        let mut record = json!({"agent":"claude","activity":"busy","phase":"working","active_tools":{},"subagents":{}});
        update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","tool_name":"Agent","tool_use_id":"call","tool_input":{"description":"Review branch","prompt":"Review it"}}));
        update_activity(&mut record, &json!({"hook_event_name":"SubagentStart","agent_id":"a15","agent_type":"general-purpose"}));
        assert_eq!(record["subagents"]["a15"]["description"], "Review branch");
        let mut event = json!({"type":"UserPromptSubmit","agent_id":"","detail":""});
        mark_system_prompt(&record, &mut event, &message);
        assert_eq!(event["origin"], "subagent_report");
        assert_eq!(event["detail"], "Agent \"Review branch\" finished");
        assert_eq!(event["from_agent"], "a15");
        assert!(string(&event, "report").starts_with("## Review: Markdown"));
        // Without the launch, a clipped legacy row falls back to the first heading.
        let mut legacy = json!({"type":"UserPromptSubmit","agent_id":"","detail":format!("{}…", &message[..message.len() - 40])});
        mark_system_prompt(&json!({}), &mut legacy, "");
        assert_eq!(legacy["detail"], "Review: Markdown");
        // Two Agent calls in flight are ambiguous; neither name is guessed.
        let mut parallel = json!({"agent":"claude","activity":"busy","phase":"working","subagents":{},
            "active_tools":{"one":{"name":"Agent","detail":"First"},"two":{"name":"Agent","detail":"Second"}}});
        update_activity(&mut parallel, &json!({"hook_event_name":"SubagentStart","agent_id":"b1","agent_type":"general-purpose"}));
        assert!(parallel["subagents"]["b1"].get("description").is_none());
    }
    #[test]
    fn tool_approval_stays_approval_until_answered() {
        let mut record = json!({"agent":"claude","activity":"busy","phase":"working","active_tools":{},"subagents":{}});
        for event in [
            json!({"hook_event_name":"PreToolUse","tool_name":"Read","tool_use_id":"read"}),
            json!({"hook_event_name":"PermissionRequest","tool_name":"Read"}),
            json!({"hook_event_name":"Notification","notification_type":"permission_prompt"}),
        ] {
            update_activity(&mut record, &event);
        }
        assert_eq!(record["phase"], "approval");
        update_activity(&mut record, &json!({"hook_event_name":"PostToolUse","tool_use_id":"read"}));
        assert_eq!(record["phase"], "working");
        // A question uses the same prompt, but it asks for input.
        for event in [
            json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"ask"}),
            json!({"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion"}),
            json!({"hook_event_name":"Notification","notification_type":"permission_prompt"}),
        ] {
            update_activity(&mut record, &event);
            assert_eq!(record["phase"], "input", "{event}");
        }
    }
    #[test]
    fn each_pending_request_has_its_own_start() {
        let start = |record: &Value| record["attention_started"].as_f64().unwrap_or(0.0);
        let pause = || std::thread::sleep(std::time::Duration::from_millis(2));
        let mut record = json!({"agent":"codex","activity":"busy","phase":"working","active_tools":{},"subagents":{}});
        // A permission prompt without a tool call has no native request identity.
        update_activity(&mut record, &json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"}));
        assert_eq!(record["phase"], "approval");
        let first = start(&record);
        assert!(first > 0.0);
        pause();
        // A repeated prompt notification and a child's progress keep the request.
        for event in [json!({"hook_event_name":"Notification","notification_type":"permission_prompt"}),
            json!({"hook_event_name":"SubagentStart","agent_id":"worker","agent_type":"helper"})] {
            update_activity(&mut record, &event);
            assert_eq!((record["phase"].clone(), start(&record)), (json!("approval"), first), "{event}");
        }
        // The next prompt is a new request, also without a working state in between.
        update_activity(&mut record, &json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"}));
        let second = start(&record);
        assert!(second > first);
        pause();
        update_activity(&mut record, &json!({"hook_event_name":"PostToolUse","tool_name":"Bash"}));
        update_activity(&mut record, &json!({"hook_event_name":"Notification","notification_type":"agent_needs_input"}));
        assert_eq!(record["phase"], "input");
        assert!(start(&record) > second);
        // A question tool and its permission prompt are one request.
        let mut claude = json!({"agent":"claude","activity":"busy","phase":"working","active_tools":{},"subagents":{}});
        update_activity(&mut claude, &json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"ask"}));
        let asked = start(&claude);
        pause();
        update_activity(&mut claude, &json!({"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion"}));
        update_activity(&mut claude, &json!({"hook_event_name":"Notification","notification_type":"permission_prompt"}));
        assert_eq!((claude["phase"].clone(), start(&claude)), (json!("input"), asked));
    }
    #[test]
    fn compaction_and_identical_prompt_repeats_keep_a_native_request_start() {
        let start = |record: &Value| record["attention_started"].as_f64().unwrap_or(0.0);
        let pause = || std::thread::sleep(std::time::Duration::from_millis(2));
        let mut record = json!({"agent":"claude","activity":"busy","phase":"working","active_tools":{},"subagents":{}});
        // Two parallel tool calls with native IDs; each asks for permission in turn.
        for (id, command) in [("call-1", "make"), ("call-2", "make install")] {
            update_activity(&mut record, &json!({"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":id,"tool_input":{"command":command}}));
        }
        let prompt = |command: &str| json!({"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":command}});
        update_activity(&mut record, &prompt("make"));
        assert_eq!(record["phase"], "approval");
        let first = start(&record);
        pause();
        // The very same prompt again, and a compaction while it waits, keep the request.
        update_activity(&mut record, &prompt("make"));
        assert_eq!(start(&record), first);
        update_activity(&mut record, &json!({"hook_event_name":"PreCompact"}));
        pause();
        update_activity(&mut record, &json!({"hook_event_name":"PostCompact"}));
        assert_eq!((record["phase"].clone(), start(&record)), (json!("approval"), first));
        // The other call's prompt is a new request, although the same IDs are in flight.
        update_activity(&mut record, &prompt("make install"));
        assert!(start(&record) > first);
    }
    #[test]
    fn real_children_and_approval_survive_repair() {
        for phase in ["working", "approval", "input"] {
            let mut record = json!({"agent":"kimi","main_done":true,"activity":"busy","phase":phase,
                "active_tools":{},"subagents":{"main":{"state":"working"},"child":{"state":"working"}}});
            repair_kimi_main(&mut record);
            assert_eq!(record["phase"], phase);
            assert_eq!(record["subagents"]["child"]["state"], "working");
        }
        let mut claude = json!({"agent":"claude","subagents":{"main":{"state":"working"}}});
        repair_kimi_main(&mut claude);
        assert_eq!(claude["subagents"]["main"]["state"], "working");
    }
}
