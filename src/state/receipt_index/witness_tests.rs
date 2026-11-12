//! zerus-z5uax: the row-set witness token. The worker verifies the exact
//! COUNT(*) at every count publication and on every run; readers check the
//! token, the schema it was taken under and the rows appended since, in
//! bounded work. zerus-elev7: an unchanged row publishes no change.
use super::*;

fn token(catalog: &Catalog, source: Source) -> Option<WitnessEntry> {
    catalog
        .db
        .query_row(
            "SELECT value FROM receipt_meta WHERE key=?",
            [format!("{WITNESS_PREFIX}{}", source.key())],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .map(|value| serde_json::from_str(&value).unwrap())
}
fn mismatches(catalog: &Catalog) -> i64 {
    catalog
        .db
        .query_row(
            "SELECT value FROM receipt_meta WHERE key=?",
            [WITNESS_MISMATCHES],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .map_or(0, |value| {
            serde_json::from_str::<Value>(&value).unwrap()["count"]
                .as_i64()
                .unwrap()
        })
}
fn witness_error(catalog: &Catalog, source: Source) -> bool {
    catalog
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_errors WHERE source=? AND name='@witness')",
            [source.key()],
            |row| row.get(0),
        )
        .unwrap()
}
fn source_row(catalog: &Catalog, source: Source) -> (bool, i64) {
    catalog
        .db
        .query_row(
            "SELECT complete,indexed FROM receipt_sources WHERE source=?",
            [source.key()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}
fn meta(catalog: &Catalog, key: &str) -> String {
    catalog
        .db
        .query_row("SELECT value FROM receipt_meta WHERE key=?", [key], |row| {
            row.get(0)
        })
        .unwrap()
}
fn walk(state: &Path) {
    let mut migration = migration::Migration::new(state).unwrap();
    while !migration.step().unwrap() {}
}
/// Terminal rows without originals, inserted through the row triggers. Only
/// for checks that never walk them.
fn bulk(catalog: &Catalog, rows: i64) {
    let transaction = catalog.db.unchecked_transaction().unwrap();
    let change: i64 = transaction
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM receipt_meta WHERE key='change_seq'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let first: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM receipt_rows WHERE source='terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    transaction
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i+1 FROM n WHERE i<?2)
             INSERT INTO receipt_rows(source,request,name,run,conversation,agent,first_message,at,files,binding,stamp,change)
             SELECT 'terminal',printf('bulk-%010d',i),'codex/p/other','other-run','other-conversation','codex',0,i,0,'fixture-binding','missing',?3+i-?1+1 FROM n",
            params![first + 1, first + rows, change],
        )
        .unwrap();
    transaction
        .execute(
            "UPDATE receipt_meta SET value=? WHERE key='change_seq'",
            [(change + rows).to_string()],
        )
        .unwrap();
    transaction.commit().unwrap();
}
/// SQLite VM steps of `f` on this connection (counted, never interrupted).
fn steps<T>(catalog: &Catalog, f: impl FnOnce() -> T) -> (T, usize) {
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = counter.clone();
    catalog.db.progress_handler(
        100,
        Some(move || {
            seen.fetch_add(100, Ordering::Relaxed);
            false
        }),
    );
    let result = f();
    catalog.db.progress_handler(100, None::<fn() -> bool>);
    (result, counter.load(Ordering::Relaxed))
}

#[test]
fn a_new_cache_is_witnessed_and_appended_rows_keep_its_token_current() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for source in Source::ALL {
        let entry = token(&catalog, source).unwrap();
        assert_eq!((entry.count, entry.seq), (0, 0));
    }
    assert!(catalog.coverage_verified());
    for n in 1..=3 {
        put(
            &catalog,
            Source::Terminal,
            &value(n, "wanted", "run", "", n as f64),
        );
    }
    // Appended rows are counted on the source's sequence range after the token.
    assert!(catalog.coverage_verified());
    assert_eq!(token(&catalog, Source::Terminal).unwrap().count, 0);
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET indexed=indexed+1 WHERE source='terminal'",
            [],
        )
        .unwrap();
    assert!(!catalog.coverage_verified());
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET indexed=indexed-1 WHERE source='terminal'",
            [],
        )
        .unwrap();
    assert!(catalog.coverage_verified());
    // An old token stays valid but makes re-verification due.
    assert!(!catalog.verification_due());
    let mut old = token(&catalog, Source::Dsh).unwrap();
    old.at = 0.0;
    catalog
        .db
        .execute(
            "UPDATE receipt_meta SET value=? WHERE key=?",
            params![
                serde_json::to_string(&old).unwrap(),
                format!("{WITNESS_PREFIX}dsh")
            ],
        )
        .unwrap();
    assert!(catalog.coverage_verified());
    assert!(catalog.verification_due());
}

