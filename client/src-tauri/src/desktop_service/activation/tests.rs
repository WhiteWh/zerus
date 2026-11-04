use super::*;
use crate::desktop_service::{
    frontend::tests::native::{responder, CacheServer},
    tests::Fixture,
};
use std::{sync::mpsc, time::Instant};

// The native responder deliberately refuses an agent-attributed parent. Run
// the GUI-only fixture in an exact child without mutating this harness's env.
fn private_case(case: &'static str) -> bool {
    private_case_as(case, false)
}
fn private_case_as(case: &'static str, agent: bool) -> bool {
    const MARKER: &str = "ZERUS_ACTIVATION_CONSUMER_CASE";
    if std::env::var(MARKER).as_deref() == Ok(case) {
        return true;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    let mut call = Invocation {
        args: [
            "--exact",
            case,
            "--nocapture",
            "--test-threads=1",
            "--color=never",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        stdin: None,
        timeout: Duration::from_secs(30),
        request_id: None,
    };
    let stdout = if agent {
        call.stdin = Some(vec![]);
        // Set attribution only in this original test child, after the runner's
        // ordinary environment clearing. The parallel harness stays unchanged.
        runner::run_owned(
            &std::env::current_exe().unwrap(),
            &call,
            &[(MARKER, case)],
            &AtomicBool::new(false),
            |command| {
                command.env("HGS_SESSION", "codex/fixture/gui-parent");
                command.env("HGS_RUN_ID", "fixture-agent-run");
                command.spawn()
            },
            Arc::new(()),
        )
        .unwrap()
    } else {
        let result = runner::run_without(
            &std::env::current_exe().unwrap(),
            &call,
            &[(MARKER, case)],
            &["HGS_SESSION", "HGS_RUN_ID"],
            64 << 10,
        )
        .unwrap();
        match &result {
            zerus_client_core::Exit::Code { code: 0, .. } => {}
            zerus_client_core::Exit::Code {
                code,
                stdout,
                stderr,
            } => panic!(
                "private GUI case {case} exited {code}\nstderr:\n{stderr}\nstdout:\n{}",
                String::from_utf8_lossy(stdout)
            ),
            other => panic!("private GUI case {case}: {other:?}"),
        }
        let zerus_client_core::Exit::Code { stdout, .. } = result else {
            unreachable!()
        };
        stdout
    };
    assert!(
        String::from_utf8_lossy(&stdout)
            .contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "the exact private GUI case must execute once: {}",
        String::from_utf8_lossy(&stdout)
    );
    false
}

#[test]
fn only_identity_and_invalid_input_preparation_errors_are_permanent() {
    use std::io::ErrorKind;
    for kind in [ErrorKind::PermissionDenied, ErrorKind::InvalidInput] {
        assert_eq!(ReadError::preparation(kind.into()), ReadError::Permanent);
    }
    for kind in [
        ErrorKind::WouldBlock,
        ErrorKind::Other,
        ErrorKind::Interrupted,
    ] {
        assert_eq!(
            ReadError::preparation(kind.into()),
            ReadError::Retryable(Reason::Preparation)
        );
    }
}

#[test]
fn an_agent_attributed_gui_stops_only_the_activation_reader_without_rearming() {
    if !private_case_as("desktop_service::activation::tests::an_agent_attributed_gui_stops_only_the_activation_reader_without_rearming", true) { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    assert_eq!(
        std::env::var("HGS_SESSION").unwrap(),
        "codex/fixture/gui-parent"
    );
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        until("actual native identity refusal stops the reader", || {
            service.shared.activation_permanent.load(Ordering::Acquire)
        })
        .await;
        assert_eq!(
            service.shared.activation_backoffs.load(Ordering::Acquire),
            0
        );
        assert!(
            waits(&fixture).is_empty(),
            "no activation CLI child was spawned"
        );
        assert!(rx.try_recv().is_err());
        assert!(
            service.snapshot().connected,
            "catalog/watch remain available"
        );
        service.join().await.unwrap();
        assert_eq!(
            service.shared.activation_rpc.snapshot().unwrap().admitted,
            0
        );
    });
}

#[test]
fn a_full_native_registration_cache_remains_retryable_without_disabling_the_catalog() {
    if !private_case("desktop_service::activation::tests::a_full_native_registration_cache_remains_retryable_without_disabling_the_catalog") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true}));
        let responder = responder();
        let held: Vec<_> = (0..ui_requests::LIMIT)
            .map(|_| {
                responder
                    .prepare_ui_request(
                        uuid::Uuid::new_v4().to_string(),
                        ui_requests::Request::ActivationWait {
                            request_id: uuid::Uuid::new_v4().to_string(),
                        },
                    )
                    .unwrap()
            })
            .collect();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        until(
            "capacity refusal enters the ordinary fresh-read backoff",
            || service.shared.activation_backoffs.load(Ordering::Acquire) == 1,
        )
        .await;
        assert!(!service.shared.activation_permanent.load(Ordering::Acquire));
        assert!(waits(&fixture).is_empty());
        assert!(rx.try_recv().is_err());
        assert!(service.snapshot().connected);
        service.join().await.unwrap();
        drop(held);
    });
}

