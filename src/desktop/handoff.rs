//! Bounded import diagnostics and revocation. Structure never grants authority.
use super::store::{HandoffLock, Location};
use crate::platform;
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
use zerus_desktop_migration::{
    Evidence, Handoff, ImportRecord, Phase, HANDOFF_FILE, MAX_HANDOFF_BYTES,
    MAX_IMPORT_RECORD_BYTES,
};

pub(super) fn read_private(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    platform::no_follow(&mut options);
    #[cfg(windows)]
    platform::refuse_link(&mut options);
    let file = options.open(path)?;
    ensure!(platform::verify_private_file(&file)?, "unsafe import file");
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "import file too large"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "import file too large");
    Ok(bytes)
}

pub fn marker(location: &Location, lock: &HandoffLock) -> Result<Option<Handoff>> {
    lock.verify(location)?;
    location.check_existing()?;
    let bytes = match read_private(&location.root.join(HANDOFF_FILE), MAX_HANDOFF_BYTES) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    Ok(Some(Handoff::parse(&bytes, &location.configuration)?))
}

/// Authority reduction only: preserve every imported identity/reference and
/// refuse malformed/newer input. The actor already fenced policy and drained
/// its owned workers before this durable phase transition.
pub fn write_revoked(location: &Location, lock: &HandoffLock, marker: &Handoff) -> Result<()> {
    lock.verify(location)?;
    location.check_existing()?;
    marker.validate(&location.configuration)?;
    let mut revoked = marker.clone();
    revoked.phase = Phase::Revoked;
    let bytes = serde_json::to_vec(&revoked)?;
    ensure!(
        bytes.len() <= MAX_HANDOFF_BYTES,
        "handoff exceeds its byte limit"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&location.root)?;
    platform::make_private(temporary.as_file())?;
    ensure!(
        platform::verify_private_file(temporary.as_file())?,
        "unsafe handoff temporary file"
    );
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    platform::persist(temporary, location.root.join(HANDOFF_FILE)).map_err(|error| error.error)?;
    platform::sync_dir(&location.root)?;
    Ok(())
}

fn evidence(location: &Location) -> Result<(Handoff, ImportRecord)> {
    let marker = Handoff::parse(
        &read_private(&location.root.join(HANDOFF_FILE), MAX_HANDOFF_BYTES)?,
        &location.configuration,
    )?;
    let record_path = location.root.join(marker.record_relative_path()?);
    // JSON supplies no path. Every fixed intermediate folder must still be a
    // private directory, not a symlink/junction into another source store.
    for directory in [
        location.root.clone(),
        location.root.join("imports"),
        record_path
            .parent()
            .expect("fixed import parent")
            .to_owned(),
    ] {
        ensure!(
            platform::verify_private(&directory, platform::Private::Directory)?,
            "unsafe import directory"
        );
    }
    let record =
        marker.verify_import_record(&read_private(&record_path, MAX_IMPORT_RECORD_BYTES)?)?;
    Ok((marker, record))
}

/// Called only by the service actor at startup and at ten-second heartbeats.
/// The actor caches this fixed-size summary; status clients never trigger disk
/// reads. Pages remain unopened until bounded transactional import consumption.
pub fn inspect(location: &Location) -> Value {
    inspect_held(location, None)
}
pub(super) fn inspect_held(location: &Location, held: Option<&HandoffLock>) -> Value {
    if fs::symlink_metadata(location.root.join(HANDOFF_FILE))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return json!({"reason":"handoff_required"});
    }
    let _lock = if let Some(held) = held {
        if held.verify(location).is_err() {
            return json!({"reason":"handoff_unavailable"});
        }
        None
    } else {
        match location.handoff_lock() {
            Ok(lock) => Some(lock),
            Err(_) => return json!({"reason":"handoff_unavailable"}),
        }
    };
    let Ok((marker, record)) = evidence(location) else {
        return json!({"reason":"invalid_import_evidence"});
    };
    let reason = if marker.phase == Phase::Revoked {
        "revoked"
    } else if record.evidence != Evidence::Drained {
        "import_not_drained"
    } else if marker.phase != Phase::Complete {
        "handoff_incomplete"
    } else {
        // Complete is past evidence, including after service restart. Missing
        // current delegation, effect guards and revocation keep writers passive.
        "delegation_required"
    };
    json!({"reason":reason,"phase":marker.phase,"evidence":record.evidence})
}
