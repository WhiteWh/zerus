use super::*;
use crate::desktop_service::frontend::tests::native::{responder, CacheServer};
use std::{fs, path::PathBuf};
use zerus_client_core::desktop::ownership::Mode;

fn private_case(case: &'static str) -> bool {
    const MARKER: &str = "ZERUS_NOTIFICATION_CONTROLS_CASE";
    if std::env::var(MARKER).as_deref() == Ok(case) {
        return true;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    let invocation = Invocation {
        args: ["--exact", case, "--nocapture", "--test-threads=1"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        stdin: None,
        timeout: Duration::from_secs(40),
        request_id: None,
    };
    let exit = zerus_client_core::run::run_without(
        &std::env::current_exe().unwrap(),
        &invocation,
        &[(MARKER, case)],
        &["HGS_SESSION", "HGS_RUN_ID"],
        64 << 10,
    )
    .unwrap();
    match exit {
        zerus_client_core::Exit::Code {
            code,
            stdout,
            stderr,
        } => assert_eq!(code, 0, "{}\n{stderr}", String::from_utf8_lossy(&stdout)),
        other => panic!("{other:?}"),
    }
    false
}
struct Fixture(PathBuf);
impl Fixture {
    fn new(config: Value) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::desktop_service::tests::private_root(&std::env::temp_dir());
        fs::write(root.join("hgs"), include_str!("fixture.py")).unwrap();
        fs::set_permissions(root.join("hgs"), fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(root);
        fixture.configure(config);
        fixture
    }
    fn configure(&self, config: Value) {
        fs::write(self.0.join("fixture.json"), config.to_string()).unwrap();
    }
    fn lines(&self, name: &str) -> Vec<Value> {
        fs::read_to_string(self.0.join(name))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
    fn controls(&self, kind: &str) -> Vec<Value> {
        self.lines("calls")
            .into_iter()
            .filter_map(|v| {
                let r = &v["value"]["request"];
                (r["control"]["kind"] == kind).then(|| r["control"].clone())
            })
            .collect()
    }
    fn setup(&self) -> Setup {
        Setup {
            source: Source::default(),
            settings: Settings::open(self.0.join("settings.json"), None),
        }
    }
    async fn connect(&self, owner: &Arc<Controller>, responder: Arc<Responder>) -> Arc<Session> {
        owner
            .connect(
                Scope {
                    configuration: "a".repeat(64),
                    service_epoch: uuid::Uuid::new_v4().to_string(),
                },
                Arc::new(relay::Connection::select(&self.0.join("hgs")).unwrap()),
                Some(responder),
                Arc::new(AtomicBool::new(false)),
                true,
            )
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
async fn until(label: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "{label}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn service_responder() -> Arc<Responder> {
    let r = responder();
    assert!(r.configure_ownership_mode_once(Mode::Service));
    r
}
async fn stop(
    owner: &Arc<Controller>,
    session: &Arc<Session>,
    worker: tauri::async_runtime::JoinHandle<()>,
) {
    session.stop.store(true, Ordering::Release);
    let closed = owner.close();
    worker.await.unwrap();
    assert_eq!(
        closed.wait_until(Instant::now() + Duration::from_secs(2)),
        zerus_client_core::desktop::drain::Observation::Idle
    );
}

type HeldRefresh = Arc<Mutex<Option<(Source, tokio::sync::watch::Sender<bool>)>>>;
fn hold_renewal_refreshes(source: &Source) -> (Arc<AtomicBool>, HeldRefresh) {
    let blocked = Arc::new(AtomicBool::new(false));
    let pending: HeldRefresh = Arc::default();
    let (block, held) = (blocked.clone(), pending.clone());
    source.set_refresh(Arc::new(move |source, complete| {
        if block.load(Ordering::Acquire) {
            assert!(held.lock().unwrap().replace((source, complete)).is_none());
        } else {
            source.observe(FocusState::Foreground);
            complete.send(true).unwrap();
        }
    }));
    (blocked, pending)
}

#[test]
fn foreground_renewal_waits_for_a_fresh_hidden_window_observation() {
    if !private_case("desktop_service::notification_controls::tests::foreground_renewal_waits_for_a_fresh_hidden_window_observation") { return; }
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let setup = fixture.setup();
        let (blocked, pending) = hold_renewal_refreshes(&setup.source);
        let owner = Controller::new(Some(setup));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("original Foreground observation delivered", || {
            fixture
                .lines("evidence")
                .iter()
                .any(|v| v["focus"]["state"] == "foreground")
        })
        .await;
        let original = session.native.snapshot().unwrap();
        blocked.store(true, Ordering::Release);
        until("renewal queued a fresh native enumeration", || {
            pending.lock().unwrap().is_some()
        })
        .await;
        assert_eq!(
            fixture.controls("focus").len(),
            1,
            "cached Foreground was not renewed before enumeration"
        );
        // The platform hid the window without a Focused(false) event. Only the
        // fresh native enumeration discovers it; scheduling alone cannot clear it.
        assert_eq!(session.native.snapshot().unwrap(), original);
        let (source, complete) = pending.lock().unwrap().take().unwrap();
        source.observe(crate::desktop_notification_controls::aggregate([Some(
            false,
        )]));
        complete.send(true).unwrap();
        until(
            "fresh hidden window sends Background instead of another Foreground",
            || {
                fixture
                    .lines("evidence")
                    .iter()
                    .any(|v| v["focus"]["state"] == "background")
            },
        )
        .await;
        assert_eq!(
            session.native.snapshot().unwrap().state,
            FocusState::Background
        );
        assert!(session.native.snapshot().unwrap().revision > original.revision);
        assert_eq!(
            fixture
                .lines("evidence")
                .iter()
                .filter(|v| v["focus"]["state"] == "foreground")
                .count(),
            1
        );
        stop(&owner, &session, worker).await;
    });
}

#[test]
fn configure_status_and_lookup_do_not_wait_for_a_blocked_foreground_check() {
    if !private_case("desktop_service::notification_controls::tests::configure_status_and_lookup_do_not_wait_for_a_blocked_foreground_check") {return}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"lose_configure":true}));
        let setup = fixture.setup();
        let (blocked, pending) = hold_renewal_refreshes(&setup.source);
        let owner = Controller::new(Some(setup));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("initial status and Foreground", || {
            owner.view().is_some_and(|v| v.status.is_some())
                && !fixture.controls("focus").is_empty()
        })
        .await;
        blocked.store(true, Ordering::Release);
        until("renewal waiting for the main thread", || {
            pending.lock().unwrap().is_some()
        })
        .await;
        let statuses = || {
            fixture
                .lines("calls")
                .iter()
                .filter(|v| v["value"]["op"] == "notification_preferences_status")
                .count()
        };
        let initial = statuses();
        let original = owner.edit(true).unwrap().request_id.unwrap();
        until("Configure runs while the callback stays blocked", || {
            fixture.controls("configure").len() == 1
                && owner.view().is_some_and(|v| v.pending && !v.running)
        })
        .await;
        assert!(pending.lock().unwrap().is_some());
        owner.check().unwrap();
        until("lookup and status run without waiting for focus", || {
            statuses() > initial
                && fixture
                    .lines("calls")
                    .iter()
                    .any(|v| v["value"]["op"] == "notification_preferences_result")
        })
        .await;
        assert_eq!(fixture.controls("configure")[0]["request_id"], original);
        assert_eq!(
            fixture.controls("configure").len(),
            1,
            "unknown edits remain lookup-only"
        );
        assert_eq!(
            fixture.controls("focus").len(),
            1,
            "no cached Foreground renewal"
        );
        assert!(pending.lock().unwrap().is_some());
        stop(&owner, &session, worker).await;
    });
}

