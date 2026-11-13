//! Cold-start exclusion, never authority to stop a native host.
//! The start lock in `ensure_host` serializes readers and publishers.
use super::*;
use std::process::Child;

/// The recorded hosts and the ancestor check (zerus-ppd26.15).
pub(super) mod recorded;

const FILE: &str = "host.identity.json";
const UNVERIFIABLE: &str = "The previous DeepSeek host could not be verified; it has been left untouched. Check its private host.log before recovering";
const STILL_RUNNING: &str = "The previous DeepSeek host is still running but its adapter is unavailable; it has been left running. Reconnect through its native UI instead of starting another host";
const UNCONFIRMED: &str = "The previous DeepSeek launch is in progress or unconfirmed; its processes have been left untouched. Retry after its launcher finishes or reconnect through the existing native UI. If a possible start cannot be verified, save any running work before restarting the computer and retrying";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Stamp(u64, u64, i64, i64);

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Running {
    version: u8,
    pid: u32,
    birth: String,
    pid_file: Stamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot: Option<String>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Launcher {
    pid: u32,
    birth: String,
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StartPhase {
    Prepared,
    Spawning,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Starting {
    version: u8,
    attempt: String,
    boot: String,
    launcher: Launcher,
    phase: StartPhase,
    pid_file: Option<(u32, Stamp)>,
    // Flat proof only: interrupted attempts cannot grow a recursive journal.
    previous: Option<Running>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Starting(Starting),
    Running(Running),
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

// The same bounded native boot sources used by worktree process discovery.
#[cfg(target_os = "linux")]
fn boot() -> Result<String> {
    let mut value = String::new();
    fs::File::open("/proc/sys/kernel/random/boot_id")
        .map_err(|_| UNVERIFIABLE)?
        .take(129)
        .read_to_string(&mut value)
        .map_err(|_| UNVERIFIABLE)?;
    if value.len() > 128 {
        return Err(UNVERIFIABLE.into());
    }
    let id = uuid::Uuid::parse_str(value.trim()).map_err(|_| UNVERIFIABLE)?;
    if id.is_nil() {
        return Err(UNVERIFIABLE.into());
    }
    Ok(id.to_string())
}
#[cfg(target_os = "macos")]
fn boot() -> Result<String> {
    let mut bytes = [0u8; 129];
    let mut size = bytes.len();
    if unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || !(1..=128).contains(&size)
    {
        return Err(UNVERIFIABLE.into());
    }
    let value = std::str::from_utf8(&bytes[..size]).map_err(|_| UNVERIFIABLE)?;
    let id = uuid::Uuid::parse_str(value.trim_end_matches('\0')).map_err(|_| UNVERIFIABLE)?;
    if id.is_nil() {
        return Err(UNVERIFIABLE.into());
    }
    Ok(id.to_string())
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot() -> Result<String> {
    Err(UNVERIFIABLE.into())
}

/// A fixed, small, private regular file. Never follow a link or wait on a FIFO.
fn read(path: &Path, cap: u64) -> Result<Option<(Vec<u8>, Stamp)>> {
    let file = match crate::platform::no_follow(OpenOptions::new().read(true)).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(UNVERIFIABLE.into()),
    };
    if !crate::platform::verify_private_file(&file).unwrap_or(false)
        || file.metadata().map_err(|_| UNVERIFIABLE)?.len() > cap
    {
        return Err(UNVERIFIABLE.into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNVERIFIABLE)?;
    if bytes.len() as u64 > cap {
        return Err(UNVERIFIABLE.into());
    }
    let id = crate::platform::FileIdentity::of(&file).map_err(|_| UNVERIFIABLE)?;
    Ok(Some((
        bytes,
        Stamp(id.device, id.inode, id.modified, id.modified_nanos),
    )))
}

fn valid_pid(pid: u32) -> bool {
    pid > 0 && pid <= i32::MAX as u32
}

fn legacy(dir: &Path) -> Result<Option<(u32, Stamp)>> {
    let Some((bytes, stamp)) = read(&dir.join("host.pid"), 32)? else {
        return Ok(None);
    };
    let pid = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|pid| valid_pid(*pid))
        .ok_or(UNVERIFIABLE)?;
    Ok(Some((pid, stamp)))
}

fn legacy_gone(pid: u32) -> Result<()> {
    match crate::platform::presence(pid) {
        crate::platform::Presence::Gone => Ok(()),
        // PID-only evidence cannot establish that this is still the original.
        crate::platform::Presence::Present | crate::platform::Presence::Unknown => {
            Err(UNVERIFIABLE.into())
        }
    }
}

// process_state treats any birth mismatch as an exited original. Reject an
// invalid saved birth first, so corruption cannot masquerade as PID reuse.
fn valid_birth(birth: &str) -> bool {
    let decimal = |text: &str| {
        text.parse::<u64>()
            .ok()
            .filter(|value| value.to_string() == text)
    };
    #[cfg(target_os = "linux")]
    {
        decimal(birth).is_some_and(|value| value > 0)
    }
    #[cfg(target_os = "macos")]
    {
        birth.split_once(':').is_some_and(|(sec, usec)| {
            decimal(sec).is_some_and(|v| v > 0) && decimal(usec).is_some_and(|v| v < 1_000_000)
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (birth, decimal);
        false
    }
}

fn valid_running(value: &Running) -> bool {
    valid_pid(value.pid)
        && valid_birth(&value.birth)
        && match (value.version, value.boot.as_deref()) {
            (1, None) => true,
            (2, Some(boot)) => canonical_uuid(boot),
            _ => false,
        }
}

fn parse_record(bytes: &[u8]) -> Result<Record> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| UNVERIFIABLE)?;
    // Null is an observed absence; a missing snapshot/backup is not evidence.
    if value["state"] == "starting"
        && !value
            .as_object()
            .is_some_and(|v| v.contains_key("pid_file") && v.contains_key("previous"))
    {
        return Err(UNVERIFIABLE.into());
    }
    if value["state"] == "running"
        && value["version"] == 1
        && value.as_object().is_some_and(|v| v.contains_key("boot"))
    {
        return Err(UNVERIFIABLE.into());
    }
    serde_json::from_value(value).map_err(|_| UNVERIFIABLE.into())
}

struct Assessment {
    boot: String,
    original: Option<Vec<u8>>,
    previous: Option<Running>,
    pid_file: Option<(u32, Stamp)>,
}

fn assess(dir: &Path) -> Result<Assessment> {
    let current_boot = boot()?;
    let pid_file = legacy(dir)?;
    let original = read(&dir.join(FILE), 1024)?.map(|(bytes, _)| bytes);
    let previous = if let Some(bytes) = &original {
        match parse_record(bytes)? {
            Record::Running(mut saved) => {
                if !valid_running(&saved) {
                    return Err(UNVERIFIABLE.into());
                }
                let previous_boot = saved.boot.as_ref().is_some_and(|b| b != &current_boot);
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if !previous_boot {
                    match crate::platform::process_state(saved.pid, Some(&saved.birth)) {
                        Ok(crate::platform::ProcessState::Exited) => {}
                        Ok(_) => {
                            return Err(if saved.boot.is_some() {
                                STILL_RUNNING
                            } else {
                                UNVERIFIABLE
                            }
                            .into());
                        }
                        Err(_) => return Err(UNVERIFIABLE.into()),
                    }
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                return Err(UNVERIFIABLE.into());
                // Never use an old birth/boot to hide a newer legacy publication.
                let (pid, stamp) = pid_file.as_ref().ok_or(UNVERIFIABLE)?;
                if *pid != saved.pid || stamp != &saved.pid_file {
                    legacy_gone(*pid)?;
                }
                // The old original is proven gone on this boot. Give a legacy
                // backup that same upper bound without inventing a live host.
                saved.version = 2;
                saved.boot.get_or_insert_with(|| current_boot.clone());
                Some(saved)
            }
            Record::Starting(saved) => {
                if saved.version != 2
                    || !canonical_uuid(&saved.attempt)
                    || !canonical_uuid(&saved.boot)
                    || !valid_pid(saved.launcher.pid)
                    || !valid_birth(&saved.launcher.birth)
                    || saved
                        .previous
                        .as_ref()
                        .is_some_and(|p| !valid_running(p) || p.version != 2)
                {
                    return Err(UNVERIFIABLE.into());
                }
                let previous_boot = saved.boot != current_boot;
                if !previous_boot {
                    if saved.phase != StartPhase::Prepared {
                        // Launcher death and a missing PID file do not prove
                        // that a detached host was never spawned.
                        return Err(UNCONFIRMED.into());
                    }
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if !matches!(
                        crate::platform::process_state(
                            saved.launcher.pid,
                            Some(&saved.launcher.birth)
                        ),
                        Ok(crate::platform::ProcessState::Exited)
                    ) {
                        return Err(UNCONFIRMED.into());
                    }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    return Err(UNVERIFIABLE.into());
                }
                if pid_file != saved.pid_file {
                    // On a later boot the old attempt is gone, but another
                    // launcher's current PID-only evidence still protects it.
                    if !previous_boot {
                        return Err(UNVERIFIABLE.into());
                    }
                    let (pid, _) = pid_file.as_ref().ok_or(UNVERIFIABLE)?;
                    legacy_gone(*pid)?;
                }
                saved.previous
            }
        }
    } else {
        if let Some((pid, _)) = &pid_file {
            legacy_gone(*pid)?;
        }
        None
    };
    Ok(Assessment {
        boot: current_boot,
        original,
        previous,
        pid_file,
    })
}

/// Whether the recorded host owner is the same process, still running. A
/// refused connect to its socket is then a saturated host, never a gone one
/// (zerus-xt6re). An unreadable record, a recorded owner that exited or
/// stopped, and a record from another boot are not a live owner.
pub(super) fn owner_alive(dir: &Path) -> bool {
    let Ok(Some((bytes, _))) = read(&dir.join(FILE), 1024) else {
        return false;
    };
    let Ok(Record::Running(saved)) = parse_record(&bytes) else {
        return false;
    };
    if !valid_running(&saved) {
        return false;
    }
    if let Some(saved_boot) = saved.boot.as_deref() {
        if boot().ok().as_deref() != Some(saved_boot) {
            return false;
        }
    }
    matches!(
        crate::platform::process_state(saved.pid, Some(&saved.birth)),
        Ok(crate::platform::ProcessState::Running)
    )
}

/// A recorded running owner for the refused-connect fixture (zerus-xt6re).
#[cfg(test)]
pub(in crate::state) fn record_running_for_test(dir: &Path, pid: u32) -> Result<()> {
    let identity = crate::platform::process_identity(pid)
        .filter(|process| valid_birth(&process.birth))
        .ok_or(UNVERIFIABLE)?;
    let record = Record::Running(Running {
        version: 1,
        pid,
        birth: identity.birth,
        pid_file: Stamp(0, 0, 0, 0),
        boot: None,
    });
    atomic(
        &dir.join(FILE),
        &serde_json::to_string(&record).map_err(|_| UNVERIFIABLE)?,
    )
}

pub(super) fn before_start(dir: &Path) -> Result<()> {
    assess(dir).map(|_| ())
}

fn publish(dir: &Path, starting: &Starting) -> Result<()> {
    let bytes =
        serde_json::to_string(&Record::Starting(starting.clone())).map_err(|_| UNVERIFIABLE)?;
    if bytes.len() > 1024 {
        return Err(UNVERIFIABLE.into());
    }
    atomic(&dir.join(FILE), &bytes)
}

fn check_starting(dir: &Path, starting: &Starting) -> Result<()> {
    let current = read(&dir.join(FILE), 1024)?.ok_or(UNVERIFIABLE)?.0;
    if !matches!(parse_record(&current)?, Record::Starting(ref owned) if owned == starting)
        || legacy(dir)? != starting.pid_file
    {
        return Err(UNVERIFIABLE.into());
    }
    Ok(())
}

// Only a positively unstarted local attempt may restore its previous proof.
// An error after a Child exists never comes here.
fn restore_unstarted(dir: &Path, starting: &Starting, original: &Option<Vec<u8>>) -> Result<()> {
    let current = read(&dir.join(FILE), 1024)?.map(|(bytes, _)| bytes);
    if &current == original {
        return Ok(());
    }
    let Some(bytes) = current else {
        return Err(UNVERIFIABLE.into());
    };
    let Record::Starting(mut owned) = parse_record(&bytes)? else {
        return Err(UNVERIFIABLE.into());
    };
    owned.phase = starting.phase;
    if &owned != starting {
        return Err(UNVERIFIABLE.into());
    }
    if let Some(bytes) = original {
        atomic(
            &dir.join(FILE),
            std::str::from_utf8(bytes).map_err(|_| UNVERIFIABLE)?,
        )
    } else {
        fs::remove_file(dir.join(FILE)).map_err(|_| UNVERIFIABLE)?;
        crate::platform::sync_dir(dir).map_err(|_| UNVERIFIABLE.into())
    }
}

/// Prepared is a durable no-spawn boundary. The second publication precedes
/// every spawn; once it succeeds only a definite spawn refusal can roll back.
/// Uncertain publication/identity after a Child exists stays unconfirmed.
pub(super) fn spawn(dir: &Path, command: &mut Command) -> Result<Child> {
    let previous = assess(dir)?;
    let identity = crate::platform::process_identity(std::process::id())
        .filter(|p| valid_birth(&p.birth))
        .ok_or(UNVERIFIABLE)?;
    let mut starting = Starting {
        version: 2,
        attempt: uuid::Uuid::new_v4().to_string(),
        boot: previous.boot,
        launcher: Launcher {
            pid: std::process::id(),
            birth: identity.birth,
        },
        phase: StartPhase::Prepared,
        pid_file: previous.pid_file,
        previous: previous.previous,
    };
    if let Err(error) = publish(dir, &starting) {
        restore_unstarted(dir, &starting, &previous.original)?;
        return Err(error);
    }
    #[cfg(test)]
    tests::after_marker(dir, starting.phase);
    if let Err(error) = check_starting(dir, &starting) {
        restore_unstarted(dir, &starting, &previous.original)?;
        return Err(error);
    }
    starting.phase = StartPhase::Spawning;
    if let Err(error) = publish(dir, &starting) {
        restore_unstarted(dir, &starting, &previous.original)?;
        return Err(error);
    }
    #[cfg(test)]
    tests::after_marker(dir, starting.phase);
    if let Err(error) = check_starting(dir, &starting) {
        restore_unstarted(dir, &starting, &previous.original)?;
        return Err(error);
    }
    let mut child = match crate::platform::spawn_detached(command) {
        Ok(child) => child,
        Err(error) => {
            restore_unstarted(dir, &starting, &previous.original)?;
            return Err(error.to_string());
        }
    };
    #[cfg(test)]
    tests::after_child(&mut child);
    let identity = crate::platform::process_identity(child.id()).filter(|p| valid_birth(&p.birth));
    let Some(identity) = identity else {
        // Even an exited wrapper is not proof about an unidentified detached
        // host. Keep the possible-start boundary; never signal or replay it.
        let _ = child.try_wait(); // Reap this original if it has already exited.
        return Err(UNCONFIRMED.into());
    };
    atomic(&dir.join("host.pid"), &child.id().to_string())?;
    let (pid, pid_file) = legacy(dir)?.ok_or(UNVERIFIABLE)?;
    if pid != child.id() {
        return Err(UNVERIFIABLE.into());
    }
    atomic(
        &dir.join(FILE),
        &serde_json::to_string(&Record::Running(Running {
            version: 2,
            pid: child.id(),
            birth: identity.birth,
            pid_file,
            boot: Some(starting.boot),
        }))
        .map_err(|_| UNVERIFIABLE)?,
    )?;
    Ok(child)
}

#[cfg(test)]
mod tests;
