//! Pinned acquisition directories. No cleanup follows a mutable pathname.
use super::*;
use std::fs::{File, OpenOptions};

pub(in crate::state) struct Directory {
    pub path: PathBuf,
    file: File,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}

impl Directory {
    /// Existing-directory reads share acquisition's pinned, no-link walk.
    /// No directory, lock or file is created by these methods.
    pub(in crate::state) fn private(&self) -> Result<bool> {
        crate::platform::verify_private_directory(&self.file).map_err(|e| e.to_string())
    }

    pub(in crate::state) fn read_private(&self, name: &str) -> Result<File> {
        if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
            return Err("Invalid existing file name".into());
        }
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = c_name(std::ffi::OsStr::new(name))?;
            // SAFETY: the parent is pinned; name is one NUL-free component.
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_NOCTTY
                        | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // SAFETY: this call owns the new descriptor.
            unsafe { File::from_raw_fd(fd) }
        };
        #[cfg(windows)]
        let file = crate::platform::no_follow(OpenOptions::new().read(true))
            .open(self.path.join(name))
            .map_err(|e| e.to_string())?;
        if !crate::platform::verify_private_file(&file).map_err(|e| e.to_string())? {
            return Err("The existing file must be private and regular".into());
        }
        Ok(file)
    }

    pub fn verify(&self) -> Result<()> {
        let current = Self::open(&self.path)?;
        let identity = |file: &File| {
            crate::platform::FileIdentity::of(file)
                .map(|id| (id.device, id.inode))
                .map_err(|e| e.to_string())
        };
        if identity(&self.file)? != identity(&current.file)? {
            return Err("Clone directory identity changed; inspect the retained folders".into());
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|e| e.to_string())?;
        // Walk from a pinned root; intermediate symlinks cannot redirect opens.
        for component in path.components() {
            match component {
                std::path::Component::RootDir => (),
                std::path::Component::Normal(name) => file = open_at(&file, name)?,
                _ => return Err("Clone directory is not canonical".into()),
            }
        }
        Ok(Self {
            path: path.into(),
            file,
        })
    }

    pub fn create(&self, name: &str, staging: &str) -> Result<Self> {
        use std::os::fd::AsRawFd;
        self.verify()?;
        let from = c_name(std::ffi::OsStr::new(staging))?;
        let to = c_name(std::ffi::OsStr::new(name))?;
        // Pin the private, unpredictable staging directory before publishing the
        // selected destination name. A visible destination can never replace the
        // descriptor subsequently inherited by Git. Keep partial staging paths.
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), from.as_ptr(), 0o700) } != 0 {
            return Err(format!(
                "Cannot create clone staging folder: {}",
                std::io::Error::last_os_error()
            ));
        }
        let file = open_at(&self.file, std::ffi::OsStr::new(staging))?;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(
                "Clone staging folder is no longer private; inspect the retained folder".into(),
            );
        }
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                self.file.as_raw_fd(),
                from.as_ptr(),
                self.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::renameatx_np(
                self.file.as_raw_fd(),
                from.as_ptr(),
                self.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let rc = -1; // Refuse publication without an atomic no-replace primitive.
        if rc != 0 {
            return Err(format!(
                "Cannot reserve destination; staging folder retained: {}",
                std::io::Error::last_os_error()
            ));
        }
        let directory = Self {
            path: self.path.join(name),
            file,
        };
        self.verify()?;
        directory.verify()?;
        Ok(directory)
    }

    pub fn anchor(&self, command: &mut Command) -> Result<()> {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let fd = self.file.as_raw_fd();
        // Only async-signal-safe work between fork and exec. The owned file is
        // alive until spawn returns; CLOEXEC closes it after fchdir succeeds.
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(fd) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
fn c_name(name: &std::ffi::OsStr) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn open_at(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "Cannot pin clone directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut files = Vec::new();
        // Retain every ancestor without delete/write sharing. Besides renames,
        // refuse concurrent write handles that can change reparse data.
        for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .share_mode(FILE_SHARE_READ)
                .open(ancestor)
                .map_err(|e| format!("Cannot pin clone directory: {e}"))?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err("Clone path changed or contains a reparse point".into());
            }
            files.push(file);
        }
        let file = files.pop().ok_or("Missing clone directory")?;
        Ok(Self {
            path: path.into(),
            file,
            _ancestors: files,
        })
    }

    pub fn create(&self, name: &str, _staging: &str) -> Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::{
            Wdk::{Foundation::OBJECT_ATTRIBUTES, Storage::FileSystem::*},
            Win32::{
                Foundation::{HANDLE, UNICODE_STRING},
                Storage::FileSystem::*,
                System::IO::IO_STATUS_BLOCK,
            },
        };
        self.verify()?;
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        let string = UNICODE_STRING {
            Length: (wide.len() * 2) as u16,
            MaximumLength: (wide.len() * 2) as u16,
            Buffer: wide.as_mut_ptr(),
        };
        let attrs = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.file.as_raw_handle(),
            ObjectName: &string,
            Attributes: 0x40, // OBJ_CASE_INSENSITIVE
            SecurityDescriptor: std::ptr::null_mut(),
            SecurityQualityOfService: std::ptr::null_mut(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // FILE_CREATE returns a handle to the new directory atomically. There
        // is no create/reopen window, nor delete sharing during native Git.
        let code = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | 0x100000,
                &attrs,
                &mut status,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ,
                FILE_CREATE,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                std::ptr::null(),
                0,
            )
        };
        if code < 0 {
            return Err(format!(
                "Cannot reserve clone directory (NTSTATUS {code:#x})"
            ));
        }
        let file = unsafe { File::from_raw_handle(handle) };
        let directory = Self {
            path: self.path.join(name),
            file,
            _ancestors: Vec::new(),
        };
        directory.verify()?;
        Ok(directory)
    }

    pub fn anchor(&self, command: &mut Command) -> Result<()> {
        self.verify()?;
        command.current_dir(&self.path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_destination_is_never_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = crate::platform::canonical(temp.path()).unwrap();
        let parent = Directory::open(&path).unwrap();
        std::fs::create_dir(path.join("existing")).unwrap();
        std::fs::write(path.join("existing/foreign"), "retained").unwrap();
        assert!(parent.create("existing", ".staging").is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("existing/foreign")).unwrap(),
            "retained"
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_replacement_before_creation_refuses_all_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let path = crate::platform::canonical(temp.path()).unwrap();
        std::fs::create_dir(path.join("parent")).unwrap();
        let parent = Directory::open(&path.join("parent")).unwrap();
        std::fs::rename(path.join("parent"), path.join("owned")).unwrap();
        std::fs::create_dir(path.join("parent")).unwrap();
        std::fs::write(path.join("parent/foreign"), "retained").unwrap();
        assert!(parent.create("checkout", ".staging").is_err());
        assert_eq!(std::fs::read_dir(path.join("parent")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(path.join("owned")).unwrap().count(), 0);
        // Causal control: the former pathname-based mkdir accepts this replacement.
        std::fs::create_dir(path.join("parent/checkout")).unwrap();
        assert!(path.join("parent/checkout").is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn pinned_ancestors_and_destination_deny_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = crate::platform::canonical(temp.path()).unwrap();
        std::fs::create_dir(path.join("parent")).unwrap();
        let parent = Directory::open(&path.join("parent")).unwrap();
        let owned = parent.create("checkout", "unused").unwrap();
        assert!(std::fs::rename(&owned.path, path.join("moved-child")).is_err());
        assert!(std::fs::rename(&parent.path, path.join("moved-parent")).is_err());
        owned.verify().unwrap();
        drop(owned);
        drop(parent);
        // Causal control: identical renames succeed after releasing the handles.
        std::fs::rename(path.join("parent/checkout"), path.join("moved-child")).unwrap();
        std::fs::rename(path.join("parent"), path.join("moved-parent")).unwrap();
    }
}
