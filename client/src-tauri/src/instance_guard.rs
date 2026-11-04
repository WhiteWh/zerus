//! Canonical GUI lifetime exclusion. This namespace never consults HOME,
//! TMPDIR, XDG paths, hgs configuration or the desktop-service GUI flag.
//! Keep the inode after exit: unlinking a flock file can split ownership.
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

pub fn directory() -> PathBuf {
    // /tmp is the OS namespace, deliberately not std::env::temp_dir(). On
    // macOS /tmp and /private/tmp name the same system-owned directory.
    PathBuf::from(format!("/tmp/com.hgdev.hgs-tray-{}", unsafe {
        libc::geteuid()
    }))
}

pub struct Guard {
    file: File,
    pub socket: PathBuf,
    inode: Option<u64>,
}

pub fn acquire(directory: &Path) -> io::Result<Option<Guard>> {
    match DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let uid = unsafe { libc::geteuid() };
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other("unsafe canonical GUI directory"));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(directory.join("gui.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(io::Error::other("unsafe canonical GUI lock"));
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(Guard {
            file,
            socket: directory.join("gui.sock"),
            inode: None,
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

impl Guard {
    pub fn bind(&mut self) -> io::Result<UnixListener> {
        // Only a kernel-lock owner may replace the crashed owner's socket.
        match fs::symlink_metadata(&self.socket) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == unsafe { libc::geteuid() } =>
            {
                fs::remove_file(&self.socket)?;
            }
            Ok(_) => return Err(io::Error::other("unsafe canonical GUI socket")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&self.socket)?;
        self.inode = Some(fs::symlink_metadata(&self.socket)?.ino());
        fs::set_permissions(
            &self.socket,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )?;
        Ok(listener)
    }

    fn remove_socket(&mut self) {
        if self.inode.is_some_and(|inode| {
            fs::symlink_metadata(&self.socket).is_ok_and(|metadata| metadata.ino() == inode)
        }) {
            let _ = fs::remove_file(&self.socket);
        }
        self.inode = None;
    }

    pub fn retain_until_exit(mut self) {
        self.remove_socket();
        // The final GUI exit hook closes listeners before the OS has ended
        // the process. Do not admit a replacement during that interval.
        std::mem::forget(self);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.remove_socket();
        // A concurrent fork may briefly retain a close-on-exec duplicate.
        // Explicit release also makes normal shutdown independent of that
        // transient inherited description. Crashes still release in kernel.
        let _ = self.file.unlock();
    }
}