#[test]
fn native_loss_and_disconnect_do_not_wait_for_a_queued_renewal_enumeration() {
    if !private_case("desktop_service::notification_controls::tests::native_loss_and_disconnect_do_not_wait_for_a_queued_renewal_enumeration") { return; }
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let setup = fixture.setup();
        let (blocked, pending) = hold_renewal_refreshes(&setup.source);
        let owner = Controller::new(Some(setup.clone()));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("original Foreground delivered", || {
            fixture
                .lines("evidence")
                .iter()
                .any(|v| v["focus"]["state"] == "foreground")
        })
        .await;
        blocked.store(true, Ordering::Release);
        until("renewal waits on main-thread work", || {
            pending.lock().unwrap().is_some()
        })
        .await;
        setup.source.observe(FocusState::Background);
        until("native loss bypasses the blocked enumeration", || {
            fixture
                .lines("evidence")
                .iter()
                .any(|v| v["focus"]["state"] == "background")
        })
        .await;
        assert!(pending.lock().unwrap().is_some());
        setup.source.observe(FocusState::Foreground);
        // The same pending enumeration is reused. Disconnect must join the
        // worker without needing the main thread to execute that callback.
        tokio::task::yield_now().await;
        stop(&owner, &session, worker).await;
        let (source, complete) = pending.lock().unwrap().take().unwrap();
        source.observe(FocusState::Foreground);
        complete.send(true).unwrap();
        assert!(session.native.snapshot().is_none());
        assert_eq!(
            fixture
                .lines("evidence")
                .iter()
                .filter(|v| v["focus"]["state"] == "foreground")
                .count(),
            1
        );
    });
}

