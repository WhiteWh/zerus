//! Best-effort cold-start reclamation. No recursive deletion and no age-based
//! authority. A private directory census also caps new overlay allocation.
use super::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn before_start(_: &Path) -> bool {
    false
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn service_pin(_: u32, _: &str) -> Option<Value> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use std::{
        ffi::CString,
        io,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::MetadataExt,
        },
    };
    const FOLDERS: usize = 256;
    const REMOVALS: usize = 16;

    fn uncertain() -> io::Error {
        io::Error::other("context plugin cleanup incomplete")
    }
    fn check(until: Instant) -> io::Result<()> {
        if Instant::now() >= until {
            Err(uncertain())
        } else {
            Ok(())
        }
    }
    fn open(at: &fs::File, name: &str, directory: bool) -> io::Result<fs::File> {
        let name = CString::new(name)?;
        // SAFETY: held directory descriptor and a NUL-terminated single name.
        let fd = unsafe {
            libc::openat(
                at.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | if directory { libc::O_DIRECTORY } else { 0 },
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let valid = if directory {
            // Legacy provider parents can be readable by others. They must
            // still be owned and writable only by this user, and never links.
            let m = file.metadata()?;
            Ok(m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0)
        } else {
            crate::platform::verify_private_file(&file)
        }?;
        if !valid {
            return Err(uncertain());
        }
        Ok(file)
    }
    fn bytes(at: &fs::File, name: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        open(at, name, false)?.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(uncertain());
        }
        Ok(bytes)
    }
    fn unlink(at: &fs::File, name: &str, directory: bool) -> io::Result<()> {
        let name = CString::new(name)?;
        if unsafe {
            libc::unlinkat(
                at.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    // Only one known service matters to each unique launch-owned folder. No
    // unrelated host process or environment can make this decision uncertain.
    fn known_gone(pid: u32, expected: &str, until: Instant) -> bool {
        if check(until).is_err() {
            return false;
        }
        match crate::platform::presence(pid) {
            crate::platform::Presence::Gone => true,
            crate::platform::Presence::Unknown => false,
            crate::platform::Presence::Present => match birth(pid) {
                Some(current) => current != expected,
                None => retired(pid, until),
            },
        }
    }
    fn valid_birth(value: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            value
                .parse::<u64>()
                .ok()
                .is_some_and(|n| n > 0 && n.to_string() == value)
        }
        #[cfg(target_os = "macos")]
        {
            value
                .split_once(':')
                .and_then(|(s, u)| Some((s.parse::<u64>().ok()?, u.parse::<u32>().ok()?)))
                .is_some_and(|(s, u)| s > 0 && u < 1_000_000 && format!("{s}:{u}") == value)
        }
    }
    fn reclaimable(owner: &Value, until: Instant) -> bool {
        if owner["revision"] != 2 {
            return false;
        }
        match owner["launch"].as_str() {
            // The start intent is persisted BEFORE spawn, under the same start
            // lock as this sweep. A prepared folder cannot have launched code.
            Some("prepared") => owner.get("service").is_none(),
            Some("bound") => {
                if owner["service"]["id"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 128)
                {
                    return false;
                }
                let Some(pid) = owner["service"]["pid"]
                    .as_u64()
                    .filter(|p| *p > 1 && *p <= i32::MAX as u64)
                else {
                    return false;
                };
                let Some(birth) = owner["service"]["birth"]
                    .as_str()
                    .filter(|b| b.len() <= 128 && valid_birth(b))
                else {
                    return false;
                };
                known_gone(pid as u32, birth, until)
            }
            _ => false, // legacy, missing/malformed pin or uncertain start
        }
    }
    pub(super) fn service_pin(pid: u32, expected: &str) -> Option<Value> {
        let before = birth(pid)?;
        let environment = checked_environment(pid, &before, |pid| {
            crate::state::task_reconcile::environment(pid).map_err(|_| uncertain())
        })
        .ok()?;
        let mut values = environment
            .split(|b| *b == 0)
            .filter_map(|s| s.strip_prefix(b"OPENCODE_CONFIG_CONTENT="));
        if values.next()? != expected.as_bytes() || values.next().is_some() {
            return None;
        }
        Some(json!({"pid":pid,"birth":before}))
    }
    fn checked_environment(
        pid: u32,
        before: &str,
        read: impl FnOnce(u32) -> io::Result<Vec<u8>>,
    ) -> io::Result<Vec<u8>> {
        let environment = read(pid)?;
        if birth(pid).as_deref() != Some(before) {
            return Err(uncertain());
        }
        Ok(environment)
    }
    #[cfg(target_os = "linux")]
    fn retired(pid: u32, until: Instant) -> bool {
        fn zombie(path: &Path) -> Option<String> {
            let mut raw = String::new();
            fs::File::open(path)
                .ok()?
                .take(65537)
                .read_to_string(&mut raw)
                .ok()?;
            if raw.len() > 65536 {
                return None;
            }
            let fields: Vec<_> = raw.rsplit_once(')')?.1.split_whitespace().collect();
            matches!(fields.first(), Some(&"Z" | &"X"))
                .then(|| fields.get(19).map(|s| s.to_string()))
                .flatten()
        }
        let path = PathBuf::from(format!("/proc/{pid}"));
        let Some(before) = zombie(&path.join("stat")) else {
            return false;
        };
        let Ok(tasks) = fs::read_dir(path.join("task")) else {
            return false;
        };
        let mut count = 0;
        for task in tasks {
            count += 1;
            if count > 1024
                || check(until).is_err()
                || task
                    .ok()
                    .and_then(|t| zombie(&t.path().join("stat")))
                    .is_none()
            {
                return false;
            }
        }
        // An exited Linux leader can still have live workers. Only a complete
        // all-zombie task inventory is retired; a missing identity alone isn't.
        count > 0 && zombie(&path.join("stat")).as_ref() == Some(&before)
    }
    #[cfg(target_os = "macos")]
    fn retired(_: u32, _: Instant) -> bool {
        false
    }
    fn remove(parent: &fs::File, path: &Path, name: &str, until: Instant) -> io::Result<()> {
        check(until)?;
        let folder = open(parent, name, true)?;
        // These exact files are the entire asset. Never walk unknown data,
        // follow a link or recursively delete a folder that gained other files.
        let names = fs::read_dir(path)?
            .take(4)
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        let owned = names.iter().any(|n| n == "owner.json");
        if !owned
            || names.is_empty()
            || names.len() > 3
            || names
                .iter()
                .any(|n| n != "index.js" && n != "package.json" && n != "owner.json")
        {
            return Err(uncertain());
        }
        let owner: Value =
            serde_json::from_slice(&bytes(&folder, "owner.json")?).map_err(|_| uncertain())?;
        if owner["nonce"] != name || !reclaimable(&owner, until) {
            return Err(uncertain());
        }
        let digest = owner["sha256"]
            .as_str()
            .filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or_else(uncertain)?;
        if names.iter().any(|n| n == "index.js")
            && digest != format!("{:x}", Sha256::digest(bytes(&folder, "index.js")?))
        {
            return Err(uncertain());
        }
        if names.iter().any(|n| n == "package.json")
            && bytes(&folder, "package.json")? != PACKAGE.as_bytes()
        {
            return Err(uncertain());
        }
        // The pathname may have been replaced during validation. Unlink files
        // only through the held original descriptor; never through that path.
        let current = open(parent, name, true)?;
        if (current.metadata()?.dev(), current.metadata()?.ino())
            != (folder.metadata()?.dev(), folder.metadata()?.ino())
        {
            return Err(uncertain());
        }
        check(until)?;
        for name in ["index.js", "package.json"] {
            if names.iter().any(|n| n == name) {
                unlink(&folder, name, false)?;
            }
        }
        if owned {
            unlink(&folder, "owner.json", false)?;
        }
        unlink(parent, name, true)
    }
    pub(super) fn sweep(state: &Path, until: Instant) -> io::Result<bool> {
        let state_fd = fs::File::open(state)?;
        let metadata = state_fd.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o022 != 0
        {
            return Err(uncertain());
        }
        let directory = state.join("opencode/context-hint/plugins");
        let mut parent = state_fd;
        for name in ["opencode", "context-hint", "plugins"] {
            parent = match open(&parent, name, true) {
                Ok(parent) => parent,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
                Err(e) => return Err(e),
            };
        }
        let mut entries = Vec::new();
        let mut complete = true;
        for entry in fs::read_dir(&directory)? {
            check(until)?;
            if entries.len() == FOLDERS {
                complete = false;
                break;
            }
            entries.push(entry?.file_name());
        }
        // With no assets there is nothing to reclaim or inspect.
        if entries.is_empty() {
            return Ok(true);
        }
        let marker = read(&state.join("opencode/context-hint/service.json"), 16384);
        let current = marker.as_ref().and_then(|m| m["nonce"].as_str());
        let mut removed = 0;
        {
            for name in &entries {
                check(until)?;
                let current_parent = fs::symlink_metadata(&directory)?;
                let held_parent = parent.metadata()?;
                if !current_parent.is_dir()
                    || (current_parent.dev(), current_parent.ino())
                        != (held_parent.dev(), held_parent.ino())
                {
                    return Err(uncertain());
                }
                if removed == REMOVALS {
                    break;
                }
                let Some(name) = name.to_str().filter(|n| uuid::Uuid::parse_str(n).is_ok()) else {
                    continue;
                };
                let path = directory.join(name);
                if current == Some(name) {
                    continue;
                }
                if remove(&parent, &path, name, until).is_ok() {
                    removed += 1;
                }
            }
        }
        // A cap is allocation policy, never deletion authority. Unknown assets
        // remain retained without hiding a known dead neighbour.
        Ok(complete && entries.len().saturating_sub(removed) < FOLDERS)
    }
    pub(super) fn before_start(state: &Path) -> bool {
        sweep(state, Instant::now() + Duration::from_millis(100)).unwrap_or(false)
    }

    #[cfg(test)]
    mod tests;
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn before_start(state: &Path) -> bool {
    native::before_start(state)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn service_pin(pid: u32, expected: &str) -> Option<Value> {
    native::service_pin(pid, expected)
}
