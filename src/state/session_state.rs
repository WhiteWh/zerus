//! WhiteWh fork: the session states (U30; the design is SESSION-STATES.md in
//! the UI handoff). Every listed or inspected session carries `session_state`
//! (working, build, user, deps, cooling, cold or stopped; clients show offline
//! for a machine that does not answer), `state_reason` (what it waits for,
//! with its source), `build` (the build Zerus runs for it, or null),
//! `last_active` and `cache_ttl`. A build started through `hgs run` or
//! `hgs team run` is work: the session is Building even while it is idle.
//!
//! A job the session started makes it wait only while it waits for it: a tool
//! call past the threshold, subagents in flight, or a job under a declared
//! budget (C12). A job that runs on after the turn ended with nothing waiting
//! on it, or past its budget, is a service: `process_work` lists it under
//! `services` and no longer counts it, so it neither holds a team's capture
//! gate nor defers the reply badge.
use super::{now, storage, teams};
use super::journal::clipped;
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// Jobs listed per session in `process_work.jobs`.
pub(crate) const MAX_JOBS: usize = 8;
/// A tool call running this long makes the session wait for it.
pub(crate) const LONG_OPERATION_SECONDS: f64 = 30.0;
/// A provider's prompt cache lifetime when nothing better is known.
const DEFAULT_TTL: u64 = 300;
/// Teams read per listing; the store's status index bounds the read.
const TEAMS_READ: i64 = 32;
/// An observed TTL older than this is not evidence for the account any more.
const OBSERVED_FOR: f64 = 7.0 * 86400.0;
const SUBAGENT_TOOLS: [&str; 3] = ["Task", "Agent", ""];

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// Decorates listing rows or one inspection with the state fields.
pub(super) fn decorate(sessions: &mut [Value]) {
    if sessions.is_empty() {
        return;
    }
    let at = now();
    let waiting = team_reasons(at);
    let builds = super::builds::running(at);
    let observed = observed_ttls();
    let mut accounts = HashMap::new();
    // Task 97: the owner queue's alerts for the phone, a team's on its lead's
    // row; read once per listing.
    let leads: Vec<(String, String, String)> =
        waiting.iter().filter(|w| w.lead).map(|w| (w.team.clone(), String::new(), w.name.clone())).collect();
    let deferred = teams::owner_queue::mobile_items(&leads);
    // Task 352: a session moved to another agent names the other session.
    super::switch_provider::decorate(sessions);
    for session in sessions.iter_mut() {
        let team = waiting.iter().find(|w| {
            w.name == text(session, "name")
                && (w.conversation_id.is_empty() || w.conversation_id == text(session, "conversation_id"))
        });
        classify_jobs(session, team.and_then(|team| team.budget.as_ref()));
        session["build"] = build_of(session, &builds).or_else(|| team.and_then(|team| team.build.clone())).unwrap_or(Value::Null);
        let key = format!("{}\n{}", text(session, "agent"), text(session, "account_home"));
        let ttl = accounts.entry(key).or_insert_with(|| cache_ttl(session, &observed)).clone();
        apply(session, ttl, team, at);
        for alert in team.map(|team| team.alerts.as_slice()).unwrap_or_default() {
            add_alert(session, alert);
        }
        let name = text(session, "name").to_owned();
        for (_, _, alert) in deferred.iter().filter(|(host, row, _)| host.is_empty() && *row == name) {
            add_alert(session, alert);
        }
    }
}

/// Task 95: the stalled lead's phone alert joins the row's `mobile_attention`
/// (the relay pushes a new item once). A row whose own question already has an
/// attention signature mixes the alert into it, so a read mark of that
/// question never hides the alert; a null signature is never suppressed and
/// stays null.
pub(crate) fn add_alert(session: &mut Value, alert: &Value) {
    use sha2::{Digest, Sha256};
    let mut items = session["mobile_attention"].as_array().cloned().unwrap_or_default();
    if items.iter().any(|item| item["question_id"] == alert["question_id"]) {
        return;
    }
    items.push(alert.clone());
    session["mobile_attention"] = json!(items);
    if let Some(signature) = session["attention_signature"].as_str() {
        let mixed = json!([signature, alert["question_id"], alert["question_hash"]]).to_string();
        session["attention_signature"] = json!(format!("{:x}", Sha256::digest(mixed.as_bytes())));
    }
}

fn apply(session: &mut Value, mut ttl: Value, team: Option<&teams::waiting::Waiting>, at: f64) {
    if session["kept_warm_at"].as_f64().is_some_and(|warmed| warmed > 0.0) {
        ttl["kept_warm"] = json!(true);
    }
    session["cache_ttl"] = ttl;
    session["last_active"] = json!(last_active(session));
    let (state, reason) = compute(session, team.map(|team| (team.lead, team.reason.as_ref())), at);
    session["session_state"] = json!(state);
    session["state_reason"] = reason.unwrap_or(Value::Null);
    if let Some(team) = team {
        session["team_id"] = json!(team.team);
        session["team_lead"] = json!(team.lead);
    }
}

/// U30: the oldest `hgs run` build the session started that still runs.
fn build_of(session: &Value, builds: &[Value]) -> Option<Value> {
    let run = text(session, "run_id");
    builds
        .iter()
        .filter(|claim| !run.is_empty() && claim["session"]["run_id"] == run && claim["session"]["name"] == session["name"])
        .min_by(|a, b| a["started"].as_f64().unwrap_or(0.0).total_cmp(&b["started"].as_f64().unwrap_or(0.0)))
        .map(|claim| json!({"source":"hgs_run","request_id":claim["request_id"],"label":claim["label"],
            "command":claim["command"],"since":claim["started"]}))
}

