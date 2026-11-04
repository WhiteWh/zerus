//! Unix (Linux and macOS) process, terminal, clipboard and file primitives.
use super::{
    available, capture, silent, ExitInfo, FileIdentity, Presence, Private, PrivateRepair, ProcessIdentity,
    ProcessImage, ProcessRow, RunError,
};
use crate::{
    cli::{Error, Result},
    config::Config,
};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicI32, Ordering},
        Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

pub fn prepare_environment() {
    let home = env::var("HOME").unwrap_or_default();
    env::set_var(
        "PATH",
        prepared_path(&home, &env::var("PATH").unwrap_or_default()),
    );
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(|key| env::var(key).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !locale.contains("UTF-8") && !locale.contains("UTF8") {
        let locale = env::var("HGS_LANG").unwrap_or_else(|_| "en_US.UTF-8".into());
        env::set_var("LANG", &locale);
        env::set_var("LC_CTYPE", locale);
        env::remove_var("LC_ALL");
    }
}
/// The PATH agents and their tools are found on: user and package-manager
/// folders first, then the caller's PATH, then user-level installer folders
/// last (npm's global prefix, Kimi Code's and OpenCode's official installers),
/// so a program found earlier keeps winning. A managed GUI may run hgs with
/// only the system folders, where OpenCode's own install is not listed.
fn prepared_path(home: &str, path: &str) -> String {
    format!(
        "{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:{path}:{home}/.npm-global/bin:{home}/.kimi-code/bin:{home}/.opencode/bin"
    )
}
/// Whether this computer runs agent sessions: Unix always does.
pub fn local_sessions() -> bool {
    true
}
pub fn code(status: ExitStatus) -> i32 {
    ExitInfo::of(status).exit_code()
}
pub(super) fn exit_info(status: ExitStatus) -> ExitInfo {
    ExitInfo {
        code: status.code(),
        signal: status.signal(),
    }
}
pub fn executable(path: impl AsRef<Path>) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
pub(super) fn is_path(program: &str) -> bool {
    program.contains('/')
}
pub(super) fn candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    vec![dir.join(program)]
}
pub fn is_macos() -> bool {
    capture("uname", &[]).is_some_and(|name| name.trim() == "Darwin")
}
pub(super) fn default_ssh() -> OsString {
    "ssh".into()
}
pub fn spawn_ssh(
    command: &mut Command,
    stdin: bool,
    stdout: bool,
    stderr: bool,
) -> io::Result<super::SshChild> {
    for (piped, set) in [
        (
            stdin,
            Command::stdin as fn(&mut Command, Stdio) -> &mut Command,
        ),
        (stdout, Command::stdout),
        (stderr, Command::stderr),
    ] {
        if piped {
            set(command, Stdio::piped());
        }
    }
    let mut child = command.spawn()?;
    Ok(super::SshChild {
        stdin: child.stdin.take().map(|pipe| Box::new(pipe) as _),
        stdout: child.stdout.take().map(|pipe| Box::new(pipe) as _),
        stderr: child.stderr.take().map(|pipe| Box::new(pipe) as _),
        child,
    })
}
pub fn ssh_output(command: &mut Command) -> io::Result<Output> {
    command.output()
}
pub fn ssh_status(command: &mut Command, input: fs::File) -> io::Result<ExitStatus> {
    command.stdin(input).status()
}
pub fn home() -> Option<String> {
    env::var("HOME").ok()
}
/// HOME byte for byte, which need not be UTF-8.
pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}
/// The canonical form of `path`, as stored and compared in state files.
pub fn canonical(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}
/// The name a program goes by in process names and command lines: the file name.
pub fn program_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}
/// `relative`, written with '/', below `base`.
pub fn below(base: &str, relative: &str) -> String {
    format!("{base}/{relative}")
}
/// This machine's short host name, the default for HGS_SELF.
pub fn hostname() -> String {
    capture("hostname", &["-s"])
        .unwrap_or_default()
        .trim()
        .to_owned()
}
/// Owner-only access for a file holding private configuration.
pub fn make_private(file: &fs::File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}
/// Owner-only access for a directory holding private configuration.
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
/// Create a directory and its missing parents, each new one owner-only.
pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}
/// Create a directory and its missing parents, each new one owner-only, for a
/// store whose privacy is checked (#526: on Windows each gets a protected DACL).
pub fn owner_only_dir(path: &Path) -> io::Result<()> {
    private_dir(path)
}
/// Owner-only access for a file this process has just created.
pub fn owner_only_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
/// Whether `path` itself (a final symbolic link is not followed) is the
/// expected kind of entry, owned by this user, closed to everyone else and,
/// for a file, linked once.
pub fn verify_private(path: &Path, kind: Private) -> io::Result<bool> {
    Ok(private_metadata(&fs::symlink_metadata(path)?, kind))
}
/// `verify_private` for a file this process has open.
pub fn verify_private_file(file: &fs::File) -> io::Result<bool> {
    Ok(private_metadata(&file.metadata()?, Private::File))
}
/// Owner-only existing directory, checked through its pinned handle.
pub fn verify_private_directory(file: &fs::File) -> io::Result<bool> {
    Ok(private_metadata(&file.metadata()?, Private::Directory))
}
fn private_metadata(metadata: &fs::Metadata, kind: Private) -> bool {
    (match kind {
        Private::Directory => metadata.is_dir(),
        Private::File => metadata.is_file() && metadata.nlink() == 1,
    }) && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o077 == 0
}
/// #569: makes the entry at `path` private when it is this user's and only too
/// open: its mode becomes 0600 (a file) or 0700 (a folder), set through a
/// descriptor that never follows a link. Another owner, a link, another kind
/// of entry or a second link is refused and the entry is left as it is.
pub fn repair_private(path: &Path, kind: Private) -> io::Result<PrivateRepair> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(PrivateRepair::Refused("it is a symbolic link".into()));
    }
    match kind {
        Private::Directory if !metadata.is_dir() => return Ok(PrivateRepair::Refused("it is not a folder".into())),
        Private::File if !metadata.is_file() => return Ok(PrivateRepair::Refused("it is not a regular file".into())),
        Private::File if metadata.nlink() != 1 => {
            return Ok(PrivateRepair::Refused(format!("the file has {} links", metadata.nlink())));
        }
        _ => {}
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Ok(PrivateRepair::Refused(format!("it is owned by uid {}", metadata.uid())));
    }
    let mode = metadata.mode() & 0o7777;
    if mode & 0o077 == 0 {
        return Ok(PrivateRepair::Private);
    }
    let wanted = if kind == Private::Directory { 0o700 } else { 0o600 };
    let flags = libc::O_NOFOLLOW | libc::O_NONBLOCK | if kind == Private::Directory { libc::O_DIRECTORY } else { 0 };
    let entry = fs::OpenOptions::new().read(true).custom_flags(flags).open(path)?;
    // The entry opened is the one checked: never another one renamed into place.
    let opened = entry.metadata()?;
    if opened.ino() != metadata.ino() || opened.dev() != metadata.dev() {
        return Ok(PrivateRepair::Refused("it changed while it was checked".into()));
    }
    entry.set_permissions(fs::Permissions::from_mode(wanted))?;
    Ok(PrivateRepair::Repaired(vec![format!("mode {mode:04o} (now {wanted:04o})")]))
}
/// Create files owner-only.
pub fn private_open(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.mode(0o600)
}
/// Refuse a symbolic link as the final component and never block on a FIFO.
/// Sets both custom flags together; separate setters replace, rather than add flags.
pub fn no_follow(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
}
/// Refuse a symbolic link as the final component.
/// Replaces custom flags. Use `no_follow` when nonblocking mode is also required.
pub fn refuse_link(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.custom_flags(libc::O_NOFOLLOW)
}
/// Never block on a FIFO.
/// Replaces custom flags. Use `no_follow` when final links must also be refused.
pub fn never_block(options: &mut fs::OpenOptions) -> &mut fs::OpenOptions {
    options.custom_flags(libc::O_NONBLOCK)
}
/// Make a completed rename in `dir` durable.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}
pub(crate) fn transient_replace_error(_: &io::Error) -> bool {
    false
}
/// Run `command` in a process group of its own: the terminal's job control does
/// not reach it and `kill_process_group` stops everything it started.
pub fn own_process_group(command: &mut Command) -> &mut Command {
    command.process_group(0)
}
/// The command that starts an agent or untracked program, `argv` with its program
/// first: here the program as named, which the system finds on PATH. Windows
/// resolves it itself (spec D8).
pub fn agent_command(argv: &[String]) -> io::Result<Command> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing program"))?;
    let mut command = Command::new(program);
    command.args(arguments);
    Ok(command)
}
/// `Command::spawn`. On Windows the child also gets a job of its own that ends
/// with this process and that nothing it starts can leave (spec D4).
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    command.spawn()
}
/// `spawn` in a process group of its own (`own_process_group`).
pub fn spawn_group(command: &mut Command) -> io::Result<Child> {
    own_process_group(command).spawn()
}
/// On macOS std creates a child's pipes with pipe() and marks them close-on-exec
/// only afterwards, and posix_spawn passes on every descriptor without that flag.
/// A child another thread starts in between inherits those ends and holds the
/// first child's input or output open until it exits itself, so an exchange that
/// waits for end of file stalls with it. Threads that spawn at the same time
/// therefore take turns (`spawn_in_turn`), on every Unix alike.
static SPAWN: Mutex<()> = Mutex::new(());
/// The spawn turn: while it is held, no `spawn_in_turn` creates pipes or starts
/// a child.
pub fn spawn_turn() -> MutexGuard<'static, ()> {
    SPAWN.lock().unwrap_or_else(|e| e.into_inner())
}
/// `Command::spawn` in the spawn turn (`SPAWN`), for every spawn that can run
/// while another thread of this process spawns.
pub fn spawn_in_turn(command: &mut Command) -> io::Result<Child> {
    let _turn = spawn_turn();
    command.spawn()
}
/// Descriptor creation and spawn share the same turn on macOS, where setting
/// close-on-exec after socket/pipe creation would otherwise race another spawn.
/// Preserve the original deadline across turn contention and preparation.
pub fn spawn_prepared_in_turn_before<T>(
    until: std::time::Instant,
    prepare: impl FnOnce() -> io::Result<(Command, T)>,
) -> io::Result<(Child, T)> {
    let _turn = spawn_turn();
    if std::time::Instant::now() >= until {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let (mut command, resources) = prepare()?;
    if std::time::Instant::now() >= until {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok((command.spawn()?, resources))
}
/// `Command::status` through `spawn`.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    command.status()
}
/// `Command::output` through `spawn`.
pub fn output(command: &mut Command) -> io::Result<Output> {
    command.output()
}
/// Stop a child started with `own_process_group` and every process in its group.
/// Never signals the caller's own process group.
pub fn kill_process_group(child: &mut Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}
pub fn file_identity(path: &Path) -> Option<FileIdentity> {
    FileIdentity::at(path).ok().map(|(identity, _)| identity)
}
/// The identity of the file this process runs, read from the running image:
/// an upgrade may already have put another file at its path.
#[cfg(target_os = "linux")]
pub fn image_identity() -> Option<FileIdentity> {
    file_identity(Path::new("/proc/self/exe"))
}
/// The identity of the file this process runs, read from the running image:
/// an upgrade may already have put another file at its path.
#[cfg(target_os = "macos")]
pub fn image_identity() -> Option<FileIdentity> {
    // struct proc_regionwithpathinfo in <sys/proc_info.h>: a proc_regioninfo, of
    // which only pri_address and pri_size are read, then the file mapped there.
    #[repr(C)]
    struct Region {
        info: [u64; 12],
        vnode: libc::vnode_info_path,
    }
    const PROC_PIDREGIONPATHINFO: libc::c_int = 8;
    const ADDRESS: usize = 10;
    const SIZE: usize = 11;
    // PROC_PIDREGIONPATHINFO_SIZE and the offsets of the fields read, as in the SDK.
    const _: () = assert!(std::mem::size_of::<Region>() == 1272);
    const _: () = assert!(std::mem::offset_of!(Region, vnode.vip_vi.vi_stat.vst_dev) == 96);
    const _: () = assert!(std::mem::offset_of!(Region, vnode.vip_vi.vi_stat.vst_ino) == 104);
    const _: () = assert!(std::mem::offset_of!(Region, vnode.vip_vi.vi_stat.vst_mtime) == 136);
    const _: () = assert!(std::mem::offset_of!(Region, vnode.vip_vi.vi_stat.vst_mtimensec) == 144);
    // This function's own code lies in the executable's __TEXT segment.
    let address = image_identity as *const () as usize as u64;
    let mut region: Region = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&region) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            PROC_PIDREGIONPATHINFO,
            address,
            &mut region as *mut _ as *mut libc::c_void,
            size,
        )
    };
    // The region found contains `address`, or else is the next one.
    let start = region.info[ADDRESS];
    let mapped = (start..start.saturating_add(region.info[SIZE])).contains(&address);
    let stat = &region.vnode.vip_vi.vi_stat;
    if read != size || !mapped || stat.vst_ino == 0 {
        return None;
    }
    Some(FileIdentity {
        // stat(2) reports the same device as a signed dev_t.
        device: stat.vst_dev as i32 as u64,
        inode: stat.vst_ino,
        modified: stat.vst_mtime,
        modified_nanos: stat.vst_mtimensec,
    })
}
impl FileIdentity {
    /// The identity of a file this process has open. (Unix callers need its
    /// metadata as well and use `with_metadata`.)
    #[allow(dead_code)]
    pub fn of(file: &fs::File) -> io::Result<Self> {
        Ok(Self::with_metadata(file)?.0)
    }
    /// The identity and metadata of an open file, read at the same moment.
    pub fn with_metadata(file: &fs::File) -> io::Result<(Self, fs::Metadata)> {
        let metadata = file.metadata()?;
        Ok((Self::from_metadata(&metadata), metadata))
    }
    /// The identity and metadata of the file `path` names, following links.
    pub fn at(path: &Path) -> io::Result<(Self, fs::Metadata)> {
        let metadata = fs::metadata(path)?;
        Ok((Self::from_metadata(&metadata), metadata))
    }
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
        }
    }
}
/// The modification time as Unix seconds and the nanoseconds within that second.
pub fn modified(metadata: &fs::Metadata) -> (i64, i64) {
    (metadata.mtime(), metadata.mtime_nsec())
}

