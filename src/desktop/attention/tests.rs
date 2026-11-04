use super::projection::{self, Projection};
use super::*;
use crate::desktop::{
    notifications::{self, Endpoint},
    store::Location,
};
use serde_json::{json, Map, Value};
use zerus_client_core::{
    attention,
    desktop::notifications::{Action, NativeId, Outcome, Platform, Session},
};
use zerus_desktop_ledger as ledger;
use zerus_watch_protocol::{Body, CatalogItem, Delta, Frame, Hello, Snapshot};
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn retire_ticks(f: &mut Fixture, n: usize) {
    for _ in 0..n {
        f.outbox.poll(&mut f.store, Instant::now());
        f.tick();
    }
}
fn settle_retirement(f: &mut Fixture) {
    for _ in 0..1024 {
        if !f.runtime.retiring() {
            return;
        }
        retire_ticks(f, 1);
    }
    panic!("retirement did not settle: {}", f.runtime.status());
}

#[test]
fn policy_off_waits_for_original_post_and_withdrawal_resources_before_idle() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    f.save(&row(), false);
    let intent: String = f
        .store
        .db
        .query_row("SELECT id FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    let post = f.host.try_next().unwrap().begin().unwrap();
    let token = post.command().action.token().to_owned();
    let native_id = NativeId::Macos {
        request_id: attention::request_id(&token),
    };
    let post_hold = post.resources();
    f.runtime.set_policy(Policy::default());
    retire_ticks(&mut f, 2);
    assert!(
        !post.cancelled(),
        "an admitted native command keeps its receipt channel"
    );
    post.finish(Outcome::Delivered {
        native_id: native_id.clone(),
    });
    retire_ticks(&mut f, 2);
    assert!(f.runtime.retiring());
    assert!(!ledger::current(&f.store.db, &intent, 1).unwrap());
    assert!(
        !f.outbox.operations_idle(),
        "the original post resources remain charged after its receipt"
    );
    drop(post_hold);
    retire_ticks(&mut f, 2);
    let withdrawal = f.host.try_next().unwrap().begin().unwrap();
    assert!(
        matches!(&withdrawal.command().action, Action::Withdraw {token: t,native_id: n} if *t==token && *n==native_id)
    );
    let withdrawal_hold = withdrawal.resources();
    withdrawal.finish(Outcome::Delivered { native_id });
    retire_ticks(&mut f, 2);
    assert!(f.runtime.retiring());
    assert!(!f.outbox.operations_idle());
    drop(withdrawal_hold);
    retire_ticks(&mut f, 2);
    assert!(!f.runtime.retiring());
    assert!(f.outbox.operations_idle());
    assert!(
        !f.host.stopping(),
        "policy-off retains the same connected host"
    );
    assert!(f.host.try_next().is_err());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    f.runtime.set_policy(enabled());
    f.snapshot(vec![row()]);
    f.ticks(4);
    assert!(
        f.host.try_next().is_err(),
        "reenabling cannot resurrect the retired edge"
    );
}

#[test]
fn policy_off_does_not_replay_an_unknown_post() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    f.save(&row(), false);
    let intent: String = f
        .store
        .db
        .query_row("SELECT id FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    f.host
        .try_next()
        .unwrap()
        .begin()
        .unwrap()
        .finish(Outcome::Unknown {
            code: zerus_client_core::desktop::notifications::Uncertainty::HostLost,
        });
    f.runtime.set_policy(Policy::default());
    retire_ticks(&mut f, 4);
    assert!(!f.runtime.retiring());
    assert!(f.host.try_next().is_err());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    f.runtime.set_policy(enabled());
    f.snapshot(vec![row()]);
    f.ticks(4);
    assert!(f.host.try_next().is_err());
}

#[test]
fn retirement_pages_are_bounded_and_can_resume_from_existing_live_edges() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    for n in 0..20 {
        let mut raw = row();
        raw["conversation_id"] = json!(format!("conversation-{n}"));
        raw["run_id"] = json!(format!("run-{n}"));
        f.save(&raw, false);
    }
    f.runtime.set_policy(Policy::default());
    retire_ticks(&mut f, 1);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM desktop_attention_edges WHERE live=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        20 - EFFECTS_PER_TURN as i64
    );
    // A restarted component can resume from the existing ledger, without a new
    // intent or inferred post retry. The lifecycle supplies its durable off state.
    f.runtime = Runtime::new(&f.source.service_epoch);
    f.runtime.request_retirement();
    retire_ticks(&mut f, 16);
    assert!(!f.runtime.retiring());
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM desktop_attention_edges WHERE live=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(f.intents(), 20);
    assert!(f.host.try_next().is_err());
}

