use super::*;
use crate::migration::{Bundle, BundleItem};
use zerus_desktop_migration::{sha256, Evidence, SOURCE};

const ID: &str = "11111111-1111-4111-8111-111111111111";
fn scope(executable: &str, host: &str) -> String {
    serde_json::to_vec(&json!([executable, host]))
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn key(kind: &str) -> String {
    format!(
        "{PREFIX}/{}/{kind}",
        scope("/opt/example/hgs", "configured-peer")
    )
}
fn pending(windows: bool) -> Value {
    json!({"request_id":ID,"url":"https://example.invalid/example.git",
        "parent":if windows {"C:/Repos"}else{"/srv/repos"},"name":"example",
        "path":if windows {"C:/Repos/example"}else{"/srv/repos/example"},
        "future_private":{"original":"retained privately"}})
}
fn snapshot(records: Vec<(String, QtValue)>) -> QtSnapshot {
    QtSnapshot {
        values: records.into_iter().collect(),
        source: "qt-ini",
        sha256: sha256(b"synthetic Qt bytes"),
        modified_at: None,
    }
}
fn raw(value: &Value) -> QtValue {
    QtValue::Bytes(serde_json::to_vec(value).unwrap())
}
fn bundle(source: SourceStream) -> Bundle {
    Bundle::new(
        SOURCE,
        false,
        "a".repeat(64),
        ID.into(),
        1,
        vec![source],
        8 << 20,
    )
    .unwrap()
}
fn bodies(source: SourceStream) -> Vec<Value> {
    let mut bodies = Vec::new();
    let mut record = None;
    for item in bundle(source) {
        match item.unwrap() {
            BundleItem::Snapshot { reference, reader } => {
                reference.verify(reader.get_ref()).unwrap();
                bodies.push(serde_json::from_slice(reader.get_ref()).unwrap());
            }
            BundleItem::Record(value) => record = Some(value),
            _ => (),
        }
    }
    let record = record.unwrap();
    assert_eq!(record.evidence, Evidence::Staged);
    assert_eq!(record.sources[0].family, "qt-repository-clones");
    assert_eq!(record.sources[0].schema, 1);
    assert!(!serde_json::to_string(&record)
        .unwrap()
        .contains("retained privately"));
    bodies
}
fn refused(source: QtSnapshot) -> SourceError {
    let stream = match qt_repository_clones(&source) {
        Ok(stream) => stream,
        Err(error) => return error,
    };
    let mut error = None;
    for item in bundle(stream) {
        match item {
            Ok(BundleItem::Record(_)) => panic!("partial source cannot finish"),
            Err(value) => error = Some(value),
            _ => (),
        }
    }
    error.expect("expected refusal")
}

#[test]
fn exact_scope_pending_previous_and_parent_survive_without_live_clone_state() {
    for windows in [false, true] {
        let pending = pending(windows);
        let qt = snapshot(vec![
            (key("pending"), raw(&pending)),
            (key("previous"), raw(&pending)),
            (key("parent"), QtValue::Text("~".into())),
        ]);
        let original = qt.values.clone();
        let values = bodies(qt_repository_clones(&qt).unwrap());
        assert_eq!(values[0]["present"], true);
        let entries = values[1]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert_eq!(entry["executable"], "/opt/example/hgs");
            assert_eq!(entry["host"], "configured-peer");
            if entry["kind"] == "parent" {
                assert_eq!(entry["record"], "~");
            } else {
                assert_eq!(entry["record"], pending);
            }
            assert_eq!(entry["key"], key(entry["kind"].as_str().unwrap()));
        }
        assert_eq!(qt.values, original);
    }
}

#[test]
fn missing_namespace_is_explicit_and_similarly_named_keys_are_not_discovered() {
    let qt = snapshot(vec![(
        "workspace/repositoryClones/other".into(),
        QtValue::Text("ignored".into()),
    )]);
    let values = bodies(qt_repository_clones(&qt).unwrap());
    assert_eq!(
        values,
        json!([{"kind":"clone-header","present":false}])
            .as_array()
            .unwrap()
            .clone()
    );
}

