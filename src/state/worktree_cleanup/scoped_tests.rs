//! Actual cleanup acquisition scopes, never a removal or signal stand-in.
use super::*;
use crate::platform::lock_tests as probe;

macro_rules! cases {
    ($($name:ident => $case:literal),* $(,)?) => { $(
        #[test]
        fn $name() { probe::control("state::worktree_lock::tests::fixture", $case); }
    )* };
}
cases! {
    registered_review_unlocks_with_a_retained_fork => "scoped_cleanup_registered",
    incomplete_listing_unlocks_and_keeps_the_pending_row => "scoped_cleanup_partial",
    failed_listing_unlocks_and_keeps_the_pending_row => "scoped_cleanup_list_error",
    changed_registration_unlocks_without_forgetting_the_row => "scoped_cleanup_changed",
    program_stop_guard_unlocks_with_a_retained_fork => "scoped_cleanup_stop_ok",
    state_writer_refusal_unlocks_repository_before_self_exists => "scoped_cleanup_stop_error",
    final_check_refusal_drops_the_ordinary_program_stop_guard => "scoped_cleanup_stop_final_error",
}

pub(in crate::state) fn run(case: &str, checkout: &Path, common: &Path) {
    let lock = super::super::worktrees::cache_file("repo", &common.to_string_lossy())
        .with_extension("lock");
    if case.contains("_stop_") {
        let capture = zerus_client_core::termination_cleanup::Capture {
            target: zerus_client_core::Target {
                computer: "fixture".into(),
                name: "claude/example/locked".into(),
                run_id: "run-locked".into(),
                conversation_id: "conversation-locked".into(),
            },
            machine: "fixture".into(),
            root: checkout.to_string_lossy().into(),
            common_dir: common.to_string_lossy().into(),
            owner_id: "fixture".into(),
        };
        let writer = (case == "scoped_cleanup_stop_error").then(|| storage::lock(None).unwrap());
        let slot = probe::capture("program-stop", &lock, || {});
        let until = if writer.is_some() {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(5)
        };
        let result = (|| -> Result<()> {
            let _guards = program_stop_guards(&capture, until)?;
            if case == "scoped_cleanup_stop_final_error" {
                return Err("fixture final proof refusal".into());
            }
            Ok(())
        })();
        assert_eq!(result.is_err(), case != "scoped_cleanup_stop_ok");
        probe::released(slot, &lock);
        drop(writer);
        return;
    }
    let record = super::super::worktree_maintenance::Removal {
        path: checkout.to_string_lossy().into(),
        common_dir: common.to_string_lossy().into(),
        branch: "work".into(),
        head: String::new(),
        fingerprint: "fixture-original".into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        reserved_at: 1.0,
        status: "pending".into(),
        detail: String::new(),
        volumes: None,
    };
    super::super::worktree_maintenance::reserve_removal(&record).unwrap();
    let context = Context {
        common: common.into(),
        bases: vec![],
        processes: ProcessReview::Ordinary(Ok(Default::default())),
        bindings: vec![],
        bindings_complete: true,
        archives: vec![],
        archives_complete: true,
    };
    // Force real Git's subsequent list to fail, without replacing the listing
    // implementation or any authority decision with a synthetic return value.
    let head = common.join("HEAD");
    let bytes = fs::read(&head).unwrap();
    let failed = case == "scoped_cleanup_list_error";
    let partial = case == "scoped_cleanup_partial";
    if partial {
        let head = fs::read(checkout.join(".git")).unwrap();
        assert!(!head.is_empty());
        let commit = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(commit.status.success());
        // Real Git emits more rows than the parser's unchanged 512-row bound;
        // the original worktree sorts after these private prunable entries.
        for index in 0..513 {
            let metadata = common.join("worktrees").join(format!("a{index:04}"));
            fs::create_dir(&metadata).unwrap();
            fs::write(metadata.join("HEAD"), &commit.stdout).unwrap();
            fs::write(metadata.join("commondir"), "../..\n").unwrap();
            fs::write(
                metadata.join("gitdir"),
                format!("{}/a{index:04}/.git\n", root().parent().unwrap().display()),
            )
            .unwrap();
        }
        let (fresh, incomplete) = list(common, common, Instant::now() + Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{}", error.detail));
        assert!(incomplete);
        assert!(!fresh.iter().any(|tree| tree["path"] == json!(checkout)));
    }
    let changed = case == "scoped_cleanup_changed";
    let checkout = checkout.to_owned();
    let moved = root().parent().unwrap().join("moved-fixture");
    let slot = probe::capture("leftovers", &lock, move || {
        if failed {
            fs::remove_file(head).unwrap();
        }
        if changed {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(["worktree", "move"])
                .arg(&checkout)
                .arg(moved)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
    });
    let trees = vec![json!({"path":record.path})];
    let (rows, cut) = leftovers(
        common,
        &trees,
        false,
        &context,
        Instant::now() + Duration::from_secs(10),
        None,
    )
    .unwrap();
    fs::write(common.join("HEAD"), bytes).unwrap();
    assert_eq!(rows.len(), usize::from(changed));
    assert!(!cut);
    let remaining = super::super::worktree_maintenance::removals(common).unwrap();
    assert_eq!(
        remaining.len(),
        usize::from(failed || partial || changed),
        "pending-row decisions must remain unchanged"
    );
    probe::released(slot, &lock);
}
