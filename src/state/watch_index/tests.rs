use super::*;
use std::io::Write;

fn record(root: &Path, provider: &str) -> Value {
    let id = uuid::Uuid::new_v4().to_string();
    json!({"agent":provider,"conversation_id":id,"run_id":"example-run","name":"codex/example/fixture",
        "transcript":root.join(format!("{id}.jsonl")),"agent_home":root.join("provider"),"created":1.})
}

fn compare(index: &mut Index, record: &Value, boundary: u64) {
    index.prepare_snapshot(record, Some(boundary)).unwrap();
    let expected = provider_messages::read_snapshot(record, Some(boundary)).unwrap();
    if let Some(actual) = index.views.messages(record) {
        same("public messages", boundary, &actual, &expected);
    }
    same(
        "settings",
        boundary,
        &index.views.settings().unwrap(),
        &effort::watch_reference(record, boundary).unwrap(),
    );
    let partial = index
        .transcript
        .as_ref()
        .is_some_and(|transcript| !transcript.partial.is_empty() || transcript.skipping);
    if let Some(actual) = index.views.usage(record, partial) {
        same(
            "usage",
            boundary,
            &actual,
            &usage::watch_reference(record, boundary).unwrap(),
        );
    }
    if record["agent"] == "codex" {
        same(
            "errors",
            boundary,
            &index.views.errors(record).unwrap(),
            &provider_errors::watch_reference(record, boundary).unwrap(),
        );
        if let Some(actual) = index.views.raw_questions() {
            same(
                "questions",
                boundary,
                &actual,
                &codex_questions::watch_reference(record, boundary).unwrap(),
            );
        }
    }
}

fn same<T: serde::Serialize + PartialEq>(kind: &str, boundary: u64, actual: &T, expected: &T) {
    // Real transcript replay failures name offsets and hashes only. Never
    // print public message bodies or native question text into test evidence.
    assert!(
        actual == expected,
        "{kind} mismatch at {boundary}, incremental={:x}, full={:x}",
        Sha256::digest(serde_json::to_vec(actual).unwrap()),
        Sha256::digest(serde_json::to_vec(expected).unwrap())
    );
}

