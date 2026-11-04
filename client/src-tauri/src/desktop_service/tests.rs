use super::*;
use std::{fs, path::Path, time::Instant};

mod silent_watch;

pub(super) struct Fixture(pub(super) PathBuf);

struct ReleaseView(std::sync::mpsc::Sender<()>);
impl Drop for ReleaseView {
    fn drop(&mut self) {
        // Release on assertions/unwind too; the callback cannot outlive its
        // test owner or expire before a descheduled test can signal it.
        let _ = self.0.send(());
    }
}

fn injected_delay(variable: &str) -> Duration {
    let milliseconds = std::env::var(variable)
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .expect("fixture delay must be milliseconds")
        })
        .unwrap_or(0);
    assert!(milliseconds <= 90_000, "fixture delay exceeds its bound");
    Duration::from_millis(milliseconds)
}

pub(super) fn private_root(parent: &Path) -> PathBuf {
    use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt};
    // Darwin's sun_path has 104 bytes, including the terminating NUL. Hosted
    // runners can supply a TMPDIR too long even for our shortest socket leaf.
    let parent = if parent
        .join("hgs-svc-0000000000000000")
        .join("native-cache.sock")
        .as_os_str()
        .as_bytes()
        .len()
        < 104
    {
        parent
    } else if cfg!(target_os = "macos") {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    };
    for _ in 0..128 {
        let root = parent.join(format!(
            "hgs-svc-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ));
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("private service fixture: {error}"),
        }
    }
    panic!("private service fixture candidates exhausted");
}

