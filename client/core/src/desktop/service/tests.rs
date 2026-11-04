use super::*;
use serde_json::json;

const EPOCH: &str = "00000000-0000-4000-8000-000000000001";
const REQUEST: &str = "00000000-0000-4000-8000-000000000002";

fn counts() -> Value {
    json!({"epoch":EPOCH,"scope":"shared_host_state","display_only":true,"authority":false,
        "local_marks_included":false,"computers":[{"computer":"peer","online":false,"complete":false,"total":4,"attention":null}],
        "aggregate":{"total":4,"known_total":4,"known_attention":0,"attention":null,"complete":false}})
}
fn status() -> Value {
    let configuration = "a".repeat(64);
    json!({"abi":ABI,"configuration":configuration,"epoch":EPOCH,"generation":3,
        "lifecycle":{"abi":"desktop-lifecycle-v1","phase":"serving","configuration":configuration,
            "epoch":EPOCH,"generation":3,"ipc_ready":true,"background":{}},"attention_counts":counts()})
}
fn frame(seq: u64, kind: &str, value: Value) -> Value {
    json!({"abi":ABI,"epoch":EPOCH,"seq":seq,"t":kind,"value":value})
}
fn state() -> State {
    State::new("a".repeat(64)).unwrap()
}

#[test]
fn one_explicit_startup_switch_is_off_by_default() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("yes"),
        Some("1 "),
    ] {
        assert!(!enabled(value));
    }
    assert!(enabled(Some("1")));
}

#[test]
fn notification_preferences_can_only_be_attached_by_the_native_client() {
    let forged = json!({"available":true,"pending":false,"status":{"configured":true,"preferences":{"optional_questions":true}}});
    let mut input = serde_json::to_value(Snapshot::default()).unwrap();
    input["notification_preferences"] = forged.clone();
    let snapshot: Snapshot = serde_json::from_value(input).unwrap();
    assert!(snapshot.notification_preferences.is_none());
    let mut value = status();
    value["notification_preferences"] = forged;
    let mut state = state();
    state.status(&value).unwrap();
    assert!(state.snapshot().notification_preferences.is_none());
}

