use super::*;
use crate::desktop_service::{
    frontend::tests::native::{responder, CacheServer},
    tests::{eventually, Fixture},
};

fn private_case(case: &'static str) -> bool {
    const MARKER: &str = "ZERUS_CREATION_CLONE_CASE";
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
    match run::run_without(
        &std::env::current_exe().unwrap(),
        &invocation,
        &[(MARKER, case)],
        &["HGS_SESSION", "HGS_RUN_ID"],
        64 << 10,
    )
    .unwrap()
    {
        zerus_client_core::Exit::Code {
            code,
            stdout,
            stderr,
        } => assert_eq!(code, 0, "{}\n{stderr}", String::from_utf8_lossy(&stdout)),
        other => panic!("{other:?}"),
    }
    false
}
fn fixture(config: Value) -> Fixture {
    let f = Fixture::new(config);
    std::fs::remove_file(f.0.join("hgs")).unwrap();
    std::fs::write(
        f.0.join("hgs"),
        format!(
            "#!/usr/bin/env python3\n{}\n{}",
            include_str!("fixture.py"),
            include_str!("../fake_hgs.py")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(f.0.join("hgs"), std::fs::Permissions::from_mode(0o700)).unwrap();
    f
}
fn operation() -> (String, Value) {
    let id = uuid::Uuid::new_v4().to_string();
    (
        id.clone(),
        json!({"id":id,"creation_schema":1,"created":1,"revision":4,"cmd":"codex","tag":"example","host":"", "state":"cloning","repository":true,
        "parent":"/srv","target":"/srv/repo","repository_name":"repo","url":"https://example.invalid/repo.git",
        "clone_id":uuid::Uuid::new_v4().to_string(),"expected_machine":"22222222-2222-4222-8222-222222222222"}),
    )
}
fn calls(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(f.0.join("clone-calls"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}
async fn connected(f: &Fixture) -> (Arc<Service>, CacheServer) {
    let responder = responder();
    assert!(responder
        .configure_ownership_mode_once(zerus_client_core::desktop::ownership::Mode::Service));
    let cache = CacheServer::start(&f.0, responder.clone());
    let (service, _) = f.start_with_responder(true, Some(responder));
    eventually("clone service connected", || service.snapshot().connected).await;
    (service, cache)
}
async fn answer(service: Arc<Service>, id: String, value: Value, fresh: bool) -> creation::Answer {
    tauri::async_runtime::spawn_blocking(move || {
        call(
            Some(service),
            &id,
            &value,
            fresh,
            false,
            &AtomicBool::new(false),
        )
    })
    .await
    .unwrap()
}
#[test]
fn original_clone_child_is_bound_and_lost_reply_only_allows_a_scoped_read() {
    if !private_case("desktop_service::creation_clone::tests::original_clone_child_is_bound_and_lost_reply_only_allows_a_scoped_read") {return}
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"clone_lost":true}));
        let (service, _cache) = connected(&f).await;
        let (id, value) = operation();
        assert!(
            !answer(service.clone(), id.clone(), value.clone(), true)
                .await
                .ok
        );
        assert!(f.0.join("clone-bound").exists());
        let result = answer(service.clone(), id.clone(), value.clone(), false).await;
        assert_eq!(result.data["status"], "cloned");
        assert_eq!(
            calls(&f)
                .iter()
                .filter(|c| c["value"]["request"]["op"] == "creation_clone")
                .count(),
            1
        );
        assert_eq!(
            calls(&f)
                .iter()
                .filter(|c| c["value"]["op"] == "creation_clone_result")
                .count(),
            1
        );
        assert!(!f.calls().iter().any(|c| c["args"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "repository"))));
        service.join().await.unwrap();
    });
}
#[test]
fn unsupported_or_mismatched_clone_results_never_advance_the_native_host() {
    if !private_case("desktop_service::creation_clone::tests::unsupported_or_mismatched_clone_results_never_advance_the_native_host") {return}
    tauri::async_runtime::block_on(async {
        for (config, writes) in [
            (json!({"clone_help_fail":true}), 0),
            (json!({"clone_wrong_identity":true}), 1),
        ] {
            let f = fixture(json!({}));
            let (service, _cache) = connected(&f).await;
            std::fs::write(f.0.join("fixture.json"), config.to_string()).unwrap();
            let (id, value) = operation();
            let result = answer(service.clone(), id, value, true).await;
            assert!(!result.ok);
            assert_eq!(
                result.error,
                zerus_client_core::repository::UNCONFIRMED_OPERATION
            );
            assert_eq!(calls(&f).len(), writes);
            service.join().await.unwrap();
        }
    });
}

