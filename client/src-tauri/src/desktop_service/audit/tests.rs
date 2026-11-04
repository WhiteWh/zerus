use super::*;
use crate::desktop_service::tests::{eventually, Fixture};

fn replace_helper(fixture: &Fixture) {
    let path = fixture.0.join("hgs");
    let replacement = fixture.0.join("hgs-new");
    std::fs::copy(&path, &replacement).unwrap();
    std::fs::rename(replacement, path).unwrap();
}
fn help_calls(fixture: &Fixture) -> usize {
    fixture
        .calls()
        .iter()
        .filter(|call| call["args"] == json!(["--help"]))
        .count()
}
fn audit_calls(fixture: &Fixture) -> usize {
    fixture
        .calls()
        .iter()
        .filter(|call| {
            call["value"]["op"]
                .as_str()
                .is_some_and(|op| op.starts_with("approval_audit_"))
        })
        .count()
}

#[test]
fn abi_probe_reuses_only_the_original_connection_and_helper_image() {
    use std::io::Write;
    let _process = crate::PROCESS_TREE.read().unwrap();
    let fixture = Fixture::new(json!({"audit_first_watch_hold":true}));
    replace_helper(&fixture); // Never modify the shared source fixture's symlink target.
    let greeted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = greeted.clone();
    let pump = crate::pump::Pump::default();
    let (sink, rx) = std::sync::mpsc::sync_channel(256);
    let drain = pump.drain_service(rx, |_| {});
    let service = Service::start(
        fixture.0.join("hgs"),
        pump,
        sink,
        true,
        drain,
        None,
        vec![Arc::new(move |frame| {
            if frame.is_some_and(|frame| frame["t"] == "hello") {
                observed.fetch_add(1, Ordering::AcqRel);
            }
        })],
    );
    let checks = async {
        eventually("connected", || service.snapshot().connected).await;
        eventually("original watch startup held", || {
            fixture.0.join("audit-first-watch-held").exists()
        })
        .await;
        assert_eq!(greeted.load(Ordering::Acquire), 0);
        let original = Read::new(Some(service.clone())).unwrap();
        assert!(original.current());
        // Preflight has published connected, but only the original watch's
        // observed hello establishes the connection that Retry must replace.
        std::fs::write(fixture.0.join("audit-first-watch-release"), []).unwrap();
        eventually("original watch greeted", || {
            greeted.load(Ordering::Acquire) == 1 && original.current()
        })
        .await;
        let scope = first(service.clone()).await.unwrap().scope;
        detail(Some(service.clone()), 53, scope).await.unwrap();
        assert_eq!(
            help_calls(&fixture),
            2,
            "one connection probe plus one audit probe"
        );
        service.retry();
        assert!(!original.current());
        eventually("same-epoch new original watch", || {
            fixture
                .calls()
                .iter()
                .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                .count()
                == 2
                && greeted.load(Ordering::Acquire) == 2
                && service.snapshot().connected
        })
        .await;
        let replacement = Read::new(Some(service.clone())).unwrap();
        assert_eq!(original.epoch, replacement.epoch);
        assert_eq!(original.generation, replacement.generation);
        assert!(!Arc::ptr_eq(&original.stop, &replacement.stop));
        assert!(!Arc::ptr_eq(&original.routing, &replacement.routing));
        let scope = first(service.clone()).await.unwrap().scope;
        detail(Some(service.clone()), 53, scope).await.unwrap();
        assert_eq!(
            help_calls(&fixture),
            4,
            "same epoch still needs the new connection's probe"
        );
        replace_helper(&fixture);
        let scope = first(service.clone()).await.unwrap().scope;
        detail(Some(service.clone()), 53, scope).await.unwrap();
        assert_eq!(help_calls(&fixture), 5, "same pathname, new file identity");
        std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.0.join("hgs"))
            .unwrap()
            .write_all(b"\n# changed helper image\n")
            .unwrap();
        let scope = first(service.clone()).await.unwrap().scope;
        detail(Some(service.clone()), 53, scope).await.unwrap();
        assert_eq!(help_calls(&fixture), 6, "in-place version change");
        let queries = audit_calls(&fixture);
        std::fs::write(
            fixture.0.join("fixture.json"),
            json!({"audit_abi":false}).to_string(),
        )
        .unwrap();
        replace_helper(&fixture);
        assert!(first(service.clone()).await.is_err());
        assert!(first(service.clone()).await.is_err());
        assert_eq!(
            help_calls(&fixture),
            8,
            "unsupported probes are never positive cached"
        );
        assert_eq!(audit_calls(&fixture), queries);
        assert!(service.snapshot().connected);
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tauri::async_runtime::block_on(checks)
    }));
    // Even a failed fixture assertion must join its original watch and helpers.
    tauri::async_runtime::block_on(service.join()).unwrap();
    assert!(service.shared.audit_help.is_empty());
    eprintln!(
        "audit fixture joined: {} logged watches, {} greetings",
        fixture
            .calls()
            .iter()
            .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
            .count(),
        greeted.load(Ordering::Acquire)
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn helper_replacement_during_probe_or_cached_query_refuses_stale_results() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        for during_probe in [true, false] {
            let fixture = Fixture::new(json!({"audit_help_hold":during_probe}));
            replace_helper(&fixture);
            let (service, _) = fixture.start(true);
            eventually("connected", || service.snapshot().connected).await;
            let scope = if during_probe {
                None
            } else {
                let page = first(service.clone()).await.unwrap();
                std::fs::write(
                    fixture.0.join("fixture.json"),
                    json!({"audit_hold":true}).to_string(),
                )
                .unwrap();
                page.scope
            };
            let shared = service.clone();
            let request = tauri::async_runtime::spawn(async move {
                if during_probe {
                    first(shared).await.map(|_| ())
                } else {
                    detail(Some(shared), 53, scope).await.map(|_| ())
                }
            });
            eventually("helper call held", || {
                fixture
                    .0
                    .join(if during_probe {
                        "audit-help-held"
                    } else {
                        "audit-held"
                    })
                    .exists()
            })
            .await;
            replace_helper(&fixture);
            std::fs::write(
                fixture.0.join(if during_probe {
                    "audit-help-release"
                } else {
                    "audit-release"
                }),
                [],
            )
            .unwrap();
            assert!(
                request.await.unwrap().is_err(),
                "an old helper result is not adopted"
            );
            if during_probe {
                assert_eq!(audit_calls(&fixture), 0);
            } else {
                assert_eq!(help_calls(&fixture), 2, "cached query does not spawn help");
            }
            assert!(service.snapshot().connected);
            service.join().await.unwrap();
        }
    });
}

