//! Phase 5/6 route, store and mediation tests with the synthetic fixture
//! people. The current process plays Alice (its OS identity owns the temporary
//! folders); Bob and Carol are other OS identities.
use super::compute;
use super::gate::{self, Admission};
use super::runtime::{self, Mode, OsIdentity, Principal, Trust};
use super::shared::{self, Kind, Store};
use super::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts/host-access-v1/fixtures").join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn policy_text(name: &str) -> String {
    let policies = fixture("policies.json");
    policies["valid"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap()["text"].as_str().unwrap().to_owned()
}
fn host() -> String {
    fixture("policies.json")["host"].as_str().unwrap().to_owned()
}
fn person(name: &str) -> String {
    let keys = fixture("synthetic-keys.json");
    keys["persons"].as_array().unwrap().iter().find(|p| p["name"] == name).unwrap()["person"].as_str().unwrap().to_owned()
}
fn me() -> OsIdentity {
    OsIdentity::current().unwrap()
}
fn other_os(n: u32) -> OsIdentity {
    OsIdentity::parse(&if cfg!(windows) { format!("S-1-5-21-111-222-333-{n}") } else { format!("uid:{n}") }).unwrap()
}

struct Host {
    _temp: tempfile::TempDir,
    dir: PathBuf,
}

fn operator(policy: &str, bindings: Value) -> Host {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("host-access");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("host-id"), host()).unwrap();
    std::fs::write(dir.join("policy.json"), policy).unwrap();
    std::fs::write(dir.join("os-bindings.json"), bindings.to_string()).unwrap();
    Host { _temp: temp, dir }
}


/// Loads `host` for `os` with the caller state folder kept beside it.
fn load(host: &Host, os: &OsIdentity, trust: Trust) -> Mode {
    runtime::load(&host.dir, os, trust, &host.dir.parent().unwrap().join("caller-state"))
}

fn bindings(pairs: &[(&OsIdentity, &str)]) -> Value {
    json!({"contract":runtime::BINDINGS_CONTRACT,"host":host(),"revision":1,
        "bindings":pairs.iter().map(|(os, context)| json!({"context":context,"os_identity":os.0})).collect::<Vec<_>>()})
}

fn alice_host() -> Host {
    operator(&policy_text("base"), bindings(&[(&me(), "alice-worker"), (&other_os(4242), "bob-worker")]))
}

fn bound(mode: &Mode) -> (&Policy, &Principal) {
    match mode {
        Mode::Bound { policy, principal } => (policy, principal),
        other => panic!("not bound: {other:?}"),
    }
}

fn verified(name: &str) -> Policy {
    verify(policy_text(name).as_bytes(), &host()).unwrap()
}

fn principal(name: &str, os: OsIdentity, context: &str) -> Principal {
    Principal { person: person(name), context: context.into(), os }
}

// ------------------------------------------------------------------ runtime

#[test]
fn no_policy_is_the_legacy_mode() {
    let temp = tempfile::tempdir().unwrap();
    assert!(matches!(runtime::load(temp.path(), &me(), Trust::TestOwned, &temp.path().join("state")), Mode::Legacy));
}

#[test]
fn the_caller_is_derived_from_the_os_binding_alone() {
    let host = alice_host();
    let mode = load(&host, &me(), Trust::TestOwned);
    let (_, principal) = bound(&mode);
    assert_eq!(principal.person, person("alice"));
    assert_eq!(principal.context, "alice-worker");
    let bob = load(&host, &other_os(4242), Trust::TestOwned);
    assert_eq!(bound(&bob).1.person, person("bob"));
    assert!(matches!(load(&host, &other_os(7), Trust::TestOwned), Mode::Unbound { .. }));
}

