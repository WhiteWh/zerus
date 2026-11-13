//! Keep cache warm: a per-session setting kept on the agent's computer, and the
//! recovery worker's round that keeps the provider prompt cache warm by sending
//! the idle agent one short marked message before the cache would expire.
//! See docs/superpowers/specs/2026-11-07-keep-cache-warm-design.md.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;

// Dormant owner-key carry for the legacy-to-ID metadata transaction.
#[cfg(unix)]
#[allow(dead_code)]
pub(super) mod owner_transfer;

/// The exact ping. Its journal marks and silent presentation depend on it.
pub(super) const TEXT: &str = "[Zerus keep-warm] Reply with exactly OK and do nothing else.";
/// Enabled sessions per computer: bounds every read of the store and the round.
const MAX_ENABLED: usize = 64;
/// Disabled entries kept for revision checks before the oldest are dropped.
const MAX_ENTRIES: usize = 512;
const MAX_FILE: u64 = 1024 * 1024;
const MAX_REVISION: u64 = i64::MAX as u64;
const USAGE: &str = "usage: hgs keep-warm <session> --json (JSON stdin: expected_run_id, expected_conversation_id, expected_revision, enabled, hours 1-7 or always)";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    name: String,
    enabled: bool,
    /// Seconds since the epoch on this computer's clock; None is Always.
    until: Option<f64>,
    revision: u64,
    set_at: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Store {
    version: u8,
    sessions: BTreeMap<String, Entry>,
}

impl Default for Store {
    fn default() -> Self {
        Self { version: 1, sessions: BTreeMap::new() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    expected_run_id: String,
    expected_conversation_id: String,
    expected_revision: u64,
    enabled: bool,
    #[serde(default)]
    hours: Option<u8>,
    #[serde(default)]
    always: bool,
}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn parse(bytes: &[u8]) -> Result<Request> {
    let request: Request = serde_json::from_slice(bytes).map_err(|_| "invalid keep-warm request")?;
    let duration = match (request.hours, request.always) {
        (None, false) => !request.enabled,
        (Some(hours), false) => request.enabled && (1..=7).contains(&hours),
        (None, true) => request.enabled,
        (Some(_), true) => false,
    };
    if !identity(&request.expected_run_id)
        || !identity(&request.expected_conversation_id)
        || request.expected_revision > MAX_REVISION
        || !duration
    {
        return Err("invalid keep-warm request".into());
    }
    Ok(request)
}

fn path() -> PathBuf {
    root().join("keep-warm.json")
}

fn lock_path() -> PathBuf {
    root().join("keep-warm.lock")
}

/// A setting's key is its session's owner key (`storage::owner_key`): a
/// canonical session ID, or the SHA-256 of a legacy label. Nothing else may
/// name a file of this feature.
fn owner_valid(owner: &str) -> bool {
    session_id::canonical(owner)
        || (owner.len() == 64 && owner.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The worker's state of one session's keep-warm, written only by the worker.
fn runtime_path(owner: &str) -> Option<PathBuf> {
    owner_valid(owner).then(|| root().join("recovery/keep-warm").join(format!("{owner}.json")))
}

/// At most `limit` bytes of a regular file, None when it does not exist. A
/// FIFO, device, directory or symbolic link is refused without blocking.
pub(super) fn read_regular(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds limits", path.display()));
    }
    Ok(Some(bytes))
}

fn read_store(path: &Path) -> Result<Store> {
    let Some(bytes) = read_regular(path, MAX_FILE)? else {
        return Ok(Store::default());
    };
    parse_store(&bytes)
}

fn parse_store(bytes: &[u8]) -> Result<Store> {
    let store: Store = serde_json::from_slice(bytes).map_err(|e| format!("invalid keep-warm settings: {e}"))?;
    if store.version != 1
        || store.sessions.len() > MAX_ENTRIES + 1
        || store.sessions.values().filter(|entry| entry.enabled).count() > MAX_ENABLED
        || store.sessions.iter().any(|(owner, entry)| {
            !owner_valid(owner) || entry.name.len() > 2048 || entry.revision > MAX_REVISION
                || entry.until.is_some_and(|until| !until.is_finite())
        })
    {
        return Err("invalid keep-warm settings".into());
    }
    Ok(store)
}

fn persist(path: &Path, store: &Store) -> Result<()> {
    let serialized = serde_json::to_string(store).map_err(|e| e.to_string())?;
    if serialized.len() as u64 >= MAX_FILE {
        return Err("keep-warm settings exceed limits".into());
    }
    atomic(path, &format!("{serialized}\n"))
}

/// The store, or None when it is unreadable: unknown, never a default "off".
pub(super) fn load() -> Option<Store> {
    read_store(&path()).ok()
}

/// The `keep_warm` object of a listing row.
fn view(entry: Option<&Entry>, runtime: &Value, record: &Value, now: f64) -> Value {
    let enabled = entry.is_some_and(|entry| entry.enabled);
    let state = if !enabled {
        "off"
    } else {
        runtime["state"].as_str().filter(|state| !state.is_empty()).unwrap_or("waiting")
    };
    json!({"enabled":enabled,"until":entry.and_then(|entry| entry.until),
        "revision":entry.map_or(0, |entry| entry.revision),"state":state,
        "next_at":if enabled { runtime["next_at"].clone() } else { Value::Null },
        "reason":if enabled { runtime["reason"].as_str().unwrap_or("") } else { "" },
        "turn_active":record["keep_warm_turn"]["started_at"].as_f64().is_some_and(|at| now - at <= TURN_SECONDS)})
}

const UNREADABLE: &str = "The keep-warm state of this session is unreadable. Nothing is sent until your next message.";

/// The states the worker writes.
const STATES: [&str; 10] = ["waiting", "warming", "cold", "uncertain", "stopped", "paused", "unsupported", "dispatching", "expired", "gone"];

/// Whether `value` is a state the worker writes, with the fields that state needs.
fn runtime_valid(value: &Value) -> bool {
    let state = value["state"].as_str().unwrap_or("");
    let time = |key: &str| value.get(key).is_none_or(|v| v.is_null() || v.as_f64().is_some_and(f64::is_finite));
    let count = |key: &str| value.get(key).is_none_or(|v| v.is_null() || v.is_u64());
    value.is_object()
        && STATES.contains(&state)
        && value.get("reason").is_none_or(Value::is_string)
        && value.get("request_id").is_none_or(Value::is_string)
        && ["since", "next_at", "sent_at", "last_ping_at"].iter().all(|key| time(key))
        && count("pings") && count("revision")
        && (!matches!(state, "dispatching" | "warming") || value["request_id"].as_str().is_some_and(|id| !id.is_empty()))
        && (state != "warming" || value["sent_at"].as_f64().is_some())
}

/// The worker's state: Null when there is none yet, an error when a file
/// exists but is not a state the worker writes, which must never read as none.
fn read_runtime_at(path: &Path) -> Result<Value> {
    let Some(bytes) = read_regular(path, 64 * 1024)? else {
        return Ok(Value::Null);
    };
    serde_json::from_slice::<Value>(&bytes).ok().filter(runtime_valid)
        .ok_or_else(|| format!("invalid keep-warm state in {}", path.display()))
}

fn read_runtime(owner: &str) -> Result<Value> {
    read_runtime_at(&runtime_path(owner).ok_or("invalid keep-warm owner")?)
}

/// What a round records when it cannot read its state: uncertain, so that
/// nothing is sent again before the person's next turn.
fn unreadable(now: f64) -> Step {
    hold("uncertain", UNREADABLE, None, Some(now))
}

/// Add `keep_warm` to one listing or inspect row. Only enabled sessions read
/// the worker's state, so a listing does at most 64 extra small reads.
pub(super) fn enrich(session: &mut Value, store: Option<&Store>, record: &Value) {
    let Some(store) = store else {
        session["keep_warm"] = Value::Null;
        return;
    };
    let owner = storage::owner_key(record);
    let entry = store.sessions.get(&owner);
    let runtime = if entry.is_some_and(|entry| entry.enabled) {
        read_runtime(&owner).unwrap_or_else(|_| json!({"state":"uncertain","reason":UNREADABLE}))
    } else {
        Value::Null
    };
    session["keep_warm"] = view(entry, &runtime, record, now());
}

fn entry(store: &Store, owner: &str) -> Option<Entry> {
    store.sessions.get(owner).cloned()
}

/// Pure compare-and-set reducer. `eligible` is asked only when enabling.
fn update(
    store: &mut Store,
    owner: &str,
    record: &Value,
    request: &Request,
    now: f64,
    eligible: impl FnOnce() -> std::result::Result<(), String>,
) -> (i32, Value, bool) {
    let run = string(record, "run_id");
    let conversation = string(record, "conversation_id");
    let previous = entry(store, owner);
    let revision = previous.as_ref().map_or(0, |entry| entry.revision);
    let refusal = |error: &str, reason: &str| {
        let mut output = json!({"ok":false,"error":error,"run_id":run,"conversation_id":conversation,
            "keep_warm":previous});
        if !reason.is_empty() {
            output["reason"] = json!(reason);
        }
        (1, output, false)
    };
    if request.expected_run_id != run || request.expected_conversation_id != conversation {
        return refusal("stale_identity", "");
    }
    if request.expected_revision != revision {
        return refusal("conflict", "");
    }
    let was_enabled = previous.as_ref().is_some_and(|entry| entry.enabled);
    if !request.enabled && !was_enabled {
        return (0, json!({"ok":true,"applied":false,"run_id":run,"conversation_id":conversation,
            "keep_warm":previous}), false);
    }
    if revision == MAX_REVISION {
        return refusal("revision_exhausted", "");
    }
    if request.enabled {
        if !was_enabled && store.sessions.values().filter(|entry| entry.enabled).count() >= MAX_ENABLED {
            return refusal("capacity", "");
        }
        if let Err(reason) = eligible() {
            return refusal("unsupported", &reason);
        }
    }
    let next = Entry {
        name: string(record, "name").into(),
        enabled: request.enabled,
        until: match request.hours {
            Some(hours) if request.enabled => Some(now + f64::from(hours) * 3600.0),
            _ => None,
        },
        revision: revision + 1,
        set_at: now,
    };
    store.sessions.insert(owner.into(), next.clone());
    prune(store);
    (0, json!({"ok":true,"applied":true,"run_id":run,"conversation_id":conversation,"keep_warm":next}), true)
}

/// Caches shorter than this are not kept warm: pings every few minutes cost
/// about as much as rebuilding the cache and fill the conversation.
const MIN_TTL: u64 = 15 * 60;
/// Pings land at 80% of the lifetime, ahead of Claude Code's idle compaction
/// at 90% and of worker scheduling delay.
const INTERVAL_PERCENT: u64 = 80;
/// Kimi Code's own `estimated_cache_duration` values, fetched 2026-11-07.
const KIMI_TABLE: &[(&str, u64)] = &[
    ("kimi-for-coding", 3600), ("kimi-for-coding-highspeed", 3600), ("k3", 1800),
    ("k3-256k", 1800), ("k3.1-pro", 1800), ("k3.1-pro-highspeed", 1800),
];

/// Whether and how this session's cache can be kept warm, from what the agent
/// reported. `expires_at` is the estimated end of the current cache lifetime.
#[derive(Debug, PartialEq)]
pub(super) struct Eligibility {
    pub supported: bool,
    pub reason_code: &'static str,
    pub reason: &'static str,
    pub ttl: Option<u64>,
    pub source: &'static str,
    pub expires_at: Option<f64>,
}

impl Eligibility {
    fn refused(reason_code: &'static str, reason: &'static str) -> Self {
        Self { supported: false, reason_code, reason, ttl: None, source: "", expires_at: None }
    }

    fn waiting(reason: &'static str) -> Self {
        Self { supported: true, reason_code: "no_cache_report", reason, ttl: None, source: "", expires_at: None }
    }

    fn known(ttl: u64, source: &'static str, expires_at: Option<f64>) -> Self {
        Self { supported: true, reason_code: "", reason: "", ttl: Some(ttl), source, expires_at }
    }

    pub fn interval(&self) -> Option<u64> {
        self.ttl.map(|ttl| ttl * INTERVAL_PERCENT / 100)
    }

    /// When the next ping is due: the start of the current lifetime plus the interval.
    pub fn due_at(&self) -> Option<f64> {
        let (ttl, interval) = (self.ttl? as f64, self.interval()? as f64);
        self.expires_at.map(|expires| expires - ttl + interval)
    }
}

/// A Codex model with OpenAI's documented 30-minute default: GPT-5.6 and later.
fn codex_thirty_minutes(model: &str) -> bool {
    let Some(version) = model.strip_prefix("gpt-") else { return false };
    let version: String = version.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), _) if major > 5 => true,
        (Some(5), Some(minor)) => minor >= 6,
        _ => false,
    }
}

