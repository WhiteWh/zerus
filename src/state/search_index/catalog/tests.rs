use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    store: Store,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        for id in 0..count {
            fs::write(
                state.join(format!("{id:064x}.json")),
                "PRIVATE_BINDING_BODY_NEVER_STAGED",
            )
            .unwrap();
        }
        let store = Store::open(&state.join("content-search-v1")).unwrap();
        Self {
            _temp: temp,
            state,
            store,
        }
    }
    fn budget(entries: usize) -> DiscoveryBudget {
        DiscoveryBudget {
            entries,
            path_bytes: 8 << 20,
            deadline: Instant::now() + Duration::from_secs(2),
        }
    }
    fn discover(&self, entries: usize) -> DiscoveryProgress {
        self.discover_with_cancel(entries, &AtomicBool::new(false))
    }
    fn discover_with_cancel(&self, entries: usize, cancel: &AtomicBool) -> DiscoveryProgress {
        // Schema commits and filesystem work may consume a whole slice on
        // Windows. Retry the same bounded page; discovery does not save an
        // interrupted directory iterator, so never add repeated entry counts.
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..128 {
            let progress = self
                .store
                .discover(&self.state, &Self::budget(entries), cancel)
                .unwrap();
            assert!(Instant::now() < deadline, "fixture discovery timed out");
            assert!(progress.entries <= entries);
            if progress.complete || progress.entries == entries {
                return progress;
            }
        }
        panic!("fixture discovery exhausted its bounded attempts");
    }
    fn totals(&self) -> (i64, i64) {
        self.store
            .index
            .connection()
            .query_row(
                "SELECT paths,pending FROM catalog_totals WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
}

#[test]
fn discovery_stages_only_paths_and_requires_full_unchanged_coverage() {
    let fixture = Fixture::new(20);
    let progress = fixture.discover(3);
    assert_eq!(progress.entries, 3);
    assert!(!progress.complete);
    assert!(fixture.totals().0 <= 3);
    let complete = fixture.discover(100);
    assert!(complete.complete);
    assert_eq!(fixture.totals(), (20, 20));
    let repeat = fixture.discover(100);
    assert!(repeat.complete);
    assert_eq!(repeat.entries, 0);
    assert_eq!(repeat.staged, 0);
    assert_eq!(fixture.totals(), (20, 20));
    assert_eq!(fixture.store.index.status().unwrap().state, "cold");
    let bytes = fs::read(fixture.store.directory.join("index.sqlite3")).unwrap();
    assert!(!bytes
        .windows(b"PRIVATE_BINDING_BODY_NEVER_STAGED".len())
        .any(|v| v == b"PRIVATE_BINDING_BODY_NEVER_STAGED"));
}

#[test]
fn fixture_discovery_resumes_after_a_schema_commit_spends_the_first_slice() {
    let fixture = Fixture::new(1);
    let first_commit = std::sync::Arc::new(AtomicBool::new(true));
    let on_commit = first_commit.clone();
    fixture.store.index.connection().commit_hook(Some(move || {
        if on_commit.swap(false, Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(1100));
        }
        false
    }));
    let progress = fixture.discover(100);
    assert!(!first_commit.load(Ordering::Relaxed));
    assert!(progress.complete);
    assert_eq!(fixture.totals(), (1, 1));
}

#[test]
fn changed_metadata_returns_only_that_queued_path_to_pending() {
    let fixture = Fixture::new(20);
    fixture.discover(100);
    fixture
        .store
        .index
        .connection()
        .execute("UPDATE catalog_queue SET done=1", [])
        .unwrap();
    assert_eq!(fixture.totals(), (20, 0));
    let replacement = fixture.state.join("replacement");
    fs::write(&replacement, "changed").unwrap();
    fs::rename(replacement, fixture.state.join(format!("{:064x}.json", 3))).unwrap();
    assert_eq!(
        fixture.store.catalog_status().unwrap()["discovery"],
        "stale"
    );
    let progress = fixture.discover(100);
    assert!(progress.complete);
    assert_eq!(fixture.totals(), (20, 1));
}