#[test]
fn current_preferences_win_and_unknown_edits_are_lookup_only_until_confirmed_negative() {
    if !private_case("desktop_service::notification_controls::tests::current_preferences_win_and_unknown_edits_are_lookup_only_until_confirmed_negative"){return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"lose_configure":true}));
        let setup = fixture.setup();
        setup.settings.set(KEY, json!(true)).unwrap();
        let owner = Controller::new(Some(setup.clone()));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder.clone()).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until(
            "fresh configured status overrides stale local preference",
            || owner.view().is_some_and(|v| v.available) && !setup.settings.flag(KEY),
        )
        .await;
        assert!(fixture.controls("configure").is_empty());
        let original = owner.edit(true).unwrap().request_id.unwrap();
        until("original spawned and exited", || {
            owner
                .state()
                .pending
                .as_ref()
                .is_some_and(|p| p.exit_confirmed)
        })
        .await;
        owner.check().unwrap();
        until("read-only original lookup", || {
            fixture
                .lines("calls")
                .iter()
                .any(|c| c["value"]["op"] == "notification_preferences_result")
        })
        .await;
        assert!(owner.view().unwrap().pending);
        assert!(owner.edit(false).is_err());
        assert_eq!(fixture.controls("configure").len(), 1);
        // Reconnect keeps the original edit and asks only for its receipt.
        session.stop.store(true, Ordering::Release);
        owner.disconnect();
        worker.await.unwrap();
        fixture.configure(json!({"lose_configure":true,"definitive":true}));
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("confirmed-exit negative settles the original", || {
            !owner.view().unwrap().pending
        })
        .await;
        assert_eq!(fixture.controls("configure").len(), 1);
        fixture.configure(json!({}));
        until("fresh status after negative", || {
            owner.view().unwrap().available && !owner.view().unwrap().running
        })
        .await;
        let opposite = owner.edit(false).unwrap().request_id.unwrap();
        assert_ne!(original, opposite);
        until("new intent saved", || {
            !owner.view().unwrap().pending && fixture.controls("configure").len() == 2
        })
        .await;
        stop(&owner, &session, worker).await;
        assert_eq!(fixture.controls("configure")[0]["request_id"], original);
    });
}

