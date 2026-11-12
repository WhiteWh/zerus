//! Actual staging control: an unrelated fork retains the exact parent writer.
use super::*;
use std::{cell::RefCell, fs::File, io::Write, os::fd::AsRawFd};

type Hook = Box<dyn FnOnce(libc::c_int)>;
thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}
pub(super) fn capture(writer: &impl AsRawFd) {
    let hook = HOOK.with_borrow_mut(Option::take);
    if let Some(hook) = hook {
        hook(writer.as_raw_fd());
    }
}

#[test]
fn staging_and_capability_probe_finish_while_a_sibling_retains_the_parent_writer() {
    use std::{os::unix::fs::PermissionsExt, sync::mpsc};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("helper");
    let text = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", MARKER);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::channel();
    let target = path.clone();
    let contents = text.clone();
    let worker = std::thread::spawn(move || {
        HOOK.with_borrow_mut(|hook| {
            *hook = Some(Box::new(move |fd| {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
                eprintln!(
                    "retaining actual staging writer fd={fd} target={:?}",
                    std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap()
                );
                ready_tx.send(Retained::new(fd)).unwrap();
            }))
        });
        super::tests::stage_script(&target, &contents);
        done_tx.send(()).unwrap();
    });
    // Fixture bounds only; no production probe deadline is changed. The gate
    // is released and the actual staging worker joined before a RED assertion.
    let held = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("actual writer hook");
    let completed = done_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    if !completed {
        drop(held);
        worker.join().unwrap();
        panic!("staging waited for a sibling's retained writer EOF");
    }
    worker.join().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The real capability path must work while the parent writer is STILL
    // retained: returning to direct executable writes must not fix the EOF bug.
    assert!(Observed::probe(path.to_str().unwrap()).required);
    drop(held);
}

#[cfg(target_os = "linux")]
struct Retained {
    pid: libc::pid_t,
    gate: File,
}
#[cfg(target_os = "linux")]
impl Retained {
    fn new(writer: libc::c_int) -> Self {
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
        let held = Self { pid, gate };
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
                assert_eq!(status, 0, "retained staging writer failed");
            }
            break;
        }
    }
}
