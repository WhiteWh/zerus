#[cfg(target_os = "linux")]
use super::*;
use serde_json::json;

#[test]
fn startup_diagnostics_select_only_closed_codes_and_never_child_content() {
    use super::{failure_code, OwnedError};
    assert_eq!(
        failure_code(
            &OwnedError::ChildFailed,
            Some("hgs: frontend process image unavailable\n")
        ),
        "frontend_process_image_unavailable"
    );
    for body in [
        "/private/fixture-secret",
        "hgs: frontend process image unavailable\n/private/fixture-secret",
        "hgs: agent-descended frontend request refused: /private/fixture-secret",
    ] {
        assert_eq!(
            failure_code(&OwnedError::ChildFailed, Some(body)),
            "child_failed"
        );
    }
    assert_eq!(
        failure_code(
            &OwnedError::ExitUnverified,
            Some("hgs: frontend process image unavailable")
        ),
        "exit_unverified"
    );
}

#[test]
fn a_remote_projection_cannot_supply_the_native_ownership_overlay() {
    let view: zerus_client_core::desktop::service::Snapshot = serde_json::from_value(json!({
        "notification_ownership":{"status":{"state":"service","identity_current":true}},
        "connected":false,"epoch":null,"generation":null,"background":{},
        "attention_counts":null,"automatic_approvals":null,"details_available":false,"code":""
    }))
    .unwrap();
    assert!(view.notification_ownership.is_none());
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use crate::desktop_service::frontend::tests::native::{responder, CacheServer};
    use serde_json::Value;
    use std::{fs, os::unix::fs::DirBuilderExt};

    struct Fixture {
        root: PathBuf,
        owner: Arc<Controller>,
        _cache: CacheServer,
    }
    impl Fixture {
        fn new(mode: Mode, config: Value) -> Self {
            let root =
                std::env::temp_dir().join(format!("zerus-ownership-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
            std::os::unix::fs::symlink(
                source.join("desktop_ownership/fake_hgs.py"),
                root.join("hgs"),
            )
            .unwrap();
            std::os::unix::fs::symlink(
                source.join("desktop_service/fake_hgs.py"),
                root.join("catalog-fixture.py"),
            )
            .unwrap();
            fs::write(root.join("fixture.json"), config.to_string()).unwrap();
            let responder = responder();
            assert!(responder.configure_ownership_mode_once(mode));
            let cache = CacheServer::start(&root, responder.clone());
            Self {
                owner: Controller::new(root.join("hgs"), Some(responder), mode),
                root,
                _cache: cache,
            }
        }
        fn run(&self, action: Option<Action>) -> View {
            let job = self
                .owner
                .admit(action, Some("a".repeat(64)), None)
                .unwrap();
            job.run();
            drop(job);
            self.owner.view()
        }
        fn config(&self, value: Value) {
            fs::write(self.root.join("fixture.json"), value.to_string()).unwrap();
        }
        fn calls(&self, command: &str) -> Vec<Value> {
            fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|call| call["args"][1] == command)
                .collect()
        }
        fn service(&self) -> (Arc<crate::desktop_service::Service>, crate::pump::Pump) {
            let pump = crate::pump::Pump::default();
            let (sink, rx) = std::sync::mpsc::sync_channel(256);
            let drain = pump.drain_service(rx, |_| {});
            let service = crate::desktop_service::Service::start(
                self.root.join("hgs"),
                pump.clone(),
                sink,
                true,
                drain,
                self.owner.responder.clone(),
                vec![],
            );
            (service, pump)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.owner.stop();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn startup_grant_uses_original_mode_and_only_revocation_requires_explicit_rearm() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Service, json!({"state":"identity_changed"}));
        let view = f.run(Some(Action::Grant));
        assert!(!view.pending, "{view:?}");
        assert!(view.code.is_empty(), "{view:?}");
        assert!(f.owner.granted());
        assert!(f.root.join("bound").exists());
        assert_eq!(
            f.calls("frontend-request")[0]["value"]["request"]["action"],
            "grant"
        );

        let revoked = Fixture::new(
            Mode::Service,
            json!({"state":"revoked","rearm_required":true}),
        );
        let view = revoked.run(Some(Action::Grant));
        assert_eq!(view.code, "ownership_rearm_required");
        assert!(revoked.calls("frontend-request").is_empty());
        revoked.run(None); // Mount/check never rearms.
        assert!(revoked.calls("frontend-request").is_empty());
        assert!(revoked.run(Some(Action::Rearm)).code.is_empty());
        assert_eq!(revoked.calls("frontend-request").len(), 1);
        assert_eq!(
            revoked.calls("frontend-request")[0]["value"]["request"]["action"],
            "rearm"
        );
    }

    #[test]
    fn lost_original_is_only_queried_and_missing_requires_a_fresh_definitive_negative() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Service, json!({"lost":true,"missing":true}));
        let original = f.run(Some(Action::Grant));
        assert!(original.pending);
        assert!(f.owner.state().intent.as_ref().unwrap().exit_confirmed);
        let same = f.run(Some(Action::Rearm));
        assert_eq!(same.request_id, original.request_id);
        assert!(same.pending, "missing is not definitive");
        assert_eq!(f.calls("frontend-request").len(), 1);
        assert_eq!(f.calls("ownership-result").len(), 1);

        f.config(json!({"missing":true,"definitive":true}));
        // Even a definitive lookup is nonterminal without verified original exit.
        f.owner.state().intent.as_mut().unwrap().exit_confirmed = false;
        assert!(f.run(None).pending);
        f.owner.state().intent.as_mut().unwrap().exit_confirmed = true;
        let settled = f.run(None);
        assert!(!settled.pending);
        assert_eq!(settled.code, "ownership_not_applied");
        f.config(json!({}));
        let fresh = f.run(Some(Action::Rearm));
        assert!(!fresh.pending);
        assert_ne!(fresh.request_id, original.request_id);
        assert_eq!(f.calls("frontend-request").len(), 2);
        assert_ne!(
            f.calls("frontend-request")[1]["value"]["request"]["request_id"],
            original.request_id.unwrap()
        );
    }

    #[test]
    fn pending_drain_and_historical_receipts_never_release_native_posting() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Native, json!({"pending":true}));
        let original = f.run(Some(Action::NativeTakeover));
        assert!(original.pending);
        assert!(!f
            .owner
            .state()
            .intent
            .as_ref()
            .unwrap()
            .receipt
            .as_ref()
            .unwrap()
            .native_ready());
        fs::write(f.root.join("settle"), []).unwrap();
        let settled = f.run(None);
        assert!(!settled.pending);
        assert_eq!(original.request_id, settled.request_id);
        assert_eq!(f.calls("frontend-request").len(), 1);
        assert!(f
            .owner
            .state()
            .intent
            .as_ref()
            .unwrap()
            .receipt
            .as_ref()
            .unwrap()
            .native_ready());

        let stale = Fixture::new(Mode::Native, json!({"stale":true}));
        assert_eq!(
            stale.run(Some(Action::NativeTakeover)).code,
            "ownership_changed"
        );
        assert!(!stale
            .owner
            .state()
            .intent
            .as_ref()
            .unwrap()
            .receipt
            .as_ref()
            .unwrap()
            .native_ready());
    }

    #[test]
    fn both_markers_configuration_and_native_mode_are_required_before_mutation() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        for config in [
            json!({"ownership_abi":false}),
            json!({"frontend_abi":false}),
            json!({"configuration":"b".repeat(64)}),
        ] {
            let f = Fixture::new(Mode::Service, config);
            assert!(!f.run(Some(Action::Grant)).code.is_empty());
            assert!(f.calls("frontend-request").is_empty());
        }
        let f = Fixture::new(Mode::Service, json!({}));
        assert_eq!(
            f.run(Some(Action::NativeTakeover)).code,
            "ownership_mode_mismatch"
        );
        assert!(f.calls("frontend-request").is_empty());
    }

    #[test]
    fn startup_retries_only_a_definitive_image_drain_refusal_with_new_bounded_intents() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            for (config, count, granted) in [
                (
                    json!({"refuse":"ownership_drain_pending","refuse_once":true}),
                    2,
                    true,
                ),
                (json!({"refuse":"ownership_drain_pending"}), 3, false),
                (
                    json!({"refuse":"ownership_resources_pending","refuse_once":true}),
                    2,
                    true,
                ),
                (
                    json!({"refuse":"ownership_transport_busy","refuse_once":true}),
                    2,
                    true,
                ),
                (json!({"refuse":"ownership_rearm_required"}), 1, false),
                (json!({"lost":true}), 1, false),
                (json!({"pending":true}), 1, false),
            ] {
                let f = Fixture::new(Mode::Service, config);
                let result = f
                    .owner
                    .startup_grant("a".repeat(64), Arc::new(AtomicBool::new(false)))
                    .await;
                let calls = f.calls("frontend-request");
                assert_eq!(calls.len(), count, "{result:?}");
                let ids: std::collections::BTreeSet<_> = calls
                    .iter()
                    .map(|call| call["value"]["request"]["request_id"].as_str().unwrap())
                    .collect();
                assert_eq!(ids.len(), count, "no old mutation is resent");
                assert_eq!(
                    f.calls("ownership-status").len(),
                    count,
                    "each new intent uses fresh scope"
                );
                assert_eq!(f.owner.granted(), granted);
            }
        });
    }

    #[test]
    fn local_transport_capacity_recovers_without_an_original_to_replay() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Service, json!({}));
        let held = [
            f.owner.gate.admit(Role::Fleet).unwrap(),
            f.owner.gate.admit(Role::Fleet).unwrap(),
        ];
        let owner = f.owner.clone();
        let worker = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while owner.view().code != "ownership_transport_busy" {
                assert!(
                    Instant::now() < until,
                    "the first admission must observe capacity"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(held);
        });
        let view = tauri::async_runtime::block_on(
            f.owner
                .startup_grant("a".repeat(64), Arc::new(AtomicBool::new(false))),
        );
        worker.join().unwrap();
        assert!(view.code.is_empty(), "{view:?}");
        assert_eq!(f.calls("frontend-request").len(), 1);
        assert_eq!(f.calls("ownership-status").len(), 1);
    }

    #[test]
    fn a_fresh_revoked_scope_or_read_failure_cannot_reuse_an_old_transient_refusal() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        for changed in [
            json!({"state":"revoked","rearm_required":true}),
            json!({"configuration":"b".repeat(64)}),
        ] {
            let f = Fixture::new(Mode::Service, json!({"refuse":"ownership_drain_pending"}));
            f.run(Some(Action::Grant));
            assert!(f.owner.retryable_grant());
            f.config(changed);
            f.run(None);
            assert!(!f.owner.retryable_grant());
            assert_eq!(f.calls("frontend-request").len(), 1);
        }
    }

    #[test]
    fn the_actual_service_connection_retries_slow_drain_and_preserves_explicit_rearm() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            for (revoked, slow) in [(false, false), (true, false), (false, true)] {
                let f = Fixture::new(
                    Mode::Service,
                    json!({"ready":true,"rearm_required":revoked,
                    "state":if revoked {"revoked"} else {"identity_changed"},
                    "refuse":if slow {Some("ownership_drain_pending")} else {None}, "refuse_once":slow}),
                );
                let pump = crate::pump::Pump::default();
                let (sink, rx) = std::sync::mpsc::sync_channel(256);
                let drain = pump.drain_service(rx, |_| {});
                let service = crate::desktop_service::Service::start(
                    f.root.join("hgs"),
                    pump,
                    sink,
                    true,
                    drain,
                    f.owner.responder.clone(),
                    vec![],
                );
                let until = Instant::now() + Duration::from_secs(10);
                while !service.snapshot().connected || f.calls("watch").is_empty() {
                    assert!(Instant::now() < until, "{:?}", service.snapshot());
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert_eq!(
                    f.calls("frontend-request").len(),
                    usize::from(!revoked) + usize::from(slow)
                );
                if revoked {
                    assert_eq!(
                        service.snapshot().notification_ownership.as_ref().unwrap()["status"]
                            ["rearm_required"],
                        true
                    );
                    service.ownership_result().await;
                    assert!(f.calls("frontend-request").is_empty());
                    let result = service.ownership_rearm().await;
                    assert!(!result.pending && result.code.is_empty(), "{result:?}");
                    assert_eq!(f.calls("frontend-request").len(), 1);
                    assert_eq!(
                        f.calls("frontend-request")[0]["value"]["request"]["action"],
                        "rearm"
                    );
                }
                service.join().await.unwrap();
            }
        });
    }

    async fn until(label: &str, check: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !check() {
            assert!(Instant::now() < until, "{label}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn long_grant_recovery_keeps_watch_and_catalog_live_until_a_later_fresh_grant() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            let f = Fixture::new(
                Mode::Service,
                json!({"ready":true,"refuse":"ownership_drain_pending","refuse_count":4}),
            );
            let (service, pump) = f.service();
            until(
                "initial prompt attempts end but watch and catalog stay live",
                || {
                    f.calls("frontend-request").len() >= 3
                        && f.calls("watch").len() == 1
                        && pump.row("", "row").is_some()
                },
            )
            .await;
            assert!(service.snapshot().connected);
            until(
                "a later capped-backoff intent grants without user Retry",
                || {
                    service
                        .snapshot()
                        .notification_ownership
                        .as_ref()
                        .is_some_and(|v| v["status"]["state"] == "service" && v["code"] == "")
                },
            )
            .await;
            let calls = f.calls("frontend-request");
            assert_eq!(calls.len(), 5);
            let ids: std::collections::BTreeSet<_> = calls
                .iter()
                .map(|c| c["value"]["request"]["request_id"].as_str().unwrap())
                .collect();
            assert_eq!(ids.len(), 5);
            assert_eq!(f.calls("ownership-status").len(), 5);
            assert_eq!(
                f.calls("watch").len(),
                1,
                "the connection is not replaced to retry a grant"
            );
            assert!(pump.row("", "row").is_some());
            service.join().await.unwrap();
        });
    }

    #[test]
    fn long_grant_recovery_stops_when_a_new_original_becomes_unknown() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            let f = Fixture::new(
                Mode::Service,
                json!({"ready":true,"refuse":"ownership_resources_pending"}),
            );
            let (service, _) = f.service();
            until("watch is open after the initial three refusals", || {
                f.calls("watch").len() == 1
            })
            .await;
            assert_eq!(f.calls("frontend-request").len(), 3);
            f.config(json!({"ready":true,"lost":true}));
            until("the fourth original is uncertain", || {
                service
                    .snapshot()
                    .notification_ownership
                    .as_ref()
                    .is_some_and(|v| v["pending"] == true && v["running"] == false)
            })
            .await;
            assert_eq!(f.calls("frontend-request").len(), 4);
            tokio::time::sleep(Duration::from_millis(2200)).await;
            assert_eq!(
                f.calls("frontend-request").len(),
                4,
                "unknown is never a transient refusal"
            );
            service.join().await.unwrap();
        });
    }

    #[test]
    fn connection_close_joins_a_long_grant_backoff_without_another_attempt() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            let f = Fixture::new(
                Mode::Service,
                json!({"ready":true,"refuse":"ownership_drain_pending"}),
            );
            let (service, _) = f.service();
            until("fourth refusal enters the next long backoff", || {
                let calls = f.calls("frontend-request");
                calls.len() == 4
                    && service
                        .snapshot()
                        .notification_ownership
                        .as_ref()
                        .is_some_and(|v| {
                            v["running"] == false
                                && v["request_id"] == calls[3]["value"]["request"]["request_id"]
                        })
            })
            .await;
            tokio::time::timeout(Duration::from_secs(5), service.join())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(f.calls("frontend-request").len(), 4);
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(f.calls("frontend-request").len(), 4);
        });
    }

    #[test]
    fn native_bootstrap_requires_exact_settlement_and_all_original_resources() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Native, json!({}));
        let proof =
            native_takeover(f.root.join("hgs"), f.owner.responder.clone().unwrap()).unwrap();
        assert_eq!(proof.helper().unwrap(), f.root.join("hgs"));
        assert!(proof._receipt.as_ref().unwrap().native_ready());
        assert_eq!(f.calls("frontend-request").len(), 1);
        let no_marker = Fixture::new(Mode::Native, json!({"ownership_abi":false}));
        let legacy = native_takeover(
            no_marker.root.join("hgs"),
            no_marker.owner.responder.clone().unwrap(),
        )
        .unwrap();
        assert!(legacy._receipt.is_none());
        assert!(no_marker.calls("ownership-status").is_empty());
        assert!(no_marker.calls("frontend-request").is_empty());
    }

    #[test]
    fn never_service_native_start_consumes_only_its_original_authenticated_noop_reply() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Native, json!({"never_service":true}));
        let proof =
            native_takeover(f.root.join("hgs"), f.owner.responder.clone().unwrap()).unwrap();
        assert_eq!(
            proof._receipt.as_ref().unwrap().outcome,
            Outcome::NativeAbsent
        );
        assert!(!proof._receipt.as_ref().unwrap().durable);
        assert!(proof._receipt.as_ref().unwrap().journal.is_none());
        assert_eq!(f.calls("frontend-request").len(), 1);
        assert!(f.calls("ownership-result").is_empty());

        let lost = Fixture::new(Mode::Native, json!({"never_service":true,"lost":true}));
        let original = lost.run(Some(Action::NativeTakeover));
        assert!(original.pending);
        // Even a correctly-shaped original no-op is forbidden in lookup.
        lost.config(json!({"never_service":true}));
        assert!(lost.run(None).pending);
        assert!(lost
            .owner
            .state()
            .intent
            .as_ref()
            .unwrap()
            .receipt
            .is_none());
        lost.config(json!({"never_service":true,"missing":true,"definitive":true}));
        assert!(
            lost.run(None).pending,
            "an unjournaled missing original cannot settle"
        );
        assert_eq!(lost.calls("frontend-request").len(), 1);
    }

    #[test]
    fn two_lost_noop_intents_exit_fenced_within_the_original_budget() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(
            Mode::Native,
            json!({"never_service":true,"lost":true,"missing":true}),
        );
        let original = f.run(Some(Action::NativeTakeover));
        assert!(original.pending);
        let before = Instant::now();
        let result = native_startup(f.owner.clone(), before + Duration::from_secs(3));
        assert!(
            result.is_err(),
            "an absent lookup cannot mint native posting permission"
        );
        assert!(before.elapsed() < Duration::from_secs(6));
        assert_ne!(f.owner.view().request_id, original.request_id);
        assert_eq!(
            f.owner
                .state()
                .retired_native
                .as_ref()
                .unwrap()
                .request
                .id(),
            original.request_id.as_deref().unwrap()
        );
        assert!(f.owner.view().pending);
        assert_eq!(f.calls("frontend-request").len(), 2);
        assert!(!f.calls("ownership-result").is_empty());
    }

    #[test]
    fn lost_initial_noop_admits_only_one_fresh_authenticated_takeover_and_uses_its_result() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(
            Mode::Native,
            json!({"never_service":true,"lost":true,"lost_once":true}),
        );
        let proof =
            native_takeover(f.root.join("hgs"), f.owner.responder.clone().unwrap()).unwrap();
        assert!(proof._receipt.as_ref().unwrap().native_ready());
        let calls = f.calls("frontend-request");
        assert_eq!(calls.len(), 2);
        let old = &calls[0]["value"]["request"]["request_id"];
        let fresh = &calls[1]["value"]["request"]["request_id"];
        assert_ne!(old, fresh);
        assert_eq!(
            proof._receipt.as_ref().unwrap().request_id,
            fresh.as_str().unwrap()
        );
        assert!(
            f.calls("ownership-status").len() >= 3,
            "fresh absent observation and fresh original scope"
        );
    }

    #[test]
    fn an_unverified_exit_or_existing_journal_never_gets_the_absent_native_exception() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        for (never_service, confirmed) in [(true, false), (false, true)] {
            let f = Fixture::new(
                Mode::Native,
                json!({"never_service":never_service,"lost":true,"missing":true}),
            );
            f.run(Some(Action::NativeTakeover));
            f.owner.state().intent.as_mut().unwrap().exit_confirmed = confirmed;
            let mut job = f
                .owner
                .admit(Some(Action::NativeTakeover), None, None)
                .unwrap();
            job.refresh_absent_native = true;
            job.run();
            drop(job);
            assert!(f.owner.view().pending);
            assert!(f.owner.state().retired_native.is_none());
            assert_eq!(f.calls("frontend-request").len(), 1);
        }
        // Replacing the CLI by an older one cannot erase an unknown original.
        let f = Fixture::new(Mode::Native, json!({"never_service":true,"lost":true}));
        f.run(Some(Action::NativeTakeover));
        f.config(json!({"ownership_abi":false}));
        let mut job = f
            .owner
            .admit(Some(Action::NativeTakeover), None, None)
            .unwrap();
        job.refresh_absent_native = true;
        job.run();
        drop(job);
        assert!(f.owner.state().legacy.is_none());
        assert!(f.owner.view().pending);
    }

    #[test]
    fn an_original_reply_with_a_different_scope_or_fingerprint_remains_unconfirmed() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        for field in ["request_id", "configuration", "fingerprint"] {
            let f = Fixture::new(Mode::Native, json!({"bad_receipt":field}));
            let view = f.run(Some(Action::NativeTakeover));
            assert!(view.pending, "{field}: {view:?}");
            assert_eq!(view.code, "ownership_receipt_mismatch");
            assert!(f.owner.state().intent.as_ref().unwrap().receipt.is_none());
            assert_eq!(f.calls("frontend-request").len(), 1);
        }
    }

    #[test]
    fn an_original_descendants_pipe_delays_native_ready_without_replaying_takeover() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        let f = Fixture::new(Mode::Native, json!({"held_pipe":true}));
        let helper = f.root.join("hgs");
        let responder = f.owner.responder.clone().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(native_takeover(helper, responder)).unwrap();
        });
        let until = Instant::now() + Duration::from_secs(12);
        while f.calls("ownership-result").is_empty() {
            assert!(
                Instant::now() < until,
                "lookup must follow the bounded original exchange"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            rx.try_recv().is_err(),
            "a settled actor receipt does not drain the original pipes"
        );
        assert!(f.root.join("pipe-held").exists());
        fs::write(f.root.join("release-pipe"), []).unwrap();
        let proof = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert!(proof._receipt.as_ref().unwrap().native_ready());
        worker.join().unwrap();
        assert_eq!(f.calls("frontend-request").len(), 1);
    }
}