#[test]
fn an_untrustworthy_policy_breaks_the_host_instead_of_falling_back() {
    let cases: Vec<(Host, &str)> = vec![
        (operator("{", bindings(&[(&me(), "alice-worker")])), "policy-malformed"),
        (operator(&policy_text("base"), json!({"contract":"other"})), "bindings-invalid"),
        (operator(&policy_text("base"), json!({"contract":runtime::BINDINGS_CONTRACT,"extra":1})), "bindings-malformed"),
        (operator(&policy_text("base"), json!({"contract":runtime::BINDINGS_CONTRACT,"host":"0".repeat(64),"revision":1,"bindings":[]})), "bindings-invalid"),
        (operator(&policy_text("base"), bindings(&[(&me(), "alice-worker"), (&me(), "bob-worker")])), "bindings-invalid"),
        (operator(&policy_text("base"), bindings(&[(&me(), "alice-worker"), (&other_os(1), "alice-worker")])), "bindings-invalid"),
        (operator(&policy_text("base"), bindings(&[(&me(), "nobody-worker")])), "binding-context-unknown"),
        (operator(&policy_text("bob-revoked"), bindings(&[(&me(), "bob-worker")])), "person-revoked"),
    ];
    for (host, code) in cases {
        match load(&host, &me(), Trust::TestOwned) {
            Mode::Broken(found) => assert_eq!(found, code),
            other => panic!("{code}: {other:?}"),
        }
    }
    let host = alice_host();
    std::fs::remove_file(host.dir.join("host-id")).unwrap();
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Broken("host-id-untrusted")));
}

#[test]
fn the_production_trust_check_refuses_a_caller_owned_operator_folder() {
    // The temporary folder belongs to this (non-operator) process.
    let host = alice_host();
    assert!(matches!(load(&host, &me(), Trust::Operator), Mode::Broken(_)));
    assert!(runtime::operator_owned_dir(&host.dir).is_err());
}

// --------------------------------------------------------------------- gate

const PRIVATE_ROUTES: &[&[&str]] = &[
    &["claude", "/work"], &["send", "s", "--json"], &["send-now", "s", "--json"], &["resume", "s"], &["fork", "s"],
    &["answer", "s"], &["session-action", "s", "--json"], &["kill", "s"], &["terminate", "s"], &["interrupt", "s"],
    &["switch-account", "s", "--json"], &["history", "s", "--json"], &["history-projection", "s", "--json"],
    &["terminal", "s"], &["inspect", "s"], &["ls"], &["account", "ls"], &["account", "inspect", "native-codex"],
    &["analytics", "usage"], &["archives", "--json"],
];

fn admit(mode: &Mode, route: &[&str], root: &Path) -> Result<Admission, gate::Refusal> {
    let args: Vec<String> = route[1..].iter().map(|s| s.to_string()).collect();
    gate::admit(mode, route[0], &args, root)
}

#[test]
fn a_legacy_host_admits_every_route_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    for route in PRIVATE_ROUTES.iter().chain([&["account", "copy", "x"][..], &["account", "_export", "x"]].iter()) {
        assert_eq!(admit(&Mode::Legacy, route, temp.path()).unwrap(), Admission::Legacy);
    }
}

#[test]
fn a_broken_policy_refuses_every_gated_route_but_help() {
    let temp = tempfile::tempdir().unwrap();
    let mode = Mode::Broken("policy-invalid");
    for route in PRIVATE_ROUTES {
        assert_eq!(admit(&mode, route, temp.path()).unwrap_err().code, "policy-broken", "{route:?}");
    }
    for route in [&["--help"][..], &["version"], &["host-access", "status", "--json"]] {
        assert!(admit(&mode, route, temp.path()).is_ok());
    }
}

#[test]
fn every_route_refuses_another_persons_state_folder() {
    let host = alice_host();
    let state = tempfile::tempdir().unwrap(); // owned by this process: Alice
    let bob = load(&host, &other_os(4242), Trust::TestOwned);
    let stranger = load(&host, &other_os(7), Trust::TestOwned);
    for route in PRIVATE_ROUTES {
        assert_eq!(admit(&bob, route, state.path()).unwrap_err().code, "state-owner", "{route:?}");
        assert_eq!(admit(&stranger, route, state.path()).unwrap_err().code, "state-owner", "{route:?}");
    }
    let alice = load(&host, &me(), Trust::TestOwned);
    for route in PRIVATE_ROUTES {
        assert!(matches!(admit(&alice, route, state.path()).unwrap(), Admission::Bound(_)), "{route:?}");
    }
}

