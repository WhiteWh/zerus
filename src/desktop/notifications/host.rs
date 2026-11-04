//! Original native host ownership, off the actor. No generic IPC constructor.
//! The worker retains its child, socket, log readers and owner lock until real
//! exit and EOF. Dropping a handle requests stop; it never asserts drain.
//! The entry owns FD0 as a connected socket and FD3 as a plain File (never an
//! unlock-on-drop wrapper). It marks both close-on-exec after entry, retains FD3
//! through its last callback/exit, and uses FD1/2 only for bounded logs.
#![allow(dead_code)] // Activation follows the platform entry and explicit handoff.
use super::{wire, Dispatch, Endpoint, Event};
use crate::{desktop::store::Location, platform};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::Shutdown,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const LOG_BYTES: usize = 65536;
const QUANTUM: usize = 16384;
const START_WAIT: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(10);
const MAX_FDS: libc::rlim_t = 1 << 20;
static HOST_ACTIVE: AtomicBool = AtomicBool::new(false);
struct Admission;
impl Admission {
    fn acquire() -> Result<Self> {
        ensure!(
            HOST_ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "notification host worker still owned"
        );
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        HOST_ACTIVE.store(false, Ordering::Release);
    }
}

trait Native: Send + 'static {
    fn executable(&self) -> &Path;
    fn revalidate(&self, child: Option<&mut Child>) -> Result<()>;
}

