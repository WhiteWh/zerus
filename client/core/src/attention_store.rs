//! Explicit checkpoint observation for the existing attention-observed store.
//! No tracker, detector, source discovery, admission closure or role authority.
//! The original owner records its load/save results; its host must close every
//! writer and retain actual background I/O before calling the one-file barrier.
use crate::{
    attention::decode_observed,
    fsx::checkpoint::{current, sync_parent, Bounded, Disk},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Write},
    path::Path,
};
use zerus_desktop_migration::sha256;

pub const CHECKPOINT_BYTES: usize = 16 << 20;
pub const CHECKPOINT_ENTRIES: usize = 50_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub sha256: String,
    pub bytes: usize,
    pub hosts: usize,
    pub entries: usize,
}
#[derive(PartialEq, Eq)]
enum Source {
    Missing,
    Hash(String),
    Unavailable,
}

/// Constant-sized observation metadata, not another copy of per-session state.
/// Install at the existing owner's initial load, never from a staged import.
pub struct Observation {
    initial_ok: bool,
    failed: bool,
    source: Source,
    view: Option<String>,
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn entries(state: &Map<String, Value>) -> io::Result<usize> {
    let mut count = 0usize;
    for value in state.values() {
        count = count
            .checked_add(value.as_object().map_or(1, |m| m.len().max(1)))
            .filter(|n| *n <= CHECKPOINT_ENTRIES)
            .ok_or_else(|| refused("attention checkpoint entry limit"))?;
    }
    Ok(count)
}
struct HashWriter {
    hash: Sha256,
    bytes: usize,
}
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > CHECKPOINT_BYTES.saturating_sub(self.bytes) {
            return Err(refused("attention checkpoint byte limit"));
        }
        self.hash.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn view(state: &Map<String, Value>) -> io::Result<String> {
    entries(state)?;
    let mut writer = HashWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, state)?;
    Ok(format!("{:x}", writer.hash.finalize()))
}
fn source(disk: &Option<Disk>) -> Source {
    disk.as_ref()
        .map(|d| Source::Hash(d.hash.clone()))
        .unwrap_or(Source::Missing)
}
impl Observation {
    /// Observes the old loader without changing its parser, Qt fallback or
    /// unreadable-file behavior. Only this barrier refuses unreliable history.
    pub fn loaded(bytes: &io::Result<Vec<u8>>, state: &Map<String, Value>) -> Self {
        let state_view = view(state).ok();
        let (initial_ok, source) = match bytes {
            Ok(raw) if raw.len() <= CHECKPOINT_BYTES => {
                let decoded = decode_observed(raw).and_then(|m| view(&m).ok());
                let valid = decoded.is_some() && decoded == state_view;
                (
                    valid,
                    if valid {
                        Source::Hash(sha256(raw))
                    } else {
                        Source::Unavailable
                    },
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (state_view.is_some(), Source::Missing)
            }
            _ => (false, Source::Unavailable),
        };
        Self {
            initial_ok,
            failed: false,
            source,
            view: state_view,
        }
    }
    /// Call only after the existing compact JSON writer reports success.
    /// Does not clear an unreliable initial load or earlier failed write.
    pub fn saved(&mut self, bytes: &[u8]) {
        let hash = (bytes.len() <= CHECKPOINT_BYTES).then(|| sha256(bytes));
        self.source = hash
            .clone()
            .map(Source::Hash)
            .unwrap_or(Source::Unavailable);
        self.view = hash;
    }
    /// Serialization/write/sync failure is sticky for this owner's lifetime.
    pub fn failed(&mut self) {
        self.failed = true;
        self.source = Source::Unavailable;
    }

    /// Off-thread after the real owner's writer/callback drain. The caller's
    /// charge must survive any outer timeout, dropped receiver and filesystem
    /// call. Unchanged bytes and unknown fields are synced without rewriting.
    /// The existing writer is used only for an originally absent source.
    /// Windows refuses before I/O. A successful metadata value grants no lease,
    /// current drain, migration publication or notification execution authority.
    pub fn checkpoint(
        &mut self,
        path: &Path,
        state: &Map<String, Value>,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<Checkpoint> {
        self.checkpoint_with(path, state, write, sync_parent)
    }
    fn checkpoint_with(
        &mut self,
        path: &Path,
        state: &Map<String, Value>,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
        sync: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Checkpoint> {
        if !cfg!(unix) {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let result = self.checked(path, state, write, sync);
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn checked(
        &mut self,
        path: &Path,
        state: &Map<String, Value>,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
        sync: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Checkpoint> {
        if !self.initial_ok || self.failed || self.source == Source::Unavailable {
            return Err(refused("attention checkpoint source is unreliable"));
        }
        let count = entries(state)?;
        if Some(view(state)?) != self.view {
            return Err(refused("attention checkpoint memory was not saved"));
        }
        let mut disk = current(path, CHECKPOINT_BYTES)?;
        if source(&disk) != self.source {
            return Err(refused("attention checkpoint source changed"));
        }
        if disk.is_none() {
            let mut buffer = Bounded {
                bytes: Vec::new(),
                limit: CHECKPOINT_BYTES,
            };
            serde_json::to_writer(&mut buffer, state)?;
            write(path, &buffer.bytes)?;
            self.saved(&buffer.bytes);
            disk = current(path, CHECKPOINT_BYTES)?;
            if source(&disk) != self.source {
                return Err(refused("attention checkpoint source changed after write"));
            }
        }
        let disk = disk.ok_or_else(|| refused("attention checkpoint missing file"))?;
        disk.file.sync_all()?;
        sync(path)?;
        Ok(Checkpoint {
            sha256: disk.hash,
            bytes: disk.bytes,
            hosts: state.len(),
            entries: count,
        })
    }
}

#[cfg(test)]
mod tests;
