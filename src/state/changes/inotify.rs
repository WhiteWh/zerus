//! Task #12 (P2.1b): the part of the Linux change bus that needs no inotify
//! call, compiled and tested on every platform with synthetic buffers:
//! reading raw `inotify_event` records, and keeping the watched folders
//! (inotify watches one folder per descriptor, never a subtree). The syscalls
//! live in linux.rs; the listing metadata feed reads its records here too.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use super::{Batch, Keep};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;

// The Linux values (linux.rs checks them against libc).
pub(crate) const IN_MODIFY: u32 = 0x0000_0002;
pub(crate) const IN_ATTRIB: u32 = 0x0000_0004;
pub(crate) const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub(crate) const IN_MOVED_FROM: u32 = 0x0000_0040;
pub(crate) const IN_MOVED_TO: u32 = 0x0000_0080;
pub(crate) const IN_CREATE: u32 = 0x0000_0100;
pub(crate) const IN_DELETE: u32 = 0x0000_0200;
pub(crate) const IN_DELETE_SELF: u32 = 0x0000_0400;
pub(crate) const IN_MOVE_SELF: u32 = 0x0000_0800;
pub(crate) const IN_UNMOUNT: u32 = 0x0000_2000;
pub(crate) const IN_Q_OVERFLOW: u32 = 0x0000_4000;
pub(crate) const IN_IGNORED: u32 = 0x0000_8000;
pub(crate) const IN_ISDIR: u32 = 0x4000_0000;

/// `struct inotify_event` before its name: wd, mask, cookie, len.
pub(crate) const HEADER: usize = 16;
/// At most this many folders are watched; more leave the caller on its pulse.
pub(crate) const MAX_WATCHES: usize = 128;

/// One record: its watch, its mask and where its name lies in the buffer
/// (without the NUL padding; empty for an event of the folder itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Raw {
    pub(crate) wd: i32,
    pub(crate) mask: u32,
    pub(crate) name: Range<usize>,
}

/// Reads the record at `*offset` in `bytes` (the filled part of the buffer)
/// and moves `*offset` past it. A record that does not fit is an error: the
/// caller cannot know what it lost.
pub(crate) fn parse_next(bytes: &[u8], offset: &mut usize) -> Result<Raw, &'static str> {
    let start = *offset;
    let header = bytes.get(start..start.saturating_add(HEADER)).ok_or("truncated change event")?;
    let word = |at: usize| [header[at], header[at + 1], header[at + 2], header[at + 3]];
    let wd = i32::from_ne_bytes(word(0));
    let mask = u32::from_ne_bytes(word(4));
    let length = u32::from_ne_bytes(word(12)) as usize;
    let first = start + HEADER;
    let end = first.checked_add(length).filter(|end| *end <= bytes.len()).ok_or("invalid event length")?;
    let last = bytes[first..end].iter().position(|byte| *byte == 0).map_or(end, |at| first + at);
    *offset = end;
    Ok(Raw { wd, mask, name: first..last })
}

/// A record's name as a path component.
pub(crate) fn component(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// What the syscall layer does after a record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    /// A kept folder appeared: watch it and its kept subfolders.
    Watch(PathBuf),
    /// This watch's folder moved away: remove the watch.
    Forget(i32),
    /// The watcher cannot go on; the caller keeps its pulse.
    Fail(String),
}

/// The watched folders, by watch descriptor, relative to the root ("" is
/// the root itself).
#[derive(Default)]
pub(crate) struct Tree {
    pub(crate) watches: BTreeMap<i32, PathBuf>,
}

impl Tree {
    /// Records a new watch. inotify answers the same descriptor for a folder
    /// watched again, so a renamed folder keeps one entry.
    pub(crate) fn watched(&mut self, wd: i32, relative: PathBuf) -> Result<(), String> {
        self.watches.insert(wd, relative);
        if self.watches.len() > MAX_WATCHES {
            return Err(format!("more than {MAX_WATCHES} folders to watch"));
        }
        Ok(())
    }

