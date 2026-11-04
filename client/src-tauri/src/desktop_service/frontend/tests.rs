use super::*;
fn approval(enabled: bool) -> Request {
    Request::AutomaticApprovals {
        request_id: uuid::Uuid::new_v4().to_string(),
        enabled,
    }
}
#[test]
fn ownership_is_not_admitted_through_the_generic_policy_command_or_receipt_parser() {
    use zerus_client_core::desktop::ownership::Action;
    assert!(serde_json::from_value::<Operation>(json!("desktop_ownership")).is_err());
    let request = Request::DesktopOwnership {
        request_id: uuid::Uuid::new_v4().to_string(),
        action: Action::Grant,
        journal: None,
        revision: 0,
    };
    assert!(checked(
        &request,
        json!({"request_id":request.id(),"op":"desktop_ownership","accepted":true,"durable":true}),
        false
    )
    .is_none());
    assert!(checked(&request, json!({"request_id":request.id(),"accepted":false,"durable":false,"code":"frontend_authentication_refused"}), false).is_none());
}
#[test]
fn notification_controls_are_excluded_from_generic_policy_and_receipt_handling() {
    use zerus_client_core::desktop::notification_control::{Command, Scope};
    assert!(serde_json::from_value::<Operation>(json!("notification_control")).is_err());
    let request = Request::NotificationControl {
        scope: Scope {
            configuration: "a".repeat(64),
            service_epoch: uuid::Uuid::new_v4().to_string(),
        },
        control: Command::Focus {
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    };
    assert!(checked(
        &request,
        json!({"request_id":request.id(),"accepted":true,"durable":true}),
        false
    )
    .is_none());
    assert!(lookup_receipt(&request,"query",json!({"request_id":"query","original_request_id":request.id(),"accepted":true,"durable":true})).is_err());
}
#[test]
fn actual_root_status_controls_client_readiness_but_never_blocks_connected_disable() {
    // Produced by the root private Store/Runtime projection and decorator test.
    // Regenerate/check with client/scripts/generate-delivery-status-fixture.py.
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fixtures/desktop-delivery-status.json"
    )))
    .unwrap();
    assert_eq!(cases.len(), 7);
    for case in cases {
        let label = case["case"].as_str().unwrap();
        let status = &case["status"];
        let (reads, approvals, enabled) = match label {
            "awaiting_handoff" => (false, false, None),
            "disabled_but_ready" | "read_degraded" => (label != "read_degraded", true, Some(false)),
            "enabled" => (true, true, Some(true)),
            "approval_degraded" => (true, false, Some(true)),
            "disabled_while_degraded" => (true, false, Some(false)),
            "drained" => (false, false, Some(false)),
            _ => panic!("unexpected real root case"),
        };
        let mut state =
            service::State::new(status["configuration"].as_str().unwrap().into()).unwrap();
        state.status(status).unwrap();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.component_ready("reads"), reads, "{label}");
        assert_eq!(snapshot.component_ready("approvals"), approvals, "{label}");
        assert_eq!(snapshot.automatic_approvals, enabled, "{label}");
        assert!(
            submission_ready(snapshot, &approval(false)),
            "connected disable: {label}"
        );
        assert_eq!(
            submission_ready(snapshot, &approval(true)),
            approvals,
            "enable: {label}"
        );
        let mut disconnected = snapshot.clone();
        disconnected.connected = false;
        assert!(!submission_ready(&disconnected, &approval(false)));
        let mut unknown_epoch = snapshot.clone();
        unknown_epoch.epoch = None;
        assert!(!submission_ready(&unknown_epoch, &approval(false)));
    }
}

