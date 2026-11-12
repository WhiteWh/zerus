//! Terminal, clipboard and graphical-session integration. External programs receive argv,
//! never interpolated shell commands (the SSH peer and tmux pane are explicit exceptions).
//!
//! Operating-system specific parts live in `unix` and `windows`. On Windows hgs is a
//! control console for remote machines until the user enables local sessions there
//! (`local_sessions`).
use crate::config::Config;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
};

mod shim;
// Read-only Windows inspection primitives. The guarded cleanup sampler is
// still closed until its object matching and owned-worker integration land.
#[cfg(any(windows, test))]
#[allow(dead_code)]
mod windows_membership;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod worktree_membership;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// Start a child of the desktop service (a remote watch, a probe, maintenance).
/// The service has no console on Windows, so its children start without a
/// console window there; on Unix they take their turn as other children do.
pub fn spawn_service_child(command: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        spawn_in_turn(command)
    }
    #[cfg(windows)]
    {
        spawn_windowless(command)
    }
}

/// Whether a console belongs to this process alone: it has a window and no other
/// process, such as the user's shell, is attached. Windows creates one for a
/// console program whose starter gave it none (a logon task, COM activation).
#[cfg(any(windows, test))]
pub fn private_console(window: bool, attached: u32) -> bool {
    window && attached == 1
}

/// A completed process roster may still contain protected processes whose
/// paths the OS refuses to disclose. Keep these separate from readable paths:
/// background cleanup keeps them; only the confirmed Terminate/Remove policy
/// may ignore actual permission denials. This data is never signal authority.
#[derive(Default)]
pub struct WorktreeProcesses {
    pub paths: Vec<(u32, PathBuf, String)>,
    pub unreadable: Vec<UnreadableWorktreeProcess>,
    /// Complete bounded native cwd/image/FD inspection, with only actual
    /// permission denials represented in `unreadable`. Never inferred from Err.
    pub membership_complete: bool,
}

#[derive(serde::Serialize)]
pub struct UnreadableWorktreeProcess {
    pub pid: u32,
    pub identity: String,
    pub command: String,
    pub cwd: bool,
    pub executable: bool,
    pub files: bool,
    /// Kernel-verified invocation ancestor, retained as diagnostic metadata.
    pub invocation_ancestor: bool,
}


impl From<Vec<(u32, PathBuf, String)>> for WorktreeProcesses {
    fn from(paths: Vec<(u32, PathBuf, String)>) -> Self {
        Self {
            paths,
            unreadable: Vec::new(),
            membership_complete: false,
        }
    }
}

/// What a local session command reports where `local_sessions()` is false: a
/// Windows computer whose local sessions are not enabled (spec D2). It exits with
/// code 2.
pub const LOCAL_SESSIONS_UNSUPPORTED: &str = "Local sessions are not enabled on this computer; \
     enable them on the Machines page or run hgs session-host install, or choose a remote machine.";

pub fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = output(Command::new(program).args(args).stderr(Stdio::null())).ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}
// Its callers (the tmux backend, the Unix clipboard helpers, the Linux recovery
// and voice service starts) are Unix-only.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn silent(program: &str, args: &[&str]) -> bool {
    status(
        Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map(|s| s.success())
    .unwrap_or(false)
}
/// The executable `program` names: itself when it is a path, otherwise the first
/// match on PATH (with each PATHEXT extension on Windows).
pub fn which(program: &str) -> Option<PathBuf> {
    which_in(program, &env::var_os("PATH").unwrap_or_default())
}
fn which_in(program: &str, path: &OsStr) -> Option<PathBuf> {
    if is_path(program) {
        return executable(program).then(|| PathBuf::from(program));
    }
    env::split_paths(path)
        .flat_map(|dir| candidates(&dir, program))
        .find(|path| executable(path))
}
pub fn available(program: &str) -> bool {
    which(program).is_some()
}

