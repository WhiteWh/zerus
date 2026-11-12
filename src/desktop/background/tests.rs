use super::*;
use crate::desktop::{notifications::preparation, ownership};
use std::sync::mpsc;

struct Fixture {
    _temp: tempfile::TempDir,
    image: std::path::PathBuf,
    location: Location,
    store: Store,
    owner: Arc<platform::FileLock>,
    broker: feed::Broker,
    runtime: Runtime,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = Config {
            home: root.to_str().unwrap().into(),
            dir: root.join("config"),
            state: root.join("state"),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        std::fs::create_dir_all(&config.dir).unwrap();
        let location = Location::new(&config).unwrap();
        location.prepare().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&location, &epoch).unwrap();
        let image = temp.path().join("hgs-tray");
        std::fs::write(&image, b"owned fixture image").unwrap();
        let origin = ownership::Origin::capture(
            &image,
            zerus_client_core::desktop::Build {
                version: "1.2.3".into(),
                source_commit: "a".repeat(40),
                source_dirty: Some(false),
            },
        )
        .unwrap();
        store
            .db
            .execute(
                "UPDATE desktop_ownership SET state='service',owner_epoch=?1,origin=?2 WHERE id=1",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    serde_json::to_string(&origin).unwrap()
                ],
            )
            .unwrap();
        let owner = Arc::new(location.lock().unwrap());
        let mut broker = feed::Broker::new(&epoch);
        let runtime = Runtime {
            fleet: fleet::Runtime::empty_for_test(&epoch),
            producer: broker.claim_internal().unwrap(),
            presence: None,
            delivery: delivery::Runtime::start(&config, &epoch).unwrap(),
            details: details::Adapter::new(&epoch),
            maintenance: maintenance::Runtime::start(&config, &epoch).unwrap(),
            cache: crate::desktop::cache_maintenance::Runtime::new(
                &location,
                &epoch,
                owner.clone(),
            ),
            attention: attention::Runtime::new(&epoch),
            activations: activation::Router::new(&location.configuration, &epoch),
            drained: false,
            stopping: false,
            #[cfg(windows)]
            swarm: crate::desktop::swarm_windows::Runtime::new(&config, owner.clone()),
            notifications: notification_service::Runtime::new(
                &config,
                &location,
                &epoch,
                owner.clone(),
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            creation: crate::desktop::creation_clone::Runtime::new(&config, &epoch, owner.clone())
                .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            worktree: crate::desktop::creation_worktree::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
            )
            .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            launch: crate::desktop::creation_launch::Runtime::new(&config, &epoch, owner.clone())
                .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            message: crate::desktop::task_message::Runtime::new(&config, &epoch, owner.clone())
                .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            send: crate::desktop::deferred_send::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
                Default::default(),
            )
            .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            answer: crate::desktop::deferred_answer::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
                Default::default(),
            )
            .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            compact: crate::desktop::compact_to_send::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
                Default::default(),
            )
            .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            resume: crate::desktop::session_resume::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
                Default::default(),
            )
            .unwrap(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            task_start: crate::desktop::task_start_reconcile::Runtime::new(
                &config,
                &epoch,
                owner.clone(),
            )
            .unwrap(),
            #[cfg(target_os = "linux")]
            route: crate::desktop::question_route::Runtime::new(&config, &epoch),
            state: config.state.clone(),
        };
        Self {
            _temp: temp,
            image,
            location,
            store,
            owner,
            broker,
            runtime,
        }
    }
}

