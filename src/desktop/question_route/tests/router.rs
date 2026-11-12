//! The router end to end over a real Store, catalog and deferred-send actor,
//! with fixture reads and a fixture child (zerus-sa9qn.8).
use super::super::{
    ledger,
    runtime::{Fetched, Runtime, PENDING, READS, SETTLE},
};
use crate::{
    config::Config,
    desktop::{
        deferred_send::{self, Ended, Reservation},
        feed,
        store::{HandoffLock, Location, Store},
    },
};
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zerus_client_core::desktop::{deferred_send as wire, question_route as route};
use zerus_desktop_catalog as catalog;
use zerus_watch_protocol::{Body, CatalogItem, Delta, Frame, Hello, Snapshot};

/// What the fixture child received: its arguments and stdin.
type Sent = Arc<Mutex<Vec<(Vec<String>, Value)>>>;
/// Named changes of a row or card.
type Changes = Vec<(&'static str, Box<dyn Fn(&mut Value)>)>;
const MACHINE: &str = "22222222-2222-4222-8222-222222222222";
const CHILD: &str = "claude/project/child";
const CHILD_ID: &str = "00000000-0000-4000-8000-000000000001";
const PARENT: &str = "codex/project/parent";
const PARENT_ID: &str = "00000000-0000-4000-8000-000000000002";
const REFERENCE: &str = "zerus://session?machine=22222222-2222-4222-8222-222222222222&host=box&name=claude%2Fproject%2Fchild&conversation=conversation-1&run=run-1";

fn child_row() -> Value {
    json!({"name": CHILD, "cmd": "claude", "state": "running", "phase": "input",
        "session_id": CHILD_ID, "run_id": "run-1", "conversation_id": "conversation-1",
        "attention_signature": "a".repeat(64), "human_request": false,
        "question_routing": {"send_to_parent": true, "revision": 0},
        "parent": {"schema": 1, "machine": MACHINE, "session_id": PARENT_ID,
            "conversation_id": "parent-conversation", "source": "launch", "at": 1, "state": "live"}})
}
fn parent_row() -> Value {
    json!({"name": PARENT, "cmd": "codex", "state": "running", "phase": "idle",
        "session_id": PARENT_ID, "run_id": "parent-run", "conversation_id": "parent-conversation"})
}
fn card(id: &str) -> Value {
    json!({"question_id": id, "tool_call_id": "toolu_1", "agent_id": "main",
        "run_id": "run-1", "conversation_id": "conversation-1", "created_at": 1.0,
        "source": "hook", "can_answer": true, "answer_transport": "claude_hook",
        "answer_unavailable_reason": "", "question_hash": "b".repeat(64),
        "questions": [{"id": "q_0", "question": "Which database?", "header": "DB",
            "multi_select": false, "options": [{"id": "opt_0_0", "label": "Postgres"},
            {"id": "opt_0_1", "label": "SQLite"}]}]})
}
fn inspect(cards: Vec<Value>) -> Value {
    json!({"name": CHILD, "agent": "claude", "session_id": CHILD_ID, "run_id": "run-1",
        "conversation_id": "conversation-1", "pending_questions": cards})
}
fn reference_of(edge: &route::Edge) -> Value {
    json!({"schema": 1, "uri": REFERENCE, "name": edge.name, "machine_id": MACHINE,
        "host": "box", "run_id": edge.run_id, "conversation_id": edge.conversation_id,
        "archive_id": "", "agent_id": ""})
}
fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
/// The CLI's own private file under the state root `state`.
fn private(state: &Path, folder: Option<&str>, name: &str, body: &[u8]) {
    let dir = folder.map_or(state.to_path_buf(), |f| state.join(f));
    if !dir.exists() {
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o700);
    }
    let temp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&temp, body).unwrap();
    mode(&temp, 0o600);
    std::fs::rename(&temp, dir.join(name)).unwrap();
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Child {
    /// `hgs send` submitted and wrote its receipt.
    Submitted,
    /// The child exited without any receipt: an `hgs send` refusal.
    NoReceipt,
    /// Refused before any child existed.
    NotStarted,
}

