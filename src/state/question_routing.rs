//! Whether a supervised child's blocking questions go to its parent session
//! (zerus-sa9qn.32, slice b of zerus-sa9qn.8): one switch per child session,
//! kept on the child's computer beside the shared notification preference so
//! every desktop and phone sees the same value. It is on by default, keyed by
//! the child's durable session ID, so a rename or a new conversation keeps
//! it. A change from an agent's own process tree is refused, by this CLI
//! before any `@host` forwarding and again on the switch's own computer.
//! That refusal is a guard against accidental agent use, not a security
//! boundary (zerus-sa9qn.49): a same-user agent can clear its session
//! variables, leave its process tree, or write this file directly. The
//! person-only control is the global switch, changed only through the
//! desktop's native frontend lane (`Request::QuestionRoutes`).
//!
//! The desktop service reads it again inside each route reservation
//! ([`send_to_parent_at`]); a change written here before that read always
//! stops the reservation, and turning it off never cancels or replays a
//! question already reserved, sent or unknown.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;

/// Sessions with a stored switch. An entry is kept once written, so its
/// revision never goes back.
const MAX_ENTRIES: usize = 4096;
const MAX_FILE: u64 = 1024 * 1024;
const MAX_REVISION: u64 = i64::MAX as u64;
const USAGE: &str = "usage: hgs question-routing <session> --json (JSON stdin: expected_run_id, expected_conversation_id, expected_revision, send_to_parent)";
pub(crate) const FILE: &str = "question-routing.json";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    send_to_parent: bool,
    revision: u64,
}
impl Default for Entry {
    fn default() -> Self {
        Self {
            send_to_parent: true,
            revision: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Store {
    version: u8,
    sessions: BTreeMap<String, Entry>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: BTreeMap::new(),
        }
    }
}
impl Store {
    /// The switch of `session_id`: its stored entry, or on at revision 0.
    fn entry(&self, session_id: &str) -> Entry {
        self.sessions.get(session_id).copied().unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    expected_run_id: String,
    expected_conversation_id: String,
    expected_revision: u64,
    send_to_parent: bool,
}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn parse(bytes: &[u8]) -> Result<Request> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| "invalid question routing request")?;
    if !identity(&request.expected_run_id)
        || !identity(&request.expected_conversation_id)
        || request.expected_revision > MAX_REVISION
    {
        return Err("invalid question routing request".into());
    }
    Ok(request)
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

fn lock_path() -> PathBuf {
    root().join("question-routing.lock")
}

fn read_store(path: &Path) -> Result<Store> {
    let Some(bytes) = keep_warm::read_regular(path, MAX_FILE)? else {
        return Ok(Store::default());
    };
    let store: Store = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid question routing settings: {e}"))?;
    if store.version != 1
        || store.sessions.len() > MAX_ENTRIES
        || store
            .sessions
            .iter()
            .any(|(id, entry)| !session_id::canonical(id) || entry.revision > MAX_REVISION)
    {
        return Err("invalid question routing settings".into());
    }
    Ok(store)
}

fn persist(path: &Path, store: &Store) -> Result<()> {
    let serialized = serde_json::to_string(store).map_err(|e| e.to_string())?;
    if serialized.len() as u64 >= MAX_FILE {
        return Err("question routing settings exceed limits".into());
    }
    atomic(path, &format!("{serialized}\n"))
}

/// The store, or None when it is unreadable: unknown, never a default "on".
pub(super) fn load() -> Option<Store> {
    read_store(&path(&root())).ok()
}

/// Whether the child `session_id` sends its questions to its parent, read
/// from the CLI state root `state` (the desktop service's own): `None` when
/// the setting is unreadable or the ID is not a session ID, which never
/// routes.
pub(crate) fn send_to_parent_at(state: &Path, session_id: &str) -> Option<bool> {
    if !session_id::canonical(session_id) {
        return None;
    }
    read_store(&path(state))
        .ok()
        .map(|store| store.entry(session_id).send_to_parent)
}

fn view(entry: Entry) -> Value {
    json!({"send_to_parent": entry.send_to_parent, "revision": entry.revision})
}

/// The `question_routing` object of a listing row: null when the setting is
/// unreadable or the row has no durable session ID.
pub(super) fn enrich(session: &mut Value, store: Option<&Store>) {
    let id = string(session, "session_id").to_owned();
    session["question_routing"] = match store {
        Some(store) if session_id::canonical(&id) => view(store.entry(&id)),
        _ => Value::Null,
    };
}

/// The pure compare-and-swap of one session's switch: the exit code, the
/// reply and whether the store changed.
fn update(store: &mut Store, record: &Value, request: &Request) -> (i32, Value, bool) {
    let id = string(record, "session_id");
    let run = string(record, "run_id");
    let conversation = string(record, "conversation_id");
    let current = store.entry(id);
    let reply = |ok: bool, error: Option<&str>, applied: bool, entry: Entry| {
        let mut value = json!({"ok": ok, "run_id": run, "conversation_id": conversation,
            "question_routing": view(entry)});
        if let Some(error) = error {
            value["error"] = json!(error);
        } else {
            value["applied"] = json!(applied);
        }
        value
    };
    if run != request.expected_run_id || conversation != request.expected_conversation_id {
        return (
            1,
            reply(false, Some("stale_identity"), false, current),
            false,
        );
    }
    if request.expected_revision != current.revision {
        return (1, reply(false, Some("conflict"), false, current), false);
    }
    if current.send_to_parent == request.send_to_parent {
        return (0, reply(true, None, false, current), false);
    }
    if !store.sessions.contains_key(id) && store.sessions.len() >= MAX_ENTRIES {
        return (1, reply(false, Some("capacity"), false, current), false);
    }
    let Some(revision) = current
        .revision
        .checked_add(1)
        .filter(|r| *r <= MAX_REVISION)
    else {
        return (
            1,
            reply(false, Some("revision_exhausted"), false, current),
            false,
        );
    };
    let next = Entry {
        send_to_parent: request.send_to_parent,
        revision,
    };
    store.sessions.insert(id.to_owned(), next);
    (0, reply(true, None, true, next), true)
}

/// The service's write after the person's verified latch (zerus-sa9qn.50):
/// the child's switch at the state root `state` becomes `send_to_parent`,
/// under the switch's lock. Only the service calls this, after the person's
/// own request; the latch, not this file, is the person's decision.
pub(crate) fn set_for_person(state: &Path, session_id: &str, send_to_parent: bool) -> Result<bool> {
    if !session_id::canonical(session_id) {
        return Err("invalid question routing session".into());
    }
    let _guard = lock(Some(&state.join("question-routing.lock")))?;
    let mut store = read_store(&path(state))?;
    let current = store.entry(session_id);
    if current.send_to_parent == send_to_parent {
        return Ok(false);
    }
    if !store.sessions.contains_key(session_id) && store.sessions.len() >= MAX_ENTRIES {
        return Err("question routing settings are full".into());
    }
    let revision = current
        .revision
        .checked_add(1)
        .filter(|r| *r <= MAX_REVISION)
        .ok_or("question routing revisions are exhausted")?;
    store.sessions.insert(
        session_id.to_owned(),
        Entry {
            send_to_parent,
            revision,
        },
    );
    persist(&path(state), &store)?;
    Ok(true)
}

/// `hgs question-routing <session> --json`: one compare-and-swap of the
/// child's switch from JSON stdin. Refused from an agent's process tree.
pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let [name, flag] = args else {
        return Err(USAGE.into());
    };
    if flag != "--json" || name.is_empty() || name.starts_with(['-', '@']) {
        return Err(USAGE.into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("question routing request exceeds limits".into());
    }
    let request = parse(&bytes)?;
    // Refuse an agent's own process tree, the parent or the child itself
    // (zerus-sa9qn.32): a guard against accidental agent use, not a boundary
    // (zerus-sa9qn.49). `hgs @host` refuses before forwarding (cli.rs).
    if session_graph::caller_record().is_some() {
        println!("{}", json!({"ok": false, "error": "agent_refused"}));
        return Ok(1);
    }
    expect(
        name,
        &request.expected_run_id,
        &request.expected_conversation_id,
    )?;
    let Some(record) = notifications::current(name)? else {
        println!("{}", json!({"ok": false, "error": "not_found"}));
        return Ok(1);
    };
    if !session_id::canonical(string(&record, "session_id")) {
        println!("{}", json!({"ok": false, "error": "unsupported"}));
        return Ok(1);
    }
    let (code, output) = {
        let _guard = lock(Some(&lock_path()))?;
        // The session again, under the lock: a rename or another run since
        // the first read is never a change of this switch.
        let current = notifications::current(name)?;
        let same = current.as_ref().is_some_and(|now| {
            ["session_id", "run_id", "conversation_id"]
                .iter()
                .all(|key| string(now, key) == string(&record, key))
        });
        if !same {
            println!("{}", json!({"ok": false, "error": "stale_identity"}));
            return Ok(1);
        }
        let mut store = read_store(&path(&root()))?;
        let (code, output, changed) = update(&mut store, &record, &request);
        if changed {
            persist(&path(&root()), &store)?;
        }
        (code, output)
    };
    println!("{output}");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({"session_id": "11111111-1111-4111-8111-111111111111", "run_id": "run-1",
            "conversation_id": "conversation-1"})
    }
    fn request(revision: u64, send: bool) -> Request {
        Request {
            expected_run_id: "run-1".into(),
            expected_conversation_id: "conversation-1".into(),
            expected_revision: revision,
            send_to_parent: send,
        }
    }

