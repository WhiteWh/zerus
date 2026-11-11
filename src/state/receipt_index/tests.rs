use super::*;

fn value(n: u128, conversation: &str, run: &str, agent: &str, at: f64) -> Value {
    json!({"request_id":uuid::Uuid::from_u128(n).to_string(),"name":"codex/fixture/receipt","run_id":run,"conversation_id":conversation,"agent_id":agent,
        "digest":format!("fixture-digest-{n}"),"status":"submitted","first_message":false,"submitted_at":at,"text":"fixture","submitted_text":"fixture",
        "attachments":[{"name":"image.png","mime":"image/png","bytes":1,"reference":"[Image #1]","path":"/private/fixture/image.png"}]})
}
fn scope(conversation: &str, run: &str, agent: &str) -> Scope {
    Scope::new(
        &json!({"agent":"codex","conversation_id":conversation,"run_id":run}),
        agent,
    )
}
fn put(catalog: &Catalog, source: Source, value: &Value) -> PathBuf {
    let directory = catalog.state.join(source.directory());
    fs::create_dir_all(&directory).unwrap();
    let filename = format!("{}.json", string(value, "request_id"));
    let path = directory.join(&filename);
    fs::write(&path, value.to_string()).unwrap();
    catalog.remember(source, &path, value).unwrap();
    path
}
fn names(page: &Page) -> Vec<String> {
    page.locators.iter().map(|l| l.id.clone()).collect()
}