fn start(
    fixture: &Fixture,
    responder: Arc<zerus_client_core::desktop::Responder>,
) -> (Arc<Service>, mpsc::Receiver<Delivery>) {
    let (tx, rx) = mpsc::sync_channel(1);
    let pump = crate::pump::Pump::default();
    let (sink, input) = mpsc::sync_channel(256);
    let drain = pump.drain_service(input, |_| {});
    let service = Service::start_observed(
        fixture.0.join("hgs"),
        pump,
        sink,
        true,
        drain,
        Some(responder),
        Observers {
            projections: vec![],
            notification_controls: None,
            retained_sources: None,
            activation: Some(Arc::new(move |delivery| {
                tx.try_send(delivery).ok();
            })),
        },
    );
    (service, rx)
}
async fn until(label: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "{label}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn waits(fixture: &Fixture) -> Vec<Value> {
    fixture
        .calls()
        .into_iter()
        .filter(|call| call["value"]["request"]["op"] == "activation_wait")
        .collect()
}
async fn receive(rx: &mpsc::Receiver<Delivery>) -> Delivery {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(value) = rx.try_recv() {
            return value;
        }
        assert!(Instant::now() < deadline, "activation delivery");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn consume(delivery: Delivery, available: bool) -> crate::desk::Outcome {
    use crate::desk::{Context, DeskInput, DeskLoop};
    use zerus_client_core::{Change, ComputerId, ComputerStatus, ViewRow};
    let target = delivery.open().target.clone();
    let row = ViewRow {
        name: "codex/renamed/exact-session".into(),
        state: if available { "running" } else { "archived" }.into(),
        run_id: Some(target.run),
        conversation_id: Some(target.conversation),
        ..ViewRow::default()
    };
    let computer = ComputerId(target.computer);
    let mut desk = DeskLoop::new(
        zerus_client_core::menubar::OpenMode::Automatic,
        None,
        Instant::now(),
    );
    let mut publish =
        |_: &zerus_client_core::menubar::Desk, _: zerus_client_core::menubar::OpenMode| {};
    let mut optional = || false;
    let mut marks = || None;
    let mut notice = || "fixture".to_owned();
    let mut cx = Context {
        publish: &mut publish,
        now: Instant::now(),
        unix: 0,
        focused: false,
        optional_questions: &mut optional,
        marks: &mut marks,
        notice_id: &mut notice,
    };
    desk.round(
        [
            DeskInput::Change(Box::new(Change::Replace {
                computer: computer.clone(),
                rows: vec![(row.name.clone(), row)],
            })),
            DeskInput::Change(Box::new(Change::Computer {
                computer,
                status: ComputerStatus::Live,
            })),
            DeskInput::ServiceClicked(Box::new(delivery)),
        ],
        &mut cx,
    )
}
#[test]
fn original_bound_child_and_one_held_delivery_keep_reads_serial_and_stop_invalidates_ui_work() {
    if !private_case("desktop_service::activation::tests::original_bound_child_and_one_held_delivery_keep_reads_serial_and_stop_invalidates_ui_work") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        let delivery = receive(&rx).await;
        assert!(fixture.0.join("original-child-bound").exists());
        assert!(delivery.current());
        assert_eq!(delivery.open().target.run, "exact-run");
        assert_eq!(delivery.open().target.conversation, "exact-conversation");
        assert_eq!(
            delivery.open().post_request_id,
            "22222222-2222-4222-8222-222222222222"
        );
        #[cfg(target_os = "linux")]
        assert!(
            matches!(&delivery.open().platform_context, Some(zerus_client_core::desktop::notifications::PlatformContext::Wayland { activation_token }) if activation_token == "ephemeral-fixture")
        );
        #[cfg(target_os = "macos")]
        assert!(delivery.open().platform_context.is_none());
        let native_main = delivery.completion();
        let outcome = consume(delivery, true);
        assert!(outcome.show);
        #[cfg(target_os = "linux")]
        assert_eq!(outcome.activation.as_deref(), Some("ephemeral-fixture"));
        assert_eq!(
            outcome.open,
            Some((
                zerus_client_core::ComputerId("".into()),
                "codex/renamed/exact-session".into()
            ))
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            waits(&fixture).len(),
            1,
            "native-main completion still holds the one delivery"
        );
        // apply() clones this hold into each queued native-main callback.
        // Dropping the desk outcome cannot admit another click while even one
        // of those original callbacks has not actually returned.
        drop(outcome);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(waits(&fixture).len(), 1);
        assert!(native_main.current());
        drop(native_main);
        until("next fresh read after UI consumes the delivery", || {
            waits(&fixture).len() == 2
        })
        .await;
        let calls = waits(&fixture);
        assert_ne!(
            calls[0]["value"]["request"]["request_id"],
            calls[1]["value"]["request"]["request_id"]
        );
        service.join().await.unwrap();
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn a_gone_target_notice_also_retains_the_single_delivery_until_native_ui_consumes_it() {
    if !private_case("desktop_service::activation::tests::a_gone_target_notice_also_retains_the_single_delivery_until_native_ui_consumes_it") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        let outcome = consume(receive(&rx).await, false);
        assert!(outcome.open.is_none());
        assert_eq!(outcome.notices.len(), 1);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(waits(&fixture).len(), 1);
        drop(outcome);
        until("new read after native notice completion", || {
            waits(&fixture).len() == 2
        })
        .await;
        service.join().await.unwrap();
    });
}