async fn first(service: Arc<Service>) -> Result<Page, String> {
    list(
        Some(service),
        " a.example ".into(),
        "agent/project/fixture".into(),
        0,
        0,
        None,
    )
    .await
}

#[test]
fn settings_reads_service_pages_and_details_without_ready_writers() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"ready":false}));
        let (service, _) = fixture.start(true);
        eventually("connected passive read", || service.snapshot().connected).await;
        let page = first(service.clone()).await.unwrap();
        assert_eq!(page.page.entries.len(), 50);
        assert_eq!(page.page.entries[0].id, 53);
        assert_eq!(page.page.entries[0].prompt, "Service-owned question");
        assert_eq!(page.page.entries[0].result, "Approved once");
        assert_eq!(
            serde_json::to_value(&page).unwrap()["entries"][0]["host"],
            "a.example"
        );
        let scope = page.scope.unwrap();
        assert_eq!(scope.through, 53);
        let text = detail(Some(service.clone()), 53, Some(scope.clone()))
            .await
            .unwrap();
        assert!(text.contains("Service-owned question"));
        assert!(text.contains("Service audit outcome"));
        let next = list(
            Some(service.clone()),
            scope.host.clone(),
            scope.session.clone(),
            page.page.next,
            0,
            Some(scope.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            next.page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(next.scope.as_ref(), Some(&scope));
        let newer = list(
            Some(service.clone()),
            scope.host.clone(),
            scope.session.clone(),
            0,
            next.page.previous,
            next.scope,
        )
        .await
        .unwrap();
        assert_eq!(newer.page.entries[0].id, 53);
        service.join().await.unwrap();
        let calls = fixture.calls();
        let queries: Vec<_> = calls
            .iter()
            .filter(|c| {
                c["value"]["op"]
                    .as_str()
                    .is_some_and(|op| op.starts_with("approval_audit_"))
            })
            .collect();
        assert_eq!(queries.len(), 4);
        assert!(queries
            .iter()
            .all(|call| call["args"] == json!(["desktop", "request", "--json"])));
        assert_eq!(queries[0]["value"]["journal"], Value::Null);
        assert_eq!(queries[2]["value"]["journal"], scope.journal);
        assert_eq!(queries[2]["value"]["through"], 53);
        assert!(calls
            .iter()
            .all(|call| call["args"][0] == "desktop" || call["args"] == json!(["--help"])));
    });
}

