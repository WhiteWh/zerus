//! Bounded namespace hints, never proof that a source is present or absent.
//! Native versions from the census remain the evidence for every row.
use super::*;

pub(super) struct Batch {
    pub entries: usize,
    pub keys: Vec<Key>,
    pub caught_up: bool,
    pub restart: bool,
}

pub(super) fn qualified() -> bool {
    cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
pub(super) use linux::Changes;

// macOS remains on the conservative advisory repair until its directory feed
// has a separately qualified delivery/drop barrier. Production activation is off.
#[cfg(not(target_os = "linux"))]
pub(super) struct Changes;
#[cfg(not(target_os = "linux"))]
impl Changes {
    pub fn new(_: &Path) -> anyhow::Result<Option<Self>> {
        Ok(None)
    }
    pub fn drain(&mut self, _: usize) -> anyhow::Result<Batch> {
        unreachable!("no native change feed on an unqualified platform")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    // Task #12: the inotify calls and the record parser are shared with the
    // change bus (`state::changes`); the namespace rules below stay here.
    use crate::state::changes::linux as native;
    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::{CString, OsStr},
        fs, io,
        os::{
            fd::{AsRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };

    const BUFFER: usize = 64 << 10;
    const MASK: u32 = libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_MODIFY
        | libc::IN_CLOSE_WRITE
        | libc::IN_ATTRIB
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;

    pub(in crate::state::listing_metadata) struct Changes {
        fd: OwnedFd,
        root: PathBuf,
        watches: BTreeMap<i32, PathBuf>,
        namespaces: BTreeSet<PathBuf>,
        buffer: Vec<u8>,
        offset: usize,
        length: usize,
    }
    fn unavailable(error: impl std::fmt::Display) -> anyhow::Error {
        anyhow::anyhow!("Session display change coverage is unavailable: {error}")
    }
    impl Changes {
        pub fn new(root: &Path) -> anyhow::Result<Option<Self>> {
            Self::install(root, native::add_watch)
        }
        fn install(
            root: &Path,
            mut watch: impl FnMut(i32, &CString, u32) -> io::Result<i32>,
        ) -> anyhow::Result<Option<Self>> {
            let fd = native::init().map_err(unavailable)?;
            let mut namespaces = BTreeSet::new();
            namespaces.insert(root.to_path_buf());
            for folder in
                storage::BINDING_DIRS
                    .into_iter()
                    .chain(["renames", "name_routes", "labels"])
            {
                let mut path = root.to_path_buf();
                for part in Path::new(folder).components() {
                    path.push(part);
                    namespaces.insert(path.clone());
                }
            }
            // Only the fixed namespace directories and their containers are
            // watched. Native quotas are never evaded with per-file watches.
            anyhow::ensure!(
                namespaces.len() <= 16,
                "Display watch namespace exceeds its bound"
            );
            let mut watches = BTreeMap::new();
            for path in &namespaces {
                match fs::symlink_metadata(path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(unavailable(e)),
                    Ok(m) => anyhow::ensure!(
                        m.is_dir() && !m.file_type().is_symlink(),
                        "Unverified display watch namespace"
                    ),
                }
                let name = CString::new(path.as_os_str().as_bytes())?;
                let wd = watch(fd.as_raw_fd(), &name, MASK).map_err(|error| {
                    if error.raw_os_error() == Some(libc::ENOSPC) {
                        unavailable("the native inotify watch limit is exhausted")
                    } else {
                        unavailable(error)
                    }
                })?;
                watches.insert(wd, path.clone());
            }
            Ok(Some(Self {
                fd,
                root: root.to_path_buf(),
                watches,
                namespaces,
                buffer: vec![0; BUFFER],
                offset: 0,
                length: 0,
            }))
        }
        pub fn drain(&mut self, limit: usize) -> anyhow::Result<Batch> {
            let mut batch = Batch {
                entries: 0,
                keys: Vec::new(),
                caught_up: false,
                restart: false,
            };
            let mut keys = BTreeSet::new();
            while batch.entries < limit {
                let Some(event) = native::next(
                    self.fd.as_raw_fd(),
                    &mut self.buffer,
                    &mut self.offset,
                    &mut self.length,
                )
                .map_err(unavailable)?
                else {
                    batch.caught_up = true;
                    break;
                };
                let name = &self.buffer[event.name.clone()];
                batch.entries += 1;
                if event.mask
                    & (libc::IN_Q_OVERFLOW
                        | libc::IN_UNMOUNT
                        | libc::IN_IGNORED
                        | libc::IN_MOVE_SELF
                        | libc::IN_DELETE_SELF)
                    != 0
                {
                    batch.restart = true;
                    break;
                }
                let parent = self
                    .watches
                    .get(&event.wd)
                    .ok_or_else(|| unavailable("unknown native watch"))?;
                let path = if name.is_empty() {
                    parent.clone()
                } else {
                    parent.join(OsStr::from_bytes(name))
                };
                let control = ["renames", "name_routes", "labels"]
                    .iter()
                    .any(|p| path.starts_with(self.root.join(p)));
                let namespace_change = event.mask
                    & (libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO)
                    != 0;
                if (control && !name.is_empty())
                    || (event.mask & libc::IN_ISDIR != 0
                        && namespace_change
                        && self.namespaces.contains(&path))
                {
                    batch.restart = true;
                    break;
                }
                if let Some(key) = Key::at(&self.root, &path) {
                    keys.insert(key.0);
                }
            }
            batch.keys = keys.into_iter().map(Key).collect();
            Ok(batch)
        }
    }
    #[cfg(test)]
    mod tests;
}