#[test]
fn malformed_scope_or_unknown_record_kind_cannot_complete_partial_evidence() {
    for suffix in [
        "not-hex/pending",
        "0/pending",
        "6e756c6c/pending",
        "5b312c325d/pending",
        "/pending",
    ] {
        assert_eq!(
            refused(snapshot(vec![(
                format!("{PREFIX}/{suffix}"),
                raw(&pending(false))
            )])),
            SourceError::UnsupportedSource
        );
    }
    for (exe, host) in [("", "peer"), ("/opt/example/hgs", "peer\nother")] {
        assert_eq!(
            refused(snapshot(vec![(
                format!("{PREFIX}/{}/pending", scope(exe, host)),
                raw(&pending(false))
            )])),
            SourceError::UnsupportedSource
        );
    }
    assert_eq!(
        refused(snapshot(vec![
            (key("pending"), raw(&pending(false))),
            (key("future-kind"), QtValue::Text("kept".into()))
        ])),
        SourceError::UnsupportedSource
    );
}

#[test]
fn invalid_original_request_or_destination_and_unsafe_url_are_kept_unhandled() {
    for field in ["request_id", "url", "parent", "name", "path"] {
        let mut bad = pending(false);
        bad.as_object_mut().unwrap().remove(field);
        assert_eq!(
            refused(snapshot(vec![(key("pending"), raw(&bad))])),
            SourceError::UnsupportedSource
        );
    }
    for (field, value) in [
        ("request_id", "not-an-id"),
        ("path", "/srv/other"),
        ("parent", "relative"),
        ("name", "../other"),
        ("url", "https://example.invalid/example?token=synthetic"),
    ] {
        let mut bad = pending(false);
        bad[field] = json!(value);
        assert_eq!(
            refused(snapshot(vec![(key("previous"), raw(&bad))])),
            SourceError::UnsupportedSource
        );
    }
    let mut qt = snapshot(vec![]);
    qt.source = "qt-future";
    assert_eq!(refused(qt), SourceError::UnsupportedSource);
}

#[test]
fn oversized_items_and_changed_sources_never_publish_a_final_record() {
    let mut value = pending(false);
    value["future_private"] = json!("x".repeat(MAX_SNAPSHOT_BYTES as usize));
    assert_eq!(
        refused(snapshot(vec![(key("pending"), raw(&value))])),
        SourceError::TooLarge
    );
    let qt = snapshot(vec![(key("pending"), raw(&pending(false)))]);
    let stream = qt_repository_clones(&qt)
        .unwrap()
        .recheck(|| Ok("b".repeat(64)));
    let mut changed = false;
    for item in bundle(stream) {
        match item {
            Ok(BundleItem::Record(_)) => panic!("changed source cannot finish"),
            Err(SourceError::SourceChanged) => changed = true,
            _ => (),
        }
    }
    assert!(changed);
}

