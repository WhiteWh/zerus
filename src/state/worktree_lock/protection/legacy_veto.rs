//! Explicit legacy advisory-veto proof. This never establishes native-private
//! root authority and never creates, rewrites, chmods or relocks a Git veto.
use super::*;

pub(super) const VERSION: u64 = 2;
pub(super) const KIND: &str = "legacy-advisory-veto";

#[cfg(unix)]
pub(super) fn version(file: &fs::File) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: geteuid reads only the current process's effective user ID.
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o600 != 0o600
        || m.mode() & 0o7022 != 0
    {
        return Err("The original legacy Git veto is not exclusively owner-writable".into());
    }
    // ctime detects an unlock/relock, temporary link or mode change even if
    // the final text/mode match; a live held descriptor prevents inode reuse.
    Ok(json!([
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec()
    ]))
}
#[cfg(not(unix))]
pub(super) fn version(_: &fs::File) -> Result<Value> {
    Err("Legacy advisory-veto continuity is unavailable on this computer".into())
}

#[cfg(unix)]
pub(super) fn open(metadata: &Directory) -> Result<fs::File> {
    use std::os::fd::FromRawFd;
    let name = std::ffi::CString::new("locked").unwrap();
    // SAFETY: one fixed component under the original no-follow directory.
    // This requests no creation, truncation, link-follow or blocking FIFO open.
    let fd = unsafe {
        libc::openat(
            metadata.descriptor(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: this call exclusively owns the returned descriptor.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    version(&file)?;
    metadata.verify()?;
    Ok(file)
}
#[cfg(not(unix))]
pub(super) fn open(_: &Directory) -> Result<fs::File> {
    Err("Legacy advisory-veto continuity is unavailable on this computer".into())
}

impl Protection {
    pub(super) fn retained_any(
        owner: &Value,
        expected: Option<&str>,
        allow_missing: bool,
    ) -> Result<Self> {
        let state = state()?;
        let receipts = state.child("worktree-protections")?;
        let proof =
            read_receipt(&receipts, &receipt(owner))?.ok_or("Missing original protective proof")?;
        if proof["version"] != VERSION {
            // The complete v1 private-file verifier is unchanged.
            return Self::retained(owner, expected, allow_missing);
        }
        if !owner_matches(owner) {
            return Err("The original checkout ownership changed".into());
        }
        let common = Directory::open(Path::new(string(owner, "common_dir")))?;
        let metadata = Directory::open(Path::new(string(owner, "metadata")))?;
        let checkout = match Directory::open(Path::new(string(owner, "path"))) {
            Ok(checkout) => Some(checkout),
            Err(_)
                if allow_missing
                    && fs::symlink_metadata(string(owner, "path"))
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(e) => return Err(e),
        };
        let repository = worktree_cleanup::repository_lock(&common.path)?;
        repository
            .try_lock_exclusive()
            .map_err(|_| "The original checkout is busy")?;
        let repository = crate::platform::FileLock::held(repository);
        #[cfg(all(test, unix))]
        crate::platform::lock_tests::reached("protection-legacy");
        let mut lock = open(&metadata)?;
        let reason = text(&mut lock)?
            .strip_prefix("zerus:")
            .and_then(|s| s.strip_suffix('\n'))
            .filter(|id| session_id::canonical(id))
            .ok_or("The original legacy Git veto changed")?
            .to_owned();
        if expected.is_some_and(|id| id != reason) {
            return Err("The original veto belongs to another session".into());
        }
        let held = Self {
            owner: owner.clone(),
            state,
            receipts,
            common,
            checkout,
            metadata,
            _repository: repository,
            receipt: receipt(owner),
            before: proof,
            lock,
            reason,
        };
        held.verify_legacy()?;
        Ok(held)
    }

    pub(super) fn verify_legacy(&self) -> Result<()> {
        self.state.verify()?;
        self.receipts.verify()?;
        self.common.verify()?;
        self.checkout_current()?;
        self.metadata.verify()?;
        if crate::platform::canonical(root()).map_err(|e| e.to_string())? != self.state.path
            || !owner_matches(&self.owner)
            || read_receipt(&self.receipts, &self.receipt)?.as_ref() != Some(&self.before)
            || self.before["version"] != VERSION
            || self.before["kind"] != KIND
            || self.before["status"] != "ready"
            || self.before["owner"] != self.owner
            || self.before["state_object"] != json!(self.state.object()?)
            || self.before["receipt_object"] != json!(self.receipts.object()?)
            || self.before["common_object"] != json!(self.common.object()?)
            || self.before["metadata_object"] != json!(self.metadata.object()?)
        {
            return Err("The original legacy veto proof changed".into());
        }
        if let Some(checkout) = &self.checkout {
            if self.before["checkout_object"] != json!(checkout.object()?) {
                return Err("The original checkout changed".into());
            }
        } else if !self.before["checkout_object"]
            .as_array()
            .is_some_and(|a| a.len() == 2 && a.iter().all(|v| v.as_u64().is_some()))
        {
            return Err("The original checkout identity is unavailable".into());
        }
        let mut current = open(&self.metadata)?;
        let actual = version(&current)?;
        if self.before["lock_object"] != object(&self.lock)?
            || self.before["lock_object"] != object(&current)?
            || self.before["lock_version"] != version(&self.lock)?
            || self.before["lock_version"] != actual
            || self.before["lock_mode"] != actual[2]
            || text(&mut current)? != format!("zerus:{}\n", self.reason)
        {
            return Err(
                "The original legacy Git veto changed; retain the binding for review".into(),
            );
        }
        Ok(())
    }
}
