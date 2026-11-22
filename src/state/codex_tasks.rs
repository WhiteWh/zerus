//! Separate interactive Codex tasks can share the original TUI process.
//! Native MCP creation is not a main-conversation transition.
use super::*;
mod recovery;
pub(super) use recovery::original_main_receipt;
pub(super) use recovery::recover_main;

const LIMIT: usize = 256;
const FIELD: &str = "codex_native_tasks";

fn call(event: &Value) -> Option<&str> {
    let id = string(event, "tool_use_id");
    (!id.is_empty() && id.len() <= 160 && !id.chars().any(char::is_control)).then_some(id)
}

fn creating(event: &Value) -> bool {
    [
        "mcp__codex_tui__create_thread",
        "mcp__codex_tui__fork_thread",
    ]
    .contains(&string(event, "tool_name"))
}

fn addressing(event: &Value) -> bool {
    [
        "mcp__codex_tui__send_message_to_thread",
        "mcp__codex_tui__read_thread",
    ]
    .contains(&string(event, "tool_name"))
}

fn created(event: &Value) -> Option<String> {
    let response = &event["tool_response"];
    if response["isError"] != false {
        return None;
    }
    let [part] = response["content"].as_array()?.as_slice() else {
        return None;
    };
    if part["type"] != "text" || string(part, "text").len() > 4096 {
        return None;
    }
    let result: Value = serde_json::from_str(string(part, "text")).ok()?;
    if result.as_object()?.len() != 1 {
        return None;
    }
    let id = string(&result, "threadId");
    uuid::Uuid::parse_str(id)
        .is_ok_and(|parsed| !parsed.is_nil() && parsed.to_string() == id)
        .then(|| id.to_owned())
}

/// Called only after the original main hook's run, caller and conversation checks.
/// Persist a pending creation before the native call can start its new task.
pub(super) fn observe(record: &mut Value, event: &Value) -> bool {
    if record["agent"] != "codex"
        || string(record, "run_id").is_empty()
        || !string(event, "agent_id").is_empty()
        || event["session_id"] != record["conversation_id"]
        || !(creating(event) || addressing(event))
    {
        return false;
    }
    let starting = event["hook_event_name"] == "PreToolUse";
    let ending = event["hook_event_name"] == "PostToolUse";
    if !starting && !ending {
        return false;
    }
    if !record[FIELD].is_object() || record[FIELD]["run"] != record["run_id"] {
        record[FIELD] = json!({"parent":record["conversation_id"],"run":record["run_id"],"pending":{},"threads":{}});
    }
    if record[FIELD]["parent"] != record["conversation_id"] {
        record[FIELD] = json!({"parent":record["conversation_id"],"run":record["run_id"],"pending":{},"threads":{},"unresolved":true});
    }
    let Some(id) = call(event) else {
        record[FIELD]["unresolved"] = json!(true);
        return true;
    };
    let state = &mut record[FIELD];
    if addressing(event) {
        let thread = string(&event["tool_input"], "threadId");
        if !uuid::Uuid::parse_str(thread).is_ok_and(|id| !id.is_nil() && id.to_string() == thread) {
            state["unresolved"] = json!(true);
        } else if thread != string(event, "session_id") {
            if state["threads"]
                .as_object()
                .is_some_and(|rows| rows.len() < LIMIT || rows.contains_key(thread))
            {
                state["threads"][thread] = json!(true);
            } else {
                state["unresolved"] = json!(true);
            }
        }
        return true;
    }
    if starting {
        if state["pending"]
            .as_object()
            .is_none_or(|rows| rows.len() >= LIMIT)
        {
            state["unresolved"] = json!(true);
        } else {
            state["pending"][id] = json!(now());
        }
    } else {
        if let Some(thread) = created(event).filter(|thread| thread != string(event, "session_id"))
        {
            if state["threads"]
                .as_object()
                .is_none_or(|rows| rows.len() >= LIMIT && !rows.contains_key(&thread))
            {
                state["unresolved"] = json!(true);
            } else {
                state["threads"][&thread] = json!(true);
            }
        } else if event["tool_response"]["isError"] != true {
            // A successful but unknown result must not authorize a foreign rebind.
            state["unresolved"] = json!(true);
        }
        if let Some(rows) = state["pending"].as_object_mut() {
            rows.remove(id);
        }
    }
    true
}

pub(super) fn known(record: &Value, id: &str) -> bool {
    let state = &record[FIELD];
    record["agent"] == "codex"
        && state["parent"] == record["conversation_id"]
        && state["run"] == record["run_id"]
        && state["threads"][id] == true
}

pub(super) fn unresolved(record: &Value) -> bool {
    let state = &record[FIELD];
    state["parent"] == record["conversation_id"]
        && state["run"] == record["run_id"]
        && (state["unresolved"] == true
            || state["pending"]
                .as_object()
                .is_some_and(|rows| !rows.is_empty()))
}

pub(super) fn multiple(record: &Value) -> bool {
    let state = &record[FIELD];
    record["agent"] == "codex"
        && state["parent"] == record["conversation_id"]
        && state["run"] == record["run_id"]
        && (unresolved(record)
            || state["threads"]
                .as_object()
                .is_some_and(|rows| !rows.is_empty()))
}

/// A launch receipt predates its conversation. Only this run's own main start
/// can supply that binding; a recovered or native-task binding cannot.
pub(super) fn bootstrap_start(record: &Value) -> bool {
    let conversation = string(record, "conversation_id");
    !conversation.is_empty()
        && !string(record, "run_id").is_empty()
        && !known(record, conversation)
        && record["codex_binding_recovery"]["run_id"] != record["run_id"]
        && recovery::first_start(record, conversation)
}

