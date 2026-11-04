//! Per-user Windows activation, matching Qt's `sessions\n` / `show\n` pipe.
//! The private exclusive file is the lifetime lock; a foreign pipe cannot
//! grant ownership. Only the current SID can access a pipe we create.
use crate::platform::windows::{own_sid, owned, token_sid, wide, Security};
#[cfg(not(feature = "live"))]
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::{
    fs::OpenOptionsExt,
    io::{AsRawHandle, OwnedHandle},
};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
    Arc, Mutex, PoisonError,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
#[cfg(not(feature = "live"))]
use unicode_casefold::UnicodeCaseFold;
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
    UI::WindowsAndMessaging::AllowSetForegroundWindow,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    Show,
    Sessions,
}
pub type Reply = Arc<dyn Fn(&OwnedHandle, &[u8]) -> Option<Vec<u8>> + Send + Sync>;
impl Activation {
    fn line(self) -> &'static [u8] {
        match self {
            Self::Show => b"show\n",
            Self::Sessions => b"sessions\n",
        }
    }
    fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes.trim_ascii() {
            b"show" => Some(Self::Show),
            b"sessions" => Some(Self::Sessions),
            _ => None,
        }
    }
}

pub struct Listening {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    _lock: File,
    _canonical: Option<Canonical>,
}
impl Drop for Listening {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Global kernel namespace + token SID, independent of profile/environment,
/// config, TEMP and Windows logon session. The object's existence is the
/// exclusion, not mutex ownership by a thread; a crash closes the last handle.
struct Canonical {
    _handle: OwnedHandle,
    pipe: String,
}
impl Canonical {
    fn name(sid: &str) -> String {
        format!("com.hgdev.hgs-tray.gui-{sid}")
    }
    fn acquire(pipe: String) -> io::Result<Option<Self>> {
        let security = Security::user_only()?;
        let attrs = security.attributes();
        let name = wide(&format!(r"Global\{pipe}"));
        let raw = unsafe { CreateMutexW(&attrs, 0, name.as_ptr()) };
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = owned(raw)?;
        Ok((!existed).then_some(Self {
            _handle: handle,
            pipe,
        }))
    }
}
pub struct Claimed {
    pub listening: Listening,
    pub activations: Receiver<Activation>,
}
pub enum Claim {
    Claimed(Claimed),
    Forwarded,
    Failed(String),
}
pub struct InstanceState(pub Mutex<Option<Listening>>);
impl InstanceState {
    pub fn release(&self) {
        if let Some(mut listening) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() {
            // Final GUI teardown can precede actual process exit. Keep the
            // canonical kernel handle until the OS closes it at exit.
            std::mem::forget(listening._canonical.take());
            drop(listening);
        }
    }
}

#[cfg(feature = "live")]
fn key(name: &str, home: &Path) -> String {
    zerus_client_core::desktop::windows::activation_name(name, home)
}
#[cfg(not(feature = "live"))]
fn key(name: &str, home: &Path) -> String {
    let digest = Sha256::digest(
        home.to_string_lossy()
            .replace('\\', "/")
            .case_fold()
            .collect::<String>()
            .as_bytes(),
    );
    format!("{name}-{:x}", digest)[..name.len() + 17].to_owned()
}

pub(crate) fn same_user(pipe: &OwnedHandle) -> bool {
    let mut pid = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 || pid == 0 {
        return false;
    }
    let Ok(process) = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
    else {
        return false;
    };
    let mut token = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            process.as_raw_handle(),
            windows_sys::Win32::Security::TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return false;
    }
    owned(token)
        .and_then(|token| token_sid(&token))
        .ok()
        .zip(own_sid().ok())
        .is_some_and(|(peer, own)| peer == own)
}

fn same_user_server(pipe: &OwnedHandle, sid: &str) -> io::Result<u32> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    let mut token = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            process.as_raw_handle(),
            windows_sys::Win32::Security::TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if token_sid(&owned(token)?)? != sid {
        return Err(io::Error::other(
            "Activation pipe belongs to a different Windows user",
        ));
    }
    Ok(pid)
}

