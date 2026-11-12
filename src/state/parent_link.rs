//! Parent links between independent sessions (zerus-sa9qn.2, spec
//! `docs/superpowers/specs/2026-11-07-account-labels-child-sessions-design.md`).
//!
//! A child's record carries `parent`: the machine and durable session ID of
//! the live tracked agent that launched or adopted it, proven by native process
//! ancestry exactly as `hgs self` proves its caller. The child's computer is
//! the link's only writer. The link is presentation and attention routing
//! only: it never authorizes a send, answer, approval, stop or removal, and it
//! never moves a session's project. Only a new child's initial placement
//! follows a parent this computer proved (zerus-d5twt).
use super::*;
use std::{fs, io::Read};

/// The launching hgs's proven caller for a new session, as compact JSON. The
/// launcher always sets it, empty when no live tracked agent owns it, so an
/// inherited value never reaches the record.
pub(crate) const ENV: &str = "HGS_PARENT";
const USAGE: &str = "usage: hgs parent <session> --json   (JSON stdin: expected_run_id, expected_conversation_id, action read|adopt|set|clear, optional replace; set or claimed adopt names parent {session_id, name, conversation_id, run_id})";
/// The longest parent chain walked when refusing a cycle.
const DEPTH: usize = 8;
/// The source of a link another computer forwarded with a launch: a claim
/// this computer cannot verify, never a live parent (zerus-l4ehe).
const FORWARDED_SOURCE: &str = "forwarded";

fn text(value: &Value, limit: usize, required: bool) -> bool {
    value.as_str().is_some_and(|s| {
        (!required || !s.is_empty()) && s.len() <= limit && !s.chars().any(char::is_control)
    })
}

/// A well-formed link: schema 1, a canonical machine UUID, a session ID or a
/// conversation or run to match, a session label and a known source.
pub(crate) fn valid(link: &Value) -> bool {
    let Some(object) = link.as_object() else {
        return false;
    };
    let canonical_machine = object
        .get("machine")
        .and_then(Value::as_str)
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|u| !u.is_nil() && u.hyphenated().to_string() == id));
    let session = object.get("session_id").and_then(Value::as_str).unwrap_or("");
    object.len() <= 12
        && object.get("schema") == Some(&json!(1))
        && canonical_machine
        && (session.is_empty() || session_id::canonical(session))
        && text(&object.get("host").cloned().unwrap_or(Value::Null), 80, true)
        && text(&object.get("name").cloned().unwrap_or(Value::Null), 512, true)
        && ["conversation_id", "run_id"]
            .iter()
            .all(|key| text(object.get(*key).unwrap_or(&json!("")), 512, false))
        && (!session.is_empty()
            || !string(link, "conversation_id").is_empty()
            || !string(link, "run_id").is_empty())
        && matches!(string(link, "source"), "launch" | "adopt" | "manual" | "claimed" | FORWARDED_SOURCE)
        && object.get("at").is_some_and(Value::is_i64)
}

/// The link naming `record` (a live tracked session on this computer).
fn link_of(record: &Value, machine: &str, host: &str, source: &str) -> Option<Value> {
    let link = json!({"schema": 1, "machine": machine, "host": host,
        "session_id": string(record, "session_id"), "name": string(record, "name"),
        "conversation_id": string(record, "conversation_id"), "run_id": string(record, "run_id"),
        "source": source, "at": now() as i64});
    valid(&link).then_some(link)
}

/// The largest shared catalog read to observe this computer's identity, as
/// the session action receipts bound it.
const CATALOG_BYTES: u64 = 128 * 1024 * 1024;

