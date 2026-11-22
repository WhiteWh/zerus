//! Immutable completion records, indexed independently of reusable tmux names.
use super::*;
use std::fs;

pub(super) fn path(id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_| "invalid archive ID")?;
    Ok(root().join("archives").join(format!("{id}.json")))
}

pub(super) fn read_archive(name: &str, id: &str) -> Result<Value> {
    if name.starts_with("dsh/") {
        return dsh::native_archives::by_id(id)
            .and_then(|row| {
                let row = row.filter(|row| row["name"] == name).ok_or("no such DeepSeek archive")?;
                session_id::verify(&row)?;
                Ok(row)
            })
            .map_err(|error| format!("{name}: cannot read archive {id}: {error}"));
    }
    let record = archive_references::read_metadata(&path(id)?)
        .map_err(|error| format!("{name}: cannot read archive {id}: {error}"))?;
    validated_record(name, id, record)
}

/// Validate an already captured native body without reopening its pathname.
pub(super) fn validated_record(name: &str, id: &str, mut record: Value) -> Result<Value> {
    rename::normalize_archive(&mut record)?;
    validate_record(name, id, &record)?;
    Ok(record)
}
/// Shared field/identity checks for an already normalized native body.
pub(super) fn validate_record(name: &str, id: &str, record: &Value) -> Result<()> {
    if !record.is_object()
        || record["version"] != 1
        || string(record, "name") != name
        || string(record, "archive_id") != id
        || !AGENTS.contains(&string(record, "agent"))
    {
        return Err(format!("{name}: invalid archive record {id}"));
    }
    session_id::verify(record)?;
    Ok(())
}

pub(super) fn records() -> Result<Vec<Value>> {
    Ok(inventory()?.records)
}

/// Complete collection for decisions that must not infer archive absence.
/// Ordinary reads may omit an unreadable unrelated archive with a warning.
pub(super) fn complete_records() -> Result<Vec<Value>> {
    let inventory = inventory()?;
    if let Some(error) = inventory.unavailable {
        return Err(error);
    }
    Ok(inventory.records)
}

struct Inventory {
    records: Vec<Value>,
    unavailable: Option<String>,
}

fn inventory() -> Result<Inventory> {
    let directory = root().join("archives");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                records: Vec::new(),
                unavailable: None,
            });
        }
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_dir() => {
            return Err("archive directory is not a directory".into());
        }
        Ok(_) => (),
    }
    let mut paths: Vec<_> = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let path = entry.path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                    .then_some(Ok(path))
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    paths.sort();
    let mut result = Vec::new();
    let mut unavailable = None;
    for path in paths {
        let loaded = (|| {
            let mut value = archive_references::read_metadata(&path)?;
            rename::normalize_archive(&mut value)?;
            read_archive(
                string(&value, "name"),
                path.file_stem().and_then(|id| id.to_str()).unwrap_or(""),
            )
        })();
        match loaded {
            Ok(record) => result.push(record),
            Err(error) => {
                let error = format!("cannot read archive {}: {error}", path.display());
                eprintln!("hgs: {error}");
                if unavailable.is_none() {
                    unavailable = Some(error);
                }
            }
        }
    }
    result.sort_by(|a, b| {
        b["archived_at"]
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&a["archived_at"].as_f64().unwrap_or_default())
    });
    Ok(Inventory {
        records: result,
        unavailable,
    })
}

/// A search-index match is not proof that the retained archive is readable.
/// Verify the exact body before using it instead of the current binding.
fn verified_existing(db: &archive_catalog::Catalog, record: &Value) -> Result<Option<String>> {
    // find verifies a selected body and every immutable identity before it
    // returns a positive; a negative requires complete compact native coverage.
    archive_catalog::find(db, record)
}

pub(super) fn remove_synced(path: &Path) -> Result<()> {
    super::listing_metadata::remove_at(&root(), path, || remove_synced_inner(path))
}