#[test]
fn retained_delivery_refuses_a_new_epoch_and_shutdown_does_not_wait_for_a_stalled_view() {
    if !private_case("desktop_service::activation::tests::retained_delivery_refuses_a_new_epoch_and_shutdown_does_not_wait_for_a_stalled_view") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        let delivery = receive(&rx).await;
        assert!(delivery.current());
        service.shared.snapshot.lock().unwrap().epoch = Some(uuid::Uuid::new_v4().to_string());
        assert!(!delivery.current());
        let completion = delivery.completion();
        let outcome = consume(delivery, true);
        assert!(!outcome.show && outcome.open.is_none());
        service.join().await.unwrap();
        assert!(!completion.current());
        assert_eq!(waits(&fixture).len(), 1);
    });
}
#[test]
fn idle_reads_use_new_ids_and_no_click_is_inferred_from_none() {
    if !private_case("desktop_service::activation::tests::idle_reads_use_new_ids_and_no_click_is_inferred_from_none") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture =
            Fixture::new(json!({"ready":true,"activation_abi":true,"activation_idle_first":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        let delivery = receive(&rx).await;
        assert_eq!(waits(&fixture).len(), 2);
        let calls = waits(&fixture);
        assert_ne!(
            calls[0]["value"]["request"]["request_id"],
            calls[1]["value"]["request"]["request_id"]
        );
        service.join().await.unwrap();
        assert!(!delivery.current());
    });
}
#[test]
fn missing_abi_and_invalid_original_reply_never_fall_back_or_replay() {
    if !private_case("desktop_service::activation::tests::missing_abi_and_invalid_original_reply_never_fall_back_or_replay") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for bad in [
            "missing-abi",
            "request",
            "epoch",
            "configuration",
            "target",
            "transport",
        ] {
            let fixture = Fixture::new(
                json!({"ready":true,"activation_abi":bad!="missing-abi","activation_invalid":bad}),
            );
            let responder = responder();
            let _cache = CacheServer::start(&fixture.0, responder.clone());
            let (service, rx) = start(&fixture, responder);
            until("connected source", || service.snapshot().connected).await;
            if bad != "missing-abi" {
                until("one original activation read", || {
                    waits(&fixture).len() == 1
                })
                .await;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            assert_eq!(waits(&fixture).len(), usize::from(bad != "missing-abi"));
            assert!(rx.try_recv().is_err(), "{bad}");
            assert!(
                service.snapshot().connected,
                "missing click ABI does not replace the catalog"
            );
            assert!(!fixture.calls().iter().any(|call| call["args"]
                == json!(["desktop", "request", "--json"])
                && call["value"]["op"] == "activation_wait"));
            service.join().await.unwrap();
        }
    });
}

