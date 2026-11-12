//! Shared read marks. The host that runs a session is their only writer and
//! source of truth: desktops and phones read them from `hgs ls --json` and
//! write them with `hgs [@host] read`.
use super::*;
pub(super) mod observation;
use sha2::{Digest, Sha256};
use std::fs;
use std::time::{Duration, Instant};

const ACTIONABLE: &[&str] = &["approval", "input", "error"];
// Question indexing shares this budget across one listing (or one `hgs read`);
// each session still gets a short slice to catch up with appended lines.
const QUESTION_BUDGET: Duration = Duration::from_secs(1);
const QUESTION_SLICE: Duration = Duration::from_millis(50);

pub(super) fn question_budget() -> Instant {
    Instant::now() + QUESTION_BUDGET
}

pub(super) fn path() -> PathBuf {
    root().join("read-marks.json")
}

/// Marks by owner (a tracked session's stable journal name, otherwise its
/// name) and conversation. A missing or unreadable file means no marks.
pub(super) fn load() -> Value {
    fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|value| value["version"] == 1 && value["sessions"].is_object())
        .map(|value| value["sessions"].clone())
        .unwrap_or_else(|| json!({}))
}

/// Writers must not turn an unreadable store into an empty one. In particular,
/// a current request is no authority to discard another owner's saved marks.
fn load_writable(path: &Path) -> Result<Value> {
    let mut file = match crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(format!("Shared read marks could not be read: {error}")),
    };
    if !crate::platform::verify_private_file(&file).map_err(|error| error.to_string())? {
        return Err("Shared read marks could not be read; the store must be a private regular file".into());
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text)
        .map_err(|error| format!("Shared read marks could not be read: {error}"))?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(|value| value["version"] == 1 && value["sessions"].is_object())
        .map(|value| value["sessions"].clone())
        .ok_or_else(|| "Shared read marks could not be read; no marks were changed".into())
}

pub(super) fn record<'a>(marks: &'a Value, owner: &str, conversation: &str) -> &'a Value {
    static NONE: Value = Value::Null;
    if conversation.is_empty() {
        return &NONE;
    }
    marks.get(owner).and_then(|m| m.get(conversation)).unwrap_or(&NONE)
}

/// Canonical identity of what needs the owner now, or None. Status-only
/// changes (activity text, timestamps, tool progress, finished children)
/// never change it; a new request, question or failed turn always does.
/// `questions` is None when they could not be indexed in time: the result then
/// differs from every signature `hgs read` can acknowledge.
pub(super) fn signature(session: &Value, questions: Option<&[Value]>) -> Option<String> {
    let running = ["", "running"].contains(&string(session, "state"))
        && string(session, "process_state") != "exited";
    if !running {
        return None;
    }
    let phase = string(session, "phase");
    let actionable = ACTIONABLE.contains(&phase);
    let attention = if !actionable {
        Value::Null
    } else if phase != "error" {
        // The start time tells apart requests without a native identity.
        json!([session["attention_id"], session["attention_started"]])
    } else if !string(session, "attention_id").is_empty() {
        session["attention_id"].clone()
    } else {
        // A failed turn without a native error identity is still a new event.
        json!(["turn", session["turn_started"]])
    };
    // The same identities the mobile connector publishes as `mobile_attention`.
    let asked = questions.map_or_else(
        || json!("unknown"),
        |questions| {
            let mut asked: Vec<Value> = questions
                .iter()
                .filter(|card| card["answer_delivery"]["status"] != "submitted")
                .map(|card| json!([card["question_id"], card["question_hash"]]))
                .collect();
            asked.sort_by_key(Value::to_string);
            asked.dedup();
            json!(asked)
        },
    );
    let mut children = Vec::new();
    if string(session, "subagent_source") != "hook_profiles" {
        for (id, child) in session["subagents"].as_object().into_iter().flatten() {
            let state = child["display_state"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| string(child, "state"));
            if ["approval", "input", "attention"].contains(&state) {
                let request = child["attention_id"]
                    .as_str()
                    .or_else(|| child["question_id"].as_str())
                    .unwrap_or("");
                children.push(json!([id, state, request]));
            }
        }
    }
    children.sort_by_key(Value::to_string);
    if !actionable && asked.as_array().is_some_and(Vec::is_empty) && children.is_empty() {
        return None;
    }
    let canonical = json!([1, session["run_id"], session["conversation_id"],
        if actionable { phase } else { "" }, attention, asked, children]);
    Some(format!("{:x}", Sha256::digest(canonical.to_string().as_bytes())))
}

/// Pending native question identities of a live tracked session. Only Codex
/// asks without changing phase; other providers are read only while waiting.
/// Ok(None) when the Codex question index cannot catch up within `budget` (or
/// a short slice of its own): a listing never runs an unbounded transcript
/// scan. Err when the Codex index cannot be read; other providers never ask
/// optional questions, so their read errors count as no questions.
pub(super) fn pending_questions(record: &Value, session: &Value, live_pane: bool, budget: Instant) -> Result<Option<Vec<Value>>> {
    if !live_pane
        || (string(record, "agent") != "codex" && !ACTIONABLE.contains(&string(session, "phase")))
        || !process_alive(record)
    {
        return Ok(Some(Vec::new()));
    }
    let deadline = budget.max(Instant::now() + QUESTION_SLICE);
    let questions = questions::current_until(record, deadline);
    if string(record, "agent") == "codex" {
        questions
    } else {
        Ok(questions.unwrap_or(Some(Vec::new())))
    }
}

