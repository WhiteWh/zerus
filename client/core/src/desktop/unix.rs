use super::{query, Hello, Observation, PROBE_WAIT, REPLY_BYTES};
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::AsRawFd,
        net::UnixStream,
    },
    path::{Component, Path, PathBuf},
    time::Instant,
};

/// Qt's QDir::tempPath/cleanPath, shared with Next's original path selection.
/// Preserve symlinks (notably /tmp on macOS) and the original socket length.
pub fn temporary_dir_from(tmpdir: Option<OsString>) -> PathBuf {
    let temp = tmpdir
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(user_temp_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut parts = Vec::new();
    for part in temp.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir) => {}
                _ => parts.push(part),
            },
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}
#[cfg(target_os = "macos")]
fn user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: output buffer is writable for its declared length.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if length == 0 || length > buffer.len() {
        return None;
    }
    // SAFETY: confstr wrote a complete NUL-terminated string in the buffer.
    let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(text.to_bytes())))
}
#[cfg(not(target_os = "macos"))]
fn user_temp_dir() -> Option<PathBuf> {
    None
}

pub fn same_user(stream: &UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    let uid = {
        // SAFETY: live fd and correctly sized output storage.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut credentials as *mut _ as *mut _,
                &mut size,
            )
        } != 0
            || size as usize != std::mem::size_of_val(&credentials)
        {
            return false;
        }
        credentials.uid
    };
    #[cfg(not(target_os = "linux"))]
    let uid = {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: live fd and valid output pointers.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return false;
        }
        uid
    };
    // SAFETY: geteuid has no preconditions.
    uid == unsafe { libc::geteuid() }
}

fn ready(stream: &UnixStream, events: i16, until: Instant) -> io::Result<()> {
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: one valid, writable pollfd; bounded remaining total deadline.
        let result = unsafe {
            libc::poll(
                &mut fd,
                1,
                left.as_millis().max(1).min(i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
fn exchange(mut stream: UnixStream, request: &[u8], until: Instant) -> Option<Vec<u8>> {
    if !same_user(&stream) {
        return None;
    }
    let mut written = 0;
    while written < request.len() {
        ready(&stream, libc::POLLOUT, until).ok()?;
        match stream.write(&request[written..]) {
            Ok(0) => return None,
            Ok(n) => written += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
    let mut bytes = Vec::new();
    loop {
        ready(&stream, libc::POLLIN, until).ok()?;
        let mut chunk = [0u8; 512];
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.len() > REPLY_BYTES {
                    return None;
                }
                if let Some(end) = bytes.iter().position(|&b| b == b'\n') {
                    if end + 1 != bytes.len() || Instant::now() >= until {
                        return None;
                    }
                    bytes.truncate(end);
                    return Some(bytes);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
}

/// Read-only, one attempt, no child/thread/cache and a one-second total socket
/// deadline. Caller supplies a FRESH random nonce for each probe. Never unlink
/// or claim the canonical socket. Only an absent path means Absent; an existing
/// stale/unsafe/unreachable endpoint is Unknown. A connected silent legacy peer
/// is LiveLegacyOrUnknown, never Absent. All outcomes remain passive.
pub fn probe_at(path: &Path, nonce: &str, configuration: &str) -> Observation {
    let Some(request) = query(nonce) else {
        return Observation::Unknown;
    };
    match probe_request(path, &request) {
        Raw::Absent => Observation::Absent,
        Raw::Unknown => Observation::Unknown,
        Raw::Live(bytes, _) => match bytes.and_then(|b| serde_json::from_slice::<Hello>(&b).ok()) {
            Some(hello) if hello.identity_matches(nonce, configuration) => Observation::LiveNext {
                hello: Box::new(hello),
            },
            _ => Observation::LiveLegacyOrUnknown,
        },
    }
}

/// Same authenticated, bounded socket exchange. A reply without a matched
/// cached challenge is useful for diagnostics only, never producer admission.
pub fn probe_producer_at(
    path: &Path,
    nonce: &str,
    configuration: &str,
) -> super::producer::Observation {
    use super::producer::{self, Observation};
    let Some(request) = producer::query(nonce) else {
        return Observation::Unknown;
    };
    match probe_request(path, &request) {
        Raw::Absent => Observation::Absent,
        Raw::Unknown => Observation::Unknown,
        Raw::Live(bytes, _) => match bytes
            .and_then(|b| serde_json::from_slice::<producer::Reply>(&b).ok())
        {
            Some(reply) if reply.identity_matches(nonce, configuration) => Observation::LiveNext {
                reply: Box::new(reply),
            },
            _ => Observation::LiveLegacyOrUnknown,
        },
    }
}

/// The evidence is still untrusted data. A service must match the returned
/// canonical frontend PID and the original child PID/birth/fingerprint against
/// its own socket peer, and check agent ancestry before accepting an intent.
pub fn probe_ui_request_at(
    path: &Path,
    attempt: &str,
    configuration: &str,
) -> Option<(u32, super::ui_requests::Reply)> {
    let request = super::ui_requests::query(attempt)?;
    let Raw::Live(Some(bytes), Some(pid)) = probe_request(path, &request) else {
        return None;
    };
    let reply: super::ui_requests::Reply = serde_json::from_slice(&bytes).ok()?;
    (reply.hello.identity_matches(attempt, configuration) && reply.request.attempt == attempt)
        .then_some((pid, reply))
}

fn peer_pid(stream: &UnixStream) -> Option<u32> {
    if !same_user(stream) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let mut value: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&value) as libc::socklen_t;
        // SAFETY: live socket and exactly sized output credentials.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut value as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        (result == 0 && size as usize == std::mem::size_of_val(&value) && value.pid > 0)
            .then_some(value.pid as u32)
    }
    #[cfg(target_os = "macos")]
    {
        let mut value: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&value) as libc::socklen_t;
        // SAFETY: live socket and one writable native PID.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut value as *mut libc::pid_t).cast(),
                &mut size,
            )
        };
        (result == 0 && size as usize == std::mem::size_of_val(&value) && value > 0)
            .then_some(value as u32)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}
enum Raw {
    Absent,
    Unknown,
    Live(Option<Vec<u8>>, Option<u32>),
}
fn probe_request(path: &Path, request: &[u8]) -> Raw {
    let until = Instant::now() + PROBE_WAIT;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Raw::Absent,
        Err(_) => return Raw::Unknown,
    };
    // SAFETY: geteuid has no preconditions. Qt's permissions may be wider than
    // the private desktop-v1 endpoint; OS peer credentials are checked below.
    if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
        return Raw::Unknown;
    }
    let stream = (|| -> io::Result<UnixStream> {
        // socket2 supplies CLOEXEC and Apple NOSIGPIPE. Never block in
        // connect on a full backlog and never retry connect after uncertainty.
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.set_nonblocking(true)?;
        let connected = socket.connect(&SockAddr::unix(path)?);
        let fd: std::os::fd::OwnedFd = socket.into();
        let stream = UnixStream::from(fd);
        if let Err(error) = connected {
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR)
            ) {
                return Err(error);
            }
            ready(&stream, libc::POLLOUT, until)?;
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            // Writable alone is insufficient after a pending Unix connect.
            stream.peer_addr()?;
        }
        Ok(stream)
    })();
    let Ok(stream) = stream else {
        return Raw::Unknown;
    };
    let pid = peer_pid(&stream);
    Raw::Live(exchange(stream, request, until), pid)
}