    #[test]
    fn the_switch_is_on_by_default_and_changes_only_by_its_current_revision() {
        let mut store = Store::default();
        let id = "11111111-1111-4111-8111-111111111111";
        assert_eq!(
            store.entry(id),
            Entry {
                send_to_parent: true,
                revision: 0
            }
        );
        // Already on: nothing changes.
        let (code, reply, changed) = update(&mut store, &record(), &request(0, true));
        assert_eq!(
            (code, reply["applied"].clone(), changed),
            (0, json!(false), false)
        );
        let (code, reply, changed) = update(&mut store, &record(), &request(0, false));
        assert_eq!((code, changed), (0, true));
        assert_eq!(
            reply["question_routing"],
            json!({"send_to_parent": false, "revision": 1})
        );
        // A second desktop with the old revision is refused, not merged.
        let (code, reply, changed) = update(&mut store, &record(), &request(0, true));
        assert_eq!(
            (code, reply["error"].clone(), changed),
            (1, json!("conflict"), false)
        );
        // Another run or conversation is a stale identity.
        let mut other = request(1, true);
        other.expected_run_id = "run-2".into();
        let (code, reply, _) = update(&mut store, &record(), &other);
        assert_eq!((code, reply["error"].clone()), (1, json!("stale_identity")));
        let (code, reply, changed) = update(&mut store, &record(), &request(1, true));
        assert_eq!((code, changed), (0, true));
        assert_eq!(
            reply["question_routing"],
            json!({"send_to_parent": true, "revision": 2})
        );
        // The entry stays, so its revision never goes back.
        assert_eq!(store.entry(id).revision, 2);
    }