/// Kimi Code's newest fetched `estimated_cache_duration` for `model`, read
/// from the session's Kimi home: bounded, read only, never required.
fn kimi_config(home: &Path, model: &str) -> Option<u64> {
    let directory = home.join("cache/client-configs");
    let mut newest: Option<(f64, Option<u64>)> = None;
    for entry in fs::read_dir(directory).ok()?.flatten().take(64) {
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with("estimated_cache_duration.json") {
            continue;
        }
        let Ok(Some(bytes)) = read_regular(&entry.path(), 64 * 1024) else { continue };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { continue };
        let fetched = value["fetchedAt"].as_f64().unwrap_or(0.0);
        let rules = &value["config"]["config"];
        if !rules.is_object() || newest.is_some_and(|(at, _)| at >= fetched) {
            continue;
        }
        newest = Some((fetched, rules[model]["cache_duration"].as_u64().filter(|ttl| (1..=86_400).contains(ttl))));
    }
    newest.and_then(|(_, ttl)| ttl)
}

/// `last_turn_at` is when the last main-agent turn (or ping) ended, the start
/// of the current cache lifetime for agents whose TTL is not observed.
pub(super) fn eligibility(record: &Value, usage: &Value, last_turn_at: Option<f64>) -> Eligibility {
    let from_turn = |ttl: u64, source| Eligibility::known(ttl, source, last_turn_at.map(|at| at + ttl as f64));
    match string(record, "agent") {
        "claude" => {
            let cache = &usage["prompt_cache"];
            match cache["ttl_seconds"].as_u64() {
                None => Eligibility::waiting("Waiting for Claude to report its cache lifetime with its next reply."),
                Some(ttl) if ttl < MIN_TTL => Eligibility::refused("claude_short_ttl",
                    "Claude uses a 5-minute cache on this account. Keeping it warm would cost about as much as rebuilding it."),
                // An estimate that ran out names when (`lapsed_at`): a deadline
                // in the past, which the round holds as a cold cache.
                Some(ttl) => Eligibility::known(ttl, "native_usage",
                    cache["expires_at"].as_f64().or(cache["lapsed_at"].as_f64())),
            }
        }
        "codex" => match input::codex_footer_model(record) {
            None => Eligibility::waiting("Waiting for Codex to report its model."),
            Some(model) if codex_thirty_minutes(&model.to_ascii_lowercase()) => from_turn(1800, "provider_default"),
            Some(_) => Eligibility::refused("codex_model",
                "OpenAI keeps a 30-minute cache only for GPT-5.6 and later. This model's cache lasts about 5 to 10 minutes."),
        },
        "kimi" => {
            let model = string(record, "model");
            let model = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
            if model.is_empty() {
                return Eligibility::waiting("Waiting for Kimi to report its model.");
            }
            let configured = (!string(record, "agent_home").is_empty())
                .then(|| kimi_config(Path::new(string(record, "agent_home")), &model)).flatten()
                .or_else(|| kimi_config(&home().join(".kimi-code"), &model).filter(|_| string(record, "agent_home").is_empty()));
            match (configured, KIMI_TABLE.iter().find(|(name, _)| *name == model)) {
                (Some(ttl), _) if ttl >= MIN_TTL => from_turn(ttl, "kimi_config"),
                (None, Some((_, ttl))) => from_turn(*ttl, "kimi_table"),
                _ => Eligibility::refused("kimi_model", "Kimi reports no cache lifetime of at least 15 minutes for this model."),
            }
        }
        "dsh" => Eligibility::refused("deepseek", "DeepSeek keeps its context cache for hours without pings."),
        _ => Eligibility::refused("agent", "This agent reports no prompt cache that pings can keep warm."),
    }
}

/// How long a submitted ping may wait for its prompt hook to mark its turn.
const PENDING_SECONDS: f64 = 120.0;
/// A ping turn that never reported its end stops marking events after this.
const TURN_SECONDS: f64 = 600.0;
/// What a ping turn keeps from the session's visible state: listings and
/// search show these instead of the ping's prompt, reply and time.
const VISIBLE: [&str; 4] = ["prompt", "last_message", "last_event_at", "turn_started"];

fn main_event(record: &Value, event: &Value) -> bool {
    let agent_id = string(event, "agent_id");
    !["SubagentStart", "SubagentStop"].contains(&string(event, "hook_event_name"))
        && (agent_id.is_empty() || (string(record, "agent") == "kimi" && agent_id == "main"))
}

fn prompt_text(event: &Value) -> String {
    event["prompt"].as_str().map(str::to_owned).unwrap_or_else(|| {
        event["prompt"].as_array().into_iter().flatten().filter(|part| part["type"] == "text")
            .map(|part| string(part, "text")).collect::<Vec<_>>().join("\n")
    })
}

fn remove(record: &mut Value, key: &str) -> Option<Value> {
    record.as_object_mut().and_then(|record| record.remove(key))
}

/// Mark a main-agent hook `event` of a ping turn before `update_activity`:
/// `event["keep_warm"]` is "prompt" for the ping's own prompt and "turn" for
/// the rest of its turn. Only the ping's exact text while its submission is
/// pending opens a ping turn; any other prompt makes the turn the person's.
pub(super) fn observe(record: &mut Value, event: &mut Value, now: f64) {
    if !main_event(record, event) {
        return;
    }
    if record["keep_warm_pending"]["submitted_at"].as_f64().is_none_or(|at| now - at > PENDING_SECONDS) {
        remove(record, "keep_warm_pending");
    }
    if record["keep_warm_turn"]["started_at"].as_f64().is_none_or(|at| now - at > TURN_SECONDS) {
        remove(record, "keep_warm_turn");
    }
    let kind = string(event, "hook_event_name");
    let prompt = ["UserPromptSubmit", "UserPromptQueued"].contains(&kind)
        || (kind == "TurnStarted" && (event["prompt"].is_string() || event["prompt"].is_array() || event["origin_kind"].is_string()));
    if !prompt {
        if record["keep_warm_turn"].is_object() {
            event["keep_warm"] = json!("turn");
        }
        return;
    }
    if prompt_text(event).trim() == TEXT {
        if record["keep_warm_turn"].is_object() {
            // The same ping reported again (Kimi's TurnStarted after its prompt).
            event["keep_warm"] = json!("turn");
            return;
        }
        if let Some(pending) = remove(record, "keep_warm_pending") {
            record["keep_warm_turn"] = json!({"request_id":pending["request_id"],"started_at":now});
            if !record["keep_warm_quiet"].is_object() {
                let mut quiet = json!({});
                for key in VISIBLE {
                    quiet[key] = if key == "prompt" { journal::user_prompt(record).clone() } else { record[key].clone() };
                }
                record["keep_warm_quiet"] = quiet;
            }
            event["keep_warm"] = json!("prompt");
            return;
        }
    }
    // The person's own message, or a job's: everything is visible again, and
    // a ping still being dispatched can no longer claim a later prompt.
    remove(record, "keep_warm_turn");
    remove(record, "keep_warm_quiet");
    remove(record, "keep_warm_pending");
    remove(record, "keep_warm_prompt_at");
}

/// Whether the latest prompt of the session was a verified keep-warm ping
/// (marked through its pending submission, never by its text alone).
pub(super) fn ping_was_last_prompt(record: &Value) -> bool {
    record["keep_warm_prompt_at"].as_f64()
        .is_some_and(|at| at >= record["recovery_user_at"].as_f64().unwrap_or(0.0))
}

/// After `update_activity`: record when the cache was last used and end a
/// ping turn at its Stop, failure or interruption.
pub(super) fn settle(record: &mut Value, event: &Value, now: f64) {
    if !main_event(record, event) {
        return;
    }
    let ping = event["keep_warm"].is_string();
    if event["keep_warm"] == "prompt" {
        record["keep_warm_prompt_at"] = json!(now);
    }
    match string(event, "hook_event_name") {
        "Stop" => {
            record["cache_touched"] = json!({"at":now,"conversation_id":record["conversation_id"]});
            if ping {
                remove(record, "keep_warm_turn");
            } else {
                record["keep_warm_real_stop_at"] = json!(now);
            }
        }
        "StopFailure" | "Interrupt" | "SessionEnd" if ping => {
            remove(record, "keep_warm_turn");
        }
        _ => {}
    }
}