#[test]
fn credential_transfer_is_refused_for_a_bound_person_only() {
    let host = alice_host();
    let state = tempfile::tempdir().unwrap();
    let alice = load(&host, &me(), Trust::TestOwned);
    for op in ["copy", "_export", "_import"] {
        assert_eq!(admit(&alice, &["account", op, "native-codex"], state.path()).unwrap_err().code, "account-transfer-unsupported");
    }
    let host = operator(&policy_text("base"), bindings(&[(&other_os(4242), "bob-worker")]));
    let unbound = load(&host, &me(), Trust::TestOwned);
    assert_eq!(admit(&unbound, &["account", "copy", "native-codex"], state.path()).unwrap(), Admission::Private);
}

#[test]
fn a_state_folder_bound_to_someone_else_keeps_its_sessions_theirs() {
    let host = alice_host();
    let state = tempfile::tempdir().unwrap();
    let alice = load(&host, &me(), Trust::TestOwned);
    let binding = gate::bind(&alice, state.path(), 100).unwrap();
    assert_eq!(binding.person, person("alice"));
    // Idempotent for the same principal.
    assert_eq!(gate::bind(&alice, state.path(), 200).unwrap(), binding);
    // The operator rebinds this OS account to Bob's context.
    let rebound = operator(&policy_text("base"), bindings(&[(&me(), "bob-worker")]));
    let bob_here = load(&rebound, &me(), Trust::TestOwned);
    for route in PRIVATE_ROUTES {
        assert_eq!(admit(&bob_here, route, state.path()).unwrap_err().code, "owner-mismatch", "{route:?}");
    }
    assert_eq!(gate::bind(&bob_here, state.path(), 300).unwrap_err().code, "owner-mismatch");
    assert_eq!(gate::require_bound(&bob_here, state.path()).unwrap_err().code, "owner-mismatch");
}

#[test]
fn shared_routes_need_an_explicit_binding_and_historical_state_stays_private() {
    let host = alice_host();
    let state = tempfile::tempdir().unwrap();
    let alice = load(&host, &me(), Trust::TestOwned);
    // Unbound history: private use continues, sharing is refused.
    assert!(admit(&alice, &["history", "s", "--json"], state.path()).is_ok());
    assert_eq!(gate::require_bound(&alice, state.path()).unwrap_err().code, "binding-required");
    gate::bind(&alice, state.path(), 1).unwrap();
    assert_eq!(gate::require_bound(&alice, state.path()).unwrap().person, person("alice"));
    let unbound = load(&host, &other_os(7), Trust::TestOwned);
    assert_eq!(gate::require_bound(&unbound, state.path()).unwrap_err().code, "binding-required");
    assert_eq!(gate::require_bound(&Mode::Legacy, state.path()).unwrap_err().code, "multi-person-disabled");
    let corrupt = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(corrupt.path().join("host-access")).unwrap();
    std::fs::write(gate::binding_path(corrupt.path()), "{}").unwrap();
    assert_eq!(admit(&alice, &["ls"], corrupt.path()).unwrap_err().code, "binding-malformed");
}

#[test]
fn identical_account_labels_in_two_scopes_never_collide() {
    let a = AccountRef::new(&person("alice"), "alice-worker", "codex", "native-codex").unwrap();
    let b = AccountRef::new(&person("bob"), "bob-worker", "codex", "native-codex").unwrap();
    assert_ne!(a, b);
    assert_ne!(a.key(), b.key());
}

#[test]
fn the_status_view_carries_no_key_material() {
    let host = alice_host();
    let state = tempfile::tempdir().unwrap();
    let alice = load(&host, &me(), Trust::TestOwned);
    let view = command::status(&alice, state.path()).to_string();
    for forbidden in ["public_key", "signature", "seed", "os_identity", &me().0] {
        assert!(!view.contains(forbidden), "{forbidden}");
    }
    assert!(view.contains("proj-shared"));
}

