//! Fixed headless entry for the original service-owned Mac notification host.
//! Native ownership precedes Hello and all notification APIs. No Tauri context,
//! instance socket, GUI writer, path discovery or durable intent store exists.
use objc2::{rc::autoreleasepool, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSDate, NSRunLoop};
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixStream,
    },
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use zerus_client_core::{
    desktop::notifications::{
        self as wire,
        macos::{
            channel::{Channel, Input},
            Emission, Host,
        },
    },
    macos_notify::peer::installation,
};

mod center;
mod delegate;

const WAIT: Duration = Duration::from_secs(10);
static TAKEN: AtomicBool = AtomicBool::new(false);

pub fn run() -> i32 {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(wire::ENTRY))
        || arguments.next().is_some()
        || TAKEN.swap(true, Ordering::AcqRel)
    {
        return 2;
    }
    match start() {
        Ok(()) => 0,
        Err(_) => {
            eprintln!("zerus-client: notification host unavailable");
            2
        }
    }
}

fn cloexec(fd: i32) -> Result<(), &'static str> {
    // SAFETY: fcntl operates only on the fixed inherited descriptors.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err("notification_inherited_descriptor");
    }
    Ok(())
}
fn inherited() -> Result<(UnixStream, Arc<File>), &'static str> {
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getuid() != libc::geteuid() } {
        return Err("notification_login_user");
    }
    let mut socket: libc::stat = unsafe { std::mem::zeroed() };
    // Inspect before taking ownership: an invalid FD must not become a File.
    if unsafe { libc::fstat(0, &mut socket) } != 0
        || socket.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || !private_owner(3)
    {
        return Err("notification_inherited_scope");
    }
    cloexec(0)?;
    cloexec(3)?;
    // SAFETY: this fixed entry is the sole owner of its inherited socket0 and
    // plain file3. No stdio reader or unlock-on-drop wrapper is constructed.
    let socket = unsafe { UnixStream::from_raw_fd(0) };
    let owner = Arc::new(unsafe { File::from_raw_fd(3) });
    socket
        .peer_addr()
        .map_err(|_| "notification_socket_not_connected")?;
    Ok((socket, owner))
}

/// Metadata only, never a role or native effect permission. Production passes
/// only the fixed inherited FD3; tests inspect their own explicitly opened files.
fn private_owner(fd: i32) -> bool {
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: descriptor inspection only, without reading or changing contents.
    (unsafe { libc::fstat(fd, &mut info) }) == 0
        && info.st_mode & libc::S_IFMT == libc::S_IFREG
        && info.st_nlink == 1
        && info.st_uid == unsafe { libc::geteuid() }
        && info.st_mode & 0o077 == 0
        && unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE == libc::O_RDWR
}

