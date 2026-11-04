use super::*;
use serde_json::json;

pub(super) fn setup() -> (Connection, Source) {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&db).unwrap();
    begin_service(&mut db, "actor-1").unwrap();
    let source = Source {
        computer: "peer".into(),
        service_epoch: "actor-1".into(),
        incarnation: 1,
        generation: 1,
    };
    connect(&mut db, &source, &hello()).unwrap();
    (db, source)
}
fn frame(seq: u64, body: Body) -> Frame {
    Frame { v: 1, seq, body }
}
pub(super) fn hello() -> Frame {
    frame(
        1,
        Body::Hello(watch::Hello {
            machine_id: Some("example-machine".into()),
            host: "example-host".into(),
            hgs_version: "1.56.0".into(),
            stream: "example-stream".into(),
            heartbeat_ms: watch::HEARTBEAT_MS,
            max_frame_bytes: watch::MAX_FRAME_BYTES,
            topics: vec![topic::CATALOG.into()],
            limits: watch::Limits::default(),
        }),
    )
}
pub(super) fn item(n: usize) -> watch::CatalogItem {
    watch::CatalogItem {
        key: format!("run:run-{n}"),
        row: json!({"name":format!("codex/example/session-{n}"),"run_id":format!("run-{n}"),"conversation_id":format!("conversation-{n}"),"process_work":{"count":3,"future":true},"unknown":{"kept":"exact"}}),
    }
}
pub(super) fn snapshot(
    seq: u64,
    page: u32,
    last: bool,
    cursor: i64,
    items: Vec<watch::CatalogItem>,
) -> Frame {
    frame(
        seq,
        Body::Snapshot(watch::Snapshot {
            topic: topic::CATALOG.into(),
            key: None,
            value: None,
            items: Some(
                items
                    .into_iter()
                    .map(|i| serde_json::to_value(i).unwrap())
                    .collect(),
            ),
            page,
            last,
            cursor,
        }),
    )
}
fn delta(seq: u64, cursor: i64, upsert: Vec<watch::CatalogItem>, remove: &[usize]) -> Frame {
    frame(
        seq,
        Body::Delta(watch::Delta {
            topic: topic::CATALOG.into(),
            upsert,
            remove: remove
                .iter()
                .map(|n| watch::Removal {
                    key: item(*n).key,
                    reason: Some("archived".into()),
                })
                .collect(),
            cursor,
        }),
    )
}
pub(super) fn heartbeat(seq: u64) -> Frame {
    frame(seq, Body::Heartbeat(watch::Heartbeat { at: 1.0 }))
}
pub(super) fn put(db: &mut Connection, source: &Source, frame: &Frame) -> Applied {
    apply(db, source, frame).unwrap()
}
pub(super) fn visible(db: &Connection) -> Vec<Value> {
    page(db, "peer", None)
        .unwrap()
        .rows
        .into_iter()
        .map(|r| serde_json::from_str(&r.json).unwrap())
        .collect()
}

#[test]
fn snapshot_swap_is_atomic_and_preserves_exact_private_rows() {
    let (mut db, source) = setup();
    assert_eq!(
        put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)])),
        Applied::Visible
    );
    let old = visible(&db);
    assert_eq!(
        put(&mut db, &source, &snapshot(3, 0, false, 2, vec![item(2)])),
        Applied::Staging
    );
    assert_eq!(visible(&db), old);
    assert!(!status(&db, "peer").unwrap().complete);
    put(&mut db, &source, &snapshot(4, 1, true, 2, vec![item(3)]));
    assert_eq!(visible(&db), vec![item(2).row, item(3).row]);
    let s = status(&db, "peer").unwrap();
    assert_eq!(s.visible_rows, 2);
    assert_eq!(s.stored_rows, 3);
    assert!(s.complete);
    let counts = totals(&db).unwrap();
    assert_eq!(counts.retained_sources, 1);
    assert_eq!(counts.obsolete_generations, 1);
    assert_eq!(counts.stored_rows, s.stored_rows);
    assert_eq!(counts.stored_bytes, s.stored_bytes);
    assert_eq!(collect(&mut db).unwrap(), Some(1));
    assert_eq!(status(&db, "peer").unwrap().stored_rows, 2);
}

