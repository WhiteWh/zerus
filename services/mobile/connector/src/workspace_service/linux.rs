//! Linux-only adapter. A pidfd is the only signal target. Never submits a
//! systemd stop/restart/kill job and never signals a PID or process group.
use super::*;
use crate::{check, config, fsguard, journal};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
const UNIT: &str = "zerus-mobile-connector.service";
const DROP: &str = "70-zerus-workspace-connector.conf";
const MARKER: &str =
    "# Written by Zerus workspace migration; only this marked drop-in may be replaced.\n";
const LIMIT: usize = 65_536;
const PROPERTIES: &str = "LoadState,ActiveState,SubState,MainPID,InvocationID,ControlGroup,KillMode,Restart,RestartForceExitStatus,ExecStart,FragmentPath,DropInPaths,ExecStop,ExecStopPost,ExecStartPre,ExecStartPost,OnSuccess,OnFailure,BoundBy,ConsistsOf,PropagatesStopTo,UpheldBy,TriggeredBy,Job,Type,RemainAfterExit,ExitType,FailureAction,SuccessAction,WatchdogUSec,RuntimeMaxUSec,Result,ExecMainStatus,ExecCondition,Conflicts,Requires,Wants,BindsTo,Upholds,Slice,StopWhenUnneeded,MemoryMax,MemoryHigh,CPUQuotaPerSecUSec,TasksMax,RootDirectory,RootImage,RootMStack,BindPaths,BindReadOnlyPaths,TemporaryFileSystem,MountImages,ExtensionImages,ExtensionDirectories,InaccessiblePaths,Environment,EnvironmentFiles,PassEnvironment";
type Unit = BTreeMap<String, String>;
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Image {
    path: PathBuf,
    dev: u64,
    ino: u64,
    sha256: String,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Process {
    pid: i32,
    start: String,
    invocation: String,
    image: Image,
    config_dev: u64,
    config_ino: u64,
    lock_dev: u64,
    lock_ino: u64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Details {
    source_files: BTreeMap<PathBuf, String>,
    config: PathBuf,
    unit: Unit,
    process: Option<Process>,
    desired: Image,
    entry: PathBuf,
    drop_path: PathBuf,
    drop_bytes: String,
    drop_previous: Option<String>,
    definition_verified: bool,
    #[serde(default)]
    managed: Option<Managed>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Managed {
    process: Process,
    generation: String,
    state: PathBuf,
    revision: String,
}
fn refused() -> ConnectorError {
    failure()
}
async fn output(program: &Path, args: &[String]) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| refused())?;
    let stdout = child.stdout.take().ok_or_else(refused)?;
    let stderr = child.stderr.take().ok_or_else(refused)?;
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let a = async {
            let mut b = Vec::new();
            stdout
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut b)
                .await
                .map(|_| b)
        };
        let b = async {
            let mut b = Vec::new();
            stderr
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut b)
                .await
                .map(|_| b)
        };
        let ((out, err), status) =
            futures_util::future::join(futures_util::future::join(a, b), child.wait()).await;
        let out = out.map_err(|_| refused())?;
        let err = err.map_err(|_| refused())?;
        if !status.map_err(|_| refused())?.success() || out.len() > LIMIT || err.len() > LIMIT {
            return Err(refused());
        }
        Ok(out)
    })
    .await
    .map_err(|_| refused())?;
    result
}
#[cfg(feature = "test-hooks")]
#[derive(Clone)]
pub struct TestPaths {
    pub manager: PathBuf,
    pub binary: PathBuf,
    pub dropin: PathBuf,
    pub cgroup: PathBuf,
}
#[cfg(feature = "test-hooks")]
tokio::task_local! { static TEST_PATHS: TestPaths; }
#[cfg(feature = "test-hooks")]
pub async fn test_scope<T>(paths: TestPaths, future: impl std::future::Future<Output = T>) -> T {
    TEST_PATHS.scope(paths, future).await
}
fn manager_program() -> PathBuf {
    #[cfg(feature = "test-hooks")]
    if let Ok(path) = TEST_PATHS.try_with(|p| p.manager.clone()) {
        return path;
    }
    PathBuf::from("/usr/bin/systemctl")
}
fn current_binary() -> Result<PathBuf> {
    #[cfg(feature = "test-hooks")]
    if let Ok(path) = TEST_PATHS.try_with(|p| p.binary.clone()) {
        return Ok(path);
    }
    std::env::current_exe().map_err(|_| refused())
}
fn current_entry(desired: &Image) -> Result<PathBuf> {
    #[cfg(feature = "test-hooks")]
    if let Ok(path) = TEST_PATHS.try_with(|p| p.binary.clone()) {
        return Ok(path);
    }
    if let Some(path) = std::env::args_os().next().map(PathBuf::from) {
        let safe = path.is_absolute()
            && path.components().all(|c| {
                !matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(|b| b" \t\n\r\"'\\%$;".contains(b));
        if safe
            && path.parent().is_some_and(|p| safe_path(p).is_ok())
            && image(&path).is_ok_and(|actual| actual == *desired)
        {
            return Ok(path);
        }
    }
    Ok(desired.path.clone())
}
async fn manager(args: &[&str]) -> Result<Vec<u8>> {
    output(
        &manager_program(),
        &std::iter::once("--user")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
    .await
}
async fn unit() -> Result<Unit> {
    let bytes = manager(&["show", UNIT, "--property", PROPERTIES, "--no-pager"]).await?;
    let text = std::str::from_utf8(&bytes).map_err(|_| refused())?;
    let mut unit = Unit::new();
    for line in text.lines() {
        let (k, v) = line.split_once('=').ok_or_else(refused)?;
        if unit.insert(k.into(), v.into()).is_some() {
            return Err(refused());
        }
    }
    // systemctl omits these documented nil-array properties even with an
    // explicit property query. Scalar and other mandatory fields stay strict.
    for key in [
        "ExecStop",
        "ExecStopPost",
        "ExecStartPre",
        "ExecStartPost",
        "ExecCondition",
        "EnvironmentFiles",
    ] {
        unit.entry(key.into()).or_default();
    }
    Ok(unit)
}
fn get<'a>(unit: &'a Unit, key: &str) -> Result<&'a str> {
    unit.get(key).map(String::as_str).ok_or_else(refused)
}
fn policy(unit: &Unit) -> Result<()> {
    if get(unit, "LoadState")? != "loaded"
        || !matches!(get(unit, "Type")?, "simple" | "exec")
        || get(unit, "KillMode")? != "process"
        || !matches!(get(unit, "Restart")?, "no" | "on-failure")
        || get(unit, "RemainAfterExit")? != "no"
        || unit.get("ExitType").is_some_and(|s| s != "main")
        || !matches!(get(unit, "Job")?, "" | "0" | "0 /")
    {
        return Err(refused());
    }
    for k in [
        "RestartForceExitStatus",
        "ExecCondition",
        "ExecStop",
        "ExecStopPost",
        "ExecStartPre",
        "ExecStartPost",
        "OnSuccess",
        "OnFailure",
        "BoundBy",
        "ConsistsOf",
        "PropagatesStopTo",
        "UpheldBy",
        "TriggeredBy",
    ] {
        if !get(unit, k)?.is_empty() {
            return Err(refused());
        }
    }
    for key in [
        "RootDirectory",
        "RootImage",
        "BindPaths",
        "BindReadOnlyPaths",
        "TemporaryFileSystem",
        "MountImages",
        "ExtensionImages",
        "ExtensionDirectories",
        "InaccessiblePaths",
        "EnvironmentFiles",
        "PassEnvironment",
    ] {
        if !get(unit, key)?.is_empty() {
            return Err(refused());
        }
    }
    // This key is absent on older managers. Any advertised remapped root refuses.
    if unit.get("RootMStack").is_some_and(|v| !v.is_empty()) {
        return Err(refused());
    }
    let environment = get(unit, "Environment")?;
    // Preserve the installer PATH assignment; no additional assignment/loader
    // override or external environment-file authority is admitted. This is a
    // unit policy, not a claim to sanitize the user manager's inherited env.
    if !environment.is_empty()
        && (!environment.starts_with("PATH=")
            || environment
                .chars()
                .any(|c| c.is_whitespace() || "\\\"'".contains(c)))
    {
        return Err(refused());
    }
    if get(unit, "StopWhenUnneeded")? != "no"
        || get(unit, "Conflicts")?
            .split_whitespace()
            .any(|x| x != "shutdown.target")
        || ["Requires", "Wants", "BindsTo", "Upholds"]
            .into_iter()
            .any(|k| {
                unit.get(k).is_none_or(|v| {
                    v.split_whitespace().any(|x| {
                        // systemd places ordinary user services in app.slice
                        // and implicitly requires that exact parent slice.
                        !matches!(x, "basic.target" | "sysinit.target")
                            && !(k == "Requires"
                                && x == "app.slice"
                                && unit.get("Slice").is_some_and(|s| s == "app.slice"))
                    })
                })
            })
    {
        return Err(refused());
    }
    if get(unit, "FailureAction")? != "none"
        || get(unit, "SuccessAction")? != "none"
        || get(unit, "WatchdogUSec")? != "0"
        || !matches!(
            get(unit, "RuntimeMaxUSec")?,
            "infinity" | "18446744073709551615"
        )
    {
        return Err(refused());
    }
    Ok(())
}
fn arguments(unit: &Unit) -> Result<Vec<String>> {
    let text = get(unit, "ExecStart")?;
    if text.matches("argv[]=").count() != 1 {
        return Err(refused());
    }
    let argv = text
        .split_once("argv[]=")
        .ok_or_else(refused)?
        .1
        .split_once(" ;")
        .ok_or_else(refused)?
        .0;
    if argv.chars().any(|c| "\t\r\n\"'\\$;".contains(c)) {
        return Err(refused());
    }
    let args = argv.split(' ').map(str::to_owned).collect::<Vec<_>>();
    if args.len() != 3
        || args[1] != "--config"
        || Path::new(&args[0]).file_name().and_then(|s| s.to_str())
            != Some("zerus-mobile-connector")
    {
        return Err(refused());
    }
    Ok(args)
}
fn safe_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| b" \t\n\r\"'\\%$;".contains(b))
    {
        return Err(refused());
    }
    for p in path.ancestors() {
        let m = p.symlink_metadata().map_err(|_| refused())?;
        if m.file_type().is_symlink()
            || !matches!(m.uid(), 0) && m.uid() != fsguard::euid()
            || m.mode() & 0o022 != 0
        {
            return Err(refused());
        }
    }
    Ok(())
}
fn image(path: &Path) -> Result<Image> {
    let path = path.canonicalize().map_err(|_| refused())?;
    safe_path(&path)?;
    let f = regular(&path)?;
    let m = f.metadata().map_err(|_| refused())?;
    if !m.is_file()
        || m.len() > 64 * 1024 * 1024
        || m.mode() & 0o111 == 0
        || m.nlink() == 0
        || path.file_name().and_then(|x| x.to_str()) != Some("zerus-mobile-connector")
    {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    f.take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(refused());
    }
    if !bytes.starts_with(b"\x7fELF") {
        return Err(refused());
    }
    Ok(Image {
        path,
        dev: m.dev(),
        ino: m.ino(),
        sha256: zerus_relay::json::digest(bytes),
    })
}
fn regular(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)
        .map_err(|_| refused())?;
    if !f.metadata().map_err(|_| refused())?.is_file() {
        return Err(refused());
    }
    Ok(f)
}
fn bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    regular(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut b)
        .map_err(|_| refused())?;
    if b.len() > limit {
        return Err(refused());
    }
    Ok(b)
}

