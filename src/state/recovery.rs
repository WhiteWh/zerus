//! Host-owned recovery episodes with a replicated, versioned policy.
//! A terminal continuation is explicitly different from retrying an API request.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) const CONTINUATION: &str = "[HGS automatic recovery] The previous turn stopped with a temporary provider error. Continue the current task from the existing conversation. Check any uncertain operation before repeating it.";
const CODEX_ACKNOWLEDGEMENT_WINDOW: f64 = 600.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    version: u32,
    revision: u64,
    enabled: bool,
    enabled_at: f64,
    service: bool,
    rate_limit: bool,
    session_limit: bool,
    session_limit_mode: String,
    network: bool,
    delays: Vec<i64>, // Legacy v1 default; per-class schedules take precedence.
    schedules: std::collections::BTreeMap<String, Vec<i64>>,
    writer: String,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: 3,
            revision: 0,
            enabled: false,
            enabled_at: 0.0,
            service: true,
            rate_limit: true,
            session_limit: true,
            session_limit_mode: "reset".into(),
            network: true,
            delays: vec![15, 30, 60, 300],
            schedules: Default::default(),
            writer: String::new(),
        }
    }
}
pub(super) fn policy_path() -> PathBuf {
    root().join("recovery/policy.json")
}
/// H1: sessions a team asked the recovery service to resume after a limit.
fn team_path() -> PathBuf {
    root().join("recovery/team.json")
}
/// A team refreshes its request; an old one from a stopped worker expires.
const TEAM_OVERRIDE_SECONDS: f64 = 900.0;
fn team_overrides() -> std::collections::BTreeMap<String, Value> {
    let Ok(value) = read_json(&team_path()) else { return Default::default() };
    value["items"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, item)| now() - item["at"].as_f64().unwrap_or(0.0) < TEAM_OVERRIDE_SECONDS)
        .map(|(name, item)| (name.clone(), item.clone()))
        .collect()
}
fn override_for(overrides: &std::collections::BTreeMap<String, Value>, snapshot: &Value) -> bool {
    overrides
        .get(string(snapshot, "name"))
        .is_some_and(|item| item["run_id"].as_str().unwrap_or_default() == string(snapshot, "run_id"))
}
/// H1: the team dispatcher names a member's session whose reset it wants the
/// recovery service to wait for; `on: false` stops that. The request is
/// refreshed at most once a minute and expires when the dispatcher stops.
pub(super) fn set_team_override(name: &str, run_id: &str, on: bool) -> Result<()> {
    let mut items: serde_json::Map<String, Value> =
        read_json(&team_path()).ok().and_then(|value| value["items"].as_object().cloned()).unwrap_or_default();
    if on {
        let stale = items.get(name).is_none_or(|item| {
            item["run_id"].as_str().unwrap_or_default() != run_id || now() - item["at"].as_f64().unwrap_or(0.0) >= 60.0
        });
        if stale {
            items.insert(name.to_owned(), json!({"run_id":run_id,"at":now()}));
        }
        while items.len() > 200 {
            let Some(oldest) = items
                .iter()
                .min_by(|a, b| a.1["at"].as_f64().unwrap_or(0.0).total_cmp(&b.1["at"].as_f64().unwrap_or(0.0)))
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            items.remove(&oldest);
        }
    } else {
        items.remove(name);
    }
    atomic(&team_path(), &json!({"items":items}).to_string())?;
    Ok(())
}
/// H1: the shared policy with a team's reset request applied: only the
/// session-limit class, waiting for the reset, even when the shared policy is
/// off.
fn effective_policy(p: &Policy, requested: bool) -> Policy {
    if !requested {
        return p.clone();
    }
    let mut q = p.clone();
    q.enabled = true;
    q.session_limit = true;
    q.session_limit_mode = "reset".into();
    if !p.enabled {
        q.service = false;
        q.rate_limit = false;
        q.network = false;
        // An old enabled_at from a previous policy must not hide this episode.
        q.enabled_at = 0.0;
    }
    q
}
fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn policy() -> Result<Policy> {
    if !policy_path().exists() {
        return Ok(Policy::default());
    }
    let p: Policy =
        serde_json::from_value(read_json(&policy_path())?).map_err(|e| e.to_string())?;
    validate_policy(&p)?;
    Ok(p)
}
fn validate_delays(delays: &[i64]) -> Result<()> {
    if delays.is_empty() || delays.len() > 16 {
        return Err("Use 1–16 delays".into());
    }
    for (index, delay) in delays.iter().enumerate() {
        if (1..=86400).contains(delay) {
            continue;
        }
        if index + 1 == delays.len() && (*delay == -1 || (*delay == 0 && index > 0)) {
            continue;
        }
        return Err(
            "Use 1–86400 seconds; end with -1 to stop or 0 to repeat the previous delay forever"
                .into(),
        );
    }
    Ok(())
}
fn validate_policy(p: &Policy) -> Result<()> {
    if ![1, 2, 3].contains(&p.version) || p.writer.len() > 128 || p.revision >= 9_000_000_000_000_000 {
        return Err("Unsupported recovery policy version or revision".into());
    }
    validate_delays(&p.delays)?;
    if !["reset", "interval"].contains(&p.session_limit_mode.as_str()) {
        return Err("Use reset or interval for session-limit recovery".into());
    }
    for (class, delays) in &p.schedules {
        if !["service", "rate_limit", "session_limit", "network"].contains(&class.as_str()) {
            return Err("Unknown recovery class".into());
        }
        validate_delays(delays)?;
    }
    Ok(())
}
fn policy_order(p: &Policy) -> (u64, String) {
    // Revision is a Lamport clock. Stable content ordering also reconciles v1
    // policies created independently before shared settings existed.
    let writer = if p.writer.is_empty() {
        format!(
            "legacy:{:?}:{}:{}:{}:{}:{:?}{}",
            p.delays, p.enabled, p.service, p.rate_limit, p.network, p.schedules,
            if p.version >= 3 { format!(":{}:{}", p.session_limit, p.session_limit_mode) } else { String::new() }
        )
    } else {
        p.writer.clone()
    };
    (p.revision, writer)
}
fn delays_for<'a>(p: &'a Policy, class: &str) -> &'a [i64] {
    p.schedules.get(class).map(Vec::as_slice).unwrap_or_else(|| {
        if class == "session_limit" { &[300, 0] } else { &p.delays }
    })
}
fn next_delay(delays: &[Value], attempt: usize) -> Option<f64> {
    match delays.get(attempt).and_then(Value::as_i64) {
        Some(value) if value > 0 => Some(value as f64),
        Some(-1) => None,
        _ if delays.last().and_then(Value::as_i64) == Some(0) => delays
            .get(delays.len().checked_sub(2)?)
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .map(|v| v as f64),
        _ => None,
    }
}
/// A job file is keyed by its session's owner key (`storage::owner_key`),
/// taken from the verified record or snapshot that the caller holds, never
/// from a label or the command's selection (#1437 review). A worker keeps the
/// key it observed, so a rename or a vanished record never moves its job to
/// the label's other holder.
fn job_path(owner: &str) -> PathBuf {
    root().join("recovery/jobs").join(format!("{owner}.json"))
}
fn load_job(owner: &str) -> Value {
    read_json(&job_path(owner)).unwrap_or(Value::Null)
}
fn owner(record: &Value) -> String {
    storage::owner_key(record)
}
/// A legacy rename moves the job from the old label's key to the new one's.
pub(super) fn rename_job(old: &str, new: &str, run_id: &str) -> Result<()> {
    let (from, to) = (storage::label_key(old), storage::label_key(new));
    let Some(mut job) = renamed_job(&from, run_id) else {
        return Ok(());
    };
    job["name"] = json!(new);
    // Called inside the existing rename transaction's lock; replay is harmless.
    atomic(&job_path(&to), &job.to_string())?;
    fs::remove_file(job_path(&from)).map_err(|e| e.to_string())
}
/// A rename in place keeps the session's key; its job takes the new label.
pub(super) fn rename_job_in_place(record: &Value, new: &str) -> Result<()> {
    let key = owner(record);
    let Some(mut job) = renamed_job(&key, string(record, "run_id")) else {
        return Ok(());
    };
    job["name"] = json!(new);
    atomic(&job_path(&key), &job.to_string())
}
/// `owner`'s job for `run_id`, stopped as uncertain when a rename caught it
/// during delivery; None when it has none for that run.
fn renamed_job(owner: &str, run_id: &str) -> Option<Value> {
    renamed_job_value(load_job(owner), run_id)
}

