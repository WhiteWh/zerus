//! The person's per-child question route switch (zerus-sa9qn.33): the relay
//! accepts exactly its payload, keeps it a mutation and names it Rust-only.
//! Database-free: only the wire verdict and the operation tables.
use serde_json::{json, Value};
use uuid::Uuid;
use zerus_relay::protocol;

const OPERATION: &str = "session_question_routing";
const RUST_ONLY: &str = include_str!("../../tests/rust-only-operations.json");
const CONTRACT: &str = include_str!(
    "../../../../mobile/android/app/src/test/resources/question-routing-contract.json"
);

fn request(payload: Value) -> Value {
    json!({"request_id":Uuid::new_v4().to_string(),"computer_id":Uuid::new_v4().to_string(),
        "operation":OPERATION,"session":"claude/example/child","payload":payload})
}
fn payload() -> Value {
    json!({"expected_run_id":"run-1","expected_conversation_id":"conversation-1",
        "expected_revision":2,"send_to_parent":false})
}
fn wire(b: &Value) -> u16 {
    protocol::request(b).map_or_else(|e| e.0.as_u16(), |()| 202)
}

#[test]
fn the_exact_payload_is_accepted_and_nothing_else() {
    assert_eq!(wire(&request(payload())), 202);
    // The shared contract's change payload is this exact shape.
    let contract: Value = serde_json::from_str(CONTRACT).unwrap();
    assert_eq!(wire(&request(contract["change"]["payload"].clone())), 202);
    for (field, value) in [
        ("send_to_parent", json!(1)),
        ("send_to_parent", json!("false")),
        ("send_to_parent", Value::Null),
        ("expected_revision", json!(-1)),
        ("expected_revision", json!(1.5)),
        ("expected_revision", json!(true)),
        ("expected_run_id", json!("")),
        ("expected_run_id", json!("x".repeat(129))),
        ("expected_conversation_id", json!("x".repeat(257))),
        ("expected_conversation_id", json!(3)),
        ("muted", json!(true)),
    ] {
        let mut bad = payload();
        bad[field] = value;
        assert_eq!(wire(&request(bad.clone())), 400, "{bad}");
    }
    for field in [
        "expected_run_id",
        "expected_conversation_id",
        "expected_revision",
        "send_to_parent",
    ] {
        let mut missing = payload();
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(wire(&request(missing)), 400, "{field}");
    }
}

#[test]
fn it_is_a_rust_only_mutation_gated_by_its_own_marker() {
    assert!(protocol::OPERATIONS.contains(&OPERATION));
    assert!(!protocol::READS.contains(&OPERATION));
    let table: Value = serde_json::from_str(RUST_ONLY).unwrap();
    let row = table["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["operation"] == OPERATION)
        .expect("listed as Rust-only");
    assert_eq!(
        (row["marker"].as_str(), row["class"].as_str()),
        (Some("question_routing"), Some("mutation"))
    );
    assert_eq!(
        table["markers"]["question_routing"],
        "session-question-routing-v1"
    );
}

const PERSON: &str = "session_question_route_person";
const PERSON_VECTOR: &str =
    include_str!("../../crypto/tests/vectors/question-route-person-v1.json");

fn person_payload() -> Value {
    let vector: Value = serde_json::from_str(PERSON_VECTOR).unwrap();
    json!({"statement": vector["statement"], "signature": vector["signature"]})
}
fn person_request(payload: Value) -> Value {
    json!({"request_id":Uuid::new_v4().to_string(),"computer_id":Uuid::new_v4().to_string(),
        "operation":PERSON,"session":"claude/example/child","payload":payload})
}

#[test]
fn a_phones_signed_statement_is_accepted_only_in_its_exact_shape() {
    // The shared crypto vector's statement and signature are the shape.
    assert_eq!(wire(&person_request(person_payload())), 202);
    type Change = Box<dyn Fn(&mut Value)>;
    let changes: Vec<(&str, Change)> = vec![
        ("extra payload key", Box::new(|p| p["extra"] = json!(1))),
        (
            "no signature",
            Box::new(|p| {
                p.as_object_mut().unwrap().remove("signature");
            }),
        ),
        (
            "short signature",
            Box::new(|p| p["signature"] = json!("x".repeat(85))),
        ),
        ("number signature", Box::new(|p| p["signature"] = json!(1))),
        (
            "extra statement key",
            Box::new(|p| p["statement"]["extra"] = json!(1)),
        ),
        (
            "no request",
            Box::new(|p| {
                p["statement"].as_object_mut().unwrap().remove("request_id");
            }),
        ),
        (
            "string switch",
            Box::new(|p| p["statement"]["person_off"] = json!("true")),
        ),
        (
            "negative time",
            Box::new(|p| p["statement"]["issued_at"] = json!(-1)),
        ),
        (
            "expiry before issue",
            Box::new(|p| p["statement"]["expires_at"] = p["statement"]["issued_at"].clone()),
        ),
        (
            "longer than five minutes",
            Box::new(|p| {
                let issued = p["statement"]["issued_at"].as_u64().unwrap();
                p["statement"]["expires_at"] = json!(issued + 301);
            }),
        ),
        (
            "long session",
            Box::new(|p| p["statement"]["session_id"] = json!("x".repeat(37))),
        ),
        (
            "empty run",
            Box::new(|p| p["statement"]["run_id"] = json!("")),
        ),
        (
            "long conversation",
            Box::new(|p| p["statement"]["conversation_id"] = json!("x".repeat(257))),
        ),
    ];
    for (name, change) in changes {
        let mut payload = person_payload();
        change(&mut payload);
        assert_eq!(wire(&person_request(payload)), 400, "{name}");
    }
}

#[test]
fn the_person_statement_is_a_rust_only_mutation_gated_by_its_own_marker() {
    assert!(protocol::OPERATIONS.contains(&PERSON));
    assert!(!protocol::READS.contains(&PERSON));
    let table: Value = serde_json::from_str(RUST_ONLY).unwrap();
    let row = table["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["operation"] == PERSON)
        .expect("listed as Rust-only");
    assert_eq!(
        (row["marker"].as_str(), row["class"].as_str()),
        (Some("question_route_person"), Some("mutation"))
    );
    assert_eq!(
        table["markers"]["question_route_person"],
        "question-route-person-v1"
    );
}