#[test]
fn twelve_thousand_foreign_receipts_do_not_fill_the_target_window() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let transaction = catalog.db.unchecked_transaction().unwrap();
    for n in 1..=12_000 {
        let record = value(
            n,
            if n <= 2 { "wanted" } else { "foreign" },
            "old-run",
            "",
            n as f64,
        );
        if n <= 2 {
            put(&catalog, Source::Terminal, &record);
        } else {
            catalog
                .remember(
                    Source::Terminal,
                    &catalog
                        .state
                        .join(Source::Terminal.directory())
                        .join(format!("{}.json", record["request_id"].as_str().unwrap())),
                    &record,
                )
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    let page = catalog
        .page(&scope("wanted", "new-run", ""), "", 100, true)
        .unwrap();
    assert_eq!(
        names(&page),
        vec![
            uuid::Uuid::from_u128(2).to_string(),
            uuid::Uuid::from_u128(1).to_string()
        ]
    );
    assert_eq!(page.metadata["examined"], 2);
    assert_eq!(page.metadata["absence_confirmed"], false);
    for locator in &page.locators {
        assert_eq!(
            catalog.original(locator).unwrap()["conversation_id"],
            "wanted"
        );
    }
    assert!(catalog
        .page(&scope("other", "new-run", ""), "", 100, true)
        .unwrap()
        .locators
        .is_empty());
    assert!(catalog
        .page(&scope("wanted", "new-run", "other-child"), "", 100, true)
        .unwrap()
        .locators
        .is_empty());
}

#[test]
fn pages_keep_the_original_snapshot_while_new_receipts_arrive() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=10 {
        put(
            &catalog,
            Source::Terminal,
            &value(n, "wanted", "run", "", 1000.0 + n as f64),
        );
    }
    let scope = scope("wanted", "run", "");
    let first = catalog.page(&scope, "", 3, true).unwrap();
    let mut seen = names(&first);
    // These backdated inserts occupy a whole bounded scan, but cannot enter
    // the frozen snapshot or keep its cursor from making progress.
    let tx = catalog.db.unchecked_transaction().unwrap();
    for n in 11..=410 {
        let value = value(n, "wanted", "run", "", 1007.5);
        catalog
            .remember(
                Source::Terminal,
                &catalog
                    .state
                    .join(Source::Terminal.directory())
                    .join(format!("{}.json", value["request_id"].as_str().unwrap())),
                &value,
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let mut cursor = first.metadata["next_cursor"].as_str().unwrap().to_owned();
    let mut finished = false;
    for _ in 0..20 {
        let page = catalog.page(&scope, &cursor, 3, true).unwrap();
        assert!(page.metadata["examined"].as_u64().unwrap() <= EXAMINE as u64);
        seen.extend(names(&page));
        if page.metadata["has_more"] != true {
            finished = true;
            break;
        }
        let next = page.metadata["next_cursor"].as_str().unwrap().to_owned();
        assert_ne!(cursor, next);
        cursor = next;
    }
    assert!(finished);
    assert_eq!(
        seen,
        (1..=10)
            .rev()
            .map(|n| uuid::Uuid::from_u128(n).to_string())
            .collect::<Vec<_>>()
    );
    assert!(catalog
        .page(&self::scope("foreign", "run", ""), &cursor, 3, true)
        .is_err());
    assert!(catalog.page(&scope, "invalid cursor", 3, true).is_err());
    assert!(catalog.page(&scope, "", 0, true).is_err());
    assert!(catalog.page(&scope, "", 101, true).is_err());
}

#[test]
fn namespaces_children_and_initial_runs_stay_separate() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for source in Source::ALL {
        put(&catalog, source, &value(1, "wanted", "run", "", 1.0));
    }
    put(
        &catalog,
        Source::Terminal,
        &value(2, "wanted", "run", "child", 2.0),
    );
    let mut initial = value(3, "", "initial-run", "", 3.0);
    initial["first_message"] = json!(true);
    put(&catalog, Source::Terminal, &initial);
    assert_eq!(
        catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap()
            .locators
            .len(),
        1
    );
    assert_eq!(
        catalog
            .page(&scope("wanted", "run", "child"), "", 100, true)
            .unwrap()
            .locators
            .len(),
        1
    );
    assert_eq!(
        catalog
            .page(&scope("new-conversation", "initial-run", ""), "", 100, true)
            .unwrap()
            .locators
            .len(),
        1
    );
    assert!(catalog
        .page(&scope("new-conversation", "other-run", ""), "", 100, true)
        .unwrap()
        .locators
        .is_empty());
    initial["resolved_conversation_id"] = json!("new-conversation");
    put(&catalog, Source::Terminal, &initial);
    assert_eq!(
        catalog
            .page(&scope("new-conversation", "resumed-run", ""), "", 100, true)
            .unwrap()
            .locators
            .len(),
        1
    );
    assert!(catalog
        .page(
            &scope("wrong-conversation", "initial-run", ""),
            "",
            100,
            true
        )
        .unwrap()
        .locators
        .is_empty());
}

#[test]
fn missing_corrupt_and_rebound_originals_are_explicit_errors() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let original = value(1, "wanted", "run", "", 1.0);
    let path = put(&catalog, Source::Terminal, &original);
    let locator = catalog
        .page(&scope("wanted", "run", ""), "", 1, true)
        .unwrap()
        .locators
        .remove(0);
    fs::remove_file(&path).unwrap();
    assert_eq!(catalog.original(&locator).unwrap_err(), "missing");
    fs::write(&path, b"not json").unwrap();
    assert_eq!(catalog.original(&locator).unwrap_err(), "corrupt");
    let mut foreign = original.clone();
    foreign["conversation_id"] = json!("foreign");
    foreign["digest"] = json!("changed");
    fs::write(&path, foreign.to_string()).unwrap();
    catalog.remember(Source::Terminal, &path, &foreign).unwrap();
    assert_eq!(catalog.original(&locator).unwrap_err(), "binding_changed");
    assert!(catalog
        .page(&scope("foreign", "run", ""), "", 100, true)
        .unwrap()
        .locators
        .is_empty());
    let current = catalog
        .page(&scope("wanted", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(current.locators[0].sequence, locator.sequence);
    assert_eq!(current.metadata["unattributed_errors"], 1);
    fs::write(&path, vec![b'x'; ORIGINAL_BYTES as usize + 1]).unwrap();
    assert_eq!(catalog.original(&locator).unwrap_err(), "oversized");
}

#[test]
fn bounded_migration_resumes_without_rewriting_history_or_sequences() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join(Source::Terminal.directory());
    fs::create_dir(&directory).unwrap();
    let mut originals = Vec::new();
    for n in 1..=73 {
        let value = value(n, "wanted", "run", "", n as f64);
        let path = directory.join(format!("{}.json", value["request_id"].as_str().unwrap()));
        let bytes = value.to_string().into_bytes();
        fs::write(&path, &bytes).unwrap();
        originals.push((path, bytes));
    }
    let corrupt = directory.join("corrupt.json");
    fs::write(&corrupt, b"{").unwrap();
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    assert!(!migration.step().unwrap());
    let catalog = Catalog::open(temp.path(), false).unwrap();
    let before: Vec<(String, i64)> = catalog
        .db
        .prepare("SELECT request,seq FROM receipt_rows ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(!before.is_empty() && before.len() <= 64);
    drop(migration);
    let writer = Catalog::open(temp.path(), true).unwrap();
    writer
        .error(Source::Terminal, "@enumeration", "enumeration_incomplete")
        .unwrap();
    drop(writer);
    let mut resumed = migration::Migration::new(temp.path()).unwrap();
    let mut done = false;
    for _ in 0..1000 {
        if resumed.step().unwrap() {
            done = true;
            break;
        }
    }
    assert!(done);
    drop(resumed);
    for (id, sequence) in before {
        assert_eq!(
            catalog
                .db
                .query_row("SELECT seq FROM receipt_rows WHERE request=?", [id], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            sequence
        );
    }
    let page = catalog
        .page(&scope("wanted", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(page.locators.len(), 73);
    assert_eq!(page.metadata["indexed"], 73);
    assert_eq!(page.metadata["state"], "partial");
    assert_eq!(page.metadata["unattributed_errors"], 1);
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(corrupt).unwrap(), b"{");
}

#[test]
fn unavailable_index_does_not_change_authoritative_receipt_persistence() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(DATABASE), b"not sqlite").unwrap();
    let value = value(1, "wanted", "run", "", 1.0);
    let path = temp
        .path()
        .join("input_receipts")
        .join(format!("{}.json", value["request_id"].as_str().unwrap()));
    assert!(before_commit(&path).is_none());
    assert!(!path.parent().unwrap().exists(), "proof capture must never create a receipt namespace");
    persist(&path, &value).unwrap();
    assert_eq!(read_original(&path).unwrap(), value);
    if let Ok(catalog) = Catalog::open(temp.path(), false) {
        assert!(catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .is_err());
    }
}


#[test]
fn committed_registration_never_rewrites_originals_and_refreshes_file_stamps() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut receipt = value(1, "wanted", "run", "", 1.0);
    receipt["status"] = json!("in_progress");
    let path = put(&catalog, Source::Terminal, &receipt);
    for status in ["in_progress", "submitted"] {
        let stamp = migration::stamp(path.parent().unwrap()).unwrap();
        catalog
            .db
            .execute(
                "UPDATE receipt_sources SET complete=1,stamp=? WHERE source='terminal'",
                [&stamp],
            )
            .unwrap();
        receipt["status"] = json!(status);
        let raw = format!("\n  {}  \n", receipt);
        // Updating an existing original in place need not change its directory.
        fs::write(&path, &raw).unwrap();
        assert_eq!(migration::stamp(path.parent().unwrap()).unwrap(), stamp);
        let original = crate::platform::FileIdentity::of(&fs::File::open(&path).unwrap()).unwrap();
        assert!(remember_committed(&path, &receipt, None));
        assert_eq!(
            crate::platform::FileIdentity::of(&fs::File::open(&path).unwrap()).unwrap(),
            original
        );
        assert_eq!(fs::read(&path).unwrap(), raw.as_bytes());
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(page.locators.len(), 1);
        // Without a pre-commit proof, do not inherit completeness even if a
        // coarse directory clock reports the same stamp after a file change.
        assert_eq!(page.metadata["migration_complete"], false);
        assert_eq!(catalog.original(&page.locators[0]).unwrap(), receipt);
        let stamp: String = catalog
            .db
            .query_row(
                "SELECT stamp FROM receipt_rows WHERE source='terminal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamp, file_stamp(&path).unwrap());
        assert!(catalog.coverage_verified());
    }
}

#[cfg(unix)]
#[test]
fn committed_registration_advances_only_its_exact_directory_delta() {
    for existing in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let directory = temp.path().join("input_receipts");
        fs::create_dir_all(&directory).unwrap();
        let mut receipt = value(1, "wanted", "run", "", 1.0);
        let path = directory.join(format!("{}.json", string(&receipt, "request_id")));
        put(
            &catalog,
            Source::Terminal,
            &value(2, "wanted", "run", "", 2.0),
        );
        if existing {
            put(&catalog, Source::Terminal, &receipt);
        }
        let mut migration = migration::Migration::new(temp.path()).unwrap();
        while !migration.step().unwrap() {}
        let epoch = catalog.change_epoch().unwrap();
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 1, true)
            .unwrap();
        assert_eq!(page.metadata["migration_complete"], true);
        let before = before_commit(&path).unwrap();
        receipt["submitted_at"] = json!(3.0);
        let raw = format!("\n {} \n", receipt);
        atomic(&path, &raw).unwrap();
        let original = crate::platform::FileIdentity::of(&fs::File::open(&path).unwrap()).unwrap();
        remember_committed(&path, &receipt, Some(&before));
        assert_eq!(fs::read(&path).unwrap(), raw.as_bytes());
        assert_eq!(
            crate::platform::FileIdentity::of(&fs::File::open(&path).unwrap()).unwrap(),
            original
        );
        assert!(catalog.coverage_verified());
        let next = catalog.change_epoch().unwrap();
        assert!(next.2 > epoch.2);
        if existing {
            // Updating ordering metadata retains batch 5's epoch invalidation.
            assert_ne!(next.0, epoch.0);
            assert!(catalog
                .page(
                    &scope("wanted", "run", ""),
                    page.metadata["next_cursor"].as_str().unwrap(),
                    1,
                    true
                )
                .is_err());
        }
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(page.metadata["migration_complete"], true);
        assert_eq!(page.metadata["catalog_complete_at_snapshot"], true);
        assert_eq!(page.locators.len(), 2);
        assert_eq!(page.locators[0].id, string(&receipt, "request_id"));
        let changed = catalog
            .changed_page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(changed.coverage, Coverage::Complete);
        assert!(changed
            .rows
            .iter()
            .any(|row| row.locator.id == string(&receipt, "request_id")
                && row.stamp == file_stamp(&path).unwrap()));
    }
}