/// This computer's machine UUID from its existing shared catalog, read
/// without the catalog lock and never initialized or saved: an optional link
/// must never wait for, or change, Swarm state (zerus-hycp0).
fn observed_machine(config: &crate::config::Config) -> Option<String> {
    let path = config.dir.join("swarm/catalog.json");
    // A regular file only: never a final link, never waiting on a FIFO.
    let file = crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(&path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > CATALOG_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(CATALOG_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > CATALOG_BYTES {
        return None;
    }
    crate::swarm::existing_node_id(&bytes).ok()
}

/// The proven local caller's link, for a session this process launches.
fn proven_link() -> Option<String> {
    let record = session_graph::caller_record()?;
    let config = crate::config::Config::load().ok()?;
    let machine = observed_machine(&config)?;
    link_of(&record, &machine, config.host(), "launch").map(|link| link.to_string())
}

/// A link another computer's hgs forwarded with this launch (zerus-sa9qn.7).
static FORWARDED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// `HGS_PARENT`'s value for a session this process launches: an explicit live
/// claim first, otherwise the proven local caller; else a forwarded link.
/// Empty keeps an inherited value from the record. Claims never prove a caller.
pub(crate) fn launcher_value() -> Result<String> {
    if let Some(choice) = CLAIMED.get() {
        let parent = claimed_parent(choice)?;
        ancestors(&parent)?;
        let config = crate::config::Config::load().map_err(|e| e.message)?;
        let machine = observed_machine(&config).ok_or("This computer's identity cannot be read yet; retry later")?;
        return link_of(&parent, &machine, config.host(), "claimed")
            .map(|link| link.to_string()).ok_or("The parent identity is incomplete".into());
    }
    Ok(proven_link().or_else(|| FORWARDED.get().cloned()).unwrap_or_default())
}

/// The launcher of a new native-API session. Resolve the link directly:
/// these sessions have no new terminal in which to replace inherited ENV.
pub(super) fn launcher_link() -> Result<Option<Value>> {
    Ok(serde_json::from_str::<Value>(&launcher_value()?).ok().filter(valid))
}

static CLAIMED: std::sync::OnceLock<Choice> = std::sync::OnceLock::new();
pub(crate) fn claimed_launch() -> bool { CLAIMED.get().is_some() }

/// An explicit, local presentation link, never process proof or permission.
pub(crate) fn accept_claimed(value: &str) -> Result<()> {
    if value.len() > 4096 { return Err("--parent identity exceeds limits".into()); }
    let choice: Choice = serde_json::from_str(value).map_err(|_| "--parent needs JSON with session_id, run_id and conversation_id".to_owned())?;
    claimed_parent(&choice)?;
    CLAIMED.set(choice).map_err(|_| "--parent may be given once".to_owned())
}

/// `--parent-link HEX` for a launch on a peer: the proven caller's link, hex
/// encoded so no shell or SSH quoting can change it. None without a caller.
pub(crate) fn forwarded_argument() -> Option<String> {
    proven_link().map(|link| link.bytes().map(|b| format!("{b:02x}")).collect())
}

/// Accepts a link the launching computer forwarded (`--parent-link HEX`): a
/// well-formed `launch` link naming another computer. It is a claim by the
/// SSH-authenticated caller that this computer cannot verify, so it is
/// recorded with the `forwarded` source: the listing never shows such a
/// parent as live, even when its IDs match a session here, and it never
/// quiets the child (zerus-l4ehe).
pub(crate) fn accept_forwarded(hex: &str) -> std::result::Result<(), String> {
    let mut link = forwarded_link(hex)?;
    let local = crate::config::Config::load().ok().and_then(|config| observed_machine(&config));
    if local.as_deref() == Some(string(&link, "machine")) {
        return Err("A link for a parent on this computer comes from this computer's own proof".into());
    }
    link["source"] = json!(FORWARDED_SOURCE);
    FORWARDED.set(link.to_string()).map_err(|_| "--parent-link may be given once".to_owned())
}

/// The well-formed `launch` link `--parent-link` carries, hex-encoded JSON.
fn forwarded_link(hex: &str) -> std::result::Result<Value, String> {
    let refuse = || "--parent-link needs a launch link from another computer".to_owned();
    // ASCII hex digits only, checked before any slicing (zerus-so6hr).
    if hex.is_empty() || hex.len() > 8192 || !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(refuse());
    }
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(refuse)?;
    let link: Value = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
    if !valid(&link) || string(&link, "source") != "launch" {
        return Err(refuse());
    }
    Ok(link)
}

/// The link the launching hgs passed, for a new session's first record.
pub(super) fn from_env() -> Option<Value> {
    nonempty_env(ENV)
        .filter(|value| value.len() <= 4096)
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .filter(valid)
}

/// The record's link for listings, when well formed.
pub(super) fn listed(record: &Value) -> Option<Value> {
    record.get("parent").filter(|link| valid(link)).cloned()
}

/// The live state of each listed row's parent, from the same listing:
/// `live` (running, its agent not exited), `saved` (paused, stopped or
/// exited), `archived`, or `missing` when this listing has no such session
/// (gone, or on another computer). Session IDs are UUIDs, so no machine
/// identity is read: a listing never waits on Swarm state (zerus-hycp0). A
/// link another computer forwarded is unverified: always `missing`, whatever
/// sessions here its IDs match (zerus-l4ehe).
pub(crate) fn decorate(sessions: &mut [Value]) {
    let state = |row: &Value| -> &'static str {
        match row["state"].as_str().unwrap_or("running") {
            "archived" => "archived",
            "running" if row["process_state"] != "exited" => "live",
            _ => "saved",
        }
    };
    // A live or saved row wins over an archived copy of the same session.
    let mut by_session: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut by_conversation: BTreeMap<String, &'static str> = BTreeMap::new();
    for row in sessions.iter() {
        let found = state(row);
        for (map, key) in [(&mut by_session, "session_id"), (&mut by_conversation, "conversation_id")] {
            let id = string(row, key);
            if key == "conversation_id" && !string(row, "session_id").is_empty() {
                continue;
            }
            if !id.is_empty() && map.get(id).is_none_or(|old| *old == "archived") {
                map.insert(id.to_owned(), found);
            }
        }
    }
    for row in sessions.iter_mut() {
        let Some(link) = row.get("parent").filter(|link| link.is_object()) else {
            continue;
        };
        let found = if string(link, "source") == FORWARDED_SOURCE {
            "missing"
        } else {
            by_session
                .get(string(link, "session_id"))
                .or_else(|| by_conversation.get(string(link, "conversation_id")))
                .copied()
                .unwrap_or("missing")
        };
        row["parent"]["state"] = json!(found);
    }
}

/// Native questions only the person may answer: folder and hook trust.
const PERSON_ONLY_QUESTIONS: [&str; 4] = ["claude-trust:", "codex-trust:", "kimi-trust:", "codex-hooks-trust:"];

/// Whether a live row holds a request only the person can answer (spec 2.2):
/// a tool or permission approval, folder or hook trust, or signing in. A
/// supervised child's other requests are its parent's to answer.
pub(super) fn human_request(session: &Value, questions: Option<&[Value]>) -> bool {
    // A native subagent's tool approval is the person's too (zerus-dn4ia).
    let approval = |child: &Value| {
        ["display_state", "state"].iter().any(|key| child[*key] == "approval")
    };
    string(session, "phase") == "approval"
        || session["subagents"].as_object().is_some_and(|children| children.values().any(approval))
        || session["subagent_previews"].as_array().is_some_and(|children| children.iter().any(approval))
        || session["provider_error"]["error_kind"] == "authentication"
        || string(session, "activity_summary") == "Sign in required"
        || questions.is_some_and(|questions| {
            questions.iter().any(|question| {
                let id = string(question, "question_id");
                PERSON_ONLY_QUESTIONS.iter().any(|prefix| id.starts_with(prefix))
            })
        })
}

/// Sets a supervised child row's `human_request` for every listing path:
/// generic records, DeepSeek and OpenCode (zerus-lgr9s). Without it a client
/// cannot tell a child's unread reply, which is its parent's, from a request
/// for the person. DeepSeek and OpenCode list a pending tool approval as a
/// question in the `input` phase; `approval` stands in for the `approval`
/// phase other agents report.
pub(super) fn mark_human_request(session: &mut Value, questions: Option<&[Value]>, approval: bool) {
    if session.get("parent").is_some() {
        session["human_request"] = json!(approval || human_request(session, questions));
    }
}

/// The session `id` without an index entry: its own ID layout record, or the
/// record at the label a link names, each read bounded, regular, no-follow
/// and nonblocking, and only when it carries that ID.
fn unindexed(id: &str, name: &str) -> Result<Option<Value>> {
    // The index is a cache. Try each fixed ID location, then the label hint;
    // an unreadable existing file is never evidence that the chain ended.
    let present = |path: &Path| match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("A session cannot be read safely; retry later".to_owned()),
    };
    if let Some(path) = storage::id_record_path(id) {
        if present(&path)? {
            return storage::read_id(id).map(Some);
        }
    }
    for provider in [provider_binding::Provider::DeepSeek, provider_binding::Provider::OpenCode] {
        if let Some(path) = provider.id_path(id) {
            if present(&path)? {
                return provider.read_id(id).map(Some);
            }
        }
    }
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(provider) = provider_binding::Provider::named(name) {
        if present(&provider.label_path(name))? {
            return provider.read_label(name).map(Some);
        }
    } else if present(&storage::label_record_path(name))? {
        return storage::read_label_bounded(name, storage::ID_RECORD_BYTES).map(Some);
    }
    Ok(None)
}

/// The parent a person chose on the desktop: a session on this computer, by
/// its durable ID, with the name, conversation and run the desktop listed.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    session_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    conversation_id: String,
    #[serde(default)]
    run_id: String,
}

