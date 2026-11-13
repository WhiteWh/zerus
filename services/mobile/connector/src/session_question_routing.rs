//! The person's per-child switch of the owner-approved question route
//! (zerus-sa9qn.32/.33): whether a supervised child's blocking questions go
//! to its parent session. A Rust-only mutation gated by its own help marker;
//! the host keeps the record. Its refusal of an agent's process tree, which
//! this connector's native call never is, is a guard against accidental agent
//! use, not a security boundary (zerus-sa9qn.49).
use serde_json::Value;

pub const OPERATION: &str = "session_question_routing";
pub const HELP_MARKER: &str = "session-question-routing-v1";
pub const UNAVAILABLE: &str = "native question routing ABI is unavailable";

pub fn validate(payload: &Value) -> Result<(), String> {
    let keys = [
        "expected_run_id",
        "expected_conversation_id",
        "expected_revision",
        "send_to_parent",
    ];
    if !payload
        .as_object()
        .is_some_and(|p| p.len() == keys.len() && keys.iter().all(|k| p.contains_key(*k)))
    {
        return Err(
            "session_question_routing requires exact run, conversation, revision and switch fields"
                .into(),
        );
    }
    for (key, maximum) in [("expected_run_id", 128), ("expected_conversation_id", 256)] {
        if !payload[key].as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= maximum
                && !s.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
        }) {
            return Err("invalid question routing identity".into());
        }
    }
    if !payload["send_to_parent"].is_boolean()
        || !payload["expected_revision"]
            .as_i64()
            .is_some_and(|r| r >= 0)
    {
        return Err("invalid question routing switch".into());
    }
    Ok(())
}

/// The host's exact reply to this payload: its closed refusals, or the
/// switch it confirmed for this run and conversation at the next revision
/// (or the same one when nothing changed).
pub fn result_valid(result: &Value, payload: &Value) -> bool {
    if result["ok"] == false {
        return result.get("applied").is_none()
            && matches!(
                result["error"].as_str(),
                Some(
                    "not_found"
                        | "stale_identity"
                        | "conflict"
                        | "unsupported"
                        | "agent_refused"
                        | "capacity"
                        | "revision_exhausted"
                )
            );
    }
    let routing = &result["question_routing"];
    let (Some(applied), Some(expected), Some(revision)) = (
        result["applied"].as_bool(),
        payload["expected_revision"].as_i64(),
        routing["revision"].as_i64(),
    ) else {
        return false;
    };
    result["ok"] == true
        && result.as_object().is_some_and(|object| object.len() == 5)
        && result["run_id"] == payload["expected_run_id"]
        && result["conversation_id"] == payload["expected_conversation_id"]
        // v1 is the switch and its revision; v2 adds the person's latch
        // mirror, display-only (zerus-sa9qn.50).
        && routing.as_object().is_some_and(|object| {
            object.len() == 2
                || (object.len() == 3 && object.get("person_off").is_some_and(Value::is_boolean))
        })
        && routing["send_to_parent"].is_boolean()
        && routing["send_to_parent"] == payload["send_to_parent"]
        && expected.checked_add(i64::from(applied)) == Some(revision)
}