#[test]
fn existing_ini_and_plist_readers_leave_the_complete_synthetic_source_unchanged() {
    use crate::settings::QtStore;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let root = std::env::temp_dir().join(format!("zerus-g3-clone-source-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    for mac in [false, true] {
        let text = pending(false).to_string();
        let (store, bytes) = if mac {
            (
                QtStore::Plist {
                    path: root.join("synthetic.plist"),
                    domain: None,
                },
                format!(
                    "<plist version=\"1.0\"><dict><key>{}</key><data>{}</data></dict></plist>",
                    key("pending").replace('/', "."),
                    STANDARD.encode(text)
                )
                .into_bytes(),
            )
        } else {
            (
                QtStore::Ini(root.join("synthetic.conf")),
                format!(
                    "[workspace]\n{}={}\n",
                    key("pending")
                        .strip_prefix("workspace/")
                        .unwrap()
                        .replace('/', "\\"),
                    serde_json::to_string(&text).unwrap()
                )
                .into_bytes(),
            )
        };
        std::fs::write(store.path(), &bytes).unwrap();
        let mtime = std::fs::metadata(store.path()).unwrap().modified().unwrap();
        let qt = store.snapshot().unwrap().unwrap();
        assert_eq!(qt.sha256, sha256(&bytes));
        let values = bodies(qt_repository_clones(&qt).unwrap());
        assert_eq!(values[1]["entries"][0]["record"], pending(false));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(store.path()).unwrap().modified().unwrap(),
            mtime
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn next_bytes(raw: &str) -> Vec<u8> {
    format!(r#"{{"version":1,"values":{{"unrelated":"private setting stays outside this family"}},"repository_clones":{raw}}}"#).into_bytes()
}
fn next_bodies(source: SourceStream, hash: &str) -> Vec<Value> {
    let mut values = Vec::new();
    let mut finished = false;
    for item in bundle(source) {
        match item.unwrap() {
            BundleItem::Snapshot { reference, reader } => {
                reference.verify(reader.get_ref()).unwrap();
                assert!(reference.bytes <= MAX_SNAPSHOT_BYTES);
                values.push(serde_json::from_slice(reader.get_ref()).unwrap());
            }
            BundleItem::Record(record) => {
                assert_eq!(record.evidence, Evidence::Staged);
                assert_eq!(record.sources[0].family, "next-repository-clones");
                assert_eq!(record.sources[0].format, "settings-json-v1");
                assert_eq!(record.sources[0].schema, 1);
                assert_eq!(record.sources[0].sha256, hash);
                finished = true;
            }
            _ => (),
        }
    }
    assert!(finished);
    values
}
fn original_extension(values: &[Value]) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let mut original = Vec::new();
    for value in &values[1..] {
        assert_eq!(value["kind"], "clone-checkpoint-bytes");
        assert_eq!(value["offset"], original.len());
        original.extend(STANDARD.decode(value["base64"].as_str().unwrap()).unwrap());
    }
    original
}

#[test]
fn next_checkpoint_keeps_exact_original_extension_bytes_and_requests_only() {
    let mut pending = pending(false);
    pending.as_object_mut().unwrap().remove("future_private");
    let mut previous = pending.clone();
    previous["request_id"] = json!("22222222-2222-4222-8222-222222222222");
    let raw = format!("{{ \n  \"version\": 1, \"entries\": {{\"{}\": {{\"previous\": {previous}, \"pending\": {pending}, \"parent\": null}}}} \n}}", "a".repeat(64));
    let bytes = next_bytes(&raw);
    let before = bytes.clone();
    let values = next_bodies(next_repository_clones(&bytes).unwrap(), &sha256(&bytes));
    assert_eq!(values[0]["present"], true);
    assert_eq!(values[0]["bytes"], raw.len());
    assert_eq!(original_extension(&values), raw.as_bytes());
    assert_eq!(bytes, before);
    assert!(!serde_json::to_string(&values)
        .unwrap()
        .contains("private setting stays outside"));
}

#[test]
fn next_absent_and_explicit_empty_extensions_stay_distinct() {
    let absent = br#"{"version":1,"values":{}}"#;
    let values = next_bodies(next_repository_clones(absent).unwrap(), &sha256(absent));
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["present"], false);
    assert_eq!(values[0]["bytes"], Value::Null);
    let raw = r#"{"version":1,"entries":{}}"#;
    let bytes = next_bytes(raw);
    let values = next_bodies(next_repository_clones(&bytes).unwrap(), &sha256(&bytes));
    assert_eq!(values[0]["present"], true);
    assert_eq!(original_extension(&values), raw.as_bytes());
}

#[test]
fn next_future_malformed_duplicate_and_oversized_checkpoints_refuse_the_family() {
    let scope = "a".repeat(64);
    for raw in [
        "null".to_owned(),
        r#"{"version":2,"entries":{}}"#.into(),
        r#"{"version":1,"entries":null}"#.into(),
        r#"{"version":1,"entries":{},"future":true}"#.into(),
        format!(
            r#"{{"version":1,"entries":{{"{scope}":{{"pending":{{"request_id":"not-an-id"}}}}}}}}"#
        ),
        r#"{"version":1,"entries":{"wrong-scope":{}}}"#.into(),
        format!(
            r#"{{"version":1,"entries":{{"{scope}":{{}},"{scope}":{{"parent":"/srv/other"}}}}}}"#
        ),
        format!(
            r#"{{"version":1,"entries":{{"{scope}":{{"parent":"/srv/example","parent":"/srv/other"}}}}}}"#
        ),
        format!(r#"{{"version":1,"entries":{{"{scope}":{{"future":true}}}}}}"#),
    ] {
        assert!(
            matches!(
                next_repository_clones(&next_bytes(&raw)),
                Err(SourceError::UnsupportedSource)
            ),
            "{raw}"
        );
    }
    let entries: serde_json::Map<String, Value> =
        (0..65).map(|n| (format!("{n:064x}"), json!({}))).collect();
    assert!(next_repository_clones(&next_bytes(
        &json!({"version":1,"entries":entries}).to_string()
    ))
    .is_err());
    assert!(matches!(
        next_repository_clones(&vec![b' '; (4 << 20) + 1]),
        Err(SourceError::TooLarge)
    ));
    let duplicate = br#"{"repository_clones":{"version":1,"entries":{}},"repository_clones":{"version":1,"entries":{}}}"#;
    assert!(next_repository_clones(duplicate).is_err());
}

#[test]
fn next_credential_bearing_pending_and_previous_urls_remain_in_the_original_only() {
    for field in ["pending", "previous"] {
        for url in [
            "https://fixture-user:fixture-secret@example.invalid/repo.git",
            "https://example.invalid/repo.git?token=synthetic",
            "https://example.invalid/repo.git#synthetic-fragment",
        ] {
            let mut pending = pending(false);
            pending.as_object_mut().unwrap().remove("future_private");
            pending["url"] = json!(url);
            let raw = json!({"version":1,"entries":{"a".repeat(64):{field:pending}}}).to_string();
            let bytes = next_bytes(&raw);
            let before = bytes.clone();
            assert!(matches!(
                next_repository_clones(&bytes),
                Err(SourceError::UnsupportedSource)
            ));
            assert_eq!(bytes, before);
        }
    }
}

#[test]
fn next_large_original_extension_is_chunked_with_exact_offsets_and_utf8_bytes() {
    let entries: serde_json::Map<String, Value> = (0..20)
        .map(|n| {
            (
                format!("{n:064x}"),
                json!({"parent":format!("/srv/{}","я".repeat(15000))}),
            )
        })
        .collect();
    let raw = json!({"version":1,"entries":entries}).to_string();
    assert!(raw.len() > 512 << 10);
    let bytes = next_bytes(&raw);
    let values = next_bodies(next_repository_clones(&bytes).unwrap(), &sha256(&bytes));
    assert!(values.len() > 2);
    assert_eq!(original_extension(&values), raw.as_bytes());
}

#[test]
fn next_source_recheck_refuses_a_final_record_and_never_changes_settings() {
    let root = std::env::temp_dir().join(format!("zerus-g3-next-clone-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("settings.json");
    let bytes = next_bytes(r#"{"version":1,"entries":{}}"#);
    std::fs::write(&path, &bytes).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let source = next_repository_clones(&std::fs::read(&path).unwrap())
        .unwrap()
        .recheck(|| Ok("b".repeat(64)));
    let mut changed = false;
    for item in bundle(source) {
        match item {
            Ok(BundleItem::Record(_)) => panic!("changed source completed"),
            Err(SourceError::SourceChanged) => changed = true,
            _ => (),
        }
    }
    assert!(changed);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    std::fs::remove_dir_all(root).unwrap();
}
