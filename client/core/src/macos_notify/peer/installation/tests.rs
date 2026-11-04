//! Negative/native and pure data checks only. No genuine self/pair positive.
use super::*;
use std::{
    os::unix::fs::DirBuilderExt,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) struct Tree(pub(super) PathBuf, PathBuf);
impl Tree {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self::from_candidates(|| {
            std::env::temp_dir().join(format!(
                "hgs-pair-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        })
    }

    fn from_candidates(mut candidate: impl FnMut() -> PathBuf) -> Self {
        for _ in 0..128 {
            let root = candidate();
            match std::fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private pair fixture: {error}"),
            }
            // Reserve the root exclusively BEFORE creating any descendants.
            // Equal clock samples or stale names must never share a fixture.
            let root = std::fs::canonicalize(root).unwrap();
            let tree = Self(root.join("Example.app"), root);
            tree.populate();
            return tree;
        }
        panic!("private pair fixture namespace exhausted");
    }

    fn populate(&self) {
        std::fs::create_dir_all(self.0.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(self.0.join("Contents/_CodeSignature")).unwrap();
        for name in [
            "MacOS/hgs",
            "MacOS/hgs-tray",
            "Info.plist",
            "_CodeSignature/CodeResources",
        ] {
            std::fs::write(self.0.join("Contents").join(name), b"synthetic, not signed").unwrap();
        }
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.1).unwrap();
    }
}

#[test]
fn occupied_fixture_candidate_is_skipped_without_reusing_or_deleting_it() {
    let original = Tree::new();
    let fresh = original.1.with_extension("collision-control");
    assert!(!fresh.exists());
    let mut attempts = 0;
    let other = Tree::from_candidates(|| {
        attempts += 1;
        if attempts == 1 {
            original.1.clone()
        } else {
            fresh.clone()
        }
    });
    assert_eq!(attempts, 2);
    assert_eq!(other.1, fresh);
    drop(other);
    assert!(!fresh.exists());
    assert!(original.0.join("Contents/MacOS/hgs").is_file());
}

#[test]
fn concurrent_fixtures_have_distinct_roots_and_independent_cleanup() {
    use std::{
        collections::HashSet,
        sync::{Arc, Barrier},
    };
    let start = Arc::new(Barrier::new(16));
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                Tree::new()
            })
        })
        .collect();
    let mut trees: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        trees
            .iter()
            .map(|tree| &tree.1)
            .collect::<HashSet<_>>()
            .len(),
        16
    );
    let first = trees.pop().unwrap();
    let root = first.1.clone();
    drop(first);
    assert!(!root.exists());
    assert!(trees
        .iter()
        .all(|tree| tree.0.join("Contents/MacOS/hgs").is_file()));
}
pub(super) fn fake_signature(bundle: &Path, app: bool) -> Signature {
    Signature {
        identifier: if app { APP_ID } else { "example.service" }.into(),
        team: "A".repeat(10), // Synthetic metadata; never native trust evidence.
        executable: bundle.join(if app {
            "Contents/MacOS/hgs-tray"
        } else {
            "Contents/MacOS/hgs"
        }),
        unique: vec![if app { 1 } else { 2 }; 20],
        requirement: "synthetic-only".into(),
    }
}
fn fake_snapshot(tree: &Tree) -> Snapshot {
    Snapshot {
        bundle: tree.0.clone(),
        generation: generation(&tree.0).unwrap(),
        app: fake_signature(&tree.0, true),
        service: fake_signature(&tree.0, false),
        self_executable: tree.0.join("Contents/MacOS/hgs"),
    }
}

#[test]
fn routing_metadata_cache_refuses_a_changed_helper_without_rehashing_each_page() {
    // Modelled verified snapshot: no native positive signature is claimed.
    let tree = Tree::new();
    let selection = Selection {
        original: fake_snapshot(&tree),
    };
    let service = tree.0.join("Contents/MacOS/hgs");
    for _ in 0..100 {
        assert_eq!(
            selection.service_executable_if_unchanged().unwrap(),
            service
        );
    }
    std::fs::write(&service, b"changed generation").unwrap();
    assert!(selection.service_executable_if_unchanged().is_err());
}

