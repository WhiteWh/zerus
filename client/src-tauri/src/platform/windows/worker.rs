//! Qt's Windows-only swarm catalog worker. Own process/job only: never an agent.
use std::os::windows::{
    io::{AsRawHandle, OwnedHandle},
    process::CommandExt,
};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use windows_sys::Win32::{
    Foundation::*,
    System::{JobObjects::*, Threading::*},
};

pub struct Worker {
    stop: Arc<OwnedHandle>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Worker {
    pub fn start(hgs: PathBuf) -> std::io::Result<Self> {
        let stop = Arc::new(super::owned(unsafe {
            CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
        })?);
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut delay = 0;
            while unsafe { WaitForSingleObject(stopped.as_raw_handle(), 0) } == WAIT_TIMEOUT {
                let job = match super::owned(unsafe {
                    CreateJobObjectW(std::ptr::null(), std::ptr::null())
                }) {
                    Ok(job) => job,
                    Err(e) => {
                        eprintln!("zerus-client: swarm worker job: {e}");
                        return;
                    }
                };
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                    unsafe { std::mem::zeroed() };
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if unsafe {
                    SetInformationJobObject(
                        job.as_raw_handle(),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    )
                } == 0
                {
                    eprintln!(
                        "zerus-client: cannot contain the swarm worker: {}",
                        std::io::Error::last_os_error()
                    );
                    return;
                }
                let start = Instant::now();
                let child = Command::new(&hgs)
                    .args(["swarm", "worker"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn();
                if let Ok(mut child) = child {
                    if unsafe {
                        AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle())
                    } == 0
                    {
                        eprintln!(
                            "zerus-client: cannot contain the swarm worker: {}",
                            std::io::Error::last_os_error()
                        );
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    let errors = child.stderr.take().map(|mut pipe| {
                        std::thread::spawn(move || {
                            use std::io::Read;
                            let mut tail = Vec::new();
                            let mut block = [0; 4096];
                            while let Ok(n) = pipe.read(&mut block) {
                                if n == 0 {
                                    break;
                                }
                                tail.extend_from_slice(&block[..n]);
                                if tail.len() > 4096 {
                                    tail.drain(..tail.len() - 4096);
                                }
                            }
                            tail
                        })
                    });
                    // Both are original owned handles: no periodic process polling.
                    let handles = [stopped.as_raw_handle(), child.as_raw_handle()];
                    let outcome =
                        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                    if outcome == WAIT_OBJECT_0 {
                        let _ = child.kill();
                    } else if outcome != WAIT_OBJECT_0 + 1 {
                        eprintln!(
                            "zerus-client: swarm worker handle wait: {}",
                            std::io::Error::last_os_error()
                        );
                        let _ = child.kill();
                    }
                    if unsafe { WaitForSingleObject(child.as_raw_handle(), 5000) } == WAIT_OBJECT_0
                    {
                        match child.wait() {
                            Ok(status) if !status.success() => {
                                eprintln!("zerus-client: swarm worker exited: {status}")
                            }
                            Err(error) => eprintln!("zerus-client: swarm worker exit: {error}"),
                            _ => {}
                        }
                    } else {
                        eprintln!("zerus-client: swarm worker exit remains unconfirmed");
                    }
                    // Closing THIS job releases only its worker/ssh descendants.
                    drop(job);
                    if let Some(errors) = errors {
                        if let Ok(tail) = errors.join() {
                            if !tail.is_empty() {
                                eprintln!(
                                    "zerus-client: swarm worker: {}",
                                    String::from_utf8_lossy(&tail)
                                );
                            }
                        }
                    }
                } else if let Err(error) = child {
                    eprintln!("zerus-client: cannot start swarm worker: {error}");
                }
                if unsafe { WaitForSingleObject(stopped.as_raw_handle(), 0) } != WAIT_TIMEOUT {
                    return;
                }
                delay = crate::windows_shell::restart_delay(delay, start.elapsed());
                if unsafe { WaitForSingleObject(stopped.as_raw_handle(), delay as u32) }
                    != WAIT_TIMEOUT
                {
                    return;
                }
            }
        });
        Ok(Self {
            stop,
            thread: Mutex::new(Some(thread)),
        })
    }
    pub fn stop(&self) {
        unsafe {
            SetEvent(self.stop.as_raw_handle());
        }
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}