struct Fixture {
    _temp: tempfile::TempDir,
    config: Config,
    location: Location,
    store: Store,
    send: deferred_send::Runtime,
    route: Runtime,
    exclusion: Arc<HandoffLock>,
    owner: Arc<crate::platform::FileLock>,
    fences: Arc<crate::desktop::not_admitted::Fences>,
    epoch: String,
    source: catalog::Source,
    seq: u64,
    cursor: i64,
    now: Instant,
    reads: Arc<AtomicUsize>,
    /// Each read takes the first reply; the last one stays.
    replies: Arc<Mutex<Vec<Value>>>,
    sent: Sent,
    child: Arc<Mutex<Child>>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = Config {
            home: root.to_str().unwrap().into(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        std::fs::create_dir(&config.dir).unwrap();
        let location = Location::new(&config).unwrap();
        location.prepare().unwrap();
        // The parent's own CLI record: the actor's last check before a child.
        use sha2::Digest;
        private(
            &config.state,
            None,
            &format!("{:x}.json", sha2::Sha256::digest(PARENT.as_bytes())),
            json!({"name": PARENT, "agent": "codex", "run_id": "parent-run",
                "conversation_id": "parent-conversation"})
            .to_string()
            .as_bytes(),
        );
        let epoch = uuid::Uuid::new_v4().to_string();
        let mut store = Store::open(&location, &epoch).unwrap();
        zerus_desktop_ledger::authorize(&mut store.db, &uuid::Uuid::new_v4().to_string(), &epoch)
            .unwrap();
        let owner = Arc::new(location.lock().unwrap());
        let exclusion = Arc::new(location.handoff_lock().unwrap());
        let fences = Arc::new(crate::desktop::not_admitted::Fences::default());
        let mut f = Self {
            send: deferred_send::Runtime::new(&config, &epoch, owner.clone(), fences.clone())
                .unwrap(),
            route: Runtime::fixture(
                &epoch,
                Arc::new(|_: &route::Edge| None),
                config.state.clone(),
            ),
            _temp: temp,
            config,
            location,
            store,
            exclusion,
            owner,
            fences,
            source: catalog::Source {
                computer: String::new(),
                service_epoch: epoch.clone(),
                incarnation: 1,
                generation: 1,
            },
            epoch,
            seq: 0,
            cursor: 0,
            now: Instant::now(),
            reads: Arc::new(AtomicUsize::new(0)),
            replies: Arc::new(Mutex::new(vec![inspect(vec![card("tool:toolu_1")])])),
            sent: Arc::new(Mutex::new(Vec::new())),
            child: Arc::new(Mutex::new(Child::Submitted)),
        };
        f.restart_runtimes();
        f
    }
    /// A service restart: the store opened again, and fresh runtimes.
    fn restart(&mut self) {
        self.store = Store::open(&self.location, &self.epoch).unwrap();
        zerus_desktop_ledger::authorize(
            &mut self.store.db,
            &uuid::Uuid::new_v4().to_string(),
            &self.epoch,
        )
        .unwrap();
        // The local source connects again as a new incarnation.
        self.source.incarnation += 1;
        self.restart_runtimes();
    }
    /// Fresh runtimes over the same store.
    fn restart_runtimes(&mut self) {
        let mut send = deferred_send::Runtime::new(
            &self.config,
            &self.epoch,
            self.owner.clone(),
            self.fences.clone(),
        )
        .unwrap();
        let (sent, child, state) = (
            self.sent.clone(),
            self.child.clone(),
            self.config.state.clone(),
        );
        send.fixture_executor(
            MACHINE,
            Arc::new(
                move |plan: &wire::Plan, args: &[String], input: &[u8], _, _| {
                    sent.lock()
                        .unwrap()
                        .push((args.to_vec(), serde_json::from_slice(input).unwrap()));
                    match *child.lock().unwrap() {
                        Child::Submitted => {
                            let receipt = json!({"request_id": plan.request_id,
                            "name": plan.session, "run_id": plan.run_id,
                            "conversation_id": plan.conversation_id, "digest": plan.input_sha256,
                            "status": wire::SUBMITTED, "transport": "terminal"});
                            private(
                                &state,
                                Some("input_receipts"),
                                &format!("{}.json", plan.request_id),
                                receipt.to_string().as_bytes(),
                            );
                            Ended::Exited { code: 0 }
                        }
                        Child::NoReceipt => Ended::Exited { code: 1 },
                        Child::NotStarted => Ended::NotStarted,
                    }
                },
            ),
        );
        send.fixture_mac_fence(false);
        send.poll(&mut self.store, Some(self.exclusion.clone()));
        self.send = send;
        let (reads, replies) = (self.reads.clone(), self.replies.clone());
        self.route = Runtime::fixture(
            &self.epoch,
            Arc::new(move |edge: &route::Edge| {
                reads.fetch_add(1, Ordering::SeqCst);
                let mut replies = replies.lock().unwrap();
                let inspect = if replies.len() > 1 {
                    replies.remove(0)
                } else {
                    replies[0].clone()
                };
                Some(Fetched {
                    inspect,
                    reference: reference_of(edge),
                })
            }),
            self.config.state.clone(),
        );
        self.route.fixture_clock(self.now);
    }
    fn apply(&mut self, body: Body, connect: bool) {
        self.seq += 1;
        let frame = Frame {
            v: 1,
            seq: self.seq,
            body,
        };
        if connect {
            catalog::connect(&mut self.store.db, &self.source, &frame).unwrap();
        } else {
            catalog::apply(&mut self.store.db, &self.source, &frame).unwrap();
        }
        self.route
            .observe_packet(
                &self.store,
                &feed::Packet::Watch {
                    computer: self.source.computer.clone(),
                    generation: self.source.generation,
                    frame: Box::new(frame),
                },
            )
            .unwrap();
    }
    fn item(row: Value) -> CatalogItem {
        CatalogItem {
            key: zerus_watch_protocol::catalog_key(&row).unwrap(),
            row,
        }
    }
    fn snapshot(&mut self, rows: Vec<Value>) {
        let hello = Hello {
            machine_id: Some(MACHINE.into()),
            host: "box".into(),
            hgs_version: "1.56.0".into(),
            stream: "stream".into(),
            heartbeat_ms: 10_000,
            max_frame_bytes: 1 << 20,
            topics: vec!["catalog".into()],
            limits: Default::default(),
        };
        self.apply(Body::Hello(hello), true);
        let pages = rows.len().div_ceil(catalog::PAGE_ROWS).max(1);
        let mut rows = rows.into_iter();
        for page in 0..pages {
            let items = rows
                .by_ref()
                .take(catalog::PAGE_ROWS)
                .map(|row| serde_json::to_value(Self::item(row)).unwrap())
                .collect();
            self.apply(
                Body::Snapshot(Snapshot {
                    topic: "catalog".into(),
                    page: page as u32,
                    key: None,
                    value: None,
                    last: page + 1 == pages,
                    cursor: 0,
                    items: Some(items),
                }),
                false,
            );
        }
    }
    fn delta(&mut self, row: Value) {
        self.cursor += 1;
        self.apply(
            Body::Delta(Delta {
                topic: "catalog".into(),
                cursor: self.cursor,
                upsert: vec![Self::item(row)],
                remove: vec![],
            }),
            false,
        );
    }
    fn tick(&mut self) {
        self.send
            .poll(&mut self.store, Some(self.exclusion.clone()));
        self.route.poll(
            &mut self.store,
            &mut self.send,
            Some(self.exclusion.clone()),
        );
        let status = self.route.status();
        assert_ne!(status["code"], "question_route_unavailable", "{status}");
    }
    fn advance(&mut self, by: Duration) {
        self.now += by;
        self.route.fixture_clock(self.now);
    }
    /// Ticks until `done`, each edge settling past its window.
    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(
                Instant::now() < until,
                "timed out: {what}: {}",
                self.route.status()
            );
            self.tick();
            self.advance(SETTLE + Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    /// Ticks long enough for any read, send and settlement to happen.
    fn settle(&mut self) {
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until {
            self.tick();
            self.advance(SETTLE + Duration::from_millis(1));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn routes(&self) -> Vec<(String, String, String)> {
        let mut query = self
            .store
            .db
            .prepare(
                "SELECT request_id,state,code FROM desktop_question_routes ORDER BY created,key",
            )
            .unwrap();
        query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn states(&self) -> Vec<String> {
        self.routes()
            .into_iter()
            .map(|(_, state, _)| state)
            .collect()
    }
    fn sent(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
    fn reply(&self, replies: Vec<Value>) {
        *self.replies.lock().unwrap() = replies;
    }
}

#[test]
fn a_settled_edge_is_read_twice_and_routed_once_with_the_fixed_text() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    // Nothing is read before the edge settles.
    for _ in 0..20 {
        f.tick();
    }
    assert_eq!((f.reads(), f.sent()), (0, 0));
    f.until("submitted", |f| f.states() == ["submitted"]);
    // The first read, then the read immediately before the reservation.
    assert_eq!(f.reads(), 2);
    let sent = f.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let (args, stdin) = &sent[0];
    assert_eq!(args, &["send", PARENT, "--json"]);
    // The parent's current row, never the link's hints.
    assert_eq!(
        (
            &stdin["expected_run_id"],
            &stdin["expected_conversation_id"]
        ),
        (&json!("parent-run"), &json!("parent-conversation"))
    );
    let text = stdin["text"].as_str().unwrap();
    assert!(text.starts_with("[Zerus child question]\n"), "{text}");
    assert!(
        text.contains(&format!("hgs inspect '{REFERENCE}'")),
        "{text}"
    );
    assert!(text.contains("> Which database?"), "{text}");
    assert!(text.len() <= route::TEXT_BYTES);
    // The request ID is the question identity's own.
    let identity = route::Identity {
        machine: MACHINE.into(),
        session_id: CHILD_ID.into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        question_id: "tool:toolu_1".into(),
        question_hash: "b".repeat(64),
    };
    assert_eq!(stdin["request_id"], identity.request_id());
    assert_eq!(f.routes()[0].0, identity.request_id());
    // The actor's own original is the owner-approved route kind.
    let receipt = deferred_send::receipt(&f.store.db, &identity.request_id())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.binding.kind, wire::Kind::RouteQuestion);
    assert_eq!(receipt.phase, wire::Phase::Recorded);
    // More events of the same edge never read or send again.
    f.delta(child_row());
    f.settle();
    assert_eq!((f.reads(), f.sent()), (2, 1));
    let status = f.route.status();
    assert_eq!(
        (status["routed"].clone(), status["authority"].clone()),
        (json!(1), json!("owner_approved_route"))
    );
}

#[test]
fn a_row_that_is_not_a_supervised_quiet_claude_child_is_never_read() {
    let changes: Changes = vec![
        (
            "human request",
            Box::new(|r| r["human_request"] = json!(true)),
        ),
        (
            "older host",
            Box::new(|r| {
                r.as_object_mut().unwrap().remove("human_request");
            }),
        ),
        (
            "parent saved",
            Box::new(|r| r["parent"]["state"] = json!("saved")),
        ),
        (
            "parent missing",
            Box::new(|r| r["parent"]["state"] = json!("missing")),
        ),
        (
            "forwarded link",
            Box::new(|r| r["parent"]["source"] = json!("forwarded")),
        ),
        (
            "no parent",
            Box::new(|r| {
                r.as_object_mut().unwrap().remove("parent");
            }),
        ),
        ("a Codex child", Box::new(|r| r["cmd"] = json!("codex"))),
        ("an approval", Box::new(|r| r["phase"] = json!("approval"))),
        ("busy", Box::new(|r| r["phase"] = json!("tool"))),
        ("exited", Box::new(|r| r["process_state"] = json!("exited"))),
    ];
    for (what, change) in changes {
        let mut f = Fixture::new();
        let mut row = child_row();
        change(&mut row);
        f.snapshot(vec![row, parent_row()]);
        f.settle();
        assert_eq!((f.reads(), f.sent(), f.routes().len()), (0, 0, 0), "{what}");
    }
}

#[test]
fn another_computers_rows_never_route() {
    let mut f = Fixture::new();
    f.source.computer = "peer".into();
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.sent()), (0, 0));
}

#[test]
fn only_claudes_own_blocking_hook_question_routes_from_a_read() {
    let changes: Changes = vec![
        (
            "Claude Code's own prompt",
            Box::new(|c| c["source"] = json!("claude_native_question")),
        ),
        (
            "a tool approval card",
            Box::new(|c| c["source"] = json!("claude_tool_approval")),
        ),
        (
            "folder trust",
            Box::new(|c| {
                c["source"] = json!("claude_folder_trust");
                c["question_id"] = json!("claude-trust:x");
            }),
        ),
        ("optional", Box::new(|c| c["optional"] = json!(true))),
        (
            "answered",
            Box::new(|c| c["answer_delivery"] = json!({"status": "submitted"})),
        ),
        (
            "not answerable",
            Box::new(|c| c["can_answer"] = json!(false)),
        ),
        (
            "a subagent's",
            Box::new(|c| c["agent_id"] = json!("agent-1")),
        ),
        ("no hash", Box::new(|c| c["question_hash"] = json!(""))),
    ];
    for (what, change) in changes {
        let mut f = Fixture::new();
        let mut card = card("tool:toolu_1");
        change(&mut card);
        f.reply(vec![inspect(vec![card])]);
        f.snapshot(vec![child_row(), parent_row()]);
        f.settle();
        // One read finds nothing to route: no second read, no send, no row.
        assert_eq!((f.reads(), f.sent(), f.routes().len()), (1, 0, 0), "{what}");
    }
}

#[test]
fn a_question_answered_before_the_second_read_is_never_sent() {
    let mut f = Fixture::new();
    f.reply(vec![inspect(vec![card("tool:toolu_1")]), inspect(vec![])]);
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (2, 0, 0));
    // A changed question (another hash) in the second read is not sent
    // either; it is a new question for a later edge.
    let mut f = Fixture::new();
    let mut changed = card("tool:toolu_1");
    changed["question_hash"] = json!("c".repeat(64));
    f.reply(vec![
        inspect(vec![card("tool:toolu_1")]),
        inspect(vec![changed]),
    ]);
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (2, 0, 0));
}