/// Task 117: the team store's read on every listing and inspection waits at
/// most this long for a writer. The installed rollback-journal store made
/// readers wait up to 10 s behind a team write, past the desktop's 5 s
/// inspect limit ("Could not refresh activity: timed out").
const TEAM_READ_WAIT: std::time::Duration = std::time::Duration::from_millis(300);
/// A busy store answers with the last good reasons, at most this old.
const TEAM_CACHE_SECONDS: f64 = 120.0;
/// The last team read's milliseconds, for the inspection's phase timings.
pub(crate) static TEAM_READ_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn team_cache_path() -> std::path::PathBuf {
    teams::store::path().with_file_name("waiting-cache.json")
}

fn team_reasons(at: f64) -> Vec<teams::waiting::Waiting> {
    let started = std::time::Instant::now();
    let reasons = team_reasons_at(&teams::store::path(), &team_cache_path(), TEAM_READ_WAIT, at);
    TEAM_READ_MS.store(started.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    reasons
}

/// The reasons from the store at `store`, or, when it stays busy past `wait`,
/// the last good ones saved at `cache` within `TEAM_CACHE_SECONDS`.
pub(crate) fn team_reasons_at(store: &Path, cache: &Path, wait: std::time::Duration, at: f64) -> Vec<teams::waiting::Waiting> {
    if !store.exists() {
        return Vec::new();
    }
    let read = teams::store::Store::open_at(store, false)
        .and_then(|found| {
            found.busy_timeout(wait)?;
            found.active_teams(TEAMS_READ)
        });
    match read {
        Ok(found) => {
            let reasons = teams::waiting::reasons(&found, at);
            let _ = super::storage::atomic_private(cache, &json!({"at":at,"reasons":reasons}).to_string());
            reasons
        }
        Err(_) => std::fs::read(cache)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|saved| at - saved["at"].as_f64().unwrap_or(0.0) <= TEAM_CACHE_SECONDS)
            .and_then(|saved| serde_json::from_value(saved["reasons"].clone()).ok())
            .unwrap_or_default(),
    }
}

/// The last native event: the cache's last use, a keepalive turn included.
/// A session without one counts from its creation.
fn last_active(session: &Value) -> f64 {
    let last = ["last_event_at", "reply_at", "kept_warm_at", "turn_started"]
        .iter()
        .filter_map(|key| session[*key].as_f64())
        .fold(0.0_f64, f64::max);
    if last > 0.0 { last } else { session["created"].as_f64().unwrap_or(0.0) }
}

/// Moves the jobs nothing waits for to `services` (U30). Only an idle
/// session's sample counts jobs; a declared budget within its minutes keeps
/// them work the session waits for.
pub(crate) fn classify_jobs(session: &mut Value, budget: Option<&Value>) {
    let work = &mut session["process_work"];
    if work["count"].as_u64().unwrap_or(0) == 0 || budget.is_some() {
        return;
    }
    let jobs = work["jobs"].as_array().cloned().unwrap_or_default();
    let mut services = work["services"].as_array().cloned().unwrap_or_default();
    for job in &jobs {
        let command = text(job, "command");
        services.push(json!({"name":program(command),"command":command,"since":job["started_at"],"kind":"job"}));
    }
    if jobs.is_empty() && !text(work, "command").is_empty() {
        // An older sample names only its first job.
        let command = text(work, "command").to_owned();
        services.push(json!({"name":program(&command),"command":command,"since":work["started_at"],"kind":"job"}));
    }
    work["services"] = json!(services);
    work["unwaited_count"] = work["count"].clone();
    work["count"] = json!(0);
    work["started_at"] = Value::Null;
    work["command"] = json!("");
    work["jobs"] = json!([]);
}

/// "npm" for "npm run dev", "vite" for "/usr/bin/vite --port 5173".
fn program(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("");
    let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    clipped(&json!(name), 40)
}