fn process(unit: &Unit, binding: &Binding) -> Result<Process> {
    process_bound(unit, binding, None)
}
fn process_bound(unit: &Unit, binding: &Binding, observed: Option<&Process>) -> Result<Process> {
    let pid = get(unit, "MainPID")?
        .parse::<i32>()
        .map_err(|_| refused())?;
    if pid <= 1 {
        return Err(refused());
    }
    let base = PathBuf::from(format!("/proc/{pid}"));
    let stat = String::from_utf8(bounded(&base.join("stat"), 4096)?).map_err(|_| refused())?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(refused)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let start = fields.get(19).ok_or_else(refused)?.to_string();
    let status = String::from_utf8(bounded(&base.join("status"), LIMIT)?).map_err(|_| refused())?;
    let uid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:\t"))
        .ok_or_else(refused)?;
    if uid.split_whitespace().count() != 4
        || uid
            .split_whitespace()
            .take(2)
            .any(|s| s.parse::<u32>().ok() != Some(fsguard::euid()))
    {
        return Err(refused());
    }
    let raw = bounded(&base.join("cmdline"), LIMIT)?;
    let args = raw
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| String::from_utf8(b.to_vec()).map_err(|_| refused()))
        .collect::<Result<Vec<_>>>()?;
    let args = match args.as_slice() {
        [exe, admit, revision, flag, config]
            if admit == "--admit-revision" && revision == &binding.revision =>
        {
            vec![exe.clone(), flag.clone(), config.clone()]
        }
        _ => args,
    };
    if args.len() != 3 || args[1] != "--config" || Path::new(&args[2]) != binding.config {
        return Err(refused());
    }
    let running = if let Some(observed) = observed {
        // Stop may target the retained old image after an installed binary was
        // atomically replaced. Verify the kernel-held bytes/inode against the
        // observed generation, never adopt the new pathname's executable.
        let file = File::open(base.join("exe")).map_err(|_| refused())?;
        let metadata = file.metadata().map_err(|_| refused())?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused())?;
        if !bytes.starts_with(b"\x7fELF")
            || bytes.len() > 64 * 1024 * 1024
            || metadata.dev() != observed.image.dev
            || metadata.ino() != observed.image.ino
            || zerus_relay::json::digest(bytes) != observed.image.sha256
        {
            return Err(refused());
        }
        observed.image.clone()
    } else {
        image(&base.join("exe"))?
    };
    let declared = arguments(unit)?;
    if Path::new(&declared[2]) != binding.config
        || if observed.is_some() {
            Path::new(&declared[0]) != running.path
        } else {
            image(Path::new(&declared[0]))? != running
        }
    {
        return Err(refused());
    }
    let cfg = binding.config.symlink_metadata().map_err(|_| refused())?;
    let lock =
        fsguard::private_file(&binding.state.join(journal::LOCK), false)?.ok_or_else(refused)?;
    let m = lock.metadata().map_err(|_| refused())?;
    let fds = std::fs::read_dir(base.join("fdinfo")).map_err(|_| refused())?;
    let mut owns = false;
    for (n, entry) in fds.enumerate() {
        if n >= 512 {
            return Err(refused());
        }
        let entry = entry.map_err(|_| refused())?;
        let file = match File::open(entry.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(refused()),
        };
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused())?;
        if bytes.len() > 16 * 1024 {
            return Err(refused());
        }
        let text = String::from_utf8(bytes).map_err(|_| refused())?;
        for line in text.lines().filter(|l| l.starts_with("lock:")) {
            let p = line.split_whitespace().collect::<Vec<_>>();
            if p.len() >= 7
                && p[2] == "FLOCK"
                && p[4] == "WRITE"
                && p[5].parse::<i32>().ok() == Some(pid)
            {
                let k = p[6].split(':').collect::<Vec<_>>();
                if k.len() == 3
                    && u32::from_str_radix(k[0], 16).ok() == Some(rustix::fs::major(m.dev()))
                    && u32::from_str_radix(k[1], 16).ok() == Some(rustix::fs::minor(m.dev()))
                    && k[2].parse::<u64>().ok() == Some(m.ino())
                {
                    owns = true
                }
            }
        }
    }
    if !owns || workspace_cli::read_config(&binding.config)?.1 != binding.revision {
        return Err(refused());
    }
    Ok(Process {
        pid,
        start,
        invocation: get(unit, "InvocationID")?.into(),
        image: running,
        config_dev: cfg.dev(),
        config_ino: cfg.ino(),
        lock_dev: m.dev(),
        lock_ino: m.ino(),
    })
}
// The unit snapshot proves service policy; cgroup membership proves that no
// agent, shell, extra process or delegated scope shares its exit lifecycle.
fn process_group(pid: i32) -> Result<String> {
    #[cfg(feature = "test-hooks")]
    if let Ok(path) = TEST_PATHS.try_with(|p| p.cgroup.join("proc-cgroup")) {
        return group_path(&bounded(&path, 4096)?);
    }
    group_path(&bounded(Path::new(&format!("/proc/{pid}/cgroup")), 4096)?)
}
fn group_path(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
    let mut lines = text.lines();
    let group = lines
        .next()
        .and_then(|line| line.strip_prefix("0::"))
        .ok_or_else(refused)?;
    if lines.next().is_some()
        || !group.starts_with('/')
        || (group != "/"
            && group
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == ".."))
    {
        return Err(refused());
    }
    Ok(group.into())
}
fn only_main_process(unit: &Unit, pid: i32) -> Result<()> {
    let group = get(unit, "ControlGroup")?;
    if group_path(format!("0::{group}\n").as_bytes())? != group
        || process_group(pid)? != group
        || !group.ends_with(&format!("/{UNIT}"))
    {
        return Err(refused());
    }
    #[cfg(feature = "test-hooks")]
    let root = TEST_PATHS
        .try_with(|p| p.cgroup.clone())
        .unwrap_or_else(|_| PathBuf::from("/sys/fs/cgroup"));
    #[cfg(not(feature = "test-hooks"))]
    let root = PathBuf::from("/sys/fs/cgroup");
    let directory = root.join(group.strip_prefix('/').ok_or_else(refused)?);
    // No symlink traversal and no delegated subgroup, even an empty one: another
    // scope can acquire members without changing the parent's cgroup.procs.
    for path in directory.ancestors().take_while(|p| p.starts_with(&root)) {
        if !path
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            return Err(refused());
        }
    }
    let entries = std::fs::read_dir(&directory).map_err(|_| refused())?;
    for (n, entry) in entries.enumerate() {
        if n >= 256
            || entry
                .map_err(|_| refused())?
                .file_type()
                .map_err(|_| refused())?
                .is_dir()
        {
            return Err(refused());
        }
    }
    let bytes = bounded(&directory.join("cgroup.procs"), LIMIT)?;
    let pids = std::str::from_utf8(&bytes)
        .map_err(|_| refused())?
        .split_whitespace()
        .collect::<Vec<_>>();
    if pids != [pid.to_string()] {
        return Err(refused());
    }
    Ok(())
}
fn control_matches(control: &Value, process: &Process) -> Result<()> {
    if control.as_object().map(|o| o.len()) != Some(3)
        || control["version"] != 1
        || control["pid"].as_i64() != Some(process.pid as i64)
        || !control["generation"]
            .as_str()
            .is_some_and(|g| g.len() == 32 && g.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(refused());
    }
    Ok(())
}
pub(super) async fn stop_preflight(
    record: Option<&Record>,
    binding: &Binding,
    control: &Value,
) -> Result<()> {
    let before = unit().await?;
    if get(&before, "ActiveState")? == "active" {
        let record = record.ok_or_else(refused)?;
        if record.binding.config != binding.config
            || record.activated_revision.as_deref() != Some(&binding.revision)
            || record.managed_node_id.as_deref() != Some(&binding.node_id)
            || record.transfer.is_some()
        {
            return Err(refused());
        }
        let detail = details(record)?;
        // A managed marked definition must still match the saved handover; no
        // mutation, reload, signal or manager Stop is part of this admission.
        if !detail.definition_verified {
            return Err(refused());
        }
        definition_shape(&detail, &before)?;
        let observed = detail.managed.as_ref().ok_or_else(refused)?;
        let process = process_bound(&before, binding, Some(&observed.process))?;
        if observed.process != process
            || observed.revision != binding.revision
            || observed.state != binding.state
            || control["generation"] != observed.generation
        {
            return Err(refused());
        }
        control_matches(control, &process)?;
        only_main_process(&before, process.pid)?;
        let after = unit().await?;
        definition_shape(&detail, &after)?;
        if process_bound(&after, binding, Some(&observed.process))? != process {
            return Err(refused());
        }
        only_main_process(&after, process.pid)
    } else {
        // A known inactive/absent unit grants no service authority. The only
        // exception is a separately verified foreground process, never an
        // unobserved or orphaned managed start.
        if !matches!(get(&before, "LoadState")?, "loaded" | "not-found")
            || get(&before, "ActiveState")? != "inactive"
            || get(&before, "MainPID")? != "0"
            || record.is_some_and(|r| r.managed_node_id.is_some())
        {
            return Err(refused());
        }
        let pid = control["pid"]
            .as_i64()
            .and_then(|p| i32::try_from(p).ok())
            .filter(|p| *p > 1)
            .ok_or_else(refused)?;
        let group = process_group(pid)?;
        if group.split('/').any(|p| {
            p.ends_with(".service") && !(p.starts_with("user@") && p.ends_with(".service"))
        }) {
            return Err(refused());
        }
        let exe = image(Path::new(&format!("/proc/{pid}/exe")))?;
        safe_path(&binding.config)?;
        let foreground = Unit::from([
            ("MainPID".into(), pid.to_string()),
            ("InvocationID".into(), String::new()),
            (
                "ExecStart".into(),
                format!(
                    "{{ argv[]={} --config {} ; }}",
                    exe.path.display(),
                    binding.config.display()
                ),
            ),
        ]);
        let process = process(&foreground, binding)?;
        control_matches(control, &process)?;
        let after = unit().await?;
        if before != after
            || process_group(pid)? != group
            || self::process(&foreground, binding)? != process
        {
            return Err(refused());
        }
        Ok(())
    }
}
fn same_definition(before: &Unit, after: &Unit) -> Result<()> {
    policy(after)?;
    for (k, v) in before {
        if !matches!(
            k.as_str(),
            "ExecStart"
                | "DropInPaths"
                | "ControlGroup"
                | "ActiveState"
                | "SubState"
                | "MainPID"
                | "Job"
                | "InvocationID"
                | "Result"
                | "ExecMainStatus"
        ) {
            let unchanged = if k == "Requires" {
                // systemd may render the same dependencies in another order
                // after reload. Membership must stay exact, including the
                // ordinary application slice admitted by policy above.
                after.get(k).is_some_and(|next| {
                    v.split_whitespace()
                        .collect::<std::collections::BTreeSet<_>>()
                        == next
                            .split_whitespace()
                            .collect::<std::collections::BTreeSet<_>>()
                })
            } else {
                after.get(k) == Some(v)
            };
            if !unchanged {
                return Err(refused());
            }
        }
    }
    Ok(())
}
fn drop_path() -> Result<PathBuf> {
    #[cfg(feature = "test-hooks")]
    if let Ok(path) = TEST_PATHS.try_with(|p| p.dropin.clone()) {
        return Ok(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(refused)?;
    Ok(base
        .join("systemd/user")
        .join(format!("{UNIT}.d"))
        .join(DROP))
}
// Open every component relative to an already verified directory. A missing
// final component is the only absent case; dangling/foreign symlinks refuse.
fn directory(path: &Path, create: bool) -> Result<Option<File>> {
    use rustix::fs::{mkdirat, openat, Mode, OFlags};
    if !path.is_absolute() {
        return Err(refused());
    }
    let mut current = File::open("/").map_err(|_| refused())?;
    for component in path.components().skip(1) {
        let std::path::Component::Normal(name) = component else {
            return Err(refused());
        };
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let next = match openat(&current, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(error) if error == rustix::io::Errno::NOENT => {
                if !create {
                    return Ok(None);
                }
                mkdirat(&current, name, Mode::from_raw_mode(0o755)).map_err(|_| refused())?;
                current.sync_all().map_err(|_| refused())?;
                openat(&current, name, flags, Mode::empty()).map_err(|_| refused())?
            }
            Err(_) => return Err(refused()),
        };
        let file: File = next.into();
        let m = file.metadata().map_err(|_| refused())?;
        if (m.uid() != 0 && m.uid() != fsguard::euid()) || m.mode() & 0o022 != 0 {
            return Err(refused());
        }
        current = file;
    }
    Ok(Some(current))
}
fn drop_read_at(directory: &File, name: &std::ffi::OsStr) -> Result<Option<String>> {
    use rustix::fs::{openat, Mode, OFlags};
    let fd = match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(e) if e == rustix::io::Errno::NOENT => return Ok(None),
        Err(_) => return Err(refused()),
    };
    let file: File = fd.into();
    let m = file.metadata().map_err(|_| refused())?;
    if !m.is_file() || m.uid() != fsguard::euid() || m.nlink() != 1 || m.mode() & 0o022 != 0 {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    if bytes.len() > LIMIT {
        return Err(refused());
    }
    let s = String::from_utf8(bytes).map_err(|_| refused())?;
    if !s.starts_with(MARKER) {
        return Err(refused());
    }
    Ok(Some(s))
}
fn drop_read(path: &Path) -> Result<Option<String>> {
    let Some(parent) = directory(path.parent().ok_or_else(refused)?, false)? else {
        return Ok(None);
    };
    drop_read_at(&parent, path.file_name().ok_or_else(refused)?)
}
fn drop_write(path: &Path, expected: Option<&str>, value: Option<&str>) -> Result<()> {
    use rustix::fs::{
        openat, renameat, renameat_with, unlinkat, AtFlags, Mode, OFlags, RenameFlags,
    };
    let parent = directory(path.parent().ok_or_else(refused)?, true)?.ok_or_else(refused)?;
    let name = path.file_name().ok_or_else(refused)?;
    if drop_read_at(&parent, name)?.as_deref() != expected {
        return Err(refused());
    }
    if let Some(value) = value {
        let temporary = format!(
            ".workspace-{}",
            crate::private_state::random_id().map_err(|_| refused())?
        );
        let fd = openat(
            &parent,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .map_err(|_| refused())?;
        let mut f: File = fd.into();
        f.write_all(value.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|_| refused())?;
        if drop_read_at(&parent, name)?.as_deref() != expected {
            let _ = unlinkat(&parent, temporary.as_str(), AtFlags::empty());
            return Err(refused());
        }
        let result = if expected.is_none() {
            renameat_with(
                &parent,
                temporary.as_str(),
                &parent,
                name,
                RenameFlags::NOREPLACE,
            )
        } else {
            renameat(&parent, temporary.as_str(), &parent, name)
        };
        if result.is_err() {
            let _ = unlinkat(&parent, temporary.as_str(), AtFlags::empty());
            return Err(refused());
        }
    } else {
        unlinkat(&parent, name, AtFlags::empty()).map_err(|_| refused())?
    }
    parent.sync_all().map_err(|_| refused())
}

fn details(record: &Record) -> Result<Details> {
    serde_json::from_value(record.details.clone()).map_err(|_| refused())
}
fn remember(store: &Store, record: &mut Record, detail: &Details) -> Result<()> {
    record.details = serde_json::to_value(detail).map_err(|_| refused())?;
    save(store, record)
}
async fn prepare(store: &Store, record: &mut Record) -> Result<Details> {
    safe_path(&record.binding.config)?;
    let before = unit().await?;
    policy(&before)?;
    let config = arguments(&before)?;
    if Path::new(&config[2]) != record.binding.config {
        return Err(refused());
    }
    image(Path::new(&config[0]))?; // An inactive Python/interpreter unit is also unsupported.
    let desired = image(&current_binary()?)?;
    let entry = current_entry(&desired)?;
    let running = if get(&before, "ActiveState")? == "active" {
        Some(process(&before, &record.binding)?)
    } else if get(&before, "ActiveState")? == "inactive" && get(&before, "MainPID")? == "0" {
        None
    } else {
        return Err(refused());
    };
    if let Some(p) = &running {
        // Rust's documented read-only check distinguishes it from the Python
        // entrypoint. Execute the retained running image, never an interpreter.
        let retained = File::open(format!("/proc/{}/exe", p.pid)).map_err(|_| refused())?;
        let metadata = retained.metadata().map_err(|_| refused())?;
        let mut image_bytes = Vec::new();
        retained
            .try_clone()
            .map_err(|_| refused())?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut image_bytes)
            .map_err(|_| refused())?;
        if metadata.dev() != p.image.dev
            || metadata.ino() != p.image.ino
            || image_bytes.len() > 64 * 1024 * 1024
            || zerus_relay::json::digest(image_bytes) != p.image.sha256
        {
            return Err(refused());
        }
        // The descriptor survives in the exec child until ELF path resolution;
        // CLOEXEC closes it only on successful exec. No numeric PID is reused.
        let executable = rustix::io::fcntl_dupfd_cloexec(&retained, 3).map_err(|_| refused())?;
        let retained_path = PathBuf::from(format!("/proc/self/fd/{}", executable.as_raw_fd()));
        let bytes = output(
            &retained_path,
            &[
                "--check".into(),
                "--json".into(),
                "--config".into(),
                record.binding.config.to_string_lossy().into(),
            ],
        )
        .await?;
        let report: Value = serde_json::from_slice(&bytes).map_err(|_| refused())?;
        if report["ok"] != true
            || report["state"]["journal"]["binding"] != "matches"
            || report["configuration"]["state_dir"] != json!(record.binding.state)
            || report["state"]["lock"] != "held"
            || process(&unit().await?, &record.binding)? != *p
        {
            return Err(refused());
        }
    }
    let report = check::report(
        &record.binding.config,
        false,
        &config::Environment::current().map_err(|_| refused())?,
        fsguard::euid(),
    );
    if report["ok"] != true || report["state"]["journal"]["binding"] != "matches" {
        return Err(refused());
    }
    let path = drop_path()?;
    let previous = drop_read(&path)?;
    let text = format!(
        "{MARKER}[Service]\nExecStart=\nExecStart={} --config {}\n",
        entry.display(),
        record.binding.config.display()
    );
    let mut source_files = BTreeMap::new();
    let source_paths = std::iter::once(get(&before, "FragmentPath")?)
        .chain(get(&before, "DropInPaths")?.split_whitespace())
        .collect::<Vec<_>>();
    if source_paths.len() > 64 {
        return Err(refused());
    }
    for source in source_paths {
        let source = PathBuf::from(source);
        safe_path(&source)?;
        let bytes = bounded(&source, LIMIT)?;
        source_files.insert(source, zerus_relay::json::digest(bytes));
    }
    let mut detail = Details {
        source_files,
        config: record.binding.config.clone(),
        unit: before,
        process: running,
        desired,
        entry,
        drop_path: path,
        drop_bytes: text,
        drop_previous: previous,
        definition_verified: false,
        managed: None,
    };
    remember(store, record, &detail)?;
    drop_write(
        &detail.drop_path,
        detail.drop_previous.as_deref(),
        Some(&detail.drop_bytes),
    )?;
    // A reload timeout is unknown: retain our file and reservation, do not signal.
    manager(&["daemon-reload"]).await?;
    let after = unit().await?;
    if definition_matches(&detail, &after).is_err() {
        drop_write(
            &detail.drop_path,
            Some(&detail.drop_bytes),
            detail.drop_previous.as_deref(),
        )?;
        manager(&["daemon-reload"]).await?;
        // Known successful rollback grants no remaining service authority.
        // An explicitly stopped legacy connector can still use manual CAS.
        record.details = Value::Null;
        save(store, record)?;
        return Err(refused());
    }
    detail.definition_verified = true;
    remember(store, record, &detail)?;
    Ok(detail)
}
fn definition_shape(detail: &Details, current: &Unit) -> Result<()> {
    same_definition(&detail.unit, current)?;
    let mut wanted = get(&detail.unit, "DropInPaths")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    wanted.insert(detail.drop_path.to_string_lossy().into_owned());
    let actual = get(current, "DropInPaths")?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    if actual != wanted {
        return Err(refused());
    }
    for (path, expected) in &detail.source_files {
        if path != &detail.drop_path {
            safe_path(path)?;
            if zerus_relay::json::digest(bounded(path, LIMIT)?) != *expected {
                return Err(refused());
            }
        }
    }
    if drop_read(&detail.drop_path)?.as_deref() != Some(&detail.drop_bytes)
        || arguments(current)?
            != vec![
                detail.entry.to_string_lossy().into_owned(),
                "--config".into(),
                detail.config.to_string_lossy().into_owned(),
            ]
    {
        return Err(refused());
    }
    Ok(())
}
fn definition_matches(detail: &Details, current: &Unit) -> Result<()> {
    definition_shape(detail, current)?;
    if image(&detail.entry)? != detail.desired {
        return Err(refused());
    }
    Ok(())
}

fn after_definition(detail: &Details, current: &Unit) -> Result<()> {
    if !detail.definition_verified {
        return Err(refused());
    }
    definition_matches(detail, current)
}
fn original_exited(process: &Process) -> Result<bool> {
    let path = PathBuf::from(format!("/proc/{}/stat", process.pid));
    match File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(refused()),
        Ok(f) => {
            let mut s = String::new();
            f.take(4097).read_to_string(&mut s).map_err(|_| refused())?;
            if s.len() > 4096 {
                return Err(refused());
            }
            let fields = s
                .rsplit_once(") ")
                .ok_or_else(refused)?
                .1
                .split_whitespace()
                .collect::<Vec<_>>();
            let start = *fields.get(19).ok_or_else(refused)?;
            Ok(start != process.start || fields.first() == Some(&"Z"))
        }
    }
}

pub(super) async fn drain(store: &Store, record: &mut Record) -> Result<Option<&'static str>> {
    if workspace_cli::read_config(&record.binding.config)?.1 != record.binding.revision {
        return Ok(Some("identity-changed"));
    }
    let mut detail = if record.details.is_null() {
        match prepare(store, record).await {
            Ok(d) => d,
            Err(_) => return Ok(Some("unsafe-unit")),
        }
    } else {
        details(record)?
    };
    let current = unit().await?;
    if !detail.definition_verified {
        if definition_matches(&detail, &current).is_err() {
            return Ok(Some("service-unconfirmed"));
        }
        detail.definition_verified = true;
        remember(store, record, &detail)?;
    }
    if after_definition(&detail, &current).is_err() {
        return Ok(Some("service-definition-conflict"));
    }
    if get(&current, "ActiveState")? == "inactive" && get(&current, "MainPID")? == "0" {
        if get(&current, "Result")? != "success" || get(&current, "ExecMainStatus")? != "0" {
            return Ok(Some("identity-changed"));
        }
        if let Some(p) = &detail.process {
            if !original_exited(p)? {
                return Ok(Some("identity-changed"));
            }
            let invocation = get(&current, "InvocationID")?;
            if !invocation.is_empty() && invocation != p.invocation {
                return Ok(Some("identity-changed"));
            }
            record.process_exited = true;
            remember(store, record, &detail)?;
        }
        verify_files(record, &detail)?;
        if !matches!(
            fsguard::probe_lock(&record.binding.state.join(journal::LOCK)),
            fsguard::LockState::Free | fsguard::LockState::Absent
        ) {
            return Ok(Some("journal-unavailable"));
        }
        return Ok(None);
    }
    let Some(expected) = detail.process.clone() else {
        return Ok(Some("identity-changed"));
    };
    // ExecStart now points at the new binary; the old live process must still
    // match the previously retained image, config, journal owner and invocation.
    let mut old_unit = current.clone();
    old_unit.insert("ExecStart".into(), get(&detail.unit, "ExecStart")?.into());
    let actual = process(&old_unit, &record.binding)?;
    if actual != expected {
        return Ok(Some("identity-changed"));
    }
    let pid = rustix::process::Pid::from_raw(expected.pid).ok_or_else(refused)?;
    let fd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
        .map_err(|_| refused())?;
    let mut again = unit().await?;
    if after_definition(&detail, &again).is_err() {
        return Ok(Some("service-definition-conflict"));
    }
    again.insert("ExecStart".into(), get(&detail.unit, "ExecStart")?.into());
    if process(&again, &record.binding)? != expected {
        return Ok(Some("identity-changed"));
    }
    // Tokio 1.53.3's replacement requires unsafe code, forbidden by this crate.
    // pidfd_open supplies an OwnedFd whose descriptor and file description stay
    // open and stable until AsyncFd drops. Only get_ref/readable borrow it here;
    // it is never swapped, closed, extracted or accessed via get_mut/into_inner.
    #[allow(deprecated)]
    let fd = tokio::io::unix::AsyncFd::new(fd).map_err(|_| refused())?;
    if !record.signal_reserved {
        record.signal_reserved = true;
        record.phase = "draining".into();
        record.reason = "draining".into();
        remember(store, record, &detail)?;
        let mut fresh = unit().await?;
        if after_definition(&detail, &fresh).is_err() {
            return Ok(Some("service-definition-conflict"));
        }
        fresh.insert("ExecStart".into(), get(&detail.unit, "ExecStart")?.into());
        if process(&fresh, &record.binding)? != expected {
            return Ok(Some("identity-changed"));
        }
        // The durable record precedes delivery. A crash/error/timeout after this
        // point never permits Check to send a second signal.
        if rustix::process::pidfd_send_signal(fd.get_ref(), rustix::process::Signal::TERM).is_err()
        {
            return Ok(Some("draining"));
        }
    }
    if !matches!(
        tokio::time::timeout(Duration::from_secs(3), fd.readable()).await,
        Ok(Ok(_))
    ) {
        return Ok(Some("drain-timeout"));
    }
    record.process_exited = true;
    remember(store, record, &detail)?;
    let current = unit().await?;
    if after_definition(&detail, &current).is_err()
        || get(&current, "ActiveState")? != "inactive"
        || get(&current, "MainPID")? != "0"
    {
        return Ok(Some("identity-changed"));
    }
    if !matches!(
        fsguard::probe_lock(&record.binding.state.join(journal::LOCK)),
        fsguard::LockState::Free | fsguard::LockState::Absent
    ) {
        return Ok(Some("journal-unavailable"));
    }
    if get(&current, "Result")? != "success"
        || get(&current, "ExecMainStatus")? != "0"
        || (!get(&current, "InvocationID")?.is_empty()
            && get(&current, "InvocationID")? != expected.invocation)
    {
        return Ok(Some("identity-changed"));
    }
    verify_files(record, &detail)?;
    // Keep the retained old process proof: a replacement generation is refused.
    detail.definition_verified = true;
    remember(store, record, &detail)?;
    Ok(None)
}