impl Fixture {
    pub(super) fn new(config: Value) -> Self {
        let root = private_root(&std::env::temp_dir());
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/desktop_service/fake_hgs.py"),
            root.join("hgs"),
        )
        .unwrap();
        fs::write(root.join("fixture.json"), config.to_string()).unwrap();
        Self(root)
    }
    pub(super) fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    pub(super) fn start(&self, allowed: bool) -> (Arc<Service>, crate::pump::Pump) {
        self.start_with_responder(allowed, None)
    }
    pub(super) fn start_with_responder(
        &self,
        allowed: bool,
        responder: Option<Arc<zerus_client_core::desktop::Responder>>,
    ) -> (Arc<Service>, crate::pump::Pump) {
        self.start_with_pump(allowed, crate::pump::Pump::default(), responder)
    }
    fn start_with_pump(
        &self,
        allowed: bool,
        pump: crate::pump::Pump,
        responder: Option<Arc<zerus_client_core::desktop::Responder>>,
    ) -> (Arc<Service>, crate::pump::Pump) {
        let (sink, rx) = std::sync::mpsc::sync_channel(256);
        let drain = pump.drain_service(rx, |_| {});
        let service = Service::start(
            self.0.join("hgs"),
            pump.clone(),
            sink,
            allowed,
            drain,
            responder,
            vec![],
        );
        std::thread::sleep(injected_delay("ZERUS_TEST_SERVICE_START_RETURN_DELAY_MS"));
        (service, pump)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_long_temp_parent_still_gets_a_private_bindable_socket_root() {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    let outer = Fixture::new(json!({}));
    let long_parent = outer.0.join("a".repeat(100));
    fs::create_dir(&long_parent).unwrap();
    let root = Fixture(private_root(&long_parent));
    assert!(!root.0.starts_with(&long_parent));
    assert_eq!(
        fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let _listener = UnixListener::bind(root.0.join("native-cache.sock")).unwrap();
    assert!(long_parent.is_dir(), "the supplied parent is never removed");
}

pub(super) async fn eventually(label: &str, check: impl Fn() -> bool) {
    // A loaded hosted macOS runner missed 10 s for an initial live baseline.
    eventually_within(label, Duration::from_secs(30), check).await;
}
async fn eventually_within(label: &str, wait: Duration, check: impl Fn() -> bool) {
    let until = Instant::now() + wait;
    let mut pause = Some(injected_delay("ZERUS_TEST_SERVICE_POLL_DELAY_MS"));
    while !check() {
        // Model descheduling after an unsuccessful observation. The service
        // workers and their product deadlines continue normally meanwhile.
        if let Some(delay) = pause.take() {
            std::thread::sleep(delay);
        }
        if Instant::now() >= until {
            // The test may have been descheduled after its false sample.
            // Use a fresh observation before declaring the fixture timed out.
            assert!(check(), "{label}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn readiness_gets_a_fresh_observation_at_the_fixture_deadline() {
    let observed = std::cell::Cell::new(false);
    tauri::async_runtime::block_on(eventually_within("fresh sample", Duration::ZERO, || {
        observed.replace(true)
    }));
    assert!(observed.get());
}

#[test]
#[should_panic(expected = "fixture still unavailable")]
fn an_unready_fixture_remains_bounded() {
    tauri::async_runtime::block_on(eventually_within(
        "fixture still unavailable",
        Duration::ZERO,
        || false,
    ));
}
fn watch_calls(calls: &[Value]) -> usize {
    calls
        .iter()
        .filter(|c| c["args"] == json!(["desktop", "watch", "--json"]))
        .count()
}

#[test]
fn replay_without_an_explicit_transport_never_launches_a_cli() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let (service, _) = fixture.start(false);
        eventually("explicit unavailable", || {
            service.snapshot().code == "replay_transport_unavailable"
        })
        .await;
        service.join().await.unwrap();
        assert!(fixture.calls().is_empty());
    });
}
#[test]
fn incompatible_cli_stays_unavailable_until_one_explicit_retry_without_fallback() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"supported":false}));
        let (service, _) = fixture.start(true);
        eventually("old CLI refused", || {
            service.snapshot().code == "incompatible_service_cli"
        })
        .await;
        assert_eq!(fixture.calls().len(), 1);
        fs::write(fixture.0.join("fixture.json"), json!({}).to_string()).unwrap();
        assert!(service.reconnect("peer")); // Composer Refresh can retry a disconnected service.
        eventually("new CLI connected", || service.snapshot().connected).await;
        eventually("one watch", || watch_calls(&fixture.calls()) == 1).await;
        assert!(!service.snapshot().background_ready());
        service.join().await.unwrap();
        assert!(fixture
            .calls()
            .iter()
            .all(|c| c["args"] == json!(["--help"]) || c["args"][0] == "desktop"));
    });
}
#[test]
fn dirty_walk_finishes_its_cursor_and_coalesces_one_refresh_without_deleting_rows() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"dirty_walk":true,"relay_finalize_delay_ms":300}));
        let (service, pump) = fixture.start(true);
        pump.push(Change::Upsert {
            computer: ComputerId::local(),
            key: "unseen".into(),
            row: zerus_client_core::view::row(&json!({"name":"codex/example/unseen"})),
        });
        eventually("partial delta waits", || {
            fixture.0.join("delta-page-two").exists()
        })
        .await;
        // The next RPC can start before the view drain applies the prior row.
        // Keep page two blocked while checking actual partial-view membership.
        eventually(
            "first delta row applied with page two still blocked",
            || pump.row("", "inserted").is_some(),
        )
        .await;
        assert!(
            pump.row("", "unseen").is_some(),
            "partial catch-up cannot infer deletion"
        );
        fs::write(fixture.0.join("finish-delta"), []).unwrap();
        eventually("one follow-up delta", || {
            fixture.0.join("refresh-started").exists()
        })
        .await;
        assert!(
            pump.row("", "unseen").is_none(),
            "fully consumed baseline plus delta establishes membership"
        );
        fs::write(fixture.0.join("finish-refresh"), []).unwrap();
        eventually("new high-water consumed", || {
            pump.row("", "fresh").is_some()
        })
        .await;
        let calls = fixture.calls();
        let pages: Vec<_> = calls
            .iter()
            .filter(|c| c["value"]["op"] == "catalog_page")
            .collect();
        assert_eq!(pages.len(), 2, "hints must not restart a baseline");
        assert!(pages[0]["value"]["cursor"].is_null());
        assert!(
            pages[1]["value"]["cursor"].is_string(),
            "the original insertion walk must finish"
        );
        let deltas: Vec<_> = calls
            .iter()
            .filter(|c| c["value"]["op"] == "catalog_changes")
            .collect();
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0]["value"]["since_revision"], "0");
        assert_eq!(deltas[1]["value"]["since_revision"], "0");
        assert!(deltas[1]["value"]["cursor"].is_string());
        assert_eq!(deltas[2]["value"]["since_revision"], "2");
        assert!(deltas[2]["value"]["cursor"].is_null());
        assert!(pump.row("", "row").is_some());
        assert!(pump.row("", "first").is_none());
        service.join().await.unwrap();
        assert_eq!(
            fs::read_to_string(fixture.0.join("page-number")).unwrap(),
            "2"
        );
    });
}
#[test]
fn uncertain_read_keeps_its_uuid_and_only_durable_actor_commit_acknowledges() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true}));
        let (service, _) = fixture.start(true);
        eventually("read component ready", || {
            service.snapshot().component_ready("reads")
        })
        .await;
        let mark = zerus_client_core::readmarks::ReadMark {
            computer: "".into(),
            name: "codex/example/row".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
            reply_id: Some("reply".into()),
            attention_signature: None,
        };
        assert!(service.read_marks(vec![mark.clone()]).await.is_err());
        assert_eq!(
            service.read_marks(vec![mark.clone()]).await.unwrap(),
            vec![mark]
        );
        let reads: Vec<_> = fixture
            .calls()
            .into_iter()
            .filter(|c| c["value"]["op"] == "read_intent")
            .collect();
        assert_eq!(reads.len(), 2);
        assert_eq!(
            reads[0]["value"]["request_id"],
            reads[1]["value"]["request_id"]
        );
        assert!(fixture
            .calls()
            .iter()
            .any(|c| c["value"]["op"] == "delivery_result"));
        assert!(request(
            &service.shared,
            json!({"op":"automatic_approvals"}),
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err());
        service.join().await.unwrap();
        assert!(!fixture
            .calls()
            .iter()
            .any(|c| c["value"]["op"] == "automatic_approvals"));
    });
}
#[test]
fn queued_catalog_rows_are_discarded_after_walk_retirement_source_replacement_and_removal() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"supported":false}));
        let (service, pump) = fixture.start(true);
        eventually("old CLI refused", || {
            service.snapshot().code == "incompatible_service_cli"
        })
        .await;
        service.shared.publish(Snapshot {
            connected: true,
            epoch: Some("epoch".into()),
            ..Snapshot::default()
        });
        let old = SourceVersion {
            incarnation: 1,
            generation: 1,
            ..SourceVersion::default()
        };
        service.shared.dirty("", old).unwrap();
        let old = service.shared.dirty.lock().unwrap().sources[""].clone();
        let delivery = |key: &str, source: &SourceVersion| CatalogDelivery {
            shared: Arc::downgrade(&service.shared),
            computer: "".into(),
            epoch: "epoch".into(),
            source: source.clone(),
            stopped: Arc::new(AtomicBool::new(false)),
            value: CatalogValue::Row(Box::new(Change::Upsert {
                computer: ComputerId::local(),
                key: key.into(),
                row: zerus_client_core::view::row(&json!({"name":"codex/example/row"})),
            })),
        };
        let queued = delivery("retired", &old);
        service.shared.dirty.lock().unwrap().restart("").unwrap();
        queued.apply(&pump, &|_| {});
        assert!(pump.row("", "retired").is_none());
        let current = service.shared.dirty.lock().unwrap().sources[""].clone();
        let queued = delivery("stale", &current);
        service
            .shared
            .dirty(
                "",
                SourceVersion {
                    incarnation: 1,
                    generation: 2,
                    ..SourceVersion::default()
                },
            )
            .unwrap();
        queued.apply(&pump, &|_| {});
        assert!(pump.row("", "stale").is_none());
        service.shared.dirty("", old.clone()).unwrap();
        let current = service.shared.dirty.lock().unwrap().sources[""].clone();
        let queued = delivery("removed", &current);
        service.shared.invalidate();
        queued.apply(&pump, &|_| {});
        assert!(pump.row("", "removed").is_none());
        service.join().await.unwrap();
    });
}