#[test]
fn removed_alias_releases_only_its_exact_projection_and_collects_in_pages() {
    let (mut db, mut source) = setup();
    put(
        &mut db,
        &source,
        &snapshot(2, 0, true, 1, (0..512).map(item).collect()),
    );
    let stale = source.clone();
    source.generation = 2;
    connect(&mut db, &source, &hello()).unwrap();
    assert!(remove_source(&mut db, &stale).is_err());
    assert!(page(&db, "peer", None).is_ok());
    disconnect_many(&mut db, &[source.clone()]).unwrap();
    remove_source(&mut db, &source).unwrap();
    assert!(page(&db, "peer", None).is_err());
    assert_eq!(totals(&db).unwrap().retained_sources, 0);
    assert_eq!(totals(&db).unwrap().stored_rows, 512);
    assert_eq!(collect(&mut db).unwrap(), Some(256));
    assert_eq!(totals(&db).unwrap().stored_rows, 256);
}

#[test]
fn incomplete_snapshot_forms_roll_back_and_retire_only_current_source() {
    for case in 0..7 {
        let (mut db, source) = setup();
        put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
        put(&mut db, &source, &snapshot(3, 0, false, 2, vec![item(2)]));
        let mut bad = snapshot(4, 1, true, 2, vec![item(3)]);
        if let Body::Snapshot(s) = &mut bad.body {
            match case {
                0 => s.page = 2,
                1 => s.cursor = 3,
                2 => s.items = None,
                3 => s.items = Some(vec![json!({"key":"run:wrong","row":item(3).row})]),
                4 => s.items = Some(vec![serde_json::to_value(item(2)).unwrap()]),
                5 => s.key = Some("unexpected".into()),
                _ => {
                    s.items = Some(vec![
                        json!({"key":"name:bad","row":{"name":"bad","run_id":123}}),
                    ])
                }
            }
        }
        assert!(apply(&mut db, &source, &bad).is_err(), "case {case}");
        assert_eq!(visible(&db), vec![item(1).row]);
        assert!(!status(&db, "peer").unwrap().complete);
        assert!(apply(&mut db, &source, &snapshot(5, 0, true, 3, vec![])).is_err());
        assert_eq!(collect(&mut db).unwrap(), Some(1));
    }
}

#[test]
fn source_generation_and_actor_fences_reject_stale_callbacks_without_damage() {
    let (mut db, source) = setup();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    let newer = Source {
        generation: 2,
        ..source.clone()
    };
    connect(&mut db, &newer, &hello()).unwrap();
    put(&mut db, &newer, &snapshot(2, 0, true, 2, vec![item(2)]));
    assert!(connect(&mut db, &source, &hello()).is_err());
    assert!(apply(&mut db, &source, &heartbeat(3)).is_err());
    assert!(disconnect(&mut db, &source).is_err());
    assert!(status(&db, "peer").unwrap().complete);
    begin_service(&mut db, "actor-2").unwrap();
    assert!(!status(&db, "peer").unwrap().complete);
    assert!(apply(&mut db, &newer, &heartbeat(3)).is_err());
    let restarted = Source {
        service_epoch: "actor-2".into(),
        ..source.clone()
    };
    connect(&mut db, &restarted, &hello()).unwrap();
    put(&mut db, &restarted, &snapshot(2, 0, true, 1, vec![item(3)]));
    assert_eq!(visible(&db), vec![item(3).row]);
}

#[test]
fn gaps_and_unknown_sequence_holes_require_a_new_complete_snapshot() {
    let (mut db, source) = setup();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    assert_eq!(put(&mut db, &source, &heartbeat(4)), Applied::Incomplete);
    assert!(!status(&db, "peer").unwrap().complete);
    // A new contiguous full snapshot restores coverage, not wall-clock freshness.
    put(&mut db, &source, &snapshot(5, 0, false, 2, vec![item(2)]));
    assert_eq!(
        put(
            &mut db,
            &source,
            &frame(
                6,
                Body::Gap(watch::Gap {
                    topic: topic::CATALOG.into(),
                    key: None,
                    reason: "overflow".into()
                })
            )
        ),
        Applied::Incomplete
    );
    assert_eq!(visible(&db), vec![item(1).row]);
    put(&mut db, &source, &snapshot(7, 0, true, 3, vec![]));
    assert!(visible(&db).is_empty());
    assert!(status(&db, "peer").unwrap().complete);
}