#[test]
fn focus_loss_invalidates_the_slot_without_releasing_the_original_pipe_charge() {
    if !private_case("desktop_service::notification_controls::tests::focus_loss_invalidates_the_slot_without_releasing_the_original_pipe_charge"){return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"hold_focus":true}));
        let setup = fixture.setup();
        let owner = Controller::new(Some(setup.clone()));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        setup.source.observe(FocusState::Foreground);
        let (selected, control) = (session.clone(), owner.clone());
        let charge = owner.gate.admit(Role::Fleet).unwrap();
        let request = Request::NotificationControl {
            scope: session.scope.clone(),
            control: Command::Focus {
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        };
        let original = tauri::async_runtime::spawn_blocking(move || {
            call(&control, &selected, charge, Some(request), Value::Null)
        });
        until("original foreground child holds pipes", || {
            fixture.0.join("focus-held").exists()
        })
        .await;
        let previous = session.native.snapshot().unwrap();
        setup.source.observe(FocusState::Background);
        let current = session.native.snapshot().unwrap();
        assert!(current.revision > previous.revision);
        assert_eq!(current.state, FocusState::Background);
        let (result, attached) = original.await.unwrap();
        assert!(result.bytes.is_err());
        assert_eq!(attached.unwrap(), previous);
        assert!(
            owner.gate.admit(Role::Fleet).is_err(),
            "bounded return did not release inherited pipes"
        );
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        assert_eq!(owner.gate.snapshot().unwrap().admitted, 1);
        assert_eq!(
            fixture.controls("focus").len(),
            1,
            "no second helper before real EOF"
        );
        fs::write(fixture.0.join("release-focus"), b"").unwrap();
        until(
            "background bypasses foreground renewal delay after custody drains",
            || {
                fixture
                    .lines("evidence")
                    .iter()
                    .any(|v| v["focus"]["state"] == "background")
            },
        )
        .await;
        let calls = fixture.controls("focus");
        assert_ne!(calls[0]["request_id"], calls[1]["request_id"]);
        let old_id = session.native.id().to_owned();
        stop(&owner, &session, worker).await;
        assert!(session.native.snapshot().is_none());
        // New original connection never replays the previous foreground state.
        let responder = service_responder();
        let owner = Controller::new(Some(setup));
        let session = fixture.connect(&owner, responder).await;
        assert_ne!(session.native.id(), old_id);
        assert_eq!(
            session.native.snapshot().unwrap().state,
            FocusState::Unknown
        );
        owner.disconnect();
    });
}

#[test]
fn unconfigured_initialization_is_once_and_historical_receipts_never_replace_current_status() {
    if !private_case("desktop_service::notification_controls::tests::unconfigured_initialization_is_once_and_historical_receipts_never_replace_current_status"){return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"unconfigured":true,"lose_configure":true}));
        let setup = fixture.setup();
        setup.settings.set(KEY, json!(true)).unwrap();
        let owner = Controller::new(Some(setup.clone()));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("first native preference is initialized once", || {
            owner
                .state()
                .pending
                .as_ref()
                .is_some_and(|p| p.exit_confirmed)
        })
        .await;
        let original = fixture.controls("configure").pop().unwrap();
        assert_eq!(original["preferences"]["optional_questions"], true);
        fixture.configure(json!({"unconfigured":true,"definitive":true}));
        owner.check().unwrap();
        until("initial journal absence is read", || {
            fixture
                .lines("calls")
                .iter()
                .any(|c| c["value"]["op"] == "notification_preferences_result")
        })
        .await;
        assert!(
            owner.view().unwrap().pending,
            "a missing initial journal is not a definitive negative"
        );
        // The exact historical saved receipt settles the edit, but the current
        // value may have been changed by a later authenticated frontend.
        let command = owner.state().pending.as_ref().unwrap().command.clone();
        let Command::Configure { preferences, .. } = command else {
            unreachable!()
        };
        let receipt = Receipt {
            abi: wire::ABI.into(),
            request_id: command.request_id().into(),
            scope: session.scope.clone(),
            fingerprint: command.fingerprint(&session.scope.configuration).unwrap(),
            journal: Some(uuid::Uuid::new_v4().to_string()),
            revision: 1,
            preferences,
            durable: true,
            outcome: wire::Outcome::Saved,
        };
        fs::write(
            fixture.0.join("saved"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        fixture.configure(json!({"current_newer":true}));
        owner.check().unwrap();
        until(
            "historical receipt followed by fresh current status",
            || {
                owner
                    .view()
                    .is_some_and(|v| !v.pending && v.status.is_some_and(|s| s.revision == 8))
                    && !setup.settings.flag(KEY)
            },
        )
        .await;
        assert_eq!(fixture.controls("configure").len(), 1);
        fixture.configure(json!({"invalid_status":true}));
        owner.check().unwrap();
        until("wrong epoch invalidates old positive settings", || {
            !owner.view().unwrap().available
        })
        .await;
        assert!(owner.edit(true).is_err());
        stop(&owner, &session, worker).await;
    });
}