/// Signal 0 probes a process without touching it. EPERM means another user's
/// process, which is never treated as gone.
pub fn presence(pid: u32) -> Presence {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return Presence::Unknown;
    };
    if pid <= 0 {
        return Presence::Unknown;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        Presence::Present
    } else if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Presence::Gone
    } else {
        Presence::Unknown
    }
}

/// ps's own start time: `Some` with its (possibly empty) output when ps ran
/// successfully. For processes whose start `process_start_time` cannot read.
pub fn process_start_fallback(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(target_os = "linux")]
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let path = PathBuf::from(format!("/proc/{pid}"));
    if fs::metadata(&path).ok()?.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let stat = fs::read_to_string(path.join("stat")).ok()?;
    let (_, tail) = stat.rsplit_once(')')?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    if fields.len() < 20 || matches!(fields[0], "Z" | "X") {
        return None;
    }
    let started = process_started_at(pid)?? as f64;
    Some(ProcessIdentity {
        parent: fields[1].parse().ok()?,
        group: Some(fields[2].parse().ok()?),
        birth: fields[19].into(),
        started,
    })
}
#[cfg(target_os = "macos")]
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if read != size
        || info.pbi_pid != pid
        || info.pbi_uid != unsafe { libc::geteuid() }
        || info.pbi_status == 5
    {
        return None;
    }
    Some(ProcessIdentity {
        parent: info.pbi_ppid,
        group: Some(info.pbi_pgid),
        birth: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        started: info.pbi_start_tvsec as f64 + info.pbi_start_tvusec as f64 / 1_000_000.,
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_identity(_: u32) -> Option<ProcessIdentity> {
    None
}

/// `process_identity` for a process of a listed table. Reading one process
/// here is cheap, so the table's parent and time are not needed.
pub fn listed_identity(pid: u32, _parent: u32, _listed_at: f64) -> Option<ProcessIdentity> {
    process_identity(pid)
}

// Read executable/argv from the kernel, not newline-delimited `ps` command text.
// A command containing newlines must not spoof a different process row or helper.
#[cfg(target_os = "linux")]
pub fn describe(pid: u32) -> Option<ProcessImage> {
    let executable = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .to_string_lossy()
        .into_owned();
    let mut bytes = Vec::new();
    fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let argv = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Some(ProcessImage {
        executable,
        argv: Some(argv),
    })
}
#[cfg(target_os = "macos")]
pub fn describe(pid: u32) -> Option<ProcessImage> {
    let mut path = vec![0u8; 4096];
    let n = unsafe {
        libc::proc_pidpath(
            pid as i32,
            path.as_mut_ptr() as *mut libc::c_void,
            path.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    let executable =
        String::from_utf8_lossy(&path[..path.iter().position(|b| *b == 0).unwrap_or(path.len())])
            .into_owned();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut bytes = vec![0u8; 1024 * 1024];
    let mut size = bytes.len();
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size < 4 {
        return Some(ProcessImage {
            executable,
            argv: None,
        });
    }
    bytes.truncate(size);
    let argc = i32::from_ne_bytes(bytes[..4].try_into().ok()?).clamp(0, 4096) as usize;
    let mut rest = &bytes[4..];
    rest = &rest[rest.iter().position(|b| *b == 0)? + 1..];
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    let argv = rest
        .split(|b| *b == 0)
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Some(ProcessImage {
        executable,
        argv: Some(argv),
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn describe(_: u32) -> Option<ProcessImage> {
    None
}

/// Every process with its parent and process group, as ps lists them.
pub fn process_table() -> std::result::Result<BTreeMap<u32, ProcessRow>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid="])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Live process inspection is unavailable".into());
    }
    let mut rows = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let numbers: Vec<u32> = line
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if numbers.len() != 3 {
            continue;
        }
        rows.insert(
            numbers[0],
            ProcessRow {
                pid: numbers[0],
                parent: numbers[1],
                group: Some(numbers[2]),
            },
        );
    }
    Ok(rows)
}

/// The parent and the arguments of `pid`.
#[cfg(target_os = "linux")]
pub fn parent_and_argv(pid: u32) -> Option<(u32, Vec<String>)> {
    let path = PathBuf::from(format!("/proc/{pid}"));
    let stat = fs::read_to_string(path.join("stat")).ok()?;
    let parent = stat[stat.rfind(')')? + 2..]
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let bytes = fs::read(path.join("cmdline")).ok()?;
    let argv = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    Some((parent, argv))
}
#[cfg(not(target_os = "linux"))]
pub fn parent_and_argv(pid: u32) -> Option<(u32, Vec<String>)> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=", "-o", "command="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let (parent, command) = line.trim().split_once(char::is_whitespace)?;
    Some((
        parent.parse().ok()?,
        shell_words::split(command.trim()).ok()?,
    ))
}
pub fn parent_process_id() -> u32 {
    unsafe { libc::getppid() as u32 }
}
/// The parent and command name of `pid` as ps prints them, each None when absent.
pub fn parent_and_name(pid: u32) -> io::Result<(Option<u32>, Option<String>)> {
    let process = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,comm="])
        .output()?;
    let process = String::from_utf8_lossy(&process.stdout);
    let mut fields = process.split_whitespace();
    Ok((
        fields.next().and_then(|n| n.parse().ok()),
        fields.next().map(str::to_owned),
    ))
}

/// Read-only compatibility fixture; cleanup uses the strict sampler below.
#[cfg(all(test, target_os = "linux"))]
pub fn current_directories() -> std::result::Result<Vec<(u32, PathBuf, String)>, String> {
    let own = std::process::id();
    let entries = fs::read_dir("/proc").map_err(|e|format!("Cannot inspect running processes: {e}"))?;
    Ok(entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            if pid == own {
                return None;
            }
            let cwd = fs::read_link(entry.path().join("cwd")).ok()?;
            let name = fs::read_to_string(entry.path().join("comm"))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            Some((pid, cwd, name))
        })
        .collect())
}
/// Without a way to read other processes' directories, nothing may count as unused.
#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
pub fn current_directories() -> std::result::Result<Vec<(u32, PathBuf, String)>, String> {
    Err("Running processes cannot be inspected on this computer".into())
}

