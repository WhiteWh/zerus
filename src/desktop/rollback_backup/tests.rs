use super::*;
use crate::desktop::store::Store;
use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

// Each independent store fixture gets its own production-sized pin registry.
// The child runs this SAME test body and assertions; there is no cap reset or
// eviction, and a zero-test child cannot count as a successful control.
#[cfg(unix)]
fn isolated(name: &str) -> bool {
    let exact = format!("desktop::store::rollback_backup::tests::{name}");
    if std::env::var("ZERUS_PRIVATE_BACKUP_TEST").as_deref() == Ok(exact.as_str()) {
        return false;
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .args([&exact, "--exact", "--nocapture"])
        .env("ZERUS_PRIVATE_BACKUP_TEST", &exact)
        .output()
        .unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(child.status.success(), "{output}");
    assert!(output.contains("1 passed; 0 failed"), "{output}");
    true
}
#[cfg(windows)]
fn isolated(_name: &str) -> bool {
    false
}

fn location(root: &Path) -> Location {
    Location {
        root: root.into(),
        configuration: "a".repeat(64),
    }
}
fn make_old(root: &Path) -> Location {
    let location = location(root);
    location.prepare().unwrap();
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    crate::desktop::creation_worktree::rewind_slots(&store.db);
    store.db.execute_batch("PRAGMA user_version=34; CREATE TABLE rollback_sentinel(value TEXT NOT NULL); INSERT INTO rollback_sentinel VALUES('original row');").unwrap();
    drop(store);
    location
}
fn epoch() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn schema(db: &Connection) -> i64 {
    db.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}
fn backup_bytes(location: &Location) -> Vec<(PathBuf, Vec<u8>)> {
    let root = location.root.join(BACKUP);
    let mut files = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let b = fs::read(&p).unwrap();
            (p, b)
        })
        .collect()
}
fn copy_for_restore(location: &Location, target: &Path) {
    fs::create_dir(target).unwrap();
    for name in [DATABASE, WAL] {
        let from = location.root.join(BACKUP).join(name);
        if from.exists() {
            fs::copy(from, target.join(name)).unwrap();
        }
    }
}
#[test]
fn production_open_keeps_one_complete_backup_and_later_starts_never_overwrite_it() {
    if isolated("production_open_keeps_one_complete_backup_and_later_starts_never_overwrite_it") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    // Force the positive no-WAL case, independently of SQLite's creation of
    // an empty WAL during a read-only probe of a WAL-mode database.
    Connection::open(location.root.join(DATABASE))
        .unwrap()
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    let owner = location.lock().unwrap();
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    let original = backup_bytes(&location);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(location.root.join(BACKUP).join(MANIFEST)).unwrap())
            .unwrap();
    assert_eq!(manifest.source_schema, 34);
    assert!(manifest.wal.is_none());
    store
        .db
        .execute_batch("INSERT INTO rollback_sentinel VALUES('after migration');")
        .unwrap();
    drop(store);
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(backup_bytes(&location), original);
    drop(store);
    let restored = temp.path().join("restored");
    copy_for_restore(&location, &restored);
    let restored = Connection::open(restored.join(DATABASE)).unwrap();
    assert_eq!(schema(&restored), 34);
    assert_eq!(
        restored
            .query_row::<i64, _, _>("SELECT count(*) FROM rollback_sentinel", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

#[test]
fn a_changed_original_during_copy_is_refused_and_its_incomplete_backup_is_retained() {
    if isolated("a_changed_original_during_copy_is_refused_and_its_incomplete_backup_is_retained") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let path = location.root.join(DATABASE);
    let changed = path.clone();
    let owner = location.lock().unwrap();
    DURING_SOURCE_COPY.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // Deliberate external-writer fault in this private fixture. Its final
            // close checkpoints the WAL while the original copy is in flight.
            let writer = Connection::open(&changed).unwrap();
            writer
                .execute_batch("INSERT INTO rollback_sentinel VALUES('external change')")
                .unwrap();
        }))
    });
    let error = Store::open_owned(&location, &epoch(), &owner)
        .err()
        .unwrap();
    DURING_SOURCE_COPY.with(|slot| slot.borrow_mut().take());
    assert!(format!("{error:#}").contains("input changed"), "{error:#}");
    assert_eq!(schema(&Connection::open(&path).unwrap()), 34);
    assert!(!location.root.join(BACKUP).join(MANIFEST).exists());
    let before = backup_bytes(&location);
    assert!(Store::open_owned(&location, &epoch(), &owner).is_err());
    assert_eq!(backup_bytes(&location), before);
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM rollback_sentinel", [], |r| r.get(0))
            .unwrap(),
        2
    );
}