#[test]
fn old_page_never_adopts_a_new_scope_filter_or_journal() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({}));
        let (service, _) = fixture.start(true);
        eventually("connected", || service.snapshot().connected).await;
        let scope = first(service.clone()).await.unwrap().scope.unwrap();
        for changed in ["configuration", "epoch", "generation", "through"] {
            let mut wrong = scope.clone();
            match changed {
                "configuration" => wrong.configuration = "b".repeat(64),
                "epoch" => wrong.epoch = uuid::Uuid::new_v4().to_string(),
                "generation" => wrong.generation += 1,
                _ => wrong.through = SAFE_INTEGER + 1,
            }
            assert_eq!(
                detail(Some(service.clone()), 1, Some(wrong))
                    .await
                    .unwrap_err(),
                CHANGED
            );
        }
        assert!(list(
            Some(service.clone()),
            "other".into(),
            scope.session.clone(),
            4,
            0,
            Some(scope.clone())
        )
        .await
        .is_err());
        assert!(list(
            Some(service.clone()),
            scope.host.clone(),
            scope.session.clone(),
            4,
            0,
            None
        )
        .await
        .is_err());
        assert!(detail(Some(service.clone()), 54, Some(scope.clone()))
            .await
            .is_err());
        let count = fixture.calls().len();
        std::fs::write(fixture.0.join("audit-replaced"), []).unwrap();
        assert_eq!(
            detail(Some(service.clone()), 1, Some(scope.clone()))
                .await
                .unwrap_err(),
            CHANGED
        );
        let replacement = first(service.clone()).await.unwrap().scope.unwrap();
        assert_ne!(replacement.journal, scope.journal);
        assert!(fixture.calls().len() > count);
        std::fs::write(
            fixture.0.join("fixture.json"),
            json!({"audit_fault":"refused"}).to_string(),
        )
        .unwrap();
        assert_eq!(
            detail(Some(service.clone()), 1, Some(replacement))
                .await
                .unwrap_err(),
            UNAVAILABLE
        );
        service.retry();
        eventually("replacement watch", || {
            fixture
                .calls()
                .iter()
                .filter(|call| call["args"] == json!(["desktop", "watch", "--json"]))
                .count()
                == 2
        })
        .await;
        // Even an equal epoch/generation cannot make a queued old call current.
        let read = Read::new(Some(service.clone())).unwrap();
        service.retry();
        assert!(!read.current());
        service.join().await.unwrap();
    });
}

#[test]
fn incompatible_or_invalid_audit_replies_never_fall_back_or_disconnect_catalogs() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        assert!(list(None, "".into(), "".into(), 0, 0, None).await.is_err());
        for fault in [
            "request_id",
            "configuration",
            "service_epoch",
            "journal",
            "oversize",
            "rows",
            "refused",
            "missing_abi",
        ] {
            let fixture =
                Fixture::new(json!({"audit_fault":fault,"audit_abi":fault != "missing_abi"}));
            let (service, _) = fixture.start(true);
            eventually("connected", || service.snapshot().connected).await;
            assert!(first(service.clone()).await.is_err(), "{fault}");
            assert!(service.snapshot().connected, "a read refusal stays local");
            service.join().await.unwrap();
            let calls = fixture.calls();
            assert!(calls
                .iter()
                .all(|call| call["args"][0] == "desktop" || call["args"] == json!(["--help"])));
            if fault == "missing_abi" {
                assert!(!calls
                    .iter()
                    .any(|call| call["value"]["op"] == "approval_audit_list"));
            }
        }
    });
}
#[test]
#[ignore = "run client/scripts/test-service-approval-audit.py with a privately built hgs"]
fn real_service_owned_journal_reaches_the_settings_read_path() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let relay =
            std::env::var_os("ZERUS_TEST_DESKTOP_AUDIT_RELAY").expect("private harness relay");
        let pump = crate::pump::Pump::default();
        let (sink, rx) = std::sync::mpsc::sync_channel(256);
        let drain = pump.drain_service(rx, |_| {});
        let service = Service::start(relay.into(), pump, sink, true, drain, None, vec![]);
        eventually("real private service connected", || {
            service.snapshot().connected
        })
        .await;
        let page = first(service.clone()).await.unwrap();
        assert_eq!(page.page.entries.len(), 50);
        assert_eq!(page.page.entries[0].id, 53);
        assert_eq!(page.page.entries[0].prompt, "Service-owned question");
        assert_eq!(page.page.entries[0].result, "Approved once");
        let scope = page.scope.unwrap();
        let body = detail(Some(service.clone()), 53, Some(scope.clone()))
            .await
            .unwrap();
        assert!(body.contains("Service audit outcome"));
        assert!(body.contains("Service-owned question"));
        let older = list(
            Some(service.clone()),
            scope.host.clone(),
            scope.session.clone(),
            page.page.next,
            0,
            Some(scope.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            older.page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(older.scope, Some(scope));
        service.join().await.unwrap();
    });
}

#[test]
fn reply_from_an_interrupted_connection_is_not_displayed() {
    let _process = crate::PROCESS_TREE.read().unwrap();
    tauri::async_runtime::block_on(async {
        let fixture = Fixture::new(json!({"audit_hold":true}));
        let (service, _) = fixture.start(true);
        eventually("connected", || service.snapshot().connected).await;
        let request = tauri::async_runtime::spawn(first(service.clone()));
        eventually("query held", || fixture.0.join("audit-held").exists()).await;
        service.retry();
        std::fs::write(fixture.0.join("audit-release"), []).unwrap();
        assert!(request.await.unwrap().is_err());
        service.join().await.unwrap();
    });
}