fn renamed_job_value(mut job: Value, run_id: &str) -> Option<Value> {
    if !job.is_object() || job["identity"][0] != run_id {
        return None;
    }
    if job["state"] == "dispatching" {
        stop(
            &mut job,
            "uncertain",
            "Session renamed during delivery; inspect the agent",
        );
    }
    Some(job)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_job_guarded(
    view: &mut guarded_recovery::View<'_>,
    old: &str,
    new: &str,
    run_id: &str,
) -> Result<()> {
    let (from, to) = (storage::label_key(old), storage::label_key(new));
    let job = view
        .read(&job_path(&from), storage::ID_RECORD_BYTES)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let Some(mut job) = renamed_job_value(job, run_id) else {
        return Ok(());
    };
    job["name"] = json!(new);
    view.atomic(&job_path(&to), job.to_string().as_bytes())?;
    view.remove(&job_path(&from))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_job_in_place_guarded(
    view: &mut guarded_recovery::View<'_>,
    record: &Value,
    new: &str,
) -> Result<()> {
    let key = owner(record);
    let job = view
        .read(&job_path(&key), storage::ID_RECORD_BYTES)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let Some(mut job) = renamed_job_value(job, string(record, "run_id")) else {
        return Ok(());
    };
    job["name"] = json!(new);
    view.atomic(&job_path(&key), job.to_string().as_bytes())
}

fn save_job(job: &Value, owner: &str) -> Result<()> {
    let previous = load_job(owner);
    let mut job = job.clone();
    let mut history = if previous["id"] == job["id"] {
        previous["history"].as_array().cloned().unwrap_or_default()
    } else {
        vec![]
    };
    if previous["state"] != job["state"] || previous["attempt"] != job["attempt"] {
        history.push(json!({"at":now(),"state":job["state"],"attempt":job["attempt"],"reason":job["reason"]}));
        if history.len() > 32 {
            history.remove(0);
        }
    }
    job["history"] = json!(history);
    atomic(&job_path(owner), &job.to_string())?;
    crate::analytics::adapters::recovery(&root(), &job);
    Ok(())
}
fn seconds(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(0.0)
}
fn failure_id(s: &Value) -> String {
    provider_errors::failure_id(&s["provider_error"])
}
fn same_failure(job: &Value, snapshot: &Value) -> bool {
    job["failure_id"] == failure_id(snapshot)
        || (snapshot["agent"] == "codex"
            && ["codex_native_log", "codex_native_transcript"].contains(&string(&snapshot["provider_error"], "source"))
            && (snapshot["provider_error"]["failure_ids"].as_array().is_some_and(|ids| ids.contains(&job["failure_id"]))
                || job["failure_ids"].as_array().is_some_and(|ids|
                    ids.contains(&json!(failure_id(snapshot))) || snapshot["provider_error"]["failure_ids"].as_array()
                        .is_some_and(|current| ids.iter().any(|id| current.contains(id))))))
}
fn remember_failure_copies(job: &mut Value, snapshot: &Value) {
    if !confirmed_codex_copies(snapshot) { return; }
    let mut ids = vec![job["failure_id"].clone(), json!(failure_id(snapshot))];
    for values in [&snapshot["provider_error"]["failure_ids"], &job["failure_ids"]] {
        if let Some(values) = values.as_array() {
            for id in values { if !ids.contains(id) && ids.len() < 32 { ids.push(id.clone()); } }
        }
    }
    ids.sort_by(|a, b| a.as_str().cmp(&b.as_str())); ids.dedup();
    job["failure_ids"] = json!(ids);
}
fn confirmed_codex_copies(snapshot: &Value) -> bool {
    let Some(ids) = snapshot["provider_error"]["failure_ids"].as_array() else { return false; };
    snapshot["agent"] == "codex" && ["codex_native_log:", "codex_native_transcript:"].iter()
        .all(|prefix| ids.iter().any(|id| id.as_str().is_some_and(|id| id.starts_with(prefix))))
}
fn identity(s: &Value) -> Value {
    json!([
        s["run_id"],
        s["conversation_id"],
        s["model"],
        s["account_id"],
        s["host_generation"]
    ])
}
// Register new provider recovery capabilities here, never infer them from a
// composer's generic ability to send text or from arbitrary tool output.
fn adapter(s: &Value) -> Option<&'static str> {
    match string(s, "agent") {
        "claude" | "kimi" | "codex" => Some("continue_message"),
        "dsh" if s["native_recovery"].is_object() => Some("retry_request"),
        "dsh" => Some("continue_message"),
        _ => None,
    }
}
fn contains(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}
fn status_code(text: &str, codes: &[&str]) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| codes.contains(&word))
}
fn class(error: &Value) -> &'static str {
    if let Some(kind) = provider_errors::native_kind(error) {
        return if kind == "session_limit" { "session_limit" } else { "manual" };
    }
    if string(error, "error_kind") == "provider_policy"
        || provider_errors::category(string(error, "detail")) == "provider_policy"
    {
        return "manual";
    }
    let native_detail = provider_errors::native_detail(error);
    if ["", "provider", "quota", "rate_limit"].contains(&string(error, "error_kind"))
        && provider_errors::rolling_rate_error(error) {
        return "rate_limit";
    }
    let text = format!(
        "{} {}",
        string(error, "error_kind"),
        native_detail
    )
    .to_lowercase();
    // Permanent failures always win, including a quota error containing 429.
    if contains(
        &text,
        &[
            "quota",
            "billing",
            "credit",
            "balance",
            "usage limit",
            "usage_limit",
            "insufficient",
            "account_on_hold",
            "authentication",
            "unauthorized",
            "api key",
            "api_key",
            "credential",
            "401",
            "403",
            "context",
            "max_output_tokens",
            "model_not_found",
            "model not found",
            "not supported",
            "invalid request",
        ],
    ) {
        return "manual";
    }
    if session_limits::recognized(&text) {
        return "session_limit";
    }
    if contains(
        &text,
        &[
            "capacity",
            "overload",
            "529",
            "503",
            "502",
            "500",
            "504",
            "internal_server_error",
            "internal server error",
            "service unavailable",
        ],
    ) || status_code(&text, &["520", "521", "522", "523", "524"])
    {
        // Cloudflare reports an unreachable or timed-out provider as 520–524, often without a body.
        return "service";
    }
    if contains(
        &text,
        &["rate_limit", "rate limit", "too many requests", "429"],
    ) {
        return "rate_limit";
    }
    if contains(
        &text,
        &[
            "connection reset",
            "connection refused",
            "connection lost",
            "connection error",
            "network error",
            "network_error",
            "timed out",
            "timeout",
            "econnreset",
            "econnrefused",
            "etimedout",
            "stream disconnected",
            // DCB-svn report (2026-11-12): DeepSeek's own words for a request
            // that its retries could not get through (code TRANSPORT).
            "transport failed",
            "transport error",
            "fetch failed",
            "socket hang up",
        ],
    ) {
        return "network";
    }
    "manual"
}
fn enabled(p: &Policy, class: &str) -> bool {
    p.enabled
        && match class {
            "service" => p.service,
            "rate_limit" => p.rate_limit,
            "session_limit" => p.session_limit,
            "network" => p.network,
            _ => false,
        }
}
fn stop(job: &mut Value, state: &str, reason: &str) {
    job["state"] = json!(state);
    job["reason"] = json!(reason);
    job["updated_at"] = json!(now());
}
/// New input to the session `record` cancels its automatic recovery.
pub(super) fn cancel_for_input(record: &Value) -> Result<()> {
    let key = owner(record);
    let mut job = load_job(&key);
    if active(&job) {
        stop(
            &mut job,
            "cancelled",
            "New input cancelled automatic recovery",
        );
        save_job(&job, &key)?;
    }
    Ok(())
}

/// Read-only cancellation description for the original Send owner. The
/// ordinary save_job wrapper still owns every existing caller. Cancellation
/// emits no recovery analytics event (the adapter ignores cancelled state).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn input_cancellation(record: &Value) -> Result<Option<(guarded_recovery::Original,Value)>> {
    let path=job_path(&owner(record));
    let Some(source)=guarded_recovery::Original::read_existing(&path,storage::ID_RECORD_BYTES)? else {return Ok(None)};
    let mut job=serde_json::from_slice::<Value>(source.bytes()).unwrap_or(Value::Null);
    if !active(&job) {return Ok(None);}
    let previous=job.clone();
    stop(&mut job,"cancelled","New input cancelled automatic recovery");
    let mut history=previous["history"].as_array().cloned().unwrap_or_default();
    if previous["state"]!=job["state"] || previous["attempt"]!=job["attempt"] {
        history.push(json!({"at":now(),"state":job["state"],"attempt":job["attempt"],"reason":job["reason"]}));
        if history.len()>32 {history.remove(0);}
    }
    job["history"]=json!(history);
    Ok(Some((source,job)))
}
fn active(job: &Value) -> bool {
    ["waiting", "dispatching", "retrying"].contains(&string(job, "state"))
}
/// Whether an automatic recovery episode owns this session's next input.
pub(super) fn job_active(record: &Value) -> bool {
    active(&load_job(&owner(record)))
}
/// The agent works again after a provider failure while the continuation it
/// accepted is still its latest input: that turn went on rather than stopped.
fn working_on_continuation(job: &Value, s: &Value) -> bool {
    job["acknowledged"] == true
        && job["attempt"].as_u64().unwrap_or(0) > 0
        && ["working", "tool", "compacting"].contains(&string(s, "phase"))
}
/// The agent accepted the continuation and the provider served it since.
/// Turn-start markers that follow the accepted input (Kimi's TurnStarted) are
/// not provider progress.
fn resumed(job: &Value, s: &Value) -> bool {
    job["acknowledged"] == true
        && s["phase"] != "error"
        && seconds(s, "provider_status_at") > seconds(job, "user_at").max(seconds(s, "turn_started"))
}
/// An account has one recovery request in flight at a time, until the agent
/// accepts its continuation and then makes progress: from there the provider
/// serves the account again, and a long working turn must not hold the
/// account's other sessions at "Waiting to retry".
fn occupies(job: &Value, s: &Value) -> bool {
    match string(job, "state") {
        "dispatching" => true,
        "retrying" => !resumed(job, s),
        _ => false,
    }
}
/// A completed terminal submission may wait behind Codex's native work. Keep
/// its existing, nondispatchable episode alive for at most ten minutes, with
/// all active cancellation guards. The receipt is not native acceptance;
/// acknowledgement still uses the existing authoritative main-agent
/// accepted-input observation.
fn acknowledgement_window(job: &Value, s: &Value, time: f64) -> f64 {
    use sha2::{Digest, Sha256};
    let receipt = &job["receipt"];
    let sent = seconds(job, "sent_at");
    let submitted = seconds(receipt, "submitted_at");
    let request_id = string(job, "request_id");
    let valid_id = uuid::Uuid::parse_str(request_id)
        .is_ok_and(|id| !id.is_nil() && id.to_string() == request_id);
    if s["agent"] != "codex" || job["agent"] != "codex"
        || job["action"] != "continue_message" || job["identity"] != identity(s)
        || string(s, "name").is_empty() || job["name"] != s["name"]
        || string(s, "run_id").is_empty() || string(s, "conversation_id").is_empty()
        || !valid_id || receipt["request_id"] != job["request_id"]
        || receipt["name"] != s["name"] || receipt["run_id"] != s["run_id"]
        || receipt["conversation_id"] != s["conversation_id"]
        || receipt["status"] != "submitted" || receipt["transport"] != "terminal"
        || receipt["first_message"] != false
        || receipt["text"] != CONTINUATION || receipt["submitted_text"] != CONTINUATION
        || !receipt["attachments"].as_array().is_some_and(Vec::is_empty)
        || sent <= 0.0 || sent < seconds(job, "started_at") || submitted < sent || submitted > time
    {
        return 30.0;
    }
    let request = json!({"request_id":request_id,"text":CONTINUATION,
        "expected_run_id":s["run_id"],"expected_conversation_id":s["conversation_id"]});
    if string(receipt, "digest") != format!("{:x}", Sha256::digest(request.to_string().as_bytes())) {
        return 30.0;
    }
    // Anchor the bound to the original dispatch, never the latest observation.
    CODEX_ACKNOWLEDGEMENT_WINDOW
}
fn schedule(job: &mut Value, s: &Value, time: f64) {
    job["action"] = json!(adapter(s));
    let category = class(&s["provider_error"]);
    if let Some(delays) = job["schedules"][category].as_array() { job["delays"] = json!(delays); }
    let n = if job["schedules"].is_object() { job["attempts"][category].as_u64().unwrap_or(0) }
        else { job["attempt"].as_u64().unwrap_or(0) } as usize;
    job["class_attempt"] = json!(n);
    let delays = job["delays"].as_array().cloned().unwrap_or_default();
    job["failure_id"] = json!(failure_id(s));
    job.as_object_mut().unwrap().remove("failure_ids");
    job.as_object_mut().unwrap().remove("resumed_at");
    remember_failure_copies(job, s);
    job["failure_at"] = s["provider_error"]["at"].clone();
    job["class"] = json!(category);
    let minimum = seconds(&s["provider_error"], "retry_not_before");
    job["reset_at"] = Value::Null;
    if category == "session_limit" && job["session_limit_mode"].as_str().unwrap_or("reset") == "reset" {
        let reset = session_limits::reset_at(&s["provider_error"]);
        job["reset_at"] = json!(reset);
        let Some(reset) = reset.filter(|v| *v > seconds(&s["provider_error"], "at")) else {
            stop(job, "blocked", "The session limit reset time is unknown or was already reached when the turn failed. Check Terminal or choose interval retries in Settings for future failures.");
            return;
        };
        job["not_before"] = json!(reset.max(minimum));
        job["due_at"] = job["not_before"].clone();
        stop(job, "waiting", "");
        return;
    }
    let Some(delay) = next_delay(&delays, n) else {
        stop(job, "exhausted", "Automatic attempts exhausted");
        return;
    };
    // Stable per-episode jitter survives restarts and avoids synchronized fleets.
    let jitter = string(job, "id").bytes().map(u64::from).sum::<u64>() % 101;
    job["not_before"] = json!(minimum);
    // Add the jitter separately: delay * 1.1 is not exact in binary floating point.
    job["due_at"] = json!((time + delay + delay * jitter as f64 / 1000.0).max(minimum));
    stop(job, "waiting", "");
}