#[test]
fn a_child_that_is_no_longer_quiet_or_a_parent_that_is_not_live_gets_nothing() {
    // The child becomes a human request between its edge and the reservation.
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    f.tick();
    f.advance(SETTLE + Duration::from_millis(1));
    f.tick();
    assert_eq!(f.route.fixture_pending(), (0, 1), "the first read started");
    let mut human = child_row();
    human["human_request"] = json!(true);
    catalog_replace(&mut f, human);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (2, 0, 0));
    // The parent's own row: absent, paused, another agent, or ambiguous.
    let parents: Vec<(&str, Vec<Value>)> = vec![
        ("absent", vec![]),
        (
            "paused",
            vec![json!({"name": PARENT, "cmd": "codex", "state": "paused",
            "session_id": PARENT_ID, "run_id": "parent-run"})],
        ),
        (
            "an OpenCode parent",
            vec![json!({"name": "opencode/project/parent", "cmd": "opencode",
            "state": "running", "session_id": PARENT_ID, "run_id": "parent-run"})],
        ),
        (
            "ambiguous",
            vec![
                parent_row(),
                json!({"name": "codex/project/copy", "cmd": "codex",
            "state": "running", "session_id": PARENT_ID, "run_id": "copy-run"}),
            ],
        ),
    ];
    for (what, parents) in parents {
        let mut f = Fixture::new();
        let mut rows = vec![child_row()];
        rows.extend(parents);
        f.snapshot(rows);
        f.settle();
        assert_eq!((f.reads(), f.sent(), f.routes().len()), (2, 0, 0), "{what}");
    }
}

/// Replaces the child's catalog row in place, without an event: the router
/// sees it only at its own reservation check.
fn catalog_replace(f: &mut Fixture, row: Value) {
    let key = zerus_watch_protocol::catalog_key(&row).unwrap();
    assert_eq!(
        f.store
            .db
            .execute(
                "UPDATE desktop_catalog_rows SET json=?1 WHERE key=?2",
                rusqlite::params![row.to_string(), key],
            )
            .unwrap(),
        1
    );
}

#[test]
fn a_routed_question_is_never_sent_again_after_a_restart() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("submitted", |f| f.states() == ["submitted"]);
    // A restart reads the still-pending question again and finds it routed.
    f.restart();
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!(f.reads(), 3, "one first read after the restart");
    assert_eq!((f.sent(), f.states()), (1, vec!["submitted".to_owned()]));
}

#[test]
fn an_unknown_result_is_kept_and_never_replayed() {
    let mut f = Fixture::new();
    *f.child.lock().unwrap() = Child::NoReceipt;
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("unknown", |f| f.states() == ["unknown"]);
    assert_eq!(f.routes()[0].2, "deferred_send_receipt_missing");
    // A new edge of the same question, and a restart, read it again but
    // never send it again.
    *f.child.lock().unwrap() = Child::Submitted;
    let mut again = child_row();
    again["attention_signature"] = json!("d".repeat(64));
    f.delta(again.clone());
    f.settle();
    f.restart();
    f.snapshot(vec![again, parent_row()]);
    f.settle();
    assert_eq!(f.sent(), 1);
    assert_eq!(f.states(), ["unknown"]);
}

#[test]
fn a_reserved_route_interrupted_by_a_restart_settles_unknown_and_never_runs() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    // Reserve without letting the actor start its child: the actor's own
    // poll is not run between the reservation and the restart.
    let until = Instant::now() + Duration::from_secs(10);
    while f.routes().is_empty() {
        assert!(Instant::now() < until, "{}", f.route.status());
        f.route
            .poll(&mut f.store, &mut f.send, Some(f.exclusion.clone()));
        f.advance(SETTLE + Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(f.states(), ["reserved"]);
    f.restart();
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!(f.states(), ["unknown"]);
    assert_eq!(f.routes()[0].2, "deferred_send_interrupted");
    assert_eq!(f.sent(), 0, "an interrupted original never runs or resends");
}

#[test]
fn a_child_renamed_with_shell_text_is_refused_and_never_sent() {
    // zerus-sa9qn.35: the routed text prints the child's name in a command.
    let hostile = "claude/example/x;touch pwned;#";
    let mut f = Fixture::new();
    let mut row = child_row();
    row["name"] = json!(hostile);
    let mut read = inspect(vec![card("tool:toolu_1")]);
    read["name"] = json!(hostile);
    f.reply(vec![read]);
    f.snapshot(vec![row, parent_row()]);
    f.until("refused", |f| f.states() == ["refused"]);
    assert_eq!(f.routes()[0].2, ledger::INVALID);
    f.settle();
    assert_eq!((f.sent(), f.states()), (0, vec!["refused".to_owned()]));
}

#[test]
fn a_child_with_a_hostile_conversation_id_is_refused_and_never_sent() {
    // zerus-sa9qn.40: a hook's session_id is only bounded.
    let hostile = "x';touch${IFS}pwned;'";
    let mut f = Fixture::new();
    let mut row = child_row();
    row["conversation_id"] = json!(hostile);
    let mut question = card("tool:toolu_1");
    question["conversation_id"] = json!(hostile);
    let mut read = inspect(vec![question]);
    read["conversation_id"] = json!(hostile);
    f.reply(vec![read]);
    f.snapshot(vec![row, parent_row()]);
    f.until("refused", |f| f.states() == ["refused"]);
    assert_eq!(f.routes()[0].2, ledger::INVALID);
    f.settle();
    assert_eq!((f.sent(), f.states()), (0, vec!["refused".to_owned()]));
}

#[test]
fn a_refusal_before_any_child_is_final() {
    let mut f = Fixture::new();
    *f.child.lock().unwrap() = Child::NotStarted;
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("refused", |f| f.states() == ["refused"]);
    assert_eq!(f.routes()[0].2, wire::NOT_STARTED);
    *f.child.lock().unwrap() = Child::Submitted;
    let mut again = child_row();
    again["attention_signature"] = json!("d".repeat(64));
    f.delta(again);
    f.settle();
    assert_eq!(f.states(), ["refused"], "never retried");
}

#[test]
fn the_per_parent_bound_refuses_the_twenty_first_question_in_an_hour() {
    let mut f = Fixture::new();
    let now = chrono::Utc::now().timestamp_millis();
    for n in 0..route::PER_PARENT_HOUR {
        f.store
            .db
            .execute(
                "INSERT INTO desktop_question_routes VALUES(?1,?2,?3,?4,'q',?5,?6,'')",
                rusqlite::params![
                    format!("{n:064x}"),
                    format!("00000000-0000-4000-8000-{n:012x}"),
                    CHILD_ID,
                    PARENT_ID,
                    now - 60_000,
                    // Refused routes do not count toward the bound.
                    if n == 0 { "refused" } else { "submitted" }
                ],
            )
            .unwrap();
    }
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("routed under the bound", |f| {
        f.states().iter().filter(|s| *s == "submitted").count() == 20
    });
    let mut next = child_row();
    next["attention_signature"] = json!("e".repeat(64));
    f.reply(vec![inspect(vec![card("tool:toolu_2")])]);
    f.delta(next);
    f.until("bounded", |f| f.routes().len() == 22);
    let last = f.routes().pop().unwrap();
    assert_eq!(
        (last.1.as_str(), last.2.as_str()),
        ("refused", ledger::PARENT_BOUND)
    );
    assert_eq!(f.sent(), 1);
    // An hour later the parent's bound is free again (the old rows age out).
    f.store
        .db
        .execute_batch(
            "DROP TRIGGER desktop_question_route_identity;
             UPDATE desktop_question_routes SET created=created-3600000;",
        )
        .unwrap();
    let mut later = child_row();
    later["attention_signature"] = json!("f".repeat(64));
    f.reply(vec![inspect(vec![card("tool:toolu_3")])]);
    f.delta(later);
    f.until("routed again", |f| f.sent() == 2);
}

#[test]
fn reads_and_the_pending_queue_stay_bounded() {
    let mut f = Fixture::new();
    // Reads that wait: at most READS in flight, at most one per child.
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap();
    let (reads, waiting) = (f.reads.clone(), gate.clone());
    f.route = Runtime::fixture(
        &f.epoch,
        Arc::new(move |_: &route::Edge| {
            reads.fetch_add(1, Ordering::SeqCst);
            let _wait = waiting.lock().unwrap();
            None
        }),
        f.config.state.clone(),
    );
    f.route.fixture_clock(f.now);
    let mut rows = vec![parent_row()];
    for n in 0..PENDING + 6 {
        let mut row = child_row();
        row["name"] = json!(format!("claude/project/child-{n}"));
        row["run_id"] = json!(format!("run-{n}"));
        // The first two are rows of one child session.
        if n >= 2 {
            row["session_id"] = json!(format!("00000000-0000-4000-8000-1{n:011x}"));
        }
        rows.push(row);
    }
    f.snapshot(rows);
    f.tick();
    let (pending, _) = f.route.fixture_pending();
    assert_eq!(pending, PENDING, "the queue keeps at most PENDING edges");
    assert_eq!(
        f.route.fixture_counts().3,
        6,
        "the rest are dropped, never queued"
    );
    f.advance(SETTLE + Duration::from_millis(1));
    for _ in 0..20 {
        f.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let (_, reading) = f.route.fixture_pending();
    assert_eq!(reading, READS);
    assert_eq!(f.reads(), READS);
    drop(held);
}

#[test]
fn one_child_has_at_most_one_read_in_flight() {
    let mut f = Fixture::new();
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap();
    let (reads, waiting) = (f.reads.clone(), gate.clone());
    f.route = Runtime::fixture(
        &f.epoch,
        Arc::new(move |_: &route::Edge| {
            reads.fetch_add(1, Ordering::SeqCst);
            let _wait = waiting.lock().unwrap();
            None
        }),
        f.config.state.clone(),
    );
    f.route.fixture_clock(f.now);
    // Two rows of one child session (a rename in flight).
    let mut copy = child_row();
    copy["name"] = json!("claude/project/child-copy");
    copy["run_id"] = json!("run-copy");
    f.snapshot(vec![child_row(), copy, parent_row()]);
    f.tick();
    f.advance(SETTLE + Duration::from_millis(1));
    for _ in 0..20 {
        f.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(f.route.fixture_pending(), (1, 1));
    assert_eq!(f.reads(), 1);
    drop(held);
}

#[test]
fn edges_wait_while_the_actor_cannot_admit() {
    let mut f = Fixture::new();
    f.send.request_stop();
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.route.fixture_pending().0), (0, 1));
}

#[test]
fn the_route_ledger_is_retained_and_settles_once() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("submitted", |f| f.states() == ["submitted"]);
    for statement in [
        "UPDATE desktop_question_routes SET state='reserved'",
        "UPDATE desktop_question_routes SET state='unknown'",
        "UPDATE desktop_question_routes SET code='x'",
        "UPDATE desktop_question_routes SET parent_session_id='00000000-0000-4000-8000-000000000009'",
        "DELETE FROM desktop_question_routes",
    ] {
        assert!(f.store.db.execute(statement, []).is_err(), "{statement}");
    }
}