/// Automatic cleanup needs a complete bounded sample of this user's live
/// processes. A failed cwd/executable read is unknown, never evidence of disuse.
#[cfg(target_os = "linux")]
pub fn worktree_processes(deadline: std::time::Instant) -> std::result::Result<super::WorktreeProcesses, String> {
    use std::io::Read;
    let deadline = deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(2));
    let uid = unsafe { libc::getuid() };
    let error = || "Running process coverage is incomplete; review the worktree manually".to_owned();
    fn read(path: &Path) -> std::io::Result<String> {
        let mut bytes=Vec::new();
        fs::File::open(path)?.take(65537).read_to_end(&mut bytes)?;
        if bytes.len()>65536 { return Err(std::io::Error::other("process metadata exceeds its bound")); }
        String::from_utf8(bytes).map_err(std::io::Error::other)
    }
    fn identity(path: &Path) -> std::io::Result<(String,bool,u32)> {
        let raw=read(&path.join("stat"))?;
        let fields:Vec<_>=raw.rsplit_once(") ").ok_or_else(||std::io::Error::other("invalid process identity"))?.1.split_whitespace().collect();
        let start=fields.get(19).ok_or_else(||std::io::Error::other("invalid process start"))?;
        let parent=fields.get(1).ok_or_else(||std::io::Error::other("invalid process parent"))?
            .parse().map_err(std::io::Error::other)?;
        Ok(((*start).to_owned(),matches!(fields.first(),Some(&"Z"|&"X")),parent))
    }
    fn ancestors(deadline: std::time::Instant) -> std::io::Result<std::collections::BTreeMap<u32,String>> {
        let mut rows=std::collections::BTreeMap::new();
        let mut pid=std::process::id();
        while pid!=0 {
            if rows.len()>=128 || std::time::Instant::now()>=deadline || rows.contains_key(&pid) {
                return Err(std::io::Error::other("incomplete invocation ancestry"));
            }
            let (start,exited,parent)=identity(&PathBuf::from(format!("/proc/{pid}")))?;
            if exited {return Err(std::io::Error::other("exited invocation ancestor"));}
            rows.insert(pid,start);pid=parent;
        }
        Ok(rows)
    }
    let invocation=ancestors(deadline).map_err(|_|error())?;
    let entries=fs::read_dir("/proc").map_err(|_|error())?;
    let boot=read(Path::new("/proc/sys/kernel/random/boot_id")).map_err(|_|error())?;
    let mut rows=super::WorktreeProcesses::default();
    for (count,entry) in entries.enumerate() {
        if count>=32768 || std::time::Instant::now()>=deadline { return Err(error()); }
        let entry=entry.map_err(|_|error())?;
        let Some(pid)=entry.file_name().to_str().and_then(|s|s.parse::<u32>().ok()) else {continue};
        let path=entry.path();
        let status=match read(&path.join("status")) {
            Ok(status)=>status,
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>continue,
            Err(_)=>return Err(error()),
        };
        let uids=status.lines().find_map(|line|line.strip_prefix("Uid:")).ok_or_else(error)?
            .split_whitespace().map(str::parse::<u32>).collect::<std::result::Result<Vec<_>,_>>().map_err(|_|error())?;
        if uids.len()!=4 {return Err(error());}
        if !uids.contains(&uid) {continue;}
        let before=match identity(&path) {
            Ok(before)=>before,
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>continue,
            Err(_)=>return Err(error()),
        };
        if before.1 {continue;}
        let cwd=fs::read_link(path.join("cwd"));
        let image=fs::read_link(path.join("exe"));
        let after=match identity(&path) {
            Ok(after)=>after,
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>continue,
            Err(_)=>return Err(error()),
        };
        if after.1 {continue;}
        if before!=after {return Err(error());}
        let name=read(&path.join("comm")).map(|s|s.trim().to_owned()).unwrap_or_default();
        // Only access denials on stable, live, fully identified processes are
        // reviewable. Timeouts, roster/identity errors and other IO errors stay
        // hard failures. A readable half still protects its actual worktree.
        let denied = |value: &std::io::Result<PathBuf>| value.as_ref().is_err_and(|e| e.kind()==std::io::ErrorKind::PermissionDenied);
        let (cwd_denied,image_denied)=(denied(&cwd),denied(&image));
        if (cwd.is_err() && !cwd_denied) || (image.is_err() && !image_denied) { return Err(error()); }
        if cwd_denied || image_denied {
            rows.unreadable.push(super::UnreadableWorktreeProcess {
                pid, identity:format!("{}:{}",boot.trim(),before.0), command:name.clone(),
                cwd:cwd_denied, executable:image_denied,
                invocation_ancestor:invocation.get(&pid)==Some(&before.0),
            });
        }
        if let Ok(cwd)=cwd { rows.paths.push((pid,cwd,name.clone())); }
        if let Ok(image)=image { rows.paths.push((pid,image,name)); }
    }
    if std::time::Instant::now()>=deadline || ancestors(deadline).map_err(|_|error())?!=invocation {return Err(error());}
    rows.unreadable.sort_by_key(|row|row.pid);
    Ok(rows)
}

#[cfg(target_os = "macos")]
pub fn worktree_processes(deadline: std::time::Instant) -> std::result::Result<super::WorktreeProcesses, String> {
    use std::os::unix::ffi::OsStrExt;
    let deadline=deadline.min(std::time::Instant::now()+std::time::Duration::from_secs(2));
    let uid=unsafe {libc::getuid()};
    let error=|detail:&str|format!("Running process coverage is incomplete: {detail}; review the worktree manually");
    fn gone(pid:i32)->bool {
        (unsafe {libc::kill(pid,0)})<0 && std::io::Error::last_os_error().raw_os_error()==Some(libc::ESRCH)
    }
    fn identity(pid:i32)->std::io::Result<libc::proc_bsdinfo> {
        let mut info:libc::proc_bsdinfo=unsafe {std::mem::zeroed()};
        let size=std::mem::size_of_val(&info) as libc::c_int;
        let read=unsafe {libc::proc_pidinfo(pid,libc::PROC_PIDTBSDINFO,0,&mut info as *mut _ as *mut libc::c_void,size)};
        if read!=size {
            // libproc can return zero for an exited PID without setting errno.
            // Only an independent ESRCH probe proves absence; stale errno must
            // never hide an inaccessible live process from the owned roster.
            return Err(if gone(pid) {std::io::Error::from_raw_os_error(libc::ESRCH)}
                else {std::io::Error::other("Owned process metadata is unavailable")});
        }
        Ok(info)
    }
    // Kernel uid-filtered rosters establish ownership before inspection. A
    // protected foreign process need not disclose BSD info to this user. Both
    // effective and real uid are required; an unreadable owned process still
    // makes the sample incomplete. libproc returns bytes, not a PID count.
    const PROC_UID_ONLY: u32 = 4;
    const PROC_RUID_ONLY: u32 = 5;
    let mut pids=std::collections::BTreeSet::new();
    for kind in [PROC_UID_ONLY, PROC_RUID_ONLY] {
        let mut buffer=vec![0 as libc::c_int;32768];
        let bytes=(buffer.len()*std::mem::size_of::<libc::c_int>()) as libc::c_int;
        let found=unsafe {libc::proc_listpids(kind,uid,buffer.as_mut_ptr() as *mut libc::c_void,bytes)};
        if found<=0 || found>=bytes || !(found as usize).is_multiple_of(std::mem::size_of::<libc::c_int>()) {return Err(error("the process list could not be verified"));}
        pids.extend(buffer.into_iter().take(found as usize/std::mem::size_of::<libc::c_int>()).filter(|pid|*pid>0));
        if pids.len()>=32768 {return Err(error("the process list exceeded its bound"));}
    }
    if !pids.contains(&(std::process::id() as libc::c_int)) {return Err(error("the current process is missing from the list"));}
    let mut rows=Vec::new();
    for pid in pids.into_iter().filter(|pid|*pid>0) {
        if std::time::Instant::now()>=deadline {return Err(error("the process check reached its time limit"));}
        let before=match identity(pid) {
            Ok(info)=>info,
            Err(e) if e.raw_os_error()==Some(libc::ESRCH)=>continue,
            Err(_)=>return Err(error(&format!("process {pid} could not be identified"))),
        };
        if before.pbi_pid!=pid as u32 {return Err(error(&format!("process {pid} returned another identity")));}
        if (before.pbi_uid!=uid && before.pbi_ruid!=uid) || before.pbi_status==libc::SZOMB {continue;}
        let mut info:libc::proc_vnodepathinfo=unsafe {std::mem::zeroed()};
        let size=std::mem::size_of_val(&info) as libc::c_int;
        let read=unsafe {libc::proc_pidinfo(pid,libc::PROC_PIDVNODEPATHINFO,0,&mut info as *mut _ as *mut libc::c_void,size)};
        let mut image=[0u8;4096];
        let length=unsafe {libc::proc_pidpath(pid,image.as_mut_ptr() as *mut libc::c_void,image.len() as u32)};
        let after=match identity(pid) {
            Ok(info)=>info,
            Err(e) if e.raw_os_error()==Some(libc::ESRCH)=>continue,
            Err(_)=>return Err(error(&format!("process {pid} could not be identified"))),
        };
        if after.pbi_status==libc::SZOMB {continue;}
        if after.pbi_pid!=pid as u32 || (before.pbi_start_tvsec,before.pbi_start_tvusec,before.pbi_uid,before.pbi_ruid)!=(after.pbi_start_tvsec,after.pbi_start_tvusec,after.pbi_uid,after.pbi_ruid) {return Err(error(&format!("process {pid} changed while being checked")));}
        if read!=size || length<=0 {
            if gone(pid) {continue;}
            return Err(error(&format!("process {pid}: {} could not be inspected", if read!=size {"its current folder"} else {"its program"})));
        }
        let path=&info.pvi_cdir.vip_path;
        let bytes=unsafe {std::slice::from_raw_parts(path.as_ptr() as *const u8,std::mem::size_of_val(path))};
        let end=bytes.iter().position(|b|*b==0).filter(|end|*end>0).ok_or_else(||error(&format!("process {pid} returned an unconfirmed path")))?;
        let image_end=image.iter().position(|b|*b==0).filter(|end|*end>0).ok_or_else(||error(&format!("process {pid} returned an unconfirmed path")))?;
        let cwd=PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end]));
        let image=PathBuf::from(std::ffi::OsStr::from_bytes(&image[..image_end]));
        let name=unsafe {std::slice::from_raw_parts(before.pbi_comm.as_ptr() as *const u8,before.pbi_comm.len())};
        let name=String::from_utf8_lossy(&name[..name.iter().position(|b|*b==0).unwrap_or(name.len())]).into_owned();
        rows.push((pid as u32,cwd,name.clone()));rows.push((pid as u32,image,name));
    }
    if std::time::Instant::now()>=deadline {return Err(error("the process check reached its time limit"));}
    Ok(rows.into())
}

