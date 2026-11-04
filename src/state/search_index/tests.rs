use super::*;
use std::cell::Cell;

struct Fixture {
    temp: tempfile::TempDir,
    state: PathBuf,
    identity: Identity,
    record: Value,
    binding: PathBuf,
    path: PathBuf,
    expired_slices: Cell<usize>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        let identity = Identity {
            name: "codex/example/search".into(),
            run_id: uuid::Uuid::new_v4().to_string(),
            conversation_id: uuid::Uuid::new_v4().to_string(),
            archive_id: String::new(),
        };
        let path = temp
            .path()
            .join(format!("{}.jsonl", identity.conversation_id));
        let record = json!({"version":1,"name":identity.name,"run_id":identity.run_id,"conversation_id":identity.conversation_id,"agent":"codex","transcript":path,"cwd":state});
        let binding = state.join(format!(
            "{:x}.json",
            Sha256::digest(identity.name.as_bytes())
        ));
        fs::write(&binding, record.to_string()).unwrap();
        fs::write(&path, "").unwrap();
        Self {
            temp,
            state,
            identity,
            record,
            binding,
            path,
            expired_slices: Cell::new(0),
        }
    }
    fn store(&self) -> Store {
        Store::open(&self.state.join("content-search-v1")).unwrap()
    }
    fn budget(&self, records: usize) -> Budget {
        let expired = self.expired_slices.get();
        self.expired_slices.set(expired.saturating_sub(1));
        Budget {
            files: 32,
            directory_entries: 2000,
            records,
            bytes: 4 << 20,
            deadline: Instant::now()
                + if expired == 0 {
                    Duration::from_secs(2)
                } else {
                    Duration::ZERO
                },
        }
    }
    fn event(id: usize, text: &str, phase: &str) -> String {
        json!({"type":"event_msg","timestamp":id as f64,"payload":{"type":"agent_message","message":text,"id":format!("m{id}"),"phase":phase}}).to_string()+"\n"
    }
    fn append(&self, text: &str) {
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    fn step(&self, store: &mut Store, records: usize) -> Progress {
        store
            .step(
                &self.state,
                std::slice::from_ref(&self.identity),
                &self.budget(records),
                &AtomicBool::new(false),
            )
            .unwrap()
    }
    fn consume_records(&self, store: &mut Store, per_slice: usize, expected: usize) -> Progress {
        let mut consumed = 0;
        for _ in 0..32 {
            let budget = self.budget(per_slice);
            let progress = store
                .step(
                    &self.state,
                    std::slice::from_ref(&self.identity),
                    &budget,
                    &AtomicBool::new(false),
                )
                .unwrap();
            assert!(progress.errors.is_empty(), "{:?}", progress.errors);
            assert!(progress.records <= per_slice);
            assert!(progress.files <= budget.files && progress.bytes <= budget.bytes);
            consumed += progress.records;
            if consumed >= expected {
                // Return the first consumed prefix, even when its indexed
                // contents are wrong. Never wait for the expected query hits.
                assert_eq!(
                    consumed, expected,
                    "fixture crossed the exact record checkpoint"
                );
                return progress;
            }
            assert!(
                progress.stopped || Instant::now() >= budget.deadline,
                "completed fixture slice consumed {consumed} records, expected {expected}"
            );
        }
        panic!("fixture record checkpoint did not finish within 32 bounded slices");
    }
    fn cursor(&self, store: &Store) -> Cursor {
        let raw: String = store
            .index
            .connection()
            .query_row(
                "SELECT cursor FROM input_cursors WHERE source=?",
                [self.identity.target().key().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }
    fn prepare_until(
        &self,
        store: &mut Store,
        targets: &[Identity],
        ready: impl Fn(&Store, &Progress) -> bool,
    ) -> Progress {
        for _ in 0..32 {
            let progress = store
                .step(
                    &self.state,
                    targets,
                    &self.budget(128),
                    &AtomicBool::new(false),
                )
                .unwrap();
            if ready(store, &progress) {
                return progress;
            }
        }
        panic!("fixture preparation did not finish within 32 bounded passes");
    }
}

#[test]
fn record_checkpoint_keeps_the_first_consumed_prefix_after_expired_slices() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let first = Fixture::event(1, "different public text", "final");
    fixture.append(&first);
    fixture.append(&Fixture::event(2, "needle later", "final"));

    // The old one-step assertion fails on this legal zero-record yield.
    fixture.expired_slices.set(1);
    let yielded = fixture.step(&mut store, 1);
    assert_eq!(yielded.records, 0);
    assert!(yielded.stopped);
    let cursors: usize = store
        .index
        .connection()
        .query_row("SELECT COUNT(*) FROM input_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cursors, 0);

    fixture.expired_slices.set(2);
    fixture.consume_records(&mut store, 1, 1);
    assert_eq!(fixture.cursor(&store).offset, first.len() as u64);
    // A completed wrong query result must not be retried into the next record.
    assert!(store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .is_empty());
}

#[test]
#[should_panic(expected = "fixture record checkpoint did not finish within 32 bounded slices")]
fn record_checkpoint_does_not_retry_expired_slices_without_a_bound() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.append(&Fixture::event(1, "needle later", "final"));
    fixture.expired_slices.set(32);
    fixture.consume_records(&mut store, 1, 1);
}

#[test]
fn source_resumes_committed_offsets_and_indexes_only_public_text() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.append(&Fixture::event(1, "needle first", "final"));
    fixture.append(&Fixture::event(2, "PRIVATE_REASONING_TOKEN", "analysis"));
    fixture.append(&Fixture::event(3, "needle second", "final"));
    fixture.expired_slices.set(2);
    let first = fixture.consume_records(&mut store, 1, 1);
    assert_eq!(first.records, 1);
    assert!(first.files <= 32);
    assert!(first.bytes <= 4 << 20);
    assert_eq!(
        fixture.cursor(&store).offset,
        Fixture::event(1, "needle first", "final").len() as u64
    );
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        1
    );
    fixture.expired_slices.set(1);
    let second = fixture.consume_records(&mut store, 128, 2);
    assert!(second.errors.is_empty());
    assert_eq!(
        fixture.cursor(&store).offset,
        fs::metadata(&fixture.path).unwrap().len()
    );
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        2
    );
    assert!(store
        .index
        .query("PRIVATE_REASONING_TOKEN", 100, None)
        .unwrap()
        .hits
        .is_empty());
    fixture.append(&Fixture::event(4, "needle third", "final"));
    fixture.expired_slices.set(1);
    fixture.consume_records(&mut store, 128, 1);
    assert_eq!(
        fixture.cursor(&store).offset,
        fs::metadata(&fixture.path).unwrap().len()
    );
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        3
    );
    assert_eq!(store.index.status().unwrap().state, "indexing"); // no catalog coverage claim
    drop(store);
    let reopened = fixture.store();
    assert_eq!(
        reopened
            .index
            .query("needle", 100, None)
            .unwrap()
            .hits
            .len(),
        3
    );
}

