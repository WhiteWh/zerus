//! Exact exit lookup and advisory archive evidence for the two Teams readers.
use super::*;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn check_root(state: &Path) -> Result<()> {
    if crate::platform::canonical(state).map_err(|e| e.to_string())?
        != crate::platform::canonical(root()).map_err(|e| e.to_string())?
    {
        return Err("Archive lookup belongs to another state root".into());
    }
    Ok(())
}

fn canonical_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| !uuid.is_nil() && uuid.to_string() == id)
}

/// A standing confirmation can use only the existing complete, unique run
/// lookup. Keep the result unknown on this pass even when repair was admitted.
pub(crate) fn find_run(state: &Path, name: &str, run: &str) -> Result<Option<String>> {
    check_root(state)?;
    let result = find_run_locked(name, run);
    if result.as_ref().is_err_and(|error| incomplete(error)) {
        refresh::request_for_team();
    }
    result
}

fn find_run_locked(name: &str, run: &str) -> Result<Option<String>> {
    let _guard =
        storage::writer_until(Instant::now() + Duration::from_millis(300))?.ok_or(INCOMPLETE)?;
    let db = pin(refresh::catalog_without_request()?)?;
    if refresh::proof(&db)? != db.directory {
        return Err(INCOMPLETE.into());
    }
    let mut query = sql(db.prepare(
        "SELECT substr(id,1,65) FROM archive_identities WHERE valid=1 AND name=? AND run=? ORDER BY id LIMIT 2",
    ))?;
    let ids = sql(query.query_map(params![name, run], |row| row.get::<_, String>(0)))?;
    let ids = sql(ids.collect::<rusqlite::Result<Vec<_>>>())?;
    if ids.len() > 1 || ids.iter().any(|id| !canonical_id(id)) {
        return Err("No exact unique exit archive".into());
    }
    if let Some(id) = ids.first() {
        let record = archive::read_archive(name, id)?;
        if record["run_id"] != run {
            return Err("The exact archive identity changed".into());
        }
    }
    current_proof(&db)?;
    Ok(ids.into_iter().next())
}

/// A sighting on disk is enough to explain a missing row to the person. It
/// conveys no authority to resume. A cold read has its own small inspection
/// budget and requests the worker even if no matching body fits that budget.
pub(crate) fn member_archive_pending(state: &Path, session: &str, launch: &str) -> Result<bool> {
    // The worker repairs its own state root. A caller reading a different
    // root retains the binding result from that root's native observation.
    if check_root(state).is_err() {
        return Ok(false);
    }
    if !canonical_id(session) || !canonical_id(launch) {
        return Err(INCOMPLETE.into());
    }
    PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(batch) = slot.as_mut() {
            batch
                .get_or_insert_with(read_pending_archives)
                .as_ref()
                .map_err(String::clone)?
                .contains(session, launch)
        } else {
            read_pending_archives()?.contains(session, launch)
        }
    })
}

type Batch = Option<Result<PendingArchives>>;
thread_local! {
    static PROBE: RefCell<Option<Batch>> = const { RefCell::new(None) };
}

/// One cold catalog read and disk walk for an entire service actor pass.
/// A changed directory invalidates the result until the next pass.
pub(crate) fn with_member_archive_probe<T>(pass: impl FnOnce() -> T) -> T {
    struct Restore(Option<Batch>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBE.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(PROBE.with(|slot| slot.replace(Some(None))));
    pass()
}

#[derive(Default)]
struct PendingArchives {
    directory: Option<String>,
    complete: bool,
    found: BTreeMap<(String, String), (PathBuf, String)>,
}
impl PendingArchives {
    fn contains(&self, session: &str, launch: &str) -> Result<bool> {
        if self
            .directory
            .as_ref()
            .is_some_and(|stamp| refresh::directory().as_ref() != Ok(stamp))
        {
            return Err(INCOMPLETE.into());
        }
        if let Some((path, stamp)) = self.found.get(&(session.to_owned(), launch.to_owned())) {
            return if archive_references::current_stamp(path).as_ref() == Ok(stamp) {
                Ok(true)
            } else {
                Err(INCOMPLETE.into())
            };
        }
        if self.complete {
            Ok(false)
        } else {
            Err(INCOMPLETE.into())
        }
    }
}

#[cfg(test)]
thread_local! {
    static PROBE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn read_pending_archives() -> Result<PendingArchives> {
    #[cfg(test)]
    PROBE_READS.with(|count| count.set(count.get() + 1));
    let directory = refresh::directory()?;
    if refresh::catalog_without_request().is_ok() {
        return Ok(PendingArchives {
            directory: Some(directory),
            complete: true,
            ..Default::default()
        });
    }
    refresh::request_for_team();
    const ENTRIES: usize = 64;
    const BYTES: u64 = 4 << 20;
    let until = Instant::now() + Duration::from_millis(25);
    let mut pending = PendingArchives {
        directory: Some(directory.clone()),
        complete: true,
        ..Default::default()
    };
    if directory == "absent" {
        return Ok(pending);
    }
    let db = refresh::existing().ok();
    let mut remaining = BYTES;
    for (examined, entry) in fs::read_dir(root().join("archives"))
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if examined == ENTRIES || Instant::now() >= until || remaining == 0 {
            pending.complete = false;
            break;
        }
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(id) = path
            .file_stem()
            .and_then(|id| id.to_str())
            .filter(|id| path.extension().is_some_and(|ext| ext == "json") && canonical_id(id))
        else {
            continue;
        };
        let Ok(stamp) = archive_references::current_stamp(&path) else {
            pending.complete = false;
            continue;
        };
        if db
            .as_ref()
            .is_some_and(|db| refresh::has_identity(db, id, &stamp))
        {
            continue;
        }
        let size = archive_references::stamped_bytes(&stamp)?;
        if size > remaining {
            pending.complete = false;
            continue;
        }
        remaining -= size;
        let Some(raw) = labels::read_file(&path, size).ok().flatten() else {
            pending.complete = false;
            continue;
        };
        let Ok(record) = serde_json::from_slice::<Value>(&raw) else {
            pending.complete = false;
            continue;
        };
        if archive::validate_record(string(&record, "name"), id, &record).is_err()
            || archive_references::current_stamp(&path).as_ref() != Ok(&stamp)
        {
            pending.complete = false;
            continue;
        }
        let session = string(&record, "session_id");
        let launch = string(&record, "launch_id");
        if record["state"] == "archived" && canonical_id(session) && canonical_id(launch) {
            pending
                .found
                .insert((session.into(), launch.into()), (path, stamp));
        }
    }
    if refresh::directory()? != directory {
        return Err(INCOMPLETE.into());
    }
    Ok(pending)
}

#[cfg(test)]
mod tests;
