//! zerus-kj3mb: terminal coverage converges while originals keep arriving.
//! A walk that the directory changed under ends with a short catch-up
//! listing instead of staying unknown until a quiet full walk.
use super::*;

/// An unregistered original, published by rename as a writer would.
fn raw(catalog: &Catalog, n: u128, at: f64) -> PathBuf {
    let directory = catalog.state.join(Source::Terminal.directory());
    fs::create_dir_all(&directory).unwrap();
    let id = uuid::Uuid::from_u128(n).to_string();
    let path = directory.join(format!("{id}.json"));
    let value = json!({"request_id":id,"name":"codex/fixture/count","run_id":"run",
        "conversation_id":"conversation","digest":"fixture","status":"submitted","submitted_at":at});
    let staged = directory.join(format!(".{id}.staged"));
    fs::write(&staged, value.to_string()).unwrap();
    fs::rename(&staged, &path).unwrap();
    path
}
/// Every catch-up entry costs a full unit of the step budget, so a listing of
/// these 150+ originals always spans several steps by count (BATCH) instead
/// of by the 25 ms slice, which a fast machine never reaches and a loaded
/// runner reaches early.
fn walker(state: &Path) -> Migration {
    let mut migration = Migration::with_sources(state, vec![Source::Terminal], true).unwrap();
    migration.light = 1;
    migration
}
/// Step to the end, calling `between` after every unfinished step. Returns
/// the most catch-up attempts the walk reached.
fn drive(migration: &mut Migration, mut between: impl FnMut(&mut Migration)) -> u32 {
    let mut attempts = 0;
    for _ in 0..10_000 {
        if migration.step().unwrap() {
            return attempts;
        }
        if let Some(walk) = migration.walk.as_ref() {
            attempts = attempts.max(walk.attempts);
        }
        between(migration);
    }
    panic!("the walk did not finish");
}
fn catching_up(migration: &Migration) -> bool {
    migration
        .walk
        .as_ref()
        .is_some_and(|walk| walk.catchup.is_some())
}
fn row_at(catalog: &Catalog, n: u128) -> Option<f64> {
    catalog
        .db
        .query_row(
            "SELECT at FROM receipt_rows WHERE source='terminal' AND request=?",
            [uuid::Uuid::from_u128(n).to_string()],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}
fn sealed_stamp(catalog: &Catalog) -> String {
    catalog
        .db
        .query_row(
            "SELECT stamp FROM receipt_sources WHERE source='terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn appends_and_rewrites_during_a_walk_converge_in_that_walk() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=150 {
        original(&catalog, n);
    }
    let mut migration = walker(temp.path());
    let mut changed = false;
    let attempts = drive(&mut migration, |_| {
        if !changed {
            // While discovery runs: a new original and a rewritten one.
            raw(&catalog, 1_000, 1_000.0);
            raw(&catalog, 5, 50.0);
            changed = true;
        }
    });
    // #5 is rewritten with a new `at`. When discovery had already passed it,
    // the first catch-up rereads it, the rebound trigger rotates the epoch on
    // purpose, and a second, quiet listing seals; when discovery had not, the
    // first catch-up is quiet. Either way the walk converges by itself.
    assert!((1..=2).contains(&attempts), "attempts: {attempts}");
    assert_eq!(state(&migration), (151, true));
    assert!(migration.catalog.coverage_verified());
    let directory = temp.path().join(Source::Terminal.directory());
    assert_eq!(sealed_stamp(&catalog), stamp(&directory).unwrap());
    assert_eq!(row_at(&catalog, 1_000), Some(1_000.0));
    assert_eq!(row_at(&catalog, 5), Some(50.0));
}

#[test]
fn a_catch_up_retries_after_a_change_and_gives_up_after_its_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=150 {
        original(&catalog, n);
    }
    // One change during the walk and one during the first catch-up listing:
    // the second listing is quiet and seals.
    let mut migration = walker(temp.path());
    let mut next = 2_000;
    let attempts = drive(&mut migration, |migration| {
        let first = migration
            .walk
            .as_ref()
            .is_some_and(|walk| walk.attempts <= 1);
        if next < 2_002 && (next == 2_000 || (catching_up(migration) && first)) {
            raw(&catalog, next, next as f64);
            next += 1;
        }
    });
    assert_eq!(attempts, 2);
    assert_eq!(state(&migration), (152, true));
    assert!(migration.catalog.coverage_verified());
    // A change during every listing: the walk ends after its attempts,
    // unknown, instead of listing forever.
    let mut migration = walker(temp.path());
    let mut next = 3_000;
    let attempts = drive(&mut migration, |_| {
        raw(&catalog, next, next as f64);
        next += 1;
    });
    assert_eq!(attempts, CATCHUP_ATTEMPTS);
    assert!(!state(&migration).1);
    assert!(!migration.catalog.coverage_verified() || !state(&migration).1);
    // Once the writer stops, the next walk seals every arrival.
    let migration = finish(temp.path());
    assert_eq!(state(&migration).0, 152 + i64::from((next - 3_000) as u32));
    assert!(state(&migration).1);
}

