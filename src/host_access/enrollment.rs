//! Device enrollment requests of contract `zerus-host-access-v1`: what a
//! person's device sends its host operator. The person's key signs the same
//! enrollment statement a policy device entry carries, so an admitted request
//! becomes that entry unchanged. The Go executor implements the same checks
//! in the same order; `contracts/host-access-v1/fixtures/enrollment-requests.json`
//! holds the shared cases.

use super::{enrollment_statement, host_id, label, person_id, person_key, Policy, CONTRACT};
use ed25519_dalek::Signature;
use serde::Deserialize;

pub const REQUEST_KIND: &str = "device-enrollment-request";
pub const MAX_REQUEST_LIFETIME: u64 = 400 * 24 * 3600;
pub const MAX_REQUEST_AGE: u64 = 7 * 24 * 3600;
const MAX_TIME: u64 = (1 << 53) - 1;
const CLOCK_SKEW: u64 = 300;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub contract: String,
    pub kind: String,
    pub host: String,
    pub person: String,
    pub public_key: String,
    pub label: String,
    pub device: String,
    pub issued: u64,
    pub expires: u64,
    pub signature: String,
}

/// Check a request against the operator's current verified policy at `now`.
/// The order of checks is part of the contract (the first failure's code is
/// reported): contract and kind, host, identity, window, age, signature,
/// revoked device, revoked person, duplicate device.
pub fn check(request: &EnrollmentRequest, policy: &Policy, now: u64) -> Result<(), &'static str> {
    if request.contract != CONTRACT || request.kind != REQUEST_KIND {
        return Err("request-contract");
    }
    if request.host != policy.host {
        return Err("request-host-mismatch");
    }
    let key = person_key(&request.public_key).ok_or("request-identity")?;
    if person_id(&key) != request.person || !host_id(&request.device) || !label(&request.label) {
        return Err("request-identity");
    }
    if request.issued >= request.expires || request.expires - request.issued > MAX_REQUEST_LIFETIME || request.expires > MAX_TIME {
        return Err("request-window");
    }
    if now + CLOCK_SKEW < request.issued || now >= request.expires || now > request.issued + MAX_REQUEST_AGE {
        return Err("request-stale");
    }
    let signature = super::decode_b64::<64>(&request.signature).ok_or("request-signature")?;
    let statement = enrollment_statement(&request.host, &request.person, &request.device, request.issued, request.expires);
    key.verify_strict(&statement, &Signature::from_bytes(&signature)).map_err(|_| "request-signature")?;
    if policy.revoked_devices.contains(&request.device) {
        return Err("request-device-revoked");
    }
    if policy.revoked_persons.contains(&request.person) {
        return Err("request-person-revoked");
    }
    if policy.devices.contains_key(&request.device) {
        return Err("request-duplicate-device");
    }
    Ok(())
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
    fn every_enrollment_request_fixture_matches() {
        let policies = fixture("policies.json");
        let host = policies["host"].as_str().unwrap().to_string();
        let mut verified = std::collections::BTreeMap::new();
        for p in policies["valid"].as_array().unwrap() {
            let policy = super::super::verify(p["text"].as_str().unwrap().as_bytes(), &host).expect("valid policy");
            verified.insert(p["name"].as_str().unwrap().to_string(), policy);
        }
        let requests = fixture("enrollment-requests.json");
        let now = requests["now"].as_u64().unwrap();
        let cases = requests["cases"].as_array().unwrap();
        assert!(cases.len() >= 15);
        for case in cases {
            let request: EnrollmentRequest = serde_json::from_value(case["request"].clone()).expect("request shape");
            let policy = &verified[case["policy"].as_str().unwrap()];
            let got = check(&request, policy, now).err().unwrap_or("");
            assert_eq!(got, case["code"].as_str().unwrap(), "{}", case["name"]);
        }
    }
}
