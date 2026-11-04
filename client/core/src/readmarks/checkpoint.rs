//! Explicit durability barrier for the existing file. This is not admission
//! closure, a complete ReadDelivery drain, source discovery or role authority.
use super::ReadMarks;
use crate::fsx::checkpoint::Bounded;
#[cfg(not(windows))]
use crate::fsx::checkpoint::{current, sync_parent, Disk};
use serde::ser::{SerializeSeq, Serializer};
use std::io;
#[cfg(not(windows))]
use std::path::Path;
use zerus_desktop_migration::sha256;

pub const CHECKPOINT_BYTES: usize = 16 << 20;
pub const CHECKPOINT_MARKS: usize = 50_000;

#[cfg(windows)]
mod windows;

#[derive(Debug)]
pub struct Checkpoint {
    pub sha256: String,
    pub bytes: usize,
    pub marks: usize,
    // The Windows source remains exclusively owned through native capture and
    // original helper/pipe lifetimes. These handles are never serialized.
    #[cfg(windows)]
    original: std::sync::Mutex<crate::fsx::windows::Opened>,
}
// Comparing checkpoint data never authenticates an owner or substitutes handles.
impl PartialEq for Checkpoint {
    fn eq(&self, other: &Self) -> bool {
        self.sha256 == other.sha256 && self.bytes == other.bytes && self.marks == other.marks
    }
}
impl Eq for Checkpoint {}
#[derive(Clone, PartialEq, Eq)]
enum Source {
    Missing,
    Hash(String),
    Unavailable,
}
pub(super) struct State {
    initial_ok: bool,
    source: Source,
    pub dirty: bool,
}
impl State {
    pub fn loaded(bytes: &io::Result<Vec<u8>>, rows: Option<usize>) -> Self {
        let (initial_ok, source) = match bytes {
            Ok(bytes)
                if bytes.len() <= CHECKPOINT_BYTES
                    && rows.is_some_and(|n| n <= CHECKPOINT_MARKS) =>
            {
                (true, Source::Hash(sha256(bytes)))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (true, Source::Missing),
            _ => (false, Source::Unavailable),
        };
        Self {
            initial_ok,
            source,
            dirty: false,
        }
    }
    pub fn saved(&mut self, bytes: &[u8]) {
        self.dirty = false;
        self.source = if bytes.len() <= CHECKPOINT_BYTES {
            Source::Hash(sha256(bytes))
        } else {
            Source::Unavailable
        };
    }
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(not(windows))]
fn source(disk: &Option<Disk>) -> Source {
    disk.as_ref()
        .map(|d| Source::Hash(d.hash.clone()))
        .unwrap_or(Source::Missing)
}
fn serialized(marks: &ReadMarks, limit: usize) -> io::Result<Vec<u8>> {
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    let mut serializer = serde_json::Serializer::new(&mut writer);
    let mut list = serializer.serialize_seq(Some(marks.marks.len()))?;
    for mark in marks.marks.values() {
        list.serialize_element(mark)?;
    }
    list.end()?;
    Ok(writer.bytes)
}
impl ReadMarks {
    /// Read the exact managed source again after the caller's original closed
    /// writer barrier/checkpoint. This neither chooses a new path nor writes.
    pub fn checkpoint_bytes(&self, checkpoint: &Checkpoint) -> io::Result<Vec<u8>> {
        #[cfg(windows)]
        {
            windows::checkpoint_bytes(&self.path, checkpoint)
        }
        #[cfg(not(windows))]
        {
            if !cfg!(unix) {
                return Err(io::ErrorKind::Unsupported.into());
            }
            let mut disk = current(&self.path, CHECKPOINT_BYTES)?
                .ok_or_else(|| refused("pending read checkpoint disappeared"))?;
            if disk.hash != checkpoint.sha256 || disk.bytes != checkpoint.bytes {
                return Err(refused("pending read checkpoint changed"));
            }
            let mut bytes = Vec::with_capacity(disk.bytes);
            use std::io::{Read, Seek};
            disk.file.rewind()?;
            disk.file
                .take(CHECKPOINT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() != checkpoint.bytes || sha256(&bytes) != checkpoint.sha256 {
                return Err(refused("pending read checkpoint changed during capture"));
            }
            Ok(bytes)
        }
    }
    /// Call off-thread, under the old owner's store mutex, AFTER closing and
    /// draining all of its writer admissions. The native service-mode importer
    /// retains that barrier through complete capture and original helper I/O.
    /// Unchanged bytes/unknown fields stay untouched. A failure grants nothing;
    /// a flush/sync failure can follow publication, so it is unknown.
    /// Windows retains the original exclusive file and validated parent handles.
    pub fn checkpoint(&mut self) -> io::Result<Checkpoint> {
        #[cfg(windows)]
        {
            windows::checkpoint(self)
        }
        #[cfg(not(windows))]
        {
            self.checkpoint_with(sync_parent)
        }
    }
    #[cfg(not(windows))]
    fn checkpoint_with(
        &mut self,
        sync: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Checkpoint> {
        if !cfg!(unix) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        if !self.checkpoint.initial_ok {
            return Err(refused(
                "pending read checkpoint source was not loaded reliably",
            ));
        }
        if self.marks.len() > CHECKPOINT_MARKS {
            return Err(refused("pending read checkpoint row limit"));
        }
        if self.checkpoint.source == Source::Unavailable {
            return Err(refused("pending read checkpoint source is unavailable"));
        }
        let mut disk = current(&self.path, CHECKPOINT_BYTES)?;
        if source(&disk) != self.checkpoint.source {
            return Err(refused("pending read checkpoint source changed"));
        }
        if self.checkpoint.dirty || disk.is_none() {
            let bytes = serialized(self, CHECKPOINT_BYTES)?;
            super::write_private_atomic(&self.path, &bytes, true)?;
            self.checkpoint.saved(&bytes);
            self.unsaved = false;
            disk = current(&self.path, CHECKPOINT_BYTES)?;
            if source(&disk) != self.checkpoint.source {
                return Err(refused(
                    "pending read checkpoint source changed after write",
                ));
            }
        }
        let disk = disk.ok_or_else(|| refused("pending read checkpoint missing file"))?;
        disk.file.sync_all()?;
        sync(&self.path)?;
        Ok(Checkpoint {
            sha256: disk.hash,
            bytes: disk.bytes,
            marks: self.marks.len(),
        })
    }
}

#[cfg(test)]
mod tests;
