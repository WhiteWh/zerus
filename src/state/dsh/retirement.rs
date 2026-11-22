//! Archive publication and restore for DeepSeek's native per-session handle.
use super::*;
use std::time::{Duration, Instant};

pub(in crate::state) const RESTORE_PENDING: &str = "DeepSeek restore is awaiting native confirmation. Use Restore on the archive to check the original outcome";
// The caller already holds the exact native identity. Release the state
// writer while waiting for the provider, since some established provider
// paths need the writer before they can release their own binding lock.
fn write_locks() -> Result<(Guard, Guard)> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let _writer = storage::writer_until(until)?.ok_or("DeepSeek state writer is busy")?;
        if let Some(bindings) =
            storage::lock_until(&root().join("dsh/bindings.lock"), Instant::now())?
        {
            return Ok((_writer, bindings));
        }
        drop(_writer);
        if Instant::now() >= until {
            return Err("DeepSeek bindings are busy; the saved records were kept".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn publish<T>(write: impl FnOnce() -> Result<T>) -> Result<T> {
    let _locks = write_locks()?;
    write()
}

pub(in crate::state) fn restore_name_busy(name: &str) -> Result<bool> {
    Ok(!PROVIDER.vacant(name)? || storage::label_taken(name))
}

pub(in crate::state) fn restore_source_held(record: &Value) -> Result<bool> {
    let name = string(record, "name");
    if PROVIDER.vacant(name)? {
        return Ok(false);
    }
    let current = binding(name)?;
    Ok(current["restore_archive_id"] == record["archive_id"]
        && current["session_id"] == record["session_id"]
        && current["conversation_id"] == record["conversation_id"])
}

pub(in crate::state) fn pending(record: &Value) -> bool {
    let id = string(record, "restore_archive_id");
    !id.is_empty()
        && (!string(record, "expected_id").is_empty()
            || !matches!(native_archives::contains(id), Ok(false)))
}

fn view(record: &Value) -> Result<Value> {
    let inspected = supported_lifecycle_view(record)?;
    if inspected["live"].as_bool().is_none() {
        return Err("DeepSeek did not confirm whether this native session is stopped".into());
    }
    Ok(inspected)
}

fn offers(view: &Value, action: &str) -> bool {
    view["lifecycleActions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|value| value == action))
}

pub(super) fn dispatch(
    command: &str,
    args: &[String],
    scope: Option<&session_action::Scope>,
) -> Result<i32> {
    let dry = args.iter().any(|arg| arg == "--dry-run");
    let args: Vec<_> = args.iter().filter(|arg| *arg != "--dry-run").collect();
    let (name, expected_run) = match args.as_slice() {
        [name] => (name.as_str(), None),
        [name, flag, run]
            if command == "terminate"
                && flag.as_str() == "--expected-run-id"
                && !run.is_empty() =>
        {
            (name.as_str(), Some(run.as_str()))
        }
        _ => return Err(format!("usage: hgs {command} <session>")),
    };
    let (_identity, mut record) = dispatch_lock::acquire(name)?;
    terminating::check_mutation(&record)?;
    if let Some(scope) = scope {
        scope.check(&record)?;
    }
    if expected_run.is_some_and(|run| run != string(&record, "run_id")) {
        return Err("DeepSeek session changed before termination; refresh and retry".into());
    }
    if dry && pending(&record) {
        return Err(RESTORE_PENDING.into());
    }
    ready(&mut record)?;

    let mut inspected = view(&record)?;
    let running = inspected["live"] == true;
    if command == "archive" && running {
        return Err(format!(
            "{name}: only stopped or paused sessions can be archived"
        ));
    }
    if running && !offers(&inspected, "forget") {
        return Err("This agent is owned by the native UI; stop it there before archiving".into());
    }
    if dry {
        println!("hgs: would {command} {name}");
        return Ok(0);
    }
    if running {
        // The resident scoped API's 'forget' disposes its exact owned handle.
        // Forgetting the Zerus binding is a separate local operation.
        native_lifecycle_action(&record, "forget", &inspected, json!({}), scope).map_err(
            |error| {
                if error.definite {
                    if let Some(scope) = scope {
                        scope.unchanged();
                    }
                }
                error.message
            },
        )?;
        let stopped = view(&record)?;
        if stopped["generation"] != inspected["generation"] || stopped["live"] != false {
            return Err("DeepSeek shutdown was not confirmed; the saved binding was kept".into());
        }
        inspected = stopped;
    }

    let mut completed = record.clone();
    clear_restore_fields(&mut completed);
    completed["version"] = json!(1);
    completed["native_archive_through_seq"] = inspected["baseline"]["asOfSeq"].clone();
    completed["completion_source"] = json!(if command == "terminate" {
        "terminate"
    } else {
        "manual_archive"
    });
    completed["completion_reason"] = json!(if command == "terminate" {
        "user_terminated"
    } else {
        "user_archived"
    });
    let id = {
        let (_writer, bindings) = write_locks()?;
        if binding(name)? != record {
            return Err(
                "DeepSeek binding changed before archiving; the saved binding was kept".into(),
            );
        }
        if let Some(scope) = scope {
            scope.changing();
        }
        let id = native_archives::publish(&completed)?;
        remove(&record)?;
        drop(bindings);
        worktree_lock::left(&record);
        id
    };
    if let Some(scope) = scope {
        scope.result(
            name,
            string(&record, "run_id"),
            string(&record, "conversation_id"),
            Some(&id),
        );
    } else {
        println!("hgs: archived {name} ({id})");
    }
    Ok(0)
}

pub(in crate::state) fn restore(
    archived: &Value,
    dry: bool,
    scope: Option<&session_action::Scope>,
) -> Result<i32> {
    if let Some(scope) = scope {
        scope.check(archived)?;
    }
    let name = string(archived, "name");
    let id = string(archived, "archive_id");
    let _identity = dispatch_lock::lock_record(archived)?;
    let check_source = || -> Result<()> {
        if archive::read_archive(name, id)? != *archived {
            return Err("DeepSeek archive changed before restore; refresh and retry".into());
        }
        Ok(())
    };

    // A proved native resume may still need local cleanup after a write
    // failure. An unconfirmed original never sends a second resume.
    let existing = publish(|| {
        check_source()?;
        if !restore_name_busy(name)? {
            return Ok(None);
        }
        let record = binding(name)?;
        if record["restore_archive_id"] != id
            || record["session_id"] != archived["session_id"]
            || record["conversation_id"] != archived["conversation_id"]
        {
            return Err(format!(
                "{name}: a live or saved session already uses this name"
            ));
        }
        Ok(Some(record))
    })?;
    if let Some(mut record) = existing {
        if dry {
            return Err(RESTORE_PENDING.into());
        }
        let outcome = outcome(&record)?.ok_or(RESTORE_PENDING)?;
        if let Some(scope) = scope {
            scope.changing();
        }
        settle(&mut record, outcome)?;
        if let Some(reason) = outcome.refusal() {
            if let Some(scope) = scope {
                scope.unchanged();
            }
            return Err(reason.into());
        }
        restored(scope, &record, id);
        return Ok(0);
    }

    if !Path::new(string(archived, "cwd")).is_dir() && !worktree_lifecycle::restorable(archived) {
        return Err("DeepSeek restore requires its original session folder".into());
    }
    let inspected = view(archived)?;
    if inspected["live"] != false || !offers(&inspected, "resume") {
        return Err("The archived DeepSeek conversation is not available to resume".into());
    }
    let plan = permission_plan(archived)?;
    if plan.generation != inspected["generation"] || plan.needs_setup {
        return Err(
            "The resident DeepSeek host cannot confirm the archive's account permissions".into(),
        );
    }
    if dry {
        println!("hgs: would restore {name} from archive {id}");
        return Ok(0);
    }

    worktree_lifecycle::restore(archived)?;
    let mut record = archived.clone();
    capture_worktree(&mut record, Path::new(string(archived, "cwd")));
    for key in [
        "archive_id",
        "archived_at",
        "state",
        "completion_source",
        "completion_reason",
        "exit_code",
        "termination_signal",
        "exited_at",
        "session_end",
        "last_restore_error",
        "native_archive_through_seq",
        "worktree_cleanup",
        "permissions_pending",
        "layout",
    ] {
        record.as_object_mut().unwrap().remove(key);
    }
    record["run_id"] = json!(uuid::Uuid::new_v4().to_string());
    record["restore_archive_id"] = json!(id);
    record["restore_host_generation"] = inspected["generation"].clone();
    record["expected_id"] = archived["conversation_id"].clone();
    record["paused"] = json!(true);
    publish(|| {
        check_source()?;
        if restore_name_busy(name)? {
            return Err(format!(
                "{name}: a live or saved session already uses this name"
            ));
        }
        if let Some(scope) = scope {
            scope.changing();
        }
        save(&mut record)
    })?;

    match native_lifecycle_action(
        &record,
        "resume",
        &inspected,
        json!({"permissionMode":plan.mode,"requestId":record["run_id"]}),
        scope,
    ) {
        Ok(()) => {
            remember(&record, Outcome::Resumed)?;
            settle(&mut record, Outcome::Resumed)?;
            restored(scope, &record, id);
            Ok(0)
        }
        Err(error) => {
            if !error.definite {
                return Err(RESTORE_PENDING.into());
            }
            remember(&record, Outcome::Refused)?;
            settle(&mut record, Outcome::Refused)?;
            if let Some(scope) = scope {
                scope.unchanged();
            }
            Err(error.message)
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Outcome {
    Resumed,
    Refused,
    HostChanged,
}

impl Outcome {
    fn tag(self) -> &'static str {
        match self {
            Self::Resumed => "resumed",
            Self::Refused => "refused",
            Self::HostChanged => "host_changed",
        }
    }

    fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Resumed => None,
            Self::Refused => Some(
                "The earlier DeepSeek restore did not start. The archive is available for a new Restore",
            ),
            Self::HostChanged => Some(
                "The DeepSeek host restarted before the earlier restore could be confirmed. The archive is available for a new Restore",
            ),
        }
    }
}

fn proof_path(record: &Value) -> Result<PathBuf> {
    let run = string(record, "run_id");
    if !session_id::canonical(run) {
        return Err("Invalid DeepSeek restore run".into());
    }
    Ok(root()
        .join("dsh/restore-outcomes")
        .join(format!("{run}.json")))
}

fn proof(record: &Value, outcome: Outcome) -> Value {
    json!({"archive_id":record["restore_archive_id"], "run_id":record["run_id"],
         "session_id":record["session_id"], "conversation_id":record["conversation_id"],
         "generation":record["restore_host_generation"],
         "status":outcome.tag()})
}

// Keep an exact acknowledgement even when the provider lock is busy for
// longer than our writer wait. The per-conversation guard covers this file.
fn remember(record: &Value, outcome: Outcome) -> Result<()> {
    atomic(&proof_path(record)?, &proof(record, outcome).to_string())
}

fn outcome(record: &Value) -> Result<Option<Outcome>> {
    if string(record, "expected_id").is_empty() {
        return Ok(Some(Outcome::Resumed));
    }
    if let Some(raw) =
        labels::read_file(&proof_path(record)?, 4096).map_err(|error| error.to_string())?
    {
        let saved: Value = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
        for result in [Outcome::Resumed, Outcome::Refused, Outcome::HostChanged] {
            if saved == proof(record, result) {
                return Ok(Some(result));
            }
        }
        return Err("DeepSeek restore acknowledgement identity changed".into());
    }
    let generation = string(record, "restore_host_generation");
    if generation.is_empty() {
        return Ok(None);
    }
    let result = observe_outcome(record, generation);
    if let Some(result) = result {
        remember(record, result)?;
    }
    Ok(result)
}

fn changed_generation(value: &Value, original: &str) -> bool {
    let observed = string(value, "generation");
    observed != original && session_id::canonical(observed)
}

fn observe_outcome(record: &Value, generation: &str) -> Option<Outcome> {
    let request = json!({"sessionId":record["conversation_id"], "generation":generation,
                        "requestId":record["run_id"]});
    if let Ok(observed) = rpc_within(
        record,
        "session-resume-status",
        request,
        Duration::from_secs(2),
    ) {
        if observed["sessionId"] == record["conversation_id"]
            && observed["requestId"] == record["run_id"]
        {
            if changed_generation(&observed, generation) {
                return Some(Outcome::HostChanged);
            }
            if observed["generation"] == generation {
                match observed["status"].as_str() {
                    Some("failed") => return Some(Outcome::Refused),
                    Some("completed") => return Some(Outcome::Resumed),
                    _ => {}
                }
            }
        }
    }
    // An older resident adapter has no receipt read. Its exact owned live
    // handle in the original generation is still positive proof. Only a
    // different generation fences an original the older adapter cannot name.
    if let Ok(view) = rpc_within(
        record,
        "inspect",
        json!({"sessionId":record["conversation_id"],"includeProcesses":false}),
        Duration::from_secs(2),
    ) {
        if changed_generation(&view, generation) {
            return Some(Outcome::HostChanged);
        }
        if view["generation"] == generation
            && view["live"] == true
            && view["owned"] == true
            && offers(&view, "forget")
        {
            return Some(Outcome::Resumed);
        }
        return None;
    }
    let ping = rpc_within(record, "ping", json!({}), Duration::from_secs(2)).ok()?;
    (ping["sessionActions"] == true && changed_generation(&ping, generation))
        .then_some(Outcome::HostChanged)
}

fn clear_restore_fields(record: &mut Value) {
    let fields = record.as_object_mut().unwrap();
    fields.remove("restore_archive_id");
    fields.remove("restore_host_generation");
}

fn settle(record: &mut Value, result: Outcome) -> Result<()> {
    let name = string(record, "name").to_owned();
    let id = string(record, "restore_archive_id").to_owned();
    publish(|| {
        if binding(&name)? != *record {
            return Err("DeepSeek restore binding changed; the archive was kept".into());
        }
        let source = native_archives::by_id(&id)?;
        if source.as_ref().is_some_and(|source| {
            source["name"] != name
                || source["conversation_id"] != record["conversation_id"]
                || source["session_id"] != record["session_id"]
        }) {
            return Err("DeepSeek restore source identity changed".into());
        }
        if source.is_none() && !string(record, "expected_id").is_empty() {
            return Err("DeepSeek restore source is unavailable".into());
        }
        if result != Outcome::Resumed {
            remove(record)?;
        } else {
            record.as_object_mut().unwrap().remove("expected_id");
            record["paused"] = json!(false);
            save(record)?;
            if let Some(source) = source {
                native_archives::remove(&source)?;
            }
            clear_restore_fields(record);
            save(record)?;
        }
        let _ = fs::remove_file(proof_path(record)?);
        Ok(())
    })
}

pub(super) fn ready(record: &mut Value) -> Result<bool> {
    if !pending(record) {
        return Ok(false);
    }
    let result = outcome(record)?.ok_or(RESTORE_PENDING)?;
    settle(record, result)?;
    if let Some(reason) = result.refusal() {
        return Err(reason.into());
    }
    Ok(true)
}

fn restored(scope: Option<&session_action::Scope>, record: &Value, archive_id: &str) {
    if let Some(scope) = scope {
        scope.result_record(record);
    } else {
        println!(
            "hgs: restored {} from archive {archive_id}",
            string(record, "name")
        );
    }
}

/// A browse-only archive read. An unavailable host leaves the saved identity
/// and the ordinary Restore decision available, with an explicit history error.
pub(in crate::state) fn inspect(record: &Value, after: i64) -> Result<Value> {
    let name = string(record, "name");
    let mut detail = journal::summary(record, false);
    detail["name"] = record["name"].clone();
    detail["agent"] = json!("dsh");
    detail["cmd"] = json!("dsh");
    detail["backend"] = json!("dsh");
    detail["state"] = json!("archived");
    detail["archive_id"] = record["archive_id"].clone();
    detail["archived_at"] = record["archived_at"].clone();
    detail["completion_source"] = record["completion_source"].clone();
    detail["completion_reason"] = record["completion_reason"].clone();
    let held = restore_source_held(record)?;
    detail["restore_name_busy"] = json!(restore_name_busy(name)? && !held);
    detail["terminal_reason"] = json!(backend::DEEPSEEK_TERMINAL.text());
    detail["pending_questions"] = json!([]);
    match rpc(
        record,
        "inspect",
        json!({"sessionId":record["conversation_id"],"includeProcesses":false}),
    ) {
        Ok(native) => {
            let mut events = native_events(&native["page"]["records"]);
            if let Some(through) = record["native_archive_through_seq"].as_i64() {
                // native_events adds one to the upstream sequence.
                events.retain(|event| event["seq"].as_i64().is_some_and(|seq| seq - 1 <= through));
            }
            let last = events
                .last()
                .and_then(|event| event["seq"].as_i64())
                .unwrap_or(after);
            events.retain(|event| event["seq"].as_i64().is_some_and(|seq| seq > after));
            detail["events"] = json!(events);
            detail["cursor"] = json!(last.max(after));
            detail["history_truncated"] = native["page"]["hasMore"].clone();
        }
        Err(error) => {
            detail["events"] = json!([]);
            detail["cursor"] = json!(after);
            detail["provider_messages_error"] = json!(error);
        }
    }
    Ok(detail)
}