#[test]
fn the_capability_is_advertised_in_help() {
    assert_eq!(gate::MARKER, "Multi-person access ABI: multi-person-access-v1");
    assert!(gate::Refusal::new("x", "y").message().starts_with("host-access/x: "));
}

// ------------------------------------------------------------------- shared

struct Shared {
    _temp: tempfile::TempDir,
    root: PathBuf,
    alice_state: PathBuf,
    bob_state: PathBuf,
}

fn shared_root() -> Shared {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("shared");
    for project in ["proj-shared", "proj-read", "proj-alice"] {
        std::fs::create_dir_all(root.join(project).join(person("alice"))).unwrap();
    }
    let alice_state = temp.path().join("alice-state");
    let bob_state = temp.path().join("bob-state");
    std::fs::create_dir_all(&alice_state).unwrap();
    std::fs::create_dir_all(&bob_state).unwrap();
    Shared { _temp: temp, root, alice_state, bob_state }
}

fn store<'a>(shared: &Shared, policy: &'a Policy, principal: &'a Principal, state: &Path) -> Store<'a> {
    Store { shared_root: shared.root.clone(), state_root: state.to_path_buf(), policy, principal, now: 1_795_000_000 }
}

fn rid() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

fn publish(store: &Store, project: &str, title: &str) -> Result<Value, gate::Refusal> {
    let body = json!({"request_id":rid(),"project":project,"title":title,"session":"claude/x",
        "entries":[{"role":"person","text":"Plan the parser"},{"role":"agent","text":"Done: parser.rs"}]}).to_string();
    let request: shared::PublishRequest = serde_json::from_str(&body).unwrap();
    store.publish(&request, body.as_bytes(), "session-1")
}

fn list(store: &Store, project: &str, cursor: Option<&str>, limit: usize) -> Result<Value, gate::Refusal> {
    let request: shared::ListRequest =
        serde_json::from_value(json!({"project":project,"kind":"history","cursor":cursor,"limit":limit})).unwrap();
    store.list(&request)
}

#[test]
fn members_read_published_history_and_others_are_refused() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let bob = principal("bob", other_os(4242), "bob-worker");
    let carol = principal("carol", other_os(9), "carol-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    let published = publish(&alice_store, "proj-shared", "Parser plan").unwrap();
    let id = published["id"].as_str().unwrap();
    let bob_store = store(&shared, &policy, &bob, &shared.bob_state);
    let page = list(&bob_store, "proj-shared", None, 10).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let read: shared::ReadRequest =
        serde_json::from_value(json!({"project":"proj-shared","kind":"history","person":person("alice"),"id":id})).unwrap();
    let record = bob_store.read(&read).unwrap();
    assert_eq!(record["read_only"], true);
    let text = record.to_string();
    for forbidden in ["agent_home", "account", "conversation", "run_id", "session-1", "claude/x", "credential"] {
        assert!(!text.contains(forbidden), "{forbidden} leaked");
    }
    // Carol is in no project; Bob is not in proj-alice.
    let carol_store = store(&shared, &policy, &carol, &shared.bob_state);
    assert_eq!(list(&carol_store, "proj-shared", None, 10).unwrap_err().code, "not-a-member");
    assert_eq!(carol_store.read(&read).unwrap_err().code, "not-a-member");
    assert_eq!(list(&bob_store, "proj-alice", None, 10).unwrap_err().code, "not-a-member");
    assert_eq!(list(&bob_store, "no-such", None, 10).unwrap_err().code, "project-unknown");
}

#[test]
fn a_revoked_reader_loses_access_on_the_next_call_even_with_a_cursor() {
    let shared = shared_root();
    let base = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let bob = principal("bob", other_os(4242), "bob-worker");
    publish(&store(&shared, &base, &alice, &shared.alice_state), "proj-shared", "One").unwrap();
    let page = list(&store(&shared, &base, &bob, &shared.bob_state), "proj-shared", None, 10).unwrap();
    let revoked = verified("bob-revoked");
    let bob_store = store(&shared, &revoked, &bob, &shared.bob_state);
    assert_eq!(list(&bob_store, "proj-shared", page["cursor"].as_str(), 10).unwrap_err().code, "person-revoked");
}

