//! Current Codex child catalogs for the guarded picker, without native SQLite
//! side effects. Ordinary reconciliation holds the read side of this exclusion;
//! a snapshot holds the write side, so no native connection can coexist with
//! its raw source descriptors (the PR1332 lock-closing trap).
use super::*;
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, sync::RwLock};

static NATIVE_SQLITE: RwLock<()> = RwLock::new(());

pub(super) struct NativeConnection {
    // Close SQLite before releasing the shared native-connection exclusion.
    db: Connection,
    _guard: std::sync::RwLockReadGuard<'static, ()>,
}
impl std::ops::Deref for NativeConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.db
    }
}
impl NativeConnection {
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let guard = NATIVE_SQLITE
            .read()
            .map_err(|_| "Native SQLite exclusion is unavailable".to_owned())?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self { db, _guard: guard })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod snapshot {
    use super::*;
    use rusqlite::OptionalExtension;
    use std::{
        collections::BTreeMap,
        fs,
        io::Write,
        path::Path,
        sync::TryLockError,
        time::{Duration, Instant},
    };

    const MAX_CHILDREN: usize = 64;
    const MAX_PATH: usize = 4096;
    const ATTEMPTS: usize = 3;
    pub(in crate::state) const MAX_SOURCE: u64 = 32 * 1024 * 1024;

    #[derive(Debug)]
    enum SnapshotError {
        Changed,
        Missing,
        Failed(String),
    }
    type SnapshotResult<T> = std::result::Result<T, SnapshotError>;

    impl std::fmt::Display for SnapshotError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Changed => formatter.write_str("Codex catalog changed"),
                Self::Missing => formatter.write_str("Codex catalog is missing"),
                Self::Failed(error) => formatter.write_str(error),
            }
        }
    }

    fn failed(error: impl std::fmt::Display) -> SnapshotError {
        SnapshotError::Failed(error.to_string())
    }
    fn live(until: Instant) -> SnapshotResult<()> {
        if Instant::now() >= until {
            Err(failed("Codex catalog read deadline expired"))
        } else {
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Version {
        identity: crate::platform::FileIdentity,
        bytes: u64,
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Source {
        version: Version,
        bytes: Vec<u8>,
    }

    #[cfg(test)]
    pub(in crate::state) static RAW_OPENS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn open_source(path: &Path) -> SnapshotResult<fs::File> {
        #[cfg(test)]
        RAW_OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(unix)]
        let opened = crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path);
        #[cfg(windows)]
        let opened = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows_sys::Win32::Storage::FileSystem::*;
            let file = crate::platform::refuse_link(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE),
            )
            .open(path);
            match file {
                Ok(file) => {
                    let metadata = file.metadata().map_err(failed)?;
                    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        return Err(failed("Codex catalog link is refused"));
                    }
                    Ok(file)
                }
                Err(error) => Err(error),
            }
        };
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SnapshotError::Missing)
            }
            Err(error) => return Err(failed(error)),
        };
        let metadata = file.metadata().map_err(failed)?;
        if !metadata.is_file() {
            return Err(failed("Codex catalog is not a regular file"));
        }
        Ok(file)
    }

    fn version(file: &fs::File) -> SnapshotResult<Version> {
        let (identity, metadata) =
            crate::platform::FileIdentity::with_metadata(file).map_err(failed)?;
        Ok(Version {
            identity,
            bytes: metadata.len(),
        })
    }

    fn read_source(path: &Path, until: Instant) -> SnapshotResult<Source> {
        let file = open_source(path)?;
        let start = version(&file)?;
        if start.bytes > MAX_SOURCE {
            return Err(failed("Codex catalog exceeds its byte bound"));
        }
        let mut bytes = vec![0; start.bytes as usize];
        let mut done = 0;
        while done < bytes.len() {
            live(until)?;
            let end = (done + 1024 * 1024).min(bytes.len());
            #[cfg(unix)]
            let result = {
                use std::os::unix::fs::FileExt;
                file.read_at(&mut bytes[done..end], done as u64)
            };
            #[cfg(windows)]
            let result = {
                use std::os::windows::fs::FileExt;
                file.seek_read(&mut bytes[done..end], done as u64)
            };
            match result {
                Ok(0) => return Err(SnapshotError::Changed),
                Ok(read) => done += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                Err(error) => return Err(failed(error)),
            }
        }
        let end = version(&file)?;
        if start != end {
            return Err(SnapshotError::Changed);
        }
        Ok(Source {
            version: start,
            bytes,
        })
    }

    fn read_wal(path: &Path, until: Instant) -> SnapshotResult<Option<Source>> {
        match read_source(path, until) {
            Ok(source) => Ok(Some(source)),
            Err(SnapshotError::Missing) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn hot_journal(home: &Path, until: Instant) -> SnapshotResult<Option<Version>> {
        live(until)?;
        match open_source(&home.join("state_5.sqlite-journal")) {
            Ok(file) => Ok(Some(version(&file)?)),
            Err(SnapshotError::Missing) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn page_size(source: &Source) -> SnapshotResult<usize> {
        let bytes = &source.bytes;
        if bytes.len() < 100 || !bytes.starts_with(b"SQLite format 3\0") {
            return Err(failed("Codex catalog is not SQLite"));
        }
        let raw = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
        let size = if raw == 1 { 65536 } else { raw };
        if !(512..=65536).contains(&size)
            || !size.is_power_of_two()
            || !bytes.len().is_multiple_of(size)
        {
            return Err(failed("Codex catalog page layout is invalid"));
        }
        if bytes[18] != 2 || bytes[19] != 2 {
            return Err(failed("Codex catalog is not in WAL mode"));
        }
        Ok(size)
    }

    fn word(bytes: &[u8], little: bool) -> u32 {
        let bytes: [u8; 4] = bytes.try_into().unwrap();
        if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    }
    fn checksum(bytes: &[u8], little: bool, first: &mut u32, second: &mut u32) {
        for pair in bytes.as_chunks::<8>().0 {
            let left = word(&pair[..4], little);
            let right = word(&pair[4..], little);
            *first = first.wrapping_add(left).wrapping_add(*second);
            *second = second.wrapping_add(right).wrapping_add(*first);
        }
    }
    fn stored(bytes: &[u8]) -> u32 {
        u32::from_be_bytes(bytes.try_into().unwrap())
    }

    /// Validates the WAL as SQLite reads it and returns the length of its
    /// committed content: the header and the current generation's frames up to
    /// its last commit frame. A frame with another salt ends the generation: a
    /// WAL restarted after a complete checkpoint keeps the earlier generation's
    /// frames after its own, and SQLite ignores them. Frames after the last
    /// commit frame belong to a transaction still being written and are not
    /// content. Inside committed content (a frame that commits, or one a later
    /// frame of the generation commits) a zero page or a failed cumulative
    /// checksum is refused (zerus-wa90v).
    fn validate_wal(
        source: &Source,
        expected_page: usize,
        until: Instant,
    ) -> SnapshotResult<usize> {
        live(until)?;
        let bytes = &source.bytes;
        if bytes.is_empty() {
            return Ok(0);
        }
        if bytes.len() < 32 {
            return Err(SnapshotError::Changed);
        }
        let magic = stored(&bytes[..4]);
        if ![0x377f0682, 0x377f0683].contains(&magic)
            || stored(&bytes[4..8]) != 3_007_000
            || stored(&bytes[8..12]) as usize != expected_page
        {
            return Err(failed("Codex WAL header is invalid"));
        }
        let little = magic == 0x377f0682;
        let (mut first, mut second) = (0, 0);
        checksum(&bytes[..24], little, &mut first, &mut second);
        if first != stored(&bytes[24..28]) || second != stored(&bytes[28..32]) {
            return Err(failed("Codex WAL header checksum failed"));
        }
        let salt = &bytes[16..24];
        let frame = 24 + expected_page;
        if !(bytes.len() - 32).is_multiple_of(frame) {
            return Err(SnapshotError::Changed);
        }
        let generation: Vec<&[u8]> = bytes[32..]
            .chunks_exact(frame)
            .take_while(|header| &header[8..16] == salt)
            .collect();
        let commits = |frames: &[&[u8]]| frames.iter().any(|header| stored(&header[4..8]) != 0);
        let mut committed = 32;
        for (index, header) in generation.iter().enumerate() {
            live(until)?;
            let refused = if stored(&header[..4]) == 0 {
                Some("Codex WAL frame identity is invalid")
            } else {
                checksum(&header[..8], little, &mut first, &mut second);
                checksum(&header[24..], little, &mut first, &mut second);
                (first != stored(&header[16..20]) || second != stored(&header[20..24]))
                    .then_some("Codex WAL frame checksum failed")
            };
            if let Some(reason) = refused {
                if commits(&generation[index..]) {
                    return Err(failed(reason));
                }
                break;
            }
            if stored(&header[4..8]) != 0 {
                committed = 32 + (index + 1) * frame;
            }
        }
        Ok(committed)
    }

    fn write_copy(path: &Path, bytes: &[u8], until: Instant) -> SnapshotResult<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        crate::platform::private_open(&mut options);
        let mut file = options.open(path).map_err(failed)?;
        for chunk in bytes.chunks(1024 * 1024) {
            live(until)?;
            file.write_all(chunk).map_err(failed)?;
        }
        file.flush().map_err(failed)?;
        live(until)
    }

    #[cfg(test)]
    type DirectoryHook = std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>>;
    #[cfg(test)]
    type ReadHook = std::cell::RefCell<Option<Box<dyn FnMut()>>>;

    #[cfg(test)]
    thread_local! {
        pub(in crate::state) static AFTER_DIRECTORY: DirectoryHook = const { DirectoryHook::new(None) };
        pub(in crate::state) static AFTER_READ: ReadHook = const { ReadHook::new(None) };
    }

    fn query_copy(
        main: &Source,
        wal: Option<&[u8]>,
        ids: &[String],
        until: Instant,
    ) -> SnapshotResult<BTreeMap<String, String>> {
        let directory = tempfile::tempdir().map_err(failed)?;
        #[cfg(unix)]
        fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .map_err(failed)?;
        #[cfg(test)]
        AFTER_DIRECTORY.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(directory.path());
            }
        });
        let main_path = directory.path().join("state_5.sqlite");
        write_copy(&main_path, &main.bytes, until)?;
        if let Some(wal) = wal {
            write_copy(&directory.path().join("state_5.sqlite-wal"), wal, until)?;
        }
        let db = Connection::open_with_flags(
            &main_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(failed)?;
        db.busy_timeout(Duration::ZERO).map_err(failed)?;
        db.progress_handler(1000, Some(move || Instant::now() >= until));
        unsafe {
            rusqlite::ffi::sqlite3_limit(
                db.handle(),
                rusqlite::ffi::SQLITE_LIMIT_SQL_LENGTH,
                64 * 1024,
            );
            rusqlite::ffi::sqlite3_limit(
                db.handle(),
                rusqlite::ffi::SQLITE_LIMIT_LENGTH,
                1024 * 1024,
            );
        }
        db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2048;",
    )
    .map_err(failed)?;
        let tx = db.unchecked_transaction().map_err(failed)?;
        let check: String = tx
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(failed)?;
        if check != "ok" {
            return Err(failed("Codex snapshot is inconsistent"));
        }
        let mut paths = BTreeMap::new();
        for id in ids {
            let path: Option<String> = tx
                .query_row(
                    "SELECT rollout_path FROM threads WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(failed)?;
            let Some(path) = path else {
                return Err(failed("Codex child is absent from the current catalog"));
            };
            if path.is_empty()
                || path.len() > MAX_PATH
                || path.contains('\0')
                || !Path::new(&path).is_absolute()
            {
                return Err(failed("Codex child rollout path is invalid"));
            }
            paths.insert(id.clone(), path);
        }
        tx.commit().map_err(failed)?;
        drop(db);
        #[cfg(test)]
        AFTER_READ.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook();
            }
        });
        Ok(paths)
    }

    fn unchanged(main: &Source, wal: &Option<Source>, home: &Path, until: Instant) -> bool {
        let current_main = read_source(&home.join("state_5.sqlite"), until);
        let current_wal = read_wal(&home.join("state_5.sqlite-wal"), until);
        matches!(current_main, Ok(current) if current == *main)
            && matches!(current_wal, Ok(current) if current == *wal)
    }

    fn attempt(
        home: &Path,
        ids: &[String],
        until: Instant,
    ) -> SnapshotResult<BTreeMap<String, String>> {
        if hot_journal(home, until)?.is_some() {
            return Err(failed("Codex rollback journal is present"));
        }
        let main = read_source(&home.join("state_5.sqlite"), until)?;
        let size = page_size(&main)?;
        let wal = read_wal(&home.join("state_5.sqlite-wal"), until)?;
        // The copy holds only the WAL's committed content (`validate_wal`).
        let committed = match &wal {
            Some(source) => Some(&source.bytes[..validate_wal(source, size, until)?]),
            None => None,
        };
        let paths = query_copy(&main, committed, ids, until)?;
        if !unchanged(&main, &wal, home, until) {
            return Err(SnapshotError::Changed);
        }
        if hot_journal(home, until)?.is_some() {
            return Err(failed("Codex rollback journal appeared during its read"));
        }
        live(until)?;
        Ok(paths)
    }

    pub(in crate::state) fn rollout_paths(
        home: &Path,
        ids: &[String],
        until: Instant,
    ) -> Result<BTreeMap<String, String>> {
        if ids.len() > MAX_CHILDREN || ids.iter().any(|id| !session_id::canonical(id)) {
            return Err("Codex child identities exceed their bound".into());
        }
        // No native connection or lock can coexist with this snapshot's raw fds.
        // Holding it through all attempts also excludes later in-process reconciliation.
        let _exclusive = match NATIVE_SQLITE.try_write() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                return Err("A native SQLite connection is already open in this process".into())
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err("Native SQLite exclusion is unavailable".into())
            }
        };
        let mut original = None;
        for _ in 0..ATTEMPTS {
            live(until).map_err(|error| error.to_string())?;
            let main =
                read_source(&home.join("state_5.sqlite"), until).map_err(|error| match error {
                    SnapshotError::Changed => "Codex catalog changed during its read".to_owned(),
                    SnapshotError::Missing => "Codex catalog is missing".to_owned(),
                    SnapshotError::Failed(error) => error,
                })?;
            // Across attempts only another file at the path is a replacement:
            // a live writer's commits and checkpoints change the same file in
            // place (its modification time too), and that attempt is retried.
            // Within an attempt the full identity, modification time included,
            // still detects every change (`read_source`, `unchanged`, the WAL)
            // (zerus-wa90v).
            let file = (main.version.identity.device, main.version.identity.inode);
            if original.get_or_insert(file) != &file {
                return Err("Codex catalog was replaced during its read".into());
            }
            match attempt(home, ids, until) {
                Ok(paths) => return Ok(paths),
                Err(SnapshotError::Changed) => (),
                Err(SnapshotError::Missing) => {
                    return Err("Codex catalog was removed during its read".into())
                }
                Err(SnapshotError::Failed(error)) => return Err(error),
            }
        }
        Err("Codex catalog changed during every snapshot attempt".into())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use snapshot::rollout_paths;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(in crate::state) use snapshot::{AFTER_DIRECTORY, AFTER_READ, MAX_SOURCE, RAW_OPENS};

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
