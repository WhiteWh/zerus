use super::*;
use crate::organization::swarm_dialog::{conflicts::Query, Action};
use serde::Serialize;

fn conflicting(values: usize) -> Value {
    let mut v = full("d1");
    v["swarm_id"] = json!("local-swarm");
    let field = json!({"kind":"project","id":"alpha","field":"name"});
    v["conflicts"] = json!([{"key":field.to_string(),"field":field,"selected":"v0","variants":(0..values).map(|i|json!({"id":format!("v{i}"),"actor":"node-1","value":format!("Value {i}")})).collect::<Vec<_>>() }]);
    v
}
fn resolved() -> Value {
    let mut v = conflicting(2);
    v["digest"] = json!("d2");
    v["conflicts"] = json!([]);
    v["organization"]["projects"][0]["name"] = json!("Value 1");
    v["versions"] = json!({"resolved-key":["resolved-op"]});
    v
}
fn fixture(get: Value, resolve: Value) -> (Fake, Organization, Arc<Mutex<Value>>) {
    let fake = Fake::new(json!([]));
    std::fs::write(fake.dir.join("config.json"),serde_json::to_vec(&json!({"machines":[{"stdout":{"local":"workstation","revision":"a".repeat(64),"machines":[]}}],"get":get,"resolve":resolve})).unwrap()).unwrap();
    let org = fake.organization(&Pump::default(), true);
    let cache = Arc::new(Mutex::new(conflicting(2)));
    let source = cache.clone();
    org.set_source(Arc::new(move || {
        let v = source.lock().unwrap();
        Some(OrganizationSnapshot {
            node_id: "node-1".into(),
            swarm_id: "local-swarm".into(),
            digest: v["digest"].as_str().unwrap().into(),
            base: v["organization"].clone(),
            versions: v["versions"].clone(),
        })
    }));
    org.observe(&state_of(&conflicting(2)));
    (fake, org, cache)
}
fn value(v: impl Serialize) -> Value {
    serde_json::to_value(v).unwrap()
}
fn open(org: &Organization) -> String {
    let id = value(org.catalog_request(None, None, false).unwrap())["id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until("config", || {
        value(org.catalog_request(Some(&id), None, false).unwrap())["busy"] == false
    });
    id
}
fn review(org: &Organization, id: &str) -> String {
    let conflict = org.conflict_page(id, 0).unwrap().rows[0].id.clone();
    value(org.conflict_review(id, Query::Open { conflict }).unwrap())["id"]
        .as_str()
        .unwrap()
        .into()
}
fn choose(org: &Organization, id: &str, review: &str, choice: usize) -> Value {
    value(
        org.conflict_review(
            id,
            Query::Choose {
                review: review.into(),
                choice,
            },
        )
        .unwrap(),
    )
}
fn issue(
    org: &Organization,
    id: &str,
    review: &str,
    confirmed: bool,
) -> Result<super::super::swarm_dialog::View, String> {
    org.catalog_request(
        Some(id),
        Some(Action::Resolve {
            review: review.into(),
            confirmed,
        }),
        false,
    )
}
fn calls(fake: &Fake, verb: &str) -> Vec<Value> {
    std::fs::read_to_string(fake.dir.join("log.ndjson"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v["argv"][1] == verb)
        .collect()
}
fn advance(org: &Organization, cache: &Mutex<Value>, v: Value) {
    *cache.lock().unwrap() = v.clone();
    org.observe(&state_of(&v));
}

#[test]
fn pages_and_choices_are_read_only_and_only_visible_values_can_be_chosen() {
    let (fake, org, cache) = fixture(json!([{"stdout":conflicting(2)}]), json!([]));
    advance(&org, &cache, conflicting(75));
    let id = open(&org);
    let r = review(&org, &id);
    assert_eq!(choose(&org, &id, &r, 1)["chosen"], 1);
    assert!(org
        .conflict_review(
            &id,
            Query::Choose {
                review: r.clone(),
                choice: 33
            }
        )
        .is_err());
    let next = value(
        org.conflict_review(
            &id,
            Query::Page {
                review: r.clone(),
                page: 1,
            },
        )
        .unwrap(),
    );
    assert_eq!(next["choices"]["offset"], 32);
    assert!(next["chosen"].is_null() && next["selection"].is_null());
    assert_eq!(choose(&org, &id, &r, 33)["chosen"], 33);
    org.conflict_page(&id, 1).unwrap();
    assert_eq!(
        value(org.conflict_review(&id, Query::View { review: r }).unwrap())["chosen"],
        33
    );
    assert!(calls(&fake, "get").is_empty() && calls(&fake, "resolve").is_empty());
    fake.finish(org);
}

#[test]
fn resolve_sends_exact_original_value_and_all_versions_once_then_waits_for_source() {
    let (fake, org, cache) = fixture(
        json!([{"stdout":conflicting(2)},{"stdout":resolved()}]),
        json!([{"stdout":resolved()}]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("Resolve and fresh read", || {
        calls(&fake, "get").len() == 2
            && !value(org.catalog_request(Some(&id), None, false).unwrap())["busy"]
                .as_bool()
                .unwrap()
    });
    assert!(org.lock().sync.busy());
    assert!(issue(&org, &id, &r, true).is_err());
    let sent: Value =
        serde_json::from_str(calls(&fake, "resolve")[0]["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(
        sent,
        json!({"key":conflicting(2)["conflicts"][0]["key"],"versions":["v0","v1"],"value":"Value 1"})
    );
    advance(&org, &cache, resolved());
    wait_until("source settlement", || org.lock().sync.ready());
    assert_eq!(
        value(org.catalog_request(Some(&id), None, false).unwrap())["status"],
        "Choice saved. No conflicts remain."
    );
    assert!(issue(&org, &id, &r, true).is_err());
    assert_eq!(calls(&fake, "resolve").len(), 1);
    fake.finish(org);
}

#[test]
fn fresh_variant_drift_refuses_before_resolve_and_consumes_the_review() {
    let changed = conflicting(3);
    let (fake, org, _) = fixture(
        json!([{"stdout":changed},{"stdout":conflicting(2)}]),
        json!([]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("fresh refusal settles", || {
        !org.lock().sync.busy()
            && value(org.catalog_request(Some(&id), None, false).unwrap())["error"] == true
    });
    assert!(calls(&fake, "resolve").is_empty());
    assert!(issue(&org, &id, &r, true).is_err());
    assert_eq!(
        value(org.catalog_request(Some(&id), None, false).unwrap())["status"],
        zerus_client_core::swarm::conflicts::CHANGED
    );
    fake.finish(org);
}

#[test]
fn unknown_resolve_never_reuses_the_review_even_if_the_current_source_changed() {
    let (fake, org, cache) = fixture(
        json!([{"stdout":conflicting(2)},{"stdout":resolved()}]),
        json!([{"exit":1,"stderr":"fixture result lost"}]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("unknown fresh read", || calls(&fake, "get").len() == 2);
    advance(&org, &cache, resolved());
    wait_until("unknown source settlement", || org.lock().sync.ready());
    assert!(issue(&org, &id, &r, true).is_err());
    assert!(
        value(org.catalog_request(Some(&id), None, false).unwrap())["status"]
            .as_str()
            .unwrap()
            .contains("fixture result lost")
    );
    org.catalog_request(Some(&id), None, true).unwrap();
    let _ = open(&org);
    assert_eq!(calls(&fake, "resolve").len(), 1);
    fake.finish(org);
}

#[test]
fn changed_variants_clear_native_choice_and_old_confirmation_identity() {
    let (fake, org, cache) = fixture(json!([]), json!([]));
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    advance(&org, &cache, conflicting(3));
    assert!(org
        .conflict_review(&id, Query::View { review: r.clone() })
        .is_err());
    let conflict = org.conflict_page(&id, 0).unwrap().rows[0].id.clone();
    let fresh = value(org.conflict_review(&id, Query::Open { conflict }).unwrap());
    assert!(fresh["chosen"].is_null());
    assert_ne!(fresh["id"], r);
    assert!(fresh["explanation"]
        .as_str()
        .unwrap()
        .starts_with(zerus_client_core::swarm::conflicts::CHANGED));
    assert!(issue(&org, &id, &r, true).is_err());
    assert!(calls(&fake, "resolve").is_empty());
    fake.finish(org);
}

#[test]
fn inherited_resolve_output_holds_writer_until_actual_return_and_close_cannot_retry() {
    let (fake, org, _) = fixture(
        json!([{"stdout":conflicting(2)}]),
        json!([{"stdout":resolved(),"hold_output":"release"}]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("Resolve issued", || calls(&fake, "resolve").len() == 1);
    std::thread::sleep(Duration::from_millis(2300));
    assert!(org.lock().sync.busy());
    assert_eq!(calls(&fake, "get").len(), 1);
    assert!(issue(&org, &id, &r, true).is_err());
    std::fs::write(fake.dir.join("release"), b"done").unwrap();
    wait_until("pipes completed", || org.lock().sync.ready());
    assert_eq!(calls(&fake, "get").len(), 2);
    assert_eq!(calls(&fake, "resolve").len(), 1);
    fake.finish(org);
}

#[test]
fn unchanged_success_reply_is_not_reported_as_a_confirmed_choice() {
    let (fake, org, _) = fixture(
        json!([{"stdout":conflicting(2)}]),
        json!([{"stdout":conflicting(2)}]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("unchanged reply settles", || {
        !org.lock().sync.busy()
            && value(org.catalog_request(Some(&id), None, false).unwrap())["error"] == true
    });
    assert!(
        value(org.catalog_request(Some(&id), None, false).unwrap())["status"]
            .as_str()
            .unwrap()
            .contains("Invalid project catalog response")
    );
    assert!(issue(&org, &id, &r, true).is_err());
    assert_eq!(calls(&fake, "resolve").len(), 1);
    fake.finish(org);
}

#[test]
fn native_removal_confirmation_is_required_before_any_read_or_writer_reservation() {
    let (fake, org, cache) = fixture(json!([]), json!([]));
    let mut v = conflicting(2);
    let field = json!({"kind":"project","id":"alpha","field":"alive"});
    v["conflicts"][0]["field"] = field.clone();
    v["conflicts"][0]["key"] = json!(field.to_string());
    v["conflicts"][0]["variants"][0]["value"] = json!(true);
    v["conflicts"][0]["variants"][1]["value"] = json!(false);
    advance(&org, &cache, v);
    let id = open(&org);
    let r = review(&org, &id);
    assert_eq!(choose(&org, &id, &r, 1)["selection"]["confirmation"], true);
    assert!(issue(&org, &id, &r, false).is_err());
    assert!(!org.lock().sync.busy());
    assert!(calls(&fake, "get").is_empty() && calls(&fake, "resolve").is_empty());
    fake.finish(org);
}

#[test]
fn concurrent_new_heads_remain_for_review_and_status_uses_the_fresh_catalog() {
    let mut concurrent = conflicting(2);
    concurrent["digest"] = json!("d3");
    concurrent["conflicts"][0]["variants"][0]["id"] = json!("new-choice-head");
    concurrent["conflicts"][0]["variants"][1]["id"] = json!("unseen-peer-head");
    concurrent["conflicts"][0]["selected"] = json!("new-choice-head");
    let (fake, org, cache) = fixture(
        json!([{"stdout":conflicting(2)},{"stdout":concurrent}]),
        json!([{"stdout":resolved()}]),
    );
    let id = open(&org);
    let r = review(&org, &id);
    choose(&org, &id, &r, 1);
    issue(&org, &id, &r, false).unwrap();
    wait_until("fresh concurrent heads", || calls(&fake, "get").len() == 2);
    advance(&org, &cache, concurrent);
    wait_until("concurrent source", || org.lock().sync.ready());
    assert_eq!(
        value(org.catalog_request(Some(&id), None, false).unwrap())["status"],
        "Choice saved. Review the remaining conflicts."
    );
    assert_eq!(org.conflict_page(&id, 0).unwrap().total, 1);
    assert_eq!(calls(&fake, "resolve").len(), 1);
    fake.finish(org);
}