#[test]
fn the_parent_lookup_uses_the_session_index() {
    let f = Fixture::new();
    let plan: Vec<String> = f
        .store
        .db
        .prepare(
            "EXPLAIN QUERY PLAN SELECT r.json FROM desktop_catalog_sources s JOIN desktop_catalog_rows r
             ON r.generation=s.visible
             AND (CASE WHEN json_valid(r.json) THEN json_extract(r.json,'$.session_id') END)=?2
             WHERE s.computer=?1 AND length(CAST(r.json AS BLOB))<=?3 LIMIT 2",
        )
        .unwrap()
        .query_map(rusqlite::params!["", PARENT_ID, 1], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("desktop_question_route_sessions")),
        "{plan:?}"
    );
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_route_leaf() {
    assert_eq!(
        super::super::SCHEMA - 1,
        crate::desktop::creation_worktree::SLOTS_SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let f = Fixture::new();
    let tables = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    super::super::remove_fixture(&f.store.db);
    f.store
        .db
        .pragma_update(None, "user_version", super::super::SCHEMA - 1)
        .unwrap();
    let before = tables(&f.store.db);
    let Fixture {
        store, location, ..
    } = f;
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    // This leaf's table, then the later switch leaf's two (schema 37) and
    // the latch leaf's two (schema 39).
    assert_eq!(tables(&store.db), before + 1 + 2 + 2);
    assert!(ledger::reserved(&store.db, 8).unwrap().is_empty());
}

/// The fixture's one routable question as the router would build it.
fn record() -> route::Record {
    let edge = route::edge(&child_row()).unwrap();
    let question = route::questions(&inspect(vec![card("tool:toolu_1")]), &edge, MACHINE).remove(0);
    route::Record::new(
        &question,
        route::Child {
            name: CHILD.into(),
            reference: REFERENCE.into(),
        },
        route::parent(&parent_row(), &edge).unwrap(),
    )
}
fn intent(scope: &wire::Scope, record: &route::Record) -> wire::Intent {
    wire::Intent {
        scope: scope.clone(),
        session: record.parent.session.clone(),
        request_id: record.request_id(),
        checkpoint: serde_json::to_string(record).unwrap(),
        original: record.stdin().unwrap(),
    }
}

#[test]
fn the_actor_admits_a_route_only_with_its_own_reservation() {
    use crate::desktop::deferred_send::Routed;
    let mut f = Fixture::new();
    let state = f.config.state.clone();
    let record = record();
    let id = record.request_id();
    let exclusion = Some(f.exclusion.clone());
    let scope = f.send.scope().clone();
    let called = std::cell::Cell::new(0);
    // macOS refuses every original: nothing is reserved or admitted.
    f.send.fixture_mac_fence(true);
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |_| {
            called.set(called.get() + 1);
            Ok(Reservation::Reserved)
        },
    );
    assert_eq!((routed, called.get()), (Routed::Unavailable, 0));
    f.send.fixture_mac_fence(false);
    // A reservation error writes nothing.
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |tx| {
            ledger::reserve(tx, &record, 1, &state)?;
            anyhow::bail!("reservation refused")
        },
    );
    assert_eq!(routed, Routed::Unavailable);
    assert!(f.routes().is_empty());
    assert!(deferred_send::receipt(&f.store.db, &id).unwrap().is_none());
    // A final refusal of the reservation commits it, with no original.
    let mut refused = record.clone();
    refused.identity.question_id = "tool:toolu_9".into();
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &refused),
        exclusion.clone(),
        |tx| {
            ledger::reserve(tx, &refused, 1, &state)?;
            Ok(Reservation::Refused)
        },
    );
    assert_eq!(routed, Routed::Refused);
    assert_eq!(f.routes().len(), 1);
    assert!(deferred_send::receipt(&f.store.db, &refused.request_id())
        .unwrap()
        .is_none());
    // Another intent than the record derives is never admitted.
    let mut changed = intent(&scope, &record);
    changed.original = changed
        .original
        .replace("Which database?", "Approve all tools.");
    let routed = f.send.route(&mut f.store, changed, exclusion.clone(), |_| {
        called.set(called.get() + 1);
        Ok(Reservation::Reserved)
    });
    assert_eq!((routed, called.get()), (Routed::Unavailable, 0));
    // The reservation and the queued original commit together.
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |tx| ledger::reserve(tx, &record, 1, &state),
    );
    assert_eq!(routed, Routed::Admitted);
    let receipt = deferred_send::receipt(&f.store.db, &id).unwrap().unwrap();
    assert_eq!(
        (receipt.binding.kind, receipt.phase),
        (wire::Kind::RouteQuestion, wire::Phase::Queued)
    );
    // The same request is never admitted again.
    let routed = f
        .send
        .route(&mut f.store, intent(&scope, &record), exclusion, |_| {
            called.set(called.get() + 1);
            Ok(Reservation::Reserved)
        });
    assert_eq!((routed, called.get()), (Routed::Unavailable, 0));
}

