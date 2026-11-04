use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, Store) {
    source_fixture(true)
}
fn source_fixture(archive: bool) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, Store) {
    let (temp, state, path, transcript, mut store) = cold_source_fixture(archive);
    prepare_discovery(&store, &state, DiscoveryBudget::interactive);
    prepare_consumption(&mut store, &state, budget);
    // Check the first completed preparation, even if its contents are wrong.
    let coverage = store.index.status().unwrap();
    assert_eq!(coverage.messages, 6);
    assert_eq!(coverage.pending_sources, 0);
    (temp, state, path, transcript, store)
}
fn cold_source_fixture(archive: bool) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, Store) {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    fs::create_dir_all(state.join("archives")).unwrap();
    let conversation = uuid::Uuid::new_v4().to_string();
    let transcript = temp.path().join(format!("{conversation}.jsonl"));
    let rows = (1..=6)
        .map(|i| {
            json!({"type":"event_msg","timestamp":i,
        "payload":{"type":"agent_message","id":format!("m{i}"),"message":format!("needle {i}")}})
            .to_string()
                + "\n"
        })
        .collect::<String>();
    fs::write(&transcript, rows).unwrap();
    let archive = if archive {
        uuid::Uuid::new_v4().to_string()
    } else {
        String::new()
    };
    let path = if archive.is_empty() {
        state.join(format!(
            "{:x}.json",
            Sha256::digest(b"codex/example/archive")
        ))
    } else {
        state.join("archives").join(format!("{archive}.json"))
    };
    fs::write(
        &path,
        json!({"version":1,"name":"codex/example/archive","agent":"codex",
        "run_id":"fixture-run","conversation_id":conversation,"archive_id":archive,
        "archived_at":42,"transcript":transcript,"cwd":state})
        .to_string(),
    )
    .unwrap();
    let store = Store::open(&state.join("content-search-v1")).unwrap();
    (temp, state, path, transcript, store)
}
fn prepare_discovery(
    store: &Store,
    state: &Path,
    mut next_budget: impl FnMut() -> DiscoveryBudget,
) {
    // Keep discovery separate from ingestion: a consume pass cannot make an
    // unfinished directory walk complete. Retry only a bounded yielded phase,
    // without polling for expected message counts or query hits.
    for _ in 0..32 {
        let budget = next_budget();
        let progress = store
            .discover(state, &budget, &AtomicBool::new(false))
            .unwrap();
        assert!(progress.entries <= budget.entries);
        assert!(progress.path_bytes <= budget.path_bytes);
        assert!(!progress.changed, "private fixture catalog changed");
        if progress.complete {
            return;
        }
        assert!(
            Instant::now() >= budget.deadline,
            "fixture discovery did not finish or yield at its deadline"
        );
    }
    panic!("fixture discovery did not finish within 32 bounded slices");
}
fn prepare_consumption(store: &mut Store, state: &Path, mut next_budget: impl FnMut() -> Budget) {
    for _ in 0..32 {
        let budget = next_budget();
        let progress = store
            .consume(state, &budget, &AtomicBool::new(false))
            .unwrap();
        let read = progress.ingestion;
        assert!(read.errors.is_empty(), "{:?}", read.errors);
        assert!(read.files <= budget.files && read.records <= budget.records);
        assert!(read.bytes <= budget.bytes);
        if !read.stopped {
            return;
        }
    }
    panic!("fixture consumption did not finish within 32 bounded slices");
}
fn budget() -> Budget {
    Budget {
        files: 128,
        directory_entries: 2000,
        records: 512,
        bytes: 8 << 20,
        deadline: Instant::now() + Duration::from_secs(2),
    }
}
fn page(store: &mut Store, state: &Path, cursor: Option<&str>, budget: &Budget) -> Value {
    let mut first = true;
    page_with_slices(store, state, cursor, || {
        let deadline = if first {
            budget.deadline
        } else {
            Instant::now() + Duration::from_secs(1)
        };
        first = false;
        Budget {
            files: budget.files,
            directory_entries: budget.directory_entries,
            records: budget.records,
            bytes: budget.bytes,
            deadline,
        }
    })
}
fn page_with_slices(
    store: &mut Store,
    state: &Path,
    cursor: Option<&str>,
    mut next_budget: impl FnMut() -> Budget,
) -> Value {
    let mut next = cursor.map(str::to_owned);
    for _ in 0..32 {
        let slice = next_budget();
        let reply = store
            .publish(
                state,
                "needle",
                2,
                next.as_deref(),
                &slice,
                &AtomicBool::new(false),
            )
            .unwrap();
        // Publication can yield before even reading the first candidate.
        // Resume that exact cursor only; a completed wrong/empty reply or a
        // proof/output budget stop must reach the original assertions.
        if Instant::now() < slice.deadline
            || reply["candidates"] != 0
            || reply["proof"]["files"] != 0
            || !reply["results"].as_array().unwrap().is_empty()
            || reply["next_cursor"].is_null()
        {
            return reply;
        }
        next = Some(reply["next_cursor"].as_str().unwrap().to_owned());
    }
    panic!("fixture publication did not finish within 32 bounded slices");
}

