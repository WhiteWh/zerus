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
