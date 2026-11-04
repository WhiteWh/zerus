use super::wire::{NativeId, Platform};
use super::*;
use crate::desktop::store::Location;
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    outbox: Outbox,
    host: Endpoint,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = id();
        let mut store = Store::open(&location, &epoch).unwrap();
        let writer = id();
        ledger::authorize(&mut store.db, &writer, &epoch).unwrap();
        let (outbox, host) = Outbox::new(Session {
            configuration: location.configuration,
            service_epoch: epoch,
            writer_epoch: writer,
            adapter: id(),
            host_epoch: id(),
            lease: id(),
            platform: Platform::Macos,
        })
        .unwrap();
        Self {
            _temp: temp,
            store,
            outbox,
            host,
        }
    }
    fn intent(&mut self) -> String {
        let token = attention::encode_token(
            json!({"host":"peer.example","run":"run-1",
            "conversation":"conversation-1","name":"fixture","notice":id()})
            .as_object()
            .unwrap(),
        );
        ledger::put_intent(
            &mut self.store.db,
            &Identity {
                kind: Kind::Post,
                target: Target::Session {
                    computer: "peer.example".into(),
                    run: "run-1".into(),
                    conversation: "conversation-1".into(),
                },
                edge: id(),
            },
            1,
            &serde_json::to_value(Action::Post {
                token,
                title: "Needs attention".into(),
                body: "Example".into(),
            })
            .unwrap(),
        )
        .unwrap()
    }
    fn request(&self, intent: &str) -> Reserve {
        let s = &self.outbox.session;
        Reserve {
            request_id: id(),
            intent_id: intent.into(),
            revision: 1,
            epoch: s.writer_epoch.clone(),
            service_epoch: s.service_epoch.clone(),
            adapter: s.adapter.clone(),
            lease: s.lease.clone(),
            retry_of: None,
        }
    }
    fn queued(&mut self) -> (Reserve, Dispatch) {
        let intent = self.intent();
        let request = self.request(&intent);
        assert!(
            matches!(self.outbox.offer(&mut self.store,request.clone(),Instant::now()).unwrap(),Offered::Queued {request_id} if request_id==request.request_id)
        );
        (request, self.host.try_next().unwrap().begin().unwrap())
    }
    fn state(&self, request: &Reserve) -> String {
        self.store
            .db
            .query_row(
                "SELECT state FROM desktop_attempts WHERE request_id=?1",
                [&request.request_id],
                |r| r.get(0),
            )
            .unwrap()
    }
}
fn delivered(command: &Command) -> Outcome {
    Outcome::Delivered {
        native_id: NativeId::Macos {
            request_id: attention::request_id(command.action.token()),
        },
    }
}
#[test]
fn only_fresh_commits_reach_the_original_host_queue() {
    let mut f = Fixture::new();
    let intent = f.intent();
    let request = f.request(&intent);
    ledger::revoke(&f.store.db, "fixture").unwrap();
    assert!(f
        .outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .is_err());
    assert!(f.host.try_next().is_err());
    ledger::authorize(&mut f.store.db, &request.epoch, &request.service_epoch).unwrap();
    f.outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .unwrap();
    assert!(matches!(
        f.outbox
            .offer(&mut f.store, request.clone(), Instant::now())
            .unwrap(),
        Offered::Existing(_)
    ));
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "delivered");
    let changed = f.request(&intent);
    assert!(matches!(
        f.outbox
            .offer(&mut f.store, changed, Instant::now())
            .unwrap(),
        Offered::Existing(_)
    ));
    assert!(f.host.try_next().is_err());
}
#[test]
fn started_drop_is_unknown_and_unknown_attempt_never_replays() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    drop(dispatch);
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "unknown");
    let next = f.request(&request.intent_id);
    assert!(matches!(
        f.outbox.offer(&mut f.store, next, Instant::now()).unwrap(),
        Offered::Existing(_)
    ));
    assert!(f.host.try_next().is_err());
}
#[test]
fn resource_charge_outlives_completion_timeout_and_stops_new_admission() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let resource = dispatch.resources();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "delivered");
    assert!(!f.outbox.operations_idle());
    f.outbox.request_stop();
    assert!(f.host.stopping());
    f.outbox.poll(&mut f.store, Instant::now() + COMMAND_WAIT);
    assert_eq!(f.state(&request), "delivered");
    assert!(!f.outbox.operation_drained());
    drop(resource);
    f.outbox.poll(&mut f.store, Instant::now() + COMMAND_WAIT);
    assert!(f.outbox.operation_drained());
    assert_eq!(f.state(&request), "delivered");
}
#[test]
fn unstarted_cancel_is_a_refusal_and_original_resources_must_drop() {
    let mut f = Fixture::new();
    let intent = f.intent();
    let request = f.request(&intent);
    f.outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .unwrap();
    f.outbox.request_stop();
    assert!(!f.outbox.operation_drained());
    assert!(f.host.try_next().unwrap().begin().is_none());
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "refused");
    assert!(f.outbox.operation_drained());
}
#[test]
fn old_or_foreign_native_receipt_cannot_settle_success() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let mut receipt = Receipt {
        binding: dispatch.command().binding.clone(),
        outcome: delivered(dispatch.command()),
    };
    receipt.binding.session.host_epoch = id();
    dispatch.finish_receipt(receipt);
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "unknown");
}
#[test]
fn callback_requires_delivered_exact_mapping_and_current_unwithdrawn_writer() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let Outcome::Delivered { native_id } = delivered(dispatch.command()) else {
        unreachable!()
    };
    let activation = Activation {
        platform_context: None,
        request_id: dispatch.command().binding.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id,
    };
    assert!(!f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    assert!(f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    let mut stale = activation.clone();
    stale.request_id = id();
    assert!(!f
        .outbox
        .current_activation(&f.store, &stale)
        .unwrap()
        .is_some());
    assert!(f.host.emit(Event::Clicked(activation.clone())));
    assert_eq!(
        f.outbox.poll(&mut f.store, Instant::now()),
        vec![activation.clone()]
    );
    assert!(f.host.emit(Event::Closed(activation.clone())));
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    assert!(!f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    assert_eq!(f.state(&request), "delivered");
}
#[test]
fn frozen_receipt_retries_sql_only_and_does_not_upgrade_after_timeout() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    f.store.db.execute_batch("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON desktop_attempts BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    drop(dispatch);
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "reserved");
    assert_eq!(f.outbox.pending.len(), 1);
    f.store
        .db
        .execute_batch("DROP TRIGGER refuse_receipt")
        .unwrap();
    f.outbox.poll(&mut f.store, Instant::now() + COMMAND_WAIT);
    assert_eq!(f.state(&request), "unknown");
    assert!(f.host.try_next().is_err());
}

#[test]
fn invalid_payload_after_reservation_retains_a_pre_api_refusal_for_settlement() {
    let mut f = Fixture::new();
    let intent = f.intent();
    let request = f.request(&intent);
    f.store
        .db
        .execute(
            "UPDATE desktop_intents SET payload='{}' WHERE id=?1",
            [&intent],
        )
        .unwrap();
    assert!(f
        .outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .is_err());
    assert!(f.host.try_next().is_err());
    assert_eq!(f.state(&request), "reserved");
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "refused");
}