#[cfg(target_os = "macos")]
pub(in crate::desktop) struct SelectedHost {
    native: zerus_client_core::macos_notify::peer::installation::ServiceSelection,
}
#[cfg(target_os = "macos")]
impl SelectedHost {
    /// The maintained installer supplies the exact paired bundle. Never scan.
    /// Security calls are synchronous: call selection off the service actor.
    pub fn select(bundle: &Path) -> Result<Self> {
        use zerus_client_core::macos_notify::peer::installation;
        let native = installation::select_for_service(bundle)
            .map_err(|_| anyhow::anyhow!("notification pair unavailable"))?;
        ensure!(
            native.supports_notification_host().unwrap_or(false),
            "notification host entry unavailable"
        );
        Ok(Self { native })
    }
    pub fn launch(
        self,
        location: &Location,
        session: wire::Session,
        endpoint: Endpoint,
    ) -> Result<OwnedHost> {
        OwnedHost::start(self, location, session, endpoint, vec![wire::ENTRY.into()])
    }
}
#[cfg(target_os = "macos")]
impl Native for SelectedHost {
    fn executable(&self) -> &Path {
        self.native.host_executable()
    }
    fn revalidate(&self, child: Option<&mut Child>) -> Result<()> {
        self.native
            .revalidate()
            .map_err(|_| anyhow::anyhow!("notification pair changed"))?;
        if let Some(child) = child {
            self.native
                .observe_original_child(child)
                .map_err(|_| anyhow::anyhow!("notification child changed"))?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Status {
    ready: bool,
    greeted: bool,
    restored: bool,
    turns: u64,
    drained: bool,
    fault: Option<&'static str>,
    exit: Option<Option<i32>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    log_overflow: bool,
    log_dropped: u64,
    unresponsive: bool,
}

pub(in crate::desktop) struct OwnedHost {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    worker: Option<JoinHandle<()>>,
    restores: mpsc::SyncSender<wire::Activation>,
    restore_done: Arc<AtomicBool>,
    failed_join: bool,
    until: Instant,
}
impl OwnedHost {
    fn start(
        native: impl Native,
        location: &Location,
        session: wire::Session,
        endpoint: Endpoint,
        args: Vec<String>,
    ) -> Result<Self> {
        ensure!(
            session.valid()
                && session.platform == wire::Platform::Macos
                && session.configuration == location.configuration,
            "notification host scope changed"
        );
        let admission = Admission::acquire()?;
        let until = Instant::now() + START_WAIT;
        let location = Location {
            root: location.root.clone(),
            configuration: location.configuration.clone(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let (restores, restored) = mpsc::sync_channel(wire::IN_FLIGHT);
        let restore_done = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            let status = status.clone();
            let restore_done = restore_done.clone();
            thread::Builder::new()
                .name("desktop-notification-host".into())
                .spawn(move || {
                    let _admission = admission;
                    match Original::spawn(&native, &location, &args, until) {
                        Ok(original) => drive(
                            Worker {
                                native,
                                session,
                                endpoint,
                                restores: restored,
                                restore_done,
                                stop,
                                status,
                                until,
                            },
                            original,
                        ),
                        Err(_) => {
                            let mut value = status.lock().unwrap_or_else(|e| e.into_inner());
                            value.fault = Some("notification_host_start_refused");
                            value.drained = true; // No returned child or native command.
                        }
                    }
                })?
        };
        Ok(Self {
            stop,
            status,
            worker: Some(worker),
            restores,
            restore_done,
            failed_join: false,
            until,
        })
    }
    /// Read-only mapping restoration from the actor's original indexed ledger.
    /// Capacity refusal never permits a resend or cache eviction.
    pub fn restore(&mut self, mapping: wire::Activation) -> bool {
        !self.stop.load(Ordering::Acquire)
            && !self.restore_done.load(Ordering::Acquire)
            && mapping.platform_context.is_none()
            && mapping.valid_for(wire::Platform::Macos)
            && self.restores.try_send(mapping).is_ok()
    }
    /// Seal the actor's fixed-highwater walk after every mapping was admitted.
    /// The worker writes the final queued restore before it pulls native work;
    /// the same FIFO channel preserves that order at the host.
    pub fn finish_restoration(&mut self) {
        self.restore_done.store(true, Ordering::Release);
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn poll_drained(&mut self) -> bool {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.failed_join = self.worker.take().unwrap().join().is_err();
        }
        !self.failed_join && self.worker.is_none() && self.status.lock().is_ok_and(|s| s.drained)
    }
    pub fn status(&self) -> Value {
        let Ok(s) = self.status.try_lock() else {
            return json!({"ready":false,"drained":false,"fault":"notification_status_busy"});
        };
        json!({"transport_ready":s.ready,"restoring":!s.restored,"ready":s.ready && s.restored && !self.stop.load(Ordering::Acquire),"drained":self.worker.is_none() && !self.failed_join && s.drained,
            "fault":if self.failed_join {Some("notification_host_worker_panicked")}
                else if !s.greeted && !s.drained && Instant::now() >= self.until {Some("notification_host_start_deadline")} else {s.fault},
            "code":if s.unresponsive {"notification_host_unresponsive"} else if s.drained {"drained"} else {"running_or_starting"},
            "turns":s.turns,"exit_observed":s.exit.is_some(),"exit":s.exit,"stdout_bytes":s.stdout.len(),"stderr_bytes":s.stderr.len(),"log_overflow":s.log_overflow,"log_dropped_bytes":s.log_dropped})
    }
}
impl Drop for OwnedHost {
    fn drop(&mut self) {
        // The original worker keeps its actual child/lock/resources if detach
        // is unavoidable. No replacement can acquire the still-held lock.
        self.request_stop();
    }
}

struct Original {
    child: Child,
    control: UnixStream,
    stdout: UnixStream,
    stderr: UnixStream,
    _lock: File,
}
impl Original {
    fn spawn(
        native: &impl Native,
        location: &Location,
        args: &[String],
        until: Instant,
    ) -> Result<Self> {
        location.check_existing()?;
        native.revalidate(None)?;
        let mut options = OpenOptions::new();
        platform::private_open(options.read(true).write(true).create(true).truncate(false));
        platform::no_follow(&mut options);
        let file = options.open(location.root.join("notification-host.lock"))?;
        ensure!(
            platform::verify_private_file(&file)?,
            "notification host lock is not private"
        );
        fs2::FileExt::try_lock_exclusive(&file).context("notification host still owned")?;
        // Reserve >=4 before stdio setup; closed service stdin must not let
        // Command's fd0 replacement overwrite the inherited owner lock.
        // SAFETY: live owned file; F_DUPFD_CLOEXEC returns a new descriptor.
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 4) };
        ensure!(fd >= 4, "cannot retain notification owner lock");
        // SAFETY: successful duplication above transfers this descriptor once.
        let lock = unsafe { File::from_raw_fd(fd) };
        drop(file); // Same flock/open description stays held by lock and fd3.
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: one live, correctly sized output value in the parent.
        ensure!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0
                && limit.rlim_cur <= MAX_FDS,
            "notification descriptor limit unavailable"
        );
        let close_until = limit.rlim_cur as libc::c_int;
        let (child, (control, stdout, stderr)) =
            platform::spawn_prepared_in_turn_before(until, || {
                let (control, child_control) = UnixStream::pair()?;
                let (stdout, child_stdout) = UnixStream::pair()?;
                let (stderr, child_stderr) = UnixStream::pair()?;
                for stream in [&control, &stdout, &stderr] {
                    stream.set_nonblocking(true)?;
                }
                let mut command = Command::new(native.executable());
                command.args(args).env_clear();
                for key in [
                    "HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG", "LC_ALL",
                ] {
                    if let Some(value) = std::env::var_os(key) {
                        command.env(key, value);
                    }
                }
                command
                    .stdin(Stdio::from(OwnedFd::from(child_control)))
                    .stdout(Stdio::from(OwnedFd::from(child_stdout)))
                    .stderr(Stdio::from(OwnedFd::from(child_stderr)));
                let lock_fd = lock.as_raw_fd();
                // Only async-signal-safe descriptor operations after fork. FD0 is the
                // full-duplex socket; FD1/2 are logs; FD3 is the shared owner lock.
                unsafe {
                    command.pre_exec(move || {
                        if libc::dup2(lock_fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        for fd in 4..close_until {
                            let flags = libc::fcntl(fd, libc::F_GETFD);
                            if flags < 0 {
                                if io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
                                    return Err(io::Error::last_os_error());
                                }
                            } else if flags & libc::FD_CLOEXEC == 0 && libc::close(fd) < 0 {
                                return Err(io::Error::last_os_error());
                            }
                            // Already-CLOEXEC descriptors (including Rust's
                            // exec-error pipe) close at exec, not before it.
                        }
                        Ok(())
                    });
                }
                Ok((command, (control, stdout, stderr)))
            })?;
        // No fallible post-spawn construction can lose this returned Child.
        Ok(Self {
            child,
            control,
            stdout,
            stderr,
            _lock: lock,
        })
    }
}

struct Pending {
    binding: wire::Binding,
    dispatch: Option<Dispatch>,
    _resources: Arc<dyn Send + Sync>,
}
struct Writing {
    bytes: Vec<u8>,
    offset: usize,
}
fn writing(value: &wire::ToHost) -> Result<Writing> {
    Ok(Writing {
        bytes: wire::encode(value).map_err(anyhow::Error::msg)?,
        offset: 0,
    })
}
fn write_turn(stream: &mut UnixStream, value: &mut Option<Writing>) -> io::Result<bool> {
    let Some(frame) = value else {
        return Ok(false);
    };
    let end = frame.bytes.len().min(frame.offset + QUANTUM);
    match stream.write(&frame.bytes[frame.offset..end]) {
        Ok(0) => Err(io::ErrorKind::WriteZero.into()),
        Ok(n) => {
            frame.offset += n;
            if frame.offset == frame.bytes.len() {
                *value = None;
            }
            Ok(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}
fn read_frame(
    stream: &mut UnixStream,
    bytes: &mut Vec<u8>,
    eof: &mut bool,
) -> Result<Option<wire::FromHost>> {
    if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
        let frame = wire::decode(&bytes[..=end]).map_err(anyhow::Error::msg)?;
        bytes.drain(..=end);
        return Ok(Some(frame));
    }
    ensure!(bytes.len() < wire::FRAME_BYTES, "notification frame limit");
    if *eof {
        ensure!(bytes.is_empty(), "partial notification frame");
        return Ok(None);
    }
    let mut chunk = [0; QUANTUM];
    let limit = chunk.len().min(wire::FRAME_BYTES - bytes.len());
    match stream.read(&mut chunk[..limit]) {
        Ok(0) => {
            *eof = true;
            ensure!(bytes.is_empty(), "partial notification frame");
        }
        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(None)
}
fn discard_turn(stream: &mut UnixStream, eof: &mut bool) {
    if *eof {
        return;
    }
    let mut bytes = [0; QUANTUM];
    if matches!(stream.read(&mut bytes), Ok(0)) {
        *eof = true;
    }
}
fn log_turn(stream: &mut UnixStream, prefix: &mut Vec<u8>, eof: &mut bool) -> Result<usize> {
    if *eof {
        return Ok(0);
    }
    let mut bytes = [0; QUANTUM];
    match stream.read(&mut bytes) {
        Ok(0) => {
            *eof = true;
            Ok(0)
        }
        Ok(n) => {
            let kept = n.min(LOG_BYTES - prefix.len());
            prefix.extend_from_slice(&bytes[..kept]);
            Ok(n - kept)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(0)
        }
        Err(e) => Err(e.into()),
    }
}

fn wait_io(
    original: &Original,
    control: bool,
    writing: bool,
    stdout: bool,
    stderr: bool,
) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: if control || writing {
                original.control.as_raw_fd()
            } else {
                -1
            },
            events: (if control { libc::POLLIN } else { 0 })
                | (if writing { libc::POLLOUT } else { 0 }),
            revents: 0,
        },
        libc::pollfd {
            fd: if stdout {
                original.stdout.as_raw_fd()
            } else {
                -1
            },
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: if stderr {
                original.stderr.as_raw_fd()
            } else {
                -1
            },
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // Readable pipes wake immediately. The finite idle timeout also services
    // original mpsc work/stop without a permanent 100 Hz polling loop.
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

struct Worker<N> {
    native: N,
    session: wire::Session,
    endpoint: Endpoint,
    restores: mpsc::Receiver<wire::Activation>,
    restore_done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    until: Instant,
}
fn drive(worker: Worker<impl Native>, mut original: Original) {
    let Worker {
        native,
        session,
        endpoint,
        restores,
        restore_done,
        stop,
        status,
        until,
    } = worker;
    let mut pending: BTreeMap<String, Pending> = BTreeMap::new();
    let mut output = writing(&wire::ToHost::Hello {
        abi: wire::ABI.into(),
        session: session.clone(),
    })
    .ok();
    let mut input = Vec::new();
    let (mut control_eof, mut stdout_eof, mut stderr_eof) = (false, false, false);
    let mut callback = None;
    let mut ready = false;
    let mut restored = false;
    let mut fault = None;
    let mut stopped = false;
    let mut stop_sent = false;
    let mut stop_at = None;
    let mut exit = None;
    if native.revalidate(Some(&mut original.child)).is_err() {
        fault = Some("notification_child_refused");
    }
    loop {
        let mut progressed = false;
        let now = Instant::now();
        if (!ready && now >= until)
            || pending
                .values()
                .any(|p| p.dispatch.as_ref().is_some_and(Dispatch::cancelled))
        {
            fault.get_or_insert("notification_host_deadline");
        }
        if endpoint.stopping() {
            stop.store(true, Ordering::Release);
        }
        if fault.is_some() {
            stop.store(true, Ordering::Release);
            ready = false;
            let _ = original.control.shutdown(Shutdown::Write);
            output = None;
        }
        if stop.load(Ordering::Acquire) && stop_at.is_none() {
            stop_at = Some(now);
        }
        if output.is_none() && fault.is_none() && exit.is_none() {
            if stop.load(Ordering::Acquire) {
                if !stop_sent {
                    output = writing(&wire::ToHost::Stop {
                        session: session.clone(),
                    })
                    .ok();
                    stop_sent = true;
                }
            } else if ready {
                if let Ok(mapping) = restores.try_recv() {
                    progressed = true;
                    output = writing(&wire::ToHost::Restore {
                        session: session.clone(),
                        mapping,
                    })
                    .ok();
                } else if restore_done.load(Ordering::Acquire) && pending.len() < wire::IN_FLIGHT {
                    restored = true;
                    if let Ok(work) = endpoint.try_next() {
                        progressed = true;
                        if let Some(dispatch) = work.begin() {
                            if dispatch.command().binding.session != session
                                || native.revalidate(Some(&mut original.child)).is_err()
                            {
                                dispatch.finish(wire::Outcome::Refused {
                                    code: wire::Refusal::NotReady,
                                });
                                fault = Some("notification_pair_changed");
                            } else if dispatch.cancelled() {
                                dispatch.finish(wire::Outcome::Refused {
                                    code: wire::Refusal::Cancelled,
                                });
                            } else {
                                match writing(&wire::ToHost::Command(dispatch.command().clone())) {
                                    Ok(frame) => {
                                        if dispatch.cancelled() {
                                            dispatch.finish(wire::Outcome::Refused {
                                                code: wire::Refusal::Cancelled,
                                            });
                                            continue;
                                        }
                                        let binding = dispatch.command().binding.clone();
                                        let resources = dispatch.resources();
                                        pending.insert(
                                            binding.request_id.clone(),
                                            Pending {
                                                binding,
                                                dispatch: Some(dispatch),
                                                _resources: resources,
                                            },
                                        );
                                        output = Some(frame);
                                    }
                                    Err(_) => dispatch.finish(wire::Outcome::Refused {
                                        code: wire::Refusal::InvalidCommand,
                                    }),
                                }
                            }
                        }
                    }
                }
            }
        }
        match write_turn(&mut original.control, &mut output) {
            Ok(written) => progressed |= written,
            Err(_) => {
                fault.get_or_insert("notification_transport_failed");
            }
        }
        if fault.is_some() {
            callback = None;
            input.clear();
            discard_turn(&mut original.control, &mut control_eof);
        } else if callback.is_none() {
            let before = (input.len(), control_eof);
            match read_frame(&mut original.control, &mut input, &mut control_eof) {
                Ok(frame) => callback = frame,
                Err(_) => {
                    fault.get_or_insert("notification_protocol_failed");
                    input.clear();
                }
            }
            progressed |= before != (input.len(), control_eof);
        }
        if let Some(frame) = callback.take() {
            match frame {
                wire::FromHost::Ready {
                    abi,
                    application,
                    session: observed,
                } if abi == wire::ABI
                    && application == wire::APPLICATION
                    && observed == session =>
                {
                    if stop_sent || stop.load(Ordering::Acquire) {
                        // A genuine greeting may already be in the socket when
                        // the parent requests Stop. It cannot reopen admission.
                    } else if !ready {
                        ready = true;
                    } else {
                        fault.get_or_insert("notification_protocol_failed");
                    }
                }
                wire::FromHost::Receipt(receipt) => {
                    match pending.get_mut(&receipt.binding.request_id) {
                        Some(p)
                            if p.binding == receipt.binding
                                && p.dispatch
                                    .as_ref()
                                    .is_some_and(|d| receipt.matches(d.command())) =>
                        {
                            p.dispatch.take().unwrap().finish_receipt(receipt);
                        }
                        _ => {
                            fault.get_or_insert("notification_receipt_mismatch");
                        }
                    }
                }
                wire::FromHost::Released { binding } => {
                    if pending
                        .get(&binding.request_id)
                        .is_some_and(|p| p.binding == binding && p.dispatch.is_none())
                    {
                        pending.remove(&binding.request_id);
                    } else {
                        fault.get_or_insert("notification_release_mismatch");
                    }
                }
                wire::FromHost::Clicked(value) if value.valid_for(session.platform) => {
                    if !endpoint.stopping() && !endpoint.emit(Event::Clicked(value.clone())) {
                        callback = Some(wire::FromHost::Clicked(value));
                    }
                }
                wire::FromHost::Closed(value)
                    if value.valid_for(session.platform) && value.platform_context.is_none() =>
                {
                    if !endpoint.emit(Event::Closed(value.clone())) {
                        callback = Some(wire::FromHost::Closed(value));
                    }
                }
                wire::FromHost::Stopped { session: observed }
                    if observed == session && stop_sent && pending.is_empty() =>
                {
                    stopped = true;
                }
                _ => {
                    fault.get_or_insert("notification_protocol_failed");
                }
            }
            // A callback retained against a full actor queue is not progress:
            // keep the idle wait until that original queue can accept it.
            progressed |= callback.is_none();
        }
        if control_eof && !stopped {
            fault.get_or_insert("notification_channel_closed");
        }
        if stopped {
            let _ = original.control.shutdown(Shutdown::Write);
        }
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.turns = s.turns.saturating_add(1);
        let out = log_turn(&mut original.stdout, &mut s.stdout, &mut stdout_eof);
        let err = log_turn(&mut original.stderr, &mut s.stderr, &mut stderr_eof);
        let dropped = out
            .as_ref()
            .copied()
            .unwrap_or(0)
            .saturating_add(err.as_ref().copied().unwrap_or(0));
        if dropped != 0 {
            s.log_overflow = true;
            s.log_dropped = s.log_dropped.saturating_add(dropped as u64);
        }
        if out.is_err() || err.is_err() {
            fault.get_or_insert("notification_log_unavailable");
        }
        match original.child.try_wait() {
            Ok(Some(result)) => {
                exit = Some(result.code());
            }
            Ok(None) => (),
            Err(_) => {
                fault.get_or_insert("notification_wait_unavailable");
            }
        }
        s.ready = ready && fault.is_none() && exit.is_none() && !stop.load(Ordering::Acquire);
        s.greeted |= ready;
        s.restored = restored;
        s.fault = fault;
        s.exit = exit;
        s.unresponsive = stop_at.is_some_and(|at: Instant| at.elapsed() >= STOP_GRACE);
        if exit.is_some() && control_eof && stdout_eof && stderr_eof {
            // Actual process exit and all original transport EOF release any
            // unknown command holds. Receipt outcome is never upgraded here.
            for p in pending.values_mut() {
                if let Some(dispatch) = p.dispatch.take() {
                    dispatch.finish(wire::Outcome::Unknown {
                        code: wire::Uncertainty::HostLost,
                    });
                }
            }
            pending.clear();
            s.ready = false;
            s.drained = true;
            s.unresponsive = false;
            return;
        }
        drop(s);
        // Restore has no reply. Drain ready mpsc/socket work back-to-back,
        // reserving the finite poll wait for a turn that made no progress.
        if !progressed
            && (!input.contains(&b'\n') || callback.is_some())
            && wait_io(
                &original,
                !control_eof && (callback.is_none() || fault.is_some()),
                output.is_some(),
                !stdout_eof,
                !stderr_eof,
            )
            .is_err()
        {
            fault.get_or_insert("notification_poll_unavailable");
        }
    }
}

#[cfg(test)]
mod tests;
