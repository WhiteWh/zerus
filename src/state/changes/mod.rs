//! P2.1 (design 4.1): the change bus. One watcher per resident process turns
//! file notifications under a folder into hints for its event loop.
//!
//! - A hint only names what may have changed: the caller runs its existing
//!   stamp comparison for it, and the stamps stay the truth. A lost or
//!   spurious notification costs one scan, never a wrong row.
//! - A burst is delivered once, [`DEBOUNCE`] after its first notification.
//! - Overflow (the system dropped notifications) is a hint that names
//!   everything; a watcher that cannot start or fails ends with
//!   [`Note::Failed`]. Either way the caller keeps its timer pulse.
//! - `HGS_CHANGE_BUS=0` starts no watcher: the caller keeps today's pulse.
//! - SQLite stores change inside files whose notifications NTFS may report
//!   late; [`DataVersion`] tells whether another connection committed.
//!
//! Windows uses `ReadDirectoryChangesW` (windows.rs). Other platforms have no
//! watcher yet (P2.1b): [`Bus::start`] refuses and the caller keeps its pulse.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
mod windows;

/// A burst of notifications is delivered once, this long after its first.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(50);
/// A hint names at most this many paths; more make it an overflow.
const MAX_PATHS: usize = 512;
/// The system's notification buffer for one watcher.
#[cfg_attr(not(windows), allow(dead_code))]
const BUFFER: usize = 64 * 1024;

/// What may have changed, relative to the watched folder.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Hint {
    pub(crate) paths: BTreeSet<PathBuf>,
    /// Notifications were lost: anything may have changed.
    pub(crate) overflow: bool,
}

#[derive(Debug)]
pub(crate) enum Note {
    Hint(Hint),
    /// The watcher stopped and sends nothing more.
    Failed(String),
}

/// False when `HGS_CHANGE_BUS=0`.
pub(crate) fn enabled() -> bool {
    std::env::var_os("HGS_CHANGE_BUS").is_none_or(|value| value != "0")
}

/// Which notified paths count; the others never wake the caller.
pub(crate) type Keep = fn(&Path) -> bool;

/// The notifications of one burst, filtered and bounded.
#[derive(Default)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Batch {
    hint: Hint,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Batch {
    fn add(&mut self, relative: PathBuf, keep: Keep) {
        if self.hint.overflow || !keep(&relative) {
            return;
        }
        self.hint.paths.insert(relative);
        if self.hint.paths.len() > MAX_PATHS {
            self.overflow();
        }
    }

    fn overflow(&mut self) {
        self.hint.overflow = true;
        self.hint.paths.clear();
    }

    /// The burst so far, when anything in it counts.
    fn take(&mut self) -> Option<Hint> {
        let hint = std::mem::take(&mut self.hint);
        (hint.overflow || !hint.paths.is_empty()).then_some(hint)
    }
}

/// A running watcher; dropping it stops its thread.
pub(crate) struct Bus {
    #[cfg(windows)]
    _watcher: windows::Watcher,
}

impl Bus {
    /// Watches `root` and its subfolders. `sink` gets every note and returns
    /// false once nobody listens, which stops the watcher; it must not block
    /// (dropping the bus waits for the watcher's thread). Returns only once
    /// the watcher is armed: a change made after it returns is notified.
    pub(crate) fn start(root: &Path, keep: Keep, sink: impl FnMut(Note) -> bool + Send + 'static) -> Result<Self, String> {
        #[cfg(windows)]
        {
            Ok(Self { _watcher: windows::Watcher::start(root, keep, BUFFER, sink)? })
        }
        #[cfg(not(windows))]
        {
            let _ = (root, keep, sink);
            Err("no change notifications on this platform yet".into())
        }
    }
}

/// `PRAGMA data_version` on a held read connection: it moves only when
/// another connection commits, so a process never wakes itself. Checked when
/// a store's file is notified, and on the safety sweep.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct DataVersion {
    db: rusqlite::Connection,
    seen: i64,
}

#[cfg_attr(not(test), allow(dead_code))]
impl DataVersion {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let db = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error.to_string())?;
        db.busy_timeout(Duration::from_millis(250)).map_err(|error| error.to_string())?;
        let seen = Self::read(&db)?;
        Ok(Self { db, seen })
    }

    fn read(db: &rusqlite::Connection) -> Result<i64, String> {
        db.query_row("PRAGMA data_version", [], |row| row.get(0)).map_err(|error| error.to_string())
    }

    /// True once after another connection committed.
    pub(crate) fn moved(&mut self) -> Result<bool, String> {
        let now = Self::read(&self.db)?;
        let moved = now != self.seen;
        self.seen = now;
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every(_: &Path) -> bool {
        true
    }

    fn no_temporary(path: &Path) -> bool {
        path.file_name().and_then(|name| name.to_str()).is_some_and(|name| !name.starts_with(".hgs-"))
    }

    #[test]
    fn a_batch_filters_bounds_and_empties() {
        let mut batch = Batch::default();
        assert_eq!(batch.take(), None, "an empty burst wakes nobody");
        batch.add(".hgs-next".into(), no_temporary);
        assert_eq!(batch.take(), None, "a burst of ignored paths wakes nobody");
        batch.add("a.json".into(), no_temporary);
        batch.add("a.json".into(), no_temporary);
        assert_eq!(batch.take().map(|hint| hint.paths.len()), Some(1));
        for index in 0..=MAX_PATHS {
            batch.add(PathBuf::from(format!("{index}.json")), every);
        }
        assert_eq!(batch.take(), Some(Hint { paths: BTreeSet::new(), overflow: true }), "too many paths are an overflow");
        assert_eq!(batch.take(), None);
    }

    #[test]
    fn the_bus_is_on_unless_switched_off() {
        // Read only: the variable is never set by tests, which run in parallel.
        assert_eq!(enabled(), std::env::var_os("HGS_CHANGE_BUS").is_none_or(|value| value != "0"));
    }

    #[test]
    fn data_version_moves_only_for_another_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite3");
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(v INTEGER);").unwrap();
        let mut version = DataVersion::open(&path).unwrap();
        assert!(!version.moved().unwrap(), "nothing committed yet");
        writer.execute("INSERT INTO t VALUES(1)", []).unwrap();
        assert!(version.moved().unwrap(), "another connection committed");
        assert!(!version.moved().unwrap(), "once per commit");
        let count: i64 = version.db.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        assert!(!version.moved().unwrap(), "its own reads never move it");
    }
}
