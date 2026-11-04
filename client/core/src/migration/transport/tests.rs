use super::*;
use crate::migration::SourceStream;
use zerus_desktop_migration::SourceRecord;

fn bundle(source: SourceStream) -> Bundle {
    Bundle::new(
        SOURCE,
        false,
        "a".repeat(64),
        "11111111-1111-4111-8111-111111111111".into(),
        1,
        vec![source],
        1 << 20,
    )
    .unwrap()
}
fn source() -> SourceStream {
    SourceStream::new(
        SourceRecord {
            family: "synthetic".into(),
            schema: 1,
            format: "json".into(),
            sha256: sha256(b"original"),
            build: None,
        },
        std::iter::once(Ok(json!({"original":"bounded private source"}))),
    )
}

#[test]
fn original_source_recheck_precedes_the_durable_expected_receipt() {
    use std::sync::atomic::Ordering;
    let rechecked = Arc::new(AtomicBool::new(false));
    let observed = rechecked.clone();
    let source = source().recheck(move || {
        observed.store(true, Ordering::Release);
        Ok(sha256(b"original"))
    });
    let mut output = Vec::new();
    let mut saved = None;
    let receipt = encode(bundle(source), &mut output, |expected| {
        assert!(rechecked.load(Ordering::Acquire));
        saved = Some(expected.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(saved, Some(receipt));
}

#[test]
fn failed_expected_receipt_persistence_writes_no_commit_record() {
    let mut output = Vec::new();
    let error = encode(bundle(source()), &mut output, |_| {
        Err(SourceError::SourceUnreadable)
    })
    .unwrap_err();
    assert_eq!(error, PublishError::Source(SourceError::SourceUnreadable));
    for line in output.split(|byte| *byte == b'\n') {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
            assert_ne!(value["t"], "record");
        }
    }
}

#[test]
fn a_changed_source_cannot_persist_an_expected_commit_or_publish_a_record() {
    let source = source().recheck(|| Err(SourceError::SourceChanged));
    let mut output = Vec::new();
    let error = encode(bundle(source), &mut output, |_| {
        panic!("invalid source reached the commit reservation")
    })
    .unwrap_err();
    assert_eq!(error, PublishError::Source(SourceError::SourceChanged));
}

fn expected() -> StagedReceipt {
    encode(bundle(source()), &mut Vec::new(), |_| Ok(())).unwrap()
}
fn negative(expected: &StagedReceipt) -> serde_json::Value {
    json!({"abi":expected.abi,"published":false,
        "configuration":expected.configuration,"import_id":expected.import_id,
        "import_record":expected.import_record,"code":"handoff_busy",
        "this_attempt_not_committed":true})
}
fn exit(value: &serde_json::Value, code: i32) -> Result<Exit, crate::run::TooLarge> {
    Ok(Exit::Code {
        code,
        stdout: serde_json::to_vec(value).unwrap(),
        stderr: String::new(),
    })
}

#[test]
fn negative_requires_the_exact_expected_record_and_original_complete_exchange() {
    let expected = expected();
    let reply = negative(&expected);
    assert_eq!(
        publication_reply(exit(&reply, 0), &expected, true),
        Err(PublishError::HandoffBusy)
    );
    assert_eq!(
        publication_reply(exit(&reply, 0), &expected, false),
        Err(PublishError::Unconfirmed)
    );
    assert_eq!(
        publication_reply(exit(&reply, 1), &expected, true),
        Err(PublishError::Unconfirmed)
    );
    for (path, value) in [
        ("abi", json!("future")),
        ("configuration", json!("b".repeat(64))),
        ("import_id", json!("22222222-2222-4222-8222-222222222222")),
        ("code", json!("some_other_error")),
        ("published", json!(true)),
        ("this_attempt_not_committed", json!(false)),
        (
            "import_record",
            json!({"version":1,"sha256":"c".repeat(64)}),
        ),
    ] {
        let mut changed = reply.clone();
        changed[path] = value;
        assert_eq!(
            publication_reply(exit(&changed, 0), &expected, true),
            Err(PublishError::Unconfirmed)
        );
    }
}

#[test]
fn lost_or_truncated_negative_never_becomes_permission_to_replay() {
    let expected = expected();
    for stdout in [Vec::new(), b"{\"published\":false".to_vec()] {
        let exit = Ok(Exit::Code {
            code: 0,
            stdout,
            stderr: String::new(),
        });
        assert_eq!(
            publication_reply(exit, &expected, true),
            Err(PublishError::Unconfirmed)
        );
    }
    assert_eq!(
        publication_reply(Ok(Exit::TimedOut), &expected, true),
        Err(PublishError::Unconfirmed)
    );
    assert_eq!(
        publication_reply(
            exit(&serde_json::to_value(&expected).unwrap(), 0),
            &expected,
            false
        ),
        Ok(expected)
    );
}

#[test]
fn writer_failure_stops_decoding_before_the_next_source_item() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source = SourceStream::new(
        SourceRecord {
            family: "synthetic".into(),
            schema: 1,
            format: "json".into(),
            sha256: sha256(b"synthetic"),
            build: None,
        },
        std::iter::from_fn(|| -> Option<Result<serde_json::Value, SourceError>> {
            panic!("no source read after broken start header")
        }),
    );
    let bundle = Bundle::new(
        SOURCE,
        false,
        "a".repeat(64),
        "11111111-1111-4111-8111-111111111111".into(),
        1,
        vec![source],
        1 << 20,
    )
    .unwrap();
    assert_eq!(
        encode(bundle, &mut Broken, |_| Ok(())).unwrap_err(),
        PublishError::Unconfirmed
    );
}
