//! `Connector.validate` (`connector.py:618-731`) and the validators it calls,
//! for the operations this build implements, with Python's order of checks
//! and its phone-visible texts. The connector is laxer than the relay in
//! places and stricter in others; both are reproduced, never unified.
use crate::pyvalue::{
    bounded, canonical_uuid, int_literal, int_within, py_len, py_uuid, token, PyError,
};
use serde_json::{Map, Value};
use std::collections::HashSet;
use zerus_relay::json;

/// An archived target may be named only this long after the snapshot that
/// listed it (seconds).
pub const ARCHIVE_SNAPSHOT_TTL: f64 = 45.0;
pub const MAX_PAYLOAD: usize = crate::native::MAX_PAYLOAD;
const IDENTITY_FIELDS: [&str; 3] = ["request_id", "expected_run_id", "expected_conversation_id"];
/// Operations that may name an archive in their payload besides inspect.
const ARCHIVED_OUTPUT: [&str; 9] = [
    "process_output",
    "history",
    "history_projection",
    "restore",
    "forget",
    "rename",
    "fork",
    "attachment_read",
    "file_read",
];
const UINT63: &str = "9223372036854775807";

pub const UNSUPPORTED: &str = "unsupported operation";
pub const REQUEST_ID: &str = "request_id must be a canonical UUID";
pub(crate) const SESSION: &str = "session must match an exact local snapshot entry";
pub(crate) const NONARCHIVED: &str = "session must match an exact nonarchived snapshot entry";

/// What the last local snapshot listed.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub sessions: HashSet<String>,
    pub archives: HashSet<(String, String)>,
    /// Seconds since that snapshot (infinite before the first).
    pub snapshot_age: f64,
    /// Archives a page, read or lookup admitted within the last hour.
    pub paged: HashSet<(String, String)>,
    /// The CLI pages archives: any archive passes validation and is checked
    /// natively before it runs (`archive_preflight`).
    pub archive_supported: bool,
}

impl Context {
    /// `archive_allowed(session, archive_id)`.
    pub fn admits(&self, session: &str, archive: &str) -> bool {
        let pair = (session.to_owned(), archive.to_owned());
        (self.snapshot_age <= ARCHIVE_SNAPSHOT_TTL && self.archives.contains(&pair))
            || self.paged.contains(&pair)
    }
}

/// A validated request: operation, session and the native payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub operation: String,
    pub session: String,
    pub payload: Option<Value>,
}

fn error(message: impl Into<String>) -> String {
    message.into()
}

fn from_py(error: PyError) -> String {
    match error {
        PyError::Value(message) => message,
        // No validator here raises one; Python would report the generic text.
        PyError::Type => crate::execute::GENERIC.into(),
    }
}

/// `checked_uuid(value)`.
pub fn checked_uuid(value: Option<&Value>) -> Result<String, String> {
    match value.and_then(Value::as_str) {
        Some(text) if canonical_uuid(text) => Ok(text.into()),
        _ => Err(error(REQUEST_ID)),
    }
}

fn keys(payload: &Map<String, Value>) -> HashSet<&str> {
    payload.keys().map(String::as_str).collect()
}

fn has(payload: &Map<String, Value>, key: &str) -> bool {
    payload.contains_key(key)
}

/// `context.validate_inspect(payload)`.
pub fn validate_inspect(payload: &Value) -> Result<(), String> {
    let allowed = [
        "archive_id",
        "after",
        "agent_id",
        "expected_run_id",
        "expected_conversation_id",
    ];
    let Some(payload) = payload
        .as_object()
        .filter(|payload| payload.keys().all(|key| allowed.contains(&key.as_str())))
    else {
        return Err(error("invalid inspect fields"));
    };
    let features = has(payload, "after") || has(payload, "agent_id");
    if features || has(payload, "expected_run_id") || has(payload, "expected_conversation_id") {
        bounded(payload.get("expected_run_id"), 128, false).map_err(from_py)?;
        bounded(payload.get("expected_conversation_id"), 256, false).map_err(from_py)?;
    }
    if let Some(after) = payload.get("after") {
        if !int_literal(after).is_some_and(|digits| int_within(&digits, "0", UINT63)) {
            return Err(error("invalid native history cursor"));
        }
    }
    if let Some(agent) = payload.get("agent_id") {
        let agent = bounded(Some(agent), 160, false).map_err(from_py)?;
        if agent.contains(['/', '\\']) || has(payload, "after") {
            return Err(error("invalid or unsupported subagent history cursor"));
        }
    }
    Ok(())
}