/// The order of SESSION-STATES.md section 4; `team` is (lead, reason).
pub(crate) fn compute(session: &Value, team: Option<(bool, Option<&Value>)>, at: f64) -> (&'static str, Option<Value>) {
    let state = text(session, "state");
    let activity = text(session, "activity");
    let phase = text(session, "phase");
    let stopped = matches!(state, "stopped" | "paused" | "archived") || text(session, "process_state") == "exited";
    let limit = usage_limit(session);
    // Working, unless the turn waits for subagents or a long tool call.
    if !stopped && activity == "busy" && !matches!(phase, "approval" | "input" | "error") {
        let tool = text(session, "current_tool");
        let active = session["subagent_active_count"].as_u64().unwrap_or(0);
        if active > 0 && SUBAGENT_TOOLS.contains(&tool) {
            return ("deps", Some(subagents(session, active)));
        }
        let started = session["tool_started"].as_f64().unwrap_or(0.0);
        if !tool.is_empty() && started > 0.0 && at - started >= LONG_OPERATION_SECONDS {
            let detail = text(session, "tool_detail");
            let command = clipped(&json!(if detail.is_empty() { tool } else { detail }), 160);
            return ("deps", Some(json!({"kind":"long_operation",
                "text":format!("Waiting for {command} \u{b7} long operation, {} so far", minutes(at - started)),
                "command":command,"tool":tool,"since":started})));
        }
        return ("working", None);
    }
    // Waiting for you: an approval, an input, an error, a reply to read.
    if state != "archived" {
        match phase {
            "approval" => return ("user", Some(json!({"kind":"approval","text":"Needs approval"}))),
            "input" => return ("user", Some(json!({"kind":"input","text":"Needs input"}))),
            "error" if limit.is_none() => {
                return ("user", Some(json!({"kind":"error","text":text(session, "activity_summary")})));
            }
            _ => {}
        }
    }
    // Building: a build or test Zerus runs for the session (hgs run, hgs team
    // run). Explicit attention comes first; the reply waits for the build.
    if !stopped && session["build"].is_object() {
        let build = &session["build"];
        let label = clipped(&build["label"], 80);
        let since = build["since"].as_f64().unwrap_or(0.0);
        let so_far = if since > 0.0 { format!(" \u{b7} {} so far", minutes(at - since)) } else { String::new() };
        return ("build", Some(json!({"kind":"build","text":format!("Running {label}{so_far}"),"label":label,
            "command":build["command"],"since":build["since"],"source":build["source"],"request_id":build["request_id"]})));
    }
    if state != "archived" {
        // A reply is deferred while the session waits for its own work.
        if unread_reply(session) && session["process_work"]["count"].as_u64().unwrap_or(0) == 0 {
            return ("user", Some(json!({"kind":"reply","text":"Response ready to read"})));
        }
    }
    if stopped {
        return ("stopped", None);
    }
    if let Some(limit) = limit {
        return ("deps", Some(limit));
    }
    if let Some((lead, Some(reason))) = team {
        let you = text(reason, "text").starts_with("Waiting for you");
        return (if lead && you { "user" } else { "deps" }, Some(reason.clone()));
    }
    if let Some(reason) = coordination(session) {
        return ("deps", Some(reason));
    }
    let work = &session["process_work"];
    if work["count"].as_u64().unwrap_or(0) > 0 {
        let started = work["started_at"].as_f64().unwrap_or(0.0);
        let command = text(work, "command");
        let so_far = if started > 0.0 { format!(", {} so far", minutes(at - started)) } else { String::new() };
        return ("deps", Some(json!({"kind":"job","text":format!("Waiting for {command} \u{b7} long operation{so_far}"),
            "command":command,"since":work["started_at"],"count":work["count"]})));
    }
    let ttl = session["cache_ttl"]["seconds"].as_f64().unwrap_or(DEFAULT_TTL as f64);
    if at - session["last_active"].as_f64().unwrap_or(0.0) < ttl {
        ("cooling", None)
    } else {
        ("cold", None)
    }
}

/// Task #13 (P2.2): the earliest moment after `at` at which a decorated row
/// (`session_state`, `state_reason`, `last_active`, `cache_ttl`) reads
/// otherwise by time alone: Cooling turns Cold when the cache TTL runs out, a
/// tool call becomes a long operation at 30 s, and a "so far" text moves to
/// its next minute. None when only an input can change it. `hgs watch`
/// derives the row again then instead of listing every row on a timer.
pub(crate) fn valid_until(session: &Value, at: f64) -> Option<f64> {
    let mut next: Option<f64> = None;
    let mut consider = |moment: f64| {
        if moment > at {
            next = Some(next.map_or(moment, |earlier: f64| earlier.min(moment)));
        }
    };
    match text(session, "session_state") {
        "working" => {
            let started = session["tool_started"].as_f64().unwrap_or(0.0);
            if !text(session, "current_tool").is_empty() && started > 0.0 {
                consider(started + LONG_OPERATION_SECONDS);
            }
        }
        "cooling" => {
            let ttl = session["cache_ttl"]["seconds"].as_f64().unwrap_or(DEFAULT_TTL as f64);
            consider(session["last_active"].as_f64().unwrap_or(0.0) + ttl);
        }
        _ => {}
    }
    let reason = &session["state_reason"];
    if matches!(text(reason, "kind"), "long_operation" | "build" | "job") {
        if let Some(since) = reason["since"].as_f64().filter(|since| *since > 0.0) {
            // `minutes` shows whole minutes: the text moves at the next one.
            let whole = ((at - since).max(0.0) / 60.0).floor();
            consider(since + (whole + 1.0) * 60.0);
        }
    }
    next
}

/// Task 115: a team member's session state from this contract, for
/// `hgs team status` and its member card. A native question or permission the
/// member waits on is Waiting for you even while the last tick still saw it
/// busy; Kimi answers its Edit and Write approvals only in its Terminal panel,
/// and its other permissions reach the lead. Otherwise the stored phase and
/// activity decide, so an idle member reads Cooling/Cold rather than busy.
pub(crate) fn member_state(member: &teams::model::Member) -> (&'static str, Option<Value>) {
    let live = member.seen.is_some_and(|seen| seen.live);
    let last = member.seen_event_at.or(member.seen.map(|seen| seen.at)).unwrap_or(0.0);
    let mut session = json!({
        "state": if live { "running" } else { "stopped" },
        "process_state": if live { "running" } else { "exited" },
        "activity": if member.seen_activity.is_empty() { "unknown" } else { member.seen_activity.as_str() },
        "phase": member.seen_phase,
        "current_tool": "",
        "last_active": last,
    });
    if let Some(asked) = member.pending.as_ref().or(member.terminal_question.as_ref()) {
        session["activity"] = json!("busy");
        session["phase"] = json!(if asked.approval { "approval" } else { "input" });
        return compute(&session, None, now());
    }
    // A native question phase is Waiting for you; the contract names it input.
    if member.seen_phase == "question" {
        session["phase"] = json!("input");
    }
    // DCB-svn report (2026-11-12): an error phase names the error, never "".
    if member.seen_phase == "error" {
        let detail = member.turn_failed.as_ref().map(|failure| failure.detail.as_str()).filter(|detail| !detail.is_empty());
        session["activity_summary"] = json!(detail.unwrap_or("Provider error"));
    }
    compute(&session, None, now())
}