#[test]
fn query_preparation_resumes_expired_phases_without_extending_a_slice() {
    let (_temp, state, _path, transcript, mut store) = cold_source_fixture(true);
    let mut discovery_slices = 0;
    prepare_discovery(&store, &state, || {
        discovery_slices += 1;
        let mut budget = DiscoveryBudget::interactive();
        if discovery_slices <= 2 {
            budget.deadline = Instant::now();
        }
        budget
    });
    assert!(discovery_slices >= 3);
    let mut consume_slices = 0;
    prepare_consumption(&mut store, &state, || {
        consume_slices += 1;
        let mut budget = budget();
        if consume_slices <= 2 {
            budget.deadline = Instant::now();
        }
        budget
    });
    assert!(consume_slices >= 3);
    let coverage = store.index.status().unwrap();
    assert_eq!(coverage.messages, 6);
    assert_eq!(coverage.pending_sources, 0);
    let encoded: String = store
        .index
        .connection()
        .query_row("SELECT cursor FROM input_cursors", [], |row| row.get(0))
        .unwrap();
    let cursor: Cursor = serde_json::from_str(&encoded).unwrap();
    assert_eq!(cursor.offset, fs::metadata(transcript).unwrap().len());
    assert_eq!(
        page(&mut store, &state, None, &budget())["results"][0]["message_id"],
        "m6"
    );
}

