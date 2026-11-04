//! Owned startup exclusion prerequisite, never startup or role authority alone.
//! No command/frontend caller holds this yet; it must span actual writer startup.
use super::{
    handoff,
    store::{self, HandoffLock, Location},
};
use anyhow::{ensure, Context, Result};
use std::{fs, io};
use zerus_desktop_migration::Phase;

/// No Clone, serialization or lock extraction. An absence observation has no
/// existing boundary to hold and never attests that a producer is ready/quiet.
pub(super) struct Startup {
    _lock: Option<HandoffLock>,
    configuration: String,
}
impl Startup {
    /// Off the GUI thread. Never create a desktop directory/database/service.
    /// Known existing private passive state may create its existing lock leaf.
    pub fn hold(location: &Location) -> Result<Self> {
        ensure!(cfg!(unix), "desktop startup exclusion is unsupported");
        location.check_existing()?;
        match fs::symlink_metadata(&location.root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    _lock: None,
                    configuration: location.configuration.clone(),
                });
            }
            result => {
                result?;
            }
        }
        // Negative preflight prevents lock-file creation in incomplete/unknown
        // state. Only the fresh repeated read under the held lock is relevant.
        ensure!(
            store::writer_active(location)? == Some(false),
            "desktop startup state is not passive"
        );
        let lock = location.handoff_lock()?;
        ensure!(
            store::writer_active(location)? == Some(false),
            "desktop startup state changed"
        );
        let marker = handoff::marker(location, &lock)?;
        ensure!(
            marker.as_ref().is_none_or(|m| m.phase == Phase::Revoked),
            "desktop handoff needs separately admitted frontend ownership"
        );
        lock.verify(location)
            .context("desktop startup scope changed")?;
        Ok(Self {
            _lock: Some(lock),
            configuration: location.configuration.clone(),
        })
    }
    pub fn configuration(&self) -> &str {
        &self.configuration
    }
}
impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Startup([owned exclusion])")
    }
}

#[cfg(all(test, unix))]
mod tests;
