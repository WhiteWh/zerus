//! Initialization excludes only the ordinary anchor transaction.
use super::*;
use crate::platform::residual_lock_tests as locks;
use std::os::unix::fs::OpenOptionsExt;

#[test]
fn initialization_success_releases_while_a_sibling_retains_its_fd() {
    locks::control(
        "state::worktree_requests::store::residual_lock_tests::fixture",
        "init-success",
    );
}
#[test]
fn invalid_anchor_releases_while_a_sibling_retains_its_fd() {
    locks::control(
        "state::worktree_requests::store::residual_lock_tests::fixture",
        "init-anchor",
    );
}
#[test]
fn replaced_lock_name_releases_the_original_object_on_error() {
    locks::control(
        "state::worktree_requests::store::residual_lock_tests::fixture",
        "init-name",
    );
}
fn write_private(path: &Path, text: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(text)
        .unwrap();
}
#[test]
#[ignore = "private exact original with pipe-gated fork"]
fn fixture() {
    let Ok(case) = std::env::var("ZERUS_WORKTREE_LOCK_CONTROL") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    crate::platform::restrict_dir(root).unwrap();
    if case == "init-anchor" {
        write_private(&root.join(ANCHOR), b"{");
    }
    let lock = root.join(STORE_LOCK);
    let original = if case == "init-name" {
        root.join("retired.lock")
    } else {
        lock.clone()
    };
    let from = lock.clone();
    let to = original.clone();
    let replace = case == "init-name";
    let held = locks::capture("request-init", &lock, move || {
        if replace {
            std::fs::rename(&from, to).unwrap();
            write_private(&from, b"");
        }
    });
    let result = Store::open(root, true);
    match case.as_str() {
        "init-success" => {
            assert!(
                result.is_ok(),
                "initialization failed: {:?}",
                result.as_ref().err()
            );
            assert!(root.join(ANCHOR).is_file());
        }
        "init-anchor" => assert!(result
            .err()
            .unwrap()
            .contains("Unreadable worktree receipt store identity")),
        "init-name" => assert!(result
            .err()
            .unwrap()
            .contains("Worktree receipt object changed")),
        _ => panic!("unknown private fixture"),
    }
    locks::released(held, &original);
}
