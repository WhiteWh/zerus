use super::*;
use crate::tests::{hello, item, put, setup, snapshot};
use serde_json::json;

fn delta(seq: u64, cursor: i64, upsert: Vec<watch::CatalogItem>, remove: Vec<String>) -> Frame {
    Frame {
        v: 1,
        seq,
        body: Body::Delta(watch::Delta {
            topic: topic::CATALOG.into(),
            cursor,
            upsert,
            remove: remove
                .into_iter()
                .map(|key| watch::Removal { key, reason: None })
                .collect(),
        }),
    }
}
fn populate(rows: usize) -> (Connection, Source, u64) {
    let (mut db, source) = setup();
    let pages = rows.div_ceil(PAGE_ROWS).max(1);
    for page in 0..pages {
        put(
            &mut db,
            &source,
            &snapshot(
                page as u64 + 2,
                page as u32,
                page + 1 == pages,
                0,
                (page * PAGE_ROWS..((page + 1) * PAGE_ROWS).min(rows))
                    .map(item)
                    .collect(),
            ),
        );
    }
    (db, source, pages as u64 + 2)
}
#[test]
fn twenty_thousand_baseline_catches_up_only_changed_keys() {
    let (mut db, source, seq) = populate(20_000);
    let start = watermark(&db, "peer").unwrap();
    let first = crate::page(&db, "peer", None).unwrap();
    let mut result: BTreeMap<_, _> = first.rows.into_iter().map(|r| (r.key, r.json)).collect();
    let mut changed = item(0);
    changed.row["future"] = json!(true);
    put(
        &mut db,
        &source,
        &delta(
            seq,
            1,
            vec![changed.clone(), item(20_000)],
            vec![item(400).key],
        ),
    );
    let mut cursor = first.next;
    while let Some(c) = cursor {
        let p = crate::page(&db, "peer", Some(&c)).unwrap();
        result.extend(p.rows.into_iter().map(|r| (r.key, r.json)));
        cursor = p.next;
    }
    assert!(!result.contains_key(&item(20_000).key));
    let changes = page(&db, &start.scope, start.revision.unwrap(), None).unwrap();
    assert_eq!(changes.items.len(), 3);
    assert!(changes.next.is_none());
    assert_eq!(changes.through, 1);
    for c in changes.items {
        match c.json {
            Some(json) => {
                result.insert(c.key, json);
            }
            None => {
                result.remove(&c.key);
            }
        }
    }
    assert_eq!(result.len(), 20_000);
    assert_eq!(
        result[&changed.key],
        serde_json::to_string(&changed.row).unwrap()
    );
    assert!(!result.contains_key(&item(400).key));
    assert!(result.contains_key(&item(20_000).key));
    assert!(page(&db, &start.scope, 1, None).unwrap().items.is_empty());
}
#[test]
fn highwater_and_original_payload_stay_stable_after_later_deletion() {
    let (mut db, source, seq) = populate(0);
    let start = watermark(&db, "peer").unwrap();
    put(
        &mut db,
        &source,
        &delta(seq, 1, (0..300).map(item).collect(), vec![]),
    );
    let first = page(&db, &start.scope, 0, None).unwrap();
    assert_eq!(first.items.len(), 128);
    put(
        &mut db,
        &source,
        &delta(seq + 1, 2, vec![], (0..300).map(|n| item(n).key).collect()),
    );
    let mut all = first.items;
    let mut next = first.next;
    while let Some(c) = next {
        let p = page(&db, &start.scope, 0, Some(&c)).unwrap();
        assert_eq!(p.through, 1);
        all.extend(p.items);
        next = p.next;
    }
    assert_eq!(all.len(), 300);
    assert!(all.iter().all(|c| c.revision == 1 && c.json.is_some()));
    let p = page(&db, &start.scope, 1, None).unwrap();
    assert!(p.items.iter().all(|c| c.revision == 2 && c.json.is_none()));
}
#[test]
fn exact_revision_key_dedup_and_atomic_rollback() {
    let (mut db, source, seq) = populate(0);
    let mark = watermark(&db, "peer").unwrap();
    let mut changed = item(1);
    changed.row["name"] = json!("renamed");
    put(
        &mut db,
        &source,
        &delta(
            seq,
            1,
            vec![item(0), item(0), item(1), changed.clone()],
            vec![item(0).key],
        ),
    );
    let p = page(&db, &mark.scope, 0, None).unwrap();
    assert_eq!(p.items.len(), 2);
    assert!(p
        .items
        .iter()
        .find(|c| c.key == item(0).key)
        .unwrap()
        .json
        .is_none());
    assert_eq!(
        p.items
            .iter()
            .find(|c| c.key == item(1).key)
            .unwrap()
            .json
            .as_deref(),
        Some(serde_json::to_string(&changed.row).unwrap().as_str())
    );
    db.execute_batch("CREATE TRIGGER fixture_abort BEFORE INSERT ON desktop_catalog_changes BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(crate::apply(&mut db, &source, &delta(seq + 1, 2, vec![item(2)], vec![])).is_err());
    assert_eq!(watermark(&db, "peer").unwrap().revision, Some(1));
    assert_eq!(crate::page(&db, "peer", None).unwrap().rows.len(), 1);
    assert!(!watermark(&db, "peer").unwrap().complete);
}
#[test]
fn item_and_byte_pressure_are_explicit_gaps_with_bounded_gc() {
    let (mut db, source, mut seq) = populate(0);
    let mark = watermark(&db, "peer").unwrap();
    for revision in 1..=130 {
        put(
            &mut db,
            &source,
            &delta(seq, revision, (0..256).map(item).collect(), vec![]),
        );
        seq += 1;
    }
    let usage = |db: &Connection| {
        let (items, bytes): (i64, i64) = db
            .query_row(
                "SELECT items,bytes FROM desktop_catalog_change_totals",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(items <= MAX_ITEMS && bytes <= MAX_BYTES);
    };
    usage(&db);
    assert!(page(&db, &mark.scope, 0, None).is_err());
    let revision = watermark(&db, "peer").unwrap().revision.unwrap();
    assert!(page(&db, &mark.scope, revision, None)
        .unwrap()
        .items
        .is_empty());
    for n in 0..25 {
        let mut big = item(0);
        big.row["payload"] = json!("x".repeat(700_000));
        put(&mut db, &source, &delta(seq, 131 + n, vec![big], vec![]));
        seq += 1;
        usage(&db);
    }
    assert!(page(&db, &mark.scope, revision, None).is_err());
    let mut calls = 0;
    while let Some(n) = crate::collect(&mut db).unwrap() {
        assert!(n <= GC_ROWS);
        calls += 1;
        assert!(calls < 500);
    }
}
#[test]
fn replacement_offline_remove_readd_and_restart_reject_old_scope() {
    let (mut db, source, seq) = populate(1);
    let old = watermark(&db, "peer").unwrap();
    put(&mut db, &source, &snapshot(seq, 0, true, 1, vec![item(1)]));
    assert!(page(&db, &old.scope, 0, None).is_err());
    let replacement = watermark(&db, "peer").unwrap();
    crate::disconnect(&mut db, &source).unwrap();
    assert!(page(&db, &replacement.scope, 0, None).is_err());
    let mut newer = source.clone();
    newer.generation += 1;
    connect(&mut db, &newer, &hello()).unwrap();
    assert!(watermark(&db, "peer").unwrap().revision.is_none());
    assert!(page(&db, &replacement.scope, 0, None).is_err());
    put(&mut db, &newer, &snapshot(2, 0, true, 0, vec![item(2)]));
    crate::remove_source(&mut db, &newer).unwrap();
    assert!(page(&db, &replacement.scope, 0, None).is_err());
    newer.generation += 1;
    connect(&mut db, &newer, &hello()).unwrap();
    put(&mut db, &newer, &snapshot(2, 0, true, 0, vec![item(3)]));
    assert!(crate::remove_source(&mut db, &source).is_err());
    let current = watermark(&db, "peer").unwrap();
    assert!(page(&db, &current.scope, 0, None).is_ok());
    crate::begin_service(&mut db, "actor-2").unwrap();
    assert!(page(&db, &current.scope, 0, None).is_err());
    let mut calls = 0;
    while let Some(n) = crate::collect(&mut db).unwrap() {
        assert!(n <= GC_ROWS);
        calls += 1;
        assert!(calls < 100);
    }
    let views: i64 = db
        .query_row(
            "SELECT count(*) FROM desktop_catalog_change_views",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(views, 1);
}
#[test]
fn reads_use_indexed_revision_ranges() {
    let (db, _, _) = populate(20_000);
    let mut stmt=db.prepare("EXPLAIN QUERY PLAN SELECT revision,key,json,bytes FROM desktop_catalog_changes WHERE generation=?1 AND (revision,key)>(?2,?3) AND revision<=?4 ORDER BY revision,key LIMIT ?5").unwrap();
    let plan: Vec<String> = stmt
        .query_map(params![1, 1, "", 100, 129], |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|s| s.contains("SEARCH desktop_catalog_changes USING INDEX")
                && s.contains("generation=?")
                && s.contains("(revision,key)>(?,?)")),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .all(|s| !s.contains("SCAN") && !s.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

#[test]
fn large_payload_pages_and_many_removed_aliases_do_not_pin_history() {
    let (mut db, source, seq) = populate(0);
    let mark = watermark(&db, "peer").unwrap();
    for n in 0..3 {
        let mut large = item(n);
        large.row["payload"] = json!("x".repeat(700_000));
        put(
            &mut db,
            &source,
            &delta(seq + n as u64, n as i64 + 1, vec![large], vec![]),
        );
    }
    let mut next = None;
    for _ in 0..3 {
        let p = page(&db, &mark.scope, 0, next.as_ref()).unwrap();
        assert_eq!(p.items.len(), 1);
        next = p.next;
    }
    assert!(next.is_none());
    crate::remove_source(&mut db, &source).unwrap();
    for n in 0..130 {
        let source = Source {
            computer: format!("peer-{n}"),
            generation: n + 2,
            ..source.clone()
        };
        connect(&mut db, &source, &hello()).unwrap();
        put(&mut db, &source, &snapshot(2, 0, true, 0, vec![item(0)]));
        put(
            &mut db,
            &source,
            &delta(3, 1, vec![item(1)], vec![item(0).key]),
        );
        crate::remove_source(&mut db, &source).unwrap();
        let mut calls = 0;
        while let Some(deleted) = crate::collect(&mut db).unwrap() {
            assert!(deleted <= GC_ROWS);
            calls += 1;
            assert!(calls < 50);
        }
    }
    let (views,items,bytes):(i64,i64,i64)=db.query_row(
        "SELECT (SELECT count(*) FROM desktop_catalog_change_views),items,bytes FROM desktop_catalog_change_totals",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!((views, items, bytes), (0, 0, 0));
}

#[test]
fn an_empty_dirty_current_view_cannot_orphan_an_obsolete_journal() {
    let (mut db, current, _) = populate(1);
    let other = Source {
        computer: "removed".into(),
        ..current
    };
    connect(&mut db, &other, &hello()).unwrap();
    put(&mut db, &other, &snapshot(2, 0, true, 0, vec![item(0)]));
    put(&mut db, &other, &delta(3, 1, vec![item(1)], vec![]));
    crate::remove_source(&mut db, &other).unwrap();
    db.execute(
        "UPDATE desktop_catalog_change_views SET dirty=1 WHERE generation=1",
        [],
    )
    .unwrap();
    crate::collect(&mut db).unwrap();
    let retained: i64 = db
        .query_row(
            "SELECT count(*) FROM desktop_catalog_generations WHERE id=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, 1,
        "row GC must retain the journal's generation until journal GC finishes"
    );
    let mut calls = 0;
    while let Some(n) = crate::collect(&mut db).unwrap() {
        assert!(n <= GC_ROWS);
        calls += 1;
        assert!(calls < 20);
    }
    let (views,items):(i64,i64)=db.query_row("SELECT (SELECT count(*) FROM desktop_catalog_change_views),items FROM desktop_catalog_change_totals",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((views, items), (1, 0));
}
