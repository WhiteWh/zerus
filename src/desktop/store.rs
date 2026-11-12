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
#[path = "rollback_backup.rs"]
pub(super) mod rollback_backup;
#[path = "store_startup.rs"]
mod startup;

pub const SCHEMA: i64 = super::question_route::SCHEMA;
pub struct HandoffLock {
    _lock: platform::FileLock,
    root: PathBuf,
    configuration: String,
}
impl HandoffLock {
    /// The identity of the exact lock file object held.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn identity(&self) -> std::io::Result<platform::FileIdentity> {
        self._lock.identity()
    }
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
    let worktrees = if version >= super::creation_worktree::SCHEMA {
        super::creation_worktree::undrained(db)?
    } else {
        0
    };
    let launches = if version >= super::creation_launch::SCHEMA {
        super::creation_launch::undrained(db)?
    } else {
        0
    };
    let messages = if version >= super::task_message::SCHEMA {
        super::task_message::undrained(db)?
    } else {
        0
    };
    let sends = if version >= super::deferred_send::SCHEMA {
        super::deferred_send::undrained(db)?
    } else {
        0
    };
    let answers = if version >= super::deferred_answer::SCHEMA {
        super::deferred_answer::undrained(db)?
    } else {
        0
    };
    let compactions = if version >= super::compact_to_send::SCHEMA {
        super::compact_to_send::undrained(db)?
    } else {
        0
    };
    let resumes = if version >= super::session_resume::SCHEMA {
        super::session_resume::undrained(db)?
    } else {
        0
    };
    task.checked_add(clones)
        .and_then(|count| count.checked_add(cache))
        .and_then(|count| count.checked_add(worktrees))
        .and_then(|count| count.checked_add(launches))
        .and_then(|count| count.checked_add(messages))
        .and_then(|count| count.checked_add(sends))
        .and_then(|count| count.checked_add(answers))
        .and_then(|count| count.checked_add(compactions))
        .and_then(|count| count.checked_add(resumes))
        .context("operation resource total overflow")
}

