//! Native public messages and Claude's recorded thinking from the exact
//! conversation. User bodies hydrate Activity previews without adding rows.
use super::*;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

// A transcript line longer than this is never a public message.
const LINE_BYTES: usize = 1024 * 1024;

/// The Codex or Claude transcript of the record's exact conversation; None for
/// other agents or a path that does not name the conversation.
fn transcript(record: &Value) -> Option<(&str, &str, PathBuf)> {
    let agent = string(record, "agent");
    let id = string(record, "conversation_id");
    if !["codex", "claude", "qwen", "hermes"].contains(&agent) || uuid::Uuid::parse_str(id).is_err() {
        return None;
    }
    let path = PathBuf::from(string(record, "transcript"));
    (path.is_absolute() && path.file_name().is_some_and(|v| v.to_string_lossy().contains(id))).then_some((agent, id, path))
}

/// A fork's own messages begin when it was created. Older ones in its
/// transcript are copies of the history it began from, which its Activity
/// shows from the source session instead (zerus-b4qk).
fn own_since(record: &Value) -> f64 {
    if string(record, "fork_parent_id").is_empty() {
        0.0
    } else {
        record["created"].as_f64().unwrap_or(0.0)
    }
}

/// Only a message with a known time before the fork's creation is a copy.
fn own(message: &Value, since: f64) -> bool {
    !message["at"]
        .as_f64()
        .is_some_and(|at| at > 0.0 && at < since)
}

/// The first line of a transcript, at most 256 KiB; a Codex transcript must
/// name the conversation in its `session_meta`.
fn first_line(file: &mut File, agent: &str, id: &str) -> Result<Vec<u8>> {
    let mut first = Vec::new();
    BufReader::new(file.take(256 * 1024)).read_until(b'\n', &mut first).map_err(|e| e.to_string())?;
    if agent == "codex" {
        let meta: Value = serde_json::from_slice(&first).map_err(|e| e.to_string())?;
        if meta["type"] != "session_meta" || meta["payload"]["id"] != id {
            return Err("Conversation transcript identity mismatch".into());
        }
    }
    Ok(first)
}

/// The public messages one transcript line contributes: Claude's thinking and
/// assistant replies of the exact conversation, never private reasoning.
fn line_messages(agent: &str, id: &str, line: &[u8]) -> Vec<Value> {
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return vec![];
    };
    event_messages(agent, id, &event)
}

fn event_messages(agent: &str, id: &str, event: &Value) -> Vec<Value> {
    if matches!(agent, "claude" | "qwen" | "hermes") && !string(&event, "sessionId").is_empty() && event["sessionId"] != id {
        return vec![];
    }
    let mut messages = Vec::new();
    if agent == "claude" {
        messages.extend(claude_thinking(&event));
    }
    if let Some((message, role)) = search::provider_event(agent, &event) {
        if role == "user" {
            let mut body = journal::activity_message_body(
                string(&message, "source"),
                string(&message, "detail"),
                message["at"].as_f64().unwrap_or(0.0),
                string(&message, "message_id"),
            );
            body["conversation_id"] = json!(id);
            messages.push(body);
        } else {
            messages.push(message);
        }
    }
    messages
}

/// A parsed line kept by a watch subscription. Blobs retain only their public
/// message projection, never the request body or private reasoning.
pub(super) enum Projection {
    Messages(Vec<Value>),
    Blob(Vec<Value>),
    Request(Value),
}

pub(super) fn project(record: &Value, event: &Value, line: &[u8]) -> Projection {
    let agent = string(record, "agent");
    let id = string(record, "conversation_id");
    if agent == "claude" && line.starts_with(b"{\"type\":\"api-request") {
        if !string(event,"sessionId").is_empty() && event["sessionId"] != id {
            return Projection::Messages(Vec::new());
        }
        if event["type"] == "api-request" { return Projection::Request(event["timestamp"].clone()); }
        let mut messages = Vec::new();
        if event["type"] == "api-request-blob" && event["message"]["role"] == "assistant" {
            push_blob(&mut messages, agent,id,event.clone(),&Value::Null,0.);
        }
        return Projection::Blob(messages);
    }
    Projection::Messages(event_messages(agent,id,event))
}

