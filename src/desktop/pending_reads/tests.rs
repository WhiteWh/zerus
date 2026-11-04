use super::*;
use crate::desktop::store::Location;
use zerus_client_core::desktop::pending_reads::Capture;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn mark(n: usize) -> reads::ReadMark {
    reads::ReadMark {
        computer: "example".into(),
        name: format!("agent/project/{n}"),
        run_id: format!("run-{n}"),
        conversation_id: format!("conversation-{n}"),
        reply_id: Some(format!("reply-{n}")),
        attention_signature: None,
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    location: Location,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: temp.path().canonicalize().unwrap().join("state/desktop"),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let store = Store::open(&location, &id()).unwrap();
        Self {
            _temp: temp,
            location,
            store,
        }
    }
    fn apply(&mut self, page: &Page) -> Value {
        let value = json!({"op":"import_pending_reads","request_id":id(),"page":page});
        frontend(&mut self.store, Validated::fixture(&value))
    }
    fn lookup(&self, page: &Page) -> Value {
        result_request(
            &self.store,
            &json!({"op":"pending_reads_import_result",
            "request_id":id(),"original_request_id":id(),"page":page}),
        )
    }
}

#[test]
fn all_capture_pages_enter_the_existing_queue_while_passive() {
    let mut f = Fixture::new();
    let marks: Vec<_> = (0..193).map(mark).collect();
    let capture = Capture::from_bytes(&serde_json::to_vec(&marks).unwrap()).unwrap();
    let mut after = 0;
    while after < capture.source().rows {
        let page = capture.page(after).unwrap();
        let receipt = f.apply(&page);
        assert_eq!(receipt["accepted"], true);
        assert_eq!(receipt["durable"], true);
        assert_eq!(receipt["delivered"], false);
        after = page.through();
        assert_eq!(receipt["through"], after);
    }
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 193);
    assert!(reads::reserve(&mut f.store.db, &id(), &id(), &id(), 0).is_err());
    let policy: bool = f
        .store
        .db
        .query_row(
            "SELECT enabled FROM desktop_delivery_policy WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!policy);
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 128).unwrap();
    assert_eq!(pending[0].mark, marks[0]);
    assert_eq!(pending[127].mark, marks[127]);
}

#[test]
fn lost_page_reply_resumes_from_existing_receipts_after_restart() {
    let mut f = Fixture::new();
    let bytes = serde_json::to_vec(&(0..130).map(mark).collect::<Vec<_>>()).unwrap();
    let capture = Capture::from_bytes(&bytes).unwrap();
    let first = capture.page(0).unwrap();
    assert_eq!(f.apply(&first)["durable"], true); // Original reply is then lost.
    drop(f.store);
    f.store = Store::open(&f.location, &id()).unwrap();
    assert_eq!(f.lookup(&first)["found"], true);
    let mut after = 0;
    while after < capture.source().rows {
        let page = capture.page(after).unwrap();
        assert_eq!(f.apply(&page)["durable"], true);
        after = page.through();
    }
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 130);
    let receipts: i64 = f
        .store
        .db
        .query_row("SELECT count(*) FROM desktop_read_requests", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(receipts, 130);
}

#[test]
fn repeat_after_retirement_does_not_resurrect_or_rebind_a_mark() {
    let mut f = Fixture::new();
    let m = mark(0);
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![m.clone()]).unwrap()).unwrap();
    let page = capture.page(0).unwrap();
    assert_eq!(f.apply(&page)["durable"], true);
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 1)
        .unwrap()
        .remove(0);
    reads::observe(
        &mut f.store.db,
        &pending,
        &reads::Snapshot {
            computer: &m.computer,
            name: &m.name,
            run: Some(&m.run_id),
            conversation: Some(&m.conversation_id),
            reply: m.reply_id.as_deref(),
            signature: None,
            read_reply: m.reply_id.as_deref(),
            read_signature: None,
        },
    )
    .unwrap();
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
    assert_eq!(f.lookup(&page)["found"], true);
    assert_eq!(f.apply(&page)["durable"], true);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
    let mark_id = pending_reads::intent_id(&f.location.configuration, &m).unwrap();
    let mut changed = m.clone();
    changed.run_id = "different-run".into();
    assert!(reads::receipt(&f.store.db, &mark_id, &changed).is_err());
}