#[test]
fn query_preparation_returns_the_first_completed_empty_consumption() {
    let (_temp, state, _path, _transcript, mut store) = cold_source_fixture(true);
    // No discovery has run yet. This is a completed empty queue, not a yield.
    // Return it immediately; retrying until six expected messages would hide it.
    let mut slices = 0;
    prepare_consumption(&mut store, &state, || {
        slices += 1;
        budget()
    });
    assert_eq!(slices, 1);
    assert_eq!(store.index.status().unwrap().messages, 0);
    assert!(store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn query_page_resumes_a_zero_candidate_yield_without_skipping_the_first_hit() {
    let (_temp, state, _path, _transcript, mut store) = fixture();
    let key: String = store
        .index
        .connection()
        .query_row("SELECT id FROM sources", [], |row| row.get(0))
        .unwrap();
    let mut writer = Index::new(
        Connection::open(store.directory.join("index.sqlite3")).unwrap(),
        DEFAULT_FILE_BYTES,
    )
    .unwrap();
    let mut slices = 0;
    let first = page_with_slices(&mut store, &state, None, || {
        slices += 1;
        let mut budget = budget();
        if slices == 1 {
            budget.deadline = Instant::now();
        } else if slices == 2 {
            // A new insertion must not replace the original cursor snapshot.
            writer
                .ingest(
                    &key,
                    &[Message {
                        id: "newer".into(),
                        at: 7.0,
                        role: Role::Assistant,
                        kind: "AgentMessage".into(),
                        source: "codex_transcript".into(),
                        seq: 0,
                        text: "needle newer".into(),
                    }],
                )
                .unwrap();
        }
        budget
    });
    assert!(slices >= 2);
    assert_eq!(first["results"][0]["message_id"], "m6");
    let next = page(&mut store, &state, first["next_cursor"].as_str(), &budget());
    assert_eq!(next["results"][0]["message_id"], "m4");
}

#[test]
#[should_panic(expected = "fixture publication did not finish within 32 bounded slices")]
fn query_page_does_not_retry_publication_without_a_bound() {
    let (_temp, state, _path, _transcript, mut store) = cold_source_fixture(true);
    page_with_slices(&mut store, &state, None, || {
        let mut budget = budget();
        budget.deadline = Instant::now();
        budget
    });
}

#[test]
#[should_panic(expected = "fixture discovery did not finish within 32 bounded slices")]
fn query_preparation_does_not_retry_discovery_without_a_bound() {
    let (_temp, state, _path, _transcript, store) = cold_source_fixture(true);
    prepare_discovery(&store, &state, || {
        let mut budget = DiscoveryBudget::interactive();
        budget.deadline = Instant::now();
        budget
    });
}

#[test]
#[should_panic(expected = "fixture consumption did not finish within 32 bounded slices")]
fn query_preparation_does_not_retry_consumption_without_a_bound() {
    let (_temp, state, _path, _transcript, mut store) = cold_source_fixture(true);
    prepare_consumption(&mut store, &state, || {
        let mut budget = budget();
        budget.deadline = Instant::now();
        budget
    });
}

#[test]
fn archive_pages_keep_exact_navigation_and_cursor_snapshot_without_fallback() {
    let (_temp, state, _path, _transcript, mut store) = fixture();
    let first = page(&mut store, &state, None, &budget());
    assert_eq!(first["abi"], QUERY_ABI);
    assert_eq!(first["index_coverage"]["state"], "ready");
    assert!(first["fallback"].is_null());
    assert_eq!(first["results"][0]["message_id"], "m6");
    assert_eq!(first["results"][0]["session"]["state"], "archived");
    assert_eq!(
        first["results"][0]["session"]["archive_id"],
        first["results"][0]["archive_id"]
    );
    assert_eq!(first["proof"]["files"], 2);
    let second = page(&mut store, &state, first["next_cursor"].as_str(), &budget());
    assert_eq!(second["results"][0]["message_id"], "m4");
    let third = page(
        &mut store,
        &state,
        second["next_cursor"].as_str(),
        &budget(),
    );
    assert_eq!(third["results"][0]["message_id"], "m2");
    assert!(third["next_cursor"].is_null());
}

#[test]
fn publication_uses_fresh_recorded_account_binding_not_an_indexed_copy() {
    let (_temp, state, path, _transcript, mut store) = fixture();
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["account_id"] = json!("changed-profile");
    record["agent_home"] = json!("/srv/example/recorded-home");
    fs::write(&path, record.to_string()).unwrap();
    let first = page(&mut store, &state, None, &budget());
    assert_eq!(
        first["results"][0]["session"]["account_id"],
        "changed-profile"
    );
    assert_eq!(
        first["results"][0]["session"]["account_home"],
        "/srv/example/recorded-home"
    );
}

#[test]
fn an_unverified_hit_is_never_consumed_when_the_proof_budget_runs_out() {
    let (_temp, state, _path, _transcript, mut store) = fixture();
    let mut tiny = budget();
    tiny.files = 1;
    let first = page(&mut store, &state, None, &tiny);
    assert!(first["results"].as_array().unwrap().is_empty());
    assert_eq!(first["proof"]["files"], 0);
    assert!(first["next_cursor"].is_string());
    let resumed = page(&mut store, &state, first["next_cursor"].as_str(), &budget());
    assert_eq!(resumed["results"][0]["message_id"], "m6");
}

#[test]
fn a_forgotten_archive_is_rejected_before_publication_without_discovery_or_sweep() {
    let (_temp, state, path, _transcript, mut store) = fixture();
    fs::remove_file(path).unwrap();
    let reply = page(&mut store, &state, None, &budget());
    assert!(reply["results"].as_array().unwrap().is_empty());
    assert_eq!(reply["unavailable_sessions"], 1);
    assert_ne!(reply["index_coverage"]["state"], "ready");
    assert!(store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn changed_transcript_identity_cannot_publish_old_copied_text() {
    let (_temp, state, _path, transcript, mut store) = fixture();
    fs::remove_file(&transcript).unwrap();
    fs::write(&transcript, "replacement").unwrap();
    let reply = page(&mut store, &state, None, &budget());
    assert!(reply["results"].as_array().unwrap().is_empty());
    assert_eq!(reply["unavailable_sessions"], 1);
}

#[test]
fn cancelled_publication_retains_a_resumable_position_and_reads_no_source_files() {
    let (_temp, state, _path, _transcript, mut store) = fixture();
    let reply = store
        .publish(&state, "needle", 2, None, &budget(), &AtomicBool::new(true))
        .unwrap();
    assert!(reply["results"].as_array().unwrap().is_empty());
    assert_eq!(reply["proof"]["files"], 0);
    let next = page(&mut store, &state, reply["next_cursor"].as_str(), &budget());
    assert_eq!(next["results"][0]["message_id"], "m6");
    assert!(store
        .publish(
            &state,
            "other",
            2,
            reply["next_cursor"].as_str(),
            &budget(),
            &AtomicBool::new(false)
        )
        .is_err());
}

#[test]
fn the_output_budget_preserves_the_first_unreturned_hit() {
    let (_temp, state, _path, _transcript, mut store) = fixture();
    let key: String = store
        .index
        .connection()
        .query_row("SELECT id FROM sources LIMIT 1", [], |r| r.get(0))
        .unwrap();
    for i in 0..80 {
        store
            .index
            .ingest(
                &key,
                &[Message {
                    id: format!("budget-{i}"),
                    at: 10.0 + i as f64,
                    role: Role::Assistant,
                    kind: "AgentMessage".into(),
                    source: "codex_transcript".into(),
                    seq: 0,
                    text: format!("needle {i} {}", "x".repeat(40_000)),
                }],
            )
            .unwrap();
    }
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..10 {
        let reply = store
            .publish(
                &state,
                "needle",
                100,
                cursor.as_deref(),
                &budget(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let hits = reply["results"].as_array().unwrap();
        assert!(serde_json::to_vec(hits).unwrap().len() <= INDEX_REPLY_BYTES);
        for hit in hits {
            assert!(seen.insert(hit["message_id"].as_str().unwrap().to_owned()));
        }
        cursor = reply["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none());
    assert_eq!(seen.len(), 86);
}

#[test]
fn current_live_histories_publish_exact_hits_while_retaining_fresh_fallback_coverage() {
    let (_temp, state, _path, _transcript, mut store) = source_fixture(false);
    let reply = page(&mut store, &state, None, &budget());
    assert_eq!(reply["results"][0]["message_id"], "m6");
    assert_eq!(reply["results"][0]["run_id"], "fixture-run");
    assert!(reply["results"][0]["session"].is_null());
    assert_ne!(reply["index_coverage"]["state"], "ready");
    assert_eq!(reply["catalog"]["fallback_sources"], 1);
}

#[test]
fn an_appended_live_prefix_is_pending_then_resumes_without_permanent_retirement() {
    use std::io::Write;
    let (_temp, state, _path, transcript, mut store) = source_fixture(false);
    let first = page(&mut store, &state, None, &budget());
    fs::OpenOptions::new().append(true).open(&transcript).unwrap().write_all(
        (json!({"type":"event_msg","timestamp":7,"payload":{"type":"agent_message","id":"m7","message":"needle appended"}}).to_string()+"\n").as_bytes()).unwrap();
    let pending = page(&mut store, &state, None, &budget());
    assert!(pending["results"].as_array().unwrap().is_empty());
    assert_eq!(pending["unavailable_sessions"], 1);
    assert_eq!(pending["catalog"]["pending_paths"], 1);
    assert_eq!(store.index.status().unwrap().sources, 1);
    let mut consume_slices = 0;
    prepare_consumption(&mut store, &state, || {
        consume_slices += 1;
        let mut budget = budget();
        if consume_slices == 1 {
            // A legal zero-record yield used to reach the m7 assertion below.
            budget.deadline = Instant::now();
        }
        budget
    });
    assert!(consume_slices >= 2);
    let current = page(&mut store, &state, None, &budget());
    assert_eq!(current["results"][0]["message_id"], "m7");
    assert_eq!(current["unavailable_sessions"], 0);
    // Existing cursors preserve their insertion high-water mark.
    let continued = page(&mut store, &state, first["next_cursor"].as_str(), &budget());
    assert_eq!(continued["results"][0]["message_id"], "m4");
}

#[test]
fn forgetting_a_live_binding_cannot_leave_an_indexed_hit_visible() {
    let (_temp, state, path, _transcript, mut store) = source_fixture(false);
    fs::remove_file(path).unwrap();
    let reply = page(&mut store, &state, None, &budget());
    assert!(reply["results"].as_array().unwrap().is_empty());
    assert_eq!(reply["unavailable_sessions"], 1);
}

#[test]
fn unsupported_adapters_are_delegated_without_a_permanent_retirement_fence() {
    let (_temp, state, _path, _transcript, mut store) = source_fixture(false);
    let mut target = store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .remove(0)
        .target;
    target.agent = "dsh".into();
    let key = store.index.register(&target).unwrap();
    store.index.source_progress(&key, false, true).unwrap();
    let reply = page(&mut store, &state, None, &budget());
    assert!(reply["results"].as_array().unwrap().is_empty());
    assert_eq!(reply["proof"]["files"], 0);
    assert_eq!(store.index.status().unwrap().sources, 1);
    assert!(store.index.register(&target).is_ok());
}
