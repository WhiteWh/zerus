//! Recover public assistant text, including commentary emitted between tools. The wire
//! file belongs to the exact recorded conversation; reading it never sends input.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use sha2::{Digest, Sha256};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

// Kimi 2.4.1 fires TurnStarted without awaiting it, but awaits the Stop hook
// before ending its step or starting another turn. Stop omits turn_id. At that
// blocking boundary the latest main turn.prompt identifies this same turn.
// Never infer it from hook arrival time, text, or the previous HGS start.
pub(super) fn bind_stop_turn(record: &Value, event: &mut Value) {
    if string(record, "agent") != "kimi"
        || string(event, "hook_event_name") != "Stop"
        || !["", "main"].contains(&string(event, "agent_id"))
    {
        return;
    }
    if event.get("turn_id").is_some() {
        event["turn_identity_source"] = json!("kimi_hook");
        return;
    }
    let Ok(paths) = questions::wire_paths(record) else { return };
    let [path] = paths.as_slice() else { return };
    if let Some(id) = stop_turn_file(path) {
        event["turn_id"] = json!(id);
        event["turn_identity_source"] = json!("kimi_wire");
    }
}

fn stop_turn_file(path: &Path) -> Option<u64> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO swapped in after discovery must not park the Stop hook while
        // it holds the global state lock, before we can check the file type.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    let (before, metadata) = crate::platform::FileIdentity::with_metadata(&file).ok()?;
    if !metadata.is_file() { return None; }
    let length = metadata.len();
    let offset = length.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    (&mut file).take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
    let (after, final_metadata) = crate::platform::FileIdentity::with_metadata(&file).ok()?;
    if bytes.len() as u64 != length - offset || before != after || final_metadata.len() != length
        || crate::platform::FileIdentity::at(path).ok()?.0 != before
    { return None; }
    stop_turn_tail(&bytes, offset > 0)
}

fn stop_turn_tail(bytes: &[u8], clipped_start: bool) -> Option<u64> {
    let mut latest = None;
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if clipped_start && index == 0 { continue; }
        if line.len() > 1024 * 1024 || !line.ends_with(b"\n") { return None; }
        let event: Value = serde_json::from_slice(line).ok()?;
        if !event.is_object() || !event["type"].is_string() { return None; }
        if string(&event, "type") != "turn.prompt" { continue; }
        match event["agentId"].as_str()? {
            "main" => {},
            "" => return None,
            _ => continue,
        }
        let id = event["turnId"].as_u64().filter(|id| *id <= i64::MAX as u64)?;
        if latest.is_some_and(|previous| id <= previous) { return None; }
        latest = Some(id);
    }
    latest
}

fn assistant_message(event: &Value) -> Option<Value> {
    if string(event, "type") != "agent.message.appended"
        || !["", "main"].contains(&string(event, "agentId"))
    {
        return None;
    }
    let envelope = &event["message"];
    let message = &envelope["message"];
    if string(message, "role") != "assistant"
        || !["completed", "stop", "end_turn", "tool_use", "tool_calls"]
            .contains(&string(&envelope["meta"]["finish"], "finishReason"))
    {
        return None;
    }
    let parts = message["content"].as_array()?;
    let text = parts
        .iter()
        .filter(|part| string(part, "type") == "text")
        .map(|part| string(part, "text"))
        .collect::<Vec<_>>()
        .join("\n");
    let at = event["time"].as_f64()? / 1000.0;
    if text.trim().is_empty() || at <= 0.0 {
        return None;
    }
    Some(json!({"type":"AgentMessage", "source":"kimi_wire", "at":at,
        "message_id":envelope["meta"]["messageId"], "agent_id":"",
        "message_phase":if ["completed", "stop", "end_turn"].contains(&string(&envelope["meta"]["finish"], "finishReason")) {"final"} else {"commentary"},
        "detail":journal::clipped(&json!(text), 32000)}))
}

