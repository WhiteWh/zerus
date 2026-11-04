use super::*;
use crate::readmarks::ReadMark;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "read-checkpoint-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf {
        self.0.join("marks.json")
    }
    #[cfg(unix)]
    fn write(&self, bytes: &[u8]) {
        crate::fsx::write_private_atomic(&self.path(), bytes, false).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn mark(name: &str) -> ReadMark {
    ReadMark {
        computer: "".into(),
        name: name.into(),
        run_id: "r1".into(),
        conversation_id: "c1".into(),
        reply_id: Some("reply".into()),
        attention_signature: None,
    }
}

#[cfg(unix)]
#[test]
fn new_and_unsaved_marks_checkpoint_in_the_existing_format_and_reload() {
    let temp = Temp::new();
    let mut store = ReadMarks::load(temp.path());
    let empty = store.checkpoint().unwrap();
    assert_eq!((empty.bytes, empty.marks), (2, 0));
    assert_eq!(empty.sha256, sha256(b"[]"));
    assert!(store.add_all([mark("one"), mark("two")]));
    let saved = store.checkpoint().unwrap();
    assert_eq!(saved.marks, 2);
    assert_eq!(ReadMarks::load(temp.path()).pending(), store.pending());
    assert_eq!(saved.sha256, sha256(&std::fs::read(temp.path()).unwrap()));
    assert!(!store.unsaved);
}

#[cfg(unix)]
#[test]
fn unchanged_source_bytes_and_unknown_fields_are_synced_without_rewrite() {
    use std::os::unix::fs::MetadataExt;
    let temp = Temp::new();
    let bytes = br#"[ {"computer":"","name":"one","run_id":"r1","conversation_id":"c1","reply_id":"reply","attention_signature":null,"future":{"kept":true}} ]"#;
    temp.write(bytes);
    let before = std::fs::metadata(temp.path()).unwrap().ino();
    let mut store = ReadMarks::load(temp.path());
    assert_eq!(store.checkpoint().unwrap().sha256, sha256(bytes));
    assert_eq!(std::fs::read(temp.path()).unwrap(), bytes);
    assert_eq!(std::fs::metadata(temp.path()).unwrap().ino(), before);
}

#[cfg(unix)]
#[test]
fn failed_ordinary_save_remains_dirty_for_the_checked_checkpoint() {
    let temp = Temp::new();
    let path = temp.0.join("parent/marks.json");
    let mut store = ReadMarks::load(path.clone());
    std::fs::write(temp.0.join("parent"), b"blocker").unwrap();
    store.add(mark("one")); // Legacy API still logs, rather than returning an error.
    assert!(!store.unsaved);
    assert!(store.checkpoint.dirty);
    assert!(store.checkpoint().is_err());
    std::fs::remove_file(temp.0.join("parent")).unwrap();
    assert_eq!(store.checkpoint().unwrap().marks, 1);
    assert_eq!(ReadMarks::load(path).pending(), [mark("one")]);
}

#[cfg(unix)]
#[test]
fn directory_sync_failure_is_not_a_receipt_even_after_the_rename() {
    let temp = Temp::new();
    let mut store = ReadMarks::load(temp.path());
    store.add_all([mark("one")]);
    let error = store
        .checkpoint_with(|_| Err(io::ErrorKind::PermissionDenied.into()))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let bytes = std::fs::read(temp.path()).unwrap();
    assert_eq!(ReadMarks::load(temp.path()).pending(), [mark("one")]);
    assert_eq!(store.checkpoint().unwrap().sha256, sha256(&bytes));
    assert_eq!(std::fs::read(temp.path()).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn malformed_or_unreadable_initial_sources_never_become_an_empty_checkpoint() {
    let temp = Temp::new();
    temp.write(b"not json");
    let mut store = ReadMarks::load(temp.path());
    assert!(
        store.pending().is_empty(),
        "legacy loading behavior stays compatible"
    );
    assert!(store.checkpoint().is_err());
    assert_eq!(std::fs::read(temp.path()).unwrap(), b"not json");
    store.add(mark("one"));
    assert!(
        store.checkpoint().is_err(),
        "an ordinary save does not erase an uncertain initial load"
    );
    let dir = temp.0.join("directory");
    std::fs::create_dir(&dir).unwrap();
    assert!(ReadMarks::load(dir.clone()).checkpoint().is_err());
    assert!(dir.is_dir());
}

#[cfg(unix)]
#[test]
fn a_changed_or_new_source_is_not_overwritten() {
    let temp = Temp::new();
    let mut missing = ReadMarks::load(temp.path());
    temp.write(b"[ ]");
    assert!(missing.checkpoint().is_err());
    let mut store = ReadMarks::load(temp.path());
    store.add_all([mark("one")]);
    temp.write(b"[]");
    assert!(store.checkpoint().is_err());
    assert_eq!(std::fs::read(temp.path()).unwrap(), b"[]");
}

#[cfg(unix)]
#[test]
fn public_linked_or_symlinked_leaves_refuse_without_mutation() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = Temp::new();
    temp.write(b"[]");
    let mut store = ReadMarks::load(temp.path());
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.checkpoint().is_err());
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let other = temp.0.join("other");
    std::fs::hard_link(temp.path(), &other).unwrap();
    assert!(store.checkpoint().is_err());
    std::fs::remove_file(temp.path()).unwrap();
    symlink(&other, temp.path()).unwrap();
    assert!(store.checkpoint().is_err());
    assert_eq!(std::fs::read(other).unwrap(), b"[]");
}

#[test]
fn serialization_stops_at_the_exact_byte_limit_without_an_unbounded_reference_list() {
    let temp = Temp::new();
    let mut store = ReadMarks::load(temp.path());
    store.add_all([mark("one"), mark("two")]);
    let expected = serde_json::to_vec(&store.pending()).unwrap();
    assert_eq!(serialized(&store, expected.len()).unwrap(), expected);
    assert!(serialized(&store, expected.len() - 1).is_err());
    assert!(!temp.path().exists());
}

#[cfg(unix)]
#[test]
fn row_and_file_limits_refuse_before_replacing_the_last_checkpoint() {
    let temp = Temp::new();
    let mut store = ReadMarks::load(temp.path());
    store.add_all((0..CHECKPOINT_MARKS).map(|i| mark(&format!("s{i}"))));
    let receipt = store.checkpoint().unwrap();
    assert_eq!(receipt.marks, CHECKPOINT_MARKS);
    store.add_all([mark("too-many")]);
    assert!(store
        .checkpoint()
        .unwrap_err()
        .to_string()
        .contains("row limit"));
    assert_eq!(sha256(&std::fs::read(temp.path()).unwrap()), receipt.sha256);
    let mut store = ReadMarks::load(temp.path());
    OpenOptions::new()
        .write(true)
        .open(temp.path())
        .unwrap()
        .set_len((CHECKPOINT_BYTES + 1) as u64)
        .unwrap();
    assert!(store
        .checkpoint()
        .unwrap_err()
        .to_string()
        .contains("bounds"));
    assert_eq!(
        std::fs::metadata(temp.path()).unwrap().len(),
        (CHECKPOINT_BYTES + 1) as u64
    );
}

#[cfg(unix)]
#[test]
fn initial_row_bounds_count_original_entries_before_key_coalescing() {
    let temp = Temp::new();
    let bytes = serde_json::to_vec(&vec![mark("same"); CHECKPOINT_MARKS + 1]).unwrap();
    assert!(bytes.len() < CHECKPOINT_BYTES);
    temp.write(&bytes);
    let mut store = ReadMarks::load(temp.path());
    assert_eq!(store.pending().len(), 1);
    assert!(store.checkpoint().is_err());
    assert_eq!(std::fs::read(temp.path()).unwrap(), bytes);
}

#[cfg(not(any(unix, windows)))]
#[test]
fn unsupported_platform_refuses_before_creating_any_checkpoint_state() {
    let temp = Temp::new();
    let mut store = ReadMarks::load(temp.path());
    assert_eq!(
        store.checkpoint().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(!temp.path().exists());
}
