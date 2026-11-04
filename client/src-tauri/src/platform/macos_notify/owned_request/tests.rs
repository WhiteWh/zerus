use super::*;
use objc2::rc::autoreleasepool;
use objc2_foundation::NSCopying;

const CORRELATION: &str = "11111111-1111-4111-8111-111111111111";
fn agent() -> String {
    attention::encode_token(
        serde_json::json!({
            "host":"", "name":"codex/example/one", "notice":"example"
        })
        .as_object()
        .unwrap(),
    )
}
fn notice(owner: Owner) -> Notice {
    Notice {
        token: match owner {
            Owner::AgentAttention => agent() + "=", // Preserve Qt padding verbatim.
            Owner::SoftwareUpdate => "zerus-update:v2:dev:123".into(),
        },
        title: "Synthetic — title".into(),
        body: "Process-local data\nТолько данные".into(),
    }
}
fn string(info: &NSDictionary, key: &NSString) -> String {
    info.objectForKey(key)
        .unwrap()
        .downcast::<NSString>()
        .unwrap()
        .to_string()
}
fn metadata(request: &UNNotificationRequest, owner: Owner, original: &Notice) {
    let content = request.content();
    assert_eq!(
        request.identifier().to_string(),
        attention::request_id(&original.token)
    );
    assert_eq!(content.title().to_string(), original.title);
    assert_eq!(content.body().to_string(), original.body);
    assert_eq!(content.categoryIdentifier().to_string(), category(owner));
    assert_eq!(
        content.sound().unwrap(),
        UNNotificationSound::defaultSound()
    );
    assert!(request.trigger().is_none());
    let info = content.userInfo();
    assert_eq!(info.count(), 2);
    assert_eq!(
        string(&info, ns_string!("hgsAttentionToken")),
        original.token
    );
    assert_eq!(
        string(&info, ns_string!("hgsAttentionCorrelationV1")),
        CORRELATION
    );
}

#[test]
fn actual_owner_metadata_preserves_raw_token_text_sound_and_unscheduled_request() {
    autoreleasepool(|_| {
        assert_ne!(
            category(Owner::AgentAttention),
            category(Owner::SoftwareUpdate)
        );
        for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
            let original = notice(owner);
            let draft = Draft::new(owner, &original, CORRELATION).unwrap();
            assert_eq!(draft.owner, owner);
            metadata(&draft.request, owner, &original);
            assert_eq!(format!("{draft:?}"), "Draft { .. }");
        }
    });
}

#[test]
fn actual_immutable_request_copy_outlives_draft_and_original_pool() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        let original = notice(owner);
        let copy = autoreleasepool(|_| {
            let draft = Draft::new(owner, &original, CORRELATION).unwrap();
            draft.request.copy() // Actual NSCopying, not only a Rust retain.
        });
        autoreleasepool(|_| metadata(&copy, owner, &original));
    }
}

#[test]
fn wrong_owner_and_unbound_legacy_text_refuse_without_classification_or_fallback() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        let other = match owner {
            Owner::AgentAttention => Owner::SoftwareUpdate,
            Owner::SoftwareUpdate => Owner::AgentAttention,
        };
        assert!(matches!(
            Draft::new(owner, &notice(other), CORRELATION),
            Err(Refused::TokenOwner)
        ));
        for token in [
            "",
            "legacy-unbound",
            "zerus.attention.other",
            "e30",
            "zerus-update:v2:dev:0123",
        ] {
            let mut original = notice(owner);
            original.token = token.into();
            assert!(matches!(
                Draft::new(owner, &original, CORRELATION),
                Err(Refused::TokenOwner)
            ));
        }
    }
}

#[test]
fn correlation_must_be_original_canonical_nonnil_bytes_and_is_never_generated() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        for correlation in [
            "",
            "00000000-0000-0000-0000-000000000000",
            "11111111-1111-4111-8111-11111111111A",
            "{11111111-1111-4111-8111-111111111111}",
            "not-a-uuid",
        ] {
            assert!(matches!(
                Draft::new(owner, &notice(owner), correlation),
                Err(Refused::Correlation)
            ));
        }
    }
}

#[test]
fn utf8_byte_limits_refuse_the_entire_input_without_clipping() {
    for owner in [Owner::AgentAttention, Owner::SoftwareUpdate] {
        let mut original = notice(owner);
        original.title = "é".repeat(TITLE_BYTES / 2) + "x";
        assert!(matches!(
            Draft::new(owner, &original, CORRELATION),
            Err(Refused::TooLarge)
        ));
        original = notice(owner);
        original.body = "é".repeat(BODY_BYTES / 2) + "x";
        assert!(matches!(
            Draft::new(owner, &original, CORRELATION),
            Err(Refused::TooLarge)
        ));
        original = notice(owner);
        original.token = "a".repeat(TOKEN_BYTES + 1);
        assert!(matches!(
            Draft::new(owner, &original, CORRELATION),
            Err(Refused::TooLarge)
        ));
    }
}

#[test]
fn actual_boundary_objects_keep_full_utf8_text_and_the_maximum_raw_agent_token() {
    autoreleasepool(|_| {
        let mut original = notice(Owner::AgentAttention);
        let mut fields = serde_json::json!({"host":"", "name":"codex/example/one", "notice":"example", "padding":""});
        let raw = fields.to_string().len();
        fields["padding"] = "a".repeat(TOKEN_BYTES / 4 * 3 - raw).into();
        original.token = attention::encode_token(fields.as_object().unwrap());
        assert_eq!(original.token.len(), TOKEN_BYTES);
        original.title = "é".repeat(TITLE_BYTES / 2);
        original.body = "界".repeat(BODY_BYTES / 3) + &"x".repeat(BODY_BYTES % 3);
        assert_eq!(original.title.len(), TITLE_BYTES);
        assert_eq!(original.body.len(), BODY_BYTES);
        let draft = Draft::new(Owner::AgentAttention, &original, CORRELATION).unwrap();
        metadata(&draft.request, Owner::AgentAttention, &original);
    });
}