#[test]
fn the_schema_34_preflight_refuses_35_without_rewriting_its_database_or_columns() {
    if isolated("the_schema_34_preflight_refuses_35_without_rewriting_its_database_or_columns") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let owner = location.lock().unwrap();
    drop(Store::open_owned(&location, &epoch(), &owner).unwrap());
    let path = location.root.join(DATABASE);
    let before = fs::read(&path).unwrap();
    let backup = backup_bytes(&location);
    let error = super::super::schema_before_write(&path, 34).unwrap_err();
    assert!(error.to_string().contains("requires a newer hgs version"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(backup_bytes(&location), backup);
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(schema(&db), crate::desktop::store::SCHEMA);
    assert_eq!(db.query_row::<i64,_,_>("SELECT count(*) FROM pragma_table_info('desktop_creation_worktrees') WHERE name IN ('creation_id','creation_slot')",[],|r|r.get(0)).unwrap(),2);
}
/// Task #421 (2026-11-09): an interrupted attempt left an EMPTY
/// schema-34-rollback (on Windows its first file could not be published), and
/// every later start refused it. Now the complete backup is made in that empty
/// folder, and the store migrates.
#[test]
fn an_empty_leftover_backup_is_retained_and_the_backup_is_made_again() {
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    platform::private_dir(&location.root.join(BACKUP)).unwrap();
    let owner = location.lock().unwrap();
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    drop(store);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(location.root.join(BACKUP).join(MANIFEST)).unwrap()).unwrap();
    assert_eq!(manifest.source_schema, 34);
    assert!(location.root.join(BACKUP).join(DATABASE).is_file());
    let others = fs::read_dir(&location.root)
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with(&format!("{BACKUP}.")))
        .count();
    assert_eq!(others, 0, "the backup is made in the leftover itself");
}

/// Task #440: `hgs desktop store-check` tells, read-only, whether a start
/// gets past the store and its rollback backup; the window relaunches only
/// when it does.
#[test]
fn the_store_check_reads_the_backup_state_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let check = || super::super::store_check(&location);
    assert_eq!((check()["code"].clone(), check()["opens"].clone()), (serde_json::json!("backup_needed"), serde_json::json!(true)));
    platform::private_dir(&location.root.join(BACKUP)).unwrap();
    assert_eq!((check()["code"].clone(), check()["opens"].clone()), (serde_json::json!("empty_leftover"), serde_json::json!(true)));
    fs::write(location.root.join(BACKUP).join("partial"), b"x").unwrap();
    assert_eq!((check()["code"].clone(), check()["opens"].clone()), (serde_json::json!("backup_incomplete"), serde_json::json!(false)));
    fs::remove_file(location.root.join(BACKUP).join("partial")).unwrap();
    fs::remove_dir(location.root.join(BACKUP)).unwrap();
    let owner = location.lock().unwrap();
    {
        let handoff = location.handoff_lock().unwrap();
        Original::new(&location, &owner, &handoff).unwrap().prepare(Some(34)).unwrap();
    }
    let before = backup_bytes(&location);
    assert_eq!(check()["code"], "backup_present");
    assert_eq!(backup_bytes(&location), before, "the check writes nothing");
    let db = Connection::open(location.root.join(DATABASE)).unwrap();
    assert_eq!(schema(&db), 34, "the check never migrates");
}