/// Show the session as it was before its ping turns.
pub(super) fn visible(output: &mut Value, record: &Value) {
    let quiet = &record["keep_warm_quiet"];
    if !quiet.is_object() {
        return;
    }
    for key in VISIBLE {
        if output.get(key).is_some() {
            output[key] = quiet[key].clone();
        }
    }
}

/// Work around the agent that its record alone does not show.
#[derive(Debug)]
pub(super) struct Facts {
    /// Live owned shell or native jobs (desktop Background); None is unknown.
    pub background: Option<u64>,
    /// Jobs the agent scheduled for itself.
    pub schedules: u64,
    /// An automatic recovery episode owns this session's next input.
    pub recovery_active: bool,
    /// When an attached terminal client last had input: `Some(None)` with
    /// none attached, None when unknown, which never admits a ping.
    pub client_input: Option<Option<f64>>,
}

impl Facts {
    pub fn observe(record: &Value) -> Self {
        let live = process_alive(record);
        let background = processes::WorkSample::default().summary(record, live)["count"].as_u64();
        let schedules = if string(record, "agent") == "kimi" {
            // Unknown schedules count as present: never ping into a job's turn.
            goals::summary(record)["schedules"]["count"].as_u64().unwrap_or(1)
        } else {
            0
        };
        let client_input = backend::current().last_client_input(terminal_name(record));
        Self { background, schedules, recovery_active: recovery::job_active(record), client_input }
    }
}

/// Why ping `id` must not be typed now, if it must not. The guarded submit
/// asks this under the input lock, once before staging and again immediately
/// before the first input byte.
fn submission_refusal(record: &Value, entry: Option<&Entry>, runtime: &Value, facts: &Facts, rule: &Eligibility, id: &str, now: f64) -> Option<&'static str> {
    let Some(entry) = entry.filter(|entry| entry.enabled) else {
        return Some("Keep cache warm was turned off; nothing was submitted");
    };
    if entry.until.is_some_and(|until| until <= now) {
        return Some("Keep cache warm has expired; nothing was submitted");
    }
    if runtime["state"] != "dispatching" || runtime["request_id"] != id || runtime["revision"].as_u64() != Some(entry.revision) {
        return Some("This keep-warm ping is no longer current; nothing was submitted");
    }
    if !telemetry::settled(record) || record["keep_warm_turn"].is_object() || record["paused"] == true
        || ["pending", "attempted"].contains(&string(&record["launch_prompt"], "status"))
    {
        return Some("The agent is not idle; nothing was submitted");
    }
    if facts.background != Some(0) {
        return Some("Background work is running or unknown; nothing was submitted");
    }
    if facts.schedules > 0 {
        return Some("The agent has scheduled jobs; nothing was submitted");
    }
    if facts.recovery_active {
        return Some("Automatic recovery owns the next message; nothing was submitted");
    }
    match facts.client_input {
        None => return Some("Terminal input activity is unknown; nothing was submitted"),
        Some(Some(at)) if now - at < TYPING_SECONDS => return Some("Someone typed in Terminal; nothing was submitted"),
        _ => {}
    }
    // Staging can take long: the cache must still be warm when the ping is typed.
    if !rule.supported || rule.expires_at.is_none_or(|expires| expires - now < COLD_MARGIN) {
        return Some("The cache may already have expired; nothing was submitted");
    }
    None
}

/// The guarded submit's check for ping `id` (`input::submit_keep_warm`).
pub(super) fn check_submission(record: &Value, id: &str) -> Result<()> {
    let owner = storage::owner_key(record);
    let store = load().ok_or("Keep cache warm settings are unreadable; nothing was submitted")?;
    let facts = Facts::observe(record);
    let runtime = read_runtime(&owner)?;
    let usage = if string(record, "agent") == "claude" { usage::read(record) } else { Value::Null };
    let rule = eligibility(record, &usage, last_turn_at(record));
    match submission_refusal(record, store.sessions.get(&owner), &runtime, &facts, &rule, id, now()) {
        Some(reason) => Err(reason.into()),
        None => Ok(()),
    }
}

/// The worker's view of one enabled session.
pub(super) struct Observed<'a> {
    pub record: &'a Value,
    /// The exact verified agent process runs in its pane.
    pub live: bool,
    /// Native usage (Claude's observed cache lifetime).
    pub usage: Value,
}

/// What one round does for one enabled session.
#[derive(Debug, PartialEq)]
enum Step {
    /// Turn the setting off: it expired or its session is gone.
    Off(&'static str, &'static str),
    /// Send nothing; record this state.
    Hold { state: &'static str, reason: String, next_at: Option<f64>, since: Option<f64> },
    /// Send one ping now.
    Send,
}

/// Never ping this close to the estimated expiry: the cache may be gone.
const COLD_MARGIN: f64 = 30.0;
/// Terminal input this recent means someone may be typing.
const TYPING_SECONDS: f64 = 120.0;
const RESUMES: &str = "Keeping it warm resumes after your next message.";

fn hold(state: &'static str, reason: impl Into<String>, next_at: Option<f64>, since: Option<f64>) -> Step {
    Step::Hold { state, reason: reason.into(), next_at, since }
}

/// One round's decision. `facts` is asked only for a due ping.
fn decide(entry: &Entry, runtime: &Value, observed: Option<&Observed>, facts: impl FnOnce() -> Facts, now: f64) -> Step {
    if entry.until.is_some_and(|until| until <= now) {
        return Step::Off("expired", "Keep cache warm ended at the chosen time.");
    }
    let Some(observed) = observed else {
        return Step::Off("gone", "The session is no longer available.");
    };
    let record = observed.record;
    let since = runtime["since"].as_f64();
    // Missed windows and unknown deliveries wait for the person's next turn.
    let resumed = since.is_some_and(|since| record["keep_warm_real_stop_at"].as_f64().unwrap_or(0.0) > since);
    let reason = string(runtime, "reason").to_owned();
    match string(runtime, "state") {
        "cold" if !resumed => return hold("cold", reason, None, since),
        "uncertain" if !resumed => return hold("uncertain", reason, None, since),
        "stopped" if !resumed => return hold("stopped", reason, None, since),
        // Each round runs alone, so a dispatching state is a round that died
        // while sending: the ping may have been typed. Never send it again.
        "dispatching" => return hold("uncertain",
            format!("A keep-warm ping may have been typed when its worker stopped. {RESUMES}"), None, Some(now)),
        "warming" => {
            let sent = runtime["sent_at"].as_f64().unwrap_or(0.0);
            let answered = last_turn_at(record).is_some_and(|at| at > sent) && !record["keep_warm_turn"].is_object();
            if !answered && now - sent > TURN_SECONDS {
                return hold("uncertain", format!("No reply to the last keep-warm ping was seen. {RESUMES}"), None, Some(now));
            }
            if !answered {
                return hold("warming", "", None, since);
            }
        }
        _ => {}
    }
    if !observed.live || record["paused"] == true {
        return hold("paused", "The agent is not running.", None, None);
    }
    if record["phase"] == "error" && ping_was_last_prompt(record) {
        return hold("stopped", format!("The last keep-warm ping failed. {RESUMES}"), None, Some(now));
    }
    let rule = eligibility(record, &observed.usage, last_turn_at(record));
    if !rule.supported {
        return hold("unsupported", rule.reason, None, None);
    }
    if !telemetry::settled(record) || record["keep_warm_turn"].is_object() {
        return hold("waiting", "The agent is working.", None, None);
    }
    let (Some(due), Some(expires)) = (rule.due_at(), rule.expires_at) else {
        return hold("waiting", rule.reason, None, None);
    };
    if expires - now < COLD_MARGIN {
        return hold("cold", format!("The cache went cold. {RESUMES}"), None, Some(now));
    }
    if now < due {
        return hold("waiting", "", Some(due), None);
    }
    let facts = facts();
    if facts.background != Some(0) {
        return hold("waiting", "Background work is running.", None, None);
    }
    if facts.schedules > 0 {
        return hold("waiting", "The agent has scheduled jobs.", None, None);
    }
    if facts.recovery_active {
        return hold("waiting", "Automatic recovery is active.", None, None);
    }
    match facts.client_input {
        None => return hold("waiting", "Terminal input activity is unknown.", None, None),
        Some(Some(at)) if now - at < TYPING_SECONDS => return hold("waiting", "Someone is typing in Terminal.", None, None),
        _ => {}
    }
    Step::Send
}

/// The enabled session's current record: by its session ID, so a rename
/// keeps it, else by its saved label with the same owner key.
/// None only when the session is positively gone; a record that exists but
/// cannot be read now is an error, and the round changes nothing.
fn bound_record(owner: &str, entry: &Entry) -> Result<Option<Value>> {
    let by_id = session_id::canonical(owner);
    let path = if by_id { storage::id_record_path(owner).ok_or("invalid keep-warm owner")? } else { record_path(&entry.name) };
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("keep-warm session {}: {error}", path.display())),
    }
    let record = if by_id { storage::read_id(owner)? } else { read(&entry.name)? };
    let archived = !string(&record, "archive_id").is_empty() || record["state"] == "archived";
    Ok((storage::owner_key(&record) == owner && !archived).then_some(record))
}

/// The worker state left when the setting turns off. A delivery fence stays:
/// turning the setting on again must not make an unknown ping a first use.
fn off_runtime(previous: &Value, state: &str, reason: &str, now: f64) -> Value {
    match string(previous, "state") {
        "uncertain" | "dispatching" | "warming" => {
            let kept = string(previous, "reason");
            let reason = if string(previous, "state") == "uncertain" && !kept.is_empty() {
                kept.to_owned()
            } else {
                format!("A keep-warm ping may have been delivered when keep-warm turned off. {RESUMES}")
            };
            json!({"state":"uncertain","reason":reason,"since":previous["since"].as_f64().unwrap_or(now),"pings":previous["pings"]})
        }
        "stopped" => previous.clone(),
        _ => json!({"state":state,"reason":reason,"since":now,"pings":previous["pings"]}),
    }
}

fn save_runtime(owner: &str, value: &Value, verify: &impl Fn() -> Result<()>) -> Result<()> {
    verify()?;
    atomic(&runtime_path(owner).ok_or("invalid keep-warm owner")?, &value.to_string())
}

