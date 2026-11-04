use super::*;
use crate::desktop::store::Location;

fn fixture() -> (tempfile::TempDir, Store, Broker) {
    let temp = tempfile::tempdir().unwrap();
    let location = Location {
        root: temp.path().canonicalize().unwrap().join("state/desktop"),
        configuration: "a".repeat(64),
    };
    location.prepare().unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    let store = Store::open(&location, &epoch).unwrap();
    (temp, store, Broker::new(&epoch))
}
fn role() -> Value {
    json!({"abi":ABI,"producer":uuid::Uuid::new_v4().to_string(),"mode":"display_only"})
}

#[test]
fn failed_internal_observation_invalidates_only_its_original_generation() {
    let (_temp, mut store, mut broker) = fixture();
    let producer = broker.claim_internal().unwrap();
    broker.apply(&mut store, &producer, &begin(1)).unwrap();
    assert!(broker.available("peer"));
    broker.invalidate(&producer, &begin(1));
    assert!(!broker.available("peer"));
    broker.apply(&mut store, &producer, &begin(2)).unwrap();
    assert!(broker.available("peer"));
    broker.invalidate(&producer, &begin(1));
    assert!(
        broker.available("peer"),
        "late failure must not invalidate replacement"
    );
}

#[test]
fn expected_aliases_are_bounded_and_an_older_listing_keeps_the_previous_set() {
    let (_temp, mut store, mut broker) = fixture();
    let producer = broker.claim_internal().unwrap();
    for (generation, metadata) in [
        (1, json!({"peers":["pending"]})),
        (2, json!({"host":"older-local"})),
    ] {
        broker
            .apply(
                &mut store,
                &producer,
                &Packet::LegacyBegin {
                    computer: String::new(),
                    generation,
                    metadata,
                    rows: 0,
                    pages: 1,
                },
            )
            .unwrap();
        broker
            .apply(
                &mut store,
                &producer,
                &Packet::LegacyPage {
                    computer: String::new(),
                    generation,
                    index: 0,
                    items: vec![],
                },
            )
            .unwrap();
        broker
            .apply(
                &mut store,
                &producer,
                &Packet::LegacyComplete {
                    computer: String::new(),
                    generation,
                },
            )
            .unwrap();
        assert_eq!(
            broker.expected(),
            Some(&BTreeSet::from([String::new(), "pending".into()]))
        );
        let counts = super::super::counts::projection(
            &store.db,
            &broker.epoch,
            |c| broker.available(c),
            broker.expected(),
        )
        .unwrap();
        assert_eq!(counts["aggregate"]["attention"], Value::Null);
    }
    assert!(expected_peers(&json!({"peers":"bad"})).is_err());
    assert!(expected_peers(&json!({"peers":["@bad"]})).is_err());
    let peers: Vec<_> = (0..64).map(|n| format!("peer-{n}")).collect();
    assert!(expected_peers(&json!({"peers":peers})).is_err());
    assert_eq!(broker.expected().unwrap().len(), 2);
}
fn begin(generation: u64) -> Packet {
    Packet::LegacyBegin {
        computer: "peer".into(),
        generation,
        metadata: json!({"machine_id":"example"}),
        rows: 0,
        pages: 1,
    }
}