#[test]
fn readers_cannot_publish_or_continue() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    assert_eq!(publish(&alice_store, "proj-read", "No").unwrap_err().code, "role-insufficient");
    let request: shared::ContinueRequest = serde_json::from_value(json!({"request_id":rid(),"project":"proj-read",
        "person":person("bob"),"id":rid(),"generation":1})).unwrap();
    assert_eq!(alice_store.continue_with_own_account(&request, b"x").unwrap_err().code, "role-insufficient");
}

#[test]
fn pages_are_bounded_stable_while_records_arrive_and_bound_to_the_policy_revision() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    for n in 0..3 {
        publish(&alice_store, "proj-shared", &format!("Record {n}")).unwrap();
    }
    let first = list(&alice_store, "proj-shared", None, 2).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    publish(&alice_store, "proj-shared", "Record 3").unwrap();
    let second = list(&alice_store, "proj-shared", first["cursor"].as_str(), 2).unwrap();
    let titles: Vec<&str> = second["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Record 2", "Record 3"]);
    let third = list(&alice_store, "proj-shared", second["cursor"].as_str(), 2).unwrap();
    assert!(third["items"].as_array().unwrap().is_empty());
    // A cursor from another revision is stale; a forged one is malformed.
    let mut newer = verified("base");
    newer.revision += 1;
    let newer_store = store(&shared, &newer, &alice, &shared.alice_state);
    assert_eq!(list(&newer_store, "proj-shared", first["cursor"].as_str(), 2).unwrap_err().code, "cursor-stale");
    assert_eq!(list(&alice_store, "proj-shared", Some("!!"), 2).unwrap_err().code, "cursor-invalid");
    // A huge limit is clamped to the page bound.
    assert!(list(&alice_store, "proj-shared", None, 1_000_000).is_ok());
}

#[test]
fn conflicting_updates_and_replayed_requests_are_exact() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    let first = publish(&alice_store, "proj-shared", "Draft").unwrap();
    let id = first["id"].as_str().unwrap().to_owned();
    let update = |expected: u64, request_id: &str, title: &str| {
        let body = json!({"request_id":request_id,"project":"proj-shared","id":id,"expected_generation":expected,
            "title":title,"session":"s","entries":[{"role":"note","text":"v2"}]}).to_string();
        let request: shared::PublishRequest = serde_json::from_str(&body).unwrap();
        alice_store.publish(&request, body.as_bytes(), "session-1")
    };
    let request_id = rid();
    let second = update(1, &request_id, "Second").unwrap();
    assert_eq!(second["generation"], 2);
    // The same request again returns its first result without a new generation.
    assert_eq!(update(1, &request_id, "Second").unwrap(), second);
    // The same request ID for another body is refused.
    assert_eq!(update(1, &request_id, "Other").unwrap_err().code, "request-conflict");
    // A writer that read generation 1 loses the race.
    assert_eq!(update(1, &rid(), "Stale").unwrap_err().code, "conflict");
    let page = list(&alice_store, "proj-shared", None, 10).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2, "one feed line per accepted generation");
}

#[test]
fn publication_schemas_refuse_authority_bearing_fields() {
    for extra in ["agent_home", "account_id", "conversation_id", "run_id", "resume", "auth"] {
        let mut body = json!({"request_id":rid(),"project":"proj-shared","title":"t","session":"s","entries":[{"role":"person","text":"x"}]});
        body[extra] = json!("x");
        assert!(serde_json::from_value::<shared::PublishRequest>(body).is_err(), "{extra}");
        let entry = json!({"request_id":rid(),"project":"proj-shared","title":"t","session":"s","entries":[{"role":"person","text":"x",extra:"y"}]});
        assert!(serde_json::from_value::<shared::PublishRequest>(entry).is_err(), "entry {extra}");
    }
}