#[cfg(unix)]
#[test]
fn concurrent_unregistered_original_prevents_complete_stamp_advance() {
    for existing in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let directory = temp.path().join("input_receipts");
        fs::create_dir_all(&directory).unwrap();
        let mut receipt = value(1, "wanted", "run", "", 1.0);
        let path = directory.join(format!("{}.json", string(&receipt, "request_id")));
        if existing {
            put(&catalog, Source::Terminal, &receipt);
        }
        let mut migration = migration::Migration::new(temp.path()).unwrap();
        while !migration.step().unwrap() {}
        let before = before_commit(&path).unwrap();
        // An older binary publishes this original after S0 without registering
        // it. Our own successful row must never silently cover that filename.
        let foreign = value(2, "wanted", "run", "", 2.0);
        let foreign_path = directory.join(format!("{}.json", string(&foreign, "request_id")));
        atomic(&foreign_path, &foreign.to_string()).unwrap();
        receipt["submitted_at"] = json!(3.0);
        atomic(&path, &receipt.to_string()).unwrap();
        remember_committed(&path, &receipt, Some(&before));
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(page.locators.len(), 1);
        assert_eq!(page.metadata["migration_complete"], false);
        assert_eq!(page.metadata["catalog_complete_at_snapshot"], false);
        let mut migration = migration::Migration::new(temp.path()).unwrap();
        while !migration.step().unwrap() {}
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(page.locators.len(), 2);
        assert_eq!(page.metadata["migration_complete"], true);
        assert_eq!(
            fs::read_to_string(foreign_path).unwrap(),
            foreign.to_string()
        );
    }
}