fn verify_files(record: &Record, detail: &Details) -> Result<()> {
    if workspace_cli::read_config(&record.binding.config)?.1 != record.binding.revision {
        return Err(refused());
    }
    if let Some(p) = &detail.process {
        let cfg = record
            .binding
            .config
            .symlink_metadata()
            .map_err(|_| refused())?;
        let lock = fsguard::private_file(&record.binding.state.join(journal::LOCK), false)?
            .ok_or_else(refused)?;
        let meta = lock.metadata().map_err(|_| refused())?;
        if (cfg.dev(), cfg.ino(), meta.dev(), meta.ino())
            != (p.config_dev, p.config_ino, p.lock_dev, p.lock_ino)
        {
            return Err(refused());
        }
    }
    Ok(())
}
/// A new start is admitted after an observed generation only when its exact
/// cooperative Stop marker and clean process exit are both proven. Only an
/// exact Store-validated retired SWITCH may omit the marker; Start cannot.
fn clean_stopped(
    detail: &Details,
    current: &Unit,
    expected_revision: &str,
    retired_switch: Option<&RetiredSwitch>,
) -> Result<()> {
    if !detail.definition_verified {
        return Err(refused());
    }
    definition_shape(detail, current)?;
    let last = detail.managed.as_ref().ok_or_else(refused)?;
    if last.revision != expected_revision
        || get(current, "ActiveState")? != "inactive"
        || get(current, "MainPID")? != "0"
        || get(current, "Result")? != "success"
        || get(current, "ExecMainStatus")? != "0"
        || !original_exited(&last.process)?
    {
        return Err(refused());
    }
    let invocation = get(current, "InvocationID")?;
    if !invocation.is_empty() && invocation != last.process.invocation {
        return Err(refused());
    }
    if !matches!(
        fsguard::probe_lock(&last.state.join(journal::LOCK)),
        fsguard::LockState::Free | fsguard::LockState::Absent
    ) {
        return Err(refused());
    }
    let read = |name| -> Result<Value> {
        let b = workspace_cli::read_private(&last.state, name)?.ok_or_else(refused)?;
        serde_json::from_slice(&b).map_err(|_| refused())
    };
    let control = read("workspace-control.json")?;
    if control["version"] != 1
        || control["generation"] != last.generation
        || control["pid"].as_i64() != Some(last.process.pid as i64)
    {
        return Err(refused());
    }
    if let Some(retired) = retired_switch {
        if retired.revision != expected_revision
            || retired.state != last.state
            || workspace_cli::read_config(&detail.config)?.1 != expected_revision
        {
            return Err(refused());
        }
    } else {
        let stop = read("workspace-stop.json")?;
        if stop["version"] != 1 || stop["generation"] != last.generation {
            return Err(refused());
        }
    }
    Ok(())
}
pub(super) async fn switch_stopped(
    store: &Store,
    record: &mut Record,
    retired: Option<&RetiredSwitch>,
) -> Result<()> {
    let detail = details(record)?;
    let current = unit().await?;
    let revision = record
        .activated_revision
        .as_deref()
        .ok_or_else(refused)?
        .to_owned();
    clean_stopped(&detail, &current, &revision, retired)?;
    let updated = refresh_start_definition(store, record, &current, retired).await?;
    clean_stopped(&updated, &unit().await?, &revision, retired)
}

