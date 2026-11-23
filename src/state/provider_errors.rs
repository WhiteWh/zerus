//! Native provider failures, separate from ordinary tool failures and prose.
use super::*;
use rusqlite::{Connection, OpenFlags};

fn structured_kind(text: &str) -> Option<&'static str> {
    let mut reset = None;
    for word in text.split(|c: char| c.is_whitespace() || "[]():,\"".contains(c)) {
        let kind = match word.strip_suffix("_error").unwrap_or(word) {
            "provider_policy" => Some("provider_policy"),
            "session_limit" => Some("session_limit"),
            "model_unavailable" => Some("model_unavailable"),
            "invalid_request" => Some("provider"),
            _ => None,
        };
        if kind == Some("session_limit") { reset = kind; }
        else if kind.is_some() { return kind; }
    }
    reset
}

// Native structured refusal/reset evidence precedes a known transient message.
// Generic saved error_kind=quota is deliberately not native type/code evidence.
pub(super) fn native_kind(error: &Value) -> Option<&'static str> {
    // A present non-string is invalid evidence, not an absent native code.
    // Keep it manual even when the human message contains a known 429 window.
    if ["native_error_type", "native_error_code"].iter()
        .any(|key| error.get(*key).is_some_and(|value| !value.is_string())) {
        return Some("provider");
    }
    if category(string(error, "detail")) == "provider_policy" { return Some("provider_policy"); }
    let mut reset = None;
    for key in ["native_error_type", "native_error_code"] {
        let value = string(error, key).to_lowercase();
        let kind = structured_kind(&value).unwrap_or_else(|| category(&value));
        if kind == "session_limit" { reset = Some(kind); }
        else if ["quota", "authentication", "context_limit", "model_unavailable", "provider_policy"].contains(&kind)
            || structured_kind(&value).is_some() { return Some(kind); }
    }
    reset
}

pub(super) fn rolling_rate_limit(text: &str) -> bool {
    let text = text.to_lowercase().replace('’', "'");
    let Some((prefix, rest)) = text.split_once("429 you've reached your 5-hour usage limit.") else {
        return false;
    };
    // Kimi's rolling rate window is separate from its monthly total quota.
    // Its native error includes quota wording and an upsell, but no reset time.
    rolling_window_reset(rest) && rolling_window_context(&text, prefix)
}

fn rolling_window_reset(rest: &str) -> bool {
    rest.trim_start().starts_with("your quota will reset when the current 5-hour window ends.")
}

fn rolling_window_context(text: &str, prefix: &str) -> bool {
    prefix.chars().last().is_none_or(|c| !c.is_ascii_alphanumeric())
        && structured_kind(prefix).is_none()
        && !["authentication", "auth_error", "autherror", "auth error", "auth failed", "auth_failed",
             "auth failure", "authorization", "not authorized", "not authorised", "unauthorized", "unauthorised",
             "api key", "api_key", "credential", "oauth", "insufficient", "account_on_hold",
             "monthly quota", "weekly quota", "billing", "credit", "balance", "context", "max_output_tokens",
             "max-tokens", "token limit", "model_not_found", "model not found", "not supported", "invalid request"]
            .iter().any(|word| text.contains(word))
        && !prefix.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| ["401", "403"].contains(&word))
}

pub(super) const KIMI_RATE_LIMIT_TYPE: &str = "APIProviderRateLimitError";

/// Kimi 2.5.2's StopFailure supplies the native type but leaves HTTP status
/// out of error_message. Retain that evidence rather than inventing a 429 in
/// the displayed error. A generic saved quota/rate label is not this proof.
pub(super) fn rolling_rate_error(error: &Value) -> bool {
    if native_kind(error).is_some() { return false; }
    if rolling_rate_limit(&native_detail(error)) { return true; }
    let code = match error.get("native_error_code") {
        None => "",
        Some(Value::String(code)) => code,
        _ => return false,
    };
    if string(error, "source") != "provider_hook"
        || string(error, "native_error_type") != KIMI_RATE_LIMIT_TYPE
        || !code.is_empty() {
        return false;
    }
    let text = string(error, "detail").to_lowercase().replace('’', "'");
    let Some(rest) = text.strip_prefix("you've reached your 5-hour usage limit.") else { return false; };
    rolling_window_reset(rest) && rolling_window_context(&text, "") && structured_kind(&text).is_none()
        && !text.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| ["401", "403"].contains(&word))
}

pub(super) fn native_detail(error: &Value) -> String {
    format!("{} {} {}", string(error, "native_error_type"), string(error, "native_error_code"), string(error, "detail"))
}