struct Worker {
    channel: Channel,
    input: mpsc::SyncSender<wire::ToHost>,
    output: mpsc::Receiver<Emission>,
    quit: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    _owner: Arc<File>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Publish closure only AFTER closing the original socket and dropping
        // its queued emissions. Disconnected output then permits local drops.
        self.channel.close();
        self.closed.store(true, Ordering::Release);
    }
}
fn worker(mut work: Worker) -> Result<(), &'static str> {
    let until = Instant::now() + WAIT;
    let selection = installation::select_own().map_err(|_| "notification_own_pair")?;
    selection
        .observe_service(work.channel.socket().ok_or("notification_socket_closed")?)
        .map_err(|_| "notification_original_service")?;
    let mut greeted = false;
    let mut pending = None;
    let mut stopped = false;
    loop {
        let mut progressed = false;
        if work.quit.load(Ordering::Acquire) {
            return Ok(());
        }
        if !greeted && Instant::now() >= until {
            return Err("notification_hello_deadline");
        }
        if pending.is_none() {
            match work.channel.read_turn()? {
                Input::Closed => return Ok(()),
                Input::Pending => {}
                Input::Progress => progressed = true,
                Input::Frame(frame) => {
                    let frame = *frame;
                    progressed = true;
                    if !greeted {
                        match &frame {
                            wire::ToHost::Hello { abi, session }
                                if abi == wire::ABI
                                    && session.valid()
                                    && session.platform == wire::Platform::Macos => {}
                            _ => return Err("notification_first_hello"),
                        }
                        greeted = true;
                    }
                    pending = Some(frame);
                }
            }
        }
        if let Some(frame) = pending.take() {
            // Re-observe the ACTUAL socket peer/code/generation immediately
            // before each bounded admission, including after a full queue.
            if matches!(frame, wire::ToHost::Command(_) | wire::ToHost::Hello { .. }) {
                selection
                    .observe_service(work.channel.socket().ok_or("notification_socket_closed")?)
                    .map_err(|_| "notification_original_service_changed")?;
            } else {
                // Restore is comparison data and Stop only reduces authority.
                // Do not hash the entire app for each of up to 8192 mappings.
                // Every effect-bearing Command still gets full native checks.
                selection
                    .service_executable_if_unchanged()
                    .map_err(|_| "notification_generation_changed")?;
            }
            match work.input.try_send(frame) {
                Ok(()) => progressed = true,
                Err(mpsc::TrySendError::Full(frame)) => pending = Some(frame),
                Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
            }
        }
        if work.channel.accepts_output() {
            match work.output.try_recv() {
                Ok(emission) => {
                    progressed = true;
                    stopped |= matches!(emission.frame, wire::FromHost::Stopped { .. });
                    work.channel.queue(emission)?;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        progressed |= work.channel.write_turn()?;
        if stopped && work.channel.output_empty() {
            return Ok(());
        }
        if progressed {
            continue;
        }
        // Real socket readiness, with a bounded wake to consume main-thread
        // output and stop. Never a busy read/JSON loop or periodic rehash.
        let mut fd = libc::pollfd {
            fd: work.channel.socket().unwrap().as_raw_fd(),
            events: if pending.is_none() { libc::POLLIN } else { 0 }
                | if work.channel.output_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 100) } < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return Err("notification_socket_poll");
        }
        if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err("notification_socket_closed");
        }
    }
}

fn turn(progressed: bool) {
    let started = Instant::now();
    let delay = if progressed { 0.0 } else { 0.1 };
    autoreleasepool(|_| {
        NSRunLoop::mainRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(delay))
    });
    // An empty native run loop may return immediately; keep that idle case
    // bounded too. No window/Dock event loop or owner display is opened.
    if progressed {
        thread::yield_now();
    } else if let Some(left) = Duration::from_millis(50).checked_sub(started.elapsed()) {
        thread::sleep(left);
    }
}
fn start() -> Result<(), &'static str> {
    let (socket, owner) = inherited()?;
    // This one descriptor is a process-lifetime host resource. Keep the plain
    // File until OS exit, including the final join/return path after callbacks.
    // Per-command callback holds remain separately accounted by the engine.
    std::mem::forget(owner.clone());
    let (input, incoming) = mpsc::sync_channel(wire::IN_FLIGHT);
    let (outgoing, output) = mpsc::sync_channel(32);
    let quit = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicBool::new(false));
    let work = Worker {
        channel: Channel::new(socket).map_err(|_| "notification_socket")?,
        input,
        output,
        quit: quit.clone(),
        closed: closed.clone(),
        _owner: owner.clone(),
    };
    let worker = thread::Builder::new()
        .name("notification-native-peer".into())
        .spawn(move || worker(work))
        .map_err(|_| "notification_peer_worker")?;
    // Keep the actual worker through every error/unknown path. Its native
    // calls may be unresponsive; no timeout, detach, kill or replacement here.
    let result = drive(&incoming, &outgoing, &quit, &closed, owner);
    if result.is_err() {
        quit.store(true, Ordering::Release);
    }
    while !worker.is_finished() {
        turn(false);
    }
    worker
        .join()
        .map_err(|_| "notification_peer_worker_failed")??;
    result
}