pub(super) struct StartPlan {
    detail: Details,
    binding: Binding,
    state: PathBuf,
    previous_generation: Option<String>,
    already_running: bool,
}

/// An explicit, revision-bound recovery is separate from ordinary Start. An
/// unknown reservation is never retried merely because its journal is free.
pub(super) async fn recover_failed_start(
    store: &Store,
    record: &mut Record,
    expected: &str,
) -> Result<()> {
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || Store::revision(store.read(RECORD).map_err(|_| refused())?.as_deref()) != expected
        || !record.start_reserved
        || record.phase != "stopped"
        || record.transfer.is_some()
    {
        return Err(refused());
    }
    let detail = details(record)?;
    let current = unit().await?;
    if !detail.definition_verified
        || detail.managed.is_some()
        || get(&current, "ActiveState")? != "inactive"
        || get(&current, "SubState")? != "dead"
        || get(&current, "MainPID")? != "0"
        || !matches!(get(&current, "Result")?, "success" | "exit-code")
        || get(&current, "ExecMainStatus")? != "1"
    {
        return Err(refused());
    }
    definition_shape(&detail, &current)?;
    let (cfg, revision) = workspace_cli::read_config(&detail.config)?;
    if record.activated_revision.as_deref() != Some(&revision)
        || record.managed_node_id.is_none()
        || record.binding.config != detail.config
    {
        return Err(refused());
    }
    let state = Path::new(cfg["state_dir"].as_str().ok_or_else(refused)?);
    fsguard::check_private_directory(state).map_err(|_| refused())?;
    let control =
        workspace_cli::read_private(state, "workspace-control.json")?.ok_or_else(refused)?;
    let control: Value = serde_json::from_slice(&control).map_err(|_| refused())?;
    let generation = control["generation"].as_str().ok_or_else(refused)?;
    let pid = control["pid"]
        .as_u64()
        .filter(|p| (2..=i32::MAX as u64).contains(p))
        .ok_or_else(refused)?;
    if control.as_object().map(|o| o.len()) != Some(3)
        || control["version"] != 1
        || generation.len() != 32
        || !generation.bytes().all(|b| b.is_ascii_hexdigit())
        || Path::new(&format!("/proc/{pid}"))
            .try_exists()
            .map_err(|_| refused())?
        || !matches!(
            fsguard::probe_lock(&state.join(journal::LOCK)),
            fsguard::LockState::Free | fsguard::LockState::Absent
        )
    {
        return Err(refused());
    }
    // Keep an immutable proof before consuming this exact failed reservation.
    // A lost response cannot reuse its old revision or repeat this operation.
    let name = format!("failed-start-recovery-{expected}.json");
    let proof = serde_json::to_vec(&json!({"version":1,"previous_record_revision":expected,
        "failed_invocation":get(&current,"InvocationID")?,"control":control,
        "config_revision":revision,"reason":"explicit-unobserved-failed-start"}))
    .map_err(|_| refused())?;
    store
        .replace(&name, &Store::revision(None), &proof)
        .map_err(|_| refused())?;
    record.start_reserved = false;
    save(store, record)
}