#[test]
fn startup_switch_selects_only_the_service_source_and_closes_legacy_read_admission() {
    const CHILD: &str = "ZERUS_FIXTURE_DESKTOP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tauri::async_runtime::block_on(async {
            assert!(mode());
            let root = PathBuf::from(std::env::var_os(CHILD).unwrap());
            let actions = crate::actions::Actions::new(root.join("marks.json"));
            actions.disarm_read_delivery().unwrap();
            assert!(actions.queue_marks(vec![]).is_err());
            let source = crate::commands::AppState::default();
            assert_eq!(source.start_source().unwrap(), "service");
            assert_eq!(source.start_source().unwrap(), "service");
            let service = source.desktop_service().unwrap();
            eventually("service subscriber", || service.snapshot().connected).await;
            assert!(!service.snapshot().background_ready());
            eventually("one watch proxy", || {
                fs::read_to_string(root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .any(|line| {
                        serde_json::from_str::<Value>(line)
                            .is_ok_and(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                    })
            })
            .await;
            service.join().await.unwrap();
        });
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    let fixture = Fixture::new(json!({}));
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "desktop_service::tests::startup_switch_selects_only_the_service_source_and_closes_legacy_read_admission", "--nocapture"])
        .env(CHILD, &fixture.0).env(service::MODE_ENV, "1")
        .env("ZERUS_CLIENT_HGS", fixture.0.join("hgs")).env_remove("ZERUS_CLIENT_REPLAY")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let calls = fixture.calls();
    assert_eq!(watch_calls(&calls), 1);
    assert!(calls
        .iter()
        .all(|call| call["args"] == json!(["--help"]) || call["args"][0] == "desktop"));
    assert!(
        !fixture.0.join("marks.json").exists(),
        "legacy mark file was not written"
    );
}

#[test]
fn service_roster_restores_settings_peer_choices_without_authorizing_legacy_marks() {
    const CHILD: &str = "ZERUS_FIXTURE_SERVICE_PEERS_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        tauri::async_runtime::block_on(async {
            assert!(mode());
            let root = PathBuf::from(root);
            let config: Value =
                serde_json::from_slice(&fs::read(root.join("fixture.json")).unwrap()).unwrap();
            let mark_file = root.join("marks.json");
            let actions = crate::actions::Actions::new(mark_file.clone());
            actions
                .queue_marks(vec![zerus_client_core::readmarks::ReadMark {
                    computer: "offline".into(),
                    name: "codex/example/recorded".into(),
                    run_id: "run".into(),
                    conversation_id: "conversation".into(),
                    reply_id: Some("reply".into()),
                    attention_signature: None,
                }])
                .unwrap();
            actions.flush_marks();
            actions.disarm_read_delivery().unwrap();
            let frozen = fs::read(&mark_file).unwrap();
            let source = crate::commands::AppState::default();
            assert!(
                source.peers().is_none(),
                "not started is unknown, not empty"
            );
            assert_eq!(source.start_source().unwrap(), "service");
            let service = source.desktop_service().unwrap();
            eventually("connected without a verified roster", || {
                service.snapshot().connected
            })
            .await;
            assert!(
                source.peers().is_none(),
                "counts/captions do not supply peer choices"
            );
            fs::write(root.join("roster-known"), []).unwrap();
            let known = BTreeSet::from(["offline".to_owned(), "waiting".to_owned()]);
            eventually(
                "native settings receive offline and not-yet-greeted peers",
                || source.peers() == Some(known.clone()),
            )
            .await;
            let mut page = zerus_client_core::worktree_policy::page::Page::new();
            page.set_peers(source.peers().unwrap());
            assert_eq!(page.view().peers, vec!["offline", "waiting"]);
            let read = page.choose("offline").unwrap();
            assert_eq!(read.route, "offline");
            assert!(read.change.is_none(), "choosing a target is only a read");
            page.answered(read.id, false, Err("Offline fixture".into()));
            if config["roster_disconnect_early"] == true {
                eventually("watch EOF marks service unavailable", || {
                    !service.snapshot().connected
                })
                .await;
                assert_eq!(
                    source.peers(),
                    Some(known),
                    "EOF retains last-known names, not a complete zero"
                );
                assert_eq!(
                    source.refresh(),
                    1,
                    "one shared source, not one retry per displayed peer"
                );
                eventually("exactly one refreshed watch", || {
                    fs::read_to_string(root.join("calls"))
                        .unwrap_or_default()
                        .lines()
                        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                        .count()
                        == 2
                })
                .await;
                eventually("refreshed fixture disconnects", || {
                    !service.snapshot().connected
                })
                .await;
            } else {
                for (stage, generation) in [("invalid", 3), ("warming", 4), ("stopping", 5)] {
                    fs::write(root.join(format!("roster-{stage}")), []).unwrap();
                    eventually("changed status consumed", || {
                        service.snapshot().generation == Some(generation)
                    })
                    .await;
                    assert_eq!(
                        source.peers(),
                        Some(known.clone()),
                        "{stage} is not membership removal"
                    );
                    page.set_peers(source.peers().unwrap());
                    assert_eq!(page.view().route, "offline");
                }
                fs::write(root.join("roster-removed"), []).unwrap();
                eventually("verified roster removes only its absent peer", || {
                    source.peers() == Some(BTreeSet::from(["waiting".to_owned()]))
                })
                .await;
                page.set_peers(source.peers().unwrap());
                assert_eq!(page.view().peers, vec!["waiting"]);
                assert!(page.view().route.is_empty());
                fs::write(root.join("roster-empty"), []).unwrap();
                eventually("verified empty peer roster is known empty", || {
                    source.peers() == Some(BTreeSet::new())
                })
                .await;
                page.set_peers(source.peers().unwrap());
                assert!(page.view().peers.is_empty());
                fs::write(root.join("roster-end"), []).unwrap();
                eventually("final EOF", || !service.snapshot().connected).await;
                assert_eq!(source.peers(), Some(BTreeSet::new()));
            }
            let inspected = std::cell::Cell::new(false);
            actions.drop_orphans(|| {
                inspected.set(true);
                source.peers()
            });
            let inspected = inspected.get();
            let after = fs::read(&mark_file).unwrap();
            service.join().await.unwrap();
            assert!(
                !inspected,
                "closed mark admission refuses before inspecting peer choices"
            );
            assert_eq!(
                after, frozen,
                "the frozen pre-cutover queue is never pruned"
            );
        });
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    for disconnected in [false, true] {
        let fixture = Fixture::new(
            json!({"peer_choices":true,"counts_computer":"counts-only","roster_disconnect_early":disconnected}),
        );
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "desktop_service::tests::service_roster_restores_settings_peer_choices_without_authorizing_legacy_marks", "--nocapture"])
            .env(CHILD, &fixture.0).env(service::MODE_ENV, "1")
            .env("ZERUS_CLIENT_HGS", fixture.0.join("hgs")).env_remove("ZERUS_CLIENT_REPLAY")
            .stdout(Stdio::piped()).stderr(Stdio::piped()).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let calls = fixture.calls();
        assert_eq!(watch_calls(&calls), if disconnected { 2 } else { 1 });
        assert!(calls
            .iter()
            .all(|call| call["args"] == json!(["--help"]) || call["args"][0] == "desktop"));
        assert!(
            !calls
                .iter()
                .any(|call| call["value"]["computer"] == "waiting"),
            "an ungreeted choice does not start a GUI watcher or read"
        );
    }
}