/// One line in `<state>/logs/keep-warm.log` for each change of what the round
/// does for a session. The worker state file holds only the latest decision,
/// so nothing said afterwards why a cache was not kept warm. The log is
/// bounded as the slow-calls log is; a failed write never fails the round.
fn log_decision(entry: &Entry, runtime: &Value) {
    let line = json!({"at":now(),"session":entry.name.chars().take(200).collect::<String>(),"revision":entry.revision,
        "state":runtime["state"],"reason":runtime["reason"],"next_at":runtime["next_at"],"pings":runtime["pings"]});
    let _ = super::slow_calls::append(&root().join("logs").join("keep-warm.log"), &line.to_string());
}

/// Turn an expired or orphaned setting off, unless it changed meanwhile.
fn turn_off(owner: &str, entry: &Entry, verify: &impl Fn() -> Result<()>) -> Result<()> {
    let _guard = lock(Some(&lock_path()))?;
    verify()?;
    let mut store = read_store(&path())?;
    if let Some(current) = store.sessions.get_mut(owner).filter(|current| *current == entry) {
        current.enabled = false;
        current.revision += 1;
        current.set_at = now();
        persist(&path(), &store)?;
    }
    Ok(())
}

/// Send one ping. The guarded submit re-checks everything under the input
/// lock; only a refusal before any input lets a later round try again.
fn send(owner: &str, entry: &Entry, record: &Value, mut runtime: Value, now: f64,
        verify: &impl Fn() -> Result<()>) -> Result<()> {
    let request_id = uuid::Uuid::new_v4().to_string();
    // The ping belongs to the setting it was decided for (its revision).
    for (key, value) in [("state", json!("dispatching")), ("request_id", json!(request_id)), ("since", json!(now)),
        ("reason", json!("")), ("next_at", Value::Null), ("revision", json!(entry.revision))] {
        runtime[key] = value;
    }
    save_runtime(owner, &runtime, verify)?;
    let payload = json!({"request_id":request_id,"text":TEXT,"expected_run_id":record["run_id"],
        "expected_conversation_id":record["conversation_id"]});
    verify()?;
    let result = input::submit_keep_warm(string(record, "name"), payload.to_string().as_bytes(), &request_id);
    match result {
        input::Submission::Receipt(_) => {
            runtime["state"] = json!("warming");
            runtime["sent_at"] = json!(now);
            runtime["last_ping_at"] = json!(now);
            runtime["pings"] = json!(runtime["pings"].as_u64().unwrap_or(0) + 1);
        }
        input::Submission::NotOffered(witness) => {
            runtime["state"] = json!("waiting");
            runtime["reason"] = json!(witness.reason);
            runtime["since"] = Value::Null;
        }
        input::Submission::Unknown(error) => {
            runtime["state"] = json!("uncertain");
            runtime["reason"] = json!(format!("The last keep-warm delivery is unknown: {}. {RESUMES}", journal::clipped(&json!(error), 300)));
        }
    }
    log_decision(entry, &runtime);
    save_runtime(owner, &runtime, verify)
}

fn round(owner: &str, entry: &Entry, panes: &Snapshot, now: f64,
         verify: &impl Fn() -> Result<()>) -> Result<()> {
    if !owner_valid(owner) {
        return Err("invalid keep-warm owner".into());
    }
    let runtime = match read_runtime(owner) {
        Ok(runtime) => runtime,
        Err(_) => {
            let Step::Hold { state, reason, since, .. } = unreadable(now) else { unreachable!() };
            return save_runtime(owner, &json!({"state":state,"reason":reason,"since":since}), verify);
        }
    };
    let record = bound_record(owner, entry)?;
    let observed = record.as_ref().map(|record| Observed {
        record,
        live: matches(record, panes_of(panes, record)) && process_alive(record),
        usage: if string(record, "agent") == "claude" { usage::read(record) } else { Value::Null },
    });
    let step = decide(entry, &runtime, observed.as_ref(),
        || record.as_ref().map_or(Facts { background: None, schedules: 0, recovery_active: false, client_input: None }, Facts::observe),
        now);
    match step {
        Step::Off(state, reason) => {
            turn_off(owner, entry, verify)?;
            let off = off_runtime(&runtime, state, reason, now);
            log_decision(entry, &off);
            save_runtime(owner, &off, verify)
        }
        Step::Hold { state, reason, next_at, since } => {
            let mut next = runtime.clone();
            if !next.is_object() {
                next = json!({});
            }
            for (key, value) in [("state", json!(state)), ("reason", json!(reason)), ("next_at", json!(next_at)), ("since", json!(since))] {
                next[key] = value;
            }
            if next == runtime {
                return Ok(());
            }
            // A new due time alone is the same decision.
            if next["state"] != runtime["state"] || next["reason"] != runtime["reason"] {
                log_decision(entry, &next);
            }
            save_runtime(owner, &next, verify)
        }
        Step::Send => send(owner, entry, record.as_ref().unwrap(), if runtime.is_object() { runtime } else { json!({}) }, now, verify),
    }
}

/// One keep-warm round over the enabled sessions of this computer. Rounds
/// never overlap: a second one (the CLI `recovery tick` beside the worker)
/// skips, so one window never gets two pings.
pub(super) fn tick() -> Result<()> {
    let Some(store) = load() else { return Ok(()) };
    if !store.sessions.values().any(|entry| entry.enabled) {
        return Ok(());
    }
    private_dir(&root().join("recovery/keep-warm"))?;
    #[cfg(unix)]
    let Some(_round) = storage::job_round::try_acquire(storage::job_round::Kind::KeepWarm)? else {
        return Ok(());
    };
    #[cfg(not(unix))]
    let file = crate::platform::private_open(fs::OpenOptions::new().create(true).truncate(false).write(true))
        .open(root().join("recovery/keep-warm.lock")).map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    if fs2::FileExt::try_lock_exclusive(&file).is_err() {
        return Ok(());
    }
    #[cfg(not(unix))]
    let _round = crate::platform::FileLock::held(file);
    let verify = || {
        #[cfg(unix)]
        { _round.verify() }
        #[cfg(not(unix))]
        { Ok(()) }
    };
    // A transition can finish after the preliminary read and before this
    // round takes its exclusion. Only the snapshot inside the original round
    // may supply owner keys or settings for work.
    let Some(store) = load() else { return Ok(()) };
    let enabled: Vec<(String, Entry)> = store.sessions.into_iter().filter(|(_, entry)| entry.enabled).collect();
    let panes = live()?;
    for (owner, entry) in enabled {
        if let Err(error) = round(&owner, &entry, &panes, now(), &verify) {
            eprintln!("hgs keep-warm: {error}");
        }
    }
    Ok(())
}

/// When the last main-agent turn or ping of the current conversation ended,
/// the start of its cache lifetime for agents whose TTL is not observed per
/// request. A clock from another conversation (before Clear) is no evidence.
pub(super) fn last_turn_at(record: &Value) -> Option<f64> {
    let touched = &record["cache_touched"];
    (!string(record, "conversation_id").is_empty() && touched["conversation_id"] == record["conversation_id"])
        .then(|| touched["at"].as_f64()).flatten().filter(|at| at.is_finite() && *at > 0.0)
}

/// Add the derived fields of the Details read to `output["keep_warm"]`.
pub(super) fn describe(output: &mut Value, record: &Value, usage: &Value) {
    if !output["keep_warm"].is_object() {
        return;
    }
    let rule = eligibility(record, usage, last_turn_at(record));
    let warm = &mut output["keep_warm"];
    warm["supported"] = json!(rule.supported);
    warm["reason_code"] = json!(rule.reason_code);
    // Turned on over a cache that is already cold: say so at once, before the
    // worker's next round records it.
    let cold = warm["enabled"] == true && string(warm, "reason").is_empty() && warm["turn_active"] != true
        && ["waiting", "cold"].contains(&string(warm, "state"))
        && rule.expires_at.is_some_and(|expires| expires - now() < COLD_MARGIN);
    warm["reason"] = if !rule.reason.is_empty() {
        json!(rule.reason)
    } else if cold {
        json!(format!("The cache went cold. {RESUMES}"))
    } else {
        json!(string(warm, "reason"))
    };
    warm["ttl_seconds"] = json!(rule.ttl);
    warm["ttl_source"] = json!(rule.source);
    warm["interval_seconds"] = json!(rule.interval());
    warm["cache_expires_at"] = json!(rule.expires_at);
}

/// Why this session cannot keep its cache warm now, if it cannot.
fn eligible(record: &Value) -> std::result::Result<(), String> {
    let usage = if string(record, "agent") == "claude" { usage::read(record) } else { Value::Null };
    let rule = eligibility(record, &usage, None);
    if rule.supported { Ok(()) } else { Err(rule.reason.into()) }
}

fn setting_binding(record: &Value) -> Value {
    json!([record["name"], record["agent"], record["run_id"],
        record["conversation_id"], record["session_id"], record["layout"]])
}

// A setting can wait behind a rename or a future legacy-to-ID transaction.
// Recheck its original storage after taking the setting lock, without taking
// the state writer in the opposite order. A changed original is never a new
// setting for the old key or another holder of the label.
fn verify_setting_binding(original: &Value) -> Result<()> {
    let current = match provider_binding::Provider::named(string(original, "name")) {
        Some(provider) if storage::in_id_layout(original) =>
            provider.read_id(string(original, "session_id"))?,
        Some(provider) => provider.read_label(string(original, "name"))?,
        None => {
            let bytes = storage::binding_bytes(&storage::own_path(original)?)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_FILE {
                return Err("Keep-warm binding exceeds its limit".into());
            }
            let current: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if current["version"] != 1 {
                return Err("Unsupported keep-warm session binding".into());
            }
            current
        }
    };
    if setting_binding(&current) != setting_binding(original) {
        return Err("The session changed while keep-warm was waiting; refresh before changing it".into());
    }
    Ok(())
}

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let [name, flag] = args else {
        return Err(USAGE.into());
    };
    if flag != "--json" || name.is_empty() || name.starts_with(['-', '@']) {
        return Err(USAGE.into());
    }
    let mut bytes = Vec::new();
    io::stdin().take(4097).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("keep-warm request exceeds limits".into());
    }
    let request = parse(super::without_bom(&bytes))?;
    // zerus-ucck P3c: the request's identities select among sessions that
    // share the label.
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    let Some(record) = notifications::current(name)? else {
        println!("{}", json!({"ok":false,"error":"not_found"}));
        return Ok(1);
    };
    let owner = storage::owner_key(&record);
    let (code, mut output, changed, entry) = {
        let _guard = lock(Some(&lock_path()))?;
        verify_setting_binding(&record)?;
        let mut store = read_store(&path())?;
        let (code, output, changed) = update(&mut store, &owner, &record, &request, now(), || eligible(&record));
        if changed {
            persist(&path(), &store)?;
        }
        (code, output, changed, store.sessions.get(&owner).cloned())
    };
    if changed && request.enabled {
        recovery::ensure_worker()?;
    }
    if code == 0 {
        let runtime = read_runtime(&owner).unwrap_or_else(|_| json!({"state":"uncertain","reason":UNREADABLE}));
        output["keep_warm"] = view(entry.as_ref(), &runtime, &record, now());
        let usage = if string(&record, "agent") == "claude" { usage::read(&record) } else { Value::Null };
        describe(&mut output, &record, &usage);
    }
    println!("{output}");
    Ok(code)
}