#[cfg(not(any(target_os = "linux",target_os = "macos")))]
pub fn worktree_processes(_deadline:std::time::Instant)->std::result::Result<super::WorktreeProcesses,String> {
    Err("Running processes cannot be inspected on this computer".into())
}

/// The files `pid` has open, as the kernel names them (macOS: lsof).
pub fn open_files(pid: u32) -> std::result::Result<Vec<PathBuf>, RunError> {
    #[cfg(target_os = "linux")]
    {
        Ok(fs::read_dir(format!("/proc/{pid}/fd"))
            .map_err(RunError::Io)?
            .flatten()
            .filter_map(|entry| fs::read_link(entry.path()).ok())
            .collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = output_until(
            Command::new("/usr/sbin/lsof").args(["-a", "-p", &pid.to_string(), "-Fn"]),
            Duration::from_secs(2),
        )?;
        if !output.status.success() {
            return Err(RunError::Failed);
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix('n').map(PathBuf::from))
            .collect())
    }
}
/// Whether `pid` has the file at `path` open; `path` is compared as the kernel names it.
pub fn holds_open(pid: u32, path: &Path) -> std::result::Result<super::OpenFiles, RunError> {
    #[cfg(test)]
    if let Some(reason) = super::forced_open_files_unknown() {
        return Ok(super::OpenFiles::Unknown(reason));
    }
    Ok(if open_files(pid)?.iter().any(|file| file == path) {
        super::OpenFiles::Held
    } else {
        super::OpenFiles::NotHeld
    })
}

/// A process to stop, pinned to its identity. Linux holds a PID file
/// descriptor, which never reaches a later process with the same ID; elsewhere
/// the identity is checked again before every signal.
pub struct PinnedProcess {
    pid: u32,
    birth: String,
    #[cfg(target_os = "linux")]
    handle: fs::File,
}

/// A verified process state. Inspection failure is distinct from confirmed exit.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState { Running, Stopped, Exited }

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn process_state(pid: u32, birth: Option<&str>) -> std::result::Result<ProcessState, String> {
    #[cfg(target_os = "linux")]
    {
        let path = PathBuf::from(format!("/proc/{pid}"));
        let read = || -> io::Result<ProcessState> {
            if fs::metadata(&path)?.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::other("Process ownership is unavailable"));
            }
            let stat = fs::read_to_string(path.join("stat"))?;
            let fields: Vec<_> = stat.rsplit_once(')').ok_or_else(|| io::Error::other("Invalid process identity"))?.1.split_whitespace().collect();
            if fields.len() < 20 { return Err(io::Error::other("Incomplete process identity")); }
            Ok(if birth.is_some_and(|birth| birth != fields[19]) || matches!(fields[0], "Z" | "X") { ProcessState::Exited }
                else if matches!(fields[0], "T" | "t") { ProcessState::Stopped } else { ProcessState::Running })
        };
        match read() {
            Ok(state) => Ok(state),
            Err(error) if error.kind() == io::ErrorKind::NotFound && presence(pid) == Presence::Gone => Ok(ProcessState::Exited),
            Err(error) => Err(format!("Could not verify PID {pid}: {error}")),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let read = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut libc::c_void, size) };
        if read != size {
            return if presence(pid) == Presence::Gone { Ok(ProcessState::Exited) }
                else { Err(format!("Could not verify PID {pid}")) };
        }
        if info.pbi_pid != pid || info.pbi_uid != unsafe { libc::geteuid() } {
            return Err(format!("Could not verify ownership of PID {pid}"));
        }
        let current = format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec);
        Ok(if birth.is_some_and(|birth| birth != current) || info.pbi_status == libc::SZOMB { ProcessState::Exited }
            else if info.pbi_status == libc::SSTOP { ProcessState::Stopped } else { ProcessState::Running })
    }
}

