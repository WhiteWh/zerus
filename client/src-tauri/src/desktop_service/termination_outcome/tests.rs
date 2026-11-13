//! Real original child/cache/protocol with fake actor/effect responses. No real
//! Terminate, Root authentication, owner state or cleanup/removal qualification.
use super::*;
use crate::desktop_service::{
    frontend::tests::native::{responder, CacheServer},
    tests::{eventually, Fixture},
};
use zerus_client_core::{Action, Outcome, Target};

// These protocol/component fixtures explicitly bypass the production platform
// gate. They supply fake actor/effect replies and do NOT qualify Linux native
// relay authentication or enable the service submission.
fn inspect_fixture(
    service: Arc<Service>,
    target: &zerus_client_core::Target,
    machine: &str,
    read: Arc<dyn Send + Sync>,
) -> Result<(Option<Capture>, Selected), String> {
    Selected::inspect_context(Context::new_supported(service, read)?, target, machine)
}

type RefusedObserver = Box<dyn FnOnce(&mut std::process::Child)>;
thread_local! {
    static RENDEZVOUS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Observe only a REAL failed binding, before the SAME child is returned to
    // run_owned. A hook never substitutes a binding result or supplies consent.
    static AFTER_SPAWN_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static REFUSED_OBSERVER: std::cell::RefCell<Option<RefusedObserver>> = const { std::cell::RefCell::new(None) };
    static AFTER_OBSERVATION: std::cell::RefCell<Option<RefusedObserver>> = const { std::cell::RefCell::new(None) };
    static AFTER_BINDING: std::cell::RefCell<Option<RefusedObserver>> = const { std::cell::RefCell::new(None) };
    static AFTER_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_RPC_INPUT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn before_rpc_input() {
    if let Some(hook) = BEFORE_RPC_INPUT.with_borrow_mut(Option::take) {
        hook();
    }
}
pub(super) fn after_capture() {
    if let Some(hook) = AFTER_CAPTURE.with_borrow_mut(Option::take) {
        hook();
    }
}
pub(super) fn after_image_observation(child: &mut std::process::Child) {
    if let Some(observer) = AFTER_OBSERVATION.with_borrow_mut(Option::take) {
        observer(child);
    }
}
pub(super) fn after_original_binding(child: &mut std::process::Child) {
    if let Some(observer) = AFTER_BINDING.with_borrow_mut(Option::take) {
        observer(child);
    }
}
pub(super) fn after_spawn_check() {
    if let Some(observer) = AFTER_SPAWN_CHECK.with_borrow_mut(Option::take) {
        observer();
    }
}
pub(super) fn observe_refused_child(child: &mut std::process::Child) {
    if let Some(observer) = REFUSED_OBSERVER.with_borrow_mut(Option::take) {
        observer(child);
    }
}

#[test]
fn accounted_read_cannot_release_input_if_scope_changes_during_observation() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for change_configuration in [false, true] {
            let f = fixture(json!({"termination_refused_registration":true}));
            let (service, _cache) = connected(&f).await;
            let actions = Arc::new(actions(&f));
            let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
            let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
            let context = Context::new_supported(service.clone(), Arc::new(hold)).unwrap();
            context.trace.phase(Phase::InspectRead);
            let checked = context.clone();
            let check_actions = actions.clone();
            let check_key = key.clone();
            let root = f.0.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let result = tauri::async_runtime::spawn_blocking(move || {
                AFTER_OBSERVATION.with_borrow_mut(|hook| {
                    *hook = Some(Box::new(move |_| {
                        if change_configuration {
                            *checked.service.shared.configuration.lock().unwrap() =
                                Some("b".repeat(64));
                        } else {
                            checked.cancel.store(true, Ordering::Release);
                        }
                    }))
                });
                REFUSED_OBSERVER.with_borrow_mut(|hook| {
                    *hook = Some(Box::new(move |child| {
                        let path = root.join("termination-refused-input");
                        let until = std::time::Instant::now() + Duration::from_secs(2);
                        while !path.exists() && std::time::Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        let _ = tx.send((
                            child.id(),
                            std::fs::read(path).is_ok_and(|raw| raw.is_empty()),
                            child.try_wait().is_ok_and(|s| s.is_none()),
                            child.stdout.is_some() && child.stderr.is_some(),
                            check_actions.begin(check_key, &Action::Terminate).is_none(),
                        ));
                    }))
                });
                let exit = context.read(&Invocation {
                    args: vec!["desktop".into(), "frontend-request".into(), "--json".into()],
                    stdin: Some(b"private-fixture-frame\n".to_vec()),
                    timeout: Duration::from_millis(200),
                    request_id: None,
                });
                AFTER_OBSERVATION.with_borrow_mut(|hook| {
                    hook.take();
                });
                REFUSED_OBSERVER.with_borrow_mut(|hook| {
                    hook.take();
                });
                (exit, context)
            })
            .await
            .unwrap();
            let (exit, context) = result;
            assert!(matches!(exit, Exit::StartFailed(ref detail) if detail == UNCONFIRMED));
            let (pid, empty, live, pipes, claim) = rx.try_recv().unwrap();
            assert!(empty && live && pipes && claim);
            assert_eq!(
                std::fs::read(f.0.join("termination-refused-input")).unwrap(),
                b""
            );
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
            assert!(context.rejected.load(Ordering::Acquire));
            assert!(!context.busy.load(Ordering::Acquire));
            let detail = context
                .trace
                .finish::<()>(Err(UNCONFIRMED.into()))
                .unwrap_err();
            assert!(
                detail.ends_with("Removal check: inspect_read / context_after_observation."),
                "{detail}"
            );
            drop(context);
            assert!(actions.begin(key, &Action::Terminate).is_some());
            assert!(effects(&calls(&f)).is_empty());
            service.join().await.unwrap();
        }
    });
}