#[test]
fn chunked_claude_and_codex_match_full_scans_at_partial_line_boundaries() {
    provider_messages::PROJECTION_NOW
        .with(|clock| *clock.borrow_mut() = Some(json!("2026-11-23T00:00:00Z")));
    for provider in ["claude", "codex"] {
        let root = tempfile::tempdir().unwrap();
        let record = record(root.path(), provider);
        let id = &record["conversation_id"];
        let header = if provider == "codex" {
            json!({"type":"session_meta","payload":{"id":id}})
        } else {
            json!({"type":"system","sessionId":id})
        };
        let mut rows = vec![header];
        if provider == "claude" {
            rows.extend([
                json!({"type":"assistant","sessionId":id,"timestamp":"2026-11-01T12:00:00Z","uuid":"first",
                    "requestedModel":"example-model","effort":"high","message":{"id":"request-1","role":"assistant",
                    "content":[{"type":"text","text":"Checking the example."}],"usage":{"input_tokens":4,"output_tokens":2}}}),
                json!({"type":"api-request-blob","sessionId":id,"hash":"blob","message":{"role":"assistant",
                    "content":[{"type":"text","text":"A reply between tools."}]}}),
                json!({"type":"api-request","sessionId":id,"timestamp":"2026-11-01T12:00:01Z"}),
                json!({"type":"system","sessionId":id,"subtype":"compact_boundary","timestamp":"2026-11-01T12:00:02Z","compactMetadata":{"postTokens":2}}),
                json!({"type":"user","sessionId":id,"timestamp":"2026-11-01T12:00:03Z","message":{"role":"user","content":"Continue with the fixture."}}),
            ]);
        } else {
            rows.extend([
                json!({"type":"turn_context","payload":{"model":"example-model","effort":"high"}}),
                json!({"type":"event_msg","timestamp":"2026-11-01T12:00:00Z","payload":{"type":"task_started","turn_id":"turn-1"}}),
                json!({"type":"response_item","timestamp":"2026-11-01T12:00:01Z","payload":{"type":"function_call",
                    "name":"request_user_input_async","call_id":"call-1","arguments":json!({"questions":[{"title":"Which fixture?","options":["One","Two"]}]}).to_string()}}),
                json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"{\"accepted\":true}"}}),
                json!({"type":"event_msg","timestamp":"2026-11-01T12:00:02Z","payload":{"type":"token_count",
                    "info":{"total_token_usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6},"last_token_usage":{"total_tokens":6}}}}),
                json!({"type":"event_msg","timestamp":"2026-11-01T12:00:03Z","payload":{"type":"agent_message","message":"Checking the example."}}),
                json!({"type":"event_msg","timestamp":"2026-11-01T12:00:04Z","payload":{"type":"task_complete","turn_id":"turn-1","error":{"message":"Fixture provider error"}}}),
            ]);
        }
        let bytes: Vec<u8> = rows
            .iter()
            .flat_map(|row| {
                if row["type"]
                    .as_str()
                    .is_some_and(|kind| kind.starts_with("api-request"))
                {
                    let mut fields = row.as_object().unwrap().clone();
                    let kind = fields.remove("type").unwrap();
                    let rest = serde_json::to_string(&fields).unwrap();
                    format!("{{\"type\":{kind},{}\n", &rest[1..]).into_bytes()
                } else {
                    format!("{row}\n").into_bytes()
                }
            })
            .collect();
        std::fs::write(string(&record, "transcript"), &bytes).unwrap();
        let first = format!("{}\n", rows[0]).len() as u64;
        let mut index = Index::default();
        let mut previous = 0;
        for boundary in (first..bytes.len() as u64)
            .step_by(37)
            .chain([bytes.len() as u64])
        {
            compare(&mut index, &record, boundary);
            assert_eq!(
                index.read_bytes,
                boundary - previous,
                "only newly appended bytes"
            );
            previous = boundary;
        }
        assert!(
            messages(&record).is_none(),
            "ordinary CLI reads never use a watch index"
        );
    }
    provider_messages::PROJECTION_NOW.with(|clock| clock.borrow_mut().take());
}

#[test]
fn replacement_shrink_rewrite_and_run_change_reset_progress() {
    let root = tempfile::tempdir().unwrap();
    let mut record = record(root.path(), "claude");
    let line = |text: &str| {
        format!(
            "{}\n",
            json!({"type":"assistant","sessionId":record["conversation_id"],
        "requestedModel":"example-model","uuid":text,"message":{"role":"assistant","content":text}})
        )
    };
    let first = line("first fixture");
    let second = line("a later, longer fixture");
    let path = PathBuf::from(string(&record, "transcript"));
    std::fs::write(&path, &first).unwrap();
    let mut index = Index::default();
    compare(&mut index, &record, first.len() as u64);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(second.as_bytes())
        .unwrap();
    compare(&mut index, &record, (first.len() + second.len()) as u64);
    assert_eq!(index.read_bytes, second.len() as u64);
    std::fs::write(&path, &first).unwrap();
    compare(&mut index, &record, first.len() as u64);
    assert_eq!(index.read_bytes, first.len() as u64);
    let rewritten = first.replace("first fixture", "other fixture");
    std::fs::write(&path, &rewritten).unwrap();
    compare(&mut index, &record, rewritten.len() as u64);
    assert_eq!(index.read_bytes, rewritten.len() as u64);
    let replacement = root.path().join("replacement");
    std::fs::write(&replacement, &second).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    compare(&mut index, &record, second.len() as u64);
    assert_eq!(index.read_bytes, second.len() as u64);
    record["run_id"] = json!("replacement-run");
    compare(&mut index, &record, second.len() as u64);
    assert_eq!(index.read_bytes, second.len() as u64);
    let conversation = uuid::Uuid::new_v4().to_string();
    let changed = second.replace(string(&record, "conversation_id"), &conversation);
    let next_path = root.path().join(format!("{conversation}.jsonl"));
    std::fs::write(&next_path, &changed).unwrap();
    record["conversation_id"] = json!(conversation);
    record["transcript"] = json!(next_path);
    compare(&mut index, &record, changed.len() as u64);
    assert_eq!(index.read_bytes, changed.len() as u64);
}

#[test]
fn comparison_masks_only_observation_timings_and_bounds_logged_fields() {
    assert!(differences(
        &json!({"summary":"same","timings_ms":{"summary":1}}),
        &json!({"summary":"same","timings_ms":{"summary":3}})
    )
    .is_empty());
    assert_eq!(
        differences(
            &json!({"pending_questions":[]}),
            &json!({"pending_questions":[1]})
        ),
        ["pending_questions"]
    );
    let many = Value::Object((0..40).map(|i| (format!("field-{i}"), json!(i))).collect());
    assert_eq!(differences(&json!({}), &many).len(), 20);
}

#[test]
fn the_minute_comparison_runs_outside_the_index_and_the_full_result_wins() {
    fn inspect(name: &str, _: i64, _: bool) -> Result<(Value, Option<watch_source::Timeline>)> {
        if CURRENT.with(|current| current.borrow().is_some()) {
            let id = Path::new(name).file_stem().unwrap().to_string_lossy();
            prepare(
                &json!({"agent":"claude","conversation_id":id,"run_id":"fixture","transcript":name}),
            );
        }
        Ok((
            json!({"incremental":CURRENT.with(|current|current.borrow().is_some())}),
            None,
        ))
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(format!("{}.jsonl", uuid::Uuid::new_v4()));
    std::fs::write(&path, []).unwrap();
    let name = path.to_str().unwrap();
    let mut index = Index::default();
    assert_eq!(
        index.refresh(name, 0, false, inspect).unwrap().0["incremental"],
        true
    );
    index.checked = Some(Instant::now() - COMPARE);
    assert_eq!(
        index.refresh(name, 0, false, inspect).unwrap().0["incremental"],
        false
    );
    assert!(CURRENT.with(|current| current.borrow().is_none()));
}

#[test]
fn a_complete_usage_seed_counts_the_first_new_record_once() {
    let root = tempfile::tempdir().unwrap();
    storage::set_test_root(Some(root.path().join("state")));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            storage::set_test_root(None);
        }
    }
    let _reset = Reset;
    let record = record(root.path(), "claude");
    let line = |id: &str, input: u64| {
        format!(
            "{}\n",
            json!({"type":"assistant","sessionId":record["conversation_id"],
        "timestamp":"2026-11-01T12:00:00Z","message":{"id":id,"role":"assistant","content":"Fixture.",
        "usage":{"input_tokens":input,"output_tokens":2}}})
        )
    };
    let path = PathBuf::from(string(&record, "transcript"));
    std::fs::write(&path, line("first", 4)).unwrap();
    assert_eq!(usage::read(&record)["status"], "ok");
    let mut index = Index::default();
    index.scoped(|| {
        prepare(&record);
        assert!(usage(&record).is_some());
    });
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(line("second", 10).as_bytes())
        .unwrap();
    let actual = index.scoped(|| {
        prepare(&record);
        usage(&record).unwrap()
    });
    let expected = usage::read(&record);
    same(
        "seeded usage",
        std::fs::metadata(path).unwrap().len(),
        &actual,
        &expected,
    );
    assert_eq!(actual["requests"], 2);
}

