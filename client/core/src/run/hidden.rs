//! Retain an original helper when Windows has requested, but not finished, exit.
use super::{Child, Exit, TooLarge};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const STOP_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// GetProcessTimes on the original Child handle; unavailable is never zero.
    pub creation_filetime: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HiddenError {
    OutputTooLarge(TooLarge),
    TerminationPending {
        child: ProcessIdentity,
    },
    /// No second helper is admitted while the first exchange or its exit is open.
    PreviousExchangePending {
        child: Option<ProcessIdentity>,
    },
}

#[derive(Default)]
struct Gate(Mutex<Option<Arc<State>>>);
impl Gate {
    fn admit(&self) -> Result<Arc<State>, HiddenError> {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = &*held {
            if !previous.returned.load(Ordering::Acquire)
                || (previous.stopping.load(Ordering::Acquire)
                    && !previous.exited.load(Ordering::Acquire))
            {
                return Err(HiddenError::PreviousExchangePending {
                    child: *previous.identity.lock().unwrap_or_else(|e| e.into_inner()),
                });
            }
        }
        let state = Arc::new(State::default());
        *held = Some(state.clone());
        Ok(state)
    }
}

#[derive(Default)]
pub(super) struct State {
    returned: AtomicBool,
    stopping: AtomicBool,
    exited: AtomicBool,
    identity: Mutex<Option<ProcessIdentity>>,
    // Also retained after a wait error or failed reaper-thread creation. No PID
    // reopening, job count, exit-code query or closed handle proves completion.
    child: Mutex<Option<Child>>,
}

impl State {
    #[cfg(windows)]
    pub(super) fn admit() -> Result<Arc<Self>, HiddenError> {
        static GATE: std::sync::OnceLock<Gate> = std::sync::OnceLock::new();
        GATE.get_or_init(Gate::default).admit()
    }

    pub(super) fn finish(&self, result: Result<Exit, TooLarge>) -> Result<Exit, HiddenError> {
        self.returned.store(true, Ordering::Release);
        if self.stopping.load(Ordering::Acquire) && !self.exited.load(Ordering::Acquire) {
            return Err(HiddenError::TerminationPending {
                child: self
                    .identity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .expect("stopping retains the original child identity"),
            });
        }
        result.map_err(HiddenError::OutputTooLarge)
    }

    pub(super) fn stop(
        self: &Arc<Self>,
        child: Child,
        exit: Exit,
        exited: Option<Arc<AtomicBool>>,
    ) -> Exit {
        self.stop_with(child, exit, exited, STOP_WAIT, Child::kill)
    }

    fn stop_with(
        self: &Arc<Self>,
        child: Child,
        exit: Exit,
        exited: Option<Arc<AtomicBool>>,
        wait: Duration,
        terminate: impl FnOnce(&mut Child) -> std::io::Result<()> + Send + 'static,
    ) -> Exit {
        *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity(&child));
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        self.stopping.store(true, Ordering::Release);
        let retained = self.clone();
        let (done, observed) = mpsc::sync_channel(1);
        // Both termination and physical wait are off the caller's deadline.
        // This one reaper retains the original Child; it never starts or retries
        // an operation. Its gate remains closed until an actual successful wait.
        let _ = std::thread::Builder::new()
            .name("hgs-helper-exit".into())
            .spawn(move || {
                let mut held = retained.child.lock().unwrap_or_else(|e| e.into_inner());
                let child = held.as_mut().expect("original helper is retained");
                let _ = terminate(child);
                if child.wait().is_ok() {
                    retained.exited.store(true, Ordering::Release);
                    if let Some(exited) = exited {
                        exited.store(true, Ordering::Release);
                    }
                    *held = None;
                }
                let _ = done.send(());
            });
        let _ = observed.recv_timeout(wait);
        exit
    }
}

fn identity(child: &Child) -> ProcessIdentity {
    #[cfg(windows)]
    let creation_filetime = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetProcessTimes;
        let mut created = FILETIME::default();
        let mut ended = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        (unsafe {
            GetProcessTimes(
                child.as_raw_handle(),
                &mut created,
                &mut ended,
                &mut kernel,
                &mut user,
            )
        } != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
            .filter(|birth| *birth != 0)
    };
    #[cfg(not(windows))]
    let creation_filetime = None;
    ProcessIdentity {
        pid: child.id(),
        creation_filetime,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    #[test]
    fn an_ignored_termination_returns_pending_and_retains_the_original_child() {
        // TerminateProcess cannot be ignored in user mode. An actual Unix child
        // ignoring TERM supplies the controlled pending-exit cut; production
        // still uses Child::kill on Windows, with its asynchronous OS semantics.
        let mut child = Command::new("python3")
            .args(["-u", "-c", "import signal,sys; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready'); sys.stdin.readline()"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        let mut release = child.stdin.take().unwrap();
        let pid = child.id();
        let gate = Gate::default();
        let state = gate.admit().unwrap();
        let weak = Arc::downgrade(&state);
        let (requested, request_seen) = mpsc::channel();
        let began = Instant::now();
        let exit = state.stop_with(
            child,
            Exit::TimedOut,
            None,
            Duration::from_millis(40),
            move |child| {
                // The owned original child is still live and ignores this
                // request. No process is adopted and no process group is used.
                assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
                requested.send(()).unwrap();
                Ok(())
            },
        );
        assert!(began.elapsed() < Duration::from_secs(2));
        request_seen.recv_timeout(Duration::from_secs(5)).unwrap();
        let identity = ProcessIdentity {
            pid,
            creation_filetime: None,
        };
        assert_eq!(
            state.finish(Ok(exit)),
            Err(HiddenError::TerminationPending { child: identity })
        );
        assert!(
            matches!(gate.admit(), Err(HiddenError::PreviousExchangePending { child: Some(value) }) if value == identity)
        );
        drop(state);
        assert!(weak.upgrade().is_some());
        // Release through its original input, rather than killing an adopted
        // PID. Only its actual exit permits a subsequent exchange.
        release.write_all(b"exit\n").unwrap();
        drop(release);
        let until = Instant::now() + Duration::from_secs(5);
        while !weak.upgrade().unwrap().exited.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "controlled child failed to exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        let next = gate.admit().unwrap();
        while weak.upgrade().is_some() {
            assert!(
                Instant::now() < until,
                "original reaper still retains the child"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(next.finish(Ok(Exit::TimedOut)), Ok(Exit::TimedOut));
    }
}
