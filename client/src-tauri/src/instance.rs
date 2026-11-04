//! One client per user, by Qt's protocol (`tray/src/SingleInstance.{h,cpp}`,
//! `tray/src/main.cpp:337-361` and `:424-433`). Concurrent starts take a
//! per-user lock in turn; the running instance listens on a Unix socket and
//! reads one line, `sessions` or `show`; a start that reaches it forwards its
//! line and exits. The switch bundle uses Qt's names, so
//! `tray/update-installed.py` finds the socket (a path ending in `/hgs-tray`)
//! and sends `sessions` to the new build.
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(not(feature = "live"))]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::identity::Platform;

#[path = "instance_guard.rs"]
mod canonical;

/// The line a start sends to the running instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    Show,
    Sessions,
    /// Public UI navigation only; never carries a callback token or permission.
    #[cfg(all(feature = "live", target_os = "linux"))]
    Navigation(zerus_client_core::desktop::activation::Target),
}

impl Activation {
    pub fn line(&self) -> Vec<u8> {
        match self {
            Self::Show => b"show\n".to_vec(),
            Self::Sessions => b"sessions\n".to_vec(),
            #[cfg(all(feature = "live", target_os = "linux"))]
            Self::Navigation(target) => format!(
                "target {}\n",
                match zerus_client_core::desktop::activation::encode_navigation(target) {
                    Some(encoded) => encoded,
                    None => return Vec::new(),
                }
            )
            .into_bytes(),
        }
    }

    /// Qt reads one line and trims it (`main.cpp:428`): `QByteArray::trimmed`
    /// drops the six ASCII blanks, vertical tab included.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let blank = |byte: &u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r');
        let start = line
            .iter()
            .position(|byte| !blank(byte))
            .unwrap_or(line.len());
        let end = line
            .iter()
            .rposition(|byte| !blank(byte))
            .map_or(start, |last| last + 1);
        match &line[start..end] {
            b"sessions" => Some(Self::Sessions),
            b"show" => Some(Self::Show),
            _ => {
                #[cfg(all(feature = "live", target_os = "linux"))]
                {
                    std::str::from_utf8(&line[start..end])
                        .ok()
                        .and_then(|line| line.strip_prefix("target "))
                        .and_then(zerus_client_core::desktop::activation::decode_navigation)
                        .map(Self::Navigation)
                }
                #[cfg(not(all(feature = "live", target_os = "linux")))]
                {
                    None
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub socket: PathBuf,
    pub lock: PathBuf,
}

/// QLocalServer puts a relative name in `QDir::tempPath()`; the start lock is
/// `QStandardPaths::RuntimeLocation` + `/<name>-start.lock` (`SingleInstance.cpp:143-146`).
pub fn paths(name: &str, temp: &Path, runtime: &Path) -> Paths {
    Paths {
        socket: temp.join(name),
        lock: runtime.join(format!("{name}-start.lock")),
    }
}

/// `QDir::tempPath()`: a non-empty `TMPDIR`, else the per-user `confstr`
/// folder on macOS (`NSTemporaryDirectory`) and `/tmp` elsewhere. Qt cleans
/// the path only lexically and keeps symbolic links, so the socket is
/// `<TMPDIR as given>/<name>`, where Qt's own QLocalSocket connects; no
/// `/private` is added toward Darwin's 104-byte socket path limit.
pub fn temp_dir() -> PathBuf {
    temp_dir_from(std::env::var_os("TMPDIR"))
}

#[cfg(feature = "live")]
fn temp_dir_from(tmpdir: Option<OsString>) -> PathBuf {
    zerus_client_core::desktop::temporary_dir_from(tmpdir)
}

#[cfg(not(feature = "live"))]
fn temp_dir_from(tmpdir: Option<OsString>) -> PathBuf {
    let temp = tmpdir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(user_temp_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    clean_path(&temp)
}

/// `QDir::cleanPath`: no repeated or trailing separators, `.` and `..`
/// resolved by name only; symbolic links stay as they are.
#[cfg(not(feature = "live"))]
fn clean_path(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir) => {}
                _ => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

#[cfg(all(not(feature = "live"), target_os = "macos"))]
fn user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: the buffer is writable for its length.
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
    // SAFETY: confstr wrote a NUL-terminated string that fits the buffer.
    let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(text.to_bytes())))
}

#[cfg(all(not(feature = "live"), not(target_os = "macos")))]
fn user_temp_dir() -> Option<PathBuf> {
    None
}

/// `QStandardPaths::RuntimeLocation`: `~/Library/Application Support` on
/// macOS; on Linux a non-empty `XDG_RUNTIME_DIR`, else Qt's
/// `<temp>/runtime-<user>`.
pub fn runtime_dir(platform: Platform, home: &Path, xdg_runtime: Option<&Path>) -> PathBuf {
    match platform {
        Platform::Mac => home.join("Library/Application Support"),
        _ => xdg_runtime
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| temp_dir().join(format!("runtime-{}", user_name()))),
    }
}

/// This user's account name (`QFileSystemEngine::resolveUserName`), else the UID.
fn user_name() -> String {
    // SAFETY: geteuid cannot fail.
    let uid = unsafe { libc::geteuid() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: a zeroed passwd is a valid out-parameter; getpwuid_r fills it
    // with pointers into the buffer, which outlives their use below.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status == 0 && !found.is_null() && !entry.pw_name.is_null() {
        // SAFETY: pw_name points at a NUL-terminated string inside the buffer.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
        if !name.to_bytes().is_empty() {
            return name.to_string_lossy().into_owned();
        }
    }
    uid.to_string()
}

/// QLockFile's first three lines (`qlockfile_unix.cpp`): PID, process name, host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: i64,
    pub app: String,
    pub host: String,
}

impl LockInfo {
    pub fn contents(&self) -> String {
        format!("{}\n{}\n{}\n", self.pid, self.app, self.host)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.split('\n');
        let pid = lines
            .next()?
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|pid| *pid > 0)?;
        Some(Self {
            pid,
            app: lines.next().unwrap_or("").to_owned(),
            host: lines.next().unwrap_or("").to_owned(),
        })
    }

    /// This process, named as QLockFile names it.
    pub fn own() -> Self {
        let pid = i64::from(std::process::id());
        Self {
            pid,
            app: process_name(pid)
                .flatten()
                .unwrap_or_else(|| "zerus-client".to_owned()),
            host: host_name(),
        }
    }
}

