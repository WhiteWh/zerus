use super::*;
use std::path::PathBuf;

const NODE: &str = "10000000-0000-4000-8000-000000000001";
const OTHER: &str = "20000000-0000-4000-8000-000000000002";

fn catalog() -> Value {
    json!({"node_id":NODE,"organization":{"projects":[
        {"id":"alpha","folders":[]},{"id":"beta","folders":[]}]}})
}
fn context(catalog: &Value) -> WindowsFoldersContext<'_> {
    WindowsFoldersContext {
        source_node: NODE,
        current_node: NODE,
        catalog,
    }
}
fn row(id: &str, path: &str) -> Value {
    json!({"id":id,"machine":"old-alias","machine_id":NODE,
        "path":path,"name":"Original","private_extension":{"keep":true}})
}
fn organization() -> Value {
    let mut foreign = row("foreign", "D:/peer");
    foreign["machine_id"] = json!(OTHER);
    json!({"version":2,"projects":[
        {"id":"alpha","sessions":[],"folders":[row("drive",r"C:\Source"),foreign]},
        {"id":"beta","sessions":[],"folders":[row("unc",r"\\server\share\Source"),row("posix","/srv/example")]}],
        "private_root":"only in original source"})
}
fn snapshot(value: &Value) -> QtSnapshot {
    let bytes = serde_json::to_vec(value).unwrap();
    QtSnapshot {
        values: [(KEY.into(), QtValue::Bytes(bytes.clone()))].into(),
        source: "qt-ini",
        sha256: zerus_desktop_migration::sha256(&bytes),
        modified_at: Some(20),
    }
}
fn seed() -> WindowsFoldersSeed {
    decode(
        &QtStore::Ini("synthetic-qt.ini".into()),
        Some(&snapshot(&organization())),
        &context(&catalog()),
        Some(10),
    )
    .unwrap()
}
fn saved(layout: &Layout) -> Value {
    serde_json::from_slice(&layout.to_json()).unwrap()
}
struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("zerus-g3-windows-{name}-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn original_explicit_local_rows_and_handled_metadata_share_one_layout_image() {
    let mut layout = Layout::default();
    layout
        .extra
        .insert("unrelated".into(), json!({"keep":true}));
    layout.remember_run("peer", "original", Some("alpha"));
    let before = saved(&layout);
    assert!(layout
        .seed_windows_folders(seed(), &context(&catalog()))
        .unwrap());
    let value = saved(&layout);
    let family = &value[windows_folders::KEY];
    assert_eq!(family["node_id"], NODE);
    assert_eq!(
        family["projects"]["alpha"],
        json!([organization()["projects"][0]["folders"][0]])
    );
    assert_eq!(
        family["projects"]["beta"],
        json!([organization()["projects"][1]["folders"][0]])
    );
    assert_eq!(value[RECORD]["family"], FAMILY);
    assert_eq!(value[RECORD]["schema"], 1);
    assert_eq!(value[RECORD]["keys"], json!([KEY]));
    assert_eq!(value[RECORD]["sha256"], snapshot(&organization()).sha256);
    assert_eq!(value[RECORD]["source_mtime_after_switch"], true);
    assert_eq!(value["run_groups"], before["run_groups"]);
    assert_eq!(value["unrelated"], before["unrelated"]);
    assert!(!value.to_string().contains("only in original source"));
    let mut reloaded = Layout::try_from_json(&layout.to_json()).unwrap();
    assert!(!reloaded.windows_folders_import_pending().unwrap());
    let before = reloaded.to_json();
    assert!(!reloaded
        .seed_windows_folders(seed(), &context(&Value::Null))
        .unwrap());
    assert_eq!(reloaded.to_json(), before);
    // Removing the family alone cannot make an already handled family reseed.
    reloaded.extra.remove(windows_folders::KEY);
    assert!(!reloaded.windows_folders_import_pending().unwrap());
}

#[test]
fn target_presence_wins_before_any_source_or_owner_decode() {
    let store = QtStore::Ini("unreadable-unused-source".into());
    for family in [
        Value::Null,
        json!({"version":77}),
        json!({"version":1,"node_id":NODE,"projects":{}}),
    ] {
        // Compatibility projection tests only presence. Strict production
        // layout loading refuses unsupported originals before any writer.
        let mut layout = Layout::from_json(
            &json!({windows_folders::KEY:family})
                .to_string()
                .into_bytes(),
        );
        let before = layout.to_json();
        assert!(read_checked(
            &layout,
            &store,
            Path::new("unused"),
            &context(&Value::Null),
            None,
            || panic!("a present target must never open Qt")
        )
        .unwrap()
        .is_none());
        assert!(!layout
            .seed_windows_folders(seed(), &context(&Value::Null))
            .unwrap());
        assert_eq!(layout.to_json(), before);
    }
}

#[test]
fn unsupported_handled_record_never_reads_or_changes_source_or_target() {
    for record in [Value::Null, json!({"version":2}), json!({"family":FAMILY})] {
        let layout =
            Layout::try_from_json(&json!({RECORD:record}).to_string().into_bytes()).unwrap();
        let before = layout.to_json();
        assert!(read_checked(
            &layout,
            &QtStore::Ini("unused".into()),
            Path::new("unused-target"),
            &context(&Value::Null),
            None,
            || panic!("unsupported record must refuse before Qt")
        )
        .is_err());
        assert_eq!(layout.to_json(), before);
    }
}

