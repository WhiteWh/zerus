use super::*;
use serde_json::{json, Value};

const CORRELATION: &str = "08a744d0-e564-4fa7-9973-59b69bcac621";

fn token() -> String {
    attention::encode_token(
        json!({
            "host":"fixture-host", "computer":"fixture-computer", "name":"claude/project/проверка",
            "run":"original-run", "conversation":"original-conversation", "notice":"reply",
            "reply":"original-reply", "opaque":{"note":"не менять"}
        })
        .as_object()
        .unwrap(),
    )
}

fn envelope() -> Envelope {
    Envelope::new(&token(), CORRELATION).unwrap()
}

#[test]
fn native_strings_round_trip_without_reencoding_the_original_unicode_target() {
    let envelope = envelope();
    let bytes = envelope.user_info();
    let info: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info[TOKEN_KEY], token());
    assert_eq!(info[CORRELATION_KEY], CORRELATION);
    assert_eq!(info.as_object().unwrap().len(), 2);
    assert_eq!(
        envelope.request_id(),
        format!("zerus.attention.{}", token())
    );
    assert_eq!(envelope.token(), token());
    assert_eq!(envelope.correlation(), CORRELATION);
    assert_eq!(
        response(true, &envelope.request_id(), &bytes),
        Ok(Response::Candidate(envelope))
    );
}

#[test]
fn a_default_action_with_only_qts_token_is_suppression_only() {
    let bytes = serde_json::to_vec(&json!({TOKEN_KEY: token()})).unwrap();
    assert_eq!(
        response(true, &envelope().request_id(), &bytes),
        Ok(Response::Ignored(Ignore::LegacyUnbound))
    );
}

#[test]
fn dismiss_and_custom_actions_produce_no_candidate() {
    assert_eq!(
        response(false, &envelope().request_id(), &envelope().user_info()),
        Ok(Response::Ignored(Ignore::NotDefaultAction))
    );
}

#[test]
fn default_click_requires_the_same_full_native_request_id() {
    for request in [
        "zerus.attention.",
        "other-prefix",
        "",
        "zerus.attention.latest",
    ] {
        assert_eq!(
            response(true, request, &envelope().user_info()),
            Err(Invalid::RequestId)
        );
    }
    let mut other = attention::target(&token()).unwrap();
    other.insert("run".into(), json!("replaced-run"));
    let other = Envelope::new(&attention::encode_token(&other), CORRELATION).unwrap();
    assert_eq!(
        response(true, &other.request_id(), &envelope().user_info()),
        Err(Invalid::RequestId)
    );
}

#[test]
fn a_correlation_is_canonical_non_nil_uuid_data() {
    for correlation in [
        "",
        "newest",
        "00000000-0000-0000-0000-000000000000",
        "08A744D0-E564-4FA7-9973-59B69BCAC621",
        "{08a744d0-e564-4fa7-9973-59b69bcac621}",
        "08a744d0-e564-4fa7-9973-59b69bcac621\n",
    ] {
        assert_eq!(
            Envelope::new(&token(), correlation),
            Err(Invalid::Correlation)
        );
        let bytes =
            serde_json::to_vec(&json!({TOKEN_KEY:token(),CORRELATION_KEY:correlation})).unwrap();
        assert_eq!(
            response(true, &envelope().request_id(), &bytes),
            Err(Invalid::Correlation)
        );
    }
}

#[test]
fn absent_is_legacy_but_null_empty_or_wrong_typed_correlation_never_is() {
    for value in [Value::Null, json!(false), json!(1), json!([]), json!({})] {
        let bytes = serde_json::to_vec(&json!({TOKEN_KEY:token(), CORRELATION_KEY:value})).unwrap();
        assert_eq!(
            response(true, &envelope().request_id(), &bytes),
            Err(Invalid::Malformed)
        );
    }
}

#[test]
fn duplicate_keys_and_unknown_envelope_versions_refuse() {
    let encoded_token = serde_json::to_string(&token()).unwrap();
    let token_duplicate =
        format!("{{\"hgsAttentionToken\":{encoded_token},\"hgsAttentionToken\":{encoded_token}}}");
    let correlation_duplicate = format!("{{\"hgsAttentionToken\":{encoded_token},\"hgsAttentionCorrelationV1\":\"{CORRELATION}\",\"hgsAttentionCorrelationV1\":\"{CORRELATION}\"}}");
    let future =
        serde_json::to_string(&json!({TOKEN_KEY:token(),"hgsAttentionCorrelationV2":CORRELATION}))
            .unwrap();
    for text in [token_duplicate, correlation_duplicate, future] {
        assert_eq!(
            response(true, &envelope().request_id(), text.as_bytes()),
            Err(Invalid::Malformed)
        );
    }
}