/// The OpenSSH client for every connection to a peer. `HGS_SSH` overrides it
/// (tests inject a fake client this way); see `default_ssh` for the default.
pub fn ssh_program() -> OsString {
    env::var_os("HGS_SSH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_ssh)
}
pub fn ssh() -> Command {
    Command::new(ssh_program())
}
/// An ssh client started with some standard streams piped to this process.
pub struct SshChild {
    pub child: Child,
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    pub stderr: Option<Box<dyn Read + Send>>,
    /// The client's job (spec D4; see `spawn`), held for its drop: what is left
    /// in it ends then, after the client has been used.
    #[cfg(windows)]
    pub(crate) _job: ChildJob,
}
/// The output of a successful ssh command, without its error text.
pub fn ssh_capture(args: &[String]) -> Option<String> {
    let remote = args.last()?;
    let out = ssh_output(crate::ssh_bytes::command(remote).ok()?.args(args).stderr(Stdio::null())).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Replace `path` with a fully written temporary file. Windows refuses the rename
/// while a scanner or reader briefly holds the destination; retry that for a
/// bounded time. Unix renames at once.
pub fn persist(
    mut temporary: tempfile::NamedTempFile,
    path: impl AsRef<Path>,
) -> Result<File, tempfile::PersistError> {
    let path = path.as_ref();
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match temporary.persist(path) {
            Ok(file) => return Ok(file),
            Err(error)
                if transient_replace_error(&error.error)
                    && std::time::Instant::now() < deadline =>
            {
                temporary = error.file;
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(200));
            }
            Err(error) => return Err(error),
        }
    }
}

/// An fs2 `try_lock_*` failure that only means another process holds the lock.
/// Windows reports ERROR_LOCK_VIOLATION, which is not `ErrorKind::WouldBlock`.
pub fn lock_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

/// An fs2 lock this process holds on a file, released explicitly when dropped.
/// Windows frees a closed handle's lock only when system resources allow, and a
/// forked child can keep a Unix descriptor open until exec, so closing the file
/// alone can leave the next process waiting.
pub struct FileLock {
    file: File,
    owner_pid: u32,
}

impl FileLock {
    pub fn exclusive(file: File) -> io::Result<Self> {
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self::held(file))
    }

    /// Takes ownership of a file whose lock this process has just acquired.
    pub fn held(file: File) -> Self {
        Self {
            file,
            owner_pid: std::process::id(),
        }
    }

    /// Preserve a caller's fallible normal release without retrying it in Drop.
    /// Error exits still use the acquisition-owner guard's ordinary Drop.
    pub(crate) fn release(mut self) -> io::Result<()> {
        let result = self.unlock_owner();
        self.owner_pid = 0;
        result
    }

    fn unlock_owner(&self) -> io::Result<()> {
        if self.owner_pid == std::process::id() {
            fs2::FileExt::unlock(&self.file)
        } else {
            Ok(())
        }
    }

    /// The identity of the exact file object this lock is held on.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn identity(&self) -> io::Result<FileIdentity> {
        FileIdentity::of(&self.file)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Rust 1.89+ File::unlock would shadow the trait method; name fs2 explicitly.
        // An inherited copy must not unlock its parent's still-live scope.
        let _ = self.unlock_owner();
    }
}

/// Whether a process ID belongs to a running process, as far as this user can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// No process has this ID.
    Gone,
    /// A process that this user may signal or open has this ID.
    Present,
    /// The ID cannot be checked here, for example because another user's
    /// process has it. Never treat it as gone.
    Unknown,
}

/// How a child ended, in Unix terms: its exit code, or the signal that ended it.
/// Windows has no signals; its crash statuses (NTSTATUS) are reported as the
/// signal of the same meaning, so a crash never looks like an ordinary failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    pub fn of(status: ExitStatus) -> Self {
        exit_info(status)
    }

    /// The shell's convention: the exit code, or 128 plus the signal.
    pub fn exit_code(self) -> i32 {
        self.code.unwrap_or_else(|| 128 + self.signal.unwrap_or(1))
    }
}