#[test]
fn a_record_in_the_wrong_partition_fails_closed() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let bob = principal("bob", other_os(4242), "bob-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    let published = publish(&alice_store, "proj-shared", "Mine").unwrap();
    let id = published["id"].as_str().unwrap();
    // Copy Alice's record into a partition claiming to be Bob's.
    let from = shared.root.join("proj-shared").join(person("alice")).join("history").join(format!("{id}.json"));
    let to_dir = shared.root.join("proj-shared").join(person("bob")).join("history");
    std::fs::create_dir_all(&to_dir).unwrap();
    std::fs::copy(&from, to_dir.join(format!("{id}.json"))).unwrap();
    let read: shared::ReadRequest =
        serde_json::from_value(json!({"project":"proj-shared","kind":"history","person":person("bob"),"id":id})).unwrap();
    assert_eq!(store(&shared, &policy, &bob, &shared.bob_state).read(&read).unwrap_err().code, "record-owner-mismatch");
    // A partition not owned by the writer cannot be published to.
    let bob_store = store(&shared, &policy, &bob, &shared.bob_state);
    let code = publish(&bob_store, "proj-shared", "Bob").unwrap_err().code;
    assert!(code == "partition-owner" || code == "partition-missing", "{code}");
}

#[test]
fn continue_with_my_account_makes_a_private_handoff_once() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let bob = principal("bob", other_os(4242), "bob-worker");
    let published = publish(&store(&shared, &policy, &alice, &shared.alice_state), "proj-shared", "Parser").unwrap();
    let bob_store = store(&shared, &policy, &bob, &shared.bob_state);
    let body = json!({"request_id":rid(),"project":"proj-shared","person":person("alice"),
        "id":published["id"],"generation":1}).to_string();
    let request: shared::ContinueRequest = serde_json::from_str(&body).unwrap();
    let first = bob_store.continue_with_own_account(&request, body.as_bytes()).unwrap();
    assert_eq!(first["account"], "own");
    let again = bob_store.continue_with_own_account(&request, body.as_bytes()).unwrap();
    assert_eq!(first, again);
    let handoffs: Vec<_> = std::fs::read_dir(shared.bob_state.join("host-access/handoffs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".handoff.json"))
        .collect();
    assert_eq!(handoffs.len(), 1, "a replay never creates a second handoff");
    let handoff: Value = serde_json::from_slice(&std::fs::read(handoffs[0].path()).unwrap()).unwrap();
    assert_eq!(handoff["owner"]["person"], person("bob"));
    let text = handoff.to_string();
    for forbidden in ["agent_home", "account_id", "conversation", "run_id", "source_digest", "session-1"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    // A stale generation is refused.
    let stale: shared::ContinueRequest = serde_json::from_value(json!({"request_id":rid(),"project":"proj-shared",
        "person":person("alice"),"id":published["id"],"generation":7})).unwrap();
    assert_eq!(bob_store.continue_with_own_account(&stale, b"y").unwrap_err().code, "conflict");
}

#[test]
fn memory_notes_are_explicit_and_typed() {
    let shared = shared_root();
    let policy = verified("base");
    let alice = principal("alice", me(), "alice-worker");
    let alice_store = store(&shared, &policy, &alice, &shared.alice_state);
    let add = |kind: &str| {
        let body = json!({"request_id":rid(),"project":"proj-shared","title":"Use sqlite","note_kind":kind,"text":"Because."}).to_string();
        alice_store.add_memory(&serde_json::from_str(&body).unwrap(), body.as_bytes())
    };
    assert_eq!(add("decision").unwrap()["generation"], 1);
    assert_eq!(add("swarm-merge").unwrap_err().code, "entry-invalid");
    let request: shared::ListRequest = serde_json::from_value(json!({"project":"proj-shared","kind":"memory"})).unwrap();
    assert_eq!(alice_store.list(&request).unwrap()["items"].as_array().unwrap().len(), 1);
    let history: shared::ListRequest = serde_json::from_value(json!({"project":"proj-shared","kind":Kind::History})).unwrap();
    assert!(alice_store.list(&history).unwrap()["items"].as_array().unwrap().is_empty());
}

// ------------------------------------------------------------------ compute

#[test]
fn mediated_files_stay_inside_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let state = temp.path().join("state");
    let root_text = root.to_string_lossy().into_owned();
    let call = |request: Value| compute::serve(&Mode::Legacy, serde_json::from_value(request).unwrap(), &state);
    call(json!({"op":"write","root":root_text,"path":"src/a.txt","content":"hello world"})).unwrap();
    assert_eq!(call(json!({"op":"read","root":root_text,"path":"src/a.txt"})).unwrap()["content"], "hello world");
    call(json!({"op":"edit","root":root_text,"path":"src/a.txt","old":"world","new":"host"})).unwrap();
    assert_eq!(std::fs::read_to_string(root.join("src/a.txt")).unwrap(), "hello host");
    assert_eq!(call(json!({"op":"list","root":root_text,"path":"src"})).unwrap()["entries"][0]["name"], "a.txt");
    for escape in ["../outside.txt", "src/../../x"] {
        assert_eq!(call(json!({"op":"write","root":root_text,"path":escape,"content":"x"})).unwrap_err().code, "path-outside");
    }
    let outside = temp.path().join("outside.txt").to_string_lossy().into_owned();
    assert_eq!(call(json!({"op":"read","root":root_text,"path":outside})).unwrap_err().code, "path-outside");
    assert!(serde_json::from_value::<compute::Request>(json!({"op":"read","root":root_text,"path":"a","agent_home":"x"})).is_err());
    // The root may not be Zerus state.
    std::fs::create_dir_all(&state).unwrap();
    let state_text = state.to_string_lossy().into_owned();
    assert_eq!(call(json!({"op":"list","root":state_text,"path":"."})).unwrap_err().code, "root-invalid");
    assert_eq!(compute::serve(&Mode::Broken("x"), serde_json::from_value(json!({"op":"list","root":root_text,"path":"."})).unwrap(), &state).unwrap_err().code, "policy-broken");
}

#[test]
fn mediated_commands_run_in_the_root_with_a_scrubbed_environment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let result = compute::serve(
        &Mode::Legacy,
        serde_json::from_value(json!({"op":"exec","root":root.to_string_lossy(),"command":"echo mediated","timeout_secs":30})).unwrap(),
        &temp.path().join("state"),
    )
    .unwrap();
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"].as_str().unwrap().contains("mediated"));
    for (name, _) in compute::mediated_env() {
        assert!(!providers::secret_env(name.as_str()), "{name}");
    }
}