/// Only the pending request frozen by the proved original-main repair may
/// read its historical receipt in that receipt's original (mistaken) context.
pub(super) fn recovered_pending(record: &Value) -> bool {
    let repair = &record["codex_binding_recovery"];
    record["agent"] == "codex"
        && repair["source"] == "original_native_prompt"
        && repair["run_id"] == record["run_id"]
        && repair["to"] == record["conversation_id"]
        && !string(repair, "from").is_empty()
        && repair["from"] != repair["to"]
        && !string(repair, "pending_request").is_empty()
        && repair["pending_request"] == record["input_pending_request"]
        && repair["pending_at"] == record["input_pending_at"]
}

/// A main /new does not end other native tasks in this same supervised run.
/// Keep their negative routing evidence without carrying it into a new run.
pub(super) fn main_changed(record: &mut Value, next: &str) {
    if record[FIELD].is_object() && record[FIELD]["run"] == record["run_id"] {
        record[FIELD]["parent"] = json!(next);
    } else {
        record.as_object_mut().unwrap().remove(FIELD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_is_scoped_to_the_original_main_and_exact_native_result() {
        let child = uuid::Uuid::new_v4().to_string();
        let mut record = json!({"agent":"codex","conversation_id":"parent","run_id":"run-1"});
        let mut event = json!({"session_id":"parent","hook_event_name":"PreToolUse",
            "tool_name":"mcp__codex_tui__create_thread","tool_use_id":"call-1"});
        assert!(observe(&mut record, &event));
        assert!(unresolved(&record));
        event["hook_event_name"] = json!("PostToolUse");
        event["tool_response"] = json!({"isError":false,"content":[{"type":"text","text":json!({"threadId":child}).to_string()}]});
        assert!(observe(&mut record, &event));
        assert!(!unresolved(&record));
        assert!(known(&record, &child));
        assert_eq!(record["conversation_id"], "parent");
        event["session_id"] = json!(child);
        assert!(!observe(&mut record, &event));
    }

    #[test]
    fn concurrent_creations_do_not_release_each_others_pending_gate() {
        let mut record = json!({"agent":"codex","conversation_id":"parent","run_id":"run-1"});
        let mut event = json!({"session_id":"parent","hook_event_name":"PreToolUse",
            "tool_name":"mcp__codex_tui__create_thread","tool_use_id":"one"});
        observe(&mut record, &event);
        event["tool_use_id"] = json!("two");
        observe(&mut record, &event);
        event["hook_event_name"] = json!("PostToolUse");
        event["tool_response"] = json!({"isError":true,"content":[]});
        observe(&mut record, &event);
        assert!(unresolved(&record));
        event["tool_use_id"] = json!("one");
        observe(&mut record, &event);
        assert!(!unresolved(&record));
    }

    #[test]
    fn addressing_a_task_is_membership_only_and_cannot_cross_runs() {
        let child = uuid::Uuid::new_v4().to_string();
        let mut record = json!({"agent":"codex","conversation_id":"parent","run_id":"run-1"});
        let event = json!({"session_id":"parent","hook_event_name":"PreToolUse",
            "tool_name":"mcp__codex_tui__read_thread","tool_use_id":"read",
            "tool_input":{"threadId":child}});
        assert!(observe(&mut record, &event));
        assert!(known(&record, &child));
        assert!(multiple(&record));
        assert!(
            record.get("subagents").is_none(),
            "a read is not current-work evidence"
        );
        record["run_id"] = json!("run-2");
        assert!(!known(&record, &child));
        assert!(!multiple(&record));
    }

    #[test]
    fn repeated_native_result_at_capacity_keeps_the_exact_known_task() {
        let child = uuid::Uuid::new_v4().to_string();
        let mut threads = serde_json::Map::new();
        threads.insert(child.clone(), json!(true));
        for _ in 1..LIMIT {
            threads.insert(uuid::Uuid::new_v4().to_string(), json!(true));
        }
        let mut record = json!({"agent":"codex","conversation_id":"parent","run_id":"run-1",
            "codex_native_tasks":{"parent":"parent","run":"run-1","pending":{},"threads":threads}});
        let event = json!({"session_id":"parent","hook_event_name":"PostToolUse",
            "tool_name":"mcp__codex_tui__create_thread","tool_use_id":"known",
            "tool_response":{"isError":false,"content":[{"type":"text","text":json!({"threadId":child}).to_string()}]}});
        assert!(observe(&mut record, &event));
        assert!(known(&record, &child));
        assert!(!unresolved(&record));
        assert_eq!(record[FIELD]["threads"].as_object().unwrap().len(), LIMIT);
    }

    #[test]
    fn main_change_retains_known_membership_but_a_new_run_does_not() {
        let child = uuid::Uuid::new_v4().to_string();
        let mut record = json!({"agent":"codex","conversation_id":"parent","run_id":"run-1",
            "codex_native_tasks":{"parent":"parent","run":"run-1","pending":{},"threads":{child.clone():true}}});
        main_changed(&mut record, "new-main");
        record["conversation_id"] = json!("new-main");
        assert!(known(&record, &child));
        assert!(multiple(&record));
        assert!(
            record.get("subagents").is_none(),
            "membership does not invent work"
        );
        record["run_id"] = json!("run-2");
        assert!(!known(&record, &child));
        main_changed(&mut record, "next-run-main");
        assert!(record.get(FIELD).is_none());
    }
}