#[test]
fn approval_seam_is_an_explicit_refusal_without_a_generic_cli_policy_call() {
    tauri::async_runtime::block_on(async {
        let id = uuid::Uuid::new_v4().to_string();
        let result = frontend::approval_command(None, id.clone(), true)
            .await
            .unwrap();
        assert_eq!(result["request_id"], id);
        assert_eq!(result["accepted"], false);
        assert_eq!(result["durable"], false);
        assert_eq!(result["code"], "frontend_route_unavailable");
    });
}

#[test]
fn exit_waits_for_the_owned_proxy_and_blocked_view_callback_before_finishing() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let pump = crate::pump::Pump::default();
        let (sink, rx) = std::sync::mpsc::sync_channel(256);
        let view_input = sink.clone();
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let release = ReleaseView(release);
        let drain = pump.drain_service(rx, move |change| {
            if matches!(change, Change::Upsert { key, .. } if key == "blocked-view-fixture") {
                entered.send(()).unwrap();
                let _ = resume.recv();
            }
        });
        let service = Service::start(
            fixture.0.join("hgs"),
            pump.clone(),
            sink,
            true,
            drain,
            None,
            vec![],
        );
        eventually("baseline before blocked view", || {
            pump.row("", "row").is_some()
        })
        .await;
        // Delay the view itself, outside a catalog's held admission locks.
        view_input
            .send(Data::Change(Box::new(Change::Upsert {
                computer: ComputerId::local(),
                key: "blocked-view-fixture".into(),
                row: zerus_client_core::ViewRow::default(),
            })))
            .unwrap();
        drop(view_input);
        eventually("view callback entered", || observed.try_recv().is_ok()).await;
        let finished = Arc::new(AtomicBool::new(false));
        let ended = finished.clone();
        let exiting = tauri::async_runtime::spawn(crate::exit_flush::finish_service_exit(
            service,
            move || ended.store(true, Ordering::Release),
        ));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!finished.load(Ordering::Acquire));
        assert!(!exiting.inner().is_finished());
        drop(release);
        exiting.await.unwrap();
        assert!(finished.load(Ordering::Acquire));
    });
}