#[test]
fn cancellation_expiry_and_invalid_budgets_cannot_claim_a_catalog() {
    let fixture = Fixture::new(20);
    let cancelled = fixture
        .store
        .discover(
            &fixture.state,
            &Fixture::budget(100),
            &AtomicBool::new(true),
        )
        .unwrap();
    assert_eq!(cancelled.entries, 0);
    assert!(!cancelled.complete);
    let budget = DiscoveryBudget {
        deadline: Instant::now() - Duration::from_secs(1),
        ..Fixture::budget(100)
    };
    let expired = fixture
        .store
        .discover(&fixture.state, &budget, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(expired.entries, 0);
    assert!(!expired.complete);
    assert!(fixture
        .store
        .discover(
            &fixture.state,
            &Fixture::budget(65_537),
            &AtomicBool::new(false)
        )
        .is_err());
}

#[test]
fn failed_schema_migration_rolls_back_and_can_be_retried() {
    let fixture = Fixture::new(1);
    let db = fixture.store.index.connection();
    // An interrupted/restored old schema cannot leave a half-created queue if
    // its migration fails. Discovery must remain unknown until a valid retry.
    db.execute_batch("CREATE TABLE catalog_totals(id INTEGER PRIMARY KEY)")
        .unwrap();
    assert!(fixture.store.catalog_schema().is_err());
    assert!(db.is_autocommit());
    let queue_exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='catalog_queue')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!queue_exists);
    db.execute_batch("DROP TABLE catalog_totals").unwrap();
    let progress = fixture.discover(100);
    assert!(progress.complete);
    assert_eq!(fixture.totals(), (1, 1));
}

#[test]
fn cancellation_during_schema_commit_keeps_setup_for_the_next_pass() {
    let fixture = Fixture::new(1);
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let on_commit = cancel.clone();
    fixture.store.index.connection().commit_hook(Some(move || {
        on_commit.store(true, Ordering::Relaxed);
        false
    }));
    let stopped = fixture
        .store
        .discover(&fixture.state, &Fixture::budget(100), &cancel)
        .unwrap();
    assert!(!stopped.complete);
    assert_eq!(stopped.entries, 0);
    assert_eq!(fixture.totals(), (0, 0));
    fixture
        .store
        .index
        .connection()
        .commit_hook(None::<fn() -> bool>);
    cancel.store(false, Ordering::Relaxed);
    let retry = fixture.discover_with_cancel(100, &cancel);
    assert!(retry.complete);
    assert_eq!(fixture.totals(), (1, 1));
}

#[test]
fn routed_bindings_archives_and_native_bindings_share_one_bounded_queue() {
    let fixture = Fixture::new(2);
    for folder in ["bindings", "archives", "dsh/bindings"] {
        fs::create_dir_all(fixture.state.join(folder)).unwrap();
    }
    fs::write(
        fixture
            .state
            .join("bindings")
            .join(format!("{:064x}.json", 42)),
        "{}",
    )
    .unwrap();
    fs::write(
        fixture
            .state
            .join("archives")
            .join(format!("{}.json", uuid::Uuid::new_v4())),
        "{}",
    )
    .unwrap();
    fs::write(
        fixture
            .state
            .join("dsh/bindings")
            .join(format!("{:064x}.json", 43)),
        "{}",
    )
    .unwrap();
    fs::write(fixture.state.join("unrelated.json"), "{}").unwrap();
    let result = fixture.discover(100);
    assert!(result.complete);
    assert_eq!(fixture.totals(), (5, 5));
}

#[cfg(unix)]
#[test]
fn discovery_never_follows_a_binding_symlink() {
    let fixture = Fixture::new(2);
    let other = fixture.state.join("unrelated.json");
    fs::write(&other, "PRIVATE").unwrap();
    std::os::unix::fs::symlink(&other, fixture.state.join(format!("{:064x}.json", 42))).unwrap();
    assert!(fixture.discover(100).complete);
    assert_eq!(fixture.totals(), (2, 2));
}
