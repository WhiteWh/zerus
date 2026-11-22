use super::*;

#[test]
fn long_native_child_replies_are_canonical_only_without_changing_the_text() {
    for state in ["working", "finished", "ended", "unknown"] {
        for text in [
            "x".repeat(16_384),
            "x".repeat(16_385),
            "界".repeat(8_000),
            "🦀".repeat(journal::REPLY_LIMIT),
        ] {
            let mut value = record();
            value["subagents"] = json!({"child":{
                "state":state,"detail":text,"reply_id":"original-child-reply",
                "attention_id":"original-child-question","name":"Worker"}});
            let original = value.clone();
            let key = Key(format!(
                "{}.json",
                storage::label_key(string(&value, "name"))
            ));
            let encoded =
                Seed::encode(&key, &value).expect("a valid native reply must not poison the index");
            assert!(encoded.len() <= model::SEED_BYTES);
            let projected = Seed::decode(&key, &encoded).unwrap();
            let child = &projected["record"]["subagents"]["child"];
            assert_eq!(child["reply_id"], "original-child-reply");
            assert_eq!(child["attention_id"], "original-child-question");
            if text.len() > 16_384 {
                assert!(child.get("detail").is_none());
                assert_eq!(projected["record"]["_listing"]["requires_canonical"], true);
                assert_eq!(projected["record"]["_listing"]["live_public"], false);
            } else {
                assert_eq!(child["detail"], text);
            }
            assert_eq!(value, original, "canonical reply/attention data changed");
        }
    }
}

#[test]
fn canonical_child_detail_admission_does_not_widen_other_compact_fields() {
    let mut value = record();
    let key = Key(format!(
        "{}.json",
        storage::label_key(string(&value, "name"))
    ));
    for field in ["prompt", "description", "detail"] {
        value["subagents"] = json!({"child":{field:if field == "detail" {
            json!(["x".repeat(16_385)])
        } else { json!("x".repeat(16_385)) }}});
        assert!(
            Seed::encode(&key, &value).is_err(),
            "{field} lost its original bound"
        );
    }
    value["subagents"] = json!({});
    value["active_tools"] = json!({"tool":{"detail":"x".repeat(16_385)}});
    assert!(Seed::encode(&key, &value).is_err());
}
