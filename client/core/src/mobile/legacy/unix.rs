//! Descriptor-relative port of Qt RelayConfiguration::ConfigStore.
use super::*;
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
};
fn name(s: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}
fn opened(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful open/openat returns a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(path: &Path, create: bool, private_final: bool) -> io::Result<File> {
    if !clean(path) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // SAFETY: the literal is NUL terminated and flags require no vararg mode.
    let mut dir = opened(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    let parts: Vec<_> = path
        .components()
        .filter_map(|c| {
            if let Component::Normal(n) = c {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let part = name(part)?;
        // SAFETY: dir is live and part is a NUL-terminated single path component.
        let mut next = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 && io::Error::last_os_error().kind() == io::ErrorKind::NotFound && create {
            // SAFETY: same held directory and single component; creation is private.
            if unsafe { libc::mkdirat(dir.as_raw_fd(), part.as_ptr(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            dir.sync_all()?;
            // SAFETY: same live arguments as the preceding openat.
            next = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    part.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        dir = opened(next)?;
        let m = dir.metadata()?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if (m.uid() != uid && m.uid() != 0)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
            || (i + 1 == parts.len()
                && (m.uid() != uid || (private_final && m.mode() & 0o077 != 0)))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(dir)
}
fn read(dir: &File, file: &CString) -> io::Result<Snapshot> {
    // SAFETY: live descriptor, valid component and read-only no-follow flags.
    let result = opened(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            file.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    });
    let fd = match result {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::default()),
        Err(_) => return Err(io::Error::other(UNREADABLE)),
    };
    let m = fd.metadata().map_err(|_| io::Error::other(UNREADABLE))?;
    // SAFETY: geteuid has no preconditions.
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > 65536
    {
        return Err(io::Error::other(PRIVATE_FILE));
    }
    let mut bytes = Vec::new();
    fd.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| io::Error::other(UNREADABLE))?;
    if bytes.len() > 65536 {
        return Err(io::Error::other(PRIVATE_FILE));
    }
    let object: Value = serde_json::from_slice(&bytes).map_err(|_| io::Error::other(NOT_OBJECT))?;
    if !object.is_object() {
        return Err(io::Error::other(NOT_OBJECT));
    }
    Ok(Snapshot {
        object,
        bytes: Some(bytes),
        device: m.dev(),
        inode: m.ino(),
    })
}
fn parent(path: &Path, create: bool) -> io::Result<(File, CString)> {
    if !clean(path) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok((
        directory(
            path.parent().ok_or(io::ErrorKind::InvalidInput)?,
            create,
            true,
        )?,
        name(path.file_name().ok_or(io::ErrorKind::InvalidInput)?)?,
    ))
}
pub(super) fn load(path: &Path) -> Result<Snapshot, String> {
    match parent(path, false) {
        Ok((dir, file)) => read(&dir, &file).map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Snapshot::default()),
        Err(_) => Err(PRIVATE.into()),
    }
}
fn same(a: &Snapshot, b: &Snapshot) -> bool {
    a.bytes == b.bytes && a.device == b.device && a.inode == b.inode
}
pub(super) fn save(path: &Path, old: &Snapshot, prepared: Prepared) -> Result<Snapshot, String> {
    let (dir, file) = parent(path, true).map_err(|_| PRIVATE)?;
    if !same(&read(&dir, &file).map_err(|e| e.to_string())?, old) {
        return Err(CHANGED.into());
    }
    let bytes = serde_json::to_vec(&prepared.object).map_err(|_| UNKNOWN)?;
    if bytes.len() > 65536 {
        return Err("Configuration exceeds the supported size.".into());
    }
    let state = if prepared.new_directory {
        let path = Path::new(prepared.object["state_dir"].as_str().ok_or(PRIVATE)?);
        let parent = directory(path.parent().ok_or(PRIVATE)?, true, false).map_err(|_| PRIVATE)?;
        let child = name(path.file_name().ok_or(PRIVATE)?).map_err(|_| PRIVATE)?;
        // SAFETY: held parent and a single NUL-terminated component; mkdir is exclusive.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), child.as_ptr(), 0o700) } != 0 {
            return Err("Cannot allocate a new private receipt directory.".into());
        }
        Some((parent, child))
    } else {
        None
    };
    let nonce = crate::fsx::temporary(Path::new(".connector"));
    let tmp = name(nonce.as_os_str()).map_err(|_| UNKNOWN)?;
    let mut renamed = false;
    let mut created = false;
    let result = (|| -> io::Result<Snapshot> {
        // SAFETY: held directory, valid single name, exclusive private creation.
        let mut fd = opened(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        created = true;
        fd.write_all(&bytes)?;
        fd.sync_all()?;
        drop(fd);
        if !same(&read(&dir, &file)?, old) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // SAFETY: both names are components within the same held directory.
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                dir.as_raw_fd(),
                file.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        renamed = true;
        dir.sync_all()?;
        if let Some((parent, _)) = &state {
            parent.sync_all()?;
        }
        read(&dir, &file)
    })();
    // SAFETY: cleanup is restricted to our random temp, and our newly created empty
    // directory before rename; no prior receipt is ever removed.
    unsafe {
        if created {
            libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0);
        }
        if !renamed {
            if let Some((parent, child)) = &state {
                libc::unlinkat(parent.as_raw_fd(), child.as_ptr(), libc::AT_REMOVEDIR);
            }
        }
    }
    result.map_err(|_| UNKNOWN.into())
}
