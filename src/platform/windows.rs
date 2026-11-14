//! Windows process, console and file primitives. A Windows build manages remote
//! machines through the system OpenSSH client and, once local sessions are
//! enabled, runs agent sessions in its session host (spec D2, D8).
use super::{
    capture, ExitInfo, FileIdentity, Presence, Private, ProcessIdentity, ProcessImage, ProcessRow,
    RunError,
};
use crate::{
    cli::{Error, Result},
    host::job::{Job, Limits},
};
use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Seek, SeekFrom},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER,
            ERROR_LOCK_VIOLATION, ERROR_MORE_DATA, ERROR_SHARING_VIOLATION, ERROR_SUCCESS,
            FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            GetAce, GetLengthSid, GetTokenInformation, IsWellKnownSid, TokenUser,
            WinBuiltinAdministratorsSid, WinCreatorOwnerRightsSid, WinLocalSystemSid,
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, GetFileType, BY_HANDLE_FILE_INFORMATION,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE,
            FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, FILE_WRITE_ATTRIBUTES, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, READ_CONTROL,
        },
        System::{
            Console::{
                FreeConsole, GenerateConsoleCtrlEvent, GetConsoleMode, GetConsoleProcessList,
                GetConsoleWindow, GetStdHandle, SetConsoleCtrlHandler, SetConsoleMode,
                SetStdHandle, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
                CTRL_SHUTDOWN_EVENT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE,
                STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First,
                Thread32Next, PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            Pipes::{
                CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
                PIPE_WAIT,
            },
            RestartManager::{
                RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
                RM_PROCESS_INFO,
            },
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, OpenThread,
                QueryFullProcessImageNameW, ResumeThread, TerminateProcess, WaitForSingleObject,
                CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP,
                CREATE_NO_WINDOW, CREATE_SUSPENDED, DETACHED_PROCESS, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                THREAD_SUSPEND_RESUME,
            },
        },
        UI::Shell::CommandLineToArgvW,
    },
};

/// The locale and the console code page stay as they are: Rust writes Unicode to
/// the console itself, and a changed code page would outlive hgs in the user's
/// shell. PATH gains the stable CLI's folder and the agent installers' folders
/// (`prepare_path`). Console children (ssh.exe, tar.exe) must not outlive hgs
/// when the GUI stops it, so this process joins a job that terminates its
/// remaining members when its last handle, held only here, closes.
///
/// That job allows breakaway (spec D4), so a session host started with
/// CREATE_BREAKAWAY_FROM_JOB outlives this process. Only detached starts use that
/// permission (the session host; `spawn_detached` for the resident OpenCode host,
/// which outlives hgs by design as on Unix): every other child starts in a
/// kill-on-close job of its own that forbids breakaway (`spawn` and the helpers
/// built on it), because Git for Windows' MSYS runtime breaks away whenever its
/// immediate job lets it. The session host (`__host serve`) and a `--run`
/// supervisor must not join this job: a host that starts in a job fails its
/// placement check, and a supervisor belongs to its session's job (D9).
pub fn prepare_environment() {
    prepare_path();
    static PREPARED: std::sync::Once = std::sync::Once::new();
    PREPARED.call_once(|| {
        // Best effort: an enclosing job that forbids nesting keeps the old behavior.
        let Ok(job) = Job::new(Limits::CLI) else {
            return;
        };
        // SAFETY: GetCurrentProcess returns the current-process pseudo-handle,
        // which is always valid and which BorrowedHandle allows.
        if job
            .assign(unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess()) })
            .is_ok()
        {
            // Held until this process exits; closing it then ends what is left.
            std::mem::forget(job);
        }
    });
}
/// `prepare_environment` for a `--run` supervisor: PATH only. The supervisor
/// belongs to its session's job, which the host created (spec D9); it joins no
/// job of its own.
pub fn prepare_supervisor() {
    prepare_path();
}
/// The Windows counterpart of the Unix PATH preparation (spec D8): the stable
/// CLI's folder `<home>\.local\bin`, where Claude Code's installer also puts
/// `claude.exe`, first; npm's global folder `%APPDATA%\npm`, where `npm install
/// -g` puts agents, and Kimi Code's `%USERPROFILE%\.kimi-code\bin` last, as on
/// Unix (`kimi_folder`); each when it exists and PATH does not list it yet. So
/// an agent that `hgs account install` just installed is found before Windows
/// gives this process the PATH its installer changed. Agents are looked up on
/// this PATH, and a session's environment is this process's.
fn prepare_path() {
    static PREPARED: std::sync::Once = std::sync::Once::new();
    PREPARED.call_once(|| {
        let first = home()
            .map(|home| PathBuf::from(below(&home, ".local/bin")))
            .filter(|folder| folder.is_dir());
        let npm = env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(|folder| PathBuf::from(folder).join("npm"));
        let kimi = kimi_folder(env::var("USERPROFILE").ok());
        let last: Vec<PathBuf> = [npm, kimi]
            .into_iter()
            .flatten()
            .filter(|folder| folder.is_dir())
            .collect();
        let path = env::var_os("PATH").unwrap_or_default();
        if let Some(path) = prepared_path(&path, first, &last) {
            env::set_var("PATH", path);
        }
    });
}
/// The folder Kimi Code's `install.ps1` puts `kimi.exe` in, below the user
/// profile `profile` as the script names it (`$env:USERPROFILE`), not below
/// HOME, which may name another folder.
fn kimi_folder(profile: Option<String>) -> Option<PathBuf> {
    profile
        .filter(|profile| !profile.is_empty())
        .map(|profile| PathBuf::from(below(&profile, ".kimi-code/bin")))
}
/// `path` with `first` in front and `last` at the end in order, each unless
/// `path` lists it already (compared without case and trailing separators);
/// None when nothing changes.
fn prepared_path(path: &OsStr, first: Option<PathBuf>, last: &[PathBuf]) -> Option<OsString> {
    let key = |folder: &Path| {
        folder
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    let mut folders: Vec<PathBuf> = env::split_paths(path).collect();
    let listed =
        |folders: &[PathBuf], folder: &Path| folders.iter().any(|entry| key(entry) == key(folder));
    let mut changed = false;
    if let Some(folder) = first.filter(|folder| !listed(&folders, folder)) {
        folders.insert(0, folder);
        changed = true;
    }
    for folder in last {
        if !listed(&folders, folder) {
            folders.push(folder.clone());
            changed = true;
        }
    }
    changed.then(|| env::join_paths(folders).ok()).flatten()
}
/// Whether this computer runs agent sessions: once the user has enabled them
/// (`hgs session-host install`, which writes the `local-sessions` marker in the
/// configuration directory; spec D2). Until then it is a control console for
/// remote machines. Whether a session host runs does not matter: saved sessions
/// stay in the state directory, and a host crash must not turn the computer back
/// into a control console.
pub fn local_sessions() -> bool {
    crate::host::staging::enabled()
}
/// Windows reports every exit as a code; crash statuses read as signals (see `exit_info`).
pub fn code(status: ExitStatus) -> i32 {
    ExitInfo::of(status).exit_code()
}
/// Crash statuses read as signals of the same meaning (`super::ntstatus_exit`,
/// spec D9); other codes, `exit(-1)` included, stay exit codes.
pub(super) fn exit_info(status: ExitStatus) -> ExitInfo {
    match status.code() {
        Some(code) => super::ntstatus_exit(code as u32),
        None => ExitInfo {
            code: None,
            signal: None,
        },
    }
}
fn extensions() -> Vec<String> {
    env::var("PATHEXT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|extension| extension.trim().to_ascii_lowercase())
        .filter(|extension| extension.len() > 1 && extension.starts_with('.'))
        .collect()
}
fn runnable_name(path: &Path, extensions: &[String]) -> bool {
    path.extension().is_some_and(|extension| {
        let extension = format!(".{}", extension.to_string_lossy().to_ascii_lowercase());
        extensions.contains(&extension)
    })
}
/// A file whose extension PATHEXT lists, as cmd.exe would run it.
pub fn executable(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    fs::metadata(path).is_ok_and(|m| m.is_file()) && runnable_name(path, &extensions())
}
pub(super) fn is_path(program: &str) -> bool {
    program.contains(['/', '\\', ':'])
}
pub(super) fn candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    let extensions = extensions();
    if runnable_name(Path::new(program), &extensions) {
        return vec![dir.join(program)];
    }
    extensions
        .iter()
        .map(|extension| dir.join(format!("{program}{extension}")))
        .collect()
}
/// The command that starts an agent or untracked program, `argv` with its program
/// first (spec D8). The program is found with PATHEXT (`super::which`). A
/// native program runs directly; an npm command shim runs what it would run
/// (`shim`), Node.js on the package's script or the package's program; any
/// other batch file, script or document is refused, as hgs never starts a
/// program through `cmd.exe /c`. NotFound when nothing on PATH has the name.
pub fn agent_command(argv: &[String]) -> io::Result<Command> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing program"))?;
    let path = super::which(program)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not found on PATH"))?;
    let (program, prefix) = resolve_program(&path)?;
    let mut command = Command::new(program);
    command.env_remove(crate::project_launcher::DATA_ENV);
    if let Some((words, data)) = crate::project_launcher::windows_command(argv)? {
        if !prefix.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a PowerShell project launcher needs a native PowerShell program"));
        }
        command.args(words).env(crate::project_launcher::DATA_ENV, data);
    } else {
        command.args(prefix).args(arguments);
    }
    Ok(command)
}
/// What runs for the program file `path`, and the arguments before the caller's.
fn resolve_program(path: &Path) -> io::Result<(PathBuf, Vec<PathBuf>)> {
    let refused = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is {what}; hgs starts programs directly, never through cmd.exe",
                path.display()
            ),
        )
    };
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("exe" | "com") => Ok((path.to_owned(), Vec::new())),
        Some("cmd" | "bat") => {
            let mut text = String::new();
            fs::File::open(path)?
                .take(64 * 1024)
                .read_to_string(&mut text)
                .map_err(|_| refused("a batch file that is not an npm command shim"))?;
            let folder = path.parent().unwrap_or(Path::new(""));
            match super::shim::npm_shim(&text) {
                Some(super::shim::NpmShim::Node { script }) => {
                    let script = folder.join(script);
                    if !script.is_file() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "the script {} of {} is missing",
                                script.display(),
                                path.display()
                            ),
                        ));
                    }
                    let bundled = folder.join("node.exe");
                    let node = if bundled.is_file() {
                        bundled
                    } else {
                        native_program("node").ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                format!("{} needs node.exe, which is not on PATH", path.display()),
                            )
                        })?
                    };
                    Ok((node, vec![script]))
                }
                Some(super::shim::NpmShim::Program { path: program }) => {
                    let program = folder.join(program);
                    let native = program.extension().is_some_and(|extension| {
                        ["exe", "com"]
                            .contains(&extension.to_string_lossy().to_ascii_lowercase().as_str())
                    });
                    if !native {
                        return Err(refused("an npm shim for something other than a program"));
                    }
                    if !program.is_file() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!(
                                "the program {} of {} is missing",
                                program.display(),
                                path.display()
                            ),
                        ));
                    }
                    Ok((program, Vec::new()))
                }
                None => Err(refused("a batch file that is not an npm command shim")),
            }
        }
        _ => Err(refused("not a program")),
    }
}
/// A native program (`.exe` or `.com`) named `name` on PATH.
fn native_program(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .flat_map(|folder| {
            [
                folder.join(format!("{name}.exe")),
                folder.join(format!("{name}.com")),
            ]
        })
        .find(|path| path.is_file())
}
/// The interactive shell of an `sh` session (spec D8): PowerShell 7 (`pwsh.exe`)
/// from PATH, else Windows PowerShell without its banner, by absolute path.
pub fn powershell() -> std::result::Result<Vec<String>, String> {
    let absolute = |path: PathBuf| canonical(&path).ok().map(|path| path.display().to_string());
    if let Some(pwsh) = native_program("pwsh").and_then(absolute) {
        return Ok(vec![pwsh]);
    }
    let system = env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .filter(|path| path.is_file());
    system
        .or_else(|| native_program("powershell"))
        .and_then(absolute)
        .map(|powershell| vec![powershell, "-NoLogo".into()])
        .ok_or_else(|| "neither pwsh.exe nor powershell.exe was found".into())
}
/// The stable CLI `<home>\.local\bin\hgs.exe` (spec D2, D8), the only hgs.exe a
/// session, hook, recipe or record names, so none of them names a portable
/// folder that a GUI update replaces. Whichever hgs.exe runs, this is the one
/// sessions start; `HGS_EXECUTABLE`, when set, must name it too. Anything else
/// fails closed, as does a computer where it is not installed.
pub fn stable_executable() -> std::result::Result<PathBuf, String> {
    let layout = crate::host::staging::Layout::current()
        .ok_or("no home directory: set USERPROFILE or HOME")?;
    stable_executable_in(&layout.cli, env::var_os("HGS_EXECUTABLE"))
}
fn stable_executable_in(
    stable: &Path,
    explicit: Option<OsString>,
) -> std::result::Result<PathBuf, String> {
    let installed = canonical(stable)
        .ok()
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "the stable hgs CLI {} is not installed; run hgs session-host install",
                stable.display()
            )
        })?;
    let same = |path: &Path| {
        canonical(path).is_ok_and(|path| {
            path.to_string_lossy().to_lowercase() == installed.to_string_lossy().to_lowercase()
        })
    };
    if let Some(explicit) = explicit
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        if !explicit.is_absolute() || !same(&explicit) {
            return Err(format!(
                "HGS_EXECUTABLE names {}, but sessions on this computer run only the stable \
                 hgs CLI {}",
                explicit.display(),
                installed.display()
            ));
        }
    }
    Ok(installed)
}
/// The Windows OpenSSH client from System32 first: a Git or MSYS ssh earlier on
/// PATH uses another configuration, key agent and home directory.
pub(super) fn default_ssh() -> OsString {
    env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe"))
        .filter(|path| path.is_file())
        .or_else(|| super::which("ssh"))
        .map_or_else(|| "ssh".into(), PathBuf::into_os_string)
}

