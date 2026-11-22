//! Explicit filesystem faults; the actual intent publication/rollback writers
//! produce the files. Payload bytes are synthetic metadata, not catalog rows.
use super::*;

fn fixture() -> (tempfile::TempDir, Directory, Directory) {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(&temp.path().canonicalize().unwrap()).unwrap();
    let queue = root.ensure_child(QUEUE).unwrap();
    (temp, root, queue)
}

#[test]
fn a_post_rename_sync_failure_removes_only_this_attempts_intent() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            repositories::directory::fail_publication_sync(None);
        }
    }
    let (_temp, root, queue) = fixture();
    let _reset = Reset;
    repositories::directory::fail_publication_sync(Some(queue.path.clone()));
    let error = publish_intent(
        &root,
        &queue,
        "attempt.json",
        br#"{"nonce":"owned-attempt"}"#,
    )
    .unwrap_err();
    assert!(error.contains("fixture directory sync failure"), "{error}");
    assert_eq!(fs::read_dir(&queue.path).unwrap().count(), 0);
}

#[test]
fn rollback_uses_the_original_parent_and_keeps_a_replacement_queue() {
    let (_temp, root, queue) = fixture();
    let bytes = br#"{"nonce":"owned-attempt"}"#;
    queue.replace_file("attempt.json", bytes).unwrap();
    let original = root.path.join("original-queue");
    fs::rename(&queue.path, &original).unwrap();
    let replacement = root.ensure_child(QUEUE).unwrap();
    replacement
        .replace_file("attempt.json", b"unrelated")
        .unwrap();
    discard(&queue, "attempt.json", bytes).unwrap();
    assert_eq!(fs::read_dir(original).unwrap().count(), 0);
    assert_eq!(
        replacement.read_file("attempt.json", 64).unwrap(),
        b"unrelated"
    );
}

#[test]
fn rollback_never_removes_an_unrelated_intent() {
    let (_temp, _root, queue) = fixture();
    queue.replace_file("attempt.json", b"unrelated").unwrap();
    assert!(discard(&queue, "attempt.json", b"owned")
        .unwrap_err()
        .contains("retained"));
    assert_eq!(queue.read_file("attempt.json", 64).unwrap(), b"unrelated");
}
