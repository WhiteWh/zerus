//! Historical original proof, separately journaled as a READ. Never send,
//! inspect, adopt a renamed identity or update the original mutation journal.
use crate::{
    execute::Failure,
    native::{Call, Native},
    runtime::Connector,
};
use serde_json::{json, Value};
use zerus_relay::{json as codec, send_receipt as contract};

pub const HELP_MARKER: &str = "delivery-receipt-v1";
pub const UNAVAILABLE: &str = "native original send receipt ABI is unavailable";
pub const MISMATCH: &str = "native original send receipt identity did not match";

pub fn native_digest(intent: &Value) -> Result<String, Failure> {
    let original = &intent["payload"];
    let bytes = if matches!(intent["provider"].as_str(), Some("dsh" | "opencode")) {
        serde_json::to_vec(&json!([intent["session"], original])).map_err(|_| Failure::Generic)?
    } else if intent["native_encoding"] == "compact" {
        serde_json::to_vec(original).map_err(|_| Failure::Generic)?
    } else {
        crate::native::stdin_bytes(original)?
    };
    Ok(codec::digest(bytes))
}

pub fn call(payload: &Value) -> Result<Call, Failure> {
    let intent = &payload["intent"];
    let original = &intent["payload"];
    let query = json!({"request_id":payload["request_id"],"message_request_id":original["request_id"],
        "expected_machine_id":intent["machine_id"],"expected_session_id":intent["session_id"],
        "expected_run_id":original["expected_run_id"],"expected_conversation_id":original["expected_conversation_id"],
        "provider":intent["provider"],"agent_id":original.get("agent_id").cloned().unwrap_or(json!("")),
        "native_digest":native_digest(intent)?});
    // A replaced or downgraded executable can outlive the cached help flags.
    // The stable, closed __state namespace refuses an unknown operation;
    // it can never reinterpret this read as a new session launch.
    let mut call = Call::new([
        "__state",
        "delivery",
        "receipt",
        intent["session"].as_str().ok_or(Failure::Generic)?,
        "--json",
    ])
    .payload(query)
    .timeout(5.0);
    call.max_output_bytes = Some(8192);
    Ok(call)
}

pub fn project(payload: &Value, answer: &Value) -> Result<Value, Failure> {
    let expected = call(payload)?.payload.unwrap();
    let intent = &payload["intent"];
    let mut result = json!({"schema":1,"request_id":payload["request_id"],"computer_id":intent["computer_id"],
        "intent_sha256":payload["intent_sha256"]});
    for (native, query) in [
        ("request_id", "request_id"),
        ("message_request_id", "message_request_id"),
        ("machine_id", "expected_machine_id"),
        ("session_id", "expected_session_id"),
        ("run_id", "expected_run_id"),
        ("conversation_id", "expected_conversation_id"),
        ("provider", "provider"),
        ("agent_id", "agent_id"),
        ("native_digest", "native_digest"),
    ] {
        if answer.get(native) != expected.get(query) {
            return Err(Failure::text(MISMATCH));
        }
        result[native] = answer[native].clone();
    }
    if answer["schema"].as_u64() != Some(1) || answer["name"] != intent["session"] {
        return Err(Failure::text(MISMATCH));
    }
    result["session"] = intent["session"].clone();
    match answer["status"].as_str() {
        Some("submitted")
            if answer["proof"] == "original_receipt"
                && answer["submitted_at"]
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && v >= 0.0)
                && answer["queued"].is_boolean() =>
        {
            result["status"] = json!("submitted");
            result["proof"] = json!("original_receipt");
            result["submitted_at"] = answer["submitted_at"].clone();
            result["queued"] = answer["queued"].clone();
            match (intent["provider"].as_str(), answer.get("native_message_id")) {
                (Some("opencode"), Some(Value::String(id)))
                    if id.starts_with("msg_")
                        && id.len() > 4
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) =>
                {
                    result["native_message_id"] = json!(id);
                }
                (Some("opencode"), _) | (_, Some(_)) => return Err(Failure::text(MISMATCH)),
                _ => {}
            }
        }
        Some("unknown") => {
            result["status"] = json!("unknown");
        }
        _ => return Err(Failure::text(MISMATCH)),
    }
    Ok(result)
}

impl<N: Native + 'static> Connector<N> {
    pub(crate) async fn send_receipt(&self, payload: &Value) -> Result<Value, Failure> {
        let intent = &payload["intent"];
        let run = intent["payload"]["expected_run_id"]
            .as_str()
            .ok_or(Failure::Generic)?;
        // Reuse the exact run->durable-ID listing index, which rejects ambiguous
        // runs. The historical name is never resolved to its current holder.
        if self.state.borrow().session_ids.get(run).map(String::as_str)
            != intent["session_id"].as_str()
        {
            return Err(Failure::text(
                "original send session binding is unavailable",
            ));
        }
        let answer = self.call(call(payload)?).await?;
        project(payload, &answer)
    }
}

pub fn validate(request: &serde_json::Map<String, Value>, id: &str) -> Result<(), String> {
    contract::validate(
        request.get("payload").unwrap_or(&Value::Null),
        id,
        request.get("computer_id").unwrap_or(&Value::Null),
        request
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .map_err(|_| "invalid original send receipt intent".into())
}