#[test]
fn independent_source_watch_and_catalog_nodes_must_match_before_a_read() {
    let catalog = catalog();
    for owner in [
        WindowsFoldersContext {
            source_node: "",
            ..context(&catalog)
        },
        WindowsFoldersContext {
            source_node: "old-alias",
            ..context(&catalog)
        },
        WindowsFoldersContext {
            source_node: OTHER,
            ..context(&catalog)
        },
        WindowsFoldersContext {
            current_node: OTHER,
            ..context(&catalog)
        },
        context(&Value::Null),
    ] {
        assert!(read_checked(
            &Layout::default(),
            &QtStore::Ini("unused".into()),
            Path::new("unused-target"),
            &owner,
            None,
            || panic!("unproven identity must refuse before source I/O")
        )
        .is_err());
    }
}

#[test]
fn fresh_node_project_and_collision_checks_refuse_without_partial_mutation() {
    let mut changed_node = catalog();
    changed_node["node_id"] = json!(OTHER);
    let mut removed = catalog();
    removed["organization"]["projects"]
        .as_array_mut()
        .unwrap()
        .pop();
    let mut collision = catalog();
    collision["organization"]["projects"][0]["folders"] = json!([row("drive", "/srv/example")]);
    for catalog in [changed_node, removed, collision] {
        let mut layout = Layout::default();
        let before = layout.to_json();
        assert!(layout
            .seed_windows_folders(seed(), &context(&catalog))
            .is_err());
        assert_eq!(layout.to_json(), before);
        assert!(layout.windows_folders_import_pending().unwrap());
    }
    let catalog = json!({"node_id":OTHER,"organization":catalog()["organization"]});
    let owner = WindowsFoldersContext {
        source_node: OTHER,
        current_node: OTHER,
        catalog: &catalog,
    };
    assert!(Layout::default()
        .seed_windows_folders(seed(), &owner)
        .is_err());
}

#[test]
fn missing_or_ambiguous_original_row_owner_never_uses_an_alias_or_path() {
    let mut candidates = Vec::new();
    for owner in [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!("old-alias")),
        Some(json!(42)),
    ] {
        let mut value = organization();
        let row = value["projects"][0]["folders"][0].as_object_mut().unwrap();
        row.remove("machine_id");
        if let Some(owner) = owner {
            row.insert("machine_id".into(), owner);
        }
        candidates.push(value);
    }
    for value in candidates {
        assert!(decode(
            &QtStore::Ini("unused".into()),
            Some(&snapshot(&value)),
            &context(&catalog()),
            None
        )
        .is_err());
    }
}

#[test]
fn malformed_duplicate_future_and_oversized_sources_stay_unhandled() {
    let mut candidates = vec![
        json!({}),
        json!({"version":3,"projects":[]}),
        json!({"projects":null}),
    ];
    for field in ["id", "folders"] {
        let mut value = organization();
        value["projects"][0][field] = Value::Null;
        candidates.push(value);
    }
    for field in ["id", "machine", "path", "name"] {
        let mut value = organization();
        value["projects"][0]["folders"][0][field] = Value::Null;
        candidates.push(value);
    }
    let mut duplicate = organization();
    duplicate["projects"][1]["folders"][0]["id"] = json!("drive");
    candidates.push(duplicate);
    let mut duplicate = organization();
    duplicate["projects"][0]["folders"]
        .as_array_mut()
        .unwrap()
        .push(row("same-path", r"c:/SOURCE"));
    candidates.push(duplicate);
    let mut duplicate = organization();
    duplicate["projects"][1]["id"] = json!("alpha");
    candidates.push(duplicate);
    let mut excess = organization();
    excess["projects"][0]["folders"] = json!(vec![row("many", "C:/Source"); swarm::FOLDERS + 1]);
    candidates.push(excess);
    for value in candidates {
        assert!(
            decode(
                &QtStore::Ini("unused".into()),
                Some(&snapshot(&value)),
                &context(&catalog()),
                None
            )
            .is_err(),
            "{value}"
        );
    }
    let mut qt = snapshot(&organization());
    qt.values
        .insert(KEY.into(), QtValue::Bytes(vec![b' '; (16 << 20) + 1]));
    assert!(decode(
        &QtStore::Ini("unused".into()),
        Some(&qt),
        &context(&catalog()),
        None
    )
    .is_err());
    assert!(decode(
        &QtStore::Ini("unused".into()),
        None,
        &context(&catalog()),
        None
    )
    .is_err());
}

#[test]
fn an_explicit_empty_source_imports_once_without_resurrecting_individual_rows() {
    let source = json!({"version":2,"projects":[]});
    let seed = decode(
        &QtStore::Ini("unused".into()),
        Some(&snapshot(&source)),
        &context(&catalog()),
        None,
    )
    .unwrap();
    let mut layout = Layout::default();
    layout
        .seed_windows_folders(seed, &context(&catalog()))
        .unwrap();
    assert_eq!(saved(&layout)[windows_folders::KEY]["projects"], json!({}));
    let before = layout.to_json();
    assert!(!layout
        .seed_windows_folders(super::tests::seed(), &context(&catalog()))
        .unwrap());
    assert_eq!(layout.to_json(), before);
}