#[test]
fn status_and_heartbeat_preserve_the_original_lease_and_refresh_component_readiness() {
    let mut initial = status();
    initial["details"] = json!({"abi":details::ABI,"epoch":EPOCH,"lease":REQUEST,"limits":{"demands_per_subscriber":64}});
    initial["detail_subscriptions"] = json!({"abi":details::ABI,"stopping":false});
    initial["lifecycle"]["background"]["fleet"] = json!({"ready":true,"code":"running"});
    let mut state = state();
    state.frame(&frame(0, "hello", initial.clone())).unwrap();
    assert!(state.snapshot().details_available);
    initial.as_object_mut().unwrap().remove("details");
    state.frame(&frame(1, "status", initial.clone())).unwrap();
    assert!(
        state.snapshot().details_available,
        "status has a summary, not a new lease"
    );
    initial["lifecycle"]["background"]["fleet"] = json!({"ready":false,"code":"source_gap"});
    assert_eq!(
        state.frame(&frame(2, "heartbeat", initial)),
        Ok(Update::Status)
    );
    assert!(!state.snapshot().component_ready("fleet"));
    assert!(state.snapshot().details_available);
    state.disconnect("offline");
    assert!(!state.snapshot().details_available);
}
#[test]
fn ipc_readiness_does_not_advertise_missing_background_components() {
    let mut state = state();
    state.frame(&frame(0, "hello", status())).unwrap();
    assert!(state.snapshot().connected);
    assert_eq!(state.snapshot().code, "background_unready");
    for name in COMPONENTS {
        assert!(!state.snapshot().component_ready(name));
    }
    let mut ready = status();
    ready["lifecycle"]["background"] = json!({"fleet":{"ready":true,"code":"running"}});
    state.frame(&frame(0, "status", ready)).unwrap();
    assert!(state.snapshot().component_ready("fleet"));
    assert!(!state.snapshot().background_ready());
}
#[test]
fn null_and_incomplete_shared_counts_never_become_local_zero_counts() {
    let mut state = state();
    state.status(&status()).unwrap();
    let projection = state.snapshot().attention_counts.as_ref().unwrap();
    assert_eq!(projection, &counts());
    assert!(projection["aggregate"]["attention"].is_null());
    assert_eq!(projection["aggregate"]["complete"], false);
    state.disconnect("service_unavailable");
    assert_eq!(state.snapshot().attention_counts, None);
    assert!(!state.snapshot().connected);
}
#[test]
fn unavailable_shared_counts_do_not_disconnect_a_valid_lifecycle() {
    let mut value = status();
    value["attention_counts"] = json!({"epoch":EPOCH,"scope":"shared_host_state","display_only":true,
        "authority":false,"local_marks_included":false,"aggregate":{"complete":false,"attention":null},"error":"counts_unavailable"});
    let mut current = state();
    assert_eq!(
        current.frame(&frame(0, "hello", value.clone())),
        Ok(Update::Status)
    );
    assert!(current.snapshot().connected);
    assert!(current.snapshot().attention_counts.is_none());
    value["attention_counts"] = counts();
    current.frame(&frame(1, "heartbeat", value)).unwrap();
    assert_eq!(current.snapshot().attention_counts, Some(counts()));
}
#[test]
fn source_roster_is_independent_bounded_and_fenced_to_its_epoch() {
    let mut value = status();
    value["attention_counts"] = Value::Null;
    let roster = json!({"epoch":EPOCH,"display_only":true,"authority":false,"computers":[
        {"computer":"","incarnation":1,"generation":2,"status":"live","accepting":true},
        {"computer":"peer","incarnation":2,"generation":3,"status":"reconnecting","accepting":false}]});
    value["background_runtime"] = roster.clone();
    let mut current = state();
    current.status(&value).unwrap();
    let sources = current.sources().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].computer, "");
    assert!(sources[0].online);
    assert!(!sources[1].online);
    for key in ["epoch", "authority", "computers"] {
        let mut invalid = roster.clone();
        invalid[key] = match key {
            "epoch" => json!(REQUEST),
            "authority" => json!(true),
            _ => json!([
                roster["computers"][0].clone(),
                roster["computers"][0].clone()
            ]),
        };
        value["background_runtime"] = invalid;
        current.status(&value).unwrap();
        assert!(current.snapshot().connected);
        assert!(current.sources().is_none());
    }
    let mut oversized = roster.clone();
    oversized["computers"] = json!((0..65).map(|i| json!({"computer":format!("peer-{i}"),"incarnation":1,"generation":1,"status":"live","accepting":true})).collect::<Vec<_>>());
    value["background_runtime"] = oversized;
    current.status(&value).unwrap();
    assert!(current.sources().is_none());
    current.disconnect("offline");
    assert!(current.sources().is_none());
}
#[test]
fn shared_projection_cannot_claim_local_marks_or_authority() {
    for (key, value) in [
        ("local_marks_included", json!(true)),
        ("authority", json!(true)),
        ("display_only", json!(false)),
    ] {
        let mut input = status();
        input["attention_counts"][key] = value;
        let mut state = state();
        assert_eq!(state.status(&input), Ok(()));
        assert!(state.snapshot().connected);
        assert!(
            state.snapshot().attention_counts.is_none(),
            "invalid display data is quarantined"
        );
    }
}
#[test]
fn wrong_configuration_epoch_and_generation_clear_current_state() {
    for field in ["configuration", "epoch", "generation"] {
        let mut state = state();
        state.status(&status()).unwrap();
        let mut next = status();
        next["lifecycle"][field] = json!("wrong");
        assert!(state.status(&next).is_err());
        assert!(!state.snapshot().connected);
        assert!(state.snapshot().attention_counts.is_none());
    }
    let mut state = state();
    state.status(&status()).unwrap();
    let mut next = status();
    next["generation"] = json!(2);
    next["lifecycle"]["generation"] = json!(2);
    assert_eq!(state.status(&next), Err("stale_service_generation"));
}
#[test]
fn targeted_sequences_may_skip_but_stale_epochs_and_regressions_are_refused() {
    let mut state = state();
    state.frame(&frame(2, "hello", status())).unwrap();
    state.frame(&frame(9, "status", status())).unwrap();
    state.frame(&frame(9, "status", status())).unwrap();
    assert_eq!(
        state.frame(&frame(8, "status", status())),
        Err("stale_service_frame")
    );
    assert!(!state.snapshot().connected);
    state.frame(&frame(0, "hello", status())).unwrap();
    let mut next = frame(1, "status", status());
    next["epoch"] = json!(REQUEST);
    assert_eq!(state.frame(&next), Err("service_epoch_changed"));
}
#[test]
fn explicit_handoff_refusal_is_not_repaired_by_an_ipc_ready_status() {
    let mut value = status();
    value["background_start"] =
        json!({"admitted":false,"code":"explicit_handoff_required","durably_blocked":true});
    for name in COMPONENTS {
        value["lifecycle"]["background"][name] = json!({"ready":false,"code":"not_started"});
    }
    let mut state = state();
    state.status(&value).unwrap();
    assert!(state.snapshot().connected);
    assert_eq!(state.snapshot().code, "explicit_handoff_required");
    assert!(!state.snapshot().background_ready());
}
#[test]
fn catalog_invalidation_and_disconnect_do_not_admit_effects() {
    let mut state = state();
    state.frame(&frame(0, "hello", status())).unwrap();
    let changed = frame(
        1,
        "catalog_changed",
        json!({"computer":"peer","removed":true,"attention_counts":counts()}),
    );
    assert_eq!(
        state.frame(&changed),
        Ok(Update::Catalog {
            computer: "peer".into(),
            removed: true
        })
    );
    assert_eq!(
        state.frame(&frame(2, "gap", Value::Null)),
        Err("service_resnapshot_required")
    );
    assert!(!state.snapshot().connected);
}
fn page() -> Value {
    json!({"request_id":REQUEST,"abi":"desktop-catalog-v1","accepted":true,"durable":false,
        "epoch":EPOCH,"computer":"peer","display_only":true,"authority":false,"complete":false,
        "changes_abi":CHANGES_ABI,"incarnation":1,"generation":1,"projection":"0123456789abcdef0123456789abcdef",
        "start_revision":null,"revision":"0","online":false,"last_known":true,"stale":false,
        "items":[{"key":"run:example","row":{"name":"claude/example/session"}}],"next_cursor":null})
}
#[test]
fn catalog_pages_bind_the_request_and_epoch_without_inventing_completeness() {
    let page = CatalogPage::checked(page(), REQUEST, EPOCH, "peer").unwrap();
    assert!(!page.complete);
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_none());
    for field in ["request_id", "epoch", "computer"] {
        let mut value = self::page();
        value[field] = json!("wrong");
        assert!(CatalogPage::checked(value, REQUEST, EPOCH, "peer").is_err());
    }
}
#[test]
fn oversized_or_authoritative_catalog_pages_are_refused() {
    let mut value = page();
    value["items"] = json!(vec![json!({"key":"example","row":{}}); CATALOG_ROWS + 1]);
    assert!(CatalogPage::checked(value, REQUEST, EPOCH, "peer").is_err());
    for field in ["durable", "authority"] {
        let mut value = page();
        value[field] = json!(true);
        assert!(CatalogPage::checked(value, REQUEST, EPOCH, "peer").is_err());
    }
}

