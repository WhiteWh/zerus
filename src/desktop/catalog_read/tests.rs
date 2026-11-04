use super::*;
use crate::desktop::store::Location;
use zerus_watch_protocol::CatalogItem;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    feed: feed::Broker,
    producer: std::sync::Arc<feed::Producer>,
    epoch: String,
}
impl Fixture {
    fn new(rows: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&location, &epoch).unwrap();
        let mut feed = feed::Broker::new(&epoch);
        let producer = feed.open(Some(&json!({"abi":feed::ABI,"producer":uuid::Uuid::new_v4().to_string(),"mode":"display_only"}))).unwrap();
        let mut fixture = Self {
            _temp: temp,
            store,
            feed,
            producer,
            epoch,
        };
        fixture.replace(rows, 1);
        fixture
    }
    fn replace(&mut self, rows: usize, generation: u64) {
        let pages = rows.div_ceil(128).max(1);
        self.apply(feed::Packet::LegacyBegin {
            computer: "peer".into(),
            generation,
            metadata: json!({"machine_id":"example-machine"}),
            rows,
            pages,
        });
        for index in 0..pages {
            let items = (index * 128..((index + 1) * 128).min(rows))
                .map(item)
                .collect();
            self.apply(feed::Packet::LegacyPage {
                computer: "peer".into(),
                generation,
                index,
                items,
            });
        }
        self.apply(feed::Packet::LegacyComplete {
            computer: "peer".into(),
            generation,
        });
    }
    fn apply(&mut self, packet: feed::Packet) {
        self.feed
            .apply(&mut self.store, &self.producer, &packet)
            .unwrap();
    }
    fn read(&self, reader: &mut Reader, cursor: Option<&str>, now: Instant) -> Value {
        reader.at(
            &self.store,
            &self.feed,
            &self.epoch,
            &query("catalog_page", "peer", cursor),
            now,
        )
    }
}
fn item(n: usize) -> CatalogItem {
    CatalogItem {
        key: format!("run:run-{n}"),
        row: json!({"run_id":format!("run-{n}"),"name":format!("codex/example/{n}"),"process_work":{"count":3,"future":true},"unknown":{"kept":"exact"}}),
    }
}
fn query(op: &str, computer: &str, cursor: Option<&str>) -> Value {
    json!({"request_id":uuid::Uuid::new_v4().to_string(),"op":op,"computer":computer,"cursor":cursor})
}
fn token(value: &Value) -> String {
    assert_eq!(value["accepted"], true, "{value}");
    value["next_cursor"].as_str().unwrap().to_owned()
}

