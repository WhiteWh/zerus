//! Read-only current executor evidence for the service's fixed recovery kind.
//! No session discovery, provider repair, terminal access or receipt fallback.
use crate::platform;
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    time::Instant,
};
use zerus_client_core::operation_recovery::task_start::Executor;

const RECORD_BYTES: u64 = 1 << 20;

// The ordinary binding format permits future fields. Only fields used by this
// current-identity predicate must be unique; none may use last-key-wins JSON.
#[derive(serde::Deserialize)]
struct RecordFields {
    #[serde(rename = "version")]
    _version: serde::de::IgnoredAny,
    #[serde(rename = "run_identity_version")]
    _run_version: serde::de::IgnoredAny,
    #[serde(rename = "name")]
    _name: serde::de::IgnoredAny,
    #[serde(rename = "agent")]
    _agent: serde::de::IgnoredAny,
    #[serde(rename = "run_id")]
    _run: serde::de::IgnoredAny,
    #[serde(rename = "conversation_id")]
    _conversation: serde::de::IgnoredAny,
    #[serde(rename = "pid")]
    _pid: serde::de::IgnoredAny,
    #[serde(rename = "process_start")]
    _start: serde::de::IgnoredAny,
    #[serde(rename = "supervisor")]
    _supervisor: SupervisorFields,
    #[serde(rename = "launch_id")]
    _launch: Option<String>,
    #[serde(rename = "exited_at")]
    _exited: Option<serde::de::IgnoredAny>,
    #[serde(rename = "pausing")]
    _pausing: Option<serde::de::IgnoredAny>,
    #[serde(rename = "paused")]
    _paused: Option<bool>,
    #[serde(rename = "resume_pending")]
    _resuming: Option<bool>,
    #[serde(rename = "archive_id")]
    _archive: Option<String>,
    #[serde(rename = "expected_id")]
    _expected: Option<String>,
    #[serde(rename = "restore_archive_id")]
    _restore: Option<String>,
    #[serde(rename = "clear_context_request")]
    _clear: Option<serde::de::IgnoredAny>,
}
#[derive(serde::Deserialize)]
struct SupervisorFields {
    #[serde(rename = "pid")]
    _pid: serde::de::IgnoredAny,
    #[serde(rename = "start")]
    _start: serde::de::IgnoredAny,
}

fn open(parent: &File, leaf: &str, directory: bool) -> Result<File> {
    let name = CString::new(leaf)?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // All names are fixed directory components or a SHA-256 filename. The
    // retained directory handle, rather than a caller path, selects the root.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    ensure!(
        if directory {
            platform::verify_private_directory(&file)?
        } else {
            platform::verify_private_file(&file)?
        },
        "executor binding is not private"
    );
    Ok(file)
}

struct Process {
    pid: u32,
    identity: platform::ProcessIdentity,
    image: String,
    file: platform::FileIdentity,
    _pin: platform::PinnedProcess,
}
impl Process {
    fn capture(pid: u32, start: &str) -> Result<Self> {
        ensure!(pid > 0 && !start.is_empty(), "executor process missing");
        let identity = platform::process_identity(pid).context("executor process unavailable")?;
        ensure!(
            platform::process_start_time(pid).as_deref() == Some(start),
            "executor birth changed"
        );
        let pin = platform::PinnedProcess::pin(pid, &identity.birth).map_err(anyhow::Error::msg)?;
        let image = platform::describe(pid)
            .context("executor image unavailable")?
            .executable;
        let file = platform::file_identity(Path::new(&image)).context("executor image missing")?;
        let value = Self {
            pid,
            identity,
            image,
            file,
            _pin: pin,
        };
        value.current()?;
        Ok(value)
    }
    fn current(&self) -> Result<()> {
        ensure!(
            self._pin.alive()
                && platform::process_identity(self.pid).as_ref() == Some(&self.identity)
                && platform::describe(self.pid).is_some_and(|p| p.executable == self.image)
                && platform::file_identity(Path::new(&self.image)) == Some(self.file),
            "original executor process changed"
        );
        Ok(())
    }
}

