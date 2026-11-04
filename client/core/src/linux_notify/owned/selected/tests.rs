use super::*;
use crate::linux_notify::owned::inspection::{inspect_selected, Inputs, Rejected, Slot};
const BUS: &str = "00112233445566778899aabbccddeeff";
fn token(owner: Owner) -> String {
    match owner {
        Owner::AgentAttention => crate::attention::encode_token(
            json!({"host":"","name":"codex/example/one","notice":"example"})
                .as_object()
                .unwrap(),
        ),
        Owner::SoftwareUpdate => "zerus-update:v2:dev:123".into(),
    }
}
fn entry(owner: Owner, state: &str, id: Option<u32>, nonce: u64) -> Value {
    let tag = match owner {
        Owner::AgentAttention => "agent_attention",
        Owner::SoftwareUpdate => "software_update",
    };
    let instance = match owner {
        Owner::AgentAttention => "11111111-1111-4111-8111-111111111111",
        Owner::SoftwareUpdate => "22222222-2222-4222-8222-222222222222",
    };
    let mut v = json!({"owner":tag,"token":token(owner),"state":state,"original":{
        "request_id":"exact request","poster_instance":instance,"post_nonce":nonce,"bus_id":BUS,
        "daemon_owner":":1.0","namespace_generation":2,"actions":["default","open-session"]}});
    if let Some(id) = id {
        v["native_id"] = json!(id);
    }
    v
}
fn text(v: Value) -> String {
    serde_json::to_string(&v).unwrap()
}
fn envelope(tokens: &str, uncertain: &str, invalidations: Option<&str>) -> Vec<u8> {
    let tail = invalidations
        .map(|v| format!(",\"invalidations\":[{v}]"))
        .unwrap_or_default();
    format!(r#"{{"version":2,"bus_id":"{BUS}","owner":":1.0","tokens":{{{tokens}}},"uncertain":[{uncertain}],"legacy_uncertain":[]{tail}}}"#).into_bytes()
}
fn invalidation() -> Value {
    let mut target = entry(Owner::AgentAttention, "delivered", Some(42), 1);
    target.as_object_mut().unwrap().remove("state");
    json!({"target":target,"reason":"native_id_collision"})
}
fn pair(a: &[u8], u: &[u8], l: Option<&[u8]>) -> Result<inspection::Inspection, Rejected> {
    inspect_selected(Inputs {
        agent: Some(a),
        update: Some(u),
        legacy: l,
    })
}
#[test]
fn explicit_mode_round_trips_every_recognized_field_and_original_namespace() {
    let a = entry(Owner::AgentAttention, "delivered", Some(42), 1);
    let pending = entry(Owner::AgentAttention, "unknown_withdrawal", Some(43), 2);
    let raw = envelope(
        &format!("\"42\":{}", text(a.clone())),
        &text(pending.clone()),
        None,
    );
    let mut value: Value = serde_json::from_slice(&raw).unwrap();
    value["bus_id"] = json!("ffeeddccbbaa99887766554433221100");
    value["owner"] = json!(":1.99");
    let raw = serde_json::to_vec(&value).unwrap();
    let saved = decode_owned_for(&raw, Owner::AgentAttention).unwrap();
    assert_eq!(saved.rows().unwrap().positive_claims().count(), 2);
    assert!(saved
        .rows()
        .unwrap()
        .positive_claims()
        .all(|(o, _)| o.namespace().bus_id == BUS && o.namespace().owner == ":1.0"));
    assert_eq!(
        serde_json::from_slice::<Value>(&saved.encode().unwrap()).unwrap(),
        value
    );
    let old = decode_owned(&raw).unwrap();
    assert!(old.has_unknown());
    assert_eq!(old.encode(), Err(Refused::UnknownFields));
    assert!(!format!("{saved:?}").contains("exact request"));
}
#[test]
fn updates_retain_exact_invalidations_and_agent_mode_refuses_the_field() {
    let target = invalidation();
    let raw = envelope("", "", Some(&text(target.clone())));
    let saved = decode_owned_for(&raw, Owner::SoftwareUpdate).unwrap();
    assert_eq!(saved.raw_entries(), 1);
    assert_eq!(
        saved.rows().unwrap().invalidations()[0]
            .data()
            .target()
            .native_id()
            .get(),
        42
    );
    let out: Value = serde_json::from_slice(&saved.encode().unwrap()).unwrap();
    assert_eq!(out["invalidations"][0], target);
    assert!(decode_owned_for(&raw, Owner::AgentAttention).is_err());
    assert!(decode_owned_for(&envelope("", "", Some("")), Owner::AgentAttention).is_err());
    let old = decode_owned(&raw).unwrap();
    assert!(old.has_unknown());
    assert_eq!(old.encode(), Err(Refused::UnknownFields));
}
#[test]
fn raw_invalidation_padding_is_counted_even_when_canonical_output_is_small() {
    let canonical = text(invalidation());
    assert!(canonical.len() < 16384);
    for len in [16384, 16385] {
        let padded = format!("{{{}{}", " ".repeat(len - canonical.len()), &canonical[1..]);
        assert_eq!(padded.len(), len);
        assert_eq!(
            serde_json::from_str::<Value>(&padded).unwrap(),
            serde_json::from_str::<Value>(&canonical).unwrap()
        );
        let raw = envelope("", "", Some(&padded));
        assert_eq!(
            decode_owned_for(&raw, Owner::SoftwareUpdate).is_ok(),
            len == 16384
        );
    }
}
#[test]
fn escaped_original_value_length_is_not_replaced_with_reencoded_length() {
    let mut v = invalidation();
    let long = crate::attention::encode_token(
        json!({"host":"","name":"codex/example/one","notice":"example","opaque":"x".repeat(2400)})
            .as_object()
            .unwrap(),
    );
    assert!(Owner::AgentAttention.accepts_token(&long));
    v["target"]["token"] = json!(long);
    let canonical = text(v);
    assert!(canonical.len() < 16384);
    let escaped = long
        .bytes()
        .map(|b| format!("\\u{b:04x}"))
        .collect::<String>();
    let raw = canonical.replace(&text(json!(long)), &format!("\"{escaped}\""));
    assert!(raw.len() > 16384);
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        serde_json::from_str::<Value>(&canonical).unwrap()
    );
    assert!(decode_owned_for(&envelope("", "", Some(&raw)), Owner::SoftwareUpdate).is_err());
}
#[test]
fn duplicate_raw_ids_known_fields_originals_and_trailing_data_refuse() {
    let row = text(entry(Owner::AgentAttention, "delivered", Some(42), 1));
    let dup = envelope(&format!("\"42\":{row},\"42\":{row}"), "", None);
    assert_eq!(
        decode_owned_for(&dup, Owner::AgentAttention).unwrap_err(),
        Refused::DuplicateIdentity
    );
    let corrupt = row.replace("\"post_nonce\":1", "\"post_nonce\":1,\"post_nonce\":2");
    assert_ne!(corrupt, row);
    assert!(decode_owned_for(
        &envelope(&format!("\"42\":{corrupt}"), "", None),
        Owner::AgentAttention
    )
    .is_err());
    let pending = text(entry(Owner::AgentAttention, "unknown_post", Some(42), 1));
    assert_eq!(
        decode_owned_for(
            &envelope(&format!("\"42\":{row}"), &pending, None),
            Owner::AgentAttention
        )
        .unwrap_err(),
        Refused::DuplicateIdentity
    );
    let mut raw = envelope(&format!("\"42\":{row}"), "", None);
    raw.extend_from_slice(b" {}");
    assert!(decode_owned_for(&raw, Owner::AgentAttention).is_err());
    let wrong = envelope(&format!("\"042\":{row}"), "", None);
    assert!(decode_owned_for(&wrong, Owner::AgentAttention).is_err());
}
#[test]
fn raw_counts_precede_row_validation_and_all_invalidation_decoding() {
    let a = envelope(
        "",
        &std::iter::repeat_n("null", 4096)
            .collect::<Vec<_>>()
            .join(","),
        None,
    );
    let u = envelope(
        "",
        &std::iter::repeat_n("null", 4096)
            .collect::<Vec<_>>()
            .join(","),
        None,
    );
    let legacy = br#"{"version":1,"owner":":1.0","tokens":{"1":"old"}}"#;
    assert_eq!(pair(&a, &u, Some(legacy)).unwrap_err(), Rejected::TooLarge);
    let u = envelope(
        "",
        &std::iter::repeat_n("null", 4096)
            .collect::<Vec<_>>()
            .join(","),
        Some(
            &std::iter::repeat_n("null", 4097)
                .collect::<Vec<_>>()
                .join(","),
        ),
    );
    assert_eq!(
        decode_owned_for(&u, Owner::SoftwareUpdate).unwrap_err(),
        Refused::TooLarge
    );
}
#[test]
fn pair_collisions_include_uncertain_unknown_and_withdrawal_ids_in_both_directions() {
    for (a_state, u_state) in [
        ("unknown_post", "withdrawal_pending"),
        ("withdrawal_pending", "unknown_withdrawal"),
        ("unknown_withdrawal", "delivered"),
    ] {
        let a = envelope(
            "",
            &text(entry(Owner::AgentAttention, a_state, Some(42), 1)),
            None,
        );
        let u = envelope(
            "",
            &text(entry(Owner::SoftwareUpdate, u_state, Some(42), 1)),
            None,
        );
        assert_eq!(
            pair(&a, &u, None).unwrap_err(),
            Rejected::CrossOwnerCollision
        );
        let mut changed: Value = serde_json::from_slice(&u).unwrap();
        changed["uncertain"][0]["original"]["daemon_owner"] = json!(":1.1");
        assert!(pair(&a, &serde_json::to_vec(&changed).unwrap(), None).is_ok());
    }
}
#[test]
fn optional_pair_refuses_historical_no_id_unknown_and_incomplete_data() {
    let empty = envelope("", "", None);
    for tag in ["possible_post", "unknown_post"] {
        let a = envelope("", &text(entry(Owner::AgentAttention, tag, None, 1)), None);
        assert_eq!(
            pair(&a, &empty, None).unwrap_err(),
            Rejected::UnconfirmedPost(Slot::Agent)
        );
    }
    let a = envelope(
        "",
        &text(json!({"owner":"agent_attention","token":token(Owner::AgentAttention)})),
        None,
    );
    assert_eq!(
        pair(&a, &empty, None).unwrap_err(),
        Rejected::MissingProvenance(Slot::Agent)
    );
    assert_eq!(
        inspect_selected(Inputs {
            agent: Some(&empty),
            update: None,
            legacy: None
        })
        .unwrap_err(),
        Rejected::IncompletePair
    );
    let u = envelope("", "", Some(&text(invalidation())));
    assert_eq!(
        pair(&empty, &u, None).unwrap_err(),
        Rejected::InvalidatedBinding
    );
}
#[test]
fn unknown_root_and_nested_fields_block_reencoding_and_pair_inspection() {
    let a = envelope(
        "",
        &text(entry(Owner::AgentAttention, "unknown_post", Some(42), 1)),
        None,
    );
    let empty = envelope("", "", None);
    for nested in [false, true] {
        let mut v: Value = serde_json::from_slice(&a).unwrap();
        if nested {
            v["uncertain"][0]["original"]["opaque"] = json!(true);
        } else {
            v["opaque"] = json!(true);
        }
        let raw = serde_json::to_vec(&v).unwrap();
        let saved = decode_owned_for(&raw, Owner::AgentAttention).unwrap();
        assert!(saved.has_unknown());
        assert_eq!(saved.encode(), Err(Refused::UnknownFields));
        assert_eq!(
            pair(&raw, &empty, None).unwrap_err(),
            Rejected::UnknownFields(Slot::Agent)
        );
    }
}
#[test]
fn prepared_output_and_outer_padding_share_the_original_file_budget() {
    let a = envelope("", "", None);
    let u = envelope("", "", None);
    let mut legacy = br#"{"version":1,"owner":":1.0","tokens":{}}"#.to_vec();
    legacy.resize(FILE_LIMIT as usize - a.len() - u.len() - 1, b' ');
    assert_eq!(pair(&a, &u, Some(&legacy)).unwrap_err(), Rejected::TooLarge); // update encoder adds invalidations:[]
    legacy.resize(FILE_LIMIT as usize, b' ');
    assert_eq!(pair(&a, &u, Some(&legacy)).unwrap_err(), Rejected::TooLarge); // original combined bytes, before parse
}
#[test]
fn contradictory_original_across_owner_envelopes_is_not_a_new_identity() {
    let a = envelope(
        "",
        &text(entry(Owner::AgentAttention, "unknown_post", Some(42), 1)),
        None,
    );
    let mut update = entry(Owner::SoftwareUpdate, "unknown_post", Some(43), 1);
    update["original"]["poster_instance"] = json!("11111111-1111-4111-8111-111111111111");
    let u = envelope("", &text(update), None);
    assert_eq!(
        pair(&a, &u, None).unwrap_err(),
        Rejected::CrossOwnerOriginal
    );
}
