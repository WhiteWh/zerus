#![cfg(unix)]
//! A phone's signed latch statement (zerus-sa9qn.50): the connector only
//! forwards it to its own computer's service and accepts that service's exact
//! receipt or closed refusal, with no real native process.
mod common;
use common::{Scratch, EPOCH};
use serde_json::{json, Value};
use std::cell::RefCell;
use zerus_mobile_connector::{
    capabilities::{self, Flags},
    execute::Outcome,
    journal::{is_mutation, Journal},
    native::{Call, Native, NativeResult, Output},
    runtime::{Connector, Settings},
    session_question_route_person::{self, HELP_MARKER, OPERATION},
};

const NAME: &str = "claude/example/child";
/// The shared crypto vector's statement and signature.
fn payload() -> Value {
    let vector: Value = serde_json::from_str(include_str!(
        "../../crypto/tests/vectors/question-route-person-v1.json"
    ))
    .unwrap();
    json!({"statement": vector["statement"], "signature": vector["signature"]})
}
fn request(n: usize) -> Value {
    json!({"request_id":format!("00000000-0000-4000-8000-{n:012}"),"operation":OPERATION,"session":NAME,"payload":payload()})
}
fn receipt() -> Value {
    let statement = &payload()["statement"];
    json!({"request_id": statement["request_id"], "op": "question_route_child", "accepted": true,
        "durable": true, "session_id": statement["session_id"], "person_off": statement["person_off"],
        "revision": 2, "applied": true, "code": ""})
}
struct Fake {
    calls: RefCell<Vec<Call>>,
    result: Value,
}
impl Native for Fake {
    async fn call(&self, call: Call) -> NativeResult {
        self.calls.borrow_mut().push(call);
        Ok(Output::Json(self.result.clone()))
    }
}
fn connector(scratch: &Scratch, result: Value, help: &str) -> Connector<Fake> {
    let node = Connector::new(
        Fake {
            calls: RefCell::new(vec![]),
            result,
        },
        Journal::open(&scratch.path("state"), "fixture", EPOCH).unwrap(),
        Settings {
            interval: 5.0,
            binding: "fixture".into(),
            default_timeout: 45.0,
            clock_scale: 1.0,
        },
        Box::new(|| EPOCH),
    );
    node.flags.set(Flags::from_help(help));
    node.state.borrow_mut().sessions.insert(NAME.into());
    node.state.borrow_mut().snapshot_seen_at = Some(node.clock());
    node
}

#[test]
fn an_exact_marker_offers_a_rust_only_mutation() {
    assert!(is_mutation(OPERATION));
    assert!(capabilities::implemented(OPERATION) && capabilities::connector_only(OPERATION));
    for (help, expected) in [
        ("", false),
        ("session-question-routing-v1", false),
        ("question-route-person-v10", false),
        ("question-route-person-v1 suffix", false),
        (HELP_MARKER, true),
        ("   question-route-person-v1\r", true),
    ] {
        let flags = Flags::from_help(help);
        assert_eq!(flags.question_route_person, expected, "{help:?}");
        let offered = flags.advertised(false)["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation == OPERATION);
        assert_eq!(offered, expected, "{help:?}");
        assert!(!flags.python_operations(false).contains(OPERATION));
    }
}

#[test]
fn only_one_exact_statement_and_its_exact_receipt_or_closed_refusal() {
    session_question_route_person::validate(&payload()).unwrap();
    type Change = Box<dyn Fn(&mut Value)>;
    let changes: Vec<(&str, Change)> = vec![
        ("extra", Box::new(|p| p["extra"] = json!(1))),
        (
            "statement extra",
            Box::new(|p| p["statement"]["extra"] = json!(1)),
        ),
        (
            "signature length",
            Box::new(|p| p["signature"] = json!("x")),
        ),
        (
            "switch",
            Box::new(|p| p["statement"]["person_off"] = json!(0)),
        ),
        (
            "lifetime",
            Box::new(|p| p["statement"]["expires_at"] = json!(4_000_000_000u64)),
        ),
        (
            "control",
            Box::new(|p| p["statement"]["run_id"] = json!("a\nb")),
        ),
        (
            "session",
            Box::new(|p| p["statement"]["session_id"] = json!("x".repeat(37))),
        ),
    ];
    for (name, change) in changes {
        let mut bad = payload();
        change(&mut bad);
        assert!(
            session_question_route_person::validate(&bad).is_err(),
            "{name}"
        );
    }
    let p = payload();
    assert!(session_question_route_person::result_valid(&receipt(), &p));
    for (key, value) in [
        ("request_id", json!("00000000-0000-4000-8000-0000000000ff")),
        ("op", json!("question_routes")),
        ("session_id", json!("00000000-0000-4000-8000-0000000000ff")),
        ("person_off", json!(true)),
        ("durable", json!(false)),
        ("applied", json!(null)),
        ("code", json!("x")),
        ("extra", json!(1)),
    ] {
        let mut bad = receipt();
        bad[key] = value;
        assert!(
            !session_question_route_person::result_valid(&bad, &p),
            "{key}"
        );
    }
    let refusal = |code: &str| {
        json!({"request_id": p["statement"]["request_id"], "op": "question_route_child",
            "accepted": false, "durable": false, "code": code})
    };
    for code in [
        "not_verified",
        "stale_identity",
        "not_found",
        "capacity",
        "invalid_request",
    ] {
        assert!(
            session_question_route_person::result_valid(&refusal(code), &p),
            "{code}"
        );
    }
    // A store failure may straddle a write: uncertain, never a refusal.
    assert!(!session_question_route_person::result_valid(
        &refusal("question_route_store_refused"),
        &p
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn it_forwards_the_statement_to_its_own_service_once() {
    let scratch = Scratch::new("route-person-native");
    let node = connector(&scratch, receipt(), HELP_MARKER);
    let Outcome::Finished(first) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        (first["state"].clone(), first["result"].clone()),
        (json!("completed"), receipt())
    );
    assert_eq!(node.execute(request(1)).await.unwrap(), Outcome::Replayed);
    let p = payload();
    assert_eq!(
        node.native.calls.borrow().as_slice(),
        &[Call::new(["desktop", "request", "--json"])
            .payload(
                json!({"op": "question_route_person", "statement": p["statement"],
                "signature": p["signature"]})
            )
            .failure_json()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn without_the_marker_or_with_another_receipt_nothing_completes() {
    let scratch = Scratch::new("route-person-gated");
    let node = connector(&scratch, receipt(), "");
    let Outcome::Finished(refused) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_ne!(refused["state"], "completed");
    assert!(node.native.calls.borrow().is_empty());
    let scratch = Scratch::new("route-person-mismatch");
    let mut other = receipt();
    other["person_off"] = json!(true);
    let node = connector(&scratch, other, HELP_MARKER);
    let Outcome::Finished(failed) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_ne!(failed["state"], "completed");
}