#[test]
fn full_projection_replacement_rebaselines_even_with_unchanged_native_generation() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for stage in ["early", "late"] {
            let fixture = Fixture::new(
                json!({"replace_projection":stage,"projection_publication_gate":true}),
            );
            let (service, pump) = fixture.start(true);
            // Writing a hint to stdout is not evidence the client consumed it.
            // Retain the old cursor page until the replacement is bound under
            // the native hub lock, so this case really requires cursor close.
            eventually("replacement hint applied before old cursor page", || {
                service
                    .shared
                    .dirty
                    .lock()
                    .unwrap()
                    .sources
                    .get("")
                    .is_some_and(|source| {
                        source.projection.as_deref() == Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                    })
            })
            .await;
            fs::write(fixture.0.join("release-old-page"), []).unwrap();
            eventually("replacement baseline and catch-up", || {
                pump.row("", "replacement").is_some() && pump.row("", "row").is_none()
            })
            .await;
            service.join().await.unwrap();
            let calls = fixture.calls();
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c["value"]["op"] == "catalog_page")
                    .count(),
                2
            );
            let closes: Vec<_> = calls
                .iter()
                .filter(|c| c["value"]["op"] == "catalog_close")
                .collect();
            assert_eq!(closes.len(), 1, "{stage}: {calls:?}");
            assert_eq!(
                closes[0]["value"]["cursor"],
                "00000000-0000-4000-8000-000000000002"
            );
            assert_eq!(closes[0]["value"]["computer"], "");
            assert!(pump.row("", "obsolete").is_none());
        }
    });
}
#[test]
fn projection_replacement_after_cursor_exhaustion_needs_no_close() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(
            json!({"replace_projection":"exhausted","projection_publication_gate":true}),
        );
        let (service, pump) = fixture.start(true);
        let through = Revision::try_from("1".to_owned()).unwrap();
        eventually("old cursor consumed and walk finished before hint", || {
            let dirty = service.shared.dirty.lock().unwrap();
            dirty.progress.get("") == Some(&through) && dirty.running.is_none()
        })
        .await;
        assert!(pump.row("", "obsolete").is_some());
        fs::write(fixture.0.join("release-replacement-hint"), []).unwrap();
        eventually("replacement prunes old completed membership", || {
            pump.row("", "replacement").is_some()
                && pump.row("", "row").is_none()
                && pump.row("", "obsolete").is_none()
        })
        .await;
        service.join().await.unwrap();
        let calls = fixture.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c["value"]["op"] == "catalog_page")
                .count(),
            2
        );
        assert!(calls.iter().any(|c| c["value"]["op"] == "catalog_changes"
            && c["value"]["projection"] == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            && c["value"]["cursor"] == "00000000-0000-4000-8000-000000000002"));
        assert!(
            !calls.iter().any(|c| c["value"]["op"] == "catalog_close"),
            "an already consumed cursor is not outstanding"
        );
    });
}
#[test]
fn retention_gap_and_unknown_cursor_outcome_restart_from_a_fresh_baseline() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for failure in ["gap", "unknown", "changed_through"] {
            let fixture = Fixture::new(
                json!({"failure":failure,"relay_finalize_delay_ms":300,"relay_finalize_number":2}),
            );
            let (service, pump) = fixture.start(true);
            eventually("fresh baseline caught up", || {
                pump.row("", "recovered").is_some() && pump.row("", "row").is_none()
            })
            .await;
            service.join().await.unwrap();
            let calls = fixture.calls();
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c["value"]["op"] == "catalog_page")
                    .count(),
                2
            );
            let deltas: Vec<_> = calls
                .iter()
                .filter(|c| c["value"]["op"] == "catalog_changes")
                .collect();
            assert_eq!(
                deltas.len(),
                if failure == "changed_through" { 3 } else { 2 }
            );
            assert_eq!(deltas[0]["value"]["since_revision"], "0");
            assert_eq!(deltas.last().unwrap()["value"]["since_revision"], "5");
            assert_ne!(
                deltas[0]["value"]["request_id"],
                deltas.last().unwrap()["value"]["request_id"]
            );
        }
    });
}
#[test]
fn offline_baseline_retains_last_known_rows_without_delta_or_complete_zero() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"offline":true}));
        let (service, pump) = fixture.start(true);
        pump.push(Change::Upsert {
            computer: ComputerId::local(),
            key: "retained".into(),
            row: zerus_client_core::view::row(&json!({"name":"codex/example/retained"})),
        });
        eventually("offline retained source", || pump.row("", "row").is_some()).await;
        service.join().await.unwrap();
        assert!(pump.row("", "retained").is_some());
        assert!(!fixture
            .calls()
            .iter()
            .any(|c| c["value"]["op"] == "catalog_changes"));
    });
}
#[test]
fn twenty_thousand_rows_need_only_one_three_key_delta_after_the_baseline() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"large":20000}));
        let started = Instant::now();
        let (service, pump) = fixture.start(true);
        // Keep a generous fixture deadline for hosted runners; the process
        // count below, rather than a timing threshold, guards spawn overhead.
        let deadline = Instant::now() + Duration::from_secs(120);
        while pump.row("", "new").is_none() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pump.row("", "row-19999").is_some());
        assert!(pump.row("", "row-1").is_none());
        service.join().await.unwrap();
        let calls = fixture.calls();
        let relays: BTreeSet<_> = calls
            .iter()
            .filter(|call| call["args"] == json!(["desktop", "catalog-relay", "--json"]))
            .filter_map(|call| call["pid"].as_u64())
            .collect();
        assert_eq!(relays.len(), 2, "one original child per walk, not per page");
        assert!(!calls
            .iter()
            .any(|call| call["args"] == json!(["desktop", "request", "--json"])));
        eprintln!("20k catalog baseline + delta: {:?}, 157 baseline pages, 2 delta reads, {} relay children", started.elapsed(), relays.len());
        assert_eq!(
            calls
                .iter()
                .filter(|c| c["value"]["op"] == "catalog_page")
                .count(),
            157
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c["value"]["op"] == "catalog_changes")
                .count(),
            2
        );
    });
}
#[test]
fn stuck_view_callback_cannot_hold_the_quit_completion_past_its_deadline() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let pump = crate::pump::Pump::default();
        let (sink, rx) = std::sync::mpsc::sync_channel(256);
        let view_input = sink.clone();
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let release = ReleaseView(release);
        let drain = pump.drain_service(rx, move |change| {
            if matches!(change, Change::Upsert { key, .. } if key == "blocked-view-fixture") {
                entered.send(()).unwrap();
                let _ = resume.recv();
            }
        });
        let service = Service::start(
            fixture.0.join("hgs"),
            pump.clone(),
            sink,
            true,
            drain,
            None,
            vec![],
        );
        eventually("baseline before blocked quit view", || {
            pump.row("", "row").is_some()
        })
        .await;
        view_input
            .send(Data::Change(Box::new(Change::Upsert {
                computer: ComputerId::local(),
                key: "blocked-view-fixture".into(),
                row: zerus_client_core::ViewRow::default(),
            })))
            .unwrap();
        drop(view_input);
        eventually("blocked callback", || observed.try_recv().is_ok()).await;
        let finished = AtomicBool::new(false);
        let started = Instant::now();
        crate::exit_flush::finish_service_exit_with_deadline(
            service.clone(),
            || finished.store(true, Ordering::Release),
            Duration::from_millis(40),
        )
        .await;
        assert!(finished.load(Ordering::Acquire));
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(injected_delay("ZERUS_TEST_SERVICE_RELEASE_DELAY_MS"));
        drop(release);
        service.join().await.unwrap();
    });
}

#[test]
fn hint_driven_delta_walks_never_take_an_already_live_computer_offline() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(
            json!({"delta_live":true,"relay_finalize_delay_ms":300,"relay_finalize_number":2}),
        );
        let pump = crate::pump::Pump::default();
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let observe = statuses.clone();
        pump.tap(Box::new(move |change| {
            if let Change::Computer { computer, status } = change {
                if computer.is_local() {
                    observe.lock().unwrap().push(*status);
                }
            }
        }));
        // Install the one-shot Live observer before startup can finish.
        let (service, pump) = fixture.start_with_pump(true, pump, None);
        eventually("initial live baseline", || {
            statuses.lock().unwrap().contains(&ComputerStatus::Live)
        })
        .await;
        statuses.lock().unwrap().clear();
        fs::write(fixture.0.join("send-delta"), []).unwrap();
        eventually("delta round trip held", || {
            fixture.0.join("live-delta-hinted").exists()
        })
        .await;
        let during_rpc = statuses.lock().unwrap().clone();
        fs::write(fixture.0.join("finish-live-delta"), []).unwrap();
        eventually("next coalesced delta held", || {
            fixture.0.join("next-live-delta-started").exists()
        })
        .await;
        let newer_head_pending = statuses.lock().unwrap().clone();
        fs::write(fixture.0.join("finish-next-live-delta"), []).unwrap();
        eventually("second contiguous delta consumed", || {
            pump.row("", "second-delta").is_some()
        })
        .await;
        let all = statuses.lock().unwrap().clone();
        service.join().await.unwrap();
        for (phase, statuses) in [
            ("RPC in flight", during_rpc),
            ("newer head pending", newer_head_pending),
            ("finished", all),
        ] {
            assert!(
                statuses.iter().all(|s| *s == ComputerStatus::Live),
                "{phase}: {statuses:?}"
            );
        }
    });
}

