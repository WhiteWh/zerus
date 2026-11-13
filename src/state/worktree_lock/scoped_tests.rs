//! Exercise the actual scoped holders in the existing private Git fixture.
use super::*;
use crate::platform::lock_tests as probe;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

macro_rules! cases {
    ($($name:ident => $case:literal),* $(,)?) => { $(
        #[test]
        fn $name() { probe::control("state::worktree_lock::tests::fixture", $case); }
    )* };
}
cases! {
    new_publication_unlocks_with_a_retained_fork => "scoped_new_ok",
    new_publication_error_before_self_unlocks_with_a_retained_fork => "scoped_new_error",
    v1_retention_unlocks_with_a_retained_fork => "scoped_retained_ok",
    v1_retention_error_before_self_unlocks_with_a_retained_fork => "scoped_retained_error",
    legacy_preparation_unlocks_with_a_retained_fork => "scoped_move_ok",
    legacy_preparation_error_before_self_unlocks_with_a_retained_fork => "scoped_move_error",
    v2_retention_unlocks_with_a_retained_fork => "scoped_legacy_ok",
    v2_retention_error_before_self_unlocks_with_a_retained_fork => "scoped_legacy_error",
    dsh_handoff_holds_the_provider_through_transfer_then_unlocks => "scoped_provider_dsh_new_ok",
    dsh_handoff_error_unlocks_the_provider => "scoped_provider_dsh_new_error",
    marked_dsh_handoff_holds_the_provider_through_transfer_then_unlocks => "scoped_provider_dsh_marked_ok",
    marked_dsh_handoff_error_unlocks_the_provider => "scoped_provider_dsh_marked_error",
    opencode_handoff_holds_the_provider_through_transfer_then_unlocks => "scoped_provider_opencode_new_ok",
    opencode_handoff_error_unlocks_the_provider => "scoped_provider_opencode_new_error",
    marked_opencode_handoff_holds_the_provider_through_transfer_then_unlocks => "scoped_provider_opencode_marked_ok",
    marked_opencode_handoff_error_unlocks_the_provider => "scoped_provider_opencode_marked_error",
}