/// At most this many optional question IDs are listed per session, the newest;
/// the count stays exact.
const OPTIONAL_IDS: usize = 32;

/// `optional_questions` of a listing row: the pending optional (Codex async)
/// questions without a submitted answer, as identities only, never their text.
/// Unknown questions (None) list null, never a count of 0. Without any, only
/// the count is listed, which keeps snapshots of many saved sessions small.
pub(super) fn optional_questions(questions: Option<&[Value]>) -> Value {
    let Some(questions) = questions else {
        return Value::Null;
    };
    let mut asked = BTreeMap::new();
    for card in questions {
        if card["source"] != "codex_async"
            || card["optional"] != true
            || card["answer_delivery"]["status"] == "submitted"
        {
            continue;
        }
        let Some(id) = card["question_id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        let at = card["created_at"].as_f64().unwrap_or(0.0);
        let newest = asked.entry(id).or_insert(at);
        *newest = newest.max(at);
    }
    if asked.is_empty() {
        return json!({"count": 0});
    }
    let mut ordered: Vec<(f64, &str)> = asked.into_iter().map(|(id, at)| (at, id)).collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    let latest = ordered.last().map(|(at, _)| *at).filter(|at| *at > 0.0);
    let ids: Vec<&str> = ordered[ordered.len().saturating_sub(OPTIONAL_IDS)..]
        .iter()
        .map(|(_, id)| *id)
        .collect();
    json!({"count": ordered.len(), "ids": ids, "latest_at": latest})
}

/// While routine ticks of a recurring job are skipped, a reply already read
/// stays current: moving back to an older reply would make it unread again.
fn keep_read_reply(session: &mut Value, mark: &Value) {
    if session["schedules"]["recurring"].as_u64().is_none_or(|count| count == 0) {
        return;
    }
    let journaled = |id: &Value| {
        let (seq, at) = id.as_str()?.split_once(':')?;
        Some((seq.parse::<i64>().ok()?, at.parse::<f64>().ok()?))
    };
    if let Some((read, at)) = journaled(&mark["reply_id"]) {
        if journaled(&session["reply_id"]).is_none_or(|(current, _)| read > current) {
            session["reply_id"] = mark["reply_id"].clone();
            session["reply_at"] = json!(at);
        }
    }
}

/// Add the C1 fields to one `hgs ls --json` session object.
pub(super) fn enrich(session: &mut Value, marks: &Value, owner: &str, questions: Option<&[Value]>) {
    let mark = record(marks, owner, string(session, "conversation_id")).clone();
    keep_read_reply(session, &mark);
    let digest = signature(session, questions);
    session["attention_signature"] = json!(digest);
    session["read"] = mark;
}

/// Add the C1 fields and `optional_questions` to one listing row, given its
/// questions as `pending_questions` found them. An unreadable index adds no
/// attention, as a listing never fails on optional indexes, but leaves the
/// optional questions unknown.
pub(super) fn enrich_listing(session: &mut Value, marks: &Value, owner: &str, questions: Result<Option<Vec<Value>>>) {
    session["optional_questions"] = optional_questions(questions.as_ref().ok().and_then(Option::as_deref));
    enrich(session, marks, owner, questions.unwrap_or(Some(Vec::new())).as_deref());
}

const MAX_CONVERSATIONS: usize = 8;
const USAGE: &str = "usage: hgs read <session> --json   (JSON stdin: run_id, conversation_id, reply_id, attention_signature)";

#[derive(Debug, PartialEq)]
struct Request {
    run: String,
    conversation: String,
    reply: Option<String>,
    signature: Option<String>,
}

fn parse(value: &Value) -> Result<Request> {
    const KEYS: [&str; 4] = ["run_id", "conversation_id", "reply_id", "attention_signature"];
    let object = value.as_object().ok_or("read mark must be a JSON object")?;
    if object.len() != KEYS.len() || KEYS.iter().any(|key| !object.contains_key(*key)) {
        return Err("read mark needs exactly run_id, conversation_id, reply_id and attention_signature".into());
    }
    let text = |key: &str| -> Result<String> {
        value[key]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(|| format!("invalid {key}"))
    };
    let optional = |key: &str| -> Result<Option<String>> {
        if value[key].is_null() { Ok(None) } else { text(key).map(Some) }
    };
    let signature = optional("attention_signature")?;
    if signature.as_deref().is_some_and(|s| {
        s.len() != 64 || !s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    }) {
        return Err("attention_signature must be a lowercase hex SHA-256".into());
    }
    Ok(Request {
        run: text("run_id")?,
        conversation: text("conversation_id")?,
        reply: optional("reply_id")?,
        signature,
    })
}

/// Each requested field moves only to the session's current value, in the
/// same run and conversation. Stale fields leave the stored mark unchanged.
/// Attention whose questions could not be indexed is never acknowledged.
fn apply(current: &Value, questions: Option<&[Value]>, request: &Request, existing: &Value, at: f64) -> (bool, bool, Value) {
    let same = string(current, "run_id") == request.run
        && string(current, "conversation_id") == request.conversation;
    let reply = same
        && request.reply.as_deref().is_some_and(|reply| current["reply_id"].as_str() == Some(reply));
    let attention = same
        && questions.is_some()
        && request.signature.is_some()
        && request.signature == signature(current, questions);
    if !reply && !attention {
        return (false, false, existing.clone());
    }
    let mut next = if existing.is_object() {
        existing.clone()
    } else {
        json!({"conversation_id": request.conversation, "reply_id": null, "attention_signature": null})
    };
    if reply {
        next["reply_id"] = json!(request.reply);
    }
    if attention {
        next["attention_signature"] = json!(request.signature);
    }
    // A duplicate acknowledgement still applies, but it is not a new read edge.
    // Preserve its timestamp and avoid rewriting the complete legacy file.
    if next != *existing {
        next["at"] = json!(at);
    }
    (reply, attention, next)
}

/// Moves the marks of a session owned by its name (DeepSeek) to its new name;
/// a tracked session keeps its journal name across renames.
pub(super) fn rename_owner(old: &str, new: &str) -> Result<()> {
    lock_rename()?.transfer(old, new)
}

/// OpenCode renames take this writer before their bindings lock. Read writers
/// inspect OpenCode while holding the same writer, so reversing that order can
/// deadlock. The existing v1 file stays the only reader/writer protocol.
pub(super) struct RenameGuard {
    _guard: Guard,
    path: PathBuf,
    sessions: Value,
}
impl RenameGuard {
    pub(super) fn at(path: &Path) -> Result<Self> {
        let guard = lock(Some(&path.with_extension("lock")))?;
        let sessions = load_writable(path)
            .map_err(|_| "Shared read marks could not be read; rename was not started")?;
        Ok(Self {
            _guard: guard,
            path: path.to_owned(),
            sessions,
        })
    }

    pub(super) fn transfer(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let map = self
            .sessions
            .as_object_mut()
            .ok_or("Invalid shared read marks")?;
        let Some(marks) = map.remove(old) else {
            return Ok(());
        };
        map.insert(new.to_owned(), marks);
        atomic(
            &self.path,
            &format!("{}\n", json!({"version": 1, "sessions": self.sessions})),
        )
    }
}

pub(super) fn lock_rename() -> Result<RenameGuard> {
    RenameGuard::at(&path())
}

/// Keep the newest conversations of one session.
fn bounded(conversations: &mut serde_json::Map<String, Value>) {
    while conversations.len() > MAX_CONVERSATIONS {
        let Some(oldest) = conversations
            .iter()
            .min_by(|a, b| a.1["at"].as_f64().unwrap_or(0.0).total_cmp(&b.1["at"].as_f64().unwrap_or(0.0)))
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        conversations.remove(&oldest);
    }
}

/// Only admitting a new owner can grow the global owner population. Recheck the
/// authoritative bindings before that growth, as before; an established owner
/// can change only its own (at most eight) conversations. Retained abandoned
/// owners are pruned at the next admission, not on every acknowledgement.
fn insert_with_owners(
    sessions: &mut Value,
    owner: &str,
    conversation: &str,
    next: Value,
    owners: impl FnOnce() -> Result<std::collections::BTreeSet<String>>,
    mut native_owner: impl FnMut(&str) -> bool,
) -> Result<()> {
    let map = sessions.as_object_mut().ok_or("invalid read marks")?;
    if !map.contains_key(owner) {
        let owners = owners()?;
        map.retain(|key, _| key == owner || owners.contains(key) || native_owner(key));
    }
    let conversations = map.entry(owner.to_owned()).or_insert_with(|| json!({}));
    let conversations = conversations.as_object_mut().ok_or("invalid read marks")?;
    conversations.insert(conversation.to_owned(), next);
    bounded(conversations);
    Ok(())
}

/// A session view, its mark owner and its pending questions.
type Current = (Value, String, Option<Vec<Value>>);
/// The session as `hgs ls --json` shows it, its mark owner and its pending
/// questions (None when not indexed in time); None when the name has no
/// current tracked or native session.
fn current(name: &str) -> Result<Option<Current>> {
    if opencode::exists(name) {
        let view=opencode::inspect(name,None)?; let questions=view["pending_questions"].as_array().cloned();return Ok(Some((view,name.to_owned(),questions)));
    }
    if dsh::exists(name) {
        return dsh::snapshot(name).map(|view| Some((view, name.to_owned(), Some(Vec::new()))));
    }
    archive::reconcile_selected(name)?;
    // An approval answered in Terminal sends no hook. As in `hgs ls`, settle it
    // from the screen first; that takes the lock, and only a pending request
    // captures the terminal.
    if record_path(name).exists() {
        if let Ok(record) = read(name) {
            claude_approval::reconcile(record);
        }
    }
    let guard = lock(None)?;
    if !record_path(name).exists() {
        return Ok(None);
    }
    let record = read(name)?;
    if archive::contains_selected(&record)? {
        return Ok(None);
    }
    let snapshot = live()?;
    let panes = panes_of(&snapshot, &record);
    // A terminal name reused by another pane is not this session.
    if panes.is_some() && !matches(&record, panes) {
        return Ok(None);
    }
    let live_pane = panes.is_some();
    if !live_pane && string(&record, "conversation_id").is_empty() {
        return Ok(None);
    }
    let view = read_view(name, &record, journal::summary(&record, live_pane), live_pane);
    drop(guard);
    // As in a listing, an unreadable index adds no attention.
    let questions = pending_questions(&record, &view, live_pane, question_budget()).unwrap_or(Some(Vec::new()));
    Ok(Some((view, journal::journal_name(&record).to_owned(), questions)))
}

/// The view of a journal-backed session that `hgs read` compares a mark
/// with, from its `summary`.
fn read_view(name: &str, record: &Value, mut summary: Value, live_pane: bool) -> Value {
    summary["name"] = json!(name);
    if !live_pane {
        summary["state"] = json!(if record["paused"].as_bool().unwrap_or(false) { "paused" } else { "stopped" });
    }
    summary
}

/// `current_read` of `hgs inspect <session> --current-read`: the reply and
/// attention identities `hgs read` would accept now, from the same inspection
/// a device displays. None without a conversation, which no mark can name.
///
/// Order is the contract. `summary` (and so the reply) must be captured
/// before the inspection reads provider messages: a reply's final message is
/// written before its Stop, so it is in that read or an earlier one. The
/// questions signed here are read before the inspection lists its own, so
/// each one that is still pending when a mark arrives was listed. Questions
/// not indexed in time, or an unreadable index, leave the signature unknown
/// (`attention_complete` false), never acknowledgeable.
pub(super) fn current_read(name: &str, journal_record: &Value, summary: Value, live_pane: bool) -> Option<Value> {
    let conversation = string(journal_record, "conversation_id");
    if conversation.is_empty() {
        return None;
    }
    let view = read_view(name, journal_record, summary, live_pane);
    #[cfg(test)]
    journal::read_order::note("signed_questions");
    // Unlike a listing or `hgs read`, which count an unreadable index as no
    // questions, this field never claims completeness it does not have: such
    // an index leaves the questions unknown (zerus-a4mub.28).
    let questions = pending_questions(journal_record, &view, live_pane, question_budget()).unwrap_or(None);
    Some(binding(journal::journal_name(journal_record), conversation, view, questions.as_deref()))
}

/// `current_read` of a native DeepSeek or OpenCode inspection (zerus-a4mub.24):
/// `view` is the session as `hgs read` compares a mark with it (`current`),
/// owned by its name. Each provider captures it in its own read order, before
/// the messages and questions its inspection shows. `questions` is None when
/// the provider's attention is unknown: then `attention_complete` is false
/// and the signature matches nothing `hgs read` accepts. None without a run
/// or conversation, which no mark can name.
pub(super) fn native_current_read(name: &str, view: Value, questions: Option<&[Value]>) -> Option<Value> {
    let conversation = string(&view, "conversation_id").to_owned();
    if string(&view, "run_id").is_empty() || conversation.is_empty() {
        return None;
    }
    Some(binding(name, &conversation, view, questions))
}

/// The reply and attention identities `hgs read` would accept now for `view`
/// and its mark `owner` in `conversation`.
fn binding(owner: &str, conversation: &str, mut view: Value, questions: Option<&[Value]>) -> Value {
    // As `hgs read`: a recurring job's read reply stays current. Only those
    // sessions need the marks file.
    if view["schedules"]["recurring"].as_u64().is_some_and(|count| count > 0) {
        let marks = load();
        keep_read_reply(&mut view, record(&marks, owner, conversation));
    }
    let reply = view["reply_id"].as_str().filter(|reply| !reply.is_empty());
    json!({
        "schema": 1,
        "run_id": view["run_id"],
        "conversation_id": view["conversation_id"],
        "reply_id": reply,
        "attention_signature": signature(&view, questions),
        "attention_complete": questions.is_some(),
    })
}

/// Which fields of `current_read` `hgs read` applies to `view` with
/// `questions` (`current`), from an empty mark.
#[cfg(test)]
pub(super) fn accepts(view: &Value, questions: Option<&[Value]>, current_read: &Value) -> (bool, bool) {
    let request = parse(&json!({
        "run_id": current_read["run_id"],
        "conversation_id": current_read["conversation_id"],
        "reply_id": current_read["reply_id"],
        "attention_signature": current_read["attention_signature"],
    }))
    .unwrap();
    let (reply, attention, _) = apply(view, questions, &request, &Value::Null, 1.0);
    (reply, attention)
}

/// `hgs read <session> --json`. An unknown session is a JSON answer with exit
/// status 1, so desktops and the mobile connector can tell it from a failure.
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let [name, flag] = args else {
        return Err(USAGE.into());
    };
    if flag != "--json" || name.is_empty() || name.starts_with(['-', '@']) {
        return Err(USAGE.into());
    }
    let request = parse(&stdin_json()?)?;
    // zerus-ucck P3c: the mark's run and conversation select among sessions
    // that share the label; with several, or none named, it refuses (exit 5).
    expect(name, &request.run, &request.conversation)?;
    // Capture the native view in writer order: a waiting older reader must
    // not overwrite a newer mark using a snapshot from before that write.
    let _guard = lock(Some(&root().join("read-marks.lock")))?;
    let Some((view, owner, questions)) = current(name)? else {
        println!("{}", json!({"ok": false, "error": "not_found"}));
        return Ok(1);
    };
    let mut sessions = load_writable(&path())?;
    let existing = record(&sessions, &owner, &request.conversation).clone();
    let mut view = view;
    keep_read_reply(&mut view, &existing);
    let (reply, attention, next) = apply(&view, questions.as_deref(), &request, &existing, now());
    if (reply || attention) && next != existing {
        insert_with_owners(&mut sessions, &owner, &request.conversation, next.clone(), || {
            records().map(|records| records.iter().map(|record| journal::journal_name(record).to_owned()).collect())
        }, |key| dsh::exists(key) || opencode::exists(key))?;
        atomic(&path(), &format!("{}\n", json!({"version": 1, "sessions": sessions})))?;
    }
    println!("{}", json!({"ok": true, "applied": {"reply_id": reply, "attention_signature": attention}, "read": next}));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_current_fields_apply_without_advancing_the_stored_timestamp() {
        let mut current = waiting();
        current["reply_id"] = json!("7:100.5");
        let digest = signature(&current, Some(&[])).unwrap();
        let both = request(Some("7:100.5"), Some(&digest));
        let (_, _, saved) = apply(&current, Some(&[]), &both, &Value::Null, 20.0);
        for repeated in [request(Some("7:100.5"), None), request(None, Some(&digest)), both] {
            let (reply, attention, next) = apply(&current, Some(&[]), &repeated, &saved, 30.0);
            assert_eq!(reply, repeated.reply.is_some());
            assert_eq!(attention, repeated.signature.is_some());
            assert_eq!(next, saved);
        }
        current["reply_id"] = json!("8:101.5");
        let (_, _, newer) = apply(&current, Some(&[]), &request(Some("8:101.5"), None), &saved, 40.0);
        assert_eq!(newer["at"], 40.0);
        assert_eq!(newer["attention_signature"], saved["attention_signature"]);
    }

    #[test]
    fn established_owner_has_a_constant_retention_scope() {
        let mut sessions = json!({"original":{"old":{"at":1.0}},"abandoned":{"other":{"at":2.0}}});
        for index in 0..20 {
            insert_with_owners(&mut sessions, "original", &format!("conversation-{index}"), json!({"at":10.0+f64::from(index)}),
                || panic!("existing owner must not enumerate bindings"),
                |_| panic!("existing owner must not enumerate native owners")).unwrap();
        }
        assert_eq!(sessions.as_object().unwrap().len(), 2);
        assert_eq!(sessions["original"].as_object().unwrap().len(), MAX_CONVERSATIONS);
        assert_eq!(sessions["abandoned"], json!({"other":{"at":2.0}}));
        assert!(sessions["original"].get("old").is_none());
    }

    #[test]
    fn new_owner_prunes_authoritatively_before_the_owner_population_grows() {
        let mut sessions = json!({"renamed-original":{"a":{"at":1.0}},"native":{"b":{"at":2.0}},"abandoned":{"c":{"at":3.0}}});
        let scans = std::cell::Cell::new(0);
        insert_with_owners(&mut sessions, "new", "d", json!({"at":4.0}), || {
            scans.set(scans.get()+1);
            Ok(["renamed-original".to_owned()].into_iter().collect())
        }, |name| name == "native").unwrap();
        assert_eq!(scans.get(), 1);
        assert_eq!(sessions, json!({"renamed-original":{"a":{"at":1.0}},"native":{"b":{"at":2.0}},"new":{"d":{"at":4.0}}}));
        let before = sessions.clone();
        assert!(insert_with_owners(&mut sessions, "unadmitted", "e", json!({"at":5.0}),
            || Err("binding inventory unavailable".into()), |_| false).is_err());
        assert_eq!(sessions, before);
    }

    #[test]
    fn writable_storage_preserves_legacy_marks_and_refuses_unreadable_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-marks.json");
        assert_eq!(load_writable(&path).unwrap(), json!({}));
        let legacy = json!({"version":1,"sessions":{"owner":{"conversation":{"reply_id":"one","at":1.0}}}});
        atomic(&path, &legacy.to_string()).unwrap();
        assert_eq!(load_writable(&path).unwrap(), legacy["sessions"]);
        // A legacy writer holds the same lock and replaces exactly this v1 file.
        let mut guard = RenameGuard::at(&path).unwrap();
        guard.transfer("owner", "renamed").unwrap();
        drop(guard);
        let mut old_writer: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(old_writer["version"], 1);
        old_writer["sessions"]["renamed"]["conversation"]["reply_id"] = json!("two");
        atomic(&path, &old_writer.to_string()).unwrap();
        assert_eq!(load_writable(&path).unwrap()["renamed"]["conversation"]["reply_id"], "two");
        for text in ["{", r#"{"version":2,"sessions":{}}"#, r#"{"version":1,"sessions":[]}"#] {
            atomic(&path, text).unwrap();
            assert!(load_writable(&path).is_err());
            assert!(RenameGuard::at(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load_writable(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn writable_storage_refuses_links_nonprivate_files_and_a_fifo_without_blocking() {
        use std::os::unix::fs::{symlink, FileTypeExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-marks.json");
        let target = dir.path().join("target.json");
        atomic(&target, r#"{"version":1,"sessions":{}}"#).unwrap();
        let unchanged = fs::read(&target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(load_writable(&path).is_err());
        assert_eq!(fs::read(&target).unwrap(), unchanged);
        fs::remove_file(&path).unwrap();
        symlink(dir.path().join("missing"), &path).unwrap();
        assert!(load_writable(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(load_writable(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&target, &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_writable(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_writable(&path).unwrap(), json!({}));
        fs::remove_file(&path).unwrap();
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert!(load_writable(&path).is_err());
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
    }

    #[test]
    fn skipped_ticks_never_move_a_read_reply_back() {
        let mark = json!({"conversation_id":"conversation-1","reply_id":"14053:1791743453.98","attention_signature":null,"at":1.0});
        let mut session = json!({"schedules":{"recurring":1},"reply_id":"13956:1791742557.19","reply_at":1791742557.19});
        keep_read_reply(&mut session, &mark);
        assert_eq!((session["reply_id"].clone(), session["reply_at"].clone()), (json!("14053:1791743453.98"), json!(1791743453.98)));
        // A newer reply from the person stays unread; no schedule changes nothing.
        let mut newer = json!({"schedules":{"recurring":1},"reply_id":"14100:1791744000.5"});
        keep_read_reply(&mut newer, &mark);
        assert_eq!(newer["reply_id"], "14100:1791744000.5");
        let mut ended = json!({"schedules":{"recurring":0},"reply_id":"13956:1791742557.19"});
        keep_read_reply(&mut ended, &mark);
        assert_eq!(ended["reply_id"], "13956:1791742557.19");
        let mut none = json!({"schedules":{"recurring":2}});
        keep_read_reply(&mut none, &mark);
        assert_eq!(none["reply_id"], "14053:1791743453.98");
        let mut unread = json!({"schedules":{"recurring":1},"reply_id":"13956:1791742557.19"});
        keep_read_reply(&mut unread, &json!(null));
        assert_eq!(unread["reply_id"], "13956:1791742557.19");
    }

    fn waiting() -> Value {
        json!({"name":"codex/project/review","run_id":"run-1","conversation_id":"conversation-1",
            "phase":"input","attention_id":"call-1","process_state":"running","activity":"busy",
            "activity_summary":"Input needed","last_event_at":10.0,"turn_started":5.0,
            "subagent_source":"hooks","subagents":{}})
    }

    fn hex(value: &str) -> bool {
        value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    }

    #[test]
    fn signature_ignores_status_only_updates() {
        let first = signature(&waiting(), Some(&[])).unwrap();
        assert!(hex(&first));
        let mut status = waiting();
        status["activity_summary"] = json!("Still waiting");
        status["last_event_at"] = json!(99.0);
        status["current_tool"] = json!("Bash");
        status["prompt"] = json!("Another preview");
        status["subagents"] = json!({"worker":{"state":"working","display_state":null,"updated":99.0}});
        assert_eq!(signature(&status, Some(&[])), Some(first));
    }

    #[test]
    fn signature_changes_for_each_new_request() {
        let first = signature(&waiting(), Some(&[])).unwrap();
        let mut next = waiting();
        next["attention_id"] = json!("call-2");
        assert_ne!(signature(&next, Some(&[])).unwrap(), first);
        let question = json!({"question_id":"q-1","question_hash":"h-1"});
        assert_ne!(signature(&waiting(), Some(std::slice::from_ref(&question))).unwrap(), first);
        let mut child = waiting();
        child["subagents"] = json!({"helper":{"state":"working","display_state":"approval"}});
        assert_ne!(signature(&child, Some(&[])).unwrap(), first);
        for (key, value) in [("run_id", "run-2"), ("conversation_id", "conversation-2"), ("phase", "approval")] {
            let mut changed = waiting();
            changed[key] = json!(value);
            assert_ne!(signature(&changed, Some(&[])).unwrap(), first, "{key}");
        }
    }

    #[test]
    fn nothing_actionable_or_not_running_has_no_signature() {
        let mut working = waiting();
        working["phase"] = json!("working");
        assert_eq!(signature(&working, Some(&[])), None);
        for (key, value) in [("state", "paused"), ("state", "stopped"), ("state", "archived"), ("process_state", "exited")] {
            let mut stopped = waiting();
            stopped[key] = json!(value);
            assert_eq!(signature(&stopped, Some(&[])), None, "{key}={value}");
        }
        // Kimi profile groups carry no per-child request identity.
        let mut profiles = working.clone();
        profiles["subagent_source"] = json!("hook_profiles");
        profiles["subagents"] = json!({"helper":{"state":"input"}});
        assert_eq!(signature(&profiles, Some(&[])), None);
    }

    #[test]
    fn pending_questions_need_attention_until_their_answer_is_submitted() {
        let mut working = waiting();
        working["phase"] = json!("working");
        let question = json!({"question_id":"q-1","question_hash":"h-1"});
        assert!(signature(&working, Some(std::slice::from_ref(&question))).is_some());
        let mut submitted = question;
        submitted["answer_delivery"] = json!({"status":"submitted"});
        assert_eq!(signature(&working, Some(&[submitted])), None);
    }

    const OPTIONAL_CONTRACT: &str =
        include_str!("../../mobile/android/app/src/test/resources/optional-question-contract.json");

    fn optional_card(call: &str, at: f64) -> Value {
        json!({"question_id":format!("codex-async:{call}:0"),"source":"codex_async","optional":true,
            "created_at":at,"question_hash":format!("h-{call}")})
    }

    #[test]
    fn optional_questions_follow_the_shared_contract() {
        let contract: Value = serde_json::from_str(OPTIONAL_CONTRACT).unwrap();
        for example in contract["listing"].as_array().unwrap() {
            let questions = example["questions"].as_array().map(Vec::as_slice);
            assert_eq!(optional_questions(questions), example["optional_questions"], "{}", example["name"]);
        }
        // Every row desktops and phones test against has a shape hgs lists.
        let sequences = contract["desktop_sequences"].as_array().unwrap();
        let rows = contract["cases"].as_array().unwrap().iter()
            .chain(sequences.iter().flat_map(|sequence| sequence["steps"].as_array().unwrap()))
            .map(|row| &row["raw"]);
        for listed in rows.filter_map(|raw| raw.get("optional_questions")).filter(|listed| !listed.is_null()) {
            let count = listed["count"].as_u64().unwrap();
            if count == 0 {
                assert_eq!(listed, &json!({"count": 0}));
                continue;
            }
            assert_eq!(listed.as_object().unwrap().len(), 3, "{listed}");
            let ids: Vec<&str> = listed["ids"].as_array().unwrap().iter().map(|id| id.as_str().unwrap()).collect();
            assert_eq!(ids.len(), (count as usize).min(OPTIONAL_IDS), "{listed}");
            assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), ids.len(), "{listed}");
            assert!(listed["latest_at"].is_null() || listed["latest_at"].as_f64().is_some_and(|at| at > 0.0));
        }
    }

    #[test]
    fn optional_questions_list_the_newest_ids_with_an_exact_count() {
        let cards: Vec<Value> = (0..40).rev().map(|index| optional_card(&format!("call-{index:02}"), 100.0 + f64::from(index))).collect();
        let listed = optional_questions(Some(&cards));
        assert_eq!(listed["count"], 40);
        let ids: Vec<String> = (8..40).map(|index| format!("codex-async:call-{index:02}:0")).collect();
        assert_eq!(listed["ids"], json!(ids));
        assert_eq!(listed["latest_at"], 139.0);
    }

    #[test]
    fn unreadable_question_indexes_list_unknown_optional_questions_without_new_attention() {
        let mut working = waiting();
        working["phase"] = json!("working");
        let pending = vec![optional_card("call-a", 100.0)];
        let listed = |questions: Result<Option<Vec<Value>>>| {
            let mut session = working.clone();
            enrich_listing(&mut session, &json!({}), "codex/project/review", questions);
            (session["optional_questions"].clone(), session["attention_signature"].clone())
        };
        let (known, asked) = listed(Ok(Some(pending.clone())));
        assert_eq!(known, json!({"count":1,"ids":["codex-async:call-a:0"],"latest_at":100.0}));
        assert_eq!(asked, json!(signature(&working, Some(&pending))));
        assert_eq!(listed(Ok(Some(Vec::new()))), (json!({"count": 0}), Value::Null));
        // An index read error adds no attention, as before, but is no count of 0.
        assert_eq!(listed(Err("Conversation transcript identity mismatch".into())), (Value::Null, Value::Null));
        // An index not complete in time is unknown in both fields.
        let (unknown, digest) = listed(Ok(None));
        assert!(unknown.is_null());
        assert_eq!(digest, json!(signature(&working, None)));
    }

    #[test]
    fn an_unreadable_question_index_keeps_requests_acknowledgeable() {
        // Optional questions are unknown (null) both while the index catches up
        // and when it cannot be read; only the first signature is unacknowledgeable.
        let mut unreadable = waiting();
        enrich_listing(&mut unreadable, &json!({}), "codex/project/review", Err("Conversation transcript identity mismatch".into()));
        assert!(unreadable["optional_questions"].is_null());
        let digest = unreadable["attention_signature"].as_str().unwrap().to_owned();
        assert_eq!(digest, signature(&waiting(), Some(&[])).unwrap());
        // `hgs read` sees the same unreadable index as no questions and applies it.
        let (_, attention, read) = apply(&waiting(), Some(&[]), &request(None, Some(&digest)), &Value::Null, 1.0);
        assert!(attention);
        assert_eq!(read["attention_signature"], json!(digest));
        let mut catching_up = waiting();
        enrich_listing(&mut catching_up, &json!({}), "codex/project/review", Ok(None));
        assert!(catching_up["optional_questions"].is_null());
        let unknown = catching_up["attention_signature"].as_str().unwrap().to_owned();
        assert_ne!(unknown, digest);
        assert!(!apply(&waiting(), None, &request(None, Some(&unknown)), &Value::Null, 1.0).1);
    }

    #[test]
    fn identity_less_errors_are_distinct_per_turn() {
        let mut failed = waiting();
        failed["phase"] = json!("error");
        failed["attention_id"] = json!("");
        let first = signature(&failed, Some(&[])).unwrap();
        failed["last_event_at"] = json!(500.0);
        assert_eq!(signature(&failed, Some(&[])).unwrap(), first);
        failed["turn_started"] = json!(6.0);
        assert_ne!(signature(&failed, Some(&[])).unwrap(), first);
    }

    #[test]
    fn identity_less_requests_are_distinct_per_request() {
        for phase in ["approval", "input"] {
            // A permission prompt without a tool call, or a tool keyed by its name.
            for id in ["", "request_user_input"] {
                let mut asked = waiting();
                asked["phase"] = json!(phase);
                asked["attention_id"] = json!(id);
                asked["attention_started"] = json!(20.0);
                let first = signature(&asked, Some(&[])).unwrap();
                asked["last_event_at"] = json!(500.0);
                asked["activity_summary"] = json!("Still waiting");
                assert_eq!(signature(&asked, Some(&[])).unwrap(), first, "{phase} {id:?}");
                asked["attention_started"] = json!(30.0);
                assert_ne!(signature(&asked, Some(&[])).unwrap(), first, "{phase} {id:?}");
            }
        }
        let mut child = waiting();
        child["phase"] = json!("working");
        child["subagents"] = json!({"external:kimi:session_child":{"state":"working","display_state":"approval",
            "attention_id":"request:20.0","updated":20.0}});
        let first = signature(&child, Some(&[])).unwrap();
        child["subagents"]["external:kimi:session_child"]["updated"] = json!(25.0);
        assert_eq!(signature(&child, Some(&[])).unwrap(), first);
        child["subagents"]["external:kimi:session_child"]["attention_id"] = json!("request:30.0");
        assert_ne!(signature(&child, Some(&[])).unwrap(), first);
    }

    #[test]
    fn unindexed_questions_are_never_acknowledged() {
        let known = signature(&waiting(), Some(&[])).unwrap();
        let unknown = signature(&waiting(), None).unwrap();
        assert!(hex(&unknown));
        assert_ne!(unknown, known);
        // Unknown questions may be pending even while the agent works.
        let mut working = waiting();
        working["phase"] = json!("working");
        assert_eq!(signature(&working, Some(&[])), None);
        assert!(signature(&working, None).is_some());
        // `hgs read` that could not index them acknowledges neither value.
        for digest in [&known, &unknown] {
            let (_, attention, kept) = apply(&waiting(), None, &request(None, Some(digest.as_str())), &Value::Null, 1.0);
            assert!(!attention);
            assert!(kept.is_null());
        }
        let (_, attention, _) = apply(&waiting(), Some(&[]), &request(None, Some(known.as_str())), &Value::Null, 1.0);
        assert!(attention);
    }

    #[test]
    fn records_are_found_by_owner_and_conversation_only() {
        let marks = json!({"codex/project/original":{"conversation-1":{"conversation_id":"conversation-1",
            "reply_id":"7:100.5","attention_signature":null,"at":1.0}}});
        assert_eq!(record(&marks, "codex/project/original", "conversation-1")["reply_id"], "7:100.5");
        assert!(record(&marks, "codex/project/original", "conversation-2").is_null());
        assert!(record(&marks, "codex/project/original", "").is_null());
        let mut session = waiting();
        enrich(&mut session, &marks, "codex/project/original", Some(&[]));
        assert_eq!(session["read"]["reply_id"], "7:100.5");
        assert!(hex(session["attention_signature"].as_str().unwrap()));
        enrich(&mut session, &marks, "codex/project/other-owner", Some(&[]));
        assert!(session["read"].is_null());
    }

    fn request(reply: Option<&str>, signature: Option<&str>) -> Request {
        Request { run: "run-1".into(), conversation: "conversation-1".into(),
            reply: reply.map(Into::into), signature: signature.map(Into::into) }
    }

    #[test]
    fn requests_are_exact() {
        let valid = json!({"run_id":"run-1","conversation_id":"conversation-1","reply_id":null,
            "attention_signature":"a".repeat(64)});
        assert_eq!(parse(&valid).unwrap(), request(None, Some("a".repeat(64).as_str())));
        for (key, value) in [("run_id", json!("")), ("conversation_id", json!(3)), ("reply_id", json!("x\ny")),
            ("attention_signature", json!("A".repeat(64))), ("attention_signature", json!("a".repeat(63)))] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(parse(&invalid).is_err(), "{key}");
        }
        let mut extra = valid.clone();
        extra["request_id"] = json!("fixture");
        assert!(parse(&extra).is_err());
        let mut missing = valid;
        missing.as_object_mut().unwrap().remove("reply_id");
        assert!(parse(&missing).is_err());
    }

    #[test]
    fn marks_move_only_to_current_values() {
        let mut current = waiting();
        current["reply_id"] = json!("7:100.5");
        let digest = signature(&current, Some(&[])).unwrap();
        let (reply, attention, mark) = apply(&current, Some(&[]), &request(Some("7:100.5"), Some(digest.as_str())), &Value::Null, 50.0);
        assert!(reply && attention);
        assert_eq!(mark, json!({"conversation_id":"conversation-1","reply_id":"7:100.5","attention_signature":digest,"at":50.0}));
        // Older values are stale and keep the stored mark.
        let older_request = request(Some("6:90.5"), Some("b".repeat(64).as_str()));
        let (reply, attention, kept) = apply(&current, Some(&[]), &older_request, &mark, 60.0);
        assert!(!reply && !attention);
        assert_eq!(kept, mark);
        // Another run or conversation makes every field stale.
        let mut other = request(Some("7:100.5"), Some(digest.as_str()));
        other.run = "run-0".into();
        assert_eq!(apply(&current, Some(&[]), &other, &Value::Null, 70.0), (false, false, Value::Null));
        // A partial mark keeps the other field.
        let older = json!({"conversation_id":"conversation-1","reply_id":"6:90.5","attention_signature":null,"at":1.0});
        let (reply, attention, partial) = apply(&current, Some(&[]), &request(None, Some(digest.as_str())), &older, 80.0);
        assert!(!reply && attention);
        assert_eq!((partial["reply_id"].clone(), partial["at"].clone()), (json!("6:90.5"), json!(80.0)));
    }

    #[test]
    fn storage_keeps_the_newest_conversations_of_each_session() {
        let mut conversations = serde_json::Map::new();
        for index in 0..10 {
            conversations.insert(format!("conversation-{index}"), json!({"at": index as f64}));
        }
        bounded(&mut conversations);
        assert_eq!(conversations.len(), MAX_CONVERSATIONS);
        assert!(!conversations.contains_key("conversation-0") && !conversations.contains_key("conversation-1"));
        assert!(conversations.contains_key("conversation-9"));
    }
}
