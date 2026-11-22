//! Repair a missed acceptance hook from the exact native main-user input.
//! No receipt is replayed, and completion/absence alone never proves delivery.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

mod bootstrap;
mod kimi_order;
mod uncertainty;
pub(in crate::state) use kimi_order::{
    bind as bind_native_context, first_context as first_native_context,
    record_hook as record_native_context,
};
pub(in crate::state) use uncertainty::remember_unknown;

const BYTES: u64 = 16 << 20;
const HEADER: u64 = 64 << 10;
const ROWS: usize = 16_384;

struct Source {
    rows: Vec<Value>,
    header: Option<Value>,
    path: PathBuf,
    held: fs::File,
    stamp: (u64, u64, u64, std::time::SystemTime),
}

impl Source {
    fn current(&self) -> bool {
        stamp(&self.held) == Some(self.stamp)
            && crate::platform::no_follow(fs::OpenOptions::new().read(true))
                .open(&self.path)
                .ok()
                .and_then(|file| stamp(&file))
                == Some(self.stamp)
    }
}

fn stamp(file: &fs::File) -> Option<(u64, u64, u64, std::time::SystemTime)> {
    let (id, meta) = crate::platform::FileIdentity::with_metadata(file).ok()?;
    (meta.is_file() && id.inode != 0).then_some((
        id.device,
        id.inode,
        meta.len(),
        meta.modified().ok()?,
    ))
}

// A held regular file, bounded header + tail. A clipped row, replacement,
// truncation, concurrent append, gap or malformed row yields unknown.
fn source(path: &Path, conversation: Option<&str>) -> Option<Source> {
    if !path.is_absolute() {
        return None;
    }
    let open = || {
        crate::platform::no_follow(fs::OpenOptions::new().read(true))
            .open(path)
            .ok()
    };
    let mut file = open()?;
    let before = stamp(&file)?;
    let mut native_header = None;
    if let Some(id) = conversation {
        let mut header = Vec::new();
        (&mut file).take(HEADER).read_to_end(&mut header).ok()?;
        let end = header.iter().position(|byte| *byte == b'\n')?;
        let row: Value = serde_json::from_slice(&header[..end]).ok()?;
        if row["type"] != "session_meta" || row["payload"]["id"] != id {
            return None;
        }
        native_header = Some(row);
    }
    let start = before.2.saturating_sub(BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    (&mut file).take(BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > BYTES || stamp(&file)? != before || stamp(&open()?)? != before {
        return None;
    }
    let mut rows = Vec::new();
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if start > 0 && index == 0 {
            continue;
        }
        // Codex tool-result rows can exceed 1MiB. They still share the SAME
        // 16MiB aggregate tail and row count; parse them rather than treating
        // unrelated tool output as lost exact-user evidence. Kimi is unchanged.
        let row_bytes = if conversation.is_some() {
            BYTES as usize
        } else {
            1 << 20
        };
        if !line.ends_with(b"\n") || line.len() > row_bytes || rows.len() == ROWS {
            return None;
        }
        let row: Value = serde_json::from_slice(line).ok()?;
        if matches!(row["type"].as_str(), Some("gap" | "wire.gap")) {
            return None;
        }
        rows.push(row);
    }
    Some(Source {
        rows,
        header: native_header,
        path: path.into(),
        held: file,
        stamp: before,
    })
}

fn kimi(record: &Value) -> Option<Source> {
    // Presentation discovery canonicalizes paths and truncates its roster.
    // Acceptance needs the complete bounded roster and the original final
    // component, so a link or an unseen second match remains unknown.
    let conversation = string(record, "conversation_id");
    if !questions::token(conversation) {
        return None;
    }
    let root = crate::platform::canonical(questions::kimi_home(record).join("sessions")).ok()?;
    let mut selected = None;
    for (index, entry) in fs::read_dir(&root).ok()?.enumerate() {
        if index == 2000 {
            return None;
        }
        let entry = entry.ok()?;
        let path = entry
            .path()
            .join(conversation)
            .join("agents/main/wire.jsonl");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if !metadata.is_file()
            || !crate::platform::canonical(&path).ok()?.starts_with(&root)
            || selected.replace(path).is_some()
        {
            return None;
        }
    }
    let path = selected?;
    let state = path.parent()?.parent()?.parent()?.join("state.json");
    let bytes = labels::read_file(&state, 64 << 10).ok()??;
    let state: Value = serde_json::from_slice(&bytes).ok()?;
    if state["id"] != record["conversation_id"] || state["version"] != 2 {
        return None;
    }
    source(&path, None)
}

fn text<'a>(parts: &'a Value, kind: &str) -> Option<&'a str> {
    let parts = parts.as_array()?;
    let [part] = parts.as_slice() else {
        return None;
    };
    (part["type"] == kind)
        .then(|| part["text"].as_str())
        .flatten()
}

