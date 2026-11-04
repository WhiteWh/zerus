use super::*;
use crate::desktop::{
    notifications::Outbox,
    store::{Location, Store},
};
use serde_json::json;
use std::{cell::Cell, time::Instant};
use wire::Binding;
use zerus_desktop_ledger::{self as ledger, Identity, Kind, Reserve, Target};

pub(super) fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(super) fn session() -> Session {
    Session {
        configuration: "a".repeat(64),
        service_epoch: uuid(),
        writer_epoch: uuid(),
        adapter: uuid(),
        host_epoch: uuid(),
        lease: uuid(),
        platform: Platform::Windows,
    }
}
pub(super) fn token() -> String {
    zerus_client_core::attention::encode_token(
        json!({
            "host":"peer.example", "run":"fixture-run", "conversation":"fixture-conversation",
            "name":"fixture", "notice":uuid()
        })
        .as_object()
        .unwrap(),
    )
}
pub(super) fn post(session: &Session, seq: i64) -> Command {
    Command {
        binding: Binding {
            session: session.clone(),
            request_id: uuid(),
            intent_id: "b".repeat(64),
            revision: 1,
            seq,
        },
        action: Action::Post {
            token: token(),
            title: "Needs attention".into(),
            body: "Fixture".into(),
        },
    }
}
pub(super) fn activation(command: &Command) -> Activation {
    Activation {
        request_id: command.binding.request_id.clone(),
        token: command.action.token().into(),
        native_id: identifier(command),
        platform_context: None,
    }
}
fn ready() -> Mappings {
    let mut result = Mappings::new(session()).unwrap();
    assert!(result.finish_restoration());
    result
}
#[derive(Default)]
struct Fake {
    preparations: usize,
    posts: usize,
    withdrawals: usize,
    refusal: Option<Refusal>,
    error: bool,
    wrong_id: bool,
}
impl Native for Fake {
    fn close(&mut self) -> bool {
        true
    }
    fn prepare(&mut self, _: &Command, _: &NativeId) -> Result<(), Refusal> {
        self.preparations += 1;
        self.refusal.map_or(Ok(()), Err)
    }
    fn post(&mut self, _: &Command, id: &NativeId) -> Result<NativeId, ()> {
        self.posts += 1;
        if self.error {
            return Err(());
        }
        if self.wrong_id {
            return Ok(NativeId::Windows {
                tag: "wrong".into(),
                group: "wrong".into(),
            });
        }
        Ok(id.clone())
    }
    fn withdraw(&mut self, _: &NativeId) -> Result<(), ()> {
        self.withdrawals += 1;
        if self.error {
            Err(())
        } else {
            Ok(())
        }
    }
}
#[test]
fn restoration_is_read_only_and_admission_is_once_only() {
    let scope = session();
    let command = post(&scope, 1);
    let mut model = Mappings::new(scope).unwrap();
    let value = activation(&command);
    let mut native = Fake::default();
    assert!(model.restore(value.clone()));
    assert!(model.restore(value.clone()));
    assert!(model.clicked(&value.request_id).is_none());
    assert_eq!(
        model.execute(&command, &mut native, || None),
        Outcome::Refused {
            code: Refusal::NotReady
        }
    );
    assert_eq!(
        (native.preparations, native.posts, native.withdrawals),
        (0, 0, 0)
    );
    assert!(model.finish_restoration());
    assert!(!model.finish_restoration());
    assert!(!model.restore(value.clone()));
    assert_eq!(model.clicked(&value.request_id), Some(value));
}
#[test]
fn changed_request_or_colliding_native_identity_cannot_replace_a_mapping() {
    let scope = session();
    let command = post(&scope, 1);
    let mut model = Mappings::new(scope).unwrap();
    let value = activation(&command);
    assert!(model.restore(value.clone()));
    let mut changed = value.clone();
    changed.token = token();
    assert!(!model.restore(changed));
    let mut changed = value.clone();
    changed.request_id = uuid();
    assert!(!model.restore(changed));
    assert!(model.finish_restoration());
    assert_eq!(model.clicked(&value.request_id), Some(value));
}
#[test]
fn restore_refuses_unsupported_native_lengths_without_truncation() {
    let scope = session();
    let command = post(&scope, 1);
    let mut model = Mappings::new(scope).unwrap();
    let mut value = activation(&command);
    value.native_id = NativeId::Windows {
        tag: "x".repeat(17),
        group: "y".into(),
    };
    assert!(!model.restore(value));
    assert!(model.delivered.is_empty());
}
#[test]
fn exact_session_and_forward_sequence_are_required_before_native_preparation() {
    let mut model = ready();
    let mut native = Fake::default();
    let mut command = post(&model.session, 1);
    command.binding.session.host_epoch = uuid();
    assert_eq!(
        model.execute(&command, &mut native, || None),
        Outcome::Refused {
            code: Refusal::InvalidCommand
        }
    );
    assert_eq!(native.preparations, 0);
    command.binding.session = model.session.clone();
    assert!(matches!(
        model.execute(&command, &mut native, || None),
        Outcome::Delivered { .. }
    ));
    assert_eq!(
        model.execute(&command, &mut native, || None),
        Outcome::Refused {
            code: Refusal::InvalidCommand
        }
    );
    assert_eq!(native.posts, 1);
}
#[test]
fn missing_native_registration_refuses_before_any_show() {
    let mut model = ready();
    let command = post(&model.session, 1);
    let mut native = Fake {
        refusal: Some(Refusal::NotReady),
        ..Default::default()
    };
    assert_eq!(
        model.execute(&command, &mut native, || None),
        Outcome::Refused {
            code: Refusal::NotReady
        }
    );
    assert_eq!(native.posts, 0);
    assert!(model.clicked(&command.binding.request_id).is_none());
}
#[test]
fn deadline_crossing_in_preparation_is_still_before_the_effect() {
    let mut model = ready();
    let command = post(&model.session, 1);
    let mut native = Fake::default();
    let checks = Cell::new(0);
    assert_eq!(
        model.execute(&command, &mut native, || {
            checks.set(checks.get() + 1);
            (checks.get() >= 2).then_some(Uncertainty::Deadline)
        }),
        Outcome::Refused {
            code: Refusal::Cancelled
        }
    );
    assert_eq!(native.posts, 0);
}
#[test]
fn deadline_crossing_during_native_show_is_unknown_and_never_a_delivered_callback() {
    let mut model = ready();
    let command = post(&model.session, 1);
    let mut native = Fake::default();
    let checks = Cell::new(0);
    assert_eq!(
        model.execute(&command, &mut native, || {
            checks.set(checks.get() + 1);
            (checks.get() >= 3).then_some(Uncertainty::Deadline)
        }),
        Outcome::Unknown {
            code: Uncertainty::Deadline
        }
    );
    assert_eq!(native.posts, 1);
    assert!(model.clicked(&command.binding.request_id).is_none());
    assert_eq!(
        model.execute(&command, &mut native, || None),
        Outcome::Refused {
            code: Refusal::InvalidCommand
        }
    );
    assert_eq!(native.posts, 1);
}
#[test]
fn native_api_error_and_wrong_native_result_are_unknown_without_fabricated_mapping() {
    for native in [
        Fake {
            error: true,
            ..Default::default()
        },
        Fake {
            wrong_id: true,
            ..Default::default()
        },
    ] {
        let mut native = native;
        let mut model = ready();
        let command = post(&model.session, 1);
        assert!(matches!(
            model.execute(&command, &mut native, || None),
            Outcome::Unknown { .. }
        ));
        assert!(model.clicked(&command.binding.request_id).is_none());
        assert_eq!(native.posts, 1);
    }
}
#[test]
fn callback_requires_a_known_delivery_and_stop_closes_clicks_before_cleanup() {
    let mut model = ready();
    let mut native = Fake::default();
    let command = post(&model.session, 1);
    assert!(model.clicked(&command.binding.request_id).is_none());
    assert!(matches!(
        model.execute(&command, &mut native, || None),
        Outcome::Delivered { .. }
    ));
    assert_eq!(
        model.clicked(&command.binding.request_id),
        Some(activation(&command))
    );
    model.request_stop();
    assert!(model.clicked(&command.binding.request_id).is_none());
    assert_eq!(
        model.closed(&command.binding.request_id),
        Some(activation(&command))
    );
    assert!(model.closed(&command.binding.request_id).is_none());
    let next = post(&model.session, 2);
    assert_eq!(
        model.execute(&next, &mut native, || None),
        Outcome::Refused {
            code: Refusal::NotReady
        }
    );
    assert_eq!(native.posts, 1);
}
#[test]
fn withdraw_joins_original_token_and_native_identity() {
    let mut model = ready();
    let mut native = Fake::default();
    let original = post(&model.session, 1);
    assert!(matches!(
        model.execute(&original, &mut native, || None),
        Outcome::Delivered { .. }
    ));
    let mut withdraw = post(&model.session, 2);
    withdraw.action = Action::Withdraw {
        token: token(),
        native_id: identifier(&original),
    };
    assert_eq!(
        model.execute(&withdraw, &mut native, || None),
        Outcome::Refused {
            code: Refusal::NotReady
        }
    );
    assert_eq!(native.withdrawals, 0);
    withdraw.binding.seq = 3;
    withdraw.action = Action::Withdraw {
        token: original.action.token().into(),
        native_id: identifier(&original),
    };
    assert_eq!(
        model.execute(&withdraw, &mut native, || None),
        Outcome::Delivered {
            native_id: identifier(&original)
        }
    );
    assert_eq!(native.withdrawals, 1);
    assert!(model.clicked(&original.binding.request_id).is_none());
}
#[test]
fn unknown_withdrawal_keeps_original_mapping_until_a_real_close() {
    let mut model = ready();
    let mut native = Fake::default();
    let original = post(&model.session, 1);
    model.execute(&original, &mut native, || None);
    let mut withdraw = post(&model.session, 2);
    withdraw.action = Action::Withdraw {
        token: original.action.token().into(),
        native_id: identifier(&original),
    };
    native.error = true;
    assert_eq!(
        model.execute(&withdraw, &mut native, || None),
        Outcome::Unknown {
            code: Uncertainty::NativeError
        }
    );
    assert_eq!(
        model.clicked(&original.binding.request_id),
        Some(activation(&original))
    );
}
#[test]
fn original_actor_dispatch_settles_only_its_durable_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let scope = session();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: scope.configuration.clone(),
    };
    location.prepare().unwrap();
    let mut store = Store::open(&location, &scope.service_epoch).unwrap();
    ledger::authorize(&mut store.db, &scope.writer_epoch, &scope.service_epoch).unwrap();
    let (mut actor, endpoint) = Outbox::new(scope.clone()).unwrap();
    let command = post(&scope, 1);
    let intent = ledger::put_intent(
        &mut store.db,
        &Identity {
            kind: Kind::Post,
            target: Target::Session {
                computer: "peer.example".into(),
                run: "fixture-run".into(),
                conversation: "fixture-conversation".into(),
            },
            edge: uuid(),
        },
        1,
        &serde_json::to_value(&command.action).unwrap(),
    )
    .unwrap();
    let request = Reserve {
        request_id: command.binding.request_id.clone(),
        intent_id: intent,
        revision: 1,
        epoch: scope.writer_epoch.clone(),
        service_epoch: scope.service_epoch.clone(),
        adapter: scope.adapter.clone(),
        lease: scope.lease.clone(),
        retry_of: None,
    };
    actor
        .offer(&mut store, request.clone(), Instant::now())
        .unwrap();
    let mut model = Mappings::new(scope).unwrap();
    model.finish_restoration();
    let mut native = Fake::default();
    model.dispatch(
        endpoint.try_next().unwrap().begin().unwrap(),
        &mut native,
        &AtomicBool::new(false),
    );
    actor.poll(&mut store, Instant::now());
    let state: String = store
        .db
        .query_row(
            "SELECT state FROM desktop_attempts WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "delivered");
    assert_eq!(native.posts, 1);
    assert!(!actor.operation_drained());
    actor.offer(&mut store, request, Instant::now()).unwrap();
    assert!(endpoint.try_next().is_err());
    assert_eq!(native.posts, 1);
    actor.request_stop();
    actor.poll(&mut store, Instant::now());
    assert!(actor.operation_drained());
}
