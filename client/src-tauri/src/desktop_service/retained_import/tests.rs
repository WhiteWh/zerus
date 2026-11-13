use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

/// zerus-obif: a run's Operations charge is released only once its child's
/// pipe readers finish, which can trail the run's return. A fixture that
/// admits again waits, bounded, for that role to drain instead of racing it;
/// production retention and the Gate are unchanged.
pub(super) fn admit_drained(report: &Report) -> zerus_client_core::desktop::drain::Charge {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while report
        .work
        .snapshot()
        .is_ok_and(|snapshot| snapshot.count(Role::Operations) > 0)
        && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    report.work.admit(Role::Operations).unwrap()
}

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    /// The download folder the report resolves, switchable like the setting.
    pub(super) downloads: Arc<Mutex<PathBuf>>,
    pub(super) report: Arc<Report>,
    pub(super) relay: Arc<zerus_client_core::desktop::relay::Connection>,
}
impl Fixture {
    pub(super) fn new(mode: &str) -> Self {
        let root = crate::desktop_service::tests::private_root(&std::env::temp_dir());
        let root = root.canonicalize().unwrap();
        let cli = root.join("hgs");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/desktop_service/retained_import/fake_cli.py"),
            &cli,
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let settings_path = root.join("settings.json");
        zerus_client_core::fsx::write_private_durable(
            &settings_path,
            br#"{"version":1,"values":{"workspace/language":"en"}}"#,
        )
        .unwrap();
        let qt = root.join("qt.ini");
        zerus_client_core::fsx::write_private_durable(&qt, b"[workspace]\nlanguage=en\n").unwrap();
        fs::write(root.join("mode"), mode).unwrap();
        let downloads = Arc::new(Mutex::new(root.join("downloads")));
        let report = Report::new(Inputs {
            creations: zerus_client_core::migration::preload::Capture::new(root.clone()),
            local_reads: zerus_client_core::migration::preload::Capture::local_reads(root.clone()),
            worktree_cache: zerus_client_core::migration::preload::Capture::worktree_cache(
                root.clone(),
            ),
            // Synthetic roots only: never this computer's Qt or dev stores.
            drafts: Arc::new(zerus_client_core::migration::draft_preload::Capture::new(
                root.clone(),
                root.join(zerus_client_core::drafts::dir(false)),
                None,
                Vec::new(),
            )),
            // A synthetic download folder under this fixture's root.
            downloads: Some({
                let folder = downloads.clone();
                Arc::new(move || folder.lock().unwrap().clone())
            }),
            data: root.clone(),
            settings: Settings::open(settings_path, None),
            qt: Some(QtStore::Ini(qt)),
        });
        let relay = Arc::new(zerus_client_core::desktop::relay::Connection::select(&cli).unwrap());
        Self {
            root,
            downloads,
            report,
            relay,
        }
    }
    pub(super) fn run(&self, explicit: bool) -> Result<(), &'static str> {
        let charge = admit_drained(&self.report);
        self.report.run(
            self.relay.clone(),
            "a".repeat(64),
            Arc::new(AtomicBool::new(false)),
            explicit,
            Arc::new(charge),
        )
    }
    pub(super) fn mode(&self, value: &str) {
        fs::write(self.root.join("mode"), value).unwrap();
    }
    pub(super) fn attempts(&self) -> usize {
        fs::read_to_string(self.root.join("publications"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.report.close();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn ordinary_owned_qt_permissions_publish_a_report_without_changing_the_source() {
    use std::os::unix::fs::MetadataExt;
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("success");
    let path = f.root.join("qt.ini");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let before = fs::metadata(&path).unwrap();
    let original = fs::read(&path).unwrap();
    assert_eq!(f.run(false), Ok(()));
    assert_eq!(f.report.status()["phase"], "published");
    assert_eq!(f.attempts(), 1);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.uid(),
            before.nlink()
        ),
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.uid(),
            after.nlink()
        )
    );
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn native_report_publishes_once_and_later_starts_only_inspect_the_original_id() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("success");
    let original = fs::read(f.root.join("qt.ini")).unwrap();
    assert_eq!(f.run(false), Ok(()));
    let first = f.report.status();
    assert_eq!(first["phase"], "published");
    assert_eq!(f.attempts(), 1);
    assert_eq!(fs::read(f.root.join("qt.ini")).unwrap(), original);
    assert!(f
        .report
        .inputs
        .settings
        .set("workspace/language", json!("en"))
        .is_ok());
    assert_eq!(f.run(true), Ok(()));
    assert_eq!(f.attempts(), 1);
    assert_eq!(f.report.status()["import_id"], first["import_id"]);
}
#[test]
fn exact_busy_retains_source_and_uuid_until_an_explicit_same_record_attempt() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("busy");
    assert_eq!(f.run(false), Err("retained_import_handoff_busy"));
    let first = f.report.status();
    assert_eq!(first["phase"], "not_committed");
    f.mode("success");
    assert!(f.run(false).is_err());
    assert_eq!(f.attempts(), 1);
    assert_eq!(f.run(true), Ok(()));
    assert_eq!(f.attempts(), 2);
    assert_eq!(f.report.status()["import_id"], first["import_id"]);
    assert_eq!(f.report.status()["import_record"], first["import_record"]);
}
#[test]
fn lost_partial_or_mismatched_result_never_becomes_a_second_publication() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    for mode in ["lost", "partial", "mismatch"] {
        let f = Fixture::new(mode);
        assert!(f.run(false).is_err(), "{mode}");
        assert_eq!(f.report.status()["phase"], "unknown", "{mode}");
        let id = f.report.status()["import_id"].clone();
        f.mode("busy");
        assert!(f.run(true).is_err(), "{mode}");
        assert_eq!(f.attempts(), 1);
        assert_eq!(f.report.status()["import_id"], id);
        assert_eq!(f.report.status()["phase"], "unknown");
    }
}
#[test]
fn exact_committed_lookup_settles_a_lost_result_without_resending_or_new_identity() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("commit_lost");
    assert!(f.run(false).is_err());
    let id = f.report.status()["import_id"].clone();
    assert_eq!(f.report.status()["phase"], "unknown");
    assert_eq!(f.run(true), Ok(()));
    assert_eq!(f.attempts(), 1);
    assert_eq!(f.report.status()["import_id"], id);
}
#[test]
fn changed_source_after_busy_is_quarantined_without_another_commit_record() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("busy");
    assert!(f.run(false).is_err());
    let id = f.report.status()["import_id"].clone();
    zerus_client_core::fsx::write_private_durable(
        &f.root.join("qt.ini"),
        b"[workspace]\nlanguage=ru\n",
    )
    .unwrap();
    f.mode("success");
    assert_eq!(f.run(true), Err("retained_import_source_changed"));
    assert_eq!(f.report.status()["phase"], "quarantined");
    assert_eq!(f.report.status()["import_id"], id);
    assert!(!f.root.join("committed.json").exists());
}
#[test]
fn absent_qt_is_explicitly_not_applicable_without_fabricated_report_or_sources() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("success");
    fs::remove_file(f.root.join("qt.ini")).unwrap();
    assert_eq!(f.run(false), Ok(()));
    assert_eq!(f.report.status()["required"], false);
    assert_eq!(f.attempts(), 0);
    assert!(!f.root.join("qt-retained-import-v1.json").exists());
}

