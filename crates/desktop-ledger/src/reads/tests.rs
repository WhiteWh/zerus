use super::*;

fn id(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn setup(db: &mut Connection) {
    let tx = db.transaction().unwrap();
    crate::migrate(&tx).unwrap();
    migrate(&tx).unwrap();
    tx.commit().unwrap();
    crate::begin_service(db, &id(1)).unwrap();
}
fn db() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    setup(&mut db);
    db
}
fn mark(host: &str, name: &str) -> ReadMark {
    ReadMark {
        computer: host.into(),
        name: name.into(),
        run_id: "run".into(),
        conversation_id: "conversation".into(),
        reply_id: Some("reply".into()),
        attention_signature: None,
    }
}
fn put(db: &mut Connection, n: u64, mark: &ReadMark) -> Queued {
    enqueue(db, &id(n), mark, 0).unwrap()
}
fn active(db: &mut Connection) {
    crate::authorize(db, &id(2), &id(1)).unwrap();
}
fn take(db: &mut Connection, n: u64, now: i64) -> Option<Delivery> {
    reserve(db, &id(2), &id(1), &id(n), now).unwrap()
}
fn one(db: &Connection) -> Pending {
    page(db, 0, watermark(db).unwrap(), 1).unwrap().remove(0)
}
fn seen<'a>(mark: &'a ReadMark, read: bool) -> Snapshot<'a> {
    Snapshot {
        computer: &mark.computer,
        name: &mark.name,
        run: Some(&mark.run_id),
        conversation: Some(&mark.conversation_id),
        reply: mark.reply_id.as_deref(),
        signature: mark.attention_signature.as_deref(),
        read_reply: if read { mark.reply_id.as_deref() } else { None },
        read_signature: if read {
            mark.attention_signature.as_deref()
        } else {
            None
        },
    }
}

#[test]
fn passive_intent_is_durable_but_cannot_dispatch() {
    let mut db = db();
    put(&mut db, 10, &mark("", "s"));
    assert!(reserve(&mut db, &id(2), &id(1), &id(11), 0).is_err());
    assert_eq!(status(&db).unwrap()["pending"], 1);
    active(&mut db);
    assert!(take(&mut db, 11, 0).is_some());
    crate::revoke(&db, "rollback").unwrap();
    assert!(reserve(&mut db, &id(2), &id(1), &id(12), 0).is_err());
}

#[test]
fn merge_and_request_replay_preserve_exact_identity() {
    let mut db = db();
    let a = mark("mac", "s");
    let first = put(&mut db, 10, &a);
    let mut b = a.clone();
    b.reply_id = None;
    b.attention_signature = Some("question".into());
    let second = put(&mut db, 11, &b);
    assert_eq!(second.revision, 2);
    assert_eq!(put(&mut db, 10, &a), first);
    assert!(enqueue(&mut db, &id(10), &b, 0).is_err());
    let merged = one(&db);
    assert_eq!(merged.mark.reply_id, a.reply_id);
    assert_eq!(merged.mark.attention_signature, b.attention_signature);
    b.run_id = "another-run".into();
    put(&mut db, 12, &b);
    assert_eq!(status(&db).unwrap()["pending"], 2);
    b.conversation_id = "another-conversation".into();
    put(&mut db, 13, &b);
    assert_eq!(status(&db).unwrap()["pending"], 3);
}

#[test]
fn import_transaction_rolls_back_all_marks_on_a_later_identity_mismatch() {
    let mut db = db();
    let original = mark("mac", "original");
    let receipt = put(&mut db, 10, &original);
    {
        let tx = db.transaction().unwrap();
        enqueue_in(&tx, &id(11), &mark("mac", "new"), 0).unwrap();
        let mut changed = original.clone();
        changed.reply_id = Some("different-reply".into());
        assert!(enqueue_in(&tx, &id(10), &changed, 0).is_err());
        // No page receipt or intent may survive the failed original transaction.
    }
    assert_eq!(status(&db).unwrap()["pending"], 1);
    assert_eq!(put(&mut db, 10, &original), receipt);
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop_read_requests WHERE request_id=?1)",
            [id(11)],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn import_transaction_commits_exact_receipts_without_authorizing_delivery() {
    let mut db = db();
    let mark = mark("mac", "original");
    let tx = db.transaction().unwrap();
    let first = enqueue_in(&tx, &id(10), &mark, 0).unwrap();
    assert_eq!(enqueue_in(&tx, &id(10), &mark, 1).unwrap(), first);
    tx.commit().unwrap();
    assert_eq!(put(&mut db, 10, &mark), first);
    assert_eq!(status(&db).unwrap()["pending"], 1);
    assert!(reserve(&mut db, &id(2), &id(1), &id(11), 0).is_err());
}

