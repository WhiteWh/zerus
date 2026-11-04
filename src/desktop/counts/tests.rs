use super::*;
use zerus_desktop_catalog::{self as catalog, Source};
use zerus_watch_protocol::{self as watch, Body, Frame};

fn fixture() -> (Connection, Source) {
    let mut db = Connection::open_in_memory().unwrap();
    catalog::migrate(&db).unwrap();
    register(&db).unwrap();
    migrate(&db).unwrap();
    catalog::begin_service(&mut db, "epoch-1").unwrap();
    (
        db,
        Source {
            computer: "peer".into(),
            service_epoch: "epoch-1".into(),
            incarnation: 1,
            generation: 1,
        },
    )
}
fn item(n: usize) -> watch::CatalogItem {
    watch::CatalogItem {
        key: format!("run:r-{n}"),
        row: json!({
        "name":format!("codex/example/s-{n}"),"run_id":format!("r-{n}"),"conversation_id":format!("c-{n}"),
        "tracked":true,"phase":"input","activity":"busy","attention_signature":"sig"}),
    }
}
fn replace(db: &mut Connection, source: &Source, items: &[watch::CatalogItem]) {
    let pages = items.len().div_ceil(catalog::PAGE_ROWS).max(1);
    catalog::legacy::begin(db, source, &json!({}), items.len(), pages).unwrap();
    if items.is_empty() {
        catalog::legacy::page(db, source, 0, &[]).unwrap();
    } else {
        for (index, chunk) in items.chunks(catalog::PAGE_ROWS).enumerate() {
            catalog::legacy::page(db, source, index, chunk).unwrap();
            assert_eq!(
                projection(db, "epoch-1", |_| true, None).unwrap()["aggregate"]["attention"],
                Value::Null
            );
        }
    }
    catalog::legacy::complete(db, source).unwrap();
}

#[test]
fn many_sessions_are_counted_in_bounded_ingress_pages_without_status_row_reads() {
    let (mut db, source) = fixture();
    let items: Vec<_> = (0..20_000).map(item).collect();
    replace(&mut db, &source, &items);
    let counts = projection(&db, "epoch-1", |_| true, None).unwrap();
    assert_eq!(counts["aggregate"]["total"], 20_000);
    assert_eq!(counts["aggregate"]["attention"], 20_000);
    assert_eq!(counts["scope"], "shared_host_state");
    // Authorizer proves a status read cannot touch the rows or call the row
    // projector; it reads only bounded metadata and maintained counters.
    db.authorizer(Some(|ctx: rusqlite::hooks::AuthContext<'_>| {
        match ctx.action {
            rusqlite::hooks::AuthAction::Read {
                table_name: "desktop_catalog_rows",
                ..
            }
            | rusqlite::hooks::AuthAction::Function {
                function_name: "desktop_shared_flags",
            } => rusqlite::hooks::Authorization::Deny,
            _ => rusqlite::hooks::Authorization::Allow,
        }
    }));
    assert_eq!(projection(&db, "epoch-1", |_| true, None).unwrap(), counts);
}

#[test]
fn replacement_restart_offline_and_unavailable_counts_never_claim_complete_zero() {
    let (mut db, mut source) = fixture();
    replace(&mut db, &source, &[item(1)]);
    catalog::disconnect_many(&mut db, &[source.clone()]).unwrap();
    let counts = projection(&db, "epoch-1", |_| true, None).unwrap();
    assert_eq!(counts["aggregate"]["attention"], Value::Null);
    assert_eq!(counts["computers"][0]["total"], 1);
    assert_eq!(counts["computers"][0]["online"], false);
    source.generation = 2;
    replace(&mut db, &source, &[]);
    assert_eq!(
        projection(&db, "epoch-1", |_| true, None).unwrap()["aggregate"]["attention"],
        0
    );
    assert_eq!(
        projection(&db, "epoch-1", |_| false, None).unwrap()["aggregate"]["attention"],
        Value::Null
    );
    catalog::begin_service(&mut db, "epoch-2").unwrap();
    let counts = projection(&db, "epoch-2", |_| true, None).unwrap();
    assert_eq!(counts["computers"][0]["online"], false);
    assert_eq!(counts["aggregate"]["attention"], Value::Null);
}