#[test]
fn ancestor_churn_keeps_routing_and_full_snapshot_comparison_current() {
    // Actual filesystem changes, modelled already-verified signatures. This
    // exercises the same generation comparison used by full revalidate(),
    // without claiming a genuine running-self/service positive in libtest.
    let tree = Tree::new();
    let before = fake_snapshot(&tree);
    let selection = Selection {
        original: fake_snapshot(&tree),
    };
    for directory in [
        tree.0.parent().unwrap(),
        tree.0.parent().unwrap().parent().unwrap(),
    ] {
        let name = tree
            .0
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        let source = directory.join(format!("{name}-churn"));
        let renamed = directory.join(format!("{name}-renamed"));
        std::fs::write(&source, b"unrelated atomic update").unwrap();
        assert!(selection.service_executable_if_unchanged().is_ok());
        assert!(same_generation(&before, &fake_snapshot(&tree)).is_ok());
        std::fs::rename(&source, &renamed).unwrap();
        assert!(selection.service_executable_if_unchanged().is_ok());
        assert!(same_generation(&before, &fake_snapshot(&tree)).is_ok());
        std::fs::remove_file(&renamed).unwrap();
    }
    std::fs::write(tree.0.join("Contents/MacOS/hgs"), b"changed helper").unwrap();
    assert!(selection.service_executable_if_unchanged().is_err());
    assert!(same_generation(&before, &fake_snapshot(&tree)).is_err());
}

#[test]
fn bundle_last_used_xattr_does_not_change_the_generation() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let tree = Tree::new();
    let before = fake_snapshot(&tree);
    let selection = Selection {
        original: fake_snapshot(&tree),
    };
    let path = CString::new(tree.0.as_os_str().as_bytes()).unwrap();
    let name = CString::new("com.apple.lastuseddate#PS").unwrap();
    let value = [0u8; 16];
    // SAFETY: fixed bounded owned scratch path/name/value; no installed app,
    // LaunchServices, signing, permissions or owner GUI is contacted.
    assert_eq!(
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        },
        0
    );
    assert!(selection.service_executable_if_unchanged().is_ok());
    assert!(same_generation(&before, &fake_snapshot(&tree)).is_ok());
}

#[test]
fn replaced_ancestor_still_invalidates_the_same_bundle() {
    let tree = Tree::new();
    let selection = Selection {
        original: fake_snapshot(&tree),
    };
    let parent = tree.0.parent().unwrap();
    let moved = parent.with_extension("old");
    assert!(!moved.exists());
    std::fs::rename(parent, &moved).unwrap();
    std::fs::create_dir(parent).unwrap();
    std::fs::rename(moved.join("Example.app"), &tree.0).unwrap();
    std::fs::remove_dir(&moved).unwrap();
    assert!(selection.service_executable_if_unchanged().is_err());
}

#[test]
fn only_exact_own_bundle_layouts_are_candidates_not_bare_or_source_paths() {
    for name in ["hgs", "hgs-tray"] {
        let p = PathBuf::from("/example/Example.app/Contents/MacOS").join(name);
        assert_eq!(layout(&p).unwrap(), Path::new("/example/Example.app"));
    }
    for p in [
        "/example/bin/hgs",
        "target/debug/hgs",
        "/example/Example.app/hgs",
        "/example/Example.app/Contents/MacOS/other",
        "/example/Example/Contents/MacOS/hgs",
        "/example/Example.app/Contents/MacOS/../MacOS/hgs",
    ] {
        assert!(
            matches!(layout(Path::new(p)), Err(Refusal::UnsupportedLayout)),
            "{p}"
        );
    }
}

#[test]
fn real_native_bare_adhoc_test_process_cannot_select_native_authority() {
    // The ordinary compiled libtest is outside a genuine paired sealed app.
    // No installed executable, signing identity, keychain or GUI is consulted.
    assert!(select_own().is_err());
}

