use super::super::{
    ledger,
    wire::{Action, NativeId, Outcome, Platform},
    Kind, Session,
};
use super::*;
use crate::desktop::store::Location;
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    outbox: Outbox,
    value: Activation,
    intent: String,
}
impl Fixture {
    fn grant(&mut self) -> (std::path::PathBuf, ownership::Grant) {
        let image = self._temp.path().join("hgs-tray");
        std::fs::write(&image, b"original native image").unwrap();
        let origin = ownership::Origin::capture(
            &image,
            desktop::Build {
                version: "1.2.3".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
        )
        .unwrap();
        self.store
            .db
            .execute(
                "UPDATE desktop_ownership SET state='service',owner_epoch=?1,origin=?2 WHERE id=1",
                rusqlite::params![
                    &self.outbox.session.writer_epoch,
                    serde_json::to_string(&origin).unwrap()
                ],
            )
            .unwrap();
        let grant = ownership::grant(&self.store, &self.outbox.session.configuration)
            .unwrap()
            .unwrap();
        (image, grant)
    }
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let session = Session {
            configuration: location.configuration.clone(),
            service_epoch: id(),
            writer_epoch: id(),
            adapter: id(),
            host_epoch: id(),
            lease: id(),
            platform: Platform::Macos,
        };
        let mut store = Store::open(&location, &session.service_epoch).unwrap();
        ledger::authorize(&mut store.db, &session.writer_epoch, &session.service_epoch).unwrap();
        let token=zerus_client_core::attention::encode_token(json!({"host":"peer.example","run":"run","conversation":"conversation","name":"fixture","notice":id()}).as_object().unwrap());
        let intent = ledger::put_intent(
            &mut store.db,
            &Identity {
                kind: Kind::Post,
                target: Target::Session {
                    computer: "peer.example".into(),
                    run: "run".into(),
                    conversation: "conversation".into(),
                },
                edge: id(),
            },
            1,
            &serde_json::to_value(Action::Post {
                token: token.clone(),
                title: "Fixture".into(),
                body: "Body".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let (mut outbox, endpoint) = Outbox::new(session).unwrap();
        outbox
            .offer_intent(&mut store, &intent, 1, Instant::now())
            .unwrap();
        let dispatch = endpoint.try_next().unwrap().begin().unwrap();
        let value = Activation {
            request_id: dispatch.command().binding.request_id.clone(),
            token: token.clone(),
            native_id: NativeId::Macos {
                request_id: zerus_client_core::attention::request_id(&token),
            },
            platform_context: None,
        };
        dispatch.finish(Outcome::Delivered {
            native_id: value.native_id.clone(),
        });
        outbox.poll(&mut store, Instant::now());
        Self {
            _temp: temp,
            store,
            outbox,
            value,
            intent,
        }
    }
    fn router(&self) -> Router {
        Router::new(
            &self.outbox.session.configuration,
            &self.outbox.session.service_epoch,
        )
    }
    fn wait(&self, router: &mut Router) -> (String, Arc<Queue>) {
        let request_id = id();
        let queue = Arc::new(Queue::default());
        router
            .wait(
                ui_authority::Validated::fixture(
                    &json!({"op":"activation_wait","request_id":request_id}),
                ),
                queue.clone(),
            )
            .unwrap();
        (request_id, queue)
    }
}

#[test]
fn only_current_service_mode_grant_can_spend_an_absence_bound_click() {
    let mut f = Fixture::new();
    let (_, grant) = f.grant();
    let mut router = f.router();
    router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
    let until = router.pending[0].until;
    let id = router.pending[0].id.clone();
    let launch = router
        .closed_frontend(
            &f.store,
            &f.outbox,
            &desktop::Observation::Absent,
            Some(&grant),
        )
        .unwrap()
        .unwrap();
    assert_eq!(launch.until, until);
    assert_eq!(launch.open().activation_id, id);
    assert_eq!(launch.open().target.conversation, "conversation");
    assert!(router.pending.is_empty());
    assert!(router
        .closed_frontend(
            &f.store,
            &f.outbox,
            &desktop::Observation::Absent,
            Some(&grant)
        )
        .unwrap()
        .is_none());
}

#[test]
fn revoked_native_takeover_configuration_and_image_change_keep_known_unsent_clicks() {
    for change in ["revoke", "native", "configuration", "image"] {
        let mut f = Fixture::new();
        let (image, grant) = f.grant();
        let mut router = f.router();
        router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
        let id = router.pending[0].id.clone();
        let until = router.pending[0].until;
        match change {
            "revoke" => ownership::invalidate(&f.store.db).unwrap(),
            "native" => {
                f.store
                    .db
                    .execute(
                        "UPDATE desktop_ownership SET state='native',rearm_required=1",
                        [],
                    )
                    .unwrap();
            }
            "configuration" => router.configuration = "b".repeat(64),
            "image" => std::fs::write(image, b"a replacement native image").unwrap(),
            _ => unreachable!(),
        }
        assert!(
            router
                .closed_frontend(
                    &f.store,
                    &f.outbox,
                    &desktop::Observation::Absent,
                    Some(&grant)
                )
                .is_err(),
            "{change}"
        );
        assert_eq!(router.pending[0].id, id);
        assert_eq!(router.pending[0].until, until);
    }
}
#[test]
fn only_original_live_mapping_reaches_the_authenticated_wait_and_never_replays() {
    let mut f = Fixture::new();
    let mut router = f.router();
    let mut forged = f.value.clone();
    forged.request_id = id();
    assert!(!router.offer(&f.store, &f.outbox, forged).unwrap());
    assert!(router.offer(&f.store, &f.outbox, f.value.clone()).unwrap());
    let (request, queue) = f.wait(&mut router);
    let mut seq = 0;
    router.poll(&f.store, Some(&f.outbox), &mut seq);
    let frame: Value = serde_json::from_slice(&queue.take().unwrap()).unwrap();
    let reply: Reply = serde_json::from_value(frame["value"].clone()).unwrap();
    assert!(reply.matches(
        &request,
        &f.outbox.session.configuration,
        &f.outbox.session.service_epoch
    ));
    let open = reply.activation.unwrap();
    assert_eq!(open.post_request_id, f.value.request_id);
    assert_eq!(open.target.conversation, "conversation");
    assert!(queue.closed());
    assert!(router.pending.is_empty());
    let (_, second) = f.wait(&mut router);
    router.waiting.as_mut().unwrap().until = Instant::now();
    router.poll(&f.store, Some(&f.outbox), &mut seq);
    let frame: Value = serde_json::from_slice(&second.take().unwrap()).unwrap();
    assert!(frame["value"]["activation"].is_null());
    // Recheck withdrawal at handoff, not only at callback admission.
    router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
    ledger::withdraw(&mut f.store.db, &f.intent, 1).unwrap();
    let (_, withdrawn) = f.wait(&mut router);
    router.waiting.as_mut().unwrap().until = Instant::now();
    router.poll(&f.store, Some(&f.outbox), &mut seq);
    let frame: Value = serde_json::from_slice(&withdrawn.take().unwrap()).unwrap();
    assert!(frame["value"]["activation"].is_null());
}
#[test]
fn canonical_absence_without_an_ownership_grant_never_switches_mode_or_spends_the_click() {
    let f = Fixture::new();
    let mut router = f.router();
    router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
    for state in [
        desktop::Observation::Unknown,
        desktop::Observation::LiveLegacyOrUnknown,
    ] {
        assert!(router
            .closed_frontend(&f.store, &f.outbox, &state, None)
            .unwrap()
            .is_none());
    }
    let original_id = router.pending[0].id.clone();
    let original_until = router.pending[0].until;
    for _ in 0..2 {
        assert!(router
            .closed_frontend(&f.store, &f.outbox, &desktop::Observation::Absent, None)
            .is_err());
        assert_eq!(router.pending.len(), 1);
        assert_eq!(router.pending[0].id, original_id);
        assert_eq!(router.pending[0].until, original_until);
    }
    // A legitimate service-mode frontend may still receive the original event
    // through its authenticated wait; failure to launch was definitely unsent.
    let (_, queue) = f.wait(&mut router);
    let mut seq = 0;
    router.poll(&f.store, Some(&f.outbox), &mut seq);
    let frame: Value = serde_json::from_slice(&queue.take().unwrap()).unwrap();
    assert_eq!(frame["value"]["activation"]["activation_id"], original_id);
    assert_eq!(
        frame["value"]["activation"]["post_request_id"],
        f.value.request_id
    );
    assert!(router.pending.is_empty());
}
#[test]
fn queue_wait_lifetime_stop_and_configuration_are_bounded() {
    let f = Fixture::new();
    let mut router = f.router();
    for _ in 0..wire::QUEUE {
        assert!(router.offer(&f.store, &f.outbox, f.value.clone()).unwrap())
    }
    assert!(!router.offer(&f.store, &f.outbox, f.value.clone()).unwrap());
    let (_, queue) = f.wait(&mut router);
    assert!(router
        .wait(
            ui_authority::Validated::fixture(&json!({"op":"activation_wait","request_id":id()})),
            Arc::default()
        )
        .is_err());
    assert!(router
        .closed_frontend(&f.store, &f.outbox, &desktop::Observation::Absent, None)
        .unwrap()
        .is_none());
    router
        .pending
        .iter_mut()
        .for_each(|p| p.until = Instant::now());
    router.expire();
    assert!(router.pending.is_empty());
    router.stop();
    assert!(queue.closed());
    assert!(router.offer(&f.store, &f.outbox, f.value.clone()).is_err());
    assert!(
        Router::new(&"b".repeat(64), &f.outbox.session.service_epoch)
            .offer(&f.store, &f.outbox, f.value.clone())
            .is_err()
    );
}
#[test]
fn policy_permit_and_detached_receiver_do_not_consume_activation() {
    let f = Fixture::new();
    let mut router = f.router();
    router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
    assert!(router
        .wait(
            ui_authority::Validated::fixture(
                &json!({"op":"automatic_approvals","request_id":id(),"enabled":false})
            ),
            Arc::default()
        )
        .is_err());
    let (_, queue) = f.wait(&mut router);
    queue.close();
    router.poll(&f.store, Some(&f.outbox), &mut 0);
    assert_eq!(router.pending.len(), 1);
}

#[test]
fn a_changed_full_proof_cannot_deliver_a_click_during_the_cache_interval() {
    use std::sync::atomic::Ordering;
    let f = Fixture::new();
    let mut router = f.router();
    router.offer(&f.store, &f.outbox, f.value.clone()).unwrap();
    let (_, queue) = f.wait(&mut router);
    let probe = Arc::new(ui_authority::ActivationProbe::default());
    router.waiting.as_mut().unwrap().peer.set_probe(
        probe.clone(),
        Instant::now() + std::time::Duration::from_secs(3600),
    );
    // Cheap handles remain live, but the original ancestry proof has changed
    // since admission. The periodic cache must not authorize this handoff.
    let original_id = router.pending.front().unwrap().id.clone();
    let original_until = router.pending.front().unwrap().until;
    probe.valid.store(false, Ordering::SeqCst);
    router.poll(&f.store, Some(&f.outbox), &mut 0);
    assert!(queue.closed());
    assert!(queue.take().is_none());
    assert_eq!(probe.full_checks.load(Ordering::SeqCst), 1);
    assert_eq!(router.pending.front().unwrap().until, original_until);
    assert_eq!(
        router.pending.len(),
        1,
        "known-unsent click survives proof refusal"
    );
    let (_, next_queue) = f.wait(&mut router);
    router.poll(&f.store, Some(&f.outbox), &mut 0);
    let frame: Value = serde_json::from_slice(&next_queue.take().unwrap()).unwrap();
    assert_eq!(frame["value"]["activation"]["activation_id"], original_id);
    assert_eq!(
        frame["value"]["activation"]["post_request_id"],
        f.value.request_id
    );
    assert!(router.pending.is_empty());
}