#[test]
fn policy_off_cancels_queued_posts_before_the_live_walk_reaches_their_edge() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    for n in 0..20 {
        let mut raw = row();
        raw["conversation_id"] = json!(format!("conversation-{n}"));
        raw["run_id"] = json!(format!("run-{n}"));
        f.save(&raw, false);
    }
    let intent: String = f
        .store
        .db
        .query_row(
            "SELECT intent FROM desktop_attention_edges ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    f.runtime.set_policy(Policy::default());
    retire_ticks(&mut f, 1);
    assert!(
        ledger::current(&f.store.db, &intent, 1).unwrap(),
        "the bounded walk has not reached the final edge"
    );
    assert!(
        f.host.try_next().unwrap().begin().is_none(),
        "queued admission is already cancelled"
    );
    settle_retirement(&mut f);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT state FROM desktop_attempts WHERE intent_id=?1",
                [&intent],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        "refused"
    );
    assert!(!ledger::current(&f.store.db, &intent, 1).unwrap());
}
fn enabled() -> Policy {
    Policy {
        enabled: true,
        optional_questions: true,
    }
}
fn row() -> Value {
    json!({"name":"codex/example/review","cmd":"codex","agent":"codex",
        "state":"running","process_state":"running","activity":"idle","phase":"idle",
        "run_id":"run","conversation_id":"conversation","conversation_state":"active",
        "created":1,"reply_id":"reply-1","attention_signature":null,
        "read":{"conversation_id":"conversation","reply_id":null,"attention_signature":null},
        "process_work":{"count":0},"subagents_active":0,"optional_questions":{"count":0}})
}
fn project(raw: &Value) -> Projection {
    projection::project("", "fixture", raw, Map::new(), enabled(), false, false).unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    outbox: notifications::Outbox,
    host: Endpoint,
    runtime: Runtime,
    source: catalog::Source,
    seq: u64,
    cursor: i64,
}
impl Fixture {
    fn new() -> Self {
        Self::from_temp(tempfile::tempdir().unwrap())
    }
    fn from_temp(temp: tempfile::TempDir) -> Self {
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = id();
        let writer = id();
        let mut store = Store::open(&location, &epoch).unwrap();
        ledger::authorize(&mut store.db, &writer, &epoch).unwrap();
        let (outbox, host) = notifications::Outbox::new(Session {
            configuration: location.configuration,
            service_epoch: epoch.clone(),
            writer_epoch: writer,
            adapter: id(),
            host_epoch: id(),
            lease: id(),
            platform: Platform::Macos,
        })
        .unwrap();
        let source = catalog::Source {
            computer: String::new(),
            service_epoch: epoch.clone(),
            incarnation: 1,
            generation: 1,
        };
        let mut runtime = Runtime::new(&epoch);
        // The catalog receives no real heartbeat while these synchronous SQL
        // fixtures walk pages. Test logical time explicitly; slow filesystems
        // and unrelated parallel tests must not silently expire the source.
        runtime.clock = Some(Instant::now());
        Self {
            _temp: temp,
            store,
            outbox,
            host,
            runtime,
            source,
            seq: 0,
            cursor: 0,
        }
    }
    fn advance(&mut self, by: Duration) {
        *self.runtime.clock.as_mut().unwrap() += by;
    }
    fn focus(&mut self) {
        let now = self.runtime.now();
        self.runtime.suppress_foreground(now);
    }
    fn expire_focus(&mut self) {
        self.focus();
        self.advance(FOCUS_TTL + Duration::from_secs(1));
    }
    fn save(&mut self, raw: &Value, baseline: bool) {
        let session = state::session("", raw).unwrap();
        let previous = state::previous(&self.store.db, &session).unwrap();
        let projection =
            projection::project("", "fixture", raw, previous, enabled(), false, baseline).unwrap();
        state::save(
            &mut self.store.db,
            "",
            "key",
            state::Position {
                scope: "scope",
                row_hash: "hash",
                policy_revision: 0,
            },
            projection,
            baseline,
        )
        .unwrap();
    }
    fn intents(&self) -> i64 {
        self.store
            .db
            .query_row(
                "SELECT count(*) FROM desktop_intents WHERE json_extract(identity,'$.kind')='post'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn apply(&mut self, body: Body, connect: bool) {
        self.seq += 1;
        let frame = Frame {
            v: 1,
            seq: self.seq,
            body,
        };
        if connect {
            catalog::connect(&mut self.store.db, &self.source, &frame).unwrap();
        } else {
            catalog::apply(&mut self.store.db, &self.source, &frame).unwrap();
        }
        self.runtime
            .observe_packet(
                &self.store,
                &feed::Packet::Watch {
                    computer: String::new(),
                    generation: 1,
                    frame: Box::new(frame),
                },
            )
            .unwrap();
    }
    fn snapshot(&mut self, rows: Vec<Value>) {
        let hello = Hello {
            machine_id: Some("fixture".into()),
            host: "fixture".into(),
            hgs_version: "1.56.0".into(),
            stream: "stream".into(),
            heartbeat_ms: 10_000,
            max_frame_bytes: 1 << 20,
            topics: vec!["catalog".into()],
            limits: Default::default(),
        };
        self.apply(Body::Hello(hello), true);
        let pages = rows.len().div_ceil(catalog::PAGE_ROWS).max(1);
        let mut rows = rows.into_iter();
        for page in 0..pages {
            let items = rows
                .by_ref()
                .take(catalog::PAGE_ROWS)
                .map(|row| {
                    serde_json::to_value(CatalogItem {
                        key: zerus_watch_protocol::catalog_key(&row).unwrap(),
                        row,
                    })
                    .unwrap()
                })
                .collect();
            self.apply(
                Body::Snapshot(Snapshot {
                    topic: "catalog".into(),
                    page: page as u32,
                    key: None,
                    value: None,
                    last: page + 1 == pages,
                    cursor: 0,
                    items: Some(items),
                }),
                false,
            );
        }
    }

    fn delta(&mut self, raw: Value) {
        self.cursor += 1;
        self.apply(
            Body::Delta(Delta {
                topic: "catalog".into(),
                cursor: self.cursor,
                upsert: vec![CatalogItem {
                    key: zerus_watch_protocol::catalog_key(&raw).unwrap(),
                    row: raw,
                }],
                remove: vec![],
            }),
            false,
        );
    }
    fn tick(&mut self) {
        self.runtime.poll(&mut self.store, Some(&mut self.outbox));
        assert!(self.runtime.error.is_none(), "{}", self.runtime.status());
    }
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.tick();
        }
    }
}
#[test]
fn unread_reply_waits_for_all_original_work_without_consuming_its_identity() {
    let raw = row();
    let ready = project(&raw);
    assert_eq!(ready.candidates.len(), 1);
    assert!(!ready.candidates[0].consumed);
    for (field, value) in [
        ("process_work", json!({"count":1})),
        ("subagent_active_count", json!(1)),
        ("subagent_active_count", json!(-1)),
        (
            "coordination_wait",
            json!({"schema":1,"status":"ok","run_id":"run","conversation_id":"conversation","count":1}),
        ),
    ] {
        let mut busy = raw.clone();
        busy[field] = value;
        assert!(project(&busy).candidates.is_empty(), "{field}");
    }
    let mut unknown = raw.clone();
    unknown["process_work"] = json!({"count":-1});
    assert!(
        !project(&unknown).candidates[0].consumed,
        "existing policy treats unknown job count as no known background work"
    );
}

