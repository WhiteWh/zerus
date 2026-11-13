//! One coalesced worker per state root; each transaction has a count/time budget.
//! Restart reopens discovery in the background and retains every committed row
//! and sequence. No directory walk (including restart work) runs in inspection.
use super::*;
use fs2::FileExt;
use std::fs::ReadDir;
#[cfg(not(test))]
use std::process::{Command, Stdio};
use std::time::Instant;

const BATCH: usize = 64;
const SLICE: Duration = Duration::from_millis(25);

pub(super) fn stamp(path: &Path) -> Result<String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let (seconds, nanos) = crate::platform::modified(&metadata);
            Ok(format!(
                "{}:{seconds}:{nanos}:{:?}",
                metadata.len(),
                metadata.created().ok()
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok("absent".into()),
        _ => Err("Receipt source is unavailable".into()),
    }
}
fn try_guard(path: &Path) -> Result<Option<Guard>> {
    private_dir(path.parent().ok_or("Invalid index lock")?)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    crate::platform::private_open(&mut options);
    crate::platform::no_follow(&mut options);
    let file = options
        .open(path)
        .map_err(|_| "Receipt index worker lock is unavailable")?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("Receipt index worker lock is unavailable".into());
    }
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(Guard::held(file))),
        Err(error) if crate::platform::lock_contended(&error) => Ok(None),
        Err(_) => Err("Receipt index worker lock is unavailable".into()),
    }
}
pub(super) fn request(state: &Path) {
    // A libtest image is not an hgs helper. Actual detached-worker coverage
    // uses the separately built CLI and a private state root.
    #[cfg(test)]
    let _ = state;
    #[cfg(not(test))]
    request_worker(state);
}
#[cfg(not(test))]
fn request_worker(state: &Path) {
    let launch = (|| -> Result<()> {
        let canonical =
            crate::platform::canonical(state).map_err(|_| "Receipt state is unavailable")?;
        let state = canonical.as_path();
        let Some(_launch) = try_guard(&state.join("receipt-index-launch.lock"))? else {
            return Ok(());
        };
        let Some(probe) = try_guard(&state.join("receipt-index-worker.lock"))? else {
            return Ok(());
        };
        let attempt = state.join("receipt-index-attempt.json");
        // A corrupt hint must not park the requesting send/inspection. Keep
        // its existing 4 KiB read bound and refuse links/non-regular objects.
        let mut options = OpenOptions::new();
        options.read(true);
        crate::platform::refuse_link(&mut options);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let last = options
            .open(&attempt)
            .ok()
            .filter(|file| file.metadata().is_ok_and(|m| m.is_file()))
            .and_then(|file| serde_json::from_reader::<_, Value>(file.take(4096)).ok())
            .and_then(|value| value["at"].as_f64())
            .unwrap_or(0.0);
        if now() - last < 5.0 {
            return Ok(());
        }
        atomic(&attempt, &json!({"at":now()}).to_string())?;
        let mut command =
            Command::new(std::env::current_exe().map_err(|_| "Receipt worker is unavailable")?);
        command
            .args(["__state", "receipt-index-worker"])
            .env("HGS_STATE_DIR", state)
            .env_remove("HGS_SESSION")
            .env_remove("HGS_RUN_ID")
            .env_remove("HGS_SESSION_ID")
            .env_remove("HGS_AGENT")
            .current_dir(state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(probe);
        let _child = crate::platform::spawn_detached(&mut command)
            .map_err(|_| "Receipt worker is unavailable")?;
        Ok(())
    })();
    let _ = launch;
}

struct Walk {
    source: Source,
    entries: Option<ReadDir>,
    started: String,
    covered: bool,
    after: Option<String>,
    indexed: i64,
    epoch: String,
    count_at_start: i64,
}
fn source_witness(catalog: &Catalog, source: Source) -> Result<(String, i64)> {
    sql(catalog.db.query_row(
        "SELECT (SELECT value FROM receipt_meta WHERE key='epoch'),indexed
           FROM receipt_sources WHERE source=?",
        [source.key()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))
}
pub(super) struct Migration {
    catalog: Catalog,
    sources: Vec<Source>,
    force: bool,
    next_source: usize,
    walk: Option<Walk>,
}
impl Migration {
    pub(super) fn new(state: &Path) -> Result<Self> {
        // Restart reconciles originals even when cached directory metadata
        // claims completion. A lost locator is not a change to its directory.
        Self::with_sources(state, Source::ALL.to_vec(), true)
    }
    pub(super) fn questions(state: &Path) -> Result<Self> {
        Self::with_sources(state, vec![Source::Question], true)
    }
    fn with_sources(state: &Path, sources: Vec<Source>, force: bool) -> Result<Self> {
        let catalog = Catalog::open(state, true)?;
        // Disk-backed SQLite scratch, scoped to this worker connection. A
        // restart begins a new walk; no in-memory set grows with history.
        sql(catalog.db.execute_batch(
            "PRAGMA temp_store=FILE; CREATE TEMP TABLE receipt_walk_seen(request TEXT PRIMARY KEY);",
        ))?;
        Ok(Self {
            catalog,
            sources,
            force,
            next_source: 0,
            walk: None,
        })
    }
    /// A bounded slice. Holding ReadDir across slices avoids rescanning a
    /// prefix on every batch; committed locators survive process interruption.
    pub(super) fn step(&mut self) -> Result<bool> {
        let deadline = Instant::now() + SLICE;
        let transaction = sql(self.catalog.db.unchecked_transaction())?;
        let mut examined = 0;
        while examined < BATCH && Instant::now() < deadline {
            if self.walk.is_none() {
                let Some(source) = self.sources.get(self.next_source).copied() else {
                    sql(transaction.commit())?;
                    return Ok(true);
                };
                self.next_source += 1;
                let directory = self.catalog.state.join(source.directory());
                let started = match stamp(&directory) {
                    Ok(stamp) => stamp,
                    Err(_) => {
                        self.catalog
                            .error(source, "@source", "source_unavailable")?;
                        sql(self.catalog.db.execute(
                            "UPDATE receipt_sources SET complete=0 WHERE source=?",
                            [source.key()],
                        ))?;
                        continue;
                    }
                };
                let prior: Option<(String, bool)> = sql(self
                    .catalog
                    .db
                    .query_row(
                        "SELECT stamp,complete FROM receipt_sources WHERE source=?",
                        [source.key()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional())?;
                if !self.force
                    && prior
                        .as_ref()
                        .is_some_and(|(stamp, complete)| *complete && *stamp == started)
                {
                    continue;
                }

                sql(self.catalog.db.execute(
                    "UPDATE receipt_sources SET complete=0 WHERE source=?",
                    [source.key()],
                ))?;
                sql(self.catalog.db.execute("DELETE FROM receipt_walk_seen", []))?;
                let entries = if started == "absent" {
                    None
                } else {
                    Some(fs::read_dir(&directory).map_err(|_| "Receipt source is unavailable")?)
                };
                let covered = prior.is_some_and(|(stamp, complete)| complete && stamp == started);
                let (epoch, count_at_start) = source_witness(&self.catalog, source)?;
                self.walk = Some(Walk {
                    source,
                    entries,
                    started,
                    covered,
                    after: None,
                    indexed: 0,
                    epoch,
                    count_at_start,
                });
            }
            let walk = self.walk.as_mut().unwrap();
            if let Some(entries) = walk.entries.as_mut() {
                match entries.next() {
                    Some(Ok(entry)) => {
                        examined += 1;
                        let filename = entry.file_name();
                        let Some(filename) =
                            filename.to_str().filter(|name| name.ends_with(".json"))
                        else {
                            continue;
                        };
                        let id = filename.strip_suffix(".json").unwrap();
                        let known: bool = sql(self.catalog.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM receipt_rows WHERE source=? AND request=?)",
                        params![walk.source.key(), id],
                        |row| row.get(0),
                    ))?;
                        if !known && walk.covered {
                            // The unchanged directory was previously covered, but
                            // its row vanished without notification. Reject old
                            // snapshot cursors and withhold completeness until the
                            // repair commits, then restore the original.
                            sql(self.catalog.db.execute(
                                "UPDATE receipt_meta SET value=? WHERE key='epoch'",
                                [uuid::Uuid::new_v4().to_string()],
                            ))?;
                            sql(self.catalog.db.execute(
                                "UPDATE receipt_sources SET complete=0 WHERE source=?",
                                [walk.source.key()],
                            ))?;
                        }
                        let loaded = read_original(&entry.path());
                        match loaded {
                            Ok(value) => {
                                if self
                                    .catalog
                                    .remember(walk.source, &entry.path(), &value)
                                    .is_err()
                                {
                                    self.catalog.error(walk.source, filename, "corrupt")?;
                                }
                            }
                            Err(reason) => self.catalog.error(walk.source, filename, reason)?,
                        }
                        sql(self
                            .catalog
                            .db
                            .execute("INSERT OR IGNORE INTO receipt_walk_seen VALUES(?)", [id]))?;
                    }
                    Some(Err(_)) => {
                        examined += 1;
                        self.catalog.error(
                            walk.source,
                            "@enumeration",
                            "enumeration_incomplete",
                        )?;
                        self.walk = None;
                    }
                    None => {
                        walk.entries = None;
                        // Discovery may repair a lost row and rotate the epoch.
                        // The retained-row count starts only after that repair.
                        (walk.epoch, walk.count_at_start) =
                            source_witness(&self.catalog, walk.source)?;
                    }
                }
                continue;
            }
            // The directory walk alone cannot find an original that vanished.
            // Page the existing binding index as well, using its (source,
            // request) unique index. Every checked row shares this step's
            // count/time budget. Present paths were already validated above.
            let row: Option<(String, bool)> = sql(self.catalog.db.query_row(
                "SELECT request,EXISTS(SELECT 1 FROM receipt_walk_seen WHERE request=receipt_rows.request)
                   FROM receipt_rows WHERE source=?1 AND request>?2
                   ORDER BY request LIMIT 1",
                params![walk.source.key(), walk.after.as_deref().unwrap_or("")],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional())?;
            if let Some((id, seen)) = row {
                examined += 1;
                walk.indexed += 1;
                walk.after = Some(id.clone());
                if seen {
                    continue;
                }
                if id.is_empty() || !bounded_identity(&id, 128) || id.contains(['/', '\\']) {
                    return Err("Receipt index is unavailable".into());
                }
                let filename = format!("{id}.json");
                let path = self
                    .catalog
                    .state
                    .join(walk.source.directory())
                    .join(&filename);
                match read_original(&path) {
                    Ok(value) => {
                        if self.catalog.remember(walk.source, &path, &value).is_err() {
                            self.catalog.error(walk.source, &filename, "corrupt")?;
                        }
                    }
                    Err(reason) => {
                        // Keep the original binding and locator. Its absence
                        // is a durable explicit gap, never an absent send or
                        // permission to replay. Publish a change so history
                        // consumers revisit an already imported original too.
                        self.catalog.error(walk.source, &filename, reason)?;
                        let stamp = format!("gap:{reason}");
                        let previous: String = sql(self.catalog.db.query_row(
                            "SELECT stamp FROM receipt_rows WHERE source=? AND request=?",
                            params![walk.source.key(), id],
                            |row| row.get(0),
                        ))?;
                        if previous != stamp {
                            let change = self.catalog.bump()?;
                            sql(self.catalog.db.execute(
                                "UPDATE receipt_rows SET stamp=?,change=? WHERE source=? AND request=?",
                                params![stamp, change, walk.source.key(), id],
                            ))?;
                        }
                    }
                }
                continue;
            }
            let source = walk.source;
            let current = stamp(&self.catalog.state.join(source.directory()));
            let (epoch, count) = source_witness(&self.catalog, source)?;
            let complete = current.as_ref().is_ok_and(|stamp| *stamp == walk.started)
                && epoch == walk.epoch
                && count == walk.count_at_start;
            // Count each retained binding in the already budgeted indexed
            // pass, including missing originals and previously seen paths.
            // A backdated arrival or a changed row set can pass the cursor:
            // keep the live trigger count until a fresh stable walk finishes.
            // An insert can also repair an existing file without changing its
            // directory stamp or epoch. The trigger witness catches that race.
            sql(self.catalog.db.execute(
                "UPDATE receipt_sources SET stamp=?1,complete=?2,
                   indexed=CASE WHEN ?2 THEN ?3 ELSE indexed END WHERE source=?4",
                params![walk.started, complete, walk.indexed, source.key()],
            ))?;
            sql(self.catalog.db.execute(
                "DELETE FROM receipt_errors WHERE source=? AND name IN('@source','@enumeration')",
                [source.key()],
            ))?;
            sql(self.catalog.db.execute(
                        "INSERT INTO receipt_meta VALUES('verified_at',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        [now().to_string()],
                    ))?;
            self.walk = None;
        }
        sql(transaction.commit())?;
        Ok(false)
    }
}
/// Bounded discovery for the canonical history indexer. The detached worker
/// stays the long-running owner; this only keeps a synchronous indexer from
/// waiting on worker scheduling, one short budget at a time.
pub(super) fn advance(state: &Path, budget: Duration) -> Result<bool> {
    let deadline = Instant::now() + budget;
    let mut migration = Migration::new(state)?;
    loop {
        if migration.step()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
    }
}

pub(super) fn worker(state: &Path) -> Result<i32> {
    let Some(_guard) = try_guard(&state.join("receipt-index-worker.lock"))? else {
        return Ok(0);
    };
    let mut migration = Migration::new(state)?;
    while !migration.step()? {
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(0)
}

#[cfg(test)]
mod tests;