#[test]
fn a_vanished_original_is_never_sealed_without_its_explicit_gap() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut paths = Vec::new();
    for n in 1..=150 {
        paths.push(original(&catalog, n));
    }
    let gone = format!("{}.json", uuid::Uuid::from_u128(7));
    let gap = |catalog: &Catalog| -> bool {
        catalog
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM receipt_errors WHERE source='terminal' AND name=?)",
                [&gone],
                |row| row.get(0),
            )
            .unwrap()
    };
    // The original disappears after discovery listed it: the row pass skips
    // it as seen, so only the catch-up's row/original agreement can notice.
    let mut migration = walker(temp.path());
    let mut removed = false;
    let attempts = drive(&mut migration, |migration| {
        let listed = migration
            .walk
            .as_ref()
            .is_some_and(|walk| walk.entries.is_none() && walk.catchup.is_none());
        if listed && !removed {
            fs::remove_file(&paths[6]).unwrap();
            removed = true;
        }
    });
    assert!(removed);
    assert_eq!(attempts, CATCHUP_ATTEMPTS);
    assert_eq!(state(&migration), (150, false));
    assert!(!gap(&catalog));
    // The next walk records the explicit gap and seals with it.
    let migration = finish(temp.path());
    assert_eq!(state(&migration), (150, true));
    assert!(gap(&catalog));
    let scope = Scope {
        source: Source::Terminal,
        name: Some("codex/fixture/count".into()),
        run: "run".into(),
        conversation: "conversation".into(),
        agent: "".into(),
    };
    assert_eq!(
        migration
            .catalog
            .page(&scope, "", 10, false)
            .unwrap()
            .metadata["unattributed_errors"],
        1
    );
}

#[test]
fn a_rewrite_that_keeps_size_and_mtime_is_reread_near_the_walk_start() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=150 {
        original(&catalog, n);
    }
    let path = temp
        .path()
        .join(Source::Terminal.directory())
        .join(format!("{}.json", uuid::Uuid::from_u128(9)));
    // Near the walk start whatever the setup cost: writing 150 originals can
    // take longer than the coarse clock on a slow runner.
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
        .unwrap();
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    catalog.remember(Source::Terminal, &path, &value).unwrap();
    let mut migration = walker(temp.path());
    let mut rewritten = false;
    drive(&mut migration, |migration| {
        // After discovery read it: same length, same mtime (a coarse clock),
        // different projection.
        let discovered = migration
            .walk
            .as_ref()
            .is_some_and(|walk| walk.entries.is_none());
        if discovered && !rewritten {
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            let before = fs::read_to_string(&path).unwrap();
            let after = before.replace("\"submitted_at\":9.0", "\"submitted_at\":8.0");
            assert_ne!(before, after);
            assert_eq!(before.len(), after.len());
            let staged = path.with_extension("staged");
            fs::write(&staged, after).unwrap();
            fs::File::options()
                .write(true)
                .open(&staged)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
            fs::rename(&staged, &path).unwrap();
            rewritten = true;
        }
    });
    assert!(rewritten);
    assert_eq!(state(&migration), (150, true));
    assert_eq!(row_at(&catalog, 9), Some(8.0));
}

#[test]
fn a_counter_desync_during_a_walk_blocks_the_catch_up_seal() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut paths = Vec::new();
    for n in 1..=150 {
        paths.push(original(&catalog, n));
    }
    let mut migration = walker(temp.path());
    let (mut removed, mut desynced) = (false, false);
    drive(&mut migration, |migration| {
        let Some(walk) = migration.walk.as_ref() else {
            return;
        };
        // After discovery listed it, an original disappears (the walk then
        // needs a catch-up); once the catch-up runs, the counter drops with
        // it, as a trigger-bypassing delete would leave it. Rows and listed
        // originals now agree by count alone; only the witness token shows
        // that the counter is no longer exact.
        if walk.entries.is_none() && walk.catchup.is_none() && !removed {
            fs::remove_file(&paths[11]).unwrap();
            removed = true;
        } else if walk.catchup.is_some() && removed && !desynced {
            catalog
                .db
                .execute(
                    "UPDATE receipt_sources SET indexed=indexed-1 WHERE source='terminal'",
                    [],
                )
                .unwrap();
            desynced = true;
        }
    });
    assert!(desynced);
    assert_eq!(state(&migration), (149, false));
    assert!(!migration.catalog.coverage_verified());
}