#[derive(PartialEq)]
struct Binding {
    name: String,
    run: String,
    conversation: String,
    session: String,
    agent: String,
    pid: u32,
    start: String,
    supervisor: u32,
    supervisor_start: String,
}
fn binding(record: &Value) -> Result<Binding> {
    let text = |key: &str| {
        record[key]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512)
            .map(str::to_owned)
            .context("executor identity missing")
    };
    ensure!(
        record["version"] == 1
            && record["run_identity_version"] == 1
            && record.get("exited_at").is_none()
            && record.get("pausing").is_none()
            && record["paused"] != true
            && record["resume_pending"] != true
            && record["expected_id"].as_str().is_none_or(str::is_empty)
            && record["restore_archive_id"]
                .as_str()
                .is_none_or(str::is_empty)
            && !super::clear_context::cleanup_pending(record)
            && record["archive_id"].as_str().is_none_or(str::is_empty),
        "executor is not current"
    );
    let agent = text("agent")?;
    ensure!(
        super::AGENTS.contains(&agent.as_str()),
        "executor agent unsupported"
    );
    let run = text("run_id")?;
    let session = record["launch_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|id| format!("launch_id:{id}"))
        .unwrap_or_else(|| format!("run_id:{run}"));
    let pid = |v: &Value| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .context("executor process missing")
    };
    Ok(Binding {
        name: text("name")?,
        run,
        conversation: text("conversation_id")?,
        session,
        agent,
        pid: pid(&record["pid"])?,
        start: text("process_start")?,
        supervisor: pid(&record["supervisor"]["pid"])?,
        supervisor_start: record["supervisor"]["start"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .context("executor supervisor missing")?
            .into(),
    })
}