/// The same ordering, duplicate and byte-window rules as read_activity.
/// Rebuilding this bounded public window never parses old transcript bytes.
pub(super) fn projected<'a>(record: &Value, lines: impl Iterator<Item=&'a Projection>) -> Vec<Value> {
    let since = own_since(record);
    let mut messages = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let flush = |messages: &mut Vec<Value>, pending: &mut Vec<Value>, at: &Value| {
        let at = search::timestamp(at);
        for mut message in pending.drain(..) {
            message["at"] = json!(at);
            if own(&message,since) && !messages.iter().any(|seen|seen["detail"] == message["detail"]) {
                messages.push(message);
            }
        }
    };
    for line in lines {
        match line {
            Projection::Messages(rows) => for message in rows {
                if own(message,since) && !repeated(&messages,message) { messages.push(message.clone()); }
            },
            Projection::Blob(rows) => pending.extend(rows.iter().cloned()),
            Projection::Request(at) => flush(&mut messages,&mut pending,at),
        }
    }
    flush(&mut messages,&mut pending,&projection_now());
    messages.sort_by(|a,b|a["at"].as_f64().unwrap_or(0.).total_cmp(&b["at"].as_f64().unwrap_or(0.)));
    let (mut bodies,mut replies) = (0,0);
    messages.reverse();
    messages.retain(|message| {
        let count = if message["type"] == "UserMessageBody" { &mut bodies } else { &mut replies };
        *count += 1;
        *count <= 100
    });
    messages.reverse();
    messages
}

/// Codex can emit both response_item and event_msg for the same public
/// message. Genuinely repeated later messages stay in the conversation, and
/// Claude's thinking is never a repeat.
fn repeated(recent: &[Value], message: &Value) -> bool {
    message["type"] != "AgentThinking" && recent.iter().rev().take(4).any(|m| {
        m["type"] == message["type"]
            && m["detail"] == message["detail"]
            && (m["at"].as_f64().unwrap_or(0.0) - message["at"].as_f64().unwrap_or(0.0)).abs() < 3.0
    })
}

/// An assistant message Claude stored as an `api-request-blob`, read as its
/// transcript line would be, at `at`; one already shown (by text) is skipped.
fn push_blob(messages: &mut Vec<Value>, agent: &str, id: &str, blob: Value, at: &Value, since: f64) {
    let event = json!({"type":"assistant","sessionId":id,"uuid":blob["hash"],"timestamp":at,"message":blob["message"]});
    let Ok(line) = serde_json::to_vec(&event) else { return };
    for message in line_messages(agent, id, &line) {
        if message["type"] == "AgentThinking" || message["detail"].as_str().unwrap_or("").trim().is_empty() {
            continue;
        }
        if own(&message, since) && !messages.iter().any(|seen| seen["detail"] == message["detail"]) {
            messages.push(message);
        }
    }
}

pub(super) fn read(record: &Value) -> Result<Vec<Value>> {
    // Subagent-history callers still receive only visible native rows.
    read_activity(record).map(|messages| {
        messages
            .into_iter()
            .filter(|message| message["type"] != "UserMessageBody")
            .collect()
    })
}

pub(super) fn read_activity(record: &Value) -> Result<Vec<Value>> {
    if let Some(messages) = super::watch_index::messages(record) {
        return Ok(messages);
    }
    if string(record, "agent") == "kimi" {
        return kimi_messages::read(record);
    }
    read_snapshot(record,None)
}

fn projection_now() -> Value {
    #[cfg(test)]
    if let Some(at) = PROJECTION_NOW.with(|at|at.borrow().clone()) { return at; }
    json!(chrono::Utc::now().to_rfc3339())
}

#[cfg(test)]
thread_local! { pub(super) static PROJECTION_NOW: std::cell::RefCell<Option<Value>> = const {std::cell::RefCell::new(None)}; }