#[test]
fn private_exact_child_at_app_layout_still_refuses_adhoc_self() {
    const MARKER: &str = "ZERUS_MAC_INSTALLATION_TEST_CHILD";
    if std::env::var_os(MARKER).is_some() {
        assert!(layout(&std::env::current_exe().unwrap()).is_ok());
        assert!(select_own().is_err());
        return;
    }
    let tree = Tree::new();
    let own = std::env::current_exe().unwrap();
    let target = tree.0.join("Contents/MacOS/hgs-tray");
    std::fs::copy(&own, &target).unwrap();
    std::fs::copy(&own, tree.0.join("Contents/MacOS/hgs")).unwrap();
    let root = tree.0.parent().unwrap();
    let mut command = std::process::Command::new(&target);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env(MARKER, "1");
    for (key, leaf) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("HGS_CONFIG_DIR", "hgs-config"),
        ("HGS_STATE_DIR", "hgs-state"),
        ("TMPDIR", "tmp"),
        ("TMUX_TMPDIR", "tmux"),
    ] {
        let dir = root.join(leaf);
        std::fs::create_dir(&dir).unwrap();
        command.env(key, dir);
    }
    let result = command.args([
        "--exact",
        "macos_notify::peer::installation::tests::private_exact_child_at_app_layout_still_refuses_adhoc_self",
        "--nocapture",
    ]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("1 passed; 0 failed; 0 ignored"), "{output}");
    println!("EXACT PRIVATE CHILD: {}", output.trim());
}

#[test]
fn actual_synthetic_unsigned_pair_fails_security_even_with_fixed_paths() {
    let tree = Tree::new();
    autoreleasepool(|_| {
        let anchor = super::super::requirement(APPLE).unwrap();
        assert!(static_signature(&tree.0, anchor.as_ptr()).is_err());
        assert!(static_signature(&tree.0.join("Contents/MacOS/hgs"), anchor.as_ptr()).is_err());
    });
}

#[test]
fn symlink_missing_and_nonregular_fixed_components_refuse() {
    let tree = Tree::new();
    let service = tree.0.join("Contents/MacOS/hgs");
    std::fs::remove_file(&service).unwrap();
    assert!(generation(&tree.0).is_err());
    std::os::unix::fs::symlink("hgs-tray", &service).unwrap();
    assert!(generation(&tree.0).is_err());
    std::fs::remove_file(&service).unwrap();
    std::fs::create_dir(&service).unwrap();
    assert!(generation(&tree.0).is_err());
}

#[test]
fn actual_file_write_replacement_and_missing_seal_change_the_generation() {
    let tree = Tree::new();
    let before = fake_snapshot(&tree);
    let service = tree.0.join("Contents/MacOS/hgs");
    std::fs::write(&service, b"different bytes and length").unwrap();
    assert!(matches!(
        same_generation(&before, &fake_snapshot(&tree)),
        Err(Refusal::GenerationChanged)
    ));
    let replacement = tree.0.join("Contents/MacOS/replacement");
    std::fs::write(&replacement, b"different bytes and length").unwrap();
    let before = fake_snapshot(&tree);
    std::fs::rename(replacement, &service).unwrap();
    assert!(same_generation(&before, &fake_snapshot(&tree)).is_err());
    std::fs::remove_file(tree.0.join("Contents/_CodeSignature/CodeResources")).unwrap();
    assert!(generation(&tree.0).is_err());
}

#[test]
fn synthetic_pair_mismatch_never_satisfies_the_closed_layout_policy() {
    let tree = Tree::new();
    let app = fake_signature(&tree.0, true);
    for field in 0..5 {
        let mut service = fake_signature(&tree.0, false);
        let mut app = fake_signature(&tree.0, true);
        match field {
            0 => service.team = "B".repeat(10),
            1 => service.executable = tree.0.join("outside-hgs"),
            2 => app.identifier = "example.foreign".into(),
            3 => app.executable = tree.0.join("outside-app"),
            _ => service.unique = app.unique.clone(),
        }
        assert!(pair(&tree.0, &app, &service).is_err());
    }
    assert_eq!(format!("{:?}", Refusal::Signature), "Signature");
    assert_eq!(app.identifier, APP_ID);
}

#[test]
fn changed_selection_members_never_revalidate_from_synthetic_metadata() {
    let tree = Tree::new();
    let before = fake_snapshot(&tree);
    for field in 0..6 {
        let mut after = fake_snapshot(&tree);
        match field {
            0 => after.app.unique[0] ^= 1,
            1 => after.service.requirement.push('x'),
            2 => after.service.team = "B".repeat(10),
            3 => after.self_executable = PathBuf::from("/example/other"),
            4 => after.bundle = PathBuf::from("/example/Other.app"),
            _ => after.generation[0].inode ^= 1,
        }
        assert!(same_generation(&before, &after).is_err());
    }
}