fn subagents(session: &Value, active: u64) -> Value {
    let names: Vec<String> = session["subagent_previews"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|preview| preview["state"] == "working")
        .map(|preview| clipped(&preview["label"], 60))
        .filter(|name| !name.is_empty())
        .take(4)
        .collect();
    let noun = if active == 1 { "subagent" } else { "subagents" };
    let listed = if names.is_empty() { String::new() } else { format!(": {}", names.join(", ")) };
    json!({"kind":"subagents","text":format!("Waiting for {active} {noun}{listed}"),"count":active,"names":names})
}

/// "6m", "1h 05m", "under 1m": the elapsed part of a reason, whole minutes.
fn minutes(seconds: f64) -> String {
    let minutes = (seconds.max(0.0) / 60.0).floor() as u64;
    match minutes {
        0 => "under 1m".into(),
        1..=59 => format!("{minutes}m"),
        _ => format!("{}h {:02}m", minutes / 60, minutes % 60),
    }
}

fn usage_limit(session: &Value) -> Option<Value> {
    if text(session, "phase") != "error" {
        return None;
    }
    let error = &session["provider_error"];
    let kind = text(error, "error_kind");
    let reset = error["reset_at"].as_f64().filter(|at| *at > 0.0);
    if !matches!(kind, "session_limit" | "quota") && !(kind == "rate_limit" && reset.is_some()) {
        return None;
    }
    let clock = reset
        .and_then(|at| Local.timestamp_opt(at as i64, 0).single())
        .map(|time| format!(" \u{b7} {}", time.format("%H:%M")))
        .unwrap_or_default();
    Some(json!({"kind":"usage_limit","text":format!("Waiting for the usage limit to reset{clock}"),
        "error_kind":kind,"resets_at":reset}))
}

fn unread_reply(session: &Value) -> bool {
    let reply = text(session, "reply_id");
    // A muted session, such as a team member, answers its lead, not you. A
    // null mark was never read; without one (an inspection) it is unknown.
    !reply.is_empty()
        && session["notification_preferences"]["muted"] != true
        && session.get("read").is_some()
        && session["read"]["reply_id"].as_str() != Some(reply)
}

fn coordination(session: &Value) -> Option<Value> {
    let wait = &session["coordination_wait"];
    let count = wait["count"].as_u64().unwrap_or(0);
    if wait["status"] != "ok" || count == 0 {
        return None;
    }
    let item = &wait["items"][0];
    let producer = text(&item["producer"], "name");
    let producer = producer.splitn(3, '/').nth(2).unwrap_or(producer);
    let what = if item["state"] == "awaiting_result" { "result" } else { "acceptance" };
    let from = if producer.is_empty() { String::new() } else { format!(" from {producer}") };
    let label = text(item, "label");
    let label = if label.is_empty() { String::new() } else { format!(": {}", clipped(&json!(label), 120)) };
    let more = if count > 1 { format!(" (+{})", count - 1) } else { String::new() };
    Some(json!({"kind":"coordination","text":format!("Waiting for {what}{from}{label}{more}"),"count":count,
        "since":item["created_at"]}))
}

/// The prompt cache's lifetime for this session's agent and account: the TTL
/// its native usage last reported, else Claude's settings, else 5 minutes.
fn cache_ttl(session: &Value, observed: &Value) -> Value {
    let account = text(session, "account_id");
    let seen = &observed[account];
    if !account.is_empty()
        && seen["ttl_seconds"].as_u64().is_some_and(|ttl| ttl > 0)
        && now() - seen["observed_at"].as_f64().unwrap_or(0.0) < OBSERVED_FOR
    {
        return json!({"seconds":seen["ttl_seconds"],"source":"native_usage","kept_warm":false});
    }
    if text(session, "agent") == "claude" {
        if let Some(seconds) = claude_settings_ttl(session) {
            return json!({"seconds":seconds,"source":"settings","kept_warm":false});
        }
    }
    json!({"seconds":DEFAULT_TTL,"source":"default","kept_warm":false})
}

/// Claude Code's cache TTL switches in the account's settings `env`:
/// `FORCE_PROMPT_CACHING_5M` wins over `ENABLE_PROMPT_CACHING_1H` (and its
/// older `_BEDROCK` form).
fn claude_settings_ttl(session: &Value) -> Option<u64> {
    let home = text(session, "account_home");
    let profile = if home.is_empty() { crate::platform::home_dir()?.join(".claude") } else { Path::new(home).to_path_buf() };
    let settings: Value = std::fs::read(profile.join("settings.json")).ok()
        .and_then(|bytes| serde_json::from_slice(super::without_bom(&bytes)).ok())?;
    let on = |key: &str| matches!(settings["env"][key].as_str().map(str::trim), Some("1" | "true" | "yes"))
        || settings["env"][key] == true || settings["env"][key] == 1;
    if on("FORCE_PROMPT_CACHING_5M") {
        Some(300)
    } else if on("ENABLE_PROMPT_CACHING_1H") || on("ENABLE_PROMPT_CACHING_1H_BEDROCK") {
        Some(3600)
    } else {
        None
    }
}

fn observed_path() -> std::path::PathBuf {
    storage::root().join("cache_ttl.json")
}

