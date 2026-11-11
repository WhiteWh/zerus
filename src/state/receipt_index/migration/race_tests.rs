use super::*;

// Reproducer from the independent PR2101 review; private original fixtures only.
fn review_seed_originals() -> (tempfile::TempDir, Migration) {
    let (temp, mut migration) = seeded(0);
    let tx = migration.catalog.db.unchecked_transaction().unwrap();
    for n in 1..=256 {
        original(&migration.catalog, n);
    }
    tx.commit().unwrap();
    migration.sources = vec![Source::Terminal];
    (temp, migration)
}

fn review_count_started(migration: &mut Migration) {
    for _ in 0..100 {
        assert!(
            !migration.step().unwrap(),
            "count finished before review cut"
        );
        if migration
            .walk
            .as_ref()
            .is_some_and(|walk| walk.entries.is_none() && walk.after.is_some())
        {
            return;
        }
    }
    panic!("could not reach a budgeted count cut");
}

fn review_finish(migration: &mut Migration) {
    for _ in 0..1000 {
        if migration.step().unwrap() {
            return;
        }
    }
    panic!("walk did not finish");
}

#[test]
fn review_in_place_repair_and_a_second_walker_cannot_lose_the_trigger_count() {
    let (temp, mut first) = review_seed_originals();
    let directory = temp.path().join(Source::Terminal.directory());
    let repaired = directory.join(format!("{}.json", uuid::Uuid::from_u128(0)));
    fs::write(&repaired, "{").unwrap();
    review_count_started(&mut first);
    let prior_stamp = stamp(&directory).unwrap();
    let prior_epoch: String = first
        .catalog
        .db
        .query_row(
            "SELECT value FROM receipt_meta WHERE key='epoch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Repair an existing unreadable original in place: no directory entry changes.
    let id = uuid::Uuid::from_u128(0).to_string();
    let value = json!({"request_id":id,"name":"codex/fixture/count","run_id":"run",
        "conversation_id":"conversation","digest":"fixture","status":"submitted","submitted_at":0.0});
    fs::write(&repaired, value.to_string()).unwrap();
    let original_before = (fs::read(&repaired).unwrap(), file_stamp(&repaired).unwrap());
    assert_eq!(stamp(&directory).unwrap(), prior_stamp);
    let mut second = Migration::with_sources(temp.path(), vec![Source::Terminal], true).unwrap();
    review_finish(&mut second);
    assert_eq!(
        state(&first).0,
        257,
        "insert trigger must count the repaired row"
    );
    assert_eq!(state(&second), (257, true));
    let epoch: String = first
        .catalog
        .db
        .query_row(
            "SELECT value FROM receipt_meta WHERE key='epoch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        epoch, prior_epoch,
        "the insert does not invalidate the captured epoch"
    );
    review_finish(&mut first);
    let actual: i64 = first
        .catalog
        .db
        .query_row(
            "SELECT COUNT(*) FROM receipt_rows WHERE source='terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let reported = state(&first);
    let verified = first.catalog.coverage_verified();
    let scope = Scope {
        source: Source::Terminal,
        name: Some("codex/fixture/count".into()),
        run: "run".into(),
        conversation: "conversation".into(),
        agent: "".into(),
    };
    let page = first.catalog.page(&scope, "", 10, false).unwrap();
    eprintln!(
        "REVIEW_INSERT_RACE indexed={} complete={} actual={} verified={} metadata={}",
        reported.0, reported.1, actual, verified, page.metadata
    );
    assert!(
        !verified || reported.0 == actual,
        "wrong count became verified coverage"
    );
    assert_eq!(page.metadata["migration_complete"], false);
    assert_eq!(page.metadata["catalog_complete_at_snapshot"], false);
    assert_eq!(
        reported.0, actual,
        "finishing overwrote the trigger-maintained count"
    );
    assert_eq!(reported, (257, false));
    let restarted = finish(temp.path());
    assert_eq!(state(&restarted), (257, true));
    assert!(restarted.catalog.coverage_verified());
    assert_eq!(
        (fs::read(&repaired).unwrap(), file_stamp(&repaired).unwrap()),
        original_before
    );
}

#[test]
fn two_stable_walkers_can_publish_the_same_count() {
    let (temp, mut first) = review_seed_originals();
    let mut second = Migration::with_sources(temp.path(), vec![Source::Terminal], true).unwrap();
    review_count_started(&mut first);
    review_count_started(&mut second);
    review_finish(&mut first);
    assert_eq!(state(&first), (256, true));
    review_finish(&mut second);
    assert_eq!(state(&second), (256, true));
    assert!(first.catalog.coverage_verified());
}

#[test]
fn an_insert_in_another_source_does_not_invalidate_this_walk() {
    let (temp, mut worker) = review_seed_originals();
    review_count_started(&mut worker);
    let directory = temp.path().join(Source::Question.directory());
    fs::create_dir_all(&directory).unwrap();
    let id = uuid::Uuid::from_u128(999).to_string();
    let path = directory.join(format!("{id}.json"));
    let value = json!({"request_id":id,"name":"codex/fixture/count","run_id":"run",
        "conversation_id":"conversation","question_id":"question","question_hash":"hash",
        "digest":"fixture","status":"submitted","submitted_at":0.0});
    fs::write(&path, value.to_string()).unwrap();
    worker
        .catalog
        .remember(Source::Question, &path, &value)
        .unwrap();
    review_finish(&mut worker);
    assert_eq!(state(&worker), (256, true));
    assert!(worker.catalog.coverage_verified());
}
