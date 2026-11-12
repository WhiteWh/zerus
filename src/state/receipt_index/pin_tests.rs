use super::*;

fn unresolved(source: Source) -> Value {
    let mut record = value(1, "", "run", "", 1.0);
    record["first_message"] = json!(true);
    if source == Source::Question {
        record["question_id"] = json!("question");
        record["question_hash"] = json!("question-hash");
    }
    record
}
fn migrate(state: &Path) {
    let mut migration = migration::Migration::new(state).unwrap();
    while !migration.step().unwrap() {}
}
fn coverage(catalog: &Catalog, source: Source) -> (bool, i64, String) {
    catalog
        .db
        .query_row(
            "SELECT complete,errors,(SELECT value FROM receipt_meta WHERE key='epoch')
         FROM receipt_sources WHERE source=?",
            [source.key()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn an_external_foreign_pin_is_binding_changed_before_any_worker_pass() {
    // dsh-balance-cl's PR2109 probe: only the cached conversation is changed.
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let record = unresolved(source);
        let path = put(&catalog, source, &record);
        migrate(temp.path());
        let original = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
        let before = coverage(&catalog, source);
        assert!(before.0);
        let external = Connection::open(temp.path().join(DATABASE)).unwrap();
        external
            .execute(
                "UPDATE receipt_rows SET conversation='foreign-conversation' WHERE source=?",
                [source.key()],
            )
            .unwrap();
        let after = coverage(&catalog, source);
        assert!(
            !after.0,
            "unproved pin kept complete coverage for {source:?}"
        );
        assert!(after.1 > 0);
        assert_ne!(after.2, before.2);
        let reason: String = catalog
            .db
            .query_row(
                "SELECT reason FROM receipt_errors WHERE source=?",
                [source.key()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "binding_changed");
        let scope = Scope {
            source,
            name: Some(string(&record, "name").into()),
            run: "other-run".into(),
            conversation: "foreign-conversation".into(),
            agent: "".into(),
        };
        let page = catalog.page(&scope, "", 100, true).unwrap();
        assert_eq!(page.metadata["catalog_complete_at_snapshot"], false);
        assert_eq!(
            (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
            original
        );
    }
}

#[test]
fn a_genuine_original_pin_preserves_coverage_for_every_receipt_family() {
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let mut record = unresolved(source);
        let path = put(&catalog, source, &record);
        migrate(temp.path());
        let before = coverage(&catalog, source);
        record["resolved_conversation_id"] = json!("own-conversation");
        fs::write(&path, record.to_string()).unwrap();
        let original = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
        catalog.remember(source, &path, &record).unwrap();
        assert_eq!(coverage(&catalog, source), before);
        assert_eq!(
            catalog
                .cached_row(source, string(&record, "request_id"))
                .unwrap()
                .unwrap()
                .conversation,
            "own-conversation"
        );
        assert_eq!(
            (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
            original
        );
        // The exemption is gone before the next SQL statement on the same connection.
        catalog
            .db
            .execute(
                "UPDATE receipt_rows SET conversation='foreign' WHERE source=?",
                [source.key()],
            )
            .unwrap();
        assert!(!coverage(&catalog, source).0);
        assert_ne!(coverage(&catalog, source).2, before.2);
    }
}

#[test]
fn supplied_metadata_cannot_pin_a_conversation_the_original_does_not_prove() {
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let record = unresolved(source);
        let path = put(&catalog, source, &record);
        migrate(temp.path());
        let original = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
        let mut claimed = record.clone();
        claimed["resolved_conversation_id"] = json!("foreign-conversation");
        catalog.remember(source, &path, &claimed).unwrap();
        assert!(!coverage(&catalog, source).0);
        assert!(coverage(&catalog, source).1 > 0);
        assert_eq!(
            catalog
                .cached_row(source, string(&record, "request_id"))
                .unwrap()
                .unwrap()
                .conversation,
            ""
        );
        assert_eq!(
            (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
            original
        );
    }
}

#[test]
fn original_proof_checks_all_metadata_and_refuses_missing_or_corrupt_sources() {
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let mut record = unresolved(source);
        record["resolved_conversation_id"] = json!("own-conversation");
        let path = put(&catalog, source, &record);
        let filename = path.file_name().unwrap().to_str().unwrap();
        let stamp = file_stamp(&path).unwrap();
        let binding = Binding::parse(source, &record, filename).unwrap();
        assert!(pin::ProvenPin::read(source, &path, &binding, &stamp).is_some());
        for (key, value) in [
            ("name", json!("foreign")),
            ("run_id", json!("foreign-run")),
            ("agent_id", json!("foreign-child")),
            ("resolved_conversation_id", json!("foreign")),
            ("first_message", json!(false)),
            ("submitted_at", json!(99.0)),
            ("attachments", json!([])),
        ] {
            let mut claim = record.clone();
            claim[key] = value;
            let binding = Binding::parse(source, &claim, filename).unwrap();
            assert!(
                pin::ProvenPin::read(source, &path, &binding, &stamp).is_none(),
                "accepted {source:?}/{key}"
            );
        }
        fs::write(&path, "{").unwrap();
        assert!(
            pin::ProvenPin::read(source, &path, &binding, &file_stamp(&path).unwrap()).is_none()
        );
        fs::remove_file(&path).unwrap();
        assert!(pin::ProvenPin::read(source, &path, &binding, &stamp).is_none());
    }
}

#[test]
fn a_failed_pin_statement_restores_the_guard_and_keeps_an_explicit_gap() {
    use rusqlite::config::DbConfig;
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut record = unresolved(Source::Terminal);
    let path = put(&catalog, Source::Terminal, &record);
    migrate(temp.path());
    catalog
        .db
        .execute_batch(
            "ALTER TABLE receipt_rows ADD COLUMN reject_pin INTEGER CHECK(conversation='')",
        )
        .unwrap();
    record["resolved_conversation_id"] = json!("own-conversation");
    fs::write(&path, record.to_string()).unwrap();
    catalog.remember(Source::Terminal, &path, &record).unwrap();
    assert!(catalog
        .db
        .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)
        .unwrap());
    let after = coverage(&catalog, Source::Terminal);
    assert!(!after.0 && after.1 > 0);
    catalog
        .db
        .execute("UPDATE receipt_rows SET run='foreign-run'", [])
        .unwrap();
    assert_ne!(coverage(&catalog, Source::Terminal).2, after.2);
}

#[test]
fn a_temporary_trigger_cannot_undo_a_pin_and_leave_complete_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut record = unresolved(Source::Terminal);
    let path = put(&catalog, Source::Terminal, &record);
    migrate(temp.path());
    catalog.db.execute_batch("CREATE TEMP TRIGGER undo_pin AFTER UPDATE OF conversation ON main.receipt_rows
        WHEN NEW.conversation<>'' BEGIN UPDATE receipt_rows SET conversation='' WHERE seq=NEW.seq; END;").unwrap();
    record["resolved_conversation_id"] = json!("own-conversation");
    fs::write(&path, record.to_string()).unwrap();
    catalog.remember(Source::Terminal, &path, &record).unwrap();
    assert!(!coverage(&catalog, Source::Terminal).0);
    assert!(coverage(&catalog, Source::Terminal).1 > 0);
}

#[test]
fn a_legacy_sql_only_pin_exemption_is_unverified_and_upgrades_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let record = unresolved(Source::Terminal);
    let path = put(&catalog, Source::Terminal, &record);
    migrate(temp.path());
    let original = fs::read(&path).unwrap();
    let guard: String = catalog
        .db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='receipt_row_rebound'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let legacy = guard.replacen(
        "BEGIN",
        "AND NOT (OLD.conversation='' AND NEW.conversation<>'') BEGIN",
        1,
    );
    catalog.db.execute_batch(&format!("DROP TRIGGER receipt_row_rebound; {legacy}; UPDATE receipt_meta SET value='2' WHERE key='rowset_protocol';")).unwrap();
    assert!(Catalog::open(temp.path(), false).is_err());
    let upgraded = Catalog::open(temp.path(), true).unwrap();
    assert!(!coverage(&upgraded, Source::Terminal).0);
    migrate(temp.path());
    assert!(coverage(&upgraded, Source::Terminal).0);
    upgraded
        .db
        .execute("UPDATE receipt_rows SET conversation='foreign'", [])
        .unwrap();
    assert!(!coverage(&upgraded, Source::Terminal).0);
    assert!(coverage(&upgraded, Source::Terminal).1 > 0);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn a_different_file_cannot_stand_in_for_this_catalogs_original() {
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let mut record = unresolved(source);
        let path = put(&catalog, source, &record);
        migrate(temp.path());
        let before = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
        record["resolved_conversation_id"] = json!("foreign-conversation");
        let substituted = foreign.path().join(path.file_name().unwrap());
        fs::write(&substituted, record.to_string()).unwrap();
        catalog.remember(source, &substituted, &record).unwrap();
        assert!(
            !coverage(&catalog, source).0,
            "a foreign file proved {source:?}'s pin"
        );
        assert!(coverage(&catalog, source).1 > 0);
        assert_eq!(
            catalog
                .cached_row(source, string(&record, "request_id"))
                .unwrap()
                .unwrap()
                .conversation,
            ""
        );
        assert_eq!(
            (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
            before
        );
    }
}
