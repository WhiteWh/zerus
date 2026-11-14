//! The DeepSeek host on Windows. The bridge listens on a named pipe named
//! after the host directory (per user and per native home) instead of a Unix
//! socket; everything else is the Unix design: one detached host per native
//! home, started under the start lock, reached by one newline-terminated
//! request per connection.
//!
//! The Unix-only parts stay Unix-only: the recorded host identity and its
//! ancestry proof (`host_identity`), the per-session shell identities
//! (`shell_identity`) and the caller proof built on them. A hosted tool call
//! on Windows carries no session markers, as before this module.
use super::*;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read};
use std::time::{Duration, Instant};

/// The bridge's named pipe. Its default security lets other accounts open it
/// only for reading, which cannot send the bridge a request.
pub(super) fn endpoint(dir: &Path) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(dir.to_string_lossy().as_bytes()));
    PathBuf::from(format!(r"\\.\pipe\hgs-dsh-{}", &digest[..32]))
}

/// ERROR_PIPE_BUSY: every listening instance is taken; the bridge adds more.
const PIPE_BUSY: i32 = 231;

fn connect(path: &Path) -> io::Result<fs::File> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match OpenOptions::new().read(true).write(true).open(path) {
            Err(error) if error.raw_os_error() == Some(PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            result => return result,
        }
    }
}

pub(super) fn rpc(record: &Value, method: &str, params: Value) -> Result<Value> {
    rpc_within(record, method, params, Duration::from_secs(35))
}

/// Interactive search waits at most this long for one host.
pub(super) fn search_rpc(record: &Value, params: Value) -> Result<Value> {
    rpc_within(record, "search-history", params, Duration::from_secs(7))
}

pub(super) fn rpc_within(record: &Value, method: &str, params: Value, read_timeout: Duration) -> Result<Value> {
    let mut pipe = connect(&endpoint(&host_dir(record))).map_err(|_| HOST_UNAVAILABLE)?;
    // The request's last byte is its newline, and the compact JSON holds no
    // other, so a failed write never wrote it and the host never ran it.
    writeln!(pipe, "{}", json!({"method":method,"params":params})).map_err(|e| format!("{INCOMPLETE}: {e}"))?;
    // A pipe handle has no read timeout: the deadline abandons the reading
    // thread, which ends with this short-lived process.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut response = String::new();
        let result = BufReader::new(pipe.take(32 * 1024 * 1024)).read_line(&mut response).map(|_| response);
        let _ = sender.send(result);
    });
    let response = match receiver.recv_timeout(read_timeout) {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => return Err(format!("Native request delivery uncertain: {e}")),
        Err(_) => return Err("Native request delivery uncertain: the DeepSeek host did not answer in time".into()),
    };
    let response: Value =
        serde_json::from_str(&response).map_err(|e| format!("Invalid native response; delivery uncertain: {e}"))?;
    if response["ok"] != true {
        return Err(string(&response, "error").into());
    }
    Ok(response["value"].clone())
}

/// The official harness as a session would start it: its npm shim resolves to
/// node and the package's script, never through cmd.exe.
fn dsh_command() -> io::Result<Command> {
    crate::platform::agent_command(&["dsh".to_owned()])
}