fn write_termination_image(f: &Fixture) {
    crate::write_executable(
        &f.0.join("hgs"),
        format!(
            "#!/usr/bin/env python3\n{}\n{}",
            include_str!("fixture.py").replace("FIXTURE_SOURCE", &serde_json::to_string(&f.0.join("hgs")).unwrap()),
            include_str!("../fake_hgs.py").replacen(
                "return {'abi': 'desktop-v1', 'configuration': 'a' * 64,",
                "return {'running': True, 'abi': 'desktop-v1', 'configuration': ('b' * 64 if args[:2] == ['desktop', 'status'] and config.get('termination_wrong_scope') and (root / 'termination-retained').exists() else 'a' * 64),",
                1,
            )
        ),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn termination_image_executes_while_its_parent_writer_is_retained() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    let f = Fixture::new(serde_json::json!({}));
    let held = crate::test_executable::HeldWriters::arm();
    write_termination_image(&f);
    let status = std::process::Command::new(f.0.join("hgs"))
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    held.assert_separate(&[f.0.join("hgs")]);
}

pub(super) fn fixture(config: Value) -> Fixture {
    let f = Fixture::new(config);
    write_termination_image(&f);
    std::fs::write(
        f.0.join("termination-capture"),
        serde_json::to_vec(&capture()).unwrap(),
    )
    .unwrap();
    f
}
pub(super) fn capture() -> Capture {
    Capture {
        target: Target {
            computer: String::new(),
            name: "codex/example/original".into(),
            run_id: "original-run".into(),
            conversation_id: "original-conversation".into(),
        },
        machine: "22222222-2222-4222-8222-222222222222".into(),
        root: "/srv/example/work".into(),
        common_dir: "/srv/example/main/.git".into(),
        owner_id: "original-owner".into(),
    }
}
pub(super) async fn connected(f: &Fixture) -> (Arc<Service>, CacheServer) {
    let responder = responder();
    assert!(responder
        .configure_ownership_mode_once(zerus_client_core::desktop::ownership::Mode::Service));
    let cache = CacheServer::start(&f.0, responder.clone());
    let (service, _) = f.start_with_responder(true, Some(responder));
    eventually("termination service connected", || {
        service.snapshot().connected
            && f.calls()
                .iter()
                .any(|call| call["args"] == json!(["desktop", "watch", "--json"]))
    })
    .await;
    (service, cache)
}
fn calls(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(f.0.join("termination-calls"))
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn requests(calls: &[Value], op: &str) -> Vec<Value> {
    calls
        .iter()
        .filter_map(|v| {
            let value = v.get("value")?;
            let request = value.get("request").unwrap_or(value);
            (request["op"] == op).then(|| request.clone())
        })
        .collect()
}
fn effects(calls: &[Value]) -> Vec<Value> {
    calls
        .iter()
        .filter(|v| {
            v["args"]
                .as_array()
                .is_some_and(|args| args.iter().any(|a| a == "session-action"))
        })
        .cloned()
        .collect()
}
fn actions(f: &Fixture) -> crate::actions::Actions {
    crate::actions::Actions::new(f.0.join("marks"))
}
fn selected(service: Arc<Service>) -> Selected {
    let original = capture();
    let (actual, selected) =
        inspect_fixture(service, &original.target, &original.machine, Arc::new(())).unwrap();
    assert_eq!(actual, Some(original));
    selected
}
fn confirm(service: Arc<Service>, pending: Arc<Pending>) -> Confirmation {
    Confirmation::new(
        uuid::Uuid::new_v4().to_string(),
        capture(),
        selected(service),
        pending,
    )
    .unwrap()
}
#[test]
fn the_offer_selection_cannot_be_replaced_before_confirmation() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for change in ["image", "connection", "cancel"] {
            let f = fixture(json!({}));
            let (service, _cache) = connected(&f).await;
            let read = Arc::new(());
            let old_read = Arc::downgrade(&read);
            let original = capture();
            let (actual, selected) =
                inspect_fixture(service.clone(), &original.target, &original.machine, read)
                    .unwrap();
            assert_eq!(actual, Some(original.clone()));
            assert!(old_read.upgrade().is_none(), "offer I/O must actually join");
            let pending = Arc::new(Pending::default());
            match change {
                "image" => {
                    // Identical bytes at the selected name are a different image.
                    std::fs::rename(f.0.join("hgs"), f.0.join("original-hgs")).unwrap();
                    std::fs::copy(f.0.join("original-hgs"), f.0.join("hgs")).unwrap();
                }
                "connection" => {
                    // Same path/configuration, but a different connection original.
                    *service.shared.relay.lock().unwrap() = Some(Arc::new(
                        relay::Connection::select(&f.0.join("hgs")).unwrap(),
                    ));
                }
                "cancel" => selected.0.cancel.store(true, Ordering::Release),
                _ => unreachable!(),
            }
            let native = Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                original,
                selected,
                pending.clone(),
            )
            .unwrap();
            assert!(native.bind(Arc::new(())).is_err(), "{change}");
            assert!(pending.0.lock().unwrap().is_empty());
            assert!(requests(&calls(&f), "termination_outcome").is_empty());
            assert!(effects(&calls(&f)).is_empty());
            service.join().await.unwrap();
        }
    });
}
#[test]
fn one_original_is_retained_before_the_same_scoped_native_call() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for (lost, wait) in [(false, false), (true, false), (false, true)] {
            let f = fixture(json!({"termination_rpc_lost":lost, "termination_effect_wait":wait}));
            let (service, _cache) = connected(&f).await;
            let pending = Arc::new(Pending::default());
            let native = confirm(service.clone(), pending.clone());
            let original = native.invocation().clone();
            let outcome = crate::actions::dispatch_cleanup_native(&actions(&f), native, None)
                .await
                .unwrap();
            assert!(matches!(outcome, Outcome::Done { .. }), "{outcome:?}");
            let calls = calls(&f);
            let offered = requests(&calls, "termination_outcome");
            assert_eq!(offered.len(), 1);
            assert_eq!(
                offered[0]["intent"]["request_id"],
                original.request_id.clone().unwrap()
            );
            assert_eq!(
                offered[0]["intent"]["original"],
                String::from_utf8(original.stdin.clone().unwrap()).unwrap()
            );
            assert_eq!(effects(&calls).len(), 1);
            assert_eq!(effects(&calls)[0]["args"], json!(original.args));
            assert_eq!(
                effects(&calls)[0]["value"],
                serde_json::from_slice::<Value>(original.stdin.as_ref().unwrap()).unwrap()
            );
            assert_eq!(
                requests(&calls, "termination_outcome_status").len(),
                usize::from(lost)
            );
            assert!(Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                capture(),
                selected(service.clone()),
                pending
            )
            .is_err());
            service.join().await.unwrap();
        }
    });
}