// Windows OpenSSH never exits once it has used a synchronous pipe for one of its
// standard streams, and synchronous pipes are what std, Python, PowerShell and
// Qt give a child. With overlapped pipe ends (as libuv gives them) it behaves.
// Every pipe between hgs and ssh.exe is therefore an overlapped named pipe, and
// so is every relayed stream of this process (see `relayed`). Only `ssh -G`
// (no input, fresh output files) and NUL as output get other handles.
fn overlapped_pipe(child_reads: bool) -> io::Result<(fs::File, fs::File)> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.subsec_nanos());
    let name: Vec<u16> = format!(
        r"\\.\pipe\hgs-{}-{}-{unique}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let (direction, access) = if child_reads {
        (PIPE_ACCESS_OUTBOUND, GENERIC_READ | FILE_WRITE_ATTRIBUTES)
    } else {
        (PIPE_ACCESS_INBOUND, GENERIC_WRITE | FILE_READ_ATTRIBUTES)
    };
    unsafe {
        let ours = CreateNamedPipeW(
            name.as_ptr(),
            direction | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        );
        if ours == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let ours = fs::File::from_raw_handle(ours);
        let theirs = CreateFileW(
            name.as_ptr(),
            access,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        );
        if theirs == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok((ours, fs::File::from_raw_handle(theirs)))
    }
}
/// A child of this process in a kill-on-close job of its own that forbids
/// breakaway (spec D4), nested in this process's job: nothing the child starts can
/// leave it, and whatever is left in it ends when this is dropped. `Spawned`
/// dereferences to the child; on Unix `spawn` returns the child itself.
pub struct Spawned {
    child: Child,
    // Dropped after the child's handles.
    job: ChildJob,
}
impl std::ops::Deref for Spawned {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for Spawned {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Spawned {
    /// Split into the child and the job that must outlive its use.
    fn into_parts(self) -> (Child, ChildJob) {
        (self.child, self.job)
    }
    /// End the child and everything it started at once.
    pub fn terminate(&self) {
        self.job.terminate();
    }
    /// Independent of the original child's exit: descendants must drain too.
    pub(crate) fn wait_tree(&self, timeout: Duration) -> io::Result<bool> {
        self.job.0.wait_empty(timeout)
    }
    /// `Child::wait_with_output`; the job ends with the call.
    pub fn wait_with_output(self) -> io::Result<Output> {
        let (child, _job) = self.into_parts();
        child.wait_with_output()
    }
}
/// The job of a `Spawned` child. Closing it ends whatever is still in it.
pub struct ChildJob(Job);
impl ChildJob {
    fn terminate(&self) {
        let _ = self.0.terminate(1);
    }
}
/// Start `command` suspended with `flags`, put it in a job of its own
/// (`Limits::CHILD`) and only then let it run, so that everything it starts is
/// in the job too. A child that cannot join is ended before it has run. The
/// command keeps `flags` as its creation flags afterwards.
fn spawn_contained(command: &mut Command, flags: u32) -> io::Result<Spawned> {
    let flags = console_flags(flags, console_attached());
    let job = Job::new(Limits::CHILD)?;
    let spawned = command.creation_flags(CREATE_SUSPENDED | flags).spawn();
    command.creation_flags(flags);
    let mut child = spawned?;
    if let Err(error) = job
        .assign(child.as_handle())
        .and_then(|()| resume(child.id()))
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(Spawned {
        child,
        job: ChildJob(job),
    })
}
/// Resume the threads of a process created suspended (it has only its first).
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: plain call; a valid snapshot handle is closed by Owned below.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = process::Owned(handle);
    // SAFETY: THREADENTRY32 is plain data; all zeroes is a valid value.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut resumed = false;
    // SAFETY: the snapshot is open and `entry` is writable with its size set.
    let mut more = unsafe { Thread32First(handle.0, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: plain call; a returned handle is closed by Owned below.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = process::Owned(thread);
            // SAFETY: `thread` is open with THREAD_SUSPEND_RESUME access.
            if unsafe { ResumeThread(thread.0) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed = true;
        }
        // SAFETY: as for Thread32First.
        more = unsafe { Thread32Next(handle.0, &mut entry) } != 0;
    }
    if resumed {
        Ok(())
    } else {
        Err(io::Error::other("the new process has no thread to resume"))
    }
}
/// `Command::spawn` for a child that ends with this process and whose own
/// children cannot leave (see `Spawned`). It shares this console's process
/// group, so a console Ctrl+C reaches it.
pub fn spawn(command: &mut Command) -> io::Result<Spawned> {
    spawn_contained(command, 0)
}
/// A service helper has the same suspended/job admission as other children,
/// without opening a console on the interactive desktop.
pub(crate) fn spawn_hidden(command: &mut Command) -> io::Result<Spawned> {
    spawn_contained(command, CREATE_NO_WINDOW)
}
/// `spawn` in a process group of its own: a console Ctrl+C for the caller does
/// not reach it (`own_process_group`).
pub fn spawn_group(command: &mut Command) -> io::Result<Spawned> {
    spawn_contained(command, CREATE_NEW_PROCESS_GROUP)
}
/// `Command::status` through `spawn`.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}
/// `Command::output` through `spawn`, with the streams `Command::output` gives
/// when none is set: no input, output and errors captured. Unlike on Unix,
/// streams the caller set are replaced too (std cannot tell which were set): a
/// discarded stderr is captured instead. Not for ssh.exe, which hangs on such
/// pipes (`ssh_output`).
pub fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(command)?.wait_with_output()
}

type Setter = fn(&mut Command, Stdio) -> &mut Command;
const STREAMS: [Setter; 3] = [Command::stdin, Command::stdout, Command::stderr];
/// Start an ssh client with the requested standard streams piped to this
/// process. Unpiped input is a pipe that ends at once, whatever the caller set:
/// the peer never sees the end of NUL (`Stdio::null()`) through Windows OpenSSH.
/// The client runs in a process group and job of its own (`spawn_group`); the
/// job lives as long as the returned `SshChild`.
pub fn spawn_ssh(
    command: &mut Command,
    stdin: bool,
    stdout: bool,
    stderr: bool,
) -> io::Result<super::SshChild> {
    let mut ours: [Option<fs::File>; 3] = Default::default();
    for (index, piped) in [stdin, stdout, stderr].into_iter().enumerate() {
        if piped || index == 0 {
            let (mine, theirs) = overlapped_pipe(index == 0)?;
            STREAMS[index](command, theirs.into());
            ours[index] = Some(mine);
        }
    }
    let spawned = spawn_group(command);
    // Keep no copy of the child's ends, so reads end when the client exits.
    for (index, pipe) in ours.iter().enumerate() {
        if pipe.is_some() {
            STREAMS[index](command, Stdio::null());
        }
    }
    let [input, output, errors] = ours;
    let (child, job) = spawned?.into_parts();
    Ok(super::SshChild {
        child,
        // Unpiped input ends here, when its only writer closes.
        stdin: input.filter(|_| stdin).map(|pipe| Box::new(pipe) as _),
        stdout: output.map(|pipe| Box::new(pipe) as _),
        stderr: errors.map(|pipe| Box::new(pipe) as _),
        _job: job,
    })
}
/// `Command::output` for an ssh client: stdin closed, stdout and stderr captured.
pub fn ssh_output(command: &mut Command) -> io::Result<Output> {
    let mut ssh = spawn_ssh(command, false, true, true)?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        pipe.map(|mut pipe| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).map(|_| bytes)
            })
        })
    };
    let (stdout, stderr) = (read(ssh.stdout.take()), read(ssh.stderr.take()));
    let status = ssh.child.wait()?;
    let collect = |reader: Option<thread::JoinHandle<io::Result<Vec<u8>>>>| {
        reader.map_or(Ok(Vec::new()), |reader| {
            reader
                .join()
                .map_err(|_| io::Error::other("ssh output reader failed"))?
        })
    };
    Ok(Output {
        status,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    })
}
/// `Command::status` for an ssh client that reads `input` and shares this
/// process's stdout and stderr. This process's stdin is not passed on. The
/// client runs in a job of its own (`spawn`), which ends once its output has
/// been relayed.
pub fn ssh_status(command: &mut Command, input: fs::File) -> io::Result<ExitStatus> {
    let relay = Relay::prepare(command, Some(input))?;
    let spawned = spawn(command);
    let pumps = relay.start(command);
    let (status, _job) = match spawned {
        Ok(mut child) => (child.wait(), Some(child)),
        Err(error) => (Err(error), None),
    };
    pumps.finish();
    status
}
/// Whether a child gets one of this process's standard streams through a relay
/// pipe rather than the handle itself. Windows OpenSSH hangs on a synchronous
/// pipe, never sends the end of a file or of a character device such as NUL,
/// and writes a file from its start, over what `>>` keeps. Only a console
/// (`ssh -t` and Ctrl+C need it) and an absent or invalid handle (unknown type)
/// are passed on as they are.
fn relayed(file_type: FILE_TYPE, console: bool) -> bool {
    !console && matches!(file_type, FILE_TYPE_DISK | FILE_TYPE_CHAR | FILE_TYPE_PIPE)
}
fn needs_relay(handle: RawHandle) -> bool {
    let mut mode = 0;
    unsafe {
        relayed(
            GetFileType(handle as HANDLE),
            GetConsoleMode(handle as HANDLE, &mut mode) != 0,
        )
    }
}
/// A child's standard streams relayed through overlapped pipes: this process's
/// own unless they are consoles (see `relayed`), and a file given to read
/// instead of this process's stdin. Output reaches a redirected file through
/// this process's handle, at its position. Input ends where its source ends.
struct Relay {
    pipes: [Option<fs::File>; 3],
    source: Option<fs::File>,
}
impl Relay {
    fn prepare(command: &mut Command, source: Option<fs::File>) -> io::Result<Self> {
        let handles = [
            io::stdin().as_raw_handle(),
            io::stdout().as_raw_handle(),
            io::stderr().as_raw_handle(),
        ];
        let mut pipes: [Option<fs::File>; 3] = Default::default();
        for (index, handle) in handles.into_iter().enumerate() {
            if (index == 0 && source.is_some()) || needs_relay(handle) {
                let (mine, theirs) = overlapped_pipe(index == 0)?;
                STREAMS[index](command, theirs.into());
                pipes[index] = Some(mine);
            }
        }
        Ok(Self { pipes, source })
    }
    /// Call after spawning: the child holds its ends, this process keeps none.
    fn start(self, command: &mut Command) -> Pumps {
        for (index, pipe) in self.pipes.iter().enumerate() {
            if pipe.is_some() {
                STREAMS[index](command, Stdio::inherit());
            }
        }
        let Self {
            pipes: [input, output, errors],
            source,
        } = self;
        // Never joined: this process's stdin may stay open after the child exits.
        // The pipe closes at the end of the input, which the child then reads.
        if let Some(mut pipe) = input {
            thread::spawn(move || {
                let _ = match source {
                    Some(mut file) => io::copy(&mut file, &mut pipe),
                    None => io::copy(&mut io::stdin().lock(), &mut pipe),
                };
            });
        }
        let mut pumps = Vec::new();
        if let Some(pipe) = output {
            pumps.push(Pump::start(pipe, io::stdout));
        }
        if let Some(pipe) = errors {
            pumps.push(Pump::start(pipe, io::stderr));
        }
        Pumps(pumps)
    }
}
/// What a relay thread is doing, for `Pumps::finish`.
#[derive(Default)]
struct Progress {
    /// Waiting in a read of the child's pipe (not writing to this process's stream).
    reading: AtomicBool,
    /// Completed reads; a change means data arrived or the pipe ended.
    reads: AtomicU64,
}
struct Pump {
    thread: thread::JoinHandle<()>,
    progress: Arc<Progress>,
}
impl Pump {
    fn start<W: io::Write + 'static>(from: fs::File, to: fn() -> W) -> Self {
        let progress = Arc::new(Progress::default());
        let shared = Arc::clone(&progress);
        let thread = thread::spawn(move || copy_flushed(from, to(), &shared));
        Self { thread, progress }
    }
}
fn copy_flushed(mut from: fs::File, mut to: impl io::Write, progress: &Progress) {
    let mut buffer = [0; 8192];
    loop {
        progress.reading.store(true, Ordering::SeqCst);
        let read = from.read(&mut buffer);
        progress.reads.fetch_add(1, Ordering::SeqCst);
        progress.reading.store(false, Ordering::SeqCst);
        let Ok(count @ 1..) = read else {
            break;
        };
        // Blocks while the reader of this process's output is busy, as the child
        // itself would block on a stream it had inherited.
        if to
            .write_all(&buffer[..count])
            .and_then(|()| to.flush())
            .is_err()
        {
            break;
        }
    }
}
/// How long a pump may wait for data after the child has exited.
const RELAY_IDLE: Duration = Duration::from_secs(5);
struct Pumps(Vec<Pump>);
impl Pumps {
    /// Relay all of the child's output after it exits, however late this
    /// process's reader takes it: data already read or still in the pipe is
    /// never dropped. Only a pump that has waited in a read with no data for
    /// `RELAY_IDLE` is abandoned; then a helper the child left running (a
    /// connection multiplexer, for example) holds the pipe open.
    fn finish(self) {
        let started = Instant::now();
        let mut pending: Vec<(Pump, u64, Instant)> = self
            .0
            .into_iter()
            .map(|pump| {
                let reads = pump.progress.reads.load(Ordering::SeqCst);
                (pump, reads, started)
            })
            .collect();
        while !pending.is_empty() {
            let now = Instant::now();
            pending.retain_mut(|(pump, reads, idle_since)| {
                if pump.thread.is_finished() {
                    return false;
                }
                let current = pump.progress.reads.load(Ordering::SeqCst);
                if current != *reads || !pump.progress.reading.load(Ordering::SeqCst) {
                    *reads = current;
                    *idle_since = now;
                }
                now.duration_since(*idle_since) < RELAY_IDLE
            });
            if !pending.is_empty() {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
/// HOME when Git Bash or the user sets it, otherwise the user profile. The
/// layout below it is the same as on Unix (`.config\hgs`, `.local\state\hgs`).
pub fn home() -> Option<String> {
    home_from(env::var("HOME").ok(), env::var("USERPROFILE").ok())
}
fn home_from(home: Option<String>, profile: Option<String>) -> Option<String> {
    home.filter(|value| !value.is_empty())
        .or(profile.filter(|value| !value.is_empty()))
}
/// `relative`, written with '/', below `base` with Windows separators.
pub fn below(base: &str, relative: &str) -> String {
    relative
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .fold(PathBuf::from(base), |path, part| path.join(part))
        .to_string_lossy()
        .into_owned()
}
/// The lowercased computer name, the default for HGS_SELF.
pub fn hostname() -> String {
    env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| capture("hostname", &[]))
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}
// Files below the user profile inherit its owner-only ACL. Windows has no mode
// bits to narrow, so the private-file helpers keep that inherited protection.
pub fn make_private(_: &fs::File) -> io::Result<()> {
    Ok(())
}
pub fn restrict_dir(_: &Path) -> io::Result<()> {
    Ok(())
}
pub fn private_open(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options
}
/// Open a reparse point (a symbolic link) itself; it is then not a regular file.
pub fn no_follow(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
}
/// NTFS journals the rename itself, and a directory cannot be flushed through a
/// handle that std can open.
pub fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}
/// A scanner, the indexer or a reader without delete sharing holds the file.
pub(crate) fn transient_replace_error(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        [
            ERROR_ACCESS_DENIED,
            ERROR_SHARING_VIOLATION,
            ERROR_LOCK_VIOLATION,
        ]
        .contains(&(code as u32))
    })
}
/// A console Ctrl+C for the caller does not reach the child.
pub fn own_process_group(command: &mut Command) -> &mut Command {
    command.creation_flags(CREATE_NEW_PROCESS_GROUP)
}
/// Stop a child. Its own children end with its job (`Spawned`, `SshChild`) or,
/// for a child started otherwise, with this process's job.
pub fn kill_process_group(child: &mut Child) {
    let _ = child.kill();
}
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            // Another user's process exists but cannot be opened.
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        CloseHandle(process);
        running
    }
}
/// Volume and file index identify the file itself; creation times do not,
/// because NTFS tunnelling gives a replacement under the same name the old one.
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    FileIdentity::of(&open_for_identity(path).ok()?).ok()
}
/// Opens a file or a directory for its identity, following links. Windows opens a
/// directory only with backup semantics; `fs::metadata` opens paths the same way.
fn open_for_identity(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}
/// Not read on Windows: callers compare the file at the path they started from.
pub fn image_identity() -> Option<FileIdentity> {
    None
}
impl FileIdentity {
    /// The identity and metadata of an open file.
    pub fn with_metadata(file: &fs::File) -> io::Result<(Self, fs::Metadata)> {
        Ok((Self::of(file)?, file.metadata()?))
    }
    /// The identity and metadata of the file `path` names, following links.
    pub fn at(path: &Path) -> io::Result<(Self, fs::Metadata)> {
        Self::with_metadata(&open_for_identity(path)?)
    }
    /// The identity of a file this process has open.
    pub fn of(file: &fs::File) -> io::Result<Self> {
        let info = file_information(file)?;
        let (modified, modified_nanos) = unix_time(ticks(info.ftLastWriteTime));
        Ok(Self {
            device: info.dwVolumeSerialNumber.into(),
            inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            modified,
            modified_nanos,
        })
    }
}
fn file_information(file: &fs::File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}
/// 100 ns ticks from 1601-01-01, when FILETIME starts, to 1970-01-01.
const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
/// FILETIME ticks as Unix seconds and nanoseconds, floored like `st_mtime`.
fn unix_time(ticks: u64) -> (i64, i64) {
    let since = ticks as i128 - UNIX_EPOCH_TICKS as i128;
    (
        since.div_euclid(10_000_000) as i64,
        (since.rem_euclid(10_000_000) * 100) as i64,
    )
}
/// Link markers live in `hgs-link-<pid>-<random>` folders of the temporary
/// directory. A client that is killed leaves its folder behind: the desktop
/// app's Disconnect ends the attach client with its job, and a closed
/// pseudoconsole ends its clients almost at once. The next interactive link
/// removes the folders whose process is gone.
const LINK_MARKER_PREFIX: &str = "hgs-link-";
/// This process's marker folder, for the console handler.
static LINK_MARKER: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The temporary directory of an interactive link's marker file.
pub fn link_marker_dir() -> io::Result<tempfile::TempDir> {
    let parent = env::temp_dir();
    remove_stale_link_markers(&parent);
    let directory = tempfile::Builder::new()
        .prefix(&format!("{LINK_MARKER_PREFIX}{}-", std::process::id()))
        .tempdir_in(&parent)?;
    if let Ok(mut current) = LINK_MARKER.lock() {
        *current = Some(directory.path().to_owned());
    }
    Ok(directory)
}
fn remove_stale_link_markers(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let owner = name
            .to_str()
            .and_then(|name| name.strip_prefix(LINK_MARKER_PREFIX))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        if owner.is_some_and(|pid| !process_alive(pid)) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
fn remove_link_marker() {
    if let Ok(current) = LINK_MARKER.lock() {
        if let Some(path) = current.as_deref() {
            let _ = fs::remove_dir_all(path);
        }
    }
}
/// Carries a LocalCommand's file path from hgs through ssh.exe to cmd.exe.
const LOCAL_COMMAND_PATH: &str = "HGS_LINK_MARKER";
/// An OpenSSH LocalCommand for `ssh` that creates `path`. Windows OpenSSH expands
/// its `%` tokens (an unknown one is fatal) and runs the result with cmd.exe
/// (`_wsystem`), which expands `%NAME%` pairs, so a temporary folder below a
/// profile name containing `%` cannot be written into the command. cmd.exe reads
/// the path from the client's environment instead: neither layer expands the
/// value again, and the quotes keep characters such as `&` and `^` literal.
pub fn create_file_command(ssh: &mut Command, path: &Path) -> String {
    ssh.env(LOCAL_COMMAND_PATH, path);
    format!("type nul > \"%%{LOCAL_COMMAND_PATH}%%\"")
}
/// Escape sequences need virtual terminal processing; enable it when the
/// console allows. A legacy console without it gets no sequences at all.
pub(super) fn virtual_terminal() -> bool {
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if output.is_null()
            || output == INVALID_HANDLE_VALUE
            || GetConsoleMode(output, &mut mode) == 0
        {
            return false;
        }
        mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
            || SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}
pub fn gui_env() -> Vec<String> {
    Vec::new()
}

/// The desktop service never keeps a console of its own: closing that window or
/// Windows Terminal tab would end it. Windows 11 24H2 and later allocate none
/// (`consoleAllocationPolicy` in src/hgs.manifest); earlier releases create one
/// for a logon task, which is released here before startup. A console that
/// another process shares, such as the terminal it was started from, is kept.
///
/// Standard handles that named the released console are cleared: Windows would
/// reuse their numbers for files opened later, and output written through a
/// stale handle would land in those files. Cleared, it is discarded, and
/// inherited child streams are empty.
pub fn release_private_console() -> bool {
    let window = !unsafe { GetConsoleWindow() }.is_null();
    let mut processes = [0u32; 2];
    let attached = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    if !super::private_console(window, attached) {
        return false;
    }
    let console = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| {
        let handle = unsafe { GetStdHandle(which) };
        let mut mode = 0;
        let named = !handle.is_null()
            && handle != INVALID_HANDLE_VALUE
            && unsafe { GetConsoleMode(handle, &mut mode) } != 0;
        (which, named)
    });
    if unsafe { FreeConsole() } == 0 {
        return false;
    }
    for (which, named) in console {
        if named {
            unsafe { SetStdHandle(which, std::ptr::null_mut()) };
        }
    }
    true
}
/// Whether a console is attached to this process. The desktop service has none.
fn console_attached() -> bool {
    let mut processes = [0u32; 1];
    (unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) }) != 0
}
/// The creation flags for a child of this process: a console program started by
/// a process without a console would otherwise get a console window of its own
/// (ssh.exe, a remote watch), so it gets a hidden one unless the caller chose.
fn console_flags(flags: u32, attached: bool) -> u32 {
    if attached || flags & (CREATE_NEW_CONSOLE | CREATE_NO_WINDOW | DETACHED_PROCESS) != 0 {
        flags
    } else {
        flags | CREATE_NO_WINDOW
    }
}
/// `Command::spawn` without a console window, for children of the desktop service.
pub fn spawn_windowless(command: &mut Command) -> io::Result<Child> {
    command.creation_flags(CREATE_NO_WINDOW).spawn()
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// The console delivers Ctrl+C and Ctrl+Break to every attached process. The
/// child (ssh.exe) handles them; hgs only waits for its exit. When the console
/// itself goes away (its window closes, the user logs off), this process ends
/// once the handler returns and no destructor runs, so the link marker is
/// removed here.
unsafe extern "system" fn interrupted(event: u32) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            INTERRUPTED.store(true, Ordering::Relaxed);
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            remove_link_marker();
            0
        }
        _ => 0,
    }
}
/// Run an interactive child on the shared console and wait for it. An
/// interrupted wait reports 130 like Unix SIGINT; otherwise the child's code.
/// The child shares this console's process group, so Ctrl+C reaches it, and
/// runs in a job of its own (`spawn`), which ends once its output has been
/// relayed.
pub fn wait_interactive(command: &mut Command) -> Result<i32> {
    wait_interactive_from(command, None)
}
/// A checked answer file uses the same overlapped SSH relay and console
/// interrupt handling as ordinary forwarding, never a synchronous pipe.
pub fn wait_interactive_with_input(command: &mut Command, input: fs::File) -> Result<i32> {
    wait_interactive_from(command, Some(input))
}
fn wait_interactive_from(command: &mut Command, input: Option<fs::File>) -> Result<i32> {
    struct Handler;
    impl Drop for Handler {
        fn drop(&mut self) {
            unsafe {
                SetConsoleCtrlHandler(Some(interrupted), 0);
            }
        }
    }
    INTERRUPTED.store(false, Ordering::Relaxed);
    // Never SetConsoleCtrlHandler(NULL, TRUE): children would inherit ignoring Ctrl+C.
    let _handler = (unsafe { SetConsoleCtrlHandler(Some(interrupted), 1) } != 0).then_some(Handler);
    let relay = Relay::prepare(command, input)?;
    let spawned = spawn(command);
    let pumps = relay.start(command);
    let mut child = spawned.map_err(|e| {
        Error::new(
            127,
            format!("{}: {e}", command.get_program().to_string_lossy()),
        )
    })?;
    let rc = code(child.wait()?);
    pumps.finish();
    Ok(if INTERRUPTED.load(Ordering::Relaxed) {
        130
    } else {
        rc
    })
}

