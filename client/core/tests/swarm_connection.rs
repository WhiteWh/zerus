use serde_json::{json, Value};
use zerus_client_core::swarm::{
    connection::{self, Preview},
    sync::EXTERNAL_CHANGED,
    MAX_PROJECTS,
};
fn local() -> Value {
    json!({"schema":1,"node_id":"local-node","swarm_id":"local-swarm","digest":"d1","initialized":true,"organization":{"projects":[{"id":"local-project","name":"Local"}]}})
}
fn remote() -> Value {
    json!({"node_id":"remote-node","swarm_id":"remote-swarm","initialized":true,"same_swarm":false,"projects":[{"id":"remote-project","name":"Remote"}]})
}
#[test]
fn existing_cli_arguments_keep_alias_and_guards_literal_and_bounded() {
    let l = local();
    let r = remote();
    let preview = Preview::checked("fixture-peer", &l, &r).unwrap();
    let before = l.clone();
    let call = preview
        .join(
            &l,
            &[("local-project".into(), "remote-project".into())],
            true,
        )
        .unwrap();
    assert_eq!(call.args, ["swarm", "join", "fixture-peer"]);
    assert_eq!(call.timeout, connection::NETWORK_TIMEOUT);
    assert!(call.request_id.is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(call.stdin.as_ref().unwrap()).unwrap(),
        json!({"node_id":"remote-node","swarm_id":"remote-swarm","project_map":{"local-project":"remote-project"},"prefer_peer_memberships":true})
    );
    assert_eq!(
        connection::preview_invocation("fixture-peer").unwrap().args,
        ["swarm", "preview", "fixture-peer"]
    );
    assert_eq!(connection::sync_invocation().args, ["swarm", "sync"]);
    assert_eq!(l, before);
}
#[test]
fn local_drift_requires_new_preview_and_mapping_never_uses_a_prefix() {
    let l = local();
    let preview = Preview::checked("fixture-peer", &l, &remote()).unwrap();
    for key in ["node_id", "swarm_id", "digest"] {
        let mut changed = l.clone();
        changed[key] = json!("changed");
        assert_eq!(
            preview.join(&changed, &[], false).unwrap_err(),
            EXTERNAL_CHANGED
        );
    }
    for mappings in [
        vec![("missing".into(), "remote-project".into())],
        vec![("local-project".into(), "missing".into())],
        vec![("local-project".into(), "remote-project".into()); 2],
    ] {
        assert!(preview.join(&l, &mappings, false).is_err());
    }
    assert!(preview
        .join(
            &l,
            &vec![("local-project".into(), "remote-project".into()); MAX_PROJECTS + 1],
            false
        )
        .is_err());
}
#[test]
fn uninitialized_catalogs_can_be_previewed_but_never_joined() {
    let mut l = local();
    let mut r = remote();
    r["initialized"] = json!(false);
    let preview = Preview::checked("fixture-peer", &l, &r).unwrap();
    assert!(!preview.initialized());
    assert!(preview
        .join(&l, &[], false)
        .unwrap_err()
        .contains("Open Projects in Zerus on fixture-peer"));
    r["initialized"] = json!(true);
    l["initialized"] = json!(false);
    let preview = Preview::checked("fixture-peer", &l, &r).unwrap();
    assert!(preview.join(&l, &[], false).is_err());
}
#[test]
fn same_catalog_flag_self_join_and_invalid_alias_cannot_choose_another_target() {
    let l = local();
    let mut r = remote();
    r["same_swarm"] = json!(true);
    assert!(Preview::checked("fixture-peer", &l, &r).is_err());
    r["swarm_id"] = l["swarm_id"].clone();
    let p = Preview::checked("fixture-peer", &l, &r).unwrap();
    assert!(p.same_swarm());
    assert!(p
        .join(
            &l,
            &[("local-project".into(), "remote-project".into())],
            false
        )
        .is_err());
    r["node_id"] = l["node_id"].clone();
    let p = Preview::checked("fixture-peer", &l, &r).unwrap();
    assert!(p.join(&l, &[], false).is_err());
    for alias in ["", "-o", "fixture peer", "@fixture"] {
        assert!(Preview::checked(alias, &l, &r).is_err());
    }
    assert_eq!(
        connection::preview_invocation("fixture;other")
            .unwrap()
            .args[2],
        "fixture;other",
        "saved aliases stay one literal argument, never shell text"
    );
}
#[test]
fn project_limits_duplicate_identity_and_partial_names_preserve_whole_choices() {
    let l = local();
    let mut r = remote();
    r["projects"] = Value::Array(
        (0..MAX_PROJECTS)
            .map(|i| json!({"id":format!("project-{i}"),"name":format!("Project {i}")}))
            .collect(),
    );
    assert_eq!(
        Preview::checked("fixture-peer", &l, &r)
            .unwrap()
            .projects()
            .len(),
        MAX_PROJECTS
    );
    r["projects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"extra","name":"Extra"}));
    assert!(Preview::checked("fixture-peer", &l, &r).is_err());
    r["projects"] = json!([{"id":"original","name":null}]);
    let p = Preview::checked("fixture-peer", &l, &r).unwrap();
    assert_eq!(p.projects()[0].id, "original");
    assert_eq!(p.projects()[0].name, "");
    r["projects"] = json!([{"id":"same","name":"One"},{"id":"same","name":"Two"}]);
    assert!(Preview::checked("fixture-peer", &l, &r).is_err());
    r["projects"] = json!([{"id":"original","name":"é".repeat(161)}]);
    assert!(Preview::checked("fixture-peer", &l, &r).is_err());
}

#[test]
fn saved_alias_projection_keeps_disabled_choices_and_exact_configuration_revision() {
    let mut list = json!({"local":"this-computer","revision":"a".repeat(64),"machines":[{"alias":"this-computer"},{"alias":"fixture-peer","enabled":false,"connection":{"identity_file":"/synthetic/private-key"}}]});
    assert_eq!(
        connection::configured_aliases(&list).unwrap(),
        ["fixture-peer"]
    );
    assert_eq!(
        connection::configuration_revision(&list, "fixture-peer").unwrap(),
        "a".repeat(64)
    );
    assert!(connection::configuration_revision(&list, "missing").is_err());
    assert!(connection::configuration_revision(&list, "this-computer").is_err());
    list["revision"] = json!("A".repeat(64));
    assert!(connection::configuration_revision(&list, "fixture-peer").is_err());
}
#[test]
fn alias_overflow_duplicates_and_unsafe_targets_refuse_the_whole_saved_list() {
    let mut list = json!({"local":"this-computer","machines":(0..513).map(|i|json!({"alias":format!("peer-{i}")})).collect::<Vec<_>>()});
    assert!(connection::configured_aliases(&list).is_err());
    list["machines"] = json!([{"alias":"same"},{"alias":"same"}]);
    assert!(connection::configured_aliases(&list).is_err());
    list["machines"] = json!([{"alias":"-o"}]);
    assert!(connection::configured_aliases(&list).is_err());
}

#[test]
fn observed_enrollment_needs_exact_local_node_target_swarm_and_saved_peer_binding() {
    let l = local();
    let preview = Preview::checked("fixture-peer", &l, &remote()).unwrap();
    let mut state = l.clone();
    assert!(!preview.joined_in(&state));
    state["swarm_id"] = json!("remote-swarm");
    state["peers"] = json!({"fixture-peer":{"node":"remote-node"}});
    assert!(preview.joined_in(&state));
    state["peers"]["fixture-peer"]["node"] = json!("other-node");
    assert!(!preview.joined_in(&state));
    state["peers"]["fixture-peer"]["node"] = json!("remote-node");
    state["node_id"] = json!("other-local");
    assert!(!preview.joined_in(&state));
}