fn observed_ttls() -> Value {
    std::fs::read(observed_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(json!({}))
}

/// Remembers the TTL an inspection's native usage reported for the account,
/// so the listing can use it without reading transcripts. Best effort.
pub(super) fn observe_ttl(account: &str, usage: &Value) {
    let Some(ttl) = usage["prompt_cache"]["ttl_seconds"].as_u64().filter(|ttl| *ttl > 0) else { return };
    if account.is_empty() || account.len() > 128 {
        return;
    }
    let mut known = observed_ttls();
    let at = now();
    if known[account]["ttl_seconds"].as_u64() == Some(ttl) && at - known[account]["observed_at"].as_f64().unwrap_or(0.0) < 3600.0 {
        return;
    }
    let Some(map) = known.as_object_mut() else { return };
    map.insert(account.to_owned(), json!({"ttl_seconds":ttl,"observed_at":at}));
    // Bounded: the newest accounts only.
    while map.len() > 64 {
        let oldest = map.iter().min_by(|a, b| a.1["observed_at"].as_f64().unwrap_or(0.0)
            .total_cmp(&b.1["observed_at"].as_f64().unwrap_or(0.0))).map(|(key, _)| key.clone());
        if let Some(oldest) = oldest { map.remove(&oldest); } else { break; }
    }
    let _ = storage::atomic(&observed_path(), &known.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task #13 (P2.2): until `valid_until` a row's state and reason text do
    /// not change by time alone, and at it they do; a row without a time
    /// transition has none.
    #[test]
    fn valid_until_is_the_next_time_driven_change() {
        let at = 1_000_000.0;
        let decorated = |mut session: Value| {
            let (state, reason) = compute(&session, None, at);
            session["session_state"] = json!(state);
            session["state_reason"] = reason.unwrap_or(Value::Null);
            session
        };
        let seen = |session: &Value, moment: f64| {
            let (state, reason) = compute(session, None, moment);
            (state, reason.map(|reason| reason["text"].clone()))
        };
        let ttl = json!({"seconds":300});
        let cases = [
            json!({"state":"running","activity":"busy","phase":"tool","current_tool":"Bash","tool_started":at - 12.5,"last_active":at,"cache_ttl":ttl}),
            json!({"state":"running","activity":"busy","phase":"tool","current_tool":"Bash","tool_detail":"cargo build","tool_started":at - 95.0,"last_active":at,"cache_ttl":ttl}),
            json!({"state":"running","activity":"idle","phase":"idle","build":{"label":"tests","since":at - 30.0},"last_active":at - 10.0,"cache_ttl":ttl}),
            json!({"state":"running","activity":"idle","phase":"idle","process_work":{"count":1,"command":"npm run dev","started_at":at - 200.0},"last_active":at - 10.0,"cache_ttl":ttl}),
            json!({"state":"running","activity":"idle","phase":"idle","last_active":at - 290.0,"cache_ttl":ttl}),
        ];
        for case in cases {
            let row = decorated(case.clone());
            let until = valid_until(&row, at).unwrap_or_else(|| panic!("no deadline for {row}"));
            let before = seen(&case, at);
            let mut moment = at;
            while moment < until {
                assert_eq!(seen(&case, moment), before, "{case} at +{}", moment - at);
                moment += 0.25;
            }
            assert_ne!(seen(&case, until), before, "{case} changes at +{}", until - at);
        }
        for quiet in [
            json!({"state":"stopped","last_active":at,"cache_ttl":ttl}),
            json!({"state":"running","activity":"idle","phase":"idle","last_active":at - 400.0,"cache_ttl":ttl}),
            json!({"state":"running","activity":"idle","phase":"approval","last_active":at,"cache_ttl":ttl}),
            json!({"state":"running","activity":"busy","phase":"thinking","last_active":at,"cache_ttl":ttl}),
        ] {
            assert_eq!(valid_until(&decorated(quiet.clone()), at), None, "{quiet}");
        }
    }

    const CONTRACT: &str = include_str!("../../mobile/android/app/src/test/resources/session-state-contract.json");

    #[test]
    fn the_shared_contract_follows_the_order_of_section_four() {
        let contract: Value = serde_json::from_str(CONTRACT).unwrap();
        assert_eq!(contract["long_operation_seconds"].as_f64(), Some(LONG_OPERATION_SECONDS));
        let at = contract["now"].as_f64().unwrap();
        for case in contract["cases"].as_array().unwrap() {
            let name = text(case, "name");
            let mut row = case["raw"].clone();
            classify_jobs(&mut row, case.get("budget"));
            let team = case.get("team").map(|team| (team["lead"] == true, Some(&team["reason"])));
            let (state, reason) = compute(&row, team, at);
            let expect = &case["expect"];
            assert_eq!(state, text(expect, "session_state"), "{name}");
            let reason = reason.unwrap_or(Value::Null);
            if let Some(kind) = expect.get("kind") {
                assert_eq!(&reason["kind"], kind, "{name}");
            } else {
                assert!(reason.is_null(), "{name}: {reason}");
            }
            if let Some(expected) = expect.get("text") {
                assert_eq!(&reason["text"], expected, "{name}");
            }
            if let Some(prefix) = expect["text_prefix"].as_str() {
                assert!(text(&reason, "text").starts_with(prefix), "{name}: {reason}");
            }
            if let Some(count) = expect.get("process_work_count") {
                assert_eq!(&row["process_work"]["count"], count, "{name}");
            }
            if let Some(names) = expect["services"].as_array() {
                let listed: Vec<&Value> = row["process_work"]["services"].as_array().unwrap().iter().map(|s| &s["name"]).collect();
                assert_eq!(listed, names.iter().collect::<Vec<_>>(), "{name}");
            }
            // A deps state always names what it waits for (section 7).
            if state == "deps" {
                assert!(text(&reason, "text").starts_with("Waiting for"), "{name}: {reason}");
            }
        }
    }

    #[test]
    fn a_job_moved_to_services_keeps_its_command_and_start() {
        let mut row = json!({"process_work":{"count":1,"started_at":5.0,"command":"/usr/bin/vite --port 5173",
            "jobs":[{"command":"/usr/bin/vite --port 5173","started_at":5.0}]}});
        classify_jobs(&mut row, None);
        assert_eq!(row["process_work"]["services"], json!([{"name":"vite","command":"/usr/bin/vite --port 5173","since":5.0,"kind":"job"}]));
        assert_eq!((row["process_work"]["count"].clone(), row["process_work"]["unwaited_count"].clone()), (json!(0), json!(1)));
        // An older sample without the job list still names its first job.
        let mut older = json!({"process_work":{"count":2,"started_at":5.0,"command":r"C:\tools\ngrok.exe http 80"}});
        classify_jobs(&mut older, None);
        assert_eq!(older["process_work"]["services"][0]["name"], "ngrok");
        // Nothing running, or an unavailable sample, stays as it was.
        for untouched in [json!({"process_work":{"count":0}}), json!({"process_work":{"count":null}})] {
            let mut row = untouched.clone();
            classify_jobs(&mut row, None);
            assert_eq!(row, untouched);
        }
    }

    #[test]
    fn durations_are_whole_minutes_and_the_reset_is_a_clock_time() {
        assert_eq!((minutes(29.0), minutes(60.0), minutes(361.0), minutes(3900.0)),
            ("under 1m".into(), "1m".into(), "6m".into(), "1h 05m".into()));
        let reset = Local.with_ymd_and_hms(2026, 10, 27, 1, 30, 0).single().unwrap().timestamp() as f64;
        let row = json!({"phase":"error","provider_error":{"error_kind":"session_limit","reset_at":reset}});
        assert_eq!(usage_limit(&row).unwrap()["text"], "Waiting for the usage limit to reset \u{b7} 01:30");
        let rate = json!({"phase":"error","provider_error":{"error_kind":"rate_limit"}});
        assert_eq!(usage_limit(&rate), None, "a rate limit without a reset is an error to see");
    }

    #[test]
    fn last_active_is_the_last_native_event_or_the_creation() {
        assert_eq!(last_active(&json!({"last_event_at":50.0,"reply_at":40.0,"kept_warm_at":70.0,"created":1.0})), 70.0);
        assert_eq!(last_active(&json!({"last_event_at":0,"created":9.0})), 9.0);
    }

    #[test]
    fn the_cache_ttl_comes_from_native_usage_then_claude_settings_then_the_default() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_str().unwrap();
        let session = json!({"agent":"claude","account_id":"acct","account_home":path});
        assert_eq!(cache_ttl(&session, &json!({})), json!({"seconds":300,"source":"default","kept_warm":false}));
        std::fs::write(home.path().join("settings.json"), r#"{"env":{"ENABLE_PROMPT_CACHING_1H":"1"}}"#).unwrap();
        assert_eq!(cache_ttl(&session, &json!({})), json!({"seconds":3600,"source":"settings","kept_warm":false}));
        std::fs::write(home.path().join("settings.json"), "\u{feff}{\"env\":{\"ENABLE_PROMPT_CACHING_1H\":\"1\",\"FORCE_PROMPT_CACHING_5M\":\"1\"}}").unwrap();
        assert_eq!(cache_ttl(&session, &json!({}))["seconds"], 300);
        let seen = json!({"acct":{"ttl_seconds":3600,"observed_at":now()}});
        assert_eq!(cache_ttl(&session, &seen), json!({"seconds":3600,"source":"native_usage","kept_warm":false}));
        let stale = json!({"acct":{"ttl_seconds":3600,"observed_at":now() - OBSERVED_FOR - 1.0}});
        assert_eq!(cache_ttl(&session, &stale)["source"], "settings");
        // Other agents keep the default until their own usage reports a TTL.
        assert_eq!(cache_ttl(&json!({"agent":"codex","account_home":path}), &json!({}))["source"], "default");
    }

    #[test]
    fn an_observed_ttl_is_remembered_per_account_for_the_listing() {
        let root = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(root.path(), || {
            observe_ttl("acct", &json!({"prompt_cache":{"ttl_seconds":3600}}));
            observe_ttl("other", &json!({"prompt_cache":{"status":"unknown"}}));
            let known = observed_ttls();
            assert_eq!(known["acct"]["ttl_seconds"], 3600);
            assert!(known.get("other").is_none());
        });
    }

    #[test]
    fn a_listing_reads_the_team_store_for_reasons_and_budgets() {
        use teams::model::{Budget, Member, MemberState, SessionRef, Task, TaskStatus, Team, TeamStatus};
        let member = |id: &str, role: &str| Member {
            id: id.into(),
            role: role.into(),
            agent: "claude".into(),
            state: MemberState::Active,
            session: Some(SessionRef { host: "h".into(), name: format!("claude/p/{id}"), run_id: "r".into(), conversation_id: format!("c-{id}") }),
            ..Default::default()
        };
        let mut builder = member("builder", "backend");
        builder.budget = Some(Budget { label: "release build".into(), minutes: 30, red_minutes: 60, declared_at: now() - 60.0, ..Default::default() });
        let team = Team {
            id: "t".into(),
            name: "T".into(),
            status: TeamStatus::Running,
            lead: "lead".into(),
            members: vec![member("lead", "lead"), member("web", "frontend"), builder],
            tasks: vec![
                Task { id: 1, title: "Build".into(), assignee: "builder".into(), status: TaskStatus::Accepted, phase: 1, ..Default::default() },
                Task { id: 2, title: "Page".into(), assignee: "web".into(), status: TaskStatus::Pending, phase: 1, ..Default::default() },
            ],
            ..Default::default()
        };
        let root = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(root.path(), || {
            teams::store::Store::open(true).unwrap()
                .create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
            let at = now();
            let job = |command: &str| json!({"count":1,"started_at":at - 600.0,"command":command,
                "jobs":[{"command":command,"started_at":at - 600.0}]});
            let row = |id: &str, work: Value| json!({"name":format!("claude/p/{id}"),"conversation_id":format!("c-{id}"),
                "agent":"codex","activity":"idle","phase":"idle","last_event_at":at - 30.0,"process_work":work});
            let mut rows = vec![row("lead", json!({"count":0})), row("web", job("npm run dev")),
                row("builder", job("cargo build --release")), row("stranger", job("npm run dev"))];
            decorate(&mut rows);
            let state = |row: &Value| (row["session_state"].clone(), row["state_reason"]["text"].clone());
            // The lead has nothing out and nothing to approve: its idle time decides.
            assert_eq!(state(&rows[0]), (json!("cooling"), Value::Null));
            assert_eq!(rows[0]["cache_ttl"], json!({"seconds":300,"source":"default","kept_warm":false}));
            assert_eq!(rows[0]["last_active"].as_f64(), Some(at - 30.0));
            // The member waits for its hand-out; its dev server is a service.
            assert_eq!(state(&rows[1]), (json!("deps"), json!("Waiting for Lead to hand out #2")));
            assert_eq!((rows[1]["process_work"]["count"].clone(), rows[1]["process_work"]["services"][0]["name"].clone()),
                (json!(0), json!("npm")));
            // A declared budget keeps the build a job the member waits for.
            assert_eq!(state(&rows[2]), (json!("deps"), json!("Waiting for cargo build --release \u{b7} long operation, 10m so far")));
            assert_eq!(rows[2]["process_work"]["count"], 1);
            // Outside a team the same dev server leaves the session Cooling.
            assert_eq!(state(&rows[3]), (json!("cooling"), Value::Null));
            assert_eq!(rows[3]["process_work"]["services"][0]["command"], "npm run dev");
        });
    }

    #[test]
    fn a_running_zerus_build_is_build_and_a_finished_one_falls_back_to_cooling() {
        let root = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(root.path(), || {
            let at = now();
            let id = "55555555-5555-4555-8555-555555555555";
            let folder = storage::root().join("builds");
            crate::platform::private_dir(&folder).unwrap();
            let claim = json!({"request_id":id,"label":"cargo test","command":"cargo test --locked","cwd":"/x",
                "session":{"name":"claude/p/dev","run_id":"r1","conversation_id":"c1"},"started":at - 5.0,"timeout_secs":600,
                "pid":std::process::id()});
            storage::atomic(&folder.join(format!("{id}.running.json")), &claim.to_string()).unwrap();
            let row = |run: &str| json!({"name":"claude/p/dev","run_id":run,"conversation_id":"c1","agent":"codex",
                "activity":"idle","phase":"idle","last_event_at":at - 30.0,"reply_id":"9:1","read":null,
                "process_work":{"count":1,"started_at":at - 60.0,"command":"npm run dev","jobs":[{"command":"npm run dev","started_at":at - 60.0}]}});
            let mut rows = vec![row("r1"), row("another-run")];
            decorate(&mut rows);
            // The build is work: Building, and the unread reply waits for it.
            assert_eq!(rows[0]["session_state"], "build");
            assert_eq!(rows[0]["state_reason"]["kind"], "build");
            assert_eq!(rows[0]["state_reason"]["text"], "Running cargo test \u{b7} under 1m so far");
            assert_eq!(rows[0]["build"]["request_id"], id);
            // The shell's own job beside it is still a service.
            assert_eq!((rows[0]["process_work"]["count"].clone(), rows[0]["process_work"]["services"][0]["name"].clone()),
                (json!(0), json!("npm")));
            // Another run of the same name is not the build's session.
            assert_eq!((rows[1]["session_state"].clone(), rows[1]["build"].clone()), (json!("user"), Value::Null));
            // The build ended: its record is no claim, and the session cools.
            std::fs::rename(folder.join(format!("{id}.running.json")), folder.join(format!("{id}.json"))).unwrap();
            let mut rows = vec![row("r1")];
            rows[0]["read"] = json!({"reply_id":"9:1"});
            decorate(&mut rows);
            assert_eq!((rows[0]["session_state"].clone(), rows[0]["build"].clone()), (json!("cooling"), Value::Null));
        });
    }

    #[test]
    fn a_keepalive_turn_marks_the_cache_kept_warm_until_it_expires() {
        let ttl = json!({"seconds":300,"source":"default","kept_warm":false});
        let mut row = json!({"activity":"idle","phase":"idle","kept_warm_at":100.0,"last_event_at":100.0,"process_work":{"count":0}});
        apply(&mut row, ttl.clone(), None, 200.0);
        assert_eq!((row["session_state"].clone(), row["cache_ttl"]["kept_warm"].clone(), row["last_active"].clone()),
            (json!("cooling"), json!(true), json!(100.0)));
        apply(&mut row, ttl, None, 400.0);
        assert_eq!((row["session_state"].clone(), row["state_reason"].clone()), (json!("cold"), Value::Null));
    }

    /// Task 95: a stalled lead's row carries the phone's alert item and the
    /// owner's reason; a member's row does not, and a row whose own question
    /// has a signature mixes the alert into it.
    #[test]
    fn a_stalled_lead_row_carries_the_phone_alert_and_the_owner_reason() {
        use teams::model::{LeadStall, Member, MemberState, SessionRef, Team, TeamStatus};
        let member = |id: &str| Member {
            id: id.into(),
            role: id.into(),
            agent: "claude".into(),
            state: MemberState::Active,
            session: Some(SessionRef { host: "h".into(), name: format!("claude/p/{id}"), run_id: "r".into(), conversation_id: format!("c-{id}") }),
            ..Default::default()
        };
        let stall = LeadStall { since: 100.0, reason: "rejected_tool".into(), items: 3, members: 2, oldest: 50.0, round: 2, notified_at: 3700.0, ..Default::default() };
        let team = Team {
            id: "t".into(),
            name: "T".into(),
            status: TeamStatus::Running,
            lead: "lead".into(),
            members: vec![member("lead"), member("web")],
            lead_stall: Some(stall),
            ..Default::default()
        };
        let root = tempfile::tempdir().unwrap();
        crate::state::with_test_state_root(root.path(), || {
            teams::store::Store::open(true).unwrap()
                .create("11111111-1111-4111-8111-111111111111", &json!({}), team, vec![]).unwrap();
            let at = now();
            let row = |id: &str| json!({"name":format!("claude/p/{id}"),"conversation_id":format!("c-{id}"),
                "agent":"claude","activity":"idle","phase":"interrupted","last_event_at":at - 30.0,"attention_signature":null});
            let mut rows = vec![row("lead"), row("web")];
            decorate(&mut rows);
            let alert = &rows[0]["mobile_attention"][0];
            assert_eq!(alert["kind"], "team_lead_stall");
            assert_eq!(alert["question_id"], "team-lead-stall:t:2");
            assert_eq!((alert["items"].clone(), alert["members"].clone()), (json!(3), json!(2)));
            assert!(alert["question_hash"].as_str().is_some_and(|hash| hash.len() == 64));
            // No question text leaves the computer, and a null signature stays null.
            assert!(alert.get("title").is_none() && alert.get("detail").is_none());
            assert!(rows[0]["attention_signature"].is_null());
            assert_eq!(rows[0]["session_state"], "user");
            assert_eq!(rows[0]["state_reason"]["kind"], "team_lead_stalled");
            assert!(rows[1].get("mobile_attention").is_none());
            // Decorating again adds nothing twice.
            let mut again = rows[0].clone();
            add_alert(&mut again, &rows[0]["mobile_attention"][0].clone());
            assert_eq!(again["mobile_attention"].as_array().unwrap().len(), 1);
            // The lead's own question keeps its card and its signature changes.
            let mut asked = row("lead");
            asked["attention_signature"] = json!("a".repeat(64));
            asked["mobile_attention"] = json!([{"question_id":"q1","question_hash":"h1"}]);
            add_alert(&mut asked, alert);
            assert_eq!(asked["mobile_attention"].as_array().unwrap().len(), 2);
            assert_ne!(asked["attention_signature"], json!("a".repeat(64)));
        });
    }

    /// Task 115: the recorded Adventure team case. The Integrator (Kimi) showed
    /// "Needs approval" in the session list while the team tick still stored it
    /// busy/working, so `hgs team status` told the lead it was active. Its state
    /// now comes from this contract: an open native approval is Waiting for you.
    #[test]
    fn a_kimi_member_waiting_on_an_approval_is_waiting_for_you_not_busy() {
        use teams::model::{Member, MemberSeen, MemberState, PendingQuestion};
        let stored = || Member {
            id: "integrator".into(),
            role: "Integrator".into(),
            agent: "kimi".into(),
            state: MemberState::Active,
            seen: Some(MemberSeen { at: now() - 120.0, live: true }),
            seen_phase: "working".into(),
            seen_activity: "busy".into(),
            seen_event_at: Some(now() - 120.0),
            ..Default::default()
        };
        // Kimi answers its tool approvals in its Terminal panel; the stored
        // pending approval outranks the phase the last tick saw.
        let mut approval = stored();
        approval.pending = Some(PendingQuestion {
            question_id: "q".into(),
            question_hash: "h".into(),
            approval: true,
            at: now(),
            ..Default::default()
        });
        assert_eq!(
            member_state(&approval),
            ("user", Some(json!({"kind":"approval","text":"Needs approval"})))
        );
        // A Kimi question the lead cannot answer in the panel is the same wait.
        let mut question = stored();
        question.terminal_question = Some(PendingQuestion {
            question_id: "q".into(),
            question_hash: "h".into(),
            at: now(),
            ..Default::default()
        });
        assert_eq!(member_state(&question).0, "user");
        assert_eq!(member_state(&question).1.unwrap()["kind"], "input");
        // The contract's question phase is Waiting for you without a card.
        let mut phase = stored();
        phase.seen_phase = "question".into();
        phase.seen_activity = "attention".into();
        assert_eq!(member_state(&phase).0, "user");
        // Without a question the stored phase decides: a busy tool works, and a
        // stopped session is stopped, never waiting.
        let mut working = stored();
        working.seen_phase = "tool".into();
        assert_eq!(member_state(&working), ("working", None));
        let mut stopped = stored();
        stopped.seen = Some(MemberSeen { at: now(), live: false });
        assert_eq!(member_state(&stopped), ("stopped", None));
    }
}