#[test]
fn memory_bounds_use_the_canonical_fallback_instead_of_an_empty_answer() {
    let root = tempfile::tempdir().unwrap();
    let claude = record(root.path(), "claude");
    let mut file = File::create(string(&claude, "transcript")).unwrap();
    for i in 0..600 {
        writeln!(file,"{}",json!({"type":"assistant","sessionId":claude["conversation_id"],"uuid":format!("fixture-{i}"),
            "timestamp":"2026-11-01T12:00:00Z","message":{"role":"assistant","content":"z".repeat(8192)}})).unwrap();
    }
    drop(file);
    let mut index = Index::default();
    let length = std::fs::metadata(string(&claude, "transcript"))
        .unwrap()
        .len();
    index.prepare_snapshot(&claude, Some(length)).unwrap();
    let (bytes, rows) = index.views.public_size();
    assert!(bytes <= 4 * 1024 * 1024 && rows <= 8192);
    assert!(
        index.views.messages(&claude).is_none(),
        "over budget means canonical scan, not an empty window"
    );

    let codex = record(root.path(), "codex");
    let mut file = File::create(string(&codex, "transcript")).unwrap();
    writeln!(
        file,
        "{}",
        json!({"type":"session_meta","payload":{"id":codex["conversation_id"]}})
    )
    .unwrap();
    for i in 0..129 {
        writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"function_call","name":"request_user_input_async",
            "call_id":format!("fixture-{i}"),"arguments":json!({"questions":[{"title":"A fixture question?","options":["One","Two"]}]}).to_string()}})).unwrap();
    }
    drop(file);
    let length = std::fs::metadata(string(&codex, "transcript"))
        .unwrap()
        .len();
    index.prepare_snapshot(&codex, Some(length)).unwrap();
    assert!(index.views.raw_questions().is_none());
    index.scoped(|| {
        assert!(
            questions(&codex, Some(Instant::now())).is_none(),
            "canonical index remains responsible"
        )
    });
}

#[test]
#[ignore = "explicit read-only qualification with ZERUS_WATCH_REPLAY_RECORDS"]
fn real_transcripts_match_full_prefix_scans() {
    let records: Vec<Value> = serde_json::from_str(
        &std::env::var("ZERUS_WATCH_REPLAY_RECORDS").expect("read-only input metadata"),
    )
    .unwrap();
    provider_messages::PROJECTION_NOW
        .with(|clock| *clock.borrow_mut() = Some(json!("2026-11-23T00:00:00Z")));
    for record in records {
        let path = PathBuf::from(string(&record, "transcript"));
        let size = std::fs::metadata(&path).unwrap().len();
        let mut index = Index::default();
        let first = size.min(2 * 1024 * 1024);
        let step = (size / 16).max(1);
        let mut previous = 0;
        for boundary in std::iter::once(first)
            .chain((first + step..size).step_by(step as usize))
            .chain([size])
        {
            let started = Instant::now();
            index
                .prepare_snapshot(&record, Some(boundary))
                .expect("read-only prefix");
            let update = started.elapsed().as_micros();
            let read = index.read_bytes;
            assert_eq!(read, boundary - previous);
            let checked = Instant::now();
            compare(&mut index, &record, boundary);
            println!(
                "{}",
                json!({"size":size,"offset":boundary,"bytes":read,
                "update_us":update,"full_comparison_us":checked.elapsed().as_micros()})
            );
            previous = boundary;
        }
        let mut file = File::open(&path).unwrap().take(size);
        let mut digest = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        println!(
            "{}",
            json!({"size":size,"sha256":format!("{:x}",digest.finalize())})
        );
    }
    provider_messages::PROJECTION_NOW.with(|clock| clock.borrow_mut().take());
}
