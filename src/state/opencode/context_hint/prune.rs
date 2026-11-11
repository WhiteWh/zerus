//! Best-effort cold-start reclamation. No recursive deletion and no age-based
//! authority. A private directory census also caps new overlay allocation.
use super::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn before_start(_: &Path) -> bool {
    false
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use std::{
        collections::BTreeSet,
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
    #[cfg(target_os = "linux")]
    fn pids(until: Instant) -> io::Result<Vec<u32>> {
        let mut rows = Vec::new();
        for (count, entry) in fs::read_dir("/proc")?.enumerate() {
            check(until)?;
            if count >= 32768 {
                return Err(uncertain());
            }
            let entry = entry?;
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            // /proc/PID can become root-owned when a same-user service makes
            // itself nondumpable. Use kernel UIDs, never that directory owner,
            // to decide which unreadable processes must protect all assets.
            let mut status = String::new();
            fs::File::open(entry.path().join("status"))?
                .take(65537)
                .read_to_string(&mut status)?;
            if status.len() > 65536 {
                return Err(uncertain());
            }
            let uids = status
                .lines()
                .find_map(|s| s.strip_prefix("Uid:"))
                .ok_or_else(uncertain)?
                .split_whitespace()
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| uncertain())?;
            if uids.len() != 4 {
                return Err(uncertain());
            }
            if uids.contains(&unsafe { libc::geteuid() }) {
                rows.push(pid);
                if rows.len() >= 4096 {
                    return Err(uncertain());
                }
            }
        }
        Ok(rows)
    }
    #[cfg(target_os = "macos")]
    fn pids(until: Instant) -> io::Result<Vec<u32>> {
        let mut rows = BTreeSet::new();
        for kind in [4, 5] {
            // effective and real UID, like native membership sampling
            check(until)?;
            let mut buffer = vec![0i32; 4096];
            let size = (buffer.len() * size_of::<i32>()) as i32;
            let used = unsafe {
                libc::proc_listpids(kind, libc::geteuid(), buffer.as_mut_ptr().cast(), size)
            };
            if used <= 0 || used >= size || !(used as usize).is_multiple_of(size_of::<i32>()) {
                return Err(uncertain());
            }
            rows.extend(
                buffer
                    .into_iter()
                    .take(used as usize / size_of::<i32>())
                    .filter(|p| *p > 0)
                    .map(|p| p as u32),
            );
            if rows.len() >= 4096 {
                return Err(uncertain());
            }
        }
        Ok(rows.into_iter().collect())
    }
    fn paths(environment: &[u8], until: Instant) -> io::Result<BTreeSet<PathBuf>> {
        let mut values = environment
            .split(|b| *b == 0)
            .filter_map(|s| s.strip_prefix(b"OPENCODE_CONFIG_CONTENT="));
        let Some(value) = values.next() else {
            return Ok(BTreeSet::new());
        };
        if values.next().is_some() {
            return Err(uncertain());
        }
        let config: Value = serde_json::from_slice(value).map_err(|_| uncertain())?;
        if !config.is_object() {
            return Err(uncertain());
        }
        let mut paths = BTreeSet::new();
        if let Some(plugins) = config.get("plugins") {
            let plugins = plugins.as_array().ok_or_else(uncertain)?;
            if plugins.len() > 64 {
                return Err(uncertain());
            }
            for plugin in plugins {
                check(until)?;
                let package = plugin
                    .as_str()
                    .or_else(|| plugin["package"].as_str())
                    .ok_or_else(uncertain)?;
                let path = PathBuf::from(package);
                if !path.is_absolute() {
                    return Err(uncertain());
                }
                // Resolve aliases too. An absent source is not in use; other
                // resolution errors cannot establish an unused directory.
                match fs::canonicalize(path) {
                    Ok(path) => {
                        paths.insert(path);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(paths)
    }
    fn live_paths(until: Instant) -> io::Result<BTreeSet<PathBuf>> {
        let mut paths_in_use = BTreeSet::new();
        let mut total = 0;
        let mut roster = pids(until)?;
        roster.sort_unstable();
        let mut identities = Vec::new();
        for &pid in &roster {
            check(until)?;
            let before = match birth(pid) {
                Some(before) => before,
                None if retired(pid, until) => continue,
                None => return Err(uncertain()),
            };
            let environment = checked_environment(pid, &before, |pid| {
                crate::state::task_reconcile::environment(pid).map_err(|_| uncertain())
            })?;
            identities.push((pid, before.clone()));
            total += environment.len();
            if total > 16 << 20 {
                return Err(uncertain());
            }
            paths_in_use.extend(paths(&environment, until)?);
            if birth(pid).as_ref() != Some(&before) {
                return Err(uncertain());
            }
        }
        let mut after = pids(until)?;
        after.sort_unstable();
        if after != roster {
            return Err(uncertain());
        }
        for (pid, before) in identities {
            check(until)?;
            if birth(pid).as_ref() != Some(&before) {
                return Err(uncertain());
            }
        }
        check(until)?;
        Ok(paths_in_use)
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
        if names.len() != if owned { 3 } else { 2 }
            || names
                .iter()
                .any(|n| n != "index.js" && n != "package.json" && n != "owner.json")
        {
            return Err(uncertain());
        }
        let script = bytes(&folder, "index.js")?;
        if bytes(&folder, "package.json")? != PACKAGE.as_bytes() {
            return Err(uncertain());
        }
        if owned {
            let owner: Value =
                serde_json::from_slice(&bytes(&folder, "owner.json")?).map_err(|_| uncertain())?;
            if owner["revision"] != 1
                || owner["nonce"] != name
                || owner["sha256"] != format!("{:x}", Sha256::digest(&script))
            {
                return Err(uncertain());
            }
        } else if script != include_bytes!("../context_plugin.js") {
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
            unlink(&folder, name, false)?;
        }
        if owned {
            unlink(&folder, "owner.json", false)?;
        }
        unlink(parent, name, true)
    }
    pub(super) fn sweep(
        state: &Path,
        until: Instant,
        live: impl FnOnce(Instant) -> io::Result<BTreeSet<PathBuf>>,
    ) -> io::Result<bool> {
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
        let live = live(until);
        let marker = read(&state.join("opencode/context-hint/service.json"), 16384);
        let current = marker.as_ref().and_then(|m| m["nonce"].as_str());
        let mut removed = 0;
        if let Ok(live) = live {
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
                if current == Some(name) || live.contains(&path) {
                    continue;
                }
                if remove(&parent, &path, name, until).is_ok() {
                    removed += 1;
                }
            }
        }
        // A cap is allocation policy, never deletion authority. If inspection
        // fails, retain every asset but do not keep growing beyond this limit.
        Ok(complete && entries.len().saturating_sub(removed) < FOLDERS)
    }
    pub(super) fn before_start(state: &Path) -> bool {
        sweep(
            state,
            Instant::now() + Duration::from_millis(100),
            live_paths,
        )
        .unwrap_or(false)
    }

    #[cfg(test)]
    mod tests;
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn before_start(state: &Path) -> bool {
    native::before_start(state)
}