fn imported() -> Request {
    let source=zerus_client_core::desktop::pending_reads::Capture::from_bytes(br#"[{"computer":"example","name":"agent/project/test","run_id":"run","conversation_id":"conversation","reply_id":"reply","attention_signature":null}]"#).unwrap();
    Request::ImportPendingReads {
        request_id: uuid::Uuid::new_v4().to_string(),
        page: Box::new(source.page(0).unwrap()),
    }
}
fn imported_receipt(request: &Request) -> Value {
    let Request::ImportPendingReads { request_id, page } = request else {
        panic!("import")
    };
    json!({"request_id":request_id,"op":"import_pending_reads","accepted":true,"durable":true,
        "source":page.source,"after":page.after,"through":page.through(),"superseded":0,"delivered":false})
}

#[test]
fn imported_page_requires_its_original_source_cursor_and_durable_transfer_receipt() {
    let request = imported();
    let receipt = imported_receipt(&request);
    assert!(checked(&request, receipt.clone(), false).unwrap().1);
    for (field, wrong) in [
        ("after", json!(1)),
        ("through", json!(999)),
        ("superseded", json!(2)),
        ("source", json!({})),
        ("durable", json!(false)),
        ("delivered", json!(true)),
        ("request_id", json!(uuid::Uuid::new_v4().to_string())),
    ] {
        let mut changed = receipt.clone();
        changed[field] = wrong;
        assert!(checked(&request, changed, false).is_none(), "{field}");
    }
    assert!(
        serde_json::from_value::<Operation>(json!("import_pending_reads")).is_err(),
        "native-only operation"
    );
}

#[test]
fn lost_import_reply_only_looks_up_the_original_page_and_then_advances() {
    let frontend = Frontend::default();
    let request = imported();
    let Begin::Work(mut work) =
        frontend.begin(Operation::ImportPendingReads, Some(request.clone()))
    else {
        panic!("initial")
    };
    work.record_original_exit(&Err(OwnedError::Incomplete));
    drop(work);
    let Begin::Work(work) = frontend.begin(Operation::ImportPendingReads, None) else {
        panic!("lookup")
    };
    assert!(work.lookup);
    assert_eq!(work.request, request);
    let id = uuid::Uuid::new_v4().to_string();
    let receipt = imported_receipt(&request);
    let read = json!({"op":"pending_reads_import_result","request_id":id,"original_request_id":request.id(),
        "accepted":true,"durable":false,"found":true,"result":receipt});
    let receipt = lookup_receipt(&request, &id, read).unwrap();
    assert_eq!(finish_receipt(work, receipt)["pending"], false);
    let next = imported();
    assert!(matches!(
        frontend.begin(Operation::ImportPendingReads, Some(next)),
        Begin::Work(_)
    ));
}
#[test]
fn an_unknown_original_intent_only_allows_receipt_lookup_and_blocks_a_new_writer() {
    let state = Frontend::default();
    let original = approval(true);
    let Begin::Work(work) = state.begin(Operation::AutomaticApprovals, Some(original.clone()))
    else {
        panic!("reserved")
    };
    assert!(!work.lookup);
    let Begin::Immediate(busy) = state.begin(Operation::AutomaticApprovals, Some(original.clone()))
    else {
        panic!("busy")
    };
    assert_eq!(busy["accepted"], Value::Null);
    drop(work); // A stopped/panicked/cancelled worker remains uncertain.
    let Begin::Immediate(blocked) =
        state.begin(Operation::AutomaticApprovals, Some(approval(false)))
    else {
        panic!("blocked")
    };
    assert_eq!(blocked["accepted"], false);
    let Begin::Work(query) = state.begin(Operation::AutomaticApprovals, Some(original.clone()))
    else {
        panic!("lookup")
    };
    assert!(query.lookup);
    assert_eq!(query.request, original);
}
#[test]
fn a_durable_receipt_releases_the_operation_and_the_same_uuid_never_replays() {
    let state = Frontend::default();
    let request = approval(true);
    let Begin::Work(work) = state.begin(Operation::AutomaticApprovals, Some(request.clone()))
    else {
        panic!("reserved")
    };
    let receipt = json!({"request_id":request.id(),"op":"automatic_approvals","enabled":true,"accepted":true,"durable":true});
    let (receipt, complete) = checked(&request, receipt, false).unwrap();
    assert!(complete);
    work.finish(receipt.clone(), true);
    let Begin::Immediate(cached) = state.begin(Operation::AutomaticApprovals, Some(request)) else {
        panic!("cached")
    };
    assert_eq!(cached, receipt);
    assert!(matches!(
        state.begin(Operation::AutomaticApprovals, Some(approval(false))),
        Begin::Work(_)
    ));
}
#[test]
fn missing_receipts_release_only_intents_whose_original_child_exit_was_confirmed() {
    let outcomes = [
        Ok(vec![]),
        Err(OwnedError::ChildFailed),
        Err(OwnedError::Incomplete),
        Err(OwnedError::TimedOut),
        Err(OwnedError::WriteFailed),
        Err(OwnedError::OutputTooLarge),
        Err(OwnedError::Cancelled),
        Err(OwnedError::ExitUnverified),
    ];
    for outcome in outcomes {
        let state = Frontend::default();
        let request = approval(true);
        let Begin::Work(mut original) =
            state.begin(Operation::AutomaticApprovals, Some(request.clone()))
        else {
            panic!("original reservation")
        };
        original.record_original_exit(&outcome);
        // Even a worker interrupted after the exchange retains the exit proof.
        drop(original);
        let Begin::Work(query) = state.begin(Operation::AutomaticApprovals, None) else {
            panic!("receipt query")
        };
        assert!(query.lookup);
        let confirmed = outcome != Err(OwnedError::ExitUnverified);
        assert_eq!(query.original_exit_confirmed, confirmed);
        let result = finish_receipt(query, Receipt::Absent);
        assert_eq!(result["pending"], !confirmed);
        let next = approval(false);
        if confirmed {
            assert_eq!(result["code"], "frontend_request_not_applied");
            let Begin::Immediate(cached) =
                state.begin(Operation::AutomaticApprovals, Some(request))
            else {
                panic!("original ID must stay refused, never sent again")
            };
            assert_eq!(cached, result);
            assert!(matches!(
                state.begin(Operation::AutomaticApprovals, Some(next)),
                Begin::Work(_)
            ));
        } else {
            let Begin::Immediate(blocked) = state.begin(Operation::AutomaticApprovals, Some(next))
            else {
                panic!("an unverified original exit cannot admit another writer")
            };
            assert_eq!(blocked["code"], "previous_frontend_request_unconfirmed");
        }
    }
}
#[test]
fn absent_receipts_require_an_exact_successful_original_id_lookup() {
    let approval = approval(true);
    let maintenance = Request::maintenance_configure(
        uuid::Uuid::new_v4().to_string(),
        maintenance::Configuration {
            update: None,
            recovery: None,
        },
    );
    let query = uuid::Uuid::new_v4().to_string();
    for request in [approval, maintenance] {
        let absent = if Operation::of(&request) == Operation::AutomaticApprovals {
            json!({"request_id":query,"original_request_id":request.id(),"op":"delivery_result","accepted":true,"durable":false,"found":false,"result":null})
        } else {
            json!({"request_id":query,"original_request_id":request.id(),"accepted":false,"code":"unknown_request"})
        };
        assert!(matches!(
            lookup_receipt(&request, &query, absent.clone()),
            Ok(Receipt::Absent)
        ));
        for (field, wrong) in [
            ("request_id", json!(uuid::Uuid::new_v4().to_string())),
            (
                "original_request_id",
                json!(uuid::Uuid::new_v4().to_string()),
            ),
            ("durable", json!(true)),
            if Operation::of(&request) == Operation::AutomaticApprovals {
                ("result", json!({"accepted":true,"durable":true}))
            } else {
                ("configured", json!(true))
            },
        ] {
            let mut invalid = absent.clone();
            invalid[field] = wrong;
            assert!(lookup_receipt(&request, &query, invalid).is_err());
        }
    }
}
#[test]
fn maintenance_config_commit_does_not_settle_an_unknown_recovery_save() {
    let request = Request::MaintenanceConfigure {
        request_id: uuid::Uuid::new_v4().to_string(),
        update: None,
        recovery: Some(Box::new(maintenance::RecoveryConfiguration {
            policy: Some(maintenance::RecoveryForm {
                enabled: false,
                rules: BTreeMap::new(),
                session_limit_mode: "reset".into(),
                delays: BTreeMap::new(),
            }),
            base: serde_json::Map::new(),
        })),
    };
    let result = json!({"request_id":request.id(),"original_request_id":request.id(),"accepted":true,"durable":true,
        "configured":true,"revision":1,"recovery_save":"unknown"});
    let (result, complete) = checked(&request, result, false).unwrap();
    assert!(!complete);
    assert_eq!(result["pending"], true);
    assert_eq!(result["recovery_edit"], true);
    assert_eq!(
        unknown(&request, "frontend_request_unconfirmed")["recovery_edit"],
        true
    );
    assert_eq!(
        refused(&request, "frontend_request_not_applied")["recovery_edit"],
        true
    );
}
#[test]
fn the_actual_version_suffix_alone_defines_the_gui_dev_number() {
    assert_eq!(version_dev_number("1.0.0"), Some(0));
    assert_eq!(version_dev_number("1.0.0-preview.local.abcdef0"), Some(0));
    assert_eq!(version_dev_number("1.0.0-nightly.42"), Some(0));
    assert_eq!(version_dev_number("1.0.0-dev.42"), Some(42));
    assert_eq!(
        version_dev_number("1.0.0-preview.local-dev.1a2b3c4"),
        Some(0)
    );
    assert_eq!(
        version_dev_number("1.0.0-preview.local-dev.1234567"),
        Some(0)
    );
    for version in [
        "1.0.0-dev.",
        "1.0.0-dev.0",
        "1.0.0-dev.042",
        "1.0.0-dev.+42",
    ] {
        assert_eq!(version_dev_number(version), None);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod native {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::desktop_service::tests::Fixture;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
        path::Path,
        thread,
    };
    use zerus_client_core::desktop::{Build, Responder};
    pub(crate) struct CacheServer {
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl CacheServer {
        pub(crate) fn start(root: &Path, responder: Arc<Responder>) -> Self {
            let listener = UnixListener::bind(root.join("native-cache.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let running = stop.clone();
            let thread = thread::spawn(move || {
                while !running.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    #[cfg(target_os = "linux")]
                    {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        assert!(zerus_client_core::desktop::same_user(&stream));
                        let mut line = Vec::new();
                        let mut byte = [0];
                        while line.len() < 64 && stream.read_exact(&mut byte).is_ok() {
                            if byte[0] == b'\n' {
                                break;
                            }
                            line.push(byte[0]);
                        }
                        let reply = responder.reply(&line).unwrap_or_else(|| b"null\n".to_vec());
                        let _ = stream.write_all(&reply);
                    }
                    #[cfg(target_os = "macos")]
                    exchange_mac(&mut stream, &responder);
                }
            });
            Self {
                stop,
                thread: Some(thread),
            }
        }
    }
    // The real Mac runner refuses SO_RCVTIMEO on this fixture socket.
    // Poll bounded nonblocking I/O instead; the cache still serves only the
    // original child's registered request, with the same peer check.
    #[cfg(target_os = "macos")]
    fn exchange_mac(stream: &mut std::os::unix::net::UnixStream, responder: &Responder) {
        use std::io::ErrorKind;
        stream.set_nonblocking(true).unwrap();
        assert!(zerus_client_core::desktop::same_user(stream));
        let until = std::time::Instant::now() + Duration::from_secs(1);
        let mut line = Vec::new();
        let mut byte = [0];
        loop {
            if std::time::Instant::now() >= until || line.len() >= 64 {
                return;
            }
            match stream.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => break,
                Ok(1) => line.push(byte[0]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                _ => return,
            }
        }
        let reply = responder.reply(&line).unwrap_or_else(|| b"null\n".to_vec());
        let mut remaining = reply.as_slice();
        while !remaining.is_empty() && std::time::Instant::now() < until {
            match stream.write(remaining) {
                Ok(0) => break,
                Ok(n) => remaining = &remaining[n..],
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                _ => break,
            }
        }
    }
    impl Drop for CacheServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    #[cfg(target_os = "linux")]
    async fn eventually(label: &str, check: impl Fn() -> bool) {
        let until = tokio::time::Instant::now() + Duration::from_secs(10);
        while !check() {
            assert!(tokio::time::Instant::now() < until, "{label}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    pub(crate) fn responder() -> Arc<Responder> {
        responder_for_configuration("a".repeat(64))
    }
    fn responder_for_configuration(configuration: String) -> Arc<Responder> {
        let build = crate::cli::build_info(&crate::cli::BUILD);
        let responder = Arc::new(Responder::new(
            "com.hgdev.hgs-tray".into(),
            Build {
                version: build["versionName"].as_str().unwrap().into(),
                source_commit: build["sourceCommit"].as_str().unwrap().into(),
                source_dirty: Some(false),
            },
            uuid::Uuid::new_v4().to_string(),
            true,
            false,
        ));
        responder.configure_once(configuration);
        responder
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn degraded_root_status_still_uses_the_original_child_disable_route() {
        let _process = crate::PROCESS_TREE.read().unwrap();
        tauri::async_runtime::block_on(async {
            let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../fixtures/desktop-delivery-status.json"
            )))
            .unwrap();
            let status = &cases
                .iter()
                .find(|case| case["case"] == "approval_degraded")
                .unwrap()["status"];
            let fixture = Fixture::new(json!({"produced_status":status}));
            std::fs::write(
                fixture.0.join("runtime-epoch"),
                status["epoch"].as_str().unwrap(),
            )
            .unwrap();
            let responder =
                responder_for_configuration(status["configuration"].as_str().unwrap().into());
            let _cache = CacheServer::start(&fixture.0, responder.clone());
            let (service, _) = fixture.start_with_responder(true, Some(responder));
            eventually("actual root degraded policy is observed", || {
                service.snapshot().connected && service.snapshot().automatic_approvals == Some(true)
            })
            .await;
            assert!(!service.snapshot().component_ready("approvals"));
            let enable = service
                .automatic_approvals(uuid::Uuid::new_v4().to_string(), true)
                .await
                .unwrap();
            let disable = service
                .automatic_approvals(uuid::Uuid::new_v4().to_string(), false)
                .await
                .unwrap();
            service.join().await.unwrap();
            assert_eq!(enable["code"], "desktop_background_unavailable");
            assert_eq!(disable["accepted"], true, "{disable}");
            assert_eq!(disable["durable"], true);
            assert_eq!(disable["enabled"], false);
            assert!(fixture.0.join("original-child-bound").exists());
            let calls = fixture.calls();
            let writes: Vec<_> = calls
                .iter()
                .filter(|call| call["args"] == json!(["desktop", "frontend-request", "--json"]))
                .collect();
            assert_eq!(
                writes.len(),
                1,
                "no enable or replay reaches the native relay"
            );
            assert_eq!(writes[0]["value"]["request"]["enabled"], false);
            assert!(!calls
                .iter()
                .any(|call| call["value"]["op"] == "automatic_approvals"));
        });
    }
    // Exercise the production startup producer through its original child and
    // native cache registration. The private fake service supplies receipts;
    // the service/ledger tests separately verify their real SQL commits.
    #[cfg(target_os = "linux")]
    async fn native_startup_import(lost_replies: bool, superseded: bool) {
        let fixture = Fixture::new(
            json!({"ready":false,"frontend_uncertain":lost_replies,"pending_reads_superseded":superseded}),
        );
        let responder = responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let (service, _) = fixture.start_with_responder(true, Some(responder));
        let path = fixture.0.join("managed-read-marks.json");
        let actions = crate::actions::Actions::new(path.clone());
        let marks: Vec<_> = (0..193)
            .map(|n| zerus_client_core::readmarks::ReadMark {
                computer: "example".into(),
                name: format!("agent/project/{n}"),
                run_id: format!("run-{n}"),
                conversation_id: format!("conversation-{n}"),
                reply_id: Some(format!("reply-{n}")),
                attention_signature: None,
            })
            .collect();
        actions.queue_marks(marks).unwrap();
        actions.disarm_read_delivery().unwrap();
        service.start_pending_read_import(actions.clone());
        eventually("initial import stops or completes", || {
            service
                .snapshot()
                .pending_read_import
                .as_ref()
                .is_some_and(|v| {
                    v["complete"] == true
                        || (v["running"] == false && v["code"] == "pending_read_import_unconfirmed")
                })
        })
        .await;
        let original_bytes = std::fs::read(&path).unwrap();
        if lost_replies {
            for completed_pages in 0..4 {
                assert_eq!(
                    fixture
                        .calls()
                        .iter()
                        .filter(|c| c["args"] == json!(["desktop", "frontend-request", "--json"]))
                        .count(),
                    completed_pages + 1
                );
                assert!(
                    service.read_marks(vec![]).await.is_err(),
                    "live reads wait for the transfer"
                );
                service.retry(); // Explicit retry looks up the original page.
                eventually("receipt lookup then next original page", || {
                    service
                        .snapshot()
                        .pending_read_import
                        .as_ref()
                        .is_some_and(|v| {
                            if completed_pages == 3 {
                                v["complete"] == true
                            } else {
                                v["through"] == ((completed_pages + 1) * 64)
                                    && v["running"] == false
                                    && v["code"] == "pending_read_import_unconfirmed"
                            }
                        })
                })
                .await;
            }
        }
        let status = service.snapshot();
        let import = status.pending_read_import.as_ref().unwrap();
        assert_eq!(import["complete"], true);
        assert_eq!(import["through"], 193);
        assert_eq!(import["superseded"], if superseded { 193 } else { 0 });
        assert!(
            service.read_marks(vec![]).await.is_ok(),
            "completed conflicts release the read gate"
        );
        assert_eq!(import["delivered"], false);
        assert!(
            !status.component_ready("reads"),
            "passive import needs no background readiness"
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            original_bytes,
            "frozen source is retained"
        );
        assert!(actions.queue_marks(vec![]).is_err());
        let calls = fixture.calls();
        let pages: Vec<_> = calls
            .iter()
            .filter(|c| c["args"] == json!(["desktop", "frontend-request", "--json"]))
            .map(|c| c["value"]["request"]["page"].clone())
            .collect();
        assert_eq!(
            pages.len(),
            4,
            "one original send per page, even with lost replies"
        );
        for (i, page) in pages.iter().enumerate() {
            assert_eq!(page["after"], i * 64);
            assert_eq!(page["source"]["rows"], 193);
        }
        assert!(
            !calls.iter().any(|c| c["value"]["op"] == "read_intent"),
            "no second read worker"
        );
        service.join().await.unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn original_child_registration_receipts_and_unknown_delivery_use_only_the_dedicated_route() {
        const CHILD: &str = "ZERUS_FRONTEND_SENDER_FIXTURE";
        const TEST:&str="desktop_service::frontend::tests::native::original_child_registration_receipts_and_unknown_delivery_use_only_the_dedicated_route";
        if std::env::var_os(CHILD).is_none() {
            let _process = crate::PROCESS_TREE.read().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, "1")
                .env_remove("HGS_SESSION")
                .env_remove("HGS_RUN_ID")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        tauri::async_runtime::block_on(async {
            native_startup_import(false, false).await;
            native_startup_import(true, false).await;
            native_startup_import(true, true).await;
            for case in [
                "success",
                "unknown",
                "missing-abi",
                "maintenance",
                "maintenance-unknown",
                "retained-pipe",
                "not-applied",
                "maintenance-not-applied",
            ] {
                let is_maintenance = case.starts_with("maintenance");
                let not_applied = case.ends_with("not-applied");
                let config = json!({"ready":true,"frontend_uncertain":matches!(case,"unknown"|"maintenance-unknown"),"frontend_abi":case!="missing-abi","frontend_retain":case=="retained-pipe", "frontend_unreachable":not_applied,"lookup_unavailable":not_applied});
                let fixture = Fixture::new(config);
                let responder = responder();
                let _cache = CacheServer::start(&fixture.0, responder.clone());
                let (service, _) = fixture.start_with_responder(true, Some(responder.clone()));
                eventually("service lifecycle ready", || {
                    service.snapshot().component_ready("approvals")
                })
                .await;
                let id = uuid::Uuid::new_v4().to_string();
                let receipt = if is_maintenance {
                    let config = maintenance::Configuration {
                        update: Some(maintenance::UpdateConfiguration {
                            gui: maintenance::GuiIdentity {
                                bundle_id: "untrusted-js".into(),
                                executable: "untrusted-js".into(),
                                app_version: "wrong".into(),
                                dev_number: 999,
                                source_commit: String::new(),
                            },
                            channel: zerus_client_core::updates::Channel::Stable,
                            automatic: true,
                        }),
                        recovery: None,
                    };
                    service
                        .maintenance_configure(id.clone(), config, "com.hgdev.hgs-tray".into())
                        .await
                        .unwrap()
                } else {
                    service.automatic_approvals(id.clone(), true).await.unwrap()
                };
                if not_applied {
                    let operation = if is_maintenance {
                        Operation::MaintenanceConfigure
                    } else {
                        Operation::AutomaticApprovals
                    };
                    assert_eq!(
                        receipt["pending"], true,
                        "a spawned failed relay is initially uncertain"
                    );
                    let unavailable = service.frontend_result(operation).await;
                    assert_eq!(
                        unavailable["pending"], true,
                        "a failed lookup proves no absence"
                    );
                    std::fs::write(fixture.0.join("lookup-back"), []).unwrap();
                    let absent = service.frontend_result(operation).await;
                    assert_eq!(absent["request_id"], id);
                    assert_eq!(absent["accepted"], false);
                    assert_eq!(absent["durable"], false);
                    assert_eq!(
                        absent["pending"], false,
                        "confirmed relay exit and no receipt settles refusal"
                    );
                    assert_eq!(absent["code"], "frontend_request_not_applied");
                    // A retry of the same logical ID returns that refusal, never another send.
                    let cached = if is_maintenance {
                        service.frontend_result(operation).await
                    } else {
                        service.automatic_approvals(id.clone(), true).await.unwrap()
                    };
                    assert_eq!(cached, absent);
                    std::fs::write(fixture.0.join("service-back"), []).unwrap();
                    let next_id = uuid::Uuid::new_v4().to_string();
                    let opposite = if is_maintenance {
                        service
                            .maintenance_configure(
                                next_id.clone(),
                                maintenance::Configuration {
                                    update: None,
                                    recovery: None,
                                },
                                "com.hgdev.hgs-tray".into(),
                            )
                            .await
                            .unwrap()
                    } else {
                        service
                            .automatic_approvals(next_id.clone(), false)
                            .await
                            .unwrap()
                    };
                    assert_eq!(opposite["request_id"], next_id);
                    assert_eq!(
                        opposite["durable"], true,
                        "a fresh intent is admitted after definitive refusal"
                    );
                    assert_eq!(opposite["pending"], false);
                    if !is_maintenance {
                        assert_eq!(opposite["enabled"], false);
                    }
                    let sends: Vec<_> = fixture
                        .calls()
                        .into_iter()
                        .filter(|call| {
                            call["args"] == json!(["desktop", "frontend-request", "--json"])
                        })
                        .collect();
                    assert_eq!(sends.len(), 2);
                    assert_eq!(
                        sends
                            .iter()
                            .filter(|c| c["value"]["request"]["request_id"] == id)
                            .count(),
                        1
                    );
                } else if case == "missing-abi" {
                    assert_eq!(receipt["accepted"], false);
                    assert_eq!(receipt["code"], "incompatible_frontend_cli");
                    assert!(!fixture.calls().iter().any(
                        |call| call["args"] == json!(["desktop", "frontend-request", "--json"])
                    ));
                } else if matches!(case, "unknown" | "maintenance-unknown" | "retained-pipe") {
                    assert_eq!(receipt["request_id"], id);
                    assert_eq!(receipt["accepted"], Value::Null);
                    assert_eq!(receipt["pending"], true);
                    if case == "unknown" {
                        // A new service epoch cannot clear native uncertainty
                        // or turn the original logical UUID into a fresh send.
                        std::fs::write(
                            fixture.0.join("runtime-epoch"),
                            uuid::Uuid::new_v4().to_string(),
                        )
                        .unwrap();
                        service.retry();
                        eventually("new service epoch", || {
                            service.snapshot().component_ready("approvals")
                                && fixture
                                    .calls()
                                    .iter()
                                    .filter(|c| c["args"] == json!(["desktop", "watch", "--json"]))
                                    .count()
                                    == 2
                        })
                        .await;
                    } else if case == "retained-pipe" {
                        assert!(
                            service.shared.rpc.snapshot().unwrap().admitted > 0,
                            "held pipe retains original registration resources"
                        );
                        let call = fixture
                            .calls()
                            .into_iter()
                            .find(|c| c["args"] == json!(["desktop", "frontend-request", "--json"]))
                            .unwrap();
                        let mut query =
                            ui_requests::query(call["value"]["attempt"].as_str().unwrap()).unwrap();
                        query.pop();
                        assert!(
                            responder.reply(&query).is_some(),
                            "the original registration stays retained with the inherited pipe"
                        );
                    }
                    let blocked = if is_maintenance {
                        service
                            .maintenance_configure(
                                uuid::Uuid::new_v4().to_string(),
                                maintenance::Configuration {
                                    update: None,
                                    recovery: None,
                                },
                                "com.hgdev.hgs-tray".into(),
                            )
                            .await
                            .unwrap()
                    } else {
                        service
                            .automatic_approvals(uuid::Uuid::new_v4().to_string(), false)
                            .await
                            .unwrap()
                    };
                    assert_eq!(blocked["code"], "previous_frontend_request_unconfirmed");
                    // Same UUID becomes a read-only lookup, never another send.
                    let result = if is_maintenance {
                        service
                            .frontend_result(Operation::MaintenanceConfigure)
                            .await
                    } else {
                        service.automatic_approvals(id.clone(), true).await.unwrap()
                    };
                    assert_eq!(result["request_id"], id);
                    assert_eq!(result["durable"], true);
                    assert_eq!(result["pending"], false);
                    assert_eq!(
                        fixture
                            .calls()
                            .iter()
                            .filter(
                                |c| c["args"] == json!(["desktop", "frontend-request", "--json"])
                            )
                            .count(),
                        1
                    );
                    assert!(fixture.calls().iter().any(|c| c["value"]["op"]
                        == if is_maintenance {
                            "maintenance_result"
                        } else {
                            "delivery_result"
                        }
                        && c["value"]["original_request_id"] == id));
                    if case == "retained-pipe" {
                        std::fs::write(fixture.0.join("release-retainer"), []).unwrap();
                        eventually("original pipes actually drained", || {
                            service.shared.rpc.snapshot().unwrap().admitted == 0
                        })
                        .await;
                    }
                } else {
                    assert_eq!(receipt["request_id"], id);
                    assert_eq!(receipt["accepted"], true);
                    assert_eq!(receipt["durable"], true);
                    assert_eq!(receipt["pending"], false);
                    let call = fixture
                        .calls()
                        .into_iter()
                        .find(|c| c["args"] == json!(["desktop", "frontend-request", "--json"]))
                        .unwrap();
                    assert_eq!(call["value"].as_object().unwrap().len(), 3);
                    assert_eq!(call["value"]["abi"], ui_requests::ABI);
                    assert!(fixture.0.join("original-child-bound").exists());
                    if is_maintenance {
                        let expected = native_gui("com.hgdev.hgs-tray").unwrap();
                        assert_eq!(
                            call["value"]["request"]["update"]["gui"],
                            serde_json::to_value(expected).unwrap()
                        );
                        assert!(call["value"]["request"]
                            .as_object()
                            .unwrap()
                            .contains_key("recovery"));
                        assert!(call["value"]["request"]["recovery"].is_null());
                    }
                    let attempt = call["value"]["attempt"].as_str().unwrap();
                    let mut query = ui_requests::query(attempt).unwrap();
                    query.pop();
                    eventually(
                        "registration released only after owned exchange drains",
                        || responder.reply(&query).is_none(),
                    )
                    .await;
                }
                assert!(request(
                    &service.shared,
                    json!({"op":"automatic_approvals","request_id":id,"enabled":true}),
                    Arc::new(AtomicBool::new(false))
                )
                .await
                .is_err());
                assert!(!fixture
                    .calls()
                    .iter()
                    .any(|c| c["value"]["op"] == "automatic_approvals"));
                service.join().await.unwrap();
            }
        });
    }
}

#[test]
fn an_incomplete_or_mismatched_receipt_never_releases_the_original_request() {
    let request = approval(true);
    let receipt = json!({"request_id":request.id(),"op":"automatic_approvals","enabled":true,"accepted":true,"durable":true});
    for (field, wrong) in [
        ("request_id", json!(uuid::Uuid::new_v4().to_string())),
        ("op", json!("read_intent")),
        ("enabled", json!(false)),
        ("durable", json!(false)),
    ] {
        let mut value = receipt.clone();
        value[field] = wrong;
        assert!(checked(&request, value, false).is_none());
    }
    // A failed query is absence of evidence, never a fresh refusal/admission.
    assert!(checked(
        &request,
        json!({"request_id":request.id(),"accepted":false,"durable":false}),
        true
    )
    .is_none());
}

#[test]
fn non_durable_store_failures_and_unknown_maintenance_states_never_allow_replay() {
    let request = approval(true);
    for code in [
        "delivery_store_refused",
        "maintenance_refused",
        "unrecognized",
    ] {
        assert!(checked(
            &request,
            json!({"request_id":request.id(),"accepted":false,"durable":false,"code":code}),
            false
        )
        .is_none());
    }
    let request = Request::maintenance_configure(
        uuid::Uuid::new_v4().to_string(),
        maintenance::Configuration {
            update: None,
            recovery: None,
        },
    );
    assert!(checked(
        &request,
        json!({"request_id":request.id(),"original_request_id":request.id(),"configured":true,
        "revision":1,"accepted":true,"durable":true,"recovery_save":"new_unknown_state"}),
        false
    )
    .is_none());
}

#[test]
fn cache_is_native_only_and_a_recorded_preparation_is_not_effect_admission() {
    use zerus_client_core::desktop::cache_maintenance::{self as wire, Command, Timing};
    assert!(serde_json::from_value::<Operation>(json!("cache_maintenance")).is_err());
    let prepare = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    let request = Request::CacheMaintenance {
        request_id: prepare.clone(),
        command: Box::new(Command::Prepare {
            operation_id: operation.clone(),
            configuration: "a".repeat(64),
            root: "/srv/example/cache".into(),
            limit: 1,
            revision: 0,
            timing: Timing::Immediate,
            policy_change: true,
        }),
    };
    let value = json!({"abi":wire::ABI,"request_id":prepare,"op":"cache_maintenance","accepted":false,"durable":true,
        "pending":true,"configuration":"a".repeat(64),"preparation_id":prepare,"operation_id":operation,
        "revision":1,"limit":1,"policy_saved":true,"phase":"prepared","binding_sha256":"b".repeat(64),"removed":0,"kept":0,"unknown":0,"drained":false});
    // The native PACKET is complete. The admitted service effect is still none.
    assert_eq!(
        checked(&request, value.clone(), false),
        Some((value.clone(), true))
    );
    let state = Frontend::default();
    let Begin::Work(reservation) = state.begin(Operation::CacheMaintenance, Some(request.clone()))
    else {
        panic!("prepare");
    };
    reservation.finish(value, true);
    let execute = Request::CacheMaintenance {
        request_id: operation,
        command: Box::new(Command::Execute {
            configuration: "a".repeat(64),
            preparation_id: prepare,
            binding_sha256: "b".repeat(64),
        }),
    };
    let Begin::Work(reservation) = state.begin(Operation::CacheMaintenance, Some(execute.clone()))
    else {
        panic!("fresh execute");
    };
    assert!(!reservation.lookup);
    reservation.finish(unknown(&execute, "lost_original_reply"), false);
    let Begin::Work(lookup) = state.begin(Operation::CacheMaintenance, Some(execute)) else {
        panic!("lookup");
    };
    assert!(lookup.lookup); // original child/request is NEVER re-sent
}