#[test]
fn a_quiet_source_keeps_its_old_token_while_another_source_grows() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    // The question token predates every terminal row; only the terminal
    // token follows the burst, as one walk's terminal publication would.
    bulk(&catalog, WITNESS_DELTA + 1);
    let transaction = catalog.db.unchecked_transaction().unwrap();
    assert!(catalog
        .witness_publish(Source::Terminal, WITNESS_DELTA + 1, WITNESS_DELTA + 1)
        .unwrap());
    transaction.commit().unwrap();
    assert_eq!(token(&catalog, Source::Question).unwrap().seq, 0);
    assert!(catalog.coverage_verified());
    // The cap is per source: a burst in one source is unknown by itself.
    bulk(&catalog, WITNESS_DELTA + 1);
    assert!(!catalog.coverage_verified());
}

#[test]
fn a_count_publication_records_a_mismatch_and_that_walk_never_reseals_it() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=2 {
        put(
            &catalog,
            Source::Terminal,
            &value(n, "wanted", "run", "", n as f64),
        );
    }
    walk(temp.path());
    assert_eq!(source_row(&catalog, Source::Terminal), (true, 2));
    assert_eq!(token(&catalog, Source::Terminal).unwrap().count, 2);
    assert!(catalog.coverage_verified());
    // A counter that no longer agrees with its rows, as a trigger-bypassing
    // write would leave it. The read check refuses it at once.
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET indexed=indexed+1 WHERE source='terminal'",
            [],
        )
        .unwrap();
    assert!(!catalog.coverage_verified());
    let scope = scope("wanted", "run", "");
    let epoch = meta(&catalog, "epoch");
    // The walk's count publication verifies the exact rows in its own
    // transaction: the mismatch is recorded, nothing is sealed.
    walk(temp.path());
    assert_eq!(source_row(&catalog, Source::Terminal), (false, 2));
    assert!(witness_error(&catalog, Source::Terminal));
    assert_eq!(mismatches(&catalog), 1);
    // Later sources of the same walk publish their own tokens; the missing
    // terminal token keeps every read unknown.
    assert!(token(&catalog, Source::Terminal).is_none());
    assert_ne!(meta(&catalog, "epoch"), epoch);
    assert!(!catalog.coverage_verified());
    let page = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(page.metadata["migration_complete"], false);
    assert_eq!(page.metadata["unattributed_errors"], 1);
    // A later walk, which starts after the record and re-reads every
    // original, may seal again. The diagnostic stays.
    walk(temp.path());
    assert_eq!(source_row(&catalog, Source::Terminal), (true, 2));
    assert!(!witness_error(&catalog, Source::Terminal));
    assert!(catalog.coverage_verified());
    assert_eq!(mismatches(&catalog), 1);
    let page = catalog.page(&scope, "", 100, true).unwrap();
    assert_eq!(page.metadata["migration_complete"], true);
    assert_eq!(page.metadata["unattributed_errors"], 0);
}

#[test]
fn a_mismatch_recorded_during_a_walk_outlives_that_walk() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    // More originals than one step examines, so the walk spans steps.
    for n in 1..=150 {
        put(
            &catalog,
            Source::Terminal,
            &value(n, "wanted", "run", "", n as f64),
        );
    }
    walk(temp.path());
    let mut migration = migration::Migration::new(temp.path()).unwrap();
    assert!(!migration.step().unwrap());
    // Another process records a mismatch while the terminal walk runs.
    let other = Catalog::open(temp.path(), true).unwrap();
    other
        .db
        .execute(
            "UPDATE receipt_sources SET indexed=indexed+1 WHERE source='terminal'",
            [],
        )
        .unwrap();
    assert_eq!(other.witness().unwrap(), Witnessed::Mismatch);
    assert!(witness_error(&catalog, Source::Terminal));
    while !migration.step().unwrap() {}
    // That walk began before the record: it cannot clear it.
    assert!(witness_error(&catalog, Source::Terminal));
    assert_eq!(
        catalog
            .page(&scope("wanted", "run", ""), "", 100, true)
            .unwrap()
            .metadata["unattributed_errors"],
        1
    );
    walk(temp.path());
    assert!(!witness_error(&catalog, Source::Terminal));
    assert_eq!(source_row(&catalog, Source::Terminal), (true, 150));
    assert!(catalog.coverage_verified());
}

#[test]
fn every_worker_run_rewitnesses_even_when_a_walk_cannot_publish() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    bulk(&catalog, WITNESS_DELTA + 1);
    // Too many rows after the token: unknown, never a long read.
    assert!(!catalog.coverage_verified());
    // The terminal walk cannot publish (its source is not a directory), so
    // only the worker's own re-witness can refresh the terminal token.
    fs::write(
        temp.path().join(Source::Terminal.directory()),
        b"not a directory",
    )
    .unwrap();
    assert_eq!(migration::worker(temp.path()).unwrap(), 0);
    let entry = token(&catalog, Source::Terminal).unwrap();
    assert_eq!(entry.count, WITNESS_DELTA + 1);
    assert!(catalog.coverage_verified());
    assert!(!catalog.verification_due());
}