#[test]
fn program_continuation_keeps_the_same_native_origin_and_claim_after_exact_done() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let actions = actions(&f);
        let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
        let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
        let hold: Arc<dyn Send + Sync> = Arc::new(hold);
        let selected = selected(service.clone());
        let selected = Selected(selected.0.with_hold(hold.clone()).unwrap());
        let original_hold = selected.0.hold.clone();
        let mut controller = Confirmation::new(
            uuid::Uuid::new_v4().to_string(),
            capture(),
            selected,
            Arc::new(Pending::default()),
        )
        .unwrap()
        .bind(hold)
        .unwrap();
        assert!(
            Arc::ptr_eq(&controller.context.hold, &original_hold),
            "the program source and its bound confirmation share the original hold identity"
        );
        drop(original_hold);
        let invocation = controller.invocation().clone();
        let image = controller.context.image.clone();
        let connection = controller.context.routing.clone();
        controller.retain().unwrap();
        controller.begin_dispatch(&invocation).unwrap();
        let (outcome, source) = controller.run_retaining_source(
            &invocation,
            std::time::Instant::now() + Duration::from_secs(60),
        );
        assert!(matches!(outcome, Outcome::Done { .. }));
        let mut source = source.expect("exact original Done preserves custody, not permission");
        assert!(Arc::ptr_eq(&source.context.image, &image));
        assert!(Arc::ptr_eq(&source.context.routing, &connection));
        assert!(same_invocation(source.invocation(), &invocation));
        assert_eq!(source.capture(), &capture());
        assert_eq!(source.archive(), "11111111-2222-4333-8444-555555555555");
        assert!(source.current());
        eventually("original native Terminate physically joined", || {
            source.original_call_joined()
        })
        .await;
        assert!(
            actions.begin(key.clone(), &Action::Terminate).is_none(),
            "pipeline retains the actual Actions claim after its first helper joins"
        );
        assert!(source.context.current());
        source.expire();
        assert!(
            !source.current(),
            "physical join does not renew the ORIGINAL holder deadline"
        );
        drop(source);
        assert!(actions.begin(key, &Action::Terminate).is_some());
        assert_eq!(effects(&calls(&f)).len(), 1);
        service.join().await.unwrap();
    });
}

#[test]
fn program_continuation_does_not_treat_a_done_reply_as_physical_pipe_settlement() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("termination-release"), b"release");
        }
    }
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"termination_effect_completed_pipe":true}));
        let _release = Release(f.0.clone());
        let (service, _cache) = connected(&f).await;
        let actions = actions(&f);
        let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
        let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
        let mut controller = confirm(service.clone(), Arc::new(Pending::default()))
            .bind(Arc::new(hold))
            .unwrap();
        let invocation = controller.invocation().clone();
        controller.retain().unwrap();
        controller.begin_dispatch(&invocation).unwrap();
        let (outcome, source) = controller.run_retaining_source(
            &invocation,
            std::time::Instant::now() + Duration::from_secs(60),
        );
        assert!(matches!(outcome, Outcome::Done { .. }), "{outcome:?}");
        let source = source.unwrap();
        assert!(
            !source.original_call_joined(),
            "exit and parsed Done cannot settle an original output reader"
        );
        assert!(actions.begin(key.clone(), &Action::Terminate).is_none());
        std::fs::write(f.0.join("termination-release"), b"release").unwrap();
        eventually("actual original output reader EOF", || {
            source.original_call_joined()
        })
        .await;
        assert!(
            actions.begin(key.clone(), &Action::Terminate).is_none(),
            "readonly pipeline origin stays charged independently"
        );
        drop(source);
        assert!(actions.begin(key, &Action::Terminate).is_some());
        assert_eq!(effects(&calls(&f)).len(), 1);
        service.join().await.unwrap();
    });
}

#[test]
fn program_continuation_never_reconstructs_an_origin_from_unknown_or_removed_outcomes() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for config in [
            json!({"termination_effect_lost":true}),
            json!({"termination_effect_cleanup":"unknown"}),
            json!({"termination_effect_cleanup":"removed"}),
        ] {
            let f = fixture(config);
            let (service, _cache) = connected(&f).await;
            let pending = Arc::new(Pending::default());
            let mut controller = confirm(service.clone(), pending.clone())
                .bind(Arc::new(()))
                .unwrap();
            let invocation = controller.invocation().clone();
            controller.retain().unwrap();
            controller.begin_dispatch(&invocation).unwrap();
            let (_outcome, source) = controller.run_retaining_source(
                &invocation,
                std::time::Instant::now() + Duration::from_secs(60),
            );
            assert!(source.is_none());
            assert!(Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                capture(),
                selected(service.clone()),
                pending
            )
            .is_err());
            assert_eq!(effects(&calls(&f)).len(), 1);
            service.join().await.unwrap();
        }
    });
}

