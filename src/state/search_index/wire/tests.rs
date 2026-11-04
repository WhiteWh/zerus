use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
    home: PathBuf,
    wire: PathBuf,
    identity: Identity,
    record: Value,
    binding: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        let home = temp.path().join("profile");
        let identity = Identity {
            name: "kimi/example/search".into(),
            run_id: uuid::Uuid::new_v4().to_string(),
            conversation_id: uuid::Uuid::new_v4().to_string(),
            archive_id: String::new(),
        };
        let wire = home
            .join("sessions/workspace-a")
            .join(&identity.conversation_id)
            .join("agents/main/wire.jsonl");
        fs::create_dir_all(wire.parent().unwrap()).unwrap();
        fs::create_dir(home.join("sessions/workspace-b")).unwrap();
        let record = json!({"version":1,"name":identity.name,"run_id":identity.run_id,
            "conversation_id":identity.conversation_id,"agent":"kimi","agent_home":home,"cwd":state});
        let binding = state.join(format!(
            "{:x}.json",
            Sha256::digest(identity.name.as_bytes())
        ));
        fs::write(&binding, record.to_string()).unwrap();
        let text=[Self::event("native-one","needle public message"),
            json!({"type":"agent.message.appended","agentId":"child","time":42000,
                "message":{"message":{"role":"assistant","content":[{"type":"text","text":"PRIVATE_CHILD_BODY"}]}}}).to_string()+"\n",
            json!({"type":"agent.message.appended","agentId":"main","time":42000,
                "message":{"message":{"role":"user","content":[{"type":"text","text":"# AGENTS.md instructions PRIVATE_SYSTEM_BODY"}]}}}).to_string()+"\n"].concat();
        fs::write(&wire, text).unwrap();
        Self {
            _temp: temp,
            state,
            home,
            wire,
            identity,
            record,
            binding,
        }
    }
    fn event(id: &str, text: &str) -> String {
        json!({"type":"agent.message.appended","agentId":"main","time":42000,
            "message":{"meta":{"messageId":id},"message":{"role":"assistant","content":[
                {"type":"think","think":"PRIVATE_REASONING_BODY"},{"type":"text","text":text}]}}})
        .to_string()
            + "\n"
    }
    fn store(&self) -> Store {
        Store::open(&self.state.join("content-search-v1")).unwrap()
    }
    fn step(&self, store: &mut Store, budget: &Budget) -> Progress {
        store
            .step(
                &self.state,
                std::slice::from_ref(&self.identity),
                budget,
                &AtomicBool::new(false),
            )
            .unwrap()
    }
    // Setup may yield at the production deadline. Resume committed work rather
    // than assuming a Windows runner finishes the fixture in one pass.
    fn prepare(&self, store: &mut Store, messages: u64, unavailable: u64) -> Progress {
        for _ in 0..32 {
            let progress = self.step(store, &budget());
            let status = store.index.status().unwrap();
            if status.messages == messages && status.unavailable_sources == unavailable
                && status.pending_sources == unavailable {
                return progress;
            }
        }
        panic!("wire fixture did not reach {messages} messages and {unavailable} unavailable sources in 32 bounded passes: {}",
            serde_json::to_string(&store.index.status().unwrap()).unwrap());
    }
    fn prepare_publication(&self, store: &mut Store) -> Value {
        // A bounded indexing slice may expire before it reaches this source.
        // Use the same 32-pass preparation fence as the other index fixtures,
        // retaining the original per-slice budgets and committed cursors.
        for _ in 0..32 {
            let slice = budget();
            let progress = self.step(store, &slice);
            if Instant::now() >= slice.deadline {
                continue;
            }
            assert!(!progress.stopped, "fixture work bound exhausted");
            assert!(progress.errors.is_empty(), "{:?}", progress.errors);
            let slice = budget();
            let reply = store
                .publish(
                    &self.state,
                    "needle",
                    100,
                    None,
                    &slice,
                    &AtomicBool::new(false),
                )
                .unwrap();
            if Instant::now() < slice.deadline {
                // Return the first completed publication, even if wrong. A
                // genuine lookup/invalidation failure must not be retried away.
                return reply;
            }
        }
        panic!("fixture preparation did not finish within 32 bounded passes");
    }
    fn publish(&self, store: &mut Store) -> Value {
        store
            .publish(
                &self.state,
                "needle",
                100,
                None,
                &budget(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }
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

#[test]
fn exact_kimi_public_wire_ingestion_and_publication_exclude_reasoning_and_foreign_agents() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let before = fs::read(&fixture.wire).unwrap();
    let progress = fixture.prepare(&mut store, 1, 0);
    assert_eq!(progress.directory_entries, 2);
    assert_eq!(progress.locator_probes, 2);
    assert_eq!(store.index.status().unwrap().messages, 1);
    let reply = fixture.publish(&mut store);
    assert_eq!(reply["results"][0]["message_id"], "native-one");
    assert_eq!(
        reply["results"][0]["event"]["detail"],
        "needle public message"
    );
    assert_eq!(reply["results"][0]["source"], "kimi_wire");
    assert_eq!(reply["results"][0]["run_id"], fixture.identity.run_id);
    assert_eq!(before, fs::read(&fixture.wire).unwrap());
    let raw = fs::read(fixture.state.join("content-search-v1/index.sqlite3")).unwrap();
    for private in [
        "PRIVATE_REASONING_BODY",
        "PRIVATE_CHILD_BODY",
        "PRIVATE_SYSTEM_BODY",
    ] {
        assert!(!raw
            .windows(private.len())
            .any(|part| part == private.as_bytes()));
    }
}

#[test]
fn partial_or_cancelled_workspace_discovery_never_admits_a_locator_or_message() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let mut tiny = budget();
    tiny.directory_entries = 1;
    let progress = (0..32).find_map(|_| {
        let progress = fixture.step(&mut store, &tiny);
        assert!(progress.stopped);
        assert_eq!(progress.locator_probes, 0);
        assert_eq!(store.index.status().unwrap().messages, 0);
        (progress.directory_entries == 1).then_some(progress)
    }).expect("partial discovery reached its one-entry cap");
    assert!(progress.stopped);
    assert_eq!(progress.directory_entries, 1);
    assert_eq!(progress.locator_probes, 0);
    assert_eq!(store.index.status().unwrap().messages, 0);
    assert_eq!(store.index.status().unwrap().pending_sources, 1);
    let cancelled = store
        .step(
            &fixture.state,
            std::slice::from_ref(&fixture.identity),
            &budget(),
            &AtomicBool::new(true),
        )
        .unwrap();
    assert_eq!(cancelled.directory_entries, 0);
    assert_eq!(cancelled.files, 0);
    fixture.prepare(&mut store, 1, 0);
    assert_eq!(
        fixture.publish(&mut store)["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_duplicate_under_an_existing_workspace_invalidates_the_saved_root_fingerprint() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.prepare(&mut store, 1, 0);
    let root = fixture.home.join("sessions");
    let before = directory_id(&root).unwrap();
    let duplicate = root
        .join("workspace-b")
        .join(&fixture.identity.conversation_id)
        .join("agents/main/wire.jsonl");
    fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
    fs::copy(&fixture.wire, &duplicate).unwrap();
    assert_eq!(
        before,
        directory_id(&root).unwrap(),
        "only the existing workspace changed"
    );
    let reply = fixture.publish(&mut store);
    assert!(reply["results"].as_array().unwrap().is_empty());
    fixture.prepare(&mut store, 1, 1);
    assert_eq!(store.index.status().unwrap().unavailable_sources, 1);
    assert!(fixture.publish(&mut store)["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_wire_added_below_an_existing_conversation_still_invalidates_uniqueness() {
    let fixture = Fixture::new();
    let duplicate = fixture
        .home
        .join("sessions/workspace-b")
        .join(&fixture.identity.conversation_id)
        .join("agents/main/wire.jsonl");
    fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
    let mut store = fixture.store();
    let initial = fixture.prepare_publication(&mut store);
    assert_eq!(initial["results"].as_array().unwrap().len(), 1);
    let root = fixture.home.join("sessions");
    let workspace = root.join("workspace-b");
    let before = (
        directory_id(&root).unwrap(),
        directory_id(&workspace).unwrap(),
    );
    fs::copy(&fixture.wire, duplicate).unwrap();
    assert_eq!(
        before,
        (
            directory_id(&root).unwrap(),
            directory_id(&workspace).unwrap()
        )
    );
    assert!(fixture.publish(&mut store)["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn an_expired_setup_slice_resumes_before_the_initial_wire_assertion() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let mut expired = budget();
    expired.deadline = Instant::now();
    let progress = fixture.step(&mut store, &expired);
    assert!(progress.stopped);
    assert_eq!(store.index.status().unwrap().messages, 0);
    let initial = fixture.prepare_publication(&mut store);
    assert_eq!(initial["results"].as_array().unwrap().len(), 1);
    assert_eq!(initial["results"][0]["message_id"], "native-one");
}

#[test]
fn locator_work_is_shared_across_targets_in_one_step() {
    let fixture = Fixture::new();
    fs::remove_dir(fixture.home.join("sessions/workspace-b")).unwrap();
    let mut second = fixture.identity.clone();
    second.name = "kimi/example/second".into();
    second.run_id = uuid::Uuid::new_v4().to_string();
    second.conversation_id = uuid::Uuid::new_v4().to_string();
    let wire = fixture
        .home
        .join("sessions/workspace-a")
        .join(&second.conversation_id)
        .join("agents/main/wire.jsonl");
    fs::create_dir_all(wire.parent().unwrap()).unwrap();
    fs::write(&wire, Fixture::event("second", "needle second")).unwrap();
    let mut record = fixture.record.clone();
    record["name"] = json!(second.name);
    record["run_id"] = json!(second.run_id);
    record["conversation_id"] = json!(second.conversation_id);
    fs::write(
        fixture
            .state
            .join(format!("{:x}.json", Sha256::digest(second.name.as_bytes()))),
        record.to_string(),
    )
    .unwrap();
    let mut store = fixture.store();
    let targets = [fixture.identity.clone(), second];
    for attempt in 0..32 {
        let mut tiny = budget();
        tiny.directory_entries = 1;
        let progress = store
            .step(&fixture.state, &targets, &tiny, &AtomicBool::new(false))
            .unwrap();
        // The shared one-entry/probe cap applies to every pass, even when
        // Windows suite load expires a pass before it reaches the locator.
        assert!(progress.directory_entries <= 1);
        assert!(progress.locator_probes <= 1);
        let status = store.index.status().unwrap();
        if progress.locator_probes == 1 && status.messages == 1 && status.pending_sources == 1 {
            assert_eq!(progress.directory_entries, 1);
            break;
        }
        assert!(
            attempt < 31,
            "fixture locator did not finish within 32 bounded passes"
        );
    }
    assert_eq!(store.index.status().unwrap().messages, 1);
    assert_eq!(store.index.status().unwrap().pending_sources, 1);
}

#[test]
fn appended_kimi_messages_resume_and_forgotten_bindings_stay_excluded() {
    use std::io::Write;
    let fixture = Fixture::new();
    let mut store = fixture.store();
    fixture.prepare(&mut store, 1, 0);
    fs::OpenOptions::new()
        .append(true)
        .open(&fixture.wire)
        .unwrap()
        .write_all(Fixture::event("native-two", "needle next public message").as_bytes())
        .unwrap();
    assert!(fixture.publish(&mut store)["results"]
        .as_array()
        .unwrap()
        .is_empty());
    fixture.prepare(&mut store, 2, 0);
    assert_eq!(
        fixture.publish(&mut store)["results"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fs::remove_file(&fixture.binding).unwrap();
    assert!(fixture.publish(&mut store)["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn a_linked_wire_never_copies_another_conversations_body() {
    let fixture = Fixture::new();
    let other = fixture._temp.path().join("foreign.jsonl");
    fs::write(&other, Fixture::event("foreign", "PRIVATE_FOREIGN_BODY")).unwrap();
    fs::remove_file(&fixture.wire).unwrap();
    std::os::unix::fs::symlink(other, &fixture.wire).unwrap();
    let mut store = fixture.store();
    fixture.prepare(&mut store, 0, 1);
    assert_eq!(store.index.status().unwrap().messages, 0);
    assert_eq!(store.index.status().unwrap().unavailable_sources, 1);
}