fn remove_synced_inner(path: &Path) -> Result<()> {
    storage::cut_write()?;
    let catalog = archive_catalog::before_removal(path);
    match fs::remove_file(path) {
        Ok(()) => {
            crate::platform::sync_dir(path.parent().ok_or("invalid record path")?)
                .map_err(|error| error.to_string())?;
            super::archive_references::forget(path);
            if let Some(db) = catalog {
                archive_catalog::removed(db, path);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Native SessionStart may confirm a conversation during an original launch.
/// That confirmation cannot release the same run's pending owner reservation.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn retirement_fenced(record:&Value,current:&Value) -> bool {
    current["run_id"]==record["run_id"] && (supervisor::owned_pending(current)
        || (current["conversation_id"]==record["conversation_id"] && terminating::pending(current)))
}

/// The caller holds the state writer lock. Write the durable copy before
/// releasing its old name; a crash between the two operations cannot lose it.
pub(super) fn save(record: &Value) -> Result<String> {
    if supervisor::owned_pending(record) {
        return Err("Original descendant owner requires cohort finalization; binding kept".into());
    }
    if terminating::pending(record) {
        return Err("Termination is unfinished; retain the session and archive for review".into());
    }
    // A caller's older copy must not retire the same binding after Terminate
    // fenced it. The caller holds the writer lock across both reads and save.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if storage::current_of(record)?.is_some_and(|current| retirement_fenced(record, &current)) {
        return Err("Termination is unfinished; retain the session and archive for review".into());
    }
    if clear_context::cleanup_pending(record) {
        return Err(
            "Restored Clear archive cleanup is unfinished; retain the binding for review".into(),
        );
    }
    let id = publish(record)?;
    // The archived session's own record goes, never its label's holder: a
    // session in the ID layout removes `sessions/<id>.json` (P3a).
    if let Some(current) = storage::current_of(record)? {
        if current["run_id"] == record["run_id"]
            && current["conversation_id"] == record["conversation_id"]
        {
            storage::remove_record(&current)?;
        }
    }
    // Its own Git worktree lock goes with it, or stays and is reported; the
    // archive stands either way (P3c).
    worktree_lock::left(record);
    Ok(id)
}

fn publish(record: &Value) -> Result<String> {
    let db = archive_catalog::writer_catalog()?;
    let existing = verified_existing(&db, record)?;
    let id = if let Some(existing) = existing {
        existing
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let mut archived = record.clone();
        archived["archive_id"] = json!(id);
        archived["archived_at"] = json!(now());
        archived["state"] = json!("archived");
        archived.as_object_mut().unwrap().remove("pausing");
        // An archive holds no label, lives in no layout and has no terminal:
        // a restore writes wherever its label then allows (P3a: the label's
        // own path), and its launch names a terminal anew (P3b).
        archived.as_object_mut().unwrap().remove("layout");
        archived.as_object_mut().unwrap().remove("terminal_name");
        let path = path(&id)?;
        atomic_record(&path, &mut archived)?;
        // Request bounded metadata catch-up after publication. The exact
        // native archive ID remains readable while coverage is incomplete;
        // unavailable indexed membership never means the archive is absent.
        archive_catalog::published(db, &path);
        id
    };
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
/// authoritative: a selected match verifies its exact native body. Incomplete
/// catalog coverage stays unavailable and requests background metadata work;
/// this never enumerates saved bindings or rebuilds inline.
pub(super) fn contains_selected(record: &Value) -> Result<bool> {
    if !root().join("archives").exists() {
        return Ok(false);
    }
    archive_catalog::contains(&archive_catalog::catalog()?, record)
}

fn reconcile_candidate(record: &Value, snapshot: &Snapshot) -> bool {
    // An environment that cannot be read counts as live: never archived.
    !terminating::pending(record)
        && !supervisor::owned_pending(record)
        && !terminal_live(snapshot, record).unwrap_or(true)
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
    let inventory = inventory()?;
    let archived = inventory.records;
    let bindings = super::storage::records()?;
    if inventory.unavailable.is_some() {
        // Keep every binding on an incomplete global reconciliation. This is
        // not a reason to block an ordinary operation on another exact name.
        return Ok((archived, bindings));
    }
    let mut saved = false;
    for record in &bindings {
        // Held original owners keep their binding; only their cohort finalizer
        // may retire it. An unrelated listing must remain available.
        if terminating::pending(record)
            || supervisor::owned_pending(record)
            || terminal_live(snapshot, record).unwrap_or(true)
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
///
/// Task 193: the read-only listing that replaced `reconcile_active_locked`, for a
/// listing that must not take the writer lock (owner decision, 2026-11-07). It
/// filters exactly as the writer's pass does but saves nothing: a record the
/// writer would save is listed as its committed state until the writer's next
/// reconcile records it. The archive catalog is only read (`contains_existing`),
/// so a stale or missing catalog lists the row rather than writing the index.
pub(super) fn read_active(snapshot: &Snapshot) -> Result<Vec<Value>> {
    let bindings = super::storage::records()?;
    let mut result = Vec::new();
    // One read-only catalog connection for the listing, never one per record.
    let catalog = archive_catalog::existing();
    for record in bindings {
        if terminating::pending(&record) { result.push(record); continue; }
        // A live exact binding cannot be the crash-left copy of a completed run.
        if matches(&record, panes_of(snapshot, &record)) || process_alive(&record) {
            result.push(record);
            continue;
        }
        if catalog.as_ref().is_some_and(|db| archive_catalog::contains(db, &record).unwrap_or(false)) {
            continue;
        }
        if terminal_live(snapshot, &record).unwrap_or(true)
            || process_alive(&record)
            || record.get("pausing").is_some()
            || record["paused"].as_bool().unwrap_or(false)
            || !string(&record, "expected_id").is_empty()
        {
            result.push(record);
            continue;
        }
        let clean = record["termination_signal"].is_null()
            && known_exit(&record)
            && ((string(&record, "agent") == "claude"
                && record.get("supervisor").is_none()
                && string(&record, "error").is_empty())
                || (string(&record, "completion_source") == "supervisor"
                    && string(&record, "completion_reason") == "clean_exit"
                    && record["exit_code"] == 0));
        if !clean {
            result.push(record);
        }
    }
    Ok(result)
}

/// Active polling never opens every archived record or its conversation journal.
/// Only bindings that might need reconciliation consult the metadata index.
pub(super) fn reconcile_active_locked(snapshot: &Snapshot) -> Result<Vec<Value>> {
    reconcile_active_records_locked(snapshot, super::storage::records()?)
}

/// A display worker may schedule an exact canonical binding, but supplies no
/// authority: every existing archive/process/run guard below is retained.
pub(super) fn reconcile_selected_locked(snapshot: &Snapshot, record: Value) -> Result<()> {
    reconcile_active_records_locked(snapshot, vec![record]).map(drop)
}
fn reconcile_active_records_locked(
    snapshot: &Snapshot,
    bindings: Vec<Value>,
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut catalog = None;
    let mut archive_unavailable = false;
    for record in bindings {
        // Retain opaque owned reservations before consulting archive membership.
        if terminating::pending(&record) || supervisor::owned_pending(&record) {
            result.push(record);
            continue;
        }
        // A live exact binding cannot be the crash-left copy of a completed run.
        if matches(&record, panes_of(snapshot, &record)) || process_alive(&record) {
            result.push(record);
            continue;
        }
        if archive_unavailable {
            result.push(record);
            continue;
        }
        if catalog.is_none() {
            match archive_catalog::catalog() {
                Ok(db) => catalog = Some(db),
                Err(_) => {
                    archive_unavailable = true;
                    result.push(record);
                    continue;
                }
            }
        }
        match verified_existing(catalog.as_ref().unwrap(), &record) {
            Ok(Some(_)) => {
                save(&record)?;
                continue;
            }
            Err(error) => {
                // This read-only proof failed before any save. Keep this
                // binding while allowing unrelated rows to remain available.
                eprintln!("hgs: cannot reconcile {}: {error}", string(&record, "name"));
                archive_unavailable = archive_catalog::incomplete(&error);
                result.push(record);
                continue;
            }
            Ok(None) => (),
        }
        if terminal_live(snapshot, &record).unwrap_or(true)
            || process_alive(&record)
            || record.get("pausing").is_some()
            || record["paused"].as_bool().unwrap_or(false)
            || !string(&record, "expected_id").is_empty()
        {
            result.push(record);
            continue;
        }
        let clean = record["termination_signal"].is_null()
            && known_exit(&record)
            && ((string(&record, "agent") == "claude"
                && record.get("supervisor").is_none()
                && string(&record, "error").is_empty())
                || (string(&record, "completion_source") == "supervisor"
                    && string(&record, "completion_reason") == "clean_exit"
                    && record["exit_code"] == 0));
        if clean {
            save(&record)?;
        } else {
            result.push(record);
        }
    }
    Ok(result)
}

pub(super) fn manual(name: &str, dry: bool) -> Result<i32> { manual_scoped(name,dry,None) }

pub(super) fn manual_scoped(name: &str, dry: bool, scope: Option<&session_action::Scope>) -> Result<i32> {
    let _guard = lock(None)?;
    let mut record = read(name)?;
    // The session's own terminal (P3b), not another under its label.
    if terminal_live(&live()?, &record)? {
        return Err(format!(
            "{name}: only stopped or paused sessions can be archived"
        ));
    }
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
    if record["backend"] == "dsh" && dsh::retirement::restore_source_held(&record)? {
        return Err(dsh::retirement::RESTORE_PENDING.into());
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
    if record["backend"] == "dsh" {
        dsh::native_archives::remove(&record)?;
    } else {
        remove_synced(&path(id)?)?;
    }
    // A lock its archive kept, once nothing else uses the checkout (P3c).
    worktree_lock::left(&record);
    Ok(())
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
    atomic_record(&path(id)?, &mut archived)
}

/// Commit the restore after its exact ID, or its explicitly cleared successor.
/// The active binding has already been synced by the caller before this runs.
pub(super) fn confirm_restore(record: &mut Value) -> Result<()> {
    if terminating::pending(record) {
        return Ok(());
    }
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
    if terminating::pending(record) || supervisor::owned_pending(record) {
        return Ok(false);
    }
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
    atomic_record(&path(id)?, &mut archived)?;
    remove_synced(&storage::own_path(record)?)?;
    Ok(true)
}

pub(super) fn known_exit(record: &Value) -> bool {
    if terminating::pending(record) {
        return false;
    }
    let end = &record["session_end"];
    if end["run_id"] != record["run_id"]
        || end["conversation_id"] != record["conversation_id"]
        || string(record, "conversation_state") != "ended"
    {
        return false;
    }
    matches!(
        (string(record, "agent"), string(end, "reason")),
        ("claude", "prompt_input_exit") | ("codex", "other") | ("kimi", "exit") | ("qwen", "prompt_input_exit")
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn selected_reconciliation_retains_a_termination_fence() {
        let mut record = json!({"name":"codex/termination-fence","agent":"codex",
            "run_id":"original-run","conversation_id":"original-conversation",
            "conversation_state":"ended","completion_source":"supervisor",
            "completion_reason":"clean_exit","exit_code":0,
            "session_end":{"run_id":"original-run","conversation_id":"original-conversation","reason":"other"}});
        assert!(reconcile_candidate(&record, &Snapshot::new()));
        assert!(clean_completion(&record));
        record["terminating"] = Value::Null;
        assert!(!reconcile_candidate(&record, &Snapshot::new()));
        assert!(!clean_completion(&record));
    }

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

#[cfg(all(test, unix))]
mod owned_listing_tests;
