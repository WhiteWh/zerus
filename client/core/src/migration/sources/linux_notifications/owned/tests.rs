use super::*;
use crate::migration::{Bundle, BundleItem};
use zerus_desktop_migration::{Evidence, MAX_SNAPSHOT_BYTES, SOURCE};
const BUS: &str = "00112233445566778899aabbccddeeff";
fn row(state: &str, id: Option<u32>, nonce: u64) -> Value {
    let token = crate::attention::encode_token(
        json!({"host":"","name":"codex/example/one","notice":"example"})
            .as_object()
            .unwrap(),
    );
    let mut value = json!({"owner":"agent_attention","token":token,"state":state,"original":{
        "request_id":format!("original-request-{nonce}"),"poster_instance":"11111111-1111-4111-8111-111111111111",
        "post_nonce":nonce,"bus_id":BUS,"daemon_owner":":1.0","namespace_generation":2,"actions":["default","open-session"]}});
    if let Some(id) = id {
        value["native_id"] = json!(id);
    }
    value
}
fn envelope() -> Value {
    json!({"version":2,"owner":":1.9","bus_id":BUS,"tokens":{"42":row("delivered",Some(42),1)},
        "uncertain":[row("possible_post",None,2),row("unknown_post",Some(43),3),row("withdrawal_pending",Some(44),4),row("unknown_withdrawal",Some(45),5)],"legacy_uncertain":[],"private_extension":{"keep":"private-original"}})
}
fn bundle(stream: SourceStream) -> Bundle {
    Bundle::new(
        SOURCE,
        false,
        "a".repeat(64),
        "22222222-2222-4222-8222-222222222222".into(),
        1,
        vec![stream],
        64 << 20,
    )
    .unwrap()
}
fn bodies(bytes: &[u8]) -> Vec<Value> {
    let mut values = Vec::new();
    let mut finished = false;
    for item in bundle(next_linux_owned_notifications(bytes).unwrap()) {
        match item.unwrap() {
            BundleItem::Snapshot { reference, reader } => {
                assert!(reference.bytes <= MAX_SNAPSHOT_BYTES);
                reference.verify(reader.get_ref()).unwrap();
                values.push(serde_json::from_slice(reader.get_ref()).unwrap());
            }
            BundleItem::Record(record) => {
                assert_eq!(record.evidence, Evidence::Staged);
                assert!(!record.automatic_approvals);
                assert_eq!(
                    record.sources[0].family,
                    "next-linux-owned-notification-identities"
                );
                assert_eq!(record.sources[0].schema, 2);
                assert_eq!(
                    record.sources[0].format,
                    "linux-notifications-owned-json-v2"
                );
                assert_eq!(record.sources[0].sha256, sha256(bytes));
                assert!(!serde_json::to_string(&record)
                    .unwrap()
                    .contains("private-original"));
                finished = true;
            }
            _ => (),
        }
    }
    assert!(finished);
    values
}
fn original(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for v in &values[1..] {
        assert_eq!(v["kind"], "linux-owned-notification-bytes");
        assert_eq!(v["offset"], bytes.len());
        bytes.extend(STANDARD.decode(v["base64"].as_str().unwrap()).unwrap());
    }
    bytes
}
#[test]
fn original_rows_unknown_fields_order_and_whitespace_survive_without_policy_encoding() {
    let mut value = envelope();
    value["uncertain"][0]["private_row"] = json!({"retain":true});
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    let projection = policy::decode_owned_for(&bytes, Owner::AgentAttention).unwrap();
    assert!(projection.has_unknown());
    assert!(projection.encode().is_err());
    let values = bodies(&bytes);
    assert_eq!(values[0]["raw_entries"], 5);
    assert_eq!(values[0]["unknown_fields"], true);
    assert_eq!(values[0]["file"], policy::FILE);
    assert_eq!(original(&values), bytes);
    assert_eq!(
        serde_json::from_slice::<Value>(&original(&values)).unwrap(),
        value
    );
}
#[test]
fn a_large_original_source_uses_bounded_byte_chunks_even_across_utf8() {
    for row_extension in [false, true] {
        let mut value = envelope();
        if row_extension {
            value["uncertain"][0]["large_private_extension"] = json!("я".repeat(600000));
        } else {
            value["large_private_extension"] = json!("я".repeat(600000));
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let values = bodies(&bytes);
        assert!(values.len() > 3);
        assert_eq!(original(&values), bytes);
    }
}
#[test]
fn wrong_owner_future_invalidations_legacy_and_duplicate_claims_are_not_repaired() {
    let mut values = Vec::new();
    for (field, v) in [
        ("version", json!(1)),
        ("version", json!(3)),
        ("legacy_uncertain", json!(["old-token"])),
        ("invalidations", json!([])),
        ("bus_id", json!("not-a-bus")),
    ] {
        let mut value = envelope();
        value[field] = v;
        values.push(value);
    }
    let mut value = envelope();
    value["tokens"]["42"]["owner"] = json!("software_update");
    value["tokens"]["42"]["token"] = json!("zerus-update:v2:dev:123");
    values.push(value);
    let mut value = envelope();
    value["uncertain"][1] = value["uncertain"][0].clone();
    values.push(value);
    for value in values {
        assert!(matches!(
            next_linux_owned_notifications(&serde_json::to_vec(&value).unwrap()),
            Err(SourceError::UnsupportedSource)
        ));
    }
    assert!(next_linux_owned_notifications(b"null").is_err());
}
#[test]
fn file_and_raw_entry_caps_apply_before_any_final_evidence() {
    assert!(matches!(
        next_linux_owned_notifications(&vec![b' '; FILE_LIMIT as usize + 1]),
        Err(SourceError::TooLarge)
    ));
    let mut value = envelope();
    value["uncertain"] = json!((0..=LIMIT)
        .map(|n| row("possible_post", None, n as u64 + 2))
        .collect::<Vec<_>>());
    // The shared streaming decoder reports its early list-limit refusal as
    // Malformed, which this adapter retains as UnsupportedSource.
    assert!(next_linux_owned_notifications(&serde_json::to_vec(&value).unwrap()).is_err());
}
#[test]
fn changed_source_blocks_the_final_manifest_without_touching_the_original_or_schema_one() {
    let bytes = serde_json::to_vec(&envelope()).unwrap();
    let before = bytes.clone();
    let source = next_linux_owned_notifications(&bytes)
        .unwrap()
        .recheck(|| Ok("b".repeat(64)));
    let mut changed = false;
    for item in bundle(source) {
        match item {
            Ok(BundleItem::Record(_)) => panic!("changed source completed"),
            Err(SourceError::SourceChanged) => changed = true,
            _ => (),
        }
    }
    assert!(changed);
    assert_eq!(bytes, before);
    let old = br#"{"version":1,"owner":":1.9","tokens":{"12":"private-token"}}"#;
    assert_eq!(
        super::super::next_linux_notifications(old)
            .unwrap()
            .identity()
            .family,
        "next-linux-notification-identities"
    );
    assert!(next_linux_owned_notifications(old).is_err());
}
