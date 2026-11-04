use super::*;
use serde_json::json;
use zerus_client_core::{Change, ComputerId, ComputerStatus};

fn request() -> Request {
    Request {
        nonce: "01234567-89ab-4cde-8fed-0123456789ab".into(),
        catalog_key: "run:fixture".into(),
        target: Target {
            computer: "build-01".into(),
            name: "dsh/sample/migrate".into(),
            run_id: "fixture".into(),
            conversation_id: "conversation-fixture".into(),
        },
    }
}

fn row() -> ViewRow {
    let request = request();
    let mut row = ViewRow {
        name: request.target.name,
        run_id: Some(request.target.run_id),
        conversation_id: Some(request.target.conversation_id),
        state: "running".into(),
        ..ViewRow::default()
    };
    row.notify.cmd = "dsh".into();
    row
}

#[test]
fn exact_original_catalog_identity_is_required_without_name_fallback() {
    let request = request();
    let pump = crate::pump::Pump::default();
    let publish = |key: &str, row: ViewRow| {
        pump.push(Change::Replace {
            computer: ComputerId::from("build-01"),
            rows: vec![(key.into(), row)],
        })
    };
    publish(&request.catalog_key, row());
    assert!(available(&pump, &request).is_ok());
    for field in ["name", "run", "conversation", "provider", "archive"] {
        let mut changed = row();
        match field {
            "name" => changed.name.push_str("-renamed"),
            "run" => changed.run_id = Some("new-run".into()),
            "conversation" => changed.conversation_id = Some("other".into()),
            "provider" => changed.notify.cmd = "claude".into(),
            "archive" => changed.state = "archived".into(),
            _ => unreachable!(),
        }
        publish(&request.catalog_key, changed);
        assert_eq!(available(&pump, &request).unwrap_err(), CHANGED, "{field}");
    }
    publish("another-key", row());
    assert_eq!(available(&pump, &request).unwrap_err(), CHANGED);
    publish(&request.catalog_key, row());
    pump.push(Change::Computer {
        computer: ComputerId::from("build-01"),
        status: ComputerStatus::Ended,
    });
    assert_eq!(available(&pump, &request).unwrap_err(), OFFLINE);
}

#[test]
fn requests_reject_malformed_nonce_alias_and_unbounded_or_controlled_identity() {
    assert!(request().valid());
    for field in ["nonce", "alias", "empty", "long", "control"] {
        let mut value = request();
        match field {
            "nonce" => value.nonce = "not-a-uuid".into(),
            "alias" => value.target.computer = "@other".into(),
            "empty" => value.target.run_id.clear(),
            "long" => value.target.conversation_id = "a".repeat(1025),
            "control" => value.target.name.push('\n'),
            _ => unreachable!(),
        }
        assert!(!value.valid(), "{field}");
    }
}

#[test]
fn existing_cli_contract_and_budgets_never_introduce_bridge_restart_or_reload() {
    for browser in [false, true] {
        let mut request = request();
        let peer = invocation(&request, browser);
        let mut expected = vec!["@build-01", "native-ui", "dsh/sample/migrate"];
        if !browser {
            expected.push("--json");
        }
        assert_eq!(peer.args, expected);
        assert_eq!(peer.timeout, Duration::from_secs(60));
        assert!(peer.stdin.is_none());
        assert!(peer.request_id.is_none());
        request.target.computer.clear();
        let local = invocation(&request, browser);
        assert_eq!(local.timeout, Duration::from_secs(45));
        assert_eq!(local.args[0], "native-ui");
    }
}

#[test]
fn private_host_errors_and_launch_tokens_never_become_status_text() {
    let exit = |code, bytes| Exit::Code {
        code,
        stdout: bytes,
        stderr: "private-token-secret".into(),
    };
    assert_eq!(parse(exit(1, Vec::new())).unwrap_err(), OPEN_FAILED);
    assert_eq!(
        parse(exit(0, b"private-token-secret".to_vec())).unwrap_err(),
        INVALID
    );
    assert_eq!(
        parse(exit(
            0,
            serde_json::to_vec(
                &json!({"url":"https://example.invalid/?token=private-token-secret"})
            )
            .unwrap()
        ))
        .unwrap_err(),
        INVALID_ADDRESS
    );
    assert!(parse(exit(
        0,
        br#"{"url":"http://127.0.0.1:43210/?token=fixture"}"#.to_vec()
    ))
    .is_ok());
}

#[test]
fn renderer_bounds_require_finite_positive_dimensions_inside_the_bounded_window() {
    let good = Bounds {
        nonce: request().nonce,
        x: 10.0,
        y: 10.0,
        width: 500.0,
        height: 300.0,
    };
    assert!(good.valid());
    for bad in [
        Bounds {
            x: -1.0,
            ..good.clone()
        },
        Bounds {
            width: 0.0,
            ..good.clone()
        },
        Bounds {
            height: f64::INFINITY,
            ..good.clone()
        },
        Bounds {
            x: 32767.0,
            ..good.clone()
        },
        Bounds {
            y: f64::NAN,
            ..good.clone()
        },
    ] {
        assert!(!bad.valid());
    }
}

#[test]
fn retirement_invalidates_the_exact_generation_and_shutdown_cancels_only_its_lookup() {
    let ui = NativeUi::default();
    let request = request();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut inner = lock(&ui.inner);
        inner.request = Some(request.clone());
        inner.generation = 8;
        inner.cancel = Some(cancel.clone());
    }
    assert!(ui.current(&request, 8));
    assert!(!ui.current(&request, 7));
    let mut other = request.clone();
    other.nonce = uuid::Uuid::new_v4().to_string();
    assert!(!ui.current(&other, 8));
    ui.shutdown();
    assert!(cancel.load(Ordering::Relaxed));
    assert!(!ui.current(&request, 8));
    assert!(ui.status().is_none());
    assert!(!ui.busy.load(Ordering::Relaxed));
}
