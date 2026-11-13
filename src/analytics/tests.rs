use super::*;
use model::*;
use serde_json::json;
use store::{Store, NIL_EPOCH};

const AT: i64 = 1_792_281_600_000; // 2026-10-18T00:00:00Z
const MACHINE: &str = "11111111-1111-4111-8111-111111111111";
fn enabled() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    crate::platform::restrict_dir(dir.path()).unwrap();
    let mut db = Store::open(dir.path(), true, true).unwrap().unwrap();
    db.configure(NIL_EPOCH, true, 90, AT - 86_400_000).unwrap();
    (dir, db)
}
fn event(id: &str, kind: Kind, at: i64) -> Event {
    Event {
        id: Identity::new(&[id]),
        scope: Identity::new(&["scope"]),
        correlation: Some(Identity::new(&["turn"])),
        provider: Provider::Codex,
        producer: Producer::Main,
        observed_ms: at,
        clock: Some(Clock {
            generation: Identity::new(&["process-generation"]),
            monotonic_ms: (at - AT + 86_400_000) as u64,
        }),
        kind,
    }
}
fn summary(db: &Store) -> Value {
    db.summary(MACHINE, "2026-10-17", "2026-10-19", AT + 60_000)
        .unwrap()
}
fn total(value: &Value, key: &str) -> u64 {
    value["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["counters"][key].as_u64())
        .sum()
}
fn usage(id: &str, at: i64, input: u64, output: u64, epoch: &str) -> Event {
    event(
        id,
        Kind::Usage {
            epoch: Identity::new(&[epoch]),
            counters: Tokens {
                input_tokens: Some(input),
                output_tokens: Some(output),
                ..Default::default()
            },
        },
        at,
    )
}

#[test]
fn absent_store_read_never_creates_or_enables_collection() {
    let dir = tempfile::tempdir().unwrap();
    let reply = handle(
        dir.path(),
        MACHINE,
        &json!({"schema":1,"action":"summary","from":"2026-10-01","through":"2026-10-18"}),
    )
    .unwrap();
    assert_eq!(reply["collection"]["enabled"], false);
    assert_eq!(reply["source_epoch"], NIL_EPOCH);
    assert!(Store::open_observer(&dir.path().join("analytics"))
        .unwrap()
        .is_none());
    assert!(!dir.path().join("analytics").exists());
}
#[test]
fn strict_read_contract_refuses_fields_dates_and_mutations() {
    let good = json!({"schema":1,"action":"summary","from":"2026-01-01","through":"2026-03-31"});
    assert!(validate_request(&good).is_ok());
    for change in [
        json!({"through":"2026-04-01"}),
        json!({"from":"2026-02-30"}),
        json!({"from":"0000-01-01"}),
        json!({"from":"2026-1-1"}),
        json!({"prompt":"private"}),
        json!({"schema":2}),
        json!({"action":"collect"}),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert!(validate_request(&bad).is_err(), "{bad}");
    }
    assert!(validate_request(
        &json!({"schema":1,"action":"purge","expected_epoch":NIL_EPOCH,"confirm":false})
    )
    .is_err());
}
#[test]
fn replay_has_one_count_and_conflicting_replay_is_refused() {
    let (_dir, mut db) = enabled();
    let first = event("one", Kind::TurnStarted, AT);
    assert!(db.record(&first, AT).unwrap());
    assert!(!db.record(&first, AT).unwrap());
    let mut changed = first.clone();
    changed.kind = Kind::TurnCompleted;
    assert!(db.record(&changed, AT).is_err());
    assert_eq!(total(&summary(&db), "turn_started"), 1);
    assert_eq!(total(&summary(&db), "turn_completed"), 0);
}
#[test]
fn independent_native_children_keep_distinct_counts() {
    let (_dir, mut db) = enabled();
    let parent = event("parent", Kind::TurnCompleted, AT);
    let mut child = event("child", Kind::TurnCompleted, AT);
    child.producer = Producer::Child;
    child.scope = Identity::new(&["child"]);
    db.record(&parent, AT).unwrap();
    db.record(&child, AT).unwrap();
    let value = summary(&db);
    assert_eq!(value["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(total(&value, "turn_completed"), 2);
    assert!(value["coverage"]["partial"].as_bool().unwrap());
}
#[test]
fn monotonic_work_splits_midnight_and_histogram_merges_by_counts() {
    let (_dir, mut db) = enabled();
    db.record(&event("start", Kind::TurnStarted, AT - 1000), AT)
        .unwrap();
    db.record(&event("stop", Kind::TurnCompleted, AT + 2000), AT + 2000)
        .unwrap();
    let value = summary(&db);
    let rows = value["buckets"].as_array().unwrap();
    assert_eq!(rows[0]["counters"]["work_ms"], 1000);
    assert_eq!(rows[1]["counters"]["work_ms"], 2000);
    let h = &rows[1]["histograms"]["work_ms"];
    assert_eq!(h["count"], 1);
    assert_eq!(h["sum"], 3000);
    assert_eq!(h["counts"][2], 1);
}
#[test]
fn restart_and_clock_jumps_never_become_measured_work() {
    for generation in [false, true] {
        let (_dir, mut db) = enabled();
        db.record(&event("start", Kind::TurnStarted, AT), AT)
            .unwrap();
        let mut end = event("end", Kind::TurnCompleted, AT + 30_000);
        if generation {
            end.clock.as_mut().unwrap().generation = Identity::new(&["new-process"]);
        } else {
            end.clock.as_mut().unwrap().monotonic_ms += 60_000;
        }
        db.record(&end, AT + 30_000).unwrap();
        let value = summary(&db);
        assert_eq!(total(&value, "work_ms"), 0);
        assert!(value["coverage"]["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("clock_discontinuity")));
    }
}
#[test]
fn matching_wait_excludes_that_turn_from_unqualified_work_duration() {
    let (_dir, mut db) = enabled();
    db.record(&event("start", Kind::TurnStarted, AT), AT)
        .unwrap();
    let mut wait = event("ask", Kind::WaitStarted(Interval::Approval), AT + 1000);
    wait.correlation = Some(Identity::new(&["request"]));
    db.record(&wait, AT + 1000).unwrap();
    let mut wrong = event(
        "wrong-answer",
        Kind::WaitResolved(Interval::Approval),
        AT + 2000,
    );
    wrong.correlation = Some(Identity::new(&["other-request"]));
    db.record(&wrong, AT + 2000).unwrap();
    assert_eq!(total(&summary(&db), "approval_wait_ms"), 0);
    let mut answer = event("answer", Kind::WaitResolved(Interval::Approval), AT + 3000);
    answer.correlation = wait.correlation;
    db.record(&answer, AT + 3000).unwrap();
    db.record(&event("end", Kind::TurnCompleted, AT + 4000), AT + 4000)
        .unwrap();
    let value = summary(&db);
    assert_eq!(total(&value, "approval_wait_ms"), 2000);
    assert_eq!(total(&value, "work_ms"), 0);
}
#[test]
fn cumulative_usage_baselines_deduplicate_order_and_reset_epochs() {
    let (_dir, mut db) = enabled();
    for event in [
        usage("initial", AT, 100, 40, "one"),
        usage("next", AT + 1000, 140, 50, "one"),
        usage("repeat", AT + 2000, 140, 50, "one"),
        usage("reset", AT + 3000, 9, 2, "two"),
        usage("late-old-epoch", AT + 2500, 900, 500, "one"),
        usage("after-reset", AT + 4000, 14, 6, "two"),
    ] {
        db.record(&event, AT + 5000).unwrap();
    }
    let value = summary(&db);
    assert_eq!(total(&value, "input_tokens"), 45);
    assert_eq!(total(&value, "output_tokens"), 14);
    assert!(value["buckets"][0]["counters"]
        .get("cache_tokens")
        .is_none());
}
#[test]
fn decreasing_component_refuses_entire_usage_delta() {
    let (_dir, mut db) = enabled();
    db.record(&usage("baseline", AT, 100, 100, "epoch"), AT)
        .unwrap();
    db.record(&usage("bad", AT + 1000, 200, 50, "epoch"), AT + 1000)
        .unwrap();
    db.record(&usage("good", AT + 2000, 250, 120, "epoch"), AT + 2000)
        .unwrap();
    assert_eq!(total(&summary(&db), "input_tokens"), 150);
    assert_eq!(total(&summary(&db), "output_tokens"), 20);
}
#[test]
fn disable_closes_spans_and_does_not_erase_existing_aggregates() {
    let (_dir, mut db) = enabled();
    db.record(&event("start", Kind::TurnStarted, AT), AT)
        .unwrap();
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.configure(&epoch, false, 90, AT + 1000).unwrap();
    assert!(!db
        .record(&event("off", Kind::TurnCompleted, AT + 2000), AT + 2000)
        .unwrap());
    let value = summary(&db);
    assert_eq!(total(&value, "turn_started"), 1);
    db.configure(value["source_epoch"].as_str().unwrap(), true, 90, AT + 3000)
        .unwrap();
    db.record(&event("after", Kind::TurnCompleted, AT + 4000), AT + 4000)
        .unwrap();
    assert_eq!(total(&summary(&db), "work_ms"), 0);
}
#[test]
fn purge_rotates_epoch_and_old_delayed_events_cannot_revive_history() {
    let (_dir, mut db) = enabled();
    let event = event("old", Kind::DeliverySubmitted, AT);
    db.record(&event, AT).unwrap();
    let old = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.purge(&old, AT + 1000).unwrap();
    assert!(!db.record(&event, AT + 2000).unwrap());
    let value = summary(&db);
    assert_ne!(value["source_epoch"], old);
    assert_eq!(total(&value, "delivery_submitted"), 0);
    assert!(db.purge(&old, AT + 2000).is_err());
}
#[test]
fn dedup_retention_cannot_recount_expired_events() {
    let (_dir, mut db) = enabled();
    let old = event("old", Kind::DeliverySubmitted, AT);
    db.record(&old, AT).unwrap();
    let later = AT + 15 * 86_400_000;
    assert!(!db.record(&old, later).unwrap());
    db.record(&event("new", Kind::DeliverySubmitted, later), later)
        .unwrap();
    assert_eq!(
        total(
            &db.summary(MACHINE, "2026-10-18", "2026-11-03", later)
                .unwrap(),
            "delivery_submitted"
        ),
        2
    );
}
#[test]
fn summaries_are_read_only_and_never_export_local_dedup_identity() {
    let (dir, mut db) = enabled();
    db.record(
        &event("private-native-identity", Kind::DeliverySubmitted, AT),
        AT,
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(dir.path().join("analytics.sqlite3")).unwrap();
    let db = Store::open(dir.path(), false, false).unwrap().unwrap();
    let result = summary(&db).to_string();
    drop(db);
    assert_eq!(
        before,
        std::fs::read(dir.path().join("analytics.sqlite3")).unwrap()
    );
    assert!(!result.contains("private-native-identity"));
    assert!(!before.windows(23).any(|s| s == b"private-native-identity"));
}
#[test]
fn raw_hook_content_is_never_stored_and_idless_hooks_are_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = Utc::now().timestamp_millis();
    let mut db = Store::open(&root.join("analytics"), true, true)
        .unwrap()
        .unwrap();
    db.configure(NIL_EPOCH, true, 90, now - 1).unwrap();
    drop(db);
    let record = json!({"agent":"codex","conversation_id":"synthetic-conversation","run_id":"run","process_start":"start"});
    adapters::hook(
        root,
        &record,
        &json!({"hook_event_name":"PermissionRequest","tool_name":"Bash","session_id":"synthetic-conversation","prompt":"NEVER_STORE_PRIVATE_TEXT","cwd":"/synthetic/private","tool_input":{"command":"NEVER_STORE_PRIVATE_TEXT"}}),
    );
    let db = Store::open(&root.join("analytics"), false, false)
        .unwrap()
        .unwrap();
    let today = Utc::now().date_naive().to_string();
    let result = db.summary(MACHINE, &today, &today, now + 1).unwrap();
    assert_eq!(total(&result, "turn_started"), 0);
    assert!(result["coverage"]["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("missing_identity")));
    let bytes = std::fs::read(root.join("analytics/analytics.sqlite3")).unwrap();
    assert!(!bytes.windows(24).any(|s| s == b"NEVER_STORE_PRIVATE_TEXT"));
}

#[test]
fn replay_with_another_observation_clock_remains_one_native_event() {
    let (_dir, mut db) = enabled();
    let first = event("native-event", Kind::TurnStarted, AT);
    assert!(db.record(&first, AT).unwrap());
    let mut replay = first.clone();
    replay.clock = None;
    assert!(!db.record(&replay, AT + 1000).unwrap());
    replay.clock = Some(Clock {
        generation: Identity::new(&["later-process"]),
        monotonic_ms: 7,
    });
    assert!(!db.record(&replay, AT + 2000).unwrap());
    assert_eq!(total(&summary(&db), "turn_started"), 1);
}

#[test]
fn approval_resolution_requires_the_exact_unconsumed_owner_request() {
    let (_dir, mut db) = enabled();
    db.record(
        &event("ordinary-tool", Kind::WaitResolved(Interval::Approval), AT),
        AT,
    )
    .unwrap();
    assert_eq!(total(&summary(&db), "approval_resolved"), 0);
    db.record(
        &event("ask", Kind::WaitStarted(Interval::Approval), AT + 1000),
        AT + 1000,
    )
    .unwrap();
    let mut wrong = event(
        "wrong-owner",
        Kind::WaitResolved(Interval::Approval),
        AT + 2000,
    );
    wrong.scope = Identity::new(&["other-owner"]);
    db.record(&wrong, AT + 2000).unwrap();
    assert_eq!(total(&summary(&db), "approval_resolved"), 0);
    assert_eq!(summary(&db)["unresolved_waits"]["observed"], 1);
    db.record(
        &event(
            "permission-result",
            Kind::WaitResolved(Interval::Approval),
            AT + 3000,
        ),
        AT + 3000,
    )
    .unwrap();
    db.record(
        &event(
            "post-tool-use",
            Kind::WaitResolved(Interval::Approval),
            AT + 4000,
        ),
        AT + 4000,
    )
    .unwrap();
    let value = summary(&db);
    assert_eq!(total(&value, "approval_resolved"), 1);
    assert_eq!(total(&value, "approval_wait_ms"), 2000);
    assert_eq!(value["unresolved_waits"]["observed"], 0);
}

#[test]
fn empty_usage_has_no_baseline_or_token_support() {
    let (dir, mut db) = enabled();
    let epoch = Identity::new(&["usage"]);
    for (id, kind) in [
        (
            "empty-one",
            Kind::Usage {
                epoch: epoch.clone(),
                counters: Tokens::default(),
            },
        ),
        (
            "empty-two",
            Kind::Usage {
                epoch: epoch.clone(),
                counters: Tokens::default(),
            },
        ),
        (
            "empty-request",
            Kind::RequestUsage {
                counters: Tokens::default(),
            },
        ),
    ] {
        db.record(&event(id, kind, AT), AT).unwrap();
    }
    let raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM usage", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let supported = |db: &Store| {
        summary(db)["coverage"]["providers"]["codex"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("tokens"))
    };
    assert!(!supported(&db));
    db.record(&usage("baseline", AT + 1000, 10, 5, "usage"), AT + 1000)
        .unwrap();
    assert!(!supported(&db));
    db.record(&usage("comparable", AT + 2000, 14, 8, "usage"), AT + 2000)
        .unwrap();
    assert!(supported(&db));
    assert_eq!(total(&summary(&db), "input_tokens"), 4);
}

#[test]
fn exact_request_usage_counts_once_and_accepts_only_nondecreasing_updates() {
    let (_dir, mut db) = enabled();
    let request = |id: &str, at, input, output| {
        event(
            id,
            Kind::RequestUsage {
                counters: Tokens {
                    input_tokens: input,
                    output_tokens: output,
                    ..Default::default()
                },
            },
            at,
        )
    };
    for value in [
        request("initial", AT, Some(100), None),
        request("repeat", AT, Some(100), None),
        request("updated", AT, Some(120), Some(5)),
        request("bad", AT + 1000, Some(110), Some(10)),
        request("good", AT + 2000, Some(130), Some(7)),
        request("late", AT + 1000, Some(900), Some(900)),
    ] {
        db.record(&value, AT + 3000).unwrap();
    }
    let mut other = request("other-message", AT + 3000, Some(20), Some(2));
    other.scope = Identity::new(&["other-request"]);
    db.record(&other, AT + 3000).unwrap();
    let value = summary(&db);
    assert_eq!(total(&value, "input_tokens"), 150);
    assert_eq!(total(&value, "output_tokens"), 9);
    assert!(value["coverage"]["providers"]["codex"]["supported"]
        .as_array()
        .unwrap()
        .contains(&json!("tokens")));
}

#[test]
fn disabled_background_maintenance_expires_data_in_bounded_batches() {
    let (dir, mut db) = enabled();
    db.record(&event("aggregate", Kind::DeliverySubmitted, AT), AT)
        .unwrap();
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.configure(&epoch, false, 90, AT + 1000).unwrap();
    let mut raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let tx = raw.transaction().unwrap();
    for i in 0u64..1004 {
        let mut identity = [0u8; 32];
        identity[..8].copy_from_slice(&i.to_be_bytes());
        tx.execute(
            "INSERT INTO events VALUES(?,?,?)",
            rusqlite::params![identity.as_slice(), AT, identity.as_slice()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let count = || {
        raw.query_row("SELECT event_rows FROM metadata WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let later = AT + 15 * 86_400_000;
    db.summary(MACHINE, "2026-10-18", "2026-11-03", later)
        .unwrap();
    assert_eq!(count(), 1005, "Read must not prune storage");
    db.maintain(later).unwrap();
    assert_eq!(count(), 5);
    assert!(!db.enabled().unwrap());
    db.maintain(later).unwrap();
    assert_eq!(count(), 0);
    assert_eq!(
        raw.query_row("SELECT count(*) FROM daily", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.maintain(AT + 91 * 86_400_000).unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM daily", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn cursor_storage_is_private_bounded_and_cleared_on_disable_and_purge() {
    let (dir, mut db) = enabled();
    let key = Identity::new(&["synthetic-stream"]);
    let fingerprint = Identity::new(&["synthetic-file-version"]);
    db.save_cursor(&key, &fingerprint, 64, AT).unwrap();
    assert_eq!(db.cursor(&key).unwrap().unwrap().0 .0, fingerprint.0);
    assert_eq!(db.cursor(&key).unwrap().unwrap().1, 64);
    let mut raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let tx = raw.transaction().unwrap();
    for i in 0u64..9999 {
        let mut identity = [0u8; 32];
        identity[..8].copy_from_slice(&i.to_be_bytes());
        tx.execute(
            "INSERT INTO cursors VALUES(?,?,?,?)",
            rusqlite::params![identity.as_slice(), fingerprint.0.as_slice(), 0, AT],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert!(db
        .save_cursor(&Identity::new(&["overflow-stream"]), &fingerprint, 64, AT)
        .is_err());
    db.save_cursor(&key, &fingerprint, 128, AT).unwrap();
    assert_eq!(db.cursor(&key).unwrap().unwrap().1, 128);
    assert_eq!(
        raw.query_row("SELECT cursor_rows FROM metadata WHERE id=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        10_000
    );
    assert!(summary(&db)["coverage"]["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("quota_exhausted")));
    db.maintain(AT + 15 * 86_400_000).unwrap();
    assert_eq!(
        raw.query_row("SELECT cursor_rows FROM metadata WHERE id=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        9000
    );
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.configure(&epoch, false, 90, AT + 15 * 86_400_000)
        .unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM cursors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.save_cursor(&key, &fingerprint, 10, AT).unwrap();
    assert!(db.cursor(&key).unwrap().is_none());
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.configure(&epoch, true, 90, AT + 15 * 86_400_000 + 1000)
        .unwrap();
    db.save_cursor(&key, &fingerprint, 10, AT + 15 * 86_400_000 + 1000)
        .unwrap();
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.purge(&epoch, AT + 15 * 86_400_000 + 2000).unwrap();
    assert!(db.cursor(&key).unwrap().is_none());
}

#[test]
fn purge_scrubs_dedup_identity_from_database_pages_after_reopen() {
    let (dir, mut db) = enabled();
    let value = event("private-event-to-scrub", Kind::DeliverySubmitted, AT);
    db.record(&value, AT).unwrap();
    drop(db);
    let path = dir.path().join("analytics.sqlite3");
    assert!(std::fs::read(&path)
        .unwrap()
        .windows(32)
        .any(|bytes| bytes == value.id.0));
    let mut db = Store::open(dir.path(), false, true).unwrap().unwrap();
    let epoch = summary(&db)["source_epoch"].as_str().unwrap().to_owned();
    db.purge(&epoch, AT + 1000).unwrap();
    drop(db);
    assert!(!std::fs::read(&path)
        .unwrap()
        .windows(32)
        .any(|bytes| bytes == value.id.0));
    assert!(!path.with_file_name("analytics.sqlite3-journal").exists());
}

#[test]
fn event_quota_refuses_new_ids_without_partial_aggregation() {
    let (dir, mut db) = enabled();
    let mut raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let tx = raw.transaction().unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO events VALUES(?,?,?)").unwrap();
        for i in 0..store::MAX_EVENTS {
            let mut identity = [0u8; 32];
            identity[..8].copy_from_slice(&i.to_be_bytes());
            insert
                .execute(rusqlite::params![
                    identity.as_slice(),
                    AT,
                    identity.as_slice()
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    let overflow = event("over-event-quota", Kind::DeliverySubmitted, AT);
    assert!(!db.record(&overflow, AT).unwrap());
    let count = || {
        raw.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(count(), store::MAX_EVENTS);
    assert_eq!(total(&summary(&db), "delivery_submitted"), 0);
    assert!(summary(&db)["coverage"]["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("quota_exhausted")));
    raw.execute("DELETE FROM events WHERE id=?", [[0u8; 32].as_slice()])
        .unwrap();
    assert!(db.record(&overflow, AT).unwrap());
    assert_eq!(count(), store::MAX_EVENTS);
    assert_eq!(total(&summary(&db), "delivery_submitted"), 1);
    assert!(
        std::fs::metadata(dir.path().join("analytics.sqlite3"))
            .unwrap()
            .len()
            <= 128 * 1024 * 1024
    );
}

#[test]
fn retired_usage_epoch_cannot_reappear_with_new_timestamps_and_recount() {
    let (_dir, mut db) = enabled();
    for value in [
        usage("a-baseline", AT, 100, 10, "a"),
        usage("a-next", AT + 1000, 200, 20, "a"),
        usage("b-baseline", AT + 2000, 50, 5, "b"),
        usage("a-late-baseline", AT + 3000, 150, 15, "a"),
        usage("a-late-next", AT + 4000, 200, 20, "a"),
        usage("b-next", AT + 5000, 60, 6, "b"),
    ] {
        db.record(&value, AT + 6000).unwrap();
    }
    let value = summary(&db);
    assert_eq!(total(&value, "input_tokens"), 110);
    assert_eq!(total(&value, "output_tokens"), 11);
}

#[test]
fn cumulative_usage_accepts_same_millisecond_monotone_components() {
    let (_dir, mut db) = enabled();
    for value in [
        usage("first", AT, 100, 10, "a"),
        usage("second", AT, 120, 12, "a"),
        usage("same", AT, 120, 12, "a"),
        usage("old", AT, 110, 11, "a"),
        usage("next", AT, 130, 13, "a"),
    ] {
        db.record(&value, AT).unwrap();
    }
    assert_eq!(total(&summary(&db), "input_tokens"), 30);
    assert_eq!(total(&summary(&db), "output_tokens"), 3);
}

#[test]
fn usage_epoch_quota_preserves_current_baseline_and_bounded_expiry() {
    let (dir, mut db) = enabled();
    let initial = usage("current-baseline", AT, 100, 10, "current");
    db.record(&initial, AT).unwrap();
    let mut raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let tx = raw.transaction().unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO usage VALUES(?,?,?,?,0)").unwrap();
        for i in 0u64..9999 {
            let mut epoch = [0u8; 32];
            epoch[..8].copy_from_slice(&i.to_be_bytes());
            insert
                .execute(rusqlite::params![
                    initial.scope.0.as_slice(),
                    epoch.as_slice(),
                    AT,
                    "{}"
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    db.record(&usage("new-epoch", AT + 1000, 900, 90, "new"), AT + 1000)
        .unwrap();
    db.record(
        &usage("current-next", AT + 2000, 120, 12, "current"),
        AT + 2000,
    )
    .unwrap();
    assert_eq!(total(&summary(&db), "input_tokens"), 20);
    assert_eq!(
        raw.query_row("SELECT usage_rows FROM metadata WHERE id=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        10_000
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM usage WHERE active=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    raw.execute(
        "UPDATE usage SET at=? WHERE active=0",
        [AT - 15 * 86_400_000],
    )
    .unwrap();
    db.maintain(AT + 2000).unwrap();
    assert_eq!(
        raw.query_row("SELECT usage_rows FROM metadata WHERE id=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        9000
    );
    let epoch: Vec<u8> = raw
        .query_row("SELECT epoch FROM usage WHERE active=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(epoch, Identity::new(&["current"]).0);
}

#[test]
fn exact_question_adapter_uses_native_request_and_durable_answer_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let setup_at = Utc::now().timestamp_millis();
    let mut db = Store::open(&root.join("analytics"), true, true)
        .unwrap()
        .unwrap();
    db.configure(NIL_EPOCH, true, 90, setup_at - 5000).unwrap();
    drop(db);
    let record = json!({"agent":"claude","conversation_id":"synthetic-conversation","run_id":"run","process_start":"process"});
    // Disk setup must not age the live hook's two-second observation window.
    let now = Utc::now().timestamp_millis();
    let timestamp = chrono::DateTime::from_timestamp_millis(now - 1000)
        .unwrap()
        .to_rfc3339();
    let ask = json!({"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_use_id":"exact-question","timestamp":timestamp,
        "tool_input":{"questions":[{"question":"PRIVATE_QUESTION_BODY"}]}});
    adapters::hook(root, &record, &ask);
    let answer = json!({"hook_event_name":"QuestionAnswered","tool_call_id":"exact-question","at":(now - 500) as f64 / 1000.0,
        "answer":"PRIVATE_ANSWER_BODY"});
    adapters::hook(root, &record, &answer);
    adapters::hook(root, &record, &answer);
    adapters::hook(
        root,
        &record,
        &json!({"hook_event_name":"PostToolUse","tool_use_id":"ordinary-tool","timestamp":timestamp,"tool_response":"PRIVATE_TOOL_BODY"}),
    );
    let db = Store::open(&root.join("analytics"), false, false)
        .unwrap()
        .unwrap();
    let today = Utc::now().date_naive();
    let from = (today - chrono::Days::new(1)).to_string();
    let value = db.summary(MACHINE, &from, &today.to_string(), now).unwrap();
    assert_eq!(total(&value, "input_requested"), 1);
    assert_eq!(total(&value, "input_resolved"), 1);
    assert_eq!(total(&value, "approval_resolved"), 0);
    assert_eq!(value["unresolved_waits"]["observed"], 0);
    let bytes = std::fs::read(root.join("analytics/analytics.sqlite3")).unwrap();
    for private in [
        b"PRIVATE_QUESTION_BODY".as_slice(),
        b"PRIVATE_ANSWER_BODY",
        b"PRIVATE_TOOL_BODY",
    ] {
        assert!(!bytes.windows(private.len()).any(|window| window == private));
    }
}

#[test]
fn stale_wait_hook_replays_never_manufacture_measured_duration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = Utc::now().timestamp_millis();
    let mut db = Store::open(&root.join("analytics"), true, true)
        .unwrap()
        .unwrap();
    db.configure(NIL_EPOCH, true, 90, now - 30_000).unwrap();
    drop(db);
    let record = json!({"agent":"claude","conversation_id":"synthetic-conversation","run_id":"run","process_start":"process"});
    let timestamp = chrono::DateTime::from_timestamp_millis(now - 10_000)
        .unwrap()
        .to_rfc3339();
    adapters::hook(
        root,
        &record,
        &json!({"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_use_id":"stale-question","timestamp":timestamp}),
    );
    adapters::hook(
        root,
        &record,
        &json!({"hook_event_name":"QuestionAnswered","tool_call_id":"stale-question","at":(now - 9000) as f64 / 1000.0}),
    );
    let db = Store::open(&root.join("analytics"), false, false)
        .unwrap()
        .unwrap();
    let today = Utc::now().date_naive();
    let value = db
        .summary(
            MACHINE,
            &(today - chrono::Days::new(1)).to_string(),
            &today.to_string(),
            now,
        )
        .unwrap();
    assert_eq!(total(&value, "input_requested"), 1);
    assert_eq!(total(&value, "input_resolved"), 0);
    assert_eq!(total(&value, "input_wait_ms"), 0);
    assert!(value["coverage"]["providers"]["claude"]["unsupported"]
        .as_array()
        .unwrap()
        .contains(&json!("input_wait_ms")));
}

#[test]
fn typed_failure_invalidates_only_its_observed_owner_work_interval() {
    for case in 0..4 {
        let (_dir, mut db) = enabled();
        db.record(&event("start", Kind::TurnStarted, AT), AT)
            .unwrap();
        let mut failure = event("failure", Kind::Failure(Failure::Network), AT + 1000);
        match case {
            1 => failure.scope = Identity::new(&["another-owner"]),
            2 => failure.correlation = Some(Identity::new(&["another-turn"])),
            3 => failure.correlation = None,
            _ => (),
        }
        db.record(&failure, AT + 1000).unwrap();
        db.record(
            &event("complete", Kind::TurnCompleted, AT + 2000),
            AT + 2000,
        )
        .unwrap();
        let value = summary(&db);
        assert_eq!(total(&value, "turn_completed"), 1);
        assert_eq!(total(&value, "failure_network"), 1);
        assert_eq!(
            total(&value, "work_ms"),
            if case == 1 || case == 2 { 2000 } else { 0 },
            "case {case}"
        );
    }
}

#[test]
fn native_clock_requires_generation_and_supported_platform_time() {
    let record = json!({"run_id":"synthetic-run","process_start":"synthetic-generation"});
    let first = adapters::clock(&record);
    #[cfg(any(unix, windows))]
    {
        let first = first.expect("supported native clock must be available");
        let second = adapters::clock(&record).unwrap();
        assert_eq!(first.generation.0, second.generation.0);
        assert!(second.monotonic_ms >= first.monotonic_ms);
        let changed = adapters::clock(
            &json!({"run_id":"another-run","process_start":"synthetic-generation"}),
        )
        .unwrap();
        assert_ne!(first.generation.0, changed.generation.0);
        let changed = adapters::clock(
            &json!({"run_id":"synthetic-run","process_start":"another-generation"}),
        )
        .unwrap();
        assert_ne!(first.generation.0, changed.generation.0);
    }
    #[cfg(not(any(unix, windows)))]
    assert!(first.is_none());
    for missing in [
        json!({}),
        json!({"run_id":"synthetic-run"}),
        json!({"process_start":"synthetic-generation"}),
    ] {
        assert!(adapters::clock(&missing).is_none());
    }
}

#[test]
fn wait_counts_and_durations_require_matching_clock_evidence() {
    for case in 0..5 {
        let (_dir, mut db) = enabled();
        let mut ask = event("ask", Kind::WaitStarted(Interval::Input), AT);
        let mut answer = event("answer", Kind::WaitResolved(Interval::Input), AT + 1000);
        match case {
            1 => {
                ask.clock = None;
                answer.clock = None;
            }
            2 => {
                answer.clock.as_mut().unwrap().generation = Identity::new(&["restarted"]);
            }
            // Model suspend: native elapsed excludes sleep; UTC has moved ahead.
            3 => {
                answer.observed_ms += 6000;
            }
            // Merely having a clock never supplies a matching native request.
            4 => {
                answer.correlation = Some(Identity::new(&["foreign-request"]));
            }
            _ => (),
        }
        db.record(&ask, AT).unwrap();
        db.record(&answer, AT + 7000).unwrap();
        db.record(&answer, AT + 7000).unwrap();
        let value = summary(&db);
        assert_eq!(total(&value, "input_requested"), 1, "case {case}");
        assert_eq!(
            total(&value, "input_resolved"),
            u64::from(!matches!(case, 1 | 4)),
            "case {case}"
        );
        assert_eq!(
            total(&value, "input_wait_ms"),
            if case == 0 { 1000 } else { 0 },
            "case {case}"
        );
        assert_eq!(value["unresolved_waits"]["observed"], u64::from(case == 4));
    }
}

#[test]
fn native_batch_commits_events_and_cursor_together_or_rolls_back() {
    for case in 0..3 {
        let (dir, mut db) = enabled();
        let reader = Store::open(dir.path(), false, false).unwrap().unwrap();
        let source = Identity::new(&["source"]);
        let fingerprint = Identity::new(&["file"]);
        let mut batch = db.native_batch(&source, AT).unwrap().unwrap();
        assert!(batch
            .record(&event("batch-event", Kind::TurnCompleted, AT), AT)
            .unwrap());
        assert_eq!(total(&summary(&reader), "turn_completed"), 0);
        assert!(reader.cursor(&source).unwrap().is_none());
        match case {
            0 => {
                batch.save_cursor(&source, &fingerprint, 123, AT).unwrap();
                batch.commit().unwrap();
            }
            1 => {
                drop(batch);
            }
            2 => {
                assert!(batch
                    .save_cursor(&source, &fingerprint, u64::MAX, AT)
                    .is_err());
                assert!(batch.commit().is_err());
            }
            _ => unreachable!(),
        }
        assert_eq!(
            total(&summary(&reader), "turn_completed"),
            u64::from(case == 0)
        );
        assert_eq!(
            reader.cursor(&source).unwrap().map(|(_, offset)| offset),
            if case == 0 { Some(123) } else { None }
        );
    }
}

#[test]
fn native_batch_rejected_event_rolls_back_its_partial_counters_and_continues() {
    let (_dir, mut db) = enabled();
    let source = Identity::new(&["source"]);
    let mut batch = db.native_batch(&source, AT).unwrap().unwrap();
    let first = event("first", Kind::TurnCompleted, AT);
    assert!(batch.record(&first, AT).unwrap());
    let mut conflict = first.clone();
    conflict.observed_ms += 1;
    assert!(batch.record(&conflict, AT + 1).is_err());
    let mut invalid = event("invalid-clock", Kind::TurnStarted, AT);
    invalid.clock.as_mut().unwrap().monotonic_ms = MAX_VALUE + 1;
    assert!(batch.record(&invalid, AT).is_err());
    batch.note_gap(Gap::Observation, AT).unwrap();
    assert!(batch
        .record(&event("last", Kind::TurnCompleted, AT), AT)
        .unwrap());
    batch
        .save_cursor(&source, &Identity::new(&["file"]), 99, AT)
        .unwrap();
    batch.commit().unwrap();
    let value = summary(&db);
    assert_eq!(total(&value, "turn_completed"), 2);
    assert_eq!(total(&value, "turn_started"), 0);
    assert_eq!(db.cursor(&source).unwrap().unwrap().1, 99);
    // Failed event identity was rolled back along with its counters.
    invalid.clock.as_mut().unwrap().monotonic_ms = 100;
    assert!(db.record(&invalid, AT).unwrap());
    assert_eq!(total(&summary(&db), "turn_started"), 1);
}

#[test]
fn native_batch_busy_and_cursor_quota_fail_before_accepting_events() {
    let (dir, mut db) = enabled();
    let mut raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let source = Identity::new(&["source"]);
    let lock = raw
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    assert!(db.native_batch(&source, AT).is_err());
    lock.rollback().unwrap();
    raw.execute("UPDATE metadata SET cursor_rows=10000 WHERE id=1", [])
        .unwrap();
    assert!(db.native_batch(&source, AT).is_err());
    assert!(db.cursor(&source).unwrap().is_none());
    assert!(summary(&db)["coverage"]["gaps"]
        .as_array()
        .unwrap()
        .contains(&json!("quota_exhausted")));
    raw.execute("UPDATE metadata SET cursor_rows=0 WHERE id=1", [])
        .unwrap();
    assert!(db.native_batch(&source, AT).unwrap().is_some());
}

#[test]
fn unknown_record_error_keeps_the_batch_poisoned_after_successful_cleanup() {
    let (dir, mut db) = enabled();
    let seed = event("seed", Kind::TurnCompleted, AT);
    db.record(&seed, AT).unwrap();
    let raw = rusqlite::Connection::open(dir.path().join("analytics.sqlite3")).unwrap();
    let payload: String = raw
        .query_row(
            "SELECT payload FROM daily WHERE provider='codex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    raw.execute(
        "UPDATE daily SET payload='invalid-json' WHERE provider='codex'",
        [],
    )
    .unwrap();
    let source = Identity::new(&["source"]);
    let mut first = event("first-valid", Kind::TurnCompleted, AT);
    first.provider = Provider::Claude;
    let second = event("second-valid", Kind::TurnCompleted, AT);
    let mut batch = db.native_batch(&source, AT).unwrap().unwrap();
    assert!(batch.record(&first, AT).unwrap());
    // Decoding corrupt stored JSON is neither a SQLite error nor rejected native
    // metadata. Successful savepoint cleanup must not make it safe to continue.
    assert!(batch.record(&second, AT).is_err());
    assert!(
        batch.commit().is_err(),
        "unclassified errors must poison the batch"
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(db.cursor(&source).unwrap().is_none());
    raw.execute(
        "UPDATE daily SET payload=? WHERE provider='codex'",
        [payload],
    )
    .unwrap();
    let mut batch = db.native_batch(&source, AT).unwrap().unwrap();
    assert!(batch.record(&first, AT).unwrap());
    assert!(batch.record(&second, AT).unwrap());
    batch
        .save_cursor(&source, &Identity::new(&["file"]), 123, AT)
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(total(&summary(&db), "turn_completed"), 3);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn send_delivery_description_is_private_pure_and_emission_keeps_observer_dedup() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = Utc::now().timestamp_millis();
    let record = json!({"agent":"codex","conversation_id":"synthetic-send-conversation",
        "prompt":"PRIVATE_NATIVE_BODY","pending_settings":{"model":"PRIVATE_CHOICE"}});
    let receipt = json!({"request_id":"00000000-0000-4000-8000-000000000001",
        "submitted_at":now as f64 / 1000.0,"text":"PRIVATE_MESSAGE_BODY",
        "attachments":[{"path":"PRIVATE_ATTACHMENT_PATH"}]});
    let notice = adapters::describe_delivery(&record, &receipt, adapters::Delivery::Submitted).unwrap();
    assert!(!root.join("analytics").exists());
    let wire = serde_json::to_string(&notice).unwrap();
    assert!(!wire.contains("PRIVATE_"));
    let mut db = Store::open(&root.join("analytics"), true, true).unwrap().unwrap();
    db.configure(NIL_EPOCH, true, 90, now - 5000).unwrap();
    drop(db);
    let raw = rusqlite::Connection::open(root.join("analytics/analytics.sqlite3")).unwrap();
    raw.execute_batch("BEGIN EXCLUSIVE").unwrap();
    // Neither pure description nor best-effort emission may wait on a writer.
    let held = adapters::describe_delivery(&record, &receipt, adapters::Delivery::Submitted).unwrap();
    adapters::emit_delivery_notice(root, held);
    raw.execute_batch("ROLLBACK").unwrap();
    adapters::emit_delivery_notice(root, notice);
    adapters::delivery(root, &record, &receipt, adapters::Delivery::Submitted);
    let uncertain = adapters::describe_delivery(&record, &receipt, adapters::Delivery::Uncertain).unwrap();
    adapters::emit_delivery_notice(root, uncertain);
    let db = Store::open(&root.join("analytics"), false, false).unwrap().unwrap();
    let today = Utc::now().date_naive();
    let value = db.summary(MACHINE, &(today - chrono::Days::new(1)).to_string(),
                           &today.to_string(), now).unwrap();
    assert_eq!(total(&value, "delivery_submitted"), 1);
    assert_eq!(total(&value, "delivery_uncertain"), 1);
    assert!(!std::fs::read(root.join("analytics/analytics.sqlite3")).unwrap()
        .windows(b"PRIVATE_".len()).any(|part| part == b"PRIVATE_"));
}