// Kimi's live loop records text as it becomes visible in the TUI. The older
// agent.message.appended envelopes can arrive together only at the end of the
// turn. step.end links those copies by messageId, without guessing from text or
// the (late) envelope timestamp. Retain the original text position in the turn.
fn wire_messages(bytes: &[u8], clipped_start: bool) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    let mut envelopes = Vec::new();
    let mut steps = BTreeMap::<String, usize>::new();
    let mut finishes = BTreeMap::<String, (String, String)>::new();
    let mut parts = BTreeSet::new();
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if (clipped_start && index == 0) || line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            continue;
        }
        if ![
            b"agent.message.appended".as_slice(),
            b"context.append_loop_event".as_slice(),
        ]
        .iter()
        .any(|kind| line.windows(kind.len()).any(|part| part == *kind))
        {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if !["", "main"].contains(&string(&event, "agentId")) {
            continue;
        }
        if let Some(message) = assistant_message(&event) {
            envelopes.push(message);
            continue;
        }
        if string(&event, "type") != "context.append_loop_event" {
            continue;
        }
        let item = &event["event"];
        if string(item, "type") == "step.end" && !string(item, "uuid").is_empty() {
            finishes.insert(
                string(item, "uuid").into(),
                (
                    string(item, "messageId").into(),
                    string(item, "finishReason").into(),
                ),
            );
            continue;
        }
        if string(item, "type") != "content.part" || string(&item["part"], "type") != "text" {
            continue;
        }
        let step = string(item, "stepUuid");
        let part = string(item, "uuid");
        let text = string(&item["part"], "text");
        let at = event["time"].as_f64().unwrap_or(0.0) / 1000.0;
        if step.is_empty()
            || part.is_empty()
            || text.trim().is_empty()
            || at <= 0.0
            || !parts.insert(part.to_owned())
        {
            continue;
        }
        if let Some(index) = steps.get(step) {
            let combined = format!("{}\n{}", string(&messages[*index], "detail"), text);
            messages[*index]["detail"] = json!(journal::clipped(&json!(combined), 32000));
        } else {
            steps.insert(step.into(), messages.len());
            messages.push(json!({"type":"AgentMessage", "source":"kimi_wire", "at":at,
                "message_id":format!("kimi-step:{step}"), "agent_id":"", "message_phase":"commentary",
                "detail":journal::clipped(&json!(text),32000)}));
        }
    }
    let mut mirrored = BTreeSet::new();
    for (step, index) in steps {
        if let Some((id, finish)) = finishes.get(&step) {
            if !id.is_empty() {
                mirrored.insert(id.clone());
            }
            if ["completed", "stop", "end_turn"].contains(&finish.as_str()) {
                messages[index]["message_phase"] = json!("final");
            }
        }
    }
    for message in envelopes {
        let id = string(&message, "message_id");
        if !id.is_empty() && !mirrored.insert(id.to_owned()) {
            continue;
        }
        messages.push(message);
    }
    messages.sort_by(|a, b| {
        a["at"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["at"].as_f64().unwrap_or(0.0))
    });
    if messages.len() > 50 {
        messages.drain(..messages.len() - 50);
    }
    messages
}

// Native turn IDs also identify duplicate callbacks from legacy + current
// tracking registrations. Publish only bounded identity/hash metadata, not the
// injected attachment instructions. Legacy association needs a complete file.
fn wire_turns(bytes: &[u8], clipped_start: bool) -> (Vec<Value>, bool) {
    let mut complete = !clipped_start;
    let mut turns = Vec::new();
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if (clipped_start && index == 0) || line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            complete = false;
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else { complete = false; continue };
        if string(&event, "type") != "turn.prompt" || !["", "main"].contains(&string(&event, "agentId")) {
            continue;
        }
        let Some(id) = event["turnId"].as_u64().filter(|id| *id <= i64::MAX as u64) else { complete = false; continue };
        let prompt = string(&event, "promptId");
        let origin = string(&event["origin"], "kind");
        let Some(parts) = event["input"].as_array() else { complete = false; continue };
        let at = event["time"].as_f64().unwrap_or(0.0) / 1000.0;
        if prompt.is_empty() || prompt.len() > 160 || !prompt.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || !(1..=40).contains(&origin.len()) || !origin.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
            || !at.is_finite() || at <= 0.0 || parts.is_empty() || parts.len() > 64
            || !parts.iter().all(|part| string(part, "type") == "text" && part["text"].is_string()) {
            complete = false;
            continue;
        }
        // UserPromptSubmit hooks may prepend their own text parts. Hash each
        // native part independently rather than treating injected context as
        // text the person submitted.
        let hashes: Vec<_> = parts.iter().map(|part|
            format!("{:x}", Sha256::digest(string(part, "text").trim().as_bytes()))).collect();
        turns.push(json!({"turn_id":id, "prompt_id":prompt, "at":at,
            "origin":origin, "text_sha256":hashes}));
        if turns.len() > 50 { turns.remove(0); complete = false; }
    }
    (turns, complete)
}