#[test]
fn malformed_utf8_shape_and_trailing_documents_refuse() {
    for bytes in [
        b"\xff".as_slice(),
        b"[]",
        b"null",
        b"{}",
        b"true",
        b"{",
        b"{}{}",
    ] {
        assert_eq!(
            response(true, &envelope().request_id(), bytes),
            Err(Invalid::Malformed)
        );
    }
}

#[test]
fn tokens_use_the_existing_attention_decoder_and_do_not_choose_an_alias() {
    for token in ["", "not-a-token", "e30", "bnVsbA", "../session"] {
        assert_eq!(Envelope::new(token, CORRELATION), Err(Invalid::Token));
    }
    let bare = attention::encode_token(
        json!({"host":"fixture", "name":"session", "notice":"reply"})
            .as_object()
            .unwrap(),
    );
    // Syntax alone is deliberately not a stable-computer/run/identity proof.
    assert!(Envelope::new(&bare, CORRELATION).is_ok());
}

#[test]
fn callback_bytes_are_limited_before_parsing_including_whitespace() {
    let mut bytes = envelope().user_info();
    bytes.resize(USER_INFO_BYTES, b' ');
    assert_eq!(
        response(true, &envelope().request_id(), &bytes),
        Ok(Response::Candidate(envelope()))
    );
    bytes.push(b' ');
    assert_eq!(
        response(true, &envelope().request_id(), &bytes),
        Err(Invalid::TooLarge)
    );
}

#[test]
fn token_limit_preserves_qt_accepted_padding_and_the_full_original_identifier() {
    let mut token = token();
    token.extend(std::iter::repeat_n('=', TOKEN_BYTES - token.len()));
    let full = Envelope::new(&token, CORRELATION).unwrap();
    assert!(full.user_info().len() <= USER_INFO_BYTES);
    assert_eq!(full.token(), token);
    assert_eq!(
        response(true, &full.request_id(), &full.user_info()),
        Ok(Response::Candidate(full))
    );
    token.push('=');
    assert_eq!(Envelope::new(&token, CORRELATION), Err(Invalid::Token));
    assert_eq!(
        response(
            true,
            &attention::request_id(&token),
            &envelope().user_info()
        ),
        Err(Invalid::TooLarge)
    );
}

#[test]
fn private_token_and_correlation_do_not_leak_through_debug_or_diagnostics() {
    let value = format!("{:?}", Response::Candidate(envelope()));
    assert!(!value.contains(&token()));
    assert!(!value.contains(CORRELATION));
    assert_eq!(value, "Candidate(Envelope { .. })");
}

#[test]
fn native_completion_error_timeout_and_no_callback_cannot_authorize_a_retry() {
    // A fake center's error after add, missing callback or cancellation after
    // possible submission all retain Unknown; none is a pre-call refusal.
    for observation in [
        PostObservation::MayHaveIssued,
        PostObservation::Completion { accepted: false },
    ] {
        assert_eq!(post_outcome(observation), Outcome::Unknown);
    }
    assert_eq!(
        post_outcome(PostObservation::VerifiedNotIssued),
        Outcome::RefusedBeforeCall
    );
    assert_eq!(
        post_outcome(PostObservation::Completion { accepted: true }),
        Outcome::AcceptedByFramework
    );
}

#[test]
fn two_void_remove_calls_are_issued_metadata_and_never_confirm_visual_removal() {
    for pending_issued in [false, true] {
        for delivered_issued in [false, true] {
            let observation = WithdrawalObservation {
                pending_issued,
                delivered_issued,
            };
            assert_eq!(observation.outcome(), Outcome::Unknown);
        }
    }
}

#[test]
fn a_different_valid_correlation_is_only_data_not_an_original_mapping() {
    let other = Envelope::new(&token(), "9498ca68-7950-4bda-af83-d1c1e8fd085e").unwrap();
    assert_ne!(other, envelope());
    assert_eq!(other.request_id(), envelope().request_id());
    assert_eq!(
        response(true, &other.request_id(), &other.user_info()),
        Ok(Response::Candidate(other))
    );
    // The actor, absent here, must reject any Candidate not bound to its exact
    // Delivered original post. Parsing has no acceptance or activation path.
}
