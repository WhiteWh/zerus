use super::*;
use serde_json::{json, Value};
const BUS: &str = "00112233445566778899aabbccddeeff";
fn agent_token() -> String {
    crate::attention::encode_token(
        json!({"host":"","name":"codex/example/one","notice":"example"})
            .as_object()
            .unwrap(),
    )
}
fn entry(owner: Owner) -> Value {
    match owner {
        Owner::AgentAttention => json!({"owner":"agent_attention","token":agent_token()}),
        Owner::SoftwareUpdate => {
            json!({"owner":"software_update","token":"zerus-update:v2:dev:123"})
        }
    }
}
fn envelope(owner: Owner, id: u32) -> Value {
    json!({"version":2,"bus_id":BUS,"owner":":1.0","tokens":{id.to_string():entry(owner)},"uncertain":[],"legacy_uncertain":[]})
}
fn bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}
fn check<'a>(
    agent: Option<&'a [u8]>,
    update: Option<&'a [u8]>,
    legacy: Option<&'a [u8]>,
) -> Result<Inspection, Rejected> {
    inspect(Inputs {
        agent,
        update,
        legacy,
    })
}
#[test]
fn explicit_roles_and_one_raw_legacy_home_are_counted_once() {
    let a = bytes(envelope(Owner::AgentAttention, 1));
    let u = bytes(envelope(Owner::SoftwareUpdate, 2));
    let l = br#"{"version":1,"owner":":1.0","tokens":{"1":"first","1":"second"}}"#;
    let view = check(Some(&a), Some(&u), Some(l)).unwrap();
    assert_eq!(view.raw_entries(), 4);
    assert_eq!(view.raw_bytes(), a.len() + u.len() + l.len());
    assert_eq!(
        view.data(Slot::Legacy)
            .unwrap()
            .legacy_uncertain()
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(
        view.data(Slot::Agent)
            .unwrap()
            .bindings()
            .next()
            .unwrap()
            .1
            .owner(),
        Owner::AgentAttention
    );
    assert_eq!(
        view.data(Slot::Update)
            .unwrap()
            .bindings()
            .next()
            .unwrap()
            .1
            .owner(),
        Owner::SoftwareUpdate
    );
    assert!(!format!("{view:?}").contains("zerus-update"));
}
#[test]
fn neither_slot_accepts_the_other_owner_in_bindings_or_uncertainty() {
    for (owner, slot) in [
        (Owner::AgentAttention, Slot::Update),
        (Owner::SoftwareUpdate, Slot::Agent),
    ] {
        for pending in [false, true] {
            let mut value = envelope(owner, 1);
            if pending {
                value["tokens"] = json!({});
                value["uncertain"] = json!([entry(owner)]);
            }
            let raw = bytes(value);
            let result = if slot == Slot::Agent {
                check(Some(&raw), None, None)
            } else {
                check(None, Some(&raw), None)
            };
            assert_eq!(result.unwrap_err(), Rejected::WrongOwner(slot));
        }
    }
}
#[test]
fn owned_envelopes_cannot_copy_the_shared_legacy_quarantine() {
    for (owner, slot) in [
        (Owner::AgentAttention, Slot::Agent),
        (Owner::SoftwareUpdate, Slot::Update),
    ] {
        let mut value = envelope(owner, 1);
        value["legacy_uncertain"] = json!(["shared"]);
        let raw = bytes(value);
        let result = if slot == Slot::Agent {
            check(Some(&raw), None, None)
        } else {
            check(None, Some(&raw), None)
        };
        assert_eq!(result.unwrap_err(), Rejected::DuplicatedLegacy(slot));
    }
}
#[test]
fn a_shared_native_id_refuses_both_inputs_without_rewriting_either() {
    let a = bytes(envelope(Owner::AgentAttention, 42));
    let u = bytes(envelope(Owner::SoftwareUpdate, 42));
    let before = (a.clone(), u.clone());
    assert_eq!(
        check(Some(&a), Some(&u), None).unwrap_err(),
        Rejected::CrossOwnerCollision
    );
    assert_eq!((a, u), before);
}
#[test]
fn bus_generation_and_unique_daemon_owner_scope_the_collision() {
    let a = bytes(envelope(Owner::AgentAttention, 42));
    for (bus, owner, collides) in [
        (BUS, ":1.0", true),
        ("00112233445566778899AABBCCDDEEFF", ":1.0", true),
        ("ffeeddccbbaa99887766554433221100", ":1.0", false),
        (BUS, ":1.1", false),
    ] {
        let mut value = envelope(Owner::SoftwareUpdate, 42);
        value["bus_id"] = json!(bus);
        value["owner"] = json!(owner);
        let u = bytes(value);
        assert_eq!(check(Some(&a), Some(&u), None).is_err(), collides);
    }
}
#[test]
fn combined_raw_entries_cannot_hide_behind_three_individual_bounds_or_sets() {
    let mut agent = envelope(Owner::AgentAttention, 1);
    agent["tokens"] = json!({});
    agent["uncertain"] = json!(vec![entry(Owner::AgentAttention); 4096]);
    let a = bytes(agent);
    let u = bytes(envelope(Owner::SoftwareUpdate, 2));
    let l = bytes(json!({"version":1,"owner":":1.0","tokens":{},"uncertain":vec!["old";4096]}));
    assert_eq!(
        check(Some(&a), Some(&u), Some(&l)).unwrap_err(),
        Rejected::TooLarge
    );
    assert_eq!(check(Some(&a), None, Some(&l)).unwrap().raw_entries(), 8192);
}
#[test]
fn combined_bytes_are_checked_before_any_parse_or_admission() {
    let a = vec![b' '; (FILE_LIMIT / 2) as usize];
    let u = vec![b' '; (FILE_LIMIT / 2) as usize + 1];
    assert_eq!(
        check(Some(&a), Some(&u), None).unwrap_err(),
        Rejected::TooLarge
    );
}
#[test]
fn unknown_or_incomplete_inputs_refuse_the_entire_inspection() {
    let mut value = envelope(Owner::AgentAttention, 1);
    value["opaque"] = json!(true);
    let a = bytes(value);
    assert_eq!(
        check(Some(&a), None, None).unwrap_err(),
        Rejected::UnknownFields(Slot::Agent)
    );
    let l = br#"{"version":1,"owner":":1.0","tokens":{},"opaque":true}"#;
    assert_eq!(
        check(None, None, Some(l)).unwrap_err(),
        Rejected::UnknownFields(Slot::Legacy)
    );
    assert!(matches!(
        check(None, Some(b"{"), None),
        Err(Rejected::Invalid {
            slot: Slot::Update,
            ..
        })
    ));
}
#[test]
fn hashes_observe_only_the_supplied_bytes_and_absence_is_not_an_empty_file() {
    let first = br#"{"version":1,"owner":":1.0","tokens":{"1":"first"}}"#;
    let second = br#"{"version":1,"owner":":1.0","tokens":{"1":"second"}}"#;
    let before = check(None, None, Some(first)).unwrap();
    let after = check(None, None, Some(second)).unwrap();
    assert_eq!(
        before.sha256(Slot::Legacy).unwrap(),
        format!("{:x}", Sha256::digest(first))
    );
    assert_ne!(before.sha256(Slot::Legacy), after.sha256(Slot::Legacy));
    assert!(check(None, None, None)
        .unwrap()
        .sha256(Slot::Legacy)
        .is_none());
    assert!(check(None, None, Some(b"")).is_err());
}
