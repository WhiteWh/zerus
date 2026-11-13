#![cfg(unix)]
//! The person's per-child question route switch (zerus-sa9qn.33): exact
//! mutation and durable receipts, with no real native process.
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
    session_question_routing::{self, HELP_MARKER, OPERATION},
};

const NAME: &str = "claude/example/child";
fn payload() -> Value {
    json!({"expected_run_id":"fixture-run","expected_conversation_id":"fixture-conversation",
        "expected_revision":0,"send_to_parent":false})
}
fn request(n: usize) -> Value {
    json!({"request_id":format!("00000000-0000-4000-8000-{n:012}"),"operation":OPERATION,"session":NAME,"payload":payload()})
}
fn receipt() -> Value {
    json!({"ok":true,"applied":true,"run_id":"fixture-run","conversation_id":"fixture-conversation",
        "question_routing":{"send_to_parent":false,"revision":1}})
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
        ("question-routing <session> --json", false),
        ("session-question-routing-v10", false),
        ("session-question-routing-v1 suffix", false),
        (HELP_MARKER, true),
        ("       session-question-routing-v1\r", true),
    ] {
        let flags = Flags::from_help(help);
        assert_eq!(flags.question_routing, expected, "{help:?}");
        let offered = flags.advertised(false)["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation == OPERATION);
        assert_eq!(offered, expected, "{help:?}");
        // The Python connector's tables never list it.
        assert!(!flags.python_operations(false).contains(OPERATION));
    }
}

#[test]
fn strict_payload_and_receipt_bind_the_revision_value_and_exact_target() {
    session_question_routing::validate(&payload()).unwrap();
    for (key, value) in [
        ("send_to_parent", json!(0)),
        ("send_to_parent", json!("false")),
        ("expected_revision", json!(true)),
        ("expected_revision", json!(-1)),
        ("expected_revision", json!(0.0)),
        ("expected_revision", json!(u64::MAX)),
        ("expected_run_id", json!("")),
        ("expected_run_id", json!("é".repeat(65))),
        ("expected_conversation_id", json!("x".repeat(257))),
        ("expected_run_id", json!("line\nbreak")),
    ] {
        let mut bad = payload();
        bad[key] = value;
        assert!(session_question_routing::validate(&bad).is_err(), "{bad}");
    }
    let mut extra = payload();
    extra["muted"] = json!(true);
    assert!(session_question_routing::validate(&extra).is_err());
    let mut missing = payload();
    missing.as_object_mut().unwrap().remove("send_to_parent");
    assert!(session_question_routing::validate(&missing).is_err());

    assert!(session_question_routing::result_valid(
        &receipt(),
        &payload()
    ));
    for (key, value) in [
        ("send_to_parent", json!(true)),
        ("send_to_parent", json!(1)),
        ("revision", json!(2)),
        ("revision", json!(true)),
        ("extra", json!(1)),
    ] {
        let mut bad = receipt();
        bad["question_routing"][key] = value;
        assert!(
            !session_question_routing::result_valid(&bad, &payload()),
            "{bad}"
        );
    }
    for (key, value) in [
        ("run_id", json!("other")),
        ("conversation_id", json!("other")),
        ("extra", json!(1)),
    ] {
        let mut bad = receipt();
        bad[key] = value;
        assert!(
            !session_question_routing::result_valid(&bad, &payload()),
            "{bad}"
        );
    }
    let mut noop = receipt();
    noop["applied"] = json!(false);
    noop["question_routing"]["revision"] = json!(0);
    assert!(session_question_routing::result_valid(&noop, &payload()));
    // The host's closed refusals, the person-only one included.
    for error in [
        "not_found",
        "stale_identity",
        "conflict",
        "unsupported",
        "agent_refused",
        "capacity",
        "revision_exhausted",
    ] {
        assert!(
            session_question_routing::result_valid(&json!({"ok":false,"error":error}), &payload()),
            "{error}"
        );
    }
    assert!(!session_question_routing::result_valid(
        &json!({"ok":false,"error":"busy"}),
        &payload()
    ));
    assert!(!session_question_routing::result_valid(
        &json!({"ok":false,"error":"conflict","applied":false}),
        &payload()
    ));
}

/// The shared contract the desktop and the phones follow
/// (mobile/android/app/src/test/resources/question-routing-contract.json).
#[test]
fn the_shared_contract_accepts_exactly_its_confirmations_and_closed_refusals() {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../../mobile/android/app/src/test/resources/question-routing-contract.json"
    ))
    .unwrap();
    assert_eq!(
        (contract["operation"].as_str(), contract["marker"].as_str()),
        (Some(OPERATION), Some(HELP_MARKER))
    );
    let payload = &contract["change"]["payload"];
    session_question_routing::validate(payload).unwrap();
    for case in contract["results"].as_array().unwrap() {
        let valid = session_question_routing::result_valid(&case["result"], payload);
        assert_eq!(valid, case["outcome"] != "uncertain", "{}", case["name"]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_receipt_uses_the_exact_payload_and_never_repeats_a_durable_mutation() {
    let scratch = Scratch::new("question-routing-native");
    let node = connector(&scratch, receipt(), HELP_MARKER);
    let Outcome::Finished(first) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_eq!(first["state"], "completed");
    assert_eq!(first["result"], receipt());
    assert_eq!(node.execute(request(1)).await.unwrap(), Outcome::Replayed);
    assert_eq!(
        node.native.calls.borrow().as_slice(),
        &[Call::new(["question-routing", NAME, "--json"])
            .payload(payload())
            .failure_json()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_refusal_is_retained_and_never_retried() {
    let scratch = Scratch::new("question-routing-refused");
    let native = json!({"ok":false,"error":"conflict","run_id":"fixture-run","conversation_id":"fixture-conversation","question_routing":{"send_to_parent":true,"revision":3}});
    let node = connector(&scratch, native.clone(), HELP_MARKER);
    let Outcome::Finished(first) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_eq!(first["result"], native);
    assert_eq!(node.execute(request(1)).await.unwrap(), Outcome::Replayed);
    assert_eq!(node.native.calls.borrow().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn without_the_marker_or_with_a_mismatched_receipt_nothing_completes() {
    let scratch = Scratch::new("question-routing-gated");
    let node = connector(&scratch, receipt(), "");
    let Outcome::Finished(refused) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_ne!(refused["state"], "completed");
    assert!(node.native.calls.borrow().is_empty());
    let scratch = Scratch::new("question-routing-mismatch");
    let mut other = receipt();
    other["question_routing"]["send_to_parent"] = json!(true);
    let node = connector(&scratch, other, HELP_MARKER);
    let Outcome::Finished(failed) = node.execute(request(1)).await.unwrap() else {
        panic!()
    };
    assert_ne!(failed["state"], "completed");
}