// Pure transition function; wall clock is supplied so tests can exercise long
// backoff sequences, restarts, and interleaved user activity without sleeping.
fn advance(mut job: Value, s: &Value, p: &Policy, time: f64) -> Value {
    // A failed keep-warm ping is not the person's work: never continue it,
    // whatever earlier, finished episode the session keeps.
    if s["phase"] == "error" && keep_warm::ping_was_last_prompt(s) && !active(&job) {
        return Value::Null;
    }
    let error = &s["provider_error"];
    let fid = failure_id(s);
    let user_at = seconds(s, "recovery_user_at");
    let same = job.is_object() && job["identity"] == identity(s);
    let own_input = s["recovery_user_text"] == CONTINUATION
        && user_at >= seconds(&job, "sent_at")
        && seconds(&job, "sent_at") > 0.0;
    let new_input = same && user_at > seconds(&job, "user_at") && !own_input;
    if job.is_object() && !same && same_failure(&job, s) {
        stop(
            &mut job,
            "cancelled",
            "The session identity or model changed",
        );
        return job;
    }
    // Repair only the old source-switch cancellation before any delivery. A
    // cancelled/uncertain attempted send and a person's cancellation stay final.
    if same && job["state"] == "cancelled" && job["reason"] == "The failed turn is no longer current"
        && job["attempt"].as_u64() == Some(0) && seconds(&job, "sent_at") == 0.0 && job["request_id"].is_null()
        && !new_input && seconds(s, "recovery_cancel_at") <= seconds(&job, "started_at")
        && seconds(s, "recovery_success_at") <= seconds(&job, "started_at")
        && s["phase"] == "error" && s["process_state"] == "running"
        && enabled(p, class(error)) && seconds(error, "at") > p.enabled_at
        && confirmed_codex_copies(s) && same_failure(&job, s) {
        stop(&mut job, "waiting", "");
    }
    if same && active(&job) {
        if same_failure(&job, s) { remember_failure_copies(&mut job, s); }
        // Upgrade durable pre-v3 episodes without restarting them or losing budgets.
        if job["session_limit_mode"].is_null() {
            job["session_limit_mode"] = json!(p.session_limit_mode);
            if !job["schedules"].is_object() {
                let legacy = job["delays"].clone();
                job["schedules"] = json!({"service":legacy,"rate_limit":legacy,"network":legacy});
                let previous_class = string(&job, "class").to_owned();
                job["attempts"] = json!({previous_class:job["attempt"]});
            }
            job["schedules"]["session_limit"] = json!(delays_for(p, "session_limit"));
        }
        // A native acknowledgement observed late may still have occurred
        // inside the bound. A marker actually accepted after expiry cannot
        // extend the account reservation when no intervening poll ran.
        if own_input && !(job["state"] == "retrying" && job["acknowledged"] != true
            && acknowledgement_window(&job, s, time) == CODEX_ACKNOWLEDGEMENT_WINDOW
            && user_at > seconds(&job, "sent_at") + CODEX_ACKNOWLEDGEMENT_WINDOW) {
            job["acknowledged"] = json!(true);
            job["user_at"] = json!(user_at);
        }
        if !p.enabled {
            stop(&mut job, "cancelled", "Automatic recovery disabled");
            return job;
        }
        if new_input || seconds(s, "recovery_cancel_at") > seconds(&job, "started_at") {
            stop(
                &mut job,
                "cancelled",
                "New input or interruption cancelled recovery",
            );
            if seconds(error, "at") <= user_at {
                return job;
            }
        } else if ["approval", "input"].contains(&string(s, "phase"))
            || s["process_state"] == "exited"
        {
            stop(&mut job, "cancelled", "Session needs attention or stopped");
            return job;
        } else if job["acknowledged"] == true
            && seconds(s, "recovery_success_at") > seconds(&job, "started_at")
        {
            job["completed_at"] = s["recovery_success_at"].clone();
            stop(&mut job, "succeeded", "Agent completed the turn");
            return job;
        } else if string(&job, "state") == "retrying" {
            if !fid.is_empty()
                && !same_failure(&job, s)
                && seconds(error, "at") > seconds(&job, "sent_at")
            {
                if job["acknowledged"] != true {
                    stop(
                        &mut job,
                        "uncertain",
                        "Native delivery acknowledgement is missing",
                    );
                } else if !enabled(p, class(error)) {
                    stop(&mut job, "cancelled", "This error requires manual recovery");
                } else {
                    schedule(&mut job, s, time);
                }
            } else if job["acknowledged"] != true
                && time > seconds(&job, "sent_at") + acknowledgement_window(&job, s, time) {
                stop(
                    &mut job,
                    "uncertain",
                    "Delivery was not confirmed; inspect the native agent",
                );
            } else if job["resumed_at"].is_null() && resumed(&job, s) {
                // The episode stays open until the turn ends so a further failure keeps
                // its budget; clients show a resumed turn as ordinary work meanwhile.
                job["resumed_at"] = json!(seconds(s, "provider_status_at"));
            }
            return job;
        } else if job["state"] == "waiting" {
            if (!same_failure(&job, s) || s["phase"] != "error") && working_on_continuation(&job, s) {
                // The failure did not end the agent's work: never restart the
                // loop or send another continuation into a working agent.
                stop(&mut job, "retrying", "The agent kept working after the provider error");
            } else if !same_failure(&job, s) || s["phase"] != "error" {
                stop(
                    &mut job,
                    "cancelled",
                    "The failed turn is no longer current",
                );
            } else if !enabled(p, class(error)) {
                stop(&mut job, "cancelled", "This reaction is disabled");
            } else if job["class"] != class(error) {
                // A saved generic rate-limit failure can now have exact session-limit evidence.
                schedule(&mut job, s, time);
            }
            return job;
        } else {
            return job;
        }
    }
    if same
        && !new_input
        && !(job["state"] == "succeeded" && seconds(error, "at") > seconds(&job, "completed_at"))
    {
        return job;
    }
    if !enabled(p, class(error))
        || adapter(s).is_none()
        || s["phase"] != "error"
        || s["process_state"] == "exited"
        || seconds(error, "at") <= p.enabled_at
        || seconds(error, "at") <= user_at
        || fid.is_empty()
    {
        return Value::Null;
    }
    let mut fresh = json!({"version":1,"id":uuid::Uuid::new_v4().to_string(),"name":s["name"],"agent":s["agent"],
        "identity":identity(s),"action":adapter(s),"attempt":0,"attempts":{},
        "schedules":(["service","rate_limit","session_limit","network"].into_iter().map(|k|(k,delays_for(p,k))).collect::<BTreeMap<_,_>>()),
        "session_limit_mode":p.session_limit_mode,
        "delays":delays_for(p,class(error)),
        "started_at":time,"user_at":user_at,"state":"waiting"});
    schedule(&mut fresh, s, time);
    fresh
}

pub(super) fn enrich(record: &Value, output: &mut Value) {
    for key in ["name", "agent", "run_id", "conversation_id"] {
        if let Some(v) = record.get(key) {
            output[key] = v.clone();
        }
    }
    for key in [
        "recovery_user_at",
        "recovery_user_text",
        "recovery_success_at",
        "recovery_cancel_at",
        "keep_warm_prompt_at",
    ] {
        if let Some(v) = record.get(key) {
            output[key] = v.clone();
        }
    }
    let job = load_job(&owner(record));
    // Never show a previous conversation's countdown in a reused session row.
    if job.is_object()
        && job["identity"][0] == output["run_id"]
        && job["identity"][1] == output["conversation_id"]
    {
        output["recovery"] = job;
    }
    output["recovery_capability"] = json!(adapter(output));
    output["recovery_error_class"] = json!(class(&output["provider_error"]));
}

pub(super) fn check_submission(record: &Value, id: &str) -> Result<()> {
    let job = load_job(&owner(record));
    let p = policy()?;
    let s = journal::summary(record, true);
    let p = effective_policy(&p, override_for(&team_overrides(), &s));
    if job["id"] != id
        || job["state"] != "dispatching"
        || job["identity"] != identity(&s)
        || !same_failure(&job, &s)
        || s["phase"] != "error"
        || !enabled(&p, class(&s["provider_error"]))
        || seconds(&s, "recovery_user_at") > seconds(&job, "user_at")
    {
        return Err("The recovery request is no longer current; nothing was submitted".into());
    }
    Ok(())
}

pub(super) fn check_native_submission(snapshot: &Value) -> Result<()> {
    let job = load_job(&owner(snapshot));
    let p = effective_policy(&policy()?, override_for(&team_overrides(), snapshot));
    if job["state"] != "dispatching"
        || job["identity"] != identity(snapshot)
        || !same_failure(&job, snapshot)
        || !enabled(&p, class(&snapshot["provider_error"]))
    {
        return Err("Native recovery was cancelled or changed".into());
    }
    Ok(())
}

