//! Immutable completion records, indexed independently of reusable tmux names.
use super::*;
use std::fs;

pub(super) fn path(id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_| "invalid archive ID")?;
    Ok(root().join("archives").join(format!("{id}.json")))
}

pub(super) fn read_archive(name: &str, id: &str) -> Result<Value> {
    let mut record: Value = serde_json::from_str(
        &fs::read_to_string(path(id)?)
            .map_err(|error| format!("{name}: cannot read archive {id}: {error}"))?,
    )
    .map_err(|error| error.to_string())?;
    rename::normalize_archive(&mut record)?;
    if !record.is_object()
        || record["version"] != 1
        || string(&record, "name") != name
        || string(&record, "archive_id") != id
        || !AGENTS.contains(&string(&record, "agent"))
    {
        return Err(format!("{name}: invalid archive record {id}"));
    }
    Ok(record)
}

pub(super) fn records() -> Result<Vec<Value>> {
    let directory = root().join("archives");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<_> = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut result = Vec::new();
    for path in paths {
        let loaded = (|| {
            let mut value: Value =
                serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            rename::normalize_archive(&mut value)?;
            read_archive(
                string(&value, "name"),
                path.file_stem().and_then(|id| id.to_str()).unwrap_or(""),
            )
        })();
        match loaded {
            Ok(record) => result.push(record),
            Err(error) => eprintln!("hgs: cannot read {}: {error}", path.display()),
        }
    }
    result.sort_by(|a, b| {
        b["archived_at"]
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&a["archived_at"].as_f64().unwrap_or_default())
    });
    Ok(result)
}

/// Worktree guards need metadata only. Stream a bounded inventory rather than
/// sorting/materializing all archives or opening any conversation journal.
/// Incomplete coverage is an error, never proof that no archive refers to a tree.
pub(super) fn visit_bounded(deadline: std::time::Instant, mut visit: impl FnMut(Value) -> Result<()>) -> Result<()> {
    use std::io::Read;
    let entries=match fs::read_dir(root().join("archives")) {
        Ok(entries)=>entries,
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(()),
        Err(e)=>return Err(e.to_string()),
    };
    let mut bytes=0;
    for (count,entry) in entries.enumerate() {
        if count>=5000 || std::time::Instant::now()>=deadline {return Err("Archive metadata inventory exceeded its check budget; review manually".into());}
        let path=entry.map_err(|e|e.to_string())?.path();
        if path.extension().is_none_or(|e|e!="json"){continue;}
        let file=crate::platform::refuse_link(fs::OpenOptions::new().read(true)).open(&path).map_err(|e|e.to_string())?;
        if !file.metadata().is_ok_and(|m|m.is_file()) {return Err("Archive metadata is not a regular file".into());}
        let mut data=Vec::new();file.take(1024*1024+1).read_to_end(&mut data).map_err(|e|e.to_string())?;
        bytes+=data.len();
        if data.len()>1024*1024 || bytes>32*1024*1024 {return Err("Archive metadata inventory exceeded its byte budget; review manually".into());}
        let mut value:Value=serde_json::from_slice(&data).map_err(|e|e.to_string())?;
        rename::normalize_archive(&mut value)?;
        let id=path.file_stem().and_then(|s|s.to_str()).unwrap_or_default();
        if value["version"]!=1 || string(&value,"archive_id")!=id
            || uuid::Uuid::parse_str(id).is_err() || string(&value,"name").is_empty()
            || !AGENTS.contains(&string(&value,"agent")) {return Err("Invalid archive metadata; review manually".into());}
        visit(value)?;
    }
    Ok(())
}