#[cfg(unix)]
#[test]
fn replacing_a_foreign_filename_cannot_inherit_complete_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let receipt = value(1, "wanted", "run", "", 1.0);
    let path = put(&catalog, Source::Terminal, &receipt);
    let old = put(
        &catalog,
        Source::Terminal,
        &value(2, "wanted", "run", "", 2.0),
    );
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    while !migration.step().unwrap() {}
    let mut before = before_commit(&path).unwrap();
    let foreign = value(3, "wanted", "run", "", 3.0);
    fs::remove_file(old).unwrap();
    atomic(
        &path.with_file_name(format!("{}.json", string(&foreign, "request_id"))),
        &foreign.to_string(),
    )
    .unwrap();
    atomic(&path, &receipt.to_string()).unwrap();
    let after = receipt_entries(&path).unwrap();
    assert_eq!(before.entries.receipts, after.receipts);
    assert_eq!(before.entries.others, after.others);
    assert_ne!(before.entries.fingerprint, after.fingerprint);
    // Model a coarse stamp collision while keeping the exact S0 entry set.
    // Leaving the old complete flag untouched is insufficient in this case.
    before.entries.stamp = after.stamp.clone();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=? WHERE source='terminal'",
            [&after.stamp],
        )
        .unwrap();
    remember_committed(&path, &receipt, Some(&before));
    let page = catalog
        .page(&scope("wanted", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(page.metadata["migration_complete"], false);
    assert_eq!(page.metadata["catalog_complete_at_snapshot"], false);
    assert!(!page
        .locators
        .iter()
        .any(|row| row.id == string(&foreign, "request_id")));
}

#[test]
fn failed_registration_of_new_and_final_originals_retains_incomplete_coverage() {
    for existing in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(temp.path(), true).unwrap();
        let mut receipt = value(1, "wanted", "run", "", 1.0);
        let directory = temp.path().join("input_receipts");
        fs::create_dir_all(&directory).unwrap();
        let filename = format!("{}.json", string(&receipt, "request_id"));
        let path = directory.join(&filename);
        if existing {
            receipt["status"] = json!("in_progress");
            put(&catalog, Source::Terminal, &receipt);
            catalog
                .error(Source::Terminal, &filename, "injected")
                .unwrap();
            catalog.db.execute_batch("CREATE TRIGGER refuse_registration BEFORE DELETE ON receipt_errors BEGIN SELECT RAISE(ABORT,'injected final registration failure'); END;").unwrap();
        } else {
            catalog.db.execute_batch("CREATE TRIGGER refuse_registration BEFORE INSERT ON receipt_rows BEGIN SELECT RAISE(ABORT,'injected new registration failure'); END;").unwrap();
        }
        receipt["status"] = json!("submitted");
        let raw = format!("\n{}\n", receipt);
        fs::write(&path, &raw).unwrap();
        let stamp = migration::stamp(&directory).unwrap();
        catalog
            .db
            .execute(
                "UPDATE receipt_sources SET complete=1,stamp=? WHERE source='terminal'",
                [&stamp],
            )
            .unwrap();
        let epoch = catalog.change_epoch().unwrap();
        assert!(!remember_committed(&path, &receipt, None));
        // A failed row write must not publish a sequence allocation by itself.
        assert_eq!(catalog.change_epoch().unwrap().2, epoch.2);
        assert_eq!(fs::read(&path).unwrap(), raw.as_bytes());
        let page = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(page.metadata["migration_complete"], false);
        assert_eq!(page.locators.len(), usize::from(existing));
        catalog
            .db
            .execute_batch("DROP TRIGGER refuse_registration")
            .unwrap();
        let mut migration = migration::Migration::new(temp.path()).unwrap();
        while !migration.step().unwrap() {}
        let repaired = catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap();
        assert_eq!(repaired.locators.len(), 1);
        assert_eq!(repaired.metadata["migration_complete"], true);
        assert_eq!(repaired.metadata["unattributed_errors"], 0);
        assert_eq!(fs::read(path).unwrap(), raw.as_bytes());
    }
}