fn detail_spec(topic: &str, after: Option<i64>) -> zerus_watch_protocol::Subscription {
    zerus_watch_protocol::Subscription {
        topic: topic.into(),
        session: matches!(topic, "activity" | "processes").then(|| {
            zerus_watch_protocol::SessionRef {
                name: "codex/example/session".into(),
                run_id: Some("run".into()),
                conversation_id: Some("conversation".into()),
            }
        }),
        project: (topic == "tasks").then(|| "project".into()),
        after,
        tail: (topic == "activity" && after.is_none()).then_some(100),
        interval_ms: (topic == "processes").then_some(10_000),
        ..Default::default()
    }
}
#[test]
fn one_service_lease_delivers_native_activity_processes_tasks_and_organization() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |change| {
            observe.lock().unwrap().push(change.clone())
        }));
        // Demand can precede the watch hello without creating a fallback.
        for topic in ["activity", "processes", "tasks", "organization"] {
            service.follow_detail("", detail_spec(topic, None)).unwrap();
        }
        eventually("four original native topics",|| {
            let rows = seen.lock().unwrap();
            rows.iter().any(|c|matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::Replace {events,cursor:101},..} if events.len()==2))
                && rows.iter().any(|c|matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::State {value,..},..} if value["message_events"][0]["detail"]=="all-day message"))
                && rows.iter().any(|c|matches!(c,Change::Processes {update:zerus_client_core::ProcessesUpdate::State {..},..}))
                && rows.iter().any(|c|matches!(c,Change::Tasks {update:zerus_client_core::TasksUpdate::List {tasks,..},..} if tasks.len()==1))
                && rows.iter().any(|c|matches!(c,Change::Organization {update:zerus_client_core::OrganizationUpdate::State {..},..}))
        }).await;
        assert!(service.snapshot().details_available);
        assert_eq!(
            service.organization_snapshot().unwrap().digest,
            "org-digest"
        );
        eventually(
            "original Hello physical identity overrides metadata caption identity",
            || {
                pump.machine(&ComputerId::local()).is_some_and(|m| {
                    m.ok == Some(true)
                        && m.machine_id.as_deref() == Some("00000000-0000-4000-8000-000000000009")
                })
            },
        )
        .await;
        assert_eq!(watch_calls(&fixture.calls()), 1);
        assert!(fixture
            .calls()
            .iter()
            .all(|c| c["args"] == json!(["--help"]) || c["args"][0] == "desktop"));
        service.join().await.unwrap();
    });
}
#[test]
fn older_activity_and_changed_process_interval_retire_ids_but_refresh_reuses_demand() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |c| observe.lock().unwrap().push(c.clone())));
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        service
            .follow_detail("", detail_spec("processes", None))
            .unwrap();
        eventually("both initial follows", || {
            fixture
                .calls()
                .iter()
                .filter(|c| c["value"]["op"] == "detail_follow")
                .count()
                == 2
        })
        .await;
        service
            .follow_detail("", detail_spec("activity", Some(0)))
            .unwrap();
        eventually("older native events",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::Events {events,..},..} if events.iter().any(|e|e["seq"]==1)))).await;
        let calls = fixture.calls();
        let activity: Vec<_> = calls
            .iter()
            .filter(|c| {
                c["value"]["op"] == "detail_follow"
                    && c["value"]["subscription"]["topic"] == "activity"
            })
            .collect();
        assert_eq!(activity.len(), 2);
        assert_ne!(activity[0]["value"]["id"], activity[1]["value"]["id"]);
        assert!(activity[1]["value"]["subscription"]["tail"].is_null());
        let old = &activity[0]["value"]["id"];
        let retired = calls
            .iter()
            .position(|c| c["value"]["op"] == "detail_unfollow" && &c["value"]["id"] == old)
            .unwrap();
        let replaced = calls
            .iter()
            .position(|c| c["value"] == activity[1]["value"])
            .unwrap();
        assert!(retired < replaced);
        let process_id = calls
            .iter()
            .find(|c| c["value"]["subscription"]["topic"] == "processes")
            .unwrap()["value"]["id"]
            .clone();
        service.processes_update("", "run:run", None).unwrap();
        eventually("bounded process refresh", || {
            fixture.calls().iter().any(|c| {
                c["value"]["subscription"]["refresh"] == true && c["value"]["id"] == process_id
            })
        })
        .await;
        service.processes_update("", "run:run", Some(2000)).unwrap();
        eventually("new interval uses a fresh view", || {
            fixture.calls().iter().any(|c| {
                c["value"]["subscription"]["interval_ms"] == 2000 && c["value"]["id"] != process_id
            })
        })
        .await;
        service.join().await.unwrap();
    });
}
#[test]
fn detail_sequence_gap_recovers_only_its_route_and_keeps_catalog_and_processes_live() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture =
            Fixture::new(json!({"details":true,"detail_gap":true,"relay_finalize_delay_ms":300}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |c| observe.lock().unwrap().push(c.clone())));
        service
            .follow_detail("", detail_spec("processes", None))
            .unwrap();
        eventually("live catalog and healthy process route", || {
            pump.row("", "row").is_some()
                && seen.lock().unwrap().iter().any(|c| {
                    matches!(
                        c,
                        Change::Processes {
                            update: zerus_client_core::ProcessesUpdate::State { .. },
                            ..
                        }
                    )
                })
        })
        .await;
        seen.lock().unwrap().clear();
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        eventually("only Activity rearmed", || {
            fixture
                .calls()
                .iter()
                .filter(|c| {
                    c["value"]["op"] == "detail_follow"
                        && c["value"]["subscription"]["topic"] == "activity"
                })
                .count()
                == 2
        })
        .await;
        service.processes_update("", "run:run", None).unwrap();
        eventually("healthy process route still delivers",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Processes {update:zerus_client_core::ProcessesUpdate::State {value:_,rev},..} if *rev>1))).await;
        assert!(service.snapshot().connected);
        assert!(pump.row("", "row").is_some());
        assert_eq!(watch_calls(&fixture.calls()), 1);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|c| !matches!(c,Change::Computer {status,..} if *status!=ComputerStatus::Live)));
        let calls = fixture.calls();
        let follows: Vec<_> = calls
            .iter()
            .filter(|c| {
                c["value"]["op"] == "detail_follow"
                    && c["value"]["subscription"]["topic"] == "activity"
            })
            .collect();
        assert_ne!(follows[0]["value"]["id"], follows[1]["value"]["id"]);
        assert!(calls.iter().any(|c| c["value"]["op"] == "detail_unfollow"
            && c["value"]["id"] == follows[0]["value"]["id"]));
        service.join().await.unwrap();
    });
}