impl PinnedProcess {
    pub fn pin(pid: u32, birth: &str) -> std::result::Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
            if fd < 0 {
                return Err("Process changed or this kernel lacks PID handle support".into());
            }
            let handle = unsafe { fs::File::from_raw_fd(fd) };
            if process_identity(pid).is_none_or(|p| p.birth != birth) {
                return Err("Process identity changed".into());
            }
            Ok(Self {
                pid,
                birth: birth.into(),
                handle,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self {
                pid,
                birth: birth.into(),
            })
        }
    }
    pub fn alive(&self) -> bool {
        process_identity(self.pid).is_some_and(|p| p.birth == self.birth)
    }
    /// Ask the process to exit (SIGTERM).
    pub fn terminate(&self) -> std::result::Result<(), String> {
        self.signal(libc::SIGTERM)
    }
    /// End the process at once (SIGKILL).
    pub fn kill(&self) -> std::result::Result<(), String> {
        self.signal(libc::SIGKILL)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn state(&self) -> std::result::Result<ProcessState, String> {
        process_state(self.pid, Some(&self.birth))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn stop(&self) -> std::result::Result<(), String> { self.signal(libc::SIGSTOP) }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn resume(&self) -> std::result::Result<(), String> { self.signal(libc::SIGCONT) }
    fn signal(&self, signal: i32) -> std::result::Result<(), String> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.state()? == ProcessState::Exited { return Ok(()); }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        if !self.alive() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        let result = {
            (unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.handle.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            }) as i32
        };
        #[cfg(not(target_os = "linux"))]
        let result = unsafe { libc::kill(self.pid as i32, signal) };
        if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(format!(
                "Could not stop PID {}: {}",
                self.pid,
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

/// Ask process `pid` to exit (SIGTERM). Nothing forces it to. IDs that
/// `kill` reads as a process group or as every process are refused.
pub fn request_exit(pid: u32) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    #[cfg(all(debug_assertions, target_os = "macos"))]
    let observation = std::panic::catch_unwind(|| pause_signal_observer::before(pid as u32))
        .ok()
        .flatten();
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    // Capture errno before any diagnostic output. A successful kill has no errno.
    let error = if result != 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        let errno = error.as_ref().and_then(io::Error::raw_os_error);
        let _ =
            std::panic::catch_unwind(|| pause_signal_observer::after(observation, result, errno));
    }
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

// BEGIN private Mac fixture observer. No observer IO precedes the original kill.
#[cfg(all(debug_assertions, target_os = "macos"))]
mod pause_signal_observer {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;
    use std::os::fd::FromRawFd;

    const FLAG: &str = "HGS_TEST_MAC_PAUSE_OBSERVER";
    const ROOT: &str = "HGS_TEST_MAC_PAUSE_ROOT";
    const MAX_ROW: usize = 768;

    #[derive(Clone, Copy)]
    pub(super) struct Before {
        pid: u32,
        sender: u32,
        at: Option<u64>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Context {
        version: u8,
        nonce: String,
        target_pid: u32,
        root_dev: u64,
        root_ino: u64,
        run: String,
        conversation: String,
        pane: String,
        saved_start: String,
    }

    fn clock() -> Option<u64> {
        let mut value: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0
            || value.tv_sec < 0
            || !(0..1_000_000_000).contains(&value.tv_nsec)
        {
            return None;
        }
        (value.tv_sec as u64)
            .checked_mul(1_000_000_000)?
            .checked_add(value.tv_nsec as u64)
    }

    pub(super) fn before(pid: u32) -> Option<Before> {
        // Environment lookup and a clock/PID scalar only: no file/proc read,
        // output, callback, new thread or admission decision before the kill.
        (env::var_os(FLAG).as_deref() == Some(std::ffi::OsStr::new("1"))).then(|| Before {
            pid,
            sender: std::process::id(),
            at: clock(),
        })
    }

    fn private(metadata: &fs::Metadata, mode: u32, file: bool) -> bool {
        metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == mode
            && if file {
                metadata.is_file() && metadata.nlink() == 1
            } else {
                metadata.is_dir()
            }
    }

    fn open_at(root: &fs::File, name: &'static [u8], flags: i32) -> io::Result<fs::File> {
        let fd = unsafe {
            libc::openat(
                root.as_raw_fd(),
                name.as_ptr().cast(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    fn hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn observe(
        before: Before,
        result: i32,
        errno: Option<i32>,
        returned_at: Option<u64>,
    ) -> io::Result<()> {
        let path = env::var_os(ROOT)
            .map(PathBuf::from)
            .ok_or(io::ErrorKind::NotFound)?;
        if path.as_os_str().len() > 4096 || !path.is_absolute() || fs::canonicalize(&path)? != path
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
            .open(path)?;
        let root = directory.metadata()?;
        if !private(&root, 0o700, false) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut input = open_at(&directory, b"manifest.json\0", libc::O_RDONLY)?;
        let initial = input.metadata()?;
        if !private(&initial, 0o600, true) || initial.len() > 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut input)
            .take(1025)
            .read_to_end(&mut bytes)?;
        let final_metadata = input.metadata()?;
        if bytes.len() > 1024
            || initial.len() != bytes.len() as u64
            || initial.dev() != final_metadata.dev()
            || initial.ino() != final_metadata.ino()
            || initial.len() != final_metadata.len()
            || !private(&final_metadata, 0o600, true)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let context: Context =
            serde_json::from_slice(&bytes).map_err(|_| io::ErrorKind::InvalidData)?;
        if context.version != 1
            || context.target_pid != before.pid
            || before.pid == 0
            || context.root_dev != root.dev()
            || context.root_ino != root.ino()
            || !hex(&context.nonce, 32)
            || [
                &context.run,
                &context.conversation,
                &context.pane,
                &context.saved_start,
            ]
            .iter()
            .any(|value| !hex(value, 64))
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        // These are post-kill observations, never an atomic delivery receipt.
        let sender_birth = process_identity(before.sender).map(|p| p.birth);
        let target_birth = process_identity(before.pid).map(|p| p.birth);
        let mut output = Vec::new();
        for (sequence, phase, at, rc, code) in [
            (1, "kill_before", before.at, None, None),
            (2, "kill_return_or_errno", returned_at, Some(result), errno),
        ] {
            let row = json!({"v":1,"role":"sender","seq":sequence,"phase":phase,
                "nonce":context.nonce,"pid":before.sender,"target":before.pid,
                "clock":"CLOCK_MONOTONIC","at":at,"run":context.run,"conversation":context.conversation,
                "pane":context.pane,"saved_start":context.saved_start,"birth_before":"unknown",
                "sender_birth_after":sender_birth,"target_birth_after":target_birth,"rc":rc,"errno":code});
            let mut row = serde_json::to_vec(&row).map_err(|_| io::ErrorKind::InvalidData)?;
            row.push(b'\n');
            if row.len() > MAX_ROW {
                return Err(io::ErrorKind::InvalidData.into());
            }
            output.extend(row);
        }
        let mut file = open_at(
            &directory,
            b"sender.jsonl\0",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        if !private(&file.metadata()?, 0o600, true) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // A short write remains visibly partial; never retry even telemetry IO.
        if file.write(&output)? != output.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok(())
    }

    pub(super) fn after(before: Option<Before>, result: i32, errno: Option<i32>) {
        if let Some(before) = before {
            let returned_at = clock();
            let _ = observe(
                before,
                result,
                if result == 0 { None } else { errno },
                returned_at,
            );
        }
    }
}
// END private Mac fixture observer.

static SHUTDOWN_CHILD: AtomicI32 = AtomicI32::new(0);
static SHUTDOWN_SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn shutdown_handler(signal: i32) {
    // Ctrl-C is an agent interaction, often just interrupting its current turn.
    // A handler (not SIG_IGN) resets to default in exec'd children.
    if signal == libc::SIGINT {
        return;
    }
    SHUTDOWN_SIGNAL.store(signal, Ordering::SeqCst);
    let pid = SHUTDOWN_CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}
/// A supervisor's stop handling until it is dropped. Interrupts (SIGINT)
/// belong to the agent; a stop request (SIGTERM, SIGHUP) is recorded for
/// `shutdown_signal` and passed to the child named by `forward_shutdown`.
pub struct ShutdownGuard(Vec<(i32, libc::sigaction)>);
impl ShutdownGuard {
    pub fn install() -> io::Result<Self> {
        SHUTDOWN_SIGNAL.store(0, Ordering::SeqCst);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = shutdown_handler as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.0.push((signal, previous));
        }
        Ok(guard)
    }
}
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        SHUTDOWN_CHILD.store(0, Ordering::SeqCst);
        for (signal, previous) in &self.0 {
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}
/// The stop request received since `ShutdownGuard::install` as a signal number, or 0.
pub fn shutdown_signal() -> i32 {
    SHUTDOWN_SIGNAL.load(Ordering::SeqCst)
}
/// Pass stop requests to `child` from now on, and one that arrived while it started.
pub fn forward_shutdown(child: &Child) {
    SHUTDOWN_CHILD.store(child.id() as i32, Ordering::SeqCst);
    let signal = SHUTDOWN_SIGNAL.load(Ordering::SeqCst);
    if signal != 0 {
        unsafe {
            libc::kill(child.id() as i32, signal);
        }
    }
}
/// Stop passing requests on: the child has exited.
pub fn shutdown_child_exited() {
    SHUTDOWN_CHILD.store(0, Ordering::SeqCst);
}

/// Start `command` in a session of its own, so the end of the terminal, SSH
/// connection or desktop app that started it does not stop it.
pub fn spawn_detached(command: &mut Command) -> io::Result<Child> {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Run `command` without input in a process group of its own, collecting at
/// most `limits` bytes of standard output and errors, until it exits and both
/// streams end. When the output is too large, `deadline` passes, `cancelled`
/// says so or the child fails, the whole group is killed; a child that
/// succeeds may leave helpers running.
pub fn run_bounded(
    command: &mut Command,
    limits: [usize; 2],
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> std::result::Result<(ExitStatus, Vec<u8>, Vec<u8>), RunError> {
    run_bounded_stdin(command, Stdio::null(), limits, deadline, cancelled)
}

/// Bounded execution with explicitly supplied stdin; ownership/deadlines match run_bounded.
pub fn run_bounded_stdin(
    command: &mut Command,
    input: Stdio,
    limits: [usize; 2],
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> std::result::Result<(ExitStatus, Vec<u8>, Vec<u8>), RunError> {
    command
        .process_group(0)
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        // Both pipes must be drained while the child runs, including large outputs.
        unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            );
        }
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    let result = loop {
        let mut eof = true;
        for (pipe, buffer, limit) in [
            (&mut stdout as &mut dyn Read, &mut out, limits[0]),
            (&mut stderr as &mut dyn Read, &mut err, limits[1]),
        ] {
            loop {
                let mut chunk = [0; 4096];
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buffer.extend_from_slice(&chunk[..n]);
                        if buffer.len() > limit {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        eof = false;
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        eof = false;
                        break;
                    }
                }
            }
        }
        if out.len() > limits[0] || err.len() > limits[1] {
            break Err(RunError::TooLarge);
        }
        if status.is_none() {
            status = child.try_wait().map_err(RunError::Io)?;
        }
        if let Some(status) = status {
            if eof {
                break Ok(status);
            }
        }
        if cancelled() || Instant::now() >= deadline {
            break Err(RunError::Stopped);
        }
        std::thread::sleep(Duration::from_millis(3));
    };
    if !result.as_ref().is_ok_and(ExitStatus::success) {
        // A hook or filter can outlive the child. Stop the whole operation on
        // timeout instead of leaving it writing after an error was reported.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result.map(|status| (status, out, err))
}

/// `Command::output` with a time limit and closed input. Output goes to
/// regular files, so a child writing much before it exits never blocks on a
/// pipe and no reader thread outlives the call. A child still running at the
/// limit is killed.
pub fn output_until(
    command: &mut Command,
    duration: Duration,
) -> std::result::Result<Output, RunError> {
    let mut stdout = tempfile::tempfile().map_err(RunError::Io)?;
    let mut stderr = tempfile::tempfile().map_err(RunError::Io)?;
    command.stdout(Stdio::from(stdout.try_clone().map_err(RunError::Io)?));
    command.stderr(Stdio::from(stderr.try_clone().map_err(RunError::Io)?));
    command.stdin(Stdio::null());
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let deadline = Instant::now() + duration;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(RunError::Io)? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Stopped);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
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

/// Keep Git from asking for credentials: a read-only check fails instead of
/// waiting for a prompt nobody sees.
pub fn git_noninteractive(command: &mut Command) -> &mut Command {
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=2",
        )
}
/// The temporary directory of an interactive link's marker file.
pub fn link_marker_dir() -> io::Result<tempfile::TempDir> {
    tempfile::tempdir()
}
/// An OpenSSH LocalCommand for `ssh` that creates `path`; ssh runs it with the user's shell.
pub fn create_file_command(_: &mut Command, path: &Path) -> String {
    format!("touch {}", super::quote(&path.to_string_lossy()))
}
/// Escape sequences reach Unix terminals unchanged.
pub(super) fn virtual_terminal() -> bool {
    true
}
/// When `pid` started, as `ps -o lstart=` prints it with `LC_ALL=C TZ=UTC`, for
/// example "Sat Oct 10 08:27:57 2026". Stored run identities compare this text,
/// so older records written through ps keep matching. Read from the kernel:
/// session listings check several identities per poll, and each ps launch cost
/// more than the rest of the listing. Empty when the process does not exist;
/// None when its start time cannot be read here, so callers can ask ps instead.
pub fn process_start_time(pid: u32) -> Option<String> {
    if pid == 0 {
        return Some(String::new());
    }
    process_started_at(pid).map(|started| started.map(lstart_text).unwrap_or_default())
}

#[cfg(target_os = "macos")]
fn process_started_at(pid: u32) -> Option<Option<u64>> {
    // The kinfo_proc that ps reads, which also covers other users' processes.
    // Its extern_proc begins with p_starttime, a timeval whose seconds come first.
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as libc::c_int,
    ];
    let mut info = [0u64; 128]; // kinfo_proc is 648 bytes
    let mut size = std::mem::size_of_val(&info);
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            info.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    match (rc, size) {
        (0, 0) => Some(None), // no such process
        (0, size) if size >= 8 => Some(Some(info[0])),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn process_started_at(pid: u32) -> Option<Option<u64>> {
    use std::sync::OnceLock;
    // procps: boot time from /proc/stat plus the start in clock ticks, truncated.
    static BOOT: OnceLock<Option<u64>> = OnceLock::new();
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    // The command name may contain spaces and parentheses; fields follow its last ')'.
    let ticks: u64 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let boot = (*BOOT.get_or_init(|| {
        fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())
    }))?;
    let hertz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|&hz| hz > 0)?;
    Some(Some(boot + ticks / hertz))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_started_at(_pid: u32) -> Option<Option<u64>> {
    None
}

/// `%a %b %e %H:%M:%S %Y` in the C locale and UTC, the form ps and ctime print.
fn lstart_text(seconds: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        day,
        time / 3600,
        time % 3600 / 60,
        time % 60,
        year
    )
}

fn runtime_directory() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}
pub fn wayland_env() -> Option<String> {
    if let Ok(value) = env::var("WAYLAND_DISPLAY") {
        if !value.is_empty() {
            return Some(value);
        }
    }
    let mut entries: Vec<_> = fs::read_dir(runtime_directory())
        .ok()?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("wayland-")
            && !name.ends_with(".lock")
            && entry.file_type().is_ok_and(|t| t.is_socket())
        {
            env::set_var("WAYLAND_DISPLAY", &name);
            return Some(name);
        }
    }
    None
}
pub fn gui_env() -> Vec<String> {
    if is_macos() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut system_env: Option<String> = None;
    for key in ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY"] {
        if env::var(key).is_ok_and(|v| !v.is_empty()) {
            continue;
        }
        let sysenv = system_env.get_or_insert_with(|| {
            capture("systemctl", &["--user", "show-environment"]).unwrap_or_default()
        });
        let mut value = sysenv
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or("")
            .to_owned();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = value[1..value.len() - 1].into();
        }
        if value.is_empty() && key == "WAYLAND_DISPLAY" {
            value = wayland_env().unwrap_or_default();
        }
        if value.is_empty() || (key == "XAUTHORITY" && fs::File::open(&value).is_err()) {
            continue;
        }
        result.push(format!("{key}={value}"));
    }
    result
}

static CHILD_PID: AtomicI32 = AtomicI32::new(0);
static INTERRUPTED: AtomicI32 = AtomicI32::new(0);
extern "C" fn forward_signal(signal: libc::c_int) {
    INTERRUPTED.store(signal, Ordering::Relaxed);
    let pid = CHILD_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}
/// Keep terminal restoration in normal Rust control flow even on SIGINT/TERM/HUP.
pub fn wait_interactive(command: &mut Command) -> Result<i32> {
    struct Handlers(Vec<(i32, libc::sighandler_t)>);
    impl Drop for Handlers {
        fn drop(&mut self) {
            CHILD_PID.store(0, Ordering::Relaxed);
            for &(sig, old) in &self.0 {
                unsafe {
                    libc::signal(sig, old);
                }
            }
        }
    }
    INTERRUPTED.store(0, Ordering::Relaxed);
    let _handlers = Handlers(
        [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            .map(|sig| {
                (sig, unsafe {
                    libc::signal(sig, forward_signal as *const () as libc::sighandler_t)
                })
            })
            .collect(),
    );
    let mut child = command.spawn().map_err(|e| {
        Error::new(
            127,
            format!("{}: {e}", command.get_program().to_string_lossy()),
        )
    })?;
    CHILD_PID.store(child.id() as i32, Ordering::Relaxed);
    let signal = INTERRUPTED.load(Ordering::Relaxed);
    if signal != 0 {
        unsafe {
            libc::kill(child.id() as i32, signal);
        }
    }
    let rc = code(child.wait()?);
    let signal = INTERRUPTED.load(Ordering::Relaxed);
    Ok(if signal == 0 { rc } else { 128 + signal })
}

fn applescript_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
pub fn clip() -> Result<i32> {
    if is_macos() {
        if !available("osascript") {
            eprintln!("hgs: clip: no osascript");
            return Ok(2);
        }
        if capture("osascript", &["-e", "clipboard info for «class PNGf»"])
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return Ok(1);
        }
        let temporary = tempfile::NamedTempFile::new()?;
        let path = applescript_string(&temporary.path().to_string_lossy());
        if silent(
            "osascript",
            &[
                "-e",
                &format!("set f to open for access POSIX file \"{path}\" with write permission"),
                "-e",
                "set eof f to 0",
                "-e",
                "write (the clipboard as «class PNGf») to f",
                "-e",
                "close access f",
            ],
        ) {
            io::copy(&mut fs::File::open(temporary.path())?, &mut io::stdout())?;
            return Ok(0);
        }
        eprintln!("hgs: clip: osascript could not read the clipboard as PNG");
        return Ok(2);
    }
    if !available("wl-paste") {
        eprintln!("hgs: clip: no wl-paste (wl-clipboard)");
        return Ok(2);
    }
    if wayland_env().is_none() {
        eprintln!(
            "hgs: clip: no Wayland socket in {}",
            runtime_directory().display()
        );
        return Ok(2);
    }
    if !capture("wl-paste", &["-l"]).is_some_and(|types| types.lines().any(|t| t == "image/png")) {
        return Ok(1);
    }
    Ok(code(
        Command::new("wl-paste")
            .args(["-t", "image/png"])
            .status()?,
    ))
}
fn clip_set(path: &Path) -> bool {
    if is_macos() {
        silent(
            "osascript",
            &[
                "-e",
                &format!(
                    "set the clipboard to (read (POSIX file \"{}\") as «class PNGf»)",
                    applescript_string(&path.to_string_lossy())
                ),
            ],
        )
    } else {
        if !available("wl-copy") || wayland_env().is_none() {
            return false;
        }
        let Ok(file) = fs::File::open(path) else {
            return false;
        };
        Command::new("wl-copy")
            .args(["-t", "image/png"])
            .stdin(file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}
fn paste_log(config: &Config, message: &str) {
    let path = PathBuf::from(
        env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{}/.local/state", config.home)),
    )
    .join("hgs");
    if fs::create_dir_all(&path).is_err() {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join("paste.log"))
    {
        let stamp = capture("date", &["+%Y-%m-%d %H:%M:%S"]).unwrap_or_default();
        let _ = writeln!(file, "{} {message}", stamp.trim());
    }
}
/// The tmux C-v binding: pull the viewing peer's clipboard image, then pass C-v on.
pub fn paste(config: &Config, session: &str, pane: &str, client: &str) -> Result<i32> {
    let terminal = crate::state::backend::current();
    let message = |text: String| {
        if !client.is_empty() {
            terminal.notify(client, &format!("hgs: {text}"));
        }
    };
    let who = terminal.viewer(session);
    let who = who.as_str();
    // tmux reports a variable removed from the session as "-NAME".
    if matches!(who, "" | "-HGS_CLIENT" | "local") {
        paste_log(
            config,
            &format!(
                "sess={session} who={} local-passthrough",
                if who.is_empty() { "unset" } else { who }
            ),
        );
    } else {
        message(format!("pulling clipboard from {who}"));
        match tempfile::NamedTempFile::new() {
            Ok(temporary) => {
                let start = Instant::now();
                let rc = temporary
                    .reopen()
                    .ok()
                    .and_then(|file| {
                        super::ssh()
                            .args(crate::machines::ssh_options(config, who))
                            .args([
                                "-o",
                                "BatchMode=yes",
                                "-o",
                                "ConnectTimeout=3",
                                who,
                                "~/.local/bin/hgs clip",
                            ])
                            .stdout(file)
                            .stderr(Stdio::null())
                            .status()
                            .ok()
                    })
                    .map(code)
                    .unwrap_or(255);
                let prefix = format!(
                    "sess={session} who={who} rc={rc} ssh={}s",
                    start.elapsed().as_secs()
                );
                match rc {
                    0 if clip_set(temporary.path()) => paste_log(
                        config,
                        &format!(
                            "{prefix} bytes={} set=ok",
                            temporary.as_file().metadata().map(|m| m.len()).unwrap_or(0)
                        ),
                    ),
                    0 => {
                        message("cannot set clipboard on this box".into());
                        paste_log(config, &format!("{prefix} set=FAIL"));
                    }
                    1 => {
                        message(format!("no image in {who} clipboard"));
                        paste_log(config, &format!("{prefix} no-image-on-{who}"));
                    }
                    2 => {
                        message(format!("{who} has no clipboard tool (hgs clip rc 2)"));
                        paste_log(config, &format!("{prefix} no-tool-on-{who}"));
                    }
                    _ => {
                        message(format!("{who} unreachable, pasting local clipboard"));
                        paste_log(config, &format!("{prefix} ssh-failed"));
                    }
                }
            }
            Err(_) => paste_log(config, &format!("sess={session} who={who} mktemp-failed")),
        }
    }
    // A failed clipboard transfer must never swallow the agent's paste key.
    terminal.forward_key(pane, "C-v");
    Ok(0)
}

/// A pipe whose two ends no child of this process inherits (N33): a child
/// holding the write end would keep the reader from ever seeing EOF.
fn cloexec_pipe() -> std::io::Result<[i32; 2]> {
    let mut fds = [0i32; 2];
    #[cfg(target_os = "linux")]
    let made = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(target_os = "linux"))]
    let made = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if made < 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(target_os = "linux"))]
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
    }
    Ok(fds)
}