#[test]
fn a_bound_caller_cannot_mediate_into_another_os_accounts_folder() {
    let temp = tempfile::tempdir().unwrap();
    let host = alice_host();
    let bob = load(&host, &other_os(4242), Trust::TestOwned);
    let request = json!({"op":"list","root":temp.path().to_string_lossy(),"path":"."});
    assert_eq!(compute::serve(&bob, serde_json::from_value(request).unwrap(), &temp.path().join("s")).unwrap_err().code, "root-owner");
}

#[test]
fn the_mcp_server_forwards_only_known_tools_and_never_falls_back() {
    let mut seen = Vec::new();
    let mut call = |request: &Value| {
        seen.push(request.clone());
        Err::<Value, String>("host-unreachable: the remote host is offline; nothing ran locally".into())
    };
    let init = compute::handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}), "/w", &mut call).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "zerus-remote");
    let tools = compute::handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), "/w", &mut call).unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);
    let offline = compute::handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"remote_bash","arguments":{"command":"make"}}}), "/w", &mut call).unwrap();
    assert_eq!(offline["result"]["isError"], true);
    assert!(offline["result"]["content"][0]["text"].as_str().unwrap().contains("host-unreachable"));
    let unknown = compute::handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"Bash","arguments":{"command":"make"}}}), "/w", &mut call).unwrap();
    assert_eq!(unknown["result"]["isError"], true);
    let widened = compute::handle(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"remote_read","arguments":{"path":"a","root":"/etc"}}}), "/w", &mut call).unwrap();
    assert_eq!(widened["result"]["isError"], true);
    assert!(compute::handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), "/w", &mut call).is_none());
    assert_eq!(seen.len(), 1, "only the valid call was forwarded");
    assert_eq!(seen[0], json!({"op":"exec","root":"/w","command":"make"}));
}