fn hook_text<'a>(agent: &str, event: &'a Value) -> Option<&'a str> {
    event["prompt"].as_str().or_else(|| {
        (agent == "kimi")
            .then(|| text(&event["prompt"], "text"))
            .flatten()
    })
}

fn timestamp(row: &Value) -> Option<f64> {
    let at = chrono::DateTime::parse_from_rfc3339(row["timestamp"].as_str()?)
        .ok()?
        .timestamp_micros() as f64
        / 1_000_000.0;
    (at.is_finite() && at > 0.0 && at <= now()).then_some(at)
}

fn consumed(
    rows: &[Value],
    agent: &str,
    pending: f64,
    submitted: &str,
    after: Option<u64>,
) -> Option<(f64, Option<u64>)> {
    let mut turn = None;
    for row in rows {
        let (at, input, native_id) = if agent == "codex" {
            let payload = &row["payload"];
            if row["type"] == "event_msg" {
                match string(payload, "type") {
                    "task_started" | "turn_started" => {
                        // UserPromptSubmit can finish before its user row is
                        // written. A later row of that old turn cannot credit
                        // a newer send. No in-turn steer inference.
                        turn = timestamp(row).zip(native_turn(agent, payload));
                    }
                    "task_complete" | "turn_complete" | "turn_aborted" => turn = None,
                    _ => {}
                }
                continue;
            }
            if row["type"] != "response_item"
                || payload["type"] != "message"
                || payload["role"] != "user"
            {
                continue;
            }
            let Some(at) = timestamp(row) else {
                continue;
            };
            let Some((started, _)) = &turn else {
                continue;
            };
            if *started <= pending || *started > at {
                continue;
            }
            let Some(input) = text(&payload["content"], "input_text") else {
                continue;
            };
            (at, input, None)
        } else {
            if row["type"] != "turn.prompt"
                || row["agentId"] != "main"
                || row["origin"]["kind"] != "user"
                || native_turn(agent, row).is_none()
            {
                continue;
            }
            let (Some(at), Some(input)) = (row["time"].as_f64(), text(&row["input"], "text"))
            else {
                continue;
            };
            let id = row["turnId"].as_u64()?;
            if after.is_some_and(|floor| id <= floor) {
                continue;
            }
            (at / 1000.0, input, Some(id))
        };
        if at.is_finite() && at > pending && at <= now() && input == submitted {
            return Some((at, native_id));
        }
    }
    None
}

fn stored_receipt(record: &Value) -> Option<Value> {
    let id = string(record, "input_pending_request");
    if !zerus_client_core::input_acknowledgment::canonical(id) {
        return None;
    }
    let bytes = labels::read_file(
        &root().join("input_receipts").join(format!("{id}.json")),
        256 << 10,
    )
    .ok()??;
    let receipt: Value = serde_json::from_slice(&bytes).ok()?;
    let digest = string(&receipt, "digest");
    if receipt["request_id"] != id
        || receipt["name"] != record["name"]
        || receipt["run_id"] != record["run_id"]
        || receipt["submitted_at"] != record["input_pending_at"]
        || !(receipt["transport"] == "terminal"
            || (record["agent"] == "codex"
                && receipt["transport"] == "codex_queue"
                && uuid::Uuid::parse_str(string(&receipt, "native_queued_message_id")).is_ok_and(
                    |id| {
                        !id.is_nil()
                            && id.to_string() == string(&receipt, "native_queued_message_id")
                    },
                )))
        || string(&receipt, "submitted_text").is_empty()
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || record
            .get("input_pending_digest")
            .is_some_and(|value| value != digest)
        || record
            .get("input_pending_text_sha256")
            .is_some_and(|value| {
                value
                    != &json!(format!(
                        "{:x}",
                        Sha256::digest(string(&receipt, "submitted_text").as_bytes())
                    ))
            })
    {
        return None;
    }
    Some(receipt)
}