#[test]
fn a_schema_change_voids_the_token_and_a_silent_loss_under_it_is_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=2 {
        put(
            &catalog,
            Source::Terminal,
            &value(n, "wanted", "run", "", n as f64),
        );
    }
    walk(temp.path());
    assert!(catalog.coverage_verified());
    // Lose a row while its delete trigger is gone, then restore the trigger
    // exactly: the guards look intact, the counter still says 2.
    let removed: String = catalog
        .db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='receipt_row_removed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    catalog
        .db
        .execute_batch(&format!(
            "DROP TRIGGER receipt_row_removed;
             DELETE FROM receipt_rows WHERE source='terminal' AND request='{}';
             {removed};",
            uuid::Uuid::from_u128(1)
        ))
        .unwrap();
    assert!(rowset_protocol(&catalog.db));
    assert_eq!(source_row(&catalog, Source::Terminal), (true, 2));
    assert!(!catalog.coverage_verified());
    // The next writable open re-witnesses the changed schema and records it.
    let reopened = Catalog::open(temp.path(), true).unwrap();
    assert!(witness_error(&reopened, Source::Terminal));
    assert_eq!(mismatches(&reopened), 1);
    assert_eq!(source_row(&reopened, Source::Terminal), (false, 1));
    assert!(!reopened.coverage_verified());
    // A walk after the record restores the lost row from its original.
    walk(temp.path());
    assert_eq!(source_row(&reopened, Source::Terminal), (true, 2));
    assert!(!witness_error(&reopened, Source::Terminal));
    assert!(reopened.coverage_verified());
}

/// The read check's work does not grow with the history: a token over
/// `rows` exact rows, then a few appended rows, then a question page bounded
/// by the inspection budget.
fn bounded_read(rows: i64) -> (usize, usize) {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut left = rows;
    while left > 0 {
        let chunk = left.min(100_000);
        bulk(&catalog, chunk);
        left -= chunk;
    }
    assert_eq!(catalog.witness().unwrap(), Witnessed::Agreed);
    let (verified, fresh) = steps(&catalog, || catalog.coverage_verified());
    assert!(verified);
    bulk(&catalog, 100);
    let (verified, appended) = steps(&catalog, || catalog.coverage_verified());
    assert!(verified);
    // A question page under the production inspection budget.
    let question = value(1, "conversation", "run", "", 1.0);
    let mut question = question;
    question["question_id"] = json!("question");
    question["question_hash"] = json!("question-hash");
    put(&catalog, Source::Question, &question);
    let stamp = migration::stamp(&temp.path().join(Source::Question.directory())).unwrap();
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET stamp=?,complete=1 WHERE source='question'",
            [stamp],
        )
        .unwrap();
    let scope = Scope {
        source: Source::Question,
        name: Some("codex/fixture/receipt".into()),
        run: "run".into(),
        conversation: "conversation".into(),
        agent: String::new(),
    };
    let page = catalog
        .page_until(
            &scope,
            "",
            64,
            false,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap_or_else(|error| panic!("{}", error.message()));
    assert_eq!(page.metadata["migration_complete"], true);
    assert_eq!(page.locators.len(), 1);
    eprintln!("WITNESS_READ rows={rows} fresh_steps={fresh} appended_steps={appended}");
    (fresh, appended)
}

#[test]
fn the_read_check_stays_bounded_with_a_large_send_history() {
    let (small_fresh, small_appended) = bounded_read(1_000);
    let (fresh, appended) = bounded_read(LARGE_HISTORY);
    // The exact COUNT(*) witness took about 3 steps per row; the token check
    // reads a fixed set of rows plus the 100 appended ones.
    assert!(fresh <= 10_000, "{fresh} steps for {LARGE_HISTORY} rows");
    assert!(appended <= 20_000, "{appended} steps for 100 appended rows");
    assert!(fresh <= small_fresh + 1_000, "{small_fresh} -> {fresh}");
    assert!(
        appended <= small_appended + 1_000,
        "{small_appended} -> {appended}"
    );
}

/// The size the coordinator asked for, about a year of the owner's sends.
#[test]
#[ignore = "about 1.3M rows; run explicitly for the full-size evidence"]
fn the_read_check_stays_bounded_with_a_year_of_sends() {
    let (fresh, appended) = bounded_read(1_300_000);
    assert!(fresh <= 10_000, "{fresh}");
    assert!(appended <= 20_000, "{appended}");
}
const LARGE_HISTORY: i64 = 200_000;

#[test]
fn an_unchanged_empty_conversation_row_publishes_no_change_on_a_later_walk() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    put(&catalog, Source::Terminal, &value(1, "", "run", "", 1.0));
    walk(temp.path());
    let first = meta(&catalog, "change_seq");
    let row: i64 = catalog
        .db
        .query_row(
            "SELECT change FROM receipt_rows WHERE source='terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    walk(temp.path());
    walk(temp.path());
    assert_eq!(meta(&catalog, "change_seq"), first);
    assert_eq!(
        catalog
            .db
            .query_row(
                "SELECT change FROM receipt_rows WHERE source='terminal'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        row
    );
    assert_eq!(source_row(&catalog, Source::Terminal), (true, 1));
}
