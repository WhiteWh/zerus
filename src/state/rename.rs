//! Rename mutable labels without changing run identity or restarting the agent.
use super::*;
use std::fs;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod claim;
pub(super) fn claim_ready(root: &Path, record: &Value, name: &str) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    { claim::ready(root, record, name) }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    { let _ = (root, record, name); Ok(()) }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod guarded;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use guarded::{
    mirror_legacy as mirror_legacy_guarded, recover_locked as recover_guarded,
    sync_legacy_locked as sync_legacy_guarded,
};

fn remove(path: &Path) -> Result<()> {
    super::listing_metadata::remove_at(&root(), path, || remove_inner(path))
}

fn remove_inner(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => crate::platform::sync_dir(path.parent().ok_or("invalid state path")?)
            .map_err(|e| e.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn load(path: &Path) -> Result<Value> {
    if let Some(bytes) = storage::listing_scope::read(path) {
        return serde_json::from_slice(&bytes.map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string());
    }
    serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn json_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if let Some(paths) = storage::listing_scope::paths(directory) {
        return paths;
    }
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<_> = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    Ok(paths)
}

fn validate(old: &str, new: &str) -> Result<()> {
    let a: Vec<_> = old.split('/').collect();
    let b: Vec<_> = new.split('/').collect();
    let valid = |parts: &[&str]| {
        (2..=3).contains(&parts.len())
            && parts.iter().all(|part| {
                !part.is_empty()
                    && !part.contains([':', '.'])
                    && part.trim() == *part
                    && !part.chars().any(char::is_control)
            })
    };
    if !valid(&a) || !valid(&b) || b.len() != 3 || a[..2] != b[..2] {
        return Err("rename must keep the same agent/project and use a nonempty tag without '/', ':', '.', control characters or surrounding whitespace".into());
    }
    Ok(())
}

// New requests only: recovery above/below keeps the recorded-name shape.
pub(super) fn validate_new(old: &str, new: &str) -> Result<()> {
    validate(old, new)?;
    validate_new_tag(new)
}

// Provider adapters keep their existing selector/prefix checks. Only the
// proposed tag gets the new policy; never revalidate a legacy source label.
pub(super) fn validate_new_tag(new: &str) -> Result<()> {
    if let Some(problem) = zerus_client_core::actions::tag_problem(new.rsplit('/').next().unwrap()) {
        return Err(problem.into());
    }
    Ok(())
}

fn session_id(name: &str) -> Result<String> {
    let id = backend::current()
        .pane_info(
            backend::Target::Name(name),
            &[backend::PaneField::SessionId],
        )?
        .value()
        .to_owned();
    if !id
        .strip_prefix('$')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(backend::INVALID_SESSION_ID.text().into());
    }
    Ok(id)
}

fn current_session_name(id: &str) -> Result<Option<String>> {
    backend::current().session_name(id)
}

pub(super) fn dispatch(args: &[String], dry: bool) -> Result<i32> { dispatch_scoped(args,dry,None) }

pub(super) fn dispatch_scoped(args: &[String], dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    let archive_id = match args {
        [_, _] => None,
        [_, _, flag, id] if flag == "--archive" => Some(id.as_str()),
        _ => return Err("usage: hgs rename <session> <new-name> [--archive ID]".into()),
    };
    let old = &args[0];
    let new = &args[1];
    validate_new(old, new)?;
    let _guard = lock(None)?;
    let snapshot = live()?;
    if archive_id.is_none()
        && old != new
        && (snapshot.contains_key(new) || storage::label_taken(new))
    {
        return Err(format!(
            "{new}: a live or saved session already uses this name"
        ));
    }
    if let Some(id) = archive_id {
        let mut record = archive::read_archive(old, id)?;
        terminating::check_mutation(&record)?;
        if let Some(scope)=scope {scope.check(&record)?;}
        if record["backend"] == "dsh" && dsh::retirement::restore_source_held(&record)? {
            return Err(dsh::retirement::RESTORE_PENDING.into());
        }
        if old != new && !dry {
            anchor(&mut record);
            record["name"] = json!(new);
            record["renamed_at"] = json!(now());
            for key in ["rename_shadow", "rename_target", "legacy_shadow"] {
                record.as_object_mut().unwrap().remove(key);
            }
            if let Some(scope)=scope {scope.changing();}
            if record["backend"] == "dsh" {
                dsh::native_archives::update_name(&record)?;
            } else {
                atomic_record(&archive::path(id)?, &mut record)?;
            }
        }
        if let Some(scope)=scope {scope.result(new,string(&record,"run_id"),string(&record,"conversation_id"),Some(id));}
    } else {
        let record = if record_path(old).exists() {
            Some(read(old)?)
        } else {
            None
        };
        if let Some(scope)=scope {scope.check(record.as_ref().ok_or("Scoped rename requires a tracked binding")?)?;}
        // The session's own terminal (P3b); an untracked one by its name.
        let terminal = record.as_ref().map_or(old.as_str(), terminal_name).to_owned();
        let live_panes = snapshot.get(&terminal);
        if record.is_none() && live_panes.is_none() {
            return Err(format!("{old}: session not found"));
        }
        if let Some(record) = &record {
            terminating::check_mutation(record)?;
            if live_panes.is_some() && !matches(record, live_panes) {
                return Err(format!("{old}: live pane does not match its saved binding"));
            }
            if record.get("pausing").is_some() || record["resume_pending"] == true {
                return Err(format!(
                    "{old}: wait for the current pause or restore to finish before renaming"
                ));
            }
        }
        if old != new && !dry {
            let id = if live_panes.is_some() {
                Some(session_id(&terminal)?)
            } else {
                None
            };
            // A session in the ID layout is renamed in place (P3a): no route
            // and no shadow at a label's path.
            let in_place = record.as_ref().is_some_and(storage::in_id_layout);
            let legacy = !in_place && record.as_ref().is_some_and(|record| {
                record["run_identity_version"] != 1
                    && record["legacy_shadow"].is_null()
                    && record["supervisor"].is_object()
                    && !attempts::identities_gone(record)
            });
            let mut record = record;
            if let Some(record) = &mut record { processes::compact(record); }
            // The terminal moves to the new label by its `$N`, under the
            // writer lock (P3b); recovery compares its name with `terminal`.
            let mut transaction = json!({"version":1,"old":old,"new":new,"record":record,
                "session_id":id,"terminal":terminal,"legacy":legacy,
                "source_routed":!in_place && route_path(old).exists()});
            if in_place {
                transaction["layout"] = json!("id");
            }
            let path = root()
                .join("renames")
                .join(format!("{}.json", uuid::Uuid::new_v4()));
            if let Some(scope)=scope {scope.changing();}
            atomic(&path, &format!("{transaction}\n"))?;
            if let Some(id) = id {
                if let Err(error) = backend::current().rename(&id, new) {
                    remove(&path)?;
                    return Err(error);
                }
            }
            commit(&transaction)?;
            remove(&path)?;
        }
    }
    if let Some(scope)=scope {
        if archive_id.is_none() {scope.result_record(&renamed(new)?);}
    } else {println!(
        "hgs: {}renamed {old} to {new}",
        if dry { "would have " } else { "" }
    );}
    Ok(0)
}

/// A rename that moved the session's terminal gives the record its new
/// terminal name (P3b). One that moved none, of a stopped or paused session,
/// clears it, so the session's terminal is its new label until a launch names
/// one: an old name another session may later take never stays (#1445
/// review).
fn moved_terminal(transaction: &Value, record: &mut Value) {
    if transaction["session_id"].is_string() {
        record["terminal_name"] = transaction["new"].clone();
    } else if let Some(fields) = record.as_object_mut() {
        fields.remove("terminal_name");
    }
}

fn anchor(record: &mut Value) {
    if string(record, "journal_name").is_empty() {
        record["journal_name"] = record["name"].clone();
    }
    // A record an older hgs rebuilt without it keeps the label it leaves.
    if string(record, "launch_label").is_empty() {
        record["launch_label"] = record["name"].clone();
    }
}

fn source_path(transaction: &Value) -> PathBuf {
    let legacy = legacy_record_path(string(transaction, "old"));
    if transaction["source_routed"] == true {
        root().join("bindings").join(legacy.file_name().unwrap())
    } else {
        legacy
    }
}

/// The record a rename just gave `new`: the selected session's, when the
/// command selected one in the ID layout, else the label's.
fn renamed(new: &str) -> Result<Value> {
    match session_id::selected().and_then(|selection| storage::id_record_path(&selection.session_id)) {
        Some(path) if path.is_file() => storage::read_id(&session_id::selected().unwrap().session_id),
        _ => read(new),
    }
}

/// A rename of a session in the ID layout (P3a): its record stays in
/// `sessions/<id>.json`, and only that record and the label index change. The
/// new label's entry is durable before the record takes it, and the old
/// label's entry goes only after the record left it. Nothing is written at a
/// label's path, so the first holder of either label is never touched.
fn commit_in_place(transaction: &Value, old: &str, new: &str) -> Result<()> {
    let record = &transaction["record"];
    let id = string(record, "session_id").to_owned();
    let path = storage::id_record_path(&id).ok_or("rename record has no session ID")?;
    let mut current = storage::read_id(&id).or_else(|_| {
        // Recovery after the record already took the new label.
        load(&path)
    })?;
    if current["run_id"] != record["run_id"] || !storage::in_id_layout(&current) {
        return Err("rename source run changed; refusing to replace its binding".into());
    }
    if string(&current, "name") != new {
        labels::add(new, &id)?;
        // Its recovery job keeps its key, the session ID, and takes the label.
        recovery::rename_job_in_place(&current, new)?;
        anchor(&mut current);
        current["name"] = json!(new);
        current["renamed_at"] = json!(now());
        moved_terminal(transaction, &mut current);
        write(&mut current)?;
    }
    labels::remove(old, &id)?;
    for path in json_files(&root().join("archives"))? {
        let mut archived = load(&path)?;
        if archived["run_id"] == current["run_id"] && string(&archived, "name") == old {
            anchor(&mut archived);
            archived["name"] = json!(new);
            atomic_record(&path, &mut archived)?;
        }
    }
    Ok(())
}

fn commit(transaction: &Value) -> Result<()> {
    let old = string(transaction, "old");
    let new = string(transaction, "new");
    validate(old, new)?;
    if transaction["layout"] == "id" && transaction["record"].is_object() {
        return commit_in_place(transaction, old, new);
    }
    let Some(mut record) = transaction["record"]
        .as_object()
        .map(|_| transaction["record"].clone())
    else {
        return Ok(());
    };
    let source = source_path(transaction);
    if source.exists() {
        let current = load(&source)?;
        if current["run_id"] != record["run_id"] {
            return Err("rename source run changed; refusing to replace its binding".into());
        }
        if current["rename_shadow"] != true {
            record = current;
        }
    }
    if record_path(new).exists() {
        let destination = read(new)?;
        if destination["run_id"] != record["run_id"] {
            return Err("rename destination was reused; refusing to replace its binding".into());
        }
    }
    anchor(&mut record);
    record["name"] = json!(new);
    record["renamed_at"] = json!(now());
    moved_terminal(transaction, &mut record);
    if transaction["legacy"] == true {
        record["legacy_shadow"] = json!({"name":old,"run_id":record["run_id"]});
    }
    // Publish the canonical copy before retiring the old binding. Hooks share
    // this lock, and recovery completes an interrupted transaction first.
    recovery::rename_job(old,new,string(&record,"run_id"))?;
    keep_warm::rename(old, new)?;
    write(&mut record)?;
    if transaction["legacy"] == true {
        atomic(
            &route_path(old),
            &format!(
                "{}\n",
                json!({"version":1,"name":old,"run_id":record["run_id"]})
            ),
        )?;
        let mut shadow = record.clone();
        shadow["name"] = json!(old);
        shadow["rename_target"] = json!(new);
        shadow["rename_shadow"] = json!(true);
        atomic_record(&legacy_record_path(old), &mut shadow)?;
    } else {
        remove(&source)?;
    }
    // A legacy supervisor can finish between the process crash and recovery.
    // Preserve its archive under the requested final label as well.
    for path in json_files(&root().join("archives"))? {
        let mut archived = load(&path)?;
        if archived["run_id"] == record["run_id"] && string(&archived, "name") == old {
            anchor(&mut archived);
            archived["name"] = json!(new);
            archived.as_object_mut().unwrap().remove("rename_shadow");
            archived.as_object_mut().unwrap().remove("rename_target");
            atomic_record(&path, &mut archived)?;
        }
    }
    Ok(())
}

/// Called immediately after obtaining the writer lock, never recursively.
pub(super) fn recover_locked() -> Result<()> {
    for path in json_files(&root().join("renames"))? {
        let transaction = load(&path)?;
        if transaction["version"] != 1 {
            return Err("invalid rename transaction".into());
        }
        let old = string(&transaction, "old");
        let new = string(&transaction, "new");
        validate(old, new)?;
        if let Some(id) = transaction["session_id"].as_str() {
            // The terminal's name before the move: its own (P3b), or for a
            // transaction an older hgs wrote, the old label.
            let from = match string(&transaction, "terminal") {
                "" => old,
                terminal => terminal,
            };
            if let Some(current) = current_session_name(id)? {
                if current == from {
                    // tmux rename never happened. All durable bindings still
                    // use the old name, so aborting is lossless.
                    remove(&path)?;
                    continue;
                }
                if current != new {
                    return Err(backend::RENAME_RECOVERY_CHANGED.text().into());
                }
            }
        }
        commit(&transaction)?;
        remove(&path)?;
    }
    Ok(())
}

pub(super) fn mirror_legacy(record: &Value) -> Result<()> {
    let legacy = &record["legacy_shadow"];
    let old = string(legacy, "name");
    if old.is_empty() || legacy["run_id"] != record["run_id"] {
        return Ok(());
    }
    let path = legacy_record_path(old);
    if !path.exists() {
        return Ok(());
    }
    let current = load(&path)?;
    if current["run_id"] != record["run_id"] {
        return Err("legacy rename shadow has a different run identity".into());
    }
    let mut shadow = record.clone();
    shadow["name"] = json!(old);
    shadow["rename_target"] = record["name"].clone();
    shadow["rename_shadow"] = json!(true);
    atomic_record(&path, &mut shadow)
}

pub(super) fn sync_legacy_locked() -> Result<()> {
    for route in json_files(&root().join("name_routes"))? {
        let route = load(&route)?;
        let old = string(&route, "name");
        let path = legacy_record_path(old);
        if !path.exists() {
            continue;
        }
        let shadow = load(&path)?;
        if shadow["rename_shadow"] != true || shadow["run_id"] != route["run_id"] {
            return Err("invalid legacy rename shadow identity".into());
        }
        let name = string(&shadow, "rename_target");
        if name.is_empty() || !record_path(name).exists() {
            continue;
        }
        let mut record = read(name)?;
        if record["run_id"] != shadow["run_id"] {
            continue;
        }
        if record["supervisor"] != shadow["supervisor"] {
            return Err("legacy supervisor completion identity changed".into());
        }
        let completed = shadow["exited_at"].as_f64().unwrap_or(0.0)
            > record["exited_at"].as_f64().unwrap_or(0.0);
        let spawned = shadow["updated"].as_f64().unwrap_or(0.0)
            > record["updated"].as_f64().unwrap_or(0.0)
            && (record["pid"] != shadow["pid"]
                || record["process_start"] != shadow["process_start"]);
        if completed || spawned {
            // A pre-upgrade runner may move from a failed initial resume to
            // its fresh fallback after rename. Only that immutable supervisor
            // and run can publish the next tracked child's PID/start pair.
            if shadow["pid"].as_u64().unwrap_or(0) == 0 {
                return Err("legacy supervisor published an invalid process identity".into());
            }
            record["pid"] = shadow["pid"].clone();
            record["process_start"] = shadow["process_start"].clone();
            if completed {
                for key in [
                    "exit_code",
                    "termination_signal",
                    "exited_at",
                    "completion_source",
                    "completion_reason",
                ] {
                    if let Some(value) = shadow.get(key) {
                        record[key] = value.clone();
                    }
                }
            }
            write(&mut record)?;
        }
    }
    Ok(())
}

pub(super) fn normalize_archive(record: &mut Value) -> Result<()> {
    if record["rename_shadow"] == true && !string(record, "rename_target").is_empty() {
        validate(string(record, "name"), string(record, "rename_target"))?;
        if record["legacy_shadow"]["run_id"] != record["run_id"]
            || record["legacy_shadow"]["name"] != record["name"]
        {
            return Err("invalid archived rename shadow identity".into());
        }
        record["name"] = record["rename_target"].clone();
        record.as_object_mut().unwrap().remove("rename_shadow");
        record.as_object_mut().unwrap().remove("rename_target");
    }
    Ok(())
}

pub(super) fn remove_legacy(record: &Value) -> Result<()> {
    let legacy = &record["legacy_shadow"];
    let old = string(legacy, "name");
    if old.is_empty() || legacy["run_id"] != record["run_id"] {
        return Ok(());
    }
    let path = legacy_record_path(old);
    if path.exists() && load(&path)?["run_id"] == record["run_id"] {
        remove(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_session_tags_reject_shell_syntax() {
        assert!(validate_new("claude/project/old$tag", "claude/project/safe").is_ok());
        assert!(validate("claude/project/old", "claude/project/old$tag").is_ok(), "recorded recovery stays compatible");
        for tag in ["bad$tag", "bad;tag", "bad\u{200b}tag", "bad\u{2019}tag"] {
            assert!(validate_new("claude/project/old", &format!("claude/project/{tag}")).is_err());
        }
    }
    /// A rename pins the history it leaves: the label as the journal of a
    /// record that had none, and the label it leaves as `launch_label` when an
    /// older hgs rebuilt the record without one. Existing values are kept.
    #[test]
    fn a_rename_anchors_the_journal_and_the_launch_label_once() {
        const JOURNAL: &str = "session:3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
        let mut old_style = json!({"name":"codex/p/one"});
        super::anchor(&mut old_style);
        assert_eq!(old_style["journal_name"], "codex/p/one");
        assert_eq!(old_style["launch_label"], "codex/p/one");
        let mut new_style =
            json!({"name":"codex/p/renamed","journal_name":JOURNAL,"launch_label":"codex/p/one"});
        super::anchor(&mut new_style);
        assert_eq!(new_style["journal_name"], JOURNAL);
        assert_eq!(new_style["launch_label"], "codex/p/one");
        let mut rebuilt = json!({"name":"codex/p/renamed","journal_name":JOURNAL});
        super::anchor(&mut rebuilt);
        assert_eq!(rebuilt["launch_label"], "codex/p/renamed");
    }

    use super::*;

    #[test]
    fn a_session_id_names_tmux_only_where_it_hosts_the_sessions() {
        struct Answers;
        impl backend::TerminalBackend for Answers {
            fn pane_info(
                &self,
                target: backend::Target,
                _fields: &[backend::PaneField],
            ) -> Result<backend::PaneInfo> {
                let backend::Target::Name(name) = target else {
                    unreachable!("{target:?}");
                };
                let id = if name.ends_with("work-1") { "$3" } else { "@1" };
                Ok(backend::PaneInfo::new(format!("{id}\n")))
            }
        }
        backend::with_fake(&Answers, || {
            assert_eq!(session_id("claude/MyProj/work-1").unwrap(), "$3");
            assert_eq!(
                session_id("claude/MyProj/work-2").unwrap_err(),
                backend::INVALID_SESSION_ID.text()
            );
        });
    }

    #[test]
    fn a_session_id_needs_digits_after_its_dollar_sign() {
        // As in `hgs a --existing` and the Terminal: `$N`, never a bare `$`.
        // The fake answers each name with that name as its session ID.
        struct Echo;
        impl backend::TerminalBackend for Echo {
            fn pane_info(
                &self,
                target: backend::Target,
                _fields: &[backend::PaneField],
            ) -> Result<backend::PaneInfo> {
                let backend::Target::Name(id) = target else {
                    unreachable!("{target:?}");
                };
                Ok(backend::PaneInfo::new(format!("{id}\n")))
            }
        }
        backend::with_fake(&Echo, || {
            assert_eq!(session_id("$0").unwrap(), "$0");
            assert_eq!(session_id("$12").unwrap(), "$12");
            for id in ["$", "", "12", "$1a", "$-1", "$$1", "%1"] {
                assert_eq!(
                    session_id(id).unwrap_err(),
                    backend::INVALID_SESSION_ID.text(),
                    "{id:?}"
                );
            }
        });
    }
}