pub(super) fn observe() -> Result<Vec<Value>> {
    let panes = live()?;
    let mut snapshots = Vec::new();
    for record in records()? {
        let mut s = journal::summary(
            &record,
            matches(&record, panes_of(&panes, &record)),
        );
        // The record's own key goes with its observation (#1437 review).
        for key in ["name", "agent", "run_id", "conversation_id", "session_id", "layout"] {
            s[key] = record[key].clone();
        }
        snapshots.push(s);
    }
    for record in dsh::snapshots().unwrap_or_default() {
        if record["state"] == "running"
            && (record["native_recovery"].is_object()
                || record["phase"] == "error"
                || active(&load_job(&owner(&record))))
        {
            if let Ok(s) = dsh::inspect(string(&record, "name"), None) {
                snapshots.push(s);
            }
        }
    }
    Ok(snapshots)
}
fn tick() -> Result<()> {
    #[cfg(unix)]
    let Some(_round) = storage::job_round::try_acquire(storage::job_round::Kind::Recovery)? else {
        return Ok(());
    };
    crate::analytics::maintenance(&root());
    let mut sends = Vec::new();
    // Native inspection can wait on an agent. Never hold the global write lock
    // while collecting observations; delivery revalidates the exact failure.
    let overrides = team_overrides();
    let observing = policy()?.enabled || !overrides.is_empty();
    let snapshots = if observing { observe()? } else { vec![] };
    let mut dismisses = Vec::new();
    {
        let _guard = lock(None)?;
        #[cfg(unix)]
        _round.verify()?;
        let p = policy()?;
        let heartbeat = root().join("recovery/heartbeat.json");
        let last = read_json(&heartbeat).unwrap_or(Value::Null);
        if last["enabled"] != p.enabled || now() - seconds(&last, "at") > 5.0 {
            atomic(
                &heartbeat,
                &json!({"at":now(),"enabled":p.enabled}).to_string(),
            )?;
        }
        if !p.enabled && overrides.is_empty() {
            return Ok(());
        }
        let mut occupied = std::collections::BTreeSet::new();
        for s in &snapshots {
            let job = load_job(&owner(s));
            if occupies(&job, s) {
                occupied.insert(json!([s["agent"], s["account_id"]]).to_string());
            }
        }
        for s in snapshots {
            let key = owner(&s);
            // H1: a team's request only wakes the session-limit class for this
            // session; everything else follows the shared policy.
            let requested = override_for(&overrides, &s);
            if !p.enabled && !requested {
                continue;
            }
            let effective = effective_policy(&p, requested);
            let old = load_job(&key);
            let mut job = advance(old.clone(), &s, &effective, now());
            if job.is_null() {
                if old.is_object() {
                    atomic(
                        &root()
                            .join("recovery/history")
                            .join(format!("{}.json", string(&old, "id"))),
                        &old.to_string(),
                    )?;
                    fs::remove_file(job_path(&key)).map_err(|e| e.to_string())?;
                }
                if s["native_recovery"].is_object() {
                    dismisses.push(s.clone());
                }
                continue;
            }
            let account = json!([s["agent"], s["account_id"]]).to_string();
            if job["state"] == "waiting"
                && now() >= seconds(&job, "due_at")
                && !occupied.contains(&account)
            {
                occupied.insert(account);
                job["attempt"] = json!(job["attempt"].as_u64().unwrap_or(0) + 1);
                if job["schedules"].is_object() {
                    let category=string(&job,"class").to_owned();
                    let n=job["attempts"][&category].as_u64().unwrap_or(0)+1;
                    job["attempts"][&category]=json!(n);job["class_attempt"]=json!(n);
                }
                job["request_id"] = json!(uuid::Uuid::new_v4().to_string());
                job["sent_at"] = json!(now());
                job["acknowledged"] = json!(false);
                stop(&mut job, "dispatching", "");
                sends.push((s.clone(), job.clone()));
            }
            if job != old {
                save_job(&job, &key)?;
            }
            if !active(&job) && s["native_recovery"].is_object() {
                dismisses.push(s.clone());
            }
        }
    }
    for s in dismisses {
        #[cfg(unix)]
        _round.verify()?;
        let _ = dsh::dismiss_recovery(&s);
    }
    for (s, mut job) in sends {
        #[cfg(unix)]
        _round.verify()?;
        let result = if s["agent"] == "dsh" {
            dsh::recover(&s, string(&job, "request_id"))
        } else {
            let payload = json!({"request_id":job["request_id"],"text":CONTINUATION,
                "expected_run_id":s["run_id"],"expected_conversation_id":s["conversation_id"]});
            input::submit(
                string(&s, "name"),
                payload.to_string().as_bytes(),
                Some(string(&job, "id")),
            )
        };
        let _guard = lock(None)?;
        #[cfg(unix)]
        _round.verify()?;
        let key = owner(&s);
        let current = load_job(&key);
        if current["id"] != job["id"] || current["state"] != "dispatching" {
            continue;
        }
        match result {
            Ok(receipt) => {
                job["acknowledged"] = json!(receipt["status"] == "confirmed");
                job["receipt"] = receipt;
                stop(&mut job, "retrying", "");
            }
            Err(e) => {
                let definitely_unsent = s["agent"] != "dsh"
                    && !root()
                        .join("input_receipts")
                        .join(format!("{}.json", string(&job, "request_id")))
                        .exists();
                if definitely_unsent {
                    job["attempt"] = json!(job["attempt"].as_u64().unwrap_or(1).saturating_sub(1));
                    if job["schedules"].is_object() {
                        let category=string(&job,"class").to_owned();
                        let n=job["attempts"][&category].as_u64().unwrap_or(1).saturating_sub(1);
                        job["attempts"][&category]=json!(n);job["class_attempt"]=json!(n);
                    }
                }
                stop(
                    &mut job,
                    if definitely_unsent {
                        "blocked"
                    } else {
                        "uncertain"
                    },
                    &e,
                );
            }
        }
        save_job(&job, &key)?;
    }
    Ok(())
}
/// The owner key of a job file: its own stem, which outlives its record.
fn stem(path: &Path) -> Option<String> {
    path.file_stem().and_then(|stem| stem.to_str()).map(str::to_owned)
}
fn recover_interrupted_jobs() -> Result<()> {
    let _guard = lock(None)?;
    if let Ok(entries) = fs::read_dir(root().join("recovery/jobs")) {
        for entry in entries.flatten() {
            let (Ok(mut job), Some(key)) = (read_json(&entry.path()), stem(&entry.path())) else {
                continue;
            };
            {
                if job["state"] == "dispatching" {
                    stop(
                        &mut job,
                        "uncertain",
                        "Recovery worker restarted during delivery; inspect the agent",
                    );
                    save_job(&job, &key)?;
                }
            }
        }
    }
    Ok(())
}
/// The installed service runs `hgs recovery worker` under this unit or label.
const SERVICE: service_worker::Service = service_worker::Service {
    unit: "hgs-recovery.service",
    label: "com.hgdev.hgs-recovery",
};
/// Held by the worker that executes recovery.
fn worker_lock() -> PathBuf {
    root().join("recovery/worker.lock")
}
/// Held by the service's worker while it waits for the executor lock.
fn standby_lock() -> PathBuf {
    root().join("recovery/service.lock")
}
fn worker() -> Result<i32> {
    private_dir(&root().join("recovery"))?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let started = fs::metadata(&executable).and_then(|m| m.modified()).ok();
    let replaced = || fs::metadata(&executable).and_then(|m| m.modified()).ok() != started;
    // launchd and systemd start their worker again 10 s after it exits, so it
    // waits for a worker a client started; that worker then gives way to it.
    let Some(owner) = service_worker::acquire(
        &worker_lock(),
        &standby_lock(),
        lock_file,
        || SERVICE.is_current(),
        replaced,
        Duration::from_secs(1),
    )?
    else {
        return Ok(0);
    };
    recover_interrupted_jobs()?;
    super::worktree_maintenance::worker();
    // Keep-warm rounds run beside recovery, whatever its policy says.
    let mut keep_warm_at: Option<std::time::Instant> = None;
    loop {
        if replaced() || owner.yields() {
            return Ok(0);
        }
        if let Err(e) = tick() {
            eprintln!("hgs recovery: {e}");
        }
        if keep_warm_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(15)) {
            keep_warm_at = Some(std::time::Instant::now());
            if let Err(e) = keep_warm::tick() {
                eprintln!("hgs keep-warm: {e}");
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
/// The session host supervises the worker on Windows (spec D14): it runs the
/// stable CLI's `hgs.exe recovery worker` and restarts it while the host runs.
/// Without a host nothing starts one, as no session runs then; a worker detached
/// from hgs.exe's job would outlive the GUI and keep the GUI's hgs.exe from
/// being replaced.
#[cfg(windows)]
pub(super) fn ensure_worker() -> Result<()> {
    crate::self_exec::ensure().map_err(|e| e.to_string())?;
    backend::host::HOST.supervise_recovery()
}
#[cfg(not(windows))]
pub(super) fn ensure_worker() -> Result<()> {
    let executable = crate::self_exec::current().map_err(|e| e.to_string())?;
    // Policy refresh must not race the installed supervisor with a detached
    // executor. Starting an active unit preserves its PID and current delivery.
    #[cfg(target_os = "linux")]
    if home().join(".config/systemd/user/hgs-recovery.service").is_file()
        && crate::platform::silent("systemctl", &["--user", "start", "--no-block", "hgs-recovery.service"]) {
        return Ok(());
    }
    // A running worker, or the service's worker waiting to take over, needs
    // no detached one.
    if crate::status::locked(&worker_lock()) || service_worker::waiting(&standby_lock()) {
        return Ok(());
    }
    // Detached from both SSH and GUI lifetime; service installation additionally
    // restarts the worker after login. The worker lock handles competing GUIs.
    let mut cmd = Command::new(executable);
    cmd.args(["recovery", "worker"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::spawn_detached(&mut cmd).map_err(|e| e.to_string())?;
    Ok(())
}
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("worker") if args.len() == 1 => return worker(),
        Some("tick") if args.len() == 1 => {
            tick()?;
            keep_warm::tick()?;
            return Ok(0);
        }
        Some("get") if args.len() == 1 => {
            let p = policy()?;
            if p.enabled {
                ensure_worker()?;
            }
            println!(
                "{}",
                json!({"order":policy_order(&p),"policy":p,"continuation_message":CONTINUATION,
                "adapters":{"claude":"continue_message","codex":"continue_message","kimi":"continue_message","dsh":"retry_request"},
                "worker":read_json(&root().join("recovery/heartbeat.json")).unwrap_or(Value::Null)})
            );
        }
        Some("set" | "sync") if args.len() == 1 => {
            let syncing = args[0] == "sync";
            let request = stdin_json()?;
            let mut p: Policy = serde_json::from_value(request).map_err(|e| e.to_string())?;
            validate_policy(&p)?;
            {
                let _guard = lock(None)?;
                let old = policy()?;
                if !syncing && (p.revision != old.revision || p.writer != old.writer) {
                    return Err("Recovery settings changed on this machine; reload them".into());
                }
                if syncing && policy_order(&p) <= policy_order(&old) {
                    println!("{}", json!({"order":policy_order(&old),"policy":old}));
                    return Ok(0);
                }
                if !syncing {
                    p.revision += 1;
                    p.writer = uuid::Uuid::new_v4().to_string();
                }
                p.version = 3;
                p.enabled_at = if p.enabled && !old.enabled {
                    now()
                } else {
                    old.enabled_at
                };
                atomic(
                    &policy_path(),
                    &serde_json::to_string(&p).map_err(|e| e.to_string())?,
                )?;
                {
                    if let Ok(entries) = fs::read_dir(root().join("recovery/jobs")) {
                        for entry in entries.flatten() {
                            let (Ok(mut job), Some(key)) = (read_json(&entry.path()), stem(&entry.path())) else {
                                continue;
                            };
                            {
                                if active(&job) && !enabled(&p, string(&job, "class")) {
                                    stop(
                                        &mut job,
                                        "cancelled",
                                        "Automatic recovery disabled for this error class",
                                    );
                                    save_job(&job, &key)?;
                                }
                            }
                        }
                    }
                }
            }
            if p.enabled {
                ensure_worker()?;
            }
            println!("{}", json!({"order":policy_order(&p),"policy":p}));
        }
        // N30: a team on another computer asks this one's recovery service to
        // wait for (or stop waiting for) a member session's reset.
        Some("team-override") if args.len() == 1 => {
            let request = stdin_json()?;
            let name = string(&request, "name");
            if name.is_empty() {
                return Err("A team override names its session".into());
            }
            let on = request["on"].as_bool().ok_or("A team override says \"on\": true or false")?;
            set_team_override(name, string(&request, "run_id"), on)?;
            if on {
                ensure_worker()?;
            }
            println!("{}", json!({"status":"ok","name":name,"on":on}));
        }
        Some("action") if args.len()==2 && args[1]=="--scoped-json" => {scoped_action(stdin_json()?)?;}
        Some("action") if args.len() == 1 => {
            let request = stdin_json()?;
            let _guard = lock(None)?;
            // The job of the session this name reads as now; with no record
            // there is no job to change.
            let key = read(string(&request, "name"))
                .map(|record| owner(&record))
                .map_err(|_| "This scheduled recovery is no longer current; nothing was changed")?;
            let mut job = load_job(&key);
            if job["id"] != request["id"] || job["state"] != "waiting" {
                return Err(
                    "This scheduled recovery is no longer current; nothing was changed".into(),
                );
            }
            match string(&request, "action") {
                "cancel" => stop(&mut job, "cancelled", "Cancelled by you"),
                "now" => {
                    job["due_at"] = json!(now().max(seconds(&job, "not_before")));
                }
                _ => return Err("Unknown recovery action".into()),
            }
            save_job(&job, &key)?;
            println!("{job}");
        }
        _ => return Err("usage: hgs recovery get | set | sync | action | worker".into()),
    }
    Ok(0)
}


/// A fixed session recovery control. Claim and job/state locks are independent;
/// no native input is submitted here, only the existing waiting job is edited.
fn scoped_action(request: Value) -> Result<()> {
    use sha2::{Digest, Sha256};
    let fields = [
        "request_id",
        "name",
        "expected_run_id",
        "expected_conversation_id",
        "job_id",
        "action",
    ];
    if !request
        .as_object()
        .is_some_and(|v| v.len() == fields.len() && fields.iter().all(|k| v.contains_key(*k)))
    {
        return Err("Invalid scoped recovery fields".into());
    }
    for field in ["request_id", "job_id"] {
        let value = string(&request, field);
        if uuid::Uuid::parse_str(value)
            .map(|v| v.is_nil() || v.to_string() != value)
            .unwrap_or(true)
        {
            return Err("Invalid recovery UUID".into());
        }
    }
    for (field, max) in [
        ("name", 512),
        ("expected_run_id", 128),
        ("expected_conversation_id", 256),
    ] {
        let value = string(&request, field);
        if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
            return Err("Invalid recovery identity".into());
        }
    }
    if !["now", "cancel"].contains(&string(&request, "action")) {
        return Err("Unknown scoped recovery action".into());
    }
    let name = string(&request, "name");
    // zerus-ucck P3c: the name arrives in the request, so it is resolved here:
    // its identities select among sessions that share the label.
    select_by(name, string(&request, "expected_run_id"), string(&request, "expected_conversation_id"))?;
    let directory = absolute_root()?.join("recovery_receipts");
    private_dir(&directory)?;
    let path = directory.join(format!("{}.json", string(&request, "request_id")));
    let _claim = lock(Some(
        &directory.join(format!("{}.lock", string(&request, "request_id"))),
    ))?;
    let hash = format!("{:x}", Sha256::digest(request.to_string()));
    let mut answer = json!({"request_id":request["request_id"],"name":name,"run_id":request["expected_run_id"],"conversation_id":request["expected_conversation_id"],"job_id":request["job_id"],"action":request["action"],"request_hash":hash});
    if path.exists() {
        answer = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if answer["request_hash"] != hash {
            return Err("Recovery UUID already used for another action".into());
        }
        if answer["status"] == "claimed" {
            answer["status"] = json!("uncertain");
            answer["error"] = json!("Earlier recovery edit was interrupted; it was not replayed");
            atomic(&path, &answer.to_string())?;
        }
    } else {
        let mut changed = false;
        let outcome = (|| -> Result<Value> {
            // Normalize native evidence without starting/reloading a host. The
            // raw persisted binding is rechecked once both local locks are held.
            let captured = if dsh::exists(name) {
                Some(dsh::binding(name)?)
            } else {
                None
            };
            let native = if captured.is_some() {
                Some(dsh::inspect(name, None)?)
            } else {
                None
            };
            // Existing DSH lifecycle uses this binding-lock then writer-lock order.
            let _binding = if captured.is_some() {
                Some(lock(Some(&root().join("dsh/bindings.lock")))?)
            } else {
                None
            };
            let _guard = lock(None)?;
            if dsh::exists(name) != captured.is_some() {
                return Err("Recovery adapter binding changed".into());
            }
            let current = if captured.is_some() {
                dsh::binding(name)?
            } else {
                read(name)?
            };
            if captured.as_ref().is_some_and(|old| old != &current) {
                return Err("Recovery binding changed during native inspection".into());
            }
            let normalized = native.unwrap_or_else(|| journal::summary(&current, false));
            if normalized["run_id"] != request["expected_run_id"]
                || normalized["conversation_id"] != request["expected_conversation_id"]
            {
                return Err("Normalized recovery session changed".into());
            }
            let key = owner(&current);
            let mut job = load_job(&key);
            if string(&current, "name") != name
                || current["run_id"] != request["expected_run_id"]
                || current["conversation_id"] != request["expected_conversation_id"]
                || !string(&current, "archive_id").is_empty()
                || job["id"] != request["job_id"]
                || job["state"] != "waiting"
                || job["identity"] != identity(&normalized)
            {
                return Err(
                    "Recovery job or exact native session identity changed; nothing was changed"
                        .into(),
                );
            }
            answer["status"] = json!("claimed");
            atomic(&path, &answer.to_string())?;
            match string(&request, "action") {
                "cancel" => stop(&mut job, "cancelled", "Cancelled by you"),
                "now" => job["due_at"] = json!(now().max(seconds(&job, "not_before"))),
                _ => unreachable!(),
            }
            changed = true;
            save_job(&job, &key)?;
            Ok(job)
        })();
        match outcome {
            Ok(job) => {
                answer["status"] = json!(if request["action"] == "cancel" {
                    "cancelled"
                } else {
                    "scheduled"
                });
                answer["recovery"] = job;
            }
            Err(error) => {
                answer["status"] = json!(if changed { "uncertain" } else { "failed" });
                answer["error"] = json!(error);
            }
        }
        atomic(&path, &answer.to_string())?;
    }
    answer.as_object_mut().unwrap().remove("request_hash");
    println!("{answer}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn test_image_refuses_before_service_activation() {
        crate::self_exec::private_probe("state::recovery::tests::service_activation_probe");
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run by test_image_refuses_before_service_activation"]
    fn service_activation_probe() {
        crate::self_exec::probe_child("state::recovery::tests::service_activation_probe");
        assert_eq!(ensure_worker().unwrap_err(), crate::self_exec::TEST_IMAGE);
    }
    fn codex_copy(snapshot: &mut Value) {
        let original = failure_id(snapshot);
        snapshot["provider_error"]["source"] = json!("codex_native_log");
        snapshot["provider_error"]["message_id"] = json!("log-copy");
        snapshot["provider_error"]["at"] = json!(10.000328);
        snapshot["provider_error"]["failure_ids"] = json!([original, failure_id(snapshot)]);
    }
    #[test]
    fn equivalent_native_evidence_preserves_deadline_budget_and_retry_acknowledgement() {
        let mut s = snapshot("codex"); s["provider_error"]["source"] = json!("codex_native_transcript");
        let waiting = advance(Value::Null, &s, &on(), 11.0);
        codex_copy(&mut s);
        let observed = advance(waiting.clone(), &s, &on(), 12.0);
        for key in ["id", "due_at", "attempt", "delays", "identity", "failure_id", "state"] { assert_eq!(observed[key], waiting[key]); }
        assert!(same_failure(&observed, &s));
        let mut retrying = observed; retrying["state"] = json!("retrying");
        retrying["attempt"] = json!(1); retrying["sent_at"] = json!(10.0001); retrying["acknowledged"] = json!(true);
        assert_eq!(advance(retrying.clone(), &s, &on(), 12.0), retrying);
        let mut completion_only = snapshot("codex"); completion_only["provider_error"]["source"] = json!("codex_native_transcript");
        assert_eq!(advance(retrying.clone(), &completion_only, &on(), 12.0), retrying);
        s["provider_error"]["message_id"] = json!("new-turn-error");
        s["provider_error"]["at"] = json!(20.0); s["provider_error"]["failure_ids"] = json!([failure_id(&s)]);
        let following = advance(retrying, &s, &on(), 21.0);
        assert_eq!(following["state"], "waiting"); assert_eq!(following["attempt"], 1);
        assert_eq!(following["failure_id"], failure_id(&s));
        assert!(!same_failure(&following, &completion_only));
    }
    #[test]
    fn repairs_only_proven_source_switch_cancellations_without_an_attempt() {
        let mut s = snapshot("codex"); s["provider_error"]["source"] = json!("codex_native_transcript");
        let mut cancelled = advance(Value::Null, &s, &on(), 11.0);
        stop(&mut cancelled, "cancelled", "The failed turn is no longer current");
        codex_copy(&mut s);
        let repaired = advance(cancelled.clone(), &s, &on(), 30.0);
        assert_eq!(repaired["state"], "waiting");
        for key in ["id", "due_at", "attempt", "delays", "identity", "failure_id"] { assert_eq!(repaired[key], cancelled[key]); }
        for (key, value) in [("attempt", json!(1)), ("sent_at", json!(12.0)), ("request_id", json!("request")),
            ("reason", json!("Cancelled by user")), ("state", json!("uncertain"))] {
            let mut protected = cancelled.clone(); protected[key] = value;
            assert_eq!(advance(protected.clone(), &s, &on(), 30.0), protected);
        }
        for (key, value) in [("recovery_cancel_at", json!(20.0)), ("recovery_success_at", json!(20.0)),
            ("phase", json!("idle")), ("process_state", json!("exited"))] {
            let mut changed = s.clone(); changed[key] = value;
            assert_eq!(advance(cancelled.clone(), &changed, &on(), 30.0), cancelled);
        }
        let mut changed = s.clone(); changed["recovery_user_at"] = json!(20.0);
        assert!(advance(cancelled.clone(), &changed, &on(), 30.0).is_null());
        let mut disabled = on(); disabled.enabled = false;
        assert_eq!(advance(cancelled.clone(), &s, &disabled, 30.0), cancelled);
        s["provider_error"]["failure_ids"] = Value::Null;
        assert_eq!(advance(cancelled.clone(), &s, &on(), 30.0), cancelled);
    }
    fn snapshot(agent: &str) -> Value {
        json!({"name":"session","agent":agent,"run_id":"run","conversation_id":"conversation","model":"model","account_id":"account",
            "phase":"error","process_state":"running","recovery_user_at":5.0,
            "provider_error":{"source":"native","message_id":"error-1","at":10.0,"detail":"API Error: 529 Overloaded"}})
    }
    fn on() -> Policy {
        Policy {
            enabled: true,
            enabled_at: 1.0,
            ..Policy::default()
        }
    }
    #[test]
    fn a_verified_keep_warm_ping_failure_never_starts_recovery() {
        let mut earlier = snapshot("claude");
        earlier["provider_error"]["at"] = json!(3.0);
        earlier["recovery_user_at"] = json!(2.0);
        let mut old = advance(Value::Null, &earlier, &on(), 3.5);
        assert_eq!(old["state"], "waiting");
        stop(&mut old, "succeeded", "Agent completed the turn");
        old["completed_at"] = json!(4.0);
        let mut ping = snapshot("claude");
        ping["recovery_user_text"] = json!(keep_warm::TEXT);
        ping["keep_warm_prompt_at"] = ping["recovery_user_at"].clone();
        assert!(advance(Value::Null, &ping, &on(), 11.0).is_null());
        assert!(advance(old.clone(), &ping, &on(), 11.0).is_null(), "a completed earlier episode must not let a ping failure in");
        // The same text typed by the person is the person's turn.
        let mut typed = ping.clone();
        typed["keep_warm_prompt_at"] = json!(1.0);
        assert_eq!(advance(old, &typed, &on(), 11.0)["state"], "waiting");
    }
    #[test]
    fn independent_sequences_keep_class_budgets_and_repeat_last_delay() {
        for values in [vec![0],vec![1,0,2],vec![1,-1,3],vec![-2],vec![]] {assert!(validate_delays(&values).is_err());}
        assert_eq!(next_delay(&[json!(15),json!(30),json!(0)],42),Some(30.0));
        assert_eq!(next_delay(&[json!(15),json!(-1)],1),None);
        assert_eq!(next_delay(&[json!(-1)],0),None);
        let mut p=on();p.schedules.insert("service".into(),vec![10,20,-1]);p.schedules.insert("network".into(),vec![3,0]);
        let mut s=snapshot("claude");let mut job=advance(Value::Null,&s,&p,11.0);
        assert_eq!(job["delays"],json!([10,20,-1]));
        job["attempts"]["service"]=json!(1);job["attempt"]=json!(1);
        s["provider_error"]["detail"]=json!("connection reset");schedule(&mut job,&s,100.0);
        assert_eq!(job["class_attempt"],0);assert_eq!(job["delays"],json!([3,0]));
        job["attempts"]["network"]=json!(10);schedule(&mut job,&s,200.0);assert_eq!(job["state"],"waiting");
        s["provider_error"]["detail"]=json!("503 unavailable");schedule(&mut job,&s,300.0);
        assert_eq!(job["class_attempt"],1);assert!(seconds(&job,"due_at")>=320.0&&seconds(&job,"due_at")<323.0);
        job["attempts"]["service"]=json!(2);schedule(&mut job,&s,400.0);assert_eq!(job["state"],"exhausted");
    }
    #[test]
    fn transient_classes_exclude_permanent_errors_and_future_agents() {
        for (detail, expected) in [
            ("529 Overloaded", "service"),
            ("HTTP 503", "service"),
            ("500 internal server error", "service"),
            ("524 status code (no body)", "service"),
            ("HTTP 520", "service"),
            ("522: connection timed out", "service"),
            ("read 5240 lines", "manual"),
            ("exit 1524", "manual"),
            ("429 rate limit", "rate_limit"),
            ("429 insufficient quota", "manual"),
            ("overloaded account balance exhausted", "manual"),
            ("401 authentication failed", "manual"),
            ("policy_violation: HTTP 503 service unavailable", "manual"),
            ("Request blocked by usage policy; HTTP 429", "manual"),
            ("context window exceeded", "manual"),
            ("stream disconnected", "network"),
            ("ECONNRESET", "network"),
            ("tool returned 1", "manual"),
        ] {
            assert_eq!(class(&json!({"detail":detail})), expected, "{detail}");
        }
        for detail in ["HTTP 503", "429 rate limit", "network error"] {
            assert_eq!(
                class(&json!({"error_kind":"provider_policy","detail":detail})),
                "manual"
            );
            let mut s = snapshot("codex");
            s["provider_error"] = json!({"error_kind":"provider_policy","detail":detail});
            assert!(advance(Value::Null, &s, &on(), 11.0).is_null());
        }
        for agent in ["claude", "kimi", "codex"] {
            assert_eq!(
                advance(Value::Null, &snapshot(agent), &on(), 11.0)["action"],
                "continue_message"
            );
        }
        assert!(advance(Value::Null, &snapshot("future-agent"), &on(), 11.0).is_null());
        let mut s = snapshot("dsh");
        assert_eq!(
            advance(Value::Null, &s, &on(), 11.0)["action"],
            "continue_message"
        );
        s["native_recovery"] = json!({"id":"step"});
        assert_eq!(
            advance(Value::Null, &s, &on(), 11.0)["action"],
            "retry_request"
        );
    }
    #[test]
    fn projected_non_string_kimi_evidence_never_admits_recovery() {
        let window = "You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends.";
        for detail in [window.to_owned(), format!("429 {window}")] {
            for code in [json!({"code":"insufficient_quota","message":"billing account exhausted"}),
                json!(401), json!(["provider_policy"]), Value::Null, json!(false)] {
                let mut s = snapshot("kimi");
                journal::update_activity(&mut s, &json!({"hook_event_name":"StopFailure",
                    "error_type":"APIProviderRateLimitError","error_message":detail,"error":code}));
                let failure = s["provider_error"].clone();
                assert!(failure.get("native_error_code").is_some_and(Value::is_null));
                provider_errors::apply(&mut s, &failure);
                assert_eq!(s["provider_error"]["native_error_code"], Value::Null);
                assert_eq!(s["provider_error"]["detail"], detail);
                assert!(!provider_errors::rolling_rate_error(&s["provider_error"]));
                assert_eq!(class(&s["provider_error"]), "manual");
                assert!(advance(Value::Null, &s, &on(), 11.0).is_null());
            }
        }
    }
    #[test]
    fn typed_kimi_window_requires_native_evidence_and_the_existing_enabled_policy() {
        let mut s = snapshot("kimi");
        s["provider_error"] = json!({"source":"provider_hook","message_id":"original-failure","at":10.0,
            "error_kind":"quota","native_error_type":"APIProviderRateLimitError",
            "detail":"You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends."});
        assert_eq!(class(&s["provider_error"]), "rate_limit");
        let before = s.clone();
        let job = advance(Value::Null, &s, &on(), 11.0);
        assert_eq!(job["state"], "waiting");
        assert_eq!(job["class"], "rate_limit");
        assert_eq!(s, before);
        assert_eq!(advance(job.clone(), &s, &on(), 12.0), job);
        let mut disabled = on(); disabled.rate_limit = false;
        assert!(advance(Value::Null, &s, &disabled, 11.0).is_null());
        for (key, value) in [("native_error_type", ""), ("native_error_code", "quota"),
            ("native_error_code", "authentication"), ("error_kind", "provider_policy")] {
            let mut changed = s.clone(); changed["provider_error"][key] = json!(value);
            assert_eq!(class(&changed["provider_error"]), "manual");
            assert!(advance(Value::Null, &changed, &on(), 11.0).is_null());
        }
    }
    #[test]
    fn kimi_rolling_window_uses_existing_rate_schedule_without_inventing_a_reset() {
        let detail = "429 You've reached your 5-hour usage limit. Your quota will reset when the current 5-hour window ends. To continue now, purchase extra usage or upgrade your plan: https://example.invalid/?tab=quota";
        let mut s = snapshot("kimi");
        s["provider_error"]["detail"] = json!(detail);
        s["provider_error"]["error_kind"] = json!("quota"); // Saved by an older hook.
        let p: Policy = serde_json::from_value(json!({"version":2,"enabled":true,"enabled_at":1,
            "schedules":{"rate_limit":[15,30,60,300,0]}})).unwrap();
        let job = advance(Value::Null, &s, &p, 11.0);
        assert_eq!(job["class"], "rate_limit");
        assert_eq!(job["state"], "waiting");
        assert_eq!(job["delays"], json!([15,30,60,300,0]));
        assert_eq!(job["not_before"], 0.0);
        assert!(seconds(&job, "due_at") >= 26.0 && seconds(&job, "due_at") < 29.0);
        assert_eq!(advance(serde_json::from_str(&job.to_string()).unwrap(), &s, &p, 12.0), job);
        let mut repeat = job.clone(); repeat["attempts"]["rate_limit"] = json!(50);
        schedule(&mut repeat, &s, 100.0);
        assert_eq!(repeat["state"], "waiting");
        assert_eq!(repeat["class_attempt"], 50);
        assert!(seconds(&repeat, "due_at") >= 400.0 && seconds(&repeat, "due_at") < 431.0);
        for enabled in [false, true] {
            let mut off = p.clone(); off.enabled = enabled; off.rate_limit = false;
            assert!(advance(Value::Null, &s, &off, 11.0).is_null());
        }
        for kind in ["authentication", "provider_policy", "context_limit"] {
            s["provider_error"]["error_kind"] = json!(kind);
            assert_eq!(class(&s["provider_error"]), "manual");
            assert!(advance(Value::Null, &s, &p, 11.0).is_null());
        }
        for detail in ["429 insufficient quota", "429 monthly quota exhausted", "429 billing limit", "You've hit your session limit"] {
            s["provider_error"] = json!({"source":"native","message_id":"other","at":10.0,"detail":detail});
            if detail.contains("session limit") {
                assert_eq!(advance(Value::Null, &s, &p, 11.0)["state"], "blocked");
            } else {
                assert!(advance(Value::Null, &s, &p, 11.0).is_null());
            }
        }
    }
    #[test]
    fn off_and_old_errors_never_schedule_and_deadlines_survive_restart() {
        let mut s = snapshot("claude");
        assert!(advance(Value::Null, &s, &Policy::default(), 11.0).is_null());
        let mut p = on();
        p.enabled_at = 20.0;
        assert!(advance(Value::Null, &s, &p, 21.0).is_null());
        s["provider_error"]["retry_not_before"] = json!(1000.0);
        let job = advance(Value::Null, &s, &on(), 11.0);
        assert_eq!(job["due_at"], 1000.0);
        let restored: Value = serde_json::from_str(&job.to_string()).unwrap();
        assert_eq!(advance(restored, &s, &on(), 500.0), job);
    }
    #[test]
    fn session_limits_wait_for_reset_and_interval_mode_uses_its_own_budget() {
        let mut s = snapshot("claude");
        s["provider_error"] = json!({"source":"provider_hook","message_id":"limit","at":10.0,
            "error_kind":"rate_limit","detail":"You've hit your session limit","reset_at":3600.0});
        let p = on();
        let job = advance(Value::Null, &s, &p, 11.0);
        assert_eq!(job["class"], "session_limit");
        assert_eq!(job["session_limit_mode"], "reset");
        assert_eq!(job["state"], "waiting");
        assert_eq!(job["due_at"], 3600.0);
        assert_eq!(job["not_before"], 3600.0);
        assert_eq!(advance(serde_json::from_str(&job.to_string()).unwrap(), &s, &p, 1000.0), job);
        // A worker recovering after the deadline can still make the scheduled attempt.
        assert_eq!(advance(Value::Null, &s, &p, 4000.0)["due_at"], 3600.0);
        assert_eq!(advance(job.clone(), &s, &Policy::default(), 12.0)["state"], "cancelled");
        let mut disabled = p.clone(); disabled.session_limit = false;
        assert!(advance(Value::Null, &s, &disabled, 11.0).is_null());
        assert_eq!(advance(job.clone(), &s, &disabled, 12.0)["state"], "cancelled");
        let mut interval = p.clone(); interval.session_limit_mode = "interval".into();
        interval.schedules.insert("session_limit".into(), vec![90, 0]);
        let mut repeat = advance(Value::Null, &s, &interval, 11.0);
        assert!(seconds(&repeat, "due_at") >= 101.0 && seconds(&repeat, "due_at") <= 110.0);
        assert_eq!(repeat["not_before"], 0.0);
        repeat["attempts"]["session_limit"] = json!(50);
        schedule(&mut repeat, &s, 200.0);
        assert_eq!(repeat["state"], "waiting");
        assert_eq!(repeat["class_attempt"], 50);
        s["provider_error"]["retry_not_before"] = json!(1000.0);
        schedule(&mut repeat, &s, 300.0);
        assert_eq!(repeat["due_at"], 1000.0);
        s["provider_error"]["detail"] = json!("429 temporary rate limit");
        schedule(&mut repeat, &s, 400.0);
        assert_eq!(repeat["class"], "rate_limit");
        assert_eq!(repeat["class_attempt"], 0);
    }
    #[test]
    fn missing_or_expired_reset_never_starts_a_short_retry_loop() {
        let mut s = snapshot("claude");
        s["provider_error"]["detail"] = json!("You've hit your session limit");
        for reset in [Value::Null, json!(10.0)] {
            s["provider_error"]["reset_at"] = reset;
            let job = advance(Value::Null, &s, &on(), 11.0);
            assert_eq!(job["state"], "blocked");
            assert_eq!(advance(job.clone(), &s, &on(), 100000.0), job);
        }
        assert_eq!(class(&json!({"error_kind":"provider_policy","detail":"You've hit your session limit"})), "manual");
        assert_eq!(class(&json!({"detail":"You've hit your session limit; insufficient balance"})), "manual");
    }
    #[test]
    fn legacy_policies_and_waiting_episodes_upgrade_without_short_limit_retries() {
        for version in [1, 2] {
            let p: Policy = serde_json::from_value(json!({"version":version,"enabled":true,
                "delays":[5,20,-1],"schedules":{"network":[7,0]}})).unwrap();
            validate_policy(&p).unwrap();
            assert!(p.session_limit);
            assert_eq!(p.session_limit_mode, "reset");
            assert_eq!(delays_for(&p, "network"), &[7,0]);
            assert_eq!(delays_for(&p, "session_limit"), &[300,0]);
        }
        let mut invalid = on(); invalid.session_limit_mode = "guess".into();
        assert!(validate_policy(&invalid).is_err());
        let mut s = snapshot("claude");
        s["provider_error"]["detail"] = json!("429 rate limit");
        let mut old = advance(Value::Null, &s, &on(), 11.0);
        old.as_object_mut().unwrap().remove("session_limit_mode");
        old.as_object_mut().unwrap().remove("schedules");
        old["attempt"] = json!(2);
        s["provider_error"]["detail"] = json!("You've hit your session limit");
        s["provider_error"]["reset_at"] = json!(3600.0);
        let upgraded = advance(old.clone(), &s, &on(), 12.0);
        assert_eq!(upgraded["id"], old["id"]);
        assert_eq!(upgraded["attempts"]["rate_limit"], 2);
        assert_eq!(upgraded["class"], "session_limit");
        assert_eq!(upgraded["due_at"], 3600.0);
    }
    #[test]
    fn budget_is_finite_across_distinct_errors_not_reset_by_progress() {
        let mut s = snapshot("kimi");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        for attempt in 1..=4 {
            let sent = attempt as f64 * 1000.0;
            job["attempt"] = json!(attempt);
            job["attempts"]["service"]=json!(attempt);
            job["state"] = json!("retrying");
            job["sent_at"] = json!(sent);
            job["acknowledged"] = json!(false);
            s["phase"] = json!("working");
            s["provider_error"] = Value::Null;
            s["recovery_user_at"] = json!(sent + 1.0);
            s["recovery_user_text"] = json!(CONTINUATION);
            job = advance(job, &s, &on(), sent + 2.0);
            assert_eq!(job["acknowledged"], true);
            s["phase"] = json!("tool");
            job = advance(job, &s, &on(), sent + 10.0);
            assert_eq!(job["attempt"], attempt);
            s["phase"] = json!("error");
            s["provider_error"] = json!({"source":"native","message_id":format!("error-{attempt}"),"at":sent+20.0,"detail":"503 unavailable"});
            job = advance(job, &s, &on(), sent + 21.0);
            assert_eq!(
                job["state"],
                if attempt == 4 { "exhausted" } else { "waiting" }
            );
        }
        assert_eq!(advance(job.clone(), &s, &on(), 99999.0), job);
    }
    #[test]
    fn new_input_cancellation_success_and_uncertain_delivery_are_terminal() {
        let mut s = snapshot("codex");
        let original = advance(Value::Null, &s, &on(), 11.0);
        let cancelled = advance(original.clone(), &s, &Policy::default(), 12.0);
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(advance(cancelled.clone(), &s, &on(), 1000.0), cancelled);
        s["recovery_user_at"] = json!(12.0);
        s["recovery_user_text"] = json!("new task");
        assert_eq!(
            advance(original.clone(), &s, &on(), 13.0)["state"],
            "cancelled"
        );
        s = snapshot("codex");
        s["phase"] = json!("approval");
        assert_eq!(
            advance(original.clone(), &s, &on(), 13.0)["state"],
            "cancelled"
        );
        s = snapshot("codex");
        let mut sent = original.clone();
        sent["state"] = json!("retrying");
        sent["sent_at"] = json!(12.0);
        assert_eq!(advance(sent.clone(), &s, &on(), 43.0)["state"], "uncertain");
        s["recovery_success_at"] = json!(20.0);
        s["recovery_user_at"] = json!(13.0);
        s["recovery_user_text"] = json!(CONTINUATION);
        assert_eq!(advance(sent, &s, &on(), 21.0)["state"], "succeeded");
        s = snapshot("codex");
        s["run_id"] = json!("new-run");
        s["provider_error"] = Value::Null;
        assert!(advance(original, &s, &on(), 30.0).is_null());
    }
    #[test]
    fn failures_while_the_agent_keeps_working_never_restart_the_loop() {
        let mut s = snapshot("claude");
        s["provider_error"]["detail"] = json!("429 Too many requests");
        let original = advance(Value::Null, &s, &on(), 11.0);
        let mut working = s.clone();
        working["phase"] = json!("tool");
        working["provider_error"] = Value::Null;
        // Before any continuation, the agent's own progress still ends the episode.
        assert_eq!(advance(original.clone(), &working, &on(), 12.0)["reason"], "The failed turn is no longer current");
        let mut job = original;
        job["state"] = json!("retrying");
        job["attempt"] = json!(1);
        job["attempts"]["service"] = json!(0);
        job["attempts"]["rate_limit"] = json!(1);
        job["sent_at"] = json!(30.0);
        job["request_id"] = json!("request");
        job["acknowledged"] = json!(false);
        s["phase"] = json!("working");
        s["provider_error"] = Value::Null;
        s["recovery_user_at"] = json!(31.0);
        s["recovery_user_text"] = json!(CONTINUATION);
        job = advance(job, &s, &on(), 31.5);
        assert_eq!((&job["state"], &job["acknowledged"]), (&json!("retrying"), &json!(true)));
        // A rate-limit hook right after the agent accepted the continuation.
        s["phase"] = json!("error");
        s["provider_error"] = json!({"source":"provider_hook","message_id":"limit-2","at":32.0,"detail":"429 Too many requests"});
        job = advance(job, &s, &on(), 32.5);
        assert_eq!((&job["state"], &job["class_attempt"]), (&json!("waiting"), &json!(1)));
        for phase in ["tool", "working", "compacting"] {
            let mut busy = s.clone();
            busy["phase"] = json!(phase);
            busy["provider_error"] = Value::Null;
            // Even after its retry is due, a working agent gets no continuation.
            let resumed = advance(job.clone(), &busy, &on(), 1000.0);
            assert_eq!(resumed["state"], "retrying", "{phase}");
            for key in ["id", "attempt", "attempts", "class_attempt", "acknowledged", "sent_at", "request_id", "failure_id"] {
                assert_eq!(resumed[key], job[key], "{key}");
            }
            assert_eq!(advance(resumed.clone(), &busy, &on(), 2000.0), resumed);
        }
        working = s.clone();
        working["phase"] = json!("tool");
        working["provider_error"] = Value::Null;
        job = advance(job, &working, &on(), 40.0);
        let mut done = working.clone();
        done["phase"] = json!("idle");
        done["recovery_success_at"] = json!(50.0);
        let finished = advance(job.clone(), &done, &on(), 51.0);
        assert_eq!((&finished["state"], &finished["attempt"]), (&json!("succeeded"), &json!(1)));
        // A later failure that ends the turn continues the same budget.
        let mut failed = working;
        failed["phase"] = json!("error");
        failed["provider_error"] = json!({"source":"provider_hook","message_id":"limit-3","at":60.0,"detail":"429 Too many requests"});
        let next = advance(job, &failed, &on(), 61.0);
        assert_eq!((&next["state"], &next["attempt"], &next["class_attempt"]), (&json!("waiting"), &json!(1), &json!(1)));
    }
    #[test]
    fn an_accepted_continuation_releases_the_account_once_the_agent_progresses() {
        let mut s = snapshot("claude");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        assert!(!occupies(&job, &s));
        job["state"] = json!("dispatching");
        assert!(occupies(&job, &s));
        job["state"] = json!("retrying");
        job["sent_at"] = json!(20.0);
        job["acknowledged"] = json!(false);
        s["phase"] = json!("working");
        s["provider_error"] = Value::Null;
        s["provider_status_at"] = json!(25.0);
        assert!(occupies(&job, &s), "delivery is unconfirmed");
        job["acknowledged"] = json!(true);
        job["user_at"] = json!(21.0);
        s["provider_status_at"] = json!(21.0);
        assert!(occupies(&job, &s), "the agent accepted it but nothing followed yet");
        s["provider_status_at"] = json!(21.5);
        assert!(!occupies(&job, &s), "the provider serves the account again");
        s["phase"] = json!("error");
        assert!(occupies(&job, &s));
        for state in ["waiting", "succeeded", "cancelled", "uncertain", "blocked", "exhausted"] {
            job["state"] = json!(state);
            assert!(!occupies(&job, &s), "{state}");
        }
    }
    #[test]
    fn a_resumed_turn_is_marked_without_ending_the_episode_or_its_budget() {
        let mut s = snapshot("codex");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        job["state"] = json!("retrying");
        job["attempt"] = json!(1);
        job["attempts"]["service"] = json!(1);
        job["class_attempt"] = json!(1);
        job["sent_at"] = json!(20.0);
        job["acknowledged"] = json!(true);
        job["user_at"] = json!(21.0);
        s["phase"] = json!("tool");
        s["provider_error"] = Value::Null;
        s["recovery_user_at"] = json!(21.0);
        s["turn_started"] = json!(21.0);
        s["provider_status_at"] = json!(21.0);
        assert!(advance(job.clone(), &s, &on(), 22.0)["resumed_at"].is_null(), "nothing followed the continuation yet");
        s["provider_status_at"] = json!(25.0);
        let resumed = advance(job.clone(), &s, &on(), 26.0);
        assert_eq!((&resumed["state"], &resumed["resumed_at"]), (&json!("retrying"), &json!(25.0)));
        for key in ["id", "attempt", "attempts", "class_attempt", "acknowledged", "sent_at", "user_at", "failure_id"] {
            assert_eq!(resumed[key], job[key], "{key}");
        }
        // Later progress keeps the first resumption, so the stored job stays unchanged.
        s["provider_status_at"] = json!(90.0);
        assert_eq!(advance(resumed.clone(), &s, &on(), 91.0), resumed);
        let mut unconfirmed = job.clone();
        unconfirmed["acknowledged"] = json!(false);
        assert!(advance(unconfirmed, &s, &on(), 26.0)["resumed_at"].is_null());
        // A further failure in the same turn reschedules on the same budget and is no longer resumed.
        let mut failed = s.clone();
        failed["phase"] = json!("error");
        failed["provider_error"] = json!({"source":"native","message_id":"error-2","at":100.0,"detail":"API Error: 529 Overloaded"});
        let next = advance(resumed.clone(), &failed, &on(), 101.0);
        assert_eq!((&next["state"], &next["class_attempt"]), (&json!("waiting"), &json!(1)));
        assert!(next.get("resumed_at").is_none());
        let mut done = s;
        done["phase"] = json!("idle");
        done["recovery_success_at"] = json!(120.0);
        assert_eq!(advance(resumed, &done, &on(), 121.0)["state"], "succeeded");
    }
    #[test]
    fn a_turn_start_after_the_continuation_is_not_provider_progress() {
        // Kimi reports TurnStarted (and TaskStarted) right after the
        // continuation's UserPromptSubmit, before the provider answers.
        let mut s = snapshot("kimi");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        job["state"] = json!("retrying");
        job["attempt"] = json!(1);
        job["sent_at"] = json!(20.0);
        job["acknowledged"] = json!(true);
        job["user_at"] = json!(21.0);
        s["phase"] = json!("working");
        s["provider_error"] = Value::Null;
        s["turn_started"] = json!(21.2);
        s["provider_status_at"] = json!(21.2);
        assert!(occupies(&job, &s), "the turn started but the provider has not served it");
        s["provider_status_at"] = json!(22.0);
        assert!(!occupies(&job, &s), "a later tool call shows the provider serves the account");
    }
    #[test]
    fn late_acknowledgement_cannot_restart_an_uncertain_episode() {
        let mut s = snapshot("claude");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        job["state"] = json!("retrying");
        job["sent_at"] = json!(12.0);
        job["attempt"] = json!(1);
        job = advance(job, &s, &on(), 43.0);
        assert_eq!(job["state"], "uncertain");
        s["recovery_user_at"] = json!(13.0);
        s["recovery_user_text"] = json!(CONTINUATION);
        s["provider_error"]["at"] = json!(45.0);
        s["provider_error"]["message_id"] = json!("late-error");
        assert_eq!(advance(job.clone(), &s, &on(), 46.0), job);
        s["recovery_user_at"] = json!(47.0);
        s["recovery_user_text"] = json!("Please try a new task");
        s["provider_error"]["at"] = json!(50.0);
        let fresh = advance(job.clone(), &s, &on(), 51.0);
        assert_ne!(fresh["id"], job["id"]);
        assert_eq!(fresh["attempt"], 0);
    }

    #[test]
    fn a_teams_request_resumes_only_the_session_limit_class_without_the_shared_policy() {
        let off = Policy::default();
        assert!(!off.enabled);
        // H1: the team setting turns on the session-limit wait for one episode
        // even when the shared policy is off; the other classes stay off.
        let requested = effective_policy(&off, true);
        assert!(requested.enabled && requested.session_limit);
        assert!(!requested.service && !requested.rate_limit && !requested.network);
        assert_eq!(requested.session_limit_mode, "reset");
        assert_eq!(requested.enabled_at, 0.0);
        // Without a request the shared policy decides.
        assert!(!effective_policy(&off, false).enabled);
        // When the shared policy is on, only the limit class changes: it waits
        // for the reset even if the policy retried on an interval.
        let on = Policy { enabled: true, session_limit_mode: "interval".into(), ..Default::default() };
        let requested = effective_policy(&on, true);
        assert!(requested.session_limit && requested.session_limit_mode == "reset");
        assert!(requested.service && requested.rate_limit && requested.network);
        // A request names a session and its run: a rebuilt session is not it.
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("claude/x/dev".to_owned(), json!({"run_id":"r2","at":now()}));
        assert!(override_for(&overrides, &json!({"name":"claude/x/dev","run_id":"r2"})));
        assert!(!override_for(&overrides, &json!({"name":"claude/x/dev","run_id":"r1"})));
        assert!(!override_for(&overrides, &json!({"name":"claude/x/other","run_id":"r2"})));
    }

    fn submitted_codex_continuation() -> (Value, Value) {
        use sha2::{Digest, Sha256};
        let s = snapshot("codex");
        let mut job = advance(Value::Null, &s, &on(), 11.0);
        job["state"] = json!("retrying");
        job["sent_at"] = json!(20.0);
        job["attempt"] = json!(1);
        job["attempts"]["service"] = json!(1);
        job["class_attempt"] = json!(1);
        job["acknowledged"] = json!(false);
        job["request_id"] = json!("11111111-1111-4111-8111-111111111111");
        let request = json!({"request_id":job["request_id"],"text":CONTINUATION,
            "expected_run_id":s["run_id"],"expected_conversation_id":s["conversation_id"]});
        job["receipt"] = json!({"request_id":job["request_id"],"name":s["name"],
            "run_id":s["run_id"],"conversation_id":s["conversation_id"],
            "status":"submitted","transport":"terminal","first_message":false,
            "submitted_at":20.25,"text":CONTINUATION,"submitted_text":CONTINUATION,
            "attachments":[],"digest":format!("{:x}",Sha256::digest(request.to_string().as_bytes()))});
        (job, s)
    }

    #[test]
    fn a_submitted_codex_continuation_waits_for_the_native_ack_without_another_attempt() {
        let (job, mut s) = submitted_codex_continuation();
        for time in [51.0, 100.0, 400.0] {
            let pending = advance(job.clone(), &s, &on(), time);
            assert_eq!(pending["state"], "retrying", "a submitted continuation is still awaiting its native acknowledgement");
            assert_eq!(pending, job, "waiting must not change the request or consume another attempt");
            assert!(occupies(&pending, &s), "the same account remains serialized while waiting");
            assert!(!resumed(&pending, &s));
        }
        // The authoritative main-agent input fields, not transcript messages.
        s["recovery_user_at"] = json!(401.0); // 381 seconds after dispatch.
        s["recovery_user_text"] = json!(CONTINUATION);
        s["turn_started"] = json!(401.0);
        s["phase"] = json!("working");
        s["provider_error"] = Value::Null;
        s["provider_status_at"] = json!(401.0);
        let accepted = advance(job.clone(), &s, &on(), 401.1);
        assert_eq!(accepted["acknowledged"], true);
        assert_eq!(accepted["user_at"], 401.0);
        assert!(occupies(&accepted, &s), "a turn-start alone is not provider progress");
        let mut done = s.clone(); done["phase"] = json!("idle");
        done["recovery_success_at"] = json!(402.0);
        assert_eq!(advance(accepted.clone(), &done, &on(), 403.0)["state"], "succeeded");
        s["phase"] = json!("error");
        s["provider_error"] = json!({"source":"native","message_id":"error-2","at":402.0,
            "error_kind":"capacity","detail":"Selected model is at capacity. Please try a different model."});
        let following = advance(accepted, &s, &on(), 403.0);
        assert_eq!(following["state"], "waiting");
        for key in ["id", "request_id", "receipt", "attempt", "attempts", "class_attempt", "schedules"] {
            assert_eq!(following[key], job[key], "preserve {key} across the later failure");
        }
        assert!(seconds(&following, "due_at") >= 433.0, "use the second delay from the existing consumed budget");
    }

    #[test]
    fn submitted_codex_ack_wait_expires_on_the_original_finite_deadline() {
        let (job, mut s) = submitted_codex_continuation();
        assert_eq!(advance(job.clone(), &s, &on(), 620.0)["state"], "retrying");
        let expired = advance(job.clone(), &s, &on(), 620.1);
        assert_eq!(expired["state"], "uncertain");
        assert!(!occupies(&expired, &s), "expiry releases the account for its other sessions");
        assert_eq!(expired["attempt"], job["attempt"]);
        s["recovery_user_at"] = json!(650.0);
        s["recovery_user_text"] = json!(CONTINUATION);
        s["provider_error"]["at"] = json!(660.0);
        s["provider_error"]["message_id"] = json!("too-late");
        assert_eq!(advance(expired.clone(), &s, &on(), 661.0), expired);
        let without_intervening_poll = advance(job.clone(), &s, &on(), 661.0);
        assert_eq!(without_intervening_poll["state"], "uncertain");
        assert_eq!(without_intervening_poll["acknowledged"], false);
        assert_eq!(without_intervening_poll["attempt"], job["attempt"]);
    }

    #[test]
    fn unproven_codex_receipts_and_other_providers_keep_the_thirty_second_cutoff() {
        let (job, s) = submitted_codex_continuation();
        for (key, value) in [
            ("request_id", json!("22222222-2222-4222-8222-222222222222")),
            ("name", json!("other")), ("run_id", json!("other")), ("conversation_id", json!("other")),
            ("status", json!("in_progress")), ("status", json!("confirmed")),
            ("transport", json!("other")), ("first_message", json!(true)),
            ("text", json!("other")), ("submitted_text", json!("other")),
            ("attachments", json!([{}])), ("digest", json!("wrong")),
            ("submitted_at", json!(0.0)), ("submitted_at", json!(19.0)),
            ("submitted_at", json!(52.0)), ("submitted_at", json!("20.25")),
        ] {
            let mut changed = job.clone(); changed["receipt"][key] = value;
            assert_eq!(advance(changed, &s, &on(), 51.0)["state"], "uncertain", "reject receipt {key}");
        }
        for key in ["receipt", "request_id"] {
            let mut changed = job.clone(); changed.as_object_mut().unwrap().remove(key);
            assert_eq!(advance(changed, &s, &on(), 51.0)["state"], "uncertain", "reject missing {key}");
        }
        let mut changed = job.clone(); changed["started_at"] = json!(21.0);
        assert_eq!(advance(changed, &s, &on(), 51.0)["state"], "uncertain", "dispatch must follow episode creation");
        for request_id in ["not-a-uuid", "00000000-0000-0000-0000-000000000000", "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"] {
            use sha2::{Digest, Sha256};
            let mut changed = job.clone(); changed["request_id"] = json!(request_id);
            changed["receipt"]["request_id"] = json!(request_id);
            let request = json!({"request_id":request_id,"text":CONTINUATION,
                "expected_run_id":s["run_id"],"expected_conversation_id":s["conversation_id"]});
            changed["receipt"]["digest"] = json!(format!("{:x}",Sha256::digest(request.to_string().as_bytes())));
            assert_eq!(advance(changed, &s, &on(), 51.0)["state"], "uncertain", "reject noncanonical request identity");
        }
        for agent in ["claude", "kimi", "dsh"] {
            let mut changed = job.clone(); let mut snapshot = s.clone();
            changed["agent"] = json!(agent); snapshot["agent"] = json!(agent);
            assert_eq!(advance(changed, &snapshot, &on(), 51.0)["state"], "uncertain", "retain {agent} cutoff");
        }
    }

    #[test]
    fn pending_codex_ack_keeps_input_policy_identity_and_attention_cancellation() {
        let (job, s) = submitted_codex_continuation();
        for (key, value) in [
            ("recovery_user_at", json!(30.0)), ("recovery_cancel_at", json!(30.0)),
            ("phase", json!("approval")), ("phase", json!("input")),
            ("process_state", json!("exited")), ("run_id", json!("other")),
            ("conversation_id", json!("other")), ("model", json!("other")),
            ("account_id", json!("other")), ("host_generation", json!("other")),
        ] {
            let mut changed = s.clone(); changed[key] = value;
            let cancelled = advance(job.clone(), &changed, &on(), 43.0);
            assert_eq!(cancelled["state"], "cancelled", "keep {key} boundary");
            assert_eq!(cancelled["attempt"], job["attempt"]);
            let mut late = s.clone(); late["recovery_user_at"] = json!(401.0);
            late["recovery_user_text"] = json!(CONTINUATION);
            assert_eq!(advance(cancelled.clone(), &late, &on(), 402.0)["state"], "cancelled");
        }
        let mut disabled = on(); disabled.enabled = false;
        let cancelled = advance(job.clone(), &s, &disabled, 43.0);
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(advance(cancelled.clone(), &s, &on(), 44.0), cancelled);
    }

    #[test]
    fn pending_codex_receipt_never_acknowledges_a_new_error_or_transcript_text() {
        let (job, mut s) = submitted_codex_continuation();
        s["provider_messages"] = json!([{"type":"UserPromptSubmit","detail":CONTINUATION,"at":40.0}]);
        let pending = advance(job.clone(), &s, &on(), 43.0);
        assert_eq!(pending, job);
        assert_eq!(pending["acknowledged"], false);
        s["provider_error"]["at"] = json!(44.0);
        s["provider_error"]["message_id"] = json!("unacknowledged-new-error");
        let stopped = advance(pending, &s, &on(), 45.0);
        assert_eq!(stopped["state"], "uncertain");
        assert_eq!(stopped["attempt"], job["attempt"]);
        assert_eq!(stopped["request_id"], job["request_id"]);
    }

    #[test]
    fn legacy_uncertain_and_crashed_dispatch_do_not_gain_a_new_ack_window() {
        let (job, mut s) = submitted_codex_continuation();
        s["recovery_user_at"] = json!(401.0);
        s["recovery_user_text"] = json!(CONTINUATION);
        s["provider_error"]["at"] = json!(402.0);
        s["provider_error"]["message_id"] = json!("after-interruption");
        for reason in ["Delivery was not confirmed; inspect the native agent",
            "Recovery worker restarted during delivery; inspect the agent",
            "Native delivery acknowledgement is missing"] {
            let mut stopped = job.clone(); stopped["state"] = json!("uncertain");
            stopped["reason"] = json!(reason);
            assert_eq!(advance(stopped.clone(), &s, &on(), 403.0), stopped);
        }
    }
}
