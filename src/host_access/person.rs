//! Full-host operations of a caller identified as a person (an OS account
//! bound by the operator to a policy context), not through a device:
//! project history and memory, and handoff continuation. The Go executor's
//! `authorizePerson` agrees on `fixtures/person-decisions.json`.

use super::{Decision, Policy, Role, Via};

/// The person operations and whether each needs the contributor role.
pub fn operation_needs_contributor(operation: &str) -> Option<bool> {
    match operation {
        "history.read" | "memory.read" => Some(false),
        "history.publish" | "memory.write" | "handoff.continue" => Some(true),
        _ => None,
    }
}

/// Decide a person operation on a project. Order (first failure reported):
/// operation, person known, person revoked, project known, membership, role.
pub fn authorize_person(policy: &Policy, person: &str, operation: &str, project: &str) -> Result<Decision, &'static str> {
    let contributor = operation_needs_contributor(operation).ok_or("operation-unknown")?;
    if !policy.labels.contains_key(person) {
        return Err("person-unknown");
    }
    if policy.revoked_persons.contains(person) {
        return Err("person-revoked");
    }
    let members = policy.projects.get(project).ok_or("project-unknown")?;
    let role = *members.get(person).ok_or("not-a-member")?;
    if contributor && role != Role::Contributor {
        return Err("role-insufficient");
    }
    Ok(Decision { person: person.to_string(), role: Some(role), via: Via::Member })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("contracts/host-access-v1/fixtures").join(name);
        serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("json")
    }

    #[test]
    fn every_person_decision_fixture_matches() {
        let policies = fixture("policies.json");
        let host = policies["host"].as_str().unwrap().to_string();
        let mut verified = std::collections::BTreeMap::new();
        for p in policies["valid"].as_array().unwrap() {
            let policy = super::super::verify(p["text"].as_str().unwrap().as_bytes(), &host).expect("valid policy");
            verified.insert(p["name"].as_str().unwrap().to_string(), policy);
        }
        let cases = fixture("person-decisions.json");
        let cases = cases["cases"].as_array().unwrap();
        assert!(cases.len() >= 16);
        for case in cases {
            let policy = &verified[case["policy"].as_str().unwrap()];
            let got = authorize_person(
                policy,
                case["person"].as_str().unwrap(),
                case["operation"].as_str().unwrap(),
                case["project"].as_str().unwrap(),
            );
            let expect = &case["expect"];
            if expect["allow"].as_bool().unwrap() {
                let decision = got.unwrap_or_else(|code| panic!("{}: refused {code}", case["name"]));
                assert_eq!(decision.person, expect["person"].as_str().unwrap(), "{}", case["name"]);
                assert_eq!(decision.role.map(|r| r.name()).unwrap_or(""), expect["role"].as_str().unwrap(), "{}", case["name"]);
                assert_eq!(decision.via, Via::Member, "{}", case["name"]);
            } else {
                assert_eq!(got.err(), Some(expect["code"].as_str().unwrap()), "{}", case["name"]);
            }
        }
    }
}
