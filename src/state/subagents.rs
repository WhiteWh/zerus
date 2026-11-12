//! Read-only child Activity. The parent roster is the authority for membership;
//! an arbitrary thread ID or filesystem path is never an inspection target.
use super::*;
use rusqlite::{params, OptionalExtension};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};

pub(super) fn inspection(name: &str, id: &str, archive_id: Option<&str>) -> Result<Value> {
    if id.is_empty()
        || id.len() > 160
        || id.contains(['/', '\\'])
        || id.chars().any(char::is_control)
    {
        return Err("Invalid subagent identity".into());
    }
    let record = {
        let _guard = lock(None)?;
        if let Some(archive) = archive_id {
            archive::read_archive(name, archive)?
        } else {
            read(name)?
        }
    };
    let child = record["subagents"]
        .get(id)
        .filter(|v| v.is_object())
        .ok_or("This subagent is not in the selected conversation")?;
    let external = child["external"] == true;
    let mut native_record = record.clone();
    if external {
        native_record["agent"] = child["provider"].clone();
        native_record["conversation_id"] = child["conversation_id"].clone();
        for key in ["transcript", "cwd", "agent_home"] {
            native_record[key] = child[key].clone();
        }
    }
    let mut output = json!({"name":name,"agent_id":id,"parent_conversation_id":record["conversation_id"],
        "conversation_id":format!("{}/{}",string(&record,"conversation_id"),id),"run_id":record["run_id"],
        "provider":native_record["agent"],"model":child["model"],"label":child["name"],"tracked":true,"read_only":true,"cwd":native_record["cwd"],
        "history_scope":"Recorded subagent activity","state":child["state"],"cursor":0,
        "completion_source":child["completion_source"],"tracking_evidence":child["tracking_evidence"]});
    if let Some(archive) = archive_id {
        output["archive_id"] = json!(archive);
        output["state"] = json!("archived");
    }
    let db = journal::event_db()?;
    let mut query = db.prepare("SELECT seq,payload FROM events WHERE name=?1 AND conversation IS ?2 AND json_extract(payload,'$.agent_id')=?3 ORDER BY seq DESC LIMIT 200").map_err(|e|e.to_string())?;
    let rows = query
        .query_map(
            params![
                journal::journal_name(&record),
                record["conversation_id"].as_str(),
                id
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, payload) = row.map_err(|e| e.to_string())?;
        let mut event: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        event["seq"] = json!(seq);
        event["agent_id"] = json!("");
        if event["type"] == "SubagentStop" {
            event["type"] = json!("Stop");
        }
        events.push(event);
    }
    events.reverse();
    let messages = if external {
        provider_messages::read(&native_record)
            .map(|messages| Some((messages, string(&native_record, "cwd").to_owned(), false)))
    } else {
        native_messages(&record, id)
    };
    let mut source_truncated = false;
    match messages {
        Ok(Some((messages, cwd, truncated))) if !messages.is_empty() => {
            // Native messages replace hook excerpts; tool events retain their
            // provenance and are merged chronologically by ActivityView.
            events.retain(|e| {
                !["Stop", "AgentMessage"].contains(&string(e, "type"))
                    && (external
                        || !["UserPromptSubmit", "TurnStarted"].contains(&string(e, "type")))
            });
            events.extend(messages);
            if !cwd.is_empty() {
                output["cwd"] = json!(cwd);
            }
            source_truncated = truncated;
            output["history_scope"] = json!("Native subagent history");
        }
        Err(_) => {
            output["history_scope"] =
                json!("Recorded events; native subagent history is unavailable");
        }
        _ => {}
    }
    if merge_activity_events(&mut events, source_truncated) {
        output["history_truncated"] = json!(true);
    }
    if events.is_empty() && !string(child, "detail").is_empty() {
        events.push(json!({"type":"AgentMessage","agent_id":"","at":child["updated"],"detail":child["detail"],"source":"subagent_summary"}));
        output["history_scope"] = json!("Only the last reported subagent result is available");
    }
    output["events"] = json!(events);
    if external {
        output["session_usage"] = usage::read(&native_record);
    }
    Ok(output)
}

fn merge_activity_events(events: &mut Vec<Value>, mut truncated: bool) -> bool {
    fn at(value: &Value) -> f64 {
        value["at"].as_f64().unwrap_or_default()
    }
    events.sort_by(|a, b| at(a).total_cmp(&at(b)));
    if events.len() > 200 {
        events.drain(..events.len() - 200);
        truncated = true;
    }
    truncated
}