#[test]
fn duplicate_hello_sequence_and_catalog_errors_never_restore_coverage() {
    for bad in [
        hello(),
        heartbeat(2),
        frame(
            3,
            Body::Error(watch::ErrorFrame {
                code: "row_too_large".into(),
                message: "synthetic".into(),
                topic: Some(topic::CATALOG.into()),
                key: None,
                fatal: false,
            }),
        ),
        frame(
            3,
            Body::Error(watch::ErrorFrame {
                code: "fatal".into(),
                message: "synthetic".into(),
                topic: Some(topic::ORGANIZATION.into()),
                key: None,
                fatal: true,
            }),
        ),
    ] {
        let (mut db, source) = setup();
        put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
        let _ = apply(&mut db, &source, &bad);
        assert!(!status(&db, "peer").unwrap().complete);
        assert!(apply(&mut db, &source, &snapshot(4, 0, true, 2, vec![])).is_err());
        assert_eq!(visible(&db), vec![item(1).row]);
    }
}

#[test]
fn deltas_keep_insertion_identity_and_counts_and_refuse_incomplete_base() {
    let (mut db, source) = setup();
    put(
        &mut db,
        &source,
        &snapshot(2, 0, true, 1, vec![item(1), item(2)]),
    );
    let before = page(&db, "peer", None).unwrap();
    let mut changed = item(1);
    changed.row["name"] = "codex/example/renamed".into();
    put(
        &mut db,
        &source,
        &delta(3, 2, vec![changed.clone(), item(3)], &[2]),
    );
    let after = page(&db, "peer", None).unwrap();
    assert_eq!(after.rows[0].id, before.rows[0].id);
    assert_eq!(visible(&db), vec![changed.row, item(3).row]);
    let s = status(&db, "peer").unwrap();
    assert_eq!(s.visible_rows, 2);
    assert_eq!(s.stored_rows, 2);
    assert_eq!(
        s.stored_bytes,
        after
            .rows
            .iter()
            .map(|r| charge(&r.key, &r.json))
            .sum::<i64>()
            + serde_json::to_vec(match &hello().body {
                Body::Hello(h) => h,
                _ => unreachable!(),
            })
            .unwrap()
            .len() as i64
    );
    assert!(apply(&mut db, &source, &delta(4, 2, vec![], &[])).is_err());
    let newer = Source {
        generation: 2,
        ..source
    };
    connect(&mut db, &newer, &hello()).unwrap();
    assert!(apply(&mut db, &newer, &delta(2, 3, vec![item(4)], &[])).is_err());
    assert_eq!(status(&db, "peer").unwrap().visible_rows, 2);
}

#[test]
fn keyset_watermark_is_bounded_under_inserts_and_replacement_invalidates_it() {
    let (mut db, source) = setup();
    put(
        &mut db,
        &source,
        &snapshot(2, 0, true, 1, (0..300).map(item).collect()),
    );
    let first = page(&db, "peer", None).unwrap();
    assert_eq!(first.rows.len(), PAGE_ROWS);
    let cursor = first.next.unwrap();
    put(&mut db, &source, &delta(3, 2, vec![item(999)], &[200]));
    let mut total = first.rows.len();
    let mut next = Some(cursor.clone());
    while let Some(cursor) = next {
        let p = page(&db, "peer", Some(&cursor)).unwrap();
        total += p.rows.len();
        assert!(p.rows.iter().all(|r| r.key != item(999).key));
        next = p.next;
    }
    assert_eq!(total, 299);
    let mut wrong = cursor.clone();
    wrong.computer = "different".into();
    assert!(page(&db, "peer", Some(&wrong)).is_err());
    put(&mut db, &source, &snapshot(4, 0, true, 3, vec![item(1)]));
    assert!(page(&db, "peer", Some(&cursor)).is_err());
}

#[test]
fn byte_limited_pages_read_one_large_row_at_a_time() {
    let (mut db, source) = setup();
    let large = |n| {
        let mut i = item(n);
        i.row["synthetic"] = "x".repeat(600 << 10).into();
        i
    };
    put(&mut db, &source, &snapshot(2, 0, false, 1, vec![large(1)]));
    put(&mut db, &source, &snapshot(3, 1, true, 1, vec![large(2)]));
    let first = page(&db, "peer", None).unwrap();
    assert_eq!(first.rows.len(), 1);
    let second = page(&db, "peer", first.next.as_ref()).unwrap();
    assert_eq!(second.rows.len(), 1);
    assert!(second.next.is_none());
    let mut oversized = item(3);
    oversized.row["synthetic"] = "x".repeat(watch::MAX_FRAME_BYTES).into();
    assert!(apply(&mut db, &source, &delta(4, 2, vec![oversized], &[])).is_err());
    assert_eq!(status(&db, "peer").unwrap().visible_rows, 2);
}