#[test]
fn a_route_is_never_a_persons_consent_and_the_persons_path_refuses_it() {
    let f = Fixture::new();
    let record = record();
    let scope = f.send.scope().clone();
    let route = intent(&scope, &record).verify_route().unwrap();
    // Each authority admits only its own kind.
    assert!(f
        .send
        .fixture_admit_as(intent(&scope, &record).verify_route().unwrap(), true)
        .is_ok());
    assert!(f.send.fixture_admit_as(route, false).is_err());
    use zerus_client_core::operation_recovery::deferred_input::{self as checkpoint, Checkpoint};
    let saved = Checkpoint::reserve(checkpoint::Intent {
        kind: checkpoint::Kind::Send,
        draft_revision: 3,
        message_sha256: checkpoint::message_sha256(
            "hello",
            Vec::<(&str, &str, &str, &[u8])>::new(),
        ),
        run_id: "parent-run".into(),
        conversation_id: "parent-conversation".into(),
        question: None,
        start: None,
        retry_of: None,
        consent: true,
    });
    let person = wire::Intent {
        scope: f.send.scope().clone(),
        session: PARENT.into(),
        request_id: saved.request_id.clone(),
        checkpoint: serde_json::to_string(&saved).unwrap(),
        original: json!({"request_id": saved.request_id, "text": "hello", "attachments": [],
            "expected_run_id": "parent-run", "expected_conversation_id": "parent-conversation"})
        .to_string(),
    };
    assert!(person.clone().verify_route().is_err());
    assert!(f
        .send
        .fixture_admit_as(person.clone().verify().unwrap(), false)
        .is_ok());
    assert!(f
        .send
        .fixture_admit_as(person.verify().unwrap(), true)
        .is_err());
    // A route record never verifies on the person's path.
    assert!(intent(&scope, &record).verify().is_err());
    // The native frontend lane refuses a route carried as a person's send.
    let mut f = f;
    let request = json!(
        zerus_client_core::desktop::ui_requests::Request::DeferredSend {
            intent: Box::new(intent(&scope, &record)),
        }
    );
    let reply = f.send.frontend(
        &mut f.store,
        crate::desktop::ui_authority::Validated::fixture(&request),
        Some(f.exclusion.clone()),
    );
    assert_eq!(reply["accepted"], false, "{reply}");
    assert!(deferred_send::receipt(&f.store.db, &record.request_id())
        .unwrap()
        .is_none());
}

// The person's switches (zerus-sa9qn.32).

/// Sets the person's global switch, as their native request does.
fn switch(f: &mut Fixture, enabled: bool) {
    let id = uuid::Uuid::new_v4().to_string();
    let reply = super::super::policy::set(&mut f.store.db, &id, enabled, 1).unwrap();
    assert_eq!(reply["enabled"], enabled);
}
/// Writes the child's own switch on its host, as `hgs question-routing` does.
fn child_switch(f: &Fixture, send: bool) {
    let store =
        json!({"version": 1, "sessions": {CHILD_ID: {"send_to_parent": send, "revision": 1}}});
    private(
        &f.config.state,
        None,
        crate::state::question_routing::FILE,
        store.to_string().as_bytes(),
    );
}
fn answered() -> Value {
    let mut row = child_row();
    row["phase"] = json!("idle");
    row["attention_signature"] = Value::Null;
    row
}

#[test]
fn the_global_switch_is_on_by_default_and_changes_only_by_the_persons_request() {
    use zerus_client_core::desktop::ui_requests::Request;
    let mut f = Fixture::new();
    assert!(super::super::policy::enabled(&f.store.db).unwrap());
    let mut status = json!({});
    super::super::decorate(&f.store.db, &mut status);
    assert_eq!(status["question_routes"], true);
    let id = uuid::Uuid::new_v4().to_string();
    let request = |enabled| {
        json!(Request::QuestionRoutes {
            request_id: id.clone(),
            enabled
        })
    };
    let validated = crate::desktop::ui_authority::Validated::fixture;
    let off = super::super::frontend(&mut f.store.db, validated(&request(false)));
    assert_eq!(
        (
            off["accepted"].clone(),
            off["durable"].clone(),
            off["enabled"].clone(),
            off["applied"].clone(),
            off["revision"].clone()
        ),
        (
            json!(true),
            json!(true),
            json!(false),
            json!(true),
            json!(1)
        )
    );
    // The same request again returns its receipt; it never changes twice.
    assert_eq!(
        super::super::frontend(&mut f.store.db, validated(&request(false))),
        off
    );
    // Another value under a used request is refused.
    let reused = super::super::frontend(&mut f.store.db, validated(&request(true)));
    assert_eq!(reused["accepted"], false);
    assert!(!super::super::policy::enabled(&f.store.db).unwrap());
    super::super::decorate(&f.store.db, &mut status);
    assert_eq!(status["question_routes"], false);
    // An uncertain result is looked up by its original request, read-only.
    let lookup = |original: &str| {
        super::super::result_request(
            &f.store.db,
            &json!({"op": "question_routes_result", "request_id": uuid::Uuid::new_v4().to_string(),
                "original_request_id": original}),
        )
    };
    let found = lookup(&id);
    assert_eq!(
        (found["found"].clone(), found["result"].clone()),
        (json!(true), off.clone())
    );
    let absent = lookup(&uuid::Uuid::new_v4().to_string());
    assert_eq!(
        (absent["found"].clone(), absent["result"].clone()),
        (json!(false), Value::Null)
    );
    assert_eq!(
        super::super::result_request(&f.store.db, &json!({"op": "question_routes_result"}))
            ["accepted"],
        false
    );
    // The row is kept; receipts are bounded.
    assert!(f
        .store
        .db
        .execute("DELETE FROM desktop_question_route_policy", [])
        .is_err());
    for n in 0..4100 {
        switch(&mut f, n % 2 == 0);
    }
    let kept: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM desktop_question_route_policy_requests",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 4096);
}

#[test]
fn off_stops_the_reservation_in_its_own_transaction_and_writes_nothing() {
    use crate::desktop::deferred_send::Routed;
    let mut f = Fixture::new();
    let state = f.config.state.clone();
    let record = record();
    let scope = f.send.scope().clone();
    let exclusion = Some(f.exclusion.clone());
    switch(&mut f, false);
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |tx| ledger::reserve(tx, &record, 1, &state),
    );
    assert_eq!(routed, Routed::Off);
    switch(&mut f, true);
    child_switch(&f, false);
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |tx| ledger::reserve(tx, &record, 1, &state),
    );
    assert_eq!(routed, Routed::Off);
    assert!(f.routes().is_empty());
    assert!(deferred_send::receipt(&f.store.db, &record.request_id())
        .unwrap()
        .is_none());
    // An unreadable child switch is unknown, which never routes.
    private(
        &f.config.state,
        None,
        crate::state::question_routing::FILE,
        b"{broken",
    );
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, &record),
        exclusion.clone(),
        |tx| ledger::reserve(tx, &record, 1, &state),
    );
    assert_eq!(routed, Routed::Off);
    child_switch(&f, true);
    let routed = f
        .send
        .route(&mut f.store, intent(&scope, &record), exclusion, |tx| {
            ledger::reserve(tx, &record, 1, &state)
        });
    assert_eq!(routed, Routed::Admitted);
}

#[test]
fn while_off_nothing_is_read_and_on_again_routes_a_still_pending_question_once() {
    let mut f = Fixture::new();
    switch(&mut f, false);
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!(
        (f.reads(), f.sent(), f.route.fixture_pending()),
        (0, 0, (0, 0))
    );
    assert_eq!(f.route.status()["code"], "off");
    switch(&mut f, true);
    f.until("submitted", |f| f.states() == ["submitted"]);
    assert_eq!((f.reads(), f.sent()), (2, 1));
}

#[test]
fn a_question_asked_and_answered_while_off_is_never_sent() {
    let mut f = Fixture::new();
    switch(&mut f, false);
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    f.delta(answered());
    f.reply(vec![inspect(vec![])]);
    f.settle();
    switch(&mut f, true);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (0, 0, 0));
}

#[test]
fn turning_routing_back_on_never_replays_a_settled_question() {
    for (child, state) in [
        (Child::Submitted, "submitted"),
        (Child::NoReceipt, "unknown"),
    ] {
        let mut f = Fixture::new();
        *f.child.lock().unwrap() = child;
        f.snapshot(vec![child_row(), parent_row()]);
        f.until(state, |f| f.states() == [state]);
        *f.child.lock().unwrap() = Child::Submitted;
        switch(&mut f, false);
        f.settle();
        switch(&mut f, true);
        let mut again = child_row();
        again["attention_signature"] = json!("d".repeat(64));
        f.delta(again);
        f.settle();
        assert_eq!(
            (f.sent(), f.states()),
            (1, vec![state.to_owned()]),
            "{state}"
        );
    }
}