#[test]
fn corrupted_epoch_scope_or_changed_capture_never_dispatches_or_reoffers() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for scenario in [
            "termination_bad_first_epoch",
            "termination_wrong_scope",
            "termination_changed_capture",
        ] {
            let mut config = json!({});
            config[scenario] = json!(true);
            let f = fixture(config);
            let (service, _cache) = connected(&f).await;
            let pending = Arc::new(Pending::default());
            let native = confirm(service.clone(), pending.clone());
            let original_selection = native.selected.clone();
            let outcome = crate::actions::dispatch_cleanup_native(&actions(&f), native, None)
                .await
                .unwrap();
            assert!(
                !matches!(outcome, Outcome::Done { .. }),
                "{scenario}:{outcome:?}"
            );
            assert!(effects(&calls(&f)).is_empty());
            assert_eq!(requests(&calls(&f), "termination_outcome").len(), 1);
            let mut renamed = capture();
            renamed.target.name = "renamed/original".into();
            renamed.target.computer = "another-alias".into();
            renamed.target.conversation_id = "successor-conversation".into();
            assert!(Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                renamed,
                original_selection,
                pending
            )
            .is_err());
            service.join().await.unwrap();
        }
    });
}
#[test]
fn native_effect_loss_consumes_the_one_shot_and_keeps_original_correlation() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"termination_effect_lost":true}));
        let (service, _cache) = connected(&f).await;
        let pending = Arc::new(Pending::default());
        let native = confirm(service.clone(), pending.clone());
        let id = native.invocation().request_id.clone().unwrap();
        let outcome = crate::actions::dispatch_cleanup_native(&actions(&f), native, None)
            .await
            .unwrap();
        assert!(!matches!(outcome, Outcome::Done { .. }));
        assert_eq!(effects(&calls(&f)).len(), 1);
        assert_eq!(requests(&calls(&f), "termination_outcome").len(), 1);
        {
            let entries = pending.0.lock().unwrap();
            let retained = entries.values().next().unwrap();
            assert_eq!(retained.original.request_id, id);
            assert_eq!(
                retained.stage.load(Ordering::Acquire),
                Stage::DispatchBegun as u8
            );
        }
        assert!(Confirmation::new(
            uuid::Uuid::new_v4().to_string(),
            capture(),
            selected(service.clone()),
            pending
        )
        .is_err());
        service.join().await.unwrap();
    });
}
struct Release(std::path::PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, []);
    }
}
#[test]
fn retained_rpc_pipe_releases_the_actions_claim_and_refuses_native_effect() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"termination_pipe":true}));
        f.1.store(true, Ordering::Release);
        let _release = Release(f.0.join("termination-release"));
        let (service, _cache) = connected(&f).await;
        let pending = Arc::new(Pending::default());
        let actions = actions(&f);
        let outcome = crate::actions::dispatch_cleanup_native(
            &actions,
            confirm(service.clone(), pending.clone()),
            None,
        )
        .await
        .unwrap();
        assert!(!matches!(outcome, Outcome::Done { .. }));
        assert!(effects(&calls(&f)).is_empty());
        let target = capture().target;
        assert_original_exited_with_live_pipe_holder(&f);
        assert!(
            actions.reserve_end(&target).is_some(),
            "a bounded RPC must release the session lane before pipe EOF"
        );
        assert!(service.shared.rpc.snapshot().unwrap().count(Role::Fleet) > 0);
        assert!(
            Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                capture(),
                selected(service.clone()),
                pending
            )
            .is_err(),
            "releasing the lane must never recreate the original consent"
        );
        std::fs::write(f.0.join("termination-release"), []).unwrap();
        eventually("original retention pipes physically finish", || {
            service.shared.rpc.snapshot().unwrap().count(Role::Fleet) == 0
        })
        .await;
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
        f.1.store(false, Ordering::Release);
    });
}

fn assert_original_exited_with_live_pipe_holder(f: &Fixture) {
    let holder: Value =
        serde_json::from_slice(&std::fs::read(f.0.join("termination-pipe-holder")).unwrap())
            .unwrap();
    assert!(
        !PathBuf::from(format!("/proc/{}", holder["parent"])).exists(),
        "the original helper was reaped"
    );
    for pipe in ["stdout", "stderr"] {
        let fd = if pipe == "stdout" { 1 } else { 2 };
        let actual = std::fs::read_link(format!("/proc/{}/fd/{fd}", holder["pid"])).unwrap();
        assert_eq!(actual.to_str(), holder[pipe].as_str());
        assert!(actual.to_str().unwrap().starts_with("pipe:["));
    }
    assert!(!f.0.join("termination-holder-exited").exists());
}

#[test]
fn retained_read_pipe_releases_the_actions_claim_without_readmitting_the_context() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"termination_inspect_pipe":true}));
        let _release = Release(f.0.join("termination-release"));
        let (service, _cache) = connected(&f).await;
        let actions = actions(&f);
        let target = capture().target;
        let hold = actions
            .begin(
                crate::actions::Lane::key(&target, &Action::Terminate),
                &Action::Terminate,
            )
            .unwrap();
        let context = Context::new_supported(service.clone(), Arc::new(hold)).unwrap();
        let exit = context.read(&Invocation {
            args: vec!["inspect".into(), target.name.clone(), "--json".into()],
            stdin: None,
            timeout: Duration::from_millis(200),
            request_id: None,
        });
        assert!(matches!(exit, Exit::StartFailed(_)), "{exit:?}");
        assert_original_exited_with_live_pipe_holder(&f);
        assert!(
            actions.reserve_end(&target).is_some(),
            "a bounded read must release the session lane before pipe EOF"
        );
        assert!(context.start_call().is_err());
        assert!(
            context.busy.load(Ordering::Acquire),
            "reader custody is still charged"
        );
        std::fs::write(f.0.join("termination-release"), []).unwrap();
        eventually("original read pipe EOF", || {
            !context.busy.load(Ordering::Acquire)
        })
        .await;
        assert!(
            context.start_call().is_err(),
            "a failed context cannot be reused after EOF"
        );
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
    });
}

