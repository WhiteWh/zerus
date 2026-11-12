//! The person's latch statement (zerus-sa9qn.50) against its shared vector,
//! which Android and iOS test too. Synthetic fixture keys only.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use zerus_mobile_crypto::*;

const VECTOR: &str = "tests/vectors/question-route-person-v1.json";

fn identity(id: &str, role: Role, seed: u8) -> Identity {
    use hpke::{Deserializable, Kem, Serializable};
    let signing = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let kem =
        <hpke::kem::X25519HkdfSha256 as Kem>::PrivateKey::from_bytes(&[seed + 64; 32]).unwrap();
    Identity::from_record(&IdentityRecord {
        version: VERSION,
        device: DevicePublic {
            device_id: id.into(),
            role,
            key_generation: 1,
            signing_public_key: encode(&signing.verifying_key().to_bytes()),
            encryption_public_key: encode(&hpke::kem::X25519HkdfSha256::sk_to_pk(&kem).to_bytes()),
        },
        signing_private_key: encode(&[seed; 32]),
        encryption_private_key: encode(&[seed + 64; 32]),
    })
    .unwrap()
}
const GATEWAY: &str = "00000000-0000-4000-8000-000000000001";
const PHONE: &str = "00000000-0000-4000-8000-000000000002";
fn statement() -> RoutePersonStatement {
    RoutePersonStatement {
        workspace_id: "workspace-fixture".into(),
        device_id: PHONE.into(),
        gateway_id: GATEWAY.into(),
        session_id: "00000000-0000-4000-8000-0000000000c1".into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        person_off: false,
        request_id: "00000000-0000-4000-8000-0000000000d1".into(),
        issued_at: 1_999_999_900,
        expires_at: 2_000_000_100,
    }
}
fn membership(gateway: &Identity, phone: &Identity) -> MembershipState {
    MembershipState {
        workspace_id: "workspace-fixture".into(),
        revision: 1,
        head: encode(&[7; 32]),
        members: BTreeMap::from([
            (GATEWAY.into(), gateway.public().clone()),
            (PHONE.into(), phone.public().clone()),
        ]),
        revoked_device_ids: BTreeSet::new(),
    }
}

#[test]
fn the_shared_vector_signs_and_verifies_exactly() {
    let gateway = identity(GATEWAY, Role::Computer, b'A');
    let phone = identity(PHONE, Role::Phone, b'B');
    let s = statement();
    let bytes = s.canonical_bytes().unwrap();
    let signature = s.sign(&phone).unwrap();
    let vector = json!({
        "warning": "PUBLIC SYNTHETIC TEST KEYS; NEVER USE IN SERVICE",
        "description": "The person's latch statement of a child's question route (zerus-sa9qn.50): a phone signs it for one gateway with its workspace signing key; the gateway's service applies it at most once before expires_at (seconds). Rust (services/mobile/crypto), Android and iOS check the canonical bytes, and verify the signature with the phone's signing public key.",
        "domain": ROUTE_PERSON_DOMAIN,
        "phone_signing_seed": encode(&[b'B'; 32]),
        "phone_signing_public_key": phone.public().signing_public_key,
        "statement": s,
        "canonical_hex": hex::encode(&bytes),
        "signature": signature,
    });
    if std::env::var_os("ZERUS_WRITE_ROUTE_PERSON_VECTOR").is_some() {
        std::fs::write(
            VECTOR,
            serde_json::to_string_pretty(&vector).unwrap() + "\n",
        )
        .unwrap();
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(VECTOR).unwrap()).unwrap();
    assert_eq!(saved, vector, "the shared vector changed");
    let parsed: RoutePersonStatement = serde_json::from_value(saved["statement"].clone()).unwrap();
    let state = membership(&gateway, &phone);
    parsed.verify(&state, &signature, 2_000_000_000).unwrap();
}

#[test]
fn only_a_current_phone_of_the_workspace_within_its_lifetime_verifies() {
    let gateway = identity(GATEWAY, Role::Computer, b'A');
    let phone = identity(PHONE, Role::Phone, b'B');
    let state = membership(&gateway, &phone);
    let s = statement();
    let signature = s.sign(&phone).unwrap();
    let now = 2_000_000_000;
    assert!(s.verify(&state, &signature, now).is_ok());
    // Within the clock skew before issue, and up to just before expiry.
    assert!(s
        .verify(&state, &signature, s.issued_at - ROUTE_PERSON_SKEW)
        .is_ok());
    assert!(s.verify(&state, &signature, s.expires_at - 1).is_ok());
    assert!(s
        .verify(&state, &signature, s.issued_at - ROUTE_PERSON_SKEW - 1)
        .is_err());
    assert!(s.verify(&state, &signature, s.expires_at).is_err());
    // A computer's signature is never a person's.
    let by_gateway = RoutePersonStatement {
        device_id: GATEWAY.into(),
        ..statement()
    };
    assert!(by_gateway
        .verify(&state, &by_gateway.sign(&gateway).unwrap(), now)
        .is_err());
    // Another workspace, an unknown or revoked phone.
    let mut other = state.clone();
    other.workspace_id = "workspace-other".into();
    assert!(s.verify(&other, &signature, now).is_err());
    let mut revoked = state.clone();
    revoked.members.remove(PHONE);
    revoked.revoked_device_ids.insert(PHONE.into());
    assert!(s.verify(&revoked, &signature, now).is_err());
    // Any field changed after signing.
    let changes: Vec<fn(&mut RoutePersonStatement)> = vec![
        |s| s.person_off = true,
        |s| s.session_id = "00000000-0000-4000-8000-0000000000c2".into(),
        |s| s.gateway_id = "00000000-0000-4000-8000-000000000009".into(),
        |s| s.run_id = "run-2".into(),
        |s| s.conversation_id = "conversation-2".into(),
        |s| s.request_id = "00000000-0000-4000-8000-0000000000d2".into(),
        |s| s.expires_at += 1,
        |s| s.issued_at -= 1,
    ];
    for change in changes {
        let mut changed = statement();
        change(&mut changed);
        assert!(
            changed.verify(&state, &signature, now).is_err(),
            "{changed:?}"
        );
    }
    // A statement longer than its lifetime or with bad fields never frames.
    for bad in [
        RoutePersonStatement {
            expires_at: statement().issued_at + ROUTE_PERSON_LIFETIME + 1,
            ..statement()
        },
        RoutePersonStatement {
            expires_at: statement().issued_at,
            ..statement()
        },
        RoutePersonStatement {
            run_id: String::new(),
            ..statement()
        },
        RoutePersonStatement {
            run_id: "x".repeat(129),
            ..statement()
        },
        RoutePersonStatement {
            conversation_id: "a\nb".into(),
            ..statement()
        },
        RoutePersonStatement {
            session_id: "a b".into(),
            ..statement()
        },
    ] {
        assert!(bad.canonical_bytes().is_err(), "{bad:?}");
    }
}