/// Full scan of a read-only prefix for replay qualification. None preserves
/// the command-line reader's existing tail behaviour.
pub(super) fn read_snapshot(record: &Value, boundary: Option<u64>) -> Result<Vec<Value>> {
    let Some((agent, id, path)) = transcript(record) else {
        return Ok(vec![]);
    };
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    first_line(&mut file, agent, id)?;
    let length = file
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    let length = boundary.map_or(length,|boundary|boundary.min(length));
    let offset = length.saturating_sub(8 * 1024 * 1024);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(boundary.map_or(8 * 1024 * 1024,|_|length-offset))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut messages: Vec<Value> = Vec::new();
    let since = own_since(record);
    // Owner, 2026-11-09: Claude Code 2.1.3xx writes the replies between tool
    // calls only as content-addressed `api-request-blob` lines (no timestamp);
    // the `api-request` that sends them next carries the time.
    let mut pending: Vec<Value> = Vec::new();
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if (offset > 0 && index == 0) || line.len() > LINE_BYTES || !line.ends_with(b"\n") {
            continue;
        }
        if agent == "claude" && line.starts_with(b"{\"type\":\"api-request") {
            let Ok(event) = serde_json::from_slice::<Value>(line) else { continue };
            if !string(&event, "sessionId").is_empty() && event["sessionId"] != id {
                continue;
            }
            match string(&event, "type") {
                "api-request-blob" if string(&event["message"], "role") == "assistant" => pending.push(event),
                "api-request" => {
                    let at = event["timestamp"].clone();
                    for blob in pending.drain(..) {
                        push_blob(&mut messages, agent, id, blob, &at, since);
                    }
                }
                _ => {}
            }
            continue;
        }
        for message in line_messages(agent, id, line) {
            if own(&message, since) && !repeated(&messages, &message) {
                messages.push(message);
            }
        }
    }
    let now = projection_now();
    for blob in pending.drain(..) {
        push_blob(&mut messages, agent, id, blob, &now, since);
    }
    messages.sort_by(|a,b|a["at"].as_f64().unwrap_or(0.).total_cmp(&b["at"].as_f64().unwrap_or(0.)));
    // Body candidates have their own bounded window; they do not evict the
    // existing newest hundred assistant messages.
    let (mut bodies, mut replies) = (0, 0);
    messages.reverse();
    messages.retain(|message| {
        let count = if message["type"] == "UserMessageBody" {
            &mut bodies
        } else {
            &mut replies
        };
        *count += 1;
        *count <= 100
    });
    messages.reverse();
    Ok(messages)
}

/// What a [`Tail`] read found.
#[derive(Debug, PartialEq)]
pub(crate) enum TailRead {
    /// Public messages written since the previous read, oldest first.
    Messages(Vec<Value>),
    /// The transcript was replaced, rewritten, shortened or fell too far
    /// behind; a reader re-reads the messages whole.
    Ended,
}

/// Follows one Codex or Claude transcript from where it ended when opened and
/// yields the public messages written since, as `read` projects them. The
/// file is identified by device, inode and a hash of its first line; reads
/// continue from an offset and finish a line split across reads.
pub(crate) struct Tail {
    agent: String,
    conversation: String,
    path: PathBuf,
    identity: (u64, u64),
    head: [u8; 32],
    offset: u64,
    partial: Vec<u8>,
    // The bytes up to the next newline belong to a line read only in part.
    skipping: bool,
    recent: Vec<Value>,
    since: f64,
}

impl Tail {
    /// A transcript yields what each read finds after this many bytes at most.
    pub(crate) const READ_BYTES: usize = 2 * 1024 * 1024;
    /// Further behind than this, a tail ends rather than catch up.
    const BEHIND_BYTES: u64 = 16 * 1024 * 1024;

    /// A tail at the transcript's current end; None for other agents, a path
    /// that does not name the conversation, or a transcript that is not there.
    pub(crate) fn open(record: &Value) -> Option<Tail> {
        let (agent, id, path) = transcript(record)?;
        let identity = crate::platform::file_identity(&path)?;
        let mut file = File::open(&path).ok()?;
        let head: [u8; 32] = Sha256::digest(first_line(&mut file, agent, id).ok()?).into();
        let offset = file.metadata().ok()?.len();
        // Opened during a write, the rest of that line is skipped.
        let mut last = [0u8; 1];
        let skipping = offset > 0
            && file.seek(SeekFrom::Start(offset - 1)).is_ok()
            && file.read_exact(&mut last).is_ok()
            && last[0] != b'\n';
        Some(Tail {
            agent: agent.to_owned(),
            conversation: id.to_owned(),
            path,
            identity: (identity.device, identity.inode),
            head,
            offset,
            partial: Vec::new(),
            skipping,
            recent: Vec::new(),
            since: own_since(record),
        })
    }

    pub(crate) fn conversation(&self) -> &str {
        &self.conversation
    }

    pub(crate) fn position(&self) -> u64 { self.offset }