fn drive(
    incoming: &mpsc::Receiver<wire::ToHost>,
    outgoing: &mpsc::SyncSender<Emission>,
    quit: &AtomicBool,
    closed: &AtomicBool,
    owner: Arc<File>,
) -> Result<(), &'static str> {
    let session = match incoming.recv() {
        Ok(wire::ToHost::Hello { abi, session }) if abi == wire::ABI => session,
        _ => return Err("notification_hello_unavailable"),
    };
    let mut host = Host::new(session.clone()).ok_or("notification_session")?;
    let mtm = MainThreadMarker::new().ok_or("notification_main_thread")?;
    let app = NSApplication::sharedApplication(mtm);
    if !app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited) {
        return Err("notification_headless_policy");
    }
    let center = center::HostCenter::new(owner.clone());
    let mut delegate = Some(delegate::Installed::new(host.native_resources(), owner));
    app.finishLaunching();
    let mut stopping = false;
    let mut pending = None;
    loop {
        let mut progressed = false;
        if closed.load(Ordering::Acquire) && !stopping {
            host.receive(
                wire::ToHost::Stop {
                    session: session.clone(),
                },
                &center,
                Instant::now(),
            )?;
            stopping = true;
        }
        for _ in 0..16 {
            if !stopping {
                if let Ok(frame) = incoming.try_recv() {
                    progressed = true;
                    let requested_stop = matches!(frame, wire::ToHost::Stop { .. });
                    if host.receive(frame, &center, Instant::now()).is_err() {
                        quit.store(true, Ordering::Release);
                        host.receive(
                            wire::ToHost::Stop {
                                session: session.clone(),
                            },
                            &center,
                            Instant::now(),
                        )?;
                        stopping = true;
                    } else if requested_stop {
                        stopping = true;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if let Some(delegate) = delegate.as_ref().filter(|_| !stopping) {
            if delegate.failed() {
                quit.store(true, Ordering::Release);
                host.receive(
                    wire::ToHost::Stop {
                        session: session.clone(),
                    },
                    &center,
                    Instant::now(),
                )?;
                stopping = true;
            }
            for _ in 0..32 {
                let Some(envelope) = delegate.next() else {
                    break;
                };
                progressed = true;
                if host.clicked(&envelope).is_err() {
                    quit.store(true, Ordering::Release);
                    host.receive(
                        wire::ToHost::Stop {
                            session: session.clone(),
                        },
                        &center,
                        Instant::now(),
                    )?;
                    stopping = true;
                    break;
                }
            }
        }
        host.poll(&center, Instant::now());
        for _ in 0..32 {
            if pending.is_none() {
                pending = host.next_output();
            }
            if let Some(emission) = pending.take() {
                let stopped = matches!(emission.frame, wire::FromHost::Stopped { .. });
                if closed.load(Ordering::Acquire) {
                    progressed = true;
                    drop(emission); // Exact socket is physically closed already.
                    if stopped {
                        return Ok(());
                    }
                } else {
                    match outgoing.try_send(emission) {
                        Ok(()) if stopped => return Ok(()),
                        Ok(()) => progressed = true,
                        Err(mpsc::TrySendError::Full(emission)) => {
                            pending = Some(emission);
                            break;
                        }
                        Err(mpsc::TrySendError::Disconnected(emission)) => {
                            drop(emission); // Worker's Drop closed its original socket first.
                            if stopped {
                                return Ok(());
                            }
                            progressed = true;
                        }
                    }
                }
            } else {
                break;
            }
        }
        if host.native_close_requested() && pending.is_none() {
            drop(delegate.take());
        }
        turn(progressed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    #[test]
    fn plain_owner_metadata_refuses_foreign_shapes_and_keeps_the_file_contents() {
        let root = std::env::temp_dir().join(format!("zerus-host-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("owner");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert!(private_owner(file.as_raw_fd()));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!private_owner(file.as_raw_fd()));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = root.join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(!private_owner(file.as_raw_fd()));
        std::fs::remove_file(alias).unwrap();
        let readonly = File::open(&path).unwrap();
        assert!(!private_owner(readonly.as_raw_fd()));
        let (socket, _) = UnixStream::pair().unwrap();
        assert!(!private_owner(socket.as_raw_fd()));
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        drop((file, readonly));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn close_on_exec_only_changes_this_tests_owned_descriptor() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let fd = socket.as_raw_fd();
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        cloexec(fd).unwrap();
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
}