#[test]
fn bounded_hostless_backlog_requests_followup_and_rejection_removes_eligibility() {
    for packet in [
        feed::Packet::Offline {
            computer: String::new(),
            generation: 1,
        },
        feed::Packet::Keepalive {},
    ] {
        let mut f = Fixture::new();
        f.runtime.set_policy(enabled());
        let rows = (0..8)
            .map(|n| {
                let mut value = row();
                value["name"] = format!("codex/example/{n}").into();
                value["run_id"] = format!("run-{n}").into();
                value["conversation_id"] = format!("conversation-{n}").into();
                value["read"]["conversation_id"] = value["conversation_id"].clone();
                value
            })
            .collect();
        f.snapshot(rows);
        f.runtime.poll(&mut f.store, None);
        assert!(f.runtime.work_pending());
        assert_eq!(f.runtime.status()["pending_rows"], 4);
        assert_eq!(
            f.intents(),
            4,
            "four rows were consumed as baseline history"
        );
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT count(*) FROM desktop_intents WHERE blocked=0",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        f.runtime.reject_packet(&packet);
        assert!(!f.runtime.work_pending());
        assert_eq!(f.runtime.status()["sources"], 0);
        assert!(f.host.try_next().is_err());
    }
}

#[test]
fn explicit_attention_precedes_reply_even_with_background_work() {
    let mut raw = row();
    raw["activity"] = json!("waiting");
    raw["phase"] = json!("approval");
    raw["attention_id"] = json!("permission-1");
    raw["attention_signature"] = json!("a".repeat(64));
    raw["process_work"] = json!({"count":1});
    let p = project(&raw);
    assert_eq!(p.candidates.len(), 1);
    assert!(!p.candidates[0].consumed);
}
#[test]
fn shared_mute_and_suppressed_reply_are_permanently_consumed() {
    let mut f = Fixture::new();
    let mut raw = row();
    raw["notification_preferences"] = json!({"conversation_id":"conversation","muted":true,"revision":1,"suppressed":{"reply_id":null,"attention_signature":null,"optional_question_ids":[],"main_attention":null,"child_attention":{}}});
    f.save(&raw, false);
    raw["notification_preferences"]["muted"] = json!(false);
    f.save(&raw, false);
    let blocked: bool = f
        .store
        .db
        .query_row("SELECT blocked FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    assert!(blocked);
    assert_eq!(f.intents(), 1);
    raw["reply_id"] = json!("reply-2");
    raw["notification_preferences"]["suppressed"]["reply_id"] = json!("reply-2");
    f.save(&raw, false);
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM desktop_intents WHERE blocked=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}
#[test]
fn rename_resume_and_repeated_projection_reuse_original_payload_and_genuine_run() {
    let mut f = Fixture::new();
    let mut raw = row();
    f.save(&raw, false);
    let original: (String, String) = f
        .store
        .db
        .query_row("SELECT identity,payload FROM desktop_intents", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    raw["name"] = json!("codex/example/renamed");
    raw["run_id"] = json!("resumed-run");
    f.save(&raw, false);
    assert_eq!(f.intents(), 1);
    assert!(f
        .store
        .db
        .query_row("SELECT live FROM desktop_attention_edges", [], |r| r
            .get::<_, bool>(0))
        .unwrap());
    let after: (String, String) = f
        .store
        .db
        .query_row("SELECT identity,payload FROM desktop_intents", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(original, after);
    assert!(after.0.contains("\"run\":\"run\""));
    let saved: Map<String, Value> =
        state::previous(&f.store.db, &state::session("", &raw).unwrap()).unwrap();
    let token = saved
        .values()
        .find_map(|e| e.get("token").and_then(Value::as_str))
        .unwrap();
    assert_eq!(attention::target(token).unwrap()["run"], "run");
}
#[test]
fn optional_question_state_and_component_signatures_do_not_repost_main() {
    let mut f = Fixture::new();
    let mut raw = row();
    raw["reply_id"] = Value::Null;
    raw["phase"] = json!("approval");
    raw["attention_id"] = json!("permission");
    f.save(&raw, false);
    raw["optional_questions"] = json!({"count":1,"ids":["q-1"]});
    raw["attention_signature"] = json!("a".repeat(64));
    f.save(&raw, false);
    assert_eq!(
        f.intents(),
        2,
        "one main + one consumed optional, no repeated main banner"
    );
    assert_eq!(
        f.store
            .db
            .query_row(
                "SELECT count(*) FROM desktop_intents WHERE blocked=0",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
#[test]
fn optional_banner_survives_an_unrelated_update_and_is_withdrawn_when_answered() {
    let mut f = Fixture::new();
    let mut raw = row();
    raw["reply_id"] = Value::Null;
    raw["optional_questions"] = json!({"count":1,"ids":["q-1"]});
    f.save(&raw, false);
    f.save(&raw, false);
    assert_eq!(f.intents(), 1);
    assert!(!f
        .store
        .db
        .query_row("SELECT blocked FROM desktop_intents", [], |r| r
            .get::<_, bool>(0))
        .unwrap());
    raw["optional_questions"] = json!({"count":0,"ids":[]});
    f.save(&raw, false);
    assert!(!f
        .store
        .db
        .query_row("SELECT live FROM desktop_attention_edges", [], |r| r
            .get::<_, bool>(0))
        .unwrap());
}
#[test]
fn policy_off_focus_history_and_initial_busy_baseline_never_post_later() {
    for (policy, focused, baseline, field, value) in [
        (Policy::default(), false, false, "phase", json!("idle")),
        (enabled(), true, false, "phase", json!("idle")),
        (enabled(), false, true, "process_work", json!({"count":1})),
        (enabled(), false, false, "state", json!("archived")),
    ] {
        let mut f = Fixture::new();
        let mut raw = row();
        raw[field] = value;
        let p = projection::project("", "fixture", &raw, Map::new(), policy, focused, baseline)
            .unwrap();
        state::save(
            &mut f.store.db,
            "",
            "key",
            state::Position {
                scope: "scope",
                row_hash: "hash",
                policy_revision: 0,
            },
            p,
            baseline,
        )
        .unwrap();
        f.save(&row(), false);
        assert_eq!(f.intents(), 1);
        assert!(f
            .store
            .db
            .query_row("SELECT blocked FROM desktop_intents", [], |r| r
                .get::<_, bool>(0))
            .unwrap());
    }
}
#[test]
fn quota_and_mapping_failure_refuse_without_evicting_unknown_or_delivered_edges() {
    let mut f = Fixture::new();
    f.store
        .db
        .execute(
            "UPDATE desktop_attention_totals SET edges=?1",
            [state::MAX_EDGES],
        )
        .unwrap();
    let p = project(&row());
    assert!(state::save(
        &mut f.store.db,
        "",
        "key",
        state::Position {
            scope: "scope",
            row_hash: "hash",
            policy_revision: 0
        },
        p,
        false
    )
    .is_err());
    assert_eq!(f.intents(), 0);
    f.store
        .db
        .execute("UPDATE desktop_attention_totals SET edges=0", [])
        .unwrap();
    f.store.db.execute_batch("CREATE TRIGGER fixture_refuse BEFORE INSERT ON desktop_intents BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let p = project(&row());
    assert!(state::save(
        &mut f.store.db,
        "",
        "key",
        state::Position {
            scope: "scope",
            row_hash: "hash",
            policy_revision: 0
        },
        p,
        false
    )
    .is_err());
    f.store
        .db
        .execute_batch("DROP TRIGGER fixture_refuse;")
        .unwrap();
    let mut resumed = row();
    resumed["run_id"] = json!("new-run");
    f.save(&resumed, false);
    assert_eq!(f.intents(), 1);
    let identity: String = f
        .store
        .db
        .query_row("SELECT identity FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    assert!(identity.contains("\"run\":\"run\""));
}
#[test]
fn oversized_inputs_do_not_create_partial_intents() {
    let mut raw = row();
    raw["optional_questions"] = json!({"count":65,"ids":vec!["q";65]});
    assert!(projection::project("", "fixture", &raw, Map::new(), enabled(), false, false).is_err());
    raw = row();
    raw["run_id"] = json!("x".repeat(1025));
    assert!(projection::project("", "fixture", &raw, Map::new(), enabled(), false, false).is_err());
}
#[test]
fn owned_snapshot_is_baseline_then_delta_posts_once_and_offline_stops_admission() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    f.snapshot(vec![row()]);
    f.ticks(4);
    assert!(f.host.try_next().is_err());
    let mut next = row();
    next["reply_id"] = json!("reply-2");
    f.delta(next.clone());
    f.ticks(4);
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let command = dispatch.command().clone();
    assert!(matches!(command.action, Action::Post { .. }));
    dispatch.finish(Outcome::Unknown {
        code: zerus_client_core::desktop::notifications::Uncertainty::HostLost,
    });
    f.outbox.poll(&mut f.store, Instant::now());
    f.delta(next.clone());
    f.ticks(4);
    assert!(f.host.try_next().is_err(), "unknown never replayed");
    f.runtime.reject("");
    next["reply_id"] = json!("reply-3");
    f.cursor += 1;
    f.seq += 1;
    catalog::apply(
        &mut f.store.db,
        &f.source,
        &Frame {
            v: 1,
            seq: f.seq,
            body: Body::Delta(Delta {
                topic: "catalog".into(),
                cursor: f.cursor,
                upsert: vec![CatalogItem {
                    key: zerus_watch_protocol::catalog_key(&next).unwrap(),
                    row: next,
                }],
                remove: vec![],
            }),
        },
    )
    .unwrap();
    f.ticks(4);
    assert!(f.host.try_next().is_err());
}
#[test]
fn terminal_receipt_is_retained_before_exact_native_withdrawal_and_callback_tombstone() {
    let mut f = Fixture::new();
    f.save(&row(), false);
    let intent: String = f
        .store
        .db
        .query_row("SELECT id FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let command = dispatch.command().clone();
    assert!(matches!(
        state::withdrawal(&mut f.store.db, &intent).unwrap(),
        state::Withdrawal::Waiting
    ));
    let native_id = NativeId::Macos {
        request_id: attention::request_id(command.action.token()),
    };
    dispatch.finish(Outcome::Delivered {
        native_id: native_id.clone(),
    });
    f.outbox.poll(&mut f.store, Instant::now());
    let state::Withdrawal::Ready(withdrawal) = state::withdrawal(&mut f.store.db, &intent).unwrap()
    else {
        panic!("missing native mapping")
    };
    f.outbox
        .offer_intent(&mut f.store, &withdrawal, 1, Instant::now())
        .unwrap();
    let native = f.host.try_next().unwrap().begin().unwrap();
    assert!(
        matches!(&native.command().action,Action::Withdraw{token,native_id:n} if token==command.action.token()&&*n==native_id)
    );
    assert!(!ledger::current(&f.store.db, &intent, 1).unwrap());
}
#[test]
fn actor_turn_and_resident_pending_rows_stay_bounded() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut rows = Vec::new();
    for i in 0..20 {
        let mut r = row();
        r["conversation_id"] = json!(format!("conversation-{i}"));
        r["run_id"] = json!(format!("run-{i}"));
        rows.push(r);
    }
    f.snapshot(rows);
    f.tick();
    assert!(f.intents() <= ROWS_PER_TURN as i64);
    assert!(f.runtime.pending.as_ref().unwrap().rows.len() <= catalog::PAGE_ROWS);
    f.runtime.request_stop();
    assert!(f.runtime.drained());
    f.ticks(1);
    assert!(f.host.try_next().is_err());
}

#[test]
fn disabled_history_is_baselined_when_policy_enables_before_host_arrives() {
    let mut f = Fixture::new();
    f.snapshot(vec![row()]);
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    assert_eq!(f.intents(), 0);
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.runtime.set_policy(enabled());
    f.delta(row());
    f.ticks(4);
    assert_eq!(f.intents(), 1);
    assert!(f.host.try_next().is_err());
    let mut next = row();
    next["reply_id"] = json!("new-reply");
    f.delta(next);
    f.ticks(4);
    assert!(f.host.try_next().unwrap().begin().is_some());
}

#[test]
fn disabled_replies_do_not_grow_storage_and_enabling_baselines_current_history() {
    for was_enabled in [false, true] {
        let mut f = Fixture::new();
        if was_enabled {
            f.runtime.set_policy(enabled());
        }
        f.snapshot(vec![row()]);
        f.ticks(4);
        f.runtime.set_policy(Policy::default());
        let counts = |store: &Store| {
            store.db.query_row(
                "SELECT (SELECT count(*) FROM desktop_attention_edges), (SELECT count(*) FROM desktop_intents), (SELECT count(*) FROM desktop_attempts), (SELECT count(*) FROM desktop_attention_sessions), edge_bytes, state_bytes FROM desktop_attention_totals WHERE id=1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?)),
            ).unwrap()
        };
        let before = if was_enabled {
            counts(&f.store)
        } else {
            (0, 0, 0, 0, 0, 0)
        };
        let mut latest = row();
        for n in 0..50 {
            latest["reply_id"] = json!(format!("disabled-reply-{n}"));
            f.delta(latest.clone());
            for _ in 0..4 {
                f.runtime.poll(&mut f.store, None);
            }
        }
        assert_eq!(counts(&f.store), before);
        assert!(!f.runtime.work_pending());
        assert!(f.runtime.error.is_none());
        assert!(f.host.try_next().is_err());

        f.runtime.set_policy(enabled());
        // A fresh owned observation starts the new policy's baseline. None of
        // the disabled interval becomes a notification backlog.
        f.delta(latest);
        f.ticks(4);
        assert!(f.host.try_next().is_err());
        let mut next = row();
        next["reply_id"] = json!("after-enable");
        f.delta(next);
        f.ticks(4);
        let dispatch = f.host.try_next().unwrap().begin().unwrap();
        let target = attention::target(dispatch.command().action.token()).unwrap();
        assert_eq!(target["reply"], "after-enable");
    }
}
#[test]
fn host_arrival_revalidates_the_live_row_and_cannot_post_a_superseded_reply() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    let mut first = row();
    first["reply_id"] = json!("reply-first");
    f.delta(first);
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    let mut latest = row();
    latest["reply_id"] = json!("reply-latest");
    f.delta(latest);
    f.ticks(4);
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let target = attention::target(dispatch.command().action.token()).unwrap();
    assert_eq!(target["reply"], "reply-latest");
    assert!(f.host.try_next().is_err());
}
#[test]
fn policy_change_invalidates_queued_admission_even_if_reenabled_before_poll() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    f.delta(row());
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    f.runtime.set_policy(Policy::default());
    f.runtime.set_policy(enabled());
    f.ticks(8);
    assert!(f.host.try_next().is_err());
    let mut next = row();
    next["reply_id"] = json!("after-enable");
    f.delta(next);
    f.ticks(4);
    assert!(f.host.try_next().unwrap().begin().is_some());
}
#[test]
fn expired_original_observation_and_retention_gap_cannot_authorize_a_post() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    f.ticks(4);
    f.delta(row());
    f.advance(FRESH + Duration::from_secs(1));
    f.ticks(4);
    assert!(f.host.try_next().is_err());
    // A fresh owned callback can restore source eligibility, but starts from a
    // consumed baseline, rather than promoting the missing interval.
    let packet = feed::Packet::Watch {
        computer: String::new(),
        generation: 1,
        frame: Box::new(Frame {
            v: 1,
            seq: f.seq,
            body: Body::Heartbeat(zerus_watch_protocol::Heartbeat { at: 0.0 }),
        }),
    };
    f.runtime.observe_packet(&f.store, &packet).unwrap();
    f.ticks(4);
    assert!(f.host.try_next().is_err());
    let mut second = row();
    second["reply_id"] = json!("lost-reply");
    f.delta(second);
    f.store
        .db
        .execute("UPDATE desktop_catalog_change_views SET floor=head", [])
        .unwrap();
    f.ticks(8);
    assert!(f.host.try_next().is_err());
}
#[test]
fn retiring_an_unstarted_original_send_cancels_it_without_a_native_close() {
    let mut f = Fixture::new();
    f.save(&row(), false);
    let intent: String = f
        .store
        .db
        .query_row("SELECT id FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    f.outbox.retire_intent(&mut f.store, &intent).unwrap();
    assert!(f.host.try_next().unwrap().begin().is_none());
    f.outbox.poll(&mut f.store, Instant::now());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT state FROM desktop_attempts", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "refused"
    );
    assert!(f.host.try_next().is_err());
}
#[test]
fn live_projection_restores_exact_callback_mapping_without_reposting_history() {
    let mut f = Fixture::new();
    f.save(&row(), false);
    let intent: String = f
        .store
        .db
        .query_row("SELECT id FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.outbox
        .offer_intent(&mut f.store, &intent, 1, Instant::now())
        .unwrap();
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let token = dispatch.command().action.token().to_owned();
    let native_id = NativeId::Macos {
        request_id: attention::request_id(&token),
    };
    dispatch.finish(Outcome::Delivered {
        native_id: native_id.clone(),
    });
    f.outbox.poll(&mut f.store, Instant::now());
    let head = f.outbox.restore_live_head(&f.store).unwrap();
    let page = f.outbox.restore_live_page(&f.store, 0, head).unwrap();
    assert!(page.complete);
    assert_eq!(page.mappings.len(), 1);
    assert_eq!(page.mappings[0].token, token);
    assert_eq!(page.mappings[0].native_id, native_id);
    assert!(f.host.try_next().is_err());
    let mut read = row();
    read["read"]["reply_id"] = json!("reply-1");
    f.save(&read, false);
    let page = f.outbox.restore_live_page(&f.store, 0, head).unwrap();
    assert!(page.mappings.is_empty());
    assert!(!ledger::current(&f.store.db, &intent, 1).unwrap());
}

#[test]
fn host_arrival_waits_again_if_a_retained_reply_has_returned_to_background_work() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    f.delta(row());
    for _ in 0..4 {
        f.runtime.poll(&mut f.store, None);
    }
    let mut busy = row();
    busy["process_work"] = json!({"count":1});
    f.delta(busy);
    f.ticks(4);
    assert!(f.host.try_next().is_err());
    f.delta(row());
    f.ticks(4);
    assert!(f.host.try_next().unwrap().begin().is_some());
}
#[test]
fn unknown_reply_activity_retains_unread_identity_without_creating_current_work() {
    let mut unknown = row();
    unknown["activity"] = json!("unknown");
    assert!(project(&unknown).candidates.is_empty());
    let mut f = Fixture::new();
    f.save(&unknown, false);
    assert_eq!(f.intents(), 0);
    f.save(&row(), false);
    assert_eq!(f.intents(), 1);
    assert!(!f
        .store
        .db
        .query_row("SELECT blocked FROM desktop_intents", [], |r| r
            .get::<_, bool>(0))
        .unwrap());
}

#[test]
fn resume_removal_and_old_archived_rows_preserve_the_original_live_notice() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    f.ticks(4);
    f.delta(row());
    f.ticks(4);
    let dispatch = f.host.try_next().unwrap().begin().unwrap();
    let token = dispatch.command().action.token().to_owned();
    dispatch.finish(Outcome::Delivered {
        native_id: NativeId::Macos {
            request_id: attention::request_id(&token),
        },
    });
    f.outbox.poll(&mut f.store, Instant::now());
    let mut resumed = row();
    resumed["run_id"] = json!("resumed-run");
    resumed["name"] = json!("codex/example/renamed");
    f.cursor += 1;
    f.apply(
        Body::Delta(Delta {
            topic: "catalog".into(),
            cursor: f.cursor,
            upsert: vec![CatalogItem {
                key: zerus_watch_protocol::catalog_key(&resumed).unwrap(),
                row: resumed,
            }],
            remove: vec![zerus_watch_protocol::Removal {
                key: "run:run".into(),
                reason: None,
            }],
        }),
        false,
    );
    f.ticks(6);
    let mut historical = row();
    historical["state"] = json!("archived");
    historical["process_state"] = json!("exited");
    f.delta(historical);
    f.ticks(4);
    assert!(f.host.try_next().is_err());
    let head = f.outbox.restore_live_head(&f.store).unwrap();
    let mappings = f
        .outbox
        .restore_live_page(&f.store, 0, head)
        .unwrap()
        .mappings;
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].token, token);
    assert_eq!(attention::target(&mappings[0].token).unwrap()["run"], "run");
}
#[test]
fn schema_fourteen_migrates_an_existing_thirteen_without_changing_prior_effects() {
    let f = Fixture::new();
    crate::desktop::cache_maintenance::remove_from_downgraded_fixture(&f.store.db);
    let root = f.store.db.path().unwrap().to_owned();
    let count: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_intents", [], |r| r.get(0))
        .unwrap();
    f.store.db.execute_batch("DROP TABLE desktop_creation_clones; DROP TABLE desktop_creation_clone_budget; DROP TABLE desktop_notification_preferences_requests; DROP TABLE desktop_notification_preferences; DROP TABLE desktop_ownership_requests; DROP TABLE desktop_ownership; DROP INDEX desktop_attention_changed_key; DROP INDEX desktop_attention_catalog_identity; DROP TABLE desktop_attention_edges; DROP TABLE desktop_attention_sessions; DROP TABLE desktop_attention_totals; PRAGMA user_version=13;").unwrap();
    let location = Location {
        root: std::path::Path::new(&root).parent().unwrap().to_owned(),
        configuration: "a".repeat(64),
    };
    drop(f.store);
    let store = Store::open(&location, &id()).unwrap();
    assert_eq!(
        store
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::desktop::store::SCHEMA
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM desktop_intents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    assert_eq!(state::head(&store.db).unwrap(), 0);
}

fn fleet_rows(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let mut raw = row();
            raw["run_id"] = json!(format!("run-{i}"));
            raw["name"] = json!(format!("codex/example/session-{i}"));
            raw["conversation_id"] = json!(format!("conversation-{i}"));
            raw["read"]["conversation_id"] = raw["conversation_id"].clone();
            raw
        })
        .collect()
}
fn assert_single_reply(f: &mut Fixture, reply: &str, turns: usize) {
    let mut observed = false;
    for _ in 0..turns {
        f.tick();
        // This synchronous fixture is also the native receiver. Claim the
        // original work before the next SQL turn, not after the whole walk:
        // catalog logical time does not suspend the real outbox deadline.
        if let Ok(work) = f.host.try_next() {
            let dispatch = work.begin().expect("newly offered fixture work must admit");
            assert!(!observed, "duplicate reply post");
            assert!(matches!(dispatch.command().action, Action::Post { .. }));
            let target = attention::target(dispatch.command().action.token()).unwrap();
            assert_eq!(target["reply"], reply);
            assert!(f.host.try_next().is_err());
            observed = true;
        }
    }
    // Continue the complete original pass after the first post, so a late
    // duplicate is still a failure rather than an early successful return.
    assert!(
        observed,
        "expected reply was not posted within {turns} turns"
    );
}