/// Task #421: the backup of an older store (schema 30, the owner's) completes:
/// its manifest, the database and, when there is one, the WAL.
#[test]
fn a_schema_30_store_is_backed_up_completely() {
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let owner = location.lock().unwrap();
    let handoff = location.handoff_lock().unwrap();
    let original = Original::new(&location, &owner, &handoff).unwrap();
    original.prepare(Some(30)).unwrap();
    let backup = location.root.join(BACKUP);
    let manifest: Manifest = serde_json::from_slice(&fs::read(backup.join(MANIFEST)).unwrap()).unwrap();
    assert_eq!(manifest.source_schema, 30);
    assert_eq!(fs::read(backup.join(DATABASE)).unwrap().len() as u64, manifest.database.bytes);
    assert_eq!(manifest.wal.is_some(), backup.join(WAL).is_file());
    // A later start finds it complete and never rewrites it.
    let before = backup_bytes(&location);
    original.prepare(Some(30)).unwrap();
    assert_eq!(backup_bytes(&location), before);
}

#[test]
fn partial_or_corrupt_backup_is_never_repaired_or_used_for_a_migration() {
    if isolated("partial_or_corrupt_backup_is_never_repaired_or_used_for_a_migration") {
        return;
    }
    for corrupt in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
        let owner = location.lock().unwrap();
        if corrupt {
            let handoff = location.handoff_lock().unwrap();
            let original = Original::new(&location, &owner, &handoff).unwrap();
            original.prepare(Some(34)).unwrap();
            fs::write(
                location.root.join(BACKUP).join(DATABASE),
                b"retain corrupt backup",
            )
            .unwrap();
        } else {
            platform::private_dir(&location.root.join(BACKUP)).unwrap();
            fs::write(
                location.root.join(BACKUP).join("incomplete"),
                b"retain partial backup",
            )
            .unwrap();
        }
        let before = backup_bytes(&location);
        assert!(Store::open_owned(&location, &epoch(), &owner).is_err());
        assert_eq!(backup_bytes(&location), before);
        let db = Connection::open(location.root.join(DATABASE)).unwrap();
        assert_eq!(schema(&db), 34);
        assert_eq!(
            db.query_row::<String, _, _>("SELECT value FROM rollback_sentinel", [], |r| r.get(0))
                .unwrap(),
            "original row"
        );
    }
}
#[test]
fn interrupted_slot_upgrade_rolls_back_the_real_transaction_and_keeps_the_backup() {
    if isolated("interrupted_slot_upgrade_rolls_back_the_real_transaction_and_keeps_the_backup") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "desktop::store::rollback_backup::tests::interrupted_upgrade_probe",
            "--exact",
            "--nocapture",
        ])
        .env("ZERUS_PRIVATE_SLOT_INTERRUPT", &location.root)
        .output()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            child.status.signal(),
            Some(libc::SIGKILL),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
    }
    #[cfg(not(unix))]
    assert_eq!(
        child.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let backup = backup_bytes(&location);
    let db = Connection::open(location.root.join(DATABASE)).unwrap();
    assert_eq!(schema(&db), 34);
    let columns = db
        .prepare("PRAGMA table_info(desktop_creation_worktrees)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(columns.contains(&"creation_id".into()));
    assert!(!columns.contains(&"creation_slot".into()));
    assert_eq!(
        db.query_row::<i64, _, _>(
            "SELECT count(*) FROM sqlite_schema WHERE name='desktop_creation_worktree_original'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row::<String, _, _>("SELECT value FROM rollback_sentinel", [], |r| r.get(0))
            .unwrap(),
        "original row"
    );
    drop(db);
    let owner = location.lock().unwrap();
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    assert_eq!(backup_bytes(&location), backup);
}
#[test]
fn interrupted_upgrade_probe() {
    let Some(root) = std::env::var_os("ZERUS_PRIVATE_SLOT_INTERRUPT") else {
        return;
    };
    let location = location(Path::new(&root));
    let owner = location.lock().unwrap();
    INTERRUPT.with(|v| v.set(true));
    let _ = Store::open_owned(&location, &epoch(), &owner);
    panic!("migration did not reach interruption boundary");
}
#[test]
fn committed_wal_is_copied_with_the_database_and_restores_the_original_row() {
    if isolated("committed_wal_is_copied_with_the_database_and_restores_the_original_row") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("state/desktop");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "desktop::store::rollback_backup::tests::leave_committed_wal_probe",
            "--exact",
            "--nocapture",
        ])
        .env("ZERUS_PRIVATE_COMMITTED_WAL", &root)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(87),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(root.join(WAL).exists());
    let location = location(&root);
    let owner = location.lock().unwrap();
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    drop(store);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join(BACKUP).join(MANIFEST)).unwrap()).unwrap();
    assert!(manifest.wal.as_ref().is_some_and(|v| v.bytes > 0));
    let restored = temp.path().join("restored");
    copy_for_restore(&location, &restored);
    let db = Connection::open(restored.join(DATABASE)).unwrap();
    assert_eq!(schema(&db), 34);
    assert_eq!(
        db.query_row::<String, _, _>(
            "SELECT value FROM rollback_sentinel WHERE value='committed WAL row'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        "committed WAL row"
    );
}
#[test]
fn leave_committed_wal_probe() {
    let Some(root) = std::env::var_os("ZERUS_PRIVATE_COMMITTED_WAL") else {
        return;
    };
    let location = make_old(Path::new(&root));
    let db = Connection::open(location.root.join(DATABASE)).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; INSERT INTO rollback_sentinel VALUES('committed WAL row');").unwrap();
    std::process::exit(87);
}

