//! Actual catalog transactions; no worktree is removed or loaned to Git.
use super::*;
use crate::platform::residual_lock_tests as locks;

#[test]
fn catalog_success_releases_while_a_sibling_retains_its_fd() {
    locks::control(
        "state::worktrees::residual_lock_tests::fixture",
        "catalog-success",
    );
}
#[test]
fn catalog_error_releases_while_a_sibling_retains_its_fd() {
    locks::control(
        "state::worktrees::residual_lock_tests::fixture",
        "catalog-error",
    );
}
#[test]
#[ignore = "private exact original with pipe-gated fork"]
fn fixture() {
    let Ok(case) = std::env::var("ZERUS_WORKTREE_LOCK_CONTROL") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "base",
    ]);
    let common = crate::platform::canonical(temp.path().join(".git")).unwrap();
    let lock = cache_file("repo", &common.to_string_lossy()).with_extension("lock");
    let head = common.join("HEAD");
    let failed = case == "catalog-error";
    let held = locks::capture("catalog-acquired", &lock, move || {
        if failed {
            fs::remove_file(head).unwrap();
        }
    });
    let published = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = published.clone();
    let path = lock.clone();
    if !failed {
        locks::arm("catalog-publish", move || {
            locks::busy(&path);
            seen.set(true);
        });
    }
    let value = catalog(temp.path(), true);
    if failed {
        assert_ne!(value["state"], "ok");
    } else {
        assert_eq!(value["state"], "ok");
        assert!(published.get());
    }
    locks::released(held, &lock);
}
