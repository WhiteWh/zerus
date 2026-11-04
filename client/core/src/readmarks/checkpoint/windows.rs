//! The existing read-marks checkpoint state machine over shared native file primitives.
use super::{
    refused, serialized, Checkpoint, ReadMarks, Source, CHECKPOINT_BYTES, CHECKPOINT_MARKS,
};
use crate::fsx::windows::{self, bytes, Opened, PublicationFailure};
use std::{io, path::Path, sync::Mutex};
use zerus_desktop_migration::sha256;
fn current(path: &Path) -> io::Result<Option<Opened>> {
    windows::current(path, CHECKPOINT_BYTES)
}
fn publish(path: &Path, data: &[u8], old: Option<Opened>) -> Result<Opened, PublicationFailure> {
    windows::publish(path, data, old, CHECKPOINT_BYTES)
}
#[cfg(test)]
fn publish_with(
    path: &Path,
    data: &[u8],
    old: Option<Opened>,
    prepare: impl FnOnce(&mut std::fs::File, &[u8]) -> io::Result<()>,
) -> Result<Opened, PublicationFailure> {
    windows::publish_with(path, data, old, CHECKPOINT_BYTES, prepare)
}
pub(super) fn checkpoint(marks: &mut ReadMarks) -> io::Result<Checkpoint> {
    checkpoint_with(marks, publish)
}
fn checkpoint_with(
    marks: &mut ReadMarks,
    publisher: impl FnOnce(&Path, &[u8], Option<Opened>) -> Result<Opened, PublicationFailure>,
) -> io::Result<Checkpoint> {
    if !marks.checkpoint.initial_ok || marks.checkpoint.source == Source::Unavailable {
        return Err(refused(
            "pending read checkpoint source was not loaded reliably",
        ));
    }
    if marks.marks.len() > CHECKPOINT_MARKS {
        return Err(refused("pending read checkpoint row limit"));
    }
    let mut disk = current(&marks.path)?;
    let observed = disk
        .as_ref()
        .map(|v| Source::Hash(v.hash.clone()))
        .unwrap_or(Source::Missing);
    if observed != marks.checkpoint.source {
        return Err(refused("pending read checkpoint source changed"));
    }
    if marks.checkpoint.dirty || disk.is_none() {
        let data = serialized(marks, CHECKPOINT_BYTES)?;
        disk = Some(match publisher(&marks.path, &data, disk) {
            Ok(published) => published,
            Err(PublicationFailure::Before(error)) => return Err(error),
            Err(PublicationFailure::Unknown(error)) => {
                // A rename/flush outcome cannot authorize another rewrite,
                // even if a later hash happens to equal the old source.
                marks.checkpoint.source = Source::Unavailable;
                // Do not expose a retryable native sharing code for an
                // uncertain publication, even if the underlying error was 32.
                return Err(io::Error::other(format!(
                    "checkpoint publication outcome is unknown: {error}"
                )));
            }
        });
        marks.checkpoint.saved(&data);
        marks.unsaved = false;
    }
    let disk = disk.ok_or_else(|| refused("pending read checkpoint missing file"))?;
    // Only pre-publication sharing contention can request an automatic retry.
    // A completed checkpoint's final sync failure stays an explicit refusal.
    disk.file
        .sync_all()
        .map_err(|error| io::Error::other(format!("checkpoint final sync failed: {error}")))?;
    Ok(Checkpoint {
        sha256: disk.hash.clone(),
        bytes: disk.bytes,
        marks: marks.marks.len(),
        original: Mutex::new(disk),
    })
}
pub(super) fn checkpoint_bytes(path: &Path, checkpoint: &Checkpoint) -> io::Result<Vec<u8>> {
    let mut disk = checkpoint
        .original
        .try_lock()
        .map_err(|_| refused("checkpoint source is busy or unavailable"))?;
    if disk.path != path || disk.hash != checkpoint.sha256 || disk.bytes != checkpoint.bytes {
        return Err(refused("checkpoint source identity changed"));
    }
    let data = bytes(&mut disk.file, CHECKPOINT_BYTES)?;
    if data.len() != checkpoint.bytes || sha256(&data) != checkpoint.sha256 {
        return Err(refused("pending read checkpoint changed during capture"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests;