#[test]
fn missing_marker_definitively_refuses_only_a_fresh_unsent_clone() {
    if !private_case("desktop_service::creation_clone::tests::missing_marker_definitively_refuses_only_a_fresh_unsent_clone") {return}
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"clone_no_abi":true}));
        let (service, _cache) = connected(&f).await;
        for windows in [false, true] {
            let (id, mut value) = operation();
            if windows {
                value["parent"] = json!(r"C:\Work");
                value["target"] = json!(r"C:\Work\repo");
                assert!(
                    wire::Plan::from_operation(&id, &value).is_none(),
                    "the missing-marker refusal precedes POSIX admission validation"
                );
            }
            let refused = answer(service.clone(), id.clone(), value.clone(), true).await;
            assert!(
                refused.ok,
                "a definite pre-send result, not a lost exchange"
            );
            assert_eq!(refused.data["status"], "failed");
            assert_eq!(refused.data["request_id"], value["clone_id"]);
            assert_eq!(refused.data["path"], value["target"]);
            assert_eq!(refused.data["detail"], UNAVAILABLE);
            let recovery = answer(service.clone(), id, value, false).await;
            assert!(!recovery.ok);
            assert_eq!(
                recovery.error,
                zerus_client_core::repository::UNCONFIRMED_OPERATION
            );
        }
        assert!(calls(&f).is_empty(), "neither mutation nor lookup was sent");
        service.join().await.unwrap();
    });
}
#[test]
fn completed_original_child_with_held_pipes_keeps_the_creation_gate_charged() {
    if !private_case("desktop_service::creation_clone::tests::completed_original_child_with_held_pipes_keeps_the_creation_gate_charged") {return}
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({"clone_hold":true}));
        let (service, _cache) = connected(&f).await;
        let (id, value) = operation();
        let reader = tauri::async_runtime::spawn(answer(service.clone(), id, value, true));
        eventually("original clone held", || f.0.join("clone-held").exists()).await;
        assert!(matches!(
            service.shared.creation_rpc.admit(Role::Fleet),
            Err(zerus_client_core::desktop::drain::Refused::Capacity)
        ));
        assert!(!reader.await.unwrap().ok);
        assert!(
            matches!(
                service.shared.creation_rpc.admit(Role::Fleet),
                Err(zerus_client_core::desktop::drain::Refused::Capacity)
            ),
            "original child exit/runner failure is not its descendants' pipe EOF"
        );
        std::fs::write(f.0.join("clone-release"), b"").unwrap();
        eventually("original creation charge released", || {
            service.shared.creation_rpc.admit(Role::Fleet).is_ok()
        })
        .await;
        assert_eq!(calls(&f).len(), 1);
        service.join().await.unwrap();
    });
}

#[test]
fn original_connection_and_helper_are_rechecked_before_each_clone_read() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let f = fixture(json!({}));
        let (service, _) = f.start(true);
        eventually("clone reader connection", || service.snapshot().connected).await;
        let read = Read::new(Some(service.clone())).unwrap();
        assert!(read.current());
        let original = service
            .shared
            .cancellation
            .lock()
            .unwrap()
            .replace(Arc::new(AtomicBool::new(false)));
        assert!(
            !read.current(),
            "same epoch/generation with a different original connection is stale"
        );
        *service.shared.cancellation.lock().unwrap() = original;
        assert!(read.current());
        let path = f.0.join("hgs");
        let next = f.0.join("replacement");
        std::fs::copy(&path, &next).unwrap();
        std::fs::rename(next, &path).unwrap();
        assert!(read.exchange(None, Value::Null, true, None).is_err());
        assert!(calls(&f).is_empty());
        service.join().await.unwrap();
        assert!(!read.current());
    });
}