#[test]
fn both_markers_and_the_original_helper_image_are_required() {
    if !private_case("desktop_service::notification_controls::tests::both_markers_and_the_original_helper_image_are_required"){return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"missing_abi":true}));
        let owner = Controller::new(Some(fixture.setup()));
        let responder = service_responder();
        let relay = Arc::new(relay::Connection::select(&fixture.0.join("hgs")).unwrap());
        let scope = Scope {
            configuration: "a".repeat(64),
            service_epoch: uuid::Uuid::new_v4().to_string(),
        };
        assert!(owner
            .connect(
                scope,
                relay,
                Some(responder.clone()),
                Arc::new(AtomicBool::new(false)),
                true
            )
            .await
            .is_none());
        assert_eq!(fixture.lines("calls").len(), 1);
        assert!(owner.edit(true).is_err());
        fixture.configure(json!({}));
        let session = fixture.connect(&owner, responder).await;
        let replacement = fixture.0.join("replacement");
        fs::copy(fixture.0.join("hgs"), &replacement).unwrap();
        fs::rename(replacement, fixture.0.join("hgs")).unwrap();
        let (result, _) = call(
            &owner,
            &session,
            owner.gate.admit(Role::Fleet).unwrap(),
            None,
            json!({"op":"notification_preferences_status","scope":session.scope}),
        );
        assert!(!result.spawned);
        assert!(result.bytes.is_err());
        assert_eq!(fixture.lines("calls").len(), 2);
        owner.disconnect();
    });
}

#[test]
fn refused_initialization_does_not_loop_or_silently_retry_the_setting() {
    if !private_case("desktop_service::notification_controls::tests::refused_initialization_does_not_loop_or_silently_retry_the_setting") {return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"unconfigured":true,"refuse_configure":true}));
        let owner = Controller::new(Some(fixture.setup()));
        let responder = service_responder();
        let _cache = CacheServer::start(&fixture.0, responder.clone());
        let session = fixture.connect(&owner, responder).await;
        let worker = tauri::async_runtime::spawn(run(owner.clone(), session.clone()));
        until("initialization refusal followed by current status", || {
            owner.view().is_some_and(|v| {
                v.available
                    && !v.pending
                    && !v.running
                    && v.code == "The previous change was not applied."
            })
        })
        .await;
        assert_eq!(fixture.controls("configure").len(), 1);
        let before = fixture.lines("calls").len();
        owner.check().unwrap();
        until("an explicit read completes", || {
            fixture.lines("calls").len() > before && !owner.view().unwrap().running
        })
        .await;
        assert_eq!(
            fixture.controls("configure").len(),
            1,
            "reading unconfigured state cannot reinitialize after refusal"
        );
        stop(&owner, &session, worker).await;
    });
}

#[test]
fn reconnect_waits_for_previous_custody_instead_of_losing_the_new_observer() {
    if !private_case("desktop_service::notification_controls::tests::reconnect_waits_for_previous_custody_instead_of_losing_the_new_observer") {return;}
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let owner = Controller::new(Some(fixture.setup()));
        let held = owner.gate.admit(Role::Fleet).unwrap();
        let mut connecting = Box::pin(fixture.connect(&owner, service_responder()));
        tokio::select! {
            biased;
            _ = &mut connecting => panic!("retained custody must block the new helper"),
            _ = tokio::task::yield_now() => (),
        }
        assert!(fixture.lines("calls").is_empty());
        assert!(!owner.view().unwrap().available);
        drop(held);
        let session = connecting.await;
        assert_eq!(
            session.native.snapshot().unwrap().state,
            FocusState::Unknown
        );
        assert_eq!(fixture.lines("calls").len(), 1);
        owner.disconnect();
        let held = owner.gate.admit(Role::Fleet).unwrap();
        let relay = Arc::new(relay::Connection::select(&fixture.0.join("hgs")).unwrap());
        let scope = Scope {
            service_epoch: uuid::Uuid::new_v4().to_string(),
            ..session.scope.clone()
        };
        let mut connecting = Box::pin(owner.connect(
            scope,
            relay,
            Some(service_responder()),
            Arc::new(AtomicBool::new(false)),
            true,
        ));
        tokio::select! {biased;_ = &mut connecting => panic!("still held"),_ = tokio::task::yield_now() => (),}
        owner.close();
        assert!(
            connecting.await.is_none(),
            "closed admissions never reopen on a later EOF"
        );
        drop(held);
        assert_eq!(fixture.lines("calls").len(), 1);
    });
}