pub(super) fn ensure_host(record: &Value) -> Result<()> {
    let dir = host_dir(record);
    private_dir(&dir)?;
    let _guard = lock(Some(&dir.join("start.lock")))?;
    if let Ok(ping) = rpc(record, "ping", json!({})) {
        if compatible_adapter(&ping) {
            return Ok(());
        }
        return Err("The running DeepSeek host uses an incompatible adapter; close its sessions before upgrading".into());
    }
    let pipe = endpoint(&dir);
    match connect(&pipe) {
        Ok(_) => return Err("The running DeepSeek host is not responding; it has been left running. Retry when it recovers".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Cannot check the native host: {e}")),
    }
    // A host an earlier start launched may not have opened its pipe yet: it is
    // waited for, never doubled by a second host.
    if let Some(pid) = starting_host(&dir) {
        return wait(record, &dir, Instant::now() + Duration::from_secs(35), &mut || {
            Ok(crate::platform::process_alive(pid))
        });
    }
    let recommended = REVIEWED_VERSIONS.last().expect("reviewed harness version");
    let version = dsh_command().and_then(|mut command| command.arg("--version").output()).map_err(|_| {
        format!("Install the official harness and its reviewed security overrides: see docs/installation.md#agents (@deepseek-ai/dsh@{recommended})")
    })?;
    if !version.status.success() || !reviewed_harness_version(String::from_utf8_lossy(&version.stdout).trim()) {
        return Err(format!("This adapter requires official @deepseek-ai/dsh@{recommended}; installed version differs"));
    }
    let script = dir.join("hgs-bridge.mjs");
    atomic(&script, BRIDGE)?;
    let overlay = dir.join("hgs.patch.json");
    let config = bridge_config(&pipe, agent_hint::for_account("native-dsh"));
    atomic(
        &overlay,
        &json!([{"insert":[{"id":"hgs-native-dsh","name":script,"config":config}]}]).to_string(),
    )?;
    let log = crate::platform::private_open(OpenOptions::new().create(true).truncate(true).write(true))
        .open(dir.join("host.log"))
        .map_err(|e| e.to_string())?;
    let mut command = dsh_command().map_err(|e| e.to_string())?;
    command.args(["--profile", "hgs"]);
    // This flag creates a profile and intentionally rejects existing ones.
    if !Path::new(string(record, "agent_home")).join("profiles/hgs/package.json").exists() {
        command.args(["--from-default-profile", "web"]);
    }
    command
        .arg("--patch")
        .arg(&overlay)
        .args(["--no-open", "--host", "127.0.0.1", "--port", "0"])
        .env("DSH_HOME", string(record, "agent_home"))
        .current_dir(home())
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    // One host serves every DeepSeek session of its agent home, so whichever
    // session happened to start it is not the session of any hosted agent.
    for name in crate::state::backend::host::SESSION_CONTEXT.iter().chain(&["HGS_TERMINAL_NAME"]) {
        command.env_remove(name);
    }
    // Detach only the dedicated native host: closing its launching terminal or
    // hgs process must not stop conversations.
    let mut child = crate::platform::spawn_detached(&mut command).map_err(|e| e.to_string())?;
    atomic(&dir.join("host.pid"), &child.id().to_string())?;
    wait(record, &dir, Instant::now() + Duration::from_secs(35), &mut || {
        Ok(child.try_wait().map_err(|e| e.to_string())?.is_none())
    })
}

/// Waits until the host answers, while `alive` says its process runs.
fn wait(record: &Value, dir: &Path, deadline: Instant, alive: &mut dyn FnMut() -> Result<bool>) -> Result<()> {
    while Instant::now() < deadline {
        if let Ok(value) = rpc(record, "ping", json!({})) {
            if value["protocol"] == 1 {
                return Ok(());
            }
        }
        if !alive()? {
            return Err(format!("Official DeepSeek host failed to start. Private log: {}", dir.join("host.log").display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("DeepSeek host is still starting; retry opening the session".into())
}

/// The pid of a host an earlier start launched within the last two minutes
/// that still runs: it may only be slow to open its pipe.
fn starting_host(dir: &Path) -> Option<u32> {
    let path = dir.join("host.pid");
    let pid: u32 = fs::read_to_string(&path).ok()?.trim().parse().ok()?;
    let age = fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    (age <= Duration::from_secs(140) && crate::platform::process_alive(pid)).then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_host_directory_has_its_own_pipe() {
        let one = endpoint(Path::new(r"C:\Users\a\.local\state\hgs\sessions\dsh\hosts\aaaa"));
        let two = endpoint(Path::new(r"C:\Users\a\.local\state\hgs\sessions\dsh\hosts\bbbb"));
        assert_ne!(one, two);
        let text = one.to_string_lossy();
        assert!(text.starts_with(r"\\.\pipe\hgs-dsh-") && text.len() == r"\\.\pipe\hgs-dsh-".len() + 32, "{text}");
    }

    #[test]
    fn a_missing_host_is_unavailable_not_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let record = json!({"agent_home": dir.path(), "name": "dsh/x/y"});
        assert_eq!(rpc_within(&record, "ping", json!({}), Duration::from_secs(1)).unwrap_err(), HOST_UNAVAILABLE);
    }
}