/// Captured by the native authentication worker, never deserialized. The same
/// process incarnations and selected record are checked again at admission
/// and before original input release. None of its data authorizes an effect.
pub(crate) struct Original {
    root: PathBuf,
    directory: File,
    filename: String,
    routed: bool,
    /// The session ID the record was read by, for an executor in the ID
    /// layout; it is read the same way on every later check.
    by_id: Option<String>,
    binding: Binding,
    process: Process,
    supervisor: Process,
}
impl Original {
    pub(crate) fn capture(
        root: &Path,
        executor: &Executor,
        input: &[u8],
        until: Instant,
    ) -> Result<Self> {
        Self::at(root.to_path_buf(), executor, input, until)
    }
    fn at(root: PathBuf, executor: &Executor, input: &[u8], until: Instant) -> Result<Self> {
        ensure!(Instant::now() < until, "executor proof expired");
        let plan: Value = serde_json::from_slice(input)?;
        ensure!(
            plan["selection"]["host"] == "" && plan["session"]["machine"] == executor.machine,
            "executor route is not local"
        );
        let name = plan["selection"]["name"]
            .as_str()
            .context("executor name missing")?;
        ensure!(plan["session"]["session"] == name, "executor route changed");
        let mut options = fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt;
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let directory = options.open(&root)?;
        ensure!(
            platform::verify_private_directory(&directory)?,
            "executor root unavailable"
        );
        let filename = format!("{:x}.json", Sha256::digest(name.as_bytes()));
        // An executor in the ID layout (zerus-ucck P3a) is read by its ID.
        let (record, routed, by_id) = match read_in_id_layout(&directory, name, executor)? {
            Some((record, id)) => {
                // The label's own record counts with the ID layout's (#1437
                // review): it must not bind this executor as well.
                if let Some((legacy, _)) = optional_read(&directory, &filename)? {
                    ensure!(
                        !binds(&legacy, name, executor),
                        "executor binds more than one session"
                    );
                }
                (record, false, Some(id))
            }
            None => {
                let (record, routed) = read(&directory, &filename)?;
                (record, routed, None)
            }
        };
        let binding = binding(&record)?;
        ensure!(
            binding.name == name
                && binding.run == executor.run_id
                && binding.conversation == executor.conversation_id
                && binding.session == executor.session_id,
            "executor binding changed"
        );
        let process = Process::capture(binding.pid, &binding.start)?;
        let supervisor = Process::capture(binding.supervisor, &binding.supervisor_start)?;
        ensure!(
            process.pid != supervisor.pid
                && process.identity.parent == supervisor.pid
                && platform::file_identity(&std::env::current_exe()?) == Some(supervisor.file),
            "executor supervisor changed"
        );
        let value = Self {
            root,
            directory,
            filename,
            routed,
            by_id,
            binding,
            process,
            supervisor,
        };
        value.current(until)?;
        Ok(value)
    }
    pub(crate) fn current(&self, until: Instant) -> Result<()> {
        ensure!(Instant::now() < until, "executor proof expired");
        let now = fs::symlink_metadata(&self.root)?;
        let original = self.directory.metadata()?;
        ensure!(
            platform::verify_private_directory(&self.directory)?
                && now.is_dir()
                && now.dev() == original.dev()
                && now.ino() == original.ino(),
            "executor root replaced"
        );
        let (record, routed) = reread(&self.directory, &self.filename, self.by_id.as_deref())?;
        ensure!(
            routed == self.routed && binding(&record)? == self.binding,
            "executor binding changed"
        );
        self.process.current()?;
        self.supervisor.current()?;
        let environment = environment(self.process.pid)?;
        let mut runs = environment
            .split(|b| *b == 0)
            .filter_map(|entry| entry.strip_prefix(b"HGS_RUN_ID="));
        ensure!(
            runs.next() == Some(self.binding.run.as_bytes()) && runs.next().is_none(),
            "executor native run changed"
        );
        self.process.current()?;
        ensure!(Instant::now() < until, "executor proof expired");
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub(super) fn environment(pid: u32) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/environ"))?
        .take((1 << 20) + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1 << 20, "executor environment unavailable");
    Ok(bytes)
}
#[cfg(target_os = "macos")]
pub(super) fn environment(pid: u32) -> Result<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut bytes = vec![0u8; 1 << 20];
    let mut size = bytes.len();
    let code = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(
        code == 0 && (4..=bytes.len()).contains(&size),
        "executor environment unavailable"
    );
    bytes.truncate(size);
    let argc = i32::from_ne_bytes(bytes[..4].try_into()?);
    ensure!((1..=4096).contains(&argc), "executor argv unavailable");
    let mut rest = &bytes[4..];
    rest = &rest[rest
        .iter()
        .position(|b| *b == 0)
        .context("executor executable unavailable")?
        + 1..];
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    for _ in 0..argc {
        rest = &rest[rest
            .iter()
            .position(|b| *b == 0)
            .context("executor argv unavailable")?
            + 1..];
    }
    Ok(rest.to_vec())
}
/// An optional fixed entry under `parent`: None only when it is verifiably
/// absent. Permission, links and every other error stay errors, so they can
/// never select a different saved binding.
fn optional(parent: &File, leaf: &str, directory: bool) -> Result<Option<File>> {
    match open(parent, leaf, directory) {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// At most [`RECORD_BYTES`] of `file`, or an error.
fn bounded(file: &mut File) -> Result<Vec<u8>> {
    bounded_to(file, RECORD_BYTES)
}

/// The label-index manifest's bound, as the CLI bounds it.
const MANIFEST_BYTES: u64 = 16 << 20;

/// At most `limit` bytes of `file`, or an error.
fn bounded_to(file: &mut File, limit: u64) -> Result<Vec<u8>> {
    ensure!(file.metadata()?.len() <= limit, "executor record too large");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "executor record too large");
    Ok(bytes)
}

/// The executor's record in the ID layout (zerus-ucck P3a), found through
/// its label's index entry `labels/<sha256(label)>.json`: the one listed
/// session whose `sessions/<id>.json` binds this executor's run, conversation
/// and session. None when the label has no entry or no listed session binds
/// it, so the label's own path is read instead; two that bind it are an error.
fn read_in_id_layout(
    directory: &File,
    name: &str,
    executor: &Executor,
) -> Result<Option<(Value, String)>> {
    // The label index names candidates only while its manifest proves them
    // (#1437 review). With no `sessions/` there is no ID layout at all.
    let manifest = match optional(directory, "labels", true)? {
        Some(labels) => optional(&labels, ".complete", false)?
            .map(|mut file| bounded_to(&mut file, MANIFEST_BYTES))
            .transpose()?
            .map(|bytes| (labels, bytes)),
        None => None,
    };
    let Some((labels, manifest)) = manifest else {
        ensure!(
            optional(directory, "sessions", true)?.is_none(),
            "executor label index cannot be proven"
        );
        return Ok(None);
    };
    // The CLI's own proof (`state::prove_label_index`): no entry means no
    // session in the ID layout; a present entry needs its file to hash to it
    // and list one or more canonical IDs, else the proof refuses.
    let key = format!("{:x}", Sha256::digest(name.as_bytes()));
    let ids = crate::state::prove_label_index(name, &manifest, || {
        optional(&labels, &format!("{key}.json"), false)
            .ok()
            .flatten()
            .and_then(|mut file| bounded(&mut file).ok())
    })
    .context("executor label index cannot be proven")?;
    let mut found = None;
    for id in ids {
        // A listed session whose file is gone is a stale entry.
        let Some(record) = read_by_id(directory, &id)? else {
            continue;
        };
        if binds(&record, name, executor) {
            ensure!(found.is_none(), "executor binds more than one session");
            found = Some((record, id));
        }
    }
    Ok(found)
}

/// Whether `record` binds `executor` under the label `name`.
fn binds(record: &Value, name: &str, executor: &Executor) -> bool {
    binding(record).is_ok_and(|binding| {
        binding.name == name
            && binding.run == executor.run_id
            && binding.conversation == executor.conversation_id
            && binding.session == executor.session_id
    })
}

/// The label's own record, as [`read`] reads it; None only when it verifiably
/// does not exist.
fn optional_read(directory: &File, filename: &str) -> Result<Option<(Value, bool)>> {
    match read(directory, filename) {
        Ok(found) => Ok(Some(found)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The record at `sessions/<id>.json`, when there is one: it must say it is
/// in the ID layout and carry that exact ID. None when it is absent.
fn read_by_id(directory: &File, id: &str) -> Result<Option<Value>> {
    if !crate::state::session_id::canonical(id) {
        return Ok(None);
    }
    let Some(sessions) = optional(directory, "sessions", true)? else {
        return Ok(None);
    };
    let Some(mut file) = optional(&sessions, &format!("{id}.json"), false)? else {
        return Ok(None);
    };
    let bytes = bounded(&mut file)?;
    let _: RecordFields = serde_json::from_slice(&bytes)?;
    let record: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        crate::state::is_id_layout_record(&record, id),
        "executor record does not match its ID"
    );
    Ok(Some(record))
}

/// The executor's record, read where it was first found: by its session ID
/// in the ID layout, where it must still be, or else at its label's path.
fn reread(directory: &File, filename: &str, by_id: Option<&str>) -> Result<(Value, bool)> {
    match by_id {
        Some(id) => Ok((
            read_by_id(directory, id)?.context("executor record left the ID layout")?,
            false,
        )),
        None => read(directory, filename),
    }
}

fn read(directory: &File, filename: &str) -> Result<(Value, bool)> {
    // Only verified absence selects the legacy namespace. Permission, links
    // and all other errors cannot silently select a different saved binding.
    let routed = optional(directory, "name_routes", true)?
        .map(|routes| optional(&routes, filename, false).map(|v| v.is_some()))
        .transpose()?
        .unwrap_or(false);
    let folder = if routed {
        Some(open(directory, "bindings", true)?)
    } else {
        None
    };
    let mut file = open(folder.as_ref().unwrap_or(directory), filename, false)?;
    let bytes = bounded(&mut file)?;
    let _: RecordFields = serde_json::from_slice(&bytes)?;
    Ok((serde_json::from_slice(&bytes)?, routed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        os::unix::fs::{symlink, PermissionsExt},
        process::{Child, Command, Stdio},
        time::Duration,
    };

    /// A stand-in executor that has finished exec. `spawn` returns before the
    /// kernel publishes the new image's environment, so /proc/PID/environ
    /// reads empty for a moment (1972 of 2000 reads right after spawn), and
    /// a loaded runner keeps it so past the fixture's setup (zerus-spum). Its
    /// echo proves it runs; the private test image also avoids Darwin's hidden
    /// platform-binary environment. It lives until killed or its stdin closes.
    fn executor(run: &str) -> Child {
        let mut child = crate::test_process::echo_command()
            .env("HGS_RUN_ID", run)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        crate::test_process::echo_ready(&mut child);
        child
    }

    struct Fixture {
        root: tempfile::TempDir,
        child: Child,
        record: Value,
        target: Executor,
        input: Vec<u8>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let run = uuid::Uuid::new_v4().to_string();
            let child = executor(&run);
            let conversation = uuid::Uuid::new_v4().to_string();
            let target = Executor {
                machine: uuid::Uuid::new_v4().to_string(),
                session_id: format!("run_id:{run}"),
                run_id: run.clone(),
                conversation_id: conversation.clone(),
            };
            let record = json!({"version":1,"run_identity_version":1,"name":"private-task-executor","agent":"codex","run_id":run,
                "conversation_id":conversation,"pid":child.id(),"process_start":platform::process_start_time(child.id()).unwrap(),
                "supervisor":{"pid":std::process::id(),"start":platform::process_start_time(std::process::id()).unwrap()}});
            let input = serde_json::to_vec(
                &json!({"selection":{"host":"","name":"private-task-executor"},
                "session":{"machine":target.machine,"session":"private-task-executor"}}),
            )
            .unwrap();
            let f = Self {
                root,
                child,
                record,
                target,
                input,
            };
            f.write();
            f
        }
        fn path(&self) -> PathBuf {
            self.root.path().join(format!(
                "{:x}.json",
                Sha256::digest(b"private-task-executor")
            ))
        }
        fn write(&self) {
            fs::write(self.path(), serde_json::to_vec(&self.record).unwrap()).unwrap();
            fs::set_permissions(self.path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn capture(&self) -> Result<Original> {
            Original::at(
                self.root.path().into(),
                &self.target,
                &self.input,
                Instant::now() + Duration::from_secs(2),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    fn until() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }

    #[test]
    fn the_stand_in_executor_publishes_its_run_before_any_capture() {
        // zerus-spum: read at once after a plain spawn, most are empty.
        for _ in 0..200 {
            let mut child = executor("stand-in-run");
            let bytes = environment(child.id()).unwrap();
            assert!(bytes.split(|b| *b == 0).any(|entry| entry == b"HGS_RUN_ID=stand-in-run"));
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    #[test]
    fn original_current_executor_accepts_unrelated_record_updates_and_refuses_identity_change() {
        let mut f = Fixture::new();
        let original = f.capture().unwrap();
        f.record["activity"] = json!("working");
        f.record["seq"] = json!(99);
        f.write();
        original.current(until()).unwrap();
        f.record["conversation_id"] = json!(uuid::Uuid::new_v4().to_string());
        f.write();
        assert!(original.current(until()).is_err());
    }
    #[test]
    fn original_process_exit_cannot_borrow_a_new_executor_or_historical_sent_receipt() {
        let mut f = Fixture::new();
        let original = f.capture().unwrap();
        f.child.kill().unwrap();
        f.child.wait().unwrap();
        let mut replacement = Command::new("/bin/sleep").arg("20").spawn().unwrap();
        f.record["pid"] = json!(replacement.id());
        f.record["process_start"] = json!(platform::process_start_time(replacement.id()).unwrap());
        f.record["last_receipt"] = json!({"status":"submitted"});
        f.write();
        assert!(original.current(until()).is_err());
        replacement.kill().unwrap();
        replacement.wait().unwrap();
    }
    #[test]
    fn missing_private_binding_and_changed_record_routes_are_not_discovery() {
        let f = Fixture::new();
        let original = f.capture().unwrap();
        fs::remove_file(f.path()).unwrap();
        assert!(f.capture().is_err());
        f.write();
        fs::create_dir(f.root.path().join("name_routes")).unwrap();
        fs::set_permissions(
            f.root.path().join("name_routes"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let route = f
            .root
            .path()
            .join("name_routes")
            .join(f.path().file_name().unwrap());
        fs::write(&route, b"{}").unwrap();
        fs::set_permissions(route, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(original.current(until()).is_err());
    }
    #[test]
    fn links_fifo_oversize_and_public_records_refuse_without_blocking_or_repair() {
        let f = Fixture::new();
        let real = f.root.path().join("real");
        fs::rename(f.path(), &real).unwrap();
        symlink(&real, f.path()).unwrap();
        assert!(f.capture().is_err());
        fs::remove_file(f.path()).unwrap();
        fs::hard_link(&real, f.path()).unwrap();
        assert!(f.capture().is_err());
        fs::remove_file(f.path()).unwrap();
        let name = CString::new(f.path().as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(f.capture().is_err());
        fs::remove_file(f.path()).unwrap();
        fs::write(f.path(), vec![b' '; RECORD_BYTES as usize + 1]).unwrap();
        fs::set_permissions(f.path(), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(f.capture().is_err());
        f.write();
        fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(f.capture().is_err());
    }
    #[test]
    fn paused_archive_foreign_route_and_wrong_supervisor_are_not_current() {
        for key in [
            "paused",
            "resume_pending",
            "pausing",
            "archive_id",
            "supervisor",
            "expected_id",
            "restore_archive_id",
            "restore_archive_cleanup",
        ] {
            let mut f = Fixture::new();
            f.record[key] = match key {
                "archive_id" | "restore_archive_id" => json!("archive"),
                "expected_id" => json!("unconfirmed conversation"),
                "supervisor" => json!({"pid":f.child.id(),"start":f.record["process_start"]}),
                _ => json!(true),
            };
            f.write();
            assert!(f.capture().is_err(), "{key}");
        }
        let mut f = Fixture::new();
        let mut input: Value = serde_json::from_slice(&f.input).unwrap();
        input["selection"]["host"] = json!("remote");
        f.input = serde_json::to_vec(&input).unwrap();
        assert!(f.capture().is_err());
    }
    #[test]
    fn original_expiry_is_not_renewed_by_a_successful_capture() {
        let f = Fixture::new();
        let original = f.capture().unwrap();
        assert!(original.current(Instant::now()).is_err());
    }
    #[test]
    fn a_saved_binding_and_plan_cannot_relabel_the_original_native_run() {
        let mut f = Fixture::new();
        let replacement = uuid::Uuid::new_v4().to_string();
        f.target.run_id = replacement.clone();
        f.target.session_id = format!("run_id:{replacement}");
        f.record["run_id"] = json!(replacement);
        f.write();
        assert!(f.capture().is_err());
    }
    fn malformed_lifecycle_fields() -> Vec<(&'static str, Value)> {
        let mut values = Vec::new();
        for field in ["paused", "resume_pending"] {
            for value in [json!("true"), json!(42), json!({}), json!([])] {
                values.push((field, value));
            }
        }
        for field in [
            "archive_id",
            "expected_id",
            "restore_archive_id",
            "launch_id",
        ] {
            for value in [json!(true), json!(42), json!({}), json!([])] {
                values.push((field, value));
            }
        }
        values
    }
    #[test]
    fn capture_refuses_malformed_optional_lifecycle_fields() {
        let mut f = Fixture::new();
        for (field, value) in malformed_lifecycle_fields() {
            f.record[field] = value.clone();
            f.write();
            assert!(f.capture().is_err(), "capture accepted {field}={value}");
            f.record.as_object_mut().unwrap().remove(field);
            f.write();
            // Each refusal must be this field's shape, with the original
            // executor, image and run marker still valid between cases.
            assert!(f.capture().is_ok(), "original executor lost after {field}");
        }
    }
    #[test]
    fn retained_current_refuses_malformed_optional_lifecycle_fields() {
        let mut f = Fixture::new();
        let original = f.capture().unwrap();
        for (field, value) in malformed_lifecycle_fields() {
            f.record[field] = value.clone();
            f.write();
            assert!(
                original.current(until()).is_err(),
                "retained executor accepted {field}={value}"
            );
            f.record.as_object_mut().unwrap().remove(field);
            f.write();
            original.current(until()).unwrap();
        }
    }
    #[test]
    fn canonical_optional_shapes_and_unrelated_future_fields_still_pass() {
        let mut f = Fixture::new();
        let original = f.capture().unwrap();
        for field in ["paused", "resume_pending"] {
            for value in [Value::Null, json!(false)] {
                f.record[field] = value;
                f.write();
                f.capture().unwrap();
                original.current(until()).unwrap();
            }
        }
        for field in [
            "archive_id",
            "expected_id",
            "restore_archive_id",
            "launch_id",
        ] {
            for value in [Value::Null, json!("")] {
                f.record[field] = value;
                f.write();
                f.capture().unwrap();
                original.current(until()).unwrap();
            }
        }
        f.record["future_executor_metadata"] = json!({"shape":[null,true,42,{"nested":"data"}]});
        f.write();
        original.current(until()).unwrap();
        let launch = uuid::Uuid::new_v4().to_string();
        f.record["launch_id"] = json!(launch);
        f.target.session_id = format!("launch_id:{launch}");
        f.write();
        f.capture().unwrap().current(until()).unwrap();
    }
    #[test]
    fn duplicate_current_identity_fields_cannot_hide_a_different_binding_or_archive() {
        let f = Fixture::new();
        for (key, value) in [("pid", "0"), ("archive_id", "\"saved\"")] {
            let mut record = f.record.clone();
            record["archive_id"] = Value::Null;
            let bytes = record.to_string();
            fs::write(f.path(), format!("{{\"{key}\":{value},{}", &bytes[1..])).unwrap();
            assert!(f.capture().is_err(), "{key}");
        }
    }
    /// zerus-ucck P3a: an executor in the ID layout is found through its
    /// label's index entry and read by its session ID on every later check.
    /// The label's first holder, at the label's own path, never stands in.
    #[test]
    fn an_executor_in_the_id_layout_is_read_by_its_id_throughout() {
        let mut f = Fixture::new();
        let mut first = f.record.clone();
        first["session_id"] = json!(uuid::Uuid::new_v4().to_string());
        first["run_id"] = json!(uuid::Uuid::new_v4().to_string());
        fs::write(f.path(), serde_json::to_vec(&first).unwrap()).unwrap();
        let private = |path: &Path, bytes: &[u8]| {
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        for folder in ["sessions", "labels"] {
            fs::create_dir(f.root.path().join(folder)).unwrap();
            fs::set_permissions(f.root.path().join(folder), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let id = uuid::Uuid::new_v4().to_string();
        let own = f.root.path().join("sessions").join(format!("{id}.json"));
        let index = f.root.path().join("labels").join(f.path().file_name().unwrap());
        f.record["session_id"] = json!(id);
        private(&own, &serde_json::to_vec(&f.record).unwrap());
        assert!(f.capture().is_err(), "the label's holder binds another run");
        let text = json!({"version":1,"label":"private-task-executor","ids":[id]}).to_string();
        private(&index, text.as_bytes());
        assert!(f.capture().is_err(), "an index no manifest proves is refused");
        // The manifest lists the label with its file's digest (#1437 review).
        let key = index.file_stem().unwrap().to_str().unwrap().to_owned();
        let manifest = f.root.path().join("labels").join(".complete");
        private(&manifest, json!({"version":1,"labels":{}}).to_string().as_bytes());
        assert!(f.capture().is_err(), "a label the manifest omits is read at its own path");
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        private(&manifest, json!({"version":1,"labels":{&key: digest}}).to_string().as_bytes());
        assert!(f.capture().is_err(), "a file there without the layout is refused");
        f.record["layout"] = json!("id");
        private(&own, &serde_json::to_vec(&f.record).unwrap());
        let original = f.capture().unwrap();
        f.record["activity"] = json!("working");
        private(&own, &serde_json::to_vec(&f.record).unwrap());
        original.current(until()).unwrap();
        // The label's own record counts too: when it binds the executor as
        // well, the proof refuses (#1437 review).
        let mut double = f.record.clone();
        double.as_object_mut().unwrap().remove("layout");
        double["session_id"] = json!(uuid::Uuid::new_v4().to_string());
        fs::write(f.path(), serde_json::to_vec(&double).unwrap()).unwrap();
        assert!(f.capture().is_err(), "two sessions bind the executor");
        fs::write(f.path(), serde_json::to_vec(&first).unwrap()).unwrap();
        f.capture().unwrap();
        // A changed index no longer matches its manifest entry, even when it
        // still lists the executor.
        let stale = uuid::Uuid::new_v4().to_string();
        private(&index, json!({"version":1,"label":"private-task-executor","ids":[id, stale]}).to_string().as_bytes());
        assert!(f.capture().is_err(), "an index its manifest no longer proves");
        private(&index, text.as_bytes());
        fs::remove_file(&own).unwrap();
        assert!(original.current(until()).is_err(), "never the label's first holder");
    }
    /// #1437 review: while `sessions/` exists, the executor proof needs the
    /// manifest's word that its label has no session in the ID layout.
    #[test]
    fn an_executor_is_refused_while_no_manifest_proves_its_label() {
        let f = Fixture::new();
        f.capture().unwrap();
        let sessions = f.root.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(f.capture().is_err());
        let labels = f.root.path().join("labels");
        fs::create_dir(&labels).unwrap();
        fs::set_permissions(&labels, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(f.capture().is_err());
        let manifest = labels.join(".complete");
        fs::write(&manifest, json!({"version":1,"labels":{}}).to_string()).unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
        f.capture().unwrap();
    }
    /// #1437 review (desktop-u, R3): the executor proof reads the label index
    /// through the CLI's own proof. A listed label's file must list one or more
    /// canonical IDs; any other shape refuses and is never "no candidates".
    #[test]
    fn an_executor_refuses_every_malformed_listed_index() {
        let f = Fixture::new();
        f.capture().unwrap();
        let private = |path: &Path, bytes: &[u8]| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        let id = uuid::Uuid::new_v4().to_string();
        let mut twin = f.record.clone();
        twin["session_id"] = json!(id);
        twin["layout"] = json!("id");
        twin["run_id"] = json!(uuid::Uuid::new_v4().to_string());
        private(&f.root.path().join("sessions").join(format!("{id}.json")), twin.to_string().as_bytes());
        let name = "private-task-executor";
        let key = format!("{:x}", Sha256::digest(name.as_bytes()));
        let prove = |index: Value| {
            let bytes = index.to_string();
            private(&f.root.path().join("labels").join(format!("{key}.json")), bytes.as_bytes());
            let digest = format!("{:x}", Sha256::digest(bytes.as_bytes()));
            private(
                &f.root.path().join("labels").join(".complete"),
                json!({"version":1,"labels":{&key: digest}}).to_string().as_bytes(),
            );
        };
        prove(json!({"version":1,"label":name,"ids":[id]}));
        f.capture().unwrap();
        for ids in [
            None,
            Some(Value::Null),
            Some(json!({})),
            Some(json!([])),
            Some(json!([1])),
            Some(json!(["not-an-id"])),
            Some(json!([id, "not-an-id"])),
            Some(json!([id, 1])),
        ] {
            let mut index = json!({"version":1,"label":name});
            if let Some(ids) = ids.clone() {
                index["ids"] = ids;
            }
            prove(index);
            assert!(f.capture().is_err(), "a listed index with ids {ids:?}");
        }
    }
}