fn bound_receipt(record: &Value, bootstrap: bool) -> Option<Value> {
    let receipt = stored_receipt(record)?;
    (receipt["conversation_id"] == record["conversation_id"]
        || (receipt["transport"] == "terminal"
            && codex_tasks::recovered_pending(record)
            && receipt["conversation_id"] == record["codex_binding_recovery"]["from"])
        || (bootstrap
            && receipt["first_message"] == true
            && receipt["conversation_id"].as_str() == Some("")))
    .then_some(receipt)
}

fn pending_receipt(record: &Value) -> Option<Value> {
    bound_receipt(record, false)
}

fn receipt(record: &Value) -> Option<Value> {
    // A historical Codex context is only a candidate here. It must pass the
    // launch-start or original-main/closed-child proof below, plus the SAME
    // held native source that proves its exact input. Other callers stay bound.
    let receipt = if record["agent"] == "codex" {
        stored_receipt(record)?
    } else {
        pending_receipt(record)?
    };
    (matches!(receipt["status"].as_str(), Some("submitted" | "uncertain"))
        && receipt["attachments"].as_array().is_some_and(Vec::is_empty))
    .then_some(receipt)
}

pub(in crate::state) fn allows_pending(record: &Value) -> bool {
    if !input_unconfirmed(record) {
        return true;
    }
    let state = uncertainty::load(record);
    !state.uncertain()
        || pending_receipt(record)
            .is_some_and(|receipt| !state.blocks(string(&receipt, "submitted_text")))
}

/// A delayed old callback is not evidence for a newer pending request. Once an
/// unknown input exists, only an unambiguous complete prompt can open its gate.
pub(in crate::state) fn allows_hook(record: &Value, event: &Value) -> bool {
    if input_unconfirmed(record)
        && (codex_tasks::recovered_pending(record)
            || (record["agent"] == "codex"
                && stored_receipt(record).is_some_and(|receipt| {
                    receipt["conversation_id"] != record["conversation_id"]
                        && !bootstrap::hook(record, &receipt, event)
                })))
    {
        return false;
    }
    if !matches!(
        string(event, "hook_event_name"),
        "UserPromptSubmit" | "UserPromptQueued" | "TurnStarted" | "TaskStarted"
    ) {
        return true;
    }
    if string(record, "agent") == "kimi"
        && matches!(
            string(event, "hook_event_name"),
            "TurnStarted" | "TaskStarted"
        )
    {
        let Some(after) = kimi_order::pending(record) else {
            return false;
        };
        let Some(id) = event["turn_id"]
            .as_u64()
            .filter(|id| *id <= i64::MAX as u64)
        else {
            return false;
        };
        if after.is_some_and(|floor| id <= floor) {
            return false;
        }
        let Some(receipt) = pending_receipt(record) else {
            return false;
        };
        if event["origin_kind"] != "user"
            || hook_text(string(record, "agent"), event) != receipt["submitted_text"].as_str()
        {
            return false;
        }
    }
    if !input_unconfirmed(record) {
        return true;
    }
    let state = uncertainty::load(record);
    if !state.uncertain() {
        return true;
    }
    let Some(receipt) = pending_receipt(record) else {
        return false;
    };
    !state.blocks(string(&receipt, "submitted_text"))
        && matches!(
            string(event, "hook_event_name"),
            "UserPromptSubmit" | "UserPromptQueued" | "TurnStarted"
        )
        && hook_text(string(record, "agent"), event) == receipt["submitted_text"].as_str()
}