#[test]
fn a_bad_lazy_source_keeps_original_bytes_without_a_final_manifest() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new("success");
    let original = b"[attention]\npendingReads={\"not-an-exact-read-key\":{}}\n";
    zerus_client_core::fsx::write_private_durable(&f.root.join("qt.ini"), original).unwrap();
    assert_eq!(f.run(false), Err("retained_import_source_kept_for_review"));
    assert_eq!(f.report.status()["phase"], "quarantined");
    assert!(!f.root.join("committed.json").exists());
    assert_eq!(fs::read(f.root.join("qt.ini")).unwrap(), original);
    let attempts = f.attempts();
    assert!(f.run(true).is_err());
    assert_eq!(f.attempts(), attempts);
}

#[cfg(target_os = "linux")]
impl Fixture {
    pub(super) fn service(
        &self,
    ) -> (
        Arc<Service>,
        crate::desktop_service::frontend::tests::native::CacheServer,
    ) {
        self.service_with_config(json!({}))
    }
    fn service_with_config(
        &self,
        extra: Value,
    ) -> (
        Arc<Service>,
        crate::desktop_service::frontend::tests::native::CacheServer,
    ) {
        self.service_observed(extra, Vec::new())
    }
    fn service_observed(
        &self,
        extra: Value,
        projections: Vec<Projection>,
    ) -> (
        Arc<Service>,
        crate::desktop_service::frontend::tests::native::CacheServer,
    ) {
        use crate::desktop_service::frontend::tests::native::{responder, CacheServer};
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        fs::remove_file(self.root.join("hgs")).unwrap();
        for (leaf, path) in [
            ("hgs", "desktop_ownership/fake_hgs.py"),
            ("catalog-fixture.py", "desktop_service/fake_hgs.py"),
            (
                "import-fixture.py",
                "desktop_service/retained_import/fake_cli.py",
            ),
        ] {
            std::os::unix::fs::symlink(source.join(path), self.root.join(leaf)).unwrap();
        }
        let mut config =
            json!({"ready":true,"state":"identity_changed","retained_import_abi":true});
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        fs::write(self.root.join("fixture.json"), config.to_string()).unwrap();
        let responder = responder();
        assert!(responder
            .configure_ownership_mode_once(zerus_client_core::desktop::ownership::Mode::Service));
        let cache = CacheServer::start(&self.root, responder.clone());
        let pump = crate::pump::Pump::default();
        let (sink, changes) = std::sync::mpsc::sync_channel(256);
        let drain = pump.drain_service(changes, |_| {});
        let service = Service::start_observed(
            self.root.join("hgs"),
            pump,
            sink,
            true,
            drain,
            Some(responder),
            Observers {
                projections,
                retained_sources: Some(self.report.inputs.clone()),
                ..Default::default()
            },
        );
        (service, cache)
    }
    pub(super) fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[cfg(target_os = "linux")]
#[test]
fn automatic_reconnect_preserves_authority_and_import_until_one_explicit_retry() {
    #[cfg(unix)]
    if crate::test_context::non_agent() {
        return;
    }
    use crate::desktop_service::tests::eventually;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = Fixture::new("success");
        let (service, _cache) = f.service_with_config(json!({
            "restart_watch":true,"refuse":"ownership_resources_pending"
        }));
        let baseline = || {
            service
                .shared
                .dirty
                .lock()
                .unwrap()
                .progress
                .contains_key("")
        };
        let mutations = || {
            f.calls()
                .iter()
                .filter(|call| {
                    matches!(
                        call["value"]["request"]["action"].as_str(),
                        Some("grant" | "rearm")
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let imports = || fs::read_to_string(f.root.join("import-calls")).unwrap_or_default();
        eventually(
            "initial native report and bounded Grant refusals settle",
            || baseline() && service.shared.ownership.retryable_grant(),
        )
        .await;
        assert_eq!(f.attempts(), 1);
        assert_eq!(
            service.snapshot().retained_source_import.unwrap()["phase"],
            "published"
        );

        // The absent socket is observed only after the old connection's workers
        // join, giving an exact boundary for counting automatic-side requests.
        let mut config: Value =
            serde_json::from_slice(&fs::read(f.root.join("fixture.json")).unwrap()).unwrap();
        config["produced_status"] = json!({"running":false});
        fs::write(f.root.join("fixture.json"), config.to_string()).unwrap();
        fs::write(f.root.join("restart-watch"), []).unwrap();
        eventually("absent service remains reconnecting", || {
            f.root.join("produced-status-read").exists()
                && service.snapshot().code == "service_unavailable"
                && service.snapshot().reconnecting
        })
        .await;
        let before = mutations();
        let before_imports = imports();
        assert!(
            !before.is_empty(),
            "the ownership fixture must actually exercise Grant"
        );
        assert!(
            !before_imports.is_empty(),
            "the retained import must actually run initially"
        );
        config.as_object_mut().unwrap().remove("produced_status");
        fs::write(f.root.join("fixture.json"), config.to_string()).unwrap();
        eventually("automatic connection reaches the second watch", || {
            f.root.join("second-watch-held").exists()
        })
        .await;
        fs::write(f.root.join("release-second-watch"), []).unwrap();
        eventually("automatic connection has a fresh catalog", || {
            baseline() && service.snapshot().connected
        })
        .await;
        assert!(
            service.shared.ownership.retryable_grant(),
            "known refusal must keep an accidental recover_grant worker observable"
        );
        // recover_grant's first retry is at one second. Keep this connection
        // alive past that deadline as well as checking its startup path.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert_eq!(
            mutations(),
            before,
            "automatic reconnect must not Grant or Rearm"
        );
        assert_eq!(
            imports(),
            before_imports,
            "automatic reconnect must not re-enter retained import, including lookup"
        );

        config.as_object_mut().unwrap().remove("refuse");
        fs::write(f.root.join("fixture.json"), config.to_string()).unwrap();
        service.retry();
        eventually("explicit Retry makes one Grant and opens its watch", || {
            service.shared.ownership.granted()
                && baseline()
                && f.calls()
                    .iter()
                    .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                    .count()
                    == 3
        })
        .await;
        let after = mutations();
        assert_eq!(
            after.len(),
            before.len() + 1,
            "exactly one new Grant belongs to explicit Retry"
        );
        assert_eq!(after.last().unwrap()["value"]["request"]["action"], "grant");
        assert_eq!(
            f.attempts(),
            1,
            "explicit Retry only checks the completed import"
        );
        assert_ne!(
            imports(),
            before_imports,
            "explicit Retry retains its original import lookup path"
        );
        service.join().await.unwrap();
    });
}

#[cfg(target_os = "linux")]
#[test]
fn busy_or_quarantined_report_preserves_grant_watch_catalog_and_preferences() {
    #[cfg(unix)]
    if crate::test_context::non_agent() {
        return;
    }
    use crate::desktop_service::tests::eventually;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for busy in [true, false] {
            let f = Fixture::new(if busy { "busy" } else { "success" });
            if !busy {
                zerus_client_core::fsx::write_private_durable(
                    &f.root.join("qt.ini"),
                    b"[attention]\npendingReads={\"not-an-exact-read-key\":{}}\n",
                )
                .unwrap();
            }
            let (service, _cache) = f.service();
            eventually(
                "report failure still reaches a granted connected catalog",
                || {
                    service.snapshot().connected
                        && service.shared.ownership.granted()
                        && service
                            .shared
                            .dirty
                            .lock()
                            .unwrap()
                            .progress
                            .contains_key("")
                },
            )
            .await;
            let expected = if busy {
                "retained_import_handoff_busy"
            } else {
                "retained_import_source_kept_for_review"
            };
            assert_eq!(
                service.snapshot().retained_source_import.as_ref().unwrap()["code"],
                expected
            );
            assert!(f
                .calls()
                .iter()
                .any(|call| call["value"]["request"]["action"] == "grant"));
            let first_attempts = f.attempts();
            if busy {
                assert_eq!(first_attempts, 1);
            }
            let view = service.ownership_rearm().await;
            assert!(!view.pending && view.code.is_empty(), "{view:?}");
            eventually(
                "rearm reconnects without silently retrying its report again",
                || {
                    service.snapshot().connected
                        && f.calls()
                            .iter()
                            .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                            .count()
                            >= 2
                },
            )
            .await;
            assert_eq!(
                service.snapshot().retained_source_import.as_ref().unwrap()["code"],
                expected
            );
            assert_eq!(
                f.attempts(),
                if busy { 2 } else { first_attempts },
                "one attempt for each explicit action only"
            );
            assert!(f
                .report
                .inputs
                .settings
                .set("workspace/language", json!("ru"))
                .is_ok());
            assert!(f
                .report
                .inputs
                .settings
                .flush_within(Duration::from_secs(5)));
            service.join().await.unwrap();
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn an_unknown_report_or_later_lookup_failure_never_blocks_a_new_connection() {
    #[cfg(unix)]
    if crate::test_context::non_agent() {
        return;
    }
    use crate::desktop_service::tests::eventually;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for published in [false, true] {
            let f = Fixture::new(if published { "success" } else { "lost" });
            assert_eq!(f.run(false).is_ok(), published);
            let id = f.report.status()["import_id"].clone();
            if published {
                f.mode("lookup_error");
            }
            let (service, _cache) = f.service();
            eventually("report lookup cannot prevent the next Grant/watch", || {
                // Preflight can publish connected before the first watch.
                // Observe the actual catalog before retrying that connection.
                service.snapshot().connected
                    && service.shared.ownership.granted()
                    && service
                        .shared
                        .dirty
                        .lock()
                        .unwrap()
                        .progress
                        .contains_key("")
            })
            .await;
            let status = service.snapshot().retained_source_import.unwrap();
            assert_eq!(
                status["code"],
                if published {
                    "retained_import_result_unavailable"
                } else {
                    "retained_import_original_result_unconfirmed"
                }
            );
            assert_eq!(status["import_id"], id);
            assert_eq!(f.attempts(), 1, "lookup never replays the report");
            service.retry();
            eventually(
                "explicit Retry still connects without replaying an unknown",
                || {
                    service.snapshot().connected
                        && f.calls()
                            .iter()
                            .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                            .count()
                            >= 2
                },
            )
            .await;
            assert_eq!(f.attempts(), 1);
            assert_eq!(
                service.snapshot().retained_source_import.unwrap()["import_id"],
                id
            );
            service.join().await.unwrap();
        }
    });
}

#[test]
fn paused_creation_capture_is_visible_beside_a_successful_independent_qt_report() {
    let f = Fixture::new("success");
    zerus_client_core::fsx::write_private_durable(
        &f.root.join("creations.json"),
        br#"{"schema":999,"operations":{}}"#,
    )
    .unwrap();
    assert!(f.report.inputs.creations.prepare().is_err());
    assert!(f.run(false).is_ok());
    let status = f.report.status();
    assert_eq!(status["creations_capture"]["paused"], true);
    assert_eq!(
        status["creations_capture"]["code"],
        "creation_source_unsupported"
    );
    assert_eq!(status["complete"], true);
}

/// G3 drafts (zerus-5wmh.66.5.2.1.2): a recorded capture failure is visible in
/// the report beside the Qt report, and the report's Retry runs the store's
/// waiting import without changing the recorded outcome.
#[test]
fn a_paused_draft_import_is_visible_and_the_report_retry_resumes_it() {
    use zerus_client_core::drafts::{self, DraftStore, ImportSource};
    let f = Fixture::new("success");
    let next = f.root.join(drafts::dir(false));
    let big = next.join("a".repeat(64));
    fs::create_dir_all(&big).unwrap();
    fs::write(big.join("draft.json"), vec![b' '; (1 << 20) + 1]).unwrap();
    let capture = f.report.inputs.drafts.clone();
    let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = published.clone();
    f.report.hook_drafts(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    let store = Arc::new(DraftStore::open_captured(next.clone(), capture.clone()));
    let weak = Arc::downgrade(&store);
    capture.hook_retry(Arc::new(move || {
        if let Some(store) = weak.upgrade() {
            store.retry_import();
        }
    }));
    // An empty source folder: the import has nothing to copy, but runs.
    let qt = f.root.join("qt-drafts");
    fs::create_dir_all(&qt).unwrap();
    let sources = vec![ImportSource {
        name: "qt".into(),
        root: qt,
    }];
    store.import(sources, |_| {});
    store.flush();
    let status = f.report.status();
    assert_eq!(status["drafts_capture"]["code"], "draft_capture_too_large");
    assert_eq!(status["drafts_capture"]["writes"], true);
    assert_eq!(status["drafts_capture"]["paused"], true);
    assert!(!next.join(drafts::IMPORT_MARKER).exists());
    assert!(
        published.load(Ordering::SeqCst) >= 2,
        "outcome and pause republish"
    );
    f.report.request_retry();
    store.flush();
    let status = f.report.status();
    assert_eq!(status["drafts_capture"]["code"], "draft_capture_too_large");
    assert_eq!(status["drafts_capture"]["paused"], false);
    assert!(next.join(drafts::IMPORT_MARKER).exists());
}

/// G3 downloads (zerus-5wmh.66.5.2.1.3): the report shows the configured
/// folder's evidence, read by bounded stats only, never consent to change it.
#[test]
fn the_report_shows_the_download_folders_evidence() {
    let f = Fixture::new("success");
    assert_eq!(
        f.report.status()["downloads_capture"]["code"],
        "download_capture_none"
    );
    let f = Fixture::new("success");
    let folder = f.root.join("downloads");
    let writer = zerus_client_core::download_cache::Writer { evidence: true };
    let part = zerus_client_core::download_cache::reserve(&folder).unwrap();
    fs::write(part.path(), b"copy").unwrap();
    writer.complete(part, "copy.txt", None, 1).unwrap();
    let status = f.report.status();
    assert_eq!(
        status["downloads_capture"]["code"],
        "download_capture_captured"
    );
    assert!(status["downloads_capture"]["capture_id"].is_string());
}

/// review-n P2 on #1256: the shown status follows the configured folder at
/// once; a cached value is never reused for another folder.
#[test]
fn the_download_status_follows_a_folder_change_at_once() {
    let f = Fixture::new("success");
    let writer = zerus_client_core::download_cache::Writer { evidence: true };
    let save = |folder: &std::path::Path| {
        let part = zerus_client_core::download_cache::reserve(folder).unwrap();
        fs::write(part.path(), b"copy").unwrap();
        writer.complete(part, "copy.txt", None, 1).unwrap();
    };
    let a = f.root.join("downloads");
    save(&a);
    assert_eq!(
        f.report.status()["downloads_capture"]["code"],
        "download_capture_captured"
    );
    // Another folder, within the reuse window: never A's value.
    *f.downloads.lock().unwrap() = f.root.join("elsewhere");
    assert_eq!(
        f.report.status()["downloads_capture"]["code"],
        "download_capture_none"
    );
    // A folder whose receipt is damaged: unreadable at once.
    let c = f.root.join("damaged");
    save(&c);
    let slots = c.join(".zerus-files-preload-v1");
    let slot = fs::read_dir(&slots)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(slot.join("receipt.json"), b"{}").unwrap();
    *f.downloads.lock().unwrap() = c;
    assert_eq!(
        f.report.status()["downloads_capture"]["code"],
        "download_capture_unreadable"
    );
    // Back to A: its own value again.
    *f.downloads.lock().unwrap() = a;
    assert_eq!(
        f.report.status()["downloads_capture"]["code"],
        "download_capture_captured"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn report_completion_wakes_the_next_group_without_watch_frames() {
    #[cfg(unix)]
    if crate::test_context::non_agent() {
        return;
    }
    use crate::desktop_service::tests::eventually;
    use migration::creation_report::Group;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = Fixture::new("success");
        let frames = Arc::new(Mutex::new(Vec::new()));
        let observed = frames.clone();
        let (service, _cache) = f.service_observed(
            json!({"silent_watch":true}),
            vec![Arc::new(move |frame| {
                if let Some(frame) = frame {
                    observed.lock().unwrap().push(frame["t"].clone());
                }
            })],
        );
        let report = service.shared.retained_import.as_ref().unwrap().clone();
        eventually("silent connected service after Qt drains", || {
            service.snapshot().connected
                && service.shared.report_frame_finished.load(Ordering::Acquire) == 1
                && report.status()["complete"] == true
                && report.work.snapshot().unwrap().count(Role::Operations) == 0
        })
        .await;
        // Queue both ready groups behind one real admission. Creations has a
        // verified missing source, so its worker uses no publisher/pipe and
        // releases Work before processing completion. Only the completion
        // schedule_current (not a later resource Drop) can wake LocalReads.
        let held = report.work.admit(Role::Operations).unwrap();
        report.inputs.creations.prepare().unwrap();
        zerus_client_core::fsx::write_private_durable(
            &f.root.join("attention-marks.json"),
            br#"{"version":1,"marks":[],"replies":[]}"#,
        )
        .unwrap();
        report.inputs.local_reads.prepare().unwrap();
        assert_eq!(
            report.group_status(Group::LocalReads)["code"],
            "local_reads_import_waiting"
        );
        assert_eq!(f.attempts(), 1);
        drop(held);
        // One initial dispatch only; no subsequent frame, explicit scheduling
        // or reconnect can conceal a missing completion wake.
        creations::schedule_current(&service.shared);
        eventually(
            "completion alone publishes the next historical group",
            || report.group_status(Group::LocalReads)["phase"] == "published",
        )
        .await;
        assert_eq!(report.group_status(Group::Creations)["required"], false);
        assert_eq!(report.group_status(Group::Creations)["complete"], true);
        assert_eq!(f.attempts(), 2);
        assert_eq!(*frames.lock().unwrap(), vec![json!("hello")]);
        assert_eq!(
            f.calls()
                .iter()
                .filter(|c| c["args"] == json!(["desktop", "watch", "--json"]))
                .count(),
            1
        );
        assert!(service.snapshot().connected && service.shared.ownership.granted());
        service.stop();
        service.join().await.unwrap();
    });
}

#[cfg(target_os = "linux")]
#[test]
fn report_qt_retry_waiting_for_a_group_completes_without_watch_or_reconnect() {
    #[cfg(unix)]
    if crate::test_context::non_agent() {
        return;
    }
    use crate::desktop_service::tests::eventually;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = Fixture::new("busy");
        let frames = Arc::new(Mutex::new(Vec::new()));
        let observed = frames.clone();
        let (service, _cache) = f.service_observed(
            json!({"silent_watch":true}),
            vec![Arc::new(move |frame| {
                if let Some(frame) = frame {
                    observed.lock().unwrap().push(frame["t"].clone());
                }
            })],
        );
        let report = service.shared.retained_import.as_ref().unwrap().clone();
        eventually("original Qt negative and connected silent service", || {
            service.snapshot().connected
                && service.shared.report_frame_finished.load(Ordering::Acquire) == 1
                && report.status()["phase"] == "not_committed"
                && report.work.snapshot().unwrap().count(Role::Operations) == 0
        })
        .await;
        let original = report.status();
        f.mode("hold");
        zerus_client_core::fsx::write_private_durable(
            &f.root.join("creations.json"),
            br#"{"schema":1,"operations":{},"opaque":"before-load"}"#,
        )
        .unwrap();
        report.inputs.creations.prepare().unwrap();
        eventually("original group holds the report slot", || {
            f.root.join("holding").exists()
        })
        .await;
        let relay = service.shared.relay.lock().unwrap().clone().unwrap();
        let stop = service.shared.cancellation.lock().unwrap().clone().unwrap();
        report.request_retry();
        // The same explicit-Retry hook used by connection, without reconnect
        // or a new hello concealing a lost completion wake.
        before_grant(
            &service.shared,
            relay,
            "a".repeat(64),
            stop,
            report.take_retry(),
        )
        .await;
        assert_eq!(
            report.status()["code"],
            "retained_import_source_resources_pending"
        );
        assert_eq!(f.attempts(), 2);
        assert!(service.snapshot().connected && service.shared.ownership.granted());
        f.mode("success");
        fs::write(f.root.join("release"), b"release original child").unwrap();
        eventually(
            "the explicit Qt retry resumes after the group drains",
            || report.status()["phase"] == "published",
        )
        .await;
        let settled = report.status();
        assert_eq!(settled["import_id"], original["import_id"]);
        assert_eq!(settled["import_record"], original["import_record"]);
        assert_eq!(f.attempts(), 3);
        assert_eq!(*frames.lock().unwrap(), vec![json!("hello")]);
        assert_eq!(
            f.calls()
                .iter()
                .filter(|c| c["args"] == json!(["desktop", "watch", "--json"]))
                .count(),
            1
        );
        assert!(service.snapshot().connected && service.shared.ownership.granted());
        service.stop();
        service.join().await.unwrap();
    });
}

#[cfg(target_os = "linux")]
#[test]
fn report_slot_wakes_only_after_the_last_original_resource_and_never_after_stop() {
    use crate::desktop_service::tests::eventually;
    use migration::creation_report::Group;
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for stopped in [false, true] {
            let f = Fixture::new("success");
            let frames = Arc::new(Mutex::new(Vec::new()));
            let observed = frames.clone();
            let (service, _cache) = f.service_observed(
                json!({"silent_watch":true}),
                vec![Arc::new(move |frame| {
                    if let Some(frame) = frame {
                        observed.lock().unwrap().push(frame["t"].clone());
                    }
                })],
            );
            let report = service.shared.retained_import.as_ref().unwrap().clone();
            eventually("initial Qt report physically drained", || {
                service.snapshot().connected
                    && service.shared.report_frame_finished.load(Ordering::Acquire) == 1
                    && report.status()["complete"] == true
                    && report.work.snapshot().unwrap().count(Role::Operations) == 0
            })
            .await;
            let completion = Arc::new(std::sync::atomic::AtomicU8::new(0));
            let resource = work_resources(
                &service.shared,
                report.work.admit(Role::Operations).unwrap(),
                completion.clone(),
            );
            let original_pipe = resource.clone();
            zerus_client_core::fsx::write_private_durable(
                &f.root.join("attention-marks.json"),
                br#"{"version":1,"marks":[],"replies":[]}"#,
            )
            .unwrap();
            report.inputs.local_reads.prepare().unwrap();
            assert_eq!(
                report.group_status(Group::LocalReads)["code"],
                "local_reads_import_source_resources_pending"
            );
            // The logical worker finished; its original pipe still owns the
            // same admission. Dropping the caller is never physical drain.
            completion.fetch_or(FINISHED, Ordering::AcqRel);
            drop(resource);
            assert_eq!(report.work.snapshot().unwrap().count(Role::Operations), 1);
            assert_eq!(f.attempts(), 1);
            if stopped {
                service.stop();
            }
            drop(original_pipe);
            if stopped {
                service.join().await.unwrap();
                assert_eq!(report.work.snapshot().unwrap().count(Role::Operations), 0);
                assert_eq!(f.attempts(), 1);
            } else {
                eventually("last original resource schedules pending report", || {
                    report.group_status(Group::LocalReads)["phase"] == "published"
                })
                .await;
                assert_eq!(f.attempts(), 2);
                assert_eq!(*frames.lock().unwrap(), vec![json!("hello")]);
                service.stop();
                service.join().await.unwrap();
            }
        }
    });
}

#[test]
fn projects_only_upgrade_keeps_next_source_guards_but_never_offers_a_runtime_report() {
    let fixture = Fixture::new("ok");
    let inputs = Inputs::native(
        crate::identity::SWITCH_ID,
        false,
        fixture.root.clone(),
        fixture.report.inputs.settings.clone(),
        Arc::new(|| PathBuf::from("/unused")),
    )
    .unwrap();
    let creations = inputs.creations.clone();
    let local_reads = inputs.local_reads.clone();
    let copy = inputs.clone();
    assert!(copy.for_runtime_report().is_none());
    assert!(Arc::ptr_eq(&creations, &inputs.creations));
    assert!(Arc::ptr_eq(&local_reads, &inputs.local_reads));
    assert!(creations.prepare().is_ok());
    assert!(local_reads.prepare().is_ok());
    assert!(!fixture.root.join("qt-retained-import-v1.json").exists());
}
