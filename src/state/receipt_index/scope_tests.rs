use super::*;

fn migrate(state: &Path) {
    let mut worker = migration::Migration::new(state).unwrap();
    while !worker.step().unwrap() {}
}

fn receipt(source: Source, first: bool) -> Value {
    let mut value = value(1, "conversation", "run", "", 10.0);
    if source == Source::Question {
        value["question_id"] = json!("question");
        value["question_hash"] = json!("question-hash");
    }
    if first {
        value["conversation_id"] = json!("");
        value["resolved_conversation_id"] = json!("conversation");
        value["first_message"] = json!(true);
    }
    value
}

fn source_state(catalog: &Catalog, source: Source) -> (bool, i64) {
    catalog
        .db
        .query_row(
            "SELECT complete,errors FROM receipt_sources WHERE source=?",
            [source.key()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn every_cached_projection_field_is_restored_from_an_unchanged_original() {
    for source in Source::ALL {
        for (field, assignment, first) in [
            ("name", "name='codex/foreign/cache-only'", false),
            ("run", "run='foreign-run'", false),
            ("agent", "agent='foreign-child'", false),
            ("first", "first_message=1", false),
            ("noncanonical-first", "first_message=2", true),
            ("at", "at=999", false),
            ("files", "files=0", false),
            ("noncanonical-files", "files=2", false),
            ("stamp", "stamp='foreign-stamp'", false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(temp.path(), true).unwrap();
            let value = receipt(source, first);
            let path = put(&catalog, source, &value);
            migrate(temp.path());
            let before = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
            let (sequence, change): (i64, i64) = catalog
                .db
                .query_row(
                    "SELECT seq,change FROM receipt_rows WHERE source=?",
                    [source.key()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            catalog
                .db
                .execute(
                    &format!("UPDATE receipt_rows SET {assignment} WHERE source=?"),
                    [source.key()],
                )
                .unwrap();
            migrate(temp.path());
            let binding =
                Binding::parse(source, &value, path.file_name().unwrap().to_str().unwrap())
                    .unwrap();
            let restored = catalog.cached_row(source, &binding.id).unwrap().unwrap();
            assert!(
                restored.matches(&binding),
                "{source:?}/{field} was not restored"
            );
            assert_eq!(restored.fingerprint, binding.fingerprint);
            assert_eq!(restored.stamp, before.1);
            let (next_sequence, next_change): (i64, i64) = catalog
                .db
                .query_row(
                    "SELECT seq,change FROM receipt_rows WHERE source=?",
                    [source.key()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(next_sequence, sequence);
            assert!(
                next_change > change,
                "repair was not published to history consumers"
            );
            assert_eq!(source_state(&catalog, source), (true, 0));
            assert!(catalog.coverage_verified());
            assert_eq!(
                (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
                before
            );
        }
    }
}

#[test]
fn verifying_an_unchanged_projection_does_not_mint_changes_or_rotate_cursors() {
    for source in Source::ALL {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let value = receipt(source, false);
        let path = put(&catalog, source, &value);
        migrate(temp.path());
        let before = change_count(&catalog);
        let epoch: String = catalog
            .db
            .query_row(
                "SELECT value FROM receipt_meta WHERE key='epoch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        catalog.remember(source, &path, &value).unwrap();
        migrate(temp.path());
        assert_eq!(change_count(&catalog), before);
        let after: String = catalog
            .db
            .query_row(
                "SELECT value FROM receipt_meta WHERE key='epoch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, epoch);
        assert_eq!(source_state(&catalog, source), (true, 0));
    }
}

#[test]
fn an_incompatible_fingerprint_or_conversation_pin_remains_a_gap() {
    for source in Source::ALL {
        for assignment in [
            "binding='foreign-binding'",
            "conversation='foreign-pinned-conversation'",
            "files='unreadable'",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(temp.path(), true).unwrap();
            let value = receipt(source, false);
            let path = put(&catalog, source, &value);
            let before = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
            catalog
                .db
                .execute(
                    &format!("UPDATE receipt_rows SET {assignment} WHERE source=?"),
                    [source.key()],
                )
                .unwrap();
            migrate(temp.path());
            assert!(
                source_state(&catalog, source).1 > 0,
                "{source:?}/{assignment} lost its gap"
            );
            assert_eq!(
                (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
                before
            );
        }
    }
}

#[test]
fn a_changed_original_binding_or_first_prompt_pin_is_never_adopted_by_repair() {
    for pin in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let mut value = receipt(Source::Terminal, pin);
        let path = put(&catalog, Source::Terminal, &value);
        if pin {
            value["resolved_conversation_id"] = json!("foreign-conversation");
        } else {
            value["digest"] = json!("foreign-digest");
        }
        fs::write(&path, value.to_string()).unwrap();
        let before = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
        migrate(temp.path());
        assert!(source_state(&catalog, Source::Terminal).1 > 0);
        let row = catalog
            .cached_row(Source::Terminal, string(&value, "request_id"))
            .unwrap()
            .unwrap();
        assert_eq!(row.conversation, "conversation");
        assert_eq!(
            (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
            before
        );
    }
}

#[test]
fn a_cache_trigger_that_undoes_repair_cannot_have_its_discrepancy_cleared() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let value = receipt(Source::Terminal, false);
    let path = put(&catalog, Source::Terminal, &value);
    let before = (fs::read(&path).unwrap(), file_stamp(&path).unwrap());
    catalog
        .db
        .execute_batch(
            "UPDATE receipt_rows SET agent='foreign-child';
         CREATE TRIGGER undo_scope_repair AFTER UPDATE OF agent ON receipt_rows WHEN NEW.agent=''
         BEGIN UPDATE receipt_rows SET agent='foreign-child' WHERE seq=NEW.seq; END;",
        )
        .unwrap();
    migrate(temp.path());
    assert!(source_state(&catalog, Source::Terminal).1 > 0);
    let page = catalog
        .page(&scope("conversation", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(page.metadata["catalog_complete_at_snapshot"], false);
    assert!(page.metadata["unattributed_errors"].as_i64().unwrap() > 0);
    assert_eq!(
        (fs::read(&path).unwrap(), file_stamp(&path).unwrap()),
        before
    );
    catalog
        .db
        .execute_batch("DROP TRIGGER undo_scope_repair")
        .unwrap();
    migrate(temp.path());
    assert_eq!(source_state(&catalog, Source::Terminal), (true, 0));
    assert_eq!(
        names(
            &catalog
                .page(&scope("conversation", "run", ""), "", 100, true)
                .unwrap()
        ),
        vec![string(&value, "request_id")]
    );
}
