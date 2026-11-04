use super::*;
use serde_json::{json, Value};

const BUS: &str = "00112233445566778899aabbccddeeff";
fn agent() -> String {
    crate::attention::encode_token(
        json!({"host":"","name":"codex/example/one","notice":"example"})
            .as_object()
            .unwrap(),
    )
}
fn entry(owner: &str, token: &str) -> Value {
    json!({"owner":owner,"token":token})
}
fn wire() -> Value {
    json!({"version":2,"bus_id":BUS,"owner":":1.0","tokens":{"42":entry("agent_attention",&agent())},
        "uncertain":[entry("software_update","zerus-update:v2:dev:123")],"legacy_uncertain":["old exact edge"]})
}
fn bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn explicit_owners_and_exact_tokens_round_trip_without_becoming_authority() {
    let saved = decode_owned(&bytes(wire())).unwrap();
    assert_eq!(
        saved.namespace().unwrap(),
        &Namespace {
            bus_id: BUS.into(),
            owner: ":1.0".into()
        }
    );
    let (id, token) = saved.bindings().next().unwrap();
    assert_eq!(id, 42);
    assert_eq!(token.owner(), Owner::AgentAttention);
    assert_eq!(token.token(), agent());
    assert_eq!(
        saved.uncertain().next().unwrap().owner(),
        Owner::SoftwareUpdate
    );
    assert_eq!(
        saved.legacy_uncertain().collect::<Vec<_>>(),
        ["old exact edge"]
    );
    assert_eq!(saved.raw_entries(), 3);
    assert!(!saved.has_unknown());
    assert_eq!(
        serde_json::from_slice::<Value>(&saved.encode().unwrap()).unwrap(),
        wire()
    );
}
#[test]
fn expected_owner_validation_never_classifies_or_changes_a_token() {
    assert_eq!(
        ScopedToken::new(Owner::SoftwareUpdate, agent()),
        Err(Refused::InvalidIdentity)
    );
    assert_eq!(
        ScopedToken::new(Owner::AgentAttention, "zerus-update:v2:dev:123".into()),
        Err(Refused::InvalidIdentity)
    );
    let exact = agent() + "=";
    assert_eq!(
        ScopedToken::new(Owner::AgentAttention, exact.clone())
            .unwrap()
            .token(),
        exact
    );
}
#[test]
fn schema1_raw_duplicate_ids_preserve_every_distinct_token_as_unbound() {
    let original=br#"{"version":1,"bus_id":"00112233445566778899aabbccddeeff","owner":":1.0","tokens":{"42":"first","42":"second","042":"third"},"uncertain":["fourth","first"]}"#;
    let saved = quarantine_legacy(original).unwrap();
    assert_eq!(saved.raw_entries(), 5);
    assert_eq!(
        saved.legacy_uncertain().collect::<Vec<_>>(),
        ["first", "fourth", "second", "third"]
    );
    assert!(saved.namespace().is_none());
    assert_eq!(saved.bindings().len(), 0);
    assert_eq!(saved.uncertain().len(), 0);
    let converted = decode_owned(&saved.encode().unwrap()).unwrap();
    assert_eq!(converted.legacy_uncertain().len(), 4);
    assert!(converted.namespace().is_none());
}
#[test]
fn raw_legacy_quota_is_checked_before_duplicate_ids_or_tokens_coalesce() {
    let ids = std::iter::repeat_n("\"42\":\"same\"", LEGACY_LIMIT + 1)
        .collect::<Vec<_>>()
        .join(",");
    let original = format!("{{\"version\":1,\"owner\":\":1.0\",\"tokens\":{{{ids}}}}}");
    assert_eq!(
        quarantine_legacy(original.as_bytes()).unwrap_err(),
        Refused::LegacyCapacity
    );
    // This intentionally demonstrates why the convenience cannot replace raw input.
    let typed = super::super::decode_saved(original.as_bytes()).unwrap();
    assert_eq!(
        OwnedSaved::from_legacy(typed)
            .unwrap()
            .legacy_uncertain()
            .len(),
        1
    );
    let raw =
        json!({"version":1,"owner":":1.0","tokens":{},"uncertain":vec!["same";LEGACY_LIMIT+1]});
    assert_eq!(
        quarantine_legacy(&bytes(raw)).unwrap_err(),
        Refused::LegacyCapacity
    );
}
#[test]
fn legacy_limit_reserves_headroom_and_refuses_wholesale() {
    let original =
        json!({"version":1,"owner":":1.0","tokens":{},"uncertain":vec!["old";LEGACY_LIMIT]});
    let saved = quarantine_legacy(&bytes(original)).unwrap();
    assert_eq!(saved.raw_entries(), LEGACY_LIMIT);
    assert_eq!(saved.legacy_uncertain().len(), 1);
    let mut typed = Saved {
        version: 1,
        ..Saved::default()
    };
    typed.tokens = (1..=LEGACY_LIMIT as u32 + 1)
        .map(|id| (id, format!("old-{id}")))
        .collect();
    assert_eq!(
        OwnedSaved::from_legacy(typed).unwrap_err(),
        Refused::LegacyCapacity
    );
    assert_eq!(LEGACY_LIMIT, 4096);
    assert_eq!(LIMIT - LEGACY_LIMIT, 4096);
}
#[test]
fn owned_raw_combined_count_is_checked_before_set_coalescing() {
    let mut value = wire();
    value["uncertain"] = json!(vec![
        entry("software_update", "zerus-update:v2:dev:123");
        LIMIT - 2
    ]);
    assert_eq!(
        decode_owned(&bytes(value.clone())).unwrap().raw_entries(),
        LIMIT
    );
    value["uncertain"]
        .as_array_mut()
        .unwrap()
        .push(entry("software_update", "zerus-update:v2:dev:123"));
    assert_eq!(decode_owned(&bytes(value)).unwrap_err(), Refused::TooLarge);
    let mut value = wire();
    value["legacy_uncertain"] = json!(vec!["old"; LEGACY_LIMIT + 1]);
    assert_eq!(
        decode_owned(&bytes(value)).unwrap_err(),
        Refused::LegacyCapacity
    );
}
#[test]
fn owned_ids_are_positive_canonical_u32_and_never_silently_last_wins() {
    for id in ["0", "-1", "01", "+1", "4294967296", "id", ""] {
        let mut value = wire();
        value["tokens"] = json!({id:entry("agent_attention",&agent())});
        assert!(decode_owned(&bytes(value)).is_err(), "{id}");
    }
    let token = serde_json::to_string(&entry("agent_attention", &agent())).unwrap();
    let raw=format!("{{\"version\":2,\"bus_id\":\"{BUS}\",\"owner\":\":1.0\",\"tokens\":{{\"42\":{token},\"42\":{token}}},\"uncertain\":[],\"legacy_uncertain\":[]}}");
    assert_eq!(
        decode_owned(raw.as_bytes()).unwrap_err(),
        Refused::DuplicateIdentity
    );
}
#[test]
fn duplicate_known_root_and_record_identity_fields_are_rejected() {
    let raw = String::from_utf8(bytes(wire())).unwrap();
    for extra in [
        "\"version\":2,",
        "\"owner\":\":1.0\",",
        "\"tokens\":{},",
        "\"bus_id\":\"\",",
        "\"uncertain\":[],",
        "\"legacy_uncertain\":[],",
    ] {
        let duplicate = format!("{{{extra}{}", &raw[1..]);
        assert!(decode_owned(duplicate.as_bytes()).is_err());
    }
    let token = serde_json::to_string(&agent()).unwrap();
    for row in [
        format!(
            "{{\"owner\":\"agent_attention\",\"owner\":\"agent_attention\",\"token\":{token}}}"
        ),
        format!("{{\"owner\":\"agent_attention\",\"token\":{token},\"token\":{token}}}"),
    ] {
        let raw=format!("{{\"version\":2,\"bus_id\":\"{BUS}\",\"owner\":\":1.0\",\"tokens\":{{\"1\":{row}}},\"uncertain\":[],\"legacy_uncertain\":[]}}");
        assert!(decode_owned(raw.as_bytes()).is_err());
    }
}
#[test]
fn unknown_root_and_record_fields_block_encoding_without_normalizing_evidence() {
    for field in ["root", "binding", "uncertain"] {
        let mut value = wire();
        match field {
            "root" => value["opaque"] = json!({"private":[1,2,3]}),
            "binding" => value["tokens"]["42"]["opaque"] = json!({"private":true}),
            _ => value["uncertain"][0]["opaque"] = json!(["private"]),
        }
        let original = bytes(value);
        let before = original.clone();
        let saved = decode_owned(&original).unwrap();
        assert!(saved.has_unknown());
        assert_eq!(saved.encode().unwrap_err(), Refused::UnknownFields);
        assert_eq!(original, before);
    }
    let raw = br#"{"version":1,"owner":":1.0","tokens":{"1":"edge"},"future":{"private":true}}"#;
    let saved = quarantine_legacy(raw).unwrap();
    assert!(saved.has_unknown());
    assert_eq!(saved.encode().unwrap_err(), Refused::UnknownFields);
}
#[test]
fn absent_namespace_is_explicit_and_cannot_carry_native_bindings() {
    let mut value = wire();
    value["bus_id"] = json!("");
    value["owner"] = json!("");
    assert_eq!(
        decode_owned(&bytes(value.clone())).unwrap_err(),
        Refused::InvalidNamespace
    );
    value["tokens"] = json!({});
    assert!(decode_owned(&bytes(value.clone()))
        .unwrap()
        .namespace()
        .is_none());
    for owner in [
        "org.freedesktop.Notifications",
        ":1",
        ":.1",
        ":1.",
        ":1.я",
        ":1.0/other",
    ] {
        let mut bad = value.clone();
        bad["bus_id"] = json!(BUS);
        bad["owner"] = json!(owner);
        assert_eq!(
            decode_owned(&bytes(bad)).unwrap_err(),
            Refused::InvalidNamespace
        );
    }
    for bus in ["", "bad", "00112233445566778899aabbccddeefg"] {
        let mut bad = value.clone();
        bad["bus_id"] = json!(bus);
        bad["owner"] = json!(":1.0");
        assert_eq!(
            decode_owned(&bytes(bad)).unwrap_err(),
            Refused::InvalidNamespace
        );
    }
}
#[test]
fn malformed_future_missing_or_wrong_owner_data_refuses() {
    for version in [0, 1, 3] {
        let mut value = wire();
        value["version"] = json!(version);
        assert!(decode_owned(&bytes(value)).is_err());
    }
    for field in [
        "version",
        "bus_id",
        "owner",
        "tokens",
        "uncertain",
        "legacy_uncertain",
    ] {
        let mut value = wire();
        value.as_object_mut().unwrap().remove(field);
        assert!(decode_owned(&bytes(value)).is_err(), "{field}");
    }
    for tag in ["AgentAttention", "agent", "software-update", ""] {
        let mut value = wire();
        value["tokens"]["42"]["owner"] = json!(tag);
        assert!(decode_owned(&bytes(value)).is_err());
    }
    let mut value = wire();
    value["tokens"]["42"] = entry("software_update", &agent());
    assert_eq!(
        decode_owned(&bytes(value)).unwrap_err(),
        Refused::InvalidIdentity
    );
    for raw in [b"null".as_slice(), b"[]", b"{}", b"{", b"{} {}", &[0xff]] {
        assert!(decode_owned(raw).is_err());
    }
}
#[test]
fn raw_bytes_and_legacy_utf8_bounds_are_enforced() {
    assert_eq!(
        decode_owned(&vec![b' '; FILE_LIMIT as usize + 1]).unwrap_err(),
        Refused::TooLarge
    );
    for token in ["".into(), "я".repeat(8193)] {
        let raw = bytes(json!({"version":1,"owner":":1.0","tokens":{"1":token}}));
        assert_eq!(
            quarantine_legacy(&raw).unwrap_err(),
            Refused::InvalidIdentity
        );
    }
    let raw = bytes(json!({"version":1,"owner":":1.0","tokens":{"1":"я".repeat(8192)}}));
    assert!(quarantine_legacy(&raw).is_ok());
}
#[test]
fn rollback_uses_a_separate_filename_and_the_schema1_decoder_stays_unchanged() {
    assert_eq!(FILE, "linux-notifications-owned.json");
    assert_ne!(FILE, "linux-notifications.json");
    let original=br#"{"version":1,"bus_id":"00112233445566778899aabbccddeeff","owner":":1.0","tokens":{"42":"legacy"},"uncertain":[]}"#;
    let before = original.to_vec();
    let converted = quarantine_legacy(original).unwrap().encode().unwrap();
    assert_eq!(original, before.as_slice());
    assert_eq!(
        super::super::decode_saved(original)
            .unwrap()
            .tokens
            .get(&42)
            .unwrap(),
        "legacy"
    );
    assert!(super::super::decode_saved(&converted).is_err());
}

#[test]
fn typed_conversion_cannot_expand_escaped_tokens_past_the_output_byte_limit() {
    let saved = Saved {
        version: 1,
        uncertain: (0..LEGACY_LIMIT)
            .map(|index| format!("{index}:{}", "\u{1}".repeat(1024)))
            .collect(),
        ..Saved::default()
    };
    assert!(saved.valid());
    assert_eq!(
        OwnedSaved::from_legacy(saved).unwrap_err(),
        Refused::TooLarge
    );
}
