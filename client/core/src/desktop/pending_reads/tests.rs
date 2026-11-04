use super::*;
use serde_json::json;

fn mark(n: usize) -> ReadMark {
    ReadMark {
        computer: "example".into(),
        name: format!("agent/project/{n}"),
        run_id: format!("run-{n}"),
        conversation_id: format!("conversation-{n}"),
        reply_id: Some(format!("reply-{n}")),
        attention_signature: None,
    }
}

#[test]
fn captures_every_entry_in_bounded_resumable_pages() {
    let marks: Vec<_> = (0..193).map(mark).collect();
    let bytes = serde_json::to_vec(&marks).unwrap();
    let capture = Capture::from_bytes(&bytes).unwrap();
    let mut after = 0;
    let mut received = Vec::new();
    while after < capture.source().rows {
        let page = capture.page(after).unwrap();
        assert!(page.valid());
        assert!(page.marks.len() <= PAGE_ROWS);
        assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_BYTES);
        assert_eq!(page, capture.page(after).unwrap());
        after = page.through();
        received.extend(page.marks);
    }
    assert_eq!(received, marks);
    assert!(capture.page(194).is_err());
}

#[test]
fn a_bad_final_mark_refuses_the_whole_capture() {
    let mut values: Vec<_> = (0..193).map(|n| json!(mark(n))).collect();
    values[192]["conversation_id"] = json!("");
    assert!(Capture::from_bytes(&serde_json::to_vec(&values).unwrap()).is_err());
    values[192] = json!(mark(192));
    values[192]["computer"] = json!("@untrusted");
    assert!(Capture::from_bytes(&serde_json::to_vec(&values).unwrap()).is_err());
}

#[test]
fn source_cursor_and_payload_cannot_be_rebound_to_another_capture() {
    let capture =
        Capture::from_bytes(&serde_json::to_vec(&(0..130).map(mark).collect::<Vec<_>>()).unwrap())
            .unwrap();
    let page = capture.page(0).unwrap();
    assert!(capture.matches(&page));
    let mut wrong = page.clone();
    wrong.source.sha256 = "b".repeat(64);
    assert!(!capture.matches(&wrong));
    let mut wrong = page.clone();
    wrong.after = 1;
    assert!(!capture.matches(&wrong));
    let mut wrong = page.clone();
    wrong.marks[0].reply_id = Some("changed".into());
    assert!(!capture.matches(&wrong));
    let mut wrong = page;
    wrong.source.rows += 1;
    assert!(!capture.matches(&wrong));
}

#[test]
fn full_source_caps_and_empty_queue_are_explicit() {
    assert!(Capture::from_bytes(&vec![b' '; SOURCE_BYTES + 1]).is_err());
    let values: Vec<_> = (0..=SOURCE_ROWS).map(mark).collect();
    assert!(Capture::from_bytes(&serde_json::to_vec(&values).unwrap()).is_err());
    let empty = Capture::from_bytes(b"[]").unwrap();
    assert!(empty.page(0).unwrap().valid());
    assert_eq!(empty.source().rows, 0);
}

#[test]
fn byte_bound_can_split_before_the_row_bound_without_truncation() {
    let mut marks: Vec<_> = (0..8).map(mark).collect();
    for m in &mut marks {
        m.reply_id = Some("r".repeat(8192));
        m.attention_signature = Some("a".repeat(8192));
    }
    let capture = Capture::from_bytes(&serde_json::to_vec(&marks).unwrap()).unwrap();
    let first = capture.page(0).unwrap();
    assert!(!first.marks.is_empty() && first.marks.len() < marks.len());
    assert_eq!(first.marks, marks[..first.through()]);
    assert_eq!(
        capture.page(first.through()).unwrap().marks[0],
        marks[first.through()]
    );
}

#[test]
fn immutable_mark_ids_ignore_source_formatting_and_preserve_target_identity() {
    let config = "a".repeat(64);
    let m = mark(1);
    let id = intent_id(&config, &m).unwrap();
    assert!(zerus_desktop_migration::canonical_uuid(&id));
    let plain = Capture::from_bytes(&serde_json::to_vec(&vec![m.clone()]).unwrap()).unwrap();
    let mut extra = json!([m]);
    extra[0]["future_field"] = json!("kept in original source only");
    let formatted = Capture::from_bytes(&serde_json::to_vec_pretty(&extra).unwrap()).unwrap();
    assert_ne!(plain.source().sha256, formatted.source().sha256);
    assert_eq!(
        intent_id(&config, &formatted.page(0).unwrap().marks[0]).unwrap(),
        id
    );
    for field in ["name", "run_id", "conversation_id", "reply_id"] {
        let mut changed = json!(m);
        changed[field] = json!("different");
        assert_ne!(
            intent_id(&config, &serde_json::from_value(changed).unwrap()).unwrap(),
            id
        );
    }
    assert_ne!(intent_id(&"b".repeat(64), &m).unwrap(), id);
}
