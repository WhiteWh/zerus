//! The shared `contracts/host-access-v1` fixtures; the executor's Go tests read
//! the same files.
use super::*;
use serde_json::Value;
use std::path::PathBuf;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts/host-access-v1/fixtures").join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn text(case: &Value) -> Vec<u8> {
    let mut bytes = case["text"].as_str().unwrap().as_bytes().to_vec();
    if let Some(size) = case["pad_to_bytes"].as_u64() {
        bytes.resize(size as usize, b' ');
    }
    bytes
}

#[test]
fn every_invalid_policy_fixture_fails_with_its_code() {
    let policies = fixture("policies.json");
    let host = policies["host"].as_str().unwrap();
    let mut seen = 0;
    for case in policies["invalid"].as_array().unwrap() {
        let result = verify(&text(case), host).map(|_| ()).map_err(PolicyError::code);
        assert_eq!(result, Err(case["expect"].as_str().unwrap()), "{}", case["name"]);
        seen += 1;
    }
    assert!(seen >= 50);
}

#[test]
fn valid_policies_verify_and_bind_their_host() {
    let policies = fixture("policies.json");
    let host = policies["host"].as_str().unwrap();
    for case in policies["valid"].as_array().unwrap() {
        let policy = verify(&text(case), host).unwrap();
        assert_eq!(policy.host, host);
        let other = "0".repeat(64);
        assert_eq!(verify(&text(case), &other).unwrap_err(), PolicyError::HostMismatch);
    }
}

#[test]
fn every_decision_fixture_matches() {
    let policies = fixture("policies.json");
    let host = policies["host"].as_str().unwrap();
    let verified: BTreeMap<String, Policy> = policies["valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| (case["name"].as_str().unwrap().to_owned(), verify(&text(case), host).unwrap()))
        .collect();
    for case in fixture("decisions.json")["cases"].as_array().unwrap() {
        let policy = &verified[case["policy"].as_str().unwrap()];
        let result = authorize(
            policy,
            case["device"].as_str().unwrap(),
            case["now"].as_u64().unwrap(),
            case["operation"].as_str().unwrap(),
            case["project"].as_str().unwrap(),
        );
        let expect = &case["expect"];
        match result {
            Ok(decision) => {
                assert!(expect["allow"].as_bool().unwrap(), "{} allowed", case["name"]);
                assert_eq!(decision.person, expect["person"].as_str().unwrap(), "{}", case["name"]);
                assert_eq!(decision.role.map(Role::name).unwrap_or(""), expect["role"].as_str().unwrap(), "{}", case["name"]);
                let via = match decision.via {
                    Via::Device => "",
                    Via::Member => "member",
                    Via::Operator => "operator",
                };
                assert_eq!(via, expect["via"].as_str().unwrap(), "{}", case["name"]);
            }
            Err(code) => {
                assert!(!expect["allow"].as_bool().unwrap(), "{} refused {code}", case["name"]);
                assert_eq!(code, expect["code"].as_str().unwrap(), "{}", case["name"]);
            }
        }
    }
}

#[test]
fn the_mobile_protocols_rejected_keys_are_rejected_as_person_keys() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("services/mobile/crypto/tests/vectors/rejected-keys-v1.json");
    let vectors: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for key in vectors["ed25519_rejected_public_keys"].as_array().unwrap() {
        assert!(person_key(key.as_str().unwrap()).is_none(), "{key}");
    }
}

#[test]
fn account_references_never_collapse_onto_bare_ids() {
    let refs = fixture("account-refs.json");
    let make = |value: &Value| {
        AccountRef::new(
            value["person"].as_str().unwrap(),
            value["context"].as_str().unwrap(),
            value["provider"].as_str().unwrap(),
            value["account"].as_str().unwrap(),
        )
    };
    for case in refs["keys"].as_array().unwrap() {
        assert_eq!(make(&case["ref"]).unwrap().key(), case["key"].as_str().unwrap());
    }
    for pair in refs["distinct"].as_array().unwrap() {
        let (a, b) = (make(&pair[0]).unwrap(), make(&pair[1]).unwrap());
        assert_ne!(a, b);
        assert_ne!(a.key(), b.key());
        assert_eq!(a.account, b.account, "the fixture pairs share the bare account ID");
    }
    for case in refs["invalid"].as_array().unwrap() {
        assert!(make(case).is_none(), "{case}");
    }
}

#[test]
fn the_statement_is_the_documented_line_format() {
    let statement = enrollment_statement(&"a".repeat(64), &"b".repeat(64), &"c".repeat(64), 1, 20);
    let text = String::from_utf8(statement).unwrap();
    assert!(text.starts_with("zerus-host-access-v1/device-enrollment\nhost="));
    assert!(text.ends_with("\nissued=1\nexpires=20\n"));
    assert!(!text.contains('\r'));
}