#[test]
fn a_lost_row_invalidates_cursors_and_is_rediscovered_without_original_changes() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let one = put(
        &catalog,
        Source::Terminal,
        &value(1, "wanted", "run", "", 1.0),
    );
    put(
        &catalog,
        Source::Terminal,
        &value(2, "wanted", "run", "", 2.0),
    );
    let original = fs::read(&one).unwrap();
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    let scope = scope("wanted", "run", "");
    let first = catalog.page(&scope, "", 1, true).unwrap();
    let cursor = first.metadata["next_cursor"].as_str().unwrap();
    catalog
        .db
        .execute(
            "DELETE FROM receipt_rows WHERE request=?",
            [uuid::Uuid::from_u128(1).to_string()],
        )
        .unwrap();
    assert!(catalog.page(&scope, cursor, 1, true).is_err());
    let missing = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(missing.metadata["migration_complete"], false);
    assert_eq!(missing.metadata["unattributed_errors"], 1);
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    while !migration.step().unwrap() {}
    let restored = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(restored.locators.len(), 2);
    assert_eq!(restored.metadata["state"], "indexed");
    assert_eq!(restored.metadata["indexed"], 2);
    assert_eq!(restored.metadata["unattributed_errors"], 0);
    assert_eq!(fs::read(one).unwrap(), original);
    assert!(catalog.page(&scope, cursor, 1, true).is_err());
}

fn change_count(catalog: &Catalog) -> i64 {
    catalog
        .db
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn changed_pages_advance_a_bounded_scope_without_foreign_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 10.0));
    put(&catalog, Source::Terminal, &value(2, "wanted", "run", "", 11.0));
    let transaction = catalog.db.unchecked_transaction().unwrap();
    for n in 3..=40 {
        let record = value(n, "foreign", "run", "", n as f64);
        catalog
            .remember(
                Source::Terminal,
                &catalog
                    .state
                    .join(Source::Terminal.directory())
                    .join(format!("{}.json", record["request_id"].as_str().unwrap())),
                &record,
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let scope = scope("wanted", "run", "");
    let first = catalog.changed_page(&scope, "", 1, true).unwrap();
    assert_eq!(first.rows.len(), 1);
    assert_eq!(
        first.rows[0].locator.id,
        uuid::Uuid::from_u128(1).to_string()
    );
    assert!(!first.drained);
    assert_eq!(first.rows[0].change, 1);
    let second = catalog.changed_page(&scope, &first.cursor, 10, true).unwrap();
    assert_eq!(second.rows.len(), 1);
    assert_eq!(
        second.rows[0].locator.id,
        uuid::Uuid::from_u128(2).to_string()
    );
    assert!(second.drained);
    let third = catalog.changed_page(&scope, &second.cursor, 10, true).unwrap();
    assert!(third.rows.is_empty());
    assert!(third.drained);
}

#[test]
fn an_update_beyond_the_frozen_upper_waits_for_the_next_chain() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 10.0));
    let second = value(2, "wanted", "run", "", 11.0);
    let path = put(&catalog, Source::Terminal, &second);
    assert_eq!(change_count(&catalog), 2);
    let scope = scope("wanted", "run", "");
    let first = catalog.changed_page(&scope, "", 1, true).unwrap();
    assert_eq!(first.rows.len(), 1);
    // The second original changes while the first chain is live. The binding
    // is immutable; the content stamp and change move beyond the frozen upper.
    let mut updated = second.clone();
    updated["status"] = json!("completed");
    fs::write(&path, updated.to_string()).unwrap();
    catalog.remember(Source::Terminal, &path, &updated).unwrap();
    assert_eq!(change_count(&catalog), 3);
    let frozen = catalog.changed_page(&scope, &first.cursor, 10, true).unwrap();
    assert!(
        frozen
            .rows
            .iter()
            .all(|row| row.locator.id != uuid::Uuid::from_u128(2).to_string()),
        "a status update leaked into the frozen chain"
    );
    assert!(frozen.drained);
    let next = catalog.changed_page(&scope, &frozen.cursor, 10, true).unwrap();
    assert_eq!(next.rows.len(), 1);
    assert_eq!(
        next.rows[0].locator.id,
        uuid::Uuid::from_u128(2).to_string()
    );
    assert_eq!(next.rows[0].change, 3);
    assert_eq!(
        catalog.original(&next.rows[0].locator).unwrap()["status"],
        "completed"
    );
}