fn forward(name: &str, activation: Activation) -> bool {
    let Ok(sid) = own_sid() else { return false };
    let pipe = wide(&format!(r"\\.\pipe\{name}"));
    let end = Instant::now() + Duration::from_millis(200);
    loop {
        let handle = unsafe {
            CreateFileW(
                pipe.as_ptr(),
                FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if let Ok(handle) = owned(handle) {
            let Ok(pid) = same_user_server(&handle, &sid) else {
                return false;
            };
            if activation == Activation::Sessions {
                unsafe {
                    AllowSetForegroundWindow(pid);
                }
            }
            let mode = PIPE_NOWAIT;
            if unsafe {
                SetNamedPipeHandleState(
                    handle.as_raw_handle(),
                    &mode,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } == 0
            {
                return false;
            }
            let bytes = activation.line();
            let mut written = 0;
            return unsafe {
                WriteFile(
                    handle.as_raw_handle(),
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            } != 0
                && written == bytes.len() as u32;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn listen(
    pipe: OwnedHandle,
    stop: Arc<AtomicBool>,
    sender: mpsc::SyncSender<Activation>,
    reply: Option<Reply>,
) {
    while !stop.load(Ordering::Acquire) {
        let state = if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0
        {
            // PIPE_NOWAIT success makes a disconnected instance available;
            // it does not mean a client has connected yet.
            ERROR_PIPE_LISTENING
        } else {
            unsafe { GetLastError() }
        };
        match state {
            // A short-lived launcher can write and close before this thread
            // accepts it. Drain those bytes before DisconnectNamedPipe discards
            // them, and reset the instance for the following launcher.
            ERROR_PIPE_CONNECTED | ERROR_NO_DATA => {}
            ERROR_PIPE_LISTENING => {
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            error => {
                eprintln!(
                    "zerus-client: activation pipe: {}",
                    io::Error::from_raw_os_error(error as i32)
                );
                return;
            }
        }
        let end = Instant::now() + Duration::from_millis(200);
        let mut bytes = Vec::with_capacity(64);
        while Instant::now() < end && !stop.load(Ordering::Acquire) && bytes.len() < 64 {
            let mut buffer = [0u8; 64];
            let mut read = 0;
            let ok = unsafe {
                ReadFile(
                    pipe.as_raw_handle(),
                    buffer.as_mut_ptr(),
                    (64 - bytes.len()) as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 && read > 0 {
                bytes.extend_from_slice(&buffer[..read as usize]);
                if let Some(lf) = bytes.iter().position(|b| *b == b'\n') {
                    if let Some(activation) = Activation::parse(&bytes[..=lf]) {
                        let _ = sender.try_send(activation);
                    } else if lf + 1 == bytes.len() && same_user(&pipe) {
                        if let Some(answer) =
                            reply.as_ref().and_then(|reply| reply(&pipe, &bytes[..lf]))
                        {
                            send_reply(&pipe, &stop, end, &answer);
                        }
                    }
                    break;
                }
            } else if unsafe { GetLastError() } != ERROR_NO_DATA {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            DisconnectNamedPipe(pipe.as_raw_handle());
        }
    }
}

fn send_reply(pipe: &OwnedHandle, stop: &AtomicBool, end: Instant, bytes: &[u8]) {
    if bytes.is_empty() || bytes.len() > 4096 || bytes.last() != Some(&b'\n') {
        return;
    }
    let mut sent = 0;
    while sent < bytes.len() && Instant::now() < end && !stop.load(Ordering::Acquire) {
        let mut written = 0;
        if unsafe {
            WriteFile(
                pipe.as_raw_handle(),
                bytes[sent..].as_ptr(),
                (bytes.len() - sent) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return;
        }
        sent += written as usize;
        if written == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // DisconnectNamedPipe discards unread output. Yield until the client has
    // consumed its bounded line and closed, or the original deadline expires.
    // Never FlushFileBuffers: a slow client must not pin the listener or drop.
    while sent == bytes.len() && Instant::now() < end && !stop.load(Ordering::Acquire) {
        if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } == 0
            && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn create_pipe(name: &str, security: &Security) -> io::Result<OwnedHandle> {
    let attrs = security.attributes();
    let pipe_name = wide(&format!(r"\\.\pipe\{name}"));
    owned(unsafe {
        CreateNamedPipeW(
            pipe_name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            64,
            200,
            &attrs,
        )
    })
}

pub fn claim(
    name: &str,
    _label: Option<&str>,
    activation: Activation,
    reply: Option<Reply>,
    admitted: impl FnOnce(),
) -> Claim {
    let canonical = if name == "hgs-tray" {
        let sid = match own_sid() {
            Ok(sid) => sid,
            Err(error) => return Claim::Failed(error.to_string()),
        };
        Some(Canonical::name(&sid))
    } else {
        None
    };
    claim_in(name, activation, reply, canonical, admitted)
}

fn claim_in(
    name: &str,
    activation: Activation,
    reply: Option<Reply>,
    canonical: Option<String>,
    admitted: impl FnOnce(),
) -> Claim {
    let canonical = if let Some(pipe) = canonical {
        match Canonical::acquire(pipe.clone()) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !forward(&pipe, activation) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return Claim::Forwarded;
            }
            Err(error) => return Claim::Failed(error.to_string()),
        }
    } else {
        None
    };
    let home = crate::identity::home_dir();
    if home.as_os_str().is_empty() {
        return Claim::Failed("The Windows profile directory is unavailable".into());
    }
    let name = key(name, &home);
    let directory = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"))
        .join("hgdev/hgs-tray");
    claim_guarded(&name, &directory, activation, reply, canonical, admitted)
}
#[cfg(test)]
fn claim_at(name: &str, directory: &Path, activation: Activation) -> Claim {
    claim_with_reply_at(name, directory, activation, None)
}
#[cfg(test)]
fn claim_with_reply_at(
    name: &str,
    directory: &Path,
    activation: Activation,
    reply: Option<Reply>,
) -> Claim {
    claim_guarded(name, directory, activation, reply, None, || {})
}
fn claim_guarded(
    name: &str,
    directory: &Path,
    activation: Activation,
    reply: Option<Reply>,
    canonical: Option<Canonical>,
    admitted: impl FnOnce(),
) -> Claim {
    // Also recognizes an already-running Qt GUI using this activation protocol.
    if forward(name, activation) {
        return Claim::Forwarded;
    }
    if let Err(e) = std::fs::create_dir_all(directory) {
        return Claim::Failed(e.to_string());
    }
    let mut lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(directory.join(format!("{name}.instance-lock")))
    {
        Ok(lock) => lock,
        Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
            let _ = forward(name, activation);
            return Claim::Forwarded;
        }
        Err(e) => return Claim::Failed(e.to_string()),
    };
    if let Err(e) = lock
        .set_len(0)
        .and_then(|()| writeln!(lock, "{}", std::process::id()))
    {
        return Claim::Failed(e.to_string());
    }
    let security = match Security::user_only() {
        Ok(s) => s,
        Err(e) => return Claim::Failed(e.to_string()),
    };
    let pipe = match create_pipe(name, &security) {
        Ok(pipe) => pipe,
        Err(error) => return Claim::Failed(error.to_string()),
    };
    let canonical_pipe = match canonical
        .as_ref()
        .map(|guard| create_pipe(&guard.pipe, &security))
        .transpose()
    {
        Ok(pipe) => pipe,
        Err(error) => return Claim::Failed(error.to_string()),
    };
    let (tx, rx) = mpsc::sync_channel(32);
    admitted();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    if let Some(pipe) = canonical_pipe {
        let stopped = stop.clone();
        let sender = tx.clone();
        // Public activation only. Authenticated desktop requests stay bound
        // to their original profile's compatibility pipe.
        threads.push(std::thread::spawn(move || {
            listen(pipe, stopped, sender, None)
        }));
    }
    let stopped = stop.clone();
    threads.push(std::thread::spawn(move || listen(pipe, stopped, tx, reply)));
    Claim::Claimed(Claimed {
        listening: Listening {
            stop,
            threads,
            _lock: lock,
            _canonical: canonical,
        },
        activations: rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    mod canonical_tests {
        use super::*;
        include!("instance_windows_tests.rs");
    }
    #[cfg(feature = "live")]
    mod evidence {
        use super::*;
        use std::{
            io::Write,
            os::windows::process::CommandExt,
            process::{Child, Command, Stdio},
        };
        use zerus_client_core::desktop::{self, ui_requests::Request, Build, Responder};
        struct Folder(std::path::PathBuf);
        impl Folder {
            fn new() -> Self {
                let path = std::env::temp_dir()
                    .join(format!("zerus-request-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir(&path).unwrap();
                Self(path)
            }
            fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Folder {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        struct Held(Child);
        impl Drop for Held {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn responder() -> Arc<Responder> {
            let responder = Arc::new(Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: "1.56.0".into(),
                    source_commit: "a".repeat(40),
                    source_dirty: Some(false),
                },
                uuid::Uuid::new_v4().to_string(),
                true,
                false,
            ));
            assert!(responder.configure_once("a".repeat(64)));
            responder
        }
        fn report_chain(mut pid: u32) {
            use zerus_client_core::desktop::windows::process::Original;
            let until = Instant::now() + Duration::from_secs(2);
            let mut rows = Vec::new();
            for _ in 0..32 {
                if Instant::now() >= until {
                    break;
                }
                match Original::open(pid) {
                    Ok(original) => {
                        let name = Path::new(&original.image)
                            .file_name()
                            .map(|v| v.to_string_lossy().chars().take(64).collect::<String>());
                        let environment_readable =
                            original.own.then(|| original.environment(until).is_ok());
                        rows.push(serde_json::json!({"pid":pid,"birth":original.birth.to_string(),
                            "parent":original.parent,"image":name,"own":original.own,
                            "current":original.current(),"environment_readable":environment_readable}));
                        if !original.own || original.parent == 0 {
                            break;
                        }
                        pid = original.parent;
                    }
                    Err(error) => {
                        rows.push(serde_json::json!({"pid":pid,"unavailable":error.to_string()}));
                        break;
                    }
                }
            }
            println!("\nFRONTEND_CHAIN {}", serde_json::to_string(&rows).unwrap());
        }
        /// Child-only fixture driven by the real private CLI integration test.
        /// It uses the production listener/cache/owned runner, creates no WebView
        /// and never enables a policy in the passive test service.
        #[test]
        fn native_frontend_request_fixture() {
            use std::{io::Read, sync::atomic::AtomicBool};
            let Some(path) = std::env::var_os("HGS_FRONTEND_TEST_CONTROL") else {
                return;
            };
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .unwrap()
                .take(4097)
                .read_to_end(&mut bytes)
                .unwrap();
            assert!(bytes.len() <= 4096);
            let control: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let mode = control["case"].as_str().unwrap().to_owned();
            if mode == "mode_check" {
                report_chain(std::process::id());
                let invocation = zerus_client_core::Invocation {
                    args: vec![
                        "desktop".into(),
                        "registration".into(),
                        "mode-check".into(),
                        "--json".into(),
                    ],
                    stdin: None,
                    timeout: Duration::from_secs(8),
                    request_id: None,
                };
                let environment: Vec<_> = std::env::vars()
                    .filter(|(key, _)| {
                        !key.eq_ignore_ascii_case("HGS_SESSION")
                            && !key.eq_ignore_ascii_case("HGS_RUN_ID")
                    })
                    .collect();
                let environment: Vec<_> = environment
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                let result = zerus_client_core::run::run_hidden_limited(
                    Path::new(control["cli"].as_str().unwrap()),
                    &invocation,
                    &environment,
                    65536,
                )
                .unwrap();
                let zerus_client_core::actions::Exit::Code {
                    code,
                    stdout,
                    stderr,
                } = result
                else {
                    panic!("original mode-check fixture child did not settle")
                };
                println!(
                    "\nMODE_CALLER_RESULT {}",
                    serde_json::json!({"code":code,
                    "stdout":String::from_utf8(stdout).unwrap(),"stderr":stderr})
                );
                return;
            }
            if mode == "observe" {
                // Private test observation only. This PID never supplies a
                // request peer, cached confirmation or mutation permit.
                let pid = u32::try_from(control["pid"].as_u64().unwrap()).unwrap();
                report_chain(pid);
                let result = zerus_client_core::desktop::windows::process::Ancestry::inspect(
                    pid,
                    Instant::now() + Duration::from_secs(2),
                );
                println!(
                    "\nFRONTEND_OBSERVATION {}",
                    serde_json::json!({
                    "diagnostic_only":true,"pid":pid,"admitted":result.is_ok(),
                    "error":result.err().map(|e| e.to_string())})
                );
                return;
            }
            let info = crate::cli::build_info(&crate::cli::BUILD);
            let responder = Arc::new(Responder::new(
                "com.hgdev.hgs-tray".into(),
                Build {
                    version: info["versionName"].as_str().unwrap().into(),
                    source_commit: info["sourceCommit"].as_str().unwrap().into(),
                    source_dirty: info["sourceDirty"].as_bool(),
                },
                uuid::Uuid::new_v4().to_string(),
                true,
                false,
            ));
            let configuration = control["configuration"].as_str().unwrap();
            assert!(responder.configure_once(if mode == "wrong_config" {
                "b".repeat(64)
            } else {
                configuration.into()
            }));
            let request = Request::AutomaticApprovals {
                request_id: control["request_id"].as_str().unwrap().into(),
                enabled: mode == "enable",
            };
            let registration = responder
                .prepare_ui_request(uuid::Uuid::new_v4().to_string(), request)
                .unwrap();
            let envelope = registration.envelope().clone();
            let resources = registration.resources();
            let cached = responder.clone();
            let reply: Reply = Arc::new(move |_, line| {
                let bytes = cached.reply(line)?;
                if mode == "wrong_birth" {
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                    value["request"]["child_birth"] = "0".into();
                    let mut bytes = serde_json::to_vec(&value).ok()?;
                    bytes.push(b'\n');
                    Some(bytes)
                } else if mode == "wrong_fingerprint" {
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                    value["request"]["fingerprint"] = "0".repeat(64).into();
                    let mut bytes = serde_json::to_vec(&value).ok()?;
                    bytes.push(b'\n');
                    Some(bytes)
                } else {
                    Some(bytes)
                }
            });
            // This fixture tests the authenticated profile pipe, not GUI
            // admission. Never touch the login user's canonical GUI guard.
            let home = crate::identity::home_dir();
            let directory = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
                .join("hgdev/hgs-tray");
            let Claim::Claimed(claimed) = claim_with_reply_at(
                &key("hgs-tray", &home),
                &directory,
                Activation::Show,
                Some(reply),
            ) else {
                panic!("private canonical fixture claim")
            };
            report_chain(std::process::id());
            let inv = zerus_client_core::Invocation {
                args: vec!["desktop".into(), "frontend-request".into(), "--json".into()],
                stdin: Some(serde_json::to_vec(&envelope).unwrap()),
                timeout: Duration::from_secs(8),
                request_id: Some(envelope.request.id().into()),
            };
            let output = zerus_client_core::run::run_owned(
                Path::new(control["cli"].as_str().unwrap()),
                &inv,
                &[],
                &AtomicBool::new(false),
                move |command| {
                    let child = command.spawn()?;
                    let _ = registration.bind_original(&child);
                    Ok(child)
                },
                resources,
            )
            .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
            println!(
                "\nFRONTEND_RESULT {}",
                serde_json::to_string(&value).unwrap()
            );
            assert!(claimed.activations.try_recv().is_err());
            drop(claimed);
            drop(responder);
        }
        #[test]
        fn actual_pipe_server_and_original_child_evidence_preserve_activation() {
            let root = Folder::new();
            let name = format!("zerus-request-{}", uuid::Uuid::new_v4());
            let responder = responder();
            let attempt = uuid::Uuid::new_v4().to_string();
            let request = Request::AutomaticApprovals {
                request_id: uuid::Uuid::new_v4().to_string(),
                enabled: false,
            };
            let registration = responder
                .prepare_ui_request(attempt.clone(), request.clone())
                .unwrap();
            let mut child = Held(
                Command::new("cmd.exe")
                    .args(["/D", "/Q", "/C", "set /p HGS_TEST_WAIT="])
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            assert!(registration.bind_original(&child.0));
            let resources = registration.resources();
            let reply: Reply = Arc::new(move |_, line| responder.reply(line));
            let Claim::Claimed(claimed) =
                claim_with_reply_at(&name, root.path(), Activation::Show, Some(reply))
            else {
                panic!("private claim")
            };
            let (peer, reply) = desktop::windows::probe_ui_request_at(
                &name,
                &attempt,
                &"a".repeat(64),
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
            assert_eq!(peer.pid, std::process::id());
            assert!(peer.current());
            assert_eq!(reply.request.child_pid, child.0.id());
            assert_eq!(reply.request.request_id, request.id());
            assert_eq!(reply.request.fingerprint, request.fingerprint());
            assert_eq!(
                reply.request.child_birth,
                desktop::windows::process::Original::open(child.0.id())
                    .unwrap()
                    .birth
                    .to_string()
            );
            assert!(
                claimed.activations.try_recv().is_err(),
                "a query cannot show a window"
            );
            assert!(desktop::windows::probe_ui_request_at(
                &name,
                &attempt,
                &"b".repeat(64),
                Instant::now() + Duration::from_secs(2)
            )
            .is_none());
            assert!(forward(&name, Activation::Sessions));
            assert_eq!(
                claimed
                    .activations
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap(),
                Activation::Sessions
            );
            child.0.stdin.take().unwrap().write_all(b"done\n").unwrap();
            child.0.wait().unwrap();
            drop(resources);
            drop(registration);
            drop(claimed);
        }
        #[test]
        fn a_client_not_reading_its_reply_cannot_pin_the_listener_on_shutdown() {
            let root = Folder::new();
            let name = format!("zerus-slow-request-{}", uuid::Uuid::new_v4());
            let (sent, observed) = mpsc::sync_channel(1);
            let reply: Reply = Arc::new(move |_, _| {
                let _ = sent.try_send(());
                Some(b"{}\n".to_vec())
            });
            let Claim::Claimed(claimed) =
                claim_with_reply_at(&name, root.path(), Activation::Show, Some(reply))
            else {
                panic!("private claim")
            };
            let pipe = wide(&format!(r"\\.\pipe\{name}"));
            let client = owned(unsafe {
                CreateFileW(
                    pipe.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            })
            .unwrap();
            let query = desktop::ui_requests::query(&uuid::Uuid::new_v4().to_string()).unwrap();
            let mut written = 0;
            assert_ne!(
                unsafe {
                    WriteFile(
                        client.as_raw_handle(),
                        query.as_ptr(),
                        query.len() as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            assert_eq!(written as usize, query.len());
            observed.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(claimed.activations.try_recv().is_err());
            let start = Instant::now();
            drop(claimed);
            assert!(start.elapsed() < Duration::from_secs(2));
            drop(client);
        }
        #[test]
        fn malformed_truncated_and_oversize_cached_replies_never_activate_or_pin_listener() {
            for body in [b"{\"hello\":".to_vec(), vec![b'x'; 4097], b"{}\n".to_vec()] {
                let root = Folder::new();
                let name = format!("zerus-bad-request-{}", uuid::Uuid::new_v4());
                let reply: Reply = Arc::new(move |_, _| Some(body.clone()));
                let Claim::Claimed(claimed) =
                    claim_with_reply_at(&name, root.path(), Activation::Show, Some(reply))
                else {
                    panic!("private claim")
                };
                assert!(desktop::windows::probe_ui_request_at(
                    &name,
                    &uuid::Uuid::new_v4().to_string(),
                    &"a".repeat(64),
                    Instant::now() + Duration::from_secs(2)
                )
                .is_none());
                assert!(claimed.activations.try_recv().is_err());
                let start = Instant::now();
                drop(claimed);
                assert!(start.elapsed() < Duration::from_secs(2));
            }
        }
        #[test]
        fn remote_names_and_unknown_attempts_fail_without_activation() {
            let root = Folder::new();
            let name = format!("zerus-unknown-request-{}", uuid::Uuid::new_v4());
            let responder = responder();
            let reply: Reply = Arc::new(move |_, line| responder.reply(line));
            let Claim::Claimed(claimed) =
                claim_with_reply_at(&name, root.path(), Activation::Show, Some(reply))
            else {
                panic!("private claim")
            };
            assert!(desktop::windows::probe_ui_request_at(
                &name,
                &uuid::Uuid::new_v4().to_string(),
                &"a".repeat(64),
                Instant::now() + Duration::from_secs(2)
            )
            .is_none());
            assert!(desktop::windows::probe_ui_request_at(
                r"\\example.invalid\pipe\hgs-tray",
                &uuid::Uuid::new_v4().to_string(),
                &"a".repeat(64),
                Instant::now() + Duration::from_secs(2)
            )
            .is_none());
            assert!(claimed.activations.try_recv().is_err());
            drop(claimed);
        }
    }
    #[test]
    fn closed_client_is_drained_before_accept_and_the_next_client_can_connect() {
        let name = format!("zerus-early-close-{}", uuid::Uuid::new_v4());
        let pipe = create_pipe(&name, &Security::user_only().unwrap()).unwrap();
        // No listener exists yet: forward must have written AND closed its
        // handle before the first ConnectNamedPipe call can possibly happen.
        assert!(forward(&name, Activation::Show));
        let (sender, receiver) = mpsc::sync_channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || listen(pipe, stopped, sender, None));
        let first = receiver.recv_timeout(Duration::from_secs(2));
        let forwarded = forward(&name, Activation::Sessions);
        let second = receiver.recv_timeout(Duration::from_secs(2));
        stop.store(true, Ordering::Release);
        thread.join().unwrap();
        assert_eq!(first, Ok(Activation::Show));
        assert!(forwarded);
        assert_eq!(second, Ok(Activation::Sessions));
    }
    #[test]
    fn profile_key_keeps_qt_unicode_casefold_compatibility() {
        assert_eq!(
            key("hgs-tray", Path::new(r"C:\Users\STRAẞE")),
            key("hgs-tray", Path::new("c:/users/strasse"))
        );
    }
    #[test]
    fn activation_is_bounded_and_unknown_commands_do_not_open_a_window() {
        assert_eq!(
            Activation::parse(b" sessions\r\n"),
            Some(Activation::Sessions)
        );
        assert_eq!(Activation::parse(b"show\n"), Some(Activation::Show));
        assert_eq!(Activation::parse(b"quit\n"), None);
    }
    #[test]
    fn login_shortcut_plus_run_registration_never_opens_a_background_window() {
        let root =
            std::env::temp_dir().join(format!("zerus-background-test-{}", uuid::Uuid::new_v4()));
        let name = format!("zerus-background-{}", uuid::Uuid::new_v4());
        let options = crate::cli::Options {
            background: true,
            ..Default::default()
        };
        assert!(!options.opens_sessions(true));
        assert!(!crate::lifecycle::window_at_start(
            options.opens_sessions(true),
            true
        ));
        let Claim::Claimed(first) = claim_at(&name, &root, Activation::Show) else {
            panic!("first login start")
        };
        assert!(matches!(
            claim_at(&name, &root, Activation::Show),
            Claim::Forwarded
        ));
        assert_eq!(
            first
                .activations
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Activation::Show
        );
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_second_start_forwards_and_release_allows_a_new_owner() {
        let root = std::env::temp_dir().join(format!("zerus-pipe-test-{}", uuid::Uuid::new_v4()));
        let name = format!("zerus-test-{}", uuid::Uuid::new_v4());
        let Claim::Claimed(first) = claim_at(&name, &root, Activation::Show) else {
            panic!("first claim")
        };
        assert!(matches!(
            claim_at(&name, &root, Activation::Sessions),
            Claim::Forwarded
        ));
        assert_eq!(
            first
                .activations
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            Activation::Sessions
        );
        drop(first);
        let Claim::Claimed(next) = claim_at(&name, &root, Activation::Show) else {
            panic!("released claim")
        };
        drop(next);
        std::fs::remove_dir_all(root).unwrap();
    }
}