#[test]
fn shared_read_mute_archive_background_and_deltas_reuse_the_existing_projection() {
    let (mut db, source) = fixture();
    // Use a real watch source so deltas exercise update/removal triggers.
    let hello = Frame {
        v: 1,
        seq: 1,
        body: Body::Hello(watch::Hello {
            machine_id: None,
            host: "peer".into(),
            hgs_version: "example".into(),
            stream: "s".into(),
            heartbeat_ms: 10_000,
            max_frame_bytes: watch::MAX_FRAME_BYTES,
            topics: vec!["catalog".into()],
            limits: watch::Limits::default(),
        }),
    };
    catalog::connect(&mut db, &source, &hello).unwrap();
    let mut rows: Vec<_> = (0..5).map(item).collect();
    rows[1].row["read"] = json!({"conversation_id":"c-1","attention_signature":"sig"});
    rows[2].row["notifications"] = json!({"muted":true});
    rows[3].row["state"] = json!("archived");
    rows[4].row["phase"] = json!("idle");
    rows[4].row["activity"] = json!("idle");
    rows[4].row["reply_id"] = json!("reply");
    rows[4].row["process_work"] = json!({"count":1,"known":true});
    let expected = rows
        .iter()
        .map(|r| zerus_client_core::view::row(&r.row))
        .filter(|r| !r.muted && r.needs_attention)
        .count();
    let frame = Frame {
        v: 1,
        seq: 2,
        body: Body::Snapshot(watch::Snapshot {
            topic: "catalog".into(),
            key: None,
            items: Some(
                rows.iter()
                    .map(|r| serde_json::to_value(r).unwrap())
                    .collect(),
            ),
            value: None,
            page: 0,
            last: true,
            cursor: 1,
        }),
    };
    catalog::apply(&mut db, &source, &frame).unwrap();
    assert_eq!(
        projection(&db, "epoch-1", |_| true, None).unwrap()["aggregate"]["attention"],
        expected
    );
    rows[0].row["read"] = json!({"conversation_id":"c-0","attention_signature":"sig"});
    let frame = Frame {
        v: 1,
        seq: 3,
        body: Body::Delta(watch::Delta {
            topic: "catalog".into(),
            cursor: 2,
            upsert: vec![rows[0].clone()],
            remove: vec![watch::Removal {
                key: rows[3].key.clone(),
                reason: None,
            }],
        }),
    };
    catalog::apply(&mut db, &source, &frame).unwrap();
    let counts = projection(&db, "epoch-1", |_| true, None).unwrap();
    assert_eq!(counts["aggregate"]["attention"], expected - 1);
    assert_eq!(counts["aggregate"]["total"], 4);
}

#[test]
fn schema_nine_migration_does_not_read_existing_rows_or_invent_counts() {
    let mut db = Connection::open_in_memory().unwrap();
    catalog::migrate(&db).unwrap();
    catalog::begin_service(&mut db, "epoch-1").unwrap();
    let source = Source {
        computer: "peer".into(),
        service_epoch: "epoch-1".into(),
        incarnation: 1,
        generation: 1,
    };
    // Build old data before installing the count schema.
    catalog::legacy::begin(&mut db, &source, &json!({}), 1, 1).unwrap();
    catalog::legacy::page(&mut db, &source, 0, &[item(1)]).unwrap();
    catalog::legacy::complete(&mut db, &source).unwrap();
    register(&db).unwrap();
    db.authorizer(Some(|ctx: rusqlite::hooks::AuthContext<'_>| {
        match ctx.action {
            rusqlite::hooks::AuthAction::Read {
                table_name: "desktop_catalog_rows",
                ..
            } => rusqlite::hooks::Authorization::Deny,
            _ => rusqlite::hooks::Authorization::Allow,
        }
    }));
    migrate(&db).unwrap();
    let counts = projection(&db, "epoch-1", |_| true, None).unwrap();
    assert_eq!(counts["aggregate"]["attention"], Value::Null);
    assert_eq!(counts["computers"][0]["total"], Value::Null);
}

#[test]
fn an_expected_peer_without_its_first_hello_keeps_aggregate_incomplete() {
    let (mut db, mut source) = fixture();
    source.computer = String::new();
    catalog::legacy::begin(&mut db, &source, &json!({"peers":["unreachable"]}), 0, 1).unwrap();
    catalog::legacy::page(&mut db, &source, 0, &[]).unwrap();
    catalog::legacy::complete(&mut db, &source).unwrap();
    let expected = BTreeSet::from([String::new(), "unreachable".into()]);
    let counts = projection(&db, "epoch-1", |_| true, Some(&expected)).unwrap();
    assert_eq!(counts["aggregate"]["attention"], Value::Null);
    assert_eq!(counts["aggregate"]["total"], Value::Null);
    assert_eq!(counts["computers"][1]["computer"], "unreachable");
    assert_eq!(counts["computers"][1]["online"], false);
}