#[test]
fn large_catalog_uses_indexed_pages_and_bounded_obsolete_collection() {
    let (mut db, source) = setup();
    const COUNT: usize = 100_000;
    for page in 0..COUNT / 128 {
        let start = page * 128;
        put(
            &mut db,
            &source,
            &snapshot(
                page as u64 + 2,
                page as u32,
                false,
                1,
                (start..start + 128).map(item).collect(),
            ),
        );
    }
    let start = COUNT / 128 * 128;
    let mut seq = (COUNT / 128) as u64 + 2;
    put(
        &mut db,
        &source,
        &snapshot(
            seq,
            (COUNT / 128) as u32,
            true,
            1,
            (start..COUNT).map(item).collect(),
        ),
    );
    assert_eq!(status(&db, "peer").unwrap().visible_rows, COUNT as i64);
    let mut next = None;
    let mut total = 0;
    loop {
        let p = page(&db, "peer", next.as_ref()).unwrap();
        assert!(p.rows.len() <= PAGE_ROWS);
        assert!(
            p.rows
                .iter()
                .map(|r| charge(&r.key, &r.json) as usize)
                .sum::<usize>()
                <= PAGE_BYTES
        );
        total += p.rows.len();
        next = p.next;
        if next.is_none() {
            break;
        }
    }
    assert_eq!(total, COUNT);
    for sql in [
        "SELECT id,key,json,bytes FROM desktop_catalog_rows WHERE generation=1 AND id>100 AND id<=90000 ORDER BY id LIMIT 129",
        "SELECT coalesce(max(id),0) FROM desktop_catalog_rows WHERE generation=1",
        "SELECT id FROM desktop_catalog_generations WHERE obsolete=1 ORDER BY id LIMIT 1",
        "SELECT id FROM desktop_catalog_rows WHERE generation=1 ORDER BY id LIMIT 256",
    ] {
        let plans: Vec<String> = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap().query_map([], |r| r.get(3)).unwrap().map(Result::unwrap).collect();
        assert!(plans.iter().all(|p| !p.contains("SCAN ") && !p.contains("TEMP B-TREE")), "{plans:?}");
        println!("indexed plan: {plans:?}");
    }
    seq += 1;
    put(
        &mut db,
        &source,
        &snapshot(seq, 0, true, 2, vec![item(999_999)]),
    );
    let mut removed = 0;
    let mut calls = 0;
    while let Some(n) = collect(&mut db).unwrap() {
        assert!(n <= GC_ROWS);
        removed += n;
        calls += 1;
    }
    assert_eq!(removed, COUNT);
    assert_eq!(calls, COUNT.div_ceil(GC_ROWS));
    assert_eq!(status(&db, "peer").unwrap().stored_rows, 1);
    println!("catalog rows={COUNT}, page cap={PAGE_ROWS}/{PAGE_BYTES}bytes, deleted={removed}, collection calls={calls}");
}

#[test]
fn restart_reopens_private_database_without_promoting_partial_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.sqlite3");
    let (memory, source) = setup();
    drop(memory);
    let mut db = Connection::open(&path).unwrap();
    migrate(&db).unwrap();
    begin_service(&mut db, "actor-1").unwrap();
    connect(&mut db, &source, &hello()).unwrap();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    put(&mut db, &source, &snapshot(3, 0, false, 2, vec![item(2)]));
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    begin_service(&mut db, "actor-2").unwrap();
    assert_eq!(visible(&db), vec![item(1).row]);
    assert!(!status(&db, "peer").unwrap().complete);
    assert_eq!(collect(&mut db).unwrap(), Some(1));
    assert!(apply(&mut db, &source, &snapshot(4, 1, true, 2, vec![item(3)])).is_err());
}

#[test]
fn source_and_empty_generation_admission_are_bounded() {
    let (mut db, source) = setup();
    for n in 1..MAX_SOURCES {
        let peer = Source {
            computer: format!("peer-{n}"),
            ..source.clone()
        };
        connect(&mut db, &peer, &hello()).unwrap();
    }
    assert!(connect(
        &mut db,
        &Source {
            computer: "overflow".into(),
            ..source.clone()
        },
        &hello()
    )
    .is_err());
    for n in 0..MAX_GENERATIONS {
        put(
            &mut db,
            &source,
            &snapshot(n as u64 + 2, 0, false, n, vec![]),
        );
    }
    assert!(apply(
        &mut db,
        &source,
        &snapshot(MAX_GENERATIONS as u64 + 2, 0, false, 1, vec![])
    )
    .is_err());
    let mut calls = 0;
    while let Some(n) = collect(&mut db).unwrap() {
        assert_eq!(n, 0);
        calls += 1;
    }
    assert_eq!(calls, MAX_GENERATIONS);
}