#[test]
fn a_completed_lost_read_rearms_after_backoff_with_a_fresh_uuid() {
    if !private_case("desktop_service::activation::tests::a_completed_lost_read_rearms_after_backoff_with_a_fresh_uuid") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        assert_eq!(RETRY, Duration::from_secs(60));
        let fixture = Fixture::new(
            json!({"ready":true,"activation_abi":true,"activation_fail_first":"transport"}),
        );
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        until("first original wait", || waits(&fixture).len() == 1).await;
        *service.shared.activation_retry.lock().unwrap() = Some(Duration::from_millis(150));
        let released = Instant::now();
        std::fs::write(fixture.0.join("release-first-activation"), b"").unwrap();
        let delivery = receive(&rx).await;
        assert!(released.elapsed() >= Duration::from_millis(150));
        let calls = waits(&fixture);
        assert_eq!(calls.len(), 2);
        assert_ne!(
            calls[0]["value"]["request"]["request_id"],
            calls[1]["value"]["request"]["request_id"]
        );
        assert!(delivery.current());
        assert_eq!(
            service.shared.activation_backoffs.load(Ordering::Acquire),
            1
        );
        service.join().await.unwrap();
        assert!(!delivery.current());
    });
}

#[test]
fn original_incomplete_pipes_block_rearm_and_remain_visible_through_join() {
    if !private_case("desktop_service::activation::tests::original_incomplete_pipes_block_rearm_and_remain_visible_through_join") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for stop_before_release in [false, true] {
            let fixture = Fixture::new(
                json!({"ready":true,"activation_abi":true,"activation_fail_first":"pipe"}),
            );
            let responder = responder();
            let _cache = CacheServer::start(&fixture.0, responder.clone());
            let (service, rx) = start(&fixture, responder);
            until("first original wait", || waits(&fixture).len() == 1).await;
            *service.shared.activation_retry.lock().unwrap() = Some(Duration::from_millis(100));
            std::fs::write(fixture.0.join("release-first-activation"), b"").unwrap();
            until(
                "owned runner returned while descendant retains pipes",
                || service.shared.activation_backoffs.load(Ordering::Acquire) == 1,
            )
            .await;
            assert!(fixture.0.join("activation-pipe-held").exists());
            tokio::time::sleep(Duration::from_millis(250)).await;
            assert_eq!(
                waits(&fixture).len(),
                1,
                "no overlapping new helper after backoff"
            );
            assert_eq!(
                service.shared.activation_rpc.snapshot().unwrap().admitted,
                1
            );
            assert!(rx.try_recv().is_err());
            if stop_before_release {
                let original = service.clone();
                let (done, mut finished) = oneshot::channel();
                let joining = tauri::async_runtime::spawn(async move {
                    let result = original.join().await;
                    let _ = done.send(());
                    result
                });
                tokio::time::sleep(Duration::from_millis(150)).await;
                assert!(
                    matches!(
                        finished.try_recv(),
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                    ),
                    "join cannot lose the held output readers"
                );
                std::fs::write(fixture.0.join("release-activation-pipe"), b"").unwrap();
                joining.await.unwrap().unwrap();
                assert_eq!(waits(&fixture).len(), 1, "stop does not start another read");
                assert_eq!(
                    service.shared.activation_rpc.snapshot().unwrap().admitted,
                    0
                );
            } else {
                std::fs::write(fixture.0.join("release-activation-pipe"), b"").unwrap();
                let delivery = receive(&rx).await;
                let calls = waits(&fixture);
                assert_eq!(calls.len(), 2);
                assert_ne!(
                    calls[0]["value"]["request"]["request_id"],
                    calls[1]["value"]["request"]["request_id"]
                );
                service.join().await.unwrap();
                assert!(!delivery.current());
            }
        }
    });
}