#[test]
fn read_commit_requires_the_original_identity_operation_and_durable_actor_receipt() {
    let receipt = json!({"request_id":REQUEST,"op":"read_intent","accepted":true,"durable":true});
    assert!(read_committed(&receipt, REQUEST));
    for (key, value) in [
        ("request_id", json!(EPOCH)),
        ("op", json!("delivery_result")),
        ("accepted", json!(false)),
        ("durable", json!(false)),
    ] {
        let mut bad = receipt.clone();
        bad[key] = value;
        assert!(!read_committed(&bad, REQUEST));
    }
}

#[test]
fn catalog_revisions_are_canonical_exact_strings_beyond_javascript_precision() {
    let parse = |s: &str| serde_json::from_value::<Revision>(json!(s));
    assert!(parse("9").unwrap() < parse("10").unwrap());
    assert!(parse("9007199254740992").unwrap() < parse("9007199254740993").unwrap());
    assert_eq!(
        serde_json::to_value(parse("9007199254740993").unwrap()).unwrap(),
        json!("9007199254740993")
    );
    for value in [
        json!(0),
        json!("01"),
        json!("-1"),
        json!("+1"),
        json!(" 1"),
        json!("9223372036854775808"),
    ] {
        assert!(serde_json::from_value::<Revision>(value).is_err());
    }
}
fn changes_page() -> Value {
    let mut p = page();
    p["online"] = json!(true);
    p["last_known"] = json!(false);
    p["complete"] = json!(true);
    p["since_revision"] = json!("9007199254740992");
    p["through_revision"] = json!("9007199254740994");
    p["revision"] = json!("9007199254740994");
    p["items"] = json!([{"op":"upsert","key":"run:example","revision":"9007199254740993","row":{}},
        {"op":"remove","key":"run:other","revision":"9007199254740994","row":null}]);
    p
}
#[test]
fn changes_pages_preserve_partial_cursors_without_claiming_walk_completion() {
    let mut p = changes_page();
    p["next_cursor"] = json!(REQUEST);
    let checked = ChangePage::checked(p, REQUEST, EPOCH, "peer").unwrap();
    assert!(checked.complete);
    assert!(checked.next_cursor.is_some());
    assert_eq!(checked.items.len(), 2);
    for (key, value) in [
        ("changes_abi", json!("wrong")),
        ("durable", json!(true)),
        ("authority", json!(true)),
        ("online", json!(false)),
        ("last_known", json!(true)),
        ("projection", json!("A".repeat(32))),
        ("through_revision", json!("9007199254740991")),
        ("revision", json!("9007199254740993")),
        ("stale", json!(true)),
    ] {
        let mut p = changes_page();
        p[key] = value;
        assert!(
            ChangePage::checked(p, REQUEST, EPOCH, "peer").is_err(),
            "{key}"
        );
    }
    for (key, value) in [
        ("revision", json!("9007199254740992")),
        ("op", json!("replace")),
        ("row", Value::Null),
    ] {
        let mut p = changes_page();
        p["items"][0][key] = value;
        assert!(ChangePage::checked(p, REQUEST, EPOCH, "peer").is_err());
    }
    let mut p = changes_page();
    p["items"] = json!(vec![p["items"][0].clone(); CATALOG_ROWS + 1]);
    assert!(ChangePage::checked(p, REQUEST, EPOCH, "peer").is_err());
}