#[test]
fn missing_unknown_corrupt_or_foreign_original_lookup_never_replays_or_dispatches() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for case in [
            "missing",
            "unknown",
            "corrupt",
            "wrong-original",
            "cleanup-authority",
        ] {
            let f = fixture(json!({"termination_rpc_lost":true,"termination_lookup_case":case}));
            let (service, _cache) = connected(&f).await;
            let pending = Arc::new(Pending::default());
            let native = confirm(service.clone(), pending.clone());
            let id = native.invocation().request_id.clone().unwrap();
            let outcome = crate::actions::dispatch_cleanup_native(&actions(&f), native, None)
                .await
                .unwrap();
            assert!(
                !matches!(outcome, Outcome::Done { .. }),
                "{case}:{outcome:?}"
            );
            let calls = calls(&f);
            assert_eq!(requests(&calls, "termination_outcome").len(), 1);
            let queries = requests(&calls, "termination_outcome_status");
            assert_eq!(queries.len(), 1);
            assert_eq!(queries[0]["original_request_id"], id);
            assert!(effects(&calls).is_empty());
            assert!(Confirmation::new(
                uuid::Uuid::new_v4().to_string(),
                capture(),
                selected(service.clone()),
                pending
            )
            .is_err());
            service.join().await.unwrap();
        }
    });
}
#[test]
fn the_live_dispatch_transition_is_one_way_even_without_a_native_spawn() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let pending = Arc::new(Pending::default());
        let mut controller = confirm(service.clone(), pending.clone())
            .bind(Arc::new(()))
            .unwrap();
        let invocation = controller.invocation().clone();
        controller.retain().unwrap();
        controller.begin_dispatch(&invocation).unwrap();
        assert!(controller.begin_dispatch(&invocation).is_err());
        assert!(controller.recheck_scope().is_err());
        assert!(matches!(controller.read(&invocation), Exit::StartFailed(_)));
        drop(controller);
        assert!(effects(&calls(&f)).is_empty());
        assert!(Confirmation::new(
            uuid::Uuid::new_v4().to_string(),
            capture(),
            selected(service.clone()),
            pending
        )
        .is_err());
        service.join().await.unwrap();
    });
}

#[test]
fn a_new_legacy_helper_is_probed_before_any_new_subcommand() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for broken in ["missing", "substring", "frontend", "utf8"] {
            let f = fixture(json!({}));
            let (service, _cache) = connected(&f).await;
            eventually("original catalog published", || {
                service.shared.pump.live("")
            })
            .await;
            let mut help = format!(
                "{HELP}\n{}\n{}\n",
                wire::HELP_MARKER,
                zerus_client_core::desktop::ui_requests::HELP_MARKER
            )
            .into_bytes();
            match broken {
                "missing" => help = b"Legacy fixture CLI\n".to_vec(),
                "substring" => {
                    help =
                        format!("prefix {} suffix\n", String::from_utf8(help).unwrap()).into_bytes()
                }
                "frontend" => help = format!("{HELP}\n{}\n", wire::HELP_MARKER).into_bytes(),
                "utf8" => help.push(0xff),
                _ => unreachable!(),
            }
            // Service's original watch stays connected; this newly installed
            // image must prove its own ABI before any new desktop command.
            std::fs::rename(f.0.join("hgs"), f.0.join("original-hgs")).unwrap();
            std::fs::write(f.0.join("hgs"), format!(
                "#!/usr/bin/python3\nimport sys,json,os\nfrom pathlib import Path\np=Path({fixture_root}).parent\nwith (p/'legacy-calls').open('a') as f:f.write(json.dumps(sys.argv[1:])+'\\n')\nif sys.argv[1:] != ['--help']:sys.exit(2)\nsys.stdout.buffer.write(bytes({help:?}))\n", fixture_root=serde_json::to_string(&f.0.join("hgs")).unwrap()
            )).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f.0.join("hgs"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            assert!(Selected::inspect(
                service.clone(),
                &capture().target,
                &capture().machine,
                Arc::new(())
            )
            .is_err());
            let actual: Vec<Value> = std::fs::read_to_string(f.0.join("legacy-calls"))
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect();
            assert_eq!(actual, vec![json!(["--help"])], "{broken}");
            assert!(requests(&calls(&f), "termination_outcome").is_empty());
            assert!(effects(&calls(&f)).is_empty());
            service.join().await.unwrap();
        }
    });
}

#[test]
fn refused_or_expired_original_binding_withholds_input_and_keeps_real_custody() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    tauri::async_runtime::block_on(async {
        for expired in [false, true] {
            let f = fixture(json!({"termination_refused_registration":true}));
            let (service, _cache) = connected(&f).await;
            let actions = Arc::new(actions(&f));
            let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
            let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
            let controller = confirm(service.clone(), Arc::new(Pending::default()))
                .bind(Arc::new(hold))
                .unwrap();
            let registration = service
                .shared
                .responder
                .as_ref()
                .unwrap()
                .prepare_ui_request(
                    uuid::Uuid::new_v4().to_string(),
                    Request::TerminationOutcome {
                        intent: Box::new(controller.intent.clone()),
                    },
                )
                .unwrap();
            let slot = registration.resources();
            let original_slot = Arc::downgrade(&slot);
            drop(slot);
            let other = if expired {
                // Expiration removes evidence, never the original resource slot.
                std::thread::sleep(
                    zerus_client_core::desktop::ui_requests::LIFETIME + Duration::from_millis(10),
                );
                None
            } else {
                let child = Owned(
                    std::process::Command::new("/bin/sleep")
                        .arg("20")
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .unwrap(),
                );
                assert!(registration.bind_original(&child.0));
                Some(child)
            };
            let input = serde_json::to_vec(registration.envelope()).unwrap();
            let context = controller.context.clone();
            drop(controller);
            let busy = context.busy.clone();
            let root = f.0.clone();
            let check_actions = actions.clone();
            let check_key = key.clone();
            let check_slot = original_slot.clone();
            let check_service = service.clone();
            let (observed, observation) = std::sync::mpsc::sync_channel(1);
            let result = tauri::async_runtime::spawn_blocking(move || {
                REFUSED_OBSERVER.with_borrow_mut(|hook| {
                    *hook = Some(Box::new(move |child| {
                        let until = std::time::Instant::now() + Duration::from_secs(2);
                        let path = root.join("termination-refused-input");
                        while !path.exists() && std::time::Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        // No panic in the spawn callback: return THIS child to the
                        // runner even when a guard mutation makes the control fail.
                        let checks = (
                            std::fs::read(path).is_ok_and(|raw| raw.is_empty()),
                            child.try_wait().is_ok_and(|s| s.is_none()),
                            check_actions.begin(check_key, &Action::Terminate).is_none(),
                            check_slot.upgrade().is_some() && busy.load(Ordering::Acquire),
                            check_service
                                .shared
                                .rpc
                                .snapshot()
                                .is_ok_and(|v| v.count(Role::Fleet) > 0),
                        );
                        let _ = observed.send(checks);
                    }))
                });
                context.rpc(
                    vec!["desktop".into(), "frontend-request".into(), "--json".into()],
                    Some(input),
                    Some(registration),
                )
            })
            .await
            .unwrap();
            assert!(result.is_err());
            assert_eq!(
                observation.try_recv().unwrap(),
                (true, true, true, true, true)
            );
            assert_eq!(
                std::fs::read(f.0.join("termination-refused-input")).unwrap(),
                b""
            );
            assert!(
                original_slot.upgrade().is_none(),
                "registration released only after physical stop/join"
            );
            assert!(actions.begin(key, &Action::Terminate).is_some());
            assert!(requests(&calls(&f), "termination_outcome").is_empty());
            assert!(effects(&calls(&f)).is_empty());
            drop(other);
            service.join().await.unwrap();
        }
    });
}

