//! Private, pipe-gated controls for ordinary acquisition-owner locks.
use super::*;
use fs2::FileExt;
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

type Hook = (&'static str, Box<dyn FnOnce()>);
thread_local! {
    static HOOKS: RefCell<Vec<Hook>> = const { RefCell::new(Vec::new()) };
}
pub(crate) fn arm(label: &'static str, hook: impl FnOnce() + 'static) {
    HOOKS.with(|hooks| hooks.borrow_mut().push((label, Box::new(hook))));
}
pub(crate) fn reached(label: &str) {
    let hook = HOOKS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        hooks
            .iter()
            .position(|(name, _)| *name == label)
            .map(|i| hooks.remove(i).1)
    });
    if let Some(hook) = hook {
        hook();
    }
}

fn pipe() -> (File, File) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

/// An exact owned child, reaped even if a regression assertion fails.
pub(crate) struct Retainer {
    pid: libc::pid_t,
    gate: File,
}
impl Retainer {
    pub(crate) fn start() -> Self {
        let (mut ready, signal) = pipe();
        let (wait, gate) = pipe();
        let fds = [
            ready.as_raw_fd(),
            signal.as_raw_fd(),
            wait.as_raw_fd(),
            gate.as_raw_fd(),
        ];
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", io::Error::last_os_error());
        if pid == 0 {
            // No allocation, Rust destructors or non-async-signal-safe work
            // in this unrelated pre-exec sibling. Retain all other descriptors.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[3]);
                let mut byte = 1_u8;
                if libc::write(fds[1], (&byte as *const u8).cast(), 1) != 1 {
                    libc::_exit(120);
                }
                if libc::read(fds[2], (&mut byte as *mut u8).cast(), 1) != 1 {
                    libc::_exit(121);
                }
                libc::_exit(0);
            }
        }
        let held = Self { pid, gate };
        drop(signal);
        drop(wait);
        ready.read_exact(&mut [0]).unwrap();
        held
    }
    pub(crate) fn finish(mut self) {
        self.gate.write_all(&[1]).unwrap();
        reap(&mut self.pid);
    }
}
impl Drop for Retainer {
    fn drop(&mut self) {
        if self.pid > 0 {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            let mut status = 0;
            while unsafe { libc::waitpid(self.pid, &mut status, 0) } < 0
                && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
            {}
        }
    }
}
fn reap(pid: &mut libc::pid_t) {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(*pid, &mut status, 0) };
        if result == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        assert_eq!(result, *pid);
        *pid = 0;
        assert_eq!(status, 0);
        break;
    }
}

pub(crate) fn busy(path: &Path) {
    let file = File::open(path).unwrap();
    let error = file
        .try_lock_exclusive()
        .expect_err("live owner lost its exclusion");
    assert!(lock_contended(&error), "{error}");
}
pub(crate) type Captured = Rc<RefCell<Option<Retainer>>>;
pub(crate) fn capture(
    label: &'static str,
    path: &Path,
    after: impl FnOnce() + 'static,
) -> Captured {
    let slot = Rc::new(RefCell::new(None));
    let output = slot.clone();
    let path = path.to_owned();
    arm(label, move || {
        busy(&path);
        *output.borrow_mut() = Some(Retainer::start());
        after();
    });
    slot
}
pub(crate) fn released(slot: Captured, path: &Path) {
    let held = slot
        .borrow_mut()
        .take()
        .expect("actual product acquisition hook was not reached");
    let file = File::open(path).unwrap();
    let result = file.try_lock_exclusive();
    assert!(
        result.is_ok(),
        "owner scope closed without releasing {}: {result:?}",
        path.display()
    );
    // Prove release while the sibling is still held, before opening its gate.
    held.finish();
}

pub(crate) fn control(fixture: &str, case: &str) {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let tmux = scratch.path().join("tmux");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&tmux).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", fixture, "--ignored", "--test-threads=1"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("HGS_STATE_DIR", scratch.path().join("state"))
        .env("TMPDIR", std::env::temp_dir())
        .env("TMUX_TMPDIR", tmux)
        .env("HGS_CONFIG_DIR", scratch.path().join("config"))
        .env("HGS_SELF", "fixture")
        .env("HGS_PEERS", "")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZERUS_WORKTREE_LOCK_CONTROL", case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{case}: private control did not settle");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{case}: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn owner_unlocks_before_a_sibling_fork_exits() {
    control("platform::lock_tests::fixture", "owner");
}
#[test]
fn child_copy_drop_cannot_unlock_the_live_parent() {
    control("platform::lock_tests::fixture", "child-copy");
}
#[test]
#[ignore = "isolated pipe-gated fork control"]
fn fixture() {
    let Ok(case) = std::env::var("ZERUS_WORKTREE_LOCK_CONTROL") else {
        return;
    };
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let path = temporary.path();
    let guard = FileLock::exclusive(temporary.reopen().unwrap()).unwrap();
    busy(path);
    if case == "owner" {
        let child = Retainer::start();
        drop(guard);
        assert!(File::open(path).unwrap().try_lock_exclusive().is_ok());
        child.finish();
    } else {
        let mut pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            drop(guard);
            unsafe {
                libc::_exit(0);
            }
        }
        reap(&mut pid);
        busy(path);
        drop(guard);
        assert!(File::open(path).unwrap().try_lock_exclusive().is_ok());
    }
}