    /// Messages written since the previous read, from at most `READ_BYTES`.
    pub(crate) fn read(&mut self) -> TailRead {
        let Some(identity) = crate::platform::file_identity(&self.path) else {
            return TailRead::Ended;
        };
        let Ok(mut file) = File::open(&self.path) else {
            return TailRead::Ended;
        };
        let Ok(len) = file.metadata().map(|meta| meta.len()) else {
            return TailRead::Ended;
        };
        if (identity.device, identity.inode) != self.identity || len < self.offset || len - self.offset > Self::BEHIND_BYTES {
            return TailRead::Ended;
        }
        if len == self.offset {
            return TailRead::Messages(vec![]);
        }
        let same = first_line(&mut file, &self.agent, &self.conversation)
            .is_ok_and(|first| <[u8; 32]>::from(Sha256::digest(first)) == self.head);
        if !same || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return TailRead::Ended;
        }
        let mut bytes = Vec::new();
        let wanted = (len - self.offset).min(Self::READ_BYTES as u64);
        if (&mut file).take(wanted).read_to_end(&mut bytes).is_err() {
            return TailRead::Ended;
        }
        self.offset += bytes.len() as u64;
        let mut messages = Vec::new();
        let mut rest = bytes.as_slice();
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let (piece, after) = rest.split_at(end + 1);
            rest = after;
            if std::mem::take(&mut self.skipping) {
                self.partial.clear();
                continue;
            }
            let line = if self.partial.is_empty() { piece } else {
                self.partial.extend_from_slice(piece);
                self.partial.as_slice()
            };
            if line.len() <= LINE_BYTES {
                for message in line_messages(&self.agent, &self.conversation, line) {
                    if own(&message, self.since) && !repeated(&self.recent, &message) {
                        self.recent.push(message.clone());
                        messages.push(message);
                    }
                }
            }
            self.partial.clear();
        }
        if self.recent.len() > 4 {
            self.recent.drain(..self.recent.len() - 4);
        }
        if !self.skipping {
            self.partial.extend_from_slice(rest);
            // A line this long is not a public message: skip the rest of it.
            if self.partial.len() > LINE_BYTES {
                self.partial.clear();
                self.skipping = true;
            }
        }
        TailRead::Messages(messages)
    }
}