#[test]
fn change_cursors_are_bound_to_the_index_epoch_and_scope() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 10.0));
    let page = catalog
        .changed_page(&scope("wanted", "run", ""), "", 10, true)
        .unwrap();
    assert_eq!(
        catalog
            .changed_page(&scope("other", "run", ""), &page.cursor, 10, true)
            .unwrap_err(),
        CURSOR_REJECTED
    );
    catalog
        .db
        .execute("UPDATE receipt_meta SET value='other' WHERE key='epoch'", [])
        .unwrap();
    assert_eq!(
        catalog
            .changed_page(&scope("wanted", "run", ""), &page.cursor, 10, true)
            .unwrap_err(),
        CURSOR_REJECTED
    );
}

#[test]
fn twelve_thousand_foreign_receipts_stay_out_of_a_changed_scope() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let transaction = catalog.db.unchecked_transaction().unwrap();
    for n in 1..=12_000 {
        let record = value(
            n,
            if n <= 2 { "wanted" } else { "foreign" },
            "run",
            "",
            n as f64,
        );
        catalog
            .remember(
                Source::Terminal,
                &catalog
                    .state
                    .join(Source::Terminal.directory())
                    .join(format!("{}.json", record["request_id"].as_str().unwrap())),
                &record,
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let page = catalog
        .changed_page(&scope("wanted", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.locator.id.clone())
            .collect::<Vec<_>>(),
        vec![
            uuid::Uuid::from_u128(1).to_string(),
            uuid::Uuid::from_u128(2).to_string()
        ]
    );
    assert!(page.drained);
}

#[test]
fn a_v1_database_migrates_before_change_indexes_are_created() {
    let temp = tempfile::tempdir().unwrap();
    // The exact #2055 schema: no stamp/change columns and no change indexes.
    let path = temp.path().join("delivery-receipts-v1.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE receipt_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE receipt_sources(source TEXT PRIMARY KEY,stamp TEXT NOT NULL DEFAULT '',complete INTEGER NOT NULL DEFAULT 0,indexed INTEGER NOT NULL DEFAULT 0,errors INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE receipt_rows(seq INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,request TEXT NOT NULL,name TEXT NOT NULL,run TEXT NOT NULL,conversation TEXT NOT NULL,agent TEXT NOT NULL,first_message INTEGER NOT NULL,at REAL NOT NULL,files INTEGER NOT NULL,binding TEXT NOT NULL,UNIQUE(source,request));
         CREATE INDEX receipt_exact ON receipt_rows(source,name,run,conversation,at DESC,request DESC);
         CREATE INDEX receipt_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC);
         CREATE INDEX receipt_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC);
         CREATE INDEX receipt_file_scope ON receipt_rows(source,agent,conversation,at DESC,request DESC) WHERE files=1;
         CREATE INDEX receipt_file_initial ON receipt_rows(source,agent,run,conversation,first_message,at DESC,request DESC) WHERE files=1;
         CREATE TABLE receipt_errors(source TEXT NOT NULL,name TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(source,name));
         CREATE TRIGGER receipt_row_count AFTER INSERT ON receipt_rows BEGIN UPDATE receipt_sources SET indexed=indexed+1 WHERE source=NEW.source; END;
         CREATE TRIGGER receipt_error_count AFTER INSERT ON receipt_errors BEGIN UPDATE receipt_sources SET errors=errors+1 WHERE source=NEW.source; END;
         CREATE TRIGGER receipt_error_clear AFTER DELETE ON receipt_errors BEGIN UPDATE receipt_sources SET errors=MAX(0,errors-1) WHERE source=OLD.source; END;
         INSERT INTO receipt_meta VALUES('epoch','v1-epoch'),('cursor_secret','v1-secret');
         INSERT INTO receipt_sources(source,complete,indexed) VALUES('terminal',1,1);
         INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding)
             VALUES('terminal','existing-request','name','run','conversation','',0,1.0,1,'existing-binding');",
    )
    .unwrap();
    drop(db);
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let columns: Vec<String> = catalog
        .db
        .prepare("SELECT name FROM pragma_table_info('receipt_rows')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    for column in ["stamp", "change"] {
        assert!(columns.iter().any(|value| value == column), "{column} was not added");
    }
    let change: i64 = catalog
        .db
        .query_row("SELECT change FROM receipt_rows WHERE request='existing-request'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(change, 1);
    let sequence: String = catalog
        .db
        .query_row("SELECT value FROM receipt_meta WHERE key='change_seq'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sequence, "1");
    let indexes: i64 = catalog
        .db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('receipt_change','receipt_change_scope','receipt_change_initial')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 3);
    // Reopening the migrated database is idempotent.
    let again = Catalog::open(temp.path(), true).unwrap();
    let change: i64 = again
        .db
        .query_row("SELECT change FROM receipt_rows WHERE request='existing-request'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(change, 1);
}

#[test]
fn a_reader_never_sees_a_sequence_before_its_row_commits() {
    let temp = tempfile::tempdir().unwrap();
    let writer = Catalog::open(temp.path(), true).unwrap();
    let reader = Catalog::open(temp.path(), true).unwrap();
    let other = Catalog::open(temp.path(), true).unwrap();
    other.db.busy_timeout(Duration::ZERO).unwrap();
    let scope = scope("wanted", "run", "");
    let first = value(1, "wanted", "run", "", 10.0);
    let directory = temp.path().join(Source::Terminal.directory());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{}.json", first["request_id"].as_str().unwrap()));
    fs::write(&path, first.to_string()).unwrap();
    let transaction = writer.db.unchecked_transaction().unwrap();
    writer.remember(Source::Terminal, &path, &first).unwrap();
    let during = reader.changed_page(&scope, "", 10, true).unwrap();
    assert!(during.rows.is_empty(), "an uncommitted change was published");
    // A second writer cannot publish a higher sequence over the held one.
    let second = value(2, "wanted", "run", "", 11.0);
    let second_path = directory.join(format!("{}.json", second["request_id"].as_str().unwrap()));
    fs::write(&second_path, second.to_string()).unwrap();
    assert!(
        other.remember(Source::Terminal, &second_path, &second).is_err(),
        "a higher sequence committed over an uncommitted lower one"
    );
    let still = reader.changed_page(&scope, "", 10, true).unwrap();
    assert!(still.rows.is_empty());
    transaction.commit().unwrap();
    let after = reader.changed_page(&scope, "", 10, true).unwrap();
    assert_eq!(after.rows.len(), 1);
    assert_eq!(
        after.rows[0].locator.id,
        uuid::Uuid::from_u128(1).to_string()
    );
}

#[test]
fn a_guarded_cache_with_a_cut_change_upgrade_recovers_its_frontier() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 1.0));
    put(&catalog, Source::Terminal, &value(2, "wanted", "run", "", 2.0));
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    // The persisted cut after ADD COLUMN change but before its seed: the
    // guarded cache has the column, unseeded rows, no change indexes and no
    // published frontier.
    catalog
        .db
        .execute_batch(
            "DROP INDEX receipt_change;
             DROP INDEX receipt_change_scope;
             DROP INDEX receipt_change_initial;
             UPDATE receipt_rows SET change=0;
             DELETE FROM receipt_meta WHERE key='change_seq';",
        )
        .unwrap();
    drop(catalog);
    let recovered = Catalog::open(temp.path(), true).unwrap();
    let rows: Vec<i64> = recovered
        .db
        .prepare("SELECT change FROM receipt_rows ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec![1, 2]);
    let frontier: String = recovered
        .db
        .query_row("SELECT value FROM receipt_meta WHERE key='change_seq'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(frontier, "2");
    let indexes: i64 = recovered
        .db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN ('receipt_change','receipt_change_scope','receipt_change_initial')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 3);
    let scope = scope("wanted", "run", "");
    assert_eq!(
        recovered.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
    let changed = recovered.changed_page(&scope, "", 100, true).unwrap();
    assert_eq!(changed.rows.len(), 2);
    assert!(changed.drained);
}

#[test]
fn a_first_prompt_pin_keeps_coverage_while_a_real_rebind_invalidates() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let record = value(1, "", "run", "", 1.0);
    put(&catalog, Source::Terminal, &record);
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    let coverage = || -> (i64, i64, i64, String) {
        let (complete, indexed, errors): (i64, i64, i64) = catalog
            .db
            .query_row(
                "SELECT complete,indexed,errors FROM receipt_sources WHERE source='terminal'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let epoch: String = catalog
            .db
            .query_row("SELECT value FROM receipt_meta WHERE key='epoch'", [], |row| {
                row.get(0)
            })
            .unwrap();
        (complete, indexed, errors, epoch)
    };
    let before = coverage();
    assert_eq!(before.0, 1);
    assert_eq!(before.1, 1);
    assert_eq!(before.2, 0);
    // The reviewer's verbatim-SQL probe: the legitimate one-way first-prompt
    // pin changes only the empty conversation. It must keep coverage and epoch.
    catalog
        .db
        .execute(
            "UPDATE receipt_rows SET conversation='resolved' WHERE source='terminal' AND request=? AND conversation=''",
            [uuid::Uuid::from_u128(1).to_string()],
        )
        .unwrap();
    assert_eq!(coverage(), before);
    // Any other identity rebind still invalidates coverage and rotates the
    // epoch, so old cursors cannot pass the missing row.
    catalog
        .db
        .execute(
            "UPDATE receipt_rows SET run='other-run' WHERE source='terminal' AND request=?",
            [uuid::Uuid::from_u128(1).to_string()],
        )
        .unwrap();
    let after = coverage();
    assert_eq!(after.0, 0);
    assert!(after.2 >= 1);
    assert_ne!(after.3, before.3);
}

#[test]
fn a_lost_witness_or_stale_verification_turns_coverage_incomplete_and_the_walk_repairs_it() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 1.0));
    put(&catalog, Source::Terminal, &value(2, "wanted", "run", "", 2.0));
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    let scope = scope("wanted", "run", "");
    assert_eq!(
        catalog.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
    // A row lost without its DELETE trigger leaves the trigger-maintained
    // witness ahead of the rows. A pinned read must not claim completion.
    catalog
        .db
        .execute("UPDATE receipt_sources SET indexed=5 WHERE source='terminal'", [])
        .unwrap();
    let page = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(page.metadata["migration_complete"], false);
    // The bounded verification requests the worker; its forced walk repairs
    // the witness (and any lost row) and refreshes the verification age.
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    while !migration.step().unwrap() {}
    let repaired = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(repaired.metadata["migration_complete"], true);
    assert_eq!(repaired.metadata["indexed"], 2);
    // Age alone is not evidence: a stale-but-consistent locator stays
    // complete and only schedules the re-verification walk.
    catalog
        .db
        .execute("UPDATE receipt_meta SET value='0' WHERE key='verified_at'", [])
        .unwrap();
    assert!(catalog.verification_due());
    assert_eq!(
        catalog.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    while !migration.step().unwrap() {}
    assert!(!catalog.verification_due());
    assert_eq!(
        catalog.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
}

#[test]
fn a_stale_but_consistent_verification_stays_complete() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 1.0));
    put(&catalog, Source::Terminal, &value(2, "wanted", "run", "", 2.0));
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    catalog
        .db
        .execute("UPDATE receipt_meta SET value='0' WHERE key='verified_at'", [])
        .unwrap();
    let scope = scope("wanted", "run", "");
    assert!(catalog.verification_due());
    let page = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(page.metadata["migration_complete"], true);
    assert_eq!(page.metadata["state"], "indexed");
    let changed = catalog.changed_page(&scope, "", 100, true).unwrap();
    assert!(changed.drained);
    assert_eq!(changed.rows.len(), 2);
    // The background walk only refreshes the verification age here.
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    while !migration.step().unwrap() {}
    assert!(!catalog.verification_due());
    assert_eq!(
        catalog.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
}

