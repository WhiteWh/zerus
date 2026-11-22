//! Advisory card inputs, never action, process or absence authority.
//! An internal cache accelerates qualified Linux ls; watch and authority stay canonical.
use super::*;
use repositories::directory::Directory;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::{
    fs::File,
    time::{Duration, Instant},
};
mod changes;
pub(super) mod control;
pub(super) mod listing;
mod model;
mod publication;
mod reconcile;
mod repair;
mod store;
pub(super) mod worker;
// Store-side helpers also remain available to private repair controls.
#[allow(dead_code)]
mod pages;
use model::{Key, Seed};
pub(super) use publication::Publication;
pub(super) use repair::background_at;
use store::Store;

// Absent opt-in costs one metadata lookup,
// with no directory creation, projection, SQLite, lock or coverage write.
fn enabled(root: &Path) -> bool {
    let marker = root.join("listing-metadata/enabled");
    if !std::fs::symlink_metadata(&marker)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() == 2)
    {
        return false;
    }
    let inspect = || -> anyhow::Result<bool> {
        use std::io::Read;
        let root = checked(Directory::open(&crate::platform::canonical(root)?))?;
        let dir = checked(root.child("listing-metadata"))?;
        match std::fs::symlink_metadata(dir.path.join("disabled")) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let file = checked(dir.read_private("enabled"))?;
        let mut bytes = Vec::new();
        file.take(3).read_to_end(&mut bytes)?;
        checked(dir.verify())?;
        checked(root.verify())?;
        Ok(bytes == b"1\n")
    };
    inspect().unwrap_or(false)
}

fn checked<T>(value: Result<T>) -> anyhow::Result<T> {
    value.map_err(anyhow::Error::msg)
}

fn public_index_error(error: &anyhow::Error) -> String {
    use rusqlite::{Error, ErrorCode};
    match error.downcast_ref::<Error>() {
        Some(Error::SqliteFailure(failure, _))
            if matches!(failure.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) =>
        {
            "The session listing is busy. Try again shortly.".into()
        }
        Some(_) => "The session listing index cannot be read. Rebuild it and try again.".into(),
        None => error.to_string(),
    }
}

fn gap(root: &Path) {
    let attempt = || -> anyhow::Result<()> {
        let root = checked(Directory::open(&crate::platform::canonical(root)?))?;
        let dir = checked(root.ensure_child("listing-metadata"))?;
        checked(
            dir.replace_file(
                "coverage-lost.json",
                format!(
                    "{{\"version\":1,\"complete\":false,\"nonce\":\"{}\"}}\n",
                    uuid::Uuid::new_v4()
                )
                .as_bytes(),
            ),
        )
    };
    let _ = attempt();
}

/// Canonical I/O runs exactly once even if advisory durability is unavailable.
/// Unknown/failed canonical calls leave pending intents; repair never replays.
pub(super) fn write_at<T>(
    root: &Path,
    path: &Path,
    record: &Value,
    source: impl FnOnce(Option<&mut Publication>) -> Result<T>,
) -> Result<T> {
    let Some(key) = Key::at(root, path) else {
        return source(None);
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if super::effort::picker_entered() {
        return source(None);
    }
    if !enabled(root) {
        return source(None);
    }
    publish(root, &key, Some(record), source)
}
pub(super) fn remove_at<T>(
    root: &Path,
    path: &Path,
    source: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let Some(key) = Key::at(root, path) else {
        return source();
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if super::effort::picker_entered() {
        return source();
    }
    if !enabled(root) {
        return source();
    }
    publish(root, &key, None, |_| source())
}
fn publish<T>(
    root: &Path,
    key: &Key,
    record: Option<&Value>,
    source: impl FnOnce(Option<&mut Publication>) -> Result<T>,
) -> Result<T> {
    let prepare = || -> anyhow::Result<(Store, String, bool)> {
        // An unsupported projection is still a canonical change. Reserve a
        // small unknown intent; never publish it as a display row or absence.
        let (seed, supported) = match record.map(|r| Seed::encode(key, r)).transpose() {
            Ok(seed) => (seed, true),
            Err(_) => (Some("{\"version\":1,\"unavailable\":true}".into()), false),
        };
        let mut store = Store::open(root)?;
        let id = store.reserve(key, seed.as_deref())?;
        Ok((store, id, supported))
    };
    let prepared = prepare();
    if !prepared.as_ref().is_ok_and(|(_, _, supported)| *supported) {
        gap(root);
    }
    let mut publication = Publication::default();
    let witness = (record.is_some() && prepared.as_ref().is_ok_and(|(_, _, supported)| *supported))
        .then_some(&mut publication);
    let result = source(witness);
    if let Ok((mut store, id, supported)) = prepared {
        if result.is_ok() && supported {
            let settled = (|| -> anyhow::Result<()> {
                store.verify()?;
                let stamp = if record.is_some() {
                    Some(publication.prove(&store, key)?)
                } else {
                    let stamp = store.source_stamp(key)?;
                    anyhow::ensure!(stamp.is_none(), "Canonical removal did not settle");
                    None
                };
                store.settle(&id, stamp.as_deref())
            })();
            if settled.is_err() {
                gap(root);
            }
        }
    }
    result
}

// Upstream merge 2026-11-17: the listing index is active on Linux only
// (docs/reference.md: macOS/Windows activation is a separate follow-up).
#[cfg(all(test, not(windows)))]
mod tests;