// Claude records some terminal-visible progress in thinking blocks. Keep it
// separate from replies; signatures and redacted blocks are never UI content.
fn claude_thinking(event: &Value) -> Option<Value> {
    if event["type"] != "assistant"
        || event["message"]["role"] != "assistant"
        || event["isMeta"] == true
        || event["isSidechain"] == true
    {
        return None;
    }
    let text = event["message"]["content"]
        .as_array()?
        .iter()
        .filter(|part| part["type"] == "thinking")
        .filter_map(|part| part["thinking"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(json!({"type":"AgentThinking","source":"claude_transcript",
        "at":search::timestamp(&event["timestamp"]),
        "message_id":format!("{}:thinking",string(event,"uuid")),"agent_id":"",
        "detail":journal::clipped(&json!(text),32_000)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    /// zerus-b4qk: a fork's transcript lines dated before the fork was
    /// created are copies of its source's history, which Activity shows from
    /// the source; the fork's own lines, and every line of a non-fork, stay.
    #[test]
    fn a_forks_copied_history_is_not_shown_again() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{id}.jsonl"));
        let line = |uuid: &str, at: &str, text: &str| {
            json!({"type":"assistant","uuid":uuid,"timestamp":at,"sessionId":id,
                "message":{"role":"assistant","content":[{"type":"text","text":text}]}})
            .to_string()
                + "\n"
        };
        fs::write(
            &path,
            line("old", "2026-10-04T12:00:00Z", "Copied from the source.")
                + &line("new", "2026-10-04T12:10:00Z", "The fork's own reply."),
        )
        .unwrap();
        let created = search::timestamp(&json!("2026-10-04T12:05:00Z"));
        let shown = |record: &Value| {
            read(record)
                .unwrap()
                .iter()
                .map(|m| m["detail"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let fork = json!({"agent":"claude","conversation_id":id,"transcript":path,"fork_parent_id":"source","created":created});
        assert_eq!(shown(&fork), ["The fork's own reply."]);
        let mut plain = fork.clone();
        plain.as_object_mut().unwrap().remove("fork_parent_id");
        assert_eq!(
            shown(&plain),
            ["Copied from the source.", "The fork's own reply."]
        );
        // A followed fork skips a copy written after it opened, too.
        use std::io::Write;
        let mut tail = Tail::open(&fork).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(
                (line("late-copy", "2026-10-04T11:00:00Z", "A late copy.")
                    + &line("later", "2026-10-04T12:20:00Z", "Later own."))
                    .as_bytes(),
            )
            .unwrap();
        let TailRead::Messages(messages) = tail.read() else {
            panic!("tail ended")
        };
        assert_eq!(
            messages
                .iter()
                .map(|m| m["detail"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Later own."]
        );
    }
    #[test]
    fn public_progress_and_claude_thinking_keep_separate_roles_without_private_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        let at = "2026-10-04T12:00:00Z";
        let rows = [
            json!({"type":"session_meta","payload":{"id":id}}),
            json!({"type":"response_item","timestamp":at,"payload":{"type":"message","role":"assistant","channel":"commentary","content":[{"type":"output_text","text":"Checking the deployment."}]}}),
            json!({"type":"event_msg","timestamp":at,"payload":{"type":"agent_message","message":"Checking the deployment."}}),
            json!({"type":"response_item","timestamp":at,"payload":{"type":"message","role":"assistant","channel":"analysis","content":[{"type":"output_text","text":"PRIVATE_REASONING"}]}}),
            json!({"type":"event_msg","timestamp":at,"payload":{"type":"agent_message","phase":"analysis","message":"PRIVATE_REASONING"}}),
        ];
        fs::write(
            &path,
            rows.iter()
                .map(|v| v.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();
        let record = json!({"agent":"codex","conversation_id":id,"transcript":path});
        let messages = read(&record).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["detail"], "Checking the deployment.");
        assert_eq!(messages[0]["message_phase"], "commentary");
        let native = [
            json!({"type":"session_meta","payload":{"id":id}}),
            json!({"type":"event_msg","timestamp":at,"payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"progress","phase":"commentary","content":[{"type":"text","text":"Progress from current Codex."}]}}}),
            json!({"type":"response_item","timestamp":at,"payload":{"type":"message","id":"progress","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Progress from current Codex."}]}}),
            json!({"type":"response_item","timestamp":at,"payload":{"type":"message","role":"assistant","phase":"analysis","content":[{"type":"output_text","text":"PRIVATE_REASONING"}]}}),
            json!({"type":"response_item","timestamp":"2026-10-04T12:00:10Z","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Progress from current Codex."}]}}),
        ];
        fs::write(&path,native.iter().map(|v|v.to_string()+"\n").collect::<String>()).unwrap();
        let messages=read(&record).unwrap();
        assert_eq!(messages.len(),2);
        assert_eq!(messages[0]["message_id"],"progress");
        assert_eq!(messages[0]["message_phase"],"commentary");
        assert_eq!(messages[1]["message_phase"],"commentary");
        assert_eq!(messages[0]["detail"],messages[1]["detail"]);
        fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":"wrong"}}).to_string() + "\n",
        )
        .unwrap();
        assert!(read(&record).is_err());
        let claude = json!({"type":"assistant","uuid":"msg-one","timestamp":at,"sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":"Still working."},{"type":"thinking","thinking":"Read all 24 pages.","signature":"PRIVATE_SIGNATURE"},{"type":"redacted_thinking","data":"REDACTED"}]}});
        let mut other = claude.clone();
        other["sessionId"] = json!("another-conversation");
        fs::write(&path, claude.to_string() + "\n" + &other.to_string() + "\n").unwrap();
        let messages =
            read(&json!({"agent":"claude","conversation_id":id,"transcript":path})).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["type"], "AgentThinking");
        assert_eq!(messages[0]["detail"], "Read all 24 pages.");
        assert_eq!(messages[1]["type"], "AgentMessage");
        assert_eq!(messages[1]["detail"], "Still working.");
        assert_ne!(messages[0]["message_id"], messages[1]["message_id"]);
        assert!(!serde_json::to_string(&messages)
            .unwrap()
            .contains("PRIVATE_SIGNATURE"));
        assert!(!serde_json::to_string(&messages)
            .unwrap()
            .contains("REDACTED"));
        for flag in ["isMeta", "isSidechain"] {
            let mut hidden = claude.clone();
            hidden[flag] = json!(true);
            assert!(claude_thinking(&hidden).is_none());
        }
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }

    fn codex(text: &str, phase: &str) -> String {
        json!({"type":"response_item","timestamp":"2026-10-04T12:00:00Z","payload":{"type":"message","role":"assistant","phase":phase,
            "content":[{"type":"output_text","text":text}]}}).to_string() + "\n"
    }

    fn details(read: TailRead) -> Vec<String> {
        match read {
            TailRead::Messages(messages) => messages.iter().map(|m| m["detail"].as_str().unwrap().to_owned()).collect(),
            TailRead::Ended => panic!("the tail ended"),
        }
    }

    #[test]
    fn a_tail_yields_public_messages_written_after_it_opened_across_split_lines() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        fs::write(&path, json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n" + &codex("Before the tail.", "commentary")).unwrap();
        let record = json!({"agent":"codex","conversation_id":id,"transcript":path});
        let mut tail = Tail::open(&record).unwrap();
        assert_eq!(tail.conversation(), id);
        assert!(details(tail.read()).is_empty());
        // The same message as a response item and an event: one message.
        let event = json!({"type":"event_msg","timestamp":"2026-10-04T12:00:01Z","payload":{"type":"agent_message","message":"Checking the deployment."}});
        append(&path, &(codex("Checking the deployment.", "commentary") + &event.to_string() + "\n" + &codex("PRIVATE_REASONING", "analysis")));
        let messages = match tail.read() { TailRead::Messages(messages) => messages, TailRead::Ended => panic!() };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["detail"], "Checking the deployment.");
        assert_eq!(messages[0]["message_phase"], "commentary");
        assert_eq!(messages, read(&record).unwrap()[1..].to_vec());
        // A line written in two parts is read once it ends.
        let line = codex("Split across writes.", "commentary");
        let (first, second) = line.split_at(20);
        append(&path, first);
        assert!(details(tail.read()).is_empty());
        append(&path, second);
        assert_eq!(details(tail.read()), ["Split across writes."]);
        // An oversized line is skipped whole, even read in parts.
        append(&path, &(codex(&"x".repeat(LINE_BYTES + 10), "commentary") + &codex("After the large line.", "commentary")));
        let mut found = Vec::new();
        for _ in 0..4 {
            found.extend(details(tail.read()));
        }
        assert_eq!(found, ["After the large line."]);
        assert!(!serde_json::to_string(&read(&record).unwrap()).unwrap().contains("PRIVATE_REASONING"));
    }

    #[test]
    fn a_tail_opened_during_a_write_skips_that_line_and_ends_when_the_file_changes() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("{id}.jsonl"));
        let message = |uuid: &str, text: &str, session: &str| json!({"type":"assistant","uuid":uuid,"timestamp":"2026-10-04T12:00:00Z","sessionId":session,
            "message":{"role":"assistant","content":[{"type":"thinking","thinking":format!("{text} (thinking)")},{"type":"text","text":text}]}}).to_string() + "\n";
        let partial = message("half", "Written in part.", &id);
        fs::write(&path, message("first", "First.", &id) + &partial[..30]).unwrap();
        let record = json!({"agent":"claude","conversation_id":id,"transcript":path});
        let mut tail = Tail::open(&record).unwrap();
        append(&path, &(partial[30..].to_owned() + &message("other", "Another conversation.", "elsewhere") + &message("next", "Next.", &id)));
        let messages = match tail.read() { TailRead::Messages(messages) => messages, TailRead::Ended => panic!() };
        assert_eq!(messages.iter().map(|m| (m["type"].as_str().unwrap(), m["detail"].as_str().unwrap())).collect::<Vec<_>>(),
                   [("AgentThinking", "Next. (thinking)"), ("AgentMessage", "Next.")]);
        assert_eq!(messages[1]["message_id"], "next");
        // A first line rewritten in place is another transcript.
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replacen("First.", "Fixed.", 1) + &message("later", "Later.", &id)).unwrap();
        assert_eq!(tail.read(), TailRead::Ended);
        // So is a shortened or replaced file.
        let mut tail = Tail::open(&record).unwrap();
        fs::write(&path, message("first", "First.", &id)).unwrap();
        assert_eq!(tail.read(), TailRead::Ended);
        let mut tail = Tail::open(&record).unwrap();
        let replacement = temp.path().join("replacement");
        fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
        append(&replacement, &message("swapped", "Swapped.", &id));
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(tail.read(), TailRead::Ended);
        // Only Codex and Claude transcripts of the exact conversation are followed.
        assert!(Tail::open(&json!({"agent":"kimi","conversation_id":id,"transcript":path})).is_none());
        assert!(Tail::open(&json!({"agent":"claude","conversation_id":uuid::Uuid::new_v4().to_string(),"transcript":path})).is_none());
        assert!(Tail::open(&json!({"agent":"claude","conversation_id":id,"transcript":temp.path().join(format!("{id}-missing.jsonl"))})).is_none());
    }
}