/// Move a legacy label's setting to its new label's key; false when there is
/// nothing to move. A free legacy label can hold only a stale setting of a
/// session that is gone, which the renamed session's own setting replaces.
fn rename_entry(store: &mut Store, old: &str, new: &str) -> bool {
    let (from, to) = (storage::label_key(old), storage::label_key(new));
    if from == to {
        return false;
    }
    let Some(mut entry) = store.sessions.remove(&from) else { return false };
    entry.name = new.to_owned();
    store.sessions.insert(to, entry);
    true
}

/// The fence a rename leaves when it cannot carry a worker state as it is.
fn carried_fence() -> String {
    json!({"state":"uncertain","reason":UNREADABLE,"since":now()}).to_string()
}

/// A legacy-label rename (`rename.rs`): the setting and worker state follow the
/// session, as its recovery job does. The worker state is carried first, then
/// the setting, then the old state is retired, so an interrupted rename never
/// leaves a setting without its state; replaying the rename repairs any step.
pub(super) fn rename(old: &str, new: &str) -> Result<()> {
    let (from, to) = (storage::label_key(old), storage::label_key(new));
    if from == to {
        return Ok(());
    }
    let (Some(from_state), Some(to_state)) = (runtime_path(&from), runtime_path(&to)) else {
        return Err("invalid keep-warm owner".into());
    };
    let _guard = lock(Some(&lock_path()))?;
    let mut store = read_store(&path())?;
    let moving = store.sessions.contains_key(&from);
    let moved = !moving && store.sessions.get(&to).is_some_and(|entry| entry.name == new);
    if !moving && !moved {
        return Ok(());
    }
    // Present or absent; anything else (permission denied) stops the rename
    // before a step, never reads as absence.
    let present = |path: &Path| match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("keep-warm state {}: {error}", path.display())),
    };
    let (had_state, had_destination) = (present(&from_state)?, present(&to_state)?);
    if had_state {
        // Carried as it is: an unreadable state stays unknown. A state already
        // at the destination after an interrupted move is never trusted over it.
        let carried = match read_regular(&from_state, 64 * 1024) {
            Ok(Some(bytes)) if !(moved && had_destination) => String::from_utf8(bytes).unwrap_or_else(|_| carried_fence()),
            _ => carried_fence(),
        };
        private_dir(to_state.parent().ok_or("invalid keep-warm state path")?)?;
        atomic(&to_state, &carried)?;
    } else if moving && had_destination {
        // The destination's state belonged to a session that is gone.
        fs::remove_file(&to_state).map_err(|e| e.to_string())?;
    }
    if moving {
        rename_entry(&mut store, old, new);
        persist(&path(), &store)?;
    }
    if had_state {
        fs::remove_file(&from_state).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Preserve the normal carry order and its SAME auxiliary exclusion, while
/// every data publication/removal retains the caller's original Root proofs.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_guarded(
    view: &mut guarded_recovery::View<'_>,
    old: &str,
    new: &str,
) -> Result<()> {
    let (from, to) = (storage::label_key(old), storage::label_key(new));
    if from == to {
        return Ok(());
    }
    let (Some(from_state), Some(to_state)) = (runtime_path(&from), runtime_path(&to)) else {
        return Err("invalid keep-warm owner".into());
    };
    let _auxiliary = view.auxiliary(&lock_path())?;
    let mut store = match view.read(&path(), MAX_FILE)? {
        Some(bytes) => parse_store(&bytes)?,
        None => Store::default(),
    };
    let moving = store.sessions.contains_key(&from);
    let moved = !moving
        && store
            .sessions
            .get(&to)
            .is_some_and(|entry| entry.name == new);
    if !moving && !moved {
        return Ok(());
    }
    let from_bytes = view.read(&from_state, 64 * 1024)?;
    let destination = view.read(&to_state, 64 * 1024)?;
    if let Some(bytes) = from_bytes.as_ref() {
        let carried = if moved && destination.is_some() {
            carried_fence()
        } else {
            String::from_utf8(bytes.clone()).unwrap_or_else(|_| carried_fence())
        };
        view.atomic(&to_state, carried.as_bytes())?;
    } else if moving && destination.is_some() {
        view.remove(&to_state)?;
    }
    if moving {
        rename_entry(&mut store, old, new);
        let serialized = serde_json::to_string(&store).map_err(|e| e.to_string())?;
        if serialized.len() as u64 >= MAX_FILE {
            return Err("keep-warm settings exceed limits".into());
        }
        view.atomic(&path(), format!("{serialized}\n").as_bytes())?;
    }
    if from_bytes.is_some() {
        view.remove(&from_state)?;
    }
    Ok(())
}