#[test]
fn uncaptured_notification_writer_revokes_the_durable_epoch_and_can_hold_the_same_receipt() {
    let mut f = Fixture::new();
    let service: String = f
        .store
        .db
        .query_row("SELECT epoch FROM service WHERE id=1", [], |row| row.get(0))
        .unwrap();
    let writer = uuid::Uuid::new_v4().to_string();
    zerus_desktop_ledger::authorize(&mut f.store.db, &writer, &service).unwrap();
    // A restart revokes admission but preserves the original durable writer.
    // No notification grant/host has been captured by this fresh Runtime.
    zerus_desktop_ledger::begin_service(&mut f.store.db, &service).unwrap();
    assert!(f.runtime.notifications.writer_epoch().is_none());
    let id = uuid::Uuid::new_v4().to_string();
    let captured = f.runtime.captured_writer_epoch();
    let reply = crate::desktop::revoke::request_with_runtime(
        &f.location,
        &mut f.store,
        &json!({"op":"revoke","request_id":id,"reason":"rollback"}),
        &service,
        captured.as_deref(),
        None,
        |store| f.runtime.drain(store, Duration::from_secs(2)),
    );
    assert!(
        f.runtime.drained(),
        "original resources must actually drain before assertion"
    );
    assert_eq!(reply["checkpointed"], true, "{reply}");
    let held =
        crate::desktop::revoke::hold::acquire_for_test(&f.location, &id).unwrap_or_else(|error| {
            panic!("same original settled receipt must acquire its hold: {error:#}")
        });
    assert_eq!(reply["request_id"], id);
    assert_eq!(reply["writer_epoch"], writer);
    drop(held);
}

