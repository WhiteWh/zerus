use super::*;
use serde_json::Value;

const HASH: &str = "84ff9e4901c3df222b879922c756dcd364ab04300d5d3c83916320dd250b72ff";

#[test]
fn canonical_profile_bytes_and_domain_hash_are_a_pinned_compatibility_contract() {
    let definition = BACKGROUND_V1;
    assert_eq!(definition.id(), "g1-background-v1");
    assert_eq!(definition.version(), 1);
    assert_eq!(definition.canonical_bytes().len(), 448);
    assert_eq!(definition.hash(), HASH);
    assert!(definition.canonical_bytes().is_ascii());
    assert!(!definition.canonical_bytes().contains(&b'\r'));
    assert!(!definition.canonical_bytes().contains(&b'\n'));
    let data: Value = serde_json::from_slice(definition.canonical_bytes()).unwrap();
    assert_eq!(
        serde_json::to_vec(&data).unwrap(),
        definition.canonical_bytes()
    );
    assert_eq!(data["id"], definition.id());
    assert_eq!(data["version"], definition.version());
    assert_eq!(data["native_notice_owner"], "agent_attention");
    assert_eq!(data["roles"].as_array().unwrap().len(), 6);
    for (row, role) in data["roles"]
        .as_array()
        .unwrap()
        .iter()
        .zip(definition.roles())
    {
        assert_eq!(row["role"], definition.role_name(*role).unwrap());
        assert_eq!(
            row["permissions"],
            serde_json::to_value(
                definition
                    .permissions(*role)
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    }
    assert_ne!(hash_without_domain(definition.canonical_bytes()), HASH);
}

fn hash_without_domain(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn exactly_the_six_approved_roles_are_included_and_native_scope_is_agent_only() {
    let definition = BACKGROUND_V1;
    let expected = [
        Role::Fleet,
        Role::Attention,
        Role::NativePosting,
        Role::Badges,
        Role::ReadDelivery,
        Role::Presence,
    ];
    assert_eq!(definition.roles(), expected);
    assert_eq!(definition.native_notice_owner(), Owner::AgentAttention);
    assert_ne!(definition.native_notice_owner(), Owner::SoftwareUpdate);
    for role in Role::ALL {
        let included = expected.contains(&role);
        assert_eq!(definition.includes(role), included);
        assert_eq!(definition.role_name(role).is_some(), included);
        assert_eq!(!definition.permissions(role).is_empty(), included);
    }
    assert_eq!(
        definition.permissions(Role::NativePosting),
        &[
            Permission::AgentPost,
            Permission::AgentWithdrawal,
            Permission::AgentActivation
        ]
    );
    for excluded in [
        Role::Operations,
        Role::Drafts,
        Role::Providers,
        Role::Downloads,
        Role::Cleanup,
        Role::AutomaticApprovals,
    ] {
        assert!(!definition.includes(excluded));
        assert!(definition.permissions(excluded).is_empty());
    }
}

#[test]
fn role_declaration_requires_the_whole_exact_set_without_duplicates_or_extras() {
    let definition = BACKGROUND_V1;
    let original = definition.roles().to_vec();
    assert!(definition.matches_roles(&original));
    let mut reversed = original.clone();
    reversed.reverse();
    assert!(definition.matches_roles(&reversed));
    assert!(!definition.matches_roles(&[]));
    for i in 0..original.len() {
        let mut missing = original.clone();
        missing.remove(i);
        assert!(!definition.matches_roles(&missing));
        let mut duplicate = original.clone();
        duplicate[i] = original[(i + 1) % original.len()];
        assert!(!definition.matches_roles(&duplicate));
    }
    for role in Role::ALL {
        let mut extra = original.clone();
        extra.push(role);
        assert!(!definition.matches_roles(&extra));
    }
}

#[test]
fn profile_matching_has_no_alias_version_or_hash_spelling_fallback() {
    let definition = BACKGROUND_V1;
    assert!(definition.matches(1, "g1-background-v1", HASH));
    for version in [0, 2, u32::MAX] {
        assert!(!definition.matches(version, definition.id(), HASH));
    }
    for id in [
        "",
        "g1-background",
        "G1-background-v1",
        "g1-background-v1 ",
        "complete",
    ] {
        assert!(!definition.matches(1, id, HASH));
    }
    for digest in [
        "".into(),
        HASH.to_uppercase(),
        format!("{HASH}\n"),
        "0".repeat(64),
        "g".repeat(64),
    ] {
        assert!(!definition.matches(1, definition.id(), &digest));
    }
}