#[test]
fn split_final_line_and_budget_stop_do_not_advance_or_retire_the_source() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let row = Fixture::event(1, "needle complete", "final");
    fixture.append(row.trim_end_matches('\n'));
    fixture.step(&mut store, 128);
    assert!(store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .is_empty());
    fixture.append("\n");
    fixture.step(&mut store, 128);
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        1
    );
    let budget = Budget {
        bytes: 10,
        ..fixture.budget(128)
    };
    let progress = store
        .step(
            &fixture.state,
            std::slice::from_ref(&fixture.identity),
            &budget,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(progress.stopped);
    assert_eq!(progress.bytes, 0);
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        1
    );
}

#[test]
fn cancellation_and_deadline_do_no_source_io() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let cancelled = store
        .step(
            &fixture.state,
            std::slice::from_ref(&fixture.identity),
            &fixture.budget(128),
            &AtomicBool::new(true),
        )
        .unwrap();
    assert!(cancelled.stopped);
    assert_eq!(cancelled.files, 0);
    assert_eq!(cancelled.bytes, 0);
    let budget = Budget {
        deadline: Instant::now() - Duration::from_secs(1),
        ..fixture.budget(128)
    };
    let expired = store
        .step(
            &fixture.state,
            std::slice::from_ref(&fixture.identity),
            &budget,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(expired.stopped);
    assert_eq!(expired.files, 0);
    assert_eq!(store.index.status().unwrap().sources, 0);
}