pub(super) fn category(text: &str) -> &'static str {
    let text = text.to_lowercase();
    // A provider policy block can quote authorized work or a transport status.
    // Keep that explicit refusal separate from credentials and transient errors.
    if [
        "cyber_policy",
        "policy_violation",
        "policyviolation",
        "content_filter",
        "contentfilter",
        "safety_violation",
        "safetyviolation",
        "request_blocked_by_policy",
    ]
    .iter()
    .any(|s| text.contains(s))
        || (["usage policy", "content policy", "safety policy", "cybersecurity"]
            .iter()
            .any(|s| text.contains(s))
            && ["flagged", "blocked", "rejected", "refused", "violat"]
                .iter()
                .any(|s| text.contains(s)))
    {
        return "provider_policy";
    }
    if rolling_rate_limit(&text) {
        return "rate_limit";
    }
    // A permanent quota failure can also mention 429 or model capacity.
    if [
        "quota",
        "usage limit",
        "usage_limit",
        "usagelimitexceeded",
        "weekly limit",
        "7d limit reached",
        "insufficient",
        "billing",
        "credit",
        "account_on_hold",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        "quota"
    } else if session_limits::recognized(&text) {
        "session_limit"
    } else if ["capacity", "overload", "model_capacity"]
        .iter()
        .any(|s| text.contains(s))
    {
        "capacity"
    } else if ["rate_limit", "rate limit", "too many requests", "429"]
        .iter()
        .any(|s| text.contains(s))
    {
        "rate_limit"
    } else if [
        "authentication",
        "auth_error",
        "autherror",
        "auth error",
        "auth failed",
        "auth_failed",
        "auth failure",
        "authorization failed",
        "authorization error",
        "authorization_error",
        "not authorized",
        "not authorised",
        "unauthorized",
        "unauthorised",
        "credential",
        "api key",
        "api_key",
        "oauth",
    ]
    .iter()
    .any(|s| text.contains(s))
        || text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == "401")
    {
        "authentication"
    } else if ["context", "max_output_tokens", "max-tokens", "token limit"]
        .iter()
        .any(|s| text.contains(s))
    {
        "context_limit"
    } else if ["model_not_found", "model not found", "does not exist"]
        .iter()
        .any(|s| text.contains(s))
    {
        "model_unavailable"
    } else {
        "provider"
    }
}

pub(super) fn event(id: &str, at: f64, detail: &str, source: &str) -> Value {
    let detail = journal::clipped(&json!(detail), 1200);
    let mut error = json!({"type":"StopFailure","source":source,"message_id":id,"agent_id":"","at":at,
        "detail":detail,"error_kind":category(&detail)});
    session_limits::enrich(&mut error);
    error
}

pub(super) fn failure_id(error: &Value) -> String {
    if !error.is_object() { return String::new(); }
    format!("{}:{}:{}", string(error, "source"), string(error, "message_id"), error["at"].as_f64().unwrap_or(0.0))
}

pub(super) fn apply(output: &mut Value, error: &Value) {
    // Normalize saved failures too; older hooks used the generic rate-limit kind.
    let mut error = error.clone();
    let detail = native_detail(&error);
    let rolling = rolling_rate_error(&error);
    let native = native_kind(&error);
    let kind = if let Some(kind) = native {
        kind.to_owned()
    } else if rolling {
        if ["", "provider", "quota", "rate_limit"].contains(&string(&error, "error_kind")) {
            "rate_limit".to_owned()
        } else { string(&error, "error_kind").to_owned() }
    } else {
        category(&format!("{} {}", string(&error, "error_kind"), detail)).to_owned()
    };
    if native.is_some() || ["session_limit", "quota", "provider_policy"].contains(&kind.as_str())
        || (kind == "rate_limit" && rolling) {
        error["error_kind"] = json!(kind);
    }
    session_limits::enrich(&mut error);
    output["phase"] = json!("error");
    output["activity"] = json!("attention");
    output["last_error"] = error["detail"].clone();
    output["activity_summary"] = json!(match string(&error, "error_kind") {
        "capacity" => "Model at capacity",
        "quota" => "Usage limit reached",
        "rate_limit" => "Rate limit reached",
        "session_limit" => "Session limit reached",
        "authentication" => "Sign-in failed",
        "provider_policy" => "Request blocked by provider",
        "context_limit" => "Context or output limit reached",
        "model_unavailable" => "Model unavailable",
        _ => "Provider error",
    });
    output["activity_detail"] = error["detail"].clone();
    output["provider_error"] = error.clone();
    output["attention_id"] = json!(format!(
        "{}:{}",
        string(&error, "source"),
        string(&error, "message_id")
    ));
    output["current_tool"] = json!("");
    output["tool_detail"] = json!("");
}

fn codex_log_detail(target: &str, body: &str) -> Option<String> {
    let marker = match target {
        "codex_core::session::turn" | "codex_core::codex" => {
            if body.contains("Turn error: ") {
                "Turn error: "
            } else if target == "codex_core::session::turn" {
                "Post-turn compaction failed; preserving the completed turn error="
            } else {
                return None;
            }
        }
        "codex_core::tasks" => "session task returned an unexpected error err=",
        _ => return None,
    };
    let detail = body.strip_prefix(marker).or_else(|| {
        body.rsplit_once(&format!(": {marker}"))
            .map(|(_, detail)| detail)
    })?;
    // Codex preserves a successful answer after other post-turn compaction
    // warnings. Only quota exhaustion marks that completed turn as failed.
    if marker.starts_with("Post-turn compaction") && category(detail) != "quota" {
        return None;
    }
    (!detail.trim().is_empty()).then(|| detail.trim().to_owned())
}