/// The caller holds Root and the selected record. Only acceptance changes;
/// original delivery/unknown receipts, clearing and person audits stay intact.
pub(in crate::state) fn reconcile_locked(record: &mut Value) -> Result<()> {
    let agent = string(record, "agent");
    if !matches!(agent, "codex" | "kimi")
        || !input_unconfirmed(record)
        || terminating::pending(record)
        || string(record, "run_id").is_empty()
        || string(record, "conversation_id").is_empty()
        || !string(record, "expected_id").is_empty()
        || record["conversation_state"] == "unknown"
        || input_typing(record).is_some()
        || send_in_flight(record)
    {
        return Ok(());
    }
    let Some(receipt) = receipt(record) else {
        return Ok(());
    };
    // A terminal receipt could name the old mistaken binding while input
    // reached the original TUI. A native queue UUID instead names its actual
    // recipient; neither a binding repair nor parent text can rehome it.
    if receipt["conversation_id"] != record["conversation_id"] && receipt["transport"] != "terminal"
    {
        return Ok(());
    }
    if uncertainty::load(record).blocks(string(&receipt, "submitted_text")) {
        return Ok(());
    }
    let after = if agent == "kimi" {
        let Some(after) = kimi_order::pending(record) else {
            return Ok(());
        };
        after
    } else {
        None
    };
    let source = if agent == "codex" {
        source(
            Path::new(string(record, "transcript")),
            Some(string(record, "conversation_id")),
        )
    } else {
        kimi(record)
    };
    let Some(source) = source else {
        return Ok(());
    };
    if agent == "codex"
        && receipt["conversation_id"] != record["conversation_id"]
        && !(codex_tasks::recovered_pending(record)
            && receipt["conversation_id"] == record["codex_binding_recovery"]["from"])
        && !bootstrap::receipt(record, &receipt)
        && !source.header.as_ref().is_some_and(|header| {
            codex_tasks::original_main_receipt(
                record,
                &source.path,
                &source.held,
                header,
                &source.rows,
                string(&receipt, "conversation_id"),
            )
        })
    {
        return Ok(());
    }
    let Some((at, native_id)) = consumed(
        &source.rows,
        agent,
        record["input_pending_at"].as_f64().unwrap_or(f64::INFINITY),
        string(&receipt, "submitted_text"),
        after,
    ) else {
        return Ok(());
    };
    if !source.current() {
        return Ok(());
    }
    record["input_seen_at"] = json!(at);
    record["input_acceptance"] = json!({"source":"native_user_input", "request_id":receipt["request_id"],
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],"pending_at":record["input_pending_at"],
        "native_turn_id":native_id,"digest":receipt["digest"],"text_sha256":format!("{:x}",Sha256::digest(string(&receipt,"submitted_text").as_bytes())),"at":at});
    if receipt["conversation_id"] != record["conversation_id"] {
        record["input_acceptance"]["receipt_conversation_id"] = receipt["conversation_id"].clone();
    }
    write(record)
}

/// Positive native completion is only eligibility for the person's unknown
/// decision. An old completed turn never settles a newer pending input itself.
pub(in crate::state) fn native_idle(record: &Value) -> bool {
    let (source, agent) = match string(record, "agent") {
        "kimi" => (kimi(record), "kimi"),
        "codex" => (
            source(
                Path::new(string(record, "transcript")),
                Some(string(record, "conversation_id")),
            ),
            "codex",
        ),
        _ => return false,
    };
    let Some(source) = source else {
        return false;
    };
    let mut turn = None;
    let mut started_at = 0.0;
    let mut ended = false;
    for row in &source.rows {
        let at = if agent == "kimi" {
            row["time"].as_f64().map(|at| at / 1000.0)
        } else {
            row["timestamp"]
                .as_str()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.timestamp_micros() as f64 / 1_000_000.0)
        };
        let payload = if agent == "kimi" {
            if row["agentId"] != "main" {
                if matches!(string(row, "type"), "turn.prompt" | "turn.ended")
                    && string(row, "agentId").is_empty()
                {
                    return false;
                }
                continue;
            }
            row.clone()
        } else {
            if row["type"] != "event_msg" {
                continue;
            }
            row["payload"].clone()
        };
        match string(&payload, "type") {
            "turn.prompt" | "task_started" | "turn_started" => {
                let Some(at) = at.filter(|at| at.is_finite() && *at > 0.0 && *at <= now()) else {
                    return false;
                };
                started_at = at;
                let Some(id) = native_turn(agent, &payload) else {
                    return false;
                };
                turn = Some(id);
                ended = false;
            }
            "turn.ended" | "task_complete" | "turn_complete" => {
                let Some(id) = native_turn(agent, &payload) else {
                    return false;
                };
                ended = at.is_some_and(|at| at.is_finite() && at >= started_at && at <= now())
                    && turn.as_ref() == Some(&id)
                    && if agent == "kimi" {
                        payload["reason"] == "completed"
                    } else {
                        payload["error"].is_null()
                    };
            }
            _ => {}
        }
    }
    ended && source.current()
}

