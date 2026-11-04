use super::*;
use crate::desktop::{ui_requests::Envelope, Build, Responder};
use serde_json::json;

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn request(action: Action) -> Request {
    Request::DesktopOwnership {
        request_id: id(1),
        action,
        journal: Some(id(2)),
        revision: 7,
    }
}
fn receipt(action: Action) -> Receipt {
    let request = request(action);
    Receipt {
        abi: ABI.into(),
        request_id: id(1),
        configuration: "a".repeat(64),
        journal: Some(id(2)),
        fingerprint: request.fingerprint(),
        action,
        revision: 8,
        accepted: true,
        durable: true,
        outcome: Outcome::Pending,
        current: Status {
            abi: ABI.into(),
            configuration: "a".repeat(64),
            journal: Some(id(2)),
            revision: 8,
            state: State::Draining,
            rearm_required: false,
            request_id: Some(id(1)),
            owner_epoch: Some(id(12)),
            identity_current: true,
        },
    }
}
fn responder() -> Responder {
    Responder::new(
        "com.hgdev.hgs-tray".into(),
        Build {
            version: "0.48.0-dev.1".into(),
            source_commit: "a".repeat(40),
            source_dirty: Some(false),
        },
        id(4),
        true,
        false,
    )
}

#[test]
fn closed_intents_distinguish_startup_rearm_and_takeover_without_mode_claims() {
    for action in [Action::Grant, Action::Rearm, Action::NativeTakeover] {
        let request = request(action);
        assert!(Envelope {
            abi: crate::desktop::ui_requests::ABI.into(),
            attempt: id(3),
            request: request.clone(),
        }
        .valid());
        assert_eq!(request.operation(), "desktop_ownership");
        for field in [
            "mode",
            "ownership_mode",
            "confirmed",
            "role",
            "frontend",
            "build",
        ] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = json!("service");
            assert!(serde_json::from_value::<Request>(value).is_err());
        }
    }
    assert_ne!(
        request(Action::Grant).fingerprint(),
        request(Action::Rearm).fingerprint()
    );
    let mut value = serde_json::to_value(request(Action::Grant)).unwrap();
    value["journal"] = json!("invalid");
    assert!(!Envelope {
        abi: crate::desktop::ui_requests::ABI.into(),
        attempt: id(3),
        request: serde_json::from_value(value).unwrap()
    }
    .valid());
}

#[test]
fn native_mode_is_once_per_instance_and_missing_or_wrong_mode_cannot_register() {
    const CASE: &str = "desktop::ownership::tests::native_mode_is_once_per_instance_and_missing_or_wrong_mode_cannot_register";
    const MARKER: &str = "ZERUS_OWNERSHIP_MODE_TEST";
    if std::env::var(MARKER).as_deref() != Ok(CASE) {
        let invocation = crate::Invocation {
            args: vec![
                "--exact".into(),
                CASE.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            stdin: None,
            request_id: None,
            timeout: std::time::Duration::from_secs(30),
        };
        let result = crate::run::run_without(
            &std::env::current_exe().unwrap(),
            &invocation,
            &[(MARKER, CASE)],
            &["HGS_SESSION", "HGS_RUN_ID"],
            64 << 10,
        )
        .unwrap();
        assert!(
            matches!(&result, crate::Exit::Code { code: 0, .. }),
            "isolated native mode case: {result:?}"
        );
        let crate::Exit::Code { stdout, .. } = result else {
            unreachable!()
        };
        assert!(String::from_utf8_lossy(&stdout)
            .contains("test result: ok. 1 passed; 0 failed; 0 ignored;"));
        return;
    }
    let responder = responder();
    assert!(responder.configure_once("a".repeat(64)));
    assert!(responder
        .prepare_ui_request(id(5), request(Action::Grant))
        .is_err());
    assert!(responder.configure_ownership_mode_once(Mode::Native));
    assert!(!responder.configure_ownership_mode_once(Mode::Service));
    assert!(responder
        .prepare_ui_request(id(6), request(Action::Grant))
        .is_err());
    assert!(responder
        .prepare_ui_request(id(7), request(Action::Rearm))
        .is_err());
    assert!(responder
        .prepare_ui_request(id(8), request(Action::NativeTakeover))
        .is_ok());
    let service = self::responder();
    assert!(service.configure_once("a".repeat(64)));
    assert!(service.configure_ownership_mode_once(Mode::Service));
    assert!(service
        .prepare_ui_request(id(9), request(Action::Grant))
        .is_ok());
    assert!(service
        .prepare_ui_request(id(10), request(Action::Rearm))
        .is_ok());
    assert!(service
        .prepare_ui_request(id(11), request(Action::NativeTakeover))
        .is_err());
}

#[test]
fn only_the_exact_settled_takeover_receipt_can_release_native_posting() {
    let request = request(Action::NativeTakeover);
    let mut value = receipt(Action::NativeTakeover);
    assert!(value.matches(&request, &"a".repeat(64)));
    assert!(!value.native_ready());
    value.outcome = Outcome::NativeReady {
        prior_owner_epoch: Some(id(12)),
        revoked: true,
        checkpointed: true,
        drained: true,
    };
    value.current.state = State::Native;
    assert!(value.matches(&request, &"a".repeat(64)));
    assert!(value.native_ready());
    let mut changed = value.clone();
    changed.current.revision += 1;
    changed.current.state = State::Service;
    changed.current.request_id = Some(id(16));
    assert!(
        changed.matches(&request, &"a".repeat(64)),
        "historical result is still readable"
    );
    assert!(
        !changed.native_ready(),
        "an old receipt cannot authorize a new takeover"
    );
    assert!(!value.matches(&request, &"b".repeat(64)));
    value.fingerprint = "b".repeat(64);
    assert!(!value.matches(&request, &"a".repeat(64)));
    for (revoked, checkpointed, drained) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        value.outcome = Outcome::NativeReady {
            prior_owner_epoch: None,
            revoked,
            checkpointed,
            drained,
        };
        assert!(!value.valid());
        assert!(!value.native_ready());
    }
    let mut granted = receipt(Action::Grant);
    granted.outcome = Outcome::Granted {
        owner_epoch: id(13),
    };
    assert!(granted.valid());
    assert!(!granted.native_ready());
}

