//! A fleet row as the listing makes it, for tests of the readers of fleet
//! rows (the Teams actor, zerus-63rru.51). It runs `watch_source::rows`' own
//! steps for one live pane of `record`: the terminal's listing of that pane is
//! the only input given by hand, and every field of the row is the producer's.
//! The state root is the process's own (`HGS_STATE_DIR`), so a caller runs it
//! in a child test process with a private root.
use super::*;

/// The live row of `record`, whose agent process this test process stands in
/// for: its `pid` and `process_start` are this process's own.
pub(crate) fn listed_live_row(mut record: Value) -> Result<Value> {
    let pid = std::process::id();
    record["pid"] = json!(pid);
    record["process_start"] = json!(storage::process_start(pid));
    let listing = backend::Listing {
        clients: Vec::new(),
        sessions: vec![backend::SessionRow {
            name: terminal_name(&record).to_owned(),
            id: "$1".into(),
            attached: 0,
            created: string(&record, "created").parse().unwrap_or(1),
            launch_id: string(&record, "launch_id").to_owned(),
        }],
    };
    let mut session = listed_rows(&listing).remove(0);
    label_row(&mut session, &record);
    session
        .as_object_mut()
        .ok_or("invalid session")?
        .extend(telemetry::unavailable().as_object().unwrap().clone());
    let marks = read_marks::load();
    let preferences = notifications::load();
    let keep_warm_settings = keep_warm::load();
    let question_routing = question_routing::load();
    let route_outcomes = question_route_outcomes::Reader::load();
    let shared = Shared {
        marks: &marks,
        preferences: preferences.as_ref(),
        keep_warm: keep_warm_settings.as_ref(),
        question_routing: question_routing.as_ref(),
        route_outcomes: route_outcomes.as_ref(),
        budget: read_marks::question_budget(),
    };
    let mut process_work = processes::WorkSample::default();
    live_row(&mut session, record, &shared, &mut process_work)?;
    let mut rows = vec![session];
    finish_rows(&mut rows, &marks, &BTreeMap::new());
    Ok(rows.remove(0))
}

/// The listed row of a DeepSeek binding `record` as its host's `list`
/// answer `list` shows it, through the same steps as `native_rows`.
pub(crate) fn listed_deepseek_row(record: &Value, list: &Value) -> Value {
    let mut rows = vec![dsh::listed_for_fixture(record, list)];
    finish_rows(&mut rows, &read_marks::load(), &BTreeMap::new());
    rows.remove(0)
}

// Session records as the supervisor writes them, for the readers of records
// (zerus-63rru.53). Every record starts from the supervisor's own first build
// of it (`lifecycle::started_record`), with this test process standing in for
// its pane and processes; the record writer, renames, archives and the
// launch's evidence are the product's own functions. The state root is the
// process's own (`HGS_STATE_DIR`): callers run in a child with a private root.

/// A tracked launch's record before it is saved: what the supervisor builds
/// for `--launch-id launch` with the allocated `session` and `run`.
pub(crate) fn launched_record(
    agent: &str,
    name: &str,
    launch: &str,
    session: &str,
    run: &str,
    cwd: &str,
    account: &str,
) -> Value {
    let base = [agent.to_owned()];
    let mut record = lifecycle::started_record(lifecycle::Started {
        name,
        agent,
        run,
        pane: "%1",
        cwd: Path::new(cwd),
        base: &base,
        hgs: "/fixture/bin/hgs".into(),
        shell: "/bin/sh".into(),
        startup_kind: "new",
        launch: (!launch.is_empty()).then(|| launch.to_owned()),
        account: Some(account.to_owned()),
        home: None,
        session: Some(session),
    });
    record["terminal_name"] = json!(name);
    record
}

/// `record` confirmed with its conversation as the agent's main SessionStart
/// hook confirms it: the assignments of `hooks::track` (which itself needs a
/// live terminal pane) and its activity update, `journal::update_activity`.
pub(crate) fn confirmed(record: &mut Value, conversation: &str) {
    let object = record.as_object_mut().unwrap();
    for key in ["expected_id", "session_end", "pending_task_updates"] {
        object.remove(key);
    }
    record["conversation_id"] = json!(conversation);
    record["conversation_state"] = json!("active");
    record["activity"] = json!("idle");
    record["phase"] = json!("idle");
    record["main_done"] = json!(true);
    let event =
        json!({"hook_event_name":"SessionStart","session_id":conversation,"source":"startup"});
    journal::update_activity(record, &event);
}

/// The record a resume of `previous` writes for its new run `run`: the
/// supervisor's build for a resume (no launch is declared and no session is
/// allocated) with what a resume carries.
pub(crate) fn resumed_record(previous: &Value, run: &str) -> Value {
    let base = [string(previous, "agent").to_owned()];
    let mut record = lifecycle::started_record(lifecycle::Started {
        name: string(previous, "name"),
        agent: string(previous, "agent"),
        run,
        pane: "%2",
        cwd: Path::new(string(previous, "cwd")),
        base: &base,
        hgs: "/fixture/bin/hgs".into(),
        shell: "/bin/sh".into(),
        startup_kind: "resume",
        launch: None,
        account: Some(string(previous, "account_id").to_owned()),
        home: None,
        session: None,
    });
    for key in ["base", "created", "launch_dir"] {
        record[key] = previous[key].clone();
    }
    let expected = string(previous, "conversation_id").to_owned();
    record["expected_id"] = json!(expected);
    record["conversation_id"] = json!(expected);
    record["transcript"] = previous["transcript"].clone();
    lifecycle::carry_resumed(previous, &mut record);
    if let Some(anchor) = previous.get("journal_name") {
        record["journal_name"] = anchor.clone();
    }
    record["terminal_name"] = json!(string(previous, "name"));
    record
}