#[test]
fn admission_is_four_global_with_fair_host_choice() {
    let mut db = db();
    for (n, host, name) in [
        (10, "a", "1"),
        (11, "a", "2"),
        (12, "a", "3"),
        (13, "b", "1"),
        (14, "b", "2"),
        (15, "c", "1"),
    ] {
        put(&mut db, n, &mark(host, name));
    }
    active(&mut db);
    let deliveries: Vec<_> = (20..24).map(|n| take(&mut db, n, 0).unwrap()).collect();
    let hosts: Vec<_> = deliveries
        .iter()
        .map(|d| d.pending.mark.computer.as_str())
        .collect();
    assert_eq!(hosts, ["a", "b", "c", "a"]);
    assert!(take(&mut db, 24, 0).is_none());
    finish(
        &mut db,
        &deliveries[1],
        Outcome::Delivered { applied_all: true },
        0,
    )
    .unwrap();
    assert_eq!(take(&mut db, 24, 0).unwrap().pending.mark.computer, "b");
    assert_eq!(status(&db).unwrap()["in_flight"], 4);
}

#[test]
fn durable_retry_uses_failure_and_partial_application_delays() {
    let mut db = db();
    put(&mut db, 10, &mark("mac", "s"));
    active(&mut db);
    let mut at = 0;
    let outcomes = [
        (Outcome::Failed, 10_000),
        (Outcome::Failed, 20_000),
        (Outcome::Delivered { applied_all: false }, 120_000),
        (Outcome::Delivered { applied_all: false }, 240_000),
        (Outcome::Delivered { applied_all: false }, 480_000),
        (Outcome::Delivered { applied_all: false }, 960_000),
        (Outcome::Delivered { applied_all: false }, 960_000),
        (Outcome::Delivered { applied_all: true }, 120_000),
        (Outcome::Failed, 10_000),
    ];
    for (n, (outcome, delay)) in outcomes.into_iter().enumerate() {
        let sent = take(&mut db, 20 + n as u64, at).unwrap();
        finish(&mut db, &sent, outcome, at).unwrap();
        assert!(take(&mut db, 100 + n as u64, at + delay - 1).is_none());
        at += delay;
    }
    assert!(take(&mut db, 200, at).is_some());
}

#[test]
fn changed_mark_waits_for_old_attempt_but_not_its_backoff() {
    let mut db = db();
    let mut m = mark("mac", "s");
    put(&mut db, 10, &m);
    active(&mut db);
    let old = take(&mut db, 20, 0).unwrap();
    m.reply_id = Some("new".into());
    put(&mut db, 11, &m);
    assert!(take(&mut db, 21, 0).is_none());
    finish(&mut db, &old, Outcome::Failed, 100).unwrap();
    let new = take(&mut db, 21, 101).unwrap();
    assert_eq!(new.pending.revision, 2);
    assert_eq!(new.pending.mark, m);
    assert!(!finish(&mut db, &old, Outcome::Failed, 102).unwrap());
    assert_eq!(status(&db).unwrap()["in_flight"], 1);
    assert!(finish(&mut db, &old, Outcome::Delivered { applied_all: true }, 102).is_err());
    let mut forged = new.clone();
    forged.pending.mark.run_id = "replaced".into();
    assert!(finish(&mut db, &forged, Outcome::NotSent, 102).is_err());
    assert!(reserve(&mut db, &id(2), &id(1), &old.id, 200).is_err());
}

#[test]
fn receipts_wait_for_snapshot_and_observation_keeps_owned_slot_until_exit() {
    let mut db = db();
    let m = mark("mac", "s");
    put(&mut db, 10, &m);
    active(&mut db);
    let sent = take(&mut db, 20, 0).unwrap();
    finish(&mut db, &sent, Outcome::Delivered { applied_all: true }, 0).unwrap();
    assert_eq!(status(&db).unwrap()["pending"], 1);
    assert!(!observe(&mut db, &sent.pending, &seen(&m, false)).unwrap());
    let sent = take(&mut db, 21, 120_000).unwrap();
    assert!(observe(&mut db, &sent.pending, &seen(&m, true)).unwrap());
    assert_eq!(status(&db).unwrap()["pending"], 0);
    assert_eq!(status(&db).unwrap()["in_flight"], 1);
    finish(&mut db, &sent, Outcome::NotSent, 120_001).unwrap();
    assert_eq!(status(&db).unwrap()["in_flight"], 0);
    assert!(page(&db, 0, watermark(&db).unwrap(), 1).unwrap().is_empty());
    // An old command receipt cannot recreate a retired intent.
    assert_eq!(put(&mut db, 10, &m).revision, 1);
    assert_eq!(status(&db).unwrap()["pending"], 0);
}