#[test]
fn stopping_during_the_full_minute_backoff_does_not_wait_for_its_deadline() {
    if !private_case("desktop_service::activation::tests::stopping_during_the_full_minute_backoff_does_not_wait_for_its_deadline") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(
            json!({"ready":true,"activation_abi":true,"activation_fail_first":"transport"}),
        );
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, _) = start(&fixture, responder);
        until("first original wait", || waits(&fixture).len() == 1).await;
        std::fs::write(fixture.0.join("release-first-activation"), b"").unwrap();
        until("production backoff started", || {
            service.shared.activation_backoffs.load(Ordering::Acquire) == 1
        })
        .await;
        assert!(service.shared.activation_retry.lock().unwrap().is_none());
        let stopped = Instant::now();
        service.join().await.unwrap();
        assert!(stopped.elapsed() < Duration::from_secs(10));
        assert_eq!(waits(&fixture).len(), 1);
    });
}

#[test]
fn a_long_activation_read_leaves_all_fleet_slots_and_a_real_detail_follow_available() {
    if !private_case("desktop_service::activation::tests::a_long_activation_read_leaves_all_fleet_slots_and_a_real_detail_follow_available") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":true,"activation_abi":true,"details":true}));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        drop(receive(&rx).await);
        until("second long poll and detail capability", || {
            waits(&fixture).len() == 2
                && service.snapshot().details_available
                && service
                    .shared
                    .activation_rpc
                    .snapshot()
                    .is_ok_and(|s| s.admitted == 1)
        })
        .await;
        let until = Instant::now() + Duration::from_secs(5);
        let mut held = loop {
            let mut held = Vec::new();
            for _ in 0..4 {
                if let Ok(charge) = service.shared.rpc.admit(Role::Fleet) {
                    held.push(charge);
                }
            }
            if held.len() == 4 {
                break held;
            }
            drop(held);
            assert!(
                Instant::now() < until,
                "all four fleet slots remain available"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(service.shared.rpc.snapshot().unwrap().admitted, 4);
        drop(held.pop());
        service
            .follow_detail(
                "",
                zerus_watch_protocol::Subscription {
                    topic: "activity".into(),
                    session: Some(zerus_watch_protocol::SessionRef {
                        name: "codex/example/session".into(),
                        run_id: Some("run".into()),
                        conversation_id: Some("conversation".into()),
                    }),
                    tail: Some(100),
                    ..Default::default()
                },
            )
            .unwrap();
        self::until(
            "real detail follow alongside original activation wait",
            || {
                fixture
                    .calls()
                    .iter()
                    .any(|c| c["value"]["op"] == "detail_follow")
            },
        )
        .await;
        assert_eq!(waits(&fixture).len(), 2);
        assert_eq!(
            service.shared.activation_rpc.snapshot().unwrap().admitted,
            1
        );
        drop(held);
        service.join().await.unwrap();
    });
}

#[cfg(target_os = "macos")]
#[test]
fn a_wayland_context_cannot_be_consumed_as_a_mac_activation() {
    if !private_case("desktop_service::activation::tests::a_wayland_context_cannot_be_consumed_as_a_mac_activation") { return; }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({
            "ready":true,"activation_abi":true,
            "activation_platform_context":{"platform":"wayland","activation_token":"foreign-platform"}
        }));
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, rx) = start(&fixture, responder);
        until("foreign platform reply refused", || {
            service.shared.activation_backoffs.load(Ordering::Acquire) == 1
        })
        .await;
        assert!(rx.try_recv().is_err());
        assert_eq!(waits(&fixture).len(), 1);
        assert!(service.snapshot().connected);
        service.join().await.unwrap();
    });
}