fn native_turn(agent: &str, payload: &Value) -> Option<Value> {
    if agent == "kimi" {
        return payload["turnId"]
            .as_u64()
            .filter(|id| *id <= i64::MAX as u64)
            .map(Value::from);
    }
    payload["turn_id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 200
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-:".contains(&byte))
        })
        .map(|id| json!(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn consumed(rows: &[Value], agent: &str, pending: f64, text: &str) -> Option<f64> {
        super::consumed(rows, agent, pending, text, None).map(|(at, _)| at)
    }
    #[test]
    fn native_hook_text_accepts_only_the_complete_native_input() {
        let kimi = json!({"prompt":[{"type":"text","text":"literal"}]});
        assert_eq!(hook_text("kimi", &kimi), Some("literal"));
        assert_eq!(hook_text("codex", &kimi), None);
        for parts in [
            json!([]),
            json!([{"type":"text","text":"literal"},{"type":"text","text":"extra"}]),
            json!([{"type":"image","text":"literal"}]),
        ] {
            assert_eq!(hook_text("kimi", &json!({"prompt":parts})), None);
        }
    }
    #[test]
    fn native_turn_identity_types_are_closed() {
        for id in [
            Value::Null,
            json!(false),
            json!(true),
            json!({}),
            json!([]),
            json!("0"),
            json!(-1),
            json!(u64::MAX),
        ] {
            assert!(native_turn("kimi", &json!({"turnId":id})).is_none());
        }
        for id in [0, 1, i64::MAX] {
            assert_eq!(native_turn("kimi", &json!({"turnId":id})), Some(json!(id)));
        }
        for id in [
            Value::Null,
            json!(false),
            json!(0),
            json!([]),
            json!(""),
            json!("bad id"),
        ] {
            assert!(native_turn("codex", &json!({"turn_id":id})).is_none());
        }
    }

    #[test]
    fn exact_user_input_only_never_timing_completion_substring_or_peer() {
        let pending = 10.0;
        let good = json!({"timestamp":"1970-01-01T00:00:11Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"exact\ninput"}]}});
        let start = json!({"type":"event_msg","timestamp":"1970-01-01T00:00:10.500Z","payload":{"type":"task_started","turn_id":"native-turn"}});
        assert_eq!(
            consumed(
                &[start.clone(), good.clone()],
                "codex",
                pending,
                "exact\ninput"
            ),
            Some(11.0)
        );
        assert!(consumed(
            &[start.clone(), good.clone()],
            "codex",
            11.0,
            "exact\ninput"
        )
        .is_none());
        for key in ["assistant", "tool", "system"] {
            let mut row = good.clone();
            row["payload"]["role"] = json!(key);
            assert!(consumed(&[start.clone(), row], "codex", pending, "exact\ninput").is_none());
        }
        for text in ["exact", " exact\ninput", "exact\ninput\n"] {
            assert!(consumed(&[start.clone(), good.clone()], "codex", pending, text).is_none());
        }
        let kimi = json!({"type":"turn.prompt","agentId":"main","origin":{"kind":"user"},"turnId":1,"time":11000,"input":[{"type":"text","text":"exact"}]});
        assert_eq!(
            consumed(std::slice::from_ref(&kimi), "kimi", pending, "exact"),
            Some(11.0)
        );
        for (key, value) in [
            ("agentId", json!("child")),
            ("origin", json!({"kind":"task"})),
            (
                "input",
                json!([{"type":"text","text":"prefix"},{"type":"text","text":"exact"}]),
            ),
        ] {
            let mut row = kimi.clone();
            row[key] = value;
            assert!(consumed(&[row], "kimi", pending, "exact").is_none());
        }
    }
}
