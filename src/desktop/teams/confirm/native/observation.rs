use super::*;
use crate::desktop::original_store::{self, Roots};
#[cfg(test)]
use rusqlite::Connection;
use std::{io::Read, path::Path};

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").into()
}
impl Native<'_> {
    pub fn snapshot(&mut self, b: &Binding) -> Result<Snapshot> {
        let (_, name) = self.binding(&b.team, &b.member)?;
        self.capture = None;
        let (snapshot, raw) = observed(&name, self.current(b, &name))?;
        self.capture = Some((snapshot.clone(), raw));
        Ok(snapshot)
    }
    fn current(&self, b: &Binding, name: &str) -> Result<(Snapshot, Value)> {
        let roots =
            Roots::open(&self.config.state, "question_receipts").map_err(|e| e.to_string())?;
        let args = vec![
            "inspect".into(),
            "--session-id".into(),
            b.session.clone(),
            "--after".into(),
            i64::MAX.to_string(),
            "--skip-processes".into(),
        ];
        if let Ok(view) = self.read(args, None) {
            let raw = original_store::session_record(
                &roots,
                &text(&view, "name"),
                &b.run,
                &text(&view, "conversation_id"),
            ).ok_or("A current native session has different or unreadable identity; no archive was selected")?;
            return self.read_view(b, &view, raw, None);
        }
        let (id, raw) = archive(&self.config.state, &roots, b, name)?;
        let view = self.read(
            vec![
                "inspect".into(),
                name.into(),
                "--archive".into(),
                id.clone(),
                "--after".into(),
                i64::MAX.to_string(),
                "--skip-processes".into(),
            ],
            None,
        )?;
        self.read_view(b, &view, raw, Some(id))
    }
    fn read_view(
        &self,
        b: &Binding,
        v: &Value,
        raw: Value,
        archive: Option<String>,
    ) -> Result<(Snapshot, Value)> {
        let mut s = settled_view(self.machine, b, v, &raw, archive)?;
        s.ready = s.live
            && crate::state::resumed_readiness(&raw) == crate::state::ResumedReadiness::Ready;
        // Before a lead's first message there is no conversation to classify
        // as resumed. Reuse the send's existing first-message observation;
        // the original send still checks the terminal and exact run itself.
        let first_lead = s.conversation.is_empty()
            && self.store.team(&b.team).map_err(|e| e.to_string())?
                .is_some_and(|team| team.lead == b.member);
        if first_lead {
            s.ready = s.live && crate::state::first_message_ready(&raw);
        }
        // Only Codex has the reviewed --observe CLI. Keep that agent scope.
        if s.live && s.agent == "codex" && (!s.conversation.is_empty() || first_lead)
            && s.questions.is_empty() {
            let request =
                json!({"expected_run_id":s.run,"expected_conversation_id":s.conversation})
                    .to_string();
            s.ready = self
                .observe_send(&s.name, request.as_bytes())
                .is_ok_and(|v| v["ready"] == true);
        }
        Ok((s, raw))
    }
}
/// A member's state as read, or unknown. A read that fails or is incomplete
/// (a session record or archive that cannot be read yet, a question index that
/// cannot be read) is never an exit (zerus-63rru.56): nothing is planned, so
/// nothing is resumed or claimed and the run keeps its one resume, and the
/// scan reports why.
pub(super) fn observed(name: &str, read: Result<(Snapshot, Value)>) -> Result<(Snapshot, Value)> {
    read.map_err(|why| {
        format!("Whether member session {name} ended is unknown ({why}); nothing was resumed")
    })
}
pub(super) fn parse_view(
    machine: &str,
    b: &Binding,
    v: &Value,
    raw: &Value,
    archive: Option<String>,
) -> Result<Snapshot> {
    if raw["worktree_owner"]["id"] != b.nonce
        || ["name", "agent", "cwd"]
            .iter()
            .any(|key| v[key] != raw[key])
        || !v["pending_questions_error"].is_null()
        || v["tracked"] != true
        || v["run_id"] != b.run
        || v["session_id"] != b.session
        || v["launch_id"] != b.launch
        || v["conversation_id"] != raw["conversation_id"]
    {
        return Err("The fresh member inspection is incomplete or changed".into());
    }
    let questions = v["pending_questions"]
        .as_array()
        .ok_or("Pending questions are unavailable")?
        .clone();
    let live = v["runtime_state"] == "live" && v["process_state"] == "running";
    let ended = archive.is_some() || v["process_state"] == "exited";
    // An archive's own inspection that lists its allowed actions without
    // `restore` does not offer one (zerus-63rru.62); its reason goes with it.
    let restore_refusal = archive.as_ref().and_then(|_| {
        let allowed = v["allowed_actions"].as_array()?;
        (!allowed.iter().any(|action| action == "restore")).then(|| {
            let why = crate::desktop::teams::reason_text(
                v["action_reasons"]["restore"].as_str().unwrap_or(""),
            );
            if why.is_empty() {
                "the archive's inspection does not offer a restore".into()
            } else {
                why
            }
        })
    });
    let s = Snapshot {
        machine: machine.into(),
        launch: text(raw, "launch_id"),
        session: text(raw, "session_id"),
        name: text(raw, "name"),
        run: text(raw, "run_id"),
        conversation: text(raw, "conversation_id"),
        agent: text(raw, "agent"),
        account: text(raw, "account_id"),
        folder: text(raw, "cwd"),
        live,
        ready: false,
        paused: paused(raw),
        ended,
        self_exit: self_exit(raw, archive.is_some()),
        exit_identity: super::super::engine::key(&json!([
            raw["completion_source"],
            raw["completion_reason"],
            raw["exited_at"],
            raw["exit_code"],
            raw["termination_signal"],
            archive
        ])),
        archive,
        receipt_at: None,
        restore_refusal,
        resume_wait: None,
        questions,
    };
    super::super::policy::session(b, &s)?;
    Ok(s)
}
pub(super) fn paused(v: &Value) -> bool {
    v["paused"] != Value::Null && v["paused"] != false
        || ["pausing", "terminating", "restore_archive_cleanup"]
            .iter()
            .any(|k| v.get(k).is_some())
}
pub(super) fn self_exit(v: &Value, archived: bool) -> bool {
    if paused(v)
        || v["completion_source"] != "supervisor"
        || !v["exited_at"]
            .as_f64()
            .is_some_and(|t| t.is_finite() && t > 0.0)
        || v["conversation_id"].as_str().is_none_or(str::is_empty)
    {
        return false;
    }
    match v["completion_reason"].as_str() {
        Some("clean_exit") => v["exit_code"] == 0 && v["termination_signal"].is_null(),
        Some("unconfirmed_exit" | "signal") if !archived => {
            v["exit_code"].as_i64().is_some()
                || v["termination_signal"].as_i64().is_some_and(|n| n > 0)
        }
        _ => false,
    }
}
fn settled_view(machine: &str, b: &Binding, v: &Value, raw: &Value, archive: Option<String>) -> Result<Snapshot> {
    let mut s = parse_view(machine, b, v, raw, archive)?;
    s.resume_wait = resume_wait(&s, v, raw)?;
    Ok(s)
}
fn resume_wait(s: &Snapshot, view: &Value, raw: &Value) -> Result<Option<ResumeWait>> {
    if !s.self_exit || s.archive.is_none() {
        return Ok(None);
    }
    if !view["restore_name_busy"].as_bool().ok_or("Restore name readiness is unavailable")? {
        return Ok(None);
    }
    let exited = raw["exited_at"].as_f64().ok_or("The original exit time is unavailable")? * 1000.0;
    let bound = super::super::engine::RESUME_SETTLE_MS;
    if !exited.is_finite() || exited <= 0.0 || exited >= (i64::MAX - bound) as f64 {
        return Err("The original exit time is invalid".into());
    }
    Ok(Some(ResumeWait {
        until: (exited.ceil() as i64).saturating_add(bound),
        reason: "waiting for the original run to settle".into(),
    }))
}
#[cfg(all(test, unix))]
mod settlement;
fn archive(state: &Path, roots: &Roots, b: &Binding, name: &str) -> Result<(String, Value)> {
    let state = crate::platform::canonical(state).map_err(|e| e.to_string())?;
    let id = crate::state::archive_catalog::find_run(&state, name, &b.run)?
        .ok_or("No exact unique exit archive")?;
    let file = roots
        .open_file(&["archives", &format!("{id}.json")])
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take((1 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1 << 20 {
        return Err("Exit archive exceeds one MiB".into());
    }
    let raw: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if raw["archive_id"] != id
        || raw["name"] != name
        || raw["run_id"] != b.run
        || raw["state"] != "archived"
    {
        return Err("The exact archive identity changed".into());
    }
    Ok((id, raw))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn native_fixture() -> (Binding, Value, Value) {
        let (b, _, s) = crate::desktop::teams::confirm::policy::tests::sample();
        let raw = json!({"name":s.name,"run_id":b.run,"conversation_id":"","session_id":b.session,"launch_id":b.launch,"agent":b.agent,"account_id":b.account,"cwd":b.folder,"worktree_owner":{"id":b.nonce}});
        let mut view = raw.clone();
        view["tracked"] = json!(true);
        view["runtime_state"] = json!("live");
        view["process_state"] = json!("running");
        view["pending_questions"] = json!(s.questions);
        (b, view, raw)
    }
    /// Runs one ignored fixture alone in a child of this test binary with a
    /// private state root and configuration and no session markers.
    fn isolated(test: &str) {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--ignored", "--test-threads=1"])
            .env("HGS_STATE_DIR", temp.path().join("state"))
            .env("HGS_CONFIG_DIR", temp.path().join("config"))
            .env_remove("HGS_SESSION")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_RUN_ID")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    mod lead151;

    /// A resumed Codex member's readiness is asked with an argv the CLI's own
    /// observe gate admits (zerus-63rru.54): the argv the service spawns runs
    /// through `cli::dispatch_at`, and must not be that gate's refusal.
    #[test]
    fn a_resumed_codex_members_observation_passes_the_clis_own_gate() {
        isolated("desktop::teams::confirm::native::observation::tests::observation_gate_fixture");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn observation_gate_fixture() {
        let state = std::path::PathBuf::from(std::env::var("HGS_STATE_DIR").unwrap());
        let config = Config {
            home: String::new(),
            dir: std::path::PathBuf::from(std::env::var("HGS_CONFIG_DIR").unwrap()),
            state: state.clone(),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        crate::platform::private_dir(&state.join("teams")).unwrap();
        std::fs::write(state.join("teams/preview"), "on").unwrap();
        let mut store = crate::teams::store::Store::open(&state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (mut b, mut view, mut raw) = native_fixture();
        b.agent = "codex".into();
        b.conversation = "conversation-1".into();
        for v in [&mut view, &mut raw] {
            v["agent"] = json!("codex");
            v["name"] = json!("codex/example/member");
            v["conversation_id"] = json!("conversation-1");
        }
        view["pending_questions"] = json!([]);
        let native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        crate::desktop::teams::child::FIXTURE_EXIT
            .with(|slot| *slot.borrow_mut() = Some(br#"{"ready":true}"#.to_vec()));
        let (s, _) = native.read_view(&b, &view, raw, None).unwrap();
        assert!(s.ready);
        let spawned = crate::desktop::teams::child::FIXTURE_ARGS
            .with(|slot| slot.borrow_mut().pop())
            .expect("the observation child");
        let gate = |argv: Vec<String>| match crate::cli::dispatch_at(argv, Instant::now()) {
            Err(error) if error.code == 2 && error.message.contains("--observe takes exactly") => {
                Err(error.message)
            }
            _ => Ok(()),
        };
        assert_eq!(gate(spawned.clone()), Ok(()), "{spawned:?}");
        // The machine guard in front of the same words is what the gate refused.
        let mut guarded = vec!["--expect-machine".to_owned(), b.machine.clone()];
        guarded.extend(spawned);
        assert!(gate(guarded).is_err());
    }

    /// The private store of an isolated fixture: its configuration, team
    /// store and stop flag, at the state root the state component writes.
    fn store_env() -> (Config, crate::teams::store::Store, Arc<AtomicBool>) {
        let state = std::path::PathBuf::from(std::env::var("HGS_STATE_DIR").unwrap());
        let config = Config {
            home: String::new(),
            dir: std::path::PathBuf::from(std::env::var("HGS_CONFIG_DIR").unwrap()),
            state: state.clone(),
            selves: vec![],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: Default::default(),
            tab: false,
            tab_colors: String::new(),
        };
        crate::platform::private_dir(&state.join("teams")).unwrap();
        std::fs::write(state.join("teams/preview"), "on").unwrap();
        let store = crate::teams::store::Store::open(&state).unwrap();
        (config, store, Arc::new(AtomicBool::new(false)))
    }

    /// A Codex member's session record, saved by the state's own writer, and
    /// the real `hgs inspect` reply for it (zerus-63rru.56): everything the
    /// confirmation reads comes from those producers. Its transcript is not
    /// written yet, as at a session's start, so the question index cannot be
    /// read. The binding is the one the record proves.
    fn real_codex(edit: impl FnOnce(&mut Value)) -> (Binding, Value, Value) {
        let state = std::env::var("HGS_STATE_DIR").unwrap();
        let conversation = uuid::Uuid::new_v4().to_string();
        let mut record = json!({"name":"codex/example/member","agent":"codex","version":1,
            "run_id":uuid::Uuid::new_v4().to_string(),"conversation_id":conversation,
            "launch_id":uuid::Uuid::new_v4().to_string(),"account_id":"account-1",
            "cwd":state,"worktree_owner":{"id":"owner-1"},"created":1.0,
            "transcript":format!("{state}/transcripts/rollout-{conversation}.jsonl")});
        edit(&mut record);
        let view = crate::state::inspection_for_test(&mut record).unwrap();
        let (sample, _, _) = crate::desktop::teams::confirm::policy::tests::sample();
        let b = Binding {
            agent: "codex".into(),
            account: "account-1".into(),
            launch: record["launch_id"].as_str().unwrap().into(),
            session: view["session_id"].as_str().unwrap_or_default().into(),
            run: record["run_id"].as_str().unwrap().into(),
            conversation: String::new(),
            folder: state,
            nonce: "owner-1".into(),
            ..sample
        };
        (b, view, record)
    }

    #[test]
    fn a_saved_dead_member_without_an_archive_needs_readable_exit_evidence() {
        isolated("desktop::teams::confirm::native::observation::tests::saved_without_archive_child");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn saved_without_archive_child() {
        use crate::desktop::teams::confirm::test_inspect;
        let (config, mut store, stop) = store_env();
        let (sample, _, _) = crate::desktop::teams::confirm::policy::tests::sample();
        let b = Binding {
            agent: "codex".into(),
            machine: crate::swarm::local_node_id(&config).unwrap(),
            launch: uuid::Uuid::new_v4().to_string(),
            session: uuid::Uuid::new_v4().to_string(),
            run: uuid::Uuid::new_v4().to_string(),
            conversation: uuid::Uuid::new_v4().to_string(),
            ..sample
        };
        let name = "codex/example/member";
        let record = test_inspect::save_then_exit(&config, &b, name);
        for key in ["runtime_state", "process_state", "tracked", "completion_source", "exited_at"] {
            assert!(record.get(key).is_none(), "{key}");
        }
        assert_eq!(crate::state::dispatch_test_archive_catalog_worker().unwrap(), 0);
        let native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        test_inspect::use_cli_inspection();
        let (snapshot, _) = native.current(&b, name).unwrap();
        assert!(snapshot.ended);
        assert!(!snapshot.live && !snapshot.self_exit);
        assert!(snapshot.archive.is_none());
        assert_eq!(
            crate::desktop::teams::confirm::policy::resume(&snapshot).unwrap_err(),
            "The exit has no exact supervisor evidence"
        );

        crate::state::listing_fixture::withhold_saved_record(name, true).unwrap();
        let error = observed(name, native.current(&b, name)).unwrap_err();
        assert_eq!(
            error,
            format!("Whether member session {name} ended is unknown (No exact unique exit archive); nothing was resumed")
        );
        assert!(!config.state.join("archive-catalog-v2/teams-attempt.json").exists());
        crate::state::listing_fixture::withhold_saved_record(name, false).unwrap();
    }

    #[test]
    fn an_unreadable_question_index_is_unknown_never_an_exit() {
        isolated("desktop::teams::confirm::native::observation::tests::index_fixture");
    }
    /// zerus-63rru.62: a member that exited cleanly, with supervisor
    /// evidence, whose archive keeps no launch recipe. The state's own
    /// inspection of that archive leaves `restore` out of `allowed_actions`
    /// and says why; the policy plans no restore, and its refusal carries
    /// that reason.
    #[test]
    fn an_archive_whose_inspection_offers_no_restore_plans_none() {
        isolated("desktop::teams::confirm::native::observation::tests::no_restore_fixture");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn no_restore_fixture() {
        let (config, _, _) = store_env();
        let (b, _, record) = real_codex(codex_self_exit);
        crate::state::archive_for_test(&record).unwrap();
        let roots = Roots::open(&config.state, "question_receipts").unwrap();
        let name = record["name"].as_str().unwrap();
        let (id, raw) = archive(&config.state, &roots, &b, name).unwrap();
        // An inspection only requests the background receipt worker. This
        // Rust test executable does not run CLI worker commands; complete
        // the private fixture through the same explicit native index read.
        crate::state::listing_fixture::complete_questions(&record).unwrap();
        let view = crate::state::archive_inspection_for_test(name, &id).unwrap();
        assert!(view["pending_questions_error"].is_null(), "{view}");
        let allowed = view["allowed_actions"].as_array().unwrap();
        assert!(!allowed.iter().any(|action| action == "restore"), "{view}");
        let reason = view["action_reasons"]["restore"]
            .as_str()
            .unwrap()
            .to_owned();
        let s = parse_view(&b.machine, &b, &view, &raw, Some(id)).unwrap();
        let refusal = crate::desktop::teams::confirm::policy::resume(&s);
        eprintln!("the inspection's reason: {reason}; the policy: {refusal:?}");
        assert!(
            refusal.as_ref().is_err_and(|why| why.contains(&reason)
                && why.ends_with(crate::desktop::teams::confirm::policy::SEAT)),
            "{refusal:?}"
        );
    }

    fn codex_self_exit(record: &mut Value) {
        // A worked member's transcript starts with the Codex session header,
        // which lets the real question index read it.
        let transcript = Path::new(record["transcript"].as_str().unwrap());
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let header = json!({"type":"session_meta","payload":{"id":record["conversation_id"]}});
        std::fs::write(transcript, format!("{header}\n")).unwrap();
        record["completion_source"] = json!("supervisor");
        record["completion_reason"] = json!("clean_exit");
        record["exit_code"] = json!(0);
        record["exited_at"] = json!(1000.0);
    }

    #[test]
    fn an_archive_is_found_by_run_as_soon_as_it_is_written() {
        isolated("desktop::teams::confirm::native::observation::tests::archive_fixture");
    }
    #[test]
    fn an_archive_with_incomplete_coverage_is_observed_after_repair() {
        isolated("desktop::teams::confirm::native::observation::tests::repair_exit_catalog_child");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn repair_exit_catalog_child() {
        use crate::desktop::teams::confirm::test_inspect;
        use std::io::Write;
        let (config, mut store, stop) = store_env();
        let (mut b, _, record) = real_codex(codex_self_exit);
        b.machine = crate::swarm::local_node_id(&config).unwrap();
        b.conversation = text(&record, "conversation_id");
        let id = crate::state::listing_fixture::archive_with_catalog_gap(&record).unwrap();
        let name = record["name"].as_str().unwrap();
        let native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        test_inspect::use_cli_inspection();

        // The original exists, but a rolled-back catalog write leaves the
        // first observation unknown. The lookup requests repair without
        // waiting for the worker or inspecting an unproved archive.
        let error = observed(name, native.current(&b, name)).unwrap_err();
        assert_eq!(
            error,
            format!(
                "Whether member session {name} ended is unknown ({}); nothing was resumed",
                crate::state::archive_catalog::INCOMPLETE
            )
        );
        let admitted = config.state.join("archive-catalog-v2/teams-attempt.json");
        assert!(admitted.is_file(), "the confirmation requested bounded repair");
        let original = config.state.join("archives").join(format!("{id}.json"));
        assert!(original.is_file());

        // Run the maintained writer explicitly in this test executable.
        // Only the next observation can use the now-covered exact archive.
        assert_eq!(
            crate::state::dispatch_test_archive_catalog_worker().unwrap(),
            0
        );
        crate::state::listing_fixture::complete_questions(&record).unwrap();
        let (snapshot, raw) = native.current(&b, name).unwrap();
        assert_eq!(snapshot.archive.as_deref(), Some(id.as_str()));
        assert_eq!(raw["archive_id"], id);
        assert!(snapshot.ended && snapshot.self_exit);
        let reason = snapshot
            .restore_refusal
            .as_deref()
            .expect("fresh CLI inspection must refuse Restore for this archive");
        let refusal = crate::desktop::teams::confirm::policy::resume(&snapshot).unwrap_err();
        assert!(refusal.contains(reason), "{refusal}");

        let mut replaced = b.clone();
        replaced.nonce = "replacement-owner".into();
        assert_eq!(
            native.current(&replaced, name).unwrap_err(),
            "The fresh member inspection is incomplete or changed"
        );
        let mut later = b.clone();
        later.run = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            native.current(&later, name).unwrap_err(),
            "No exact unique exit archive"
        );

        // A stale catalog still has the same matching row when the native
        // file changes in place. The next read needs fresh coverage again.
        std::fs::OpenOptions::new()
            .append(true)
            .open(original)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(
            native.current(&b, name).unwrap_err(),
            crate::state::archive_catalog::INCOMPLETE
        );
    }
    #[test]
    fn a_persons_pause_reads_as_paused_not_as_an_exit_to_resume() {
        isolated("desktop::teams::confirm::native::observation::tests::pause_fixture");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn index_fixture() {
        let (config, mut store, stop) = store_env();
        let (b, view, _) = real_codex(|_| ());
        // The producer itself reports the unreadable index.
        assert!(view["pending_questions_error"].is_string(), "{view}");
        let native = Native::new(&config, &b.machine, &stop, &mut store).unwrap();
        let name = view["name"].as_str().unwrap();
        crate::desktop::teams::child::FIXTURE_EXIT
            .with(|slot| *slot.borrow_mut() = Some(serde_json::to_vec(&view).unwrap()));
        let read = native.current(&b, name);
        assert_eq!(
            read.as_ref().err().map(String::as_str),
            Some("The fresh member inspection is incomplete or changed"),
            "{read:?}"
        );
        let unknown = observed(name, read).unwrap_err();
        assert!(
            unknown.contains("is unknown") && unknown.contains("nothing was resumed"),
            "{unknown}"
        );
        // The same reply without the producer's error is read as the member.
        let mut readable = view.clone();
        readable
            .as_object_mut()
            .unwrap()
            .remove("pending_questions_error");
        crate::desktop::teams::child::FIXTURE_EXIT
            .with(|slot| *slot.borrow_mut() = Some(serde_json::to_vec(&readable).unwrap()));
        let snapshot = observed(name, native.current(&b, name)).unwrap().0;
        assert!(!snapshot.ended, "{snapshot:?}");
    }

    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn archive_fixture() {
        let (config, _, _) = store_env();
        let (b, _, record) = real_codex(|_| ());
        let id = crate::state::archive_for_test(&record).unwrap();
        let roots = Roots::open(&config.state, "question_receipts").unwrap();
        let (found, raw) = archive(&config.state, &roots, &b, "codex/example/member").unwrap();
        assert_eq!(
            (found.as_str(), &raw["state"]),
            (id.as_str(), &json!("archived"))
        );
        let alias = config.state.join("alias");
        std::os::unix::fs::symlink(&config.state, &alias).unwrap();
        let (found, _) = archive(&alias, &roots, &b, "codex/example/member").unwrap();
        assert_eq!(found, id);
        let path = config.state.join("archive-catalog-v1.sqlite3");
        // Simulate an interrupted build using only the advisory cache. An
        // indexed row alone must not authorize an exact exit observation.
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "UPDATE catalog_meta SET value='0' WHERE key='archive_scan_complete_v2'",
            [],
        )
        .unwrap();
        drop(db);
        let error = archive(&alias, &roots, &b, "codex/example/member").unwrap_err();
        assert!(
            error.starts_with(crate::state::archive_catalog::INCOMPLETE),
            "{error}"
        );
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "UPDATE catalog_meta SET value='1' WHERE key='archive_scan_complete_v2'",
            [],
        )
        .unwrap();
        drop(db);
        let saved = path.with_extension("saved");
        std::fs::rename(&path, &saved).unwrap();
        let error = archive(&alias, &roots, &b, "codex/example/member").unwrap_err();
        assert!(
            error.starts_with(crate::state::archive_catalog::INCOMPLETE),
            "{error}"
        );
        std::os::unix::fs::symlink(&saved, &path).unwrap();
        assert!(archive(&alias, &roots, &b, "codex/example/member").is_err());
    }
    #[test]
    #[ignore = "run by its isolated wrapper"]
    fn pause_fixture() {
        let (b, _, mut record) = real_codex(|record| {
            // A finished run of this test's own process: started, not running.
            record["pid"] = json!(u32::MAX - 1);
            record["process_start"] = json!("1");
        });
        crate::state::pause_for_test(&mut record).unwrap();
        let mut view = crate::state::inspection_for_test(&mut record).unwrap();
        view.as_object_mut()
            .unwrap()
            .remove("pending_questions_error");
        let s = parse_view(&b.machine, &b, &view, &record, None).unwrap();
        assert_eq!((s.paused, s.ended, s.live), (true, true, false), "{view}");
    }

    #[test]
    fn native_read_requires_the_bound_launch_run_session_agent_account_folder_and_owner() {
        let (b, v, raw) = native_fixture();
        let s = parse_view(&b.machine, &b, &v, &raw, None).unwrap();
        super::super::super::policy::trust(&b, &s).unwrap();
        for field in [
            "run_id",
            "session_id",
            "launch_id",
            "agent",
            "account_id",
            "cwd",
        ] {
            let mut other = raw.clone();
            other[field] = json!("other");
            let mut view = v.clone();
            view[field] = other[field].clone();
            assert!(
                parse_view(&b.machine, &b, &view, &other, None).is_err(),
                "{field}"
            );
        }
        let mut other = raw.clone();
        other["worktree_owner"]["id"] = json!("other");
        assert!(parse_view(&b.machine, &b, &v, &other, None).is_err());
        for field in [
            "tracked",
            "pending_questions",
            "pending_questions_error",
            "name",
            "conversation_id",
            "run_id",
            "session_id",
            "launch_id",
            "agent",
            "cwd",
        ] {
            let mut view = v.clone();
            view[field] = json!("other");
            assert!(
                parse_view(&b.machine, &b, &view, &raw, None).is_err(),
                "{field}"
            );
        }
        let mut bound = b.clone();
        bound.conversation = "conversation-1".into();
        assert!(parse_view(&b.machine, &bound, &v, &raw, None).is_err());
    }
    /// The real startup shape end to end: inspect and the record both carry
    /// a null conversation, and so does the card. The grant takes it.
    #[test]
    fn a_real_startup_view_with_null_conversations_reaches_the_trust_grant() {
        let (b, s) = crate::desktop::teams::confirm::policy::tests::real_card();
        let raw = json!({"name":s.name,"run_id":b.run,"conversation_id":null,"session_id":b.session,"launch_id":b.launch,"agent":b.agent,"account_id":b.account,"cwd":b.folder,"worktree_owner":{"id":b.nonce}});
        let mut view = raw.clone();
        view["tracked"] = json!(true);
        view["runtime_state"] = json!("live");
        view["process_state"] = json!("running");
        view["pending_questions"] = json!(s.questions);
        let parsed = parse_view(&b.machine, &b, &view, &raw, None).unwrap();
        assert_eq!(parsed.conversation, "");
        assert_eq!(
            super::super::super::policy::trust(&b, &parsed).unwrap(),
            s.questions[0]
        );
    }
    #[test]
    fn clean_supervisor_archive_and_explicit_crashes_are_eligible_but_ambiguous_exits_are_manual() {
        let clean = json!({"completion_source":"supervisor","completion_reason":"clean_exit","exited_at":1000.0,"exit_code":0,"termination_signal":null,"conversation_id":"conversation-1"});
        assert!(self_exit(&clean, true));
        for (key, value) in [
            ("completion_source", json!("manual_archive")),
            ("completion_source", json!("manual_terminate")),
            ("completion_reason", json!("user_archived")),
            ("completion_reason", json!("user_terminated")),
            ("completion_reason", json!("pause")),
            ("exited_at", Value::Null),
            ("conversation_id", json!("")),
            ("exit_code", json!(1)),
            ("paused", json!(true)),
            ("pausing", Value::Null),
            ("terminating", Value::Null),
        ] {
            let mut v = clean.clone();
            v[key] = value;
            assert!(!self_exit(&v, true), "{key}");
        }
        let mut crash = clean.clone();
        crash["completion_reason"] = json!("unconfirmed_exit");
        crash["exit_code"] = json!(1);
        assert!(self_exit(&crash, false));
        assert!(!self_exit(&crash, true));
        crash["completion_reason"] = json!("signal");
        crash["exit_code"] = Value::Null;
        crash["termination_signal"] = json!(9);
        assert!(self_exit(&crash, false));
        assert!(!self_exit(&crash, true));
        crash["termination_signal"] = Value::Null;
        assert!(!self_exit(&crash, false));
    }
}