/// The record `hgs resume --fresh` writes over `previous` for its new run.
pub(crate) fn started_fresh_record(previous: &Value, run: &str) -> Value {
    let base = [string(previous, "agent").to_owned()];
    let mut record = lifecycle::started_record(lifecycle::Started {
        name: string(previous, "name"),
        agent: string(previous, "agent"),
        run,
        pane: "%3",
        cwd: Path::new(string(previous, "cwd")),
        base: &base,
        hgs: "/fixture/bin/hgs".into(),
        shell: "/bin/sh".into(),
        startup_kind: "new",
        launch: None,
        account: Some(string(previous, "account_id").to_owned()),
        home: None,
        session: None,
    });
    clear_context::carry_fresh_start(previous, &mut record);
    record["terminal_name"] = json!(string(previous, "name"));
    record
}

/// Saves `record` with the record writer, which also notes the ID index.
pub(crate) fn save(record: &mut Value) -> Result<()> {
    let _guard = lock(None)?;
    storage::write(record)
}

/// Reads the record back through the production label/ID routing.
pub(crate) fn saved_record(name: &str) -> Result<Value> {
    storage::read(name)
}

/// The accepted hook's main activity update and normal record writer, with
/// a clock supplied by the fixture that admitted the hand-out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn record_activity_hook(name: &str, event: &Value, at: f64) -> Result<Value> {
    let _guard = lock(None)?;
    let mut record = storage::read(name)?;
    journal::update_activity_at(&mut record, event, at);
    storage::write(&mut record)?;
    Ok(record)
}

/// Temporarily withhold the saved source without changing its bytes or the
/// index. Tests use a private root and restore the same source to recover.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn withhold_saved_record(name: &str, withheld: bool) -> Result<()> {
    let path = storage::label_record_path(name);
    let held = path.with_extension("withheld");
    let (from, to) = if withheld { (path, held) } else { (held, path) };
    std::fs::rename(from, to).map_err(|error| error.to_string())
}

/// `hgs rename from to`.
pub(crate) fn rename(from: &str, to: &str) -> Result<()> {
    rename::dispatch(&[from.to_owned(), to.to_owned()], false).map(|_| ())
}

/// Archives `record`, as `hgs archive` does.
pub(crate) fn archive(record: &Value) -> Result<String> {
    let _guard = lock(None)?;
    archive::save(record)
}

/// Let native save publish the archive while its incremental catalog
/// transaction rolls back, leaving the existing SQLite cache in place.
pub(crate) fn archive_with_catalog_gap(record: &Value) -> Result<String> {
    if archive_catalog::worker()? != 0 {
        return Err("The fixture could not initialize the archive catalog".into());
    }
    let db = rusqlite::Connection::open(root().join("archive-catalog-v1.sqlite3"))
        .map_err(|e| e.to_string())?;
    db.execute_batch(
        "CREATE TRIGGER fixture_catalog_rollback BEFORE INSERT ON archive_rows
         BEGIN SELECT RAISE(ABORT, 'fixture catalog publication rollback'); END;",
    ).map_err(|e| e.to_string())?;
    let result = archive(record);
    db.execute_batch("DROP TRIGGER fixture_catalog_rollback;").map_err(|e| e.to_string())?;
    result
}

/// The launch's evidence as a detached tracked launch writes it, which first
/// prunes evidence older than a week.
pub(crate) fn launch_started(launch: &str, name: &str, session: &str) -> Result<()> {
    launches::started_with_session(launch, name, session)
}

/// The end of run `run` of `name`, started for `launch`, as its supervisor
/// reports it: once the run confirmed its conversation, the evidence goes.
pub(crate) fn launch_finished(launch: &str, name: &str, run: &str) {
    std::env::set_var("HGS_LAUNCH_ID", launch);
    std::env::set_var("HGS_SESSION", name);
    std::env::set_var("HGS_RUN_ID", run);
    launches::finished(std::time::Duration::from_secs(600), Ok(0));
    for key in ["HGS_LAUNCH_ID", "HGS_SESSION", "HGS_RUN_ID"] {
        std::env::remove_var(key);
    }
}

/// Makes `launch`'s evidence `days` old, as if written that long ago.
pub(crate) fn age_launch(launch: &str, days: u64) {
    let path = launches::folder_in(&storage::root()).join(format!("{launch}.json"));
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400))
        .unwrap();
}

/// Complete this private fixture's native question/receipt index before an
/// observation control that requires known questions. Ordinary inspections
/// keep their bounded cold-index behavior.
pub(crate) fn complete_questions(record: &Value) -> Result<()> {
    codex_questions::current(record).map(|_| ())
}