#[test]
fn unseeded_rows_or_a_behind_frontier_are_not_complete_and_repair_restarts_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "wanted", "run", "", 1.0));
    put(&catalog, Source::Terminal, &value(2, "wanted", "run", "", 2.0));
    let stamp = migration::stamp(&temp.path().join(Source::Terminal.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='terminal'",
            [stamp],
        )
        .unwrap();
    let scope = scope("wanted", "run", "");
    let epoch = |catalog: &Catalog| -> String {
        catalog
            .db
            .query_row("SELECT value FROM receipt_meta WHERE key='epoch'", [], |row| row.get(0))
            .unwrap()
    };
    let initial_epoch = epoch(&catalog);
    // F2: the change indexes exist, but the rows are unseeded and the
    // frontier is 0. A Complete reader would skip both originals.
    catalog
        .db
        .execute_batch(
            "UPDATE receipt_rows SET change=0;
             UPDATE receipt_meta SET value='0' WHERE key='change_seq';",
        )
        .unwrap();
    assert!(!catalog.coverage_verified());
    let broken = catalog.page(&scope, "", 1, true).unwrap();
    assert_eq!(broken.metadata["migration_complete"], false);
    let stale_cursor = broken.metadata["next_cursor"].as_str().unwrap().to_owned();
    drop(catalog);
    let repaired = Catalog::open(temp.path(), true).unwrap();
    let rows: Vec<i64> = repaired
        .db
        .prepare("SELECT change FROM receipt_rows ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec![1, 2]);
    let frontier: String = repaired
        .db
        .query_row("SELECT value FROM receipt_meta WHERE key='change_seq'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(frontier, "2");
    assert!(repaired.coverage_verified());
    assert_ne!(epoch(&repaired), initial_epoch);
    assert_eq!(
        repaired.page(&scope, "", 100, true).unwrap().metadata["migration_complete"],
        true
    );
    assert!(repaired.page(&scope, &stale_cursor, 100, true).is_err());
    // G: only the frontier is behind MAX(change); rows and indexes are intact.
    repaired
        .db
        .execute("UPDATE receipt_meta SET value='0' WHERE key='change_seq'", [])
        .unwrap();
    assert!(!repaired.coverage_verified());
    let broken = repaired.page(&scope, "", 1, true).unwrap();
    assert_eq!(broken.metadata["migration_complete"], false);
    let stale_cursor = broken.metadata["next_cursor"].as_str().unwrap().to_owned();
    let behind_epoch = epoch(&repaired);
    drop(repaired);
    let repaired = Catalog::open(temp.path(), true).unwrap();
    let frontier: String = repaired
        .db
        .query_row("SELECT value FROM receipt_meta WHERE key='change_seq'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(frontier, "2");
    assert!(repaired.coverage_verified());
    assert_ne!(epoch(&repaired), behind_epoch);
    assert!(repaired.page(&scope, &stale_cursor, 100, true).is_err());
    let changed = repaired.changed_page(&scope, "", 100, true).unwrap();
    assert_eq!(changed.rows.len(), 2);
    assert!(changed.drained);
}
