use super::*;
use fs2::FileExt;
const WAL_LIMIT: u64 = 64 << 20;
const WAL_QUOTA: &str = "Display WAL quota reached; background checkpoint required";
fn wal_budget(dir: &Directory) -> anyhow::Result<()> {
    if let Some(file) = checked(dir.read_private_optional("index.sqlite3-wal"))? {
        anyhow::ensure!(file.metadata()?.len() <= WAL_LIMIT, WAL_QUOTA);
    }
    Ok(())
}
fn index_file(dir: &Directory) -> anyhow::Result<File> {
    checked(dir.read_private_optional("index.sqlite3"))?.ok_or_else(|| {
        anyhow::anyhow!("The session listing index is still being built. Try again shortly.")
    })
}
pub(super) struct Store {
    pub db: Connection,
    pub root: Directory,
    selected: PathBuf,
    dir: Directory,
    file: File,
    _gate: Option<File>,
}
/// Only this exact failure precedes every native-feed read in a repair step.
#[derive(Debug)]
pub(super) struct PublisherBusy;
impl std::fmt::Display for PublisherBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Display publisher is busy; retain the background census")
    }
}
impl std::error::Error for PublisherBusy {}
impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_for(path, false)
    }
    fn open_for(path: &Path, checkpoint: bool) -> anyhow::Result<Self> {
        let root = checked(Directory::open(&crate::platform::canonical(path)?))?;
        let dir = checked(root.ensure_child("listing-metadata"))?;
        anyhow::ensure!(
            !dir.path.join("disabled").try_exists()?,
            "Display index is disabled"
        );
        let gate = checked(dir.open_private_lock("writer.lock"))?;
        match gate.try_lock_exclusive() {
            Ok(()) => (),
            Err(error) if crate::platform::lock_contended(&error) => {
                return Err(PublisherBusy.into());
            }
            Err(error) => return Err(error.into()),
        }
        if !checkpoint {
            wal_budget(&dir)?;
        }
        let (file, new) = match checked(dir.read_private_optional("index.sqlite3"))? {
            Some(f) => (f, false),
            None => {
                anyhow::ensure!(
                    checked(dir.read_private_optional("index.sqlite3-wal"))?.is_none()
                        && checked(dir.read_private_optional("index.sqlite3-shm"))?.is_none(),
                    "Orphaned display journal files"
                );
                (checked(dir.write_new_file("index.sqlite3", &[]))?, true)
            }
        };
        let db = Connection::open_with_flags(
            dir.path.join("index.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        if new {
            db.execute_batch("PRAGMA synchronous=FULL; PRAGMA max_page_count=131072;")?;
            db.execute_batch("PRAGMA user_version=3; PRAGMA journal_mode=WAL;
              CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL,root TEXT NOT NULL,epoch TEXT NOT NULL,writes INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 0,generation INTEGER NOT NULL DEFAULT 0,serial INTEGER NOT NULL DEFAULT 0,ready INTEGER NOT NULL DEFAULT 0);
              CREATE TABLE sources(source TEXT PRIMARY KEY,seed TEXT,stamp TEXT,revision INTEGER NOT NULL,tombstone INTEGER NOT NULL CHECK(tombstone IN(0,1)),issue TEXT);
              CREATE TABLE intents(id TEXT PRIMARY KEY,source TEXT NOT NULL,seed TEXT,created REAL NOT NULL);
              CREATE INDEX intents_source ON intents(source);
              CREATE INDEX sources_tombstone ON sources(tombstone,source);
              CREATE INDEX sources_name ON sources(json_extract(seed,'$.record.name')) WHERE tombstone=0;
              CREATE TABLE scan(id INTEGER PRIMARY KEY CHECK(id=1),generation INTEGER NOT NULL,phase INTEGER NOT NULL,folder INTEGER NOT NULL,after_key TEXT NOT NULL,dirs TEXT NOT NULL,writes INTEGER NOT NULL,gap TEXT NOT NULL,problems INTEGER NOT NULL,base INTEGER NOT NULL DEFAULT 0);
              CREATE TABLE queue(generation INTEGER NOT NULL,source TEXT NOT NULL,PRIMARY KEY(generation,source));
              CREATE TABLE dirty(generation INTEGER NOT NULL,source TEXT NOT NULL,PRIMARY KEY(generation,source));
              CREATE TABLE snapshots(generation INTEGER NOT NULL,source TEXT NOT NULL,seed TEXT NOT NULL,stamp TEXT,PRIMARY KEY(generation,source));
              CREATE INDEX snapshots_session ON snapshots(generation,json_extract(seed,'$.record.session_id'));
              CREATE INDEX snapshots_conversation ON snapshots(generation,json_extract(seed,'$.record.conversation_id'));
              CREATE INDEX snapshots_journal ON snapshots(generation,coalesce(json_extract(seed,'$.record.journal_name'),json_extract(seed,'$.record.name')));
              CREATE TABLE generations(generation INTEGER PRIMARY KEY,complete INTEGER NOT NULL,dirs TEXT NOT NULL,gap TEXT NOT NULL,high_water INTEGER NOT NULL DEFAULT 0);
              CREATE INDEX generations_complete ON generations(complete,generation);")?;
            db.execute(
                "INSERT INTO meta(id,version,root,epoch) VALUES(1,1,?1,?2)",
                params![
                    serde_json::to_string(&checked(root.object())?)?,
                    uuid::Uuid::new_v4().to_string()
                ],
            )?;
        }
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(version != 0, "Incomplete display cache schema");
        anyhow::ensure!(
            !matches!(version, 1 | 2),
            "Display reader schema requires background rebuild"
        );
        anyhow::ensure!(version == 3, "Unknown display cache version");
        let identity: String =
            db.query_row("SELECT root FROM meta WHERE id=1 AND version=1", [], |r| {
                r.get(0)
            })?;
        anyhow::ensure!(
            identity == serde_json::to_string(&checked(root.object())?)?,
            "Display root/store identity changed"
        );
        if !new {
            db.execute_batch("PRAGMA synchronous=FULL; PRAGMA max_page_count=131072;")?;
        }
        let store = Self {
            db,
            root,
            selected: path.to_path_buf(),
            dir,
            file,
            _gate: Some(gate),
        };
        store.verify()?;
        Ok(store)
    }

    /// A pinned reader never takes the advisory writer gate or initializes
    /// schema. WAL readers cannot force ordinary publishers to lose coverage.
    pub(super) fn read(path: &Path) -> anyhow::Result<Self> {
        Self::read_with_busy_timeout(path, Duration::ZERO)
    }

    pub(super) fn read_listing(path: &Path) -> anyhow::Result<Self> {
        // SQLite can briefly hold an exclusive lock during WAL recovery.
        // Give the foreground admission read time to obtain its snapshot.
        Self::read_with_busy_timeout(path, Duration::from_millis(100))
    }

    fn read_with_busy_timeout(path: &Path, timeout: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(enabled(path), "Display index is disabled");
        let root = checked(Directory::open(&crate::platform::canonical(path)?))?;
        let dir = checked(root.child("listing-metadata"))?;
        wal_budget(&dir)?;
        let file = index_file(&dir)?;
        let db = Connection::open_with_flags(
            dir.path.join("index.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(timeout)?;
        db.execute_batch("BEGIN DEFERRED")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(version == 3, "Display snapshot requires metadata repair");
        let identity: String =
            db.query_row("SELECT root FROM meta WHERE id=1 AND version=1", [], |r| {
                r.get(0)
            })?;
        anyhow::ensure!(
            identity == serde_json::to_string(&checked(root.object())?)?,
            "Display root/store identity changed"
        );
        let store = Self {
            db,
            root,
            selected: path.to_path_buf(),
            dir,
            file,
            _gate: None,
        };
        store.verify()?;
        Ok(store)
    }
    pub fn verify(&self) -> anyhow::Result<()> {
        checked(self.root.verify())?;
        if self.selected != self.root.path {
            anyhow::ensure!(
                crate::platform::canonical(&self.selected)? == self.root.path,
                "Selected display root changed"
            );
        }
        checked(self.dir.verify())?;
        let current = index_file(&self.dir)?;
        let held = crate::platform::FileIdentity::of(&self.file)?;
        let at = crate::platform::FileIdentity::of(&current)?;
        anyhow::ensure!(
            (held.device, held.inode) == (at.device, at.inode),
            "Display index namespace changed"
        );
        Ok(())
    }
    pub fn reserve(&mut self, key: &Key, seed: Option<&str>) -> anyhow::Result<String> {
        self.verify()?;
        key.validate()?;
        let id = uuid::Uuid::new_v4().to_string();
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("UPDATE meta SET writes=writes+1,ready=0 WHERE id=1", [])?;
        let outstanding: i64 = tx.query_row("SELECT count(*) FROM intents", [], |r| r.get(0))?;
        anyhow::ensure!(outstanding < 4096, "Display intent capacity exceeded");
        tx.execute(
            "INSERT INTO intents VALUES(?1,?2,?3,?4)",
            params![id, key.0, seed, now()],
        )?;
        tx.commit()?;
        self.verify()?;
        Ok(id)
    }
    pub fn settle(&mut self, id: &str, stamp: Option<&str>) -> anyhow::Result<()> {
        self.verify()?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (source, seed): (String, Option<String>) =
            tx.query_row("SELECT source,seed FROM intents WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        tx.execute("UPDATE meta SET revision=revision+1 WHERE id=1", [])?;
        tx.execute("INSERT INTO sources(source,seed,stamp,revision,tombstone,issue) VALUES(?1,?2,?3,(SELECT revision FROM meta WHERE id=1),?4,NULL)
          ON CONFLICT(source) DO UPDATE SET seed=excluded.seed,stamp=excluded.stamp,revision=excluded.revision,tombstone=excluded.tombstone,issue=NULL",params![source,seed,stamp,i64::from(stamp.is_none())])?;
        tx.execute("DELETE FROM intents WHERE id=?1", [id])?;
        tx.commit()?;
        self.verify()
    }
    pub fn source_parent(&self, key: &Key) -> anyhow::Result<(Directory, String)> {
        key.validate()?;
        let mut parent = checked(Directory::open(&self.root.path))?;
        let path = Path::new(&key.0);
        for part in path.parent().unwrap().components() {
            parent = checked(
                parent.child(
                    part.as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("Invalid display source"))?,
                ),
            )?;
        }
        Ok((parent, path.file_name().unwrap().to_str().unwrap().into()))
    }
    pub fn source_stamp(&self, key: &Key) -> anyhow::Result<Option<String>> {
        let (parent, name) = self.source_parent(key)?;
        let stamp = match crate::platform::no_follow(std::fs::OpenOptions::new().read(true))
            .open(parent.path.join(name))
        {
            Ok(file) => Some(version(&file)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        checked(parent.verify())?;
        Ok(stamp)
    }
}
pub(super) fn version(file: &File) -> anyhow::Result<String> {
    let (id, m) = crate::platform::FileIdentity::with_metadata(file)?;
    stamp(&id, &m)
}
pub(super) fn publication_version(file: &File) -> anyhow::Result<(String, String)> {
    let (id, m) = crate::platform::FileIdentity::with_metadata(file)?;
    let stamp = stamp(&id, &m)?;
    // Rename can change ctime. The private written object's identity, length
    // and mtime must remain unchanged across its publication, before a full
    // post-publication stamp is certified. Both come from this one fstat.
    let stable =
        serde_json::to_string(&(id.device, id.inode, id.modified, id.modified_nanos, m.len()))?;
    Ok((stable, stamp))
}
fn stamp(id: &crate::platform::FileIdentity, m: &std::fs::Metadata) -> anyhow::Result<String> {
    anyhow::ensure!(m.is_file(), "Display source is not regular");
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        (m.ctime(), m.ctime_nsec())
    };
    #[cfg(not(unix))]
    let changed = (0, 0);
    Ok(serde_json::to_string(&(
        id.device,
        id.inode,
        id.modified,
        id.modified_nanos,
        m.len(),
        changed,
    ))?)
}

impl Store {
    /// Recover only this component's disposable cache, never canonical files.
    /// One retained cache set bounds quarantine storage across repeated faults.
    pub fn recover(path: &Path, error: &anyhow::Error) -> anyhow::Result<bool> {
        if error.to_string() == WAL_QUOTA {
            // Only the background owner may checkpoint. A pinned reader makes
            // this nonblocking attempt report busy; canonical writers do not wait.
            let store = match Self::open_for(path, true) {
                Ok(store) => store,
                Err(error)
                    if error.to_string() == "Display reader schema requires background rebuild" =>
                {
                    return Self::recover(path, &error);
                }
                Err(error) => return Err(error),
            };
            let (busy, _, _): (i64, i64, i64) =
                store
                    .db
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
            store.verify()?;
            return Ok(busy == 0);
        }
        let eligible = match error.downcast_ref::<rusqlite::Error>() {
            Some(rusqlite::Error::SqliteFailure(code, message)) => {
                let code = code.extended_code & 0xff;
                code == rusqlite::ffi::SQLITE_CORRUPT
                    || code == rusqlite::ffi::SQLITE_NOTADB
                    || (code == rusqlite::ffi::SQLITE_ERROR
                        && message
                            .as_deref()
                            .is_some_and(|m| m.starts_with("no such table:")))
            }
            Some(rusqlite::Error::QueryReturnedNoRows) => true,
            _ => matches!(
                error.to_string().as_str(),
                "Display root/store identity changed"
                    | "Orphaned display journal files"
                    | "Incomplete display cache schema"
                    | "Display reader schema requires background rebuild"
            ),
        };
        if !eligible {
            return Ok(false);
        }
        let root = checked(Directory::open(&crate::platform::canonical(path)?))?;
        let dir = checked(root.child("listing-metadata"))?;
        anyhow::ensure!(
            !dir.path.join("disabled").try_exists()?,
            "Display index is disabled"
        );
        let gate = checked(dir.open_private_lock("writer.lock"))?;
        gate.try_lock_exclusive()?;
        if checked(dir.read_private_optional("index.sqlite3"))?.is_some() {
            if let Ok(db) = Connection::open_with_flags(
                dir.path.join("index.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            ) {
                if let Ok(version) = db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                {
                    anyhow::ensure!(
                        matches!(version, 0..=3),
                        "Unknown display cache version; rebuild refused"
                    );
                }
            }
        }
        for name in ["index.sqlite3", "index.sqlite3-wal", "index.sqlite3-shm"] {
            checked(root.verify())?;
            checked(dir.verify())?;
            let Some(file) = checked(dir.read_private_optional(name))? else {
                continue;
            };
            let backup = format!("retained-{name}");
            if checked(dir.read_private_optional(&backup))?.is_some() {
                std::fs::remove_file(dir.path.join(&backup))?;
            }
            let current = checked(dir.read_private(name))?;
            anyhow::ensure!(
                crate::platform::FileIdentity::of(&file)?
                    == crate::platform::FileIdentity::of(&current)?,
                "Display cache changed before rebuild"
            );
            std::fs::rename(dir.path.join(name), dir.path.join(backup))?;
            crate::platform::sync_dir(&dir.path)?;
        }
        checked(root.verify())?;
        checked(dir.verify())?;
        Ok(true)
    }
}
