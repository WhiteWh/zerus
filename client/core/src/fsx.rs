//! The core's whole-file writes: the read marks ([`crate::readmarks`]), the
//! attention marks ([`crate::marks`]), the organization's pending edits
//! ([`crate::swarm::draft`]) and the files the app saves for its owner.
pub(crate) mod checkpoint;
pub(crate) mod preload;
#[cfg(windows)]
pub(crate) mod windows;

use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Native managed metadata only. Reuse the checkpoint reader's bounded,
/// no-follow/private/single-link checks; this selects no path or permission.
pub fn read_private_bounded(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    if limit == 0 || limit > 16 << 20 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(windows)]
    {
        let Some(mut source) = windows::capture(path, limit)? else {
            return Ok(None);
        };
        source.unchanged_bytes(path, limit).map(Some)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, limit);
        Err(io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    {
        use std::io::{Read, Seek};
        let Some(mut source) = checkpoint::current(path, limit)? else {
            return Ok(None);
        };
        source.file.rewind()?;
        let mut bytes = Vec::with_capacity(source.bytes);
        source.file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() != source.bytes || zerus_desktop_migration::sha256(&bytes) != source.hash {
            return Err(io::Error::other("private source changed during capture"));
        }
        Ok(Some(bytes))
    }
}

/// The existing atomic private writer with its parent-directory durability
/// barrier. Native callers retain uncertainty if either sync fails.
pub fn write_private_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        write_private_atomic(path, bytes, true)?;
        checkpoint::sync_parent(path)
    }
    #[cfg(windows)]
    {
        let limit = 16 << 20;
        let old = windows::current(path, limit)?;
        windows::publish(path, bytes, old, limit)
            .map(|_| ())
            .map_err(|failure| match failure {
                windows::PublicationFailure::Before(e) => e,
                windows::PublicationFailure::Unknown(e) => {
                    io::Error::other(format!("private publication outcome unknown: {e}"))
                }
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, bytes);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// How many random names a write tries before it gives up.
const ATTEMPTS: u32 = 16;

/// A random temporary name beside `path`: `<path>.<16 hex digits>.tmp`, as
/// hgs's `NamedTempFile` names one in the target's folder. The digits come
/// from the standard library's randomly keyed hasher over a counter, the
/// process and the time.
pub(crate) fn temporary(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    if let Ok(time) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(time.as_nanos());
    }
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{:016x}.tmp", hasher.finish()));
    PathBuf::from(name)
}

/// A new temporary file beside `path`, readable and writable by this user
/// only on Unix (mode 0600 from its creation). `create_new` never opens an
/// existing file or follows a link in its place; a name that exists is
/// skipped for another.
fn private_file(path: &Path) -> io::Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let name = temporary(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&name) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)))
}

/// Replaces `path` with `bytes` as one change. They go to a new temporary
/// file with a random name beside it ([`temporary`]) that only this user
/// may read (0600 on Unix: read marks and drafts hold session and project
/// names), and the file is renamed over the target. A reader sees the old
/// file or the new one, never a part of either; a failure keeps the old file
/// and removes the temporary one. A missing folder is created. `durable`
/// flushes the temporary file to disk before the rename, as Qt's `QSaveFile`
/// commits its settings; the read and attention marks skip it, as they did
/// before this writer.
pub fn write_private_atomic(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        std::fs::create_dir_all(folder)?;
    }
    let (temporary, mut file) = private_file(path)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| if durable { file.sync_all() } else { Ok(()) })
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temporary, path)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zerus-fsx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn names(dir: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_write_replaces_the_file_whole_and_creates_its_folder() {
        let dir = scratch("replace");
        let path = dir.join("nested").join("state.json");
        write_private_atomic(&path, b"first", true).unwrap();
        write_private_atomic(&path, b"second", false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            names(&dir.join("nested")),
            ["state.json"],
            "no temporary file is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_rename_keeps_the_target_and_removes_the_temporary_file() {
        let dir = scratch("failed");
        // A folder where the file should go: the rename over it fails.
        let path = dir.join("state.json");
        std::fs::create_dir_all(path.join("kept")).unwrap();
        assert!(write_private_atomic(&path, b"bytes", true).is_err());
        assert!(path.join("kept").is_dir());
        assert_eq!(names(&dir), ["state.json"], "no temporary file is left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A random name in the target's folder, and a file that happens to have
    // such a name is never touched.
    #[test]
    fn temporary_names_are_random_beside_the_target_and_other_files_stay() {
        let dir = scratch("names");
        let path = dir.join("state.json");
        let (first, second) = (temporary(&path), temporary(&path));
        assert_ne!(first, second);
        for name in [&first, &second] {
            assert_eq!(name.parent(), Some(dir.as_path()));
            let name = name.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("state.json.") && name.ends_with(".tmp") && name.len() == 31);
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&first, b"someone else's").unwrap();
        write_private_atomic(&path, b"new", true).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Read marks and drafts hold session and project names, so only this
    // user may read them.
    #[cfg(unix)]
    #[test]
    fn the_file_is_this_user_s_alone_also_when_it_replaces_a_readable_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let path = dir.join("state.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        for durable in [true, false] {
            write_private_atomic(&path, b"new", durable).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "durable {durable}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
