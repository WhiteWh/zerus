use super::*;

#[cfg(unix)]
#[test]
fn a_symlinked_parent_keeps_no_follow_database_opens_working() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("physical");
    fs::create_dir(&real).unwrap();
    let alias = temp.path().join("alias");
    symlink(&real, &alias).unwrap();
    let state = alias.join("state");
    let catalog = Catalog::open(&state, true).unwrap();
    let record = value(1, "conversation", "run", "", 1.0);
    put(&catalog, Source::Terminal, &record);
    let mut migration = migration::Migration::new(&state).unwrap();
    while !migration.step().unwrap() {}
    let reader = Catalog::open(&state, false).unwrap();
    let page = reader
        .page(&scope("conversation", "run", ""), "", 100, true)
        .unwrap();
    assert_eq!(names(&page), vec![string(&record, "request_id")]);
    assert_eq!(page.metadata["catalog_complete_at_snapshot"], true);
}

#[test]
fn sqlite_diagnostics_are_bounded_and_do_not_return_paths_or_payloads() {
    let error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(14 | (6 << 8)),
        Some("fixture-private-path and secret-payload".into()),
    );
    let message = sql::<()>(Err(error)).unwrap_err();
    assert!(message.starts_with("Receipt index is unavailable"));
    assert!(message.contains("CannotOpen"));
    assert!(message.contains("1550"));
    assert!(!message.contains("fixture-private-path"));
    assert!(!message.contains("secret-payload"));
    assert!(message.len() < 128);
}

#[cfg(unix)]
#[test]
fn canonical_ancestors_never_follow_a_state_or_database_leaf_link() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let state_link = temp.path().join("state-link");
    symlink(&outside, &state_link).unwrap();
    assert!(Catalog::open(&state_link, true).is_err());
    assert!(!outside.join(DATABASE).exists());
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    let original = temp.path().join("outside.sqlite3");
    fs::write(&original, b"fixture-kept").unwrap();
    symlink(&original, state.join(DATABASE)).unwrap();
    assert!(Catalog::open(&state, true).is_err());
    assert!(Catalog::open(&state, false).is_err());
    assert_eq!(fs::read(&original).unwrap(), b"fixture-kept");
    assert!(!temp.path().join("outside.sqlite3-wal").exists());
    assert!(!temp.path().join("outside.sqlite3-shm").exists());
}