#[test]
fn native_activity_gap_keeps_its_subscription_and_delivers_the_replacement_tail() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true,"native_gap":true}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |c| observe.lock().unwrap().push(c.clone())));
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        eventually("new native tail after Gap",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::Replace {events,cursor:200},..} if events.len()==1 && events[0]["seq"]==200))).await;
        assert!(service.snapshot().connected);
        assert_eq!(watch_calls(&fixture.calls()), 1);
        assert_eq!(
            fixture
                .calls()
                .iter()
                .filter(|c| c["value"]["op"] == "detail_follow")
                .count(),
            1
        );
        assert!(!seen.lock().unwrap().iter().any(|c| matches!(
            c,
            Change::Activity {
                update: zerus_client_core::ActivityUpdate::Ended,
                ..
            }
        )));
        service.join().await.unwrap();
    });
}

#[test]
fn timed_out_follow_leaves_only_that_view_unavailable_and_does_not_replay_it() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true,"follow_timeout":true}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |c| observe.lock().unwrap().push(c.clone())));
        service
            .follow_detail("", detail_spec("processes", None))
            .unwrap();
        eventually("healthy process state", || {
            seen.lock().unwrap().iter().any(|c| {
                matches!(
                    c,
                    Change::Processes {
                        update: zerus_client_core::ProcessesUpdate::State { .. },
                        ..
                    }
                )
            })
        })
        .await;
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        eventually("timed out view reports unavailable",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::Connection {ready:false,pending:false,code},..} if code=="service_request_unconfirmed"))).await;
        service.processes_update("", "run:run", None).unwrap();
        eventually("healthy route still refreshes",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Processes {update:zerus_client_core::ProcessesUpdate::State {rev,..},..} if *rev>1))).await;
        assert!(service.snapshot().connected);
        assert!(pump.row("", "row").is_some());
        assert_eq!(watch_calls(&fixture.calls()), 1);
        assert_eq!(
            fixture
                .calls()
                .iter()
                .filter(|c| c["value"]["op"] == "detail_follow"
                    && c["value"]["subscription"]["topic"] == "activity")
                .count(),
            1
        );
        service.join().await.unwrap();
    });
}

#[test]
fn refused_detail_demand_is_visible_without_disconnecting_other_views() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true,"detail_refused":true}));
        let (service, pump) = fixture.start(true);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observe = seen.clone();
        pump.tap(Box::new(move |c| observe.lock().unwrap().push(c.clone())));
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        eventually("explicit unsupported topic",||seen.lock().unwrap().iter().any(|c|
            matches!(c,Change::Activity {update:zerus_client_core::ActivityUpdate::Connection {ready:false,code,..},..} if code=="unsupported_topic"))).await;
        assert!(service.snapshot().connected);
        assert_eq!(watch_calls(&fixture.calls()), 1);
        service.join().await.unwrap();
    });
}

#[test]
fn missing_detail_abi_settles_activity_as_unavailable_without_a_fallback() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let (service, pump) = fixture.start(true);
        eventually("older service connected", || {
            watch_calls(&fixture.calls()) == 1 && service.snapshot().connected
        })
        .await;
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        let batch = pump.take(Duration::ZERO);
        assert!(batch.activity.iter().any(|out| out.kind == "connection"
            && out.payload["ready"] == false
            && out.payload["pending"] == false));
        assert!(!service.snapshot().details_available);
        assert!(!fixture
            .calls()
            .iter()
            .any(|c| c["value"]["op"] == "detail_follow"));
        service.join().await.unwrap();
    });
}

#[test]
fn temporary_rpc_admission_pressure_retries_follow_without_halting_the_view() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"details":true}));
        let (service, pump) = fixture.start(true);
        eventually("metadata and catalog bootstrap drained", || {
            service.snapshot().details_available
                && pump
                    .machine(&ComputerId::local())
                    .is_some_and(|machine| machine.ok == Some(true))
                && service.shared.rpc.snapshot().is_ok_and(|s| s.admitted == 0)
        })
        .await;
        let held: Vec<_> = (0..4)
            .map(|_| service.shared.rpc.admit(Role::Fleet).unwrap())
            .collect();
        service
            .follow_detail("", detail_spec("activity", None))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!fixture
            .calls()
            .iter()
            .any(|c| c["value"]["op"] == "detail_follow"));
        drop(held);
        eventually("one admitted follow after pressure clears", || {
            fixture
                .calls()
                .iter()
                .any(|c| c["value"]["op"] == "detail_follow")
        })
        .await;
        assert!(service.snapshot().connected);
        assert_eq!(watch_calls(&fixture.calls()), 1);
        service.join().await.unwrap();
    });
}
#[test]
fn service_source_roster_bootstraps_catalogs_independently_of_shared_counts() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for config in [
            json!({"counts_degraded":true}),
            json!({"counts_null":true}),
            json!({"counts_computer":"counts-only"}),
        ] {
            let fixture = Fixture::new(config);
            let (service, pump) = fixture.start(true);
            eventually(
                "catalog delivered from the independent source roster",
                || pump.row("", "row").is_some(),
            )
            .await;
            assert!(service.snapshot().connected);
            assert_eq!(watch_calls(&fixture.calls()), 1);
            assert!(fixture
                .calls()
                .iter()
                .filter(|call| call["value"]["op"] == "catalog_page")
                .all(|call| call["value"]["computer"] == ""));
            service.join().await.unwrap();
        }
    });
}
#[test]
fn count_failures_during_status_and_catalog_hints_keep_the_current_catalog_and_transport_live() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"counts_transition":true,"relay_finalize_delay_ms":300}));
        let (service, pump) = fixture.start(true);
        eventually("baseline applied", || {
            pump.row("", "row").is_some()
                && service
                    .shared
                    .dirty
                    .lock()
                    .unwrap()
                    .progress
                    .contains_key("")
        })
        .await;
        let _ = pump.take(Duration::ZERO);
        fs::write(fixture.0.join("drop-counts"), []).unwrap();
        eventually("contiguous delta after degraded counts", || {
            pump.row("", "after-counts").is_some()
        })
        .await;
        assert!(service.snapshot().connected);
        assert!(service.snapshot().attention_counts.is_none());
        let batch = pump.take(Duration::ZERO);
        assert!(batch
            .computers
            .iter()
            .all(|computer| !matches!(computer.status.as_str(), "reconnecting" | "probing")));
        fs::write(fixture.0.join("restore-counts"), []).unwrap();
        eventually("count projection recovers", || {
            service.snapshot().attention_counts.is_some()
        })
        .await;
        assert_eq!(watch_calls(&fixture.calls()), 1);
        assert_eq!(
            fixture
                .calls()
                .iter()
                .filter(|call| call["value"]["op"] == "catalog_page")
                .count(),
            1
        );
        service.join().await.unwrap();
    });
}
#[test]
fn runtime_transport_readiness_does_not_rewalk_a_last_known_catalog_on_every_status() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"coverage_offline":true}));
        let (service, pump) = fixture.start(true);
        eventually("last-known baseline finished", || {
            pump.row("", "row").is_some()
                && service
                    .shared
                    .rpc
                    .snapshot()
                    .is_ok_and(|rpc| rpc.admitted == 0)
                && !service.shared.dirty.lock().unwrap().pending.contains("")
        })
        .await;
        let before = service.shared.dirty.lock().unwrap().sources[""].clone();
        assert!(
            !before.online,
            "catalog coverage, rather than transport, supplies currentness"
        );
        let pages = fixture
            .calls()
            .iter()
            .filter(|call| call["value"]["op"] == "catalog_page")
            .count();
        fs::write(fixture.0.join("repeat-status"), []).unwrap();
        eventually("both runtime status frames processed", || {
            service.snapshot().generation == Some(3)
        })
        .await;
        assert!(
            service.shared.dirty.lock().unwrap().sources[""] == before,
            "runtime transport metadata cannot restart the last-known catalog"
        );
        assert_eq!(
            fixture
                .calls()
                .iter()
                .filter(|call| call["value"]["op"] == "catalog_page")
                .count(),
            pages
        );
        assert!(service.snapshot().connected);
        service.join().await.unwrap();
    });
}

