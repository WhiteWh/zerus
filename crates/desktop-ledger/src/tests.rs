use super::*;
use serde_json::json;
use std::{
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

fn id(n: u32) -> String {
    format!("{n:08x}-1111-4111-8111-111111111111")
}
fn setup(db: &mut Connection) {
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .unwrap();
    let tx = db.transaction().unwrap();
    migrate(&tx).unwrap();
    tx.commit().unwrap();
    begin_service(db, &id(1)).unwrap();
}
fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    setup(&mut db);
    db
}
fn identity(edge: &str) -> Identity {
    Identity {
        kind: Kind::Post,
        target: Target::Session {
            computer: "peer.example".into(),
            run: "run-1".into(),
            conversation: "conversation-1".into(),
        },
        edge: edge.into(),
    }
}

#[test]
fn local_computer_alias_is_distinct_from_a_named_peer() {
    let mut db = database();
    let remote = put_intent(&mut db, &identity("edge"), 1, &json!({})).unwrap();
    let mut local = identity("edge");
    if let Target::Session { computer, .. } = &mut local.target {
        computer.clear();
    }
    let local_id = put_intent(&mut db, &local, 1, &json!({})).unwrap();
    assert_ne!(local_id, remote);
    if let Target::Session { computer, .. } = &mut local.target {
        *computer = "invalid\npeer".into();
    }
    assert!(put_intent(&mut db, &local, 1, &json!({})).is_err());
}
fn request(intent: &str, n: u32) -> Reserve {
    Reserve {
        request_id: id(n + 10),
        intent_id: intent.into(),
        revision: 1,
        epoch: id(2),
        service_epoch: id(1),
        adapter: id(3),
        lease: id(4),
        retry_of: None,
    }
}
fn ready(db: &mut Connection, edge: &str) -> String {
    authorize(db, &id(2), &id(1)).unwrap();
    put_intent(db, &identity(edge), 1, &json!({"title":"Example"})).unwrap()
}

#[test]
fn revoked_cleanup_is_only_an_exact_original_delivery_and_never_a_post_or_replay() {
    let mut db = database();
    authorize(&mut db, &id(2), &id(1)).unwrap();
    let identity = identity("confirmed");
    let native = json!({"platform":"macos","request_id":id(20)});
    let post = put_intent(
        &mut db,
        &identity,
        1,
        &json!({"op":"post","token":"original","title":"Title","body":"Body"}),
    )
    .unwrap();
    let post_req = request(&post, 1);
    let response = reserve(&mut db, &post_req).unwrap();
    let receipt = json!({"binding":{"request_id":post_req.request_id,"intent_id":post,"revision":1,"seq":response.attempt.seq,
        "session":{"writer_epoch":post_req.epoch,"service_epoch":post_req.service_epoch,"adapter":post_req.adapter,"lease":post_req.lease}},
        "outcome":{"state":"delivered","native_id":native}});
    finish(&mut db, &post_req, State::Delivered, &receipt).unwrap();
    let mut close = identity;
    close.kind = Kind::Withdrawal;
    let withdrawal = put_intent(
        &mut db,
        &close,
        1,
        &json!({"op":"withdraw","token":"original","native_id":native}),
    )
    .unwrap();
    revoke(&db, "manual_revoke").unwrap();
    let req = request(&withdrawal, 2);
    assert!(reserve(&mut db, &req).is_err());
    assert!(reserve_withdrawal(&mut db, &request(&post, 3)).is_err());
    let mut stale = req.clone();
    stale.epoch = id(90);
    assert!(reserve_withdrawal(&mut db, &stale).is_err());
    let mut corrupt = receipt.clone();
    corrupt["binding"]["request_id"] = id(91).into();
    db.execute(
        "UPDATE desktop_attempts SET receipt=?2 WHERE request_id=?1",
        params![post_req.request_id, corrupt.to_string()],
    )
    .unwrap();
    assert!(
        reserve_withdrawal(&mut db, &req).is_err(),
        "a mismatched receipt cannot name a native cleanup target"
    );
    db.execute(
        "UPDATE desktop_attempts SET receipt=?2 WHERE request_id=?1",
        params![post_req.request_id, receipt.to_string()],
    )
    .unwrap();
    db.execute(
        "UPDATE desktop_intents SET payload=?2 WHERE id=?1",
        params![
            withdrawal,
            json!({"op":"withdraw","token":"other","native_id":native}).to_string()
        ],
    )
    .unwrap();
    assert!(
        reserve_withdrawal(&mut db, &req).is_err(),
        "cleanup cannot replace the original token"
    );
    db.execute(
        "UPDATE desktop_intents SET payload=?2 WHERE id=?1",
        params![
            withdrawal,
            json!({"op":"withdraw","token":"original","native_id":native}).to_string()
        ],
    )
    .unwrap();
    let fresh = reserve_withdrawal(&mut db, &req).unwrap();
    assert!(fresh.fresh);
    assert!(
        native::inspect(&mut db, &req).is_err(),
        "read-only lookup cannot reconstruct cleanup origin"
    );
    assert_eq!(
        native::inspect_original(&mut db, fresh)
            .unwrap()
            .snapshot()
            .identity()
            .kind,
        Kind::Withdrawal
    );
    assert_eq!(status(&db).unwrap()["revoked"], true);
    finish(&mut db, &req, State::Unknown, &json!({"state":"unknown"})).unwrap();
    let again = reserve_withdrawal(&mut db, &request(&withdrawal, 4)).unwrap();
    assert!(!again.fresh);
    assert!(native::inspect_original(&mut db, again).is_err());
    let mut retry = request(&withdrawal, 5);
    retry.retry_of = Some(req.request_id.clone());
    assert!(reserve_withdrawal(&mut db, &retry).is_err());
}

