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
fn row(state: &str, id: Option<u32>) -> Value {
    let mut v = json!({"owner":"agent_attention","token":agent(),"state":state,
        "original":{"request_id":"original request","poster_instance":"11111111-1111-4111-8111-111111111111",
        "post_nonce":1,"bus_id":BUS,"daemon_owner":":1.0","namespace_generation":2,"actions":["default","open-session"]}});
    if let Some(id) = id {
        v["native_id"] = json!(id);
    }
    v
}
fn bytes(v: Value) -> Vec<u8> {
    serde_json::to_vec(&v).unwrap()
}
fn parse(v: Value, p: Placement) -> Result<ClaimRow, Refused> {
    decode_claim(&bytes(v), Owner::AgentAttention, p)
}
fn invalidation(reason: &str) -> Value {
    let mut target = row("delivered", Some(42));
    target.as_object_mut().unwrap().remove("state");
    json!({"target":target,"reason":reason})
}
#[test]
fn every_closed_state_round_trips_and_all_positive_claims_are_inspectable() {
    let mut rows = Vec::new();
    for (i, (tag, id)) in [
        ("possible_post", None),
        ("delivered", Some(42)),
        ("withdrawal_pending", Some(43)),
        ("unknown_post", None),
        ("unknown_post", Some(44)),
        ("unknown_withdrawal", Some(45)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut v = row(tag, id);
        v["original"]["post_nonce"] = json!(i + 1);
        let placement = if tag == "delivered" {
            Placement::Tokens(id.unwrap())
        } else {
            Placement::Uncertain
        };
        let parsed = parse(v.clone(), placement).unwrap();
        let ClaimData::Retained(c) = parsed.data() else {
            panic!("lost provenance")
        };
        assert_eq!(c.tag(), tag);
        assert_eq!(c.native_id().map(NonZeroU32::get), id);
        assert_eq!(c.original().request_id(), "original request");
        assert_eq!(
            c.original().poster_instance(),
            "11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(c.original().namespace().bus_id, BUS);
        assert_eq!(c.original().namespace_generation().get(), 2);
        assert_eq!(
            c.original()
                .actions()
                .iter()
                .map(Action::key)
                .collect::<Vec<_>>(),
            ["default", "open-session"]
        );
        assert!(!format!("{parsed:?} {c:?} {:?}", c.original()).contains("original request"));
        let encoded = parsed.encode().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), v);
        assert!(!decode_claim(&encoded, Owner::AgentAttention, placement)
            .unwrap()
            .missing_provenance());
        rows.push(parsed);
    }
    let rows = Rows::new(Owner::AgentAttention, rows, vec![]).unwrap();
    assert_eq!(rows.claims().len(), 6);
    assert_eq!(
        rows.positive_claims()
            .map(|(_, id)| id.get())
            .collect::<Vec<_>>(),
        [42, 43, 44, 45]
    );
}
#[test]
fn old_rows_stay_historical_and_never_become_delivered() {
    let v = json!({"owner":"agent_attention","token":agent()});
    for placement in [Placement::Tokens(42), Placement::Uncertain] {
        let parsed = parse(v.clone(), placement).unwrap();
        assert!(parsed.missing_provenance());
        assert!(matches!(parsed.data(), ClaimData::Historical(_)));
        assert_eq!(
            serde_json::from_slice::<Value>(&parsed.encode().unwrap()).unwrap(),
            v
        );
        assert_eq!(
            Rows::new(Owner::AgentAttention, vec![parsed], vec![])
                .unwrap()
                .positive_claims()
                .count(),
            0
        );
    }
}
#[test]
fn partial_null_and_contradictory_provenance_never_downgrade_to_historical() {
    for key in ["original", "state"] {
        let mut v = row("delivered", Some(42));
        v.as_object_mut().unwrap().remove(key);
        assert!(parse(v, Placement::Tokens(42)).is_err());
        let mut v = row("delivered", Some(42));
        v[key] = Value::Null;
        assert!(parse(v, Placement::Tokens(42)).is_err());
    }
    for (tag, id) in [
        ("possible_post", Some(42)),
        ("delivered", None),
        ("withdrawal_pending", None),
        ("unknown_withdrawal", None),
        ("future", Some(42)),
        ("unknown_post", Some(0)),
    ] {
        assert!(parse(row(tag, id), Placement::Uncertain).is_err());
    }
}
#[test]
fn positive_map_membership_never_supplies_a_missing_or_different_id() {
    for (tag, id) in [
        ("possible_post", None),
        ("unknown_post", None),
        ("delivered", Some(41)),
    ] {
        assert!(parse(row(tag, id), Placement::Tokens(42)).is_err());
    }
    assert!(parse(row("delivered", Some(42)), Placement::Tokens(0)).is_err());
}
#[test]
fn raw_uuid_request_counters_and_namespace_are_validated_without_normalization() {
    for uuid in [
        "00000000-0000-0000-0000-000000000000",
        "AAAAAAAA-1111-4111-8111-111111111111",
        "11111111111141118111111111111111",
    ] {
        let mut v = row("delivered", Some(42));
        v["original"]["poster_instance"] = json!(uuid);
        assert!(parse(v, Placement::Tokens(42)).is_err());
    }
    for request in [
        "".to_string(),
        "x".repeat(513),
        "nul\0".into(),
        "line\n".into(),
    ] {
        let mut v = row("delivered", Some(42));
        v["original"]["request_id"] = json!(request);
        assert!(parse(v, Placement::Tokens(42)).is_err());
    }
    for key in ["post_nonce", "namespace_generation"] {
        let mut v = row("delivered", Some(42));
        v["original"][key] = json!(0);
        assert!(parse(v, Placement::Tokens(42)).is_err());
    }
    let mut v = row("delivered", Some(42));
    v["original"]["daemon_owner"] = json!("well.known");
    assert!(parse(v, Placement::Tokens(42)).is_err());
}
#[test]
fn allowed_actions_are_nonempty_duplicate_free_and_never_change_owner() {
    for actions in [
        json!([]),
        json!(["default", "default"]),
        json!(["other"]),
        json!(["default", "open-session", "default"]),
    ] {
        let mut v = row("delivered", Some(42));
        v["original"]["actions"] = actions;
        assert!(parse(v, Placement::Tokens(42)).is_err());
    }
    assert!(decode_claim(
        &bytes(row("delivered", Some(42))),
        Owner::SoftwareUpdate,
        Placement::Tokens(42)
    )
    .is_err());
}
#[test]
fn duplicate_known_fields_are_refused_even_in_nested_objects() {
    let text = String::from_utf8(bytes(row("delivered", Some(42)))).unwrap();
    for (old, new) in [
        (
            "\"state\":\"delivered\"",
            "\"state\":\"delivered\",\"state\":\"unknown_post\"",
        ),
        ("\"post_nonce\":1", "\"post_nonce\":1,\"post_nonce\":2"),
        ("\"actions\":[", "\"post_nonce\":2,\"actions\":["),
    ] {
        let corrupt = text.replace(old, new);
        assert_ne!(corrupt, text);
        assert!(decode_claim(
            corrupt.as_bytes(),
            Owner::AgentAttention,
            Placement::Tokens(42)
        )
        .is_err());
    }
}
#[test]
fn duplicate_or_contradictory_placement_of_one_original_is_refused() {
    let a = parse(row("delivered", Some(42)), Placement::Tokens(42)).unwrap();
    let b = parse(row("unknown_post", Some(42)), Placement::Uncertain).unwrap();
    assert_eq!(
        Rows::new(Owner::AgentAttention, vec![a, b], vec![]).unwrap_err(),
        Refused::DuplicateIdentity
    );
    let a = parse(row("delivered", Some(42)), Placement::Tokens(42)).unwrap();
    let mut changed = row("delivered", Some(43));
    changed["original"]["request_id"] = json!("changed");
    let b = parse(changed, Placement::Tokens(43)).unwrap();
    assert_eq!(
        Rows::new(Owner::AgentAttention, vec![a, b], vec![]).unwrap_err(),
        Refused::DuplicateIdentity
    );
}
#[test]
fn exact_agent_invalidations_belong_only_to_explicit_update_envelopes() {
    for reason in [
        "native_id_collision",
        "namespace_changed",
        "withdrawal_conflict",
        "unverified_result",
    ] {
        let v = invalidation(reason);
        let raw = bytes(v.clone());
        assert!(decode_invalidation(&raw, Owner::AgentAttention).is_err());
        let parsed = decode_invalidation(&raw, Owner::SoftwareUpdate).unwrap();
        assert_eq!(parsed.data().reason().tag(), reason);
        assert_eq!(parsed.data().target().native_id().get(), 42);
        assert_eq!(parsed.data().target().original().namespace().bus_id, BUS);
        assert_eq!(
            serde_json::from_slice::<Value>(&parsed.encode().unwrap()).unwrap(),
            v
        );
        assert!(Rows::new(Owner::AgentAttention, vec![], vec![parsed]).is_err());
    }
    let mut v = invalidation("future");
    assert!(decode_invalidation(&bytes(v.clone()), Owner::SoftwareUpdate).is_err());
    v["reason"] = json!("namespace_changed");
    v["target"]["owner"] = json!("software_update");
    assert!(decode_invalidation(&bytes(v), Owner::SoftwareUpdate).is_err());
}
#[test]
fn unknown_fields_at_every_level_remain_visible_and_block_encoding() {
    for path in ["row", "original"] {
        let mut v = row("delivered", Some(42));
        if path == "row" {
            v["opaque"] = json!({"private":true})
        } else {
            v["original"]["opaque"] = json!([1, 2])
        }
        let parsed = parse(v, Placement::Tokens(42)).unwrap();
        assert!(parsed.has_unknown());
        assert_eq!(parsed.encode(), Err(Refused::UnknownFields));
    }
    for path in ["row", "target", "original"] {
        let mut v = invalidation("namespace_changed");
        match path {
            "row" => v["opaque"] = json!(true),
            "target" => v["target"]["opaque"] = json!(true),
            _ => v["target"]["original"]["opaque"] = json!(true),
        }
        let parsed = decode_invalidation(&bytes(v), Owner::SoftwareUpdate).unwrap();
        assert!(parsed.has_unknown());
        assert_eq!(parsed.encode(), Err(Refused::UnknownFields));
    }
}
#[test]
fn raw_limits_apply_to_rows_and_invalidations_before_coalescing() {
    assert_eq!(
        decode_claim(
            &vec![b' '; FILE_LIMIT as usize + 1],
            Owner::AgentAttention,
            Placement::Uncertain
        )
        .unwrap_err(),
        Refused::TooLarge
    );
    assert_eq!(
        decode_invalidation(&vec![b' '; 16385], Owner::SoftwareUpdate).unwrap_err(),
        Refused::TooLarge
    );
    let raw = bytes(json!({"owner":"agent_attention","token":agent()}));
    let rows = (0..LIMIT + 1)
        .map(|_| decode_claim(&raw, Owner::AgentAttention, Placement::Uncertain).unwrap())
        .collect();
    assert_eq!(
        Rows::new(Owner::AgentAttention, rows, vec![]).unwrap_err(),
        Refused::TooLarge
    );
    let update = bytes(json!({"owner":"software_update","token":"zerus-update:v2:dev:123"}));
    let rows = (0..LIMIT)
        .map(|_| decode_claim(&update, Owner::SoftwareUpdate, Placement::Uncertain).unwrap())
        .collect();
    let invalidation = decode_invalidation(
        &bytes(invalidation("native_id_collision")),
        Owner::SoftwareUpdate,
    )
    .unwrap();
    assert_eq!(
        Rows::new(Owner::SoftwareUpdate, rows, vec![invalidation]).unwrap_err(),
        Refused::TooLarge
    );
    let mut padded = row("unknown_post", None);
    padded["opaque"] = json!("x".repeat(FILE_LIMIT as usize / 2));
    let raw = bytes(padded);
    let a = decode_claim(&raw, Owner::AgentAttention, Placement::Uncertain).unwrap();
    let b = decode_claim(&raw, Owner::AgentAttention, Placement::Uncertain).unwrap();
    assert_eq!(
        Rows::new(Owner::AgentAttention, vec![a, b], vec![]).unwrap_err(),
        Refused::TooLarge
    );
}
#[test]
fn existing_envelope_codec_still_refuses_to_rewrite_unintegrated_provenance() {
    let v = json!({"version":2,"bus_id":BUS,"owner":":1.0","tokens":{"42":row("delivered",Some(42))},"uncertain":[],"legacy_uncertain":[]});
    let old = super::super::decode_owned(&bytes(v)).unwrap();
    assert!(old.has_unknown());
    assert_eq!(old.encode(), Err(Refused::UnknownFields));
}