#[test]
fn lookup_preserves_original_identity_and_missing_is_not_an_implicit_negative() {
    let query = Query::DesktopOwnershipResult {
        request_id: id(14),
        configuration: "a".repeat(64),
        original_request_id: id(1),
        fingerprint: request(Action::Grant).fingerprint(),
        journal: Some(id(2)),
        revision: 7,
    };
    let mut reply = Lookup {
        abi: ABI.into(),
        request_id: id(14),
        configuration: "a".repeat(64),
        found: false,
        definitive: false,
        receipt: None,
    };
    assert!(reply.matches(&query));
    assert!(!reply.definitive);
    reply.found = true;
    assert!(!reply.matches(&query));
    reply.receipt = Some(receipt(Action::Grant));
    assert!(reply.matches(&query));
    reply.receipt.as_mut().unwrap().request_id = id(15);
    assert!(!reply.matches(&query));
    reply.receipt.as_mut().unwrap().request_id = id(1);
    reply.definitive = true;
    assert!(!reply.matches(&query));
}

#[test]
fn only_an_explicit_authenticated_absent_takeover_receipt_can_settle_the_noop() {
    let request = Request::DesktopOwnership {
        request_id: id(20),
        action: Action::NativeTakeover,
        journal: None,
        revision: 0,
    };
    let mut value = receipt(Action::NativeTakeover);
    value.request_id = id(20);
    value.fingerprint = request.fingerprint();
    value.journal = None;
    value.revision = 0;
    value.durable = false;
    value.outcome = Outcome::NativeAbsent;
    value.current.journal = None;
    value.current.revision = 0;
    value.current.owner_epoch = None;
    value.current.state = State::Native;
    value.current.request_id = Some(id(20));
    assert!(value.matches(&request, &"a".repeat(64)));
    assert!(value.native_ready());
    let query = Query::for_request(id(21), "a".repeat(64), &request).unwrap();
    let lookup = Lookup {
        abi: ABI.into(),
        request_id: id(21),
        configuration: "a".repeat(64),
        found: true,
        definitive: false,
        receipt: Some(value.clone()),
    };
    assert!(
        !lookup.matches(&query),
        "a read-only lookup cannot manufacture an unstored no-op permission"
    );
    value.durable = true;
    assert!(!value.valid());
    value.durable = false;
    value.current.state = State::Service;
    assert!(!value.native_ready());
    value.current.state = State::Native;
    value.current.journal = Some(id(2));
    assert!(!value.native_ready());
}