async fn refresh_start_definition(
    store: &Store,
    record: &mut Record,
    current: &Unit,
    retired_switch: Option<&RetiredSwitch>,
) -> Result<Details> {
    let mut detail = details(record)?;
    if !detail.definition_verified {
        definition_matches(&detail, current)?;
        detail.definition_verified = true;
        remember(store, record, &detail)?;
    }
    definition_shape(&detail, current)?;
    let desired = image(&current_binary()?)?;
    let entry = current_entry(&desired)?;
    if desired == detail.desired && entry == detail.entry {
        return Ok(detail);
    }
    // Updating a running connector is cooperative Stop's responsibility. Start
    // never terminates it or adopts a replacement image by a mutable pathname.
    if get(current, "ActiveState")? != "inactive" || get(current, "MainPID")? != "0" {
        return Err(refused());
    }
    if record.start_reserved {
        clean_stopped(
            &detail,
            current,
            record.activated_revision.as_deref().ok_or_else(refused)?,
            retired_switch,
        )?;
    }
    let previous = detail.clone();
    detail.entry = entry;
    detail.desired = desired;
    detail.drop_bytes = format!(
        "{MARKER}[Service]\nExecStart=\nExecStart={} --config {}\n",
        detail.entry.display(),
        detail.config.display()
    );
    detail.definition_verified = false;
    remember(store, record, &detail)?;
    drop_write(
        &detail.drop_path,
        Some(&previous.drop_bytes),
        Some(&detail.drop_bytes),
    )?;
    // Unknown reload delivery is reconciled only by the exact effective shape
    // on a later explicit Start; never resend a start or arbitrary file write.
    manager(&["daemon-reload"]).await?;
    if definition_matches(&detail, &unit().await?).is_err() {
        drop_write(
            &detail.drop_path,
            Some(&detail.drop_bytes),
            Some(&previous.drop_bytes),
        )?;
        manager(&["daemon-reload"]).await?;
        remember(store, record, &previous)?;
        return Err(refused());
    }
    detail.definition_verified = true;
    remember(store, record, &detail)?;
    Ok(detail)
}
pub(super) async fn reserve_start(store: &Store, record: &mut Record) -> Result<StartPlan> {
    let current = unit().await?;
    let detail = refresh_start_definition(store, record, &current, None).await?;
    let current = unit().await?;
    after_definition(&detail, &current)?;
    let (cfg, revision) = workspace_cli::read_config(&record.binding.config)?;
    if record.activated_revision.as_deref() != Some(&revision)
        || cfg.get("node_token").is_some()
        || cfg["encrypted_workspace"]["version"] != 1
    {
        return Err(refused());
    }
    let state = PathBuf::from(cfg["state_dir"].as_str().ok_or_else(refused)?);
    let already_running = get(&current, "ActiveState")? == "active";
    if !already_running {
        if get(&current, "ActiveState")? != "inactive" || get(&current, "MainPID")? != "0" {
            return Err(refused());
        }
        if record.start_reserved {
            clean_stopped(&detail, &current, &revision, None)?;
        }
    }
    fsguard::create_directory(&state).map_err(|_| refused())?;
    fsguard::check_private_directory(&state).map_err(|_| refused())?;
    let previous_generation = workspace_cli::read_private(&state, "workspace-control.json")?
        .map(|b| serde_json::from_slice::<Value>(&b).map_err(|_| refused()))
        .transpose()?
        .and_then(|v| v["generation"].as_str().map(str::to_owned));
    let binding = Binding {
        revision,
        node_id: record.managed_node_id.clone().ok_or_else(refused)?,
        config: record.binding.config.clone(),
        state: state.clone(),
        relay_binding: String::new(),
    };
    // Consume the prior completed generation's clean-stop proof before the
    // Store guard is released. Concurrent/lost Start cannot reuse it.
    let mut detail = detail;
    if !already_running {
        detail.managed = None;
    }
    record.start_reserved = true;
    remember(store, record, &detail)?;
    Ok(StartPlan {
        detail,
        binding,
        state,
        previous_generation,
        already_running,
    })
}
pub(super) async fn start(plan: StartPlan) -> Result<(Value, Managed)> {
    if !plan.already_running {
        manager(&["start", UNIT]).await?;
    }
    loop {
        let current = unit().await?;
        after_definition(&plan.detail, &current)?;
        if workspace_cli::read_config(&plan.binding.config)?.1 != plan.binding.revision {
            return Err(refused());
        }
        if get(&current, "ActiveState")? == "active" {
            // The main process may not have acquired the journal yet. Only a
            // fully verified process+generation can complete this reservation.
            if let Ok(process) = process(&current, &plan.binding) {
                if process.image != plan.detail.desired {
                    return Err(refused());
                }
                if let Ok(status) = crate::workspace_lifecycle::status(&plan.state) {
                    if status["running"] == true
                        && (plan.already_running
                            || status["generation"].as_str() != plan.previous_generation.as_deref())
                    {
                        let generation = status["generation"]
                            .as_str()
                            .ok_or_else(refused)?
                            .to_owned();
                        let control =
                            workspace_cli::read_private(&plan.state, "workspace-control.json")?
                                .ok_or_else(refused)?;
                        let control: Value =
                            serde_json::from_slice(&control).map_err(|_| refused())?;
                        if control["pid"].as_i64() != Some(process.pid as i64)
                            || control["generation"] != generation
                        {
                            return Err(refused());
                        }
                        return Ok((
                            status,
                            Managed {
                                process,
                                generation,
                                state: plan.state,
                                revision: plan.binding.revision,
                            },
                        ));
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
pub(super) fn observed(store: &Store, record: &mut Record, observed: Managed) -> Result<()> {
    let mut detail = details(record)?;
    detail.managed = Some(observed);
    remember(store, record, &detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn safe() -> Unit {
        let mut u = Unit::new();
        for k in PROPERTIES.split(',') {
            u.insert(k.into(), String::new());
        }
        for(k,v)in[("LoadState","loaded"),("Type","simple"),("KillMode","process"),("Restart","on-failure"),("RemainAfterExit","no"),("ExitType","main"),("FailureAction","none"),("SuccessAction","none"),("WatchdogUSec","0"),("RuntimeMaxUSec","infinity"),("StopWhenUnneeded","no"),("ExecStart","{ path=/usr/bin/zerus-mobile-connector ; argv[]=/usr/bin/zerus-mobile-connector --config /private/connector.json ; ignore_errors=no ; }")]{u.insert(k.into(),v.into());}
        u
    }
    #[test]
    fn stop_cgroup_paths_refuse_ambiguous_or_escaping_hierarchies() {
        assert_eq!(
            group_path(b"0::/user.slice/app.slice/zerus-mobile-connector.service\n").unwrap(),
            "/user.slice/app.slice/zerus-mobile-connector.service"
        );
        assert_eq!(group_path(b"0::/\n").unwrap(), "/");
        for group in [
            "",
            "1:name=systemd:/unit",
            "0::relative",
            "0::/../unit",
            "0::/a//unit",
            "0::/unit\n0::/other",
        ] {
            assert!(group_path(group.as_bytes()).is_err(), "{group}");
        }
    }
    #[test]
    fn ordinary_user_service_needs_no_base_installer_marker() {
        let u = safe();
        policy(&u).unwrap();
        assert_eq!(arguments(&u).unwrap()[2], "/private/connector.json");
    }
    #[test]
    fn default_user_application_slice_is_only_an_exact_parent_requirement() {
        let mut u = safe();
        u.insert("Slice".into(), "app.slice".into());
        u.insert("Requires".into(), "basic.target app.slice".into());
        policy(&u).unwrap();
        for (key, value) in [
            ("Slice", "custom.slice"),
            ("Slice", ""),
            ("Requires", "basic.target custom.slice"),
            ("Requires", "basic.target app.slice native.service"),
            ("Wants", "app.slice"),
            ("BindsTo", "app.slice"),
            ("Upholds", "app.slice"),
        ] {
            let mut bad = u.clone();
            bad.insert(key.into(), value.into());
            assert!(policy(&bad).is_err(), "{key}={value}");
        }
    }
    #[test]
    fn process_kill_mode_cannot_override_dependent_tmux_or_exit_hooks() {
        for (k, v) in [
            ("ConsistsOf", "tmux-spawn-synthetic.scope"),
            ("BoundBy", "native.scope"),
            ("PropagatesStopTo", "native.scope"),
            ("ExecStopPost", "/usr/bin/kill"),
            ("OnSuccess", "native.service"),
            ("Restart", "always"),
            ("KillMode", "control-group"),
        ] {
            let mut u = safe();
            u.insert(k.into(), v.into());
            assert!(policy(&u).is_err(), "{k}");
        }
    }
    #[test]
    fn unknown_or_interpreter_command_is_refused() {
        for cmd in ["{ argv[]=/usr/bin/python --config /private/connector.json ; }","{ argv[]=/usr/bin/zerus-mobile-connector --config /private/connector.json ; } { argv[]=second ; }","{ argv[]=/usr/bin/zerus-mobile-connector --config /private/connector.json;evil ; }"]{let mut u=safe();u.insert("ExecStart".into(),cmd.into());assert!(arguments(&u).is_err())}
    }
    #[test]
    fn installer_path_is_allowed_but_external_loader_and_remapping_are_refused() {
        let mut u = safe();
        u.insert("Environment".into(), "PATH=/private/bin:/usr/bin".into());
        policy(&u).unwrap();
        for (key, value) in [
            ("Environment", "LD_PRELOAD=/private/injected.so"),
            ("Environment", "PATH=/usr/bin LD_AUDIT=/private/injected.so"),
            ("EnvironmentFiles", "/private/service.env"),
            ("RootDirectory", "/private/root"),
            ("BindPaths", "/private/other:/private/bin"),
        ] {
            let mut bad = u.clone();
            bad.insert(key.into(), value.into());
            assert!(policy(&bad).is_err(), "{key}");
        }
    }
    #[test]
    fn only_owned_execstart_change_is_accepted_by_readback() {
        let old = safe();
        let mut new = old.clone();
        new.insert("ExecStart".into(), "new reviewed executable".into());
        same_definition(&old, &new).unwrap();
        new.insert("KillMode".into(), "mixed".into());
        assert!(same_definition(&old, &new).is_err());
    }
    #[test]
    fn requires_readback_ignores_order_but_keeps_exact_membership() {
        let mut before = safe();
        before.insert("Slice".into(), "app.slice".into());
        before.insert("Requires".into(), "basic.target app.slice".into());
        let mut after = before.clone();
        after.insert("Requires".into(), "app.slice basic.target".into());
        same_definition(&before, &after).unwrap();
        for value in [
            "basic.target",
            "app.slice",
            "app.slice basic.target sysinit.target",
        ] {
            after.insert("Requires".into(), value.into());
            assert!(same_definition(&before, &after).is_err(), "{value}");
        }
        after = before.clone();
        after.insert("Slice".into(), "custom.slice".into());
        assert!(same_definition(&before, &after).is_err());
    }
}