/// H2: the current process reads `bytes` as stdin through an anonymous pipe,
/// in memory only: a JSON request may hold a private key and must not land on
/// disk.
pub fn stdin_from_bytes(bytes: Vec<u8>) -> std::io::Result<()> {
    let fds = cloexec_pipe()?;
    // dup2 leaves the new stdin inheritable: a child (ssh for `hgs @peer`)
    // reads it. The write end stays with this process only, so the reader
    // sees EOF once the bytes are written (N33).
    if unsafe { libc::dup2(fds[0], 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe { libc::close(fds[0]) };
    std::thread::spawn(move || {
        let mut offset = 0usize;
        while offset < bytes.len() {
            let written = unsafe {
                libc::write(fds[1], bytes[offset..].as_ptr() as *const _, bytes.len() - offset)
            };
            if written <= 0 {
                break;
            }
            offset += written as usize;
        }
        unsafe { libc::close(fds[1]) };
    });
    Ok(())
}

/// Write the script `text` to `path` for a test to run at once. The system
/// will not execute a file that is open for writing (ETXTBSY), and a child that
/// another test thread starts while this process writes one keeps that
/// descriptor until it execs itself, so a shell writes the script: no
/// descriptor of this process ever has it open for writing.
#[cfg(test)]
pub(crate) fn write_script(path: &Path, text: &str) {
    let status = Command::new("/bin/sh")
        .args(["-c", r#"printf %s "$1" > "$0""#])
        .arg(path)
        .arg(text)
        .status()
        .unwrap();
    assert!(status.success(), "cannot write {}", path.display());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N33: a child started while the stdin pipe is being written never
    /// holds its write end, so the reader sees EOF once this process closes it.
    #[test]
    fn the_stdin_pipe_write_end_never_reaches_a_child() {
        let fds = cloexec_pipe().unwrap();
        for fd in fds {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "fd {fd} flags {flags}");
        }
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        unsafe { libc::close(fds[1]) };
        assert!(unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) } >= 0);
        let mut byte = [0u8; 1];
        // Another parallel test may be between fork and exec, temporarily
        // holding even a CLOEXEC descriptor. Wait for that bounded transition;
        // inheriting it into the long-lived sleep child must still fail.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (read, error) = loop {
            let read = unsafe { libc::read(fds[0], byte.as_mut_ptr() as *mut _, 1) };
            let error = std::io::Error::last_os_error();
            if read >= 0 || !matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EINTR))
                || std::time::Instant::now() >= deadline {
                break (read, error);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let _ = child.kill();
        let _ = child.wait();
        unsafe { libc::close(fds[0]) };
        assert_eq!(read, 0, "{error}");
    }

    #[test]
    fn prepared_path_keeps_the_caller_order_and_appends_user_installers() {
        assert_eq!(
            prepared_path("/home/user", "/usr/bin:/bin"),
            "/home/user/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:\
             /home/user/.npm-global/bin:/home/user/.kimi-code/bin:/home/user/.opencode/bin"
        );
        // OpenCode's official user install comes last: any `opencode` the caller's
        // PATH or the prepended folders hold is found first.
        let path = prepared_path("/home/user", "/usr/bin");
        let folders: Vec<&str> = path.split(':').collect();
        assert_eq!(folders.last(), Some(&"/home/user/.opencode/bin"));
        assert_eq!(
            folders
                .iter()
                .filter(|f| f.ends_with("/.opencode/bin"))
                .count(),
            1
        );
        assert!(
            folders.iter().position(|f| *f == "/usr/bin")
                < folders.iter().position(|f| f.ends_with("/.opencode/bin"))
        );
    }

    fn ps_start(pid: u32) -> Option<String> {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    #[test]
    fn start_time_text_matches_ps_lstart_form() {
        // C locale, UTC, the day padded with a space: stored identities compare as text.
        assert_eq!(lstart_text(0), "Thu Jan  1 00:00:00 1970");
        assert_eq!(lstart_text(1_791_620_877), "Sat Oct 10 08:27:57 2026");
        assert_eq!(lstart_text(1_759_309_200), "Wed Oct  1 09:00:00 2025");
        assert_eq!(lstart_text(951_782_400), "Tue Feb 29 00:00:00 2000");
    }

    #[test]
    fn process_start_time_matches_ps() {
        let parent = unsafe { libc::getppid() } as u32;
        for pid in [std::process::id(), parent, 1] {
            let Some(expected) = ps_start(pid) else {
                return;
            };
            let actual = process_start_time(pid).expect("readable start time");
            // A process that cannot be read must stay unknown rather than look absent.
            if expected.is_empty() {
                continue;
            }
            assert_eq!(actual, expected, "pid {pid}");
        }
    }

    #[test]
    fn missing_process_has_no_start_time() {
        assert_eq!(process_start_time(0), Some(String::new()));
        // pid_max is far below this on Linux and macOS.
        assert_eq!(process_start_time(99_999_999), Some(String::new()));
    }

    #[test]
    #[ignore = "compares every visible process with ps; run manually on each platform"]
    fn every_process_start_time_matches_ps() {
        let output = Command::new("ps")
            .args(["-axo", "pid=,lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        let (mut checked, mut differ) = (0, Vec::new());
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim_start();
            let Some((pid, expected)) = line.split_once(' ') else {
                continue;
            };
            let pid: u32 = pid.parse().unwrap();
            match process_start_time(pid) {
                Some(actual) if actual == expected.trim() => checked += 1,
                Some(actual) if actual.is_empty() => {} // exited since ps listed it
                other => differ.push(format!("{pid}: {other:?} != {expected:?}")),
            }
        }
        assert!(differ.is_empty(), "{checked} matched; differ: {differ:#?}");
        assert!(checked > 10);
    }
    #[test]
    fn exit_information_keeps_record_values() {
        // Wait statuses: exit code 3, then death by SIGKILL and by SIGTERM.
        let exited = ExitInfo::of(ExitStatus::from_raw(3 << 8));
        assert_eq!(
            exited,
            ExitInfo {
                code: Some(3),
                signal: None
            }
        );
        assert_eq!(exited.exit_code(), 3);
        let killed = ExitInfo::of(ExitStatus::from_raw(libc::SIGKILL));
        assert_eq!(
            killed,
            ExitInfo {
                code: None,
                signal: Some(libc::SIGKILL)
            }
        );
        assert_eq!(killed.exit_code(), 128 + libc::SIGKILL);
        assert_eq!(
            code(ExitStatus::from_raw(libc::SIGTERM)),
            128 + libc::SIGTERM
        );
    }

    #[test]
    fn presence_never_calls_a_foreign_process_gone() {
        assert_eq!(presence(std::process::id()), Presence::Present);
        assert_eq!(presence(0), Presence::Unknown);
        assert_eq!(presence(u32::MAX), Presence::Unknown);
        // Another user's process (init, unless the tests run as root).
        if unsafe { libc::kill(1, 0) } != 0 {
            assert_eq!(presence(1), Presence::Unknown);
        }
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // A reaped ID is free unless the system has already reused it.
        assert_ne!(presence(pid), Presence::Unknown);
    }

    #[test]
    fn process_identities_match_the_kernel() {
        let own = std::process::id();
        let identity = process_identity(own).expect("own identity");
        assert_eq!(identity.parent, unsafe { libc::getppid() } as u32);
        assert_eq!(identity.group, Some(unsafe { libc::getpgrp() } as u32));
        assert_eq!(parent_process_id(), identity.parent);
        // Job IDs hash this text: Linux start ticks, macOS seconds:microseconds.
        #[cfg(target_os = "linux")]
        assert!(
            !identity.birth.is_empty() && identity.birth.bytes().all(|b| b.is_ascii_digit()),
            "{}",
            identity.birth
        );
        #[cfg(target_os = "macos")]
        assert!(identity
            .birth
            .split_once(':')
            .is_some_and(
                |(seconds, micros)| seconds.parse::<u64>().is_ok() && micros.parse::<u64>().is_ok()
            ));
        assert_eq!(
            Some(lstart_text(identity.started as u64)),
            process_start_time(own)
        );
        assert_eq!(
            process_table().unwrap()[&own],
            ProcessRow {
                pid: own,
                parent: identity.parent,
                group: identity.group,
            }
        );
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let (parent, name) = parent_and_name(child.id()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(parent, Some(own));
        assert_eq!(
            name.and_then(|name| program_name(Path::new(&name)))
                .as_deref(),
            Some("sleep")
        );
        assert!(process_identity(child.id()).is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn process_images_and_ancestry_read_argv_from_the_kernel() {
        let own = std::process::id();
        let image = describe(own).expect("own image");
        assert_eq!(Path::new(&image.executable), env::current_exe().unwrap());
        assert_eq!(image.argv, Some(env::args().collect::<Vec<_>>()));
        assert_eq!(
            parent_and_argv(own),
            Some((parent_process_id(), env::args().collect()))
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn open_files_and_current_directories_name_what_a_process_uses() {
        let dir = tempfile::tempdir().unwrap();
        let folder = fs::canonicalize(dir.path()).unwrap();
        let path = folder.join("held");
        let file = fs::File::create(&path).unwrap();
        let own = std::process::id();
        assert_eq!(holds_open(own, &path).unwrap(), crate::platform::OpenFiles::Held);
        assert!(open_files(own).unwrap().contains(&path));
        drop(file);
        assert_eq!(holds_open(own, &path).unwrap(), crate::platform::OpenFiles::NotHeld);

        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(&folder)
            .spawn()
            .unwrap();
        // spawn() can return before exec renames the child in /proc, so wait
        // until the child reports its new name.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let directories = loop {
            let directories = current_directories().unwrap();
            let renamed = directories
                .iter()
                .any(|(pid, _, name)| *pid == child.id() && name == "sleep");
            if renamed || std::time::Instant::now() >= deadline {
                break directories;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(directories
            .iter()
            .any(|(pid, cwd, name)| *pid == child.id() && *cwd == folder && name == "sleep"));
        assert!(directories.iter().all(|(pid, _, _)| *pid != own));
    }

    #[test]
    fn pinned_processes_signal_only_their_identity() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let birth = process_identity(child.id()).unwrap().birth;
        #[cfg(target_os = "linux")]
        assert_eq!(
            PinnedProcess::pin(child.id(), "another birth")
                .err()
                .as_deref(),
            Some("Process identity changed")
        );
        let pinned = PinnedProcess::pin(child.id(), &birth).unwrap();
        assert!(pinned.alive());
        pinned.terminate().unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert!(!pinned.alive());
        // Gone: no signal reaches whatever has the ID now.
        pinned.kill().unwrap();
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn pinned_process_states_distinguish_stopped_replaced_and_exited() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let birth = process_identity(child.id()).unwrap().birth;
        let pinned = PinnedProcess::pin(child.id(), &birth).unwrap();
        assert_eq!(pinned.state().unwrap(), ProcessState::Running);
        assert_eq!(process_state(child.id(), Some("different birth")).unwrap(), ProcessState::Exited);
        pinned.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while pinned.state().unwrap() == ProcessState::Running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(pinned.state().unwrap(), ProcessState::Stopped);
        pinned.resume().unwrap();
        pinned.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(pinned.state().unwrap(), ProcessState::Exited);
        assert_eq!(process_state(child.id(), None).unwrap(), ProcessState::Exited);
    }

    #[test]
    fn exit_requests_are_sigterm_and_report_a_missing_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        request_exit(child.id()).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        let error = request_exit(u32::MAX >> 1).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
        // 0 would signal this process group and u32::MAX (-1) every process.
        for pid in [0, u32::MAX, 1 << 31] {
            assert_eq!(
                request_exit(pid).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn shutdown_requests_reach_the_child_and_interrupts_do_not() {
        let installed = |signal| unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut current);
            current.sa_sigaction
        };
        let handler = shutdown_handler as *const () as libc::sighandler_t;
        let guard = ShutdownGuard::install().unwrap();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            assert_eq!(installed(signal), handler);
        }
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        forward_shutdown(&child);
        // The handler as the signals would run it, without signalling the test process.
        shutdown_handler(libc::SIGINT);
        assert_eq!(shutdown_signal(), 0);
        shutdown_handler(libc::SIGTERM);
        assert_eq!(shutdown_signal(), libc::SIGTERM);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        shutdown_child_exited();
        // A request that arrived first reaches a child started afterwards.
        let mut late = Command::new("sleep").arg("30").spawn().unwrap();
        forward_shutdown(&late);
        assert_eq!(late.wait().unwrap().signal(), Some(libc::SIGTERM));
        drop(guard);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            assert_ne!(installed(signal), handler);
        }
    }

    #[test]
    fn detached_children_lead_their_own_session() {
        let mut child = spawn_detached(Command::new("sleep").arg("30")).unwrap();
        let session = unsafe { libc::getsid(child.id() as i32) };
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(session, child.id() as i32);
    }

    #[test]
    fn bounded_input_handle_preserves_bytes_eof_and_output_limits() {
        use std::io::{Seek, SeekFrom, Write};
        let make = || {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(b"literal \x00\xff\n").unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            Stdio::from(file)
        };
        let (status,out,err)=run_bounded_stdin(&mut Command::new("cat"),make(),[64,64],
            Instant::now()+Duration::from_secs(5),&||false).unwrap();
        assert!(status.success());assert_eq!(out,b"literal \x00\xff\n");assert!(err.is_empty());
        assert!(matches!(run_bounded_stdin(&mut Command::new("cat"),make(),[2,64],
            Instant::now()+Duration::from_secs(5),&||false),Err(RunError::TooLarge)));
    }

    #[test]
    fn bounded_runs_collect_output_and_stop_their_whole_group() {
        let never = || false;
        let far = Instant::now() + Duration::from_secs(30);
        let (status, out, err) = run_bounded(
            Command::new("sh").args(["-c", "printf out; printf err >&2; exit 3"]),
            [16, 16],
            far,
            &never,
        )
        .unwrap();
        assert_eq!(
            (status.code(), out.as_slice(), err.as_slice()),
            (Some(3), &b"out"[..], &b"err"[..])
        );
        assert!(matches!(
            run_bounded(
                Command::new("sh").args(["-c", "printf 12345"]),
                [4, 16],
                far,
                &never
            ),
            Err(RunError::TooLarge)
        ));
        assert!(matches!(
            run_bounded(&mut Command::new("hgs-no-such-program"), [4, 4], far, &never),
            Err(RunError::Spawn(error)) if error.kind() == io::ErrorKind::NotFound
        ));
        assert!(matches!(
            run_bounded(Command::new("sleep").arg("30"), [4, 4], far, &|| true),
            Err(RunError::Stopped)
        ));
        // A helper that outlives the shell holds the output open until the
        // deadline, which then stops the whole group, helper included.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper");
        let started = Instant::now();
        let result = run_bounded(
            Command::new("sh").arg("-c").arg(format!(
                "sleep 30 & echo $! > {}; exit 0",
                super::super::quote(&marker.to_string_lossy())
            )),
            [16, 16],
            Instant::now() + Duration::from_millis(300),
            &never,
        );
        assert!(matches!(result, Err(RunError::Stopped)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let ended = |helper: u32| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while process_identity(helper).is_some() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            process_identity(helper).is_none()
        };
        assert!(ended(
            fs::read_to_string(&marker).unwrap().trim().parse().unwrap()
        ));
        // A failed child's helpers end with it too; a successful child's stay.
        for (exit, survives) in [(1, false), (0, true)] {
            let (status, _, _) = run_bounded(
                Command::new("sh").arg("-c").arg(format!(
                    "sleep 30 >/dev/null 2>&1 & echo $! > {}; exit {exit}",
                    super::super::quote(&marker.to_string_lossy())
                )),
                [16, 16],
                far,
                &never,
            )
            .unwrap();
            assert_eq!(status.code(), Some(exit));
            let helper: u32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
            if survives {
                assert!(process_identity(helper).is_some());
                unsafe {
                    libc::kill(helper as i32, libc::SIGKILL);
                }
            } else {
                assert!(ended(helper));
            }
        }
    }

    #[test]
    fn timed_output_goes_through_files_and_stops_at_the_limit() {
        let output = output_until(
            Command::new("sh").args(["-c", "printf out; printf err >&2"]),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(
            (output.stdout.as_slice(), output.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
        let started = Instant::now();
        assert!(matches!(
            output_until(Command::new("sleep").arg("30"), Duration::from_millis(100)),
            Err(RunError::Stopped)
        ));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn private_entries_are_owned_closed_and_linked_once() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("private");
        private_dir(&folder).unwrap();
        assert_eq!(fs::metadata(&folder).unwrap().mode() & 0o777, 0o700);
        assert!(verify_private(&folder, Private::Directory).unwrap());
        assert!(!verify_private(&folder, Private::File).unwrap());
        let file = folder.join("store");
        let handle = private_open(fs::OpenOptions::new().create_new(true).write(true))
            .open(&file)
            .unwrap();
        assert!(verify_private(&file, Private::File).unwrap());
        assert!(verify_private_file(&handle).unwrap());
        fs::hard_link(&file, folder.join("second")).unwrap();
        assert!(!verify_private(&file, Private::File).unwrap());
        assert!(!verify_private_file(&handle).unwrap());
        fs::remove_file(folder.join("second")).unwrap();
        std::os::unix::fs::symlink(&file, folder.join("link")).unwrap();
        assert!(!verify_private(&folder.join("link"), Private::File).unwrap());
        assert!(refuse_link(fs::OpenOptions::new().read(true))
            .open(folder.join("link"))
            .is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(!verify_private(&file, Private::File).unwrap());
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!verify_private(&folder, Private::Directory).unwrap());
        assert!(verify_private(&folder.join("missing"), Private::File).is_err());
    }

    #[test]
    fn opening_a_fifo_without_a_writer_never_blocks() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Without O_NONBLOCK this open would wait for a writer forever.
        let file = never_block(fs::OpenOptions::new().read(true))
            .open(&fifo)
            .unwrap();
        assert!(file.metadata().unwrap().file_type().is_fifo());
    }

    #[test]
    fn file_identities_read_one_moment_of_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"one").unwrap();
        let file = fs::File::open(&path).unwrap();
        let (identity, metadata) = FileIdentity::with_metadata(&file).unwrap();
        assert_eq!(FileIdentity::of(&file).unwrap(), identity);
        assert_eq!(FileIdentity::at(&path).unwrap().0, identity);
        assert_eq!(file_identity(&path), Some(identity));
        // Caches and receipts store these values as they are.
        assert_eq!(
            (identity.device, identity.inode),
            (metadata.dev(), metadata.ino())
        );
        assert_eq!(
            (identity.modified, identity.modified_nanos),
            (metadata.mtime(), metadata.mtime_nsec())
        );
        assert_eq!(
            modified(&metadata),
            (metadata.mtime(), metadata.mtime_nsec())
        );
    }

    #[test]
    fn the_running_image_is_the_file_this_test_started_from() {
        let path = std::env::current_exe().unwrap();
        assert_eq!(image_identity(), Some(file_identity(&path).unwrap()));
    }

    #[test]
    fn names_paths_and_git_settings_keep_their_unix_forms() {
        assert_eq!(
            program_name(Path::new("/usr/bin/node")).as_deref(),
            Some("node")
        );
        assert_eq!(
            program_name(Path::new("bin/codex.js")).as_deref(),
            Some("codex.js")
        );
        assert_eq!(
            program_name(Path::new("Python3.12")).as_deref(),
            Some("Python3.12")
        );
        assert_eq!(program_name(Path::new("/")), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            canonical(dir.path()).unwrap(),
            fs::canonicalize(dir.path()).unwrap()
        );
        assert_eq!(home_dir(), env::var_os("HOME").map(PathBuf::from));
        let mut git = Command::new("git");
        git_noninteractive(&mut git);
        let environment: Vec<_> = git
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.and_then(|v| v.to_str())))
            .collect();
        assert_eq!(
            environment,
            [
                ("GIT_ASKPASS", Some("/bin/false")),
                (
                    "GIT_SSH_COMMAND",
                    Some("ssh -o BatchMode=yes -o ConnectTimeout=2")
                ),
                ("GIT_TERMINAL_PROMPT", Some("0")),
                ("SSH_ASKPASS", Some("/bin/false")),
            ]
        );
    }
}