#[test]
fn scope_drift_during_observation_or_binding_withholds_input_until_real_custody_settles() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for after_binding in [false, true] {
            for change_configuration in [false, true] {
                let f = fixture(json!({"termination_refused_registration":true}));
                let (service, _cache) = connected(&f).await;
                let actions = Arc::new(actions(&f));
                let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
                let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
                let controller = confirm(service.clone(), Arc::new(Pending::default()))
                    .bind(Arc::new(hold))
                    .unwrap();
                let registration = service
                    .shared
                    .responder
                    .as_ref()
                    .unwrap()
                    .prepare_ui_request(
                        uuid::Uuid::new_v4().to_string(),
                        Request::TerminationOutcome {
                            intent: Box::new(controller.intent.clone()),
                        },
                    )
                    .unwrap();
                let slot = registration.resources();
                let original_slot = Arc::downgrade(&slot);
                drop(slot);
                let input = serde_json::to_vec(registration.envelope()).unwrap();
                let context = controller.context.clone();
                drop(controller);
                let checked = context.clone();
                let check_service = service.clone();
                let check_actions = actions.clone();
                let check_key = key.clone();
                let check_slot = original_slot.clone();
                let root = f.0.clone();
                let (changed_tx, changed_rx) = std::sync::mpsc::sync_channel(1);
                let (observed_tx, observed_rx) = std::sync::mpsc::sync_channel(1);
                let result = tauri::async_runtime::spawn_blocking(move || {
                    let change: RefusedObserver = Box::new(move |child| {
                        let live = child.try_wait().is_ok_and(|s| s.is_none());
                        if change_configuration {
                            *checked.service.shared.configuration.lock().unwrap() =
                                Some("b".repeat(64));
                        } else {
                            checked.cancel.store(true, Ordering::Release);
                        }
                        let _ = changed_tx.send((child.id(), live));
                    });
                    if after_binding {
                        AFTER_BINDING.with_borrow_mut(|hook| *hook = Some(change));
                    } else {
                        AFTER_OBSERVATION.with_borrow_mut(|hook| *hook = Some(change));
                    }
                    REFUSED_OBSERVER.with_borrow_mut(|hook| {
                        *hook = Some(Box::new(move |child| {
                            let path = root.join("termination-refused-input");
                            let until = std::time::Instant::now() + Duration::from_secs(2);
                            while !path.exists() && std::time::Instant::now() < until {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            // All assertions are outside the spawn hook. Even a
                            // failing mutation returns THIS child to the runner.
                            let checks = (
                                std::fs::read(path).is_ok_and(|raw| raw.is_empty()),
                                child.try_wait().is_ok_and(|s| s.is_none()),
                                check_actions.begin(check_key, &Action::Terminate).is_none(),
                                check_slot.upgrade().is_some(),
                                check_service
                                    .shared
                                    .rpc
                                    .snapshot()
                                    .is_ok_and(|v| v.count(Role::Fleet) > 0),
                            );
                            let _ = observed_tx.send(checks);
                        }))
                    });
                    let result = context.rpc(
                        vec!["desktop".into(), "frontend-request".into(), "--json".into()],
                        Some(input),
                        Some(registration),
                    );
                    AFTER_OBSERVATION.with_borrow_mut(|hook| {
                        hook.take();
                    });
                    AFTER_BINDING.with_borrow_mut(|hook| {
                        hook.take();
                    });
                    REFUSED_OBSERVER.with_borrow_mut(|hook| {
                        hook.take();
                    });
                    (result, context)
                })
                .await
                .unwrap();
                let (result, context) = result;
                assert_eq!(result, Err(UNCONFIRMED.into()));
                let (pid, live) = changed_rx.try_recv().unwrap();
                assert!(live, "barrier must run on the actual live original child");
                assert_eq!(
                    observed_rx.try_recv().unwrap(),
                    (true, true, true, true, true)
                );
                assert_eq!(
                    std::fs::read(f.0.join("termination-refused-input")).unwrap(),
                    b""
                );
                let mut status = 0;
                assert_eq!(
                    unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ECHILD)
                );
                assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
                assert!(
                    original_slot.upgrade().is_none(),
                    "original registration survives until pipe/child settlement"
                );
                // A later current scope or ABI probe cannot clear rejection.
                context.cancel.store(false, Ordering::Release);
                *service.shared.configuration.lock().unwrap() =
                    Some(context.scope.configuration.clone());
                assert!(context.rejected.load(Ordering::Acquire));
                assert_eq!(context.probe_abi(), Err(UNCONFIRMED.into()));
                assert!(requests(&calls(&f), "termination_outcome").is_empty());
                assert!(effects(&calls(&f)).is_empty());
                assert!(!context.busy.load(Ordering::Acquire));
                drop(context);
                assert!(actions.begin(key, &Action::Terminate).is_some());
                service.join().await.unwrap();
            }
        }
    });
}

