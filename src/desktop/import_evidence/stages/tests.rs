use super::*;
const NOW: u64 = 10 * AGE;
struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: zerus_desktop_migration::canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        directory(&location.root.join("imports")).unwrap();
        Self {
            _temp: temp,
            location,
        }
    }
    fn input(&self, created: u64) -> (PathBuf, Lease) {
        let temp = tempfile::Builder::new()
            .prefix(".stage-")
            .tempdir_in(self.location.root.join("imports"))
            .unwrap();
        platform::restrict_dir(temp.path()).unwrap();
        let lease = Lease::new(
            &self.location,
            temp.path(),
            &uuid::Uuid::new_v4().to_string(),
            Kind::Input,
            created,
        )
        .unwrap();
        for kind in ["snapshots", "pages"] {
            directory(&temp.path().join(kind)).unwrap();
        }
        private_write(
            &temp
                .path()
                .join("snapshots")
                .join(format!("{}.json", sha256(b"{}"))),
            b"{}",
        )
        .unwrap();
        private_write(&temp.path().join("record-v1.json"), b"{}").unwrap();
        (temp.keep(), lease)
    }
}
#[test]
fn stale_native_input_is_swept_only_after_its_original_lease_ends() {
    let f = Fixture::new();
    let original = f._temp.path().join("original-qt.conf");
    fs::write(&original, b"owner preferences").unwrap();
    let published = f
        .location
        .root
        .join("imports")
        .join(uuid::Uuid::new_v4().to_string());
    directory(&published).unwrap();
    private_write(&published.join("record-v1.json"), b"immutable evidence").unwrap();
    let (path, mut lease) = f.input(NOW - AGE - 1);
    lease.retain();
    let lock = f.location.handoff_lock().unwrap();
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 0);
    assert!(path.exists());
    drop(lease);
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 1);
    assert!(!path.exists());
    assert_eq!(fs::read(original).unwrap(), b"owner preferences");
    assert_eq!(
        fs::read(published.join("record-v1.json")).unwrap(),
        b"immutable evidence"
    );
}
#[test]
fn fresh_replaced_linked_unknown_and_over_limit_inputs_stay_untouched() {
    for case in [
        "fresh", "future", "marker", "lease", "hardlink", "unknown", "large", "legacy",
    ] {
        let f = Fixture::new();
        let created = match case {
            "fresh" => NOW,
            "future" => NOW + 1,
            _ => NOW - AGE - 1,
        };
        let (path, mut lease) = f.input(created);
        lease.retain();
        let record = lease.record.clone();
        drop(lease);
        let root = f.location.root.join("imports");
        let (marker, lock_path) = paths(&root, &record.stage);
        match case {
            "marker" => {
                let mut changed = record.clone();
                changed.configuration = "b".repeat(64);
                fs::write(&marker, serde_json::to_vec(&changed).unwrap()).unwrap();
            }
            "lease" => {
                fs::rename(&lock_path, f._temp.path().join("original-lock")).unwrap();
                drop(file(&lock_path, true).unwrap());
            }
            "hardlink" => {
                fs::hard_link(
                    path.join("record-v1.json"),
                    f._temp.path().join("other-source"),
                )
                .unwrap();
            }
            "unknown" => private_write(&path.join("foreign.txt"), b"keep").unwrap(),
            "large" => {
                let body = fs::OpenOptions::new()
                    .write(true)
                    .open(
                        path.join("snapshots")
                            .join(format!("{}.json", sha256(b"{}"))),
                    )
                    .unwrap();
                body.set_len(MAX_SNAPSHOT_BYTES + 1).unwrap();
            }
            "legacy" => {
                fs::remove_file(marker).unwrap();
                fs::remove_file(lock_path).unwrap();
            }
            _ => (),
        }
        let lock = f.location.handoff_lock().unwrap();
        assert_eq!(
            sweep_at(&f.location, &lock, NOW).unwrap().removed,
            0,
            "{case}"
        );
        assert!(path.join("record-v1.json").is_file(), "{case}");
    }
}
#[test]
fn claimed_stage_retains_both_leases_and_is_swept_after_the_actual_job_and_cli_end() {
    let f = Fixture::new();
    let (original, mut source) = f.input(NOW - AGE - 1);
    source.retain();
    let container = tempfile::Builder::new()
        .prefix(".held-import-")
        .tempdir_in(f.location.root.join("imports"))
        .unwrap();
    platform::restrict_dir(container.path()).unwrap();
    let mut claim = Lease::new(
        &f.location,
        container.path(),
        &source.record.import_id,
        Kind::Claim {
            source: source.record.stage.clone(),
            directory: source.record.directory,
        },
        NOW - AGE - 1,
    )
    .unwrap();
    claim.retain();
    fs::rename(&original, container.path().join("input")).unwrap();
    let path = container.keep();
    let lock = f.location.handoff_lock().unwrap();
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 0);
    drop(claim);
    assert_eq!(
        sweep_at(&f.location, &lock, NOW).unwrap().removed,
        0,
        "original CLI still owns its physical lease"
    );
    assert!(path.join("input/record-v1.json").is_file());
    drop(source);
    let removed = sweep_at(&f.location, &lock, NOW).unwrap().removed;
    assert!((1..=REMOVE).contains(&removed));
    assert!(!path.exists());
    assert_eq!(
        fs::read_dir(f.location.root.join("imports"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn native_claim_can_clean_its_exact_input_after_creator_sidecars_are_gone() {
    let f = Fixture::new();
    let (original, source) = f.input(NOW - AGE - 1);
    let container = tempfile::Builder::new()
        .prefix(".held-import-")
        .tempdir_in(f.location.root.join("imports"))
        .unwrap();
    platform::restrict_dir(container.path()).unwrap();
    let mut claim = Lease::new(
        &f.location,
        container.path(),
        &source.record.import_id,
        Kind::Claim {
            source: source.record.stage.clone(),
            directory: source.record.directory,
        },
        NOW - AGE - 1,
    )
    .unwrap();
    claim.retain();
    fs::rename(&original, container.path().join("input")).unwrap();
    let path = container.keep();
    drop(source);
    drop(claim);
    let lock = f.location.handoff_lock().unwrap();
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 1);
    assert!(!path.exists());
}
#[test]
fn sweep_requires_the_original_handoff_and_has_a_fixed_per_pass_limit() {
    let f = Fixture::new();
    let other = Fixture::new();
    let wrong = other.location.handoff_lock().unwrap();
    let mut paths = vec![];
    for _ in 0..6 {
        let (path, mut lease) = f.input(NOW - AGE - 1);
        lease.retain();
        drop(lease);
        paths.push(path);
    }
    assert!(sweep_at(&f.location, &wrong, NOW).is_err());
    assert!(paths.iter().all(|p| p.exists()));
    let lock = f.location.handoff_lock().unwrap();
    let first = sweep_at(&f.location, &lock, NOW).unwrap();
    assert_eq!(first.removed, REMOVE);
    assert!(first.incomplete);
    for _ in 0..3 {
        let pass = sweep_at(&f.location, &lock, NOW).unwrap();
        assert!(pass.removed <= REMOVE);
    }
    assert!(paths.iter().all(|p| !p.exists()));
}
#[test]
#[cfg(unix)]
fn symbolic_directory_replacement_never_deletes_the_other_tree() {
    let f = Fixture::new();
    let (path, mut lease) = f.input(NOW - AGE - 1);
    lease.retain();
    drop(lease);
    let outside = f._temp.path().join("outside");
    directory(&outside).unwrap();
    private_write(&outside.join("keep"), b"keep").unwrap();
    fs::rename(&path, f._temp.path().join("original-stage")).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    let lock = f.location.handoff_lock().unwrap();
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 0);
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
}

#[test]
#[cfg(unix)]
fn a_parent_replaced_after_planning_is_refused_before_any_file_is_removed() {
    let f = Fixture::new();
    let (path, mut lease) = f.input(NOW - AGE - 1);
    lease.retain();
    let record = lease.record.clone();
    drop(lease);
    let root = f.location.root.join("imports");
    let owned = proof(&f.location, &root, &record.stage, NOW).unwrap();
    let (entries, _) = plan(&root, &owned, &f.location, NOW).unwrap();
    let key = id(&super::super::held::directory_handle(&root).unwrap()).unwrap();
    let original = f._temp.path().join("original-after-plan");
    fs::rename(&path, &original).unwrap();
    std::os::unix::fs::symlink(&original, &path).unwrap();
    assert!(remove(entries, &root, key).is_err());
    assert!(original.join("record-v1.json").is_file());
    assert!(original
        .join("snapshots")
        .join(format!("{}.json", sha256(b"{}")))
        .is_file());
}

#[test]
fn a_claim_plan_refuses_a_still_present_source_stage_without_deleting_either() {
    let f = Fixture::new();
    let (original, mut source) = f.input(NOW - AGE - 1);
    source.retain();
    let container = tempfile::Builder::new()
        .prefix(".held-import-")
        .tempdir_in(f.location.root.join("imports"))
        .unwrap();
    platform::restrict_dir(container.path()).unwrap();
    let mut claim = Lease::new(
        &f.location,
        container.path(),
        &source.record.import_id,
        Kind::Claim {
            source: source.record.stage.clone(),
            directory: source.record.directory,
        },
        NOW - AGE - 1,
    )
    .unwrap();
    claim.retain();
    fs::rename(&original, container.path().join("input")).unwrap();
    let container = container.keep();
    // An unrelated private replacement at the original source name must block
    // claim cleanup even when all original sidecars and claimed bytes match.
    directory(&original).unwrap();
    private_write(&original.join("keep.json"), b"do not delete").unwrap();
    drop(source);
    drop(claim);
    let root = f.location.root.join("imports");
    let name = container.file_name().unwrap().to_str().unwrap();
    let owned = proof(&f.location, &root, name, NOW).unwrap();
    assert!(plan(&root, &owned, &f.location, NOW).is_err());
    drop(owned);
    let lock = f.location.handoff_lock().unwrap();
    assert_eq!(sweep_at(&f.location, &lock, NOW).unwrap().removed, 0);
    assert_eq!(
        fs::read(original.join("keep.json")).unwrap(),
        b"do not delete"
    );
    assert!(container.join("input/record-v1.json").is_file());
}