#[test]
fn failed_notification_authorize_never_publishes_an_uncommitted_writer() {
    let _serial = preparation::TEST_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut f = Fixture::new();
    assert!(ownership::grant(&f.store, &f.location.configuration)
        .unwrap()
        .is_some());
    f.store.db.execute_batch("CREATE TRIGGER r9_authorize_failure BEFORE UPDATE OF epoch ON desktop_writer BEGIN SELECT RAISE(ABORT,'fixture authorize failure'); END;").unwrap();
    let (release, blocked) = mpsc::channel();
    let admission = f
        .runtime
        .notifications
        .begin_blocked_for_test(&mut f.store, blocked);
    let published = f.runtime.notifications.writer_epoch().map(str::to_owned);
    let _ = release.send(());
    f.runtime
        .drain(&mut f.store, Duration::from_secs(2))
        .unwrap();
    assert!(
        admission.is_err(),
        "fixture must exercise failed ledger authorization"
    );
    assert!(
        published.is_none(),
        "a failed authorize is not a captured writer"
    );
    let epoch: Option<String> = f
        .store
        .db
        .query_row("SELECT epoch FROM desktop_writer WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(epoch.is_none());
}

#[test]
fn slow_replacement_keeps_draining_on_actor_turns_without_a_client_retry() {
    let _serial = preparation::TEST_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Fixture {
        _temp,
        image,
        location,
        mut store,
        owner,
        mut broker,
        mut runtime,
    } = Fixture::new();
    let (release, resume) = mpsc::sync_channel(1);
    runtime.notifications.block_selection_for_test(resume);
    assert!(runtime.drain(&mut store, Duration::ZERO).is_err());
    assert!(!runtime.drained());
    assert!(location.handoff_lock().is_err());
    assert!(!replacement_permitted(true, runtime.drained(), &location, &store).unwrap());
    // No frontend call/retry follows the timeout. Only normal actor polling.
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    let mut seq = 0;
    while !runtime.drained() {
        assert!(Instant::now() < until, "original selection did not join");
        runtime.poll(&mut store, &mut broker, &mut seq);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(location.handoff_lock().is_ok());
    assert!(replacement_permitted(true, runtime.drained(), &location, &store).unwrap());
    assert!(
        !replacement_permitted(false, runtime.drained(), &location, &store).unwrap(),
        "passive serve cannot start background"
    );
    // Routine update changes only launch eligibility, not the retained grant.
    std::fs::write(&image, b"updated owned fixture image").unwrap();
    assert!(!ownership::service_granted(&store, &location.configuration).unwrap());
    assert!(replacement_permitted(true, true, &location, &store).unwrap());
    ownership::invalidate(&store.db).unwrap();
    assert!(
        !replacement_permitted(true, true, &location, &store).unwrap(),
        "revoke cannot be rearmed by a drained handle"
    );
    drop(runtime);
    drop(owner);
    assert!(location.lock().is_ok());
}

#[test]
fn a_cache_stop_database_error_still_stops_every_other_component() {
    let _serial = preparation::TEST_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut f = Fixture::new();
    f.runtime.start_presence(&f.location).unwrap();
    f.store.db.execute_batch("PRAGMA query_only=ON;").unwrap();
    let error = f.runtime.drain(&mut f.store, Duration::ZERO).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(|e| e.sqlite_error_code()),
        Some(rusqlite::ErrorCode::ReadOnly),
        "{error:#}"
    );
    assert!(f.runtime.stopping);
    assert!(
        !f.runtime.drained(),
        "stop admission is not a join or SQL drain"
    );
    assert_eq!(f.runtime.cache.status()["stopping"], true);
    assert_eq!(f.runtime.notifications.status()["stopping"], true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(f.runtime.creation.status()["code"], "draining");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(f.runtime.worktree.status()["code"], "draining");
    let presence = f.runtime.presence.as_ref().unwrap();
    assert_eq!(presence.status()["code"], "draining");
    // Restore only this fixture connection. Subsequent original actor polling
    // must join actual workers before the runtime can report drained.
    f.store.db.execute_batch("PRAGMA query_only=OFF;").unwrap();
    f.runtime
        .drain(&mut f.store, Duration::from_secs(3))
        .unwrap();
    assert!(f.runtime.drained());
}

/// A valid deferred answer to a Claude folder-trust question, built as the
/// client builds it.
fn deferred_answer_intent(
    configuration: String,
    epoch: String,
) -> zerus_client_core::desktop::deferred_answer::Intent {
    use zerus_client_core::{
        desktop::deferred_answer::{Intent, Scope},
        operation_recovery::deferred_input as checkpoint,
        questions::{consent_identity, AnswerItem},
    };
    let question = serde_json::json!({"question_id": "q-1", "question_hash": "ab".repeat(32),
        "run_id": "run-1", "conversation_id": "conversation-1", "source": "claude_folder_trust",
        "answer_transport": "claude_tui", "questions": [{"id": "q-1", "question": "Trust?",
            "options": [{"id": "yes", "label": "Yes"}]}]});
    let answers = vec![AnswerItem::Choice {
        question_id: "q-1".into(),
        selected_option_ids: vec!["yes".into()],
        text: String::new(),
    }];
    let identity = consent_identity(&question);
    let reserved = checkpoint::Checkpoint::reserve(checkpoint::Intent {
        kind: checkpoint::Kind::Answer,
        draft_revision: 1,
        message_sha256: checkpoint::message_sha256(
            &serde_json::to_string(&answers).unwrap(),
            std::iter::empty(),
        ),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        question: Some(checkpoint::Question {
            id: "q-1".into(),
            identity_sha256: checkpoint::identity_sha256(&identity),
        }),
        start: None,
        retry_of: None,
        consent: false,
    });
    let original = serde_json::json!({"request_id": reserved.request_id, "question_id": "q-1",
        "expected_question_hash": "ab".repeat(32), "expected_run_id": "run-1",
        "expected_conversation_id": "conversation-1", "answers": answers});
    Intent {
        scope: Scope {
            configuration,
            service_epoch: epoch,
        },
        session: "claude/private-answer".into(),
        request_id: reserved.request_id.clone(),
        checkpoint: serde_json::to_string(&reserved).unwrap(),
        pinned: identity.to_string(),
        original: original.to_string(),
    }
}

/// A valid compact-to-send, built as the client builds it.
fn compact_to_send_intent(
    configuration: String,
    epoch: String,
) -> zerus_client_core::desktop::compact_to_send::Intent {
    use zerus_client_core::{
        desktop::compact_to_send::{Intent, Scope},
        operation_recovery::deferred_input as checkpoint,
    };
    let reserved = checkpoint::Checkpoint::reserve_compact_to_send(checkpoint::Intent {
        kind: checkpoint::Kind::CompactToSend,
        draft_revision: 1,
        message_sha256: checkpoint::message_sha256("after compaction", std::iter::empty()),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        question: None,
        start: None,
        retry_of: None,
        consent: true,
    })
    .unwrap();
    let compaction = serde_json::json!({"request_id": reserved.request_id,
        "expected_run_id": "run-1", "expected_conversation_id": "conversation-1"});
    let original = serde_json::json!({"request_id": reserved.send_request_id,
        "text": "after compaction", "attachments": [], "expected_run_id": "run-1",
        "expected_conversation_id": "conversation-1", "expected_compaction_id": reserved.request_id});
    Intent {
        scope: Scope {
            configuration,
            service_epoch: epoch,
        },
        session: "claude/private-compact".into(),
        request_id: reserved.request_id.clone(),
        send_request_id: reserved.send_request_id.clone().unwrap(),
        checkpoint: serde_json::to_string(&reserved).unwrap(),
        compaction: compaction.to_string(),
        original: original.to_string(),
    }
}

/// zerus-5wmh.66.4.8: a compact-to-send reaches its actor, which refuses
/// it without a lease before any row or child.
#[test]
fn a_compact_to_send_reaches_its_actor_which_refuses_without_a_lease() {
    let mut f = Fixture::new();
    let intent = compact_to_send_intent(
        f.location.configuration.clone(),
        uuid::Uuid::new_v4().to_string(),
    );
    intent.clone().verify().unwrap();
    let request = zerus_client_core::desktop::ui_requests::Request::CompactToSend {
        intent: Box::new(intent.clone()),
    };
    let authority = crate::desktop::ui_authority::Validated::fixture(&serde_json::json!(request));
    let answer = f.runtime.frontend_request(&mut f.store, authority);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let admission: zerus_client_core::desktop::compact_to_send::Admission =
            serde_json::from_value(answer).unwrap();
        assert_eq!(admission.request_id, intent.request_id);
        assert!(!admission.accepted && !admission.durable && admission.receipt.is_none());
        #[cfg(target_os = "linux")]
        assert_eq!(admission.code, "compact_to_send_unavailable");
        #[cfg(target_os = "macos")]
        assert_eq!(admission.code, "compact_to_send_native_image_unavailable");
    }
    #[cfg(windows)]
    assert_eq!(
        answer,
        serde_json::json!({"request_id": intent.request_id, "accepted": false,
            "durable": false, "code": "compact_to_send_unsupported"})
    );
    let rows: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_compact_to_sends", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

/// A valid session resume, built as the client builds it.
fn session_resume_intent(
    configuration: String,
    epoch: String,
) -> zerus_client_core::desktop::session_resume::Intent {
    use zerus_client_core::{
        desktop::session_resume::{Intent, Scope},
        operation_recovery::deferred_input as checkpoint,
    };
    let reserved = checkpoint::Checkpoint::reserve_session_resume(checkpoint::Intent {
        kind: checkpoint::Kind::ResumeToSend,
        draft_revision: 1,
        message_sha256: checkpoint::message_sha256("after resume", std::iter::empty()),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        question: None,
        start: Some(checkpoint::Start::Resume),
        retry_of: None,
        consent: true,
    })
    .unwrap();
    let resume = serde_json::json!({"request_id": reserved.request_id, "action": "resume",
        "expected_run_id": "run-1", "expected_conversation_id": "conversation-1"});
    let original = serde_json::json!({"request_id": reserved.send_request_id,
        "text": "after resume", "attachments": [], "expected_run_id": "run-1",
        "expected_conversation_id": "conversation-1"});
    Intent {
        scope: Scope {
            configuration,
            service_epoch: epoch,
        },
        session: "claude/private-resume".into(),
        request_id: reserved.request_id.clone(),
        send_request_id: reserved.send_request_id.clone().unwrap(),
        checkpoint: serde_json::to_string(&reserved).unwrap(),
        resume: resume.to_string(),
        original: original.to_string(),
    }
}

/// zerus-5wmh.66.4.9: a session resume reaches its actor, which refuses it
/// without a lease before any row or child.
#[test]
fn a_session_resume_reaches_its_actor_which_refuses_without_a_lease() {
    let mut f = Fixture::new();
    let intent = session_resume_intent(
        f.location.configuration.clone(),
        uuid::Uuid::new_v4().to_string(),
    );
    intent.clone().verify().unwrap();
    let request = zerus_client_core::desktop::ui_requests::Request::SessionResume {
        intent: Box::new(intent.clone()),
    };
    let authority = crate::desktop::ui_authority::Validated::fixture(&serde_json::json!(request));
    let answer = f.runtime.frontend_request(&mut f.store, authority);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let admission: zerus_client_core::desktop::session_resume::Admission =
            serde_json::from_value(answer).unwrap();
        assert_eq!(admission.request_id, intent.request_id);
        assert!(!admission.accepted && !admission.durable && admission.receipt.is_none());
        #[cfg(target_os = "linux")]
        assert_eq!(admission.code, "session_resume_unavailable");
        #[cfg(target_os = "macos")]
        assert_eq!(admission.code, "session_resume_native_image_unavailable");
    }
    #[cfg(windows)]
    assert_eq!(
        answer,
        serde_json::json!({"request_id": intent.request_id, "accepted": false,
            "durable": false, "code": "session_resume_unsupported"})
    );
    let rows: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_session_resumes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn a_deferred_answer_reaches_its_actor_which_refuses_without_a_lease() {
    let mut f = Fixture::new();
    let intent = deferred_answer_intent(
        f.location.configuration.clone(),
        uuid::Uuid::new_v4().to_string(),
    );
    intent.clone().verify().unwrap();
    let request = zerus_client_core::desktop::ui_requests::Request::DeferredAnswer {
        intent: Box::new(intent.clone()),
    };
    let authority = crate::desktop::ui_authority::Validated::fixture(&serde_json::json!(request));
    let answer = f.runtime.frontend_request(&mut f.store, authority);
    // The actor's own typed refusal: no fleet source, so no lease; nothing
    // is admitted and no row exists.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let admission: zerus_client_core::desktop::deferred_answer::Admission =
            serde_json::from_value(answer).unwrap();
        assert_eq!(admission.request_id, intent.request_id);
        assert!(!admission.accepted && !admission.durable && admission.receipt.is_none());
        #[cfg(target_os = "linux")]
        assert_eq!(admission.code, "deferred_answer_unavailable");
        #[cfg(target_os = "macos")]
        assert_eq!(admission.code, "deferred_answer_native_image_unavailable");
    }
    // Windows has no executor.
    #[cfg(windows)]
    assert_eq!(
        answer,
        serde_json::json!({"request_id": intent.request_id, "accepted": false,
            "durable": false, "code": "deferred_answer_unsupported"})
    );
    let rows: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_deferred_answers", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn a_deferred_send_reaches_its_actor_which_refuses_without_a_lease() {
    use zerus_client_core::{
        desktop::{
            deferred_send::{Intent, Scope},
            ui_requests::Request,
        },
        operation_recovery::deferred_input as checkpoint,
    };
    let mut f = Fixture::new();
    let reserved = checkpoint::Checkpoint::reserve(checkpoint::Intent {
        kind: checkpoint::Kind::Send,
        draft_revision: 1,
        message_sha256: checkpoint::message_sha256("hello", std::iter::empty()),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        question: None,
        start: None,
        retry_of: None,
        consent: true,
    });
    let original = serde_json::json!({"request_id": reserved.request_id, "text": "hello",
        "attachments": [], "expected_run_id": "run-1",
        "expected_conversation_id": "conversation-1"});
    let request = Request::DeferredSend {
        intent: Box::new(Intent {
            scope: Scope {
                configuration: f.location.configuration.clone(),
                service_epoch: uuid::Uuid::new_v4().to_string(),
            },
            session: "claude/private-send".into(),
            request_id: reserved.request_id.clone(),
            checkpoint: serde_json::to_string(&reserved).unwrap(),
            original: original.to_string(),
        }),
    };
    let authority = crate::desktop::ui_authority::Validated::fixture(&serde_json::json!(request));
    let answer = f.runtime.frontend_request(&mut f.store, authority);
    // The actor's own typed refusal: no fleet source, so no lease; nothing
    // is admitted and no row exists.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let admission: zerus_client_core::desktop::deferred_send::Admission =
            serde_json::from_value(answer).unwrap();
        assert_eq!(admission.request_id, reserved.request_id);
        assert!(!admission.accepted && !admission.durable && admission.receipt.is_none());
        #[cfg(target_os = "linux")]
        assert_eq!(admission.code, "deferred_send_unavailable");
        #[cfg(target_os = "macos")]
        assert_eq!(admission.code, "deferred_send_native_image_unavailable");
    }
    // Windows has no executor.
    #[cfg(windows)]
    assert_eq!(
        answer,
        serde_json::json!({"request_id": reserved.request_id, "accepted": false,
            "durable": false, "code": "deferred_send_unsupported"})
    );
    let rows: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_deferred_sends", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}