    /// Adds what a record names to `batch` and says what the syscall layer
    /// must do. Records of a watch already removed are late and ignored.
    pub(super) fn event(&mut self, wd: i32, mask: u32, name: &[u8], keep: Keep, batch: &mut Batch) -> Step {
        if mask & IN_Q_OVERFLOW != 0 {
            batch.overflow();
            return Step::Nothing;
        }
        let Some(folder) = self.watches.get(&wd).cloned() else { return Step::Nothing };
        let root = folder.as_os_str().is_empty();
        if mask & IN_UNMOUNT != 0 {
            return Step::Fail("the watched file system was unmounted".into());
        }
        if mask & IN_IGNORED != 0 {
            self.watches.remove(&wd);
            if root {
                return Step::Fail("the watched folder is gone".into());
            }
            batch.add(folder, keep);
            return Step::Nothing;
        }
        if name.is_empty() && mask & (IN_DELETE_SELF | IN_MOVE_SELF) != 0 {
            if root {
                return Step::Fail("the watched folder was removed or moved".into());
            }
            batch.add(folder, keep);
            if mask & IN_MOVE_SELF != 0 {
                // Its later records would name its old place.
                self.watches.remove(&wd);
                return Step::Forget(wd);
            }
            // IN_IGNORED follows a deletion.
            return Step::Nothing;
        }
        if name.is_empty() {
            return Step::Nothing;
        }
        let path = folder.join(component(name));
        let appeared = mask & IN_ISDIR != 0 && mask & (IN_CREATE | IN_MOVED_TO) != 0 && keep(&path);
        batch.add(path.clone(), keep);
        if appeared {
            Step::Watch(path)
        } else {
            Step::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Hint;
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    /// One `inotify_event` as the kernel writes it: the name NUL-padded to a
    /// multiple of four, `len` counting the padding.
    fn record(wd: i32, mask: u32, name: &str) -> Vec<u8> {
        let padded = if name.is_empty() { 0 } else { (name.len() + 1).div_ceil(4) * 4 };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&wd.to_ne_bytes());
        bytes.extend_from_slice(&mask.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&(padded as u32).to_ne_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.resize(HEADER + padded, 0);
        bytes
    }

    /// The state folder's rule: no temporary files, and only some folders.
    fn keep(path: &Path) -> bool {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        !name.starts_with(".hgs-") && !path.starts_with("logs")
    }

    fn fresh() -> Tree {
        let mut tree = Tree::default();
        tree.watched(1, PathBuf::new()).unwrap();
        tree.watched(2, PathBuf::from("bindings")).unwrap();
        tree
    }

    fn paths(batch: &mut Batch) -> BTreeSet<PathBuf> {
        batch.take().map(|hint| hint.paths).unwrap_or_default()
    }

    #[test]
    fn records_are_read_one_after_another_and_a_short_one_is_an_error() {
        let mut bytes = record(1, IN_CREATE, "a.json");
        bytes.extend(record(2, IN_DELETE_SELF, ""));
        bytes.extend(record(1, IN_MOVED_TO, "a-much-longer-name.json"));
        let mut offset = 0;
        let mut seen = Vec::new();
        while offset < bytes.len() {
            let raw = parse_next(&bytes, &mut offset).unwrap();
            seen.push((raw.wd, raw.mask, String::from_utf8(bytes[raw.name].to_vec()).unwrap()));
        }
        assert_eq!(
            seen,
            [(1, IN_CREATE, String::from("a.json")), (2, IN_DELETE_SELF, String::new()), (1, IN_MOVED_TO, String::from("a-much-longer-name.json"))]
        );
        assert_eq!(offset, bytes.len());

        let whole = record(1, IN_CREATE, "a.json");
        assert_eq!(parse_next(&whole[..HEADER - 1], &mut 0), Err("truncated change event"));
        assert_eq!(parse_next(&whole[..HEADER + 2], &mut 0), Err("invalid event length"));
        let mut offset = 0;
        assert_eq!(parse_next(&whole, &mut offset).map(|raw| raw.name.len()), Ok(6));
        assert_eq!(parse_next(&whole, &mut offset), Err("truncated change event"), "nothing after the last record");
    }

    #[test]
    fn records_name_paths_relative_to_the_root_and_new_kept_folders_are_watched() {
        let mut tree = fresh();
        let mut batch = Batch::default();
        assert_eq!(tree.event(1, IN_CREATE, b"a.json", keep, &mut batch), Step::Nothing);
        assert_eq!(tree.event(2, IN_MOVED_TO, b"b.json", keep, &mut batch), Step::Nothing);
        assert_eq!(tree.event(1, IN_CREATE, b".hgs-next", keep, &mut batch), Step::Nothing);
        assert_eq!(paths(&mut batch), BTreeSet::from([PathBuf::from("a.json"), Path::new("bindings").join("b.json")]));

        // A new kept folder is named and watched; one the caller ignores is neither.
        assert_eq!(tree.event(1, IN_CREATE | IN_ISDIR, b"labels", keep, &mut batch), Step::Watch("labels".into()));
        assert_eq!(tree.event(1, IN_MOVED_TO | IN_ISDIR, b"logs", keep, &mut batch), Step::Nothing);
        assert_eq!(paths(&mut batch), BTreeSet::from([PathBuf::from("labels")]));
        tree.watched(3, "labels".into()).unwrap();
        tree.event(3, IN_CLOSE_WRITE, b"l.json", keep, &mut batch);
        assert_eq!(paths(&mut batch), BTreeSet::from([Path::new("labels").join("l.json")]));

        // A record of a watch already removed is late: nothing.
        assert_eq!(tree.event(9, IN_CREATE, b"x.json", keep, &mut batch), Step::Nothing);
        assert_eq!(batch.take(), None);
    }

    #[test]
    fn overflow_loss_and_removal_are_handled() {
        let mut tree = fresh();
        let mut batch = Batch::default();
        tree.event(1, IN_CREATE, b"a.json", keep, &mut batch);
        assert_eq!(tree.event(-1, IN_Q_OVERFLOW, b"", keep, &mut batch), Step::Nothing);
        assert_eq!(batch.take(), Some(Hint { paths: BTreeSet::new(), overflow: true }), "an overflow names everything");

        // A subfolder deleted: named, and forgotten once the kernel drops its watch.
        assert_eq!(tree.event(2, IN_DELETE_SELF, b"", keep, &mut batch), Step::Nothing);
        assert_eq!(tree.event(2, IN_IGNORED, b"", keep, &mut batch), Step::Nothing);
        assert!(!tree.watches.contains_key(&2));
        assert_eq!(paths(&mut batch), BTreeSet::from([PathBuf::from("bindings")]));

        // A subfolder moved away: named, and its watch removed.
        tree.watched(4, "labels".into()).unwrap();
        assert_eq!(tree.event(4, IN_MOVE_SELF, b"", keep, &mut batch), Step::Forget(4));
        assert!(!tree.watches.contains_key(&4));
        assert_eq!(paths(&mut batch), BTreeSet::from([PathBuf::from("labels")]));

        // The root itself gone, moved or unmounted: the watcher fails.
        for mask in [IN_DELETE_SELF, IN_MOVE_SELF, IN_IGNORED, IN_UNMOUNT] {
            let mut tree = fresh();
            assert!(matches!(tree.event(1, mask, b"", keep, &mut batch), Step::Fail(_)), "{mask:#x}");
        }
        let mut tree = fresh();
        assert!(matches!(tree.event(2, IN_UNMOUNT, b"", keep, &mut batch), Step::Fail(_)));
    }

    #[test]
    fn the_number_of_watched_folders_is_bounded() {
        let mut tree = Tree::default();
        for wd in 0..MAX_WATCHES as i32 {
            tree.watched(wd, PathBuf::from(wd.to_string())).unwrap();
        }
        tree.watched(0, "renamed".into()).unwrap();
        assert_eq!(tree.watches.len(), MAX_WATCHES, "a folder watched again keeps one entry");
        assert!(tree.watched(MAX_WATCHES as i32, "one-more".into()).is_err());
    }
}
