use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use zerus_client_core::{ActivityUpdate, Change, Coalescer, ComputerId};

#[test]
fn folded_activity_keeps_its_envelope_but_jpegs_are_read_as_bytes() {
    let host = ComputerId::from("example-peer");
    let bytes = [255, 216, 1, 2, 255, 217];
    let event = |bytes: &[u8]| json!({"source":"hgs_voice","message_id":"message-1","voice":{"id":"message-1","video":{"poster_base64":STANDARD.encode(bytes),"width":200}},"unknown":7});
    let mut batch = Coalescer::new();
    let push = |batch: &mut Coalescer, update| {
        batch.push(Change::Activity {
            computer: host.clone(),
            key: "run:one".into(),
            update,
        })
    };
    push(
        &mut batch,
        ActivityUpdate::Event {
            id: "event-1".into(),
            rev: 2,
            event: event(&bytes),
        },
    );
    push(
        &mut batch,
        ActivityUpdate::Event {
            id: "event-1".into(),
            rev: 1,
            event: event(&[255, 216, 9, 255, 217]),
        },
    );
    assert!(batch.poster(&host, "run:one", "message-1").is_none());
    let view = batch.take();
    assert_eq!(view.activity[0].kind, "event");
    assert_eq!(view.activity[0].payload["rev"], 2);
    assert_eq!(view.activity[0].payload["event"]["unknown"], 7);
    assert_eq!(
        view.activity[0].payload["event"]["voice"]["video"]["poster_available"],
        true
    );
    let wire = serde_json::to_string(&view).unwrap();
    assert!(!wire.contains("poster_base64"));
    assert!(!wire.contains(&STANDARD.encode(bytes)));
    assert_eq!(
        &*batch.poster(&host, "run:one", "message-1").unwrap(),
        bytes
    );
    assert!(batch.take().is_empty());
    assert!(batch.poster(&host, "run:one", "message-1").is_some());
    push(
        &mut batch,
        ActivityUpdate::State {
            value: json!({"provider_messages":[event(&bytes)],"other":"preserve"}),
            rev: 3,
        },
    );
    assert!(!serde_json::to_string(&batch.take())
        .unwrap()
        .contains("poster_base64"));
    push(
        &mut batch,
        ActivityUpdate::Replace {
            events: vec![event(&bytes)],
            cursor: 9,
        },
    );
    let view = batch.take();
    assert_eq!(view.activity[0].payload["cursor"], 9);
    assert!(!serde_json::to_string(&view)
        .unwrap()
        .contains("poster_base64"));
    push(&mut batch, ActivityUpdate::Ended);
    batch.take();
    assert!(batch.poster(&host, "run:one", "message-1").is_none());
}
