use super::super::store::Store;
use super::*;
use zerus_desktop_migration::{canonical, Handoff, ImportRecordRef, HANDOFF_FILE};

fn location(root: &std::path::Path) -> Location {
    Location {
        root: canonical(&root.join("state/desktop")).unwrap(),
        configuration: "a".repeat(64),
    }
}
#[test]
fn absent_state_stays_absent_and_does_not_claim_an_existing_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(temp.path());
    let held = Startup::hold(&location).unwrap();
    assert_eq!(held.configuration(), location.configuration);
    assert!(held._lock.is_none());
    assert!(!location.root.parent().unwrap().exists());
    drop(held);
    assert!(!location.root.parent().unwrap().exists());
}

#[test]
fn known_existing_passive_state_holds_the_actual_handoff_lock_until_drop() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(temp.path());
    location.prepare().unwrap();
    let _store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    let held = Startup::hold(&location).unwrap();
    assert!(held._lock.is_some());
    assert!(location.handoff_lock().is_err());
    assert!(Startup::hold(&location).is_err());
    drop(held);
    assert!(location.handoff_lock().is_ok());
    assert!(Startup::hold(&location).is_ok());
}

#[test]
fn incomplete_or_unknown_state_is_not_absence_and_creates_no_lock() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(temp.path());
    location.prepare().unwrap();
    assert!(Startup::hold(&location).is_err());
    assert!(!location.root.join("handoff.lock").exists());
    let store = Store::open(&location, &uuid::Uuid::new_v4().to_string()).unwrap();
    store.db.execute_batch("PRAGMA user_version=999").unwrap();
    assert!(Startup::hold(&location).is_err());
    assert!(!location.root.join("handoff.lock").exists());
}

#[test]
fn active_policy_and_handoff_markers_cannot_admit_legacy_startup() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(temp.path());
    location.prepare().unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    let mut store = Store::open(&location, &epoch).unwrap();
    zerus_desktop_ledger::authorize(&mut store.db, &uuid::Uuid::new_v4().to_string(), &epoch)
        .unwrap();
    assert!(Startup::hold(&location).is_err());
    zerus_desktop_ledger::revoke(&store.db, "manual_revoke").unwrap();
    let mut marker = Handoff {
        version: 1,
        configuration: location.configuration.clone(),
        source: "com.hgdev.hgs-tray".into(),
        import_id: uuid::Uuid::new_v4().to_string(),
        writer_epoch: uuid::Uuid::new_v4().to_string(),
        phase: Phase::Imported,
        import_record: ImportRecordRef {
            version: 1,
            sha256: "b".repeat(64),
        },
        automatic_approvals: false,
    };
    for phase in [Phase::Imported, Phase::Quiesced, Phase::Complete] {
        marker.phase = phase;
        let bytes = serde_json::to_vec(&marker).unwrap();
        std::fs::write(location.root.join(HANDOFF_FILE), &bytes).unwrap();
        crate::platform::make_private(
            &std::fs::File::open(location.root.join(HANDOFF_FILE)).unwrap(),
        )
        .unwrap();
        assert!(Startup::hold(&location).is_err());
        assert_eq!(
            std::fs::read(location.root.join(HANDOFF_FILE)).unwrap(),
            bytes
        );
    }
    marker.phase = Phase::Revoked;
    std::fs::write(
        location.root.join(HANDOFF_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    assert!(Startup::hold(&location).is_ok());
    std::fs::write(location.root.join(HANDOFF_FILE), b"unsupported").unwrap();
    assert!(Startup::hold(&location).is_err());
}

#[test]
fn linked_state_is_preserved_and_never_creates_a_boundary_in_its_target() {
    let temp = tempfile::tempdir().unwrap();
    let location = location(temp.path());
    let target = temp.path().join("foreign");
    crate::platform::private_dir(&target).unwrap();
    crate::platform::private_dir(location.root.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &location.root).unwrap();
    assert!(Startup::hold(&location).is_err());
    assert!(location.root.is_symlink());
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
}