#[test]
fn an_oversized_native_receipt_is_bounded_and_never_restores_dispatch() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"termination_effect_oversize":true}));
        let (service, _cache) = connected(&f).await;
        let pending = Arc::new(Pending::default());
        let actions = actions(&f);
        let key = crate::actions::Lane::key(&capture().target, &Action::Terminate);
        let hold = actions.begin(key.clone(), &Action::Terminate).unwrap();
        let mut controller = confirm(service.clone(), pending.clone())
            .bind(Arc::new(hold))
            .unwrap();
        let invocation = controller.invocation().clone();
        controller.retain().unwrap();
        controller.begin_dispatch(&invocation).unwrap();
        let exit = controller.run_native(&invocation);
        assert!(
            matches!(&exit, Exit::StartFailed(_)),
            "the native runner must enforce the receipt cap"
        );
        assert!(!matches!(
            capture().completed(&invocation, exit),
            Outcome::Done { .. }
        ));
        assert_eq!(effects(&calls(&f)).len(), 1);
        assert!(Confirmation::new(
            uuid::Uuid::new_v4().to_string(),
            capture(),
            selected(service.clone()),
            pending
        )
        .is_err());
        eventually("oversize native resources physically joined", || {
            actions.begin(key.clone(), &Action::Terminate).is_some()
        })
        .await;
        service.join().await.unwrap();
    });
}

#[test]
fn overflow_releases_the_actions_claim_but_keeps_original_dispatch_unconfirmed() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("termination-release"), b"release");
        }
    }
    tauri::async_runtime::block_on(async {
        let f = fixture(
            json!({"termination_effect_oversize":true,"termination_effect_oversize_pipe":true}),
        );
        f.1.store(true, Ordering::Release);
        let _release = Release(f.0.clone());
        let (service, _cache) = connected(&f).await;
        let pending = Arc::new(Pending::default());
        let actions = actions(&f);
        let original = confirm(service.clone(), pending.clone());
        let image = Arc::downgrade(&original.selected.0.image);
        let target = original.capture().target.clone();
        let outcome = crate::actions::dispatch_cleanup_native(&actions, original, None)
            .await
            .unwrap();
        assert!(!matches!(outcome, Outcome::Done { .. }));
        assert_eq!(effects(&calls(&f)).len(), 1);
        assert_original_exited_with_live_pipe_holder(&f);
        assert!(
            actions.reserve_end(&target).is_some(),
            "a bounded native call must release the session lane before pipe EOF"
        );
        assert!(
            image.upgrade().is_some(),
            "original image custody survives lane revocation"
        );
        assert!(Confirmation::new(
            uuid::Uuid::new_v4().to_string(),
            capture(),
            selected(service.clone()),
            pending
        )
        .is_err());
        std::fs::write(f.0.join("termination-release"), b"release").unwrap();
        eventually("overflow original pipe EOF", || image.upgrade().is_none()).await;
        service.join().await.unwrap();
        f.1.store(false, Ordering::Release);
    });
}

#[test]
fn an_in_place_rewrite_after_the_last_spawn_check_cannot_replace_native_code() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let mut controller = confirm(service.clone(), Arc::new(Pending::default()))
            .bind(Arc::new(()))
            .unwrap();
        controller.retain().unwrap();
        let invocation = controller.invocation().clone();
        controller.begin_dispatch(&invocation).unwrap();
        let path = f.0.join("hgs");
        let marker = f.0.join("replacement-executed");
        use std::os::unix::fs::MetadataExt;
        let original_inode = std::fs::metadata(&path).unwrap().ino();
        AFTER_SPAWN_CHECK.with_borrow_mut(|slot| {
            *slot = Some(Box::new(move || {
                std::fs::write(
                    &path,
                    format!(
                        "#!/bin/sh\nprintf replacement > '{}'\nexit 0\n",
                        marker.display()
                    ),
                )
                .unwrap();
                assert_eq!(std::fs::metadata(&path).unwrap().ino(), original_inode);
            }))
        });
        let exit = controller.run_native(&invocation);
        let replacement = f.0.join("replacement-executed").exists();
        let native_calls = effects(&calls(&f)).len();
        // Mutation failures must also settle the original watch/pipe resources.
        service.join().await.unwrap();
        assert!(matches!(exit, Exit::Code { code: 0, stdout, .. }
            if serde_json::from_slice::<Value>(&stdout).unwrap()["status"] == "completed"));
        assert!(!replacement);
        assert_eq!(native_calls, 1);
    });
}

#[test]
fn read_only_offer_diagnostics_pin_real_refusal_stages_without_effects_or_child_text() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for (case, expected) in [
            ("abi", "abi_probe / abi_marker"),
            ("status", "service_scope / status_scope"),
            ("lifecycle", "lifecycle_probe / check_refused"),
            ("inspect_exit", "inspect_read / child_exit"),
            ("inspect_json", "inspect_read / check_refused"),
            ("capture", "capture / check_refused"),
            ("final", "final_offer_current / context_current"),
        ] {
            let f = fixture(json!({}));
            let (service, _cache) = connected(&f).await;
            // Change reply DATA only after the real connection is ready. The
            // retained executable remains the same immutable Linux snapshot.
            let temporary = f.0.join("fixture-diagnostic.json");
            std::fs::write(
                &temporary,
                json!({"termination_offer_case": case}).to_string(),
            )
            .unwrap();
            std::fs::rename(temporary, f.0.join("fixture.json")).unwrap();
            let checked = service.clone();
            let original = capture();
            let result = tauri::async_runtime::spawn_blocking(move || {
                if case == "final" {
                    let cancel = checked.shared.cancellation.lock().unwrap().clone().unwrap();
                    AFTER_CAPTURE.with_borrow_mut(|hook| {
                        *hook = Some(Box::new(move || cancel.store(true, Ordering::Release)));
                    });
                }
                let result =
                    inspect_fixture(checked, &original.target, &original.machine, Arc::new(()));
                AFTER_CAPTURE.with_borrow_mut(|hook| {
                    hook.take();
                });
                result
            })
            .await
            .unwrap();
            let detail = result
                .err()
                .expect("the actual read-only offer must refuse");
            assert_eq!(detail, format!("{UNCONFIRMED}\nRemoval check: {expected}."));
            for private in [
                "private fixture",
                "/srv/example",
                "original-run",
                "original-owner",
            ] {
                assert!(!detail.contains(private), "{detail}");
            }
            assert!(requests(&calls(&f), "termination_outcome").is_empty());
            assert!(effects(&calls(&f)).is_empty());
            service.join().await.unwrap();
        }
    });
}