#[test]
fn collection_caps_large_payload_bytes_as_well_as_row_count() {
    let (mut db, source) = setup();
    for n in 0..9 {
        let mut large = item(n);
        large.row["synthetic"] = "x".repeat(800 << 10).into();
        put(
            &mut db,
            &source,
            &snapshot(n as u64 + 2, n as u32, n == 8, 1, vec![large]),
        );
    }
    put(&mut db, &source, &snapshot(11, 0, true, 2, vec![]));
    let before = totals(&db).unwrap().stored_bytes;
    assert_eq!(collect(&mut db).unwrap(), Some(5));
    let after = totals(&db).unwrap().stored_bytes;
    assert!(before - after <= GC_BYTES as i64);
    assert_eq!(collect(&mut db).unwrap(), Some(4));
    assert_eq!(collect(&mut db).unwrap(), None);
    db.execute(
        "INSERT INTO desktop_catalog_generations(computer,obsolete) VALUES('peer',1)",
        [],
    )
    .unwrap();
    let generation = db.last_insert_rowid();
    db.execute("INSERT INTO desktop_catalog_rows(id,generation,key,json,bytes) VALUES(?1,?2,'run:old','{}',64)",
        params![i64::from(i32::MAX)+1,generation]).unwrap();
    assert_eq!(collect(&mut db).unwrap(), Some(1));
}

#[test]
fn storage_limit_refuses_entire_frame_and_keeps_visible_generation() {
    let (mut db, source) = setup();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    // Deterministic aggregate admission fixture; no production-sized allocation.
    db.execute(
        "UPDATE desktop_catalog_totals SET bytes=?1 WHERE id=1",
        [MAX_BYTES],
    )
    .unwrap();
    assert!(apply(&mut db, &source, &delta(3, 2, vec![item(2)], &[])).is_err());
    assert_eq!(visible(&db), vec![item(1).row]);
    assert!(!status(&db, "peer").unwrap().complete);
    let count: i64 = db
        .query_row("SELECT count(*) FROM desktop_catalog_rows", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    // A real SQLite FULL rolls back both the row and counter mutations.
    let (mut db, source) = setup();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    db.pragma_update(None, "max_page_count", pages).unwrap();
    let mut large = item(2);
    large.row["synthetic"] = "x".repeat(64 << 10).into();
    let error = apply(&mut db, &source, &delta(3, 2, vec![large], &[])).unwrap_err();
    assert!(error.to_string().contains("full"), "{error:#}");
    assert_eq!(visible(&db), vec![item(1).row]);
    assert_eq!(status(&db, "peer").unwrap().stored_rows, 1);
}

#[test]
fn batch_retirement_is_exact_atomic_and_bounded() {
    let (mut db, source) = setup();
    put(&mut db, &source, &snapshot(2, 0, true, 1, vec![item(1)]));
    let mut replacement = source.clone();
    replacement.incarnation += 1;
    connect(&mut db, &replacement, &hello()).unwrap();
    put(
        &mut db,
        &replacement,
        &snapshot(2, 0, true, 2, vec![item(2)]),
    );
    let mut other = source.clone();
    other.computer = "other".into();
    connect(&mut db, &other, &hello()).unwrap();
    put(&mut db, &other, &snapshot(2, 0, false, 1, vec![item(3)]));
    // A late validation failure rolls back even the earlier exact retirement.
    let mut invalid = source.clone();
    invalid.generation = 0;
    assert!(disconnect_many(&mut db, &[replacement.clone(), invalid]).is_err());
    assert!(status(&db, "peer").unwrap().complete);
    assert!(disconnect_many(&mut db, &vec![source.clone(); MAX_SOURCES as usize + 1]).is_err());
    disconnect_many(&mut db, &[source, other.clone()]).unwrap();
    assert!(
        status(&db, "peer").unwrap().complete,
        "stale tuple retired replacement"
    );
    assert_eq!(visible(&db), vec![item(2).row]);
    assert!(!state(&db, "other").unwrap().unwrap().active);
    assert!(state(&db, "other").unwrap().unwrap().staged.is_none());
    disconnect_many(&mut db, &[other, replacement.clone()]).unwrap();
    disconnect_many(&mut db, &[replacement]).unwrap();
    assert!(!status(&db, "peer").unwrap().complete);
    assert_eq!(
        visible(&db),
        vec![item(2).row],
        "last-known display must survive"
    );
}