pub use self::process::*;
#[path = "windows_repair.rs"]
mod repair;
pub use repair::repair_private;
/// Process, file and control primitives for `state`.
mod process {
    use super::*;

    /// `home` as a path.
    pub fn home_dir() -> Option<PathBuf> {
        home().map(PathBuf::from)
    }
    /// The canonical form of `path` without the `\\?\` prefix that canonicalize
    /// adds, whenever the plain form names the same file. Stored and compared
    /// paths, and paths agents report, use the plain form.
    pub fn canonical(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        fs::canonicalize(path).map(plain_path)
    }
    /// A verbatim path is kept only when the plain form would differ: longer than
    /// MAX_PATH, or with a component Win32 would change or treat as a device
    /// (trailing dots or spaces, `NUL`, `COM1.txt`, a stream name after `:`).
    pub(super) fn plain_path(path: PathBuf) -> PathBuf {
        let Some(text) = path.to_str() else {
            return path;
        };
        let (plain, components) = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            (format!(r"\\{rest}"), rest)
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            let bytes = rest.as_bytes();
            if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
                return path;
            }
            (rest.to_owned(), &rest[3..])
        } else {
            return path;
        };
        let legacy = |name: &str| {
            let stem = name
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end()
                .to_ascii_uppercase();
            let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && stem.as_bytes()[3].is_ascii_digit()
                    && stem.as_bytes()[3] != b'0');
            !name.is_empty()
                && !name.ends_with(['.', ' '])
                && !name.contains(['<', '>', '"', '/', '|', '?', '*', ':'])
                && !device
        };
        if plain.encode_utf16().count() >= 260
            || !components
                .split('\\')
                .filter(|name| !name.is_empty())
                .all(legacy)
        {
            return path;
        }
        PathBuf::from(plain)
    }
    /// Extensions of process images and of the command shims npm installs. Not
    /// PATHEXT: it differs between computers and by default also lists scripts
    /// such as `.js`, so `codex.js` would read as `codex` on one and not another.
    const PROGRAM_EXTENSIONS: [&str; 4] = [".exe", ".com", ".bat", ".cmd"];
    /// The name a program goes by in process names and command lines: the file
    /// name without an extension that makes it a program (`node.exe` is `node`),
    /// lowercase because Windows names are case-insensitive.
    pub fn program_name(path: &Path) -> Option<String> {
        let name = path.file_name()?.to_str()?.to_lowercase();
        match name.rfind('.') {
            Some(dot) if dot > 0 && PROGRAM_EXTENSIONS.contains(&&name[dot..]) => {
                Some(name[..dot].to_owned())
            }
            _ => Some(name),
        }
    }
    /// Directories below the user profile inherit its owner-only ACL.
    pub fn private_dir(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }
    /// Open a reparse point (a symbolic link) itself; it is then not a regular file.
    pub fn refuse_link(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
    }
    /// A file path names no FIFO here, so opening one never waits for a writer.
    pub fn never_block(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
        options
    }
    /// Whether `path` itself (a link or junction is a reparse point and never
    /// counts) is the expected kind of entry, owned by this user, with no access
    /// for anyone but this user, SYSTEM and Administrators and, for a file, a
    /// single link.
    pub fn verify_private(path: &Path, kind: Private) -> io::Result<bool> {
        let entry = fs::OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        private_entry(&entry, kind)
    }
    /// `verify_private` for a file this process has open (for reading or writing).
    pub fn verify_private_file(file: &fs::File) -> io::Result<bool> {
        private_entry(file, Private::File)
    }
    /// Owner-only existing directory, checked through its pinned handle.
    pub fn verify_private_directory(file: &fs::File) -> io::Result<bool> {
        private_entry(file, Private::Directory)
    }
    /// WhiteWh fork: an existing directory this user owns that nobody else can
    /// change: every ACE granting another principal access grants only reading
    /// (no write, append, delete, child delete, attribute, ACL or owner change).
    /// Codex grants its sandbox group read access on `~/.local`; that folder
    /// stays usable as an ancestor of a private record, which itself must
    /// still pass `verify_private_directory`/`verify_private_file`.
    pub fn verify_guarded_directory(file: &fs::File) -> io::Result<bool> {
        let info = file_information(file)?;
        let attributes = info.dwFileAttributes;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Ok(false);
        }
        guarded_security(file.as_raw_handle() as HANDLE)
    }
    /// Any access that lets a principal change an entry or the names below it.
    const CHANGING_ACCESS: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
        | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
        | 0x0000_0010 // FILE_WRITE_EA
        | 0x0000_0040 // FILE_DELETE_CHILD
        | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
        | 0x0001_0000 // DELETE
        | 0x0004_0000 // WRITE_DAC
        | 0x0008_0000 // WRITE_OWNER
        | 0x0200_0000 // MAXIMUM_ALLOWED
        | 0x1000_0000 // GENERIC_ALL
        | 0x4000_0000; // GENERIC_WRITE
    fn guarded_security(handle: HANDLE) -> io::Result<bool> {
        let user = current_user()
            .ok_or_else(|| io::Error::other("this user's identity is unavailable"))?;
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let code = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        struct Descriptor(PSECURITY_DESCRIPTOR);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
        let _descriptor = Descriptor(descriptor);
        if owner.is_null() || unsafe { sid_bytes(owner) } != user {
            return Ok(false);
        }
        if dacl.is_null() {
            return Ok(false);
        }
        for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
                || header.AceType == ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                return Ok(false);
            }
            let allowed = ace as *const ACCESS_ALLOWED_ACE;
            let sid = unsafe { std::ptr::addr_of!((*allowed).SidStart) } as PSID;
            let trusted = unsafe {
                sid_bytes(sid) == user
                    || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                    || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                    || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0
            };
            if !trusted && unsafe { (*allowed).Mask } & CHANGING_ACCESS != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn private_entry(file: &fs::File, kind: Private) -> io::Result<bool> {
        let info = file_information(file)?;
        let attributes = info.dwFileAttributes;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(false);
        }
        let shaped = match kind {
            Private::Directory => attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            Private::File => attributes & FILE_ATTRIBUTE_DIRECTORY == 0 && info.nNumberOfLinks == 1,
        };
        Ok(shaped && private_security(file.as_raw_handle() as HANDLE)?)
    }
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    /// The owner is this user and every ACE that grants access names this user,
    /// SYSTEM, Administrators or OWNER RIGHTS, the ACL a user profile passes on.
    /// An unknown kind of allowing ACE fails closed.
    fn private_security(handle: HANDLE) -> io::Result<bool> {
        let user = current_user()
            .ok_or_else(|| io::Error::other("this user's identity is unavailable"))?;
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let code = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if code != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        struct Descriptor(PSECURITY_DESCRIPTOR);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
        let _descriptor = Descriptor(descriptor);
        if owner.is_null() || unsafe { sid_bytes(owner) } != user {
            return Ok(false);
        }
        // A missing DACL grants everyone everything.
        if dacl.is_null() {
            return Ok(false);
        }
        for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
            let mut ace = std::ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
                || header.AceType == ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                return Ok(false);
            }
            let sid = unsafe { std::ptr::addr_of!((*(ace as *const ACCESS_ALLOWED_ACE)).SidStart) }
                as PSID;
            let trusted = unsafe {
                sid_bytes(sid) == user
                    || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                    || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
                    || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0
            };
            if !trusted {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// # Safety
    /// `sid` points to a valid SID.
    unsafe fn sid_bytes<'a>(sid: PSID) -> &'a [u8] {
        std::slice::from_raw_parts(sid as *const u8, GetLengthSid(sid) as usize)
    }
    /// The user SID of `process`'s token, when it can be read.
    fn token_user(process: HANDLE) -> Option<Vec<u8>> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = Owned(token);
        let mut size = 0;
        unsafe {
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut size);
        }
        if size == 0 {
            return None;
        }
        // u64 elements keep the TOKEN_USER header aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return None;
        }
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        Some(unsafe { sid_bytes(user.User.Sid) }.to_vec())
    }
    pub(super) fn current_user() -> Option<&'static [u8]> {
        static USER: OnceLock<Option<Vec<u8>>> = OnceLock::new();
        USER.get_or_init(|| token_user(unsafe { GetCurrentProcess() }))
            .as_deref()
    }

    /// A kernel handle this process owns, closed when dropped.
    pub(super) struct Owned(pub(super) HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn open_process(pid: u32, access: u32) -> io::Result<Owned> {
        let process = unsafe { OpenProcess(access, 0, pid) };
        if process.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Owned(process))
        }
    }
    /// OpenProcess reports an ID that no process has as an invalid parameter.
    fn no_such_process(error: &io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
    }
    /// A process object stays readable after its process exits while any handle
    /// to it is open; such a process is gone, like a Unix zombie.
    fn exited(process: HANDLE) -> bool {
        unsafe { WaitForSingleObject(process, 0) == WAIT_OBJECT_0 }
    }
    fn creation_ticks(process: HANDLE) -> Option<u64> {
        let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [creation, exit, kernel, user] = &mut times;
        (unsafe { GetProcessTimes(process, creation, exit, kernel, user) } != 0)
            .then(|| ticks(*creation))
    }

    pub fn presence(pid: u32) -> Presence {
        // 0 is the idle process, never a process of ours.
        if pid == 0 {
            return Presence::Unknown;
        }
        match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) {
            Ok(process) if exited(process.0) => Presence::Gone,
            Ok(_) => Presence::Present,
            Err(error) if no_such_process(&error) => Presence::Gone,
            Err(_) => Presence::Unknown,
        }
    }
    /// When `pid` was created, as decimal 100 ns ticks. Records are per computer,
    /// so this compares as text exactly like the Unix `lstart` form, at a far finer
    /// resolution: Windows reuses process IDs quickly. Empty when the process does
    /// not exist or has exited; None when it cannot be opened (another user's).
    pub fn process_start_time(pid: u32) -> Option<String> {
        if pid == 0 {
            return Some(String::new());
        }
        let process =
            match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) {
                Ok(process) => process,
                Err(error) if no_such_process(&error) => return Some(String::new()),
                Err(_) => return None,
            };
        if exited(process.0) {
            return Some(String::new());
        }
        creation_ticks(process.0).map(|ticks| ticks.to_string())
    }
    /// There is no ps to ask.
    pub fn process_start_fallback(_: u32) -> Option<String> {
        None
    }

    /// Every process as (pid, parent, image file name) from a Toolhelp snapshot.
    fn snapshot() -> io::Result<Vec<(u32, u32, String)>> {
        let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = Owned(handle);
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut rows = Vec::new();
        let mut more = unsafe { Process32FirstW(handle.0, &mut entry) } != 0;
        while more {
            let name = &entry.szExeFile;
            let length = name.iter().position(|c| *c == 0).unwrap_or(name.len());
            rows.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&name[..length]),
            ));
            more = unsafe { Process32NextW(handle.0, &mut entry) } != 0;
        }
        Ok(rows)
    }
    /// The image file name (`node.exe`) of every process that runs now, by ID.
    /// Toolhelp names processes of every user and integrity level.
    pub fn process_names() -> io::Result<BTreeMap<u32, String>> {
        Ok(snapshot()?
            .into_iter()
            .map(|(pid, _, name)| (pid, name))
            .collect())
    }
    /// Toolhelp keeps a parent's ID after the parent exits, and Windows soon gives
    /// that ID to another process: a parent link counts only while a process with
    /// that ID exists and was created no later than the child. Otherwise 0.
    fn verified_parent(parent: u32, child_created: u64) -> u32 {
        let created = open_process(parent, PROCESS_QUERY_LIMITED_INFORMATION)
            .ok()
            .and_then(|process| creation_ticks(process.0));
        match created {
            Some(created) if parent != 0 && created <= child_created => parent,
            _ => 0,
        }
    }
    fn listed_parent(pid: u32) -> Option<u32> {
        snapshot()
            .ok()?
            .into_iter()
            .find(|row| row.0 == pid)
            .map(|row| row.1)
    }
    /// FILETIME ticks as Unix seconds.
    fn unix_seconds(ticks: u64) -> f64 {
        (ticks as f64 - UNIX_EPOCH_TICKS as f64) / 10_000_000.
    }

    pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
        let process =
            open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE).ok()?;
        if exited(process.0) || token_user(process.0).as_deref() != current_user() {
            return None;
        }
        let created = creation_ticks(process.0)?;
        Some(ProcessIdentity {
            parent: verified_parent(listed_parent(pid)?, created),
            group: None,
            birth: created.to_string(),
            started: unix_seconds(created),
        })
    }
    pub fn describe(pid: u32) -> Option<ProcessImage> {
        let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        let mut path = vec![0u16; 32_768];
        let mut size = path.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut size)
        } == 0
        {
            return None;
        }
        Some(ProcessImage {
            executable: String::from_utf16_lossy(&path[..size as usize]),
            argv: command_line(process.0).map(|line| split_command_line(&line)),
        })
    }
    /// The command line a process was started with (Windows 8.1 and later).
    fn command_line(process: HANDLE) -> Option<Vec<u16>> {
        let mut size = 0u32;
        unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut size,
            );
        }
        if (size as usize) < std::mem::size_of::<UNICODE_STRING>() {
            return None;
        }
        // A UNICODE_STRING followed by its text; u64 elements keep it aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        };
        if status < 0 {
            return None;
        }
        let text = unsafe { &*(buffer.as_ptr() as *const UNICODE_STRING) };
        if text.Buffer.is_null() {
            return Some(Vec::new());
        }
        Some(
            unsafe { std::slice::from_raw_parts(text.Buffer, usize::from(text.Length) / 2) }
                .to_vec(),
        )
    }
    /// Arguments as the C runtime splits a command line.
    fn split_command_line(line: &[u16]) -> Vec<String> {
        // An empty line would give this process's own path instead.
        if line
            .iter()
            .all(|c| *c == u16::from(b' ') || *c == u16::from(b'\t'))
        {
            return Vec::new();
        }
        let text: Vec<u16> = line.iter().copied().chain(Some(0)).collect();
        let mut count = 0;
        let argv = unsafe { CommandLineToArgvW(text.as_ptr(), &mut count) };
        if argv.is_null() {
            return Vec::new();
        }
        let arguments = (0..usize::try_from(count).unwrap_or(0))
            .map(|index| unsafe {
                let argument = *argv.add(index);
                let length = (0..).take_while(|&at| *argument.add(at) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(argument, length))
            })
            .collect();
        unsafe {
            LocalFree(argv.cast());
        }
        arguments
    }
    /// Every process with its parent as Toolhelp lists it; Windows has no process
    /// groups. Parent links are raw here; `process_identity` verifies them.
    pub fn process_table() -> std::result::Result<BTreeMap<u32, ProcessRow>, String> {
        let rows = snapshot().map_err(|_| "Live process inspection is unavailable".to_owned())?;
        Ok(rows
            .into_iter()
            .map(|(pid, parent, _)| {
                (
                    pid,
                    ProcessRow {
                        pid,
                        parent,
                        group: None,
                    },
                )
            })
            .collect())
    }
    /// The verified parent and the arguments of `pid`.
    pub fn parent_and_argv(pid: u32) -> Option<(u32, Vec<String>)> {
        let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        let created = creation_ticks(process.0)?;
        let argv = split_command_line(&command_line(process.0)?);
        Some((verified_parent(listed_parent(pid)?, created), argv))
    }
    pub fn parent_process_id() -> u32 {
        let created = creation_ticks(unsafe { GetCurrentProcess() }).unwrap_or(0);
        listed_parent(std::process::id()).map_or(0, |parent| verified_parent(parent, created))
    }
    /// The verified parent and image file name of `pid`, each None when unknown.
    pub fn parent_and_name(pid: u32) -> io::Result<(Option<u32>, Option<String>)> {
        let Some((_, parent, name)) = snapshot()?.into_iter().find(|row| row.0 == pid) else {
            return Ok((None, None));
        };
        let created = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)
            .ok()
            .and_then(|process| creation_ticks(process.0));
        let parent = created
            .map(|created| verified_parent(parent, created))
            .filter(|parent| *parent != 0);
        Ok((parent, Some(name)))
    }
    /// Another process's current directory lives in its memory, not in anything
    /// the system reports; until hgs reads it there, nothing may count as unused.
    pub fn current_directories() -> std::result::Result<Vec<(u32, PathBuf, String)>, String> {
        Err("Running processes cannot be inspected on this computer".into())
    }
    pub fn worktree_processes(
        _deadline: std::time::Instant,
    ) -> std::result::Result<crate::platform::WorktreeProcesses, String> {
        current_directories().map(Into::into)
    }
    /// Windows cannot list another process's open files without a driver; it can
    /// only say who holds a given file (`holds_open`).
    // Retain this explicit unsupported capability and its contract test even
    // though Windows fork readiness no longer calls PID-to-file enumeration.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_files(_: u32) -> std::result::Result<Vec<PathBuf>, RunError> {
        Err(RunError::Unsupported)
    }
    /// Whether `pid` has the file at `path` open, as the Restart Manager reports it.
    pub fn holds_open(pid: u32, path: &Path) -> std::result::Result<bool, RunError> {
        let check = |code: u32| {
            if code == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(RunError::Io(io::Error::from_raw_os_error(code as i32)))
            }
        };
        struct Session(u32);
        impl Drop for Session {
            fn drop(&mut self) {
                unsafe {
                    RmEndSession(self.0);
                }
            }
        }
        let mut handle = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        check(unsafe { RmStartSession(&mut handle, 0, key.as_mut_ptr()) })?;
        let session = Session(handle);
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let files = [name.as_ptr()];
        check(unsafe {
            RmRegisterResources(
                session.0,
                1,
                files.as_ptr(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        })?;
        let mut holders: Vec<RM_PROCESS_INFO> = Vec::new();
        loop {
            let (mut needed, mut count, mut reasons) = (0, holders.len() as u32, 0);
            let list = if holders.is_empty() {
                std::ptr::null_mut()
            } else {
                holders.as_mut_ptr()
            };
            let code = unsafe { RmGetList(session.0, &mut needed, &mut count, list, &mut reasons) };
            if code == ERROR_SUCCESS {
                holders.truncate(count as usize);
                break;
            }
            if code != ERROR_MORE_DATA {
                return Err(RunError::Io(io::Error::from_raw_os_error(code as i32)));
            }
            holders = (0..needed).map(|_| RM_PROCESS_INFO::default()).collect();
        }
        Ok(holders
            .iter()
            .any(|holder| holder.Process.dwProcessId == pid))
    }

    /// A process to stop, pinned by an open handle: Windows never gives its ID to
    /// another process while the handle is open.
    pub struct PinnedProcess {
        pid: u32,
        handle: Owned,
    }
    impl PinnedProcess {
        pub fn pin(pid: u32, birth: &str) -> std::result::Result<Self, String> {
            let handle = open_process(
                pid,
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            )
            .map_err(|_| "Process changed or cannot be opened".to_owned())?;
            if exited(handle.0)
                || creation_ticks(handle.0)
                    .map(|ticks| ticks.to_string())
                    .as_deref()
                    != Some(birth)
            {
                return Err("Process identity changed".into());
            }
            Ok(Self { pid, handle })
        }
        pub fn alive(&self) -> bool {
            !exited(self.handle.0)
        }
        /// Windows has no polite request to exit that reaches any process; the
        /// caller's grace period passes and `kill` ends it.
        pub fn terminate(&self) -> std::result::Result<(), String> {
            Ok(())
        }
        pub fn kill(&self) -> std::result::Result<(), String> {
            if self.alive() && unsafe { TerminateProcess(self.handle.0, 1) } == 0 && self.alive() {
                return Err(format!(
                    "Could not stop PID {}: {}",
                    self.pid,
                    io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }

    /// Windows has no request to exit that reaches a process on another console;
    /// the session host asks an agent's supervisor instead (Phase 2 D11).
    pub fn request_exit(_: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an agent on Windows is stopped through its session host",
        ))
    }

    /// Stop requests in the signal numbers records use: SIGTERM for Ctrl+Break
    /// (the pause request), SIGHUP for a console that closes.
    const SIGHUP: i32 = 1;
    const SIGTERM: i32 = 15;
    static SHUTDOWN_SIGNAL: AtomicI32 = AtomicI32::new(0);
    static SHUTDOWN_DONE: AtomicBool = AtomicBool::new(false);
    pub(super) unsafe extern "system" fn shutdown_event(event: u32) -> i32 {
        match event {
            // Ctrl+C belongs to the agent, which shares the console and gets it too.
            CTRL_C_EVENT => 1,
            CTRL_BREAK_EVENT => {
                SHUTDOWN_SIGNAL.store(SIGTERM, Ordering::SeqCst);
                1
            }
            CTRL_CLOSE_EVENT => {
                SHUTDOWN_SIGNAL.store(SIGHUP, Ordering::SeqCst);
                // Windows ends this process once the handler returns, and at the
                // latest after 5 s: leave time to record how the agent ended.
                let deadline = Instant::now() + Duration::from_secs(4);
                while !SHUTDOWN_DONE.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                }
                1
            }
            // Neither says which user logs off or whether this session ends.
            CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => 1,
            _ => 0,
        }
    }
    /// A supervisor's stop handling until it is dropped. Ctrl+C belongs to the
    /// agent; Ctrl+Break and a closing console are recorded for `shutdown_signal`.
    /// The console delivers both to the agent itself, which shares it.
    pub struct ShutdownGuard(());
    impl ShutdownGuard {
        pub fn install() -> io::Result<Self> {
            SHUTDOWN_SIGNAL.store(0, Ordering::SeqCst);
            SHUTDOWN_DONE.store(false, Ordering::SeqCst);
            unsafe {
                // A process group created for this process starts with Ctrl+C
                // ignored, and children inherit that; the agent must receive it.
                SetConsoleCtrlHandler(None, 0);
                if SetConsoleCtrlHandler(Some(shutdown_event), 1) == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(Self(()))
        }
    }
    impl Drop for ShutdownGuard {
        fn drop(&mut self) {
            SHUTDOWN_DONE.store(true, Ordering::SeqCst);
            unsafe {
                SetConsoleCtrlHandler(Some(shutdown_event), 0);
            }
        }
    }
    pub fn shutdown_signal() -> i32 {
        SHUTDOWN_SIGNAL.load(Ordering::SeqCst)
    }
    /// The agent the supervisor started last: its process ID and a handle that
    /// keeps the ID from being reused, for the supervisor channel's evidence
    /// (`host::supervisor`, spec D10).
    static AGENT: Mutex<Option<(u32, OwnedHandle)>> = Mutex::new(None);
    /// `child` is the supervisor's agent from now on (`supervised_agent`). The
    /// console delivers later requests to it itself, as it shares this process
    /// group. One that arrived before it started never reached it: raise
    /// Ctrl+Break again for the group.
    pub fn forward_shutdown(child: &Child) {
        let pinned = child.as_handle().try_clone_to_owned().ok();
        *AGENT.lock().unwrap_or_else(PoisonError::into_inner) =
            pinned.map(|handle| (child.id(), handle));
        if SHUTDOWN_SIGNAL.load(Ordering::SeqCst) != 0 {
            let _ = break_process_group();
        }
    }
    /// Nothing is passed on (see `forward_shutdown`), so nothing stops. The
    /// agent stays the supervisor's until the next one starts: its evidence then
    /// shows it gone.
    pub fn shutdown_child_exited() {}
    /// The agent `forward_shutdown` named last: its process ID and whether it
    /// still runs.
    pub fn supervised_agent() -> Option<(u32, bool)> {
        AGENT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(pid, handle)| (*pid, !exited(handle.as_raw_handle())))
    }
    /// D11's pause: Ctrl+Break for this process's group, which a supervisor the
    /// session host started leads (CREATE_NEW_PROCESS_GROUP) and its agent
    /// shares with every tool it started, as a Unix pane's group does. The
    /// supervisor's own handler records it and keeps running, but the console
    /// runs that handler on a thread of its own, which may come after the agent
    /// has ended: the request is recorded as SIGTERM before the event goes out,
    /// so that the `--run` fallback sees the stop however soon the agent ends
    /// (spec D9). A process that leads no group reaches no process this way.
    pub fn break_process_group() -> io::Result<()> {
        let recorded = SHUTDOWN_SIGNAL
            .compare_exchange(0, SIGTERM, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        // SAFETY: plain call; the event goes to the processes on this
        // process's console whose group ID is this process's ID.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, std::process::id()) } == 0 {
            let error = io::Error::last_os_error();
            // Nothing went out, so nothing was asked to stop.
            if recorded {
                let _ = SHUTDOWN_SIGNAL.compare_exchange(
                    SIGTERM,
                    0,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// Start `command` outside this process's job, with a hidden console of its
    /// own (its console children never open a window) and its own process group.
    /// `prepare_environment`'s job allows that (spec D4); an enclosing job that
    /// forbids breakaway makes it fail.
    pub fn spawn_detached(command: &mut Command) -> io::Result<Child> {
        command
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn()
    }

    type Reader = thread::JoinHandle<io::Result<Vec<u8>>>;
    fn read_limited(pipe: Option<impl Read + Send + 'static>, limit: usize) -> Option<Reader> {
        let pipe = pipe?;
        Some(thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        }))
    }
    /// Run `command` without input in a job of its own (see `spawn_group`),
    /// collecting at most `limits` bytes of standard output and errors, until it
    /// exits and both streams end. When the output is too large, `deadline`
    /// passes, `cancelled` says so or the child fails, the whole job ends at
    /// once; whatever a successful child left running ends with the job when
    /// the call returns.
    pub fn run_bounded(
        command: &mut Command,
        limits: [usize; 2],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> std::result::Result<(ExitStatus, Vec<u8>, Vec<u8>), RunError> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_group(command).map_err(RunError::Spawn)?;
        let mut readers = [
            read_limited(child.stdout.take(), limits[0]),
            read_limited(child.stderr.take(), limits[1]),
        ];
        let mut outputs: [Option<Vec<u8>>; 2] = [None, None];
        let mut status = None;
        let result = loop {
            for (reader, output) in readers.iter_mut().zip(outputs.iter_mut()) {
                if reader.as_ref().is_some_and(|reader| reader.is_finished()) {
                    // A failed read leaves the stream unfinished, as on Unix: the
                    // run then ends at its deadline.
                    if let Ok(Ok(bytes)) = reader.take().unwrap().join() {
                        *output = Some(bytes);
                    }
                }
            }
            if outputs
                .iter()
                .zip(limits)
                .any(|(output, limit)| output.as_ref().is_some_and(|bytes| bytes.len() > limit))
            {
                break Err(RunError::TooLarge);
            }
            if status.is_none() {
                status = child.try_wait().map_err(RunError::Io)?;
            }
            if let Some(status) = status {
                if outputs.iter().all(Option::is_some) {
                    break Ok(status);
                }
            }
            if cancelled() || Instant::now() >= deadline {
                break Err(RunError::Stopped);
            }
            thread::sleep(Duration::from_millis(3));
        };
        if !result.as_ref().is_ok_and(ExitStatus::success) {
            child.terminate();
        }
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        let [out, err] = outputs;
        result.map(|status| (status, out.unwrap_or_default(), err.unwrap_or_default()))
    }
    /// `Command::output` with a time limit and closed input, in a job of its own
    /// (see `spawn_group`). Output goes to regular files, so a child writing much
    /// before it exits never blocks on a pipe and no reader thread outlives the
    /// call. The job ends at the limit, and with the call.
    pub fn output_until(
        command: &mut Command,
        duration: Duration,
    ) -> std::result::Result<Output, RunError> {
        let mut stdout = tempfile::tempfile().map_err(RunError::Io)?;
        let mut stderr = tempfile::tempfile().map_err(RunError::Io)?;
        command.stdout(Stdio::from(stdout.try_clone().map_err(RunError::Io)?));
        command.stderr(Stdio::from(stderr.try_clone().map_err(RunError::Io)?));
        command.stdin(Stdio::null());
        let mut child = spawn_group(command).map_err(RunError::Spawn)?;
        let deadline = Instant::now() + duration;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(RunError::Io)? {
                break status;
            }
            if Instant::now() >= deadline {
                child.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Stopped);
            }
            thread::sleep(Duration::from_millis(25));
        };
        drop(child);
        stdout.seek(SeekFrom::Start(0)).map_err(RunError::Io)?;
        stderr.seek(SeekFrom::Start(0)).map_err(RunError::Io)?;
        let mut output = Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        stdout
            .read_to_end(&mut output.stdout)
            .map_err(RunError::Io)?;
        stderr
            .read_to_end(&mut output.stderr)
            .map_err(RunError::Io)?;
        Ok(output)
    }

    /// Keep Git for Windows from asking for credentials. A set but empty
    /// GIT_ASKPASS also overrides core.askpass and SSH_ASKPASS, Git Credential
    /// Manager must not open a window, and the ssh command runs in Git's own
    /// shell, whose `ssh` is Git's bundled client (System32 OpenSSH hangs on the
    /// synchronous pipes Git gives it). To be confirmed on a Windows computer.
    pub fn git_noninteractive(command: &mut Command) -> &mut Command {
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .env("GCM_INTERACTIVE", "never")
            .env(
                "GIT_SSH_COMMAND",
                "ssh -o BatchMode=yes -o ConnectTimeout=2",
            )
    }

    /// The modification time as Unix seconds and the nanoseconds within that second.
    pub fn modified(metadata: &fs::Metadata) -> (i64, i64) {
        unix_time(metadata.last_write_time())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_of_a_process_without_a_console_gets_a_hidden_one() {
        // A process with a console shares it (an interactive ssh, a terminal command).
        assert_eq!(console_flags(0, true), 0);
        assert_eq!(
            console_flags(CREATE_NEW_PROCESS_GROUP, true),
            CREATE_NEW_PROCESS_GROUP
        );
        // The desktop service has none: its children get no console window.
        assert_eq!(console_flags(0, false), CREATE_NO_WINDOW);
        assert_eq!(
            console_flags(CREATE_NEW_PROCESS_GROUP, false),
            CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        );
        // An explicit console choice stays the caller's.
        for chosen in [CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, DETACHED_PROCESS] {
            assert_eq!(console_flags(chosen, false), chosen);
        }
    }

    #[test]
    fn a_released_private_console_leaves_no_stale_standard_handles() {
        use std::{io::Write, os::windows::io::IntoRawHandle};
        use windows_sys::Win32::System::Console::AllocConsole;
        const CHILD: &str = "HGS_TEST_PRIVATE_CONSOLE_CHILD";
        if let Some(folder) = env::var_os(CHILD) {
            // A console this process alone uses, as Windows gives a logon task
            // before 24H2, with the standard handles naming it.
            unsafe { FreeConsole() };
            assert_ne!(
                unsafe { AllocConsole() },
                0,
                "{}",
                io::Error::last_os_error()
            );
            let open = |name: &str| {
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(name)
                    .unwrap()
                    .into_raw_handle()
            };
            let (input, output) = (open("CONIN$"), open("CONOUT$"));
            for (which, handle) in [
                (STD_INPUT_HANDLE, input),
                (STD_OUTPUT_HANDLE, output),
                (STD_ERROR_HANDLE, output),
            ] {
                assert_ne!(unsafe { SetStdHandle(which, handle) }, 0);
            }
            assert!(release_private_console());
            for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                assert!(
                    unsafe { GetStdHandle(which) }.is_null(),
                    "stale standard handle {which}"
                );
            }
            // Files opened afterwards may get the old numbers; output never reaches them.
            let folder = PathBuf::from(folder);
            let files: Vec<fs::File> = (0..16)
                .map(|n| fs::File::create(folder.join(format!("{n}.db"))).unwrap())
                .collect();
            println!("stale standard output");
            eprintln!("stale standard error");
            let _ = io::stdout().flush();
            drop(files);
            for n in 0..16 {
                assert_eq!(
                    fs::metadata(folder.join(format!("{n}.db"))).unwrap().len(),
                    0
                );
            }
            return;
        }
        let folder = tempfile::tempdir().unwrap();
        let status = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::windows::tests::a_released_private_console_leaves_no_stale_standard_handles",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, folder.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
        for n in 0..16 {
            assert_eq!(
                fs::metadata(folder.path().join(format!("{n}.db")))
                    .unwrap()
                    .len(),
                0
            );
        }
    }

    #[test]
    fn path_gains_the_stable_cli_and_agent_installer_folders_once() {
        let path = |folders: &[&str]| env::join_paths(folders).unwrap();
        let bin = PathBuf::from(r"C:\Users\a\.local\bin");
        let npm = PathBuf::from(r"C:\Users\a\AppData\Roaming\npm");
        let kimi = PathBuf::from(r"C:\Users\a\.kimi-code\bin");
        assert_eq!(
            prepared_path(
                &path(&[r"C:\Windows"]),
                Some(bin.clone()),
                &[npm.clone(), kimi.clone()]
            ),
            Some(path(&[
                r"C:\Users\a\.local\bin",
                r"C:\Windows",
                r"C:\Users\a\AppData\Roaming\npm",
                r"C:\Users\a\.kimi-code\bin"
            ]))
        );
        // Listed already, in another case or with a trailing separator.
        assert_eq!(
            prepared_path(
                &path(&[
                    r"c:\users\a\.local\bin\",
                    r"C:\Windows",
                    r"C:\Users\A\AppData\Roaming\npm",
                    r"C:\USERS\a\.kimi-code\bin\"
                ]),
                Some(bin.clone()),
                &[npm.clone(), kimi.clone()]
            ),
            None
        );
        assert_eq!(
            prepared_path(&path(&[r"C:\Windows"]), None, std::slice::from_ref(&npm)),
            Some(path(&[r"C:\Windows", r"C:\Users\a\AppData\Roaming\npm"]))
        );
        assert_eq!(
            prepared_path(
                &path(&[r"C:\Windows", r"C:\Users\a\AppData\Roaming\npm"]),
                None,
                &[npm, kimi]
            ),
            Some(path(&[
                r"C:\Windows",
                r"C:\Users\a\AppData\Roaming\npm",
                r"C:\Users\a\.kimi-code\bin"
            ]))
        );
        assert_eq!(prepared_path(&path(&[r"C:\Windows"]), None, &[]), None);
    }

    #[test]
    fn kimi_code_is_looked_for_below_the_user_profile() {
        assert_eq!(
            kimi_folder(Some(r"C:\Users\a".into())),
            Some(PathBuf::from(r"C:\Users\a\.kimi-code\bin"))
        );
        assert_eq!(kimi_folder(Some(String::new())), None);
        assert_eq!(kimi_folder(None), None);
    }

    #[test]
    fn sessions_name_only_the_stable_cli() {
        let folder = tempfile::tempdir().unwrap();
        let stable = folder.path().join(r".local\bin\hgs.exe");
        let error = stable_executable_in(&stable, None).unwrap_err();
        assert!(error.contains("hgs session-host install"), "{error}");
        fs::create_dir_all(stable.parent().unwrap()).unwrap();
        fs::write(&stable, b"").unwrap();
        let canonical_stable = canonical(&stable).unwrap();
        assert_eq!(
            stable_executable_in(&stable, None).unwrap(),
            canonical_stable
        );
        // HGS_EXECUTABLE may name it in any case, never another file.
        let upper = OsString::from(stable.to_string_lossy().to_uppercase());
        assert_eq!(
            stable_executable_in(&stable, Some(upper)).unwrap(),
            canonical_stable
        );
        assert_eq!(
            stable_executable_in(&stable, Some(OsString::new())).unwrap(),
            canonical_stable
        );
        let portable = folder.path().join(r"Zerus\hgs.exe");
        fs::create_dir_all(portable.parent().unwrap()).unwrap();
        fs::write(&portable, b"").unwrap();
        for explicit in [portable.into_os_string(), OsString::from(r"hgs.exe")] {
            let error = stable_executable_in(&stable, Some(explicit)).unwrap_err();
            assert!(error.contains("only the stable hgs CLI"), "{error}");
        }
    }

    #[test]
    fn agents_run_directly_or_through_their_npm_shim() {
        let folder = tempfile::tempdir().unwrap();
        let npm = folder.path();
        let program = npm.join("agent.exe");
        fs::write(&program, b"").unwrap();
        let command = agent_command(&[program.display().to_string(), "--flag".into()]).unwrap();
        assert_eq!(command.get_program(), program.as_os_str());
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--flag"]);

        let script = npm.join(r"node_modules\@scope\agent\cli.js");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, b"").unwrap();
        fs::write(npm.join("node.exe"), b"").unwrap();
        let shim = npm.join("agent.cmd");
        fs::write(
            &shim,
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\n\
             SETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  \
             SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  \
             SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || \
             title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@scope\\agent\\cli.js\" %*\r\n",
        )
        .unwrap();
        let command =
            agent_command(&[shim.display().to_string(), "line one\nline two".into()]).unwrap();
        assert_eq!(command.get_program(), npm.join("node.exe").as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [script.as_os_str(), OsStr::new("line one\nline two")]
        );

        let wrapper = npm.join("wrapper.cmd");
        fs::write(&wrapper, "@echo off\r\nnode \"%~dp0\\cli.js\" %*\r\n").unwrap();
        let error = agent_command(&[wrapper.display().to_string()]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            error.to_string().contains("never through cmd.exe"),
            "{error}"
        );
        let missing = npm.join("missing.exe");
        assert_eq!(
            agent_command(&[missing.display().to_string()])
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn home_prefers_a_set_home_over_the_profile() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(
            home_from(some(r"C:\msys"), some(r"C:\Users\a")).as_deref(),
            Some(r"C:\msys")
        );
        assert_eq!(
            home_from(some(""), some(r"C:\Users\a")).as_deref(),
            Some(r"C:\Users\a")
        );
        assert_eq!(
            home_from(None, some(r"C:\Users\a")).as_deref(),
            Some(r"C:\Users\a")
        );
        assert_eq!(home_from(None, some("")), None);
        assert_eq!(
            below(r"C:\Users\a b", ".config/hgs"),
            r"C:\Users\a b\.config\hgs"
        );
        assert_eq!(
            below(r"C:\Users\a\", ".local/state"),
            r"C:\Users\a\.local\state"
        );
    }

    #[test]
    fn pathext_decides_what_runs() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["tool.EXE", "script.cmd", "plain", "notes.txt"] {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        assert!(executable(dir.path().join("tool.EXE")));
        assert!(executable(dir.path().join("script.cmd")));
        assert!(!executable(dir.path().join("plain")));
        assert!(!executable(dir.path().join("notes.txt")));
        assert!(candidates(dir.path(), "tool").contains(&dir.path().join("tool.exe")));
        assert_eq!(
            candidates(dir.path(), "tool.exe"),
            [dir.path().join("tool.exe")]
        );
        assert!(is_path(r"C:\tools\ssh.exe") && is_path("./ssh") && !is_path("ssh"));
    }

    #[test]
    fn only_consoles_and_absent_streams_reach_a_child_unrelayed() {
        use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_UNKNOWN;
        // A console: `ssh -t` and Ctrl+C need the handle itself.
        assert!(!relayed(FILE_TYPE_CHAR, true));
        // NUL or a serial port, a redirected file (`< in`, `>> log`) and any pipe.
        for file_type in [FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE] {
            assert!(relayed(file_type, false), "{file_type}");
        }
        // No standard handle at all, or an invalid one.
        assert!(!relayed(FILE_TYPE_UNKNOWN, false));

        let dir = tempfile::tempdir().unwrap();
        let file = fs::File::create(dir.path().join("log")).unwrap();
        let nul = fs::File::open("NUL").unwrap();
        let (ours, theirs) = overlapped_pipe(true).unwrap();
        for handle in [&file, &nul, &ours, &theirs] {
            assert!(needs_relay(handle.as_raw_handle()));
        }
        assert!(!needs_relay(std::ptr::null_mut()));
        assert!(!needs_relay(INVALID_HANDLE_VALUE as RawHandle));
        // A console, when this test runs with one.
        if let Ok(console) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONOUT$")
        {
            assert!(!needs_relay(console.as_raw_handle()));
        }
    }

    #[test]
    fn processes_and_files_have_stable_identities() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
        assert!(!process_alive(u32::MAX));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"one").unwrap();
        let first = file_identity(&path).unwrap();
        assert_eq!(file_identity(&path), Some(first));
        let next = dir.path().join("next");
        fs::write(&next, b"two").unwrap();
        fs::remove_file(&path).unwrap();
        fs::rename(&next, &path).unwrap();
        assert_ne!(file_identity(&path).unwrap().inode, first.inode);
    }

    #[test]
    fn link_markers_survive_command_characters_in_the_temporary_path() {
        let dir = tempfile::tempdir().unwrap();
        // A profile name may contain `%`; OpenSSH and cmd.exe both expand it.
        let folder = dir.path().join("50%off %OS% a&b ^c");
        fs::create_dir(&folder).unwrap();
        let marker = folder.join("up");
        let mut ssh = Command::new("ssh");
        let command = create_file_command(&mut ssh, &marker);
        assert_eq!(command, r#"type nul > "%%HGS_LINK_MARKER%%""#);
        let (_, value) = ssh
            .get_envs()
            .find(|(name, _)| *name == LOCAL_COMMAND_PATH)
            .unwrap();
        assert_eq!(value, Some(marker.as_os_str()));
        // OpenSSH turns `%%` into `%` and runs the result with `cmd.exe /c`,
        // which inherits the client's environment.
        let status = Command::new("cmd.exe")
            .arg("/c")
            .raw_arg(command.replace("%%", "%"))
            .env(LOCAL_COMMAND_PATH, &marker)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(marker.is_file());
    }

    #[test]
    fn link_markers_of_gone_clients_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        // A client that has exited, as one the desktop app killed.
        let mut gone = Command::new("cmd.exe")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let stale = dir
            .path()
            .join(format!("{LINK_MARKER_PREFIX}{gone_pid}-AbC123"));
        let live = dir
            .path()
            .join(format!("{LINK_MARKER_PREFIX}{}-XyZ789", std::process::id()));
        let unrelated = dir.path().join("hgs-linkage");
        for folder in [&stale, &live, &unrelated] {
            fs::create_dir(folder).unwrap();
            fs::write(folder.join("up"), b"").unwrap();
        }
        remove_stale_link_markers(dir.path());
        assert!(!stale.exists());
        assert!(live.join("up").is_file());
        assert!(unrelated.join("up").is_file());
    }

    #[test]
    fn a_closing_console_removes_the_link_marker() {
        let marker = link_marker_dir().unwrap();
        let name = marker
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(&format!("{LINK_MARKER_PREFIX}{}-", std::process::id())));
        fs::write(marker.path().join("up"), b"").unwrap();
        // Ctrl+C belongs to ssh.exe; the marker stays.
        assert_eq!(unsafe { interrupted(CTRL_C_EVENT) }, 1);
        assert!(marker.path().join("up").is_file());
        INTERRUPTED.store(false, Ordering::Relaxed);
        // The console closes: the process ends after the handler returns.
        assert_eq!(unsafe { interrupted(CTRL_CLOSE_EVENT) }, 0);
        assert!(!marker.path().exists());
    }

    #[test]
    fn private_files_stay_writable_and_directories_need_no_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.json");
        let file = private_open(fs::OpenOptions::new().create_new(true).write(true))
            .open(&path)
            .unwrap();
        make_private(&file).unwrap();
        restrict_dir(dir.path()).unwrap();
        sync_dir(dir.path()).unwrap();
        assert!(!fs::metadata(&path).unwrap().permissions().readonly());
    }

    #[test]
    fn replacement_waits_for_a_reader_without_delete_sharing() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("catalog.json");
        fs::write(&target, b"old").unwrap();
        // FILE_SHARE_READ only, as some readers and scanners open files.
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(reader);
        });
        let mut temporary = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        temporary.write_all(b"new").unwrap();
        super::super::persist(temporary, &target).unwrap();
        release.join().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");

        // A holder that never lets go fails within the bounded wait.
        let _reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let mut temporary = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        temporary.write_all(b"blocked").unwrap();
        let started = std::time::Instant::now();
        assert!(super::super::persist(temporary, &target).is_err());
        // The retry budget is 1 s. This cap is deliberately loose: it flags a
        // refusal that returns far too late, without measuring runner load. It
        // proves neither the budget nor that a hung call would be caught here.
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn verbatim_prefixes_go_only_when_the_plain_path_names_the_same_file() {
        let plain = |text: &str| {
            process::plain_path(PathBuf::from(text))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(plain(r"\\?\C:\Users\a b\project"), r"C:\Users\a b\project");
        assert_eq!(plain(r"\\?\C:\"), r"C:\");
        assert_eq!(plain(r"\\?\UNC\server\share\dir"), r"\\server\share\dir");
        // Win32 would change or misread these, so they keep the prefix.
        for kept in [
            r"\\?\C:\dir\name.",
            r"\\?\C:\dir\name ",
            r"\\?\C:\NUL",
            r"\\?\C:\dir\com1.txt",
            r"\\?\C:\file:stream",
            r"\\?\Volume{0b7f}\dir",
            r"\\?\GLOBALROOT\Device\X",
        ] {
            assert_eq!(plain(kept), kept);
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(&long), long);
        assert_eq!(plain(r"C:\already\plain"), r"C:\already\plain");
        let dir = tempfile::tempdir().unwrap();
        let path = canonical(dir.path()).unwrap();
        assert!(!path.to_string_lossy().starts_with(r"\\?\"), "{path:?}");
        assert_eq!(
            fs::canonicalize(&path).unwrap(),
            fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn program_names_drop_runnable_extensions_and_case() {
        let name = |path: &str| program_name(Path::new(path));
        assert_eq!(
            name(r"C:\Program Files\nodejs\node.exe").as_deref(),
            Some("node")
        );
        assert_eq!(name("C:/npm/Codex.CMD").as_deref(), Some("codex"));
        // The default PATHEXT lists `.js` and `.vbs`; scripts keep their names.
        assert_eq!(name("codex.js").as_deref(), Some("codex.js"));
        assert_eq!(name("Setup.VBS").as_deref(), Some("setup.vbs"));
        assert_eq!(name("python3.12.exe").as_deref(), Some("python3.12"));
        assert_eq!(name(".exe").as_deref(), Some(".exe"));
    }

    #[test]
    fn crash_statuses_read_as_signals() {
        use std::os::windows::process::ExitStatusExt;
        let info = |raw: u32| ExitInfo::of(ExitStatus::from_raw(raw));
        assert_eq!(
            info(3),
            ExitInfo {
                code: Some(3),
                signal: None
            }
        );
        // exit(-1) is an ordinary failure, not a crash.
        assert_eq!(
            info(u32::MAX),
            ExitInfo {
                code: Some(-1),
                signal: None
            }
        );
        assert_eq!(
            info(0xC000_013A),
            ExitInfo {
                code: None,
                signal: Some(2)
            }
        );
        assert_eq!(info(0xC000_0005).signal, Some(11));
        assert_eq!(info(0xC000_0409).exit_code(), 134);
        // The --run fallback counts 128 and above as a crash, never as
        // "nothing to resume".
        assert_eq!(code(ExitStatus::from_raw(0xC000_013A)), 130);
        assert_eq!(code(ExitStatus::from_raw(1)), 1);
    }

    #[test]
    fn processes_have_identities_parents_and_images() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let own = std::process::id();
        assert_eq!(presence(own), Presence::Present);
        assert_eq!(presence(0), Presence::Unknown);
        let start = process_start_time(own).unwrap();
        assert!(
            !start.is_empty() && start.bytes().all(|b| b.is_ascii_digit()),
            "{start}"
        );
        assert_eq!(process_start_fallback(own), None);
        let identity = process_identity(own).unwrap();
        assert_eq!(identity.birth, start);
        assert_eq!(identity.group, None);
        assert_eq!(identity.parent, parent_process_id());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(identity.started > now - 86_400. && identity.started <= now + 1.);
        assert_eq!(process_table().unwrap()[&own].group, None);
        let image = describe(own).unwrap();
        assert_eq!(
            fs::canonicalize(&image.executable).unwrap(),
            fs::canonicalize(env::current_exe().unwrap()).unwrap()
        );
        assert_eq!(image.argv, Some(env::args().collect::<Vec<_>>()));

        let mut child = Command::new("cmd.exe")
            .args(["/c", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let (parent, name) = parent_and_name(child.id()).unwrap();
        let (argv_parent, argv) = parent_and_argv(child.id()).unwrap();
        drop(child.stdin.take());
        child.wait().unwrap();
        assert_eq!(parent, Some(own));
        assert_eq!(
            name.and_then(|name| program_name(Path::new(&name)))
                .as_deref(),
            Some("cmd")
        );
        assert_eq!(argv_parent, own);
        assert_eq!(argv[1..], ["/c", "pause"]);
        // `child` still holds the exited process's handle: it counts as gone.
        assert_eq!(process_start_time(child.id()).as_deref(), Some(""));
        assert_eq!(presence(child.id()), Presence::Gone);
        assert!(process_identity(child.id()).is_none());
        assert!(matches!(current_directories(), Err(error) if !error.is_empty()));
        assert!(matches!(open_files(own), Err(RunError::Unsupported)));
    }

    #[test]
    fn pinned_processes_end_only_their_identity() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let birth = process_identity(child.id()).unwrap().birth;
        assert!(PinnedProcess::pin(child.id(), "1").is_err());
        let pinned = PinnedProcess::pin(child.id(), &birth).unwrap();
        assert!(pinned.alive());
        // No polite request reaches an arbitrary process; the caller then kills.
        pinned.terminate().unwrap();
        assert!(pinned.alive());
        let error = request_exit(child.id()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(pinned.alive());
        pinned.kill().unwrap();
        child.wait().unwrap();
        assert!(!pinned.alive());
        pinned.kill().unwrap();
    }

    #[test]
    fn restart_manager_names_the_holder_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let other = dir.path().join("other.jsonl");
        fs::write(&path, b"{}\n").unwrap();
        fs::write(&other, b"{}\n").unwrap();
        // The child holds the file as its standard input while it runs.
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(fs::File::open(&path).unwrap())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let held = holds_open(child.id(), &path);
        let unrelated = holds_open(child.id(), &other);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(held.unwrap());
        assert!(!unrelated.unwrap());
    }

    #[test]
    fn bounded_runs_end_their_whole_job() {
        let never = || false;
        let far = Instant::now() + Duration::from_secs(30);
        let (status, out, err) = run_bounded(
            Command::new("cmd.exe").raw_arg("/c echo out& echo err 1>&2& exit 3"),
            [64, 64],
            far,
            &never,
        )
        .unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&err).trim(), "err");
        assert!(matches!(
            run_bounded(
                Command::new("cmd.exe").raw_arg("/c echo 12345"),
                [4, 64],
                far,
                &never
            ),
            Err(RunError::TooLarge)
        ));
        assert!(matches!(
            run_bounded(
                &mut Command::new("hgs-no-such-program"),
                [4, 4],
                far,
                &never
            ),
            Err(RunError::Spawn(_))
        ));
        assert!(matches!(
            run_bounded(
                Command::new("ping").args(["-n", "30", "127.0.0.1"]),
                [4096, 4096],
                far,
                &|| true
            ),
            Err(RunError::Stopped)
        ));
        // A helper that outlives the shell holds the output open; the deadline
        // ends the whole job.
        let started = Instant::now();
        let result = run_bounded(
            Command::new("cmd.exe").raw_arg("/c start /b ping -n 60 127.0.0.1 & exit 0"),
            [8192, 8192],
            Instant::now() + Duration::from_millis(500),
            &never,
        );
        assert!(matches!(result, Err(RunError::Stopped)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(20));

        let output = output_until(
            Command::new("cmd.exe").raw_arg("/c echo out"),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "out");
        assert!(matches!(
            output_until(
                Command::new("ping").args(["-n", "30", "127.0.0.1"]),
                Duration::from_millis(200)
            ),
            Err(RunError::Stopped)
        ));
    }

    #[test]
    fn private_entries_need_their_kind_and_one_link() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("store");
        private_dir(&folder).unwrap();
        let file = folder.join("data");
        fs::write(&file, b"x").unwrap();
        // The file inherits the folder's owner and ACL, so both answers agree.
        let private = verify_private(&folder, Private::Directory).unwrap();
        assert_eq!(verify_private(&file, Private::File).unwrap(), private);
        assert_eq!(
            verify_private_file(&fs::File::open(&file).unwrap()).unwrap(),
            private
        );
        assert!(!verify_private(&folder, Private::File).unwrap());
        assert!(!verify_private(&file, Private::Directory).unwrap());
        fs::hard_link(&file, folder.join("second")).unwrap();
        assert!(!verify_private(&file, Private::File).unwrap());
        assert!(verify_private(&folder.join("missing"), Private::File).is_err());
    }

    #[test]
    fn a_directory_has_an_identity_like_a_file() {
        // Directories open only with backup semantics; the search catalog stamps them.
        let dir = tempfile::tempdir().unwrap();
        let (identity, metadata) = FileIdentity::at(dir.path()).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(file_identity(dir.path()), Some(identity));
        fs::write(dir.path().join("file"), b"x").unwrap();
        assert_eq!(
            FileIdentity::at(dir.path()).unwrap().0.inode,
            identity.inode
        );
    }

    #[test]
    fn file_identities_carry_unix_time_with_nanoseconds() {
        use std::time::UNIX_EPOCH;
        assert_eq!(unix_time(UNIX_EPOCH_TICKS), (0, 0));
        assert_eq!(unix_time(UNIX_EPOCH_TICKS + 15_000_001), (1, 500_000_100));
        assert_eq!(unix_time(UNIX_EPOCH_TICKS - 1), (-1, 999_999_900));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"x").unwrap();
        let (identity, metadata) = FileIdentity::at(&path).unwrap();
        assert_eq!(file_identity(&path), Some(identity));
        assert_eq!(
            (identity.modified, identity.modified_nanos),
            modified(&metadata)
        );
        let since = metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap();
        assert_eq!(identity.modified, since.as_secs() as i64);
        assert_eq!(identity.modified_nanos, i64::from(since.subsec_nanos()));
    }

    #[test]
    fn supervisor_console_events_record_stop_requests() {
        let guard = ShutdownGuard::install().unwrap();
        let event = |kind| unsafe { process::shutdown_event(kind) };
        // Ctrl+C belongs to the agent; logoff and shutdown name no session.
        for kind in [CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            assert_eq!(event(kind), 1);
        }
        assert_eq!(shutdown_signal(), 0);
        assert_eq!(event(CTRL_BREAK_EVENT), 1);
        assert_eq!(shutdown_signal(), 15);
        drop(guard);
        // Once the outcome is recorded, a closing console waits no longer.
        let started = Instant::now();
        assert_eq!(event(CTRL_CLOSE_EVENT), 1);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(shutdown_signal(), 1);
    }

    #[test]
    fn git_never_prompts_on_windows() {
        let mut git = Command::new("git");
        git_noninteractive(&mut git);
        let environment: Vec<_> = git
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.and_then(|v| v.to_str())))
            .collect();
        assert_eq!(
            environment,
            [
                ("GCM_INTERACTIVE", Some("never")),
                ("GIT_ASKPASS", Some("")),
                (
                    "GIT_SSH_COMMAND",
                    Some("ssh -o BatchMode=yes -o ConnectTimeout=2")
                ),
                ("GIT_TERMINAL_PROMPT", Some("0")),
                ("SSH_ASKPASS", Some("")),
            ]
        );
    }

    /// Where a probe below reports, when its parent test runs it.
    const PROBE_REPORT: &str = "HGS_TEST_JOB_PROBE_REPORT";

    /// This test binary running the ignored test `name`, reporting to `report`.
    fn probe_command(name: &str, report: &Path) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([name, "--exact", "--ignored", "--test-threads=1"])
            .env(PROBE_REPORT, report)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// The lines of a probe's report once it is complete.
    fn report_of(report: &Path) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = fs::read_to_string(report).unwrap_or_default();
            if text.ends_with("done\n") {
                return text.lines().map(str::to_owned).collect();
            }
            assert!(
                Instant::now() < deadline,
                "no report from the probe: {text:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The value after `key=` in a report.
    fn value<'a>(lines: &'a [String], key: &str) -> &'a str {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("no {key} in {lines:?}"))
    }

    fn ping() -> Command {
        let mut command = Command::new("ping");
        command
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// A process to watch and, if needed, end.
    fn open(pid: &str) -> std::os::windows::io::OwnedHandle {
        use std::os::windows::io::OwnedHandle;
        let pid: u32 = pid.parse().unwrap();
        // SAFETY: plain call; a returned handle is owned below.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!handle.is_null(), "{pid}: {}", io::Error::last_os_error());
        // SAFETY: OpenProcess returned a handle that nothing else owns.
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }

    /// Whether the process has ended within `wait`.
    fn ended(process: &std::os::windows::io::OwnedHandle, wait: Duration) -> bool {
        let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the handle is open with SYNCHRONIZE access.
        unsafe { WaitForSingleObject(process.as_raw_handle(), millis) == WAIT_OBJECT_0 }
    }

    /// Run in its own job by `a_bounded_child_dies_with_its_job_and_cannot_break_away`.
    #[test]
    #[ignore = "run by a_bounded_child_dies_with_its_job_and_cannot_break_away"]
    // Its children run on past it on purpose; Windows keeps no zombies.
    #[allow(clippy::zombie_processes)]
    fn contained_child_probe() {
        let Some(report) = env::var_os(PROBE_REPORT) else {
            return;
        };
        let allows = crate::host::job::current_job_allows_breakaway().unwrap();
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/d", "/c", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let breakaway = match spawn_detached(&mut cmd) {
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                "denied".to_owned()
            }
            Err(error) => error.to_string(),
            Ok(mut child) => {
                let _ = child.wait();
                "allowed".to_owned()
            }
        };
        // Started the plain way: it lands in this process's job.
        let helper = ping().spawn().unwrap();
        fs::write(
            report,
            format!(
                "allows={allows:?}\nbreakaway={breakaway}\nhelper={}\ndone\n",
                helper.id()
            ),
        )
        .unwrap();
        // The parent ends this, and the helper with it.
        thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn a_bounded_child_dies_with_its_job_and_cannot_break_away() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        let child = spawn(&mut probe_command(
            "platform::windows::tests::contained_child_probe",
            &report,
        ))
        .unwrap();
        let lines = report_of(&report);
        // Its immediate job forbids breakaway, so nothing it starts can leave.
        assert_eq!(value(&lines, "allows"), "Some(false)");
        assert_eq!(value(&lines, "breakaway"), "denied");
        let probe = open(&child.id().to_string());
        let helper = open(value(&lines, "helper"));
        assert!(!ended(&probe, Duration::ZERO) && !ended(&helper, Duration::ZERO));
        // Dropping the child closes its job: the child and its helper end.
        drop(child);
        assert!(ended(&probe, Duration::from_secs(10)));
        assert!(ended(&helper, Duration::from_secs(10)));
    }

    /// Run inside a job that allows breakaway by
    /// `the_cli_job_lets_only_a_detached_child_leave`, as hgs.exe runs.
    #[test]
    #[ignore = "run by the_cli_job_lets_only_a_detached_child_leave"]
    // Its children run on past it on purpose; Windows keeps no zombies.
    #[allow(clippy::zombie_processes)]
    fn cli_job_probe() {
        let Some(report) = env::var_os(PROBE_REPORT) else {
            return;
        };
        let report = PathBuf::from(report);
        prepare_environment();
        let allows = crate::host::job::current_job_allows_breakaway().unwrap();
        let detached = spawn_detached(&mut ping()).unwrap();
        let contained = spawn(&mut ping()).unwrap();
        fs::write(
            &report,
            format!(
                "allows={allows:?}\ndetached={}\ncontained={}\ndone\n",
                detached.id(),
                contained.id()
            ),
        )
        .unwrap();
        // Exit once the parent has looked; the contained child's job closes then.
        let go = report.with_extension("exit");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !go.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        std::mem::forget(contained);
    }

    #[test]
    fn the_cli_job_lets_only_a_detached_child_leave() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        // Stands in for whatever runs hgs.exe (a console, the GUI, sshd); it lets
        // a child break away and ends what is left in it when it closes.
        let outer = Job::new(Limits::CLI).unwrap();
        let mut command = probe_command("platform::windows::tests::cli_job_probe", &report);
        let mut child = command.creation_flags(CREATE_SUSPENDED).spawn().unwrap();
        if let Err(error) = outer
            .assign(child.as_handle())
            .and_then(|()| resume(child.id()))
        {
            // Never leave a suspended process behind.
            let _ = child.kill();
            panic!("cannot start the probe in its job: {error}");
        }
        let lines = report_of(&report);
        assert_eq!(value(&lines, "allows"), "Some(true)");
        let detached = open(value(&lines, "detached"));
        let contained = open(value(&lines, "contained"));
        assert!(!outer.contains(detached.as_handle()).unwrap());
        assert!(outer.contains(contained.as_handle()).unwrap());

        fs::write(report.with_extension("exit"), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the probe did not exit");
            thread::sleep(Duration::from_millis(20));
        }
        // The contained child ended with the probe's job for it; the detached
        // one runs on after both jobs closed.
        assert!(ended(&contained, Duration::from_secs(10)));
        drop(outer);
        assert!(!ended(&detached, Duration::from_millis(500)));
        // SAFETY: the handle is open with PROCESS_TERMINATE access.
        unsafe { TerminateProcess(detached.as_raw_handle(), 1) };
        assert!(ended(&detached, Duration::from_secs(10)));
    }

    #[test]
    fn contained_helpers_keep_the_std_contract() {
        let output =
            output(Command::new("cmd.exe").raw_arg("/d /c echo out& echo err 1>&2& exit 4"))
                .unwrap();
        assert_eq!(output.status.code(), Some(4));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "err");
        let status = status(
            Command::new("cmd.exe")
                .args(["/d", "/c", "exit", "3"])
                .stdout(Stdio::null()),
        )
        .unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(spawn(&mut Command::new("hgs-no-such-program")).is_err());
        // A command can be started again after a contained start.
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c", "exit", "0"])
            .stdout(Stdio::null());
        assert!(spawn_group(&mut command).unwrap().wait().unwrap().success());
        assert!(command.status().unwrap().success());
    }
}