#[test]
fn first_import_adopts_an_existing_settled_intent_without_new_delivery() {
    let mut f = Fixture::new();
    let m = mark(0);
    reads::enqueue(&mut f.store.db, &id(), &m, 0).unwrap();
    let service: String = f
        .store
        .db
        .query_row("SELECT epoch FROM service WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let writer = id();
    zerus_desktop_ledger::authorize(&mut f.store.db, &writer, &service).unwrap();
    let original = reads::reserve(&mut f.store.db, &writer, &service, &id(), 0)
        .unwrap()
        .unwrap();
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 1)
        .unwrap()
        .remove(0);
    reads::observe(
        &mut f.store.db,
        &pending,
        &reads::Snapshot {
            computer: &m.computer,
            name: &m.name,
            run: Some(&m.run_id),
            conversation: Some(&m.conversation_id),
            reply: m.reply_id.as_deref(),
            signature: None,
            read_reply: m.reply_id.as_deref(),
            read_signature: None,
        },
    )
    .unwrap();
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![m]).unwrap()).unwrap();
    assert_eq!(f.apply(&capture.page(0).unwrap())["durable"], true);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
    assert_eq!(reads::status(&f.store.db).unwrap()["in_flight"], 1);
    let retained: String = f
        .store
        .db
        .query_row(
            "SELECT attempt FROM desktop_reads WHERE seq=?1",
            [original.pending.seq],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, original.id);
}

#[test]
fn conflicting_frozen_mark_is_durably_superseded_without_blocking_the_page() {
    let mut f = Fixture::new();
    let old = mark(0);
    let mut current = old.clone();
    current.reply_id = Some("newer-reply".into());
    reads::enqueue(&mut f.store.db, &id(), &current, 0).unwrap();
    let capture =
        Capture::from_bytes(&serde_json::to_vec(&vec![old.clone(), mark(1)]).unwrap()).unwrap();
    let page = capture.page(0).unwrap();
    assert_eq!(f.apply(&page)["accepted"], true);
    assert_eq!(f.apply(&page)["superseded"], 1);
    assert_eq!(f.lookup(&page)["result"]["superseded"], 1);
    let original = reads::receipt(
        &f.store.db,
        &pending_reads::intent_id(&f.location.configuration, &old).unwrap(),
        &old,
    )
    .unwrap()
    .unwrap();
    assert!(original.superseded);
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 128).unwrap();
    assert_eq!(pending[0].mark, current);
    assert_eq!(
        pending.len(),
        2,
        "the conflicting row does not block later marks"
    );
    // A repeat remains superseded even if the current row subsequently retires.
    reads::observe(
        &mut f.store.db,
        &pending[0],
        &reads::Snapshot {
            computer: &current.computer,
            name: &current.name,
            run: Some(&current.run_id),
            conversation: Some(&current.conversation_id),
            reply: current.reply_id.as_deref(),
            signature: None,
            read_reply: current.reply_id.as_deref(),
            read_signature: None,
        },
    )
    .unwrap();
    assert_eq!(f.apply(&page)["superseded"], 1);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 1);
    assert_eq!(
        reads::receipt(&f.store.db, &original.request_id, &old).unwrap(),
        Some(original)
    );
}

#[test]
fn a_different_frozen_part_after_settlement_is_new_intent() {
    let mut f = Fixture::new();
    let old = mark(0);
    let initial = reads::enqueue(&mut f.store.db, &id(), &old, 0).unwrap();
    let service: String = f
        .store
        .db
        .query_row("SELECT epoch FROM service WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let writer = id();
    zerus_desktop_ledger::authorize(&mut f.store.db, &writer, &service).unwrap();
    let original = reads::reserve(&mut f.store.db, &writer, &service, &id(), 0)
        .unwrap()
        .unwrap();
    reads::observe(
        &mut f.store.db,
        &original.pending,
        &reads::Snapshot {
            computer: &old.computer,
            name: &old.name,
            run: Some(&old.run_id),
            conversation: Some(&old.conversation_id),
            reply: old.reply_id.as_deref(),
            signature: None,
            read_reply: old.reply_id.as_deref(),
            read_signature: None,
        },
    )
    .unwrap();
    let mut newer = old.clone();
    newer.reply_id = Some("reply-after-native-mode".into());
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![newer.clone()]).unwrap()).unwrap();
    let page = capture.page(0).unwrap();
    assert_eq!(f.apply(&page)["superseded"], 0);
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 1)
        .unwrap()
        .remove(0);
    assert_eq!(pending.revision, initial.revision + 1);
    assert_eq!(pending.mark, newer);
    assert_eq!(reads::status(&f.store.db).unwrap()["in_flight"], 1);
    let stable_id = pending_reads::intent_id(&f.location.configuration, &newer).unwrap();
    let receipt = reads::receipt(&f.store.db, &stable_id, &newer)
        .unwrap()
        .unwrap();
    assert!(!receipt.superseded);
    assert_eq!(f.apply(&page)["superseded"], 0);
    assert_eq!(
        reads::receipt(&f.store.db, &stable_id, &newer).unwrap(),
        Some(receipt)
    );
}