fn exact_choice(choice: &Choice) -> bool {
    session_id::canonical(&choice.session_id)
        && uuid::Uuid::parse_str(&choice.session_id).is_ok_and(|id| !id.is_nil())
        && [&choice.run_id, &choice.conversation_id].iter().all(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        && choice.name.len() <= 512 && !choice.name.chars().any(char::is_control)
}

/// Legacy label records can predate the SID index. Without a name hint,
/// locate one exact SID through bounded metadata only, never full histories.
fn claim_by_id(id: &str) -> Result<Option<Value>> {
    use std::time::{Duration, Instant};
    let until = Instant::now() + Duration::from_secs(2);
    let mut count = 0;
    let mut bytes = 0;
    let mut found = None;
    for namespace in storage::BINDING_DIRS {
        let entries = match fs::read_dir(root().join(namespace)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries {
            count += 1;
            if count > 5000 || Instant::now() >= until { return Err("Parent lookup exceeded its metadata bound; include a name hint".into()); }
            let path = entry.map_err(|e| e.to_string())?.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            if path.extension().is_none_or(|v| v != "json") || !storage::binding_stem(namespace, stem) { continue; }
            let raw = labels::read_file(&path, storage::ID_RECORD_BYTES).map_err(|e| e.to_string())?.ok_or("A parent binding disappeared")?;
            bytes += raw.len();
            if bytes > 64 << 20 { return Err("Parent lookup exceeded its byte bound; include a name hint".into()); }
            let record: Value = serde_json::from_slice(&raw).map_err(|_| "A parent binding cannot be read safely".to_owned())?;
            if string(&record, "session_id") != id || !string(&record, "archive_id").is_empty() || record["rename_shadow"] == true { continue; }
            if found.is_some() { return Err("The parent session ID is ambiguous".into()); }
            found = Some(string(&record, "name").to_owned());
        }
    }
    if Instant::now() >= until { return Err("Parent lookup exceeded its metadata bound; include a name hint".into()); }
    match found { Some(name) => read_session(id, &name), None => Ok(None) }
}

/// Claims name a complete identity and must freshly match a live local row.
fn claimed_parent(choice: &Choice) -> Result<Value> {
    if !exact_choice(choice) { return Err("A claimed parent needs its exact session_id, run_id and conversation_id".into()); }
    let record = match read_session(&choice.session_id, &choice.name)? {
        Some(record) => Some(record),
        None if choice.name.is_empty() => claim_by_id(&choice.session_id)?,
        None => None,
    }.ok_or("The claimed parent is not a session on this computer")?;
    if string(&record, "run_id") != choice.run_id || string(&record, "conversation_id") != choice.conversation_id {
        return Err("The claimed parent changed; refresh and retry".into());
    }
    require_live(&record)?;
    let current = read_session(&choice.session_id, string(&record, "name"))?.ok_or("The claimed parent disappeared")?;
    if string(&current, "run_id") != choice.run_id || string(&current, "conversation_id") != choice.conversation_id {
        return Err("The claimed parent changed; refresh and retry".into());
    }
    if !string(&current, "archive_id").is_empty() || current["state"] == "archived" || current["rename_shadow"] == true {
        return Err("A claimed parent must be a live session on this computer".into());
    }
    Ok(current)
}

/// How long a claim check waits for a native host (zerus-p24ce): hgs self,
/// adopt and --parent run in an agent's tool shell and must not stall there.
pub(super) const CLAIM_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// A host that did not answer the claim check in time. Never treated as live.
pub(super) const CLAIM_BUSY: &str = "The native host did not confirm the claimed parent within 2 seconds; it may be busy. Retry when it answers";

fn require_live(record: &Value) -> Result<()> {
    if !string(record, "archive_id").is_empty()
        || record["state"] == "archived"
        || record["rename_shadow"] == true
    {
        return Err("A claimed parent must be a live session on this computer".into());
    }
    let live = match string(record, "backend") {
        "dsh" => dsh::claim_live(record, CLAIM_WAIT)?,
        "opencode" => opencode::claim_live(record, CLAIM_WAIT)?,
        _ => process_alive(record) && terminal_live(&live_sessions()?.0, record)?,
    };
    if !live {
        return Err("A claimed parent must be a live session on this computer".into());
    }
    Ok(())
}

/// Native shell environment is a claim, never the process proof used by sends,
/// graph attribution or permissions. Native hosts already expose these IDs.
pub(crate) fn native_self() -> Result<Option<Value>> {
    use provider_binding::Provider;
    let (provider, native, native_home) = match (nonempty_env("DSH_SESSION_ID"), nonempty_env("OPENCODE_SESSION_ID")) {
        (None, None) => return Ok(None),
        (Some(id), None) => (Provider::DeepSeek, id, nonempty_env("DSH_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".dsh"))),
        (None, Some(id)) => (Provider::OpenCode, id, opencode::config_home()),
        _ => return Err("Multiple native session identities are present; clear the inherited identity before using hgs self".into()),
    };
    let record = provider.native_caller(&native, &native_home)?.ok_or("The native conversation is not a tracked Zerus session on this computer")?;
    let choice = Choice { session_id: string(&record, "session_id").into(), name: string(&record, "name").into(),
        run_id: string(&record, "run_id").into(), conversation_id: string(&record, "conversation_id").into() };
    claimed_parent(&choice).map(Some)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    expected_run_id: String,
    expected_conversation_id: String,
    action: String,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    parent: Option<Choice>,
}

/// The session `id` on this computer: through its index entry, else its ID
/// layout record or the label `name` hints, each read bounded, regular,
/// no-follow and nonblocking. None when this computer has no such session;
/// an indexed record that cannot be read safely is an error.
pub(super) fn read_session(id: &str, name: &str) -> Result<Option<Value>> {
    let unsafe_read = |_| "A session cannot be read safely; retry later".to_owned();
    let found = match session_index::indexed(id) {
        Some((label, session_index::Kind::Record)) => {
            Some(storage::read_label_bounded(&label, storage::ID_RECORD_BYTES).map_err(unsafe_read)?)
        }
        Some((_, session_index::Kind::IdRecord)) => Some(storage::read_id(id).map_err(unsafe_read)?),
        Some((name, session_index::Kind::DeepSeek)) => Some(provider_binding::Provider::DeepSeek.read_label(&name).map_err(unsafe_read)?),
        Some((_, session_index::Kind::DeepSeekId)) => Some(dsh::read_id(id).map_err(unsafe_read)?),
        Some((name, session_index::Kind::OpenCode)) => Some(provider_binding::Provider::OpenCode.read_label(&name).map_err(unsafe_read)?),
        Some((_, session_index::Kind::OpenCodeId)) => Some(opencode::read_id(id).map_err(unsafe_read)?),
        None => unindexed(id, name).map_err(unsafe_read)?,
    };
    match found {
        Some(record) if string(&record, "session_id") != id => Err("A session changed while it was read; retry".into()),
        found => Ok(found),
    }
}

/// `hgs parent <session> --json`: read, adopt (set the link to the proven
/// caller) or clear one tracked session's parent link.
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let [name, flag] = args else {
        return Err(USAGE.into());
    };
    if flag != "--json" || name.starts_with(['-', '@']) {
        return Err(USAGE.into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 8192 {
        return Err("parent request exceeds limits".into());
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| USAGE.to_owned())?;
    if !matches!(request.action.as_str(), "read" | "adopt" | "set" | "clear")
        || (request.action == "set" && request.parent.is_none())
        || (!matches!(request.action.as_str(), "set" | "adopt") && request.parent.is_some())
    {
        return Err(USAGE.into());
    }
    expect(name, &request.expected_run_id, &request.expected_conversation_id)?;
    // Prove the caller before taking the writer lock: its own record is read.
    let caller = if request.action == "adopt" && request.parent.is_none() {
        Some(session_graph::caller_record().ok_or("hgs parent adopt: no live Zerus session owns this process")?)
    } else {
        None
    };
    let config = crate::config::Config::load().map_err(|e| e.message)?;
    let machine = if matches!(request.action.as_str(), "adopt" | "set") {
        observed_machine(&config).ok_or("This computer's identity cannot be read yet; retry later")?
    } else {
        String::new()
    };
    let _writer = lock(None)?;
    // Parent writers share the state lock for cycle checks, then the native
    // provider's own lock so a metadata change cannot overwrite its updates.
    let provider = provider_binding::Provider::named(name);
    let _dsh = if matches!(provider, Some(provider_binding::Provider::DeepSeek)) {
        Some(lock(Some(&root().join("dsh/bindings.lock")))?)
    } else { None };
    let _opencode = if matches!(provider, Some(provider_binding::Provider::OpenCode)) {
        Some(opencode::lock_bindings()?)
    } else { None };
    let mut record = match provider {
        Some(provider) => provider.read(name)?,
        None => read_bounded(name, storage::ID_RECORD_BYTES)?,
    };
    let save = |record: &mut Value| match provider {
        Some(provider) => provider.save(record),
        None => write(record),
    };
    if string(&record, "run_id") != request.expected_run_id
        || string(&record, "conversation_id") != request.expected_conversation_id
    {
        return Err(format!("{name} changed; refresh and retry"));
    }
    if !string(&record, "archive_id").is_empty() || record["state"] == "archived" {
        return Err("Archived sessions keep their parent link; restore the session first".into());
    }
    let previous = listed(&record);
    match request.action.as_str() {
        "read" => {}
        "clear" => {
            if record.as_object_mut().and_then(|o| o.remove("parent")).is_some() {
                save(&mut record)?;
            }
        }
        _ => {
            // Adopt uses process proof by default or an explicit live claim;
            // set is the session a person chose.
            let (chosen, source) = match (caller, request.parent.as_ref()) {
                (Some(caller), _) => (caller, "adopt"),
                (None, Some(choice)) if request.action == "adopt" => (claimed_parent(choice)?, "claimed"),
                (None, Some(choice)) => {
                    if !session_id::canonical(&choice.session_id) {
                        return Err("Choose the parent by its session ID".into());
                    }
                    let parent = read_session(&choice.session_id, &choice.name)?
                        .ok_or("The chosen parent is not a session on this computer")?;
                    if [(&choice.conversation_id, "conversation_id"), (&choice.run_id, "run_id")]
                        .iter()
                        .any(|(expected, key)| !expected.is_empty() && string(&parent, key) != expected.as_str())
                    {
                        return Err("The chosen parent changed; refresh and retry".into());
                    }
                    if !string(&parent, "archive_id").is_empty() || parent["state"] == "archived" {
                        return Err("An archived session cannot be a parent".into());
                    }
                    (parent, "manual")
                }
                _ => return Err(USAGE.into()),
            };
            let target = string(&record, "session_id");
            if string(&chosen, "session_id") == target && !target.is_empty()
                || string(&chosen, "run_id") == string(&record, "run_id")
            {
                return Err(if source == "adopt" { "A session cannot adopt itself" } else { "A session cannot be its own parent" }.into());
            }
            if ancestors(&chosen)?.contains(&target.to_owned()) {
                return Err("This session already leads the chosen parent; the link would make a cycle".into());
            }
            if let Some(old) = previous.as_ref() {
                let same = string(old, "machine") == machine
                    && string(old, "session_id") == string(&chosen, "session_id");
                if !same && !request.replace {
                    return Err(format!(
                        "{name} already has the parent {}; pass \"replace\": true to change it",
                        string(old, "name")
                    ));
                }
            }
            let link = link_of(&chosen, &machine, config.host(), source)
                .ok_or("The parent session has no complete identity yet; retry after it starts")?;
            record["parent"] = link;
            save(&mut record)?;
        }
    }
    println!("{}", json!({"ok": true, "name": name, "parent": listed(&record), "previous": previous}));
    Ok(0)
}

/// The session IDs of `record`'s parents on this computer, nearest first,
/// through the session ID index. Each record is read bounded, a regular file
/// only, never following a final link or waiting on a FIFO (zerus-mzgfh). A
/// chain that ends at a session this computer no longer lists, or on another
/// computer, is complete. A chain longer than [`DEPTH`], or an indexed record
/// that cannot be read, proves nothing, so adoption refuses (zerus-lwz9t).
fn ancestors(record: &Value) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut current = record.clone();
    loop {
        // A forwarded parent is on another computer: the chain here ends.
        let Some(link) = listed(&current).filter(|link| string(link, "source") != FORWARDED_SOURCE) else {
            return Ok(found);
        };
        let id = string(&link, "session_id").to_owned();
        if id.is_empty() || found.contains(&id) {
            return Ok(found);
        }
        if found.len() == DEPTH {
            return Err(format!("The caller's parents reach deeper than {DEPTH} sessions; adoption cannot rule out a cycle"));
        }
        found.push(id.clone());
        let next = read_session(&id, string(&link, "name"))
            .map_err(|_| "A parent of the caller cannot be read safely; adoption cannot rule out a cycle".to_owned())?;
        let Some(next) = next else { return Ok(found); };
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> Value {
        json!({"schema":1,"machine":"11111111-1111-4111-8111-111111111111","host":"arch",
            "session_id":"22222222-2222-4222-8222-222222222222","name":"claude/p/lead",
            "conversation_id":"c","run_id":"r","source":"launch","at":1})
    }

    #[test]
    fn native_conversation_ids_are_opaque_link_identity() {
        for (agent, conversation) in [("dsh", "session-12345678-1234-4234-8234-123456789abc"), ("opencode", "ses_native")] {
            let record = json!({"name":format!("{agent}/example/native"),"session_id":"33333333-3333-4333-8333-333333333333",
                "run_id":"run-native","conversation_id":conversation});
            let linked = link_of(&record, "11111111-1111-4111-8111-111111111111", "fixture", "manual").unwrap();
            assert_eq!(linked["conversation_id"], conversation);
            assert!(valid(&linked));
        }
    }

    #[test]
    fn native_parent_routes_and_chains_use_private_state() {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "state::parent_link::tests::native_parent_fixture", "--ignored", "--test-threads=1"])
            .env("HGS_STATE_DIR", state.path())
            .env("HGS_CONFIG_DIR", state.path().join("config"))
            .env("ZERUS_PARENT_FIXTURE", "1")
            .env_remove("HGS_SESSION_ID").env_remove("HGS_SESSION").env_remove("HGS_RUN_ID")
            .output().unwrap();
        assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn native_parent_fixture() {
        if std::env::var("ZERUS_PARENT_FIXTURE").as_deref() != Ok("1") { return; }
        let mut prior: Option<Value> = None;
        for (provider, agent, conversation) in [
            (provider_binding::Provider::DeepSeek, "dsh", "session-native"),
            (provider_binding::Provider::OpenCode, "opencode", "ses_native"),
        ] {
            for by_id in [false, true] {
                let id = uuid::Uuid::new_v4().to_string();
                let name = format!("{agent}/example/{by_id}");
                let mut record = json!({"backend":agent,"agent":agent,"name":name,"session_id":id,
                    "run_id":"run-native","conversation_id":format!("{conversation}-{by_id}"),"agent_home":root().join(agent)});
                if let Some(parent) = prior.as_ref() { record["parent"] = parent.clone(); }
                let path = if by_id {
                    record["layout"] = json!("id");
                    provider.id_path(&id).unwrap()
                } else { provider.label_path(&name) };
                atomic(&path, &record.to_string()).unwrap();
                let native = string(&record, "conversation_id");
                let home = PathBuf::from(string(&record, "agent_home"));
                assert_eq!(provider.native_caller(native, &home).unwrap(), Some(record.clone()));
                // A cache miss reads fixed ID locations or the link's exact label.
                assert_eq!(read_session(&id, &name).unwrap(), Some(record.clone()));
                let before = ancestors(&record).unwrap();
                session_index::note(&record, provider.kind(&record));
                assert_eq!(read_session(&id, &name).unwrap(), Some(record.clone()));
                assert_eq!(ancestors(&record).unwrap(), before);
                prior = link_of(&record, "11111111-1111-4111-8111-111111111111", "fixture", "manual");
                let child = json!({"parent":prior});
                assert!(ancestors(&child).unwrap().contains(&id));
                // Unsafe indexed bindings never shorten a chain into success.
                fs::write(&path, "{broken").unwrap();
                assert!(ancestors(&child).is_err());
                fs::remove_file(root().join("ids").join(format!("{id}.json"))).unwrap();
                assert!(ancestors(&child).is_err());
                atomic(&path, &record.to_string()).unwrap();
                let mut duplicate = record.clone();
                duplicate["session_id"] = json!(uuid::Uuid::new_v4().to_string());
                duplicate["name"] = json!(format!("{agent}/example/duplicate-{by_id}"));
                let duplicate_path = if by_id { provider.id_path(string(&duplicate, "session_id")).unwrap() }
                    else { provider.label_path(string(&duplicate, "name")) };
                atomic(&duplicate_path, &duplicate.to_string()).unwrap();
                // Writers note all holders of a native conversation. A warm
                // cache must not turn duplicate identities into one caller.
                provider.save(&mut duplicate).unwrap();
                assert!(provider.native_caller(native, &home).is_err());
                // A partial writer note cannot hide another unindexed legacy
                // holder of the same native identity after a cache loss.
                fs::remove_dir_all(root().join("native-identities")).unwrap();
                provider.save(&mut duplicate).unwrap();
                assert!(provider.native_caller(native, &home).is_err());
                fs::remove_file(duplicate_path).unwrap();
            }
        }
        // Typed or inherited HGS_PARENT never establishes a native launcher.
        // This fixture runs alone in its own process.
        std::env::set_var(ENV, link().to_string());
        assert!(launcher_link().unwrap().is_none());
    }

    fn isolated(fixture: &str) {
        let state = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("state::parent_link::tests::{fixture}"),
                "--ignored",
                "--test-threads=1",
            ])
            .env("HGS_STATE_DIR", state.path())
            .env("HGS_CONFIG_DIR", state.path().join("config"))
            .env("XDG_STATE_HOME", state.path().join("xdg"))
            .env("ZERUS_CLAIMED_PARENT_FIXTURE", "1")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_SESSION")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn claim_record(name: &str, conversation: &str, home: &Path) -> Value {
        json!({"backend":"dsh","agent":"dsh","name":name,"session_id":uuid::Uuid::new_v4().to_string(),
            "run_id":"run-claim","conversation_id":conversation,"agent_home":home})
    }

    #[test]
    fn a_warm_native_identity_index_sees_bindings_written_outside_save() {
        isolated("warm_index_fixture");
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn warm_index_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-zww71: a warm native-identity index stays warm through hgs's
        // own writes but never hides a binding written outside Provider::save.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("cache-home");
        let native = "session-cache";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        let ledger = || -> Value {
            serde_json::from_slice(
                &fs::read(root().join("native-identities/dsh-writes.json")).unwrap(),
            )
            .unwrap()
        };
        let mut other = claim_record("dsh/example/other", "session-other", &home);
        {
            // While the provider lock is held a cold scan is refused as busy,
            // so these lookups prove the warm index answered.
            let _held = lock(Some(&root().join("dsh").join("bindings.lock"))).unwrap();
            let accounted = ledger();
            provider.save(&mut other).unwrap();
            // hgs accounts for its own write: nothing is left unaccounted.
            assert_ne!(ledger()["stamp"], accounted["stamp"]);
            assert_eq!(
                (ledger()["epoch"].clone(), ledger()["dirty_since"].clone()),
                (accounted["epoch"].clone(), Value::Null)
            );
            assert_eq!(
                provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
                first["session_id"]
            );
        }
        // An older hgs or a guarded rename writes a binding without noting it.
        let foreign = claim_record("dsh/example/foreign", native, &home);
        atomic(
            &provider.label_path("dsh/example/foreign"),
            &foreign.to_string(),
        )
        .unwrap();
        assert!(
            provider.native_caller(native, &home).is_err(),
            "a foreign duplicate is ambiguous, not the cached session"
        );
        // A later write of hgs's own does not account for the foreign one.
        provider.save(&mut other).unwrap();
        assert!(
            ledger()["dirty_since"].is_u64(),
            "the foreign write stays unaccounted"
        );
        assert!(
            provider.native_caller(native, &home).is_err(),
            "still ambiguous after an own write"
        );
        fs::remove_file(provider.label_path("dsh/example/foreign")).unwrap();
        // An unreadable unrelated binding holds no session: the warm index
        // still answers, without a scan (the provider lock is held).
        fs::write(provider.label_path("dsh/example/broken"), "{broken").unwrap();
        let _held = lock(Some(&root().join("dsh").join("bindings.lock"))).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
    }

    /// A listener that never accepts, with a queue of `backlog` entries (zerus-taljw).
    #[cfg(unix)]
    fn full_unix_listener(path: &Path, backlog: libc::c_int) -> std::os::fd::OwnedFd {
        use std::os::fd::FromRawFd;
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        assert!(
            bytes.len() < address.sun_path.len(),
            "test socket path too long"
        );
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                address.sun_path.as_mut_ptr() as *mut u8,
                bytes.len(),
            )
        };
        #[cfg(target_os = "macos")]
        {
            address.sun_len = std::mem::size_of::<libc::sockaddr_un>() as u8;
        }
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0, "socket: {}", io::Error::last_os_error());
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let bound = unsafe {
            libc::bind(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                &address as *const _ as *const libc::sockaddr,
                length,
            )
        };
        assert_eq!(bound, 0, "bind: {}", io::Error::last_os_error());
        let listening = unsafe { libc::listen(std::os::fd::AsRawFd::as_raw_fd(&fd), backlog) };
        assert_eq!(listening, 0, "listen: {}", io::Error::last_os_error());
        fd
    }

    /// Connections queued until a nonblocking connect finds the queue full.
    #[cfg(unix)]
    fn fill_unix_queue(path: &Path) -> Vec<std::os::unix::net::UnixStream> {
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                address.sun_path.as_mut_ptr() as *mut u8,
                bytes.len(),
            )
        };
        #[cfg(target_os = "macos")]
        {
            address.sun_len = std::mem::size_of::<libc::sockaddr_un>() as u8;
        }
        let length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let mut held = Vec::new();
        loop {
            let socket = crate::platform::test_nonblocking_socket();
            let connected = unsafe {
                libc::connect(
                    std::os::fd::AsRawFd::as_raw_fd(&socket),
                    &address as *const _ as *const libc::sockaddr,
                    length,
                )
            };
            if connected == 0 {
                held.push(socket);
                continue;
            }
            let error = io::Error::last_os_error();
            drop(socket);
            if error.raw_os_error().is_some_and(crate::platform::queue_full) {
                break;
            }
            panic!("filling the listen queue: {error}");
        }
        held
    }

    fn ledger_of(provider: provider_binding::Provider) -> Value {
        let backend = match provider {
            provider_binding::Provider::DeepSeek => "dsh",
            provider_binding::Provider::OpenCode => "opencode",
        };
        serde_json::from_slice(
            &fs::read(
                root()
                    .join("native-identities")
                    .join(format!("{backend}-writes.json")),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_full_unix_listen_queue_refuses_a_claim_within_the_bound() {
        isolated("full_queue_fixture");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn full_queue_fixture() {
        use sha2::Digest;
        use std::time::{Duration, Instant};
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-taljw: a host whose listen queue never frees must refuse the
        // claim at its bound instead of blocking the agent's tool shell.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("queue-home");
        let mut parent = claim_record("dsh/example/parent", "session-parent", &home);
        provider.save(&mut parent).unwrap();
        let choice = Choice {
            session_id: string(&parent, "session_id").into(),
            name: string(&parent, "name").into(),
            run_id: string(&parent, "run_id").into(),
            conversation_id: string(&parent, "conversation_id").into(),
        };
        let hosts = root().join("dsh/hosts").join(
            &format!(
                "{:x}",
                sha2::Sha256::digest(home.to_string_lossy().as_bytes())
            )[..16],
        );
        fs::create_dir_all(&hosts).unwrap();
        let listener = full_unix_listener(&hosts.join("host.sock"), 1);
        let fill = fill_unix_queue(&hosts.join("host.sock"));
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(claimed_parent(&choice));
        });
        let started = Instant::now();
        match received.recv_timeout(Duration::from_secs(6)) {
            Ok(result) => {
                assert!(
                    started.elapsed() < Duration::from_secs(6),
                    "{:?}",
                    started.elapsed()
                );
                let error = result.unwrap_err();
                assert!(
                    error.contains("did not confirm") || error.contains("must be a live session"),
                    "{error}"
                );
            }
            Err(_) => panic!("the claim blocked past its bound"),
        }
        drop(fill);
        drop(listener);
    }

    #[cfg(unix)]
    #[test]
    fn an_in_place_binding_rewrite_is_seen() {
        isolated("in_place_rewrite_fixture");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn in_place_rewrite_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-hgt89: a write that keeps the file's directory entry but
        // rewrites its content must not leave the warm index trusted.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("in-place-home");
        let native = "session-in-place";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        let mut other = claim_record("dsh/example/other", "session-other", &home);
        provider.save(&mut other).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        // An older writer rewrites `other` in place, so it now holds the
        // claimed conversation too without changing any directory entry.
        let duplicate = claim_record("dsh/example/other", native, &home);
        fs::write(
            provider.label_path("dsh/example/other"),
            duplicate.to_string(),
        )
        .unwrap();
        assert!(
            provider.native_caller(native, &home).is_err(),
            "an in-place duplicate is ambiguous, not the cached session"
        );
    }

    #[test]
    fn a_failed_native_identity_note_is_not_accounted() {
        isolated("failed_note_fixture");
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn failed_note_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-8g1je: a write whose optional native-identity note failed must
        // stay unaccounted, so a later warm lookup cannot trust the index.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("failed-note-home");
        let native = "session-note";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        let accounted = ledger_of(provider)["stamp"].clone();
        // A directory where the derived cache file belongs makes the note fail.
        let cache = provider_binding::cache_path_for(provider, native, &home);
        fs::remove_file(&cache).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let mut duplicate = claim_record("dsh/example/duplicate", native, &home);
        provider.save(&mut duplicate).unwrap();
        assert_eq!(
            ledger_of(provider)["stamp"],
            accounted,
            "a failed native-identity note must not be accounted as written"
        );
        assert!(
            provider.native_caller(native, &home).is_err(),
            "an unaccounted duplicate must not answer from the warm index"
        );
        fs::remove_dir(&cache).unwrap();
    }

    #[test]
    fn a_dirty_ledger_is_rebaselined_by_the_next_complete_scan() {
        isolated("dirty_rebaseline_fixture");
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn dirty_rebaseline_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-8g1je: a ledger that fell behind an unaccounted write must be
        // re-baselined by the next complete scan instead of making every warm
        // claim pay the bounded changed-binding check.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("rebaseline-home");
        let native = "session-rebaseline";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        let before = ledger_of(provider);
        let foreign = claim_record("dsh/example/foreign", "session-unrelated", &home);
        atomic(
            &provider.label_path("dsh/example/foreign"),
            &foreign.to_string(),
        )
        .unwrap();
        fs::remove_file(provider.label_path("dsh/example/foreign")).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"],
            "the answer stays the warm session"
        );
        let after = ledger_of(provider);
        assert_ne!(
            after["stamp"], before["stamp"],
            "the dirty ledger was re-baselined"
        );
        assert_ne!(
            after["epoch"], before["epoch"],
            "a complete scan retires older indexes"
        );
        assert!(after["dirty_since"].is_null(), "{after}");
    }

    #[test]
    fn an_unchanged_overflow_ledger_does_not_scan_every_find() {
        isolated("overflow_stamp_fixture");
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn overflow_stamp_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-wult0: overflow never proves an identity. It stays unavailable,
        // but an unchanged bounded witness must not cost a complete scan.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("overflow-home");
        let native = "session-overflow";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        for index in 0..provider_binding::RECORDS_FOR_TEST {
            let name = format!("dsh/example/fill-{index}");
            let record = claim_record(&name, &format!("session-fill-{index}"), &home);
            fs::write(provider.label_path(&name), record.to_string()).unwrap();
        }
        let find = || {
            let before = provider_binding::complete_scan_count();
            let error = provider.native_caller(native, &home).unwrap_err();
            assert!(error.contains("metadata bound"), "{error}");
            provider_binding::complete_scan_count() - before
        };
        let mut counts = vec![find(), find()];
        let mut changed = first.clone();
        changed["overflow_change"] = json!(true);
        fs::write(
            provider.label_path(string(&changed, "name")),
            changed.to_string(),
        )
        .unwrap();
        counts.extend([find(), find()]);
        println!("overflow-find-scans={counts:?}");
        assert_eq!(counts, [1, 0, 1, 0]);
    }

    #[test]
    fn over_bound_outcomes_are_preserved() {
        isolated("overflow_outcomes_fixture");
    }

    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn overflow_outcomes_fixture() {
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // The warm changed-binding check has the same RECORDS bound as the
        // complete scan, so these actual over-bound outcomes are unavailable.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("overflow-outcomes-home");
        let native = "session-overflow-outcomes";
        let mut first = claim_record("dsh/example/first", native, &home);
        provider.save(&mut first).unwrap();
        assert_eq!(
            provider.native_caller(native, &home).unwrap().unwrap()["session_id"],
            first["session_id"]
        );
        for index in 0..provider_binding::RECORDS_FOR_TEST {
            let name = format!("dsh/example/fill-{index}");
            let record = claim_record(&name, &format!("session-fill-{index}"), &home);
            fs::write(provider.label_path(&name), record.to_string()).unwrap();
        }
        let mut other = claim_record("dsh/example/other", "session-other", &home);
        provider.save(&mut other).unwrap();
        let warm = provider.native_caller(native, &home);
        println!("overflow-warm-current={warm:?}");
        assert!(warm.unwrap_err().contains("metadata bound"));
        let absent = provider.native_caller("session-absent", &home);
        println!("overflow-warm-absent={absent:?}");
        assert!(absent.unwrap_err().contains("metadata bound"));
        let mut unlisted = claim_record("dsh/example/fill-0", native, &home);
        unlisted["overflow_change"] = json!(true);
        fs::write(
            provider.label_path(string(&unlisted, "name")),
            unlisted.to_string(),
        )
        .unwrap();
        let changed = provider.native_caller(native, &home);
        println!("overflow-unlisted-changed={changed:?}");
        assert!(changed.unwrap_err().contains("metadata bound"));
    }

    #[cfg(unix)]
    #[test]
    fn a_busy_native_host_refuses_a_claim_quickly() {
        isolated("busy_host_fixture");
    }

    #[cfg(unix)]
    struct Owner(std::process::Child);
    #[cfg(unix)]
    impl Owner {
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.kill()?;
            self.0.wait().map(|_| ())
        }
    }
    #[cfg(unix)]
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_connect_on_a_live_host_is_busy_not_dead() {
        isolated("refused_connect_fixture");
    }

    /// zerus-xt6re: BSD refuses a connect on a full listen queue at once
    /// (ECONNREFUSED, sometimes ECONNRESET). The recorded owner decides: a
    /// live owner is a busy host, a dead owner or a missing socket a definite
    /// no. Emulated on Linux by injecting the refusal at the claim's connect.
    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn refused_connect_fixture() {
        use sha2::Digest;
        use std::time::{Duration, Instant};
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("refused-home");
        let mut parent = claim_record("dsh/example/parent", "session-parent", &home);
        provider.save(&mut parent).unwrap();
        let choice = Choice {
            session_id: string(&parent, "session_id").into(),
            name: string(&parent, "name").into(),
            run_id: string(&parent, "run_id").into(),
            conversation_id: string(&parent, "conversation_id").into(),
        };
        let hosts = root().join("dsh/hosts").join(
            &format!(
                "{:x}",
                sha2::Sha256::digest(home.to_string_lossy().as_bytes())
            )[..16],
        );
        fs::create_dir_all(&hosts).unwrap();
        let socket = hosts.join("host.sock");
        // A small explicit backlog: fill_unix_queue opens one socket per
        // queue slot, and the default SOMAXCONN backlog exhausts descriptors
        // under the package check's low nofile limit (zerus-xt6re, zerus-taljw).
        let listener = full_unix_listener(&socket, 1);
        // A genuinely saturated host: its listen queue is full, so BSD would
        // answer a non-blocking connect with ECONNREFUSED at once.
        let fill = fill_unix_queue(&socket);
        let mut owner = Owner(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        dsh::record_running_for_test(&hosts, owner.0.id()).unwrap();
        dsh::refuse_connect_for_test(std::io::ErrorKind::ConnectionRefused);
        let started = Instant::now();
        let error = claimed_parent(&choice).unwrap_err();
        assert!(error.contains("did not confirm"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // A reset refusal is the same busy answer.
        dsh::refuse_connect_for_test(std::io::ErrorKind::ConnectionReset);
        let error = claimed_parent(&choice).unwrap_err();
        assert!(error.contains("did not confirm"), "{error}");
        // The socket is gone: a definite no, even with a live owner recorded.
        fs::remove_file(&socket).unwrap();
        let error = claimed_parent(&choice).unwrap_err();
        assert!(error.contains("must be a live session"), "{error}");
        // The recorded owner exited: a definite no, even with the socket back.
        drop(listener);
        let rebound = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        owner.kill().unwrap();
        let error = claimed_parent(&choice).unwrap_err();
        assert!(error.contains("must be a live session"), "{error}");
        dsh::allow_connect_for_test();
        drop(fill);
        drop(rebound);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "process-isolated private state fixture"]
    fn busy_host_fixture() {
        use sha2::Digest;
        use std::time::{Duration, Instant};
        if std::env::var("ZERUS_CLAIMED_PARENT_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        // zerus-p24ce: a host that accepts the claim check but never answers
        // is refused as busy within the claim bound, never treated as live.
        let provider = provider_binding::Provider::DeepSeek;
        let home = root().join("busy-home");
        let mut parent = claim_record("dsh/example/parent", "session-parent", &home);
        provider.save(&mut parent).unwrap();
        let choice = Choice {
            session_id: string(&parent, "session_id").into(),
            name: string(&parent, "name").into(),
            run_id: string(&parent, "run_id").into(),
            conversation_id: string(&parent, "conversation_id").into(),
        };
        let hosts = root().join("dsh/hosts").join(
            &format!(
                "{:x}",
                sha2::Sha256::digest(home.to_string_lossy().as_bytes())
            )[..16],
        );
        fs::create_dir_all(&hosts).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(hosts.join("host.sock")).unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                held.push(stream);
            }
        });
        let started = Instant::now();
        let error = claimed_parent(&choice).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?} {error}",
            started.elapsed()
        );
        assert!(error.contains("did not confirm"), "{error}");
        // No host at all: definitively not live, at once.
        fs::remove_file(hosts.join("host.sock")).unwrap();
        let started = Instant::now();
        let error = claimed_parent(&choice).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(error.contains("must be a live session"), "{error}");
    }

    #[test]
    fn a_link_needs_a_canonical_machine_an_identity_a_label_and_a_known_source() {
        assert!(valid(&link()));
        for (key, value) in [
            ("schema", json!(2)), ("machine", json!("not-a-uuid")),
            ("machine", json!("00000000-0000-0000-0000-000000000000")),
            ("machine", json!("11111111-1111-4111-8111-11111111111A")),
            ("session_id", json!("X")), ("name", json!("")), ("name", json!("a\nb")),
            ("host", json!("")), ("source", json!("guess")), ("at", json!("now")),
        ] {
            let mut bad = link();
            bad[key] = value;
            assert!(!valid(&bad), "{key}");
        }
        let mut anonymous = link();
        for key in ["session_id", "conversation_id", "run_id"] {
            anonymous[key] = json!("");
        }
        assert!(!valid(&anonymous));
        let mut older = link();
        older["session_id"] = json!("");
        assert!(valid(&older), "an older parent record matches by conversation");
    }

    #[test]
    fn only_approvals_trust_and_sign_in_are_the_persons_requests() {
        let row = |phase: &str| json!({"phase": phase});
        assert!(human_request(&row("approval"), None));
        assert!(!human_request(&row("input"), None));
        assert!(!human_request(&row("idle"), Some(&[])));
        let asked = |id: &str| vec![json!({"question_id": id})];
        for id in ["claude-trust:abc", "codex-trust:abc", "kimi-trust:abc", "codex-hooks-trust:abc"] {
            assert!(human_request(&row("input"), Some(&asked(id))), "{id}");
        }
        assert!(!human_request(&row("input"), Some(&asked("toolu_ask_user"))));
        assert!(!human_request(&row("input"), Some(&asked("x-claude-trust:abc"))));
        assert!(human_request(&json!({"phase":"error","provider_error":{"error_kind":"authentication"}}), None));
        assert!(!human_request(&json!({"phase":"error","provider_error":{"error_kind":"quota"}}), None));
        assert!(human_request(&json!({"phase":"input","activity_summary":"Sign in required"}), None));
        let child = |state: &str| json!({"phase":"tool","subagents":{"kid":{"state":"working","display_state":state}}});
        assert!(human_request(&child("approval"), None), "a native subagent's approval");
        assert!(!human_request(&child("input"), None));
        assert!(human_request(&json!({"phase":"tool","subagent_previews":[{"state":"approval"}]}), None));
    }

    #[test]
    fn a_forwarded_link_must_be_a_well_formed_launch_link() {
        let hex = |value: &Value| value.to_string().bytes().map(|b| format!("{b:02x}")).collect::<String>();
        for bad in ["", "zz", "abc", &hex(&json!({"schema": 2}))] {
            assert!(accept_forwarded(bad).is_err(), "{bad}");
        }
        let mut adopted = link();
        adopted["source"] = json!("adopt");
        assert!(accept_forwarded(&hex(&adopted)).is_err(), "only a launch is forwarded");
        let mut forwarded = link();
        forwarded["source"] = json!(FORWARDED_SOURCE);
        assert!(accept_forwarded(&hex(&forwarded)).is_err(), "a forwarded link is not forwarded again");
        // Non-ASCII input is refused, never sliced inside a character (zerus-so6hr).
        for bad in ["\u{20ac}x", "a\u{20ac}", "\u{e9}\u{e9}", "0g"] {
            assert!(forwarded_link(bad).is_err(), "{bad}");
        }
        assert_eq!(forwarded_link(&hex(&link())).unwrap(), link());
    }

    #[test]
    fn a_forwarded_parent_is_never_live_even_when_its_ids_match_here() {
        // zerus-l4ehe: another computer's claim naming a live session here.
        let live = "22222222-2222-4222-8222-222222222222";
        let mut claim = link();
        claim["session_id"] = json!(live);
        claim["conversation_id"] = json!("conv-live");
        claim["source"] = json!(FORWARDED_SOURCE);
        assert!(valid(&claim));
        let mut rows = vec![
            json!({"name":"claude/p/lead","session_id":live,"conversation_id":"conv-live","state":"running"}),
            json!({"name":"codex/p/kid","parent":claim}),
        ];
        decorate(&mut rows);
        assert_eq!(rows[1]["parent"]["state"], "missing");
        // The same IDs from a local proof match.
        rows[1]["parent"]["source"] = json!("launch");
        decorate(&mut rows);
        assert_eq!(rows[1]["parent"]["state"], "live");
    }

    #[test]
    fn listings_report_the_parent_state_from_the_same_listing() {
        let machine = "11111111-1111-4111-8111-111111111111";
        let child = |session: &str| json!({"name":"codex/p/kid","parent":{"machine":machine,"session_id":session,"conversation_id":"conv"}});
        let mut rows = vec![
            json!({"name":"claude/p/lead","session_id":"22222222-2222-4222-8222-222222222222","state":"running"}),
            json!({"name":"claude/p/old","session_id":"33333333-3333-4333-8333-333333333333","state":"archived"}),
            json!({"name":"claude/p/old","session_id":"33333333-3333-4333-8333-333333333333","state":"paused"}),
            json!({"name":"claude/p/gone","session_id":"44444444-4444-4444-8444-444444444444","state":"running","process_state":"exited"}),
            child("22222222-2222-4222-8222-222222222222"),
            child("33333333-3333-4333-8333-333333333333"),
            child("44444444-4444-4444-8444-444444444444"),
            child("55555555-5555-4555-8555-555555555555"),
        ];
        let states = |rows: &[Value]| rows[4..].iter().map(|row| row["parent"]["state"].clone()).collect::<Vec<_>>();
        decorate(&mut rows);
        assert_eq!(states(&rows), [json!("live"), json!("saved"), json!("saved"), json!("missing")]);
    }
}