#[test]
fn stale_snapshot_cannot_retire_changed_mark() {
    let mut db = db();
    let mut m = mark("mac", "s");
    put(&mut db, 10, &m);
    let old = one(&db);
    m.reply_id = Some("new".into());
    put(&mut db, 11, &m);
    assert!(!observe(&mut db, &old, &seen(&old.mark, true)).unwrap());
    let new = one(&db);
    let mut snapshot = seen(&m, false);
    snapshot.computer = "another-host";
    assert!(observe(&mut db, &new, &snapshot).is_err());
    snapshot.computer = &m.computer;
    snapshot.run = Some("reused-run");
    assert!(observe(&mut db, &new, &snapshot).unwrap());
    assert_eq!(status(&db).unwrap()["pending"], 0);
}

#[test]
fn concurrent_connections_share_four_global_slots() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("concurrent.sqlite3");
    let mut db = Connection::open(&path).unwrap();
    setup(&mut db);
    for n in 0..8 {
        put(&mut db, 10 + n, &mark("peer", &format!("s-{n}")));
    }
    active(&mut db);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let jobs: Vec<_> = (0..8)
        .map(|n| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut db = Connection::open(path).unwrap();
                db.busy_timeout(std::time::Duration::from_secs(2)).unwrap();
                barrier.wait();
                take(&mut db, 20 + n, 0).is_some()
            })
        })
        .collect();
    let admitted = jobs
        .into_iter()
        .map(|j| usize::from(j.join().unwrap()))
        .sum::<usize>();
    assert_eq!(admitted, 4);
    assert_eq!(status(&db).unwrap()["in_flight"], 4);
}

#[test]
fn changed_intent_after_snapshot_waits_for_original_owned_worker() {
    let mut db = db();
    let m = mark("mac", "s");
    put(&mut db, 10, &m);
    active(&mut db);
    let sent = take(&mut db, 20, 0).unwrap();
    observe(&mut db, &sent.pending, &seen(&m, true)).unwrap();
    put(&mut db, 11, &m);
    assert!(take(&mut db, 21, 0).is_none());
    finish(
        &mut db,
        &sent,
        Outcome::Delivered { applied_all: true },
        100,
    )
    .unwrap();
    assert_eq!(take(&mut db, 21, 101).unwrap().pending.revision, 2);
}

#[test]
fn crash_recovery_preserves_intent_and_fences_old_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("queue.sqlite3");
    let mut db = Connection::open(&path).unwrap();
    setup(&mut db);
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .unwrap();
    let m = mark("mac", "s");
    let original = put(&mut db, 10, &m);
    active(&mut db);
    let sent = take(&mut db, 20, 0).unwrap();
    drop(db);
    let mut db = Connection::open(path).unwrap();
    crate::begin_service(&mut db, &id(3)).unwrap();
    assert_eq!(recover(&mut db, &id(3), 1_000).unwrap(), 1);
    assert_eq!(recover(&mut db, &id(3), 2_000).unwrap(), 0);
    assert_eq!(put(&mut db, 10, &m), original);
    assert!(finish(
        &mut db,
        &sent,
        Outcome::Delivered { applied_all: true },
        2_000
    )
    .is_err());
    assert!(reserve(&mut db, &id(2), &id(3), &id(21), 11_000).is_err());
    crate::authorize(&mut db, &id(4), &id(3)).unwrap();
    assert!(reserve(&mut db, &id(4), &id(3), &id(21), 10_999)
        .unwrap()
        .is_none());
    assert!(reserve(&mut db, &id(4), &id(3), &id(21), 11_000)
        .unwrap()
        .is_some());
    assert!(reserve(&mut db, &id(4), &id(3), &sent.id, 11_000).is_err());
}

#[test]
fn exact_inflight_result_can_settle_after_revocation_without_new_dispatch() {
    let mut db = db();
    put(&mut db, 10, &mark("mac", "s"));
    active(&mut db);
    let sent = take(&mut db, 20, 0).unwrap();
    crate::revoke(&db, "rollback").unwrap();
    finish(&mut db, &sent, Outcome::NotSent, 1).unwrap();
    assert_eq!(status(&db).unwrap()["pending"], 1);
    assert!(reserve(&mut db, &id(2), &id(1), &id(21), 1).is_err());
}

