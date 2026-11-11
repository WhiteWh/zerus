//! Exercise the actual claim/dispatch/journal boundary. This file also builds
//! against the parent, so its positive recovery assertion supplies a RED control.
#![cfg(unix)]
mod common;
use common::{Scratch, EPOCH};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use zerus_mobile_connector::{
    capabilities::Flags,
    execute::Outcome,
    journal::{Journal, JOURNAL},
    native::{Call, Native, NativeError, NativeResult, Output},
    runtime::{Connector, Settings},
};
use zerus_relay::json as codec;

const READ: &str = "11111111-1111-4111-8111-111111111111";
const ORIGINAL: &str = "22222222-2222-4222-8222-222222222222";
const MACHINE: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";
const NAME: &str = "codex/example/send";

fn intent() -> Value {
    json!({"schema":1,"computer_id":"fixture","machine_id":MACHINE,"session_id":SESSION_ID,
        "session":NAME,"provider":"codex","native_encoding":"python_default",
        "payload":{"request_id":ORIGINAL,"expected_run_id":"run","expected_conversation_id":"conversation",
            "text":"  e\u{301}\nexact trailing space  ","attachments":[]}})
}
fn request(intent: Value) -> Value {
    json!({"request_id":READ,"operation":"send_receipt","computer_id":intent["computer_id"],"session":intent["session"],
        "payload":{"schema":1,"request_id":READ,"intent_sha256":codec::digest(codec::canonical(&intent).unwrap()),"intent":intent}})
}
fn response(query: &Value, name: &Value) -> Value {
    json!({"schema":1,"request_id":query["request_id"],"message_request_id":query["message_request_id"],
        "name":name,"machine_id":query["expected_machine_id"],"session_id":query["expected_session_id"],
        "run_id":query["expected_run_id"],"conversation_id":query["expected_conversation_id"],
        "provider":query["provider"],"agent_id":query["agent_id"],"native_digest":query["native_digest"],
        "status":"submitted","proof":"original_receipt","submitted_at":EPOCH,"queued":false})
}
struct Fake {
    calls: RefCell<Vec<Call>>,
    edit: RefCell<Option<(String, Value)>>,
}
impl Native for Fake {
    async fn call(&self, call: Call) -> NativeResult {
        let outer = call.payload.as_ref().unwrap();
        let (argv, q) = if call.argv == ["swarm", "mobile-peer", "--json"] {
            (
                outer["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>(),
                &outer["payload"],
            )
        } else {
            (call.argv.iter().map(String::as_str).collect(), outer)
        };
        assert_eq!(argv, ["__state", "delivery", "receipt", NAME, "--json"]);
        assert_eq!(call.max_output_bytes, Some(8192));
        assert_eq!(call.timeout, Some(5.0));
        let mut answer = response(q, &json!(NAME));
        if let Some((key, value)) = &*self.edit.borrow() {
            answer[key] = value.clone();
        }
        self.calls.borrow_mut().push(call);
        Ok(Output::Json(answer))
    }
}
fn node<N: Native + 'static>(
    native: N,
    dir: &std::path::Path,
    run: &str,
    session: &str,
) -> Connector<N> {
    let node = Connector::new(
        native,
        Journal::open(dir, "fixture", EPOCH).unwrap(),
        Settings {
            interval: 5.0,
            binding: "fixture".into(),
            default_timeout: 45.0,
            clock_scale: 1.0,
        },
        Box::new(|| EPOCH),
    );
    node.flags.set(Flags::from_help("delivery-receipt-v1"));
    node.state
        .borrow_mut()
        .session_ids
        .insert(run.into(), session.into());
    node
}
fn fake(s: &Scratch) -> Connector<Fake> {
    node(
        Fake {
            calls: RefCell::new(vec![]),
            edit: RefCell::new(None),
        },
        &s.path("journal"),
        "run",
        SESSION_ID,
    )
}
fn finished(outcome: Outcome) -> Value {
    match outcome {
        Outcome::Finished(value) => value,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exact_original_proof_is_a_separate_read_and_does_not_reexecute_a_send() {
    let s = Scratch::new("send-receipt");
    let n = fake(&s);
    let r = request(intent());
    let old = json!({"state":"uncertain","result":null,"error":"lost original result"});
    assert!(n
        .journal
        .claim(ORIGINAL, "send", Some("original-fingerprint"), EPOCH)
        .unwrap());
    n.journal.finish(ORIGINAL, &old, EPOCH).unwrap();
    let before = n.journal.pending().unwrap();
    let answer = finished(n.execute(r.clone()).await.unwrap());
    assert_eq!(answer["state"], "completed", "{answer}");
    assert_eq!(answer["result"]["status"], "submitted");
    assert_eq!(answer["result"]["message_request_id"], ORIGINAL);
    assert_eq!(
        answer["result"]["intent_sha256"],
        r["payload"]["intent_sha256"]
    );
    assert!(answer["result"].get("text").is_none());
    assert_eq!(n.execute(r).await.unwrap(), Outcome::Replayed);
    assert_eq!(n.native.calls.borrow().len(), 1);
    assert_eq!(
        n.journal
            .pending()
            .unwrap()
            .iter()
            .find(|(id, _)| id == ORIGINAL),
        before.iter().find(|(id, _)| id == ORIGINAL)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn missing_marker_bad_intent_and_wrong_listed_session_never_call_native() {
    for variant in [
        "old",
        "bad-digest",
        "wrong-session",
        "wrong-computer",
        "unknown-option",
        "same-id",
    ] {
        let s = Scratch::new(variant);
        let n = fake(&s);
        let mut r = request(intent());
        match variant {
            "old" => n
                .flags
                .set(Flags::from_help("prefix-delivery-receipt-v1-suffix")),
            "bad-digest" => r["payload"]["intent_sha256"] = json!("0".repeat(64)),
            "wrong-session" => {
                n.state
                    .borrow_mut()
                    .session_ids
                    .insert("run".into(), MACHINE.into());
            }
            "wrong-computer" => r["computer_id"] = json!("other"),
            "unknown-option" => {
                r["payload"]["intent"]["payload"]["future"] = json!(1);
                r["payload"]["intent_sha256"] = json!(codec::digest(
                    codec::canonical(&r["payload"]["intent"]).unwrap()
                ));
            }
            _ => {
                r["payload"]["intent"]["payload"]["request_id"] = json!(READ);
                r["payload"]["intent_sha256"] = json!(codec::digest(
                    codec::canonical(&r["payload"]["intent"]).unwrap()
                ));
            }
        }
        assert_ne!(
            finished(n.execute(r).await.unwrap())["state"],
            "completed",
            "{variant}"
        );
        assert!(n.native.calls.borrow().is_empty(), "{variant}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_result_identity_and_positive_status_is_checked() {
    for key in [
        "request_id",
        "message_request_id",
        "machine_id",
        "session_id",
        "run_id",
        "conversation_id",
        "name",
        "provider",
        "agent_id",
        "native_digest",
        "proof",
        "status",
        "submitted_at",
        "queued",
    ] {
        let s = Scratch::new(key);
        let n = fake(&s);
        *n.native.edit.borrow_mut() = Some((key.into(), json!("foreign")));
        assert_eq!(
            finished(n.execute(request(intent())).await.unwrap())["state"],
            "failed",
            "{key}"
        );
        assert_eq!(n.native.calls.borrow().len(), 1);
    }
    let s = Scratch::new("unknown");
    let n = fake(&s);
    *n.native.edit.borrow_mut() = Some(("status".into(), json!("unknown")));
    let result = finished(n.execute(request(intent())).await.unwrap());
    assert_eq!(result["result"]["status"], "unknown");
    assert!(result["result"].get("submitted_at").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn v1_decoded_computer_and_restart_classes_preserve_the_original_identity() {
    let s = Scratch::new("v1-decoded");
    let n = fake(&s);
    let mut r = request(intent());
    r.as_object_mut().unwrap().remove("computer_id");
    assert_eq!(
        finished(n.execute(r).await.unwrap())["result"]["status"],
        "submitted"
    );
    drop(n);
    let s = Scratch::new("restart-classes");
    let n = fake(&s);
    n.journal
        .claim(READ, "send_receipt", Some("read"), EPOCH)
        .unwrap();
    n.journal
        .claim(ORIGINAL, "send", Some("send"), EPOCH)
        .unwrap();
    drop(n);
    let n = fake(&s);
    let pending = n.journal.pending().unwrap();
    let state = |id| {
        serde_json::from_str::<Value>(&pending.iter().find(|(key, _)| key == id).unwrap().1)
            .unwrap()["state"]
            .clone()
    };
    assert_eq!(state(READ), "failed");
    assert_eq!(state(ORIGINAL), "uncertain");
    assert!(n.native.calls.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_guarded_read_refuses_a_different_machine_before_the_closed_native_entry() {
    let s = Scratch::new("read-machine-guard");
    let n = fake(&s);
    let result =
        Connector::<Fake>::encrypted_guarded(SESSION_ID.into(), n.execute(request(intent())))
            .await
            .unwrap();
    assert_eq!(finished(result)["state"], "failed");
    assert!(n.native.calls.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn receipt_reads_share_the_bounded_read_cap_without_charging_mutation_capacity() {
    let s = Scratch::new("receipt-read-cap");
    let n = fake(&s);
    let db = rusqlite::Connection::open(s.path("journal").join(JOURNAL)).unwrap();
    db.execute_batch(&format!("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<{}) INSERT INTO requests(request_id,operation,state,created_at,updated_at) SELECT CAST(x AS TEXT),'send_receipt','running',{EPOCH},{EPOCH} FROM n",zerus_mobile_connector::journal::READ_CAP)).unwrap();
    assert!(matches!(
        n.execute(request(intent())).await.unwrap(),
        Outcome::Rejected(_)
    ));
    assert!(n.native.calls.borrow().is_empty());
    assert!(n
        .journal
        .claim(ORIGINAL, "send", Some("independent mutation"), EPOCH)
        .unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_routed_reads_are_receipted_without_native_calls_or_a_connector_panic() {
    for payload in [None, Some(Value::Null), Some(json!({"intent":false}))] {
        let s = Scratch::new("malformed-receipt-route");
        let n = fake(&s);
        let mut r = request(intent());
        r["gateway_route"] =
            json!({"schema":1,"route_id":null,"computer_id":SESSION_ID,"machine_id":MACHINE});
        if let Some(payload) = payload {
            r["payload"] = payload;
        } else {
            r.as_object_mut().unwrap().remove("payload");
        }
        assert_eq!(finished(n.execute(r).await.unwrap())["state"], "failed");
        assert!(n.native.calls.borrow().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_cold_peer_refuses_without_listing_or_provider_rpc_then_a_ready_scope_reads_once() {
    use zerus_mobile_connector::fleet::{route_id, PeerNative, Route};
    let s = Scratch::new("receipt-peer");
    let n = fake(&s);
    n.flags.set(Flags::from_help(
        "delivery-receipt-v1\nmobile-peers --json\nmobile-peer --json",
    ));
    let identity = route_id("peer.example.test", MACHINE);
    let fleet = n.fleet.as_ref().unwrap();
    *fleet.local_id.borrow_mut() = Some(SESSION_ID.into());
    fleet.routes.borrow_mut().insert(
        identity.clone(),
        Route {
            route_id: identity.clone(),
            machine_id: MACHINE.into(),
            via: "peer.example.test".into(),
            name: "Peer".into(),
            online: true,
        },
    );
    let peer = Rc::new(n.peer_context(
        MACHINE,
        PeerNative {
            gateway: n.native.clone(),
            routes: fleet.routes.clone(),
            slots: fleet.slots.clone(),
            identity: identity.clone(),
            machine: MACHINE.into(),
            default_timeout: 45.0,
        },
    ));
    peer.flags.set(Flags::from_help("delivery-receipt-v1"));
    peer.state
        .borrow_mut()
        .session_ids
        .insert("run".into(), SESSION_ID.into());
    fleet
        .contexts
        .borrow_mut()
        .insert(identity.clone(), peer.clone());
    let mut i = intent();
    i["computer_id"] = json!(SESSION_ID);
    let mut r = request(i);
    r["gateway_route"] =
        json!({"schema":1,"route_id":identity,"computer_id":SESSION_ID,"machine_id":MACHINE});
    assert_eq!(
        finished(n.execute(r.clone()).await.unwrap())["state"],
        "failed"
    );
    assert!(n.native.calls.borrow().is_empty());
    peer.snapshot_ready.set();
    r["request_id"] = json!(MACHINE);
    r["payload"]["request_id"] = json!(MACHINE);
    assert_eq!(
        finished(n.execute(r).await.unwrap())["result"]["status"],
        "submitted"
    );
    assert_eq!(n.native.calls.borrow().len(), 1);
}

/// Actual hgs fixtures are provided by the maintained Python terminal harness.
/// Tests build without it; the qualification command explicitly runs --ignored.
#[derive(Clone)]
struct Real {
    fixture: Rc<Value>,
    lost: Rc<Cell<bool>>,
    calls: Rc<RefCell<Vec<Vec<String>>>>,
}
impl Native for Real {
    async fn call(&self, call: Call) -> NativeResult {
        use std::process::Stdio;
        use tokio::{io::AsyncWriteExt, process::Command};
        let effective: Vec<String> = if call.argv.first().is_some_and(|v| v == "swarm") {
            assert_eq!(call.argv, ["swarm", "__mobile-peer-local", "--json"]);
            call.payload.as_ref().unwrap()["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        } else if call.argv.first().is_some_and(|v| v == "--expect-machine") {
            assert_eq!(
                call.argv[1],
                self.fixture["intent"]["machine_id"].as_str().unwrap()
            );
            call.argv[2..].to_vec()
        } else {
            call.argv.clone()
        };
        let effective = if effective
            .get(..2)
            .is_some_and(|v| v == ["__state", "delivery"])
        {
            effective[1..].to_vec()
        } else {
            effective
        };
        assert!(matches!(effective[0].as_str(), "send" | "delivery"));
        self.calls.borrow_mut().push(effective.clone());
        let mut child = Command::new(self.fixture["hgs"].as_str().unwrap())
            .args(&call.argv)
            .env_clear()
            .envs(
                self.fixture["env"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k, v.as_str().unwrap())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(
                &zerus_mobile_connector::native::stdin_bytes(call.payload.as_ref().unwrap())
                    .unwrap(),
            )
            .await
            .unwrap();
        drop(input);
        let output = child.wait_with_output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if effective[0] == "send" && !self.lost.replace(true) {
            return Err(NativeError::Failed("native command timed out"));
        }
        Ok(Output::Json(
            serde_json::from_slice(&output.stdout).unwrap(),
        ))
    }
}
async fn dispatch_real(n: &Connector<Real>, value: Value, fixture: &Value) -> Outcome {
    let machine = fixture["intent"]["machine_id"].as_str().unwrap().to_owned();
    match fixture["mode"].as_str().unwrap() {
        "routed" => Connector::<Real>::guarded(machine, n.execute(value))
            .await
            .unwrap(),
        "encrypted" => Connector::<Real>::encrypted_guarded(machine, n.execute(value))
            .await
            .unwrap(),
        _ => n.execute(value).await.unwrap(),
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "run with a private scripted terminal fixture; never native owner state"]
async fn actual_native_submission_lost_result_restart_and_read_never_replays() {
    let path = std::env::var_os("ZERUS_SEND_RECEIPT_FIXTURE").expect("private fixture required");
    let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let intent = &fixture["intent"];
    let original = &intent["payload"];
    let run = original["expected_run_id"].as_str().unwrap();
    let session = intent["session_id"].as_str().unwrap();
    let dir = std::path::Path::new(fixture["journal"].as_str().unwrap());
    let native = Real {
        fixture: Rc::new(fixture.clone()),
        lost: Rc::new(Cell::new(false)),
        calls: Rc::new(RefCell::new(vec![])),
    };
    let n = node(native.clone(), dir, run, session);
    n.state
        .borrow_mut()
        .sessions
        .insert(intent["session"].as_str().unwrap().into());
    let send = json!({"request_id":original["request_id"],"computer_id":intent["computer_id"],"operation":"send","session":intent["session"],"payload":original});
    let outcome = finished(dispatch_real(&n, send.clone(), &fixture).await);
    assert_eq!(outcome["state"], "uncertain", "{outcome}");
    let original_path = std::path::Path::new(fixture["original"].as_str().unwrap());
    let bytes = std::fs::read(original_path).unwrap();
    let modified = std::fs::metadata(original_path)
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["status"],
        "submitted"
    );
    drop(n);
    let n = node(native.clone(), dir, run, session);
    let before = n.journal.pending().unwrap();
    let read = request(intent.clone());
    let answer = finished(dispatch_real(&n, read.clone(), &fixture).await);
    assert_eq!(answer["state"], "completed", "{answer}");
    assert_eq!(answer["result"]["status"], "submitted");
    assert_eq!(
        dispatch_real(&n, read.clone(), &fixture).await,
        Outcome::Replayed
    );
    let mut next = read;
    next["request_id"] = json!(MACHINE);
    next["payload"]["request_id"] = json!(MACHINE);
    assert_eq!(
        finished(dispatch_real(&n, next, &fixture).await)["result"]["status"],
        "submitted"
    );
    assert_eq!(dispatch_real(&n, send, &fixture).await, Outcome::Replayed);
    let id = original["request_id"].as_str().unwrap();
    assert_eq!(
        n.journal
            .pending()
            .unwrap()
            .iter()
            .find(|(key, _)| key == id),
        before.iter().find(|(key, _)| key == id)
    );
    assert_eq!(std::fs::read(original_path).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(original_path)
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(
        native
            .calls
            .borrow()
            .iter()
            .filter(|args| args[0] == "send")
            .count(),
        1
    );
    assert_eq!(
        native
            .calls
            .borrow()
            .iter()
            .filter(|args| args[0] == "delivery")
            .count(),
        2
    );
    assert!(dir.join(JOURNAL).exists());
    println!(
        "SEND_RECEIPT_REAL_CONTROL={}",
        json!({"mode":fixture["mode"],"native_sends":1,"native_lookups":2,"original_journal_unchanged":true,"original_bytes_and_mtime_unchanged":true})
    );
}
