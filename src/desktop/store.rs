//! The service actor alone owns this connection and its transactional schema.
use crate::{config::Config, platform};
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use zerus_desktop_migration::{canonical, configuration, HANDOFF_LOCK};

#[path = "maintenance.rs"]
pub(crate) mod maintenance;

pub const SCHEMA: i64 = super::cache_effects::SCHEMA;
pub struct HandoffLock {
    _lock: platform::FileLock,
    root: PathBuf,
    configuration: String,
}
impl HandoffLock {
    pub fn verify(&self, location: &Location) -> Result<()> {
        ensure!(
            self.root == location.root && self.configuration == location.configuration,
            "handoff lock belongs to another configuration"
        );
        Ok(())
    }
}
pub struct Location {
    pub root: PathBuf,
    pub configuration: String,
}
impl Location {
    pub fn new(config: &Config) -> Result<Self> {
        let state = canonical(&config.state)?;
        Ok(Self {
            root: state.join("desktop"),
            configuration: configuration(&config.state, &config.dir)?,
        })
    }
    pub fn prepare(&self) -> Result<()> {
        for path in [
            self.root.parent().context("missing state folder")?,
            &self.root,
        ] {
            platform::private_dir(path)?;
            ensure!(
                platform::verify_private(path, platform::Private::Directory)?,
                "desktop folders must be private directories"
            );
        }
        Ok(())
    }
    pub fn check_existing(&self) -> Result<()> {
        for path in [
            self.root.parent().context("missing state folder")?,
            &self.root,
        ] {
            match platform::verify_private(path, platform::Private::Directory) {
                Ok(true) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                _ => anyhow::bail!("desktop folders must be private directories"),
            }
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<platform::FileLock> {
        let file = private_file(&self.root.join("owner.lock"), true)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("another desktop service owns this state")?;
        Ok(platform::FileLock::held(file))
    }
    /// Read-only preflight must not create desktop state on a never-used host.
    /// An existing owner still has to be excluded before returning a no-op.
    pub fn existing_lock(&self) -> Result<Option<platform::FileLock>> {
        self.check_existing()?;
        let file = match private_file(&self.root.join("owner.lock"), false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        fs2::FileExt::try_lock_exclusive(&file)
            .context("another desktop service owns this state")?;
        Ok(Some(platform::FileLock::held(file)))
    }
    /// Passive inspection releases this immediately. Migrated writers will
    /// retain it for their full lifetime; the service process lock is separate.
    pub fn handoff_lock(&self) -> Result<HandoffLock> {
        self.check_existing()?;
        let file = private_file(&self.root.join(HANDOFF_LOCK), true)?;
        fs2::FileExt::try_lock_exclusive(&file).context("desktop handoff is busy")?;
        Ok(HandoffLock {
            _lock: platform::FileLock::held(file),
            root: self.root.clone(),
            configuration: self.configuration.clone(),
        })
    }
    /// Distinguish an actual flock contender from path/identity/open failures.
    /// Neither branch creates authority or repairs an existing state path.
    #[cfg_attr(windows, allow(dead_code))] // Also used by G3's cross-platform import transport.
    pub(super) fn try_handoff_lock(&self) -> Result<Option<HandoffLock>> {
        self.check_existing()?;
        let file = private_file(&self.root.join(HANDOFF_LOCK), true)?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(HandoffLock {
                _lock: platform::FileLock::held(file),
                root: self.root.clone(),
                configuration: self.configuration.clone(),
            })),
            Err(error) if platform::lock_contended(&error) => Ok(None),
            Err(error) => Err(error).context("desktop handoff lock failed"),
        }
    }
    /// A Mac host can outlive its service while original native callback copies
    /// drain. Its FD3 holds this separate flock; owner.lock absence is not drain.
    #[cfg(unix)]
    pub(super) fn notification_host_exclusion(&self) -> Result<Option<platform::FileLock>> {
        self.check_existing()?;
        let file = match private_file(&self.root.join("notification-host.lock"), false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        fs2::FileExt::try_lock_exclusive(&file)?;
        Ok(Some(platform::FileLock::held(file)))
    }
}

fn private_file(path: &Path, create: bool) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    platform::private_open(
        options
            .read(true)
            .write(true)
            .create(create)
            .truncate(false),
    );
    #[cfg(unix)]
    platform::no_follow(&mut options);
    #[cfg(windows)]
    platform::refuse_link(&mut options);
    let file = options.open(path)?;
    ensure!(
        platform::verify_private_file(&file)?,
        "desktop state must be private single-link regular files"
    );
    Ok(file)
}

/// Reused before opening SQLite in either the actor or a legacy admission probe.
pub(super) fn database_files(location: &Location) -> Result<()> {
    for name in [
        "state.sqlite3",
        "state.sqlite3-wal",
        "state.sqlite3-shm",
        "state.sqlite3-journal",
    ] {
        match platform::verify_private(&location.root.join(name), platform::Private::File) {
            Ok(true) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => anyhow::bail!("desktop database files must be private single-link regular files"),
        }
    }
    Ok(())
}

/// Read one indexed policy row, without migration, restart or authority changes.
/// None means no database; schema one predates effect owners and stays passive.
pub(super) fn writer_active(location: &Location) -> Result<Option<bool>> {
    location.check_existing()?;
    match fs::symlink_metadata(location.root.join("state.sqlite3")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => {
            result?;
        }
    }
    database_files(location)?;
    let db = Connection::open_with_flags(
        location.root.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_millis(250))?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!((1..=SCHEMA).contains(&version), "unknown desktop schema");
    let configuration: String = db.query_row(
        "SELECT substr(configuration,1,65) FROM service WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        configuration == location.configuration,
        "desktop configuration mismatch"
    );
    if version < 9 {
        let unexpected: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='desktop_producer_prepared'",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            unexpected == 0,
            "prepared journal has an older schema marker"
        );
    }
    if version == 1 {
        return Ok(Some(false));
    }
    let revoked: i64 = db.query_row("SELECT revoked FROM desktop_writer WHERE id=1", [], |r| {
        r.get(0)
    })?;
    ensure!(matches!(revoked, 0 | 1), "invalid desktop writer policy");
    if version >= 9 {
        ensure!(
            super::producer_prepared::inspect_db(&db, &location.configuration)?.is_none(),
            "unsettled producer preparation requires reconciliation before legacy presence"
        );
    }
    Ok(Some(revoked == 0 || operation_undrained(&db)? != 0))
}