#[test]
fn many_pages_keep_one_cursor_and_preserve_raw_rows_without_writes() {
    let f = Fixture::new(10_000);
    let mut reader = Reader::default();
    let now = Instant::now();
    let changes = f.store.db.total_changes();
    let mut cursor = None;
    let mut count = 0;
    let mut pages = 0;
    loop {
        let value = f.read(&mut reader, cursor.as_deref(), now);
        assert_eq!(value["accepted"], true, "{value}");
        assert_eq!(value["complete"], true);
        assert_eq!(value["authority"], false);
        assert_eq!(value["durable"], false);
        let rows = value["items"].as_array().unwrap();
        assert!(rows.len() <= 128);
        for row in rows {
            assert_eq!(row["key"], item(count).key);
            assert_eq!(row["row"], item(count).row);
            count += 1;
        }
        protocol::encode(&crate::desktop::frame(
            &f.epoch,
            u64::MAX,
            "response",
            value.clone(),
        ))
        .unwrap();
        cursor = value["next_cursor"].as_str().map(str::to_owned);
        assert_eq!(reader.walks.len(), usize::from(cursor.is_some()));
        pages += 1;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(count, 10_000);
    assert_eq!(pages, 79);
    assert_eq!(f.store.db.total_changes(), changes);
    f.producer.close();
    assert_eq!(
        f.read(&mut reader, None, now)["complete"],
        false,
        "atomic closure must invalidate coverage before retirement"
    );
}

#[test]
fn cursors_are_one_use_scoped_private_and_closed_explicitly() {
    let f = Fixture::new(1000);
    let mut reader = Reader::default();
    let now = Instant::now();
    let first = token(&f.read(&mut reader, None, now));
    let wrong = reader.at(
        &f.store,
        &f.feed,
        &f.epoch,
        &query("catalog_page", "other", Some(&first)),
        now,
    );
    assert_eq!(wrong["code"], "catalog_cursor_scope");
    assert_eq!(reader.walks.len(), 1);
    let mut restarted = Reader::default();
    assert_eq!(
        f.read(&mut restarted, Some(&first), now)["code"],
        "catalog_cursor_expired"
    );
    let second = token(&f.read(&mut reader, Some(&first), now));
    assert_ne!(first, second);
    assert_eq!(
        f.read(&mut reader, Some(&first), now)["code"],
        "catalog_cursor_expired"
    );
    let close = query("catalog_close", "peer", Some(&second));
    assert_eq!(
        reader.at(&f.store, &f.feed, &f.epoch, &close, now)["closed"],
        true
    );
    assert_eq!(
        reader.at(&f.store, &f.feed, &f.epoch, &close, now)["accepted"],
        false
    );
    assert!(reader.walks.is_empty());
    for value in [
        query("catalog_page", "bad alias", None),
        query("catalog_close", "peer", None),
        query("catalog_page", "peer", Some("bad")),
        json!({"op":"catalog_page","computer":"peer"}),
        json!({"op":"catalog_page","request_id":uuid::Uuid::new_v4().to_string(),"computer":"peer","after":1}),
    ] {
        assert_eq!(
            reader.at(&f.store, &f.feed, &f.epoch, &value, now)["accepted"],
            false
        );
    }
}

#[test]
fn walk_capacity_idle_and_absolute_expiry_never_grow_metadata() {
    let f = Fixture::new(1000);
    let mut reader = Reader::default();
    let now = Instant::now();
    let mut tokens = Vec::new();
    for _ in 0..WALKS {
        tokens.push(token(&f.read(&mut reader, None, now)));
    }
    assert_eq!(reader.walks.len(), WALKS);
    assert_eq!(
        f.read(&mut reader, None, now)["code"],
        "catalog_walk_capacity"
    );
    let next = token(&f.read(&mut reader, Some(&tokens[0]), now + Duration::from_secs(1)));
    assert_eq!(reader.walks.len(), WALKS, "rotation needs no extra slot");
    assert_eq!(
        f.read(
            &mut reader,
            Some(&next),
            now + IDLE + Duration::from_secs(1)
        )["code"],
        "catalog_cursor_expired"
    );
    assert!(reader.walks.is_empty());
    let mut current = token(&f.read(&mut reader, None, now));
    for seconds in [100, 200, 300, 400, 500] {
        current = token(&f.read(
            &mut reader,
            Some(&current),
            now + Duration::from_secs(seconds),
        ));
    }
    assert_eq!(
        f.read(&mut reader, Some(&current), now + LIFETIME)["code"],
        "catalog_cursor_expired"
    );
    assert!(reader.walks.is_empty());
}

#[test]
fn replaced_generation_and_storage_failures_refuse_only_the_read() {
    let mut f = Fixture::new(300);
    let mut reader = Reader::default();
    let now = Instant::now();
    let old = token(&f.read(&mut reader, None, now));
    f.replace(300, 2);
    assert_eq!(
        f.read(&mut reader, Some(&old), now)["code"],
        "catalog_read_refused"
    );
    assert!(reader.walks.is_empty());
    f.store
        .db
        .execute_batch("ALTER TABLE desktop_catalog_sources RENAME TO broken_sources")
        .unwrap();
    assert_eq!(
        f.read(&mut reader, None, now)["code"],
        "catalog_read_refused"
    );
    f.store
        .db
        .execute_batch("ALTER TABLE broken_sources RENAME TO desktop_catalog_sources")
        .unwrap();
    assert_eq!(f.read(&mut reader, None, now)["complete"], true);
    assert_eq!(f.store.effect_status()["revoked"], true);
}

#[test]
fn encoded_output_and_corrupt_row_failures_release_the_walk() {
    let f = Fixture::new(300);
    let mut reader = Reader::default();
    let now = Instant::now();
    let old = token(&f.read(&mut reader, None, now));
    // Seed a bounded stored row whose escaped key pushes the IPC envelope over
    // its limit. Stored size is checked before allocation, encoded size after.
    let key = "\u{1}".repeat(200_000);
    f.store.db.execute("UPDATE desktop_catalog_rows SET key=?1,json='{}',bytes=200066 WHERE id=(SELECT min(id)+128 FROM desktop_catalog_rows)",[&key]).unwrap();
    assert_eq!(
        f.read(&mut reader, Some(&old), now)["code"],
        "catalog_read_refused"
    );
    assert!(reader.walks.is_empty());
    assert_eq!(
        f.read(&mut reader, Some(&old), now)["code"],
        "catalog_cursor_expired"
    );
    // Simulate damaged stored bytes, bypassing the normal writer's JSON
    // validation and derived-count trigger.
    f.store.db.execute_batch("DROP TRIGGER desktop_counts_update;
        UPDATE desktop_catalog_rows SET key='repaired',json='not-json',bytes=80 WHERE length(key)>1000").unwrap();
    // The malformed private body is never returned as a diagnostic.
    let first = token(&f.read(&mut reader, None, now));
    let result = f.read(&mut reader, Some(&first), now);
    assert_eq!(result["code"], "catalog_read_refused");
    assert!(!result.to_string().contains("not-json"));
}

impl Fixture {
    fn watch(rows: usize) -> (Self, u64) {
        use zerus_watch_protocol::{Body, Frame, Hello, Limits, Snapshot};
        let mut f = Self::new(0);
        f.apply(feed::Packet::Watch {
            computer: "peer".into(),
            generation: 2,
            frame: Box::new(Frame {
                v: 1,
                seq: 1,
                body: Body::Hello(Hello {
                    machine_id: Some("example-machine".into()),
                    host: "example-host".into(),
                    hgs_version: "1.56.0".into(),
                    stream: "fixture-stream".into(),
                    heartbeat_ms: 1000,
                    max_frame_bytes: zerus_watch_protocol::MAX_FRAME_BYTES,
                    topics: vec!["catalog".into()],
                    limits: Limits::default(),
                }),
            }),
        });
        let pages = rows.div_ceil(128).max(1);
        for p in 0..pages {
            f.apply(feed::Packet::Watch {
                computer: "peer".into(),
                generation: 2,
                frame: Box::new(Frame {
                    v: 1,
                    seq: p as u64 + 2,
                    body: Body::Snapshot(Snapshot {
                        topic: "catalog".into(),
                        key: None,
                        value: None,
                        items: Some(
                            (p * 128..((p + 1) * 128).min(rows))
                                .map(|n| serde_json::to_value(item(n)).unwrap())
                                .collect(),
                        ),
                        page: p as u32,
                        last: p + 1 == pages,
                        cursor: 0,
                    }),
                }),
            });
        }
        (f, pages as u64 + 2)
    }
    fn delta(
        &mut self,
        seq: u64,
        revision: i64,
        upsert: Vec<CatalogItem>,
        remove: Vec<String>,
    ) -> Value {
        use zerus_watch_protocol::{Body, Delta, Frame, Removal};
        self.feed
            .apply(
                &mut self.store,
                &self.producer,
                &feed::Packet::Watch {
                    computer: "peer".into(),
                    generation: 2,
                    frame: Box::new(Frame {
                        v: 1,
                        seq,
                        body: Body::Delta(Delta {
                            topic: "catalog".into(),
                            cursor: revision,
                            upsert,
                            remove: remove
                                .into_iter()
                                .map(|key| Removal { key, reason: None })
                                .collect(),
                        }),
                    }),
                },
            )
            .unwrap()
            .unwrap()
    }
    fn changes(
        &self,
        reader: &mut Reader,
        mark: &Value,
        since: &str,
        cursor: Option<&str>,
        now: Instant,
    ) -> Value {
        let q = json!({"request_id":uuid::Uuid::new_v4().to_string(),"op":"catalog_changes","computer":"peer",
            "epoch":mark["epoch"],"incarnation":mark["incarnation"],"generation":mark["generation"],"projection":mark["projection"],
            "since_revision":since,"cursor":cursor});
        reader.at(&self.store, &self.feed, &self.epoch, &q, now)
    }
}
#[test]
fn large_baseline_keeps_first_watermark_and_busy_hints_need_only_one_change_page() {
    let (mut f, seq) = Fixture::watch(20_000);
    let mut reader = Reader::default();
    let now = Instant::now();
    let first = f.read(&mut reader, None, now);
    let mut next = first["next_cursor"].as_str().map(str::to_owned);
    assert_eq!(first["start_revision"], "0");
    assert_eq!(first["changes_abi"], CHANGES_ABI);
    let mut changed = item(0);
    changed.row["name"] = json!("renamed");
    let hint = f.delta(
        seq,
        1,
        vec![changed.clone(), item(20_000)],
        vec![item(500).key],
    );
    assert_eq!(hint["revision"], "1");
    assert_eq!(hint["projection"], first["projection"]);
    assert_eq!(hint["reset_required"], false);
    let mut count = first["items"].as_array().unwrap().len();
    let mut requests = 1;
    while let Some(c) = next {
        let p = f.read(&mut reader, Some(&c), now);
        assert_eq!(p["accepted"], true, "{p}");
        assert_eq!(p["start_revision"], "0");
        assert_eq!(p["revision"], "1");
        assert_eq!(p["stale"], true);
        assert_eq!(p["complete"], false);
        count += p["items"].as_array().unwrap().len();
        requests += 1;
        next = p["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(requests, 157);
    assert_eq!(count, 19_999);
    let deltas = f.changes(&mut reader, &first, "0", None, now);
    assert_eq!(deltas["accepted"], true, "{deltas}");
    assert_eq!(deltas["through_revision"], "1");
    assert_eq!(deltas["next_cursor"], Value::Null);
    let rows = deltas["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows
        .iter()
        .any(|r| r["key"] == changed.key && r["row"] == changed.row));
    assert!(rows
        .iter()
        .any(|r| r["key"] == item(500).key && r["op"] == "remove"));
    assert!(reader.walks.is_empty());
}
#[test]
fn changes_share_capacity_one_use_ttl_and_fixed_highwater() {
    let (mut f, seq) = Fixture::watch(0);
    let mut reader = Reader::default();
    let now = Instant::now();
    let mark = f.read(&mut reader, None, now);
    f.delta(seq, 1, (0..300).map(item).collect(), vec![]);
    let mut tokens = Vec::new();
    for _ in 0..WALKS {
        tokens.push(token(&f.changes(&mut reader, &mark, "0", None, now)));
    }
    assert_eq!(
        f.changes(&mut reader, &mark, "0", None, now)["code"],
        "catalog_walk_capacity"
    );
    f.delta(seq + 1, 2, vec![], (0..300).map(|n| item(n).key).collect());
    let second = f.changes(&mut reader, &mark, "0", Some(&tokens[0]), now);
    assert_eq!(second["through_revision"], "1");
    assert_eq!(second["revision"], "2");
    assert_eq!(second["stale"], true);
    assert!(second["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["op"] == "upsert"));
    assert_eq!(
        f.changes(&mut reader, &mark, "0", Some(&tokens[0]), now)["code"],
        "catalog_cursor_expired"
    );
    assert_eq!(
        f.changes(&mut reader, &mark, "0", Some(&tokens[1]), now + IDLE)["code"],
        "catalog_cursor_expired"
    );
    assert!(reader.walks.is_empty());
}
#[test]
fn changes_gap_and_disconnect_reset_without_complete_zero_or_retained_cursor() {
    let (mut f, seq) = Fixture::watch(300);
    let mut reader = Reader::default();
    let now = Instant::now();
    let mark = f.read(&mut reader, None, now);
    let baseline = token(&mark);
    f.delta(seq, 1, (0..300).map(item).collect(), vec![]);
    let changes = token(&f.changes(&mut reader, &mark, "0", None, now));
    f.store
        .db
        .execute(
            "UPDATE desktop_catalog_change_views SET floor=1,dirty=1",
            [],
        )
        .unwrap();
    let gap = f.changes(&mut reader, &mark, "0", Some(&changes), now);
    assert_eq!(gap["code"], "catalog_changes_gap");
    assert_eq!(gap["reset_required"], true);
    f.apply(feed::Packet::Offline {
        computer: "peer".into(),
        generation: 2,
    });
    assert_eq!(
        f.read(&mut reader, Some(&baseline), now)["code"],
        "catalog_read_refused"
    );
    let offline = f.read(&mut reader, None, now);
    assert_eq!(offline["online"], false);
    assert_eq!(offline["complete"], false);
    assert_eq!(offline["last_known"], true);
    assert_eq!(offline["start_revision"], Value::Null);
    assert_eq!(offline["items"].as_array().unwrap().len(), 128);
    let reset = f.changes(&mut reader, &mark, "1", None, now);
    assert_eq!(reset["code"], "catalog_changes_reset");
    let new_epoch = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        reader.at(
            &f.store,
            &f.feed,
            &new_epoch,
            &query("catalog_page", "peer", None),
            now
        )["online"],
        false
    );
}

#[test]
fn actor_epoch_and_change_start_cannot_be_rebound_by_a_continuation() {
    let (mut f, seq) = Fixture::watch(300);
    let mut reader = Reader::default();
    let now = Instant::now();
    let mark = f.read(&mut reader, None, now);
    let baseline = token(&mark);
    let new_epoch = uuid::Uuid::new_v4().to_string();
    let wrong_epoch = reader.at(
        &f.store,
        &f.feed,
        &new_epoch,
        &query("catalog_page", "peer", Some(&baseline)),
        now,
    );
    assert_eq!(wrong_epoch["code"], "catalog_cursor_expired");
    assert!(reader.walks.is_empty());
    f.delta(seq, 1, (0..300).map(item).collect(), vec![]);
    let cursor = token(&f.changes(&mut reader, &mark, "0", None, now));
    let changed_start = f.changes(&mut reader, &mark, "1", Some(&cursor), now);
    assert_eq!(changed_start["code"], "catalog_changes_reset");
    assert!(reader.walks.is_empty());
    assert_eq!(
        f.changes(&mut reader, &mark, "0", Some(&cursor), now)["code"],
        "catalog_cursor_expired"
    );
    for invalid in ["01", "-1", "1.0", "9223372036854775808"] {
        assert_eq!(
            f.changes(&mut reader, &mark, invalid, None, now)["accepted"],
            false
        );
    }
}