#[test]
fn exact_producer_retirement_blocks_old_callbacks_and_preserves_replacements() {
    let (_temp, mut store, mut broker) = fixture();
    let first = broker.open(Some(&role())).unwrap();
    assert!(broker.open(Some(&role())).is_err());
    broker.apply(&mut store, &first, &begin(1)).unwrap();
    first.close();
    assert!(broker
        .apply(&mut store, &first, &Packet::Keepalive {})
        .is_err());
    assert!(
        broker.open(Some(&role())).is_err(),
        "retirement must finish first"
    );
    assert!(broker.retire(&mut store).unwrap());
    let second = broker.open(Some(&role())).unwrap();
    assert!(second.incarnation > first.incarnation);
    broker.apply(&mut store, &second, &begin(1)).unwrap();
    assert!(broker.apply(&mut store, &first, &begin(2)).is_err());
    broker.failed(&first);
    assert!(!broker.status()["error"].as_bool().unwrap());
    assert!(second.alive());
    broker
        .apply(
            &mut store,
            &second,
            &Packet::LegacyPage {
                computer: "peer".into(),
                generation: 1,
                index: 0,
                items: vec![],
            },
        )
        .unwrap();
    broker
        .apply(
            &mut store,
            &second,
            &Packet::LegacyComplete {
                computer: "peer".into(),
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(broker.status()["sources"][0]["complete"], true);
    assert_eq!(broker.status()["sources"][0]["authority"], false);
    broker.stop(&mut store).unwrap();
    assert!(!catalog::status(&store.db, "peer").unwrap().complete);
}

#[test]
fn retained_ingress_is_bounded_and_close_cancels_waiters() {
    let quota = Arc::new(Ingress::default());
    let producer = Arc::new(Producer {
        incarnation: 1,
        alive: AtomicBool::new(true),
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let permit = quota
        .reserve(INGRESS_BYTES - 1024, &producer, &stopped)
        .unwrap();
    assert_eq!(quota.usage.lock().unwrap().bytes, INGRESS_BYTES);
    let waiter = {
        let (q, p, s) = (quota.clone(), producer.clone(), stopped.clone());
        std::thread::spawn(move || q.reserve(1, &p, &s).is_none())
    };
    producer.close();
    assert!(waiter.join().unwrap());
    drop(permit);
    assert_eq!(quota.usage.lock().unwrap().bytes, 0);
    let producer = Producer {
        incarnation: 2,
        alive: AtomicBool::new(true),
    };
    let permits: Vec<_> = (0..INGRESS_ITEMS)
        .map(|_| quota.reserve(0, &producer, &stopped).unwrap())
        .collect();
    assert_eq!(quota.usage.lock().unwrap().items, INGRESS_ITEMS);
    stopped.store(true, Ordering::Release);
    assert!(quota.reserve(0, &producer, &stopped).is_none());
    drop(permits);
    assert_eq!(quota.usage.lock().unwrap().items, 0);
}

#[test]
fn role_parser_and_page_decode_refuse_unknown_and_oversized_input() {
    let (_temp, _store, mut broker) = fixture();
    for value in [
        json!({}),
        json!({"abi":ABI,"producer":"bad","mode":"display_only"}),
        json!({"abi":"newer","producer":uuid::Uuid::new_v4().to_string(),"mode":"display_only"}),
        json!({"abi":ABI,"producer":uuid::Uuid::new_v4().to_string(),"mode":"writer"}),
    ] {
        assert!(broker.open(Some(&value)).is_err());
    }
    assert!(serde_json::from_value::<Packet>(json!({"t":"keepalive","extra":true})).is_err());
    let rows: Vec<_> = (0..129)
        .map(|n| json!({"key":format!("run:{n}"),"row":{"name":"example"}}))
        .collect();
    assert!(serde_json::from_value::<Packet>(
        json!({"t":"legacy_page","computer":"peer","generation":1,"index":0,"items":rows})
    )
    .is_err());
}

#[test]
fn retirement_failure_pauses_replacement_and_recovers_after_storage_repair() {
    let (_temp, mut store, mut broker) = fixture();
    let producer = broker.open(Some(&role())).unwrap();
    broker.apply(&mut store, &producer, &begin(1)).unwrap();
    producer.close();
    store
        .db
        .execute_batch("ALTER TABLE desktop_catalog_sources RENAME TO broken_sources")
        .unwrap();
    assert!(broker.retire(&mut store).is_err());
    assert_eq!(broker.status()["error"], true);
    assert!(broker.open(Some(&role())).is_err());
    store
        .db
        .execute_batch("ALTER TABLE broken_sources RENAME TO desktop_catalog_sources")
        .unwrap();
    broker.retry_at = Instant::now();
    assert!(broker.retire(&mut store).unwrap());
    assert!(broker.open(Some(&role())).is_ok());
}