/// QLockFile's default stale time.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// QLockFile's `isApparentlyStale`: on this host, a holder that has exited
/// or whose PID now runs another program; on any host, a file older than
/// 30 s. `process` answers None for no such process, `Some(None)` for a
/// process whose name is unknown and `Some(Some(name))` otherwise.
pub fn is_stale(
    info: Option<&LockInfo>,
    age: Duration,
    host: &str,
    process: impl Fn(i64) -> Option<Option<String>>,
) -> bool {
    if let Some(info) = info {
        if info.host.is_empty() || info.host == host {
            let running = match process(info.pid) {
                None => false,
                Some(Some(name)) if !name.is_empty() => Path::new(&info.app)
                    .file_name()
                    .is_some_and(|app| app == name.as_str()),
                Some(_) => true,
            };
            if !running {
                return true;
            }
        }
    }
    age > STALE_AFTER
}

/// None when no process has this PID; its name when the system tells it:
/// `proc_name` on macOS, the executable's file name on Linux, as QLockFile's
/// `processNameByPid` reads them. On Linux a process whose executable link
/// cannot be read counts as gone, as in Qt.
pub fn process_name(pid: i64) -> Option<Option<String>> {
    let raw = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(raw, 0) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return None;
    }
    running_name(raw)
}

#[cfg(target_os = "macos")]
fn running_name(pid: libc::pid_t) -> Option<Option<String>> {
    let mut buffer = [0u8; 1024];
    // SAFETY: the buffer is writable for its length.
    let length = unsafe { libc::proc_name(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    Some((length > 0).then(|| String::from_utf8_lossy(&buffer[..length as usize]).into_owned()))
}

#[cfg(not(target_os = "macos"))]
fn running_name(pid: libc::pid_t) -> Option<Option<String>> {
    exe_name(fs::read_link(format!("/proc/{pid}/exe")))
}

/// QLockFile's Linux `processNameByPid`: the file name of `/proc/<pid>/exe`
/// without the ` (deleted)` that marks a replaced binary. None when the
/// link cannot be read.
#[cfg(not(target_os = "macos"))]
fn exe_name(link: io::Result<PathBuf>) -> Option<Option<String>> {
    let link = link.ok()?;
    let text = link.to_string_lossy();
    let path = Path::new(text.strip_suffix(" (deleted)").unwrap_or(&text));
    Some(
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    )
}

/// `QSysInfo::machineHostName()`.
pub fn host_name() -> String {
    let mut buffer = [0u8; 512];
    // SAFETY: the buffer is writable for its length.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return String::new();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// A taken start lock; dropping it removes the file, as `QLockFile::unlock` does.
pub struct StartLock {
    path: PathBuf,
    _file: File,
}

impl Drop for StartLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub enum LockAttempt {
    Taken(StartLock),
    Busy,
    Unavailable,
}

/// The file's age; QLockFile counts a modification time in the future too.
fn age(path: &Path) -> Duration {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return Duration::ZERO;
    };
    let now = SystemTime::now();
    now.duration_since(modified)
        .or_else(|_| modified.duration_since(now))
        .unwrap_or_default()
}

fn stale_now(path: &Path, host: &str) -> bool {
    let info = fs::read_to_string(path)
        .ok()
        .and_then(|text| LockInfo::parse(&text));
    is_stale(info.as_ref(), age(path), host, process_name)
}

/// Takes QLockFile's advisory lock; false when a live holder keeps it.
fn flock_now(file: &File) -> bool {
    // SAFETY: an advisory lock on an open descriptor.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// QLockFile guards a removal with `<lock>.rmlock`, itself a lock file taken
/// by the same rules, and removes only a file that no live holder keeps locked.
fn remove_stale(path: &Path, own: &LockInfo) -> bool {
    let mut guard = OsString::from(path.as_os_str());
    guard.push(".rmlock");
    let LockAttempt::Taken(_guard) = try_lock(Path::new(&guard), own) else {
        return false;
    };
    stale_now(path, &own.host)
        && OpenOptions::new()
            .write(true)
            .open(path)
            .is_ok_and(|file| flock_now(&file))
        && fs::remove_file(path).is_ok()
}

/// One attempt to take the lock, removing a stale one first (`QLockFile::tryLock(0)`).
pub fn try_lock(path: &Path, own: &LockInfo) -> LockAttempt {
    for _ in 0..2 {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(path)
        {
            Ok(mut file) => {
                // Held while the file exists, so a remover can tell a live
                // holder from a crashed one (QLockFile's native lock).
                let _ = flock_now(&file);
                if file.write_all(own.contents().as_bytes()).is_err() {
                    let _ = fs::remove_file(path);
                    return LockAttempt::Unavailable;
                }
                let _ = file.sync_data();
                return LockAttempt::Taken(StartLock {
                    path: path.to_owned(),
                    _file: file,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !(stale_now(path, &own.host) && remove_stale(path, own)) {
                    return LockAttempt::Busy;
                }
            }
            Err(_) => return LockAttempt::Unavailable,
        }
    }
    LockAttempt::Busy
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceState {
    #[default]
    Absent,
    Starting,
    Running,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Service {
    pub state: ServiceState,
    pub pid: i64,
}

/// `launchctl print gui/<uid>/<label>`: only the job's own properties, one
/// level inside its braces (`SingleInstance.cpp:74-105`).
pub fn launchd_service(description: &str) -> Service {
    let (mut state, mut pid, mut depth) = ("", 0_i64, 0_i32);
    for raw in description.lines() {
        let line = raw.trim();
        if line == "}" {
            depth -= 1;
            continue;
        }
        if depth == 1 {
            if let Some((name, value)) = line.split_once(" = ") {
                match name {
                    "state" => state = value,
                    "pid" => pid = value.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        if line.ends_with('{') {
            depth += 1;
        }
    }
    let state = match state {
        "running" => ServiceState::Running,
        "spawn scheduled" => ServiceState::Starting,
        _ => return Service::default(),
    };
    Service {
        state,
        pid: pid.max(0),
    }
}

/// A start that finds no instance leaves the socket to a service another
/// process runs or is about to run (`SingleInstance::waitsForService`).
pub fn waits_for_service(service: &Service, own_pid: i64) -> bool {
    service.state != ServiceState::Absent && service.pid != own_pid
}

/// `launchctl print`, bounded at 2 s like `SingleInstance::currentService`.
fn launchd_now(label: &str) -> Service {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let Ok(child) = Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{label}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Service::default();
    };
    let pid = child.id();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(output)) if output.status.success() => {
            launchd_service(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(_) => Service::default(),
        Err(_) => {
            // SAFETY: the child is ours and still running.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            Service::default()
        }
    }
}

/// `SingleInstance::Startup`'s defaults.
pub struct Timing {
    pub lock_wait: Duration,
    pub service_wait: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            lock_wait: Duration::from_secs(10),
            service_wait: Duration::from_secs(30),
            poll: Duration::from_millis(200),
        }
    }
}

/// What `start` does to the world, so tests can script it.
pub trait Start {
    fn lock(&mut self, wait: Duration) -> Result<(), String>;
    fn unlock(&mut self);
    fn forward(&mut self, activation: Activation) -> bool;
    fn service(&mut self) -> Service;
    fn claim(&mut self) -> io::Result<()>;
    fn pause(&mut self, pause: Duration);
    fn elapsed(&self) -> Duration;
    fn report(&mut self, line: &str);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Forwarded,
    Claimed,
    Failed(String),
}

/// `SingleInstance::start` (`SingleInstance.cpp:148-191`): probing the
/// socket, removing a stale one and listening are one step under the lock; a
/// start that launchd does not run leaves the socket to the service while
/// it runs or is about to.
pub fn start(
    effects: &mut impl Start,
    activation: Activation,
    own_pid: i64,
    timing: &Timing,
) -> Outcome {
    let mut lockable = true;
    let mut wait_reported = false;
    loop {
        if lockable {
            if let Err(why) = effects.lock(timing.lock_wait) {
                lockable = false;
                effects.report(&why);
            }
        }
        if effects.forward(activation.clone()) {
            effects.unlock();
            return Outcome::Forwarded;
        }
        let service = effects.service();
        if waits_for_service(&service, own_pid) {
            if effects.elapsed() < timing.service_wait {
                if !wait_reported {
                    wait_reported = true;
                    effects.report(&if service.pid > 0 {
                        format!(
                            "waiting for the Zerus service (pid {}) to open its socket",
                            service.pid
                        )
                    } else {
                        "waiting for the Zerus service to start".to_owned()
                    });
                }
                effects.unlock();
                effects.pause(timing.poll);
                continue;
            }
            effects.report(&format!(
                "the Zerus service did not open its socket within {} s; starting here",
                timing.service_wait.as_secs_f64()
            ));
        }
        let outcome = match effects.claim() {
            Ok(()) => Outcome::Claimed,
            Err(error) => Outcome::Failed(error.to_string()),
        };
        effects.unlock();
        return outcome;
    }
}

/// Hands the activation to an instance that answers (`SingleInstance.cpp:22-31`).
pub fn forward(socket: &Path, activation: Activation) -> bool {
    let line = activation.line();
    if line.is_empty() {
        return false;
    }
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    stream.write_all(&line).is_ok()
}

struct UnixStart<'a> {
    paths: &'a Paths,
    own: &'a LockInfo,
    label: Option<&'a str>,
    lock: Option<StartLock>,
    started: Instant,
    listener: Option<UnixListener>,
}

impl Start for UnixStart<'_> {
    fn lock(&mut self, wait: Duration) -> Result<(), String> {
        if let Some(folder) = self.paths.lock.parent() {
            // Qt's runtime folder is the user's own (`QStandardPaths`).
            let _ = DirBuilder::new().recursive(true).mode(0o700).create(folder);
        }
        let deadline = Instant::now() + wait;
        loop {
            match try_lock(&self.paths.lock, self.own) {
                LockAttempt::Taken(lock) => {
                    self.lock = Some(lock);
                    return Ok(());
                }
                LockAttempt::Busy if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                LockAttempt::Busy => {
                    return Err(format!(
                        "another start keeps {} locked; starting without it",
                        self.paths.lock.display()
                    ))
                }
                LockAttempt::Unavailable => {
                    return Err(format!(
                        "cannot create {}; concurrent starts are not serialized",
                        self.paths.lock.display()
                    ))
                }
            }
        }
    }

    fn unlock(&mut self) {
        self.lock = None;
    }

    fn forward(&mut self, activation: Activation) -> bool {
        forward(&self.paths.socket, activation)
    }

    fn service(&mut self) -> Service {
        if cfg!(target_os = "macos") {
            self.label.map(launchd_now).unwrap_or_default()
        } else {
            Service::default()
        }
    }

    fn claim(&mut self) -> io::Result<()> {
        // Nothing answers: the socket, if any, is a crashed instance's.
        match fs::remove_file(&self.paths.socket) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = UnixListener::bind(&self.paths.socket)?;
        fs::set_permissions(&self.paths.socket, Permissions::from_mode(0o600))?;
        self.listener = Some(listener);
        Ok(())
    }

    fn pause(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn report(&mut self, line: &str) {
        eprintln!("zerus-client: {line}");
    }
}

/// The claimed socket; dropping it removes the file it bound, as
/// `QLocalServer::close` does, unless another instance replaced it.
pub struct Listening {
    socket: PathBuf,
    inode: u64,
    // Stop both listeners before releasing lifetime exclusion.
    _servers: Vec<Server>,
    _canonical: Option<canonical::Guard>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        if fs::metadata(&self.socket).is_ok_and(|metadata| metadata.ino() == self.inode) {
            let _ = fs::remove_file(&self.socket);
        }
    }
}

pub struct Claimed {
    pub listening: Listening,
    /// Lines from later starts, queued until the app takes them.
    pub activations: Receiver<Activation>,
}

pub enum Claim {
    Claimed(Claimed),
    Forwarded,
    Failed(String),
}

/// The claimed socket, kept in the app state until the app exits.
pub struct InstanceState(pub Mutex<Option<Listening>>);

impl InstanceState {
    /// The exit hook's last step: the socket goes, unless another instance
    /// replaced it. Tauri never drops its managed state, so this is the only
    /// place the file is removed.
    pub fn release(&self) {
        if let Some(mut listening) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() {
            if let Some(guard) = listening._canonical.take() {
                guard.retain_until_exit();
            }
            drop(listening);
        }
    }
}

/// Takes the single-instance socket named `name`, or hands `activation` to
/// the instance that holds it.
pub type Reply = std::sync::Arc<dyn Fn(&UnixStream, &[u8]) -> Option<Vec<u8>> + Send + Sync>;

pub fn claim(
    name: &str,
    label: Option<&str>,
    activation: Activation,
    reply: Option<Reply>,
    admitted: impl FnOnce(),
) -> Claim {
    claim_in(
        name,
        label,
        activation,
        reply,
        &canonical::directory(),
        admitted,
    )
}

fn claim_in(
    name: &str,
    label: Option<&str>,
    activation: Activation,
    reply: Option<Reply>,
    directory: &Path,
    admitted: impl FnOnce(),
) -> Claim {
    let guard = if name == "hgs-tray" {
        match canonical_claim(directory, activation.clone()) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => return Claim::Forwarded,
            Err(error) => return Claim::Failed(error.to_string()),
        }
    } else {
        None
    };
    let xdg_runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let runtime = runtime_dir(
        Platform::current(),
        &crate::identity::home_dir(),
        xdg_runtime.as_deref(),
    );
    claim_guarded(
        &paths(name, &temp_dir(), &runtime),
        label,
        activation,
        &LockInfo::own(),
        &Timing::default(),
        reply,
        (guard, admitted),
    )
}

fn canonical_claim(
    directory: &Path,
    activation: Activation,
) -> io::Result<Option<canonical::Guard>> {
    if let Some(guard) = canonical::acquire(directory)? {
        return Ok(Some(guard));
    }
    // A concurrent start may still be opening its listeners. Failure to focus
    // is a clean duplicate exit, never permission to create another GUI.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !forward(&directory.join("gui.sock"), activation.clone()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(None)
}

#[cfg(test)]
pub fn claim_at(
    paths: &Paths,
    label: Option<&str>,
    activation: Activation,
    own: &LockInfo,
    timing: &Timing,
) -> Claim {
    claim_with_reply(paths, label, activation, own, timing, None)
}

#[cfg(test)]
fn claim_with_reply(
    paths: &Paths,
    label: Option<&str>,
    activation: Activation,
    own: &LockInfo,
    timing: &Timing,
    reply: Option<Reply>,
) -> Claim {
    claim_guarded(paths, label, activation, own, timing, reply, (None, || {}))
}

fn claim_guarded(
    paths: &Paths,
    label: Option<&str>,
    activation: Activation,
    own: &LockInfo,
    timing: &Timing,
    reply: Option<Reply>,
    admission: (Option<canonical::Guard>, impl FnOnce()),
) -> Claim {
    let (mut guard, admitted) = admission;
    let mut effects = UnixStart {
        paths,
        own,
        label,
        lock: None,
        started: Instant::now(),
        listener: None,
    };
    match start(&mut effects, activation, own.pid, timing) {
        Outcome::Forwarded => Claim::Forwarded,
        Outcome::Failed(error) => Claim::Failed(error),
        Outcome::Claimed => {
            // Renderer setup may write a crash marker and change environment.
            // Only an admitted GUI does it, before listener/GTK threads exist.
            admitted();
            let Some(listener) = effects.listener.take() else {
                return Claim::Failed("the socket was not kept".to_owned());
            };
            let inode = fs::metadata(&paths.socket)
                .map(|metadata| metadata.ino())
                .unwrap_or(0);
            let (sender, activations) = mpsc::sync_channel(32);
            let mut servers = Vec::new();
            if let Some(guard) = &mut guard {
                let listener = match guard.bind() {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = fs::remove_file(&paths.socket);
                        return Claim::Failed(error.to_string());
                    }
                };
                let sender = sender.clone();
                // The canonical endpoint carries public navigation only.
                // Configuration-bound desktop Hello remains on Qt's socket.
                servers.push(listen(
                    listener,
                    move |activation| {
                        let _ = sender.try_send(activation);
                    },
                    None,
                ));
            }
            servers.push(listen(
                listener,
                move |activation| {
                    let _ = sender.try_send(activation);
                },
                reply,
            ));
            Claim::Claimed(Claimed {
                listening: Listening {
                    socket: paths.socket.clone(),
                    inode,
                    _servers: servers,
                    _canonical: guard,
                },
                activations,
            })
        }
    }
}

/// How long one connection may take to send its line.
const LINE_WAIT: Duration = Duration::from_secs(1);

struct Server {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = self.wake.shutdown(std::net::Shutdown::Write);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reads the first line of each connection (`main.cpp:424-433`). Qt acts only
/// on a complete line (`canReadLine`); a connection gets at most 1 s in all
/// and 64 bytes, so a silent or slow client cannot hold the next start back.
fn listen(
    listener: UnixListener,
    deliver: impl Fn(Activation) + Send + 'static,
    reply: Option<Reply>,
) -> Server {
    listener
        .set_nonblocking(true)
        .expect("instance listener is nonblocking");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let (wake, awakened) = UnixStream::pair().expect("instance wake socket");
    let thread = std::thread::Builder::new()
        .name("zerus-client-instance".into())
        .spawn(move || {
            while !stopped.load(std::sync::atomic::Ordering::Acquire) {
                // Sleep in the kernel until a launch arrives or Drop wakes
                // shutdown. Two GUI endpoints must not add idle polling.
                let mut ready = [
                    libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: awakened.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: both descriptors and the writable array outlive poll.
                if unsafe { libc::poll(ready.as_mut_ptr(), ready.len() as libc::nfds_t, -1) } < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                if ready[1].revents != 0 || stopped.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(_) => return,
                };
                // BSD may inherit the listener file flags. Keep accepted I/O
                // bounded by the explicit deadlines, as on Linux.
                let _ = stream.set_nonblocking(false);
                let deadline = Instant::now() + LINE_WAIT;
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                // macOS refuses a timeout once the peer has closed, as a second
                // start does right after its line. Its bytes are buffered then:
                // read them without blocking, so no read can wait unbounded.
                if stream.set_read_timeout(Some(LINE_WAIT)).is_err() {
                    let _ = stream.set_nonblocking(true);
                }
                let complete = loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    #[cfg(all(feature = "live", target_os = "linux"))]
                    let cap = zerus_client_core::desktop::activation::NAVIGATION_BYTES + 8;
                    #[cfg(not(all(feature = "live", target_os = "linux")))]
                    let cap = 64;
                    if line.len() >= cap || left.is_zero() {
                        break false;
                    }
                    // macOS refuses the option once the peer has closed; the
                    // remaining bytes then read without waiting.
                    let _ = stream.set_read_timeout(Some(left));
                    match stream.read(&mut byte) {
                        Ok(1) if byte[0] == b'\n' => break true,
                        Ok(1) => line.push(byte[0]),
                        _ => break false,
                    }
                };
                if let Some(activation) = complete.then(|| Activation::parse(&line)).flatten() {
                    deliver(activation);
                } else if complete {
                    if let Some(bytes) = reply.as_ref().and_then(|reply| reply(&stream, &line)) {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if !left.is_zero()
                            && bytes.len() <= 4096
                            && stream.set_write_timeout(Some(left)).is_ok()
                        {
                            let _ = stream.write_all(&bytes);
                        }
                    }
                }
            }
        })
        .expect("the instance thread starts");
    Server {
        stop,
        wake,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    mod canonical_tests {
        use super::*;
        include!("instance_tests.rs");
    }

    /// A test's own folder, removed when the test ends (also on a panic).
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("zci-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    #[test]
    fn a_tests_folder_goes_with_it() {
        let dir = scratch("drop");
        fs::write(dir.join("socket-like"), b"").unwrap();
        let path = dir.to_path_buf();
        drop(dir);
        assert!(!path.exists(), "scratch folders are removed");
    }

    fn fast() -> Timing {
        Timing {
            lock_wait: Duration::from_millis(300),
            service_wait: Duration::from_millis(400),
            poll: Duration::from_millis(20),
        }
    }

    #[cfg(all(feature = "live", target_os = "linux"))]
    #[test]
    fn navigation_instance_wire_is_target_only_and_retains_exact_identity() {
        let value = Activation::Navigation(zerus_client_core::desktop::activation::Target {
            computer: "peer.example".into(),
            run: "original-run".into(),
            conversation: "original-conversation".into(),
        });
        assert_eq!(Activation::parse(&value.line()), Some(value));
        assert!(Activation::parse(br#"target {"computer":"peer","run":"r","conversation":"c","activation_token":"injected"}"#).is_none());
        let dir = scratch("navigation");
        let paths = paths("navigation", &dir, &dir);
        let Claim::Claimed(first) =
            claim_at(&paths, None, Activation::Show, &LockInfo::own(), &fast())
        else {
            panic!("first claims");
        };
        let target = zerus_client_core::desktop::activation::Target {
            computer: "peer.example".into(),
            run: "r".repeat(1024),
            conversation: "c".repeat(1024),
        };
        let value = Activation::Navigation(target);
        assert!(forward(&paths.socket, value.clone()));
        assert_eq!(
            first
                .activations
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            value
        );
    }

    #[test]
    fn activation_lines_are_qt_lines() {
        assert_eq!(Activation::Sessions.line(), b"sessions\n");
        assert_eq!(Activation::Show.line(), b"show\n");
        assert_eq!(
            Activation::parse(b" sessions\r"),
            Some(Activation::Sessions)
        );
        assert_eq!(Activation::parse(b"show"), Some(Activation::Show));
        assert_eq!(Activation::parse(b"quit"), None);
        assert_eq!(
            Activation::parse(b"\x0bsessions\x0c"),
            Some(Activation::Sessions),
            "QByteArray::trimmed"
        );
        assert_eq!(Activation::parse(b"session"), None);
        assert_eq!(Activation::parse(b""), None);
    }

    #[test]
    fn paths_use_qt_names() {
        let p = paths(
            "hgs-tray",
            Path::new("/private/var/folders/x/T"),
            Path::new("/Users/example/Library/Application Support"),
        );
        assert_eq!(p.socket, Path::new("/private/var/folders/x/T/hgs-tray"));
        assert_eq!(
            p.lock,
            Path::new("/Users/example/Library/Application Support/hgs-tray-start.lock")
        );
        assert_eq!(
            runtime_dir(Platform::Mac, Path::new("/Users/example"), None),
            Path::new("/Users/example/Library/Application Support")
        );
        assert_eq!(
            runtime_dir(
                Platform::Linux,
                Path::new("/home/example"),
                Some(Path::new("/run/user/1000"))
            ),
            Path::new("/run/user/1000")
        );
        // Qt's fallback without XDG_RUNTIME_DIR.
        let fallback = temp_dir().join(format!("runtime-{}", user_name()));
        assert_eq!(
            runtime_dir(Platform::Linux, Path::new("/home/example"), None),
            fallback
        );
        assert_eq!(
            runtime_dir(
                Platform::Linux,
                Path::new("/home/example"),
                Some(Path::new(""))
            ),
            fallback
        );
    }

    #[test]
    fn the_socket_folder_is_qts_lexically_cleaned_temp_path() {
        let dir = scratch("temp");
        let real = dir.join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut given = link.clone().into_os_string();
        given.push("/");
        assert_eq!(
            temp_dir_from(Some(given)),
            link,
            "a symbolic link stays, the trailing slash goes"
        );
        assert_eq!(
            temp_dir_from(Some("/var/folders/x/T/".into())),
            Path::new("/var/folders/x/T"),
            "never /private/var"
        );
        assert_eq!(
            temp_dir_from(Some("/tmp//a/./b/../c/".into())),
            Path::new("/tmp/a/c")
        );
        assert_eq!(temp_dir_from(Some("/../tmp".into())), Path::new("/tmp"));
        assert_eq!(
            paths(
                "hgs-tray",
                &temp_dir_from(Some("/var/folders/x/T/".into())),
                Path::new("/r")
            )
            .socket,
            Path::new("/var/folders/x/T/hgs-tray")
        );
        assert_eq!(
            temp_dir_from(Some(OsString::new())),
            temp_dir_from(None),
            "an empty TMPDIR counts as unset"
        );
        assert!(temp_dir_from(None).is_absolute());
    }

    #[test]
    fn lock_staleness_follows_qlockfile() {
        let info = LockInfo {
            pid: 42,
            app: "hgs-tray".into(),
            host: "mac".into(),
        };
        let young = Duration::from_secs(1);
        assert!(
            is_stale(Some(&info), young, "mac", |_| None),
            "the holder exited"
        );
        assert!(
            is_stale(Some(&info), young, "mac", |_| Some(Some("Safari".into()))),
            "its PID runs another program"
        );
        assert!(!is_stale(Some(&info), young, "mac", |_| Some(Some(
            "hgs-tray".into()
        ))));
        assert!(!is_stale(Some(&info), young, "mac", |_| Some(None)));
        assert!(
            !is_stale(Some(&info), young, "other", |_| None),
            "another host's holder is not judged by PID"
        );
        assert!(
            is_stale(Some(&info), Duration::from_secs(31), "mac", |_| Some(Some(
                "hgs-tray".into()
            ))),
            "QLockFile's 30 s"
        );
        assert!(is_stale(None, Duration::from_secs(31), "mac", |_| None));
        assert!(!is_stale(None, young, "mac", |_| None));
        assert_eq!(
            LockInfo::parse("42\nhgs-tray\nmac\nmachine-id\nboot-id\n"),
            Some(info.clone())
        );
        assert_eq!(LockInfo::parse(&info.contents()), Some(info));
        assert_eq!(LockInfo::parse("x\n"), None);
    }

    #[test]
    fn launchd_print_reads_only_the_jobs_own_state() {
        let text = |state: &str, pid: &str| {
            let state = if state.is_empty() {
                String::new()
            } else {
                format!("\tstate = {state}\n")
            };
            let pid = if pid.is_empty() {
                String::new()
            } else {
                format!("\tpid = {pid}\n")
            };
            format!(
                "gui/501/com.hgdev.hgs-tray = {{\n\tactive count = 1\n\tpath = /Users/example/Library/LaunchAgents/com.hgdev.hgs-tray.plist\n\ttype = LaunchAgent\n{state}\n\tprogram = /Applications/Zerus.app/Contents/MacOS/hgs-tray\n\tevent triggers = {{\n\t\tcom.example.trigger => {{\n\t\t\tstate = running\n\t\t\tpid = 7\n\t\t}}\n\t}}\n{pid}\tlast exit code = (never exited)\n}}\n"
            )
        };
        assert_eq!(
            launchd_service(&text("running", "4242")),
            Service {
                state: ServiceState::Running,
                pid: 4242
            }
        );
        assert_eq!(
            launchd_service(&text("spawn scheduled", "")),
            Service {
                state: ServiceState::Starting,
                pid: 0
            }
        );
        assert_eq!(
            launchd_service(&text("not running", "")),
            Service::default()
        );
        assert_eq!(launchd_service(&text("", "")), Service::default());
        assert_eq!(launchd_service(""), Service::default());
    }

    #[test]
    fn only_another_service_process_keeps_the_socket() {
        assert!(!waits_for_service(&Service::default(), 100));
        assert!(!waits_for_service(
            &Service {
                state: ServiceState::Running,
                pid: 100
            },
            100
        ));
        assert!(waits_for_service(
            &Service {
                state: ServiceState::Running,
                pid: 200
            },
            100
        ));
        assert!(waits_for_service(
            &Service {
                state: ServiceState::Starting,
                pid: 0
            },
            100
        ));
        assert!(waits_for_service(
            &Service {
                state: ServiceState::Starting,
                pid: 200
            },
            100
        ));
    }

    /// Scripted effects for `start`.
    #[derive(Default)]
    struct Script {
        lock_error: Option<String>,
        forwards: VecDeque<bool>,
        services: VecDeque<Service>,
        claim_error: Option<String>,
        elapsed: Duration,
        locked: bool,
        log: Vec<String>,
    }

    impl Start for Script {
        fn lock(&mut self, _wait: Duration) -> Result<(), String> {
            self.log.push("lock".into());
            match &self.lock_error {
                Some(error) => Err(error.clone()),
                None => {
                    self.locked = true;
                    Ok(())
                }
            }
        }
        fn unlock(&mut self) {
            if self.locked {
                self.log.push("unlock".into());
            }
            self.locked = false;
        }
        fn forward(&mut self, activation: Activation) -> bool {
            let reached = self.forwards.pop_front().unwrap_or(false);
            self.log.push(format!("forward {activation:?} {reached}"));
            reached
        }
        fn service(&mut self) -> Service {
            self.services.pop_front().unwrap_or_default()
        }
        fn claim(&mut self) -> io::Result<()> {
            self.log.push("claim".into());
            match &self.claim_error {
                Some(error) => Err(io::Error::other(error.clone())),
                None => Ok(()),
            }
        }
        fn pause(&mut self, pause: Duration) {
            self.elapsed += pause;
        }
        fn elapsed(&self) -> Duration {
            self.elapsed
        }
        fn report(&mut self, line: &str) {
            self.log.push(format!("report {line}"));
        }
    }

    #[test]
    fn a_start_waits_for_the_starting_service_then_forwards() {
        let mut script = Script {
            forwards: [false, false, true].into(),
            services: [
                Service {
                    state: ServiceState::Starting,
                    pid: 0,
                },
                Service {
                    state: ServiceState::Running,
                    pid: 500,
                },
            ]
            .into(),
            ..Script::default()
        };
        assert_eq!(
            start(&mut script, Activation::Sessions, 100, &fast()),
            Outcome::Forwarded
        );
        assert!(
            !script.log.iter().any(|line| line == "claim"),
            "{:?}",
            script.log
        );
        assert_eq!(
            script
                .log
                .iter()
                .filter(|line| line.starts_with("report waiting"))
                .count(),
            1,
            "reported once"
        );
        assert!(!script.locked, "the lock is released");
        // Each pause leaves the lock to other starts, as Qt's `lock.unlock()` does.
        let locking: Vec<&str> = script
            .log
            .iter()
            .map(String::as_str)
            .filter(|line| *line == "lock" || *line == "unlock")
            .collect();
        assert_eq!(
            locking,
            ["lock", "unlock", "lock", "unlock", "lock", "unlock"]
        );
    }

    #[test]
    fn the_services_own_process_claims_at_once() {
        let mut script = Script {
            services: [Service {
                state: ServiceState::Running,
                pid: 100,
            }]
            .into(),
            ..Script::default()
        };
        assert_eq!(
            start(&mut script, Activation::Show, 100, &fast()),
            Outcome::Claimed
        );
        assert_eq!(
            script.log,
            ["lock", "forward Show false", "claim", "unlock"]
        );
    }

    #[test]
    fn a_service_that_never_listens_is_given_up_after_the_wait() {
        let mut script = Script {
            services: std::iter::repeat_n(
                Service {
                    state: ServiceState::Running,
                    pid: 500,
                },
                100,
            )
            .collect(),
            ..Script::default()
        };
        assert_eq!(
            start(&mut script, Activation::Show, 100, &fast()),
            Outcome::Claimed
        );
        assert!(
            script.log.iter().any(|line| line
                == "report the Zerus service did not open its socket within 0.4 s; starting here"),
            "{:?}",
            script.log
        );
    }

    #[test]
    fn a_lock_that_cannot_be_taken_is_reported_once_and_skipped() {
        let mut script = Script {
            lock_error: Some("another start keeps x locked; starting without it".into()),
            services: [Service {
                state: ServiceState::Starting,
                pid: 0,
            }]
            .into(),
            forwards: [false, true].into(),
            ..Script::default()
        };
        assert_eq!(
            start(&mut script, Activation::Show, 100, &fast()),
            Outcome::Forwarded
        );
        assert_eq!(script.log.iter().filter(|line| *line == "lock").count(), 1);
    }

    #[test]
    fn a_failed_listen_is_reported() {
        let mut script = Script {
            claim_error: Some("address in use".into()),
            ..Script::default()
        };
        assert_eq!(
            start(&mut script, Activation::Show, 100, &fast()),
            Outcome::Failed("address in use".into())
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn delegation_challenge_leaves_show_and_sessions_on_the_original_socket() {
        use zerus_client_core::desktop::{self, Build, Observation, Responder};
        let dir = scratch("delegation");
        let paths = paths("zerus-test", &dir, &dir);
        let responder = Responder::new(
            "com.hgdev.hgs-tray".into(),
            Build {
                version: "1.2.3".into(),
                source_commit: "b".repeat(40),
                source_dirty: Some(false),
            },
            "22222222-2222-4222-8222-222222222222".into(),
            true,
            false,
        );
        assert!(responder.configure_once("a".repeat(64)));
        let reply: Reply = std::sync::Arc::new(move |stream, line| {
            desktop::same_user(stream)
                .then(|| responder.reply(line))
                .flatten()
        });
        let Claim::Claimed(first) = claim_with_reply(
            &paths,
            None,
            Activation::Show,
            &LockInfo::own(),
            &fast(),
            Some(reply),
        ) else {
            panic!("first claims")
        };
        assert!(matches!(
            desktop::probe_at(
                &paths.socket,
                "11111111-1111-4111-8111-111111111111",
                &"a".repeat(64)
            ),
            Observation::LiveNext { .. }
        ));
        assert!(
            first.activations.try_recv().is_err(),
            "a query never activates the window"
        );
        for activation in [Activation::Show, Activation::Sessions] {
            assert!(matches!(
                claim_at(&paths, None, activation.clone(), &LockInfo::own(), &fast()),
                Claim::Forwarded
            ));
            assert_eq!(
                first
                    .activations
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap(),
                activation
            );
        }
        assert!(paths.socket.exists());
    }

    #[test]
    fn a_second_start_forwards_its_line_to_the_first() {
        let dir = scratch("forward");
        let paths = paths("zerus-test", &dir, &dir);
        let own = LockInfo::own();
        let Claim::Claimed(first) = claim_at(&paths, None, Activation::Show, &own, &fast()) else {
            panic!("the first start claims")
        };
        assert!(matches!(
            claim_at(&paths, None, Activation::Sessions, &own, &fast()),
            Claim::Forwarded
        ));
        assert_eq!(
            first
                .activations
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            Activation::Sessions
        );
        assert!(!paths.lock.exists(), "the start lock is released");
        assert_eq!(
            fs::metadata(&paths.socket).unwrap().permissions().mode() & 0o777,
            0o600,
            "only this user may connect"
        );
        // The exit hook's path: the app state hands the socket back.
        let state = InstanceState(Mutex::new(Some(first.listening)));
        state.release();
        assert!(!paths.socket.exists(), "quitting removes the socket");
        state.release();
    }

    #[test]
    fn quitting_keeps_a_socket_another_instance_put_there() {
        let dir = scratch("replaced");
        let paths = paths("zerus-test", &dir, &dir);
        let Claim::Claimed(first) =
            claim_at(&paths, None, Activation::Show, &LockInfo::own(), &fast())
        else {
            panic!("the first start claims")
        };
        fs::remove_file(&paths.socket).unwrap();
        let _replacement = UnixListener::bind(&paths.socket).unwrap();
        InstanceState(Mutex::new(Some(first.listening))).release();
        assert!(paths.socket.exists(), "the replacement's socket stays");
    }

    #[test]
    fn a_crashed_instance_socket_is_claimed() {
        let dir = scratch("crashed");
        let paths = paths("zerus-test", &dir, &dir);
        {
            // A child forked meanwhile would keep the listener open until it execs.
            let _tree = crate::PROCESS_TREE
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(UnixListener::bind(&paths.socket).unwrap());
        }
        assert!(paths.socket.exists(), "a socket file nobody listens on");
        let Claim::Claimed(claimed) =
            claim_at(&paths, None, Activation::Show, &LockInfo::own(), &fast())
        else {
            panic!("a crashed instance's socket is replaced")
        };
        assert!(forward(&paths.socket, Activation::Sessions));
        assert_eq!(
            claimed
                .activations
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            Activation::Sessions
        );
    }

    #[test]
    fn a_dead_holders_lock_is_taken() {
        let dir = scratch("dead");
        let lock = dir.join("zerus-test-start.lock");
        let own = LockInfo::own();
        fs::write(
            &lock,
            LockInfo {
                pid: exited_pid(),
                app: own.app.clone(),
                host: own.host.clone(),
            }
            .contents(),
        )
        .unwrap();
        assert!(matches!(try_lock(&lock, &own), LockAttempt::Taken(_)));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_linux_holder_is_named_as_qlockfile_names_it() {
        let info = LockInfo {
            pid: 42,
            app: "hgs-tray".into(),
            host: "arch".into(),
        };
        let young = Duration::from_secs(1);
        let replaced = |_| exe_name(Ok("/opt/zerus/hgs-tray (deleted)".into()));
        assert_eq!(replaced(0), Some(Some("hgs-tray".to_owned())));
        assert!(
            !is_stale(Some(&info), young, "arch", replaced),
            "a holder whose binary was replaced still runs"
        );
        let unreadable = |_| exe_name(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(
            is_stale(Some(&info), young, "arch", unreadable),
            "an unreadable executable link counts as gone"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_linux_holder_with_a_deleted_binary_runs_and_an_unreadable_one_is_gone() {
        let dir = scratch("deleted");
        let holder = dir.join("zci-holder");
        let mut child = {
            // No other test forks while the copy is open for writing (ETXTBSY).
            let _tree = crate::PROCESS_TREE
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            fs::copy("/bin/sleep", &holder).unwrap();
            Command::new(&holder)
                .arg("30")
                .stdin(Stdio::null())
                .spawn()
                .unwrap()
        };
        fs::remove_file(&holder).unwrap();
        let pid = i64::from(child.id());
        let name = process_name(pid);
        let own = LockInfo::own();
        let info = LockInfo {
            pid,
            app: "zci-holder".into(),
            host: own.host.clone(),
        };
        let stale = is_stale(Some(&info), Duration::from_secs(1), &own.host, process_name);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            name,
            Some(Some("zci-holder".to_owned())),
            "the deleted binary keeps its name"
        );
        assert!(!stale, "its holder still runs");
        // SAFETY: geteuid cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                process_name(1),
                None,
                "init's executable link is unreadable here"
            );
        }
    }

    /// A PID whose process has exited and been reaped.
    fn exited_pid() -> i64 {
        let _tree = crate::PROCESS_TREE
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut child = Command::new("true").spawn().unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_dead_removers_guard_does_not_keep_a_stale_lock() {
        let dir = scratch("guard");
        let lock = dir.join("zerus-test-start.lock");
        let guard = dir.join("zerus-test-start.lock.rmlock");
        let own = LockInfo::own();
        let dead = LockInfo {
            pid: exited_pid(),
            app: own.app.clone(),
            host: own.host.clone(),
        }
        .contents();
        fs::write(&lock, &dead).unwrap();
        fs::write(&guard, &dead).unwrap();
        assert!(
            matches!(try_lock(&lock, &own), LockAttempt::Taken(_)),
            "QLockFile judges its .rmlock like any lock file"
        );
        assert!(!guard.exists() && !lock.exists());
    }

    #[test]
    fn a_lock_older_or_newer_than_30_s_is_stale() {
        let dir = scratch("age");
        let lock = dir.join("zerus-test-start.lock");
        let own = LockInfo::own();
        fs::write(&lock, own.contents()).unwrap();
        assert!(!stale_now(&lock, &own.host), "this process holds it");
        let file = File::options().write(true).open(&lock).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(40))
            .unwrap();
        assert!(stale_now(&lock, &own.host), "QLockFile's 30 s");
        file.set_modified(SystemTime::now() + Duration::from_secs(40))
            .unwrap();
        assert!(
            stale_now(&lock, &own.host),
            "QLockFile compares the absolute age"
        );
    }

    #[test]
    fn only_a_complete_line_activates() {
        let dir = scratch("line");
        let paths = paths("zerus-test", &dir, &dir);
        let Claim::Claimed(claimed) =
            claim_at(&paths, None, Activation::Show, &LockInfo::own(), &fast())
        else {
            panic!("the first start claims")
        };
        let mut unfinished = UnixStream::connect(&paths.socket).unwrap();
        unfinished.write_all(b"sessions").unwrap();
        drop(unfinished);
        assert!(forward(&paths.socket, Activation::Show));
        assert_eq!(
            claimed
                .activations
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            Activation::Show,
            "Qt acts on a line only once canReadLine"
        );
    }

    #[test]
    fn a_slow_client_holds_the_next_start_back_at_most_a_second() {
        let dir = scratch("drip");
        let paths = paths("zerus-test", &dir, &dir);
        let Claim::Claimed(claimed) =
            claim_at(&paths, None, Activation::Show, &LockInfo::own(), &fast())
        else {
            panic!("the first start claims")
        };
        let mut slow = UnixStream::connect(&paths.socket).unwrap();
        let started = Instant::now();
        // 1.8 s for the whole line, each byte well within a 1 s read timeout.
        let dripping = std::thread::spawn(move || {
            for byte in b"sessions\n" {
                if slow.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        assert!(forward(&paths.socket, Activation::Show));
        let first = claimed
            .activations
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let waited = started.elapsed();
        dripping.join().unwrap();
        assert_eq!(
            first,
            Activation::Show,
            "the slow line is dropped at its deadline"
        );
        assert!(
            waited < Duration::from_millis(1700),
            "the next start waited {waited:?}"
        );
    }

    #[test]
    fn a_live_holders_lock_is_busy_until_released() {
        let dir = scratch("live");
        let lock = dir.join("zerus-test-start.lock");
        let own = LockInfo::own();
        let LockAttempt::Taken(held) = try_lock(&lock, &own) else {
            panic!("the first attempt takes it")
        };
        assert!(matches!(try_lock(&lock, &own), LockAttempt::Busy));
        drop(held);
        assert!(!lock.exists());
        assert!(matches!(try_lock(&lock, &own), LockAttempt::Taken(_)));
    }
}
