use serde_json::{json, Value};
use zerus_client_core::swarm::dialog::{self, Sync};
fn snapshot() -> Value {
    json!({"schema":1,"node_id":"local","swarm_id":"catalog","digest":"d1","initialized":true,"conflicts":[],"peers":{},"machines":[],"organization":{}})
}
fn row(id: usize, alias: &str) -> Value {
    json!({"id":format!("machine-{id}"),"name":format!("Computer {id}"),"connection":alias,"local":false})
}
#[test]
fn computers_follow_qt_local_access_peer_errors_times_and_update_suffix_rules() {
    let mut value = snapshot();
    value["machines"] = json!([{"id":"local","name":"Local","local":true},row(1,""),row(2,"waiting"),row(3,"synced"),row(4,"bad")]);
    value["peers"] = json!({"waiting":{"last_sync":0},"synced":{"last_sync":1793000000.0,"tasks_outdated":true},"bad":{"error":"synthetic SSH error","tasks_outdated":true}});
    let view = dialog::view(&value, 7).unwrap();
    assert_eq!(view.revision, 7);
    assert_eq!(view.swarm_id, "catalog");
    assert_eq!(view.computers[0].access, "This computer");
    assert_eq!(view.computers[0].sync, Sync::Local);
    assert_eq!(view.computers[1].access, "Not configured");
    assert_eq!(view.computers[1].sync, Sync::Shared);
    assert_eq!(view.computers[2].sync, Sync::Waiting);
    assert_eq!(view.computers[3].sync, Sync::Synced { at: 1793000000 });
    assert!(view.computers[3].tasks_outdated);
    assert_eq!(
        view.computers[4].sync,
        Sync::Error {
            text: "synthetic SSH error".into()
        }
    );
    assert!(!view.computers[4].tasks_outdated);
    assert_eq!(value["peers"]["synced"]["last_sync"], 1793000000.0);
}
#[test]
fn full_computer_limit_succeeds_and_overflow_or_duplicate_identity_refuses_whole_view() {
    let mut value = snapshot();
    value["machines"] = Value::Array((0..dialog::COMPUTERS).map(|i| row(i, "")).collect());
    assert_eq!(
        dialog::view(&value, 0).unwrap().computers.len(),
        dialog::COMPUTERS
    );
    value["machines"]
        .as_array_mut()
        .unwrap()
        .push(row(1000, ""));
    assert_eq!(dialog::view(&value, 0).unwrap_err(), dialog::TOO_LARGE);
    value["machines"] = json!([row(1, ""), row(1, "")]);
    assert_eq!(dialog::view(&value, 0).unwrap_err(), dialog::INVALID);
}
#[test]
fn aggregate_utf8_text_and_individual_error_bounds_refuse_without_truncation() {
    let mut value = snapshot();
    value["machines"] = Value::Array((0..300).map(|i| row(i, &format!("peer-{i}"))).collect());
    value["peers"] = serde_json::from_str(&format!(
        "{{{}}}",
        (0..300)
            .map(|i| format!("\"peer-{i}\":{{\"error\":\"{}\"}}", "é".repeat(1900)))
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap();
    assert_eq!(dialog::view(&value, 0).unwrap_err(), dialog::TOO_LARGE);
    value["machines"] = json!([row(1, "peer")]);
    value["peers"] = json!({"peer":{"error":"x".repeat(4001)}});
    assert_eq!(dialog::view(&value, 0).unwrap_err(), dialog::TOO_LARGE);
}
#[test]
fn malformed_shape_field_and_sync_time_never_look_like_healthy_rows() {
    let mut value = snapshot();
    value["machines"] = json!([row(1, "peer")]);
    value["peers"] = json!({"peer":{"last_sync":-1}});
    assert!(dialog::view(&value, 0).is_err());
    value["peers"] = json!({"peer":false});
    assert!(dialog::view(&value, 0).is_err());
    value["peers"] = json!({});
    value["machines"][0]["name"] = json!(false);
    assert!(dialog::view(&value, 0).is_err());
    value["machines"] = json!({});
    assert!(dialog::view(&value, 0).is_err());
}