#[test]
fn repeated_catalog_failure_retires_only_its_peer_until_manual_retry_or_a_new_scope() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for scope_change in [false, true] {
            let fixture = Fixture::new(json!({"peer_failure":true}));
            let pump = crate::pump::Pump::default();
            pump.push(Change::Upsert {
                computer: ComputerId::from("broken"),
                key: "cached".into(),
                row: zerus_client_core::view::row(&json!({"name":"codex/example/cached"})),
            });
            let statuses = Arc::new(Mutex::new(Vec::new()));
            let observed = statuses.clone();
            pump.tap(Box::new(move |change| {
                if let Change::Computer { computer, status } = change {
                    observed.lock().unwrap().push((computer.clone(), *status));
                }
            }));
            // Install the one-shot halt observer and cached row before startup.
            let (service, pump) = fixture.start_with_pump(true, pump, None);
            eventually("only broken catalog exhausted its two walks", || {
                statuses.lock().unwrap().contains(&(
                    ComputerId::from("broken"),
                    ComputerStatus::Reconnecting { attempt: 2 },
                ))
            })
            .await;
            assert!(service.snapshot().connected);
            assert!(
                pump.row("broken", "cached").is_some(),
                "failed baseline cannot prune last-known rows"
            );
            assert!(pump.row("", "row").is_some());
            let peer_pages = || {
                fixture
                    .calls()
                    .iter()
                    .filter(|call| {
                        call["value"]["op"] == "catalog_page"
                            && call["value"]["computer"] == "broken"
                    })
                    .count()
            };
            assert_eq!(peer_pages(), 2);
            statuses.lock().unwrap().clear();
            fs::write(fixture.0.join("peer-hints"), []).unwrap();
            eventually(
                "healthy local delta and all repeated peer hints consumed",
                || service.snapshot().generation == Some(2) && pump.row("", "after-peer").is_some(),
            )
            .await;
            // The serial walker has processed its pending work; no time-based
            // assertion can accidentally pass before the final peer hint.
            eventually("pending work drained without another peer RPC", || {
                let drained = {
                    let dirty = service.shared.dirty.lock().unwrap();
                    dirty.pending.is_empty()
                        && dirty.progress.get("")
                            == Some(&Revision::try_from("1".to_owned()).unwrap())
                };
                drained
                    && service
                        .shared
                        .rpc
                        .snapshot()
                        .is_ok_and(|rpc| rpc.admitted == 0)
            })
            .await;
            assert_eq!(
                peer_pages(),
                2,
                "ordinary hints cannot restart a halted peer"
            );
            assert!(statuses
                .lock()
                .unwrap()
                .iter()
                .all(|(computer, status)| !computer.is_local() || *status == ComputerStatus::Live));
            assert_eq!(watch_calls(&fixture.calls()), 1);
            if scope_change {
                fs::write(fixture.0.join("peer-scope-recovered"), []).unwrap();
            } else {
                fs::write(fixture.0.join("peer-recovered"), []).unwrap();
                // Retry now and both native tray Refresh paths call local
                // reconnect first; service mode has no GUI peer listing.
                assert!(service.reconnect(""));
                assert!(!service.reconnect(""), "pending Refresh is not restarted");
            }
            eventually("only recovered peer finishes a new baseline", || {
                pump.row("broken", "cached").is_none()
                    && service
                        .shared
                        .dirty
                        .lock()
                        .unwrap()
                        .progress
                        .contains_key("broken")
            })
            .await;
            assert!(pump.row("", "after-peer").is_some());
            assert!(service.snapshot().connected);
            assert!(!service.reconnect(""), "Refresh leaves live catalogs alone");
            assert!(!service.reconnect("not-listed"));
            assert!(statuses
                .lock()
                .unwrap()
                .iter()
                .all(|(computer, status)| !computer.is_local() || *status == ComputerStatus::Live));
            assert_eq!(
                watch_calls(&fixture.calls()),
                1,
                "peer Retry keeps the shared subscription"
            );
            assert_eq!(
                fixture
                    .calls()
                    .iter()
                    .filter(|call| call["value"]["op"] == "catalog_page"
                        && call["value"]["computer"] == "")
                    .count(),
                1
            );
            service.join().await.unwrap();
        }
    });
}
