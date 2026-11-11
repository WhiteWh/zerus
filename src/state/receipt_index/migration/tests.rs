use super::*;

fn seeded(count: u128) -> (tempfile::TempDir, Migration) {
    let temp = tempfile::tempdir().unwrap();
    let migration = Migration::new(temp.path()).unwrap();
    let transaction = migration.catalog.db.unchecked_transaction().unwrap();
    for n in 1..=count {
        migration.catalog.db.execute(
            "INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding,stamp,change)
             VALUES('terminal',?1,'codex/fixture/count','run','conversation','',0,?2,0,'fixture','fixture',?2)",
            params![uuid::Uuid::from_u128(n).to_string(), n as i64],
        ).unwrap();
    }
    transaction.commit().unwrap();
    (temp, migration)
}

fn finishing(migration: &mut Migration, count: u128) {
    let directory = migration.catalog.state.join(Source::Terminal.directory());
    fs::create_dir_all(&directory).unwrap();
    migration.next_source = migration.sources.len();
    migration.walk = Some(Walk {
        source: Source::Terminal,
        entries: None,
        started: stamp(&directory).unwrap(),
        covered: false,
        after: Some(uuid::Uuid::from_u128(count).to_string()),
        indexed: count as i64,
        count_at_start: state(migration).0,
        epoch: migration
            .catalog
            .db
            .query_row(
                "SELECT value FROM receipt_meta WHERE key='epoch'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
    });
}

#[test]
fn a_finishing_step_does_constant_sql_work_at_twelve_thousand_rows() {
    let (_temp, mut migration) = seeded(12_000);
    finishing(&mut migration, 12_000);
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = operations.clone();
    migration.catalog.db.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    assert!(migration.step().unwrap());
    migration
        .catalog
        .db
        .progress_handler(0, None::<fn() -> bool>);
    let work = operations.load(Ordering::Relaxed);
    eprintln!("FINAL_COUNT rows=12000 sqlite_vm_operations={work}");
    assert!(
        work < 1_000,
        "source finalization exceeded constant SQL work: {work}"
    );
    let state: (i64, bool) = migration
        .catalog
        .db
        .query_row(
            "SELECT indexed,complete FROM receipt_sources WHERE source='terminal'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (12_000, true));
}

fn original(catalog: &Catalog, n: u128) -> PathBuf {
    let directory = catalog.state.join(Source::Terminal.directory());
    fs::create_dir_all(&directory).unwrap();
    let id = uuid::Uuid::from_u128(n).to_string();
    let path = directory.join(format!("{id}.json"));
    let value = json!({"request_id":id,"name":"codex/fixture/count","run_id":"run",
        "conversation_id":"conversation","digest":"fixture","status":"submitted","submitted_at":n as f64});
    fs::write(&path, value.to_string()).unwrap();
    catalog.remember(Source::Terminal, &path, &value).unwrap();
    path
}

fn state(migration: &Migration) -> (i64, bool) {
    migration
        .catalog
        .db
        .query_row(
            "SELECT indexed,complete FROM receipt_sources WHERE source='terminal'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn finish(state: &Path) -> Migration {
    let mut migration = Migration::new(state).unwrap();
    while !migration.step().unwrap() {}
    migration
}

#[test]
fn a_backdated_arrival_preserves_the_live_count_until_a_stable_restart() {
    let (temp, mut migration) = seeded(0);
    let kept = original(&migration.catalog, 1);
    original(&migration.catalog, 2);
    let bytes = fs::read(&kept).unwrap();
    finishing(&mut migration, 2);
    // Its request sorts behind the already consumed cursor.
    original(&migration.catalog, 0);
    assert!(migration.step().unwrap());
    assert_eq!(state(&migration), (3, false));
    let restarted = finish(temp.path());
    assert_eq!(state(&restarted), (3, true));
    assert!(restarted.catalog.coverage_verified());
    assert_eq!(fs::read(&kept).unwrap(), bytes);
}

#[test]
fn a_changed_rowset_cannot_publish_the_old_partial_count() {
    let (temp, mut migration) = seeded(0);
    let kept = original(&migration.catalog, 1);
    original(&migration.catalog, 2);
    let bytes = fs::read(&kept).unwrap();
    finishing(&mut migration, 2);
    migration
        .catalog
        .db
        .execute(
            "DELETE FROM receipt_rows WHERE source='terminal' AND request=?",
            [uuid::Uuid::from_u128(1).to_string()],
        )
        .unwrap();
    assert!(migration.step().unwrap());
    assert_eq!(state(&migration), (1, false));
    let restarted = finish(temp.path());
    assert_eq!(state(&restarted), (2, true));
    assert!(restarted.catalog.coverage_verified());
    assert_eq!(fs::read(&kept).unwrap(), bytes);
}

#[test]
fn delayed_sql_cannot_turn_finalization_into_a_source_scan() {
    let (_temp, mut migration) = seeded(12_000);
    finishing(&mut migration, 12_000);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let observed = callbacks.clone();
    migration.catalog.db.progress_handler(
        1_000,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
            false
        }),
    );
    let started = Instant::now();
    assert!(migration.step().unwrap());
    let elapsed = started.elapsed();
    migration
        .catalog
        .db
        .progress_handler(0, None::<fn() -> bool>);
    let callbacks = callbacks.load(Ordering::Relaxed);
    eprintln!(
        "FINAL_COUNT_DELAY rows=12000 callbacks={callbacks} elapsed_us={} slice_us={}",
        elapsed.as_micros(),
        SLICE.as_micros()
    );
    assert_eq!(
        callbacks, 0,
        "finishing rescanned the source outside the slice"
    );
    assert_eq!(state(&migration), (12_000, true));
}

#[test]
fn discovery_repair_can_rotate_the_epoch_before_the_count_starts() {
    let (temp, migration) = seeded(0);
    let kept = original(&migration.catalog, 1);
    original(&migration.catalog, 2);
    let bytes = fs::read(&kept).unwrap();
    finish(temp.path());
    // Legacy row loss did not invalidate coverage or decrement its witness.
    let guard: String = migration
        .catalog
        .db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='receipt_row_removed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    migration
        .catalog
        .db
        .execute_batch("DROP TRIGGER receipt_row_removed")
        .unwrap();
    migration
        .catalog
        .db
        .execute(
            "DELETE FROM receipt_rows WHERE source='terminal' AND request=?",
            [uuid::Uuid::from_u128(1).to_string()],
        )
        .unwrap();
    migration.catalog.db.execute_batch(&guard).unwrap();
    let repaired = finish(temp.path());
    assert_eq!(state(&repaired), (2, true));
    assert!(repaired.catalog.coverage_verified());
    assert_eq!(fs::read(&kept).unwrap(), bytes);
}

#[path = "race_tests.rs"]
mod insert_race;
