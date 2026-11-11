//! OpenCode 2's shared background service, authenticated on loopback.
//! Bindings hold Zerus and session identities, never provider keys.
use super::*;
#[cfg_attr(not(unix), allow(dead_code))]
mod catalog;
#[cfg_attr(not(unix), allow(dead_code))]
mod favorites;
mod forms;
mod history;
#[cfg_attr(not(unix), allow(dead_code))]
mod providers;
mod service;
mod terminal;
pub(crate) use terminal::attach;
#[cfg_attr(not(unix), allow(dead_code))]
mod shortlist;
#[cfg(unix)]
pub(crate) use favorites::set_favorite;
// Every platform validates a favorite change, so --dry-run answers alike.
pub(crate) use favorites::validate as validate_favorite;
// Accounts reads the catalog, the install state and starts the service only on Unix.
#[cfg(unix)]
pub(crate) use catalog::{catalog, install_state, models, start_from_accounts};
// Accounts fits the OpenCode row only on Unix.
#[cfg(unix)]
pub(crate) use shortlist::{fit_row, ROW_BUDGET};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::time::{Duration, Instant};

fn binding_path(name: &str) -> PathBuf {
    PROVIDER.label_path(name)
}
thread_local! {
    /// Whether this thread holds opencode/bindings.lock. Code running under
    /// an action's own lock (its inspection, an adoption) never waits on it.
    static BINDINGS_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// opencode/bindings.lock, released when dropped. A guard taken while this
/// thread already holds the lock holds nothing more. It stays on the thread
/// that took it, whose flag it clears.
pub(super) struct Bindings(Option<Guard>, std::marker::PhantomData<*const ()>);
impl Drop for Bindings {
    fn drop(&mut self) {
        if self.0.take().is_some() {
            BINDINGS_HELD.with(|held| held.set(false));
        }
    }
}
fn bindings_lock() -> PathBuf {
    root().join("opencode/bindings.lock")
}
/// The bindings lock, waiting as long as another process holds it.
pub(super) fn lock_bindings() -> Result<Bindings> {
    lock_bindings_at(&bindings_lock())
}
fn lock_bindings_at(path: &Path) -> Result<Bindings> {
    if BINDINGS_HELD.with(std::cell::Cell::get) {
        return Ok(Bindings(None, std::marker::PhantomData));
    }
    let guard = lock(Some(path))?;
    BINDINGS_HELD.with(|held| held.set(true));
    Ok(Bindings(Some(guard), std::marker::PhantomData))
}
/// The bindings lock within `wait`, or None while another process holds it.
pub(super) fn try_bindings(wait: Duration) -> Option<Bindings> {
    try_bindings_at(&bindings_lock(), wait)
}
fn try_bindings_at(path: &Path, wait: Duration) -> Option<Bindings> {
    if BINDINGS_HELD.with(std::cell::Cell::get) {
        return Some(Bindings(None, std::marker::PhantomData));
    }
    let guard = lock_until(path, Instant::now() + wait).ok()??;
    BINDINGS_HELD.with(|held| held.set(true));
    Some(Bindings(Some(guard), std::marker::PhantomData))
}
const PROVIDER: provider_binding::Provider = provider_binding::Provider::OpenCode;
pub(crate) fn exists(name: &str) -> bool { PROVIDER.exists(name) }
pub(super) fn binding(name: &str) -> Result<Value> { PROVIDER.read(name) }
pub(super) fn read_id(id: &str) -> Result<Value> { PROVIDER.read_id(id) }
pub(super) fn save(record: &mut Value) -> Result<()> { PROVIDER.save(record) }
pub(super) fn binding_records() -> Vec<Value> { PROVIDER.records() }
pub(super) fn native_id(id: &str, prefix: &str) -> bool {
    id.starts_with(prefix) && id.len() <= 128
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
pub(crate) fn config_home() -> PathBuf {
    nonempty_env("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            nonempty_env("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config"))
                .join("opencode")
        })
}
/// A fresh OpenCode identifier: the prefix and 26 random letters or digits.
pub(super) fn new_id(prefix: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut suffix = String::with_capacity(26);
    // Bytes 6 and 8 of a v4 UUID carry fixed version and variant bits, so
    // only the other 14 are random. 248 = 4 * 62: bytes at or above it are
    // dropped to avoid modulo bias.
    while suffix.len() < 26 {
        for (index, byte) in uuid::Uuid::new_v4().into_bytes().into_iter().enumerate() {
            if index != 6 && index != 8 && byte < 248 && suffix.len() < 26 {
                suffix.push(ALPHABET[byte as usize % ALPHABET.len()] as char);
            }
        }
    }
    format!("{prefix}{suffix}")
}
fn session_path(record: &Value, suffix: &str) -> String {
    format!("/api/session/{}{suffix}", string(record, "conversation_id"))
}
use service::Response;

fn attach_hint(native: &dyn Native, record: &mut Value) {
    attach_hint_with(
        native,
        record,
        agent_hint::for_account("native-opencode").is_some(),
        save,
    )
}

/// The instruction entry is an effective prompt update, not transcript history.
/// OpenCode can repeat a retained entry after a lost answer, so persist the
/// suppression mark first and never replay an uncertain attachment.
fn attach_hint_with(
    native: &dyn Native,
    record: &mut Value,
    enabled: bool,
    persist: impl FnOnce(&mut Value) -> Result<()>,
) {
    if !enabled || !agent_hint::claim(record) || persist(record).is_err() {
        return;
    }
    let path = format!(
        "/api/experimental/session/{}/instructions/entries/zerus.agent-hint",
        string(record, "conversation_id")
    );
    let _ = native.raw("PUT", &path, Some(&json!({"value":agent_hint::HINT})));
}

/// One newest message answers whether the native conversation has turns. The
/// caller treats an unavailable or malformed answer as nonempty, never as
/// permission to attach or as a reason to block the launch.
fn empty_history(native: &dyn Native, record: &Value) -> Result<bool> {
    let answer = get(native, &session_path(record, "/message?limit=1"))?;
    answer["data"]
        .as_array()
        .map(|messages| messages.is_empty())
        .ok_or_else(|| "Invalid OpenCode message history".into())
}

pub(crate) fn account_context(name: &str) -> Result<Value> {
    let record = binding(name)?;
    Ok(
        json!({"provider":"opencode","id":"native-opencode","home":record["agent_home"],"run_id":record["run_id"]}),
    )
}
enum Legacy {
    Current,
    Adopted,
    Unsupported,
}
/// Pure decision for a binding written before the OpenCode 2 adapter: it is
/// adopted only when the migration completed and OpenCode 2 returns exactly
/// this session in exactly this directory.
fn legacy_verdict(record: &Value, migration: &Value, session: Option<&Value>) -> Legacy {
    if record["service"] == "opencode2" {
        return Legacy::Current;
    }
    let (Some(id), Some(cwd)) = (
        record["conversation_id"].as_str().filter(|v| !v.is_empty()),
        record["cwd"].as_str().filter(|v| !v.is_empty()),
    ) else {
        return Legacy::Unsupported;
    };
    let data = session.map(|value| &value["data"]);
    if migration["status"] == "completed"
        && data.is_some_and(|data| {
            data["id"].as_str() == Some(id) && data["location"]["directory"].as_str() == Some(cwd)
        })
    {
        Legacy::Adopted
    } else {
        Legacy::Unsupported
    }
}
type Reply = std::result::Result<Response, String>;
/// Server text as an OpenCode reason presents it. As `journal::clipped`:
/// escape sequences and C0 controls other than tabs and line breaks removed,
/// at most 300 characters plus an ellipsis. Unlike it, DEL and C1 controls are
/// removed too, before the cap, so they never count toward it.
pub(super) fn server_reason(text: &str) -> String {
    let clean: String = journal::clipped(&json!(text), usize::MAX)
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    journal::clipped(&json!(clean), 300)
}
/// OpenCode's refusal as a session row or error carries it: the server's own
/// message as `server_reason` presents it.
fn refusal_text(response: &Response) -> String {
    let reason = response.body["message"]
        .as_str()
        .or(response.body["data"]["message"].as_str())
        .unwrap_or("Native operation refused");
    format!(
        "OpenCode HTTP {}: {}",
        response.status,
        server_reason(reason)
    )
}
fn successful(response: &Response) -> bool {
    (200..300).contains(&response.status)
}
/// Whether the migration finished. Only a successful reply answers; any
/// transport or HTTP failure is an error, never "not migrated".
fn migration_completed(reply: &Reply) -> Result<bool> {
    match reply {
        Err(why) => Err(why.clone()),
        Ok(response) if !successful(response) => Err(refusal_text(response)),
        Ok(response) => {
            let body = if response.body["data"].is_object() {
                &response.body["data"]
            } else {
                &response.body
            };
            Ok(body["status"] == "completed")
        }
    }
}
/// The verdict once the migration completed: a missing session is
/// Unsupported, any other failure is an error.
fn settled_verdict(record: &Value, session: &Reply) -> Result<Legacy> {
    match session {
        Err(why) => Err(why.clone()),
        Ok(response) if response.status == 404 => Ok(Legacy::Unsupported),
        Ok(response) if !successful(response) => Err(refusal_text(response)),
        Ok(response) => Ok(legacy_verdict(
            record,
            &json!({"status": "completed"}),
            Some(&response.body),
        )),
    }
}
/// The mapping from the two native replies to a verdict. The flag says the
/// verdict is settled for this service (the migration finished) and may be
/// cached; the session is requested only after a completed migration.
fn legacy_decision(
    record: &Value,
    migration: &Reply,
    session: impl FnOnce() -> Reply,
) -> Result<(Legacy, bool)> {
    if record["service"] == "opencode2" {
        return Ok((Legacy::Current, false));
    }
    if !migration_completed(migration)? {
        return Ok((Legacy::Unsupported, false));
    }
    settled_verdict(record, &session()).map(|verdict| (verdict, true))
}
/// `migration` is fetched at most once per caller and only when needed.
fn legacy_state(
    service: &service::Service,
    record: &Value,
    migration: &mut Option<Reply>,
) -> Result<(Legacy, bool)> {
    if record["service"] == "opencode2" {
        return Ok((Legacy::Current, false));
    }
    let reply =
        migration.get_or_insert_with(|| service.raw("GET", "/api/experimental/migration/v1", None));
    legacy_decision(record, reply, || {
        service.raw("GET", &session_path(record, ""), None)
    })
}
/// Drop OpenCode 1 fields and mark the binding as an OpenCode 2 binding.
fn modernize(record: &mut Value) {
    let map = record.as_object_mut().unwrap();
    for key in [
        "paused",
        "native_data_home",
        "native_config_file",
        "creation_pending",
        "creation_body",
    ] {
        map.remove(key);
    }
    record["service"] = json!("opencode2");
}
const VERDICT_LIMIT: usize = 5000;
fn verdict_path() -> PathBuf {
    root().join("opencode/legacy-verdicts.json")
}
fn verdict_key(record: &Value, registration: &service::Registration) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        string(record, "name"),
        string(record, "conversation_id"),
        registration.id,
        registration.pid
    )
}
fn load_verdicts() -> Vec<String> {
    let Ok(file) = fs::File::open(verdict_path()) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > 4 * 1024 * 1024
    {
        return Vec::new();
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| {
            value["unsupported"].as_array().map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .take(VERDICT_LIMIT)
                    .collect()
            })
        })
        .unwrap_or_default()
}
fn save_verdicts(keys: &[String]) {
    let path = verdict_path();
    if let Some(parent) = path.parent() {
        if private_dir(parent).is_err() {
            return;
        }
    }
    let _ = atomic(&path, &json!({"unsupported": keys}).to_string());
}
/// Rewrite an adopted binding so it is never checked again. Skipped when the
/// binding lock is busy; the next poll tries again.
fn adopt_binding(name: &str, conversation_id: &str) {
    let Some(_guard) = try_bindings(Duration::from_millis(200)) else {
        return;
    };
    if let Ok(mut record) = binding(name) {
        if record["conversation_id"] == conversation_id {
            modernize(&mut record);
            let _ = save(&mut record);
        }
    }
}
/// The binding keeps its pending creation, so launching the same session
/// name again reconciles it by its ID; OpenCode 2 sessions have no Resume.
const CREATION_MISMATCH: &str = "OpenCode creation acknowledgement did not match; inspect the session, then launch the same session name again to reconcile it";
enum Refusal {
    /// The service definitely refused the creation (HTTP 4xx).
    Definite(String),
    Other(String),
}
fn creation_reply(
    reply: Reply,
    acknowledged: &dyn Fn(&Value) -> bool,
) -> std::result::Result<(), Refusal> {
    match reply {
        Ok(response) if successful(&response) => {
            if acknowledged(&response.body) {
                Ok(())
            } else {
                Err(Refusal::Other(CREATION_MISMATCH.into()))
            }
        }
        Ok(response) if (400..500).contains(&response.status) => {
            Err(Refusal::Definite(refusal_text(&response)))
        }
        Ok(response) => Err(Refusal::Other(refusal_text(&response))),
        Err(why) => Err(Refusal::Other(why)),
    }
}
/// After a lost or uncertain creation reply: ask for this exact ID, and only
/// when it does not exist POST the same body once (the ID makes it idempotent).
fn reconcile_creation(
    get: &mut dyn FnMut() -> Reply,
    post: &mut dyn FnMut() -> Reply,
    acknowledged: &dyn Fn(&Value) -> bool,
) -> std::result::Result<(), Refusal> {
    if let Ok(response) = get() {
        if successful(&response) {
            return creation_reply(Ok(response), acknowledged);
        }
    }
    creation_reply(post(), acknowledged)
}
/// The first POST; a definite refusal or acknowledgement ends it, anything
/// uncertain is reconciled.
fn create_session(
    get: &mut dyn FnMut() -> Reply,
    post: &mut dyn FnMut() -> Reply,
    acknowledged: &dyn Fn(&Value) -> bool,
) -> std::result::Result<(), Refusal> {
    match post() {
        Ok(response) if successful(&response) || (400..500).contains(&response.status) => {
            creation_reply(Ok(response), acknowledged)
        }
        _ => reconcile_creation(get, post, acknowledged),
    }
}
pub(crate) fn launch(
    name: &str,
    cwd: &str,
    launch_id: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    dry: bool,
    new_only: bool,
) -> Result<()> {
    if dry {
        println!("Create native OpenCode session {name} in {cwd}");
        return Ok(());
    }
    let parent = parent_link::launcher_link()?;
    let _writer = lock(None)?;
    let _guard = lock_bindings()?;
    if storage::label_taken(name) || live()?.contains_key(name) {
        return Err(
            "This name belongs to an existing terminal session; it was left untouched".into(),
        );
    }
    let mode = crate::accounts::permission_mode(
        &crate::config::Config::load().map_err(|e| e.message)?,
        "native-opencode",
        "opencode",
    )
    .map_err(|e| e.message)?;
    if mode != "provider" {
        return Err("OpenCode uses its native permission configuration; select Provider settings for this account".into());
    }
    let existing = PROVIDER.existing_for_launch(name)?;
    let present = existing.is_some();
    if present && parent_link::claimed_launch() { return Err("--parent applies only to new sessions; use hgs parent adopt for an existing session".into()); }
    if present && new_only {
        return Err("This OpenCode session name already exists".into());
    }
    let cwd = if present {
        None
    } else {
        let cwd = crate::platform::canonical(cwd).map_err(|e| e.to_string())?;
        if !cwd.is_dir() {
            return Err("OpenCode requires an existing directory".into());
        }
        Some(cwd)
    };
    let service = service::ensure_started()?;
    if let Some(mut record) = existing {
        if resumes_pending(&record) {
            return resume_pending(&service, record);
        }
        let (verdict, _) = legacy_state(&service, &record, &mut None)?;
        let adopted = match verdict {
            Legacy::Current => false,
            Legacy::Adopted => true,
            Legacy::Unsupported => {
                return Err(
                    "This conversation belongs to OpenCode 1, which Zerus no longer supports"
                        .into(),
                )
            }
        };
        verify_session(&service, &record)?;
        if adopted {
            modernize(&mut record);
            save(&mut record)?;
        }
        if empty_history(&service, &record).unwrap_or(false) {
            attach_hint(&service, &mut record);
        }
        return Ok(());
    }
    let cwd = cwd.unwrap();
    let selected = model
        .map(|value| catalog::offered_model(&service, &cwd, value, effort.unwrap_or("")))
        .transpose()?;
    let mut record = json!({"backend":"opencode","service":"opencode2","agent":"opencode","name":name,"cwd":cwd,"launch_id":launch_id,"run_id":uuid::Uuid::new_v4().to_string(),"conversation_id":new_id("ses_"),"created":now(),"agent_home":config_home(),"creation_pending":true});
    if let Some(parent) = parent { record["parent"] = parent; }
    let mut body = creation_body(&record);
    if let Some(model) = selected {
        body["model"] = model;
    }
    // Persist the chosen native ID and exact body before the first creation
    // attempt so one launch never has two identities.
    record["creation_body"] = body;
    save(&mut record)?;
    // A child its proven parent launched starts in the parent's project,
    // written after the state and bindings locks are released (zerus-d5twt).
    let child = record
        .get("parent")
        .filter(|link| crate::swarm::placing_parent(link))
        .map(|link| {
            (
                link.clone(),
                string(&record, "run_id").to_owned(),
                record["conversation_id"].clone(),
            )
        });
    finish_creation(&service, record, true)?;
    if let Some((parent, run, conversation)) = child {
        drop(_guard);
        drop(_writer);
        if let Ok(config) = crate::config::Config::load() {
            crate::swarm::place_child_session(&config, name, &cwd, &run, &conversation, &parent);
        }
    } else if let Ok(config) = crate::config::Config::load() {
        crate::swarm::infer_new_session(&config, name, &cwd);
    }
    Ok(())
}
fn creation_body(record: &Value) -> Value {
    let name = string(record, "name");
    let tag = name.rsplit('/').next().unwrap_or(name);
    json!({"id":record["conversation_id"],"location":{"directory":record["cwd"]},"title":tag,"metadata":{"zerus":{"run_id":record["run_id"],"launch_id":record["launch_id"]}}})
}
/// Only OpenCode 2 bindings are reconciled against the service; an OpenCode 1
/// binding goes through the legacy verdict and is never POSTed or deleted.
fn resumes_pending(record: &Value) -> bool {
    record["creation_pending"] == true && record["service"] == "opencode2"
}
/// A binding left with creation_pending (crash or lost reply) is reconciled
/// instead of blocking its name.
fn resume_pending(service: &service::Service, record: Value) -> Result<()> {
    finish_creation(service, record, false)
}
fn finish_creation(service: &service::Service, mut record: Value, first: bool) -> Result<()> {
    let body = if record["creation_body"].is_object() {
        record["creation_body"].clone()
    } else {
        creation_body(&record)
    };
    let acknowledged = |value: &Value| {
        value["data"]["id"] == record["conversation_id"]
            && value["data"]["location"]["directory"] == record["cwd"]
    };
    let path = session_path(&record, "");
    let mut get = || service.raw("GET", &path, None);
    let mut post = || service.raw("POST", "/api/session", Some(&body));
    let result = if first {
        create_session(&mut get, &mut post, &acknowledged)
    } else {
        reconcile_creation(&mut get, &mut post, &acknowledged)
    };
    match result {
        Ok(()) => {
            let map = record.as_object_mut().unwrap();
            map.remove("creation_pending");
            map.remove("creation_body");
            save(&mut record)?;
            attach_hint(service, &mut record);
            Ok(())
        }
        Err(Refusal::Definite(reason)) => {
            let _ = PROVIDER.remove(&record);
            Err(reason)
        }
        Err(Refusal::Other(reason)) => Err(reason),
    }
}
fn verify_session(native: &dyn Native, record: &Value) -> Result<Value> {
    exact_session(record, get(native, &session_path(record, ""))?)
}
/// The session object of a `GET /api/session/{id}` answer, only when it is
/// exactly this binding's session in this binding's directory.
fn exact_session(record: &Value, answer: Value) -> Result<Value> {
    if answer["data"]["id"] != record["conversation_id"]
        || answer["data"]["location"]["directory"] != record["cwd"]
    {
        return Err("The exact native OpenCode session or directory changed".into());
    }
    Ok(answer["data"].clone())
}
/// What one inspection read from the service for one session.
struct Observed {
    /// The session object.
    session: Value,
    active: bool,
    page: history::Page,
    /// The `/inbox` response.
    inbox: Value,
    forms: Value,
    permissions: Value,
    /// The Zerus request that sent each of the newest user messages, from the
    /// receipt index (message ID to request ID).
    requests: std::collections::HashMap<String, String>,
}
/// The status of one binding; `None` when the service could not be read,
/// which keeps native state unknown rather than stopped.
fn normalize(record: &Value, observed: Option<&Observed>) -> Value {
    let available = observed.is_some();
    let null = Value::Null;
    let session = observed.map_or(&null, |o| &o.session);
    let active = observed.is_some_and(|o| o.active);
    let messages: &[Value] = observed.map_or(&[], |o| &o.page.messages);
    let pending = observed.map_or_else(Vec::new, |o| forms::cards(record, &o.forms, &o.permissions));
    let failure = history::last_error(messages);
    let phase = if !available {
        "unknown"
    } else if !pending.is_empty() {
        "input"
    } else if active {
        "working"
    } else if failure.is_some() {
        "error"
    } else {
        "idle"
    };
    let (activity, summary) = match phase {
        "input" => ("attention", "Needs attention"),
        "working" => ("busy", "Working"),
        "error" => ("attention", "Provider request failed"),
        "idle" => ("idle", "Ready"),
        _ => ("unknown", "Inspection unavailable"),
    };
    let delivered = history::rows(messages);
    // A message admitted between the inbox and history reads is shown once.
    let ids: std::collections::HashSet<&str> = messages.iter().map(|m| string(m, "id")).collect();
    let queued: Vec<Value> = observed
        .map_or_else(Vec::new, |o| history::queued_rows(&o.inbox, !active))
        .into_iter()
        .filter(|row| !ids.contains(string(row, "message_id")))
        .collect();
    // A reply becomes the session's reply once OpenCode completed it, so a
    // read mark taken while it streams never covers its final text.
    let reply = delivered
        .iter()
        .rev()
        .find(|e| e["type"] == "AgentMessage" && e["streaming"] != true);
    let prompt = delivered
        .iter()
        .rev()
        .find(|e| e["type"] == "UserPromptSubmit");
    let turn_started = prompt.and_then(|r| r["at"].as_f64()).unwrap_or(0.0);
    let mut allowed = if available {
        vec!["rename", "forget"]
    } else {
        vec!["forget"]
    };
    if !active
        && queued
            .iter()
            .any(|row| row["delivery_state"] == "not_delivered")
    {
        allowed.push("remove_queued");
    }
    let model = &session["model"];
    let settings_model = match (string(model, "providerID"), string(model, "id")) {
        ("", _) | (_, "") => Value::Null,
        (provider, id) => json!(format!("{provider}/{id}")),
    };
    let effort = model["variant"]
        .as_str()
        .filter(|variant| !variant.is_empty() && *variant != "default")
        .map_or(Value::Null, |variant| json!(variant));
    let (state, process_state, runtime_state) = if available {
        ("running", "running", "live")
    } else {
        ("unknown", "unknown", "unknown")
    };
    let parts = string(record, "name").splitn(3, '/').collect::<Vec<_>>();
    let idle = available && pending.is_empty();
    let mut events = delivered.clone();
    events.extend(queued);
    // A prompt Zerus sent names its request, so a client can match its own
    // message even when OpenCode dates a queued prompt by its delivery.
    if let Some(observed) = observed {
        for row in events.iter_mut().filter(|e| e["type"] == "UserPromptSubmit") {
            if let Some(request) = observed.requests.get(string(row, "message_id")) {
                row["request_id"] = json!(request);
            }
        }
    }
    let mut value = json!({"name":record["name"],"cmd":"opencode","agent":"opencode","backend":"opencode","project":parts.get(1),"tag":parts.get(2),"run_id":record["run_id"],"conversation_id":record["conversation_id"],"launch_id":record["launch_id"],"created":record["created"],"cwd":record["cwd"],"cwd_source":"native_session","tracked":true,"resumable":true,"attached":0,"clients":[],
        "state":state,"process_state":process_state,"runtime_state":runtime_state,"conversation_state":"active","phase":phase,"activity":activity,"activity_summary":summary,
        "activity_detail":reply.map(|r|r["detail"].clone()),"prompt":prompt.map(|r|r["detail"].clone()),"reply_id":reply.map(|r|r["reply_id"].clone()),"reply_at":reply.map(|r|r["at"].clone()),
        "last_event_at":session["time"]["updated"].as_f64().unwrap_or(0.0)/1000.0,"turn_started":turn_started,"account_id":"native-opencode","account_home":record["agent_home"],
        "model":model["id"],"model_provider":model["providerID"],"settings_model":settings_model,"effort":effort});
    if !string(record, "session_id").is_empty() {
        value["session_id"] = record["session_id"].clone();
    }
    if let Some(parent) = parent_link::listed(record) { value["parent"] = parent; }
    let controls = json!({"send_available":idle,"send_supported":idle,"settings_change_supported":idle && !active,"interrupt_supported":available && active,
        "terminal_supported":false,"terminal_reason":backend::OPENCODE_TERMINAL_REASON.text(),
        "terminal_attach_supported":cfg!(unix) && available && record["service"] == "opencode2",
        "terminal_attach_reason":if available { "" } else { "Refresh this session after its OpenCode service reconnects." },
        "native_ui_available":false,"bulk_lifecycle_supported":false,"attachments_supported":false,
        "clear_context_supported":false,"clear_context_reason":"OpenCode context reset is not available in this adapter",
        "compact_context_supported":false,"compact_context_reason":"OpenCode compaction arrives in a later update",
        "allowed_actions":allowed,"action_reasons":{"pause":"OpenCode sessions have no separate process to pause","resume":"OpenCode sessions have no separate process to pause",
            "archive":"OpenCode archives are not supported","restore":"OpenCode archives are not supported","fork":"OpenCode fork arrives in a later update","terminate":"Use Stop for this OpenCode session"},
        "pending_questions":pending,"events":events,"replace_events":true,"cursor":0,"history_truncated":observed.is_some_and(|o| o.page.truncated)});
    for (key, item) in controls.as_object().into_iter().flatten() {
        value[key] = item.clone();
    }
    if !pending.is_empty() {
        value["attention_id"] = json!(format!(
            "opencode_question:{}",
            pending
                .iter()
                .filter_map(|q| q["question_id"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let (Some(failure), "error") = (&failure, phase) {
        let error = provider_errors::event(
            &format!("opencode-error-{}", string(failure, "id")),
            failure["at"].as_f64().unwrap_or(0.0),
            string(failure, "message"),
            "opencode_native",
        );
        provider_errors::apply(&mut value, &error);
    }
    value
}
/// A row without a reading of the session: no events, so clients keep the
/// transcript they already show, and the reason as its detail.
fn unread(record: &Value, reason: &str) -> Value {
    let mut row = normalize(record, None);
    if let Some(map) = row.as_object_mut() {
        map.remove("events");
        map.remove("replace_events");
    }
    row["activity_detail"] = json!(reason);
    row["inspection_reason"] = json!(reason);
    row
}
/// A binding whose service could not be read: unknown, with the reason.
fn unavailable(record: &Value, reason: String) -> Value {
    let mut row = unread(record, &reason);
    row["inspection_unavailable"] = json!(true);
    row
}
/// A binding that OpenCode 2 did not migrate: stopped, and only Forget.
fn legacy_row(record: &Value) -> Value {
    let mut row = unread(
        record,
        "This conversation belongs to OpenCode 1, which Zerus no longer supports",
    );
    row["state"] = json!("stopped");
    row["process_state"] = json!("exited");
    row["runtime_state"] = json!("stopped");
    row["activity_summary"] = json!("OpenCode 1 conversation");
    row
}
/// One GET that must succeed.
fn get(native: &dyn Native, path: &str) -> Result<Value> {
    let response = native.raw("GET", path, None)?;
    if successful(&response) {
        return Ok(response.body);
    }
    Err(refusal_text(&response))
}
/// The sessions OpenCode is running now, keyed by session ID.
fn active_sessions(native: &dyn Native) -> Result<Value> {
    let active = get(native, "/api/session/active")?;
    if !active["data"].is_object() {
        return Err("Invalid OpenCode session activity".into());
    }
    Ok(active["data"].clone())
}
fn pending_requests(native: &dyn Native, record: &Value, suffix: &str) -> Result<Value> {
    let answer = get(native, &session_path(record, suffix))?;
    if !answer["data"].is_array() {
        return Err("Invalid OpenCode pending requests".into());
    }
    Ok(answer["data"].clone())
}
/// Everything one session's status needs, given the shared activity map.
fn observe(
    native: &dyn Native,
    record: &Value,
    active: &Value,
    history: impl FnOnce() -> Result<history::Page>,
    requests: bool,
) -> Result<Observed> {
    let session = exact_session(record, get(native, &session_path(record, ""))?)?;
    let forms = pending_requests(native, record, "/form")?;
    let permissions = pending_requests(native, record, "/permission")?;
    // The inbox first: an item admitted meanwhile is then in the history
    // read; one listed in both is shown once.
    let inbox = get(native, &session_path(record, "/inbox"))?;
    if !inbox["data"].is_array() {
        return Err("Invalid OpenCode inbox".into());
    }
    let page = history()?;
    // Only a full inspection shows events; listings and actions drop them.
    let requests = if requests {
        sent_requests(&Ledger::current(), record, &page.messages, &inbox)
    } else {
        Default::default()
    };
    Ok(Observed {
        session,
        active: active.get(string(record, "conversation_id")).is_some(),
        page,
        inbox,
        forms,
        permissions,
        requests,
    })
}
/// At most this many of the newest user messages are looked up.
const SENT_LOOKUPS: usize = 100;
/// A receipt larger than this is not read for attribution.
const RECEIPT_READ: u64 = 512 * 1024;
/// The Zerus requests that sent the newest user messages of this session:
/// queued ones first, then history newest first. A message is attributed
/// only when its body is exactly the text its receipt sent, as `reconcile`
/// requires; the same ID with another body stays unattributed.
fn sent_requests(
    ledger: &Ledger,
    record: &Value,
    messages: &[Value],
    inbox: &Value,
) -> std::collections::HashMap<String, String> {
    // A message delivered between the inbox and history reads is judged by
    // its delivered copy only.
    let delivered: std::collections::HashSet<&str> =
        messages.iter().map(|m| string(m, "id")).collect();
    let queued = inbox["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| !delivered.contains(string(item, "id")))
        .map(|item| (item, &item["payload"]["text"]));
    queued
        .chain(messages.iter().rev().map(|message| (message, &message["text"])))
        .filter(|(m, _)| m["type"] == "user")
        .map(|(m, text)| (string(m, "id"), text))
        .filter(|(id, _)| native_id(id, "msg_"))
        .take(SENT_LOOKUPS)
        .filter_map(|(id, text)| {
            let index = ledger.indexed(id)?;
            if index["conversation_id"] != record["conversation_id"] {
                return None;
            }
            let request = string(&index, "request_id");
            let receipt = ledger.sent(request)?;
            (receipt["native_message_id"] == id
                && receipt["conversation_id"] == record["conversation_id"]
                && receipt["run_id"] == record["run_id"]
                && text.is_string()
                && receipt["text"] == *text)
                .then(|| (id.to_owned(), request.to_owned()))
        })
        .collect()
}
#[derive(Clone, Copy, PartialEq)]
enum Detail {
    /// A session list row: status only.
    Listing,
    /// The session's own state for an action.
    Own,
    /// `hgs inspect`: also the model options for its folder.
    Full,
}
fn processes() -> Value {
    json!({"items":[],"complete":false,"source":"opencode_native","reason":"Individual OpenCode shell process controls are not yet qualified"})
}
fn parent_view(
    service: &service::Service,
    record: &Value,
    active: &Value,
    detail: Detail,
) -> Result<Value> {
    let observed = observe(
        service,
        record,
        active,
        || history::fetch(service, record),
        detail == Detail::Full,
    )?;
    let mut result = normalize(record, Some(&observed));
    if detail != Detail::Listing {
        result["attachment_messages"] = attachments::messages(record, "");
        result["voice_messages"] =
            conversation_history::inspected_voice_messages(record, &result, None);
    }
    if detail == Detail::Full {
        let pinned: Vec<String> = ["settings_model", "pending_model"]
            .iter()
            .filter_map(|key| result[*key].as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        let pinned: Vec<&str> = pinned.iter().map(String::as_str).collect();
        let options = catalog::session_options(&PathBuf::from(string(record, "cwd")), &pinned);
        result["model_options"] = options["model_options"].clone();
        result["model_options_truncated"] = options["model_options_truncated"].clone();
    }
    let questions = result["pending_questions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    read_marks::enrich(
        &mut result,
        &read_marks::load(),
        string(record, "name"),
        Some(&questions),
    );
    Ok(result)
}

/// GET-only preference baseline. Never adopts a legacy binding or starts a service.
pub(super) fn notification_snapshot(record: &Value) -> Result<(Value, Option<Vec<Value>>)> {
    if record["service"] != "opencode2" { return Err("baseline_unavailable".into()); }
    let service = service::connect().map_err(|_| "baseline_unavailable")?;
    notification_snapshot_with(&service, record, || history::fetch(&service, record))
}
/// Every read of one observation ends by the same absolute deadline.
struct Bounded<'a> {
    service: &'a service::Service,
    deadline: Instant,
}
impl Native for Bounded<'_> {
    fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
        service::request(&self.service.reg, method, path, body, self.deadline)
    }
    fn raw_until(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        deadline: Instant,
    ) -> Result<Response> {
        service::request(
            &self.service.reg,
            method,
            path,
            body,
            deadline.min(self.deadline),
        )
    }
}
/// Whether a claimed parent runs now: the same observation as its status, every
/// read ending within `limit` of the service connection. No service is a
/// definite no; a service that has not answered by then is busy, never live.
/// (Windows reports a refused loopback connection only after about two seconds
/// of SYN retries, so a service that vanishes mid-check can read as busy there.)
pub(super) fn claim_live(record: &Value, limit: Duration) -> Result<bool> {
    if record["service"] != "opencode2" {
        return Ok(false);
    }
    let Ok(service) = service::connect() else {
        return Ok(false);
    };
    claim_live_on(&service, record, Instant::now() + limit)
}
fn claim_live_on(service: &service::Service, record: &Value, deadline: Instant) -> Result<bool> {
    let bounded = Bounded { service, deadline };
    match notification_snapshot_with(&bounded, record, || history::fetch(&bounded, record)) {
        Ok((row, _)) => Ok(row["state"] == "running" && row["process_state"] == "running"),
        Err(_) if Instant::now() >= deadline => Err(parent_link::CLAIM_BUSY.into()),
        Err(_) => Ok(false),
    }
}
fn notification_snapshot_with(native: &dyn Native, record: &Value, history: impl FnOnce() -> Result<history::Page>) -> Result<(Value, Option<Vec<Value>>)> {
    let active = active_sessions(native)?;
    let observed = observe(native, record, &active, history, false)?;
    let result = normalize(record, Some(&observed));
    let questions = result["pending_questions"].as_array().cloned();
    Ok((result, questions))
}

/// The short list a full answer carries when the session cannot be read.
type CachedOptions = fn(&service::Unavailable, &Path, &[&str]) -> Value;
/// An answer without the session's own state. A full one still carries the
/// short list from the last home catalog, never read from the service, so
/// clients do not mistake these details for an older hgs's.
fn without_session(
    record: &Value,
    why: &service::Unavailable,
    reason: String,
    detail: Detail,
    cached_options: CachedOptions,
) -> Value {
    let mut row = unavailable(record, reason);
    if detail == Detail::Full {
        let pinned: Vec<String> = ["settings_model", "pending_model"]
            .iter()
            .filter_map(|key| row[*key].as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        let pinned: Vec<&str> = pinned.iter().map(String::as_str).collect();
        let options = cached_options(why, &PathBuf::from(string(record, "cwd")), &pinned);
        row["model_options"] = options["model_options"].clone();
        row["model_options_truncated"] = options["model_options_truncated"].clone();
    }
    row
}
/// Inspects bindings against one service connection: at most one
/// `/api/session/active` and one migration check per instance.
struct Inspector {
    service: std::result::Result<service::Service, service::Unavailable>,
    /// Named only when a reason needs it.
    computer: Option<String>,
    active: Option<Result<Value>>,
    migration: Option<Reply>,
    verdicts: Option<Vec<String>>,
    verdicts_changed: bool,
    /// The cached home short list; tests stand in for the cache.
    cached_options: CachedOptions,
}
impl Inspector {
    /// Never starts the service.
    fn connect() -> Self {
        Inspector {
            service: service::connect(),
            computer: None,
            active: None,
            migration: None,
            verdicts: None,
            verdicts_changed: false,
            cached_options: catalog::stopped_options,
        }
    }
    fn row(&mut self, record: &Value, detail: Detail) -> Value {
        let mut row = self.view(record, detail);
        if detail != Detail::Listing && row.get("processes").is_none() {
            row["processes"] = processes();
        }
        row
    }
    fn view(&mut self, record: &Value, detail: Detail) -> Value {
        let cached_options = self.cached_options;
        let service = match &self.service {
            Err(why) => {
                let computer = self.computer.get_or_insert_with(service::computer);
                let reason = why.reason(computer);
                return without_session(record, why, reason, detail, cached_options);
            }
            Ok(service) => service,
        };
        let key = verdict_key(record, &service.reg);
        let known = record["service"] != "opencode2"
            && self
                .verdicts
                .get_or_insert_with(load_verdicts)
                .contains(&key);
        let state = if known {
            Ok((Legacy::Unsupported, false))
        } else {
            legacy_state(service, record, &mut self.migration)
        };
        let mut current = record.clone();
        // A failed read keeps the cached short list, like a stopped service.
        let failed = |reason: String| {
            let why = service::Unavailable::Unreachable(reason.clone());
            without_session(record, &why, reason, detail, cached_options)
        };
        match state {
            Err(reason) => return failed(reason),
            Ok((Legacy::Unsupported, settled)) => {
                if settled && !known {
                    let keys = self.verdicts.get_or_insert_with(load_verdicts);
                    if keys.len() >= VERDICT_LIMIT {
                        keys.clear();
                    }
                    keys.push(key);
                    self.verdicts_changed = true;
                }
                return legacy_row(record);
            }
            Ok((Legacy::Adopted, _)) => {
                adopt_binding(string(record, "name"), string(record, "conversation_id"));
                modernize(&mut current);
            }
            Ok((Legacy::Current, _)) => {}
        }
        let active = match self.active.get_or_insert_with(|| active_sessions(service)) {
            Ok(active) => active,
            Err(reason) => return failed(reason.clone()),
        };
        parent_view(service, &current, active, detail).unwrap_or_else(failed)
    }
    fn finish(self) {
        if self.verdicts_changed {
            save_verdicts(self.verdicts.as_deref().unwrap_or(&[]));
        }
    }
}
pub(crate) fn inspect(name: &str, agent_id: Option<&str>) -> Result<Value> {
    let record = binding(name)?;
    if agent_id.is_some() {
        return Err("OpenCode child sessions arrive in a later update".into());
    }
    let mut inspector = Inspector::connect();
    let mut value = inspector.row(&record, Detail::Full);
    notifications::enrich(&mut value, notifications::load().as_ref(), journal::journal_name(&record));
    inspector.finish();
    waits::decorate(std::slice::from_mut(&mut value));
    Ok(value)
}
/// The session's own state for delivery, answer, interrupt and lifecycle
/// checks; unknown with its reason while the service is unavailable.
pub(crate) fn own_state(name: &str) -> Result<Value> {
    let record = binding(name)?;
    let mut inspector = Inspector::connect();
    let value = inspector.row(&record, Detail::Own);
    inspector.finish();
    Ok(value)
}
pub(crate) fn snapshots() -> Result<Vec<Value>> {
    let records = binding_records();
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let mut inspector = Inspector::connect();
    let mut rows = Vec::new();
    let preferences = notifications::load();
    for record in &records {
        let mut row = inspector.row(record, Detail::Listing);
        notifications::enrich(&mut row, preferences.as_ref(), journal::journal_name(record));
        if let Some(map) = row.as_object_mut() {
            for key in [
                "events",
                "voice_messages",
                "attachment_messages",
                "processes",
                "model_options",
                "model_options_truncated",
            ] {
                map.remove(key);
            }
        }
        rows.push(row);
    }
    inspector.finish();
    Ok(rows)
}
fn checked_request(record: &Value) -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(30 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 30 * 1024 * 1024 {
        return Err("Native request exceeds bounds".into());
    }
    let request: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let id =
        uuid::Uuid::parse_str(string(&request, "request_id")).map_err(|_| "Invalid request ID")?;
    if id.is_nil()
        || id.to_string() != string(&request, "request_id")
        || request["expected_run_id"] != record["run_id"]
        || request["expected_conversation_id"] != record["conversation_id"]
    {
        return Err("Session changed; refresh before trying again".into());
    }
    Ok(request)
}
/// The native OpenCode 2 operations that delivery needs.
trait Native {
    fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response>;
    /// The same request, ending no later than `deadline`.
    fn raw_until(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _deadline: Instant,
    ) -> Result<Response> {
        self.raw(method, path, body)
    }
}
impl Native for service::Service {
    fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
        service::Service::raw(self, method, path, body)
    }
    fn raw_until(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        deadline: Instant,
    ) -> Result<Response> {
        let deadline = deadline.min(Instant::now() + Duration::from_secs(10));
        service::request(&self.reg, method, path, body, deadline)
    }
}
#[derive(Debug)]
enum Delivery {
    Delivered,
    Queued,
    /// The service definitively rejected the message (a 4xx answer).
    Refused(String),
}
fn refusal_reason(response: &Response) -> String {
    let text = response.body["message"]
        .as_str()
        .or(response.body["data"]["message"].as_str())
        .unwrap_or("request rejected");
    let clean: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
    format!("HTTP {}: {clean}", response.status)
}
fn refused(reason: &str) -> String {
    if reason == REMOVED {
        return REMOVED.into();
    }
    format!("OpenCode refused this message: {reason}")
}
const MISMATCH: &str = "Native message acknowledgement did not match; delivery uncertain";
const UNCERTAIN: &str = "Native message ID has conflicting text; delivery uncertain";
const UNCERTAIN_STATE: &str = "Native delivery state is uncertain";
const UNCERTAIN_REPLAY: &str = "Delivery uncertain; this request will not be replayed. Inspect native history before creating a new request";
/// The definite non-delivery of a message the user removed from the queue.
const REMOVED: &str = "Removed from the OpenCode queue before delivery";
const REMOVAL_UNCONFIRMED: &str =
    "OpenCode did not confirm the removal; Zerus will not send this message again";
const REMOVALS_UNREADABLE: &str = "OpenCode removal records cannot be read";
const REMOVAL_LIMIT: usize = 5000;
/// Private delivery state under `opencode/`: receipts by request ID, an
/// index from native message ID to request ID, the removal records and the
/// lock that orders receipt changes.
struct Ledger(PathBuf);
#[derive(Clone, Copy, Debug, PartialEq)]
enum Removal {
    /// Written before the DELETE: forbids any repost, proves nothing.
    Intended,
    /// OpenCode confirmed the message left the queue without delivery.
    Removed,
}
impl Ledger {
    fn current() -> Self {
        Ledger(root().join("opencode"))
    }
    fn receipt(&self, request_id: &str) -> PathBuf {
        self.0.join("receipts").join(format!("{request_id}.json"))
    }
    fn index(&self, message_id: &str) -> PathBuf {
        self.0
            .join("receipt-index")
            .join(format!("{message_id}.json"))
    }
    fn lock(&self) -> Result<Guard> {
        lock(Some(&self.0.join("receipts.lock")))
    }
    fn removals_path(&self) -> PathBuf {
        self.0.join("removed-messages.json")
    }
    /// Every removal record, oldest first. A missing file has none; any
    /// other failure is an error, never "none".
    fn removals(&self) -> Result<Vec<Value>> {
        let file = match fs::File::open(self.removals_path()) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(REMOVALS_UNREADABLE.into()),
        };
        let mut bytes = Vec::new();
        if file
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > 4 * 1024 * 1024
        {
            return Err(REMOVALS_UNREADABLE.into());
        }
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value["removals"].as_array().cloned())
            .ok_or_else(|| REMOVALS_UNREADABLE.into())
    }
    fn removal(&self, record: &Value, message_id: &str) -> Result<Option<Removal>> {
        Ok(self
            .removals()?
            .iter()
            .find(|e| {
                e["conversation_id"] == record["conversation_id"] && e["message_id"] == message_id
            })
            .map(|e| {
                if e["state"] == "removed" {
                    Removal::Removed
                } else {
                    Removal::Intended
                }
            }))
    }
    /// Record a removal of one session's message, keeping the newest
    /// `REMOVAL_LIMIT`. Callers hold the bindings lock.
    fn record_removal(&self, record: &Value, message_id: &str, state: Removal) -> Result<()> {
        let mut entries = self.removals()?;
        entries.retain(|e| {
            e["conversation_id"] != record["conversation_id"] || e["message_id"] != message_id
        });
        let state = if state == Removal::Removed {
            "removed"
        } else {
            "intended"
        };
        entries.push(json!({"conversation_id":record["conversation_id"],"message_id":message_id,"state":state,"at":now()}));
        let excess = entries.len().saturating_sub(REMOVAL_LIMIT);
        entries.drain(..excess);
        atomic(
            &self.removals_path(),
            &json!({ "removals": entries }).to_string(),
        )
    }
    /// The receipt of one send, read within a bound; None when absent or
    /// unreadable.
    fn sent(&self, request_id: &str) -> Option<Value> {
        let mut bytes = Vec::new();
        fs::File::open(self.receipt(request_id))
            .ok()?
            .take(RECEIPT_READ + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > RECEIPT_READ {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }
    /// The request ID and session that sent a native message, if indexed.
    fn indexed(&self, message_id: &str) -> Option<Value> {
        let mut bytes = Vec::new();
        fs::File::open(self.index(message_id))
            .ok()?
            .take(4097)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 4096 {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        uuid::Uuid::parse_str(string(&value, "request_id")).ok()?;
        Some(value)
    }
}
/// Mark one receipt as a definite non-delivery after a confirmed removal.
fn refuse_removed(ledger: &Ledger, record: &Value, message_id: &str) -> Result<()> {
    let Some(index) = ledger.indexed(message_id) else {
        return Ok(());
    };
    let path = ledger.receipt(string(&index, "request_id"));
    let _guard = ledger.lock()?;
    let Ok(bytes) = fs::read(&path) else {
        return Ok(());
    };
    let mut receipt: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if receipt["native_message_id"] != message_id
        || receipt["conversation_id"] != record["conversation_id"]
        || receipt["status"] == "refused"
    {
        return Ok(());
    }
    receipt["status"] = json!("refused");
    receipt["reason"] = json!(REMOVED);
    atomic(&path, &receipt.to_string())
}
/// A receipt whose message was confirmed removed becomes that definite
/// non-delivery before anything else is read; nothing is sent for it.
fn apply_removal(ledger: &Ledger, record: &Value, receipt: &mut Value, path: &Path) -> Result<()> {
    let id = string(receipt, "native_message_id").to_owned();
    if receipt["status"] == "refused"
        || receipt["conversation_id"] != record["conversation_id"]
        || !native_id(&id, "msg_")
        || !matches!(ledger.removal(record, &id), Ok(Some(Removal::Removed)))
    {
        return Ok(());
    }
    let _guard = ledger.lock()?;
    receipt["status"] = json!("refused");
    receipt["reason"] = json!(REMOVED);
    atomic(path, &receipt.to_string())
}
/// Resolve one message ID exactly from native state. It is never posted
/// again: OpenCode 2.0.32 forgets an ID once its queued message is removed,
/// so a repost could deliver a message the user removed. A message in
/// neither history nor the inbox stays uncertain.
fn reconcile(
    native: &dyn Native,
    ledger: &Ledger,
    record: &Value,
    message_id: &str,
    text: &str,
) -> Result<Delivery> {
    let removal = ledger.removal(record, message_id)?;
    if removal == Some(Removal::Removed) {
        return Ok(Delivery::Refused(REMOVED.into()));
    }
    let found = native.raw(
        "GET",
        &session_path(record, &format!("/message/{message_id}")),
        None,
    )?;
    match found.status {
        200 => {
            let data = &found.body["data"];
            if data["type"] == "user" && data["id"] == message_id && data["text"] == text {
                return Ok(Delivery::Delivered);
            }
            return Err(UNCERTAIN.into());
        }
        404 if found.body["_tag"] == "MessageNotFoundError" => {}
        _ => return Err(UNCERTAIN_STATE.into()),
    }
    // An unconfirmed removal: only history can still answer.
    if removal.is_some() {
        return Err(UNCERTAIN_STATE.into());
    }
    let inbox = native.raw("GET", &session_path(record, "/inbox"), None)?;
    if inbox.status != 200 {
        return Err(UNCERTAIN_STATE.into());
    }
    match inbox.body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["id"] == message_id)
    {
        Some(item) if item["payload"]["text"] == text => Ok(Delivery::Queued),
        Some(_) => Err(UNCERTAIN.into()),
        None => Err(UNCERTAIN_STATE.into()),
    }
}
fn running_service() -> Result<service::Service> {
    // Sending is explicit, but phase 1 does not start the service for it.
    service::connect().map_err(|why| why.reason(&service::computer()))
}
fn check_receipt_identity(record: &Value, receipt: &Value) -> Result<()> {
    if !native_id(string(receipt, "native_message_id"), "msg_")
        || receipt["conversation_id"] != record["conversation_id"]
        || receipt["run_id"] != record["run_id"]
    {
        return Err("Native receipt identity changed".into());
    }
    Ok(())
}
/// Reconcile one receipt; a definite refusal is recorded in it.
fn settle(
    native: &dyn Native,
    ledger: &Ledger,
    record: &Value,
    receipt: &mut Value,
    path: &Path,
) -> Result<Delivery> {
    check_receipt_identity(record, receipt)?;
    let id = string(receipt, "native_message_id").to_owned();
    let text = string(receipt, "text").to_owned();
    let outcome = reconcile(native, ledger, record, &id, &text)?;
    // A removal recorded since the inbox was read wins over that read.
    let _guard = ledger.lock()?;
    let outcome = match (outcome, ledger.removal(record, &id)?) {
        (Delivery::Queued, Some(Removal::Intended)) => return Err(UNCERTAIN_STATE.into()),
        (Delivery::Queued, Some(Removal::Removed)) => Delivery::Refused(REMOVED.into()),
        (outcome, _) => outcome,
    };
    match &outcome {
        Delivery::Delivered | Delivery::Queued => receipt["status"] = json!("submitted"),
        Delivery::Refused(reason) => {
            receipt["status"] = json!("refused");
            receipt["reason"] = json!(reason);
        }
    }
    atomic(path, &receipt.to_string())?;
    Ok(outcome)
}
pub(super) fn reconcile_receipt(record: &Value, receipt: &mut Value) -> Result<bool> {
    reconcile_receipt_with(&Ledger::current(), record, receipt, running_service)
}
/// Core of `reconcile_receipt`: a confirmed removal answers without
/// connecting; otherwise the exact native session is read.
fn reconcile_receipt_with<N: Native>(
    ledger: &Ledger,
    record: &Value,
    receipt: &mut Value,
    connect: impl FnOnce() -> Result<N>,
) -> Result<bool> {
    let path = ledger.receipt(string(receipt, "request_id"));
    apply_removal(ledger, record, receipt, &path)?;
    match receipt["status"].as_str() {
        Some("submitted") => return Ok(true),
        Some("refused") => return Err(refused(string(receipt, "reason"))),
        _ => {}
    }
    let native = connect()?;
    verify_session(&native, record)?;
    match settle(&native, ledger, record, receipt, &path)? {
        Delivery::Refused(reason) => Err(refused(&reason)),
        _ => Ok(true),
    }
}
/// Whether a form or permission is waiting on the user for this session.
fn waiting_for_user(native: &dyn Native, record: &Value) -> Result<bool> {
    for suffix in ["/form", "/permission"] {
        let answer = native.raw("GET", &session_path(record, suffix), None)?;
        if answer.status != 200 {
            return Err("OpenCode pending requests could not be read; no message was sent".into());
        }
        if answer.body["data"].as_array().is_none_or(|v| !v.is_empty()) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn session_active(native: &dyn Native, record: &Value) -> bool {
    native
        .raw("GET", "/api/session/active", None)
        .ok()
        .filter(|r| r.status == 200)
        .is_some_and(|r| r.body["data"].get(string(record, "conversation_id")).is_some())
}
fn send(record: &Value, request: &Value) -> Result<Value> {
    if !string(request, "agent_id").is_empty() {
        return Err("OpenCode child delivery is not supported".into());
    }
    let text = string(request, "text");
    if text.len() > 65536 {
        return Err("Message exceeds64KiB".into());
    }
    // A first version cannot silently downgrade attachments or compaction
    // continuation into plain text.
    if request["attachments"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return Err("OpenCode attachments are not yet supported by this adapter".into());
    }
    if !string(request, "expected_compaction_id").is_empty() {
        return Err("OpenCode compaction continuation is not supported".into());
    }
    if text.trim().is_empty() {
        return Err("Message is empty".into());
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(json!([record["name"], request]).to_string().as_bytes())
    );
    let service = running_service()?;
    deliver(
        &service,
        &Ledger::current(),
        record,
        request,
        digest,
        session_graph::target_receipt(record),
    )
}
/// Core of `send`: everything after validation, against any native service.
fn deliver(
    native: &dyn Native,
    ledger: &Ledger,
    record: &Value,
    request: &Value,
    digest: String,
    graph_target: Option<Value>,
) -> Result<Value> {
    let text = string(request, "text");
    let path = &ledger.receipt(string(request, "request_id"));
    if path.exists() {
        let mut prior: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if prior["digest"] != digest {
            return Err("This request ID was already used for another message".into());
        }
        apply_removal(ledger, record, &mut prior, path)?;
        match prior["status"].as_str() {
            Some("submitted") => return Ok(prior),
            Some("refused") => return Err(refused(string(&prior, "reason"))),
            _ => {}
        }
        verify_native(native, record)?;
        return match settle(native, ledger, record, &mut prior, path) {
            Ok(Delivery::Refused(reason)) => Err(refused(&reason)),
            Ok(_) => Ok(prior),
            Err(_) => Err(UNCERTAIN_REPLAY.into()),
        };
    }
    verify_native(native, record)?;
    if waiting_for_user(native, record)? {
        return Err(
            "OpenCode is waiting for an exact question or permission; no message was sent".into(),
        );
    }
    let message_id = new_id("msg_");
    let mut receipt = json!({"request_id":request["request_id"],"name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],"digest":digest,"native_message_id":message_id,"text":text,"submitted_text":text,"submitted_at":now(),"at":now(),"status":"pending","attachments":[],"transport":"native_opencode"});
    if let Some(target) = graph_target {
        receipt["graph_target"] = target;
    }
    let queued = session_active(native, record);
    // The index first: a receipt is never written without it, so a removal
    // can find the receipt of the message it removes.
    atomic(
        &ledger.index(&message_id),
        &json!({"request_id":request["request_id"],"conversation_id":record["conversation_id"]})
            .to_string(),
    )?;
    atomic(path, &receipt.to_string())?;
    let posted = native.raw(
        "POST",
        &session_path(record, "/prompt"),
        Some(&json!({"id":message_id,"text":text,"delivery":"queue"})),
    );
    match posted {
        Ok(answer) if (200..300).contains(&answer.status) => {
            let data = &answer.body["data"];
            if data["id"] != message_id
                || data["sessionID"] != record["conversation_id"]
                || data["payload"]["text"] != text
            {
                return Err(MISMATCH.into());
            }
        }
        Ok(answer) if (400..500).contains(&answer.status) => {
            let reason = refusal_reason(&answer);
            receipt["status"] = json!("refused");
            // Keep the detailed first refusal local; a durable delivery
            // record needs only the confirmed HTTP status, not provider text.
            receipt["reason"] = json!(format!("HTTP {}", answer.status));
            atomic(path, &receipt.to_string())?;
            return Err(refused(&reason));
        }
        // A transport error or a 5xx leaves the admission unknown.
        _ => match settle(native, ledger, record, &mut receipt, path) {
            Ok(Delivery::Refused(reason)) => return Err(refused(&reason)),
            Ok(_) => {}
            Err(_) => return Err(UNCERTAIN_REPLAY.into()),
        },
    }
    receipt["status"] = json!("submitted");
    if queued {
        receipt["queued"] = json!(true);
    }
    atomic(path, &receipt.to_string())?;
    Ok(receipt)
}
fn verify_native(native: &dyn Native, record: &Value) -> Result<()> {
    let result = native.raw("GET", &session_path(record, ""), None)?;
    if result.status != 200
        || result.body["data"]["id"] != record["conversation_id"]
        || result.body["data"]["location"]["directory"] != record["cwd"]
    {
        return Err("The exact native OpenCode session or directory changed".into());
    }
    Ok(())
}
/// The route and body of one reply, from the pending card, the exact native
/// request it was made from, and the answers.
fn reply_for(card: &Value, native: &Value, answers: &[Value]) -> Result<(String, Value)> {
    let id = string(card, "question_id");
    if card["kind"] == "opencode_permission" {
        let decision = forms::permission_decision(answers)?;
        Ok((
            format!("/permission/{id}/reply"),
            json!({"decision": decision}),
        ))
    } else {
        Ok((
            format!("/form/{id}/reply"),
            forms::form_answer(card, native, answers)?,
        ))
    }
}
const CHANGED: &str = "This question changed or is no longer pending";
/// Server text safe to show: control characters removed, at most 200 chars.
fn clip_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}
fn answer(record: &Value, request: &Value) -> Result<Value> {
    let current = own_state(string(record, "name"))?;
    let card = current["pending_questions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|q| {
            q["question_id"] == request["question_id"]
                && q["question_hash"] == request["expected_question_hash"]
        })
        .ok_or(CHANGED)?;
    let path = root()
        .join("opencode/answers")
        .join(format!("{}.json", string(request, "request_id")));
    answer_with(&running_service()?, record, request, card, &path)
}
/// Core of `answer`: the journal, the exact native request and the reply.
fn answer_with(
    native: &dyn Native,
    record: &Value,
    request: &Value,
    card: &Value,
    path: &Path,
) -> Result<Value> {
    if path.exists() {
        return Err("This answer was already attempted; refresh its native acknowledgement".into());
    }
    let raw = request["answers"]
        .as_array()
        .ok_or("Answers are required")?;
    // The exact native request the card was made from, read again now.
    let suffix = if card["kind"] == "opencode_permission" {
        "/permission"
    } else {
        "/form"
    };
    let pending = pending_requests(native, record, suffix)?;
    let found = pending
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["id"] == card["question_id"])
        .ok_or(CHANGED)?;
    let none = json!([]);
    let one = json!([found]);
    let again = if suffix == "/form" {
        forms::cards(record, &one, &none)
    } else {
        forms::cards(record, &none, &one)
    };
    if again.first().map(|c| &c["question_hash"]) != Some(&card["question_hash"]) {
        return Err(CHANGED.into());
    }
    let (route, body) = reply_for(card, found, raw)?;
    let mut receipt = json!({"request_id":request["request_id"],"name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"],"question_id":request["question_id"],"question_hash":request["expected_question_hash"],"status":"pending"});
    atomic(path, &receipt.to_string())?;
    // A 4xx is a definite refusal; anything else leaves the delivery unknown
    // and the pending journal blocks a replay.
    let reply = native.raw("POST", &session_path(record, &route), Some(&body))?;
    if (400..500).contains(&reply.status) {
        receipt["status"] = json!("refused");
        atomic(path, &receipt.to_string())?;
        if reply.body["_tag"] == "FormInvalidAnswerError" {
            let reason = reply.body["message"].as_str().unwrap_or("invalid answer");
            return Err(format!("OpenCode rejected this answer: {}", clip_text(reason)));
        }
        return Err(CHANGED.into());
    }
    if !successful(&reply) {
        let reason = reply.body["message"]
            .as_str()
            .or(reply.body["data"]["message"].as_str())
            .unwrap_or("Native operation failed");
        return Err(format!("OpenCode HTTP {}: {}", reply.status, clip_text(reason)));
    }
    receipt["status"] = json!("answered");
    atomic(path, &receipt.to_string())?;
    Ok(receipt)
}
pub(crate) fn interrupt(record: &Value) -> Result<()> {
    let _guard = lock_bindings()?;
    let original = binding(string(record, "name"))?;
    if record["run_id"] != original["run_id"]
        || record["conversation_id"] != original["conversation_id"]
    {
        return Err("OpenCode turn identity changed".into());
    }
    let current = own_state(string(record, "name"))?;
    if current["interrupt_supported"] != true || current["turn_started"] != record["turn_started"] {
        return Err("OpenCode turn changed before interruption".into());
    }
    interrupt_with(&running_service()?, &original)
}
/// Core of `interrupt`: success when OpenCode says it interrupted the turn or
/// the session is no longer running. From the POST on, OpenCode may have
/// stopped the turn, so every error says so (zerus-1wchz).
fn interrupt_with(native: &dyn Native, record: &Value) -> Result<()> {
    (|| {
        let reply = native.raw(
            "POST",
            &session_path(record, "/interrupt"),
            Some(&json!({})),
        )?;
        let said = reply.body["interrupted"] == true || reply.body["data"]["interrupted"] == true;
        if successful(&reply) && said {
            return Ok(());
        }
        // A failed lookup never counts as "not running".
        if let Ok(active) = active_sessions(native) {
            if active.get(string(record, "conversation_id")).is_none() {
                return Ok(());
            }
        }
        Err(format!(
            "OpenCode did not confirm the interruption ({})",
            refusal_reason(&reply)
        ))
    })()
    .map_err(|error: String| format!("Interrupt delivery uncertain: {error}"))
}
const NOT_QUEUED: &str = "This message is no longer queued";
/// The message a `remove_queued` request may name: `msg_` and 1-124 ASCII
/// letters, digits, `_` or `-`, the shape the relay and the mobile connector
/// check too (`tests/fixtures/opencode-queued-message-ids.json`).
pub(super) fn queued_message_id(id: &str) -> bool {
    id.len() > "msg_".len() && native_id(id, "msg_")
}
/// The exact native message ID of a `remove_queued` request.
fn queued_id(request: &Value) -> Result<String> {
    let id = string(request, "message_id");
    if !queued_message_id(id) {
        return Err("Choose the exact queued OpenCode message".into());
    }
    Ok(id.to_owned())
}
/// Whether a message ID is in this session's history: `Some(true)` when
/// delivered, `Some(false)` only for OpenCode's exact MessageNotFoundError.
fn delivered(native: &dyn Native, record: &Value, id: &str) -> Result<Option<bool>> {
    let found = native.raw(
        "GET",
        &session_path(record, &format!("/message/{id}")),
        None,
    )?;
    Ok(match found.status {
        200 => Some(true),
        404 if found.body["_tag"] == "MessageNotFoundError" => Some(false),
        _ => None,
    })
}
/// Whether this session's inbox lists the message, or None when unreadable.
fn listed(native: &dyn Native, record: &Value, id: &str) -> Option<bool> {
    let inbox = native
        .raw("GET", &session_path(record, "/inbox"), None)
        .ok()?;
    if inbox.status != 200 {
        return None;
    }
    let items = inbox.body["data"].as_array()?;
    Some(items.iter().any(|item| item["id"] == id))
}
/// Core of `remove_queued`: a message can be removed only while it sits in
/// the inbox of this session, which is not running, and was never delivered.
/// A removal intent is recorded before the DELETE so no retry ever posts
/// the message again; only a removal OpenCode confirms makes its receipt a
/// definite non-delivery. `before_delete` runs once, immediately before the
/// DELETE, so a clean refusal is never recorded as a possible change.
fn remove_queued_with(
    native: &dyn Native,
    ledger: &Ledger,
    record: &Value,
    id: &str,
    before_delete: impl FnOnce(),
) -> Result<()> {
    let conversation = &record["conversation_id"];
    let inbox = get(native, &session_path(record, "/inbox"))?;
    let item = inbox["data"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == id))
        .ok_or(NOT_QUEUED)?;
    if item["sessionID"] != *conversation {
        return Err(NOT_QUEUED.into());
    }
    let active = active_sessions(native)?;
    if active.get(string(record, "conversation_id")).is_some() {
        return Err(NOT_QUEUED.into());
    }
    match delivered(native, record, id)? {
        Some(false) => {}
        Some(true) => return Err(NOT_QUEUED.into()),
        None => return Err("OpenCode could not confirm this message is still queued".into()),
    }
    if ledger
        .indexed(id)
        .is_some_and(|index| index["conversation_id"] != *conversation)
    {
        return Err(NOT_QUEUED.into());
    }
    ledger.record_removal(record, id, Removal::Intended)?;
    before_delete();
    let reply = native.raw("DELETE", &session_path(record, &format!("/inbox/{id}")), None)?;
    if reply.status == 404 {
        return Err(NOT_QUEUED.into());
    }
    if !successful(&reply) {
        return Err(format!("OpenCode could not remove this message ({})", refusal_reason(&reply)));
    }
    // OpenCode 2.0.32 answers 204 even when the message had already left the
    // inbox, so read again: the inbox first, then history, because delivery
    // moves a message from one to the other in a single transaction.
    if listed(native, record, id) != Some(false) {
        return Err(REMOVAL_UNCONFIRMED.into());
    }
    match delivered(native, record, id) {
        Ok(Some(false)) => {}
        Ok(Some(true)) => return Err(NOT_QUEUED.into()),
        _ => return Err(REMOVAL_UNCONFIRMED.into()),
    }
    ledger.record_removal(record, id, Removal::Removed)?;
    // Best effort: the removal record gives every later read the same answer.
    let _ = refuse_removed(ledger, record, id);
    Ok(())
}
/// Core of a model change: set the exact model, then read it back.
fn change_model(native: &dyn Native, record: &Value, model: &Value) -> Result<()> {
    let reply = native.raw(
        "POST",
        &session_path(record, "/model"),
        Some(&json!({"model": model})),
    )?;
    if !successful(&reply) {
        return Err(format!("OpenCode refused this model change ({})", refusal_reason(&reply)));
    }
    let seen = verify_session(native, record)?["model"].clone();
    // A model without an explicit variant reports "default".
    let variant_ok = match model["variant"].as_str() {
        Some(variant) => seen["variant"] == variant,
        None => true,
    };
    if seen["id"] != model["id"] || seen["providerID"] != model["providerID"] || !variant_ok {
        return Err(
            "Native OpenCode model change was not confirmed; inspect before retrying".into(),
        );
    }
    Ok(())
}
/// Core of rename: set only the title, then read it back.
fn change_title(native: &dyn Native, record: &Value, title: &str) -> Result<()> {
    let reply = native.raw(
        "PATCH",
        &session_path(record, ""),
        Some(&json!({"title": title})),
    )?;
    if !successful(&reply) {
        return Err(format!("OpenCode refused this rename ({})", refusal_reason(&reply)));
    }
    if verify_session(native, record)?["title"] != title {
        return Err(
            "Native OpenCode rename acknowledgement did not match; inspect before retrying".into(),
        );
    }
    Ok(())
}
const NO_PROCESS: &str = "OpenCode sessions have no separate process to pause";

fn validate_rename(old: &str, new: &str) -> Result<()> {
    let a = old.split('/').collect::<Vec<_>>();
    let b = new.split('/').collect::<Vec<_>>();
    if a.len() != 3
        || b.len() != 3
        || a[..2] != b[..2]
        || b.iter().any(|p| {
            p.is_empty()
                || p.trim() != *p
                || p.contains([':', '.'])
                || p.chars().any(char::is_control)
        })
    {
        return Err("Rename must preserve provider and project".into());
    }
    Ok(())
}

struct ActionGuard {
    // Drop bindings before read marks, in reverse acquisition order.
    _bindings: Bindings,
    rename_marks: Option<read_marks::RenameGuard>,
}
fn lock_action(
    rename: bool,
    read_lock: impl FnOnce() -> Result<read_marks::RenameGuard>,
    binding_lock: impl FnOnce() -> Result<Bindings>,
) -> Result<ActionGuard> {
    let rename_marks = if rename { Some(read_lock()?) } else { None };
    let bindings = binding_lock()?;
    Ok(ActionGuard {
        _bindings: bindings,
        rename_marks,
    })
}
fn renames_owner(action: &str, name: &str, new: Option<&str>) -> Result<bool> {
    let Some(new) = new.filter(|new| action == "rename" && *new != name) else {
        return Ok(false);
    };
    validate_rename(name, new)?;
    Ok(true)
}
fn dispatch_renames_owner(command: &str, name: &str, args: &[String]) -> Result<bool> {
    if args.iter().any(|v| v == "--dry-run" || v == "--archive") {
        return Ok(false);
    }
    renames_owner(command, name, args.get(1).filter(|v| !v.starts_with('-')).map(String::as_str))
}

struct ActionStorage<'a> {
    ledger: &'a Ledger,
    rename_marks: Option<&'a mut read_marks::RenameGuard>,
}

/// Once native title identity is confirmed, publish the new binding and marks
/// before retiring the old binding. Any failure remains uncertain; no rollback
/// or second native PATCH is inferred from a partially published local rename.
fn rename_with<N: Native>(
    record: &mut Value,
    new: &str,
    scope: Option<&session_action::Scope>,
    connect: impl FnOnce() -> Result<N>,
    save: impl FnOnce(&mut Value) -> Result<()>,
    marks: impl FnOnce(&str, &str) -> Result<()>,
    retire: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    let old = string(record, "name").to_owned();
    if let Some(scope) = scope {
        scope.changing();
    }
    change_title(&connect()?, record, new.rsplit('/').next().unwrap_or(new))?;
    notifications::anchor(record);
    record["name"] = json!(new);
    save(record)?;
    if old != new {
        marks(&old, new)?;
        retire(&old)?;
    }
    if let Some(scope) = scope {
        scope.result_record(record);
    }
    Ok(())
}
fn action(
    record: &mut Value,
    action: &str,
    new_name: Option<&str>,
    message_id: Option<&str>,
    scope: Option<&session_action::Scope>,
    rename_marks: Option<&mut read_marks::RenameGuard>,
) -> Result<()> {
    action_in(
        ActionStorage {
            ledger: &Ledger::current(),
            rename_marks,
        },
        running_service,
        record,
        action,
        new_name,
        message_id,
        scope,
    )
}
/// Core of `action`, against any native service and private state.
fn action_in<N: Native>(
    storage: ActionStorage<'_>,
    connect: impl FnOnce() -> Result<N>,
    record: &mut Value,
    action: &str,
    new_name: Option<&str>,
    message_id: Option<&str>,
    scope: Option<&session_action::Scope>,
) -> Result<()> {
    if ["pause", "resume"].contains(&action) {
        return Err(NO_PROCESS.into());
    }
    if !["rename", "forget", "remove_queued"].contains(&action) {
        return Err("This OpenCode lifecycle action is not supported".into());
    }
    if let Some(scope) = scope {
        scope.check(record)?;
    }
    let current = own_state(string(record, "name"))?;
    if !current["allowed_actions"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == action))
    {
        return Err("This action is unavailable for the exact OpenCode session".into());
    }
    let old = string(record, "name").to_owned();
    if action == "remove_queued" {
        let id = queued_id(&json!({"message_id": message_id}))?;
        remove_queued_with(&connect()?, storage.ledger, record, &id, || {
            if let Some(scope) = scope {
                scope.changing();
            }
        })?;
        if let Some(scope) = scope {
            scope.result_record(record);
        }
        return Ok(());
    }
    if action == "rename" {
        let new = new_name.ok_or("Choose a new OpenCode name")?;
        validate_rename(&old, new)?;
        if new != old && exists(new) {
            return Err("This name is already used".into());
        }
        if new != old && storage.rename_marks.is_none() {
            return Err("OpenCode rename requires the ordered shared read-mark writer".into());
        }
        PROVIDER.prepare_label(record, new)?;
        let by_id = storage::in_id_layout(record);
        return rename_with(
            record,
            new,
            scope,
            connect,
            |record| {
                if by_id { recovery::rename_job_in_place(record, string(record, "name"))?; }
                save(record)
            },
            |old, new| {
                if by_id { return Ok(()); }
                storage
                    .rename_marks
                    .ok_or("Missing shared read-mark writer")?
                    .transfer(old, new)
            },
            |old| if by_id { Ok(()) } else { archive::remove_synced(&binding_path(old)) },
        );
    } else if let Some(scope) = scope {
        scope.changing();
    }
    if action == "forget" {
        PROVIDER.remove(record)?;
        history::forget(record);
        return Ok(());
    }
    save(record)?;
    if let Some(scope) = scope {
        scope.result_record(record);
    }
    Ok(())
}
pub(super) fn session_action(
    name: &str,
    kind: &str,
    new_name: Option<&str>,
    message_id: Option<&str>,
    scope: &session_action::Scope,
) -> Result<()> {
    let mut guard = lock_action(
        renames_owner(kind, name, new_name)?,
        read_marks::lock_rename,
        lock_bindings,
    )?;
    let mut record = binding(name)?;
    action(
        &mut record,
        kind,
        new_name,
        message_id,
        Some(scope),
        guard.rename_marks.as_mut(),
    )
}
pub(crate) fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    let name = args.first().ok_or("Choose an OpenCode session")?;
    let mut guard = if command != "inspect" && command != "exists" {
        let renames = dispatch_renames_owner(command, name, args)?;
        Some(lock_action(
            renames,
            read_marks::lock_rename,
            lock_bindings,
        )?)
    } else {
        None
    };
    let mut record = binding(name)?;
    match command {
        "exists" => {}
        "inspect" => {
            if args.iter().any(|v| v == "--archive") {
                return Err("OpenCode archive inspection is not supported".into());
            }
            let agent = match args.iter().position(|v| v == "--agent") {
                Some(i) => Some(
                    args.get(i + 1)
                        .map(String::as_str)
                        .ok_or("Choose an OpenCode child session")?,
                ),
                None => None,
            };
            println!("{}", inspect(name, agent)?);
        }
        "send" => {
            let request = checked_request(&record)?;
            println!("{}", send(&record, &request)?);
        }
        "answer" => {
            let request = checked_request(&record)?;
            println!("{}", answer(&record, &request)?);
        }
        "settings" => {
            let request = checked_request(&record)?;
            if own_state(name)?["settings_change_supported"] != true {
                return Err("OpenCode must be live and idle before changing its model".into());
            }
            let service = running_service()?;
            let model = catalog::offered_model(
                &service,
                Path::new(string(&record, "cwd")),
                string(&request, "model"),
                string(&request, "effort"),
            )?;
            let path = root()
                .join("opencode/settings")
                .join(format!("{}.json", string(&request, "request_id")));
            if path.exists() {
                return Err(
                    "This model change was already attempted; inspect its native state".into(),
                );
            }
            atomic(&path, &json!({"status":"pending"}).to_string())?;
            change_model(&service, &record, &model)?;
            println!(
                "{}",
                json!({"status":"applied","scope":"session","request_id":request["request_id"],"name":name,"run_id":record["run_id"],"conversation_id":record["conversation_id"],"model":request["model"],"effort":request["effort"]})
            );
        }
        "remove_queued" => {
            let id = queued_id(&json!({"message_id": args.get(1)}))?;
            if own_state(name)?["allowed_actions"]
                .as_array()
                .is_none_or(|values| values.iter().all(|v| v != "remove_queued"))
            {
                return Err(NOT_QUEUED.into());
            }
            remove_queued_with(&running_service()?, &Ledger::current(), &record, &id, || {})?;
            println!("{}", own_state(name)?);
        }
        "pause" | "resume" => return Err(NO_PROCESS.into()),
        "rename" | "forget" => {
            if args.iter().any(|v| v == "--dry-run") {
                println!("{command} native OpenCode {name}");
                return Ok(0);
            }
            if args.iter().any(|v| v == "--archive") {
                return Err("OpenCode archives are not supported".into());
            }
            action(
                &mut record,
                command,
                args.get(1)
                    .filter(|v| !v.starts_with('-'))
                    .map(String::as_str),
                None,
                None,
                guard.as_mut().and_then(|guard| guard.rename_marks.as_mut()),
            )?;
        }
        _ => {
            return Err(format!(
                "{command} is not supported for native OpenCode sessions"
            ))
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_claim_check_never_waits_past_its_bound_or_counts_silence_as_live() {
        // zerus-p24ce: a service that accepts and never answers is busy.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                held.push(stream);
            }
        });
        let service = |port| service::Service {
            reg: service::Registration {
                id: String::new(),
                version: String::new(),
                port,
                pid: 0,
                password: None,
            },
        };
        let record =
            json!({"service":"opencode2","conversation_id":"ses_claim","cwd":"/tmp/example"});
        let started = Instant::now();
        let busy = claim_live_on(
            &service(port),
            &record,
            started + Duration::from_millis(300),
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(busy, Err(parent_link::CLAIM_BUSY.to_owned()));
        // A service that closes the connection unanswered: a definite no,
        // before the bound, on every platform.
        let quitting = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let quit = quitting.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in quitting.incoming() {
                drop(stream);
            }
        });
        let started = Instant::now();
        assert_eq!(
            claim_live_on(&service(quit), &record, started + Duration::from_secs(2)),
            Ok(false)
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        // Nothing listening. Linux and macOS refuse a loopback connection at
        // once: a definite no. Windows retries a refused SYN for about two
        // seconds, so its refusal can arrive only at the bound and read as
        // busy; it is never live and never outlasts the bound.
        let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let free = closed.local_addr().unwrap().port();
        drop(closed);
        let started = Instant::now();
        let refused = claim_live_on(&service(free), &record, started + Duration::from_secs(2));
        #[cfg(not(windows))]
        {
            assert_eq!(refused, Ok(false));
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{:?}",
                started.elapsed()
            );
        }
        #[cfg(windows)]
        {
            assert!(
                refused == Ok(false) || refused == Err(parent_link::CLAIM_BUSY.to_owned()),
                "{refused:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );
        }
    }
    use super::*;
    use std::collections::HashMap;

    struct RenameNative {
        patches: std::cell::Cell<usize>,
        reads: std::cell::Cell<usize>,
        refused: bool,
        mismatched: bool,
        title: String,
    }
    impl Native for &RenameNative {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            assert_eq!(path, "/api/session/ses_A");
            match method {
                "PATCH" => {
                    self.patches.set(self.patches.get() + 1);
                    assert_eq!(body.unwrap(), &json!({"title":self.title}));
                    Ok(Response {
                        status: if self.refused { 500 } else { 204 },
                        body: json!(null),
                    })
                }
                "GET" => {
                    self.reads.set(self.reads.get() + 1);
                    Ok(Response {
                        status: 200,
                        body: json!({"data":{"id":"ses_A","location":{"directory":"/x"},
                        "title":if self.mismatched { "another" } else { &self.title }}}),
                    })
                }
                _ => panic!("Unexpected native request"),
            }
        }
    }
    fn rename_native(title: &str) -> RenameNative {
        RenameNative {
            patches: Default::default(),
            reads: Default::default(),
            refused: false,
            mismatched: false,
            title: title.into(),
        }
    }
    fn rename_marks(old: &str) -> Value {
        json!({"version":1,"sessions":{old:{"ses_A":{"conversation_id":"ses_A","reply_id":"msg_reply",
            "attention_signature":null,"at":20.0},"ses_previous":{"conversation_id":"ses_previous","reply_id":"msg_before","at":10.0}},
            "claude/other/session":{"conversation":{"reply_id":"untouched","at":30.0}}}})
    }
    fn marks_at(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    #[test]
    fn renamed_opencode_owner_keeps_the_same_v1_read_marks() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = ("opencode/p/old", "opencode/p/new");
        let marks = dir.path().join("read-marks.json");
        let before = rename_marks(old);
        atomic(&marks, &before.to_string()).unwrap();
        let mut guard = read_marks::RenameGuard::at(&marks).unwrap();
        let old_binding = dir.path().join("old.json");
        let new_binding = dir.path().join("new.json");
        let mut record = json!({"name":old,"run_id":"r1","conversation_id":"ses_A","cwd":"/x"});
        atomic(&old_binding, &record.to_string()).unwrap();
        let scope = session_action::Scope::expecting(old, "r1", "ses_A");
        let native = rename_native("new");
        rename_with(
            &mut record,
            new,
            Some(&scope),
            || Ok(&native),
            |record| {
                // Publication does not let another reader capture the new name
                // before this writer has transferred the old owner's marks.
                atomic(&new_binding, &record.to_string())?;
                assert!(lock_until(&marks.with_extension("lock"), Instant::now())?.is_none());
                Ok(())
            },
            |old, new| guard.transfer(old, new),
            |_| archive::remove_synced(&old_binding),
        )
        .unwrap();
        let after = marks_at(&marks);
        assert_eq!(after["version"], 1);
        assert!(after["sessions"].get(old).is_none());
        assert_eq!(after["sessions"][new], before["sessions"][old]);
        assert_eq!(
            after["sessions"]["claude/other/session"],
            before["sessions"]["claude/other/session"]
        );
        assert_eq!(
            read_marks::record(&after["sessions"], new, "ses_A")["reply_id"],
            "msg_reply"
        );
        assert!(!old_binding.exists());
        let saved = marks_at(&new_binding);
        assert_eq!(
            (
                saved["name"].clone(),
                saved["run_id"].clone(),
                saved["conversation_id"].clone()
            ),
            (json!(new), json!("r1"), json!("ses_A"))
        );
        assert_eq!((native.patches.get(), native.reads.get()), (1, 1));
        assert!(scope.was_changing() && scope.has_result());
    }
    #[test]
    fn opencode_rename_failure_keeps_a_readable_binding_and_never_claims_a_result() {
        for failure in ["native", "identity", "save", "marks", "retire"] {
            let dir = tempfile::tempdir().unwrap();
            let (old, new) = ("opencode/p/old", "opencode/p/new");
            let marks = dir.path().join("read-marks.json");
            let before = rename_marks(old);
            atomic(&marks, &before.to_string()).unwrap();
            let mut guard = read_marks::RenameGuard::at(&marks).unwrap();
            let (old_binding, new_binding) =
                (dir.path().join("old.json"), dir.path().join("new.json"));
            let mut record = json!({"name":old,"run_id":"r1","conversation_id":"ses_A","cwd":"/x"});
            atomic(&old_binding, &record.to_string()).unwrap();
            let scope = session_action::Scope::expecting(old, "r1", "ses_A");
            let mut native = rename_native("new");
            native.refused = failure == "native";
            native.mismatched = failure == "identity";
            let result = rename_with(
                &mut record,
                new,
                Some(&scope),
                || Ok(&native),
                |record| {
                    if failure == "save" {
                        Err("controlled binding write failure".into())
                    } else {
                        atomic(&new_binding, &record.to_string())
                    }
                },
                |old, new| {
                    if failure == "marks" {
                        Err("controlled read-mark write failure".into())
                    } else {
                        guard.transfer(old, new)
                    }
                },
                |_| {
                    if failure == "retire" {
                        Err("controlled old-binding removal failure".into())
                    } else {
                        archive::remove_synced(&old_binding)
                    }
                },
            );
            assert!(result.is_err(), "{failure}");
            assert!(scope.was_changing() && !scope.has_result(), "{failure}");
            assert_eq!(native.patches.get(), 1, "{failure}");
            assert!(old_binding.exists(), "{failure}");
            if failure == "marks" || failure == "retire" {
                assert!(new_binding.exists(), "{failure}");
            } else {
                assert!(!new_binding.exists(), "{failure}");
            }
            if failure == "retire" {
                assert_eq!(marks_at(&marks)["sessions"][new], before["sessions"][old]);
            } else {
                assert_eq!(marks_at(&marks), before, "{failure}");
            }
        }
    }
    #[test]
    fn opencode_rename_preflight_refuses_bad_marks_before_bindings_or_native_work() {
        for contents in [
            "not JSON",
            "{\"version\":2,\"sessions\":{}}",
            "{\"version\":1,\"sessions\":null}",
            "directory",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("read-marks.json");
            if contents == "directory" {
                fs::create_dir(&path).unwrap();
            } else {
                fs::write(&path, contents).unwrap();
            }
            let binding_called = std::cell::Cell::new(false);
            let native_called = std::cell::Cell::new(false);
            let result = lock_action(
                true,
                || read_marks::RenameGuard::at(&path),
                || {
                    binding_called.set(true);
                    lock_bindings_at(&dir.path().join("bindings.lock"))
                },
            )
            .map(|_| {
                native_called.set(true);
            });
            assert_eq!(
                result.unwrap_err(),
                "Shared read marks could not be read; rename was not started"
            );
            assert!(!binding_called.get() && !native_called.get());
            if contents != "directory" {
                assert_eq!(fs::read_to_string(&path).unwrap(), contents);
            }
        }
    }
    #[test]
    fn same_name_dry_run_and_nonrename_need_no_read_mark_writer() {
        let dir = tempfile::tempdir().unwrap();
        for flag in ["--dry-run", "--archive"] {
            let args = ["opencode/p/old", "opencode/p/new", flag].map(str::to_owned);
            assert!(!dispatch_renames_owner("rename", &args[0], &args).unwrap());
        }
        for (kind, name, new) in [
            ("rename", "opencode/p/same", Some("opencode/p/same")),
            ("forget", "opencode/p/same", None),
            ("rename", "opencode/p/old", None),
        ] {
            assert!(!renames_owner(kind, name, new).unwrap());
        }
        assert!(renames_owner("rename", "opencode/p/old", Some("opencode/p/new")).unwrap());
        assert!(renames_owner("rename", "opencode/p/old", Some("opencode/other/new")).is_err());
        let _guard = lock_action(
            false,
            || panic!("Read-mark writer must not be opened"),
            || lock_bindings_at(&dir.path().join("bindings.lock")),
        )
        .unwrap();
        assert!(!dir.path().join("read-marks.lock").exists());
        let native = rename_native("same");
        let mut record =
            json!({"name":"opencode/p/same","run_id":"r1","conversation_id":"ses_A","cwd":"/x"});
        rename_with(
            &mut record,
            "opencode/p/same",
            None,
            || Ok(&native),
            |_| Ok(()),
            |_, _| panic!("Same name must not transfer marks"),
            |_| panic!("Same name must not retire binding"),
        )
        .unwrap();
        assert_eq!(native.patches.get(), 1); // Preserve the existing title-confirm/save behavior.
    }
    #[test]
    fn missing_owner_transfer_preserves_every_other_v1_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-marks.json");
        let bytes = b"{\"version\":1, \"sessions\":{\"other\":{\"conversation\":{\"reply_id\":\"same\"}}}}\n";
        atomic(&path, std::str::from_utf8(bytes).unwrap()).unwrap();
        let mut guard = read_marks::RenameGuard::at(&path).unwrap();
        guard.transfer("missing", "new").unwrap();
        guard.transfer("other", "other").unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn opencode_rename_waits_for_read_marks_before_acquiring_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let marks = dir.path().join("read-marks.json");
        let binding = dir.path().join("bindings.lock");
        let held = read_marks::RenameGuard::at(&marks).unwrap();
        let (waiting, ready) = std::sync::mpsc::channel();
        let (thread_marks, thread_binding) = (marks.clone(), binding.clone());
        let writer = std::thread::spawn(move || {
            lock_action(
                true,
                || {
                    waiting.send(()).unwrap();
                    read_marks::RenameGuard::at(&thread_marks)
                },
                || lock_bindings_at(&thread_binding),
            )
            .map(drop)
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        // The pending renamer must not own bindings while an earlier read writer
        // still owns marks and needs to inspect the OpenCode binding.
        let earlier_read = try_bindings_at(&binding, Duration::ZERO);
        assert!(earlier_read.is_some(), "renamer acquired bindings before the earlier read writer");
        let earlier_read = earlier_read.unwrap();
        drop(earlier_read);
        drop(held);
        writer.join().unwrap().unwrap();
    }
    #[test]
    fn new_ids_match_opencode_and_stored_ids_keep_the_existing_check() {
        for prefix in ["ses_", "msg_"] {
            let id = new_id(prefix);
            assert_eq!(id.len(), 30);
            assert!(id[4..].bytes().all(|b| b.is_ascii_alphanumeric()));
            assert!(native_id(&id, prefix));
        }
        assert_ne!(new_id("ses_"), new_id("ses_"));
        // OpenCode 1 bindings used ses_ plus 32 hex characters; they stay valid.
        assert!(native_id("ses_0123456789abcdef0123456789abcdef", "ses_"));
        assert!(native_id("ses_eb44cc196ffeOcEUMCZm4t66t7", "ses_"));
        assert!(!native_id("ses_bad id", "ses_"));
    }
    #[test]
    fn legacy_bindings_are_adopted_only_for_their_own_migrated_session() {
        let record = json!({"conversation_id": "ses_0123456789abcdef0123456789abcdef", "cwd": "/workspace/fixture", "native_data_home": "/x"});
        let session = json!({"data": {"id": "ses_0123456789abcdef0123456789abcdef", "location": {"directory": "/workspace/fixture"}}});
        assert!(matches!(
            legacy_verdict(&record, &json!({"status": "completed"}), Some(&session)),
            Legacy::Adopted
        ));
        assert!(matches!(
            legacy_verdict(&record, &json!({"status": "running"}), Some(&session)),
            Legacy::Unsupported
        ));
        assert!(matches!(
            legacy_verdict(&record, &json!({"status": "completed"}), None),
            Legacy::Unsupported
        ));
        let moved = json!({"data": {"id": "ses_0123456789abcdef0123456789abcdef", "location": {"directory": "/elsewhere"}}});
        assert!(matches!(
            legacy_verdict(&record, &json!({"status": "completed"}), Some(&moved)),
            Legacy::Unsupported
        ));
        let current = json!({"conversation_id": "ses_x", "cwd": "/w", "service": "opencode2"});
        assert!(matches!(
            legacy_verdict(&current, &json!({}), None),
            Legacy::Current
        ));
    }
    #[test]
    fn legacy_verdict_needs_exact_nonempty_identity() {
        let done = json!({"status": "completed"});
        let record = json!({"conversation_id": "ses_a", "cwd": "/workspace/fixture"});
        let good =
            json!({"data": {"id": "ses_a", "location": {"directory": "/workspace/fixture"}}});
        assert!(matches!(
            legacy_verdict(&record, &done, Some(&good)),
            Legacy::Adopted
        ));
        let other =
            json!({"data": {"id": "ses_b", "location": {"directory": "/workspace/fixture"}}});
        assert!(matches!(
            legacy_verdict(&record, &done, Some(&other)),
            Legacy::Unsupported
        ));
        assert!(matches!(
            legacy_verdict(&record, &done, Some(&json!({}))),
            Legacy::Unsupported
        ));
        for bad in [
            json!({"cwd": "/w"}),
            json!({"conversation_id": "ses_a"}),
            json!({"conversation_id": "", "cwd": ""}),
        ] {
            let blank = json!({"data": {"id": bad["conversation_id"], "location": {"directory": bad["cwd"]}}});
            assert!(matches!(
                legacy_verdict(&bad, &done, Some(&blank)),
                Legacy::Unsupported
            ));
        }
    }
    fn reply(status: u16, body: Value) -> Reply {
        Ok(Response { status, body })
    }
    #[test]
    fn only_definite_answers_make_a_binding_unsupported() {
        let record = json!({"conversation_id": "ses_a", "cwd": "/w"});
        let session = || {
            reply(
                200,
                json!({"data": {"id": "ses_a", "location": {"directory": "/w"}}}),
            )
        };
        let verdict = |m: Reply, s: Reply| legacy_decision(&record, &m, || s);
        let done = || reply(200, json!({"data": {"status": "completed"}}));
        assert!(matches!(
            verdict(done(), session()),
            Ok((Legacy::Adopted, true))
        ));
        assert!(matches!(
            verdict(done(), reply(404, json!({}))),
            Ok((Legacy::Unsupported, true))
        ));
        assert!(matches!(
            verdict(
                done(),
                reply(
                    200,
                    json!({"data": {"id": "ses_z", "location": {"directory": "/w"}}})
                )
            ),
            Ok((Legacy::Unsupported, true))
        ));
        // Not completed: unsupported but not settled, and no session request.
        let pending = legacy_decision(&record, &reply(200, json!({"status": "running"})), || {
            panic!("no session GET")
        });
        assert!(matches!(pending, Ok((Legacy::Unsupported, false))));
        // Errors never become a verdict.
        assert!(verdict(Err("refused".into()), session()).is_err());
        assert!(verdict(reply(500, json!({})), session()).is_err());
        assert!(verdict(done(), Err("timeout".into())).is_err());
        assert!(verdict(done(), reply(503, json!({}))).is_err());
        let current = json!({"service": "opencode2"});
        assert!(matches!(
            legacy_decision(&current, &Err("x".into()), || panic!("none")),
            Ok((Legacy::Current, false))
        ));
    }
    fn ack(value: &Value) -> bool {
        value["data"]["id"] == "ses_a"
    }
    fn counted(
        get: Reply,
        post: Reply,
        first: bool,
    ) -> (std::result::Result<(), Refusal>, usize, usize) {
        let (mut gets, mut posts) = (0, 0);
        let mut g = || {
            gets += 1;
            get.clone()
        };
        let mut p = || {
            posts += 1;
            post.clone()
        };
        let result = if first {
            create_session(&mut g, &mut p, &ack)
        } else {
            reconcile_creation(&mut g, &mut p, &ack)
        };
        (result, gets, posts)
    }
    #[test]
    fn lost_creation_replies_are_reconciled_without_a_second_identity() {
        let found = reply(200, json!({"data": {"id": "ses_a"}}));
        let (result, gets, posts) = counted(found.clone(), Err("lost".into()), false);
        assert!(result.is_ok() && gets == 1 && posts == 0);
        let (result, gets, posts) = counted(reply(404, json!({})), found.clone(), false);
        assert!(result.is_ok() && gets == 1 && posts == 1);
        let wrong = reply(200, json!({"data": {"id": "ses_other"}}));
        let (result, _, posts) = counted(wrong, found.clone(), false);
        assert!(
            matches!(&result, Err(Refusal::Other(why)) if why == CREATION_MISMATCH
                && !why.contains("resum"))
                && posts == 0
        );
        let (result, _, posts) = counted(reply(404, json!({})), wrong_ack(), false);
        assert!(matches!(result, Err(Refusal::Other(_))) && posts == 1);
        // First attempt: a definite refusal ends it; a lost reply reconciles.
        let (result, gets, posts) =
            counted(found.clone(), reply(400, json!({"message": "bad"})), true);
        assert!(matches!(result, Err(Refusal::Definite(_))) && gets == 0 && posts == 1);
        let (result, gets, posts) = counted(found.clone(), Err("lost".into()), true);
        assert!(result.is_ok() && gets == 1 && posts == 1);
    }
    fn wrong_ack() -> Reply {
        reply(200, json!({"data": {"id": "ses_other"}}))
    }
    #[test]
    fn session_paths_use_the_conversation_id() {
        let record = json!({"conversation_id": "ses_abc"});
        assert_eq!(session_path(&record, ""), "/api/session/ses_abc");
        assert_eq!(
            session_path(&record, "/prompt"),
            "/api/session/ses_abc/prompt"
        );
    }
    struct HintNative<'a> {
        calls: std::cell::RefCell<Vec<(String, String, Value)>>,
        saves: &'a std::cell::Cell<usize>,
        reply: Option<Result<Response>>,
    }
    impl Native for HintNative<'_> {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            assert_eq!(
                self.saves.get(),
                1,
                "the suppression mark was not saved first"
            );
            self.calls
                .borrow_mut()
                .push((method.into(), path.into(), body.unwrap().clone()));
            self.reply.clone().unwrap_or(Ok(Response {
                status: 204,
                body: Value::Null,
            }))
        }
    }
    #[test]
    fn instruction_hint_is_claimed_before_one_fail_soft_attachment() {
        let record = || json!({"conversation_id":"ses_A"});
        let saves = std::cell::Cell::new(0);
        let native = HintNative {
            calls: Default::default(),
            saves: &saves,
            reply: None,
        };
        let mut run = record();
        let persist = |record: &mut Value| {
            saves.set(saves.get() + 1);
            assert_eq!(
                record["agent_hint"],
                json!({"conversation_id":"ses_A","revision":1})
            );
            Ok(())
        };
        attach_hint_with(&native, &mut run, true, persist);
        assert_eq!(saves.get(), 1);
        assert_eq!(
            native.calls.borrow().as_slice(),
            &[(
                "PUT".into(),
                "/api/experimental/session/ses_A/instructions/entries/zerus.agent-hint".into(),
                json!({"value":agent_hint::HINT})
            )]
        );
        for enabled in [true, false] {
            attach_hint_with(&native, &mut run, enabled, |_| {
                panic!("a claimed hint saves again")
            });
        }
        assert_eq!(native.calls.borrow().len(), 1);
        assert_eq!(saves.get(), 1);
        run["agent_hint"]["conversation_id"] = json!("ses_other");
        run["conversation_id"] = json!("");
        attach_hint_with(&native, &mut run, true, |_| panic!("an empty ID writes"));
        assert_eq!(native.calls.borrow().len(), 1);
    }
    #[test]
    fn instruction_hint_opt_out_and_uncertain_failures_never_replay() {
        let saves = std::cell::Cell::new(0);
        let native = HintNative {
            calls: Default::default(),
            saves: &saves,
            reply: Some(Err("connection lost".into())),
        };
        let mut opted_out = json!({"conversation_id":"ses_A"});
        attach_hint_with(&native, &mut opted_out, false, |_| {
            panic!("an opted-out hint saves")
        });
        assert!(opted_out.get("agent_hint").is_none());
        assert!(native.calls.borrow().is_empty());
        let mut unsaved = opted_out.clone();
        attach_hint_with(&native, &mut unsaved, true, |_| {
            Err("private state unavailable".into())
        });
        assert!(native.calls.borrow().is_empty());
        for reply in [Some(Err("connection lost".into())), None] {
            let saves = std::cell::Cell::new(0);
            let native = HintNative {
                calls: Default::default(),
                saves: &saves,
                reply,
            };
            let mut run = json!({"conversation_id":"ses_A"});
            for _ in 0..2 {
                attach_hint_with(&native, &mut run, true, |_| {
                    saves.set(saves.get() + 1);
                    Ok(())
                });
            }
            assert_eq!(saves.get(), 1);
            assert_eq!(native.calls.borrow().len(), 1);
        }
        let saves = std::cell::Cell::new(0);
        let refused = HintNative {
            calls: Default::default(),
            saves: &saves,
            reply: Some(Ok(Response {
                status: 503,
                body: json!({}),
            })),
        };
        let mut run = json!({"conversation_id":"ses_A"});
        attach_hint_with(&refused, &mut run, true, |_| {
            saves.set(saves.get() + 1);
            Ok(())
        });
        attach_hint_with(&refused, &mut run, true, |_| {
            panic!("a refused hint saves again")
        });
        assert_eq!(refused.calls.borrow().len(), 1);
    }
    struct Scripted(std::cell::RefCell<Vec<(String, u16, Value)>>);
    impl Native for Scripted {
        fn raw(&self, method: &str, path: &str, _: Option<&Value>) -> Result<Response> {
            let mut script = self.0.borrow_mut();
            let (expected, status, body) = script.remove(0);
            assert_eq!(expected, format!("{method} {path}"));
            Ok(Response { status, body })
        }
    }
    #[test]
    fn hint_emptiness_uses_one_bounded_history_read_and_rejects_bad_answers() {
        let record = json!({"conversation_id":"ses_A"});
        for (status, body, expected) in [
            (200, json!({"data":[]}), Some(true)),
            (200, json!({"data":[{"type":"user"}]}), Some(false)),
            (200, json!({"data":null}), None),
            (503, json!({}), None),
        ] {
            let native = Scripted(std::cell::RefCell::new(vec![(
                "GET /api/session/ses_A/message?limit=1".into(),
                status,
                body,
            )]));
            assert_eq!(empty_history(&native, &record).ok(), expected);
            assert!(native.0.borrow().is_empty());
        }
    }
    #[test]
    fn notification_baseline_uses_only_gets_and_retains_current_identity() {
        let record = json!({"name":"opencode/test/session","service":"opencode2","backend":"opencode","conversation_id":"ses_A","run_id":"run-A","cwd":"/workspace/fixture"});
        let native = Scripted(std::cell::RefCell::new(vec![
            ("GET /api/session/active".into(), 200, json!({"data":{}})),
            ("GET /api/session/ses_A".into(), 200, json!({"data":{"id":"ses_A","location":{"directory":"/workspace/fixture"}}})),
            ("GET /api/session/ses_A/form".into(), 200, json!({"data":[]})),
            ("GET /api/session/ses_A/permission".into(), 200, json!({"data":[]})),
            ("GET /api/session/ses_A/inbox".into(), 200, json!({"data":[]})),
        ]));
        let (view, questions) = notification_snapshot_with(&native, &record, || Ok(history::Page { messages: Vec::new(), truncated: false })).unwrap();
        assert!(native.0.borrow().is_empty());
        assert_eq!(view["conversation_id"], "ses_A");
        assert_eq!(view["run_id"], "run-A");
        assert_eq!(view["phase"], "idle");
        assert_eq!(questions, Some(Vec::new()));
        assert_eq!(record["service"], "opencode2");
    }
    fn fixture(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }
    #[test]
    fn server_refusals_are_bounded_and_sanitized_like_get() {
        // OpenCode's own message, longer and dirtier than a row should carry.
        let long = format!("\x1b[31mbad\x1b[0m\u{7}\r{}\ttail\n", "x".repeat(400));
        for body in [
            json!({ "message": long }),
            json!({"data": { "message": long }}),
        ] {
            let response = Response {
                status: 500,
                body: body.clone(),
            };
            let text = refusal_text(&response);
            let native = Scripted(std::cell::RefCell::new(vec![(
                "GET /api/x".into(),
                500,
                body,
            )]));
            assert_eq!(get(&native, "/api/x").unwrap_err(), text);
            let reason = text.strip_prefix("OpenCode HTTP 500: ").unwrap();
            assert_eq!(reason.chars().count(), 301, "{reason}");
            assert!(reason.starts_with("badxxx") && reason.ends_with('…'));
            assert!(!reason.chars().any(char::is_control));
            // The same text wherever a reply becomes a session row or a refusal.
            assert_eq!(
                migration_completed(&Ok(response.clone())).unwrap_err(),
                text
            );
            assert!(matches!(
                creation_reply(Ok(response.clone()), &ack),
                Err(Refusal::Other(other)) if other == text
            ));
            let refused = Response {
                status: 400,
                ..response
            };
            assert!(matches!(
                creation_reply(Ok(refused.clone()), &ack),
                Err(Refusal::Definite(definite)) if definite == refusal_text(&refused)
                    && definite.chars().count() == "OpenCode HTTP 400: ".len() + 301
            ));
        }
        // Tabs and line breaks stay, as get() keeps them; other controls go.
        let short = Response {
            status: 409,
            body: json!({"message": "a\tb\nc\u{0}\x1b[2Kd"}),
        };
        assert_eq!(refusal_text(&short), "OpenCode HTTP 409: a\tb\ncd");
        let silent = Response {
            status: 502,
            body: json!("Bad gateway"),
        };
        assert_eq!(
            refusal_text(&silent),
            "OpenCode HTTP 502: Native operation refused"
        );
        // DEL and C1 controls go too, and do not count toward the bound.
        let c1 = Response {
            status: 409,
            body: json!({"message": "a\u{7f}b\u{85}c\u{9b}31md\te\nf"}),
        };
        assert_eq!(refusal_text(&c1), "OpenCode HTTP 409: abc31md\te\nf");
        let padded = Response {
            status: 500,
            body: json!({ "message": format!("{}{}", "\u{85}\u{7f}\u{9b}".repeat(100), "x".repeat(400)) }),
        };
        let text = refusal_text(&padded);
        assert_eq!(text, format!("OpenCode HTTP 500: {}…", "x".repeat(300)));
        let native = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/x".into(),
            500,
            padded.body.clone(),
        )]));
        assert_eq!(get(&native, "/api/x").unwrap_err(), text);
    }
    #[test]
    fn a_queued_message_is_found_in_the_inbox_after_a_lost_response() {
        let record = json!({"conversation_id": "ses_A"});
        let inbox = fixture(include_bytes!("../../tests/fixtures/opencode2/inbox_busy.json"));
        let queued = inbox["data"][0]["id"].as_str().unwrap().to_owned();
        let native = Scripted(std::cell::RefCell::new(vec![
            (
                format!("GET /api/session/ses_A/message/{queued}"),
                404,
                fixture(include_bytes!("../../tests/fixtures/opencode2/message_not_found.json")),
            ),
            ("GET /api/session/ses_A/inbox".into(), 200, inbox.clone()),
        ]));
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            reconcile(
                &native,
                &Ledger(dir.path().into()),
                &record,
                &queued,
                "queued later"
            )
            .unwrap(),
            Delivery::Queued
        ));
    }
    #[test]
    fn an_unknown_message_is_uncertain_and_never_reposted() {
        let prompt = fixture(include_bytes!("../../tests/fixtures/opencode2/prompt.json"));
        let session = prompt["data"]["sessionID"].as_str().unwrap().to_owned();
        let record = json!({"conversation_id": session});
        let id = prompt["data"]["id"].as_str().unwrap().to_owned();
        let native = Scripted(std::cell::RefCell::new(vec![
            (
                format!("GET /api/session/{session}/message/{id}"),
                404,
                json!({"_tag": "MessageNotFoundError"}),
            ),
            (
                format!("GET /api/session/{session}/inbox"),
                200,
                json!({"data": []}),
            ),
        ]));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            reconcile(&native, &Ledger(dir.path().into()), &record, &id, "hello").unwrap_err(),
            UNCERTAIN_STATE
        );
        assert!(native.0.borrow().is_empty());
    }
    #[test]
    fn conflicting_text_for_the_same_id_is_uncertain() {
        let record = json!({"conversation_id": "ses_A"});
        let native = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/session/ses_A/message/msg_X".into(),
            200,
            json!({"data": {"id": "msg_X", "type": "user", "text": "other"}}),
        )]));
        let dir = tempfile::tempdir().unwrap();
        assert!(reconcile(
            &native,
            &Ledger(dir.path().into()),
            &record,
            "msg_X",
            "hello"
        )
        .is_err());
    }
    #[test]
    fn pending_forms_or_permissions_block_sending_but_a_busy_session_does_not() {
        let record = json!({"conversation_id": "ses_A"});
        let empty = || json!({"data": []});
        let native = Scripted(std::cell::RefCell::new(vec![
            ("GET /api/session/ses_A/form".into(), 200, empty()),
            ("GET /api/session/ses_A/permission".into(), 200, empty()),
        ]));
        assert!(!waiting_for_user(&native, &record).unwrap());
        let permissions = fixture(include_bytes!("../../tests/fixtures/opencode2/permissions.json"));
        let native = Scripted(std::cell::RefCell::new(vec![
            ("GET /api/session/ses_A/form".into(), 200, empty()),
            ("GET /api/session/ses_A/permission".into(), 200, permissions),
        ]));
        assert!(waiting_for_user(&native, &record).unwrap());
        let busy = fixture(include_bytes!("../../tests/fixtures/opencode2/active_busy.json"));
        let record = json!({"conversation_id": "ses_IycejmWY8RpJq1aeBnnIpbaNAt"});
        let native = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/session/active".into(),
            200,
            busy,
        )]));
        assert!(session_active(&native, &record));
    }
    #[test]
    fn only_opencode2_bindings_resume_a_pending_creation() {
        assert!(resumes_pending(&json!({"service":"opencode2","creation_pending":true})));
        assert!(!resumes_pending(&json!({"creation_pending":true})));
        assert!(!resumes_pending(&json!({"service":"opencode1","creation_pending":true})));
        assert!(!resumes_pending(&json!({"service":"opencode2"})));
    }
    type Step = (String, Result<Response>);
    struct Live {
        script: std::cell::RefCell<Vec<Step>>,
        posted: std::cell::RefCell<Vec<Value>>,
        dir: PathBuf,
        path: PathBuf,
    }
    impl Native for Live {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            let (expected, reply) = self.script.borrow_mut().remove(0);
            assert_eq!(expected, format!("{method} {path}"));
            if method == "POST" {
                let receipt: Value =
                    serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap();
                assert_eq!(receipt["status"], "pending");
                assert_eq!(receipt["native_message_id"], body.unwrap()["id"]);
                self.posted.borrow_mut().push(body.unwrap().clone());
            }
            reply
        }
    }
    fn ok(body: Value) -> Result<Response> {
        Ok(Response { status: 200, body })
    }
    fn status(status: u16, body: Value) -> Result<Response> {
        Ok(Response { status, body })
    }
    fn live(dir: &Path, steps: Vec<(&str, Result<Response>)>) -> Live {
        Live {
            script: std::cell::RefCell::new(
                steps.into_iter().map(|(p, r)| (p.to_owned(), r)).collect(),
            ),
            posted: Default::default(),
            dir: dir.into(),
            path: dir
                .join("receipts")
                .join(format!("{}.json", string(&request(), "request_id"))),
        }
    }
    fn session_record() -> Value {
        json!({"name":"w","run_id":"r1","conversation_id":"ses_A","cwd":"/x"})
    }
    fn request() -> Value {
        json!({"request_id":"11111111-1111-4111-8111-111111111111","text":"hello"})
    }
    fn preflight() -> Vec<(&'static str, Result<Response>)> {
        vec![
            ("GET /api/session/ses_A", ok(json!({"data":{"id":"ses_A","location":{"directory":"/x"}}}))),
            ("GET /api/session/ses_A/form", ok(json!({"data":[]}))),
            ("GET /api/session/ses_A/permission", ok(json!({"data":[]}))),
            ("GET /api/session/active", ok(json!({"data":{}}))),
        ]
    }
    fn run(native: &Live, digest: &str) -> Result<Value> {
        deliver(
            native,
            &Ledger(native.dir.clone()),
            &session_record(),
            &request(),
            digest.into(),
            None,
        )
    }
    #[test]
    fn a_message_is_journaled_before_the_post_and_marked_queued_when_busy() {
        let dir = tempfile::tempdir().unwrap();
        let mut steps = preflight();
        steps[3] = ("GET /api/session/active", ok(json!({"data":{"ses_A":{"type":"running"}}})));
        steps.push(("POST /api/session/ses_A/prompt", ok(json!(null))));
        let native = live(dir.path(), steps);
        struct Echo<'a>(&'a Live);
        impl Native for Echo<'_> {
            fn raw(&self, m: &str, p: &str, b: Option<&Value>) -> Result<Response> {
                if m == "POST" {
                    let _ = self.0.raw(m, p, b)?;
                    let id = b.unwrap()["id"].clone();
                    return ok(json!({"data":{"id":id,"sessionID":"ses_A","payload":{"text":"hello"}}}));
                }
                self.0.raw(m, p, b)
            }
        }
        let receipt = deliver(
            &Echo(&native),
            &Ledger(native.dir.clone()),
            &session_record(),
            &request(),
            "d".into(),
            None,
        )
        .unwrap();
        assert_eq!(receipt["status"], "submitted");
        assert_eq!(receipt["queued"], true);
        assert!(native.script.borrow().is_empty());
        let saved: Value = serde_json::from_slice(&fs::read(&native.path).unwrap()).unwrap();
        assert_eq!(saved["status"], "submitted");
    }
    #[test]
    fn a_refused_post_is_recorded_and_never_reposted() {
        let dir = tempfile::tempdir().unwrap();
        let mut steps = preflight();
        steps.push(("POST /api/session/ses_A/prompt", status(400, json!({"message":"bad"}))));
        let native = live(dir.path(), steps);
        let error = run(&native, "d").unwrap_err();
        assert_eq!(error, "OpenCode refused this message: HTTP 400: bad");
        let saved: Value = serde_json::from_slice(&fs::read(&native.path).unwrap()).unwrap();
        assert_eq!(saved["status"], "refused");
        let retry = live(dir.path(), vec![]);
        assert_eq!(run(&retry, "d").unwrap_err(), "OpenCode refused this message: HTTP 400");
        assert!(retry.posted.borrow().is_empty());
        let other = live(dir.path(), vec![]);
        assert!(run(&other, "e").unwrap_err().contains("already used"));
    }
    #[test]
    fn a_local_refusal_retains_detail_without_copying_it_into_delivery_audit() {
        let secret = "https://provider.example.invalid/v1?key=fixture-YO85-SEND\u{1b}[31m";
        let record = session_record();
        let native = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/x".into(), 403, json!({"message":secret}),
        )]));
        let detail = get(&native, "/api/x").unwrap_err();
        let local = unavailable(&record, detail);
        assert!(string(&local, "inspection_reason").contains("fixture-YO85-SEND"));
        assert_eq!(local["inspection_reason"], local["activity_detail"]);

        let dir = tempfile::tempdir().unwrap();
        let mut steps = preflight();
        steps.push(("POST /api/session/ses_A/prompt", status(403, json!({"message":secret}))));
        let native = live(dir.path(), steps);
        assert!(run(&native, "d").unwrap_err().contains("fixture-YO85-SEND"));
        let stored = fs::read_to_string(&native.path).unwrap();
        assert!(!stored.contains("provider.example.invalid"));
        assert!(!stored.contains("fixture-YO85-SEND"));
        assert_eq!(serde_json::from_str::<Value>(&stored).unwrap()["reason"], "HTTP 403");
        let retry = live(dir.path(), vec![]);
        assert_eq!(run(&retry, "d").unwrap_err(), "OpenCode refused this message: HTTP 403");
        assert!(retry.posted.borrow().is_empty());
    }
    #[test]
    fn a_mismatched_acknowledgement_is_uncertain_without_a_repost() {
        let dir = tempfile::tempdir().unwrap();
        let mut steps = preflight();
        steps.push((
            "POST /api/session/ses_A/prompt",
            ok(json!({"data":{"id":"msg_other","sessionID":"ses_A","payload":{"text":"hello"}}})),
        ));
        let native = live(dir.path(), steps);
        assert_eq!(run(&native, "d").unwrap_err(), MISMATCH);
        assert_eq!(native.posted.borrow().len(), 1);
        let saved: Value = serde_json::from_slice(&fs::read(&native.path).unwrap()).unwrap();
        assert_eq!(saved["status"], "pending");
    }
    #[test]
    fn a_transport_error_reconciles_and_a_retry_reuses_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut steps = preflight();
        steps.push(("POST /api/session/ses_A/prompt", Err("reset".into())));
        steps.push(("GET /api/session/ses_A/message/ID", status(500, json!({}))));
        let native = live(dir.path(), steps);
        // The message ID is generated inside, so match the path loosely.
        struct Loose<'a>(&'a Live);
        impl Native for Loose<'_> {
            fn raw(&self, m: &str, p: &str, b: Option<&Value>) -> Result<Response> {
                let p = if p.contains("/message/") { "/api/session/ses_A/message/ID" } else { p };
                self.0.raw(m, p, b)
            }
        }
        let error = deliver(
            &Loose(&native),
            &Ledger(native.dir.clone()),
            &session_record(),
            &request(),
            "d".into(),
            None,
        )
        .unwrap_err();
        assert!(error.starts_with("Delivery uncertain"));
        let first = native.posted.borrow()[0]["id"].as_str().unwrap().to_owned();
        let saved: Value = serde_json::from_slice(&fs::read(&native.path).unwrap()).unwrap();
        assert_eq!(saved["status"], "pending");
        assert_eq!(saved["native_message_id"], first);
        // Same request and digest: reconcile only; the message is now found.
        let retry = live(
            dir.path(),
            vec![
                ("GET /api/session/ses_A", ok(json!({"data":{"id":"ses_A","location":{"directory":"/x"}}}))),
                ("GET /api/session/ses_A/message/ID", ok(json!({"data":{"id":first,"type":"user","text":"hello"}}))),
            ],
        );
        let receipt = deliver(
            &Loose(&retry),
            &Ledger(retry.dir.clone()),
            &session_record(),
            &request(),
            "d".into(),
            None,
        )
        .unwrap();
        assert_eq!(receipt["status"], "submitted");
        assert_eq!(receipt["native_message_id"], first);
        assert!(retry.posted.borrow().is_empty());
    }
    #[test]
    fn only_a_message_not_found_404_reads_the_inbox() {
        let record = json!({"conversation_id": "ses_A"});
        let native = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/session/ses_A/message/msg_X".into(),
            404,
            json!({"_tag": "SessionNotFoundError"}),
        )]));
        let dir = tempfile::tempdir().unwrap();
        assert!(reconcile(
            &native,
            &Ledger(dir.path().into()),
            &record,
            "msg_X",
            "hello"
        )
        .is_err());
        assert!(native.0.borrow().is_empty());
    }
    /// How the fake answers a DELETE of a queued message.
    #[derive(Clone, Copy, Default, PartialEq)]
    enum Deletion {
        /// Removed, 204.
        #[default]
        Removes,
        /// Removed, but the response is lost.
        LostAfterRemoving,
        /// Not removed, HTTP 500.
        Fails,
        /// Not removed, HTTP 404.
        Missing,
        /// Gone without a trace, HTTP 404.
        MissingAndGone,
        /// Delivered by a racing turn first; OpenCode 2.0.32 still answers 204.
        DeliveredFirst,
    }
    /// One OpenCode 2.0.32 session `ses_A` in directory `/x`, modelled on the
    /// pinned upstream inbox: a prompt ID is deduplicated only against the
    /// current inbox and delivered messages, so a removed ID is admitted again.
    #[derive(Default)]
    struct World {
        active: bool,
        down: bool,
        lose_post: bool,
        deletion: Deletion,
        inbox: Vec<Value>,
        messages: Vec<Value>,
        posts: usize,
        deletes: usize,
        calls: usize,
    }
    struct Fake(std::cell::RefCell<World>);
    impl Native for &Fake {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            let mut w = self.0.borrow_mut();
            w.calls += 1;
            if w.down {
                return Err("connection refused".into());
            }
            let reply = |status: u16, body: Value| Ok(Response { status, body });
            if let Some(id) = path.strip_prefix("/api/session/ses_A/message/") {
                assert_eq!(method, "GET");
                return match w.messages.iter().find(|m| m["id"] == id) {
                    Some(m) => reply(200, json!({ "data": m })),
                    None => reply(404, json!({"_tag": "MessageNotFoundError"})),
                };
            }
            if let Some(id) = path.strip_prefix("/api/session/ses_A/inbox/") {
                assert_eq!(method, "DELETE");
                w.deletes += 1;
                let id = id.to_owned();
                let present = w.inbox.iter().any(|i| i["id"] == id.as_str());
                let remove = |w: &mut World| w.inbox.retain(|i| i["id"] != id.as_str());
                return match w.deletion {
                    Deletion::Removes => {
                        remove(&mut w);
                        reply(204, Value::Null)
                    }
                    Deletion::LostAfterRemoving => {
                        remove(&mut w);
                        Err("connection reset".into())
                    }
                    Deletion::Fails => reply(500, json!({"message": "busy"})),
                    Deletion::Missing => reply(404, json!({"_tag": "SessionNotFoundError"})),
                    Deletion::MissingAndGone => {
                        remove(&mut w);
                        reply(404, json!({"_tag": "SessionNotFoundError"}))
                    }
                    Deletion::DeliveredFirst => {
                        if present {
                            remove(&mut w);
                            w.messages
                                .push(json!({"id": id, "type": "user", "text": "hello"}));
                        }
                        reply(204, Value::Null)
                    }
                };
            }
            match (method, path) {
                ("GET", "/api/session/active") => {
                    let data = if w.active {
                        json!({"ses_A": {}})
                    } else {
                        json!({})
                    };
                    reply(200, json!({ "data": data }))
                }
                ("GET", "/api/session/ses_A") => reply(
                    200,
                    json!({"data": {"id": "ses_A", "location": {"directory": "/x"}}}),
                ),
                ("GET", "/api/session/ses_A/form") | ("GET", "/api/session/ses_A/permission") => {
                    reply(200, json!({"data": []}))
                }
                ("GET", "/api/session/ses_A/inbox") => reply(200, json!({"data": w.inbox.clone()})),
                ("POST", "/api/session/ses_A/prompt") => {
                    w.posts += 1;
                    let body = body.unwrap();
                    let item = json!({"id": body["id"], "sessionID": "ses_A", "type": "user", "delivery": "queue", "payload": {"text": body["text"]}});
                    let known = w.inbox.iter().any(|i| i["id"] == body["id"])
                        || w.messages.iter().any(|m| m["id"] == body["id"]);
                    if !known {
                        if w.active {
                            w.inbox.push(item.clone());
                        } else {
                            w.messages.push(
                                json!({"id": body["id"], "type": "user", "text": body["text"]}),
                            );
                        }
                    }
                    if w.lose_post {
                        // The process loses the service right after the POST.
                        w.down = true;
                        return Err("connection reset".into());
                    }
                    reply(200, json!({ "data": item }))
                }
                _ => panic!("unexpected {method} {path}"),
            }
        }
    }
    impl Fake {
        fn new() -> Self {
            Fake(Default::default())
        }
        fn w(&self) -> std::cell::RefMut<'_, World> {
            self.0.borrow_mut()
        }
        /// The message `hello` of `request()` was admitted while the session
        /// was busy, every response after the POST was lost, and the user
        /// then stopped the session: the receipt is pending, the message
        /// queued and removable.
        fn queued_while_pending(dir: &Path) -> (Self, String) {
            let fake = Fake::new();
            fake.w().active = true;
            fake.w().lose_post = true;
            let error = t_deliver(&fake, dir).unwrap_err();
            assert_eq!(error, UNCERTAIN_REPLAY);
            let receipt = t_receipt(dir);
            assert_eq!(receipt["status"], "pending");
            let id = receipt["native_message_id"].as_str().unwrap().to_owned();
            let mut w = fake.w();
            assert_eq!(w.posts, 1);
            assert_eq!(w.inbox.len(), 1);
            w.down = false;
            w.lose_post = false;
            w.active = false;
            drop(w);
            (fake, id)
        }
    }
    fn t_receipt_path(dir: &Path) -> PathBuf {
        dir.join("receipts")
            .join(format!("{}.json", string(&request(), "request_id")))
    }
    fn t_receipt(dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(t_receipt_path(dir)).unwrap()).unwrap()
    }
    fn t_deliver(fake: &Fake, dir: &Path) -> Result<Value> {
        deliver(
            &fake,
            &Ledger(dir.into()),
            &session_record(),
            &request(),
            "d".into(),
            None,
        )
    }
    fn t_remove(fake: &Fake, dir: &Path, id: &str) -> Result<()> {
        remove_queued_with(&fake, &Ledger(dir.into()), &session_record(), id, || {})
    }
    fn t_reconcile(fake: &Fake, dir: &Path, id: &str) -> Result<Delivery> {
        reconcile(&fake, &Ledger(dir.into()), &session_record(), id, "hello")
    }
    fn t_reconcile_receipt(fake: &Fake, dir: &Path, receipt: &mut Value) -> Result<bool> {
        reconcile_receipt_with(&Ledger(dir.into()), &session_record(), receipt, || Ok(fake))
    }
    const T_REMOVED: &str = "Removed from the OpenCode queue before delivery";
    #[test]
    fn a_message_removed_while_its_receipt_is_pending_is_never_reposted() {
        let dir = tempfile::tempdir().unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        t_remove(&fake, dir.path(), &id).unwrap();
        assert!(fake.w().inbox.is_empty());
        // The same request again: a definite non-delivery, never a second POST.
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), T_REMOVED);
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), T_REMOVED);
        let w = fake.w();
        assert_eq!((w.posts, w.deletes), (1, 1));
        assert!(w.inbox.is_empty() && w.messages.is_empty());
        let receipt = t_receipt(dir.path());
        assert_eq!(
            (receipt["status"].as_str(), receipt["reason"].as_str()),
            (Some("refused"), Some(T_REMOVED))
        );
        assert_eq!(receipt["native_message_id"], id);
    }
    #[test]
    fn a_late_reconciliation_after_removal_is_refused_without_contacting_opencode() {
        let dir = tempfile::tempdir().unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        let mut receipt = t_receipt(dir.path());
        // A receipt the removal could not find by its message ID.
        let _ = fs::remove_dir_all(dir.path().join("receipt-index"));
        t_remove(&fake, dir.path(), &id).unwrap();
        assert_eq!(t_receipt(dir.path())["status"], "pending");
        let calls = fake.w().calls;
        assert_eq!(
            t_reconcile_receipt(&fake, dir.path(), &mut receipt).unwrap_err(),
            T_REMOVED
        );
        assert_eq!(fake.w().calls, calls);
        assert_eq!(fake.w().posts, 1);
        assert_eq!(
            (receipt["status"].as_str(), receipt["reason"].as_str()),
            (Some("refused"), Some(T_REMOVED))
        );
        assert_eq!(t_receipt(dir.path()), receipt);
    }
    #[test]
    fn a_message_in_neither_history_nor_inbox_is_uncertain_and_never_posted() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new();
        assert!(t_reconcile(&fake, dir.path(), "msg_X").is_err());
        assert_eq!(fake.w().posts, 0);
        // A journaled request whose POST never reached OpenCode is not sent either.
        let (fake, _) = Fake::queued_while_pending(dir.path());
        fake.w().inbox.clear();
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), UNCERTAIN_REPLAY);
        assert_eq!(fake.w().posts, 1);
        assert_eq!(t_receipt(dir.path())["status"], "pending");
    }
    #[test]
    fn stale_removal_targets_are_refused_without_a_delete() {
        let dir = tempfile::tempdir().unwrap();
        let item = |session: &str| json!({"id": "msg_Q", "sessionID": session, "type": "user", "delivery": "queue", "payload": {"text": "hello"}});
        // Each refusal is clean: no DELETE and no removal record.
        let refused_untouched = |fake: &Fake| {
            assert_eq!(t_remove(fake, dir.path(), "msg_Q").unwrap_err(), NOT_QUEUED);
            assert_eq!(fake.w().deletes, 0);
            let ledger = Ledger(dir.path().into());
            assert_eq!(ledger.removal(&session_record(), "msg_Q").unwrap(), None);
            assert!(!ledger.removals_path().exists());
        };
        // Not queued.
        refused_untouched(&Fake::new());
        // Already delivered: present in history. Without the history check
        // before the DELETE this would record an intent and DELETE.
        let fake = Fake::new();
        fake.w().inbox.push(item("ses_A"));
        fake.w()
            .messages
            .push(json!({"id": "msg_Q", "type": "user", "text": "hello"}));
        refused_untouched(&fake);
        // Listed for another session.
        let fake = Fake::new();
        fake.w().inbox.push(item("ses_B"));
        refused_untouched(&fake);
        // The session is running.
        let fake = Fake::new();
        fake.w().inbox.push(item("ses_A"));
        fake.w().active = true;
        refused_untouched(&fake);
    }
    /// A scoped Remove for another run or session is refused before any
    /// native call or removal record.
    #[test]
    fn a_removal_for_another_run_or_session_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let fake = Fake::new();
        fake.w().inbox.push(json!({"id": "msg_Q", "sessionID": "ses_A", "type": "user", "delivery": "queue", "payload": {"text": "hello"}}));
        // No binding exists under this name, so nothing past the identity
        // check could succeed either.
        let name = format!("opencode/p/{}", uuid::Uuid::new_v4());
        for (run, conversation) in [("r2", "ses_A"), ("r1", "ses_B")] {
            let scope = session_action::Scope::expecting(&name, run, conversation);
            let mut record =
                json!({"name": name, "run_id": "r1", "conversation_id": "ses_A", "cwd": "/x"});
            let connected = std::cell::Cell::new(false);
            let error = action_in(
                ActionStorage { ledger: &ledger, rename_marks: None },
                || {
                    connected.set(true);
                    Ok(&fake)
                },
                &mut record,
                "remove_queued",
                None,
                Some("msg_Q"),
                Some(&scope),
            )
            .unwrap_err();
            assert_eq!(
                error,
                "Session identity changed; no replacement session was selected"
            );
            assert!(!connected.get());
            assert!(!scope.was_changing());
            let w = fake.w();
            assert_eq!((w.calls, w.deletes, w.inbox.len()), (0, 0, 1));
            drop(w);
            assert_eq!(ledger.removal(&session_record(), "msg_Q").unwrap(), None);
            assert!(!ledger.removals_path().exists());
        }
    }
    #[test]
    fn a_lost_or_failed_delete_never_allows_a_repost() {
        for deletion in [Deletion::LostAfterRemoving, Deletion::Fails] {
            let dir = tempfile::tempdir().unwrap();
            let (fake, id) = Fake::queued_while_pending(dir.path());
            fake.w().deletion = deletion;
            assert!(t_remove(&fake, dir.path(), &id).is_err());
            assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), UNCERTAIN_REPLAY);
            assert_eq!(fake.w().posts, 1);
            assert_eq!(t_receipt(dir.path())["status"], "pending");
            assert!(fake.w().messages.is_empty());
        }
    }
    #[test]
    fn a_missing_delete_is_delivered_only_when_history_shows_it() {
        // 404, and the message is later delivered: reported delivered.
        let dir = tempfile::tempdir().unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        fake.w().deletion = Deletion::Missing;
        assert_eq!(t_remove(&fake, dir.path(), &id).unwrap_err(), NOT_QUEUED);
        let mut w = fake.w();
        w.inbox.clear();
        w.messages
            .push(json!({"id": id, "type": "user", "text": "hello"}));
        drop(w);
        assert_eq!(t_deliver(&fake, dir.path()).unwrap()["status"], "submitted");
        assert_eq!(t_receipt(dir.path())["status"], "submitted");
        // 404, and the message is nowhere: uncertain, never posted again.
        let dir = tempfile::tempdir().unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        fake.w().deletion = Deletion::MissingAndGone;
        assert_eq!(t_remove(&fake, dir.path(), &id).unwrap_err(), NOT_QUEUED);
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), UNCERTAIN_REPLAY);
        assert_eq!(fake.w().posts, 1);
        assert_eq!(t_receipt(dir.path())["status"], "pending");
    }
    #[test]
    fn a_204_for_a_message_delivered_meanwhile_is_not_a_removal() {
        let dir = tempfile::tempdir().unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        fake.w().deletion = Deletion::DeliveredFirst;
        assert_eq!(t_remove(&fake, dir.path(), &id).unwrap_err(), NOT_QUEUED);
        assert_eq!(t_receipt(dir.path())["status"], "pending");
        assert_eq!(t_deliver(&fake, dir.path()).unwrap()["status"], "submitted");
        assert_eq!(fake.w().posts, 1);
    }
    #[test]
    fn a_crash_between_the_intent_and_the_delete_never_reposts() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let (fake, id) = Fake::queued_while_pending(dir.path());
        // The intent was written, then the process died before the DELETE.
        ledger
            .record_removal(&session_record(), &id, Removal::Intended)
            .unwrap();
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), UNCERTAIN_REPLAY);
        let mut receipt = t_receipt(dir.path());
        assert!(t_reconcile_receipt(&fake, dir.path(), &mut receipt).is_err());
        assert_eq!(t_receipt(dir.path())["status"], "pending");
        let w = fake.w();
        assert_eq!((w.posts, w.inbox.len()), (1, 1));
        drop(w);
        // Removing it again confirms the removal.
        t_remove(&fake, dir.path(), &id).unwrap();
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), T_REMOVED);
        assert_eq!(fake.w().posts, 1);
        assert!(fake.w().messages.is_empty());
    }
    #[test]
    fn a_message_indexed_for_another_session_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        atomic(
            &ledger.index("msg_Q"),
            &json!({"request_id": "22222222-2222-4222-8222-222222222222", "conversation_id": "ses_B"}).to_string(),
        )
        .unwrap();
        let fake = Fake::new();
        fake.w().inbox.push(json!({"id": "msg_Q", "sessionID": "ses_A", "type": "user", "payload": {"text": "hello"}}));
        assert_eq!(
            t_remove(&fake, dir.path(), "msg_Q").unwrap_err(),
            NOT_QUEUED
        );
        assert_eq!(fake.w().deletes, 0);
        assert_eq!(ledger.removal(&session_record(), "msg_Q").unwrap(), None);
    }
    #[test]
    fn removal_records_are_bounded_and_unreadable_ones_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let entries: Vec<Value> = (0..REMOVAL_LIMIT)
            .map(|n| json!({"conversation_id": "ses_A", "message_id": format!("msg_{n}"), "state": "removed"}))
            .collect();
        atomic(
            &ledger.removals_path(),
            &json!({ "removals": entries }).to_string(),
        )
        .unwrap();
        ledger
            .record_removal(&session_record(), "msg_new", Removal::Intended)
            .unwrap();
        let kept = ledger.removals().unwrap();
        assert_eq!(kept.len(), REMOVAL_LIMIT);
        assert_eq!(kept[0]["message_id"], "msg_1");
        assert_eq!(
            ledger.removal(&session_record(), "msg_new").unwrap(),
            Some(Removal::Intended)
        );
        // Unreadable records never read as "no removal" and block a removal.
        fs::write(ledger.removals_path(), b"{\"removals\":").unwrap();
        let (fake, id) = Fake::queued_while_pending(dir.path());
        assert!(t_reconcile(&fake, dir.path(), &id).is_err());
        assert!(t_remove(&fake, dir.path(), &id).is_err());
        assert_eq!(t_deliver(&fake, dir.path()).unwrap_err(), UNCERTAIN_REPLAY);
        let w = fake.w();
        assert_eq!((w.posts, w.deletes), (1, 0));
    }
    fn opencode2(name: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/opencode2/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    fn observed(messages: &str, active: bool, inbox: Value) -> Observed {
        let mut page = opencode2(messages)["data"].as_array().unwrap().clone();
        page.reverse();
        Observed {
            session: json!({"id":"ses_A","model":{"id":"local","providerID":"fixture","variant":"default"},"time":{"updated":5000}}),
            active,
            page: history::Page {
                messages: page,
                truncated: false,
            },
            inbox,
            forms: json!([]),
            permissions: json!([]),
            requests: Default::default(),
        }
    }
    fn status_record(id: &str) -> Value {
        json!({"name":"opencode/project/one","run_id":"run","conversation_id":id,"cwd":"/x"})
    }
    fn no_inbox() -> Value {
        json!({"data": []})
    }
    #[test]
    fn an_idle_session_is_ready_for_messages_and_settings() {
        let value = normalize(
            &status_record("ses_A"),
            Some(&observed("messages", false, no_inbox())),
        );
        assert_eq!(value["phase"], "idle");
        assert_eq!(value["activity"], "idle");
        assert_eq!(value["activity_summary"], "Ready");
        assert_eq!(value["state"], "running");
        assert_eq!(value["send_available"], true);
        assert_eq!(value["settings_change_supported"], true);
        assert_eq!(value["interrupt_supported"], false);
        assert_eq!(value["allowed_actions"], json!(["rename", "forget"]));
        assert_eq!(value["settings_model"], "fixture/local");
        assert!(value["effort"].is_null());
        assert_eq!(value["events"].as_array().unwrap().len(), 3);
        assert_eq!(value["reply_id"], "msg_14bb2d63e001NhfRTKy4frROM8");
        assert_eq!(value["activity_detail"], "Native OpenCode reply");
        assert_eq!(value["prompt"], "hello");
        assert_eq!(value["turn_started"], 1792271373.881);
        assert_eq!(value["history_truncated"], false);
        for key in [
            "terminal_supported",
            "attachments_supported",
            "native_ui_available",
            "compact_context_supported",
        ] {
            assert_eq!(value[key], false, "{key}");
        }
        assert_eq!(
            value["action_reasons"]["pause"],
            "OpenCode sessions have no separate process to pause"
        );
        assert_eq!(
            value["action_reasons"]["fork"],
            "OpenCode fork arrives in a later update"
        );
        assert_eq!(
            value["action_reasons"]["terminate"],
            "Use Stop for this OpenCode session"
        );
        assert_eq!(
            value["compact_context_reason"],
            "OpenCode compaction arrives in a later update"
        );
        assert!(value.get("attention_id").is_none());
    }
    #[test]
    fn a_running_session_is_working_and_still_accepts_queued_messages() {
        let inbox = opencode2("inbox_busy");
        let value = normalize(
            &status_record("ses_A"),
            Some(&observed("messages", true, inbox)),
        );
        assert_eq!(value["phase"], "working");
        assert_eq!(value["activity"], "busy");
        assert_eq!(value["activity_summary"], "Working");
        assert_eq!(value["send_available"], true);
        assert_eq!(value["interrupt_supported"], true);
        assert_eq!(value["settings_change_supported"], false);
        let events = value["events"].as_array().unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[3]["delivery_state"], "queued");
        assert_eq!(events[3]["detail"], "queued later");
        assert_eq!(value["allowed_actions"], json!(["rename", "forget"]));
        // Queued text is not the prompt the agent is working on.
        assert_eq!(value["prompt"], "hello");
    }
    #[test]
    fn prompts_zerus_sent_name_their_request() {
        let mut observed = observed("messages", true, opencode2("inbox_busy"));
        observed.requests = [
            ("msg_6kezDqwZEklnh8eFGVy3UbP6iX", "request-delivered"),
            ("msg_gVmstesSuU81H2vTHdmxp65tNI", "request-queued"),
            ("msg_14bb2d63e001NhfRTKy4frROM8", "request-not-a-prompt"),
        ]
        .map(|(id, request)| (id.to_owned(), request.to_owned()))
        .into();
        let value = normalize(&status_record("ses_A"), Some(&observed));
        let named: Vec<(&str, &str, &str)> = value["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (string(e, "type"), string(e, "message_id"), string(e, "request_id")))
            .filter(|(_, _, request)| !request.is_empty())
            .collect();
        assert_eq!(
            named,
            [
                ("UserPromptSubmit", "msg_6kezDqwZEklnh8eFGVy3UbP6iX", "request-delivered"),
                ("UserPromptSubmit", "msg_gVmstesSuU81H2vTHdmxp65tNI", "request-queued"),
            ]
        );
    }
    #[test]
    fn sent_requests_come_only_from_this_sessions_receipt_index() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let index = |id: &str, request: &str, conversation: &str| {
            atomic(
                &ledger.index(id),
                &json!({"request_id": request, "conversation_id": conversation}).to_string(),
            )
            .unwrap()
        };
        // Receipts with the exact bodies OpenCode shows.
        let receipt = |id: &str, request: &str, text: &str| {
            atomic(
                &ledger.receipt(request),
                &json!({"request_id": request, "conversation_id": "ses_A", "run_id": "run",
                    "native_message_id": id, "text": text})
                .to_string(),
            )
            .unwrap()
        };
        let a = "6f0e3f43-4b4c-4f2d-9f43-3d6b6c1f1a01";
        let b = "6f0e3f43-4b4c-4f2d-9f43-3d6b6c1f1a02";
        index("msg_6kezDqwZEklnh8eFGVy3UbP6iX", a, "ses_A");
        index("msg_gVmstesSuU81H2vTHdmxp65tNI", b, "ses_A");
        receipt("msg_6kezDqwZEklnh8eFGVy3UbP6iX", a, "hello");
        receipt("msg_gVmstesSuU81H2vTHdmxp65tNI", b, "queued later");
        // User messages whose index names another session or no request.
        index("msg_otherSession00000000000000", a, "ses_B");
        index("msg_notARequest000000000000000", "not-a-uuid", "ses_A");
        let observed = observed("messages", true, opencode2("inbox_busy"));
        let mut messages = observed.page.messages.clone();
        messages.push(json!({"id": "msg_otherSession00000000000000", "type": "user"}));
        messages.push(json!({"id": "msg_notARequest000000000000000", "type": "user"}));
        let found = sent_requests(&ledger, &status_record("ses_A"), &messages, &observed.inbox);
        let expected: std::collections::HashMap<String, String> = [
            ("msg_6kezDqwZEklnh8eFGVy3UbP6iX".to_owned(), a.to_owned()),
            ("msg_gVmstesSuU81H2vTHdmxp65tNI".to_owned(), b.to_owned()),
        ]
        .into();
        assert_eq!(found, expected);
        // Only the newest user messages are looked up: the queued one first.
        let many: Vec<Value> = (0..SENT_LOOKUPS + 5)
            .map(|n| json!({"id": format!("msg_{n:026}"), "type": "user", "text": format!("t{n}")}))
            .collect();
        for n in 0..SENT_LOOKUPS + 5 {
            let request = format!("6f0e3f43-4b4c-4f2d-9f43-{n:012}");
            index(&format!("msg_{n:026}"), &request, "ses_A");
            receipt(&format!("msg_{n:026}"), &request, &format!("t{n}"));
        }
        let found = sent_requests(&ledger, &status_record("ses_A"), &many, &observed.inbox);
        assert_eq!(found.len(), SENT_LOOKUPS);
        assert!(found.contains_key("msg_gVmstesSuU81H2vTHdmxp65tNI"));
        assert!(found.contains_key(&format!("msg_{:026}", SENT_LOOKUPS + 4)));
        assert!(!found.contains_key(&format!("msg_{:026}", 5)));
    }
    #[test]
    fn the_bindings_lock_never_waits_on_this_threads_own_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bindings.lock");
        let held = lock_bindings_at(&path).unwrap();
        // An action's own inspection: granted at once, holding nothing more.
        let started = Instant::now();
        let inner = try_bindings_at(&path, Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        drop(inner);
        // Another thread (as another process would) waits and gives up.
        let other = path.clone();
        let busy = std::thread::spawn(move || {
            try_bindings_at(&other, Duration::from_millis(50)).is_none()
        });
        assert!(busy.join().unwrap());
        drop(held);
        let other = path.clone();
        let free = std::thread::spawn(move || {
            try_bindings_at(&other, Duration::from_millis(50)).is_some()
        });
        assert!(free.join().unwrap());
    }
    #[test]
    fn a_request_is_attributed_only_to_its_exact_message_body() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let sent = |id: &str, request: &str, text: &str| {
            atomic(
                &ledger.index(id),
                &json!({"request_id": request, "conversation_id": "ses_A"}).to_string(),
            )
            .unwrap();
            atomic(
                &ledger.receipt(request),
                &json!({"request_id": request, "conversation_id": "ses_A", "run_id": "run",
                    "native_message_id": id, "text": text, "status": "uncertain"})
                .to_string(),
            )
            .unwrap();
        };
        let a = "6f0e3f43-4b4c-4f2d-9f43-3d6b6c1f1a01";
        let b = "6f0e3f43-4b4c-4f2d-9f43-3d6b6c1f1a02";
        // The same message ID with another body is not this request, as
        // reconcile leaves it uncertain.
        sent("msg_6kezDqwZEklnh8eFGVy3UbP6iX", a, "not what OpenCode shows");
        // A message dated by its late delivery, with the exact body, is.
        sent("msg_gVmstesSuU81H2vTHdmxp65tNI", b, "queued later");
        let observed = observed("messages", true, opencode2("inbox_busy"));
        let found = sent_requests(&ledger, &status_record("ses_A"), &observed.page.messages, &observed.inbox);
        let expected: std::collections::HashMap<String, String> =
            [("msg_gVmstesSuU81H2vTHdmxp65tNI".to_owned(), b.to_owned())].into();
        assert_eq!(found, expected);
        // A message in both reads is judged by its delivered copy: an edited
        // delivered body is not attributed even though the queued copy matched.
        let edited = vec![json!({"id": "msg_gVmstesSuU81H2vTHdmxp65tNI", "type": "user", "text": "edited on delivery"})];
        assert!(sent_requests(&ledger, &status_record("ses_A"), &edited, &observed.inbox).is_empty());
        // Another run of the same conversation is not this receipt's.
        let mut other_run = status_record("ses_A");
        other_run["run_id"] = json!("another-run");
        assert!(sent_requests(&ledger, &other_run, &observed.page.messages, &observed.inbox).is_empty());
        // The delivered copy of the same message, with its exact body, too.
        let delivered = vec![json!({"id": "msg_gVmstesSuU81H2vTHdmxp65tNI", "type": "user", "text": "queued later"})];
        let found = sent_requests(&ledger, &status_record("ses_A"), &delivered, &json!({"data": []}));
        assert_eq!(found, expected);
    }
    #[test]
    fn after_stop_queued_messages_are_not_delivered_and_removable() {
        let value = normalize(
            &status_record("ses_A"),
            Some(&observed("messages", false, opencode2("inbox_busy"))),
        );
        assert_eq!(value["phase"], "idle");
        assert_eq!(value["events"][3]["delivery_state"], "not_delivered");
        assert_eq!(
            value["allowed_actions"],
            json!(["rename", "forget", "remove_queued"])
        );
    }
    #[test]
    fn a_message_admitted_between_reads_is_shown_once() {
        let inbox = json!({"data":[{"id":"msg_6kezDqwZEklnh8eFGVy3UbP6iX","type":"user","payload":{"text":"hello"},"time":{"created":1}}]});
        let value = normalize(
            &status_record("ses_A"),
            Some(&observed("messages", false, inbox)),
        );
        assert_eq!(value["events"].as_array().unwrap().len(), 3);
        assert_eq!(value["allowed_actions"], json!(["rename", "forget"]));
    }
    #[test]
    fn replies_route_to_the_form_or_permission_with_validated_bodies() {
        let record = json!({"run_id":"run","conversation_id":"ses_A"});
        let form = json!({"id":"frm_1","sessionID":"ses_A","title":"Q","fields":[{"key":"q0","title":"C","type":"string"}]});
        let card = forms::cards(&record, &json!([form]), &json!([])).remove(0);
        let ask = [json!({"question_id":"q0","selected_option_ids":[],"text":"hi"})];
        assert_eq!(
            reply_for(&card, &form, &ask).unwrap(),
            (
                "/form/frm_1/reply".to_owned(),
                json!({"answer":{"q0":"hi"}})
            )
        );
        let permission =
            json!({"id":"per_1","sessionID":"ses_A","action":"shell","resources":["x"]});
        let card = forms::cards(&record, &json!([]), &json!([permission])).remove(0);
        let ok = [json!({"question_id":"permission","selected_option_ids":["reject"],"text":""})];
        assert_eq!(
            reply_for(&card, &permission, &ok).unwrap(),
            (
                "/permission/per_1/reply".to_owned(),
                json!({"decision":"reject"})
            )
        );
        assert!(reply_for(&card, &permission, &ask).is_err());
    }
    struct Answering {
        forms: std::cell::RefCell<Value>,
        reply: Result<Response>,
        posted: std::cell::RefCell<Vec<(String, Value)>>,
    }
    impl Native for Answering {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            if method == "GET" {
                assert_eq!(path, "/api/session/ses_A/form");
                return ok(json!({"data": self.forms.borrow().clone()}));
            }
            self.posted.borrow_mut().push((path.to_owned(), body.unwrap().clone()));
            self.reply.clone()
        }
    }
    fn answering(reply: Result<Response>) -> (Answering, Value, Value) {
        let form = json!({"id":"frm_1","sessionID":"ses_A","title":"Q","fields":[{"key":"q0","title":"C","type":"string"}]});
        let card = forms::cards(&session_record(), &json!([form.clone()]), &json!([])).remove(0);
        let request = json!({"request_id":"r1","question_id":"frm_1","expected_question_hash":card["question_hash"],
            "answers":[{"question_id":"q0","selected_option_ids":[],"text":"hi"}]});
        let native = Answering { forms: json!([form]).into(), reply, posted: Default::default() };
        (native, card, request)
    }
    fn journal(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    #[test]
    fn a_4xx_answer_is_refused_once_and_never_retried() {
        for tag in ["FormAlreadySettledError", "FormNotFoundError"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("r1.json");
            let (native, card, request) = answering(status(409, json!({"_tag":tag,"message":"gone"})));
            let record = session_record();
            assert_eq!(answer_with(&native, &record, &request, &card, &path).unwrap_err(), CHANGED);
            assert_eq!(native.posted.borrow().len(), 1);
            assert_eq!(native.posted.borrow()[0].0, "/api/session/ses_A/form/frm_1/reply");
            assert_eq!(journal(&path)["status"], "refused");
            assert!(answer_with(&native, &record, &request, &card, &path).is_err());
            assert_eq!(native.posted.borrow().len(), 1);
        }
    }
    #[test]
    fn an_invalid_answer_shows_the_clipped_server_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r1.json");
        let message = format!("bad\u{7}{}", "y".repeat(500));
        let (native, card, request) =
            answering(status(400, json!({"_tag":"FormInvalidAnswerError","message":message})));
        let error = answer_with(&native, &session_record(), &request, &card, &path).unwrap_err();
        assert_eq!(error, format!("OpenCode rejected this answer: bad{}", "y".repeat(197)));
        assert_eq!(journal(&path)["status"], "refused");
    }
    #[test]
    fn a_changed_native_request_is_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r1.json");
        let (native, card, request) = answering(status(204, json!({})));
        native.forms.borrow_mut()[0]["title"] = json!("Changed");
        assert_eq!(answer_with(&native, &session_record(), &request, &card, &path).unwrap_err(), CHANGED);
        assert!(native.posted.borrow().is_empty());
        assert!(!path.exists());
        *native.forms.borrow_mut() = json!([]);
        assert_eq!(answer_with(&native, &session_record(), &request, &card, &path).unwrap_err(), CHANGED);
        assert!(native.posted.borrow().is_empty());
    }
    #[test]
    fn an_unknown_delivery_stays_pending_and_blocks_a_replay() {
        for reply in [status(503, json!({"message":"down\u{7}"})), Err("reset".to_owned())] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("r1.json");
            let (native, card, request) = answering(reply);
            let record = session_record();
            assert!(answer_with(&native, &record, &request, &card, &path).is_err());
            assert_eq!(journal(&path)["status"], "pending");
            assert!(answer_with(&native, &record, &request, &card, &path)
                .unwrap_err()
                .contains("already attempted"));
            assert_eq!(native.posted.borrow().len(), 1);
        }
    }
    #[test]
    fn an_accepted_answer_is_journaled_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r1.json");
        let (native, card, request) = answering(status(204, json!({})));
        let receipt = answer_with(&native, &session_record(), &request, &card, &path).unwrap();
        assert_eq!(receipt["status"], "answered");
        assert_eq!(native.posted.borrow()[0].1, json!({"answer":{"q0":"hi"}}));
    }
    #[test]
    fn pending_forms_or_permissions_need_attention_before_work() {
        let forms = opencode2("forms_question");
        let id = forms["data"][0]["sessionID"].as_str().unwrap();
        let mut seen = observed("messages_question", true, no_inbox());
        seen.forms = forms["data"].clone();
        let value = normalize(&status_record(id), Some(&seen));
        assert_eq!(value["phase"], "input");
        assert_eq!(value["activity"], "attention");
        assert_eq!(value["activity_summary"], "Needs attention");
        assert_eq!(value["send_available"], false);
        assert_eq!(value["settings_change_supported"], false);
        assert_eq!(value["interrupt_supported"], true);
        assert_eq!(
            value["attention_id"],
            "opencode_question:frm_14bb2df2500101YAecuyKBa9qA"
        );
        let permissions = opencode2("permissions");
        let id = permissions["data"][0]["sessionID"].as_str().unwrap();
        let mut seen = observed("messages", false, no_inbox());
        seen.permissions = permissions["data"].clone();
        let value = normalize(&status_record(id), Some(&seen));
        assert_eq!(value["phase"], "input");
        assert_eq!(value["pending_questions"][0]["kind"], "opencode_permission");
        // Another session's request never needs this session's attention.
        let value = normalize(&status_record("ses_A"), Some(&seen));
        assert_eq!(value["phase"], "idle");
    }
    #[test]
    fn a_failed_turn_is_a_provider_error_until_work_resumes() {
        let record = status_record("ses_A");
        let value = normalize(
            &record,
            Some(&observed("messages_error", false, no_inbox())),
        );
        assert_eq!(value["phase"], "error");
        assert_eq!(value["activity"], "attention");
        assert_eq!(
            value["provider_error"]["message_id"],
            "opencode-error-msg_14bb546e7001QR5FwtWF0VburA"
        );
        assert_eq!(value["provider_error"]["at"], 1792271533.799);
        assert_eq!(value["last_error"], "Fixture provider rejected");
        let busy = normalize(&record, Some(&observed("messages_error", true, no_inbox())));
        assert_eq!(busy["phase"], "working");
        assert!(busy.get("provider_error").is_none());
    }
    #[test]
    fn an_unreadable_service_is_unknown_and_allows_only_forget() {
        let record = status_record("ses_A");
        let value = unavailable(&record, "OpenCode is not running on fixture".into());
        assert_eq!(value["state"], "unknown");
        assert_eq!(value["phase"], "unknown");
        assert_eq!(value["activity"], "unknown");
        assert_eq!(value["send_available"], false);
        assert_eq!(value["interrupt_supported"], false);
        assert_eq!(value["allowed_actions"], json!(["forget"]));
        assert_eq!(value["inspection_unavailable"], true);
        assert_eq!(
            value["inspection_reason"],
            "OpenCode is not running on fixture"
        );
        assert_eq!(
            value["activity_detail"],
            "OpenCode is not running on fixture"
        );
        let legacy = legacy_row(&record);
        assert_eq!(legacy["state"], "stopped");
        assert_eq!(legacy["allowed_actions"], json!(["forget"]));
        assert!(legacy.get("inspection_unavailable").is_none());
        assert_eq!(legacy["activity_detail"], legacy["inspection_reason"]);
        // Without a reading, clients keep the transcript they already show.
        for row in [&value, &legacy] {
            assert!(row.get("events").is_none());
            assert!(row.get("replace_events").is_none());
        }
    }
    #[test]
    fn a_streaming_reply_is_shown_but_not_yet_the_session_reply() {
        let mut seen = observed("messages", true, no_inbox());
        seen.page.messages.truncate(2);
        seen.page.messages[1]["time"]
            .as_object_mut()
            .unwrap()
            .remove("completed");
        let value = normalize(&status_record("ses_A"), Some(&seen));
        assert_eq!(value["events"][1]["detail"], "Native OpenCode reply");
        assert!(value["reply_id"].is_null());
        assert!(value["activity_detail"].is_null());
    }
    #[test]
    fn effort_is_the_session_variant_and_truncation_is_reported() {
        let mut seen = observed("messages", false, no_inbox());
        seen.session["model"]["variant"] = json!("high");
        seen.page.truncated = true;
        let value = normalize(&status_record("ses_A"), Some(&seen));
        assert_eq!(value["effort"], "high");
        assert_eq!(value["history_truncated"], true);
    }
    #[test]
    fn one_inspection_reads_each_route_once_and_the_activity_map_is_shared() {
        let dir = tempfile::tempdir().unwrap();
        let id = "ses_IycejmWY8RpJq1aeBnnIpbaNAt";
        let record = status_record(id);
        let session = json!({"data":{"id":id,"location":{"directory":"/x"},"model":{"id":"local","providerID":"fixture"}}});
        let messages = opencode2("messages");
        let next = messages["cursor"]["next"].as_str().unwrap().to_owned();
        let native = Scripted(std::cell::RefCell::new(vec![
            (format!("GET /api/session/{id}"), 200, session.clone()),
            (
                format!("GET /api/session/{id}/form"),
                200,
                json!({"data": []}),
            ),
            (
                format!("GET /api/session/{id}/permission"),
                200,
                json!({"data": []}),
            ),
            (
                format!("GET /api/session/{id}/inbox"),
                200,
                opencode2("inbox_busy"),
            ),
            (
                format!("GET /api/session/{id}/message?limit=100&order=desc"),
                200,
                messages,
            ),
            (
                format!("GET /api/session/{id}/message?limit=100&cursor={next}"),
                200,
                json!({"data": [], "cursor": {"next": null}}),
            ),
        ]));
        let active = opencode2("active_busy")["data"].clone();
        let cache = dir.path().join("history.json");
        let seen = observe(
            &native,
            &record,
            &active,
            || history::fetch_at(&native, &record, &cache),
            false,
        )
        .unwrap();
        assert!(native.0.borrow().is_empty());
        assert!(seen.active);
        assert_eq!(seen.page.messages.len(), 3);
        assert_eq!(normalize(&record, Some(&seen))["phase"], "working");
        // The next poll reuses the cached history after one page.
        let native = Scripted(std::cell::RefCell::new(vec![
            (format!("GET /api/session/{id}"), 200, session),
            (
                format!("GET /api/session/{id}/form"),
                200,
                json!({"data": []}),
            ),
            (
                format!("GET /api/session/{id}/permission"),
                200,
                json!({"data": []}),
            ),
            (
                format!("GET /api/session/{id}/inbox"),
                200,
                json!({"data": []}),
            ),
            (
                format!("GET /api/session/{id}/message?limit=100&order=desc"),
                200,
                opencode2("messages"),
            ),
        ]));
        let seen = observe(
            &native,
            &record,
            &json!({}),
            || history::fetch_at(&native, &record, &cache),
            false,
        )
        .unwrap();
        assert!(native.0.borrow().is_empty());
        assert!(!seen.active);
        assert_eq!(seen.page.messages.len(), 3);
    }
    #[test]
    fn a_moved_session_or_invalid_answers_fail_the_inspection() {
        let record = status_record("ses_A");
        let moved = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/session/ses_A".into(),
            200,
            json!({"data":{"id":"ses_A","location":{"directory":"/elsewhere"}}}),
        )]));
        let none = || -> Result<history::Page> { panic!("no history read") };
        assert!(observe(&moved, &record, &json!({}), none, false).is_err());
        let invalid = Scripted(std::cell::RefCell::new(vec![
            (
                "GET /api/session/ses_A".into(),
                200,
                json!({"data":{"id":"ses_A","location":{"directory":"/x"}}}),
            ),
            (
                "GET /api/session/ses_A/form".into(),
                200,
                json!({"data": {}}),
            ),
        ]));
        assert!(observe(&invalid, &record, &json!({}), none, false).is_err());
        let refused = Scripted(std::cell::RefCell::new(vec![(
            "GET /api/session/active".into(),
            500,
            json!({"message": "busy"}),
        )]));
        assert_eq!(
            active_sessions(&refused).unwrap_err(),
            "OpenCode HTTP 500: busy"
        );
    }

    /// Scripted answers that also record every request body.
    struct Seen {
        script: std::cell::RefCell<Vec<(String, u16, Value)>>,
        bodies: std::cell::RefCell<Vec<(String, Option<Value>)>>,
    }
    impl Native for Seen {
        fn raw(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Response> {
            let (expected, status, reply) = self.script.borrow_mut().remove(0);
            assert_eq!(expected, format!("{method} {path}"));
            self.bodies
                .borrow_mut()
                .push((expected, body.cloned()));
            Ok(Response { status, body: reply })
        }
    }
    fn seen(steps: Vec<(&str, u16, Value)>) -> Seen {
        Seen {
            script: std::cell::RefCell::new(
                steps.into_iter().map(|(p, s, b)| (p.to_owned(), s, b)).collect(),
            ),
            bodies: Default::default(),
        }
    }
    fn bound() -> Value {
        json!({"name":"opencode/p/t","conversation_id":"ses_A","cwd":"/x"})
    }
    fn session_with(model: Value, title: &str) -> Value {
        json!({"data":{"id":"ses_A","location":{"directory":"/x"},"model":model,"title":title}})
    }
    #[test]
    fn settings_validate_effort_against_the_model_variants() {
        let rows = catalog::project_all(
            &fixture(include_bytes!("../../tests/fixtures/opencode2/models.json")),
            &HashMap::new(),
        )
        .0;
        assert_eq!(
            catalog::model_ref(&rows, "fixture/local", "high").unwrap(),
            json!({"providerID": "fixture", "id": "local", "variant": "high"})
        );
        assert_eq!(
            catalog::model_ref(&rows, "fixture/local", "").unwrap(),
            json!({"providerID": "fixture", "id": "local"})
        );
        assert!(catalog::model_ref(&rows, "fixture/local", "ultra").is_err());
        assert!(catalog::model_ref(&rows, "fixture/missing", "").is_err());
    }
    #[test]
    fn remove_queued_requires_an_exact_message_id() {
        assert!(queued_id(&json!({"message_id": "msg_gVmstesSuU81H2vTHdmxp65tNI"})).is_ok());
        assert!(queued_id(&json!({"message_id": "../x"})).is_err());
        assert!(queued_id(&json!({})).is_err());
        // The shape the relay and the mobile connector check too.
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/opencode-queued-message-ids.json"
        ))
        .unwrap();
        for (kind, valid) in [("accept", true), ("reject", false)] {
            let ids = fixture[kind].as_array().unwrap();
            assert!(!ids.is_empty(), "{kind}");
            for id in ids {
                assert_eq!(queued_id(&json!({"message_id": id})).is_ok(), valid, "{id}");
                assert_eq!(queued_message_id(id.as_str().unwrap()), valid, "{id}");
            }
        }
    }
    #[test]
    fn interrupt_succeeds_when_acknowledged_or_already_stopped() {
        let ok = seen(vec![(
            "POST /api/session/ses_A/interrupt",
            200,
            json!({"interrupted": true}),
        )]);
        interrupt_with(&ok, &bound()).unwrap();
        let idle = seen(vec![
            ("POST /api/session/ses_A/interrupt", 200, json!({"interrupted": false})),
            ("GET /api/session/active", 200, json!({"data": {}})),
        ]);
        interrupt_with(&idle, &bound()).unwrap();
        let running = seen(vec![
            ("POST /api/session/ses_A/interrupt", 200, json!({"interrupted": false})),
            ("GET /api/session/active", 200, json!({"data": {"ses_A": {}}})),
        ]);
        // Every end after the POST may have stopped the turn (zerus-1wchz).
        let error = interrupt_with(&running, &bound()).unwrap_err();
        assert!(
            error.starts_with("Interrupt delivery uncertain: OpenCode did not confirm"),
            "{error}"
        );
        let unknown = seen(vec![
            ("POST /api/session/ses_A/interrupt", 500, json!({"message": "down"})),
            ("GET /api/session/active", 500, json!({"message": "down"})),
        ]);
        let error = interrupt_with(&unknown, &bound()).unwrap_err();
        assert!(
            error.starts_with("Interrupt delivery uncertain: "),
            "{error}"
        );
        // A reply lost after the POST went out.
        struct Lost;
        impl Native for Lost {
            fn raw(&self, _: &str, _: &str, _: Option<&Value>) -> Result<Response> {
                Err("connection reset by peer".into())
            }
        }
        assert_eq!(
            interrupt_with(&Lost, &bound()).unwrap_err(),
            "Interrupt delivery uncertain: connection reset by peer"
        );
    }
    #[test]
    fn a_queued_message_is_removed_only_while_stopped_and_still_queued() {
        let inbox = json!({"data":[{"id":"msg_A","sessionID":"ses_A","type":"user","payload":{"text":"x"}}]});
        let unknown = json!({"_tag": "MessageNotFoundError"});
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger(dir.path().into());
        let ok = seen(vec![
            ("GET /api/session/ses_A/inbox", 200, inbox.clone()),
            ("GET /api/session/active", 200, json!({"data": {}})),
            ("GET /api/session/ses_A/message/msg_A", 404, unknown.clone()),
            ("DELETE /api/session/ses_A/inbox/msg_A", 204, Value::Null),
            ("GET /api/session/ses_A/inbox", 200, json!({"data": []})),
            ("GET /api/session/ses_A/message/msg_A", 404, unknown.clone()),
        ]);
        let calls = std::cell::Cell::new(0);
        remove_queued_with(&ok, &ledger, &bound(), "msg_A", || {
            // The intent is durable before the DELETE.
            assert_eq!(
                ledger.removal(&bound(), "msg_A").unwrap(),
                Some(Removal::Intended)
            );
            calls.set(calls.get() + 1)
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert!(ok.script.borrow().is_empty());
        assert_eq!(
            ledger.removal(&bound(), "msg_A").unwrap(),
            Some(Removal::Removed)
        );
        // Removal records are per session.
        assert_eq!(
            ledger
                .removal(&json!({"conversation_id": "ses_B"}), "msg_A")
                .unwrap(),
            None
        );
        let gone = seen(vec![(
            "GET /api/session/ses_A/inbox",
            200,
            json!({"data":[]}),
        )]);
        assert_eq!(
            remove_queued_with(&gone, &ledger, &bound(), "msg_A", || panic!("no change"))
                .unwrap_err(),
            NOT_QUEUED
        );
        let active = seen(vec![
            ("GET /api/session/ses_A/inbox", 200, inbox.clone()),
            (
                "GET /api/session/active",
                200,
                json!({"data": {"ses_A": {}}}),
            ),
        ]);
        assert_eq!(
            remove_queued_with(&active, &ledger, &bound(), "msg_A", || panic!("no change"))
                .unwrap_err(),
            NOT_QUEUED
        );
        // History that cannot be read is not "never delivered".
        let unread = seen(vec![
            ("GET /api/session/ses_A/inbox", 200, inbox.clone()),
            ("GET /api/session/active", 200, json!({"data": {}})),
            (
                "GET /api/session/ses_A/message/msg_A",
                500,
                json!({"message": "down"}),
            ),
        ]);
        let fresh = Ledger(dir.path().join("unread"));
        assert_eq!(
            remove_queued_with(&unread, &fresh, &bound(), "msg_A", || panic!("no change"))
                .unwrap_err(),
            "OpenCode could not confirm this message is still queued"
        );
        // It reached the history read, and nothing was recorded or deleted.
        assert!(unread.script.borrow().is_empty());
        assert_eq!(fresh.removal(&bound(), "msg_A").unwrap(), None);
        assert!(!fresh.removals_path().exists());
        // A 204 whose message is still listed is not a confirmed removal.
        let listed = seen(vec![
            ("GET /api/session/ses_A/inbox", 200, inbox.clone()),
            ("GET /api/session/active", 200, json!({"data": {}})),
            ("GET /api/session/ses_A/message/msg_A", 404, unknown.clone()),
            ("DELETE /api/session/ses_A/inbox/msg_A", 204, Value::Null),
            ("GET /api/session/ses_A/inbox", 200, inbox),
        ]);
        let other = Ledger(dir.path().join("other"));
        assert_eq!(
            remove_queued_with(&listed, &other, &bound(), "msg_A", || {}).unwrap_err(),
            REMOVAL_UNCONFIRMED
        );
        assert_eq!(
            other.removal(&bound(), "msg_A").unwrap(),
            Some(Removal::Intended)
        );
    }
    #[test]
    fn a_model_variant_is_sent_and_verified() {
        let model = json!({"providerID":"fixture","id":"local","variant":"high"});
        let ok = seen(vec![
            ("POST /api/session/ses_A/model", 204, Value::Null),
            ("GET /api/session/ses_A", 200, session_with(model.clone(), "t")),
        ]);
        change_model(&ok, &bound(), &model).unwrap();
        assert_eq!(ok.bodies.borrow()[0].1, Some(json!({"model": model})));
        let wrong = seen(vec![
            ("POST /api/session/ses_A/model", 204, Value::Null),
            (
                "GET /api/session/ses_A",
                200,
                session_with(json!({"providerID":"fixture","id":"local","variant":"low"}), "t"),
            ),
        ]);
        assert_eq!(
            change_model(&wrong, &bound(), &model).unwrap_err(),
            "Native OpenCode model change was not confirmed; inspect before retrying"
        );
    }
    #[test]
    fn the_default_effort_sends_no_variant_and_accepts_default() {
        let model = json!({"providerID":"fixture","id":"local"});
        let ok = seen(vec![
            ("POST /api/session/ses_A/model", 204, Value::Null),
            (
                "GET /api/session/ses_A",
                200,
                session_with(json!({"providerID":"fixture","id":"local","variant":"default"}), "t"),
            ),
        ]);
        change_model(&ok, &bound(), &model).unwrap();
        assert_eq!(ok.bodies.borrow()[0].1, Some(json!({"model": model})));
        let other = seen(vec![
            ("POST /api/session/ses_A/model", 204, Value::Null),
            (
                "GET /api/session/ses_A",
                200,
                session_with(json!({"providerID":"fixture","id":"other"}), "t"),
            ),
        ]);
        assert!(change_model(&other, &bound(), &model).is_err());
    }
    #[test]
    fn rename_sends_only_the_title_and_verifies_it() {
        let ok = seen(vec![
            ("PATCH /api/session/ses_A", 204, Value::Null),
            ("GET /api/session/ses_A", 200, session_with(json!({}), "new")),
        ]);
        change_title(&ok, &bound(), "new").unwrap();
        assert_eq!(ok.bodies.borrow()[0].1, Some(json!({"title": "new"})));
        let stale = seen(vec![
            ("PATCH /api/session/ses_A", 204, Value::Null),
            ("GET /api/session/ses_A", 200, session_with(json!({}), "old")),
        ]);
        assert!(change_title(&stale, &bound(), "new").is_err());
    }
    /// Stands in for the cached home short list and echoes the folder asked for.
    fn cached_stub(_: &service::Unavailable, location: &Path, _: &[&str]) -> Value {
        json!({"model_options": [{"value": "cache/stub", "location": location}],
            "model_options_truncated": false})
    }
    fn inspector(
        service: std::result::Result<service::Service, service::Unavailable>,
        migration: Option<Reply>,
        active: Option<Result<Value>>,
    ) -> Inspector {
        Inspector {
            service,
            computer: Some("build-01".into()),
            active,
            migration,
            verdicts: Some(Vec::new()),
            verdicts_changed: false,
            cached_options: cached_stub,
        }
    }
    /// A registered service that refuses every connection before a byte is sent.
    fn refusing() -> service::Service {
        service::Service {
            reg: service::Registration {
                id: "a".into(),
                version: "2.0.32".into(),
                port: 0,
                pid: 5,
                password: None,
            },
        }
    }
    /// Clients take full details without `model_options_truncated` for an
    /// older hgs, so every unavailable answer keeps the short-list keys.
    #[test]
    fn every_unavailable_session_detail_keeps_the_short_list_keys() {
        let legacy = json!({"backend": "opencode", "name": "opencode/workspace/a",
            "conversation_id": "ses_A", "cwd": "/home/example/project"});
        let mut current = legacy.clone();
        current["service"] = json!("opencode2");
        let cases = [
            (
                "service stopped",
                inspector(Err(service::Unavailable::NotRunning), None, None),
                &current,
            ),
            (
                "migration check failed",
                inspector(Ok(refusing()), Some(Err("refused".into())), None),
                &legacy,
            ),
            (
                "activity read failed",
                inspector(Ok(refusing()), None, Some(Err("OpenCode HTTP 500".into()))),
                &current,
            ),
            (
                "session read failed",
                inspector(Ok(refusing()), None, Some(Ok(json!({})))),
                &current,
            ),
        ];
        let cached = json!([{"value": "cache/stub", "location": "/home/example/project"}]);
        let mut wrong = Vec::new();
        for (case, mut inspector, record) in cases {
            let full = inspector.view(record, Detail::Full);
            // Actions and listings never carry a model list.
            let own = inspector.view(record, Detail::Own);
            if full["inspection_unavailable"] != true
                || full["model_options"] != cached
                || full["model_options_truncated"] != false
                || own["inspection_unavailable"] != true
                || own.get("model_options_truncated").is_some()
            {
                wrong.push(case);
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }
    #[test]
    fn pause_and_resume_explain_that_there_is_no_process() {
        let mut record = bound();
        for kind in ["pause", "resume"] {
            assert_eq!(
                action(&mut record, kind, None, None, None, None).unwrap_err(),
                NO_PROCESS
            );
        }
    }
}
