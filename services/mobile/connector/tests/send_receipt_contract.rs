use serde_json::{json, Value};
use zerus_mobile_connector::{capabilities::Flags, journal::is_mutation, send_receipt};
use zerus_relay::{json as codec, protocol, send_receipt as contract};
const READ: &str = "11111111-1111-4111-8111-111111111111";
fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("../../vectors/send_receipt/intents.json")).unwrap()
        ["vectors"]
        .as_array()
        .unwrap()
        .clone()
}
fn request(intent: Value) -> Value {
    json!({"request_id":READ,"operation":"send_receipt","computer_id":intent["computer_id"],"session":intent["session"],
        "payload":{"schema":1,"request_id":READ,"intent_sha256":contract::intent_digest(&intent).unwrap(),"intent":intent}})
}
#[test]
fn byte_goldens_keep_two_digests_options_normalization_crlf_and_whitespace_distinct() {
    for v in vectors() {
        let i = &v["intent"];
        assert_eq!(codec::canonical(i).unwrap(), v["canonical"]);
        assert_eq!(contract::intent_digest(i).unwrap(), v["intent_sha256"]);
        assert_eq!(
            send_receipt::native_digest(i).unwrap(),
            v["terminal_python_default_sha256"]
        );
        let mut compact = i.clone();
        compact["native_encoding"] = json!("compact");
        assert_eq!(
            send_receipt::native_digest(&compact).unwrap(),
            v["terminal_compact_sha256"]
        );
        for provider in ["dsh", "opencode"] {
            let mut native = i.clone();
            native["provider"] = json!(provider);
            assert_eq!(
                send_receipt::native_digest(&native).unwrap(),
                v["native_agent_sha256"]
            );
        }
        assert_ne!(
            contract::intent_digest(i).unwrap(),
            send_receipt::native_digest(i).unwrap()
        );
        assert_eq!(
            protocol::request(&request(i.clone())).is_ok(),
            v["admitted"] == true,
            "{}",
            v["id"]
        );
    }
}
#[test]
fn exact_marker_and_read_class_are_independent_of_send_and_python() {
    assert!(protocol::READS.contains(&"send_receipt"));
    assert!(!is_mutation("send_receipt"));
    assert!(!protocol::READ_SQL.contains("send_receipt"));
    for help in [
        "",
        "prefix-delivery-receipt-v1",
        "delivery-receipt-v10",
        "delivery-receipt-v1-suffix",
    ] {
        assert!(!Flags::from_help(help).advertised(false)["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("send_receipt")));
    }
    let flags = Flags::from_help(" delivery-receipt-v1 \n");
    assert!(!flags.python_operations(true).contains("send_receipt"));
    assert!(flags.advertised(false)["operations"]
        .as_array()
        .unwrap()
        .contains(&json!("send_receipt")));
}
#[test]
fn incomplete_foreign_unbounded_or_extra_intents_are_refused() {
    let base = request(vectors()[0]["intent"].clone());
    for key in ["schema", "request_id", "intent_sha256", "intent"] {
        let mut bad = base.clone();
        bad["payload"].as_object_mut().unwrap().remove(key);
        assert!(protocol::request(&bad).is_err(), "{key}");
    }
    for key in [
        "computer_id",
        "machine_id",
        "session_id",
        "session",
        "provider",
        "native_encoding",
        "payload",
        "schema",
    ] {
        let mut bad = base.clone();
        bad["payload"]["intent"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(protocol::request(&bad).is_err(), "{key}");
    }
    for (field, value) in [
        ("text", json!("x".repeat(65537))),
        ("attachments", json!([{}])),
        ("future", json!(true)),
        ("request_id", json!(READ)),
    ] {
        let mut intent = base["payload"]["intent"].clone();
        intent["payload"][field] = value;
        assert!(protocol::request(&request(intent)).is_err(), "{field}");
    }
    for field in ["computer_id", "session"] {
        let mut bad = base.clone();
        bad[field] = json!("other");
        assert!(protocol::request(&bad).is_err(), "{field}");
    }
}