    #[test]
    fn the_store_is_bounded_and_strict() {
        let mut store = Store::default();
        for n in 0..MAX_ENTRIES {
            store.sessions.insert(
                format!("11111111-1111-4111-8111-{n:012}"),
                Entry {
                    send_to_parent: false,
                    revision: 1,
                },
            );
        }
        let mut full = record();
        full["session_id"] = json!("22222222-2222-4222-8222-222222222222");
        let (code, reply, changed) = update(&mut store, &full, &request(0, false));
        assert_eq!(
            (code, reply["error"].clone(), changed),
            (1, json!("capacity"), false)
        );
        let temp = tempfile::tempdir().unwrap();
        for bad in [
            r#"{"version":2,"sessions":{}}"#,
            r#"{"version":1,"sessions":{"name":{"send_to_parent":false,"revision":1}}}"#,
            r#"{"version":1,"sessions":{},"extra":1}"#,
            r#"{"version":1,"sessions":{"11111111-1111-4111-8111-111111111111":{"send_to_parent":false,"revision":1,"x":1}}}"#,
        ] {
            std::fs::write(temp.path().join(FILE), bad).unwrap();
            assert!(read_store(&temp.path().join(FILE)).is_err(), "{bad}");
            // Unreadable is unknown, which never routes.
            assert_eq!(
                send_to_parent_at(temp.path(), "11111111-1111-4111-8111-111111111111"),
                None
            );
        }
    }

    #[test]
    fn the_service_reads_the_current_value_and_a_listing_row_shows_it() {
        let temp = tempfile::tempdir().unwrap();
        let id = "11111111-1111-4111-8111-111111111111";
        assert_eq!(
            send_to_parent_at(temp.path(), id),
            Some(true),
            "on by default"
        );
        assert_eq!(send_to_parent_at(temp.path(), "claude/x/y"), None);
        let mut store = Store::default();
        update(&mut store, &record(), &request(0, false));
        persist(&path(temp.path()), &store).unwrap();
        assert_eq!(send_to_parent_at(temp.path(), id), Some(false));
        let mut row = json!({"session_id": id});
        enrich(&mut row, Some(&store));
        assert_eq!(
            row["question_routing"],
            json!({"send_to_parent": false, "revision": 1})
        );
        enrich(&mut row, None);
        assert_eq!(row["question_routing"], Value::Null, "unknown storage");
        let mut legacy = json!({"name": "claude/x/y"});
        enrich(&mut legacy, Some(&store));
        assert_eq!(
            legacy["question_routing"],
            Value::Null,
            "no durable session ID"
        );
    }
}