#[test]
fn an_existing_backup_requires_original_root_store_configuration_and_its_complete_roster() {
    if isolated(
        "an_existing_backup_requires_original_root_store_configuration_and_its_complete_roster",
    ) {
        return;
    }
    for changed in [
        "configuration",
        "root_inode",
        "database_inode",
        "extra_file",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
        let owner = location.lock().unwrap();
        {
            let handoff = location.handoff_lock().unwrap();
            let original = Original::new(&location, &owner, &handoff).unwrap();
            original.prepare(Some(34)).unwrap();
        }
        let backup = location.root.join(BACKUP);
        if changed == "extra_file" {
            fs::write(backup.join("unexpected"), b"preserve").unwrap();
        } else {
            let file = backup.join(MANIFEST);
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            if changed == "configuration" {
                value[changed] = serde_json::json!("b".repeat(64));
            } else {
                value[changed] = serde_json::json!(0);
            }
            fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let before = backup_bytes(&location);
        assert!(Store::open_owned(&location, &epoch(), &owner).is_err());
        assert_eq!(backup_bytes(&location), before);
        let db = Connection::open(location.root.join(DATABASE)).unwrap();
        assert_eq!(schema(&db), 34);
    }
}

#[cfg(windows)]
#[test]
fn a_fresh_or_already_35_windows_store_starts_without_inventing_a_backup() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(&temp.path().canonicalize().unwrap().join("state/desktop"));
    location.prepare().unwrap();
    let owner = location.lock().unwrap();
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    assert!(!location.root.join(BACKUP).exists());
    drop(store);
    let store = Store::open_owned(&location, &epoch(), &owner).unwrap();
    assert_eq!(schema(&store.db), crate::desktop::store::SCHEMA);
    assert!(!location.root.join(BACKUP).exists());
}