/// Every admitted effect kind participates in takeover/revoke exclusion. A new
/// per-kind ledger must never disappear behind task-only resource totals.
pub(super) fn operation_undrained(db: &Connection) -> Result<u64> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let task = if version >= 7 {
        zerus_desktop_ledger::operations::totals(db)?.undrained
    } else {
        0
    };
    let clones = if version >= super::creation_clone::SCHEMA {
        super::creation_clone::undrained(db)?
    } else {
        0
    };
    let cache = if version >= super::cache_maintenance::SCHEMA {
        zerus_desktop_ledger::operations::cache::undrained(db)?
    } else {
        0
    };
    task.checked_add(clones)
        .and_then(|count| count.checked_add(cache))
        .context("operation resource total overflow")
}

pub struct Store {
    pub(super) db: Connection,
    pub generation: u64,
    recovery_error: bool,
    read_recovery_error: bool,
    operation_recovery_error: bool,
    creation_recovery_error: bool,
    catalog_collection_error: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    custody: super::custody::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    creation_custody: super::creation_clone::Recovery,
}
impl Store {
    pub fn open(location: &Location, epoch: &str) -> Result<Self> {
        let path = location.root.join("state.sqlite3");
        database_files(location)?;
        let flags = OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        // A newer schema is probed read-only before any PRAGMA or migration write.
        if path.try_exists()? {
            let probe =
                Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let version: i64 = probe.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            ensure!(
                (0..=SCHEMA).contains(&version),
                "desktop state requires a newer hgs version"
            );
        }
        let _file = private_file(&path, true)?;
        let mut db = Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        super::counts::register(&db)?;
        db.busy_timeout(Duration::from_secs(2))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            let tables: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )?;
            ensure!(tables == 0, "unversioned desktop state is not empty");
            let tx = db.transaction()?;
            tx.execute_batch("CREATE TABLE service (id INTEGER PRIMARY KEY CHECK(id=1), configuration TEXT NOT NULL, generation INTEGER NOT NULL, epoch TEXT NOT NULL); PRAGMA user_version=1;")?;
            tx.execute(
                "INSERT INTO service VALUES(1,?1,0,'')",
                [&location.configuration],
            )?;
            tx.commit()?;
        }
        let configuration: String =
            db.query_row("SELECT configuration FROM service WHERE id=1", [], |row| {
                row.get(0)
            })?;
        ensure!(
            configuration == location.configuration,
            "desktop state belongs to another configuration"
        );
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA max_page_count=131072; PRAGMA journal_size_limit=67108864;")?;
        if version < 2 {
            let tx = db.transaction()?;
            zerus_desktop_ledger::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=2;")?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = db.transaction()?;
            tx.execute_batch(
                "CREATE TABLE desktop_commands (
                request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                reason TEXT NOT NULL CHECK(reason IN('manual','rollback')),
                writer_epoch TEXT, state TEXT NOT NULL CHECK(state IN('prepared','settled')),
                result TEXT, at INTEGER NOT NULL);
                PRAGMA user_version=3;",
            )?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = db.transaction()?;
            zerus_desktop_ledger::reads::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=4;")?;
            tx.commit()?;
        }
        if version < 5 {
            let tx = db.transaction()?;
            zerus_desktop_catalog::migrate_base(&tx)?;
            tx.execute_batch("PRAGMA user_version=5;")?;
            tx.commit()?;
        }
        if version < 6 {
            let tx = db.transaction()?;
            zerus_desktop_catalog::migrate_metadata(&tx)?;
            tx.execute_batch("PRAGMA user_version=6;")?;
            tx.commit()?;
        }
        if version < 7 {
            let tx = db.transaction()?;
            zerus_desktop_ledger::operations::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=7;")?;
            tx.commit()?;
        }
        if version < 8 {
            let tx = db.transaction()?;
            zerus_desktop_ledger::operations::custody::migrate(&tx)?;
            tx.execute_batch("CREATE INDEX desktop_commands_pending ON desktop_commands(state,at,request_id); PRAGMA user_version=8;")?;
            tx.commit()?;
        }
        if version < 9 {
            let tx = db.transaction()?;
            super::producer_prepared::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=9;")?;
            tx.commit()?;
        }
        if version < 10 {
            let tx = db.transaction()?;
            super::counts::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=10;")?;
            tx.commit()?;
        }
        if version < 11 {
            let tx = db.transaction()?;
            super::delivery::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=11;")?;
            tx.commit()?;
        }
        if version < 12 {
            let tx = db.transaction()?;
            maintenance::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=12;")?;
            tx.commit()?;
        }
        if version < 13 {
            let tx = db.transaction()?;
            zerus_desktop_catalog::changes::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=13;")?;
            tx.commit()?;
        }
        if version < 14 {
            let tx = db.transaction()?;
            super::attention::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=14;")?;
            tx.commit()?;
        }
        if version < 15 {
            let tx = db.transaction()?;
            super::ownership::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=15;")?;
            tx.commit()?;
        }
        if version < 16 {
            let tx = db.transaction()?;
            super::notification_controls::migrate(&tx)?;
            tx.execute_batch("PRAGMA user_version=16;")?;
            tx.commit()?;
        }
        if version < super::creation_clone::SCHEMA {
            let tx = db.transaction()?;
            super::creation_clone::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::creation_clone::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::cache_maintenance::SCHEMA {
            let tx = db.transaction()?;
            super::cache_maintenance::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::cache_maintenance::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::cache_effects::SCHEMA {
            let tx = db.transaction()?;
            super::cache_effects::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::cache_effects::SCHEMA)?;
            tx.commit()?;
        }
        // Opening Store follows acquisition of the real exclusive serve owner.
        // Cache workers are in-process and retain it through all actual I/O.
        let tx = db.transaction()?;
        zerus_desktop_ledger::operations::cache::recover(&tx)?;
        tx.commit()?;
        zerus_desktop_catalog::begin_service(&mut db, epoch)?;
        // No marker or previous policy re-enables a worker after restart.
        zerus_desktop_ledger::begin_service(&mut db, epoch)?;
        zerus_desktop_ledger::recover_batch(&mut db, epoch)?;
        let read_recovery_error = zerus_desktop_ledger::reads::recover(
            &mut db,
            epoch,
            chrono::Utc::now().timestamp_millis(),
        )
        .is_err();
        let operation_recovery_error =
            zerus_desktop_ledger::operations::recover_batch(&mut db, epoch).is_err();
        let creation_recovery_error = super::creation_clone::startup(&mut db).is_err();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE service SET generation=generation+1, epoch=?1 WHERE id=1",
            [epoch],
        )?;
        let generation = tx.query_row("SELECT generation FROM service WHERE id=1", [], |row| {
            row.get(0)
        })?;
        tx.commit()?;
        platform::sync_dir(&location.root)?;
        Ok(Self {
            db,
            generation,
            recovery_error: false,
            read_recovery_error,
            operation_recovery_error,
            creation_recovery_error,
            catalog_collection_error: false,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            custody: super::custody::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            creation_custody: super::creation_clone::Recovery::default(),
        })
    }
    pub fn checkpoint(&self) -> Result<()> {
        let (busy, _, _): (i64, i64, i64) =
            self.db
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        ensure!(
            busy == 0,
            "desktop checkpoint is busy; revocation is not settled"
        );
        Ok(())
    }
    pub fn effect_status(&self) -> serde_json::Value {
        let mut status = zerus_desktop_ledger::status(&self.db).unwrap_or_else(|_| serde_json::json!({"revoked":null,"admissions_paused":true,"error":"ledger_unavailable"}));
        if self.recovery_error {
            status["admissions_paused"] = true.into();
            status["error"] = "ledger_recovery_failed".into();
        }
        status
    }
    pub fn read_status(&self) -> serde_json::Value {
        let mut status = zerus_desktop_ledger::reads::status(&self.db).unwrap_or_else(|_| {
            serde_json::json!({"pending":null,"in_flight":null,"error":"read_queue_unavailable"})
        });
        status["admissions_paused"] = true.into();
        status["recovery_error"] = self.read_recovery_error.into();
        status
    }
    pub fn operation_status(&self) -> serde_json::Value {
        let clone = super::creation_clone::summary(&self.db);
        let mut status = match zerus_desktop_ledger::operations::totals(&self.db).and_then(
            |totals| {
                Ok((
                    totals,
                    operation_undrained(&self.db)?,
                    clone
                        .as_ref()
                        .map_err(|_| anyhow::anyhow!("creation summary unavailable"))?,
                ))
            },
        ) {
            Ok((totals, undrained, clone)) => {
                serde_json::json!({"unsettled":totals.unsettled+clone["unsettled"].as_u64().unwrap_or(0),"undrained":undrained,
                "kinds":{"task_start_reconcile":{"unsettled":totals.unsettled,"undrained":totals.undrained},"creation_clone":clone},
                "admissions_paused":true,"recovery_error":self.operation_recovery_error || self.creation_recovery_error})
            }
            Err(_) => {
                serde_json::json!({"unsettled":null,"undrained":null,"admissions_paused":true,
                "error":"operation_store_unavailable","recovery_error":self.operation_recovery_error || self.creation_recovery_error})
            }
        };
        match super::cache_maintenance::undrained(self) {
            Ok(count) => {
                status["cache_undrained"] = count.into();
            }
            Err(_) => {
                status["cache_undrained"] = serde_json::Value::Null;
                status["undrained"] = serde_json::Value::Null;
                status["error"] = "cache_store_unavailable".into();
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            status["resource_custody"] = self.custody.status();
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            status["resource_custody"] =
                serde_json::json!({"supported":false,"helper_state":"unavailable"});
        }
        if status["undrained"] != 0 {
            use rusqlite::OptionalExtension;
            if let Ok(Some((attempt, record))) =
                zerus_desktop_ledger::operations::custody::pending(&self.db)
            {
                if status["resource_custody"]["attempt_id"] != attempt.ticket.attempt_id {
                    status["resource_custody"] = serde_json::json!({"state":"unavailable","attempt_id":attempt.ticket.attempt_id,
                        "helper_state":"unavailable","helper_pid":record.and_then(|r|r.helper.map(|h|h.pid))});
                }
            }
            let request = self.db.query_row("SELECT request_id FROM desktop_commands WHERE state='prepared' ORDER BY at,request_id LIMIT 1",[],|r|r.get::<_,String>(0)).optional().ok().flatten();
            status["resource_custody"]["revoke_request_id"] = request.into();
            status["resource_custody"]["remediation"] = super::revoke::OPERATION_REMEDIATION.into();
        }
        if clone
            .as_ref()
            .ok()
            .is_some_and(|v| v["undrained"].as_u64().is_some_and(|n| n > 0))
        {
            let task = status["resource_custody"].take();
            status["resource_custody"] = serde_json::json!({"state":"retained","creation_clone":clone.ok(),
                "task_start_reconcile":task,"remediation":super::revoke::OPERATION_REMEDIATION});
        }
        status
    }
    pub fn operation_undrained(&self) -> Result<u64> {
        operation_undrained(&self.db)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn creation_recovery_ok(&self) -> bool {
        !self.creation_recovery_error
    }
    pub fn recover_operation_custody(&mut self, location: &Location) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.custody.tick(location, &mut self.db).is_err() {
            self.operation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.creation_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = location;
    }
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(super) fn retry_operation_custody(&mut self) {
        self.custody.retry_now();
    }
    pub fn catalog_status(&self) -> serde_json::Value {
        match zerus_desktop_catalog::totals(&self.db) {
            Ok(totals) => serde_json::json!({
                "retained_sources":totals.retained_sources,"stored_rows":totals.stored_rows,
                "stored_bytes":totals.stored_bytes,"obsolete_generations":totals.obsolete_generations,
                "admissions_paused":true,"collection_error":self.catalog_collection_error,
            }),
            Err(_) => serde_json::json!({
                "retained_sources":null,"stored_rows":null,"stored_bytes":null,
                "obsolete_generations":null,"admissions_paused":true,
                "collection_error":self.catalog_collection_error,"error":"catalog_unavailable",
            }),
        }
    }
    /// One indexed batch, never a loop over all retained rows. False backs off
    /// both idle and failed collection; failure affects only this component.
    pub fn collect_catalog(&mut self) -> bool {
        match zerus_desktop_catalog::collect(&mut self.db) {
            Ok(progress) => {
                self.catalog_collection_error = false;
                progress.is_some()
            }
            Err(_) => {
                self.catalog_collection_error = true;
                false
            }
        }
    }
    pub fn recover_effects(&mut self, epoch: &str) {
        self.recovery_error = zerus_desktop_ledger::recover_batch(&mut self.db, epoch).is_err();
        self.operation_recovery_error =
            zerus_desktop_ledger::operations::recover_batch(&mut self.db, epoch).is_err();
        self.read_recovery_error = zerus_desktop_ledger::reads::recover(
            &mut self.db,
            epoch,
            chrono::Utc::now().timestamp_millis(),
        )
        .is_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    #[test]
    fn catalog_collection_yields_and_failure_is_local_and_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let _owner = location.lock().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        let mut store = Store::open(&location, &epoch).unwrap();
        let baseline = zerus_desktop_ledger::put_baseline(
            &mut store.db,
            &zerus_desktop_ledger::Identity {
                kind: zerus_desktop_ledger::Kind::Post,
                target: zerus_desktop_ledger::Target::Session {
                    computer: "peer".into(),
                    run: "run-1".into(),
                    conversation: "conversation-1".into(),
                },
                edge: "edge-1".into(),
            },
            1,
            &serde_json::json!({"token":"synthetic"}),
        )
        .unwrap();
        zerus_desktop_ledger::reads::enqueue(
            &mut store.db,
            &uuid::Uuid::new_v4().to_string(),
            &zerus_desktop_ledger::reads::ReadMark {
                computer: "peer".into(),
                name: "codex/example/session".into(),
                run_id: "run-1".into(),
                conversation_id: "conversation-1".into(),
                reply_id: Some("reply-1".into()),
                attention_signature: None,
            },
            0,
        )
        .unwrap();
        let tx = store.db.transaction().unwrap();
        tx.execute(
            "INSERT INTO desktop_catalog_generations(computer,obsolete) VALUES('peer',1)",
            [],
        )
        .unwrap();
        let obsolete = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO desktop_catalog_generations(computer) VALUES('peer')",
            [],
        )
        .unwrap();
        let visible = tx.last_insert_rowid();
        for n in 0..385 {
            tx.execute(
                "INSERT INTO desktop_catalog_rows(generation,key,json,bytes) VALUES(?1,?2,'{}',64)",
                params![
                    if n == 384 { visible } else { obsolete },
                    format!("run:{n}")
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        assert!(store.collect_catalog());
        assert_eq!(store.catalog_status()["stored_rows"], 129);
        assert!(store.collect_catalog());
        assert_eq!(store.catalog_status()["stored_rows"], 1);
        assert_eq!(store.catalog_status()["obsolete_generations"], 0);
        assert!(!store.collect_catalog());
        store
            .db
            .execute_batch(
                "ALTER TABLE desktop_catalog_generations RENAME TO broken_catalog_generations",
            )
            .unwrap();
        assert!(!store.collect_catalog());
        assert_eq!(store.catalog_status()["collection_error"], true);
        assert_eq!(store.catalog_status()["error"], "catalog_unavailable");
        assert_eq!(store.read_status()["pending"], 1);
        assert!(store.effect_status()["error"].is_null());
        assert!(zerus_desktop_ledger::current(&store.db, &baseline, 1).unwrap());
        store
            .db
            .execute_batch(
                "ALTER TABLE broken_catalog_generations RENAME TO desktop_catalog_generations",
            )
            .unwrap();
        assert!(!store.collect_catalog());
        assert_eq!(store.catalog_status()["collection_error"], false);
        assert_eq!(store.catalog_status()["stored_rows"], 1);
    }
    #[test]
    fn read_queue_recovers_under_a_new_service_epoch_and_fails_locally() {
        use zerus_desktop_ledger::reads;
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "fixture".into(),
        };
        location.prepare().unwrap();
        let _owner = location.lock().unwrap();
        let first = uuid::Uuid::new_v4().to_string();
        let mut store = Store::open(&location, &first).unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        zerus_desktop_ledger::authorize(&mut store.db, &epoch, &first).unwrap();
        let mark = reads::ReadMark {
            computer: "peer".into(),
            name: "session".into(),
            run_id: "run".into(),
            conversation_id: "conversation".into(),
            reply_id: Some("reply".into()),
            attention_signature: None,
        };
        reads::enqueue(&mut store.db, &uuid::Uuid::new_v4().to_string(), &mark, 0).unwrap();
        let sent = reads::reserve(
            &mut store.db,
            &epoch,
            &first,
            &uuid::Uuid::new_v4().to_string(),
            0,
        )
        .unwrap()
        .unwrap();
        drop(store);
        let second = uuid::Uuid::new_v4().to_string();
        let mut store = Store::open(&location, &second).unwrap();
        assert_eq!(store.read_status()["pending"], 1);
        assert_eq!(store.read_status()["in_flight"], 0);
        assert_eq!(store.read_status()["admissions_paused"], true);
        assert!(reads::finish(&mut store.db, &sent, reads::Outcome::NotSent, 0).is_err());
        // A fresh in-flight job lets recovery exercise its indexed lookup.
        zerus_desktop_ledger::authorize(&mut store.db, &epoch, &second).unwrap();
        reads::reserve(
            &mut store.db,
            &epoch,
            &second,
            &uuid::Uuid::new_v4().to_string(),
            i64::MAX,
        )
        .unwrap()
        .unwrap();
        store
            .db
            .execute_batch("ALTER TABLE desktop_read_attempts RENAME TO broken_read_attempts")
            .unwrap();
        store.recover_effects(&second);
        assert_eq!(store.read_status()["recovery_error"], true);
        assert!(store.effect_status()["error"].is_null());
        store
            .db
            .execute_batch("ALTER TABLE broken_read_attempts RENAME TO desktop_read_attempts")
            .unwrap();
        store.recover_effects(&second);
        assert_eq!(store.read_status()["recovery_error"], false);
    }
    #[test]
    fn ledger_recovery_failure_is_component_local_and_clears_after_repair() {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "fixture".into(),
        };
        location.prepare().unwrap();
        let _owner = location.lock().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        let mut store = Store::open(&location, &epoch).unwrap();
        let other = Connection::open(location.root.join("state.sqlite3")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        store.recover_effects(&epoch);
        assert_eq!(store.effect_status()["error"], "ledger_recovery_failed");
        assert_eq!(store.effect_status()["revoked"], true);
        other.execute_batch("ROLLBACK").unwrap();
        store.recover_effects(&epoch);
        assert!(store.effect_status()["error"].is_null());
        assert_eq!(store.effect_status()["attempts"]["unknown"], 0);
    }
    #[test]
    #[cfg(unix)]
    fn handoff_lock_never_creates_a_leaf_through_a_replaced_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("desktop");
        let foreign = temp.path().join("foreign");
        platform::private_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, &root).unwrap();
        let location = Location {
            root,
            configuration: "fixture".into(),
        };
        assert!(location.handoff_lock().is_err());
        assert!(!foreign.join(HANDOFF_LOCK).exists());
    }
    #[cfg(windows)]
    #[test]
    fn windows_verbatim_locations_keep_identity_and_do_not_create_missing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let mut parent = temp.path().canonicalize().unwrap();
        // Cover both the ordinary short temp path and an extended-length path.
        for long in [false, true] {
            if long {
                for _ in 0..8 {
                    parent.push("synthetic-long-desktop-folder-component");
                }
                fs::create_dir_all(&parent).unwrap();
                assert!(parent.as_os_str().len() > 260);
            }
            let root = fs::canonicalize(&parent).unwrap();
            assert!(
                matches!(root.components().next(), Some(std::path::Component::Prefix(prefix))
                if matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_) | std::path::Prefix::VerbatimUNC(_, _)))
            );
            let text = root.to_str().unwrap();
            let ordinary = if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{share}"))
            } else {
                PathBuf::from(text.strip_prefix(r"\\?\").unwrap())
            };
            let config = |root: &Path| Config {
                home: root.to_str().unwrap().into(),
                dir: root.join("missing-config"),
                state: root.join("missing-state"),
                selves: vec![],
                peers: vec![],
                base_peers: vec![],
                peers_from_environment: false,
                machines: Default::default(),
                tab: false,
                tab_colors: String::new(),
            };
            let direct = Location::new(&config(&root)).unwrap();
            let alias = Location::new(&config(&ordinary)).unwrap();
            assert_eq!(direct.root, root.join("missing-state").join("desktop"));
            assert_eq!(direct.root, alias.root);
            assert_eq!(direct.configuration, alias.configuration);
            assert_eq!(canonical(&root).unwrap(), root);
            assert!(!root.join("missing-state").exists());
            assert!(!root.join("missing-config").exists());
        }
    }

    #[test]
    fn canonical_aliases_share_the_os_lock() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("desktop");
        platform::private_dir(&root).unwrap();
        let location = Location {
            root,
            configuration: "fixture".into(),
        };
        let owner = location.lock().unwrap();
        assert!(location.lock().is_err());
        drop(owner);
        assert!(location.lock().is_ok());
        #[cfg(unix)]
        {
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&location.root, &alias).unwrap();
            assert_eq!(
                canonical(&alias).unwrap(),
                canonical(&location.root).unwrap()
            );
        }
    }
}