#[test]
fn focus_refresh_never_restarts_a_multi_page_baseline_or_loses_a_later_reply() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    let revision = f.runtime.policy_revision;
    for _ in 0..100 {
        f.focus();
        f.tick();
        assert_eq!(f.runtime.policy_revision, revision);
    }
    assert!(
        f.runtime.sources[""].baselined,
        "focus must allow the original walk to finish"
    );
    f.expire_focus();
    f.tick();
    assert!(f.runtime.sources[""].baselined);
    let mut changed = rows[199].clone();
    changed["reply_id"] = json!("new-199");
    f.delta(changed);
    assert_single_reply(&mut f, "new-199", 400);
}

#[test]
fn initial_baseline_consumes_first_observation_before_admitting_later_replies() {
    for position in [100, 199] {
        let mut f = Fixture::new();
        f.runtime.set_policy(enabled());
        let rows = fleet_rows(200);
        f.snapshot(rows.clone());
        f.tick();
        assert!(!f.runtime.sources[""].baselined);
        let mut changed = rows[position].clone();
        changed["reply_id"] = json!(format!("new-{position}"));
        f.delta(changed);
        if position == 100 {
            // The queued page still has the old start value, so it can first
            // consume reply-1 and then admit the delta's genuinely new reply.
            assert_single_reply(&mut f, &format!("new-{position}"), 400);
        } else {
            // No before-image exists for this unread page. Conservatively
            // consume its first value; do not invent an empty start-state.
            f.ticks(400);
            assert!(f.host.try_next().is_err());
            let mut after = rows[position].clone();
            after["reply_id"] = json!("after-first-observation");
            f.delta(after);
            assert_single_reply(&mut f, "after-first-observation", 10);
        }
    }
}