#[test]
fn migration_and_process_start_are_revoked_by_default() {
    let mut db = database();
    assert_eq!(status(&db).unwrap()["revoked"], true);
    let intent = put_intent(&mut db, &identity("edge"), 1, &json!({})).unwrap();
    assert!(reserve(&mut db, &request(&intent, 1)).is_err());
    assert!(authorize(&mut db, &id(2), &id(99)).is_err());
    authorize(&mut db, &id(2), &id(1)).unwrap();
    assert!(reserve(&mut db, &request(&intent, 1)).unwrap().fresh);
}
#[test]
fn duplicate_request_and_duplicate_edge_never_grant_another_send() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    let first = reserve(&mut db, &req).unwrap();
    assert!(first.fresh);
    assert!(!reserve(&mut db, &req).unwrap().fresh);
    let other = reserve(&mut db, &request(&intent, 2)).unwrap();
    assert!(!other.fresh);
    assert_eq!(other.attempt.request_id, req.request_id);
    let mut changed = req.clone();
    changed.lease = id(99);
    assert!(reserve(&mut db, &changed).is_err());
    assert_eq!(status(&db).unwrap()["attempts"]["reserved"], 1);
}
#[test]
fn unknown_is_terminal_and_cannot_authorize_retry() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    finish(&mut db, &req, State::Unknown, &json!({"code":"no_reply"})).unwrap();
    assert!(!reserve(&mut db, &req).unwrap().fresh);
    let mut retry = request(&intent, 2);
    retry.retry_of = Some(req.request_id.clone());
    assert!(reserve(&mut db, &retry).is_err());
    assert!(finish(&mut db, &req, State::Delivered, &json!({})).is_err());
    assert_eq!(status(&db).unwrap()["attempts"]["unknown"], 1);
}
#[test]
fn exact_receipt_pins_and_duplicate_finish_preserve_counts() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    for field in 0..5 {
        let mut wrong = req.clone();
        match field {
            0 => wrong.epoch = id(90),
            1 => wrong.service_epoch = id(90),
            2 => wrong.adapter = id(90),
            3 => wrong.lease = id(90),
            _ => wrong.revision = 2,
        }
        assert!(finish(
            &mut db,
            &wrong,
            State::Delivered,
            &json!({"native_id":"example"})
        )
        .is_err());
    }
    let receipt = json!({"native_id":"example","owner":"example-owner"});
    finish(&mut db, &req, State::Delivered, &receipt).unwrap();
    finish(&mut db, &req, State::Delivered, &receipt).unwrap();
    assert!(finish(
        &mut db,
        &req,
        State::Delivered,
        &json!({"native_id":"different"})
    )
    .is_err());
    let counts = status(&db).unwrap();
    assert_eq!(counts["attempts"]["delivered"], 1);
    assert_eq!(counts["attempts"]["reserved"], 0);
}
#[test]
fn retry_requires_latest_confirmed_pre_delivery_refusal() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    finish(
        &mut db,
        &req,
        State::Refused,
        &json!({"code":"denied_before_send"}),
    )
    .unwrap();
    assert!(!reserve(&mut db, &request(&intent, 2)).unwrap().fresh);
    let mut retry = request(&intent, 2);
    retry.retry_of = Some(req.request_id.clone());
    assert!(reserve(&mut db, &retry).unwrap().fresh);
    assert!(!reserve(&mut db, &retry).unwrap().fresh);
    let mut stale = request(&intent, 3);
    stale.retry_of = Some(req.request_id);
    assert!(reserve(&mut db, &stale).is_err());
    finish(&mut db, &retry, State::Unknown, &json!({})).unwrap();
    stale.retry_of = Some(retry.request_id);
    assert!(reserve(&mut db, &stale).is_err());
}
#[test]
fn post_revision_changes_cannot_repost_a_delivered_or_unknown_edge() {
    for outcome in [State::Delivered, State::Unknown] {
        let mut db = database();
        let intent = ready(&mut db, "edge");
        let req = request(&intent, 1);
        reserve(&mut db, &req).unwrap();
        finish(&mut db, &req, outcome, &json!({})).unwrap();
        put_intent(&mut db, &identity("edge"), 2, &json!({"title":"Changed"})).unwrap();
        let mut new = request(&intent, 2);
        new.revision = 2;
        assert!(!reserve(&mut db, &new).unwrap().fresh);
    }
}
#[test]
fn badge_reservation_is_for_the_latest_projection_revision() {
    let mut db = database();
    authorize(&mut db, &id(2), &id(1)).unwrap();
    let key = Identity {
        kind: Kind::Badge,
        target: Target::Device {
            configuration: "a".repeat(64),
        },
        edge: "all".into(),
    };
    let intent = put_intent(&mut db, &key, 1, &json!({"count":1})).unwrap();
    let old = request(&intent, 1);
    reserve(&mut db, &old).unwrap();
    put_intent(&mut db, &key, 2, &json!({"count":2})).unwrap();
    assert!(!current(&db, &intent, 1).unwrap());
    assert!(current(&db, &intent, 2).unwrap());
    let mut stale = request(&intent, 2);
    assert!(reserve(&mut db, &stale).is_err());
    stale.revision = 2;
    assert!(reserve(&mut db, &stale).unwrap().fresh);
    finish(&mut db, &old, State::Delivered, &json!({})).unwrap();
    assert!(!current(&db, &intent, 1).unwrap());
}
#[test]
fn imported_and_focus_suppressed_baselines_stay_non_deliverable() {
    let mut db = database();
    authorize(&mut db, &id(2), &id(1)).unwrap();
    let intent = put_baseline(&mut db, &identity("historic"), 1, &json!({})).unwrap();
    assert!(current(&db, &intent, 1).unwrap());
    assert!(reserve(&mut db, &request(&intent, 1)).is_err());
    put_intent(&mut db, &identity("historic"), 2, &json!({})).unwrap();
    let mut next = request(&intent, 2);
    next.revision = 2;
    assert!(reserve(&mut db, &next).is_err());
}
#[test]
fn withdrawal_invalidates_current_activation_before_native_settlement() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    withdraw(&mut db, &intent, 2).unwrap();
    assert!(!current(&db, &intent, 1).unwrap());
    assert!(!current(&db, &intent, 2).unwrap());
    finish(&mut db, &req, State::Delivered, &json!({})).unwrap();
    assert!(put_intent(&mut db, &identity("edge"), 3, &json!({})).is_err());
    let mut next = request(&intent, 2);
    next.revision = 2;
    assert!(reserve(&mut db, &next).is_err());
}
#[test]
fn revocation_blocks_new_reservations_but_accepts_exact_in_flight_receipt() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    revoke(&db, "manual_rollback").unwrap();
    let other = put_intent(&mut db, &identity("other"), 1, &json!({})).unwrap();
    assert!(reserve(&mut db, &request(&other, 2)).is_err());
    finish(&mut db, &req, State::Delivered, &json!({})).unwrap();
    begin_service(&mut db, &id(90)).unwrap();
    assert_eq!(status(&db).unwrap()["reason"], "manual_rollback");
}
#[test]
fn crash_reservation_survives_reopen_and_never_becomes_fresh_again() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut db = Connection::open(&path).unwrap();
    setup(&mut db);
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    begin_service(&mut db, &id(90)).unwrap();
    assert_eq!(recover_batch(&mut db, &id(90)).unwrap(), 1);
    assert!(!reserve(&mut db, &req).unwrap().fresh);
    assert_eq!(status(&db).unwrap()["attempts"]["unknown"], 1);
    assert!(finish(&mut db, &req, State::Delivered, &json!({})).is_err());
}
#[test]
fn recovery_is_bounded_and_refuses_a_stale_service_epoch() {
    let mut db = database();
    authorize(&mut db, &id(2), &id(1)).unwrap();
    for n in 0..300 {
        let intent = put_intent(&mut db, &identity(&format!("edge-{n}")), 1, &json!({})).unwrap();
        reserve(&mut db, &request(&intent, n)).unwrap();
    }
    begin_service(&mut db, &id(90)).unwrap();
    assert!(recover_batch(&mut db, &id(1)).is_err());
    assert_eq!(recover_batch(&mut db, &id(90)).unwrap(), RECOVERY_ROWS);
    assert_eq!(recover_batch(&mut db, &id(90)).unwrap(), 44);
    assert_eq!(recover_batch(&mut db, &id(90)).unwrap(), 0);
    assert_eq!(status(&db).unwrap()["attempts"]["unknown"], 300);
}
#[test]
fn indexed_keyset_pages_remain_stable_while_new_attempts_arrive() {
    let mut db = database();
    authorize(&mut db, &id(2), &id(1)).unwrap();
    for n in 0..300 {
        let intent = put_intent(&mut db, &identity(&format!("edge-{n}")), 1, &json!({})).unwrap();
        reserve(&mut db, &request(&intent, n)).unwrap();
    }
    let first = page(&db, i64::MAX, 128).unwrap();
    let anchor = first.last().unwrap().seq;
    let intent = put_intent(&mut db, &identity("new"), 1, &json!({})).unwrap();
    reserve(&mut db, &request(&intent, 999)).unwrap();
    let second = page(&db, anchor, 128).unwrap();
    assert!(second.iter().all(|item| item.seq < anchor));
    let third = page(&db, second.last().unwrap().seq, 128).unwrap();
    assert_eq!(first.len() + second.len() + third.len(), 300);
    assert!(page(&db, i64::MAX, 129).is_err());
    assert!(page(&db, 0, 1).is_err());
}
#[test]
fn concurrent_reservations_on_two_connections_grant_only_one_delivery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.sqlite3");
    let mut db = Connection::open(&path).unwrap();
    setup(&mut db);
    let intent = ready(&mut db, "edge");
    drop(db);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|n| {
            let path = path.clone();
            let barrier = barrier.clone();
            let intent = intent.clone();
            thread::spawn(move || {
                let mut db = Connection::open(path).unwrap();
                db.busy_timeout(Duration::from_secs(2)).unwrap();
                barrier.wait();
                reserve(&mut db, &request(&intent, n)).unwrap().fresh
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn failed_durable_reservation_never_grants_delivery() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    db.execute_batch("CREATE TRIGGER injected BEFORE INSERT ON desktop_attempts BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(reserve(&mut db, &req).is_err());
    assert_eq!(status(&db).unwrap()["attempts"]["reserved"], 0);
    db.execute_batch("DROP TRIGGER injected").unwrap();
    assert!(reserve(&mut db, &req).unwrap().fresh);
}
#[test]
fn sqlite_full_preserves_existing_unknown_and_refuses_new_intent() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    let req = request(&intent, 1);
    reserve(&mut db, &req).unwrap();
    finish(&mut db, &req, State::Unknown, &json!({})).unwrap();
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    db.pragma_update(None, "max_page_count", pages + 1).unwrap();
    let error = put_intent(
        &mut db,
        &identity("too-large-for-disk"),
        1,
        &json!("p".repeat(60_000)),
    )
    .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(e,_)) if e.code==rusqlite::ErrorCode::DiskFull)
    );
    assert!(!reserve(&mut db, &req).unwrap().fresh);
    assert_eq!(status(&db).unwrap()["attempts"]["unknown"], 1);
}
#[test]
fn input_bounds_and_changed_revision_payload_fail_before_reservation() {
    let mut db = database();
    let intent = ready(&mut db, "edge");
    assert!(put_intent(&mut db, &identity("edge"), 1, &json!({"changed":true})).is_err());
    assert!(put_intent(
        &mut db,
        &identity("oversize"),
        1,
        &json!("x".repeat(PAYLOAD_BYTES))
    )
    .is_err());
    let mut req = request(&intent, 1);
    req.revision = u64::MAX;
    assert!(reserve(&mut db, &req).is_err());
    req.revision = 1;
    req.request_id = "not-a-uuid".into();
    assert!(reserve(&mut db, &req).is_err());
}

#[test]
fn canonical_intent_lookup_is_read_only_and_matches_original_insert() {
    let mut db = database();
    let post = identity("lookup");
    let before = db.total_changes();
    let key = intent_id(&post).unwrap();
    assert_eq!(db.total_changes(), before);
    assert_eq!(put_intent(&mut db, &post, 1, &json!({})).unwrap(), key);
    let mut withdrawal = post.clone();
    withdrawal.kind = Kind::Withdrawal;
    assert_ne!(intent_id(&withdrawal).unwrap(), key);
    withdrawal.edge.clear();
    assert!(intent_id(&withdrawal).is_err());
}