#[test]
fn pending_resource_limit_never_reserves_an_extra_attempt() {
    let mut f = Fixture::new();
    for _ in 0..wire::IN_FLIGHT {
        let intent = f.intent();
        let request = f.request(&intent);
        f.outbox
            .offer(&mut f.store, request, Instant::now())
            .unwrap();
    }
    let intent = f.intent();
    let request = f.request(&intent);
    assert!(f
        .outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .is_err());
    let count: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM desktop_attempts WHERE request_id=?1",
            [request.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    f.outbox.request_stop();
    while let Ok(work) = f.host.try_next() {
        drop(work);
    }
    f.outbox.poll(&mut f.store, Instant::now());
    assert!(f.outbox.operation_drained());
}

#[test]
fn revocation_invalidates_current_clicks_before_native_close_completes() {
    let mut f = Fixture::new();
    let (_, dispatch) = f.queued();
    let Outcome::Delivered { native_id } = delivered(dispatch.command()) else {
        unreachable!()
    };
    let activation = Activation {
        platform_context: None,
        request_id: dispatch.command().binding.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id,
    };
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    assert!(f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    ledger::revoke(&f.store.db, "fixture").unwrap();
    assert!(!f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
}

#[test]
fn delivered_mapping_survives_upgrade_without_a_second_post_or_host_ledger() {
    let mut f = Fixture::new();
    let (old, dispatch) = f.queued();
    let Outcome::Delivered { native_id } = delivered(dispatch.command()) else {
        unreachable!()
    };
    let activation = Activation {
        platform_context: None,
        request_id: old.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id,
    };
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    let mut session = f.outbox.session.clone();
    session.service_epoch = id();
    session.writer_epoch = id();
    session.host_epoch = id();
    session.lease = id();
    ledger::begin_service(&mut f.store.db, &session.service_epoch).unwrap();
    let (next, host) = Outbox::new(session.clone()).unwrap();
    f.outbox = next;
    f.host = host;
    assert!(f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_none());
    ledger::authorize(
        &mut f.store.db,
        &session.writer_epoch,
        &session.service_epoch,
    )
    .unwrap();
    assert!(f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    let request = f.request(&old.intent_id);
    assert!(matches!(
        f.outbox
            .offer(&mut f.store, request, Instant::now())
            .unwrap(),
        Offered::Existing(_)
    ));
    assert!(f.host.try_next().is_err());
}

#[test]
fn restoration_is_paged_at_a_fixed_highwater_and_never_replays_work() {
    let mut f = Fixture::new();
    for _ in 0..17 {
        let (_, dispatch) = f.queued();
        let outcome = delivered(dispatch.command());
        dispatch.finish(outcome);
        f.outbox.poll(&mut f.store, Instant::now());
    }
    let badge = ledger::put_intent(
        &mut f.store.db,
        &Identity {
            kind: Kind::Badge,
            target: Target::Device {
                configuration: f.outbox.session.configuration.clone(),
            },
            edge: id(),
        },
        1,
        &json!({"badge":3}),
    )
    .unwrap();
    let request = f.request(&badge);
    assert!(ledger::reserve(&mut f.store.db, &request).unwrap().fresh);
    ledger::finish(
        &mut f.store.db,
        &request,
        State::Delivered,
        &json!({"badge":3}),
    )
    .unwrap();
    let head = f.outbox.restore_head(&f.store).unwrap();
    let first = f.outbox.restore_page(&f.store, 0, head).unwrap();
    assert_eq!(first.mappings.len(), 16);
    assert!(!first.complete);
    let (_, dispatch) = f.queued();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox.poll(&mut f.store, Instant::now());
    let last = f
        .outbox
        .restore_page(&f.store, first.after, first.through)
        .unwrap();
    assert!(last.complete);
    assert_eq!(last.mappings.len(), 1);
    assert_eq!(last.after, head);
    assert!(f.host.try_next().is_err());
    ledger::revoke(&f.store.db, "fixture").unwrap();
    assert!(f.outbox.restore_head(&f.store).is_err());
    assert!(f.outbox.restore_page(&f.store, 0, head).is_err());
}

#[test]
fn an_on_time_original_completion_keeps_mapping_when_the_actor_polls_late() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let Outcome::Delivered { native_id } = delivered(dispatch.command()) else {
        unreachable!()
    };
    let activation = Activation {
        platform_context: None,
        request_id: request.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id,
    };
    let deadline = dispatch.deadline();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.outbox
        .poll(&mut f.store, deadline + Duration::from_secs(1));
    assert_eq!(f.state(&request), "delivered");
    assert_eq!(f.outbox.status()["pending"], 1);
    assert!(!f.outbox.operations_idle());
    assert!(f
        .outbox
        .current_activation(&f.store, &activation)
        .unwrap()
        .is_some());
    f.outbox.request_stop();
    assert!(!f.outbox.operation_drained());
    drop(held);
    f.outbox
        .poll(&mut f.store, deadline + Duration::from_secs(2));
    assert!(f.outbox.operation_drained());
    assert_eq!(f.state(&request), "delivered");
}

fn activation(dispatch: &Dispatch) -> Activation {
    let Outcome::Delivered { native_id } = delivered(dispatch.command()) else {
        unreachable!()
    };
    Activation {
        request_id: dispatch.command().binding.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id,
        platform_context: None,
    }
}

#[test]
fn early_close_settles_while_original_resources_still_hold_drain() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let value = activation(&dispatch);
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    assert!(f.host.emit(Event::Closed(value.clone())));
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    assert_eq!(f.state(&request), "delivered");
    assert_eq!(f.outbox.status()["callbacks"], 0);
    assert!(!f.outbox.operations_idle());
    drop(held);
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    assert_eq!(f.state(&request), "delivered");
    assert_eq!(f.outbox.status()["callbacks"], 0);
    assert!(f
        .outbox
        .current_activation(&f.store, &value)
        .unwrap()
        .is_none());
    let head = f.outbox.restore_head(&f.store).unwrap();
    assert!(f
        .outbox
        .restore_page(&f.store, 0, head)
        .unwrap()
        .mappings
        .is_empty());
}

#[test]
fn early_click_keeps_ephemeral_wayland_context_but_restoration_does_not() {
    let mut f = Fixture::new();
    f.outbox.session.platform = Platform::Linux;
    f.host.platform = Platform::Linux;
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let native_id = NativeId::Linux {
        bus_id: "a".repeat(32),
        owner: ":1.4".into(),
        id: 7,
    };
    let value = Activation {
        request_id: request.request_id.clone(),
        token: dispatch.command().action.token().into(),
        native_id: native_id.clone(),
        platform_context: Some(wire::PlatformContext::Wayland {
            activation_token: "fixture-activation".into(),
        }),
    };
    dispatch.finish(Outcome::Delivered { native_id });
    assert!(f.host.emit(Event::Clicked(value.clone())));
    assert_eq!(
        f.outbox.poll(&mut f.store, Instant::now()),
        vec![value.clone()]
    );
    assert!(!f.outbox.operations_idle());
    drop(held);
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    let head = f.outbox.restore_head(&f.store).unwrap();
    let restored = f.outbox.restore_page(&f.store, 0, head).unwrap();
    assert_eq!(restored.mappings.len(), 1);
    assert!(restored.mappings[0].platform_context.is_none());
    let raw: String = f
        .store
        .db
        .query_row(
            "SELECT receipt FROM desktop_attempts WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("fixture-activation"));
}

#[test]
fn early_callback_is_discarded_after_unknown_without_retry_or_mapping() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let value = activation(&dispatch);
    let until = dispatch.deadline();
    assert!(f.host.emit(Event::Closed(value.clone())));
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.outbox.status()["callbacks"], 1);
    f.outbox.poll(&mut f.store, until);
    assert_eq!(f.state(&request), "unknown");
    assert_eq!(f.outbox.status()["callbacks"], 0);
    // A genuinely missing completion caused the deadline. A late success
    // cannot upgrade that consumed unknown, even after its resources release.
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    drop(held);
    f.outbox.poll(&mut f.store, until);
    assert!(f
        .outbox
        .current_activation(&f.store, &value)
        .unwrap()
        .is_none());
    assert!(f.host.try_next().is_err());
}

#[test]
fn a_failed_close_commit_retains_the_original_callback_for_sql_only_retry() {
    let mut f = Fixture::new();
    let (_, dispatch) = f.queued();
    let value = activation(&dispatch);
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.store.db.execute_batch("CREATE TRIGGER refuse_close BEFORE UPDATE OF withdrawn ON desktop_intents BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert!(f.host.emit(Event::Closed(value.clone())));
    assert!(f.host.emit(Event::Clicked(value.clone())));
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    assert_eq!(f.outbox.status()["callbacks"], 2);
    f.store
        .db
        .execute_batch("DROP TRIGGER refuse_close")
        .unwrap();
    assert!(f.outbox.poll(&mut f.store, Instant::now()).is_empty());
    assert_eq!(f.outbox.status()["callbacks"], 0);
    assert!(f
        .outbox
        .current_activation(&f.store, &value)
        .unwrap()
        .is_none());
    assert!(f.host.try_next().is_err());
}

#[test]
fn deferred_and_channel_callbacks_share_one_budget_and_hold_drain() {
    let mut f = Fixture::new();
    let (_, dispatch) = f.queued();
    let held = dispatch.resources();
    let value = activation(&dispatch);
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.store.db.execute_batch("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON desktop_attempts BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    for _ in 0..EVENTS {
        assert!(f.host.emit(Event::Closed(value.clone())));
    }
    assert!(!f.host.emit(Event::Closed(value.clone())));
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.outbox.status()["callbacks"], EVENTS);
    assert!(!f.host.emit(Event::Closed(value)));
    f.outbox.request_stop();
    assert!(!f.outbox.operation_drained());
    drop(held);
    f.store
        .db
        .execute_batch("DROP TRIGGER refuse_receipt")
        .unwrap();
    f.outbox.poll(&mut f.store, Instant::now());
    assert!(f.outbox.operation_drained());
}

#[test]
fn on_time_held_receipt_retries_only_sql_after_the_deadline() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let until = dispatch.deadline();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    f.store.db.execute_batch("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON desktop_attempts BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(f.state(&request), "reserved");
    assert!(matches!(
        f.outbox.pending[0].terminal.as_ref().unwrap().outcome,
        Outcome::Delivered { .. }
    ));
    f.store
        .db
        .execute_batch("DROP TRIGGER refuse_receipt")
        .unwrap();
    f.outbox.poll(&mut f.store, until + Duration::from_secs(1));
    assert_eq!(f.state(&request), "delivered");
    assert!(
        f.host.try_next().is_err(),
        "SQL retry cannot offer native work"
    );
    f.outbox.request_stop();
    assert!(!f.outbox.operation_drained());
    drop(held);
    f.outbox.poll(&mut f.store, until + Duration::from_secs(2));
    assert!(f.outbox.operation_drained());
}

#[test]
fn completed_held_commands_keep_the_full_admission_charge_before_reservation() {
    let mut f = Fixture::new();
    let mut holds = Vec::new();
    for _ in 0..wire::IN_FLIGHT {
        let (request, dispatch) = f.queued();
        holds.push(dispatch.resources());
        let outcome = delivered(dispatch.command());
        dispatch.finish(outcome);
        f.outbox.poll(&mut f.store, Instant::now());
        assert_eq!(f.state(&request), "delivered");
    }
    assert_eq!(f.outbox.pending.len(), wire::IN_FLIGHT);
    let intent = f.intent();
    let request = f.request(&intent);
    assert!(f
        .outbox
        .offer(&mut f.store, request.clone(), Instant::now())
        .is_err());
    let count: i64 = f
        .store
        .db
        .query_row(
            "SELECT count(*) FROM desktop_attempts WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "busy resources refuse BEFORE another reservation");
    assert!(f.host.try_next().is_err());
    drop(holds);
    f.outbox.poll(&mut f.store, Instant::now());
    assert!(f.outbox.operations_idle());
    assert!(matches!(
        f.outbox
            .offer(&mut f.store, request, Instant::now())
            .unwrap(),
        Offered::Queued { .. }
    ));
}

#[test]
fn a_truly_late_completion_stays_unknown_until_and_after_resource_release() {
    let mut f = Fixture::new();
    let (request, dispatch) = f.queued();
    let held = dispatch.resources();
    let value = activation(&dispatch);
    let until = dispatch.deadline();
    let outcome = delivered(dispatch.command());
    dispatch.finish(outcome);
    // Exact native-time boundary, without widening a production timeout/sleep.
    f.outbox.pending[0]
        .shared
        .reply
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .at = until;
    assert!(f.host.emit(Event::Clicked(value.clone())));
    assert!(f.outbox.poll(&mut f.store, until).is_empty());
    assert_eq!(f.state(&request), "unknown");
    assert!(f
        .outbox
        .current_activation(&f.store, &value)
        .unwrap()
        .is_none());
    f.outbox.request_stop();
    assert!(!f.outbox.operation_drained());
    drop(held);
    f.outbox.poll(&mut f.store, until + Duration::from_secs(1));
    assert!(f.outbox.operation_drained());
    assert_eq!(f.state(&request), "unknown");
    assert!(f.host.try_next().is_err());
}

#[test]
fn cancelled_unpulled_work_has_a_definitive_refusal_at_deadline() {
    let mut f = Fixture::new();
    let intent = f.intent();
    let now = Instant::now();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, now)
        .unwrap();
    f.outbox.retire_intent(&mut f.store, &intent).unwrap();
    f.outbox.poll(&mut f.store, now + COMMAND_WAIT);
    let (state, receipt): (String, String) = f
        .store
        .db
        .query_row(
            "SELECT state,receipt FROM desktop_attempts WHERE intent_id=?1",
            [&intent],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "refused");
    let receipt: Receipt = serde_json::from_str(&receipt).unwrap();
    assert_eq!(
        receipt.outcome,
        Outcome::Refused {
            code: Refusal::Cancelled
        }
    );
    assert!(
        !f.outbox.operations_idle(),
        "original queued work still holds resources"
    );
    assert!(f.host.try_next().unwrap().begin().is_none());
    f.outbox.poll(&mut f.store, now + COMMAND_WAIT);
    assert!(f.outbox.operations_idle());
}

#[test]
fn original_owner_lock_survives_endpoint_and_outbox_drop_until_callback_release() {
    let mut f = Fixture::new();
    let location = Location {
        root: f._temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: f.outbox.session.configuration.clone(),
    };
    let owner = Arc::new(location.lock().unwrap());
    let (mut outbox, endpoint) =
        Outbox::new_owned(f.outbox.session.clone(), owner.clone()).unwrap();
    let intent = f.intent();
    outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    let dispatch = endpoint.try_next().unwrap().begin().unwrap();
    let resources = dispatch.resources();
    dispatch.finish(Outcome::Unknown {
        code: Uncertainty::HostLost,
    });
    drop(owner);
    drop(endpoint);
    drop(outbox);
    assert!(
        location.existing_lock().is_err(),
        "original callback still pins actual owner.lock"
    );
    drop(resources);
    assert!(location.existing_lock().unwrap().is_some());
}
