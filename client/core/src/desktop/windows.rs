//! Read-only queries on the existing canonical per-user activation pipe.
use super::{ui_requests, PROBE_WAIT, REPLY_BYTES};
use sha2::{Digest, Sha256};
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    time::Instant,
};
use unicode_casefold::UnicodeCaseFold;
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

pub mod process;

/// Qt-compatible Unicode casefold and original 16-hex home digest. Both the
/// listener and service use this function; neither selects another namespace.
pub fn activation_name(name: &str, home: &Path) -> String {
    let digest = Sha256::digest(
        home.to_string_lossy()
            .replace('\\', "/")
            .case_fold()
            .collect::<String>()
            .as_bytes(),
    );
    format!("{name}-{:x}", digest)[..name.len() + 17].to_owned()
}
fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful native call transfers this one handle to its owner.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
fn user(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut token = std::ptr::null_mut();
    // SAFETY: live process handle and writable output.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut size = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut size,
        )
    };
    if size == 0 || size > 64 << 10 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut data = vec![0u64; (size as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            data.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: native TOKEN_USER and SID were written into aligned owned data.
    let sid = unsafe { (*(data.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    Ok(
        unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize) }
            .to_vec(),
    )
}
fn birth(process: HANDLE) -> Option<String> {
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: live handle, exactly four writable outputs.
    (unsafe {
        GetProcessTimes(
            process,
            &mut times[0],
            &mut times[1],
            &mut times[2],
            &mut times[3],
        )
    } != 0)
        .then(|| {
            ((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
                .to_string()
        })
}
/// Retain the actual server object through authentication and mutation. A PID
/// obtained later, a JSON PID, or a connection failure cannot substitute it.
pub struct Peer {
    pub pid: u32,
    pub birth: String,
    process: OwnedHandle,
}
impl Peer {
    pub fn current(&self) -> bool {
        (unsafe { GetProcessId(self.process.as_raw_handle()) }) == self.pid
            && (unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) }) == WAIT_TIMEOUT
            && birth(self.process.as_raw_handle()).as_deref() == Some(&self.birth)
    }
}
fn server(pipe: &OwnedHandle) -> Option<Peer> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 || pid == 0 {
        return None;
    }
    let process = owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    })
    .ok()?;
    if user(process.as_raw_handle()).ok()? != user(unsafe { GetCurrentProcess() }).ok()? {
        return None;
    }
    let peer = Peer {
        pid,
        birth: birth(process.as_raw_handle())?,
        process,
    };
    let mut again = 0;
    (peer.current()
        && unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut again) } != 0
        && again == pid)
        .then_some(peer)
}

/// One bounded local exchange, never WaitNamedPipe/FlushFileBuffers or a helper
/// process. Keep the authenticated server handle with the decoded evidence.
pub fn probe_ui_request_at(
    name: &str,
    attempt: &str,
    configuration: &str,
    until: Instant,
) -> Option<(Peer, ui_requests::Reply)> {
    if name.is_empty()
        || name.len() > 128
        || name
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && b != b'-')
    {
        return None;
    }
    let request = ui_requests::query(attempt)?;
    let until = until.min(Instant::now() + PROBE_WAIT);
    let name: Vec<u16> = format!(r"\\.\pipe\{name}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let pipe = loop {
        if Instant::now() >= until {
            return None;
        }
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        match owned(handle) {
            Ok(pipe) => break pipe,
            Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(_) => return None,
        }
    };
    let peer = server(&pipe)?;
    let mode = PIPE_NOWAIT;
    if unsafe {
        SetNamedPipeHandleState(
            pipe.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return None;
    }
    let mut written = 0;
    while written < request.len() {
        if Instant::now() >= until || !peer.current() {
            return None;
        }
        let mut count = 0;
        let ok = unsafe {
            WriteFile(
                pipe.as_raw_handle(),
                request[written..].as_ptr(),
                (request.len() - written) as u32,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        written += count as usize;
        if count == 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let mut reply = Vec::new();
    loop {
        if Instant::now() >= until || reply.len() == REPLY_BYTES || !peer.current() {
            return None;
        }
        let mut bytes = [0u8; REPLY_BYTES];
        let mut count = 0;
        let cap = bytes.len().min(REPLY_BYTES - reply.len());
        let ok = unsafe {
            ReadFile(
                pipe.as_raw_handle(),
                bytes.as_mut_ptr(),
                cap as u32,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if count > 0 {
            reply.extend_from_slice(&bytes[..count as usize]);
        }
        if let Some(lf) = reply.iter().position(|b| *b == b'\n') {
            if lf + 1 != reply.len() || !peer.current() {
                return None;
            }
            let reply: ui_requests::Reply = serde_json::from_slice(&reply[..lf]).ok()?;
            return (reply.hello.identity_matches(attempt, configuration)
                && reply.request.attempt == attempt)
                .then_some((peer, reply));
        }
        if ok == 0 && unsafe { GetLastError() } != ERROR_NO_DATA {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