/// Drop the oldest disabled entries beyond the bound; enabled ones stay.
fn prune(store: &mut Store) {
    let excess = store.sessions.len().saturating_sub(MAX_ENTRIES);
    if excess == 0 {
        return;
    }
    let mut disabled: Vec<(f64, String)> = store.sessions.iter()
        .filter(|(_, entry)| !entry.enabled)
        .map(|(owner, entry)| (entry.set_at, owner.clone()))
        .collect();
    disabled.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, owner) in disabled.into_iter().take(excess) {
        store.sessions.remove(&owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_794_000_000.0;

    fn record() -> Value {
        json!({"name":"claude/project/warm","run_id":"run-1","conversation_id":"conversation-1"})
    }

    fn request(body: Value) -> Request {
        let mut base = json!({"expected_run_id":"run-1","expected_conversation_id":"conversation-1",
            "expected_revision":0,"enabled":true,"hours":3});
        for (key, value) in body.as_object().unwrap() {
            if value.is_null() {
                base.as_object_mut().unwrap().remove(key);
            } else {
                base[key] = value.clone();
            }
        }
        parse(base.to_string().as_bytes()).unwrap()
    }

    fn ok() -> std::result::Result<(), String> {
        Ok(())
    }

    #[test]
    fn enabling_sets_a_deadline_on_this_computers_clock() {
        let mut store = Store::default();
        let (code, output, changed) = update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        assert_eq!((code, changed), (0, true));
        assert_eq!(output["keep_warm"]["until"], json!(NOW + 3.0 * 3600.0));
        assert_eq!(output["keep_warm"]["revision"], 1);
        assert_eq!(output["keep_warm"]["enabled"], true);
        let always = request(json!({"hours":null,"always":true,"expected_revision":1}));
        let (_, output, _) = update(&mut store, "owner", &record(), &always, NOW + 5.0, ok);
        assert_eq!(output["keep_warm"]["until"], Value::Null);
        assert_eq!(output["keep_warm"]["revision"], 2);
    }

    #[test]
    fn disabling_keeps_the_revision_and_repeating_it_changes_nothing() {
        let mut store = Store::default();
        update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        let off = request(json!({"enabled":false,"hours":null,"expected_revision":1}));
        let (code, output, changed) = update(&mut store, "owner", &record(), &off, NOW, ok);
        assert_eq!((code, changed), (0, true));
        assert_eq!((output["keep_warm"]["enabled"].clone(), output["keep_warm"]["revision"].clone()), (json!(false), json!(2)));
        let again = request(json!({"enabled":false,"hours":null,"expected_revision":2}));
        let (code, output, changed) = update(&mut store, "owner", &record(), &again, NOW, ok);
        assert_eq!((code, changed, output["applied"].clone()), (0, false, json!(false)));
    }

    #[test]
    fn stale_identity_conflict_and_ineligible_agents_are_refused() {
        let mut store = Store::default();
        let stale = request(json!({"expected_run_id":"run-0"}));
        assert_eq!(update(&mut store, "owner", &record(), &stale, NOW, ok).1["error"], "stale_identity");
        let conflict = request(json!({"expected_revision":4}));
        assert_eq!(update(&mut store, "owner", &record(), &conflict, NOW, ok).1["error"], "conflict");
        let (code, output, changed) = update(&mut store, "owner", &record(), &request(json!({})), NOW,
            || Err("DeepSeek keeps its context cache for hours without pings.".into()));
        assert_eq!((code, changed, output["error"].clone()), (1, false, json!("unsupported")));
        assert_eq!(output["reason"], "DeepSeek keeps its context cache for hours without pings.");
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn at_most_sixty_four_sessions_keep_their_cache_warm() {
        let mut store = Store::default();
        for index in 0..MAX_ENABLED {
            update(&mut store, &format!("owner-{index}"), &record(), &request(json!({})), NOW, ok);
        }
        let (code, output, _) = update(&mut store, "owner-new", &record(), &request(json!({})), NOW, ok);
        assert_eq!((code, output["error"].clone()), (1, json!("capacity")));
        // Changing the duration of an enabled session is not a new one.
        let longer = request(json!({"hours":7,"expected_revision":1}));
        assert_eq!(update(&mut store, "owner-0", &record(), &longer, NOW, ok).0, 0);
    }

    #[test]
    fn the_listing_shows_the_setting_worker_state_and_a_running_ping() {
        let mut store = Store::default();
        update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        let entry = store.sessions.get("owner");
        let runtime = json!({"state":"waiting","next_at":NOW + 2880.0,"reason":""});
        let mut current = record();
        assert_eq!(view(entry, &runtime, &current, NOW), json!({"enabled":true,"until":NOW + 10800.0,"revision":1,
            "state":"waiting","next_at":NOW + 2880.0,"reason":"","turn_active":false}));
        current["keep_warm_turn"] = json!({"request_id":"ping","started_at":NOW - 30.0});
        assert_eq!(view(entry, &runtime, &current, NOW)["turn_active"], true);
        // A ping turn that never reported its end hides nothing for long.
        assert_eq!(view(entry, &runtime, &current, NOW + TURN_SECONDS)["turn_active"], false);
        // No setting, or a disabled one, is simply off; worker state is not shown.
        assert_eq!(view(None, &Value::Null, &record(), NOW), json!({"enabled":false,"until":null,"revision":0,
            "state":"off","next_at":null,"reason":"","turn_active":false}));
        let off = Entry { enabled: false, ..entry.unwrap().clone() };
        assert_eq!(view(Some(&off), &runtime, &record(), NOW)["state"], "off");
        // A new setting has no worker state yet.
        assert_eq!(view(entry, &Value::Null, &record(), NOW)["state"], "waiting");
    }

    #[test]
    fn corrupt_or_oversized_stores_are_unknown_not_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep-warm.json");
        assert!(read_store(&path).unwrap().sessions.is_empty());
        std::fs::write(&path, "{\"version\":1,\"sessions\":{}}").unwrap();
        assert!(read_store(&path).is_ok());
        for invalid in ["{", "{\"version\":2,\"sessions\":{}}", "{\"version\":1,\"sessions\":{},\"x\":1}"] {
            std::fs::write(&path, invalid).unwrap();
            assert!(read_store(&path).is_err(), "{invalid}");
        }
        std::fs::write(&path, " ".repeat(MAX_FILE as usize + 1)).unwrap();
        assert!(read_store(&path).is_err());
        let mut store = Store::default();
        update(&mut store, &"c".repeat(64), &record(), &request(json!({})), NOW, ok);
        persist(&path, &store).unwrap();
        assert_eq!(read_store(&path).unwrap().sessions, store.sessions);
    }

    fn claude(ttl: Value, expires: Value) -> (Value, Value) {
        (json!({"agent":"claude","model":"claude-opus-5-5"}),
         json!({"prompt_cache":{"status":"warm","ttl_seconds":ttl,"expires_at":expires}}))
    }

    #[test]
    fn claude_uses_the_ttl_its_last_request_wrote() {
        let (record, usage) = claude(json!(3600), json!(NOW + 3000.0));
        let rule = eligibility(&record, &usage, None);
        assert_eq!((rule.supported, rule.ttl, rule.source), (true, Some(3600), "native_usage"));
        assert_eq!(rule.interval(), Some(2880));
        // The last request was at NOW - 600; the ping is due 48 minutes after it.
        assert_eq!(rule.due_at(), Some(NOW - 600.0 + 2880.0));
        let (record, usage) = claude(json!(300), json!(NOW + 200.0));
        let short = eligibility(&record, &usage, None);
        assert_eq!((short.supported, short.reason_code), (false, "claude_short_ttl"));
        assert!(short.reason.contains("5-minute"), "{}", short.reason);
        // Before the first reply reports a cache, keep-warm can be on but waits.
        let waiting = eligibility(&json!({"agent":"claude"}), &json!({}), None);
        assert_eq!((waiting.supported, waiting.reason_code, waiting.due_at()), (true, "no_cache_report", None));
    }

    /// A Claude estimate that ran out is a cold cache: the round says so and
    /// waits for the next real turn, instead of waiting for a lifetime report
    /// that only the next reply could bring.
    #[test]
    fn a_lapsed_claude_estimate_is_a_cold_cache_not_a_missing_report() {
        let mut record = idle();
        record["agent"] = json!("claude");
        record["model"] = json!("claude-opus-5-5");
        let lapsed = json!({"prompt_cache":{"status":"unknown","ttl_seconds":3600,"lapsed_at":NOW - 2400.0}});
        let rule = eligibility(&record, &lapsed, None);
        assert_eq!((rule.supported, rule.reason_code, rule.ttl, rule.expires_at), (true, "", Some(3600), Some(NOW - 2400.0)));
        let observed = Observed { record: &record, live: true, usage: lapsed };
        let cold = decide(&enabled(), &Value::Null, Some(&observed), quiet, NOW);
        assert!(matches!(&cold, Step::Hold { state: "cold", reason, since: Some(_), .. } if reason.starts_with("The cache went cold.")), "{cold:?}");
        // A lifetime the agent never reported still waits for its next reply.
        let unreported = Observed { record: &record, live: true, usage: json!({"prompt_cache":{"status":"unknown"}}) };
        let waiting = decide(&enabled(), &Value::Null, Some(&unreported), quiet, NOW);
        assert!(matches!(&waiting, Step::Hold { state: "waiting", reason, .. } if reason.contains("report its cache lifetime")), "{waiting:?}");
    }

    #[test]
    fn codex_keeps_thirty_minutes_only_on_models_with_that_documented_cache() {
        for model in ["gpt-5.6", "gpt-5.6-codex", "gpt-6.1-sol", "gpt-7"] {
            let rule = eligibility(&json!({"agent":"codex","model":model}), &json!({}), Some(NOW));
            assert_eq!((rule.supported, rule.ttl, rule.source), (true, Some(1800), "provider_default"), "{model}");
            assert_eq!(rule.due_at(), Some(NOW + 1440.0));
        }
        for model in ["gpt-5.5", "gpt-5", "o3", "codex-mini-latest", "gpt-4.1"] {
            let rule = eligibility(&json!({"agent":"codex","model":model}), &json!({}), Some(NOW));
            assert_eq!((rule.supported, rule.reason_code), (false, "codex_model"), "{model}");
        }
        // The launch argument names the model before Codex reports it.
        let launched = json!({"agent":"codex","base":["codex","-m","gpt-6.1-sol"]});
        assert!(eligibility(&launched, &json!({}), Some(NOW)).supported);
        let unknown = eligibility(&json!({"agent":"codex"}), &json!({}), Some(NOW));
        assert_eq!((unknown.supported, unknown.reason_code), (true, "no_cache_report"));
        // No finished turn yet: nothing to keep warm.
        assert_eq!(eligibility(&json!({"agent":"codex","model":"gpt-6.1-sol"}), &json!({}), None).due_at(), None);
    }

    #[test]
    fn kimi_follows_its_own_cache_duration_estimates() {
        // An empty Kimi home: the built-in table decides.
        let empty = tempfile::tempdir().unwrap();
        let bare = empty.path().to_str().unwrap().to_owned();
        let rule = |model: &str| eligibility(&json!({"agent":"kimi","model":model,"agent_home":bare}), &json!({}), Some(NOW));
        assert_eq!(rule("kimi-for-coding").ttl, Some(3600));
        assert_eq!(rule("kimi-for-coding-highspeed").ttl, Some(3600));
        assert_eq!(rule("k3").ttl, Some(1800));
        assert_eq!(rule("k3.1-pro").ttl, Some(1800));
        assert_eq!(rule("k3").source, "kimi_table");
        assert_eq!((rule("moonshot-v1").supported, rule("moonshot-v1").reason_code), (false, "kimi_model"));
        let dir = tempfile::tempdir().unwrap();
        let configs = dir.path().join("cache/client-configs");
        std::fs::create_dir_all(&configs).unwrap();
        std::fs::write(configs.join("estimated_cache_duration.json"), json!({"version":1,"fetchedAt":1,
            "config":{"version":1,"config":{"k3":{"min_tokens_to_hint":1,"cache_duration":1200}}}}).to_string()).unwrap();
        std::fs::write(configs.join("mainland-cn%3A%2Fclient_configs%3Aestimated_cache_duration.json"), json!({"version":1,"fetchedAt":2,
            "config":{"version":1,"config":{"k3":{"min_tokens_to_hint":1,"cache_duration":2400},"k4":{"min_tokens_to_hint":1,"cache_duration":3600}}}}).to_string()).unwrap();
        let home = dir.path().to_str().unwrap();
        let configured = |model: &str| eligibility(&json!({"agent":"kimi","model":model,"agent_home":home}), &json!({}), Some(NOW));
        // The newest fetched configuration decides; it can add models.
        assert_eq!((configured("k3").ttl, configured("k3").source), (Some(2400), "kimi_config"));
        assert_eq!(configured("k4").ttl, Some(3600));
    }

    #[test]
    fn deepseek_opencode_and_unknown_agents_are_not_offered() {
        for agent in ["dsh", "opencode", "sh"] {
            let rule = eligibility(&json!({"agent":agent}), &json!({}), Some(NOW));
            assert_eq!((rule.supported, rule.reason_code), (false, if agent == "dsh" { "deepseek" } else { "agent" }));
            assert!(!rule.reason.is_empty());
        }
    }

    fn idle() -> Value {
        json!({"name":"claude/project/warm","agent":"claude","run_id":"run-1","conversation_id":"conversation-1",
            "activity":"idle","phase":"idle","active_tools":{},"subagents":{}})
    }

    fn quiet() -> Facts {
        Facts { background: Some(0), schedules: 0, recovery_active: false, client_input: Some(None) }
    }

    #[test]
    fn a_ping_goes_only_to_a_settled_agent_with_nothing_else_pending() {
        let mut store = Store::default();
        update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        let entry = store.sessions.get("owner");
        let runtime = json!({"state":"dispatching","request_id":"ping-1","revision":1});
        let warm = Eligibility::known(3600, "native_usage", Some(NOW + 600.0));
        let refusal = |record: &Value, entry: Option<&Entry>, runtime: &Value, facts: &Facts, now: f64| {
            submission_refusal(record, entry, runtime, facts, &warm, "ping-1", now)
        };
        assert_eq!(refusal(&idle(), entry, &runtime, &quiet(), NOW), None);
        // The setting changed or expired, or another ping is current.
        let off = Entry { enabled: false, ..entry.unwrap().clone() };
        assert!(refusal(&idle(), Some(&off), &runtime, &quiet(), NOW).is_some());
        assert!(refusal(&idle(), None, &runtime, &quiet(), NOW).is_some());
        assert!(refusal(&idle(), entry, &runtime, &quiet(), NOW + 3.0 * 3600.0).is_some());
        assert!(refusal(&idle(), entry, &json!({"state":"dispatching","request_id":"ping-0"}), &quiet(), NOW).is_some());
        assert!(refusal(&idle(), entry, &json!({"state":"waiting","request_id":"ping-1"}), &quiet(), NOW).is_some());
        // The agent works, or work or input waits.
        for (key, value) in [("activity", json!("busy")), ("phase", json!("tool")),
            ("active_tools", json!({"t":{"name":"Bash"}})), ("keep_warm_turn", json!({"request_id":"ping-0"})),
            ("launch_prompt", json!({"status":"pending"})), ("paused", json!(true))] {
            let mut busy = idle();
            busy[key] = value;
            assert!(refusal(&busy, entry, &runtime, &quiet(), NOW).is_some(), "{key}");
        }
        for facts in [Facts { background: Some(1), ..quiet() }, Facts { background: None, ..quiet() },
            Facts { schedules: 1, ..quiet() }, Facts { recovery_active: true, ..quiet() },
            // Unknown terminal activity, or someone typed in Terminal moments ago.
            Facts { client_input: None, ..quiet() }, Facts { client_input: Some(Some(NOW - 30.0)), ..quiet() }] {
            assert!(refusal(&idle(), entry, &runtime, &facts, NOW).is_some(), "{facts:?}");
        }
        assert_eq!(refusal(&idle(), entry, &runtime, &Facts { client_input: Some(Some(NOW - 121.0)), ..quiet() }, NOW), None);
    }

    #[test]
    fn a_ping_decided_for_an_older_setting_is_not_typed() {
        let mut store = Store::default();
        update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        let longer = request(json!({"hours":7,"expected_revision":1}));
        update(&mut store, "owner", &record(), &longer, NOW, ok);
        let warm = Eligibility::known(3600, "native_usage", Some(NOW + 600.0));
        let decided = |revision: u64| json!({"state":"dispatching","request_id":"ping-1","revision":revision});
        let entry = store.sessions.get("owner");
        assert!(submission_refusal(&idle(), entry, &decided(1), &quiet(), &warm, "ping-1", NOW).is_some());
        assert!(submission_refusal(&idle(), entry, &json!({"state":"dispatching","request_id":"ping-1"}), &quiet(), &warm, "ping-1", NOW).is_some());
        assert_eq!(submission_refusal(&idle(), entry, &decided(2), &quiet(), &warm, "ping-1", NOW), None);
    }

    #[test]
    fn the_cache_clock_belongs_to_the_current_conversation() {
        let mut record = ended(600.0);
        assert_eq!(last_turn_at(&record), Some(NOW - 600.0));
        // Clear started a new conversation: the old clock says nothing about it.
        record["conversation_id"] = json!("conversation-2");
        assert_eq!(last_turn_at(&record), None);
        let rule = eligibility(&record, &Value::Null, last_turn_at(&record));
        assert_eq!(rule.due_at(), None);
        assert!(matches!(step(&enabled(), Value::Null, Some(&record), quiet(), Some(None)), Step::Hold { state: "waiting", .. }));
        // Legacy evidence without a conversation is no evidence.
        let mut legacy = idle();
        legacy["recovery_success_at"] = json!(NOW - 600.0);
        legacy["cache_touched_at"] = json!(NOW - 600.0);
        assert_eq!(last_turn_at(&legacy), None);
    }

    #[test]
    fn a_person_prompt_during_dispatch_cancels_the_pending_ping_mark() {
        let mut record = pinged();
        let person = deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":"Stop and look at this"})));
        assert!(person.get("keep_warm").is_none());
        assert!(record.get("keep_warm_pending").is_none());
        // The ping's own hook arriving later is then just text, never hidden.
        let late = deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        assert!(late.get("keep_warm").is_none());
        assert!(record.get("keep_warm_turn").is_none());
    }

    #[test]
    fn a_legacy_label_rename_carries_the_setting() {
        let (old, new) = ("claude/p/old", "claude/p/new");
        let mut store = Store::default();
        update(&mut store, &storage::label_key(old), &record(), &request(json!({})), NOW, ok);
        assert!(rename_entry(&mut store, old, new));
        assert!(!store.sessions.contains_key(&storage::label_key(old)));
        let moved = store.sessions.get(&storage::label_key(new)).unwrap();
        assert_eq!((moved.name.as_str(), moved.enabled, moved.revision), (new, true, 1));
        // Replaying the rename changes nothing.
        assert!(!rename_entry(&mut store, old, new));
        // A free legacy label can hold only a stale setting of a session that
        // is gone: the renamed session's own setting replaces it.
        update(&mut store, &storage::label_key("claude/p/other"), &record(), &request(json!({"hours":7})), NOW, ok);
        assert!(rename_entry(&mut store, new, "claude/p/other"));
        let replaced = store.sessions.get(&storage::label_key("claude/p/other")).unwrap();
        assert_eq!((replaced.name.as_str(), replaced.until), ("claude/p/other", Some(NOW + 3.0 * 3600.0)));
    }

    #[test]
    fn the_last_check_before_typing_needs_a_cache_that_is_still_warm() {
        let mut store = Store::default();
        update(&mut store, "owner", &record(), &request(json!({})), NOW, ok);
        let entry = store.sessions.get("owner");
        let runtime = json!({"state":"dispatching","request_id":"ping-1","revision":1});
        let refusal = |rule: &Eligibility, now: f64| submission_refusal(&idle(), entry, &runtime, &quiet(), rule, "ping-1", now);
        // Staging took long enough that the cache is within 30 s of expiry, or past it.
        let expiring = Eligibility::known(1800, "provider_default", Some(NOW + 29.0));
        assert!(refusal(&expiring, NOW).is_some());
        assert!(refusal(&expiring, NOW + 60.0).is_some());
        assert!(refusal(&Eligibility::known(1800, "provider_default", None), NOW).is_some());
        assert!(refusal(&Eligibility::refused("codex_model", "old"), NOW).is_some());
        assert!(refusal(&Eligibility::waiting("no report"), NOW).is_some());
        assert_eq!(refusal(&Eligibility::known(1800, "provider_default", Some(NOW + 31.0)), NOW), None);
    }

    #[test]
    fn only_session_ids_and_label_keys_name_settings() {
        let label = "a".repeat(64);
        let id = "0f8b5f7e-3c2a-4d1e-9b6a-7c5d4e3f2a1b";
        for owner in [label.as_str(), id] {
            assert!(owner_valid(owner), "{owner}");
            assert!(runtime_path(owner).is_some());
        }
        for owner in ["/tmp/canary", "../escape", "nested/key", "", "A".repeat(64).as_str(), "short", "a".repeat(65).as_str()] {
            assert!(!owner_valid(owner), "{owner}");
            assert!(runtime_path(owner).is_none(), "{owner}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep-warm.json");
        let entry = json!({"name":"claude/p/warm","enabled":true,"until":null,"revision":1,"set_at":NOW});
        std::fs::write(&path, json!({"version":1,"sessions":{"/tmp/canary":entry}}).to_string()).unwrap();
        assert!(read_store(&path).is_err());
        std::fs::write(&path, json!({"version":1,"sessions":{id:entry}}).to_string()).unwrap();
        assert!(read_store(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("keep-warm.json");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || sender.send((read_store(&path).is_err(), read_runtime_at(&path).is_err())).unwrap());
        assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), (true, true));
        let link = dir.path().join("link.json");
        std::fs::write(dir.path().join("real.json"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.json"), &link).unwrap();
        assert!(read_runtime_at(&link).is_err());
    }

    #[test]
    fn a_malformed_worker_state_is_never_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.json");
        assert_eq!(read_runtime_at(&path).unwrap(), Value::Null);
        std::fs::write(&path, "{\"state\":\"dispatch").unwrap();
        assert!(read_runtime_at(&path).is_err());
        std::fs::write(&path, "[]").unwrap();
        assert!(read_runtime_at(&path).is_err());
        std::fs::write(&path, json!({"state":"warming","request_id":"ping-1","sent_at":NOW}).to_string()).unwrap();
        assert_eq!(read_runtime_at(&path).unwrap()["state"], "warming");
        // A well-formed object that is not a state the worker writes is unknown, not none.
        for invalid in [json!({}), json!({"state":""}), json!({"state":"bogus"}), json!({"state":5}),
            json!({"state":"dispatching"}), json!({"state":"dispatching","request_id":""}),
            json!({"state":"warming","request_id":"ping-1"}), json!({"state":"waiting","since":"yesterday"}),
            json!({"state":"waiting","reason":7}), json!({"state":"cold","since":f64::MAX * 0.0 + 1.0, "next_at":"soon"})] {
            std::fs::write(&path, invalid.to_string()).unwrap();
            assert!(read_runtime_at(&path).is_err(), "{invalid}");
        }
        for valid in [json!({"state":"waiting","reason":"","next_at":null,"since":null}), json!({"state":"uncertain","reason":"x","since":NOW}),
            json!({"state":"dispatching","request_id":"ping-1","revision":1,"since":NOW}), json!({"state":"gone","reason":"x","since":NOW,"pings":null})] {
            std::fs::write(&path, valid.to_string()).unwrap();
            assert!(read_runtime_at(&path).is_ok(), "{valid}");
        }
        // A round that cannot read its state sends nothing until a real turn.
        assert!(matches!(unreadable(NOW), Step::Hold { state: "uncertain", since: Some(at), .. } if at == NOW));
    }

    fn hook(kind: &str, extra: Value) -> Value {
        let mut event = json!({"hook_event_name":kind,"agent_id":""});
        for (key, value) in extra.as_object().unwrap() { event[key] = value.clone(); }
        event
    }

    fn pinged() -> Value {
        let mut record = idle();
        record.as_object_mut().unwrap().extend(json!({"prompt":"Fix the tests","last_message":"Done.",
            "last_event_at":NOW - 2880.0,"turn_started":NOW - 3000.0,"input_pending_at":NOW,
            "keep_warm_pending":{"request_id":"request-1","ping":"ping-1","submitted_at":NOW}}).as_object().unwrap().clone());
        record
    }

    /// One hook through observe, a stand-in for update_activity, then settle.
    fn deliver(record: &mut Value, mut event: Value) -> Value {
        observe(record, &mut event, NOW);
        let kind = string(&event, "hook_event_name").to_owned();
        record["last_event_at"] = json!(NOW + 1.0);
        if kind == "UserPromptSubmit" { record["prompt"] = event["prompt"].clone(); }
        if kind == "Stop" { record["last_message"] = event["last_assistant_message"].clone(); }
        settle(record, &event, NOW + 1.0);
        event
    }

    #[test]
    fn a_ping_turn_is_marked_and_leaves_the_visible_session_unchanged() {
        let mut record = pinged();
        let prompt = deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        assert_eq!(prompt["keep_warm"], "prompt");
        assert!(record["keep_warm_turn"].is_object());
        assert!(record.get("keep_warm_pending").is_none());
        let reply = deliver(&mut record, hook("Stop", json!({"last_assistant_message":"OK"})));
        assert_eq!(reply["keep_warm"], "turn");
        assert!(record.get("keep_warm_turn").is_none());
        assert_eq!(record["cache_touched"], json!({"at":NOW + 1.0,"conversation_id":"conversation-1"}));
        assert!(record.get("keep_warm_real_stop_at").is_none());
        // Listings keep the person's prompt, reply and time.
        let mut output = json!({"prompt":TEXT,"last_message":"OK","last_event_at":NOW + 1.0,"turn_started":NOW});
        visible(&mut output, &record);
        assert_eq!(output, json!({"prompt":"Fix the tests","last_message":"Done.",
            "last_event_at":NOW - 2880.0,"turn_started":NOW - 3000.0}));
        // The person's next message makes everything visible again.
        let next = deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":"Next task"})));
        assert!(next.get("keep_warm").is_none());
        assert!(record.get("keep_warm_quiet").is_none());
        deliver(&mut record, hook("Stop", json!({"last_assistant_message":"Done again."})));
        assert_eq!(record["keep_warm_real_stop_at"], json!(NOW + 1.0));
    }

    #[test]
    fn a_message_the_person_queues_during_a_ping_makes_the_turn_theirs() {
        let mut record = pinged();
        deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        let queued = deliver(&mut record, hook("UserPromptQueued", json!({"prompt":"Also check CI"})));
        assert!(queued.get("keep_warm").is_none());
        assert!(record.get("keep_warm_turn").is_none());
        assert!(record.get("keep_warm_quiet").is_none());
        let stop = deliver(&mut record, hook("Stop", json!({"last_assistant_message":"CI is green."})));
        assert!(stop.get("keep_warm").is_none());
    }

    #[test]
    fn the_ping_text_alone_is_not_a_ping() {
        // Typed by someone, or after the pending ping expired: an ordinary prompt.
        let mut record = idle();
        let typed = deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        assert!(typed.get("keep_warm").is_none());
        let mut stale = pinged();
        stale["keep_warm_pending"]["submitted_at"] = json!(NOW - 600.0);
        let late = deliver(&mut stale, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        assert!(late.get("keep_warm").is_none());
        assert!(stale.get("keep_warm_pending").is_none());
        // Subagent events never join or end the main ping turn.
        let mut record = pinged();
        deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
        let child = deliver(&mut record, hook("SubagentStop", json!({"agent_id":"worker"})));
        assert!(child.get("keep_warm").is_none());
        assert!(record["keep_warm_turn"].is_object());
    }

    #[test]
    fn a_failed_ping_ends_its_turn() {
        for kind in ["StopFailure", "Interrupt"] {
            let mut record = pinged();
            deliver(&mut record, hook("UserPromptSubmit", json!({"prompt":TEXT})));
            let failed = deliver(&mut record, hook(kind, json!({"error":"rate_limit"})));
            assert_eq!(failed["keep_warm"], "turn", "{kind}");
            assert!(record.get("keep_warm_turn").is_none(), "{kind}");
        }
    }

    fn enabled() -> Entry {
        Entry { name: "codex/project/warm".into(), enabled: true, until: Some(NOW + 3600.0), revision: 1, set_at: NOW - 60.0 }
    }

    /// An idle Codex session whose last turn ended `ago` seconds before NOW.
    fn ended(ago: f64) -> Value {
        let mut record = idle();
        record["agent"] = json!("codex");
        record["model"] = json!("gpt-6.1-sol");
        record["cache_touched"] = json!({"at":NOW - ago,"conversation_id":record["conversation_id"]});
        record
    }

    fn step(entry: &Entry, runtime: Value, record: Option<&Value>, facts: Facts, input: Option<Option<f64>>) -> Step {
        let observed = record.map(|record| Observed { record, live: true, usage: Value::Null });
        decide(entry, &runtime, observed.as_ref(), || Facts { client_input: input, ..facts }, NOW)
    }

    fn held(state: &str) -> impl Fn(&Step) -> bool + '_ {
        move |step| matches!(step, Step::Hold { state: held, .. } if *held == state)
    }

    #[test]
    fn the_round_pings_only_a_due_warm_idle_session() {
        let due = ended(1500.0);
        assert_eq!(step(&enabled(), Value::Null, Some(&due), quiet(), Some(None)), Step::Send);
        // Typing in an attached terminal two minutes ago no longer blocks it.
        assert_eq!(step(&enabled(), Value::Null, Some(&due), quiet(), Some(Some(NOW - 121.0))), Step::Send);
        assert!(held("waiting")(&step(&enabled(), Value::Null, Some(&due), quiet(), Some(Some(NOW - 30.0)))));
        // Unknown terminal activity never admits a ping.
        let unknown = step(&enabled(), Value::Null, Some(&due), quiet(), None);
        assert!(held("waiting")(&unknown), "{unknown:?}");
        // Not due yet: it says when.
        let early = step(&enabled(), Value::Null, Some(&ended(600.0)), quiet(), Some(None));
        assert_eq!(early, Step::Hold { state: "waiting", reason: String::new(), next_at: Some(NOW + 840.0), since: None });
        // Past or within 30 s of the estimated expiry: cold, nothing sent.
        assert!(held("cold")(&step(&enabled(), Value::Null, Some(&ended(1775.0)), quiet(), Some(None))));
        assert!(held("cold")(&step(&enabled(), Value::Null, Some(&ended(4000.0)), quiet(), Some(None))));
        for facts in [Facts { background: Some(1), ..quiet() }, Facts { background: None, ..quiet() },
            Facts { schedules: 2, ..quiet() }, Facts { recovery_active: true, ..quiet() }] {
            assert!(held("waiting")(&step(&enabled(), Value::Null, Some(&due), facts, Some(None))));
        }
        let mut busy = due.clone();
        busy["activity"] = json!("busy");
        assert!(held("waiting")(&step(&enabled(), Value::Null, Some(&busy), quiet(), Some(None))));
        let mut old = due.clone();
        old["model"] = json!("gpt-5.5");
        assert!(held("unsupported")(&step(&enabled(), Value::Null, Some(&old), quiet(), Some(None))));
    }

    #[test]
    fn expiry_and_a_vanished_session_turn_the_setting_off() {
        let late = Entry { until: Some(NOW - 1.0), ..enabled() };
        assert!(matches!(step(&late, Value::Null, Some(&ended(1500.0)), quiet(), Some(None)), Step::Off("expired", _)));
        assert!(matches!(step(&enabled(), Value::Null, None, quiet(), Some(None)), Step::Off("gone", _)));
        let always = Entry { until: None, ..enabled() };
        assert_eq!(step(&always, Value::Null, Some(&ended(1500.0)), quiet(), Some(None)), Step::Send);
    }

    #[test]
    fn missed_windows_and_unknown_deliveries_wait_for_the_next_real_turn() {
        for state in ["cold", "uncertain", "stopped"] {
            let runtime = json!({"state":state,"reason":"kept","since":NOW - 100.0});
            let mut record = ended(1500.0);
            assert!(held(state)(&step(&enabled(), runtime.clone(), Some(&record), quiet(), Some(None))), "{state}");
            record["keep_warm_real_stop_at"] = json!(NOW - 50.0);
            assert_eq!(step(&enabled(), runtime, Some(&record), quiet(), Some(None)), Step::Send, "{state}");
        }
        // A round that died while sending never sends again on its own.
        let dispatching = json!({"state":"dispatching","request_id":"ping-1","since":NOW - 5.0});
        assert!(held("uncertain")(&step(&enabled(), dispatching, Some(&ended(1500.0)), quiet(), Some(None))));
        // A sent ping waits for its turn; without one it becomes uncertain.
        let warming = json!({"state":"warming","request_id":"ping-1","sent_at":NOW - 20.0,"since":NOW - 20.0});
        assert!(held("warming")(&step(&enabled(), warming, Some(&ended(1500.0)), quiet(), Some(None))));
        let silent = json!({"state":"warming","request_id":"ping-1","sent_at":NOW - TURN_SECONDS - 1.0,"since":NOW - 700.0});
        assert!(held("uncertain")(&step(&enabled(), silent, Some(&ended(1500.0)), quiet(), Some(None))));
        let answered = json!({"state":"warming","request_id":"ping-1","sent_at":NOW - 20.0,"since":NOW - 20.0});
        assert!(held("waiting")(&step(&enabled(), answered, Some(&ended(10.0)), quiet(), Some(None))));
        // A failed ping stops keep-warm until the person's next turn.
        let mut failed = ended(1500.0);
        failed["phase"] = json!("error");
        failed["recovery_user_at"] = json!(NOW - 20.0);
        failed["keep_warm_prompt_at"] = json!(NOW - 20.0);
        assert!(held("stopped")(&step(&enabled(), Value::Null, Some(&failed), quiet(), Some(None))));
    }

    #[test]
    fn turning_the_setting_off_keeps_a_delivery_fence() {
        for fenced in ["uncertain", "dispatching", "warming"] {
            let previous = json!({"state":fenced,"reason":"kept","since":NOW - 50.0,"request_id":"ping-1","sent_at":NOW - 60.0});
            let off = off_runtime(&previous, "gone", "The session is no longer available.", NOW);
            assert_eq!((off["state"].clone(), off["since"].clone()), (json!("uncertain"), json!(NOW - 50.0)), "{fenced}");
            assert!(runtime_valid(&off));
        }
        let stopped = json!({"state":"stopped","reason":"failed","since":NOW - 50.0});
        assert_eq!(off_runtime(&stopped, "expired", "ended", NOW), stopped);
        let plain = off_runtime(&json!({"state":"waiting","pings":3}), "expired", "ended", NOW);
        assert_eq!(plain, json!({"state":"expired","reason":"ended","since":NOW,"pings":3}));
        assert_eq!(off_runtime(&Value::Null, "gone", "x", NOW)["state"], "gone");
    }

    #[test]
    fn a_paused_or_stopped_agent_gets_nothing() {
        let record = ended(1500.0);
        let observed = Observed { record: &record, live: false, usage: Value::Null };
        assert!(held("paused")(&decide(&enabled(), &Value::Null, Some(&observed), quiet, NOW)));
    }

    #[test]
    fn requests_need_exactly_one_valid_duration_when_enabling() {
        let parsed = |body: Value| parse(body.to_string().as_bytes());
        let base = |extra: Value| {
            let mut value = json!({"expected_run_id":"run-1","expected_conversation_id":"conversation-1",
                "expected_revision":0,"enabled":true});
            for (key, v) in extra.as_object().unwrap() { value[key] = v.clone(); }
            value
        };
        for invalid in [json!({}), json!({"hours":0}), json!({"hours":8}), json!({"hours":3,"always":true}),
            json!({"enabled":false,"hours":3}), json!({"enabled":false,"always":true}), json!({"hours":3,"extra":1})] {
            assert!(parsed(base(invalid.clone())).is_err(), "{invalid}");
        }
        for valid in [json!({"hours":1}), json!({"hours":7}), json!({"always":true}), json!({"enabled":false})] {
            assert!(parsed(base(valid.clone())).is_ok(), "{valid}");
        }
    }
}