pub struct Store {
    pub(super) db: Connection,
    // On Windows the connection closes before its original handle. Unix's
    // registry keeps that one original descriptor for the whole process.
    _startup_pin: Option<std::sync::Arc<fs::File>>,
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    worktree_custody: super::creation_worktree::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    launch_custody: super::creation_launch::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    message_custody: super::task_message::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    send_custody: super::deferred_send::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    answer_custody: super::deferred_answer::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    compact_custody: super::compact_to_send::Recovery,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    resume_custody: super::session_resume::Recovery,
}
impl Store {
    /// All production callers retain their same real service-owner guard.
    /// Handoff is needed for existing-data migration, not passive startup.
    pub fn open_owned(
        location: &Location,
        epoch: &str,
        owner: &platform::FileLock,
    ) -> Result<Self> {
        let startup = startup::Startup::observe(location, owner)?;
        let handoff = match startup.mode() {
            startup::Mode::Migrate(_) => Some(location.handoff_lock()?),
            _ => None,
        };
        let original = handoff
            .as_ref()
            .map(|held| rollback_backup::Original::new(location, owner, held))
            .transpose()?;
        startup.reobserve()?;
        if let (Some(original), startup::Mode::Migrate(version)) = (&original, startup.mode()) {
            #[cfg(windows)]
            original.prepare(Some(version)).context("preserve desktop rollback backup before schema 35; keep the backup for review and see docs/desktop-schema-35-rollback.md")?;
            #[cfg(not(windows))]
            original
                .prepare(Some(version))
                .context("preserve desktop rollback backup before schema 35")?;
        }
        let store = Self::open_inner(location, epoch, original.as_ref(), Some(&startup))?;
        startup.verify()?;
        if let Some(original) = original {
            original.verify()?;
        }
        Ok(store)
    }
    // Unit fixtures without a service still exercise the migration itself;
    // rollback controls use open_owned with both real original locks.
    #[cfg(test)]
    pub fn open(location: &Location, epoch: &str) -> Result<Self> {
        Self::open_inner(location, epoch, None, None)
    }
    fn open_inner(
        location: &Location,
        epoch: &str,
        original: Option<&rollback_backup::Original<'_>>,
        startup: Option<&startup::Startup<'_>>,
    ) -> Result<Self> {
        let path = location.root.join("state.sqlite3");
        database_files(location)?;
        let flags = OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        // A newer schema is probed read-only before any PRAGMA or migration write.
        if startup.is_none() {
            schema_before_write(&path, SCHEMA)?;
        }
        // Production's original RO observations have fully closed. Backup
        // positional reads used the retained pin; other copy handles closed.
        // On Unix, closing any descriptor of this file drops this process's
        // POSIX locks on it, including those of every live SQLite connection
        // to it. So the file is created and checked through its path only, and
        // the connection is bound to that file after the open (zerus-hpat).
        #[cfg(unix)]
        let identity = {
            if startup.is_none() {
                crate::sqlite_file::create(&path)?;
            }
            crate::sqlite_file::private(&path).map_err(|_| {
                anyhow::anyhow!("desktop state must be private single-link regular files")
            })?
        };
        // Windows locks belong to the handle that took them.
        #[cfg(not(unix))]
        if startup.is_none() {
            drop(private_file(&path, true)?);
        }
        #[cfg(all(test, unix))]
        around_open(&path, false);
        let mut db = Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        #[cfg(all(test, unix))]
        around_open(&path, true);
        #[cfg(unix)]
        crate::sqlite_file::bind_private(&db, &path, identity).context("desktop state changed")?;
        super::counts::register(&db)?;
        db.busy_timeout(Duration::from_secs(2))?;
        // Validate the empty shape before SQLite itself creates its rollback
        // journal. The deferred snapshot and its upgrade remain one atomic
        // transaction; a competing writer refuses the upgrade, never adopts.
        let initial = db.transaction()?;
        if let Some(startup) = startup {
            startup.bind(&initial)?;
        }
        let version: i64 = initial.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            let tables: i64 = initial.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
                [],
                |row| row.get(0),
            )?;
            ensure!(tables == 0, "unversioned desktop state is not empty");
            initial.execute_batch("CREATE TABLE service (id INTEGER PRIMARY KEY CHECK(id=1), configuration TEXT NOT NULL, generation INTEGER NOT NULL, epoch TEXT NOT NULL); PRAGMA user_version=1;")?;
            initial.execute(
                "INSERT INTO service VALUES(1,?1,0,'')",
                [&location.configuration],
            )?;
        }
        let configuration: String =
            initial.query_row("SELECT configuration FROM service WHERE id=1", [], |row| {
                row.get(0)
            })?;
        ensure!(
            configuration == location.configuration,
            "desktop state belongs to another configuration"
        );
        initial.commit()?;
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
        if version < super::attention::SCHEMA {
            let tx = db.transaction()?;
            super::attention::migrate_retention(&tx)?;
            tx.pragma_update(None, "user_version", super::attention::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::catalog_read::SCHEMA {
            let tx = db.transaction()?;
            zerus_desktop_catalog::changes::migrate_reasons(&tx)?;
            tx.pragma_update(None, "user_version", super::catalog_read::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::delivery::SCHEMA {
            // Adds the column and indexes only; the delivery scan derives the
            // flag of existing rows later in bounded batches.
            let tx = db.transaction()?;
            zerus_desktop_catalog::approvals::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::delivery::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::creation_worktree::SCHEMA {
            let tx = db.transaction()?;
            super::creation_worktree::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::creation_worktree::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::creation_launch::SCHEMA {
            let tx = db.transaction()?;
            super::creation_launch::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::creation_launch::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::termination_outcome::SCHEMA {
            let tx = db.transaction()?;
            super::termination_outcome::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::termination_outcome::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::task_message::SCHEMA {
            let tx = db.transaction()?;
            super::task_message::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::task_message::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::task_message::INPUTS_SCHEMA {
            let tx = db.transaction()?;
            super::task_message::migrate_inputs(&tx)?;
            tx.pragma_update(None, "user_version", super::task_message::INPUTS_SCHEMA)?;
            tx.commit()?;
        }
        if version < super::deferred_send::SCHEMA {
            let tx = db.transaction()?;
            super::deferred_send::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::deferred_send::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::deferred_answer::SCHEMA {
            let tx = db.transaction()?;
            super::deferred_answer::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::deferred_answer::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::compact_to_send::SCHEMA {
            let tx = db.transaction()?;
            super::compact_to_send::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::compact_to_send::SCHEMA)?;
            tx.commit()?;
        }
        if version < 31 {
            let tx = db.transaction()?;
            zerus_desktop_ledger::diagnostics::migrate(&tx)?;
            tx.pragma_update(None, "user_version", 31)?;
            tx.commit()?;
        }
        if version < super::attention::STARTUP_SCHEMA {
            let tx = db.transaction()?;
            super::attention::migrate_startup(&tx)?;
            tx.pragma_update(None, "user_version", super::attention::STARTUP_SCHEMA)?;
            tx.commit()?;
        }
        if version < super::delivery::CATEGORIES_SCHEMA {
            let tx = db.transaction()?;
            super::delivery::migrate_categories(&tx)?;
            tx.pragma_update(None, "user_version", super::delivery::CATEGORIES_SCHEMA)?;
            tx.commit()?;
        }
        if version < super::session_resume::SCHEMA {
            let tx = db.transaction()?;
            super::session_resume::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::session_resume::SCHEMA)?;
            tx.commit()?;
        }
        if version < super::creation_worktree::SLOTS_SCHEMA {
            if let Some(original) = original {
                original.verify()?;
            }
            let tx = db.transaction()?;
            super::creation_worktree::migrate_slots(&tx)?;
            tx.pragma_update(None, "user_version", super::creation_worktree::SLOTS_SCHEMA)?;
            if let Some(original) = original {
                original.verify()?;
            }
            tx.commit()?;
        }
        if version < super::question_route::SCHEMA {
            let tx = db.transaction()?;
            super::question_route::migrate(&tx)?;
            tx.pragma_update(None, "user_version", super::question_route::SCHEMA)?;
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
        // Either creation kind failing recovery pauses creation admission.
        let creation_recovery_error = super::creation_clone::startup(&mut db).is_err()
            | super::creation_worktree::startup(&mut db).is_err()
            | super::creation_launch::startup(&mut db).is_err()
            | super::task_message::startup(&mut db).is_err()
            | super::deferred_send::startup(&mut db).is_err()
            | super::deferred_answer::startup(&mut db).is_err()
            | super::compact_to_send::startup(&mut db).is_err()
            | super::session_resume::startup(&mut db).is_err();
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
            _startup_pin: startup.map(|startup| startup.pin().file()),
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
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            worktree_custody: super::creation_worktree::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            launch_custody: super::creation_launch::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            message_custody: super::task_message::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            send_custody: super::deferred_send::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            answer_custody: super::deferred_answer::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            compact_custody: super::compact_to_send::Recovery::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            resume_custody: super::session_resume::Recovery::default(),
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
        let worktree = super::creation_worktree::summary(&self.db);
        let launch = super::creation_launch::summary(&self.db);
        let message = super::task_message::summary(&self.db);
        let send = super::deferred_send::summary(&self.db);
        let answer = super::deferred_answer::summary(&self.db);
        let compact = super::compact_to_send::summary(&self.db);
        let resume = super::session_resume::summary(&self.db);
        let mut status = match zerus_desktop_ledger::operations::totals(&self.db).and_then(
            |totals| {
                let summary = |value: &Result<serde_json::Value>| match value {
                    Ok(value) => Ok(value.clone()),
                    Err(_) => Err(anyhow::anyhow!("creation summary unavailable")),
                };
                Ok((
                    totals,
                    operation_undrained(&self.db)?,
                    summary(&clone)?,
                    summary(&worktree)?,
                    summary(&launch)?,
                    summary(&message)?,
                    summary(&send)?,
                    summary(&answer)?,
                    summary(&compact)?,
                    summary(&resume)?,
                ))
            },
        ) {
            Ok((
                totals,
                undrained,
                clone,
                worktree,
                launch,
                message,
                send,
                answer,
                compact,
                resume,
            )) => {
                let unsettled = checked_unsettled(totals.unsettled,
                    [&clone,&worktree,&launch,&message,&send,&answer,&compact,&resume]);
                serde_json::json!({"unsettled":unsettled,"undrained":undrained,
                "kinds":{"task_start_reconcile":{"unsettled":totals.unsettled,"undrained":totals.undrained},"creation_clone":clone,"creation_worktree":worktree,"creation_launch":launch,"task_message":message,"deferred_send":send,"deferred_answer":answer,"compact_to_send":compact,"session_resume":resume},
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
        let retained = |summary: &Result<serde_json::Value>| {
            summary
                .as_ref()
                .ok()
                .is_some_and(|v| v["undrained"].as_u64().is_some_and(|n| n > 0))
        };
        if retained(&clone)
            || retained(&worktree)
            || retained(&launch)
            || retained(&message)
            || retained(&send)
            || retained(&answer)
            || retained(&compact)
            || retained(&resume)
        {
            let task = status["resource_custody"].take();
            status["resource_custody"] = serde_json::json!({"state":"retained","creation_clone":clone.ok(),
                "creation_worktree":worktree.ok(),"creation_launch":launch.ok(),"task_message":message.ok(),"deferred_send":send.ok(),"deferred_answer":answer.ok(),"compact_to_send":compact.ok(),"session_resume":resume.ok(),
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn operation_recovery_ok(&self) -> bool {
        !self.operation_recovery_error && !self.creation_recovery_error
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.worktree_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.launch_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.message_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.send_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.answer_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.compact_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.resume_custody.tick(location, &mut self.db).is_err() {
            self.creation_recovery_error = true;
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = location;
    }
    /// The deferred sends a live runtime drains itself; Store recovery skips
    /// them while their token lives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn send_owned(&self) -> super::deferred_send::Owned {
        self.send_custody.owned()
    }
    /// The deferred answers a live runtime drains itself; Store recovery
    /// skips them while their token lives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn answer_owned(&self) -> super::deferred_answer::Owned {
        self.answer_custody.owned()
    }
    /// The compact-to-sends a live runtime drains itself; Store recovery
    /// skips them while their token lives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn compact_owned(&self) -> super::compact_to_send::Owned {
        self.compact_custody.owned()
    }
    /// The resume-then-sends a live runtime drains itself; Store recovery
    /// skips them while their token lives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn resume_owned(&self) -> super::session_resume::Owned {
        self.resume_custody.owned()
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

// Kept separate so the schema-34 ceiling can be exercised against a real
// migrated store. This is the same read-only refusal older binaries use.
fn schema_before_write(path: &Path, maximum: i64) -> Result<Option<i64>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let probe = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let version: i64 = probe.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure!(
        (0..=maximum).contains(&version),
        "desktop state requires a newer hgs version"
    );
    Ok(Some(version))
}

/// The swap test's hook: runs after the private check (`false`) and after
/// SQLite's open (`true`).
#[cfg(all(test, unix))]
type AroundOpen = Box<dyn FnMut(&Path, bool)>;

#[cfg(all(test, unix))]
thread_local! {
    static AROUND_OPEN: std::cell::RefCell<Option<AroundOpen>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
fn around_open(path: &Path, opened: bool) {
    AROUND_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, opened);
        }
    });
}

fn checked_unsettled<'a>(initial: u64, leaves: impl IntoIterator<Item=&'a serde_json::Value>) -> Option<u64> {
    leaves.into_iter().try_fold(initial, |sum, leaf| sum.checked_add(leaf["unsettled"].as_u64()?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn incomplete_or_invalid_operation_counts_never_become_zero() {
        use serde_json::json;
        assert_eq!(super::checked_unsettled(3,[&json!({"unsettled":2})]),Some(5));
        for leaf in [json!({}),json!({"unsettled":null}),json!({"unsettled":-1}),json!({"unsettled":0.5}),json!({"unsettled":"0"})] {
            assert_eq!(super::checked_unsettled(3,[&leaf]),None);
        }
        assert_eq!(super::checked_unsettled(u64::MAX,[&json!({"unsettled":1})]),None);
        assert_eq!(super::checked_unsettled(u64::MAX,[&json!({"unsettled":0})]),Some(u64::MAX));
    }
    use super::*;
    use rusqlite::params;

    /// review-q's S2 on #1349, the A-B-A swap: the path names the checked
    /// database A at the private check and again at the bind, but SQLite
    /// opened B in between. Only `bind_private` (SQLite's HAS_MOVED) can tell.
    #[cfg(unix)]
    #[test]
    fn opening_refuses_a_database_swapped_in_during_the_open() {
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let _owner = location.lock().unwrap();
        let epoch = uuid::Uuid::new_v4().to_string();
        drop(Store::open(&location, &epoch).unwrap());
        let aside = |name: &str| temp.path().join(name);
        AROUND_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |path: &Path, opened: bool| {
                let aside = |name: &str| {
                    path.parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(name)
                };
                if opened {
                    std::fs::rename(path, aside("b.sqlite3")).unwrap();
                    std::fs::rename(aside("a.sqlite3"), path).unwrap();
                } else {
                    std::fs::rename(path, aside("a.sqlite3")).unwrap();
                    crate::sqlite_file::create(path).unwrap();
                }
            }));
        });
        let swapped = Store::open(&location, &epoch);
        AROUND_OPEN.with(|hook| hook.borrow_mut().take());
        let error = format!(
            "{:#}",
            swapped.err().expect("the swapped-in database is refused")
        );
        assert!(error.contains("desktop state changed"), "{error}");
        // A is back at its path, unchanged, and opens again.
        assert!(aside("b.sqlite3").exists());
        Store::open(&location, &epoch).unwrap();
    }
    /// zerus-hpat: opening the store again keeps the SQLite locks of a store
    /// already open in this process, also after a restart: no raw descriptor
    /// of the live database is closed.
    #[cfg(unix)]
    #[test]
    fn opening_the_store_again_keeps_a_live_store_s_locks() {
        use crate::sqlite_file::foreign_lock_refused;
        let temp = tempfile::tempdir().unwrap();
        let location = Location {
            root: canonical(&temp.path().join("state/desktop")).unwrap(),
            configuration: "a".repeat(64),
        };
        location.prepare().unwrap();
        let _owner = location.lock().unwrap();
        let path = location.root.join("state.sqlite3");
        let epoch = uuid::Uuid::new_v4().to_string();
        // The second round is a restart: every earlier store is gone.
        for round in 0..2 {
            let live = Store::open(&location, &epoch).unwrap();
            live.db.execute_batch("BEGIN").unwrap();
            let _: i64 = live
                .db
                .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
                .unwrap();
            assert!(
                foreign_lock_refused(&path),
                "round {round}: the store holds its lock"
            );
            drop(Store::open(&location, &epoch));
            assert!(
                foreign_lock_refused(&path),
                "round {round}: another open kept the lock"
            );
            live.db.execute_batch("COMMIT").unwrap();
            drop(live);
            assert!(
                !foreign_lock_refused(&path),
                "round {round}: the lock went with the store"
            );
        }
    }

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