#[test]
fn policy_baseline_preserves_a_later_page_reply_newer_than_its_start_revision() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.ticks(100);
    assert!(f.runtime.sources[""].baselined);
    f.runtime.set_policy(Policy::default());
    f.runtime.set_policy(enabled());
    settle_retirement(&mut f);
    f.tick();
    let mut changed = rows[199].clone();
    changed["reply_id"] = json!("after-policy");
    f.delta(changed);
    assert_single_reply(&mut f, "after-policy", 400);
}

#[test]
fn retention_gap_baseline_preserves_replies_after_its_new_start() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.ticks(100);
    let mut lost = rows[0].clone();
    lost["reply_id"] = json!("lost-interval");
    f.delta(lost);
    f.store
        .db
        .execute("UPDATE desktop_catalog_change_views SET floor=head", [])
        .unwrap();
    f.tick();
    f.tick();
    assert!(!f.runtime.sources[""].baselined);
    let mut changed = rows[199].clone();
    changed["reply_id"] = json!("after-gap");
    f.delta(changed);
    assert_single_reply(&mut f, "after-gap", 400);
}

#[test]
fn losing_journal_coverage_mid_baseline_resets_before_consuming_an_unproven_row() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.tick();
    let mut changed = rows[199].clone();
    changed["reply_id"] = json!("lost-interval");
    f.delta(changed);
    f.store
        .db
        .execute("UPDATE desktop_catalog_change_views SET floor=head", [])
        .unwrap();
    f.tick();
    assert_eq!(f.runtime.sources[""].since, 1);
    assert!(f.runtime.pending.is_none());
    // The lost interval is consumed, but a later edge is still observed.
    f.tick();
    let mut changed = rows[0].clone();
    changed["reply_id"] = json!("new-after-reset");
    f.delta(changed);
    assert_single_reply(&mut f, "new-after-reset", 400);
}

