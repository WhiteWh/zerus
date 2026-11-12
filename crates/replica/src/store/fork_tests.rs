use super::ProjectStore;
use fs2::FileExt;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};

struct Forked(libc::pid_t);

impl Forked {
    fn new(pid: libc::pid_t) -> Self {
        assert!(pid > 0, "fork failed: {}", std::io::Error::last_os_error());
        Self(pid)
    }

    fn wait(&mut self) {
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(self.0, &mut status, 0) };
            if result == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            assert_eq!(result, self.0);
            self.0 = 0;
            assert_eq!(status, 0, "the private fork child failed");
            return;
        }
    }
}

impl Drop for Forked {
    fn drop(&mut self) {
        if self.0 > 0 {
            // Only our still-unreaped child can have this PID. A failed test
            // must release a child waiting at the pipe gate before returning.
            unsafe {
                if libc::waitpid(self.0, std::ptr::null_mut(), libc::WNOHANG) == 0 {
                    libc::kill(self.0, libc::SIGKILL);
                    while libc::waitpid(self.0, std::ptr::null_mut(), 0) == -1
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                    }
                }
            }
        }
    }
}

fn pipe() -> (File, File) {
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    }
}

#[test]
fn dropping_the_owner_unlocks_while_a_sibling_fork_retains_the_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let store = ProjectStore::open(root.path(), "fork-held").unwrap();
    let other = File::open(store.dir.join(".lock")).unwrap();
    assert!(other.try_lock_exclusive().is_err());
    let (mut ready_read, ready_write) = pipe();
    let (gate_read, mut gate_write) = pipe();
    let descriptors = [
        ready_read.as_raw_fd(),
        ready_write.as_raw_fd(),
        gate_read.as_raw_fd(),
        gate_write.as_raw_fd(),
    ];
    let pid = std::thread::spawn(move || unsafe {
        let pid = libc::fork();
        if pid == 0 {
            // Nothing except async-signal-safe calls until _exit. The child
            // keeps its inherited store fd open throughout this handshake.
            libc::close(descriptors[0]);
            libc::close(descriptors[3]);
            let mut byte = 1_u8;
            if libc::write(descriptors[1], (&byte as *const u8).cast(), 1) != 1 {
                libc::_exit(120);
            }
            if libc::read(descriptors[2], (&mut byte as *mut u8).cast(), 1) != 1 {
                libc::_exit(121);
            }
            libc::_exit(0);
        }
        pid
    })
    .join()
    .unwrap();
    let mut child = Forked::new(pid);
    drop(ready_write);
    drop(gate_read);
    ready_read.read_exact(&mut [0]).unwrap();
    drop(store);
    assert!(
        other.try_lock_exclusive().is_ok(),
        "the owner's drop must release its lock before the child execs or exits"
    );
    gate_write.write_all(&[1]).unwrap();
    child.wait();
}

#[test]
fn dropping_a_fork_child_copy_keeps_the_parent_store_locked() {
    let root = tempfile::tempdir().unwrap();
    let store = ProjectStore::open(root.path(), "fork-copy").unwrap();
    let other = File::open(store.dir.join(".lock")).unwrap();
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        drop(store);
        unsafe { libc::_exit(0) };
    }
    let mut child = Forked::new(pid);
    child.wait();
    assert!(
        other.try_lock_exclusive().is_err(),
        "a child's drop must not unlock its parent's live store"
    );
    drop(store);
    assert!(other.try_lock_exclusive().is_ok());
}