#[test]
fn large_queue_uses_indexed_candidates_and_bounded_stable_pages() {
    let mut db = db();
    {
        let tx = db.transaction().unwrap();
        {
            let mut insert = tx.prepare("INSERT INTO desktop_reads(key,computer,revision,mark,due_at,failures,unapplied,settled) VALUES(?1,?2,1,?3,?4,0,0,0)").unwrap();
            for n in 0..50_000 {
                let m = mark(&format!("host-{:02}", n % 20), &format!("s-{n:05}"));
                insert
                    .execute(params![
                        key(&m).unwrap(),
                        m.computer,
                        encode(&m).unwrap(),
                        n
                    ])
                    .unwrap();
            }
        }
        for n in 0..20 {
            refresh(&tx, &format!("host-{n:02}")).unwrap();
        }
        tx.commit().unwrap();
    }
    let queries = [
        ("EXPLAIN QUERY PLAN SELECT computer FROM desktop_read_hosts WHERE in_flight=0 AND next_due<=100 ORDER BY next_due,computer LIMIT 1", "desktop_read_hosts_due"),
        ("EXPLAIN QUERY PLAN SELECT seq,revision,mark FROM desktop_reads WHERE computer='host-00' AND attempt IS NULL AND settled=0 AND due_at<=100 ORDER BY due_at,seq LIMIT 1", "desktop_reads_due"),
        ("EXPLAIN QUERY PLAN SELECT seq,revision,mark FROM desktop_reads WHERE seq>10 AND seq<=50000 AND settled=0 ORDER BY seq LIMIT 128", "INTEGER PRIMARY KEY"),
    ];
    for (sql, index) in queries {
        let details = db
            .prepare(sql)
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ");
        assert!(details.contains(index), "{details}");
        assert!(
            !details.contains("SCAN ") && !details.contains("TEMP B-TREE"),
            "{details}"
        );
    }
    let through = watermark(&db).unwrap();
    put(&mut db, 10, &mark("new", "later"));
    assert_eq!(page(&db, 49_990, through, 128).unwrap().len(), 10);
    assert!(page(&db, 0, through, 129).is_err());
    active(&mut db);
    for n in 20..24 {
        assert!(take(&mut db, n, 100).is_some());
    }
    assert!(take(&mut db, 24, 100).is_none());
    assert_eq!(status(&db).unwrap()["pending"], 50_001);
    println!("large read queue: 50000 seeded rows, 3 indexed plans, 4 bounded admissions");
}

#[test]
fn full_database_refuses_before_queue_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Connection::open(temp.path().join("full.sqlite3")).unwrap();
    setup(&mut db);
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    db.pragma_update(None, "max_page_count", pages).unwrap();
    let mut m = mark("mac", "s");
    m.reply_id = Some("r".repeat(8192));
    assert!(enqueue(&mut db, &id(10), &m, 0).is_err());
    assert_eq!(status(&db).unwrap()["pending"], 0);
    let requests: i64 = db
        .query_row("SELECT count(*) FROM desktop_read_requests", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(requests, 0);
}

#[test]
fn invalid_marks_refuse_without_durable_admission() {
    let mut db = db();
    let m = mark("mac", "s");
    for bad in [
        ReadMark {
            computer: "@peer".into(),
            ..m.clone()
        },
        ReadMark {
            reply_id: None,
            ..m.clone()
        },
        ReadMark {
            run_id: String::new(),
            ..m.clone()
        },
        ReadMark {
            attention_signature: Some("x".repeat(8193)),
            ..m.clone()
        },
    ] {
        assert!(enqueue(&mut db, &id(10), &bad, 0).is_err());
    }
    assert_eq!(status(&db).unwrap()["pending"], 0);
    let mut retry = Retry::default();
    assert_eq!(retry.finished(false, false, i64::MAX), i64::MAX);
}

#[test]
fn native_source_filter_does_not_reserve_offline_hosts_or_break_fairness() {
    let mut db = db();
    active(&mut db);
    for (n, host, name) in [
        (10, "offline", "a"),
        (11, "a", "a"),
        (12, "a", "b"),
        (13, "b", "a"),
    ] {
        put(&mut db, n, &mark(host, name));
    }
    assert!(reserve_for(&mut db, &id(2), &id(1), &id(20), 0, &[])
        .unwrap()
        .is_none());
    let hosts = vec!["a".into(), "b".into()];
    let chosen: Vec<_> = (21..24)
        .map(|n| {
            reserve_for(&mut db, &id(2), &id(1), &id(n), 0, &hosts)
                .unwrap()
                .unwrap()
                .pending
                .mark
                .computer
        })
        .collect();
    assert_eq!(chosen, ["a", "b", "a"]);
    assert!(reserve_for(&mut db, &id(2), &id(1), &id(24), 0, &hosts)
        .unwrap()
        .is_none());
    assert!(reserve_for(
        &mut db,
        &id(2),
        &id(1),
        &id(25),
        0,
        &["a".into(), "a".into()]
    )
    .is_err());
}