#[cfg(windows)]
#[test]
fn a_windows_crash_during_backup_keeps_schema_34_and_refuses_the_incomplete_copy() {
    for phase in ["database", "manifest"] {
        let temp = tempfile::tempdir().unwrap();
        let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
        // Establish literal DB/WAL absence/bytes before the read-only probe.
        Connection::open(location.root.join(DATABASE))
            .unwrap()
            .execute_batch("PRAGMA journal_mode=DELETE")
            .unwrap();
        let before = fs::read(location.root.join(DATABASE)).unwrap();
        assert!(!location.root.join(WAL).exists());
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "desktop::store::rollback_backup::tests::windows_backup_crash_probe",
                "--exact",
                "--nocapture",
            ])
            .env("ZERUS_PRIVATE_BACKUP_CRASH", &location.root)
            .env("ZERUS_PRIVATE_BACKUP_PHASE", phase);
        let child = crate::platform::output_until(&mut command, Duration::from_secs(45)).unwrap();
        assert_eq!(
            child.status.code(),
            Some(88),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(fs::read(location.root.join(DATABASE)).unwrap(), before);
        assert!(!location.root.join(WAL).exists());
        let backup = backup_bytes(&location);
        assert!(!location.root.join(BACKUP).join(MANIFEST).exists());
        let owner = location.lock().unwrap();
        let error = match Store::open_owned(&location, &epoch(), &owner) {
            Ok(_) => panic!("an incomplete backup must not be repaired"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("docs/desktop-schema-35-rollback.md"));
        assert_eq!(backup_bytes(&location), backup);
        assert_eq!(fs::read(location.root.join(DATABASE)).unwrap(), before);
        let db = Connection::open_with_flags(
            location.root.join(DATABASE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(schema(&db), 34);
        assert_eq!(
            db.query_row::<String, _, _>("SELECT value FROM rollback_sentinel", [], |row| row
                .get(0),)
                .unwrap(),
            "original row"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_backup_crash_probe() {
    let Some(root) = std::env::var_os("ZERUS_PRIVATE_BACKUP_CRASH") else {
        return;
    };
    let phase = match std::env::var("ZERUS_PRIVATE_BACKUP_PHASE")
        .unwrap()
        .as_str()
    {
        "database" => "database",
        "manifest" => "manifest",
        _ => panic!("unexpected private backup crash boundary"),
    };
    let location = location(Path::new(&root));
    let owner = location.lock().unwrap();
    BACKUP_CRASH.with(|slot| slot.set(Some(phase)));
    let _ = Store::open_owned(&location, &epoch(), &owner);
    panic!("backup did not reach its private crash boundary");
}
#[cfg(unix)]
#[test]
fn irregular_or_replaced_inputs_and_exclusions_refuse_before_migration() {
    if isolated("irregular_or_replaced_inputs_and_exclusions_refuse_before_migration") {
        return;
    }
    use std::os::unix::fs::symlink;
    for which in [
        DATABASE,
        WAL,
        "owner.lock",
        zerus_desktop_migration::HANDOFF_LOCK,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let location = make_old(&temp.path().canonicalize().unwrap().join("state/desktop"));
        let owner = location.lock().unwrap();
        let handoff = location.handoff_lock().unwrap();
        let original = Original::new(&location, &owner, &handoff).unwrap();
        let path = location.root.join(which);
        let retained = location.root.join(format!("retained-{which}"));
        if path.exists() {
            fs::rename(&path, &retained).unwrap();
        }
        if which == WAL {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        } else if which == DATABASE {
            symlink(&retained, &path).unwrap();
        } else {
            fs::write(&path, b"replaced exclusion").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let start = Instant::now();
        assert!(original.prepare(Some(34)).is_err(), "{which}");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!location.root.join(BACKUP).join(MANIFEST).exists());
    }
}

#[cfg(unix)]
fn rollback_reader(path: &Path) -> Connection {
    let reader = Connection::open(path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM rollback_sentinel;")
        .unwrap();
    assert!(crate::sqlite_file::foreign_lock_refused(path));
    reader
}
#[cfg(unix)]
fn old_without_wal(root: &Path) -> Location {
    let location = make_old(root);
    Connection::open(location.root.join(DATABASE))
        .unwrap()
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    location
}
#[cfg(unix)]
#[test]
fn rejected_wal_and_journal_aliases_preserve_an_independent_reader_lock() {
    if isolated("rejected_wal_and_journal_aliases_preserve_an_independent_reader_lock") {
        return;
    }
    let mut observed = Vec::new();
    for name in [WAL, "state.sqlite3-journal"] {
        let temp = tempfile::tempdir().unwrap();
        let location = old_without_wal(&temp.path().canonicalize().unwrap().join("state/desktop"));
        let owner = location.lock().unwrap();
        let handoff = location.handoff_lock().unwrap();
        let original = Original::new(&location, &owner, &handoff).unwrap();
        let path = location.root.join(DATABASE);
        let reader = rollback_reader(&path);
        let alias = location.root.join(name);
        fs::hard_link(&path, &alias).unwrap();
        let refused = if name == WAL {
            Input::open(&original.root, WAL).is_err()
        } else {
            no_journal(&original.root).is_err()
        };
        let lock_kept = crate::sqlite_file::foreign_lock_refused(&path);
        fs::remove_file(alias).unwrap();
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        let last_close_released = !crate::sqlite_file::foreign_lock_refused(&path);
        observed.push((name, refused, lock_kept, last_close_released));
    }
    assert!(
        observed
            .iter()
            .all(|(_, refused, lock_kept, released)| *refused && *lock_kept && *released),
        "{observed:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_input_replaced_between_held_and_named_checks_does_not_close_its_alias() {
    if isolated("an_input_replaced_between_held_and_named_checks_does_not_close_its_alias") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = old_without_wal(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let owner = location.lock().unwrap();
    let handoff = location.handoff_lock().unwrap();
    let original = Original::new(&location, &owner, &handoff).unwrap();
    let path = location.root.join(DATABASE);
    let wal = location.root.join(WAL);
    fs::write(&wal, b"private input").unwrap();
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
    let input = Input::open(&original.root, WAL).unwrap().unwrap();
    let reader = rollback_reader(&path);
    let saved = location.root.join("retained-wal");
    let (changed, main, retained) = (wal.clone(), path.clone(), saved.clone());
    BEFORE_INPUT_NAME.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            fs::rename(&changed, retained).unwrap();
            fs::hard_link(main, changed).unwrap();
        }))
    });
    let refused = input.verify(&original.root).is_err();
    let lock_kept = crate::sqlite_file::foreign_lock_refused(&path);
    fs::remove_file(wal).unwrap();
    fs::rename(saved, location.root.join(WAL)).unwrap();
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    assert!(
        refused && lock_kept,
        "refused={refused}, lock_kept={lock_kept}"
    );
    assert!(!crate::sqlite_file::foreign_lock_refused(&path));
}

#[cfg(unix)]
#[test]
fn an_alias_opened_after_the_name_check_is_retained_even_when_validation_fails() {
    if isolated("an_alias_opened_after_the_name_check_is_retained_even_when_validation_fails") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = old_without_wal(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let owner = location.lock().unwrap();
    let handoff = location.handoff_lock().unwrap();
    let original = Original::new(&location, &owner, &handoff).unwrap();
    let path = location.root.join(DATABASE);
    let wal = location.root.join(WAL);
    fs::write(&wal, b"private input").unwrap();
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
    let reader = rollback_reader(&path);
    let before = super::super::startup::retained_count();
    let main = path.clone();
    super::super::startup::BEFORE_RAW_OPEN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |root, name| {
            assert_eq!(name, WAL);
            fs::remove_file(root.path.join(name)).unwrap();
            fs::hard_link(main, root.path.join(name)).unwrap();
        }))
    });
    let refused = Input::open(&original.root, WAL).is_err();
    let retained = super::super::startup::retained_count() == before + 1;
    let lock_kept = crate::sqlite_file::foreign_lock_refused(&path);
    fs::remove_file(wal).unwrap();
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    assert!(
        refused && retained && lock_kept,
        "refused={refused}, retained={retained}, lock_kept={lock_kept}"
    );
    assert!(!crate::sqlite_file::foreign_lock_refused(&path));
}