pub(super) fn remove_synced(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => crate::platform::sync_dir(path.parent().ok_or("invalid record path")?)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// The caller holds the state writer lock. Write the durable copy before
/// releasing its old name; a crash between the two operations cannot lose it.
pub(super) fn save(record: &Value) -> Result<String> {
    if clear_context::cleanup_pending(record) {
        return Err("Restored Clear archive cleanup is unfinished; retain the binding for review".into());
    }
    let db=archive_catalog::catalog()?;
    let existing=archive_catalog::find(&db,record)?;
    let id = if let Some(existing) = existing {
        existing
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let mut archived = record.clone();
        archived["archive_id"] = json!(id);
        archived["archived_at"] = json!(now());
        archived["state"] = json!("archived");
        archived.as_object_mut().unwrap().remove("pausing");
        atomic(&path(&id)?, &format!("{archived}\n"))?;
        id
    };
    if record_path(string(record, "name")).exists() {
        let current = read(string(record, "name"))?;
        if current["run_id"] == record["run_id"]
            && current["conversation_id"] == record["conversation_id"]
        {
            remove_synced(&record_path(string(record, "name")))?;
            rename::remove_legacy(&current)?;
        }
    }
    Ok(id)
}

pub(super) fn equivalent(record: &Value, archived: &[Value]) -> bool {
    archived.iter().any(|entry| {
        entry["name"] == record["name"]
            && entry["run_id"] == record["run_id"]
            && entry["conversation_id"] == record["conversation_id"]
    })
}

/// Legacy Claude already provides an unambiguous exit reason. Reconcile only
/// after its pane and exact process have gone; ambiguous old records stay saved.
pub(super) fn reconcile() -> Result<()> {
    if !root().exists() {
        return Ok(());
    }
    let _guard = lock(None)?;
    reconcile_locked(&live()?).map(drop)
}

/// Indexed membership for one current binding. The archive JSON stays
/// authoritative: catalog() refreshes a changed archive directory before the
/// lookup. This can rebuild that index, but never enumerates saved bindings.
pub(super) fn contains_selected(record: &Value) -> Result<bool> {
    if !root().join("archives").exists() {
        return Ok(false);
    }
    archive_catalog::contains(&archive_catalog::catalog()?, record)
}

fn reconcile_candidate(record: &Value, snapshot: &Snapshot) -> bool {
    !snapshot.contains_key(string(record, "name"))
        && !process_alive(record)
        && record.get("pausing").is_none()
        && !record["paused"].as_bool().unwrap_or(false)
        && string(record, "expected_id").is_empty()
}

fn clean_completion(record: &Value) -> bool {
    record["termination_signal"].is_null()
        && known_exit(record)
        && ((string(record, "agent") == "claude"
            && record.get("supervisor").is_none()
            && string(record, "error").is_empty())
            || (string(record, "completion_source") == "supervisor"
                && string(record, "completion_reason") == "clean_exit"
                && record["exit_code"] == 0))
}

/// Read-mark admission reconciles just its requested binding. Global lifecycle
/// reconciliation remains in listings; an acknowledgement must not sweep every
/// unrelated saved session while holding the shared-read writer.
pub(super) fn reconcile_selected(name: &str) -> Result<()> {
    if !root().exists() {
        return Ok(());
    }
    let _guard = lock(None)?;
    if !record_path(name).exists() {
        return Ok(());
    }
    let record = read(name)?;
    if reconcile_candidate(&record, &live()?)
        && (contains_selected(&record)? || clean_completion(&record))
    {
        save(&record)?;
    }
    Ok(())
}

/// `reconcile` for a caller that holds the state lock and read `snapshot`
/// under it. Returns the archives and the bindings as it leaves them.
pub(super) fn reconcile_locked(snapshot: &Snapshot) -> Result<(Vec<Value>, Vec<Value>)> {
    let archived = records()?;
    let bindings = super::storage::records()?;
    let mut saved = false;
    for record in &bindings {
        if snapshot.contains_key(string(record, "name"))
            || process_alive(record)
            || record.get("pausing").is_some()
            || record["paused"].as_bool().unwrap_or(false)
            || !string(record, "expected_id").is_empty()
        {
            continue;
        }
        if equivalent(record, &archived) {
            save(record)?;
            saved = true;
            continue;
        }
        let explicit_claude = string(record, "agent") == "claude"
            && record.get("supervisor").is_none()
            && record["termination_signal"].is_null()
            && string(record, "error").is_empty()
            && known_exit(record);
        let confirmed_supervisor = string(record, "completion_source") == "supervisor"
            && string(record, "completion_reason") == "clean_exit"
            && record["exit_code"] == 0
            && record["termination_signal"].is_null()
            && known_exit(record);
        if explicit_claude || confirmed_supervisor {
            save(record)?;
            saved = true;
        }
    }
    if saved {
        return Ok((records()?, super::storage::records()?));
    }
    Ok((archived, bindings))
}

/// Active polling never opens every archived record or its conversation journal.
/// Only bindings that might need reconciliation consult the metadata index.
pub(super) fn reconcile_active_locked(snapshot: &Snapshot) -> Result<Vec<Value>> {
    let bindings = super::storage::records()?;
    let mut result = Vec::new();
    let mut catalog=None;
    for record in bindings {
        // A live exact binding cannot be the crash-left copy of a completed run.
        if matches(&record,snapshot.get(string(&record,"name"))) || process_alive(&record) { result.push(record);continue; }
        if catalog.is_none(){catalog=Some(archive_catalog::catalog()?);}
        if archive_catalog::contains(catalog.as_ref().unwrap(), &record)? { save(&record)?; continue; }
        if snapshot.contains_key(string(&record, "name")) || process_alive(&record)
            || record.get("pausing").is_some() || record["paused"].as_bool().unwrap_or(false)
            || !string(&record, "expected_id").is_empty() {
            result.push(record); continue;
        }
        let clean = record["termination_signal"].is_null() && known_exit(&record) &&
            ((string(&record,"agent")=="claude" && record.get("supervisor").is_none() && string(&record,"error").is_empty()) ||
             (string(&record,"completion_source")=="supervisor" && string(&record,"completion_reason")=="clean_exit" && record["exit_code"]==0));
        if clean { save(&record)?; } else { result.push(record); }
    }
    Ok(result)
}

pub(super) fn manual(name: &str, dry: bool) -> Result<i32> { manual_scoped(name,dry,None) }

pub(super) fn manual_scoped(name: &str, dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    let _guard = lock(None)?;
    if live()?.contains_key(name) {
        return Err(format!(
            "{name}: only stopped or paused sessions can be archived"
        ));
    }
    let mut record = read(name)?;
    if let Some(scope)=scope {scope.check(&record)?;}
    if process_alive(&record) {
        return Err(format!("{name}: tracked agent process is still running"));
    }
    if string(&record, "conversation_id").is_empty() {
        return Err(format!("{name}: conversation ID is not confirmed"));
    }
    if dry {
        println!("hgs: would archive {name}");
        return Ok(0);
    }
    record["completion_source"] = json!("manual_archive");
    record["completion_reason"] = json!("user_archived");
    if let Some(scope)=scope {scope.changing();}
    let id = save(&record)?;
    drop(_guard);
    super::worktree_lifecycle::after_archive(&record, &id);
    if let Some(scope)=scope {scope.result(name,string(&record,"run_id"),string(&record,"conversation_id"),Some(&id));}
    else {println!("hgs: archived {name} ({id})");}
    Ok(0)
}

pub(super) fn forget(name: &str, id: &str, dry: bool) -> Result<()> { forget_scoped(name,id,dry,None) }

pub(super) fn forget_scoped(name: &str, id: &str, dry: bool, scope: Option<&session_action::Scope>) -> Result<()> {
    let _guard = lock(None)?;
    let record=read_archive(name, id)?;
    if let Some(scope)=scope {scope.check(&record)?;}
    if clear_context::cleanup_pending(&record) {
        return Err("Restored Clear archive cleanup is unfinished; retain the archive for review".into());
    }
    // Do not remove the only recoverable source of a not-yet-confirmed restore.
    if record_path(name).exists() && {
        let active = read(name)?;
        string(&active, "restore_archive_id") == id || clear_context::cleanup_pending(&active)
    } {
        return Err(format!(
            "{name}: archive restore is still awaiting confirmation"
        ));
    }
    if dry {
        println!("hgs: would forget archive {id} ({name})");
        return Ok(());
    }
    if let Some(scope)=scope {scope.changing();}
    remove_synced(&path(id)?)
}

/// The immutable source of a still-pending restore, before its binding changes.
pub(super) fn restore_source(record: &Value) -> Result<Value> {
    let archived = read_archive(string(record, "name"), string(record, "restore_archive_id"))?;
    if string(record, "expected_id").is_empty()
        || record["expected_id"] != record["conversation_id"]
        || archived["conversation_id"] != record["expected_id"]
        || archived["agent"] != record["agent"]
        || string(&archived, "run_id").is_empty()
    {
        return Err("archive restore source changed; refresh before clearing".into());
    }
    Ok(archived)
}

/// Fence the exact original archive BEFORE publishing its cleared successor.
/// A resident old supervisor may later move that successor into Archive. The
/// source then still refuses current Forget/Restore without scanning archives.
/// This runs only for the already accepted real main clear SessionStart.
pub(super) fn reserve_clear_source(record: &Value) -> Result<()> {
    if !clear_context::restore_committed(record) {
        return Err("restored Clear cleanup identity is incomplete".into());
    }
    let anchor = &record["restore_archive_cleanup"];
    let id = string(anchor, "archive_id");
    let mut archived = read_archive(string(record, "name"), id)?;
    if !clear_context::restored_successor(record, &archived) {
        return Err("archive restore source changed before successor publication".into());
    }
    if let Some(prior) = archived.get("restore_archive_clear_source") {
        if prior != anchor {
            return Err("archive Clear source fence changed; retain both records for review".into());
        }
        // A retry must make the previously written fence durable too; existence
        // alone is not a successful file/directory sync receipt.
        fs::File::open(path(id)?).and_then(|file| file.sync_all()).map_err(|e| e.to_string())?;
        return crate::platform::sync_dir(path(id)?.parent().ok_or("invalid archive path")?)
            .map_err(|error| error.to_string());
    }
    archived["restore_archive_clear_source"] = anchor.clone();
    atomic(&path(id)?, &format!("{archived}\n"))
}

/// Commit the restore after its exact ID, or its explicitly cleared successor.
/// The active binding has already been synced by the caller before this runs.
pub(super) fn confirm_restore(record: &mut Value) -> Result<()> {
    let pending = clear_context::cleanup_pending(record);
    if pending && !clear_context::restore_committed(record) {
        return Err("restored Clear cleanup identity is incomplete; retain both records for review".into());
    }
    let id = if pending { string(&record["restore_archive_cleanup"], "archive_id") }
        else { string(record, "restore_archive_id") }.to_owned();
    if id.is_empty() || !string(record, "expected_id").is_empty() {
        return Ok(());
    }
    if path(&id)?.try_exists().map_err(|error| error.to_string())? {
        let archived = read_archive(string(record, "name"), &id)?;
        if pending && archived["restore_archive_clear_source"] != record["restore_archive_cleanup"] {
            return Err("archive Clear source fence changed; retain both records for review".into());
        }
        if archived["conversation_id"] != record["conversation_id"]
            && !clear_context::restored_successor(record, &archived)
        {
            return Err("archive restore confirmed a different conversation".into());
        }
        remove_synced(&path(&id)?)?;
    }
    if pending {
        // A prior unlink may have succeeded while its directory sync failed.
        // Re-sync even the already-absent cut before dropping either anchor.
        crate::platform::sync_dir(path(&id)?.parent().ok_or("invalid archive path")?)
            .map_err(|error| error.to_string())?;
    }
    // Do not discard the in-memory fence either if the final write fails.
    let mut settled = record.clone();
    settled.as_object_mut().unwrap().remove("restore_archive_id");
    settled.as_object_mut().unwrap().remove("resume_pending");
    if pending {
        settled.as_object_mut().unwrap().remove("restore_archive_cleanup");
        settled.as_object_mut().unwrap().remove("clear_context_request");
    }
    write(&mut settled)?;
    *record = settled;
    Ok(())
}

pub(super) fn failed_restore(record: &mut Value) -> Result<bool> {
    // An accepted SessionStart already synced the active conversation. Retry
    // finalization only; never discard it after an archive cleanup failure.
    // On failure, propagate uncertainty with both records left recoverable.
    if clear_context::cleanup_pending(record)
        || (!string(record, "restore_archive_id").is_empty() && string(record, "expected_id").is_empty()) {
        confirm_restore(record)?;
        return Ok(false);
    }
    let id = string(record, "restore_archive_id");
    if id.is_empty() || !path(id)?.exists() {
        return Ok(false);
    }
    let mut archived = read_archive(string(record, "name"), id)?;
    archived["last_restore_error"] = json!(if string(record, "error").is_empty() {
        "agent exited before confirming the archived conversation"
    } else {
        string(record, "error")
    });
    atomic(&path(id)?, &format!("{archived}\n"))?;
    remove_synced(&record_path(string(record, "name")))?;
    Ok(true)
}

pub(super) fn known_exit(record: &Value) -> bool {
    let end = &record["session_end"];
    if end["run_id"] != record["run_id"]
        || end["conversation_id"] != record["conversation_id"]
        || string(record, "conversation_state") != "ended"
    {
        return false;
    }
    matches!(
        (string(record, "agent"), string(end, "reason")),
        ("claude", "prompt_input_exit") | ("codex", "other") | ("kimi", "exit")
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    // Each filesystem cut runs in its own literal environment: other parallel
    // state tests must never see this test's HGS_STATE_DIR.
    #[test]
    fn restored_clear_finalization_preserves_each_durable_cut() {
        for cut in [
            "before_start",
            "unconfirmed_clear",
            "before_delete",
            "after_delete",
            "after_flags",
            "unreadable",
            "replaced",
            "stale_anchor",
            "missing_anchor",
            "new_before_delete", "new_after_delete", "new_after_flags", "new_unreadable",
            "new_replaced", "new_stale_anchor", "new_missing_anchor", "new_missing_request_marker",
            "new_missing_cleanup_anchor", "new_malformed_cleanup", "new_flag_write_failure",
            "new_stale_request", "new_stale_cleanup", "new_unlink_before_sync",
            "new_missing_source_fence", "new_malformed_source_fence", "new_stale_source_fence",
            "new_before_active_write",
        ] {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "state::archive::tests::restore_cut_probe",
                    "--exact",
                    "--nocapture",
                ])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", root.path())
                .env("HGS_CONFIG_DIR", root.path().join("config"))
                .env("HGS_STATE_DIR", root.path().join("state"))
                .env("HGS_TEST_RESTORE_CUT", cut)
                .output()
                .unwrap();
            println!(
                "restore cut {cut}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                output.status.success(),
                "{cut}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn restore_cut_probe() {
        let Ok(cut) = std::env::var("HGS_TEST_RESTORE_CUT") else {
            return;
        };
        let id = "11111111-1111-4111-8111-111111111111";
        let name = "codex/restore-test";
        let mut source = json!({"version":1,"name":name,"agent":"codex","archive_id":id,
            "run_id":"archived-run","conversation_id":"saved"});
        let mut record = json!({"version":1,"name":name,"agent":"codex","run_id":"live-run",
            "pid":42,"conversation_id":"new","restore_archive_id":id,"activity":"idle","phase":"idle",
            "main_done":true,"active_tools":{},"subagents":{},"session_clear":{"at":1.0,
                "run_id":"live-run","conversation_id":"new","previous_conversation_id":"saved",
                "source":"native_hook","restore_archive_id":id,"restore_archive_run_id":"archived-run",
                "restore_clear_request_id":"22222222-2222-4222-8222-222222222222"}});
        let modern = cut.starts_with("new_");
        let cut = cut.strip_prefix("new_").unwrap_or(&cut);
        if modern {
            record["restore_archive_cleanup"] = json!({"phase":"committed","run_id":"live-run",
                "conversation_id":"new","previous_conversation_id":"saved","archive_id":id,
                "archive_run_id":"archived-run","request_id":"22222222-2222-4222-8222-222222222222"});
            record["clear_context_request"] = json!({"run_id":"live-run","conversation_id":"saved",
                "restore_archive_id":id,"restore_archive_run_id":"archived-run",
                "request_id":"22222222-2222-4222-8222-222222222222","restore_archive_cleanup_pending":true});
            record.as_object_mut().unwrap().remove("restore_archive_id");
        }
        if ["before_start", "unconfirmed_clear"].contains(&cut) {
            record["conversation_id"] = json!("saved");
            record["expected_id"] = json!("saved");
            record.as_object_mut().unwrap().remove("session_clear");
            if cut == "unconfirmed_clear" {
                record["clear_context_request"] = json!({"run_id":"live-run","conversation_id":"saved",
                    "restore_archive_id":id,"restore_archive_run_id":"archived-run",
                    "request_id":"22222222-2222-4222-8222-222222222222"});
            }
        } else {
            assert!(clear_context::restore_committed(&record));
            assert!(!clear_context::available(&record, true));
        }
        if modern { source["restore_archive_clear_source"] = record["restore_archive_cleanup"].clone(); }
        atomic(&path(id).unwrap(), &source.to_string()).unwrap();
        match cut {
            "after_delete" => remove_synced(&path(id).unwrap()).unwrap(),
            "unlink_before_sync" => fs::remove_file(path(id).unwrap()).unwrap(),
            "after_flags" => {
                remove_synced(&path(id).unwrap()).unwrap();
                record.as_object_mut().unwrap().remove("restore_archive_id");
                record.as_object_mut().unwrap().remove("restore_archive_cleanup");
                record.as_object_mut().unwrap().remove("clear_context_request");
            }
            "unreadable" => fs::write(path(id).unwrap(), b"incomplete archive").unwrap(),
            "replaced" => {
                let mut replaced = source.clone();
                replaced["run_id"] = json!("replacement-run");
                atomic(&path(id).unwrap(), &replaced.to_string()).unwrap();
            }
            "stale_anchor" => record["session_clear"]["run_id"] = json!("earlier-run"),
            "missing_anchor" => { record.as_object_mut().unwrap().remove("session_clear"); }
            "missing_request_marker" => { record["clear_context_request"].as_object_mut().unwrap().remove("restore_archive_cleanup_pending"); }
            "missing_cleanup_anchor" => { record.as_object_mut().unwrap().remove("restore_archive_cleanup"); }
            "malformed_cleanup" => record["restore_archive_cleanup"] = Value::Null,
            "stale_request" => record["clear_context_request"]["request_id"] = json!("33333333-3333-4333-8333-333333333333"),
            "stale_cleanup" => record["restore_archive_cleanup"]["conversation_id"] = json!("other"),
            "missing_source_fence" | "malformed_source_fence" | "stale_source_fence" => {
                let mut changed = source.clone();
                match cut {
                    "missing_source_fence" => { changed.as_object_mut().unwrap().remove("restore_archive_clear_source"); }
                    "malformed_source_fence" => changed["restore_archive_clear_source"] = Value::Null,
                    _ => changed["restore_archive_clear_source"]["request_id"] = json!("stale"),
                }
                atomic(&path(id).unwrap(), &changed.to_string()).unwrap();
            }
            "before_start" | "unconfirmed_clear" | "before_delete" | "flag_write_failure" | "before_active_write" => {}
            _ => panic!("unknown cut"),
        }
        if cut == "before_active_write" {
            use std::os::unix::fs::PermissionsExt;
            let mut original = record.clone();
            original["conversation_id"] = json!("saved"); original["expected_id"] = json!("saved");
            original["restore_archive_id"] = json!(id);
            original.as_object_mut().unwrap().remove("restore_archive_cleanup");
            original["clear_context_request"].as_object_mut().unwrap().remove("restore_archive_cleanup_pending");
            write(&mut original).unwrap();
            let mut unfenced = source.clone();
            unfenced.as_object_mut().unwrap().remove("restore_archive_clear_source");
            atomic(&path(id).unwrap(), &unfenced.to_string()).unwrap();
            reserve_clear_source(&record).unwrap();
            fs::set_permissions(root(), fs::Permissions::from_mode(0o500)).unwrap();
            let result = write(&mut record);
            fs::set_permissions(root(), fs::Permissions::from_mode(0o700)).unwrap();
            assert!(result.is_err());
            assert_eq!(read(name).unwrap()["conversation_id"], "saved");
            assert_eq!(read_archive(name,id).unwrap()["restore_archive_clear_source"], record["restore_archive_cleanup"]);
            assert!(failed_restore(&mut original).unwrap());
            assert!(clear_context::cleanup_pending(&read_archive(name,id).unwrap()));
            return;
        }
        write(&mut record).unwrap();
        if cut == "flag_write_failure" {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root(), fs::Permissions::from_mode(0o500)).unwrap();
            let result = confirm_restore(&mut record);
            fs::set_permissions(root(), fs::Permissions::from_mode(0o700)).unwrap();
            assert!(result.is_err(), "expected real final active-write refusal");
            assert!(clear_context::cleanup_pending(&record));
            assert!(clear_context::cleanup_pending(&read(name).unwrap()));
            assert!(!path(id).unwrap().exists());
        }
        let before = fs::read(path(id).unwrap()).ok();
        assert_eq!(clear_context::available(&record, true), cut == "after_flags");
        // Exercise the real finalizer with a synthetic failed exit status, not
        // a claim that this probe ran or stopped an agent process.
        let result = supervisor::finish(
            name,
            "live-run",
            42,
            std::process::ExitStatus::from_raw(256),
        );
        if ["before_start", "unconfirmed_clear"].contains(&cut) {
            result.unwrap();
            assert!(!record_path(name).exists());
            let kept = read_archive(name, id).unwrap();
            assert_eq!(kept["conversation_id"], "saved");
            assert!(!string(&kept, "last_restore_error").is_empty());
            return;
        }
        let current = read(name).unwrap();
        assert_eq!(current["conversation_id"], "new");
        assert_eq!(current["run_id"], "live-run");
        assert_eq!(current["session_clear"], record["session_clear"]);
        if ["stale_anchor", "missing_anchor", "missing_request_marker", "missing_cleanup_anchor", "malformed_cleanup", "stale_request", "stale_cleanup", "missing_source_fence", "malformed_source_fence", "stale_source_fence"].contains(&cut) {
            assert!(result.is_err());
            assert_eq!(fs::read(path(id).unwrap()).ok(), before);
            if modern { assert!(clear_context::cleanup_pending(&current)); }
            else { assert_eq!(current["restore_archive_id"], id); }
            return;
        }
        if ["unreadable", "replaced"].contains(&cut) {
            assert!(result.is_err());
            assert_eq!(fs::read(path(id).unwrap()).ok(), before);
            assert!(clear_context::restore_committed(&current));
            // Retrying finalization after the source is made readable and exact
            // uses the already durable transition, with no new hook or command.
            atomic(&path(id).unwrap(), &source.to_string()).unwrap();
            let mut current = current;
            confirm_restore(&mut current).unwrap();
        } else {
            result.unwrap();
        }
        let current = read(name).unwrap();
        assert_eq!(current["conversation_id"], "new");
        assert!(current.get("restore_archive_id").is_none());
        assert!(!clear_context::cleanup_pending(&current));
        assert!(!path(id).unwrap().exists());
    }
}
