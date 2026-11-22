use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

// A snapshot is prepared before any native proof is registered. These are
// copy bounds, not an extension of a caller's proof or execution deadline.
const MAX_IMAGE_BYTES: u64 = 256 << 20;
const COPY_WAIT: Duration = Duration::from_secs(5);
const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

fn sealed(file: &File) -> bool {
    // An ordinary file, unsupported kernel or incomplete seal set refuses.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    seals >= 0 && seals & SEALS == SEALS
}

fn snapshot(source: &mut File, expected: &Stamp) -> io::Result<File> {
    if expected.bytes == 0 || expected.bytes > MAX_IMAGE_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // Explicit executable intent also refuses when the kernel's memfd policy
    // forbids executable snapshots. Never fall back to a pathname/ordinary fd.
    // MFD_EXEC is Linux UAPI 0x0010 (since 6.3). Older kernels refuse this path.
    const MFD_EXEC: libc::c_uint = 0x0010;
    let fd = unsafe {
        libc::memfd_create(
            c"zerus-helper-image".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC | MFD_EXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned one new owned fd, not a borrowed handle.
    let mut image = unsafe { File::from_raw_fd(fd) };
    let until = Instant::now() + COPY_WAIT;
    let mut remaining = expected.bytes;
    let mut buffer = [0; 64 << 10];
    while remaining > 0 {
        if Instant::now() >= until {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let n = remaining.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..n])?;
        image.write_all(&buffer[..n])?;
        remaining -= n as u64;
    }
    #[cfg(test)]
    tests::after_copy();
    let mut tail = [0];
    if source.read(&mut tail)? != 0 || stamp(source)? != *expected || Instant::now() >= until {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // No writable mappings or foreign handles are published before this seal.
    // F_SEAL_WRITE (not merely FUTURE_WRITE) rules out existing writable maps.
    if unsafe { libc::fcntl(image.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if !sealed(&image) || image.metadata()?.len() != expected.bytes || Instant::now() >= until {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(image)
}

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
}
fn stamp(file: &File) -> io::Result<Stamp> {
    let m = file.metadata()?;
    if !m.is_file() || m.ino() == 0 || m.mode() & 0o111 == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(Stamp {
        device: m.dev(),
        inode: m.ino(),
        bytes: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
        mode: m.mode(),
    })
}
fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    if !m.is_dir() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok((m.dev(), m.ino()))
}
struct Folder {
    path: PathBuf,
    file: File,
    id: (u64, u64),
}
impl Folder {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let id = identity(&file)?;
        Ok(Self {
            path: path.into(),
            file,
            id,
        })
    }
    fn current(&self) -> bool {
        identity(&self.file).ok() == Some(self.id)
            && Self::open(&self.path).is_ok_and(|now| now.id == self.id)
    }
}
pub struct Pin {
    requested: PathBuf,
    resolved: PathBuf,
    source: File,
    image: File,
    original: Stamp,
    program: PathBuf,
    script: bool,
    folders: Vec<Folder>,
}

/// Correlation from this snapshot's SAME unreaped native child. This supplies
/// no role, consent or exit witness, and cannot be constructed from a PID.
pub struct OriginalChild {
    pid: u32,
    parent: u32,
    birth: String,
}
impl OriginalChild {
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn parent(&self) -> u32 {
        self.parent
    }
    pub fn birth(&self) -> &str {
        &self.birth
    }
}

fn child_identity(child: &Child) -> io::Result<(u32, String)> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{}/stat", child.id()))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let fields = text
        .rsplit_once(") ")
        .ok_or(io::ErrorKind::InvalidData)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let parent = fields
        .get(1)
        .ok_or(io::ErrorKind::InvalidData)?
        .parse::<u32>()
        .map_err(io::Error::other)?;
    let birth = *fields.get(19).ok_or(io::ErrorKind::InvalidData)?;
    if parent != std::process::id()
        || birth.len() > 20
        || birth.is_empty()
        || !birth.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok((parent, birth.into()))
}
impl Pin {
    /// Capture one immutable execution image. The fixed `/proc/self/exe` input
    /// opens the kernel's running object itself, not a reopened installed name.
    /// `current` also requires that the selected name still denotes that source.
    /// This type supplies neither native relay authentication nor consent.
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let resolved = path.canonicalize()?;
        let ancestors = resolved.ancestors().skip(1).collect::<Vec<_>>();
        if ancestors.len() > 128 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let folders = ancestors
            .into_iter()
            .rev()
            .map(Folder::open)
            .collect::<io::Result<Vec<_>>>()?;
        let mut source = if path == Path::new("/proc/self/exe") {
            File::open("/proc/self/exe")?
        } else {
            open(&resolved)?
        };
        let original = stamp(&source)?;
        use std::os::unix::fs::FileExt;
        let mut prefix = [0; 2];
        let script = source.read_at(&mut prefix, 0)? == 2 && &prefix == b"#!";
        let snapshot = snapshot(&mut source, &original)?;
        // The immutable snapshot is created ONCE, before help/ABI probes. Its
        // bytes, not the mutable selected inode, supply every later execution.
        // One descriptor, never 0-2 and not inherited by unrelated spawns.
        let fd = unsafe { libc::fcntl(snapshot.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a new owned descriptor.
        let image = unsafe { File::from_raw_fd(fd) };
        let pin = Self {
            requested: path.into(),
            resolved,
            source,
            image,
            original,
            program: format!("/proc/self/fd/{fd}").into(),
            script,
            folders,
        };
        if !pin.current() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(pin)
    }
    pub fn current(&self) -> bool {
        self.requested
            .canonicalize()
            .is_ok_and(|p| p == self.resolved)
            && stamp(&self.source).ok().as_ref() == Some(&self.original)
            && open(&self.resolved).and_then(|f| stamp(&f)).ok().as_ref() == Some(&self.original)
            && self.folders.iter().all(Folder::current)
            && sealed(&self.image)
    }
    pub fn executable(&self) -> &Path {
        &self.program
    }
    /// The canonical path this image was copied from, which `current` checks
    /// still holds the same file. A child executing [`Self::executable`] runs
    /// a sealed memfd: its own `/proc/self/exe` names no file, so anything it
    /// hands to another program to run (a terminal's command) needs this path.
    pub fn source(&self) -> &Path {
        &self.resolved
    }
    /// Observe the actual kernel exe object while the original child still
    /// owns its unreleased input. Scripts cannot establish this native-image
    /// identity: their executing object would be an interpreter. On refusal
    /// close stdin and return the SAME Child to its owner's ordinary reaper.
    /// A later socket borrower still needs fresh kernel object/birth checks.
    pub fn observe_original_child(&self, child: &mut Child) -> io::Result<OriginalChild> {
        let result = (|| {
            if self.script || child.stdin.is_none() || child.try_wait()?.is_some() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let before = child_identity(child)?;
            let actual = File::open(format!("/proc/{}/exe", child.id()))?;
            let expected = stamp(&self.image)?;
            if !sealed(&actual)
                || stamp(&actual)? != expected
                || child_identity(child)? != before
                || child.try_wait()?.is_some()
                || stamp(&File::open(format!("/proc/{}/exe", child.id()))?)? != expected
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(OriginalChild {
                pid: child.id(),
                parent: before.0,
                birth: before.1,
            })
        })();
        if result.is_err() {
            drop(child.stdin.take());
        }
        result
    }
    pub fn inherit(&self, command: &mut Command) -> io::Result<()> {
        if command.get_program() != self.executable() || !sealed(&self.image) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let fd = self.image.as_raw_fd();
        command.arg0(&self.requested);
        if !self.script {
            // Native ELF resolves its executable descriptor before exec closes
            // CLOEXEC files. It needs no inherited execution descriptor.
            return Ok(());
        }
        // SAFETY: allocation-free fcntl only after fork. No parent flags change;
        // the live Context retains this descriptor through spawn and all I/O.
        // Scripts need this one descriptor after exec to read their exact source.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

mod relay;
pub use relay::{PeerImage, ServiceImage};