/// NTSTATUS values that end a Windows process the way a signal ends a Unix one,
/// with that signal's number: Ctrl+C, Ctrl+Break or a closed console, memory
/// faults, illegal instructions, arithmetic faults, breakpoints, fail-fast
/// aborts (which is how a Rust panic set to abort ends) and the exceptions that
/// C++ and .NET programs leave unhandled.
#[cfg_attr(unix, allow(dead_code))]
const CRASH_SIGNALS: [(u32, i32); 20] = [
    (0xC000_013A, 2),  // STATUS_CONTROL_C_EXIT: SIGINT
    (0xC000_0005, 11), // STATUS_ACCESS_VIOLATION: SIGSEGV
    (0xC000_0006, 11), // STATUS_IN_PAGE_ERROR
    (0xC000_00FD, 11), // STATUS_STACK_OVERFLOW
    (0xC000_008C, 11), // STATUS_ARRAY_BOUNDS_EXCEEDED
    (0xC000_001D, 4),  // STATUS_ILLEGAL_INSTRUCTION: SIGILL
    (0xC000_0096, 4),  // STATUS_PRIVILEGED_INSTRUCTION
    (0xC000_008D, 8),  // STATUS_FLOAT_DENORMAL_OPERAND: SIGFPE
    (0xC000_008E, 8),  // STATUS_FLOAT_DIVIDE_BY_ZERO
    (0xC000_008F, 8),  // STATUS_FLOAT_INEXACT_RESULT
    (0xC000_0090, 8),  // STATUS_FLOAT_INVALID_OPERATION
    (0xC000_0091, 8),  // STATUS_FLOAT_OVERFLOW
    (0xC000_0092, 8),  // STATUS_FLOAT_STACK_CHECK
    (0xC000_0093, 8),  // STATUS_FLOAT_UNDERFLOW
    (0xC000_0094, 8),  // STATUS_INTEGER_DIVIDE_BY_ZERO
    (0xC000_0095, 8),  // STATUS_INTEGER_OVERFLOW
    (0x8000_0003, 5),  // STATUS_BREAKPOINT: SIGTRAP
    (0xC000_0409, 6),  // STATUS_STACK_BUFFER_OVERRUN (fail fast): SIGABRT
    (0xE06D_7363, 6),  // an unhandled C++ exception (MSVC)
    (0xE043_4352, 6),  // an unhandled .NET exception
];

/// How a Windows process that exited with `status` ended, in Unix terms (spec
/// D9). A status of the table is its signal. Any other error status that
/// Windows defines (severity "error" with the customer bit clear: an unhandled
/// exception or a fatal system status such as a missing DLL) ends a process
/// abnormally too and reads as SIGABRT, so a crash never looks like an
/// ordinary failure: completion reasons and the `--run` resume fallback treat
/// it as a crash. Every other value stays an exit code, `exit(-1)` and other
/// negative codes included. Plain Rust, so its tests run on every platform.
#[cfg_attr(unix, allow(dead_code))]
pub(crate) fn ntstatus_exit(status: u32) -> ExitInfo {
    const SIGABRT: i32 = 6;
    let signal = CRASH_SIGNALS
        .iter()
        .find(|(crash, _)| *crash == status)
        .map(|&(_, signal)| signal)
        .or((status & 0xE000_0000 == 0xC000_0000).then_some(SIGABRT));
    match signal {
        Some(signal) => ExitInfo {
            code: None,
            signal: Some(signal),
        },
        None => ExitInfo {
            code: Some(status as i32),
            signal: None,
        },
    }
}

/// One row of the system process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent: u32,
    /// The process group; Windows has none.
    pub group: Option<u32>,
}

/// A process of this user, as the system reports it now. Other users' and
/// exited (zombie) processes have none.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessIdentity {
    pub parent: u32,
    /// The process group; Windows has none.
    pub group: Option<u32>,
    /// What tells this process from a later one with the same ID: start clock
    /// ticks on Linux, `seconds:microseconds` on macOS and the creation time in
    /// 100 ns ticks on Windows.
    pub birth: String,
    /// When it started, in Unix seconds.
    pub started: f64,
}

/// The executable and arguments of a process, read from the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessImage {
    pub executable: String,
    /// None when only the executable could be read.
    pub argv: Option<Vec<String>>,
}

/// Distinguishes a file from a different one later found at the same path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    /// Modification time in Unix seconds and the nanoseconds within that second.
    pub modified: i64,
    pub modified_nanos: i64,
}

/// What `verify_private` expects at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Private {
    Directory,
    /// A regular file with a single link.
    File,
}