#[test]
fn legacy_groups_reuses_the_existing_decoder_and_keeps_original_folder_rows() {
    let mut value = organization();
    let projects = value.as_object_mut().unwrap().remove("projects").unwrap();
    value["groups"] = projects;
    value["default_project"] = json!("alpha");
    let seed = decode(
        &QtStore::Ini("unused".into()),
        Some(&snapshot(&value)),
        &context(&catalog()),
        None,
    )
    .unwrap();
    let mut layout = Layout::default();
    layout
        .seed_windows_folders(seed, &context(&catalog()))
        .unwrap();
    assert_eq!(
        saved(&layout)[windows_folders::KEY]["projects"]["alpha"][0],
        value["groups"][0]["folders"][0]
    );
}

#[test]
fn byte_limit_rejects_the_whole_candidate_and_record() {
    let mut layout = Layout::default();
    layout.extra.insert(
        "padding".into(),
        json!("x".repeat(MAX_BYTES as usize - layout.to_json().len() - 20)),
    );
    let before = layout.to_json();
    assert!(before.len() as u64 <= MAX_BYTES);
    assert!(layout
        .seed_windows_folders(seed(), &context(&catalog()))
        .is_err());
    assert_eq!(layout.to_json(), before);
    assert!(layout.windows_folders_import_pending().unwrap());
}

#[test]
fn raw_ini_and_plist_sources_keep_bytes_mtime_and_hash_and_refuse_overlap() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let fixture = Fixture::new("raw");
    for mac in [false, true] {
        let text = organization().to_string();
        let (store, bytes) = if mac {
            (QtStore::Plist { path:fixture.0.join("synthetic.plist"),domain:None },
             format!("<plist version=\"1.0\"><dict><key>workspace.organization</key><data>{}</data></dict></plist>",STANDARD.encode(text)).into_bytes())
        } else {
            (
                QtStore::Ini(fixture.0.join("synthetic.ini")),
                format!(
                    "[workspace]\norganization={}\n",
                    serde_json::to_string(&text).unwrap()
                )
                .into_bytes(),
            )
        };
        std::fs::write(store.path(), &bytes).unwrap();
        let mtime = std::fs::metadata(store.path()).unwrap().modified().unwrap();
        let seed = qt_windows_folders(
            &Layout::default(),
            &store,
            &fixture.0.join("layout.json"),
            &context(&catalog()),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            seed.record["sha256"],
            zerus_desktop_migration::sha256(&bytes)
        );
        assert!(qt_windows_folders(
            &Layout::default(),
            &store,
            store.path(),
            &context(&catalog()),
            None
        )
        .is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(store.path()).unwrap().modified().unwrap(),
            mtime
        );
    }
}

#[test]
fn changed_unreadable_or_disappearing_final_source_never_yields_a_seed() {
    let fixture = Fixture::new("recheck");
    for change in 0..3 {
        let mut reads = 0;
        assert!(read_checked(
            &Layout::default(),
            &QtStore::Ini(fixture.0.join("source.ini")),
            &fixture.0.join("layout.json"),
            &context(&catalog()),
            None,
            || {
                reads += 1;
                let mut qt = snapshot(&organization());
                if reads == 2 {
                    match change {
                        0 => qt.sha256 = "b".repeat(64),
                        1 => return Err("synthetic read failure".into()),
                        _ => return Ok(None),
                    }
                }
                Ok(Some(qt))
            }
        )
        .is_err());
        assert_eq!(reads, 2);
    }
}

#[cfg(unix)]
#[test]
fn failed_existing_layout_checkpoint_restarts_unhandled_and_success_commits_both() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("checkpoint");
    let path = fixture.0.join("layout.json");
    let original = Layout::default().to_json();
    swarm::draft::write(&path, &original).unwrap();
    let mut layout = Layout::try_from_json(&original).unwrap();
    layout
        .seed_windows_folders(seed(), &context(&catalog()))
        .unwrap();
    std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = swarm::draft::write(&path, &layout.to_json());
    std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut restarted = Layout::try_from_json(&std::fs::read(&path).unwrap()).unwrap();
    assert!(restarted.windows_folders_import_pending().unwrap());
    restarted
        .seed_windows_folders(seed(), &context(&catalog()))
        .unwrap();
    swarm::draft::write(&path, &restarted.to_json()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value.get(windows_folders::KEY).is_some());
    assert!(value.get(RECORD).is_some());
    let restarted = Layout::try_from_json(&bytes).unwrap();
    assert!(!restarted.windows_folders_import_pending().unwrap());
    assert!(read_checked(
        &restarted,
        &QtStore::Ini("unused".into()),
        Path::new("unused"),
        &context(&Value::Null),
        None,
        || panic!("a handled restart must not reopen Qt")
    )
    .unwrap()
    .is_none());
}