#[test]
fn off_never_cancels_a_route_already_reserved() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    let until = Instant::now() + Duration::from_secs(10);
    while f.routes().is_empty() {
        assert!(Instant::now() < until, "{}", f.route.status());
        f.route
            .poll(&mut f.store, &mut f.send, Some(f.exclusion.clone()));
        f.advance(SETTLE + Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(f.states(), ["reserved"]);
    switch(&mut f, false);
    f.until("submitted", |f| f.states() == ["submitted"]);
    assert_eq!(f.sent(), 1);
}

#[test]
fn a_child_switched_off_is_never_read_and_its_host_switch_stops_a_stale_row() {
    // The row shows the person's switch: nothing is read.
    let mut f = Fixture::new();
    let mut off = child_row();
    off["question_routing"] = json!({"send_to_parent": false, "revision": 1});
    f.snapshot(vec![off, parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (0, 0, 0));
    // A row not yet updated: the reservation reads the host's switch.
    let mut f = Fixture::new();
    child_switch(&f, false);
    f.snapshot(vec![child_row(), parent_row()]);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (2, 0, 0));
    assert!(f.route.fixture_off() >= 1);
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_switch_leaf_switched_on() {
    assert_eq!(
        super::super::POLICY_SCHEMA - 1,
        super::super::SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let f = Fixture::new();
    let tables = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    super::super::remove_policy_fixture(&f.store.db);
    f.store
        .db
        .pragma_update(None, "user_version", super::super::POLICY_SCHEMA - 1)
        .unwrap();
    let before = tables(&f.store.db);
    let Fixture {
        store, location, ..
    } = f;
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    // This leaf's two tables, then the later latch leaf's two (schema 39).
    assert_eq!(tables(&store.db), before + 2 + 2);
    assert!(
        super::super::policy::enabled(&store.db).unwrap(),
        "on by default"
    );
}

#[test]
fn a_question_pending_when_routing_turns_on_again_counts_once() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    f.tick();
    assert_eq!(f.route.fixture_pending(), (1, 0), "queued, still settling");
    // Off forgets the queue; the row does not change while off.
    switch(&mut f, false);
    f.tick();
    assert_eq!(f.route.fixture_pending(), (0, 0));
    // On again reads the catalog from a new baseline: still pending and
    // supervised, so it routes once.
    switch(&mut f, true);
    f.until("submitted", |f| f.states() == ["submitted"]);
    assert_eq!((f.reads(), f.sent()), (2, 1));
}

#[test]
fn switching_off_during_a_first_read_reads_nothing_more() {
    let mut f = Fixture::new();
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap();
    let (reads, waiting) = (f.reads.clone(), gate.clone());
    f.route = Runtime::fixture(
        &f.epoch,
        Arc::new(move |edge: &route::Edge| {
            reads.fetch_add(1, Ordering::SeqCst);
            let _wait = waiting.lock().unwrap();
            Some(Fetched {
                inspect: inspect(vec![card("tool:toolu_1")]),
                reference: reference_of(edge),
            })
        }),
        f.config.state.clone(),
    );
    f.route.fixture_clock(f.now);
    f.snapshot(vec![child_row(), parent_row()]);
    f.tick();
    f.advance(SETTLE + Duration::from_millis(1));
    f.tick();
    assert_eq!(f.route.fixture_pending(), (0, 1), "the first read started");
    switch(&mut f, false);
    f.tick();
    drop(held);
    f.settle();
    assert_eq!((f.reads(), f.sent(), f.routes().len()), (1, 0, 0));
}

/// The read-only Activity request of `session` (zerus-sa9qn.33).
fn activity(f: &Fixture, session: &str) -> Value {
    super::super::activity_request(
        &f.store.db,
        &json!({"op": "question_route_activity",
            "request_id": uuid::Uuid::new_v4().to_string(), "session_id": session}),
    )
}
fn lines(f: &Fixture, session: &str) -> Vec<Value> {
    let reply = activity(f, session);
    assert_eq!(reply["accepted"], true, "{reply}");
    reply["routes"].as_array().unwrap().clone()
}
/// A route admitted by the actor at `at` (ms), then settled as `state`.
fn admitted(f: &mut Fixture, record: &route::Record, at: i64, state: &str) {
    let scope = f.send.scope().clone();
    let root = f.config.state.clone();
    let routed = f.send.route(
        &mut f.store,
        intent(&scope, record),
        Some(f.exclusion.clone()),
        |tx| ledger::reserve(tx, record, at, &root),
    );
    assert_eq!(routed, deferred_send::Routed::Admitted);
    if state != "reserved" {
        ledger::settle(&mut f.store.db, &record.identity.key(), state, "").unwrap();
    }
}
fn question(record: &route::Record, n: usize) -> route::Record {
    let mut other = record.clone();
    other.identity.question_id = format!("tool:toolu_{n}");
    other
}

#[test]
fn a_submitted_route_is_one_line_for_the_child_and_the_parent() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("submitted", |f| f.states() == ["submitted"]);
    let sent = f.sent.lock().unwrap()[0].1["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = f.routes()[0].0.clone();
    let child = lines(&f, CHILD_ID);
    assert_eq!(child.len(), 1);
    let line = &child[0];
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    // The child's line: its own reference and the parent, never the text.
    assert_eq!(
        keys(line),
        ["at", "child", "parent", "question", "request_id", "role"]
    );
    assert_eq!(
        (line["request_id"].as_str(), line["role"].as_str()),
        (Some(id.as_str()), Some("child"))
    );
    assert_eq!(
        line["child"],
        json!({"name": CHILD, "reference": REFERENCE, "session_id": CHILD_ID})
    );
    assert_eq!(
        line["parent"],
        json!({"name": PARENT, "session_id": PARENT_ID})
    );
    assert_eq!(
        line["question"],
        json!({"kind": "single-choice", "questions": 1, "options": 2, "preview": "Which database?"})
    );
    assert!(line["at"].as_i64().is_some_and(|at| at > 0));
    // The parent's line also carries the exact text it received.
    let parent = lines(&f, PARENT_ID);
    assert_eq!(parent.len(), 1);
    assert_eq!(parent[0]["role"], "parent");
    assert_eq!(parent[0]["text"].as_str(), Some(sent.as_str()));
    assert_eq!(parent[0]["child"], line["child"]);
    // Another session has none.
    assert!(lines(&f, &uuid::Uuid::new_v4().to_string()).is_empty());
}

#[test]
fn refused_reserved_and_unknown_routes_have_no_line() {
    let mut f = Fixture::new();
    *f.child.lock().unwrap() = Child::NoReceipt;
    f.snapshot(vec![child_row(), parent_row()]);
    f.until("unknown", |f| f.states() == ["unknown"]);
    let mut f2 = Fixture::new();
    *f2.child.lock().unwrap() = Child::NotStarted;
    f2.snapshot(vec![child_row(), parent_row()]);
    f2.until("refused", |f| f.states() == ["refused"]);
    for f in [&f, &f2] {
        assert!(lines(f, CHILD_ID).is_empty());
        assert!(lines(f, PARENT_ID).is_empty());
    }
    // Admitted but not yet settled: still nothing.
    let mut f3 = Fixture::new();
    admitted(&mut f3, &record(), 1, "reserved");
    assert_eq!(f3.states(), ["reserved"]);
    assert!(lines(&f3, CHILD_ID).is_empty());
}

#[test]
fn the_activity_request_is_exact_and_names_no_other_route() {
    let f = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    for bad in [
        json!({"op": "question_route_activity", "request_id": id}),
        json!({"op": "question_route_activity", "request_id": id, "session_id": CHILD_ID, "limit": 100}),
        json!({"op": "question_routes_result", "request_id": id, "session_id": CHILD_ID}),
        json!({"op": "question_route_activity", "request_id": id, "session_id": "0000000A-0000-4000-8000-000000000001"}),
        json!({"op": "question_route_activity", "request_id": id, "session_id": "00000000-0000-0000-0000-000000000000"}),
        json!({"op": "question_route_activity", "request_id": id, "session_id": format!("{CHILD_ID}' OR 1=1 --")}),
        json!({"op": "question_route_activity", "request_id": "x", "session_id": CHILD_ID}),
        json!({"op": "question_route_activity", "request_id": id, "session_id": 1}),
        json!([]),
    ] {
        let reply = super::super::activity_request(&f.store.db, &bad);
        assert_eq!(
            (reply["accepted"].as_bool(), reply["code"].as_str()),
            (Some(false), Some("invalid_question_route_activity")),
            "{bad}"
        );
        assert!(reply.get("routes").is_none());
    }
    // A ledger row whose original is not its route's is never shown.
    let mut f = Fixture::new();
    admitted(&mut f, &record(), 1, "submitted");
    f.store
        .db
        .execute(
            "INSERT INTO desktop_question_routes(key,request_id,child_session_id,parent_session_id,
             question_id,created,state,code) VALUES(?1,?2,?3,?4,'tool:x',2,'submitted','')",
            rusqlite::params![
                "c".repeat(64),
                uuid::Uuid::new_v4().to_string(),
                CHILD_ID,
                PARENT_ID
            ],
        )
        .unwrap();
    assert_eq!(lines(&f, CHILD_ID).len(), 1);
}

#[test]
fn only_the_newest_twenty_lines_of_either_role_are_read() {
    let mut f = Fixture::new();
    const HOUR: i64 = 3_600_000;
    // 15 routes of the child to its parent, then 15 to it as a parent of
    // its own child, each an hour apart under the per-parent bound.
    let base = record();
    let mut grandchild = base.clone();
    grandchild.identity.session_id = "00000000-0000-4000-8000-000000000003".into();
    grandchild.child.name = "claude/project/grandchild".into();
    grandchild.parent = route::Parent {
        session: CHILD.into(),
        session_id: CHILD_ID.into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
    };
    for n in 0..15 {
        admitted(
            &mut f,
            &question(&base, n),
            (2 * n as i64 + 1) * HOUR,
            "submitted",
        );
        admitted(
            &mut f,
            &question(&grandchild, n),
            (2 * n as i64 + 2) * HOUR,
            "submitted",
        );
    }
    // A refused and a reserved one are newest, yet never shown.
    admitted(&mut f, &question(&base, 99), 100 * HOUR, "refused");
    let shown = lines(&f, CHILD_ID);
    assert_eq!(shown.len(), super::super::activity::LIMIT);
    let at: Vec<i64> = shown.iter().map(|l| l["at"].as_i64().unwrap()).collect();
    let mut newest: Vec<i64> = (11..=30).rev().map(|n| n * HOUR).collect();
    newest.truncate(20);
    assert_eq!(at, newest, "newest first, both roles merged");
    let roles: Vec<&str> = shown.iter().map(|l| l["role"].as_str().unwrap()).collect();
    assert_eq!(roles.iter().filter(|r| **r == "child").count(), 10);
    assert!(shown
        .iter()
        .all(|l| (l["role"] == "parent") == l.get("text").is_some()));
    // Each read uses its own partial index.
    for (sql, index) in [
        (
            super::super::activity::CHILD_SQL,
            "desktop_question_routes_child_submitted",
        ),
        (
            super::super::activity::PARENT_SQL,
            "desktop_question_routes_parent_submitted",
        ),
    ] {
        let plan: Vec<String> = f
            .store
            .db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(rusqlite::params![CHILD_ID, 20], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(plan.iter().any(|step| step.contains(index)), "{plan:?}");
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_activity_indexes() {
    assert_eq!(
        super::super::ACTIVITY_SCHEMA - 1,
        super::super::POLICY_SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let mut f = Fixture::new();
    admitted(&mut f, &record(), 1, "submitted");
    let objects = |db: &rusqlite::Connection| -> (i64, i64) {
        db.query_row(
            "SELECT count(*) FILTER (WHERE type='table'),count(*) FILTER (WHERE type='index')
             FROM sqlite_schema",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    super::super::remove_activity_fixture(&f.store.db);
    f.store
        .db
        .pragma_update(None, "user_version", super::super::POLICY_SCHEMA)
        .unwrap();
    let (tables, indexes) = objects(&f.store.db);
    let Fixture {
        store, location, ..
    } = f;
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    // The two partial indexes, then the later latch leaf's two tables, its
    // receipt index and their two primary key indexes (schema 39).
    assert_eq!(objects(&store.db), (tables + 2, indexes + 2 + 3));
    // The route settled before the upgrade is indexed and shown.
    let reply = super::super::activity_request(
        &store.db,
        &json!({"op": "question_route_activity",
            "request_id": uuid::Uuid::new_v4().to_string(), "session_id": CHILD_ID}),
    );
    assert_eq!(reply["routes"].as_array().map(Vec::len), Some(1), "{reply}");
}

/// The person's latch request for the fixture child (zerus-sa9qn.50).
fn latch_request(id: &str, person_off: bool, expected_revision: u64) -> Value {
    json!(
        zerus_client_core::desktop::ui_requests::Request::QuestionRouteChild {
            request_id: id.into(),
            session_id: CHILD_ID.into(),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            person_off,
            expected_revision,
        }
    )
}
fn latch(f: &mut Fixture, request: &Value) -> Value {
    super::super::latch_frontend(
        &mut f.store.db,
        crate::desktop::ui_authority::Validated::fixture(request),
    )
}

#[test]
fn the_persons_latch_stops_a_child_whatever_its_host_file_says() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    let id = || uuid::Uuid::new_v4().to_string();
    let off = latch(&mut f, &latch_request(&id(), true, 0));
    assert_eq!(
        (
            off["accepted"].clone(),
            off["person_off"].clone(),
            off["revision"].clone(),
            off["applied"].clone()
        ),
        (json!(true), json!(true), json!(1), json!(true)),
        "{off}"
    );
    // The host file and the row say on; the latch alone keeps it from
    // routing, and nothing is written.
    child_switch(&f, true);
    f.settle();
    assert_eq!((f.sent(), f.routes().len()), (0, 0));
    assert!(f.route.fixture_off() >= 1);
    // An agent turning the host file on again changes nothing either.
    let mut again = child_row();
    again["attention_signature"] = json!("d".repeat(64));
    f.delta(again);
    f.settle();
    assert_eq!((f.sent(), f.routes().len()), (0, 0));
    // Clearing it needs the person's request at the current revision.
    let stale = latch(&mut f, &latch_request(&id(), false, 0));
    assert_eq!(
        (
            stale["accepted"].clone(),
            stale["code"].clone(),
            stale["person_off"].clone(),
            stale["revision"].clone()
        ),
        (json!(false), json!("conflict"), json!(true), json!(1))
    );
    assert!(super::super::latch::latched(&f.store.db, CHILD_ID).unwrap());
    let on = latch(&mut f, &latch_request(&id(), false, 1));
    assert_eq!(
        (on["person_off"].clone(), on["revision"].clone()),
        (json!(false), json!(2))
    );
    // A new edge of the still-pending question now routes once.
    let mut later = child_row();
    later["attention_signature"] = json!("e".repeat(64));
    f.delta(later);
    f.until("submitted", |f| f.states() == ["submitted"]);
    assert_eq!(f.sent(), 1);
}

#[test]
fn the_latch_request_is_exact_kept_and_bounded() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    let id = uuid::Uuid::new_v4().to_string();
    let off = latch(&mut f, &latch_request(&id, true, 0));
    // The same request again returns its receipt; another value is refused.
    assert_eq!(latch(&mut f, &latch_request(&id, true, 0)), off);
    assert_eq!(
        latch(&mut f, &latch_request(&id, false, 1))["accepted"],
        false
    );
    // Already so: confirmed without a new revision.
    let same = latch(
        &mut f,
        &latch_request(&uuid::Uuid::new_v4().to_string(), true, 1),
    );
    assert_eq!(
        (same["applied"].clone(), same["revision"].clone()),
        (json!(false), json!(1))
    );
    // Only the child's exact current run and conversation, on this computer.
    for (field, value, code) in [
        ("run_id", json!("run-2"), "stale_identity"),
        ("conversation_id", json!("conversation-2"), "stale_identity"),
        (
            "session_id",
            json!("00000000-0000-4000-8000-000000000009"),
            "not_found",
        ),
    ] {
        let mut request = latch_request(&uuid::Uuid::new_v4().to_string(), false, 1);
        request[field] = value;
        let refused = latch(&mut f, &request);
        assert_eq!(
            (refused["accepted"].clone(), refused["code"].clone()),
            (json!(false), json!(code)),
            "{field}: {refused}"
        );
    }
    assert!(super::super::latch::latched(&f.store.db, CHILD_ID).unwrap());
    // An uncertain result is looked up read-only; the status names the
    // current value for the next compare-and-swap.
    let lookup = super::super::latch_result(
        &f.store.db,
        &json!({"op": "question_route_child_result",
            "request_id": uuid::Uuid::new_v4().to_string(), "original_request_id": id}),
    );
    assert_eq!(
        (lookup["found"].clone(), lookup["result"].clone()),
        (json!(true), off)
    );
    let status = super::super::latch_status(
        &f.store.db,
        &json!({"op": "question_route_child_status",
            "request_id": uuid::Uuid::new_v4().to_string(), "session_id": CHILD_ID}),
    );
    assert_eq!(
        (status["person_off"].clone(), status["revision"].clone()),
        (json!(true), json!(1))
    );
    for bad in [
        json!({"op": "question_route_child_status", "request_id": id, "session_id": CHILD_ID, "x": 1}),
        json!({"op": "question_route_child_status", "request_id": id, "session_id": "x"}),
        json!({"op": "question_route_child_result", "request_id": id}),
        // Each read answers only its own operation.
        json!({"op": "question_route_child_result", "request_id": id, "session_id": CHILD_ID}),
        json!({"op": "question_route_child_status", "request_id": id, "original_request_id": id}),
    ] {
        let refused = if bad.get("session_id").is_some() {
            super::super::latch_status(&f.store.db, &bad)
        } else {
            super::super::latch_result(&f.store.db, &bad)
        };
        assert_eq!(refused["accepted"], false, "{bad}");
    }
    // The latch is kept; its receipts are immutable.
    for statement in [
        "DELETE FROM desktop_question_route_children",
        "UPDATE desktop_question_route_child_requests SET person_off=0",
    ] {
        assert!(f.store.db.execute(statement, []).is_err(), "{statement}");
    }
    // At the bound a new child's latch is refused; existing ones still change.
    f.store
        .db
        .execute(
            "INSERT INTO desktop_question_route_children(session_id,person_off,revision)
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
             SELECT printf('%08x-0000-4000-8000-%012x',i,i),1,1 FROM n",
            [super::super::latch::CHILDREN - 1],
        )
        .unwrap();
    let full = super::super::latch::set(
        &mut f.store.db,
        &uuid::Uuid::new_v4().to_string(),
        "00000000-0000-4000-8000-00000000000a",
        true,
        Some(0),
        1,
    )
    .unwrap();
    assert_eq!(
        (full["accepted"].clone(), full["code"].clone()),
        (json!(false), json!("capacity"))
    );
    let changed = latch(
        &mut f,
        &latch_request(&uuid::Uuid::new_v4().to_string(), false, 1),
    );
    assert_eq!(changed["revision"], 2);
}

#[test]
fn exact_previous_store_upgrade_adds_only_the_latch_leaf() {
    assert_eq!(
        super::super::LATCH_SCHEMA - 1,
        super::super::ACTIVITY_SCHEMA,
        "rebase this test onto the actual merge-order predecessor"
    );
    let f = Fixture::new();
    let tables = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    super::super::remove_latch_fixture(&f.store.db);
    f.store
        .db
        .pragma_update(None, "user_version", super::super::ACTIVITY_SCHEMA)
        .unwrap();
    let before = tables(&f.store.db);
    let Fixture {
        store, location, ..
    } = f;
    drop(store);
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        store
            .db
            .query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    assert_eq!(tables(&store.db), before + 2);
    assert!(!super::super::latch::latched(&store.db, CHILD_ID).unwrap());
}

/// A phone's latch statement as the connector forwards it (zerus-sa9qn.50).
fn person(request_id: &str, person_off: bool) -> Value {
    json!({"op": "question_route_person", "signature": "c2lnbmF0dXJl",
        "statement": {"workspace_id": "workspace-fixture", "device_id": "phone-1",
            "gateway_id": "gateway-1", "session_id": CHILD_ID, "run_id": "run-1",
            "conversation_id": "conversation-1", "person_off": person_off,
            "request_id": request_id, "issued_at": 1, "expires_at": 2}})
}
/// The verifier's confirmation of exactly this statement.
fn verified(input: &Value) -> anyhow::Result<Value> {
    Ok(json!({"ok": true, "verified": input["statement"]}))
}
fn host(f: &Fixture) -> Option<bool> {
    crate::state::question_routing::send_to_parent_at(&f.config.state, CHILD_ID)
}

#[test]
fn a_phones_verified_statement_sets_the_latch_once_and_the_host_follows() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    let state = f.config.state.clone();
    let id = uuid::Uuid::new_v4().to_string();
    let seen = std::cell::Cell::new(0);
    let off = super::super::latch_person(&mut f.store.db, &state, &person(&id, true), |input| {
        seen.set(seen.get() + 1);
        // The verifier gets exactly the statement and its signature.
        assert_eq!(
            input,
            &json!({"statement": person(&id, true)["statement"], "signature": "c2lnbmF0dXJl"})
        );
        verified(input)
    });
    assert_eq!(
        (
            off["accepted"].clone(),
            off["person_off"].clone(),
            off["revision"].clone(),
            off["host"].clone()
        ),
        (json!(true), json!(true), json!(1), json!(true)),
        "{off}"
    );
    assert!(super::super::latch::latched(&f.store.db, CHILD_ID).unwrap());
    assert_eq!(host(&f), Some(false));
    // The same statement again returns its receipt, never verified or applied twice.
    let again = super::super::latch_person(&mut f.store.db, &state, &person(&id, true), |_| {
        panic!("a replay is never verified again")
    });
    assert_eq!(again["revision"], 1);
    assert_eq!(seen.get(), 1);
    // Another value under the same request is refused.
    let reused = super::super::latch_person(&mut f.store.db, &state, &person(&id, false), verified);
    assert_eq!(reused["accepted"], false);
    // The phone turns it on again: the latch clears, then the host follows.
    let on = super::super::latch_person(
        &mut f.store.db,
        &state,
        &person(&uuid::Uuid::new_v4().to_string(), false),
        verified,
    );
    assert_eq!(
        (on["person_off"].clone(), on["revision"].clone()),
        (json!(false), json!(2))
    );
    assert!(!super::super::latch::latched(&f.store.db, CHILD_ID).unwrap());
    assert_eq!(host(&f), Some(true));
}

#[test]
fn an_unverified_or_mismatched_statement_changes_nothing() {
    let mut f = Fixture::new();
    f.snapshot(vec![child_row(), parent_row()]);
    let state = f.config.state.clone();
    let mut other_session = person(&uuid::Uuid::new_v4().to_string(), true);
    other_session["statement"]["session_id"] = json!("00000000-0000-4000-8000-000000000009");
    let mut other_run = person(&uuid::Uuid::new_v4().to_string(), true);
    other_run["statement"]["run_id"] = json!("run-2");
    let fresh = || person(&uuid::Uuid::new_v4().to_string(), true);
    type Verify = Box<dyn Fn(&Value) -> anyhow::Result<Value>>;
    let cases: Vec<(&str, Value, Verify, &str)> = vec![
        (
            "refused",
            fresh(),
            Box::new(|_| anyhow::bail!("not verified")),
            "not_verified",
        ),
        (
            "not ok",
            fresh(),
            Box::new(|_| Ok(json!({"ok": false}))),
            "not_verified",
        ),
        (
            "another statement",
            fresh(),
            Box::new(|input| {
                let mut other = input["statement"].clone();
                other["person_off"] = json!(false);
                Ok(json!({"ok": true, "verified": other}))
            }),
            "not_verified",
        ),
        ("not found", other_session, Box::new(verified), "not_found"),
        ("stale", other_run, Box::new(verified), "stale_identity"),
    ];
    for (name, request, verify, code) in cases {
        let reply = super::super::latch_person(&mut f.store.db, &state, &request, verify);
        assert_eq!(
            (reply["accepted"].clone(), reply["code"].clone()),
            (json!(false), json!(code)),
            "{name}: {reply}"
        );
    }
    // Shapes are exact; nothing is verified for them.
    let mut extra = fresh();
    extra["extra"] = json!(1);
    let mut field = fresh();
    field["statement"]["extra"] = json!(1);
    let mut session = fresh();
    session["statement"]["session_id"] = json!("x");
    let mut signature = fresh();
    signature["signature"] = json!(1);
    for bad in [
        extra,
        field,
        session,
        signature,
        json!({"op": "question_route_person"}),
    ] {
        let reply = super::super::latch_person(&mut f.store.db, &state, &bad, |_| {
            panic!("an invalid request is never verified")
        });
        assert_eq!(reply["code"], "invalid_request", "{bad}");
    }
    assert!(!super::super::latch::latched(&f.store.db, CHILD_ID).unwrap());
    assert_eq!(host(&f), Some(true));
}