fn codex_logs(record: &Value) -> Result<Vec<Value>> {
    let conversation = string(record, "conversation_id");
    if uuid::Uuid::parse_str(conversation).is_err() {
        return Ok(vec![]);
    }
    let native_home = string(record, "agent_home");
    let native_home = if native_home.is_empty() {
        home().join(".codex")
    } else if Path::new(native_home).is_absolute() {
        PathBuf::from(native_home)
    } else {
        PathBuf::from(string(record, "launch_dir")).join(native_home)
    };
    let db = Connection::open_with_flags(
        native_home.join("logs_2.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_millis(100))
        .map_err(|e| e.to_string())?;
    // Native emitters cover regular turns and compaction failures. Do not
    // interpret tool output or ordinary account-usage telemetry as a failure.
    let mut query = db
        .prepare(
            "SELECT id, ts, ts_nanos, feedback_log_body, target FROM logs
        WHERE thread_id = ? AND target IN ('codex_core::session::turn','codex_core::codex','codex_core::tasks')
        AND (feedback_log_body LIKE '%Turn error: %'
          OR feedback_log_body LIKE '%Post-turn compaction failed; preserving the completed turn error=%'
          OR feedback_log_body LIKE '%session task returned an unexpected error err=%')
        ORDER BY ts DESC, ts_nanos DESC LIMIT 8",
        )
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([conversation], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for row in rows.flatten() {
        if let Some(detail) = codex_log_detail(&row.4, &row.3) {
            errors.push(event(
                &format!("codex-error-{}", row.0),
                row.1 as f64 + row.2 as f64 / 1e9,
                &detail,
                "codex_native_log",
            ));
        }
    }
    errors.reverse();
    Ok(errors)
}

fn codex_transcript(record: &Value) -> Result<(Vec<Value>, f64)> {
    codex_transcript_snapshot(record,None)
}

fn codex_transcript_snapshot(record: &Value, boundary: Option<u64>) -> Result<(Vec<Value>, f64)> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    #[cfg(test)]
    TRANSCRIPT_SCANS.with(|count| count.set(count.get() + 1));
    let id = string(record, "conversation_id");
    let path = PathBuf::from(string(record, "transcript"));
    if uuid::Uuid::parse_str(id).is_err()
        || !path.is_absolute()
        || !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(id))
    {
        return Ok((vec![], 0.0));
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut first = String::new();
    BufReader::new((&mut file).take(256 * 1024))
        .read_line(&mut first)
        .map_err(|e| e.to_string())?;
    let first: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
    if first["type"] != "session_meta" || first["payload"]["id"] != id {
        return Err("Conversation transcript identity mismatch".into());
    }
    let length = file
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    let length = boundary.map_or(length,|boundary|boundary.min(length));
    let offset = length.saturating_sub(8*1024*1024);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(boundary.map_or(8*1024*1024,|_|length-offset))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    let mut progress: f64 = 0.0;
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if (offset > 0 && index == 0) || line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            continue;
        }
        if !std::str::from_utf8(line).is_ok_and(|text| {
            text.contains("\"error\"")
                || text.contains("\"task_started\"")
                || text.contains("\"task_complete\"")
                || text.contains("\"turn_complete\"")
        }) {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let (error, advanced) = watch_event(&row);
        progress = progress.max(advanced);
        errors.extend(error);
    }
    if errors.len() > 8 {
        errors.drain(..errors.len() - 8);
    }
    Ok((errors, progress))
}

/// Pure native-event reduction shared by the full scan and watch's append
/// index. No log correlation or action admission is cached here.
pub(super) fn watch_event(row: &Value) -> (Option<Value>, f64) {
    if row["type"] != "event_msg" { return (None,0.); }
    let payload = &row["payload"];
    let at = search::timestamp(&row["timestamp"]);
    if at <= 0. { return (None,0.); }
    let failure = match string(payload,"type") {
        "task_started" => return (None,at),
        "task_complete" | "turn_complete" => {
            if payload["error"].is_null() {
                return (None,if !string(payload,"last_agent_message").trim().is_empty() {at} else {0.});
            }
            &payload["error"]
        }
        "error" => payload,
        _ => return (None,0.),
    };
    let detail = string(failure,"message");
    if detail.trim().is_empty() { return (None,0.); }
    let mut error = event(&format!("codex-turn-error-{}-{at}",string(payload,"turn_id")),at,detail,"codex_native_transcript");
    error["error_kind"] = json!(category(&format!("{} {}",failure["codex_error_info"],detail)));
    if !string(payload,"turn_id").is_empty() { error["turn_id"] = payload["turn_id"].clone(); }
    error["native_turn_complete"] = json!(matches!(string(payload,"type"),"task_complete"|"turn_complete"));
    (Some(error),0.)
}

#[cfg(test)]
pub(super) fn watch_reference(record: &Value, boundary: u64) -> Result<(Vec<Value>,f64)> {
    codex_transcript_snapshot(record,Some(boundary))
}

#[cfg(test)]
thread_local! { static TRANSCRIPT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// codex_transcript() reads up to 8 MiB of each transcript; stopped and
/// archived sessions reuse their last scan (one host re-read 129 MB per listing).
fn codex_transcript_cached(record: &Value) -> Result<(Vec<Value>, f64)> {
    if let Some(value) = super::watch_index::errors(record) { return Ok(value); }
    let path = PathBuf::from(string(record, "transcript"));
    let mut failure = None;
    let value = scan_cache::cached("codex-provider-errors-v3", &path, string(record, "conversation_id"), || {
        codex_transcript(record)
            .map(|(errors, progress)| json!({"errors": errors, "progress": progress}))
            .map_err(|error| failure = Some(error))
            .ok()
    });
    if let Some(error) = failure {
        return Err(error);
    }
    let mut value = value.ok_or("Conversation transcript unavailable")?;
    Ok((value["errors"].as_array_mut().map(std::mem::take).unwrap_or_default(), value["progress"].as_f64().unwrap_or(0.0)))
}

/// Native completion of the current failed turn, independent of stale hooks.
/// Log text and a standalone error event never prove that a turn has ended.
pub(super) fn completed_failure(record: &Value) -> bool {
    if record["agent"] != "codex" { return false; }
    let Ok((errors, progress)) = codex_transcript_cached(record) else { return false; };
    let progress = progress.max(record["turn_started"].as_f64().unwrap_or(0.0))
        .max(record["last_main_progress_at"].as_f64().unwrap_or(0.0));
    errors.last().is_some_and(|error| error["native_turn_complete"] == true
        && !string(error,"turn_id").is_empty()
        && error["at"].as_f64().unwrap_or(0.0) > progress)
}

fn codex(record: &Value) -> (Vec<Value>, f64) {
    let mut errors = codex_logs(record).unwrap_or_default();
    let (native_errors, native_progress) = codex_transcript_cached(record).unwrap_or_default();
    errors.extend(native_errors);
    (correlate_codex_errors(errors, native_progress), native_progress)
}

fn correlate_codex_errors(mut errors: Vec<Value>, progress: f64) -> Vec<Value> {
    // Establish native turn anchors first. Identical failures in successive
    // turns must not share aliases merely because they are close in time.
    errors.sort_by(|a, b| {
        (a["source"] == "codex_native_log").cmp(&(b["source"] == "codex_native_log")).then_with(||
        a["at"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["at"].as_f64().unwrap_or(0.0)))
    });
    let mut unique: Vec<Value> = Vec::new();
    for mut error in errors {
        let distance = |previous: &Value| (previous["at"].as_f64().unwrap_or(0.0) - error["at"].as_f64().unwrap_or(0.0)).abs();
        let matching = unique.iter().enumerate().filter(|(_, previous)| {
            previous["detail"] == error["detail"] && distance(previous) <= 3.0
                && (previous["at"].as_f64().unwrap_or(0.0) > progress) == (error["at"].as_f64().unwrap_or(0.0) > progress)
                && (string(previous, "turn_id").is_empty() || string(&error, "turn_id").is_empty()
                    || previous["turn_id"] == error["turn_id"])
        }).min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b))).map(|(index, _)| index);
        if let Some(index) = matching {
            let previous = unique.remove(index);
            let turn = if !string(&previous, "turn_id").is_empty() { previous["turn_id"].clone() } else { error["turn_id"].clone() };
            let mut ids = previous["failure_ids"].as_array().cloned().unwrap_or_else(|| vec![json!(failure_id(&previous))]);
            ids.push(json!(failure_id(&error))); ids.sort_by(|a, b| a.as_str().cmp(&b.as_str())); ids.dedup();
            // Preserve the latest native timestamp used by GUI freshness guards.
            if previous["at"].as_f64().unwrap_or(0.0) > error["at"].as_f64().unwrap_or(0.0) { error = previous; }
            error["failure_ids"] = json!(ids);
            if !turn.is_null() { error["turn_id"] = turn; }
        }
        unique.push(error);
    }
    unique.sort_by(|a, b| a["at"].as_f64().unwrap_or(0.0).total_cmp(&b["at"].as_f64().unwrap_or(0.0)));
    unique
}