#[test]
fn disconnected_offer_context_is_diagnostic_without_spawning_a_native_effect() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _pump) = f.start(false);
        let original = capture();
        let detail = inspect_fixture(
            service.clone(),
            &original.target,
            &original.machine,
            Arc::new(()),
        )
        .err()
        .expect("disallowed context cannot inspect");
        assert_eq!(
            detail,
            format!("{UNCONFIRMED}\nRemoval check: context_prepare / disallowed.")
        );
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
    });
}

#[test]
fn offer_physical_finish_diagnostic_keeps_the_original_rpc_charge_until_real_release() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let context = Context::new_supported(service.clone(), Arc::new(())).unwrap();
        context.trace.phase(Phase::AbiProbe);
        let held = context.start_call().unwrap();
        assert!(context.busy.load(Ordering::Acquire));
        assert!(service.shared.rpc.snapshot().unwrap().count(Role::Fleet) > 0);
        assert!(context.finish_call().is_err());
        let detail = context
            .trace
            .finish::<()>(Err(UNCONFIRMED.into()))
            .unwrap_err();
        assert_eq!(
            detail,
            format!("{UNCONFIRMED}\nRemoval check: abi_probe / physical_finish.")
        );
        assert!(context.busy.load(Ordering::Acquire));
        drop(held);
        assert!(!context.busy.load(Ordering::Acquire));
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
    });
}

#[test]
fn rpc_scope_drift_after_pipe_setup_is_sticky_and_cannot_readmit_the_context() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let context = Context::new_supported(service.clone(), Arc::new(())).unwrap();
        let checked = context.clone();
        BEFORE_RPC_INPUT.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move || {
                *checked.service.shared.configuration.lock().unwrap() = Some("b".repeat(64));
            }));
        });
        assert!(context.rpc(vec!["--help".into()], None, None).is_err());
        assert!(context.rejected.load(Ordering::Acquire));
        assert!(!context.busy.load(Ordering::Acquire));
        *service.shared.configuration.lock().unwrap() = Some("a".repeat(64));
        let called = Arc::new(AtomicBool::new(false));
        let spawned = called.clone();
        AFTER_BINDING.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move |_| spawned.store(true, Ordering::Release)));
        });
        assert!(context.rpc(vec!["--help".into()], None, None).is_err());
        AFTER_BINDING.with_borrow_mut(|hook| {
            hook.take();
        });
        assert!(!called.load(Ordering::Acquire));
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
    });
}

pub(super) fn rendezvous_enabled() -> bool {
    RENDEZVOUS.get()
}
struct Rendezvous;
impl Rendezvous {
    fn enter() -> Self {
        RENDEZVOUS.set(true);
        Self
    }
}
impl Drop for Rendezvous {
    fn drop(&mut self) {
        RENDEZVOUS.set(false);
    }
}

#[test]
fn mac_transport_prefix_covers_rpc_reads_and_exact_native_invocation() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let _transport = Rendezvous::enter();
        let mut controller = confirm(service.clone(), Arc::new(Pending::default()))
            .bind(Arc::new(()))
            .unwrap();
        let invocation = controller.invocation().clone();
        controller.retain().unwrap();
        controller.begin_dispatch(&invocation).unwrap();
        let outcome = controller.run_native(&invocation);
        assert!(matches!(outcome, Exit::Code { code: 0, .. }), "{outcome:?}");
        let wrapped: Vec<Vec<String>> = std::fs::read_to_string(f.0.join("rendezvous-calls"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            wrapped.iter().filter(|args| **args == ["--help"]).count(),
            3
        );
        assert!(wrapped
            .iter()
            .any(|args| args.iter().any(|arg| arg == "inspect")));
        assert_eq!(
            wrapped
                .iter()
                .filter(|args| **args == invocation.args)
                .count(),
            1
        );
        let native = effects(&calls(&f));
        assert_eq!(native.len(), 1);
        assert_eq!(
            native[0]["value"],
            serde_json::from_slice::<Value>(invocation.stdin.as_ref().unwrap()).unwrap()
        );
        assert_eq!(
            native[0]["value"]["request_id"],
            invocation.request_id.unwrap()
        );
        service.join().await.unwrap();
    });
}

#[test]
fn mac_transport_refusal_during_observation_releases_no_prefix_or_private_input() {
    if crate::test_context::desktop_fixture() {
        return;
    }
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _cache) = connected(&f).await;
        let _transport = Rendezvous::enter();
        let context = Context::new_supported(service.clone(), Arc::new(())).unwrap();
        let changed = context.clone();
        AFTER_OBSERVATION.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move |_| {
                changed.cancel.store(true, Ordering::Release);
            }))
        });
        let path = f.0.join("rendezvous-refused-input");
        let saw_eof = Arc::new(AtomicBool::new(false));
        let observed = saw_eof.clone();
        REFUSED_OBSERVER.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move |child| {
                // Observe the exact child's actual EOF before the owned runner
                // handles cancellation. Never panic/drop custody in spawn.
                let until = std::time::Instant::now() + Duration::from_secs(2);
                while !path.exists() && std::time::Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
                observed.store(
                    child.stdin.is_none()
                        && child.stdout.is_some()
                        && child.stderr.is_some()
                        && std::fs::read(path).is_ok_and(|bytes| bytes.is_empty()),
                    Ordering::Release,
                );
            }));
        });
        assert!(context
            .rpc(
                vec!["desktop".into(), "frontend-request".into(), "--json".into()],
                Some(b"private-fixture-frame\n".to_vec()),
                None
            )
            .is_err());
        assert!(saw_eof.load(Ordering::Acquire));
        assert!(!f.0.join("rendezvous-calls").exists());
        assert!(!context.busy.load(Ordering::Acquire));
        assert!(effects(&calls(&f)).is_empty());
        service.join().await.unwrap();
    });
}