fn identity_fields(
    payload: &Value,
    request_id: &str,
    extras: &[&str],
    message: &str,
) -> Result<(), String> {
    let valid = payload.as_object().is_some_and(|payload| {
        let keys = keys(payload);
        IDENTITY_FIELDS.iter().all(|field| keys.contains(field))
            && keys
                .iter()
                .all(|key| IDENTITY_FIELDS.contains(key) || extras.contains(key))
            && payload.get("request_id").and_then(Value::as_str) == Some(request_id)
    });
    if valid {
        Ok(())
    } else {
        Err(error(message))
    }
}

/// `context.validate_core` for `process_output`.
pub fn validate_process_output(payload: &Value, request_id: &str) -> Result<(), String> {
    identity_fields(
        payload,
        request_id,
        &["process_id", "generation", "archive_id"],
        "invalid scoped native operation fields",
    )?;
    bounded(payload.get("expected_run_id"), 128, false).map_err(from_py)?;
    bounded(payload.get("expected_conversation_id"), 256, true).map_err(from_py)?;
    token(payload.get("process_id"), 160).map_err(from_py)?;
    if let Some(generation) = payload.get("generation") {
        token(Some(generation), 256).map_err(from_py)?;
    }
    if let Some(archive) = payload.get("archive_id") {
        let value = bounded(Some(archive), 36, false).map_err(from_py)?;
        py_uuid(&value).map_err(from_py)?;
        if !canonical_uuid(&value) {
            return Err(error("invalid archive identity"));
        }
    }
    Ok(())
}

/// `history.validate_history(payload, request_id)`.
pub fn validate_history(payload: &Value, request_id: &str) -> Result<(), String> {
    identity_fields(
        payload,
        request_id,
        &[
            "archive_id",
            "agent_id",
            "limit",
            "before",
            "after",
            "around",
            "around_incoming_seq",
            "history_epoch",
        ],
        "invalid exact history fields",
    )?;
    let object = payload.as_object().expect("checked");
    bounded(payload.get("expected_run_id"), 128, false).map_err(from_py)?;
    bounded(payload.get("expected_conversation_id"), 256, false).map_err(from_py)?;
    if let Some(limit) = payload.get("limit") {
        if !int_literal(limit).is_some_and(|digits| int_within(&digits, "1", "100")) {
            return Err(error("invalid history page limit"));
        }
    }
    let selectors = ["before", "after", "around", "around_incoming_seq"]
        .iter()
        .filter(|key| has(object, key))
        .count();
    if selectors > 1 {
        return Err(error("choose one history selector"));
    }
    for field in ["before", "after", "around"] {
        if let Some(value) = payload.get(field) {
            bounded(Some(value), 2048, false).map_err(from_py)?;
        }
    }
    if has(object, "around_incoming_seq") != has(object, "history_epoch") {
        return Err(error("incoming anchor requires exact history epoch"));
    }
    if let Some(sequence) = payload.get("around_incoming_seq") {
        if !int_literal(sequence).is_some_and(|digits| int_within(&digits, "0", UINT63)) {
            return Err(error("invalid incoming sequence"));
        }
        bounded(payload.get("history_epoch"), 128, false).map_err(from_py)?;
    }
    if let Some(agent) = payload.get("agent_id") {
        let agent = bounded(Some(agent), 160, false).map_err(from_py)?;
        if agent.contains(['/', '\\']) {
            return Err(error("invalid child identity"));
        }
    }
    if let Some(archive) = payload.get("archive_id") {
        let value = bounded(Some(archive), 36, false).map_err(from_py)?;
        let parsed = py_uuid(&value).map_err(from_py)?;
        if !canonical_uuid(&value) || parsed == 0 {
            return Err(error("invalid archive identity"));
        }
    }
    Ok(())
}

/// Projection requests retain the same outer UUID and target admission as history.
/// The shared relay validator checks only this new ABI, including UTF-8 byte limits.
pub fn validate_history_projection(payload: &Value, request_id: &str) -> Result<(), String> {
    identity_fields(
        payload,
        request_id,
        &[
            "archive_id",
            "agent_id",
            "history_epoch",
            "projection_after",
            "projection_until",
        ],
        "invalid history projection request",
    )?;
    zerus_relay::protocol::history_projection(payload)
        .map_err(|_| error("invalid history projection request"))
}