pub(super) fn enrich(record: &Value, output: &mut Value, live: bool) {
    let (errors, native_progress) = if record["agent"] == "codex" {
        codex(record)
    } else {
        (vec![], 0.0)
    };
    let progress = record["last_main_progress_at"].as_f64().unwrap_or_else(|| {
        // Existing sessions predate the dedicated root-progress field. Child
        // hooks and heartbeats must not acknowledge a failed parent turn.
        if errors.is_empty() { return 0.0; }
        use rusqlite::OptionalExtension;
        journal::event_db().ok().and_then(|db|db.query_row(
            "SELECT payload FROM events WHERE name=? AND conversation=?
             AND json_extract(payload,'$.agent_id')='' AND json_extract(payload,'$.type') IN
             ('SessionStart','UserPromptSubmit','UserPromptQueued','TurnStarted','TaskStarted','PreToolUse','PostToolUse','PostToolUseFailure','PermissionRequest','PermissionResult','PreCompact','PostCompact','Stop','Interrupt')
             ORDER BY seq DESC LIMIT 1",
             rusqlite::params![journal::journal_name(record),string(record,"conversation_id")],|r|r.get::<_,String>(0))
             .optional().ok().flatten()).and_then(|s|serde_json::from_str::<Value>(&s).ok())
             .and_then(|event|event["at"].as_f64()).unwrap_or(0.0)
    })
        .max(record["turn_started"].as_f64().unwrap_or(0.0))
        .max(native_progress);
    output["provider_status_at"] = json!(progress
        .max(
            errors
                .last()
                .and_then(|error| error["at"].as_f64())
                .unwrap_or(0.0)
        )
        .max(record["provider_error"]["at"].as_f64().unwrap_or(0.0)));
    let error = errors
        .last()
        .filter(|e| e["at"].as_f64().unwrap_or(0.0) > progress)
        .or_else(|| {
            record["provider_error"]
                .is_object()
                .then_some(&record["provider_error"])
                .filter(|error| error["at"].as_f64().unwrap_or(0.0) > progress)
        });
    if live && output["process_state"] != "exited" {
        if let Some(error) = error {
            apply(output, error);
        }
    }
    output["provider_errors"] = json!(errors);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_turn_needs_native_completion_after_the_latest_progress() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{conversation}.jsonl"));
        let header = json!({"type":"session_meta","payload":{"id":conversation}});
        let mut record = json!({"agent":"codex","conversation_id":conversation,"transcript":path,
            "turn_started":90.0,"last_main_progress_at":90.0});
        let failure = |kind: &str| json!({"type":"event_msg","timestamp":"1970-01-01T00:01:40Z",
            "payload":{"type":kind,"turn_id":"failed-turn","message":"Synthetic request rejected",
                "error":{"message":"Synthetic request rejected"}}});
        std::fs::write(&path,format!("{header}\n{}\n",failure("error"))).unwrap();
        assert!(!completed_failure(&record),"a standalone native error is not completion");
        std::fs::write(&path,format!("{header}\n{}\n",failure("task_complete"))).unwrap();
        assert!(completed_failure(&record));
        record["last_main_progress_at"] = json!(101.0);
        assert!(!completed_failure(&record));
        record["last_main_progress_at"] = json!(90.0);
        let started = json!({"type":"event_msg","timestamp":"1970-01-01T00:01:41Z",
            "payload":{"type":"task_started","turn_id":"next-turn"}});
        std::fs::write(&path,format!("{header}\n{}\n{started}\n",failure("task_complete"))).unwrap();
        assert!(!completed_failure(&record),"a later native start revokes the old completion");
        record["conversation_id"] = json!(uuid::Uuid::new_v4().to_string());
        assert!(!completed_failure(&record),"another conversation cannot borrow completion");
    }
    const KIMI_WINDOW: &str = "429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends. To continue now, purchase extra usage or upgrade your plan: https://example.invalid/subscription?tab=quota";

    #[test]
    fn typed_kimi_window_keeps_its_native_message_and_failure_identity() {
        let detail = KIMI_WINDOW.strip_prefix("429 ").unwrap();
        let mut error = event("native-window", 123.0, detail, "provider_hook");
        assert_eq!(error["error_kind"], "quota");
        assert!(!rolling_rate_error(&error), "the message alone is not native status evidence");
        error["native_error_type"] = json!(KIMI_RATE_LIMIT_TYPE);
        error["native_error_code"] = json!("");
        assert!(rolling_rate_error(&error));
        let mut output = json!({"run_id":"original-run","conversation_id":"original-conversation"});
        apply(&mut output, &error);
        assert_eq!(output["provider_error"]["error_kind"], "rate_limit");
        assert_eq!(output["provider_error"]["detail"], detail);
        assert_eq!(output["provider_error"]["native_error_type"], KIMI_RATE_LIMIT_TYPE);
        assert_eq!(failure_id(&output["provider_error"]), failure_id(&error));
        assert_eq!(output["run_id"], "original-run");
        assert_eq!(output["conversation_id"], "original-conversation");
        assert!(output["provider_error"].get("reset_at").is_none());
        assert!(output["provider_error"].get("retry_not_before").is_none());
        for kind in ["authentication", "context_limit", "provider_policy"] {
            error["error_kind"] = json!(kind);
            apply(&mut output, &error);
            assert_eq!(output["provider_error"]["error_kind"], kind);
        }
    }

    #[test]
    fn typed_window_never_promotes_missing_evidence_or_permanent_errors() {
        let detail = KIMI_WINDOW.strip_prefix("429 ").unwrap();
        let valid = json!({"source":"provider_hook","detail":detail,"native_error_type":KIMI_RATE_LIMIT_TYPE});
        for (key, value) in [("source", "provider_log"), ("native_error_type", ""),
            ("native_error_type", "OtherRateLimitError"), ("native_error_code", "quota"),
            ("native_error_code", "authentication"), ("native_error_code", "provider_policy"),
            ("native_error_code", "model_unavailable"), ("native_error_code", "unknown") ] {
            let mut error = valid.clone(); error[key] = json!(value);
            assert!(!rolling_rate_error(&error), "{error}");
        }
        for value in [Value::Null, json!(429), json!({}), json!([])] {
            let mut error = valid.clone(); error["native_error_code"] = value;
            assert!(!rolling_rate_error(&error), "{error}");
        }
        for message in ["You've reached your 5-hour usage limit.".to_owned(),
            detail.replace("5-hour", "weekly"), format!("{detail} HTTP 401"), format!("{detail} HTTP 403"),
            format!("{detail} monthly quota exhausted"), format!("{detail} insufficient credit"),
            format!("{detail} content_filter"), format!("{detail} invalid_request_error")] {
            let mut error = valid.clone(); error["detail"] = json!(message);
            assert!(!rolling_rate_error(&error), "{error}");
        }
    }

    #[test]
    fn kimi_rolling_window_is_a_rate_limit_despite_quota_and_upgrade_wording() {
        for detail in [KIMI_WINDOW.to_owned(), KIMI_WINDOW.replace("You've", "You’ve"),
            format!("[provider.api_error] {KIMI_WINDOW}"), KIMI_WINDOW.to_uppercase()] {
            assert_eq!(category(&detail), "rate_limit", "{detail}");
            let error = event("native-window", 123.0, &detail, "provider_hook");
            assert_eq!(error["error_kind"], "rate_limit");
            assert!(error.get("reset_at").is_none());
        }
    }

    #[test]
    fn rolling_window_normalizes_saved_quota_without_rewriting_native_identity() {
        let mut error = event("native-window", 123.0, KIMI_WINDOW, "provider_hook");
        error["error_kind"] = json!("quota");
        let mut output = json!({"run_id":"run","conversation_id":"conversation"});
        apply(&mut output, &error);
        assert_eq!(output["provider_error"]["error_kind"], "rate_limit");
        assert_eq!(output["activity_summary"], "Rate limit reached");
        assert_eq!(output["last_error"], KIMI_WINDOW);
        assert_eq!(failure_id(&output["provider_error"]), failure_id(&error));
        assert_eq!(output["run_id"], "run");
        assert_eq!(output["conversation_id"], "conversation");
        for kind in ["authentication", "provider_policy", "context_limit"] {
            error["error_kind"] = json!(kind);
            apply(&mut output, &error);
            assert_eq!(output["provider_error"]["error_kind"], kind);
        }
    }

    #[test]
    fn other_quota_and_explicit_policy_failures_remain_permanent() {
        for detail in ["429 quota exhausted", "429 You've reached your 5-hour usage limit.",
            "Your quota will reset when the current 5-hour window ends.",
            "429 You've reached your weekly usage limit. Your quota will reset next week."] {
            assert_eq!(category(detail), "quota", "{detail}");
        }
        assert_eq!(category(&format!("1{KIMI_WINDOW}")), "quota");
        for prefix in ["monthly quota exhausted", "billing cycle exhausted", "insufficient credit", "HTTP 401 authentication failed"] {
            assert_ne!(category(&format!("{prefix}: {KIMI_WINDOW}")), "rate_limit");
        }
        assert_eq!(category(&format!("content_filter: {KIMI_WINDOW}")), "provider_policy");
        for kind in ["auth_error", "context_limit", "billing", "provider_policy", "provider_policy_error",
            "session_limit", "session_limit_error", "model_unavailable", "model_unavailable_error", "invalid_request", "invalid_request_error"] {
            let detail = format!("{kind} {KIMI_WINDOW}");
            assert_ne!(category(&detail), "rate_limit");
            let mut error = event("native-window", 123.0, KIMI_WINDOW, "provider_hook");
            error["native_error_type"] = json!(kind);
            error["error_kind"] = json!(category(&detail));
            let mut output = json!({});
            apply(&mut output, &error);
            assert_ne!(output["provider_error"]["error_kind"], "rate_limit");
        }
    }

    #[test]
    fn upgraded_scan_does_not_reuse_cached_failures_without_native_turn_ids() {
        let temp = tempfile::tempdir().unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{conversation}.jsonl"));
        let header = json!({"type":"session_meta","payload":{"id":conversation}});
        let failure = json!({"type":"event_msg","timestamp":"2026-10-12T12:00:00.000Z",
            "payload":{"type":"task_complete","turn_id":"current-turn","error":{"message":"Selected model is at capacity"}}});
        std::fs::write(&path, format!("{header}\n{failure}\n")).unwrap();
        let legacy = json!({"errors":[event("legacy-entry", 100.0, "Selected model is at capacity", "codex_native_transcript")],"progress":0.0});
        assert!(scan_cache::cached("codex-provider-errors", &path, &conversation, ||Some(legacy)).is_some());
        let (errors, _) = codex_transcript_cached(&json!({"transcript":path,"conversation_id":conversation})).unwrap();
        assert_eq!(errors[0]["turn_id"], "current-turn");
    }
    #[test]
    fn codex_log_and_completion_keep_both_exact_failure_references() {
        let mut completion = event("completed-turn", 100.0, "Selected model is at capacity", "codex_native_transcript");
        completion["turn_id"] = json!("turn-one");
        let log = event("log-one", 100.000328, "Selected model is at capacity", "codex_native_log");
        let ids = [json!(failure_id(&completion)), json!(failure_id(&log))];
        let errors = correlate_codex_errors(vec![log, completion], 90.0);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["turn_id"], "turn-one");
        assert_eq!(errors[0]["at"], 100.000328);
        let aliases = errors[0]["failure_ids"].as_array().unwrap();
        assert!(ids.iter().all(|id| aliases.contains(id)));
    }
    #[test]
    fn repeated_capacity_errors_in_distinct_turns_keep_separate_references() {
        let native = |turn: &str, at| {
            let mut e = event(turn, at, "Selected model is at capacity", "codex_native_transcript");
            e["turn_id"] = json!(turn); e
        };
        let log1 = event("log-one", 100.000328, "Selected model is at capacity", "codex_native_log");
        let log2 = event("log-two", 100.200328, "Selected model is at capacity", "codex_native_log");
        let errors = correlate_codex_errors(vec![log2.clone(), native("turn-two", 100.2), log1.clone(), native("turn-one", 100.0)], 90.0);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0]["turn_id"], "turn-one");
        assert_eq!(errors[1]["turn_id"], "turn-two");
        assert!(errors[0]["failure_ids"].as_array().unwrap().contains(&json!(failure_id(&log1))));
        assert!(!errors[0]["failure_ids"].as_array().unwrap().contains(&json!(failure_id(&log2))));
        assert!(!errors[1]["failure_ids"].as_array().unwrap().contains(&json!(failure_id(&log1))));
        // The old completion can be outside the bounded transcript tail. Its
        // remaining log still cannot become evidence for the new failed turn.
        let errors = correlate_codex_errors(vec![log1.clone(), log2, native("turn-two", 100.2)], 100.1);
        assert_eq!(errors.len(), 2);
        assert!(!errors[1]["failure_ids"].as_array().unwrap().contains(&json!(failure_id(&log1))));
    }
    #[test]
    fn native_errors_are_conversation_scoped_and_clear_on_main_progress() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let db = Connection::open(temp.path().join("logs_2.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT);").unwrap();
        for (n,thread,target,body) in [(1,id.as_str(),"codex_core::session::turn","run_turn: Turn error: Selected model is at capacity. Please try a different model."),
            (2,"other","codex_core::session::turn","run_turn: Turn error: quota exhausted"),
            (3,id.as_str(),"codex_core::tools","run_turn: Turn error: tool output text"),
            (4,id.as_str(),"codex_core::session::turn","A regular status message")] {
            db.execute("INSERT INTO logs VALUES(?,100,500000000,?,?,?)",rusqlite::params![n,thread,target,body]).unwrap();
        }
        let mut record = json!({"agent":"codex","agent_home":temp.path(),"conversation_id":id,"last_event_at":150,"last_main_progress_at":90});
        let mut output = json!({"process_state":"running","phase":"tool"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "error");
        assert_eq!(output["provider_error"]["error_kind"], "capacity");
        assert_eq!(output["provider_errors"].as_array().unwrap().len(), 1);
        assert_eq!(output["attention_id"], "codex_native_log:codex-error-1");
        record["last_main_progress_at"] = json!(101);
        output = json!({"phase":"working"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "working");
        assert_eq!(output["provider_errors"].as_array().unwrap().len(), 1);
        record["last_main_progress_at"] = json!(90);
        output = json!({"phase":"idle"});
        enrich(&record, &mut output, false);
        assert_eq!(output["phase"], "idle");
        record["conversation_id"] = json!(uuid::Uuid::new_v4().to_string());
        assert!(codex(&record).0.is_empty());
    }
    #[test]
    fn provider_failure_kinds_keep_generic_failures_visible() {
        for (message, kind) in [
            ("quota exhausted", "quota"),
            (
                "429 usage limit reached; model capacity unavailable",
                "quota",
            ),
            ("7d limit reached. Resets in 4d18h", "quota"),
            ("429 Too Many Requests", "rate_limit"),
            ("authentication_failed", "authentication"),
            ("Auth error: expired credentials", "authentication"),
            ("HTTP 401", "authentication"),
            ("Invalid API key", "authentication"),
            ("An authorized request lost its connection", "provider"),
            ("Provider returned event_id=40123", "provider"),
            (
                "Your request was flagged as potentially violating our usage policy. This is authorized security testing.",
                "provider_policy",
            ),
            (
                "Request flagged for high risk cybersecurity activity in an authorized audit",
                "provider_policy",
            ),
            ("policy_violation (HTTP 503): authorized work", "provider_policy"),
            ("content_filter: quota and HTTP 429", "provider_policy"),
            ("cyber_policy", "provider_policy"),
            ("overloaded", "capacity"),
            ("max_output_tokens", "context_limit"),
            ("model_not_found", "model_unavailable"),
            ("Connection lost", "provider"),
        ] {
            assert_eq!(category(message), kind);
        }
    }

    #[test]
    fn policy_failure_preserves_native_detail_and_attention_identity() {
        let detail = "Your request was flagged as potentially violating our usage policy. Authorized security testing.";
        let error = event("native-failure", 123.0, detail, "codex_native_log");
        let mut output = json!({"run_id":"synthetic-run","conversation_id":"synthetic-conversation"});
        apply(&mut output, &error);
        assert_eq!(output["provider_error"]["error_kind"], "provider_policy");
        assert_eq!(output["activity_summary"], "Request blocked by provider");
        assert_eq!(output["last_error"], detail);
        assert_eq!(output["activity_detail"], detail);
        assert_eq!(output["attention_id"], "codex_native_log:native-failure");
        assert_eq!(output["run_id"], "synthetic-run");
        assert_eq!(output["conversation_id"], "synthetic-conversation");
    }

    #[test]
    fn compaction_failures_use_exact_native_emitters_and_messages() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let db = Connection::open(temp.path().join("logs_2.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE logs(id INTEGER, ts INTEGER, ts_nanos INTEGER, thread_id TEXT, target TEXT, feedback_log_body TEXT);").unwrap();
        for (n,target,body) in [
            (1,"codex_core::tasks","session_loop: session task returned an unexpected error err=You’ve hit your usage limit."),
            (2,"codex_core::session::turn","run_turn: Post-turn compaction failed; preserving the completed turn error=You’ve hit your usage limit."),
            (3,"codex_core::tools","run_turn: Turn error: quota exhausted"),
            (4,"codex_core::session::turn","update_rate_limits: usage limit reached"),
            (5,"codex_core::session::turn","test fixture containing Turn error: quota exhausted"),
            (6,"codex_core::session::turn","run_turn: Post-turn compaction failed; preserving the completed turn error=Model at capacity"),
        ] {
            db.execute("INSERT INTO logs VALUES(?,100,0,?,?,?)",rusqlite::params![n,id,target,body]).unwrap();
        }
        let record = json!({"agent":"codex","agent_home":temp.path(),"conversation_id":id,"last_main_progress_at":90});
        assert_eq!(codex(&record).0.len(), 1); // Duplicate native details merge.
        let mut output = json!({"phase":"compacting","activity":"busy","process_state":"running"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "error");
        assert_eq!(output["provider_error"]["error_kind"], "quota");
        assert_eq!(output["activity_summary"], "Usage limit reached");
    }

    #[test]
    fn native_failed_completion_survives_missing_logs_then_clears_on_new_progress() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        let at = search::timestamp(&json!("2026-10-09T08:00:00Z"));
        let rows = [
            json!({"type":"session_meta","payload":{"id":id}}),
            json!({"type":"event_msg","timestamp":"2026-10-09T08:00:00Z","payload":{"type":"task_complete","turn_id":"failed-turn","error":{"message":"Provider refused the request","codex_error_info":"usage_limit_exceeded"}}}),
            json!({"type":"event_msg","timestamp":"2026-10-09T08:00:01Z","payload":{"type":"agent_message","message":"Example: usage limit reached"}}),
            json!({"type":"response_item","timestamp":"2026-10-09T08:00:02Z","payload":{"type":"function_call_output","error":{"message":"tool output quota exhausted"}}}),
            json!({"type":"event_msg","timestamp":"2026-10-09T08:00:03Z","payload":{"type":"task_complete","turn_id":"failed-turn","error":null}}),
        ];
        std::fs::write(&path,rows.iter().map(|row|row.to_string()+"\n").collect::<String>()
            + "{\"type\":\"event_msg\",\"payload\":{\"type\":\"error\",\"message\":\"incomplete").unwrap();
        let mut record = json!({"agent":"codex","agent_home":temp.path(),"conversation_id":id,"transcript":path,"last_main_progress_at":at-0.1});
        let mut output = json!({"process_state":"running","activity":"busy","phase":"tool"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "error");
        assert_eq!(output["activity"], "attention");
        assert_eq!(output["provider_error"]["error_kind"], "quota");
        assert_eq!(output["provider_errors"].as_array().unwrap().len(), 1);
        record["last_main_progress_at"] = json!(at + 10.0);
        output = json!({"phase":"working","activity":"busy"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "working");
        assert!(output["provider_error"].is_null());
        record["last_main_progress_at"] = json!(at - 1.0);
        std::fs::write(&path,rows.iter().map(|row|row.to_string()+"\n").collect::<String>()
            + &json!({"type":"event_msg","timestamp":"2026-10-09T08:01:00Z","payload":{"type":"task_started","turn_id":"new-turn"}}).to_string()+"\n").unwrap();
        output = json!({"phase":"working","activity":"busy"});
        enrich(&record, &mut output, true);
        assert_eq!(output["phase"], "working"); // Native progress survives missing hooks.
                                                // Even a correctly named file cannot supply another thread's state.
        std::fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":uuid::Uuid::new_v4().to_string()}})
                .to_string()
                + "\n",
        )
        .unwrap();
        assert!(codex_transcript(&record).is_err());
    }
    #[test]
    fn unchanged_transcripts_reuse_their_cached_scan() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        scan_cache::DIR.with(|dir| *dir.borrow_mut() = Some(cache.clone()));
        let id = uuid::Uuid::new_v4().to_string();
        let path = temp.path().join(format!("rollout-{id}.jsonl"));
        let failed = |turn: &str, second: u32| json!({"type":"event_msg","timestamp":format!("2026-10-09T08:00:{second:02}Z"),
            "payload":{"type":"task_complete","turn_id":turn,"error":{"message":format!("Provider failed {turn}")}}}).to_string() + "\n";
        let meta = json!({"type":"session_meta","payload":{"id":id}}).to_string() + "\n";
        std::fs::write(&path, meta.clone() + &failed("first", 1)).unwrap();
        let record = json!({"agent":"codex","agent_home":temp.path(),"conversation_id":id,"transcript":path});
        let scans = || TRANSCRIPT_SCANS.with(|count| count.get());
        let before = scans();
        let first = codex(&record);
        assert_eq!(first.0.len(), 1);
        // Archived and stopped sessions keep their transcripts: later listings reuse the scan.
        for _ in 0..3 {
            assert_eq!(codex(&record), first);
        }
        assert_eq!(scans() - before, 1);
        // An appended failure changes the file and is scanned again.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, failed("second", 2).as_bytes()).unwrap();
        drop(file);
        let second = codex(&record);
        assert_eq!(second.0.len(), 2);
        assert_eq!(scans() - before, 2);
        assert_eq!(codex(&record), second);
        assert_eq!(scans() - before, 2);
        // A replaced file is a different file, whatever its timestamps.
        let replacement = temp.path().join("replacement");
        std::fs::write(&replacement, meta.clone() + &failed("third", 3) + &failed("forth", 4)).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(codex(&record).0[1]["detail"], "Provider failed forth");
        assert_eq!(scans() - before, 3);
        // An unreadable cache entry only costs a scan.
        for entry in std::fs::read_dir(&cache).unwrap() {
            std::fs::write(entry.unwrap().path(), "{not json").unwrap();
        }
        assert_eq!(codex(&record).0.len(), 2);
        assert_eq!(scans() - before, 4);
        // Another conversation's identity is never served from the cache.
        let other = json!({"agent":"codex","agent_home":temp.path(),"conversation_id":uuid::Uuid::new_v4().to_string(),"transcript":path});
        assert!(codex(&other).0.is_empty());
        scan_cache::DIR.with(|dir| *dir.borrow_mut() = None);
    }
}