pub(super) fn read(record: &Value) -> Result<Vec<Value>> {
    read_details(record, false).map(|value| value.0)
}

pub(super) fn read_with_turns(record: &Value) -> Result<(Vec<Value>, Vec<Value>, bool)> {
    read_details(record, true)
}

fn read_details(record: &Value, include_turns: bool) -> Result<(Vec<Value>, Vec<Value>, bool)> {
    let mut messages = Vec::new();
    let mut turns = Vec::new();
    let mut complete = false;
    for path in questions::wire_paths(record)? {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        let offset = length.saturating_sub(MAX_BYTES);
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        (&mut file).take(MAX_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        messages.extend(wire_messages(&bytes, offset > 0));
        if include_turns {
            (turns, complete) = wire_turns(&bytes, offset > 0);
            complete &= file.metadata().map_err(|e| e.to_string())?.len() == length;
        }
    }
    Ok((messages, turns, complete))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_turn(id: u64, prompt: &str) -> Value {
        json!({"type":"turn.prompt", "agentId":"main", "turnId":id,
            "promptId":format!("prompt-{id}"), "origin":{"kind":"user"}, "time":100000,
            "input":[{"type":"text", "text":prompt}]})
    }

    #[test]
    fn blocking_stop_binds_the_latest_main_turn_from_a_complete_stable_tail() {
        let child = json!({"type":"turn.prompt","agentId":"child","turnId":99});
        let bytes = lines(&[user_turn(0,"first"), user_turn(1,"follow-up"), child]);
        assert_eq!(stop_turn_tail(&bytes, false), Some(1));
        assert_eq!(stop_turn_tail(&lines(&[user_turn(0,"first")]), false), Some(0));
        assert_eq!(stop_turn_tail(&[b"partial older row\n".to_vec(),bytes.clone()].concat(), true), Some(1));
        // No lifetime turn-count cap: only this bounded tail's latest identity
        // is needed, not a claim that the complete conversation was loaded.
        let turns=(0..80).map(|id| user_turn(id,"fixture")).collect::<Vec<_>>();
        assert_eq!(stop_turn_tail(&lines(&turns),false),Some(79));
        let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("wire.jsonl");
        std::fs::write(&path,&bytes).unwrap();
        assert_eq!(stop_turn_file(&path),Some(1));
        assert_eq!(stop_turn_file(dir.path()),None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_replaced_fifo_does_not_park_the_blocking_stop_reader() {
        use std::os::unix::{ffi::OsStrExt,fs::OpenOptionsExt};
        use std::{sync::mpsc,thread,time::Duration};
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("wire.jsonl");
        std::fs::write(&path,lines(&[user_turn(1,"fixture")])).unwrap();
        assert_eq!(stop_turn_file(&path),Some(1));
        std::fs::remove_file(&path).unwrap();
        let name=std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: this NUL-terminated path remains live for the syscall and
        // names only this test's private temporary directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(),0o600) },0);
        let fifo=path.clone();let (send,receive)=mpsc::channel();
        let reader=thread::spawn(move || send.send(stop_turn_file(&fifo)).unwrap());
        let result=receive.recv_timeout(Duration::from_secs(5));
        // A broken blocking implementation must still leave no worker behind.
        // Linux permits O_RDWR on a FIFO without a peer; keep its writer alive
        // until the original reader has joined, even if that thread was late.
        let rescue=result.is_err().then(|| std::fs::OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NONBLOCK).open(&path).unwrap());
        reader.join().unwrap();drop(rescue);
        assert_eq!(result,Ok(None),"a nonregular replacement must be refused without waiting for a writer");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path,lines(&[user_turn(2,"fixture")])).unwrap();
        assert_eq!(stop_turn_file(&path),Some(2));
    }

    #[test]
    fn unknown_or_nonmonotonic_stop_identity_never_falls_back_to_a_previous_turn() {
        let valid=lines(&[user_turn(1,"fixture")]);
        for bytes in [valid[..valid.len()-1].to_vec(),[valid.clone(),b"invalid\n".to_vec()].concat(),
            lines(&[user_turn(1,"a"),user_turn(1,"b")]),
            lines(&[user_turn(1,"a"),user_turn(0,"b")]),
            [valid.clone(),vec![b'x';1024*1024+1],b"\n".to_vec()].concat()] {
            assert_eq!(stop_turn_tail(&bytes,false),None);
        }
        for id in [Value::Null,json!(-1),json!("1"),json!(1.5),json!(true),json!(u64::MAX)] {
            let mut unknown=user_turn(2,"unknown"); unknown["turnId"]=id;
            assert_eq!(stop_turn_tail(&[valid.clone(),lines(&[unknown])].concat(),false),None);
        }
        let mut missing=user_turn(2,"unknown");missing.as_object_mut().unwrap().remove("agentId");
        assert_eq!(stop_turn_tail(&[valid,lines(&[missing])].concat(),false),None);
    }

    #[test]
    fn attachment_turn_metadata_retains_zero_and_distinct_native_ids_without_prompt_text() {
        let prompt = "[File #1] (Read the attached file at \"/synthetic/request/document.pdf\") Check it";
        let (turns, complete) = wire_turns(&lines(&[user_turn(0, prompt), user_turn(1, prompt)]), false);
        assert!(complete);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["turn_id"], 0);
        assert_eq!(turns[1]["turn_id"], 1);
        assert_eq!(turns[0]["text_sha256"], turns[1]["text_sha256"]);
        assert!(!json!(turns).to_string().contains("/synthetic/"));
        let mut injected = user_turn(0, prompt);
        injected["input"].as_array_mut().unwrap().insert(0, json!({"type":"text", "text":"Synthetic memory hook context"}));
        let (turns, complete) = wire_turns(&lines(&[injected]), false);
        assert!(complete);
        assert_eq!(turns[0]["text_sha256"][1], format!("{:x}", Sha256::digest(prompt.as_bytes())));
    }

    #[test]
    fn incomplete_or_unbounded_native_turn_sources_never_admit_legacy_association() {
        let bytes = lines(&[user_turn(0, "Check")]);
        assert!(!wire_turns(&bytes, true).1);
        assert!(!wire_turns(&bytes[..bytes.len() - 1], false).1);
        assert!(!wire_turns(&[bytes.clone(), b"invalid\n".to_vec()].concat(), false).1);
        let many = (0..51).map(|id| user_turn(id, "Check")).collect::<Vec<_>>();
        let (turns, complete) = wire_turns(&lines(&many), false);
        assert_eq!(turns.len(), 50);
        assert!(!complete);
        let mut unsupported = user_turn(0, "Check");
        unsupported["input"] = json!([{"type":"image", "data":"synthetic"}]);
        assert!(!wire_turns(&lines(&[unsupported]), false).1);
        let mut child = user_turn(0, "Check"); child["agentId"] = json!("agent-1");
        assert!(wire_turns(&lines(&[child]), false).0.is_empty());
    }

    #[test]
    fn malformed_native_turn_origins_never_escape_into_metadata() {
        for origin in [json!({"text":"Synthetic private extra prompt"}), json!(["Synthetic private extra prompt"]),
            json!("Synthetic private extra prompt"), json!("x".repeat(41)), json!(true), json!(0), Value::Null] {
            let mut turn = user_turn(0, "Check"); turn["origin"]["kind"] = origin;
            let (turns, complete) = wire_turns(&lines(&[turn]), false);
            assert!(!complete);
            assert!(turns.is_empty());
        }
        let mut turn = user_turn(0, "Check"); turn["promptId"] = json!("Synthetic private extra prompt");
        assert_eq!(wire_turns(&lines(&[turn]), false), (vec![], false));
    }

    fn text_part(step: &str, part: &str, text: &str, time: u64) -> Value {
        json!({"type":"context.append_loop_event", "agentId":"main", "time":time,
            "event":{"type":"content.part", "uuid":part, "stepUuid":step,
                "part":{"type":"text", "text":text}}})
    }

    fn step_end(step: &str, id: &str, finish: &str) -> Value {
        json!({"type":"context.append_loop_event", "agentId":"main", "time":400000,
            "event":{"type":"step.end", "uuid":step, "messageId":id, "finishReason":finish}})
    }

    fn envelope(id: &str, text: &str, finish: &str) -> Value {
        json!({"type":"agent.message.appended", "time":400000,
            "message":{"message":{"role":"assistant", "content":[{"type":"text", "text":text}]},
                "meta":{"messageId":id, "finish":{"finishReason":finish}}}})
    }

    fn lines(events: &[Value]) -> Vec<u8> {
        events
            .iter()
            .map(|event| event.to_string() + "\n")
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn live_text_appears_before_turn_finishes_without_private_parts() {
        let public = text_part(
            "step-17",
            "part-17",
            "Всё собрано. Теперь делаю HTML-файл.",
            100000,
        );
        let mut think = text_part("step-17", "think", "private reasoning", 99000);
        think["event"]["part"]["type"] = json!("think");
        let mut child = text_part("child-step", "child", "child text", 100001);
        child["agentId"] = json!("worker");
        let tool = json!({"type":"context.append_loop_event", "time":100002,
            "event":{"type":"tool.result", "stepUuid":"step-17", "uuid":"result",
                "part":{"type":"text", "text":"tool output"}}});
        let bytes = lines(&[think, child, public.clone(), tool, public]);
        let messages = wire_messages(&bytes, false);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]["detail"],
            "Всё собрано. Теперь делаю HTML-файл."
        );
        assert_eq!(messages[0]["at"], 100.0);
        assert_eq!(messages[0]["message_phase"], "commentary");
        assert_eq!(messages[0]["message_id"], "kimi-step:step-17");
    }

    #[test]
    fn delayed_mirrors_keep_original_times_and_distinct_repeated_messages() {
        let bytes = lines(&[
            text_part("first", "p1", "Checking", 100000),
            text_part("first", "p2", "the file", 100100),
            step_end("first", "msg-1", "tool_use"),
            text_part("second", "p3", "Checking\nthe file", 200000),
            step_end("second", "msg-2", "tool_use"),
            text_part("final", "p4", "Done", 300000),
            step_end("final", "msg-3", "stop"),
            envelope("msg-1", "Checking\nthe file", "tool_calls"),
            envelope("msg-2", "Checking\nthe file", "tool_calls"),
            envelope("msg-3", "Done", "completed"),
        ]);
        let messages = wire_messages(&bytes, false);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["detail"], "Checking\nthe file");
        assert_eq!(messages[1]["detail"], messages[0]["detail"]);
        assert_ne!(messages[0]["message_id"], messages[1]["message_id"]);
        assert_eq!(messages[0]["at"], 100.0);
        assert_eq!(messages[1]["at"], 200.0);
        assert_eq!(messages[2]["at"], 300.0);
        assert_eq!(messages[2]["message_phase"], "final");
        assert_eq!(messages[2]["detail"], "Done");
    }

    #[test]
    fn legacy_envelopes_and_partial_tail_are_handled() {
        let legacy = envelope("old-progress", "Legacy progress", "tool_calls");
        let mut bytes = lines(&[
            legacy.clone(),
            legacy,
            envelope("old-final", "Legacy final", "completed"),
        ]);
        // A writer can be partway through the next record during a poll.
        bytes.extend_from_slice(b"{\"type\":\"context.append_loop_event\"");
        let messages = wire_messages(&bytes, false);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["detail"], "Legacy progress");
        assert_eq!(messages[1]["detail"], "Legacy final");
        let mut clipped = b"partial old record\n".to_vec();
        clipped.extend_from_slice(&bytes);
        assert_eq!(wire_messages(&clipped, true), messages);
    }

    #[test]
    fn public_text_in_final_and_tool_messages_only() {
        let mut event = json!({"type":"agent.message.appended","time":1700000000123u64,
            "message":{"message":{"role":"assistant","content":[
                {"type":"think","think":"private reasoning"},
                {"type":"text","text":"All pushed. **343 tests passed.**"}],"toolCalls":[]},
                "meta":{"finish":{"finishReason":"completed"},"messageId":"msg-final"}}});
        let result = assistant_message(&event).unwrap();
        assert_eq!(result["detail"], "All pushed. **343 tests passed.**");
        assert_eq!(result["at"], 1700000000.123);
        event["message"]["message"]["toolCalls"] = json!([{"name":"Bash"}]);
        event["message"]["meta"]["finish"]["finishReason"] = json!("tool_calls");
        let progress = assistant_message(&event).unwrap();
        assert_eq!(progress["detail"], "All pushed. **343 tests passed.**");
        assert_eq!(progress["message_phase"], "commentary");
        event["message"]["message"]["toolCalls"] = json!([]);
        event["agentId"] = json!("child");
        assert!(assistant_message(&event).is_none());
        event["agentId"] = json!("main");
        event["message"]["meta"]["finish"]["finishReason"] = json!("length");
        assert!(assistant_message(&event).is_none());
    }
}