pub(super) fn run(
    case: &str,
    main: &Path,
    checkout: &Path,
    common: &Path,
    record: &Value,
    veto: &Path,
) {
    fs::set_permissions(root(), fs::Permissions::from_mode(0o700)).unwrap();
    if case.starts_with("scoped_cleanup_") {
        worktree_cleanup::scoped_tests::run(case, checkout, common);
        return;
    }
    let _writer = storage::lock(None).unwrap();
    let repo_lock = super::super::worktrees::cache_file("repo", &common.to_string_lossy())
        .with_extension("lock");
    if case.starts_with("scoped_provider_") {
        provider(case, record, veto);
        return;
    }
    let mut current = record.clone();
    let kind = case.split('_').nth(1).unwrap();
    let failed = case.ends_with("error");
    if kind == "move" || kind == "legacy" {
        atomic(
            &storage::legacy_record_path(string(record, "name")),
            &record.to_string(),
        )
        .unwrap();
        // Let real Git create the actual original advisory veto, including its
        // ordinary 0644 mode, under this private fixture's temporary umask.
        let old = unsafe { libc::umask(0o022) };
        let output = Command::new("git")
            .args([
                "worktree",
                "lock",
                "--reason",
                &format!("zerus:{}", string(record, "session_id")),
            ])
            .arg(checkout)
            .current_dir(main)
            .output()
            .unwrap();
        unsafe {
            libc::umask(old);
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if kind == "legacy" {
            drop(Protection::for_legacy_move(&mut current).unwrap().unwrap());
        }
    } else if kind == "retained" {
        drop(Protection::acquire(record).unwrap().unwrap());
        current["worktree_protection"] = current["worktree_owner"]["id"].clone();
    }
    let original = veto.exists().then(|| {
        let identity = crate::platform::file_identity(veto).unwrap();
        ((identity.device, identity.inode), fs::read(veto).unwrap())
    });
    let label = match kind {
        "new" => "protection-new",
        "retained" => "protection-retained",
        "move" => "protection-move",
        "legacy" => "protection-legacy",
        _ => panic!("{case}"),
    };
    let receipt = root().join("worktree-protections").join(format!(
        "{}.json",
        storage::label_key(string(&current["worktree_owner"], "id"))
    ));
    // Wrong expected SID is checked after the v2 lock is taken, before Self
    // exists; do not replace or temporarily remove the original Git veto.
    if kind == "legacy" && failed {
        current["session_id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    let corrupt_receipt = kind == "retained";
    let kind_is_writer = kind == "new" || kind == "move";
    let slot = probe::capture(label, &repo_lock, move || {
        if failed {
            if corrupt_receipt {
                let mut proof: Value =
                    serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
                proof["status"] = json!("preparing");
                fs::write(receipt, proof.to_string()).unwrap();
            } else if kind_is_writer {
                storage::cut::arm(0);
            }
        }
    });
    let result = match kind {
        "new" => Protection::acquire(&current),
        "move" => Protection::for_legacy_move(&mut current),
        "retained" => protection::existing(&current),
        "legacy" => protection::existing(&current),
        _ => unreachable!(),
    };
    storage::cut::disarm();
    if failed {
        assert!(
            result.is_err(),
            "expected the existing post-acquisition refusal"
        );
    } else {
        result.as_ref().unwrap().as_ref().unwrap().verify().unwrap();
    }
    drop(result);
    probe::released(slot, &repo_lock);
    if let Some((identity, bytes)) = original {
        let after = crate::platform::file_identity(veto).unwrap();
        assert_eq!(identity, (after.device, after.inode));
        assert_eq!(fs::read(veto).unwrap(), bytes);
    } else if failed {
        assert!(
            !veto.exists(),
            "a refused new publication cannot create a veto"
        );
    } else {
        assert_eq!(
            fs::read_to_string(veto).unwrap(),
            format!("zerus:{}\n", string(record, "session_id"))
        );
    }
}

fn provider(case: &str, record: &Value, veto: &Path) {
    let provider = case.split('_').nth(2).unwrap();
    let mut legacy = record.clone();
    let id = uuid::Uuid::new_v4().to_string();
    legacy["session_id"] = json!(id);
    legacy["run_id"] = json!("legacy-run");
    legacy["name"] = json!(format!("{provider}/example/legacy"));
    legacy["backend"] = json!(provider);
    legacy["conversation_id"] = json!(if provider == "dsh" {
        "session-native"
    } else {
        "ses_native"
    });
    let marked = case.contains("_marked_");
    if marked {
        legacy["worktree_protection"] = record["worktree_owner"]["id"].clone();
    }
    let binding = root().join(provider).join("bindings").join(format!(
        "{}.json",
        storage::label_key(string(&legacy, "name"))
    ));
    atomic(&binding, &legacy.to_string()).unwrap();
    let protection = Protection::acquire(record).unwrap().unwrap();
    let identity = crate::platform::file_identity(veto).unwrap();
    let holder = worktree_cleanup::bindings()
        .unwrap()
        .into_iter()
        .find(|r| r.session_id == id)
        .unwrap();
    let provider_lock = root().join(provider).join("bindings.lock");
    let failed = case.ends_with("error");
    let slot = probe::capture("provider-acquired", &provider_lock, move || {
        if failed {
            storage::cut::arm(0);
        }
    });
    let path = provider_lock.clone();
    probe::arm("provider-transfer", move || probe::busy(&path));
    let result = protection.transfer(&holder);
    storage::cut::disarm();
    if failed {
        assert!(result.is_err());
    } else {
        result.unwrap();
    }
    probe::released(slot, &provider_lock);
    let after = crate::platform::file_identity(veto).unwrap();
    assert_eq!(
        (identity.device, identity.inode),
        (after.device, after.inode)
    );
    assert_eq!(
        fs::read_to_string(veto).unwrap(),
        format!(
            "zerus:{}\n",
            if failed {
                string(record, "session_id")
            } else {
                &id
            }
        )
    );
    let saved: Value = serde_json::from_slice(&fs::read(binding).unwrap()).unwrap();
    let mut expected = legacy;
    if !failed {
        expected["worktree_protection"] = record["worktree_owner"]["id"].clone();
    }
    assert_eq!(saved, expected);
}