#[test]
fn a_reply_observed_while_focused_stays_consumed_after_focus_expires() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.ticks(100);
    f.focus();
    let mut changed = rows[199].clone();
    changed["reply_id"] = json!("seen-focused");
    f.delta(changed.clone());
    f.ticks(10);
    assert!(f.host.try_next().is_err());
    f.expire_focus();
    f.delta(changed.clone());
    f.ticks(10);
    assert!(f.host.try_next().is_err());
    changed["reply_id"] = json!("new-background");
    f.delta(changed);
    assert_single_reply(&mut f, "new-background", 10);
}

#[test]
fn an_offer_failure_is_explicitly_non_retryable_even_if_the_writer_is_repaired() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    f.ticks(4);
    ledger::revoke(&f.store.db, "fixture").unwrap();
    f.delta(row());
    f.runtime.poll(&mut f.store, Some(&mut f.outbox));
    let status = f.runtime.status();
    assert_eq!(status["stopped_on_error"], true);
    assert_eq!(status["error_retryable"], false);
    assert!(status["error"]
        .as_str()
        .unwrap()
        .contains("notification post offer failed"));
    assert!(f.host.try_next().is_err());
    let writer: String = f
        .store
        .db
        .query_row("SELECT epoch FROM desktop_writer WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    ledger::authorize(&mut f.store.db, &writer, &f.runtime.epoch).unwrap();
    f.runtime.poll(&mut f.store, Some(&mut f.outbox));
    assert_eq!(f.runtime.status()["error"], status["error"]);
    assert!(f.host.try_next().is_err());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn twenty_thousand_row_delta_stream_reports_expression_index_cost() {
    // Paired local samples, not a latency threshold or a native/process count.
    // Only this expression index differs; the new journal index is present in
    // both fixtures. Every catalog row is updated once in bounded native pages.
    for indexed in [false, true] {
        let mut f = Fixture::new();
        if !indexed {
            f.store
                .db
                .execute_batch("DROP INDEX desktop_attention_catalog_identity")
                .unwrap();
        }
        let rows = fleet_rows(20_000);
        f.snapshot(rows.clone());
        let started = Instant::now();
        for page in rows.chunks(catalog::PAGE_ROWS) {
            let upsert = page
                .iter()
                .cloned()
                .map(|mut row| {
                    row["fixture_delta"] = json!(1);
                    CatalogItem {
                        key: zerus_watch_protocol::catalog_key(&row).unwrap(),
                        row,
                    }
                })
                .collect();
            f.cursor += 1;
            f.apply(
                Body::Delta(Delta {
                    topic: "catalog".into(),
                    cursor: f.cursor,
                    upsert,
                    remove: Vec::new(),
                }),
                false,
            );
        }
        let elapsed = started.elapsed();
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM desktop_catalog_changes", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            20_000
        );
        assert_eq!(f.store.db.query_row("SELECT count(*) FROM desktop_catalog_rows r JOIN desktop_catalog_sources s ON s.visible=r.generation WHERE json_extract(r.json,'$.fixture_delta')=1",[],|r|r.get::<_,i64>(0)).unwrap(),20_000);
        println!(
            "attention expression-index={indexed}: 20000 delta rows / {} frames / {:.3} seconds",
            f.cursor,
            elapsed.as_secs_f64()
        );
    }
}