#[test]
fn a_recorded_mismatch_never_gets_a_catch_up_even_if_its_token_returns() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=150 {
        original(&catalog, n);
    }
    catalog
        .db
        .execute(
            "UPDATE receipt_sources SET indexed=indexed+1 WHERE source='terminal'",
            [],
        )
        .unwrap();
    let mut migration = walker(temp.path());
    migration.light = 1;
    drive(&mut migration, |migration| {
        // Once the walk has counted, any writable open re-witnesses: after
        // the mismatch that restores a token over the repaired counter.
        if migration
            .walk
            .as_ref()
            .is_none_or(|walk| walk.entries.is_none())
        {
            drop(Catalog::open(temp.path(), true).unwrap());
        }
    });
    // The walk that recorded the mismatch did not seal, catch-up or not.
    assert_eq!(state(&migration), (150, false));
    assert!(catalog
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_errors WHERE source='terminal' AND name='@witness')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
}

fn error_for(catalog: &Catalog, name: &str) -> bool {
    catalog
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_errors WHERE source='terminal' AND name=?)",
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

/// zerus-kj3mb.1, the review probe: a gap restored after the row pass and an
/// already listed original that vanishes must not cancel out.
#[test]
fn a_restored_gap_cannot_offset_a_vanished_original() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    let mut paths = Vec::new();
    for n in 1..=150 {
        paths.push(original(&catalog, n));
    }
    // #3's original is missing before the walk: the row pass records its gap.
    let kept = fs::read(&paths[2]).unwrap();
    fs::remove_file(&paths[2]).unwrap();
    let third = format!("{}.json", uuid::Uuid::from_u128(3));
    let seventh = format!("{}.json", uuid::Uuid::from_u128(7));
    let mut migration = walker(temp.path());
    let mut swapped = false;
    drive(&mut migration, |migration| {
        let Some(walk) = migration.walk.as_ref() else {
            return;
        };
        // After the row pass recorded #3's gap, before the walk publishes:
        // #3 comes back and #7, already listed, disappears.
        if walk.entries.is_none()
            && walk.catchup.is_none()
            && error_for(&catalog, &third)
            && !swapped
        {
            fs::write(&paths[2], &kept).unwrap();
            fs::remove_file(&paths[6]).unwrap();
            swapped = true;
        }
    });
    assert!(swapped);
    // The row without an original keeps the source unknown.
    assert_eq!(state(&migration), (150, false));
    assert!(!error_for(&catalog, &third), "the restored gap was re-read");
    // The next walk records #7's explicit gap and seals with it.
    let next = finish(temp.path());
    assert_eq!(state(&next), (150, true));
    assert!(error_for(&catalog, &seventh));
}

/// zerus-kj3mb.2: a catch-up that cannot enumerate leaves a visible error,
/// which only a complete walk that started after it clears.
#[test]
fn a_catch_up_enumeration_failure_stays_visible_until_a_complete_walk() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(temp.path(), true).unwrap();
    for n in 1..=150 {
        original(&catalog, n);
    }
    let mut migration = walker(temp.path());
    let mut appended = false;
    drive(&mut migration, |migration| {
        if !appended {
            raw(&catalog, 4_000, 4_000.0);
            appended = true;
        } else if catching_up(migration) {
            FAIL_CATCHUP_ENTRY.with(|fail| fail.set(true));
        }
    });
    assert!(!state(&migration).1);
    assert!(error_for(&catalog, "@enumeration"));
    // An incomplete walk (a change in every listing) keeps it.
    let mut migration = walker(temp.path());
    let mut next = 5_000;
    drive(&mut migration, |_| {
        raw(&catalog, next, next as f64);
        next += 1;
    });
    assert!(!state(&migration).1);
    assert!(error_for(&catalog, "@enumeration"));
    // A complete walk clears it.
    let migration = finish(temp.path());
    assert!(state(&migration).1);
    assert!(!error_for(&catalog, "@enumeration"));
}