/// `context.validate_mark_read(payload)`.
pub fn validate_mark_read(payload: &Value) -> Result<(), String> {
    let fields = [
        "run_id",
        "conversation_id",
        "reply_id",
        "attention_signature",
    ];
    let exact = payload.as_object().is_some_and(|payload| {
        payload.len() == fields.len() && fields.iter().all(|field| payload.contains_key(*field))
    });
    if !exact {
        return Err(error(
            "mark_read requires exactly run, conversation, reply and attention fields",
        ));
    }
    bounded(payload.get("run_id"), 128, false).map_err(from_py)?;
    bounded(payload.get("conversation_id"), 256, false).map_err(from_py)?;
    if !payload["reply_id"].is_null() {
        bounded(payload.get("reply_id"), 256, false).map_err(from_py)?;
    }
    let signature = &payload["attention_signature"];
    if !signature.is_null()
        && !signature.as_str().is_some_and(|text| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(error("invalid attention signature"));
    }
    Ok(())
}

fn absolute(value: Option<&Value>, message: &str) -> Result<String, String> {
    let value = bounded(value, 4096, false).map_err(from_py)?;
    if !value.starts_with('/') {
        return Err(error(message));
    }
    Ok(value)
}

/// `launch.validate` for the catalog reads and the creations, and
/// `worktrees.validate` for the worktree listing.
pub fn validate_launch(
    operation: &str,
    payload: Option<&Value>,
    request_id: &str,
) -> Result<(), String> {
    if crate::launch::handles(operation) {
        return crate::launch::validate(operation, payload, request_id).map_err(from_py);
    }
    let object = payload.and_then(Value::as_object);
    if operation == "worktrees" {
        let exact = object.is_some_and(|payload| {
            payload.len() == 2
                && payload.contains_key("path")
                && payload.get("request_id").and_then(Value::as_str) == Some(request_id)
        });
        if !exact {
            return Err(error("invalid scoped worktree fields"));
        }
        absolute(
            object.and_then(|payload| payload.get("path")),
            "worktree path must be absolute",
        )?;
        return Ok(());
    }
    let fields: &[&str] = match operation {
        "catalog" => &["request_id"],
        _ => &["request_id", "path"],
    };
    let exact = object.is_some_and(|payload| {
        payload.len() == fields.len()
            && fields.iter().all(|field| payload.contains_key(*field))
            && payload.get("request_id").and_then(Value::as_str) == Some(request_id)
    });
    if !exact {
        return Err(error("invalid scoped catalog/launch fields"));
    }
    if operation == "dirs" {
        let path = bounded(object.and_then(|payload| payload.get("path")), 4096, false)
            .map_err(from_py)?;
        if path != "~" && !path.starts_with('/') {
            return Err(error("directory must be absolute or home"));
        }
    }
    Ok(())
}

pub(crate) fn valid_session(session: Option<&Value>) -> Option<&str> {
    let session = session?.as_str()?;
    let control = |c: char| (c as u32) < 32 || c as u32 == 127;
    (!session.is_empty()
        && py_len(session) <= 512
        && !session.starts_with(['-', '@'])
        && !session.contains('@')
        && !session.chars().any(control))
    .then_some(session)
}