#[test]
fn initial_baseline_unrelated_field_change_does_not_post_an_old_unread_reply() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.tick();
    let mut unrelated = rows[150].clone();
    unrelated["created"] = json!(2);
    f.delta(unrelated);
    f.ticks(400);
    assert!(f.host.try_next().is_err());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn initial_baseline_unrelated_field_change_does_not_post_old_native_attention() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut rows = fleet_rows(200);
    rows[150]["reply_id"] = Value::Null;
    rows[150]["phase"] = json!("approval");
    rows[150]["attention_id"] = json!("old-permission");
    rows[150]["activity"] = json!("waiting");
    f.snapshot(rows.clone());
    f.tick();
    let mut unrelated = rows[150].clone();
    unrelated["created"] = json!(2);
    f.delta(unrelated);
    f.ticks(400);
    assert!(f.host.try_next().is_err());
    assert_eq!(
        f.store
            .db
            .query_row("SELECT count(*) FROM desktop_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn known_policy_baseline_unrelated_change_keeps_the_old_reply_consumed() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let rows = fleet_rows(200);
    f.snapshot(rows.clone());
    f.ticks(100);
    f.runtime.set_policy(Policy::default());
    f.runtime.set_policy(enabled());
    f.tick();
    let mut unrelated = rows[150].clone();
    unrelated["created"] = json!(2);
    f.delta(unrelated);
    f.ticks(400);
    assert!(f.host.try_next().is_err());
}
#[test]
fn an_empty_saved_tracker_is_known_state_and_allows_new_delta_edges() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut rows = fleet_rows(200);
    for raw in &mut rows {
        raw["reply_id"] = Value::Null;
        raw.as_object_mut().unwrap().remove("optional_questions");
    }
    f.snapshot(rows.clone());
    f.ticks(100);
    let session = state::session("", &rows[150]).unwrap();
    assert!(state::has_previous(&f.store.db, &session).unwrap());
    assert!(state::previous(&f.store.db, &session).unwrap().is_empty());
    f.runtime.set_policy(Policy::default());
    f.runtime.set_policy(enabled());
    settle_retirement(&mut f);
    f.tick();
    let mut changed = rows[150].clone();
    changed["reply_id"] = json!("first-new-reply");
    f.delta(changed);
    assert_single_reply(&mut f, "first-new-reply", 400);
}

#[cfg(unix)]
#[test]
fn symlinked_temp_parent_is_resolved_before_opening_private_sqlite() {
    let real = tempfile::tempdir().unwrap();
    let links = tempfile::tempdir().unwrap();
    let alias = links.path().join("temp-alias");
    std::os::unix::fs::symlink(real.path(), &alias).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("attention-")
        .tempdir_in(&alias)
        .unwrap();
    assert!(temp.path().starts_with(&alias));
    let f = Fixture::from_temp(temp);
    let path = std::path::Path::new(f.store.db.path().unwrap());
    assert!(path.starts_with(real.path().canonicalize().unwrap()));
    assert_eq!(path, path.canonicalize().unwrap());
}

#[test]
fn foreground_ttl_expires_at_the_logical_deadline_without_sleeping() {
    let mut f = Fixture::new();
    f.runtime.set_policy(enabled());
    let mut initial = row();
    initial["reply_id"] = Value::Null;
    f.snapshot(vec![initial]);
    f.ticks(4);
    f.focus();
    f.advance(FOCUS_TTL);
    f.delta(row());
    assert_single_reply(&mut f, "reply-1", 4);
}
