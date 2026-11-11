//! Exact frozen ordinary-send intents. This additive READ never grants replay.
use crate::{
    error::{Error, Result},
    json, protocol,
};
use serde_json::{json, Value};

pub const OPERATION: &str = "send_receipt";
pub const MAX_TEXT: usize = 65536;

/// The canonical intent has only bounded strings, integer schema and an empty
/// attachment array. Canonical JSON uses sorted keys and lowercase ASCII
/// escapes, with no whitespace, normalization or trimming of string content.
pub fn intent_digest(intent: &Value) -> Result<String> {
    Ok(json::digest(json::canonical(intent)?))
}

pub fn validate(payload: &Value, id: &str, computer: &Value, session: &str) -> Result<()> {
    protocol::fields(
        payload,
        &["schema", "request_id", "intent", "intent_sha256"],
        &[],
    )?;
    protocol::uuid(&payload["request_id"], true)?;
    let intent = &payload["intent"];
    protocol::fields(
        intent,
        &[
            "schema",
            "computer_id",
            "machine_id",
            "session_id",
            "session",
            "provider",
            "native_encoding",
            "payload",
        ],
        &[],
    )?;
    if payload["schema"].as_u64() != Some(1)
        || intent["schema"].as_u64() != Some(1)
        || payload["request_id"] != id
        || &intent["computer_id"] != computer
        || intent["session"] != session
        || session.is_empty()
        || session.len() > 512
        || session.chars().any(char::is_control)
        || !matches!(
            intent["provider"].as_str(),
            Some("claude" | "codex" | "kimi" | "dsh" | "opencode")
        )
        || !matches!(
            intent["native_encoding"].as_str(),
            Some("python_default" | "compact")
        )
    {
        return Err(Error::BAD);
    }
    protocol::uuid(&intent["machine_id"], true)?;
    protocol::uuid(&intent["session_id"], true)?;
    let original = &intent["payload"];
    protocol::uuid(&original["request_id"], true)?;
    if original["request_id"] == id
        || original["text"].as_str().is_none_or(|s| s.len() > MAX_TEXT)
        || original
            .get("attachments")
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::BAD);
    }
    // Reuse the send admission contract without executing it. Original options
    // are never synthesized: absent and explicitly empty remain distinct bytes.
    protocol::request(
        &json!({"request_id":original["request_id"],"computer_id":computer,
        "operation":"send","session":session,"payload":original}),
    )?;
    if payload["intent_sha256"].as_str() != Some(intent_digest(intent)?.as_str()) {
        return Err(Error::BAD);
    }
    Ok(())
}
