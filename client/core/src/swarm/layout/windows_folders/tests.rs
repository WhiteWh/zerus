use super::*;
const NODE: &str = "10000000-0000-4000-8000-000000000001";
const OTHER: &str = "20000000-0000-4000-8000-000000000002";
const NEW: &str = "30000000-0000-4000-8000-000000000003";
fn org() -> Value {
    json!({"projects":[{"id":"p","folders":[]},{"id":"q","folders":[]}]})
}
fn row(id: &str, path: &str) -> Value {
    json!({"id":id,"machine":"old-alias","machine_id":NODE,"path":path,"name":"Source","private_extension":{"retain":true}})
}
fn value() -> Value {
    json!({"version":1,"node_id":NODE,"projects":{"p":[row("original",r"C:\Source") ]},"private_family_extension":{"retain":true}})
}
fn loaded(value: Value) -> Layout {
    Layout::try_from_json(
        &serde_json::to_vec(&json!({"version":1,KEY:value,"unknown_layout":{"keep":true}}))
            .unwrap(),
    )
    .unwrap()
}
fn original(layout: &Layout) -> swarm::Folder {
    swarm::folder_value(&layout.extra[KEY]["projects"]["p"][0]).unwrap()
}
#[test]
fn absent_initializer_is_whole_once_and_preserves_source_and_extensions() {
    let mut layout = Layout::default();
    let source = value();
    let source_before = source.clone();
    assert_eq!(
        layout.local_windows_folders_state(NODE).unwrap(),
        State::Absent
    );
    assert_eq!(
        layout
            .initialize_local_windows_folders(source.clone(), NODE, &org())
            .unwrap(),
        Initialized::Inserted
    );
    assert_eq!(source, source_before);
    assert_eq!(layout.extra[KEY], source);
    let bytes = layout.to_json();
    let mut loaded = Layout::try_from_json(&bytes).unwrap();
    assert_eq!(
        loaded
            .initialize_local_windows_folders(
                json!({"version":1,"node_id":NODE,"projects":{}}),
                NODE,
                &org()
            )
            .unwrap(),
        Initialized::AlreadyPresent
    );
    assert_eq!(loaded.to_json(), bytes);
}
#[test]
fn explicit_empty_null_and_unsupported_are_never_absence() {
    for family in [
        json!({"version":1,"node_id":NODE,"projects":{}}),
        Value::Null,
        json!({"version":2}),
        json!({"version":1,"node_id":NODE,"projects":[]}),
    ] {
        let bytes = serde_json::to_vec(&json!({"version":1,KEY:family})).unwrap();
        let mut layout = Layout::from_json(&bytes);
        assert!(layout.has_local_windows_folders());
        let before = layout.to_json();
        assert_eq!(
            layout
                .initialize_local_windows_folders(value(), NODE, &org())
                .unwrap(),
            Initialized::AlreadyPresent
        );
        assert_eq!(layout.to_json(), before);
        if family["version"] != 1 || !family["projects"].is_object() {
            assert!(Layout::try_from_json(&bytes).is_err());
        }
    }
}
#[test]
fn existing_presence_wins_before_any_irrelevant_candidate_owner_or_catalog_decode() {
    for family in [
        Value::Null,
        json!({"version":1,"node_id":NODE,"projects":{}}),
        json!({"version":77}),
    ] {
        let bytes = serde_json::to_vec(&json!({"version":1,KEY:family})).unwrap();
        // Compatibility projection for a presence check only; strict file
        // writers refuse null/unsupported and retain their original bytes.
        let mut layout = Layout::from_json(&bytes);
        let before = layout.to_json();
        assert_eq!(
            layout
                .initialize_local_windows_folders(Value::Null, "", &Value::Null)
                .unwrap(),
            Initialized::AlreadyPresent
        );
        assert_eq!(layout.to_json(), before);
    }
}
#[test]
fn set_requires_the_explicit_current_place_owner_and_never_retags_foreign_inputs() {
    let change = folder_edit::Change::Set {
        original: None,
        id: NEW.into(),
        machine: "old-alias".into(),
        path: "D:/Example".into(),
        name: String::new(),
    };
    for owner in [None, Some(OTHER), Some("")] {
        let mut layout = loaded(value());
        let before = layout.to_json();
        assert_eq!(
            layout
                .edit_local_windows_folder(&org(), NODE, "p", owner, &change)
                .unwrap_err(),
            OWNER
        );
        assert_eq!(layout.to_json(), before);
    }
}
#[test]
fn changed_or_unknown_node_is_dormant_and_cannot_rebind() {
    let mut layout = loaded(value());
    let before = layout.to_json();
    for node in [OTHER, ""] {
        assert_eq!(
            layout.local_windows_folders_state(node).unwrap(),
            State::Dormant
        );
        assert_eq!(
            layout.overlay_local_windows_folders(&org(), node).unwrap(),
            org()
        );
        assert_eq!(
            layout.remove_local_windows_project(node, "p").unwrap_err(),
            OWNER
        );
    }
    let other = json!({"version":1,"node_id":OTHER,"projects":{}});
    assert_eq!(
        layout
            .initialize_local_windows_folders(other, OTHER, &org())
            .unwrap(),
        Initialized::AlreadyPresent
    );
    assert_eq!(layout.to_json(), before);
}
#[test]
fn missing_or_ambiguous_source_ownership_refuses_the_whole_family() {
    for missing in [Value::Null, json!(OTHER), json!("old-alias")] {
        let mut candidate = value();
        candidate["projects"]["p"][0]["machine_id"] = missing;
        let mut layout = Layout::default();
        assert!(layout
            .initialize_local_windows_folders(candidate, NODE, &org())
            .is_err());
        assert!(!layout.has_local_windows_folders());
    }
    let mut candidate = value();
    candidate["projects"]["missing"] = json!([]);
    let mut layout = Layout::default();
    assert_eq!(
        layout
            .initialize_local_windows_folders(candidate, NODE, &org())
            .unwrap_err(),
        swarm::edit::GONE
    );
    assert!(!layout.has_local_windows_folders());
}
#[test]
fn duplicate_ids_or_normalized_windows_paths_are_refused() {
    let candidates = [
        json!({"version":1,"node_id":NODE,"projects":{"p":[row("same",r"C:\A")],"q":[row("same",r"D:\B")]}}),
        json!({"version":1,"node_id":NODE,"projects":{"p":[row("one",r"C:\Repo\A\..\B"),row("two","c:/repo/b")]}}),
        json!({"version":1,"node_id":NODE,"projects":{"p":[row("one",r"\\Server\Share\Folder"),row("two","//server/share/folder/")]}}),
    ];
    for candidate in candidates {
        let mut layout = Layout::default();
        assert_eq!(
            layout
                .initialize_local_windows_folders(candidate, NODE, &org())
                .unwrap_err(),
            AMBIGUOUS
        );
        assert!(!layout.has_local_windows_folders());
    }
}
#[test]
fn drive_relative_device_and_incomplete_unc_paths_are_not_folders() {
    for path in [
        r"C:relative",
        r"\root",
        r"\\server",
        r"\\?\C:\",
        r"\\.\pipe\name",
        "/posix",
    ] {
        let mut candidate = value();
        candidate["projects"]["p"][0]["path"] = json!(path);
        let mut layout = Layout::default();
        assert!(layout
            .initialize_local_windows_folders(candidate, NODE, &org())
            .is_err());
        assert!(!layout.has_local_windows_folders());
    }
}
#[test]
fn overlay_preserves_peer_posix_and_never_edits_shared_input_or_dormant_entries() {
    let layout = loaded(value());
    let mut organization = org();
    let peer = json!({"id":"peer","machine":"peer","machine_id":OTHER,"path":"//server/share","private_extension":7});
    organization["projects"][0]["folders"] = json!([peer]);
    let before = organization.clone();
    let shown = layout
        .overlay_local_windows_folders(&organization, NODE)
        .unwrap();
    assert_eq!(organization, before);
    assert_eq!(shown["projects"][0]["folders"][0], peer);
    assert_eq!(shown["projects"][0]["folders"].as_array().unwrap().len(), 2);
    assert!(!shown.as_object().unwrap().contains_key(KEY));
    let missing = json!({"projects":[{"id":"q","folders":[]}]});
    assert_eq!(
        layout
            .overlay_local_windows_folders(&missing, NODE)
            .unwrap(),
        missing
    );
    assert_eq!(layout.extra[KEY], value());
}
#[test]
fn edits_preserve_original_ids_and_extensions_and_use_windows_comparison() {
    let mut layout = loaded(value());
    let old = original(&layout);
    layout
        .edit_local_windows_folder(
            &org(),
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Set {
                original: Some(old),
                id: NEW.into(),
                machine: "new-alias".into(),
                path: r"c:\Changed\A\..\B".into(),
                name: " New label ".into(),
            },
        )
        .unwrap();
    let result = &layout.extra[KEY]["projects"]["p"][0];
    assert_eq!(result["id"], "original");
    assert_eq!(result["path"], "c:/Changed/B");
    assert_eq!(result["machine_id"], NODE);
    assert_eq!(result["private_extension"]["retain"], true);
    assert_eq!(
        layout.extra[KEY]["private_family_extension"]["retain"],
        true
    );
    assert!(
        serde_json::from_slice::<Value>(&layout.to_json()).unwrap()["unknown_layout"]["keep"]
            .as_bool()
            .unwrap()
    );
    let before = layout.to_json();
    assert!(layout
        .edit_local_windows_folder(
            &org(),
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Set {
                original: None,
                id: NEW.into(),
                machine: "another-alias".into(),
                path: "C:/changed/b".into(),
                name: "".into()
            }
        )
        .is_err());
    assert_eq!(layout.to_json(), before);
}
#[test]
fn copy_move_remove_and_explicit_project_deletion_are_metadata_only() {
    let mut layout = loaded(value());
    let old = original(&layout);
    layout
        .edit_local_windows_folder(
            &org(),
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Transfer {
                original: old.clone(),
                destination: "q".into(),
                copy: true,
                id: NEW.into(),
            },
        )
        .unwrap();
    assert_eq!(layout.extra[KEY]["projects"]["q"][0]["id"], NEW);
    assert_eq!(layout.extra[KEY]["projects"]["p"][0]["id"], "original");
    layout
        .edit_local_windows_folder(
            &org(),
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Remove { original: old },
        )
        .unwrap();
    assert_eq!(layout.extra[KEY]["projects"]["p"], json!([]));
    layout.remove_local_windows_project(NODE, "q").unwrap();
    assert!(layout.has_local_windows_folders());
    assert_eq!(
        layout
            .initialize_local_windows_folders(value(), NODE, &org())
            .unwrap(),
        Initialized::AlreadyPresent
    );
}
#[test]
fn stale_records_new_noncanonical_ids_and_shared_windows_edits_refuse() {
    let mut layout = loaded(value());
    let mut old = original(&layout);
    old.path = "D:/replaced".into();
    let before = layout.to_json();
    assert!(layout
        .edit_local_windows_folder(
            &org(),
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Remove { original: old }
        )
        .is_err());
    assert_eq!(layout.to_json(), before);
    let change = folder_edit::Change::Set {
        original: None,
        id: "not-a-new-uuid".into(),
        machine: "local".into(),
        path: "D:/Example".into(),
        name: "".into(),
    };
    assert!(layout
        .edit_local_windows_folder(&org(), NODE, "p", Some(NODE), &change)
        .is_err());
    let change = folder_edit::Change::Set {
        original: None,
        id: NEW.into(),
        machine: "local".into(),
        path: "D:/Example".into(),
        name: "".into(),
    };
    let mut shared = org();
    assert!(folder_edit::apply(&mut shared, "p", &change).is_err());
    assert_eq!(shared, org());
}
#[test]
fn whole_record_project_and_byte_limits_do_not_commit_a_prefix() {
    let mut too_many = value();
    too_many["projects"]["p"] = json!((0..=swarm::FOLDERS)
        .map(|i| row(&format!("old-{i}"), &format!("C:/Example/{i}")))
        .collect::<Vec<_>>());
    let mut too_many_projects = value();
    too_many_projects["projects"] = Value::Object(
        (0..=swarm::MAX_PROJECTS)
            .map(|i| (format!("p-{i}"), json!([])))
            .collect(),
    );
    let mut too_large = value();
    too_large["private_padding"] = json!("x".repeat(MAX_BYTES as usize));
    for candidate in [too_many, too_many_projects, too_large] {
        let mut layout = Layout::default();
        let before = layout.to_json();
        assert_eq!(
            layout
                .initialize_local_windows_folders(candidate, NODE, &org())
                .unwrap_err(),
            LIMIT
        );
        assert_eq!(layout.to_json(), before);
        assert!(!layout.has_local_windows_folders());
    }
}
#[test]
fn combined_shared_bounds_collisions_and_inaccessible_edits_refuse_without_writes() {
    let mut catalog = org();
    catalog["projects"][0]["folders"]=json!((0..swarm::FOLDERS).map(|i|json!({"id":format!("peer-{i}"),"machine":"peer","machine_id":OTHER,"path":format!("/srv/example/{i}")})).collect::<Vec<_>>());
    let mut layout = Layout::default();
    assert_eq!(
        layout
            .initialize_local_windows_folders(value(), NODE, &catalog)
            .unwrap_err(),
        LIMIT
    );
    assert!(!layout.has_local_windows_folders());
    let mut collision = org();
    collision["projects"][1]["folders"] =
        json!([{"id":"original","machine":"peer","path":"/srv/example"}]);
    assert_eq!(
        layout
            .initialize_local_windows_folders(value(), NODE, &collision)
            .unwrap_err(),
        AMBIGUOUS
    );
    assert!(!layout.has_local_windows_folders());
    let mut layout = loaded(value());
    let before = layout.to_json();
    let mut catalog = org();
    catalog["projects"][0]["accessible"] = json!(false);
    assert!(layout
        .edit_local_windows_folder(
            &catalog,
            NODE,
            "p",
            Some(NODE),
            &folder_edit::Change::Remove {
                original: original(&layout)
            }
        )
        .is_err());
    assert_eq!(layout.to_json(), before);
}