#[test]
fn a_later_pass_sweeps_a_forgotten_target_without_it_being_requested_again() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.append(&Fixture::event(1, "needle public", "final"));
    fixture.step(&mut store, 128);
    fs::remove_file(&fixture.binding).unwrap();
    store
        .step(
            &fixture.state,
            &[],
            &fixture.budget(128),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(store
        .index
        .query("needle", 100, None)
        .unwrap()
        .hits
        .is_empty());
    assert_eq!(store.index.status().unwrap().messages, 0);
    let cursors: i64 = store
        .index
        .connection()
        .query_row("SELECT COUNT(*) FROM input_cursors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cursors, 0);
}

#[test]
fn forgotten_changed_and_replaced_sources_cannot_reappear() {
    for mode in ["forget", "binding", "transcript"] {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        fixture.append(&Fixture::event(1, "needle public", "final"));
        fixture.prepare_until(
            &mut store,
            std::slice::from_ref(&fixture.identity),
            |store, _| store.index.status().unwrap().messages == 1,
        );
        assert_eq!(
            store.index.query("needle", 100, None).unwrap().hits.len(),
            1
        );
        match mode {
            "forget" => fs::remove_file(&fixture.binding).unwrap(),
            "binding" => {
                let mut changed = fixture.record.clone();
                changed["conversation_id"] = json!(uuid::Uuid::new_v4().to_string());
                fs::write(&fixture.binding, changed.to_string()).unwrap();
            }
            _ => {
                let replacement = fixture.temp.path().join("replacement");
                fs::write(&replacement, Fixture::event(2, "needle replaced", "final")).unwrap();
                fs::rename(replacement, &fixture.path).unwrap();
            }
        }
        let result = fixture.prepare_until(
            &mut store,
            std::slice::from_ref(&fixture.identity),
            |_, progress| !progress.errors.is_empty(),
        );
        assert!(!result.errors.is_empty(), "{mode}");
        assert!(
            store
                .index
                .query("needle", 100, None)
                .unwrap()
                .hits
                .is_empty(),
            "{mode}"
        );
        fixture.step(&mut store, 128);
        assert!(store
            .index
            .query("needle", 100, None)
            .unwrap()
            .hits
            .is_empty());
    }
}

#[test]
fn journal_pages_use_exact_conversation_and_resume_the_sequence() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let db = Connection::open(fixture.state.join("events.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE events(seq INTEGER PRIMARY KEY,name TEXT,conversation TEXT,payload TEXT);CREATE INDEX events_session ON events(name,conversation,seq)").unwrap();
    for i in 1..=3 {
        db.execute(
            "INSERT INTO events VALUES(?,?,?,?)",
            params![
                i,
                fixture.identity.name,
                fixture.identity.conversation_id,
                json!({"type":"Stop","agent_id":"","detail":format!("needle {i}"),"at":i})
                    .to_string()
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO events VALUES(4,?,?,?)",
        params![
            fixture.identity.name,
            "another-conversation",
            json!({"type":"Stop","detail":"PRIVATE_OTHER_CONVERSATION","at":4}).to_string()
        ],
    )
    .unwrap();
    fixture.expired_slices.set(1);
    fixture.consume_records(&mut store, 1, 1);
    assert_eq!(fixture.cursor(&store).seq, 1);
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        1
    );
    fixture.expired_slices.set(2);
    fixture.consume_records(&mut store, 1, 1);
    assert_eq!(fixture.cursor(&store).seq, 2);
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        2
    );
    fixture.expired_slices.set(1);
    fixture.consume_records(&mut store, 128, 1);
    assert_eq!(fixture.cursor(&store).seq, 3);
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        3
    );
    assert!(store
        .index
        .query("PRIVATE_OTHER", 100, None)
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn same_name_live_and_archive_remain_distinct_through_archive_forget() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.append(&Fixture::event(1, "needle live", "final"));
    let archived = Identity {
        name: fixture.identity.name.clone(),
        run_id: uuid::Uuid::new_v4().to_string(),
        conversation_id: uuid::Uuid::new_v4().to_string(),
        archive_id: uuid::Uuid::new_v4().to_string(),
    };
    let transcript = fixture
        .temp
        .path()
        .join(format!("{}.jsonl", archived.conversation_id));
    fs::write(&transcript, Fixture::event(2, "needle archive", "final")).unwrap();
    let mut record = fixture.record.clone();
    record["run_id"] = json!(archived.run_id);
    record["conversation_id"] = json!(archived.conversation_id);
    record["archive_id"] = json!(archived.archive_id);
    record["archived_at"] = json!(42);
    record["transcript"] = json!(transcript);
    fs::create_dir(fixture.state.join("archives")).unwrap();
    let archive = fixture
        .state
        .join("archives")
        .join(format!("{}.json", archived.archive_id));
    fs::write(&archive, record.to_string()).unwrap();
    fixture.prepare_until(
        &mut store,
        &[fixture.identity.clone(), archived.clone()],
        |store, _| store.index.status().unwrap().messages == 2,
    );
    assert_eq!(
        store.index.query("needle", 100, None).unwrap().hits.len(),
        2
    );
    fs::remove_file(archive).unwrap();
    fixture.prepare_until(&mut store, &[], |store, _| {
        store.index.status().unwrap().messages == 1
    });
    let page = store.index.query("needle", 100, None).unwrap();
    assert_eq!(page.hits.len(), 1);
    assert!(page.hits[0].target.archive_id.is_empty());
}

#[cfg(unix)]
#[test]
fn copied_loose_modes_links_and_other_configuration_are_refused() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    drop(fixture.store());
    let dir = fixture.state.join("content-search-v1");
    let db = dir.join("index.sqlite3");
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    {
        let store = fixture.store();
        let tx = store.index.connection().unchecked_transaction().unwrap();
        tx.execute("UPDATE totals SET catalog_ready=1 WHERE id=1", [])
            .unwrap();
        assert_eq!(
            fs::metadata(dir.join("index.sqlite3-journal"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        tx.rollback().unwrap();
    }
    fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Store::open(&dir).is_err());
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let journal = dir.join("index.sqlite3-journal");
    fs::write(&journal, "").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Store::open(&dir).is_err());
    fs::remove_file(&journal).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(&dir).is_err());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let copy = fixture.state.join("original");
    fs::rename(&db, &copy).unwrap();
    symlink(&copy, &db).unwrap();
    assert!(Store::open(&dir).is_err());
    fs::remove_file(&db).unwrap();
    fs::rename(&copy, &db).unwrap();
    let other = Fixture::new();
    let other_dir = other.state.join("content-search-v1");
    private_dir(&other_dir).unwrap();
    fs::copy(&db, other_dir.join("index.sqlite3")).unwrap();
    assert!(Store::open(&other_dir).is_err());
}