#[test]
fn a_conflicting_frozen_mark_preserves_the_original_in_flight_ticket() {
    let mut f = Fixture::new();
    let current = mark(0);
    reads::enqueue(&mut f.store.db, &id(), &current, 0).unwrap();
    let service: String = f
        .store
        .db
        .query_row("SELECT epoch FROM service WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let writer = id();
    zerus_desktop_ledger::authorize(&mut f.store.db, &writer, &service).unwrap();
    let original = reads::reserve(&mut f.store.db, &writer, &service, &id(), 0)
        .unwrap()
        .unwrap();
    let mut frozen = current.clone();
    frozen.reply_id = Some("different-frozen-reply".into());
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![frozen.clone()]).unwrap()).unwrap();
    let page = capture.page(0).unwrap();
    assert_eq!(f.apply(&page)["superseded"], 1);
    assert_eq!(f.apply(&page)["durable"], true);
    assert_eq!(
        reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 1).unwrap(),
        vec![original.pending.clone()]
    );
    assert_eq!(reads::status(&f.store.db).unwrap()["in_flight"], 1);
    let ticket: String = f
        .store
        .db
        .query_row(
            "SELECT attempt FROM desktop_reads WHERE seq=?1",
            [original.pending.seq],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ticket, original.id);
    let request = pending_reads::intent_id(&f.location.configuration, &frozen).unwrap();
    let receipt = reads::receipt(&f.store.db, &request, &frozen)
        .unwrap()
        .unwrap();
    assert!(receipt.superseded);
    reads::finish(&mut f.store.db, &original, reads::Outcome::NotSent, 1).unwrap();
    assert_eq!(f.apply(&page)["superseded"], 1);
    assert_eq!(
        reads::receipt(&f.store.db, &request, &frozen).unwrap(),
        Some(receipt)
    );
}

#[test]
fn historical_import_adds_a_missing_part_without_erasing_current_parts() {
    let mut f = Fixture::new();
    let current = mark(0);
    reads::enqueue(&mut f.store.db, &id(), &current, 0).unwrap();
    let mut imported = current.clone();
    imported.reply_id = None;
    imported.attention_signature = Some("question".into());
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![imported]).unwrap()).unwrap();
    assert_eq!(f.apply(&capture.page(0).unwrap())["durable"], true);
    let pending = reads::page(&f.store.db, 0, reads::watermark(&f.store.db).unwrap(), 1).unwrap();
    assert_eq!(pending[0].mark.reply_id, current.reply_id);
    assert_eq!(
        pending[0].mark.attention_signature.as_deref(),
        Some("question")
    );
}

#[test]
fn invalid_last_capture_or_cursor_materializes_nothing() {
    let f = Fixture::new();
    let mut values: Vec<_> = (0..193).map(|n| json!(mark(n))).collect();
    values[192]["reply_id"] = Value::Null;
    assert!(Capture::from_bytes(&serde_json::to_vec(&values).unwrap()).is_err());
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![mark(0)]).unwrap()).unwrap();
    let mut page = capture.page(0).unwrap();
    page.after = 2;
    assert_eq!(f.lookup(&page)["accepted"], false);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 0);
}

#[test]
fn later_conflicting_receipt_rolls_back_the_entire_original_page() {
    let mut f = Fixture::new();
    let a = mark(0);
    let b = mark(1);
    let key = pending_reads::intent_id(&f.location.configuration, &b).unwrap();
    let mut other = b.clone();
    other.reply_id = Some("different".into());
    reads::enqueue(&mut f.store.db, &key, &other, 0).unwrap();
    let capture = Capture::from_bytes(&serde_json::to_vec(&vec![a, b]).unwrap()).unwrap();
    assert_eq!(f.apply(&capture.page(0).unwrap())["accepted"], false);
    assert_eq!(reads::status(&f.store.db).unwrap()["pending"], 1);
}