#[cfg(test)]
mod full_own_message {
    use super::*;
    #[test]
    fn handwritten_oversize_native_row_does_not_supply_the_missing_older_body() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        let prefix = "Common instructions. "
            .repeat(70)
            .chars()
            .take(1200)
            .collect::<String>();
        let a = prefix.clone() + " KEEP the file";
        let b = prefix + " DELETE the file";
        let old = json!({"type":"response_item","timestamp":100.0,"payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":a},{"type":"input_image","image_url":"x".repeat(LINE_BYTES)}]}});
        let new = json!({"type":"response_item","timestamp":105.0,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":b}]}});
        std::fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":id}}).to_string()
                + "\n"
                + &old.to_string()
                + "\n"
                + &new.to_string()
                + "\n",
        )
        .unwrap();
        let shown = read_activity(&json!({"agent":"codex","conversation_id":id,"transcript":path}))
            .unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0]["detail"], b);
    }

    #[test]
    fn native_codex_and_claude_user_text_is_available_for_activity_hydration() {
        let text = "before ".repeat(170)
            + "[Image #1] (Inspect the attached image at \"/example/image.png\") AFTER IMAGE";
        let codex = json!({"type":"response_item","timestamp":100.0,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}});
        let claude = json!({"type":"user","sessionId":"cid","timestamp":100.0,"uuid":"user-1","message":{"role":"user","content":[{"type":"text","text":text}]}});
        for (agent, row) in [("codex", codex), ("claude", claude)] {
            let messages = line_messages(agent, "cid", row.to_string().as_bytes());
            assert_eq!(messages.len(), 1, "{agent}");
            assert_eq!(messages[0]["detail"], text);
            assert_eq!(messages[0]["type"], "UserMessageBody");
        }
    }

    #[test]
    fn handwritten_native_user_rows_keep_scope_and_explicit_body_limits() {
        let text = "x".repeat(40000);
        let claude = json!({"type":"user","sessionId":"cid","timestamp":100.0,"uuid":"user-1","message":{"role":"user","content":[{"type":"text","text":text}]}});
        let shown = line_messages("claude", "cid", claude.to_string().as_bytes());
        assert_eq!(shown[0]["detail"].as_str().unwrap().chars().count(), 32001);
        assert_eq!(shown[0]["message_text_truncated"], true);
        assert!(line_messages("claude", "foreign", claude.to_string().as_bytes()).is_empty());
        for hidden in [
            json!({"isMeta":true}),
            json!({"isSidechain":true}),
            json!({"isCompactSummary":true}),
        ] {
            let mut row = claude.clone();
            row.as_object_mut()
                .unwrap()
                .extend(hidden.as_object().unwrap().clone());
            assert!(line_messages("claude", "cid", row.to_string().as_bytes()).is_empty());
        }
    }

    #[test]
    fn handwritten_native_body_rows_do_not_replace_subagent_rows_or_evict_replies() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        let mut lines = json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n";
        for index in 0..120 {
            for role in ["user", "assistant"] {
                lines.push_str(&(json!({"type":"response_item","timestamp":100.0+index as f64*10.0,"payload":{"type":"message","role":role,
                    "content":[{"type":if role=="user" {"input_text"} else {"output_text"},"text":format!("{role} {index}")}]}}).to_string()+"\n"));
            }
        }
        std::fs::write(&path, lines).unwrap();
        let record = json!({"agent":"codex","conversation_id":id,"transcript":path});
        let visible = read(&record).unwrap();
        assert_eq!(visible.len(), 100);
        assert!(visible
            .iter()
            .all(|message| message["type"] == "AgentMessage"));
        let activity = read_activity(&record).unwrap();
        assert_eq!(activity.len(), 200);
        assert_eq!(
            activity
                .iter()
                .filter(|message| message["type"] == "UserMessageBody")
                .count(),
            100
        );
    }
}