/// Why a bounded child process gave no result.
#[derive(Debug)]
pub enum RunError {
    /// The child could not be started.
    Spawn(io::Error),
    /// Waiting for the child or reading its output failed.
    Io(io::Error),
    /// The child wrote more output than allowed.
    TooLarge,
    /// The deadline passed or the caller cancelled the run.
    Stopped,
    /// The child reported a failure (`lsof` outside Linux).
    #[cfg_attr(any(target_os = "linux", not(unix)), allow(dead_code))]
    Failed,
    /// This computer cannot answer the question.
    #[cfg_attr(unix, allow(dead_code))]
    // Windows retains the unsupported enumeration API for explicit capability
    // queries; fork readiness now uses known-path holder discovery instead.
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
}

pub fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_./:=@%+,~-".contains(&c))
    {
        word.into()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}
pub fn join<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|s| quote(s.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}
/// Quotes a word for an SSH peer's login shell, which may be fish. fish also
/// reads `\\` and `\'` inside single quotes, so every backslash stays outside
/// them; a word without one keeps the `quote` form.
pub fn remote_quote(word: &str) -> String {
    if !word.contains('\\') {
        return quote(word);
    }
    let mut quoted = String::from("'");
    for c in word.chars() {
        match c {
            '\'' => quoted.push_str(r"'\''"),
            '\\' => quoted.push_str(r"'\\'"),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}
pub fn remote_join<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|s| remote_quote(s.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}
/// Runs a POSIX script under `sh` on an SSH peer. The peer's login shell,
/// which may be fish, then parses only one quoted word.
pub fn remote_sh(script: &str) -> String {
    format!("sh -c {}", remote_quote(script))
}

fn konsole_args(method: &str, args: &[&str]) -> Option<Vec<String>> {
    let service = env::var("KONSOLE_DBUS_SERVICE")
        .ok()
        .filter(|s| !s.is_empty())?;
    let session = env::var("KONSOLE_DBUS_SESSION")
        .ok()
        .filter(|s| !s.is_empty())?;
    if !available("busctl") {
        return None;
    }
    let mut result = vec![
        "--user".into(),
        "call".into(),
        service,
        session,
        "org.kde.konsole.Session".into(),
        method.into(),
    ];
    result.extend(args.iter().map(|s| s.to_string()));
    Some(result)
}
fn konsole(method: &str, args: &[&str]) {
    if let Some(args) = konsole_args(method, args) {
        let _ = status(
            Command::new("busctl")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}
fn konsole_get(mode: &str) -> String {
    let Some(args) = konsole_args("tabTitleFormat", &["i", mode]) else {
        return String::new();
    };
    output(Command::new("busctl").args(args).stderr(Stdio::null()))
        .ok()
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.trim()
                .strip_prefix("s \"")
                .and_then(|s| s.strip_suffix('"'))
                .map(String::from)
        })
        .unwrap_or_default()
}
/// POSIX cksum, retained so existing host colours do not change on migration.
fn cksum(bytes: &[u8]) -> u32 {
    let feed = |mut crc: u32, byte: u8| {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
        crc
    };
    let mut crc = bytes.iter().fold(0, |crc, &byte| feed(crc, byte));
    let mut len = bytes.len();
    while len != 0 {
        crc = feed(crc, len as u8);
        len >>= 8;
    }
    !crc
}
pub fn tab_color(config: &Config, host: &str) -> String {
    for pair in config.tab_colors.split_whitespace() {
        if let Some((key, value)) = pair.split_once('=') {
            if key == host {
                return value.into();
            }
        }
    }
    if host == "local" {
        return String::new();
    }
    [
        "#f5a623", "#7aa2f7", "#00c9a7", "#e06c9f", "#bb9af7", "#9ece6a",
    ][(cksum(host.as_bytes()) % 6) as usize]
        .into()
}

pub struct TabGuard {
    active: bool,
    konsole: bool,
    formats: [String; 2],
}
impl TabGuard {
    pub fn begin(config: &Config, title: &str, color: &str) -> Self {
        let mut guard = Self {
            active: config.tab && io::stdout().is_terminal() && virtual_terminal(),
            konsole: false,
            formats: Default::default(),
        };
        if !guard.active {
            return guard;
        }
        guard.konsole = konsole_args("tabTitleFormat", &[]).is_some();
        if guard.konsole {
            guard.formats = [konsole_get("0"), konsole_get("1")];
            konsole("setTabTitleFormat", &["is", "0", title]);
            konsole("setTabTitleFormat", &["is", "1", title]);
            if !color.is_empty() {
                konsole("setTabColor", &["s", color]);
            }
        }
        print!("\x1b]30;{title}\x07\x1b]0;{title}\x07");
        let _ = io::stdout().flush();
        guard
    }
}
impl Drop for TabGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if self.konsole {
            konsole("setTabColor", &["s", ""]);
            for (i, value) in self.formats.iter().enumerate() {
                if !value.is_empty() {
                    konsole("setTabTitleFormat", &["is", &i.to_string(), value]);
                }
            }
        } else {
            print!("\x1b]30;%d : %n\x07\x1b]0;\x07");
            let _ = io::stdout().flush();
        }
    }
}
pub fn restore_terminal() {
    if io::stdout().is_terminal() && virtual_terminal() {
        print!("\x1b[r\x1b(B\x1b[m\x1b[?1l\x1b>\x1b[0 q\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1005l\x1b[?2004l\x1b[?1004l\x1b[>4m\x1b[?69l\x1b[?2031l\x1b[?1049l");
        let _ = io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_console_says_how_to_enable_local_sessions_in_one_line() {
        let message = LOCAL_SESSIONS_UNSUPPORTED;
        assert!(!message.contains('\n'));
        assert!(message.starts_with("Local sessions are not enabled on this computer; "));
        assert!(message.contains("the Machines page or run hgs session-host install"));
    }

    #[test]
    fn remote_commands_keep_posix_quoting_for_windows_paths() {
        assert_eq!(quote("~/.local/bin/hgs"), "~/.local/bin/hgs");
        assert_eq!(quote(r"C:\Users\a b"), r"'C:\Users\a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
        assert_eq!(
            join(&["inspect", "zerus://session?a=1&b=2", "--local"]),
            "inspect 'zerus://session?a=1&b=2' --local"
        );
    }

    #[test]
    fn remote_words_keep_backslashes_outside_quotes_for_fish() {
        assert_eq!(remote_quote("~/.local/bin/hgs"), "~/.local/bin/hgs");
        assert_eq!(remote_quote("it's"), quote("it's"));
        assert_eq!(remote_quote(r"a\b"), r"'a'\\'b'");
        assert_eq!(remote_quote(r"a\'b\"), r"'a'\\''\''b'\\''");
        assert_eq!(remote_join(&["search", r"C:\x y"]), r"search 'C:'\\'x y'");
        assert_eq!(remote_sh("if true; then :; fi"), "sh -c 'if true; then :; fi'");
    }

    #[test]
    fn windows_crash_statuses_read_as_signals_and_other_codes_stay_codes() {
        let exited = |code: i32| ExitInfo {
            code: Some(code),
            signal: None,
        };
        let signalled = |signal: i32| ExitInfo {
            code: None,
            signal: Some(signal),
        };
        assert_eq!(ntstatus_exit(0), exited(0));
        assert_eq!(ntstatus_exit(3), exited(3));
        // exit(-1) and other negative codes are ordinary failures.
        assert_eq!(ntstatus_exit(u32::MAX), exited(-1));
        assert_eq!(ntstatus_exit(0xFFFF_FFFE), exited(-2));
        // Ctrl+C, Ctrl+Break and a closed console (spec D11, S5).
        assert_eq!(ntstatus_exit(0xC000_013A), signalled(2));
        assert_eq!(ntstatus_exit(0xC000_013A).exit_code(), 130);
        assert_eq!(ntstatus_exit(0xC000_0005), signalled(11));
        assert_eq!(ntstatus_exit(0xC000_0409).exit_code(), 134);
        assert_eq!(ntstatus_exit(0xE06D_7363), signalled(6));
        // Other error statuses Windows defines: a missing DLL, a failed DLL
        // start, heap corruption.
        for status in [0xC000_0135, 0xC000_0142, 0xC000_0374, 0xC06D_007E] {
            assert_eq!(ntstatus_exit(status), signalled(6), "{status:#x}");
        }
        // Informational and warning statuses, and customer codes, stay codes.
        assert_eq!(ntstatus_exit(0x4001_0004), exited(0x4001_0004));
        assert_eq!(ntstatus_exit(0x8000_0005), exited(0x8000_0005_u32 as i32));
        assert_eq!(ntstatus_exit(0xE000_0001), exited(0xE000_0001_u32 as i32));
    }

    #[test]
    fn executables_are_found_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = "hgs-platform-fixture";
        let file = dir.path().join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        std::fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Other directories on PATH must not hide or invent the fixture.
        let path = env::join_paths([dir.path().join("missing"), dir.path().to_owned()]).unwrap();
        assert_eq!(which_in(name, &path).as_deref(), Some(file.as_path()));
        assert_eq!(which_in("hgs-platform-absent", &path), None);
        assert_eq!(which_in(name, OsStr::new("")), None);
        assert!(executable(&file));
        assert!(!executable(dir.path()));
        assert_eq!(
            which(&file.to_string_lossy()).as_deref(),
            Some(file.as_path())
        );
    }

    #[test]
    fn replacement_is_atomic_and_contention_is_recognized() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("data.json");
        std::fs::write(&target, b"old").unwrap();
        let mut temporary = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        temporary.write_all(b"new").unwrap();
        persist(temporary, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        sync_dir(dir.path()).unwrap();

        use fs2::FileExt;
        let lock = dir.path().join("data.lock");
        let holder = File::create(&lock).unwrap();
        holder.lock_exclusive().unwrap();
        let contender = File::open(&lock).unwrap();
        let error = contender.try_lock_exclusive().unwrap_err();
        assert!(lock_contended(&error), "{error:?}");
        assert!(!lock_contended(&io::Error::from(io::ErrorKind::NotFound)));
        drop(holder);

        // Dropping the guard releases the lock even while another handle to the
        // same open file stays alive, as in a child between fork and exec.
        let owner = File::create(&lock).unwrap();
        let still_open = owner.try_clone().unwrap();
        drop(FileLock::exclusive(owner).unwrap());
        contender.try_lock_exclusive().unwrap();
        drop(still_open);
    }
}

#[cfg(test)]
mod console_allocation_tests {
    #[test]
    fn only_a_console_no_other_process_uses_is_released() {
        // A logon task or COM start: Windows made a console for this process alone.
        assert!(super::private_console(true, 1));
        // Started from a terminal: the user's shell shares it, so it stays.
        assert!(!super::private_console(true, 2));
        // No console window, or no attached process count: nothing to release.
        assert!(!super::private_console(false, 1));
        assert!(!super::private_console(true, 0));
    }

    #[test]
    fn desktop_service_children_start_without_console_windows() {
        // A plain spawn from the console-less service would give every remote
        // watch, probe or maintenance child (and its ssh.exe) a window.
        for (name, source) in [
            ("fleet", include_str!("../desktop/fleet.rs")),
            ("maintenance", include_str!("../desktop/maintenance.rs")),
        ] {
            let body = source
                .split("\nfn spawn(command: &mut ")
                .nth(1)
                .and_then(|rest| rest.split("\n}\n").next())
                .expect(name);
            assert!(
                body.contains("platform::spawn_service_child(command)"),
                "{name}"
            );
            assert!(!body.contains(".spawn()"), "{name} spawns directly");
        }
        let shared = include_str!("mod.rs")
            .split("pub fn spawn_service_child(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("spawn_service_child");
        assert!(shared.contains("#[cfg(windows)]") && shared.contains("spawn_windowless(command)"));
        assert!(
            include_str!("windows.rs").contains("command.creation_flags(CREATE_NO_WINDOW).spawn()")
        );
    }

    #[test]
    fn the_manifest_gives_hgs_no_console_when_its_starter_has_none() {
        let manifest = include_str!("../hgs.manifest");
        let settings = manifest
            .split("<windowsSettings>")
            .nth(1)
            .and_then(|rest| rest.split("</windowsSettings>").next())
            .expect("windowsSettings");
        assert!(settings.contains(
            r#"<consoleAllocationPolicy xmlns="http://schemas.microsoft.com/SMI/2024/WindowsSettings">detached</consoleAllocationPolicy>"#
        ));
        // The settings it already had stay.
        assert!(
            settings.contains(">UTF-8</activeCodePage>")
                && settings.contains(">true</longPathAware>")
        );
    }
}

#[cfg(all(test, unix))]
pub(crate) mod residual_lock_tests;