#[cfg(unix)]
#[test]
fn reused_manifest_and_backup_inputs_use_positional_reads_without_new_descriptors() {
    use std::io::{Seek, SeekFrom};
    if isolated("reused_manifest_and_backup_inputs_use_positional_reads_without_new_descriptors") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let location = old_without_wal(&temp.path().canonicalize().unwrap().join("state/desktop"));
    let owner = location.lock().unwrap();
    let handoff = location.handoff_lock().unwrap();
    let original = Original::new(&location, &owner, &handoff).unwrap();
    original.prepare(Some(34)).unwrap();
    let backup = original.root.child(BACKUP).unwrap();
    let count = super::super::startup::retained_count();
    let first = Input::open(&backup, MANIFEST).unwrap().unwrap();
    let second = Input::open(&backup, MANIFEST).unwrap().unwrap();
    assert!(Arc::ptr_eq(&first.file, &second.file));
    let mut cursor = first.file.as_ref();
    cursor.seek(SeekFrom::End(0)).unwrap();
    original.prepare(Some(34)).unwrap();
    original.prepare(Some(34)).unwrap();
    assert_eq!(super::super::startup::retained_count(), count);
    assert_eq!(
        schema(&Connection::open(location.root.join(DATABASE)).unwrap()),
        34
    );
}
