//! Test-only executable publication. Parent-process writes are inert data;
//! the only executable writer is a copy child that is reaped before use.
use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};

pub(crate) fn write_executable(path: &Path, contents: impl AsRef<[u8]>) {
    let name = path.file_name().expect("executable name").to_string_lossy();
    let source = path.with_file_name(format!(".{name}.source"));
    write_in_parent(&source, contents);
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            panic!("cannot replace {}: {error}", path.display())
        }
        _ => {}
    }
    // Rename would preserve the parent-writable inode. The copy child owns
    // the distinct executable writer and must exit before the image is used.
    let status = Command::new("/bin/cp")
        .arg(&source)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "cannot write {}", path.display());
    fs::remove_file(source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Parent-owned data only. The Linux control retains the real writer before
/// its close, reproducing a parallel fork without relying on scheduling.
pub(crate) fn write_in_parent(path: &Path, contents: impl AsRef<[u8]>) {
    let mut file = File::create(path).unwrap();
    file.write_all(contents.as_ref()).unwrap();
    #[cfg(target_os = "linux")]
    capture(&file, path);
}

#[cfg(target_os = "linux")]
thread_local! {
    static WRITERS: std::cell::RefCell<Option<Vec<Retained>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "linux")]
pub(crate) struct HeldWriters;
#[cfg(target_os = "linux")]
impl HeldWriters {
    pub(crate) fn arm() -> Self {
        WRITERS.with_borrow_mut(|writers| assert!(writers.replace(Vec::new()).is_none()));
        Self
    }
    pub(crate) fn assert_separate(&self, executables: &[std::path::PathBuf]) {
        use std::os::unix::fs::MetadataExt;
        WRITERS.with_borrow(|writers| {
            let writers = writers.as_ref().unwrap();
            assert!(
                !writers.is_empty(),
                "actual factory writer was not retained"
            );
            for path in executables {
                let object = fs::metadata(path).unwrap();
                for held in writers {
                    assert_ne!(
                        held.object,
                        (object.dev(), object.ino()),
                        "a retained parent writer aliases executable {}",
                        path.display()
                    );
                }
            }
        });
    }
}
#[cfg(target_os = "linux")]
impl Drop for HeldWriters {
    fn drop(&mut self) {
        WRITERS.with_borrow_mut(|writers| drop(writers.take()));
    }
}

#[cfg(target_os = "linux")]
fn capture(file: &File, path: &Path) {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    WRITERS.with_borrow_mut(|writers| {
        if let Some(writers) = writers {
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
            let object = file.metadata().unwrap();
            let held = Retained::new(file.as_raw_fd(), (object.dev(), object.ino()));
            eprintln!(
                "retained CLOEXEC writer: pid={} fd={} object={:?} path={}",
                held.pid,
                file.as_raw_fd(),
                held.object,
                path.display()
            );
            writers.push(held);
        }
    });
}

#[cfg(target_os = "linux")]
struct Retained {
    pid: libc::pid_t,
    gate: File,
    object: (u64, u64),
}
#[cfg(target_os = "linux")]
impl Retained {
    fn new(writer: libc::c_int, object: (u64, u64)) -> Self {
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        let pipe = || {
            let mut fds = [-1; 2];
            assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
            unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
        };
        let (gate_read, gate) = pipe();
        let (mut ready, ready_write) = pipe();
        let mut keep = [
            writer as u32,
            gate_read.as_raw_fd() as u32,
            ready_write.as_raw_fd() as u32,
        ];
        keep.sort_unstable();
        let pid = unsafe { libc::fork() };
        assert!(
            pid >= 0,
            "fixture fork: {}",
            std::io::Error::last_os_error()
        );
        if pid == 0 {
            // Keep only the chosen writer and handshake. In particular, do
            // not retain another test's stdin, stdout or exec-error pipe.
            // Only allocation-free async-signal-safe operations run after fork.
            unsafe {
                let mut start = 0_u32;
                for fd in keep {
                    if start < fd && libc::syscall(libc::SYS_close_range, start, fd - 1, 0_u32) != 0
                    {
                        libc::_exit(120);
                    }
                    start = fd + 1;
                }
                if libc::syscall(libc::SYS_close_range, start, u32::MAX, 0_u32) != 0 {
                    libc::_exit(121);
                }
                let mut byte = 1_u8;
                if libc::write(ready_write.as_raw_fd(), (&byte as *const u8).cast(), 1) != 1 {
                    libc::_exit(122);
                }
                loop {
                    let read = libc::read(gate_read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1);
                    if read == 1 {
                        break;
                    }
                    if read == 0 {
                        libc::_exit(124);
                    }
                    if *libc::__errno_location() != libc::EINTR {
                        libc::_exit(123);
                    }
                }
                libc::_exit(0);
            }
        }
        let held = Self { pid, gate, object };
        drop(gate_read);
        drop(ready_write);
        ready.read_exact(&mut [0]).unwrap();
        held
    }
}
#[cfg(target_os = "linux")]
impl Drop for Retained {
    fn drop(&mut self) {
        let _ = self.gate.write_all(&[1]);
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            if !std::thread::panicking() {
                assert_eq!(result, self.pid);
                assert_eq!(status, 0, "retained fixture writer failed");
            }
            break;
        }
    }
}