/// `Connector.validate(request)` for an implemented operation, after the
/// claim. `request_id` is the checked identity.
pub fn validate(
    request: &Map<String, Value>,
    request_id: &str,
    context: &Context,
) -> Result<Validated, String> {
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .filter(|operation| crate::capabilities::implemented(operation))
        .ok_or_else(|| error(UNSUPPORTED))?;
    if operation == "send_receipt" {
        crate::send_receipt::validate(request, request_id)?;
        return Ok(Validated {
            operation: operation.into(),
            session: request["session"].as_str().expect("validated").into(),
            payload: request.get("payload").cloned(),
        });
    }
    if crate::media::handles(operation) {
        return crate::media::validate(operation, request, request_id, context);
    }
    let validated = |session: &str, payload: Option<Value>| Validated {
        operation: operation.into(),
        session: session.into(),
        payload,
    };
    if zerus_relay::repositories::OPERATIONS.contains(&operation) {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("repository acquisition uses no session target"));
        }
        zerus_relay::repositories::validate(
            operation,
            request.get("payload").unwrap_or(&Value::Null),
            request_id,
        )
        .map_err(|_| error("invalid repository acquisition fields"))?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    if operation == crate::git_status::OPERATION {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("Git status uses no session target"));
        }
        crate::git_status::validate(request.get("payload").unwrap_or(&Value::Null), request_id)?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    if operation == "analytics_read" {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("analytics reads use no session target"));
        }
        zerus_relay::protocol::analytics_read(request.get("payload").unwrap_or(&Value::Null))
            .map_err(|_| error("invalid analytics read fields or UTC range"))?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    if operation == "archive_list" {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("archive browsing uses no session target"));
        }
        crate::archives::validate(request.get("payload"))?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    if crate::capabilities::LAUNCH.contains(&operation) {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("catalog/launch use no existing session target"));
        }
        validate_launch(operation, request.get("payload"), request_id)?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    if zerus_relay::protocol::TASKS.contains(&operation) {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("the phone's tasks use no session target"));
        }
        let payload = request.get("payload").unwrap_or(&Value::Null);
        zerus_relay::protocol::task_payload(operation, payload)
            .map_err(|_| error("invalid task request"))?;
        return Ok(validated("", Some(payload.clone())));
    }
    if operation == "set_mobile_delivery" {
        if request.get("session").and_then(Value::as_str) != Some("") {
            return Err(error("the delivery setting uses no session target"));
        }
        crate::mutations::validate_delivery(request.get("payload"))?;
        return Ok(validated("", request.get("payload").cloned()));
    }
    let session = valid_session(request.get("session")).ok_or_else(|| error(SESSION))?;
    if request.contains_key("archive_id") {
        return Err(error("archive identity belongs only in inspect payload"));
    }
    if operation == crate::session_notifications::OPERATION {
        if !context.sessions.contains(session) {
            return Err(error(NONARCHIVED));
        }
        let payload = request.get("payload").unwrap_or(&Value::Null);
        crate::session_notifications::validate(payload).map_err(error)?;
        return Ok(validated(session, Some(payload.clone())));
    }
    if operation == crate::session_question_routing::OPERATION {
        if !context.sessions.contains(session) {
            return Err(error(NONARCHIVED));
        }
        let payload = request.get("payload").unwrap_or(&Value::Null);
        crate::session_question_routing::validate(payload).map_err(error)?;
        return Ok(validated(session, Some(payload.clone())));
    }
    if operation == "mark_read" {
        if !context.sessions.contains(session) {
            return Err(error(NONARCHIVED));
        }
        let payload = request.get("payload").unwrap_or(&Value::Null);
        validate_mark_read(payload)?;
        return Ok(validated(session, Some(payload.clone())));
    }
    if operation == "inspect" {
        let empty = Value::Object(Map::new());
        let payload = request.get("payload").unwrap_or(&empty);
        validate_inspect(payload)?;
        if let Some(archive) = payload.get("archive_id") {
            let archive = checked_uuid(Some(archive))?;
            if !context.admits(session, &archive) && !context.archive_supported {
                if context.snapshot_age > ARCHIVE_SNAPSHOT_TTL {
                    return Err(error(
                        "archive snapshot is stale; refresh before inspection",
                    ));
                }
                return Err(error("archive must match an exact local snapshot entry"));
            }
            return Ok(validated(session, Some(payload.clone())));
        }
        if !context.sessions.contains(session) {
            return Err(error(NONARCHIVED));
        }
        let payload = payload
            .as_object()
            .filter(|payload| !payload.is_empty())
            .map(|_| payload.clone());
        return Ok(validated(session, payload));
    }
    // Every other session operation: the scoped path.
    let payload = request.get("payload");
    let object = payload.and_then(Value::as_object);
    if let Some(archive) = object.and_then(|payload| payload.get("archive_id")) {
        if !ARCHIVED_OUTPUT.contains(&operation) {
            return Err(error("archived targets support inspection only"));
        }
        let archive = checked_uuid(Some(archive))?;
        if !context.admits(session, &archive) && !context.archive_supported {
            return Err(error(
                "archive must match a fresh exact local snapshot entry",
            ));
        }
    } else if !context.sessions.contains(session) {
        return Err(error(NONARCHIVED));
    }
    let Some(object) = object
        .filter(|payload| payload.get("request_id").and_then(Value::as_str) == Some(request_id))
    else {
        return Err(error("payload request_id must match request identity"));
    };
    let run = object.get("expected_run_id").and_then(Value::as_str);
    if run.is_none_or(str::is_empty) {
        return Err(error("expected_run_id is required"));
    }
    let payload = payload.expect("checked");
    match operation {
        "history" => validate_history(payload, request_id)?,
        "history_projection" => validate_history_projection(payload, request_id)?,
        "project_assignment" | "project_assign" => {
            crate::session_projects::validate(operation, payload, request_id)?
        }
        "hooks_status" => {
            crate::mutations::validate_for_session("clear_context", session, payload, request_id)?
        }
        "process_output" => validate_process_output(payload, request_id)?,
        _ => crate::mutations::validate_for_session(operation, session, payload, request_id)?,
    }
    match json::python_default_len(payload, false) {
        Ok(size) if size > MAX_PAYLOAD => Err(error(crate::native::REQUEST_TOO_LARGE)),
        Ok(_) => Ok(validated(session, Some(payload.clone()))),
        Err(_) => Err(error("invalid native payload")),
    }
}