fn native_messages(record: &Value, id: &str) -> Result<Option<(Vec<Value>, String, bool)>> {
    let agent = string(record, "agent");
    let native_home = if string(record, "agent_home").is_empty() {
        home().join(match agent {
            "codex" => ".codex",
            "claude" => ".claude",
            "kimi" => ".kimi-code",
            _ => return Ok(None),
        })
    } else {
        PathBuf::from(string(record, "agent_home"))
    };
    let mut cwd = String::new();
    let path = match agent {
        "codex" => {
            if uuid::Uuid::parse_str(id).is_err() {
                return Ok(None);
            }
            let db = codex_sqlite::NativeConnection::open(native_home.join("state_5.sqlite"))
                .map_err(|e| e.to_string())?;
            db.busy_timeout(std::time::Duration::from_millis(20))
                .map_err(|e| e.to_string())?;
            let row = db
                .query_row(
                    "SELECT rollout_path,cwd FROM threads WHERE id=?1",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((path, dir)) = row else {
                return Ok(None);
            };
            cwd = dir;
            PathBuf::from(path)
        }
        "claude" => {
            let Some(path) = claude_subagents::paths(record, &[id]).remove(id) else {
                return Ok(None);
            };
            path
        }
        "kimi" => {
            let Some(main) = questions::wire_paths(record)?.into_iter().next() else {
                return Ok(None);
            };
            main.parent()
                .and_then(Path::parent)
                .ok_or("Invalid wire location")?
                .join(id)
                .join("wire.jsonl")
        }
        _ => return Ok(None),
    };
    let path = crate::platform::canonical(&path).map_err(|e| e.to_string())?;
    if !path.starts_with(crate::platform::canonical(&native_home).map_err(|e| e.to_string())?) {
        return Err("Native history is outside the selected account".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    if agent == "codex" {
        let mut header = String::new();
        BufReader::new((&mut file).take(256 * 1024))
            .read_line(&mut header)
            .map_err(|e| e.to_string())?;
        let header: Value = serde_json::from_str(&header).map_err(|e| e.to_string())?;
        if header["type"] != "session_meta" || header["payload"]["id"] != id {
            return Err("Subagent transcript identity mismatch".into());
        }
    }
    let offset = file
        .metadata()
        .map_err(|e| e.to_string())?
        .len()
        .saturating_sub(8 * 1024 * 1024);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut messages = Vec::new();
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if (offset > 0 && index == 0) || line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            continue;
        }
        let Ok(mut event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if agent == "kimi" {
            if !["", id].contains(&string(&event, "agentId")) {
                continue;
            }
            event["agentId"] = json!("main");
        } else if agent == "claude" {
            if event["agentId"] != id || event["sessionId"] != record["conversation_id"] {
                continue;
            }
            // Sidechain messages are the selected child's own conversation.
            event["isSidechain"] = json!(false);
        }
        if let Some((message, _)) = search::provider_event(agent, &event) {
            if messages.last().is_none_or(|last: &Value| {
                last["detail"] != message["detail"] || last["type"] != message["type"]
            }) {
                messages.push(message);
            }
        }
    }
    let truncated = offset > 0 || messages.len() > 200;
    if messages.len() > 200 {
        messages.drain(..messages.len() - 200);
    }
    Ok(Some((messages, cwd, truncated)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(at: i64, kind: &str) -> Value {
        json!({"at":at,"type":kind,"detail":format!("{kind} {at}")})
    }

    #[test]
    fn merged_page_keeps_the_latest_two_hundred_in_stable_order() {
        let mut events = (1..=150)
            .map(|at| event(at, "hook"))
            .chain((101..=250).map(|at| event(at, "native")))
            .collect::<Vec<_>>();
        assert!(merge_activity_events(&mut events, false));
        assert_eq!(events.len(), 200);
        assert_eq!(events.first().unwrap()["at"], 101);
        assert_eq!(events.last().unwrap()["at"], 250);
        assert!(events.iter().any(|event| event["type"] == "hook"));
        assert!(events.iter().any(|event| event["type"] == "native"));
        let hook = events
            .iter()
            .position(|event| event["at"] == 101 && event["type"] == "hook")
            .unwrap();
        let native = events
            .iter()
            .position(|event| event["at"] == 101 && event["type"] == "native")
            .unwrap();
        assert!(hook < native, "equal timestamps retain source order");
    }

    #[test]
    fn exactly_two_hundred_does_not_infer_truncation() {
        let mut events = (1..=100)
            .map(|at| event(at, "hook"))
            .chain((101..=200).map(|at| event(at, "native")))
            .collect::<Vec<_>>();
        assert!(!merge_activity_events(&mut events, false));
        assert_eq!(events.len(), 200);
        assert_eq!(events.first().unwrap()["at"], 1);
        assert_eq!(events.last().unwrap()["at"], 200);
    }

    #[test]
    fn native_truncation_is_retained_and_ties_keep_source_order() {
        let mut events = vec![event(1, "first"), event(1, "second"), event(1, "third")];
        assert!(merge_activity_events(&mut events, true));
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }
}