#[test]
fn claude_runs_with_only_the_remote_tools_and_no_token_in_argv() {
    let argv = compute::claude_args(Path::new("/opt/hgs"), "build-host", "/home/alice/work").unwrap();
    assert_eq!(&argv[..3], ["--tools", "", "--strict-mcp-config"]);
    let config: Value = serde_json::from_str(&argv[4]).unwrap();
    assert_eq!(config["mcpServers"].as_object().unwrap().len(), 1);
    let text = argv.join(" ");
    for forbidden in ["TOKEN", "API_KEY", "sk-", "oauth", "CLAUDE_CONFIG_DIR", "credentials"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    assert!(compute::claude_args(Path::new("/opt/hgs"), "-oProxyCommand=x", "/w").is_err());
    assert!(compute::claude_args(Path::new("/opt/hgs"), "h", "relative").is_err());
}

// ------------------------------------------------- downgrade and rollback

fn policy_with_revision(revision: u64) -> String {
    // Re-signing is not needed: the revision is not part of any signature.
    let mut value: Value = serde_json::from_str(&policy_text("base")).unwrap();
    value["revision"] = json!(revision);
    value.to_string()
}

fn write_policy(host: &Host, text: &str) {
    std::fs::write(host.dir.join("policy.json"), text).unwrap();
}

fn caller_state(host: &Host) -> PathBuf {
    host.dir.parent().unwrap().join("caller-state")
}

fn broken(mode: Mode) -> &'static str {
    match mode {
        Mode::Broken(code) => code,
        other => panic!("not broken: {other:?}"),
    }
}

#[test]
fn a_fresh_legacy_host_with_no_marker_stays_legacy() {
    let host = alice_host();
    std::fs::remove_file(host.dir.join("policy.json")).unwrap();
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Legacy));
    assert!(!runtime::seen_any(&caller_state(&host)));
}

#[test]
fn a_host_seen_as_multi_person_never_returns_to_legacy() {
    let host = alice_host();
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
    assert!(runtime::seen_any(&caller_state(&host)));
    std::fs::remove_file(host.dir.join("policy.json")).unwrap();
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "policy-lost");
    // An unbound caller sharing that state folder is refused the same way.
    assert_eq!(broken(load(&host, &other_os(7), Trust::TestOwned)), "policy-lost");
}

#[test]
fn a_policy_rollback_and_a_same_revision_replacement_are_refused() {
    let host = alice_host();
    write_policy(&host, &policy_with_revision(5));
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
    // The same bytes again are fine; a newer revision advances the mark.
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
    write_policy(&host, &policy_with_revision(6));
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
    write_policy(&host, &policy_with_revision(5));
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "policy-rollback");
    // Revision 6 with other bytes (same content, reordered whitespace).
    let mut replaced = policy_with_revision(6);
    replaced.push(' ');
    write_policy(&host, &replaced);
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "policy-replaced");
    write_policy(&host, &policy_with_revision(6));
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
}

#[test]
fn a_lost_or_corrupt_mark_after_the_marker_fails_closed_without_rebootstrap() {
    let host = alice_host();
    assert!(matches!(load(&host, &me(), Trust::TestOwned), Mode::Bound { .. }));
    let mark = caller_state(&host).join("host-access/high-water").join(format!("{}.json", host_id_of()));
    assert!(mark.exists());
    std::fs::write(&mark, "{not json").unwrap();
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "high-water-unreadable");
    std::fs::remove_file(&mark).unwrap();
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "high-water-lost");
    // Still lost on the next call: nothing rebootstraps it.
    assert_eq!(broken(load(&host, &me(), Trust::TestOwned)), "high-water-lost");
}

fn host_id_of() -> String {
    host()
}
